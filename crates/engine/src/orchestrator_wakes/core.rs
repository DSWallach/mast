//! The pure batching and user-first policy behind orchestrator wakes. No I/O,
//! no clock of its own: every call takes `now` (epoch ms), so tests drive it
//! with a fake clock.

use std::collections::{HashMap, VecDeque};

use mast_proto::{WakeEventKind, WakeEventView, WakeHoldReason, WakeSettings, WakeStatus};
use serde::{Deserialize, Serialize};

/// How long events are collected before they go out as one message. At most
/// one wake is delivered per window.
pub const WAKE_WINDOW_MS: i64 = 45_000;
/// Events are held while the user interacted with the chat this recently
/// (focused, typed, sent).
pub const USER_QUIET_MS: i64 = 120_000;
/// A delivered event key suppresses an identical event for this long.
pub const DEDUPE_TTL_MS: i64 = 10 * 60_000;
/// A lone stopped auto-continue is delivered once it is this old, so a worker
/// that gave up is never silent. Finished turns never wake on their own.
pub const ROUTINE_STOP_MAX_HOLD_MS: i64 = 30 * 60_000;
/// Recent events remembered per orchestrator for the "unseen" marks.
const LOG_CAP: usize = 50;

/// One worker event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeEvent {
    pub id: String,
    /// Identity for dedupe (same action re-raised = same key).
    pub key: String,
    pub worker_chat_id: String,
    pub worker_title: Option<String>,
    pub kind: WakeEventKind,
    /// One line, as the orchestrator will read it.
    pub summary: String,
    /// Full report text of a `WorkerReport` event (already capped).
    pub report: Option<String>,
    /// Present only for an explicit final handback, never inferred from text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_handback: Option<FinalHandback>,
    pub at_ms: i64,
    /// Earlier identical-kind routine events folded into this one (finished
    /// turns of one worker), so the batch can still say how many there were.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeats: u32,
    /// Delivered as soon as the orchestrator is idle: skips the batch window,
    /// the routine hold and the recent-activity hold (never a running turn,
    /// a queued user message or a non-empty draft). Set only for autonomous
    /// workers' approvals and stopped auto-continues.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub urgent: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Stable identity carried from an authenticated final handback to its
/// delivery receipt. A generation is specific to one worker assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalHandback {
    pub operation_id: String,
    pub worker_chat_id: String,
    pub parent_chat_id: String,
    pub project_id: String,
    pub assignment_generation: String,
}

/// What the engine knows about the orchestrator chat right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ctx {
    /// A turn is in flight (running or parked on a question).
    pub busy: bool,
    /// The user has queued messages in this chat.
    pub queue_nonempty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing is held.
    Nothing,
    /// Deliver this batch now.
    Deliver(Batch),
    /// Held until `at_ms`; re-evaluate then.
    WaitUntil { at_ms: i64, reason: WakeHoldReason },
    /// Held until something changes (turn end, queue drain, draft cleared).
    Hold(WakeHoldReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub events: Vec<WakeEvent>,
}

impl Batch {
    /// The one message the orchestrator receives.
    pub fn render(&self, mut label: impl FnMut(&str) -> String) -> String {
        let mut out = format!(
            "{}, not from the user: worker updates for you, the \
             orchestrator. Act on them with your Mast tools (work_outline_read_subtree, \
             read_chat, send_message); ignore ones you've handled. Direct the user to Needs You for a worker's live approval or question.]\n\
             Worker updates:",
            mast_proto::SYSTEM_MESSAGE_PREFIX
        );
        for event in self.events.iter().filter(|e| !is_routine(e.kind)) {
            out.push_str("\n- ");
            if matches!(event.kind, WakeEventKind::QueueReady | WakeEventKind::ExternalRunChanged) {
                out.push_str(&event.summary);
                continue;
            }
            out.push_str(&label(&event.worker_chat_id));
            let short: String = event.worker_chat_id.chars().take(12).collect();
            let marker = format!("chat {short}");
            let detail = event.summary.find(&marker).and_then(|index| {
                event.summary[index + marker.len()..]
                    .strip_prefix(") — ")
                    .or_else(|| event.summary[index + marker.len()..].strip_prefix(" — "))
            }).or_else(|| event.summary.split_once(" — ").map(|(_, detail)| detail));
            if let Some(detail) = detail {
                out.push_str(" — ");
                out.push_str(detail);
            } else {
                out.push_str(" — ");
                out.push_str(&event.summary);
            }
            if let Some(report) = &event.report {
                for line in report.lines() {
                    out.push_str("\n  | ");
                    out.push_str(line);
                }
            }
        }
        // Routine events collapse to one line per chat: counts, not a list.
        let mut routine: Vec<&str> = Vec::new();
        for event in self.events.iter().filter(|e| is_routine(e.kind)) {
            if !routine.contains(&event.worker_chat_id.as_str()) {
                routine.push(&event.worker_chat_id);
            }
        }
        for worker in routine {
            let mine = || self.events.iter()
                .filter(|e| is_routine(e.kind) && e.worker_chat_id == worker);
            let turns: u32 = mine().filter(|e| e.kind == WakeEventKind::TurnFinished)
                .map(|e| 1 + e.repeats).sum();
            let mut parts = Vec::new();
            if turns > 0 {
                parts.push(format!("{turns} turn{} finished", if turns == 1 { "" } else { "s" }));
            }
            if mine().any(|e| e.kind == WakeEventKind::AutoContinueStopped) {
                parts.push("auto-continue stopped".to_owned());
            }
            out.push_str("\n- ");
            out.push_str(&label(worker));
            out.push_str(" — ");
            out.push_str(&parts.join(", "));
        }
        out
    }
}

#[derive(Debug, Clone)]
struct LogEntry {
    event: WakeEvent,
    delivered: bool,
}

#[derive(Debug, Default)]
struct Orch {
    settings: WakeSettings,
    held: Vec<WakeEvent>,
    /// When the first held event arrived (start of the batch window).
    window_start_ms: Option<i64>,
    last_delivery_ms: Option<i64>,
    last_turn_start_ms: i64,
    draft_non_empty: bool,
    last_user_ms: Option<i64>,
    delivered_keys: HashMap<String, i64>,
    delivered_ids: HashMap<String, i64>,
    log: VecDeque<LogEntry>,
}

#[derive(Debug, Default)]
pub struct WakeCore {
    orchs: HashMap<String, Orch>,
}

impl WakeCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_event_id(&mut self) -> String {
        crate::new_id()
    }

    pub fn remember_delivered_id(&mut self, orch: &str, id: &str, now: i64) {
        let ids = &mut self.orchs.entry(orch.to_owned()).or_default().delivered_ids;
        ids.insert(id.to_owned(), now);
        if ids.len() > 500 && let Some(oldest) = ids.iter().min_by_key(|(_, at)| *at).map(|(id, _)| id.clone()) {
            ids.remove(&oldest);
        }
    }

    pub fn settings(&self, orch: &str) -> WakeSettings {
        self.orchs.get(orch).map(|o| o.settings).unwrap_or_default()
    }

    /// Load or change settings. Turning wakes (or a kind) off drops held
    /// events of the kinds that no longer wake.
    pub fn set_settings(&mut self, orch: &str, settings: WakeSettings) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.settings = settings;
        o.held.retain(|e| wakes(&settings, e));
        if o.held.is_empty() {
            o.window_start_ms = None;
        }
    }

    /// Copy settings during a hand-off without discarding already held events.
    pub fn set_settings_preserving_held(&mut self, orch: &str, settings: WakeSettings) {
        self.orchs.entry(orch.to_owned()).or_default().settings = settings;
    }

    pub fn all_settings(&self) -> HashMap<String, WakeSettings> {
        self.orchs
            .iter()
            .filter(|(_, o)| o.settings != WakeSettings::default())
            .map(|(id, o)| (id.clone(), o.settings))
            .collect()
    }

    pub fn has_held(&self, orch: &str) -> bool {
        self.orchs.get(orch).is_some_and(|o| !o.held.is_empty())
    }

    pub fn discard_held(&mut self, orch: &str) {
        if let Some(state) = self.orchs.get_mut(orch) {
            state.held.clear();
            state.window_start_ms = None;
        }
    }

    pub fn discard_cancelled_handbacks(
        &mut self,
        orch: &str,
        cancelled: &std::collections::HashSet<String>,
    ) {
        if let Some(state) = self.orchs.get_mut(orch) {
            state.held.retain(|event| event.final_handback.as_ref()
                .is_none_or(|tag| !cancelled.contains(&tag.operation_id)));
            if state.held.is_empty() {
                state.window_start_ms = None;
            }
        }
    }

    /// Transfer undelivered events without reapplying settings or deduplication.
    /// The caller serializes this with delivery and copies settings separately.
    pub fn transfer_held(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let Some(source) = self.orchs.get_mut(from) else {
            return;
        };
        let held = std::mem::take(&mut source.held);
        let window_start = source.window_start_ms.take();
        if held.is_empty() {
            return;
        }
        let destination = self.orchs.entry(to.to_owned()).or_default();
        for event in held {
            if !destination.held.iter().any(|existing| existing.id == event.id) {
                push_log(destination, event.clone(), false);
                destination.held.push(event);
            }
        }
        if !destination.held.is_empty() {
            destination.window_start_ms = match (destination.window_start_ms, window_start) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
    }

    /// Orchestrators with held events, for re-evaluation after a state change.
    pub fn orchestrators_with_held(&self) -> Vec<String> {
        self.orchs
            .iter()
            .filter(|(_, o)| !o.held.is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Whether `orch` already holds, undelivered, an event of `kind` whose key
    /// starts with `key_prefix`.
    pub fn holds_kind_with_key_prefix(&self, orch: &str, kind: WakeEventKind, key_prefix: &str) -> bool {
        self.orchs
            .get(orch)
            .is_some_and(|o| o.held.iter().any(|e| e.kind == kind && e.key.starts_with(key_prefix)))
    }

    /// Snapshot events awaiting queue acceptance.
    pub fn held_events(&self) -> HashMap<String, Vec<WakeEvent>> {
        self.orchs.iter().filter(|(_, orch)| !orch.held.is_empty())
            .map(|(id, orch)| (id.clone(), orch.held.clone())).collect()
    }

    /// Record a worker event. Always logged (for the unseen marks); held for
    /// delivery only when the settings wake for its kind and it is not a
    /// duplicate. Returns whether it was held.
    pub fn record(&mut self, orch: &str, event: WakeEvent, now: i64) -> bool {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.delivered_keys.retain(|_, at| now - *at < DEDUPE_TTL_MS);
        if o.held.iter().any(|e| e.id == event.id || e.key == event.key)
            || o.delivered_ids.contains_key(&event.id) || o.delivered_keys.contains_key(&event.key) {
            return false;
        }
        // Raised then resolved before the orchestrator heard of it: neither
        // needs to be said.
        if event.kind == WakeEventKind::ActionResolved
            && let Some(raised) = o.held.iter().position(|e| {
                e.kind == WakeEventKind::ActionRaised
                    && e.worker_chat_id == event.worker_chat_id
                    && resolved_key(&e.key) == event.key
            })
        {
            o.held.remove(raised);
            if o.held.is_empty() {
                o.window_start_ms = None;
            }
            push_log(o, event, false);
            return false;
        }
        let wakes = wakes(&o.settings, &event);
        push_log(o, event.clone(), false);
        if !wakes {
            return false;
        }
        if event.kind == WakeEventKind::TurnFinished {
            // Only the latest finished turn per worker is worth saying.
            let mut event = event;
            let mut folded = 0;
            o.held.retain(|e| {
                let same = e.kind == WakeEventKind::TurnFinished
                    && e.worker_chat_id == event.worker_chat_id;
                if same {
                    folded += 1 + e.repeats;
                }
                !same
            });
            event.repeats = folded;
            if o.held.is_empty() {
                o.window_start_ms = Some(now);
            }
            o.held.push(event);
            return true;
        }
        // The batch window opens with the first event that can wake; routine
        // events held before it must not shorten the coalescing wait.
        if o.held.iter().all(|e| is_routine(e.kind)) && !is_routine(event.kind) {
            o.window_start_ms = Some(now);
        } else if o.held.is_empty() {
            o.window_start_ms = Some(now);
        }
        o.held.push(event);
        true
    }

    /// The orchestrator started a turn: everything before now is seen.
    pub fn note_turn_start(&mut self, orch: &str, now: i64) {
        self.orchs
            .entry(orch.to_owned())
            .or_default()
            .last_turn_start_ms = now;
    }

    /// The user is acting in this chat.
    pub fn note_user_activity(&mut self, orch: &str, now: i64, draft_non_empty: Option<bool>) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.last_user_ms = Some(now);
        if let Some(draft) = draft_non_empty {
            o.draft_non_empty = draft;
        }
    }

    /// Put back events whose delivery was cancelled before the orchestrator
    /// saw them (the user's message went first).
    pub fn restore(&mut self, orch: &str, events: Vec<WakeEvent>, now: i64) {
        self.restore_events(orch, events, now, false);
    }

    pub fn restore_for_hand_off(&mut self, orch: &str, events: Vec<WakeEvent>, now: i64) {
        self.restore_events(orch, events, now, true);
    }

    fn restore_events(&mut self, orch: &str, events: Vec<WakeEvent>, now: i64, preserve: bool) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        for event in events {
            o.delivered_keys.remove(&event.key);
            o.delivered_ids.remove(&event.id);
            if let Some(entry) = o.log.iter_mut().find(|l| l.event.id == event.id) {
                entry.delivered = false;
            }
            if !o.held.iter().any(|e| if preserve { e.id == event.id } else { e.key == event.key })
                && (preserve || wakes(&o.settings, &event)) {
                o.held.push(event);
            }
        }
        if !o.held.is_empty() && o.window_start_ms.is_none() {
            o.window_start_ms = Some(now);
        }
    }

    /// The hold rules. `force` is "Deliver now": it skips the window, the
    /// draft and the recent-activity hold, but never a running turn or a
    /// queued user message.
    pub fn decide(&mut self, orch: &str, now: i64, ctx: Ctx, force: bool) -> Decision {
        let Some(o) = self.orchs.get_mut(orch) else {
            return Decision::Nothing;
        };
        if o.held.is_empty() {
            return Decision::Nothing;
        }
        if ctx.busy {
            return Decision::Hold(WakeHoldReason::Busy);
        }
        if ctx.queue_nonempty {
            return Decision::Hold(WakeHoldReason::Queued);
        }
        // An urgent event goes out now, with everything held beside it. The
        // user still goes first: a draft keeps it held.
        let urgent = o.held.iter().any(|e| e.urgent);
        if urgent && !force && o.draft_non_empty {
            return Decision::Hold(WakeHoldReason::Draft);
        }
        let force = force || urgent;
        if !force && o.held.iter().all(|e| is_routine(e.kind)) {
            let stop_due = o.held.iter()
                .filter(|e| e.kind == WakeEventKind::AutoContinueStopped)
                .map(|e| e.at_ms + ROUTINE_STOP_MAX_HOLD_MS)
                .min();
            match stop_due {
                Some(at) if at > now => {
                    return Decision::WaitUntil { at_ms: at, reason: WakeHoldReason::Routine };
                }
                Some(_) => {}
                None => return Decision::Hold(WakeHoldReason::Routine),
            }
        }
        if !force {
            if o.draft_non_empty {
                return Decision::Hold(WakeHoldReason::Draft);
            }
            if let Some(at) = o.last_user_ms.map(|t| t + USER_QUIET_MS)
                && at > now
            {
                return Decision::WaitUntil {
                    at_ms: at,
                    reason: WakeHoldReason::RecentActivity,
                };
            }
            // At most one wake per window: the batch window opens with the
            // first held event and the previous delivery also closes one.
            let window_open = o.window_start_ms.unwrap_or(now) + WAKE_WINDOW_MS;
            let after_last = o.last_delivery_ms.map_or(0, |t| t + WAKE_WINDOW_MS);
            let at = window_open.max(after_last);
            if at > now {
                return Decision::WaitUntil {
                    at_ms: at,
                    reason: WakeHoldReason::Window,
                };
            }
        }
        Decision::Deliver(Batch {
            events: o.held.clone(),
        })
    }

    /// The batch went out: it is no longer held, and its keys are remembered.
    pub fn delivered(&mut self, orch: &str, batch: &Batch, now: i64) {
        let Some(o) = self.orchs.get_mut(orch) else {
            return;
        };
        let ids: Vec<&str> = batch.events.iter().map(|e| e.id.as_str()).collect();
        o.held.retain(|e| !ids.contains(&e.id.as_str()));
        for event in &batch.events {
            o.delivered_keys.insert(event.key.clone(), now);
            o.delivered_ids.insert(event.id.clone(), now);
            if o.delivered_ids.len() > 500 && let Some(oldest) = o.delivered_ids.iter().min_by_key(|(_, at)| *at).map(|(id, _)| id.clone()) {
                o.delivered_ids.remove(&oldest);
            }
            if let Some(entry) = o.log.iter_mut().find(|l| l.event.id == event.id) {
                entry.delivered = true;
            }
        }
        o.last_delivery_ms = Some(now);
        o.window_start_ms = if o.held.is_empty() { None } else { Some(now) };
    }

    /// Read model for the UI.
    pub fn status(&mut self, orch: &str, now: i64, ctx: Ctx) -> WakeStatus {
        let decision = self.decide(orch, now, ctx, false);
        let Some(o) = self.orchs.get(orch) else {
            return WakeStatus {
                chat_id: orch.to_owned(),
                ..WakeStatus::default()
            };
        };
        let view = |e: &WakeEvent, delivered: bool| WakeEventView {
            id: e.id.clone(),
            worker_chat_id: e.worker_chat_id.clone(),
            worker_title: e.worker_title.clone(),
            kind: e.kind,
            summary: e.summary.clone(),
            report: e.report.clone(),
            at_ms: e.at_ms,
            delivered,
            unseen: e.at_ms >= o.last_turn_start_ms && !delivered,
        };
        let recent: Vec<WakeEventView> = o
            .log
            .iter()
            .rev()
            .map(|l| view(&l.event, l.delivered))
            .collect();
        let (hold_reason, next_delivery_at_ms) = match decision {
            Decision::Hold(r) => (Some(r), None),
            Decision::WaitUntil { at_ms, reason } => (Some(reason), Some(at_ms)),
            Decision::Nothing | Decision::Deliver(_) => (None, None),
        };
        WakeStatus {
            chat_id: orch.to_owned(),
            is_orchestrator: false, // the service stamps this
            settings: o.settings,
            held: o.held.iter().map(|e| view(e, false)).collect(),
            hold_reason,
            next_delivery_at_ms,
            unseen_count: recent.iter().filter(|e| e.unseen).count() as u32,
            recent,
        }
    }
}

/// Whether `settings` wake for `event`. An autonomous worker's auto-continue
/// is on by its task spec, not by the orchestrator's toggle, so its stop
/// (marked urgent) follows the master switch only.
fn wakes(settings: &WakeSettings, event: &WakeEvent) -> bool {
    settings.wakes_for(event.kind)
        || (event.urgent && event.kind == WakeEventKind::AutoContinueStopped && settings.enabled)
}

/// Events that need no action by themselves and fold into the next batch.
fn is_routine(kind: WakeEventKind) -> bool {
    matches!(kind, WakeEventKind::TurnFinished | WakeEventKind::AutoContinueStopped)
}

fn push_log(o: &mut Orch, event: WakeEvent, delivered: bool) {
    o.log.push_back(LogEntry { event, delivered });
    while o.log.len() > LOG_CAP {
        o.log.pop_front();
    }
}

/// The `ActionResolved` key matching a raised key (`act:<x>` ↔ `res:<x>`).
pub fn resolved_key(raised_key: &str) -> String {
    format!(
        "res:{}",
        raised_key.strip_prefix("act:").unwrap_or(raised_key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORCH: &str = "orch";

    #[test]
    fn repeated_event_id_is_not_delivered_twice() {
        let mut core = WakeCore::new();
        let first = event(&mut core, "worker", WakeEventKind::WorkerReport, "first", 1);
        assert!(core.record(ORCH, first.clone(), 1));
        let batch = match core.decide(ORCH, WAKE_WINDOW_MS + 1, IDLE, false) {
            Decision::Deliver(batch) => batch,
            other => panic!("expected delivery: {other:?}"),
        };
        core.delivered(ORCH, &batch, WAKE_WINDOW_MS + 1);
        let mut replay = first;
        replay.key = "different-key".into();
        assert!(!core.record(ORCH, replay, WAKE_WINDOW_MS + 2));
    }

    #[test]
    fn wake_event_serialization_preserves_identity_and_report() {
        let mut core = WakeCore::new();
        let mut original = event(&mut core, "worker", WakeEventKind::WorkerReport, "report", 123);
        original.report = Some("report body".into());
        let bytes = serde_json::to_vec(&original).unwrap();
        let restored: WakeEvent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, original);
        assert_ne!(WakeCore::new().next_event_id(), original.id);
    }

    #[test]
    fn final_handback_is_explicit_and_survives_event_serialization() {
        let mut core = WakeCore::new();
        let mut report = event(&mut core, "worker", WakeEventKind::WorkerReport, "r", 1);
        let legacy = serde_json::to_value(&report).unwrap();
        assert!(legacy.get("final_handback").is_none());
        let legacy_report: WakeEvent = serde_json::from_value(legacy).unwrap();
        assert!(legacy_report.final_handback.is_none());
        report.final_handback = Some(FinalHandback {
            operation_id: "op".into(),
            worker_chat_id: "worker".into(),
            parent_chat_id: "parent".into(),
            project_id: "project".into(),
            assignment_generation: "generation".into(),
        });
        let restored: WakeEvent = serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(restored.final_handback, report.final_handback);
    }

    fn event(
        core: &mut WakeCore,
        worker: &str,
        kind: WakeEventKind,
        key: &str,
        at: i64,
    ) -> WakeEvent {
        WakeEvent {
            id: core.next_event_id(),
            key: key.into(),
            worker_chat_id: worker.into(),
            worker_title: Some(format!("title {worker}")),
            kind,
            summary: format!("{worker} — {key}"),
            report: None,
            final_handback: None,
            at_ms: at,
            repeats: 0,
            urgent: false,
        }
    }

    fn raised(core: &mut WakeCore, worker: &str, key: &str, at: i64) -> WakeEvent {
        event(
            core,
            worker,
            WakeEventKind::ActionRaised,
            &format!("act:{key}"),
            at,
        )
    }

    const IDLE: Ctx = Ctx {
        busy: false,
        queue_nonempty: false,
    };

    #[test]
    fn hand_off_transfer_held_preserves_reports_and_is_safe_to_retry() {
        let mut core = WakeCore::new();
        let mut report = event(&mut core, "worker", WakeEventKind::WorkerReport, "report", 1_000);
        report.report = Some("details".into());
        assert!(core.record("source", report.clone(), 1_000));
        core.transfer_held("source", "destination");
        core.transfer_held("source", "destination");
        assert!(!core.has_held("source"));
        assert_eq!(
            core.decide("destination", 50_000, IDLE, false),
            Decision::Deliver(Batch { events: vec![report] })
        );
    }

    #[test]
    fn a_transferred_offer_is_found_by_the_key_prefix_the_queue_uses() {
        let mut core = WakeCore::new();
        let offer = event(&mut core, "mgr", WakeEventKind::QueueReady, "queue:item-1:1:100", 1_000);
        assert!(core.record("source", offer, 1_000));
        let prefix = |item: &str| format!("queue:{item}:");
        assert!(core.holds_kind_with_key_prefix("source", WakeEventKind::QueueReady, &prefix("item-1")));
        assert!(!core.holds_kind_with_key_prefix("destination", WakeEventKind::QueueReady, &prefix("item-1")));
        core.transfer_held("source", "destination");
        assert!(core.holds_kind_with_key_prefix("destination", WakeEventKind::QueueReady, &prefix("item-1")));
        // Another item, or another kind of event, is not the same offer.
        assert!(!core.holds_kind_with_key_prefix("destination", WakeEventKind::QueueReady, &prefix("item-2")));
        assert!(!core.holds_kind_with_key_prefix("destination", WakeEventKind::WorkerReport, &prefix("item-1")));
    }

    #[test]
    fn hand_off_transfer_held_merges_existing_events_and_preserves_window() {
        let mut core = WakeCore::new();
        let source = raised(&mut core, "worker-1", "one", 1_000);
        let destination = raised(&mut core, "worker-2", "two", 10_000);
        assert!(core.record("source", source.clone(), 1_000));
        assert!(core.record("destination", destination.clone(), 10_000));
        core.transfer_held("source", "destination");
        assert_eq!(
            core.decide("destination", 30_000, IDLE, false),
            Decision::WaitUntil {
                at_ms: 1_000 + WAKE_WINDOW_MS,
                reason: WakeHoldReason::Window,
            }
        );
        assert_eq!(
            core.decide("destination", 50_000, IDLE, false),
            Decision::Deliver(Batch { events: vec![destination, source] })
        );
    }

    #[test]
    fn bare_turn_finishes_never_wake_and_ride_the_next_real_event() {
        let mut core = WakeCore::new();
        for n in 0..10 {
            let e = event(&mut core, "w", WakeEventKind::TurnFinished, &format!("turn:{n}"), n * 1_000);
            assert!(core.record(ORCH, e, n * 1_000));
        }
        assert_eq!(core.held_events()[ORCH].len(), 1);
        for now in [60_000, 10 * 60_000, 24 * 3_600_000] {
            assert_eq!(core.decide(ORCH, now, IDLE, false), Decision::Hold(WakeHoldReason::Routine));
        }
        let report = event(&mut core, "w", WakeEventKind::WorkerReport, "rep", 100_000);
        assert!(core.record(ORCH, report, 100_000));
        assert!(matches!(core.decide(ORCH, 100_000 + 1, IDLE, false),
            Decision::WaitUntil { reason: WakeHoldReason::Window, .. }));
        match core.decide(ORCH, 100_000 + WAKE_WINDOW_MS, IDLE, false) {
            Decision::Deliver(batch) => {
                assert_eq!(batch.events.len(), 2);
                let text = batch.render(|_| "Default model per role · Sonnet".into());
                let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("- ")).collect();
                assert_eq!(lines.len(), 2, "{text}");
                assert!(text.contains("- Default model per role · Sonnet — 10 turns finished"), "{text}");
            }
            other => panic!("expected delivery: {other:?}"),
        }
    }

    #[test]
    fn routine_line_names_turns_and_a_stop_once_per_chat() {
        let mut core = WakeCore::new();
        core.set_settings(ORCH, WakeSettings { auto_continue: true, ..WakeSettings::default() });
        for n in 0..4 {
            let e = event(&mut core, "w", WakeEventKind::TurnFinished, &format!("t{n}"), n);
            assert!(core.record(ORCH, e, n));
        }
        let stop = event(&mut core, "w", WakeEventKind::AutoContinueStopped, "stop", 5);
        assert!(core.record(ORCH, stop, 5));
        let err = event(&mut core, "x", WakeEventKind::Errored, "err", 6);
        assert!(core.record(ORCH, err, 6));
        let Decision::Deliver(batch) = core.decide(ORCH, 6 + WAKE_WINDOW_MS, IDLE, true) else { panic!() };
        let text = batch.render(|id| format!("chat {id}"));
        assert!(text.contains("- chat w — 4 turns finished, auto-continue stopped"), "{text}");
        assert_eq!(text.matches("chat w").count(), 1, "{text}");
        assert!(text.find("chat x").unwrap() < text.find("chat w").unwrap(), "{text}");
    }

    #[test]
    fn busy_and_queued_outrank_routine_and_turns_collapse_per_worker() {
        let mut core = WakeCore::new();
        for w in ["a", "b"] {
            let e = event(&mut core, w, WakeEventKind::TurnFinished, &format!("turn:{w}"), 0);
            assert!(core.record(ORCH, e, 0));
        }
        let busy = Ctx { busy: true, queue_nonempty: false };
        assert_eq!(core.decide(ORCH, 1, busy, false), Decision::Hold(WakeHoldReason::Busy));
        let queued = Ctx { busy: false, queue_nonempty: true };
        assert_eq!(core.decide(ORCH, 1, queued, false), Decision::Hold(WakeHoldReason::Queued));
        assert_eq!(core.status(ORCH, 1, IDLE).hold_reason, Some(WakeHoldReason::Routine));
        assert_eq!(core.held_events()[ORCH].len(), 2);
    }

    #[test]
    fn error_wakes_with_held_turn_finishes() {
        let mut core = WakeCore::new();
        let turn = event(&mut core, "w", WakeEventKind::TurnFinished, "turn", 0);
        assert!(core.record(ORCH, turn, 0));
        let err = event(&mut core, "w", WakeEventKind::Errored, "err", 1_000);
        assert!(core.record(ORCH, err, 1_000));
        assert!(matches!(core.decide(ORCH, 1_000 + WAKE_WINDOW_MS, IDLE, false), Decision::Deliver(b) if b.events.len() == 2));
    }

    #[test]
    fn routine_events_survive_hand_off_and_force_still_delivers() {
        let mut core = WakeCore::new();
        let turn = event(&mut core, "w", WakeEventKind::TurnFinished, "turn", 0);
        assert!(core.record("source", turn, 0));
        core.transfer_held("source", "destination");
        assert_eq!(core.decide("destination", 1_000, IDLE, false), Decision::Hold(WakeHoldReason::Routine));
        let report = event(&mut core, "w", WakeEventKind::WorkerReport, "rep", 2_000);
        assert!(core.record("destination", report, 2_000));
        assert!(matches!(core.decide("destination", 2_000 + WAKE_WINDOW_MS, IDLE, false), Decision::Deliver(b) if b.events.len() == 2));
        let mut other = WakeCore::new();
        let turn = event(&mut other, "w", WakeEventKind::TurnFinished, "turn", 0);
        assert!(other.record(ORCH, turn, 0));
        assert!(matches!(other.decide(ORCH, 1, IDLE, true), Decision::Deliver(_)));
    }

    #[test]
    fn lone_stopped_auto_continue_is_delivered_after_the_max_hold() {
        let mut core = WakeCore::new();
        core.set_settings(ORCH, WakeSettings { auto_continue: true, ..WakeSettings::default() });
        let stop = event(&mut core, "w", WakeEventKind::AutoContinueStopped, "stop", 0);
        assert!(core.record(ORCH, stop, 0));
        assert_eq!(core.decide(ORCH, 1_000, IDLE, false), Decision::WaitUntil {
            at_ms: ROUTINE_STOP_MAX_HOLD_MS, reason: WakeHoldReason::Routine });
        assert!(matches!(core.decide(ORCH, ROUTINE_STOP_MAX_HOLD_MS, IDLE, false), Decision::Deliver(_)));
    }

    #[test]
    fn urgent_events_skip_the_window_and_recent_activity_but_not_the_user() {
        let mut core = WakeCore::new();
        // A delivery just went out and the user was active a moment ago.
        let earlier = event(&mut core, "w", WakeEventKind::Errored, "earlier", 0);
        assert!(core.record(ORCH, earlier, 0));
        let Decision::Deliver(batch) = core.decide(ORCH, WAKE_WINDOW_MS, IDLE, false) else { panic!() };
        core.delivered(ORCH, &batch, WAKE_WINDOW_MS);
        let now = WAKE_WINDOW_MS + 10;
        core.note_user_activity(ORCH, now - 5, None);
        let turn = event(&mut core, "w", WakeEventKind::TurnFinished, "turn", now);
        assert!(core.record(ORCH, turn, now));
        let mut approval = event(&mut core, "w", WakeEventKind::WorkerAwaitingApproval, "input:w:r1", now);
        approval.urgent = true;
        assert!(core.record(ORCH, approval.clone(), now));
        // Duplicate signal for the same request: not held twice.
        let mut again = event(&mut core, "w", WakeEventKind::WorkerAwaitingApproval, "input:w:r1", now);
        again.urgent = true;
        assert!(!core.record(ORCH, again, now));
        let Decision::Deliver(batch) = core.decide(ORCH, now, IDLE, false) else { panic!("urgent waits") };
        assert_eq!(batch.events.len(), 2, "held routine events ride along");
        // A blocked orchestrator (mid-turn) or a queued user message still holds it.
        let busy = Ctx { busy: true, queue_nonempty: false };
        assert_eq!(core.decide(ORCH, now, busy, false), Decision::Hold(WakeHoldReason::Busy));
        let queued = Ctx { busy: false, queue_nonempty: true };
        assert_eq!(core.decide(ORCH, now, queued, false), Decision::Hold(WakeHoldReason::Queued));
        core.note_user_activity(ORCH, now, Some(true));
        assert_eq!(core.decide(ORCH, now, IDLE, false), Decision::Hold(WakeHoldReason::Draft));
        core.note_user_activity(ORCH, now, Some(false));
        core.delivered(ORCH, &batch, now);
        // Delivered once: the same request is never told again.
        let mut replay = approval;
        replay.id = core.next_event_id();
        assert!(!core.record(ORCH, replay, now + 1));
    }

    #[test]
    fn an_urgent_stopped_auto_continue_is_not_held_as_routine() {
        let mut core = WakeCore::new();
        let mut stop = event(&mut core, "w", WakeEventKind::AutoContinueStopped, "stop", 0);
        stop.urgent = true;
        // The orchestrator's own auto-continue toggle is off (the default).
        assert!(core.record(ORCH, stop, 0));
        assert!(matches!(core.decide(ORCH, 0, IDLE, false), Decision::Deliver(b) if b.events.len() == 1));
    }

    #[test]
    fn defaults_wake_for_required_actions_and_errors_only() {
        let s = WakeSettings::default();
        assert!(s.enabled);
        assert!(s.wakes_for(WakeEventKind::ActionRaised));
        assert!(s.wakes_for(WakeEventKind::ActionResolved));
        assert!(s.wakes_for(WakeEventKind::Errored));
        assert!(s.wakes_for(WakeEventKind::TurnFinished));
        assert!(!s.wakes_for(WakeEventKind::PrOpened));
        assert!(!s.wakes_for(WakeEventKind::PrMerged));
        let off = WakeSettings {
            enabled: false,
            ..s
        };
        assert!(WakeEventKind::ALL.iter().all(|k| !off.wakes_for(*k)));
    }

    #[test]
    fn events_batch_into_one_message_after_the_window() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 1_000);
        let b = event(&mut core, "w2", WakeEventKind::Errored, "err:w2", 20_000);
        assert!(core.record(ORCH, a, 1_000));
        assert!(core.record(ORCH, b, 20_000));
        // Inside the window: wait for its end, counted from the first event.
        assert_eq!(
            core.decide(ORCH, 30_000, IDLE, false),
            Decision::WaitUntil {
                at_ms: 1_000 + WAKE_WINDOW_MS,
                reason: WakeHoldReason::Window
            }
        );
        let Decision::Deliver(batch) = core.decide(ORCH, 1_000 + WAKE_WINDOW_MS, IDLE, false)
        else {
            panic!("expected a delivery");
        };
        assert_eq!(batch.events.len(), 2);
        let text = batch.render(|id| id.to_owned());
        assert!(text.contains("Worker updates:"));
        assert!(text.contains("w1 — act:a") && text.contains("w2 — err:w2"));
        core.delivered(ORCH, &batch, 50_000);
        assert_eq!(core.decide(ORCH, 60_000, IDLE, false), Decision::Nothing);
    }

    #[test]
    fn report_uses_current_title_after_recording() {
        let mut core = WakeCore::new();
        let mut report = event(&mut core, "worker", WakeEventKind::WorkerReport, "report", 1_000);
        report.summary = "GPT-6 (chat worker) — worker report: finished".into();
        report.report = Some("finished".into());
        assert!(core.record(ORCH, report, 1_000));
        let Decision::Deliver(batch) = core.decide(ORCH, 50_000, IDLE, false) else {
            panic!("expected a delivery");
        };
        let text = batch.render(|_| "Current title (chat worker)".into());
        assert!(text.contains("Current title (chat worker) — worker report: finished"));
        assert!(!text.contains("GPT-6"));
    }

    #[test]
    fn at_most_one_wake_per_window() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        core.record(ORCH, a, 0);
        let Decision::Deliver(batch) = core.decide(ORCH, WAKE_WINDOW_MS, IDLE, false) else {
            panic!()
        };
        core.delivered(ORCH, &batch, WAKE_WINDOW_MS);
        // A new event right after: its own window opens, and the previous
        // delivery's window also has to pass.
        let b = raised(&mut core, "w2", "b", WAKE_WINDOW_MS + 1_000);
        core.record(ORCH, b, WAKE_WINDOW_MS + 1_000);
        assert_eq!(
            core.decide(ORCH, WAKE_WINDOW_MS + 2_000, IDLE, false),
            Decision::WaitUntil {
                at_ms: 2 * WAKE_WINDOW_MS + 1_000,
                reason: WakeHoldReason::Window
            }
        );
    }

    #[test]
    fn never_delivers_mid_turn_or_ahead_of_a_queued_user_message() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        core.record(ORCH, a, 0);
        let late = 10 * WAKE_WINDOW_MS;
        let busy = Ctx { busy: true, ..IDLE };
        assert_eq!(
            core.decide(ORCH, late, busy, false),
            Decision::Hold(WakeHoldReason::Busy)
        );
        // Even "Deliver now" never interrupts a turn.
        assert_eq!(
            core.decide(ORCH, late, busy, true),
            Decision::Hold(WakeHoldReason::Busy)
        );
        let queued = Ctx {
            queue_nonempty: true,
            ..IDLE
        };
        assert_eq!(
            core.decide(ORCH, late, queued, false),
            Decision::Hold(WakeHoldReason::Queued)
        );
        assert_eq!(
            core.decide(ORCH, late, queued, true),
            Decision::Hold(WakeHoldReason::Queued)
        );
        assert!(matches!(
            core.decide(ORCH, late, IDLE, false),
            Decision::Deliver(_)
        ));
    }

    #[test]
    fn holds_while_the_user_has_a_draft_and_for_two_minutes_after_activity() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        core.record(ORCH, a, 0);
        let t = 5 * WAKE_WINDOW_MS;
        core.note_user_activity(ORCH, t, Some(true));
        // A draft holds indefinitely, however old the activity.
        assert_eq!(
            core.decide(ORCH, t + 10 * USER_QUIET_MS, IDLE, false),
            Decision::Hold(WakeHoldReason::Draft)
        );
        core.note_user_activity(ORCH, t + 1_000, Some(false));
        // Draft cleared, but the user was just here.
        assert_eq!(
            core.decide(ORCH, t + 2_000, IDLE, false),
            Decision::WaitUntil {
                at_ms: t + 1_000 + USER_QUIET_MS,
                reason: WakeHoldReason::RecentActivity
            }
        );
        assert!(matches!(
            core.decide(ORCH, t + 1_000 + USER_QUIET_MS, IDLE, false),
            Decision::Deliver(_)
        ));
        // "Deliver now" skips the draft and activity holds.
        core.note_user_activity(ORCH, t + 200_000, Some(true));
        assert!(matches!(
            core.decide(ORCH, t + 200_001, IDLE, true),
            Decision::Deliver(_)
        ));
    }

    #[test]
    fn duplicates_are_dropped_held_or_already_delivered() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        assert!(core.record(ORCH, a.clone(), 0));
        let again = raised(&mut core, "w1", "a", 5);
        assert!(!core.record(ORCH, again, 5), "held duplicate");
        let Decision::Deliver(batch) = core.decide(ORCH, WAKE_WINDOW_MS, IDLE, false) else {
            panic!()
        };
        core.delivered(ORCH, &batch, WAKE_WINDOW_MS);
        let reraised = raised(&mut core, "w1", "a", WAKE_WINDOW_MS + 1);
        assert!(
            !core.record(ORCH, reraised, WAKE_WINDOW_MS + 1),
            "delivered duplicate"
        );
        let later = raised(&mut core, "w1", "a", DEDUPE_TTL_MS + WAKE_WINDOW_MS + 5);
        assert!(
            core.record(ORCH, later, DEDUPE_TTL_MS + WAKE_WINDOW_MS + 5),
            "after the dedupe TTL it is news again"
        );
    }

    #[test]
    fn raised_then_resolved_before_delivery_cancels_out() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        core.record(ORCH, a, 0);
        let resolved = event(&mut core, "w1", WakeEventKind::ActionResolved, "res:a", 10);
        assert!(!core.record(ORCH, resolved, 10));
        assert!(!core.has_held(ORCH));
        assert_eq!(
            core.decide(ORCH, 10 * WAKE_WINDOW_MS, IDLE, false),
            Decision::Nothing
        );
    }

    #[test]
    fn settings_gate_which_kinds_are_held_but_all_are_logged() {
        let mut core = WakeCore::new();
        let pr = event(&mut core, "w1", WakeEventKind::PrOpened, "pr:w1:0", 0);
        assert!(
            !core.record(ORCH, pr, 0),
            "prs is off by default"
        );
        assert!(!core.has_held(ORCH));
        let status = core.status(ORCH, 1, IDLE);
        assert_eq!(status.recent.len(), 1, "still shown for the unseen marks");
        // Turn it on: the next one is held.
        core.set_settings(
            ORCH,
            WakeSettings {
                prs: true,
                ..WakeSettings::default()
            },
        );
        let pr = event(&mut core, "w1", WakeEventKind::PrOpened, "pr:w1:1", 2);
        assert!(core.record(ORCH, pr, 2));
        // Master switch off drops what is held.
        core.set_settings(
            ORCH,
            WakeSettings {
                enabled: false,
                ..WakeSettings::default()
            },
        );
        assert!(!core.has_held(ORCH));
    }

    #[test]
    fn events_after_the_orchestrators_last_turn_are_unseen() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 100);
        core.record(ORCH, a, 100);
        core.note_turn_start(ORCH, 50);
        let status = core.status(ORCH, 200, IDLE);
        assert_eq!(status.unseen_count, 1);
        assert!(status.recent[0].unseen);
        core.note_turn_start(ORCH, 150);
        let status = core.status(ORCH, 200, IDLE);
        assert_eq!(status.unseen_count, 0);
        assert!(!status.recent[0].unseen);
    }

    #[test]
    fn a_cancelled_delivery_returns_its_events_to_the_batch() {
        let mut core = WakeCore::new();
        let a = raised(&mut core, "w1", "a", 0);
        core.record(ORCH, a, 0);
        let Decision::Deliver(batch) = core.decide(ORCH, WAKE_WINDOW_MS, IDLE, false) else {
            panic!()
        };
        core.delivered(ORCH, &batch, WAKE_WINDOW_MS);
        core.restore(ORCH, batch.events, WAKE_WINDOW_MS + 10);
        assert!(core.has_held(ORCH));
        assert!(!core.status(ORCH, WAKE_WINDOW_MS + 10, IDLE).recent[0].delivered);
    }
}
