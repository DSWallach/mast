//! Picks the model for a task: the cheapest one the ledger, the quota and the
//! task's requirements allow, with an occasional canary probe below the floor.

use super::store::{ModelFitLedger, ModelKey};
use super::tier_below;
use crate::EngineError;
use mast_proto::{ModelTier, TaskClass};
use std::collections::BTreeSet;

/// Chance that an eligible selection probes one tier below the floor.
pub const CANARY_PROBABILITY: f64 = 0.10;
/// A probe needs the probed model's quota usage to be below this fraction of
/// its cap.
pub const CANARY_MAX_USAGE: f64 = 0.5;

/// Quota state per model. Implemented by whatever tracks provider quota.
pub trait Headroom {
    /// Quota left under the cap. A model is routable only when this is above
    /// zero; the unit is the implementation's.
    fn headroom(&self, model: &ModelKey) -> f64;
    /// Used fraction of the cap, `0.0..=1.0`, or `None` when unknown. Unknown
    /// usage never allows a canary probe.
    fn used_of_cap(&self, model: &ModelKey) -> Option<f64>;
}

pub trait Clock {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Source of the canary dice roll.
pub trait UniformRng {
    /// A value in `0.0..1.0`.
    fn next_unit(&mut self) -> f64;
}

/// SplitMix64: small, seedable and good enough for sampling. Not for secrets.
#[derive(Debug, Clone)]
pub struct SeededRng(u64);

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn from_entropy() -> Self {
        Self::new(uuid::Uuid::new_v4().as_u128() as u64)
    }
}

impl UniformRng for SeededRng {
    fn next_unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // The top 53 bits fill an f64 mantissa exactly.
        (z >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// A model the caller could launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub model: ModelKey,
    /// CLI or model version it would run; empty when unknown.
    pub version: String,
    pub tier: ModelTier,
    /// Relative price per token; only compared within a tier.
    pub token_cost: u64,
    pub capabilities: BTreeSet<String>,
    pub context_window_tokens: u32,
}

/// What is being routed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteRequest {
    pub task_class: TaskClass,
    /// Estimated size of the worker's context packet.
    pub packet_tokens: u32,
    pub required_capabilities: BTreeSet<String>,
    /// A critical task is never used as a canary.
    pub critical: bool,
    /// Raises the floor for this selection only, as when a worker is swapped
    /// for one a tier up ([`super::tier_above`]).
    pub min_tier: Option<ModelTier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub model: ModelKey,
    pub tier: ModelTier,
    /// A probe below the floor. Record its outcome with `canary` set.
    pub canary: bool,
    /// The task class's learned floor at selection time.
    pub floor: ModelTier,
}

/// Why a candidate could not take the task.
///
/// Anything under the floor is [`Rejection::BelowFloor`], whatever else is
/// true of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    BelowFloor,
    Demoted { until_ms: i64 },
    MissingCapability,
    NoHeadroom,
    ContextTooSmall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub model: ModelKey,
    pub reason: Rejection,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    /// Every candidate was rejected; each with the first reason that applied.
    #[error("no eligible model for this task")]
    NoEligibleModel(Vec<Rejected>),
    #[error(transparent)]
    Ledger(#[from] EngineError),
}

pub struct ModelRouter<C = SystemClock, R = SeededRng> {
    ledger: ModelFitLedger,
    clock: C,
    rng: R,
}

impl ModelRouter {
    pub fn new(ledger: ModelFitLedger) -> Self {
        Self::with_sources(ledger, SystemClock, SeededRng::from_entropy())
    }
}

impl<C: Clock, R: UniformRng> ModelRouter<C, R> {
    pub fn with_sources(ledger: ModelFitLedger, clock: C, rng: R) -> Self {
        Self { ledger, clock, rng }
    }

    pub fn ledger(&self) -> &ModelFitLedger {
        &self.ledger
    }

    /// The cheapest `(tier, token_cost)` candidate at or above the floor that
    /// is not demoted, has the capabilities and headroom, and whose context
    /// window holds the packet. Ties go to the lower [`ModelKey`], so input
    /// order never matters.
    ///
    /// When the floor is above the smallest tier, its cooldown is over and the
    /// task is not critical, one selection in ten goes instead to a candidate
    /// one tier below the floor that passes every other check and has used
    /// less than half its cap. The dice are only rolled when such a candidate
    /// exists.
    ///
    /// A candidate showing a new version has its ledger state reset first (see
    /// [`ModelFitLedger::observe_version`]).
    pub fn select(
        &mut self,
        request: &RouteRequest,
        candidates: &[Candidate],
        headroom: &dyn Headroom,
    ) -> Result<Selection, RouteError> {
        let now = self.clock.now_ms();
        let floor = self.ledger.floor(&request.task_class)?;
        let min_tier = floor.min_tier.max(request.min_tier.unwrap_or_default());
        let probe_tier = tier_below(floor.min_tier).filter(|tier| {
            !request.critical
                && now >= floor.cooldown_until_ms
                && request.min_tier.is_none_or(|required| *tier >= required)
        });

        let mut best: Option<&Candidate> = None;
        let mut probe: Option<&Candidate> = None;
        let mut rejected = Vec::new();
        for candidate in candidates {
            self.ledger
                .observe_version(&candidate.model, &candidate.version, now)?;
            if candidate.tier < min_tier {
                rejected.push(Rejected {
                    model: candidate.model.clone(),
                    reason: Rejection::BelowFloor,
                });
                if probe_tier == Some(candidate.tier)
                    && cheaper(candidate, probe)
                    && self.unfit(request, candidate, headroom, now)?.is_none()
                    && headroom
                        .used_of_cap(&candidate.model)
                        .is_some_and(|used| used < CANARY_MAX_USAGE)
                {
                    probe = Some(candidate);
                }
            } else if let Some(reason) = self.unfit(request, candidate, headroom, now)? {
                rejected.push(Rejected {
                    model: candidate.model.clone(),
                    reason,
                });
            } else if cheaper(candidate, best) {
                best = Some(candidate);
            }
        }

        let Some(best) = best else {
            return Err(RouteError::NoEligibleModel(rejected));
        };
        let (chosen, canary) = match probe {
            Some(probe) if self.rng.next_unit() < CANARY_PROBABILITY => (probe, true),
            _ => (best, false),
        };
        Ok(Selection {
            model: chosen.model.clone(),
            tier: chosen.tier,
            canary,
            floor: floor.min_tier,
        })
    }

    /// The first reason other than the floor that rules a candidate out.
    fn unfit(
        &self,
        request: &RouteRequest,
        candidate: &Candidate,
        headroom: &dyn Headroom,
        now: i64,
    ) -> Result<Option<Rejection>, EngineError> {
        if let Some(fit) = self
            .ledger
            .fit(&request.task_class, &candidate.model, now)?
            && fit.is_demoted(now)
        {
            return Ok(Some(Rejection::Demoted {
                until_ms: fit.demoted_until_ms,
            }));
        }
        Ok(
            if !request
                .required_capabilities
                .is_subset(&candidate.capabilities)
            {
                Some(Rejection::MissingCapability)
            } else if !positive(headroom.headroom(&candidate.model)) {
                Some(Rejection::NoHeadroom)
            } else if request.packet_tokens > candidate.context_window_tokens {
                Some(Rejection::ContextTooSmall)
            } else {
                None
            },
        )
    }
}

/// False for zero, negative and NaN.
fn positive(headroom: f64) -> bool {
    headroom > 0.0
}

fn cheaper(candidate: &Candidate, than: Option<&Candidate>) -> bool {
    than.is_none_or(|other| {
        (candidate.tier, candidate.token_cost, &candidate.model)
            < (other.tier, other.token_cost, &other.model)
    })
}
