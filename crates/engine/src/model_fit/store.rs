//! `<profile_store>/model-fit.sqlite3`: the append-only `model_outcomes` log,
//! the EWMA `model_fit_aggregates` per task class and model, and the per-class
//! tier floors with their canary progress.
//!
//! Every time is a caller-supplied Unix millisecond, so the ledger has no clock
//! of its own. Calls block on SQLite.

use super::failure::{Attribution, FailureKind, TaskResult, attribute};
use super::{fail, tier_above, tier_below};
use crate::EngineError;
use mast_proto::{ModelTier, TaskClass};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The recorded failure rate halves over this long without new evidence.
pub const HALF_LIFE_MS: i64 = 14 * DAY_MS;
/// First demotion; each repeat doubles it.
pub const DEMOTION_BASE_MS: i64 = 7 * DAY_MS;
/// Longest demotion.
pub const DEMOTION_MAX_MS: i64 = 30 * DAY_MS;
/// This many counted failures in a row demote.
pub const DEMOTE_CONSECUTIVE: u32 = 2;
/// With at least this many trials, a high failure rate demotes.
pub const DEMOTE_MIN_TRIALS: u32 = 3;
pub const DEMOTE_FAIL_RATE: f64 = 0.45;
/// Weight of the newest trial. The first few trials use `1 / trials` where that
/// is larger, which makes the rate a plain average until there is history.
pub const EWMA_ALPHA: f64 = 0.3;
/// Demoted models at one tier that raise a task class's floor above it.
const DEMOTED_MODELS_TO_RAISE_FLOOR: u32 = 2;
/// Clean canary passes in a row that lower a floor by one tier.
pub const CANARY_PASSES_TO_DEESCALATE: u32 = 2;

const SCHEMA_VERSION: i64 = 1;

/// A model as launched: the harness and its model id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelKey {
    pub harness: String,
    pub model: String,
}

impl ModelKey {
    pub fn new(harness: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            harness: harness.into(),
            model: model.into(),
        }
    }
}

/// One finished task attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Unique per observation; recording the same id again changes nothing.
    pub outcome_id: String,
    /// The Work Item (or other task) attempted.
    pub task_id: String,
    pub task_class: TaskClass,
    pub model: ModelKey,
    /// CLI or model version that ran it; empty when unknown.
    pub version: String,
    pub tier: ModelTier,
    pub result: TaskResult,
    /// The attempt was a canary probe one tier below the floor.
    pub canary: bool,
    pub peak_context_tokens: Option<u64>,
    pub context_window_tokens: Option<u64>,
}

/// What recording an outcome did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    pub attribution: Attribution,
    /// The outcome id was already recorded; nothing changed.
    pub duplicate: bool,
    /// Set when this outcome demoted the model for this task class.
    pub demoted_until_ms: Option<i64>,
    /// Set when this outcome moved the task class's floor, to its new value.
    pub floor_moved_to: Option<ModelTier>,
}

/// The learned state of one model on one task class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitState {
    pub tier: ModelTier,
    /// Counted trials (passes and counted failures) since the last demotion.
    pub trials: u32,
    /// Failure rate decayed to the time of the query.
    pub ewma_fail_rate: f64,
    pub consecutive_failures: u32,
    /// How many demotions the next one doubles over.
    pub demotion_level: u32,
    pub demoted_until_ms: i64,
}

impl FitState {
    fn new(tier: ModelTier) -> Self {
        Self {
            tier,
            trials: 0,
            ewma_fail_rate: 0.0,
            consecutive_failures: 0,
            demotion_level: 0,
            demoted_until_ms: 0,
        }
    }

    pub fn is_demoted(&self, now_ms: i64) -> bool {
        self.demoted_until_ms > now_ms
    }
}

/// The lowest tier a task class may be routed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FloorState {
    pub min_tier: ModelTier,
    /// No canary probes below the floor before this time.
    pub cooldown_until_ms: i64,
    pub canary_passes: u32,
}

/// Handle to the ledger. Clones share one connection.
#[derive(Clone)]
pub struct ModelFitLedger(Arc<Mutex<Connection>>);

impl ModelFitLedger {
    /// Open or create `model-fit.sqlite3` under the profile store `root`.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        let mut db = Connection::open(root.join("model-fit.sqlite3")).map_err(fail)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(fail)?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(fail)?;
        if version > SCHEMA_VERSION {
            return Err(fail("ledger was written by a newer Mast version"));
        }
        db.pragma_update(None, "journal_mode", "WAL")
            .map_err(fail)?;
        db.pragma_update(None, "synchronous", "NORMAL")
            .map_err(fail)?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(fail)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS model_outcomes(
               seq INTEGER PRIMARY KEY AUTOINCREMENT, outcome_id TEXT NOT NULL UNIQUE, at INTEGER NOT NULL,
               task_id TEXT NOT NULL, task_class TEXT NOT NULL, harness TEXT NOT NULL, model TEXT NOT NULL,
               version TEXT NOT NULL, tier INTEGER NOT NULL, result TEXT NOT NULL, attribution TEXT NOT NULL,
               canary INTEGER NOT NULL, peak_context_tokens INTEGER, context_window_tokens INTEGER);
             CREATE INDEX IF NOT EXISTS model_outcomes_task ON model_outcomes(task_id, task_class, harness, model);
             CREATE TABLE IF NOT EXISTS model_fit_aggregates(
               task_class TEXT NOT NULL, harness TEXT NOT NULL, model TEXT NOT NULL, tier INTEGER NOT NULL,
               trials INTEGER NOT NULL, ewma_fail_rate REAL NOT NULL, consecutive_failures INTEGER NOT NULL,
               demotion_level INTEGER NOT NULL, demoted_until_ms INTEGER NOT NULL, updated_at INTEGER NOT NULL,
               PRIMARY KEY(task_class, harness, model));
             CREATE TABLE IF NOT EXISTS task_class_floors(
               task_class TEXT PRIMARY KEY, min_tier INTEGER NOT NULL,
               cooldown_until_ms INTEGER NOT NULL, canary_passes INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS model_versions(
               harness TEXT NOT NULL, model TEXT NOT NULL, version TEXT NOT NULL, first_seen INTEGER NOT NULL,
               PRIMARY KEY(harness, model, version));
             PRAGMA user_version=1;",
        )
        .map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(Self(Arc::new(Mutex::new(db))))
    }

    fn write<T>(
        &self,
        work: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Result<T, EngineError> {
        let mut db = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(fail)?;
        let value = work(&tx).map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(value)
    }

    fn read<T>(
        &self,
        work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, EngineError> {
        let db = self.0.lock().unwrap_or_else(|p| p.into_inner());
        work(&db).map_err(fail)
    }

    /// Append an outcome and apply what it teaches: the model's failure rate,
    /// demotion, the task class's floor and canary progress. All of it commits
    /// together.
    pub fn record(&self, outcome: &Outcome, now_ms: i64) -> Result<Recorded, EngineError> {
        let class = class_key(&outcome.task_class)?;
        self.write(|tx| record_in(tx, &class, outcome, now_ms))
    }

    /// Note the version a model is running. The first time a known model shows a
    /// version it never ran before, its evidence and any remaining demotion are
    /// halved on every task class. Returns whether that happened.
    pub fn observe_version(
        &self,
        model: &ModelKey,
        version: &str,
        now_ms: i64,
    ) -> Result<bool, EngineError> {
        self.write(|tx| observe_version_in(tx, model, version, now_ms))
    }

    /// The model's state on a task class, or `None` with no counted evidence.
    pub fn fit(
        &self,
        task_class: &TaskClass,
        model: &ModelKey,
        now_ms: i64,
    ) -> Result<Option<FitState>, EngineError> {
        let class = class_key(task_class)?;
        self.read(|db| {
            Ok(load_fit(db, &class, model)?
                .map(|(state, updated_at)| decayed(state, updated_at, now_ms)))
        })
    }

    pub fn floor(&self, task_class: &TaskClass) -> Result<FloorState, EngineError> {
        let class = class_key(task_class)?;
        self.read(|db| load_floor(db, &class))
    }

    #[cfg(test)]
    pub(super) fn outcome_count(&self) -> Result<u64, EngineError> {
        self.read(|db| {
            db.query_row("SELECT COUNT(*) FROM model_outcomes", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|count| count as u64)
        })
    }
}

fn record_in(
    tx: &Transaction<'_>,
    class: &str,
    outcome: &Outcome,
    now_ms: i64,
) -> rusqlite::Result<Recorded> {
    let mut recorded = Recorded {
        attribution: Attribution::Success,
        duplicate: false,
        demoted_until_ms: None,
        floor_moved_to: None,
    };
    let existing: Option<String> = tx
        .query_row(
            "SELECT attribution FROM model_outcomes WHERE outcome_id=?1",
            [&outcome.outcome_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(attribution) = existing {
        recorded.attribution = parse_attribution(&attribution);
        recorded.duplicate = true;
        return Ok(recorded);
    }
    let model = &outcome.model;
    observe_version_in(tx, model, &outcome.version, now_ms)?;

    let result = match outcome.result {
        TaskResult::CleanPass => "clean_pass",
        TaskResult::Failed(kind) => kind.as_str(),
    };
    let earlier_review_rejects: i64 = tx.query_row(
        "SELECT COUNT(*) FROM model_outcomes
         WHERE task_id=?1 AND task_class=?2 AND harness=?3 AND model=?4 AND result=?5",
        params![
            outcome.task_id,
            class,
            model.harness,
            model.model,
            FailureKind::SubstantiveReviewReject.as_str()
        ],
        |r| r.get(0),
    )?;
    recorded.attribution = attribute(
        outcome.result,
        outcome.tier,
        outcome.peak_context_tokens,
        outcome.context_window_tokens,
        earlier_review_rejects as u64,
    );
    tx.execute(
        "INSERT INTO model_outcomes(outcome_id,at,task_id,task_class,harness,model,version,tier,result,
           attribution,canary,peak_context_tokens,context_window_tokens)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            outcome.outcome_id,
            now_ms,
            outcome.task_id,
            class,
            model.harness,
            model.model,
            outcome.version,
            tier_number(outcome.tier),
            result,
            recorded.attribution.as_str(),
            outcome.canary,
            outcome.peak_context_tokens.map(sql_count),
            outcome.context_window_tokens.map(sql_count),
        ],
    )?;

    let failed = match recorded.attribution {
        Attribution::Success => false,
        Attribution::ReasoningInadequacy => true,
        Attribution::ContextOverload | Attribution::Excluded | Attribution::BelowThreshold => {
            return Ok(recorded);
        }
    };

    let mut state = load_fit(tx, class, model)?
        .map(|(state, updated_at)| decayed(state, updated_at, now_ms))
        .unwrap_or_else(|| FitState::new(outcome.tier));
    state.tier = outcome.tier;
    state.trials = state.trials.saturating_add(1);
    let alpha = EWMA_ALPHA.max(1.0 / f64::from(state.trials));
    let sample = if failed { 1.0 } else { 0.0 };
    state.ewma_fail_rate += alpha * (sample - state.ewma_fail_rate);
    state.consecutive_failures = if failed {
        state.consecutive_failures.saturating_add(1)
    } else {
        0
    };

    let demote = failed
        && !state.is_demoted(now_ms)
        && (state.consecutive_failures >= DEMOTE_CONSECUTIVE
            || (state.trials >= DEMOTE_MIN_TRIALS && state.ewma_fail_rate >= DEMOTE_FAIL_RATE));
    if demote {
        state.demoted_until_ms = now_ms.saturating_add(demotion_ms(state.demotion_level));
        state.demotion_level = state.demotion_level.saturating_add(1);
        // The next demotion needs fresh evidence.
        state.trials = 0;
        state.ewma_fail_rate = 0.0;
        state.consecutive_failures = 0;
        recorded.demoted_until_ms = Some(state.demoted_until_ms);
    }
    save_fit(tx, class, model, &state, now_ms)?;

    if demote {
        recorded.floor_moved_to = raise_floor(tx, class, state.tier, now_ms)?;
    }
    if outcome.canary
        && let Some(lowered) = apply_canary(tx, class, outcome.tier, failed)?
    {
        recorded.floor_moved_to = Some(lowered);
    }
    Ok(recorded)
}

/// With enough demoted models at `tier`, put the class's floor above it.
/// Returns the new floor if it moved.
fn raise_floor(
    tx: &Transaction<'_>,
    class: &str,
    tier: ModelTier,
    now_ms: i64,
) -> rusqlite::Result<Option<ModelTier>> {
    let above = tier_above(tier);
    if above == tier {
        return Ok(None);
    }
    let (demoted, latest): (u32, Option<i64>) = tx.query_row(
        "SELECT COUNT(*), MAX(demoted_until_ms) FROM model_fit_aggregates
         WHERE task_class=?1 AND tier=?2 AND demoted_until_ms>?3",
        params![class, tier_number(tier), now_ms],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut floor = load_floor(tx, class)?;
    if demoted < DEMOTED_MODELS_TO_RAISE_FLOOR || above < floor.min_tier {
        return Ok(None);
    }
    let moved = above > floor.min_tier;
    floor.min_tier = above;
    floor.cooldown_until_ms = floor.cooldown_until_ms.max(latest.unwrap_or(now_ms));
    floor.canary_passes = 0;
    save_floor(tx, class, &floor)?;
    Ok(moved.then_some(above))
}

/// Count a canary result from the tier just below the floor. Returns the new
/// floor if it was lowered.
fn apply_canary(
    tx: &Transaction<'_>,
    class: &str,
    tier: ModelTier,
    failed: bool,
) -> rusqlite::Result<Option<ModelTier>> {
    let mut floor = load_floor(tx, class)?;
    if tier_below(floor.min_tier) != Some(tier) {
        return Ok(None);
    }
    let mut lowered = None;
    if failed {
        floor.canary_passes = 0;
    } else {
        floor.canary_passes += 1;
        if floor.canary_passes >= CANARY_PASSES_TO_DEESCALATE {
            floor.min_tier = tier;
            floor.canary_passes = 0;
            lowered = Some(tier);
        }
    }
    save_floor(tx, class, &floor)?;
    Ok(lowered)
}

fn observe_version_in(
    tx: &Transaction<'_>,
    model: &ModelKey,
    version: &str,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    if version.is_empty() {
        return Ok(false);
    }
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO model_versions(harness,model,version,first_seen) VALUES(?1,?2,?3,?4)",
        params![model.harness, model.model, version, now_ms],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    let versions: i64 = tx.query_row(
        "SELECT COUNT(*) FROM model_versions WHERE harness=?1 AND model=?2",
        params![model.harness, model.model],
        |r| r.get(0),
    )?;
    if versions < 2 {
        return Ok(false);
    }
    let classes: Vec<String> = tx
        .prepare("SELECT task_class FROM model_fit_aggregates WHERE harness=?1 AND model=?2")?
        .query_map(params![model.harness, model.model], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for class in classes {
        let Some((state, updated_at)) = load_fit(tx, &class, model)? else {
            continue;
        };
        let mut state = decayed(state, updated_at, now_ms);
        state.trials /= 2;
        state.ewma_fail_rate *= 0.5;
        state.consecutive_failures = 0;
        state.demotion_level /= 2;
        if state.is_demoted(now_ms) {
            state.demoted_until_ms = now_ms + (state.demoted_until_ms - now_ms) / 2;
        }
        save_fit(tx, &class, model, &state, now_ms)?;
    }
    Ok(true)
}

fn demotion_ms(level: u32) -> i64 {
    DEMOTION_BASE_MS
        .saturating_mul(1_i64 << level.min(16))
        .min(DEMOTION_MAX_MS)
}

/// Apply the half-life to the failure rate stored at `updated_at`.
fn decayed(mut state: FitState, updated_at: i64, now_ms: i64) -> FitState {
    let elapsed = now_ms.saturating_sub(updated_at).max(0);
    state.ewma_fail_rate *= 0.5_f64.powf(elapsed as f64 / HALF_LIFE_MS as f64);
    state
}

fn load_fit(
    db: &Connection,
    class: &str,
    model: &ModelKey,
) -> rusqlite::Result<Option<(FitState, i64)>> {
    db.query_row(
        "SELECT tier,trials,ewma_fail_rate,consecutive_failures,demotion_level,demoted_until_ms,updated_at
         FROM model_fit_aggregates WHERE task_class=?1 AND harness=?2 AND model=?3",
        params![class, model.harness, model.model],
        |r| {
            Ok((
                FitState {
                    tier: tier_from(r.get(0)?),
                    trials: r.get(1)?,
                    ewma_fail_rate: r.get(2)?,
                    consecutive_failures: r.get(3)?,
                    demotion_level: r.get(4)?,
                    demoted_until_ms: r.get(5)?,
                },
                r.get(6)?,
            ))
        },
    )
    .optional()
}

fn save_fit(
    db: &Connection,
    class: &str,
    model: &ModelKey,
    state: &FitState,
    now_ms: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO model_fit_aggregates(task_class,harness,model,tier,trials,ewma_fail_rate,
           consecutive_failures,demotion_level,demoted_until_ms,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            class,
            model.harness,
            model.model,
            tier_number(state.tier),
            state.trials,
            state.ewma_fail_rate,
            state.consecutive_failures,
            state.demotion_level,
            state.demoted_until_ms,
            now_ms
        ],
    )?;
    Ok(())
}

fn load_floor(db: &Connection, class: &str) -> rusqlite::Result<FloorState> {
    Ok(db
        .query_row(
            "SELECT min_tier,cooldown_until_ms,canary_passes FROM task_class_floors WHERE task_class=?1",
            [class],
            |r| {
                Ok(FloorState {
                    min_tier: tier_from(r.get(0)?),
                    cooldown_until_ms: r.get(1)?,
                    canary_passes: r.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn save_floor(db: &Connection, class: &str, floor: &FloorState) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO task_class_floors(task_class,min_tier,cooldown_until_ms,canary_passes)
         VALUES(?1,?2,?3,?4)",
        params![
            class,
            tier_number(floor.min_tier),
            floor.cooldown_until_ms,
            floor.canary_passes
        ],
    )?;
    Ok(())
}

/// The stored form of a task class.
fn class_key(task_class: &TaskClass) -> Result<String, EngineError> {
    serde_json::to_string(task_class).map_err(fail)
}

fn tier_number(tier: ModelTier) -> i64 {
    match tier {
        ModelTier::Small => 1,
        ModelTier::Medium => 2,
        ModelTier::Large => 3,
    }
}

fn tier_from(number: i64) -> ModelTier {
    match number {
        ..=1 => ModelTier::Small,
        2 => ModelTier::Medium,
        _ => ModelTier::Large,
    }
}

fn sql_count(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn parse_attribution(stored: &str) -> Attribution {
    [
        Attribution::Success,
        Attribution::ReasoningInadequacy,
        Attribution::ContextOverload,
        Attribution::BelowThreshold,
    ]
    .into_iter()
    .find(|attribution| attribution.as_str() == stored)
    .unwrap_or(Attribution::Excluded)
}
