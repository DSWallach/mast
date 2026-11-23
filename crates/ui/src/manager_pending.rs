//! Optimistic echo of a project Manager's settings (read-only, worker model,
//! fallback chain) while their `WorkOutlineMutate` round trip and the watch
//! frame that carries the result are on their way.
//!
//! A click records a pending value per (item, setting) and the Settings tab
//! draws it at once. The value leaves the overlay when
//! - the host refused it (rollback; the caller shows the error), or
//! - the host acknowledged it (or the transport failed, so it may have been
//!   applied) and either the snapshot's role holds it, or a newer watch frame
//!   than the one current at the acknowledgement arrived [`ACK_GRACE`] after
//!   it without holding it (another device wrote on top), or [`HARD_CAP`]
//!   passed with no frame at all.
//!
//! An unacknowledged edit never settles: a stale frame that happens to hold
//! the value, or the clock, must not end it before the host answered.
//!
//! One mutation per (item, setting) is in flight at a time. A click made
//! meanwhile only updates the value drawn; [`ManagerPending::finish`] hands
//! the latest value back to send once the earlier mutation ends, so the
//! host's last write is the user's last click even when an earlier one was
//! retrying.
//!
//! The shell owns one [`ManagerPending`]; nothing here touches the network or
//! the clock (callers pass `now` and the snapshot revision).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mast_proto::{StandingRole, StandingRoleModel};

/// How long after the acknowledgement a newer frame that lacks the value
/// counts as "someone else wrote on top".
pub(crate) const ACK_GRACE: Duration = Duration::from_secs(5);

/// How long an acknowledged value may stay drawn with no frame at all (a dead
/// watch stream must not mask the stored value forever).
pub(crate) const HARD_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
struct Slot<T> {
    /// Of the newest edit; the value drawn is that edit's.
    seq: u64,
    value: T,
    /// A mutation for an edit of this slot is on the wire.
    in_flight: bool,
    /// An edit newer than the in-flight one waits to be sent.
    queued: bool,
    acked_at: Option<Instant>,
    /// The snapshot revision first seen after the acknowledgement.
    acked_revision: Option<u64>,
}

impl<T: PartialEq + Clone> Slot<T> {
    fn started(prev: Option<Slot<T>>, seq: u64, value: T) -> (Self, bool) {
        let busy = prev.is_some_and(|prev| prev.in_flight);
        (Self { seq, value, in_flight: true, queued: busy, acked_at: None, acked_revision: None }, !busy)
    }

    /// Whether the slot is finished against what the snapshot stores.
    fn settled(&mut self, stored: &T, revision: u64, now: Instant) -> bool {
        let Some(acked) = self.acked_at else { return false };
        if self.in_flight || self.queued {
            return false;
        }
        if &self.value == stored {
            return true;
        }
        let baseline = *self.acked_revision.get_or_insert(revision);
        let waited = now.saturating_duration_since(acked);
        (revision != baseline && waited >= ACK_GRACE) || waited >= HARD_CAP
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ItemPending {
    read_only: Option<Slot<bool>>,
    worker_model: Option<Slot<Option<StandingRoleModel>>>,
    fallback_chain: Option<Slot<Vec<StandingRoleModel>>>,
}

impl ItemPending {
    fn is_empty(&self) -> bool {
        self.read_only.is_none() && self.worker_model.is_none() && self.fallback_chain.is_none()
    }
}

/// The three settings as the Settings tab should draw them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ManagerSettings {
    pub read_only: bool,
    pub worker_model: Option<StandingRoleModel>,
    pub fallback_chain: Vec<StandingRoleModel>,
}

/// Which setting a started edit or its outcome belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Setting {
    ReadOnly,
    WorkerModel,
    FallbackChain,
}

/// One edit's setting and sequence number, handed back to
/// [`ManagerPending::finish`] so the outcome of an older edit never touches a
/// newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ticket {
    setting: Setting,
    seq: u64,
}

/// A value to send: the setting's new value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingValue {
    ReadOnly(bool),
    WorkerModel(Option<StandingRoleModel>),
    FallbackChain(Vec<StandingRoleModel>),
}

/// An edit that is ready to go on the wire.
pub(crate) type Dispatch = (Ticket, PendingValue);

/// How the host answered one mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Applied (or a transport failure left it unknown): keep drawing the
    /// value until the snapshot shows it.
    Acked(Instant),
    /// Refused for good: draw the stored value again.
    Refused,
}

#[derive(Debug, Default)]
pub(crate) struct ManagerPending {
    items: HashMap<String, ItemPending>,
    next_seq: u64,
}

impl ManagerPending {
    fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Record a click. `Some` when the mutation can go now; `None` when one is
    /// already in flight for this setting (the latest value follows from
    /// [`Self::finish`]).
    pub(crate) fn start_read_only(&mut self, item_id: &str, value: bool) -> Option<Dispatch> {
        let seq = self.next_seq();
        let item = self.items.entry(item_id.to_owned()).or_default();
        let (slot, send) = Slot::started(item.read_only.take(), seq, value);
        item.read_only = Some(slot);
        send.then(|| (Ticket { setting: Setting::ReadOnly, seq }, PendingValue::ReadOnly(value)))
    }

    pub(crate) fn start_worker_model(&mut self, item_id: &str, value: Option<StandingRoleModel>) -> Option<Dispatch> {
        let seq = self.next_seq();
        let item = self.items.entry(item_id.to_owned()).or_default();
        let (slot, send) = Slot::started(item.worker_model.take(), seq, value.clone());
        item.worker_model = Some(slot);
        send.then(|| (Ticket { setting: Setting::WorkerModel, seq }, PendingValue::WorkerModel(value)))
    }

    pub(crate) fn start_fallback_chain(&mut self, item_id: &str, value: Vec<StandingRoleModel>) -> Option<Dispatch> {
        let seq = self.next_seq();
        let item = self.items.entry(item_id.to_owned()).or_default();
        let (slot, send) = Slot::started(item.fallback_chain.take(), seq, value.clone());
        item.fallback_chain = Some(slot);
        send.then(|| (Ticket { setting: Setting::FallbackChain, seq }, PendingValue::FallbackChain(value)))
    }

    /// One mutation ended. Returns the queued newer edit of the same setting,
    /// if a click arrived while this one was in flight.
    pub(crate) fn finish(&mut self, item_id: &str, ticket: Ticket, outcome: Outcome) -> Option<Dispatch> {
        let item = self.items.get_mut(item_id)?;
        let next = match ticket.setting {
            Setting::ReadOnly => {
                finish_slot(&mut item.read_only, ticket, outcome, PendingValue::ReadOnly)
            }
            Setting::WorkerModel => {
                finish_slot(&mut item.worker_model, ticket, outcome, PendingValue::WorkerModel)
            }
            Setting::FallbackChain => {
                finish_slot(&mut item.fallback_chain, ticket, outcome, PendingValue::FallbackChain)
            }
        };
        if item.is_empty() {
            self.items.remove(item_id);
        }
        next
    }

    /// Drop every overlay the snapshot has caught up with, or that outlived
    /// its grace. `revision` is the Space snapshot's current frame revision.
    pub(crate) fn settle(&mut self, item_id: &str, role: Option<&StandingRole>, revision: u64, now: Instant) {
        let stored = stored_settings(role);
        let Some(item) = self.items.get_mut(item_id) else { return };
        if item.read_only.as_mut().is_some_and(|slot| slot.settled(&stored.read_only, revision, now)) {
            item.read_only = None;
        }
        if item.worker_model.as_mut().is_some_and(|slot| slot.settled(&stored.worker_model, revision, now)) {
            item.worker_model = None;
        }
        if item.fallback_chain.as_mut().is_some_and(|slot| slot.settled(&stored.fallback_chain, revision, now)) {
            item.fallback_chain = None;
        }
        if item.is_empty() {
            self.items.remove(item_id);
        }
    }

    /// The settings to draw: the stored ones with every pending edit on top.
    pub(crate) fn effective(&self, item_id: &str, role: Option<&StandingRole>) -> ManagerSettings {
        let mut settings = stored_settings(role);
        if let Some(item) = self.items.get(item_id) {
            if let Some(slot) = &item.read_only {
                settings.read_only = slot.value;
            }
            if let Some(slot) = &item.worker_model {
                settings.worker_model = slot.value.clone();
            }
            if let Some(slot) = &item.fallback_chain {
                settings.fallback_chain = slot.value.clone();
            }
        }
        settings
    }

    /// The pending read-only value, so a click toggles what is drawn now.
    pub(crate) fn pending_read_only(&self, item_id: &str) -> Option<bool> {
        self.items.get(item_id)?.read_only.as_ref().map(|slot| slot.value)
    }

    /// The pending chain, so an edit builds on what is drawn now.
    pub(crate) fn pending_fallback_chain(&self, item_id: &str) -> Option<Vec<StandingRoleModel>> {
        self.items.get(item_id)?.fallback_chain.as_ref().map(|slot| slot.value.clone())
    }

    /// Whether any edit of `item_id` has not settled.
    #[cfg(test)]
    pub(crate) fn has_pending(&self, item_id: &str) -> bool {
        self.items.contains_key(item_id)
    }
}

fn finish_slot<T: PartialEq + Clone>(
    slot: &mut Option<Slot<T>>, ticket: Ticket, outcome: Outcome, wrap: fn(T) -> PendingValue,
) -> Option<Dispatch> {
    let current = slot.as_mut()?;
    if current.seq != ticket.seq {
        // An older edit ended while a newer click waits (or was refused): the
        // newer one goes next. Its overlay stays either way.
        current.in_flight = false;
        if !current.queued {
            return None;
        }
        current.queued = false;
        current.in_flight = true;
        return Some((Ticket { seq: current.seq, ..ticket }, wrap(current.value.clone())));
    }
    current.in_flight = false;
    match outcome {
        Outcome::Acked(now) => current.acked_at = Some(now),
        Outcome::Refused => *slot = None,
    }
    None
}

/// What the role record stores, with the defaults of a project that never
/// chose.
fn stored_settings(role: Option<&StandingRole>) -> ManagerSettings {
    ManagerSettings {
        read_only: StandingRole::manager_runs_read_only(role),
        worker_model: role.and_then(|role| role.worker_model.clone()),
        fallback_chain: role.map(|role| role.fallback_chain.clone()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{StandingRoleKind, StandingRoleState};

    fn role() -> StandingRole {
        StandingRole {
            id: "manager:p".into(),
            role: StandingRoleKind::Manager,
            scope_item_id: "p".into(),
            current_chat_id: Some("c".into()),
            chain: vec!["c".into()],
            fallback_chain: Vec::new(),
            state: StandingRoleState::Idle,
            worker_model: None,
            read_only: None,
        }
    }

    fn model(harness: &str, id: Option<&str>) -> StandingRoleModel {
        StandingRoleModel { harness: harness.into(), model: id.map(str::to_owned) }
    }

    fn ack(pending: &mut ManagerPending, ticket: Ticket, now: Instant) -> Option<Dispatch> {
        pending.finish("p", ticket, Outcome::Acked(now))
    }

    #[test]
    fn a_click_shows_at_once_and_holds_through_the_reply_until_the_frame() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        assert!(pending.effective("p", None).read_only, "an unchosen project reads on");
        let (ticket, value) = pending.start_read_only("p", false).expect("nothing in flight: send now");
        assert_eq!(value, PendingValue::ReadOnly(false));
        assert!(!pending.effective("p", None).read_only, "drawn before any reply");
        assert!(ack(&mut pending, ticket, now).is_none());
        pending.settle("p", None, 3, now);
        assert!(!pending.effective("p", None).read_only, "no flicker back between the reply and the frame");
        let mut stored = role();
        stored.read_only = Some(false);
        pending.settle("p", Some(&stored), 4, now);
        assert!(!pending.has_pending("p"), "the frame carries it: nothing left to wait for");
        assert!(!pending.effective("p", Some(&stored)).read_only);
    }

    #[test]
    fn a_refused_edit_rolls_back() {
        let mut pending = ManagerPending::default();
        let (ticket, _) = pending.start_read_only("p", false).unwrap();
        pending.finish("p", ticket, Outcome::Refused);
        assert!(!pending.has_pending("p"));
        assert!(pending.effective("p", None).read_only, "rolled back to the stored value");
    }

    #[test]
    fn a_stale_frame_holding_the_value_does_not_settle_an_unacknowledged_edit() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let mut stored = role();
        stored.read_only = Some(true);
        // On -> Off -> On quickly: the second click makes the pending value
        // equal the stored one while neither write has been answered.
        let (first, _) = pending.start_read_only("p", false).unwrap();
        assert!(pending.start_read_only("p", true).is_none(), "the first is still in flight");
        pending.settle("p", Some(&stored), 2, now);
        assert!(pending.has_pending("p"), "a frame that happens to agree is not an answer");
        // The first write ends: the latest click goes out next.
        let (second, value) = ack(&mut pending, first, now).expect("the queued click is sent");
        assert_eq!(value, PendingValue::ReadOnly(true));
        pending.settle("p", Some(&stored), 2, now);
        assert!(pending.has_pending("p"), "still waiting for the second answer");
        assert!(ack(&mut pending, second, now).is_none());
        pending.settle("p", Some(&stored), 3, now);
        assert!(!pending.has_pending("p"), "answered, and the frame holds it");
    }

    #[test]
    fn two_clicks_go_out_one_at_a_time_and_the_last_value_is_sent_last() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let (first, _) = pending.start_fallback_chain("p", vec![model("codex", None)]).unwrap();
        assert!(pending.start_fallback_chain("p", vec![model("codex", None), model("cursor", None)]).is_none());
        assert!(pending.start_fallback_chain("p", vec![model("cursor", None)]).is_none(), "a third replaces the queued one");
        assert_eq!(pending.effective("p", None).fallback_chain, vec![model("cursor", None)], "the newest is drawn");
        let (second, value) = ack(&mut pending, first, now).expect("one follow-up");
        assert_eq!(value, PendingValue::FallbackChain(vec![model("cursor", None)]));
        assert!(ack(&mut pending, second, now).is_none());
    }

    #[test]
    fn a_refused_older_edit_still_sends_the_newer_click_and_keeps_its_overlay() {
        let mut pending = ManagerPending::default();
        let (first, _) = pending.start_read_only("p", false).unwrap();
        assert!(pending.start_read_only("p", true).is_none());
        let sent = pending.finish("p", first, Outcome::Refused);
        assert!(matches!(sent, Some((_, PendingValue::ReadOnly(true)))), "the queued click is not lost");
        assert!(pending.has_pending("p"));
    }

    #[test]
    fn an_acknowledged_value_yields_after_a_newer_frame_that_lacks_it() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let (ticket, _) = pending.start_read_only("p", false).unwrap();
        ack(&mut pending, ticket, now);
        // Another device wrote on top: every frame says read-only again.
        let mut stored = role();
        stored.read_only = Some(true);
        pending.settle("p", Some(&stored), 7, now);
        assert!(!pending.effective("p", Some(&stored)).read_only, "inside the grace");
        pending.settle("p", Some(&stored), 8, now + ACK_GRACE);
        assert!(pending.effective("p", Some(&stored)).read_only, "a newer frame after the grace: the last write wins");
    }

    #[test]
    fn a_slow_stream_keeps_the_acknowledged_value_until_the_hard_cap() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let (ticket, _) = pending.start_read_only("p", false).unwrap();
        ack(&mut pending, ticket, now);
        let mut stored = role();
        stored.read_only = Some(true);
        pending.settle("p", Some(&stored), 7, now);
        // No frame since the acknowledgement: the grace alone does not end it.
        pending.settle("p", Some(&stored), 7, now + ACK_GRACE * 3);
        assert!(!pending.effective("p", Some(&stored)).read_only, "a reconnecting stream must not snap it back");
        pending.settle("p", Some(&stored), 7, now + HARD_CAP);
        assert!(pending.effective("p", Some(&stored)).read_only, "but a dead stream cannot hide the stored value for ever");
    }

    #[test]
    fn a_lost_reply_keeps_the_overlay_while_the_edit_is_in_flight() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        pending.start_worker_model("p", Some(model("cursor", Some("claude-sonnet-5-5")))).unwrap();
        pending.settle("p", None, 5, now + HARD_CAP * 10);
        assert_eq!(pending.effective("p", None).worker_model, Some(model("cursor", Some("claude-sonnet-5-5"))),
            "no answer yet: the clock never drops it");
    }

    #[test]
    fn an_edit_acknowledged_after_a_transport_failure_settles_like_any_other() {
        // The caller reports an uncertain transport failure as `Acked`: the
        // value stays drawn, and the snapshot decides.
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let (ticket, _) = pending.start_read_only("p", false).unwrap();
        ack(&mut pending, ticket, now);
        let mut stored = role();
        stored.read_only = Some(false);
        pending.settle("p", Some(&stored), 2, now);
        assert!(!pending.has_pending("p"), "the host had applied it after all");
    }

    #[test]
    fn settings_overlay_independently_per_item_and_setting() {
        let mut pending = ManagerPending::default();
        pending.start_fallback_chain("p", vec![model("codex", None)]);
        let mut stored = role();
        stored.read_only = Some(false);
        let seen = pending.effective("p", Some(&stored));
        assert!(!seen.read_only, "an unedited setting reads the snapshot");
        assert_eq!(seen.fallback_chain, vec![model("codex", None)]);
        assert!(pending.effective("q", None).fallback_chain.is_empty(), "another project is unaffected");
        assert_eq!(pending.pending_fallback_chain("p"), Some(vec![model("codex", None)]));
        assert_eq!(pending.pending_read_only("p"), None);
    }

    #[test]
    fn the_default_worker_model_is_a_value_too() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let mut stored = role();
        stored.worker_model = Some(model("cursor", None));
        let (ticket, value) = pending.start_worker_model("p", None).unwrap();
        assert_eq!(value, PendingValue::WorkerModel(None));
        assert_eq!(pending.effective("p", Some(&stored)).worker_model, None, "Default clears the stored model at once");
        ack(&mut pending, ticket, now);
        stored.worker_model = None;
        pending.settle("p", Some(&stored), 2, now);
        assert!(!pending.has_pending("p"));
    }
}
