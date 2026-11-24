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
//!
//! A project Manager has no parent and follows its role's `fallback_chain`
//! instead. The first usable entry after the one the chat is on becomes a
//! hand-off intent that the automatic Manager hand-off loop completes, so the
//! role rotation, wake transfer and restart recovery are that path's. An empty
//! chain leaves the Manager errored as before. A chain with no usable entry
//! left says so in the Manager chat and raises one Required Action. The
//! assistant's chain is ignored.
//!
//! Which limits are per model, from the vendors' docs (checked 2026-11-23):
//! only Claude has any. Its `seven_day_opus` / `seven_day_sonnet` windows and
//! the "You've hit your Opus limit" message apply only to requests to that
//! family, so switching models keeps the chat working; the session (5-hour)
//! and all-model weekly windows are shared by every Claude model. Codex limits
//! (5-hour and weekly, or credits) are one pool per account that Codex and
//! ChatGPT share, and OpenAI states that switching models does not restore
//! it. Cursor's included usage is one weekly pool per account (a legacy plan
//! has two monthly pools, Cursor models and third-party models), so a
//! `usage_limit` there is treated as account-wide too: the answer is another
//! harness, never another Cursor model. A Claude limit of any other family
//! (for instance a separate Fable weekly window) classifies as account-wide
//! `usage_weekly` and so moves to another harness rather than another Claude
//! model, which is safe but conservative.

use std::collections::{HashMap, HashSet};

use mast_proto::work_outline::{WorkOutlineErrorCode, derive_id, roles};
use mast_proto::{
    CreateChatForWorkItemParams, HarnessId, SessionStatus, StandingRole, StandingRoleKind,
    StandingRoleModel, WorkItemPrompt, cheaper_claude_model,
};
use tokio_util::sync::CancellationToken;

use crate::artifacts::ArtifactsService;
use crate::automatic_manager_handoff::{manager_status, no_status_text};
use crate::handoff_roles::{AutomaticHandoff, RoleObservations};
use crate::orchestrator_wakes::OrchestratorWakes;
use crate::sessions::SessionsEngine;
use crate::work_outline::LEDGER_KEY_PREFIX;
use crate::work_outline_launch::WorkItemLaunchService;
use crate::{DocHost, WorkspaceHost};

/// Give up on one chat's step after this many one-second attempts. The
/// decision stays recorded, so the next start resumes it.
const MAX_ATTEMPTS: u32 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// Move the chat down to `to`, the next cheaper model, and re-run.
    Step { to: String },
    /// Start a successor worker on `harness`.
    Successor(HarnessId),
    /// Nothing to switch to: report only.
    Report,
    /// A Manager's hand-off to entry `index` of its fallback chain. The
    /// harness and model are stored so a replay never reads the chain again.
    Chain { index: usize, harness: HarnessId, model: Option<String> },
    /// A Manager's chain has no usable entry left.
    Exhausted,
}

impl Decision {
    fn encode(&self) -> String {
        match self {
            Self::Step { to } => format!("step:{to}"),
            Self::Successor(harness) => format!("successor:{}", harness_name(*harness)),
            Self::Report => "report".into(),
            Self::Chain { index, harness, model } => {
                format!("chain:{index}:{}:{}", harness_name(*harness), model.as_deref().unwrap_or_default())
            }
            Self::Exhausted => "exhausted".into(),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        if text == "report" {
            return Some(Self::Report);
        }
        if text == "exhausted" {
            return Some(Self::Exhausted);
        }
        if let Some(rest) = text.strip_prefix("chain:") {
            let (index, rest) = rest.split_once(':')?;
            let (harness, model) = rest.split_once(':')?;
            return Some(Self::Chain {
                index: index.parse().ok()?,
                harness: parse_harness(harness)?,
                model: Some(model.to_owned()).filter(|model| !model.is_empty()),
            });
        }
        if let Some(to) = text.strip_prefix("step:") {
            return Some(Self::Step { to: to.into() });
        }
        parse_harness(text.strip_prefix("successor:")?).map(Self::Successor)
    }
}

fn parse_harness(text: &str) -> Option<HarnessId> {
    serde_json::from_value(serde_json::Value::String(text.into())).ok()
}

/// Whether a chain entry's model names the model a chat runs on; no model
/// stands for the harness default and only equals no model.
fn same_model(entry: Option<&str>, chat: Option<&str>) -> bool {
    let norm = |model: Option<&str>| model.map(str::trim).filter(|m| !m.is_empty()).map(str::to_ascii_lowercase);
    norm(entry) == norm(chat)
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

/// The operation id of a Manager's successor for one (chat, error entry).
/// Its label differs from the context-window hand-off's, so the two never share one.
fn manager_operation(chat: &str, entry: &str) -> String {
    derive_id(&format!("{chat}:{entry}"), "credit-fallback-manager")
}

/// Ledger prefix of the chain index a Manager chat holds; written for the
/// successor before its launch.
fn index_prefix(chat: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}credit-fallback-index:{chat}:")
}

/// Finish the record of an error that something else (a manual Retry) moved
/// the chat past before its hand-off committed.
pub(crate) fn finish_superseded(workspace: &WorkspaceHost, chat: &str, entry: &str) {
    let key = format!("{}:done", base_key(chat, entry));
    if let Err(error) = workspace.ledger_mark(&key) {
        tracing::warn!(%key, %error, "credit fallback record could not be written");
    }
}

/// A Manager hand-off that was saved but keeps failing to launch: one notice in
/// the Manager chat and one Required Action on the project, once per record.
pub(crate) fn report_stuck(workspace: &WorkspaceHost, launch: &WorkItemLaunchService, chat: &str, entry: &str, why: &str) {
    let base = base_key(chat, entry);
    let marked = format!("{base}:stuck");
    if workspace.ledger_holds(&marked) { return; }
    let line = format!(
        "Credit fallback: Mast could not start the next Manager for chat {chat} ({why}). It keeps trying; the Manager may need a manual switch.");
    if let Err(error) = launch.notify_chat(chat, &derive_id(&base, "credit-fallback-stuck-notice"), &format!("[Mast system message: {line}]")) {
        tracing::warn!(%chat, %error, "credit fallback notice was not written");
    }
    if let Some(role) = workspace.standing_role_of_chat(chat)
        && let Err(error) = launch.raise_request(
            &role.scope_item_id, "A Manager is out of credits and its hand-off keeps failing", &line,
            &derive_id(&base, "credit-fallback-stuck-request"))
    {
        tracing::warn!(%error, "credit fallback request could not be raised");
        return;
    }
    if let Err(error) = workspace.ledger_mark(&marked) {
        tracing::warn!(key = %marked, %error, "credit fallback record could not be written");
    }
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
    host: DocHost,
    artifacts: ArtifactsService,
}

pub(crate) fn start(
    workspace: WorkspaceHost, sessions: SessionsEngine, launch: WorkItemLaunchService,
    wakes: OrchestratorWakes, host: DocHost, artifacts: ArtifactsService,
) -> CancellationToken {
    let service = CreditFallback { workspace, sessions, launch, wakes, host, artifacts };
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
                if attempts.get(&chat).is_some_and(|n| *n >= service.max_attempts(&chat)) {
                    if waiting.remove(&chat) { service.give_up(&chat).await; }
                    continue;
                }
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

    /// How many attempts a chat's step gets before Mast gives up on it. A test
    /// hook shortens this so a test need not wait a minute.
    fn max_attempts(&self, chat: &str) -> u32 {
        if self.hooked("credit_fallback_attempts", chat) { 2 } else { MAX_ATTEMPTS }
    }

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

    /// Local worker and Manager chats that already hold a decision.
    fn chats_with_a_record(&self) -> HashSet<String> {
        let device = self.workspace.device_id().to_owned();
        self.workspace.read_chats().unwrap_or_default().into_iter()
            .filter(|chat| !chat.archived && chat.device_id == device)
            .filter(|chat| chat.parent_chat_id.is_some() || self.manager_role(&chat.id).is_some())
            .filter(|chat| has_record(&self.workspace, &chat.id))
            .map(|chat| chat.id)
            .collect()
    }

    /// The Manager role `chat` currently holds. The assistant's role, and a
    /// chat that was replaced, are not one.
    fn manager_role(&self, chat: &str) -> Option<StandingRole> {
        self.workspace.standing_role_of_chat(chat)
            .filter(|role| role.role == StandingRoleKind::Manager && role.current_chat_id.as_deref() == Some(chat))
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

    /// Where `chat` sits in `chain`: the index recorded for it when it was
    /// started as a successor, else the first entry naming its harness and
    /// model. `None` when it is on no entry, so the walk starts at the top.
    fn chain_position(&self, chat: &str, chain: &[StandingRoleModel], harness: HarnessId, model: Option<&str>) -> Option<usize> {
        let prefix = index_prefix(chat);
        if let Some(index) = self.workspace.ledger_find_prefix(&prefix).ok().flatten()
            .and_then(|key| key.strip_prefix(&prefix)?.parse().ok())
        {
            return Some(index);
        }
        chain.iter().position(|entry| {
            parse_harness(&entry.harness) == Some(harness) && same_model(entry.model.as_deref(), model)
        })
    }

    /// A Manager's decision from its role's chain. `None` leaves it errored:
    /// an empty chain, another hand-off already owning the chat, or an error
    /// that is not a usage limit.
    fn decide_manager(&self, chat: &str, code: &str, config: &mast_proto::ChatConfig, role: &StandingRole) -> Option<Decision> {
        let chain = &role.fallback_chain;
        if chain.is_empty() { return None; }
        // A context-window hand-off already waiting finishes first; its
        // successor meets the same limit and moves on from there.
        let tokens = self.sessions.agent_tokens();
        if tokens.pending_handoffs.automatic_intents().iter().any(|intent| intent.source == chat) { return None; }
        // The model the failed turn ran on, not the chat's current one.
        let ran = self.sessions.last_request(chat).and_then(|request| request.model)
            .or_else(|| config.model.clone()).filter(|model| !model.trim().is_empty());
        let start = self.chain_position(chat, chain, config.harness, ran.as_deref()).map_or(0, |index| index + 1);
        let enabled = self.sessions.enabled_harnesses();
        let next = chain.iter().enumerate().skip(start).find_map(|(index, entry)| {
            let harness = parse_harness(&entry.harness)?;
            (harness != HarnessId::Mock && enabled.contains(&harness))
                .then(|| (index, harness, entry.model.clone().filter(|model| !model.trim().is_empty())))
        });
        let Some((index, harness, model)) = next else { return Some(Decision::Exhausted) };
        // The chain overrides the built-in ladder: a per-model limit steps in
        // place only when the next entry is the same Claude harness on a
        // cheaper model.
        if code == "usage_weekly_model" && harness == HarnessId::ClaudeCode && config.harness == HarnessId::ClaudeCode
            && let Some(to) = &model
        {
            let current = ran.unwrap_or_else(|| HarnessId::ClaudeCode.default_model().unwrap_or_default().into());
            if rung(to) < rung(&current) {
                return Some(Decision::Step { to: to.clone() });
            }
        }
        Some(Decision::Chain { index, harness, model })
    }

    fn decide(&self, chat: &str, code: &str, manager: Option<&StandingRole>) -> Option<Decision> {
        if self.hooked("credit_fallback_decide", chat) { return None; }
        // A chat already continued elsewhere is not handed off a second time.
        if has_successor(&self.workspace, chat) { return None; }
        let config = self.workspace.chat_config(chat)?;
        if let Some(role) = manager {
            if !matches!(code, "usage_5h" | "usage_weekly" | "usage_limit" | "usage_weekly_model") { return None; }
            return self.decide_manager(chat, code, &config, role);
        }
        let per_model = code == "usage_weekly_model" && config.harness == HarnessId::ClaudeCode;
        if !per_model && !matches!(code, "usage_5h" | "usage_weekly" | "usage_limit" | "usage_weekly_model") {
            return None;
        }
        if per_model {
            // The model the failed turn ran on, not the chat's current one: a
            // turn that raced a switch must not step down twice.
            let ran = self.sessions.last_request(chat).and_then(|request| request.model)
                .or_else(|| config.model.clone()).filter(|model| !model.trim().is_empty());
            let current = ran.unwrap_or_else(|| HarnessId::ClaudeCode.default_model().unwrap_or_default().into());
            if let Some(to) = cheaper_claude_model(&current) {
                return Some(Decision::Step { to: to.into() });
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

    /// Out of attempts: say so once, so the Manager is not left guessing. The
    /// line has its own record, so a successor that starts after this (the
    /// decision is kept and the next start resumes it) still reports. A record
    /// the Manager was already told about (a switch that was reported before
    /// its re-run kept failing) gets no second, contradicting line, and no
    /// Required Action that says nothing is left to switch to.
    async fn give_up(&self, chat_id: &str) {
        let Some((entry, code)) = self.sessions.errored_limit(chat_id) else { return };
        let base = base_key(chat_id, &entry);
        tracing::debug!(chat = %chat_id, %entry, %code, max_attempts = self.max_attempts(chat_id),
            "credit fallback gave up on the chat's step; it is retried at the next start");
        let line = format!(
            "Credit fallback: chat {chat_id} hit {} and Mast could not switch it automatically; it needs a manual switch.",
            limit_label(&code));
        if !self.holds(&format!("{base}:reported")) {
            self.send(chat_id, &base, "gave-up-reported", "credit-fallback-gave-up-notice", &line).await;
        }
        // The Required Action says nothing is left to switch to, which is false
        // for a Manager whose switch was made and only the re-run keeps failing.
        if let Some(role) = self.manager_role(chat_id)
            && !matches!(self.recorded(&base), Some(Decision::Step { .. }))
        {
            self.escalate(&base, &role, &line);
        }
    }

    /// A Manager has no parent to tell, so a failure it cannot recover from
    /// also reaches the user as one Required Action on the project, once per
    /// (chat, error entry).
    fn escalate(&self, base: &str, role: &StandingRole, line: &str) -> bool {
        let raised = format!("{base}:escalated");
        if self.holds(&raised) { return true; }
        match self.launch.raise_request(
            &role.scope_item_id, "A Manager is out of credits and has nothing left to switch to", line,
            &derive_id(base, "credit-fallback-escalation"),
        ) {
            Ok(()) => self.mark(&raised),
            Err(error) => {
                tracing::warn!(%error, "credit fallback request could not be raised");
                false
            }
        }
    }

    async fn step(
        &self, chat_id: &str, baseline: &HashMap<String, Option<String>>, settled: &HashSet<(String, String)>,
    ) -> Outcome {
        let Ok(Some(chat)) = self.workspace.chat(chat_id) else { return Outcome::Ignore };
        let manager = self.manager_role(chat_id);
        if chat.archived || (chat.parent_chat_id.is_none() && manager.is_none())
            || chat.device_id != self.workspace.device_id() {
            return Outcome::Ignore;
        }
        let Some((entry, code)) = self.sessions.errored_limit(chat_id) else { return Outcome::Ignore };
        if settled.contains(&(chat_id.to_owned(), entry.clone())) { return Outcome::Ignore; }
        // An error already there when the host started is only resumed; a new
        // error entry on the same chat (a hand retry that failed again) is not.
        let allow_new = baseline.get(chat_id).is_none_or(|before| before.as_deref() != Some(entry.as_str()));
        let base = base_key(chat_id, &entry);
        if self.holds(&format!("{base}:done")) { return Outcome::Settled(entry); }
        if manager.is_none() && self.workspace.registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)
            .is_none_or(|row| row.deleted) {
            return Outcome::Ignore;
        }
        let decision = match self.recorded(&base) {
            Some(decision) => decision,
            None if !allow_new => return Outcome::Ignore,
            None => {
                let Some(decision) = self.decide(chat_id, &code, manager.as_ref()) else { return Outcome::Ignore };
                // Two writers racing here both read back the first record.
                if !self.mark(&format!("{base}:decided:{}", decision.encode())) { return Outcome::Retry; }
                self.recorded(&base).unwrap_or(decision)
            }
        };
        let line = match &decision {
            Decision::Step { to } => match self.apply_step(chat_id, &entry, &base, to).await {
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
            Decision::Chain { index, harness, model } => {
                let Some(role) = &manager else { return Outcome::Ignore };
                match self.apply_chain(chat_id, &entry, &base, role, *index, *harness, model.as_deref(), &code).await {
                    Ok(line) => line,
                    Err(outcome) => return outcome,
                }
            }
            Decision::Exhausted => {
                let line = format!(
                    "Credit fallback: Manager chat {chat_id} hit {} and no usable entry is left in its fallback chain; it needs credits or a manual switch.",
                    limit_label(&code));
                self.report(chat_id, &base, &line).await;
                if manager.as_ref().is_some_and(|role| !self.escalate(&base, role, &line)) {
                    return Outcome::Retry;
                }
                None
            }
        };
        if let Some(line) = line { self.report(chat_id, &base, &line).await; }
        self.mark(&format!("{base}:done"));
        Outcome::Settled(entry)
    }

    /// One line to the Manager, at most once per record.
    async fn report(&self, chat_id: &str, base: &str, line: &str) {
        self.send(chat_id, base, "reported", "credit-fallback-notice", line).await;
    }

    /// One line to the Manager, at most once per `marker` of the record. A
    /// refusal (parent archived or on another device) is logged and counts as
    /// sent: it never clears by waiting. Delivery is at least once across a
    /// crash between the send and the mark. `label` keys a Manager's own
    /// notice, so two kinds of line never share one id.
    async fn send(&self, chat_id: &str, base: &str, marker: &str, label: &str, line: &str) {
        let reported = format!("{base}:{marker}");
        if self.holds(&reported) { return; }
        let has_parent = self.workspace.chat(chat_id).ok().flatten().is_some_and(|chat| chat.parent_chat_id.is_some());
        if has_parent {
            if let Err(error) = self.wakes.report_to_parent(chat_id, line).await {
                tracing::warn!(chat = %chat_id, %error, "credit fallback report was not delivered");
            }
        } else if let Err(error) = self.launch.notify_chat(
            chat_id, &derive_id(base, label), &format!("[Mast system message: {line}]"),
        ) {
            // A Manager has no parent; its own transcript is where the user looks.
            tracing::warn!(chat = %chat_id, %error, "credit fallback notice was not written");
        }
        self.mark(&reported);
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
            // After the launch, so a failed one never blocks the stalled chat.
            self.mark(&format!("{}{successor}", successor_prefix(chat_id)));
            self.mark(&created);
        }
        Ok(Some(format!(
            "Credit fallback: chat {chat_id} hit {}; continued as chat {successor} on {}. The stalled chat is kept.",
            limit_label(code), harness_name(harness))))
    }

    /// Save the Manager's hand-off to chain entry `index` as an automatic
    /// hand-off intent. The automatic Manager hand-off loop launches it, so a
    /// lost reply, a retry and a restart all resume there from the intent file.
    #[allow(clippy::too_many_arguments)]
    async fn apply_chain(
        &self, chat_id: &str, entry: &str, base: &str, role: &StandingRole, index: usize,
        harness: HarnessId, model: Option<&str>, code: &str,
    ) -> Result<Option<String>, Outcome> {
        let operation = manager_operation(chat_id, entry);
        let successor = derive_id(&operation, roles::CHAT);
        let created = format!("{base}:created");
        if !self.holds(&created) {
            let pending = self.sessions.agent_tokens().pending_handoffs.automatic_intents().into_iter()
                .find(|intent| intent.source == chat_id);
            match pending {
                // Saved by an earlier pass that lost its mark.
                Some(intent) if intent.request.operation_id == operation => {}
                // Another hand-off owns the Manager: let it finish.
                Some(_) => return Err(Outcome::Ignore),
                None => {
                    if self.hooked("credit_fallback_successor", chat_id) { return Err(Outcome::Retry); }
                    // Before the intent, so the successor knows its place the
                    // moment it can run.
                    if !self.mark(&format!("{}{index}", index_prefix(&successor))) { return Err(Outcome::Retry); }
                    let intent = match self.manager_intent(chat_id, entry, &operation, role, harness, model).await {
                        Ok(intent) => intent,
                        Err(error) => {
                            tracing::warn!(chat = %chat_id, %error, "credit fallback hand-off could not be prepared yet");
                            return Err(Outcome::Retry);
                        }
                    };
                    match self.sessions.agent_tokens().pending_handoffs.remember_automatic(intent) {
                        Ok(true) => {}
                        // Another hand-off took the source between the check and the save.
                        Ok(false) => return Err(Outcome::Ignore),
                        Err(error) => {
                            tracing::warn!(chat = %chat_id, %error, "credit fallback hand-off intent could not be saved");
                            return Err(Outcome::Retry);
                        }
                    }
                }
            }
            if !self.mark(&created) { return Err(Outcome::Retry); }
        }
        Ok(Some(format!(
            "Credit fallback: this Manager hit {}; the project is handed to chat {successor} on {}{}.",
            limit_label(code), harness_name(harness), model.map(|m| format!(" ({m})")).unwrap_or_default())))
    }

    async fn manager_intent(
        &self, chat_id: &str, entry: &str, operation: &str, role: &StandingRole, harness: HarnessId, model: Option<&str>,
    ) -> Result<AutomaticHandoff, mast_proto::WorkOutlineError> {
        let missing = |what: &str| mast_proto::WorkOutlineError::new(WorkOutlineErrorCode::NotFound, what.to_owned());
        let chat = self.workspace.chat(chat_id).ok().flatten().ok_or_else(|| missing("The Manager chat was not found"))?;
        let space = chat.space_id.clone().ok_or_else(|| missing("The Manager chat has no Space"))?;
        let mut config = self.workspace.chat_config(chat_id).ok_or_else(|| missing("The Manager chat has no configuration"))?;
        // The chain entry's harness and model, not the stalled chat's.
        config.harness = harness;
        config.model = model.map(str::to_owned);
        config.reasoning = None;
        config.model_options.clear();
        let status = manager_status(&self.artifacts, &space, &role.scope_item_id)
            .unwrap_or_else(|| no_status_text(&self.host, chat_id));
        // Only what the user typed: Mast's own queued rows (a status nudge) are not theirs.
        let queued: Vec<_> = self.host.open(chat_id).ok().and_then(|handle| handle.doc().read_queue().ok())
            .unwrap_or_default().into_iter()
            .filter(|message| !message.text.starts_with(mast_proto::SYSTEM_MESSAGE_PREFIX))
            .collect();
        let mut text = format!(
            "Continue this project. The previous Manager, chat {chat_id}, stopped because its harness ran out of credits or hit a usage limit. \
Use the latest Manager status:\n\n{status}");
        if !queued.is_empty() {
            text.push_str("\n\nThe user had queued these messages for the previous Manager. It never read them, so answer them now:");
            for message in &queued {
                text.push_str(&format!("\n- {}", message.text.chars().take(2000).collect::<String>()));
                if !message.attachments.is_empty() {
                    text.push_str(&format!(" ({} attachment(s) stayed in chat {chat_id})", message.attachments.len()));
                }
            }
        }
        let request = CreateChatForWorkItemParams {
            work_item_id: role.scope_item_id.clone(),
            initial_prompt: WorkItemPrompt { text, attachments: Vec::new() },
            config: Some(config),
            execution_override: None,
            worktree_choice: None,
            operation_id: operation.to_owned(),
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: None,
        };
        let tokens = self.sessions.agent_tokens();
        let observations = RoleObservations::capture(&self.workspace, &tokens, &chat);
        let mut intent = self.launch.prepare_automatic_handoff(&request, chat_id, &space, observations).await?;
        intent.credit_entry = Some(entry.to_owned());
        intent.carried = queued.iter().map(|message| message.id.clone()).collect();
        Ok(intent)
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
            Decision::Step { to: "claude-sonnet-5-5".into() },
            Decision::Successor(HarnessId::Codex),
            Decision::Report,
            Decision::Exhausted,
            Decision::Chain { index: 2, harness: HarnessId::ClaudeCode, model: Some("claude-sonnet-5-5".into()) },
            Decision::Chain { index: 0, harness: HarnessId::Codex, model: None },
        ] {
            assert_eq!(Decision::decode(&decision.encode()), Some(decision));
        }
        assert_eq!(Decision::decode("successor:nonsense"), None);
        assert_eq!(Decision::decode("chain:x:codex:"), None);
        assert_eq!(Decision::decode("chain:1:nonsense:"), None);
    }

    #[test]
    fn a_chain_decision_keeps_a_model_that_contains_a_colon() {
        let decision = Decision::Chain { index: 1, harness: HarnessId::Opencode, model: Some("provider:model:7".into()) };
        assert_eq!(Decision::decode(&decision.encode()), Some(decision));
    }

    #[test]
    fn the_manager_operation_is_stable_valid_and_not_the_context_window_one() {
        let op = manager_operation("chat", "entry");
        assert_eq!(op, manager_operation("chat", "entry"));
        assert_ne!(op, manager_operation("chat", "other"));
        assert!(op.len() <= 64 && op.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(op, derive_id("chat:entry", "automatic-manager-handoff"));
        assert_ne!(op, successor_operation("chat", "entry"));
    }

    #[test]
    fn a_chain_model_matches_a_chat_model_only_on_the_same_name() {
        assert!(same_model(Some("Claude-Sonnet-5-5"), Some("claude-sonnet-5-5")));
        assert!(same_model(None, Some("  ")));
        assert!(same_model(None, None));
        assert!(!same_model(Some("claude-opus-5-5"), Some("claude-sonnet-5-5")));
        assert!(!same_model(None, Some("claude-sonnet-5-5")));
    }

    #[test]
    fn the_successor_operation_is_stable_and_valid() {
        let op = successor_operation("chat", "entry");
        assert_eq!(op, successor_operation("chat", "entry"));
        assert_ne!(op, successor_operation("chat", "other"));
        assert!(op.len() <= 64 && op.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
