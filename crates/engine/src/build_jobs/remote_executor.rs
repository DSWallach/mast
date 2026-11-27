//! SSH execution on a remote build host (the Fedora desktop) by delegating to
//! `scripts/remote-cargo.sh`.
//!
//! # Lazy slot checkouts (P10)
//!
//! Nothing is provisioned when a job is enqueued or while it waits. A slot's checkout
//! (`~/Work/mast-wt-build-<slot>`, a detached `git worktree` of `~/Work/mast`) is created, or
//! moved to the job's `head_sha`, only in [`RemoteExecutor::start`], that is, when the scheduler
//! dequeues the job for execution. The same step writes the slot environment file
//! `~/Work/mast-slot-<slot>.env` (`CARGO_TARGET_DIR=~/Work/mast-target-<slot>` and
//! `CARGO_INCREMENTAL`, `1` on the integration slot `a` only) unless an unmanaged file is already
//! there. The commit must already exist on the host (push the branch first); a missing commit
//! fails the job with that reason instead of building something else. The job builds the commit
//! in the checkout root, not the (possibly dirty) worktree it was enqueued from.
//!
//! The job itself is `scripts/remote-cargo.sh start ... -- cargo ...` under the name
//! `mastbj-<job id>`; `status`, `wait` and `stop` follow it. The start option comes from
//! [`StartMode`]: the three-slot desktop uses `--admit` (a job that carries a quiet-window lease
//! uses `--quiet-window`), cutover mode uses `--device-serial <serial>`, and `Plain` passes no
//! option. The script's own refusals are mapped: admission limits, a held lock, a busy slot or
//! an unreachable host put the job back in the queue (`StartError::Busy`); a slot, checkout or
//! target-directory policy refusal fails the job.

use super::{
    HostKind, HostProfile, Job, StartMode,
    runner::{Executor, ExitRecord, Probe, StartCtx, StartError, Started},
};
use std::{collections::HashMap, io::Write, path::PathBuf, process::Stdio, sync::Arc};

/// Creates or moves the slot checkout, strictly while the slot is idle. It takes the script's own
/// slot lock (`<prefix><slot>.lock`) and refuses (exit 7) when the slot's job file names a job
/// that is still alive, so a checkout is never moved under a running cargo. Its own job name
/// (a retry after a dropped start) is the one exception: nothing is touched and it exits 0 with
/// `already running`. `alive`/`starttime` are the script's own liveness check (PID plus start
/// time), copied so this runs outside the script.
pub(super) const ENSURE_CHECKOUT: &str = r#"set -eu
repo=$HOME/$1 dir=$HOME/$2 sha=$3 slot=$4 incr=$5 target=$6 name=$7
pfx=${REMOTE_CARGO_SLOT_PREFIX:-/tmp/remote-cargo-slot-}
starttime() {
    if [ -r "/proc/$1/stat" ]; then
        s=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
        rp=")"
        ( set -- ${s##*"$rp" }; eval "echo \${20}" )
    else
        ps -o lstart= -p "$1" 2>/dev/null | tr -s ' '
    fi
}
alive() {
    [ -n "$1" ] || return 1
    p=$(cat "/tmp/$1.pid" 2>/dev/null) || return 1
    { [ -n "$p" ] && kill -0 "$p" 2>/dev/null; } || return 1
    [ -s "/tmp/$1.start" ] || return 0
    [ "$(starttime "$p")" = "$(cat "/tmp/$1.start")" ]
}
exec 9> "${pfx}$slot.lock"
if ! flock -w 10 9; then echo "slot $slot lock timeout"; exit 7; fi
old=$(cat "${pfx}$slot.job" 2>/dev/null || true)
if [ -n "$old" ] && [ ! -e "/tmp/$old.exit" ] && alive "$old"; then
    if [ "$old" = "$name" ]; then echo "already running $name"; exit 0; fi
    echo "slot $slot busy: $old"; exit 7
fi
[ -e "$repo/.git" ] || { echo "no repository at $repo on this host"; exit 3; }
git -C "$repo" cat-file -e "$sha^{commit}" 2>/dev/null || { echo "commit $sha is not on this host; push the branch to it first"; exit 4; }
if [ ! -e "$dir/.git" ]; then
    git -C "$repo" worktree add --detach "$dir" "$sha" >/dev/null 2>&1 || { echo "could not create the slot checkout $dir"; exit 5; }
fi
if [ "$(git -C "$dir" rev-parse HEAD)" != "$sha" ]; then
    git -C "$dir" checkout --detach --force "$sha" >/dev/null 2>&1 || { echo "could not check out $sha in $dir"; exit 6; }
fi
mkdir -p "$HOME/Work"
envf=$HOME/Work/mast-slot-$slot.env
marker='# managed by mast build_jobs'
if [ ! -e "$envf" ] || head -n 1 "$envf" | grep -qF "$marker"; then
    { echo "$marker"; echo "CARGO_TARGET_DIR=\$HOME/$target"; echo "CARGO_INCREMENTAL=$incr"; echo "export CARGO_TARGET_DIR CARGO_INCREMENTAL"; } > "$envf.tmp.$$" && mv "$envf.tmp.$$" "$envf"
else
    echo "note: leaving the unmanaged $envf in place"
fi
echo "ready $dir at $sha"
"#;

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

    /// The ssh program for the engine's own ssh calls (the checkout step, `df`, log fetch); the
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

/// Single-quote every argument so the remote login shell passes it through untouched.
pub(crate) fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("'{}' ", arg.replace('\'', "'\\''")))
        .collect()
}

impl RemoteTransport for ProcessTransport {
    fn remote_cargo(&self, args: &[String]) -> Result<ScriptOutput, String> {
        mast_harness::command("sh")
            .arg(&self.script)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .output()
            .map(output)
            .map_err(|e| format!("cannot run {}: {e}", self.script.display()))
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
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(script.as_bytes())
                .map_err(|e| format!("ssh stdin: {e}"))?;
        }
        child
            .wait_with_output()
            .map(output)
            .map_err(|e| format!("ssh: {e}"))
    }
}

pub struct RemoteExecutor {
    transport: Arc<dyn RemoteTransport>,
    /// Host profile id -> `user@host` for the profiles this executor serves.
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
        // P10: the slot checkout exists only from the moment a job is dequeued for it, and it is
        // only touched while the slot is idle (the script refuses with exit 7 otherwise).
        let ensure = self
            .transport
            .ssh_sh(
                &remote.ssh,
                ENSURE_CHECKOUT,
                &[
                    remote.repo.clone(),
                    checkout.clone(),
                    sha.to_string(),
                    ctx.slot.name.clone(),
                    if ctx.slot.incremental { "1" } else { "0" }.to_string(),
                    target,
                    name.clone(),
                ],
            )
            .map_err(|e| StartError::Busy(format!("host unreachable: {e}")))?;
        if unreachable(&ensure) {
            return Err(StartError::Busy(format!(
                "host unreachable: {}",
                ensure.stderr.trim()
            )));
        }
        match ensure.code {
            0 => {}
            7 => return Err(StartError::Busy(first_line(&ensure.stdout, &ensure.stderr))),
            _ => {
                return Err(StartError::Failed(first_line(
                    &ensure.stdout,
                    &ensure.stderr,
                )));
            }
        }
        if ensure.stdout.trim_start().starts_with("already running") {
            // A retry whose first start did succeed: adopt it, touch nothing.
            return Ok(Started {
                remote_name: Some(name),
                ..Started::default()
            });
        }
        let mut args = vec![s("start")];
        match (&remote.mode, job.lease_id.is_some()) {
            (StartMode::Admit, true) => args.push(s("--quiet-window")),
            (StartMode::Admit, false) => args.push(s("--admit")),
            (StartMode::DeviceSerial(serial), _) => {
                args.extend([s("--device-serial"), serial.clone()])
            }
            (StartMode::Plain, _) => {}
        }
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
            _ => return self.confirm_start(&remote.ssh, &name, "the start answer was lost"),
        };
        let text = out.stdout.trim().to_string();
        match out.code {
            0 => Ok(Started {
                pid: parse_pid(&text),
                pgid: None,
                proc_start_time: None,
                remote_name: Some(name),
            }),
            1 if text.contains("name in use") => {
                self.confirm_start(&remote.ssh, &name, "the job name is already in use")
            }
            1 if is_transient_refusal(&text) => Err(StartError::Busy(text)),
            _ => Err(StartError::Failed(first_line(&text, &out.stderr))),
        }
    }

    fn probe(&self, job: &Job) -> Probe {
        let (Some(name), Some(host)) = (job.remote_name.clone(), self.ssh_of(job)) else {
            return Probe::Gone("remote job name was not recorded".into());
        };
        let out = match self.cargo(vec![s("status"), host.clone(), name.clone()]) {
            Ok(out) if !unreachable(&out) => out,
            Ok(out) => return Probe::Unknown(out.stderr.trim().to_string()),
            Err(error) => return Probe::Unknown(error),
        };
        let text = out.stdout.trim();
        if text == "running" {
            return Probe::Running {
                waiting_for_lock: false,
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
            // A start that was never confirmed (no pid recorded) and that the host has never heard of
            // did not happen: requeue it. A confirmed job the host forgot was lost.
            "unknown" if job.pid.is_none() => Probe::NotStarted,
            "unknown" => Probe::Gone("the host no longer knows this job (rebooted?)".into()),
            other => Probe::Unknown(format!("unexpected status output {other:?}")),
        }
    }

    fn stop(&self, job: &Job, _attempt: u32) -> Result<(), String> {
        let (Some(name), Some(host)) = (job.remote_name.clone(), self.ssh_of(job)) else {
            return Err("remote job name was not recorded".into());
        };
        let out = self.cargo(vec![s("stop"), host, name])?;
        // The script's own stop sends TERM, then KILL after ten seconds, so a retry is a fresh
        // attempt. Only exit 0 (stopped, or already finished) counts as done: 2 means the host does
        // not know the job yet (a start still in flight), 1 that it is not the job's own process.
        match out.code {
            0 => Ok(()),
            1 => Err(format!(
                "not stopped: {}",
                first_line(&out.stdout, &out.stderr)
            )),
            2 => Err("the host does not know this job yet".into()),
            code => Err(format!("remote stop exited {code}: {}", out.stderr.trim())),
        }
    }
}

impl RemoteExecutor {
    /// Decide what an unconfirmed `start` did, from the host's own `status` of the job name:
    /// running or finished means it started (adopt it); a name the host does not know means it
    /// did not (requeue); an unreachable host leaves the job claimed with no pid, and `probe`
    /// settles it later (`NotStarted` requeues, anything else follows the normal path).
    fn confirm_start(&self, ssh: &str, name: &str, why: &str) -> Result<Started, StartError> {
        let adopt = || Started {
            remote_name: Some(name.to_string()),
            ..Started::default()
        };
        match self.cargo(vec![s("status"), ssh.to_string(), name.to_string()]) {
            Ok(out) if !unreachable(&out) => {
                let text = out.stdout.trim();
                if text == "running" || text.starts_with("exit ") {
                    Ok(adopt())
                } else if text == "unknown" {
                    Err(StartError::Busy(format!("{why}; the host has no such job")))
                } else {
                    Err(StartError::Failed(format!("{why} but the job is {text}")))
                }
            }
            _ => Ok(adopt()),
        }
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
