//! Conservative parsing for command approvals. This module does not approve commands.
//!
//! Shell constructs that cannot be represented exactly are rejected. In particular,
//! expansion other than scoped $PWD assignments, redirection other than a log file
//! in /tmp, background jobs and nested shells are never interpreted.

use mast_proto::work_outline::{ApprovalRule, ApprovalRuleHarness};
use mast_proto::{FrictionKind, FrictionReport, FrictionSource};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommandSegment {
    pub argv: Vec<String>,
    pub pipeline_input: bool,
}

/// Strip only wrappers whose entire behavior is known. Keep assignments for
/// safety checks and for rules that require isolated configuration directories.
pub(crate) fn normalize_segment(segment: &CommandSegment) -> Option<(Vec<String>, Vec<String>)> {
    let mut argv = segment.argv.as_slice();
    let mut assignments = Vec::new();
    while let Some(word) = argv.first() {
        if !is_assignment(word) {
            break;
        }
        assignments.push(word.clone());
        argv = &argv[1..];
    }
    if argv.starts_with(&["caffeinate".into(), "-i".into()]) {
        argv = &argv[2..];
    }
    if argv.len() >= 4 && argv[0] == "perl" && argv[1] == "-e" && is_alarm_wrapper(&argv[2]) {
        argv = &argv[3..];
    }
    if argv.is_empty() || is_shell(argv) {
        return None;
    }
    Some((assignments, argv.to_vec()))
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_alarm_wrapper(script: &str) -> bool {
    let Some(seconds) = script
        .strip_prefix("alarm ")
        .and_then(|s| s.strip_suffix("; exec @ARGV"))
    else {
        return false;
    };
    !seconds.is_empty() && seconds.bytes().all(|c| c.is_ascii_digit())
}

/// Return one matching rule per executable segment. Unknown syntax fails closed.
/// The caller must apply the safety rails before acting on this result.
pub(crate) fn matching_rules<'a>(
    command: &Value,
    harness: ApprovalRuleHarness,
    cwd: &Path,
    permitted_roots: &[&Path],
    rules: &'a [ApprovalRule],
) -> Option<Vec<&'a ApprovalRule>> {
    let cwd = cwd.canonicalize().ok()?;
    if !permitted_roots
        .iter()
        .any(|root| root.canonicalize().is_ok_and(|root| cwd.starts_with(root)))
    {
        return None;
    }
    let (segments, logs) = parse_approval_command(command)?;
    if !passes_safety_rails_with_rules(&segments, rules) || !logs.iter().all(|path| log_target_is_safe(path)) {
        return None;
    }
    let mut matches = Vec::new();
    for segment in &segments {
        let (assignments, argv) = normalize_segment(segment)?;
        if argv.iter().any(|word| word.contains('$')) {
            return None;
        }
        if known_wrapper(segment, &argv) {
            if !passes_safety_rails(std::slice::from_ref(segment)) {
                return None;
            }
            continue;
        }
        let rule = rules.iter().find(|rule| {
            if !rule.enabled
                || (rule.harness != ApprovalRuleHarness::Any && rule.harness != harness)
            {
                return false;
            }
            if !passes_safety_rails_with_rules(std::slice::from_ref(segment), std::slice::from_ref(*rule)) {
                return false;
            }
            if let Some(policy) = &rule.constraints {
                if let Some(script) = &policy.repository_executable {
                    if !repository_executable_matches(script, rule, &argv, &cwd, permitted_roots) {
                        return false;
                    }
                }
                if let Some(expected) = &policy.environment {
                    let mut expected = expected.clone();
                    let mut actual = assignments.clone();
                    expected.sort();
                    actual.sort();
                    if expected != actual {
                        return false;
                    }
                }
            }
            if rule.argv_prefix.is_empty() && rule.command_regex.is_none() {
                return false;
            }
            if !argv.starts_with(&rule.argv_prefix) {
                return false;
            }
            if assignments
                .iter()
                .any(|assignment| assignment.contains('$'))
            {
                let Some(scope) = rule
                    .cwd_scope
                    .as_deref()
                    .and_then(|s| Path::new(s).canonicalize().ok())
                else {
                    return false;
                };
                if !cwd.starts_with(&scope)
                    || !assignments
                        .iter()
                        .all(|assignment| scoped_pwd_assignment(assignment, &cwd, &scope))
                {
                    return false;
                }
            }
            if rule.require_isolated_home && !isolated_configuration(&assignments) {
                return false;
            }
            if let Some(pattern) = &rule.command_regex {
                let is_script = rule.constraints.as_ref().is_some_and(|policy| policy.repository_executable.is_some());
                let effective_pattern = if is_script { format!("^(?:{pattern})$") } else { pattern.clone() };
                let Ok(regex) = regex::Regex::new(&effective_pattern) else {
                    return false;
                };
                let command = shell_words::join(&argv);
                let matched = regex.find(&command);
                if !matched.is_some_and(|m| !is_script || (m.start() == 0 && m.end() == command.len())) {
                    return false;
                }
            }
            if let Some(scope) = &rule.cwd_scope {
                let Ok(scope) = Path::new(scope).canonicalize() else {
                    return false;
                };
                if !permitted_roots.iter().any(|root| {
                    root.canonicalize()
                        .is_ok_and(|root| scope.starts_with(root))
                }) || !cwd.starts_with(scope)
                {
                    return false;
                }
            }
            true
        })?;
        matches.push(rule);
    }
    (!matches.is_empty()).then_some(matches)
}

fn repository_executable_matches(
    script: &mast_proto::work_outline::RepositoryApprovalExecutable,
    rule: &ApprovalRule,
    argv: &[String],
    cwd: &Path,
    permitted_roots: &[&Path],
) -> bool {
    if argv.first().map(String::as_str) != Some(script.path.as_str())
        || !mast_proto::work_outline::repository_executable_path(&script.path)
    {
        return false;
    }
    let Some(scope) = rule.cwd_scope.as_deref().and_then(|scope| Path::new(scope).canonicalize().ok()) else {
        return false;
    };
    if cwd != scope.as_path() || !permitted_roots.iter().any(|root| root.canonicalize().is_ok_and(|root| {
        root == scope || (scope.starts_with(&root) && checkout_top_level(&scope))
    })) {
        return false;
    }
    let Ok(path) = cwd.join(&script.path).canonicalize() else { return false; };
    if !path.starts_with(&scope) || !path.is_file() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0) {
            return false;
        }
    }
    let Some(expected) = script.sha256.as_deref() else { return false; };
    read_executable_digest(&path).is_some_and(|(digest, _)| digest == expected)
}

/// Fingerprinted executables above this size are never approved by content.
const MAX_FINGERPRINTED_BYTES: u64 = 1024 * 1024;

fn read_executable_digest(path: &Path) -> Option<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FINGERPRINTED_BYTES { return None; }
    let mut bytes = Vec::new();
    file.take(MAX_FINGERPRINTED_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FINGERPRINTED_BYTES { return None; }
    Some((format!("{:x}", Sha256::digest(&bytes)), bytes))
}

fn checkout_top_level(scope: &Path) -> bool {
    let Ok(output) = mast_harness::git_command().arg("-C").arg(scope).args(["rev-parse", "--show-toplevel"]).output() else { return false; };
    output.status.success() && std::str::from_utf8(&output.stdout).ok()
        .and_then(|path| Path::new(path.trim()).canonicalize().ok()).is_some_and(|path| path == scope)
}

/// Fingerprint a repository-relative executable when a user saves its rule.
pub(crate) fn fingerprint_repository_executable(scope: &str, relative: &str) -> Option<String> {
    describe_repository_executable(scope, relative).map(|description| description.sha256)
}

pub(crate) struct RepositoryExecutableDescription {
    pub sha256: String,
    pub size: usize,
    pub head: String,
}

/// The digest, size and first lines of a checkout executable, for the
/// proposal a user reviews.
pub(crate) fn describe_repository_executable(scope: &str, relative: &str) -> Option<RepositoryExecutableDescription> {
    if !mast_proto::work_outline::repository_executable_path(relative) { return None; }
    let scope = Path::new(scope).canonicalize().ok()?;
    let path = scope.join(relative).canonicalize().ok()?;
    if !path.starts_with(&scope) || !path.is_file() { return None; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path).ok()?.permissions().mode() & 0o111 == 0 { return None; }
    }
    let (sha256, bytes) = read_executable_digest(&path)?;
    let head = String::from_utf8_lossy(&bytes).lines().take(5).map(|line| line.chars().take(120).collect::<String>()).collect::<Vec<_>>().join("\n");
    Some(RepositoryExecutableDescription { sha256, size: bytes.len(), head })
}

// Resolve the nearest existing ancestor as well as checking the lexical path:
// a symlink in a target directory must not escape the approved scope.
fn scoped_pwd_assignment(assignment: &str, cwd: &Path, scope: &Path) -> bool {
    let Some((_, value)) = assignment.split_once('=') else {
        return false;
    };
    if !value.contains('$') {
        return true;
    }
    let Some(suffix) = value.strip_prefix("$PWD") else {
        return false;
    };
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return false;
    }
    let relative = suffix.strip_prefix('/').unwrap_or("");
    if relative
        .split('/')
        .any(|part| part == ".." || part == "." || (part.is_empty() && !relative.is_empty()))
    {
        return false;
    }
    let mut path = cwd.join(relative);
    while !path.exists() {
        if !path.pop() {
            return false;
        }
    }
    path.canonicalize()
        .is_ok_and(|path| path.starts_with(scope))
}

fn known_wrapper(segment: &CommandSegment, argv: &[String]) -> bool {
    (!segment.pipeline_input
        && argv.len() == 3
        && argv[0] == "ulimit"
        && argv[1] == "-n"
        && !argv[2].is_empty()
        && argv[2].bytes().all(|c| c.is_ascii_digit()))
        || (segment.pipeline_input
            && matches!(argv[0].as_str(), "rg" | "head" | "/usr/bin/head" | "cut"))
}

/// Preview the same fail-closed decision used by the engine.
pub fn matching_rule_labels(
    command: &str,
    harness: ApprovalRuleHarness,
    cwd: &Path,
    permitted_roots: &[&Path],
    rules: &[ApprovalRule],
) -> Option<Vec<String>> {
    matching_rules(
        &Value::String(command.to_owned()),
        harness,
        cwd,
        permitted_roots,
        rules,
    )
    .map(|matches| matches.into_iter().map(|rule| rule.label.clone()).collect())
}

/// Prefill a disabled argument-prefix rule when exactly one segment needs a rule.
pub fn prefill_rule_from_command(command: &Value, id: String) -> Option<ApprovalRule> {
    try_prefill_rule(command, id).ok()
}

/// [`prefill_rule_from_command`], saying why a command cannot become a rule.
pub fn try_prefill_rule(command: &Value, id: String) -> Result<ApprovalRule, &'static str> {
    const INEXACT: &str = "It uses shell expansion, a redirect, a background job or a nested shell, which a rule cannot match exactly.";
    let Some((segments, _)) = parse_approval_command(command) else {
        return Err(INEXACT);
    };
    if !passes_safety_rails(&segments) {
        return Err("It is outside the command families rules support, or a safety rule blocks it.");
    }
    let mut command = None;
    let mut require_isolated_home = false;
    for segment in &segments {
        let Some((assignments, argv)) = normalize_segment(segment) else {
            return Err(INEXACT);
        };
        if known_wrapper(segment, &argv) {
            continue;
        }
        if assignments.iter().any(|assignment| {
            matches!(
                assignment.split_once('=').map(|(name, _)| name),
                Some("HOME" | "CLAUDE_CONFIG_DIR" | "CODEX_HOME")
            )
        }) {
            if !isolated_configuration(&assignments) {
                return Err("It sets HOME, CLAUDE_CONFIG_DIR or CODEX_HOME outside /tmp.");
            }
            require_isolated_home = true;
        }
        if command.replace(argv).is_some() {
            return Err("It has more than one command. Add one rule per command in Space settings.");
        }
    }
    let Some(argv_prefix) = command else {
        return Err("It has no command to match.");
    };
    Ok(ApprovalRule {
        id,
        label: "Command approval".into(),
        created_by: "user".into(),
        harness: ApprovalRuleHarness::Codex,
        enabled: false,
        argv_prefix,
        command_regex: None,
        cwd_scope: None,
        require_isolated_home,
        constraints: None,
        hit_count: 0,
        last_used_at: None,
    })
}

/// The stored command is cut at this many characters.
const RECORDED_COMMAND_LIMIT: usize = 1024;

/// Draft a rule from a command the user approved in the past, read from its
/// Permission friction row. The draft is only a proposal: it is marked enabled
/// so that the user's save turns it on, and nothing is active before then.
/// It matches exactly the recorded command (an anchored regex over the
/// quoted words), unscoped so it applies in the Space root only, for Codex.
/// A row that cannot prove it holds the exact command, whose outcome was not
/// an approval by the user, or that is not a single supported command, gives
/// no draft and says why.
pub fn rule_from_history(report: &FrictionReport, id: String) -> Result<ApprovalRule, String> {
    let Some(approval) = &report.approval else {
        return Err("This report has no command.".into());
    };
    if report.kind != FrictionKind::Permission {
        return Err("Only approval requests can become rules.".into());
    }
    // Agents and imports can write any text into a report; only a prompt the
    // engine recorded itself describes a real request.
    if report.provenance.source != FrictionSource::Auto {
        return Err("Only approval prompts Mast recorded itself can become rules.".into());
    }
    if report.provenance.harness != "codex" {
        return Err(format!("Space rules apply to Codex only; this command ran in {}.", report.provenance.harness));
    }
    // A rule's own answer is recorded as an approval too, but nobody decided it.
    if ["Allowed by rule ", "Auto-approved by rule ", "Denied by rule "].iter().any(|prefix| report.summary.starts_with(prefix)) {
        return Err("A rule already decided this request, so there is nothing to turn into a rule.".into());
    }
    match approval.answer {
        Some(true) => {}
        Some(false) => return Err("You denied this command, so no rule is offered.".into()),
        None => return Err("No answer was recorded (the request was cancelled, interrupted or is still waiting), so no rule is offered.".into()),
    }
    let command = approval.command.as_str();
    if command.contains("[REDACTED]") || mast_harness::redact::redact_secrets(command) != command {
        return Err("Secrets were removed from the stored command, so it is no longer exact.".into());
    }
    if command.chars().count() >= RECORDED_COMMAND_LIMIT {
        return Err("The stored command was cut at 1,024 characters.".into());
    }
    if !approval.exact && !legacy_command_is_exact(command, &report.what_happened) {
        return Err("This was recorded before Mast kept exact commands and cannot be checked. Add the rule in Space settings.".into());
    }
    let value = Value::String(command.to_owned());
    // A rule matches the command words whatever the environment assignments
    // are, so a draft from a command that sets any would match more than it.
    let sets_environment = parse_approval_command(&value).is_some_and(|(segments, _)| {
        segments.iter().any(|segment| normalize_segment(segment).is_some_and(|(assignments, _)| !assignments.is_empty()))
    });
    if sets_environment {
        return Err("It sets environment variables, which a rule would not pin. Add the rule in Space settings.".into());
    }
    let mut rule = try_prefill_rule(&value, id).map_err(|reason| reason.to_owned())?;
    let words = shell_words::join(&rule.argv_prefix);
    rule.label = format!("Allow {}", words.chars().take(80).collect::<String>());
    // The matcher joins argv the same way, so this matches only this command.
    rule.command_regex = Some(format!("^{}$", regex::escape(&words)));
    rule.enabled = true;
    Ok(rule)
}

/// Rows from before exact capture hold the text between the first two
/// backticks of Codex's prompt, with an argv list joined by spaces. Accept one
/// only when it is plain words, which no quoting can distort, and the whole
/// prompt shows nothing was cut off. An argument that itself contained a space
/// still reads as two words, which is why the draft shows the prefix for review.
fn legacy_command_is_exact(command: &str, prompt: &str) -> bool {
    !command.is_empty()
        && command.split(' ').all(|word| {
            !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
        })
        && prompt.starts_with(&format!("Codex wants to run `{command}`. Allow it?"))
}

fn isolated_configuration(assignments: &[String]) -> bool {
    let Ok(temporary_root) = Path::new("/tmp").canonicalize() else {
        return false;
    };
    ["HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"]
        .iter()
        .all(|name| {
            assignments
                .iter()
                .rev()
                .find_map(|assignment| {
                    let (key, value) = assignment.split_once('=')?;
                    (key == *name).then_some(value)
                })
                .is_some_and(|value| {
                    Path::new(value).is_absolute()
                        && Path::new(value).canonicalize().is_ok_and(|path| {
                            path.starts_with(&temporary_root) && path != temporary_root
                        })
                })
        })
}

/// Reject commands with protected paths, dangerous executables or installers.
/// These checks are deliberately stricter than the minimum policy.
fn passes_safety_rails(segments: &[CommandSegment]) -> bool {
    passes_safety_rails_with_rules(segments, &[])
}

fn passes_safety_rails_with_rules(segments: &[CommandSegment], rules: &[ApprovalRule]) -> bool {
    for segment in segments {
        if segment
            .argv
            .iter()
            .any(|arg| arg.contains("sudo") || arg.contains("rm -rf") || arg.contains("git push"))
            || segment.argv.windows(2).any(|pair| {
                (pair[0] == "git" && pair[1] == "push")
                    || (pair[0] == "rm" && matches!(pair[1].as_str(), "-rf" | "-fr"))
            })
        {
            return false;
        }
        if segment.argv.iter().any(|arg| {
            let lower = arg.to_ascii_lowercase();
            [".claude", ".codex", ".zeron"]
                .iter()
                .any(|path| lower.contains(path))
        }) {
            return false;
        }
        let Some((assignments, argv)) = normalize_segment(segment) else {
            return false;
        };
        let configured_assignments = rules.iter().any(|rule| {
            rule.enabled && rule.constraints.as_ref().and_then(|policy| policy.environment.as_ref())
                .is_some_and(|expected| {
                    let mut expected = expected.clone();
                    let mut actual = assignments.clone();
                    expected.sort();
                    actual.sort();
                    expected == actual
                })
        });
        let configured_script = rules.iter().any(|rule| {
            rule.enabled && rule.constraints.as_ref().and_then(|policy| policy.repository_executable.as_ref())
                .is_some_and(|script| argv.first() == Some(&script.path))
        });
        if assignments.iter().any(|assignment| {
            if configured_assignments {
                return false;
            }
            let (name, value) = assignment.split_once('=').unwrap_or(("", ""));
            !matches!(
                name,
                "HOME" | "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | "CARGO_TARGET_DIR"
            ) && !preview_option(name, value, &argv)
        }) {
            return false;
        }
        let Some(program) = Path::new(&argv[0])
            .file_name()
            .and_then(|name| name.to_str())
        else {
            return false;
        };
        let program = program.to_ascii_lowercase();
        let read_only_handoff = is_read_only_probe(&argv);
        // `git status` reads git configuration (which can name a program to
        // run) from places `HOME` redirects, so a probe takes no inline
        // assignments unless a rule lists exactly that environment.
        if read_only_handoff && !assignments.is_empty() && !configured_assignments {
            return false;
        }
        // Restrict the initial implementation to the reviewed command families.
        // Unknown executables and interpreters still require a human decision.
        if !matches!(
            program.as_str(),
            "cargo" | "ps" | "pgrep" | "rg" | "head" | "cut" | "ulimit"
        ) && !read_only_handoff && !configured_script && argv[0] != "scripts/ui-preview.sh"
        {
            return false;
        }
        if argv[0] != program
            && !configured_script
            && argv[0] != "scripts/ui-preview.sh"
            && !matches!(
                argv[0].as_str(),
                "/bin/ps" | "/usr/bin/pgrep" | "/usr/bin/head"
            )
        {
            return false;
        }
        if program == "cargo"
            && !matches!(
                argv.get(1).map(String::as_str),
                Some("test" | "nextest" | "build" | "check" | "clippy")
            )
        {
            return false;
        }
        // Configuration files are opaque, so only inline assignments with a
        // non-network key can be checked without interpreting another file.
        if program == "cargo" && !cargo_configuration_is_safe(&argv) {
            return false;
        }
        if program == "rg" && (argv.len() != 2 || argv[1].starts_with('-')) {
            return false;
        }
        if program == "head"
            && !(argv.len() == 1
                || (argv.len() == 3
                    && argv[1] == "-n"
                    && argv[2].bytes().all(|c| c.is_ascii_digit())))
        {
            return false;
        }
        if program == "cut"
            && !(argv.len() == 2
                && argv[1].strip_prefix("-c").is_some_and(|range| {
                    !range.is_empty() && range.bytes().all(|c| c.is_ascii_digit() || c == b'-')
                }))
        {
            return false;
        }
        // Interpreters and transfer utilities can hide arbitrary shell code or installs.
        if matches!(
            program.as_str(),
            "rm" | "sudo"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "curl"
                | "wget"
                | "python"
                | "python3"
                | "perl"
                | "ruby"
                | "node"
                | "npm"
                | "npx"
                | "pnpm"
                | "yarn"
                | "brew"
                | "pip"
                | "pip3"
        ) {
            return false;
        }
        if program == "git" && argv.iter().skip(1).any(|arg| arg == "push") {
            return false;
        }
        if program == "cargo"
            && argv.get(1).is_some_and(|arg| arg == "nextest")
            && argv
                .iter()
                .skip(2)
                .take_while(|arg| arg.as_str() != "--")
                .any(|arg| {
                    matches!(
                        arg.as_str(),
                        "install" | "publish" | "login" | "add" | "update"
                    )
                })
        {
            return false;
        }
    }
    true
}

/// The read-only probes agents chain before a build (`df -Pk . && git status
/// --short --branch && git rev-parse HEAD`): exact commands only, so the
/// checks can share one command. Each still needs a rule that covers it.
fn is_read_only_probe(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    matches!(
        words.as_slice(),
        ["df", "-Pk", "."]
            | ["git", "worktree", "list"]
            | ["git", "status", "--short", "--branch"]
            | ["git", "rev-parse", "HEAD"]
    )
}

// These options choose screens, fixtures and sizes. Values have no slashes or dots.
fn preview_option(name: &str, value: &str, argv: &[String]) -> bool {
    argv[0] == "scripts/ui-preview.sh"
        && name.strip_prefix("MAST_PREVIEW_").is_some_and(|option| {
            matches!(
                option,
                "ASSISTANT"
                    | "WIDTH"
                    | "HARNESS"
                    | "RETRY"
                    | "SUBAGENTS"
                    | "THEMES"
                    | "SCREENS"
                    | "STYLES"
                    | "LAYOUTS"
                    | "COMPARE_ROWS"
                    | "COMPARE_SCREENS"
            )
        })
        && !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b',' | b'_' | b'-'))
}

fn cargo_configuration_is_safe(argv: &[String]) -> bool {
    let mut args = argv.iter().skip(2);
    while let Some(arg) = args.next() {
        let value = if arg == "--config" {
            args.next().map(String::as_str)
        } else {
            arg.strip_prefix("--config=")
        };
        if arg == "--config" || arg.starts_with("--config=") {
            let Some((key, _)) = value.and_then(|value| value.split_once('=')) else {
                return false;
            };
            let key = key.to_ascii_lowercase();
            if key.contains("registry")
                || key.contains("registries")
                || key.contains("net")
                || key.contains("source")
            {
                return false;
            }
        }
    }
    true
}

/// Parse a provider command into all executable segments, or reject ambiguous syntax.
pub(crate) fn parse_command(command: &Value) -> Option<Vec<CommandSegment>> {
    parse_command_with(command, false, &mut Vec::new())
}

/// Approval rules also accept log redirects and quoted operators, which user
/// permission rules do not. The caller must check the returned log targets.
fn parse_approval_command(command: &Value) -> Option<(Vec<CommandSegment>, Vec<String>)> {
    let mut logs = Vec::new();
    let segments = parse_command_with(command, true, &mut logs)?;
    Some((segments, logs))
}

fn parse_command_with(
    command: &Value,
    approval: bool,
    logs: &mut Vec<String>,
) -> Option<Vec<CommandSegment>> {
    match command {
        Value::String(script) => {
            if let Some(argv) = parse_shell_invocation(script) {
                return parse_argv(argv, approval, logs);
            }
            let segments = parse_script(script, approval, logs)?;
            if segments.len() == 1 && is_shell(&segments[0].argv) {
                parse_argv(segments.into_iter().next()?.argv, approval, logs)
            } else {
                Some(segments)
            }
        }
        Value::Array(values) => {
            let argv: Vec<String> = values
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            parse_argv(argv, approval, logs)
        }
        _ => None,
    }
}

// A provider may serialize a shell invocation as one string. Decode only a
// simple invocation: no outer shell expansion, control operators, or redirects.
fn parse_shell_invocation(script: &str) -> Option<Vec<String>> {
    let mut quote = None;
    let mut escaped = false;
    for c in script.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote != Some('\'') && c == '\\' {
            escaped = true;
            continue;
        }
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if q == c => quote = None,
            (None, ';' | '|' | '&' | '<' | '>' | '\n' | '\r' | '$' | '`') => return None,
            (Some('"'), '$' | '`') => return None,
            _ => {}
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    let argv = shell_words::split(script).ok()?;
    is_shell(&argv).then_some(argv)
}

fn parse_argv(
    argv: Vec<String>,
    approval: bool,
    logs: &mut Vec<String>,
) -> Option<Vec<CommandSegment>> {
    if is_shell(&argv) {
        return parse_script(&argv[2], approval, logs);
    }
    if argv.is_empty() {
        return None;
    }
    Some(vec![CommandSegment {
        argv,
        pipeline_input: false,
    }])
}

fn is_shell(argv: &[String]) -> bool {
    argv.len() == 3
        && matches!(argv[0].as_str(), "/bin/zsh" | "/bin/bash" | "/bin/sh")
        && matches!(argv[1].as_str(), "-c" | "-lc")
}

fn parse_script(
    script: &str,
    approval: bool,
    logs: &mut Vec<String>,
) -> Option<Vec<CommandSegment>> {
    let stripped;
    let script = if approval {
        stripped = strip_log_redirects(script, logs);
        stripped.as_str()
    } else {
        script
    };
    // Do not attempt shell expansion, escaping, comments, redirection, globbing,
    // command substitution or compound commands. For approval rules, quotes make
    // the remaining operators literal. Backquotes and backslashes are always rejected.
    let mut quote = None;
    for c in script.chars() {
        if matches!(c, '`' | '\\' | '\r' | '\0') {
            return None;
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) if approval => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            _ if matches!(
                c,
                '<' | '>' | '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '#'
            ) =>
            {
                return None;
            }
            _ => {}
        }
    }
    // Only standalone assignment words can contain this expansion. In
    // particular, a single-quoted value is literal in the shell, not $PWD.
    if script.contains('$') {
        let assignment = regex::Regex::new(r#"(?:^|[\s;])[_a-zA-Z][_a-zA-Z0-9]*=(?:\"\$PWD(?:/[a-zA-Z0-9_.-]+)*\"|\$PWD(?:/[a-zA-Z0-9_.-]+)*)(?:\s|$)"#).ok()?;
        let covered: Vec<_> = assignment.find_iter(script).collect();
        if script.match_indices('$').any(|(index, _)| {
            !covered
                .iter()
                .any(|m| m.start() <= index && index < m.end())
        }) {
            return None;
        }
    }
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut pipeline_input = false;
    let mut chars = script.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        if matches!(c, ';' | '\n' | '|' | '&') {
            let next_is_same = chars.peek().is_some_and(|(_, next)| *next == c);
            let length = if c == '&' && next_is_same {
                chars.next();
                2
            } else {
                1
            };
            if (c == '&' && length == 1) || (c == '|' && next_is_same) {
                return None;
            }
            let argv = shell_words::split(script[start..index].trim()).ok()?;
            if argv.is_empty() {
                return None;
            }
            segments.push(CommandSegment {
                argv,
                pipeline_input,
            });
            pipeline_input = c == '|';
            start = index + length;
        }
    }
    if quote.is_some() {
        return None;
    }
    let argv = shell_words::split(script[start..].trim()).ok()?;
    if argv.is_empty() {
        return None;
    }
    segments.push(CommandSegment {
        argv,
        pipeline_input,
    });
    Some(segments)
}

/// Remove `> /tmp/<name> 2>&1` where it ends a segment outside quotes, and
/// collect the targets. Agents log long runs this way. Only spaces and tabs
/// count as separators, because the shell splits words on nothing else.
fn strip_log_redirects(script: &str, logs: &mut Vec<String>) -> String {
    static REDIRECT: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"[ \t]> ?((?:/private)?/tmp/[A-Za-z0-9_][A-Za-z0-9_.-]*) 2>&1([ \t]*(?:;|&&|\n|$))",
        )
        .expect("valid log redirect pattern")
    });
    let mut stripped = String::with_capacity(script.len());
    let mut last = 0;
    for captures in REDIRECT.captures_iter(script) {
        let (Some(whole), Some(target), Some(end)) =
            (captures.get(0), captures.get(1), captures.get(2))
        else {
            continue;
        };
        if !outside_quotes(&script[..whole.start()]) {
            continue;
        }
        logs.push(target.as_str().to_owned());
        stripped.push_str(&script[last..whole.start()]);
        stripped.push_str(end.as_str());
        last = whole.end();
    }
    stripped.push_str(&script[last..]);
    stripped
}

/// A log target may be missing or a regular file the engine's user owns.
/// Best effort: the shell opens the file later, so a swap in between is possible.
fn log_target_is_safe(path: &str) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.is_file() && owned_by_current_user(&meta),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn owned_by_current_user(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    meta.uid() == unsafe { libc::geteuid() }
}

fn outside_quotes(prefix: &str) -> bool {
    let mut quote = None;
    for c in prefix.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None => {}
        }
    }
    quote.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn separates_shell_segments_without_splitting_quoted_arguments() {
        let parsed = parse_command(&json!(["/bin/zsh", "-lc", "ulimit -n 4096; CARGO_TARGET_DIR=/tmp/target perl -e 'alarm 120; exec @ARGV' cargo test && ps -ax | rg cargo"])).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[1].argv[3], "alarm 120; exec @ARGV");
        assert!(parsed[3].pipeline_input);
    }

    #[test]
    fn rejects_ambiguous_shell_constructs() {
        for script in [
            "cargo test;",
            "cargo test || echo no",
            "cargo test & echo no",
            "cargo test > out",
            "cargo test $(touch x)",
            "cargo test `touch x`",
            "cargo test \\\nrm x",
            "cargo test # comment",
            "cargo test |",
        ] {
            assert!(parse_command(&json!(script)).is_none(), "{script:?}");
        }
    }

    #[test]
    fn strips_only_known_wrappers_and_retains_assignments() {
        let segments = parse_command(&json!(
            "HOME=/tmp/test caffeinate -i perl -e 'alarm 120; exec @ARGV' cargo test"
        ))
        .unwrap();
        let (env, argv) = normalize_segment(&segments[0]).unwrap();
        assert_eq!(env, ["HOME=/tmp/test"]);
        assert_eq!(argv, ["cargo", "test"]);
        assert!(!is_alarm_wrapper(
            "alarm 120; system('touch x'); exec @ARGV"
        ));
    }

    #[test]
    fn every_segment_requires_a_matching_rule_and_scope() {
        let root = tempfile::tempdir().unwrap();
        let rule = ApprovalRule {
            id: "test".into(),
            label: "Tests".into(),
            created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex,
            enabled: true,
            argv_prefix: vec!["cargo".into(), "test".into()],
            command_regex: Some("^cargo test".into()),
            cwd_scope: Some(root.path().to_string_lossy().into()),
            require_isolated_home: false,
            constraints: None,
            hit_count: 0,
            last_used_at: None,
        };
        let rules = [rule];
        let matches = |command: &str| {
            matching_rules(
                &json!(command),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                &rules,
            )
            .is_some()
        };
        assert!(matches("ulimit -n 4096; HOME=/tmp/test cargo test"));
        assert!(!matches("cargo test --offline; git push"));
        assert!(matches("cargo test --offline | rg test"));
        assert!(!matches("cargo test --offline | rg --pre=evil"));
        assert!(!matches("cargo build"));
        assert!(
            matching_rules(
                &json!("cargo test"),
                ApprovalRuleHarness::ClaudeCode,
                root.path(),
                &[root.path()],
                &rules
            )
            .is_none()
        );
        assert!(
            matching_rules(
                &json!("cargo test"),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[],
                &rules
            )
            .is_none()
        );
    }

    #[test]
    fn scoped_pwd_assignments_require_safe_shell_syntax_and_scope() {
        let root = tempfile::tempdir().unwrap();
        let rule = ApprovalRule {
            id: "preview".into(),
            label: "Preview".into(),
            created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex,
            enabled: true,
            argv_prefix: vec!["scripts/ui-preview.sh".into()],
            command_regex: None,
            cwd_scope: Some(root.path().to_string_lossy().into()),
            require_isolated_home: false,
            constraints: None,
            hit_count: 0,
            last_used_at: None,
        };
        let check = |script: &str, rule: &ApprovalRule| {
            matching_rules(
                &json!(["/bin/zsh", "-lc", script]),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                std::slice::from_ref(rule),
            )
            .is_some()
        };
        assert!(check(
            "CARGO_TARGET_DIR=\"$PWD/target-clean\" perl -e 'alarm 1200; exec @ARGV' scripts/ui-preview.sh --screens chat",
            &rule
        ));
        let serialized = r#"/bin/zsh -lc 'CARGO_TARGET_DIR="$PWD/target-clean" perl -e '"'"'alarm 1200; exec @ARGV'"'"' scripts/ui-preview.sh --screens chat'"#;
        assert!(
            matching_rules(
                &json!(serialized),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                std::slice::from_ref(&rule)
            )
            .is_some()
        );
        assert!(
            matching_rules(
                &json!(format!("{serialized}; rm -rf /tmp/x")),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                std::slice::from_ref(&rule)
            )
            .is_none()
        );
        assert!(check("CARGO_TARGET_DIR=$PWD scripts/ui-preview.sh", &rule));
        for script in [
            "CARGO_TARGET_DIR=$HOME scripts/ui-preview.sh",
            "CARGO_TARGET_DIR=$(touch /tmp/x) scripts/ui-preview.sh",
            "CARGO_TARGET_DIR=`touch /tmp/x` scripts/ui-preview.sh",
            "CARGO_TARGET_DIR=$PWD/../escape scripts/ui-preview.sh",
            "CARGO_TARGET_DIR='$PWD/target' scripts/ui-preview.sh",
            "CARGO_TARGET_DIR=$PWD/target scripts/ui-preview.sh; rm -rf /tmp/x",
            "CARGO_TARGET_DIR=$PWD/target scripts/ui-preview.sh && rm -rf /tmp/x",
        ] {
            assert!(!check(script, &rule), "{script}");
        }
        let mut unscoped = rule.clone();
        unscoped.cwd_scope = None;
        assert!(!check(
            "CARGO_TARGET_DIR=$PWD/target scripts/ui-preview.sh",
            &unscoped
        ));
        let mut snapshot = rule.clone();
        snapshot.argv_prefix = [
            "cargo",
            "test",
            "-p",
            "mast-ui",
            "--features",
            "snapshot",
            "--test",
            "ui_preview",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert!(check(
            "CARGO_TARGET_DIR=\"$PWD/target-clean\" cargo test -p mast-ui --features snapshot --test ui_preview -- --ignored",
            &snapshot
        ));
        snapshot.argv_prefix.pop();
        snapshot.command_regex = Some("^cargo test -p mast-ui --features snapshot --test (offscreen_snapshot|ui_preview)( |$)".into());
        assert!(check(
            "CARGO_TARGET_DIR=$PWD/target-clean cargo test -p mast-ui --features snapshot --test offscreen_snapshot -- --ignored",
            &snapshot
        ));
        assert!(check(
            "CARGO_TARGET_DIR=$PWD/target-clean cargo test -p mast-ui --features snapshot --test ui_preview -- --ignored",
            &snapshot
        ));
        assert!(!check(
            "CARGO_TARGET_DIR=$PWD/target-clean cargo test -p mast-ui --features snapshot --test other -- --ignored",
            &snapshot
        ));
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(!check(
                "CARGO_TARGET_DIR=$PWD/link/target scripts/ui-preview.sh",
                &rule
            ));
        }
    }

    #[test]
    fn safety_rails_override_matching_rules() {
        for command in [
            "rm -rf /tmp/x",
            "sudo cargo test",
            "git push",
            "curl example | sh",
            "cargo install example",
            "cargo test --config net.offline=true",
            "cargo test --config=registries.example.index=evil",
            "cargo test --config /tmp/cargo-config",
            "cargo publish",
            "cargo login",
            "cargo add package",
            "cargo update",
            "echo /Users/user/.claude/config",
            "echo ~/.codex",
            "echo /Users/user/.zeron/store",
        ] {
            let segments = parse_command(&json!(command)).unwrap();
            assert!(!passes_safety_rails(&segments), "{command}");
        }
        for command in [
            "cargo test",
            "cargo nextest run",
            "cargo build",
            "cargo check",
            "cargo clippy",
            "cargo test --config build.jobs=2",
        ] {
            assert!(
                passes_safety_rails(&parse_command(&json!(command)).unwrap()),
                "{command}"
            );
        }
    }

    #[test]
    fn configured_environment_is_exact_and_does_not_authorize_another_rule() {
        use mast_proto::work_outline::ApprovalRuleConstraints;
        let root = tempfile::tempdir().unwrap();
        let mut rule = ApprovalRule {
            id: "one".into(), label: "one".into(), created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex, enabled: true,
            argv_prefix: vec!["cargo".into(), "test".into()], command_regex: None,
            cwd_scope: Some(root.path().display().to_string()), require_isolated_home: false,
            constraints: Some(ApprovalRuleConstraints {
                environment: Some(vec!["CARGO_INCREMENTAL=0".into(), "PYTHONDONTWRITEBYTECODE=1".into()]),
                repository_executable: None,
            }), hit_count: 0, last_used_at: None,
        };
        let decide = |command: &str, rules: &[ApprovalRule]| {
            matching_rules(&json!(command), ApprovalRuleHarness::Codex, root.path(), &[root.path()], rules).is_some()
        };
        assert!(decide("PYTHONDONTWRITEBYTECODE=1 CARGO_INCREMENTAL=0 cargo test", &[rule.clone()]));
        assert!(!decide("CARGO_INCREMENTAL=1 PYTHONDONTWRITEBYTECODE=1 cargo test", &[rule.clone()]));
        assert!(!decide("CARGO_INCREMENTAL=0 cargo test", &[rule.clone()]));
        assert!(!decide("CARGO_INCREMENTAL=0 PYTHONDONTWRITEBYTECODE=1 cargo build", &[rule.clone()]));
        let mut legacy = rule.clone();
        legacy.id = "two".into();
        legacy.argv_prefix = vec!["cargo".into(), "build".into()];
        legacy.constraints = None;
        assert!(!decide("CARGO_INCREMENTAL=0 PYTHONDONTWRITEBYTECODE=1 cargo build", &[rule.clone(), legacy]));
        rule.enabled = false;
        assert!(!decide("CARGO_INCREMENTAL=0 PYTHONDONTWRITEBYTECODE=1 cargo test", &[rule]));
    }

    #[cfg(unix)]
    #[test]
    fn repository_executable_requires_matching_content_and_checkout() {
        use mast_proto::work_outline::{ApprovalRuleConstraints, RepositoryApprovalExecutable};
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("scripts")).unwrap();
        let script = root.path().join("scripts/run.sh");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&script).unwrap()));
        let rule = ApprovalRule {
            id: "script".into(), label: "script".into(), created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex, enabled: true,
            argv_prefix: vec!["scripts/run.sh".into()], command_regex: Some("^scripts/run\\.sh abc$".into()),
            cwd_scope: Some(root.path().display().to_string()), require_isolated_home: false,
            constraints: Some(ApprovalRuleConstraints { environment: Some(vec![]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/run.sh".into(), sha256: Some(digest) }) }),
            hit_count: 0, last_used_at: None,
        };
        let decide = |command: &str| matching_rules(&json!(command), ApprovalRuleHarness::Codex, root.path(), &[root.path()], std::slice::from_ref(&rule)).is_some();
        assert!(decide("scripts/run.sh abc"));
        assert!(!decide("scripts/run.sh abc extra"));
        let nested = root.path().join("nested");
        std::fs::create_dir_all(nested.join("scripts")).unwrap();
        std::fs::copy(&script, nested.join("scripts/run.sh")).unwrap();
        assert!(matching_rules(&json!("scripts/run.sh abc"), ApprovalRuleHarness::Codex, &nested, &[root.path()], std::slice::from_ref(&rule)).is_none());
        let outside = tempfile::tempdir().unwrap();
        let escaped = outside.path().join("escape.sh");
        std::fs::copy(&script, &escaped).unwrap();
        std::os::unix::fs::symlink(&escaped, root.path().join("scripts/escape.sh")).unwrap();
        let mut escape_rule = rule.clone();
        escape_rule.argv_prefix = vec!["scripts/escape.sh".into()];
        escape_rule.command_regex = Some("^scripts/escape\\.sh abc$".into());
        escape_rule.constraints.as_mut().unwrap().repository_executable.as_mut().unwrap().path = "scripts/escape.sh".into();
        assert!(matching_rules(&json!("scripts/escape.sh abc"), ApprovalRuleHarness::Codex, root.path(), &[root.path()], &[escape_rule]).is_none());
        std::fs::write(&script, b"#!/bin/sh\nexit 1\n").unwrap();
        assert!(!decide("scripts/run.sh abc"));
    }

    #[test]
    fn script_rules_are_fully_anchored_and_oversized_files_never_match() {
        use mast_proto::work_outline::{ApprovalRuleConstraints, RepositoryApprovalExecutable};
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("scripts")).unwrap();
        let script = root.path().join("scripts/run.sh");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&script).unwrap()));
        // Built directly, since validation refuses a top-level alternation.
        let rule = ApprovalRule {
            id: "script".into(), label: "script".into(), created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex, enabled: true,
            argv_prefix: vec!["scripts/run.sh".into()], command_regex: Some("^scripts/run\\.sh|^abc$".into()),
            cwd_scope: Some(root.path().display().to_string()), require_isolated_home: false,
            constraints: Some(ApprovalRuleConstraints { environment: Some(vec![]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/run.sh".into(), sha256: Some(digest) }) }),
            hit_count: 0, last_used_at: None,
        };
        let decide = |command: &str| matching_rules(&json!(command), ApprovalRuleHarness::Codex, root.path(), &[root.path()], std::slice::from_ref(&rule)).is_some();
        assert!(decide("scripts/run.sh"));
        assert!(!decide("scripts/run.sh --anything"));
        let big = root.path().join("scripts/big.sh");
        std::fs::write(&big, vec![b'#'; (MAX_FINGERPRINTED_BYTES + 1) as usize]).unwrap();
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(fingerprint_repository_executable(root.path().to_str().unwrap(), "scripts/big.sh").is_none());
        assert!(fingerprint_repository_executable(root.path().to_str().unwrap(), "scripts/run.sh").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_digest_from_another_file_or_in_another_spelling_never_matches() {
        use mast_proto::work_outline::{ApprovalRuleConstraints, RepositoryApprovalExecutable};
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("scripts")).unwrap();
        let write = |name: &str, body: &[u8]| {
            let path = root.path().join("scripts").join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            format!("{:x}", Sha256::digest(body))
        };
        let own = write("run.sh", b"#!/bin/sh\nexit 0\n");
        let other = write("other.sh", b"#!/bin/sh\nexit 1\n");
        let rule = |digest: &str| ApprovalRule {
            id: "script".into(), label: "script".into(), created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex, enabled: true,
            argv_prefix: vec!["scripts/run.sh".into()], command_regex: Some("^scripts/run\\.sh abc$".into()),
            cwd_scope: Some(root.path().display().to_string()), require_isolated_home: false,
            constraints: Some(ApprovalRuleConstraints { environment: Some(vec![]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/run.sh".into(), sha256: Some(digest.into()) }) }),
            hit_count: 0, last_used_at: None,
        };
        let decide = |digest: &str| matching_rules(&json!("scripts/run.sh abc"), ApprovalRuleHarness::Codex, root.path(), &[root.path()], &[rule(digest)]).is_some();
        assert!(decide(&own));
        // Stored rules can be built without validation (a synced or hand-edited document).
        assert!(!decide(&other));
        assert!(!decide(&own.to_uppercase()));
        assert!(!decide(&format!("sha256:{own}")));
        assert!(!decide(&own[..63]));
        assert!(!decide(""));
        // A rule saved without any digest never matches.
        let mut unpinned = rule(&own);
        unpinned.constraints.as_mut().unwrap().repository_executable.as_mut().unwrap().sha256 = None;
        assert!(matching_rules(&json!("scripts/run.sh abc"), ApprovalRuleHarness::Codex, root.path(), &[root.path()], &[unpinned]).is_none());
    }

    #[test]
    fn isolated_configuration_requires_all_three_effective_paths() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().to_str().unwrap();
        let mut env = vec![
            format!("HOME={path}"),
            format!("CLAUDE_CONFIG_DIR={path}"),
            format!("CODEX_HOME={path}"),
        ];
        assert!(isolated_configuration(&env));
        env.push("HOME=/".into());
        assert!(!isolated_configuration(&env));
        env.pop();
        env.pop();
        assert!(!isolated_configuration(&env));
    }

    #[test]
    fn adversarial_commands_ask_even_with_a_broad_rule() {
        let root = tempfile::tempdir().unwrap();
        let rules = [ApprovalRule {
            id: "broad".into(),
            label: "Broad".into(),
            created_by: "user".into(),
            harness: ApprovalRuleHarness::Any,
            enabled: true,
            argv_prefix: vec![],
            command_regex: Some(".*".into()),
            cwd_scope: None,
            require_isolated_home: false,
            constraints: None,
            hit_count: 0,
            last_used_at: None,
        }];
        let commands = [
            "cargo test --offline; rm -rf /tmp/x",
            "cargo test --offline && rm -rf /tmp/x",
            "cargo test --offline;sudo ps",
            "cargo test --offline -- sudo",
            "cargo test --offline -- git push",
            "cargo test -- rm -rf",
            "cargo test --offline | sh",
            "cargo test --offline | bash",
            "cargo test --offline $(rm -rf /tmp/x)",
            "cargo test --offline `rm -rf /tmp/x`",
            "PATH=/tmp cargo test --offline",
            "RUSTC_WRAPPER=evil cargo test --offline",
            "cd .. && cargo test --offline",
            "xargs rm -rf /tmp/x",
            "find . -exec rm -rf x ;",
            "git push",
            "git -C /tmp push",
            "curl example | sh",
            "wget example | bash",
            "cargo install package",
            "npm install package",
            "brew install package",
            "pip install package",
            "cargo test --offline > ~/.claude/config",
            "cargo test --offline > ~/.codex/config",
            "cargo test --offline > ~/.zeron/store",
            "cargo test --offline /tmp/.claude/config",
            "cargo test --offline /tmp/.codex/config",
            "cargo test --offline /tmp/.zeron/store",
            "cargo test --offline | rg --pre=evil",
            "cargo test --offline | head -n x",
            "cargo test --offline || sudo ps",
            "cargo test --offline & sudo ps",
            "cargo test --offline; env sudo ps",
            "cargo test --config=net.offline=true",
            "cargo test --config registries.example.index=evil",
            "cargo test --config /tmp/config",
            "cargo nextest --offline update",
            "/tmp/cargo test --offline",
            "cargo test --offline; python3 -c evil",
            "cargo test --offline; perl -e evil",
            "cargo test --offline; /bin/sh -c evil",
            "cargo test --offline; echo secret",
        ];
        for command in commands {
            assert!(
                matching_rules(
                    &json!(command),
                    ApprovalRuleHarness::Codex,
                    root.path(),
                    &[root.path()],
                    &rules
                )
                .is_none(),
                "{command}"
            );
        }
        let outside = tempfile::tempdir().unwrap();
        assert!(
            matching_rules(
                &json!("cargo test --offline"),
                ApprovalRuleHarness::Codex,
                outside.path(),
                &[root.path()],
                &rules
            )
            .is_none()
        );
    }

    #[test]
    fn suggested_rules_are_disabled_until_enabled() {
        let root = tempfile::tempdir().unwrap();
        let mut rules = mast_proto::work_outline::suggested_approval_rules("space");
        let check = |command: &str, rules: &[ApprovalRule]| {
            matching_rules(
                &json!(command),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                rules,
            )
            .is_some()
        };
        assert!(!check("ps -ax", &rules));
        rules.iter_mut().for_each(|rule| rule.enabled = true);
        assert!(check("ps -ax", &rules));
        assert!(check("pgrep cargo", &rules));
        assert!(check("scripts/ui-preview.sh", &rules));
        assert!(check(
            "cargo test --offline --features snapshot --test offscreen_snapshot",
            &rules
        ));
        assert!(!check("cargo test --offline", &rules));
        let isolated = tempfile::tempdir_in("/tmp").unwrap();
        let path = isolated.path().to_str().unwrap();
        assert!(check(
            &format!("HOME={path} CLAUDE_CONFIG_DIR={path} CODEX_HOME={path} cargo test"),
            &rules
        ));
    }

    #[test]
    fn logged_preview_and_process_commands_match_rules() {
        let root = tempfile::tempdir().unwrap();
        let mut rules = mast_proto::work_outline::suggested_approval_rules("space");
        rules.iter_mut().for_each(|rule| {
            rule.enabled = true;
            rule.require_isolated_home = false;
        });
        let check = |script: &str| {
            matching_rules(
                &json!(["/bin/zsh", "-lc", script]),
                ApprovalRuleHarness::Codex,
                root.path(),
                &[root.path()],
                &rules,
            )
            .is_some()
        };
        for script in [
            "MAST_PREVIEW_ASSISTANT=1 MAST_PREVIEW_WIDTH=600 scripts/ui-preview.sh --screens overview-column > /private/tmp/mast-preview.log 2>&1",
            "MAST_PREVIEW_WIDTH=1440 scripts/ui-preview.sh --screens a > /tmp/wide.log 2>&1\nMAST_PREVIEW_WIDTH=640 scripts/ui-preview.sh --screens a > /tmp/narrow.log 2>&1",
            "cargo test -p mast-engine --lib token_usage -- --test-threads=1 > /tmp/tests.log 2>&1",
            "cargo test > /tmp/a.log 2>&1 && cargo nextest run > /tmp/b.log 2>&1",
            "cargo test >/tmp/a.log 2>&1",
            "cargo test\t--lib > /tmp/a.log 2>&1\ncargo test",
            "ps -Ao pid,etime,command | rg 'cargo (build|test)|rustc --crate-name' | cut -c1-260",
            "ps -ax | rg \"it's [a-z]\"",
        ] {
            assert!(check(script), "{script:?}");
        }
        for script in [
            "cargo test > /tmp/a.log",
            "cargo test >> /tmp/a.log 2>&1",
            "cargo test > /tmp/../etc/a 2>&1",
            "cargo test > /tmp/dir/a.log 2>&1",
            "cargo test > /tmp/.hidden 2>&1",
            "cargo test > /var/tmp/a.log 2>&1",
            "cargo test > /tmp/a.log 2>&1 &",
            "cargo test > /tmp/a.log 2>&1 | sh",
            "cargo test < /tmp/a.log",
            "cargo test 2>&1",
            "cargo test 1> /tmp/a.log 2>&1",
            "cargo test &> /tmp/a.log",
            "cargo test >| /tmp/a.log 2>&1",
            "cargo test > /tmp/a.log 2>&1 > /tmp/b.log 2>&1",
            "cargo test\u{a0}> /tmp/a.log 2>&1",
            "cargo test\u{c}> /tmp/a.log 2>&1",
            "cargo test '> /tmp/a.log 2>&1",
            "MAST_PREVIEW_WIDTH=600 cargo test",
            "MAST_PREVIEW_OUT=/etc scripts/ui-preview.sh",
            "MAST_PREVIEW_SERVE=1 scripts/ui-preview.sh",
            "MAST_PREVIEW_WIDTH= scripts/ui-preview.sh",
            "ps -ax | cut -d, -f1",
            "ps -ax | awk '{print $1}'",
            "touch crates/ui/src/lib.rs; scripts/ui-preview.sh",
        ] {
            assert!(!check(script), "{script:?}");
        }
        for (script, argv) in [
            ("rg '> /tmp/a.log 2>&1'", "> /tmp/a.log 2>&1"),
            ("rg \"> /tmp/a.log 2>&1\"", "> /tmp/a.log 2>&1"),
        ] {
            let (segments, logs) = parse_approval_command(&json!(script)).unwrap();
            assert_eq!(segments[0].argv, ["rg", argv]);
            assert!(logs.is_empty());
        }
        let (segments, logs) = parse_approval_command(&json!(
            "cargo test > /tmp/a.log 2>&1\ncargo build > /private/tmp/b.log 2>&1"
        ))
        .unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1].argv, ["cargo", "build"]);
        assert_eq!(logs, ["/tmp/a.log", "/private/tmp/b.log"]);
        assert!(parse_approval_command(&json!("cargo test\n > /tmp/a.log 2>&1")).is_none());
        let dir = tempfile::Builder::new()
            .prefix("mast-approval-dir")
            .tempdir_in("/tmp")
            .unwrap();
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(!check(&format!("cargo test > /tmp/{name} 2>&1")));
        let file = tempfile::Builder::new()
            .prefix("mast-approval-log")
            .tempfile_in("/tmp")
            .unwrap();
        let name = file.path().file_name().unwrap().to_str().unwrap();
        assert!(check(&format!("cargo test > /tmp/{name} 2>&1")));
        #[cfg(unix)]
        {
            let name = format!("mast-approval-link-{}", std::process::id());
            let link = Path::new("/tmp").join(&name);
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(root.path(), &link).unwrap();
            let blocked = !check(&format!("cargo test > /tmp/{name} 2>&1"));
            std::fs::remove_file(&link).unwrap();
            assert!(blocked);
        }
    }

    #[test]
    fn permission_rules_parse_strictly() {
        for command in [
            "cargo test > /tmp/a.log 2>&1",
            "awk 'BEGIN{system(\"x\")}'",
            "find . -name '*.rs'",
            "rg '(a|b)'",
        ] {
            assert!(parse_command(&json!(command)).is_none(), "{command}");
        }
    }

    #[test]
    fn prefill_is_disabled_and_preserves_quoted_arguments() {
        let rule =
            prefill_rule_from_command(&json!("pgrep 'worker process'"), "id".into()).unwrap();
        assert!(!rule.enabled);
        assert_eq!(rule.argv_prefix, ["pgrep", "worker process"]);
        assert!(prefill_rule_from_command(&json!("ps; rm -rf x"), "id".into()).is_none());
        let rule = prefill_rule_from_command(
            &json!("ulimit -n 4096; perl -e 'alarm 120; exec @ARGV' cargo test -p mast-ui"),
            "id".into(),
        )
        .unwrap();
        assert_eq!(rule.argv_prefix, ["cargo", "test", "-p", "mast-ui"]);
        assert!(prefill_rule_from_command(&json!("ps; pgrep cargo"), "id".into()).is_none());
        assert!(prefill_rule_from_command(&json!("HOME=/tmp cargo test"), "id".into()).is_none());
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let path = home.path().to_str().unwrap();
        let command = format!("HOME={path} CLAUDE_CONFIG_DIR={path} CODEX_HOME={path} cargo test");
        assert!(
            prefill_rule_from_command(&json!(command), "id".into())
                .unwrap()
                .require_isolated_home
        );
    }

    #[test]
    fn the_chained_build_preflight_matches_through_one_rule_per_probe() {
        let root = tempfile::tempdir().unwrap();
        let rule = |id: &str, prefix: &[&str]| ApprovalRule {
            id: id.into(),
            label: id.into(),
            created_by: "user".into(),
            harness: ApprovalRuleHarness::Codex,
            enabled: true,
            argv_prefix: prefix.iter().map(|word| (*word).to_owned()).collect(),
            command_regex: None,
            cwd_scope: Some(root.path().to_string_lossy().into()),
            require_isolated_home: false,
            constraints: None,
            hit_count: 0,
            last_used_at: None,
        };
        let matches = |command: &str, rules: &[ApprovalRule]| {
            matching_rules(&json!(command), ApprovalRuleHarness::Codex, root.path(), &[root.path()], rules)
                .map(|matched| matched.len())
        };
        let preflight = "df -Pk . && git status --short --branch && git rev-parse HEAD";
        let all = [
            rule("df", &["df", "-Pk", "."]),
            rule("status", &["git", "status", "--short", "--branch"]),
            rule("head", &["git", "rev-parse", "HEAD"]),
        ];
        assert_eq!(matches(preflight, &all), Some(3));
        // Without a rule for one probe the whole command still asks.
        assert_eq!(matches(preflight, &all[..2]), None);
        // A broad rule does not widen the probes beyond their exact commands.
        let broad = [rule("df", &["df", "-Pk", "."]), rule("git", &["git"])];
        assert_eq!(matches("df -Pk . && git status --short --branch", &broad), Some(2));
        for command in [
            "git status",
            "git status --short --branch --ignored",
            "git rev-parse HEAD~1",
            "git rev-parse --show-toplevel",
            "git log",
            "df -Pk . && git push",
            // An inline HOME would redirect where git reads its configuration.
            "HOME=/tmp/elsewhere git status --short --branch",
            "df -Pk . && CARGO_TARGET_DIR=/tmp/t git rev-parse HEAD",
        ] {
            assert_eq!(matches(command, &broad), None, "{command}");
        }
    }
}

#[cfg(test)]
mod history_tests;
