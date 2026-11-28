//! SSH execution on a remote build host (the Fedora desktop) by delegating to
//! `scripts/remote-cargo.sh`.
//!
//! # Lazy slot checkouts (P10)
//!
//! Nothing is provisioned when a job is enqueued or while it waits. A slot's checkout
//! (`<checkout prefix><slot>`, a detached `git worktree` of the host's repository) is created, or
//! moved to the job's `head_sha`, only by the `start --checkout` call [`RemoteExecutor::start`]
//! makes when the scheduler dequeues the job. The script also writes the slot environment file
//! `~/Work/mast-slot-<slot>.env` (`CARGO_TARGET_DIR=<target prefix><slot>` and
//! `CARGO_INCREMENTAL`) unless an unmanaged file is already there. The commit must already exist
//! on the host (push the branch first); a missing commit fails the job with that reason instead
//! of building something else. The job builds the commit in the checkout root, not the (possibly
//! dirty) worktree it was enqueued from.
//!
//! # Ownership and fencing (enforced by the host)
//!
//! No engine owns a host. The script holds the slot's `flock` from its "is the slot idle" check,
//! across the checkout, until the new job's name is in the slot's job file; from then on the slot
//! is busy for as long as that job's runner is alive (PID plus start time). So two engines, two
//! devices or a hand-run `remote-cargo.sh start` sharing a slot can never move a checkout under
//! each other's job: the later one is refused with `slot X busy` and its job goes back to the
//! queue.
//!
//! # Lost connections
//!
//! Every start attempt has its own job name (`mastbj-<job id>-<attempt>`). When the answer to a
//! start is lost, `remote-cargo.sh fence <name>` settles it atomically on the host through the
//! job's claim file: `fenced` means the job has not run and now never will, even if the start
//! command is still on its way, so it is safe to queue the job again; `started` or `exit N`
//! means the host owns a job under that name, which is adopted. While the host cannot be
//! reached the job stays `running` without a pid, and every later probe fences first.
//!
//! The job itself is `scripts/remote-cargo.sh start ... -- cargo ...`; `status`, `wait` and
//! `stop` follow it. The start option comes from [`StartMode`]: the three-slot desktop uses
//! `--admit` (a job that carries a quiet-window lease uses `--quiet-window`), cutover mode uses
//! `--device-serial <serial>`, and `Plain` passes no admission option. The script's own refusals
//! are mapped: admission limits, a held lock, a busy slot or an unreachable host put the job back
//! in the queue: a refusal of the *slot* (`slot X busy`, `slot X lock timeout`) is
//! `StartError::Busy` and the job goes to the next free slot, a refusal of the *host* (admission
//! limits, held locks, ssh down) is `StartError::HostBusy` and pauses the host's starts for a
//! backoff; a slot, checkout or target-directory policy refusal fails the job.

use super::{
    HostKind, HostProfile, Job, StartMode,
    runner::{Executor, ExitRecord, Probe, StartCtx, StartError, Started, StopError},
};
use std::{collections::HashMap, io::Write, path::PathBuf, process::Stdio, sync::Arc};

/// Check that `alias` is an ssh *alias* (`desktop`), not `user@host`, an address or an option.
/// Raw addresses reintroduce host-key prompts that stall a job, and a leading `-` would be read
/// by ssh as an option; the script rejects the latter as well.
pub fn validate_ssh_alias(alias: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("ssh target {alias:?} is not an alias: {why}"));
    if alias.is_empty() || alias.starts_with('-') {
        return bad("empty or starts with '-'");
    }
    if !alias
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return bad("only letters, digits, '.', '_' and '-' are allowed (no user@host, no ':')");
    }
    if alias.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return bad("looks like an IP address");
    }
    Ok(())
}

/// Output of one script run.
#[derive(Debug, Clone, Default)]
pub struct ScriptOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// How the engine reaches the host. [`ProcessTransport`] is the real one; tests substitute a fake.
pub trait RemoteTransport: Send + Sync {
    /// Run `scripts/remote-cargo.sh <args...>`.
    fn remote_cargo(&self, args: &[String]) -> Result<ScriptOutput, String>;
    /// Run `script` as `sh -s -- <args>` on `ssh_target` (script on stdin).
    fn ssh_sh(
        &self,
        ssh_target: &str,
        script: &str,
        args: &[String],
    ) -> Result<ScriptOutput, String>;
}

pub struct ProcessTransport {
    script: PathBuf,
    env: Vec<(String, String)>,
    ssh: PathBuf,
}

impl ProcessTransport {
    /// `script` is the path of `scripts/remote-cargo.sh`.
    pub fn new(script: PathBuf) -> Self {
        Self {
            script,
            env: Vec::new(),
            ssh: PathBuf::from("ssh"),
        }
    }

    /// The ssh program for the engine's own ssh calls (`df`, pid and log fetch); the
    /// script runs whatever `ssh` is on its `PATH`. Tests substitute a fake.
    pub fn with_ssh(mut self, ssh: PathBuf) -> Self {
        self.ssh = ssh;
        self
    }

    /// Extra environment for the script and ssh child processes (tests point `HOME` and `PATH`).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }
}

fn output(out: std::process::Output) -> ScriptOutput {
    ScriptOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// What a lost answer looks like to the callers: exit 255, as when ssh itself fails.
fn lost_answer(why: String) -> ScriptOutput {
    ScriptOutput {
        code: 255,
        stdout: String::new(),
        stderr: why,
    }
}

/// Single-quote every argument so the remote login shell passes it through untouched.
pub(crate) fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("'{}' ", arg.replace('\'', "'\\''")))
        .collect()
}

impl RemoteTransport for ProcessTransport {
    fn remote_cargo(&self, args: &[String]) -> Result<ScriptOutput, String> {
        // `sh <missing file>` exits 127 like any failed job; name the real problem instead.
        if !self.script.is_file() {
            return Err(format!(
                "remote-cargo.sh not found at {} (pass an absolute path)",
                self.script.display()
            ));
        }
        // A spawn failure means the script cannot be run here (`Err`: fail the job). Once it
        // runs, trouble waiting for it is a lost answer like an ssh failure (exit 255).
        let child = mast_harness::command("sh")
            .arg(&self.script)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.script.display()))?;
        Ok(child
            .wait_with_output()
            .map(output)
            .unwrap_or_else(|e| lost_answer(format!("waiting for remote-cargo.sh: {e}"))))
    }

    fn ssh_sh(
        &self,
        ssh_target: &str,
        script: &str,
        args: &[String],
    ) -> Result<ScriptOutput, String> {
        let mut child = mast_harness::command(&self.ssh)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=30",
            ])
            .arg(ssh_target)
            .arg(format!("sh -s -- {}", quote_args(args)))
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        // ssh started: from here a write or wait error is a lost answer (exit 255, the job goes
        // back to the queue), not a reason to fail the job.
        if let Some(mut stdin) = child.stdin.take()
            && let Err(e) = stdin.write_all(script.as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(lost_answer(format!("writing to ssh: {e}")));
        }
        Ok(child
            .wait_with_output()
            .map(output)
            .unwrap_or_else(|e| lost_answer(format!("waiting for ssh: {e}"))))
    }
}

/// The host's answer to `fence`.
enum Fence {
    NeverRuns,
    Started,
    Unknown(String),
}

pub struct RemoteExecutor {
    transport: Arc<dyn RemoteTransport>,
    /// Host profile id -> ssh alias for the profiles this executor serves.
    ssh: HashMap<String, String>,
}

impl RemoteExecutor {
    pub fn new(transport: Arc<dyn RemoteTransport>, hosts: &[HostProfile]) -> Self {
        let ssh = hosts
            .iter()
            .filter_map(|host| match &host.kind {
                HostKind::Remote(remote) => Some((host.id.clone(), remote.ssh.clone())),
                HostKind::Local { .. } => None,
            })
            .collect();
        Self { transport, ssh }
    }

    fn ssh_of(&self, job: &Job) -> Option<String> {
        self.ssh.get(&job.host).cloned()
    }

    fn cargo(&self, args: Vec<String>) -> Result<ScriptOutput, String> {
        self.transport.remote_cargo(&args)
    }
}

fn s(text: &str) -> String {
    text.to_string()
}

/// `255` is ssh itself failing (also what the script reports for it).
fn unreachable(out: &ScriptOutput) -> bool {
    out.code == 255
}

impl Executor for RemoteExecutor {
    fn start(&self, ctx: &StartCtx<'_>) -> Result<Started, StartError> {
        let job = ctx.job;
        let HostKind::Remote(remote) = &ctx.host.kind else {
            return Err(StartError::Failed("not a remote host".into()));
        };
        if job.argv.first().map(String::as_str) != Some("cargo") {
            return Err(StartError::Failed(
                "only a cargo command runs on a remote build host".into(),
            ));
        }
        let Some(sha) = job.head_sha.as_deref().filter(|sha| !sha.is_empty()) else {
            return Err(StartError::Failed(
                "a remote build job needs headSha".into(),
            ));
        };
        let Some(name) = job.remote_name.clone() else {
            return Err(StartError::Failed("remote job name missing".into()));
        };
        let checkout = format!("{}{}", remote.checkout_prefix, ctx.slot.name);
        let target = format!("{}{}", remote.target_prefix, ctx.slot.name);
        let mut args = vec![s("start")];
        match (&remote.mode, job.lease_id.is_some()) {
            (StartMode::Admit, true) => args.push(s("--quiet-window")),
            (StartMode::Admit, false) => args.push(s("--admit")),
            (StartMode::DeviceSerial(serial), _) => {
                args.extend([s("--device-serial"), serial.clone()])
            }
            (StartMode::Plain, _) => {}
        }
        // P10: the checkout happens inside the start, under the host's slot lock.
        args.extend([
            s("--checkout"),
            remote.repo.clone(),
            sha.to_string(),
            target,
            s(if ctx.slot.incremental { "1" } else { "0" }),
        ]);
        args.extend([
            remote.ssh.clone(),
            checkout,
            ctx.slot.name.clone(),
            name.clone(),
            s("--"),
        ]);
        args.extend(job.argv.iter().cloned());
        // A start whose answer was lost may still have started: ask the host before deciding.
        let out = match self.cargo(args) {
            Ok(out) if !unreachable(&out) => out,
            // Exit 255 is ssh itself failing: the answer was lost and the job may be running.
            Ok(_) => return self.confirm_start(&remote.ssh, &name, "the start answer was lost"),
            // The script could not be run on this machine (missing path, spawn failure): the
            // job never started, and retrying would fail the same way, so fail it visibly.
            Err(error) => {
                return Err(StartError::Failed(format!(
                    "cannot run remote-cargo.sh: {error}"
                )));
            }
        };
        // Warnings and notes come before the verdict and must not be read as one.
        let text = verdict(&out.stdout);
        match out.code {
            0 => Ok(Started {
                // `Some(0)` = started on the host, pid not read (never treated as not started).
                pid: parse_pid(&text).or(Some(0)),
                pgid: None,
                proc_start_time: None,
                remote_name: Some(name),
            }),
            1 if text.contains("name in use") => {
                self.confirm_start(&remote.ssh, &name, "the job name is already in use")
            }
            1 if is_slot_refusal(&text) => Err(StartError::Busy(text)),
            1 if is_transient_refusal(&text) => Err(StartError::HostBusy(text)),
            _ => Err(StartError::Failed(first_line(&text, &out.stderr))),
        }
    }

    fn probe(&self, job: &Job) -> Probe {
        let (Some(name), Some(host)) = (job.remote_name.clone(), self.ssh_of(job)) else {
            return Probe::Gone("remote job name was not recorded".into());
        };
        // A start that was never confirmed is settled first: fenced off, it can safely run
        // again under a new name; otherwise the host has the job and `status` describes it.
        if job.pid.is_none() {
            match self.fence(&host, &name) {
                Fence::NeverRuns => return Probe::NotStarted,
                Fence::Started => {}
                Fence::Unknown(why) => return Probe::Unknown(why),
            }
        }
        let out = match self.cargo(vec![s("status"), host.clone(), name.clone()]) {
            Ok(out) if !unreachable(&out) => out,
            Ok(out) => return Probe::Unknown(out.stderr.trim().to_string()),
            Err(error) => return Probe::Unknown(error),
        };
        let text = out.stdout.trim();
        if text == "running" {
            // A job claimed without a pid (its start was confirmed late) learns it now.
            let pid = job
                .pid
                .is_none()
                .then(|| self.read_pid(&host, &name))
                .flatten();
            return Probe::Running {
                waiting_for_lock: false,
                pid,
            };
        }
        if let Some(code) = text
            .strip_prefix("exit ")
            .and_then(|c| c.trim().parse::<i32>().ok())
        {
            return Probe::Exited(self.collect(job, &host, &name, code));
        }
        match text {
            "died" => Probe::Gone("the job died on the host without an exit marker".into()),
            "unknown" => Probe::Gone("the host no longer knows this job (rebooted?)".into()),
            other => Probe::Unknown(format!("unexpected status output {other:?}")),
        }
    }

    fn stop(&self, job: &Job, _attempt: u32) -> Result<(), StopError> {
        let (Some(name), Some(host)) = (job.remote_name.clone(), self.ssh_of(job)) else {
            return Err(StopError::Failed("remote job name was not recorded".into()));
        };
        let out = self
            .cargo(vec![s("stop"), host, name])
            .map_err(StopError::Failed)?;
        // The script's own stop sends TERM, then KILL after ten seconds, so a retry is a fresh
        // attempt. Only exit 0 (stopped, or already finished) counts as done: 2 means the host does
        // not know the job yet (a start still in flight), 1 that it is not the job's own process.
        // Exit 255 is ssh failing: nothing was learned, so it must not count toward the cap.
        match out.code {
            0 => Ok(()),
            255 => Err(StopError::Unreachable(out.stderr.trim().to_string())),
            1 => Err(StopError::Failed(format!(
                "not stopped: {}",
                first_line(&out.stdout, &out.stderr)
            ))),
            2 => Err(StopError::Failed(
                "the host does not know this job yet".into(),
            )),
            code => Err(StopError::Failed(format!(
                "remote stop exited {code}: {}",
                out.stderr.trim()
            ))),
        }
    }
}

impl RemoteExecutor {
    /// Ask the host to settle whether the job `name` ever runs (see the module docs).
    fn fence(&self, ssh: &str, name: &str) -> Fence {
        match self.cargo(vec![s("fence"), ssh.to_string(), name.to_string()]) {
            Ok(out) if out.code == 0 => match out.stdout.trim() {
                "fenced" => Fence::NeverRuns,
                text if text == "started" || text.starts_with("exit ") => Fence::Started,
                text => Fence::Unknown(format!("unexpected fence output {text:?}")),
            },
            Ok(out) => Fence::Unknown(first_line(&out.stdout, &out.stderr)),
            Err(error) => Fence::Unknown(error),
        }
    }

    /// Decide what an unconfirmed `start` did: a job the host has is adopted; one fenced off is
    /// queued again; an unreachable host leaves the job claimed with no pid, and `probe` settles
    /// it later.
    fn confirm_start(&self, ssh: &str, name: &str, why: &str) -> Result<Started, StartError> {
        match self.fence(ssh, name) {
            Fence::Started => Ok(self.confirmed(ssh, name)),
            Fence::NeverRuns => Err(StartError::HostBusy(format!(
                "{why}; the job never started and is fenced off"
            ))),
            Fence::Unknown(_) => Ok(Started {
                remote_name: Some(name.to_string()),
                ..Started::default()
            }),
        }
    }

    /// A start the host confirmed. The pid is recorded (read from the job's pid file, `0` when it
    /// cannot be read) so that a host that later forgets the job, after a reboot cleared `/tmp`,
    /// reads as `lost` and is never mistaken for a start that did not happen and run again.
    fn confirmed(&self, ssh: &str, name: &str) -> Started {
        Started {
            pid: self.read_pid(ssh, name).or(Some(0)),
            remote_name: Some(name.to_string()),
            ..Started::default()
        }
    }

    /// The job's pid from its pid file on the host, if it can be read.
    fn read_pid(&self, ssh: &str, name: &str) -> Option<i64> {
        self.transport
            .ssh_sh(
                ssh,
                "cat \"/tmp/$1.pid\" 2>/dev/null || true",
                &[name.to_string()],
            )
            .ok()
            .and_then(|out| out.stdout.trim().parse::<i64>().ok())
            .filter(|pid| *pid > 0)
    }

    /// Fetch the trimmed report and the log tail of a finished remote job into the local log.
    fn collect(&self, job: &Job, host: &str, name: &str, code: i32) -> ExitRecord {
        let report = self
            .cargo(vec![s("wait"), host.to_string(), name.to_string(), s("1s")])
            .ok()
            .map(|out| out.stdout.lines().skip(1).collect::<Vec<_>>().join("\n"))
            .filter(|text| !text.trim().is_empty());
        if let Ok(log) = self.transport.ssh_sh(
            host,
            "tail -c 2097152 \"/tmp/$1.log\" 2>/dev/null || true",
            &[name.to_string()],
        ) {
            let _ = std::fs::write(&job.log_path, log.stdout);
        }
        ExitRecord {
            version: 1,
            exit_code: Some(code),
            summary: report,
            ..Default::default()
        }
    }
}

/// A refusal that is about one slot (`slot b busy: ...`, `slot b lock timeout`): the next slot may
/// still take the job. Everything else in [`is_transient_refusal`] is about the whole host.
fn is_slot_refusal(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("slot ") && (line.contains(" busy") || line.contains("lock timeout"))
    })
}

/// Refusals that clear by themselves: a busy slot, a held lock, the admission limits.
fn is_transient_refusal(text: &str) -> bool {
    [
        "busy",
        "lock timeout",
        "already run",
        "is held",
        "quiet window",
        "idle host",
        "below the",
        "MemAvailable",
        "admission lock",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// The script's output without its `warning:` and `note:` lines.
fn verdict(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| !line.starts_with("warning:") && !line.starts_with("note:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn parse_pid(text: &str) -> Option<i64> {
    let rest = text.split(" pid ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn first_line(stdout: &str, stderr: &str) -> String {
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("remote command failed")
        .trim()
        .to_string()
}
