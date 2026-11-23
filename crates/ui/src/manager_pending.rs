//! Optimistic echo of a project Manager's settings (read-only, worker model,
//! fallback chain) while their `WorkOutlineMutate` round trip and the watch
//! frame that carries the result are on their way.
//!
//! A click records a pending value per (item, setting) and the Settings tab
//! draws it at once. The value leaves the overlay when
//! - the mutation failed (rollback; the caller shows the error),
//! - the snapshot's role already holds it, or
//! - the host acknowledged it and [`ACK_GRACE`] passed without the snapshot
//!   showing it, so a later edit from another device wins instead of the
//!   overlay masking it forever.
//!
//! Between the acknowledgement and the frame the overlay keeps drawing the
//! value, so the control never flickers back to the old one. The shell owns
//! one [`ManagerPending`]; nothing here touches the network or the clock
//! (callers pass `now`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mast_proto::{StandingRole, StandingRoleModel};

/// How long an acknowledged value may stay unconfirmed by the snapshot.
pub(crate) const ACK_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
struct Slot<T> {
    seq: u64,
    value: T,
    acked_at: Option<Instant>,
}

impl<T: PartialEq + Clone> Slot<T> {
    fn new(seq: u64, value: T) -> Self {
        Self { seq, value, acked_at: None }
    }

    /// Whether the slot is finished: confirmed by the snapshot, or acknowledged
    /// and past the grace.
    fn settled(&self, snapshot_value: &T, now: Instant) -> bool {
        &self.value == snapshot_value
            || self.acked_at.is_some_and(|acked| now.saturating_duration_since(acked) >= ACK_GRACE)
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

/// A pending edit: its setting and sequence number, handed back to
/// [`ManagerPending::ack`] / [`ManagerPending::fail`] so an outcome of an
/// older edit never touches a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ticket {
    setting: Setting,
    seq: u64,
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

    pub(crate) fn start_read_only(&mut self, item_id: &str, value: bool) -> Ticket {
        let seq = self.next_seq();
        self.items.entry(item_id.to_owned()).or_default().read_only = Some(Slot::new(seq, value));
        Ticket { setting: Setting::ReadOnly, seq }
    }

    pub(crate) fn start_worker_model(&mut self, item_id: &str, value: Option<StandingRoleModel>) -> Ticket {
        let seq = self.next_seq();
        self.items.entry(item_id.to_owned()).or_default().worker_model = Some(Slot::new(seq, value));
        Ticket { setting: Setting::WorkerModel, seq }
    }

    pub(crate) fn start_fallback_chain(&mut self, item_id: &str, value: Vec<StandingRoleModel>) -> Ticket {
        let seq = self.next_seq();
        self.items.entry(item_id.to_owned()).or_default().fallback_chain = Some(Slot::new(seq, value));
        Ticket { setting: Setting::FallbackChain, seq }
    }

    /// The host acknowledged the edit: the value stays drawn until the
    /// snapshot shows it (or the grace passes).
    pub(crate) fn ack(&mut self, item_id: &str, ticket: Ticket, now: Instant) {
        let Some(item) = self.items.get_mut(item_id) else { return };
        match ticket.setting {
            Setting::ReadOnly => ack_slot(&mut item.read_only, ticket.seq, now),
            Setting::WorkerModel => ack_slot(&mut item.worker_model, ticket.seq, now),
            Setting::FallbackChain => ack_slot(&mut item.fallback_chain, ticket.seq, now),
        }
    }

    /// The edit failed for good: draw the stored value again. A newer edit of
    /// the same setting keeps its overlay.
    pub(crate) fn fail(&mut self, item_id: &str, ticket: Ticket) {
        let Some(item) = self.items.get_mut(item_id) else { return };
        match ticket.setting {
            Setting::ReadOnly => fail_slot(&mut item.read_only, ticket.seq),
            Setting::WorkerModel => fail_slot(&mut item.worker_model, ticket.seq),
            Setting::FallbackChain => fail_slot(&mut item.fallback_chain, ticket.seq),
        }
        if item.is_empty() {
            self.items.remove(item_id);
        }
    }

    /// Drop every overlay the snapshot has caught up with (or that was
    /// acknowledged and timed out). Returns when the next acknowledged
    /// overlay times out, so the caller can schedule one more repaint.
    pub(crate) fn settle(&mut self, item_id: &str, role: Option<&StandingRole>, now: Instant) -> Option<Duration> {
        let stored = stored_settings(role);
        let item = self.items.get_mut(item_id)?;
        if item.read_only.as_ref().is_some_and(|slot| slot.settled(&stored.read_only, now)) {
            item.read_only = None;
        }
        if item.worker_model.as_ref().is_some_and(|slot| slot.settled(&stored.worker_model, now)) {
            item.worker_model = None;
        }
        if item.fallback_chain.as_ref().is_some_and(|slot| slot.settled(&stored.fallback_chain, now)) {
            item.fallback_chain = None;
        }
        let waits = [
            item.read_only.as_ref().and_then(|slot| slot.acked_at),
            item.worker_model.as_ref().and_then(|slot| slot.acked_at),
            item.fallback_chain.as_ref().and_then(|slot| slot.acked_at),
        ]
        .into_iter()
        .flatten()
        .map(|acked| ACK_GRACE.saturating_sub(now.saturating_duration_since(acked)))
        .min();
        if item.is_empty() {
            self.items.remove(item_id);
        }
        waits
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

    /// Whether any edit of `item_id` has not settled.
    #[cfg(test)]
    pub(crate) fn has_pending(&self, item_id: &str) -> bool {
        self.items.contains_key(item_id)
    }
}

fn ack_slot<T>(slot: &mut Option<Slot<T>>, seq: u64, now: Instant) {
    if let Some(slot) = slot.as_mut().filter(|slot| slot.seq == seq) {
        slot.acked_at = Some(now);
    }
}

fn fail_slot<T>(slot: &mut Option<Slot<T>>, seq: u64) {
    if slot.as_ref().is_some_and(|slot| slot.seq == seq) {
        *slot = None;
    }
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

    #[test]
    fn a_click_shows_at_once_and_holds_through_the_reply_until_the_frame() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        assert!(pending.effective("p", None).read_only, "an unchosen project reads on");
        let ticket = pending.start_read_only("p", false);
        assert!(!pending.effective("p", None).read_only, "drawn before any reply");
        pending.ack("p", ticket, now);
        assert_eq!(pending.settle("p", None, now), Some(ACK_GRACE));
        assert!(!pending.effective("p", None).read_only, "no flicker back between the reply and the frame");
        let mut stored = role();
        stored.read_only = Some(false);
        assert_eq!(pending.settle("p", Some(&stored), now), None, "the frame carries it: nothing left to wait for");
        assert!(!pending.has_pending("p"));
        assert!(!pending.effective("p", Some(&stored)).read_only);
    }

    #[test]
    fn a_failed_edit_rolls_back_and_a_newer_edit_keeps_its_overlay() {
        let mut pending = ManagerPending::default();
        let first = pending.start_read_only("p", false);
        let second = pending.start_read_only("p", true);
        pending.fail("p", first);
        assert!(pending.has_pending("p"), "the older failure leaves the newer edit alone");
        pending.fail("p", second);
        assert!(!pending.has_pending("p"));
        assert!(pending.effective("p", None).read_only, "rolled back to the stored value");
    }

    #[test]
    fn an_acknowledged_value_the_snapshot_never_shows_yields_to_the_stored_one() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let ticket = pending.start_read_only("p", false);
        pending.ack("p", ticket, now);
        // Another device wrote on top: the snapshot says read-only again.
        let mut stored = role();
        stored.read_only = Some(true);
        assert!(!pending.effective("p", Some(&stored)).read_only, "still inside the grace");
        pending.settle("p", Some(&stored), now + ACK_GRACE);
        assert!(pending.effective("p", Some(&stored)).read_only, "the last write wins after the grace");
    }

    #[test]
    fn an_unacknowledged_edit_is_never_dropped_by_the_clock() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        pending.start_worker_model("p", Some(model("cursor", Some("claude-sonnet-5-5"))));
        pending.settle("p", None, now + ACK_GRACE * 10);
        assert_eq!(pending.effective("p", None).worker_model, Some(model("cursor", Some("claude-sonnet-5-5"))),
            "a lost reply keeps the overlay while the retry runs");
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
    }

    #[test]
    fn the_default_worker_model_is_a_value_too() {
        let now = Instant::now();
        let mut pending = ManagerPending::default();
        let mut stored = role();
        stored.worker_model = Some(model("cursor", None));
        let ticket = pending.start_worker_model("p", None);
        assert_eq!(pending.effective("p", Some(&stored)).worker_model, None, "Default clears the stored model at once");
        pending.ack("p", ticket, now);
        stored.worker_model = None;
        pending.settle("p", Some(&stored), now);
        assert!(!pending.has_pending("p"));
    }
}
