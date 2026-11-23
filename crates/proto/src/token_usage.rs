//! Device-local token accounting and optimization diagnostics. Diagnostic estimates
//! describe observed content, never additional provider-billed usage.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
}

impl TokenCounts {
    /// Input/output are inclusive totals after adapter normalization.
    pub fn selected(&self, kind: TokenKind) -> u64 {
        match kind {
            TokenKind::All => self.input.saturating_add(self.output),
            TokenKind::Input => self.input,
            TokenKind::Output => self.output,
            TokenKind::UncachedInput => self
                .cache_read
                .zip(self.cache_write)
                .map(|(r, w)| self.input.saturating_sub(r.saturating_add(w)))
                .unwrap_or(0),
            TokenKind::CacheRead => self.cache_read.unwrap_or(0),
            TokenKind::CacheWrite => self.cache_write.unwrap_or(0),
            TokenKind::Reasoning => self.reasoning.unwrap_or(0),
            TokenKind::NonReasoningOutput => self
                .reasoning
                .map(|r| self.output.saturating_sub(r))
                .unwrap_or(0),
        }
    }
    pub fn known(&self, kind: TokenKind) -> bool {
        match kind {
            TokenKind::UncachedInput => self.cache_read.is_some() && self.cache_write.is_some(),
            TokenKind::CacheRead => self.cache_read.is_some(),
            TokenKind::CacheWrite => self.cache_write.is_some(),
            TokenKind::Reasoning | TokenKind::NonReasoningOutput => self.reasoning.is_some(),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenKind {
    #[default]
    All,
    Input,
    Output,
    UncachedInput,
    CacheRead,
    CacheWrite,
    Reasoning,
    NonReasoningOutput,
}
impl TokenKind {
    pub const ALL: [Self; 8] = [
        Self::All,
        Self::Input,
        Self::Output,
        Self::UncachedInput,
        Self::CacheRead,
        Self::CacheWrite,
        Self::Reasoning,
        Self::NonReasoningOutput,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All tokens",
            Self::Input => "Input",
            Self::Output => "Output",
            Self::UncachedInput => "Uncached input",
            Self::CacheRead => "Cache read",
            Self::CacheWrite => "Cache write",
            Self::Reasoning => "Reasoning",
            Self::NonReasoningOutput => "Output excluding reasoning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMeasurement {
    /// Stable provider request id, or an adapter-generated turn result id.
    pub id: String,
    pub model: Option<String>,
    pub counts: TokenCounts,
    /// `request`, `turn`, `turnIncludingChildren`, or excluded `resumeBaseline`. Aggregates supersede
    /// request measurements for the same turn/model in billing totals.
    pub scope: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDiagnostic {
    pub id: String,
    pub category: String,
    pub label: String,
    pub revision: Option<String>,
    pub channel: Option<String>,
    pub bytes: u64,
    /// What was observed, e.g. `harnessResponse`, `injected`, `read`.
    pub stage: String,
    /// `bytesOnly`: no tokenizer assumption or billed-token attribution.
    pub method: String,
    pub is_error: Option<bool>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageQuery {
    pub model: Option<String>,
    #[serde(default)]
    pub token_kind: TokenKind,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub activity: Option<String>,
    /// Requires token-usage-insights-v1. Legacy hosts ignore this field;
    /// callers must verify response support before presenting filtered totals.
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageGroup {
    pub label: String,
    pub tokens: u64,
    pub runs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRun {
    pub id: String,
    pub chat_id: String,
    pub model: String,
    pub activity: String,
    pub role: String,
    #[serde(default)]
    pub agent_type: String,
    pub at: i64,
    pub tokens: u64,
    pub status: String,
    pub configuration: String,
    #[serde(default)]
    pub context: UsageContext,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageContext {
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub parent_chat_id: Option<String>,
    pub space_id: Option<String>,
    pub work_item_id: Option<String>,
    pub logical_turn: Option<String>,
    pub harness: Option<String>,
    pub reasoning: Option<String>,
    pub configured_model: Option<String>,
    pub build: Option<String>,
    #[serde(default)]
    pub build_revision: Option<String>,
    /// `fresh`, `resumed` (native session resumed by id) or `continued`
    /// (a later turn in the same harness process). Unrecorded on older rows.
    #[serde(default)]
    pub session_start: Option<String>,
    /// Zero-based turn ordinal within one harness process.
    #[serde(default)]
    pub run_turn: Option<u64>,
    #[serde(default)]
    pub chat_created_at: Option<i64>,
    /// Name of the scheduled task whose run owns this chat. Every turn of the
    /// chat carries it, including later follow-ups; unrecorded on older rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task: Option<String>,
    /// What started the turn: `Human`, `Other chat`, `Admin chat`,
    /// `Hand-off`, or a system label such as `Worker updates`. Unrecorded on
    /// older rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

/// Per-turn accounting with the same supersession rules as the totals.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnProfile {
    pub turn_id: String,
    pub chat_id: String,
    pub at: i64,
    pub status: String,
    pub activity: String,
    pub agent_type: String,
    pub harness: Option<String>,
    pub models: Vec<String>,
    pub session_start: Option<String>,
    pub chat_age_ms: Option<i64>,
    /// Inclusive input; cache fields are subsets, `None` when unreported for
    /// any accounted measurement in the turn.
    pub counts: TokenCounts,
    /// `turn`, `turnIncludingChildren`, `request` (provisional sum) or `legacy`.
    pub scope: String,
    /// Provider-reported main-agent requests; `None` when not reported.
    pub requests: Option<u64>,
    pub subagent_requests: Option<u64>,
    pub peak_context: Option<u64>,
    pub tool_calls: u64,
    /// Raw harness responses where captured, else retained transcript results.
    pub tool_result_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnGroup {
    pub label: String,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    /// Input of turns that reported cache reads; the cache-share denominator.
    pub cache_known_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_known_turns: u64,
    pub request_known_input: u64,
    pub requests: u64,
    pub request_known_turns: u64,
    pub median_input: u64,
    pub p90_input: u64,
    pub peak_context_max: u64,
    pub peak_context_known_turns: u64,
    pub tool_result_bytes: u64,
    pub example_chat_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessCoverage {
    pub harness: String,
    pub turns: u64,
    pub cache_read_turns: u64,
    pub cache_write_turns: u64,
    pub reasoning_turns: u64,
    pub request_count_turns: u64,
    pub legacy_only_turns: u64,
    pub session_start_turns: u64,
    pub semantics: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAnalysis {
    /// Largest input first, at most 50.
    pub turns: Vec<TurnProfile>,
    /// Harness · agent type · session start.
    pub workflows: Vec<TurnGroup>,
    pub chat_ages: Vec<TurnGroup>,
    pub harnesses: Vec<HarnessCoverage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticGroup {
    pub category: String,
    pub label: String,
    pub revision: Option<String>,
    pub channel: Option<String>,
    pub stage: String,
    pub method: String,
    pub bytes: u64,
    pub observations: u64,
    pub errors: u64,
    pub example_chat_id: String,
    #[serde(default)]
    pub repeated: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageSnapshot {
    /// False on legacy responses, including a host that ignored agent_type.
    #[serde(default)]
    pub insights_available: bool,
    pub total: u64,
    pub runs: u64,
    pub unknown_breakdown: u64,
    pub models: Vec<String>,
    pub activities: Vec<UsageGroup>,
    pub roles: Vec<UsageGroup>,
    pub days: Vec<UsageGroup>,
    pub details: Vec<UsageRun>,
    pub has_more: bool,
    pub diagnostics: Vec<DiagnosticGroup>,
    pub coverage: Vec<String>,
    pub recording_since: Option<i64>,
    pub updated_at: i64,
    #[serde(default)]
    pub configurations: Vec<ConfigurationUsage>,
    #[serde(default)]
    pub context_samples: Vec<UsageContextSample>,
    #[serde(default)]
    pub available_agent_types: Vec<String>,
    #[serde(default)]
    pub agent_types: Vec<AgentUsageGroup>,
    /// Child requests are a partial breakdown, not extra billed usage.
    #[serde(default)]
    pub observed_agents: Vec<AgentUsageGroup>,
    #[serde(default)]
    pub efficiency: EfficiencySummary,
    #[serde(default)]
    pub insights: Vec<EfficiencyInsight>,
    /// `None` from hosts that predate per-turn analysis.
    #[serde(default)]
    pub turn_analysis: Option<TurnAnalysis>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationUsage {
    pub fingerprint: String,
    pub model: String,
    pub activity: String,
    pub tokens: u64,
    pub completed: u64,
    pub failed: u64,
    pub partial: u64,
    pub completed_tokens: u64,
    pub runs: u64,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub failed_tokens: u64,
    #[serde(default)]
    pub build_revision: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageGroup {
    #[serde(default)]
    pub unknown_breakdown: u64,
    pub agent_type: String,
    pub role: String,
    pub tokens: u64,
    pub turns: u64,
    pub observations: u64,
    pub example_chat_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EfficiencySummary {
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub partial: u64,
    pub completed_tokens: u64,
    pub failed_tokens: u64,
    pub partial_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EfficiencyInsight {
    pub category: String,
    pub label: String,
    pub channel: Option<String>,
    pub stage: String,
    pub agent_type: String,
    pub occurrences: u64,
    pub repeated: u64,
    pub bytes: u64,
    pub repeated_bytes: u64,
    pub errors: u64,
    pub empty_results: u64,
    pub example_chat_id: String,
    pub example_turn_id: String,
    pub example_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageContextSample {
    pub at: i64,
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    pub role: String,
}
