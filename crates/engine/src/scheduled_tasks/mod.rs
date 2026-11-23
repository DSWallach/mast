//! Profile-local scheduler. Only the owning engine claims occurrences.
mod cleanup;
pub(crate) use cleanup::DesktopRuns;
pub mod evaluator;
mod identity;
pub(crate) mod import;
mod lifecycle;
mod notion;
pub(crate) use notion::run_prompt;
pub(crate) mod results;
mod store;
use crate::{DocHost, EngineError, SessionsEngine, WorkspaceHost};
use mast_doc::SessionCommandStatus;
use mast_proto::*;
use std::{path::{Path, PathBuf}, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
const AWAITING_INPUT_LIMIT_MINUTES: i64 = 30;
/// How often run identities are rechecked without a task change (new
/// imported runs keep arriving until a task is cut over).
const IDENTITY_RECHECK_MS: i64 = 10 * 60_000;
pub(crate) fn fail(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

#[cfg(debug_assertions)]
fn disabled_recovery_roots() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static ROOTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(Default::default)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn test_disable_standing_recovery_lookup(root: &Path, disabled: bool) {
    let mut roots = disabled_recovery_roots().lock().unwrap_or_else(|e| e.into_inner());
    if disabled { roots.insert(root.to_owned()); } else { roots.remove(root); }
}

/// Standing-chat lookups for one boot recovery pass. The scheduler database
/// opens (and migrates) at most once, on the first chat that needs it, and
/// serves every later chat of the pass; a pass over many stale chats used to
/// open it once per chat. An open that failed stays failed for the rest of the
/// pass, so each chat reads as uncertain and is left alone.
pub(crate) struct StandingRecovery<'a> {
    root: &'a Path,
    store: std::cell::OnceCell<Result<Option<store::Store>, String>>,
    #[cfg(test)]
    opens: std::cell::Cell<usize>,
}

impl<'a> StandingRecovery<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            store: std::cell::OnceCell::new(),
            #[cfg(test)]
            opens: std::cell::Cell::new(0),
        }
    }

    fn open(&self) -> Result<Option<store::Store>, String> {
        #[cfg(debug_assertions)]
        if disabled_recovery_roots().lock().unwrap_or_else(|e| e.into_inner()).contains(self.root) {
            return Ok(None);
        }
        match std::fs::metadata(self.root.join("scheduled-tasks.sqlite3")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        }
        #[cfg(test)]
        self.opens.set(self.opens.get() + 1);
        store::Store::open(self.root).map(Some).map_err(|error| error.to_string())
    }

    /// Identify an accepted standing-chat message before boot recovery
    /// considers reviving it. The saved run target and message ID survive role
    /// rotation.
    pub(crate) fn messages(&self, chat: &str, last_user: Option<&str>) -> Result<Vec<String>, EngineError> {
        let store = match self.store.get_or_init(|| self.open()) {
            Ok(Some(store)) => store,
            Ok(None) => return Ok(Vec::new()),
            Err(error) => return Err(fail(error)),
        };
        Ok(store.runs_for_chat(chat)?.into_iter().filter_map(|run| {
            let message = crate::work_outline_launch::scheduled_id(&run.id, "message");
            (run.snapshot.output == ScheduledTaskOutput::Interactive
                && run.snapshot.chat_mode == ScheduledTaskChatMode::ContinueChat
                && (run.state.active() || last_user == Some(message.as_str())))
                .then_some(message)
        }).collect())
    }

    #[cfg(test)]
    fn opens(&self) -> usize {
        self.opens.get()
    }
}

#[cfg(test)]
mod standing_recovery_tests {
    use super::*;

    #[test]
    fn the_database_opens_once_per_pass_and_never_when_absent() {
        let root = tempfile::tempdir().unwrap();
        drop(store::Store::open(root.path()).unwrap());
        let pass = StandingRecovery::new(root.path());
        for chat in ["chat-a", "chat-b", "chat-c"] {
            assert!(pass.messages(chat, Some("message")).unwrap().is_empty());
        }
        assert_eq!(pass.opens(), 1, "one open serves every chat of the pass");
        // A later pass opens afresh.
        let next = StandingRecovery::new(root.path());
        assert!(next.messages("chat-a", None).unwrap().is_empty());
        assert_eq!(next.opens(), 1);

        let empty = tempfile::tempdir().unwrap();
        let none = StandingRecovery::new(empty.path());
        assert!(none.messages("chat-a", None).unwrap().is_empty());
        assert!(none.messages("chat-b", None).unwrap().is_empty());
        assert_eq!(none.opens(), 0, "no scheduler database, nothing to open");
    }

    #[test]
    fn an_unopenable_database_is_uncertain_for_every_chat_of_the_pass() {
        let root = tempfile::tempdir().unwrap();
        // A directory where the database should be: present, but not openable.
        std::fs::create_dir(root.path().join("scheduled-tasks.sqlite3")).unwrap();
        let pass = StandingRecovery::new(root.path());
        assert!(pass.messages("chat-a", None).is_err());
        assert!(pass.messages("chat-b", None).is_err());
        assert_eq!(pass.opens(), 1, "the failed open is not retried per chat");
    }
}

struct Inner {
    store: store::Store,
    missing_sources: PathBuf,
    workspace: WorkspaceHost,
    host: DocHost,
    sessions: SessionsEngine,
    registry: Arc<crate::HarnessRegistry>,
    launch_service: crate::work_outline_launch::WorkItemLaunchService,
    wakes: crate::orchestrator_wakes::OrchestratorWakes,
    gate: tokio::sync::Mutex<()>,
    cancel: CancellationToken,
    /// When run identities were last reconciled; `None` = due now.
    identities_checked: std::sync::Mutex<Option<i64>>,
}
#[derive(Clone)]
pub struct ScheduledTasks(Arc<Inner>);
impl ScheduledTasks {
    pub fn open(
        root: &Path,
        workspace: WorkspaceHost,
        host: DocHost,
        sessions: SessionsEngine,
        registry: Arc<crate::HarnessRegistry>,
        launch_service: crate::work_outline_launch::WorkItemLaunchService,
        wakes: crate::orchestrator_wakes::OrchestratorWakes,
    ) -> Result<Self, EngineError> {
        Ok(Self(Arc::new(Inner {
            store: store::Store::open(root)?,
            missing_sources: root.join(crate::external_import::SCHEDULED_SOURCE_MISSING),
            workspace,
            host,
            sessions,
            registry,
            launch_service,
            wakes,
            gate: tokio::sync::Mutex::new(()),
            cancel: CancellationToken::new(),
            identities_checked: std::sync::Mutex::new(None),
        })))
    }
    /// Whether new interactive chats start with the briefing index line.
    pub fn briefing_index_enabled(&self) -> bool {
        self.0.store.briefing_index().unwrap_or(false)
    }
    /// The same setting without holding the scheduler (which holds the
    /// sessions engine), for callbacks the sessions engine itself stores.
    /// Every live task's id, current name and Space, read without holding the
    /// scheduler, for the same kind of callback.
    pub fn task_names_reader(
        &self,
    ) -> impl Fn() -> Vec<crate::briefing::TaskAlias> + Send + Sync + 'static {
        let store = self.0.store.clone();
        move || {
            store
                .tasks()
                .map(|tasks| {
                    tasks
                        .into_iter()
                        .filter(|task| !task.deleted)
                        .map(|task| crate::briefing::TaskAlias {
                            id: task.id,
                            name: task.draft.name,
                            space_id: task.draft.space_id,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }
    pub fn briefing_index_reader(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let store = self.0.store.clone();
        move || store.briefing_index().unwrap_or(false)
    }
    pub fn start(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=this.0.cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(5))=>{if let Err(e)=this.tick(crate::now_ms()).await{tracing::error!(error=%e,"scheduled tasks tick failed");}}}
            }
        });
    }
    pub async fn shutdown(&self) {
        self.0.cancel.cancel();
        let _guard = self.0.gate.lock().await;
    }
    pub async fn request(
        &self,
        request: ScheduledTasksRequest,
    ) -> Result<ScheduledTasksSnapshot, EngineError> {
        self.request_from_client(request, true, true, true, true).await
    }

    pub(crate) async fn request_from_client(
        &self,
        request: ScheduledTasksRequest,
        output_provided: bool,
        after_success_provided: bool,
        chat_mode_provided: bool,
        refresh_provided: bool,
    ) -> Result<ScheduledTasksSnapshot, EngineError> {
        let _guard = self.0.gate.lock().await;
        let now = crate::now_ms();
        let mut input = serde_json::to_string(&request).map_err(fail)?;
        // Preserve legacy operation inputs. Explicit Interactive on an
        // updated client can change a mode that a legacy save would preserve.
        if output_provided
            && matches!(&request, ScheduledTasksRequest::Save { draft, .. }
                if draft.output == ScheduledTaskOutput::Interactive)
        {
            input.push_str("\noutput=interactive");
        }
        // The default is omitted in stored drafts. An explicit Archive on a
        // Background save can reset a previously saved Keep.
        if after_success_provided
            && matches!(&request, ScheduledTasksRequest::Save { draft, .. }
                if draft.after_success == ScheduledTaskAfterSuccess::Archive
                    && draft.output == ScheduledTaskOutput::Background)
        {
            input.push_str("\nafterSuccess=archive");
        }
        if chat_mode_provided && matches!(&request, ScheduledTasksRequest::Save { draft, .. } if draft.chat_mode == ScheduledTaskChatMode::NewChat) {
            input.push_str("\nchatMode=newChat");
        }
        if refresh_provided && matches!(&request, ScheduledTasksRequest::Save { draft, .. } if draft.refresh == ScheduledTaskRefresh::Never) {
            input.push_str("\nrefresh=never");
        }
        let mut response = ScheduledTasksSnapshot {
            device_id: self.0.workspace.device_id().into(),
            briefing_index: self.0.store.briefing_index()?,
            ..Default::default()
        };
        let operation = match &request {
            ScheduledTasksRequest::Save { operation_id, .. }
            | ScheduledTasksRequest::Delete { operation_id, .. }
            | ScheduledTasksRequest::RunNow { operation_id, .. }
            | ScheduledTasksRequest::Import { operation_id, .. } => Some(operation_id),
            _ => None,
        };
        let mutates = operation.is_some();
        if let Some(operation) = operation {
            if self.0.store.replay(operation, &input)? {
                response.tasks = self
                    .0
                    .store
                    .tasks()?
                    .into_iter()
                    .filter(|t| !t.deleted)
                    .collect();
                response.runs = self.listed_runs()?;
                return Ok(response);
            }
        }
        match request {
            ScheduledTasksRequest::List => {}
            ScheduledTasksRequest::Cleanup { mode } => {
                // Cleanup takes its own short lock at commit, after inspecting
                // the transcript queues off the async worker.
                drop(_guard);
                response.cleanup = Some(self.clean_up_runs(mode).await?);
                return Ok(response);
            }
            ScheduledTasksRequest::SetBriefingIndex { enabled } => {
                self.0.store.set_briefing_index(enabled)?;
                response.briefing_index = enabled;
            }
            ScheduledTasksRequest::History { id, before } => {
                response.runs = self.0.store.runs(Some(&id), before)?;
                return Ok(response);
            }
            ScheduledTasksRequest::Preview { schedule } => {
                evaluator::validate(&schedule).map_err(fail)?;
                let mut at = now;
                for _ in 0..5 {
                    let Some(next) = evaluator::next(&schedule, at).map_err(fail)? else {
                        break;
                    };
                    response.preview.push(next);
                    at = next;
                }
                return Ok(response);
            }
            ScheduledTasksRequest::Save {
                id,
                expected_revision,
                mut draft,
                operation_id,
            } => {
                if id.is_empty() || id.len() > 128 {
                    return Err(fail("Task ID must be 1–128 bytes"));
                }
                let old = self.0.store.tasks()?.into_iter().find(|t| t.id == id);
                if old
                    .as_ref()
                    .is_some_and(|t| t.device_id != response.device_id)
                {
                    return Err(fail(
                        "This restored task belongs to another device; import a new paused copy instead of transferring it implicitly",
                    ));
                }

                // An older client omits output on edits and pause/enable.
                // Keep the stored mode until a client explicitly changes it.
                if !output_provided && let Some(task) = &old {
                    draft.output = task.draft.output;
                }
                if let Some(task) = &old
                    && (!after_success_provided || draft.output == ScheduledTaskOutput::Interactive)
                {
                    draft.after_success = task.draft.after_success;
                }
                // A client that predates the setting omits it; keep the stored one.
                if draft.write_to_notion.is_none() && let Some(task) = &old {
                    draft.write_to_notion = task.draft.write_to_notion;
                }
                if let Some(task) = &old {
                    if !chat_mode_provided { draft.chat_mode = task.draft.chat_mode; }
                    if !refresh_provided { draft.refresh = task.draft.refresh; }
                }
                self.validate(&draft)?;
                if draft.enabled {
                    self.validate_model(&draft).await?;
                    if let Some(source) = old.as_ref().and_then(|t| t.import_source.as_deref()) {
                        if !draft.cutover_confirmed {
                            return Err(fail(
                                "Confirm in the task editor that Claude is paused and no source run is active",
                            ));
                        }
                        if old.as_ref().and_then(|t| t.source_handoff_at).is_none() {
                            import::verify_paused(source)?;
                        }
                    }
                }
                let next = if draft.enabled {
                    if old
                        .as_ref()
                        .is_some_and(|t| t.draft.enabled && t.draft.schedule == draft.schedule)
                    {
                        old.as_ref().and_then(|t| t.next_due)
                    } else {
                        evaluator::next(&draft.schedule, now).map_err(fail)?
                    }
                } else {
                    None
                };
                if draft.enabled && next.is_none() {
                    return Err(fail("The schedule has no future occurrence"));
                }
                let task = ScheduledTask {
                    id,
                    revision: expected_revision + 1,
                    device_id: response.device_id.clone(),
                    source_handoff_at: old.as_ref().and_then(|t| t.source_handoff_at).or_else(
                        || {
                            (draft.enabled
                                && old.as_ref().is_some_and(|t| t.import_source.is_some()))
                            .then_some(now)
                        },
                    ),
                    draft,
                    created_at: old.as_ref().map(|t| t.created_at).unwrap_or(now),
                    updated_at: now,
                    next_due: next,
                    deleted: false,
                    import_source: old.and_then(|t| t.import_source),
                };
                self.0
                    .store
                    .save(task, expected_revision, &operation_id, &input)?;
            }
            ScheduledTasksRequest::Delete {
                id,
                expected_revision,
                operation_id,
            } => {
                let mut t = self.find(&id)?;
                t.deleted = true;
                t.draft.enabled = false;
                t.next_due = None;
                t.revision += 1;
                t.updated_at = now;
                self.0
                    .store
                    .save(t, expected_revision, &operation_id, &input)?;
            }
            ScheduledTasksRequest::RunNow { id, operation_id } => {
                let t = self.find(&id)?;
                self.validate(&t.draft)?;
                self.validate_model(&t.draft).await?;
                let active = self.0.store.active()?;
                let busy = active.iter().any(|r| r.task_id == id);
                self.0.store.claim(
                    &t,
                    now,
                    now,
                    Some((&operation_id, &input)),
                    if busy {
                        Some("Previous edition is still active")
                    } else if active.len() >= 64 {
                        Some("Scheduled dispatch queue is full")
                    } else {
                        None
                    },
                )?;
            }
            ScheduledTasksRequest::ImportPreview {
                registry_path,
                timezone,
                space_id,
            } => {
                response.candidates =
                    import::preview(registry_path.as_deref(), &timezone, space_id)?;
                return Ok(response);
            }
            ScheduledTasksRequest::Import {
                candidate,
                operation_id,
            } => {
                let mut draft = candidate.draft;
                draft.enabled = false;
                draft.config.auto_approve = false;
                draft.config.approval_rules = true;
                // A client that predates the setting omits it; imports start off.
                draft.write_to_notion.get_or_insert(false);
                self.validate(&draft)?;
                let id =
                    import::task_id(self.0.store.scope(), &response.device_id, &candidate.source);
                let previous = self.0.store.tasks()?.into_iter().find(|t| t.id == id);
                if previous.as_ref().is_some_and(|t| !t.deleted) {
                    return Err(fail(
                        "This Claude task is already imported; edit it in Mast",
                    ));
                }
                let expected_revision = previous.as_ref().map_or(0, |t| t.revision);
                let t = ScheduledTask {
                    id,
                    revision: expected_revision + 1,
                    device_id: response.device_id.clone(),
                    draft,
                    created_at: previous.as_ref().map_or(now, |t| t.created_at),
                    updated_at: now,
                    next_due: None,
                    deleted: false,
                    import_source: Some(candidate.source),
                    source_handoff_at: None,
                };
                self.0
                    .store
                    .save(t, expected_revision, &operation_id, &input)?;
            }
        }
        // A saved, deleted or imported task can change which runs it owns.
        if mutates {
            *self
                .0
                .identities_checked
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
        response.tasks = self
            .0
            .store
            .tasks()?
            .into_iter()
            .filter(|t| !t.deleted)
            .collect();
        response.runs = self.listed_runs()?;
        Ok(response)
    }
    /// The newest runs across tasks plus every task's own newest run, newest
    /// first, so each task's last outcome is listed however rarely it runs.
    fn listed_runs(&self) -> Result<Vec<ScheduledRun>, EngineError> {
        let mut runs = self.0.store.runs(None, None)?;
        for latest in self.0.store.latest_per_task()? {
            if !runs.iter().any(|r| r.id == latest.id) {
                runs.push(latest);
            }
        }
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        Ok(runs)
    }
    /// When run identities were last reconciled; `None` = due on the next tick.
    #[doc(hidden)]
    pub fn identities_checked_at(&self) -> Option<i64> {
        *self.0.identities_checked.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Rewrite this device's run identities to match its task ledger, in one
    /// registry write. Gated on a synced registry like the orphan sweep in
    /// `spaces.rs`: a replica that has not heard from the server this boot
    /// may hold stale rows, and writing over them would win by LWW.
    pub fn reconcile_run_identities(&self, now: i64) -> Result<usize, EngineError> {
        if !self.0.workspace.registry_synced() {
            return Ok(0);
        }
        let checked = || self.0.identities_checked.lock().unwrap_or_else(|e| e.into_inner());
        if checked().is_some_and(|at| now - at < IDENTITY_RECHECK_MS) {
            return Ok(0);
        }
        let chats = self.0.workspace.read_chats()?;
        let tasks = self.0.store.tasks()?;
        let task_files = tasks
            .iter()
            .filter(|t| !t.deleted)
            .filter_map(|t| Some((t.id.clone(), import::registry_file(t.import_source.as_deref()?)?)))
            .collect();
        let updates = identity::plan(&chats, &tasks, &task_files, self.0.workspace.device_id(), |id| {
            self.0.missing_sources.join(id).is_file()
        });
        let written = self.0.workspace.set_chats_scheduled_task_info(&updates)?;
        *checked() = Some(now);
        if written > 0 {
            tracing::info!(runs = written, "reconciled scheduled run identities");
        }
        Ok(written)
    }
    /// Spaces this device's tasks launch into, paused tasks included: deleting
    /// one would fail every later run of its task.
    pub fn task_space_ids(&self) -> Result<std::collections::HashSet<String>, EngineError> {
        Ok(self
            .0
            .store
            .tasks()?
            .into_iter()
            .filter(|t| !t.deleted && t.device_id == self.0.workspace.device_id())
            .filter_map(|t| t.draft.space_id)
            .collect())
    }
    fn find(&self, id: &str) -> Result<ScheduledTask, EngineError> {
        self.0
            .store
            .tasks()?
            .into_iter()
            .find(|t| t.id == id && !t.deleted && t.device_id == self.0.workspace.device_id())
            .ok_or_else(|| fail("Task not found on this device"))
    }
    fn validate(&self, d: &ScheduledTaskDraft) -> Result<(), EngineError> {
        if d.output == ScheduledTaskOutput::Interactive
            && d.chat_mode == ScheduledTaskChatMode::ContinueChat
            && d.work_item_id.is_none()
        {
            return Err(fail("Continuing a chat requires interactive output and a work item"));
        }
        if d.output == ScheduledTaskOutput::Background && d.work_item_id.is_none() {
            return Err(fail("Background tasks require a work item"));
        }
        if d.name.trim().is_empty()
            || d.name.chars().count() > 120
            || d.prompt.trim().is_empty()
            || d.prompt.len() > 128 * 1024
        {
            return Err(fail(
                "Enter a task name (up to 120 characters) and instructions (up to 128 KiB)",
            ));
        }
        if d.grace_minutes > 1440 {
            return Err(fail("Missed-run grace must be at most 24 hours"));
        }
        if !Path::new(&d.cwd).is_absolute() || !Path::new(&d.cwd).is_dir() {
            return Err(fail(
                "Choose an existing absolute working directory on the run device",
            ));
        }
        if let Some(id) = &d.space_id {
            let s = self
                .0
                .workspace
                .space(id)?
                .ok_or_else(|| fail("Space no longer exists"))?;
            if s.device_id != self.0.workspace.device_id() {
                return Err(fail("Task Space belongs to another device"));
            }
        }
        if let Some(item) = &d.work_item_id {
            let space = d
                .space_id
                .as_deref()
                .filter(|s| self.0.workspace.work_item_space(item).as_deref() == Some(*s))
                .ok_or_else(|| fail("The work item is not in this task's Space"))?;
            let snapshot = mast_doc::materialize_work_outline(
                space,
                &self.0.workspace.work_outline_inputs(space),
            );
            if snapshot.item(item).is_none_or(|i| i.archived_at.is_some()) {
                return Err(fail("The task's work item is archived or deleted"));
            }
        }
        evaluator::validate(&d.schedule).map_err(fail)
    }
    async fn validate_model(&self, d: &ScheduledTaskDraft) -> Result<(), EngineError> {
        let harness = self.0.registry.resolve(d.config.harness)?;
        let models = tokio::time::timeout(Duration::from_secs(20), harness.models())
            .await
            .map_err(|_| fail("Model discovery timed out; check the execution account"))??;
        let models = mast_harness::normalize_model_rows(d.config.harness, models);
        let Some(id) = &d.config.model else {
            return Ok(());
        };
        let model = models
            .iter()
            .find(|m| &m.id == id)
            .ok_or_else(|| fail("The chosen model is not available on this execution device"))?;
        if d.config
            .reasoning
            .is_some_and(|r| !model.reasoning_levels.contains(&r))
        {
            return Err(fail("Thinking level is not supported by this model"));
        }
        for (id, value) in &d.config.model_options {
            if !model.options.iter().any(|o| {
                &o.id == id
                    && o.choices
                        .iter()
                        .any(|c| value.as_str() == Some(c.id.as_str()))
            }) {
                return Err(fail(format!("Unsupported model option: {id}")));
            }
        }
        Ok(())
    }
    pub async fn tick(&self, now: i64) -> Result<(), EngineError> {
        let _guard = self.0.gate.lock().await;
        if self.0.cancel.is_cancelled() {
            return Ok(());
        }
        if let Err(e) = self.reconcile_run_identities(now) {
            tracing::warn!(error = %e, "scheduled run identity reconcile failed");
        }
        // Reconcile before claiming so finished runs do not block the next occurrence.
        for mut r in self.0.store.active()? {
            let previous = r.clone();
            let Some(chat) = r.chat_id.clone() else {
                continue;
            };
            if r.state == ScheduledRunState::Launching {
                continue;
            }
            let msg = crate::work_outline_launch::scheduled_id(&r.id, "message");
            let (command, landed) = self.0.host.message_command_state(&chat, &msg)?;
            let mut awaiting = None;
            if r.snapshot.output == ScheduledTaskOutput::Interactive
                && r.snapshot.chat_mode == ScheduledTaskChatMode::ContinueChat {
                let outcome = store::blocking(|| self.0.sessions.message_turn_outcome(&chat, &msg))?;
                let finalizing = self.0.sessions.message_outcome_finalizing(&chat, &msg)
                    || self.0.sessions.message_restart_pending(&chat);
                r.state = match outcome {
                    Some(DoneStatus::Completed) => ScheduledRunState::Completed,
                    Some(DoneStatus::Errored) => ScheduledRunState::Failed,
                    Some(DoneStatus::Interrupted) => ScheduledRunState::Interrupted,
                    None => match command.as_ref().map(|c| c.status) {
                        Some(SessionCommandStatus::Rejected | SessionCommandStatus::Expired) => ScheduledRunState::Failed,
                        Some(SessionCommandStatus::Cancelled | SessionCommandStatus::Superseded) => ScheduledRunState::Cancelled,
                        Some(SessionCommandStatus::Pending) => ScheduledRunState::Queued,
                        None if landed => match self.0.sessions.session_status(&chat) {
                            Some(s) if s.status == SessionStatus::AwaitingInput => {
                                awaiting = s.awaiting_input;
                                ScheduledRunState::AwaitingInput
                            }
                            Some(s) if s.status == SessionStatus::Working => ScheduledRunState::Running,
                            _ if finalizing => ScheduledRunState::Queued,
                            _ => {
                                r.detail = Some(format!("Outcome unknown for run {} in chat {}; not automatically retried", r.id, chat));
                                ScheduledRunState::Failed
                            }
                        },
                        None => ScheduledRunState::Queued,
                        Some(SessionCommandStatus::Applied) => match self.0.sessions.session_status(&chat) {
                            Some(s) if s.status == SessionStatus::AwaitingInput => {
                                awaiting = s.awaiting_input;
                                ScheduledRunState::AwaitingInput
                            }
                            Some(s) if s.status == SessionStatus::Working => ScheduledRunState::Running,
                            _ if finalizing => ScheduledRunState::Queued,
                            _ => {
                                r.detail = Some(format!("Outcome unknown for run {} in chat {}; not automatically retried", r.id, chat));
                                ScheduledRunState::Failed
                            }
                        },
                    },
                };
            } else if command
                .as_ref()
                .is_some_and(|c| c.status == SessionCommandStatus::Pending)
            {
                r.state = ScheduledRunState::Queued;
            } else if let Some(s) = self.0.sessions.session_status(&chat) {
                r.state = match s.status {
                    SessionStatus::Working => ScheduledRunState::Running,
                    SessionStatus::AwaitingInput => ScheduledRunState::AwaitingInput,
                    SessionStatus::Errored => ScheduledRunState::Failed,
                    SessionStatus::Idle if s.last_completed_turn.is_some() => {
                        ScheduledRunState::Completed
                    }
                    SessionStatus::Idle => ScheduledRunState::Interrupted,
                };
                awaiting = s
                    .awaiting_input
                    .filter(|_| r.state == ScheduledRunState::AwaitingInput);
            } else {
                r.state = match command.as_ref().map(|c| c.status) {
                    Some(SessionCommandStatus::Pending) => ScheduledRunState::Queued,
                    Some(SessionCommandStatus::Rejected | SessionCommandStatus::Expired) => {
                        ScheduledRunState::Failed
                    }
                    Some(SessionCommandStatus::Cancelled | SessionCommandStatus::Superseded) => {
                        ScheduledRunState::Cancelled
                    }
                    Some(SessionCommandStatus::Applied) if landed => {
                        match store::blocking(|| self.0.sessions.last_journaled_turn_outcome(&chat))? {
                            Some(DoneStatus::Completed) => ScheduledRunState::Completed,
                            Some(DoneStatus::Errored) => ScheduledRunState::Failed,
                            Some(DoneStatus::Interrupted) | None => ScheduledRunState::Interrupted,
                        }
                    }
                    _ => ScheduledRunState::Interrupted,
                };
            }
            if matches!(r.state, ScheduledRunState::Failed | ScheduledRunState::Cancelled)
                && !r.detail.as_deref().is_some_and(|detail| detail.starts_with("Outcome unknown for run "))
            {
                r.detail = command.and_then(|c| c.resolution);
            }
            let request = awaiting.as_ref().map(|a| a.request_id.clone());
            if request != r.awaiting_request_id {
                r.awaiting_since = request.is_some().then_some(now);
                r.awaiting_request_id = request;
            }
            // Nobody answers an unattended run; stop it so it frees its slot.
            if let Some(input) = awaiting.filter(|_| {
                r.awaiting_since
                    .is_some_and(|since| now - since >= AWAITING_INPUT_LIMIT_MINUTES * 60_000)
            }) {
                match self.0.sessions.interrupt(&chat).await {
                    Ok(true) => {
                        r.state = ScheduledRunState::Interrupted;
                        let header = input
                            .questions
                            .first()
                            .map(|q| q.header.as_str())
                            .filter(|h| !h.is_empty());
                        r.detail = Some(format!(
                            "Stopped after {AWAITING_INPUT_LIMIT_MINUTES} min waiting for input{}",
                            header.map(|h| format!(": {h}")).unwrap_or_default()
                        ));
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(chat = %chat, error = %e, "scheduled run interrupt failed")
                    }
                }
            }
            // A completed Background run must have published its results; a
            // skipped publication fails the run, so it is neither archived
            // nor left looking successful.
            if r.state == ScheduledRunState::Completed
                && let Some(mut results) = r.results.take()
            {
                match results::published_revision(&self.0.workspace, &r, &results) {
                    Ok(revision) => results.revision = Some(revision),
                    Err(reason) => {
                        r.state = ScheduledRunState::Failed;
                        r.detail = Some(reason);
                    }
                }
                r.results = Some(results);
            }
            if !r.state.active() {
                r.finished_at = Some(now);
            }
            if r != previous {
                let followup = r.state != previous.state
                    || r.awaiting_request_id != previous.awaiting_request_id;
                self.record_run(&r, followup.then_some(now))?;
            }
        }
        let active = self.0.store.active()?;
        let mut queued = active.len();
        for t in self.0.store.tasks()?.iter().filter(|t| {
            !t.deleted && t.draft.enabled && t.device_id == self.0.workspace.device_id()
        }) {
            let Some(first) = t.next_due.filter(|at| *at <= now) else {
                continue;
            };
            let Some(at) =
                evaluator::latest_due(&t.draft.schedule, first, now, t.draft.grace_minutes)
                    .map_err(fail)?
            else {
                continue;
            };
            let missed = now - at > i64::from(t.draft.grace_minutes) * 60_000 + 5_000;
            let busy = active.iter().any(|r| r.task_id == t.id);
            self.0.store.claim(
                t,
                at,
                now,
                None,
                if busy {
                    Some("Previous edition is still active")
                } else if missed {
                    Some("Missed-run grace expired")
                } else if queued >= 64 {
                    Some("Scheduled dispatch queue is full")
                } else {
                    queued += 1;
                    None
                },
            )?;
        }
        let active = self.0.store.active()?;
        let mut running = active
            .iter()
            .filter(|r| r.state != ScheduledRunState::Launching)
            .count();
        for mut r in active
            .into_iter()
            .filter(|r| r.state == ScheduledRunState::Launching)
        {
            let task = self.find(&r.task_id);
            let cancelled = task
                .as_ref()
                .map_or(true, |t| !r.manual && !t.draft.enabled);
            if cancelled {
                r.state = ScheduledRunState::Cancelled;
                r.finished_at = Some(now);
                r.detail = Some("The task was paused or deleted before dispatch".into());
                // The user already acted. There is no unattended run to
                // review when dispatch is cancelled before a chat exists.
                if let Some(chat_id) = &r.chat_id
                    && self.0.workspace.chat(chat_id)?.is_some()
                {
                    self.record_run(&r, Some(now))?;
                } else {
                    self.0.store.update_run(&r)?;
                }
                continue;
            }
            let deadline_base = if r.manual {
                r.created_at
            } else {
                r.scheduled_for
            };
            let grace = if r.manual {
                i64::from(r.snapshot.grace_minutes.max(1)) * 60_000
            } else {
                i64::from(r.snapshot.grace_minutes) * 60_000 + 5_000
            };
            if now - deadline_base > grace {
                r.state = ScheduledRunState::Skipped;
                r.detail = Some("Dispatch queue grace expired".into());
                r.finished_at = Some(now);
                self.0.store.update_run(&r)?;
                continue;
            }
            if running >= 2 {
                continue;
            }
            let model_check = tokio::select! {
                _=self.0.cancel.cancelled()=>return Ok(()),
                result=self.validate_model(&r.snapshot)=>result,
            };
            if self.0.cancel.is_cancelled() {
                return Ok(());
            }
            let launched = match model_check {
                Ok(()) => self.launch(&mut r).await,
                Err(e) => Err(e),
            };
            match launched {
                Ok(true) => {
                    r.state = ScheduledRunState::Queued;
                    running += 1
                }
                Ok(false) => continue,
                // A shutdown while waiting for the shared handoff gate did
                // not dispatch this occurrence. Leave its durable Launching
                // row intact so the next engine can resume it.
                Err(_) if self.0.cancel.is_cancelled() => return Ok(()),
                Err(e) => {
                    r.state = ScheduledRunState::Failed;
                    r.detail = Some(e.to_string());
                    r.finished_at = Some(now);
                }
            }
            self.record_run(&r, Some(now))?;
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.reconcile_followups(now))
            .await
            .map_err(fail)??;
        Ok(())
    }
    async fn launch(&self, r: &mut ScheduledRun) -> Result<bool, EngineError> {
        self.validate(&r.snapshot)?;
        // A rename since the first run must not start a second artifact.
        if let (Some(results), Some(space), Some(item)) =
            (&mut r.results, &r.snapshot.space_id, &r.snapshot.work_item_id)
        {
            let resolved = results::resolve(
                &self.0.workspace,
                space,
                item,
                &r.task_id,
                &r.snapshot.name,
            );
            results.artifact_id = resolved.artifact_id;
            results.title = resolved.title;
        }
        let task = self.find(&r.task_id)?;
        if let Some(source) = task.import_source.as_deref() {
            if !r.snapshot.cutover_confirmed {
                return Err(fail(
                    "Confirm the Claude handoff in the task editor before running",
                ));
            }
            if task.source_handoff_at.is_none() {
                import::verify_paused(source)?;
            }
        }

        // Select the parent or standing holder under the same hand-off gate.
        let _launch = tokio::select! {
            _ = self.0.cancel.cancelled() => return Err(fail("Scheduled task dispatch stopped")),
            guard = self.0.launch_service.lock_launch() => guard,
        };
        if self.0.cancel.is_cancelled() {
            return Err(fail("Scheduled task dispatch stopped"));
        }
        if r.snapshot.output == ScheduledTaskOutput::Interactive
            && r.snapshot.chat_mode == ScheduledTaskChatMode::ContinueChat {
            let input = r.clone();
            return self.0.launch_service.launch_scheduled_standing_locked(&input, &self.0.wakes, |target, generation, source| {
                r.chat_id = Some(target.to_owned());
                r.assistant_generation = generation.map(str::to_owned);
                r.rotation_source_chat_id = source.map(str::to_owned);
                self.0.store.update_run(r)
            }).await;
        }
        crate::work_outline_launch::launch_scheduled_chat(
            &self.0.workspace,
            &self.0.host,
            &self.0.sessions,
            r,
        ).map(|()| true)
    }
}
