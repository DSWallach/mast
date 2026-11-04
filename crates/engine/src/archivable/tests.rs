use serde_json::{Value, json};

use super::*;

fn chat(extra: Value) -> Chat {
    let mut base = json!({
        "id": "w1", "deviceId": "d", "createdAt": "2026-10-01T00:00:00Z",
        "archived": false, "spaceId": "s",
    });
    base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).expect("chat fixture")
}

fn item(status: &str, archived_at: Option<i64>) -> WorkItemView {
    serde_json::from_value(json!({
        "id": "i1", "spaceId": "s", "title": "t", "status": "active",
        "effectiveStatus": status, "archivedAt": archived_at,
    }))
    .expect("item fixture")
}

fn request(text: &str, state: &str) -> UserActionRequestView {
    serde_json::from_value(json!({
        "id": "r", "chatId": "w1", "workItemId": "i1", "text": text, "state": state,
    }))
    .expect("request fixture")
}

fn now() -> DateTime<Utc> {
    "2026-11-02T00:00:00Z".parse().unwrap()
}

struct Case {
    chat: Chat,
    indicator: ChatIndicator,
    queued: bool,
    item: Option<WorkItemView>,
    requests: Vec<UserActionRequestView>,
    prs: Vec<&'static str>,
}

impl Case {
    fn new() -> Self {
        Self {
            chat: chat(json!({ "lastMessageAt": "2026-11-01T00:00:00Z" })),
            indicator: ChatIndicator::Idle,
            queued: false,
            item: Some(item("active", None)),
            requests: Vec::new(),
            prs: Vec::new(),
        }
    }

    fn verdict(&self) -> Archivable {
        archivable(&ArchivableInput {
            chat: &self.chat,
            indicator: self.indicator,
            queued: self.queued,
            item: self.item.as_ref(),
            requests: self.requests.iter().collect(),
            pr_states: self.prs.iter().map(|s| s.to_string()).collect(),
            now: now(),
        })
    }
}

#[test]
fn an_open_item_with_nothing_else_is_not_finished() {
    let v = Case::new().verdict();
    assert!(!v.archivable, "{v:?}");
}

#[test]
fn a_completed_item_finishes_the_chat() {
    let mut c = Case::new();
    c.item = Some(item("completed", None));
    assert_eq!(c.verdict(), Archivable::yes("its item is completed"));
}

#[test]
fn a_merged_or_closed_pr_finishes_the_chat() {
    let mut c = Case::new();
    c.prs = vec!["MERGED"];
    assert_eq!(c.verdict(), Archivable::yes("its PR is merged"));
    c.prs = vec!["CLOSED"];
    assert!(c.verdict().archivable);
    c.prs = vec!["MERGED", "OPEN"];
    assert!(!c.verdict().archivable);
    c.prs = vec!["OPEN"];
    assert!(!c.verdict().archivable);
}

#[test]
fn a_resolved_ready_to_merge_finishes_the_chat() {
    let mut c = Case::new();
    c.requests = vec![request("Ready to merge: feature", "resolved")];
    assert_eq!(c.verdict(), Archivable::yes("its Ready to merge was resolved"));
    // A resolved ordinary question does not.
    c.requests = vec![request("Which colour?", "resolved")];
    assert!(!c.verdict().archivable);
    c.requests[0].kind = Some(mast_proto::work_outline_user_actions::UserActionKind::Handoff);
    assert!(c.verdict().archivable);
    c.requests[0].text = "Ready to merge: feature".into();
    c.requests[0].kind = Some(mast_proto::work_outline_user_actions::UserActionKind::Ask);
    assert!(!c.verdict().archivable);
}

#[test]
fn idle_for_days_needs_a_completed_or_archived_item() {
    let mut c = Case::new();
    c.chat = chat(json!({ "lastMessageAt": "2026-10-20T00:00:00Z" }));
    // Long idle but the item is still open: not finished.
    assert!(!c.verdict().archivable);
    c.item = Some(item("active", Some(5)));
    assert_eq!(c.verdict(), Archivable::yes("idle for more than 3 days"));
    // Archived item but the chat was active within the window.
    c.chat = chat(json!({ "lastMessageAt": "2026-10-31T12:00:00Z" }));
    assert!(!c.verdict().archivable);
}

#[test]
fn busy_chats_are_never_archivable() {
    let mut c = Case::new();
    c.item = Some(item("completed", None));
    c.indicator = ChatIndicator::Working;
    assert_eq!(c.verdict().reason, "still working");
    c.indicator = ChatIndicator::AwaitingInput;
    assert_eq!(c.verdict().reason, "waiting for input");
    c.indicator = ChatIndicator::Idle;
    c.queued = true;
    assert_eq!(c.verdict().reason, "has a queued message");
    c.queued = false;
    // An unseen completed turn is idle for this purpose.
    c.indicator = ChatIndicator::Completed;
    assert!(c.verdict().archivable);
}

#[test]
fn an_open_required_action_blocks_every_branch() {
    let mut c = Case::new();
    c.item = Some(item("completed", None));
    c.prs = vec!["MERGED"];
    c.requests = vec![request("Which colour?", "open")];
    assert_eq!(c.verdict().reason, "has an open Required Action");
}

#[test]
fn an_archived_chat_is_not_archivable_again() {
    let mut c = Case::new();
    c.item = Some(item("completed", None));
    c.chat = chat(json!({ "archived": true }));
    assert_eq!(c.verdict().reason, "already archived");
}

#[test]
fn the_log_numbers_entries_and_serves_since() {
    let log = AgentArchiveLog::default();
    for n in 0..3 {
        log.record(AgentArchive {
            space_id: "s".into(),
            chat_id: format!("c{n}"),
            chat_title: None,
            orchestrator_chat_id: "o".into(),
            orchestrator_title: None,
            reason: "r".into(),
            forced: false,
            at_ms: 0,
            seq: 0,
        });
    }
    assert_eq!(log.since(0).len(), 3);
    let after = log.since(1);
    assert_eq!(after.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
}
