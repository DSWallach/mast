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

/// Run ids this process started and has not seen exit, with the chat that
/// started each. Process-wide, because the engine builds more than one
/// service instance.
fn live_runs() -> &'static Mutex<HashMap<String, String>> {
    static LIVE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Whether `chat_id` has a command running that it is waiting on: its single
/// exit wake is coming, so the chat is waiting rather than stalled.
pub fn chat_has_running_command(chat_id: &str) -> bool {
    lock(live_runs()).values().any(|chat| chat == chat_id)
}

/// Marks a command as running for `chat_id` until the guard drops, so tests
/// can exercise the waiting signal without spawning a process.
#[doc(hidden)]
pub fn track_running_command_for_test(chat_id: &str, run_id: &str) -> impl Drop {
    struct Guard(String);
    impl Drop for Guard {
        fn drop(&mut self) {
            lock(live_runs()).remove(&self.0);
        }
    }
    lock(live_runs()).insert(run_id.to_owned(), chat_id.to_owned());
    Guard(run_id.to_owned())
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
    exit: PathBuf,
    meta: PathBuf,
    interrupted: PathBuf,
    woken: PathBuf,
}

impl RunPaths {
    fn new(dir: &Path, run_id: &str) -> Self {
        let file = |ext: &str| dir.join(format!("{run_id}.{ext}"));
        Self {
            log: file("log"),
            exit: file("exit"),
            meta: file("meta"),
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

fn read_tail(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL_BYTES))).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

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
            let tail = read_tail(&paths.log);
            let lines: Vec<&str> = tail.lines().collect();
            let start = lines.len().saturating_sub(3);
            format!("Command run `{key}` succeeded. Log: {log}\n{}", lines[start..].join("\n"))
        }
        Outcome::Exited(code) => format!(
            "Command run `{key}` failed with exit code {code}. Log: {log}\n{}",
            failure_summary(&read_tail(&paths.log))
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
        if self.workspace.chat_is_manager(&context.grant.chat_id) {
            return Err(forbidden("a Manager coordinates and cannot start commands"));
        }
        let Some(config) = chat.config.as_ref().filter(|c| c.sandbox != SandboxLevel::ReadOnly) else {
            return Err(forbidden("a read-only chat cannot start commands"));
        };
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
        if let Some(meta) = paths.meta() {
            if meta.command != command {
                return Err(AgentToolError::invalid(
                    "idempotencyKey was already used for a different command; use a new key",
                ));
            }
            return self.command_state(&dir, &key, &run_id);
        }
        let chat_cwd = self.authorize_command(context, &command)?;
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
        for stale in [&paths.log, &paths.exit, &paths.meta, &paths.interrupted, &paths.woken] {
            let _ = std::fs::remove_file(stale);
        }
        let log = create_new(&paths.log).map_err(io("log"))?;
        let err_log = log.try_clone().map_err(|e| AgentToolError::invalid(e.to_string()))?;
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
            .arg(r#"sh -c "$1"; code=$?; echo $code > "$2.tmp"; mv "$2.tmp" "$2"; exit $code"#)
            .arg("mast-run-command")
            .arg(&command)
            .arg(&paths.exit)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err_log);
        let mut child = child.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&paths.meta);
            AgentToolError::invalid(format!("cannot start the command: {e}"))
        })?;
        lock(live_runs()).insert(run_id.clone(), meta.chat_id.clone());
        let wake = self.waker(dir.clone(), meta.chat_id, run_id.clone());
        let thread_paths = paths.clone();
        let thread_run_id = run_id.clone();
        std::thread::spawn(move || {
            let run_id = thread_run_id;
            let status = child.wait();
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
            lock(live_runs()).remove(&run_id);
            wake(Outcome::Exited(code));
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
    /// meanwhile is not woken (the command itself runs to the end). The
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
            let archived = match workspace.chat(&chat_id) {
                Ok(chat) => chat.is_none_or(|chat| chat.archived),
                Err(error) => {
                    tracing::warn!(%error, chat = %chat_id, "command-run wake could not read the chat; it is retried at the next start");
                    return;
                }
            };
            if !archived {
                let text = wake_text(&meta.key, &outcome, &paths);
                let message_id = derive_id(&run_id, "command-wake");
                let delivered = handle.block_on(doc_host.queue_message_with_id(&chat_id, &message_id, &text, Vec::new(), true));
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
            let wake = self.waker(dir.clone(), meta.chat_id, run_id);
            std::thread::spawn(move || wake(outcome));
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
                reply["failureSummary"] = failure_summary(&read_tail(&paths.log)).into();
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

    #[test]
    fn failure_summary_without_failures_is_just_the_tail() {
        let log: String = (0..30).map(|i| format!("ok {i}\n")).collect();
        let summary = failure_summary(&log);
        assert_eq!(summary.lines().count(), SUMMARY_TAIL_LINES);
        assert!(summary.ends_with("ok 29"));
    }
}
