//! Poller tests: a fake `gh` that records every call, and a virtual clock.
//! Nothing here can reach GitHub or a codespace.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use mast_proto::{BoxFact, ExternalRunSettings, RunState};

use super::gh::*;
use super::poller::*;

const T0: i64 = 1_800_000_000_000;

#[derive(Default)]
struct World {
    calls: Vec<String>,
    scripts: Vec<String>,
    boxes: BTreeMap<String, BoxFact>,
    list_fail: Option<GhFailure>,
    honor_etag: bool,
    probe_result: HashMap<String, ProbeOutcome>,
    exits: HashMap<String, i32>,
    tmux_down: HashSet<String>,
    output_age: Option<u64>,
    list_delay_ms: i64,
    probe_delay_ms: i64,
    reads: HashMap<String, ReadOutcome>,
}

struct Fake {
    world: Mutex<World>,
    clock: Arc<AtomicI64>,
    /// Held by the next list call: it reports it entered, then waits.
    gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

impl GhRunner for Fake {
    fn list_codespaces(&self, etag: Option<&str>) -> ListOutcome {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
        let world = &mut *self.world.lock().unwrap();
        world.calls.push("api user/codespaces".into());
        self.clock.fetch_add(world.list_delay_ms, Ordering::SeqCst);
        if let Some(failure) = &world.list_fail {
            return ListOutcome::Failed(failure.clone());
        }
        let body = format!("{:?}", world.boxes);
        let body_hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            body.hash(&mut hasher);
            hasher.finish()
        };
        let tag = format!("W/{body_hash}");
        if world.honor_etag && etag == Some(tag.as_str()) {
            return ListOutcome::NotModified;
        }
        ListOutcome::Boxes(ListedBoxes {
            etag: Some(tag),
            body_hash,
            boxes: world
                .boxes
                .iter()
                .map(|(n, f)| (n.clone(), f.clone()))
                .collect(),
            total: world.boxes.len() as u32,
        })
    }

    fn read_codespace(&self, name: &str) -> ReadOutcome {
        let world = &mut *self.world.lock().unwrap();
        world.calls.push(format!("api user/codespaces/{name}"));
        if let Some(outcome) = world.reads.get(name) {
            return outcome.clone();
        }
        world
            .boxes
            .get(name)
            .cloned()
            .map_or(ReadOutcome::Gone, ReadOutcome::Box)
    }

    fn probe(&self, name: &str, script: &str) -> ProbeOutcome {
        let world = &mut *self.world.lock().unwrap();
        world.calls.push(format!("codespace ssh {name}"));
        world.scripts.push(script.to_owned());
        self.clock.fetch_add(world.probe_delay_ms, Ordering::SeqCst);
        if let Some(outcome) = world.probe_result.get(name) {
            return outcome.clone();
        }
        let now_s = self.clock.load(Ordering::SeqCst) / 1000;
        let mut out = String::new();
        for line in script.lines() {
            if let Some(id) = line
                .strip_prefix("echo 'run=")
                .and_then(|l| l.strip_suffix('\''))
            {
                out.push_str(&format!("run={id}\n"));
                if let Some(code) = world.exits.get(id) {
                    out.push_str(&format!("exit={code}\n"));
                }
                out.push_str(&format!(
                    "tmux={}\n",
                    u8::from(!world.tmux_down.contains(id))
                ));
                if let Some(age) = world.output_age {
                    out.push_str(&format!("mtime={}\n", now_s - age as i64));
                }
            }
        }
        out.push_str("done=1\n");
        ProbeOutcome::Output(out)
    }
}

struct Rig {
    core: PollerCore,
    fake: Arc<Fake>,
    clock: Arc<AtomicI64>,
    input: TickInput,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

fn settings() -> ExternalRunSettings {
    ExternalRunSettings {
        run_dir: Some("/runs/{runTag}".into()),
        exit_file: Some("exit".into()),
        output_file: Some("out.log".into()),
        tmux_session: Some("t-{runTag}".into()),
        ..ExternalRunSettings::default()
    }
}

fn available() -> BoxFact {
    BoxFact {
        state: "Available".into(),
        last_used_at: None,
        idle_timeout_min: None,
        at: 0,
    }
}

fn run(id: &str, box_name: &str) -> RunInfo {
    RunInfo {
        id: id.into(),
        run_tag: format!("tag-{id}"),
        codespace_name: Some(box_name.into()),
        live: true,
        ended_at: None,
        open_actions: 0,
    }
}

impl Rig {
    fn new(runs: Vec<RunInfo>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache/external-runs-observed.json");
        let clock = Arc::new(AtomicI64::new(T0));
        let fake = Arc::new(Fake {
            world: Mutex::new(World::default()),
            clock: clock.clone(),
            gate: Mutex::new(None),
        });
        {
            let mut world = fake.world.lock().unwrap();
            for run in &runs {
                world
                    .boxes
                    .entry(run.codespace_name.clone().unwrap())
                    .or_insert_with(available);
            }
        }
        let mut rig = Self {
            core: Self::core(&fake, &clock, &path, 0.0),
            fake,
            clock,
            input: TickInput {
                spaces: vec![SpaceInput {
                    space_id: "space".into(),
                    settings: settings(),
                    runs,
                }],
            },
            path,
            _dir: dir,
        };
        rig.core.registry_changed(T0, true);
        rig
    }

    fn core(
        fake: &Arc<Fake>,
        clock: &Arc<AtomicI64>,
        path: &std::path::Path,
        jitter: f64,
    ) -> PollerCore {
        let clock = clock.clone();
        PollerCore::new(
            fake.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
            path.to_path_buf(),
            Box::new(move || jitter),
        )
    }

    /// A new core over the same cache file, as after an engine restart.
    fn restart(&mut self) {
        self.core = Self::core(&self.fake, &self.clock, &self.path, 0.0);
        self.core.registry_changed(self.now(), true);
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.fake.world.lock().unwrap()
    }

    fn tick(&mut self) -> Vec<WakeCandidate> {
        self.core.tick(self.now(), &self.input)
    }

    /// Advance to the next due time and tick once.
    fn next(&mut self) -> Vec<WakeCandidate> {
        let due = self.core.next_due_ms().expect("a tick is scheduled");
        self.clock.fetch_max(due, Ordering::SeqCst);
        self.tick()
    }

    /// Tick on schedule until a probe has run; returns every wake.
    fn until_probe(&mut self) -> Vec<WakeCandidate> {
        let before = self.calls("codespace ssh");
        let mut wakes = Vec::new();
        for _ in 0..20 {
            wakes.extend(self.next());
            if self.calls("codespace ssh") > before {
                return wakes;
            }
        }
        panic!("no probe ran");
    }

    /// Tick on schedule until `until`; returns every wake.
    fn simulate_until(&mut self, until: i64) -> Vec<WakeCandidate> {
        let mut wakes = Vec::new();
        while self.core.next_due_ms().is_some_and(|due| due < until) {
            wakes.extend(self.next());
        }
        self.clock.fetch_max(until, Ordering::SeqCst);
        wakes
    }

    fn calls(&self, prefix: &str) -> usize {
        self.world()
            .calls
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }

    fn state(&self, id: &str) -> (RunState, String) {
        let view = self.core.view(&self.input.spaces[0]);
        let obs = &view.runs[id];
        (obs.state, obs.why.clone())
    }
}

#[test]
fn idle_makes_no_call_and_schedules_nothing() {
    let mut rig = Rig::new(vec![]);
    rig.core.registry_changed(T0, false);
    assert_eq!(rig.core.next_due_ms(), None);
    rig.tick();
    assert!(rig.world().calls.is_empty());
    assert_eq!(rig.core.next_due_ms(), None);
}

#[test]
fn one_live_run_over_an_hour_stays_inside_the_budget() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.simulate_until(T0 + 3_600_000);
    let first_hour = rig.calls("api user/codespaces");
    assert!(
        first_hour <= 30,
        "list calls in an hour with a fast window: {first_hour}"
    );
    assert!(rig.calls("codespace ssh") <= 12);
    let before = (rig.calls("api user/codespaces"), rig.calls("codespace ssh"));
    rig.simulate_until(T0 + 7_200_000);
    let second = rig.calls("api user/codespaces") - before.0;
    assert!(
        (19..=21).contains(&second),
        "steady list calls in an hour: {second}"
    );
    assert!(rig.calls("codespace ssh") - before.1 <= 12);
    println!(
        "one live run, steady hour: {second} list calls, {} probes",
        rig.calls("codespace ssh") - before.1
    );
}

#[test]
fn five_live_runs_share_one_list_call_and_probe_per_box() {
    let runs: Vec<_> = (1..=5)
        .map(|n| run(&format!("a{n}"), &format!("box-{n}")))
        .collect();
    let mut rig = Rig::new(runs);
    rig.simulate_until(T0 + 3_600_000);
    let before = (rig.calls("api user/codespaces"), rig.calls("codespace ssh"));
    rig.simulate_until(T0 + 7_200_000);
    let lists = rig.calls("api user/codespaces") - before.0;
    let probes = rig.calls("codespace ssh") - before.1;
    assert!((19..=21).contains(&lists), "{lists}");
    assert!(probes <= 60, "{probes}");
    println!("five live runs, steady hour: {lists} list calls, {probes} probes");
}

#[test]
fn two_runs_on_one_box_make_one_ssh_call() {
    let mut rig = Rig::new(vec![run("a1", "box-1"), run("a2", "box-1")]);
    rig.tick();
    assert_eq!(rig.calls("codespace ssh"), 1);
    let script = rig.world().scripts[0].clone();
    assert!(script.contains("run=a1") && script.contains("run=a2"));
}

#[test]
fn the_fast_window_relaxes_to_the_base_interval() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    assert_eq!(rig.core.next_due_ms(), Some(T0 + FAST_INTERVAL_MS));
    rig.simulate_until(T0 + FAST_WINDOW_MS + 1);
    let before = rig.now();
    rig.next();
    let due = rig.core.next_due_ms().unwrap();
    assert_eq!(due - rig.now(), BASE_INTERVAL_MS, "relaxed from {before}");
    rig.core.registry_changed(rig.now(), true);
    assert!(rig.core.due(rig.now()));
    rig.tick();
    assert_eq!(rig.core.next_due_ms(), Some(rig.now() + FAST_INTERVAL_MS));
}

#[test]
fn failures_double_the_interval_up_to_fifteen_minutes_and_reset_on_success() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.simulate_until(T0 + FAST_WINDOW_MS + 1);
    rig.world().list_fail = Some(GhFailure::Unreachable("offline".into()));
    let mut gaps = Vec::new();
    for _ in 0..5 {
        rig.next();
        gaps.push(rig.core.next_due_ms().unwrap() - rig.now());
    }
    assert_eq!(gaps, [360_000, 720_000, 900_000, 900_000, 900_000]);
    assert_eq!(rig.core.counters.backoff_level, 5);
    assert_eq!(
        rig.state("a1"),
        (RunState::Unknown, "gh unreachable: offline".into())
    );
    rig.world().list_fail = None;
    rig.next();
    assert_eq!(
        rig.core.next_due_ms().unwrap() - rig.now(),
        BASE_INTERVAL_MS
    );
    assert_eq!(rig.core.counters.backoff_level, 0);
}

#[test]
fn an_auth_failure_waits_a_flat_fifteen_minutes_and_never_goes_further() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().list_fail = Some(GhFailure::NotSignedIn);
    for _ in 0..4 {
        rig.next();
        assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), MAX_INTERVAL_MS);
    }
    assert_eq!(
        rig.state("a1"),
        (RunState::Unknown, "gh not signed in".into())
    );
    assert_eq!(rig.calls("codespace ssh"), 0);
    assert_eq!(rig.calls("api user/codespaces/"), 0);
    rig.world().list_fail = Some(GhFailure::Missing);
    rig.next();
    assert_eq!(rig.state("a1").1, "gh not installed");
}

#[test]
fn a_rate_limit_honors_retry_after_else_waits_fifteen_minutes() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().list_fail = Some(GhFailure::RateLimited {
        retry_after_s: Some(120),
    });
    rig.next();
    assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), 120_000);
    rig.world().list_fail = Some(GhFailure::RateLimited {
        retry_after_s: None,
    });
    rig.next();
    assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), MAX_INTERVAL_MS);
}

#[test]
fn jitter_stays_within_ten_percent() {
    for jitter in [-1.0, 1.0, 0.0] {
        let mut rig = Rig::new(vec![run("a1", "box-1")]);
        rig.core = Rig::core(&rig.fake, &rig.clock, &rig.path, jitter);
        rig.core.registry_changed(T0, true);
        rig.simulate_until(T0 + FAST_WINDOW_MS + 1);
        rig.next();
        let gap = (rig.core.next_due_ms().unwrap() - rig.now()) as f64;
        let base = BASE_INTERVAL_MS as f64;
        assert!(
            gap >= base * 0.9 - 1.0 && gap <= base * 1.1 + 1.0,
            "{gap} for {jitter}"
        );
        assert_eq!(gap as i64 == BASE_INTERVAL_MS, jitter == 0.0);
    }
}

#[test]
fn a_sleep_gap_is_one_immediate_tick_not_a_burst() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.simulate_until(T0 + 600_000);
    let before = rig.calls("api user/codespaces");
    rig.clock.fetch_add(3 * 3_600_000, Ordering::SeqCst);
    assert!(rig.core.due(rig.now()));
    rig.tick();
    assert_eq!(rig.calls("api user/codespaces") - before, 1);
    assert!(!rig.core.due(rig.now()));
}

#[test]
fn a_box_close_to_idling_out_is_not_probed() {
    let now_s = T0 / 1000;
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Available".into(),
            last_used_at: Some(now_s - 30 * 60 + 80),
            idle_timeout_min: Some(30),
            at: 0,
        },
    );
    rig.tick();
    assert_eq!(rig.calls("codespace ssh"), 0);
    assert_eq!(rig.core.counters.probes_skipped_idle_guard, 1);
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Available".into(),
            last_used_at: Some(now_s - 30 * 60 + 400),
            idle_timeout_min: Some(30),
            at: 0,
        },
    );
    rig.next();
    assert_eq!(rig.calls("codespace ssh"), 1);
}

#[test]
fn a_box_long_past_its_idle_window_is_still_probed() {
    let now_s = T0 / 1000;
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Available".into(),
            last_used_at: Some(now_s - 2 * 3600),
            idle_timeout_min: Some(60),
            at: 0,
        },
    );
    rig.tick();
    assert_eq!(rig.calls("codespace ssh"), 1);
    assert_eq!(rig.core.counters.probes_skipped_idle_guard, 0);
}

#[test]
fn a_stale_list_rereads_the_box_once_before_probing() {
    let two = || Rig::new(vec![run("a1", "box-1"), run("a2", "box-2")]);
    let mut rig = two();
    rig.tick();
    assert_eq!(rig.calls("api user/codespaces/"), 0);
    assert_eq!(rig.calls("codespace ssh"), 2);

    // The first probe is slow, so the list is old by the time box-2 is reached.
    let mut rig = two();
    rig.world().probe_delay_ms = STALE_LIST_MS + 1;
    rig.tick();
    assert_eq!(
        rig.world()
            .calls
            .iter()
            .filter(|c| c.as_str() == "api user/codespaces/box-2")
            .count(),
        1
    );
    assert_eq!(rig.calls("api user/codespaces/"), 1);
    assert_eq!(rig.calls("codespace ssh"), 2);

    let mut rig = two();
    rig.world().probe_delay_ms = STALE_LIST_MS + 1;
    rig.world().reads.insert(
        "box-2".into(),
        ReadOutcome::Box(BoxFact {
            state: "Shutdown".into(),
            ..available()
        }),
    );
    rig.tick();
    assert_eq!(
        rig.calls("codespace ssh"),
        1,
        "a box that stopped during the tick is not connected to"
    );
    assert_eq!(rig.state("a2").0, RunState::Orphaned);
}

#[test]
fn a_stopped_hint_confirmed_by_a_read_is_a_box_state_not_a_failure() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    {
        let mut world = rig.world();
        world
            .probe_result
            .insert("box-1".into(), ProbeOutcome::BoxStopped);
        world.reads.insert(
            "box-1".into(),
            ReadOutcome::Box(BoxFact {
                state: "Shutdown".into(),
                ..available()
            }),
        );
    }
    rig.tick();
    assert_eq!(rig.core.counters.probe_failures, 0);
    assert_eq!(rig.calls("api user/codespaces/box-1"), 1);
    assert_eq!(rig.state("a1").0, RunState::Orphaned);
}

#[test]
fn a_stopped_hint_for_an_available_box_never_orphans_or_wakes() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world()
        .probe_result
        .insert("box-1".into(), ProbeOutcome::BoxStopped);
    let wakes = rig.simulate_until(T0 + 600_000);
    assert!(wakes.is_empty(), "{wakes:?}");
    assert_ne!(rig.state("a1").0, RunState::Orphaned);
    assert!(rig.core.counters.rereads >= 1);
    assert!(rig.core.counters.probe_failures >= 1);
    assert!(rig.core.counters.list_unchanged > 0);
}

#[test]
fn a_box_read_as_stopped_is_corrected_by_the_next_unchanged_list() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    {
        let mut world = rig.world();
        world
            .probe_result
            .insert("box-1".into(), ProbeOutcome::BoxStopped);
        world.reads.insert(
            "box-1".into(),
            ReadOutcome::Box(BoxFact {
                state: "Shutdown".into(),
                ..available()
            }),
        );
    }
    assert!(rig.tick().is_empty());
    assert_eq!(rig.state("a1").0, RunState::Orphaned);
    {
        let mut world = rig.world();
        world.probe_result.clear();
        world.reads.clear();
    }
    assert!(rig.next().is_empty());
    assert_eq!(rig.state("a1").0, RunState::Running);
    assert_eq!(rig.core.counters.list_unchanged, 0);
}

#[test]
fn capacity_refresh_never_loops_on_a_failing_gh() {
    let mut rig = Rig::new(vec![]);
    rig.world().list_fail = Some(GhFailure::Unreachable("down".into()));
    for _ in 0..30 {
        rig.clock.fetch_add(1_000, Ordering::SeqCst);
        rig.core.refresh_capacity(rig.now());
    }
    assert_eq!(rig.calls("api user/codespaces"), 1);
    assert!(!rig.core.snapshot().capacity_is_stale(rig.now()));
    rig.clock.fetch_add(MAX_INTERVAL_MS, Ordering::SeqCst);
    rig.core.refresh_capacity(rig.now());
    assert_eq!(rig.calls("api user/codespaces"), 2);
}

#[test]
fn capacity_refresh_stops_while_gh_is_signed_out_or_forbidden() {
    for failure in [GhFailure::NotSignedIn, GhFailure::Missing, GhFailure::Forbidden] {
        let mut rig = Rig::new(vec![]);
        rig.world().list_fail = Some(failure);
        for _ in 0..10 {
            rig.clock.fetch_add(1_000, Ordering::SeqCst);
            rig.core.refresh_capacity(rig.now());
        }
        assert_eq!(rig.calls("api user/codespaces"), 1);
        assert!(!rig.core.snapshot().capacity_is_stale(rig.now()));
        rig.clock.fetch_add(MAX_INTERVAL_MS, Ordering::SeqCst);
        assert!(rig.core.snapshot().capacity_is_stale(rig.now()));
        rig.core.refresh_capacity(rig.now());
        assert_eq!(rig.calls("api user/codespaces"), 2);
    }
}

#[test]
fn a_fixed_gh_clears_the_problem_on_the_first_refresh_after_the_flat_window() {
    let mut rig = Rig::new(vec![]);
    rig.world().list_fail = Some(GhFailure::Forbidden);
    rig.core.refresh_capacity(rig.now());
    let observed = |rig: &Rig| rig.core.view(&rig.input.spaces[0]);
    assert_eq!(
        observed(&rig).gh_problem.as_deref(),
        Some("gh forbidden: check the codespace scope")
    );
    rig.world().list_fail = None;
    rig.clock.fetch_add(MAX_INTERVAL_MS - 1, Ordering::SeqCst);
    assert!(!rig.core.snapshot().capacity_is_stale(rig.now()));
    rig.clock.fetch_add(1, Ordering::SeqCst);
    assert!(rig.core.snapshot().capacity_is_stale(rig.now()));
    rig.core.refresh_capacity(rig.now());
    assert_eq!(rig.calls("api user/codespaces"), 2);
    let view = observed(&rig);
    assert_eq!(view.gh_problem, None);
    assert!(view.capacity.is_some());
}

#[test]
fn a_plain_403_is_a_scope_problem_on_a_flat_fifteen_minutes() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().list_fail = Some(GhFailure::Forbidden);
    rig.next();
    assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), MAX_INTERVAL_MS);
    rig.next();
    assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), MAX_INTERVAL_MS);
    assert_eq!(
        rig.core.view(&rig.input.spaces[0]).gh_problem.as_deref(),
        Some("gh forbidden: check the codespace scope")
    );
}

#[test]
fn an_auth_failure_from_one_probe_never_touches_gh_health_or_the_schedule() {
    for failure in [GhFailure::Forbidden, GhFailure::NotSignedIn] {
        let mut rig = Rig::new(vec![run("a1", "box-1"), run("a2", "box-2")]);
        rig.world()
            .probe_result
            .insert("box-1".into(), ProbeOutcome::Failed(failure));
        rig.tick();
        assert_eq!(rig.state("a1"), (RunState::Unknown, "probe failed".into()));
        assert_eq!(rig.state("a2").0, RunState::Running);
        assert_eq!(rig.core.view(&rig.input.spaces[0]).gh_problem, None);
        assert_eq!(rig.core.counters.list_failures, 0);
        assert_eq!(rig.core.counters.backoff_level, 0);
        assert_eq!(rig.core.next_due_ms().unwrap() - rig.now(), FAST_INTERVAL_MS);
        let probes_to = |rig: &Rig, name: &str| {
            rig.world()
                .calls
                .iter()
                .filter(|c| c.as_str() == format!("codespace ssh {name}"))
                .count()
        };
        let box_2 = probes_to(&rig, "box-2");
        rig.next();
        assert_eq!(probes_to(&rig, "box-1"), 1);
        assert_eq!(probes_to(&rig, "box-2"), box_2);
        assert_eq!(rig.core.view(&rig.input.spaces[0]).gh_problem, None);
        assert_eq!(rig.core.counters.list_failures, 0);
    }
}

#[test]
fn a_failed_probe_backs_off_five_ten_twenty_minutes_for_that_box_only() {
    let mut rig = Rig::new(vec![run("a1", "box-1"), run("a2", "box-2")]);
    rig.world().probe_result.insert(
        "box-1".into(),
        ProbeOutcome::Failed(GhFailure::Unreachable("x".into())),
    );
    rig.tick();
    assert_eq!(rig.state("a1"), (RunState::Unknown, "probe failed".into()));
    assert_eq!(rig.state("a2").0, RunState::Running);
    let probes_to = |rig: &Rig, name: &str| {
        rig.world()
            .calls
            .iter()
            .filter(|c| c.as_str() == format!("codespace ssh {name}"))
            .count()
    };
    let mut starts = vec![rig.now()];
    for _ in 0..3 {
        let before = probes_to(&rig, "box-1");
        while probes_to(&rig, "box-1") == before {
            rig.next();
        }
        starts.push(rig.now());
    }
    let gaps: Vec<i64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        gaps[0] >= 300_000 && gaps[0] < 300_000 + BASE_INTERVAL_MS,
        "{gaps:?}"
    );
    assert!(
        gaps[1] >= 600_000 && gaps[1] < 600_000 + BASE_INTERVAL_MS,
        "{gaps:?}"
    );
    assert!(
        gaps[2] >= 1_200_000 && gaps[2] < 1_200_000 + BASE_INTERVAL_MS,
        "{gaps:?}"
    );
}

#[test]
fn a_stopped_box_is_never_probed_and_orphans_within_one_tick() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    assert_eq!(rig.state("a1").0, RunState::Running);
    let probes = rig.calls("codespace ssh");
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Shutdown".into(),
            ..available()
        },
    );
    rig.next();
    assert_eq!(rig.state("a1").0, RunState::Orphaned);
    rig.next();
    assert_eq!(rig.calls("codespace ssh"), probes);
}

#[test]
fn an_orphan_wakes_only_after_two_consecutive_shutdown_reads() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Shutdown".into(),
            ..available()
        },
    );
    assert!(rig.next().is_empty());
    let wakes = rig.next();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].reason, WakeReason::Orphaned);
    assert!(rig.next().is_empty());
    assert!(rig.next().is_empty());
}

#[test]
fn a_box_that_comes_back_before_the_second_read_does_not_wake() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Shutdown".into(),
            ..available()
        },
    );
    assert!(rig.next().is_empty());
    rig.world().boxes.insert("box-1".into(), available());
    assert!(rig.next().is_empty());
    rig.world().boxes.insert(
        "box-1".into(),
        BoxFact {
            state: "Shutdown".into(),
            ..available()
        },
    );
    assert!(rig.next().is_empty());
}

#[test]
fn transient_box_states_never_orphan_or_wake() {
    for state in ["Starting", "ShuttingDown", "Queued", "Unavailable"] {
        let mut rig = Rig::new(vec![run("a1", "box-1")]);
        rig.tick();
        rig.world().boxes.insert(
            "box-1".into(),
            BoxFact {
                state: state.into(),
                ..available()
            },
        );
        for _ in 0..4 {
            assert!(rig.next().is_empty(), "{state}");
        }
        assert_eq!(rig.state("a1").0, RunState::Unknown, "{state}");
    }
}

#[test]
fn done_and_failed_wake_once_and_a_replayed_tick_does_not_repeat() {
    let mut rig = Rig::new(vec![run("a1", "box-1"), run("a2", "box-2")]);
    assert!(rig.tick().is_empty());
    {
        let mut world = rig.world();
        world.exits.insert("a1".into(), 0);
        world.exits.insert("a2".into(), 3);
    }
    let mut wakes = Vec::new();
    for _ in 0..6 {
        wakes.extend(rig.next());
    }
    wakes.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    assert_eq!(wakes.len(), 2, "{wakes:?}");
    assert_eq!(wakes[0].reason, WakeReason::Done);
    assert_eq!(wakes[1].reason, WakeReason::Failed(3));
    assert_eq!(rig.state("a2").0, RunState::Failed(3));
    assert!(rig.tick().is_empty());
}

#[test]
fn running_ticks_and_unknown_flicker_do_not_wake() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    for _ in 0..4 {
        assert!(rig.next().is_empty());
    }
    rig.world().list_fail = Some(GhFailure::Unreachable("blip".into()));
    assert!(rig.next().is_empty());
    rig.world().list_fail = None;
    assert!(rig.next().is_empty());
    assert_eq!(rig.state("a1").0, RunState::Running);
}

#[test]
fn unknown_for_fifteen_minutes_wakes_once() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.world().list_fail = Some(GhFailure::NotSignedIn);
    let mut wakes = Vec::new();
    for _ in 0..10 {
        wakes.extend(rig.next());
    }
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].reason, WakeReason::UnknownHeld);
    rig.world().list_fail = None;
    rig.next();
    rig.world().list_fail = Some(GhFailure::Unreachable("again".into()));
    for _ in 0..2 {
        assert!(
            rig.next().is_empty(),
            "a new spell is shorter than fifteen minutes"
        );
    }
}

#[test]
fn a_restart_does_not_storm_or_repeat() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.world().exits.insert("a1".into(), 0);
    assert_eq!(rig.until_probe().len(), 1);
    rig.restart();
    assert!(rig.tick().is_empty());
    assert!(rig.next().is_empty());
    assert_eq!(rig.state("a1").0, RunState::Done);
}

#[test]
fn a_restart_reads_not_yet_observed_until_the_first_tick() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.restart();
    assert_eq!(
        rig.state("a1"),
        (RunState::Unknown, "not yet observed".into())
    );
    rig.tick();
    assert_eq!(rig.state("a1").0, RunState::Running);
}

#[test]
fn an_exit_seen_survives_a_restart_and_a_dead_gh() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world().exits.insert("a1".into(), 0);
    rig.tick();
    rig.restart();
    rig.world().list_fail = Some(GhFailure::NotSignedIn);
    rig.tick();
    assert_eq!(rig.state("a1").0, RunState::Done);
}

#[test]
fn a_transition_missed_while_stopped_wakes_once_after_the_restart() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.restart();
    rig.world().exits.insert("a1".into(), 5);
    let wakes = rig.tick();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].reason, WakeReason::Failed(5));
    rig.restart();
    assert!(rig.tick().is_empty());
}

#[test]
fn a_reopened_run_starts_a_new_episode() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.world().exits.insert("a1".into(), 0);
    let first = rig.until_probe();
    assert_eq!(first.len(), 1);
    rig.input.spaces[0].runs[0].live = false;
    rig.input.spaces[0].runs[0].ended_at = Some(T0 / 1000);
    rig.next();
    rig.input.spaces[0].runs[0].live = true;
    rig.input.spaces[0].runs[0].ended_at = None;
    rig.world().exits.clear();
    rig.core.registry_changed(rig.now(), true);
    assert!(rig.tick().is_empty());
    assert_eq!(rig.state("a1").0, RunState::Running);
    rig.world().exits.insert("a1".into(), 0);
    let second = rig.until_probe();
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].key, second[0].key);
}

#[test]
fn the_box_total_shows_with_and_without_a_limit_and_never_wakes() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    for n in 2..=6 {
        rig.world().boxes.insert(
            format!("box-{n}"),
            BoxFact {
                state: "Shutdown".into(),
                ..available()
            },
        );
    }
    rig.tick();
    let line = |rig: &Rig| rig.core.view(&rig.input.spaces[0]).capacity.unwrap().line();
    assert_eq!(line(&rig), "1/5 running, 6 boxes");
    rig.input.spaces[0].settings.box_limit = Some(7);
    assert_eq!(line(&rig), "1/5 running, 6/7 boxes");
    for n in 7..=9 {
        rig.world().boxes.insert(
            format!("box-{n}"),
            BoxFact {
                state: "Shutdown".into(),
                ..available()
            },
        );
    }
    for _ in 0..4 {
        assert!(rig.next().is_empty(), "reaching the box limit wakes no one");
    }
    assert_eq!(line(&rig), "1/5 running, 9/7 boxes");
    assert_eq!(rig.core.counters.wakes_reserved, 0);
}

#[test]
fn capacity_breach_wakes_once_per_episode() {
    let runs: Vec<_> = (1..=2)
        .map(|n| run(&format!("a{n}"), &format!("box-{n}")))
        .collect();
    let mut rig = Rig::new(runs);
    rig.input.spaces[0].settings.capacity_cap = 3;
    {
        let mut world = rig.world();
        world.boxes.insert(
            "box-3".into(),
            BoxFact {
                state: "Shutdown".into(),
                ..available()
            },
        );
    }
    assert!(rig.tick().is_empty());
    rig.world().boxes.insert("box-3".into(), available());
    let wakes = rig.next();
    assert_eq!(wakes.len(), 1);
    assert_eq!(
        wakes[0].reason,
        WakeReason::CapacityBreach { running: 3, cap: 3 }
    );
    assert!(rig.next().is_empty());
    rig.world().boxes.insert(
        "box-3".into(),
        BoxFact {
            state: "Shutdown".into(),
            ..available()
        },
    );
    assert!(rig.next().is_empty());
    rig.world().boxes.insert("box-3".into(), available());
    let again = rig.next();
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].key, wakes[0].key);
}

#[test]
fn a_first_sighting_at_capacity_does_not_wake() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.input.spaces[0].settings.capacity_cap = 1;
    assert!(rig.tick().is_empty());
    assert!(rig.next().is_empty());
}

#[test]
fn an_unchanged_list_skips_work_and_a_matching_etag_is_a_304() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    rig.next();
    assert_eq!(rig.core.counters.list_unchanged, 1);
    rig.world().honor_etag = true;
    rig.next();
    assert_eq!(rig.core.counters.list_not_modified, 1);
    assert_eq!(rig.state("a1").0, RunState::Running);
}

#[test]
fn needs_human_follows_the_open_actions_of_the_item() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.input.spaces[0].runs[0].open_actions = 2;
    rig.tick();
    assert_eq!(rig.state("a1").0, RunState::NeedsHuman);
}

#[test]
fn an_unsafe_run_name_is_never_sent_to_the_box() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.input.spaces[0].runs[0].run_tag = "x; rm -rf /".into();
    rig.tick();
    assert_eq!(rig.calls("codespace ssh"), 0);
    assert_eq!(
        rig.state("a1"),
        (RunState::Unknown, "run name not probe-safe".into())
    );
}

#[test]
fn an_unconfigured_space_lists_but_never_probes() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.input.spaces[0].settings = ExternalRunSettings::default();
    rig.tick();
    assert_eq!(rig.calls("codespace ssh"), 0);
    assert_eq!(
        rig.state("a1"),
        (RunState::Unknown, "probe not configured".into())
    );
}

#[test]
fn only_the_three_read_only_commands_ever_run() {
    let mut rig = Rig::new(vec![
        run("a1", "box-1"),
        run("a2", "box-1"),
        run("a3", "box-2"),
    ]);
    rig.world().probe_delay_ms = STALE_LIST_MS + 1;
    rig.tick();
    rig.world()
        .probe_result
        .insert("box-2".into(), ProbeOutcome::BoxStopped);
    rig.world().exits.insert("a1".into(), 1);
    rig.world().list_fail = Some(GhFailure::RateLimited {
        retry_after_s: None,
    });
    rig.simulate_until(rig.now() + 3_600_000);
    rig.world().list_fail = Some(GhFailure::NotSignedIn);
    rig.simulate_until(rig.now() + 3_600_000);
    let world = rig.world();
    assert!(!world.calls.is_empty());
    for call in &world.calls {
        let allowed = call == "api user/codespaces"
            || call.starts_with("api user/codespaces/")
            || call.starts_with("codespace ssh ");
        assert!(allowed, "{call}");
        for banned in ["start", "stop", "delete", "create", "auth", "login"] {
            assert!(!call.contains(banned), "{call}");
        }
    }
    for script in &world.scripts {
        for banned in [
            "rm ",
            "kill",
            "start",
            "stop",
            "delete",
            "create",
            "auth",
            "new-session",
            "send-keys",
            "tee",
            "mv ",
        ] {
            assert!(!script.contains(banned), "{banned} in {script}");
        }
    }
}

#[test]
fn the_cache_is_written_on_change_only_and_never_more_than_every_thirty_seconds() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.tick();
    let writes = rig.core.counters.cache_writes;
    assert_eq!(writes, 1);
    assert!(rig.path.exists());
    for _ in 0..5 {
        rig.next();
    }
    assert_eq!(
        rig.core.counters.cache_writes, writes,
        "unchanged facts are not rewritten"
    );
    rig.world().exits.insert("a1".into(), 0);
    rig.until_probe();
    assert_eq!(
        rig.core.counters.cache_writes,
        writes + 1,
        "a transition flushes at once"
    );
}

#[test]
fn a_run_with_no_probe_data_is_unknown_not_orphaned() {
    let mut rig = Rig::new(vec![run("a1", "box-1")]);
    rig.world()
        .probe_result
        .insert("box-1".into(), ProbeOutcome::Output("done=1\n".into()));
    rig.tick();
    assert_eq!(rig.state("a1"), (RunState::Unknown, "probe failed".into()));
}

#[test]
fn fifty_boxes_and_ten_runs_cost_little_per_tick() {
    let runs: Vec<_> = (0..10)
        .map(|n| run(&format!("a{n}"), &format!("box-{n}")))
        .collect();
    let mut rig = Rig::new(runs);
    for n in 10..50 {
        rig.world().boxes.insert(format!("box-{n}"), available());
    }
    rig.tick();
    let started = std::time::Instant::now();
    for _ in 0..20 {
        rig.next();
    }
    let per_tick = started.elapsed() / 20;
    println!("50 boxes, 10 runs: {per_tick:?} per tick (fake gh, includes cache checks)");
    assert!(
        per_tick < std::time::Duration::from_millis(50),
        "{per_tick:?}"
    );
}

mod service {
    use super::*;
    use crate::external_runs::ExternalRuns;
    use crate::orchestrator_wakes::OrchestratorWakes;
    use crate::workspace_host::{WorkspaceHost, WorkspaceHostConfig};
    use crate::{DocHost, DocHostConfig, HarnessRegistry, RunJournal, SessionsEngine};
    use mast_proto::{HarnessId, WakeSettings, WorkOutlineMutation};
    use mast_sync::DocsStore;

    struct World2 {
        workspace: WorkspaceHost,
        wakes: OrchestratorWakes,
        runs: ExternalRuns,
        fake: Arc<Fake>,
        clock: Arc<AtomicI64>,
        path: PathBuf,
        run_id: String,
        item: String,
        _dir: tempfile::TempDir,
    }

    fn core_for(fake: &Arc<Fake>, clock: &Arc<AtomicI64>, path: &std::path::Path) -> PollerCore {
        let clock = clock.clone();
        PollerCore::new(
            fake.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
            path.to_path_buf(),
            Box::new(|| 0.0),
        )
    }

    async fn world() -> World2 {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let workspace = WorkspaceHost::open(
            store.clone(),
            WorkspaceHostConfig {
                device_id: "device".into(),
                device_name: "test".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        workspace
            .create_space("space", "device", "/tmp/space", None, false)
            .unwrap();
        for (id, parent) in [("orch", None), ("worker", Some("orch"))] {
            workspace
                .create_chat_with_parent(
                    id,
                    Some("space"),
                    Some("device"),
                    None,
                    None,
                    parent.map(str::to_owned),
                )
                .unwrap();
        }
        let actor = mast_proto::WorkOutlineActor {
            kind: mast_proto::WorkOutlineActorKind::System,
            device_id: "device".into(),
            chat_id: None,
            message_id: None,
        };
        let mutate = |mutation: WorkOutlineMutation| {
            workspace
                .work_outline_mutate(&mutation, &actor, &[])
                .unwrap()
        };
        let item = mutate(WorkOutlineMutation::CreateWorkItem(
            mast_proto::CreateWorkItemParams {
                space_id: "space".into(),
                parent_work_item_id: None,
                placement: Default::default(),
                title: "Feature".into(),
                description: None,
                status: None,
                operation_id: "item".into(),
            },
        ))
        .work_item_id
        .unwrap();
        mutate(WorkOutlineMutation::AssignChatToWorkItem(
            mast_proto::AssignChatToWorkItemParams {
                chat_id: "worker".into(),
                work_item_id: item.clone(),
                operation_id: "assign".into(),
            },
        ));
        mutate(WorkOutlineMutation::SetExternalRunSettings(
            mast_proto::SetExternalRunSettingsParams {
                space_id: "space".into(),
                settings: settings(),
                operation_id: "settings".into(),
            },
        ));
        mutate(WorkOutlineMutation::UpsertExternalRun(
            mast_proto::UpsertExternalRunParams {
                work_item_id: item.clone(),
                run_tag: "tag-1".into(),
                codespace_name: Some("box-1".into()),
                owner_chat_id: Some("worker".into()),
                expected_alive: Some(true),
                operation_id: "run".into(),
                ..Default::default()
            },
        ));
        let run_id = mast_proto::external_run_id(&item, "tag-1");
        let sessions = SessionsEngine::new(
            "device".into(),
            Arc::new(RunJournal::open(dir.path().join("journal")).unwrap()),
            Arc::new(HarnessRegistry::new()),
        );
        let doc_host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let wakes = OrchestratorWakes::new(
            sessions,
            doc_host,
            workspace.clone(),
            None,
            Arc::new(|| 1_000_000),
        );
        wakes.ingest_chats(&workspace.read_chats().unwrap()).await;
        wakes.set_settings("orch", WakeSettings::default()).await;
        let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
        let fake = Arc::new(Fake {
            world: Mutex::new(World::default()),
            clock: clock.clone(),
            gate: Mutex::new(None),
        });
        fake.world
            .lock()
            .unwrap()
            .boxes
            .insert("box-1".into(), available());
        let path = dir.path().join("cache/external-runs-observed.json");
        let runs = ExternalRuns::new(path.clone(), workspace.clone(), wakes.clone());
        runs.start_with_core(core_for(&fake, &clock, &path));
        World2 {
            workspace,
            wakes,
            runs,
            fake,
            clock,
            path,
            run_id,
            item,
            _dir: dir,
        }
    }

    impl World2 {
        fn stop_run(&self) {
            let actor = mast_proto::WorkOutlineActor {
                kind: mast_proto::WorkOutlineActorKind::System,
                device_id: "device".into(),
                chat_id: None,
                message_id: None,
            };
            self.workspace
                .work_outline_mutate(
                    &WorkOutlineMutation::UpsertExternalRun(mast_proto::UpsertExternalRunParams {
                        work_item_id: self.item.clone(),
                        run_tag: "tag-1".into(),
                        expected_alive: Some(false),
                        operation_id: "stop".into(),
                        ..Default::default()
                    }),
                    &actor,
                    &[],
                )
                .unwrap();
        }

        fn calls(&self, prefix: &str) -> usize {
            let world = self.fake.world.lock().unwrap();
            world
                .calls
                .iter()
                .filter(|call| call.starts_with(prefix))
                .count()
        }

        fn step(&self) -> Vec<WakeCandidate> {
            let now = self.clock.fetch_add(61_000, Ordering::SeqCst) + 61_000;
            self.runs.tick_now(now)
        }

        fn held(&self) -> Vec<String> {
            self.wakes
                .status("orch")
                .held
                .into_iter()
                .map(|e| e.summary)
                .collect()
        }
    }

    #[tokio::test]
    async fn no_tick_writes_the_registry() {
        let w = world().await;
        let marker = w.workspace.work_outline_inputs_marker("space");
        let rows = serde_json::to_string(&w.workspace.work_outline_inputs("space").runs).unwrap();
        w.fake
            .world
            .lock()
            .unwrap()
            .exits
            .insert(w.run_id.clone(), 0);
        let mut wakes = Vec::new();
        for _ in 0..8 {
            wakes.extend(w.step());
        }
        w.runs.deliver_for_test(wakes).await;
        assert_eq!(w.workspace.work_outline_inputs_marker("space"), marker);
        assert_eq!(
            serde_json::to_string(&w.workspace.work_outline_inputs("space").runs).unwrap(),
            rows
        );
        let observed = w.runs.observed("space");
        assert_eq!(observed.runs[&w.run_id].state, RunState::Done);
        assert_eq!(observed.capacity.unwrap().cap, 5);
    }

    #[tokio::test]
    async fn a_finished_run_wakes_its_orchestrator_once_across_a_restart() {
        let w = world().await;
        w.step();
        w.fake
            .world
            .lock()
            .unwrap()
            .exits
            .insert(w.run_id.clone(), 4);
        let mut wakes = Vec::new();
        for _ in 0..8 {
            wakes.extend(w.step());
        }
        assert_eq!(wakes.len(), 1);
        w.runs.deliver_for_test(wakes.clone()).await;
        w.runs.deliver_for_test(wakes).await;
        let held = w.held();
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(
            held[0].contains("failed: exit 4") && held[0].contains("Feature"),
            "{held:?}"
        );

        w.runs.start_with_core(core_for(&w.fake, &w.clock, &w.path));
        assert!(w.step().is_empty());
        assert!(w.step().is_empty());
        assert_eq!(w.held().len(), 1);
    }

    #[tokio::test]
    async fn the_master_switch_silences_it_and_the_transition_is_not_replayed() {
        let w = world().await;
        w.wakes
            .set_settings(
                "orch",
                WakeSettings {
                    enabled: false,
                    ..Default::default()
                },
            )
            .await;
        w.step();
        w.fake
            .world
            .lock()
            .unwrap()
            .exits
            .insert(w.run_id.clone(), 0);
        let mut wakes = Vec::new();
        for _ in 0..8 {
            wakes.extend(w.step());
        }
        w.runs.deliver_for_test(wakes).await;
        assert!(w.held().is_empty());
        w.wakes.set_settings("orch", WakeSettings::default()).await;
        assert!(w.step().is_empty());
    }

    #[tokio::test]
    async fn a_nudge_with_an_unchanged_view_does_not_bump() {
        let w = world().await;
        w.stop_run();
        w.fake.world.lock().unwrap().list_fail = Some(GhFailure::Unreachable("down".into()));
        w.runs.step_for_test(true).await;
        let revision = w.runs.revision();
        assert_eq!(revision, 1);
        assert_eq!(w.calls("api user/codespaces"), 1);
        for _ in 0..5 {
            w.runs.step_for_test(true).await;
        }
        assert_eq!(w.runs.revision(), revision);
        assert_eq!(w.calls("api user/codespaces"), 1);
        assert_eq!(
            w.runs.observed("space").gh_problem.as_deref(),
            Some("gh unreachable: down")
        );
    }

    #[tokio::test]
    async fn observed_answers_while_a_tick_is_blocked_in_the_runner() {
        let w = world().await;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *w.fake.gate.lock().unwrap() = Some((entered_tx, release_rx));
        let runs = w.runs.clone();
        let now = chrono::Utc::now().timestamp_millis();
        let ticking = tokio::task::spawn_blocking(move || runs.tick_now(now));
        let entered = tokio::task::spawn_blocking(move || {
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok()
        })
        .await
        .unwrap();
        assert!(entered, "the tick never reached the runner");
        let runs = w.runs.clone();
        let read = tokio::task::spawn_blocking(move || (runs.observed("space"), runs.counters()));
        let (view, _) = tokio::time::timeout(std::time::Duration::from_secs(5), read)
            .await
            .expect("observed waited for the tick")
            .unwrap();
        assert!(view.hosted);
        release_tx.send(()).unwrap();
        ticking.await.unwrap();
        assert_eq!(w.runs.counters().ticks, 1);
    }

    #[tokio::test]
    async fn unrelated_marker_bumps_with_no_live_runs_do_no_materialization() {
        let w = world().await;
        w.step();
        w.step();
        assert_eq!(w.runs.materializations(), 2, "a live run reads every time");
        w.stop_run();
        w.step();
        let settled = w.runs.materializations();
        assert_eq!(settled, 3);
        for archived in [true, false, true] {
            let before = w.workspace.work_outline_inputs_marker("space");
            w.workspace.set_chat_archived("worker", archived).unwrap();
            let after = w.workspace.work_outline_inputs_marker("space");
            assert_ne!(before, after);
            assert_eq!(before.0, after.0);
            w.step();
        }
        assert_eq!(w.runs.materializations(), settled);
    }

    #[tokio::test]
    async fn another_device_answers_not_hosted() {
        let w = world().await;
        w.workspace
            .create_space("elsewhere", "other-device", "/tmp/else", None, false)
            .unwrap();
        assert!(!w.runs.observed("elsewhere").hosted);
        assert!(w.runs.observed("space").hosted);
    }
}
