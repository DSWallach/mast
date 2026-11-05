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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::FutureExt;

use mast_proto::{
    Chat, Session, SessionStatus, WakeEventKind, WakeSettings, WakeStatus, WorkOutlineActorKind,
    WorkOutlineSnapshot,
};

pub use self::core::{
    Batch, Ctx, DEDUPE_TTL_MS, Decision, USER_QUIET_MS, WAKE_WINDOW_MS, WakeCore, WakeEvent,
    resolved_key,
};
use crate::doc_host::DocHost;
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
}

impl EventTurns {
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

    /// The queue row was pulled back before it ran: its events go back to
    /// the wake service.
    pub fn cancel_pending(&self, chat_id: &str) {
        if let Some(turn) = lock(&self.turns).remove(chat_id) {
            lock(&self.cancelled)
                .entry(chat_id.to_owned())
                .or_default()
                .extend(turn.events);
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
}

struct Inner {
    state: Mutex<State>,
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    event_turns: Arc<EventTurns>,
    clock: Clock,
    store: Option<PathBuf>,
    /// Ends the watchers: they hold the sessions/doc-host graph alive.
    cancel: tokio_util::sync::CancellationToken,
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
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                sessions,
                doc_host,
                workspace,
                event_turns,
                clock,
                store,
                cancel: tokio_util::sync::CancellationToken::new(),
            }),
        }
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

    fn now(&self) -> i64 {
        (self.inner.clock)()
    }

    fn spawn_watchers(&self) {
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
        {
            let mut state = lock(&self.inner.state);
            state.core.set_settings(chat_id, settings);
            state
                .settings_overrides
                .insert(chat_id.to_owned(), settings);
            self.persist(&state);
        }
        self.evaluate(chat_id, false).await;
        self.status(chat_id)
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
        state.core.transfer_held(from, to);
        state.handed_off_to.insert(from.to_owned(), to.to_owned());
        state.timers.remove(from);
        self.persist(state);
    }

    /// Hand off direct workers and their pending wakes to another orchestrator.
    pub async fn hand_off_orchestrator(&self, from: &str, to: &str, actor: &str) -> Result<Vec<String>, String> {
        let workers = self.hand_off_orchestrator_without_delivery(from, to, actor)?;
        self.finish_orchestrator_hand_off(to);
        self.evaluate(to, false).await;
        Ok(workers)
    }

    pub(crate) fn hand_off_orchestrator_without_delivery(&self, from: &str, to: &str, actor: &str) -> Result<Vec<String>, String> {
        let workers = {
            let mut state = lock(&self.inner.state);
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
            workers
        };
        Ok(workers)
    }

    pub(crate) fn finish_orchestrator_hand_off(&self, to: &str) {
        lock(&self.inner.state).hand_off_pending.remove(to);
    }

    fn persist(&self, state: &State) {
        let Some(path) = &self.inner.store else {
            return;
        };
        let settings = serde_json::json!({
            "autoContinue": state.auto_continue_default,
            "orchestrators": state.settings_overrides,
            "autoContinueWorkers": state.auto_continue,
            "handedOffTo": state.handed_off_to,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&settings) {
            let _ = std::fs::write(path, bytes);
        }
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
                    header.contains("approval") || header.contains("permission")
                });
                let kind = if approval { "approval" } else { "input" };
                let detail = format!("is waiting for {kind}: {}", truncate(&question, 300));
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
                if kind == WakeEventKind::TurnFinished {
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
        if !self.hosted_here(worker) || session.status != SessionStatus::Idle {
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
                enabled: settings.auto_continue,
                item_open: item.status != mast_proto::WorkItemStatus::Completed
                    && item.archived_at.is_none(),
                action_open,
                completed: session.last_completed_turn.is_some()
                    && session.last_completed_turn != start.last_completed_turn,
                awaiting_input: session.awaiting_input.is_some(),
                tool_failures: session.last_turn_tool_failures.is_some(),
                user_sent_during_turn: start.user_sent_during_turn
                    || last_message_id != start.last_message_id,
                queue_nonempty: !queue.is_empty() || self.inner.sessions.turn_in_flight(worker),
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
                    match self.inner.doc_host.queue_event_message(worker, &text) {
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
        {
            let mut state = lock(&self.inner.state);
            let seeded = state.chats_seeded;
            for chat in chats {
                let info = ChatInfo {
                    title: chat.title.clone(),
                    ticket: firm_link(chat.linked_ticket_id.as_ref(), chat.linked_ticket_source),
                    pr_url: firm_link(chat.linked_pr_url.as_ref(), chat.linked_pr_source),
                    parent: chat.parent_chat_id.clone(),
                };
                let prev = state.chats.insert(chat.id.clone(), info.clone());
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
    }

    /// A worker's PR merged (fed by whoever learns PR state).
    pub async fn note_pr_merged(&self, worker_chat_id: &str, pr_url: &str) {
        let now = self.now();
        let mut recorded = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let line = format!("{} — PR merged: {pr_url}", label(&state, worker_chat_id));
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
        let text = text.trim();
        if text.is_empty() {
            return Err("text is empty".into());
        }
        let (parent_id, truncated) = {
        let mut state = lock(&self.inner.state);
        let worker = self
            .inner
            .workspace
            .chat(from)
            .ok()
            .flatten()
            .ok_or_else(|| format!("unknown chat {from}"))?;
        let parent_id = worker
            .parent_chat_id
            .clone()
            .filter(|p| p != from)
            .ok_or("this chat has no parent chat, so there is nobody to report to")?;
        let parent = self
            .inner
            .workspace
            .chat(&parent_id)
            .ok()
            .flatten()
            .ok_or("the parent chat no longer exists")?;
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
        let now = self.now();
        {
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
            event.report = Some(report);
            state.core.record(&parent_id, event, now);
        }
        (parent_id, truncated)
        };
        self.evaluate(&parent_id, false).await;
        let status = self.status(&parent_id);
        if !status.settings.wakes_for(WakeEventKind::WorkerReport) {
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
                let id8 = request.id.chars().take(8).collect::<String>();
                let mut line = format!("{} — {text}", label(&state, &request.chat_id));
                if let Some(pr) = state
                    .chats
                    .get(&request.chat_id)
                    .and_then(|c| c.pr_url.clone())
                {
                    line.push_str(&format!(" (PR {pr})"));
                }
                if kind == WakeEventKind::ActionRaised {
                    line.push_str(&format!(" [request {id8}]"));
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
        // Orchestrators that recorded something, plus any holding events
        // whose blocker (a running turn) may just have ended.
        let mut targets: HashSet<String> = recorded;
        targets.extend(lock(&self.inner.state).core.orchestrators_with_held());
        for orch in targets {
            self.evaluate(&orch, false).await;
        }
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
        let now = self.now();
        let ctx = self.ctx(orch);
        // Keep selection and queueing under the same lock: several watchers can
        // evaluate the same orchestrator concurrently.
        let mut state = lock(&self.inner.state);
        if state.hand_off_pending.contains(orch) {
            return;
        }
        let cancelled = self.inner.event_turns.take_cancelled(orch);
        if !cancelled.is_empty() {
            state.core.restore(orch, cancelled, now);
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
                let text = batch.render();
                match self.inner.doc_host.queue_event_message(orch, &text) {
                    Ok(message_id) => {
                        self.inner
                            .event_turns
                            .register(orch, message_id, batch.events.clone());
                        state.core.delivered(orch, &batch, now);
                    }
                    Err(err) => {
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
        at_ms: now,
    }
}

/// Orchestrators told about `worker`: its parent chat, and the chats whose
/// project worker destination contains the worker's item.
fn parents_of(state: &State, worker: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(parent) = state.chats.get(worker).and_then(|c| c.parent.clone())
        && parent != worker
    {
        out.push(parent);
    }
    for map in state.destination_parents.values() {
        for orch in map.get(worker).into_iter().flatten() {
            if orch != worker && !out.contains(orch) {
                out.push(orch.clone());
            }
        }
    }
    let mut redirected = Vec::new();
    for mut parent in out {
        let mut remaining = state.handed_off_to.len();
        while remaining > 0 {
            let Some(next) = state.handed_off_to.get(&parent) else { break };
            parent = next.clone();
            remaining -= 1;
        }
        if parent != worker && !redirected.contains(&parent) {
            redirected.push(parent);
        }
    }
    redirected
}

fn is_orchestrator(state: &State, chat_id: &str) -> bool {
    state
        .chats
        .values()
        .any(|c| c.parent.as_deref() == Some(chat_id))
        || state
            .destination_parents
            .values()
            .any(|m| m.values().any(|orchs| orchs.iter().any(|o| o == chat_id)))
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
    fn hand_off_redirects_project_destination_wakes() {
        let mut state = State::default();
        state.chats.insert("worker".into(), ChatInfo { parent: Some("successor".into()), ..Default::default() });
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
