//! Execution of a started job: the [`Executor`] contract, the local executor and the detached
//! supervisor behind `mast build-job-supervise <job-id>`.
//!
//! # Local process model
//!
//! The engine writes `<jobs dir>/<id>.spec.json`, then spawns the supervisor as a `setsid` session
//! leader (so it has its own session and process group, and survives an engine restart). The
//! supervisor starts the command as its child in a process group of its own, points the child's
//! stdout and stderr at `<id>.log`, enforces the wall-clock timeout and, when the child ends,
//! writes `<id>.exit` atomically (temp file, fsync, rename). Cancel is SIGTERM to the supervisor,
//! which forwards it to the child's group and sends SIGKILL after 10 s. The engine only ever
//! signals processes it started, after checking the recorded PID start time so a reused PID is
//! never touched.
//!
//! The exit file is versioned (`"version": 1`); a reader that sees another version treats the job
//! as `lost`.

use super::{HostKind, HostProfile, Job, RemoteTransport, SlotConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::CString,
    io::Write,
    os::unix::{ffi::OsStrExt, process::CommandExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const EXIT_FILE_VERSION: u32 = 1;
const KILL_GRACE: Duration = Duration::from_secs(10);

/// Everything a supervisor needs, written by the engine next to the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorSpec {
    pub job_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    /// Variables set on top of the supervisor's own environment.
    pub env: BTreeMap<String, String>,
    pub log_path: String,
    pub timeout_secs: u64,
}

/// What a finished job left behind (`<id>.exit`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ExitRecord {
    pub version: u32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// `timeout` or `cancel` when the supervisor ended the command itself.
    pub reason: Option<String>,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub spawn_error: Option<String>,
    /// Trimmed report of a remote job (failure lines and tail); local jobs leave it empty.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Started {
    pub pid: Option<i64>,
    pub pgid: Option<i64>,
    pub proc_start_time: Option<String>,
    pub remote_name: Option<String>,
}

pub enum StartError {
    /// This *slot* cannot take the job right now (another job owns it, or its lock timed out); the
    /// job goes back to `queued` and the scheduler tries it on the next free slot at once.
    Busy(String),
    /// The *host* refused or could not be reached (an admission limit, a held quiet-window or
    /// device lock, a lost answer): every slot would be refused the same way, so the job goes
    /// back to `queued` and the host's starts pause for a short backoff.
    HostBusy(String),
    /// The job cannot run (bad cwd, spawn failure, commit not on the host); it ends `failed`.
    Failed(String),
}

pub enum Probe {
    Running {
        waiting_for_lock: bool,
        /// The job's pid, when the executor learned it and the store has none yet (a remote job
        /// whose start was confirmed late); `None` otherwise.
        pid: Option<i64>,
    },
    Exited(ExitRecord),
    /// Neither running nor finished: the job is `lost` (or `cancelled` when a stop was requested).
    Gone(String),
    /// The state could not be read (ssh down); the job stays `running`.
    Unknown(String),
    /// A start whose outcome was never confirmed turned out not to have happened: requeue it.
    NotStarted,
}

/// Why a stop attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopError {
    /// The host could not be reached (ssh failed): nothing is known, and it never counts toward
    /// the stop cap; the stop is retried every 60 s for as long as the host stays unreachable.
    Unreachable(String),
    /// The host (or the local process table) answered and the job was not stopped.
    Failed(String),
}

impl std::fmt::Display for StopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "host unreachable: {why}"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

pub struct StartCtx<'a> {
    pub job: &'a Job,
    pub host: &'a HostProfile,
    pub slot: &'a SlotConfig,
    pub log_path: &'a Path,
}

pub trait Executor: Send + Sync {
    fn start(&self, ctx: &StartCtx<'_>) -> Result<Started, StartError>;
    fn probe(&self, job: &Job) -> Probe;
    /// Ask a running job to stop. `attempt` counts earlier attempts (0 first): the first two ask
    /// politely (TERM), later ones escalate to KILL. An `Err` means the job was not signalled
    /// (still starting, host unreachable); the caller retries with backoff. Only the job's own
    /// process group is signalled.
    fn stop(&self, job: &Job, attempt: u32) -> Result<(), StopError>;
}

/// The command `LocalExecutor` runs as `<exe> <args...> <job-id>`; the shipped binary is
/// `mast build-job-supervise <job-id>`.
#[derive(Debug, Clone)]
pub struct SupervisorCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

impl SupervisorCommand {
    pub fn current_exe() -> Self {
        Self {
            exe: std::env::current_exe().unwrap_or_default(),
            args: vec!["build-job-supervise".into()],
        }
    }
}

/// Environment a slot imposes: `CARGO_INCREMENTAL` (`1` only on the integration slot) and its own
/// `CARGO_TARGET_DIR`.
pub(crate) fn slot_env(slot: &SlotConfig) -> Vec<(String, String)> {
    let mut env = vec![(
        "CARGO_INCREMENTAL".to_string(),
        if slot.incremental { "1" } else { "0" }.to_string(),
    )];
    if let Some(dir) = &slot.target_dir {
        env.push(("CARGO_TARGET_DIR".to_string(), dir.clone()));
    }
    env
}

pub struct LocalExecutor {
    dir: PathBuf,
    supervisor: SupervisorCommand,
    /// Spawned supervisors, kept so a finished one is reaped instead of left as a zombie.
    children: Mutex<HashMap<String, std::process::Child>>,
}

impl LocalExecutor {
    pub fn new(dir: PathBuf, supervisor: SupervisorCommand) -> Self {
        Self {
            dir,
            supervisor,
            children: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, id: &str, ext: &str) -> PathBuf {
        self.dir.join(format!("{id}.{ext}"))
    }
}

impl Executor for LocalExecutor {
    fn start(&self, ctx: &StartCtx<'_>) -> Result<Started, StartError> {
        let job = ctx.job;
        let fail = |message: String| StartError::Failed(message);
        if !Path::new(&job.cwd).is_dir() {
            return Err(fail(format!(
                "working directory {} does not exist",
                job.cwd
            )));
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| fail(format!("build jobs directory: {e}")))?;
        let mut env = job.env.clone();
        env.extend(slot_env(ctx.slot));
        let spec = SupervisorSpec {
            job_id: job.id.clone(),
            argv: job.argv.clone(),
            cwd: job.cwd.clone(),
            env,
            log_path: ctx.log_path.to_string_lossy().into_owned(),
            timeout_secs: job.timeout_secs,
        };
        let _ = std::fs::remove_file(self.path(&job.id, "exit"));
        write_atomic(
            &self.path(&job.id, "spec.json"),
            &serde_json::to_vec(&spec).map_err(|e| fail(e.to_string()))?,
        )
        .map_err(|e| fail(format!("spec file: {e}")))?;
        let mut command = mast_harness::command(&self.supervisor.exe);
        command
            .args(&self.supervisor.args)
            .arg(&job.id)
            .env("MAST_BUILD_JOBS_DIR", &self.dir)
            .env_remove("MAST_AGENT_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and runs between fork and exec; it gives the
        // supervisor its own session and process group so it outlives the engine.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|e| fail(format!("could not start the supervisor: {e}")))?;
        let pid = child.id() as i64;
        let start_time = proc_start_time(pid);
        self.children
            .lock()
            .map_err(|e| fail(e.to_string()))?
            .insert(job.id.clone(), child);
        Ok(Started {
            pid: Some(pid),
            pgid: Some(pid),
            proc_start_time: start_time,
            remote_name: None,
        })
    }

    fn probe(&self, job: &Job) -> Probe {
        if let Ok(mut children) = self.children.lock()
            && let Some(child) = children.get_mut(&job.id)
            && matches!(child.try_wait(), Ok(Some(_)))
        {
            children.remove(&job.id);
        }
        if let Some(result) = read_exit(&self.path(&job.id, "exit")) {
            return result;
        }
        let Some(pid) = job.pid else {
            return Probe::Gone("the engine stopped before the supervisor was recorded".into());
        };
        let current = proc_start_time(pid);
        if current.is_some() && current == job.proc_start_time {
            return Probe::Running {
                pid: None,
                waiting_for_lock: waiting_for_lock(Path::new(&job.log_path)),
            };
        }
        // The supervisor may have written its record between the two checks.
        if let Some(result) = read_exit(&self.path(&job.id, "exit")) {
            return result;
        }
        match current {
            Some(_) => Probe::Gone(format!("pid {pid} now belongs to another process (reused)")),
            None => Probe::Gone("the supervisor ended without an exit record".into()),
        }
    }

    fn stop(&self, job: &Job, attempt: u32) -> Result<(), StopError> {
        let Some(pid) = job.pid else {
            return Err(StopError::Failed(
                "job has no recorded pid (it may still be starting)".into(),
            ));
        };
        if proc_start_time(pid).is_none() || proc_start_time(pid) != job.proc_start_time {
            return Err(StopError::Failed(format!(
                "pid {pid} is not this job's supervisor; nothing was signalled"
            )));
        }
        if attempt < 2 {
            // The supervisor forwards TERM to the command's group and sends KILL after 10 s.
            // SAFETY: plain kill(2) on a pid whose start time matched the recorded one.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            return Ok(());
        }
        // The supervisor did not end the command: kill the command's group, then the supervisor's
        // own group (it is its session leader). Both are groups this engine started.
        if let Some(child) = std::fs::read_to_string(self.path(&job.id, "child"))
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
        {
            // SAFETY: the command's own group, written by the supervisor that started it.
            unsafe { libc::kill(-child, libc::SIGKILL) };
        }
        // SAFETY: the supervisor's own group.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        Ok(())
    }
}

fn read_exit(path: &Path) -> Option<Probe> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<ExitRecord>(&bytes) {
        Ok(record) if record.version == EXIT_FILE_VERSION => Some(Probe::Exited(record)),
        Ok(record) => Some(Probe::Gone(format!(
            "exit record has unknown version {}",
            record.version
        ))),
        Err(error) => Some(Probe::Gone(format!("exit record unreadable: {error}"))),
    }
}

fn waiting_for_lock(log: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(log) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut tail = Vec::new();
    if file
        .seek(SeekFrom::Start(len.saturating_sub(4096)))
        .is_err()
        || file.read_to_end(&mut tail).is_err()
    {
        return false;
    }
    let text = String::from_utf8_lossy(&tail);
    // The notice is printed once while waiting; later build output means the lock was acquired.
    text.lines()
        .last()
        .is_some_and(|line| line.contains("Blocking waiting for file lock"))
}

/// Write `bytes` to `path` through a temp file, fsync and rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Start time of a live process as an opaque string, or `None` when there is no such process
/// (or it is a zombie). Field 22 of `/proc/<pid>/stat` where there is a `/proc`, else `ps`.
pub fn proc_start_time(pid: i64) -> Option<String> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // "pid (comm) S ..." — comm may hold spaces and parens, so split after the last ')'.
        let rest = stat.rsplit_once(')')?.1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.first() == Some(&"Z") {
            return None;
        }
        return fields.get(19).map(|s| s.to_string());
    }
    let output = mast_harness::command("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (output.status.success() && !text.is_empty()).then_some(text)
}

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_signal: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The supervisor: run the spec'd command, write its log and the exit file. Returns the
/// supervisor's own process exit code (`0` once an exit record is written).
pub fn supervise(dir: &Path, job_id: &str) -> i32 {
    let spec: SupervisorSpec = match std::fs::read(dir.join(format!("{job_id}.spec.json")))
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
    {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("build-job-supervise: cannot read the spec for {job_id}: {error}");
            return 2;
        }
    };
    let exit_path = dir.join(format!("{job_id}.exit"));
    let mut record = ExitRecord {
        version: EXIT_FILE_VERSION,
        started_at_ms: Some(now_ms()),
        ..Default::default()
    };
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let write_exit = |record: &ExitRecord| {
        let bytes = serde_json::to_vec(record).unwrap_or_default();
        if let Err(error) = write_atomic(&exit_path, &bytes) {
            eprintln!("build-job-supervise: cannot write the exit record: {error}");
            return 3;
        }
        0
    };
    let log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)
    {
        Ok(file) => file,
        Err(error) => {
            record.spawn_error = Some(format!("cannot open the log {}: {error}", spec.log_path));
            record.finished_at_ms = Some(now_ms());
            return write_exit(&record);
        }
    };
    let Some((program, args)) = spec.argv.split_first() else {
        record.spawn_error = Some("empty command".into());
        record.finished_at_ms = Some(now_ms());
        return write_exit(&record);
    };
    let mut command = mast_harness::command(program);
    command
        .args(args)
        .current_dir(&spec.cwd)
        .envs(&spec.env)
        .stdin(Stdio::null());
    match (log.try_clone(), log.try_clone()) {
        (Ok(out), Ok(err)) => {
            command.stdout(out).stderr(err);
        }
        _ => {
            record.spawn_error = Some("cannot duplicate the log handle".into());
            record.finished_at_ms = Some(now_ms());
            return write_exit(&record);
        }
    }
    // SAFETY: setpgid(0, 0) between fork and exec puts the command in its own group.
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            record.spawn_error = Some(format!("could not start {program}: {error}"));
            record.finished_at_ms = Some(now_ms());
            return write_exit(&record);
        }
    };
    let group = child.id() as i32;
    let _ = write_atomic(
        &dir.join(format!("{job_id}.child")),
        group.to_string().as_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(spec.timeout_secs);
    let mut terminated_at: Option<Instant> = None;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        let now = Instant::now();
        if terminated_at.is_none() {
            let reason = if now >= deadline {
                Some("timeout")
            } else if TERM.load(Ordering::SeqCst) {
                Some("cancel")
            } else {
                None
            };
            if let Some(reason) = reason {
                record.reason = Some(reason.into());
                // SAFETY: the command's own group, created by the pre_exec above.
                unsafe { libc::kill(-group, libc::SIGTERM) };
                terminated_at = Some(now);
            }
        } else if !killed && terminated_at.is_some_and(|t| now.duration_since(t) >= KILL_GRACE) {
            // SAFETY: as above.
            unsafe { libc::kill(-group, libc::SIGKILL) };
            killed = true;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(status) = status {
        record.exit_code = status.code();
        record.signal = status.signal();
    }
    record.finished_at_ms = Some(now_ms());
    write_exit(&record)
}

/// Free space on the filesystem a host builds on.
pub struct SystemDiskProbe {
    transport: Arc<dyn RemoteTransport>,
}

impl SystemDiskProbe {
    pub fn new(transport: Arc<dyn RemoteTransport>) -> Self {
        Self { transport }
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

impl super::DiskProbe for SystemDiskProbe {
    fn free_gib(&self, host: &HostProfile) -> Result<f64, String> {
        match &host.kind {
            HostKind::Local { probe_path } => {
                let path =
                    CString::new(probe_path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
                // SAFETY: `path` is NUL-terminated and `stat` is a plain out-parameter.
                let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
                if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
                    return Err(format!(
                        "statvfs {}: {}",
                        probe_path.display(),
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(stat.f_bavail as f64 * stat.f_frsize as f64 / GIB)
            }
            HostKind::Remote(remote) => {
                let out = self
                    .transport
                    .ssh_sh(
                        &remote.ssh,
                        "df -Pk \"$HOME\" | awk 'NR == 2 { print $4 }'",
                        &[],
                    )
                    .map_err(|e| format!("df on {}: {e}", remote.ssh))?;
                out.stdout
                    .trim()
                    .parse::<f64>()
                    .map(|kib| kib * 1024.0 / GIB)
                    .map_err(|_| format!("df on {} printed {:?}", remote.ssh, out.stdout.trim()))
            }
        }
    }
}
