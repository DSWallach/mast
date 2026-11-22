//! Credit fallback: a worker that runs out of credits or hits a usage limit
//! keeps going instead of sitting errored.
//!
//! A per-model Claude limit steps the chat down one model in place (Opus to
//! Sonnet to Haiku, never up) and re-runs the failed turn. Any account-wide
//! limit, or the bottom of the ladder, starts a successor worker on the same
//! work item on another enabled harness and keeps the stalled chat. With no
//! harness left it reports instead. Cursor's transient `rate_limit` is never
//! acted on.
//!
//! The record is a set of keys in the durable processed-command ledger, keyed
//! by (chat, error entry id): `decided` holds the decision itself, so a retry,
//! a manual Retry or a restart re-reads it instead of deciding again, and
//! `switched`, `created`, `requeued`, `reported` and `done` mark the steps. A
//! chat that was already errored when the host started is only resumed if it
//! has a decision; it never gets a new one.

use std::collections::{HashMap, HashSet};

use mast_proto::work_outline::{WorkOutlineErrorCode, derive_id, roles};
use mast_proto::{HarnessId, SessionStatus, cheaper_claude_model};
use tokio_util::sync::CancellationToken;

use crate::orchestrator_wakes::OrchestratorWakes;
use crate::sessions::SessionsEngine;
use crate::work_outline::LEDGER_KEY_PREFIX;
use crate::work_outline_launch::WorkItemLaunchService;
use crate::WorkspaceHost;

/// Give up on one chat's step after this many one-second attempts. The
/// decision stays recorded, so the next start resumes it.
const MAX_ATTEMPTS: u32 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// Move the chat from one model to the next cheaper one and re-run.
    Step { from: String, to: String },
    /// Start a successor worker on `harness`.
    Successor(HarnessId),
    /// Nothing to switch to: report only.
    Report,
}

impl Decision {
    fn encode(&self) -> String {
        match self {
            Self::Step { from, to } => format!("step:{from}>{to}"),
            Self::Successor(harness) => format!("successor:{}", harness_name(*harness)),
            Self::Report => "report".into(),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        if text == "report" {
            return Some(Self::Report);
        }
        if let Some(rest) = text.strip_prefix("step:") {
            let (from, to) = rest.split_once('>')?;
            return Some(Self::Step { from: from.into(), to: to.into() });
        }
        let harness = text.strip_prefix("successor:")?;
        serde_json::from_value(serde_json::Value::String(harness.into())).ok().map(Self::Successor)
    }
}

fn harness_name(harness: HarnessId) -> String {
    serde_json::to_value(harness).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn base_key(chat: &str, entry: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}credit-fallback:{chat}:{entry}")
}

fn successor_prefix(chat: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}credit-fallback-succ:{chat}:")
}

/// Whether `chat` was already continued by a credit-fallback successor.
pub(crate) fn has_successor(workspace: &WorkspaceHost, chat: &str) -> bool {
    workspace.ledger_find_prefix(&successor_prefix(chat)).ok().flatten().is_some()
}

/// Whether `chat` has any credit-fallback record.
pub(crate) fn has_record(workspace: &WorkspaceHost, chat: &str) -> bool {
    workspace.ledger_find_prefix(&format!("{LEDGER_KEY_PREFIX}credit-fallback:{chat}:")).ok().flatten().is_some()
}

fn lineage_prefix(chat: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}credit-fallback-from:{chat}:")
}

/// The operation id of the successor for one (chat, error entry).
fn successor_operation(chat: &str, entry: &str) -> String {
    derive_id(&format!("{chat}:{entry}"), "credit-fallback-successor")
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Nothing to do for this chat right now.
    Ignore,
    /// Finished; do not look at this error again.
    Settled(String),
    /// A step could not finish yet; try again shortly.
    Retry,
}

#[derive(Clone)]
pub(crate) struct CreditFallback {
    workspace: WorkspaceHost,
    sessions: SessionsEngine,
    launch: WorkItemLaunchService,
    wakes: OrchestratorWakes,
}

pub(crate) fn start(
    workspace: WorkspaceHost, sessions: SessionsEngine, launch: WorkItemLaunchService,
    wakes: OrchestratorWakes,
) -> CancellationToken {
    let service = CreditFallback { workspace, sessions, launch, wakes };
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let mut rx = service.sessions.watch_sessions();
    // Chats already errored now were errored before this host started.
    let baseline: HashMap<String, Option<String>> = rx.borrow().iter()
        .filter(|session| session.status == SessionStatus::Errored)
        .map(|session| (session.chat_id.clone(), service.sessions.errored_limit(&session.chat_id).map(|(entry, _)| entry)))
        .collect();
    tokio::spawn(async move {
        let mut settled: HashSet<(String, String)> = HashSet::new();
        let mut attempts: HashMap<String, u32> = HashMap::new();
        let mut recheck = tokio::time::interval(std::time::Duration::from_secs(1));
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Chats whose last step could not finish; a quiet tick revisits only these.
        // A decision made before a restart is finished even though the session
        // list does not show an errored chat until it runs again.
        let mut waiting: HashSet<String> = service.chats_with_a_record();
        let mut woke = true;
        loop {
            let mut chats: Vec<String> = waiting.iter().cloned().collect();
            if woke {
                chats.extend(rx.borrow().iter()
                    .filter(|session| session.status == SessionStatus::Errored && !waiting.contains(&session.chat_id))
                    .map(|session| session.chat_id.clone()));
            }
            for chat in chats {
                if stopped.is_cancelled() { return; }
                if attempts.get(&chat).is_some_and(|n| *n >= MAX_ATTEMPTS) { continue; }
                match service.step(&chat, &baseline, &settled).await {
                    Outcome::Settled(entry) => {
                        settled.insert((chat.clone(), entry));
                        attempts.remove(&chat);
                        waiting.remove(&chat);
                    }
                    Outcome::Retry => {
                        *attempts.entry(chat.clone()).or_default() += 1;
                        waiting.insert(chat);
                    }
                    Outcome::Ignore => {
                        attempts.remove(&chat);
                        waiting.remove(&chat);
                    }
                }
            }
            tokio::select! {
                _ = stopped.cancelled() => return,
                changed = rx.changed() => { if changed.is_err() { return; } woke = true; }
                _ = recheck.tick() => { woke = false; }
            }
        }
    });
    cancel
}

impl CreditFallback {
    fn holds(&self, key: &str) -> bool { self.workspace.ledger_holds(key) }

    /// Test hook: hold a step so a test can stop the host between two steps.
    fn hooked(&self, event: &str, chat: &str) -> bool {
        self.sessions.agent_tokens().manager_status().test_hook(event, chat)
    }

    fn mark(&self, key: &str) -> bool {
        match self.workspace.ledger_mark(key) {
            Ok(_) => true,
            Err(error) => { tracing::warn!(%key, %error, "credit fallback record could not be written"); false }
        }
    }

    /// Local worker chats that already hold a decision.
    fn chats_with_a_record(&self) -> HashSet<String> {
        let device = self.workspace.device_id().to_owned();
        self.workspace.read_chats().unwrap_or_default().into_iter()
            .filter(|chat| !chat.archived && chat.parent_chat_id.is_some() && chat.device_id == device)
            .filter(|chat| has_record(&self.workspace, &chat.id))
            .map(|chat| chat.id)
            .collect()
    }

    fn recorded(&self, base: &str) -> Option<Decision> {
        let prefix = format!("{base}:decided:");
        let key = self.workspace.ledger_find_prefix(&prefix).ok().flatten()?;
        Decision::decode(key.strip_prefix(&prefix)?)
    }

    /// Harnesses already part of this chat's fallback lineage, itself included.
    fn lineage_harnesses(&self, chat: &str) -> HashSet<HarnessId> {
        let mut seen = HashSet::new();
        let mut next = Some(chat.to_owned());
        while let Some(id) = next.take() {
            if !seen.insert(id.clone()) || seen.len() > 16 { break; }
            next = self.workspace.ledger_find_prefix(&lineage_prefix(&id)).ok().flatten()
                .and_then(|key| key.strip_prefix(&lineage_prefix(&id)).map(str::to_owned));
        }
        seen.iter().filter_map(|id| self.workspace.chat_config(id)).map(|config| config.harness).collect()
    }

    fn decide(&self, chat: &str, code: &str) -> Option<Decision> {
        if self.hooked("credit_fallback_decide", chat) { return None; }
        // A chat already continued elsewhere is not handed off a second time.
        if has_successor(&self.workspace, chat) { return None; }
        let config = self.workspace.chat_config(chat)?;
        let per_model = code == "usage_weekly_model" && config.harness == HarnessId::ClaudeCode;
        if !per_model && !matches!(code, "usage_5h" | "usage_weekly" | "usage_limit" | "usage_weekly_model") {
            return None;
        }
        if per_model {
            // The model the failed turn ran on, not the chat's current one: a
            // turn that raced a switch must not step down twice.
            let ran = self.sessions.last_request(chat).and_then(|request| request.model)
                .or_else(|| config.model.clone()).filter(|model| !model.trim().is_empty());
            let current = ran.clone().unwrap_or_else(|| HarnessId::ClaudeCode.default_model().unwrap_or_default().into());
            if let Some(to) = cheaper_claude_model(&current) {
                return Some(Decision::Step { from: ran.unwrap_or_default(), to: to.into() });
            }
        }
        let used = self.lineage_harnesses(chat);
        let enabled = self.sessions.enabled_harnesses();
        let claude_first = enabled.iter().copied().filter(|h| *h == HarnessId::ClaudeCode);
        let others = enabled.iter().copied().filter(|h| *h != HarnessId::ClaudeCode);
        let harness = claude_first.chain(others)
            .find(|h| *h != HarnessId::Mock && *h != config.harness && !used.contains(h));
        Some(harness.map_or(Decision::Report, Decision::Successor))
    }

    async fn step(
        &self, chat_id: &str, baseline: &HashMap<String, Option<String>>, settled: &HashSet<(String, String)>,
    ) -> Outcome {
        let Ok(Some(chat)) = self.workspace.chat(chat_id) else { return Outcome::Ignore };
        if chat.archived || chat.parent_chat_id.is_none() || chat.device_id != self.workspace.device_id() {
            return Outcome::Ignore;
        }
        let Some((entry, code)) = self.sessions.errored_limit(chat_id) else { return Outcome::Ignore };
        if settled.contains(&(chat_id.to_owned(), entry.clone())) { return Outcome::Ignore; }
        // An error already there when the host started is only resumed; a new
        // error entry on the same chat (a hand retry that failed again) is not.
        let allow_new = baseline.get(chat_id).is_none_or(|before| before.as_deref() != Some(entry.as_str()));
        let base = base_key(chat_id, &entry);
        if self.holds(&format!("{base}:done")) { return Outcome::Settled(entry); }
        if self.workspace.registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)
            .is_none_or(|row| row.deleted) {
            return Outcome::Ignore;
        }
        let decision = match self.recorded(&base) {
            Some(decision) => decision,
            None if !allow_new => return Outcome::Ignore,
            None => {
                let Some(decision) = self.decide(chat_id, &code) else { return Outcome::Ignore };
                // Two writers racing here both read back the first record.
                if !self.mark(&format!("{base}:decided:{}", decision.encode())) { return Outcome::Retry; }
                self.recorded(&base).unwrap_or(decision)
            }
        };
        let line = match &decision {
            Decision::Step { to, .. } => match self.apply_step(chat_id, &entry, &base, to).await {
                Ok(line) => line,
                Err(outcome) => return outcome,
            },
            Decision::Successor(harness) => match self.apply_successor(chat_id, &entry, &base, *harness, &code).await {
                Ok(line) => line,
                Err(outcome) => return outcome,
            },
            Decision::Report => Some(format!(
                "Credit fallback: chat {chat_id} ({}) hit {} and no other enabled harness is left; it needs credits or a manual switch.",
                harness_name(self.workspace.chat_config(chat_id).map(|c| c.harness).unwrap_or(HarnessId::Mock)), limit_label(&code))),
        };
        // A refused report (parent archived, elsewhere, or reports off) is tried
        // again on the next tick; the work itself is already done.
        if let Some(line) = line && !self.report(chat_id, &base, &line).await { return Outcome::Retry; }
        self.mark(&format!("{base}:done"));
        Outcome::Settled(entry)
    }

    /// One line to the Manager, at most once per record. `false` when the
    /// parent refused it, so nothing is marked and a later call tries again.
    async fn report(&self, chat_id: &str, base: &str, line: &str) -> bool {
        let reported = format!("{base}:reported");
        if self.holds(&reported) { return true; }
        match self.wakes.report_to_parent(chat_id, line).await {
            Ok(_) => { self.mark(&reported); true }
            Err(error) => {
                tracing::warn!(chat = %chat_id, %error, "credit fallback report was not delivered");
                false
            }
        }
    }

    /// Switch the model (unless the chat is already on `to` or cheaper), report
    /// and requeue.
    async fn apply_step(&self, chat_id: &str, entry: &str, base: &str, to: &str) -> Result<Option<String>, Outcome> {
        let switched = format!("{base}:switched");
        if !self.holds(&switched) {
            let Some(mut config) = self.workspace.chat_config(chat_id) else { return Err(Outcome::Ignore) };
            // A chat the user already moved to this rung or a cheaper one stays
            // where they put it; the turn still runs again.
            if config.model.as_deref().is_none_or(|current| rung(current) > rung(to)) {
                config.model = Some(to.into());
                if self.workspace.set_chat_config(chat_id, &config).is_err() { return Err(Outcome::Retry); }
            }
            if !self.mark(&switched) { return Err(Outcome::Retry); }
        }
        let landed = self.workspace.chat_config(chat_id).and_then(|config| config.model).unwrap_or_else(|| to.into());
        let line = format!(
            "Credit fallback: chat {chat_id} hit its per-model limit; it is on {landed} now and the turn runs again.");
        // Said as soon as the switch is durable: a manual Retry may take the
        // requeue's place, and the Manager should hear of the switch either way.
        self.report(chat_id, base, &line).await;
        if !self.holds(&format!("{base}:requeued")) {
            if self.hooked("credit_fallback_requeue", chat_id) { return Err(Outcome::Retry); }
            match self.sessions.retry_errored_turn(chat_id, entry).await {
                Ok(true) => {}
                // Someone else (a manual Retry) already ran this turn again.
                Ok(false) if self.sessions.errored_limit(chat_id).is_none_or(|(now, _)| now != entry) => {}
                Ok(false) | Err(_) => return Err(Outcome::Retry),
            }
            self.mark(&format!("{base}:requeued"));
        }
        Ok(Some(line))
    }

    async fn apply_successor(&self, chat_id: &str, entry: &str, base: &str, harness: HarnessId, code: &str) -> Result<Option<String>, Outcome> {
        let operation = successor_operation(chat_id, entry);
        let successor = derive_id(&operation, roles::CHAT);
        let created = format!("{base}:created");
        if !self.holds(&created) {
            // Recorded before the launch so the new chat's own fallback never
            // returns to a harness this lineage already used.
            self.mark(&format!("{}{chat_id}", lineage_prefix(&successor)));
            self.mark(&format!("{}{successor}", successor_prefix(chat_id)));
            if self.hooked("credit_fallback_successor", chat_id) { return Err(Outcome::Retry); }
            let prompt = successor_prompt(chat_id);
            match self.launch.create_credit_successor(chat_id, &operation, harness, prompt).await {
                Ok(_) => {}
                // The same operation with different inputs (the stalled chat's
                // settings changed): fine only if the first attempt got its turn in.
                Err(error) if error.code == WorkOutlineErrorCode::OperationConflict
                    && self.launch.credit_successor_turn_queued(&successor, &operation) => {}
                Err(error) => {
                    tracing::warn!(chat = %chat_id, %error, "credit fallback successor was not created yet");
                    return Err(Outcome::Retry);
                }
            }
            self.mark(&created);
        }
        Ok(Some(format!(
            "Credit fallback: chat {chat_id} hit {}; continued as chat {successor} on {}. The stalled chat is kept.",
            limit_label(code), harness_name(harness))))
    }
}

/// Where a Claude model sits on the fallback ladder; higher is dearer. A model
/// that is not on it counts as dearest, so a step down always applies to it.
fn rung(model: &str) -> u8 {
    let model = model.to_ascii_lowercase();
    if model.contains("haiku") { 0 } else if model.contains("sonnet") { 1 } else { 2 }
}

fn limit_label(code: &str) -> &'static str {
    match code {
        "usage_5h" => "its 5-hour usage limit",
        "usage_weekly" | "usage_weekly_model" => "its weekly usage limit",
        _ => "out of credits or at its usage limit",
    }
}

fn successor_prompt(stalled: &str) -> String {
    format!(
        "Chat {stalled} was working on this work item and stopped because its harness ran out of credits or hit a usage limit. \
Continue its work. Read that chat (read_chat) to recover the branch, the worktree and the remaining task, \
then call bind_chat_worktree for the worktree. Do not redo work that is already committed. \
Leave chat {stalled} as it is."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decision_round_trips_through_its_ledger_key() {
        for decision in [
            Decision::Step { from: "claude-opus-5-5".into(), to: "claude-sonnet-5-5".into() },
            Decision::Step { from: String::new(), to: "claude-sonnet-5-5".into() },
            Decision::Successor(HarnessId::Codex),
            Decision::Report,
        ] {
            assert_eq!(Decision::decode(&decision.encode()), Some(decision));
        }
        assert_eq!(Decision::decode("successor:nonsense"), None);
    }

    #[test]
    fn the_successor_operation_is_stable_and_valid() {
        let op = successor_operation("chat", "entry");
        assert_eq!(op, successor_operation("chat", "entry"));
        assert_ne!(op, successor_operation("chat", "other"));
        assert!(op.len() <= 64 && op.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
