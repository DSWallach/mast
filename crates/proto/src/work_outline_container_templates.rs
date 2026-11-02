//! Container templates (contract amendment "container templates"): rules
//! configured on a work item (e.g. "Projects") that every DIRECT child
//! follows. Each followed child gets real items (the rule nodes) with real,
//! template-linked instruction rows, so chats, Required Actions, links and the
//! instruction pipeline work unchanged.
//!
//! The rules live in `workItems.containerTemplate` of the container. The root
//! node describes the child itself (its title is ignored: the child keeps its
//! own); its children are the items created under each child.
//! Materialized items carry `workItems.templateNodeId` (the rule node they
//! follow); materialized instruction rows carry
//! `workItemInstructions.templateInstructionId`. A child without
//! `templateNodeId` "doesn't follow the template" until Apply.

use serde::{Deserialize, Serialize};

/// Limits (same order of magnitude as copy templates).
pub mod container_template_limits {
    pub const MAX_RULE_NODES: usize = 64;
    pub const MAX_RULE_DEPTH: usize = 6;
    pub const MAX_RULE_INSTRUCTIONS_PER_NODE: usize = 16;
    /// Serialized rules, in UTF-16 code units (an op is capped at 15,000).
    pub const MAX_RULES_UTF16_UNITS: usize = 13_000;
}

/// One instruction of a rule node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInstruction {
    /// Stable id; empty on input = assigned by the registry.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
}

/// One node of the rules. `id` is stable across edits (renames follow it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleNode {
    /// Stable id; empty on input = assigned by the registry.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Description given to a new item (set when created, not kept in sync).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The item is a project (`isProject`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_project: bool,
    /// The item is the worker destination of its nearest project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_worker_destination: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<RuleInstruction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RuleNode>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// The rules of a container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerTemplate {
    /// Starts at 1; every save adds 1.
    #[serde(default)]
    pub revision: u32,
    /// Describes each child (title ignored); its children are created under it.
    #[serde(default)]
    pub root: RuleNode,
}

impl RuleNode {
    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    pub fn depth(&self) -> usize {
        1 + self.children.iter().map(Self::depth).max().unwrap_or(0)
    }

    /// Preorder `(parent id, node)`; the root's parent is `None`.
    pub fn preorder(&self) -> Vec<(Option<&str>, &RuleNode)> {
        fn walk<'a>(
            parent: Option<&'a str>,
            node: &'a RuleNode,
            out: &mut Vec<(Option<&'a str>, &'a RuleNode)>,
        ) {
            out.push((parent, node));
            for child in &node.children {
                walk(Some(node.id.as_str()), child, out);
            }
        }
        let mut out = Vec::new();
        walk(None, self, &mut out);
        out
    }

    pub fn find(&self, id: &str) -> Option<&RuleNode> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Size and shape checks; the message is user-presentable.
    pub fn check(&self) -> Result<(), String> {
        use container_template_limits as l;
        if self.node_count() > l::MAX_RULE_NODES {
            return Err(format!("A template holds at most {} items", l::MAX_RULE_NODES));
        }
        if self.depth() > l::MAX_RULE_DEPTH {
            return Err(format!("A template nests at most {} levels", l::MAX_RULE_DEPTH));
        }
        let mut destinations = 0;
        for (parent, node) in self.preorder() {
            if node.instructions.len() > l::MAX_RULE_INSTRUCTIONS_PER_NODE {
                return Err(format!(
                    "An item holds at most {} instructions",
                    l::MAX_RULE_INSTRUCTIONS_PER_NODE
                ));
            }
            if parent.is_some() && node.title.trim().is_empty() {
                return Err("Every template item needs a title".into());
            }
            if node.is_worker_destination {
                destinations += 1;
                if parent.is_none() {
                    return Err("The root cannot be the worker destination".into());
                }
            }
        }
        if destinations > 1 {
            return Err("Only one item can be the worker destination".into());
        }
        Ok(())
    }

    /// Give every node and instruction without an id a deterministic one
    /// (`derive(seed, "rule-<n>")`, in preorder), keeping existing ids.
    pub fn assign_ids(&mut self, seed: &str) {
        fn walk(node: &mut RuleNode, seed: &str, n: &mut usize) {
            if node.id.is_empty() {
                node.id = crate::work_outline::derive_id(seed, &format!("rule-{n}"));
            }
            *n += 1;
            for ins in &mut node.instructions {
                if ins.id.is_empty() {
                    ins.id = crate::work_outline::derive_id(seed, &format!("rule-{n}"));
                }
                *n += 1;
            }
            for child in &mut node.children {
                walk(child, seed, n);
            }
        }
        walk(self, seed, &mut 0);
    }
}

/// `setContainerTemplate`: replace (or, with `template: None`, clear) the
/// rules of `work_item_id`. User action only (admin chats join later). Saving
/// propagates to every following child (see the registry). Reply `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetContainerTemplateParams {
    pub operation_id: String,
    pub space_id: String,
    pub work_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<ContainerTemplate>,
}

/// `applyContainerTemplate`: make `work_item_id` follow its parent's rules.
/// `work_item_id` is the container (apply to every direct child) or one
/// child. Existing items are matched by title level by level and adopted;
/// missing ones are created; nothing is deleted or moved. Reply `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyContainerTemplateParams {
    pub operation_id: String,
    pub space_id: String,
    pub work_item_id: String,
}
