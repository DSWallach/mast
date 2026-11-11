//! The device's personal assistant chat: the device-local designation store.
//!
//! Only the RPC layer (`SetPersonalAssistant`, `ClearPersonalAssistant`)
//! writes it; no agent tool does. Authenticate re-reads it on every agent
//! call, so a clear takes effect immediately.

use std::path::PathBuf;
use std::sync::Mutex;

use mast_proto::PersonalAssistant;

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

    /// Designate `chat_id`, replacing any previous assistant.
    pub fn set(&self, chat_id: &str) -> PersonalAssistant {
        let assistant = PersonalAssistant {
            chat_id: chat_id.to_owned(),
            designated_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let mut state = lock(&self.state);
        state.current = Some(assistant.clone());
        Self::persist(&state);
        assistant
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

    pub fn is_assistant(&self, chat_id: &str) -> bool {
        lock(&self.state)
            .current
            .as_ref()
            .is_some_and(|a| a.chat_id == chat_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_replaces_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-assistant.json");
        let store = PersonalAssistantStore::default();
        store.set_store(path.clone());
        store.set("a");
        store.set("b");
        assert!(!store.is_assistant("a"));
        assert!(store.is_assistant("b"));

        let reloaded = PersonalAssistantStore::default();
        reloaded.set_store(path.clone());
        assert_eq!(reloaded.get().map(|a| a.chat_id), Some("b".into()));

        reloaded.clear();
        let cleared = PersonalAssistantStore::default();
        cleared.set_store(path);
        assert_eq!(cleared.get(), None);
    }
}
