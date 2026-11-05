//! Durable wake records. File operations are called from a blocking worker.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::WakeEvent;

const EVENT_CAP: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WakeStore {
    pub orchestrators: HashMap<String, Vec<StoredEvent>>,
    #[serde(default)]
    pub completed_ids: HashMap<String, Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::WakeEventKind;

    fn event(at_ms: i64) -> StoredEvent {
        StoredEvent {
            event: WakeEvent {
                id: at_ms.to_string(),
                key: at_ms.to_string(),
                worker_chat_id: "worker".into(),
                worker_title: None,
                kind: WakeEventKind::WorkerReport,
                summary: String::new(),
                report: Some("report".into()),
                at_ms,
            },
            message_id: None,
            delivered: false,
            retired_messages: Vec::new(),
        }
    }

    #[test]
    fn limits_each_orchestrator_by_oldest_event() {
        let mut store = WakeStore::default();
        store
            .orchestrators
            .insert("chat".into(), (0..503).rev().map(event).collect());
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
    fn completed_identity_survives_store_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wakes.json");
        let mut store = WakeStore::default();
        store.remember_completed("chat", "1");
        store.write(&path).unwrap();
        let saved = WakeStore::read(&path);
        let mut core = super::super::WakeCore::new();
        for id in &saved.completed_ids["chat"] {
            core.remember_delivered_id("chat", id, 10);
        }
        assert!(!core.record("chat", event(1).event, 11));
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
        store
            .orchestrators
            .insert("chat".into(), vec![queued, dispatched, lost]);
        let held = store.reconcile(
            "chat",
            &HashSet::from(["queued".into()]),
            &HashSet::from(["dispatched".into()]),
        );
        assert_eq!(held.iter().map(|e| e.at_ms).collect::<Vec<_>>(), vec![3]);
        assert_eq!(store.orchestrators["chat"].len(), 2);
        assert_eq!(
            store.orchestrators["chat"][0].message_id.as_deref(),
            Some("queued")
        );
    }

    #[test]
    fn transfer_is_idempotent_and_reholds_entries() {
        let mut store = WakeStore::default();
        let mut record = event(1);
        record.message_id = Some("message".into());
        store.orchestrators.insert("source".into(), vec![record]);
        store.transfer("source", "destination", &HashSet::from(["1".into()]));
        store.transfer("source", "destination", &HashSet::from(["1".into()]));
        assert_eq!(store.orchestrators["destination"].len(), 1);
        assert!(store.orchestrators["destination"][0].message_id.is_none());
    }

    #[test]
    fn transfer_keeps_dispatched_events_with_the_source() {
        let mut store = WakeStore::default();
        let mut dispatched = event(1);
        dispatched.message_id = Some("message".into());
        store
            .orchestrators
            .insert("source".into(), vec![dispatched, event(2)]);
        store.transfer("source", "destination", &HashSet::from(["2".into()]));
        assert_eq!(store.orchestrators["source"][0].event.id, "1");
        assert_eq!(store.orchestrators["destination"][0].event.id, "2");
    }

    #[test]
    fn synchronization_retains_queued_events_and_removes_cancelled_held_events() {
        let mut store = WakeStore::default();
        let held = HashMap::from([("chat".into(), vec![event(1).event, event(2).event])]);
        store.synchronize_held(&held);
        store.mark_queued("chat", &HashSet::from(["1".into()]), "message");
        store.synchronize_held(&HashMap::new());
        let entries = &store.orchestrators["chat"];
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event.id, "1");
        assert_eq!(entries[0].message_id.as_deref(), Some("message"));
    }

    #[test]
    fn restored_event_clears_queue_receipt() {
        let mut store = WakeStore::default();
        let held = HashMap::from([("chat".into(), vec![event(1).event])]);
        store.synchronize_held(&held);
        store.mark_queued("chat", &HashSet::from(["1".into()]), "message");
        store.synchronize_held(&held);
        assert!(store.orchestrators["chat"][0].message_id.is_none());
    }

    #[test]
    fn repeated_cancellation_retains_all_unflushed_queue_identities() {
        let mut store = WakeStore::default();
        let held = HashMap::from([("chat".into(), vec![event(1).event])]);
        store.synchronize_held(&held);
        store.mark_queued("chat", &HashSet::from(["1".into()]), "message-1");
        store.synchronize_held(&held);
        store.mark_queued("chat", &HashSet::from(["1".into()]), "message-2");
        store.synchronize_held(&held);
        assert_eq!(store.orchestrators["chat"][0].retired_messages.len(), 2);
    }

    #[test]
    fn measure_full_store_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wakes.json");
        let mut store = WakeStore::default();
        let entries = (0..500)
            .map(|number| {
                let mut entry = event(number);
                entry.event.report = Some("x".repeat(4096));
                entry
            })
            .collect();
        store.orchestrators.insert("chat".into(), entries);
        let started = std::time::Instant::now();
        store.write(&path).unwrap();
        eprintln!("500 wake records: {:?}", started.elapsed());
        assert_eq!(WakeStore::read(&path).orchestrators["chat"].len(), 500);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredEvent {
    pub event: WakeEvent,
    /// Queue and transcript messages retain the same id.
    pub message_id: Option<String>,
    #[serde(default)]
    pub delivered: bool,
    /// Queue rows removed before their removal was durably snapshotted.
    #[serde(default)]
    pub retired_messages: Vec<RetiredMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RetiredMessage {
    pub chat_id: String,
    pub message_id: String,
}

impl StoredEvent {
    pub fn retire(&mut self, chat: &str, message: &str) {
        if !self
            .retired_messages
            .iter()
            .any(|entry| entry.chat_id == chat && entry.message_id == message)
        {
            self.retired_messages.push(RetiredMessage {
                chat_id: chat.to_owned(),
                message_id: message.to_owned(),
            });
            if self.retired_messages.len() > EVENT_CAP {
                self.retired_messages.remove(0);
                tracing::warn!(%chat, "discarded oldest retired wake identity at capacity");
            }
        }
    }
}

impl WakeStore {
    pub fn remember_completed(&mut self, chat: &str, id: &str) {
        let ids = self.completed_ids.entry(chat.to_owned()).or_default();
        if !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_owned());
        }
        if ids.len() > EVENT_CAP {
            let count = ids.len() - EVENT_CAP;
            ids.drain(..count);
            tracing::warn!(%chat, count, "discarded oldest completed wake identities at capacity");
        }
    }
    /// Update held events while retaining queue receipts until acknowledged.
    pub fn synchronize_held(&mut self, held: &HashMap<String, Vec<WakeEvent>>) {
        for (chat, entries) in &mut self.orchestrators {
            entries.retain(|entry| {
                entry.message_id.is_some()
                    || held
                        .get(chat)
                        .is_some_and(|events| events.iter().any(|event| event.id == entry.event.id))
            });
        }
        for (chat, events) in held {
            let entries = self.orchestrators.entry(chat.clone()).or_default();
            for event in events {
                if let Some(entry) = entries.iter_mut().find(|entry| entry.event.id == event.id) {
                    if let Some(message) = entry.message_id.clone() {
                        entry.retire(chat, &message);
                    }
                    entry.message_id = None;
                    entry.delivered = false;
                } else {
                    entries.push(StoredEvent {
                        event: event.clone(),
                        message_id: None,
                        delivered: false,
                        retired_messages: Vec::new(),
                    });
                }
            }
        }
        self.limit();
    }

    pub fn mark_queued(&mut self, chat: &str, event_ids: &HashSet<String>, message_id: &str) {
        if let Some(entries) = self.orchestrators.get_mut(chat) {
            for entry in entries
                .iter_mut()
                .filter(|entry| event_ids.contains(&entry.event.id))
            {
                entry.message_id = Some(message_id.to_owned());
                entry.delivered = false;
            }
        }
    }

    pub fn mark_delivered(&mut self, chat: &str, message_id: &str) {
        if let Some(entries) = self.orchestrators.get_mut(chat) {
            for entry in entries
                .iter_mut()
                .filter(|entry| entry.message_id.as_deref() == Some(message_id))
            {
                entry.delivered = true;
            }
        }
    }

    /// Reconcile receipts before restoring held events. Queue entries remain
    /// durable until their corresponding transcript entry appears.
    #[cfg(test)]
    pub fn reconcile(
        &mut self,
        chat: &str,
        queue: &HashSet<String>,
        transcript: &HashSet<String>,
    ) -> Vec<WakeEvent> {
        let Some(events) = self.orchestrators.get_mut(chat) else {
            return Vec::new();
        };
        events.retain(|entry| {
            !entry
                .message_id
                .as_ref()
                .is_some_and(|id| transcript.contains(id))
        });
        let mut held = Vec::new();
        for entry in events {
            if entry
                .message_id
                .as_ref()
                .is_none_or(|id| !queue.contains(id))
            {
                entry.message_id = None;
                entry.delivered = false;
                held.push(entry.event.clone());
            }
        }
        held
    }

    pub fn transfer(&mut self, from: &str, to: &str, held_ids: &HashSet<String>) {
        if from == to {
            return;
        }
        if let Some(ids) = self.completed_ids.get(from).cloned() {
            for id in ids {
                self.remember_completed(to, &id);
            }
        }
        let Some(entries) = self.orchestrators.remove(from) else {
            return;
        };
        let (remaining, entries): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| entry.delivered || !held_ids.contains(&entry.event.id));
        if !remaining.is_empty() {
            self.orchestrators.insert(from.to_owned(), remaining);
        }
        let destination = self.orchestrators.entry(to.to_owned()).or_default();
        for mut entry in entries {
            if !destination
                .iter()
                .any(|existing| existing.event.id == entry.event.id)
            {
                if let Some(message) = entry.message_id.clone() {
                    entry.retire(from, &message);
                }
                entry.message_id = None;
                entry.delivered = false;
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
