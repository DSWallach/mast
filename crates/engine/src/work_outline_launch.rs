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
use crate::lead_role;
use crate::handoff_roles::{AutomaticHandoff, PendingHandoff, RoleObservations};
use crate::path_leases;
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
                role: None,
                base_revision_id: None,
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

/// Branch prefix of worker worktrees. Not `mast/`, so `is_owned_branch` is false.
const WORK_BRANCH_PREFIX: &str = "work/";

/// Rename a new worker worktree's branch `<slug>` to `work/<slug>`.
fn move_to_work_branch(worktree: &Path, slug: &str) -> Result<(), String> {
    let target = format!("{WORK_BRANCH_PREFIX}{slug}");
    match crate::chat_workspace_plan::current_branch(worktree) {
        Some(current) if current == target => Ok(()),
        Some(current) if current == slug => {
            let output = mast_harness::git_command()
                .args(["branch", "-m", slug, &target])
                .current_dir(worktree)
                .output()
                .map_err(|e| format!("failed to spawn git: {e}"))?;
            output.status.success().then_some(()).ok_or_else(|| {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            })
        }
        // Someone checked out another branch: leave it alone.
        _ => Ok(()),
    }
}

fn worker_branch_name(issue: Option<&str>, title: &str) -> String {
    let issue = issue.map(crate::chat_workspace_plan::kebab_case).filter(|s| !s.is_empty());
    let mut title = crate::chat_workspace_plan::kebab_case(title);
    if let Some(prefix) = issue.as_deref() {
        if title == prefix { title.clear(); }
        else if let Some(rest) = title.strip_prefix(&format!("{prefix}-")) { title = rest.to_owned(); }
    }
    title.truncate(title.len().min(48));
    let title = title.trim_end_matches('-');
    match (issue, title.is_empty()) {
        (Some(issue), false) => format!("{issue}-{title}"),
        (Some(issue), true) => issue,
        (None, false) => title.to_owned(),
        (None, true) => "worker".to_owned(),
    }
}

#[cfg(test)]
#[test]
fn worker_branch_names_include_the_issue_and_title() {
    assert_eq!(worker_branch_name(Some("ENG-123"), "ENG-123 · Fix login"), "eng-123-fix-login");
    assert_eq!(worker_branch_name(None, "Repair login"), "repair-login");
    assert_eq!(worker_branch_name(None, "!!!"), "worker");
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
    WorkersTransferred,
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

/// A plain worker launch (no role, not a hand-off) is the only kind a Lead's
/// Worker limits apply to.
fn hand_off_free(p: &CreateChatForWorkItemParams, hand_off: bool) -> bool {
    !hand_off && p.standing_role.is_none()
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
        automatic: bool,
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

/// A standing-role chat created provisionally: its marker, and whether it
/// keeps its parent link (a Lead does; its Manager is the parent).
struct ProvisionalRole {
    tag: String,
    keeps_parent: bool,
}

impl WorkItemLaunchService {
    pub(crate) async fn lock_launch(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gate.lock().await
    }
    #[cfg(debug_assertions)]
    pub(crate) async fn test_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock_launch().await
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
        self.launch_chat_for_work_item(p, None, None).await
    }

    /// The Lead tier's checks that need the whole project (Lead count, subtree
    /// lease, budget, parent), run on the device the Manager is on before a
    /// Lead launch is forwarded to the device that owns the item. The owner
    /// runs them again; this device sees every Lead the Manager has started.
    pub async fn precheck_lead(&self, p: &CreateChatForWorkItemParams) -> Result<(), WorkOutlineError> {
        if p.standing_role != Some(mast_proto::StandingRoleKind::Lead) {
            return Ok(());
        }
        let _gate = self.gate.lock().await;
        let chat_id = derive_id(&p.operation_id, roles::CHAT);
        let not_found = || error(WorkOutlineErrorCode::NotFound, "Work item not found");
        let space_id = self.workspace.work_item_space(&p.work_item_id).ok_or_else(not_found)?;
        let inputs = self.workspace.work_outline_inputs(&space_id);
        let snapshot = mast_doc::materialize_work_outline(&space_id, &inputs);
        let item = snapshot.item(&p.work_item_id).ok_or_else(not_found)?;
        self.admit_lead_tier(p, &chat_id, &snapshot, &inputs, item, true, false).map(|_| ())
    }

    /// Hold the launch gate: the one lock every launch and every Lead-tier
    /// count (Leads per project, Workers per Lead) is taken under.
    pub async fn lock_gate(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.gate.clone().lock_owned().await
    }

    /// Select and durably record a scheduled standing-chat target while the
    /// same gate used by role hand-offs is held. An already selected target is
    /// an operation input: a retry must not silently move it to a successor.
    pub(crate) async fn launch_scheduled_standing_locked<F>(
        &self,
        run: &mast_proto::ScheduledRun,
        wakes: &crate::orchestrator_wakes::OrchestratorWakes,
        persist_target: F,
    ) -> Result<bool, EngineError>
    where
        F: FnOnce(&str, Option<&str>, Option<&str>) -> Result<(), EngineError>,
    {
        let item = run.snapshot.work_item_id.as_deref()
            .ok_or_else(|| EngineError::Other("A standing chat requires a work item".into()))?;
        let role = mast_proto::StandingRoleKind::Assistant;
        let candidate = mast_proto::derive_id(&run.id, roles::CHAT);
        // A previous launch may have durably queued its first message and
        // crashed before activation. Finish that start before choosing a
        // target for a different operation.
        if self.workspace.standing_role_chat(role, item).is_none() {
            for provisional in self.workspace.provisional_role_chats(role, item) {
                let launched = provisional.standing_role_provisional_operation()
                    .map(|op| self.doc_host.has_launch_command(&provisional.id, &derive_id(op, roles::COMMAND)))
                    .transpose()?.unwrap_or(false);
                if launched {
                    self.activate_role_chat(role, item, &provisional.id)
                        .map_err(|e| EngineError::Other(e.message))?;
                    self.doc_host.kick_drains();
                    break;
                }
            }
        }
        let current = self.workspace.standing_role_chat(role, item);
        let mut generation = run.assistant_generation.clone();
        let mut rotation_source = run.rotation_source_chat_id.clone();
        let target = if let Some(target) = &run.chat_id {
            target.clone()
        } else if let Some(target) = current.clone() {
            let chat = self.workspace.chat(&target)?
                .ok_or_else(|| EngineError::Other("The standing assistant chat is missing".into()))?;
            let rotate = match run.snapshot.refresh {
                mast_proto::ScheduledTaskRefresh::Never => false,
                mast_proto::ScheduledTaskRefresh::Weekly =>
                    run.created_at.saturating_sub(chat.created_at.timestamp_millis()) >= 7 * 24 * 60 * 60 * 1000,
                mast_proto::ScheduledTaskRefresh::ContextHeavy => self.doc_host.open(&target).ok()
                    .and_then(|handle| handle.doc().context_usage())
                    .map(|usage| usage.within_limit(
                        self.workspace.chat_config(&target)
                            .and_then(|config| mast_proto::context_limit_tokens(&config.model_options)),
                    ))
                    .and_then(|usage| usage.fraction()).is_some_and(|fraction| fraction >= 0.80),
            };
            if rotate {
                rotation_source = Some(target);
                candidate.clone()
            } else {
                target
            }
        } else {
            let chats = self.workspace.read_chats()?;
            let designation = self.sessions.agent_tokens().personal_assistant().get();
            let existing = designation.as_ref().and_then(|d| d.current(&chats))
                .filter(|chat| !chat.archived && chat.device_id == self.workspace.device_id())
                .filter(|chat| chat.space_id.as_deref().is_some_and(|space|
                    self.workspace.chat_assigned_work_item(&chat.id, space).as_deref() == Some(item)));
            if let Some(chat) = existing {
                self.workspace.adopt_standing_role(role, item, &chat.id, &[])
                    .map_err(EngineError::Other)?;
                self.workspace.persist_registry_now()?;
                chat.id.clone()
            } else {
                generation = self.sessions.agent_tokens().personal_assistant()
                    .observe_empty_generation().map_err(|e| EngineError::Other(e.to_string()))?;
                candidate.clone()
            }
        };
        // Manager hand-off takes the drain lock before this gate. Never wait
        // for that lock while holding the gate: defer a busy rotation instead.
        let source_handle = rotation_source.as_deref().map(|source| self.doc_host.open(source)).transpose()?;
        let _source_queue = if let Some(handle) = &source_handle {
            let Ok(guard) = handle.drain_lock.try_lock() else { return Ok(false); };
            if current.as_deref() == rotation_source.as_deref()
                && (self.sessions.turn_in_flight(handle.chat_id())
                    || handle.doc().read_queue().ok().is_none_or(|queue| !queue.is_empty())
                    || handle.doc().read_commands().ok().is_none_or(|commands|
                        commands.iter().any(|command| command.status == mast_doc::SessionCommandStatus::Pending)))
            {
                return Ok(false);
            }
            Some(guard)
        } else { None };
        persist_target(&target, generation.as_deref(), rotation_source.as_deref())?;
        let prompt = WorkItemPrompt {
            text: crate::scheduled_tasks::results::prompt_with_contract(run),
            attachments: vec![],
        };
        if let Some(source) = rotation_source.as_deref() {
            let source_chat = self.workspace.chat(source)?
                .ok_or_else(|| EngineError::Other("The assistant hand-off source is missing".into()))?;
            let observed = crate::handoff_roles::RoleObservations::capture(
                &self.workspace, &self.sessions.agent_tokens(), &source_chat,
            );
            let params = CreateChatForWorkItemParams {
                work_item_id: item.into(),
                initial_prompt: WorkItemPrompt {
                    text: format!("Continue the personal assistant role. Read the preceding chat {source} for its conversation and decisions, then perform this scheduled task:\n\n{}", prompt.text),
                    attachments: vec![],
                },
                config: Some(run.snapshot.config.clone()),
                execution_override: None,
                worktree_choice: None,
                operation_id: run.id.clone(),
                parent_chat_id: None,
                target_device_id: None,
                account_id: None,
                standing_role: None,
            };
            self.create_orchestrator_successor_locked(&params, wakes, source,
                HandOffOrigin::User { archive: Some(true), observed_roles: &observed, automatic: false }, Some((&run.snapshot, generation.as_deref())), true).await
                .map_err(|e| EngineError::Other(e.message))?;
        } else if target == candidate && current.is_none() {
            let params = CreateChatForWorkItemParams {
                work_item_id: item.into(),
                initial_prompt: prompt,
                config: Some(run.snapshot.config.clone()),
                execution_override: Some(mast_proto::ExecutionOverride {
                    configuration: mast_proto::ExecutionConfiguration::implicit(run.snapshot.cwd.clone()),
                }),
                worktree_choice: None,
                operation_id: run.id.clone(),
                parent_chat_id: None,
                target_device_id: None,
                account_id: None,
                standing_role: Some(role),
            };
            self.launch_chat_for_work_item_locked(&params, None, None, Some((&run.snapshot, generation.as_deref()))).await
                .map_err(|e| EngineError::Other(e.message))?;
        } else {
            // The Manager hand-off path takes the source drain lock before the
            // launch gate. Never wait for it while holding that gate.
            let handle = self.doc_host.open(&target)?;
            let Ok(_target_guard) = handle.drain_lock.try_lock() else { return Ok(false); };
            let message_id = derive_id(&run.id, roles::MESSAGE);
            let (command, landed) = self.doc_host.message_command_state(&target, &message_id)?;
            if command.is_none() && !landed {
                let still_current = self.workspace.standing_role_chat(role, item).as_deref() == Some(target.as_str());
                let available = self.workspace.chat(&target)?.is_some_and(|chat| !chat.archived);
                if !still_current || !available {
                    return Err(EngineError::Other("The selected standing chat was replaced before this run was queued; its target was not changed".into()));
                }
            }
            let params = ContinueChatForWorkItemParams {
                chat_id: target.clone(),
                prompt,
                operation_id: run.id.clone(),
            };
            self.continue_chat_for_work_item_target(&params, Some(&run.snapshot)).await
                .map_err(|e| EngineError::Other(e.message))?;
        }
        if let Some(generation) = generation.as_deref() {
            let holder = self.workspace.standing_role_chat(role, item).unwrap_or(target);
            let chat = self.workspace.chat(&holder)?
                .ok_or_else(|| EngineError::Other("The standing assistant chat is missing".into()))?;
            self.sessions.agent_tokens().personal_assistant()
                .set_if_empty_generation(&chat, generation)
                .map_err(|e| EngineError::Other(e.to_string()))?;
        }
        Ok(true)
    }

    pub(crate) async fn create_orchestrator_successor(
        &self,
        p: &CreateChatForWorkItemParams,
        wakes: &crate::orchestrator_wakes::OrchestratorWakes,
        source: &str,
        origin: HandOffOrigin<'_>,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        let _gate = self.gate.lock().await;
        self.create_orchestrator_successor_locked(p, wakes, source, origin, None, true).await
    }

    async fn create_orchestrator_successor_locked(
        &self,
        p: &CreateChatForWorkItemParams,
        wakes: &crate::orchestrator_wakes::OrchestratorWakes,
        source: &str,
        origin: HandOffOrigin<'_>,
        scheduled: Option<(&mast_proto::ScheduledTaskDraft, Option<&str>)>,
        inherit_account: bool,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        let requested_successor = derive_id(&p.operation_id, roles::CHAT);
        let mut successor = self.handoff_successor_request(p, source)?;
        // A credit-limit successor never inherits the stalled chat's account pin.
        if !inherit_account {
            successor.account_id = None;
        }
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
        self.launch_chat_for_work_item_locked(&successor, Some((wakes, source, origin, &requested_successor)), None, scheduled)
            .await
    }

    /// Start a successor worker for `stalled` on `harness`, in the stalled
    /// chat's own execution root, under the same work item and parent. The
    /// launch is idempotent per `operation_id`, so a retry or a restart
    /// returns the chat the first call made.
    ///
    /// A successor on the stalled chat's own harness (`harness` equals its
    /// configured one) keeps its model, reasoning level and model options, the
    /// context window among them. On another harness those mean nothing, so the
    /// launch picks that harness's defaults.
    pub(crate) async fn create_credit_successor(
        &self, stalled: &str, operation_id: &str, harness: mast_proto::HarnessId, prompt: String,
        wakes: &crate::orchestrator_wakes::OrchestratorWakes,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        let work_item_id = self.workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, stalled)
            .filter(|row| !row.deleted)
            .and_then(|row| row.fields.get("workItemId").and_then(|v| v.as_str()).map(str::to_owned))
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "The stalled chat has no work item"))?;
        let mut config = self.workspace.chat_config(stalled)
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "The stalled chat has no configuration"))?;
        if config.harness != harness {
            config.harness = harness;
            config.model = None;
            config.reasoning = None;
            config.model_options.clear();
        }
        let request = CreateChatForWorkItemParams {
            work_item_id,
            initial_prompt: WorkItemPrompt { text: prompt, attachments: Vec::new() },
            config: Some(config),
            execution_override: None,
            worktree_choice: None,
            operation_id: operation_id.to_owned(),
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: None,
        };
        // The successor is a real hand-off of the stalled chat: lineage is
        // recorded, its own workers and pending wakes move to the successor,
        // and the stalled chat is kept (it may run again once its limit
        // resets). A chat that already handed off to another successor, or
        // that holds a standing role, is launched as a plain successor.
        let successor = derive_id(operation_id, roles::CHAT);
        let handed_off_elsewhere = self.workspace.handed_off_to(stalled).is_some_and(|to| to != successor);
        let source_chat = self.workspace.chat(stalled).map_err(engine_error)?;
        let real_hand_off = !handed_off_elsewhere
            && !self.workspace.chat_in_standing_role(stalled)
            && source_chat.as_ref().is_some_and(|chat| !chat.archived);
        let Some(source_chat) = source_chat.filter(|_| real_hand_off) else {
            let mut request = self.handoff_successor_request(&request, stalled)?;
            // The stalled chat's account pin is always dropped, on its own harness
            // too: a successor never inherits it.
            request.account_id = None;
            let reply = self.create_chat_for_work_item(&request).await?;
            self.mark_continued(&reply.chat_id, stalled);
            return Ok(reply);
        };
        let observed = crate::handoff_roles::RoleObservations::capture(
            &self.workspace, &self.sessions.agent_tokens(), &source_chat,
        );
        let _gate = self.gate.lock().await;
        let reply = self.create_orchestrator_successor_locked(
            &request, wakes, stalled,
            HandOffOrigin::User { archive: Some(false), observed_roles: &observed, automatic: false },
            None, false,
        ).await?;
        self.mark_continued(&reply.chat_id, stalled);
        Ok(reply)
    }

    /// Give an engine-made continuation (a credit-limit successor, a stopped
    /// worker continued by a launch) a title that tells it from its source:
    /// the source's title plus " (continued)". A successor the user already
    /// renamed, and a repeat call, keep what they have.
    pub(crate) fn mark_continued(&self, successor: &str, source: &str) {
        let title = |id: &str| self.workspace.chat(id).ok().flatten().and_then(|chat| chat.title)
            .filter(|title| !title.trim().is_empty());
        let (Some(source_title), Some(current)) = (title(source), title(successor)) else { return };
        if current != source_title {
            return;
        }
        let base = source_title.trim_end_matches(" (continued)");
        if let Err(error) = self.workspace.rename_chat(successor, &format!("{base} (continued)")) {
            tracing::warn!(chat = %successor, %error, "could not title the continuation");
        }
    }

    /// Whether the successor chat for `operation_id` has its first turn queued
    /// or landed (a retry whose inputs drifted can still accept it).
    pub(crate) fn credit_successor_turn_queued(&self, chat_id: &str, operation_id: &str) -> bool {
        self.doc_host.message_command_state(chat_id, &derive_id(operation_id, roles::MESSAGE))
            .map(|(command, landed)| command.is_some() || landed)
            .unwrap_or(false)
    }

    fn handoff_successor_request(
        &self, p: &CreateChatForWorkItemParams, source: &str,
    ) -> Result<CreateChatForWorkItemParams, WorkOutlineError> {
        let source_chat = self
            .workspace
            .chat(source)
            .map_err(engine_error)?
            .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Source chat not found"))?;
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
        // A source whose parent was handed off since keeps pointing at the
        // archived predecessor; the successor reports to the live chat.
        successor.parent_chat_id = self.workspace.live_parent(source).or(source_chat.parent_chat_id);
        let harness = successor
            .config
            .as_ref()
            .map(|config| config.harness)
            .unwrap_or_else(|| self.doc_host.harness_for(source));
        successor.account_id = self.sessions.chat_account_pin(source, harness);
        Ok(successor)
    }

    fn prepare_launch_model(
        &self,
        p: &CreateChatForWorkItemParams,
        chat_id: &str,
        settings: Option<&mast_proto::work_outline::WorkOutlineSpaceSettings>,
    ) -> CreateChatForWorkItemParams {
        let mut filled = with_default_model(p, self.doc_host.harness_for(chat_id), chat_id, settings);
        // A retry of a partly finished launch keeps the model its first attempt
        // stored on the chat, so the chat row and the first turn never differ
        // after a Space default changes in between. Only a model stored for the
        // same harness is reused: a model id means nothing on another one.
        let requested = p.config.as_ref().and_then(|c| c.model.as_deref()).is_some_and(|m| !m.trim().is_empty());
        if !requested
            && let Some(stored) = self.workspace.chat(chat_id).ok().flatten().and_then(|chat| chat.config)
            && let Some(config) = filled.config.as_mut()
        {
            config.model = reused_stored_model(config.harness, &stored).or(config.model.take());
        }
        let parent_rules = p.parent_chat_id.as_deref()
            .and_then(|id| self.workspace.chat_config(id))
            .is_some_and(|config| config.approval_rules && !config.auto_approve);
        if let Some(config) = filled.config.as_mut().filter(|_| parent_rules) {
            if !config.auto_approve {
                config.approval_rules = true;
            }
        }
        filled
    }

    pub(crate) async fn prepare_automatic_handoff(
        &self, request: &CreateChatForWorkItemParams, source: &str, space: &str,
        observations: RoleObservations,
    ) -> Result<AutomaticHandoff, WorkOutlineError> {
        let request = self.handoff_successor_request(request, source)?;
        let successor = derive_id(&request.operation_id, roles::CHAT);
        let request = self.prepare_launch_model(&request, &successor, None);
        let execution = self.effective_configuration(&request, space).await?;
        let snapshot = mast_doc::materialize_work_outline(space, &self.workspace.work_outline_inputs(space));
        let instructions = snapshot.effective_instructions(&request.work_item_id);
        let workers = self.workspace.read_chats().map_err(engine_error)?.into_iter()
            .filter(|chat| !chat.archived && chat.parent_chat_id.as_deref() == Some(source))
            .map(|chat| chat.id).collect::<Vec<_>>();
        let worker_appendix = self.handoff_worker_appendix(space, workers)?;
        Ok(AutomaticHandoff { source: source.into(), request, execution, instructions,
            worker_appendix, observations, parked: false, credit_entry: None, carried: Vec::new() })
    }

    /// Write one system notice into `chat_id`; a repeat of `message_id` is a no-op.
    pub(crate) fn notify_chat(&self, chat_id: &str, message_id: &str, text: &str) -> Result<(), WorkOutlineError> {
        let handle = self.doc_host.open(chat_id).map_err(engine_error)?;
        handle
            .write_system_notice(message_id, text, chrono::Utc::now().timestamp_millis())
            .map_err(|e| error(WorkOutlineErrorCode::Invalid, e.to_string()))
    }

    /// Raise one Required Action on `work_item_id`, once per `dedupe_key`.
    pub(crate) fn raise_request(
        &self, work_item_id: &str, text: &str, info: &str, dedupe_key: &str,
    ) -> Result<(), WorkOutlineError> {
        use mast_proto::work_outline_user_actions::{
            CreateUserActionRequestParams, UserActionKind, UserActionOriginKind, UserActionRequestOrigin,
        };
        let mutation = WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
            options: Vec::new(),
            links: Vec::new(),
            facts: Vec::new(),
            kind: Some(UserActionKind::Ask),
            priority: None,
            work_item_id: work_item_id.to_owned(),
            origin: UserActionRequestOrigin {
                kind: UserActionOriginKind::System,
                chat_id: String::new(),
                message_id: None,
                tool_call_id: None,
            },
            text: text.to_owned(),
            info: Some(info.to_owned()),
            dedupe_key: Some(dedupe_key.to_owned()),
            operation_id: derive_id(dedupe_key, "credit-fallback-request"),
        });
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::System,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        match self.workspace.work_outline_mutate(&mutation, &actor, &[]) {
            Ok(_) => Ok(()),
            // Already raised, here or on another device, with other details.
            Err(e) if e.code == WorkOutlineErrorCode::OperationConflict => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn automatic_handoff_committed(&self, intent: &AutomaticHandoff) -> bool {
        let successor = derive_id(&intent.request.operation_id, roles::CHAT);
        self.workspace.completed_user_hand_off_to(
            &intent.source, &successor, self.workspace.device_id(), &intent.execution.space_id, Some(true),
        ).as_deref() == Some(successor.as_str())
    }

    pub(crate) async fn resume_automatic_handoff(
        &self, intent: &AutomaticHandoff, wakes: &crate::orchestrator_wakes::OrchestratorWakes,
    ) -> Result<WorkItemLaunchState, WorkOutlineError> {
        let operation = &intent.request.operation_id;
        let successor = derive_id(operation, roles::CHAT);
        wakes.hold_automatic_manager_handoff(&successor, operation);
        if self.automatic_handoff_committed(intent) {
            let source = self.workspace.chat(&intent.source).map_err(engine_error)?
                .ok_or_else(|| conflict("The original chat no longer exists"))?;
            let pending = PendingHandoff::new(&source, &successor, Some(true))
                .ok_or_else(|| conflict("The original chat no longer belongs to a Space"))?;
            let tokens = self.sessions.agent_tokens();
            self.workspace.persist_registry_now().map_err(engine_error)?;
            tokens.pending_handoffs.finish(&self.workspace, &tokens, &pending, &successor)
                .map_err(engine_error)?;
            // An Assistant successor's held first command starts only once the
            // transfer is finished and its drain is woken.
            self.doc_host.kick_drains();
            wakes.finish_automatic_manager_handoff(&successor, operation);
            wakes.evaluate(&successor, false).await;
            return Ok(WorkItemLaunchState::Started);
        }
        let reply = self.launch_chat_for_work_item(
            &intent.request,
            Some((wakes, &intent.source, HandOffOrigin::User {
                archive: Some(true), observed_roles: &intent.observations, automatic: true,
            }, &successor)),
            Some(intent),
        ).await?;
        if reply.state == WorkItemLaunchState::Started {
            wakes.finish_automatic_manager_handoff(&successor, operation);
            wakes.evaluate(&successor, false).await;
        }
        Ok(reply.state)
    }

    pub(crate) fn automatic_handoff_still_owned(&self, intent: &AutomaticHandoff) -> bool {
        let successor = derive_id(&intent.request.operation_id, roles::CHAT);
        self.check_automatic_handoff(intent, &successor).is_ok()
    }

    fn check_automatic_handoff(&self, intent: &AutomaticHandoff, successor: &str) -> Result<(), WorkOutlineError> {
        let p = &intent.request;
        let held = self.workspace.standing_role_of_chat(&intent.source)
            .filter(|role| role.scope_item_id == p.work_item_id)
            .ok_or_else(|| conflict("The standing role changed during automatic handoff"))?;
        let kind = held.role;
        let role = self.workspace.standing_role(kind, &p.work_item_id)
            .filter(|role| role.current_chat_id.as_deref() == Some(intent.source.as_str()))
            .ok_or_else(|| conflict("The standing role changed during automatic handoff"))?;
        let reservation = self.workspace.pending_standing_hand_off(kind, &p.work_item_id);
        let own = reservation.as_ref().is_some_and(|reservation| {
            reservation.from_chat_id == intent.source && reservation.to_chat_id == successor
                && reservation.operation_id == p.operation_id
        });
        if (reservation.is_some() && !own)
            || (role.state != mast_proto::StandingRoleState::Idle && !own)
        {
            return Err(conflict("Another handoff owns this standing role"));
        }
        let source = self.workspace.chat(&intent.source).map_err(engine_error)?
            .ok_or_else(|| conflict("The original chat no longer exists"))?;
        if source.archived || source.device_id != self.workspace.device_id()
            || source.space_id.as_deref() != Some(intent.execution.space_id.as_str())
            || self.workspace.work_item_space(&p.work_item_id).as_deref() != source.space_id.as_deref()
        {
            return Err(conflict("The original chat no longer belongs to this item and device"));
        }
        Ok(())
    }

    fn handoff_worker_appendix(&self, space: &str, workers: Vec<String>) -> Result<String, WorkOutlineError> {
        let sessions = self.workspace.read_sessions().map_err(engine_error)?;
        let current = mast_doc::materialize_work_outline(space, &self.workspace.work_outline_inputs(space));
        let mut appendix = String::from("

[Mast: reparented workers and their open Required Actions]");
        if workers.is_empty() {
            appendix.push_str("
No active workers.");
        }
        for worker in workers {
            let chat = self.workspace.chat(&worker).map_err(engine_error)?;
            let title = chat.as_ref().and_then(|chat| chat.title.as_deref()).unwrap_or("Untitled");
            let status = sessions.iter().find(|session| session.chat_id == worker)
                .map(|session| format!("{:?}", session.status)).unwrap_or_else(|| "Unknown".into());
            appendix.push_str(&format!("
- {worker} | {title} | {status}"));
            for action in current.user_action_requests.iter().filter(|action| {
                action.chat_id == worker && action.is_unresolved()
            }) {
                appendix.push_str(&format!("
  - Required Action {}: {}", action.id, action.text));
            }
        }
        Ok(appendix)
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
        automatic: Option<&AutomaticHandoff>,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        let _gate = self.gate.lock().await;
        self.launch_chat_for_work_item_locked(p, hand_off, automatic, None).await
    }

    async fn launch_chat_for_work_item_locked(
        &self,
        p: &CreateChatForWorkItemParams,
        hand_off: Option<(&crate::orchestrator_wakes::OrchestratorWakes, &str, HandOffOrigin<'_>, &str)>,
        automatic: Option<&AutomaticHandoff>,
        scheduled: Option<(&mast_proto::ScheduledTaskDraft, Option<&str>)>,
    ) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
        if crate::work_outline::writes_disabled() {
            return Err(error(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            ));
        }
        check_operation_id(&p.operation_id)?;
        check_prompt(&p.initial_prompt)?;
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
        // CLI default (a retry reuses the model its first attempt stored).
        if let Some(intent) = automatic {
            self.check_automatic_handoff(intent, &chat_id)?;
        }

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
        let outline_inputs = self.workspace.work_outline_inputs(&space_id);
        let snapshot = mast_doc::materialize_work_outline(&space_id, &outline_inputs);
        let item = snapshot.item(&p.work_item_id).ok_or_else(not_found)?;
        if item.archived_at.is_some() {
            return Err(error(
                WorkOutlineErrorCode::Archived,
                "The work item is archived",
            ));
        }
        let lead_launch = p.standing_role == Some(mast_proto::StandingRoleKind::Lead)
            || hand_off.is_some_and(|(_, source, _, _)| self.chat_holds_lead(source));
        // An automatic successor keeps its stored config, except a Lead's with
        // none: it is filled so the read-only sandbox below has a config to bind.
        let filled = (automatic.is_none() || (lead_launch && p.config.is_none()))
            .then(|| self.prepare_launch_model(p, &chat_id, Some(&snapshot.space_settings)));
        let p = filled.as_ref().unwrap_or(p);
        // A worker started without a window gets the Space's (Codex only).
        let windowed;
        let p = match with_worker_context_window(
            p,
            &snapshot.space_settings,
            hand_off.is_some(),
        ) {
            Some(changed) => {
                windowed = changed;
                &windowed
            }
            None => p,
        };
        // Lead tier: admission (parent, depth, limits, budget, lease) and the
        // read-only sandbox. A hand-off successor of a Lead only keeps the
        // sandbox: it takes the role over, so it adds no Lead.
        let under_lead = self.admit_lead_tier(p, &chat_id, &snapshot, &outline_inputs, item, lead_launch, hand_off.is_some())?;
        let read_only_lead;
        let p = if lead_launch && p.config.as_ref().is_some_and(|c| c.sandbox != SandboxLevel::ReadOnly) {
            let mut changed = p.clone();
            if let Some(config) = changed.config.as_mut() {
                config.sandbox = SandboxLevel::ReadOnly;
            }
            read_only_lead = changed;
            &read_only_lead
        } else {
            p
        };
        // A role launch is the role's first start: only a project can have a
        // Manager, and a start that finds the Manager already started sends
        // nothing. Each operation creates its own hidden provisional chat; the
        // chat becomes the Manager only after its first message is durably
        // queued. The gate held above serializes launches, so at most one
        // provisional chat of a role has a queued message, and every start
        // finishes that one first.
        if let Some(kind) = p.standing_role {
            if kind == mast_proto::StandingRoleKind::Manager && !item.is_project {
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
        // A Manager (new, or the successor of one) always delivers: its
        // built-in role instructions are part of every revision it gets.
        // The new chat counts too: once a retried hand-off's role has moved
        // to it, the source no longer holds the role, and the decision must
        // not change under the operation's revision.
        let holds_manager = |chat: &str| {
            self.workspace
                .standing_role_of_chat(chat)
                .is_some_and(|role| role.role == mast_proto::StandingRoleKind::Manager)
        };
        let existing_revision = self
            .workspace
            .instruction_revision(&derive_id(op, roles::REVISION));
        let is_manager = match &existing_revision {
            // A revision this operation already wrote fixes the decision,
            // however far the role has moved since.
            Some(revision) => revision
                .body
                .entries
                .first()
                .is_some_and(|entry| entry.instruction_id == mast_proto::MANAGER_ROLE_INSTRUCTION_ID),
            None => {
                p.standing_role == Some(mast_proto::StandingRoleKind::Manager)
                    || holds_manager(&chat_id)
                    || hand_off.is_some_and(|(_, source, _, _)| holds_manager(source))
            }
        };
        let manager_role = is_manager.then_some(mast_proto::StandingRoleKind::Manager);
        let effective = snapshot.effective_instructions(&p.work_item_id);
        if let Some(intent) = automatic
            && effective != intent.instructions
            && !intent.instructions.entries.is_empty()
            && self.workspace.instruction_revision(&derive_id(op, roles::REVISION)).is_none()
        {
            return Err(conflict("Work item instructions changed before the automatic launch captured its revision"));
        }
        let no_effective = manager_role.is_none()
            && automatic.map_or(effective.entries.is_empty(), |intent| intent.instructions.entries.is_empty());
        let deliver_instructions = self.launch_delivers_instructions(&chat_id, &message_id, op, no_effective)?;
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
        // A Manager always runs read-only; refuse a harness that cannot
        // enforce that before the chat exists.
        let manager_read_only = lead_launch
            || (manager_role.is_some()
                && mast_proto::StandingRole::manager_runs_read_only(
                    self.workspace
                        .standing_role(mast_proto::StandingRoleKind::Manager, &p.work_item_id)
                        .as_ref(),
                ));
        if manager_read_only && !mast_harness::capabilities(harness).read_only {
            return Err(error(
                WorkOutlineErrorCode::Invalid,
                format!("{harness:?} has no read-only mode, which a Manager or Lead needs; use Claude Code or Codex"),
            ));
        }
        // A Manager's stacked envelope (role plus the project's instructions)
        // must fit the revision budget and the harness before anything is
        // written or any worker moves.
        // A retry keeps the immutable revision its operation already wrote,
        // so project instructions edited since cannot block recovery after
        // workers moved.
        if manager_role.is_some() && existing_revision.is_none() {
            let role = self
                .workspace
                .standing_role(mast_proto::StandingRoleKind::Manager, &p.work_item_id);
            let entries = mast_proto::stack_manager_role_entry(
                snapshot
                    .effective_instructions(&p.work_item_id)
                    .revision_entries(),
                role.as_ref(),
            );
            let body = mast_proto::build_revision_body(entries)?;
            let envelope = mast_proto::InstructionEnvelope::from_revision("preflight", &body)
                .map_err(|err| error(WorkOutlineErrorCode::Invalid, err.to_string()))?;
            let support = self.sessions.instruction_support(harness).map_err(|err| {
                error(
                    WorkOutlineErrorCode::Capability,
                    format!("The harness cannot be used on this device: {err}"),
                )
            })?;
            crate::instruction_delivery::check_support(harness, &support, &envelope)
                .map_err(|message| error(WorkOutlineErrorCode::Capability, message))?;
        }

        self.claim_inputs(op, &inputs_hash)?;
        if let Some((
            _,
            source,
            HandOffOrigin::User {
                archive,
                observed_roles,
                ..
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
                .prepare(&intent, observed_roles, &self.workspace)
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
                if automatic.is_some_and(|intent| intent.execution != binding.resolved) {
                    return Err(conflict("The automatic successor's execution binding changed"));
                }
                binding.resolved.clone()
            }
            None => match automatic {
                Some(intent) => intent.execution.clone(),
                None => {
                let task_specs = path_leases::task_specs(&outline_inputs);
                let spec = task_specs.get(&item.id);
                let worker_launch = hand_off.is_none() && p.parent_chat_id.is_some()
                    && path_leases::provisions_worktree(
                        p.config.as_ref().map_or(SandboxLevel::WorkspaceWrite, |c| c.sandbox),
                        p.standing_role,
                        spec,
                    );
                if worker_launch {
                    self.take_path_lease(&snapshot, &item.id, spec, &task_specs, under_lead.as_deref())?;
                }
                let mut resolved = self.effective_configuration(p, &space_id).await?;
                let has_item_setup = matches!(&resolved.source_scope, mast_proto::work_outline_execution::ExecutionSourceScope::WorkItem { .. });
                let has_worktree = resolved.configuration.bindings.iter()
                    .any(|binding| binding.worktree_id.is_some() || binding.worktree_path.is_some());
                if worker_launch && snapshot.space_settings.worker_worktrees
                    && p.execution_override.is_none() && p.worktree_choice.is_none()
                    && !has_item_setup && !has_worktree && resolved.invalid_reason.is_none()
                {
                    let issue = snapshot.linear_issue_reference(&item.id).map(|r| r.external_id.as_str());
                    let name = worker_branch_name(issue, &item.title);
                    self.apply_worker_worktree(&mut resolved, &name, op).await?;
                    let pinned = self.workspace.configure_worker_execution_if_absent(&mast_proto::ConfigureExecutionParams {
                        scope: ExecutionScope::WorkItem { work_item_id: item.id.clone() },
                        configuration: Some(resolved.configuration.clone()),
                        operation_id: derive_id(op, "worker-execution"),
                    }, &WorkOutlineActor { kind: WorkOutlineActorKind::System,
                        device_id: self.workspace.device_id().to_owned(), chat_id: None, message_id: None })?;
                    if !pinned {
                        return Err(conflict("The item's execution setup changed while creating the worker worktree"));
                    }
                    resolved.source_scope = mast_proto::work_outline_execution::ExecutionSourceScope::WorkItem {
                        work_item_id: item.id.clone(),
                    };
                }
                resolved
                }
            },
        };
        let validated = self
            .execution_scope
            .validate_resolved(&resolved, ExecutionHostCapabilities::default())
            .await
            .map_err(|errors| validation_error(&errors))?;
        let mut request = self.launch_request(p, &validated);
        if let Some((task, _)) = scheduled {
            request.scheduled_task = Some(task.name.clone());
            request.scheduled_notion_writes = task.write_to_notion;
        }
        self.checkpoint(LaunchStep::Validated)?;
        if let Some(intent) = automatic {
            self.check_automatic_handoff(intent, &chat_id)?;
        }

        // ── 3. Chat ──
        self.ensure_chat(
            &chat_id,
            &space_id,
            &validated.execution_root,
            p.config.clone(),
            p.parent_chat_id.as_deref(),
            p.standing_role.map(|kind| ProvisionalRole {
                tag: mast_proto::standing_role_provisional_marker(kind, &p.work_item_id, op),
                // Only a Lead has a parent (its Manager); other roles start unparented.
                keeps_parent: kind == mast_proto::StandingRoleKind::Lead,
            }),
        )?;
        // A Work Outline launch has no Run worktree directive, so stamp the
        // branch needed for opted-in PR tracking here. This launch-time lookup
        // leaves an existing value alone.
        if p.parent_chat_id.is_some() && (snapshot.space_settings.merged_pr_completion.is_some()
            || (snapshot.space_settings.worker_worktrees && resolved.configuration.bindings.iter().any(|binding| binding.worktree_path.is_some())))
            && self.workspace.chat(&chat_id).map_err(engine_error)?.is_some_and(|chat| chat.branch.is_none())
        {
            let root = std::path::PathBuf::from(&validated.execution_root);
            if let Some(branch) = tokio::task::spawn_blocking(move || crate::chat_workspace_plan::current_branch(&root))
                .await.map_err(|e| engine_error(EngineError::Other(e.to_string())))? {
                self.workspace.set_chat_branch_if_absent(&chat_id, &branch).map_err(engine_error)?;
            }
        }
        // A hand-off successor continues the source's conversation, so it keeps
        // the source's title (user-set or automatic) rather than being retitled.
        // An existing successor title wins, so a retry after a rename is stable.
        // This runs before worker naming so a renamed worker source is kept too.
        if let Some((_, source, _, _)) = hand_off
            && let Some(title) = self
                .workspace
                .chat(source)
                .map_err(engine_error)?
                .and_then(|chat| chat.title)
                .filter(|title| !title.trim().is_empty())
            && self
                .workspace
                .chat(&chat_id)
                .map_err(engine_error)?
                .is_some_and(|chat| chat.title.as_deref().is_none_or(|t| t.trim().is_empty()))
        {
            self.workspace
                .rename_chat(&chat_id, &title)
                .map_err(engine_error)?;
        }
        // Name the chat before its first turn can schedule automatic titling.
        // Preserve an existing title on retries or after a user rename.
        if p.parent_chat_id.is_some()
            && self
            .workspace
            .chat(&chat_id)
            .map_err(engine_error)?
            .is_some_and(|chat| {
                chat.title
                    .as_deref()
                    .is_none_or(|title| title.trim().is_empty())
            })
        {
            let title = match snapshot.linear_issue_reference(&item.id) {
                Some(reference) => format!("{} {}", reference.external_id, item.title),
                None if item.title.trim().eq_ignore_ascii_case("planning") => {
                    let context = item.parent_work_item_id.as_deref()
                        .and_then(|id| snapshot.item(id))
                        .map(|parent| parent.title.as_str())
                        .filter(|title| !title.trim().is_empty() && !title.trim().eq_ignore_ascii_case("planning"))
                        .unwrap_or_else(|| space.display_name());
                    format!("{context} {}", item.title)
                }
                None => item.title.clone(),
            };
            self.workspace
                .rename_chat(&chat_id, &title)
                .map_err(engine_error)?;
        }
        // The account chip's pick binds before the first run resolves one.
        if let (Some(account_id), Some(config)) = (p.account_id.as_deref(), p.config.as_ref()) {
            self.sessions
                .pin_chat_account(&chat_id, config.harness, account_id)
                .map_err(engine_error)?;
        }
        self.checkpoint(LaunchStep::ChatCreated)?;

        // ── 4. assignment ──
        self.ensure_assignment(&chat_id, &p.work_item_id, op)?;
        // Starting work on a pending item marks it active; completed and
        // archived items are never touched. Read live so a retry after the
        // item moved on does not revert it.
        let still_pending = mast_doc::materialize_work_outline(
            &space_id,
            &self.workspace.work_outline_inputs(&space_id),
        )
        .item(&p.work_item_id)
        .is_some_and(|item| {
            item.archived_at.is_none() && item.status == mast_proto::WorkItemStatus::Pending
        });
        if still_pending {
            self.mutate(WorkOutlineMutation::UpdateWorkItem(mast_proto::UpdateWorkItemParams {
                work_item_id: p.work_item_id.clone(),
                changes: mast_proto::WorkItemChanges {
                    status: Some(mast_proto::WorkItemStatus::Active),
                    ..Default::default()
                },
                operation_id: derive_id(op, "activate-item"),
            }))?;
        }
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
            if let Some(intent) = automatic {
                request.prompt.push_str(&intent.worker_appendix);
            } else {
                let workers = wakes.hand_off_orchestrator_without_delivery(source, &chat_id, source).await
                    .map_err(|detail| error(WorkOutlineErrorCode::Invalid, detail))?;
                request.prompt.push_str(&self.handoff_worker_appendix(&space_id, workers)?);
            }
        }

        // ── 6. revision + launch application (only with instructions) ──
        let (revision_id, application_id, payload) = if deliver_instructions {
            let revision_id = self
                .mutate(WorkOutlineMutation::CreateInstructionRevision(
                    CreateInstructionRevisionParams {
                        work_item_id: p.work_item_id.clone(),
                        operation_id: op.to_owned(),
                        role: manager_role,
                        base_revision_id: None,
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
        if let Some(intent) = automatic {
            self.check_automatic_handoff(intent, &chat_id)?;
            if let Some((wakes, source, _, _)) = hand_off {
                wakes.hand_off_orchestrator_without_delivery(source, &chat_id, source).await
                    .map_err(|detail| error(WorkOutlineErrorCode::Invalid, detail))?;
            }
            self.workspace.persist_registry_now().map_err(engine_error)?;
            self.checkpoint(LaunchStep::WorkersTransferred)?;
        }
        self.checkpoint(LaunchStep::RegistryPersisted)?;
        if p.standing_role == Some(mast_proto::StandingRoleKind::Assistant)
            && let Some((_, Some(generation))) = scheduled
        {
            let chat = self.workspace.chat(&chat_id).map_err(engine_error)?
                .ok_or_else(|| error(WorkOutlineErrorCode::NotFound, "Assistant chat not found"))?;
            self.sessions.agent_tokens().personal_assistant()
                .set_if_empty_generation(&chat, generation).map_err(|e| engine_error(e.into()))?;
        }
        if let Some(intent) = automatic {
            self.check_automatic_handoff(intent, &chat_id)?;
        }
        self.ensure_turn_queued(&chat_id, &command_id, &message_id, payload)?;
        self.checkpoint(LaunchStep::TurnQueued)?;

        // ── 8. standing role ──
        // Last: the Chat stays hidden until its first message is queued AND
        // saved, then one registry batch makes it the Manager. Idempotent on
        // a retry (an adopted chat is returned as is).
        if let Some(kind) = p.standing_role {
            self.activate_role_chat(kind, &p.work_item_id, &chat_id)?;
            if kind == mast_proto::StandingRoleKind::Assistant {
                self.doc_host.kick_drains();
            }
        }
        self.checkpoint(LaunchStep::RoleAdopted)?;

        let (mut state, mut detail) = self.turn_state(&chat_id, &message_id)?;
        // An Assistant successor's first turn waits for the device-local
        // designation transfer, which only the hand-off completion starts, so
        // its hand-off cannot wait for the turn to start.
        let source_is_assistant = hand_off.is_some_and(|(_, source, _, _)| {
            self.workspace.standing_role_of_chat(source)
                .is_some_and(|role| role.role == mast_proto::StandingRoleKind::Assistant)
        });
        if automatic.is_some() && !source_is_assistant && state == WorkItemLaunchState::Queued {
            // During the normal fast path, keep the source send gate until
            // rotation commits. A stalled or rejected successor is retried
            // from its durable intent without blocking the source indefinitely.
            let settle = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let result = self.turn_state(&chat_id, &message_id)?;
                    if result.0 != WorkItemLaunchState::Queued {
                        return Ok::<_, WorkOutlineError>(result);
                    }
                }
            };
            if let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(1), settle).await {
                (state, detail) = result?;
            }
        }
        if let Some((wakes, source, origin, requested)) = hand_off
            && (automatic.is_none() || source_is_assistant || state == WorkItemLaunchState::Started)
        {
            if state != WorkItemLaunchState::Rejected {
                if let Some(intent) = automatic {
                    self.check_automatic_handoff(intent, &chat_id)?;
                }
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
            if automatic.is_some() && source_is_assistant {
                self.doc_host.kick_drains();
            }
            if automatic.is_none() {
                wakes.finish_orchestrator_hand_off(&chat_id);
                wakes.evaluate(&chat_id, false).await;
                if self.workspace.standing_role_of_chat(&chat_id)
                    .is_some_and(|role| role.role == mast_proto::StandingRoleKind::Assistant)
                {
                    self.doc_host.kick_drains();
                }
            }
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
        let HandOffOrigin::User { archive, automatic, .. } = origin else {
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
            .complete_user_chat_hand_off(source, successor, requested, operation_id, archive, automatic)
            .map_err(invalid)?;
        self.workspace
            .persist_registry_now()
            .map_err(engine_error)?;
        tokens
            .pending_handoffs
            .finish(&self.workspace, &tokens, &intent, successor)
            .map_err(engine_error)?;
        // Anything that reached the source after its last queue check (a
        // manual hand-off takes no lock on it) now belongs to the successor.
        self.doc_host.forward_handed_off_queue_soon(source);
        Ok(())
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
        self.apply_worktree_choice_internal(resolved, choice, operation_id, false).await
    }

    async fn apply_worktree_choice_internal(
        &self,
        resolved: &mut ResolvedExecutionConfiguration,
        choice: &WorktreeChoice,
        operation_id: &str,
        exact_slug: bool,
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
                    .create_worktree_for_operation(&binding, name, operation_id, exact_slug)
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

    async fn apply_worker_worktree(
        &self,
        resolved: &mut ResolvedExecutionConfiguration,
        name: &str,
        operation_id: &str,
    ) -> Result<(), WorkOutlineError> {
        // Select a free name, or the checkout already claimed by this launch.
        let repo = resolved.configuration.bindings.iter().find(|b| b.primary)
            .or_else(|| (resolved.configuration.bindings.len() == 1).then(|| &resolved.configuration.bindings[0]))
            .map(|binding| binding.repo_path.clone())
            .unwrap_or_else(|| resolved.configuration.execution_root.clone());
        let mut suffix = 1_u32;
        let selected = loop {
            let candidate = if suffix == 1 { name.to_owned() } else { format!("{name}-{suffix}") };
            let repo_path = std::path::PathBuf::from(&repo);
            let candidate_owned = candidate.clone();
            let (path, occupied) = tokio::task::spawn_blocking(move || {
                let (path, _) = crate::chat_workspace_plan::chat_worktree_target_for_slug(&repo_path, &candidate_owned)?;
                let occupied = path.exists()
                    || crate::chat_workspace_plan::local_branch_exists(&repo_path, &candidate_owned)
                    || crate::chat_workspace_plan::local_branch_exists(&repo_path, &format!("{WORK_BRANCH_PREFIX}{candidate_owned}"));
                Ok::<_, EngineError>((path, occupied))
            }).await.map_err(|e| engine_error(EngineError::Other(e.to_string())))?.map_err(engine_error)?;
            if !occupied || self.workspace.ledger_holds(&ledger_worktree_key(operation_id, &path)) {
                break candidate;
            }
            if suffix >= 100 {
                return Err(error(WorkOutlineErrorCode::Invalid, "No available worktree name after 100 attempts"));
            }
            suffix = suffix.checked_add(1).ok_or_else(|| error(WorkOutlineErrorCode::Invalid, "No available worktree name"))?;
        };
        self.apply_worktree_choice_internal(resolved, &WorktreeChoice::CreateNew { name: selected.clone() }, operation_id, true).await?;
        // The checkout's branch is `work/<slug>`: never a `mast/*` branch, so
        // Mast neither renames it from the chat title nor deletes it with the
        // worktree. Repeating this after a crash is a no-op.
        let worktree = std::path::PathBuf::from(&resolved.configuration.execution_root);
        tokio::task::spawn_blocking(move || move_to_work_branch(&worktree, &selected))
            .await
            .map_err(|e| engine_error(EngineError::Other(e.to_string())))?
            .map_err(|err| error(WorkOutlineErrorCode::Invalid, format!("Could not name the worker branch: {err}")))
    }

    fn chat_holds_lead(&self, chat: &str) -> bool {
        self.workspace
            .standing_role_of_chat(chat)
            .is_some_and(|role| role.role == mast_proto::StandingRoleKind::Lead)
    }

    /// The Lead tier's launch rules (see `lead_role`): a new Lead needs the
    /// project's Manager as parent, a free slot among the project's 4, a
    /// context packet under 8k tokens, a budget carved from the project's and a
    /// subtree lease no other Lead holds. A Worker under a Lead (or deeper) is
    /// refused at depth 3 or past the Lead's 6 Workers, and its `ownedPaths`
    /// and budget must fit the Lead's. A launch with no Lead above it passes.
    #[allow(clippy::too_many_arguments)]
    fn admit_lead_tier(
        &self,
        p: &CreateChatForWorkItemParams,
        chat_id: &str,
        snapshot: &mast_proto::WorkOutlineSnapshot,
        inputs: &mast_doc::WorkOutlineInputs,
        item: &mast_proto::WorkItemView,
        lead_launch: bool,
        hand_off: bool,
    ) -> Result<Option<String>, WorkOutlineError> {
        let refuse = |refusal: lead_role::LeadRefusal| conflict(refusal.to_string());
        let node = |id: &str| lead_role::chat_node(&self.workspace, id);
        let specs = path_leases::task_specs(inputs);
        let project = snapshot.project_root(&item.id).map(|root| root.id.clone());
        let chats = self.workspace.read_chats().unwrap_or_default();
        if lead_launch {
            lead_role::check_context_size(&p.initial_prompt.text).map_err(refuse)?;
            if hand_off || self.workspace.standing_role_chat(mast_proto::StandingRoleKind::Lead, &item.id).as_deref() == Some(chat_id) {
                return Ok(None);
            }
            if item.is_project {
                return Err(error(WorkOutlineErrorCode::Invalid, "A Lead takes a work item inside a project, not the project"));
            }
            lead_role::check_lead_parent(p.parent_chat_id.as_deref(), &node).map_err(refuse)?;
            let other_leads: Vec<String> = self
                .workspace
                .standing_roles()
                .into_iter()
                .filter(|role| role.role == mast_proto::StandingRoleKind::Lead && role.scope_item_id != item.id)
                .filter(|role| snapshot.project_root(&role.scope_item_id).map(|root| &root.id) == project.as_ref())
                // A Lead whose item is Completed has handed its subtree back:
                // it frees its slot and its lease even if its chat is not archived.
                .filter(|role| {
                    snapshot.item(&role.scope_item_id).is_none_or(|lead| lead.status != mast_proto::WorkItemStatus::Completed)
                })
                .filter(|role| {
                    role.current_chat_id
                        .as_deref()
                        .is_some_and(|id| chats.iter().any(|chat| chat.id == id && !chat.archived))
                })
                .map(|role| role.scope_item_id)
                .collect();
            lead_role::check_lead_count(other_leads.len()).map_err(refuse)?;
            let spec = specs.get(&item.id).cloned().unwrap_or_default();
            let project_budget = project.as_ref().and_then(|id| specs.get(id)).map(|s| s.item_budget.clone()).unwrap_or_default();
            let siblings: Vec<_> = other_leads.iter().filter_map(|id| specs.get(id)).map(|s| s.item_budget.clone()).collect();
            lead_role::carve_budget(&project_budget, &siblings, &spec.item_budget).map_err(refuse)?;
            if !spec.owned_paths.is_empty() {
                let held: Vec<_> = other_leads
                    .iter()
                    .filter_map(|id| specs.get(id).filter(|s| !s.owned_paths.is_empty()).map(|s| lead_role::subtree_lease(id, s)))
                    .collect();
                path_leases::PathLeaseRegistry::new(held)
                    .check(&lead_role::subtree_lease(&item.id, &spec))
                    .map_err(|wait| conflict(format!("Lead subtree lease: {wait}")))?;
            }
            return Ok(None);
        }
        let Some(parent) = p.parent_chat_id.as_deref().filter(|_| hand_off_free(p, hand_off)) else {
            return Ok(None);
        };
        let Some(lead_chat) = lead_role::admit_child(&self.workspace, parent, chat_id).map_err(refuse)? else {
            return Ok(None);
        };
        let Some(lead_item) = self.workspace.standing_role_of_chat(&lead_chat).map(|role| role.scope_item_id) else {
            return Ok(None);
        };
        if let Some(lead_spec) = specs.get(&lead_item) {
            let spec = specs.get(&item.id).cloned().unwrap_or_default();
            lead_role::check_worker_paths(&lead_role::subtree_lease(&lead_item, lead_spec), &spec).map_err(refuse)?;
            let siblings: Vec<_> = snapshot
                .items
                .iter()
                .filter(|other| other.id != item.id && other.id != lead_item && other.archived_at.is_none())
                .filter(|other| snapshot.is_within(&lead_item, &other.id))
                // Only items a live Worker is running on hold a share.
                .filter(|other| {
                    snapshot.listed_chat_ids(other).any(|id| chats.iter().any(|chat| chat.id == *id && !chat.archived))
                })
                .filter_map(|other| specs.get(&other.id))
                .map(|s| s.item_budget.clone())
                .collect();
            lead_role::carve_budget(&lead_spec.item_budget, &siblings, &spec.item_budget).map_err(refuse)?;
        }
        Ok(Some(lead_item))
    }

    /// Hold the item's path lease for a worker launch, or refuse with the
    /// holder to wait behind. An item with no `ownedPaths` takes no lease.
    fn take_path_lease(
        &self,
        snapshot: &mast_proto::WorkOutlineSnapshot,
        item_id: &str,
        spec: Option<&mast_proto::WorkItemTaskSpec>,
        specs: &std::collections::HashMap<String, mast_proto::WorkItemTaskSpec>,
        lead_item: Option<&str>,
    ) -> Result<(), WorkOutlineError> {
        let Some(spec) = spec.filter(|spec| !spec.owned_paths.is_empty()) else {
            return Ok(());
        };
        let held = path_leases::active_holders(snapshot, item_id, &|id| specs.get(id).cloned(), &|chat| {
            let activity = crate::work_queue::registry_chat_activity(&self.workspace, chat);
            activity.worker && !activity.archived
        });
        // Only the Lead this Worker is a child of is exempt: its subtree
        // lease is the envelope the Worker's paths were checked against at
        // admission. Any other holder, a Lead's lease included, still counts.
        let held: Vec<_> = held.into_iter().filter(|lease| Some(lease.holder.as_str()) != lead_item).collect();
        path_leases::PathLeaseRegistry::new(held)
            .check(&path_leases::Lease::from_spec(item_id, spec))
            .map_err(|wait| conflict(format!("Path lease: {wait}")))?;
        if path_leases::launch_scale(Some(spec)) == path_leases::LaunchScale::DirectSingleAgent {
            tracing::info!(work_item = item_id, "work item launch: DirectSingleAgent fast path (one worker, no Lead)");
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
        exact_slug: bool,
    ) -> Result<(String, String), WorkOutlineError> {
        let repo_path = std::path::PathBuf::from(&binding.repo_path);
        let name_owned = name.to_owned();
        let target = {
            let repo_path = repo_path.clone();
            tokio::task::spawn_blocking(move || {
                if exact_slug {
                    crate::chat_workspace_plan::chat_worktree_target_for_slug(&repo_path, &name_owned)
                } else {
                    crate::chat_workspace_plan::chat_worktree_target(&repo_path, &name_owned)
                }
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
            if exact_slug {
                crate::chat_workspace_plan::create_chat_worktree_for_slug(&repo_path, &name_owned)
            } else {
                crate::chat_workspace_plan::create_chat_worktree(&repo_path, &name_owned)
            }
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
        role: Option<ProvisionalRole>,
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
            None => match role {
                Some(ProvisionalRole { tag, keeps_parent }) => self.workspace.create_provisional_role_chat_under(
                    chat_id,
                    space_id,
                    config,
                    execution_root.to_owned(),
                    tag,
                    parent_chat_id.filter(|_| keeps_parent).map(str::to_owned),
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
        self.continue_chat_for_work_item_inner(p, None).await
    }

    // User-authored continuation follows a completed hand-off while holding
    // the source and successor drain locks.
    async fn continue_chat_for_work_item_inner(
        &self,
        p: &ContinueChatForWorkItemParams,
        scheduled: Option<&mast_proto::ScheduledTaskDraft>,
    ) -> Result<ContinueChatForWorkItemReply, WorkOutlineError> {
        check_operation_id(&p.operation_id)?;
        check_prompt(&p.prompt)?;
        let op = p.operation_id.as_str();
        let message_id = derive_id(op, roles::MESSAGE);
        let command_id = derive_id(op, roles::COMMAND);
        // Match the composer boundary: a send which races an automatic
        // rotation waits for it and validates the committed successor.
        let source = self.doc_host.open(&p.chat_id).map_err(engine_error)?;
        let _source_guard = source.drain_lock.lock().await;
        let chain = self.doc_host.committed_handoff_chain(&p.chat_id).map_err(engine_error)?;
        let target = chain.last().expect("handoff chain includes source");
        let target_handle = self.doc_host.open(target).map_err(engine_error)?;
        let _target_guard = if target != &p.chat_id { Some(target_handle.drain_lock.lock().await) } else { None };
        if self.doc_host.committed_handoff_chain(&p.chat_id).map_err(engine_error)? != chain {
            return Err(error(WorkOutlineErrorCode::Invalid, "Manager handoff changed during send; retry"));
        }
        // A lost reply must not duplicate a turn accepted before rotation.
        for previous in chain.iter().take(chain.len().saturating_sub(1)) {
            let (command, landed) = self.doc_host.message_command_state(previous, &message_id).map_err(engine_error)?;
            if let Some(entry) = command {
                let original = command_request(&entry.payload).ok_or_else(|| conflict("operationId was already used for a different command"))?;
                if !request_matches_prompt(original, &p.prompt) {
                    return Err(conflict("operationId was already used for different turn inputs"));
                }
                let (state, detail) = self.turn_state(previous, &message_id)?;
                return Ok(ContinueChatForWorkItemReply { chat_id: previous.clone(), message_id, command_id, state, detail });
            }
            if landed {
                return Err(conflict("Cannot verify the original turn inputs; its command is unavailable"));
            }
            // A row held at the source before the hand-off is still the delivery.
            if self.doc_host.queued_message(previous, &message_id).map_err(engine_error)?.is_some() {
                return Ok(ContinueChatForWorkItemReply { chat_id: previous.clone(), message_id, command_id, state: WorkItemLaunchState::Queued, detail: None });
            }
        }
        let mut effective = p.clone();
        effective.chat_id = target.clone();
        self.continue_chat_for_work_item_target(&effective, scheduled).await
    }

    // Caller has already selected the target and holds its drain lock. The
    // scheduled path also holds the shared launch gate and must never follow
    // a different chat after persisting its selected target.
    async fn continue_chat_for_work_item_target(
        &self,
        p: &ContinueChatForWorkItemParams,
        scheduled: Option<&mast_proto::ScheduledTaskDraft>,
    ) -> Result<ContinueChatForWorkItemReply, WorkOutlineError> {
        check_operation_id(&p.operation_id)?;
        check_prompt(&p.prompt)?;
        let message_id = derive_id(&p.operation_id, roles::MESSAGE);
        let command_id = derive_id(&p.operation_id, roles::COMMAND);
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
        // Recover an accepted operation before consulting mutable chat config
        // or dispatch validation. A later settings change must not rewrite or
        // invalidate the original command.
        let (existing, landed) = self.doc_host.message_command_state(&p.chat_id, &message_id).map_err(engine_error)?;
        if let Some(entry) = existing {
            if command_request(&entry.payload).is_none_or(|request| !request_matches_prompt(request, &p.prompt)) {
                return Err(conflict("operationId was already used for different turn inputs"));
            }
            let (state, detail) = self.turn_state(&p.chat_id, &message_id)?;
            return Ok(ContinueChatForWorkItemReply { chat_id: p.chat_id.clone(), message_id, command_id, state, detail });
        }
        if landed {
            return Err(conflict("Cannot verify the original turn inputs; its command is unavailable"));
        }
        // The same send first held in the queue (the Chat was busy) and now
        // retried while it is idle: that row is the delivery, not a new one,
        // even if the user edited its text meanwhile.
        if self.doc_host.queued_message(&p.chat_id, &message_id).map_err(engine_error)?.is_some() {
            return Ok(ContinueChatForWorkItemReply { chat_id: p.chat_id.clone(), message_id, command_id, state: WorkItemLaunchState::Queued, detail: None });
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
        if let Some(task) = scheduled
            && canonical_lenient(Path::new(&task.cwd)) != canonical_lenient(Path::new(&cwd))
        {
            return Err(error(WorkOutlineErrorCode::Invalid, "The task's folder differs from the standing chat's folder"));
        }
        let config = scheduled.map(|task| &task.config).or(chat.config.as_ref());
        let mut request = prompt_request(&p.prompt, config, &cwd);
        if let Some(task) = scheduled {
            request.scheduled_task = Some(task.name.clone());
            request.scheduled_notion_writes = task.write_to_notion;
        }
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
        self.doc_host
            .queue_command_with_id(&p.chat_id, &command_id, payload)
            .map_err(|err| match err {
                EngineError::InstructionDelivery(rejection) => {
                    error(WorkOutlineErrorCode::Capability, rejection.message.clone())
                }
                other => engine_error(other),
            })?;
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

/// The Space's default model for the role this launch starts: a Manager for a
/// standing-role launch, a worker for any other child of a chat. `None` leaves
/// the built-in (a launch with no parent chat is a user start, not a worker).
/// Automatic Manager hand-off passes no settings: it freezes the model into its
/// durable intent, so a successor follows the request, not later setting edits.
fn role_default_model<'a>(
    p: &CreateChatForWorkItemParams,
    settings: Option<&'a mast_proto::work_outline::WorkOutlineSpaceSettings>,
) -> Option<&'a str> {
    let settings = settings?;
    if p.standing_role == Some(mast_proto::StandingRoleKind::Manager) {
        return settings.manager_default_model.as_deref();
    }
    if p.standing_role.is_some() || p.parent_chat_id.is_none() {
        return None;
    }
    Some(
        settings
            .worker_default_model
            .as_deref()
            .unwrap_or(mast_proto::DEFAULT_WORKER_CLAUDE_MODEL),
    )
}

/// The model a retried launch reuses from the chat its first attempt stored:
/// non-empty and on the harness the retry resolved to. A retry whose harness
/// drifted (the host default changed in between) resolves a model of its own.
fn reused_stored_model(harness: mast_proto::HarnessId, stored: &ChatConfig) -> Option<String> {
    stored
        .model
        .as_deref()
        .filter(|m| stored.harness == harness && !m.trim().is_empty())
        .map(str::to_owned)
}

/// `p` with an explicit model: an absent config becomes the host's default
/// harness with default options, and an empty model becomes the launch's role
/// default (see [`role_default_model`]), else that harness's built-in default.
fn with_default_model(
    p: &CreateChatForWorkItemParams,
    default_harness: mast_proto::HarnessId,
    chat_id: &str,
    settings: Option<&mast_proto::work_outline::WorkOutlineSpaceSettings>,
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
    if config.fill_role_default_model(role_default_model(&p, settings)) {
        tracing::debug!(chat = %chat_id, "work item launch had no model; filled in the role default");
    }
    p.config = Some(config);
    p
}

/// `p` with the Space's worker context window when this launch starts a
/// worker (a child chat, not a role or hand-off successor) on Codex without a
/// window of its own. `None` when nothing changes.
fn with_worker_context_window(
    p: &CreateChatForWorkItemParams,
    settings: &mast_proto::work_outline::WorkOutlineSpaceSettings,
    hand_off: bool,
) -> Option<CreateChatForWorkItemParams> {
    let tokens = settings.worker_context_window.filter(|n| *n > 0)?;
    let config = p.config.as_ref()?;
    if hand_off
        || p.parent_chat_id.is_none()
        || p.standing_role.is_some()
        || config.harness != mast_proto::HarnessId::Codex
        || config.model_options.contains_key("contextWindow")
    {
        return None;
    }
    let mut p = p.clone();
    p.config
        .as_mut()?
        .model_options
        .insert("contextWindow".into(), tokens.to_string().into());
    Some(p)
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

fn command_request(payload: &SessionCommandPayload) -> Option<&RunRequest> {
    match payload {
        SessionCommandPayload::Run { request, .. } | SessionCommandPayload::RunWithInstructions { request, .. } => Some(request),
        _ => None,
    }
}

fn request_matches_prompt(request: &RunRequest, prompt: &WorkItemPrompt) -> bool {
    request.prompt == crate::doc_host::queued_message_prompt(&prompt.text, &prompt.attachments)
        && request.attachments == prompt.attachments
}

#[cfg(test)]
mod default_model_tests {
    use super::*;
    use mast_proto::work_outline::WorkOutlineSpaceSettings;
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
        let p = with_default_model(&params(None), HarnessId::ClaudeCode, "chat", None);
        let config = p.config.expect("config filled in");
        assert_eq!(config.harness, HarnessId::ClaudeCode);
        assert_eq!(config.model.as_deref(), Some("claude-opus-5-5"));
        let request = prompt_request(&p.initial_prompt, Some(&config), "/w");
        assert_eq!(request.model.as_deref(), Some("claude-opus-5-5"));
    }

    fn worker(harness: HarnessId, options: &[(&str, &str)]) -> CreateChatForWorkItemParams {
        let mut p = params(Some(ChatConfig {
            approval_rules: false,
            harness,
            model: None,
            reasoning: None,
            model_options: options.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        }));
        p.parent_chat_id = Some("manager".into());
        p
    }

    fn window_of(p: &CreateChatForWorkItemParams) -> Option<String> {
        p.config.as_ref()?.model_options.get("contextWindow")?.as_str().map(str::to_owned)
    }

    #[test]
    fn a_codex_worker_without_a_window_gets_the_space_default_and_claude_gets_nothing() {
        let settings = mast_proto::work_outline::WorkOutlineSpaceSettings::default();
        let codex = with_worker_context_window(&worker(HarnessId::Codex, &[]), &settings, false)
            .expect("codex worker gets the default");
        assert_eq!(window_of(&codex).as_deref(), Some("200000"));
        let request = prompt_request(&codex.initial_prompt, codex.config.as_ref(), "/w");
        assert_eq!(request.model_options["contextWindow"], "200000");
        assert!(
            with_worker_context_window(&worker(HarnessId::ClaudeCode, &[]), &settings, false)
                .is_none(),
            "Claude has no supported cap, so nothing is set"
        );
    }

    #[test]
    fn the_worker_window_default_leaves_every_other_launch_alone() {
        let settings = mast_proto::work_outline::WorkOutlineSpaceSettings::default();
        let skipped = |p: CreateChatForWorkItemParams, hand_off: bool| {
            with_worker_context_window(&p, &settings, hand_off).is_none()
        };
        // An explicit choice, including `default`, wins over the Space.
        assert!(skipped(worker(HarnessId::Codex, &[("contextWindow", "default")]), false));
        assert!(skipped(worker(HarnessId::Codex, &[("contextWindow", "500000")]), false));
        // Human chats, roles and hand-off successors are not workers.
        let mut human = worker(HarnessId::Codex, &[]);
        human.parent_chat_id = None;
        assert!(skipped(human, false));
        let mut role = worker(HarnessId::Codex, &[]);
        role.standing_role = Some(mast_proto::StandingRoleKind::Manager);
        assert!(skipped(role, false));
        assert!(skipped(worker(HarnessId::Codex, &[]), true));
        // A Space that turned the default off, or a launch without a config.
        let mut off = settings.clone();
        off.worker_context_window = None;
        assert!(with_worker_context_window(&worker(HarnessId::Codex, &[]), &off, false).is_none());
        assert!(skipped(params(None), false));
        // A Space's own size replaces the built-in one.
        let mut custom = settings;
        custom.worker_context_window = Some(128_000);
        let sized = with_worker_context_window(&worker(HarnessId::Codex, &[]), &custom, false).unwrap();
        assert_eq!(window_of(&sized).as_deref(), Some("128000"));
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
            None,
        );
        assert_eq!(
            kept.config.unwrap().model.as_deref(),
            Some("claude-sonnet-5")
        );
        let filled = with_default_model(&params(Some(config(None))), HarnessId::ClaudeCode, "c", None);
        assert_eq!(
            filled.config.unwrap().model.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    fn model_of(p: &CreateChatForWorkItemParams, settings: &WorkOutlineSpaceSettings) -> Option<String> {
        with_default_model(p, HarnessId::ClaudeCode, "c", Some(settings)).config.unwrap().model
    }

    #[test]
    fn a_worker_without_a_model_gets_the_worker_default_and_a_manager_keeps_opus() {
        let settings = WorkOutlineSpaceSettings::default();
        assert_eq!(model_of(&worker(HarnessId::ClaudeCode, &[]), &settings).as_deref(), Some("claude-sonnet-5-5"));
        let mut manager = worker(HarnessId::ClaudeCode, &[]);
        manager.standing_role = Some(mast_proto::StandingRoleKind::Manager);
        manager.parent_chat_id = None;
        assert_eq!(model_of(&manager, &settings).as_deref(), Some("claude-opus-5-5"));
        let mut user_start = worker(HarnessId::ClaudeCode, &[]);
        user_start.parent_chat_id = None;
        assert_eq!(model_of(&user_start, &settings).as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn configured_role_defaults_apply_and_an_explicit_model_wins() {
        let settings = WorkOutlineSpaceSettings {
            worker_default_model: Some("claude-haiku-4-5-20251001".into()),
            manager_default_model: Some("claude-sonnet-5-5".into()),
            ..Default::default()
        };
        assert_eq!(model_of(&worker(HarnessId::ClaudeCode, &[]), &settings).as_deref(), Some("claude-haiku-4-5-20251001"));
        let mut manager = worker(HarnessId::ClaudeCode, &[]);
        manager.standing_role = Some(mast_proto::StandingRoleKind::Manager);
        assert_eq!(model_of(&manager, &settings).as_deref(), Some("claude-sonnet-5-5"));
        let mut explicit = worker(HarnessId::ClaudeCode, &[]);
        explicit.config.as_mut().unwrap().model = Some("claude-opus-5-5".into());
        assert_eq!(model_of(&explicit, &settings).as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn role_defaults_never_reach_other_harnesses() {
        let settings = WorkOutlineSpaceSettings::default();
        assert_eq!(model_of(&worker(HarnessId::Codex, &[]), &settings), None);
    }

    #[test]
    fn a_stored_model_is_reused_only_on_the_harness_that_stored_it() {
        let stored = |harness: HarnessId, model: Option<&str>| ChatConfig {
            approval_rules: false,
            harness,
            model: model.map(str::to_owned),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        };
        let reused = |harness, config: ChatConfig| reused_stored_model(harness, &config);
        assert_eq!(
            reused(HarnessId::ClaudeCode, stored(HarnessId::ClaudeCode, Some("claude-sonnet-5-5"))).as_deref(),
            Some("claude-sonnet-5-5")
        );
        // The retry resolved to another harness: the stored id is meaningless there.
        assert_eq!(reused(HarnessId::Codex, stored(HarnessId::ClaudeCode, Some("claude-sonnet-5-5"))), None);
        // Nothing stored, or a blank model, reuses nothing.
        assert_eq!(reused(HarnessId::ClaudeCode, stored(HarnessId::ClaudeCode, None)), None);
        assert_eq!(reused(HarnessId::ClaudeCode, stored(HarnessId::ClaudeCode, Some("  "))), None);
    }
}

#[cfg(test)]
mod credit_successor_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use mast_harness::{Harness, HarnessError, RunControls};
    use mast_proto::{AgentEvent, DEFAULT_WORKER_CLAUDE_MODEL, DoneStatus, HarnessId, Model, ReasoningLevel};
    use serde_json::json;

    use super::*;
    use crate::{EngineCore, HarnessRegistry};

    /// A harness whose every turn completes at once.
    struct Idle(HarnessId);

    #[async_trait]
    impl Harness for Idle {
        fn id(&self) -> HarnessId { self.0 }
        fn display_name(&self) -> &str { "Idle" }
        fn reasoning_levels(&self) -> &[ReasoningLevel] { &[ReasoningLevel::Medium, ReasoningLevel::High] }
        fn deterministic_turn_end(&self) -> bool { true }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> { Ok(Vec::new()) }

        async fn run(
            &self, _request: RunRequest, _controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let done = AgentEvent::Done { status: DoneStatus::Completed, result: None, error: None, session_id: None };
            Ok(futures::stream::iter([Ok(done)]).boxed())
        }
    }

    #[tokio::test]
    async fn a_successor_keeps_the_model_settings_only_on_the_same_harness() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, data) = (root.join("repo"), root.join("data"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("device-id"), "dev-a").unwrap();
        let registry = HarnessRegistry::new();
        for id in [HarnessId::ClaudeCode, HarnessId::Codex] {
            registry.register(Arc::new(Idle(id)));
        }
        let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::ClaudeCode, None).unwrap();
        let client = mast_rpc::memory_client(core.rpc_service());
        for params in [
            json!({"op":"createSpace","spaceId":"space-1","deviceId":"dev-a","path":repo}),
            json!({"op":"createChat","chatId":"manager","spaceId":"space-1","deviceId":"dev-a"}),
        ] {
            client.call(mast_rpc::methods::MUTATE, params).await.unwrap();
        }
        client.call(mast_rpc::methods::WORK_OUTLINE_MUTATE,
            json!({"op":"createWorkItem","spaceId":"space-1","title":"w","operationId":"item-w"})).await.unwrap();

        // Neither harness's default model, level or options.
        let options = [("contextWindow".to_owned(), json!("400000")), ("fastMode".to_owned(), json!(true))];
        let stalled_config = ChatConfig {
            approval_rules: false,
            harness: HarnessId::ClaudeCode,
            model: Some("claude-opus-5-5".into()),
            reasoning: Some(ReasoningLevel::High),
            model_options: options.into_iter().collect(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        };
        let launch = &core.work_item_launch;
        let wakes = &core.orchestrator_wakes;
        let stalled = launch.create_chat_for_work_item(&CreateChatForWorkItemParams {
            work_item_id: derive_id("item-w", roles::ITEM),
            initial_prompt: WorkItemPrompt { text: "do the task".into(), attachments: Vec::new() },
            config: Some(stalled_config.clone()),
            execution_override: None,
            worktree_choice: None,
            operation_id: "launch-w".into(),
            parent_chat_id: Some("manager".into()),
            target_device_id: None,
            account_id: None,
            standing_role: None,
        }).await.unwrap().chat_id;
        let stalled_config = core.workspace.chat_config(&stalled).unwrap();
        core.workspace.rename_chat(&stalled, "Fix the bug").unwrap();

        let same = launch.create_credit_successor(&stalled, "successor-same", HarnessId::ClaudeCode, "go on".into(), wakes)
            .await.unwrap().chat_id;
        let kept = core.workspace.chat_config(&same).unwrap();
        assert_ne!(same, stalled);
        assert_eq!(kept.harness, HarnessId::ClaudeCode);
        assert_eq!(kept.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(kept.reasoning, Some(ReasoningLevel::High));
        assert_eq!(kept.model_options, stalled_config.model_options);
        assert_eq!(kept.model_options.get("contextWindow"), Some(&json!("400000")));
        // A real hand-off: lineage is recorded, the successor reports to the
        // same parent, and the stalled chat is kept.
        assert_eq!(core.workspace.handed_off_to(&stalled).as_deref(), Some(same.as_str()));
        assert_eq!(core.workspace.chat(&same).unwrap().unwrap().parent_chat_id.as_deref(), Some("manager"));
        assert!(!core.workspace.chat(&stalled).unwrap().unwrap().archived);
        // The successor is titled apart from the chat it continues, once.
        assert_eq!(core.workspace.chat(&same).unwrap().unwrap().title.as_deref(), Some("Fix the bug (continued)"));

        // A retry of the same operation returns the same chat.
        let again = launch.create_credit_successor(&stalled, "successor-same", HarnessId::ClaudeCode, "go on".into(), wakes)
            .await.unwrap().chat_id;
        assert_eq!(again, same);

        // Back to Claude from a Codex chat: nothing of Codex's settings carries.
        let mut codex = stalled_config.clone();
        codex.harness = HarnessId::Codex;
        codex.model = Some("gpt-5".into());
        core.workspace.set_chat_config(&stalled, &codex).unwrap();
        let cross = launch.create_credit_successor(&stalled, "successor-cross", HarnessId::ClaudeCode, "go on".into(), wakes)
            .await.unwrap().chat_id;
        let fresh = core.workspace.chat_config(&cross).unwrap();
        assert_eq!(fresh.harness, HarnessId::ClaudeCode);
        assert_eq!(fresh.model.as_deref(), Some(DEFAULT_WORKER_CLAUDE_MODEL));
        assert_eq!(fresh.reasoning, None);
        assert!(fresh.model_options.is_empty(), "{:?}", fresh.model_options);
        core.shutdown().await;
    }
}
