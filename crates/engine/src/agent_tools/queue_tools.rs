//! `work_queue_read` / `work_queue_set` / `work_queue_pause`: a project's
//! Manager (and an admin chat) manages the project's work queue with built-in
//! tools. The caller's role is read from the synced role record on every
//! call, every target must sit inside the project, and the registry writer
//! re-checks the holder and the targets under its own lock, so a hand-off or
//! a concurrent move cannot slip a write through. Workers, other projects'
//! Managers and a superseded Manager are refused.

use mast_proto::{
    FieldPatch, QueueChange, QueuePausedBy, SetQueuePausedParams, SetQueuePositionParams,
    UpdateWorkItemParams, WorkItemChanges, WorkOutlineMutation, WorkOutlinePlacement,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolErrorCode, AgentToolsService, CallContext, non_empty, parse_args};

const DEFAULT_OFFLOAD_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadArgs {
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetArgs {
    work_item_id: String,
    action: String,
    #[serde(default)]
    position: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    waiting_until: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PauseArgs {
    #[serde(default)]
    project_id: Option<String>,
    paused: bool,
    #[serde(default)]
    reason: Option<String>,
}

fn invalid(message: impl Into<String>) -> AgentToolError {
    AgentToolError::invalid(message.into())
}

fn forbidden(message: impl Into<String>) -> AgentToolError {
    AgentToolError::refused(AgentToolErrorCode::Forbidden, message)
}

fn time_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(text) => text
            .parse::<i64>()
            .ok()
            .or_else(|| chrono::DateTime::parse_from_rfc3339(text).ok().map(|at| at.timestamp_millis())),
        _ => None,
    }
}

impl AgentToolsService {
    /// The project this call may manage and in what capacity. The holder of
    /// the Manager role is scoped to its own project; an admin chat may name
    /// any project of its Space. Anything else is refused.
    fn queue_scope(
        &self,
        context: &CallContext<'_>,
        project: Option<String>,
    ) -> Result<(String, QueuePausedBy), AgentToolError> {
        let space = &context.grant.space_id;
        let admin = context.admin_space.as_deref() == Some(space.as_str());
        let named = project
            .map(|id| self.resolve_work_item_id(context, &id))
            .transpose()?;
        let (project, by) = match (&context.manager_scope, named, admin) {
            // `manager_scope` is empty for a chat with an admin grant, so a
            // Manager here is never also an admin.
            (Some(root), None, _) => (root.clone(), QueuePausedBy::Manager),
            (Some(root), Some(id), _) if id == *root => (id, QueuePausedBy::Manager),
            (None, Some(id), true) => (id, QueuePausedBy::Admin),
            (None, None, true) => {
                let current = context.require_item("managing the queue")?;
                let snapshot = self.work_outline.snapshot(space);
                let root = snapshot
                    .project_root(&current)
                    .map(|item| item.id.clone())
                    .ok_or_else(|| invalid("name the projectId of the queue"))?;
                (root, QueuePausedBy::Admin)
            }
            (Some(_), Some(_), false) => {
                return Err(forbidden("a project Manager can only manage the queue of its own project"));
            }
            _ => {
                return Err(forbidden(
                    "only the project's Manager role holder or an admin chat can manage a work queue",
                ));
            }
        };
        let snapshot = self.work_outline.snapshot(space);
        if !snapshot.item(&project).is_some_and(|item| item.is_project && item.archived_at.is_none()) {
            return Err(invalid("that item is not a live project"));
        }
        Ok((project, by))
    }

    /// An item the caller names must sit inside the project (never the project).
    fn queue_item(&self, context: &CallContext<'_>, project: &str, raw: &str) -> Result<String, AgentToolError> {
        let id = self.resolve_work_item_id(context, raw)?;
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        if snapshot.item(&id).is_none() || !snapshot.is_within(project, &id) {
            return Err(forbidden(format!("item {id} is outside your project; a Manager can only change its own project's queue")));
        }
        if id == project {
            return Err(invalid("the project itself cannot be queued"));
        }
        Ok(id)
    }

    fn queue_guard(&self, context: &CallContext<'_>, by: QueuePausedBy, root: &str, inside: Vec<String>) -> Option<crate::work_outline::ManagerGuard> {
        (by == QueuePausedBy::Manager).then(|| crate::work_outline::ManagerGuard {
            chat_id: context.grant.chat_id.clone(),
            root: root.to_owned(),
            inside,
            ..Default::default()
        })
    }

    pub(super) fn work_queue_read(&self, context: &CallContext<'_>, arguments: &Value) -> Result<Value, AgentToolError> {
        let args: ReadArgs = parse_args(arguments)?;
        let (project, _) = self.queue_scope(context, non_empty(args.project_id))?;
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        let now = chrono::Utc::now().timestamp_millis();
        let cache = super::lock(&self.pr_ticket_cache).clone();
        let queue = snapshot.work_queue(
            &project,
            now,
            &|url| match &cache {
                Some(cache) => crate::work_queue::cached_pr_facts(cache, url, crate::work_queue::PrLookup::Peek),
                None => mast_proto::work_status::PullRequestFacts::default(),
            },
            &|chat| crate::work_queue::registry_chat_activity(&self.workspace, chat),
        );
        let entries: Vec<Value> = queue
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
                value["position"] = json!(index + 1);
                if let Some(item) = snapshot.item(&entry.work_item_id) {
                    if let Some(until) = item.waiting_until.filter(|until| *until > now) {
                        value["waitingUntil"] = json!(until);
                        value["waitingReason"] = json!(item.waiting_reason);
                    }
                }
                value
            })
            .collect();
        let item = snapshot.item(&project);
        Ok(json!({
            "projectId": project,
            "paused": item.is_some_and(|item| item.queue_paused),
            "pause": item.and_then(|item| item.queue_pause.clone()),
            "entries": entries,
        }))
    }

    pub(super) fn work_queue_set(&self, context: &CallContext<'_>, arguments: &Value) -> Result<Value, AgentToolError> {
        let args: SetArgs = parse_args(arguments)?;
        let operation_id = context.operation_id(arguments)?;
        let (project, by) = self.queue_scope(context, None)?;
        let item = self.queue_item(context, &project, &args.work_item_id)?;
        let mut inside = vec![item.clone()];
        let mutation = match args.action.as_str() {
            "place" | "move" => {
                let placement = match &args.position {
                    None => WorkOutlinePlacement::Last,
                    Some(Value::String(word)) if word == "first" => WorkOutlinePlacement::First,
                    Some(Value::String(word)) if word == "last" => WorkOutlinePlacement::Last,
                    Some(Value::Object(map)) => {
                        let (before, raw) = match (map.get("before"), map.get("after")) {
                            (Some(raw), None) => (true, raw),
                            (None, Some(raw)) => (false, raw),
                            _ => return Err(invalid("position takes exactly one of before or after")),
                        };
                        let anchor = self.queue_item(context, &project, raw.as_str().unwrap_or_default())?;
                        inside.push(anchor.clone());
                        if before {
                            WorkOutlinePlacement::Before { sibling_work_item_id: anchor }
                        } else {
                            WorkOutlinePlacement::After { sibling_work_item_id: anchor }
                        }
                    }
                    Some(_) => return Err(invalid("position is first, last, {before: id} or {after: id}")),
                };
                WorkOutlineMutation::SetQueuePosition(SetQueuePositionParams {
                    work_item_id: item,
                    change: QueueChange::Place { placement },
                    operation_id,
                })
            }
            "remove" => WorkOutlineMutation::SetQueuePosition(SetQueuePositionParams {
                work_item_id: item,
                change: QueueChange::Remove,
                operation_id,
            }),
            "offload" => {
                let reason = non_empty(args.reason).ok_or_else(|| invalid("offload needs a reason"))?;
                let now = chrono::Utc::now().timestamp_millis();
                let until = match &args.waiting_until {
                    None => now + DEFAULT_OFFLOAD_MS,
                    Some(value) => time_ms(value).filter(|until| *until > now).ok_or_else(|| invalid("waitingUntil must be a future unix-ms or ISO time"))?,
                };
                WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                    work_item_id: item,
                    changes: WorkItemChanges {
                        waiting_until: FieldPatch::Set(until),
                        waiting_reason: FieldPatch::Set(reason),
                        ..Default::default()
                    },
                    operation_id,
                })
            }
            "resume" => WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: item,
                changes: WorkItemChanges {
                    waiting_until: FieldPatch::Clear,
                    waiting_reason: FieldPatch::Clear,
                    ..Default::default()
                },
                operation_id,
            }),
            other => return Err(invalid(format!("action must be place, remove, offload or resume, not {other:?}"))),
        };
        let guard = self.queue_guard(context, by, &project, inside);
        self.apply_scoped(context, &mutation, Some(&context.grant.space_id), guard.as_ref())?;
        self.work_queue_read(context, &json!({ "projectId": project }))
    }

    pub(super) fn work_queue_pause(&self, context: &CallContext<'_>, arguments: &Value) -> Result<Value, AgentToolError> {
        let args: PauseArgs = parse_args(arguments)?;
        let operation_id = context.operation_id(arguments)?;
        let (project, by) = self.queue_scope(context, non_empty(args.project_id))?;
        if by == QueuePausedBy::Manager && non_empty(args.reason.clone()).is_none() {
            return Err(invalid("a Manager must give a reason to pause or resume the queue"));
        }
        let mutation = WorkOutlineMutation::SetQueuePaused(SetQueuePausedParams {
            work_item_id: project.clone(),
            paused: args.paused,
            by: Some(by),
            reason: non_empty(args.reason),
            operation_id,
        });
        let guard = self.queue_guard(context, by, &project, vec![project.clone()]);
        self.apply_scoped(context, &mutation, Some(&context.grant.space_id), guard.as_ref())?;
        self.work_queue_read(context, &json!({ "projectId": project }))
    }
}
