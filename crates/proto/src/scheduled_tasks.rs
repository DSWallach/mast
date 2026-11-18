//! Host-owned scheduled tasks. Credentials stay in the host's account store.
use crate::ChatConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledChat {
    pub task_id: String,
    pub task_name: String,
    pub run_id: String,
    #[serde(default)]
    pub session_created_at: Option<i64>,
    /// The identity an imported run carried before it joined its native task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_task_id: Option<String>,
    /// An imported run's prompt file (its tag's `file`), which the source
    /// registry row lists as `filePath`. `None` for native runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    /// The run's task was deleted or never became a native task; default
    /// views hide it, the Scheduled tasks page still lists it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retired: bool,
    /// Captured from the task's run snapshot when the chat is created.
    #[serde(default, skip_serializing_if = "ScheduledTaskOutput::is_interactive")]
    pub output: ScheduledTaskOutput,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledTaskOutput {
    #[default]
    Interactive,
    Background,
}

impl ScheduledTaskOutput {
    fn is_interactive(&self) -> bool {
        *self == Self::Interactive
    }
}

impl crate::Chat {
    pub fn is_background_task_run(&self) -> bool {
        self.scheduled_task_info
            .as_ref()
            .is_some_and(|task| task.output == ScheduledTaskOutput::Background)
    }
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
    /// Work Outline item each run is attached to; runs inherit its instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    // Keep the legacy wire shape for interactive tasks and their replay keys.
    #[serde(default, skip_serializing_if = "ScheduledTaskOutput::is_interactive")]
    pub output: ScheduledTaskOutput,
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
    /// The input request the run is parked on, and when the scheduler first saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_since: Option<i64>,
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
/// Retired tasks and latest runs with background output have no default row.
pub fn latest_task_chat_ids(chats: &[crate::Chat]) -> std::collections::HashSet<&str> {
    let mut latest = std::collections::HashMap::new();
    for chat in chats.iter().filter(|c| !c.archived) {
        let Some(task) = chat.scheduled_task_info.as_ref().filter(|t| !t.retired) else {
            continue;
        };
        let key = (&chat.device_id, &chat.space_id, &task.task_id);
        let previous: &mut &crate::Chat = latest.entry(key).or_insert(chat);
        if task_chat_order(chat) > task_chat_order(previous) {
            *previous = chat;
        }
    }
    latest
        .into_values()
        .filter(|c| !c.is_background_task_run())
        .map(|c| c.id.as_str())
        .collect()
}

/// Runs omitted from default views, including background and retired runs.
/// Run history still reaches them.
pub fn hidden_task_chat_ids(chats: &[crate::Chat]) -> std::collections::HashSet<&str> {
    let latest = latest_task_chat_ids(chats);
    chats
        .iter()
        .filter(|c| c.scheduled_task_info.is_some() && !latest.contains(c.id.as_str()))
        .map(|c| c.id.as_str())
        .collect()
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
    fn retired_runs_have_no_latest_and_older_runs_are_hidden() {
        let mut retired = chat("retired", "a", "gone", 30);
        retired.scheduled_task_info.as_mut().unwrap().retired = true;
        let mut plain = chat("plain", "a", "x", 0);
        plain.scheduled_task_info = None;
        let chats = vec![
            retired,
            chat("old", "a", "one", 10),
            chat("new", "a", "one", 20),
            plain,
        ];
        assert_eq!(
            latest_task_chat_ids(&chats),
            std::collections::HashSet::from(["new"])
        );
        assert_eq!(
            hidden_task_chat_ids(&chats),
            std::collections::HashSet::from(["retired", "old"])
        );
    }
    #[test]
    fn new_fields_are_additive_on_the_wire() {
        let info = ScheduledChat {
            task_id: "t".into(),
            task_name: "n".into(),
            run_id: "r".into(),
            ..Default::default()
        };
        let value = serde_json::to_value(&info).unwrap();
        assert!(
            value.get("retired").is_none()
                && value.get("legacyTaskId").is_none()
                && value.get("sourceFile").is_none()
        );
        let draft: serde_json::Value =
            serde_json::json!({"retired": true, "taskId": "t", "taskName": "n", "runId": "r"});
        assert!(
            serde_json::from_value::<ScheduledChat>(draft)
                .unwrap()
                .retired
        );
    }
    #[test]
    fn draft_output_defaults_to_interactive_and_uses_the_output_key() {
        let legacy = serde_json::json!({
            "name": "Briefing",
            "prompt": "Summarize changes",
            "cwd": "/work",
            "config": {"harness": "codex", "sandbox": "workspace-write"},
            "schedule": {"type": "once", "at": 10},
            "graceMinutes": 5,
            "workItemId": "item",
        });
        let mut draft: ScheduledTaskDraft = serde_json::from_value(legacy).unwrap();
        assert_eq!(draft.output, ScheduledTaskOutput::Interactive);
        let interactive = serde_json::to_value(&draft).unwrap();
        assert!(interactive.get("output").is_none());
        assert_eq!(
            serde_json::from_value::<ScheduledTaskDraft>(interactive).unwrap(),
            draft
        );

        draft.output = ScheduledTaskOutput::Background;
        let background = serde_json::to_value(&draft).unwrap();
        assert_eq!(background["output"], "background");
        assert_eq!(
            serde_json::from_value::<ScheduledTaskDraft>(background).unwrap(),
            draft
        );
    }

    #[test]
    fn background_runs_are_hidden_without_reviving_superseded_runs() {
        let mut background_new = chat("background-new", "d", "one", 30);
        let mut background_old = chat("background-old", "d", "two", 10);
        for run in [&mut background_new, &mut background_old] {
            run.scheduled_task_info.as_mut().unwrap().output = ScheduledTaskOutput::Background;
            let info = serde_json::to_value(run.scheduled_task_info.as_ref().unwrap()).unwrap();
            assert_eq!(info["output"], "background");
        }
        let chats = [
            chat("interactive-old", "d", "one", 20),
            background_new,
            background_old,
            chat("interactive-new", "d", "two", 20),
        ];
        assert_eq!(
            latest_task_chat_ids(&chats),
            std::collections::HashSet::from(["interactive-new"])
        );
        assert_eq!(
            hidden_task_chat_ids(&chats),
            std::collections::HashSet::from([
                "interactive-old",
                "background-new",
                "background-old"
            ])
        );
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
