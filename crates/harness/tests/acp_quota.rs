//! ACP quota error frames and the sandboxed-worker environment, against a
//! scripted agent written to a temp dir (no real agent binary). All secrets
//! here are synthetic.

#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // spawn sites here are tests

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use mast_harness::{AcpHarness, AgentToolsInjection, CancellationToken, Harness, RunControls};
use mast_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};

const AGENT: &str = r#"#!/bin/sh
emit() { printf '%s\n' "$1"; }
rid() { printf '%s' "$1" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
has() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
read -r line
emit "{\"id\":$(rid "$line"),\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{}}}"
read -r line
emit "{\"id\":$(rid "$line"),\"result\":{\"sessionId\":\"s-1\"}}"
while read -r line; do
  has "$line" '"method":"session/prompt"' || { emit "{\"id\":$(rid "$line"),\"result\":{}}"; continue; }
  pid=$(rid "$line")
  case "$line" in
  *scenario:weekly*) emit "{\"id\":$pid,\"error\":{\"code\":-32000,\"message\":\"Internal error\",\"data\":\"You've hit your weekly Opus usage limit\"}}" ;;
  *scenario:five*) emit "{\"id\":$pid,\"error\":{\"code\":-32000,\"message\":\"You've hit your 5-hour limit\"}}" ;;
  *scenario:credits*) emit "{\"id\":$pid,\"error\":{\"code\":-32000,\"message\":\"Your workspace is out of credits\"}}" ;;
  *scenario:throttle*) emit "{\"id\":$pid,\"error\":{\"code\":429,\"message\":\"Rate limit reached for requests per minute\"}}" ;;
  *scenario:plain*) emit "{\"id\":$pid,\"error\":{\"code\":-32000,\"message\":\"tool crashed\"}}" ;;
  *scenario:env*)
    path=$(printf '%s' "$line" | sed 's/.*scenario:env \([^"]*\)".*/\1/')
    env > "$path"
    emit "{\"id\":$pid,\"result\":{\"stopReason\":\"end_turn\"}}" ;;
  esac
done
"#;

fn agent(dir: &Path) -> AcpHarness {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-quota-acp.sh");
    std::fs::write(&path, AGENT).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    AcpHarness::grok()
        .with_executable(path)
        .with_graces(Duration::from_millis(100), Duration::from_millis(150))
}

fn request(prompt: &str, sandbox: SandboxLevel) -> RunRequest {
    RunRequest {
        scheduled_task: None,
        scheduled_notion_writes: None,
        approval_rules: false,
        prompt: prompt.into(),
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

async fn collect(
    stream: futures::stream::BoxStream<'static, Result<AgentEvent, mast_harness::HarnessError>>,
) -> Vec<AgentEvent> {
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

fn limit_codes(events: &[AgentEvent]) -> Vec<(Option<String>, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Error { code, message, .. } => Some((code.clone(), message.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn acp_prompt_errors_carry_the_limit_code_before_the_done() {
    let dir = tempfile::tempdir().unwrap();
    for (scenario, code) in [
        ("weekly", "usage_weekly_model"),
        ("five", "usage_5h"),
        ("credits", "usage_limit"),
        ("throttle", "rate_limit"),
    ] {
        let events = collect(
            agent(dir.path())
                .run(
                    request(
                        &format!("scenario:{scenario}"),
                        SandboxLevel::WorkspaceWrite,
                    ),
                    controls(),
                )
                .await
                .unwrap(),
        )
        .await;
        let errors = limit_codes(&events);
        assert_eq!(errors.len(), 1, "{scenario}: {errors:?}");
        assert_eq!(errors[0].0.as_deref(), Some(code), "{scenario}");
        let last = events.last().unwrap();
        assert!(
            matches!(
                last,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    error: Some(_),
                    ..
                }
            ),
            "{scenario}: the Done follows the limit error, got {last:?}"
        );
        let text = errors[0].1.to_ascii_lowercase();
        if code == "rate_limit" {
            assert!(
                !text.contains("rate limit") && !text.contains("429"),
                "{text}"
            );
        } else {
            assert!(
                text.contains("usage limit"),
                "the quota guard must still read it: {text}"
            );
        }
    }
}

#[tokio::test]
async fn an_ordinary_acp_failure_gets_no_limit_code() {
    let dir = tempfile::tempdir().unwrap();
    let events = collect(
        agent(dir.path())
            .run(
                request("scenario:plain", SandboxLevel::WorkspaceWrite),
                controls(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(limit_codes(&events).is_empty());
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::Done {
            status: DoneStatus::Errored,
            ..
        }
    ));
}

fn tools() -> AgentToolsInjection {
    AgentToolsInjection {
        server_name: "mast-agent".into(),
        command: PathBuf::from("/bin/true"),
        args: vec!["mcp".into()],
        env: [
            (
                "MAST_AGENT_TOKEN".to_owned(),
                "zat_scoped_synthetic".to_owned(),
            ),
            (
                mast_harness::account_home::WORKER_SCRUB_ENV.to_owned(),
                "1".to_owned(),
            ),
        ]
        .into(),
    }
}

async fn child_env(dir: &Path, sandbox: SandboxLevel, with_tools: bool) -> String {
    let dump = dir.join(format!("env-{sandbox:?}-{with_tools}.txt"));
    let prompt = format!("scenario:env {}", dump.display());
    let harness = agent(dir);
    let stream = if with_tools {
        harness
            .run_with_agent_tools(request(&prompt, sandbox), controls(), tools())
            .await
    } else {
        harness.run(request(&prompt, sandbox), controls()).await
    }
    .unwrap();
    let events = collect(stream).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
    std::fs::read_to_string(dump).unwrap()
}

#[tokio::test]
async fn a_sandboxed_acp_worker_child_sees_no_foreign_credentials_or_homes() {
    // The only test in this binary that touches the process environment, and it
    // does so before any child starts.
    // SAFETY: no other thread reads or writes these names concurrently.
    unsafe {
        std::env::set_var("CODEX_HOME", "/tmp/w4t1-other-account-home");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/w4t1-other-claude-home");
        std::env::set_var("GEMINI_CLI_HOME", "/tmp/w4t1-other-gemini-home");
        std::env::set_var("W4T1_SYNTHETIC_PROVIDER_KEY", "synthetic-key-value");
        std::env::set_var("W4T1_REFRESH_TOKEN", "synthetic-refresh-value");
        std::env::set_var("MAST_AGENT_TOKEN", "zat_inherited_other_scope");
    }
    let dir = tempfile::tempdir().unwrap();

    let worker = child_env(dir.path(), SandboxLevel::WorkspaceWrite, true).await;
    for forbidden in [
        "w4t1-other-account-home",
        "w4t1-other-claude-home",
        "w4t1-other-gemini-home",
        "synthetic-key-value",
        "synthetic-refresh-value",
        "zat_inherited_other_scope",
    ] {
        assert!(
            !worker.contains(forbidden),
            "{forbidden} reached the worker child:\n{worker}"
        );
    }
    assert!(
        worker.lines().any(|l| l.starts_with("PATH=")),
        "the child keeps its PATH"
    );
    assert!(
        worker.lines().any(|l| l.starts_with("HOME=")),
        "the child keeps HOME"
    );

    // Controls: outside WorkspaceWrite, or (see worker_env_scrub) without the worker flag, the environment is untouched.
    for (sandbox, with_tools) in [
        (SandboxLevel::WorkspaceWrite, false),
        (SandboxLevel::DangerFullAccess, true),
    ] {
        let kept = child_env(dir.path(), sandbox, with_tools).await;
        assert!(
            kept.contains("W4T1_SYNTHETIC_PROVIDER_KEY=synthetic-key-value"),
            "{sandbox:?}/{with_tools}"
        );
    }
}
