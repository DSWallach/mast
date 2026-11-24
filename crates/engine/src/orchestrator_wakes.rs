//! Worker-event wakes for orchestrator chats.
//!
//! An orchestrator is a chat whose workers are its child chats (`create_chat`
//! sets `parentChatId`) or chats on items under its project's worker
//! destination. The engine watches what those workers do, from the events it
//! already has: the Work Outline snapshot (Required Actions raised/resolved),
//! the session watch (errored, turn finished) and chat rows (PR linked). There
//! is no polling loop; the only timers are one-shot sleeps to the end of a
//! hold window.
//!
//! Events are batched per orchestrator ([`core::WAKE_WINDOW_MS`]) and delivered
//! as ONE system-attributed message, only while the orchestrator is idle and
//! the user is not in the chat. The user always goes first: a user message
//! pulls back a wake that has not started and interrupts an event-started
//! turn (see `DocHost::yield_event_turn_to_user`).

mod auto_continue;
mod core;
mod store;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::FutureExt;

use mast_doc::registry::final_handback::FinalHandbackIntent;
use mast_proto::{
    Chat, Session, SessionStatus, WakeEventKind, WakeSettings, WakeStatus, WorkOutlineActorKind,
    WorkOutlineSnapshot,
};

pub use self::core::{
    Batch, Ctx, DEDUPE_TTL_MS, Decision, USER_QUIET_MS, WAKE_WINDOW_MS, WakeCore, WakeEvent,
    resolved_key,
};
use crate::doc_host::{ChatDocHandle, DocHost, queue_text_hash};
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Epoch milliseconds.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| chrono::Utc::now().timestamp_millis())
}

fn materialize_due_outline<T>(
    space_id: &str,
    inputs: &mast_doc::WorkOutlineInputs,
    now: i64,
    materialize: impl FnOnce() -> T,
) -> Option<T> {
    let due = inputs.items.iter().any(|row| {
        row.fields.get("spaceId").and_then(|v| v.as_str()) == Some(space_id)
            && row.fields.get("waitingUntil").and_then(|v| v.as_i64()).is_some_and(|until| until <= now)
            && row.fields.get("archivedAt").and_then(|v| v.as_i64()).is_none()
            && row.fields.get("status").and_then(|v| v.as_str()) != Some("completed")
    });
    due.then(materialize)
}

#[cfg(test)]
mod waiting_outline_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn no_due_items_skip_materialization() {
        let mut inputs = mast_doc::WorkOutlineInputs::default();
        let mut row: mast_doc::RegistryRow = serde_json::from_value(serde_json::json!({
            "kind": "workItems", "id": "item", "fields": {
                "spaceId": "space", "waitingUntil": 101
            }
        })).unwrap();
        inputs.items.push(row.clone());
        let calls = Cell::new(0);
        let materialize = || { calls.set(calls.get() + 1); };
        assert_eq!(materialize_due_outline("space", &inputs, 100, materialize), None);
        assert_eq!(calls.get(), 0);
        row.fields.insert("waitingUntil".into(), serde_json::json!(100));
        inputs.items[0] = row;
        assert_eq!(materialize_due_outline("space", &inputs, 100, materialize), Some(()));
        assert_eq!(calls.get(), 1);
    }
}

// The original file was a flat map of chat ids to settings. Preserve it on
// read; missing autoContinue fields inherit the global default.
fn read_settings(bytes: &[u8]) -> (bool, HashMap<String, WakeSettings>) {
    let Ok(serde_json::Value::Object(mut root)) = serde_json::from_slice(bytes) else {
        return (false, HashMap::new());
    };
    let default = root
        .remove("autoContinue")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let entries = match root.remove("orchestrators") {
        Some(serde_json::Value::Object(entries)) => entries,
        _ => root,
    };
    let overrides = entries
        .into_iter()
        .filter_map(|(chat, mut value)| {
            if let Some(object) = value.as_object_mut() {
                object
                    .entry("autoContinue")
                    .or_insert(serde_json::Value::Bool(default));
            }
            serde_json::from_value(value)
                .ok()
                .map(|settings| (chat, settings))
        })
        .collect();
    (default, overrides)
}

#[derive(Debug)]
struct EventTurn {
    message_id: String,
    events: Vec<WakeEvent>,
    /// The drain took the queue row.
    dispatched: bool,
    /// The turn was seen in flight.
    started: bool,
}

/// Wake messages in flight, shared between the wake service and the doc host
/// (which makes way for the user).
#[derive(Debug, Default)]
pub struct EventTurns {
    turns: Mutex<HashMap<String, EventTurn>>,
    cancelled: Mutex<HashMap<String, Vec<WakeEvent>>>,
    persistence: Mutex<Option<WakePersistence>>,
}

#[derive(Clone)]
struct WakePersistence {
    path: PathBuf,
    store: store::WakeStore,
    generations: HashMap<(String, String), (Arc<crate::chat_persistence::ChatPersistence>, u64)>,
    retired_generations: HashMap<(String, String), (Arc<crate::chat_persistence::ChatPersistence>, u64)>,
    legacy_receipts: HashMap<(String, String), std::time::Instant>,
    legacy_retired: HashMap<(String, String), std::time::Instant>,
    unavailable: HashSet<String>,
}

impl std::fmt::Debug for WakePersistence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WakePersistence").field("path", &self.path).finish_non_exhaustive()
    }
}

impl WakePersistence {
    fn prune(&mut self) {
        let mut retired: Vec<_> = self.retired_generations.iter()
            .filter(|(_, (snapshot, generation))| snapshot.has_saved(*generation))
            .map(|(key, _)| key.clone()).collect();
        retired.extend(self.legacy_retired.iter()
            .filter(|(_, at)| at.elapsed() >= std::time::Duration::from_secs(3))
            .map(|(key, _)| key.clone()));
        for (chat, message) in retired {
            let mut referenced = false;
            for entries in self.store.orchestrators.values_mut() {
                for entry in entries {
                    if entry.message_id.as_deref() == Some(&message) { referenced = true; }
                    entry.retired_messages.retain(|retired| retired.chat_id != chat || retired.message_id != message);
                }
            }
            if !referenced {
                self.retired_generations.remove(&(chat.clone(), message.clone()));
                self.legacy_retired.remove(&(chat, message));
            }
        }
        for entries in self.store.orchestrators.values_mut() {
            entries.retain(|entry| !entry.discarded || entry.message_id.is_some() || !entry.retired_messages.is_empty());
        }
        let saved: Vec<_> = self.generations.iter()
            .filter(|(_, (snapshot, generation))| snapshot.has_saved(*generation))
            .map(|(key, _)| key.clone()).collect();
        for (chat, message) in saved {
            let mut completed = Vec::new();
            let mut handbacks = Vec::new();
            if let Some(entries) = self.store.orchestrators.get_mut(&chat) {
                entries.retain(|entry| {
                    let prune = entry.delivered && entry.retired_messages.is_empty() && entry.message_id.as_deref() == Some(&message);
                    if prune {
                        completed.push(entry.event.id.clone());
                        if let Some(receipt) = entry.handback_receipt(&chat) {
                            handbacks.push(receipt);
                        }
                    }
                    !prune
                });
            }
            for id in completed { self.store.remember_completed(&chat, &id); }
            for handback in handbacks { self.store.remember_handback(handback); }
            let retained = self.store.orchestrators.get(&chat).is_some_and(|entries| entries.iter().any(|entry| entry.delivered && entry.message_id.as_deref() == Some(&message)));
            if !retained { self.generations.remove(&(chat, message)); }
        }
        let expired: Vec<_> = self.legacy_receipts.iter()
            .filter(|(_, at)| at.elapsed() >= std::time::Duration::from_secs(3))
            .map(|(key, _)| key.clone()).collect();
        for (chat, message) in expired {
            let mut completed = Vec::new();
            if let Some(entries) = self.store.orchestrators.get_mut(&chat) {
                entries.retain(|entry| {
                    let prune = entry.delivered && entry.retired_messages.is_empty() && entry.message_id.as_deref() == Some(&message);
                    if prune { completed.push(entry.event.id.clone()); }
                    !prune
                });
            }
            for id in completed { self.store.remember_completed(&chat, &id); }
            let retained = self.store.orchestrators.get(&chat).is_some_and(|entries| entries.iter().any(|entry| entry.delivered && entry.message_id.as_deref() == Some(&message)));
            if !retained { self.legacy_receipts.remove(&(chat, message)); }
        }
    }
    fn write(&self) -> Result<(), String> {
        self.store.write(&self.path).map_err(|error| error.to_string())
    }
}

impl EventTurns {
    fn final_handback_receipt(&self, expected: &core::FinalHandback) -> Result<bool, String> {
        let guard = lock(&self.persistence);
        let persistence = guard.as_ref().ok_or("durable wake storage is unavailable")?;
        if !persistence.store.final_handback_history_ready {
            return Err("final handback delivery history is unavailable".into());
        }
        if persistence.store.cancelled_handbacks.contains(&expected.operation_id) {
            return Err("the Manager removed or edited the queued final report".into());
        }
        match persistence.store.delivered_handbacks.get(&expected.operation_id) {
            None => Ok(false),
            Some(Some(receipt)) if &receipt.handback == expected && receipt.is_verified() => Ok(true),
            Some(_) => Err("final handback receipt conflicts with this intent".into()),
        }
    }
    pub(crate) fn note_retired(&self, chat_id: &str, message_id: &str, snapshot: Option<&Arc<crate::chat_persistence::ChatPersistence>>) {
        if let Some(persistence) = lock(&self.persistence).as_mut() {
            if let Some(snapshot) = snapshot {
                persistence.retired_generations.insert((chat_id.to_owned(), message_id.to_owned()), (snapshot.clone(), snapshot.generation()));
            } else {
                persistence.legacy_retired.insert((chat_id.to_owned(), message_id.to_owned()), std::time::Instant::now());
            }
        }
    }
    fn synchronize_held(&self, held: &HashMap<String, Vec<WakeEvent>>) -> Result<(), String> {
        if let Some(persistence) = lock(&self.persistence).as_mut() {
            let before = persistence.clone();
            persistence.prune();
            let mut effective = held.clone();
            for chat in &persistence.unavailable {
                if let Some(entries) = persistence.store.orchestrators.get(chat) {
                    effective.entry(chat.clone()).or_default().extend(entries.iter().filter(|entry| !entry.discarded && entry.message_id.is_none()).map(|entry| entry.event.clone()));
                }
            }
            persistence.store.synchronize_held(&effective);
            if let Err(error) = persistence.write() {
                *persistence = before;
                return Err(error);
            }
        }
        Ok(())
    }

    fn mark_queued(
        &self,
        chat_id: &str,
        events: &[WakeEvent],
        message_id: &str,
        text: &str,
    ) -> Result<(), String> {
        if let Some(persistence) = lock(&self.persistence).as_mut() {
            let before = persistence.clone();
            persistence.prune();
            let text_hash = events.iter().any(|event| event.final_handback.is_some())
                .then(|| queue_text_hash(text));
            persistence.store.mark_queued(
                chat_id,
                &events.iter().map(|event| event.id.clone()).collect(),
                message_id,
                text_hash.as_deref(),
            );
            if let Err(error) = persistence.write() {
                *persistence = before;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn mark_delivered(self: &Arc<Self>, handle: &ChatDocHandle, message_id: &str) {
        let chat_id = handle.chat_id();
        if let Some(persistence) = lock(&self.persistence).as_mut() {
            let Some(events) = persistence.store.orchestrators.get(chat_id) else { return };
            let mut events = events.iter().filter(|entry| entry.message_id.as_deref() == Some(message_id)).peekable();
            if events.peek().is_none() {
                return;
            }
            let transcript_hash = if events.any(|entry| entry.event.final_handback.is_some()) {
                match handle.doc().read_entries() {
                    Ok(entries) => entries.iter().find(|entry| entry.id == message_id)
                        .and_then(handback_transcript_hash),
                    Err(error) => {
                        tracing::warn!(%error, chat = %chat_id, "could not verify final handback transcript");
                        return;
                    }
                }
            } else {
                None
            };
            persistence.store.mark_delivered(chat_id, message_id, transcript_hash.as_deref());
            if let Some(snapshot) = &handle.persistence {
                persistence.generations.insert((chat_id.to_owned(), message_id.to_owned()), (snapshot.clone(), snapshot.generation()));
            } else {
                persistence.legacy_receipts.insert((chat_id.to_owned(), message_id.to_owned()), std::time::Instant::now());
            }
            if let Err(error) = persistence.write() {
                tracing::warn!(%error, chat = %chat_id, "could not save wake delivery receipt");
            }
        }
        self.schedule_prune(chat_id, message_id);
    }

    pub(crate) fn is_final_handback_message(&self, chat_id: &str, message_id: &str) -> bool {
        lock(&self.persistence).as_ref().is_some_and(|persistence| {
            persistence.store.orchestrators.get(chat_id).is_some_and(|entries| {
                entries.iter().any(|entry| entry.message_id.as_deref() == Some(message_id)
                    && entry.event.final_handback.is_some())
            })
        })
    }

    fn mark_final_report_batch_disabled(&self, chat_id: &str, message_id: &str) -> Result<Vec<WakeEvent>, String> {
        let mut guard = lock(&self.persistence);
        let Some(persistence) = guard.as_mut() else { return Ok(Vec::new()) };
        let Some(entries) = persistence.store.orchestrators.get_mut(chat_id) else { return Ok(Vec::new()) };
        let mut events = Vec::new();
        for entry in entries.iter_mut().filter(|entry| entry.message_id.as_deref() == Some(message_id)) {
            if entry.event.final_handback.is_some() {
                entry.discarded = true;
            }
            events.push(entry.event.clone());
        }
        // Keep the in-process discard even if the write fails. The persisted
        // settings independently suppress the row on restart. Return the events
        // regardless so the caller can restore them instead of stranding them.
        if let Err(error) = persistence.write() {
            tracing::warn!(%error, chat = %chat_id, "could not persist disabled final report marker");
        }
        Ok(events)
    }

    /// Keep an explicit removal durable before the queue row can disappear.
    /// The queue drain is held by the caller while it verifies the row still exists.
    pub(crate) fn cancel_removed_handback(&self, chat_id: &str, message_id: &str) -> Result<bool, String> {
        let mut guard = lock(&self.persistence);
        let Some(persistence) = guard.as_mut() else { return Ok(false) };
        let Some(entries) = persistence.store.orchestrators.get(chat_id) else { return Ok(false) };
        let cancelled: Vec<_> = entries.iter()
            .filter(|entry| entry.message_id.as_deref() == Some(message_id))
            .filter_map(|entry| entry.event.final_handback.as_ref().map(|tag| tag.operation_id.clone()))
            .collect();
        if cancelled.is_empty() {
            return Ok(false);
        }
        let before = persistence.store.clone();
        persistence.store.cancelled_handbacks.extend(cancelled);
        for entry in persistence.store.orchestrators.get_mut(chat_id).into_iter().flatten() {
            if entry.message_id.as_deref() == Some(message_id) {
                entry.discarded = true;
            }
        }
        if let Err(error) = persistence.write() {
            persistence.store = before;
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn finish_removed_handback(self: &Arc<Self>, handle: &ChatDocHandle, message_id: &str) {
        let chat_id = handle.chat_id();
        if let Err(error) = self.discard_queued_message(chat_id, message_id, handle.persistence.as_ref()) {
            tracing::warn!(%error, chat = %chat_id, "could not retire removed wake row");
        }
        self.clear_message(chat_id, message_id);
        self.schedule_prune(chat_id, message_id);
    }

    pub(crate) fn schedule_prune(self: &Arc<Self>, chat_id: &str, message_id: &str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let turns = Arc::downgrade(self);
        let chat = chat_id.to_owned();
        let message = message_id.to_owned();
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let Some(turns) = turns.upgrade() else { return };
                let chat = chat.clone();
                let message = message.clone();
                let done = tokio::task::spawn_blocking(move || turns.prune_delivered(&chat, &message)).await;
                if !matches!(done, Ok(false)) { return; }
            }
        });
    }

    fn prune_delivered(&self, chat_id: &str, message_id: &str) -> bool {
        let mut guard = lock(&self.persistence);
        let Some(persistence) = guard.as_mut() else { return true };
        let before = persistence.clone();
        persistence.prune();
        let pending = persistence.store.orchestrators.get(chat_id).is_some_and(|entries| entries.iter().any(|entry| entry.message_id.as_deref() == Some(message_id) || entry.retired_messages.iter().any(|retired| retired.chat_id == chat_id && retired.message_id == message_id)));
        if persistence.store == before.store { return !pending; }
        if let Err(error) = persistence.write() {
            tracing::warn!(%error, chat = %chat_id, "could not prune wake delivery receipt");
            *persistence = before;
            return false;
        }
        !pending
    }
    pub fn register(&self, chat_id: &str, message_id: String, events: Vec<WakeEvent>) {
        lock(&self.turns).insert(
            chat_id.to_owned(),
            EventTurn {
                message_id,
                events,
                dispatched: false,
                started: false,
            },
        );
    }

    pub(crate) fn is_discarded_message(&self, chat_id: &str, message_id: &str) -> bool {
        lock(&self.persistence).as_ref().is_some_and(|persistence| {
            persistence.store.orchestrators.get(chat_id).is_some_and(|entries| {
                entries.iter().any(|entry| {
                    (entry.discarded && entry.message_id.as_deref() == Some(message_id))
                        || entry.retired_messages.iter()
                            .any(|row| row.chat_id == chat_id && row.message_id == message_id)
                })
            })
        })
    }

    pub(crate) fn discard_queued_message(&self, chat_id: &str, message_id: &str, snapshot: Option<&Arc<crate::chat_persistence::ChatPersistence>>) -> Result<(), String> {
        let mut guard = lock(&self.persistence);
        if let Some(persistence) = guard.as_mut() {
            let before = persistence.clone();
            if let Some(entries) = persistence.store.orchestrators.get_mut(chat_id) {
                for entry in entries.iter_mut().filter(|entry| entry.message_id.as_deref() == Some(message_id)) {
                    entry.discarded = true;
                }
            }
            // Persist the discard before retiring the queue identity.
            if let Err(error) = persistence.write() {
                *persistence = before;
                return Err(error);
            }
            if let Some(entries) = persistence.store.orchestrators.get_mut(chat_id) {
                for entry in entries.iter_mut().filter(|entry| entry.message_id.as_deref() == Some(message_id)) {
                    entry.retire(chat_id, message_id);
                    entry.message_id = None;
                }
            }
            if let Some(snapshot) = snapshot {
                persistence.retired_generations.insert((chat_id.to_owned(), message_id.to_owned()), (snapshot.clone(), snapshot.generation()));
            } else {
                persistence.legacy_retired.insert((chat_id.to_owned(), message_id.to_owned()), std::time::Instant::now());
            }
            if let Err(error) = persistence.write() {
                // The first write remains a valid discard receipt for retry.
                *persistence = before;
                if let Some(entries) = persistence.store.orchestrators.get_mut(chat_id) {
                    for entry in entries.iter_mut().filter(|entry| entry.message_id.as_deref() == Some(message_id)) { entry.discarded = true; }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn is_event_message(&self, message_id: &str) -> bool {
        lock(&self.turns)
            .values()
            .any(|t| t.message_id == message_id)
    }

    /// The queue row id of a wake that has not been taken yet.
    pub fn pending_message(&self, chat_id: &str) -> Option<String> {
        lock(&self.turns)
            .get(chat_id)
            .filter(|t| !t.dispatched && !t.started)
            .map(|t| t.message_id.clone())
    }

    /// A Manager send pulled this exact row back before dispatch. Retire a
    /// final report's queue identity even if its event is later dropped while
    /// Worker Reports are off.
    pub(crate) fn cancel_pulled_message(self: &Arc<Self>, handle: &ChatDocHandle, message_id: &str) {
        let chat_id = handle.chat_id();
        self.note_retired(chat_id, message_id, handle.persistence.as_ref());
        if self.is_final_handback_message(chat_id, message_id) {
            match handle.doc().read_entries() {
                Ok(entries) if entries.iter().any(|entry| entry.id == message_id) => {
                    // A partial submission wrote a transcript entry before
                    // the queue row was put back. Verify that entry instead
                    // of restoring a second delivery attempt.
                    self.mark_delivered(handle, message_id);
                    self.clear_message(chat_id, message_id);
                    return;
                }
                Ok(_) => self.retire_pulled_handback(chat_id, message_id),
                Err(error) => {
                    tracing::warn!(%error, chat = %chat_id, "could not reconcile pulled final report");
                    self.clear_message(chat_id, message_id);
                    return;
                }
            }
            self.schedule_prune(chat_id, message_id);
        }
        let mut turns = lock(&self.turns);
        if turns.get(chat_id).is_some_and(|turn| turn.message_id == message_id)
            && let Some(turn) = turns.remove(chat_id)
        {
            lock(&self.cancelled).entry(chat_id.to_owned()).or_default().extend(turn.events);
        }
    }

    fn retire_pulled_handback(&self, chat_id: &str, message_id: &str) {
        let mut guard = lock(&self.persistence);
        let Some(persistence) = guard.as_mut() else { return };
        let Some(entries) = persistence.store.orchestrators.get_mut(chat_id) else { return };
        for entry in entries.iter_mut().filter(|entry| entry.message_id.as_deref() == Some(message_id)) {
            entry.retire(chat_id, message_id);
            entry.message_id = None;
            entry.message_text_hash = None;
            entry.delivered = false;
        }
        if let Err(error) = persistence.write() {
            tracing::warn!(%error, chat = %chat_id, "could not persist final report pullback");
        }
    }

    pub fn take_cancelled(&self, chat_id: &str) -> Vec<WakeEvent> {
        lock(&self.cancelled).remove(chat_id).unwrap_or_default()
    }

    pub fn mark_dispatched(&self, chat_id: &str) {
        if let Some(turn) = lock(&self.turns).get_mut(chat_id) {
            turn.dispatched = true;
        }
    }

    /// An event turn is running (or about to).
    pub fn has_started(&self, chat_id: &str) -> bool {
        lock(&self.turns)
            .get(chat_id)
            .is_some_and(|t| t.dispatched || t.started)
    }

    fn clear_message(&self, chat_id: &str, message_id: &str) {
        let mut turns = lock(&self.turns);
        if turns.get(chat_id).is_some_and(|turn| turn.message_id == message_id) {
            turns.remove(chat_id);
        }
    }

    pub fn clear(&self, chat_id: &str) {
        lock(&self.turns).remove(chat_id);
    }

    /// Follow the orchestrator's turn: seen in flight → started; finished
    /// after that → no longer an event turn.
    fn observe(&self, chat_id: &str, in_flight: bool) {
        let mut turns = lock(&self.turns);
        let Some(turn) = turns.get_mut(chat_id) else {
            return;
        };
        if in_flight {
            turn.started = true;
        } else if turn.started {
            turns.remove(chat_id);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ChatInfo {
    title: Option<String>,
    ticket: Option<String>,
    pr_url: Option<String>,
    parent: Option<String>,
    archived: bool,
    last_active: i64,
}

struct TurnStart {
    last_message_id: Option<String>,
    last_completed_turn: Option<String>,
    user_sent_during_turn: bool,
}

#[derive(Default)]
struct State {
    core: WakeCore,
    auto_continue: auto_continue::AutoContinue,
    turn_starts: HashMap<String, TurnStart>,
    auto_continue_default: bool,
    settings_overrides: HashMap<String, WakeSettings>,
    handed_off_to: HashMap<String, String>,
    hand_off_pending: HashSet<String>,
    // Independent of the generic transfer gate: an unrelated handoff finish
    // must not release a pending automatic successor's wakes.
    automatic_handoff_pending: HashMap<String, String>,
    chats: HashMap<String, ChatInfo>,
    chats_seeded: bool,
    /// Per Space: worker chat → orchestrators by project worker destination.
    destination_parents: HashMap<String, HashMap<String, Vec<String>>>,
    /// Required Action request id → open (unresolved) when last seen.
    requests: HashMap<String, bool>,
    seeded_spaces: HashSet<String>,
    statuses: HashMap<String, SessionStatus>,
    sessions_seeded: bool,
    timers: HashMap<String, i64>,
    /// When the long-tool-call check last ran (wake clock, ms).
    last_long_call_check_ms: Option<i64>,
}

struct Inner {
    state: Mutex<State>,
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    event_turns: Arc<EventTurns>,
    clock: Clock,
    #[cfg(debug_assertions)]
    test_clock_offset_ms: std::sync::atomic::AtomicI64,
    store: Option<PathBuf>,
    /// Ends the watchers: they hold the sessions/doc-host graph alive.
    cancel: tokio_util::sync::CancellationToken,
    /// Where a report goes when its worker was started by a paired peer that
    /// named a parent chat on its own device (set once, when the peer listener starts).
    remote_reports: std::sync::OnceLock<Arc<dyn crate::peer_link::RemoteReportSink>>,
}

/// What became of a report a paired device collected (see [`OrchestratorWakes::report_from_peer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerReportOutcome {
    /// Queued for the parent's next "Worker updates" batch (or already was).
    Queued,
    /// The parent turned worker reports off; nothing was queued.
    ReportsOff,
    /// Undeliverable for good (the parent is gone or lives elsewhere).
    Dropped(&'static str),
}

#[derive(Clone)]
pub struct OrchestratorWakes {
    inner: Arc<Inner>,
}

impl OrchestratorWakes {
    /// Build the service without starting its watchers (tests drive the
    /// `ingest_*` entry points directly).
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        store: Option<PathBuf>,
        clock: Clock,
    ) -> Self {
        let event_turns = doc_host.event_turns();
        let mut state = State::default();
        if let Some(path) = &store
            && let Ok(bytes) = std::fs::read(path)
        {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(saved) = value.get("autoContinueWorkers")
                && let Ok(saved) = serde_json::from_value(saved.clone())
            {
                state.auto_continue = saved;
            }
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(saved) = value.get("handedOffTo")
                && let Ok(saved) = serde_json::from_value(saved.clone())
            {
                state.handed_off_to = saved;
            }
            let (auto_continue_default, saved) = read_settings(&bytes);
            state.auto_continue_default = auto_continue_default;
            for (chat, settings) in saved {
                state.core.set_settings(&chat, settings);
                state.settings_overrides.insert(chat, settings);
            }
        }
        if let Some(settings_path) = &store {
            let path = settings_path.with_file_name("orchestrator-wake-events.json");
            let mut saved = store::WakeStore::read(&path);
            saved.establish_final_handback_history(workspace.has_locally_authenticated_final_handbacks());
            let mut retired_generations = HashMap::new();
            let mut generations = HashMap::new();
            let mut legacy_retired = HashMap::new();
            let mut legacy_receipts = HashMap::new();
            let mut unavailable = HashSet::new();
            for chat in saved.orchestrators.keys().cloned().collect::<Vec<_>>() {
                let invalid_handback = saved.orchestrators[&chat].iter().any(|entry| {
                    let Some(tag) = &entry.event.final_handback else { return false };
                    !workspace.authenticated_final_handback(&tag.operation_id).is_some_and(|intent| {
                        *tag == handback_identity(&intent)
                            && entry.event.worker_chat_id == intent.worker_chat_id
                            && entry.event.kind == WakeEventKind::WorkerReport
                            && entry.event.report.as_deref() == Some(&intent.report)
                    })
                });
                if invalid_handback {
                    tracing::warn!(%chat, "could not authenticate stored final handback");
                    unavailable.insert(chat);
                    continue;
                }
                let settings = state.core.settings(&chat);
                let ineligible = workspace.chat(&chat).ok().flatten().is_some_and(|recipient| recipient.archived)
                    || !settings.enabled;
                let handle = match doc_host.open(&chat) {
                    Ok(handle) => handle,
                    Err(error) => { tracing::warn!(%error, %chat, "could not open wake recipient during recovery"); unavailable.insert(chat); continue; }
                };
                let queue: HashSet<String> = match handle.doc().read_queue() {
                    Ok(queue) => queue.into_iter().map(|item| item.id).collect(),
                    Err(error) => { tracing::warn!(%error, %chat, "could not read wake recipient queue during recovery"); unavailable.insert(chat); continue; }
                };
                let final_messages: HashSet<&str> = saved.orchestrators[&chat].iter()
                    .filter(|entry| entry.event.final_handback.is_some())
                    .filter_map(|entry| entry.message_id.as_deref()).collect();
                let transcript: HashMap<String, Option<String>> = if !final_messages.is_empty() {
                    match handle.doc().read_entries() {
                        Ok(entries) => entries.into_iter().map(|entry| {
                            let hash = final_messages.contains(entry.id.as_str())
                                .then(|| handback_transcript_hash(&entry)).flatten();
                            (entry.id, hash)
                        }).collect(),
                        Err(error) => {
                            tracing::warn!(%error, %chat, "could not read final handback recipient transcript");
                            unavailable.insert(chat);
                            continue;
                        }
                    }
                } else {
                    handle.doc().recent_message_ids(50).into_iter().map(|id| (id, None)).collect()
                };
                // A disabled or archived Manager can already have received the
                // report. Reconcile its transcript before discarding stale wakes.
                let mut changed = false;
                if let Some(entries) = saved.orchestrators.get_mut(&chat) {
                    for entry in entries {
                        let report_in_transcript = entry.message_id.as_ref().and_then(|id| transcript.get(id));
                        let delivered_handback = entry.event.final_handback.as_ref().is_some_and(|tag| {
                            if saved.cancelled_handbacks.contains(&tag.operation_id) {
                                return false;
                            }
                            let Some(hash) = report_in_transcript else { return false };
                            if entry.has_original_text(hash.as_deref()) && tag.parent_chat_id == chat {
                                return true;
                            }
                            if tag.parent_chat_id == chat {
                                changed |= saved.cancelled_handbacks.insert(tag.operation_id.clone());
                            }
                            false
                        });
                        let cancelled = entry.event.final_handback.as_ref().is_some_and(|tag|
                            saved.cancelled_handbacks.contains(&tag.operation_id));
                        // Apply the per-kind toggle to undelivered events (a retired queue
                        // ID can outlive a crash); delivered ordinary entries keep the
                        // existing completion rules.
                        let kind_off = !settings.wakes_for(entry.event.kind);
                        let disabled = ineligible
                            || (kind_off && (entry.event.final_handback.is_some() || report_in_transcript.is_none()));
                        let discard = !delivered_handback && (disabled || entry.discarded || cancelled);
                        changed |= entry.discarded != discard;
                        entry.discarded = discard;
                    }
                }
                if changed && let Err(error) = saved.write(&path) {
                    tracing::warn!(%error, %chat, "could not persist wake reconciliation before recovery");
                    unavailable.insert(chat);
                    continue;
                }
                let mut removed = HashSet::new();
                let mut cross_blocked = HashSet::new();
                if let Some(events) = saved.orchestrators.get(&chat) {
                    let cross: HashSet<_> = events.iter().flat_map(|entry| entry.retired_messages.iter())
                        .filter(|retired| retired.chat_id != chat).map(|retired| (retired.chat_id.clone(), retired.message_id.clone())).collect();
                    for (source, id) in cross {
                            match doc_host.open(&source).and_then(|source_handle| {
                                let present = source_handle.doc().read_queue()?.iter().any(|item| item.id == id);
                                let removed = doc_host.retire_queued_wake_row(&source_handle, &id)?;
                                if !present || removed {
                                    if let Some(snapshot) = &source_handle.persistence {
                                        retired_generations.insert((source.clone(), id.clone()), (snapshot.clone(), snapshot.generation()));
                                    } else {
                                        legacy_retired.insert((source.clone(), id.clone()), std::time::Instant::now());
                                    }
                                }
                                Ok((present, removed))
                            }) {
                                Ok((_, true)) => {},
                                Ok((true, false)) => { cross_blocked.insert((source.clone(), id.clone())); },
                                Ok((false, false)) => {},
                                Err(error) => { cross_blocked.insert((source.clone(), id.clone())); tracing::warn!(%error, chat = %source, "could not reclaim transferred wake row"); },
                            }
                    }
                    let ids: HashSet<_> = events.iter().flat_map(|entry| entry.message_id.iter().chain(entry.retired_messages.iter().filter(|retired| retired.chat_id == chat).map(|retired| &retired.message_id))).filter(|id| queue.contains(*id)).cloned().collect();
                    for id in events.iter().filter_map(|entry| entry.message_id.as_ref().filter(|id| entry.discarded || !transcript.contains_key(*id))).filter(|id| !queue.contains(*id)) {
                        if let Some(snapshot) = &handle.persistence {
                            retired_generations.insert((chat.clone(), id.clone()), (snapshot.clone(), snapshot.generation()));
                        } else {
                            legacy_retired.insert((chat.clone(), id.clone()), std::time::Instant::now());
                        }
                    }
                    for retired in events.iter().flat_map(|entry| entry.retired_messages.iter()).filter(|retired| retired.chat_id == chat && !queue.contains(&retired.message_id)) {
                        if let Some(snapshot) = &handle.persistence {
                            retired_generations.insert((chat.clone(), retired.message_id.clone()), (snapshot.clone(), snapshot.generation()));
                        } else {
                            legacy_retired.insert((chat.clone(), retired.message_id.clone()), std::time::Instant::now());
                        }
                    }
                    for id in ids {
                        match doc_host.retire_queued_wake_row(&handle, &id) {
                            Ok(true) => {
                                if let Some(snapshot) = &handle.persistence {
                                    retired_generations.insert((chat.clone(), id.clone()), (snapshot.clone(), snapshot.generation()));
                                } else {
                                    legacy_retired.insert((chat.clone(), id.clone()), std::time::Instant::now());
                                }
                                removed.insert(id);
                            },
                            Ok(false) => {},
                            Err(error) => tracing::warn!(%error, chat = %chat, "could not reclaim wake row"),
                        }
                    }
                }
                let mut held = Vec::new();
                let mut completed = Vec::new();
                let mut handbacks = Vec::new();
                let mut blocked = false;
                if let Some(events) = saved.orchestrators.get_mut(&chat) {
                    events.retain_mut(|entry| {
                        if entry.retired_messages.iter().any(|retired| cross_blocked.contains(&(retired.chat_id.clone(), retired.message_id.clone()))) { blocked = true; return true; }
                        if entry.discarded {
                            if let Some(id) = entry.message_id.clone() {
                                if queue.contains(&id) && !removed.contains(&id) { blocked = true; return true; }
                                entry.retire(&chat, &id);
                                entry.message_id = None;
                            }
                            if entry.retired_messages.iter().any(|retired| retired.chat_id == chat && queue.contains(&retired.message_id) && !removed.contains(&retired.message_id)) { blocked = true; }
                            return !entry.retired_messages.is_empty();
                        }
                        if let Some(id) = entry.message_id.clone() {
                            if transcript.contains_key(&id) {
                                let snapshot_pending = handle.persistence.as_ref().is_none_or(|snapshot| !snapshot.has_saved(snapshot.generation()));
                                if removed.contains(&id) || !entry.retired_messages.is_empty() || snapshot_pending {
                                    entry.delivered = true;
                                    if removed.contains(&id) { entry.retire(&chat, &id); }
                                    return true;
                                }
                                completed.push(entry.event.id.clone());
                                if let Some(receipt) = entry.handback_receipt(&chat) {
                                    handbacks.push(receipt);
                                }
                                return false;
                            }
                            if queue.contains(&id) && !removed.contains(&id) { blocked = true; return true; }
                            entry.retire(&chat, &id);
                            entry.message_id = None;
                            entry.delivered = false;
                        }
                        if entry.retired_messages.iter().any(|retired| retired.chat_id == chat && queue.contains(&retired.message_id) && !removed.contains(&retired.message_id)) { blocked = true; return true; }
                        held.push(entry.event.clone());
                        true
                    });
                }
                if blocked { unavailable.insert(chat.clone()); }
                for id in completed { saved.remember_completed(&chat, &id); }
                for handback in handbacks { saved.remember_handback(handback); }
                if !held.is_empty() { state.core.note_user_activity(&chat, clock(), None); }
                state.core.restore_for_hand_off(&chat, held, clock());
                if let (Some(snapshot), Some(events)) = (&handle.persistence, saved.orchestrators.get(&chat)) {
                    for entry in events.iter().filter(|entry| entry.delivered) {
                        if let Some(id) = &entry.message_id {
                            generations.insert((chat.clone(), id.clone()), (snapshot.clone(), snapshot.generation()));
                        }
                    }
                }
                if handle.persistence.is_none() && let Some(events) = saved.orchestrators.get(&chat) {
                    for entry in events.iter().filter(|entry| entry.delivered) {
                        if let Some(id) = &entry.message_id { legacy_receipts.insert((chat.clone(), id.clone()), std::time::Instant::now()); }
                    }
                }
            }
            for (chat, ids) in &saved.completed_ids {
                for id in ids { state.core.remember_delivered_id(chat, id, clock()); }
            }
            let result = std::thread::scope(|scope| scope.spawn(|| saved.write(&path)).join());
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not save restored wakes");
                    saved.final_handback_history_ready = false;
                }
                Err(_) => saved.final_handback_history_ready = false,
            }
            *lock(&event_turns.persistence) = Some(WakePersistence { path, store: saved, generations, retired_generations, legacy_receipts, legacy_retired, unavailable });
            let pending: Vec<_> = lock(&event_turns.persistence).as_ref().map(|p| p.generations.keys().chain(p.legacy_receipts.keys()).chain(p.retired_generations.keys()).chain(p.legacy_retired.keys()).cloned().collect()).unwrap_or_default();
            for (chat, message) in pending { event_turns.schedule_prune(&chat, &message); }
        }
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                sessions,
                doc_host,
                workspace,
                event_turns,
                clock,
                #[cfg(debug_assertions)]
                test_clock_offset_ms: std::sync::atomic::AtomicI64::new(0),
                store,
                cancel: tokio_util::sync::CancellationToken::new(),
                remote_reports: std::sync::OnceLock::new(),
            }),
        }
    }

    /// Route reports of peer-created workers to `sink` (see [`crate::peer_link`]).
    /// Only the first call takes effect.
    pub fn set_remote_reports(&self, sink: Arc<dyn crate::peer_link::RemoteReportSink>) {
        let _ = self.inner.remote_reports.set(sink);
    }

    /// Build the service and start its event-driven watchers.
    pub fn start(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        store: Option<PathBuf>,
    ) -> Self {
        let this = Self::new(sessions, doc_host, workspace, store, system_clock());
        this.spawn_watchers();
        this
    }

    /// Stop the watchers so a retired engine graph can be freed.
    pub fn shutdown(&self) {
        self.inner.cancel.cancel();
    }

    /// Prune receipts after the document host's graceful snapshot flush.
    pub async fn flush_receipts(&self) {
        let turns = self.inner.event_turns.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || {
            let mut guard = lock(&turns.persistence);
            if let Some(persistence) = guard.as_mut() {
                let before = persistence.clone();
                persistence.prune();
                if persistence.store != before.store && let Err(error) = persistence.write() {
                    *persistence = before;
                    tracing::warn!(%error, "could not save wake receipts during shutdown");
                }
            }
        }).await {
            tracing::warn!(%error, "wake receipt shutdown worker failed");
        }
    }

    fn now(&self) -> i64 {
        let now = (self.inner.clock)();
        #[cfg(debug_assertions)]
        let now = now.saturating_add(self.inner.test_clock_offset_ms.load(std::sync::atomic::Ordering::Relaxed));
        now
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn test_advance_clock(&self, by_ms: i64) {
        self.inner.test_clock_offset_ms.fetch_add(by_ms, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn spawn_watchers(&self) {
        // The chat watch coalesces rapid changes. Discard while the chat is
        // still archived, before a local unarchive can hide that transition.
        let weak = Arc::downgrade(&self.inner);
        self.inner.workspace.set_before_chat_unarchive(Arc::new(move |chat_id| {
            if let Some(inner) = weak.upgrade() {
                let wakes = OrchestratorWakes { inner };
                wakes.evaluate_blocking(chat_id, false);
                let state = lock(&wakes.inner.state);
                let pending_discard = lock(&wakes.inner.event_turns.persistence).as_ref()
                    .and_then(|persistence| persistence.store.orchestrators.get(chat_id))
                    .is_some_and(|entries| entries.iter().any(|entry| !entry.discarded));
                if state.core.has_held(chat_id) || pending_discard {
                    return Err("could not discard archived wake events before unarchiving".into());
                }
            }
            Ok(())
        }));
        let this = self.clone();
        tokio::spawn(async move {
            // Registry and wake receipts have been restored, and the engine's
            // session recovery runs before these watchers start.
            this.recover_final_handbacks().await;
            loop {
                this.reconcile_final_handback_archives().await;
                if this.watch_ended(tokio::time::sleep(std::time::Duration::from_secs(2)).map(Ok::<(), ()>)).await {
                    break;
                }
            }
        });
        let mut signals = self.inner.sessions.watch_worker_signals();
        let this = self.clone();
        tokio::spawn(async move {
            if let Ok(chats) = this.inner.workspace.read_chats() {
                this.ingest_chats(&chats).await;
            }
            loop {
                tokio::select! {
                    _ = this.inner.cancel.cancelled() => break,
                    signal = signals.recv() => match signal {
                        Ok(signal) => this.ingest_worker_signal(signal).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => tracing::warn!(n, "worker wake signals dropped"),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut rx = this.inner.sessions.watch_sessions();
            loop {
                let sessions = rx.borrow_and_update().clone();
                this.ingest_sessions(&sessions).await;
                if this.watch_ended(rx.changed()).await {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut rx = this.inner.workspace.watch_session_rows();
            let mut observed = HashSet::new();
            loop {
                let sessions = rx.borrow_and_update().clone();
                let now = chrono::Utc::now();
                let mut live = HashSet::new();
                for session in sessions {
                    if session.device_id == this.inner.doc_host.device_id()
                        || mast_proto::view::effective_indicator(Some(&session), now) != mast_proto::view::Indicator::AwaitingInput
                    {
                        continue;
                    }
                    let Some(input) = session.awaiting_input else { continue; };
                    let key = (session.chat_id.clone(), input.request_id.clone());
                    if !observed.contains(&key) {
                        if !this.inner.workspace.chat(&session.chat_id).ok().flatten()
                            .is_some_and(|chat| !chat.archived && chat.device_id == session.device_id)
                        {
                            continue;
                        }
                        this.ingest_worker_signal(crate::sessions::WorkerSignal::Input { chat_id: session.chat_id, input }).await;
                    }
                    live.insert(key);
                }
                observed = live;
                if this.watch_ended(rx.changed()).await {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut rx = this.inner.workspace.watch_chats();
            loop {
                let chats = rx.borrow_and_update().clone();
                this.ingest_chats(&chats).await;
                if this.watch_ended(rx.changed()).await {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let worker = this.clone();
                let _ = tokio::task::spawn_blocking(move || worker.notify_due_waiting_items()).await;
                this.flag_long_tool_calls().await;
                if this.watch_ended(tokio::time::sleep(std::time::Duration::from_secs(30)).map(Ok::<(), ()>)).await {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut changes = this.inner.workspace.watch_changes();
            let mut markers: HashMap<String, (u64, u64, bool)> = HashMap::new();
            loop {
                if this
                    .watch_ended(
                        tokio::time::sleep(std::time::Duration::from_millis(500)).map(Ok::<(), ()>),
                    )
                    .await
                {
                    break;
                }
                changes.borrow_and_update();
                let spaces = this.inner.workspace.read_spaces().unwrap_or_default();
                for space in spaces {
                    let marker = this.inner.workspace.work_outline_inputs_marker(&space.id);
                    if markers.get(&space.id) == Some(&marker) {
                        continue;
                    }
                    let workspace = this.inner.workspace.clone();
                    let space_id = space.id.clone();
                    let Ok((marker, snapshot)) = tokio::task::spawn_blocking(move || {
                        let (marker, inputs) = workspace.work_outline_inputs_marked(&space_id);
                        (
                            marker,
                            mast_doc::materialize_work_outline(&space_id, &inputs),
                        )
                    })
                    .await
                    else {
                        continue;
                    };
                    markers.insert(space.id.clone(), marker);
                    this.ingest_snapshot(&snapshot).await;
                }
                if this.watch_ended(changes.changed()).await {
                    break;
                }
            }
        });
    }

    /// Tells each Manager once about a working worker whose single tool call
    /// has had no result past [`crate::tool_watch::LONG_TOOL_CALL_MS`]. The
    /// worker's session still reads "working", so no idle check sees it.
    #[doc(hidden)]
    pub async fn flag_long_tool_calls(&self) {
        /// Longer than this between checks (the tick is 30 s) means the host
        /// slept or stalled, and no call was quiet for that time.
        const RESUME_GAP_MS: i64 = 3 * 30_000;
        let now = self.now();
        let (working, resumed) = {
            let mut state = lock(&self.inner.state);
            let resumed = state
                .last_long_call_check_ms
                .is_some_and(|last| now.saturating_sub(last) > RESUME_GAP_MS);
            state.last_long_call_check_ms = Some(now);
            let working: HashSet<String> = state
                .statuses
                .iter()
                .filter(|(_, status)| **status == SessionStatus::Working)
                .map(|(chat, _)| chat.clone())
                .collect();
            (working, resumed)
        };
        if resumed {
            crate::tool_watch::all_resumed(now);
            return;
        }
        let long = crate::tool_watch::long_calls(now, crate::tool_watch::LONG_TOOL_CALL_MS, |chat| {
            working.contains(chat)
        });
        if long.is_empty() {
            return;
        }
        let mut recorded = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            for call in long {
                let minutes = call.quiet_ms / 60_000;
                let detail = format!(
                    "a tool call has had no result for {minutes} min ({}); the chat still shows as working. It may be a long legitimate run: read_chat to look, interrupt_chat if it is stuck",
                    call.label
                );
                let line = format!("{} — {detail}", label(&state, &call.chat_id));
                let key = format!("long-tool:{}:{}", call.chat_id, call.tool_id);
                let managers = parents_of(&state, &call.chat_id);
                // Without a Manager to tell, the call stays unreported.
                if managers.is_empty() {
                    continue;
                }
                for orch in managers {
                    let event = make_event(&mut state, &call.chat_id, WakeEventKind::LongToolCall, key.clone(), &line, now);
                    state.core.record(&orch, event, now);
                    recorded.insert(orch);
                }
                crate::tool_watch::mark_flagged(&call.chat_id, &call.tool_id);
            }
        }
        self.after_ingest(recorded).await;
    }

    #[doc(hidden)]
    pub fn notify_due_waiting_items(&self) {
        use mast_proto::work_outline_user_actions::{UserActionKind, UserActionPriority};
        use mast_proto::{CreateUserActionRequestParams, UserActionOriginKind, UserActionRequestOrigin, WorkOutlineActor, WorkOutlineMutation};
        let workspace = &self.inner.workspace;
        let device_id = workspace.device_id();
        let actor = WorkOutlineActor { kind: WorkOutlineActorKind::System, device_id: device_id.to_owned(), chat_id: None, message_id: None };
        for space in workspace.read_spaces().unwrap_or_default() {
            if space.device_id != device_id { continue; }
            let inputs = workspace.work_outline_inputs(&space.id);
            let Some(snapshot) = materialize_due_outline(&space.id, &inputs, self.now(), || {
                mast_doc::materialize_work_outline(&space.id, &inputs)
            }) else { continue; };
            if !snapshot.available { continue; }
            for item in snapshot.items.iter().filter(|item| item.archived_at.is_none() && item.status != mast_proto::WorkItemStatus::Completed) {
                let Some(until) = item.waiting_until.filter(|until| *until <= self.now()) else { continue; };
                let operation_id = mast_proto::work_outline::derive_id(&format!("{}\0{}\0{}", space.id, item.id, until), "waiting-due");
                let reason = item.waiting_reason.as_deref().filter(|reason| !reason.trim().is_empty()).unwrap_or("ready to review");
                let mutation = WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
                    options: vec![], links: vec![], facts: vec![],
                    kind: Some(UserActionKind::Notice), priority: Some(UserActionPriority::Fyi),
                    work_item_id: item.id.clone(),
                    origin: UserActionRequestOrigin { kind: UserActionOriginKind::System, ..Default::default() },
                    text: format!("Waiting period ended: {}", reason.chars().take(150).collect::<String>()),
                    info: None, dedupe_key: Some(operation_id.clone()), operation_id,
                });
                let _ = workspace.work_outline_mutate(&mutation, &actor, &[]);
            }
        }
    }

    /// Wait for `next`; true when the watch closed or the service shut down.
    async fn watch_ended<T, E>(
        &self,
        next: impl std::future::Future<Output = Result<T, E>>,
    ) -> bool {
        tokio::select! {
            _ = self.inner.cancel.cancelled() => true,
            result = next => result.is_err(),
        }
    }

    // ── settings / status / user activity ─────────────────────────────────

    pub fn settings(&self, chat_id: &str) -> WakeSettings {
        let state = lock(&self.inner.state);
        state
            .settings_overrides
            .get(chat_id)
            .copied()
            .unwrap_or_else(|| WakeSettings {
                auto_continue: state.auto_continue_default,
                ..WakeSettings::default()
            })
    }

    pub async fn set_settings(&self, chat_id: &str, settings: WakeSettings) -> WakeStatus {
        let reports_off = settings.enabled && !settings.worker_reports;
        let handle = if reports_off && self.hosted_here(chat_id) {
            self.inner.doc_host.open(chat_id).ok()
        } else {
            None
        };
        // Disable queued final reports at the same boundary that submits a
        // queue row to the Manager transcript.
        let drain = match &handle {
            Some(handle) => Some(handle.drain_lock.lock().await),
            None => None,
        };
        let this = self.clone();
        let chat = chat_id.to_owned();
        let settings_handle = handle.clone();
        let reports_enabled = tokio::task::spawn_blocking(move || {
            let mut state = lock(&this.inner.state);
            let had_reports = state.core.settings(&chat).wakes_for(WakeEventKind::WorkerReport);
            state.core.set_settings(&chat, settings);
            state.settings_overrides.insert(chat, settings);
            this.persist(&state);
            if let Some(handle) = &settings_handle
                && let Err(error) = this.stop_queued_final_reports(&mut state, handle)
            {
                tracing::warn!(%error, "could not retract disabled final report");
            }
            if let Err(error) = this.inner.event_turns.synchronize_held(&state.core.held_events()) {
                tracing::warn!(%error, "could not save held wake events");
            }
            !had_reports && settings.wakes_for(WakeEventKind::WorkerReport)
        }).await;
        drop(drain);
        match reports_enabled {
            Ok(true) => self.recover_final_handbacks_for(Some(chat_id)).await,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "wake settings worker failed"),
        }
        self.evaluate(chat_id, false).await;
        self.status(chat_id)
    }

    /// Call under both the wake state and Manager queue drain locks. Preserve
    /// already delivered reports and restore still-enabled events in a mixed batch.
    fn stop_queued_final_reports(&self, state: &mut State, handle: &ChatDocHandle) -> Result<(), String> {
        let chat = handle.chat_id();
        let turns = &self.inner.event_turns;
        let queue = handle.doc().read_queue().map_err(|error| error.to_string())?;
        let rows: Vec<_> = queue.into_iter()
            .filter(|row| turns.is_final_handback_message(chat, &row.id)).collect();
        if rows.is_empty() {
            return Ok(());
        }
        let transcript = handle.doc().read_entries().map_err(|error| error.to_string())?;
        // Retract every row even if one fails, so a later queued report cannot
        // dispatch while Worker Reports are off. Report the first error.
        // At most one wake row is queued per Manager: `evaluate_blocking`
        // decides under the wake state lock with the queue sampled under that
        // same lock, and `WakeCore::decide` holds while any queue row exists
        // (`core.rs` Hold(Queued)); one Deliver renders a single batch as one
        // row, however many events (a mixed batch can hold several final
        // reports) it contains. So this loop normally sees one row, but that is
        // not strictly guaranteed: a Deliver landing in the gap between the
        // queue drain's take and the session turning active, followed by a
        // failed dispatch that re-queues the first row, can leave two. The
        // per-row retraction here covers that.
        let mut first_error = None;
        for row in rows {
            let stopped = (|| -> Result<(), String> {
                if transcript.iter().any(|entry| entry.id == row.id) {
                    self.inner.doc_host.discard_queued_wake(chat, &row.id).map_err(|error| error.to_string())?;
                    turns.mark_delivered(handle, &row.id);
                } else {
                    let events = turns.mark_final_report_batch_disabled(chat, &row.id)?;
                    if let Err(error) = self.inner.doc_host.discard_queued_wake(chat, &row.id) {
                        // The batch is already marked discarded; put its events
                        // back so a later re-enable does not strand the report.
                        state.core.restore(chat, events, self.now());
                        return Err(error.to_string());
                    }
                    turns.note_retired(chat, &row.id, handle.persistence.as_ref());
                    state.core.restore(chat, events, self.now());
                    turns.synchronize_held(&state.core.held_events())?;
                    turns.discard_queued_message(chat, &row.id, handle.persistence.as_ref())?;
                    turns.schedule_prune(chat, &row.id);
                }
                Ok(())
            })();
            // Drop the in-memory pending id even on failure, so a later pull
            // does not find a row that is no longer queued.
            turns.clear_message(chat, &row.id);
            if let Err(error) = stopped {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Reclaim a queued wake and transfer its events and effective settings.
    /// Call under the wake state lock, serialized with wake delivery.
    fn transfer_wakes(&self, state: &mut State, from: &str, to: &str) {
        self.inner.doc_host.pull_pending_event_message(from);
        let cancelled = self.inner.event_turns.take_cancelled(from);
        state.core.restore_for_hand_off(from, cancelled, self.now());
        let settings = state.settings_overrides.get(from).copied().unwrap_or_else(|| WakeSettings {
            auto_continue: state.auto_continue_default,
            ..WakeSettings::default()
        });
        state.core.set_settings_preserving_held(to, settings);
        state.settings_overrides.insert(to.to_owned(), settings);
        let held_ids: HashSet<String> = state.core.held_events().remove(from).unwrap_or_default().into_iter().map(|event| event.id).collect();
        state.core.transfer_held(from, to);
        if let Some(persistence) = lock(&self.inner.event_turns.persistence).as_mut() {
            persistence.store.transfer(from, to, &held_ids);
        }
        state.handed_off_to.insert(from.to_owned(), to.to_owned());
        state.timers.remove(from);
    }

    /// Hand off direct workers and their pending wakes to another orchestrator.
    pub async fn hand_off_orchestrator(&self, from: &str, to: &str, actor: &str) -> Result<Vec<String>, String> {
        let workers = self.hand_off_orchestrator_without_delivery(from, to, actor).await?;
        self.finish_orchestrator_hand_off(to);
        self.evaluate(to, false).await;
        Ok(workers)
    }

    pub(crate) async fn hand_off_orchestrator_without_delivery(&self, from: &str, to: &str, actor: &str) -> Result<Vec<String>, String> {
        let this = self.clone();
        let (from, to, actor) = (from.to_owned(), to.to_owned(), actor.to_owned());
        tokio::task::spawn_blocking(move || this.hand_off_orchestrator_blocking(&from, &to, &actor)).await
            .map_err(|error| format!("wake hand-off worker failed: {error}"))?
    }

    fn hand_off_orchestrator_blocking(&self, from: &str, to: &str, actor: &str) -> Result<Vec<String>, String> {
        let workers = {
            let mut state = lock(&self.inner.state);
            if lock(&self.inner.event_turns.persistence).as_ref().is_some_and(|persistence| persistence.unavailable.contains(from)) {
                return Err("wake recovery is blocked for the source chat; restart and retry the hand-off".into());
            }
            if state.handed_off_to.get(from).is_some_and(|existing| existing != to) {
                return Err("source already handed off to another chat".into());
            }
            let mut cursor = to;
            let mut remaining = state.handed_off_to.len() + 1;
            while let Some(next) = state.handed_off_to.get(cursor) {
                if next == from || remaining == 0 {
                    return Err("hand-off would create a routing cycle".into());
                }
                cursor = next;
                remaining -= 1;
            }
            let (workers, replayed) = self.inner.workspace.hand_off_chat_children(from, to, actor, self.now())?;
            state.hand_off_pending.insert(to.to_owned());
            // Registry and wake settings use separate stores. If a process
            // stopped after the registry write, complete the transfer on retry.
            if !replayed || state.handed_off_to.get(from).map(String::as_str) != Some(to) {
                self.transfer_wakes(&mut state, from, to);
                for worker in &workers {
                    if let Some(info) = state.chats.get_mut(worker) {
                        info.parent = Some(to.to_owned());
                    }
                }
            }
            if let Some(persistence) = lock(&self.inner.event_turns.persistence).as_mut() {
                persistence.write().map_err(|error| format!("could not save transferred wake events: {error}"))?;
            }
            self.persist_checked(&state).map_err(|error| format!("could not save wake settings: {error}"))?;
            workers
        };
        Ok(workers)
    }

    pub(crate) fn hold_automatic_manager_handoff(&self, successor: &str, operation: &str) {
        lock(&self.inner.state).automatic_handoff_pending
            .entry(successor.to_owned()).or_insert_with(|| operation.to_owned());
    }

    pub(crate) fn finish_automatic_manager_handoff(&self, successor: &str, operation: &str) {
        let mut state = lock(&self.inner.state);
        if state.automatic_handoff_pending.get(successor).map(String::as_str) == Some(operation) {
            state.automatic_handoff_pending.remove(successor);
            state.hand_off_pending.remove(successor);
        }
    }

    pub(crate) fn finish_orchestrator_hand_off(&self, to: &str) {
        lock(&self.inner.state).hand_off_pending.remove(to);
    }

    fn persist(&self, state: &State) {
        if let Err(error) = self.persist_checked(state) {
            tracing::warn!(%error, "could not save orchestrator wake settings");
        }
    }

    fn persist_checked(&self, state: &State) -> Result<(), std::io::Error> {
        let Some(path) = &self.inner.store else {
            return Ok(());
        };
        let settings = serde_json::json!({
            "autoContinue": state.auto_continue_default,
            "orchestrators": state.settings_overrides,
            "autoContinueWorkers": state.auto_continue,
            "handedOffTo": state.handed_off_to,
        });
        let bytes = serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?;
        let staging = path.with_extension("handoff-tmp");
        let mut file = std::fs::File::create(&staging)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        std::fs::rename(staging, path)?;
        Ok(())
    }

    pub fn status(&self, chat_id: &str) -> WakeStatus {
        let ctx = self.ctx(chat_id);
        let now = self.now();
        let mut state = lock(&self.inner.state);
        let mut status = state.core.status(chat_id, now, ctx);
        status.settings = state
            .settings_overrides
            .get(chat_id)
            .copied()
            .unwrap_or_else(|| WakeSettings {
                auto_continue: state.auto_continue_default,
                ..WakeSettings::default()
            });
        status.is_orchestrator = is_orchestrator(&state, chat_id);
        status
    }

    /// The user is focused / typing / has a draft in `chat_id`.
    pub async fn note_user_activity(&self, chat_id: &str, draft_non_empty: Option<bool>) {
        let now = self.now();
        lock(&self.inner.state)
            .core
            .note_user_activity(chat_id, now, draft_non_empty);
        self.evaluate(chat_id, false).await;
    }

    /// "Deliver now": skips the window and the user-activity hold, never a
    /// running turn or a queued user message.
    pub async fn deliver_now(&self, chat_id: &str) -> WakeStatus {
        self.evaluate(chat_id, true).await;
        self.status(chat_id)
    }

    // ── ingestion (event driven) ──────────────────────────────────────────

    /// Consume the input bridge and end-of-turn signals, without relying on a
    /// coalescing session watch to observe every prompt or completed turn.
    pub async fn ingest_worker_signal(&self, signal: crate::sessions::WorkerSignal) {
        use crate::sessions::WorkerSignal;
        // A newly assigned worker can ask before either workspace watcher
        // catches up. Refresh its Space so project-destination orchestrators
        // receive the event as well as its direct parent.
        let worker = match &signal {
            WorkerSignal::Input { chat_id, .. } | WorkerSignal::ToolFailures { chat_id, .. } => {
                chat_id
            }
        };
        if let Ok(chats) = self.inner.workspace.read_chats() {
            let space_id = chats
                .iter()
                .find(|chat| &chat.id == worker)
                .and_then(|chat| chat.space_id.clone());
            self.ingest_chats(&chats).await;
            if let Some(space_id) = space_id {
                let workspace = self.inner.workspace.clone();
                if let Ok(snapshot) = tokio::task::spawn_blocking(move || {
                    let inputs = workspace.work_outline_inputs(&space_id);
                    mast_doc::materialize_work_outline(&space_id, &inputs)
                })
                .await
                {
                    self.ingest_snapshot(&snapshot).await;
                }
            }
        }
        let now = self.now();
        let (worker, kind, detail, key) = match signal {
            WorkerSignal::Input { chat_id, input } => {
                let question = input
                    .questions
                    .iter()
                    .map(|q| q.question.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                let approval = input.questions.iter().any(|q| {
                    let header = q.header.to_lowercase();
                    q.command_approval.is_some() || header.starts_with("approve ") || header.contains("approval") || header.contains("permission")
                });
                let kind = if approval { "approval" } else { "input" };
                let detail = format!("is waiting for {kind}: {} (answer in Needs You)", truncate(&question, 300));
                let key = format!("input:{}:{}", chat_id, input.request_id);
                (chat_id, WakeEventKind::AwaitingInput, detail, key)
            }
            WorkerSignal::ToolFailures { chat_id, failures } => {
                let detail = format!(
                    "turn ended after {} failed Mast tool call{} ({})",
                    failures.count,
                    if failures.count == 1 { "" } else { "s" },
                    failures.tools.join(", ")
                );
                let key = format!("tool-fail:{}:{}", chat_id, uuid::Uuid::new_v4());
                (chat_id, WakeEventKind::ToolCallsFailed, detail, key)
            }
        };
        let mut recorded = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let line = format!("{} — {detail}", label(&state, &worker));
            for orch in parents_of(&state, &worker) {
                let event = make_event(&mut state, &worker, kind, key.clone(), &line, now);
                state.core.record(&orch, event, now);
                recorded.insert(orch);
            }
        }
        self.after_ingest(recorded).await;
    }

    /// Session transitions of workers (errored, turn finished) and of
    /// orchestrators (turn start/end re-evaluates held events).
    pub async fn ingest_sessions(&self, sessions: &[Session]) {
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        let mut finished = Vec::new();
        // Recovery may read a journal once; never do that under the wake lock.
        let limited_sessions: HashSet<String> = sessions.iter()
            .filter(|session| session.status == SessionStatus::Errored)
            .filter(|session| self.inner.sessions.usage_limit_reset(&session.chat_id).is_some_and(|at| at > now))
            .map(|session| session.chat_id.clone()).collect();
        {
            let mut state = lock(&self.inner.state);
            let seeded = state.sessions_seeded;
            for session in sessions {
                let prev = state
                    .statuses
                    .insert(session.chat_id.clone(), session.status);
                let in_flight = matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                );
                self.inner.event_turns.observe(&session.chat_id, in_flight);
                if in_flight && prev.is_none_or(|p| !is_in_flight(p)) {
                    state.core.note_turn_start(&session.chat_id, now);
                    // This is seen after the turn began: a declaration the
                    // worker already made in it stays.
                    crate::agent_tools::clear_awaiting_background(
                        &session.chat_id,
                        session.started_at.map(|started| started.timestamp_millis()),
                    );
                    if let Some(last_message) = self.last_external_message(&session.chat_id) {
                        let user_sent_during_turn = session.started_at.is_some_and(|started| {
                            last_message
                                .as_ref()
                                .is_some_and(|(_, at)| *at > started.timestamp_millis())
                        });
                        state.turn_starts.insert(
                            session.chat_id.clone(),
                            TurnStart {
                                last_message_id: last_message.map(|(id, _)| id),
                                last_completed_turn: session.last_completed_turn.clone(),
                                user_sent_during_turn,
                            },
                        );
                    }
                }
                // A turn that ended (or parked) has no tool call open.
                if !in_flight && prev.is_some_and(is_in_flight) {
                    crate::tool_watch::chat_turn_boundary(&session.chat_id);
                }
                if !seeded {
                    continue;
                }
                let Some(prev) = prev else { continue };
                let kind = match (is_in_flight(prev), session.status) {
                    (_, s) if s == prev => continue,
                    (_, SessionStatus::Errored) => WakeEventKind::Errored,
                    (true, SessionStatus::Idle) => WakeEventKind::TurnFinished,
                    _ => continue,
                };
                let limited_error = kind == WakeEventKind::Errored
                    && limited_sessions.contains(&session.chat_id);
                if kind == WakeEventKind::TurnFinished || limited_error {
                    if let Some(start) = state.turn_starts.remove(&session.chat_id) {
                        finished.push((session.clone(), start));
                    }
                } else {
                    state.turn_starts.remove(&session.chat_id);
                }
                let summary = match kind {
                    WakeEventKind::Errored => "errored (read_chat for details)",
                    _ => "finished a turn",
                };
                let key = format!(
                    "{}:{}:{}",
                    kind_tag(kind),
                    session.chat_id,
                    now / WAKE_WINDOW_MS
                );
                let line = format!("{} — {summary}", label(&state, &session.chat_id));
                for orch in parents_of(&state, &session.chat_id) {
                    let event =
                        make_event(&mut state, &session.chat_id, kind, key.clone(), &line, now);
                    state.core.record(&orch, event, now);
                    recorded.insert(orch);
                }
            }
            state.sessions_seeded = true;
        }
        for (session, start) in finished {
            self.continue_worker(&session, start).await;
        }
        self.after_ingest(recorded).await;
    }

    /// The latest externally submitted message; Mast's system messages are
    /// written as user entries by the queue and do not reset the budget.
    fn last_external_message(&self, chat_id: &str) -> Option<Option<(String, i64)>> {
        let handle = self.inner.doc_host.open(chat_id).ok()?;
        let entries = handle.doc().read_entries().ok()?;
        Some(entries.into_iter().rev().find(|entry| {
            entry.role == mast_doc::MessageRole::User
                && !entry.parts.iter().any(|part| matches!(part,
                    mast_doc::MessagePart::Text { text, .. } if text.starts_with(mast_proto::SYSTEM_MESSAGE_PREFIX)))
        }).map(|entry| (entry.id, entry.created_at)))
    }

    async fn continue_worker(&self, session: &Session, start: TurnStart) {
        let worker = &session.chat_id;
        if let Some(reset) = self.inner.sessions.usage_limit_reset(worker).filter(|at| *at > self.now()) {
            let this = self.clone();
            let session = session.clone();
            let delay = (reset - self.now()) as u64;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if let Some(current) = this.inner.sessions.session_status(&session.chat_id)
                    && current.status == session.status
                    && current.updated_at == session.updated_at
                    && current.last_completed_turn == session.last_completed_turn
                    && this.inner.sessions.usage_limit_reset(&session.chat_id).is_none_or(|at| at <= this.now())
                {
                    this.continue_worker_ready(&current, start, true).await;
                }
            });
            return;
        }
        self.continue_worker_ready(session, start, false).await;
    }

    async fn continue_worker_ready(&self, session: &Session, start: TurnStart, limited: bool) {
        let worker = &session.chat_id;
        // A worker the credit fallback handed to a successor is not resumed
        // beside it when its limit resets.
        if crate::credit_fallback::has_successor(&self.inner.workspace, worker) {
            return;
        }
        if !self.hosted_here(worker) || (session.status != SessionStatus::Idle
            && !(limited && session.status == SessionStatus::Errored)) {
            return;
        }
        let Ok(chats) = self.inner.workspace.read_chats() else {
            return;
        };
        let Some(space_id) = chats
            .iter()
            .find(|chat| &chat.id == worker)
            .and_then(|chat| chat.space_id.clone())
        else {
            return;
        };
        self.ingest_chats(&chats).await;
        let workspace = self.inner.workspace.clone();
        let Ok(snapshot) = tokio::task::spawn_blocking(move || {
            mast_doc::materialize_work_outline(&space_id, &workspace.work_outline_inputs(&space_id))
        })
        .await
        else {
            return;
        };
        if !snapshot.available {
            return;
        }
        self.ingest_snapshot(&snapshot).await;
        let Some(item) = snapshot
            .items
            .iter()
            .find(|item| item.chat_ids.contains(worker))
        else {
            return;
        };
        if snapshot.assignments.iter().find(|assignment| assignment.chat_id == *worker)
            .and_then(|assignment| assignment.generation.as_deref())
            .is_some_and(|generation| self.inner.workspace.has_final_handback_for_assignment(worker, generation))
        {
            return;
        }
        let Some(last_message) = self.last_external_message(worker) else {
            return;
        };
        let last_message_id = last_message.map(|(id, _)| id);
        let Ok(handle) = self.inner.doc_host.open(worker) else {
            return;
        };
        let Ok(queue) = handle.doc().read_queue() else {
            return;
        };
        let action_open = snapshot
            .user_action_requests
            .iter()
            .any(|request| request.chat_id == *worker && request.is_unresolved());
        let now = self.now();
        let recorded = {
            let mut state = lock(&self.inner.state);
            let Some(orch) = parents_of(&state, worker).into_iter().next() else {
                return;
            };
            let settings = state
                .settings_overrides
                .get(&orch)
                .copied()
                .unwrap_or_else(|| WakeSettings {
                    auto_continue: state.auto_continue_default,
                    ..WakeSettings::default()
                });
            let mut next = state.auto_continue.clone();
            let decision = next.decide(auto_continue::Turn {
                worker_id: worker,
                item_id: &item.id,
                last_message_id: last_message_id.as_deref(),
                enabled: settings.enabled && settings.auto_continue,
                item_open: item.status != mast_proto::WorkItemStatus::Completed
                    && item.archived_at.is_none(),
                item_waiting: item.waiting_until.is_some(),
                action_open,
                completed: limited || (session.last_completed_turn.is_some()
                    && session.last_completed_turn != start.last_completed_turn),
                awaiting_input: session.awaiting_input.is_some(),
                tool_failures: session.last_turn_tool_failures.is_some(),
                user_sent_during_turn: start.user_sent_during_turn
                    || last_message_id != start.last_message_id,
                queue_nonempty: !queue.is_empty() || self.inner.sessions.turn_in_flight(worker),
                command_running: crate::agent_tools::chat_has_running_command(worker)
                    || crate::agent_tools::is_awaiting_background(worker, chrono::Utc::now().timestamp_millis()),
            });
            match decision {
                auto_continue::Decision::None => {
                    if state.auto_continue != next {
                        state.auto_continue = next;
                        self.persist(&state);
                    }
                    None
                }
                auto_continue::Decision::Continue(count) => {
                    let text = format!(
                        "[Mast system message, not from the user: auto-continue {count}/3]\nYour work item is still open. Continue the task; if you are done, mark the item completed or raise a Required Action (kind handoff) — if you're blocked, raise one with kind ask."
                    );
                    match self.inner.doc_host.queue_event_message(worker, &text, &crate::new_id()) {
                        Ok(id) => {
                            self.inner.event_turns.register(worker, id, Vec::new());
                            state.auto_continue = next;
                            self.persist(&state);
                        }
                        Err(err) => {
                            tracing::warn!(chat = %worker, error = %err, "worker auto-continue queue failed")
                        }
                    }
                    None
                }
                auto_continue::Decision::ReportStopped => {
                    state.core.set_settings(&orch, settings);
                    let line = format!(
                        "{} — worker stopped with its item open after 3 auto-continues",
                        label(&state, worker)
                    );
                    let event = make_event(
                        &mut state,
                        worker,
                        WakeEventKind::AutoContinueStopped,
                        format!("auto-continue:{worker}:{}", uuid::Uuid::new_v4()),
                        &line,
                        now,
                    );
                    state.core.record(&orch, event, now);
                    state.auto_continue = next;
                    self.persist(&state);
                    Some(orch)
                }
            }
        };
        if let Some(orch) = recorded {
            self.after_ingest(HashSet::from([orch])).await;
        }
    }

    /// Chat rows: parent links, titles, and PR links (PR opened).
    pub async fn ingest_chats(&self, chats: &[Chat]) {
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        let mut archived = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            let seeded = state.chats_seeded;
            for chat in chats {
                let info = ChatInfo {
                    title: chat.title.clone(),
                    ticket: firm_link(chat.linked_ticket_id.as_ref(), chat.linked_ticket_source),
                    pr_url: firm_link(chat.linked_pr_url.as_ref(), chat.linked_pr_source),
                    parent: chat.parent_chat_id.clone(),
                    archived: chat.archived,
                    last_active: chat.last_message_at.unwrap_or(chat.created_at).timestamp_millis(),
                };
                let prev = state.chats.insert(chat.id.clone(), info.clone());
                if info.archived && prev.as_ref().is_none_or(|previous| !previous.archived) {
                    archived.push(chat.id.clone());
                }
                if !seeded {
                    continue;
                }
                if let Some(url) = &info.pr_url
                    && prev.as_ref().is_none_or(|p| p.pr_url.as_ref() != Some(url))
                {
                    let line = format!("{} — PR opened: {url}", label(&state, &chat.id));
                    for orch in parents_of(&state, &chat.id) {
                        let event = make_event(
                            &mut state,
                            &chat.id,
                            WakeEventKind::PrOpened,
                            format!("pr:{}:{url}", chat.id),
                            &line,
                            now,
                        );
                        state.core.record(&orch, event, now);
                        recorded.insert(orch);
                    }
                }
            }
            state.chats_seeded = true;
        }
        self.after_ingest(recorded).await;
        for chat in archived {
            self.evaluate(&chat, false).await;
        }
    }

    /// Whether any current orchestrator has enabled PR-merged wakes.
    pub fn accepts_pr_merged(&self, worker_chat_id: &str) -> bool {
        let state = lock(&self.inner.state);
        parents_of(&state, worker_chat_id).iter().any(|orch|
            state.core.settings(orch).wakes_for(WakeEventKind::PrMerged))
    }

    /// Queue a worker's merged PR for its orchestrators.
    pub async fn note_pr_merged(&self, worker_chat_id: &str, pr_url: &str, detail: &str) {
        let now = self.now();
        let mut recorded = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let line = format!("{} — PR merged: {detail}", label(&state, worker_chat_id));
            for orch in parents_of(&state, worker_chat_id) {
                let event = make_event(
                    &mut state,
                    worker_chat_id,
                    WakeEventKind::PrMerged,
                    format!("prm:{worker_chat_id}:{pr_url}"),
                    &line,
                    now,
                );
                state.core.record(&orch, event, now);
                recorded.insert(orch);
            }
        }
        self.after_ingest(recorded).await;
    }

    /// Tell a project's Manager that a queued item is ready (project work
    /// queue). Only the device hosting the Manager delivers; the usual batching
    /// and user-first rules apply. Returns whether this device hosts the
    /// Manager, i.e. whether the offer is made here (even if the wake was
    /// deduped, repeated for an unread earlier offer, or held back by the settings).
    pub async fn note_queue_ready(&self, manager: &str, item_id: &str, summary: &str, nonce: &str) -> bool {
        if !self.hosted_here(manager) {
            return false;
        }
        let now = self.now();
        let held = {
            let mut state = lock(&self.inner.state);
            // An offer the Manager has not read yet says it already. A Manager
            // handed the previous one's held wakes holds the earlier offer
            // under another nonce, and a second line would only repeat it.
            if state.core.holds_kind_with_key_prefix(manager, WakeEventKind::QueueReady, &format!("queue:{item_id}:")) {
                tracing::debug!(manager, item_id, "queue offer already held unread; not repeated");
                return true;
            }
            let event = make_event(
                &mut state,
                manager,
                WakeEventKind::QueueReady,
                format!("queue:{item_id}:{nonce}"),
                summary,
                now,
            );
            state.core.record(manager, event, now)
        };
        if held {
            self.after_ingest(HashSet::from([manager.to_owned()])).await;
        }
        true
    }

    /// The orchestrator an external run's wake goes to, when this device
    /// hosts it and its master wake switch is on: the run's owner chat if that
    /// is an orchestrator, else the orchestrator of the owner or of a chat on
    /// the run's item.
    pub fn external_run_target(&self, owner: Option<&str>, item_chats: &[String]) -> Option<String> {
        let state = lock(&self.inner.state);
        let resolve = |chat: &str| -> Option<String> {
            if is_orchestrator(&state, chat) {
                return Some(chat.to_owned());
            }
            parents_of(&state, chat).into_iter().next()
        };
        let target = owner
            .and_then(&resolve)
            .or_else(|| item_chats.iter().find_map(|chat| resolve(chat)))?;
        drop(state);
        self.hosted_here(&target).then_some(target)
    }

    /// Queue an external run's change for `orchestrator`. It follows the
    /// master wake switch only, and rides the usual batching and user-first rules.
    pub async fn note_external_run_changed(&self, orchestrator: &str, key: &str, summary: &str) {
        let now = self.now();
        let held = {
            let mut state = lock(&self.inner.state);
            let event = make_event(
                &mut state,
                orchestrator,
                WakeEventKind::ExternalRunChanged,
                format!("extrun:{key}"),
                summary,
                now,
            );
            state.core.record(orchestrator, event, now)
        };
        if held {
            self.after_ingest(HashSet::from([orchestrator.to_owned()])).await;
        }
    }

    /// A worker's report to its parent chat (`report_to_parent`, or
    /// `send_message` to the parent): queued as a "worker report" event and
    /// delivered in the next "Worker updates:" batch under the usual
    /// idle / user-first rules. Parent only, same Space, and only when this
    /// device hosts the parent (only the host delivers).
    pub async fn report_to_parent(
        &self,
        from: &str,
        text: &str,
    ) -> Result<mast_proto::ReportToParentResult, String> {
        // A worker a paired peer started, with a parent on the peer's device:
        // queue the report for the peer to collect.
        if let Some(sink) = self.inner.remote_reports.get().cloned() {
            let title = self.inner.workspace.chat(from).ok().flatten().and_then(|chat| chat.title);
            let (from, text) = (from.to_owned(), text.to_owned());
            let routed = tokio::task::spawn_blocking(move || sink.report(&from, title.as_deref(), &text))
                .await
                .map_err(|error| format!("remote report worker failed: {error}"))?;
            if let Some(result) = routed {
                return result;
            }
        }
        self.report_to_parent_inner(from, text, None).await
    }

    /// Deliver a report a paired device collected for one of this device's
    /// chats: the same "Worker updates" path as a local worker's report, keyed
    /// by the report's id so a replay (a lost acknowledgement, a restart) is a
    /// no-op. `Err` means try again later; `Ok` means the sender may forget it.
    pub async fn report_from_peer(
        &self,
        peer: &str,
        report: &mast_proto::peer_link::PeerReportItem,
    ) -> Result<PeerReportOutcome, String> {
        let (body, _) = mast_proto::cap_worker_report(&report.text);
        if body.is_empty() {
            return Ok(PeerReportOutcome::Dropped("the report is empty"));
        }
        let (parent_id, worker) = {
            let state = lock(&self.inner.state);
            let mut parent = report.parent_chat_id.clone();
            for _ in 0..state.handed_off_to.len() {
                match state.handed_off_to.get(&parent) {
                    Some(next) => parent = next.clone(),
                    None => break,
                }
            }
            (parent, format!("peer:{peer}:{}", report.chat_id))
        };
        // A read error is retried (nothing is acknowledged); only a missing chat drops the report.
        let Some(parent) = self.inner.workspace.chat(&parent_id).map_err(|e| format!("could not read the parent chat: {e}"))? else {
            return Ok(PeerReportOutcome::Dropped("the parent chat no longer exists"));
        };
        if parent.device_id != self.inner.doc_host.device_id() {
            return Ok(PeerReportOutcome::Dropped("the parent chat is hosted on another device"));
        }
        // An archived parent cannot take a report, and retrying never helps: the
        // report goes to the Manager of the parent's work item, or is dropped.
        let (parent_id, rerouted_from) = if parent.archived {
            match self.manager_for_archived(&parent) {
                Some(manager) => (manager, Some(parent_id)),
                None => return Ok(PeerReportOutcome::Dropped("the parent chat is archived and its work item has no Manager here")),
            }
        } else {
            (parent_id, None)
        };
        let now = self.now();
        {
            let mut state = lock(&self.inner.state);
            if !state.core.settings(&parent_id).wakes_for(WakeEventKind::WorkerReport) {
                return Ok(PeerReportOutcome::ReportsOff);
            }
            let title = match report.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                Some(title) => format!("{title} [{peer}]"),
                None => format!("worker on {peer}"),
            };
            let info = state.chats.entry(worker.clone()).or_default();
            info.title = Some(title);
            info.parent = Some(parent_id.clone());
            let first = body.lines().next().unwrap_or_default();
            let line = format!("{} — worker report: {}", label(&state, &worker), truncate(first, 120));
            let mut event = make_event(
                &mut state,
                &worker,
                WakeEventKind::WorkerReport,
                format!("peerrep:{peer}:{}", report.id),
                &line,
                now,
            );
            event.id = mast_proto::derive_id(&format!("{peer}:{}", report.id), "peer-report-wake");
            let rerouted = rerouted_from.as_deref().map(|from| format!(" Its parent chat {from} is archived, so it came to you as the Manager."));
            event.report = Some(format!(
                "[remote worker, chat {} on paired device {peer}: read it through that peer's Mast MCP server.{}]\n{body}",
                report.chat_id,
                rerouted.unwrap_or_default()
            ));
            state.core.record(&parent_id, event, now);
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.persist_held())
            .await
            .map_err(|error| format!("wake persistence worker failed: {error}"))??;
        self.evaluate(&parent_id, false).await;
        Ok(PeerReportOutcome::Queued)
    }

    /// Tell the parents a paired device's workers may be reporting to that the
    /// device cannot be reached, so reports queued there are waiting. One notice
    /// per `outage` per parent (the key repeats, so a replay is a no-op).
    /// Returns how many parent chats on this device were told.
    pub async fn note_peer_unreachable(&self, peer: &str, outage: &str, parents: &[String], detail: &str) -> usize {
        let now = self.now();
        let mut told = HashSet::new();
        for parent in parents {
            if !self.hosted_here(parent) || self.inner.workspace.chat(parent).ok().flatten().is_none_or(|c| c.archived) {
                continue;
            }
            let worker = format!("peer:{peer}");
            let mut state = lock(&self.inner.state);
            if !state.core.settings(parent).wakes_for(WakeEventKind::WorkerReport) {
                continue;
            }
            state.chats.entry(worker.clone()).or_default().title = Some(format!("paired device {peer}"));
            let line = format!("{} — worker reports from {peer} are not arriving: the link to it is down", label(&state, &worker));
            let mut event = make_event(&mut state, &worker, WakeEventKind::WorkerReport, format!("peerdown:{peer}:{outage}"), &line, now);
            event.id = mast_proto::derive_id(&format!("{peer}:{outage}:{parent}"), "peer-unreachable-wake");
            event.report = Some(format!(
                "[The link to paired device {peer} is down: {detail}. Reports from your workers on it stay queued there and arrive once the link is back. Check the saved URL in `mast peer list`, or read a worker's chat through that peer's Mast MCP server.]"
            ));
            if state.core.record(parent, event, now) {
                told.insert(parent.clone());
            }
        }
        if !told.is_empty() {
            let this = self.clone();
            if let Ok(Err(error)) = tokio::task::spawn_blocking(move || this.persist_held()).await {
                tracing::warn!(%error, "could not persist the peer-unreachable notice");
            }
            for parent in &told {
                self.evaluate(parent, false).await;
            }
        }
        told.len()
    }

    /// The live Manager chat on this device for the work item an archived chat
    /// was assigned to, or the nearest ancestor item that has one.
    fn manager_for_archived(&self, archived: &mast_proto::Chat) -> Option<String> {
        let space = archived.space_id.as_deref()?;
        let mut item = self.inner.workspace.chat_assigned_work_item(&archived.id, space)?;
        for _ in 0..32 {
            if let Some(manager) = self.inner.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &item) {
                let live = self.inner.workspace.chat(&manager).ok().flatten().is_some_and(|c| !c.archived);
                return (live && self.hosted_here(&manager)).then_some(manager);
            }
            item = self.inner.workspace.effective_work_item_parent(space, &item)?;
        }
        None
    }

    /// Queue an authenticated intent through the same delivery path as a
    /// worker report, retaining its typed identity for the durable receipt.
    pub async fn report_final_handback(
        &self,
        intent: &FinalHandbackIntent,
    ) -> Result<mast_proto::ReportToParentResult, String> {
        if !self.inner.workspace.final_handback_is_locally_authenticated(intent) {
            return Err("final handback has no local authentication receipt".into());
        }
        self.report_to_parent_inner(&intent.worker_chat_id, &intent.report, Some(intent)).await
    }

    /// Retry locally authenticated intents after recovery. Unavailable
    /// history or changed ownership is retained for a later explicit review.
    pub async fn recover_final_handbacks(&self) {
        self.recover_final_handbacks_for(None).await;
    }

    async fn recover_final_handbacks_for(&self, parent: Option<&str>) {
        let workspace = self.inner.workspace.clone();
        let Ok(intents) = tokio::task::spawn_blocking(move || workspace.locally_authenticated_final_handbacks()).await else {
            tracing::warn!("could not load final handback recovery intents");
            return;
        };
        for intent in intents {
            if self.inner.cancel.is_cancelled() { break; }
            if parent.is_some_and(|parent| parent != intent.parent_chat_id) {
                continue;
            }
            if let Err(error) = self.report_final_handback(&intent).await {
                tracing::warn!(%error, worker = %intent.worker_chat_id, "final handback remains pending");
            }
        }
    }

    async fn reconcile_final_handback_archives(&self) {
        let this = self.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || {
            let workspace = &this.inner.workspace;
            for intent in workspace.locally_authenticated_final_handbacks() {
                if this.inner.cancel.is_cancelled() {
                    break;
                }
                if let Err(error) = this.archive_delivered_handback(&intent) {
                    tracing::warn!(%error, worker = %intent.worker_chat_id, "could not archive final handback");
                }
            }
        }).await {
            tracing::warn!(%error, "final handback archive worker failed");
        }
    }

    fn archive_delivered_handback(&self, intent: &FinalHandbackIntent) -> Result<(), crate::EngineError> {
        let turns = &self.inner.event_turns;
        let expected = handback_identity(intent);
        if intent.keep_chat || !matches!(turns.final_handback_receipt(&expected), Ok(true)) {
            return Ok(());
        }
        let workspace = &self.inner.workspace;
        if !workspace.final_handback_archive_pending(intent) {
            return Ok(());
        }
        let host = &self.inner.doc_host;
        let worker = &intent.worker_chat_id;
        let handle = match host.loaded(worker) {
            Some(handle) => handle,
            None if host.scheduled_run_doc(worker)?.is_some() => host.open(worker)?,
            None => return Ok(()),
        };
        let Ok(_drain) = handle.drain_lock.try_lock() else {
            return Ok(());
        };
        let doc = handle.doc();
        let archived = workspace.archive_final_handback_if(intent, &handle.work_ingress, || {
            let delivered = matches!(turns.final_handback_receipt(&expected), Ok(true));
            let queued = handle.has_relayed_work()
                || self.inner.sessions.turn_in_flight(worker)
                || self.inner.sessions.has_any_pending_input(worker)
                || self.inner.sessions.has_interrupted_turn(worker)
                || DocHost::session_doc_has_queued_turn(doc)?;
            let no_owned_subagents = !queued && !doc.read_entries()?.iter()
                .flat_map(|entry| &entry.parts)
                .any(|part| {
                    use mast_doc::{MessagePart, SubagentStatus};
                    matches!(part, MessagePart::Tool {
                        subagent_status: Some(SubagentStatus::Running),
                        ..
                    } | MessagePart::Tool {
                        subagent_ref: Some(_),
                        subagent_status: None,
                        ..
                    })
                });
            Ok((delivered, queued, no_owned_subagents))
        })?;
        drop(_drain);
        if archived {
            workspace.persist_registry_now()?;
        }
        Ok(())
    }

    async fn report_to_parent_inner(
        &self,
        from: &str,
        text: &str,
        final_intent: Option<&FinalHandbackIntent>,
    ) -> Result<mast_proto::ReportToParentResult, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("text is empty".into());
        }
        let (parent_id, truncated) = {
            let mut state = lock(&self.inner.state);
            if let Some(intent) = final_intent {
                if self.inner.event_turns.final_handback_receipt(&handback_identity(intent))? {
                    return Ok(mast_proto::ReportToParentResult {
                        parent_chat_id: intent.parent_chat_id.clone(),
                        delivery: "delivered".into(),
                        held_reason: None,
                        truncated: false,
                    });
                }
                if lock(&self.inner.event_turns.persistence).as_ref().is_some_and(|persistence| {
                    persistence.unavailable.contains(&intent.parent_chat_id)
                }) {
                    return Err("final handback recipient recovery is unavailable".into());
                }
            }
            let worker = self
                .inner
                .workspace
                .chat(from)
                .ok()
                .flatten()
                .ok_or_else(|| format!("unknown chat {from}"))?;
            if final_intent.is_some() && worker.device_id != self.inner.doc_host.device_id() {
                return Err("worker is hosted on another device; final handback remains pending".into());
            }
            let parent_id = worker
                .parent_chat_id
                .clone()
                .filter(|p| p != from)
                .ok_or("this chat has no parent chat, so there is nobody to report to")?;
            if let Some(intent) = final_intent
                && (intent.parent_chat_id != parent_id || worker.space_id.as_deref() != Some(intent.space_id.as_str()))
            {
                return Err("worker parent or Space changed since final handback".into());
            }
            let parent = self
                .inner
                .workspace
                .chat(&parent_id)
                .ok()
                .flatten()
                .ok_or("the parent chat no longer exists")?;
            if parent.archived {
                return Err("the parent chat is archived; hand off or unarchive it before reporting".into());
            }
            if parent.space_id != worker.space_id {
                return Err("the parent chat is in another Space; reports never cross Spaces".into());
            }
            if parent.device_id != self.inner.doc_host.device_id() {
                return Err(
                    "the parent chat runs on another device; worker reports are only delivered \
                     by the device that hosts the parent (read_chat / Required Actions still work)"
                        .into(),
                );
            }
            let (report, truncated) = mast_proto::cap_worker_report(text);
            if !state.core.settings(&parent_id).wakes_for(WakeEventKind::WorkerReport) {
                if final_intent.is_some() {
                    return Err("parent worker reports are disabled; final handback remains pending".into());
                }
                return Ok(mast_proto::ReportToParentResult {
                    parent_chat_id: parent_id,
                    delivery: "parentReportsOff".into(),
                    held_reason: None,
                    truncated,
                });
            }
            let in_delivery = final_intent.is_some_and(|intent| {
                lock(&self.inner.event_turns.persistence).as_ref().is_some_and(|persistence| {
                    persistence.store.orchestrators.values().flatten().any(|entry| {
                        entry.event.final_handback.as_ref().is_some_and(|tag| tag.operation_id == intent.operation_id)
                            && entry.message_id.is_some()
                    })
                })
            });
            let now = self.now();
            if !in_delivery {
                let info = state.chats.entry(from.to_owned()).or_default();
                info.title = worker.title.clone();
                info.ticket = worker.linked_ticket_id.clone();
                info.parent = Some(parent_id.clone());
                let first = report.lines().next().unwrap_or_default();
                let line = format!(
                    "{} — worker report: {}",
                    label(&state, from),
                    truncate(first, 120)
                );
                let key = format!("rep:{from}:{}", report_hash(&report, now / WAKE_WINDOW_MS));
                let mut event = make_event(
                    &mut state,
                    from,
                    WakeEventKind::WorkerReport,
                    key,
                    &line,
                    now,
                );
                if let Some(intent) = final_intent {
                    event.id = mast_proto::derive_id(&intent.operation_id, "final-handback-wake");
                    event.key = format!("final:{}", intent.operation_id);
                    event.final_handback = Some(handback_identity(intent));
                }
                event.report = Some(report);
                state.core.record(&parent_id, event, now);
            }
            (parent_id, truncated)
        };
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.persist_held()).await
            .map_err(|error| format!("wake persistence worker failed: {error}"))??;
        self.evaluate(&parent_id, false).await;
        let status = self.status(&parent_id);
        if !status.settings.wakes_for(WakeEventKind::WorkerReport) {
            if final_intent.is_some() {
                return Err("parent worker reports are disabled; final handback remains pending".into());
            }
            return Ok(mast_proto::ReportToParentResult {
                parent_chat_id: parent_id,
                delivery: "parentReportsOff".into(),
                held_reason: None,
                truncated,
            });
        }
        Ok(mast_proto::ReportToParentResult {
            parent_chat_id: parent_id,
            delivery: "queuedForParent".into(),
            held_reason: status.hold_reason,
            truncated,
        })
    }

    /// Required Actions raised/resolved, plus the project-destination
    /// worker relation, from one Space's snapshot.
    pub async fn ingest_snapshot(&self, snapshot: &WorkOutlineSnapshot) {
        if !snapshot.available {
            return;
        }
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            state
                .destination_parents
                .insert(snapshot.space_id.clone(), destination_parents(snapshot));
            let seeded = !state.seeded_spaces.insert(snapshot.space_id.clone());
            for request in &snapshot.user_action_requests {
                let open = request.is_unresolved();
                let key = format!(
                    "act:{}:{}",
                    request.work_item_id,
                    request.dedupe_key.as_deref().unwrap_or(&request.text)
                );
                let prev = state.requests.insert(request.id.clone(), open);
                if !seeded || request.chat_id.is_empty() {
                    continue;
                }
                let was_open = prev.unwrap_or(false);
                let (kind, event_key, text) = if open && !was_open {
                    let mut text = request.text.trim().to_owned();
                    if let Some(info) = request.info.as_deref().map(str::trim)
                        && !info.is_empty()
                    {
                        text = format!("{text}: {}", truncate(info, 160));
                    }
                    (WakeEventKind::ActionRaised, key, text)
                } else if !open && was_open {
                    (
                        WakeEventKind::ActionResolved,
                        resolved_key(&key),
                        format!("resolved: {}", request.text.trim()),
                    )
                } else {
                    continue;
                };
                let mut line = format!("{} — {text}", label(&state, &request.chat_id));
                if let Some(pr) = state
                    .chats
                    .get(&request.chat_id)
                    .and_then(|c| c.pr_url.clone())
                {
                    line.push_str(&format!(" (PR {pr})"));
                }
                if kind == WakeEventKind::ActionRaised {
                    line.push_str(&format!(" [request {}]", request.id));
                }
                for orch in parents_of(&state, &request.chat_id) {
                    if kind == WakeEventKind::ActionResolved
                        && request.resolution.as_ref().is_some_and(|resolution| {
                            !resolution.answered_by_user()
                                && resolution.by.kind == WorkOutlineActorKind::Agent
                                && resolution.by.chat_id.as_deref() == Some(orch.as_str())
                        })
                    {
                        continue;
                    }
                    let event = make_event(
                        &mut state,
                        &request.chat_id,
                        kind,
                        event_key.clone(),
                        &line,
                        now,
                    );
                    state.core.record(&orch, event, now);
                    recorded.insert(orch);
                }
            }
        }
        self.after_ingest(recorded).await;
    }

    async fn after_ingest(&self, recorded: HashSet<String>) {
        if !recorded.is_empty() {
            let this = self.clone();
            match tokio::task::spawn_blocking(move || this.persist_held()).await {
                Ok(Ok(())) => {},
                Ok(Err(error)) => tracing::warn!(%error, "could not save held wake events"),
                Err(error) => tracing::warn!(%error, "held wake persistence worker failed"),
            }
        }
        // Orchestrators that recorded something, plus any holding events
        // whose blocker (a running turn) may just have ended.
        let mut targets: HashSet<String> = recorded;
        targets.extend(lock(&self.inner.state).core.orchestrators_with_held());
        for orch in targets {
            self.evaluate(&orch, false).await;
        }
    }

    fn persist_held(&self) -> Result<(), String> {
        let state = lock(&self.inner.state);
        self.inner.event_turns.synchronize_held(&state.core.held_events())
    }

    // ── delivery ──────────────────────────────────────────────────────────

    fn ctx(&self, orch: &str) -> Ctx {
        let queue_nonempty = self
            .inner
            .doc_host
            .open(orch)
            .ok()
            .and_then(|h| h.doc().read_queue().ok())
            .is_some_and(|q| !q.is_empty());
        Ctx {
            busy: self.inner.sessions.turn_in_flight(orch),
            queue_nonempty,
        }
    }

    /// Whether this device hosts the chat `orch`.
    pub fn hosts(&self, orch: &str) -> bool {
        self.hosted_here(orch)
    }

    /// Whether this device hosts `orch` (only the host delivers).
    fn hosted_here(&self, orch: &str) -> bool {
        self.inner
            .workspace
            .chat(orch)
            .ok()
            .flatten()
            .is_some_and(|c| c.device_id == self.inner.doc_host.device_id())
    }

    /// Decide for one orchestrator and act: deliver, arm a one-shot timer, or
    /// keep holding until the next event (turn end, draft cleared).
    pub async fn evaluate(&self, orch: &str, force: bool) {
        let this = self.clone();
        let orch = orch.to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || this.evaluate_blocking(&orch, force)).await {
            tracing::warn!(%error, "wake evaluation worker failed");
        }
    }

    fn evaluate_blocking(&self, orch: &str, force: bool) {
        let now = self.now();
        let limit_reset = self.inner.sessions.usage_limit_reset(orch).filter(|at| *at > now);
        // Keep selection and queueing under the same lock: several watchers can
        // evaluate the same orchestrator concurrently.
        let mut state = lock(&self.inner.state);
        if state.automatic_handoff_pending.contains_key(orch) {
            return;
        }
        // Sample the queue under the same lock that queues a wake row. A
        // context read before the lock could be stale (empty) while another
        // evaluation queues a row, letting a second wake row queue behind it.
        let ctx = self.ctx(orch);
        let archived = self.inner.workspace.chat(orch).ok().flatten().is_some_and(|chat| chat.archived);
        if archived || !state.core.settings(orch).enabled {
            {
                let mut guard = lock(&self.inner.event_turns.persistence);
                if let Some(persistence) = guard.as_mut() {
                    let before = persistence.store.clone();
                    if let Some(entries) = persistence.store.orchestrators.get_mut(orch) {
                        for entry in entries { entry.discarded = true; }
                    }
                    if let Err(error) = persistence.write() {
                        persistence.store = before;
                        tracing::warn!(%error, chat = %orch, "could not persist wake discard");
                        return;
                    }
                }
            }
            let pending = self.inner.event_turns.pending_message(orch);
            self.inner.doc_host.pull_pending_event_message(orch);
            let _ = self.inner.event_turns.take_cancelled(orch);
            state.core.discard_held(orch);
            state.timers.remove(orch);
            if let Some(persistence) = lock(&self.inner.event_turns.persistence).as_mut() {
                if let Some(entries) = persistence.store.orchestrators.get_mut(orch) {
                    for entry in entries.iter_mut() {
                        if pending.as_ref().is_some_and(|id| entry.message_id.as_ref() == Some(id))
                            && !self.inner.event_turns.has_started(orch)
                            && self.inner.event_turns.pending_message(orch).is_none() {
                            let id = entry.message_id.take().unwrap();
                            entry.retire(orch, &id);
                        }
                    }
                    entries.retain(|entry| entry.message_id.is_some() || !entry.retired_messages.is_empty());
                }
                if let Err(error) = persistence.write() {
                    tracing::warn!(%error, chat = %orch, "could not save wake discard receipt");
                }
            }
            if let Some(id) = pending { self.inner.event_turns.schedule_prune(orch, &id); }
            tracing::info!(chat = %orch, "discarded wakes for archived or disabled coordinator");
            return;
        }
        if state.hand_off_pending.contains(orch) {
            return;
        }
        if let Some(reset) = limit_reset {
            drop(state);
            self.arm_timer(orch, reset);
            return;
        }
        let cancelled = self.inner.event_turns.take_cancelled(orch);
        if !cancelled.is_empty() {
            state.core.restore(orch, cancelled, now);
            if let Err(error) = self.inner.event_turns.synchronize_held(&state.core.held_events()) {
                tracing::warn!(%error, "could not save restored wake events");
                return;
            }
        }
        if let Some(persistence) = lock(&self.inner.event_turns.persistence).as_ref() {
            state.core.discard_cancelled_handbacks(orch, &persistence.store.cancelled_handbacks);
        }
        match state.core.decide(orch, now, ctx, force) {
            Decision::Nothing | Decision::Hold(_) => {}
            Decision::WaitUntil { at_ms, .. } => {
                drop(state);
                self.arm_timer(orch, at_ms);
            }
            Decision::Deliver(batch) => {
                if !self.hosted_here(orch) {
                    return;
                }
                let text = batch.render(|id| {
                    let title = self.inner.workspace.chat(id).ok().flatten().and_then(|chat| chat.title);
                    wake_label(id, title.as_deref())
                });
                let message_id = crate::new_id();
                if let Err(error) = self.inner.event_turns.synchronize_held(&state.core.held_events()) {
                    tracing::warn!(%error, chat = %orch, "could not save held wake events");
                    return;
                }
                if let Err(error) = self.inner.event_turns.mark_queued(orch, &batch.events, &message_id, &text) {
                    tracing::warn!(%error, chat = %orch, "could not save wake queue receipt");
                    return;
                }
                self.inner.event_turns.register(orch, message_id.clone(), batch.events.clone());
                match self.inner.doc_host.queue_event_message(orch, &text, &message_id) {
                    Ok(message_id) => {
                        let _ = message_id;
                        state.core.delivered(orch, &batch, now);
                    }
                    Err(err) => {
                        self.inner.event_turns.clear(orch);
                        if let Err(error) = self.inner.event_turns.synchronize_held(&state.core.held_events()) {
                            tracing::warn!(%error, "could not save held wake events");
                        }
                        tracing::warn!(chat = %orch, error = %err, "orchestrator wake delivery failed");
                    }
                }
            }
        }
    }

    fn arm_timer(&self, orch: &str, at_ms: i64) {
        let now = self.now();
        {
            let mut state = lock(&self.inner.state);
            if state
                .timers
                .get(orch)
                .is_some_and(|&t| t > now && t <= at_ms)
            {
                return; // an earlier timer re-evaluates (and re-arms) first
            }
            state.timers.insert(orch.to_owned(), at_ms);
        }
        let this = self.clone();
        let orch = orch.to_owned();
        let wait = (at_ms - now).max(0) as u64;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            {
                let mut state = lock(&this.inner.state);
                if state.timers.get(&orch) == Some(&at_ms) {
                    state.timers.remove(&orch);
                }
            }
            this.evaluate(&orch, false).await;
        });
    }
}

fn handback_transcript_hash(entry: &mast_doc::SessionMessageEntry) -> Option<String> {
    match (entry.role, entry.parts.as_slice()) {
        (mast_doc::MessageRole::User, [mast_doc::MessagePart::Text { text, .. }]) => {
            Some(queue_text_hash(text))
        }
        _ => None,
    }
}

fn handback_identity(intent: &FinalHandbackIntent) -> core::FinalHandback {
    core::FinalHandback {
        operation_id: intent.operation_id.clone(),
        worker_chat_id: intent.worker_chat_id.clone(),
        parent_chat_id: intent.parent_chat_id.clone(),
        project_id: intent.project_id.clone(),
        assignment_generation: intent.assignment_generation.clone(),
    }
}

fn is_in_flight(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Working | SessionStatus::AwaitingInput
    )
}

fn kind_tag(kind: WakeEventKind) -> &'static str {
    match kind {
        WakeEventKind::ActionRaised => "act",
        WakeEventKind::ActionResolved => "res",
        WakeEventKind::Errored => "err",
        WakeEventKind::TurnFinished => "turn",
        WakeEventKind::PrOpened => "pr",
        WakeEventKind::PrMerged => "prm",
        WakeEventKind::WorkerReport => "rep",
        WakeEventKind::AwaitingInput => "input",
        WakeEventKind::ToolCallsFailed => "tool-fail",
        WakeEventKind::AutoContinueStopped => "auto-continue-stopped",
        WakeEventKind::QueueReady => "queue",
        WakeEventKind::LongToolCall => "long-tool",
        WakeEventKind::ExternalRunChanged => "ext-run",
    }
}

fn report_hash(text: &str, bucket: i64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (text, bucket).hash(&mut h);
    h.finish()
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

/// A link worth naming a worker by or announcing: anything but a bare
/// `mentioned` one, which only says the chat's text contained the id or URL
/// (a planner discussing ENG-3219 is not working on it, and a PR it read
/// about was not opened by it).
fn firm_link(value: Option<&String>, source: Option<mast_proto::ChatLinkSource>) -> Option<String> {
    match source {
        Some(mast_proto::ChatLinkSource::Mentioned) | None => None,
        Some(_) => value.cloned(),
    }
}

fn wake_label(chat_id: &str, title: Option<&str>) -> String {
    let short: String = chat_id.chars().take(12).collect();
    match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => format!("{title} (chat {short})"),
        None => format!("chat {short}"),
    }
}

/// "ENG-2818" when the worker has a firmly linked ticket, else its title, then the
/// chat id prefix the tools accept.
fn label(state: &State, chat_id: &str) -> String {
    let short: String = chat_id.chars().take(12).collect();
    let info = state.chats.get(chat_id);
    let name = info
        .and_then(|c| c.ticket.clone())
        .or_else(|| info.and_then(|c| c.title.clone()))
        .filter(|n| !n.trim().is_empty());
    match name {
        Some(name) => format!("{} (chat {short})", name.trim()),
        None => format!("chat {short}"),
    }
}

fn make_event(
    state: &mut State,
    worker: &str,
    kind: WakeEventKind,
    key: String,
    summary: &str,
    now: i64,
) -> WakeEvent {
    WakeEvent {
        id: state.core.next_event_id(),
        key,
        worker_chat_id: worker.to_owned(),
        worker_title: state.chats.get(worker).and_then(|c| c.title.clone()),
        kind,
        summary: summary.to_owned(),
        report: None,
        final_handback: None,
        at_ms: now,
        repeats: 0,
    }
}

/// Orchestrators told about `worker`: its parent chat, and the chats whose
/// project worker destination contains the worker's item.
fn parents_of(state: &State, worker: &str) -> Vec<String> {
    let redirect = |candidate: &str| {
        let mut current = candidate;
        for _ in 0..state.handed_off_to.len() {
            match state.handed_off_to.get(current) {
                Some(next) => current = next,
                None => break,
            }
        }
        current.to_owned()
    };
    let live = |id: &str| state.chats.get(id).is_some_and(|chat| !chat.archived);
    if let Some(parent) = state.chats.get(worker).and_then(|chat| chat.parent.as_deref()) {
        let parent = redirect(parent);
        if parent != worker && live(&parent) {
            return if state.core.settings(&parent).enabled { vec![parent] } else { Vec::new() };
        }
    }
    state.destination_parents.values()
        .filter_map(|map| map.get(worker))
        .flatten()
        .map(|candidate| redirect(candidate))
        .filter(|candidate| candidate != worker && live(candidate) && state.core.settings(candidate).enabled)
        .max_by_key(|candidate| (state.chats[candidate].last_active, candidate.clone()))
        .into_iter().collect()
}

fn is_orchestrator(state: &State, chat_id: &str) -> bool {
    state.chats.get(chat_id).is_some_and(|chat| !chat.archived)
        && state.core.settings(chat_id).enabled
        && (state.chats.values().any(|chat| chat.parent.as_deref() == Some(chat_id))
            || state.destination_parents.values().any(|map| map.values().any(|candidates| candidates.iter().any(|candidate| candidate == chat_id))))
}

/// Worker chat → orchestrators, for one Space: a chat on item X is the
/// orchestrator of every chat on an item inside X's project worker
/// destination (`worker_destination_for`).
fn destination_parents(snapshot: &WorkOutlineSnapshot) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for item in &snapshot.items {
        if item.chat_ids.is_empty() {
            continue;
        }
        let Some(destination) = snapshot.worker_destination_for(&item.id) else {
            continue;
        };
        for worker_item in &snapshot.items {
            if worker_item.chat_ids.is_empty() || !snapshot.is_within(destination, &worker_item.id)
            {
                continue;
            }
            for worker in &worker_item.chat_ids {
                for orch in &item.chat_ids {
                    if orch != worker {
                        let list = out.entry(worker.clone()).or_default();
                        if !list.contains(orch) {
                            list.push(orch.clone());
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod hand_off_tests {
    use super::*;

    #[test]
    fn destination_selects_most_recent_enabled_live_coordinator() {
        let mut state = State::default();
        state.chats.insert("worker".into(), ChatInfo::default());
        state.chats.insert("first".into(), ChatInfo { last_active: 1, ..Default::default() });
        state.chats.insert("second".into(), ChatInfo { last_active: 2, ..Default::default() });
        state.destination_parents.entry("space".into()).or_default().insert("worker".into(), vec!["first".into(), "second".into()]);
        assert_eq!(parents_of(&state, "worker"), vec!["second"]);
        state.chats.get_mut("second").unwrap().archived = true;
        assert_eq!(parents_of(&state, "worker"), vec!["first"]);
        state.core.set_settings("first", WakeSettings { enabled: false, ..WakeSettings::default() });
        assert!(parents_of(&state, "worker").is_empty());
    }

    #[test]
    fn live_parent_precedes_destination_and_disabled_parent_does_not_fall_back() {
        let mut state = State::default();
        state.chats.insert("worker".into(), ChatInfo { parent: Some("parent".into()), ..Default::default() });
        state.chats.insert("parent".into(), ChatInfo::default());
        state.chats.insert("destination".into(), ChatInfo::default());
        state.destination_parents.entry("space".into()).or_default().insert("worker".into(), vec!["destination".into()]);
        assert_eq!(parents_of(&state, "worker"), vec!["parent"]);
        state.core.set_settings("parent", WakeSettings { enabled: false, ..WakeSettings::default() });
        assert!(parents_of(&state, "worker").is_empty());
        state.chats.get_mut("parent").unwrap().archived = true;
        assert_eq!(parents_of(&state, "worker"), vec!["destination"]);
    }

    #[test]
    fn hand_off_redirects_project_destination_wakes() {
        let mut state = State::default();
        state.chats.insert("worker".into(), ChatInfo { parent: Some("successor".into()), ..Default::default() });
        state.chats.insert("successor".into(), ChatInfo::default());
        state.destination_parents.entry("space".into()).or_default().insert("worker".into(), vec!["source".into()]);
        state.handed_off_to.insert("source".into(), "successor".into());
        assert_eq!(parents_of(&state, "worker"), vec!["successor"]);
    }
}

#[cfg(test)]
mod firm_link_tests {
    use super::firm_link;
    use mast_proto::ChatLinkSource;

    #[test]
    fn a_mentioned_link_neither_labels_nor_announces() {
        let id = "ENG-3219".to_string();
        assert_eq!(firm_link(Some(&id), Some(ChatLinkSource::Mentioned)), None);
        assert_eq!(firm_link(Some(&id), None), None);
        assert_eq!(
            firm_link(Some(&id), Some(ChatLinkSource::Manual)),
            Some(id.clone())
        );
        assert_eq!(firm_link(Some(&id), Some(ChatLinkSource::Branch)), Some(id));
    }
}
