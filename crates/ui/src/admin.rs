//! Admin chats, UI side: the live-grant slice of `AppState`, the pure
//! formatting (time left, badge text, dialog copy), the merge of the engine's
//! admin audit turns into Recent Changes, and the RPC calls. Rendering lives
//! in `shell/admin_ui.rs`; the engine owns the grants (`mast_proto::admin`).
//!
//! Observers are only notified when something shown changes: grants compare
//! by value, and the once-a-second tick only notifies when a badge label
//! (the minute count, or expiry) differs from what is on screen.

use std::time::{Duration, Instant};

use gpui::Context;
use mast_proto::{
    AdminAuditListParams, AdminAuditTurn, AdminGrant, AdminUndoTurnParams, AdminUndoTurnReply,
    GrantAdminParams,
};
use mast_rpc::methods;

use crate::state::AppState;

/// Grants are re-read this often while any exist.
pub(crate) const GRANT_POLL: Duration = Duration::from_secs(5);
/// The audit list is re-read this often while the outline is visible.
pub(crate) const AUDIT_POLL: Duration = Duration::from_secs(10);
/// Action summaries listed under one admin turn in Recent Changes.
pub(crate) const ACTIONS_SHOWN: usize = 6;

// ----------------------------------------------------------------------
// Duration choices and copy
// ----------------------------------------------------------------------

/// The grant dialog's duration choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DurationChoice {
    OneHour,
    #[default]
    FourHours,
    EightHours,
    TwentyFourHours,
    UntilRevoked,
}

impl DurationChoice {
    pub(crate) const ALL: [DurationChoice; 5] = [
        Self::OneHour,
        Self::FourHours,
        Self::EightHours,
        Self::TwentyFourHours,
        Self::UntilRevoked,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OneHour => "1 hour",
            Self::FourHours => "4 hours",
            Self::EightHours => "8 hours",
            Self::TwentyFourHours => "24 hours",
            Self::UntilRevoked => "Until revoked",
        }
    }

    /// Selector suffix (`admin-dialog-duration-{key}`).
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::OneHour => "1h",
            Self::FourHours => "4h",
            Self::EightHours => "8h",
            Self::TwentyFourHours => "24h",
            Self::UntilRevoked => "forever",
        }
    }

    /// `None` = until revoked.
    pub(crate) fn secs(self) -> Option<u64> {
        match self {
            Self::OneHour => Some(60 * 60),
            Self::FourHours => Some(4 * 60 * 60),
            Self::EightHours => Some(8 * 60 * 60),
            Self::TwentyFourHours => Some(24 * 60 * 60),
            Self::UntilRevoked => None,
        }
    }
}

pub(crate) fn grant_params(
    chat_id: &str,
    choice: DurationChoice,
    skip_bulk_confirmation: bool,
) -> GrantAdminParams {
    GrantAdminParams {
        chat_id: chat_id.to_owned(),
        duration_secs: choice.secs(),
        skip_bulk_confirmation,
    }
}

/// The dialog's one-sentence explanation.
pub(crate) fn explanation(space_name: &str) -> String {
    format!(
        "This chat's agent will be able to do anything you can do in the app inside {space_name} \
         until the grant expires, except Linear or GitHub writes and irreversible deletes. \
         Everything is attributed to the chat and undoable."
    )
}

/// "Grant admin for this Space…" or, for a chat that already is admin,
/// "Renew admin…".
pub(crate) fn grant_entry_label(is_admin: bool) -> &'static str {
    if is_admin {
        "Renew admin\u{2026}"
    } else {
        "Grant admin for this Space\u{2026}"
    }
}

/// The dialog's opt-in checkbox (off by default).
pub(crate) const SKIP_BULK_LABEL: &str = "Skip bulk confirmations";

pub(crate) const REVOKE_LABEL: &str = "Revoke admin";

// ----------------------------------------------------------------------
// Time left and the badge
// ----------------------------------------------------------------------

/// "3h left", "12m left", "<1m left" or "until revoked"; `None` once expired.
/// Minutes round up and hours round down, so a fresh 4 h grant reads "4h".
pub(crate) fn time_left(expires_at_ms: Option<i64>, now_ms: i64) -> Option<String> {
    let at = match expires_at_ms {
        None => return Some("until revoked".to_owned()),
        Some(at) => at,
    };
    let left = at - now_ms;
    if left <= 0 {
        return None;
    }
    if left < 60_000 {
        return Some("<1m left".to_owned());
    }
    let minutes = (left + 59_999) / 60_000;
    Some(if minutes < 60 {
        format!("{minutes}m left")
    } else {
        format!("{}h left", minutes / 60)
    })
}

/// "Admin · <Space> · 3h left"; `None` once the grant expired.
pub(crate) fn badge_text(space_name: &str, grant: &AdminGrant, now_ms: i64) -> Option<String> {
    let left = time_left(grant.expires_at_ms, now_ms)?;
    Some(format!("Admin \u{b7} {space_name} \u{b7} {left}"))
}

/// The badge's tooltip: what the grant does beyond the label.
pub(crate) fn badge_tooltip(grant: &AdminGrant) -> Option<&'static str> {
    grant
        .skip_bulk_confirmation
        .then_some("Bulk changes run without asking for your approval")
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ----------------------------------------------------------------------
// State slice
// ----------------------------------------------------------------------

/// Live grants plus the admin audit cache.
#[derive(Debug, Default)]
pub(crate) struct AdminState {
    /// Sorted by chat id so equal lists compare equal.
    grants: Vec<AdminGrant>,
    pub(crate) loaded: bool,
    audit: Vec<AdminAuditTurn>,
    audit_loaded: bool,
    grants_at: Option<Instant>,
    audit_at: Option<Instant>,
    grants_in_flight: bool,
    audit_in_flight: bool,
    /// (chat id, time-left text) as last shown; the tick re-renders on change.
    shown: Vec<(String, String)>,
}

impl AdminState {
    /// Replace the grants; `true` when they changed.
    pub(crate) fn set_grants(&mut self, mut grants: Vec<AdminGrant>) -> bool {
        grants.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        let changed = grants != self.grants || !self.loaded;
        self.loaded = true;
        if changed {
            self.grants = grants;
        }
        changed
    }

    /// Insert or replace one grant (the reply of a grant / renew).
    pub(crate) fn upsert(&mut self, grant: AdminGrant) -> bool {
        let mut grants = self.grants.clone();
        grants.retain(|g| g.chat_id != grant.chat_id);
        grants.push(grant);
        self.set_grants(grants)
    }

    /// Drop a chat's grant; `true` when it had one.
    pub(crate) fn remove(&mut self, chat_id: &str) -> bool {
        let before = self.grants.len();
        self.grants.retain(|g| g.chat_id != chat_id);
        self.grants.len() != before
    }

    /// Any grant the engine reported (live or about to be pruned).
    pub(crate) fn has_grants(&self) -> bool {
        !self.grants.is_empty()
    }

    /// The chat's grant while it is live.
    pub(crate) fn grant_for(&self, chat_id: &str, now_ms: i64) -> Option<&AdminGrant> {
        self.grants
            .iter()
            .find(|g| g.chat_id == chat_id && g.is_live(now_ms))
    }

    pub(crate) fn is_admin(&self, chat_id: &str, now_ms: i64) -> bool {
        self.grant_for(chat_id, now_ms).is_some()
    }

    pub(crate) fn audit(&self) -> &[AdminAuditTurn] {
        &self.audit
    }

    pub(crate) fn set_audit(&mut self, turns: Vec<AdminAuditTurn>) -> bool {
        let changed = turns != self.audit || !self.audit_loaded;
        self.audit_loaded = true;
        if changed {
            self.audit = turns;
        }
        changed
    }

    /// The time-left texts on screen, for change detection.
    fn labels(&self, now_ms: i64) -> Vec<(String, String)> {
        self.grants
            .iter()
            .filter_map(|g| Some((g.chat_id.clone(), time_left(g.expires_at_ms, now_ms)?)))
            .collect()
    }

    /// `true` when a badge label (or which badges exist) differs from the
    /// last call: the caller then re-renders.
    pub(crate) fn labels_changed(&mut self, now_ms: i64) -> bool {
        let labels = self.labels(now_ms);
        if labels == self.shown {
            return false;
        }
        self.shown = labels;
        true
    }

    /// Is a grants read due? Never loaded, or grants exist and 5 s passed.
    fn grants_due(&self, now: Instant) -> bool {
        if self.grants_in_flight {
            return false;
        }
        if !self.loaded {
            return self.grants_at.is_none_or(|at| now.duration_since(at) >= GRANT_POLL);
        }
        self.has_grants()
            && self
                .grants_at
                .is_none_or(|at| now.duration_since(at) >= GRANT_POLL)
    }

    /// Is an audit read due? Only while the outline is visible, and only if
    /// any grant exists or the last list was non-empty.
    fn audit_due(&self, now: Instant, outline_visible: bool) -> bool {
        if self.audit_in_flight || !outline_visible {
            return false;
        }
        if !(self.has_grants() || !self.audit.is_empty() || !self.audit_loaded) {
            return false;
        }
        self.audit_at
            .is_none_or(|at| now.duration_since(at) >= AUDIT_POLL)
    }

    /// The newest admin turn of `space_id` that can still be undone.
    pub(crate) fn newest_undoable(&self, space_id: &str) -> Option<&AdminAuditTurn> {
        self.audit
            .iter()
            .filter(|t| t.space_id == space_id && !t.undone && !t.actions.is_empty())
            .max_by_key(|t| t.updated_at_ms)
    }
}

// ----------------------------------------------------------------------
// RPC
// ----------------------------------------------------------------------

impl AppState {
    /// The 1 s heartbeat: re-render when a badge label changes, and (when
    /// `polling`) read grants / audit when due. Observers hear only changes.
    pub(crate) fn admin_tick(&mut self, polling: bool, cx: &mut Context<Self>) {
        if self.admin.labels_changed(now_ms()) {
            cx.notify();
        }
        if !polling {
            return;
        }
        let now = Instant::now();
        if self.admin.grants_due(now) {
            self.refresh_admin_grants(cx);
        }
        if self.admin.audit_due(now, self.work_outline_is_visible()) {
            self.refresh_admin_audit(cx);
        }
    }

    /// Read `ListAdminGrants` now.
    pub(crate) fn refresh_admin_grants(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        self.admin.grants_in_flight = true;
        self.admin.grants_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let grants = engine
                .client()
                .call_as::<Vec<AdminGrant>>(methods::LIST_ADMIN_GRANTS, serde_json::json!({}))
                .await
                .ok();
            this.update(cx, |state, cx| {
                state.admin.grants_in_flight = false;
                if let Some(grants) = grants
                    && state.admin.set_grants(grants)
                {
                    state.admin.labels_changed(now_ms());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Read `AdminAuditList` now.
    pub(crate) fn refresh_admin_audit(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        self.admin.audit_in_flight = true;
        self.admin.audit_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let turns = engine
                .client()
                .call_as::<Vec<AdminAuditTurn>>(
                    methods::ADMIN_AUDIT_LIST,
                    serde_json::to_value(AdminAuditListParams::default()).unwrap_or_default(),
                )
                .await
                .ok();
            this.update(cx, |state, cx| {
                state.admin.audit_in_flight = false;
                if let Some(turns) = turns
                    && state.admin.set_audit(turns)
                {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `GrantAdmin` (a renew is a second grant); the reply lands in the slice
    /// and the list is re-read.
    pub(crate) fn grant_admin(&mut self, params: GrantAdminParams, cx: &mut Context<Self>) {
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = engine
                .client()
                .call_as::<AdminGrant>(
                    methods::GRANT_ADMIN,
                    serde_json::to_value(&params).unwrap_or_default(),
                )
                .await
                .ok();
            this.update(cx, |state, cx| {
                if let Some(grant) = reply
                    && state.admin.upsert(grant)
                {
                    state.admin.labels_changed(now_ms());
                    cx.notify();
                }
                state.refresh_admin_grants(cx);
            })
            .ok();
        })
        .detach();
    }

    /// `RevokeAdmin`: the badge goes at once; the re-read is the truth.
    pub(crate) fn revoke_admin(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.admin.remove(chat_id) {
            self.admin.labels_changed(now_ms());
            cx.notify();
        }
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        let chat_id = chat_id.to_owned();
        cx.spawn(async move |this, cx| {
            let _ = engine
                .client()
                .call(methods::REVOKE_ADMIN, serde_json::json!({ "chatId": chat_id }))
                .await;
            this.update(cx, |state, cx| state.refresh_admin_grants(cx)).ok();
        })
        .detach();
    }
}

/// `AdminUndoTurn`, then the audit re-read; `done` hears the reply.
pub(crate) fn undo_admin_turn(
    state: &gpui::Entity<AppState>,
    turn_id: &str,
    cx: &mut gpui::App,
) -> Option<gpui::Task<Option<AdminUndoTurnReply>>> {
    let engine = state.read(cx).engine().cloned()?;
    let params = AdminUndoTurnParams {
        turn_id: turn_id.to_owned(),
    };
    let state = state.downgrade();
    Some(cx.spawn(async move |cx| {
        let reply = engine
            .client()
            .call_as::<AdminUndoTurnReply>(
                methods::ADMIN_UNDO_TURN,
                serde_json::to_value(&params).unwrap_or_default(),
            )
            .await
            .ok();
        state
            .update(cx, |state, cx| state.refresh_admin_audit(cx))
            .ok();
        reply
    }))
}

// ----------------------------------------------------------------------
// Recent Changes
// ----------------------------------------------------------------------

/// One UI action-log entry as Recent Changes sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiRecent {
    pub id: u64,
    pub label: String,
    pub undone: bool,
    pub at_ms: i64,
}

/// A Recent Changes row source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecentItem {
    Ui(UiRecent),
    Admin(AdminAuditTurn),
}

impl RecentItem {
    fn at_ms(&self) -> i64 {
        match self {
            Self::Ui(ui) => ui.at_ms,
            Self::Admin(turn) => turn.updated_at_ms,
        }
    }
}

/// The Space's UI entries and admin turns, newest first by time.
pub(crate) fn merge_recent(
    ui: Vec<UiRecent>,
    turns: &[AdminAuditTurn],
    space_id: &str,
) -> Vec<RecentItem> {
    let mut items: Vec<RecentItem> = ui.into_iter().map(RecentItem::Ui).collect();
    items.extend(
        turns
            .iter()
            .filter(|t| t.space_id == space_id && !t.actions.is_empty())
            .cloned()
            .map(RecentItem::Admin),
    );
    // Stable: equal times keep the UI entries (already newest first) in order.
    items.sort_by_key(|item| std::cmp::Reverse(item.at_ms()));
    items
}

/// "<label> · N actions".
pub(crate) fn turn_title(turn: &AdminAuditTurn) -> String {
    let n = turn.actions.len();
    let noun = if n == 1 { "action" } else { "actions" };
    let mut title = format!("{} \u{b7} {n} {noun}", turn.label);
    if turn.undone {
        title.push_str(" (undone)");
    }
    title
}

/// The per-action summaries listed under a turn (capped).
pub(crate) fn turn_action_rows(turn: &AdminAuditTurn) -> Vec<String> {
    let mut rows: Vec<String> = turn
        .actions
        .iter()
        .take(ACTIONS_SHOWN)
        .map(|a| a.summary.clone())
        .collect();
    if turn.actions.len() > ACTIONS_SHOWN {
        rows.push(format!(
            "\u{2026} and {} more",
            turn.actions.len() - ACTIONS_SHOWN
        ));
    }
    rows
}

/// What to say after `AdminUndoTurn` answered.
pub(crate) fn undo_notice(turn: &AdminAuditTurn, reply: &AdminUndoTurnReply) -> String {
    let n = reply.undone;
    let noun = if n == 1 { "action" } else { "actions" };
    let mut text = format!("Undid {n} {noun} {}", turn.label);
    if !reply.skipped.is_empty() {
        text.push_str(&format!(
            ". Skipped {} (changed since): {}",
            reply.skipped.len(),
            reply.skipped.join("; ")
        ));
    }
    text
}

#[cfg(test)]
pub(crate) mod tests;
