//! Conservative parsing for command approvals. This module does not approve commands.
//!
//! Shell constructs that cannot be represented exactly are rejected. In particular,
//! expansion, redirection, background jobs and nested shells are never interpreted.

use mast_proto::work_outline::{ApprovalRule, ApprovalRuleHarness};
use serde_json::Value;
use std::path::Path;

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
    let segments = parse_command(command)?;
    if !passes_safety_rails(&segments) {
        return None;
    }
    let mut matches = Vec::new();
    for segment in &segments {
        let (_, argv) = normalize_segment(segment)?;
        if !segment.pipeline_input
            && argv.len() == 3
            && argv[0] == "ulimit"
            && argv[1] == "-n"
            && !argv[2].is_empty()
            && argv[2].bytes().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if segment.pipeline_input && matches!(argv[0].as_str(), "rg" | "head" | "/usr/bin/head") {
            continue;
        }
        let rule = rules.iter().find(|rule| {
            if !rule.enabled
                || (rule.harness != ApprovalRuleHarness::Any && rule.harness != harness)
            {
                return false;
            }
            if rule.argv_prefix.is_empty() && rule.command_regex.is_none() {
                return false;
            }
            if !argv.starts_with(&rule.argv_prefix) {
                return false;
            }
            if let Some(pattern) = &rule.command_regex {
                let Ok(regex) = regex::Regex::new(pattern) else {
                    return false;
                };
                if !regex.is_match(&shell_words::join(&argv)) {
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

/// Reject commands with protected paths, dangerous executables or installers.
/// These checks are deliberately stricter than the minimum policy.
fn passes_safety_rails(segments: &[CommandSegment]) -> bool {
    for segment in segments {
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
        if assignments.iter().any(|assignment| {
            let name = assignment
                .split_once('=')
                .map(|(name, _)| name)
                .unwrap_or("");
            !matches!(
                name,
                "HOME" | "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | "CARGO_TARGET_DIR"
            )
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
        // Restrict the initial implementation to the reviewed command families.
        // Unknown executables and interpreters still require a human decision.
        if !matches!(
            program.as_str(),
            "cargo" | "ps" | "pgrep" | "rg" | "head" | "ulimit"
        ) && argv[0] != "scripts/ui-preview.sh"
        {
            return false;
        }
        if argv[0] != program
            && argv[0] != "scripts/ui-preview.sh"
            && !matches!(
                argv[0].as_str(),
                "/bin/ps" | "/usr/bin/pgrep" | "/usr/bin/head"
            )
        {
            return false;
        }
        if program == "cargo"
            && !matches!(argv.get(1).map(String::as_str), Some("test" | "nextest"))
        {
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
        if matches!(program.as_str(), "cargo" | "rustup")
            && argv
                .iter()
                .skip(1)
                .any(|arg| matches!(arg.as_str(), "install" | "update"))
        {
            return false;
        }
    }
    true
}

/// Parse a provider command into all executable segments, or reject ambiguous syntax.
pub(crate) fn parse_command(command: &Value) -> Option<Vec<CommandSegment>> {
    match command {
        Value::String(script) => {
            let segments = parse_script(script)?;
            if segments.len() == 1 && is_shell(&segments[0].argv) {
                parse_argv(segments.into_iter().next()?.argv)
            } else {
                Some(segments)
            }
        }
        Value::Array(values) => {
            let argv: Vec<String> = values
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            parse_argv(argv)
        }
        _ => None,
    }
}

fn parse_argv(argv: Vec<String>) -> Option<Vec<CommandSegment>> {
    if is_shell(&argv) {
        return parse_script(&argv[2]);
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

fn parse_script(script: &str) -> Option<Vec<CommandSegment>> {
    // Do not attempt shell expansion, escaping, comments, redirection, globbing,
    // command substitution or compound commands, even in quoted arguments.
    if script.chars().any(|c| {
        matches!(
            c,
            '$' | '`'
                | '\\'
                | '<'
                | '>'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '('
                | ')'
                | '#'
                | '\r'
                | '\0'
        )
    }) {
        return None;
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
        assert!(!matches("cargo test; git push"));
        assert!(matches("cargo test | rg test"));
        assert!(!matches("cargo test | rg --pre=evil"));
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
    fn safety_rails_override_matching_rules() {
        for command in [
            "rm -rf /tmp/x",
            "sudo cargo test",
            "git push",
            "curl example | sh",
            "cargo install example",
            "echo /Users/user/.claude/config",
            "echo ~/.codex",
            "echo /Users/user/.zeron/store",
        ] {
            let segments = parse_command(&json!(command)).unwrap();
            assert!(!passes_safety_rails(&segments), "{command}");
        }
        assert!(passes_safety_rails(
            &parse_command(&json!("cargo test")).unwrap()
        ));
    }
}
