//! `work_outline_admin`: a batch of Work Outline changes anywhere in the
//! Space of an admin chat (a live user grant), with a dry-run preview, a
//! user-approved confirmation for bulk batches, per-action audit entries with
//! inverses, and a per-call grant re-check.
//!
//! Steps are plain JSON objects with an `op`; items created earlier in the
//! batch are addressed as `"$<ref>"` (the step's `ref`). See `STEP_OPS`.

use std::collections::HashMap;

use mast_proto::agent_tools::{self, tools};
use mast_proto::work_outline::{derive_id, roles};
use mast_proto::{
    ADMIN_BULK_DEDUPE_PREFIX, ADMIN_BULK_THRESHOLD,
    ArchiveWorkItemInstructionParams, ArchiveWorkItemParams, AssignChatToWorkItemParams,
    CreateUserActionRequestParams, CreateWorkItemInstructionParams, CreateWorkItemParams,
    DescendantArchivePolicy, FieldPatch, InstructionScope, MoveWorkItemParams,
    RejectWorkItemInstructionProposalParams, RemoveChatFromWorkItemParams,
    ResolveUserActionRequestParams, RestoreWorkItemParams, UpdateWorkItemInstructionParams,
    UpdateWorkItemParams, UserActionRequestOrigin, WorkItemChanges, WorkItemStatus,
    WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
    ApproveWorkItemInstructionProposalParams,
};
use mast_proto::work_outline_container_templates::{
    ApplyContainerTemplateParams, ContainerTemplate, SetContainerTemplateParams,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{AgentToolError, AgentToolErrorCode, AgentToolsService, CallContext, non_empty};

/// Most steps (after `group` expansion) one call may carry.
const MAX_STEPS: usize = 200;
/// Preview lines returned and put in the Required Action detail.
const PREVIEW_LINES: usize = 40;

/// The step vocabulary, shown in tool errors.
pub const STEP_OPS: &str = "create{parentId?,title,description?,status?,ref?} | \
rename{itemId,title} | describe{itemId,description|null} | \
move{itemId,parentId|null,position?:first|last|{before|after:id}} | \
group{title,itemIds[],parentId?,ref?} | status{itemId,status:active|completed} | \
archive{itemId} | restore{itemId} | project{itemId,isProject} | \
destination{itemId,destinationId|null} | convert_to_project{itemId,destinationId?} | \
assign_chat{chatId,itemId|null} | instruction_add{itemId|null,text} | \
instruction_edit{instructionId,text} | instruction_remove{instructionId} | \
proposal_accept{proposalId,text?,allowStale?} | proposal_dismiss{proposalId,reason?} | \
resolve_request{requestId,note?} | \
template_set{itemId,template|null} | template_apply{itemId (a container: all its children; a child: just it)}";

fn invalid(message: impl Into<String>) -> AgentToolError {
    AgentToolError::invalid(message.into())
}

fn forbidden(message: impl Into<String>) -> AgentToolError {
    AgentToolError::refused(AgentToolErrorCode::Forbidden, message)
}

/// One built action: what it does, how to say it, and how to take it back.
struct Built {
    kind: &'static str,
    summary: String,
    mutation: WorkOutlineMutation,
    inverse: Option<WorkOutlineMutation>,
    /// The step's `ref` when it creates an item.
    creates_ref: Option<String>,
}

struct Ctx<'a> {
    snapshot: &'a WorkOutlineSnapshot,
    space_id: &'a str,
    /// ref name → real item id (apply) or the placeholder `$name` (preview).
    refs: HashMap<String, String>,
    dry: bool,
    /// "by admin chat <title> (granted by Mikey)".
    who: String,
}

impl Ctx<'_> {
    fn title(&self, id: &str) -> String {
        match self.snapshot.item(id) {
            Some(item) => format!("\"{}\"", item.title),
            None => match id.strip_prefix('$') {
                Some(name) => format!("new item {name}"),
                None => id.to_owned(),
            },
        }
    }

    /// An item id argument: a real id, or `$ref` resolved through `refs`.
    fn item(&self, step: &Map<String, Value>, key: &str) -> Result<String, AgentToolError> {
        let raw = text(step, key)?;
        self.resolve(&raw)
    }

    fn resolve(&self, raw: &str) -> Result<String, AgentToolError> {
        match raw.strip_prefix('$') {
            Some(name) => self
                .refs
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("unknown ref ${name}: define it with an earlier create/group step"))),
            None => Ok(raw.to_owned()),
        }
    }

    /// An optional nullable parent: absent key is an error for `move`.
    fn parent(&self, step: &Map<String, Value>, key: &str, required: bool) -> Result<Option<String>, AgentToolError> {
        match step.get(key) {
            None if required => Err(invalid(format!("{key} is required (null = the Space root)"))),
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => self.resolve(s.trim()).map(Some),
            Some(_) => Err(invalid(format!("{key} must be an item id or null"))),
        }
    }

    fn placement(&self, step: &Map<String, Value>) -> Result<WorkOutlinePlacement, AgentToolError> {
        Ok(match step.get("position") {
            None | Some(Value::Null) => WorkOutlinePlacement::Last,
            Some(Value::String(s)) if s == "last" => WorkOutlinePlacement::Last,
            Some(Value::String(s)) if s == "first" => WorkOutlinePlacement::First,
            Some(Value::Object(o)) => {
                if let Some(Value::String(id)) = o.get("before") {
                    WorkOutlinePlacement::Before { sibling_work_item_id: self.resolve(id)? }
                } else if let Some(Value::String(id)) = o.get("after") {
                    WorkOutlinePlacement::After { sibling_work_item_id: self.resolve(id)? }
                } else {
                    return Err(invalid("position object needs before or after"));
                }
            }
            Some(_) => return Err(invalid("position must be first, last, {before:id} or {after:id}")),
        })
    }

    /// Where `id` sits now, as a placement that puts it back.
    fn old_placement(&self, id: &str) -> (Option<String>, WorkOutlinePlacement) {
        let Some(item) = self.snapshot.item(id) else {
            return (None, WorkOutlinePlacement::Last);
        };
        let parent = item.parent_work_item_id.clone();
        let siblings: &[String] = match &parent {
            Some(p) => self.snapshot.item(p).map_or(&[], |p| p.child_ids.as_slice()),
            None => self.snapshot.root_work_item_ids.as_slice(),
        };
        let placement = match siblings.iter().position(|s| s == id) {
            Some(0) | None => WorkOutlinePlacement::First,
            Some(i) => WorkOutlinePlacement::After { sibling_work_item_id: siblings[i - 1].clone() },
        };
        (parent, placement)
    }
}

fn text(step: &Map<String, Value>, key: &str) -> Result<String, AgentToolError> {
    match step.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_owned()),
        _ => Err(invalid(format!("{key} is required"))),
    }
}

fn opt_text(step: &Map<String, Value>, key: &str) -> Option<String> {
    step.get(key).and_then(Value::as_str).map(str::to_owned).filter(|s| !s.trim().is_empty())
}

fn status_of(step: &Map<String, Value>) -> Result<WorkItemStatus, AgentToolError> {
    match step.get("status").and_then(Value::as_str) {
        Some("active") => Ok(WorkItemStatus::Active),
        Some("completed") => Ok(WorkItemStatus::Completed),
        other => Err(invalid(format!("status must be active or completed, not {other:?}"))),
    }
}

fn status_name(s: WorkItemStatus) -> &'static str {
    match s {
        WorkItemStatus::Active => "active",
        WorkItemStatus::Completed => "completed",
        WorkItemStatus::Pending => "pending",
    }
}

fn update(item: &str, changes: WorkItemChanges, op: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: item.to_owned(),
        changes,
        operation_id: op.to_owned(),
    })
}

/// Build one step. `op` = the forward operation id; the inverse's id derives
/// from it.
fn build(ctx: &Ctx, step: &Map<String, Value>, op: &str) -> Result<Built, AgentToolError> {
    let kind = step.get("op").and_then(Value::as_str).unwrap_or_default();
    let undo_op = derive_id(op, "admin-undo");
    let snap = ctx.snapshot;
    let made = |kind: &'static str, summary: String, mutation, inverse| Built {
        kind,
        summary,
        mutation,
        inverse,
        creates_ref: None,
    };
    match kind {
        "create" => {
            let parent = ctx.parent(step, "parentId", false)?;
            let title = text(step, "title")?;
            let name = opt_text(step, "ref");
            let id = if ctx.dry { name.as_ref().map(|n| format!("${n}")) } else { None };
            let _ = id;
            let status = match step.get("status").and_then(Value::as_str) {
                Some("active") => Some(WorkItemStatus::Active),
                None | Some("pending") => None,
                Some(o) => return Err(invalid(format!("status must be pending or active, not {o:?}"))),
            };
            let new_id = derive_id(op, roles::ITEM);
            let mutation = WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                space_id: ctx.space_id.to_owned(),
                parent_work_item_id: parent.clone(),
                placement: ctx.placement(step)?,
                title: title.clone(),
                description: opt_text(step, "description"),
                status,
                operation_id: op.to_owned(),
            });
            let under = parent.as_deref().map_or("the Space root".to_owned(), |p| ctx.title(p));
            let mut built = made(
                "create",
                format!("Create \"{title}\" under {under}"),
                mutation,
                Some(WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
                    work_item_id: new_id,
                    descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                    operation_id: undo_op,
                })),
            );
            built.creates_ref = name;
            Ok(built)
        }
        "rename" => {
            let item = ctx.item(step, "itemId")?;
            let title = text(step, "title")?;
            let old = snap.item(&item).map(|i| i.title.clone());
            Ok(made(
                "rename",
                format!("Rename {} to \"{title}\"", ctx.title(&item)),
                update(&item, WorkItemChanges { title: Some(title), ..Default::default() }, op),
                old.map(|t| update(&item, WorkItemChanges { title: Some(t), ..Default::default() }, &undo_op)),
            ))
        }
        "describe" => {
            let item = ctx.item(step, "itemId")?;
            let patch = match opt_text(step, "description") {
                Some(d) => FieldPatch::Set(d),
                None => FieldPatch::Clear,
            };
            let old = snap.item(&item).map(|i| match &i.description {
                Some(d) => FieldPatch::Set(d.clone()),
                None => FieldPatch::Clear,
            });
            Ok(made(
                "describe",
                format!("Change the description of {}", ctx.title(&item)),
                update(&item, WorkItemChanges { description: patch, ..Default::default() }, op),
                old.map(|d| update(&item, WorkItemChanges { description: d, ..Default::default() }, &undo_op)),
            ))
        }
        "move" => {
            let item = ctx.item(step, "itemId")?;
            let parent = ctx.parent(step, "parentId", true)?;
            let (old_parent, old_place) = ctx.old_placement(&item);
            let dest = parent.as_deref().map_or("the Space root".to_owned(), |p| ctx.title(p));
            let existed = snap.item(&item).is_some();
            Ok(made(
                "move",
                format!("Move {} under {dest}", ctx.title(&item)),
                WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
                    work_item_id: item.clone(),
                    destination_parent_work_item_id: parent,
                    placement: ctx.placement(step)?,
                    operation_id: op.to_owned(),
                }),
                existed.then(|| {
                    WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
                        work_item_id: item,
                        destination_parent_work_item_id: old_parent,
                        placement: old_place,
                        operation_id: undo_op,
                    })
                }),
            ))
        }
        "status" => {
            let item = ctx.item(step, "itemId")?;
            let status = status_of(step)?;
            let old = snap.item(&item).map(|i| i.status);
            Ok(made(
                "status",
                format!("Mark {} {}", ctx.title(&item), status_name(status)),
                update(&item, WorkItemChanges { status: Some(status), ..Default::default() }, op),
                old.map(|s| update(&item, WorkItemChanges { status: Some(s), ..Default::default() }, &undo_op)),
            ))
        }
        "archive" | "restore" => {
            let item = ctx.item(step, "itemId")?;
            let archive = kind == "archive";
            let archive_m = |op: &str| {
                WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
                    work_item_id: item.clone(),
                    descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                    operation_id: op.to_owned(),
                })
            };
            let restore_m = |op: &str| {
                WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
                    work_item_id: item.clone(),
                    operation_id: op.to_owned(),
                })
            };
            Ok(if archive {
                made("archive", format!("Archive {}", ctx.title(&item)), archive_m(op), Some(restore_m(&undo_op)))
            } else {
                made("restore", format!("Restore {}", ctx.title(&item)), restore_m(op), Some(archive_m(&undo_op)))
            })
        }
        "project" | "convert_to_project" => {
            let item = ctx.item(step, "itemId")?;
            let on = if kind == "project" {
                step.get("isProject").and_then(Value::as_bool).ok_or_else(|| invalid("isProject (bool) is required"))?
            } else {
                true
            };
            let old = snap.item(&item).map(|i| i.is_project);
            let mut changes = WorkItemChanges { is_project: Some(on), ..Default::default() };
            let mut summary = format!(
                "{} {}",
                if on { "Make project:" } else { "Stop being a project:" },
                ctx.title(&item)
            );
            if kind == "convert_to_project"
                && let Some(dest) = opt_text(step, "destinationId")
            {
                let dest = ctx.resolve(&dest)?;
                summary.push_str(&format!(" with worker destination {}", ctx.title(&dest)));
                changes.worker_destination = FieldPatch::Set(dest);
            }
            Ok(made(
                if kind == "project" { "project" } else { "convert to project" },
                summary,
                update(&item, changes, op),
                old.map(|o| update(&item, WorkItemChanges { is_project: Some(o), ..Default::default() }, &undo_op)),
            ))
        }
        "destination" => {
            let item = ctx.item(step, "itemId")?;
            let dest = ctx.parent(step, "destinationId", true)?;
            let old = snap.item(&item).and_then(|i| i.worker_destination_id.clone());
            let patch = |d: Option<String>| match d {
                Some(d) => FieldPatch::Set(d),
                None => FieldPatch::Clear,
            };
            Ok(made(
                "worker destination",
                format!(
                    "Set worker destination of {} to {}",
                    ctx.title(&item),
                    dest.as_deref().map_or("none".to_owned(), |d| ctx.title(d))
                ),
                update(&item, WorkItemChanges { worker_destination: patch(dest), ..Default::default() }, op),
                Some(update(&item, WorkItemChanges { worker_destination: patch(old), ..Default::default() }, &undo_op)),
            ))
        }
        "assign_chat" => {
            let chat = text(step, "chatId")?;
            let item = ctx.parent(step, "itemId", true)?;
            let old = snap
                .assignments
                .iter()
                .find(|a| a.chat_id == chat && a.state == mast_proto::WorkItemChatAssignmentState::Associated)
                .and_then(|a| a.work_item_id.clone());
            let assign = |item: Option<String>, op: &str| match item {
                Some(work_item_id) => WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
                    chat_id: chat.clone(),
                    work_item_id,
                    operation_id: op.to_owned(),
                }),
                None => WorkOutlineMutation::RemoveChatFromWorkItem(RemoveChatFromWorkItemParams {
                    chat_id: chat.clone(),
                    operation_id: op.to_owned(),
                }),
            };
            Ok(made(
                "move chat",
                format!(
                    "Move chat {chat} to {}",
                    item.as_deref().map_or("Unassociated".to_owned(), |i| ctx.title(i))
                ),
                assign(item, op),
                Some(assign(old, &undo_op)),
            ))
        }
        "instruction_add" => {
            let item = ctx.parent(step, "itemId", true)?;
            let body = text(step, "text")?;
            let scope = match &item {
                Some(work_item_id) => InstructionScope::WorkItem { work_item_id: work_item_id.clone() },
                None => InstructionScope::SpaceRoot,
            };
            Ok(made(
                "instruction add",
                format!(
                    "Add instruction to {}: {}",
                    item.as_deref().map_or("the Space root".to_owned(), |i| ctx.title(i)),
                    clip(&body)
                ),
                WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                    space_id: ctx.space_id.to_owned(),
                    scope,
                    text: body,
                    placement: None,
                    source: None,
                    operation_id: op.to_owned(),
                }),
                Some(WorkOutlineMutation::ArchiveWorkItemInstruction(ArchiveWorkItemInstructionParams {
                    instruction_id: derive_id(op, roles::INSTRUCTION),
                    operation_id: undo_op,
                })),
            ))
        }
        "instruction_edit" => {
            let id = text(step, "instructionId")?;
            let body = text(step, "text")?;
            let old = snap.instructions.iter().find(|i| i.id == id).map(|i| i.text.clone());
            Ok(made(
                "instruction edit",
                format!("Edit instruction {id}: {}", clip(&body)),
                WorkOutlineMutation::UpdateWorkItemInstruction(UpdateWorkItemInstructionParams {
                    instruction_id: id.clone(),
                    text: body,
                    operation_id: op.to_owned(),
                }),
                old.map(|t| {
                    WorkOutlineMutation::UpdateWorkItemInstruction(UpdateWorkItemInstructionParams {
                        instruction_id: id,
                        text: t,
                        operation_id: undo_op,
                    })
                }),
            ))
        }
        "instruction_remove" => {
            let id = text(step, "instructionId")?;
            let old = snap.instructions.iter().find(|i| i.id == id).cloned();
            let inverse = old.map(|i| {
                WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                    space_id: ctx.space_id.to_owned(),
                    scope: match i.work_item_id {
                        Some(work_item_id) => InstructionScope::WorkItem { work_item_id },
                        None => InstructionScope::SpaceRoot,
                    },
                    text: i.text,
                    placement: None,
                    source: None,
                    operation_id: undo_op.clone(),
                })
            });
            Ok(made(
                "instruction remove",
                format!("Remove instruction {id}"),
                WorkOutlineMutation::ArchiveWorkItemInstruction(ArchiveWorkItemInstructionParams {
                    instruction_id: id,
                    operation_id: op.to_owned(),
                }),
                inverse,
            ))
        }
        "proposal_accept" => {
            let id = text(step, "proposalId")?;
            let approved = opt_text(step, "text")
                .or_else(|| snap.instruction_proposals.iter().find(|p| p.id == id).map(|p| p.text.clone()))
                .unwrap_or_default();
            Ok(made(
                "accept proposal",
                format!("Accept instruction proposal {id}: {}", clip(&approved)),
                WorkOutlineMutation::ApproveWorkItemInstructionProposal(
                    ApproveWorkItemInstructionProposalParams {
                        proposal_id: id,
                        approved_text: approved,
                        placement: None,
                        allow_stale: step.get("allowStale").and_then(Value::as_bool).unwrap_or(false),
                        operation_id: op.to_owned(),
                    },
                ),
                None,
            ))
        }
        "proposal_dismiss" => {
            let id = text(step, "proposalId")?;
            Ok(made(
                "dismiss proposal",
                format!("Dismiss instruction proposal {id}"),
                WorkOutlineMutation::RejectWorkItemInstructionProposal(
                    RejectWorkItemInstructionProposalParams {
                        proposal_id: id,
                        reason: opt_text(step, "reason"),
                        operation_id: op.to_owned(),
                    },
                ),
                None,
            ))
        }
        "template_set" => {
            let item = ctx.item(step, "itemId")?;
            let template = match step.get("template") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let t: ContainerTemplate = serde_json::from_value(v.clone())
                        .map_err(|e| invalid(format!("template is not a valid ContainerTemplate: {e}")))?;
                    Some(t)
                }
            };
            let old = snap.item(&item).map(|i| i.container_template.clone());
            let set = |template: Option<ContainerTemplate>, op: &str| {
                WorkOutlineMutation::SetContainerTemplate(SetContainerTemplateParams {
                    operation_id: op.to_owned(),
                    space_id: ctx.space_id.to_owned(),
                    work_item_id: item.clone(),
                    template,
                })
            };
            let summary = match &template {
                Some(t) => format!(
                    "Set the container template of {} ({} rule nodes)",
                    ctx.title(&item),
                    t.root.node_count()
                ),
                None => format!("Clear the container template of {}", ctx.title(&item)),
            };
            Ok(made(
                "container template",
                summary,
                set(template, op),
                old.map(|o| set(o, &undo_op)),
            ))
        }
        "template_apply" => {
            let item = ctx.item(step, "itemId")?;
            let kids = snap.item(&item).filter(|i| i.container_template.is_some()).map(|i| i.child_ids.len());
            let summary = match kids {
                Some(n) => format!("Apply the template of {} to its {n} children", ctx.title(&item)),
                None => format!("Make {} follow its parent's template", ctx.title(&item)),
            };
            Ok(made(
                "apply template",
                summary,
                WorkOutlineMutation::ApplyContainerTemplate(ApplyContainerTemplateParams {
                    operation_id: op.to_owned(),
                    space_id: ctx.space_id.to_owned(),
                    work_item_id: item,
                }),
                None,
            ))
        }
        "resolve_request" => {
            let id = text(step, "requestId")?;
            let about = snap
                .user_action_requests
                .iter()
                .find(|r| r.id == id)
                .map_or(id.clone(), |r| format!("\"{}\"", clip(&r.text)));
            Ok(made(
                "resolve request",
                format!("Resolve Required Action {about}"),
                WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
                    outcome: None,
                    values: None,
                    via: None,
                    user_action_request_id: id,
                    note: Some(match opt_text(step, "note") {
                        Some(n) => format!("{n} ({})", ctx.who),
                        None => format!("Resolved {}", ctx.who),
                    }),
                    answer: None,
                    operation_id: op.to_owned(),
                }),
                None,
            ))
        }
        other => Err(invalid(format!("unknown op {other:?}; steps are: {STEP_OPS}"))),
    }
}

fn clip(s: &str) -> String {
    let s = s.trim().replace('\n', " ");
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

/// Expand `group` into a create plus one move per item, so each is audited,
/// counted and undone on its own.
fn expand(steps: &[Value], snapshot: &WorkOutlineSnapshot) -> Result<Vec<Map<String, Value>>, AgentToolError> {
    let mut out = Vec::new();
    for (i, raw) in steps.iter().enumerate() {
        let Some(step) = raw.as_object() else {
            return Err(invalid(format!("step {i} is not an object")));
        };
        if step.get("op").and_then(Value::as_str) != Some("group") {
            out.push(step.clone());
            continue;
        }
        let ids: Vec<String> = step
            .get("itemIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(invalid(format!("step {i}: group needs itemIds")));
        }
        let name = opt_text(step, "ref").unwrap_or_else(|| format!("group{i}"));
        let parent = match step.get("parentId") {
            Some(v) => v.clone(),
            None => snapshot
                .item(&ids[0])
                .and_then(|item| item.parent_work_item_id.clone())
                .map_or(Value::Null, Value::String),
        };
        let mut create = Map::new();
        create.insert("op".into(), json!("create"));
        create.insert("title".into(), step.get("title").cloned().unwrap_or(Value::Null));
        create.insert("parentId".into(), parent);
        create.insert("ref".into(), json!(name));
        out.push(create);
        for id in ids {
            let mut mv = Map::new();
            mv.insert("op".into(), json!("move"));
            mv.insert("itemId".into(), json!(id));
            mv.insert("parentId".into(), json!(format!("${name}")));
            out.push(mv);
        }
    }
    Ok(out)
}

impl AgentToolsService {
    /// `work_outline_admin`.
    pub(super) fn admin(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let Some(space) = context.admin_space.clone() else {
            return Err(forbidden(
                "This chat is not an admin chat (or its grant expired or was revoked). Only the \
                 user can grant admin, from the chat's menu: \"Grant admin for this Space…\"",
            ));
        };
        let admin = self.tokens.admin();
        let snapshot = self.work_outline.snapshot(&space);

        // Read-only view of the whole Space.
        if arguments.get("read").is_some() {
            let depth = arguments
                .get("maxDepth")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 8) as usize;
            fn walk(s: &WorkOutlineSnapshot, id: &str, depth: usize, count: &mut usize) -> Value {
                let Some(item) = s.item(id) else { return Value::Null };
                *count += 1;
                let children: Vec<Value> = if depth > 1 && *count < 400 {
                    item.child_ids.iter().map(|c| walk(s, c, depth - 1, count)).collect()
                } else {
                    Vec::new()
                };
                json!({
                    "id": item.id, "title": item.title, "status": item.status,
                    "archived": item.archived_at.is_some(), "isProject": item.is_project,
                    "childCount": item.child_ids.len(), "children": children,
                })
            }
            let mut count = 0usize;
            let roots: Vec<Value> = snapshot
                .root_work_item_ids
                .iter()
                .map(|id| walk(&snapshot, id, depth, &mut count))
                .collect();
            return Ok(json!({
                "spaceId": space,
                "roots": roots,
                "openRequests": snapshot.user_action_requests.iter().filter(|r| r.is_open() && !r.deduped)
                    .take(100).map(|r| json!({"id": r.id, "workItemId": r.work_item_id, "text": r.text, "chatId": r.chat_id}))
                    .collect::<Vec<_>>(),
                "proposals": snapshot.instruction_proposals.iter().filter(|p| p.status == mast_proto::InstructionProposalStatus::Pending)
                    .take(100).map(|p| json!({"id": p.id, "text": p.text, "action": p.action}))
                    .collect::<Vec<_>>(),
            }));
        }

        let steps = arguments
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("steps (array) is required; each step is one of: {STEP_OPS}")))?;
        let actions = expand(steps, &snapshot)?;
        if actions.is_empty() {
            return Err(invalid("steps is empty"));
        }
        if actions.len() > MAX_STEPS {
            return Err(invalid(format!("at most {MAX_STEPS} steps per call (got {})", actions.len())));
        }
        let key = non_empty(arguments.get("idempotencyKey").and_then(Value::as_str).map(str::to_owned))
            .ok_or_else(|| invalid("idempotencyKey is required for work_outline_admin"))?;
        let steps_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&actions).unwrap_or_default()));
        let op_for = |i: usize| agent_tools::operation_id(&context.grant.chat_id, &format!("{key}#{i}"));

        // Preview: build every step against placeholders.
        let mut refs: HashMap<String, String> = HashMap::new();
        let mut preview = Vec::new();
        for (i, step) in actions.iter().enumerate() {
            let ctx = Ctx { snapshot: &snapshot, space_id: &space, refs: refs.clone(), dry: true, who: crate::admin::attribution(&context.chat_title) };
            let built = build(&ctx, step, &op_for(i)).map_err(|e| step_error(i, e))?;
            if let Some(name) = built.creates_ref {
                refs.insert(name.clone(), format!("${name}"));
            }
            preview.push(built.summary);
        }
        let dry_run = arguments.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
        let grant = admin.live(&context.grant.chat_id);
        let skip_bulk = grant.as_ref().is_some_and(|g| g.skip_bulk_confirmation);
        // A container apply touches each of its children.
        let weight: usize = actions
            .iter()
            .map(|a| {
                if a.get("op").and_then(Value::as_str) != Some("template_apply") {
                    return 1;
                }
                a.get("itemId")
                    .and_then(Value::as_str)
                    .and_then(|id| snapshot.item(id))
                    .filter(|i| i.container_template.is_some())
                    .map_or(1, |i| i.child_ids.len().max(1))
            })
            .sum();
        let bulk = weight > ADMIN_BULK_THRESHOLD && !skip_bulk;
        let shown: Vec<&String> = preview.iter().take(PREVIEW_LINES).collect();

        let confirm = non_empty(arguments.get("confirmToken").and_then(Value::as_str).map(str::to_owned));
        if dry_run && confirm.is_none() && !bulk {
            return Ok(json!({ "dryRun": true, "count": actions.len(), "preview": shown, "bulk": false }));
        }
        if bulk {
            match confirm {
                None => return self.open_bulk_approval(context, &steps_hash, weight, &preview),
                Some(token) => {
                    let request_id = admin
                        .check_approval(&token, &context.grant.chat_id, &steps_hash)
                        .map_err(|e| forbidden(e))?;
                    let approved = snapshot.user_action_requests.iter().find(|r| r.id == request_id).is_some_and(|r| {
                        !r.is_open()
                            && r.resolution
                                .as_ref()
                                .is_some_and(|res| res.by.kind == mast_proto::WorkOutlineActorKind::User && res.outcome.as_deref() == Some("approved"))
                    });
                    if !approved {
                        return Err(forbidden(
                            "The user has not approved this bulk change yet: the Required Action \
                             \"Approve bulk change\" must be approved by the user themself (an \
                             agent's resolve does not count). Ask them, then retry with the same \
                             confirmToken.",
                        ));
                    }
                    if dry_run {
                        return Ok(json!({ "dryRun": true, "approved": true, "count": actions.len(), "preview": shown }));
                    }
                    admin.use_approval(&token);
                }
            }
        }

        // Apply, re-checking the grant before every step so a revoke stops
        // the sweep at once.
        let turn_id = context.grant.run_id.clone();
        let mut refs: HashMap<String, String> = HashMap::new();
        let mut results = Vec::new();
        for (i, step) in actions.iter().enumerate() {
            if admin.live(&context.grant.chat_id).is_none_or(|g| g.space_id != space) {
                return Err(forbidden(format!(
                    "admin was revoked or expired after {i} of {} steps; those are in the audit log",
                    actions.len()
                )));
            }
            let ctx = Ctx { snapshot: &snapshot, space_id: &space, refs: refs.clone(), dry: false, who: crate::admin::attribution(&context.chat_title) };
            let built = build(&ctx, step, &op_for(i)).map_err(|e| step_error(i, e))?;
            let reply = self.apply(context, &built.mutation).map_err(|e| {
                invalid(format!(
                    "step {i} ({}) failed after {i} applied: {}",
                    built.summary,
                    e.to_wire_message()
                ))
            })?;
            if let (Some(name), Some(id)) = (built.creates_ref.clone(), reply.work_item_id.clone()) {
                refs.insert(name, id);
            }
            admin.record_action(
                &turn_id,
                &context.grant.chat_id,
                &context.chat_title,
                &space,
                built.kind,
                built.summary.clone(),
                built.inverse,
            );
            results.push(json!({ "summary": built.summary, "workItemId": reply.work_item_id }));
        }
        Ok(json!({
            "applied": results.len(),
            "results": results,
            "turnId": turn_id,
            "attribution": crate::admin::attribution(&context.chat_title),
            "note": "Recorded in the outline's Recent Changes; the user can undo this whole turn.",
        }))
    }

    fn open_bulk_approval(
        &self,
        context: &CallContext,
        steps_hash: &str,
        count: usize,
        preview: &[String],
    ) -> Result<Value, AgentToolError> {
        let item = context.require_item("requesting approval for a bulk change")?;
        let mut detail = preview
            .iter()
            .take(PREVIEW_LINES)
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        if preview.len() > PREVIEW_LINES {
            detail.push_str(&format!("\n… and {} more", preview.len() - PREVIEW_LINES));
        }
        let dedupe = format!("{ADMIN_BULK_DEDUPE_PREFIX}{}", &steps_hash[..16]);
        let op = agent_tools::operation_id(&context.grant.chat_id, &format!("bulk-approval:{}", &steps_hash[..24]));
        let request_text = format!(
            "Approve bulk change: {count} outline changes by admin chat {}",
            context.chat_title
        );
        let reply = self.apply(
            context,
            &WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
            links: Vec::new(),
            facts: Vec::new(),
            kind: Some(mast_proto::work_outline_user_actions::UserActionKind::Approve),
            priority: None,
                work_item_id: item.clone(),
                origin: UserActionRequestOrigin {
                    kind: Default::default(),
                    chat_id: context.grant.chat_id.clone(),
                    message_id: None,
                    tool_call_id: None,
                },
                text: request_text,
                info: Some(detail),
                dedupe_key: Some(dedupe),
                operation_id: op,
            }),
        )?;
        let request_id = reply
            .user_action_request_id
            .ok_or_else(|| invalid("the approval request was not created"))?;
        let token = self.tokens.admin().open_approval(&context.grant.chat_id, steps_hash, &request_id);
        Ok(json!({
            "needsApproval": true,
            "count": count,
            "preview": preview.iter().take(PREVIEW_LINES).collect::<Vec<_>>(),
            "confirmToken": token,
            "requestId": request_id,
            "note": "Nothing was changed. A Required Action \"Approve bulk change\" now waits on \
                     the user. Tell them in chat, then wait. When they approve it themself, call \
                     work_outline_admin again with the SAME steps and this confirmToken (valid \
                     30 minutes, one use).",
        }))
    }

    /// `AdminUndoTurn`: apply the stored inverses of one admin turn, newest
    /// first, as the local user. Steps whose target changed are skipped.
    pub fn admin_undo_turn(&self, turn_id: &str) -> Result<mast_proto::AdminUndoTurnReply, AgentToolError> {
        let steps = self
            .tokens
            .admin()
            .take_turn_for_undo(turn_id)
            .ok_or_else(|| invalid("unknown or already undone admin turn"))?;
        let mut reply = mast_proto::AdminUndoTurnReply::default();
        for (summary, inverse) in steps {
            match inverse {
                None => reply.skipped.push(format!("{summary} (cannot be undone)")),
                Some(m) => match self.work_outline.mutate(&m) {
                    Ok(_) => reply.undone += 1,
                    Err(e) => reply.skipped.push(format!("{summary} ({})", e.to_wire_message())),
                },
            }
        }
        Ok(reply)
    }
}

fn step_error(i: usize, e: AgentToolError) -> AgentToolError {
    invalid(format!("step {i}: {}", e.to_wire_message()))
}

// Silence the aliased import used only to keep the grouped `use` tidy.
#[allow(unused)]
const _: () = {
    let _ = tools::ADMIN;
};
