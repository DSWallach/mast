//! Rules drafted from approval history, and the exact command the history keeps.

use super::*;
use mast_proto::UserInputQuestion;
use serde_json::json;

fn row(command: &str, exact: bool, answer: Option<bool>, prompt: &str, source: &str, harness: &str) -> FrictionReport {
    serde_json::from_value(json!({
        "id": "r", "kind": "permission", "summary": "Approve command requested", "whatHappened": prompt,
        "workaround": null, "suggestion": null,
        "approval": {"command": command, "why": "sandbox", "answer": answer, "exact": exact},
        "provenance": {"chatId": "c", "harness": harness, "model": null, "spaceId": "s", "createdAt": 1, "source": source},
        "fingerprint": "f", "occurrences": 1, "lastSeen": 1, "status": "open"
    })).unwrap()
}

fn exact(command: &str) -> FrictionReport {
    row(command, true, Some(true), &format!("Codex wants to run `{command}`. Allow it?"), "auto", "codex")
}

fn legacy(command: &str, prompt: &str) -> FrictionReport {
    row(command, false, Some(true), prompt, "auto", "codex")
}

fn refusal(report: &FrictionReport) -> String {
    rule_from_history(report, "id".into()).expect_err("no draft")
}

fn question(command: serde_json::Value, prompt: &str, reason: Option<&str>) -> UserInputQuestion {
    serde_json::from_value(json!({
        "id": "q", "header": "Approve command", "question": prompt, "options": ["Yes", "No"],
        "commandApproval": {"command": command, "cwd": "/tmp", "reason": reason},
    })).unwrap()
}

#[test]
fn an_approved_exact_command_drafts_a_narrow_rule_that_saving_enables() {
    let command = "cargo nextest run -p mast-engine --test engine";
    let rule = rule_from_history(&exact(command), "rule-1".into()).unwrap();
    assert_eq!(rule.argv_prefix, shell_words::split(command).unwrap());
    assert_eq!(rule.harness, ApprovalRuleHarness::Codex);
    assert!(rule.enabled, "the draft becomes active only when the user saves it");
    assert_eq!((rule.cwd_scope, rule.command_regex, rule.constraints), (None, None, None));
    assert_eq!(rule.label, format!("Allow {command}"));
    let quoted = rule_from_history(&exact("pgrep 'worker process'"), "rule-2".into()).unwrap();
    assert_eq!(quoted.argv_prefix, ["pgrep", "worker process"]);
}

#[test]
fn a_draft_matches_the_command_it_came_from_and_nothing_wider() {
    let root = tempfile::tempdir().unwrap();
    let command = "cargo nextest run -p mast-engine --test engine";
    let rule = rule_from_history(&exact(command), "rule-1".into()).unwrap();
    let decide = |text: &str| matching_rule_labels(text, ApprovalRuleHarness::Codex, root.path(), &[root.path()], std::slice::from_ref(&rule)).is_some();
    assert!(decide(command));
    assert!(decide(&format!("{command} -- --nocapture")), "a prefix rule covers added arguments");
    assert!(!decide("cargo nextest run -p mast-ui"));
    assert!(!decide("cargo build"));
}

#[test]
fn only_approved_commands_become_rules() {
    let denied = row("cargo test", true, Some(false), "Codex wants to run `cargo test`. Allow it?", "auto", "codex");
    assert!(refusal(&denied).contains("denied"));
    let unanswered = row("cargo test", true, None, "Codex wants to run `cargo test`. Allow it?", "auto", "codex");
    assert!(refusal(&unanswered).contains("No answer was recorded"));
}

#[test]
fn only_engine_recorded_codex_rows_become_rules() {
    for source in ["agent", "import"] {
        let report = row("cargo test", true, Some(true), "Codex wants to run `cargo test`. Allow it?", source, "codex");
        assert!(refusal(&report).contains("recorded itself"), "{source}");
    }
    let claude = row("cargo test", true, Some(true), "run cargo test", "auto", "claude-code");
    assert!(refusal(&claude).contains("Codex only"));
}

#[test]
fn a_redacted_or_cut_command_never_becomes_a_rule() {
    let redacted = exact("cargo test --token [REDACTED]");
    assert!(refusal(&redacted).contains("Secrets were removed"));
    let long = format!("cargo test {}", "a".repeat(1024));
    assert!(refusal(&row(&long[..1024], true, Some(true), "", "auto", "codex")).contains("cut at 1,024"));
}

#[test]
fn rows_from_before_exact_capture_need_a_matching_prompt_and_plain_words() {
    let plain = "cargo test -p mast-engine";
    let rule = rule_from_history(&legacy(plain, &format!("Codex wants to run `{plain}`. Allow it? Reason: sandbox")), "id".into()).unwrap();
    assert_eq!(rule.argv_prefix, ["cargo", "test", "-p", "mast-engine"]);
    // The backtick split cut this command: the prompt shows the rest of it.
    let cut = "cargo test ";
    assert!(refusal(&legacy(cut, "Codex wants to run `cargo test `pwd``. Allow it?")).contains("before Mast kept exact commands"));
    let cut_without_space = "cargo test";
    assert!(refusal(&legacy(cut_without_space, "Codex wants to run `cargo test`pwd``. Allow it?")).contains("before Mast kept exact commands"));
    // Quoting is not recoverable from the old text.
    let quoted = "pgrep 'worker process'";
    assert!(refusal(&legacy(quoted, &format!("Codex wants to run `{quoted}`. Allow it?"))).contains("before Mast kept exact commands"));
    // Another harness's prompt text is not Codex's.
    assert!(refusal(&legacy(plain, "Run cargo test?")).contains("before Mast kept exact commands"));
}

#[test]
fn commands_a_rule_cannot_match_say_why() {
    let reason = |command: &str| refusal(&exact(command));
    assert!(reason("cargo test $(whoami)").contains("cannot match exactly"));
    assert!(reason("cargo test > out.txt").contains("cannot match exactly"));
    assert!(reason("cargo test && cargo build").contains("more than one command"));
    assert!(reason("git push origin main").contains("safety rule"));
    assert!(reason("sudo cargo test").contains("safety rule"));
    assert!(reason("npm test").contains("outside the command families"));
    assert!(reason("cat /Users/me/.claude/settings.json").contains("safety rule"));
}

#[test]
fn the_recorded_command_is_the_structured_command() {
    let prompt = "Codex wants to run `git commit -m a b`. Allow it? Reason: sandbox";
    // An argv list is quoted word by word, so words keep their boundaries.
    let approval = crate::sessions::friction_approval(&question(json!(["pgrep", "worker process"]), prompt, Some("sandbox"))).unwrap();
    assert_eq!(approval.command, "pgrep 'worker process'");
    assert!(approval.exact);
    assert_eq!(approval.why, "sandbox");
    // The backtick that ended the old split is part of the command now.
    let script = "cargo test `pwd` -p mast-engine";
    let approval = crate::sessions::friction_approval(&question(json!(script), &format!("Codex wants to run `{script}`. Allow it?"), None)).unwrap();
    assert_eq!(approval.command, script);
    assert!(approval.exact);
    // A prompt without structured details keeps the old, inexact reading.
    let mut plain = question(json!(null), "Codex wants to run `cargo test`. Allow it?", None);
    plain.command_approval = None;
    let approval = crate::sessions::friction_approval(&plain).unwrap();
    assert_eq!((approval.command.as_str(), approval.exact), ("cargo test", false));
    // An empty argv list has no exact text.
    let empty = crate::sessions::friction_approval(&question(json!([]), "Codex wants to run a command. Allow it?", None)).unwrap();
    assert!(!empty.exact);
}

#[test]
fn an_argv_list_round_trips_into_the_same_rule_words() {
    let words = ["pgrep", "-f", "worker process", "it's"];
    let approval = crate::sessions::friction_approval(&question(json!(words), "Codex wants to run `x`. Allow it?", None)).unwrap();
    assert_eq!(shell_words::split(&approval.command).unwrap(), words);
}
