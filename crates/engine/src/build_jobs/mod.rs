//! Durable shared build queue (`build-jobs.sqlite3`, W3-T1).
//!
//! The queue owns *waiting*; Cargo's own file lock stays the only thing that guarantees
//! correctness. A job never changes `CARGO_TARGET_DIR` semantics, never cleans or sweeps, and only
//! ever signals process groups it started itself.
//!
//! # Pieces
//!
//! - [`store`]: SQLite schema (version 1, below), WAL, `synchronous=FULL`. A partial unique index
//!   `jobs_slot_owner ON jobs(host, slot) WHERE state = 'running'` makes "one running job per slot"
//!   a database invariant, not only a scheduler rule.
//! - [`scheduler`]: pure planning. Fair across worktrees (FIFO inside one worktree; not strict
//!   round-robin, only the worktree that ran last is skipped),
//!   host profile limits (`max_runners`, named slots), the `50/15/12 GiB` disk gates and the
//!   exclusive quiet-window lease.
//! - [`runner`]: the [`runner::Executor`] contract, the local executor and the detached supervisor
//!   (`mast build-job-supervise <job-id>`, [`runner::supervise`]).
//! - [`recovery`]: restart reconciliation of `running` rows (exit file, PID + start time
//!   re-adoption, reused PID => `lost`) and the `pidfd` exit watcher.
//! - [`remote_executor`]: SSH execution that delegates to `scripts/remote-cargo.sh`, including the
//!   lazy slot checkout (P10).
//!
//! # Known limit: one engine owns a host's slots
//!
//! The checkout step holds the script's slot lock, but the lock is released before
//! `remote-cargo.sh start` takes it again, so a second engine (or a hand-run job) sharing the
//! same slot can move the checkout in that gap and one job can build the other's commit. The
//! queue assumes it is the only thing that checks out and starts jobs in a host's slots; W3-T2
//! must wire exactly one engine to each remote host.
//!
//! # `cargo test` versus `cargo nextest`: what holds the build-directory lock
//!
//! Measured on the Fedora desktop with cargo 1.100.0 and cargo-nextest 0.9.152: a throwaway crate
//! whose test starts a second `cargo build` on the same `--target-dir` while the test body runs.
//!
//! - **`cargo test` and `cargo nextest run` behave the same.** Each holds the build-directory lock
//!   while it compiles and releases it before any test binary runs. The second build finished in
//!   about 100 ms with no "Blocking waiting for file lock" message under both runners.
//! - **A compile holds the lock.** In the control run a `build.rs` started the same second build
//!   while an outer `cargo build` was compiling: it blocked and printed "Blocking waiting for
//!   file lock on build directory".
//!
//! So the queue treats the lock as held for the *build phase* only, whichever runner a job uses;
//! the *test phase* of a `cargo test` job does not block other Cargo processes on that target.
//! Consequences:
//!
//! - Every slot has its own `CARGO_TARGET_DIR` (`mast-target-{a,b,c}`), so jobs in different
//!   slots never wait on each other's lock. One running job per slot keeps a slot's own lock
//!   uncontended; it does not replace the lock.
//! - The queue never takes the lock itself. A job whose log's last line is `Blocking waiting for
//!   file lock` shows `reason = "waiting for cargo lock"` while it stays `running` (someone ran
//!   Cargo on that target outside the queue); the queue never kills the holder.
//! - A slot stays occupied for the whole job, test phase included, because the job owns the slot,
//!   not because Cargo's lock is held.
//!
//! # Store schema (version 1)
//!
//! W3-T2 (tools), W3-T5 (merge queue) and W3-T6 (UI) read these tables; do not change a column
//! without bumping `PRAGMA user_version` and adding a migration.
//!
//! ```text
//! jobs(id PK, seq UNIQUE, chat_id, run_id, space_id, device_id, host, lane='cargo-shared',
//!      worktree_key, cwd, branch, head_sha, argv JSON, env JSON, label, integration 0|1,
//!      start_free_gib NULL|int, timeout_secs, lease_id NULL, state, reason, slot,
//!      created_at, started_at, finished_at (epoch ms), pid, pgid, proc_start_time,
//!      remote_name, exit_code, signal, summary_json, log_path, stop_reason NULL|cancel|disk|timeout,
//!      stop_attempts (answered attempts only), stop_last_at, stop_unreachable)
//! operations(op_id PK, job_id, input_hash)          -- idempotency: same key + same input => same job
//! waiters(job_id, chat_id, wake 0|1, wake_state none|pending|sent|failed, wake_attempts,
//!         wake_next_at, PK(job_id, chat_id))                       -- the owner and every coalesced chat
//! meta(key PK, value)                                -- last_ran:<host>
//! quiet_windows(host PK, lease_id, owner, requested_at, granted_at NULL, expires_at)
//! ```
//!
//! `state` is `awaiting_approval | queued | running | succeeded | failed | cancelled | timed_out |
//! lost`. Timestamps are epoch milliseconds.
//!
//! # Wakes
//!
//! A terminal job marks each waiter with `wake = 1` as `wake_state = 'pending'`. Pending wakes of
//! one chat are batched: the window opens at the first completion and the batch is delivered once
//! [`BuildJobsConfig::batch_window_ms`] (10 s) has passed, as one message of at most
//! [`WAKE_MAX_BYTES`] (2 KB) through [`WakeSink`] (`DocHost::queue_event_message` in the engine).
//! Delivery is **at least once**: a wake is marked `sent` only after [`WakeSink::deliver`]
//! returns, so a crash between the two re-sends it after a restart, and the chat's event queue
//! does not deduplicate by message id. A failed delivery stays `pending` and is retried with
//! backoff (`wake_next_at`: 1 s, 2 s, 4 s ... capped at 60 s) and given up as `failed` after
//! 20 attempts.

mod recovery;
mod remote_executor;
mod runner;
mod scheduler;
mod store;
#[cfg(test)]
mod tests;

pub use recovery::{Recovered, watch_exit};
pub use remote_executor::{
    ProcessTransport, RemoteExecutor, RemoteTransport, ScriptOutput, validate_ssh_alias,
};
pub use runner::{
    Executor, ExitRecord, LocalExecutor, Probe, StartCtx, StartError, Started, StopError,
    SupervisorCommand, supervise,
};
pub use scheduler::{Candidate, Hold, QuietWindowView, plan as plan_starts, plan_excluding};
pub use store::Store;

use crate::EngineError;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Upper bound of one completion wake (job id, argv, state, timing and error lines).
pub const WAKE_MAX_BYTES: usize = 2048;
/// Number of compiler/test failure lines a wake carries per job.
pub const WAKE_ERROR_LINES: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum BuildJobError {
    #[error("build job store: {0}")]
    Store(String),
    #[error(
        "operationConflict: idempotency key {0} was already used with a different build command"
    )]
    OperationConflict(String),
    #[error("build queue limit: {0}")]
    Limit(String),
    #[error("no such build job: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

impl From<BuildJobError> for EngineError {
    fn from(error: BuildJobError) -> Self {
        EngineError::Other(error.to_string())
    }
}

pub(crate) fn store_error(error: impl std::fmt::Display) -> BuildJobError {
    BuildJobError::Store(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    AwaitingApproval,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Lost,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "awaiting_approval",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Lost => "lost",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "awaiting_approval" => Self::AwaitingApproval,
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "lost" => Self::Lost,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::AwaitingApproval | Self::Queued | Self::Running)
    }
}

/// What a caller hands to [`BuildJobs::enqueue`]. Owner fields come from the agent token, never
/// from client input; argv must already have passed `approval_commands` validation (W3-T2).
#[derive(Debug, Clone, Default)]
pub struct NewJob {
    /// `operation_id(chatId, idempotencyKey)`. A retry returns the same job; the same key with a
    /// different command is `operationConflict`.
    pub op_id: String,
    pub chat_id: String,
    pub run_id: Option<String>,
    pub space_id: String,
    pub device_id: String,
    /// Host profile id (`BuildJobsConfig::hosts`); `None` uses the default host.
    pub host: Option<String>,
    /// Worktree identity (`checkout_id`): the round-robin and coalescing key.
    pub worktree_key: String,
    pub cwd: String,
    pub branch: Option<String>,
    /// Commit a remote slot checkout is materialized at. Required for a remote host.
    pub head_sha: Option<String>,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub label: Option<String>,
    /// Wake the owner when the job ends.
    pub wake: bool,
    pub timeout_secs: Option<u64>,
    /// Integration jobs run on the integration slot (`a`, `CARGO_INCREMENTAL=1`) only.
    pub integration: bool,
    /// Per-project free-space override for the start gate (`30` instead of `50`).
    pub start_free_gib: Option<u64>,
    /// A quiet-window lease this job belongs to; it may run while that lease is granted.
    pub lease_id: Option<String>,
    /// Start in `awaiting_approval` (no matching Space `ApprovalRule`).
    pub awaiting_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub seq: i64,
    pub chat_id: String,
    pub run_id: Option<String>,
    pub space_id: String,
    pub device_id: String,
    pub host: String,
    pub lane: String,
    pub worktree_key: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub label: Option<String>,
    pub integration: bool,
    pub start_free_gib: Option<u64>,
    pub timeout_secs: u64,
    pub lease_id: Option<String>,
    pub state: JobState,
    pub reason: Option<String>,
    pub slot: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// The supervisor's pid (local) or the remote runner's pid. `Some(0)` means "started on the
    /// host, pid unknown" (never "not started"); `None` is a start that was never confirmed.
    pub pid: Option<i64>,
    pub pgid: Option<i64>,
    pub proc_start_time: Option<String>,
    pub remote_name: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub summary_json: Option<String>,
    pub log_path: String,
    pub stop_reason: Option<String>,
    /// Stop attempts made on the running job (TERM sent, or tried and failed) and the time of the last.
    pub stop_attempts: u32,
    pub stop_last_at: Option<i64>,
    /// The last stop attempt could not reach the host (see [`StopError::Unreachable`]).
    pub stop_unreachable: bool,
}

#[derive(Debug, Clone)]
pub struct Enqueued {
    pub job: Job,
    /// 1-based place among the queued jobs of the job's host (0 once it is no longer queued).
    pub position: usize,
    /// The job attached to an identical queued job as an extra waiter.
    pub coalesced: bool,
    /// The idempotency key had been used before with the same command.
    pub replay: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub chat_id: Option<String>,
    pub space_id: Option<String>,
    pub active_only: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub host: String,
    pub lease_id: String,
    pub owner: String,
    /// `false` while queued and running jobs drain; `true` once the host is exclusive.
    pub granted: bool,
    pub expires_at: i64,
}

/// Per-host disk gates. `start_free_gib` may be overridden per job (`NewJob::start_free_gib`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskGates {
    /// Free GiB needed to start a job (50 by default, 30 for a project override).
    pub start_free_gib: u64,
    /// Below this, nothing starts and the host reports a hold (15).
    pub hold_free_gib: u64,
    /// Below this a running job's own process group is aborted (12).
    pub abort_own_pgid_gib: u64,
    /// Free space assumed consumed by each job planned from one free-space reading (10), so
    /// several starts in one tick cannot all pass the gate on the same 50 GiB.
    pub start_reserve_gib: u64,
}

impl Default for DiskGates {
    fn default() -> Self {
        Self {
            start_free_gib: 50,
            hold_free_gib: 15,
            abort_own_pgid_gib: 12,
            start_reserve_gib: 10,
        }
    }
}

/// A named Cargo slot: its own `CARGO_TARGET_DIR` and (remote) checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotConfig {
    pub name: String,
    /// `CARGO_INCREMENTAL=1`. Only the integration slot `a` sets it; worker slots set `0`.
    pub incremental: bool,
    /// `CARGO_TARGET_DIR` for the slot, or `None` to inherit the engine's own value.
    pub target_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteHost {
    /// The ssh *alias* of the host (`desktop`), never `user@host` or an address; `open` refuses
    /// anything else.
    pub ssh: String,
    /// Path of the repository on the host, relative to `$HOME`.
    pub repo: String,
    /// Slot checkout directory is `<checkout_prefix><slot>`, relative to `$HOME`.
    pub checkout_prefix: String,
    /// Slot target directory is `<target_prefix><slot>`, relative to `$HOME`.
    pub target_prefix: String,
    /// Which `remote-cargo.sh start` mode the host's jobs use.
    pub mode: StartMode,
}

/// The `scripts/remote-cargo.sh start` option a host's jobs use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartMode {
    /// No option: no device lock and no space check (the engine's own gates apply).
    Plain,
    /// `--admit`: the concurrent slots `a b c`. The script enforces the slot checkout
    /// (`~/Work/mast-wt-build-<slot>`), the target directory, `CARGO_INCREMENTAL=0` outside slot
    /// `a`, at most three running jobs and its disk and memory floors. A job that carries a
    /// quiet-window lease starts with `--quiet-window` instead.
    Admit,
    /// `--device-serial <serial>`: cutover mode, one job per device.
    DeviceSerial(String),
}

#[derive(Debug, Clone)]
pub enum HostKind {
    /// Runs on this machine through the detached supervisor. `probe_path` is where free space is read.
    Local {
        probe_path: PathBuf,
    },
    Remote(RemoteHost),
}

#[derive(Debug, Clone)]
pub struct HostProfile {
    pub id: String,
    pub kind: HostKind,
    pub slots: Vec<SlotConfig>,
    /// `3` for the verified desktop profile; `1` during cutover or without isolated slots.
    pub max_runners: usize,
    pub disk: DiskGates,
}

impl HostProfile {
    /// The verified three-slot desktop: slots `a` (integration, incremental), `b`, `c`. Pass the
    /// ssh *alias* (`desktop`), never the raw address, so no host-key prompt can stall a job.
    pub fn desktop(ssh: &str) -> Self {
        let slot = |name: &str, incremental: bool| SlotConfig {
            name: name.into(),
            incremental,
            target_dir: Some(format!("Work/mast-target-{name}")),
        };
        Self {
            id: "desktop".into(),
            kind: HostKind::Remote(RemoteHost {
                ssh: ssh.into(),
                repo: "Work/mast".into(),
                checkout_prefix: "Work/mast-wt-build-".into(),
                target_prefix: "Work/mast-target-".into(),
                mode: StartMode::Admit,
            }),
            slots: vec![slot("a", true), slot("b", false), slot("c", false)],
            max_runners: 3,
            disk: DiskGates::default(),
        }
    }

    /// Desktop during cutover: one device-serial runner on slot `a`.
    pub fn desktop_serial(ssh: &str, serial: &str) -> Self {
        let mut profile = Self::desktop(ssh);
        profile.slots.truncate(1);
        profile.max_runners = 1;
        if let HostKind::Remote(remote) = &mut profile.kind {
            remote.mode = StartMode::DeviceSerial(serial.into());
        }
        profile
    }

    /// This machine: one runner, one slot, the engine's own target directory.
    pub fn local(probe_path: PathBuf) -> Self {
        Self {
            id: "local".into(),
            kind: HostKind::Local { probe_path },
            slots: vec![SlotConfig {
                name: "local".into(),
                incremental: false,
                target_dir: None,
            }],
            max_runners: 1,
            disk: DiskGates::default(),
        }
    }

    pub fn slot(&self, name: &str) -> Option<&SlotConfig> {
        self.slots.iter().find(|slot| slot.name == name)
    }
}

#[derive(Clone)]
pub struct BuildJobsConfig {
    /// Profile store root: holds `build-jobs.sqlite3` and `build-jobs/<id>.{log,spec.json,exit}`.
    pub store_root: PathBuf,
    pub hosts: Vec<HostProfile>,
    pub default_host: String,
    pub supervisor: SupervisorCommand,
    pub batch_window_ms: i64,
    pub per_chat_limit: usize,
    pub per_space_limit: usize,
    pub default_timeout_secs: u64,
    pub quiet_window_ttl_ms: i64,
    pub tick_ms: u64,
}

impl BuildJobsConfig {
    pub fn new(store_root: PathBuf, hosts: Vec<HostProfile>) -> Self {
        let default_host = hosts.first().map(|h| h.id.clone()).unwrap_or_default();
        Self {
            store_root,
            hosts,
            default_host,
            supervisor: SupervisorCommand::current_exe(),
            batch_window_ms: 10_000,
            per_chat_limit: 4,
            per_space_limit: 32,
            default_timeout_secs: 3600,
            quiet_window_ttl_ms: 2 * 60 * 60 * 1000,
            tick_ms: 2000,
        }
    }

    pub fn jobs_dir(&self) -> PathBuf {
        self.store_root.join("build-jobs")
    }

    pub fn host(&self, id: &str) -> Option<&HostProfile> {
        self.hosts.iter().find(|host| host.id == id)
    }
}

/// Delivers a completion wake to a chat. The engine implements it with
/// `DocHost::queue_event_message`.
pub trait WakeSink: Send + Sync {
    fn deliver(&self, chat_id: &str, text: &str, message_id: &str) -> Result<(), String>;
}

/// Free space of the filesystem a host builds on.
pub trait DiskProbe: Send + Sync {
    fn free_gib(&self, host: &HostProfile) -> Result<f64, String>;
}

/// The number of build jobs a chat is waiting on. W2-T5's stall rule counts `0 active build_jobs`.
pub trait ActiveBuildJobs: Send + Sync {
    fn active_build_jobs(&self, chat_id: &str) -> usize;
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

struct Inner {
    cfg: BuildJobsConfig,
    store: Store,
    executors: HashMap<String, Arc<dyn Executor>>,
    disk: Arc<dyn DiskProbe>,
    wake: Arc<dyn WakeSink>,
    clock: Clock,
    notify: Arc<tokio::sync::Notify>,
    holds: Mutex<HashMap<String, Hold>>,
    /// Host id -> (until, reason): starts on the host pause after a host-level refusal.
    host_backoff: Mutex<HashMap<String, (i64, String)>>,
    /// Job id -> until: an unreadable job is probed again only after this time (no ssh storm).
    probe_backoff: Mutex<HashMap<String, i64>>,
    /// Host id -> (until, error): a failed free-space probe is reused until this time.
    disk_backoff: Mutex<HashMap<String, (i64, String)>>,
}

#[derive(Clone)]
pub struct BuildJobs {
    inner: Arc<Inner>,
}

/// Builder for [`BuildJobs`]; the defaults are the real executors and probes.
pub struct BuildJobsBuilder {
    cfg: BuildJobsConfig,
    executors: HashMap<String, Arc<dyn Executor>>,
    transport: Option<Arc<dyn RemoteTransport>>,
    disk: Option<Arc<dyn DiskProbe>>,
    wake: Option<Arc<dyn WakeSink>>,
    clock: Option<Clock>,
}

impl BuildJobsBuilder {
    pub fn executor(mut self, host: &str, executor: Arc<dyn Executor>) -> Self {
        self.executors.insert(host.into(), executor);
        self
    }
    pub fn transport(mut self, transport: Arc<dyn RemoteTransport>) -> Self {
        self.transport = Some(transport);
        self
    }
    pub fn disk(mut self, disk: Arc<dyn DiskProbe>) -> Self {
        self.disk = Some(disk);
        self
    }
    pub fn wake(mut self, wake: Arc<dyn WakeSink>) -> Self {
        self.wake = Some(wake);
        self
    }
    pub fn clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Some(Arc::new(clock));
        self
    }

    pub fn open(self) -> Result<BuildJobs, BuildJobError> {
        for host in &self.cfg.hosts {
            if let HostKind::Remote(remote) = &host.kind {
                remote_executor::validate_ssh_alias(&remote.ssh).map_err(BuildJobError::Invalid)?;
            }
        }
        std::fs::create_dir_all(self.cfg.jobs_dir()).map_err(store_error)?;
        let store = Store::open(&self.cfg.store_root)?;
        let transport: Arc<dyn RemoteTransport> = self.transport.unwrap_or_else(|| {
            Arc::new(ProcessTransport::new(PathBuf::from(
                "scripts/remote-cargo.sh",
            )))
        });
        let mut executors = self.executors;
        for host in &self.cfg.hosts {
            executors
                .entry(host.id.clone())
                .or_insert_with(|| match &host.kind {
                    HostKind::Local { .. } => Arc::new(LocalExecutor::new(
                        self.cfg.jobs_dir(),
                        self.cfg.supervisor.clone(),
                    )),
                    HostKind::Remote(_) => {
                        Arc::new(RemoteExecutor::new(transport.clone(), &self.cfg.hosts))
                    }
                });
        }
        let disk = self
            .disk
            .unwrap_or_else(|| Arc::new(runner::SystemDiskProbe::new(transport.clone())));
        let wake = self.wake.unwrap_or_else(|| Arc::new(NoWake));
        let clock = self.clock.unwrap_or_else(|| Arc::new(crate::now_ms));
        Ok(BuildJobs {
            inner: Arc::new(Inner {
                cfg: self.cfg,
                store,
                executors,
                disk,
                wake,
                clock,
                notify: Arc::new(tokio::sync::Notify::new()),
                holds: Mutex::new(HashMap::new()),
                host_backoff: Mutex::new(HashMap::new()),
                probe_backoff: Mutex::new(HashMap::new()),
                disk_backoff: Mutex::new(HashMap::new()),
            }),
        })
    }
}

struct NoWake;
impl WakeSink for NoWake {
    fn deliver(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

impl BuildJobs {
    pub fn builder(cfg: BuildJobsConfig) -> BuildJobsBuilder {
        BuildJobsBuilder {
            cfg,
            executors: HashMap::new(),
            transport: None,
            disk: None,
            wake: None,
            clock: None,
        }
    }

    pub fn config(&self) -> &BuildJobsConfig {
        &self.inner.cfg
    }

    fn now(&self) -> i64 {
        (self.inner.clock)()
    }

    /// Queue a job (or return the job an idempotent retry or an identical queued job already has).
    pub fn enqueue(&self, new: NewJob) -> Result<Enqueued, BuildJobError> {
        let host = new
            .host
            .clone()
            .unwrap_or_else(|| self.inner.cfg.default_host.clone());
        let Some(profile) = self.inner.cfg.host(&host) else {
            return Err(BuildJobError::Invalid(format!("unknown build host {host}")));
        };
        if new.argv.is_empty() {
            return Err(BuildJobError::Invalid("a build job needs a command".into()));
        }
        if matches!(profile.kind, HostKind::Remote(_))
            && new.head_sha.as_deref().is_none_or(str::is_empty)
        {
            return Err(BuildJobError::Invalid(
                "a remote build job needs headSha".into(),
            ));
        }
        if matches!(profile.kind, HostKind::Remote(_)) && !new.env.is_empty() {
            return Err(BuildJobError::Invalid(
                "a remote build job cannot set env: the host runs the job with its slot environment"
                    .into(),
            ));
        }
        if new.env.contains_key("CARGO_TARGET_DIR") || new.env.contains_key("CARGO_INCREMENTAL") {
            return Err(BuildJobError::Invalid(
                "CARGO_TARGET_DIR and CARGO_INCREMENTAL belong to the slot, not the job".into(),
            ));
        }
        let id = crate::new_id();
        let log_path = self.inner.cfg.jobs_dir().join(format!("{id}.log"));
        let timeout = new
            .timeout_secs
            .unwrap_or(self.inner.cfg.default_timeout_secs)
            .max(1);
        let out = self.inner.store.insert(
            &new,
            &host,
            &id,
            &log_path.to_string_lossy(),
            timeout,
            self.now(),
            self.inner.cfg.per_chat_limit,
            self.inner.cfg.per_space_limit,
        )?;
        self.inner.notify.notify_one();
        Ok(out)
    }

    /// `awaiting_approval` -> `queued`.
    pub fn approve(&self, job_id: &str) -> Result<Job, BuildJobError> {
        let job = self.inner.store.approve(job_id)?;
        self.inner.notify.notify_one();
        Ok(job)
    }

    /// `awaiting_approval` -> `cancelled`, with the owner woken.
    pub fn deny(&self, job_id: &str, reason: &str) -> Result<Job, BuildJobError> {
        let job = self.get(job_id)?;
        if job.state != JobState::AwaitingApproval {
            return Err(BuildJobError::Invalid(format!(
                "job {job_id} is {}, not awaiting approval",
                job.state.as_str()
            )));
        }
        self.inner
            .store
            .finish_unstarted(job_id, JobState::Cancelled, reason, self.now())
    }

    /// Cancel a job. A queued job is cancelled at once; a running job gets SIGTERM to its own
    /// process group (SIGKILL after 10 s) and ends `cancelled`. `requester` restricts the call to
    /// a waiting chat (`None` is the user acting from the UI).
    pub fn cancel(&self, job_id: &str, requester: Option<&str>) -> Result<Job, BuildJobError> {
        let job = self.get(job_id)?;
        if let Some(chat) = requester
            && !self.inner.store.is_waiter(job_id, chat)?
        {
            return Err(BuildJobError::Invalid(format!(
                "chat {chat} does not own job {job_id}"
            )));
        }
        self.cancel_from(job)
    }

    /// Cancel from a snapshot that may be stale: the job can start, finish or be requeued between
    /// the read and the write, so a write that changes nothing re-reads and acts on what the job
    /// is now (a job requeued meanwhile is cancelled as a queued job).
    fn cancel_from(&self, mut job: Job) -> Result<Job, BuildJobError> {
        let job_id = job.id.clone();
        for _ in 0..4 {
            match job.state {
                JobState::AwaitingApproval | JobState::Queued => {
                    let ended = self.inner.store.finish_unstarted(
                        &job_id,
                        JobState::Cancelled,
                        "cancelled",
                        self.now(),
                    )?;
                    if ended.state != JobState::Running {
                        return Ok(ended);
                    }
                    job = ended; // it started meanwhile: stop it instead
                }
                JobState::Running => {
                    let requested = self.inner.store.set_stop_reason(&job_id, "cancel")?;
                    let current = self.get(&job_id)?;
                    if requested
                        || current.state == JobState::Running && current.stop_reason.is_some()
                    {
                        // A failed stop (still starting, ssh down) is retried by `reap`.
                        if requested {
                            self.try_stop(&current, self.now());
                        }
                        self.inner.notify.notify_one();
                        return self.get(&job_id);
                    }
                    job = current; // it ended or was requeued meanwhile: act on that
                }
                _ => return Ok(job),
            }
        }
        self.get(&job_id)
    }

    /// Cancel the queued jobs only `chat_id` waits on (archiving a chat). Running jobs finish.
    pub fn cancel_queued_for_chat(&self, chat_id: &str) -> Result<Vec<String>, BuildJobError> {
        self.inner.store.cancel_queued_for_chat(chat_id, self.now())
    }

    pub fn get(&self, job_id: &str) -> Result<Job, BuildJobError> {
        self.inner
            .store
            .get(job_id)?
            .ok_or_else(|| BuildJobError::NotFound(job_id.into()))
    }

    /// Read a bounded byte window of a job's local log. Callers must authorize the job first.
    pub fn log_slice(&self, job: &Job, offset: u64, max_bytes: usize) -> Result<(String, u64), BuildJobError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = match std::fs::File::open(&job.log_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((String::new(), offset)),
            Err(error) => return Err(store_error(error)),
        };
        file.seek(SeekFrom::Start(offset)).map_err(store_error)?;
        let mut bytes = Vec::new();
        file.take(max_bytes.clamp(1, 64 * 1024) as u64).read_to_end(&mut bytes).map_err(store_error)?;
        let next = offset.saturating_add(bytes.len() as u64);
        Ok((String::from_utf8_lossy(&bytes).into_owned(), next))
    }

    pub fn arm_wake(&self, job_id: &str, chat_id: &str) -> Result<(), BuildJobError> {
        if !self.inner.store.arm_wake(job_id, chat_id)? {
            return Err(BuildJobError::Invalid("chat does not own this build job".into()));
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    pub fn is_waiter(&self, job_id: &str, chat_id: &str) -> Result<bool, BuildJobError> {
        self.inner.store.is_waiter(job_id, chat_id)
    }

    pub fn list(&self, filter: &ListFilter) -> Result<Vec<Job>, BuildJobError> {
        self.inner.store.list(filter)
    }

    /// 1-based queue position on the job's host, or 0 when it is not queued.
    pub fn position(&self, job_id: &str) -> Result<usize, BuildJobError> {
        self.inner.store.position(job_id)
    }

    /// Why the last scheduling pass left queued jobs on `host` waiting, if it did.
    pub fn hold(&self, host: &str) -> Option<Hold> {
        self.inner.holds.lock().ok()?.get(host).cloned()
    }

    /// Chats waiting on a non-terminal job (`awaiting_approval`, `queued` or `running`).
    pub fn active_job_count(&self, chat_id: &str) -> usize {
        self.inner.store.active_count(chat_id).unwrap_or(0)
    }

    /// Ask for the exclusive quiet-window lease on `host`. The lease holds queued jobs, waits for
    /// running jobs to drain (`granted` turns true on a later tick) and lapses after the TTL.
    /// `Err(Invalid)` while another owner holds it.
    pub fn acquire_quiet_window(&self, host: &str, owner: &str) -> Result<Lease, BuildJobError> {
        if self.inner.cfg.host(host).is_none() {
            return Err(BuildJobError::Invalid(format!("unknown build host {host}")));
        }
        let now = self.now();
        let lease = self.inner.store.acquire_lease(
            host,
            owner,
            now,
            now + self.inner.cfg.quiet_window_ttl_ms,
        )?;
        self.inner.notify.notify_one();
        Ok(lease)
    }

    pub fn quiet_window(&self, host: &str) -> Result<Option<Lease>, BuildJobError> {
        self.inner.store.lease(host, self.now())
    }

    pub fn release_quiet_window(&self, host: &str, lease_id: &str) -> Result<bool, BuildJobError> {
        let released = self.inner.store.release_lease(host, lease_id)?;
        self.inner.notify.notify_one();
        Ok(released)
    }

    /// Reconcile `running` rows after an engine start: finish from an exit file, re-adopt a live
    /// supervisor (same PID and start time), mark anything else `lost`. Call once before ticking.
    pub fn recover(&self) -> Result<Recovered, BuildJobError> {
        recovery::recover(self)
    }

    /// One scheduling pass: reap, enforce timeouts and the abort gate, start, deliver wakes.
    pub fn tick(&self) {
        let now = self.now();
        for host in &self.inner.cfg.hosts {
            self.tick_host(host, now);
        }
        self.flush_wakes(now);
    }

    /// Run [`tick`](Self::tick) every `tick_ms` (or sooner when a job is enqueued or a watched
    /// process exits) until the returned task is aborted.
    pub fn spawn(&self) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let pass = this.clone();
                let _ = tokio::task::spawn_blocking(move || pass.tick()).await;
                let wait = std::time::Duration::from_millis(this.inner.cfg.tick_ms);
                let _ = tokio::time::timeout(wait, this.inner.notify.notified()).await;
            }
        })
    }

    fn tick_host(&self, host: &HostProfile, now: i64) {
        let Some(executor) = self.inner.executors.get(&host.id) else {
            return;
        };
        let store = &self.inner.store;
        let running = store.running(&host.id).unwrap_or_default();
        if running.is_empty() && store.queued(&host.id).is_ok_and(|queued| queued.is_empty()) {
            // Idle host: nothing to probe over ssh. A lease on an empty host is granted at once.
            let _ = store.expire_leases(now);
            let _ = store.grant_lease(&host.id);
            if let Ok(mut holds) = self.inner.holds.lock() {
                holds.remove(&host.id);
            }
            return;
        }
        let free = self.free_gib(host, now);
        for job in &running {
            self.reap(
                host,
                executor.as_ref(),
                job,
                now,
                free.as_ref().ok().copied(),
            );
        }
        let _ = store.expire_leases(now);
        // A refused slot (busy, or the host refused the start) is skipped for the rest of this
        // tick and the plan is made again, so one stuck slot cannot stall the others.
        let mut unavailable: HashSet<String> = HashSet::new();
        for _ in 0..=host.slots.len() {
            let running = store.running(&host.id).unwrap_or_default();
            if running.is_empty() {
                let _ = store.grant_lease(&host.id);
            }
            // A host-level refusal pauses this host's starts; reaps and wakes carry on.
            let backoff = self
                .inner
                .host_backoff
                .lock()
                .ok()
                .and_then(|b| b.get(&host.id).cloned())
                .filter(|(until, _)| now < *until);
            if let Some((_, reason)) = backoff {
                if store
                    .queued(&host.id)
                    .is_ok_and(|queued| !queued.is_empty())
                    && let Ok(mut holds) = self.inner.holds.lock()
                {
                    holds.insert(host.id.clone(), Hold::HostRefused(reason));
                }
                break;
            }
            let lease = store.lease(&host.id, now).ok().flatten();
            let queued = store.queued(&host.id).unwrap_or_default();
            let candidates: Vec<Candidate> = queued
                .iter()
                .map(|job| Candidate {
                    id: job.id.clone(),
                    seq: job.seq,
                    worktree_key: job.worktree_key.clone(),
                    integration: job.integration,
                    start_free_gib: job.start_free_gib,
                    lease_id: job.lease_id.clone(),
                })
                .collect();
            let busy: Vec<(String, String)> = running
                .iter()
                .map(|job| {
                    (
                        job.worktree_key.clone(),
                        job.slot.clone().unwrap_or_default(),
                    )
                })
                .collect();
            let last_ran = store.meta(&format!("last_ran:{}", host.id)).ok().flatten();
            let window = lease.as_ref().map(|lease| QuietWindowView {
                lease_id: lease.lease_id.clone(),
                granted: lease.granted,
            });
            let plan = plan_excluding(
                host,
                &candidates,
                &busy,
                last_ran.as_deref(),
                free.clone(),
                window.as_ref(),
                &unavailable,
            );
            if let Ok(mut holds) = self.inner.holds.lock() {
                match &plan.held {
                    Some(hold) if !candidates.is_empty() => {
                        holds.insert(host.id.clone(), hold.clone());
                    }
                    _ => {
                        holds.remove(&host.id);
                    }
                }
            }
            let mut refused = None;
            let mut host_refused = false;
            for (job_id, slot) in &plan.starts {
                let Some(job) = queued.iter().find(|job| &job.id == job_id) else {
                    continue;
                };
                match self.start_job(host, executor.as_ref(), job, slot, now) {
                    StartOutcome::Refused => {
                        refused = Some(slot.clone());
                        break;
                    }
                    // Every slot would be refused the same way: stop for this tick.
                    StartOutcome::HostRefused => {
                        host_refused = true;
                        break;
                    }
                    StartOutcome::Started | StartOutcome::Skipped => {}
                }
            }
            if host_refused {
                break;
            }
            match refused {
                Some(slot) => {
                    unavailable.insert(slot);
                }
                None => break,
            }
        }
    }

    /// The host's free space; a failed probe (ssh down) is reused for 30 s instead of waiting out
    /// another ssh timeout on every tick.
    fn free_gib(&self, host: &HostProfile, now: i64) -> Result<f64, String> {
        if let Ok(cache) = self.inner.disk_backoff.lock()
            && let Some((until, error)) = cache.get(&host.id)
            && now < *until
        {
            return Err(error.clone());
        }
        let free = self.inner.disk.free_gib(host);
        if let Ok(mut cache) = self.inner.disk_backoff.lock() {
            match &free {
                Err(error) => {
                    cache.insert(
                        host.id.clone(),
                        (now + UNREACHABLE_PROBE_BACKOFF_MS, error.clone()),
                    );
                }
                Ok(_) => {
                    cache.remove(&host.id);
                }
            }
        }
        free
    }

    /// Try to start `job` in `slot`. `Refused` means the host turned the start down (the job is
    /// queued again) and the slot should be skipped for the rest of the tick.
    fn start_job(
        &self,
        host: &HostProfile,
        executor: &dyn Executor,
        job: &Job,
        slot: &str,
        now: i64,
    ) -> StartOutcome {
        let store = &self.inner.store;
        let Some(slot_config) = host.slot(slot) else {
            return StartOutcome::Skipped;
        };
        let remote_name = format!("mastbj-{}", job.id);
        match store.claim_running(&job.id, slot, &remote_name, now) {
            Ok(true) => {}
            Ok(false) => return StartOutcome::Skipped,
            Err(error) => {
                tracing::warn!(job = %job.id, %error, "build job claim failed");
                return StartOutcome::Skipped;
            }
        }
        let mut claimed = job.clone();
        claimed.slot = Some(slot.into());
        claimed.remote_name = Some(remote_name);
        claimed.started_at = Some(now);
        let ctx = StartCtx {
            job: &claimed,
            host,
            slot: slot_config,
            log_path: Path::new(&job.log_path),
        };
        match executor.start(&ctx) {
            Ok(started) => {
                // Only a start that happened counts as "ran last": a refused one must not push
                // its own worktree behind the others when the plan is made again.
                let _ = store.set_meta(&format!("last_ran:{}", host.id), &job.worktree_key);
                if let Err(error) = store.record_started(&job.id, &started) {
                    tracing::warn!(job = %job.id, %error, "could not record build job start");
                }
                if let (Some(pid), Some(start_time)) =
                    (started.pid, started.proc_start_time.clone())
                    && matches!(host.kind, HostKind::Local { .. })
                {
                    let notify = self.inner.notify.clone();
                    watch_exit(pid, start_time, Arc::new(move || notify.notify_one()));
                }
                StartOutcome::Started
            }
            Err(StartError::Busy(reason)) => {
                // Atomic: a cancel that landed during the start window ends the job `cancelled`
                // instead of leaving a stop request on a job that is queued again.
                let _ = store.unclaim(&job.id, &reason, now);
                StartOutcome::Refused
            }
            Err(StartError::HostBusy(reason)) => {
                let _ = store.unclaim(&job.id, &reason, now);
                if let Ok(mut backoff) = self.inner.host_backoff.lock() {
                    backoff.insert(
                        host.id.clone(),
                        (now + HOST_START_BACKOFF_MS, reason.clone()),
                    );
                }
                if let Ok(mut holds) = self.inner.holds.lock() {
                    holds.insert(host.id.clone(), Hold::HostRefused(reason));
                }
                StartOutcome::HostRefused
            }
            Err(StartError::Failed(reason)) => {
                let _ = store.finish(
                    &job.id,
                    JobState::Failed,
                    None,
                    None,
                    Some(&reason),
                    None,
                    now,
                );
                StartOutcome::Skipped
            }
        }
    }

    /// Timeout, disk abort and stop retries for a job that reads as running or unreadable.
    fn enforce_limits(&self, host: &HostProfile, job: &Job, now: i64, free: Option<f64>) {
        let store = &self.inner.store;
        if job.stop_reason.is_some() {
            if job.stop_attempts >= MAX_STOP_ATTEMPTS {
                self.give_up_stopping(job, now);
            } else if stop_due(job, now) {
                // A stop that failed or did not take is retried with backoff and escalated.
                self.try_stop(job, now);
            }
            return;
        }
        let started = job.started_at.unwrap_or(now);
        let grace = if matches!(host.kind, HostKind::Local { .. }) {
            30_000
        } else {
            0
        };
        let over_time = now > started + job.timeout_secs as i64 * 1000 + grace;
        let disk_abort = free.is_some_and(|free| free < host.disk.abort_own_pgid_gib as f64);
        let reason = if over_time {
            Some("timeout")
        } else if disk_abort {
            Some("disk")
        } else {
            None
        };
        if let Some(reason) = reason {
            let _ = store.set_stop_reason(&job.id, reason);
            let mut stopping = job.clone();
            stopping.stop_reason = Some(reason.into());
            self.try_stop(&stopping, now);
        }
    }

    /// Every stop attempt failed: end the job as the stop reason says and say that the host may
    /// still be running it, instead of retrying forever and holding the slot.
    fn give_up_stopping(&self, job: &Job, now: i64) {
        let (state, what) = match job.stop_reason.as_deref() {
            Some("cancel") => (JobState::Cancelled, "cancelled".to_string()),
            Some("timeout") => (
                JobState::TimedOut,
                format!("timed out after {} s", job.timeout_secs),
            ),
            _ => (
                JobState::Failed,
                "aborted: free disk below the abort gate".to_string(),
            ),
        };
        let reason = format!(
            "{what}; it could not be stopped after {} attempts and the host may still be running it",
            job.stop_attempts
        );
        let _ = self
            .inner
            .store
            .finish(&job.id, state, None, None, Some(&reason), None, now);
    }

    /// A start that was never confirmed turned out not to have happened (an ssh drop answered
    /// "unknown job"): put the job back in the queue, or end it `cancelled` when a stop was asked.
    fn requeue_unstarted(&self, job: &Job, now: i64) {
        let _ = self
            .inner
            .store
            .unclaim(&job.id, "start unconfirmed; retrying", now);
    }

    /// Ask the job's executor to stop it. An answered attempt counts toward the escalation and
    /// the cap; one that could not reach the host does not, and is retried every 60 s.
    fn try_stop(&self, job: &Job, now: i64) {
        let Some(executor) = self.inner.executors.get(&job.host) else {
            return;
        };
        let (counted, unreachable) = match executor.stop(job, job.stop_attempts) {
            Ok(()) => (true, false),
            Err(error @ StopError::Unreachable(_)) => {
                tracing::warn!(job = %job.id, %error, "build job stop could not reach the host; will retry");
                (false, true)
            }
            Err(error) => {
                tracing::warn!(job = %job.id, attempt = job.stop_attempts, %error, "build job stop failed; will retry");
                (true, false)
            }
        };
        let _ = self
            .inner
            .store
            .record_stop_attempt(&job.id, now, counted, unreachable);
    }

    fn reap(
        &self,
        host: &HostProfile,
        executor: &dyn Executor,
        job: &Job,
        now: i64,
        free: Option<f64>,
    ) {
        let store = &self.inner.store;
        // An unreadable job is probed again only after a pause: while the host is unreachable each
        // probe costs an ssh timeout, and the limits below do not need a fresh answer.
        let paused = self
            .inner
            .probe_backoff
            .lock()
            .ok()
            .and_then(|b| b.get(&job.id).copied())
            .is_some_and(|until| now < until);
        let probe = if paused {
            Probe::Unknown("host unreachable (probe paused)".into())
        } else {
            executor.probe(job)
        };
        if let Ok(mut backoff) = self.inner.probe_backoff.lock() {
            match &probe {
                Probe::Unknown(_) if !paused => {
                    backoff.insert(job.id.clone(), now + UNREACHABLE_PROBE_BACKOFF_MS);
                }
                Probe::Unknown(_) => {}
                _ => {
                    backoff.remove(&job.id);
                }
            }
        }
        match probe {
            Probe::Running {
                waiting_for_lock,
                pid,
            } => {
                let note = waiting_for_lock.then_some("waiting for cargo lock");
                if job.reason.as_deref() != note {
                    let _ = store.set_reason(&job.id, note);
                }
                // A start confirmed late: learn the pid so a later `unknown` reads as lost.
                if let (None, Some(pid)) = (job.pid, pid) {
                    let _ = store.set_pid(&job.id, pid);
                }
                self.enforce_limits(host, job, now, free);
            }
            Probe::Exited(record) => self.finish_exited(job, &record, now),
            Probe::Gone(why) => {
                // A job we were stopping and that vanished (KILLed supervisor) ended as asked.
                let (state, reason) = match job.stop_reason.as_deref() {
                    Some("cancel") => (JobState::Cancelled, "cancelled".to_string()),
                    Some("timeout") => (
                        JobState::TimedOut,
                        format!("timed out after {} s", job.timeout_secs),
                    ),
                    Some("disk") => (
                        JobState::Failed,
                        "aborted: free disk below the abort gate".to_string(),
                    ),
                    _ => (JobState::Lost, why),
                };
                let _ = store.finish(&job.id, state, None, None, Some(&reason), None, now);
            }
            Probe::NotStarted => self.requeue_unstarted(job, now),
            Probe::Unknown(error) => {
                tracing::debug!(job = %job.id, %error, "build job probe failed; keeping it running");
                // Say so where users look, and keep the timeout and the disk gate: an unreadable
                // job is neither finished nor forgotten.
                if job.reason.as_deref() != Some("host unreachable; retrying") {
                    let _ = store.set_reason(&job.id, Some("host unreachable; retrying"));
                }
                self.enforce_limits(host, job, now, free);
            }
        }
    }

    fn finish_exited(&self, job: &Job, record: &ExitRecord, now: i64) {
        // The engine's own stop reason wins: the supervisor records `cancel` for any TERM, so a
        // timeout or disk abort it was sent would otherwise read as a cancel.
        let (state, reason) = match job.stop_reason.as_deref() {
            Some("cancel") => (JobState::Cancelled, Some("cancelled".to_string())),
            Some("disk") => (
                JobState::Failed,
                Some("aborted: free disk below the abort gate".to_string()),
            ),
            Some("timeout") => (
                JobState::TimedOut,
                Some(format!("timed out after {} s", job.timeout_secs)),
            ),
            _ => match record.reason.as_deref() {
                Some("cancel") => (JobState::Cancelled, Some("cancelled".to_string())),
                Some("timeout") => (
                    JobState::TimedOut,
                    Some(format!("timed out after {} s", job.timeout_secs)),
                ),
                _ if record.exit_code == Some(0) => (JobState::Succeeded, None),
                _ => (JobState::Failed, record.spawn_error.clone()),
            },
        };
        let started = record.started_at_ms.or(job.started_at).unwrap_or(now);
        let finished = record.finished_at_ms.unwrap_or(now);
        let summary = serde_json::json!({
            "durationMs": (finished - started).max(0),
            "queueWaitMs": (started - job.created_at).max(0),
            "report": record.summary,
        })
        .to_string();
        let _ = self.inner.store.finish(
            &job.id,
            state,
            record.exit_code,
            record.signal,
            reason.as_deref(),
            Some(&summary),
            finished,
        );
    }

    /// Deliver due wake batches. A batch is due once its oldest completion is `batch_window_ms` old.
    fn flush_wakes(&self, now: i64) {
        let store = &self.inner.store;
        let Ok(batches) = store.pending_wakes(now) else {
            return;
        };
        for (chat_id, jobs) in batches {
            let oldest = jobs
                .iter()
                .filter_map(|job| job.finished_at)
                .min()
                .unwrap_or(now);
            if now < oldest + self.inner.cfg.batch_window_ms {
                continue;
            }
            let text = render_wake(&jobs);
            let message_id = format!("build-wake-{}", jobs[0].id);
            let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
            match self.inner.wake.deliver(&chat_id, &text, &message_id) {
                Ok(()) => {
                    let _ = store.mark_wakes(&chat_id, &ids, "sent");
                }
                Err(error) => {
                    tracing::warn!(chat = %chat_id, %error, "build job wake delivery failed");
                    let _ = store.bump_wake_attempts(&chat_id, &ids, now);
                }
            }
        }
    }
}

impl ActiveBuildJobs for BuildJobs {
    fn active_build_jobs(&self, chat_id: &str) -> usize {
        self.active_job_count(chat_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartOutcome {
    Started,
    /// Nothing to do for this plan entry (the claim was lost, or the job ended `failed`).
    Skipped,
    /// The slot refused the start and the job is queued again: try the next slot.
    Refused,
    /// The host refused the start (or could not be reached) and the job is queued again: stop
    /// starting on this host for a backoff.
    HostRefused,
}

/// After a host-level refusal the host's starts pause for this long (tick-driven, no sleeping).
const HOST_START_BACKOFF_MS: i64 = 15_000;
/// An unreadable job or a failed free-space probe is tried again only after this long.
const UNREACHABLE_PROBE_BACKOFF_MS: i64 = 30_000;

/// Answered stop attempts after which a job that cannot be stopped is ended (about six minutes
/// of backoff). Attempts that could not reach the host do not count.
const MAX_STOP_ATTEMPTS: u32 = 10;

/// A stop is retried 5 s after the first answered attempt, then 10 s, 20 s ... capped at 60 s;
/// when the host could not be reached the wait is 60 s.
fn stop_due(job: &Job, now: i64) -> bool {
    let wait = if job.stop_unreachable {
        60_000
    } else {
        (5_000i64 << job.stop_attempts.saturating_sub(1).min(4)).min(60_000)
    };
    (job.stop_attempts == 0 && !job.stop_unreachable)
        || job.stop_last_at.is_none_or(|last| now >= last + wait)
}

/// Render one chat's wake: at most [`WAKE_MAX_BYTES`], ending with the `build_log` pointer.
pub fn render_wake(jobs: &[Job]) -> String {
    const FOOTER: &str = "Use build_log { jobId } for the full log.";
    let mut out = String::new();
    let per_job = (WAKE_MAX_BYTES - FOOTER.len() - 16) / jobs.len().max(1);
    for job in jobs {
        let started = job.started_at.unwrap_or(job.created_at);
        let finished = job.finished_at.unwrap_or(started);
        let mut block = format!(
            "build {} {} exit {} in {}s (queued {}s) {}{}\n$ {}\n",
            job.id,
            job.state.as_str(),
            job.exit_code
                .map_or_else(|| "-".into(), |code| code.to_string()),
            (finished - started).max(0) / 1000,
            (started - job.created_at).max(0) / 1000,
            job.branch.as_deref().unwrap_or(&job.worktree_key),
            job.reason
                .as_deref()
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default(),
            job.argv.join(" "),
        );
        if !matches!(job.state, JobState::Succeeded) {
            for line in failure_lines(Path::new(&job.log_path), WAKE_ERROR_LINES) {
                block.push_str(&line);
                block.push('\n');
            }
        }
        out.push_str(truncate_bytes(&block, per_job));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    let keep = truncate_bytes(&out, WAKE_MAX_BYTES - FOOTER.len() - 1).len();
    out.truncate(keep);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(FOOTER);
    out
}

fn truncate_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The first `limit` compiler-error / failing-test lines of a log (first 256 KiB), each trimmed.
pub(crate) fn failure_lines(path: &Path, limit: usize) -> Vec<String> {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = file.take(256 * 1024).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf)
        .lines()
        .filter(|line| {
            line.starts_with("error")
                || line.trim_start().starts_with("FAIL ")
                || line.contains("FAILED")
                || line.contains("panicked")
        })
        .take(limit)
        .map(|line| truncate_bytes(line, 200).to_string())
        .collect()
}

/// The engine's wake sink: a build completion becomes an event message in the owning chat.
impl WakeSink for crate::doc_host::DocHost {
    fn deliver(&self, chat_id: &str, text: &str, message_id: &str) -> Result<(), String> {
        self.queue_build_job_wake(chat_id, text, message_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
