use super::{ScheduledTasks, fail, store::Followup};
use crate::EngineError;
use mast_doc::{SessionCommandPayload, SessionCommandStatus, SessionDoc};
use mast_proto::*;

const FOLLOWUP_RETRY_MS: i64 = 60_000;

fn has_queued_turn(doc: &SessionDoc) -> Result<bool, EngineError> {
    Ok(!doc.read_queue()?.is_empty()
        || doc.read_commands()?.iter().any(|command| {
            command.status == SessionCommandStatus::Pending
                && matches!(
                    command.payload,
                    SessionCommandPayload::Run { .. }
                        | SessionCommandPayload::RunWithInstructions { .. }
                        | SessionCommandPayload::Steer { .. }
                        | SessionCommandPayload::RespondInput { .. }
                )
        }))
}

impl ScheduledTasks {
    pub(super) fn record_run(
        &self,
        run: &ScheduledRun,
        changed_at: Option<i64>,
    ) -> Result<(), EngineError> {
        let needs_action = match run.state {
            ScheduledRunState::Completed => true,
            ScheduledRunState::AwaitingInput
            | ScheduledRunState::Failed
            | ScheduledRunState::Interrupted
            | ScheduledRunState::Cancelled => {
                run.snapshot.output == ScheduledTaskOutput::Background
            }
            _ => false,
        };
        self.0
            .store
            .write_run(run, changed_at.filter(|_| needs_action))
    }

    pub(super) fn reconcile_followups(&self, now: i64) -> Result<(), EngineError> {
        if !self.0.workspace.registry_synced() {
            return Ok(());
        }
        let mut done = Vec::new();
        for followup in [Followup::Action, Followup::Archive] {
            for run in self.0.store.due_followups(followup, now)? {
                let result = match followup {
                    Followup::Action => self.reconcile_run_action(&run),
                    Followup::Archive => self.reconcile_run_archive(&run),
                };
                match result {
                    Ok(true) => done.push((followup, run)),
                    Ok(false) => self.0.store.finish_followup(
                        followup,
                        &run,
                        Some(now + FOLLOWUP_RETRY_MS),
                    )?,
                    Err(error) => {
                        tracing::warn!(run = %run.id, %error, "scheduled run follow-up failed");
                        self.0.store.finish_followup(
                            followup,
                            &run,
                            Some(now + FOLLOWUP_RETRY_MS),
                        )?;
                    }
                }
            }
        }
        if !done.is_empty() {
            self.0.workspace.persist_scheduled_followups()?;
            for (followup, run) in done {
                self.0.store.finish_followup(followup, &run, None)?;
            }
        }
        Ok(())
    }

    fn reconcile_run_archive(&self, run: &ScheduledRun) -> Result<bool, EngineError> {
        let Some(chat_id) = run.chat_id.as_deref() else {
            return Ok(true);
        };
        let workspace = &self.0.workspace;
        let tokens = self.0.sessions.agent_tokens();
        // Doc cache code can read the registry under its own lock. Obtain
        // the doc before taking the registry lock, then inspect its contents.
        let doc = self.0.host.scheduled_run_doc(chat_id)?;
        workspace.archive_scheduled_run_if(
            chat_id,
            |chat| {
                chat.is_background_task_run()
                    && chat.scheduled_task_info
                    .as_ref()
                    .is_some_and(|info| info.task_id == run.task_id && info.run_id == run.id)
                    // The normal assistant matcher excludes Background runs,
                    // but a saved designation might still name this chat.
                    && !tokens
                        .personal_assistant()
                        .get()
                        .is_some_and(|assistant| assistant.chat_id == chat.id)
            },
            || {
                if self.0.sessions.turn_in_flight(chat_id) {
                    return Ok(false);
                }
                match &doc {
                    Some(doc) => Ok(!has_queued_turn(doc)?),
                    None => Ok(false),
                }
            },
        )
    }

    fn reconcile_run_action(&self, run: &ScheduledRun) -> Result<bool, EngineError> {
        let workspace = &self.0.workspace;
        let dedupe_key = format!("scheduled-run-failed:{}", run.task_id);
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::System,
            device_id: workspace.device_id().into(),
            chat_id: None,
            message_id: None,
        };
        let requests = workspace.scheduled_task_actions(&dedupe_key);
        let resolve = |request: &UserActionRequestView, note: &str| {
            workspace
                .work_outline_mutate(
                    &WorkOutlineMutation::ResolveUserActionRequest(
                        ResolveUserActionRequestParams {
                            user_action_request_id: request.id.clone(),
                            note: Some(note.into()),
                            operation_id: work_outline::derive_id(&run.id, &request.id),
                            outcome: None,
                            values: None,
                            via: None,
                            answer: None,
                        },
                    ),
                    &actor,
                    &[],
                )
                .map_err(|e| fail(e.message))?;
            Ok::<_, EngineError>(())
        };
        if run.state == ScheduledRunState::Completed {
            for request in &requests {
                resolve(
                    request,
                    &format!("A run of {} completed successfully.", run.snapshot.name),
                )?;
            }
            return Ok(true);
        }

        let (Some(space), Some(item)) = (&run.snapshot.space_id, &run.snapshot.work_item_id) else {
            return Ok(true);
        };
        let snapshot =
            mast_doc::materialize_work_outline(space, &workspace.work_outline_inputs(space));
        if snapshot
            .item(item)
            .is_none_or(|item| item.archived_at.is_some())
        {
            return Ok(true);
        }
        let status = match run.state {
            ScheduledRunState::AwaitingInput => "is waiting for input",
            ScheduledRunState::Failed => "failed",
            ScheduledRunState::Interrupted => "was interrupted",
            ScheduledRunState::Cancelled => "was cancelled",
            _ => return Ok(true),
        };
        let operation_id = work_outline::derive_id(
            &run.id,
            &format!(
                "{status}:{}",
                run.awaiting_request_id.as_deref().unwrap_or_default()
            ),
        );
        let ledger_key = format!("scheduled-action:{operation_id}");
        if workspace.ledger_holds(&ledger_key) {
            return Ok(true);
        }
        let mut links = Vec::new();
        if let Some(chat_id) = &run.chat_id
            && workspace.chat(chat_id)?.is_some()
        {
            links.push(RequestLink {
                label: "Open run chat".into(),
                target: RequestLinkTarget::Chat {
                    id: chat_id.clone(),
                },
            });
        }
        for previous in requests
            .iter()
            .filter(|request| request.work_item_id != *item)
        {
            resolve(
                previous,
                "A newer run reported its outcome on the task’s new work item.",
            )?;
        }
        let request = CreateUserActionRequestParams {
            work_item_id: item.clone(),
            origin: UserActionRequestOrigin {
                kind: UserActionOriginKind::System,
                ..Default::default()
            },
            text: format!("Scheduled task “{}” {status}", run.snapshot.name),
            info: run.detail.as_ref().map(|detail| {
                detail
                    .chars()
                    .take(user_action_limits::MAX_REQUEST_INFO_CHARS)
                    .collect()
            }),
            dedupe_key: Some(dedupe_key),
            operation_id,
            options: vec![],
            links,
            facts: vec![],
            kind: Some(UserActionKind::Ask),
            priority: None,
        };
        let mutation = if let Some(previous) = requests
            .iter()
            .find(|request| request.work_item_id == *item)
        {
            WorkOutlineMutation::UpdateUserActionRequest(UpdateUserActionRequestParams {
                user_action_request_id: previous.id.clone(),
                request,
            })
        } else {
            WorkOutlineMutation::CreateUserActionRequest(request)
        };
        workspace
            .work_outline_mutate(&mutation, &actor, &[ledger_key])
            .map_err(|e| fail(e.message))?;
        Ok(true)
    }
}
