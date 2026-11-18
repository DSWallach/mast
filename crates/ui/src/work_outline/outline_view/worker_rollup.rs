//! Cached worker counts for outline items.
use std::collections::{HashMap, HashSet};

use mast_proto::{ChatIndicator, WorkOutlineSnapshot};
use mast_proto::work_outline_user_actions::{UserActionKind, UserActionPriority};
use super::super::model::OutlineChat;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WorkerRollup {
    pub live: usize,
    pub needs_you: bool,
    pub coordinator: Option<String>,
}

/// Build once per snapshot/chat input change; row renderers only read the cache.
pub(super) fn build(snapshot: &WorkOutlineSnapshot, chats: &[OutlineChat]) -> HashMap<String, WorkerRollup> {
    let chats: HashMap<_, _> = chats.iter().filter(|chat| !chat.archived && chat.space_id.as_deref() == Some(&snapshot.space_id)).map(|chat| (chat.id.as_str(), chat)).collect();
    let dangling: HashSet<_> = snapshot.assignments.iter().filter(|assignment| assignment.state == mast_proto::WorkItemChatAssignmentState::DanglingChat).map(|assignment| assignment.chat_id.as_str()).collect();
    let items: HashMap<_, _> = snapshot.items.iter().filter(|item| item.archived_at.is_none()).map(|item| (item.id.as_str(), item)).collect();
    let mut assigned = HashMap::new();
    let mut rollups = HashMap::new();
    for item in items.values() {
        let coordinator = item.chat_ids.iter().filter(|id| !dangling.contains(id.as_str())).filter_map(|id| chats.get(id.as_str()).copied()).min_by_key(|chat| chat.parent_chat_id.is_some()).map(|chat| chat.id.clone());
        for id in &item.chat_ids {
            if !dangling.contains(id.as_str()) && chats.contains_key(id.as_str()) { assigned.entry(id.as_str()).or_insert(item.id.as_str()); }
        }
        rollups.insert(item.id.clone(), WorkerRollup { coordinator, ..Default::default() });
    }
    let waiting: HashSet<_> = snapshot.user_action_requests.iter().filter(|request| {
        request.is_unresolved() && request.effective_kind() != UserActionKind::Notice
            && request.priority != Some(UserActionPriority::Fyi)
            && !(request.effective_kind() == UserActionKind::Handoff && request.priority != Some(UserActionPriority::Blocking))
            && request.priority != Some(UserActionPriority::Normal)
    }).map(|request| request.chat_id.as_str()).collect();
    // Totals propagate bottom-up, so even deep trees cost O(items log items + chats).
    let mut totals: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut direct: HashMap<&str, (usize, usize, usize, usize)> = HashMap::new();
    for chat in chats.values() {
        let origin = assigned.get(chat.id.as_str()).copied().or_else(|| chat.parent_chat_id.as_deref().and_then(|parent| assigned.get(parent).copied()));
        let Some(origin) = origin.filter(|id| items.contains_key(id)) else { continue };
        let done = items.get(origin).is_some_and(|item| item.effective_status == mast_proto::WorkItemEffectiveStatus::Completed);
        let needs_you = !done && (chat.status == ChatIndicator::AwaitingInput || waiting.contains(chat.id.as_str()));
        let total = totals.entry(origin).or_default();
        total.0 += 1;
        total.1 += usize::from(needs_you);
        let entry = direct.entry(origin).or_default();
        entry.0 += 1;
        entry.1 += usize::from(needs_you);
        let coordinator = rollups.get(origin).and_then(|rollup| rollup.coordinator.as_deref());
        if chat.parent_chat_id.is_some() && chat.parent_chat_id.as_deref() == coordinator {
            entry.2 += 1;
            entry.3 += usize::from(needs_you);
        }
    }
    let mut ordered: Vec<_> = items.values().copied().collect();
    ordered.sort_by_key(|item| std::cmp::Reverse(item.depth));
    for item in ordered {
        let total = totals.get(item.id.as_str()).copied().unwrap_or_default();
        let own = direct.get(item.id.as_str()).copied().unwrap_or_default();
        if let Some(rollup) = rollups.get_mut(&item.id) {
            rollup.live = total.0 - own.0 + own.2;
            rollup.needs_you = total.1 - own.1 + own.3 > 0;
        }
        if let Some(parent) = item.parent_work_item_id.as_deref().filter(|id| items.contains_key(id)) {
            let entry = totals.entry(parent).or_default();
            entry.0 += total.0;
            entry.1 += total.1;
        }
    }
    rollups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::model::tests::chat;

    #[test]
    fn counts_descendants_and_child_chats_without_counting_coordinator() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId":"s", "available":true, "items":[
                {"id":"root", "spaceId":"s", "chatIds":["coordinator"], "childIds":["child"]},
                {"id":"child", "spaceId":"s", "parentWorkItemId":"root", "depth":1, "chatIds":["worker"]}
            ], "userActionRequests":[{"id":"ask", "spaceId":"s", "workItemId":"child", "chatId":"worker", "kind":"ask", "text":"Choose"}]
        })).unwrap();
        let coordinator = chat("coordinator", Some("s"));
        let mut worker = chat("worker", Some("s"));
        worker.parent_chat_id = Some("coordinator".into());
        let mut unassigned = chat("unassigned", Some("s"));
        unassigned.parent_chat_id = Some("coordinator".into());
        let mut archived = chat("archived", Some("s"));
        archived.parent_chat_id = Some("coordinator".into());
        archived.archived = true;
        let rows = build(&snapshot, &[coordinator, worker, unassigned, archived]);
        assert_eq!(rows["root"].live, 2);
        assert!(rows["root"].needs_you);
        assert_eq!(rows["root"].coordinator.as_deref(), Some("coordinator"));
    }

    #[test]
    fn item_without_coordinator_counts_descendant_chats() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId":"s", "available":true, "items":[
                {"id":"root", "spaceId":"s", "childIds":["child"]},
                {"id":"child", "spaceId":"s", "parentWorkItemId":"root", "depth":1, "chatIds":["worker"]}
            ]
        })).unwrap();
        let rows = build(&snapshot, &[chat("worker", Some("s"))]);
        assert_eq!(rows["root"].live, 1);
        assert_eq!(rows["root"].coordinator, None);
    }
}
