//! The poller's state machine. It is synchronous and takes the time as an
//! argument, so cadence, backoff and the call budget are tested on a virtual
//! clock. It reads the registry only through [`TickInput`] and never writes
//! it: a tick changes the observed cache and nothing else.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use mast_proto::{
    BoxFact, Capacity, DeriveInput, ExternalRunSettings, ExternalRunsObserved, GhHealth, Probe,
    RunObservation, RunState, derive_run_state,
};
use serde::Serialize;

use super::cache::{self, CacheFile, CapState, CapacityFact, RunRecord};
use super::gh::{
    self, GhFailure, GhRunner, ListOutcome, ProbeOutcome, ProbeTarget, ReadOutcome, safe_box_name,
};

pub const BASE_INTERVAL_MS: i64 = 180_000;
pub const FAST_INTERVAL_MS: i64 = 60_000;
pub const FAST_WINDOW_MS: i64 = 300_000;
pub const MAX_INTERVAL_MS: i64 = 900_000;
pub const PROBE_INTERVAL_MS: i64 = 300_000;
pub const PROBE_BACKOFF_MS: [i64; 3] = [300_000, 600_000, 1_200_000];
pub const STALE_LIST_MS: i64 = 30_000;
pub const IDLE_GUARD_S: i64 = 90;
pub const CAPACITY_STALE_MS: i64 = 600_000;
pub const UNKNOWN_WAKE_MS: i64 = 900_000;
pub const ORPHAN_CONFIRMATIONS: u32 = 2;
pub const WRITE_INTERVAL_MS: i64 = 30_000;

/// One run as the poller sees it, read from the snapshot by the caller.
#[derive(Debug, Clone)]
pub struct RunInfo {
    pub id: String,
    pub run_tag: String,
    pub codespace_name: Option<String>,
    /// Expected alive, not ended, item not archived, recently updated.
    pub live: bool,
    /// Epoch seconds, when the durable record ended.
    pub ended_at: Option<i64>,
    pub open_actions: usize,
}

#[derive(Debug, Clone)]
pub struct SpaceInput {
    pub space_id: String,
    pub settings: ExternalRunSettings,
    pub runs: Vec<RunInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct TickInput {
    pub spaces: Vec<SpaceInput>,
}

impl TickInput {
    fn live(&self) -> impl Iterator<Item = (&SpaceInput, &RunInfo)> {
        self.spaces.iter().flat_map(|space| {
            space
                .runs
                .iter()
                .filter(|run| run.live)
                .map(move |run| (space, run))
        })
    }

    pub fn has_live(&self) -> bool {
        self.live().next().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeReason {
    Done,
    Failed(i32),
    Orphaned,
    UnknownHeld,
    CapacityBreach { running: u32, cap: u32 },
}

/// A wake whose dedupe key is already reserved in the cache file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeCandidate {
    pub key: String,
    pub space_id: String,
    pub run_id: String,
    pub reason: WakeReason,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub ticks: u64,
    pub list_calls: u64,
    pub list_unchanged: u64,
    pub list_not_modified: u64,
    pub list_failures: u64,
    pub rereads: u64,
    pub probes: u64,
    pub probe_failures: u64,
    pub probes_skipped_idle_guard: u64,
    pub backoff_level: u32,
    pub last_tick_ms: u64,
    pub wakes_reserved: u64,
    pub cache_writes: u64,
}

#[derive(Debug, Default)]
struct BoxProbe {
    next_ms: i64,
    failures: u32,
    seen_available: bool,
}

/// When the next tick is due.
#[derive(Debug, Default)]
struct Schedule {
    fast_until_ms: i64,
    failures: u32,
    /// Auth trouble: a flat wait that never escalates.
    flat: bool,
    retry_after_ms: Option<i64>,
    next_due_ms: Option<i64>,
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct PollerCore {
    runner: Arc<dyn GhRunner>,
    clock: Clock,
    path: PathBuf,
    jitter: Box<dyn FnMut() -> f64 + Send>,
    file: CacheFile,
    written: CacheFile,
    last_write_ms: i64,
    gh: GhHealth,
    listed_this_session: bool,
    list_at_ms: i64,
    probes: HashMap<String, BoxProbe>,
    last_probe: HashMap<String, Probe>,
    probe_at: HashMap<String, i64>,
    orphan_streak: HashMap<String, (u32, i64)>,
    unknown_since_ms: HashMap<String, i64>,
    schedule: Schedule,
    /// No capacity refresh before this, set by the last failed list call.
    capacity_retry_ms: i64,
    pub counters: Counters,
}

/// The part of the core the readers need, copied out at the end of a tick so
/// they never wait on a tick in flight.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    runs: BTreeMap<String, RunObservation>,
    capacity: Option<CapacityFact>,
    gh_problem: Option<String>,
    next_due_ms: Option<i64>,
    refresh_blocked: bool,
    capacity_retry_ms: i64,
}

impl Snapshot {
    pub fn due(&self, now_ms: i64) -> bool {
        self.next_due_ms.is_some_and(|due| now_ms >= due)
    }

    pub fn next_due_ms(&self) -> Option<i64> {
        self.next_due_ms
    }

    pub fn capacity_is_stale(&self, now_ms: i64) -> bool {
        refresh_due(
            self.capacity.as_ref().map(|fact| fact.at),
            self.refresh_blocked,
            self.capacity_retry_ms,
            now_ms,
        )
    }

    /// Whether what a reader would see differs, leaving out the schedule.
    pub fn same_view(&self, other: &Snapshot) -> bool {
        self.runs == other.runs
            && self.capacity == other.capacity
            && self.gh_problem == other.gh_problem
    }

    /// The observed view for one Space. Non-terminal runs already read "not
    /// yet observed" until a tick of this engine has seen them.
    pub fn view(&self, space: &SpaceInput) -> ExternalRunsObserved {
        let runs = space
            .runs
            .iter()
            .filter_map(|run| {
                self.runs
                    .get(&run.id)
                    .map(|obs| (run.id.clone(), obs.clone()))
            })
            .collect();
        let capacity = self.capacity.as_ref().map(|fact| Capacity {
            running: fact.running,
            cap: space.settings.capacity_cap,
            total: fact.total,
            box_limit: space.settings.box_limit,
            at: fact.at,
        });
        ExternalRunsObserved {
            hosted: true,
            capacity,
            gh_problem: self.gh_problem.clone(),
            runs,
        }
    }
}

fn refresh_due(capacity_at_s: Option<i64>, blocked: bool, retry_ms: i64, now_ms: i64) -> bool {
    !blocked
        && now_ms >= retry_ms
        && capacity_at_s.is_none_or(|at| now_ms - at * 1000 > CAPACITY_STALE_MS)
}

fn secs(ms: i64) -> i64 {
    ms.div_euclid(1000)
}

impl PollerCore {
    pub fn new(
        runner: Arc<dyn GhRunner>,
        clock: Clock,
        path: PathBuf,
        jitter: Box<dyn FnMut() -> f64 + Send>,
    ) -> Self {
        let file = cache::load(&path);
        Self {
            runner,
            clock,
            written: file.clone(),
            path,
            jitter,
            file,
            last_write_ms: i64::MIN / 2,
            gh: GhHealth::Ok,
            listed_this_session: false,
            list_at_ms: 0,
            probes: HashMap::new(),
            last_probe: HashMap::new(),
            probe_at: HashMap::new(),
            orphan_streak: HashMap::new(),
            unknown_since_ms: HashMap::new(),
            schedule: Schedule::default(),
            capacity_retry_ms: 0,
            counters: Counters::default(),
        }
    }

    #[cfg(test)]
    pub fn next_due_ms(&self) -> Option<i64> {
        self.schedule.next_due_ms
    }

    /// A live run changed in the registry: look soon, and often for a while.
    pub fn registry_changed(&mut self, now_ms: i64, has_live: bool) {
        if !has_live {
            self.schedule.next_due_ms = None;
            return;
        }
        self.schedule.fast_until_ms = now_ms + FAST_WINDOW_MS;
        let soon = now_ms;
        self.schedule.next_due_ms =
            Some(self.schedule.next_due_ms.map_or(soon, |due| due.min(soon)));
    }

    pub fn due(&self, now_ms: i64) -> bool {
        self.schedule.next_due_ms.is_some_and(|due| now_ms >= due)
    }

    fn interval_ms(&mut self, now_ms: i64) -> i64 {
        if let Some(wait) = self.schedule.retry_after_ms.take() {
            return wait;
        }
        if self.schedule.flat {
            return MAX_INTERVAL_MS;
        }
        let base = if now_ms < self.schedule.fast_until_ms {
            FAST_INTERVAL_MS
        } else {
            BASE_INTERVAL_MS
        };
        let doubled = base.saturating_mul(1i64 << self.schedule.failures.min(10));
        let interval = doubled.min(MAX_INTERVAL_MS);
        let jitter = (self.jitter)().clamp(-1.0, 1.0);
        (interval as f64 * (1.0 + 0.1 * jitter)) as i64
    }

    fn fail(&mut self, failure: &GhFailure) {
        self.counters.list_failures += 1;
        self.schedule.flat = false;
        match failure {
            GhFailure::Missing => {
                self.gh = GhHealth::Missing;
                self.schedule.flat = true;
            }
            GhFailure::NotSignedIn => {
                self.gh = GhHealth::NotSignedIn;
                self.schedule.flat = true;
            }
            GhFailure::Forbidden => {
                self.gh = GhHealth::Forbidden;
                self.schedule.flat = true;
            }
            GhFailure::RateLimited { retry_after_s } => {
                self.gh = GhHealth::Unreachable("rate limited".into());
                self.schedule.failures += 1;
                let wait = retry_after_s.map_or(MAX_INTERVAL_MS, |s| {
                    (s as i64 * 1000).clamp(10_000, 3_600_000)
                });
                self.schedule.retry_after_ms = Some(wait);
            }
            GhFailure::Unreachable(why) => {
                self.gh = GhHealth::Unreachable(why.clone());
                self.schedule.failures += 1;
            }
        }
        self.counters.backoff_level = self.schedule.failures;
    }

    fn succeed(&mut self) {
        self.gh = GhHealth::Ok;
        self.schedule.failures = 0;
        self.schedule.flat = false;
        self.counters.backoff_level = 0;
        self.capacity_retry_ms = 0;
    }

    /// How long the next call waits after a failure, without consuming it.
    fn backoff_ms(&self) -> i64 {
        if let Some(wait) = self.schedule.retry_after_ms {
            return wait;
        }
        if self.schedule.flat {
            return MAX_INTERVAL_MS;
        }
        BASE_INTERVAL_MS
            .saturating_mul(1i64 << self.schedule.failures.min(10))
            .min(MAX_INTERVAL_MS)
    }

    fn refresh_blocked(&self) -> bool {
        matches!(
            self.gh,
            GhHealth::NotSignedIn | GhHealth::Missing | GhHealth::Forbidden
        )
    }

    /// One list call. Returns whether it produced a usable list.
    fn list(&mut self, now_ms: i64) -> bool {
        self.counters.list_calls += 1;
        let now_s = secs(now_ms);
        match self.runner.list_codespaces(self.file.etag.as_deref()) {
            ListOutcome::NotModified => {
                self.counters.list_not_modified += 1;
                self.succeed();
            }
            ListOutcome::Boxes(list) => {
                self.succeed();
                if self.listed_this_session && self.file.list_hash == Some(list.body_hash) {
                    self.counters.list_unchanged += 1;
                } else {
                    self.file.boxes = list
                        .boxes
                        .into_iter()
                        .map(|(name, mut fact)| {
                            fact.at = now_s;
                            (name, fact)
                        })
                        .collect();
                    self.file.etag = list.etag;
                    self.file.list_hash = Some(list.body_hash);
                    self.file.capacity = Some(CapacityFact {
                        running: self
                            .file
                            .boxes
                            .values()
                            .filter(|b| b.is_available())
                            .count() as u32,
                        total: list.total.max(self.file.boxes.len() as u32),
                        at: now_s,
                    });
                }
            }
            ListOutcome::Failed(failure) => {
                self.fail(&failure);
                self.capacity_retry_ms = now_ms + self.backoff_ms();
                return false;
            }
        }
        if let Some(capacity) = self.file.capacity.as_mut() {
            capacity.at = now_s;
        }
        self.listed_this_session = true;
        self.list_at_ms = (self.clock)();
        true
    }

    /// Refresh the capacity line when it is older than ten minutes and no
    /// live run keeps it fresh. One list call, no probes, and none while gh is
    /// signed out or a failure is still backing off.
    pub fn refresh_capacity(&mut self, now_ms: i64) {
        if self.capacity_is_stale(now_ms) && self.list(now_ms) {
            self.persist(now_ms, false);
        }
    }

    /// Whether a capacity refresh is worth making now: the line is old and
    /// neither gh health nor a backoff says to wait.
    pub fn capacity_is_stale(&self, now_ms: i64) -> bool {
        refresh_due(
            self.file.capacity.as_ref().map(|fact| fact.at),
            self.refresh_blocked(),
            self.capacity_retry_ms,
            now_ms,
        )
    }

    /// A box fact read directly. When it disagrees with the list, the list's
    /// body hash is dropped so the next list is applied rather than skipped.
    fn apply_read(&mut self, name: &str, mut fresh: BoxFact, now_s: i64) -> bool {
        fresh.at = now_s;
        let available = fresh.is_available();
        if self.file.boxes.get(name).map(|fact| &fact.state) != Some(&fresh.state) {
            self.file.list_hash = None;
        }
        self.file.boxes.insert(name.to_owned(), fresh);
        available
    }

    fn probe_failed(&mut self, state: &mut BoxProbe, now_ms: i64, ids: &[&String]) {
        self.counters.probe_failures += 1;
        let step = (state.failures as usize).min(PROBE_BACKOFF_MS.len() - 1);
        state.next_ms = now_ms + PROBE_BACKOFF_MS[step];
        state.failures += 1;
        for id in ids {
            self.last_probe.insert((*id).clone(), Probe::Failed);
        }
    }

    fn probe_runs(&mut self, now_ms: i64, input: &TickInput) {
        let mut by_box: BTreeMap<String, Vec<(&SpaceInput, &RunInfo)>> = BTreeMap::new();
        for (space, run) in input.live() {
            if let Some(name) = run.codespace_name.as_deref() {
                by_box
                    .entry(name.to_owned())
                    .or_default()
                    .push((space, run));
            }
        }
        for (name, runs) in by_box {
            let mut state = self.probes.remove(&name).unwrap_or_default();
            self.probe_box(now_ms, &name, &runs, &mut state);
            self.probes.insert(name, state);
        }
    }

    fn probe_box(
        &mut self,
        now_ms: i64,
        name: &str,
        runs: &[(&SpaceInput, &RunInfo)],
        state: &mut BoxProbe,
    ) {
        let now_s = secs(now_ms);
        if !self.file.boxes.get(name).is_some_and(BoxFact::is_available) {
            state.seen_available = false;
            for (_, run) in runs {
                self.last_probe.remove(&run.id);
            }
            return;
        }
        let first = !state.seen_available;
        state.seen_available = true;
        let mut targets: Vec<(String, String, String, String, Option<String>)> = Vec::new();
        for (space, run) in runs {
            let settings = &space.settings;
            let (Some(dir), Some(exit), Some(session)) = (
                &settings.run_dir,
                &settings.exit_file,
                &settings.tmux_session,
            ) else {
                self.last_probe.insert(run.id.clone(), Probe::NotConfigured);
                continue;
            };
            if !gh::safe_run_id(&run.id) {
                self.last_probe.insert(run.id.clone(), Probe::Unsafe);
                continue;
            }
            let dir = dir.replace("{runTag}", &run.run_tag);
            let session = session.replace("{runTag}", &run.run_tag);
            if !plain_path(&dir) || !plain_name(&session) {
                self.last_probe.insert(run.id.clone(), Probe::Unsafe);
                continue;
            }
            targets.push((
                run.id.clone(),
                dir,
                exit.clone(),
                session,
                settings.output_file.clone(),
            ));
        }
        if targets.is_empty() || (!first && now_ms < state.next_ms) {
            return;
        }
        if !safe_box_name(name) {
            for target in &targets {
                self.last_probe.insert(target.0.clone(), Probe::Failed);
            }
            return;
        }
        if let Some(BoxFact {
            last_used_at: Some(used),
            idle_timeout_min: Some(timeout),
            ..
        }) = self.file.boxes.get(name)
            && used + i64::from(*timeout) * 60 - now_s < IDLE_GUARD_S
        {
            self.counters.probes_skipped_idle_guard += 1;
            return;
        }
        if (self.clock)() - self.list_at_ms > STALE_LIST_MS {
            self.counters.rereads += 1;
            match self.runner.read_codespace(name) {
                ReadOutcome::Box(fresh) => {
                    if !self.apply_read(name, fresh, now_s) {
                        return;
                    }
                }
                ReadOutcome::Gone => {
                    self.file.boxes.remove(name);
                    self.file.list_hash = None;
                    return;
                }
                ReadOutcome::Failed(_) => return,
            }
        }
        let probe_targets: Vec<ProbeTarget<'_>> = targets
            .iter()
            .map(|(id, dir, exit, session, output)| ProbeTarget {
                run_id: id,
                dir,
                exit_file: exit,
                output_file: output.as_deref(),
                session,
            })
            .collect();
        let script = gh::probe_script(&probe_targets);
        self.counters.probes += 1;
        match self.runner.probe(name, &script) {
            ProbeOutcome::Output(text) => {
                state.failures = 0;
                state.next_ms = now_ms + PROBE_INTERVAL_MS;
                let facts = gh::parse_probe(&text, now_s);
                for (id, ..) in &targets {
                    let probe = facts.get(id).cloned().map_or(Probe::Failed, Probe::Seen);
                    self.probe_at.insert(id.clone(), now_ms);
                    self.last_probe.insert(id.clone(), probe);
                }
            }
            ProbeOutcome::BoxStopped => {
                self.counters.rereads += 1;
                let confirmed_stopped = match self.runner.read_codespace(name) {
                    ReadOutcome::Box(fresh) => !self.apply_read(name, fresh, now_s),
                    ReadOutcome::Gone => {
                        self.file.boxes.remove(name);
                        self.file.list_hash = None;
                        true
                    }
                    ReadOutcome::Failed(_) => false,
                };
                if confirmed_stopped {
                    state.seen_available = false;
                    for (id, ..) in &targets {
                        self.last_probe.remove(id);
                    }
                } else {
                    let ids: Vec<&String> = targets.iter().map(|target| &target.0).collect();
                    self.probe_failed(state, now_ms, &ids);
                }
            }
            ProbeOutcome::Failed(failure) => {
                if matches!(
                    failure,
                    GhFailure::NotSignedIn | GhFailure::Missing | GhFailure::Forbidden
                ) {
                    self.fail(&failure);
                }
                let ids: Vec<&String> = targets.iter().map(|target| &target.0).collect();
                self.probe_failed(state, now_ms, &ids);
            }
        }
    }

    pub fn tick(&mut self, now_ms: i64, input: &TickInput) -> Vec<WakeCandidate> {
        let started = std::time::Instant::now();
        let now_s = secs(now_ms);
        self.counters.ticks += 1;
        self.note_reopens(input);
        if !input.has_live() {
            self.schedule.next_due_ms = None;
            self.persist(now_ms, false);
            return Vec::new();
        }
        let listed = self.list(now_ms);
        if listed {
            self.probe_runs(now_ms, input);
        }
        let mut wakes = Vec::new();
        let mut changed = false;
        let mut transitioned = false;
        for (space, run) in input.live() {
            let (state_changed, known_transition, wake) = self.observe_run(now_ms, space, run);
            changed |= state_changed;
            transitioned |= known_transition;
            wakes.extend(wake);
        }
        if listed {
            wakes.extend(self.capacity_wakes(input));
        }
        if transitioned {
            self.schedule.fast_until_ms = now_ms + FAST_WINDOW_MS;
        }
        let interval = self.interval_ms(now_ms);
        self.schedule.next_due_ms = Some(now_ms + interval);
        self.file.prune(now_s);
        self.persist(now_ms, changed || !wakes.is_empty());
        self.counters.last_tick_ms = started.elapsed().as_millis() as u64;
        wakes
    }

    fn note_reopens(&mut self, input: &TickInput) {
        for space in &input.spaces {
            for run in &space.runs {
                let Some(record) = self.file.runs.get_mut(&run.id) else {
                    continue;
                };
                if run.ended_at.is_some() {
                    record.ended_seen = true;
                    record.ended_at = run.ended_at;
                } else if run.live && record.ended_seen {
                    record.episode += 1;
                    record.ended_seen = false;
                    record.ended_at = None;
                    record.last_known = None;
                    record.unknown_fired = false;
                    record.obs.exit_code = None;
                    record.obs.state = RunState::Unknown;
                    self.orphan_streak.remove(&run.id);
                    self.last_probe.remove(&run.id);
                    if let Some(probe) = run
                        .codespace_name
                        .as_ref()
                        .and_then(|n| self.probes.get_mut(n))
                    {
                        probe.next_ms = 0;
                    }
                }
            }
        }
    }

    fn observe_run(
        &mut self,
        now_ms: i64,
        space: &SpaceInput,
        run: &RunInfo,
    ) -> (bool, bool, Option<WakeCandidate>) {
        let now_s = secs(now_ms);
        let known_exit = self.file.runs.get(&run.id).and_then(|r| r.obs.exit_code);
        let box_fact = run
            .codespace_name
            .as_ref()
            .and_then(|n| self.file.boxes.get(n));
        let available = box_fact.is_some_and(BoxFact::is_available);
        let probe = if !available {
            Probe::Pending
        } else {
            self.last_probe.get(&run.id).cloned().unwrap_or_else(|| {
                if space.settings.probe_configured() {
                    Probe::Pending
                } else {
                    Probe::NotConfigured
                }
            })
        };
        let derived = derive_run_state(&DeriveInput {
            gh: &self.gh,
            observed: self.listed_this_session,
            box_fact,
            probe: probe.clone(),
            known_exit,
            open_actions: run.open_actions,
            stall_after_s: mast_proto::external_run_state::DEFAULT_STALL_AFTER_SECS,
        });
        let idle_s = match &probe {
            Probe::Seen(fact) => fact.output_age_s,
            _ => None,
        };
        let record = self
            .file
            .runs
            .entry(run.id.clone())
            .or_insert_with(|| RunRecord {
                obs: RunObservation {
                    state: RunState::Unknown,
                    since: now_s,
                    seen_at: now_s,
                    why: String::new(),
                    exit_code: None,
                    idle_s: None,
                },
                last_known: None,
                ended_seen: false,
                episode: 0,
                unknown_fired: false,
                unknown_spells: 0,
                ended_at: None,
            });
        let previous = record.obs.state;
        let changed = previous != derived.state;
        let known_transition =
            changed && previous != RunState::Unknown && derived.state != RunState::Unknown;
        if changed {
            record.obs.since = now_s;
        }
        record.obs.state = derived.state;
        record.obs.why = derived.why;
        record.obs.seen_at = now_s;
        record.obs.idle_s = idle_s;
        if derived.exit_code.is_some() {
            record.obs.exit_code = derived.exit_code;
        }
        let candidate = |reason, word: &str, record: &RunRecord| WakeCandidate {
            key: format!("{}|{word}|{}", run.id, record.episode),
            space_id: space.space_id.clone(),
            run_id: run.id.clone(),
            reason,
        };
        let state = derived.state;
        let mut wake = None;
        if state == RunState::Unknown {
            let since = *self
                .unknown_since_ms
                .entry(run.id.clone())
                .or_insert(now_ms);
            if !record.unknown_fired && now_ms - since >= UNKNOWN_WAKE_MS {
                record.unknown_fired = true;
                record.unknown_spells += 1;
                let mut found = candidate(WakeReason::UnknownHeld, "unknown", record);
                found.key = format!("{}.{}", found.key, record.unknown_spells);
                wake = Some(found);
            }
        } else {
            self.unknown_since_ms.remove(&run.id);
            record.unknown_fired = false;
            let previous = record.last_known;
            let prior_open = previous.is_some_and(|p| !p.is_terminal());
            let mut settle = true;
            match state {
                RunState::Orphaned => {
                    let stamp = if box_fact.is_some_and(BoxFact::is_shutdown) {
                        self.list_at_ms
                    } else {
                        self.probe_at
                            .get(&run.id)
                            .copied()
                            .unwrap_or(self.list_at_ms)
                    };
                    let streak = self
                        .orphan_streak
                        .entry(run.id.clone())
                        .or_insert((0, i64::MIN));
                    if streak.1 != stamp {
                        *streak = (streak.0 + 1, stamp);
                    }
                    if streak.0 >= ORPHAN_CONFIRMATIONS {
                        if prior_open && previous != Some(RunState::Orphaned) {
                            wake = Some(candidate(WakeReason::Orphaned, "orphaned", record));
                        }
                    } else {
                        settle = false;
                    }
                }
                RunState::Done if prior_open => {
                    wake = Some(candidate(WakeReason::Done, "done", record));
                }
                RunState::Failed(code) if prior_open => {
                    wake = Some(candidate(WakeReason::Failed(code), "failed", record));
                }
                _ => {}
            }
            if state != RunState::Orphaned {
                self.orphan_streak.remove(&run.id);
            }
            if settle {
                record.last_known = Some(state);
            }
        }
        let wake = wake.filter(|wake| self.reserve(&wake.key));
        (changed, known_transition, wake)
    }

    fn reserve(&mut self, key: &str) -> bool {
        let fresh = self.file.notified.insert(key.to_owned());
        if fresh {
            self.counters.wakes_reserved += 1;
        }
        fresh
    }

    fn capacity_wakes(&mut self, input: &TickInput) -> Vec<WakeCandidate> {
        let Some(fact) = self.file.capacity.clone() else {
            return Vec::new();
        };
        let mut wakes = Vec::new();
        for space in &input.spaces {
            let Some(anchor) = space
                .runs
                .iter()
                .filter(|run| run.live)
                .map(|run| &run.id)
                .min()
            else {
                continue;
            };
            let capacity = Capacity {
                running: fact.running,
                cap: space.settings.capacity_cap,
                total: fact.total,
                box_limit: space.settings.box_limit,
                at: fact.at,
            };
            let now_at_cap = capacity.at_cap();
            let state = self
                .file
                .capacity_state
                .entry(space.space_id.clone())
                .or_insert(CapState {
                    at_cap: now_at_cap,
                    episode: 0,
                });
            match (state.at_cap, now_at_cap) {
                (false, true) => {
                    state.at_cap = true;
                    let key = format!("capacity:{}|breach|{}", space.space_id, state.episode);
                    if self.file.notified.insert(key.clone()) {
                        self.counters.wakes_reserved += 1;
                        wakes.push(WakeCandidate {
                            key,
                            space_id: space.space_id.clone(),
                            run_id: anchor.clone(),
                            reason: WakeReason::CapacityBreach {
                                running: capacity.running,
                                cap: capacity.cap,
                            },
                        });
                    }
                }
                (true, false) => {
                    state.at_cap = false;
                    state.episode += 1;
                }
                _ => {}
            }
        }
        wakes
    }

    /// Write the cache when the facts changed, at most every 30 s unless forced.
    pub fn persist(&mut self, now_ms: i64, force: bool) {
        if self.file.same_facts(&self.written) {
            return;
        }
        if !force && now_ms - self.last_write_ms < WRITE_INTERVAL_MS {
            return;
        }
        self.file.v = cache::CACHE_VERSION;
        self.file.saved_at = secs(now_ms);
        match cache::save(&self.path, &self.file) {
            Ok(()) => {
                self.written = self.file.clone();
                self.last_write_ms = now_ms;
                self.counters.cache_writes += 1;
            }
            Err(error) => {
                tracing::warn!(%error, "external runs: could not save the observed cache")
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let unobserved = !self.listed_this_session && self.gh == GhHealth::Ok;
        let runs = self
            .file
            .runs
            .iter()
            .map(|(id, record)| {
                let mut obs = record.obs.clone();
                if unobserved && !obs.state.is_terminal() {
                    obs.state = RunState::Unknown;
                    obs.why = "not yet observed".into();
                    obs.idle_s = None;
                }
                (id.clone(), obs)
            })
            .collect();
        Snapshot {
            runs,
            capacity: self.file.capacity.clone(),
            gh_problem: match &self.gh {
                GhHealth::Ok => None,
                GhHealth::Missing => Some("gh not installed".into()),
                GhHealth::NotSignedIn => Some("gh not signed in".into()),
                GhHealth::Forbidden => Some("gh forbidden: check the codespace scope".into()),
                GhHealth::Unreachable(why) => Some(format!("gh unreachable: {why}")),
            },
            next_due_ms: self.schedule.next_due_ms,
            refresh_blocked: self.refresh_blocked(),
            capacity_retry_ms: self.capacity_retry_ms,
        }
    }

    #[cfg(test)]
    pub fn view(&self, space: &SpaceInput) -> ExternalRunsObserved {
        self.snapshot().view(space)
    }
}

fn plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= 300
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn plain_path(text: &str) -> bool {
    text.starts_with('/')
        && !text.contains("..")
        && text.chars().count() <= 400
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
}
