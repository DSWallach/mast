//! Read-only health checks over facts collected for one outline subtree.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Cached repository default branches. This is queried when health is
/// requested, never from a render callback. Failures are cached as well.
#[derive(Default)]
pub struct DefaultBranchCache {
    entries: tokio::sync::Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl DefaultBranchCache {
    async fn get_with<F, Fut>(&self, path: &str, lookup: F) -> Option<String>
    where F: FnOnce() -> Fut, Fut: std::future::Future<Output = Option<String>> {
        let mut entries = self.entries.lock().await;
        if let Some((at, branch)) = entries.get(path)
            && at.elapsed() < if branch.is_some() { Duration::from_secs(900) } else { Duration::from_secs(300) } {
            return branch.clone();
        }
        let branch = lookup().await;
        if entries.len() >= 128 && !entries.contains_key(path)
            && let Some(oldest) = entries.iter().min_by_key(|(_, (at, _))| *at).map(|(key, _)| key.clone()) {
            entries.remove(&oldest);
        }
        entries.insert(path.to_owned(), (Instant::now(), branch.clone()));
        branch
    }

    /// Resolve the locally recorded remote HEAD. A missing or invalid value
    /// stays unknown; it must not be guessed from a checkout's current branch.
    pub async fn get(&self, path: &str) -> Option<String> {
        if path.is_empty() || crate::repos::is_automatic_access_blocked(std::path::Path::new(path)) { return None; }
        self.get_with(path, || async {
            let mut command = mast_harness::git_command_async();
            command.current_dir(path).args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
                .stdin(std::process::Stdio::null()).kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(3), async {
                let _permit = crate::git_gate::acquire("health:default-branch").await;
                command.output().await
            }).await.ok()?.ok()?;
            if !output.status.success() { return None; }
            let name = String::from_utf8(output.stdout).ok()?;
            let branch = name.trim().strip_prefix("origin/")?.trim();
            (!branch.is_empty()).then(|| branch.to_owned())
        }).await
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum HealthEntry {
    SeveralLiveChats { item_id: String, chat_ids: Vec<String> },
    ChatOnContainer { item_id: String, chat_id: String },
    MergedPrOnOpenItem { item_id: String, url: String },
    UnlinkedPr { url: String },
    WorkerOnDefaultBranch { chat_id: String },
    StaleChat { chat_id: String },
}

/// Why a chat must not be archived by a cleanup, ordered from the strongest
/// reason. The engine reports it with each cleanup chat so the Tracker never
/// offers or applies an archive for these chats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthBlock {
    Running,
    AwaitingInput,
    Errored,
    OpenRequest,
    Queued,
    StandingRole,
    /// Started chats that are still live, so archiving would orphan them.
    Coordinating,
    /// A block from a newer engine: treated as blocked, never archived.
    #[serde(other)]
    Unknown,
}

impl HealthBlock {
    /// Short phrase for "kept: ..." notes in the Tracker.
    pub fn note(self) -> &'static str {
        match self {
            HealthBlock::Running => "running",
            HealthBlock::AwaitingInput => "waiting for input",
            HealthBlock::Errored => "stopped with an error",
            HealthBlock::OpenRequest => "has an open Required Action",
            HealthBlock::Queued => "has a queued message",
            HealthBlock::StandingRole => "holds a Manager or assistant role",
            HealthBlock::Coordinating => "is coordinating chats that are still live",
            HealthBlock::Unknown => "can't be archived from this version",
        }
    }
}

/// Per-chat facts for the cleanup rows: how long the chat has been idle and
/// whether anything blocks archiving it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthChatFacts {
    pub idle_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<HealthBlock>,
    /// Chats started by this one that are not archived.
    #[serde(default)]
    pub live_children: usize,
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

/// Convert the scoped outline into health inputs. Linear completion is an
/// optional cached observation keyed by item id; absence stays unknown.
pub fn health_items(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    root: &str,
    linear_completion: &std::collections::HashMap<String, bool>,
) -> Vec<HealthItem> {
    subtree_items(snapshot, root).into_iter().map(|item| HealthItem {
        id: item.id.clone(),
        container: !item.child_ids.is_empty(),
        completed: item.effective_status == mast_proto::WorkItemEffectiveStatus::Completed,
        linear_completed: linear_completion.get(&item.id).copied(),
        chat_ids: item.chat_ids.clone(),
    }).collect()
}

/// Find a project issue identifier at a branch-name boundary.
pub fn branch_issue(branch: &str, issues: &std::collections::HashSet<String>) -> Option<String> {
    let upper = branch.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_alphabetic() || (start > 0 && bytes[start - 1].is_ascii_alphanumeric()) { start += 1; continue; }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_alphanumeric() { end += 1; }
        if end < bytes.len() && bytes[end] == b'-' {
            let digits = end + 1;
            end = digits;
            while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
            if end > digits && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric()) && issues.contains(&upper[start..end]) {
                return Some(upper[start..end].to_owned());
            }
        }
        start = end.max(start + 1);
    }
    None
}

/// Fold chats matched to the same cached Linear issue into one health item.
/// Explicit issue references take precedence over branch matches. Issues
/// without a local item get a temporary id for the read-only result.
pub fn reconcile_health_chats(
    items: &mut Vec<HealthItem>,
    chats: &mut [HealthChat],
    item_issues: &HashMap<String, String>,
    chat_issues: &HashMap<String, String>,
    issues: &HashMap<String, bool>,
) {
    let mut issue_items = HashMap::<String, usize>::new();
    let item_indexes: HashMap<_, _> = items.iter().enumerate().map(|(index, item)| (item.id.clone(), index)).collect();
    let mut effective_issues = item_issues.clone();
    let explicit_issues: std::collections::HashSet<_> = item_issues.values().collect();
    for item in items.iter().filter(|item| !item.container && !item_issues.contains_key(&item.id)) {
        if let Some(issue) = item.chat_ids.iter().filter_map(|id| chat_issues.get(id)).find(|issue| issues.contains_key(*issue) || explicit_issues.contains(*issue)) {
            effective_issues.insert(item.id.clone(), issue.clone());
        }
    }
    for (index, item) in items.iter().enumerate().filter(|(_, item)| !item.container) {
        if let Some(issue) = effective_issues.get(&item.id) {
            issue_items.entry(issue.clone()).or_insert(index);
        }
    }
    let mut moves = HashMap::new();
    for chat in chats.iter_mut() {
        let source = chat.item_id.as_ref().and_then(|id| item_indexes.get(id)).copied();
        if source.is_some_and(|index| items[index].container) { continue; }
        let issue = chat.item_id.as_ref().and_then(|id| effective_issues.get(id))
            .or_else(|| chat_issues.get(&chat.id).filter(|issue| issues.contains_key(*issue) || issue_items.contains_key(*issue)));
        let Some(issue) = issue else { continue; };
        let target = *issue_items.entry(issue.clone()).or_insert_with(|| {
            let index = items.len();
            items.push(HealthItem { id: format!("linear:{issue}"), container: false, completed: false,
                linear_completed: issues.get(issue).copied(), chat_ids: Vec::new() });
            index
        });
        items[target].linear_completed = issues.get(issue).copied();
        moves.insert(chat.id.clone(), target);
        chat.item_id = Some(items[target].id.clone());
    }
    for item in items.iter_mut() {
        if !item.container { item.chat_ids.retain(|id| !moves.contains_key(id)); }
    }
    for (chat_id, target) in moves {
        items[target].chat_ids.push(chat_id);
    }
    for item in items { item.chat_ids.sort_unstable(); }
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
    // Several chats may report the same PR. Combine their observations before
    // deciding whether it is unlinked, so a linked observation wins over an
    // otherwise identical unlinked one regardless of input order.
    let mut observations = HashMap::<&str, (bool, bool, bool, Vec<&str>)>::new();
    let mut pr_order = Vec::new();
    for pr in prs {
        if !observations.contains_key(pr.url.as_str()) { pr_order.push(pr.url.as_str()); }
        let entry = observations.entry(pr.url.as_str()).or_default();
        entry.0 |= pr.linked_to_ticket;
        entry.1 |= pr.open;
        entry.2 |= pr.merged;
        if let Some(item_id) = pr.item_id.as_deref().filter(|id| items_by_id.contains_key(id)) {
            entry.0 |= items_by_id.get(item_id).is_some_and(|item| !item.container);
            entry.3.push(item_id);
        }
    }
    for url in pr_order {
        let (linked, open, merged, item_ids) = &observations[url];
        let mut seen = HashSet::new();
        for item in item_ids.iter().filter(|id| seen.insert(**id)).filter_map(|id| items_by_id.get(*id)) {
            if *merged && (!item.completed || item.linear_completed == Some(false)) {
                entries.push(HealthEntry::MergedPrOnOpenItem { item_id: item.id.clone(), url: url.to_owned() });
            }
        }
        if !*linked && (*open || *merged) {
            entries.push(HealthEntry::UnlinkedPr { url: url.to_owned() });
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

    #[tokio::test]
    async fn default_branch_cache_reuses_results_and_backs_off_failures() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = DefaultBranchCache::default();
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            assert_eq!(cache.get_with("repo", || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Some("main".into())
            }).await.as_deref(), Some("main"));
            assert_eq!(cache.get_with("missing", || async {
                calls.fetch_add(1, Ordering::SeqCst);
                None
            }).await, None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

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
        let facts = health_items(&snapshot, "root", &[("child".to_owned(), true)].into_iter().collect());
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].linear_completed, None);
        assert_eq!(facts[1].linear_completed, Some(true));
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
    fn multiple_chats_reporting_one_pr_do_not_duplicate_or_mislabel_it() {
        let unlinked = HealthPullRequest { url: "pr".into(), item_id: None, linked_to_ticket: false, open: true, merged: false };
        let linked = HealthPullRequest { linked_to_ticket: true, ..unlinked.clone() };
        assert!(check_health(&[], &[], &[unlinked.clone(), linked.clone()]).is_empty());
        assert!(check_health(&[], &[], &[linked, unlinked.clone()]).is_empty());
        assert_eq!(check_health(&[], &[], &[unlinked.clone(), unlinked]).len(), 1);
        let item = HealthItem { id: "item".into(), container: false, completed: false, linear_completed: None, chat_ids: vec![] };
        let merged = HealthPullRequest { url: "pr".into(), item_id: Some("item".into()), linked_to_ticket: true, open: false, merged: true };
        assert_eq!(check_health(&[item], &[], &[merged.clone(), merged]).len(), 1);
        let container = HealthItem { id: "container".into(), container: true, completed: false, linear_completed: None, chat_ids: vec![] };
        let on_container = HealthPullRequest { url: "pr".into(), item_id: Some("container".into()), linked_to_ticket: false, open: true, merged: false };
        assert!(matches!(&check_health(&[container], &[], &[on_container])[..], [HealthEntry::UnlinkedPr { .. }]));
    }

    #[test]
    fn chats_on_the_same_linear_issue_share_one_cleanup_entry() {
        let item = |id: &str, chat: &str| HealthItem { id: id.into(), container: false, completed: false, linear_completed: Some(false), chat_ids: vec![chat.into()] };
        let chat = |id: &str, item: &str| HealthChat { id: id.into(), item_id: Some(item.into()), archived: false, finished: false,
            stale: false, has_open_pr: Some(false), on_default_branch: false, has_bound_worktree: false };
        let mut items = vec![item("a", "one"), item("b", "two"), item("local", "three")];
        let mut chats = vec![chat("one", "a"), chat("two", "b"), chat("three", "local")];
        let item_issues = [("a".into(), "EX-1".into()), ("b".into(), "EX-1".into())].into_iter().collect();
        let chat_issues = [("three".into(), "EX-1".into())].into_iter().collect();
        let issues = [("EX-1".into(), false)].into_iter().collect();
        reconcile_health_chats(&mut items, &mut chats, &item_issues, &chat_issues, &issues);
        assert!(matches!(&check_health(&items, &chats, &[])[..], [HealthEntry::SeveralLiveChats { item_id, chat_ids }] if item_id == "a" && chat_ids == &["one", "three", "two"]));
        let mut shared = vec![HealthItem { id: "shared".into(), container: false, completed: false, linear_completed: None, chat_ids: vec!["one".into(), "two".into()] }];
        let mut workers = vec![chat("one", "shared"), chat("two", "shared")];
        let branches = [("one".into(), "EX-1".into()), ("two".into(), "EX-2".into())].into_iter().collect();
        let both = [("EX-1".into(), false), ("EX-2".into(), false)].into_iter().collect();
        reconcile_health_chats(&mut shared, &mut workers, &HashMap::new(), &branches, &both);
        assert!(matches!(&check_health(&shared, &workers, &[])[..], [HealthEntry::SeveralLiveChats { item_id, .. }] if item_id == "shared"));
        let mut outside = vec![item("x", "one"), item("y", "two"), item("z", "three")];
        let mut outside_chats = vec![chat("one", "x"), chat("two", "y"), chat("three", "z")];
        let refs = [("x".into(), "OUT-900".into()), ("y".into(), "OUT-900".into())].into_iter().collect();
        let branches = [("three".into(), "OUT-900".into())].into_iter().collect();
        reconcile_health_chats(&mut outside, &mut outside_chats, &refs, &branches, &issues);
        assert!(matches!(&check_health(&outside, &outside_chats, &[])[..], [HealthEntry::SeveralLiveChats { item_id, .. }] if item_id == "x"));
    }

    #[test]
    fn branch_issue_matches_only_known_identifiers_at_boundaries() {
        let issues = ["LONGTEAM-123".to_owned()].into_iter().collect();
        assert_eq!(branch_issue("feature/longteam-123-fix", &issues).as_deref(), Some("LONGTEAM-123"));
        assert_eq!(branch_issue("feature/longteam-1234-fix", &issues), None);
        assert_eq!(branch_issue("xlongteam-123-fix", &issues), None);
    }

    #[test]
    fn chat_facts_use_camel_case_and_tolerate_missing_fields() {
        let facts = HealthChatFacts { idle_seconds: 90, blocked: Some(HealthBlock::OpenRequest), live_children: 2 };
        assert_eq!(serde_json::to_value(&facts).unwrap(), serde_json::json!({ "idleSeconds": 90, "blocked": "openRequest", "liveChildren": 2 }));
        let idle: HealthChatFacts = serde_json::from_value(serde_json::json!({ "idleSeconds": 5 })).unwrap();
        assert_eq!(idle, HealthChatFacts { idle_seconds: 5, blocked: None, live_children: 0 });
        assert_eq!(serde_json::to_value(&idle).unwrap(), serde_json::json!({ "idleSeconds": 5, "liveChildren": 0 }));
        assert_eq!(HealthBlock::StandingRole.note(), "holds a Manager or assistant role");
        // A block this version does not know still arrives as a block, not a parse failure.
        let newer: HealthChatFacts = serde_json::from_value(serde_json::json!({ "idleSeconds": 5, "blocked": "somethingNew" })).unwrap();
        assert_eq!(newer.blocked, Some(HealthBlock::Unknown));
    }

    #[test]
    fn unknown_pr_state_does_not_mark_a_chat_stale() {
        let chat = HealthChat { id: "chat".into(), item_id: None, archived: false, finished: false, stale: true,
            has_open_pr: None, on_default_branch: false, has_bound_worktree: false };
        assert!(check_health(&[], &[chat], &[]).is_empty());
    }
}
