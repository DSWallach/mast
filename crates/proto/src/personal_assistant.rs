//! The personal assistant chat: one user-designated chat per device whose
//! agent tools may read every other chat, in every Space. The widening is
//! read-only (list, get, read); sends, interrupts and archives keep their
//! normal reach. Only the user sets or clears it (IPC RPCs); no agent tool
//! does.
//!
//! A scheduled-task run designates its task: every run of that task is the
//! assistant, and the sidebar shows (and opens) the latest one.

use serde::{Deserialize, Serialize};

use crate::Chat;

/// The device's personal assistant designation (device-local; never synced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAssistant {
    /// The chat the user picked.
    pub chat_id: String,
    pub designated_at_ms: i64,
    /// Set when that chat is a scheduled-task run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task: Option<PersonalAssistantTask>,
}

/// The scheduled task behind the designation, keyed like
/// [`crate::latest_task_chat_ids`] groups its runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAssistantTask {
    pub task_id: String,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
}

impl PersonalAssistant {
    pub fn for_chat(chat: &Chat, designated_at_ms: i64) -> Self {
        Self {
            chat_id: chat.id.clone(),
            designated_at_ms,
            scheduled_task: chat.scheduled_task_info.as_ref().map(|task| PersonalAssistantTask {
                task_id: task.task_id.clone(),
                device_id: chat.device_id.clone(),
                space_id: chat.space_id.clone(),
            }),
        }
    }

    /// The picked chat, or any run of its scheduled task.
    pub fn covers(&self, chat: &Chat) -> bool {
        if chat.id == self.chat_id {
            return true;
        }
        let (Some(task), Some(info)) = (&self.scheduled_task, &chat.scheduled_task_info) else {
            return false;
        };
        info.task_id == task.task_id
            && chat.device_id == task.device_id
            && chat.space_id == task.space_id
    }

    /// The chat the sidebar shows: the task's latest unarchived run, else the
    /// picked chat.
    pub fn current<'a>(&self, chats: &'a [Chat]) -> Option<&'a Chat> {
        if self.scheduled_task.is_some()
            && let Some(latest) = chats
                .iter()
                .filter(|c| !c.archived && c.scheduled_task_info.is_some() && self.covers(c))
                .max_by(|a, b| crate::task_chat_order(a).cmp(&crate::task_chat_order(b)))
        {
            return Some(latest);
        }
        chats.iter().find(|c| c.id == self.chat_id)
    }
}

/// Reply of `GetPersonalAssistant`, `SetPersonalAssistant` and
/// `ClearPersonalAssistant`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAssistantReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<PersonalAssistant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, task: &str, created_at: i64, space: &str) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": "d1", "title": id, "archived": false, "spaceId": space,
            "createdAt": "2026-11-01T00:00:00Z",
            "scheduledTaskInfo": {
                "taskId": task, "taskName": "Briefing", "runId": id,
                "sessionCreatedAt": created_at
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_scheduled_run_designates_its_task_and_resolves_to_the_latest_run() {
        let chats = vec![
            run("old", "t1", 1, "s1"),
            run("new", "t1", 3, "s1"),
            run("other-task", "t2", 5, "s1"),
            run("other-space", "t1", 9, "s2"),
        ];
        let assistant = PersonalAssistant::for_chat(&chats[0], 0);
        assert!(assistant.covers(&chats[1]));
        assert!(!assistant.covers(&chats[2]));
        assert!(!assistant.covers(&chats[3]));
        assert_eq!(assistant.current(&chats).map(|c| c.id.as_str()), Some("new"));
    }

    #[test]
    fn an_ordinary_chat_covers_only_itself() {
        let mut plain = run("plain", "t1", 1, "s1");
        plain.scheduled_task_info = None;
        let chats = vec![plain, run("run", "t1", 2, "s1")];
        let assistant = PersonalAssistant::for_chat(&chats[0], 0);
        assert!(!assistant.covers(&chats[1]));
        assert_eq!(assistant.current(&chats).map(|c| c.id.as_str()), Some("plain"));
    }

    #[test]
    fn a_saved_designation_without_a_task_still_loads() {
        let saved: PersonalAssistant =
            serde_json::from_str(r#"{"chatId":"c1","designatedAtMs":5}"#).unwrap();
        assert_eq!(saved.scheduled_task, None);
    }
}
