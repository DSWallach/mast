//! Tests run with a temporary store root and fake commands (`sh -c`, `sleep`); no Cargo, no
//! network. The detached supervisor is exercised for real: the test binary re-enters itself as
//! `build-job-supervise <id>` (see [`supervisor_entry`]).

#![allow(clippy::disallowed_methods)] // test repositories are built with plain `git`

use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// The local executor spawns `current_exe() build-job-supervise <id>`; in this test binary that
/// is the test harness, so intercept the argument before it parses its own.
#[ctor::ctor(unsafe)]
fn supervisor_entry() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("build-job-supervise") {
        let dir = std::env::var_os("MAST_BUILD_JOBS_DIR")
            .map(PathBuf::from)
            .expect("MAST_BUILD_JOBS_DIR");
        std::process::exit(supervise(&dir, &args[2]));
    }
}

struct FakeDisk(Mutex<f64>);
impl DiskProbe for FakeDisk {
    fn free_gib(&self, _: &HostProfile) -> Result<f64, String> {
        Ok(*self.0.lock().unwrap())
    }
}

#[derive(Default)]
struct RecordingWake {
    sent: Mutex<Vec<(String, String, String)>>,
    fail: AtomicBool,
    /// Every delivery attempt, failed or not.
    attempts: std::sync::atomic::AtomicUsize,
}
impl WakeSink for RecordingWake {
    fn deliver(&self, chat: &str, text: &str, id: &str) -> Result<(), String> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("chat busy".into());
        }
        self.sent
            .lock()
            .unwrap()
            .push((chat.into(), text.into(), id.into()));
        Ok(())
    }
}

#[derive(Default)]
struct FakeExecutor {
    started: Mutex<Vec<(String, String)>>,
    envs: Mutex<HashMap<String, Vec<(String, String)>>>,
    done: Mutex<HashMap<String, i32>>,
    stopped: Mutex<Vec<String>>,
    /// `(job id, attempt)` of every stop call, including failed ones.
    stop_calls: Mutex<Vec<(String, u32)>>,
    /// The next this many stop calls fail without signalling anything.
    stop_errors: std::sync::atomic::AtomicU32,
    busy: AtomicBool,
    /// Runs inside `start`, after the job was claimed (a cancel arriving in the start window).
    on_start: Mutex<Option<Box<dyn Fn() + Send>>>,
}
impl FakeExecutor {
    fn finish(&self, id: &str, code: i32) {
        self.done.lock().unwrap().insert(id.into(), code);
    }
    fn started_ids(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }
}
impl Executor for FakeExecutor {
    fn start(&self, ctx: &StartCtx<'_>) -> Result<Started, StartError> {
        if let Some(hook) = self.on_start.lock().unwrap().as_ref() {
            hook();
        }
        if self.busy.load(Ordering::SeqCst) {
            return Err(StartError::Busy("slot busy".into()));
        }
        self.started
            .lock()
            .unwrap()
            .push((ctx.job.id.clone(), ctx.slot.name.clone()));
        self.envs
            .lock()
            .unwrap()
            .insert(ctx.job.id.clone(), runner::slot_env(ctx.slot));
        Ok(Started {
            pid: Some(1),
            ..Started::default()
        })
    }
    fn probe(&self, job: &Job) -> Probe {
        match self.done.lock().unwrap().get(&job.id) {
            Some(code) => Probe::Exited(ExitRecord {
                version: 1,
                exit_code: Some(*code),
                ..Default::default()
            }),
            None => Probe::Running {
                waiting_for_lock: false,
            },
        }
    }
    fn stop(&self, job: &Job, attempt: u32) -> Result<(), String> {
        self.stop_calls
            .lock()
            .unwrap()
            .push((job.id.clone(), attempt));
        if job.pid.is_none() {
            return Err("job has no recorded pid (it may still be starting)".into());
        }
        if self
            .stop_errors
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("host unreachable".into());
        }
        self.stopped.lock().unwrap().push(job.id.clone());
        self.done.lock().unwrap().insert(job.id.clone(), 143);
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    jobs: BuildJobs,
    clock: Arc<AtomicI64>,
    disk: Arc<FakeDisk>,
    wake: Arc<RecordingWake>,
    exec: Arc<FakeExecutor>,
}

fn desktop(max_runners: usize) -> HostProfile {
    let mut profile = HostProfile::desktop("fakehost");
    profile.max_runners = max_runners;
    profile
}

impl Fixture {
    fn new(profile: HostProfile) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let exec = Arc::new(FakeExecutor::default());
        let (jobs, clock, disk, wake) = Self::build(
            dir.path(),
            profile,
            Some(exec.clone() as Arc<dyn Executor>),
            None,
        );
        Self {
            _dir: dir,
            jobs,
            clock,
            disk,
            wake,
            exec,
        }
    }

    fn build(
        root: &Path,
        profile: HostProfile,
        executor: Option<Arc<dyn Executor>>,
        clock: Option<Arc<AtomicI64>>,
    ) -> (BuildJobs, Arc<AtomicI64>, Arc<FakeDisk>, Arc<RecordingWake>) {
        let clock = clock.unwrap_or_else(|| Arc::new(AtomicI64::new(1_000_000)));
        let disk = Arc::new(FakeDisk(Mutex::new(500.0)));
        let wake = Arc::new(RecordingWake::default());
        let host = profile.id.clone();
        let cfg = BuildJobsConfig::new(root.to_path_buf(), vec![profile]);
        let tick_clock = clock.clone();
        let mut builder = BuildJobs::builder(cfg)
            .disk(disk.clone())
            .wake(wake.clone())
            .clock(move || tick_clock.load(Ordering::SeqCst));
        if let Some(executor) = executor {
            builder = builder.executor(&host, executor);
        }
        (builder.open().unwrap(), clock, disk, wake)
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    fn running(&self) -> Vec<Job> {
        self.jobs
            .list(&ListFilter {
                active_only: true,
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .filter(|job| job.state == JobState::Running)
            .collect()
    }
}

fn job(chat: &str, tree: &str, key: &str) -> NewJob {
    NewJob {
        op_id: format!("{chat}:{tree}:{key}"),
        chat_id: chat.into(),
        space_id: "space".into(),
        device_id: "device".into(),
        worktree_key: tree.into(),
        cwd: format!("/wt/{tree}"),
        head_sha: Some("abc123".into()),
        argv: ["cargo", "nextest", "run", key].map(String::from).to_vec(),
        wake: true,
        ..Default::default()
    }
}

// Slots, fairness, coalescing -------------------------------------------------------------------

#[test]
fn three_slots_run_independently_and_no_slot_has_two_owners() {
    let fx = Fixture::new(desktop(3));
    for n in 0..5 {
        fx.jobs
            .enqueue(job(&format!("chat{n}"), &format!("tree{n}"), "t"))
            .unwrap();
    }
    fx.jobs.tick();
    let started = fx.exec.started.lock().unwrap().clone();
    assert_eq!(started.len(), 3, "max_runners = 3");
    let mut slots: Vec<_> = started.iter().map(|(_, slot)| slot.clone()).collect();
    slots.sort();
    assert_eq!(slots, ["a", "b", "c"]);
    // The database refuses a second running owner for a taken slot.
    let queued = fx
        .jobs
        .list(&ListFilter {
            active_only: true,
            ..Default::default()
        })
        .unwrap();
    let waiting = queued
        .iter()
        .find(|job| job.state == JobState::Queued)
        .unwrap();
    assert!(
        !fx.jobs
            .inner
            .store
            .claim_running(&waiting.id, "b", "mastbj-x", 1)
            .unwrap()
    );
    // Worker jobs fill the worker slots first; slot a only after.
    let envs = fx.exec.envs.lock().unwrap();
    let on = |slot: &str| {
        let id = &started.iter().find(|(_, s)| s == slot).unwrap().0;
        envs[id].clone()
    };
    assert!(on("a").contains(&("CARGO_INCREMENTAL".into(), "1".into())));
    assert!(on("b").contains(&("CARGO_INCREMENTAL".into(), "0".into())));
    assert!(on("c").contains(&("CARGO_TARGET_DIR".into(), "Work/mast-target-c".into())));
}

#[test]
fn integration_job_runs_only_on_slot_a_and_workers_do_not_take_it_meanwhile() {
    let fx = Fixture::new(desktop(3));
    fx.jobs.enqueue(job("c1", "t1", "t")).unwrap();
    fx.jobs.enqueue(job("c2", "t2", "t")).unwrap();
    fx.jobs.tick();
    let mut integration = job("lead", "int", "t");
    integration.integration = true;
    let i = fx.jobs.enqueue(integration).unwrap().job;
    let w3 = fx.jobs.enqueue(job("c3", "t3", "t")).unwrap().job;
    fx.jobs.tick();
    let slot_of = |id: &str| {
        fx.exec
            .started
            .lock()
            .unwrap()
            .iter()
            .find(|(j, _)| j == id)
            .map(|(_, s)| s.clone())
    };
    assert_eq!(
        slot_of(&i.id).as_deref(),
        Some("a"),
        "integration runs on the incremental slot"
    );
    assert_eq!(
        slot_of(&w3.id),
        None,
        "no free worker slot, and a is taken by the integration job"
    );
    assert_eq!(fx.jobs.hold("desktop"), Some(Hold::RunnerCap));
}

#[test]
fn round_robin_across_worktrees_with_fifo_inside_one() {
    let fx = Fixture::new(desktop(1));
    let mut ids = Vec::new();
    for (chat, tree, key) in [
        ("a", "A", "1"),
        ("a2", "A", "2"),
        ("a3", "A", "3"),
        ("b", "B", "1"),
        ("c", "C", "1"),
    ] {
        ids.push((
            format!("{tree}{key}"),
            fx.jobs.enqueue(job(chat, tree, key)).unwrap().job.id,
        ));
    }
    let label = |id: &str| ids.iter().find(|(_, j)| j == id).unwrap().0.clone();
    for _ in 0..5 {
        fx.jobs.tick();
        let running = fx.running();
        assert_eq!(running.len(), 1);
        fx.exec.finish(&running[0].id, 0);
    }
    fx.jobs.tick();
    let order: Vec<String> = fx.exec.started_ids().iter().map(|id| label(id)).collect();
    // Oldest head first, except the worktree that just ran: A2 (queued 2nd) beats C1 (5th) once B1 ran.
    assert_eq!(order, ["A1", "B1", "A2", "C1", "A3"]);
}

#[test]
fn identical_queued_jobs_coalesce_and_wake_every_waiter() {
    let fx = Fixture::new(desktop(1));
    // Occupy the only runner so the twins stay queued.
    fx.jobs.enqueue(job("busy", "other", "x")).unwrap();
    fx.jobs.tick();
    let first = fx.jobs.enqueue(job("chat-1", "tree", "t")).unwrap();
    let second = fx.jobs.enqueue(job("chat-2", "tree", "t")).unwrap();
    assert!(!first.coalesced && second.coalesced);
    assert_eq!(first.job.id, second.job.id);
    assert_eq!(fx.jobs.active_job_count("chat-2"), 1);
    let blocker = fx.running()[0].id.clone();
    fx.exec.finish(&blocker, 0);
    fx.jobs.tick();
    // The twin is running now: an identical job is new work, because the tree may have changed.
    let third = fx.jobs.enqueue(job("chat-3", "tree", "t")).unwrap();
    assert!(!third.coalesced && third.job.id != first.job.id);
    fx.exec.finish(&first.job.id, 1);
    fx.jobs.tick();
    fx.advance(10_000);
    fx.jobs.tick();
    let chats: Vec<String> = fx
        .wake
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(c, ..)| c.clone())
        .collect();
    assert!(chats.contains(&"chat-1".to_string()) && chats.contains(&"chat-2".to_string()));
}

#[test]
fn idempotency_key_replays_and_conflicts() {
    let fx = Fixture::new(desktop(1));
    let first = fx.jobs.enqueue(job("chat", "tree", "t")).unwrap();
    let replay = fx.jobs.enqueue(job("chat", "tree", "t")).unwrap();
    assert!(replay.replay);
    assert_eq!(first.job.id, replay.job.id);
    let mut other = job("chat", "tree", "t");
    other.argv.push("--release".into());
    assert!(matches!(
        fx.jobs.enqueue(other),
        Err(BuildJobError::OperationConflict(_))
    ));
}

#[test]
fn per_chat_limit_and_slot_env_are_enforced_at_enqueue() {
    let fx = Fixture::new(desktop(1));
    for n in 0..4 {
        fx.jobs.enqueue(job("chat", &format!("t{n}"), "t")).unwrap();
    }
    assert!(matches!(
        fx.jobs.enqueue(job("chat", "t9", "t")),
        Err(BuildJobError::Limit(_))
    ));
    let mut env = job("other", "t", "t");
    env.env.insert("CARGO_TARGET_DIR".into(), "/x".into());
    assert!(matches!(
        fx.jobs.enqueue(env),
        Err(BuildJobError::Invalid(_))
    ));
    let mut no_sha = job("other", "t", "t");
    no_sha.head_sha = None;
    assert!(matches!(
        fx.jobs.enqueue(no_sha),
        Err(BuildJobError::Invalid(_))
    ));
}

#[test]
fn awaiting_approval_does_not_run_until_approved() {
    let fx = Fixture::new(desktop(3));
    let mut pending = job("chat", "tree", "t");
    pending.awaiting_approval = true;
    let queued = fx.jobs.enqueue(pending).unwrap().job;
    fx.jobs.tick();
    assert!(fx.exec.started_ids().is_empty());
    assert_eq!(
        fx.jobs.active_job_count("chat"),
        1,
        "awaiting approval counts as active"
    );
    fx.jobs.approve(&queued.id).unwrap();
    fx.jobs.tick();
    assert_eq!(fx.exec.started_ids(), [queued.id]);
}

// Disk gates ------------------------------------------------------------------------------------

#[test]
fn disk_gates_50_start_30_override_15_hold_12_abort() {
    let fx = Fixture::new(desktop(3));
    *fx.disk.0.lock().unwrap() = 40.0;
    let plain = fx.jobs.enqueue(job("a", "A", "t")).unwrap().job;
    let mut project = job("b", "B", "t");
    project.start_free_gib = Some(30);
    let project = fx.jobs.enqueue(project).unwrap().job;
    fx.jobs.tick();
    // 40 GiB free: below the default 50, above the project override 30.
    assert_eq!(fx.exec.started_ids(), [project.id.clone()]);
    assert!(matches!(
        fx.jobs.hold("desktop"),
        Some(Hold::DiskBelowStart { need_gib: 50, .. })
    ));
    // Below the 15 GiB hold nothing starts, override or not.
    *fx.disk.0.lock().unwrap() = 14.0;
    let mut another = job("c", "C", "t");
    another.start_free_gib = Some(10);
    fx.jobs.enqueue(another).unwrap();
    fx.jobs.tick();
    assert_eq!(fx.exec.started_ids().len(), 1);
    assert!(matches!(
        fx.jobs.hold("desktop"),
        Some(Hold::DiskBelowHold { .. })
    ));
    // 12.5 GiB: the running job is kept; 11.5 GiB: its own process group is stopped.
    *fx.disk.0.lock().unwrap() = 12.5;
    fx.jobs.tick();
    assert!(fx.exec.stopped.lock().unwrap().is_empty());
    *fx.disk.0.lock().unwrap() = 11.5;
    fx.jobs.tick();
    assert_eq!(*fx.exec.stopped.lock().unwrap(), [project.id.clone()]);
    fx.jobs.tick();
    let ended = fx.jobs.get(&project.id).unwrap();
    assert_eq!(ended.state, JobState::Failed);
    assert!(ended.reason.unwrap().contains("abort gate"));
    // Back above 50 GiB the held job starts.
    *fx.disk.0.lock().unwrap() = 60.0;
    fx.jobs.tick();
    assert!(fx.exec.started_ids().contains(&plain.id));
}

// Quiet-window lease ----------------------------------------------------------------------------

#[test]
fn quiet_window_drains_holds_queued_jobs_and_releases() {
    let fx = Fixture::new(desktop(3));
    let running = fx.jobs.enqueue(job("a", "A", "t")).unwrap().job;
    fx.jobs.tick();
    let lease = fx.jobs.acquire_quiet_window("desktop", "bench").unwrap();
    assert!(!lease.granted);
    assert!(
        fx.jobs
            .acquire_quiet_window("desktop", "someone-else")
            .is_err()
    );
    let held = fx.jobs.enqueue(job("b", "B", "t")).unwrap().job;
    fx.jobs.tick();
    assert_eq!(
        fx.exec.started_ids(),
        [running.id.clone()],
        "queued jobs hold during the drain"
    );
    assert!(!fx.jobs.quiet_window("desktop").unwrap().unwrap().granted);
    // The running job finishes; the next tick grants the lease with the host empty.
    fx.exec.finish(&running.id, 0);
    fx.jobs.tick();
    assert!(fx.jobs.quiet_window("desktop").unwrap().unwrap().granted);
    assert_eq!(
        fx.exec.started_ids().len(),
        1,
        "still held while the benchmark runs"
    );
    assert_eq!(fx.jobs.hold("desktop"), Some(Hold::QuietWindow));
    // A job that carries the lease runs inside it; the rest keep waiting.
    let mut timing = job("bench", "T", "timing");
    timing.lease_id = Some(lease.lease_id.clone());
    let timing = fx.jobs.enqueue(timing).unwrap().job;
    fx.jobs.tick();
    assert_eq!(
        fx.exec.started_ids(),
        [running.id.clone(), timing.id.clone()]
    );
    // Release lets the held job go.
    assert!(
        fx.jobs
            .release_quiet_window("desktop", &lease.lease_id)
            .unwrap()
    );
    fx.jobs.tick();
    assert!(fx.exec.started_ids().contains(&held.id));
}

#[test]
fn quiet_window_lapses_after_its_ttl() {
    let fx = Fixture::new(desktop(3));
    fx.jobs.acquire_quiet_window("desktop", "bench").unwrap();
    let queued = fx.jobs.enqueue(job("a", "A", "t")).unwrap().job;
    fx.jobs.tick();
    assert!(fx.exec.started_ids().is_empty());
    fx.advance(fx.jobs.config().quiet_window_ttl_ms + 1);
    fx.jobs.tick();
    assert_eq!(fx.exec.started_ids(), [queued.id]);
}

#[test]
fn start_refused_as_busy_puts_the_job_back_in_the_queue() {
    let fx = Fixture::new(desktop(3));
    fx.exec.busy.store(true, Ordering::SeqCst);
    let queued = fx.jobs.enqueue(job("a", "A", "t")).unwrap().job;
    fx.jobs.tick();
    let after = fx.jobs.get(&queued.id).unwrap();
    assert_eq!(after.state, JobState::Queued);
    assert_eq!(after.reason.as_deref(), Some("slot busy"));
    assert_eq!(fx.jobs.position(&queued.id).unwrap(), 1);
    fx.exec.busy.store(false, Ordering::SeqCst);
    fx.jobs.tick();
    assert_eq!(fx.jobs.get(&queued.id).unwrap().state, JobState::Running);
}

#[test]
fn cancel_queued_and_running_and_chat_ownership() {
    let fx = Fixture::new(desktop(1));
    let running = fx.jobs.enqueue(job("owner", "A", "1")).unwrap().job;
    let queued = fx.jobs.enqueue(job("owner", "B", "2")).unwrap().job;
    fx.jobs.tick();
    assert!(fx.jobs.cancel(&queued.id, Some("intruder")).is_err());
    assert_eq!(
        fx.jobs.cancel(&queued.id, Some("owner")).unwrap().state,
        JobState::Cancelled
    );
    fx.jobs.cancel(&running.id, None).unwrap();
    assert_eq!(*fx.exec.stopped.lock().unwrap(), [running.id.clone()]);
    fx.jobs.tick();
    assert_eq!(fx.jobs.get(&running.id).unwrap().state, JobState::Cancelled);
    assert_eq!(fx.jobs.active_job_count("owner"), 0);
}

// Wakes -----------------------------------------------------------------------------------------

#[test]
fn completions_within_ten_seconds_batch_into_one_wake_under_2kb() {
    let fx = Fixture::new(desktop(3));
    let first = fx.jobs.enqueue(job("chat", "A", "1")).unwrap().job;
    let second = fx.jobs.enqueue(job("chat", "B", "2")).unwrap().job;
    fx.jobs.tick();
    for id in [&first.id, &second.id] {
        std::fs::write(
            &fx.jobs.get(id).unwrap().log_path,
            "error[E0308]: mismatched types\n".repeat(80),
        )
        .unwrap();
        fx.exec.finish(id, 101);
    }
    fx.jobs.tick();
    fx.advance(9_000);
    fx.jobs.tick();
    assert!(
        fx.wake.sent.lock().unwrap().is_empty(),
        "the window is still open"
    );
    fx.advance(1_000);
    fx.jobs.tick();
    fx.jobs.tick();
    let sent = fx.wake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "one message, delivered once");
    let (chat, text, _) = &sent[0];
    assert_eq!(chat, "chat");
    assert!(text.len() <= WAKE_MAX_BYTES, "{} bytes", text.len());
    assert!(text.contains(&first.id) && text.contains(&second.id) && text.contains("failed"));
    assert!(text.ends_with("Use build_log { jobId } for the full log."));
}

#[test]
fn a_failed_delivery_stays_pending_and_is_retried() {
    let fx = Fixture::new(desktop(3));
    let queued = fx.jobs.enqueue(job("chat", "A", "1")).unwrap().job;
    fx.jobs.tick();
    fx.exec.finish(&queued.id, 0);
    fx.jobs.tick();
    fx.advance(10_000);
    fx.wake.fail.store(true, Ordering::SeqCst);
    fx.jobs.tick();
    assert!(fx.wake.sent.lock().unwrap().is_empty());
    fx.wake.fail.store(false, Ordering::SeqCst);
    fx.jobs.tick();
    assert_eq!(fx.wake.sent.lock().unwrap().len(), 1);
}

#[test]
fn wake_is_not_requested_when_wake_is_false() {
    let fx = Fixture::new(desktop(3));
    let mut quiet = job("chat", "A", "1");
    quiet.wake = false;
    let queued = fx.jobs.enqueue(quiet).unwrap().job;
    fx.jobs.tick();
    fx.exec.finish(&queued.id, 0);
    fx.jobs.tick();
    fx.advance(60_000);
    fx.jobs.tick();
    assert!(fx.wake.sent.lock().unwrap().is_empty());
}

// Scheduler (pure) ------------------------------------------------------------------------------

#[test]
fn plan_keeps_the_integration_slot_for_integration_jobs() {
    let profile = desktop(3);
    let cand = |id: &str, tree: &str, integration: bool, seq: i64| Candidate {
        id: id.into(),
        seq,
        worktree_key: tree.into(),
        integration,
        start_free_gib: None,
        lease_id: None,
    };
    let candidates = [
        cand("w1", "A", false, 1),
        cand("w2", "B", false, 2),
        cand("w3", "C", false, 3),
        cand("i1", "D", true, 4),
    ];
    let plan = plan_starts(&profile, &candidates, &[], None, Ok(100.0), None);
    assert_eq!(
        plan.starts,
        [
            ("w1".into(), "b".into()),
            ("w2".into(), "c".into()),
            ("i1".into(), "a".into())
        ]
    );
    // w3 has no free worker slot and a is taken by the integration job; it waits at the runner cap.
    assert_eq!(plan.held, Some(Hold::RunnerCap));
}

#[test]
fn plan_holds_without_disk_information() {
    let candidate = Candidate {
        id: "j".into(),
        seq: 1,
        worktree_key: "A".into(),
        integration: false,
        start_free_gib: None,
        lease_id: None,
    };
    let plan = plan_starts(
        &desktop(3),
        &[candidate],
        &[],
        None,
        Err("ssh down".into()),
        None,
    );
    assert!(plan.starts.is_empty());
    assert_eq!(plan.held, Some(Hold::DiskUnknown("ssh down".into())));
}

// Real supervisor, restart recovery -------------------------------------------------------------

fn local_fixture() -> (tempfile::TempDir, BuildJobs, Arc<AtomicI64>) {
    let dir = tempfile::tempdir().unwrap();
    let probe = dir.path().to_path_buf();
    let (jobs, clock, ..) = Fixture::build(dir.path(), HostProfile::local(probe), None, None);
    (dir, jobs, clock)
}

fn local_job(chat: &str, script: &str) -> NewJob {
    NewJob {
        op_id: format!("{chat}:{script}"),
        chat_id: chat.into(),
        space_id: "space".into(),
        device_id: "device".into(),
        worktree_key: chat.into(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        argv: vec!["sh".into(), "-c".into(), script.into()],
        wake: true,
        ..Default::default()
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn wait_for_state(jobs: &BuildJobs, id: &str, state: JobState) -> Job {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        jobs.tick();
        let job = jobs.get(id).unwrap();
        if job.state == state {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}: job is {} ({:?}, exit {:?})",
            state.as_str(),
            job.state.as_str(),
            job.reason,
            job.exit_code
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn local_supervisor_runs_the_command_logs_and_reports_the_exit_code() {
    let (_dir, jobs, _) = local_fixture();
    let queued = jobs
        .enqueue(local_job("c", "echo hello-from-job; echo oops >&2; exit 3"))
        .unwrap()
        .job;
    let done = wait_for_state(&jobs, &queued.id, JobState::Failed);
    assert_eq!(done.exit_code, Some(3));
    let log = std::fs::read_to_string(&done.log_path).unwrap();
    assert!(
        log.contains("hello-from-job") && log.contains("oops"),
        "{log}"
    );
    assert!(done.pid.is_some() && done.proc_start_time.is_some());
    let exit_file = jobs.config().jobs_dir().join(format!("{}.exit", done.id));
    assert!(exit_file.exists(), "the supervisor leaves the exit record");
}

#[test]
fn local_job_cancel_terminates_the_command_group() {
    let (_dir, jobs, _) = local_fixture();
    let queued = jobs.enqueue(local_job("c", "sleep 60")).unwrap().job;
    wait_until("running", || {
        jobs.tick();
        jobs.get(&queued.id).unwrap().state == JobState::Running
    });
    jobs.cancel(&queued.id, None).unwrap();
    wait_for_state(&jobs, &queued.id, JobState::Cancelled);
}

#[test]
fn local_job_timeout_ends_timed_out() {
    let (_dir, jobs, _) = local_fixture();
    let mut slow = local_job("c", "sleep 60");
    slow.timeout_secs = Some(1);
    let queued = jobs.enqueue(slow).unwrap().job;
    let done = wait_for_state(&jobs, &queued.id, JobState::TimedOut);
    assert!(done.reason.unwrap().contains("timed out"));
}

#[test]
fn engine_restart_readopts_a_live_supervisor_and_finishes_from_its_exit_file() {
    let (dir, jobs, clock) = local_fixture();
    let queued = jobs
        .enqueue(local_job("c", "sleep 2; echo survived"))
        .unwrap()
        .job;
    wait_until("running", || {
        jobs.tick();
        jobs.get(&queued.id).unwrap().state == JobState::Running
    });
    drop(jobs); // the engine goes away; the detached supervisor keeps going
    let (restarted, ..) = Fixture::build(
        dir.path(),
        HostProfile::local(dir.path().to_path_buf()),
        None,
        Some(clock),
    );
    let recovered = restarted.recover().unwrap();
    assert_eq!(recovered.adopted, [queued.id.clone()], "{recovered:?}");
    let done = wait_for_state(&restarted, &queued.id, JobState::Succeeded);
    assert!(
        std::fs::read_to_string(&done.log_path)
            .unwrap()
            .contains("survived")
    );
}

#[test]
fn restart_finishes_a_job_whose_exit_file_arrived_while_the_engine_was_down() {
    let (dir, jobs, clock) = local_fixture();
    let queued = jobs.enqueue(local_job("c", "exit 0")).unwrap().job;
    wait_until("running", || {
        jobs.tick();
        jobs.get(&queued.id).unwrap().state == JobState::Running
    });
    let exit_file = jobs.config().jobs_dir().join(format!("{}.exit", queued.id));
    wait_until("exit file", || exit_file.exists());
    drop(jobs);
    let (restarted, ..) = Fixture::build(
        dir.path(),
        HostProfile::local(dir.path().to_path_buf()),
        None,
        Some(clock),
    );
    let recovered = restarted.recover().unwrap();
    assert_eq!(recovered.finished, [queued.id.clone()]);
    assert_eq!(
        restarted.get(&queued.id).unwrap().state,
        JobState::Succeeded
    );
}

#[test]
fn restart_marks_a_reused_pid_lost_and_never_signals_it() {
    let (dir, jobs, clock) = local_fixture();
    let queued = jobs.enqueue(local_job("c", "sleep 60")).unwrap().job;
    // Pretend the supervisor was this very (live) test process, recorded with another start time.
    let own = std::process::id() as i64;
    assert!(
        jobs.inner
            .store
            .claim_running(&queued.id, "local", "mastbj-x", 1)
            .unwrap()
    );
    jobs.inner
        .store
        .record_started(
            &queued.id,
            &Started {
                pid: Some(own),
                pgid: Some(own),
                proc_start_time: Some("1".into()),
                remote_name: None,
            },
        )
        .unwrap();
    drop(jobs);
    let (restarted, ..) = Fixture::build(
        dir.path(),
        HostProfile::local(dir.path().to_path_buf()),
        None,
        Some(clock),
    );
    let recovered = restarted.recover().unwrap();
    assert_eq!(recovered.lost, [queued.id.clone()]);
    let lost = restarted.get(&queued.id).unwrap();
    assert_eq!(lost.state, JobState::Lost);
    assert!(lost.reason.clone().unwrap().contains("reused"));
    // stop() refuses a pid whose start time does not match, so the process was never signalled.
    let executor = LocalExecutor::new(
        restarted.config().jobs_dir(),
        SupervisorCommand::current_exe(),
    );
    let mut stale = lost.clone();
    stale.proc_start_time = Some("1".into());
    assert!(executor.stop(&stale, 0).is_err());
}

#[test]
fn restart_marks_a_supervisor_that_died_without_an_exit_record_lost() {
    let (dir, jobs, clock) = local_fixture();
    let queued = jobs.enqueue(local_job("c", "sleep 60")).unwrap().job;
    wait_until("running", || {
        jobs.tick();
        jobs.get(&queued.id).unwrap().state == JobState::Running
    });
    let running = jobs.get(&queued.id).unwrap();
    let child_file = jobs
        .config()
        .jobs_dir()
        .join(format!("{}.child", queued.id));
    wait_until("child file", || child_file.exists());
    let child: i32 = std::fs::read_to_string(&child_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: kills the test's own supervisor and its command group.
    unsafe {
        libc::kill(running.pid.unwrap() as i32, libc::SIGKILL);
        libc::kill(-child, libc::SIGKILL);
    }
    wait_until("supervisor gone", || {
        runner::proc_start_time(running.pid.unwrap()).is_none()
    });
    drop(jobs);
    let (restarted, ..) = Fixture::build(
        dir.path(),
        HostProfile::local(dir.path().to_path_buf()),
        None,
        Some(clock),
    );
    let recovered = restarted.recover().unwrap();
    assert_eq!(recovered.lost, [queued.id.clone()]);
    assert!(
        restarted
            .get(&queued.id)
            .unwrap()
            .reason
            .unwrap()
            .contains("without an exit record")
    );
}

#[test]
fn an_exit_record_of_an_unknown_version_reads_as_lost() {
    let (dir, jobs, clock) = local_fixture();
    let queued = jobs.enqueue(local_job("c", "sleep 60")).unwrap().job;
    assert!(
        jobs.inner
            .store
            .claim_running(&queued.id, "local", "mastbj-x", 1)
            .unwrap()
    );
    std::fs::write(
        jobs.config().jobs_dir().join(format!("{}.exit", queued.id)),
        r#"{"version":99,"exitCode":0}"#,
    )
    .unwrap();
    drop(jobs);
    let (restarted, ..) = Fixture::build(
        dir.path(),
        HostProfile::local(dir.path().to_path_buf()),
        None,
        Some(clock),
    );
    restarted.recover().unwrap();
    let lost = restarted.get(&queued.id).unwrap();
    assert_eq!(lost.state, JobState::Lost);
    assert!(lost.reason.unwrap().contains("unknown version"));
}

#[test]
fn a_newer_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let c = rusqlite::Connection::open(dir.path().join("build-jobs.sqlite3")).unwrap();
    c.pragma_update(None, "user_version", 99).unwrap();
    drop(c);
    assert!(Store::open(dir.path()).is_err());
}

// Remote executor: adapter over scripts/remote-cargo.sh -------------------------------------------

#[derive(Default)]
struct FakeTransport {
    cargo_calls: Mutex<Vec<Vec<String>>>,
    ssh_calls: Mutex<Vec<(String, Vec<String>)>>,
    /// Replies keyed by the first argument of a remote-cargo call (`start`, `status`, ...).
    replies: Mutex<HashMap<String, (i32, String)>>,
    ensure_reply: Mutex<Option<(i32, String)>>,
}
impl FakeTransport {
    fn reply(&self, verb: &str, code: i32, stdout: &str) {
        self.replies
            .lock()
            .unwrap()
            .insert(verb.into(), (code, stdout.into()));
    }
}
impl RemoteTransport for FakeTransport {
    fn remote_cargo(&self, args: &[String]) -> Result<ScriptOutput, String> {
        self.cargo_calls.lock().unwrap().push(args.to_vec());
        let (code, stdout) = self
            .replies
            .lock()
            .unwrap()
            .get(&args[0])
            .cloned()
            .unwrap_or((0, String::new()));
        Ok(ScriptOutput {
            code,
            stdout,
            stderr: String::new(),
        })
    }
    fn ssh_sh(&self, ssh: &str, script: &str, args: &[String]) -> Result<ScriptOutput, String> {
        self.ssh_calls
            .lock()
            .unwrap()
            .push((ssh.into(), args.to_vec()));
        if script.contains("git -C") {
            let (code, stdout) = self
                .ensure_reply
                .lock()
                .unwrap()
                .clone()
                .unwrap_or((0, "ready".into()));
            return Ok(ScriptOutput {
                code,
                stdout,
                stderr: String::new(),
            });
        }
        Ok(ScriptOutput {
            code: 0,
            stdout: "log tail\nerror: boom\n".into(),
            stderr: String::new(),
        })
    }
}

fn remote_fixture(
    profile: HostProfile,
) -> (
    tempfile::TempDir,
    BuildJobs,
    Arc<FakeTransport>,
    Arc<AtomicI64>,
) {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(FakeTransport::default());
    let clock = Arc::new(AtomicI64::new(1_000_000));
    let tick_clock = clock.clone();
    let cfg = BuildJobsConfig::new(dir.path().to_path_buf(), vec![profile]);
    let jobs = BuildJobs::builder(cfg)
        .transport(transport.clone())
        .disk(Arc::new(FakeDisk(Mutex::new(500.0))))
        .wake(Arc::new(RecordingWake::default()))
        .clock(move || tick_clock.load(Ordering::SeqCst))
        .open()
        .unwrap();
    (dir, jobs, transport, clock)
}

#[test]
fn slot_checkout_is_materialized_only_when_the_job_is_dequeued() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply(
        "start",
        0,
        "started mastbj-x pid 4242 slot b log /tmp/mastbj-x.log\n",
    );
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    assert!(
        transport.ssh_calls.lock().unwrap().is_empty(),
        "nothing is provisioned at enqueue"
    );
    jobs.tick();
    let ssh = transport.ssh_calls.lock().unwrap().clone();
    let (_, ensure) = ssh
        .iter()
        .find(|(_, args)| args.len() == 6)
        .expect("checkout step");
    assert_eq!(
        ensure[..4],
        [
            "Work/mast".to_string(),
            "Work/mast-wt-build-b".into(),
            "abc123".into(),
            "b".into()
        ]
    );
    assert_eq!(
        ensure[4], "0",
        "worker slots build with CARGO_INCREMENTAL=0"
    );
    let calls = transport.cargo_calls.lock().unwrap().clone();
    let name = format!("mastbj-{}", queued.id);
    assert_eq!(
        calls[0],
        [
            "start",
            "--admit",
            "fakehost",
            "Work/mast-wt-build-b",
            "b",
            name.as_str(),
            "--",
            "cargo",
            "nextest",
            "run",
            "t"
        ]
    );
    let running = jobs.get(&queued.id).unwrap();
    assert_eq!(
        (running.state, running.pid, running.slot.as_deref()),
        (JobState::Running, Some(4242), Some("b"))
    );
}

#[test]
fn integration_slot_checkout_is_incremental_and_serial_mode_passes_the_device_serial() {
    let (_dir, jobs, transport, _) =
        remote_fixture(HostProfile::desktop_serial("fakehost", "desktop-1"));
    let mut int = job("lead", "int", "t");
    int.integration = true;
    jobs.enqueue(int).unwrap();
    jobs.tick();
    let ssh = transport.ssh_calls.lock().unwrap().clone();
    assert_eq!(ssh[0].1[1], "Work/mast-wt-build-a");
    assert_eq!(ssh[0].1[4], "1");
    let calls = transport.cargo_calls.lock().unwrap().clone();
    assert_eq!(
        calls[0][..3],
        [
            "start".to_string(),
            "--device-serial".into(),
            "desktop-1".into()
        ]
    );
}

#[test]
fn remote_probe_maps_every_script_status() {
    let (_dir, jobs, transport, clock) = remote_fixture(desktop(3));
    transport.reply("start", 0, "started x pid 7 slot b\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    transport.reply("status", 0, "running\n");
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Running);
    // An unreachable host keeps the job running instead of failing it.
    transport.reply("status", 255, "");
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Running);
    transport.reply("status", 0, "exit 101\n");
    transport.reply(
        "wait",
        1,
        "exit 101\nerror: boom\ntest result: FAILED. 1 passed; 1 failed\n",
    );
    jobs.tick();
    let failed = jobs.get(&queued.id).unwrap();
    assert_eq!(
        (failed.state, failed.exit_code),
        (JobState::Failed, Some(101))
    );
    assert!(failed.summary_json.unwrap().contains("test result: FAILED"));
    assert!(
        std::fs::read_to_string(&failed.log_path)
            .unwrap()
            .contains("error: boom")
    );
    let _ = clock;
}

#[test]
fn remote_died_and_unknown_jobs_are_lost() {
    for (status, expected) in [
        ("died", "without an exit marker"),
        ("unknown", "no longer knows"),
    ] {
        let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
        let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
        jobs.tick();
        transport.reply("status", 1, status);
        jobs.tick();
        let lost = jobs.get(&queued.id).unwrap();
        assert_eq!(lost.state, JobState::Lost);
        assert!(lost.reason.unwrap().contains(expected));
    }
}

#[test]
fn remote_busy_slot_requeues_and_a_missing_commit_fails_the_job() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 1, "slot b busy: someone\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Queued);
    *transport.ensure_reply.lock().unwrap() = Some((
        4,
        "commit abc123 is not on this host; push the branch to it first".into(),
    ));
    jobs.tick();
    let failed = jobs.get(&queued.id).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.reason.unwrap().contains("not on this host"));
}

#[test]
fn only_cargo_runs_on_a_remote_host() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    let mut shell = job("chat", "A", "t");
    shell.argv = vec!["sh".into(), "-c".into(), "id".into()];
    let queued = jobs.enqueue(shell).unwrap().job;
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Failed);
    assert!(transport.cargo_calls.lock().unwrap().is_empty());
}

#[test]
fn remote_stop_goes_through_remote_cargo_stop() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    jobs.cancel(&queued.id, None).unwrap();
    let calls = transport.cargo_calls.lock().unwrap().clone();
    assert_eq!(calls.last().unwrap()[0], "stop");
}

#[test]
fn lease_jobs_start_with_quiet_window_and_admission_refusals_requeue_but_policy_refusals_fail() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    let lease = jobs.acquire_quiet_window("desktop", "bench").unwrap();
    let mut timing = job("bench", "T", "timing");
    timing.lease_id = Some(lease.lease_id.clone());
    jobs.enqueue(timing).unwrap();
    jobs.tick();
    assert_eq!(
        transport.cargo_calls.lock().unwrap()[0][..2],
        ["start", "--quiet-window"]
    );

    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 1, "refused: 3 job(s) already run, limit 3\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Queued);
    transport.reply(
        "start",
        1,
        "refused: slot b must run in /home/x/Work/mast-wt-build-b, not y\n",
    );
    jobs.tick();
    let failed = jobs.get(&queued.id).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.reason.unwrap().contains("must run in"));
}

/// A throwaway build host for the real `scripts/remote-cargo.sh`: `ssh` is a shell that runs the
/// command locally, `$HOME` is a temp dir, `cargo` is a fake (it blocks while `$HOME/hold`
/// exists) and the slot name and every lock path are the test's own, so a real host's jobs are
/// never counted or blocked.
/// Run `git` in the throwaway host's repository (`$HOME/Work/mast`).
fn git_in(home: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
        .arg(home.join("Work/mast"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct RealHost {
    _dir: tempfile::TempDir,
    home: PathBuf,
    slot: String,
    sha: String,
    transport: Arc<ProcessTransport>,
    jobs: BuildJobs,
}

impl RealHost {
    /// `None` only when `MAST_SKIP_REAL_SCRIPT_TEST` is set and a prerequisite is missing, with a
    /// loud message; otherwise a missing prerequisite fails the test instead of passing it.
    fn new() -> Option<Self> {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/remote-cargo.sh");
        let have = |tool: &str| {
            std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(tool).exists()))
        };
        let missing: Vec<&str> = ["flock", "setsid", "git"]
            .into_iter()
            .filter(|tool| !have(tool))
            .chain((!script.exists()).then_some("scripts/remote-cargo.sh"))
            .collect();
        if !missing.is_empty() {
            if std::env::var_os("MAST_SKIP_REAL_SCRIPT_TEST").is_some() {
                eprintln!("SKIPPED real remote-cargo.sh test: missing {missing:?}");
                return None;
            }
            panic!(
                "the real remote-cargo.sh tests need {missing:?}; set MAST_SKIP_REAL_SCRIPT_TEST=1 to skip them knowingly"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join("Work/mast")).unwrap();
        let write_exec = |path: PathBuf, body: &str| {
            std::fs::write(&path, body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_exec(
            bin.join("ssh"),
            "#!/bin/sh\nwhile [ \"$1\" = -o ]; do shift 2; done\nshift\ncd \"$HOME\" || exit 255\nexec sh -c \"$*\"\n",
        );
        // The script refuses `--admit` below 15 GiB free on $HOME; a throwaway HOME on tmpfs has less.
        write_exec(
            bin.join("df"),
            "#!/bin/sh\necho 'Filesystem 1024-blocks Used Available Capacity Mounted on'\necho 'fake 999999999 1 999999999 1% /'\n",
        );
        write_exec(
            bin.join("cargo"),
            &format!(
                "#!/bin/sh\necho \"$CARGO_INCREMENTAL $CARGO_TARGET_DIR $*\" > {home}/cargo-ran\nwhile [ -e {home}/hold ]; do sleep 0.1; done\n",
                home = home.display()
            ),
        );
        git_in(&home, &["init", "-q"]);
        git_in(&home, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let sha = git_in(&home, &["rev-parse", "HEAD"]);
        let slot = format!("zt{}", std::process::id());
        let mut profile = HostProfile::desktop("fakehost");
        profile.slots = vec![SlotConfig {
            name: slot.clone(),
            incremental: false,
            target_dir: None,
        }];
        profile.max_runners = 1;
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        // `--admit` runs only in the configured slots.
        std::fs::write(
            home.join("Work/mast-remote-cargo.conf"),
            format!("REMOTE_CARGO_SLOTS=\"{slot}\"\nREMOTE_CARGO_MIN_MEM_KIB=1\n"),
        )
        .unwrap();
        let lock = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        let transport = Arc::new(
            ProcessTransport::new(script)
                .with_ssh(bin.join("ssh"))
                .with_env(vec![
                    ("HOME".into(), home.to_string_lossy().into_owned()),
                    ("PATH".into(), path),
                    ("REMOTE_CARGO_SLOT_PREFIX".into(), lock("slot-")),
                    ("REMOTE_CARGO_QUIET_LOCK".into(), lock("quiet.lock")),
                    ("REMOTE_CARGO_ADMIT_LOCK".into(), lock("admit.lock")),
                    ("REMOTE_CARGO_DEVICE_LOCK".into(), lock("device.lock")),
                ]),
        );
        let cfg = BuildJobsConfig::new(dir.path().join("store"), vec![profile]);
        std::fs::create_dir_all(&cfg.store_root).unwrap();
        let jobs = BuildJobs::builder(cfg)
            .transport(transport.clone())
            .disk(Arc::new(FakeDisk(Mutex::new(500.0))))
            .open()
            .unwrap();
        Some(Self {
            _dir: dir,
            home,
            slot,
            sha,
            transport,
            jobs,
        })
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.home, args)
    }

    fn checkout(&self) -> PathBuf {
        self.home.join(format!("Work/mast-wt-build-{}", self.slot))
    }

    fn checkout_head(&self) -> String {
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(self.checkout())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&head.stdout).trim().to_string()
    }

    fn job(&self) -> NewJob {
        let mut new = job("chat", "A", "t");
        new.head_sha = Some(self.sha.clone());
        new.argv = vec!["cargo".into(), "--version".into()];
        new
    }

    /// Run the checkout step by hand, as `RemoteExecutor::start` does.
    fn ensure(&self, sha: &str, name: &str) -> ScriptOutput {
        self.transport
            .ssh_sh(
                "fakehost",
                remote_executor::ENSURE_CHECKOUT,
                &[
                    "Work/mast".into(),
                    format!("Work/mast-wt-build-{}", self.slot),
                    sha.into(),
                    self.slot.clone(),
                    "0".into(),
                    format!("Work/mast-target-{}", self.slot),
                    name.into(),
                ],
            )
            .unwrap()
    }
}

impl Drop for RealHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.home.join("hold"));
        for job in self.jobs.list(&ListFilter::default()).unwrap_or_default() {
            if let Some(name) = job.remote_name {
                for ext in ["pid", "start", "claim", "exit", "log"] {
                    let _ = std::fs::remove_file(format!("/tmp/{name}.{ext}"));
                }
            }
        }
    }
}

#[test]
fn real_remote_cargo_script_runs_a_job_end_to_end_with_a_fake_ssh() {
    let Some(host) = RealHost::new() else { return };
    let queued = host.jobs.enqueue(host.job()).unwrap().job;
    assert!(!host.checkout().exists(), "lazy: no checkout while queued");
    let done = wait_for_state(&host.jobs, &queued.id, JobState::Succeeded);
    assert!(
        host.checkout().join(".git").exists(),
        "checkout created when the job was dequeued"
    );
    assert_eq!(host.checkout_head(), host.sha);
    let env_file =
        std::fs::read_to_string(host.home.join(format!("Work/mast-slot-{}.env", host.slot)))
            .unwrap();
    assert!(
        env_file.contains("CARGO_INCREMENTAL=0")
            && env_file.contains(&format!("Work/mast-target-{}", host.slot)),
        "{env_file}"
    );
    let ran = std::fs::read_to_string(host.home.join("cargo-ran")).unwrap();
    assert!(ran.starts_with("0 ") && ran.contains("--version"), "{ran}");
    assert_eq!(done.exit_code, Some(0));
}

#[test]
fn checkout_is_never_moved_under_a_running_job_on_its_slot() {
    let Some(host) = RealHost::new() else { return };
    std::fs::write(host.home.join("hold"), "").unwrap();
    let queued = host.jobs.enqueue(host.job()).unwrap().job;
    wait_until("running", || {
        host.jobs.tick();
        host.jobs.get(&queued.id).unwrap().state == JobState::Running
    });
    wait_until("cargo started", || host.home.join("cargo-ran").exists());
    host.git(&["commit", "-q", "--allow-empty", "-m", "second"]);
    let second = host.git(&["rev-parse", "HEAD"]);
    // Another job's checkout is refused while the slot's job is alive, and nothing moves.
    let busy = host.ensure(&second, "mastbj-someone-else");
    assert_eq!(busy.code, 7, "{busy:?}");
    assert!(busy.stdout.contains("busy"), "{busy:?}");
    assert_eq!(host.checkout_head(), host.sha);
    // The running job's own retry adopts it and touches nothing.
    let own = host.ensure(&second, queued.remote_name.as_deref().unwrap());
    assert_eq!(own.code, 0, "{own:?}");
    assert!(own.stdout.contains("already running"), "{own:?}");
    assert_eq!(host.checkout_head(), host.sha);
    // Once the job ends the slot is idle and the checkout may move.
    std::fs::remove_file(host.home.join("hold")).unwrap();
    wait_for_state(&host.jobs, &queued.id, JobState::Succeeded);
    let idle = host.ensure(&second, "mastbj-someone-else");
    assert_eq!(idle.code, 0, "{idle:?}");
    assert_eq!(host.checkout_head(), second);
}

// Stop retry, cancel during the start window ----------------------------------------------------

#[test]
fn a_failed_stop_is_retried_with_backoff_and_escalates_on_the_third_attempt() {
    let fx = Fixture::new(desktop(3));
    let queued = fx.jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    fx.jobs.tick();
    fx.exec.stop_errors.store(2, Ordering::SeqCst);
    fx.jobs.cancel(&queued.id, None).unwrap();
    let calls = |fx: &Fixture| fx.exec.stop_calls.lock().unwrap().clone();
    assert_eq!(calls(&fx), [(queued.id.clone(), 0)]);
    fx.jobs.tick();
    assert_eq!(calls(&fx).len(), 1, "not due before the 5 s backoff");
    fx.advance(5_000);
    fx.jobs.tick();
    assert_eq!(calls(&fx).len(), 2);
    fx.advance(5_000);
    fx.jobs.tick();
    assert_eq!(calls(&fx).len(), 2, "the second backoff is 10 s");
    fx.advance(5_000);
    fx.jobs.tick();
    assert_eq!(
        calls(&fx),
        [
            (queued.id.clone(), 0),
            (queued.id.clone(), 1),
            (queued.id.clone(), 2)
        ],
        "attempt 2 is the escalation"
    );
    fx.jobs.tick();
    assert_eq!(fx.jobs.get(&queued.id).unwrap().state, JobState::Cancelled);
}

#[test]
fn cancel_before_the_start_is_recorded_is_applied_once_the_job_has_a_pid() {
    let fx = Fixture::new(desktop(3));
    let queued = fx.jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    // The job is claimed `running` but the start has not returned yet: no pid to stop.
    assert!(
        fx.jobs
            .inner
            .store
            .claim_running(&queued.id, "b", "mastbj-x", 1)
            .unwrap()
    );
    fx.jobs.cancel(&queued.id, None).unwrap();
    assert_eq!(fx.jobs.get(&queued.id).unwrap().state, JobState::Running);
    assert!(
        fx.exec.stopped.lock().unwrap().is_empty(),
        "nothing was signalled"
    );
    fx.jobs
        .inner
        .store
        .record_started(
            &queued.id,
            &Started {
                pid: Some(1),
                ..Started::default()
            },
        )
        .unwrap();
    fx.advance(5_000);
    fx.jobs.tick();
    assert_eq!(*fx.exec.stopped.lock().unwrap(), [queued.id.clone()]);
    fx.jobs.tick();
    assert_eq!(fx.jobs.get(&queued.id).unwrap().state, JobState::Cancelled);
}

#[test]
fn a_cancel_during_a_start_that_ends_busy_is_not_requeued() {
    let fx = Fixture::new(desktop(3));
    let queued = fx.jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    fx.exec.busy.store(true, Ordering::SeqCst);
    let jobs = fx.jobs.clone();
    let id = queued.id.clone();
    *fx.exec.on_start.lock().unwrap() = Some(Box::new(move || {
        jobs.cancel(&id, None).unwrap();
    }));
    fx.jobs.tick();
    assert_eq!(fx.jobs.get(&queued.id).unwrap().state, JobState::Cancelled);
}

// Remote: ambiguous starts, timeouts, stop codes --------------------------------------------------

#[test]
fn a_lost_start_answer_adopts_a_running_job_requeues_an_unknown_one_and_settles_later() {
    // The answer was lost but the host runs the job: adopt it, never run it twice.
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 255, "");
    transport.reply("status", 0, "running\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    let running = jobs.get(&queued.id).unwrap();
    assert_eq!(running.state, JobState::Running);
    assert_eq!(running.remote_name, Some(format!("mastbj-{}", queued.id)));

    // The host has never heard of it: the start did not happen, so it is queued again.
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 255, "");
    transport.reply("status", 0, "unknown\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Queued);

    // Host unreachable for both calls: claimed without a pid, settled by a later probe.
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 255, "");
    transport.reply("status", 255, "");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    let claimed = jobs.get(&queued.id).unwrap();
    assert_eq!((claimed.state, claimed.pid), (JobState::Running, None));
    // The host answers "unknown" for a job whose start was never confirmed: it goes back to the
    // queue (the scheduler may try it again at once, here into a busy-slot refusal) instead of
    // ending `lost`.
    transport.reply("status", 0, "unknown\n");
    transport.reply("start", 1, "slot b busy: mastbj-other\n");
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Queued);
}

#[test]
fn name_in_use_is_adopted_only_when_the_host_shows_the_job_running_or_finished() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 1, "name in use: mastbj-x\n");
    transport.reply("status", 0, "running\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    assert_eq!(jobs.get(&queued.id).unwrap().state, JobState::Running);

    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    transport.reply("start", 1, "name in use: mastbj-x\n");
    transport.reply("status", 1, "died\n");
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    let failed = jobs.get(&queued.id).unwrap();
    assert_eq!(
        failed.state,
        JobState::Failed,
        "a dead name is not a started job"
    );
    assert!(failed.reason.unwrap().contains("already in use"));
}

#[test]
fn a_busy_slot_refusal_of_the_checkout_step_requeues_without_starting() {
    let (_dir, jobs, transport, _) = remote_fixture(desktop(3));
    *transport.ensure_reply.lock().unwrap() = Some((7, "slot b busy: mastbj-other".into()));
    let queued = jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    jobs.tick();
    let after = jobs.get(&queued.id).unwrap();
    assert_eq!(after.state, JobState::Queued);
    assert!(after.reason.unwrap().contains("busy"));
    assert!(
        transport.cargo_calls.lock().unwrap().is_empty(),
        "start was never attempted"
    );
}

#[test]
fn remote_timeout_stops_the_job_and_a_stop_the_host_does_not_know_yet_is_retried() {
    let (_dir, jobs, transport, clock) = remote_fixture(desktop(3));
    let mut new = job("chat", "A", "t");
    new.timeout_secs = Some(10);
    let queued = jobs.enqueue(new).unwrap().job;
    jobs.tick();
    transport.reply("status", 0, "running\n");
    transport.reply("stop", 2, "unknown job\n");
    let stops = || {
        transport
            .cargo_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call[0] == "stop")
            .count()
    };
    clock.fetch_add(11_000, Ordering::SeqCst);
    jobs.tick();
    assert_eq!(stops(), 1, "the timeout asks the host to stop it");
    jobs.tick();
    assert_eq!(
        stops(),
        1,
        "exit 2 is a failed stop, retried only after the backoff"
    );
    clock.fetch_add(5_000, Ordering::SeqCst);
    transport.reply("stop", 0, "stopped: exit 143\n");
    jobs.tick();
    assert_eq!(stops(), 2);
    transport.reply("status", 0, "exit 143\n");
    jobs.tick();
    let ended = jobs.get(&queued.id).unwrap();
    assert_eq!(ended.state, JobState::TimedOut);
    assert!(ended.reason.unwrap().contains("timed out after 10 s"));
}

#[test]
fn a_remote_job_cannot_carry_env_and_only_ssh_aliases_are_accepted() {
    let (_dir, jobs, _, _) = remote_fixture(desktop(3));
    let mut with_env = job("chat", "A", "t");
    with_env.env.insert("RUST_LOG".into(), "debug".into());
    assert!(matches!(
        jobs.enqueue(with_env),
        Err(BuildJobError::Invalid(_))
    ));

    for ok in ["desktop", "fedora-desktop", "build.host_1"] {
        assert_eq!(validate_ssh_alias(ok), Ok(()), "{ok}");
    }
    for bad in [
        "david@100.105.54.55",
        "100.105.54.55",
        "-oProxyCommand=x",
        "",
        "host:22",
        "a b",
        "user@desktop",
    ] {
        assert!(validate_ssh_alias(bad).is_err(), "{bad:?} must be refused");
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = BuildJobsConfig::new(
        dir.path().to_path_buf(),
        vec![HostProfile::desktop("david@100.105.54.55")],
    );
    assert!(matches!(
        BuildJobs::builder(cfg).open(),
        Err(BuildJobError::Invalid(_))
    ));
}

// Disk reserve, wake backoff ----------------------------------------------------------------------

#[test]
fn starts_planned_from_one_free_space_reading_each_reserve_10_gib() {
    let candidate = |id: &str, tree: &str, seq: i64| Candidate {
        id: id.into(),
        seq,
        worktree_key: tree.into(),
        integration: false,
        start_free_gib: None,
        lease_id: None,
    };
    let candidates = [
        candidate("j1", "A", 1),
        candidate("j2", "B", 2),
        candidate("j3", "C", 3),
    ];
    // 62 GiB: j1 sees 62, j2 sees 52, j3 sees 42 (< 50): only two start.
    let plan = plan_starts(&desktop(3), &candidates, &[], None, Ok(62.0), None);
    assert_eq!(plan.starts.len(), 2);
    assert!(matches!(
        plan.held,
        Some(Hold::DiskBelowStart { need_gib: 50, .. })
    ));
    // 90 GiB: all three start.
    let plan = plan_starts(&desktop(3), &candidates, &[], None, Ok(90.0), None);
    assert_eq!(plan.starts.len(), 3);
    // 30 GiB with a project override of 30: one start (30), the next would see 20.
    let mut low = candidates.clone();
    low.iter_mut().for_each(|c| c.start_free_gib = Some(30));
    let plan = plan_starts(&desktop(3), &low, &[], None, Ok(30.0), None);
    assert_eq!(plan.starts.len(), 1);
}

#[test]
fn a_failed_wake_is_retried_with_exponential_backoff_not_every_tick() {
    let fx = Fixture::new(desktop(3));
    let queued = fx.jobs.enqueue(job("chat", "A", "t")).unwrap().job;
    fx.jobs.tick();
    fx.exec.finish(&queued.id, 0);
    fx.jobs.tick();
    fx.advance(10_000);
    fx.wake.fail.store(true, Ordering::SeqCst);
    let attempts = |fx: &Fixture| fx.wake.attempts.load(Ordering::SeqCst);
    fx.jobs.tick();
    fx.jobs.tick();
    assert_eq!(attempts(&fx), 1, "the same tick instant does not retry");
    fx.advance(1_000);
    fx.jobs.tick();
    assert_eq!(attempts(&fx), 2, "1 s after the first failure");
    fx.advance(1_000);
    fx.jobs.tick();
    assert_eq!(attempts(&fx), 2, "the next backoff is 2 s");
    fx.advance(1_000);
    fx.wake.fail.store(false, Ordering::SeqCst);
    fx.jobs.tick();
    assert_eq!(attempts(&fx), 3);
    assert_eq!(fx.wake.sent.lock().unwrap().len(), 1);
}
