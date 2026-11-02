//! Pure admin logic plus the `AppState` RPC calls against a scripted wire.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::AppContext as _;
use mast_proto::{AdminAuditAction, AdminAuditTurn, AdminGrant, AdminUndoTurnReply};
use mast_rpc::{RpcClient, methods};

use super::*;
use crate::state::EngineHandle;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

pub(crate) fn grant(chat: &str, expires: Option<i64>) -> AdminGrant {
    AdminGrant {
        chat_id: chat.into(),
        space_id: "s1".into(),
        granted_at_ms: 0,
        expires_at_ms: expires,
        skip_bulk_confirmation: false,
    }
}

pub(crate) fn turn(id: &str, at_ms: i64, actions: &[&str]) -> AdminAuditTurn {
    AdminAuditTurn {
        turn_id: id.into(),
        chat_id: "c1".into(),
        chat_title: "Tidy".into(),
        space_id: "s1".into(),
        started_at_ms: at_ms - 10,
        updated_at_ms: at_ms,
        label: "by admin chat Tidy".into(),
        actions: actions
            .iter()
            .enumerate()
            .map(|(i, summary)| AdminAuditAction {
                at_ms: at_ms - 10 + i as i64,
                kind: "rename".into(),
                summary: (*summary).into(),
            })
            .collect(),
        undone: false,
    }
}

#[test]
fn time_left_reads_hours_minutes_and_the_last_minute() {
    let at = |left: i64| time_left(Some(1_000 + left), 1_000);
    assert_eq!(at(4 * HOUR).as_deref(), Some("4h left"));
    assert_eq!(at(3 * HOUR + 59 * MIN).as_deref(), Some("3h left"));
    assert_eq!(at(HOUR).as_deref(), Some("1h left"));
    assert_eq!(at(HOUR - 1).as_deref(), Some("59m left"));
    assert_eq!(at(2 * MIN + 1).as_deref(), Some("3m left"));
    assert_eq!(at(MIN).as_deref(), Some("1m left"));
    assert_eq!(at(MIN - 1).as_deref(), Some("<1m left"));
    assert_eq!(at(1).as_deref(), Some("<1m left"));
    assert_eq!(at(0), None);
    assert_eq!(at(-5), None);
    assert_eq!(time_left(None, 1_000).as_deref(), Some("until revoked"));
}

#[test]
fn badge_text_names_the_space_and_vanishes_at_expiry() {
    let g = grant("c1", Some(3 * HOUR + 10));
    assert_eq!(
        badge_text("Mast", &g, 0).as_deref(),
        Some("Admin \u{b7} Mast \u{b7} 4h left")
    );
    assert_eq!(
        badge_text("Mast", &g, 10 + 30 * MIN).as_deref(),
        Some("Admin \u{b7} Mast \u{b7} 3h left")
    );
    assert_eq!(badge_text("Mast", &g, 3 * HOUR + 10), None);
    assert_eq!(
        badge_text("Mast", &grant("c1", None), 5).as_deref(),
        Some("Admin \u{b7} Mast \u{b7} until revoked")
    );
}

#[test]
fn every_duration_choice_builds_its_grant_params() {
    let secs: Vec<Option<u64>> = DurationChoice::ALL
        .iter()
        .map(|c| grant_params("c1", *c, false).duration_secs)
        .collect();
    assert_eq!(
        secs,
        [Some(3_600), Some(14_400), Some(28_800), Some(86_400), None]
    );
    assert_eq!(DurationChoice::default(), DurationChoice::FourHours);
    assert_eq!(
        DurationChoice::default().secs(),
        Some(mast_proto::ADMIN_DEFAULT_DURATION_SECS)
    );
    let params = grant_params("chat-9", DurationChoice::UntilRevoked, false);
    assert_eq!(params.chat_id, "chat-9");
    // "Until revoked" is sent as an absent / null duration.
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        serde_json::json!({
            "chatId": "chat-9", "durationSecs": null, "skipBulkConfirmation": false
        })
    );
    assert!(grant_params("c", DurationChoice::OneHour, true).skip_bulk_confirmation);
    let text = explanation("Mast");
    assert!(text.contains("anything you can do in the app inside Mast"), "{text}");
    assert!(text.contains("except Linear or GitHub writes and irreversible deletes"));
    assert!(text.contains("attributed to the chat and undoable"));
    assert_eq!(badge_tooltip(&grant("c", None)), None);
    let mut skipping = grant("c", None);
    skipping.skip_bulk_confirmation = true;
    assert!(badge_tooltip(&skipping).is_some());
    assert_eq!(grant_entry_label(false), "Grant admin for this Space\u{2026}");
    assert_eq!(grant_entry_label(true), "Renew admin\u{2026}");
}

#[test]
fn the_slice_only_reports_real_changes() {
    let mut admin = AdminState::default();
    // The first answer counts even when empty (it marks "loaded").
    assert!(admin.set_grants(vec![]));
    assert!(!admin.set_grants(vec![]));
    assert!(admin.set_grants(vec![grant("b", None), grant("a", Some(5))]));
    // Order does not matter.
    assert!(!admin.set_grants(vec![grant("a", Some(5)), grant("b", None)]));
    assert!(admin.set_grants(vec![grant("a", Some(6)), grant("b", None)]));
    assert!(admin.remove("a") && !admin.remove("a"));
    assert!(admin.is_admin("b", 1_000) && !admin.is_admin("a", 0));
    // Expired grants are not admin even before the next read prunes them.
    assert!(admin.upsert(grant("c", Some(100))));
    assert!(admin.is_admin("c", 99) && !admin.is_admin("c", 100));
    assert!(!admin.upsert(grant("c", Some(100))));
}

#[test]
fn labels_change_once_per_visible_step() {
    let mut admin = AdminState::default();
    admin.set_grants(vec![grant("a", Some(10 * MIN))]);
    assert!(admin.labels_changed(0));
    assert!(!admin.labels_changed(1_000));
    // Within the same ceil-minute the label is unchanged; the next one flips.
    assert!(!admin.labels_changed(MIN - 1));
    assert!(admin.labels_changed(MIN + 1));
    // Expiry removes the badge: a change; afterwards quiet again.
    assert!(admin.labels_changed(10 * MIN));
    assert!(!admin.labels_changed(11 * MIN));
}

#[test]
fn polling_is_due_only_when_it_should_be() {
    let t0 = Instant::now();
    let mut admin = AdminState::default();
    assert!(admin.grants_due(t0), "never loaded: load now");
    admin.grants_at = Some(t0);
    assert!(!admin.grants_due(t0 + Duration::from_secs(1)));
    admin.set_grants(vec![]);
    // Loaded, nothing granted: no polling.
    assert!(!admin.grants_due(t0 + Duration::from_secs(60)));
    admin.set_grants(vec![grant("a", None)]);
    assert!(!admin.grants_due(t0 + Duration::from_secs(4)));
    assert!(admin.grants_due(t0 + Duration::from_secs(5)));
    admin.grants_in_flight = true;
    assert!(!admin.grants_due(t0 + Duration::from_secs(50)));

    // Audit: outline must be visible; then only with grants or entries.
    let mut admin = AdminState::default();
    admin.set_audit(vec![]);
    admin.audit_at = Some(t0);
    let later = t0 + Duration::from_secs(30);
    assert!(!admin.audit_due(later, true), "no grants, empty list");
    admin.set_grants(vec![grant("a", None)]);
    assert!(!admin.audit_due(later, false), "outline hidden");
    assert!(!admin.audit_due(t0 + Duration::from_secs(9), true));
    assert!(admin.audit_due(t0 + Duration::from_secs(10), true));
    admin.set_grants(vec![]);
    admin.set_audit(vec![turn("t1", 5, &["x"])]);
    assert!(admin.audit_due(later, true), "last list was non-empty");
}

#[test]
fn admin_turns_merge_with_ui_entries_by_time() {
    let ui = |id: u64, at_ms: i64| UiRecent {
        id,
        label: format!("ui {id}"),
        undone: false,
        at_ms,
    };
    let mut other = turn("t-other", 500, &["x"]);
    other.space_id = "s2".into();
    let items = merge_recent(
        vec![ui(3, 300), ui(2, 200), ui(1, 100)],
        &[turn("t1", 250, &["a", "b"]), other, turn("t-empty", 900, &[])],
        "s1",
    );
    let order: Vec<String> = items
        .iter()
        .map(|item| match item {
            RecentItem::Ui(ui) => ui.label.clone(),
            RecentItem::Admin(turn) => turn.turn_id.clone(),
        })
        .collect();
    assert_eq!(order, ["ui 3", "t1", "ui 2", "ui 1"]);
}

#[test]
fn a_turn_reads_as_one_entry_with_its_actions() {
    let mut t = turn("t1", 5, &["Renamed \"A\" to \"B\""]);
    assert_eq!(turn_title(&t), "by admin chat Tidy \u{b7} 1 action");
    t = turn("t1", 5, &["a", "b", "c", "d", "e", "f", "g", "h"]);
    assert_eq!(turn_title(&t), "by admin chat Tidy \u{b7} 8 actions");
    let rows = turn_action_rows(&t);
    assert_eq!(rows.len(), ACTIONS_SHOWN + 1);
    assert_eq!(rows.last().unwrap(), "\u{2026} and 2 more");
    t.undone = true;
    assert!(turn_title(&t).ends_with("(undone)"));

    let ok = AdminUndoTurnReply {
        undone: 3,
        skipped: vec![],
    };
    assert_eq!(undo_notice(&t, &ok), "Undid 3 actions by admin chat Tidy");
    let partial = AdminUndoTurnReply {
        undone: 1,
        skipped: vec!["Moved X".into(), "Renamed Y".into()],
    };
    assert_eq!(
        undo_notice(&t, &partial),
        "Undid 1 action by admin chat Tidy. Skipped 2 (changed since): Moved X; Renamed Y"
    );
}

// ----------------------------------------------------------------------
// RPC against a scripted wire
// ----------------------------------------------------------------------

pub(crate) struct Wire {
    _runtime: tokio::runtime::Runtime,
    pub(crate) requests: tokio::sync::mpsc::Receiver<String>,
    replies: tokio::sync::mpsc::Sender<String>,
}

impl Wire {
    pub(crate) fn new() -> (Self, EngineHandle, tokio::runtime::EnterGuard<'static>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // The guard must outlive the runtime handle it borrows; leak the
        // handle for the test's lifetime.
        let handle: &'static tokio::runtime::Handle = Box::leak(Box::new(runtime.handle().clone()));
        let guard = handle.enter();
        let (out, requests) = tokio::sync::mpsc::channel(32);
        let (replies, inbound) = tokio::sync::mpsc::channel(32);
        let engine = EngineHandle::from_test_client(RpcClient::new(out, inbound));
        (
            Self {
                _runtime: runtime,
                requests,
                replies,
            },
            engine,
            guard,
        )
    }

    /// Every request sent so far as (method, params).
    pub(crate) fn sent(&mut self) -> Vec<(String, serde_json::Value)> {
        self.answer(&|_| None)
    }

    /// Drain the requests; answer each one `script` has a reply for.
    pub(crate) fn answer(
        &mut self,
        script: &dyn Fn(&str) -> Option<serde_json::Value>,
    ) -> Vec<(String, serde_json::Value)> {
        let mut out = Vec::new();
        while let Ok(frame) = self.requests.try_recv() {
            let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let method = request["method"].as_str().unwrap().to_owned();
            if let Some(ok) = script(&method) {
                let reply = serde_json::json!({ "id": request["id"], "ok": ok });
                self._runtime.block_on(async {
                    self.replies.send(reply.to_string()).await.unwrap();
                    while self.replies.capacity() < self.replies.max_capacity() {
                        tokio::task::yield_now().await;
                    }
                });
            }
            out.push((method, request["params"].clone()));
        }
        out
    }
}

fn counting_state(
    cx: &mut gpui::TestAppContext,
    engine: Option<EngineHandle>,
) -> (gpui::Entity<AppState>, Rc<Cell<u32>>) {
    let state = cx.new(|_| AppState::new());
    if let Some(engine) = engine {
        state.update(cx, |state, _| state.set_test_engine(engine));
    }
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    cx.update(|cx| {
        cx.observe(&state, move |_, _| seen.set(seen.get() + 1))
            .detach()
    });
    (state, count)
}

#[gpui::test]
fn grant_and_revoke_send_their_rpcs_and_the_badge_state_follows(cx: &mut gpui::TestAppContext) {
    let (mut wire, engine, _guard) = Wire::new();
    let (state, _) = counting_state(cx, Some(engine));
    state.update(cx, |state, cx| {
        state.grant_admin(grant_params("c1", DurationChoice::EightHours, false), cx)
    });
    cx.run_until_parked();
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, methods::GRANT_ADMIN);
    assert_eq!(
        sent[0].1,
        serde_json::json!({
            "chatId": "c1", "durationSecs": 28_800, "skipBulkConfirmation": false
        })
    );

    state.update(cx, |state, cx| {
        state.admin.set_grants(vec![grant("c1", None)]);
        state.revoke_admin("c1", cx)
    });
    // Gone at once, before the engine answers.
    state.read_with(cx, |state, _| assert!(!state.admin.has_grants()));
    cx.run_until_parked();
    let sent = wire.sent();
    assert_eq!(sent[0].0, methods::REVOKE_ADMIN);
    assert_eq!(sent[0].1, serde_json::json!({ "chatId": "c1" }));
}

#[gpui::test]
fn the_tick_notifies_only_when_a_label_or_the_grants_change(cx: &mut gpui::TestAppContext) {
    let (state, count) = counting_state(cx, None);
    // Nothing granted: ticks are silent.
    state.update(cx, |state, cx| state.admin_tick(false, cx));
    state.update(cx, |state, cx| state.admin_tick(false, cx));
    assert_eq!(count.get(), 0);

    let far = now_ms() + 3 * HOUR + 30 * MIN;
    state.update(cx, |state, cx| {
        state.admin.set_grants(vec![grant("c1", Some(far))]);
        state.admin_tick(false, cx);
    });
    assert_eq!(count.get(), 1, "the badge appeared");
    for _ in 0..5 {
        state.update(cx, |state, cx| state.admin_tick(false, cx));
    }
    assert_eq!(count.get(), 1, "an unchanged label stays quiet");

    // The grant lapses: one more notification, then quiet again.
    state.update(cx, |state, cx| {
        state.admin.set_grants(vec![grant("c1", Some(now_ms() - 1))]);
        state.admin_tick(false, cx);
        state.admin_tick(false, cx);
    });
    assert_eq!(count.get(), 2);
}

#[gpui::test]
fn polling_reads_grants_then_audit_and_applies_the_replies(cx: &mut gpui::TestAppContext) {
    let (mut wire, engine, _guard) = Wire::new();
    let (state, count) = counting_state(cx, Some(engine));
    state.update(cx, |state, cx| {
        state.declare_work_outline_visible(vec!["s1".into()], cx);
        state.admin_tick(true, cx);
    });
    cx.run_until_parked();
    let script = |method: &str| match method {
        m if m == methods::LIST_ADMIN_GRANTS => Some(serde_json::json!([grant("c1", None)])),
        m if m == methods::ADMIN_AUDIT_LIST => {
            Some(serde_json::to_value(vec![turn("t1", 5, &["a"])]).unwrap())
        }
        _ => None,
    };
    let sent = wire.answer(&script);
    let methods_sent: Vec<&str> = sent.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(methods_sent, [methods::LIST_ADMIN_GRANTS, methods::ADMIN_AUDIT_LIST]);
    cx.run_until_parked();
    state.read_with(cx, |state, _| {
        assert!(state.admin.is_admin("c1", now_ms()));
        assert_eq!(state.admin.audit().len(), 1);
    });
    assert!(count.get() >= 2);

    // Not due again within the polling windows: nothing is sent.
    state.update(cx, |state, cx| state.admin_tick(true, cx));
    cx.run_until_parked();
    assert!(wire.sent().is_empty());

    // An unchanged re-read does not notify observers.
    let before = count.get();
    state.update(cx, |state, cx| state.refresh_admin_grants(cx));
    cx.run_until_parked();
    wire.answer(&script);
    cx.run_until_parked();
    assert_eq!(count.get(), before);
}

#[gpui::test]
fn a_turn_undo_sends_admin_undo_turn_and_rereads_the_audit(cx: &mut gpui::TestAppContext) {
    let (mut wire, engine, _guard) = Wire::new();
    let (state, _) = counting_state(cx, Some(engine));
    let task = cx.update(|cx| undo_admin_turn(&state, "run-7", cx)).unwrap();
    cx.run_until_parked();
    let script = |method: &str| {
        (method == methods::ADMIN_UNDO_TURN)
            .then(|| serde_json::json!({ "undone": 2, "skipped": ["Moved X"] }))
    };
    let sent = wire.answer(&script);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, methods::ADMIN_UNDO_TURN);
    assert_eq!(sent[0].1, serde_json::json!({ "turnId": "run-7" }));
    cx.run_until_parked();
    let reply = cx.foreground_executor().block_test(task).expect("a reply");
    assert_eq!(reply.undone, 2);
    assert_eq!(reply.skipped, ["Moved X"]);
    cx.run_until_parked();
    let sent = wire.sent();
    assert_eq!(sent.len(), 1, "the audit is re-read after an undo: {sent:?}");
    assert_eq!(sent[0].0, methods::ADMIN_AUDIT_LIST);
}
