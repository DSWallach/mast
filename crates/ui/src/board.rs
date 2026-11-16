//! Derived coordination board facts. This module does not fetch or persist data.
use crate::team::{self, TeamInput, WorkerFacts};
use mast_proto::{ToolCall, WorkItemEffectiveStatus, WorkOutlineSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BoardStatus {
    Todo,
    InProgress,
    InReview,
    Done,
}

impl BoardStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Todo => "To do",
            Self::InProgress => "In progress",
            Self::InReview => "In review",
            Self::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardItem {
    pub id: String,
    pub title: String,
    pub status: BoardStatus,
    pub updated_at: i64,
    pub created_at: i64,
    pub worker: Option<WorkerFacts>,
    pub worker_title: Option<String>,
    pub report: Option<String>,
    pub ticket: Option<String>,
    pub request_id: Option<String>,
}

/// Return one row per unarchived item, including items without a chat.
pub(crate) fn board(
    snapshot: &WorkOutlineSnapshot,
    root: Option<&str>,
    input: &TeamInput<'_>,
) -> Vec<BoardItem> {
    let scope = root.map(|id| {
        let mut ids = crate::work_outline::model::descendants(snapshot, id);
        ids.insert(id.to_owned());
        ids
    });
    snapshot
        .items
        .iter()
        .filter(|item| item.archived_at.is_none())
        .filter(|item| scope.as_ref().is_none_or(|ids| ids.contains(&item.id)))
        .map(|item| {
            let worker = item
                .chat_ids
                .iter()
                .filter_map(|id| {
                    input
                        .chats
                        .iter()
                        .find(|chat| &chat.id == id && !chat.archived)
                })
                .map(|chat| team::worker(chat, Some(&item.id), input))
                .max_by_key(|worker| worker.last_active);
            let status = if item.effective_status == WorkItemEffectiveStatus::Completed {
                BoardStatus::Done
            } else if snapshot.user_action_requests.iter().any(|request| {
                request.work_item_id == item.id
                    && request.is_open()
                    && !request.deduped
                    && request.effective_kind()
                        == mast_proto::work_outline_user_actions::UserActionKind::Handoff
            }) || worker.as_ref().is_some_and(|worker| {
                worker.ready.is_some()
                    || worker
                        .prs
                        .iter()
                        .any(|pr| pr.state.eq_ignore_ascii_case("open"))
            }) {
                BoardStatus::InReview
            } else if worker.is_some()
                && matches!(
                    item.effective_status,
                    WorkItemEffectiveStatus::Active | WorkItemEffectiveStatus::UserActionRequired
                )
            {
                BoardStatus::InProgress
            } else {
                BoardStatus::Todo
            };
            let worker_chat = worker
                .as_ref()
                .and_then(|worker| input.chats.iter().find(|chat| chat.id == worker.chat_id));
            let worker_title = worker_chat.and_then(|chat| chat.title.clone());
            let report = worker_chat
                .and_then(|chat| chat.last_message_preview.as_deref())
                .and_then(team::activity_line);
            BoardItem {
                request_id: worker.as_ref().and_then(|worker| snapshot.user_action_requests.iter().find(|request| request.chat_id == worker.chat_id && request.is_open() && !request.deduped && !matches!(request.effective_kind(), mast_proto::work_outline_user_actions::UserActionKind::Handoff | mast_proto::work_outline_user_actions::UserActionKind::Notice) && request.priority != Some(mast_proto::work_outline_user_actions::UserActionPriority::Fyi)).map(|request| request.id.clone())),
                worker_title,
                report,
                ticket: crate::chat_work_context::ticket(snapshot, &item.id)
                    .map(|ticket| ticket.identifier),
                id: item.id.clone(),
                title: item.title.clone(),
                status,
                updated_at: item.updated_at,
                created_at: item.created_at,
                worker,
            }
        })
        .collect()
}

/// Activity for a typed call, falling back to the existing preview otherwise.
pub(crate) fn activity(call: Option<&ToolCall>, preview: Option<&str>) -> Option<String> {
    let text = match call {
        Some(ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. }) => {
            Some(format!("editing {path}"))
        }
        Some(ToolCall::ApplyPatch { path }) => Some(format!(
            "editing {}",
            path.as_deref().unwrap_or("workspace")
        )),
        Some(ToolCall::Exec { command }) => Some(format!("running {command}")),
        _ => None,
    };
    text.as_deref().or(preview).and_then(team::activity_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_prefers_typed_calls_and_falls_back() {
        assert_eq!(
            activity(
                Some(&ToolCall::Exec {
                    command: "cargo test".into()
                }),
                Some("message")
            ),
            Some("running cargo test".into())
        );
        assert_eq!(
            activity(None, Some("  last message\nmore")),
            Some("last message".into())
        );
        assert_eq!(activity(None, None), None);
    }
    #[test]
    fn board_classifies_items_and_scopes_descendants() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId":"s", "available":true,
            "items":[
                {"id":"root", "spaceId":"s", "title":"Root", "childIds":["active", "done"]},
                {"id":"active", "spaceId":"s", "title":"Active", "parentWorkItemId":"root", "effectiveStatus":"active", "chatIds":["worker"]},
                {"id":"done", "spaceId":"s", "title":"Done", "parentWorkItemId":"root", "effectiveStatus":"completed"},
                {"id":"other", "spaceId":"s", "title":"Other"}
            ]
        })).unwrap();
        let chats: Vec<mast_proto::Chat> =
            vec![serde_json::from_value(serde_json::json!({
            "id":"worker", "deviceId":"local", "spaceId":"s", "archived":false, "createdAt":"2026-09-08T00:00:00Z"
        })).unwrap()];
        let indicators = [("worker".to_owned(), mast_proto::ChatIndicator::Working)]
            .into_iter()
            .collect();
        let links = Default::default();
        let input = TeamInput {
            chat_id: "",
            chats: &chats,
            indicators: &indicators,
            snapshot: Some(&snapshot),
            links: &links,
            now: chrono::Utc::now(),
        };
        let rows = board(&snapshot, Some("root"), &input);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].status, BoardStatus::Todo);
        assert_eq!(rows[1].status, BoardStatus::InProgress);
        assert_eq!(rows[2].status, BoardStatus::Done);
        assert_eq!(board(&snapshot, None, &input).len(), 4);
    }
    #[test]
    fn handoff_and_waiting_facts_drive_actions() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({
            "spaceId":"s", "available":true,
            "items":[
                {"id":"review", "spaceId":"s", "title":"Review", "effectiveStatus":"active", "chatIds":["reviewer"]},
                {"id":"waiting", "spaceId":"s", "title":"Waiting", "effectiveStatus":"userActionRequired", "chatIds":["worker"]}
            ],
            "userActionRequests":[
                {"id":"r", "spaceId":"s", "workItemId":"review", "chatId":"reviewer", "kind":"handoff", "state":"open", "text":"Ready to merge"},
                {"id":"w", "spaceId":"s", "workItemId":"waiting", "chatId":"worker", "kind":"ask", "state":"open", "text":"Choose one"}
            ]
        })).unwrap();
        let chats: Vec<mast_proto::Chat> = ["reviewer", "worker"].into_iter().map(|id| serde_json::from_value(serde_json::json!({"id":id,"deviceId":"local","spaceId":"s","archived":false,"createdAt":"2026-09-08T00:00:00Z"})).unwrap()).collect();
        let indicators = Default::default();
        let links = Default::default();
        let input = TeamInput {
            chat_id: "",
            chats: &chats,
            indicators: &indicators,
            snapshot: Some(&snapshot),
            links: &links,
            now: chrono::Utc::now(),
        };
        let rows = board(&snapshot, None, &input);
        assert_eq!(rows[0].status, BoardStatus::InReview);
        assert_eq!(
            rows[1].worker.as_ref().unwrap().state,
            team::WorkerState::Waiting
        );
    }
    #[test]
    fn most_recent_worker_supplies_pr_and_diff_facts() {
        use mast_engine::pr_ticket_cache::{ChatLinkStatus, DiffStat, PrStatus};
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({"spaceId":"s","items":[{"id":"i","spaceId":"s","title":"Item","effectiveStatus":"active","chatIds":["older","newer"]}]})).unwrap();
        let chats: Vec<mast_proto::Chat> = [("older", "2026-01-01T00:00:00Z"), ("newer", "2026-02-01T00:00:00Z")].into_iter().map(|(id, date)| serde_json::from_value(serde_json::json!({"id":id,"deviceId":"local","spaceId":"s","archived":false,"createdAt":date})).unwrap()).collect();
        let pr = PrStatus {
            number: 41,
            url: None,
            state: "OPEN".into(),
            is_draft: false,
            review_decision: None,
            reviewers: vec![],
            checks: None,
            title: None,
            branch: None,
            head_sha: None,
            mergeable: String::new(),
            has_reviewer_requested: false,
            merged_at: None,
        };
        let links = [(
            "newer".into(),
            ChatLinkStatus {
                pr: Some(pr),
                diff_stat: Some(DiffStat {
                    files_changed: 1,
                    lines_added: 12,
                    lines_removed: 3,
                }),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();
        let indicators = Default::default();
        let input = TeamInput {
            chat_id: "",
            chats: &chats,
            indicators: &indicators,
            snapshot: Some(&snapshot),
            links: &links,
            now: chrono::Utc::now(),
        };
        let rows = board(&snapshot, None, &input);
        let worker = rows[0].worker.as_ref().unwrap();
        assert_eq!(worker.chat_id, "newer");
        assert_eq!(worker.prs[0].number, 41);
        assert_eq!(worker.diff, Some((12, 3)));
        assert_eq!(rows[0].status, BoardStatus::InReview);
    }
}
