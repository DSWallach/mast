//! Quota classification for agents that report limits as free text (ACP
//! `session/prompt` errors, opencode `session.error` and retry notices).
//!
//! The codes match the ones the Claude, Codex and Cursor harnesses already
//! emit, so the engine's credit fallback and reset-wake paths read them the
//! same way:
//!
//! - `usage_5h` / `usage_weekly` / `usage_weekly_model`: a plan window is spent;
//! - `usage_limit`: out of credits or plan usage with no window named;
//! - `rate_limit`: a transient throttle that clears on its own.
//!
//! A transient throttle's own wording ("rate limit", "429", "too many
//! requests") is exactly what the UI quota guard reads as out of credits, so
//! it is replaced, as Cursor's is. A usage limit always says "usage limit
//! reached" so the guard sees it.
//!
//! The reset time is unknown from text alone (`resets_at` stays `None`).

use mast_proto::AgentEvent;

/// A classified limit error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Limit {
    pub code: &'static str,
}

fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(word).any(|(at, _)| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + word.len()).copied();
        let edge = |b: Option<u8>| b.is_none_or(|b| !b.is_ascii_alphanumeric());
        edge(before) && edge(after)
    })
}

/// Classify `text`, or `None` when it is not a limit error.
pub(crate) fn classify(text: &str) -> Option<Limit> {
    let t = text.to_ascii_lowercase();
    let weekly = [
        "weekly",
        "7-day",
        "7 day",
        "seven_day",
        "seven-day",
        "per week",
    ]
    .iter()
    .any(|n| t.contains(n));
    let five_hour = ["5-hour", "5 hour", "five_hour", "five-hour", "five hour"]
        .iter()
        .any(|n| t.contains(n))
        || has_word(&t, "5h");
    let names_limit = t.contains("limit") || t.contains("quota");
    let quota = [
        "usage limit",
        "usage_limit",
        "out of credits",
        "out of usage",
        "insufficient_quota",
        "insufficient quota",
        "exceeded your current quota",
        "exceeded your quota",
        "quota exceeded",
        "quota exhausted",
        "credit balance is too low",
        "billing_hard_limit",
    ]
    .iter()
    .any(|n| t.contains(n))
        || (t.contains("hit your") && t.contains("limit"))
        || (t.contains("resource_exhausted") && t.contains("quota"));
    let windowed = (weekly || five_hour) && names_limit;

    if quota || windowed {
        let code = if weekly {
            if ["opus", "sonnet", "haiku", "fable"]
                .iter()
                .any(|m| t.contains(m))
            {
                "usage_weekly_model"
            } else {
                "usage_weekly"
            }
        } else if five_hour {
            "usage_5h"
        } else {
            "usage_limit"
        };
        return Some(Limit { code });
    }

    let transient = [
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "overloaded",
        "throttl",
    ]
    .iter()
    .any(|n| t.contains(n))
        || has_word(&t, "429");
    transient.then_some(Limit { code: "rate_limit" })
}

/// The `AgentEvent::Error` for a limit error, or `None` for any other text.
pub(crate) fn limit_event(agent_name: &str, text: &str) -> Option<AgentEvent> {
    let limit = classify(text)?;
    let message = if limit.code == "rate_limit" {
        format!("{agent_name} is throttling this account, retry shortly")
    } else if text.to_ascii_lowercase().contains("usage limit") {
        text.to_owned()
    } else {
        format!("{agent_name} usage limit reached: {text}")
    };
    Some(AgentEvent::Error {
        message,
        code: Some(limit.code.into()),
        resets_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Option<&'static str> {
        classify(text).map(|l| l.code)
    }

    #[test]
    fn plan_windows_are_told_apart() {
        assert_eq!(
            code("You've hit your 5-hour limit. Resets at 6pm"),
            Some("usage_5h")
        );
        assert_eq!(code("Five Hour Limit Remaining is 0"), Some("usage_5h"));
        assert_eq!(
            code("Weekly limit reached, try again Monday"),
            Some("usage_weekly")
        );
        assert_eq!(
            code("You've hit your weekly Opus usage limit"),
            Some("usage_weekly_model")
        );
        assert_eq!(code("seven_day quota exceeded"), Some("usage_weekly"));
    }

    #[test]
    fn out_of_credits_without_a_window_is_a_usage_limit() {
        assert_eq!(
            code("Your workspace is out of credits."),
            Some("usage_limit")
        );
        assert_eq!(
            code(
                r#"429: {"error":{"type":"insufficient_quota","message":"You exceeded your current quota"}}"#
            ),
            Some("usage_limit"),
            "an OpenAI quota 429 is a usage limit, not a throttle"
        );
        assert_eq!(
            code("RESOURCE_EXHAUSTED: Quota exceeded for project"),
            Some("usage_limit")
        );
        assert_eq!(
            code("You have reached your usage limit"),
            Some("usage_limit")
        );
    }

    #[test]
    fn a_throttle_is_transient() {
        assert_eq!(
            code("Rate limit reached for requests per minute. Try again in 20s"),
            Some("rate_limit")
        );
        assert_eq!(code("HTTP 429 Too Many Requests"), Some("rate_limit"));
        assert_eq!(code("Overloaded"), Some("rate_limit"));
        assert_eq!(code("provider is throttling requests"), Some("rate_limit"));
    }

    #[test]
    fn ordinary_text_is_not_a_limit() {
        assert_eq!(code("cargo build failed with exit code 1"), None);
        assert_eq!(code("weekly report generated"), None);
        assert_eq!(code("request id 14290 failed"), None);
        assert_eq!(code("took 15h to finish"), None);
        assert_eq!(code("model returned an empty response"), None);
    }

    #[test]
    fn events_keep_what_the_quota_guard_needs() {
        let AgentEvent::Error {
            code,
            message,
            resets_at,
        } = limit_event("Pi", "429 Too Many Requests: rate limit exceeded").unwrap()
        else {
            panic!("an error event");
        };
        assert_eq!(code.as_deref(), Some("rate_limit"));
        assert_eq!(resets_at, None);
        // The throttle keeps none of the server's wording.
        let lower = message.to_ascii_lowercase();
        for guard in [
            "usage limit",
            "rate limit",
            "rate_limit",
            "quota exceeded",
            "limit reached",
            "too many requests",
        ] {
            assert!(!lower.contains(guard), "{guard} in {message}");
        }
        let AgentEvent::Error { code, message, .. } =
            limit_event("Pi", "You've hit your weekly limit").unwrap()
        else {
            panic!("an error event");
        };
        assert_eq!(code.as_deref(), Some("usage_weekly"));
        assert!(
            message.to_ascii_lowercase().contains("usage limit reached"),
            "{message}"
        );
        assert!(limit_event("Pi", "all good").is_none());
    }
}
