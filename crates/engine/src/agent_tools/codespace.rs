//! A dedicated action tool. Manual external-run registration never calls it.

use super::*;
use crate::external_runs::launcher::{self, Action, Dispatch};
use crate::permission_rules::{PermissionRequest, RuleDecision};
use mast_proto::{UserActionKind, WorkOutlineActorKind};

const APPROVAL_KEY_PREFIX: &str = "codespace-";

pub(crate) fn is_codespace_approval_key(key: &str) -> bool {
    key.strip_prefix(APPROVAL_KEY_PREFIX).is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[derive(Default)]
pub(super) struct Launches {
    active: Option<String>,
    recent: std::collections::VecDeque<(String, Value)>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Mode {
    RequestApproval,
    Execute,
    Status,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Args {
    action: Action,
    mode: Mode,
    work_item_id: Option<String>,
    #[serde(default)]
    scope: String,
    /// Stable across lost replies and process restarts. Use a new value
    /// for a new follow-up on the same session, never to retry a launch.
    #[serde(default)]
    idempotency_key: String,
}

impl AgentToolsService {
    pub(super) fn codespace_task(
        &self,
        context: &CallContext<'_>,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let args: Args = parse_args(arguments)?;
        let root = context.require_item("managing a Codespace task")?;
        let space = &context.grant.space_id;
        let owner = &context.grant.chat_id;
        let item = args
            .work_item_id
            .as_deref()
            .map(|id| self.resolve_work_item_id(context, id))
            .transpose()?
            .unwrap_or_else(|| root.clone());
        let snapshot = self.work_outline.snapshot(space);
        if !snapshot.is_within(&root, &item) || !owns_scope(&self.workspace, space, owner) {
            return Err(WorkOutlineError::unauthorized("Codespace tasks run on the owning Space host for the caller's item or its descendants.").into());
        }
        let target = snapshot
            .item(&item)
            .filter(|item| item.archived_at.is_none())
            .ok_or_else(|| {
                AgentToolError::invalid("The Codespace task needs an unarchived work item.")
            })?;
        if args.scope.chars().count() > 3000 || args.idempotency_key.chars().count() > 100 {
            return Err(AgentToolError::invalid(
                "Scope and attempt are limited to 3000 and 100 characters.",
            ));
        }
        let starts_worker = matches!(args.action, Action::Launch | Action::Resume);
        if starts_worker && args.scope.trim().is_empty() {
            return Err(AgentToolError::invalid(
                "Provide the verified scope and any resolved decisions for this turn.",
            ));
        }
        if matches!(args.action, Action::Resume | Action::Stop | Action::Cleanup)
            && args.idempotency_key.trim().is_empty()
        {
            return Err(AgentToolError::invalid(
                "Resume, stop and cleanup need an idempotency key stable across retries.",
            ));
        }
        let approval_detail = format!(
            "Work item: {} ({item})\n\n{}",
            target.title,
            args.scope.trim()
        );
        if approval_detail.chars().count() > mast_proto::user_action_limits::MAX_REQUEST_INFO_CHARS
        {
            return Err(AgentToolError::invalid(
                "Shorten the scope so the full operation fits in Required Actions.",
            ));
        }
        let sessions = lock(&self.sessions).clone();
        let mutating = args.action != Action::Inspect;
        check_host_policy(
            &self.workspace,
            sessions.as_ref(),
            owner,
            mutating && args.mode != Mode::Status,
        )
        .map_err(AgentToolError::invalid)?;
        let dispatch = Dispatch::load(
            &self.workspace.store_root(),
            space,
            &item,
            owner,
            args.action,
            args.scope,
            &args.idempotency_key,
        )
        .map_err(AgentToolError::invalid)?;
        let digest = dispatch.digest.clone();
        let key = format!("{APPROVAL_KEY_PREFIX}{digest}");
        let approval_text = match args.action {
            Action::Launch => "Approve launching a Codespace worker with this scope",
            Action::Resume => "Approve resuming the existing Codespace worker with this scope",
            Action::Stop => "Approve stopping the verified Codespace",
            Action::Cleanup => {
                "Approve preserving the transcript and deleting the merged Codespace"
            }
            Action::Inspect => "",
        };
        if args.mode == Mode::RequestApproval {
            if args.action == Action::Inspect {
                return Err(AgentToolError::invalid("Inspection is read-only."));
            }
            if starts_worker && blocked(&snapshot, &item, &key) {
                return Err(AgentToolError::invalid(
                    "Resolve the outstanding work-item decisions before requesting dispatch.",
                ));
            }
            return self.create_user_action_request(
                context,
                &json!({
                    "request": approval_text,
                    "detail": approval_detail,
                    "kind": "approve",
                    "dedupeKey": key,
                    "idempotencyKey": key
                }),
            );
        }
        let mut launches = lock(&self.codespaces);
        if launches.active.as_deref() == Some(&digest) {
            return Ok(json!({ "state": "inProgress", "operationId": digest }));
        }
        if args.mode == Mode::Status {
            return Ok(launches
                .recent
                .iter()
                .rev()
                .find(|(id, _)| id == &digest)
                .map(|(_, result)| result.clone())
                .unwrap_or_else(|| json!({ "state": "noRecentResult", "operationId": digest })));
        }
        if launches.active.is_some() {
            return Ok(json!({ "state": "hostBusy", "operationId": digest }));
        }

        let workspace = self.workspace.clone();
        let outline = self.work_outline.clone();
        let state = self.codespaces.clone();
        let space = space.clone();
        let owner = owner.clone();
        let action = args.action;
        let authorize = move || {
            check_host_policy(&workspace, sessions.as_ref(), &owner, mutating)?;
            if action != Action::Inspect && crate::work_outline::writes_disabled() {
                return Err("Work Outline writes are disabled on this host.");
            }
            if !owns_scope(&workspace, &space, &owner)
                || workspace.chat_assigned_work_item(&owner, &space).as_deref()
                    != Some(root.as_str())
            {
                return Err("The requesting Chat or Space changed host or assignment.");
            }
            let snapshot = outline.snapshot(&space);
            if !snapshot.is_within(&root, &item)
                || snapshot.item(&item).is_none_or(|item| {
                    item.archived_at.is_some()
                        || (starts_worker && item.status == WorkItemStatus::Completed)
                })
            {
                return Err("This work item is no longer eligible for the operation.");
            }
            if mutating {
                require_native_approval(snapshot.user_action_requests.iter().any(|request| {
                    approved(
                        request,
                        &root,
                        &owner,
                        &key,
                        approval_text,
                        &approval_detail,
                    )
                }))?;
            }
            if starts_worker && blocked(&snapshot, &item, &key) {
                return Err("Resolve the outstanding decisions before dispatch.");
            }
            Ok(())
        };
        authorize().map_err(AgentToolError::invalid)?;
        let outline = self.work_outline.clone();
        let actor = outline.agent_actor(&context.grant.chat_id, None);
        let settings = snapshot.space_settings.external_runs;
        let operation_id = digest.clone();
        launches.active = Some(digest.clone());
        let worker = std::thread::Builder::new().name("mast-codespace-task".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch.execute(&settings, authorize, |params| {
                    outline.mutate_as(&WorkOutlineMutation::UpsertExternalRun(params), &actor)
                        .map_err(|_| "Could not record the run in Tracker. Retry this operation to reconcile.")?;
                    Ok(())
                })
            })).unwrap_or(Err("The launcher task failed. Retry the same operation to reconcile."));
            let reply = match result {
                Ok(result) => json!({ "state": "completed", "operationId": digest, "result": result }),
                Err(error) => json!({ "state": "blocked", "operationId": digest, "reason": error }),
            };
            let mut launches = lock(&state);
            launches.active = None;
            launches.recent.retain(|(id, _)| id != &digest);
            if launches.recent.len() == 64 {
                launches.recent.pop_front();
            }
            launches.recent.push_back((digest, reply));
        });
        if worker.is_err() {
            launches.active = None;
            return Err(AgentToolError::invalid(
                "Could not queue the Codespace operation.",
            ));
        }
        Ok(json!({ "state": "queued", "operationId": operation_id }))
    }
}

fn check_host_policy(
    workspace: &WorkspaceHost,
    sessions: Option<&crate::SessionsEngine>,
    chat_id: &str,
    mutating: bool,
) -> Result<(), &'static str> {
    let sessions = sessions.ok_or("The host's permission rules are unavailable.")?;
    let chat = workspace.chat(chat_id).ok().flatten();
    let can_write = chat
        .and_then(|chat| chat.config)
        .is_some_and(|config| config.sandbox != mast_proto::SandboxLevel::ReadOnly)
        && !workspace.manager_runs_read_only(chat_id)
        && !sessions
            .last_request(chat_id)
            .is_some_and(|request| request.sandbox == mast_proto::SandboxLevel::ReadOnly);
    let decision = sessions
        .permission_rules()
        .decide(&PermissionRequest::McpTool {
            server: "mast-agent".into(),
            tool: tools::CODESPACE_TASK.into(),
        });
    check_native_policy(can_write, mutating, decision)
}

fn check_native_policy(
    can_write: bool,
    mutating: bool,
    decision: RuleDecision,
) -> Result<(), &'static str> {
    if matches!(decision, RuleDecision::Deny(_)) {
        return Err("The host permission rules deny the native Codespace task tool.");
    }
    if mutating && !can_write {
        return Err("Run this Codespace operation from a chat with a writable sandbox.");
    }
    Ok(())
}

fn require_native_approval(approved: bool) -> Result<(), &'static str> {
    approved.then_some(()).ok_or(
        "Call this tool with mode requestApproval, then wait for the user to approve the exact operation in Required Actions.",
    )
}

fn owns_scope(workspace: &WorkspaceHost, space_id: &str, chat_id: &str) -> bool {
    let device_id = workspace.device_id();
    workspace
        .space(space_id)
        .ok()
        .flatten()
        .is_some_and(|space| space.device_id == device_id)
        && workspace.chat(chat_id).ok().flatten().is_some_and(|chat| {
            chat.device_id == device_id && chat.space_id.as_deref() == Some(space_id)
        })
}

fn blocked(snapshot: &mast_proto::WorkOutlineSnapshot, item: &str, key: &str) -> bool {
    snapshot.user_action_requests.iter().any(|request| {
        request.is_unresolved()
            && snapshot.is_within(&request.work_item_id, item)
            && request.dedupe_key.as_deref() != Some(key)
    })
}

fn approved(
    request: &mast_proto::UserActionRequestView,
    root: &str,
    owner: &str,
    key: &str,
    text: &str,
    detail: &str,
) -> bool {
    request.work_item_id == root
        && request.chat_id == owner
        && request.dedupe_key.as_deref() == Some(key)
        && request.kind == Some(UserActionKind::Approve)
        && request.text == text
        && request.info.as_deref() == Some(detail)
        && !request.is_open()
        && !request.deduped
        && request.resolution.as_ref().is_some_and(|resolution| {
            resolution.by.kind == WorkOutlineActorKind::User
                && resolution.outcome.as_deref() == Some("approved")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_dispatch_observes_host_denies_and_uses_exact_approval_for_ask() {
        use crate::permission_rules::RuleSet;

        let request = PermissionRequest::McpTool {
            server: "mast-agent".into(),
            tool: tools::CODESPACE_TASK.into(),
        };
        let denied = RuleSet::parse(
            r#"{"permissions":{
            "allow":["mcp__mast-agent__work_item_codespace_task"],
            "deny":["mcp__mast-agent__work_item_codespace_task"]
        }}"#,
        );
        assert!(check_native_policy(true, true, denied.decide(&request)).is_err());

        let unmatched = RuleSet::parse(r#"{"permissions":{"allow":["Bash(example launch)"]}}"#);
        let asked = RuleSet::parse(
            r#"{"permissions":{"ask":["mcp__mast-agent__work_item_codespace_task"]}}"#,
        );
        for rules in [unmatched, asked] {
            assert_eq!(rules.decide(&request), RuleDecision::Ask);
            assert!(check_native_policy(false, true, rules.decide(&request)).is_err());
            assert!(check_native_policy(true, true, rules.decide(&request)).is_ok());
            assert!(
                require_native_approval(false)
                    .unwrap_err()
                    .contains("requestApproval")
            );
            assert!(require_native_approval(true).is_ok());
        }
    }

    #[test]
    fn only_the_humans_approval_of_the_exact_operation_counts() {
        let mut request: mast_proto::UserActionRequestView = serde_json::from_value(json!({
            "id": "approval", "workItemId": "parent", "chatId": "owner", "dedupeKey": "codespace-key",
            "kind": "approve", "state": "resolved", "text": "approve the action", "info": "exact scope",
            "resolution": { "outcome": "approved", "by": { "kind": "user" } }
        })).unwrap();
        let check = |request: &mast_proto::UserActionRequestView| {
            approved(
                request,
                "parent",
                "owner",
                "codespace-key",
                "approve the action",
                "exact scope",
            )
        };
        assert!(check(&request));
        request.resolution.as_mut().unwrap().by.kind = WorkOutlineActorKind::Agent;
        assert!(!check(&request));
        request.resolution.as_mut().unwrap().by.kind = WorkOutlineActorKind::User;
        request.resolution.as_mut().unwrap().outcome = Some("declined".into());
        assert!(!check(&request));
        request.resolution.as_mut().unwrap().outcome = Some("approved".into());
        request.info = Some("other scope".into());
        assert!(!check(&request));
    }
}
