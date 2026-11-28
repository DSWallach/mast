//! P12: every harness spawn path rebuilds a sandboxed worker's child
//! environment from the allowlist, and nothing else does. A fake executable
//! dumps the environment the harness actually gave it. All values are
//! synthetic; none is a real credential.

#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // spawn sites here are tests

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use mast_harness::account_home::WORKER_SCRUB_ENV;
use mast_harness::{
    AcpHarness, AgentToolsInjection, CancellationToken, ClaudeHarness, CodexHarness, CursorHarness,
    Harness, OpencodeHarness, RunControls,
};
use mast_proto::{RunRequest, SandboxLevel};

/// Dumps its environment next to itself (atomically), then exits.
const DUMP: &str = "#!/bin/sh\nd=\"$(dirname \"$0\")\"\nenv > \"$d/env.tmp\" && mv \"$d/env.tmp\" \"$d/env.txt\"\nexit 0\n";

/// Inherited by the test process, so by every harness before it spawns.
const AMBIENT: &[(&str, &str)] = &[
    ("CLAUDE_CONFIG_DIR", "/tmp/w4t1-claude-home"),
    ("CODEX_HOME", "/tmp/w4t1-codex-home"),
    ("GEMINI_CLI_HOME", "/tmp/w4t1-gemini-home"),
    ("ANTHROPIC_API_KEY", "synthetic-anthropic-key"),
    ("OPENAI_API_KEY", "synthetic-openai-key"),
    ("GH_TOKEN", "synthetic-gh-token"),
    ("W4T1_REFRESH_TOKEN", "synthetic-refresh-token"),
    ("W4T1_UNLISTED_VENDOR_VAR", "synthetic-unlisted"),
    ("MAST_AGENT_TOKEN", "zat_inherited_from_another_scope"),
];

type Build = fn(PathBuf) -> Box<dyn Harness>;

/// (name, harness builder, ambient names this harness keeps for its Default
/// account when scrubbed: its own home var and its own provider key).
fn table() -> Vec<(&'static str, Build, &'static [&'static str])> {
    vec![
        (
            "claude",
            |exe| Box::new(ClaudeHarness::new().with_executable(exe)),
            &["CLAUDE_CONFIG_DIR", "ANTHROPIC_API_KEY"],
        ),
        (
            "codex",
            |exe| Box::new(CodexHarness::new().with_executable(exe)),
            &["CODEX_HOME", "OPENAI_API_KEY"],
        ),
        (
            "acp",
            |exe| Box::new(AcpHarness::grok().with_executable(exe)),
            &[],
        ),
        (
            "opencode",
            |exe| Box::new(OpencodeHarness::new().with_executable(exe)),
            &[],
        ),
        (
            "cursor",
            |exe| Box::new(CursorHarness::new().with_executable(exe)),
            &[],
        ),
    ]
}

fn request(sandbox: SandboxLevel) -> RunRequest {
    RunRequest {
        scheduled_task: None,
        scheduled_notion_writes: None,
        approval_rules: false,
        prompt: "hi".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        fork_from: None,
        instructions: None,
    }
}

fn controls() -> RunControls {
    let (_steer_tx, steer_rx) = mpsc::channel(8);
    RunControls {
        request_input: Box::new(|_| {
            let (_tx, rx) = oneshot::channel();
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        approval: None,
    }
}

fn tools(flagged: bool) -> AgentToolsInjection {
    let mut env = BTreeMap::from([(
        "MAST_AGENT_TOKEN".to_owned(),
        "zat_scoped_to_this_run".to_owned(),
    )]);
    if flagged {
        env.insert(WORKER_SCRUB_ENV.to_owned(), "1".to_owned());
    }
    AgentToolsInjection {
        server_name: "mast-agent".into(),
        command: PathBuf::from("/bin/true"),
        args: vec!["mcp".into()],
        env,
    }
}

/// Start a run against a fake that dumps its environment; return that dump.
async fn child_env(
    harness_name: &str,
    build: Build,
    sandbox: SandboxLevel,
    flagged: bool,
) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("fake-agent.sh");
    std::fs::write(&exe, DUMP).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = build(exe);
    // The run may fail or hang after the spawn (the fake speaks no protocol);
    // only the spawn matters, so abort it once the dump exists.
    let run = tokio::spawn(async move {
        // Hold the stream open: dropping it kills the child before it can dump.
        let stream = harness
            .run_with_agent_tools(request(sandbox), controls(), tools(flagged))
            .await
            .map_err(|e| e.to_string())?;
        let _held = stream;
        futures::future::pending::<()>().await;
        Ok::<(), String>(())
    });
    let dump = dir.path().join("env.txt");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut run = Some(run);
    while !Path::new(&dump).exists() {
        // The task only ends early when the run failed before spawning.
        if run.as_ref().is_some_and(|r| r.is_finished()) {
            let outcome = run.take().unwrap().await;
            if !matches!(outcome, Ok(Ok(()))) {
                panic!(
                    "{harness_name} ({sandbox:?}, flagged={flagged}) never spawned the fake agent: {outcome:?}"
                );
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{harness_name} ({sandbox:?}, flagged={flagged}) spawned but its fake never dumped the environment"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if let Some(run) = run {
        run.abort();
    }
    std::fs::read_to_string(dump).unwrap()
}

fn names(dump: &str) -> Vec<&str> {
    dump.lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect()
}

#[tokio::test]
async fn every_harness_spawn_path_scrubs_a_flagged_worker_and_only_that() {
    // The only test in this binary; the environment is set before any child starts.
    // SAFETY: no other thread reads or writes the environment concurrently.
    unsafe {
        for (name, value) in AMBIENT {
            std::env::set_var(name, value);
        }
    }
    for (harness, build, keeps) in table() {
        // A flagged WorkspaceWrite worker gets only the allowlist.
        let scrubbed = child_env(harness, build, SandboxLevel::WorkspaceWrite, true).await;
        let got = names(&scrubbed);
        for (name, _) in AMBIENT {
            let expected = keeps.contains(name);
            assert_eq!(
                got.contains(name),
                expected,
                "{harness}: {name} should be {} for a flagged worker\n{scrubbed}",
                if expected {
                    "kept (its own Default login)"
                } else {
                    "dropped"
                }
            );
        }
        for synthetic in [
            "synthetic-gh-token",
            "synthetic-refresh-token",
            "zat_inherited_from_another_scope",
        ] {
            assert!(
                !scrubbed.contains(synthetic),
                "{harness}: {synthetic} leaked"
            );
        }
        assert!(
            got.contains(&"PATH") && got.contains(&"HOME"),
            "{harness}: lost its basics"
        );

        // An unflagged launch keeps its full environment, token or not.
        let unflagged = child_env(harness, build, SandboxLevel::WorkspaceWrite, false).await;
        for (name, value) in AMBIENT {
            assert!(
                unflagged.lines().any(|l| l == format!("{name}={value}")),
                "{harness}: unflagged launch lost {name}"
            );
        }
        // A flag outside WorkspaceWrite changes nothing.
        let full = child_env(harness, build, SandboxLevel::DangerFullAccess, true).await;
        assert!(
            full.contains("W4T1_UNLISTED_VENDOR_VAR=synthetic-unlisted"),
            "{harness}: full-access run was scrubbed"
        );
    }

    // The escape hatch is opt-in: everything above ran with it unset. Naming a
    // variable passes exactly that variable; the scoped token and refresh
    // tokens stay out even when named (a run's own token rides its MCP config,
    // not the child's environment).
    // SAFETY: as above.
    unsafe {
        std::env::set_var(
            "MAST_WORKER_ENV_PASSTHROUGH",
            "W4T1_UNLISTED_VENDOR_VAR, MAST_AGENT_TOKEN W4T1_REFRESH_TOKEN",
        );
    }
    for (harness, build, _) in table() {
        let dump = child_env(harness, build, SandboxLevel::WorkspaceWrite, true).await;
        assert!(
            dump.lines()
                .any(|l| l == "W4T1_UNLISTED_VENDOR_VAR=synthetic-unlisted"),
            "{harness}: a named variable must pass\n{dump}"
        );
        for still_gone in [
            "synthetic-gh-token",
            "synthetic-refresh-token",
            "zat_inherited_from_another_scope",
        ] {
            assert!(
                !dump.contains(still_gone),
                "{harness}: {still_gone} leaked through the passthrough"
            );
        }
    }
}
