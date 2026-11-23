//! Starts one reviewer when a worker reports a tested commit.
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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use mast_proto::{
    CreateChatForWorkItemParams, DispatchKind, DispatchRecord, DispatchState, ReportCompletion,
    StandingRoleKind, WorkItemPrompt, dispatch_operation_id,
};

use crate::work_outline_launch::WorkItemLaunchService;
use crate::workspace_host::WorkspaceHost;

/// Room left in a worker report for the dispatch line.
const DISPATCH_LINE_ROOM: usize = 700;

#[derive(Clone)]
pub struct ReviewDispatcher {
    workspace: WorkspaceHost,
    launch: WorkItemLaunchService,
    store: Option<PathBuf>,
    records: Arc<Mutex<BTreeMap<String, DispatchRecord>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The saved records. A missing file is empty. An unreadable or corrupt one is
/// moved aside, never overwritten, so the records stay recoverable.
fn load_records(path: &std::path::Path) -> BTreeMap<String, DispatchRecord> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            tracing::error!(%error, "review dispatch: records unreadable");
            set_aside(path);
            return BTreeMap::new();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::error!(%error, "review dispatch: records corrupt; moved aside");
        set_aside(path);
        BTreeMap::new()
    })
}

fn set_aside(path: &std::path::Path) {
    let aside = path.with_extension(format!("corrupt-{}", now_ms()));
    if let Err(error) = std::fs::rename(path, &aside) {
        tracing::error!(%error, "review dispatch: could not move records aside");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl ReviewDispatcher {
    pub fn new(workspace: WorkspaceHost, launch: WorkItemLaunchService, store: Option<PathBuf>) -> Self {
        let records = store.as_deref().map(load_records).unwrap_or_default();
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

    fn save(&self, records: &BTreeMap<String, DispatchRecord>) -> Result<(), String> {
        let Some(path) = &self.store else { return Ok(()) };
        let bytes = serde_json::to_vec(records).map_err(|e| e.to_string())?;
        // Write beside the file, sync, then rename: a crash never leaves a torn file.
        let tmp = path.with_extension("tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| format!("could not save dispatch records: {e}"))
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
            matches!(&record.state, DispatchState::Started { chat_id } if chat_id == worker_chat_id)
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
         with the commit you reviewed and a clear verdict.",
        sha = completion.tested_sha,
        evidence = completion.evidence,
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
