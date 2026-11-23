//! `run_command` / `command_status`: a long command hosted by the engine so a
//! worker can end its turn instead of polling. The command writes to a log
//! file and its wrapper shell records the exit code in a sidecar file. The
//! calling Chat is woken once by a queued message whose id derives from the
//! run, and a `.woken` marker written only after delivery (or after a
//! deliberate skip for an archived chat) lets every start-up retry a wake that
//! was lost, without ever sending a second one.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use mast_proto::SandboxLevel;
use mast_proto::agent_tools::AgentToolErrorCode;
use mast_proto::work_outline::derive_id;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolsService, CallContext, lock, parse_args};
use crate::permission_rules::{PermissionRequest, RuleDecision};

const LOG_TAIL_BYTES: u64 = 64 * 1024;
const SUMMARY_TAIL_LINES: usize = 12;
const SUMMARY_MATCH_LINES: usize = 12;
const SUMMARY_LINE_CHARS: usize = 240;
const MAX_COMMAND_CHARS: usize = 8 * 1024;
/// Commands one chat may have running at once.
const MAX_RUNS_PER_CHAT: usize = 4;
/// A log rotates to `<run>.log.old` once it reaches this size, so a run keeps
/// at most about twice this on disk (the newest output, which is what a
/// failure summary reads).
const LOG_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
/// How long the exit wake waits for the last output to reach the log.
const PUMP_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// A run's files are removed this long after its wake was delivered.
const RUN_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long an orphaned command gets to stop after SIGTERM before SIGKILL.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(2);
/// What a shell records for a command killed by SIGPIPE (128 + 13).
const EXIT_SIGPIPE: i32 = 141;
/// How often, and how far apart, a wake retries a refused send (a Manager
/// hand-off still completing is the usual reason) before the next start.
const SEND_RETRIES: usize = 5;
const SEND_RETRY_DELAY: Duration = Duration::from_secs(2);

/// A run this process started and has not seen finish.
struct LiveEntry {
    chat_id: String,
    started_ms: i64,
}

/// Run ids this process started and has not seen finish, with the chat that
/// started each. Process-wide, because the engine builds more than one
/// service instance.
fn live_runs() -> &'static Mutex<HashMap<String, LiveEntry>> {
    static LIVE: OnceLock<Mutex<HashMap<String, LiveEntry>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Keeps a run in the live map until it drops, so the entry goes however the
/// run's thread ends, including a panic.
struct LiveRun(String);

impl LiveRun {
    fn track(run_id: &str, chat_id: &str, started_ms: i64) -> Self {
        lock(live_runs()).insert(run_id.to_owned(), LiveEntry { chat_id: chat_id.to_owned(), started_ms });
        Self(run_id.to_owned())
    }
}

impl Drop for LiveRun {
    fn drop(&mut self) {
        lock(live_runs()).remove(&self.0);
    }
}

/// Delivers the exit wake while the run still counts as live, so no nudge can
/// slip in between the exit and the wake being queued. The run stops counting
/// as soon as the delivery ends, whether it succeeded, failed or panicked
/// (a failed one is retried at the next start).
fn finish_run(live: LiveRun, deliver: impl FnOnce()) {
    let _live = live;
    deliver();
}

/// Whether `chat_id` has a command running that it is waiting on: its single
/// exit wake is coming, so the chat is waiting rather than stalled. A command
/// silences nudges for at most an hour; a longer one still wakes the chat at
/// its exit, but a chat that stopped waiting for it is no longer exempt.
pub fn chat_has_running_command(chat_id: &str) -> bool {
    chat_has_running_command_at(chat_id, chrono::Utc::now().timestamp_millis())
}

/// [`chat_has_running_command`] at an explicit time.
pub fn chat_has_running_command_at(chat_id: &str, now_ms: i64) -> bool {
    let limit_ms = (super::await_background::MAX_MINUTES as i64) * 60_000;
    lock(live_runs())
        .values()
        .any(|run| run.chat_id == chat_id && now_ms.saturating_sub(run.started_ms) < limit_ms)
}

/// Marks a command as running for `chat_id` until the guard drops, so tests
/// can exercise the waiting signal without spawning a process.
#[doc(hidden)]
pub fn track_running_command_for_test(chat_id: &str, run_id: &str) -> impl Drop {
    track_running_command_since_for_test(chat_id, run_id, chrono::Utc::now().timestamp_millis())
}

/// [`track_running_command_for_test`] for a command that started at `started_ms`.
#[doc(hidden)]
pub fn track_running_command_since_for_test(chat_id: &str, run_id: &str, started_ms: i64) -> impl Drop {
    LiveRun::track(run_id, chat_id, started_ms)
}

/// Makes "no run record yet, so start it" atomic across concurrent calls.
static START_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
pub(super) struct CommandRuns;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunMeta {
    chat_id: String,
    key: String,
    command: String,
    /// The engine process that started the run; only a dead one's runs are
    /// recovered as interrupted.
    #[serde(default)]
    engine_pid: u32,
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// The runs directory of one engine (scoped by its device id, so profiles and
/// tests never see each other's runs), private to the user.
fn runs_dir(device_id: &str) -> Result<PathBuf, AgentToolError> {
    let dir = std::env::temp_dir().join(format!("mast-command-runs-{device_id}"));
    let invalid = |e: std::io::Error| AgentToolError::invalid(format!("cannot prepare the runs directory: {e}"));
    std::fs::create_dir_all(&dir).map_err(invalid)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(&dir).map_err(invalid)?;
        // SAFETY: geteuid has no preconditions.
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(AgentToolError::invalid("the runs directory is not owned by this user"));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(invalid)?;
    }
    Ok(dir)
}

/// Create a private file that must not exist and is never reached through a
/// symlink.
fn create_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[derive(Clone)]
struct RunPaths {
    log: PathBuf,
    /// The previous segment of a log that rotated.
    log_old: PathBuf,
    exit: PathBuf,
    meta: PathBuf,
    /// The wrapper shell's pid, written once the command has started.
    pid: PathBuf,
    interrupted: PathBuf,
    woken: PathBuf,
}

impl RunPaths {
    fn new(dir: &Path, run_id: &str) -> Self {
        let file = |ext: &str| dir.join(format!("{run_id}.{ext}"));
        Self {
            log: file("log"),
            log_old: file("log.old"),
            exit: file("exit"),
            meta: file("meta"),
            pid: file("pid"),
            interrupted: file("interrupted"),
            woken: file("woken"),
        }
    }

    fn meta(&self) -> Option<RunMeta> {
        serde_json::from_slice(&std::fs::read(&self.meta).ok()?).ok()
    }

    fn exit_code(&self) -> Option<i32> {
        std::fs::read_to_string(&self.exit).ok()?.trim().parse().ok()
    }
}

/// Short failure summary of a finished run: lines that look like failures,
/// then the last lines, each cut to a readable width.
pub(super) fn failure_summary(log: &str) -> String {
    let cut = |line: &str| -> String { line.trim_end().chars().take(SUMMARY_LINE_CHARS).collect() };
    let lines: Vec<&str> = log.lines().collect();
    let interesting = |line: &&&str| {
        let lower = line.to_ascii_lowercase();
        lower.contains("fail") || lower.contains("panicked") || lower.starts_with("error")
    };
    let mut out: Vec<String> = lines.iter().filter(interesting).take(SUMMARY_MATCH_LINES).map(|l| cut(l)).collect();
    if !out.is_empty() {
        out.push("...".to_owned());
    }
    let start = lines.len().saturating_sub(SUMMARY_TAIL_LINES);
    out.extend(lines[start..].iter().map(|l| cut(l)));
    out.join("\n")
}

/// The last `max` bytes of a file (all of it when shorter).
fn tail_bytes(path: &Path, max: u64) -> Vec<u8> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(max))).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    bytes
}

/// The end of a run's output. A log that has just rotated is short, so the
/// previous segment completes the tail.
fn read_tail(paths: &RunPaths) -> String {
    let mut bytes = tail_bytes(&paths.log, LOG_TAIL_BYTES);
    let missing = LOG_TAIL_BYTES.saturating_sub(bytes.len() as u64);
    if missing > 0 {
        let mut earlier = tail_bytes(&paths.log_old, missing);
        earlier.append(&mut bytes);
        bytes = earlier;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Copies a command's output into its log. At `segment_limit` bytes the log
/// moves to `log_old` (replacing the previous one) and a fresh log starts, so
/// a runaway command cannot fill the disk. Always drains the pipe, so the
/// command never blocks on a full pipe, whatever happens to the log.
fn pump_log(mut source: impl Read, log: std::fs::File, paths: &RunPaths, segment_limit: u64) {
    let mut log = Some(log);
    let mut written = 0u64;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = match source.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        if written >= segment_limit {
            let _ = std::fs::rename(&paths.log, &paths.log_old);
            log = create_new(&paths.log).ok();
            written = 0;
        }
        if let Some(file) = log.as_mut() {
            let _ = file.write_all(&buffer[..read]);
        }
        written += read as u64;
    }
}

/// Removes the files of finished runs: those whose wake was delivered (or
/// that have no record at all) and that nothing has touched for `retention`.
/// A run this process is running, or one still waiting for a wake retry, is
/// never removed.
fn prune_runs(dir: &Path, now: SystemTime, retention: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut runs: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if let Some((run_id, _)) = name.split_once('.') {
            runs.entry(run_id.to_owned()).or_default().push(entry.path());
        }
    }
    for (run_id, files) in runs {
        let paths = RunPaths::new(dir, &run_id);
        if lock(live_runs()).contains_key(&run_id) || (paths.meta.exists() && !paths.woken.exists()) {
            continue;
        }
        let newest = files
            .iter()
            .filter_map(|file| std::fs::symlink_metadata(file).ok()?.modified().ok())
            .max();
        if newest.is_some_and(|newest| now.duration_since(newest).is_ok_and(|age| age >= retention)) {
            for file in files {
                let _ = std::fs::remove_file(file);
            }
        }
    }
}

/// Whether `pid` is the wrapper shell of the run with this id. The id is in
/// the wrapper's command line (as `$0`), so a recycled pid is never mistaken
/// for it.
#[cfg(unix)]
fn is_run_wrapper(pid: i32, run_id: &str) -> bool {
    mast_harness::command("ps")
        // `-ww`: BSD ps cuts a command line at the terminal width otherwise,
        // and the run's name sits after the wrapper script.
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&wrapper_name(run_id)))
}

/// `$0` of a run's wrapper shell.
fn wrapper_name(run_id: &str) -> String {
    format!("mast-run-command-{run_id}")
}

#[cfg(unix)]
fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks that the group exists.
    unsafe { libc::kill(-pgid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Stops a command an engine that died left running: the wrapper shell's
/// process group gets SIGTERM, then SIGKILL after a grace period. Does nothing
/// unless the recorded pid is still that run's wrapper.
#[cfg(unix)]
fn terminate_orphan(paths: &RunPaths, run_id: &str) {
    let Some(pid) = std::fs::read_to_string(&paths.pid)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 1)
    else {
        return;
    };
    if !is_run_wrapper(pid, run_id) {
        // Normal when the command already ended; a recycled pid looks the same.
        tracing::debug!(pid, run = %run_id, "command-run pid is not this run's wrapper; nothing to stop");
        return;
    }
    // SAFETY: the wrapper was started as the leader of its own process group
    // (`process_group(0)`), so `-pid` addresses exactly the run's processes.
    unsafe { libc::kill(-pid, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + KILL_GRACE;
    while group_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if group_alive(pid) {
        // SAFETY: as above.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

#[cfg(not(unix))]
fn terminate_orphan(_paths: &RunPaths, _run_id: &str) {}

/// How a run ended, for the wake message.
enum Outcome {
    Exited(i32),
    Interrupted,
}

fn wake_text(key: &str, outcome: &Outcome, paths: &RunPaths) -> String {
    let log = paths.log.display();
    match outcome {
        Outcome::Interrupted => format!(
            "Command run `{key}` was interrupted: the engine restarted before it finished, so it has no exit code. Log so far: {log}\nStart it again with a new idempotencyKey."
        ),
        Outcome::Exited(0) => {
            let tail = read_tail(paths);
            let lines: Vec<&str> = tail.lines().collect();
            let start = lines.len().saturating_sub(3);
            format!("Command run `{key}` succeeded. Log: {log}\n{}", lines[start..].join("\n"))
        }
        Outcome::Exited(code) => format!(
            "Command run `{key}` failed with exit code {code}. Log: {log}\n{}",
            failure_summary(&read_tail(paths))
        ),
    }
}

impl AgentToolsService {
    fn runs_dir(&self) -> Result<PathBuf, AgentToolError> {
        let doc_host = self
            .doc_host
            .as_ref()
            .ok_or_else(|| AgentToolError::refused(AgentToolErrorCode::Unsupported, "no chat host"))?;
        runs_dir(doc_host.device_id())
    }

    /// Check the caller may start this command. The command runs outside the
    /// agent's sandbox, so it is allowed only where the harness would run it
    /// without asking: never from a read-only chat, never from an Ask-mode
    /// chat, and in Rules mode only when an allow rule covers it. A deny rule
    /// always wins, and missing rules fail closed. Returns the chat's cwd.
    fn authorize_command(&self, context: &CallContext, command: &str) -> Result<Option<String>, AgentToolError> {
        let chat = self
            .workspace
            .chat(&context.grant.chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| AgentToolError::invalid("the Chat no longer exists"))?;
        let forbidden = |message: &str| AgentToolError::refused(AgentToolErrorCode::Forbidden, message);
        if self.workspace.manager_runs_read_only(&context.grant.chat_id) {
            return Err(forbidden("a read-only Manager cannot start commands"));
        }
        let Some(config) = chat.config.as_ref().filter(|c| c.sandbox != SandboxLevel::ReadOnly) else {
            return Err(forbidden("a read-only chat cannot start commands"));
        };
        // Turning the setting on stops commands at once (above). Turning it off
        // does not reach a run that started read-only, as it does not reach the
        // harness's own sandbox for that run: it applies from the next run.
        if self.workspace.chat_is_manager(&context.grant.chat_id)
            && lock(&self.sessions)
                .as_ref()
                .and_then(|sessions| sessions.last_request(&context.grant.chat_id))
                .is_some_and(|request| request.sandbox == SandboxLevel::ReadOnly)
        {
            return Err(forbidden(
                "this run started read-only; a change that allows commands applies from its next run",
            ));
        }
        let rules = lock(&self.sessions)
            .as_ref()
            .map(|s| s.permission_rules())
            .ok_or_else(|| forbidden("permission rules are unavailable"))?;
        let request = PermissionRequest::Command { command: Value::String(command.to_owned()) };
        let decision = rules.decide(&request);
        if let RuleDecision::Deny(rule) = &decision {
            return Err(forbidden(&format!("denied by permission rule {rule}")));
        }
        let allowed = config.auto_approve || (config.approval_rules && matches!(decision, RuleDecision::Allow(_)));
        if !allowed {
            return Err(forbidden(
                "this chat would ask before running the command; run it through the harness shell, or use an auto-approve or Rules chat with an allow rule",
            ));
        }
        Ok(chat.cwd)
    }

    pub(super) fn run_command(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: RunArgs = parse_args(arguments)?;
        let key = key_of(arguments);
        let command = args.command.trim().to_owned();
        if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
            return Err(AgentToolError::invalid(format!("command must be 1 to {MAX_COMMAND_CHARS} characters")));
        }
        let dir = self.runs_dir()?;
        let run_id = derive_id(&operation_id, "command-run");
        let paths = RunPaths::new(&dir, &run_id);
        // Check, authorize and spawn under one lock: two calls with one key
        // start one command.
        let _start = lock(&START_LOCK);
        prune_runs(&dir, SystemTime::now(), RUN_RETENTION);
        if let Some(meta) = paths.meta() {
            if meta.command != command {
                return Err(AgentToolError::invalid(
                    "idempotencyKey was already used for a different command; use a new key",
                ));
            }
            return self.command_state(&dir, &key, &run_id);
        }
        let chat_cwd = self.authorize_command(context, &command)?;
        let running = lock(live_runs()).values().filter(|run| run.chat_id == context.grant.chat_id).count();
        if running >= MAX_RUNS_PER_CHAT {
            return Err(AgentToolError::invalid(format!(
                "this chat already has {running} commands running or finishing (the limit is {MAX_RUNS_PER_CHAT}); wait for one to finish, then start this one again"
            )));
        }
        let chat_root = chat_cwd
            .as_deref()
            .and_then(|c| std::fs::canonicalize(c).ok())
            .ok_or_else(|| AgentToolError::invalid("the Chat has no working directory"))?;
        let cwd = match args.cwd {
            Some(cwd) => std::fs::canonicalize(cwd).map_err(|_| AgentToolError::invalid("cwd must be an existing directory"))?,
            None => chat_root.clone(),
        };
        if !cwd.is_dir() || !cwd.starts_with(&chat_root) {
            return Err(AgentToolError::invalid("cwd must be a directory inside the Chat's working directory"));
        }
        let io = |what: &'static str| move |e: std::io::Error| AgentToolError::invalid(format!("cannot create the {what}: {e}"));
        // Leftovers of a half-started run with this id are not a live run.
        for stale in [&paths.log, &paths.log_old, &paths.exit, &paths.meta, &paths.pid, &paths.interrupted, &paths.woken] {
            let _ = std::fs::remove_file(stale);
        }
        let log = create_new(&paths.log).map_err(io("log"))?;
        let meta = RunMeta {
            chat_id: context.grant.chat_id.clone(),
            key: key.clone(),
            command: command.clone(),
            engine_pid: std::process::id(),
        };
        // Write the record whole (temp file, then rename) so a concurrent
        // reader never sees half of it.
        let meta_tmp = paths.meta.with_extension("meta.tmp");
        let _ = std::fs::remove_file(&meta_tmp);
        create_new(&meta_tmp)
            .and_then(|mut file| file.write_all(&serde_json::to_vec(&meta).unwrap_or_default()))
            .and_then(|()| std::fs::rename(&meta_tmp, &paths.meta))
            .map_err(io("run record"))?;
        let mut child = mast_harness::command("sh");
        child
            .arg("-c")
            // The command's stdout and stderr go through a pipe to `pump_log`,
            // which caps the log. `$0` names the run, so a later engine can
            // tell this wrapper from a recycled pid.
            .arg(r#"sh -c "$1" 2>&1; code=$?; echo $code > "$2.tmp"; mv "$2.tmp" "$2"; exit $code"#)
            .arg(wrapper_name(&run_id))
            .arg(&command)
            .arg(&paths.exit)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // Its own process group, so an engine that finds the run orphaned can
        // stop the whole command tree.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            child.process_group(0);
        }
        let mut child = child.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&paths.meta);
            AgentToolError::invalid(format!("cannot start the command: {e}"))
        })?;
        let live = LiveRun::track(&run_id, &meta.chat_id, chrono::Utc::now().timestamp_millis());
        let pid_tmp = paths.pid.with_extension("pid.tmp");
        let _ = std::fs::remove_file(&pid_tmp);
        if let Err(error) = create_new(&pid_tmp)
            .and_then(|mut file| file.write_all(child.id().to_string().as_bytes()))
            .and_then(|()| std::fs::rename(&pid_tmp, &paths.pid))
        {
            tracing::warn!(%error, run = %run_id, "command-run pid file was not written; an engine restart cannot stop this command");
        }
        let (drained, output_done) = std::sync::mpsc::channel::<()>();
        if let Some(output) = child.stdout.take() {
            let pump_paths = paths.clone();
            std::thread::spawn(move || {
                pump_log(output, log, &pump_paths, LOG_SEGMENT_BYTES);
                let _ = drained.send(());
            });
        }
        let wake = self.waker(dir.clone(), meta.chat_id, run_id.clone());
        let thread_paths = paths.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            // The last output reaches the log before the wake reads it. A
            // background process that kept the pipe open does not hold it up.
            let _ = output_done.recv_timeout(PUMP_DRAIN_GRACE);
            // A signal leaves no code: report it as a shell does, 128+signal.
            #[cfg(unix)]
            let code = {
                use std::os::unix::process::ExitStatusExt;
                status.ok().and_then(|s| s.code().or_else(|| s.signal().map(|n| 128 + n)))
            };
            #[cfg(not(unix))]
            let code = status.ok().and_then(|s| s.code());
            let code = match thread_paths.exit_code() {
                Some(recorded) => recorded,
                None => {
                    // The wrapper died before recording it; record it so
                    // status and the wake agree.
                    let code = code.unwrap_or(-1);
                    let _ = std::fs::write(&thread_paths.exit, code.to_string());
                    code
                }
            };
            finish_run(live, || wake(Outcome::Exited(code)));
        });
        self.command_state(&dir, &key, &run_id)
    }

    pub(super) fn command_status(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let run_id = derive_id(&operation_id, "command-run");
        self.command_state(&self.runs_dir()?, &key_of(arguments), &run_id)
    }

    /// A delivery closure for one run. The message id derives from the run,
    /// so any number of deliveries queue at most one message. A chat archived
    /// meanwhile is not woken (the command itself runs to the end), except that
    /// a chat that handed its role over is woken through its successor. The
    /// `.woken` marker is written only after delivery or that deliberate skip,
    /// so a failed or runtime-less attempt is retried at the next start.
    fn waker(&self, dir: PathBuf, chat_id: String, run_id: String) -> Box<dyn Fn(Outcome) + Send + Sync> {
        let doc_host = self.doc_host.clone();
        let workspace = self.workspace.clone();
        let handle = tokio::runtime::Handle::try_current().ok();
        Box::new(move |outcome| {
            let paths = RunPaths::new(&dir, &run_id);
            if paths.woken.exists() {
                return;
            }
            let (Some(doc_host), Some(handle)) = (&doc_host, &handle) else { return };
            let Some(meta) = paths.meta() else { return };
            // A failed lookup is a failed delivery (retried), not "archived".
            let chat = match workspace.chat(&chat_id) {
                Ok(chat) => chat,
                Err(error) => {
                    tracing::warn!(%error, chat = %chat_id, "command-run wake could not read the chat; it is retried at the next start");
                    return;
                }
            };
            // A chat that handed its standing role over is normally archived
            // by the rotation, but its work goes on with the successor: the
            // wake follows the committed hand-off chain (as a user's message
            // does, under the same lock, so a hand-off that commits during
            // the send is followed too) instead of being dropped as
            // "archived". A first-start chat that is not adopted yet is still
            // its own destination.
            let provisional = chat.as_ref().is_some_and(|chat| chat.standing_role_provisional().is_some());
            let handed_over = !provisional && chat.as_ref().is_some_and(|chat| chat.standing_role_successor.is_some());
            // A user message revives an archived chat, which a wake must not
            // do: when the role's current holder has itself been archived, the
            // wake is skipped like one for any archived chat.
            let destination_retired = handed_over
                && doc_host
                    .committed_handoff_chain(&chat_id)
                    .ok()
                    .and_then(|chain| chain.last().cloned())
                    .and_then(|destination| workspace.chat(&destination).ok().flatten())
                    .is_some_and(|destination| destination.archived);
            if !destination_retired && chat.as_ref().is_some_and(|chat| !chat.archived || handed_over) {
                let mut text = wake_text(&meta.key, &outcome, &paths);
                if handed_over {
                    text = format!(
                        "Chat {chat_id}, which held your role before you, started this command and the role was handed to you, so you get its result. command_status only answers the chat that started the command, so read the log.\n{text}"
                    );
                }
                let message_id = derive_id(&run_id, "command-wake");
                let mut attempt = 0;
                let delivered = loop {
                    let result = if provisional {
                        handle.block_on(doc_host.queue_message_with_id(&chat_id, &message_id, &text, Vec::new(), true))
                    } else {
                        handle.block_on(doc_host.queue_user_message(&chat_id, Some(message_id.as_str()), &text, Vec::new(), true))
                    };
                    // A hand-off still in flight refuses the send for a moment.
                    if result.is_err() && attempt < SEND_RETRIES {
                        attempt += 1;
                        std::thread::sleep(SEND_RETRY_DELAY);
                        continue;
                    }
                    break result;
                };
                if let Err(error) = delivered {
                    tracing::warn!(%error, chat = %chat_id, "command-run wake was not delivered; it is retried at the next start");
                    return;
                }
            }
            let _ = std::fs::write(&paths.woken, b"");
        })
    }

    /// At engine start, for every run this process is not running and that
    /// has not been woken for: a recorded exit wakes with that result (the
    /// child outlived the engine); no exit means the engine died with the
    /// child, so mark the run `interrupted` and wake saying so. The marker
    /// comes first, so a straggler can never turn the run into a result. A
    /// delivery that fails or has no runtime is retried at the next start.
    pub fn recover_command_runs(&self) {
        let Ok(dir) = self.runs_dir() else { return };
        prune_runs(&dir, SystemTime::now(), RUN_RETENTION);
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(run_id) = path
                .extension()
                .is_some_and(|e| e == "meta")
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
            else {
                continue;
            };
            let paths = RunPaths::new(&dir, &run_id);
            if paths.woken.exists() || lock(live_runs()).contains_key(&run_id) {
                continue;
            }
            let Some(meta) = paths.meta() else { continue };
            let outcome = match paths.exit_code() {
                _ if paths.interrupted.exists() => Outcome::Interrupted,
                // The command's output pipe closed with its engine, so a
                // command that wrote afterwards died of SIGPIPE: that is the
                // restart, not a result the command chose.
                Some(code) if code == EXIT_SIGPIPE && meta.engine_pid != std::process::id() && !pid_alive(meta.engine_pid) => {
                    if std::fs::write(&paths.interrupted, b"").is_err() {
                        continue;
                    }
                    Outcome::Interrupted
                }
                Some(code) => Outcome::Exited(code),
                None => {
                    if meta.engine_pid == std::process::id() || pid_alive(meta.engine_pid) {
                        continue;
                    }
                    if std::fs::write(&paths.interrupted, b"").is_err() {
                        continue;
                    }
                    Outcome::Interrupted
                }
            };
            // An interrupted run's command may still be running (the engine
            // that started it is gone): stop it before waking, so the log the
            // wake points at is final.
            let orphan = matches!(outcome, Outcome::Interrupted).then(|| paths.clone());
            let wake = self.waker(dir.clone(), meta.chat_id, run_id.clone());
            std::thread::spawn(move || {
                if let Some(paths) = orphan {
                    terminate_orphan(&paths, &run_id);
                }
                wake(outcome);
            });
        }
    }

    fn command_state(&self, dir: &Path, key: &str, run_id: &str) -> Result<Value, AgentToolError> {
        let paths = RunPaths::new(dir, run_id);
        let Some(meta) = paths.meta() else {
            return Err(AgentToolError::invalid("no run was started with this idempotencyKey"));
        };
        let exit_code = paths.exit_code();
        let state = if paths.interrupted.exists() {
            "interrupted"
        } else if exit_code.is_some() {
            "exited"
        } else if lock(live_runs()).contains_key(run_id) {
            "running"
        } else {
            "interrupted"
        };
        let mut reply = json!({
            "key": key,
            "state": state,
            "command": meta.command,
            "logPath": paths.log.display().to_string(),
        });
        if state == "exited" && let Some(code) = exit_code {
            reply["exitCode"] = code.into();
            if code != 0 {
                reply["failureSummary"] = failure_summary(&read_tail(&paths)).into();
            }
        }
        if state == "interrupted" {
            reply["hint"] = "the engine restarted before the command finished, so there is no exit code; start it again with a new idempotencyKey".into();
        }
        Ok(reply)
    }
}

fn key_of(arguments: &Value) -> String {
    arguments
        .get("idempotencyKey")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_summary_leads_with_failure_lines_then_the_tail() {
        let mut log = String::new();
        for i in 0..40 {
            log.push_str(&format!("line {i}\n"));
        }
        log.push_str("test engine::broken ... FAILED\nthread 'x' panicked at src/a.rs:1\nlast line\n");
        let summary = failure_summary(&log);
        let lines: Vec<&str> = summary.lines().collect();
        assert!(lines[0].contains("FAILED"));
        assert!(lines[1].contains("panicked"));
        assert_eq!(lines[2], "...");
        assert_eq!(lines.last(), Some(&"last line"));
        assert!(lines.len() <= SUMMARY_MATCH_LINES + SUMMARY_TAIL_LINES + 1);
    }

    fn run_files(dir: &Path, run_id: &str, extensions: &[&str]) -> RunPaths {
        for extension in extensions {
            std::fs::write(dir.join(format!("{run_id}.{extension}")), "x").unwrap();
        }
        RunPaths::new(dir, run_id)
    }

    #[test]
    fn the_log_rotates_at_the_segment_size_and_keeps_the_newest_output() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run");
        let log = create_new(&paths.log).unwrap();
        let input: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
        // Fed in pieces of one line, so rotation happens between lines.
        struct Lines(std::vec::IntoIter<String>);
        impl Read for Lines {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some(line) = self.0.next() else { return Ok(0) };
                buf[..line.len()].copy_from_slice(line.as_bytes());
                Ok(line.len())
            }
        }
        let lines: Vec<String> = input.lines().map(|l| format!("{l}\n")).collect();
        pump_log(Lines(lines.into_iter()), log, &paths, 200);
        let current = std::fs::read_to_string(&paths.log).unwrap();
        let old = std::fs::read_to_string(&paths.log_old).unwrap();
        assert!(current.len() < 200 + 10 && old.len() < 200 + 10, "{} {}", current.len(), old.len());
        assert!(current.ends_with("line 099\n"));
        // The two segments are consecutive: the old one ends where the current begins.
        let last_old: usize = old.lines().last().unwrap()[5..].parse().unwrap();
        let first_current: usize = current.lines().next().unwrap()[5..].parse().unwrap();
        assert_eq!(first_current, last_old + 1);
        // A tail that the short current log cannot fill reaches back into the old one.
        let tail = read_tail(&paths);
        assert!(tail.starts_with("line 000") || tail.contains(old.lines().last().unwrap()));
        assert!(tail.ends_with("line 099\n"));
    }

    #[test]
    fn old_finished_runs_are_removed_and_live_or_unwoken_ones_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let woken = run_files(dir.path(), "woken", &["log", "log.old", "meta", "exit", "pid", "woken"]);
        let unwoken = run_files(dir.path(), "unwoken", &["log", "meta", "exit"]);
        let stray = run_files(dir.path(), "stray", &["log", "meta.tmp"]);
        let live = run_files(dir.path(), "live", &["log", "meta", "woken"]);
        let _running = track_running_command_for_test("chat", "live");
        let later = SystemTime::now() + RUN_RETENTION + Duration::from_secs(60);
        // Nothing is old enough yet.
        prune_runs(dir.path(), SystemTime::now(), RUN_RETENTION);
        assert!(woken.log.exists() && stray.log.exists());
        prune_runs(dir.path(), later, RUN_RETENTION);
        assert!(!woken.log.exists() && !woken.log_old.exists() && !woken.meta.exists() && !woken.pid.exists() && !woken.woken.exists());
        assert!(!stray.log.exists(), "files with no run record go too");
        assert!(unwoken.meta.exists() && unwoken.log.exists(), "a run still owed a wake is kept");
        assert!(live.meta.exists() && live.log.exists(), "a running run is kept");
    }

    #[test]
    fn a_running_command_mutes_for_at_most_an_hour() {
        let hour = 60 * 60_000;
        let _run = LiveRun::track("cap-run", "cap-chat", 1_000);
        assert!(chat_has_running_command_at("cap-chat", 1_000));
        assert!(chat_has_running_command_at("cap-chat", 1_000 + hour - 1));
        assert!(!chat_has_running_command_at("cap-chat", 1_000 + hour));
        assert!(!chat_has_running_command_at("another-chat", 1_000));
    }

    #[test]
    fn the_mute_holds_through_the_wake_and_drops_when_it_fails() {
        let delivering = LiveRun::track("wake-run", "wake-chat", chrono::Utc::now().timestamp_millis());
        finish_run(delivering, || assert!(chat_has_running_command("wake-chat"), "no gap before the wake is queued"));
        assert!(!chat_has_running_command("wake-chat"));

        // A wake write that panics still ends the mute.
        let failing = LiveRun::track("fail-run", "fail-chat", chrono::Utc::now().timestamp_millis());
        let outcome = std::thread::spawn(move || finish_run(failing, || panic!("the wake write failed"))).join();
        assert!(outcome.is_err());
        assert!(!chat_has_running_command("fail-chat"));
    }

    #[test]
    fn failure_summary_without_failures_is_just_the_tail() {
        let log: String = (0..30).map(|i| format!("ok {i}\n")).collect();
        let summary = failure_summary(&log);
        assert_eq!(summary.lines().count(), SUMMARY_TAIL_LINES);
        assert!(summary.ends_with("ok 29"));
    }
}
