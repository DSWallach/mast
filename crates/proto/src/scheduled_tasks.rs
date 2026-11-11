//! Host-owned scheduled tasks. Credentials stay in the host's account store.
use crate::ChatConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledChat {
    pub task_id: String,
    pub task_name: String,
    pub run_id: String,
    #[serde(default)]
    pub session_created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TaskSchedule {
    Cron {
        expression: String,
        timezone: String,
    },
    Once {
        at: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskDraft {
    pub name: String,
    pub prompt: String,
    pub cwd: String,
    pub space_id: Option<String>,
    pub config: ChatConfig,
    pub schedule: TaskSchedule,
    #[serde(default)]
    pub enabled: bool,
    /// Zero skips missed runs; otherwise coalesce within this window.
    pub grace_minutes: u32,
    /// Explicit source handoff acknowledgment for an imported task.
    #[serde(default)]
    pub cutover_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: String,
    pub revision: u64,
    pub device_id: String,
    pub draft: ScheduledTaskDraft,
    pub created_at: i64,
    pub updated_at: i64,
    pub next_due: Option<i64>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub import_source: Option<String>,
    #[serde(default)]
    pub source_handoff_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledRunState {
    Launching,
    Queued,
    Running,
    AwaitingInput,
    Completed,
    Failed,
    Interrupted,
    Cancelled,
    Skipped,
}
impl ScheduledRunState {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Launching | Self::Queued | Self::Running | Self::AwaitingInput
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledRun {
    pub id: String,
    pub task_id: String,
    pub scheduled_for: i64,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub manual: bool,
    pub state: ScheduledRunState,
    pub detail: Option<String>,
    pub chat_id: Option<String>,
    pub task_revision: u64,
    pub snapshot: ScheduledTaskDraft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledImportCandidate {
    pub source: String,
    pub source_enabled: bool,
    pub draft: ScheduledTaskDraft,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTasksSnapshot {
    pub device_id: String,
    pub tasks: Vec<ScheduledTask>,
    pub runs: Vec<ScheduledRun>,
    #[serde(default)]
    pub candidates: Vec<ScheduledImportCandidate>,
    #[serde(default)]
    pub preview: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScheduledTasksRequest {
    List,
    Save {
        id: String,
        expected_revision: u64,
        draft: ScheduledTaskDraft,
        operation_id: String,
    },
    Delete {
        id: String,
        expected_revision: u64,
        operation_id: String,
    },
    RunNow {
        id: String,
        operation_id: String,
    },
    History {
        id: String,
        #[serde(default)]
        before: Option<i64>,
    },
    Preview {
        schedule: TaskSchedule,
    },
    ImportPreview {
        #[serde(default)]
        registry_path: Option<String>,
        timezone: String,
        space_id: Option<String>,
    },
    Import {
        candidate: ScheduledImportCandidate,
        operation_id: String,
    },
}

/// Default sidebar/overview projection; activity on an older run never promotes it.
pub fn latest_task_chat_ids(chats: &[crate::Chat]) -> std::collections::HashSet<&str> {
    let mut latest = std::collections::HashMap::new();
    for chat in chats.iter().filter(|c| !c.archived) {
        let Some(task) = &chat.scheduled_task_info else {
            continue;
        };
        let key = (&chat.device_id, &chat.space_id, &task.task_id);
        let previous: &mut &crate::Chat = latest.entry(key).or_insert(chat);
        if task_chat_order(chat) > task_chat_order(previous) {
            *previous = chat;
        }
    }
    latest.into_values().map(|c| c.id.as_str()).collect()
}

pub fn task_chat_order(chat: &crate::Chat) -> (i64, &str) {
    (
        chat.scheduled_task_info
            .as_ref()
            .and_then(|t| t.session_created_at)
            .unwrap_or(chat.created_at.timestamp_millis()),
        chat.id.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chat(id: &str, device: &str, task: &str, created: i64) -> crate::Chat {
        serde_json::from_value(serde_json::json!({"id":id,"deviceId":device,"archived":false,"createdAt":"2026-01-01T00:00:00Z","scheduledTaskInfo":{"taskId":task,"taskName":"Same display name","runId":id,"sessionCreatedAt":created}})).unwrap()
    }
    #[test]
    fn latest_is_creation_not_activity_and_is_source_scoped() {
        let mut older = chat("older", "a", "one", 10);
        older.last_message_at = Some(chrono::Utc::now());
        let chats = vec![
            older,
            chat("newer", "a", "one", 20),
            chat("same-name", "a", "two", 9),
            chat("other-device", "b", "one", 8),
        ];
        let ids = latest_task_chat_ids(&chats);
        assert!(!ids.contains("older"));
        assert_eq!(ids.len(), 3);
        assert!(ids.contains("newer"));
        assert!(ids.contains("same-name"));
        assert!(ids.contains("other-device"));
    }
    #[test]
    fn stable_ties() {
        let chats = vec![chat("a", "d", "t", 10), chat("b", "d", "t", 10)];
        assert_eq!(
            latest_task_chat_ids(&chats),
            std::collections::HashSet::from(["b"])
        );
    }
    #[test]
    fn legacy_name_marker_does_not_break_chat_decoding() {
        let mut value = serde_json::to_value(chat("old", "d", "t", 10)).unwrap();
        value.as_object_mut().unwrap().remove("scheduledTaskInfo");
        value["scheduledTask"] = serde_json::json!("legacy-task-name");
        let decoded: crate::Chat = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.id, "old");
        assert!(decoded.scheduled_task_info.is_none());
    }
}
