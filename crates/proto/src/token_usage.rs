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
    pub space_id: Option<String>,
    pub work_item_id: Option<String>,
    pub logical_turn: Option<String>,
    pub harness: Option<String>,
    pub reasoning: Option<String>,
    pub configured_model: Option<String>,
    pub build: Option<String>,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageContextSample {
    pub at: i64,
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    pub role: String,
}
