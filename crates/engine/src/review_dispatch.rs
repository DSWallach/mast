//! Starts one reviewer when a worker reports a tested commit, and holds the
//! Worker records of the work queue (one offer per item and queue key).
//!
//! The worker's `report_to_parent` carries a [`ReportCompletion`]. The engine
//! records a [`DispatchRecord`] keyed by (work item, commit), then launches the
//! reviewer under an operation id derived from that key only. A duplicate
//! report, a restart or a successor Manager finds the record and replays or
//! returns the same launch, so a second reviewer never starts.
//!
//! Records live in one file beside the engine's data. That is only safe with
//! one authoritative engine, so dispatch stays disabled (recorded as blocked)
//! once the workspace syncs through a server.
//!
//! # Cross-family reviewers
//!
//! A reviewer should run on a different model family than the implementer
//! (plan v3 §7.1). [`select_reviewer`] picks one among the signed-in harnesses
//! ([`signed_in_reviewers`]) and, when no other family is signed in, falls back
//! to the implementer's family and records why. The choice is a library result:
//! [`ReviewerChoice::config`] is the launch config for it, and the merge queue
//! keeps it on the land request. [`ReviewDispatcher::dispatch`] does not apply
//! it yet.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use mast_proto::{
    ChatConfig, CreateChatForWorkItemParams, DispatchKind, DispatchRecord, DispatchState,
    HarnessId, ReportCompletion, SandboxLevel, StandingRoleKind, WorkItemPrompt,
    dispatch_operation_id, resolve_model,
};
use serde::{Deserialize, Serialize};

use crate::agent_accounts::{AgentAccounts, LocalSignInStatus};
use crate::work_outline_launch::WorkItemLaunchService;
use crate::workspace_host::WorkspaceHost;

/// Room left in a worker report for the dispatch line.
const DISPATCH_LINE_ROOM: usize = 700;

#[derive(Clone)]
pub struct ReviewDispatcher {
    workspace: WorkspaceHost,
    launch: WorkItemLaunchService,
    store: Option<Arc<RecordFile>>,
    records: Arc<Mutex<BTreeMap<String, DispatchRecord>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The records file. A corrupt or unreadable file is moved aside, never
/// overwritten. While it could not be moved, nothing is written over it.
struct RecordFile {
    path: PathBuf,
    /// Where the bad file still has to go. `Some` blocks every save.
    pending_aside: Mutex<Option<PathBuf>>,
}

impl RecordFile {
    /// The saved records. A missing file is empty.
    fn open(path: PathBuf) -> (Self, BTreeMap<String, DispatchRecord>) {
        let aside = path.with_extension(format!("corrupt-{}", now_ms()));
        Self::open_with_aside(path, aside)
    }

    fn open_with_aside(path: PathBuf, aside: PathBuf) -> (Self, BTreeMap<String, DispatchRecord>) {
        let file = Self { path, pending_aside: Mutex::new(None) };
        let records = match std::fs::read(&file.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::error!(%error, "review dispatch: records corrupt; moving aside");
                file.set_aside(aside);
                BTreeMap::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                tracing::error!(%error, "review dispatch: records unreadable; moving aside");
                file.set_aside(aside);
                BTreeMap::new()
            }
        };
        (file, records)
    }

    /// Move the bad file to `aside`. If that fails, remember it so `save`
    /// tries again and refuses to write until it works.
    fn set_aside(&self, aside: PathBuf) {
        let moved = match std::fs::rename(&self.path, &aside) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                tracing::error!(%error, "review dispatch: could not move records aside");
                false
            }
        };
        *lock(&self.pending_aside) = (!moved).then_some(aside);
    }

    fn save(&self, records: &BTreeMap<String, DispatchRecord>) -> Result<(), String> {
        let pending = lock(&self.pending_aside).clone();
        if let Some(aside) = pending {
            self.set_aside(aside);
            if lock(&self.pending_aside).is_some() {
                tracing::error!(path = %self.path.display(), "review dispatch: saves blocked until the bad records file is moved aside");
                return Err(format!(
                    "the existing dispatch records at {} are unreadable and could not be moved aside, so nothing is saved over them",
                    self.path.display()
                ));
            }
        }
        let bytes = serde_json::to_vec(records).map_err(|e| e.to_string())?;
        // Write beside the file, sync, then rename: a crash never leaves a torn file.
        let tmp = self.path.with_extension("tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| format!("could not save dispatch records: {e}"))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl ReviewDispatcher {
    pub fn new(workspace: WorkspaceHost, launch: WorkItemLaunchService, store: Option<PathBuf>) -> Self {
        let (store, records) = match store.map(RecordFile::open) {
            Some((file, records)) => (Some(Arc::new(file)), records),
            None => (None, BTreeMap::new()),
        };
        Self { workspace, launch, store, records: Arc::new(Mutex::new(records)) }
    }

    /// Every dispatch recorded for `work_item_id`, oldest first.
    pub fn records_for_item(&self, work_item_id: &str) -> Vec<DispatchRecord> {
        let mut records: Vec<_> = lock(&self.records)
            .values()
            .filter(|record| record.work_item_id == work_item_id)
            .cloned()
            .collect();
        records.sort_by_key(|record| (record.updated_at, record.operation_id.clone()));
        records
    }

    /// The worker record of a queued item, if it was ever offered.
    pub(crate) fn worker_record(&self, item_id: &str) -> Option<DispatchRecord> {
        lock(&self.records)
            .values()
            .find(|record| record.kind == DispatchKind::Worker && record.work_item_id == item_id)
            .cloned()
    }

    /// Every item that has a worker record.
    pub(crate) fn worker_items(&self) -> Vec<String> {
        lock(&self.records)
            .values()
            .filter(|record| record.kind == DispatchKind::Worker)
            .map(|record| record.work_item_id.clone())
            .collect()
    }

    /// Make `record` the one worker record of its item. A failed save leaves
    /// the records as they were.
    pub(crate) fn put_worker(&self, record: DispatchRecord) -> Result<(), String> {
        let mut records = lock(&self.records);
        let before = records.clone();
        records.retain(|_, old| !(old.kind == DispatchKind::Worker && old.work_item_id == record.work_item_id));
        records.insert(record.operation_id.clone(), record);
        self.save(&records).inspect_err(|_| *records = before)
    }

    /// Drop the worker records of `items`. A failed save leaves them in place.
    pub(crate) fn retire_workers(&self, items: &[String]) -> Result<(), String> {
        let mut records = lock(&self.records);
        let before = records.clone();
        records.retain(|_, old| !(old.kind == DispatchKind::Worker && items.contains(&old.work_item_id)));
        if records.len() == before.len() {
            return Ok(());
        }
        self.save(&records).inspect_err(|_| *records = before)
    }

    /// An offered item whose worker exists is started. Only a claimed record
    /// moves. The error says the change is not durable.
    pub(crate) fn mark_worker_started(&self, item_id: &str, chat_id: &str) -> Option<String> {
        let operation_id = {
            let records = lock(&self.records);
            records
                .values()
                .find(|r| r.kind == DispatchKind::Worker
                        && r.work_item_id == item_id
                        && matches!(r.state, DispatchState::Claimed | DispatchState::Blocked { .. }))?
                .operation_id
                .clone()
        };
        self.set_state(&operation_id, DispatchState::Started { chat_id: chat_id.to_owned() }).1
    }

    fn save(&self, records: &BTreeMap<String, DispatchRecord>) -> Result<(), String> {
        match &self.store {
            Some(file) => file.save(records),
            None => Ok(()),
        }
    }

    /// Set the record's state and persist, returning the updated record.
    /// The state is kept in memory even when the save fails, so this process
    /// still answers truthfully; the error says it is not durable.
    fn set_state(&self, operation_id: &str, state: DispatchState) -> (Option<DispatchRecord>, Option<String>) {
        let mut records = lock(&self.records);
        let Some(record) = records.get_mut(operation_id) else { return (None, None) };
        record.state = state;
        record.updated_at = now_ms();
        let record = record.clone();
        let error = self.save(&records).err();
        if let Some(error) = &error {
            tracing::warn!(%error, "review dispatch: state not persisted");
        }
        (Some(record), error)
    }

    /// Start the reviewer for `completion` once, and return the line the
    /// parent should read. `Ok(None)` when the worker is not in a project run
    /// by a Manager (an ordinary report).
    pub async fn dispatch(
        &self,
        worker_chat_id: &str,
        completion: &ReportCompletion,
    ) -> Result<Option<String>, String> {
        let completion = completion.normalized()?;
        let worker = self
            .workspace
            .chat(worker_chat_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown chat {worker_chat_id}"))?;
        let space = worker.space_id.clone().ok_or("worker is not in a Space")?;
        let item = self
            .workspace
            .chat_assigned_work_item(worker_chat_id, &space)
            .ok_or("worker is not assigned to a work item, so there is nothing to review")?;
        let Some(manager) = self.manager_of(&space, &item) else {
            return Ok(None);
        };
        // A reviewer reporting back is not asking for another review.
        if lock(&self.records).values().any(|record| {
            record.kind == DispatchKind::Review
                && matches!(&record.state, DispatchState::Started { chat_id } if chat_id == worker_chat_id)
        }) {
            return Ok(None);
        }
        let operation_id = dispatch_operation_id(DispatchKind::Review, &item, &completion.tested_sha);

        let launch = {
            let mut records = lock(&self.records);
            let replay = match records.get(&operation_id) {
                Some(record) if matches!(record.state, DispatchState::Started { .. }) => {
                    return Ok(Some(started_line(record)));
                }
                // Only a half-made chat needs its original launch back, because
                // the launch inputs include the parent. With no chat yet,
                // start over from the current Manager and blockers.
                Some(record)
                    if matches!(record.state, DispatchState::Claimed)
                        && matches!(self.workspace.chat(&mast_proto::work_outline::derive_id(&operation_id, mast_proto::work_outline::roles::CHAT)), Ok(Some(_))) =>
                {
                    Some(record.launch.clone())
                }
                // Absent, blocked earlier, or claimed with no chat: look at the blockers again.
                _ => None,
            };
            match replay {
                Some(launch) => launch,
                None => {
                    let blocked = self.blocker(&worker, &manager);
                    let launch = blocked
                        .is_none()
                        .then(|| review_launch(&item, &completion, &manager, &operation_id));
                    let record = DispatchRecord {
                        queue_key: None,
                        boot_id: String::new(),
                        attempt: 0,
                        start_path: false,
                        kind: DispatchKind::Review,
                        work_item_id: item.clone(),
                        commit_sha: completion.tested_sha.clone(),
                        operation_id: operation_id.clone(),
                        worker_chat_id: worker_chat_id.to_owned(),
                        manager_chat_id: manager.clone(),
                        evidence: completion.evidence.clone(),
                        launch: launch.clone(),
                        state: match blocked {
                            Some((reason, owner)) => DispatchState::Blocked { reason, owner },
                            None => DispatchState::Claimed,
                        },
                        updated_at: now_ms(),
                    };
                    let previous = records.insert(operation_id.clone(), record);
                    if let Err(error) = self.save(&records) {
                        match previous {
                            Some(previous) => records.insert(operation_id.clone(), previous),
                            None => records.remove(&operation_id),
                        };
                        return Err(error);
                    }
                    launch
                }
            }
        };
        let Some(launch) = launch else {
            let records = lock(&self.records);
            return Ok(records.get(&operation_id).map(blocked_line));
        };
        match self.launch.create_chat_for_work_item(&launch).await {
            Ok(reply) => {
                let (record, unsaved) =
                    self.set_state(&operation_id, DispatchState::Started { chat_id: reply.chat_id });
                let mut line = record.as_ref().map(started_line).unwrap_or_default();
                if let Some(error) = unsaved {
                    line.push_str(&format!(" The record was not saved ({error}); a restart may start it again."));
                }
                Ok(Some(line))
            }
            Err(error) => Ok(Some(format!(
                "Review not started yet ({}). It is retried when this report is delivered again.",
                error.message
            ))),
        }
    }

    /// The Manager of the project `item` is in.
    fn manager_of(&self, space: &str, item: &str) -> Option<String> {
        let mut cursor = Some(item.to_owned());
        for _ in 0..64 {
            let id = cursor?;
            if let Some(manager) = self.workspace.standing_role_chat(StandingRoleKind::Manager, &id) {
                return Some(manager);
            }
            cursor = self.workspace.effective_work_item_parent(space, &id);
        }
        None
    }

    /// Why a dispatch must not start now, and who can resolve it.
    fn blocker(&self, worker: &mast_proto::Chat, manager: &str) -> Option<(String, String)> {
        if !self.workspace.is_local_authority() {
            return Some((
                "Automatic review dispatch needs one authoritative engine, and this workspace syncs \
                 through a server where another device could also dispatch. Start the reviewer by hand."
                    .into(),
                "user".into(),
            ));
        }
        let here = self.workspace.device_id();
        let manager_here = self
            .workspace
            .chat(manager)
            .ok()
            .flatten()
            .is_some_and(|chat| !chat.archived && chat.device_id == here);
        if worker.device_id != here || !manager_here {
            return Some((
                "The worker and the Manager must both run on the device that dispatches.".into(),
                "user".into(),
            ));
        }
        None
    }
}

/// Who makes a model. Two models of one family share blind spots, whatever
/// harness runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelFamily {
    Claude,
    /// GPT and Codex models.
    Gpt,
    Gemini,
    Grok,
    /// The harness runs several vendors' models and the model id does not say
    /// which, so it cannot be shown to differ from anything.
    Unknown,
}

impl ModelFamily {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Gpt => "GPT/Codex",
            Self::Gemini => "Gemini",
            Self::Grok => "Grok",
            Self::Unknown => "unknown",
        }
    }
}

/// The family a model id names, if it names one. Handles provider-prefixed ids
/// such as `anthropic/claude-...`.
fn named_family(model: &str) -> Option<ModelFamily> {
    let model = model.to_ascii_lowercase();
    let has = |names: &[&str]| names.iter().any(|name| model.contains(name));
    let token = |names: &[&str]| {
        model
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|part| names.contains(&part))
    };
    if has(&["claude"]) || token(&["opus", "sonnet", "haiku", "fable"]) {
        Some(ModelFamily::Claude)
    } else if has(&["gpt", "codex"]) || token(&["o1", "o3", "o4"]) {
        Some(ModelFamily::Gpt)
    } else if has(&["gemini"]) {
        Some(ModelFamily::Gemini)
    } else if has(&["grok"]) {
        Some(ModelFamily::Grok)
    } else {
        None
    }
}

/// The model family of a run. The model id wins over the harness, because a
/// harness such as Cursor or opencode runs other vendors' models; without a
/// telling id, only a single-vendor harness settles it.
pub fn model_family(harness: HarnessId, model: Option<&str>) -> ModelFamily {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    if let Some(family) = model.and_then(named_family) {
        return family;
    }
    match harness {
        HarnessId::ClaudeCode => ModelFamily::Claude,
        HarnessId::Codex => ModelFamily::Gpt,
        // Antigravity also offers other vendors' models, so an id that names
        // none of them is not assumed to be Gemini.
        HarnessId::Antigravity if model.is_none() => ModelFamily::Gemini,
        HarnessId::Grok => ModelFamily::Grok,
        _ => ModelFamily::Unknown,
    }
}

/// A harness and model a reviewer could run on, already known to be signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCandidate {
    pub harness: HarnessId,
    /// `None` uses the harness's own default.
    pub model: Option<String>,
}

/// The reviewer picked for one implementer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerChoice {
    pub harness: HarnessId,
    pub model: Option<String>,
    pub family: ModelFamily,
    pub implementer_family: ModelFamily,
    /// Why the reviewer is not known to be from another family. `None` exactly
    /// when it is.
    pub fallback_reason: Option<String>,
}

impl ReviewerChoice {
    pub fn is_cross_family(&self) -> bool {
        self.fallback_reason.is_none()
    }

    /// The launch config of this reviewer: read-only, asking for approvals.
    pub fn config(&self) -> ChatConfig {
        ChatConfig {
            harness: self.harness,
            model: self.model.clone().or_else(|| resolve_model(self.harness, None)),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::ReadOnly,
            auto_approve: false,
            approval_rules: false,
        }
    }
}

/// Pick a reviewer for work done on `implementer_harness` / `implementer_model`
/// from `candidates`, in their order of preference. A candidate of a known,
/// different family wins; otherwise the first candidate of the implementer's
/// family (else the first at all) is taken and the reason recorded. `None` when
/// there is no candidate.
pub fn select_reviewer(
    implementer_harness: HarnessId,
    implementer_model: Option<&str>,
    candidates: &[ReviewerCandidate],
) -> Option<ReviewerChoice> {
    let implementer = model_family(implementer_harness, implementer_model);
    let family = |c: &ReviewerCandidate| model_family(c.harness, c.model.as_deref());
    let choose = |c: &ReviewerCandidate, fallback_reason: Option<String>| ReviewerChoice {
        harness: c.harness,
        model: c.model.clone(),
        family: family(c),
        implementer_family: implementer,
        fallback_reason,
    };
    if implementer == ModelFamily::Unknown {
        let pick = candidates
            .iter()
            .find(|c| family(c) != ModelFamily::Unknown)
            .or_else(|| candidates.first())?;
        return Some(choose(
            pick,
            Some(
                "The implementer's model family is unknown, so the reviewer cannot be shown to \
                 be from another one."
                    .into(),
            ),
        ));
    }
    if let Some(other) = candidates
        .iter()
        .find(|c| family(c) != ModelFamily::Unknown && family(c) != implementer)
    {
        return Some(choose(other, None));
    }
    if let Some(same) = candidates.iter().find(|c| family(c) == implementer) {
        return Some(choose(
            same,
            Some(format!(
                "No other model family is signed in, so the reviewer shares the implementer's \
                 {} family.",
                implementer.label()
            )),
        ));
    }
    candidates.first().map(|c| {
        choose(
            c,
            Some(format!(
                "No signed-in reviewer is known to be outside the implementer's {} family.",
                implementer.label()
            )),
        )
    })
}

/// The reviewers this device is signed in for, most preferred first. Only
/// harnesses whose local sign-in can be checked are offered.
pub async fn signed_in_reviewers(accounts: &AgentAccounts) -> Vec<ReviewerCandidate> {
    let mut candidates = Vec::new();
    for harness in [HarnessId::ClaudeCode, HarnessId::Codex, HarnessId::Antigravity] {
        if accounts.local_sign_in_status(harness).await == LocalSignInStatus::SignedIn {
            candidates.push(ReviewerCandidate {
                harness,
                model: resolve_model(harness, None),
            });
        }
    }
    candidates
}

fn review_launch(
    item: &str,
    completion: &ReportCompletion,
    manager: &str,
    operation_id: &str,
) -> CreateChatForWorkItemParams {
    let text = format!(
        "Review commit {sha} of work item {item}. A worker tested it and reported this evidence:\n\n\
         {evidence}\n\n\
         Review that exact commit, not a later one. Report findings with report_to_parent, ending \
         with the commit you reviewed and a clear verdict.\n\n{flow_review}",
        sha = completion.tested_sha,
        evidence = completion.evidence,
        flow_review = crate::pr_review_launch::MAST_FLOW_REVIEW_NOTE,
    );
    CreateChatForWorkItemParams {
        work_item_id: item.to_owned(),
        initial_prompt: WorkItemPrompt { text, attachments: Vec::new() },
        config: None,
        execution_override: None,
        worktree_choice: None,
        operation_id: operation_id.to_owned(),
        parent_chat_id: Some(manager.to_owned()),
        target_device_id: None,
        account_id: None,
        standing_role: None,
    }
}

fn started_line(record: &DispatchRecord) -> String {
    let DispatchState::Started { chat_id } = &record.state else { return String::new() };
    format!(
        "Review dispatched: reviewer chat {chat_id} on commit {} (operation {}).",
        record.commit_sha, record.operation_id
    )
}

fn blocked_line(record: &DispatchRecord) -> String {
    match &record.state {
        DispatchState::Blocked { reason, owner } => format!("Review blocked ({owner}): {reason}"),
        _ => String::new(),
    }
}

/// `text` cut so a dispatch line still fits under the report cap, then the line.
pub(crate) fn with_dispatch_line(text: &str, line: &str) -> String {
    let text = text.trim();
    let line = cut(line, DISPATCH_LINE_ROOM);
    let limit = mast_proto::MAX_WORKER_REPORT_BYTES - DISPATCH_LINE_ROOM - 2;
    if text.len() <= limit {
        return format!("{text}\n\n{line}");
    }
    format!("{}\n[report cut]\n\n{line}", cut(text, limit - 14))
}

/// `text` cut to at most `max` bytes on a char boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[doc(hidden)]
pub fn with_dispatch_line_for_tests(text: &str, line: &str) -> String {
    with_dispatch_line(text, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corrupt_file_that_cannot_be_moved_aside_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.json");
        std::fs::write(&path, b"{ not json").unwrap();
        // A non-empty directory in the way makes the move fail on every platform.
        let aside = dir.path().join("records.corrupt-test");
        std::fs::create_dir(&aside).unwrap();
        std::fs::write(aside.join("keep"), b"x").unwrap();

        let (file, records) = RecordFile::open_with_aside(path.clone(), aside.clone());
        assert!(records.is_empty());
        let error = file.save(&BTreeMap::new()).unwrap_err();
        assert!(error.contains("could not be moved aside"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json", "the bad file is untouched");
        // Still blocked on the next save.
        assert!(file.save(&BTreeMap::new()).is_err());

        // Once the way is clear the next save moves the file aside, then writes.
        std::fs::remove_dir_all(&aside).unwrap();
        file.save(&BTreeMap::new()).unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), b"{ not json", "the bad file is kept");
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        file.save(&BTreeMap::new()).unwrap();
    }

    fn candidate(harness: HarnessId, model: Option<&str>) -> ReviewerCandidate {
        ReviewerCandidate { harness, model: model.map(str::to_owned) }
    }

    #[test]
    fn the_model_id_decides_the_family_before_the_harness() {
        use HarnessId as H;
        use ModelFamily as F;
        for (harness, model, family) in [
            (H::ClaudeCode, None, F::Claude),
            (H::Codex, Some("gpt-6-codex"), F::Gpt),
            (H::Cursor, Some("claude-sonnet-5-5"), F::Claude),
            (H::Opencode, Some("openai/o3"), F::Gpt),
            (H::Antigravity, None, F::Gemini),
            (H::Antigravity, Some("claude-opus-5-5"), F::Claude),
            (H::Antigravity, Some("mystery-1"), F::Unknown),
            (H::Cursor, Some("composer-2"), F::Unknown),
            (H::Cursor, None, F::Unknown),
        ] {
            assert_eq!(model_family(harness, model), family, "{harness:?} {model:?}");
        }
    }

    #[test]
    fn a_reviewer_from_another_family_is_preferred() {
        let candidates = [
            candidate(HarnessId::ClaudeCode, Some("claude-opus-5-5")),
            candidate(HarnessId::Cursor, Some("claude-sonnet-5-5")),
            candidate(HarnessId::Cursor, Some("composer-2")),
            candidate(HarnessId::Codex, None),
        ];
        let choice =
            select_reviewer(HarnessId::ClaudeCode, Some("claude-sonnet-5-5"), &candidates).unwrap();
        assert_eq!((choice.harness, choice.family), (HarnessId::Codex, ModelFamily::Gpt));
        assert!(choice.is_cross_family());
        let back = select_reviewer(HarnessId::Codex, None, &candidates).unwrap();
        assert_eq!(back.family, ModelFamily::Claude);
        assert!(back.is_cross_family());
        let config = back.config();
        assert_eq!(config.sandbox, SandboxLevel::ReadOnly);
        assert!(!config.auto_approve);
    }

    #[test]
    fn a_same_family_fallback_records_why() {
        let only_claude = [
            candidate(HarnessId::Cursor, Some("composer-2")),
            candidate(HarnessId::ClaudeCode, None),
        ];
        let choice = select_reviewer(HarnessId::ClaudeCode, None, &only_claude).unwrap();
        assert_eq!(choice.harness, HarnessId::ClaudeCode);
        assert!(!choice.is_cross_family());
        let reason = choice.fallback_reason.clone().unwrap();
        assert!(reason.contains("No other model family is signed in"), "{reason}");
        assert!(reason.contains("Claude"), "{reason}");
        assert_eq!(choice.config().model.as_deref(), Some(mast_proto::DEFAULT_CLAUDE_MODEL));

        let unknown = select_reviewer(HarnessId::Cursor, None, &only_claude).unwrap();
        assert_eq!(unknown.family, ModelFamily::Claude);
        assert!(unknown.fallback_reason.unwrap().contains("unknown"));

        let unprovable =
            select_reviewer(HarnessId::ClaudeCode, None, &only_claude[..1]).unwrap();
        assert!(!unprovable.is_cross_family());
        assert!(select_reviewer(HarnessId::ClaudeCode, None, &[]).is_none());
    }

    #[test]
    fn a_missing_file_is_empty_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.json");
        let (file, records) = RecordFile::open(path.clone());
        assert!(records.is_empty());
        file.save(&BTreeMap::new()).unwrap();
        assert!(path.exists());
    }
}
