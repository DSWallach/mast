//! Read-only health checks over facts collected for one outline subtree.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthItem {
    pub id: String,
    pub container: bool,
    pub completed: bool,
    pub linear_completed: Option<bool>,
    pub chat_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthChat {
    pub id: String,
    pub item_id: Option<String>,
    pub archived: bool,
    pub finished: bool,
    pub stale: bool,
    /// `None` means the cached PR state is not known yet.
    pub has_open_pr: Option<bool>,
    pub on_default_branch: bool,
    pub has_bound_worktree: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthPullRequest {
    pub url: String,
    pub item_id: Option<String>,
    pub linked_to_ticket: bool,
    pub open: bool,
    pub merged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum HealthEntry {
    SeveralLiveChats { item_id: String, chat_ids: Vec<String> },
    ChatOnContainer { item_id: String, chat_id: String },
    MergedPrOnOpenItem { item_id: String, url: String },
    UnlinkedPr { url: String },
    WorkerOnDefaultBranch { chat_id: String },
    StaleChat { chat_id: String },
}

/// Select live items in one subtree in a single pass over the materialized
/// parent-before-child snapshot. This never widens to the enclosing project.
pub fn subtree_items<'a>(snapshot: &'a mast_proto::WorkOutlineSnapshot, root: &str) -> Vec<&'a mast_proto::WorkItemView> {
    use std::collections::HashSet;
    let mut included = HashSet::new();
    let mut items = Vec::new();
    for item in &snapshot.items {
        if item.archived_at.is_none() && (item.id == root || item.parent_work_item_id.as_deref().is_some_and(|id| included.contains(id))) {
            included.insert(item.id.as_str());
            items.push(item);
        }
    }
    items
}

/// Builds the list from a single snapshot, without IO or mutations. Inputs
/// must already be restricted to the caller's permitted subtree.
pub fn check_health(items: &[HealthItem], chats: &[HealthChat], prs: &[HealthPullRequest]) -> Vec<HealthEntry> {
    use std::collections::{HashMap, HashSet};
    let chats_by_id: HashMap<_, _> = chats.iter().map(|chat| (chat.id.as_str(), chat)).collect();
    let items_by_id: HashMap<_, _> = items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut entries = Vec::new();
    for item in items {
        let mut seen = HashSet::new();
        let live: Vec<_> = item.chat_ids.iter().filter(|id| seen.insert(id.as_str()) && chats_by_id.get(id.as_str()).is_some_and(|chat| !chat.archived && !chat.finished)).cloned().collect();
        if !item.container && live.len() > 1 {
            entries.push(HealthEntry::SeveralLiveChats { item_id: item.id.clone(), chat_ids: live.clone() });
        }
        if item.container {
            for chat_id in item.chat_ids.iter().filter(|id| chats_by_id.get(id.as_str()).is_some_and(|chat| !chat.archived)) {
                entries.push(HealthEntry::ChatOnContainer { item_id: item.id.clone(), chat_id: chat_id.clone() });
            }
        }
    }
    for pr in prs {
        match pr.item_id.as_deref().and_then(|id| items_by_id.get(id)) {
            Some(item) if pr.merged && (!item.completed || item.linear_completed == Some(false)) => entries.push(HealthEntry::MergedPrOnOpenItem { item_id: item.id.clone(), url: pr.url.clone() }),
            None if !pr.linked_to_ticket && (pr.open || pr.merged) => entries.push(HealthEntry::UnlinkedPr { url: pr.url.clone() }),
            _ => {}
        }
    }
    for chat in chats.iter().filter(|chat| !chat.archived && !chat.finished) {
        let item = chat.item_id.as_deref().and_then(|id| items_by_id.get(id)).copied();
        let ticket = item.is_some_and(|item| !item.container);
        if ticket && chat.on_default_branch && !chat.has_bound_worktree {
            entries.push(HealthEntry::WorkerOnDefaultBranch { chat_id: chat.id.clone() });
        }
        if chat.stale && chat.has_open_pr == Some(false) && !item.is_some_and(|item| item.completed || item.linear_completed == Some(true)) {
            entries.push(HealthEntry::StaleChat { chat_id: chat.id.clone() });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_does_not_include_siblings_or_archived_descendants() {
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId": "space", "items": [
                {"id": "root", "spaceId": "space"},
                {"id": "child", "spaceId": "space", "parentWorkItemId": "root"},
                {"id": "archived", "spaceId": "space", "parentWorkItemId": "root", "archivedAt": 1},
                {"id": "hidden", "spaceId": "space", "parentWorkItemId": "archived"},
                {"id": "sibling", "spaceId": "space"}
            ]
        })).unwrap();
        let ids: Vec<_> = subtree_items(&snapshot, "root").into_iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["root", "child"]);
        assert!(subtree_items(&snapshot, "missing").is_empty());
    }

    #[test]
    fn detects_each_condition_and_ignores_finished_chats() {
        let items = vec![HealthItem { id: "ticket".into(), container: false, completed: false, linear_completed: None, chat_ids: vec!["a".into(), "b".into(), "finished".into()] },
            HealthItem { id: "container".into(), container: true, completed: false, linear_completed: None, chat_ids: vec!["c".into()] }];
        let chat = |id: &str, item: &str| HealthChat { id: id.into(), item_id: Some(item.into()), archived: false, finished: false,
            stale: false, has_open_pr: Some(false), on_default_branch: false, has_bound_worktree: false };
        let mut a = chat("a", "ticket"); a.stale = true; a.on_default_branch = true;
        let b = chat("b", "ticket");
        let c = chat("c", "container");
        let mut finished = chat("finished", "ticket"); finished.finished = true; finished.stale = true;
        let prs = vec![HealthPullRequest { url: "merged".into(), item_id: Some("ticket".into()), linked_to_ticket: true, open: false, merged: true },
            HealthPullRequest { url: "unlinked".into(), item_id: None, linked_to_ticket: false, open: true, merged: false }];
        let entries = check_health(&items, &[a, b, c, finished], &prs);
        assert_eq!(entries.len(), 6);
        assert!(matches!(&entries[0], HealthEntry::SeveralLiveChats { chat_ids, .. } if chat_ids.len() == 2));
        assert!(entries.iter().any(|entry| matches!(entry, HealthEntry::ChatOnContainer { .. })));
        assert!(entries.iter().any(|entry| matches!(entry, HealthEntry::MergedPrOnOpenItem { .. })));
        assert!(entries.iter().any(|entry| matches!(entry, HealthEntry::UnlinkedPr { .. })));
        assert!(entries.iter().any(|entry| matches!(entry, HealthEntry::WorkerOnDefaultBranch { .. })));
        assert!(entries.iter().any(|entry| matches!(entry, HealthEntry::StaleChat { .. })));
    }

    #[test]
    fn open_pr_prevents_stale_and_bound_worktree_prevents_default_branch_warning() {
        let item = HealthItem { id: "item".into(), container: false, completed: true, linear_completed: None, chat_ids: vec!["chat".into()] };
        let chat = HealthChat { id: "chat".into(), item_id: Some("item".into()), archived: false, finished: false, stale: true,
            has_open_pr: Some(true), on_default_branch: true, has_bound_worktree: true };
        let pr = HealthPullRequest { url: "merged".into(), item_id: Some("item".into()), linked_to_ticket: true, open: false, merged: true };
        assert!(check_health(&[item], &[chat], &[pr]).is_empty());
    }

    #[test]
    fn completed_local_item_with_open_linear_ticket_still_needs_cleanup() {
        let item = HealthItem { id: "item".into(), container: false, completed: true, linear_completed: Some(false), chat_ids: vec![] };
        let pr = HealthPullRequest { url: "merged".into(), item_id: Some("item".into()), linked_to_ticket: true, open: false, merged: true };
        assert!(matches!(&check_health(&[item], &[], &[pr])[..], [HealthEntry::MergedPrOnOpenItem { .. }]));
    }

    #[test]
    fn pull_request_matched_to_a_linear_ticket_is_not_unlinked() {
        let pr = HealthPullRequest { url: "open".into(), item_id: None, linked_to_ticket: true, open: true, merged: false };
        assert!(check_health(&[], &[], &[pr]).is_empty());
    }

    #[test]
    fn unknown_pr_state_does_not_mark_a_chat_stale() {
        let chat = HealthChat { id: "chat".into(), item_id: None, archived: false, finished: false, stale: true,
            has_open_pr: None, on_default_branch: false, has_bound_worktree: false };
        assert!(check_health(&[], &[chat], &[]).is_empty());
    }
}
