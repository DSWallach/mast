//! The approval ledger: one record per approval request, with its outcome and
//! who decided it. Friction rows stay the deduped summary; the ledger is the
//! per-request history.

use serde::{Deserialize, Serialize};

/// Rows kept per Space (oldest dropped first).
pub const MAX_APPROVAL_LEDGER_ROWS_PER_SPACE: usize = 5_000;
/// Rows older than this are dropped, in days.
pub const APPROVAL_LEDGER_RETENTION_DAYS: i64 = 180;
/// Longest command stored, after redaction.
pub const MAX_LEDGER_COMMAND_CHARS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerDecision {
    Pending,
    Allowed,
    Denied,
    Cancelled,
}

impl LedgerDecision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "waiting for an answer",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "allowed" => Some(Self::Allowed),
            "denied" => Some(Self::Denied),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerDecider {
    User,
    UserRule,
    SpaceRule,
    Auto,
    Teardown,
}

impl LedgerDecider {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "you",
            Self::UserRule => "your rule",
            Self::SpaceRule => "Space rule",
            Self::Auto => "auto",
            Self::Teardown => "run ended",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::UserRule => "user_rule",
            Self::SpaceRule => "space_rule",
            Self::Auto => "auto",
            Self::Teardown => "teardown",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "user_rule" => Some(Self::UserRule),
            "space_rule" => Some(Self::SpaceRule),
            "auto" => Some(Self::Auto),
            "teardown" => Some(Self::Teardown),
            _ => None,
        }
    }
}

/// Where the request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerSource {
    /// The harness asked.
    Prompt,
    /// The `run_command` agent tool.
    RunCommand,
}

impl LedgerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::RunCommand => "run_command",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prompt" => Some(Self::Prompt),
            "run_command" => Some(Self::RunCommand),
            _ => None,
        }
    }
}

/// One device-local approval history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalLedgerEntry {
    /// Row id, derived from the chat and request ids.
    pub id: String,
    pub space_id: String,
    pub chat_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Redacted, capped command text.
    pub command: String,
    /// The command was redacted or cut, so it must never seed a rule.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub source: LedgerSource,
    pub requested_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    pub decision: LedgerDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<LedgerDecider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
}

impl ApprovalLedgerEntry {
    pub fn matches_query(&self, query: &str) -> bool {
        let fields = [
            self.command.as_str(),
            self.harness.as_str(),
            self.rule_id.as_deref().unwrap_or(""),
            self.decision.label(),
            self.decider.map_or("not decided", LedgerDecider::label),
        ];
        let searchable = fields.map(str::to_lowercase);
        query.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            searchable.iter().any(|field| field.contains(&word))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListApprovalLedgerParams {
    pub space_id: String,
    /// Case-insensitive text over command, harness, rule, outcome and decider.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub decision: Option<LedgerDecision>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
}

/// Rows newest first, plus how many matched before the limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListApprovalLedgerResult {
    #[serde(default)]
    pub entries: Vec<ApprovalLedgerEntry>,
    #[serde(default)]
    pub total: usize,
}
