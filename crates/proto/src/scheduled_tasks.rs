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
    /// The run snapshot's Write to Notion setting, for the agent environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_to_notion: Option<bool>,
    /// Replies to this finished run continue in this chat instead of opening
    /// a new follow-up chat. Set per run from its composer; default off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub followups_in_place: bool,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledTaskAfterSuccess {
    #[default]
    Archive,
    Keep,
}

/// How an interactive task chooses the chat for each run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledTaskChatMode {
    #[default]
    NewChat,
    ContinueChat,
}

impl ScheduledTaskChatMode {
    fn is_new_chat(&self) -> bool { *self == Self::NewChat }
}

/// When a continuing chat should be handed off to a fresh successor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduledTaskRefresh {
    Weekly,
    ContextHeavy,
    #[default]
    Never,
}

impl ScheduledTaskRefresh {
    fn is_never(&self) -> bool { *self == Self::Never }
}

impl ScheduledTaskAfterSuccess {
    fn is_archive(&self) -> bool {
        *self == Self::Archive
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
    /// Applies only to Background runs and is captured in each run snapshot.
    #[serde(default, skip_serializing_if = "ScheduledTaskAfterSuccess::is_archive")]
    pub after_success: ScheduledTaskAfterSuccess,
    /// Existing tasks continue to start a new chat until explicitly changed.
    #[serde(default, skip_serializing_if = "ScheduledTaskChatMode::is_new_chat")]
    pub chat_mode: ScheduledTaskChatMode,
    #[serde(default, skip_serializing_if = "ScheduledTaskRefresh::is_never")]
    pub refresh: ScheduledTaskRefresh,
    /// Whether the task's procedure may write to Notion. `None` is a task
    /// saved before the setting existed: runs carry no Notion instruction and
    /// keep their prior behavior. New and imported tasks start `Some(false)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_to_notion: Option<bool>,
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
    /// Empty assistant designation observed when first selecting this run's
    /// target; used to avoid overwriting a subsequent user choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_generation: Option<String>,
    /// Source of a rotation selected by this run, retained for retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_source_chat_id: Option<String>,
    pub task_revision: u64,
    pub snapshot: ScheduledTaskDraft,
    /// The input request the run is parked on, and when the scheduler first saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_since: Option<i64>,
    /// The results artifact this Background run promised to publish. Absent
    /// for Interactive runs and for runs launched before the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<ScheduledRunResults>,
    /// When the run's chat was permanently deleted by retention. The row, its
    /// outcome and timestamps stay; the transcript and `detail` are gone, so
    /// views show the run without a link to the chat. Absent for every run
    /// that still has its chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged_at: Option<i64>,
}

/// Role prefix of a task's results artifact. The suffix is a hash of the
/// task id, so the role stays within the artifact role limit.
pub const RESULTS_ROLE_PREFIX: &str = "scheduled-results:";

/// Identity of a Background task's results artifact on its work item.
/// There is one per task and item; every run adds one revision to it.
/// Consumers find it by `role` (stable across task renames), never by title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledRunResults {
    pub artifact_id: String,
    pub title: String,
    pub role: String,
    /// The revision this run published, set once the run is verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

impl ScheduledRunResults {
    /// Roles compare trimmed and lowercased, exactly as `artifact_id` does.
    pub fn normalize_role(role: &str) -> String {
        role.trim().to_lowercase()
    }

    /// Whether `role` is in the reserved results namespace, in any case.
    pub fn is_results_role(role: &str) -> bool {
        Self::normalize_role(role).starts_with(RESULTS_ROLE_PREFIX)
    }

    pub fn role_for(task_id: &str) -> String {
        format!(
            "{RESULTS_ROLE_PREFIX}{}",
            crate::work_outline::derive_id(task_id, "results")
        )
    }

    pub fn title_for(task_name: &str) -> String {
        crate::artifacts::clean_title(&format!("Results: {}", task_name.trim()))
    }

    pub fn new(space_id: &str, work_item_id: &str, task_id: &str, task_name: &str) -> Self {
        let title = Self::title_for(task_name);
        let role = Self::role_for(task_id);
        Self {
            artifact_id: crate::artifacts::artifact_id(
                space_id,
                Some(work_item_id),
                None,
                &title,
                Some(&role),
            ),
            title,
            role,
            revision: None,
        }
    }

    /// Start of every Required Action dedupe key a scheduled run raises.
    pub const NEED_DEDUPE_PREFIX: &'static str = "scheduled-need:";

    /// Start of the dedupe key of the Required Action the scheduler raises for a
    /// failed run; the task id follows.
    pub const FAILED_DEDUPE_PREFIX: &'static str = "scheduled-run-failed:";

    /// The task identity inside a results role (the same hash its Required
    /// Action keys carry), for a normalized role.
    pub fn key_of_role(role: &str) -> Option<String> {
        role.strip_prefix(RESULTS_ROLE_PREFIX).map(str::to_owned)
    }

    /// [`Self::dedupe_prefix`] from [`Self::key_of_role`]'s key.
    pub fn need_prefix_for_key(key: &str) -> String {
        format!("{}{key}:", Self::NEED_DEDUPE_PREFIX)
    }

    /// Prefix of the Required Action dedupe keys this task's runs raise.
    pub fn dedupe_prefix(task_id: &str) -> String {
        format!(
            "{}{}:",
            Self::NEED_DEDUPE_PREFIX,
            crate::work_outline::derive_id(task_id, "results")
        )
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<ScheduledRunCleanup>,
    /// Whether new interactive chats on this device start with the one-line
    /// briefing index. Absent from older hosts, which read as off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub briefing_index: bool,
    /// Days after archiving that this device permanently deletes the chat of
    /// an archived scheduled run. Absent means Off, which is also what hosts
    /// that predate the setting read as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledRunCleanup {
    pub tasks: Vec<ScheduledRunCleanupTask>,
    /// False for previews and when eligibility changed before confirmation.
    pub applied: bool,
}

impl ScheduledRunCleanup {
    pub fn chat_ids(&self) -> impl Iterator<Item = &String> {
        self.tasks.iter().flat_map(|task| &task.chat_ids)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledRunCleanupTask {
    pub task_id: String,
    pub task_name: String,
    pub space_id: Option<String>,
    pub count: usize,
    pub chat_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ScheduledRunCleanupMode {
    Preview,
    Apply { expected_chat_ids: Vec<String> },
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
    Cleanup {
        #[serde(flatten)]
        mode: ScheduledRunCleanupMode,
    },
    /// Switch the chat-start briefing index. Hosts without
    /// `briefing-read-v1` reject the action, so clients gate it.
    SetBriefingIndex {
        enabled: bool,
    },
    /// Set how many days an archived scheduled run's chat is kept before it is
    /// permanently deleted; `None` turns deletion Off. Hosts without
    /// `scheduled-run-retention-v1` reject the action, so clients gate it.
    SetRetention {
        #[serde(default)]
        days: Option<u32>,
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
    latest_task_chats(chats, false)
        .filter(|chat| !chat.is_background_task_run())
        .map(|chat| chat.id.as_str())
        .collect()
}

/// One run per task for the sidebar's Scheduled section: the newest run of each
/// (device, Space, task), whatever its output mode or archive state, so a task
/// keeps its row when its latest run is Background or archived. Retired tasks
/// have no row. `order` holds task IDs the user has placed; the rest follow,
/// newest run first.
pub fn scheduled_sidebar_runs<'a>(
    chats: impl IntoIterator<Item = &'a crate::Chat>,
    order: &[String],
) -> Vec<&'a crate::Chat> {
    let mut latest = std::collections::HashMap::new();
    for chat in chats {
        let Some(task) = chat.scheduled_task_info.as_ref().filter(|t| !t.retired) else {
            continue;
        };
        let key = (&chat.device_id, &chat.space_id, &task.task_id);
        let previous: &mut &crate::Chat = latest.entry(key).or_insert(chat);
        if task_chat_order(chat) > task_chat_order(previous) {
            *previous = chat;
        }
    }
    let rank = |chat: &crate::Chat| {
        let id = chat.scheduled_task_info.as_ref().map(|t| t.task_id.as_str());
        order
            .iter()
            .position(|placed| Some(placed.as_str()) == id)
            .unwrap_or(usize::MAX)
    };
    let mut runs: Vec<_> = latest.into_values().collect();
    runs.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| task_chat_order(b).cmp(&task_chat_order(a)))
    });
    runs
}

/// The same grouping and ordering for cleanup, where the newest edition is
/// protected regardless of output mode unless a separate rule makes it eligible.
pub fn latest_task_chat_ids_for_cleanup(chats: &[crate::Chat]) -> std::collections::HashSet<&str> {
    latest_task_chats(chats, true)
        .map(|chat| chat.id.as_str())
        .collect()
}

fn latest_task_chats(
    chats: &[crate::Chat],
    include_retired: bool,
) -> impl Iterator<Item = &crate::Chat> {
    let mut latest = std::collections::HashMap::new();
    for chat in chats.iter().filter(|c| !c.archived) {
        let Some(task) = chat.scheduled_task_info.as_ref().filter(|t| include_retired || !t.retired) else {
            continue;
        };
        let key = (&chat.device_id, &chat.space_id, &task.task_id);
        let previous: &mut &crate::Chat = latest.entry(key).or_insert(chat);
        if task_chat_order(chat) > task_chat_order(previous) {
            *previous = chat;
        }
    }
    latest.into_values()
}

/// Runs omitted from default views, including background and retired runs,
/// plus chats a standing role has rotated away from. Run history still
/// reaches the runs, and `read_chat` still reaches the predecessors.
pub fn hidden_task_chat_ids(chats: &[crate::Chat]) -> std::collections::HashSet<&str> {
    let latest = latest_task_chat_ids(chats);
    chats
        .iter()
        .filter(|c| {
            c.is_standing_role_predecessor()
                || (c.scheduled_task_info.is_some() && !latest.contains(c.id.as_str()))
        })
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
    fn standing_role_predecessors_are_hidden_with_old_runs() {
        let mut old_role_chat = chat("rotated", "a", "x", 0);
        old_role_chat.scheduled_task_info = None;
        old_role_chat.standing_role_successor = Some("current".into());
        let mut current = chat("current", "a", "x", 0);
        current.scheduled_task_info = None;
        let chats = vec![old_role_chat, current];
        assert_eq!(
            hidden_task_chat_ids(&chats),
            std::collections::HashSet::from(["rotated"])
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
        assert!(background.get("afterSuccess").is_none());
        assert_eq!(
            serde_json::from_value::<ScheduledTaskDraft>(background).unwrap(),
            draft
        );
        assert_eq!(draft.after_success, ScheduledTaskAfterSuccess::Archive);
        draft.after_success = ScheduledTaskAfterSuccess::Keep;
        let background = serde_json::to_value(&draft).unwrap();
        assert_eq!(background["afterSuccess"], "keep");
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
            latest_task_chat_ids_for_cleanup(&chats),
            std::collections::HashSet::from(["background-new", "interactive-new"])
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

    #[test]
    fn a_run_row_without_results_reads_and_writes_unchanged() {
        let results = ScheduledRunResults::new("space", "item", "task", "Slack");
        assert!(results.role.len() <= crate::artifacts::artifact_limits::MAX_ROLE_CHARS);
        let long = "x".repeat(128);
        assert!(
            ScheduledRunResults::role_for(&long).len()
                <= crate::artifacts::artifact_limits::MAX_ROLE_CHARS
        );
        let mut value = serde_json::to_value(ScheduledRunResults::new("s", "i", "t", "n")).unwrap();
        assert!(value.get("revision").is_none());
        value["revision"] = 3.into();
        let decoded: ScheduledRunResults = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.revision, Some(3));
    }

    fn sample_run(results: Option<ScheduledRunResults>) -> ScheduledRun {
        ScheduledRun {
            id: "run".into(),
            task_id: "task".into(),
            scheduled_for: 1,
            created_at: 2,
            finished_at: None,
            manual: false,
            state: ScheduledRunState::Queued,
            detail: None,
            chat_id: Some("chat".into()),
            assistant_generation: None,
            rotation_source_chat_id: None,
            task_revision: 1,
            snapshot: ScheduledTaskDraft {
                name: "Slack".into(),
                prompt: "Go".into(),
                cwd: "/".into(),
                space_id: Some("space".into()),
                config: ChatConfig {
                    approval_rules: false,
                    harness: crate::HarnessId::Mock,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: crate::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                },
                schedule: TaskSchedule::Once { at: 3 },
                enabled: false,
                grace_minutes: 0,
                cutover_confirmed: false,
                work_item_id: Some("item".into()),
                output: ScheduledTaskOutput::Background,
                after_success: Default::default(),
                chat_mode: Default::default(),
                refresh: Default::default(),
                write_to_notion: None,
            },
            awaiting_request_id: None,
            awaiting_since: None,
            results,
            purged_at: None,
        }
    }

    /// The reader before the results contract: the same run without `results`.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyScheduledRun {
        id: String,
        task_id: String,
        scheduled_for: i64,
        created_at: i64,
        finished_at: Option<i64>,
        manual: bool,
        state: ScheduledRunState,
        detail: Option<String>,
        chat_id: Option<String>,
        task_revision: u64,
        snapshot: ScheduledTaskDraft,
        #[serde(default)]
        awaiting_request_id: Option<String>,
        #[serde(default)]
        awaiting_since: Option<i64>,
    }

    #[test]
    fn an_old_run_payload_reads_in_the_new_reader_without_results() {
        let with = sample_run(Some(ScheduledRunResults::new("space", "item", "task", "Slack")));
        let mut old = serde_json::to_value(&with).unwrap();
        old.as_object_mut().unwrap().remove("results");
        let decoded: ScheduledRun = serde_json::from_value(old).unwrap();
        assert_eq!(decoded, sample_run(None));
    }

    #[test]
    fn legacy_task_defaults_to_new_chat_and_omits_new_settings() {
        let legacy = serde_json::to_value(sample_run(None).snapshot).unwrap();
        assert!(legacy.get("chatMode").is_none());
        assert!(legacy.get("refresh").is_none());
        let draft: ScheduledTaskDraft = serde_json::from_value(legacy).unwrap();
        assert_eq!(draft.chat_mode, ScheduledTaskChatMode::NewChat);
        assert_eq!(draft.refresh, ScheduledTaskRefresh::Never);
    }

    #[test]
    fn a_new_run_payload_reads_in_the_legacy_reader_and_omits_empty_results() {
        let with = sample_run(Some(ScheduledRunResults::new("space", "item", "task", "Slack")));
        let json = serde_json::to_value(&with).unwrap();
        assert!(json.get("results").is_some());
        let legacy: LegacyScheduledRun = serde_json::from_value(json).unwrap();
        assert_eq!(legacy.id, "run");
        assert_eq!(legacy.snapshot, with.snapshot);
        // A run with no promise serializes exactly as before the contract.
        let without = serde_json::to_value(sample_run(None)).unwrap();
        assert!(without.get("results").is_none());
        let again: LegacyScheduledRun = serde_json::from_value(without).unwrap();
        assert_eq!(again.chat_id.as_deref(), Some("chat"));
    }

    #[test]
    fn reserved_roles_compare_trimmed_and_lowercased() {
        let role = ScheduledRunResults::role_for("task");
        let shouted = format!("  {}  ", role.to_uppercase());
        assert!(ScheduledRunResults::is_results_role(&shouted));
        assert_eq!(ScheduledRunResults::normalize_role(&shouted), role);
        assert!(!ScheduledRunResults::is_results_role("doc"));
    }

    #[test]
    fn briefing_index_wire_shape_is_compatible_both_ways() {
        let request = ScheduledTasksRequest::SetBriefingIndex { enabled: true };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"action": "setBriefingIndex", "enabled": true})
        );
        // Off is omitted, so an older client never sees the field; a snapshot
        // from an older host has none and reads as off.
        let off = serde_json::to_value(ScheduledTasksSnapshot::default()).unwrap();
        assert!(off.get("briefingIndex").is_none());
        let mut on = ScheduledTasksSnapshot::default();
        on.briefing_index = true;
        assert_eq!(serde_json::to_value(&on).unwrap()["briefingIndex"], true);
        let old: ScheduledTasksSnapshot =
            serde_json::from_value(serde_json::json!({"deviceId": "d", "tasks": [], "runs": []})).unwrap();
        assert!(!old.briefing_index);
    }

    #[test]
    fn retention_wire_shape_is_compatible_both_ways() {
        let set = |days| serde_json::to_value(ScheduledTasksRequest::SetRetention { days }).unwrap();
        assert_eq!(set(Some(14)), serde_json::json!({"action": "setRetention", "days": 14}));
        assert_eq!(set(None), serde_json::json!({"action": "setRetention", "days": null}));
        let bare: ScheduledTasksRequest =
            serde_json::from_value(serde_json::json!({"action": "setRetention"})).unwrap();
        assert!(matches!(bare, ScheduledTasksRequest::SetRetention { days: None }));
        // Off is omitted from the snapshot; a snapshot from a host without the
        // setting has none and reads as off.
        let off = serde_json::to_value(ScheduledTasksSnapshot::default()).unwrap();
        assert!(off.get("retentionDays").is_none());
        let mut on = ScheduledTasksSnapshot::default();
        on.retention_days = Some(14);
        assert_eq!(serde_json::to_value(&on).unwrap()["retentionDays"], 14);
        let old: ScheduledTasksSnapshot =
            serde_json::from_value(serde_json::json!({"deviceId": "d", "tasks": [], "runs": []})).unwrap();
        assert_eq!(old.retention_days, None);
    }

    #[test]
    fn the_purged_marker_is_absent_until_set_and_older_runs_read_as_kept() {
        let mut run = sample_run(None);
        assert!(serde_json::to_value(&run).unwrap().get("purgedAt").is_none());
        run.purged_at = Some(9);
        assert_eq!(serde_json::to_value(&run).unwrap()["purgedAt"], 9);
        let mut legacy = serde_json::to_value(sample_run(None)).unwrap();
        legacy.as_object_mut().unwrap().remove("purgedAt");
        let read: ScheduledRun = serde_json::from_value(legacy).unwrap();
        assert_eq!(read.purged_at, None);
    }

    #[test]
    fn need_keys_share_the_published_prefix() {
        let role = ScheduledRunResults::role_for("task");
        let key = ScheduledRunResults::key_of_role(&role).unwrap();
        assert_eq!(
            ScheduledRunResults::need_prefix_for_key(&key),
            ScheduledRunResults::dedupe_prefix("task")
        );
        assert_eq!(ScheduledRunResults::key_of_role("plan"), None);
        assert!(ScheduledRunResults::dedupe_prefix("task").starts_with(ScheduledRunResults::NEED_DEDUPE_PREFIX));
    }
}
