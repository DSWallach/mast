//! Headroom: how far each usage window is from a provider's hard cap.
//!
//! The data comes through [`HeadroomSource`], so the Quota caps section does
//! not care where windows are read from. Today that is [`AccountWindows`], the
//! plan windows already shown on each account (W4-T1's parsers). Live
//! `QuotaBroker` data (W4-T2b) is one more implementation of the trait; nothing
//! in the page changes.

use chrono::{DateTime, Utc};
use mast_proto::{AgentAccountsSnapshot, HarnessId};

/// How close a window is to the cap, as far as the bar's colour goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomState {
    /// More than [`CLOSE_MARGIN`] below the cap.
    Clear,
    /// Within [`CLOSE_MARGIN`] of the cap.
    Close,
    /// At or past the cap.
    Over,
}

/// A window is "close" when this much of the plan, or less, is left before the
/// cap (10 percentage points).
pub const CLOSE_MARGIN: f32 = 0.10;

/// One usage window of a provider: a time window (session, week) or, when the
/// provider reports one, a per-model bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadroomWindow {
    pub label: String,
    /// `Some(model)` for a per-model bucket, `None` for a time window.
    pub model: Option<String>,
    /// 0.0..=1.0 of the window used.
    pub used: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

/// Where the headroom windows of a provider come from.
pub trait HeadroomSource {
    fn windows(&self, harness: HarnessId) -> Vec<HeadroomWindow>;
}

/// The windows already on the account list: the active account of each
/// provider (the one new work would run on).
pub struct AccountWindows<'a>(pub Option<&'a AgentAccountsSnapshot>);

impl HeadroomSource for AccountWindows<'_> {
    fn windows(&self, harness: HarnessId) -> Vec<HeadroomWindow> {
        let Some(snapshot) = self.0 else {
            return Vec::new();
        };
        snapshot
            .accounts
            .iter()
            .find(|account| account.harness == harness && account.active)
            .map(|account| {
                account
                    .usage_windows
                    .iter()
                    .map(|window| HeadroomWindow {
                        label: window.label.clone(),
                        model: (!is_time_label(&window.label)).then(|| window.label.clone()),
                        used: window.used_fraction.clamp(0.0, 1.0),
                        resets_at: window.resets_at,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Whether a window label names a stretch of time ("Session", "Week", "5h",
/// "Weekly") rather than a model. Account windows carry no scope yet, so a
/// label that is not a time window is taken to be a model bucket.
pub fn is_time_label(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    lower.starts_with(|c: char| c.is_ascii_digit())
        || ["session", "hour", "day", "daily", "week", "month", "wk", "mo"]
            .iter()
            .any(|word| lower.contains(word))
}

/// A window set against a cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Headroom {
    pub used: f32,
    pub cap: f32,
}

impl Headroom {
    pub fn new(used: f32, cap: f32) -> Self {
        Self {
            used: used.clamp(0.0, 1.0),
            cap: cap.clamp(0.0, 1.0),
        }
    }

    pub fn state(self) -> HeadroomState {
        if self.used >= self.cap {
            HeadroomState::Over
        } else if self.used >= self.cap - CLOSE_MARGIN {
            HeadroomState::Close
        } else {
            HeadroomState::Clear
        }
    }

    /// "23% left to cap", or "over cap by 4%".
    pub fn label(self) -> String {
        let points = ((self.cap - self.used).abs() * 100.0).round() as u32;
        match self.state() {
            HeadroomState::Over => format!("over cap by {points}%"),
            _ => format!("{points}% left to cap"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{AgentAccount, AgentUsageWindow};

    fn account(harness: HarnessId, active: bool, windows: &[(&str, f32)]) -> AgentAccount {
        AgentAccount {
            id: format!("{harness:?}-{active}"),
            harness,
            email: None,
            plan_label: None,
            active,
            usage_windows: windows
                .iter()
                .map(|(label, used)| AgentUsageWindow {
                    label: (*label).into(),
                    used_fraction: *used,
                    resets_at: None,
                })
                .collect(),
            display_name: None,
            organization: None,
            auth_kind: None,
            switchable: true,
            saved_at: None,
            promoted: false,
        }
    }

    #[test]
    fn state_follows_the_cap_not_the_global_thresholds() {
        let at = |used, cap| Headroom::new(used, cap).state();
        assert_eq!(at(0.70, 0.85), HeadroomState::Clear);
        assert_eq!(at(0.76, 0.85), HeadroomState::Close);
        assert_eq!(at(0.85, 0.85), HeadroomState::Over);
        // 90% used is fine under a 95% cap's margin only when 10 points away.
        assert_eq!(at(0.84, 0.95), HeadroomState::Clear);
        assert_eq!(at(0.90, 0.95), HeadroomState::Close);
    }

    #[test]
    fn labels_say_how_much_room_is_left() {
        assert_eq!(Headroom::new(0.62, 0.85).label(), "23% left to cap");
        assert_eq!(Headroom::new(0.85, 0.85).label(), "over cap by 0%");
        assert_eq!(Headroom::new(0.91, 0.85).label(), "over cap by 6%");
        assert_eq!(Headroom::new(2.0, 0.85).label(), "over cap by 15%");
    }

    #[test]
    fn account_windows_use_the_active_account_and_split_models() {
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![
                account(HarnessId::ClaudeCode, false, &[("Session", 0.99)]),
                account(
                    HarnessId::ClaudeCode,
                    true,
                    &[("Session", 0.4), ("Week", 0.61), ("Sonnet", 0.2)],
                ),
                account(HarnessId::Codex, true, &[("5h", 0.1)]),
            ],
            warnings: vec![],
        };
        let source = AccountWindows(Some(&snapshot));
        let claude = source.windows(HarnessId::ClaudeCode);
        let labels: Vec<_> = claude.iter().map(|w| (w.label.as_str(), w.model.as_deref())).collect();
        assert_eq!(
            labels,
            [("Session", None), ("Week", None), ("Sonnet", Some("Sonnet"))]
        );
        assert_eq!(claude[0].used, 0.4, "the inactive account is not read");
        assert_eq!(source.windows(HarnessId::Codex).len(), 1);
        assert!(source.windows(HarnessId::Antigravity).is_empty());
        assert!(AccountWindows(None).windows(HarnessId::Codex).is_empty());
    }
}
