//! Container templates (contract amendment "container templates"): rules
//! stored on a container item (`workItems.containerTemplate`) that every
//! DIRECT child follows.
//!
//! * A new child of a templated container gets the whole structure in the same
//!   batch as its creation (ids derived from the child id and the rule node id,
//!   so two devices converge instead of duplicating).
//! * `setContainerTemplate` saves new rules and propagates to every following
//!   child: added nodes and instructions are created, titles / project flags /
//!   instruction text follow, removed nodes and instructions are detached or
//!   archived. Nothing is ever deleted.
//! * `applyContainerTemplate` adopts existing items by title (level by level)
//!   and creates the rest.
//!
//! Followed items carry `templateNodeId`; followed instruction rows carry
//! `templateInstructionId`. A detached item simply has them cleared.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mast_proto::work_outline::{
    WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError, WorkOutlineErrorCode, derive_id,
    limits,
};
use mast_proto::work_outline_container_templates::{
    ApplyContainerTemplateParams, ContainerTemplate, RuleNode, SetContainerTemplateParams,
    container_template_limits,
};
use mast_proto::work_outline_instructions::WorkItemInstructionView;
use serde_json::{Value, json};

use super::instructions::scope_siblings;
use super::materialize::{DecodedItem, decode_item};
use super::{
    KIND_WORK_ITEMS, Outcome, Planned, WORK_OUTLINE_ROW_VERSION, actor_row_value,
    check_operation_id, err, invalid, place,
};
use crate::registry::{RegistryDoc, RowState};

const KIND_INSTRUCTIONS: &str = super::KIND_WORK_ITEM_INSTRUCTIONS;

/// Whether a plan step may overwrite what a following item already has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Create missing structure and adopt unmarked items by title; never
    /// overwrite a title or an instruction text.
    Adopt,
}

/// A snapshot of one Space's items and instructions for planning.
struct World {
    items: BTreeMap<String, DecodedItem>,
    children: HashMap<String, Vec<String>>,
    instructions: Vec<WorkItemInstructionView>,
}

impl World {
    fn live_children(&self, id: &str) -> Vec<&DecodedItem> {
        self.children
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|c| self.items.get(c))
            .filter(|c| !c.archived())
            .collect()
    }

    /// Items at or below `root` carrying a `templateNodeId`.
    fn followed_in(&self, root: &str) -> Vec<&DecodedItem> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_owned()];
        while let Some(id) = stack.pop() {
            if let Some(item) = self.items.get(&id)
                && item.template_node_id.is_some()
            {
                out.push(item);
            }
            if let Some(children) = self.children.get(&id) {
                stack.extend(children.iter().cloned());
            }
        }
        out
    }
}

/// Where a rule node ended up.
type NodeItems = BTreeMap<String, String>;

struct Writer<'a> {
    doc: &'a RegistryDoc,
    world: &'a World,
    space_id: &'a str,
    actor: &'a WorkOutlineActor,
    now_ms: i64,
    plan: Vec<Planned>,
    /// Extra fields to merge into an existing plan entry / update.
    patches: BTreeMap<String, BTreeMap<String, Value>>,
    /// Item ids created by this plan (rows in `plan`, not in `world`).
    created: BTreeSet<String>,
}

impl RegistryDoc {
    fn container_world(&self, space_id: &str) -> World {
        let rows = self.overlay_rows(KIND_WORK_ITEMS);
        let tree = super::materialize::build_tree(space_id, &rows);
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for id in tree.items.keys() {
            children.insert(id.clone(), tree.children_of(Some(id)).to_vec());
        }
        World {
            items: tree.items,
            children,
            instructions: self.instructions_of_space(space_id),
        }
    }

    /// Container-template rules of `container`, when set.
    fn rules_of(&self, container: &str) -> Option<ContainerTemplate> {
        let row = self.overlay_row(KIND_WORK_ITEMS, container)?;
        decode_item(&row).ok()?.container_template
    }

    /// Append, to the plan of a freshly planned child creation, the structure
    /// of its parent's container template (no-op without rules). `plan`
    /// already holds the child's upsert.
    pub(super) fn plan_follow_for_new_child(
        &self,
        space_id: &str,
        parent: Option<&str>,
        child_id: &str,
        actor: &WorkOutlineActor,
        now_ms: i64,
        plan: &mut Vec<Planned>,
    ) -> Outcome<()> {
        let Some(parent) = parent else {
            return Ok(());
        };
        let Some(rules) = self.rules_of(parent) else {
            return Ok(());
        };
        let world = self.container_world(space_id);
        let mut w = Writer {
            doc: self,
            world: &world,
            space_id,
            actor,
            now_ms,
            plan: Vec::new(),
            patches: BTreeMap::new(),
            created: BTreeSet::from([child_id.to_owned()]),
        };
        w.follow(child_id, &rules, false, Mode::Adopt)?;
        let (extra, patches) = (w.plan, w.patches);
        // The child's own patch merges into its upsert; the rest is appended.
        for planned in plan.iter_mut() {
            if planned.id == child_id
                && let (Some(set), Some(patch)) = (planned.set.as_mut(), patches.get(child_id))
            {
                set.extend(patch.clone());
            }
        }
        plan.extend(extra);
        for (id, patch) in patches {
            if id != child_id && !plan.iter().any(|p| p.kind == KIND_WORK_ITEMS && p.id == id) {
                plan.push(Planned::update(KIND_WORK_ITEMS, &id, patch));
            }
        }
        Ok(())
    }

    /// `setContainerTemplate`.
    pub fn set_container_template(
        &mut self,
        p: &SetContainerTemplateParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        if actor.kind == WorkOutlineActorKind::Agent {
            return Err(WorkOutlineError::unauthorized(
                "Only you can change a container's template",
            ));
        }
        self.require_space(&p.space_id)?;
        let container = self.live_item(&p.work_item_id)?;
        if container.space_id != p.space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The work item belongs to another Space",
            ));
        }
        if container.archived() {
            return Err(err(WorkOutlineErrorCode::Archived, "The work item is archived"));
        }
        if container.template_node_id.is_some() {
            return Err(invalid(
                "An item that follows a template cannot hold a template (nesting comes later)",
            ));
        }
        let old = container.container_template.clone();
        let new = match &p.template {
            None => None,
            Some(template) => {
                let mut root = template.root.clone();
                root.assign_ids(&p.operation_id);
                root.check()
                    .map_err(|m| err(WorkOutlineErrorCode::LimitExceeded, m))?;
                let ids: BTreeSet<&str> =
                    root.preorder().iter().map(|(_, n)| n.id.as_str()).collect();
                if ids.len() != root.node_count() {
                    return Err(invalid("Template item ids must be unique"));
                }
                for (_, node) in root.preorder() {
                    let mut total = 0;
                    for ins in &node.instructions {
                        super::instructions::check_instruction_text(&ins.text)?;
                        total += ins.text.chars().count();
                    }
                    if total > limits::MAX_EFFECTIVE_INSTRUCTION_CHARS / 2 {
                        return Err(err(
                            WorkOutlineErrorCode::LimitExceeded,
                            "An item's template instructions are too long",
                        ));
                    }
                }
                let revision = old.as_ref().map_or(1, |o| o.revision.saturating_add(1));
                Some(ContainerTemplate { revision, root })
            }
        };
        if old == new {
            return Ok(());
        }
        if let Some(new) = &new {
            let units = serde_json::to_string(new).map_err(|e| invalid(e.to_string()))?;
            if units.encode_utf16().count() > container_template_limits::MAX_RULES_UTF16_UNITS {
                return Err(err(
                    WorkOutlineErrorCode::LimitExceeded,
                    "The template is too large",
                ));
            }
        }
        let world = self.container_world(&p.space_id);
        let mut w = Writer {
            doc: self,
            world: &world,
            space_id: &p.space_id,
            actor,
            now_ms,
            plan: Vec::new(),
            patches: BTreeMap::new(),
            created: BTreeSet::new(),
        };
        let followers: Vec<String> = world
            .live_children(&p.work_item_id)
            .into_iter()
            .filter(|c| {
                old.as_ref()
                    .is_some_and(|o| c.template_node_id.as_deref() == Some(o.root.id.as_str()))
            })
            .map(|c| c.id.clone())
            .collect();
        for child in &followers {
            w.propagate(child, old.as_ref(), new.as_ref())?;
        }
        let (mut plan, patches) = (w.plan, w.patches);
        for (id, patch) in patches {
            let kind = if patch.contains_key("__instruction") {
                KIND_INSTRUCTIONS
            } else {
                KIND_WORK_ITEMS
            };
            let mut patch = patch;
            patch.remove("__instruction");
            plan.push(Planned::update(kind, &id, patch));
        }
        plan.push(Planned::update(
            KIND_WORK_ITEMS,
            &p.work_item_id,
            BTreeMap::from([
                (
                    "containerTemplate".to_owned(),
                    new.as_ref()
                        .map_or(Value::Null, |n| serde_json::to_value(n).unwrap_or(Value::Null)),
                ),
                ("updatedAt".to_owned(), json!(now_ms)),
            ]),
        ));
        self.commit(plan, &[(KIND_WORK_ITEMS, p.work_item_id.as_str())])
    }

    /// `applyContainerTemplate`.
    pub fn apply_container_template(
        &mut self,
        p: &ApplyContainerTemplateParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        let target = self.live_item(&p.work_item_id)?;
        if target.space_id != p.space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The work item belongs to another Space",
            ));
        }
        let world = self.container_world(&p.space_id);
        let (rules, children): (ContainerTemplate, Vec<String>) =
            if let Some(rules) = target.container_template.clone() {
                let kids = world
                    .live_children(&p.work_item_id)
                    .into_iter()
                    .map(|c| c.id.clone())
                    .collect();
                (rules, kids)
            } else {
                let parent = target
                    .raw_parent
                    .clone()
                    .ok_or_else(|| invalid("This item has no template to follow"))?;
                let rules = self
                    .rules_of(&parent)
                    .ok_or_else(|| invalid("The parent has no template"))?;
                (rules, vec![p.work_item_id.clone()])
            };
        if target.archived() {
            return Err(err(WorkOutlineErrorCode::Archived, "The work item is archived"));
        }
        let mut w = Writer {
            doc: self,
            world: &world,
            space_id: &p.space_id,
            actor,
            now_ms,
            plan: Vec::new(),
            patches: BTreeMap::new(),
            created: BTreeSet::new(),
        };
        for child in &children {
            w.follow(child, &rules, true, Mode::Adopt)?;
        }
        let (mut plan, patches) = (w.plan, w.patches);
        for (id, patch) in patches {
            plan.push(Planned::update(KIND_WORK_ITEMS, &id, patch));
        }
        self.commit(plan, &[])
    }
}

impl Writer<'_> {
    fn patch(&mut self, id: &str, field: &str, value: Value) {
        self.patches
            .entry(id.to_owned())
            .or_default()
            .insert(field.to_owned(), value);
    }

    /// Make `child` follow `rules`: mark, adopt/create items and instruction
    /// rows, flags, worker destination.
    fn follow(
        &mut self,
        child: &str,
        rules: &ContainerTemplate,
        child_exists: bool,
        mode: Mode,
    ) -> Outcome<()> {
        let mut nodes = NodeItems::new();
        nodes.insert(rules.root.id.clone(), child.to_owned());
        self.patch(child, "templateNodeId", json!(rules.root.id));
        self.patch(child, "updatedAt", json!(self.now_ms));
        self.fill(child, &rules.root, child_exists, &mut nodes, mode)?;
        // Worker destinations: each project node's destination node.
        for (_, node) in rules.root.preorder() {
            if !node.is_project {
                continue;
            }
            let Some(project) = nodes.get(&node.id).cloned() else {
                continue;
            };
            if let Some(dest) = destination_of(node).and_then(|d| nodes.get(&d.id)).cloned() {
                let has = self
                    .world
                    .items
                    .get(&project)
                    .and_then(|i| i.worker_destination_id.clone())
                    .and_then(|id| self.world.items.get(&id))
                    .is_some_and(|d| !d.archived());
                if !has {
                    self.patch(&project, "workerDestinationId", json!(dest));
                }
            }
        }
        Ok(())
    }

    /// One node: flags, instructions, then its child nodes.
    fn fill(
        &mut self,
        item_id: &str,
        node: &RuleNode,
        exists: bool,
        nodes: &mut NodeItems,
        mode: Mode,
    ) -> Outcome<()> {
        if node.is_project && !self.world.items.get(item_id).is_some_and(|i| i.is_project) {
            self.patch(item_id, "isProject", json!(true));
        }
        self.plan_instructions(item_id, node, exists)?;
        // Child nodes.
        let existing: Vec<&DecodedItem> = if exists {
            self.world.live_children(item_id)
        } else {
            Vec::new()
        };
        let mut claimed: BTreeSet<String> = BTreeSet::new();
        let mut to_create: Vec<(&RuleNode, String)> = Vec::new();
        for child in &node.children {
            let derived = derive_id(item_id, &child.id);
            let found = existing
                .iter()
                .find(|c| c.template_node_id.as_deref() == Some(child.id.as_str()))
                .map(|c| c.id.clone())
                .or_else(|| {
                    existing
                        .iter()
                        .find(|c| {
                            c.template_node_id.is_none()
                                && !claimed.contains(&c.id)
                                && c.title
                                    .as_deref()
                                    .is_some_and(|t| t.trim().eq_ignore_ascii_case(child.title.trim()))
                        })
                        .map(|c| c.id.clone())
                });
            if let Some(id) = found {
                claimed.insert(id.clone());
                if self
                    .world
                    .items
                    .get(&id)
                    .is_none_or(|i| i.template_node_id.as_deref() != Some(child.id.as_str()))
                {
                    self.patch(&id, "templateNodeId", json!(child.id));
                }
                nodes.insert(child.id.clone(), id.clone());
                self.fill(&id, child, true, nodes, mode)?;
                continue;
            }
            // Re-follow a detached derived item that is still in place.
            match self.doc.row_state(KIND_WORK_ITEMS, &derived) {
                RowState::Live => {
                    let in_place = self.world.items.get(&derived).is_some_and(|i| {
                        i.raw_parent.as_deref() == Some(item_id) && !i.archived()
                    });
                    if in_place {
                        self.patch(&derived, "templateNodeId", json!(child.id));
                        nodes.insert(child.id.clone(), derived.clone());
                        self.fill(&derived, child, true, nodes, mode)?;
                    }
                }
                RowState::Tombstoned => {}
                RowState::Absent => to_create.push((child, derived)),
            }
        }
        if to_create.is_empty() {
            return Ok(());
        }
        // Keys after the parent's existing children.
        let siblings: Vec<(String, Option<String>)> = existing
            .iter()
            .map(|c| (c.id.clone(), c.valid_key().map(str::to_owned)))
            .collect();
        let placed = place(&siblings, siblings.len(), to_create.len())?;
        for (id, key) in &placed.rekeys {
            self.patch(id, "orderKey", json!(key));
        }
        for (ix, (child, derived)) in to_create.into_iter().enumerate() {
            let mut set = BTreeMap::from([
                ("parentWorkItemId".to_owned(), json!(item_id)),
                ("orderKey".to_owned(), json!(placed.keys[ix])),
                ("spaceId".to_owned(), json!(self.space_id)),
                ("title".to_owned(), json!(child.title.trim())),
                ("status".to_owned(), json!("pending")),
                ("inheritParentInstructions".to_owned(), json!(true)),
                ("createdBy".to_owned(), actor_row_value(self.actor)),
                ("createdAt".to_owned(), json!(self.now_ms)),
                ("updatedAt".to_owned(), json!(self.now_ms)),
                ("inputsHash".to_owned(), json!(derive_id(&derived, "template"))),
                ("templateNodeId".to_owned(), json!(child.id)),
                ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
            ]);
            if let Some(description) = &child.description {
                set.insert("description".into(), json!(description));
            }
            if child.is_project {
                set.insert("isProject".into(), json!(true));
            }
            self.plan.push(Planned::upsert(KIND_WORK_ITEMS, &derived, set));
            self.created.insert(derived.clone());
            nodes.insert(child.id.clone(), derived.clone());
            self.fill(&derived, child, false, nodes, mode)?;
        }
        Ok(())
    }

    /// Create the node's instruction rows that `item_id` lacks (muted =
    /// archived rows count as present).
    fn plan_instructions(&mut self, item_id: &str, node: &RuleNode, exists: bool) -> Outcome<()> {
        let have: BTreeSet<&str> = if exists {
            self.world
                .instructions
                .iter()
                .filter(|i| i.work_item_id.as_deref() == Some(item_id))
                .filter_map(|i| i.template_instruction_id.as_deref())
                .collect()
        } else {
            BTreeSet::new()
        };
        let missing: Vec<_> = node
            .instructions
            .iter()
            .filter(|ri| !have.contains(ri.id.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let siblings = if exists {
            scope_siblings(&self.world.instructions, Some(item_id), None)
        } else {
            Vec::new()
        };
        let placed = place(&siblings, siblings.len(), missing.len())?;
        for (id, key) in &placed.rekeys {
            self.plan.push(Planned::update(
                KIND_INSTRUCTIONS,
                id,
                BTreeMap::from([("orderKey".to_owned(), json!(key))]),
            ));
        }
        for (ix, ri) in missing.into_iter().enumerate() {
            let id = derive_id(item_id, &ri.id);
            if self.doc.row_state(KIND_INSTRUCTIONS, &id) != RowState::Absent {
                continue;
            }
            self.plan.push(Planned::upsert(
                KIND_INSTRUCTIONS,
                &id,
                BTreeMap::from([
                    ("spaceId".to_owned(), json!(self.space_id)),
                    ("workItemId".to_owned(), json!(item_id)),
                    ("text".to_owned(), json!(ri.text)),
                    ("orderKey".to_owned(), json!(placed.keys[ix])),
                    ("createdBy".to_owned(), actor_row_value(self.actor)),
                    ("createdAt".to_owned(), json!(self.now_ms)),
                    ("updatedAt".to_owned(), json!(self.now_ms)),
                    ("inputsHash".to_owned(), json!(derive_id(&id, "template"))),
                    ("templateInstructionId".to_owned(), json!(ri.id)),
                    ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                ]),
            ));
        }
        Ok(())
    }

    /// Propagate a rules change to one following child.
    fn propagate(
        &mut self,
        child: &str,
        old: Option<&ContainerTemplate>,
        new: Option<&ContainerTemplate>,
    ) -> Outcome<()> {
        let empty = RuleNode::default();
        let old_root = old.map_or(&empty, |o| &o.root);
        let new_root = new.map(|n| &n.root);
        let followed: HashMap<&str, &DecodedItem> = self
            .world
            .followed_in(child)
            .into_iter()
            .filter(|i| !i.archived())
            .filter_map(|i| i.template_node_id.as_deref().map(|n| (n, i)))
            .collect();
        let old_nodes: HashMap<&str, &RuleNode> =
            old_root.preorder().into_iter().map(|(_, n)| (n.id.as_str(), n)).collect();
        let new_nodes: HashMap<&str, &RuleNode> = new_root
            .map(|r| r.preorder().into_iter().map(|(_, n)| (n.id.as_str(), n)).collect())
            .unwrap_or_default();

        // Removed nodes: detach (keep items and their instructions).
        let mut detach_instr: BTreeSet<&str> = BTreeSet::new();
        for (id, node) in &old_nodes {
            if new_nodes.contains_key(id) {
                continue;
            }
            if let Some(item) = followed.get(id) {
                self.patch(&item.id, "templateNodeId", Value::Null);
            }
            detach_instr.extend(node.instructions.iter().map(|i| i.id.as_str()));
        }
        // Surviving nodes: title, flags, instruction text.
        for (id, new_node) in &new_nodes {
            let Some(item) = followed.get(id) else { continue };
            let Some(old_node) = old_nodes.get(id) else { continue };
            if *id != new_root.map_or("", |r| r.id.as_str())
                && new_node.title.trim() != item.title.clone().unwrap_or_default().trim()
                && new_node.title.trim() != old_node.title.trim()
            {
                self.patch(&item.id, "title", json!(new_node.title.trim()));
            }
            if new_node.is_project != old_node.is_project
                && item.is_project != new_node.is_project
            {
                self.patch(&item.id, "isProject", json!(new_node.is_project));
            }
            for old_ins in &old_node.instructions {
                let rows = self.world.instructions.iter().filter(|r| {
                    r.work_item_id.as_deref() == Some(item.id.as_str())
                        && r.template_instruction_id.as_deref() == Some(old_ins.id.as_str())
                        && !r.archived()
                });
                match new_node.instructions.iter().find(|n| n.id == old_ins.id) {
                    Some(new_ins) if new_ins.text != old_ins.text => {
                        for row in rows {
                            if row.text != new_ins.text {
                                self.instruction_patch(&row.id, "text", json!(new_ins.text));
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        for row in rows {
                            self.instruction_patch(&row.id, "archivedAt", json!(self.now_ms));
                            self.instruction_patch(
                                &row.id,
                                "archivedBy",
                                actor_row_value(self.actor),
                            );
                        }
                    }
                }
            }
        }
        for row in &self.world.instructions {
            if let Some(tid) = row.template_instruction_id.as_deref()
                && detach_instr.contains(tid)
                && row
                    .work_item_id
                    .as_deref()
                    .is_some_and(|w| followed.values().any(|i| i.id == w) || w == child)
            {
                let id = row.id.clone();
                self.instruction_patch(&id, "templateInstructionId", Value::Null);
            }
        }
        // Added nodes and instructions.
        if let Some(new) = new {
            let before = self.plan.len();
            self.follow_added(child, new)?;
            let _ = before;
            // A moved worker destination: rebind when the old one is gone.
            let nodes = self.node_items(child, new);
            for (_, node) in new.root.preorder() {
                if !node.is_project {
                    continue;
                }
                let (Some(project), Some(dest)) = (
                    nodes.get(&node.id),
                    destination_of(node).and_then(|d| nodes.get(&d.id)),
                ) else {
                    continue;
                };
                let old_dest = old_nodes
                    .get(node.id.as_str())
                    .and_then(|o| destination_of(o))
                    .map(|d| d.id.as_str());
                let new_dest = destination_of(node).map(|d| d.id.as_str());
                if old_dest != new_dest
                    && self
                        .world
                        .items
                        .get(project)
                        .and_then(|i| i.worker_destination_id.as_deref())
                        != Some(dest.as_str())
                {
                    self.patch(project, "workerDestinationId", json!(dest));
                }
            }
        }
        Ok(())
    }

    /// Existing followed items of `child`, by rule node id (created ones are
    /// resolved by the derived id).
    fn node_items(&self, child: &str, rules: &ContainerTemplate) -> NodeItems {
        let mut out = NodeItems::new();
        for item in self.world.followed_in(child) {
            if let Some(n) = &item.template_node_id {
                out.insert(n.clone(), item.id.clone());
            }
        }
        for (parent, node) in rules.root.preorder() {
            if out.contains_key(&node.id) {
                continue;
            }
            if let Some(parent_item) = parent.and_then(|p| out.get(p)).cloned() {
                let derived = derive_id(&parent_item, &node.id);
                if self.created.contains(&derived) {
                    out.insert(node.id.clone(), derived);
                }
            }
        }
        out
    }

    fn instruction_patch(&mut self, id: &str, field: &str, value: Value) {
        let patch = self.patches.entry(id.to_owned()).or_default();
        patch.insert("__instruction".to_owned(), json!(true));
        patch.insert(field.to_owned(), value);
        patch.insert("updatedAt".to_owned(), json!(self.now_ms));
    }

    /// Add the parts of `rules` a following `child` lacks, without adopting
    /// by title and without touching existing items.
    fn follow_added(&mut self, child: &str, rules: &ContainerTemplate) -> Outcome<()> {
        let mut nodes = NodeItems::new();
        for item in self.world.followed_in(child) {
            if let Some(n) = &item.template_node_id {
                nodes.insert(n.clone(), item.id.clone());
            }
        }
        // Walk only nodes that exist as items; create what's missing below.
        self.fill_added(child, &rules.root, &mut nodes)
    }

    fn fill_added(&mut self, item_id: &str, node: &RuleNode, nodes: &mut NodeItems) -> Outcome<()> {
        self.plan_instructions(item_id, node, true)?;
        let mut missing: Vec<&RuleNode> = Vec::new();
        for child in &node.children {
            if let Some(existing) = nodes.get(&child.id).cloned() {
                self.fill_added(&existing, child, nodes)?;
            } else {
                missing.push(child);
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        // Reuse the creation path through a one-level rule node.
        let wrapper = RuleNode {
            children: missing.into_iter().cloned().collect(),
            ..node.clone()
        };
        let wrapper = RuleNode {
            instructions: Vec::new(),
            ..wrapper
        };
        let mut created_nodes = NodeItems::new();
        self.fill(item_id, &wrapper, true, &mut created_nodes, Mode::Adopt)?;
        nodes.extend(created_nodes);
        Ok(())
    }
}

/// The worker-destination node of a project node: the first flagged
/// descendant not below another project.
fn destination_of(project: &RuleNode) -> Option<&RuleNode> {
    fn find<'a>(node: &'a RuleNode) -> Option<&'a RuleNode> {
        for child in &node.children {
            if child.is_worker_destination {
                return Some(child);
            }
            if !child.is_project
                && let Some(found) = find(child)
            {
                return Some(found);
            }
        }
        None
    }
    find(project)
}

#[cfg(test)]
mod tests;
