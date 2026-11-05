//! Durable wake records. File operations are called from a blocking worker.

use std::collections::{HashMap, HashSet};
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

    #[test]
    fn reconciliation_retains_queued_and_restores_unreceipted_events() {
        let mut store = WakeStore::default();
        let mut queued = event(1);
        queued.message_id = Some("queued".into());
        let mut dispatched = event(2);
        dispatched.message_id = Some("dispatched".into());
        let mut lost = event(3);
        lost.message_id = Some("lost".into());
        store.orchestrators.insert("chat".into(), vec![queued, dispatched, lost]);
        let held = store.reconcile("chat", &HashSet::from(["queued".into()]), &HashSet::from(["dispatched".into()]));
        assert_eq!(held.iter().map(|e| e.at_ms).collect::<Vec<_>>(), vec![3]);
        assert_eq!(store.orchestrators["chat"].len(), 2);
        assert_eq!(store.orchestrators["chat"][0].message_id.as_deref(), Some("queued"));
    }

    #[test]
    fn transfer_is_idempotent_and_reholds_entries() {
        let mut store = WakeStore::default();
        let mut record = event(1);
        record.message_id = Some("message".into());
        store.orchestrators.insert("source".into(), vec![record]);
        store.transfer("source", "destination");
        store.transfer("source", "destination");
        assert_eq!(store.orchestrators["destination"].len(), 1);
        assert!(store.orchestrators["destination"][0].message_id.is_none());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredEvent {
    pub event: WakeEvent,
    /// Queue and transcript messages retain the same id.
    pub message_id: Option<String>,
}

impl WakeStore {
    /// Reconcile receipts before restoring held events. Queue entries remain
    /// durable until their corresponding transcript entry appears.
    pub fn reconcile(&mut self, chat: &str, queue: &HashSet<String>, transcript: &HashSet<String>) -> Vec<WakeEvent> {
        let Some(events) = self.orchestrators.get_mut(chat) else { return Vec::new() };
        events.retain(|entry| !entry.message_id.as_ref().is_some_and(|id| transcript.contains(id)));
        let mut held = Vec::new();
        for entry in events {
            if entry.message_id.as_ref().is_none_or(|id| !queue.contains(id)) {
                entry.message_id = None;
                held.push(entry.event.clone());
            }
        }
        held
    }

    pub fn transfer(&mut self, from: &str, to: &str) {
        if from == to { return; }
        let Some(entries) = self.orchestrators.remove(from) else { return };
        let destination = self.orchestrators.entry(to.to_owned()).or_default();
        for mut entry in entries {
            if !destination.iter().any(|existing| existing.event.id == entry.event.id) {
                entry.message_id = None;
                destination.push(entry);
            }
        }
        self.limit();
    }

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
