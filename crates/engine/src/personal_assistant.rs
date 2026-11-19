//! The device's personal assistant chat: the device-local designation store.
//!
//! User RPCs set or clear the designation and can move it during a hand-off.
//! Authenticate re-reads it on every agent
//! call, so a clear takes effect immediately.

use std::path::PathBuf;
use std::sync::Mutex;

use mast_proto::{Chat, PersonalAssistant};

#[derive(Default)]
struct State {
    current: Option<PersonalAssistant>,
    store: Option<PathBuf>,
}

#[derive(Default)]
pub struct PersonalAssistantStore {
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl PersonalAssistantStore {
    /// Persist at `path` (device-local) and load the saved designation.
    pub fn set_store(&self, path: PathBuf) {
        let mut state = lock(&self.state);
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(saved) = serde_json::from_slice::<Option<PersonalAssistant>>(&bytes)
        {
            state.current = saved;
        }
        state.store = Some(path);
    }

    fn persist(state: &State) {
        if let Some(path) = &state.store
            && let Ok(bytes) = serde_json::to_vec(&state.current)
        {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Designate `chat` (and its scheduled task, if it is a run), replacing
    /// any previous assistant.
    pub fn set(&self, chat: &Chat) -> PersonalAssistant {
        let assistant =
            PersonalAssistant::for_chat(chat, chrono::Utc::now().timestamp_millis());
        let mut state = lock(&self.state);
        state.current = Some(assistant.clone());
        Self::persist(&state);
        assistant
    }

    pub(crate) fn transfer_for_hand_off(&self, from: &Chat, successor: &str, chats: &[Chat]) {
        let mut state = lock(&self.state);
        if let Some(assistant) = &state.current
            && assistant.covers(from, chats)
        {
            state.current = Some(PersonalAssistant {
                chat_id: successor.to_owned(),
                designated_at_ms: assistant.designated_at_ms,
                scheduled_task: None,
            });
            Self::persist(&state);
        }
    }

    pub fn clear(&self) {
        let mut state = lock(&self.state);
        if state.current.take().is_some() {
            Self::persist(&state);
        }
    }

    pub fn get(&self) -> Option<PersonalAssistant> {
        lock(&self.state).current.clone()
    }

    /// `chat` is the assistant. `chats` is read only for a scheduled run the
    /// designation does not match directly (a merged imported task).
    pub fn is_assistant(&self, chat: &Chat, chats: impl FnOnce() -> Vec<Chat>) -> bool {
        let Some(assistant) = self.get() else {
            return false;
        };
        assistant.covers(chat, &[])
            || (assistant.scheduled_task.is_some()
                && chat.scheduled_task_info.is_some()
                && assistant.covers(chat, &chats()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, task: Option<&str>) -> Chat {
        let mut chat = serde_json::json!({
            "id": id, "deviceId": "d1", "archived": false, "spaceId": "s1",
            "createdAt": "2026-11-01T00:00:00Z"
        });
        if let Some(task) = task {
            chat["scheduledTaskInfo"] =
                serde_json::json!({ "taskId": task, "taskName": "Briefing", "runId": id });
        }
        serde_json::from_value(chat).unwrap()
    }

    #[test]
    fn set_replaces_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-assistant.json");
        let store = PersonalAssistantStore::default();
        store.set_store(path.clone());
        store.set(&chat("a", None));
        store.set(&chat("b", Some("t1")));
        assert!(!store.is_assistant(&chat("a", None), Vec::new));
        assert!(store.is_assistant(&chat("b", Some("t1")), Vec::new));
        // A later run of the same task is the assistant too.
        assert!(store.is_assistant(&chat("c", Some("t1")), Vec::new));

        let reloaded = PersonalAssistantStore::default();
        reloaded.set_store(path.clone());
        assert_eq!(reloaded.get().map(|a| a.chat_id), Some("b".into()));
        assert!(reloaded.is_assistant(&chat("c", Some("t1")), Vec::new));

        reloaded.clear();
        let cleared = PersonalAssistantStore::default();
        cleared.set_store(path);
        assert_eq!(cleared.get(), None);
    }

    #[test]
    fn a_native_run_of_a_merged_designated_task_is_the_assistant() {
        let store = PersonalAssistantStore::default();
        store.set(&chat("imported", Some("legacy")));
        let mut merged = chat("imported", Some("native"));
        merged.scheduled_task_info.as_mut().unwrap().legacy_task_id = Some("legacy".into());
        let native_run = chat("native-run", Some("native"));
        let chats = vec![merged, native_run.clone()];
        assert!(store.is_assistant(&native_run, || chats.clone()));
        assert!(!store.is_assistant(&chat("other", Some("other")), || chats.clone()));
        // A direct match never reads the chat list.
        assert!(store.is_assistant(&chat("imported", None), || unreachable!()));
    }
}
