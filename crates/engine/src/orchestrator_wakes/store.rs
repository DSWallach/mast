//! Durable wake records. File operations are called from a blocking worker.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::WakeEvent;

const EVENT_CAP: usize = 500;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct WakeStore {
    pub orchestrators: HashMap<String, Vec<StoredEvent>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::WakeEventKind;

    fn event(at_ms: i64) -> StoredEvent {
        StoredEvent {
            event: WakeEvent {
                id: at_ms.to_string(), key: at_ms.to_string(), worker_chat_id: "worker".into(),
                worker_title: None, kind: WakeEventKind::WorkerReport, summary: String::new(),
                report: Some("report".into()), at_ms,
            },
            message_id: None,
        }
    }

    #[test]
    fn limits_each_orchestrator_by_oldest_event() {
        let mut store = WakeStore::default();
        store.orchestrators.insert("chat".into(), (0..503).rev().map(event).collect());
        store.limit();
        let events = &store.orchestrators["chat"];
        assert_eq!(events.len(), 500);
        assert_eq!(events[0].event.at_ms, 3);
    }

    #[test]
    fn missing_and_corrupt_store_are_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wakes.json");
        assert!(WakeStore::read(&path).orchestrators.is_empty());
        std::fs::write(&path, b"{").unwrap();
        assert!(WakeStore::read(&path).orchestrators.is_empty());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredEvent {
    pub event: WakeEvent,
    /// Queue and transcript messages retain the same id.
    pub message_id: Option<String>,
}

impl WakeStore {
    pub fn read(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "invalid wake store");
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read wake store");
                Self::default()
            }
        }
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let staging = path.with_extension("pending-tmp");
        let mut file = std::fs::File::create(&staging)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        std::fs::rename(staging, path)
    }

    pub fn limit(&mut self) {
        for (chat, events) in &mut self.orchestrators {
            if events.len() > EVENT_CAP {
                events.sort_by_key(|event| event.event.at_ms);
                let count = events.len() - EVENT_CAP;
                events.drain(..count);
                tracing::warn!(%chat, count, "discarded oldest pending wake events at capacity");
            }
        }
        self.orchestrators.retain(|_, events| !events.is_empty());
    }
}
