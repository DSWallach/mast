//! Rails for autonomous child workers (Conflict C1 of the v0.3.0 plan).
//!
//! A worker is *autonomous* when it is a child chat (`parentChatId` set) and
//! its work item's task spec sets `requiredReport`: a Manager or Lead started
//! it to run without a person in the loop. Only those workers get the
//! stricter rails: default auto-continue, an urgent approval wake, the
//! approval deadline, stall and repeated-error wakes, out-of-scope edit flags
//! and per-kind long-tool limits. Every other chat, in particular an
//! interactive chat without a parent, keeps exactly the old behavior.
//!
//! Pure policy plus one small in-memory activity registry fed by the run loop
//! (`sessions.rs`); the wake service applies it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use mast_proto::{AgentEvent, ToolCall, UserInputQuestion, WorkItemTaskSpec};

/// Approval deadline when the task spec does not set `approvalTimeoutSecs`.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 600;
/// A working autonomous worker this quiet (no stream output, nothing open or
/// running) is reported as stalled.
pub const STALL_AFTER_MS: i64 = 5 * 60_000;
/// Identical consecutive tool errors that make a loop alert.
pub const ERROR_STRIKES: u32 = 3;

/// Whether a chat gets the autonomous-worker rails.
pub fn is_autonomous(parent_chat_id: Option<&str>, spec: Option<&WorkItemTaskSpec>) -> bool {
    parent_chat_id.is_some_and(|parent| !parent.is_empty()) && spec.is_some_and(|spec| spec.required_report)
}

/// How long an autonomous worker's approval may wait before it is declined.
/// `approvalTimeoutSecs: 0` turns the deadline off.
pub fn approval_timeout_ms(spec: Option<&WorkItemTaskSpec>) -> Option<i64> {
    let secs = spec
        .and_then(|spec| spec.approval_timeout_secs)
        .unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS);
    (secs > 0).then(|| i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX))
}

/// Programs whose prompts administer the host: only a person may answer
/// them, so they are never declined on a deadline.
const HOST_ADMIN_PROGRAMS: [&str; 22] = [
    "sudo", "doas", "pkexec", "su", "run0", "dnf", "dnf5", "yum", "rpm", "rpm-ostree", "apt", "apt-get",
    "dpkg", "zypper", "pacman", "snap", "passwd", "useradd", "usermod", "groupadd", "visudo", "firewall-cmd",
];

/// Whether an approval prompt is a host-administration prompt (`sudo`, a
/// system package manager, a system service change) that only the user may
/// answer. Such prompts are surfaced to the Manager but never auto-declined.
pub fn is_human_only_admin(question: &UserInputQuestion) -> bool {
    let text = match question.command_approval.as_ref().map(|details| &details.command) {
        Some(serde_json::Value::String(script)) => script.clone(),
        Some(serde_json::Value::Array(words)) => words
            .iter()
            .filter_map(|word| word.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => question.question.split('`').nth(1).unwrap_or(&question.question).to_owned(),
    };
    command_needs_host_admin(&text)
}

fn command_needs_host_admin(text: &str) -> bool {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '\'' | '"'))
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().enumerate().any(|(index, word)| {
        let program = word.rsplit('/').next().unwrap_or(word);
        if HOST_ADMIN_PROGRAMS.contains(&program) {
            return true;
        }
        // A system (not `--user`) service change.
        program == "systemctl"
            && !words[index + 1..].contains(&"--user")
            && words[index + 1..].iter().any(|w| {
                matches!(*w, "start" | "stop" | "restart" | "enable" | "disable" | "mask" | "unmask" | "daemon-reload")
            })
    })
}

/// The answer that declines an approval question, if it offers one.
pub fn decline_label(question: &UserInputQuestion) -> Option<String> {
    question
        .options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("no"))
        .or_else(|| {
            question.options.iter().find(|option| {
                let lower = option.to_ascii_lowercase();
                ["deny", "decline", "reject", "don't", "do not", "cancel"]
                    .iter()
                    .any(|word| lower.starts_with(word))
            })
        })
        .cloned()
}

/// Paths in `changed` that no pattern in `owned` covers. A pattern is a
/// repo-relative path or glob: `*` and `?` stay within one path segment,
/// `**` crosses segments, `{a,b}` lists alternatives, and a plain path also
/// covers everything below it. No owned paths means nothing is flagged: the
/// task did not declare a scope.
pub fn outside_owned_paths(changed: &[String], owned: &[String]) -> Vec<String> {
    if owned.is_empty() {
        return Vec::new();
    }
    let patterns: Vec<regex::Regex> = owned.iter().filter_map(|pattern| owned_path_regex(pattern)).collect();
    changed
        .iter()
        .filter(|path| !patterns.iter().any(|pattern| pattern.is_match(path)))
        .cloned()
        .collect()
}

fn owned_path_regex(pattern: &str) -> Option<regex::Regex> {
    let pattern = pattern.trim().trim_start_matches("./").trim_end_matches('/');
    if pattern.is_empty() {
        return None;
    }
    let mut out = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut in_braces = false;
    let mut globbed = false;
    while index < chars.len() {
        match chars[index] {
            '*' if chars.get(index + 1) == Some(&'*') => {
                globbed = true;
                index += 1;
                // `**/` also matches no directory at all.
                if chars.get(index + 1) == Some(&'/') {
                    index += 1;
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => {
                globbed = true;
                out.push_str("[^/]*");
            }
            '?' => {
                globbed = true;
                out.push_str("[^/]");
            }
            '{' if !in_braces => {
                in_braces = true;
                out.push_str("(?:");
            }
            '}' if in_braces => {
                in_braces = false;
                out.push(')');
            }
            ',' if in_braces => out.push('|'),
            ' ' if in_braces => {}
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        index += 1;
    }
    if in_braces {
        return None;
    }
    // A plain path (or a brace list of them) covers itself and what is below it.
    if !globbed || out.ends_with(')') {
        out.push_str("(?:/.*)?");
    }
    out.push('$');
    regex::Regex::new(&out).ok()
}

// ── post-turn scope check ──────────────────────────────────────────────────

fn git_text(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let _permit = crate::git_gate::acquire_blocking("scope_flags");
    let output = mast_harness::git_command()
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The commit a worktree is on, when `dir` is inside one.
pub fn worktree_head(dir: &std::path::Path) -> Option<String> {
    git_text(dir, &["rev-parse", "HEAD"]).map(|head| head.trim().to_owned()).filter(|head| !head.is_empty())
}

/// Repo-relative paths that differ from `base` in the worktree at `dir`:
/// commits since `base`, staged and unstaged edits, and untracked files.
/// Read-only; nothing is changed or reverted.
pub fn changed_since(dir: &std::path::Path, base: &str) -> Option<Vec<String>> {
    let top = git_text(dir, &["rev-parse", "--show-toplevel"])?;
    let top = std::path::Path::new(top.trim());
    let tracked = git_text(top, &["diff", "--name-only", "--no-renames", base, "--"])?;
    let untracked = git_text(top, &["ls-files", "--others", "--exclude-standard", "--full-name"]).unwrap_or_default();
    let mut paths: Vec<String> = tracked
        .lines()
        .chain(untracked.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    Some(paths)
}

// ── activity registry ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Activity {
    last_event_ms: i64,
    /// Tool call id → signature (what was called), so an error result can be
    /// matched to the call that produced it.
    calls: HashMap<String, String>,
    /// Signature + error text of the current run of identical errors.
    streak: Option<(String, u32)>,
}

const MAX_CALLS_REMEMBERED: usize = 256;

fn registry() -> &'static Mutex<HashMap<String, Activity>> {
    static ACTIVITY: OnceLock<Mutex<HashMap<String, Activity>>> = OnceLock::new();
    ACTIVITY.get_or_init(Default::default)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Three identical tool errors in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedError {
    pub strikes: u32,
    /// The error, one redacted line.
    pub excerpt: String,
}

fn signature(call: &ToolCall) -> String {
    match call {
        ToolCall::Exec { command } => format!("exec:{command}"),
        ToolCall::Mcp { server, tool, .. } => format!("mcp:{server}:{tool}"),
        ToolCall::Unknown { name, .. } => format!("tool:{name}"),
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

fn excerpt(text: &str) -> String {
    let flat = mast_harness::redact::redact_secrets(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() > 160 {
        format!("{}…", flat.chars().take(160).collect::<String>())
    } else {
        flat
    }
}

/// The run loop saw `event` from `chat_id` at `now_ms`. Records stream
/// activity and returns an alert when the same call failed with the same
/// error [`ERROR_STRIKES`] times in a row (once per run of errors).
pub fn note_event(chat_id: &str, event: &AgentEvent, now_ms: i64) -> Option<RepeatedError> {
    let mut registry = lock(registry());
    let activity = registry.entry(chat_id.to_owned()).or_default();
    activity.last_event_ms = now_ms;
    match event {
        AgentEvent::ToolCall { id, call } => {
            if activity.calls.len() >= MAX_CALLS_REMEMBERED {
                activity.calls.clear();
            }
            activity.calls.insert(id.clone(), signature(call));
            None
        }
        AgentEvent::ToolResult { id, is_error, output, exit_code, .. } => {
            let call = activity.calls.remove(id)?;
            if !is_error {
                // The same call succeeding ends its error run.
                if activity.streak.as_ref().is_some_and(|(key, _)| key.starts_with(&format!("{call}\u{0}"))) {
                    activity.streak = None;
                }
                return None;
            }
            let error = output
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .or_else(|| exit_code.map(|code| format!("exit code {code}")))
                .unwrap_or_else(|| "failed".to_owned());
            let key = format!("{call}\u{0}{error}");
            let strikes = match &activity.streak {
                Some((current, strikes)) if *current == key => strikes + 1,
                _ => 1,
            };
            activity.streak = Some((key, strikes));
            (strikes == ERROR_STRIKES).then(|| RepeatedError { strikes, excerpt: excerpt(&error) })
        }
        _ => None,
    }
}

/// When the chat last streamed anything, if it has since this process began.
pub fn last_event_ms(chat_id: &str) -> Option<i64> {
    lock(registry()).get(chat_id).map(|activity| activity.last_event_ms)
}

/// The chat was deleted or archived: forget it.
pub fn forget(chat_id: &str) {
    lock(registry()).remove(chat_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(required_report: bool, timeout: Option<u64>) -> WorkItemTaskSpec {
        WorkItemTaskSpec { required_report, approval_timeout_secs: timeout, ..Default::default() }
    }

    fn approval(command: serde_json::Value) -> UserInputQuestion {
        UserInputQuestion {
            id: "q".into(),
            header: "Approve command".into(),
            question: "Run it?".into(),
            options: vec!["Yes".into(), "No".into()],
            multi_select: false,
            allow_custom: false,
            approval_scope: None,
            command_approval: Some(mast_proto::CommandApproval {
                command,
                cwd: None,
                reason: None,
                sandbox_permissions: None,
            }),
            permission: None,
        }
    }

    #[test]
    fn only_child_chats_with_a_required_report_are_autonomous() {
        assert!(is_autonomous(Some("manager"), Some(&spec(true, None))));
        assert!(!is_autonomous(None, Some(&spec(true, None))), "an interactive parentless chat");
        assert!(!is_autonomous(Some(""), Some(&spec(true, None))));
        assert!(!is_autonomous(Some("manager"), Some(&spec(false, None))));
        assert!(!is_autonomous(Some("manager"), None));
    }

    #[test]
    fn the_approval_deadline_defaults_to_ten_minutes_and_zero_turns_it_off() {
        assert_eq!(approval_timeout_ms(Some(&spec(true, None))), Some(600_000));
        assert_eq!(approval_timeout_ms(None), Some(600_000));
        assert_eq!(approval_timeout_ms(Some(&spec(true, Some(90)))), Some(90_000));
        assert_eq!(approval_timeout_ms(Some(&spec(true, Some(0)))), None);
    }

    #[test]
    fn host_admin_prompts_are_human_only() {
        let human = |command: serde_json::Value| is_human_only_admin(&approval(command));
        assert!(human("sudo dnf install -y ripgrep".into()));
        assert!(human(serde_json::json!(["/usr/bin/sudo", "true"])));
        assert!(human("cd /tmp && pkexec rm x".into()));
        assert!(human("rpm-ostree upgrade".into()));
        assert!(human("bash -lc 'sudo systemctl restart sshd'".into()));
        assert!(human("systemctl restart sshd".into()));
        assert!(!human("systemctl --user restart mast".into()));
        assert!(!human("systemctl status sshd".into()));
        assert!(!human("cargo nextest run -p mast-engine".into()));
        assert!(!human("git push origin HEAD".into()));
        assert!(!human("grep -r sudoers docs".into()));
        let mut text_only = approval(serde_json::Value::Null);
        text_only.command_approval = None;
        text_only.question = "Allow `sudo dnf upgrade`?".into();
        assert!(is_human_only_admin(&text_only));
        text_only.question = "Allow `cargo test`?".into();
        assert!(!is_human_only_admin(&text_only));
    }

    #[test]
    fn decline_prefers_no_then_deny_words() {
        let mut question = approval(serde_json::Value::Null);
        assert_eq!(decline_label(&question).as_deref(), Some("No"));
        question.options = vec!["Allow once".into(), "Deny".into()];
        assert_eq!(decline_label(&question).as_deref(), Some("Deny"));
        question.options = vec!["Allow".into()];
        assert_eq!(decline_label(&question), None);
    }

    #[test]
    fn owned_paths_cover_globs_braces_and_directories() {
        let owned: Vec<String> = [
            "crates/harness/src/acp/agent_specs.rs",
            "crates/engine/src/{orchestrator_wakes/**, sessions.rs, auto_continue.rs}",
            "docs/guide",
            "crates/ui/src/*.rs",
        ]
        .map(str::to_owned)
        .to_vec();
        let changed: Vec<String> = [
            "crates/harness/src/acp/agent_specs.rs",
            "crates/engine/src/orchestrator_wakes/core.rs",
            "crates/engine/src/orchestrator_wakes/a/b.rs",
            "crates/engine/src/sessions.rs",
            "docs/guide/orchestrating.md",
            "crates/ui/src/lib.rs",
            "crates/ui/src/shell/mod.rs",
            "crates/engine/src/rpc.rs",
            "crates/harness/src/acp/agent_specs.rs.orig",
            "docs/guidebook.md",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            outside_owned_paths(&changed, &owned),
            [
                "crates/ui/src/shell/mod.rs",
                "crates/engine/src/rpc.rs",
                "crates/harness/src/acp/agent_specs.rs.orig",
                "docs/guidebook.md",
            ]
        );
        assert!(outside_owned_paths(&changed, &[]).is_empty(), "no declared scope flags nothing");
        assert_eq!(
            outside_owned_paths(&["src/a.rs".into(), "x/src/b/c.rs".into()], &["**/src/**".into()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn changed_since_lists_commits_edits_and_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git").args(args).current_dir(root)
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output().unwrap().status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("owned")).unwrap();
        std::fs::write(root.join("owned/a.rs"), "a").unwrap();
        std::fs::write(root.join("shared.rs"), "s").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = worktree_head(root).unwrap();
        assert_eq!(changed_since(root, &base).unwrap(), Vec::<String>::new());
        std::fs::write(root.join("owned/a.rs"), "a2").unwrap();
        git(&["commit", "-qam", "owned edit"]);
        std::fs::write(root.join("shared.rs"), "s2").unwrap();
        std::fs::write(root.join("owned/new.rs"), "n").unwrap();
        std::fs::write(root.join("stray.txt"), "x").unwrap();
        let changed = changed_since(&root.join("owned"), &base).unwrap();
        assert_eq!(changed, ["owned/a.rs", "owned/new.rs", "shared.rs", "stray.txt"]);
        assert_eq!(outside_owned_paths(&changed, &["owned/**".into()]), ["shared.rs", "stray.txt"]);
        // Flagging never touches the worktree.
        assert_eq!(std::fs::read_to_string(root.join("shared.rs")).unwrap(), "s2");
    }

    #[test]
    fn three_identical_errors_alert_once_and_a_success_resets() {
        let chat = "autonomy-strikes";
        forget(chat);
        let call = |id: &str| AgentEvent::ToolCall {
            id: id.into(),
            call: ToolCall::Exec { command: "cargo check".into() },
        };
        let failed = |id: &str, text: &str| AgentEvent::ToolResult {
            id: id.into(),
            is_error: true,
            exit_code: Some(101),
            output: Some(text.into()),
            diff: None,
        };
        let mut alerts = Vec::new();
        for (index, text) in ["E0308 mismatched types", "E0308 mismatched types", "E0425 unresolved", "E0425 unresolved", "E0425 unresolved", "E0425 unresolved"]
            .iter()
            .enumerate()
        {
            let id = format!("c{index}");
            assert_eq!(note_event(chat, &call(&id), 10), None);
            alerts.extend(note_event(chat, &failed(&id, text), 20));
        }
        assert_eq!(alerts, [RepeatedError { strikes: 3, excerpt: "E0425 unresolved".into() }]);
        // Success of the same call ends the run; the next errors count anew.
        note_event(chat, &call("ok"), 30);
        note_event(chat, &AgentEvent::ToolResult { id: "ok".into(), is_error: false, exit_code: Some(0), output: None, diff: None }, 30);
        for index in 0..2 {
            let id = format!("again{index}");
            note_event(chat, &call(&id), 40);
            assert_eq!(note_event(chat, &failed(&id, "E0425 unresolved"), 40), None);
        }
        assert_eq!(last_event_ms(chat), Some(40));
        forget(chat);
        assert_eq!(last_event_ms(chat), None);
    }
}
