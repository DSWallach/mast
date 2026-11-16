//! Automatic quota-hit detection and timed default-model switching.
//!
//! Checks both polled account usage windows (`AgentUsageWindow::used_fraction >= 1.0`)
//! and recent session errors indicating rate/usage limits. When the active default
//! model/harness hits its quota, the shell prompts the user to switch the default
//! to another model for a chosen time window; once `expires_at` passes, the
//! previous default selection is restored automatically.

use chrono::{DateTime, Duration, Utc};
use mast_proto::{AgentAccount, Chat, HarnessId, Session, SessionStatus};

use crate::settings::{DefaultModelPreference, TemporaryDefaultModelOverride};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaHit {
    pub harness: HarnessId,
    pub model_id: Option<String>,
    pub model_label: Option<String>,
    pub reason: String,
    pub resets_at: Option<DateTime<Utc>>,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchDurationOption {
    UntilReset(DateTime<Utc>),
    Minutes(i64),
}

impl SwitchDurationOption {
    pub fn options_for(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Vec<Self> {
        let mut opts = Vec::new();
        if let Some(reset) = resets_at
            && reset > now
        {
            opts.push(Self::UntilReset(reset));
        }
        opts.extend([
            Self::Minutes(30),
            Self::Minutes(60),
            Self::Minutes(120),
            Self::Minutes(240),
            Self::Minutes(480),
            Self::Minutes(1440),
        ]);
        opts
    }

    pub fn expires_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::UntilReset(at) => *at,
            Self::Minutes(mins) => now + Duration::minutes(*mins),
        }
    }

    pub fn label(&self, now: DateTime<Utc>) -> String {
        match self {
            Self::UntilReset(at) => {
                let mins = (*at - now).num_minutes().max(1);
                if mins >= 60 {
                    let h = mins / 60;
                    let m = mins % 60;
                    if m == 0 {
                        format!("Until reset ({h}h)")
                    } else {
                        format!("Until reset ({h}h {m}m)")
                    }
                } else {
                    format!("Until reset ({mins}m)")
                }
            }
            Self::Minutes(mins) => {
                if *mins >= 60 && *mins % 60 == 0 {
                    format!("{}h", mins / 60)
                } else {
                    format!("{mins}m")
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCandidate {
    pub harness: HarnessId,
    pub harness_name: String,
    pub model: String,
    pub model_label: String,
}

pub fn is_quota_error_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "usage limit",
        "rate limit",
        "rate_limit",
        "quota exceeded",
        "hit your quota",
        "limit reached",
        "exceeded your current quota",
        "too many requests",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Inspect polled usage accounts for the active default harness. Returns a
/// `QuotaHit` if any window on the active account (or only account) for
/// `active_harness` is at 100% (`used_fraction >= 0.999`) and not already past
/// `resets_at`.
pub fn detect_usage_quota_hit(
    active_harness: HarnessId,
    active_model_id: Option<&str>,
    active_model_label: Option<&str>,
    accounts: &[AgentAccount],
    now: DateTime<Utc>,
) -> Option<QuotaHit> {
    let matching: Vec<&AgentAccount> = accounts
        .iter()
        .filter(|a| a.harness == active_harness)
        .collect();
    let account = matching
        .iter()
        .copied()
        .find(|a| a.active)
        .or_else(|| matching.first().copied())?;

    let window = account.usage_windows.iter().find(|w| {
        w.used_fraction >= 0.999 && w.resets_at.is_none_or(|reset| reset > now)
    })?;

    let resets_ms = window
        .resets_at
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    let subject = active_model_label
        .or(active_model_id)
        .unwrap_or_else(|| harness_display_name(active_harness));
    let reason = format!("{subject} hit its {} quota", window.label);
    Some(QuotaHit {
        harness: active_harness,
        model_id: active_model_id.map(str::to_owned),
        model_label: active_model_label.map(str::to_owned),
        reason,
        resets_at: window.resets_at,
        dedupe_key: format!("usage:{active_harness:?}:{}:{resets_ms}", window.label),
    })
}

/// Inspect recent errored sessions for a quota/rate-limit message matching the
/// active default harness.
pub fn detect_session_quota_hit(
    active_harness: HarnessId,
    active_model_id: Option<&str>,
    active_model_label: Option<&str>,
    chats: &[Chat],
    sessions: &[Session],
    now: DateTime<Utc>,
) -> Option<QuotaHit> {
    let recent_cutoff = now - Duration::minutes(5);
    for session in sessions {
        if session.status != SessionStatus::Errored || session.updated_at < recent_cutoff {
            continue;
        }
        let Some(chat) = chats.iter().find(|c| c.id == session.chat_id && !c.archived) else {
            continue;
        };
        let chat_harness = chat.config.as_ref().map(|c| c.harness);
        if chat_harness != Some(active_harness) {
            continue;
        }
        if let Some(model_id) = active_model_id
            && let Some(chat_model) = chat.config.as_ref().and_then(|c| c.model.as_deref())
            && chat_model != model_id
        {
            continue;
        }
        let Some(preview) = chat.last_message_preview.as_deref() else {
            continue;
        };
        if !is_quota_error_text(preview) {
            continue;
        }
        let subject = active_model_label
            .or(active_model_id)
            .unwrap_or_else(|| harness_display_name(active_harness));
        return Some(QuotaHit {
            harness: active_harness,
            model_id: active_model_id.map(str::to_owned),
            model_label: active_model_label.map(str::to_owned),
            reason: format!("{subject} hit a usage/rate limit"),
            resets_at: None,
            dedupe_key: format!("session:{}:{}", chat.id, session.updated_at.timestamp()),
        });
    }
    None
}

pub fn build_temporary_override(
    candidate: &QuotaCandidate,
    previous: DefaultModelPreference,
    duration: &SwitchDurationOption,
    reason: String,
    now: DateTime<Utc>,
) -> TemporaryDefaultModelOverride {
    TemporaryDefaultModelOverride {
        harness: candidate.harness,
        model: candidate.model.clone(),
        model_label: Some(candidate.model_label.clone()),
        previous,
        started_at: now,
        expires_at: duration.expires_at(now),
        reason: Some(reason),
    }
}

pub fn harness_display_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Cursor => "Cursor",
        HarnessId::Devin => "Devin",
        HarnessId::Grok => "Grok",
        HarnessId::Hermes => "Hermes",
        HarnessId::Pi => "Pi",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Antigravity => "Antigravity",
        HarnessId::Mock => "Mock",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{UiSettings, effective_default_model};
    use chrono::TimeZone;
    use mast_proto::AgentUsageWindow;

    fn t(mins_offset: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, 18, 0, 0).unwrap() + Duration::minutes(mins_offset)
    }

    fn account(harness: HarnessId, active: bool, windows: Vec<AgentUsageWindow>) -> AgentAccount {
        AgentAccount {
            id: "acc-1".into(),
            harness,
            email: None,
            plan_label: None,
            active,
            usage_windows: windows,
            display_name: None,
            organization: None,
            auth_kind: None,
            switchable: true,
            saved_at: None,
            promoted: false,
        }
    }

    #[test]
    fn usage_quota_hit_detects_full_window_and_ignores_stale_or_partial() {
        let now = t(0);
        let accounts = vec![account(
            HarnessId::ClaudeCode,
            true,
            vec![AgentUsageWindow {
                label: "5-hour".into(),
                used_fraction: 0.85,
                resets_at: Some(t(90)),
            }],
        )];
        assert!(
            detect_usage_quota_hit(
                HarnessId::ClaudeCode,
                Some("claude-opus-5-5"),
                Some("Claude Opus 5.5"),
                &accounts,
                now
            )
            .is_none()
        );

        let full_accounts = vec![account(
            HarnessId::ClaudeCode,
            true,
            vec![AgentUsageWindow {
                label: "5-hour".into(),
                used_fraction: 1.0,
                resets_at: Some(t(90)),
            }],
        )];
        let hit = detect_usage_quota_hit(
            HarnessId::ClaudeCode,
            Some("claude-opus-5-5"),
            Some("Claude Opus 5.5"),
            &full_accounts,
            now,
        )
        .expect("should detect 100% quota");
        assert_eq!(hit.harness, HarnessId::ClaudeCode);
        assert_eq!(hit.resets_at, Some(t(90)));
        assert!(hit.reason.contains("Claude Opus 5.5"));

        // Past reset time should not trigger.
        assert!(
            detect_usage_quota_hit(
                HarnessId::ClaudeCode,
                Some("claude-opus-5-5"),
                Some("Claude Opus 5.5"),
                &full_accounts,
                t(91),
            )
            .is_none()
        );
    }

    #[test]
    fn temporary_override_switches_default_and_reverts_after_window() {
        let now = t(0);
        let previous = DefaultModelPreference {
            harness: Some(HarnessId::ClaudeCode),
            model: Some("claude-opus-5-5".into()),
            model_label: Some("Claude Opus 5.5".into()),
        };
        let candidate = QuotaCandidate {
            harness: HarnessId::Codex,
            harness_name: "Codex".into(),
            model: "gpt-5.4".into(),
            model_label: "GPT-5.4".into(),
        };
        let ov = build_temporary_override(
            &candidate,
            previous.clone(),
            &SwitchDurationOption::Minutes(60),
            "Claude Opus 5.5 hit its 5-hour quota".into(),
            now,
        );
        let mut settings = UiSettings::default();
        settings.default_model = previous.clone();
        settings.temporary_default_override = Some(ov);

        // During the 60m window, effective default is GPT-5.4.
        let during = effective_default_model(&settings, t(30));
        assert_eq!(during.harness, Some(HarnessId::Codex));
        assert_eq!(during.model.as_deref(), Some("gpt-5.4"));

        // After the 60m window completes, effective default reverts to Claude Opus 5.5.
        let after = effective_default_model(&settings, t(60));
        assert_eq!(after, previous);
    }

    #[test]
    fn quota_error_text_matches_rate_and_usage_limits() {
        assert!(is_quota_error_text(
            "Claude usage limit reached — try again after the limit resets."
        ));
        assert!(is_quota_error_text("HTTP 429: Too many requests"));
        assert!(!is_quota_error_text("cargo build failed with exit code 1"));
    }
}
