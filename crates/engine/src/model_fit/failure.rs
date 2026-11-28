//! What a task result says about the model that produced it.

use mast_proto::ModelTier;

/// Why a task attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    GateCompileOrTestFail,
    ContractOrScopeViolation,
    LoopOrBudgetThrash,
    /// A reviewer rejected the work on substance. One rejection is ordinary
    /// review; the second and later ones on the same task count.
    SubstantiveReviewReject,
    /// The parent swapped the worker out as inadequate for the task.
    ParentQualityEscalation,
    QuotaOrRateLimit,
    AuthExpiry,
    DiskHold,
    PeerDisconnect,
}

impl FailureKind {
    /// Whether this kind can ever count against a model. Infrastructure
    /// failures cannot.
    pub fn counts_against_model(self) -> bool {
        match self {
            Self::GateCompileOrTestFail
            | Self::ContractOrScopeViolation
            | Self::LoopOrBudgetThrash
            | Self::SubstantiveReviewReject
            | Self::ParentQualityEscalation => true,
            Self::QuotaOrRateLimit | Self::AuthExpiry | Self::DiskHold | Self::PeerDisconnect => {
                false
            }
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::GateCompileOrTestFail => "gate_compile_or_test_fail",
            Self::ContractOrScopeViolation => "contract_or_scope_violation",
            Self::LoopOrBudgetThrash => "loop_or_budget_thrash",
            Self::SubstantiveReviewReject => "substantive_review_reject",
            Self::ParentQualityEscalation => "parent_quality_escalation",
            Self::QuotaOrRateLimit => "quota_or_rate_limit",
            Self::AuthExpiry => "auth_expiry",
            Self::DiskHold => "disk_hold",
            Self::PeerDisconnect => "peer_disconnect",
        }
    }
}

/// How a task attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskResult {
    CleanPass,
    Failed(FailureKind),
}

/// How the ledger treats a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Success,
    /// Counts against the model.
    ReasoningInadequacy,
    /// The context was overfull: split the ticket or tighten its slice. Does
    /// not count against the model.
    ContextOverload,
    /// Infrastructure failure. Does not count against the model.
    Excluded,
    /// A task's first review rejection. Does not count against the model.
    BelowThreshold,
}

impl Attribution {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ReasoningInadequacy => "reasoning_inadequacy",
            Self::ContextOverload => "context_overload",
            Self::Excluded => "excluded",
            Self::BelowThreshold => "below_threshold",
        }
    }
}

/// Peak context occupancy above which a failure is blamed on the context
/// rather than the model. Matches the tier's forced session-swap point
/// ([`crate::context_slice::PacketTier::hard_swap_fraction`]).
pub fn context_ceiling(tier: ModelTier) -> f32 {
    use crate::context_slice::PacketTier;
    match tier {
        ModelTier::Small => PacketTier::Tier1,
        ModelTier::Medium => PacketTier::Tier2,
        ModelTier::Large => PacketTier::Tier3,
    }
    .hard_swap_fraction()
}

/// Whether the peak occupancy went over the tier's ceiling. Unknown occupancy
/// or an unknown window is not an overload.
pub fn context_overloaded(
    tier: ModelTier,
    peak_tokens: Option<u64>,
    window_tokens: Option<u64>,
) -> bool {
    match (peak_tokens, window_tokens) {
        (Some(peak), Some(window)) if window > 0 => {
            peak as f64 / window as f64 > f64::from(context_ceiling(tier))
        }
        _ => false,
    }
}

/// Attribute a result. `earlier_review_rejects` is how many times this model's
/// work on this task was already rejected in review.
pub fn attribute(
    result: TaskResult,
    tier: ModelTier,
    peak_tokens: Option<u64>,
    window_tokens: Option<u64>,
    earlier_review_rejects: u64,
) -> Attribution {
    let TaskResult::Failed(kind) = result else {
        return Attribution::Success;
    };
    if !kind.counts_against_model() {
        Attribution::Excluded
    } else if kind == FailureKind::SubstantiveReviewReject && earlier_review_rejects == 0 {
        Attribution::BelowThreshold
    } else if context_overloaded(tier, peak_tokens, window_tokens) {
        Attribution::ContextOverload
    } else {
        Attribution::ReasoningInadequacy
    }
}
