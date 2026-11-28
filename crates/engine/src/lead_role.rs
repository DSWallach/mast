//! The Lead tier: Manager → Lead → Worker.
//!
//! A Lead is a standing role scoped to one work item (its subtree). It is
//! read-only, holds a path lease over the subtree and a budget carved from the
//! project's, and runs at most [`MAX_WORKERS_PER_LEAD`] Workers. The project
//! has at most [`MAX_LEADS_PER_PROJECT`] Leads and the tree is never deeper
//! than [`MAX_LEAD_HIERARCHY_DEPTH`] (Manager 0, Lead 1, Worker 2).
//!
//! Everything here is a pure policy over a small view of the chat tree, so the
//! launch path, the wake router and a paired peer's transport (stage B) apply
//! the same rules. The wake firewall is two types in `mast_proto`: a
//! [`LeadContextPacket`] goes down (at most 8k tokens) and a [`LeadDigest`]
//! comes up (at most 350 tokens). A Manager never receives a leaf Worker's
//! event: the router sends it to the Lead only.

use mast_proto::{
    ItemBudget, LeadContextPacket, LeadDigest, MAX_LEAD_CONTEXT_TOKENS, MAX_LEAD_HIERARCHY_DEPTH,
    MAX_LEADS_PER_PROJECT, MAX_WORKERS_PER_LEAD, StandingRoleKind, WorkItemTaskSpec, approx_tokens,
};

use crate::path_leases::{Lease, path_covered_by};
use crate::workspace_host::WorkspaceHost;

/// What the policy needs to know about one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatNode {
    pub parent: Option<String>,
    /// The standing role the chat holds now, if any.
    pub role: Option<StandingRoleKind>,
    /// The chat's parent is a Manager on a paired device, so it is not in
    /// the chain but still counts one level of depth.
    pub remote_parent: bool,
}

/// Why a Lead-tier launch is refused. `Display` is the message the caller sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadRefusal {
    /// A Lead's parent must be the project's Manager.
    LeadNeedsManager,
    /// The launch would be deeper than [`MAX_LEAD_HIERARCHY_DEPTH`].
    TooDeep { depth: usize },
    TooManyLeads,
    TooManyWorkers,
    /// A Lead's prompt (its context packet) is over [`MAX_LEAD_CONTEXT_TOKENS`].
    ContextTooLarge { tokens: u32 },
    BudgetExceeded { what: &'static str, asked: u64, left: u64 },
    /// The parent has a budget, so the child must name its own share.
    BudgetUnspecified { what: &'static str },
    /// A Worker's `ownedPaths` entry is outside its Lead's subtree lease.
    PathOutsideLease { path: String },
}

impl std::fmt::Display for LeadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeadNeedsManager => write!(f, "A Lead must be started by the project's Manager"),
            Self::TooDeep { depth } => write!(
                f,
                "Lead hierarchy depth {depth} is over the limit of {MAX_LEAD_HIERARCHY_DEPTH} (Manager → Lead → Worker)"
            ),
            Self::TooManyLeads => write!(f, "This project already has {MAX_LEADS_PER_PROJECT} Leads"),
            Self::TooManyWorkers => write!(f, "This Lead already has {MAX_WORKERS_PER_LEAD} Workers"),
            Self::ContextTooLarge { tokens } => write!(
                f,
                "The Lead context packet is about {tokens} tokens, over the limit of {MAX_LEAD_CONTEXT_TOKENS}"
            ),
            Self::BudgetExceeded { what, asked, left } => {
                write!(f, "The Lead's {what} budget of {asked} exceeds the {left} left in its parent's")
            }
            Self::BudgetUnspecified { what } => {
                write!(f, "The parent has a {what} budget, so this item needs its own {what} budget")
            }
            Self::PathOutsideLease { path } => {
                write!(f, "ownedPaths `{path}` is outside the Lead's subtree lease")
            }
        }
    }
}

/// Ancestors of `chat` (nearest first), stopping at a missing node or a cycle.
fn ancestors(chat: &str, node: &dyn Fn(&str) -> Option<ChatNode>) -> Vec<(String, ChatNode)> {
    let mut out: Vec<(String, ChatNode)> = Vec::new();
    let mut next = node(chat).and_then(|n| n.parent);
    while let Some(id) = next {
        if id == chat || out.iter().any(|(seen, _)| *seen == id) {
            break;
        }
        let Some(found) = node(&id) else { break };
        next = found.parent.clone();
        out.push((id, found));
    }
    out
}

/// A Lead's parent must hold the Manager role.
pub fn check_lead_parent(
    parent: Option<&str>,
    node: &dyn Fn(&str) -> Option<ChatNode>,
) -> Result<(), LeadRefusal> {
    match parent.and_then(node) {
        Some(ChatNode { role: Some(StandingRoleKind::Manager), .. }) => Ok(()),
        _ => Err(LeadRefusal::LeadNeedsManager),
    }
}

/// `live_leads` counts the project's Leads other than the one being launched.
pub fn check_lead_count(live_leads: usize) -> Result<(), LeadRefusal> {
    (live_leads < MAX_LEADS_PER_PROJECT).then_some(()).ok_or(LeadRefusal::TooManyLeads)
}

/// Admit a Worker launched under `parent`. Returns the Lead chat the Worker
/// belongs to, or `None` when no Lead is in its ancestry (a plain Manager's
/// Worker, which this tier does not touch). A Worker's depth is its parent's
/// ancestor count plus two; any chain with a Lead in it that would reach
/// depth 3 (a Worker of a Worker of a Lead) is refused.
pub fn check_worker_parent(
    parent: &str,
    node: &dyn Fn(&str) -> Option<ChatNode>,
    live_workers_of_parent: usize,
) -> Result<Option<String>, LeadRefusal> {
    let Some(parent_node) = node(parent) else { return Ok(None) };
    let chain = ancestors(parent, node);
    let parent_is_lead = parent_node.role == Some(StandingRoleKind::Lead);
    let lead_above = chain.iter().any(|(_, n)| n.role == Some(StandingRoleKind::Lead));
    if !parent_is_lead && !lead_above {
        return Ok(None);
    }
    let remote = parent_node.remote_parent || chain.iter().any(|(_, n)| n.remote_parent);
    let depth = chain.len() + 1 + usize::from(remote);
    if depth > usize::from(MAX_LEAD_HIERARCHY_DEPTH) {
        return Err(LeadRefusal::TooDeep { depth });
    }
    if live_workers_of_parent >= MAX_WORKERS_PER_LEAD {
        return Err(LeadRefusal::TooManyWorkers);
    }
    Ok(Some(parent.to_owned()))
}

/// The policy's view of a chat in the registry.
pub fn chat_node(workspace: &WorkspaceHost, chat: &str) -> Option<ChatNode> {
    let row = workspace.chat(chat).ok().flatten()?;
    let peer_lead = workspace.peer_lead_item(chat).is_some();
    Some(ChatNode {
        parent: workspace.live_parent(chat).or(row.parent_chat_id),
        role: workspace
            .standing_role_of_chat(chat)
            .map(|role| role.role)
            .or(peer_lead.then_some(StandingRoleKind::Lead)),
        remote_parent: peer_lead,
    })
}

/// Admit a new plain chat (`new_chat`) under `parent`: the one check every
/// way of starting a child chat goes through. Returns the Lead chat it will
/// belong to, or `None` when no Lead is above it.
pub fn admit_child(workspace: &WorkspaceHost, parent: &str, new_chat: &str) -> Result<Option<String>, LeadRefusal> {
    let live_workers = workspace
        .read_chats()
        .unwrap_or_default()
        .iter()
        .filter(|chat| chat.parent_chat_id.as_deref() == Some(parent) && !chat.archived && chat.id != new_chat)
        .filter(|chat| workspace.standing_role_of_chat(&chat.id).is_none())
        .count();
    check_worker_parent(parent, &|id| chat_node(workspace, id), live_workers)
}

/// A Lead's prompt is its context packet: refuse one over the cap.
pub fn check_context_size(prompt: &str) -> Result<(), LeadRefusal> {
    let tokens = approx_tokens(prompt);
    (tokens <= MAX_LEAD_CONTEXT_TOKENS).then_some(()).ok_or(LeadRefusal::ContextTooLarge { tokens })
}

/// Check a requested budget fits what is left of `parent` after `siblings`.
/// An unspecified limit on the parent is unlimited. A child that names no
/// limit where its parent has one is refused: the budget is a share the child
/// declares, and Mast does not guess it. Returns the budget the child holds.
pub fn carve_budget(
    parent: &ItemBudget,
    siblings: &[ItemBudget],
    request: &ItemBudget,
) -> Result<ItemBudget, LeadRefusal> {
    fn carve(
        what: &'static str,
        parent: Option<u64>,
        siblings: u64,
        request: Option<u64>,
    ) -> Result<Option<u64>, LeadRefusal> {
        let Some(total) = parent else { return Ok(request) };
        let left = total.saturating_sub(siblings);
        match request {
            Some(asked) if asked > left => Err(LeadRefusal::BudgetExceeded { what, asked, left }),
            Some(asked) => Ok(Some(asked)),
            None => Err(LeadRefusal::BudgetUnspecified { what }),
        }
    }
    let spent = |f: fn(&ItemBudget) -> Option<u64>| siblings.iter().filter_map(f).sum::<u64>();
    Ok(ItemBudget {
        max_tokens: carve("token", parent.max_tokens, spent(|b| b.max_tokens), request.max_tokens)?,
        max_turns: carve(
            "turn",
            parent.max_turns.map(u64::from),
            spent(|b| b.max_turns.map(u64::from)),
            request.max_turns.map(u64::from),
        )?
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    })
}

/// A Lead holds one lease over its subtree, named by its `ownedPaths`.
pub fn subtree_lease(lead_item_id: &str, spec: &WorkItemTaskSpec) -> Lease {
    Lease::from_spec(lead_item_id, spec)
}

/// Every `ownedPaths` entry of a Worker must sit under the Lead's lease. A Lead
/// with no `ownedPaths` leases nothing, so it does not constrain its Workers
/// (their own leases still apply).
pub fn check_worker_paths(lead: &Lease, worker: &WorkItemTaskSpec) -> Result<(), LeadRefusal> {
    if lead.owned_paths.is_empty() {
        return Ok(());
    }
    match worker
        .owned_paths
        .iter()
        .find(|path| !lead.owned_paths.iter().any(|outer| path_covered_by(path, outer)))
    {
        Some(path) => Err(LeadRefusal::PathOutsideLease { path: path.clone() }),
        None => Ok(()),
    }
}

/// The packet a Lead is started with, from its item's task spec.
pub fn context_packet(lead_item_id: &str, spec: &WorkItemTaskSpec, brief: &str) -> LeadContextPacket {
    LeadContextPacket {
        lead_item_id: lead_item_id.to_owned(),
        goal: spec.goal.clone(),
        owned_paths: spec.owned_paths.clone(),
        req_ids: spec.req_ids.clone(),
        budget: spec.item_budget.clone(),
        brief: brief.to_owned(),
    }
}

/// What a Manager receives for a Lead's report: the digest, never the report.
pub fn digest_for_manager(lead_item_id: &str, lead_chat_id: &str, report: &str) -> String {
    digest_for_manager_status(lead_item_id, lead_chat_id, report, false)
}

/// [`digest_for_manager`] with the status: `done` for a report that carries a
/// completion, `InProgress` otherwise. A report that already is this Lead's
/// digest within the cap comes back as it was, so a digest made on the
/// Lead's device is not wrapped again on the Manager's.
pub fn digest_for_manager_status(lead_item_id: &str, lead_chat_id: &str, report: &str, done: bool) -> String {
    let header = format!("[Lead digest, item {lead_item_id}, chat {lead_chat_id}]");
    if report.starts_with(&header) && approx_tokens(report) <= mast_proto::MAX_LEAD_DIGEST_TOKENS {
        return report.to_owned();
    }
    let mut digest = LeadDigest::from_report(lead_item_id, lead_chat_id, report);
    if done {
        digest.status = mast_proto::LeadDigestStatus::Done;
    }
    digest.bounded().render()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    type Tree = HashMap<&'static str, ChatNode>;

    fn tree(rows: &[(&'static str, Option<&str>, Option<StandingRoleKind>)]) -> Tree {
        rows.iter()
            .map(|(id, parent, role)| {
                (*id, ChatNode { parent: parent.map(str::to_owned), role: *role, remote_parent: false })
            })
            .collect()
    }

    fn lookup(tree: &Tree) -> impl Fn(&str) -> Option<ChatNode> + '_ {
        |id| tree.get(id).cloned()
    }

    fn spec(paths: &[&str]) -> WorkItemTaskSpec {
        WorkItemTaskSpec { owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(), ..Default::default() }
    }

    use StandingRoleKind::{Lead, Manager};

    #[test]
    fn a_lead_must_hang_off_the_manager() {
        let t = tree(&[("m", None, Some(Manager)), ("l", Some("m"), Some(Lead)), ("w", Some("l"), None)]);
        assert_eq!(check_lead_parent(Some("m"), &lookup(&t)), Ok(()));
        for bad in [Some("l"), Some("w"), Some("gone"), None] {
            assert_eq!(check_lead_parent(bad, &lookup(&t)), Err(LeadRefusal::LeadNeedsManager));
        }
    }

    #[test]
    fn depth_three_is_refused_and_depth_two_is_not() {
        let t = tree(&[
            ("m", None, Some(Manager)),
            ("l", Some("m"), Some(Lead)),
            ("w", Some("l"), None),
            ("plain", Some("m"), None),
            ("sub", Some("plain"), None),
        ]);
        assert_eq!(check_worker_parent("l", &lookup(&t), 0), Ok(Some("l".into())));
        assert_eq!(check_worker_parent("w", &lookup(&t), 0), Err(LeadRefusal::TooDeep { depth: 3 }));
        // A Manager's own Workers, with no Lead above, are not this tier's business.
        assert_eq!(check_worker_parent("m", &lookup(&t), 0), Ok(None));
        assert_eq!(check_worker_parent("plain", &lookup(&t), 0), Ok(None));
        assert_eq!(check_worker_parent("sub", &lookup(&t), 0), Ok(None));
    }

    #[test]
    fn the_four_lead_and_six_worker_limits_are_exact() {
        assert_eq!(check_lead_count(MAX_LEADS_PER_PROJECT - 1), Ok(()));
        assert_eq!(check_lead_count(MAX_LEADS_PER_PROJECT), Err(LeadRefusal::TooManyLeads));
        let t = tree(&[("m", None, Some(Manager)), ("l", Some("m"), Some(Lead))]);
        assert_eq!(check_worker_parent("l", &lookup(&t), MAX_WORKERS_PER_LEAD - 1), Ok(Some("l".into())));
        assert_eq!(check_worker_parent("l", &lookup(&t), MAX_WORKERS_PER_LEAD), Err(LeadRefusal::TooManyWorkers));
    }

    #[test]
    fn a_cyclic_tree_does_not_loop() {
        let t = tree(&[("a", Some("b"), Some(Lead)), ("b", Some("a"), None)]);
        assert!(check_worker_parent("a", &lookup(&t), 0).is_ok());
    }

    #[test]
    fn budgets_are_carved_from_the_parent() {
        let parent = ItemBudget { max_tokens: Some(1000), max_turns: Some(50) };
        let used = [ItemBudget { max_tokens: Some(600), max_turns: Some(10) }];
        let ask = |t, n| ItemBudget { max_tokens: t, max_turns: n };
        assert_eq!(carve_budget(&parent, &used, &ask(Some(400), Some(40))).unwrap(), ask(Some(400), Some(40)));
        assert_eq!(
            carve_budget(&parent, &used, &ask(Some(401), None)),
            Err(LeadRefusal::BudgetExceeded { what: "token", asked: 401, left: 400 })
        );
        assert_eq!(
            carve_budget(&parent, &used, &ask(None, Some(1))),
            Err(LeadRefusal::BudgetUnspecified { what: "token" })
        );
        // An unspecified parent limit is unlimited.
        assert_eq!(carve_budget(&ItemBudget::default(), &used, &ask(Some(9_999), None)).unwrap(), ask(Some(9_999), None));
    }

    #[test]
    fn worker_paths_must_sit_inside_the_lead_lease() {
        let lead = subtree_lease("lead", &spec(&["crates/engine/**"]));
        assert_eq!(check_worker_paths(&lead, &spec(&["crates/engine/src/a.rs", "crates/engine/tests"])), Ok(()));
        assert_eq!(
            check_worker_paths(&lead, &spec(&["crates/ui/src/a.rs"])),
            Err(LeadRefusal::PathOutsideLease { path: "crates/ui/src/a.rs".into() })
        );
        assert_eq!(
            check_worker_paths(&lead, &spec(&["crates/*/src/a.rs"])),
            Err(LeadRefusal::PathOutsideLease { path: "crates/*/src/a.rs".into() })
        );
        assert_eq!(check_worker_paths(&subtree_lease("l", &spec(&[])), &spec(&["anything"])), Ok(()));
    }

    #[test]
    fn two_leads_cannot_lease_overlapping_subtrees() {
        let registry = crate::path_leases::PathLeaseRegistry::new([subtree_lease("a", &spec(&["crates/engine/**"]))]);
        assert!(registry.check(&subtree_lease("b", &spec(&["crates/ui/**"]))).is_ok());
        assert!(registry.check(&subtree_lease("b", &spec(&["crates/engine/src"]))).is_err());
    }

    #[test]
    fn the_manager_gets_a_bounded_digest_and_none_of_the_report() {
        let report = format!("merged the slice\n{}", "leaf noise ".repeat(2_000));
        let text = digest_for_manager("item", "chat", &report);
        assert!(approx_tokens(&text) <= mast_proto::MAX_LEAD_DIGEST_TOKENS);
        assert!(text.starts_with("[Lead digest, item item, chat chat]"));
        assert!(text.contains("merged the slice"));
        assert_eq!(check_context_size(&"x".repeat(32_000)), Ok(()));
        assert!(check_context_size(&"x".repeat(32_004)).is_err());
    }

    #[test]
    fn the_packet_carries_the_leads_slice() {
        let mut s = spec(&["crates/engine/**"]);
        s.goal = "ship it".into();
        s.item_budget.max_tokens = Some(5);
        let packet = context_packet("lead", &s, "see the contract");
        assert!(packet.render().contains("ship it") && packet.render().contains("crates/engine/**"));
        assert_eq!(packet.budget.max_tokens, Some(5));
    }

    #[test]
    fn a_report_with_scope_and_token_lines_still_digests_within_the_cap() {
        let digest_lines = format!(
            "Scope: edited outside the task's ownedPaths (flagged, not reverted): {}\nTokens: {{\"inputTokens\":123456,\"turns\":40}}",
            (0..80).map(|n| format!("crates/engine/src/some/long/path_{n}.rs")).collect::<Vec<_>>().join(", ")
        );
        let report = format!("Slice merged, 2 PRs open.\n{}\n{digest_lines}", "detail ".repeat(2_000));
        let out = digest_for_manager("item", "chat", &report);
        assert!(out.starts_with("[Lead digest"), "{out}");
        assert!(mast_proto::approx_tokens(&out) <= mast_proto::MAX_LEAD_DIGEST_TOKENS, "{} bytes", out.len());
        assert!(out.contains("Slice merged, 2 PRs open."));
        // Lines past the cap are cut, not the headline.
        let short = format!("Done.\n{digest_lines}");
        let out = digest_for_manager("item", "chat", &short);
        assert!(out.starts_with("[Lead digest"), "{out}");
        assert!(mast_proto::approx_tokens(&out) <= mast_proto::MAX_LEAD_DIGEST_TOKENS, "{} bytes", out.len());
    }
}
