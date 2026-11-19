//! Work Outline launch (Milestone 3 stage 2): `CreateChatForWorkItem` and
//! `ContinueChatForWorkItem` (contract section 4; idempotency audit section
//! 7; execution audit section 5.4).
//!
//! `createChatForWorkItem` is ONE recoverable, idempotent workflow whose
//! state is derived from durable facts (no journal):
//!
//! 1. claim the operation's inputs in the device-local processed-command
//!    ledger (`work-outline:launch:{op}` + `work-outline:launch-in:{hash}:
//!    {op}`), so a replay with other inputs is `operationConflict`;
//! 2. resolve the effective execution configuration (the captured binding
//!    when a previous attempt got that far, else the item's resolution or the
//!    deliberate override, plus the worktree choice: an existing worktree, or
//!    a new `.worktrees/<label>/<slug>` created idempotently for this
//!    operation), validate it on this (owning) device, and check the
//!    harness's instruction capability. Nothing is written before this
//!    passes, and it re-runs on every retry;
//! 3. create the Chat (`derive_id(op, "chat")`) in the item's Space with
//!    `cwd` = the validated execution root (never retargeted later);
//! 4. assign it (only if no assignment exists: a user's later reassignment is
//!    never stomped);
//! 5. capture the execution binding (keyed by Chat id, carries `op`);
//! 6. `createInstructionRevision` (`derive_id(op, "revision")`) and
//!    `applyInstructionRevision` (`derive_id(op, "application")`, reason
//!    `launch`, turn boundary = the first message id);
//! 7. queue the first Turn LAST: `runWithInstructions` pinned to that
//!    revision, command id `derive_id(op, "command")`, message id
//!    `derive_id(op, "message")`. The session doc dedupes the command id and
//!    the processed ledger dedupes execution, so there is exactly one Run.
//!
//! Every step reads before it writes, so a retry after a crash at any step
//! completes the missing ones without duplicates.

use std::path::Path;
use std::sync::{Arc, Mutex};

use mast_doc::{RowState, SessionCommandPayload, SessionCommandStatus};
use mast_proto::work_outline::{WorkOutlineErrorCode, derive_id, roles};
use mast_proto::{
    ApplyInstructionRevisionParams, AssignChatToWorkItemParams, ChatConfig,
    ContinueChatForWorkItemParams, ContinueChatForWorkItemReply, CreateChatForWorkItemParams,
    CreateChatForWorkItemReply, CreateInstructionRevisionParams, ExecutionHostCapabilities,
    ExecutionOverride, ExecutionScope, ExecutionValidationError, InstructionApplicationReason,
    InstructionRevisionPin, InstructionTurnBoundary, RepositoryBinding,
    ResolvedExecutionConfiguration, RunRequest, SandboxLevel, ValidatedExecution,
    WorkItemLaunchState, WorkItemPrompt, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError,
    WorkOutlineMutation, WorktreeChoice,
};
use serde::Serialize;

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::execution_scope::{
    ExecutionScopeService, canonical_lenient, describe_repository, rejection_message,
};
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// Shared deterministic launch identities, including standalone scheduled chats.
pub(crate) fn scheduled_id(operation: &str, role: &str) -> String {
    derive_id(operation, role)
}

/// Standalone variant of the normal durable launch path. Intent and immutable
/// inputs are persisted by the scheduler before this is called. Each step is
/// idempotent, including the first message and command across a crash/retry.
pub(crate) fn launch_scheduled_chat(
    workspace: &WorkspaceHost,
    host: &DocHost,
    sessions: &SessionsEngine,
    run: &mast_proto::ScheduledRun,
) -> Result<(), EngineError> {
    let chat_id = run
        .chat_id
        .as_deref()
        .ok_or_else(|| EngineError::Other("Missing scheduled chat identity".into()))?;
    let draft = &run.snapshot;
    let request = prompt_request(
        &WorkItemPrompt {
            text: crate::scheduled_tasks::results::prompt_with_contract(run),
            attachments: vec![],
        },
        Some(&draft.config),
        &draft.cwd,
    );
    // Check before the first write, like an item launch: a run that cannot
    // start leaves no chat behind to become its task's latest run.
    let item = match draft.work_item_id.as_deref() {
        Some(item) => Some((
            item,
            validate_scheduled_item(workspace, host, sessions, run, chat_id, item, &request)?,
        )),
        None => None,
    };
    let parent = match (draft.space_id.as_deref(), draft.work_item_id.as_deref()) {
        (Some(space), Some(item)) => {
            let snapshot = mast_doc::materialize_work_outline(space, &workspace.work_outline_inputs(space));
            snapshot.project_root(item).and_then(|project| workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &project.id))
        }
        _ => None,
    };
    workspace.create_scheduled_chat(
        chat_id,
        draft.space_id.as_deref(),
        draft.config.clone(),
        draft.cwd.clone(),
        mast_proto::ScheduledChat {
            task_id: run.task_id.clone(),
            task_name: draft.name.clone(),
            session_created_at: Some(run.created_at),
            run_id: run.id.clone(),
            output: draft.output,
            write_to_notion: draft.write_to_notion,
            ..Default::default()
        },
        parent,
    )?;
    if workspace.chat(chat_id)?.is_some_and(|c| c.title.is_none()) {
        workspace.rename_chat(chat_id, &draft.name)?;
    }
    let message_id = scheduled_id(&run.id, roles::MESSAGE);
    let command_id = scheduled_id(&run.id, roles::COMMAND);
    let payload = match item {
        Some((item, deliver)) => {
            scheduled_item_payload(workspace, run, chat_id, item, &message_id, request, deliver)?
        }
        None => SessionCommandPayload::Run {
            request,
            message_id,
        },
    };
    host.queue_command_with_id(chat_id, &command_id, payload)?;
    Ok(())
}

/// The checks an item launch makes before writing anything: Work Outline
/// writes are on, the item still exists, and the harness can receive its
/// instructions when the run delivers them. Returns whether it delivers:
/// the same stable decision as an item launch, so a revision this run
/// already created means it delivers, whatever the item holds now.
fn validate_scheduled_item(
    workspace: &WorkspaceHost,
    host: &DocHost,
    sessions: &SessionsEngine,
    run: &mast_proto::ScheduledRun,
    chat_id: &str,
    work_item_id: &str,
    request: &RunRequest,
) -> Result<bool, EngineError> {
    if crate::work_outline::writes_disabled() {
        return Err(EngineError::Other(
            "Work Outline changes are turned off on this device".into(),
        ));
    }
    let space = workspace
        .work_item_space(work_item_id)
        .ok_or_else(|| EngineError::Other("The task's work item was deleted".into()))?;
    let deliver = workspace.registry_row_state(
        mast_doc::KIND_INSTRUCTION_REVISIONS,
        &derive_id(&run.id, roles::REVISION),
    ) != RowState::Absent
        || !mast_doc::materialize_work_outline(&space, &workspace.work_outline_inputs(&space))
            .effective_instructions(work_item_id)
            .entries
            .is_empty();
    if deliver
        && let mast_harness::InstructionSupport::Unsupported { reason, .. } =
            sessions.instruction_support(host.harness_for_request(chat_id, request))?
    {
        return Err(EngineError::Other(format!(
            "This harness cannot receive the work item's instructions: {reason}"
        )));
    }
    Ok(deliver)
}

/// Attach a scheduled run to its task's work item (steps 4 and 6 of the item
/// launch, with the run as the operation) and pick the first Turn's payload:
/// pinned to the item's instructions when `deliver`. Replay-safe: a retry
/// finds the assignment, revision and application it already wrote.
fn scheduled_item_payload(
    workspace: &WorkspaceHost,
    run: &mast_proto::ScheduledRun,
    chat_id: &str,
    work_item_id: &str,
    message_id: &str,
    request: RunRequest,
    deliver: bool,
) -> Result<SessionCommandPayload, EngineError> {
    let failed = |e: WorkOutlineError| EngineError::Other(e.message);
    let actor = WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: workspace.device_id().to_owned(),
        chat_id: None,
        message_id: None,
    };
    let assigned = workspace
        .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)
        .is_some_and(|row| !row.deleted);
    if !assigned {
        workspace
            .work_outline_mutate(
                &WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
                    chat_id: chat_id.to_owned(),
                    work_item_id: work_item_id.to_owned(),
                    operation_id: derive_id(&run.id, roles::ASSIGNMENT),
                }),
                &actor,
                &[],
            )
            .map_err(failed)?;
    }
    if !deliver {
        return Ok(SessionCommandPayload::Run {
            request,
            message_id: message_id.to_owned(),
        });
    }
    let revision_id = workspace
        .work_outline_mutate(
            &WorkOutlineMutation::CreateInstructionRevision(CreateInstructionRevisionParams {
                work_item_id: work_item_id.to_owned(),
                operation_id: run.id.clone(),
            }),
            &actor,
            &[],
        )
        .and_then(|reply| reply.into_revision_id())
        .map_err(failed)?;
    workspace
        .work_outline_mutate(
            &WorkOutlineMutation::ApplyInstructionRevision(ApplyInstructionRevisionParams {
                chat_id: chat_id.to_owned(),
                revision_id: revision_id.clone(),
                reason: InstructionApplicationReason::Launch,
                turn_boundary: Some(InstructionTurnBoundary {
                    message_id: Some(message_id.to_owned()),
                }),
                operation_id: run.id.clone(),
            }),
            &actor,
            &[],
        )
        .map_err(failed)?;
    let revision = workspace
        .instruction_revision(&revision_id)
        .ok_or_else(|| EngineError::Other("The launch instruction revision is missing".into()))?;
    Ok(SessionCommandPayload::RunWithInstructions {
        request,
        message_id: message_id.to_owned(),
        instructions: InstructionRevisionPin {
            revision_id,
            revision_hash: revision.body.hash.clone(),
        },
        context_reset_confirmed: false,
    })
}

/// Ledger key: this device accepted `operationId` for a launch.
fn ledger_launch_key(operation_id: &str) -> String {
    format!(
        "{}launch:{operation_id}",
        crate::work_outline::LEDGER_KEY_PREFIX
    )
}

/// Ledger key: the inputs `operationId` was accepted with.
fn ledger_launch_inputs_key(inputs_hash: &str, operation_id: &str) -> String {
    format!(
        "{}launch-in:{inputs_hash}:{operation_id}",
        crate::work_outline::LEDGER_KEY_PREFIX
    )
}

/// Ledger key: this operation is about to create the worktree at `path`
/// (claimed BEFORE `git worktree add`, so a retry can tell its own worktree
/// from someone else's).
/// The ledger record of the exact checkout a launch created at `path`
/// (the ledger is a key set, so the checkout id is part of the key). A retry
/// adopts the path only when this record names the checkout found there.
fn ledger_worktree_checkout_key(operation_id: &str, path: &Path, checkout_id: &str) -> String {
    let role = format!("worktree-checkout:{}:{checkout_id}", path.display());
    format!(
        "{}launch-wt-checkout:{}",
        crate::work_outline::LEDGER_KEY_PREFIX,
        derive_id(operation_id, &role)
    )
}

fn ledger_worktree_key(operation_id: &str, path: &Path) -> String {
    let role = format!("worktree:{}", path.display());
    format!(
        "{}launch-wt:{}",
        crate::work_outline::LEDGER_KEY_PREFIX,
        derive_id(operation_id, &role)
    )
}

/// A completed workflow step, for crash injection in tests: the workflow
/// fails right AFTER the named step is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchStep {
    /// Inputs claimed, configuration validated, capability checked.
    Validated,
    ChatCreated,
    Assigned,
    BindingCaptured,
    RevisionCreated,
    RevisionApplied,
    /// The provisional chat, its binding and records are saved (role launches).
    RegistryPersisted,
    TurnQueued,
    /// The new Chat holds the standing role (role launches only).
    RoleAdopted,
}

fn error(code: WorkOutlineErrorCode, message: impl Into<String>) -> WorkOutlineError {
    WorkOutlineError::new(code, message)
}

fn conflict(message: impl Into<String>) -> WorkOutlineError {
    error(WorkOutlineErrorCode::OperationConflict, message)
}

fn engine_error(err: EngineError) -> WorkOutlineError {
    error(WorkOutlineErrorCode::Invalid, err.to_string())
}

/// A validation failure as a Work Outline error (`invalid` with the
/// `execution-scope:<code>: …` rejection text).
fn validation_error(errors: &[ExecutionValidationError]) -> WorkOutlineError {
    error(WorkOutlineErrorCode::Invalid, rejection_message(errors))
}

fn same_path(a: &str, b: &str) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && canonical_lenient(Path::new(a)) == canonical_lenient(Path::new(b))
}

/// What a start is told when the Manager already exists (started by an earlier
/// attempt): the message was not sent, so the dialog keeps its text.
const MANAGER_ALREADY_STARTED: &str =
    "This project's Manager already started; your message wasn't sent. Open the Manager instead.";

fn check_operation_id(operation_id: &str) -> Result<(), WorkOutlineError> {
    let ok = !operation_id.is_empty()
        && operation_id.chars().count() <= mast_proto::work_outline::limits::MAX_OPERATION_ID_CHARS
        && operation_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if ok {
        Ok(())
    } else {
        Err(error(
            WorkOutlineErrorCode::Invalid,
            "operationId must be 1-64 characters of letters, digits, '-', '_', '.', ':'",
        ))
    }
}

fn check_prompt(prompt: &WorkItemPrompt) -> Result<(), WorkOutlineError> {
    if prompt.text.trim().is_empty() && prompt.attachments.is_empty() {
        return Err(error(WorkOutlineErrorCode::Invalid, "The prompt is empty"));
    }
    Ok(())
}

/// The inputs a launch's `operationId` is bound to (everything but the id
/// and the routing hint).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchInputs<'a> {
    work_item_id: &'a str,
    initial_prompt: &'a WorkItemPrompt,
    config: &'a Option<ChatConfig>,
    execution_override: &'a Option<ExecutionOverride>,
    worktree_choice: &'a Option<WorktreeChoice>,
    /// Omitted when absent, so a user launch's hash never changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_chat_id: &'a Option<String>,
    /// Likewise omitted when absent: an operation id used for a role launch
    /// cannot be replayed as a plain one.
    #[serde(skip_serializing_if = "Option::is_none")]
    standing_role: &'a Option<mast_proto::StandingRoleKind>,
    /// Explicit archive choices are part of a user hand-off operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    hand_off_archive: Option<bool>,
}

#[derive(Clone, Copy)]
pub(crate) enum HandOffOrigin<'a> {
    User {
        archive: Option<bool>,
        observed_roles: &'a crate::handoff_roles::RoleObservations,
    },
    Agent,
}

impl HandOffOrigin<'_> {
    fn archive_choice(self) -> Option<bool> {
        match self {
            Self::User { archive, .. } => archive,
            Self::Agent => None,
        }
    }
}

/// The launch workflow and Turn continuation. Cheap to clone.
#[derive(Clone)]
pub struct WorkItemLaunchService {
    workspace: WorkspaceHost,
    doc_host: DocHost,
    sessions: SessionsEngine,
    execution_scope: ExecutionScopeService,
    /// Serializes launches so two concurrent calls with one operation id
    /// cannot interleave their steps.
    gate: Arc<tokio::sync::Mutex<()>>,
    /// Test hook: fail right after this step.
    fault: Arc<Mutex<Option<LaunchStep>>>,
}

impl WorkItemLaunchService {
    #[cfg(test)]
    pub(crate) async fn test_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gate.lock().await
    }
    pub fn new(
        workspace: WorkspaceHost,
        doc_host: DocHost,
        sessions: SessionsEngine,
        execution_scope: ExecutionScopeService,
    ) -> Self {
        Self {
            workspace,
            doc_host,
            sessions,
            execution_scope,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            fault: Arc::new(Mutex::new(None)),
        }
    }

    /// Crash injection (tests): the next workflow fails right after `step`
    /// is durable. `None` clears it.
    pub fn inject_fault(&self, step: Option<LaunchStep>) {
        *self.fault.lock().unwrap_or_else(|e| e.into_inner()) = step;
    }

    fn checkpoint(&self, step: LaunchStep) -> Result<(), WorkOutlineError> {
        let mut fault = self.fault.lock().unwrap_or_else(|e| e.into_inner());
        if *fault == Some(step) {
            *fault = None;
            return Err(error(
                WorkOutlineErrorCode::Invalid,
                format!("injected fault after {step:?}"),
            ));
        }
        Ok(())
    }

    fn actor(&self) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        }
    }

    fn mutate(
        &self,
        mutation: WorkOutlineMutation,
    ) -> Result<mast_proto::WorkOutlineMutateReply, WorkOutlineError> {
        // No ledger keys: every step here is replay-safe in the doc layer
        // (create-if-absent with an immutable inputsHash, compare-before-
        // write), and the launch's own ledger keys guard its inputs.
        self.workspace
            .work_outline_mutate(&mutation, &self.actor(), &[])
    }

    /// Claim the operation's inputs: the first call records them, a replay
    /// with the same inputs proceeds (to complete missing steps), a replay
    /// with other inputs is `operationConflict`.
    fn claim_inputs(&self, operation_id: &str, inputs_hash: &str) -> Result<(), WorkOutlineError> {
        let op_key = ledger_launch_key(operation_id);
        let inputs_key = ledger_launch_inputs_key(inputs_hash, operation_id);
        if self.workspace.ledger_holds(&op_key) {
            return if self.workspace.ledger_holds(&inputs_key) {
                Ok(())
            } else {
                Err(conflict(
                    "operationId was already used for a different launch",
                ))
            };
        }
        // Inputs first: a crash between the two inserts leaves a stray inputs
        // key, never an operation key without its inputs.
        self.workspace
            .ledger_mark(&inputs_key)
            .and_then(|_| self.workspace.ledger_mark(&op_key))
            .map(drop)
            .map_err(engine_error)
    }

    // ── createChatForWorkItem ───────────────────────────────────────────

    /// Run (or resume) the launch workflow on this device, which must own
    /// the work item's Space.
    pub async fn create_chat_for_work_item(
        &self,
        p: &CreateChatForWorkItemParams,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        self.launch_chat_for_work_item(p, None).await
    }

    pub(crate) async fn create_orchestrator_successor(
        &self,
        p: &CreateChatForWorkItemParams,
        wakes: &crate::orchestrator_wakes::OrchestratorWakes,
        source: &str,
        origin: HandOffOrigin<'_>,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        let source_chat = self
            .workspace
            .chat(source)
            .map_err(engine_error)?
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Source chat not found"))?;
        let requested_successor = derive_id(&p.operation_id, roles::CHAT);
        let mut successor = p.clone();
        let configuration = match self
            .workspace
            .checked_chat_execution_binding(source)
            .map_err(engine_error)?
        {
            Some(binding) => binding.resolved.configuration,
            None => mast_proto::ExecutionConfiguration::implicit(
                self.sessions
                    .resolve_cwd(source, source_chat.cwd.as_deref().unwrap_or("~"))
                    .map_err(engine_error)?,
            ),
        };
        successor.execution_override = Some(ExecutionOverride { configuration });
        successor.parent_chat_id = source_chat.parent_chat_id;
        let harness = successor
            .config
            .as_ref()
            .map(|config| config.harness)
            .unwrap_or_else(|| self.doc_host.harness_for(source));
        successor.account_id = self.sessions.chat_account_pin(source, harness);
        // A hand-off that stopped after workers moved is finished by asking
        // again with the same summary and settings, even from a reopened
        // dialog or after a restart that minted a fresh operation id.
        if let Some(operation) = self
            .workspace
            .completed_hand_off_retry_operation(source, &requested_successor)
        {
            successor.operation_id = operation;
        } else if let Some(pending) = self.workspace.part_done_hand_off_from(source)
            && pending.operation_id != successor.operation_id
        {
            let hash = mast_doc::registry::work_outline::inputs_hash(&LaunchInputs {
                work_item_id: &successor.work_item_id,
                initial_prompt: &successor.initial_prompt,
                config: &successor.config,
                execution_override: &successor.execution_override,
                worktree_choice: &successor.worktree_choice,
                parent_chat_id: &successor.parent_chat_id,
                standing_role: &successor.standing_role,
                hand_off_archive: origin.archive_choice(),
            });
            if self
                .workspace
                .ledger_holds(&ledger_launch_inputs_key(&hash, &pending.operation_id))
            {
                successor.operation_id = pending.operation_id;
            }
        }
        self.launch_chat_for_work_item(&successor, Some((wakes, source, origin, &requested_successor)))
            .await
    }

    async fn launch_chat_for_work_item(
        &self,
        p: &CreateChatForWorkItemParams,
        hand_off: Option<(
            &crate::orchestrator_wakes::OrchestratorWakes,
            &str,
            HandOffOrigin<'_>,
            &str,
        )>,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        if crate::work_outline::writes_disabled() {
            return Err(error(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            ));
        }
        check_operation_id(&p.operation_id)?;
        check_prompt(&p.initial_prompt)?;
        let _gate = self.gate.lock().await;
        let op = p.operation_id.as_str();
        let chat_id = derive_id(op, roles::CHAT);
        let message_id = derive_id(op, roles::MESSAGE);
        let command_id = derive_id(op, roles::COMMAND);
        let inputs_hash = mast_doc::registry::work_outline::inputs_hash(&LaunchInputs {
            work_item_id: &p.work_item_id,
            initial_prompt: &p.initial_prompt,
            config: &p.config,
            execution_override: &p.execution_override,
            worktree_choice: &p.worktree_choice,
            parent_chat_id: &p.parent_chat_id,
            standing_role: &p.standing_role,
            hand_off_archive: hand_off.and_then(|(_, _, origin, _)| origin.archive_choice()),
        });
        // The hash above covers what the caller sent; from here on the chat
        // always carries a concrete model, so the harness never inherits its
        // CLI default (a retry derives the same fill).
        let mut filled = with_default_model(p, self.doc_host.harness_for(&chat_id), &chat_id);
        // Ask doesn't gate Claude or ACP agents at all, so a child of a Rules
        // chat that isn't Auto keeps the parent's rules.
        let parent_rules = p
            .parent_chat_id
            .as_deref()
            .and_then(|id| self.workspace.chat_config(id))
            .is_some_and(|c| c.approval_rules && !c.auto_approve);
        if let Some(config) = filled.config.as_mut().filter(|_| parent_rules) {
            if !config.auto_approve {
                config.approval_rules = true;
            }
        }
        let p = &filled;

        // Ownership: the Space must live on this device (the RPC layer
        // forwards everything else to the owner).
        let not_found = || error(WorkOutlineErrorCode::NotFound, "Work item not found");
        let space_id = self
            .workspace
            .work_item_space(&p.work_item_id)
            .ok_or_else(not_found)?;
        let space = self
            .workspace
            .space(&space_id)
            .ok()
            .flatten()
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Space not found"))?;
        if space.device_id != self.workspace.device_id() {
            return Err(validation_error(&[
                ExecutionValidationError::DeviceMismatch {
                    expected_device_id: space.device_id.clone(),
                    found_device_id: self.workspace.device_id().to_owned(),
                },
            ]));
        }
        let snapshot = mast_doc::materialize_work_outline(
            &space_id,
            &self.workspace.work_outline_inputs(&space_id),
        );
        let item = snapshot.item(&p.work_item_id).ok_or_else(not_found)?;
        if item.archived_at.is_some() {
            return Err(error(
                WorkOutlineErrorCode::Archived,
                "The work item is archived",
            ));
        }
        // A role launch is the role's first start: only a project can have a
        // Manager, and a start that finds the Manager already started sends
        // nothing. Each operation creates its own hidden provisional chat; the
        // chat becomes the Manager only after its first message is durably
        // queued. The gate held above serializes launches, so at most one
        // provisional chat of a role has a queued message, and every start
        // finishes that one first.
        if let Some(kind) = p.standing_role {
            if kind != mast_proto::StandingRoleKind::Manager || !item.is_project {
                return Err(error(
                    WorkOutlineErrorCode::Invalid,
                    "Only a project can have a Manager",
                ));
            }
            match self.workspace.standing_role_chat(kind, &p.work_item_id) {
                // This operation's own, already adopted chat: a retry.
                Some(current) if current == chat_id => {}
                Some(_) => return Err(conflict(MANAGER_ALREADY_STARTED)),
                None => {
                    for candidate in self.workspace.provisional_role_chats(kind, &p.work_item_id) {
                        if candidate.id == chat_id {
                            continue;
                        }
                        // Only the recorded launch command counts: a message
                        // that reached the chat any other way is not a start.
                        let launched = match candidate.standing_role_provisional_operation() {
                            Some(operation) => self
                                .doc_host
                                .has_launch_command(&candidate.id, &derive_id(operation, roles::COMMAND))
                                .map_err(engine_error)?,
                            None => false,
                        };
                        if launched {
                            // An earlier start queued its message and stopped
                            // before adopting: surface it, and send nothing more.
                            self.activate_role_chat(kind, &p.work_item_id, &candidate.id)?;
                            return Err(conflict(MANAGER_ALREADY_STARTED));
                        }
                    }
                }
            }
        }
        // Reserve the successor on the source's role before anything is
        // created, moved or queued. A source a role already replaced, a Space
        // the role cannot follow, or another part-done hand-off ends the
        // launch here, with nothing written. The gate held above serializes
        // launches; the reservation survives a restart.
        if let Some((_, source, _, _)) = hand_off {
            self.workspace
                .reserve_standing_role_hand_off(source, &chat_id, op, Some(&space_id))
                .map_err(conflict)?;
        }
        if self
            .workspace
            .registry_row_state(mast_doc::KIND_CHATS, &chat_id)
            == RowState::Tombstoned
        {
            return Err(error(
                WorkOutlineErrorCode::NotFound,
                "The Chat this launch created was deleted; start a new launch",
            ));
        }

        // Instruction delivery is needed only when the item has effective
        // instructions. The decision is stable across retries: an existing
        // Turn or launch revision of this operation fixes it; otherwise the
        // item's current effective instructions decide. With none, the
        // launch writes no revision or application and queues a plain Run
        // (no selection exists, so ingestion pins nothing), so harnesses
        // without instruction support can launch; instructions added later
        // reach the Chat through drift + Apply.
        let deliver_instructions = self.launch_delivers_instructions(
            &chat_id,
            &message_id,
            op,
            snapshot
                .effective_instructions(&p.work_item_id)
                .entries
                .is_empty(),
        )?;
        // Capability before anything is written (ledger included).
        let harness = self.doc_host.harness_for_request(
            &chat_id,
            &prompt_request(&p.initial_prompt, p.config.as_ref(), ""),
        );
        if !self.sessions.harness_installed(harness) {
            let name = serde_json::to_value(harness)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{harness:?}"));
            return Err(error(
                WorkOutlineErrorCode::Capability,
                format!("agent not installed: {name}"),
            ));
        }
        if deliver_instructions {
            self.require_instruction_support(harness)?;
        }

        self.claim_inputs(op, &inputs_hash)?;
        if let Some((
            _,
            source,
            HandOffOrigin::User {
                archive,
                observed_roles,
            },
            _,
        )) = hand_off
        {
            let from = self
                .workspace
                .chat(source)
                .map_err(engine_error)?
                .ok_or_else(|| {
                    error(WorkOutlineErrorCode::NotFound, "Hand-off source not found")
                })?;
            if from.device_id != self.workspace.device_id()
                || from.space_id.as_deref() != Some(space_id.as_str())
            {
                return Err(conflict(
                    "Hand-off source does not belong to this Space and device",
                ));
            }
            let intent = crate::handoff_roles::PendingHandoff::new(&from, &chat_id, archive)
                .ok_or_else(|| conflict("Hand-off source has no Space"))?;
            self.sessions
                .agent_tokens()
                .pending_handoffs
                .prepare(&intent, observed_roles)
                .map_err(|err| engine_error(err.into()))?;
        }

        // ── 2. effective configuration + validation + capability ──
        let existing_binding = self
            .workspace
            .chat_execution_binding_state(&chat_id)
            .checked()
            .map_err(|error| validation_error(&[error]))?;
        let resolved = match &existing_binding {
            Some(binding) => {
                if binding.operation_id.as_deref() != Some(op) {
                    return Err(conflict(
                        "This Chat's execution binding was captured by another operation",
                    ));
                }
                binding.resolved.clone()
            }
            None => self.effective_configuration(p, &space_id).await?,
        };
        let validated = self
            .execution_scope
            .validate_resolved(&resolved, ExecutionHostCapabilities::default())
            .await
            .map_err(|errors| validation_error(&errors))?;
        let mut request = self.launch_request(p, &validated);
        self.checkpoint(LaunchStep::Validated)?;

        // ── 3. Chat ──
        self.ensure_chat(
            &chat_id,
            &space_id,
            &validated.execution_root,
            p.config.clone(),
            p.parent_chat_id.as_deref(),
            p.standing_role.map(|kind| {
                mast_proto::standing_role_provisional_marker(kind, &p.work_item_id, op)
            }),
        )?;
        // The account chip's pick binds before the first run resolves one.
        if let (Some(account_id), Some(config)) = (p.account_id.as_deref(), p.config.as_ref()) {
            self.sessions
                .pin_chat_account(&chat_id, config.harness, account_id)
                .map_err(engine_error)?;
        }
        self.checkpoint(LaunchStep::ChatCreated)?;

        // ── 4. assignment ──
        self.ensure_assignment(&chat_id, &p.work_item_id, op)?;
        self.checkpoint(LaunchStep::Assigned)?;

        // ── 5. execution binding ──
        if existing_binding.is_none() {
            self.workspace.capture_chat_execution_binding(
                &chat_id,
                &resolved,
                op,
                &self.actor(),
            )?;
        }
        self.checkpoint(LaunchStep::BindingCaptured)?;

        // The successor exists now, so the full destination rules apply
        // before workers move or a second turn can be queued.
        if let Some((_, source, _, _)) = hand_off {
            self.workspace
                .check_standing_role_hand_off(source, &chat_id, None)
                .map_err(conflict)?;
        }
        if let Some((wakes, source, _, _)) = hand_off {
            request.prompt.insert_str(0, "[Mast system message, not from the user: coordinator hand-off.]\n");
            request.prompt.push_str(&format!("\n\n[Mast: source chat {source}. Read it with read_chat for the previous conversation and decisions.]"));
            let workers = wakes.hand_off_orchestrator_without_delivery(source, &chat_id, source).await
                .map_err(|detail| error(WorkOutlineErrorCode::Invalid, &detail))?;
            let sessions = self.workspace.read_sessions().map_err(engine_error)?;
            let current = mast_doc::materialize_work_outline(
                &space_id, &self.workspace.work_outline_inputs(&space_id));
            request.prompt.push_str("\n\n[Mast: reparented workers and their open Required Actions]");
            if workers.is_empty() {
                request.prompt.push_str("\nNo active workers.");
            }
            for worker in workers {
                let chat = self.workspace.chat(&worker).map_err(engine_error)?;
                let title = chat.as_ref().and_then(|c| c.title.as_deref()).unwrap_or("Untitled");
                let status = sessions.iter().find(|s| s.chat_id == worker)
                    .map(|s| format!("{:?}", s.status)).unwrap_or_else(|| "Unknown".into());
                request.prompt.push_str(&format!("\n- {worker} | {title} | {status}"));
                for action in current.user_action_requests.iter().filter(|a| a.chat_id == worker && a.is_unresolved()) {
                    request.prompt.push_str(&format!("\n  - Required Action {}: {}", action.id, action.text));
                }
            }
        }

        // ── 6. revision + launch application (only with instructions) ──
        let (revision_id, application_id, payload) = if deliver_instructions {
            let revision_id = self
                .mutate(WorkOutlineMutation::CreateInstructionRevision(
                    CreateInstructionRevisionParams {
                        work_item_id: p.work_item_id.clone(),
                        operation_id: op.to_owned(),
                    },
                ))?
                .into_revision_id()?;
            self.checkpoint(LaunchStep::RevisionCreated)?;
            let application_id = self
                .mutate(WorkOutlineMutation::ApplyInstructionRevision(
                    ApplyInstructionRevisionParams {
                        chat_id: chat_id.clone(),
                        revision_id: revision_id.clone(),
                        reason: InstructionApplicationReason::Launch,
                        turn_boundary: Some(InstructionTurnBoundary {
                            message_id: Some(message_id.clone()),
                        }),
                        operation_id: op.to_owned(),
                    },
                ))?
                .into_application_id()?;
            self.checkpoint(LaunchStep::RevisionApplied)?;
            let revision = self
                .workspace
                .instruction_revision(&revision_id)
                .ok_or_else(|| {
                    error(
                        WorkOutlineErrorCode::NotFound,
                        "The launch instruction revision is missing",
                    )
                })?;
            let payload = SessionCommandPayload::RunWithInstructions {
                request,
                message_id: message_id.clone(),
                instructions: InstructionRevisionPin {
                    revision_id: revision_id.clone(),
                    revision_hash: revision.body.hash.clone(),
                },
                context_reset_confirmed: false,
            };
            (Some(revision_id), Some(application_id), payload)
        } else {
            let payload = SessionCommandPayload::Run {
                request,
                message_id: message_id.clone(),
            };
            (None, None, payload)
        };

        // ── 7. first Turn, last ──
        if let Some(kind) = p.standing_role
            && self
                .workspace
                .standing_role_chat(kind, &p.work_item_id)
                .is_some_and(|current| current != chat_id)
        {
            return Err(conflict(MANAGER_ALREADY_STARTED));
        }
        // A role's first message is queued only once the chat that carries it
        // is saved: the registry row, assignment and binding are what mark the
        // chat provisional and tie it to its folder, and the queued command
        // can start running at once.
        if p.standing_role.is_some() {
            self.workspace.persist_registry_now().map_err(engine_error)?;
        }
        self.checkpoint(LaunchStep::RegistryPersisted)?;
        self.ensure_turn_queued(&chat_id, &command_id, &message_id, payload)?;
        self.checkpoint(LaunchStep::TurnQueued)?;

        // ── 8. standing role ──
        // Last: the Chat stays hidden until its first message is queued AND
        // saved, then one registry batch makes it the Manager. Idempotent on
        // a retry (an adopted chat is returned as is).
        if let Some(kind) = p.standing_role {
            self.activate_role_chat(kind, &p.work_item_id, &chat_id)?;
        }
        self.checkpoint(LaunchStep::RoleAdopted)?;

        let (state, detail) = self.turn_state(&chat_id, &message_id)?;
        if let Some((wakes, source, origin, requested)) = hand_off {
            if state != WorkItemLaunchState::Rejected {
                let handle = self.doc_host.open(source).map_err(engine_error)?;
                handle
                    .write_user_message(
                        &derive_id(op, "hand-off-notice"),
                        &format!("[Mast hand-off: {chat_id}]"),
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|err| error(WorkOutlineErrorCode::Invalid, err.to_string()))?;
                self.complete_hand_off(source, &chat_id, requested, op, &space_id, origin)?;
            }
            wakes.finish_orchestrator_hand_off(&chat_id);
            wakes.evaluate(&chat_id, false).await;
        }
        Ok(CreateChatForWorkItemReply {
            chat_id,
            message_id,
            command_id,
            revision_id,
            application_id,
            execution_root: validated.execution_root,
            state,
            detail,
        })
    }

    fn complete_hand_off(
        &self,
        source: &str,
        successor: &str,
        requested: &str,
        operation_id: &str,
        space_id: &str,
        origin: HandOffOrigin<'_>,
    ) -> Result<(), WorkOutlineError> {
        let invalid = |detail: String| error(WorkOutlineErrorCode::Invalid, detail);
        let HandOffOrigin::User { archive, .. } = origin else {
            return self
                .workspace
                .rotate_standing_role_for_chat(source, successor)
                .map(drop)
                .map_err(invalid);
        };
        let chat = |id| {
            self.workspace
                .chat(id)
                .map_err(engine_error)?
                .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Hand-off chat not found"))
        };
        let from = chat(source)?;
        let to = chat(successor)?;
        if [&from, &to].iter().any(|chat| {
            chat.space_id.as_deref() != Some(space_id)
                || chat.device_id != self.workspace.device_id()
        }) || to.scheduled_task_info.is_some()
        {
            return Err(conflict(
                "Hand-off chats must belong to the same Space and device",
            ));
        }
        let tokens = self.sessions.agent_tokens();
        let intent = crate::handoff_roles::PendingHandoff::new(&from, successor, archive)
            .ok_or_else(|| conflict("Hand-off source has no Space"))?;
        // Publish the registry completion only after recovery can find the
        // roles and the successor's first turn is durable.
        self.doc_host
            .persist_chat_now(successor)
            .map_err(engine_error)?;
        self.workspace
            .complete_user_chat_hand_off(source, successor, requested, operation_id, archive)
            .map_err(invalid)?;
        self.workspace
            .persist_registry_now()
            .map_err(engine_error)?;
        tokens
            .pending_handoffs
            .finish(&self.workspace, &tokens, &intent, successor)
            .map_err(engine_error)
    }

    /// Make a provisional first-start chat the role's chat: save its session
    /// doc (the queued message), adopt it in one registry batch, then save the
    /// registry, so a success is on disk. Every activation goes through here.
    fn activate_role_chat(
        &self,
        kind: mast_proto::StandingRoleKind,
        item_id: &str,
        chat_id: &str,
    ) -> Result<(), WorkOutlineError> {
        self.doc_host.persist_chat_now(chat_id).map_err(engine_error)?;
        self.workspace
            .activate_standing_role_chat(kind, item_id, chat_id, &[])
            .map_err(conflict)?;
        self.workspace.persist_registry_now().map_err(engine_error)
    }

    /// Whether this launch delivers a Work Outline instruction revision (see
    /// the call site). `no_effective` = the item has no effective
    /// instructions right now.
    fn launch_delivers_instructions(
        &self,
        chat_id: &str,
        message_id: &str,
        operation_id: &str,
        no_effective: bool,
    ) -> Result<bool, WorkOutlineError> {
        if self
            .workspace
            .chat(chat_id)
            .map_err(engine_error)?
            .is_some()
        {
            let (latest, _) = self
                .doc_host
                .message_command_state(chat_id, message_id)
                .map_err(engine_error)?;
            match latest.map(|entry| entry.payload) {
                Some(SessionCommandPayload::RunWithInstructions { .. }) => return Ok(true),
                Some(SessionCommandPayload::Run { .. }) => return Ok(false),
                _ => {}
            }
        }
        let revision = derive_id(operation_id, roles::REVISION);
        if self
            .workspace
            .registry_row_state(mast_doc::KIND_INSTRUCTION_REVISIONS, &revision)
            != RowState::Absent
        {
            return Ok(true);
        }
        Ok(!no_effective)
    }

    /// The configuration this launch captures: the override or the item's
    /// whole-row resolution, with the worktree choice applied.
    async fn effective_configuration(
        &self,
        p: &CreateChatForWorkItemParams,
        space_id: &str,
    ) -> Result<ResolvedExecutionConfiguration, WorkOutlineError> {
        let mut resolved = self
            .workspace
            .resolve_execution(&ExecutionScope::WorkItem {
                work_item_id: p.work_item_id.clone(),
            })?;
        if resolved.space_id != space_id {
            return Err(error(
                WorkOutlineErrorCode::CrossSpace,
                "The work item's execution configuration belongs to another Space",
            ));
        }
        if let Some(over) = &p.execution_override {
            resolved.configuration = over.configuration.clone();
            resolved.invalid_reason =
                mast_proto::check_execution_configuration(&over.configuration).err();
        }
        if let Some(choice) = &p.worktree_choice {
            // A Manager start's worktree belongs to the role, not the attempt,
            // so an interrupted attempt's clean checkout is reused by the next
            // one with the same name instead of blocking it. The text has
            // spaces, which an operation id never has.
            let owner = match p.standing_role {
                Some(kind) => format!("standing role worktree {} {}", kind.as_str(), p.work_item_id),
                None => p.operation_id.clone(),
            };
            self.apply_worktree_choice(&mut resolved, choice, &owner).await?;
        }
        Ok(resolved)
    }

    /// Pin the chosen worktree on the configuration's target repository (the
    /// primary binding, else the only binding, else the repository at the
    /// execution root). The execution root must be that repository's
    /// checkout (else `invalid`, before any worktree exists); the worktree
    /// becomes the execution root.
    async fn apply_worktree_choice(
        &self,
        resolved: &mut ResolvedExecutionConfiguration,
        choice: &WorktreeChoice,
        operation_id: &str,
    ) -> Result<(), WorkOutlineError> {
        let invalid = |message: &str| error(WorkOutlineErrorCode::Invalid, message);
        let configuration = &mut resolved.configuration;
        let target = match configuration.bindings.iter().position(|b| b.primary) {
            Some(index) => Some(index),
            None if configuration.bindings.len() == 1 => Some(0),
            None if configuration.bindings.is_empty() => None,
            None => {
                return Err(invalid(
                    "Mark one repository as primary to choose a worktree for this launch",
                ));
            }
        };
        let probe = self.execution_scope.probe();
        // The repository the worktree belongs to.
        let mut binding = match target {
            Some(index) => {
                let binding = configuration.bindings[index].clone();
                if binding.worktree_id.is_some() || binding.worktree_path.is_some() {
                    return Err(invalid(
                        "The execution configuration already pins a worktree for this repository",
                    ));
                }
                binding
            }
            None => {
                let root = Path::new(&configuration.execution_root);
                let identity = describe_repository(probe, root).await.map_err(|_| {
                    invalid("The execution root is not a Git checkout, so it has no worktrees")
                })?;
                RepositoryBinding {
                    repo_id: identity.repo_id,
                    repo_fingerprint: identity.repo_fingerprint,
                    repo_path: identity.repo_path,
                    member: true,
                    primary: true,
                    worktree_id: None,
                    worktree_path: None,
                    expected_ref: None,
                }
            }
        };
        // The chosen worktree becomes the execution root, so the root must be
        // that repository's checkout. Otherwise the agent would run in the
        // root while the worktree sat unused (or outside it): refuse before
        // any worktree is created.
        let root_is_repo = target.is_none()
            || same_path(&configuration.execution_root, &binding.repo_path)
            || same_path(&configuration.execution_root, binding.checkout_path());
        if !root_is_repo {
            return Err(invalid(&format!(
                "A worktree can only be chosen when the execution root is the primary \
                 repository's checkout; the execution root {} is not {}. Start without a \
                 worktree, or set the execution root to the repository in the execution \
                 configuration.",
                configuration.execution_root, binding.repo_path
            )));
        }
        let (worktree_id, worktree_path) = match choice {
            WorktreeChoice::UseExisting {
                worktree_id,
                worktree_path,
            } => (worktree_id.clone(), worktree_path.clone()),
            WorktreeChoice::CreateNew { name } => {
                let (path, checkout_id) = self
                    .create_worktree_for_operation(&binding, name, operation_id)
                    .await?;
                (checkout_id, path)
            }
        };
        binding.worktree_id = Some(worktree_id);
        binding.worktree_path = Some(worktree_path.clone());
        configuration.execution_root = worktree_path;
        match target {
            Some(index) => configuration.bindings[index] = binding,
            None => configuration.bindings.push(binding),
        }
        Ok(())
    }

    /// Create `.worktrees/<label>/<slug(name)>` for `binding`'s repository
    /// with the existing chat-worktree flow, idempotently for this operation:
    /// the target path is claimed in the ledger before `git worktree add`,
    /// and the created checkout's id is recorded after it; a retry that finds
    /// the path reuses it only if this operation claimed it AND recorded
    /// exactly the checkout found there (a worktree someone else put at the
    /// path is never adopted). Any failure fails the launch (never the base
    /// checkout). Returns `(path, checkout id)`.
    async fn create_worktree_for_operation(
        &self,
        binding: &RepositoryBinding,
        name: &str,
        operation_id: &str,
    ) -> Result<(String, String), WorkOutlineError> {
        let repo_path = std::path::PathBuf::from(&binding.repo_path);
        let name_owned = name.to_owned();
        let target = {
            let repo_path = repo_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::chat_workspace_plan::chat_worktree_target(&repo_path, &name_owned)
            })
            .await
            .map_err(|e| engine_error(EngineError::Other(e.to_string())))?
            .map_err(engine_error)?
        };
        let (path, _slug) = target;
        let key = ledger_worktree_key(operation_id, &path);
        if path.exists() {
            if !self.workspace.ledger_holds(&key) {
                return Err(error(
                    WorkOutlineErrorCode::Invalid,
                    format!(
                        "A worktree already exists at {}; choose another name or select it as an \
                         existing worktree",
                        path.display()
                    ),
                ));
            }
            let identity = describe_repository(self.execution_scope.probe(), &path)
                .await
                .map_err(engine_error)?;
            if identity.repo_id != binding.repo_id {
                return Err(error(
                    WorkOutlineErrorCode::Invalid,
                    format!(
                        "{} is not a worktree of the configured repository",
                        path.display()
                    ),
                ));
            }
            let recorded = ledger_worktree_checkout_key(operation_id, &path, &identity.checkout_id);
            if !self.workspace.ledger_holds(&recorded) {
                return Err(error(
                    WorkOutlineErrorCode::Invalid,
                    format!(
                        "The worktree at {} is not the one this launch created. Remove or move \
                         it, or start the Chat with another worktree name or by selecting it as \
                         an existing worktree.",
                        path.display()
                    ),
                ));
            }
            return Ok((
                canonical_lenient(&path).to_string_lossy().into_owned(),
                identity.checkout_id,
            ));
        }
        self.workspace.ledger_mark(&key).map_err(engine_error)?;
        let name_owned = name.to_owned();
        let created = tokio::task::spawn_blocking(move || {
            crate::chat_workspace_plan::create_chat_worktree(&repo_path, &name_owned)
        })
        .await
        .map_err(|e| engine_error(EngineError::Other(e.to_string())))?
        .map_err(|err| {
            error(
                WorkOutlineErrorCode::Invalid,
                format!("Could not create the worktree: {err}"),
            )
        })?;
        let created_path = canonical_lenient(Path::new(&created.worktree_path));
        let identity = describe_repository(self.execution_scope.probe(), &created_path)
            .await
            .map_err(engine_error)?;
        // Recorded under the claimed target path (the retry looks it up there).
        self.workspace
            .ledger_mark(&ledger_worktree_checkout_key(
                operation_id,
                &path,
                &identity.checkout_id,
            ))
            .map_err(engine_error)?;
        Ok((
            created_path.to_string_lossy().into_owned(),
            identity.checkout_id,
        ))
    }

    /// The first Turn's request: the visible prompt (attachment refs ride the
    /// text exactly as a composer `Run` sends them), the picked config, and
    /// the validated execution root. No worktree directive, no envelope (the
    /// host resolves the pinned revision).
    fn launch_request(
        &self,
        p: &CreateChatForWorkItemParams,
        validated: &ValidatedExecution,
    ) -> RunRequest {
        prompt_request(
            &p.initial_prompt,
            p.config.as_ref(),
            &validated.execution_root,
        )
    }

    fn require_instruction_support(
        &self,
        harness: mast_proto::HarnessId,
    ) -> Result<(), WorkOutlineError> {
        let support = self.sessions.instruction_support(harness).map_err(|err| {
            error(
                WorkOutlineErrorCode::Capability,
                format!("The harness cannot be used on this device: {err}"),
            )
        })?;
        match support {
            mast_harness::InstructionSupport::Supported { .. } => Ok(()),
            mast_harness::InstructionSupport::Unsupported {
                reason,
                upgrade_hint,
            } => {
                let mut message =
                    format!("This harness cannot receive Work Outline instructions: {reason}");
                if let Some(hint) = upgrade_hint {
                    message.push_str(&format!(" ({hint})"));
                }
                Err(error(WorkOutlineErrorCode::Capability, message))
            }
        }
    }

    /// Create the Chat row, or verify the existing one is this launch's
    /// (same Space, same execution root). Never retargets.
    fn ensure_chat(
        &self,
        chat_id: &str,
        space_id: &str,
        execution_root: &str,
        config: Option<ChatConfig>,
        parent_chat_id: Option<&str>,
        provisional_tag: Option<String>,
    ) -> Result<(), WorkOutlineError> {
        match self.workspace.chat(chat_id).map_err(engine_error)? {
            Some(chat) => {
                if chat.space_id.as_deref() != Some(space_id) {
                    return Err(conflict("The launch's Chat belongs to another Space"));
                }
                if parent_chat_id.is_some() && chat.parent_chat_id.as_deref() != parent_chat_id {
                    return Err(conflict("The launch's Chat has another parent Chat"));
                }
                if !same_path(chat.cwd.as_deref().unwrap_or(""), execution_root) {
                    return Err(conflict("The launch's Chat was created in another folder"));
                }
                Ok(())
            }
            None => match provisional_tag {
                Some(tag) => self.workspace.create_provisional_role_chat(
                    chat_id,
                    space_id,
                    config,
                    execution_root.to_owned(),
                    tag,
                ),
                None => self.workspace.create_chat_with_parent(
                    chat_id,
                    Some(space_id),
                    None,
                    config,
                    Some(execution_root.to_owned()),
                    parent_chat_id.map(str::to_owned),
                ),
            }
            .map_err(engine_error),
        }
    }

    /// Assign the Chat to the item unless an assignment row already exists
    /// (a retry never stomps a reassignment the user made meanwhile).
    fn ensure_assignment(
        &self,
        chat_id: &str,
        work_item_id: &str,
        operation_id: &str,
    ) -> Result<(), WorkOutlineError> {
        let row = self
            .workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)
            .filter(|row| !row.deleted);
        if let Some(row) = row {
            let current = row.fields.get("workItemId").and_then(|v| v.as_str());
            if current == Some(work_item_id) {
                return Ok(());
            }
            return Err(conflict(
                "The Chat was moved to another work item after this launch started",
            ));
        }
        self.mutate(WorkOutlineMutation::AssignChatToWorkItem(
            AssignChatToWorkItemParams {
                chat_id: chat_id.to_owned(),
                work_item_id: work_item_id.to_owned(),
                operation_id: derive_id(operation_id, roles::ASSIGNMENT),
            },
        ))
        .map(drop)
    }

    /// Queue the Turn with its derived command id unless it is already
    /// there; re-issue a dead attempt (rejected or expired before its message
    /// landed) through the existing retry path.
    fn ensure_turn_queued(
        &self,
        chat_id: &str,
        command_id: &str,
        message_id: &str,
        payload: SessionCommandPayload,
    ) -> Result<(), WorkOutlineError> {
        let (latest, landed) = self
            .doc_host
            .message_command_state(chat_id, message_id)
            .map_err(engine_error)?;
        if landed {
            return Ok(());
        }
        match latest {
            None => self
                .doc_host
                .queue_launch_command_with_id(chat_id, command_id, payload)
                .map(drop)
                .map_err(engine_error),
            Some(entry)
                if matches!(
                    entry.status,
                    SessionCommandStatus::Rejected | SessionCommandStatus::Expired
                ) =>
            {
                self.doc_host.retry_delivery(chat_id).map_err(engine_error)
            }
            Some(entry) => {
                if command_prompt(&entry.payload) != command_prompt(&payload) {
                    return Err(conflict(
                        "operationId was already used for a different prompt",
                    ));
                }
                Ok(())
            }
        }
    }

    /// The state of the Turn carrying `message_id` (its latest attempt).
    fn turn_state(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<(WorkItemLaunchState, Option<String>), WorkOutlineError> {
        let (latest, landed) = self
            .doc_host
            .message_command_state(chat_id, message_id)
            .map_err(engine_error)?;
        Ok(match latest {
            Some(entry) => match entry.status {
                SessionCommandStatus::Pending => (WorkItemLaunchState::Queued, None),
                SessionCommandStatus::Applied => (WorkItemLaunchState::Started, entry.resolution),
                _ => (
                    WorkItemLaunchState::Rejected,
                    entry.resolution.or(Some(format!("{:?}", entry.status))),
                ),
            },
            None if landed => (WorkItemLaunchState::Started, None),
            None => (WorkItemLaunchState::Queued, None),
        })
    }

    // ── continueChatForWorkItem ─────────────────────────────────────────

    /// Queue the next Turn of a work-item Chat with derived ids. The engine
    /// pins the Chat's selected revision at ingestion. A bound Chat hosted
    /// here is validated now; one hosted elsewhere queues durably and its
    /// host revalidates at drain (decision 6.4.1).
    pub async fn continue_chat_for_work_item(
        &self,
        p: &ContinueChatForWorkItemParams,
    ) -> Result<ContinueChatForWorkItemReply, WorkOutlineError> {
        check_operation_id(&p.operation_id)?;
        check_prompt(&p.prompt)?;
        let op = p.operation_id.as_str();
        let message_id = derive_id(op, roles::MESSAGE);
        let command_id = derive_id(op, roles::COMMAND);
        let chat = self
            .workspace
            .chat(&p.chat_id)
            .map_err(engine_error)?
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Chat not found"))?;
        // A Manager's first start takes only its own launch's message.
        if chat.standing_role_provisional().is_some() {
            return Err(error(
                WorkOutlineErrorCode::Invalid,
                "This Manager has not started yet; start it from the project's Manager row",
            ));
        }
        // A malformed binding is bound-and-invalid: refuse, never treat the
        // Chat as unbound.
        let binding = self
            .workspace
            .chat_execution_binding_state(&p.chat_id)
            .checked()
            .map_err(|error| validation_error(&[error]))?;
        let assigned = self
            .workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id)
            .filter(|row| !row.deleted)
            .and_then(|row| {
                row.fields
                    .get("workItemId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .is_some_and(|item| !item.is_empty());
        if binding.is_none() && !assigned {
            return Err(error(
                WorkOutlineErrorCode::Invalid,
                "This Chat is not associated with a work item",
            ));
        }
        let cwd = match &binding {
            Some(binding) if chat.device_id == self.workspace.device_id() => self
                .execution_scope
                .validate_for_dispatch(&p.chat_id, ExecutionHostCapabilities::default())
                .await
                .map_err(|errors| validation_error(&errors))?
                .map(|validated| validated.execution_root)
                .unwrap_or_else(|| binding.resolved.configuration.execution_root.clone()),
            Some(binding) => binding.resolved.configuration.execution_root.clone(),
            // An empty cwd would run in the engine's own directory: refuse.
            None => chat
                .cwd
                .clone()
                .filter(|cwd| !cwd.trim().is_empty())
                .ok_or_else(|| {
                    error(
                        WorkOutlineErrorCode::Invalid,
                        "This Chat has no folder to run in. Set the Chat's folder, or start a \
                         new Chat from the work item.",
                    )
                })?,
        };
        let mut request = prompt_request(&p.prompt, chat.config.as_ref(), &cwd);
        // Inputs are bound to the operation by the command itself: a replay
        // with another prompt finds the derived command carrying different
        // text (`operationConflict`).
        if request.harness.is_none() {
            request.harness = Some(self.doc_host.harness_for(&p.chat_id));
        }
        if let Some(harness) = request.harness
            && request.model.as_deref().is_none_or(|m| m.trim().is_empty())
        {
            request.model = mast_proto::resolve_model(harness, None);
            tracing::debug!(chat = %p.chat_id, model = ?request.model, "continued turn had no model; filled in the Mast default");
        }
        let payload = SessionCommandPayload::Run {
            request,
            message_id: message_id.clone(),
        };
        let (latest, landed) = self
            .doc_host
            .message_command_state(&p.chat_id, &message_id)
            .map_err(engine_error)?;
        match latest {
            Some(entry) => {
                if command_prompt(&entry.payload) != command_prompt(&payload) {
                    return Err(conflict(
                        "operationId was already used for a different prompt",
                    ));
                }
            }
            None if landed => {}
            None => {
                self.doc_host
                    .queue_command_with_id(&p.chat_id, &command_id, payload)
                    .map_err(|err| match err {
                        EngineError::InstructionDelivery(rejection) => {
                            error(WorkOutlineErrorCode::Capability, rejection.message.clone())
                        }
                        other => engine_error(other),
                    })?;
            }
        }
        let (state, detail) = self.turn_state(&p.chat_id, &message_id)?;
        Ok(ContinueChatForWorkItemReply {
            chat_id: p.chat_id.clone(),
            message_id,
            command_id,
            state,
            detail,
        })
    }

    // ── child Chats ─────────────────────────────────────────────────────

    /// A Mast-created child Chat (`parentChatId`) inherits its parent's
    /// work-item assignment, selected instruction revision (application
    /// reason `forkInherit`), and execution binding (acceptance 19). Ids
    /// derive from the child's id, so a retried create is a no-op. Each part
    /// is skipped when it does not apply (unassigned parent, other Space,
    /// no selection, unbound parent, or a child created in another folder
    /// than the parent's execution root).
    pub fn inherit_from_parent(
        &self,
        child_id: &str,
        parent_id: &str,
    ) -> Result<(), WorkOutlineError> {
        if crate::work_outline::writes_disabled() {
            return Ok(());
        }
        let Some(child) = self.workspace.chat(child_id).map_err(engine_error)? else {
            return Ok(());
        };
        let Some(parent) = self.workspace.chat(parent_id).map_err(engine_error)? else {
            return Ok(());
        };
        if child.space_id.is_none() || child.space_id != parent.space_id {
            return Ok(());
        }
        let op = derive_id(child_id, "fork-inherit");
        let parent_item = self
            .workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, parent_id)
            .filter(|row| !row.deleted)
            .and_then(|row| {
                row.fields
                    .get("workItemId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .filter(|item| !item.is_empty());
        let Some(item) = parent_item else {
            return Ok(());
        };
        let child_assignment = self
            .workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, child_id)
            .filter(|row| !row.deleted);
        if child_assignment.is_none() {
            self.mutate(WorkOutlineMutation::AssignChatToWorkItem(
                AssignChatToWorkItemParams {
                    chat_id: child_id.to_owned(),
                    work_item_id: item,
                    operation_id: derive_id(&op, roles::ASSIGNMENT),
                },
            ))?;
        }
        if let Some(binding) = self.workspace.chat_execution_binding(parent_id)
            && self.workspace.chat_execution_binding(child_id).is_none()
            && same_path(
                child.cwd.as_deref().unwrap_or(""),
                &binding.resolved.configuration.execution_root,
            )
        {
            self.workspace.capture_chat_execution_binding(
                child_id,
                &binding.resolved,
                &op,
                &self.actor(),
            )?;
        }
        if let Some(pin) = self.workspace.selected_instruction_revision(parent_id) {
            self.mutate(WorkOutlineMutation::ApplyInstructionRevision(
                ApplyInstructionRevisionParams {
                    chat_id: child_id.to_owned(),
                    revision_id: pin.revision_id,
                    reason: InstructionApplicationReason::ForkInherit,
                    turn_boundary: None,
                    operation_id: op,
                },
            ))?;
        }
        Ok(())
    }
}

/// `p` with an explicit model: an absent config becomes the host's default
/// harness with default options, and an empty model becomes that harness's
/// built-in default.
fn with_default_model(
    p: &CreateChatForWorkItemParams,
    default_harness: mast_proto::HarnessId,
    chat_id: &str,
) -> CreateChatForWorkItemParams {
    let mut p = p.clone();
    let mut config = p.config.take().unwrap_or_else(|| ChatConfig { approval_rules: false,
        harness: default_harness,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    });
    if config.fill_default_model() {
        tracing::debug!(chat = %chat_id, "work item launch had no model; filled in the Mast default");
    }
    p.config = Some(config);
    p
}

/// A Turn's request from a prompt, a chat config, and a cwd.
fn prompt_request(prompt: &WorkItemPrompt, config: Option<&ChatConfig>, cwd: &str) -> RunRequest {
    RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: config.is_some_and(|c| c.approval_rules),
        prompt: crate::doc_host::queued_message_prompt(&prompt.text, &prompt.attachments),
        harness: config.map(|c| c.harness),
        model: config.and_then(|c| c.model.clone()),
        reasoning: config.and_then(|c| c.reasoning),
        model_options: config.map(|c| c.model_options.clone()).unwrap_or_default(),
        cwd: cwd.to_owned(),
        sandbox: config
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite),
        auto_approve: config.is_some_and(|c| c.auto_approve),
        resume: None,
        fork_from: None,
        attachments: prompt.attachments.clone(),
        worktree: None,
        instructions: None,
    }
}

fn command_prompt(payload: &SessionCommandPayload) -> Option<&str> {
    match payload {
        SessionCommandPayload::Run { request, .. }
        | SessionCommandPayload::RunWithInstructions { request, .. } => Some(&request.prompt),
        SessionCommandPayload::Steer { prompt, .. } => Some(prompt),
        _ => None,
    }
}

#[cfg(test)]
mod default_model_tests {
    use super::*;
    use mast_proto::HarnessId;

    fn params(config: Option<ChatConfig>) -> CreateChatForWorkItemParams {
        CreateChatForWorkItemParams {
            work_item_id: "item".into(),
            initial_prompt: WorkItemPrompt {
                text: "go".into(),
                attachments: Vec::new(),
            },
            config,
            execution_override: None,
            worktree_choice: None,
            operation_id: "op".into(),
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: None,
        }
    }

    #[test]
    fn a_launch_without_a_config_carries_the_default_model() {
        let p = with_default_model(&params(None), HarnessId::ClaudeCode, "chat");
        let config = p.config.expect("config filled in");
        assert_eq!(config.harness, HarnessId::ClaudeCode);
        assert_eq!(config.model.as_deref(), Some("claude-opus-5-5"));
        let request = prompt_request(&p.initial_prompt, Some(&config), "/w");
        assert_eq!(request.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn a_launch_keeps_an_explicit_model_and_fills_an_empty_one() {
        let config = |model: Option<&str>| ChatConfig { approval_rules: false,
            harness: HarnessId::ClaudeCode,
            model: model.map(str::to_owned),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        };
        let kept = with_default_model(
            &params(Some(config(Some("claude-sonnet-5")))),
            HarnessId::ClaudeCode,
            "c",
        );
        assert_eq!(
            kept.config.unwrap().model.as_deref(),
            Some("claude-sonnet-5")
        );
        let filled = with_default_model(&params(Some(config(None))), HarnessId::ClaudeCode, "c");
        assert_eq!(
            filled.config.unwrap().model.as_deref(),
            Some("claude-opus-5-5")
        );
    }
}
