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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardItem {
    pub id: String,
    pub title: String,
    pub status: BoardStatus,
    pub updated_at: i64,
    pub worker: Option<WorkerFacts>,
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
            } else if worker.as_ref().is_some_and(|worker| {
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
            BoardItem {
                id: item.id.clone(),
                title: item.title.clone(),
                status,
                updated_at: item.updated_at,
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
}
