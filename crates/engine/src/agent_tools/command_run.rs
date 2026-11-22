//! `run_command` / `command_status`: a long command hosted by the engine so a
//! worker can end its turn instead of polling. The command writes to a log
//! file and its shell records the exit code in a sidecar file, so the result
//! survives an engine restart. The calling Chat is woken once on exit by a
//! queued message whose id derives from the run, so a retry, a status call
//! after a restart, or a second exit observer never wakes it twice.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

#[derive(Default)]
pub(super) struct CommandRuns {
    /// Run ids this engine process started and has not seen exit.
    runs: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
}

fn runs_dir() -> PathBuf {
    std::env::temp_dir().join("mast-command-runs")
}

struct RunPaths {
    log: PathBuf,
    exit: PathBuf,
    meta: PathBuf,
    interrupted: PathBuf,
}

impl RunPaths {
    fn new(run_id: &str) -> Self {
        let dir = runs_dir();
        Self {
            log: dir.join(format!("{run_id}.log")),
            exit: dir.join(format!("{run_id}.exit")),
            meta: dir.join(format!("{run_id}.meta")),
            interrupted: dir.join(format!("{run_id}.interrupted")),
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

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunMeta {
    chat_id: String,
    key: String,
    command: String,
    /// The engine process that started the run; only a dead one's runs are
    /// recovered as interrupted, so two engines sharing a temp dir (or tests
    /// sharing a process) never interrupt each other's live runs.
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
    /// Check the caller may start this command. The command runs outside the
    /// agent's sandbox, so it is allowed only where the harness would run it
    /// without asking: never from a read-only chat, never from an Ask-mode
    /// chat, and in Rules mode only when an allow rule covers it. A deny rule
    /// always wins.
    fn authorize_command(&self, context: &CallContext, command: &str) -> Result<(), AgentToolError> {
        let chat = self
            .workspace
            .chat(&context.grant.chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| AgentToolError::invalid("the Chat no longer exists"))?;
        let forbidden = |message: &str| AgentToolError::refused(AgentToolErrorCode::Forbidden, message);
        let config = chat.config.as_ref();
        if config.is_none_or(|c| c.sandbox == SandboxLevel::ReadOnly) {
            return Err(forbidden("a read-only chat cannot start commands"));
        }
        let config = config.expect("checked above");
        let rules = lock(&self.sessions).as_ref().map(|s| s.permission_rules());
        let request = PermissionRequest::Command { command: Value::String(command.to_owned()) };
        let decision = rules.map(|r| r.decide(&request));
        if let Some(RuleDecision::Deny(rule)) = &decision {
            return Err(forbidden(&format!("denied by permission rule {rule}")));
        }
        if config.auto_approve {
            return Ok(());
        }
        if config.approval_rules && matches!(decision, Some(RuleDecision::Allow(_))) {
            return Ok(());
        }
        Err(forbidden(
            "this chat would ask before running the command; run it through the harness shell, or use an auto-approve or Rules chat with an allow rule",
        ))
    }

    pub(super) fn run_command(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: RunArgs = parse_args(arguments)?;
        let key = key_of(arguments);
        let command = args.command.trim().to_owned();
        if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
            return Err(AgentToolError::invalid(format!("command must be 1 to {MAX_COMMAND_CHARS} characters")));
        }
        let run_id = derive_id(&operation_id, "command-run");
        let paths = RunPaths::new(&run_id);
        if let Some(meta) = paths.meta() {
            if meta.command != command {
                return Err(AgentToolError::invalid(
                    "idempotencyKey was already used for a different command; use a new key",
                ));
            }
            return self.command_state(&key, &run_id);
        }
        self.authorize_command(context, &command)?;
        let cwd = match args.cwd {
            Some(cwd) => PathBuf::from(cwd),
            None => self
                .workspace
                .chat(&context.grant.chat_id)
                .ok()
                .flatten()
                .and_then(|c| c.cwd)
                .map(PathBuf::from)
                .ok_or_else(|| AgentToolError::invalid("the Chat has no working directory; pass cwd"))?,
        };
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(AgentToolError::invalid("cwd must be an existing absolute directory"));
        }
        std::fs::create_dir_all(runs_dir()).map_err(|e| AgentToolError::invalid(format!("cannot create the log directory: {e}")))?;
        let log = std::fs::File::create(&paths.log).map_err(|e| AgentToolError::invalid(format!("cannot create the log: {e}")))?;
        let err_log = log.try_clone().map_err(|e| AgentToolError::invalid(e.to_string()))?;
        let meta = RunMeta { chat_id: context.grant.chat_id.clone(), key: key.clone(), command: command.clone(), engine_pid: std::process::id() };
        std::fs::write(&paths.meta, serde_json::to_vec(&meta).unwrap_or_default())
            .map_err(|e| AgentToolError::invalid(e.to_string()))?;
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
        lock(&self.command_runs.runs).insert(run_id.clone());
        let wake = self.waker(meta.chat_id, run_id.clone());
        let runs = self.command_runs.runs.clone();
        let thread_run_id = run_id.clone();
        std::thread::spawn(move || {
            let run_id = thread_run_id;
            let status = child.wait();
            lock(&runs).remove(&run_id);
            // A signal has no code; report it as 128+signal like a shell.
            let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            wake(Outcome::Exited(paths_exit_code(&run_id).unwrap_or(code)));
        });
        self.command_state(&key, &run_id)
    }

    pub(super) fn command_status(&self, context: &CallContext, arguments: &Value) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let run_id = derive_id(&operation_id, "command-run");
        self.command_state(&key_of(arguments), &run_id)
    }

    /// A delivery closure for one run. The message id derives from the run,
    /// so any number of deliveries queue at most one message, and a chat that
    /// was archived meanwhile is not woken (the command itself keeps running).
    fn waker(&self, chat_id: String, run_id: String) -> Box<dyn Fn(Outcome) + Send + Sync> {
        let doc_host = self.doc_host.clone();
        let workspace = self.workspace.clone();
        let handle = tokio::runtime::Handle::try_current().ok();
        Box::new(move |outcome| {
            let (Some(doc_host), Some(handle)) = (&doc_host, &handle) else { return };
            if workspace.chat(&chat_id).ok().flatten().is_none_or(|chat| chat.archived) {
                return;
            }
            let paths = RunPaths::new(&run_id);
            let Some(meta) = paths.meta() else { return };
            let text = wake_text(&meta.key, &outcome, &paths);
            let message_id = derive_id(&run_id, "command-wake");
            let delivered = handle.block_on(doc_host.queue_message_with_id(&chat_id, &message_id, &text, Vec::new(), true));
            if let Err(error) = delivered {
                tracing::warn!(%error, chat = %chat_id, "command-run wake was not delivered");
            }
        })
    }

    /// At engine start: a run with no recorded exit belonged to a previous
    /// engine process, which died (or orphaned) its child. Mark it
    /// `interrupted` and wake its chat once; the marker comes first, so a
    /// child that outlives the engine can never turn the run into a result.
    pub fn recover_command_runs(&self) {
        let Ok(entries) = std::fs::read_dir(runs_dir()) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(run_id) = path.extension().is_some_and(|e| e == "meta").then(|| path.file_stem()?.to_str().map(str::to_owned)).flatten() else {
                continue;
            };
            let paths = RunPaths::new(&run_id);
            if paths.exit_code().is_some() || paths.interrupted.exists() || lock(&self.command_runs.runs).contains(&run_id) {
                continue;
            }
            let Some(meta) = paths.meta() else { continue };
            if meta.engine_pid == std::process::id() || pid_alive(meta.engine_pid) {
                continue;
            }
            if std::fs::write(&paths.interrupted, b"").is_err() {
                continue;
            }
            let wake = self.waker(meta.chat_id, run_id);
            std::thread::spawn(move || wake(Outcome::Interrupted));
        }
    }

    fn command_state(&self, key: &str, run_id: &str) -> Result<Value, AgentToolError> {
        let paths = RunPaths::new(run_id);
        let Some(meta) = paths.meta() else {
            return Err(AgentToolError::invalid("no run was started with this idempotencyKey"));
        };
        let exit_code = paths.exit_code();
        let state = if paths.interrupted.exists() {
            "interrupted"
        } else if exit_code.is_some() {
            "exited"
        } else if lock(&self.command_runs.runs).contains(run_id) {
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

fn paths_exit_code(run_id: &str) -> Option<i32> {
    RunPaths::new(run_id).exit_code()
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
