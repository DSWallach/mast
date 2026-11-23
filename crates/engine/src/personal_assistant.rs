//! The device's personal assistant chat: the device-local designation store.
//!
//! User RPCs set or clear the designation and can move it during a hand-off.
//! Authenticate re-reads it on every agent
//! call, so a clear takes effect immediately.

use crate::handoff_role_file::HandoffRoleFile;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use mast_proto::{Chat, PersonalAssistant};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct StoredAssistant {
    #[serde(flatten)]
    assistant: PersonalAssistant,
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Default)]
struct State {
    current: Option<StoredAssistant>,
    generation: String,
    store: HandoffRoleFile,
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
        if let Some((saved, generation)) = state.store.load_with_generation::<Option<StoredAssistant>>(path) {
            state.current = saved;
            state.generation = generation.unwrap_or_default();
        }
    }

    fn persist(state: &State) {
        if let Err(error) = state.store.save_with_generation(&state.current, Some(&state.generation)) {
            tracing::warn!(%error, "could not save personal assistant designation");
        }
    }

    /// Persist the empty designation's generation before recording a launch.
    /// A later user set or clear invalidates this observation, even if the
    /// designation is empty again when recovery runs.
    pub(crate) fn observe_empty_generation(&self) -> io::Result<Option<String>> {
        let mut state = lock(&self.state);
        if state.current.is_some() { return Ok(None); }
        if state.generation.is_empty() { state.generation = crate::new_id(); }
        state.store.save_with_generation(&state.current, Some(&state.generation))?;
        Ok(Some(state.generation.clone()))
    }

    pub(crate) fn set_if_empty_generation(&self, chat: &Chat, observed: &str) -> io::Result<bool> {
        let mut state = lock(&self.state);
        if state.current.is_some() || state.generation != observed { return Ok(false); }
        let current = Some(StoredAssistant {
            assistant: PersonalAssistant::for_chat(chat, chrono::Utc::now().timestamp_millis()),
            revision: Some(crate::new_id()),
        });
        let generation = crate::new_id();
        state.store.save_with_generation(&current, Some(&generation))?;
        state.current = current;
        state.generation = generation;
        Ok(true)
    }

    /// Designate `chat` (and its scheduled task, if it is a run), replacing
    /// any previous assistant.
    pub fn set(&self, chat: &Chat) -> PersonalAssistant {
        let assistant =
            PersonalAssistant::for_chat(chat, chrono::Utc::now().timestamp_millis());
        let mut state = lock(&self.state);
        state.current = Some(StoredAssistant {
            assistant: assistant.clone(),
            revision: Some(crate::new_id()),
        });
        state.generation = crate::new_id();
        Self::persist(&state);
        assistant
    }

    pub(crate) fn observe_hand_off(&self, from: &Chat, chats: &[Chat]) -> Option<StoredAssistant> {
        lock(&self.state)
            .current
            .as_ref()
            .filter(|entry| entry.assistant.covers(from, chats))
            .cloned()
    }

    pub(crate) fn transfer_for_hand_off(
        &self,
        from: &Chat,
        successor: &str,
        chats: &[Chat],
        observed: Option<&StoredAssistant>,
    ) -> io::Result<()> {
        let mut state = lock(&self.state);
        if state.store.contains(successor) {
            return Ok(());
        }
        let mut current = state.current.clone();
        if current.as_ref() == observed
            && let Some(current) = &mut current
            && current.assistant.covers(from, chats)
        {
            current.assistant.chat_id = successor.to_owned();
            current.assistant.scheduled_task = None;
            current.revision = Some(crate::new_id());
        }
        let generation = if current != state.current { crate::new_id() } else { state.generation.clone() };
        state.store.complete_with_generation(successor, &current, Some(&generation))?;
        state.current = current;
        state.generation = generation;
        Ok(())
    }

    pub fn clear(&self) {
        let mut state = lock(&self.state);
        state.current = None;
        state.generation = crate::new_id();
        Self::persist(&state);
    }

    pub fn get(&self) -> Option<PersonalAssistant> {
        lock(&self.state)
            .current
            .as_ref()
            .map(|entry| entry.assistant.clone())
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
    fn empty_observation_survives_restart_and_an_intervening_clear_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-assistant.json");
        let store = PersonalAssistantStore::default();
        store.set_store(path.clone());
        let generation = store.observe_empty_generation().unwrap().unwrap();
        drop(store);
        let reopened = PersonalAssistantStore::default();
        reopened.set_store(path.clone());
        reopened.clear();
        drop(reopened);
        let recovered = PersonalAssistantStore::default();
        recovered.set_store(path);
        assert!(!recovered.set_if_empty_generation(&chat("candidate", None), &generation).unwrap());
        assert!(recovered.get().is_none());
    }

    #[test]
    fn legacy_reader_can_load_a_designation_with_generation_metadata() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LegacySnapshot {
            roles: Option<StoredAssistant>,
            completed_handoffs: std::collections::HashSet<String>,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-assistant.json");
        let store = PersonalAssistantStore::default();
        store.set_store(path.clone());
        store.set(&chat("assistant", None));
        let legacy: LegacySnapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(legacy.roles.unwrap().assistant.chat_id, "assistant");
        assert!(legacy.completed_handoffs.is_empty());
    }

    #[test]
    fn legacy_writer_dropping_generation_invalidates_an_observation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-assistant.json");
        let store = PersonalAssistantStore::default();
        store.set_store(path.clone());
        let observed = store.observe_empty_generation().unwrap().unwrap();
        drop(store);
        std::fs::write(&path, br#"{"roles":null,"completedHandoffs":[]}"#).unwrap();
        let reopened = PersonalAssistantStore::default();
        reopened.set_store(path);
        assert!(!reopened.set_if_empty_generation(&chat("candidate", None), &observed).unwrap());
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
