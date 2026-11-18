//! Reconcile project issues, local items and worker branches into ticket rows.
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalTicket {
    pub item_id: String,
    pub issue_identifier: Option<String>,
    pub chat_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchChat {
    pub chat_id: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketSourceRow {
    pub issue_identifier: Option<String>,
    pub item_ids: Vec<String>,
    pub chat_ids: Vec<String>,
}

/// The caller supplies only leaf items and eligible worker chats. Every issue
/// and every unmatched local item is represented once, in source order.
pub(crate) fn reconcile_tickets(
    issue_identifiers: &[String],
    items: &[LocalTicket],
    branch_chats: &[BranchChat],
) -> Vec<TicketSourceRow> {
    let mut rows = Vec::new();
    let mut issues = HashMap::new();
    for identifier in issue_identifiers {
        let key = identifier.to_ascii_uppercase();
        if issues.contains_key(&key) { continue; }
        issues.insert(key, rows.len());
        rows.push(TicketSourceRow { issue_identifier: Some(identifier.clone()), item_ids: Vec::new(), chat_ids: Vec::new() });
    }
    let branch_matches: HashMap<_, _> = branch_chats.iter().filter_map(|chat|
        branch_issue(&chat.branch, &issues).map(|index| (chat.chat_id.as_str(), index))).collect();
    let mut assigned_chats = HashSet::new();
    for item in items {
        let index = item.issue_identifier.as_ref().and_then(|id| issues.get(&id.to_ascii_uppercase()).copied())
            .or_else(|| item.issue_identifier.is_none().then(|| item.chat_ids.iter().find_map(|id| branch_matches.get(id.as_str()).copied())).flatten())
            .unwrap_or_else(|| {
                rows.push(TicketSourceRow { issue_identifier: None, item_ids: Vec::new(), chat_ids: Vec::new() });
                rows.len() - 1
            });
        rows[index].item_ids.push(item.item_id.clone());
        for chat_id in &item.chat_ids {
            if assigned_chats.insert(chat_id.clone()) { rows[index].chat_ids.push(chat_id.clone()); }
        }
    }
    for chat in branch_chats {
        if assigned_chats.contains(&chat.chat_id) { continue; }
        if let Some(&index) = branch_matches.get(chat.chat_id.as_str()) {
            assigned_chats.insert(chat.chat_id.clone());
            rows[index].chat_ids.push(chat.chat_id.clone());
        }
    }
    rows
}

fn branch_issue(branch: &str, issues: &HashMap<String, usize>) -> Option<usize> {
    let upper = branch.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_alphabetic() || (start > 0 && bytes[start - 1].is_ascii_alphanumeric()) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_alphanumeric() { end += 1; }
        if end < bytes.len() && bytes[end] == b'-' {
            let digits = end + 1;
            end = digits;
            while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
            if end > digits && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric()) {
                if let Some(index) = issues.get(&upper[start..end]) { return Some(*index); }
            }
        }
        start = end.max(start + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_issue_links_and_branch_matches_without_duplicate_chats() {
        let issues = vec!["ENG-12".into(), "ENG-13".into(), "ENG-12".into()];
        let items = vec![LocalTicket { item_id: "a".into(), issue_identifier: Some("eng-12".into()), chat_ids: vec!["one".into()] },
            LocalTicket { item_id: "b".into(), issue_identifier: Some("ENG-12".into()), chat_ids: vec!["two".into(), "one".into()] },
            LocalTicket { item_id: "c".into(), issue_identifier: Some("OTHER-1".into()), chat_ids: vec![] },
            LocalTicket { item_id: "d".into(), issue_identifier: None, chat_ids: vec!["four".into()] }];
        let chats = vec![BranchChat { chat_id: "three".into(), branch: "feature/eng-13-work".into() },
            BranchChat { chat_id: "one".into(), branch: "eng-13-work".into() },
            BranchChat { chat_id: "unmatched".into(), branch: "feature/eng-130-work".into() },
            BranchChat { chat_id: "four".into(), branch: "eng-13-detail".into() }];
        let rows = reconcile_tickets(&issues, &items, &chats);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].item_ids, ["a", "b"]);
        assert_eq!(rows[0].chat_ids, ["one", "two"]);
        assert_eq!(rows[1].item_ids, ["d"]);
        assert_eq!(rows[1].chat_ids, ["four", "three"]);
        assert_eq!(rows[2].item_ids, ["c"]);
        assert!(rows[2].issue_identifier.is_none());
    }

    #[test]
    fn local_items_work_without_linear() {
        let rows = reconcile_tickets(&[], &[LocalTicket { item_id: "item".into(), issue_identifier: None,
            chat_ids: vec!["worker".into()] }], &[]);
        assert_eq!(rows[0].item_ids, ["item"]);
        assert_eq!(rows[0].chat_ids, ["worker"]);
    }
}
