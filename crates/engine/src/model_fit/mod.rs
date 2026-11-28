//! Minimal-fit model selection with a persistent record of which models fail at
//! which kinds of task (design plan v3 §6.1 and §6.2). See
//! `docs/internal/model-fit.md`.
//!
//! - [`classify_domain`] derives a task's domain from the paths it owns or changed.
//! - [`FailureKind`] and [`attribute`] decide whether a result says anything
//!   about the model: infrastructure failures never do, and a failure in an
//!   overfull context is blamed on the brief.
//! - [`ModelFitLedger`] stores outcomes, demotions and per-task-class tier
//!   floors in `<profile_store>/model-fit.sqlite3`.
//! - [`ModelRouter`] picks the cheapest model that is allowed to take a task.
//!
//! The task and tier types are the wire types in [`mast_proto`]:
//! [`ModelTier::Small`], [`ModelTier::Medium`] and [`ModelTier::Large`] are the
//! design's Tier 1, 2 and 3. Nothing here is wired into launch or `swap_worker`
//! yet, and floors are not synced between devices.

mod classify;
mod failure;
mod router;
mod store;
#[cfg(test)]
mod tests;

pub use classify::{classify_domain, task_class};
pub use failure::{
    Attribution, FailureKind, TaskResult, attribute, context_ceiling, context_overloaded,
};
pub use router::{
    CANARY_MAX_USAGE, CANARY_PROBABILITY, Candidate, Clock, Headroom, ModelRouter, Rejected,
    Rejection, RouteError, RouteRequest, SeededRng, Selection, SystemClock, UniformRng,
};
pub use store::{
    CANARY_PASSES_TO_DEESCALATE, DEMOTE_CONSECUTIVE, DEMOTE_FAIL_RATE, DEMOTE_MIN_TRIALS,
    DEMOTION_BASE_MS, DEMOTION_MAX_MS, EWMA_ALPHA, FitState, FloorState, HALF_LIFE_MS,
    ModelFitLedger, ModelKey, Outcome, Recorded,
};

pub use mast_proto::{ComplexityTier, ModelTier, TaskClass, TaskDomain, TaskRole};

/// The tier above `tier`; [`ModelTier::Large`] is the highest.
pub fn tier_above(tier: ModelTier) -> ModelTier {
    match tier {
        ModelTier::Small => ModelTier::Medium,
        ModelTier::Medium | ModelTier::Large => ModelTier::Large,
    }
}

/// The tier below `tier`, if there is one.
pub fn tier_below(tier: ModelTier) -> Option<ModelTier> {
    match tier {
        ModelTier::Small => None,
        ModelTier::Medium => Some(ModelTier::Small),
        ModelTier::Large => Some(ModelTier::Medium),
    }
}

fn fail(error: impl std::fmt::Display) -> crate::EngineError {
    crate::EngineError::Other(format!("Model fit: {error}"))
}
