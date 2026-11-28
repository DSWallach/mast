//! Headroom: how far each usage window is from a provider's hard cap.
//!
//! The data comes through [`HeadroomSource`], so the Quota caps section does
//! not care where windows are read from. The page uses [`BrokerWindows`]: the
//! quota broker's live view from `GetQuotaHeadroom`, with [`AccountWindows`]
//! (the plan windows on each account) until that reply arrives, or for an
//! engine that cannot answer it.

use chrono::{DateTime, Utc};
use mast_engine::quota_probes::{ProviderHeadroom, QuotaHeadroom};
use mast_proto::{AgentAccountsSnapshot, HarnessId, ScopedUsageWindow, UsageWindowScope};

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

/// The quota broker's windows. A provider it could not read shows its local
/// estimate, or nothing ("Usage unavailable") when usage is unknown; it never
/// falls back to older account windows once the broker has answered.
pub struct BrokerWindows<'a> {
    pub live: Option<&'a QuotaHeadroom>,
    pub accounts: AccountWindows<'a>,
}

impl HeadroomSource for BrokerWindows<'_> {
    fn windows(&self, harness: HarnessId) -> Vec<HeadroomWindow> {
        let Some(live) = self.live else {
            return self.accounts.windows(harness);
        };
        let id = match harness {
            HarnessId::ClaudeCode => "claude-code",
            HarnessId::Codex => "codex",
            HarnessId::Antigravity => "antigravity",
            _ => return Vec::new(),
        };
        live.providers
            .iter()
            .find(|provider| provider.harness == id)
            .map(broker_windows)
            .unwrap_or_default()
    }
}

fn broker_windows(provider: &ProviderHeadroom) -> Vec<HeadroomWindow> {
    let window = |w: &ScopedUsageWindow| HeadroomWindow {
        label: w.label.clone(),
        model: match &w.scope {
            UsageWindowScope::ModelFamily(family) => Some(family.clone()),
            UsageWindowScope::AccountWide => None,
        },
        // The broker caps on whichever of the two is higher.
        used: w
            .used_fraction
            .max(w.credit_budget_fraction.unwrap_or(0.0))
            .clamp(0.0, 1.0),
        resets_at: w.resets_at,
    };
    if !provider.windows.is_empty() {
        return provider.windows.iter().map(window).collect();
    }
    provider
        .estimated
        .map(|used| HeadroomWindow {
            // Short enough for the bar's label column.
            label: "Estimated".into(),
            model: None,
            used: used.clamp(0.0, 1.0),
            resets_at: None,
        })
        .into_iter()
        .collect()
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
        || [
            "session", "hour", "day", "daily", "week", "month", "wk", "mo",
        ]
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
    fn broker_windows_replace_account_windows_once_they_arrive() {
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![account(HarnessId::Codex, true, &[("5h", 0.1)])],
            warnings: vec![],
        };
        let live = QuotaHeadroom {
            providers: vec![
                ProviderHeadroom {
                    harness: "claude-code".into(),
                    windows: vec![ScopedUsageWindow {
                        label: "Opus Week".into(),
                        used_fraction: 0.2,
                        credit_budget_fraction: Some(0.7),
                        scope: UsageWindowScope::ModelFamily("Opus".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ProviderHeadroom {
                    harness: "antigravity".into(),
                    estimated: Some(0.4),
                    ..Default::default()
                },
                ProviderHeadroom {
                    harness: "codex".into(),
                    needs_sign_in: true,
                    ..Default::default()
                },
            ],
        };
        let source = |live| BrokerWindows {
            live,
            accounts: AccountWindows(Some(&snapshot)),
        };
        assert_eq!(source(None).windows(HarnessId::Codex)[0].used, 0.1);
        let broker = source(Some(&live));
        let claude = broker.windows(HarnessId::ClaudeCode);
        assert_eq!(claude[0].model.as_deref(), Some("Opus"));
        assert_eq!(claude[0].used, 0.7);
        let agy = broker.windows(HarnessId::Antigravity);
        assert_eq!(agy[0].label, "Estimated");
        assert_eq!(agy[0].used, 0.4);
        assert!(broker.windows(HarnessId::Codex).is_empty());
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
        let labels: Vec<_> = claude
            .iter()
            .map(|w| (w.label.as_str(), w.model.as_deref()))
            .collect();
        assert_eq!(
            labels,
            [
                ("Session", None),
                ("Week", None),
                ("Sonnet", Some("Sonnet"))
            ]
        );
        assert_eq!(claude[0].used, 0.4, "the inactive account is not read");
        assert_eq!(source.windows(HarnessId::Codex).len(), 1);
        assert!(source.windows(HarnessId::Antigravity).is_empty());
        assert!(AccountWindows(None).windows(HarnessId::Codex).is_empty());
    }
}
