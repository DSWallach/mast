//! Profile-local scheduler. Only the owning engine claims occurrences.
pub mod evaluator;
pub(crate) mod import;
mod store;
use crate::{DocHost, EngineError, SessionsEngine, WorkspaceHost};
use mast_doc::SessionCommandStatus;
use mast_proto::*;
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
pub(crate) fn fail(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

struct Inner {
    store: store::Store,
    workspace: WorkspaceHost,
    host: DocHost,
    sessions: SessionsEngine,
    registry: Arc<crate::HarnessRegistry>,
    gate: tokio::sync::Mutex<()>,
    cancel: CancellationToken,
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
    ) -> Result<Self, EngineError> {
        Ok(Self(Arc::new(Inner {
            store: store::Store::open(root)?,
            workspace,
            host,
            sessions,
            registry,
            gate: tokio::sync::Mutex::new(()),
            cancel: CancellationToken::new(),
        })))
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
        let _guard = self.0.gate.lock().await;
        let now = crate::now_ms();
        let input = serde_json::to_string(&request).map_err(fail)?;
        let mut response = ScheduledTasksSnapshot {
            device_id: self.0.workspace.device_id().into(),
            ..Default::default()
        };
        let operation = match &request {
            ScheduledTasksRequest::Save { operation_id, .. }
            | ScheduledTasksRequest::Delete { operation_id, .. }
            | ScheduledTasksRequest::RunNow { operation_id, .. }
            | ScheduledTasksRequest::Import { operation_id, .. } => Some(operation_id),
            _ => None,
        };
        if let Some(operation) = operation {
            if self.0.store.replay(operation, &input)? {
                response.tasks = self
                    .0
                    .store
                    .tasks()?
                    .into_iter()
                    .filter(|t| !t.deleted)
                    .collect();
                response.runs = self.0.store.runs(None, None)?;
                return Ok(response);
            }
        }
        match request {
            ScheduledTasksRequest::List => {}
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
                draft,
                operation_id,
            } => {
                if id.is_empty() || id.len() > 128 {
                    return Err(fail("Task ID must be 1–128 bytes"));
                }
                self.validate(&draft)?;
                if draft.enabled {
                    self.validate_model(&draft).await?;
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

                if draft.enabled {
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
                self.validate(&draft)?;
                let id =
                    import::task_id(self.0.store.scope(), &response.device_id, &candidate.source);
                if self.0.store.tasks()?.iter().any(|t| t.id == id) {
                    return Err(fail(
                        "This Claude task is already imported; edit it in Mast",
                    ));
                }
                let t = ScheduledTask {
                    id,
                    revision: 1,
                    device_id: response.device_id.clone(),
                    draft,
                    created_at: now,
                    updated_at: now,
                    next_due: None,
                    deleted: false,
                    import_source: Some(candidate.source),
                    source_handoff_at: None,
                };
                self.0.store.save(t, 0, &operation_id, &input)?;
            }
        }
        response.tasks = self
            .0
            .store
            .tasks()?
            .into_iter()
            .filter(|t| !t.deleted)
            .collect();
        response.runs = self.0.store.runs(None, None)?;
        Ok(response)
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
        // Reconcile before claiming so finished runs do not block the next occurrence.
        for mut r in self.0.store.active()? {
            let previous_state = r.state;
            let Some(chat) = r.chat_id.clone() else {
                continue;
            };
            if r.state == ScheduledRunState::Launching {
                continue;
            }
            let msg = crate::work_outline_launch::scheduled_id(&r.id, "message");
            let (command, _) = self.0.host.message_command_state(&chat, &msg)?;
            if command
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
            } else {
                r.state = match command.map(|c| c.status) {
                    Some(SessionCommandStatus::Pending) => ScheduledRunState::Queued,
                    Some(SessionCommandStatus::Rejected | SessionCommandStatus::Expired) => {
                        ScheduledRunState::Failed
                    }
                    Some(SessionCommandStatus::Cancelled | SessionCommandStatus::Superseded) => {
                        ScheduledRunState::Cancelled
                    }
                    _ => ScheduledRunState::Interrupted,
                };
            }
            if !r.state.active() {
                r.finished_at = Some(now);
            }
            if r.state != previous_state {
                self.0.store.update_run(&r)?;
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
                self.0.store.update_run(&r)?;
                continue;
            }
            if running >= 2 {
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
            let model_check = tokio::select! {
                _=self.0.cancel.cancelled()=>return Ok(()),
                result=self.validate_model(&r.snapshot)=>result,
            };
            if self.0.cancel.is_cancelled() {
                return Ok(());
            }
            let launched = match model_check {
                Ok(()) => self.launch(&r),
                Err(e) => Err(e),
            };
            match launched {
                Ok(()) => {
                    r.state = ScheduledRunState::Queued;
                    running += 1
                }
                Err(e) => {
                    r.state = ScheduledRunState::Failed;
                    r.detail = Some(e.to_string());
                    r.finished_at = Some(now);
                }
            }
            self.0.store.update_run(&r)?;
        }
        Ok(())
    }
    fn launch(&self, r: &ScheduledRun) -> Result<(), EngineError> {
        self.validate(&r.snapshot)?;
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

        crate::work_outline_launch::launch_scheduled_chat(&self.0.workspace, &self.0.host, r)
    }
}
