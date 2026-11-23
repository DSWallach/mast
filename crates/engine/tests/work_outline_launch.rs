//! Work Outline launch (Milestone 3 stage 2) against a real offline engine
//! with the instruction-aware mock harness and temp git repositories:
//! - `CreateChatForWorkItem` creates one Chat, assignment, binding, revision,
//!   application, command, and Run, and the queued pin is the delivered one
//!   (acceptance 10, 16);
//! - a crash after EACH workflow step, then a retry, still yields exactly one
//!   of each (acceptance 16);
//! - a bound Chat with a pruned worktree, a moved root, a re-cloned repository,
//!   or a resume from another folder blocks launch AND resume with an
//!   actionable error and never falls back (acceptance 14);
//! - child Chats inherit assignment, binding, and revision (acceptance 19);
//!   harness-native subagents never become Chats (acceptance 20);
//! - bound Chats cannot be retargeted (SetChatCwd, CreateChatWorktree, a Run
//!   carrying another cwd) and skip branch auto-rename; unbound Chats keep
//!   today's behavior.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use mast_doc::{
    KIND_INSTRUCTION_REVISION_APPLICATIONS, KIND_INSTRUCTION_REVISIONS,
    KIND_WORK_ITEM_CHAT_ASSIGNMENTS, MessageRole, SessionCommandEntry, SessionCommandPayload,
    SessionCommandStatus,
};
use mast_engine::execution_scope::ExecutionScopeService;
use mast_engine::orchestrator_wakes::WAKE_WINDOW_MS;
use mast_engine::work_outline_launch::{LaunchStep, WorkItemLaunchService};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::mock::InstructionMockHarness;
use mast_proto::{
    CreateChatForWorkItemParams, CreateChatForWorkItemReply, ExecutionRepositoryIdentity,
    HarnessId, RunRequest, SandboxLevel, WorkItemLaunchState, WorkItemPrompt, WorkOutlineError,
    WorkOutlineErrorCode, WorktreeChoice, derive_id, roles,
};
use mast_rpc::{RpcClient, methods};
use serde_json::{Value, json};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-1";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let output = mast_harness::git_command()
        .args([
            "-c",
            "user.name=Zeron Test",
            "-c",
            "user.email=test@zeron.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn init_repo(path: &Path, file: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", file]);
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    mock: InstructionMockHarness,
    client: RpcClient,
    root: PathBuf,
    repo: PathBuf,
    item: String,
}

fn outline_error(text: String) -> WorkOutlineError {
    text.find("work-outline/")
        .and_then(|at| WorkOutlineError::from_wire_message(&text[at..]))
        .unwrap_or_else(|| panic!("not a work outline error: {text}"))
}

async fn rig() -> Rig {
    rig_with(InstructionMockHarness::default(), true).await
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_worktree_is_opt_in_and_avoids_branch_collisions() {
    let rig = rig().await;
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":"parent",
        "spaceId":SPACE, "deviceId":DEVICE})).await.unwrap();
    let mut off = params(&rig, "worker-off", "off");
    off.parent_chat_id = Some("parent".into());
    let reply = launch(&rig, &off).await.unwrap();
    assert_eq!(rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"setWorkerAutomation",
        "spaceId":SPACE, "workerWorktrees":true, "operationId":"enable-worker-worktrees"})).await.unwrap();
    let configured = rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"createWorkItem", "spaceId":SPACE,
        "title":"Configured", "operationId":"configured-item"})).await.unwrap();
    let configured_id = configured["workItemId"].as_str().unwrap();
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"configureExecution",
        "scope":{"type":"workItem", "workItemId":configured_id},
        "configuration":{"executionRoot":rig.repo.to_string_lossy(), "bindings":[], "schema":1},
        "operationId":"configure-item"})).await.unwrap();
    let mut explicit = params(&rig, "worker-configured", "configured");
    explicit.work_item_id = configured_id.into();
    explicit.parent_chat_id = Some("parent".into());
    let existing = launch(&rig, &explicit).await.unwrap();
    assert_eq!(rig.core.workspace.chat(&existing.chat_id).unwrap().unwrap().cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    assert!(!rig.repo.join(".worktrees/workspace/configured").exists());
    git(&rig.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&rig.repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
    git(&rig.repo, &["checkout", "-b", "local-work"]);
    git(&rig.repo, &["branch", "-D", "main"]);
    git(&rig.repo, &["branch", "launch-me"]);
    let occupied = rig.repo.join(".worktrees/workspace/launch-me-2");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("keep"), "untouched").unwrap();
    let mut on = params(&rig, "worker-on", "on");
    on.parent_chat_id = Some("parent".into());
    let reply = launch(&rig, &on).await.unwrap();
    let path = rig.repo.join(".worktrees/workspace/launch-me-3");
    assert_eq!(rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().cwd.as_deref(), Some(&*path.to_string_lossy()));
    assert_eq!(git(&path, &["branch", "--show-current"]), "launch-me-3");
    assert_eq!(rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().branch.as_deref(), Some("launch-me-3"));
    assert!(matches!(rig.core.workspace.chat_execution_binding(&reply.chat_id).unwrap().resolved.source_scope,
        mast_proto::work_outline_execution::ExecutionSourceScope::WorkItem { ref work_item_id } if work_item_id == &rig.item));
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), git(&rig.repo, &["rev-parse", "refs/remotes/origin/main"]));
    assert_eq!(std::fs::read_to_string(occupied.join("keep")).unwrap(), "untouched");
    assert_eq!(launch(&rig, &on).await.unwrap().chat_id, reply.chat_id);
    let snapshot = mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE));
    assert!(snapshot.execution_configurations.iter().any(|config| config.work_item_id.as_deref() == Some(&rig.item)
        && config.configuration.as_ref().is_some_and(|config| config.execution_root == path.to_string_lossy())));
    // Once an item has a setup, a later worker reuses it rather than creating another worktree.
    let mut again = params(&rig, "worker-again", "again");
    again.parent_chat_id = Some("parent".into());
    let reply = launch(&rig, &again).await.unwrap();
    assert_eq!(rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().cwd.as_deref(), Some(&*path.to_string_lossy()));
    assert!(!rig.repo.join(".worktrees/workspace/launch-me-4").exists());
    rig.core.shutdown().await;
}

/// A rig whose mock declares `mock`'s support; `instructions` adds one
/// Space-root instruction (so every item has effective instructions).
async fn rig_with(mock: InstructionMockHarness, instructions: bool) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    init_repo(&repo, "README");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core assembles");
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": DEVICE,
                    "path": repo.to_string_lossy() }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Launch me",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    if instructions {
        add_space_instruction(&client, "op-instruction").await;
    }
    Rig {
        _tmp: tmp,
        core,
        mock,
        client,
        root,
        repo,
        item: derive_id("op-item", roles::ITEM),
    }
}

async fn add_space_instruction(client: &RpcClient, op: &str) {
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                    "scope": { "type": "spaceRoot" }, "text": "Run the tests first.",
                    "operationId": op }),
        )
        .await
        .unwrap();
}

fn params(rig: &Rig, op: &str, prompt: &str) -> CreateChatForWorkItemParams {
    CreateChatForWorkItemParams {
        work_item_id: rig.item.clone(),
        initial_prompt: WorkItemPrompt {
            text: prompt.into(),
            attachments: Vec::new(),
        },
        config: Some(mast_proto::ChatConfig { approval_rules: false,
            harness: HarnessId::Mock,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
        }),
        execution_override: None,
        worktree_choice: None,
        operation_id: op.into(),
        parent_chat_id: None,
        target_device_id: None,
        account_id: None,
        standing_role: None,
    }
}

async fn launch(
    rig: &Rig,
    params: &CreateChatForWorkItemParams,
) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
    rig.client
        .call(
            methods::CREATE_CHAT_FOR_WORK_ITEM,
            serde_json::to_value(params).unwrap(),
        )
        .await
        .map(|value| serde_json::from_value(value).expect("launch reply decodes"))
        .map_err(|e| outline_error(e.to_string()))
}

#[tokio::test]
async fn missing_agent_is_rejected_before_creating_a_chat() {
    let rig = rig_with(InstructionMockHarness::default(), false).await;
    let mut request = params(&rig, "missing-agent", "Hello");
    request.config.as_mut().unwrap().harness = HarnessId::Codex;
    let error = launch(&rig, &request).await.unwrap_err();
    assert_eq!(error.message, "agent not installed: codex");
    assert!(rig.core.workspace.chat(&derive_id("missing-agent", roles::CHAT)).unwrap().is_none());
}

#[tokio::test]
async fn signed_out_claude_launch_reaches_transcript() {
    const ISOLATED: &str = "MAST_TEST_FIRST_RUN_CLAUDE_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let profile = tempfile::Builder::new().prefix("mast-first-run-").tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "work_outline_launch::signed_out_claude_launch_reaches_transcript"])
            .env(ISOLATED, "1")
            .env("HOME", profile.path())
            .env("MAST_DATA_DIR", profile.path().join("data"))
            .env("CLAUDE_CONFIG_DIR", profile.path().join("claude"))
            .env("CODEX_HOME", profile.path().join("codex"))
            .env("TMPDIR", std::env::temp_dir())
            .env("MAST_NO_LOGIN_SHELL", "1")
            .output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"), "child did not run exactly one test: {}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }

    let tmp = tempfile::Builder::new().prefix("mast-first-run-").tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    init_repo(&repo, "README");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    let harness = mast_harness::ClaudeHarness::new().with_executable(fixture).with_account_home(root.join("claude"));
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(harness));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::ClaudeCode, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace", "spaceId":SPACE, "deviceId":DEVICE, "path":repo})).await.unwrap();
    client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"createWorkItem", "spaceId":SPACE, "title":"First launch", "operationId":"first-item"})).await.unwrap();
    let request = CreateChatForWorkItemParams {
        work_item_id: derive_id("first-item", roles::ITEM),
        initial_prompt: WorkItemPrompt { text: "scenario:signed-out".into(), attachments: Vec::new() },
        config: Some(mast_proto::ChatConfig { approval_rules: false, harness: HarnessId::ClaudeCode, model: None, reasoning: None, model_options: Default::default(), sandbox: SandboxLevel::WorkspaceWrite, auto_approve: true }),
        execution_override: None, worktree_choice: None, operation_id: "first-launch".into(), parent_chat_id: None, target_device_id: None, account_id: None, standing_role: None,
    };
    let reply: CreateChatForWorkItemReply = serde_json::from_value(client.call(methods::CREATE_CHAT_FOR_WORK_ITEM, serde_json::to_value(request).unwrap()).await.unwrap()).unwrap();
    wait_for(|| core.doc_host.open(&reply.chat_id).ok().and_then(|handle| handle.doc().read_entries().ok()).is_some_and(|entries| entries.iter().any(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Error { message, .. } if message == "Authentication failed — sign in to Claude again.")))), "Claude authentication error in transcript").await;
    core.shutdown().await;
}

#[tokio::test]
async fn codex_only_launch_spawns_selected_executable() {
    const ISOLATED: &str = "MAST_TEST_FIRST_RUN_CODEX_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let profile = tempfile::Builder::new().prefix("mast-first-run-").tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "work_outline_launch::codex_only_launch_spawns_selected_executable"])
            .env(ISOLATED, "1")
            .env("HOME", profile.path())
            .env("MAST_DATA_DIR", profile.path().join("data"))
            .env("CLAUDE_CONFIG_DIR", profile.path().join("claude"))
            .env("CODEX_HOME", profile.path().join("codex"))
            .env("TMPDIR", std::env::temp_dir())
            .env("MAST_NO_LOGIN_SHELL", "1")
            .output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"), "child did not run exactly one test: {}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let tmp = tempfile::Builder::new().prefix("mast-first-run-").tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    init_repo(&repo, "README");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh");
    let harness = mast_harness::CodexHarness::new().with_executable(fixture).with_account_home(root.join("codex"));
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(harness));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Codex, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace", "spaceId":SPACE, "deviceId":DEVICE, "path":repo})).await.unwrap();
    client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"createWorkItem", "spaceId":SPACE, "title":"First launch", "operationId":"first-item"})).await.unwrap();
    let request = CreateChatForWorkItemParams {
        work_item_id: derive_id("first-item", roles::ITEM),
        initial_prompt: WorkItemPrompt { text: "scenario:publication".into(), attachments: Vec::new() },
        config: Some(mast_proto::ChatConfig { approval_rules: false, harness: HarnessId::Codex, model: None, reasoning: None, model_options: Default::default(), sandbox: SandboxLevel::WorkspaceWrite, auto_approve: true }),
        execution_override: None, worktree_choice: None, operation_id: "first-launch".into(), parent_chat_id: None, target_device_id: None, account_id: None, standing_role: None,
    };
    let reply: CreateChatForWorkItemReply = serde_json::from_value(client.call(methods::CREATE_CHAT_FOR_WORK_ITEM, serde_json::to_value(request).unwrap()).await.unwrap()).unwrap();
    wait_for(|| core.doc_host.open(&reply.chat_id).ok().and_then(|handle| handle.doc().read_entries().ok()).is_some_and(|entries| entries.iter().any(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("Publication turn completed"))))), "fake Codex response in transcript").await;
    core.shutdown().await;
}

async fn continue_chat(rig: &Rig, chat: &str, op: &str, prompt: &str) -> Result<Value, String> {
    rig.client
        .call(
            methods::CONTINUE_CHAT_FOR_WORK_ITEM,
            json!({ "chatId": chat, "prompt": { "text": prompt }, "operationId": op }),
        )
        .await
        .map_err(|e| e.to_string())
}

async fn describe(rig: &Rig, path: &Path) -> ExecutionRepositoryIdentity {
    let value = rig
        .client
        .call(
            methods::DESCRIBE_EXECUTION_REPOSITORY,
            json!({ "path": path.to_string_lossy() }),
        )
        .await
        .expect("describe");
    serde_json::from_value(value).unwrap()
}

fn service(rig: &Rig) -> WorkItemLaunchService {
    WorkItemLaunchService::new(
        rig.core.workspace.clone(),
        rig.core.doc_host.clone(),
        rig.core.sessions.clone(),
        ExecutionScopeService::new(rig.core.workspace.clone(), rig.core.repos.clone()),
    )
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Turns whose prompt starts with `prefix` (auto-titling runs use other
/// prompts on the same mock).
fn turns_with(rig: &Rig, prefix: &str) -> Vec<mast_harness::mock::RecordedInstructionTurn> {
    rig.mock
        .turns()
        .into_iter()
        .filter(|t| t.prompt.starts_with(prefix))
        .collect()
}

fn run_cwd(rig: &Rig, turn: &mast_harness::mock::RecordedInstructionTurn) -> String {
    rig.mock.runs()[turn.run_index].cwd.clone()
}

async fn settle(rig: &Rig, chat: &str) {
    wait_for(
        || !rig.core.sessions.turn_in_flight(chat),
        "the turn to settle",
    )
    .await;
}

fn commands(rig: &Rig, chat: &str) -> Vec<SessionCommandEntry> {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap()
}

fn user_messages(rig: &Rig, chat: &str) -> usize {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .iter()
        .filter(|e| e.role == MessageRole::User)
        .count()
}

async fn wait_command(rig: &Rig, chat: &str, id: &str) -> SessionCommandEntry {
    wait_for(
        || {
            commands(rig, chat)
                .iter()
                .any(|c| c.id == id && c.status != SessionCommandStatus::Pending)
        },
        "the command to resolve",
    )
    .await;
    commands(rig, chat)
        .into_iter()
        .find(|c| c.id == id)
        .unwrap()
}

fn row_field(rig: &Rig, kind: &str, id: &str, field: &str) -> Option<Value> {
    rig.core
        .workspace
        .registry_row(kind, id)
        .filter(|row| !row.deleted)
        .and_then(|row| row.fields.get(field).cloned())
}

fn applications_of(rig: &Rig, chat: &str) -> Vec<mast_doc::RegistryRow> {
    rig.core
        .workspace
        .work_outline_inputs(SPACE)
        .applications
        .into_iter()
        .filter(|row| row.fields.get("chatId").and_then(Value::as_str) == Some(chat))
        .collect()
}

fn run_request(prompt: &str, cwd: &str) -> RunRequest {
    RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: false,
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        fork_from: None,
        instructions: None,
    }
}

/// Queue a plain Run the way any device (or the Mast MCP `send`) does.
fn queue_run(rig: &Rig, chat: &str, prompt: &str, cwd: &str) -> String {
    rig.core
        .doc_host
        .queue_command(
            chat,
            SessionCommandPayload::Run {
                request: run_request(prompt, cwd),
                message_id: format!("msg-{prompt}"),
            },
        )
        .expect("queue run")
}

fn assert_rejected_with(entry: &SessionCommandEntry, code: &str) {
    assert_eq!(entry.status, SessionCommandStatus::Rejected, "{entry:?}");
    let resolution = entry.resolution.clone().unwrap_or_default();
    assert!(
        resolution.contains(&format!("execution-scope:{code}")),
        "expected {code}: {resolution}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 10 + 16: one of everything, pinned revision delivered
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn launch_creates_each_record_once_and_delivers_the_pinned_revision() {
    let rig = rig().await;
    let p = params(&rig, "op-launch", "launch one");
    let reply = launch(&rig, &p).await.expect("launch");
    assert_eq!(reply.chat_id, derive_id("op-launch", roles::CHAT));
    assert_eq!(reply.message_id, derive_id("op-launch", roles::MESSAGE));
    assert_eq!(reply.command_id, derive_id("op-launch", roles::COMMAND));
    assert_eq!(
        reply.revision_id.as_deref(),
        Some(&*derive_id("op-launch", roles::REVISION))
    );
    assert_eq!(
        reply.application_id.as_deref(),
        Some(&*derive_id("op-launch", roles::APPLICATION))
    );
    assert_eq!(reply.execution_root, rig.repo.to_string_lossy());
    let chat = reply.chat_id.clone();

    wait_for(
        || turns_with(&rig, "launch one").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    let turn = &turns_with(&rig, "launch one")[0];
    // Acceptance 10: the revision captured at queue time is the delivered one.
    assert_eq!(turn.revision_id.as_deref(), reply.revision_id.as_deref());
    assert_eq!(run_cwd(&rig, turn), rig.repo.to_string_lossy());

    // Records.
    let row = rig.core.workspace.chat(&chat).unwrap().expect("chat row");
    assert_eq!(row.space_id.as_deref(), Some(SPACE));
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    assert_eq!(
        row_field(&rig, KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &chat, "workItemId"),
        Some(json!(rig.item))
    );
    let binding = rig
        .core
        .workspace
        .chat_execution_binding(&chat)
        .expect("binding");
    assert_eq!(binding.operation_id.as_deref(), Some("op-launch"));
    assert_eq!(
        binding.resolved.configuration.execution_root,
        rig.repo.to_string_lossy()
    );
    assert!(
        rig.core
            .workspace
            .registry_row(
                KIND_INSTRUCTION_REVISIONS,
                reply.revision_id.as_deref().unwrap()
            )
            .is_some()
    );
    let applications = applications_of(&rig, &chat);
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].fields["reason"], json!("launch"));
    assert_eq!(
        applications[0].fields["turnBoundary"],
        json!({ "messageId": reply.message_id })
    );
    let cmds = commands(&rig, &chat);
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].id, reply.command_id);
    assert!(matches!(
        &cmds[0].payload,
        SessionCommandPayload::RunWithInstructions { instructions, .. }
            if Some(&instructions.revision_id) == reply.revision_id.as_ref()
    ));
    assert_eq!(user_messages(&rig, &chat), 1);

    // Replay: same reply, nothing new.
    let again = launch(&rig, &p).await.expect("replay");
    assert_eq!(again.chat_id, reply.chat_id);
    assert_eq!(again.revision_id, reply.revision_id);
    assert_eq!(again.state, WorkItemLaunchState::Started);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(turns_with(&rig, "launch one").len(), 1);
    assert_eq!(commands(&rig, &chat).len(), 1);
    assert_eq!(applications_of(&rig, &chat).len(), 1);

    // Same operation id, other inputs.
    let other = launch(&rig, &params(&rig, "op-launch", "launch other"))
        .await
        .expect_err("conflict");
    assert_eq!(other.code, WorkOutlineErrorCode::OperationConflict);

    // Continue: derived ids, retry-safe, pinned to the selected revision.
    let first = continue_chat(&rig, &chat, "op-continue", "launch next")
        .await
        .expect("continue");
    assert_eq!(
        first["messageId"],
        json!(derive_id("op-continue", roles::MESSAGE))
    );
    wait_for(
        || turns_with(&rig, "launch next").len() == 1,
        "the second turn",
    )
    .await;
    settle(&rig, &chat).await;
    let replay = continue_chat(&rig, &chat, "op-continue", "launch next")
        .await
        .expect("continue replay");
    assert_eq!(replay["commandId"], first["commandId"]);
    let conflict = continue_chat(&rig, &chat, "op-continue", "launch different")
        .await
        .expect_err("continue conflict");
    assert!(conflict.contains("operationConflict"), "{conflict}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let next = turns_with(&rig, "launch next");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].revision_id.as_deref(), reply.revision_id.as_deref());
    assert_eq!(commands(&rig, &chat).len(), 2);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_after_each_step_then_retry_yields_exactly_one_of_each() {
    let rig = rig().await;
    let launcher = service(&rig);
    let steps = [
        LaunchStep::Validated,
        LaunchStep::ChatCreated,
        LaunchStep::Assigned,
        LaunchStep::BindingCaptured,
        LaunchStep::RevisionCreated,
        LaunchStep::RevisionApplied,
        LaunchStep::TurnQueued,
    ];
    for (index, step) in steps.iter().enumerate() {
        let op = format!("op-crash-{index}");
        let prompt = format!("launch crash {index}");
        let p = params(&rig, &op, &prompt);
        launcher.inject_fault(Some(*step));
        let failed = launcher.create_chat_for_work_item(&p).await;
        assert!(failed.is_err(), "{step:?} must fail");
        let reply = launcher
            .create_chat_for_work_item(&p)
            .await
            .unwrap_or_else(|e| panic!("retry after {step:?}: {e}"));
        let chat = reply.chat_id.clone();
        wait_for(|| turns_with(&rig, &prompt).len() == 1, "the launched turn").await;
        settle(&rig, &chat).await;
        // A third call (another retry) changes nothing either.
        launcher.create_chat_for_work_item(&p).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(turns_with(&rig, &prompt).len(), 1, "one Run after {step:?}");
        assert_eq!(commands(&rig, &chat).len(), 1, "one command after {step:?}");
        assert_eq!(user_messages(&rig, &chat), 1, "one message after {step:?}");
        assert_eq!(applications_of(&rig, &chat).len(), 1);
        let inputs = rig.core.workspace.work_outline_inputs(SPACE);
        assert_eq!(
            inputs.chats.iter().filter(|c| c.id == chat).count(),
            1,
            "one chat after {step:?}"
        );
        assert_eq!(
            inputs
                .chat_execution_bindings
                .iter()
                .filter(|row| row.id == chat)
                .count(),
            1
        );
        assert_eq!(
            row_field(&rig, KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &chat, "workItemId"),
            Some(json!(rig.item))
        );
        assert_eq!(
            turns_with(&rig, &prompt)[0].revision_id.as_deref(),
            reply.revision_id.as_deref()
        );
    }
    // Totals: one per launch.
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    assert_eq!(inputs.applications.len(), steps.len());
    assert_eq!(inputs.chat_execution_bindings.len(), steps.len());
    assert_eq!(
        inputs.assignments.iter().filter(|row| !row.deleted).count(),
        steps.len()
    );
    assert!(
        rig.core
            .workspace
            .registry_row(
                KIND_INSTRUCTION_REVISION_APPLICATIONS,
                &derive_id("op-crash-0", roles::APPLICATION)
            )
            .is_some()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 14: strict validation, no fallback
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pruned_worktree_blocks_launch_and_resume() {
    let rig = rig().await;
    let mut p = params(&rig, "op-prune", "launch prune");
    p.worktree_choice = Some(WorktreeChoice::CreateNew {
        name: "prune me".into(),
    });
    let reply = launch(&rig, &p).await.expect("launch into a new worktree");
    let worktree = rig.repo.join(".worktrees/workspace/prune-me");
    assert_eq!(reply.execution_root, worktree.to_string_lossy());
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch prune").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "launch prune")[0]),
        worktree.to_string_lossy()
    );
    // A retry reuses the worktree this operation created.
    launch(&rig, &p).await.expect("retry reuses the worktree");
    let identity = describe(&rig, &worktree).await;

    std::fs::remove_dir_all(&worktree).unwrap();

    // Resume through the work-item path: blocked up front.
    let error = continue_chat(&rig, &chat, "op-prune-next", "launch prune next")
        .await
        .expect_err("pruned worktree blocks continue");
    assert!(error.contains("execution-scope:worktreePruned"), "{error}");
    // Resume through a plain Run (any device, MCP send): rejected at drain,
    // no user message, no Run, no fallback.
    let id = queue_run(&rig, &chat, "launch prune raw", &rig.repo.to_string_lossy());
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "worktreePruned");
    assert_eq!(user_messages(&rig, &chat), 1);
    assert!(turns_with(&rig, "launch prune raw").is_empty());

    // A new launch selecting the pruned worktree is blocked before any write.
    let mut q = params(&rig, "op-prune-2", "launch prune two");
    q.worktree_choice = Some(WorktreeChoice::UseExisting {
        worktree_id: identity.checkout_id.clone(),
        worktree_path: worktree.to_string_lossy().into_owned(),
    });
    let error = launch(&rig, &q).await.expect_err("blocked");
    assert!(
        error.message.contains("execution-scope:worktreePruned"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-prune-2", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moved_root_blocks_resume_and_never_falls_back() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-move", "launch move"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch move").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    std::fs::rename(&rig.repo, rig.root.join("repo-moved")).unwrap();

    let error = continue_chat(&rig, &chat, "op-move-next", "launch move next")
        .await
        .expect_err("moved root blocks continue");
    assert!(error.contains("execution-scope:rootMissing"), "{error}");
    let id = queue_run(&rig, &chat, "launch move raw", "/tmp");
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "rootMissing");
    assert!(turns_with(&rig, "launch move raw").is_empty());
    // The Chat is never retargeted.
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));

    // A fresh launch is blocked too, before any Chat exists.
    let error = launch(&rig, &params(&rig, "op-move-2", "launch move two"))
        .await
        .expect_err("blocked");
    assert!(
        error.message.contains("execution-scope:rootMissing"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-move-2", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recloned_repository_fingerprint_mismatch_blocks_launch_and_resume() {
    let rig = rig().await;
    let other = rig.root.join("other");
    init_repo(&other, "first");
    let identity = describe(&rig, &other).await;
    let mut p = params(&rig, "op-clone", "launch clone");
    p.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": {
                "executionRoot": other.to_string_lossy(),
                "bindings": [{
                    "repoId": identity.repo_id, "repoFingerprint": identity.repo_fingerprint,
                    "repoPath": identity.repo_path, "member": true, "primary": true,
                }],
                "schema": 1,
            }
        }))
        .unwrap(),
    );
    let reply = launch(&rig, &p).await.expect("launch with override");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch clone").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;

    std::fs::remove_dir_all(&other).unwrap();
    init_repo(&other, "second-history");

    let error = continue_chat(&rig, &chat, "op-clone-next", "launch clone next")
        .await
        .expect_err("re-cloned repository blocks continue");
    assert!(
        error.contains("execution-scope:repositoryIdentityMismatch"),
        "{error}"
    );
    let id = queue_run(&rig, &chat, "launch clone raw", &other.to_string_lossy());
    assert_rejected_with(
        &wait_command(&rig, &chat, &id).await,
        "repositoryIdentityMismatch",
    );
    assert!(turns_with(&rig, "launch clone raw").is_empty());

    let mut q = p.clone();
    q.operation_id = "op-clone-2".into();
    let error = launch(&rig, &q).await.expect_err("blocked");
    assert!(
        error
            .message
            .contains("execution-scope:repositoryIdentityMismatch"),
        "{error:?}"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bound_resume_from_another_folder_is_an_error_not_a_fresh_session() {
    let rig = rig().await;
    let launcher = service(&rig);
    let p = params(&rig, "op-resume", "launch resume");
    // Everything but the Turn exists.
    launcher.inject_fault(Some(LaunchStep::RevisionApplied));
    assert!(launcher.create_chat_for_work_item(&p).await.is_err());
    let chat = derive_id("op-resume", roles::CHAT);
    // The Chat's stored native session ran somewhere else.
    rig.core
        .workspace
        .set_chat_harness_session(&chat, "native-elsewhere", "/somewhere/else");
    let reply = launcher
        .create_chat_for_work_item(&p)
        .await
        .expect("queued");
    let entry = wait_command(&rig, &chat, &reply.command_id).await;
    assert_rejected_with(&entry, "resumeRootMismatch");
    assert!(turns_with(&rig, "launch resume").is_empty());
    assert_eq!(user_messages(&rig, &chat), 0);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_owner_blocks_a_fresh_launch() {
    let rig = rig().await;
    let remote_repo = rig.root.join("remote");
    init_repo(&remote_repo, "r");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-remote", "deviceId": "dev-b",
                    "path": remote_repo.to_string_lossy() }),
        )
        .await
        .unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": "space-remote", "title": "Remote",
                    "operationId": "op-remote-item" }),
        )
        .await
        .unwrap();
    let mut p = params(&rig, "op-remote", "launch remote");
    p.work_item_id = derive_id("op-remote-item", roles::ITEM);
    let error = launch(&rig, &p).await.expect_err("offline owner");
    assert!(
        error.message.contains("execution-scope:deviceOffline"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-remote", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Retargeting, branch rename, unbound behavior
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bound_chats_cannot_be_retargeted() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-pin", "launch pin"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch pin").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");

    let error = rig
        .client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": chat, "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .expect_err("SetChatCwd refuses");
    assert!(
        error
            .to_string()
            .contains("execution-scope:boundChatRetarget"),
        "{error}"
    );
    // Setting the root itself is harmless.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": chat, "cwd": rig.repo.to_string_lossy() }),
        )
        .await
        .expect("same root is allowed");
    let error = rig
        .client
        .call(
            methods::CREATE_CHAT_WORKTREE,
            json!({ "chatId": chat, "repoPath": rig.repo.to_string_lossy(), "name": "nope" }),
        )
        .await
        .expect_err("CreateChatWorktree refuses");
    assert!(
        error
            .to_string()
            .contains("execution-scope:boundChatRetarget"),
        "{error}"
    );
    assert!(!rig.repo.join(".worktrees/workspace/nope").exists());
    // A createChat replay with another cwd never moves an existing Chat.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE, "deviceId": DEVICE,
                    "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .unwrap();
    // The Mast MCP `send` shape: a Run built from another cwd still runs in
    // the bound root.
    let id = queue_run(&rig, &chat, "launch pin mcp", &elsewhere.to_string_lossy());
    let entry = wait_command(&rig, &chat, &id).await;
    assert_eq!(entry.status, SessionCommandStatus::Applied, "{entry:?}");
    wait_for(
        || turns_with(&rig, "launch pin mcp").len() == 1,
        "the MCP turn",
    )
    .await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "launch pin mcp")[0]),
        rig.repo.to_string_lossy()
    );
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    // A Run carrying a worktree directive is refused.
    let mut request = run_request("launch pin worktree", &rig.repo.to_string_lossy());
    request.worktree = Some(mast_proto::WorktreeSpec {
        repo_path: rig.repo.to_string_lossy().into_owned(),
        base: "main".into(),
        space_id: None,
    });
    let id = rig
        .core
        .doc_host
        .queue_command(
            &chat,
            SessionCommandPayload::Run {
                request,
                message_id: "msg-worktree".into(),
            },
        )
        .unwrap();
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "boundChatRetarget");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_auto_rename_is_skipped_for_bound_chats() {
    let rig = rig().await;
    let repos = mast_engine::repos::Repos::with_worktrees_root(
        &rig.root.join("data"),
        DEVICE,
        rig.repo.join(".worktrees"),
    );
    let worktree = repos.create_worktree(&rig.repo, "main").await.expect("wt");
    assert!(worktree.branch.starts_with("mast/"));
    let wt_path = std::fs::canonicalize(&worktree.path).unwrap();
    let identity = describe(&rig, &wt_path).await;
    let mut p = params(&rig, "op-title", "launch title [hold]");
    p.worktree_choice = Some(WorktreeChoice::UseExisting {
        worktree_id: identity.checkout_id,
        worktree_path: wt_path.to_string_lossy().into_owned(),
    });
    let reply = launch(&rig, &p).await.expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch title").len() == 1,
        "the held turn",
    )
    .await;
    rig.core
        .workspace
        .set_chat_branch(&chat, &worktree.branch)
        .unwrap();
    rig.mock.release();
    wait_for(
        || {
            rig.core
                .workspace
                .chat(&chat)
                .ok()
                .flatten()
                .is_some_and(|c| c.title.as_deref().is_some_and(|t| !t.is_empty()))
        },
        "the auto title",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(
        git(&wt_path, &["branch", "--show-current"]),
        worktree.branch
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_chats_keep_todays_cwd_behavior() {
    let rig = rig().await;
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "plain", "spaceId": SPACE, "deviceId": DEVICE }),
        )
        .await
        .unwrap();
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": "plain", "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .expect("unbound chats retarget freely");
    let id = queue_run(&rig, "plain", "plain run", &elsewhere.to_string_lossy());
    let entry = wait_command(&rig, "plain", &id).await;
    assert_eq!(entry.status, SessionCommandStatus::Applied, "{entry:?}");
    wait_for(
        || turns_with(&rig, "plain run").len() == 1,
        "the plain turn",
    )
    .await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "plain run")[0]),
        elsewhere.to_string_lossy()
    );
    // Removing automatic ownership restores the unassociated boundary.
    assert!(rig.core.workspace.chat_assigned_work_item("plain", SPACE).is_some());
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({
        "op":"removeChatFromWorkItem", "chatId":"plain", "operationId":"remove-plain-item"
    })).await.unwrap();
    let error = continue_chat(&rig, "plain", "op-plain", "plain next")
        .await
        .expect_err("not a work-item chat");
    assert!(error.contains("work-outline/invalid"), "{error}");
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 19 (child Chats) and 20 (subagents)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn child_chats_inherit_assignment_binding_and_revision() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-parent", "launch parent"))
        .await
        .expect("launch");
    let parent = reply.chat_id.clone();
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-1", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent }),
        )
        .await
        .unwrap();
    let child = rig.core.workspace.chat("child-1").unwrap().unwrap();
    assert_eq!(child.parent_chat_id.as_deref(), Some(parent.as_str()));
    assert_eq!(child.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    assert_eq!(
        row_field(
            &rig,
            KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
            "child-1",
            "workItemId"
        ),
        Some(json!(rig.item))
    );
    let binding = rig
        .core
        .workspace
        .chat_execution_binding("child-1")
        .expect("inherited binding");
    assert_eq!(
        binding.resolved,
        rig.core
            .workspace
            .chat_execution_binding(&parent)
            .unwrap()
            .resolved
    );
    let applications = applications_of(&rig, "child-1");
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].fields["reason"], json!("forkInherit"));
    assert_eq!(
        applications[0].fields["revisionId"],
        json!(reply.revision_id)
    );
    assert_eq!(
        rig.core
            .workspace
            .selected_instruction_revision("child-1")
            .map(|pin| pin.revision_id),
        reply.revision_id.clone()
    );
    // A retried create is a no-op.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-1", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent }),
        )
        .await
        .unwrap();
    assert_eq!(applications_of(&rig, "child-1").len(), 1);

    // A child explicitly created in another folder keeps the assignment and
    // revision but not the binding.
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-2", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent,
                    "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .unwrap();
    assert!(
        rig.core
            .workspace
            .chat_execution_binding("child-2")
            .is_none()
    );
    assert_eq!(
        row_field(
            &rig,
            KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
            "child-2",
            "workItemId"
        ),
        Some(json!(rig.item))
    );
    assert_eq!(applications_of(&rig, "child-2").len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_native_subagents_are_not_promoted_to_chats() {
    let rig = rig().await;
    let before = rig.core.workspace.read_chats().unwrap().len();
    let reply = launch(&rig, &params(&rig, "op-sub", "launch sub [subagent]"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(|| turns_with(&rig, "launch sub").len() == 1, "the turn").await;
    settle(&rig, &chat).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let chats = rig.core.workspace.read_chats().unwrap();
    assert_eq!(chats.len(), before + 1, "only the launched Chat exists");
    assert!(
        chats
            .iter()
            .all(|c| c.parent_chat_id.as_deref() != Some(chat.as_str()))
    );
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    assert_eq!(inputs.assignments.len(), 1);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Harnesses without instruction support; per-Chat worktree exceptions
// ---------------------------------------------------------------------------

fn unsupported_mock() -> InstructionMockHarness {
    InstructionMockHarness::new(mast_harness::InstructionSupport::Unsupported {
        reason: "no channel".into(),
        upgrade_hint: None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_instructions_launch_without_delivery_on_unsupported_harnesses() {
    let rig = rig_with(unsupported_mock(), false).await;
    let reply = launch(&rig, &params(&rig, "op-plain-launch", "launch plain"))
        .await
        .expect("launches without instructions");
    assert!(reply.revision_id.is_none() && reply.application_id.is_none());
    let chat = reply.chat_id.clone();
    wait_for(|| turns_with(&rig, "launch plain").len() == 1, "the turn").await;
    settle(&rig, &chat).await;
    assert!(turns_with(&rig, "launch plain")[0].revision_id.is_none());
    let cmds = commands(&rig, &chat);
    assert_eq!(cmds.len(), 1);
    assert!(matches!(cmds[0].payload, SessionCommandPayload::Run { .. }));
    assert_eq!(cmds[0].status, SessionCommandStatus::Applied);
    assert!(applications_of(&rig, &chat).is_empty());
    assert!(
        rig.core
            .workspace
            .registry_row(
                KIND_INSTRUCTION_REVISIONS,
                &derive_id("op-plain-launch", roles::REVISION)
            )
            .is_none()
    );
    assert!(
        rig.core
            .workspace
            .selected_instruction_revision(&chat)
            .is_none()
    );
    assert!(rig.core.workspace.chat_execution_binding(&chat).is_some());
    // A replay stays plain even after instructions appear.
    add_space_instruction(&rig.client, "op-later-instruction").await;
    let again = launch(&rig, &params(&rig, "op-plain-launch", "launch plain"))
        .await
        .expect("replay");
    assert!(again.revision_id.is_none());
    assert_eq!(commands(&rig, &chat).len(), 1);
    // Continue: no selection exists, so ingestion pins nothing.
    continue_chat(&rig, &chat, "op-plain-next", "launch plain next")
        .await
        .expect("continue");
    wait_for(
        || turns_with(&rig, "launch plain next").len() == 1,
        "the next turn",
    )
    .await;
    assert!(
        commands(&rig, &chat)
            .iter()
            .all(|c| matches!(c.payload, SessionCommandPayload::Run { .. }))
    );

    // Non-empty instructions now: a new launch is refused before anything is
    // written (no Chat, no ledger claim, no command).
    let error = launch(&rig, &params(&rig, "op-needs-delivery", "launch needs"))
        .await
        .expect_err("capability");
    assert_eq!(error.code, WorkOutlineErrorCode::Capability);
    let refused = derive_id("op-needs-delivery", roles::CHAT);
    assert!(rig.core.workspace.chat(&refused).unwrap().is_none());
    assert!(
        rig.core
            .workspace
            .chat_execution_binding(&refused)
            .is_none()
    );
    // Same operation id with other inputs is NOT a conflict: nothing was
    // claimed.
    let error = launch(&rig, &params(&rig, "op-needs-delivery", "launch needs 2"))
        .await
        .expect_err("still capability");
    assert_eq!(error.code, WorkOutlineErrorCode::Capability);
    assert!(turns_with(&rig, "launch needs").is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn per_chat_worktree_is_not_an_execution_exception_but_an_override_is() {
    let rig = rig().await;
    let mut p = params(&rig, "op-wt-exception", "launch wt");
    p.worktree_choice = Some(WorktreeChoice::CreateNew {
        name: "per chat".into(),
    });
    let worktree = launch(&rig, &p).await.expect("launch").chat_id;
    let other = rig.root.join("other");
    init_repo(&other, "o");
    let mut q = params(&rig, "op-override-exception", "launch override");
    q.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": { "executionRoot": other.to_string_lossy(), "schema": 1 }
        }))
        .unwrap(),
    );
    let overridden = launch(&rig, &q).await.expect("launch").chat_id;
    let plain = launch(&rig, &params(&rig, "op-plain-exception", "launch same"))
        .await
        .expect("launch")
        .chat_id;
    let snapshot =
        mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE));
    let exception = |chat: &str| snapshot.chat_execution_binding(chat).unwrap().exception;
    assert!(
        !exception(&worktree),
        "a per-Chat worktree is the normal flow"
    );
    assert!(!exception(&plain));
    assert!(exception(&overridden), "an override is an exception");
    assert_eq!(
        snapshot.execution_exception_chats(&rig.item),
        std::slice::from_ref(&overridden)
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 2: a malformed binding is bound-and-invalid, never unbound
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn malformed_binding_rejects_every_gate_instead_of_reading_as_unbound() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-bad", "launch bad"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch bad").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    // A newer build rewrites the binding in a shape this build cannot read.
    let mut row = rig
        .core
        .workspace
        .registry_row("chatExecutionBindings", &chat)
        .unwrap();
    row.fields.insert(
        "resolved".into(),
        json!({"schema": 2, "roots": ["/elsewhere"]}),
    );
    row.seq = 1_000_000;
    rig.core.workspace.apply_registry_rows(1_000_000, vec![row]);
    assert!(matches!(
        rig.core.workspace.chat_execution_binding_state(&chat),
        mast_proto::ChatExecutionBindingState::Malformed { .. }
    ));

    // Plain Run (any device, MCP send): rejected at drain, nothing written.
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");
    let messages = user_messages(&rig, &chat);
    let id = queue_run(&rig, &chat, "launch bad raw", &elsewhere.to_string_lossy());
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "configInvalid");
    assert_eq!(user_messages(&rig, &chat), messages);
    assert!(turns_with(&rig, "launch bad raw").is_empty());
    // Work-item continue.
    let error = continue_chat(&rig, &chat, "op-bad-next", "launch bad next")
        .await
        .expect_err("malformed binding blocks continue");
    assert!(error.contains("execution-scope:configInvalid"), "{error}");
    // Retarget and new worktree.
    let error = rig
        .client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": chat, "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .expect_err("SetChatCwd refuses");
    assert!(
        error.to_string().contains("execution-scope:configInvalid"),
        "{error}"
    );
    let error = rig
        .client
        .call(
            methods::CREATE_CHAT_WORKTREE,
            json!({ "chatId": chat, "repoPath": rig.repo.to_string_lossy(), "name": "nope" }),
        )
        .await
        .expect_err("CreateChatWorktree refuses");
    assert!(
        error.to_string().contains("execution-scope:configInvalid"),
        "{error}"
    );
    assert!(!rig.repo.join(".worktrees/workspace/nope").exists());
    // The relaunch of the same operation refuses too.
    let error = launch(&rig, &params(&rig, "op-bad", "launch bad"))
        .await
        .expect_err("relaunch refuses");
    assert!(
        error.message.contains("execution-scope:configInvalid"),
        "{error:?}"
    );
    // Validation by Chat reports it rather than "no binding".
    let value = rig
        .client
        .call(
            methods::VALIDATE_EXECUTION_CONFIGURATION,
            json!({ "chatId": chat }),
        )
        .await
        .expect("validation reply");
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(
        value["errors"][0]["error"]["code"], "configInvalid",
        "{value}"
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 1: HOME is never an execution root, and never remapped
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn home_directory_root_is_rejected_not_remapped_to_the_projectless_folder() {
    let rig = rig().await;
    let home = std::env::var("HOME").expect("HOME is set");
    // A launch override naming HOME is refused before any Chat exists.
    let mut p = params(&rig, "op-home", "launch home");
    p.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": { "executionRoot": home, "bindings": [], "schema": 1 }
        }))
        .unwrap(),
    );
    let error = launch(&rig, &p).await.expect_err("HOME root is refused");
    assert!(
        error.message.contains("execution-scope:homeDirectoryRoot"),
        "{error:?}"
    );
    assert!(
        error
            .message
            .contains("Choose a project folder inside your home directory"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-home", roles::CHAT))
            .unwrap()
            .is_none()
    );
    // A binding captured with HOME by an older build: a Run is rejected,
    // never run in the per-Chat projectless folder.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "home-chat", "spaceId": SPACE,
                    "deviceId": DEVICE, "cwd": home }),
        )
        .await
        .unwrap();
    let mut resolved = rig
        .core
        .workspace
        .resolve_execution(&mast_proto::ExecutionScope::Space {
            space_id: SPACE.into(),
        })
        .unwrap();
    resolved.configuration.execution_root = home.clone();
    resolved.configuration.bindings.clear();
    rig.core
        .workspace
        .capture_chat_execution_binding(
            "home-chat",
            &resolved,
            "op-home-capture",
            &mast_proto::WorkOutlineActor {
                kind: mast_proto::WorkOutlineActorKind::User,
                device_id: DEVICE.into(),
                chat_id: None,
                message_id: None,
            },
        )
        .unwrap();
    let id = queue_run(&rig, "home-chat", "home raw", &home);
    assert_rejected_with(
        &wait_command(&rig, "home-chat", &id).await,
        "homeDirectoryRoot",
    );
    assert!(turns_with(&rig, "home raw").is_empty());
    assert_eq!(user_messages(&rig, "home-chat"), 0);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 3: a bound Chat mid-turn at restart auto-resumes
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bound_chat_mid_turn_at_restart_auto_resumes_in_its_root() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-restart", "launch restart [hold]"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch restart").len() == 1,
        "the held turn",
    )
    .await;
    assert!(rig.core.sessions.turn_in_flight(&chat));
    let Rig {
        _tmp,
        core,
        client,
        root,
        repo,
        ..
    } = rig;
    core.shutdown().await;
    drop(client);
    drop(core);

    // Relaunch over the same data dir: the boot recovery's auto-resume runs
    // through the execution gate, which must already be wired.
    let mock = InstructionMockHarness::default();
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(harnesses),
        HarnessId::Mock,
        None,
    )
    .expect("engine core reassembles");
    wait_for(
        || {
            mock.turns()
                .iter()
                .any(|t| t.prompt.starts_with("launch restart"))
        },
        "the bound turn to auto-resume",
    )
    .await;
    let turn = mock
        .turns()
        .into_iter()
        .find(|t| t.prompt.starts_with("launch restart"))
        .unwrap();
    assert_eq!(mock.runs()[turn.run_index].cwd, repo.to_string_lossy());
    let rejected = core
        .doc_host
        .open(&chat)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap()
        .into_iter()
        .any(|c| c.status == SessionCommandStatus::Rejected);
    assert!(!rejected);
    core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 4: a plain Turn's resume check runs before any write
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn plain_run_resume_mismatch_writes_nothing_and_queue_promotion_does_not_duplicate() {
    // No instructions: the launch queues a plain Run.
    let rig = rig_with(InstructionMockHarness::default(), false).await;
    let launcher = service(&rig);
    let p = params(&rig, "op-plain-resume", "plain resume");
    launcher.inject_fault(Some(LaunchStep::BindingCaptured));
    assert!(launcher.create_chat_for_work_item(&p).await.is_err());
    launcher.inject_fault(None);
    let chat = derive_id("op-plain-resume", roles::CHAT);
    rig.core
        .workspace
        .set_chat_harness_session(&chat, "native-elsewhere", "/somewhere/else");

    // Queue promotion: the row is refused before any write and goes back to
    // the queue once; no user message lands, so a retry cannot duplicate.
    let queued = rig
        .core
        .doc_host
        .queue_message(&chat, "plain queued", Vec::new())
        .expect("queue");
    let queue = |rig: &Rig| {
        rig.core
            .doc_host
            .open(&chat)
            .unwrap()
            .doc()
            .read_queue()
            .unwrap()
    };
    let error = rig
        .core
        .doc_host
        .send_queued_now(&chat, &queued)
        .await
        .expect_err("promotion refused");
    assert!(
        error
            .to_string()
            .contains("execution-scope:resumeRootMismatch"),
        "{error}"
    );
    // A drain woken by the restore may retry (and refuse) it again; either
    // way the row settles back in the queue exactly once.
    wait_for(|| queue(&rig).len() == 1, "the row to return to the queue").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = queue(&rig);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, queued);
    assert_eq!(user_messages(&rig, &chat), 0);
    assert!(turns_with(&rig, "plain queued").is_empty());

    // The launch's plain Run: rejected at drain with nothing written.
    let reply = launcher
        .create_chat_for_work_item(&p)
        .await
        .expect("queued");
    assert!(reply.revision_id.is_none(), "an instruction-free launch");
    let entry = wait_command(&rig, &chat, &reply.command_id).await;
    assert_rejected_with(&entry, "resumeRootMismatch");
    assert_eq!(user_messages(&rig, &chat), 0);
    assert!(turns_with(&rig, "plain resume").is_empty());
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 5: a createNew retry never adopts a foreign worktree
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn create_new_retry_refuses_a_foreign_worktree_at_the_claimed_path() {
    let rig = rig().await;
    let launcher = service(&rig);
    let mut p = params(&rig, "op-foreign", "launch foreign");
    p.worktree_choice = Some(WorktreeChoice::CreateNew {
        name: "foreign".into(),
    });
    // The worktree is created, then the launch crashes.
    launcher.inject_fault(Some(LaunchStep::Validated));
    assert!(launcher.create_chat_for_work_item(&p).await.is_err());
    launcher.inject_fault(None);
    let worktree = rig.repo.join(".worktrees/workspace/foreign");
    assert!(worktree.is_dir());
    // Someone moves it away and puts another worktree of the same
    // repository at the same path (a different checkout identity).
    let moved = rig.root.join("foreign-moved");
    git(
        &rig.repo,
        &[
            "worktree",
            "move",
            &worktree.to_string_lossy(),
            &moved.to_string_lossy(),
        ],
    );
    git(
        &rig.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "intruder",
            &worktree.to_string_lossy(),
        ],
    );
    let error = launcher
        .create_chat_for_work_item(&p)
        .await
        .expect_err("the foreign worktree is not adopted");
    assert!(
        error.message.contains("is not the one this launch created"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-foreign", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 7: continue never runs an unbound Chat in an empty cwd
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn continue_refuses_an_assigned_unbound_chat_without_a_folder() {
    let rig = rig().await;
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "no-cwd", "spaceId": SPACE, "deviceId": DEVICE }),
        )
        .await
        .unwrap();
    // A row whose folder was cleared (an older client, a partial import).
    rig.core.workspace.set_chat_cwd("no-cwd", "").unwrap();
    assert!(
        rig.core
            .workspace
            .chat("no-cwd")
            .unwrap()
            .unwrap()
            .cwd
            .is_none_or(|cwd| cwd.is_empty()),
        "fixture: the Chat has no folder"
    );
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "no-cwd", "workItemId": rig.item,
                    "operationId": "op-assign-no-cwd" }),
        )
        .await
        .unwrap();
    let error = continue_chat(&rig, "no-cwd", "op-no-cwd", "no cwd next")
        .await
        .expect_err("an empty cwd is refused");
    assert!(error.contains("work-outline/invalid"), "{error}");
    assert!(error.contains("no folder to run in"), "{error}");
    assert!(commands(&rig, "no-cwd").is_empty());
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 8: a worktree choice needs the root to be that repo's checkout
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn worktree_choice_is_refused_when_the_root_is_not_the_repository_checkout() {
    let rig = rig().await;
    // Root = a parent folder holding the repository (not its checkout).
    let identity = describe(&rig, &rig.repo).await;
    let mut p = params(&rig, "op-wt-root", "launch wt root");
    p.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": {
                "executionRoot": rig.root.to_string_lossy(),
                "bindings": [{
                    "repoId": identity.repo_id, "repoFingerprint": identity.repo_fingerprint,
                    "repoPath": identity.repo_path, "member": true, "primary": true,
                }],
                "schema": 1,
            }
        }))
        .unwrap(),
    );
    p.worktree_choice = Some(WorktreeChoice::CreateNew {
        name: "never".into(),
    });
    let error = launch(&rig, &p).await.expect_err("worktree choice refused");
    assert_eq!(error.code, WorkOutlineErrorCode::Invalid, "{error:?}");
    assert!(
        error
            .message
            .contains("execution root is the primary repository's checkout"),
        "{error:?}"
    );
    // Refused before any worktree or Chat exists.
    assert!(!rig.repo.join(".worktrees/workspace/never").exists());
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-wt-root", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review fix 6: diff-sync never serves a member checkout that is not bound
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn diff_sync_skips_a_member_path_whose_checkout_is_not_the_bound_one() {
    let rig = rig().await;
    let member = rig.repo.join(".worktrees/m/w");
    std::fs::create_dir_all(member.parent().unwrap()).unwrap();
    git(
        &rig.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "member",
            &member.to_string_lossy(),
        ],
    );
    let member = std::fs::canonicalize(&member).unwrap();
    let repo = describe(&rig, &rig.repo).await;
    let pinned = describe(&rig, &member).await;
    let mut p = params(&rig, "op-member", "launch member");
    p.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": {
                "executionRoot": rig.repo.to_string_lossy(),
                "bindings": [{
                    "repoId": repo.repo_id, "repoFingerprint": repo.repo_fingerprint,
                    "repoPath": repo.repo_path, "member": true, "primary": true,
                    "worktreeId": pinned.checkout_id,
                    "worktreePath": member.to_string_lossy(),
                }],
                "schema": 1,
            }
        }))
        .unwrap(),
    );
    let reply = launch(&rig, &p).await.expect("launch");
    wait_for(
        || turns_with(&rig, "launch member").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &reply.chat_id).await;
    let has_diff = |id: &str| {
        rig.core
            .diff_sync
            .watch_diffs()
            .borrow()
            .iter()
            .any(|diff| diff.checkout_id == id)
    };
    // The bound member gets its own entry.
    rig.core.diff_sync.repair_now().await;
    wait_for(|| has_diff(&pinned.checkout_id), "the member diff").await;

    // Another checkout now sits at the member path.
    git(
        &rig.repo,
        &["worktree", "remove", "--force", &member.to_string_lossy()],
    );
    init_repo(&member, "foreign");
    let foreign = describe(&rig, &member).await;
    assert_ne!(foreign.checkout_id, pinned.checkout_id);
    rig.core.diff_sync.repair_now().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        assert!(
            !has_diff(&foreign.checkout_id),
            "the foreign checkout must not get a member entry"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// A work item rooted at a nested repository (a submodule folder below the
// Space path) runs its Chat there, so the agent loads that folder's skills
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn item_rooted_at_a_nested_repository_runs_its_chat_there() {
    let rig = rig().await;
    // The Space folder is the superproject; `ui` is its own repository below
    // it (a submodule checkout), bound as a pinned, non-primary member.
    let ui = rig.repo.join("ui");
    init_repo(&ui, "package.json");
    let ui = std::fs::canonicalize(&ui).unwrap();
    let identity = describe(&rig, &ui).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({
                "op": "configureExecution", "spaceId": SPACE,
                "scope": { "type": "workItem", "workItemId": rig.item },
                "configuration": {
                    "executionRoot": ui.to_string_lossy(),
                    "bindings": [{
                        "repoId": identity.repo_id,
                        "repoFingerprint": identity.repo_fingerprint,
                        "repoPath": identity.repo_path,
                        "member": true, "primary": false,
                        "worktreeId": identity.checkout_id,
                        "worktreePath": ui.to_string_lossy(),
                    }],
                    "schema": 1,
                },
                "operationId": "op-configure-ui",
            }),
        )
        .await
        .expect("configure the item at ui");

    let reply = launch(&rig, &params(&rig, "op-ui", "launch ui"))
        .await
        .expect("launch");
    assert_eq!(reply.execution_root, ui.to_string_lossy());
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch ui").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "launch ui")[0]),
        ui.to_string_lossy(),
        "the agent runs in the submodule, not the Space folder"
    );
    let row = rig.core.workspace.chat(&chat).unwrap().expect("chat row");
    assert_eq!(
        row.cwd.as_deref(),
        Some(&*ui.to_string_lossy()),
        "the composer's `/` list discovers skills in the chat's cwd"
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Manager: lazy creation of a project's standing role chat
// ---------------------------------------------------------------------------

async fn mark_project(rig: &Rig) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": rig.item,
                    "changes": { "isProject": true }, "operationId": "op-mark-project" }),
        )
        .await
        .unwrap();
}

fn manager_params(rig: &Rig, op: &str, prompt: &str) -> CreateChatForWorkItemParams {
    CreateChatForWorkItemParams {
        standing_role: Some(mast_proto::StandingRoleKind::Manager),
        ..params(rig, op, prompt)
    }
}

fn manager_chat(rig: &Rig) -> Option<String> {
    rig.core
        .workspace
        .standing_role_chat(mast_proto::StandingRoleKind::Manager, &rig.item)
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_context_threshold_starts_successor_with_project_status() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "auto-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    let draft = mast_proto::ScheduledTaskDraft {
        name: "Existing scheduled task".into(), prompt: "Continue scheduled work".into(),
        cwd: rig.repo.to_string_lossy().into_owned(), space_id: Some(SPACE.into()),
        config: params(&rig, "unused", "unused").config.unwrap(),
        schedule: mast_proto::TaskSchedule::Cron { expression: "0 8 * * 1-5".into(), timezone: "UTC".into() },
        enabled: false, grace_minutes: 360, cutover_confirmed: false, work_item_id: Some(rig.item.clone()),
        output: Default::default(), after_success: Default::default(), chat_mode: Default::default(), refresh: Default::default(), write_to_notion: None,
    };
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::Save { id: "existing-task".into(), expected_revision: 0, draft, operation_id: "save-existing-task".into() }).await.unwrap();
    let lease = rig.core.sessions.agent_tokens().mint(&source, "status-run", SPACE, &rig.core.device_id);
    let sections = mast_proto::MANAGER_STATUS_SECTIONS.iter().map(|section| format!("## {section}\nproject continuity evidence\n\n")).collect::<String>();
    let content = format!("Updated: 2026-01-01 12:00\n\n{sections}");
    rig.client.call(mast_proto::agent_tools::TOOL_CALL_METHOD, json!({ "token": lease.token(), "tool": "artifact_publish", "arguments": {
        "title": "Manager status", "role": "manager-status", "content": content,
        "note": "checkpoint", "idempotencyKey": "auto-status"
    }})).await.unwrap();
    let result = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Trigger the handoff [hold]" }, "operationId": "auto-threshold-turn" })).await.unwrap();
    assert!(result["chatId"].is_string());
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "threshold turn start").await;
    rig.core.workspace.create_chat_with_parent("pending-worker", Some(SPACE), None, None, None, Some(source.clone())).unwrap();
    rig.core.orchestrator_wakes.report_to_parent("pending-worker", "pending handoff wake").await.unwrap();
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.release();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "automatic successor").await;
    let successor = manager_chat(&rig).unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("project continuity evidence")), "successor first turn with status").await;
    assert!(rig.core.workspace.chat(&source).unwrap().unwrap().archived);
    assert_eq!(rig.core.workspace.chat_assigned_work_item(&successor, SPACE).as_deref(), Some(rig.item.as_str()));
    // The transferred wake is held for one wake window, so it must not have reached the
    // successor yet. Then move the wake clock past the window instead of sleeping the
    // real 45 s and evaluate the successor. The hook exists only in debug builds and the
    // shift lasts for the rest of the test.
    assert!(!rig.mock.turns().iter().any(|turn| turn.prompt.contains("pending handoff wake")),
        "the transferred wake must stay held until the wake window elapses");
    const PAST_THE_WINDOW_MS: i64 = WAKE_WINDOW_MS + 1_000;
    rig.core.orchestrator_wakes.test_advance_clock(PAST_THE_WINDOW_MS);
    rig.core.orchestrator_wakes.evaluate(&successor, false).await;
    let wake_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !rig.mock.turns().iter().any(|turn| turn.prompt.contains("pending handoff wake")) {
        assert!(tokio::time::Instant::now() < wake_deadline, "timed out waiting for transferred wake: {:?}", rig.core.orchestrator_wakes.status(&successor));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("pending handoff wake")).count(), 1);
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "existing-task".into(), operation_id: "run-existing-task".into() }).await.unwrap();
    rig.core.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis()).await.unwrap();
    let snapshot = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    let scheduled_chat = snapshot.runs.iter().find(|run| run.task_id == "existing-task").unwrap().chat_id.as_deref().unwrap();
    let scheduled = rig.core.workspace.chat(scheduled_chat).unwrap().unwrap();
    assert_eq!(scheduled.parent_chat_id.as_deref(), Some(successor.as_str()));
    assert_eq!(scheduled.config.as_ref().unwrap().harness, HarnessId::Mock);
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Continue scheduled work")), "scheduled task start").await;
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "existing-task".into(), operation_id: "run-existing-task".into() }).await.unwrap();
    let replay = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    assert_eq!(replay.runs.iter().filter(|run| run.task_id == "existing-task").count(), 1);
    assert_eq!(rig.core.workspace.chat(scheduled_chat).unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        rig.core.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis()).await.unwrap();
        let snapshot = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
        if snapshot.runs.iter().any(|run| run.task_id == "existing-task" && run.state == mast_proto::ScheduledRunState::Completed) { break; }
        assert!(tokio::time::Instant::now() < deadline, "scheduled task did not complete");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let report = rig.core.orchestrator_wakes.report_to_parent(scheduled_chat, "scheduled task complete").await.unwrap();
    assert_eq!(report.parent_chat_id, successor);
    assert_eq!(report.delivery, "queuedForParent");
    rig.core.shutdown().await;
}

fn assistant_params(rig: &Rig, op: &str, prompt: &str) -> CreateChatForWorkItemParams {
    CreateChatForWorkItemParams {
        standing_role: Some(mast_proto::StandingRoleKind::Assistant),
        ..params(rig, op, prompt)
    }
}

fn assistant_chat(rig: &Rig) -> Option<String> {
    rig.core.workspace.standing_role_chat(mast_proto::StandingRoleKind::Assistant, &rig.item)
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_context_threshold_hands_off_and_moves_designation() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &assistant_params(&rig, "auto-assistant", "Start assisting")).await.unwrap().chat_id;
    wait_for(|| assistant_chat(&rig).as_deref() == Some(source.as_str()), "Assistant role").await;
    settle(&rig, &source).await;
    let source_chat = rig.core.workspace.chat(&source).unwrap().unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.personal_assistant().set(&source_chat);

    rig.mock.hold_matching(Some("At threshold"));
    continue_chat(&rig, &source, "assistant-at", "At threshold").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(80), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| assistant_chat(&rig).is_some_and(|chat| chat != source), "assistant successor").await;
    let successor = assistant_chat(&rig).unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("personal assistant role") && turn.prompt.contains(&source)),
        "successor first turn").await;
    assert!(rig.core.workspace.chat(&source).unwrap().unwrap().archived);
    assert_eq!(rig.core.workspace.chat_assigned_work_item(&successor, SPACE).as_deref(), Some(rig.item.as_str()));
    assert_eq!(tokens.personal_assistant().get().map(|assistant| assistant.chat_id), Some(successor.clone()));
    assert!(manager_chat(&rig).is_none_or(|chat| chat != successor));
    // Only one successor was started.
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("personal assistant role")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_below_context_threshold_keeps_its_role() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &assistant_params(&rig, "below-assistant", "Start assisting")).await.unwrap().chat_id;
    wait_for(|| assistant_chat(&rig).as_deref() == Some(source.as_str()), "Assistant role").await;
    settle(&rig, &source).await;
    rig.mock.hold_matching(Some("Below threshold"));
    continue_chat(&rig, &source, "assistant-below", "Below threshold").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "below-threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(79), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    settle(&rig, &source).await;
    tokio::time::sleep(Duration::from_millis(1250)).await;
    assert_eq!(assistant_chat(&rig).as_deref(), Some(source.as_str()));
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("personal assistant role")).count(), 0);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_handoff_resumes_after_restart_with_one_successor() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &assistant_params(&rig, "restart-assistant", "Start assisting")).await.unwrap().chat_id;
    wait_for(|| assistant_chat(&rig).as_deref() == Some(source.as_str()), "Assistant role").await;
    settle(&rig, &source).await;
    let source_chat = rig.core.workspace.chat(&source).unwrap().unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.personal_assistant().set(&source_chat);
    // The intent is saved, but no attempt may start before the restart.
    let block = Arc::new(AtomicBool::new(true));
    let blocking = block.clone();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, _| {
        event == "automatic_handoff_attempt" && blocking.load(SeqCst)
    })));
    rig.mock.hold_matching(Some("Restart threshold"));
    continue_chat(&rig, &source, "assistant-restart-turn", "Restart threshold").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let intents_path = rig.root.join("data/automatic-manager-handoffs.json");
    wait_for(|| std::fs::read_to_string(&intents_path).is_ok_and(|text| text.contains(&source)), "durable assistant intent").await;
    assert_eq!(assistant_chat(&rig).as_deref(), Some(source.as_str()));

    let Rig { _tmp, core, mock, client, root, item, .. } = rig;
    drop(tokens);
    drop(client);
    core.shutdown().await;
    drop(core);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();
    wait_for(|| restarted.workspace.standing_role_chat(mast_proto::StandingRoleKind::Assistant, &item)
        .is_some_and(|chat| chat != source), "assistant successor after restart").await;
    let successor = restarted.workspace.standing_role_chat(mast_proto::StandingRoleKind::Assistant, &item).unwrap();
    let queue_state = || format!("successor {successor}: session {:?}, queue {:?}, commands {:?}, turns {:?}",
        restarted.sessions.session_status(&successor),
        restarted.doc_host.open(&successor).map(|handle| handle.doc().read_queue()),
        restarted.doc_host.open(&successor).map(|handle| handle.doc().read_commands()), mock.turns());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !mock.turns().iter().any(|turn| turn.prompt.contains("personal assistant role") && turn.prompt.contains(&source)) {
        assert!(tokio::time::Instant::now() < deadline, "successor first turn after restart: {}", queue_state());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(restarted.workspace.chat(&source).unwrap().unwrap().archived);
    assert_eq!(restarted.sessions.agent_tokens().personal_assistant().get().map(|assistant| assistant.chat_id), Some(successor));
    assert_eq!(mock.turns().iter().filter(|turn| turn.prompt.contains("personal assistant role")).count(), 1);
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_critical_context_without_status_keeps_predecessor_context() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "no-status-manager", "Initial project priority")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    rig.mock.hold_matching(Some("Recent decision context"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Recent decision context" }, "operationId": "no-status-turn" })).await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "threshold turn start").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "automatic successor without status").await;
    let successor = manager_chat(&rig).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !rig.mock.turns().iter().any(|turn| turn.prompt.contains("Recent decision context") && turn.prompt.contains(&source)) {
        assert!(tokio::time::Instant::now() < deadline, "missing successor fallback context: {:?}", rig.mock.turns());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rig.core.workspace.chat(&source).unwrap().unwrap().standing_role_successor.as_deref(), Some(successor.as_str()));
    let mut changed_config = rig.core.workspace.chat(&source).unwrap().unwrap().config.unwrap();
    changed_config.model = Some("changed-after-acceptance".into());
    rig.core.workspace.set_chat_config(&source, &changed_config).unwrap();
    let old_replay = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Recent decision context" }, "operationId": "no-status-turn" })).await.unwrap();
    assert_eq!(old_replay["chatId"], source, "a retry preserves the original accepted turn");
    assert!(rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Recent decision context", "attachments": ["/tmp/changed.png"] }, "operationId": "no-status-turn" })).await.is_err(), "changed attachments conflict across handoff");
    let send = json!({ "chatId": source, "messageId": "after-handoff-user-message", "text": "Continue after rotation", "attachments": [], "holdForTurnEnd": true });
    rig.client.call(methods::QUEUE_MESSAGE, send.clone()).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !rig.mock.turns().iter().any(|turn| turn.prompt.contains("Continue after rotation")) {
        assert!(tokio::time::Instant::now() < deadline, "post-handoff send stalled: source queue {:?}, successor queue {:?}, successor session {:?}", rig.core.doc_host.open(&source).unwrap().doc().read_queue(), rig.core.doc_host.open(&successor).unwrap().doc().read_queue(), rig.core.sessions.session_status(&successor));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.client.call(methods::QUEUE_MESSAGE, send).await.unwrap();
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Continue after rotation")).count(), 1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while rig.core.sessions.turn_in_flight(&successor) {
        assert!(tokio::time::Instant::now() < deadline, "successor did not settle: session {:?}, turns {:?}, queue {:?}", rig.core.sessions.session_status(&successor), rig.mock.turns(), rig.core.doc_host.open(&successor).unwrap().doc().read_queue());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let followup = json!({ "chatId": source, "prompt": { "text": "Idle follow-up after handoff" }, "operationId": "idle-after-handoff" });
    let reply = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, followup.clone()).await.unwrap();
    assert_eq!(reply["chatId"], successor);
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Idle follow-up after handoff")), "idle follow-up at successor").await;
    let mut successor_config = rig.core.workspace.chat(&successor).unwrap().unwrap().config.unwrap();
    successor_config.model = Some("changed-after-followup".into());
    rig.core.workspace.set_chat_config(&successor, &successor_config).unwrap();
    let replay = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, followup).await.unwrap();
    assert_eq!(replay["chatId"], successor);
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Idle follow-up after handoff")).count(), 1);
    let incomplete_id = derive_id("missing-continuation-command", roles::MESSAGE);
    rig.core.doc_host.open(&successor).unwrap().write_user_message(&incomplete_id, "Unverifiable prior input", chrono::Utc::now().timestamp_millis()).unwrap();
    assert!(rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Unverifiable prior input" }, "operationId": "missing-continuation-command" })).await.is_err(), "a missing original command must not be replayed without input verification");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_eighty_percent_checkpoints_then_hands_off() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "checkpoint-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = attempts.clone();
    let owner = source.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        event == "queue" && chat == owner
            && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
    })));
    rig.mock.hold_matching(Some("Your last turn changed state"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Threshold work [hold]" }, "operationId": "checkpoint-trigger" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Threshold work [hold]")), "threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(80), Some(100)).unwrap();
    rig.mock.release();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Your last turn changed state")), "checkpoint turn").await;
    assert!(attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    tokens.manager_status().set_test_hook(None);
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    let lease = rig.core.sessions.agent_tokens().mint(&source, "checkpoint-run", SPACE, &rig.core.device_id);
    let sections = mast_proto::MANAGER_STATUS_SECTIONS.iter().map(|section| format!("## {section}\ncheckpoint evidence\n\n")).collect::<String>();
    rig.client.call(mast_proto::agent_tools::TOOL_CALL_METHOD, json!({"token":lease.token(), "tool":"artifact_publish", "arguments": {
        "title":"Manager status", "role":"manager-status", "content":format!("Updated: 2026-01-01\n\n{sections}"), "note":"checkpoint", "idempotencyKey":"checkpoint-status"
    }})).await.unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "successor after checkpoint").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("checkpoint evidence")), "successor status").await;
    rig.core.shutdown().await;
}

fn status_nudges(mock: &InstructionMockHarness) -> usize {
    mock.turns().iter().filter(|turn| turn.prompt.contains("Your last turn changed state")).count()
}

async fn publish_manager_status(core: &EngineCore, client: &RpcClient, source: &str, key: &str) {
    let lease = core.sessions.agent_tokens().mint(source, key, SPACE, &core.device_id);
    let sections = mast_proto::MANAGER_STATUS_SECTIONS.iter().map(|section| format!("## {section}\ncheckpoint evidence\n\n")).collect::<String>();
    client.call(mast_proto::agent_tools::TOOL_CALL_METHOD, json!({"token":lease.token(), "tool":"artifact_publish", "arguments": {
        "title":"Manager status", "role":"manager-status", "content":format!("Updated: 2026-01-01\n\n{sections}"), "note":key, "idempotencyKey":key
    }})).await.unwrap();
}

/// A Manager at 80% context that has been asked for its status once and
/// whose checkpoint turn has finished without a status.
async fn manager_asked_for_status(rig: &Rig, op: &str) -> String {
    mark_project(rig).await;
    let source = launch(rig, &manager_params(rig, op, "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(rig, &source).await;
    rig.mock.hold_matching(Some("Threshold work"));
    continue_chat(rig, &source, &format!("{op}-trigger"), "Threshold work").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&source), "threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(80), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| status_nudges(&rig.mock) == 1, "status request").await;
    settle(rig, &source).await;
    wait_for(|| checkpoint_records(&rig.root).contains_key(&source), "durable checkpoint").await;
    // Let the watcher see the checkpoint turn finish without a status.
    tokio::time::sleep(Duration::from_millis(1250)).await;
    assert_eq!(manager_chat(rig).as_deref(), Some(source.as_str()));
    source
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_status_request_is_not_repeated_after_restart_and_still_hands_off() {
    let rig = rig().await;
    let source = manager_asked_for_status(&rig, "restart-checkpoint").await;

    let saved_turn = checkpoint_records(&rig.root)[&source]["turn"].as_str().unwrap().to_owned();

    let Rig { _tmp, core, mock, client, root, item, .. } = rig;
    drop(client);
    core.shutdown().await;
    drop(core);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();
    let client = mast_rpc::memory_client(restarted.rpc_service());
    let role_chat = || restarted.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &item);

    // A completed turn after the restart asks nothing more of the Manager.
    client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Work after restart" }, "operationId": "after-restart" })).await.unwrap();
    wait_for(|| mock.turns().iter().any(|turn| turn.prompt.contains("Work after restart")), "turn after restart").await;
    wait_for(|| !restarted.sessions.turn_in_flight(&source), "turn after restart to settle").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(status_nudges(&mock), 1, "a restart must not queue a second status request");
    assert_eq!(role_chat().as_deref(), Some(source.as_str()));

    // The saved baseline still lets a newer status hand the Manager off.
    publish_manager_status(&restarted, &client, &source, "restart-checkpoint-status").await;
    client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Publish turn" }, "operationId": "publish-after-restart" })).await.unwrap();
    wait_for(|| role_chat().is_some_and(|chat| chat != source), "successor after restart checkpoint").await;
    wait_for(|| mock.turns().iter().any(|turn| turn.prompt.contains("checkpoint evidence")), "successor status").await;
    assert_eq!(status_nudges(&mock), 1);
    // The mock numbers a run's steered turns afresh, so the turn that led to
    // the hand-off carries the id saved before the restart. The restored
    // record must not mistake it for the turn that asked for the status.
    assert_eq!(restarted.sessions.session_status(&source).unwrap().last_completed_turn.as_deref(), Some(saved_turn.as_str()));
    // The completed hand-off clears the record.
    wait_for(|| !checkpoint_records(&root).contains_key(&source), "checkpoint cleared").await;
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_checkpoint_of_an_archived_chat_is_pruned_and_an_unknown_chat_is_kept() {
    let rig = rig().await;
    let source = manager_asked_for_status(&rig, "pruned-checkpoint").await;
    let Rig { _tmp, core, mock, client, root, .. } = rig;
    drop(client);
    core.shutdown().await;
    drop(core);
    // A record whose chat the registry does not know yet, as before a sync.
    let mut records = checkpoint_records(&root);
    records.insert("not-synced-yet".into(), json!({ "turn": "t", "revision": 2 }));
    std::fs::write(root.join("data/automatic-manager-checkpoints.json"), serde_json::to_vec(&records).unwrap()).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();

    restarted.workspace.set_chat_archived(&source, true).unwrap();
    wait_for(|| !checkpoint_records(&root).contains_key(&source), "archived chat's checkpoint pruned").await;
    assert!(checkpoint_records(&root).contains_key("not-synced-yet"), "an unknown chat keeps its record");
    restarted.shutdown().await;
}

fn checkpoint_records(root: &Path) -> serde_json::Map<String, Value> {
    std::fs::read(root.join("data/automatic-manager-checkpoints.json")).ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_checkpoint_is_dropped_when_context_falls_below_the_threshold() {
    let rig = rig().await;
    let source = manager_asked_for_status(&rig, "falling-checkpoint").await;

    // A reset or compaction: a newer status no longer hands the Manager off.
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(40), Some(100)).unwrap();
    publish_manager_status(&rig.core, &rig.client, &source, "falling-checkpoint-status").await;
    continue_chat(&rig, &source, "falling-low", "Work at low usage").await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Work at low usage")), "low usage turn").await;
    settle(&rig, &source).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()), "a stale checkpoint must not hand off a small context");
    assert!(!checkpoint_records(&rig.root).contains_key(&source));

    // Climbing again is asked about again.
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(85), Some(100)).unwrap();
    continue_chat(&rig, &source, "falling-high", "Work at high usage").await.unwrap();
    wait_for(|| status_nudges(&rig.mock) == 2, "second status request after the context fell").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    rig.core.shutdown().await;
}

#[tokio::test]
async fn automatic_handoff_resumes_frozen_intent_after_transfer_and_restart() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "retry-source", "Start managing")).await.unwrap().chat_id;
    settle(&rig, &source).await;
    rig.core.workspace.create_chat_with_parent(
        "retry-worker", Some(SPACE), Some(DEVICE), None, None, Some(source.clone()),
    ).unwrap();
    rig.core.workspace.rename_chat("retry-worker", "Original worker title").unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.admin().grant(&source, SPACE, None, true);
    let attempts = Arc::new(AtomicUsize::new(0));
    let owner = source.clone();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        event == "automatic_handoff_attempt" && chat == owner && attempts.fetch_add(1, SeqCst) > 0
    })));
    rig.core.test_inject_work_item_launch_fault(LaunchStep::WorkersTransferred);

    let threshold = "Original status source turn";
    rig.mock.hold_matching(Some(threshold));
    continue_chat(&rig, &source, "retry-first-turn", threshold).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains(threshold)), "threshold turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| rig.core.workspace.part_done_hand_off_from(&source).is_some(), "durable worker transfer").await;
    let pending = rig.core.workspace.part_done_hand_off_from(&source).unwrap();
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    assert_eq!(rig.core.workspace.chat("retry-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(pending.to_chat_id.as_str()));

    // Inputs and role grants can change before the recovery attempt. The
    // successor must still use the original prepared launch and owner.
    rig.core.workspace.rename_chat("retry-worker", "Later worker title").unwrap();
    let intervening_grant = tokens.admin().grant(&source, SPACE, None, false);
    let later_turn = "New source input after transfer with a new operation";
    continue_chat(&rig, &source, "retry-later-turn", later_turn).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains(later_turn)), "later source turn").await;
    settle(&rig, &source).await;
    assert_eq!(rig.core.workspace.part_done_hand_off_from(&source).unwrap(), pending);
    let report_text = "Hold this report until the exact successor has started";
    let report = rig.core.orchestrator_wakes.report_to_parent("retry-worker", report_text).await.unwrap();
    assert_eq!(report.parent_chat_id, pending.to_chat_id);
    assert_eq!(report.delivery, "queuedForParent");
    let event = rig.core.orchestrator_wakes.status(&pending.to_chat_id).held.into_iter()
        .find(|event| event.report.as_deref() == Some(report_text)).unwrap();

    let Rig { _tmp, core, mock, client, root, item, .. } = rig;
    drop(tokens);
    drop(client);
    core.shutdown().await;
    drop(core);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();
    // This test uses a current-thread executor. Install the retry pause
    // before the engine's first spawned task can be polled.
    let (watcher, resume) = restarted.test_start_paused_manager_watcher();
    let client = mast_rpc::memory_client(restarted.rpc_service());
    let deliver = || client.call(methods::WAKE_DELIVER_NOW, json!({ "chatId": pending.to_chat_id }));
    let successor = restarted.doc_host.open(&pending.to_chat_id).unwrap();
    let starts_before = mock.turns().len();
    assert!(restarted.orchestrator_wakes.status(&pending.to_chat_id).held.iter().any(|held| held.id == event.id));

    deliver().await.unwrap();
    let assert_held = || {
        assert!(restarted.orchestrator_wakes.status(&pending.to_chat_id).held.iter().any(|held| held.id == event.id),
            "the worker report must stay held until the automatic handoff commits");
        assert!(!restarted.sessions.has_live_run(&pending.to_chat_id));
        assert_eq!(mock.turns().len(), starts_before);
        assert!(successor.doc().read_queue().unwrap().is_empty());
        let wake_path = root.join("data/orchestrator-wake-events.json");
        let wakes: serde_json::Value = serde_json::from_slice(&std::fs::read(wake_path).unwrap()).unwrap();
        let record = wakes["orchestrators"][&pending.to_chat_id].as_array().unwrap().iter()
            .find(|entry| entry["event"]["id"] == event.id).unwrap();
        assert!(record["message_id"].is_null());
        assert_eq!(record["delivered"], false);
    };
    assert_held();
    restarted.orchestrator_wakes.test_advance_clock(121_000);
    restarted.orchestrator_wakes.evaluate(&pending.to_chat_id, false).await;
    assert_held();

    mock.hold_matching(Some("coordinator hand-off"));
    resume.send(()).unwrap();
    wait_for(|| restarted.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &item).as_deref()
        == Some(pending.to_chat_id.as_str()), "original successor after restart").await;
    wait_for(|| mock.turns().iter().any(|turn| turn.prompt.contains("coordinator hand-off")),
        "the original successor's first harness start").await;
    let successors = mock.turns().into_iter().filter(|turn| turn.prompt.contains("coordinator hand-off")).collect::<Vec<_>>();
    assert_eq!(successors.len(), 1);
    assert!(successors[0].prompt.contains("Original worker title"));
    assert!(!successors[0].prompt.contains("Later worker title"));
    assert!(!successors[0].prompt.contains(later_turn));
    let tokens = restarted.sessions.agent_tokens();
    assert_eq!(tokens.admin().live(&source), Some(intervening_grant));
    assert!(tokens.admin().live(&pending.to_chat_id).is_none());
    assert!(restarted.workspace.chat(&source).unwrap().unwrap().archived);
    assert_eq!(mock.turns().len(), starts_before + 1);
    assert!(mock.turns().iter().all(|turn| !turn.prompt.contains(report_text)));
    mock.hold_matching(None);
    mock.release();
    wait_for(|| restarted.sessions.session_status(&pending.to_chat_id).is_some_and(|status|
        status.status == mast_proto::SessionStatus::Idle), "successor first turn to settle").await;
    deliver().await.unwrap();
    wait_for(|| mock.turns().iter().any(|turn| turn.prompt.contains(report_text)), "released wake to start").await;
    deliver().await.unwrap();
    let turns = mock.turns();
    assert_eq!(turns.len(), starts_before + 2);
    assert!(turns[starts_before].prompt.contains("coordinator hand-off"));
    assert!(turns[starts_before + 1].prompt.contains(report_text));
    watcher.cancel();
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn automatic_handoff_retries_rejected_first_turn_on_same_successor() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "rejected-source", "Start managing")).await.unwrap().chat_id;
    settle(&rig, &source).await;
    let reject = Arc::new(AtomicBool::new(true));
    let rejecting = reject.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, _| {
        event == "automatic_handoff_turn_reject" && rejecting.load(SeqCst)
    })));
    let threshold = "Threshold with a rejected successor";
    rig.mock.hold_matching(Some(threshold));
    continue_chat(&rig, &source, "rejected-trigger", threshold).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains(threshold)), "rejection threshold").await;
    let handle = rig.core.doc_host.open(&source).unwrap();
    handle.doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| handle.doc().read_entries().unwrap().iter().any(|entry| entry.parts.iter()
        .any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("first turn was rejected")))),
        "visible rejected successor notice").await;
    let pending = rig.core.workspace.part_done_hand_off_from(&source).unwrap();
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    reject.store(false, SeqCst);
    wait_for(|| manager_chat(&rig).as_deref() == Some(pending.to_chat_id.as_str()), "same successor to take the role").await;
    tokens.manager_status().set_test_hook(None);
    let successor = rig.core.doc_host.open(&pending.to_chat_id).unwrap();
    let commands = successor.doc().read_commands().unwrap();
    assert!(commands.iter().any(|command| command.status == mast_doc::SessionCommandStatus::Rejected));
    assert!(commands.iter().any(|command| command.status == mast_doc::SessionCommandStatus::Applied));
    let logical_message = derive_id(&pending.operation_id, roles::MESSAGE);
    assert_eq!(successor.doc().read_entries().unwrap().iter().filter(|entry| entry.id == logical_message).count(), 1);
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("coordinator hand-off")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn automatic_handoff_parks_when_a_manual_handoff_changes_the_owner() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "parked-intent-source", "Start managing")).await.unwrap().chat_id;
    settle(&rig, &source).await;
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, _| event == "automatic_handoff_attempt")));
    let threshold = "Prepare the automatic intent";
    rig.mock.hold_matching(Some(threshold));
    continue_chat(&rig, &source, "parked-intent-trigger", threshold).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains(threshold)), "prepared threshold turn").await;
    let handle = rig.core.doc_host.open(&source).unwrap();
    handle.doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let intents_path = rig.root.join("data/automatic-manager-handoffs.json");
    wait_for(|| intents_path.is_file(), "on-disk automatic intent before reservation").await;

    let operation = "manual-operation-after-intent";
    rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": source, "summary": "Choose the manual successor", "operationId": operation, "archive": true
    })).await.unwrap();
    let manual = derive_id(operation, roles::CHAT);
    wait_for(|| manager_chat(&rig).as_deref() == Some(manual.as_str()), "manual successor").await;
    tokens.manager_status().set_test_hook(None);
    wait_for(|| handle.doc().read_entries().unwrap().iter().any(|entry| entry.parts.iter()
        .any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("Automatic handoff paused")))),
        "stale automatic intent to park visibly").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(manual.as_str()));
    // The source no longer owns the role, so the parked intent can never
    // finish. The watcher parks it, then clears it on its next pass.
    wait_for(|| std::fs::read(&intents_path).ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
        .is_some_and(|intents| intents.is_empty()),
        "stale parked intent cleared once the source lost the role").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("coordinator hand-off")),
        "manual successor to start its handoff turn").await;
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("coordinator hand-off")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_defers_handoff_until_queued_user_message_runs() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "queued-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    rig.mock.hold_matching(Some("Active turn"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Active turn" }, "operationId": "queued-active" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Active turn")), "active turn").await;
    rig.client.call(methods::QUEUE_MESSAGE, json!({ "chatId": source, "messageId": "queued-user-decision", "text": "Queued user decision", "attachments": [], "holdForTurnEnd": true })).await.unwrap();
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(Some("Queued user decision"));
    rig.mock.release();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Queued user decision")), "queued user message dispatched").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()), "handoff must wait for queued input");
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !manager_chat(&rig).is_some_and(|chat| chat != source) {
        let handle = rig.core.doc_host.open(&source).unwrap();
        assert!(tokio::time::Instant::now() < deadline, "handoff deferred: session {:?}, turns {:?}, queue {:?}, commands {:?}, usage {:?}", rig.core.sessions.session_status(&source), rig.mock.turns(), handle.doc().read_queue(), handle.doc().read_commands(), handle.doc().context_usage());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Queued user decision") && turn.prompt.contains(&source)), "successor receives queued decision context").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_user_send_during_rotation_reaches_successor_once() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "during-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    rig.mock.hold_matching(Some("Threshold work for rotation"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Threshold work for rotation" }, "operationId": "during-threshold" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Threshold work for rotation")), "threshold turn").await;
    let launch_guard = rig.core.test_work_item_launch_guard().await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let handle = rig.core.doc_host.open(&source).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::time::timeout(Duration::from_millis(150), handle.test_queue_drain_guard()).await.is_err() { break; }
        assert!(tokio::time::Instant::now() < deadline, "handoff did not acquire the source queue gate");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    let send = rig.client.call(methods::QUEUE_MESSAGE, json!({ "chatId": source, "messageId": "during-rotation-user-message", "text": "Decision during rotation", "attachments": [], "holdForTurnEnd": true }));
    tokio::pin!(send);
    assert!(tokio::time::timeout(Duration::from_millis(150), &mut send).await.is_err(), "send must wait for rotation");
    drop(launch_guard);
    send.await.expect("send after rotation");
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "automatic successor").await;
    let successor = manager_chat(&rig).unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Decision during rotation")), "user message at successor").await;
    wait_for(|| rig.core.doc_host.open(&successor).unwrap().doc().read_entries().unwrap().iter().any(|entry| entry.id == "during-rotation-user-message"), "user message recorded in successor").await;
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Decision during rotation")).count(), 1);
    assert!(rig.core.doc_host.open(&source).unwrap().doc().read_queue().unwrap().iter().all(|row| row.id != "during-rotation-user-message"));
    assert_ne!(source, successor);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn completion_during_deferred_timer_pass_is_not_consumed() {
    let rig = rig().await;
    mark_project(&rig).await;
    let (watcher, selected, resume) = rig.core.test_start_timer_paused_manager_watcher();
    tokio::time::timeout(Duration::from_secs(5), selected).await.unwrap().unwrap();
    rig.mock.hold_matching(Some("Completion during timer selection"));
    let source = launch(&rig, &manager_params(&rig, "timer-race-manager", "Completion during timer selection")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Completion during timer selection"), "first turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| rig.core.sessions.session_status(&source).is_some_and(|session| session.status == mast_proto::SessionStatus::Idle && session.last_completed_turn.is_some()), "completion during timer pass").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    resume.send(()).unwrap();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "handoff after deferred-only pass").await;
    watcher.cancel();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_completed_chat_hands_off_after_manager_adoption() {
    let rig = rig().await;
    mark_project(&rig).await;
    rig.mock.hold_matching(Some("Before Manager adoption"));
    let source = launch(&rig, &params(&rig, "adopted-manager", "Before Manager adoption")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Before Manager adoption"), "existing chat first turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    settle(&rig, &source).await;
    tokio::time::sleep(Duration::from_millis(1250)).await;
    assert_eq!(manager_chat(&rig), None);
    rig.core.workspace.adopt_standing_role(mast_proto::StandingRoleKind::Manager, &rig.item, &source, &[]).unwrap();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "handoff after role-only adoption").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_manager_queue_does_not_stall_other_manager_handoff() {
    let rig = rig().await;
    mark_project(&rig).await;
    rig.mock.hold_matching(Some("Busy manager A"));
    let source_a = launch(&rig, &manager_params(&rig, "busy-manager-a", "Busy manager A")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Busy manager A"), "A first turn").await;
    rig.core.doc_host.open(&source_a).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    let a_handle = rig.core.doc_host.open(&source_a).unwrap();
    let a_guard = a_handle.test_queue_drain_guard().await;
    rig.mock.hold_matching(None);
    rig.mock.release();
    settle(&rig, &source_a).await;
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Another project", "operationId": "busy-project-b" })).await.unwrap();
    let project_b = derive_id("busy-project-b", roles::ITEM);
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "updateWorkItem", "workItemId": project_b, "changes": { "isProject": true }, "operationId": "busy-mark-b" })).await.unwrap();
    rig.mock.hold_matching(Some("Manager B turn"));
    let mut b = manager_params(&rig, "busy-manager-b", "Manager B turn");
    b.work_item_id = project_b.clone();
    let source_b = launch(&rig, &b).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Manager B turn"), "B first turn").await;
    rig.core.doc_host.open(&source_b).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| rig.core.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &project_b).is_some_and(|chat| chat != source_b), "B handoff while A queue is locked").await;
    drop(a_guard);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_non_manager_does_not_block_handoff_watcher() {
    let rig = rig().await;
    mark_project(&rig).await;
    let worker = launch(&rig, &params(&rig, "ordinary-worker", "Ordinary worker [hold]")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Ordinary worker [hold]"), "worker turn").await;
    let worker_handle = rig.core.doc_host.open(&worker).unwrap();
    let worker_guard = worker_handle.test_queue_drain_guard().await;
    rig.mock.release();
    wait_for(|| rig.core.sessions.session_status(&worker).is_some_and(|session| session.status == mast_proto::SessionStatus::Idle && session.last_completed_turn.is_some()), "worker completion").await;
    tokio::time::sleep(Duration::from_millis(1250)).await;
    rig.mock.hold_matching(Some("Manager after ordinary worker"));
    let source = launch(&rig, &manager_params(&rig, "after-worker-manager", "Manager after ordinary worker")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Manager after ordinary worker"), "Manager turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "Manager handoff while worker lock is held").await;
    drop(worker_guard);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_completion_before_watcher_task_starts_still_hands_off() {
    let rig = rig().await;
    mark_project(&rig).await;
    let (watcher, resume) = rig.core.test_start_paused_manager_watcher();
    rig.mock.hold_matching(Some("First delayed-watcher turn"));
    let source = launch(&rig, &manager_params(&rig, "delayed-watcher-manager", "First delayed-watcher turn")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "First delayed-watcher turn"), "first turn").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| rig.core.sessions.session_status(&source).is_some_and(|session| session.status == mast_proto::SessionStatus::Idle && session.last_completed_turn.is_some()), "first completion before watcher starts").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    let handle = rig.core.doc_host.open(&source).unwrap();
    let mut blocked = mast_doc::QueuedMessage::new("blocked-row", "Waiting for review", DEVICE);
    blocked.delivery_gate = Some(mast_doc::QueueDeliveryGate::ReviewRequired { previous_lease_id: "lease".into(), owner_device_id: DEVICE.into(), since_ms: 0, base_text_hash: "hash".into() });
    handle.doc().push_queued(&blocked).unwrap();
    resume.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(1250)).await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()), "queued input defers handoff");
    handle.doc().remove_queued("blocked-row").unwrap();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "handoff after delayed watcher start").await;
    watcher.cancel();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn coalesced_first_completion_still_triggers_manager_handoff() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source_a = launch(&rig, &manager_params(&rig, "coalesced-a", "Start Manager A")).await.unwrap().chat_id;
    settle(&rig, &source_a).await;
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Second project", "operationId": "coalesced-project-b" })).await.unwrap();
    let project_b = derive_id("coalesced-project-b", roles::ITEM);
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "updateWorkItem", "workItemId": project_b, "changes": { "isProject": true }, "operationId": "coalesced-mark-b" })).await.unwrap();
    rig.mock.hold_matching(Some("First B turn"));
    let mut b = manager_params(&rig, "coalesced-b", "First B turn");
    b.work_item_id = project_b.clone();
    let source_b = launch(&rig, &b).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("First B turn")), "B first turn").await;
    rig.core.doc_host.open(&source_b).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    let launch_guard = rig.core.test_work_item_launch_guard().await;
    rig.core.doc_host.open(&source_a).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source_a, "prompt": { "text": "Trigger A handoff" }, "operationId": "coalesced-trigger-a" })).await.unwrap();
    let a_handle = rig.core.doc_host.open(&source_a).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::time::timeout(Duration::from_millis(150), a_handle.test_queue_drain_guard()).await.is_err() { break; }
        assert!(tokio::time::Instant::now() < deadline, "A handoff did not acquire source gate");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.mock.hold_matching(None);
    rig.mock.release();
    wait_for(|| rig.core.sessions.session_status(&source_b).is_some_and(|session| session.status == mast_proto::SessionStatus::Idle && session.last_completed_turn.is_some()), "B first turn completion while watcher is blocked").await;
    assert_eq!(rig.core.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &project_b).as_deref(), Some(source_b.as_str()));
    drop(launch_guard);
    wait_for(|| rig.core.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &project_b).is_some_and(|chat| chat != source_b), "B successor without another user turn").await;
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt == "First B turn").count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_continue_during_manager_rotation_reaches_successor() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "idle-race-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    rig.mock.hold_matching(Some("Threshold for idle continuation"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Threshold for idle continuation" }, "operationId": "idle-race-trigger" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Threshold for idle continuation")), "threshold turn").await;
    let launch_guard = rig.core.test_work_item_launch_guard().await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let handle = rig.core.doc_host.open(&source).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::time::timeout(Duration::from_millis(150), handle.test_queue_drain_guard()).await.is_err() { break; }
        assert!(tokio::time::Instant::now() < deadline, "handoff did not acquire source gate");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let followup = json!({ "chatId": source, "prompt": { "text": "Idle request during rotation" }, "operationId": "idle-race-followup" });
    let send = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, followup.clone());
    let duplicate = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, followup.clone());
    tokio::pin!(send);
    tokio::pin!(duplicate);
    assert!(tokio::time::timeout(Duration::from_millis(150), &mut send).await.is_err());
    assert!(tokio::time::timeout(Duration::from_millis(150), &mut duplicate).await.is_err());
    drop(launch_guard);
    let (reply, duplicate) = tokio::join!(send, duplicate);
    let reply = reply.unwrap();
    let duplicate = duplicate.unwrap();
    let successor = manager_chat(&rig).unwrap();
    assert_ne!(source, successor);
    assert_eq!(reply["chatId"], successor);
    assert_eq!(duplicate["chatId"], successor);
    assert_eq!(reply["commandId"], duplicate["commandId"]);
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Idle request during rotation")), "successor follow-up").await;
    let replay = rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, followup).await.unwrap();
    assert_eq!(replay["chatId"], successor);
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Idle request during rotation")).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_start_during_manager_rotation_uses_successor_parent() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "scheduled-race-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    let draft = mast_proto::ScheduledTaskDraft {
        name: "Scheduled during handoff".into(), prompt: "Execute scheduled rotation task".into(),
        cwd: rig.repo.to_string_lossy().into_owned(), space_id: Some(SPACE.into()),
        config: params(&rig, "unused", "unused").config.unwrap(),
        schedule: mast_proto::TaskSchedule::Cron { expression: "0 8 * * 1-5".into(), timezone: "UTC".into() },
        enabled: false, grace_minutes: 360, cutover_confirmed: false, work_item_id: Some(rig.item.clone()),
        output: Default::default(), after_success: Default::default(), write_to_notion: None, chat_mode: Default::default(), refresh: Default::default(),
    };
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::Save { id: "rotation-task".into(), expected_revision: 0, draft, operation_id: "save-rotation-task".into() }).await.unwrap();
    rig.mock.hold_matching(Some("Threshold for scheduled rotation"));
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Threshold for scheduled rotation" }, "operationId": "scheduled-race-trigger" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Threshold for scheduled rotation")), "threshold turn").await;
    let launch_guard = rig.core.test_work_item_launch_guard().await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.mock.hold_matching(None);
    rig.mock.release();
    let source_handle = rig.core.doc_host.open(&source).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::time::timeout(Duration::from_millis(150), source_handle.test_queue_drain_guard()).await.is_err() { break; }
        assert!(tokio::time::Instant::now() < deadline, "handoff did not acquire source gate");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "rotation-task".into(), operation_id: "run-rotation-task".into() }).await.unwrap();
    let tick = rig.core.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis());
    tokio::pin!(tick);
    assert!(tokio::time::timeout(Duration::from_millis(150), &mut tick).await.is_err(), "scheduled start waits for handoff");
    drop(launch_guard);
    tick.await.unwrap();
    wait_for(|| manager_chat(&rig).is_some_and(|chat| chat != source), "automatic successor").await;
    let successor = manager_chat(&rig).unwrap();
    let snapshot = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    let child = snapshot.runs.iter().find(|run| run.task_id == "rotation-task").unwrap().chat_id.as_deref().unwrap();
    assert_eq!(rig.core.workspace.chat(child).unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Execute scheduled rotation task")), "scheduled turn").await;
    let report = rig.core.orchestrator_wakes.report_to_parent(child, "scheduled rotation complete").await.unwrap();
    assert_eq!(report.parent_chat_id, successor);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_run_started_before_handoff_keeps_one_turn_and_new_parent() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "running-task-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt == "Start managing"), "Manager first turn").await;
    settle(&rig, &source).await;
    let draft = mast_proto::ScheduledTaskDraft {
        name: "Running task".into(), prompt: "Run across Manager handoff [hold]".into(),
        cwd: rig.repo.to_string_lossy().into_owned(), space_id: Some(SPACE.into()),
        config: params(&rig, "unused", "unused").config.unwrap(),
        schedule: mast_proto::TaskSchedule::Cron { expression: "0 8 * * 1-5".into(), timezone: "UTC".into() },
        enabled: false, grace_minutes: 360, cutover_confirmed: false, work_item_id: Some(rig.item.clone()),
        output: Default::default(), after_success: Default::default(), write_to_notion: None, chat_mode: Default::default(), refresh: Default::default(),
    };
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::Save { id: "running-task".into(), expected_revision: 0, draft, operation_id: "save-running-task".into() }).await.unwrap();
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "running-task".into(), operation_id: "run-running-task".into() }).await.unwrap();
    rig.core.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis()).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Run across Manager handoff")), "scheduled task start").await;
    let snapshot = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    let child = snapshot.runs.iter().find(|run| run.task_id == "running-task").unwrap().chat_id.clone().unwrap();
    assert_eq!(rig.core.workspace.chat(&child).unwrap().unwrap().parent_chat_id.as_deref(), Some(source.as_str()));
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(92), Some(100)).unwrap();
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Rotate while task runs" }, "operationId": "running-task-trigger" })).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !manager_chat(&rig).is_some_and(|chat| chat != source) {
        assert!(tokio::time::Instant::now() < deadline, "no successor: source session {:?}, child session {:?}, turns {:?}, source queue {:?}, usage {:?}, role {:?}", rig.core.sessions.session_status(&source), rig.core.sessions.session_status(&child), rig.mock.turns(), rig.core.doc_host.open(&source).unwrap().doc().read_queue(), rig.core.doc_host.open(&source).unwrap().doc().context_usage(), rig.core.workspace.standing_role_of_chat(&source));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let successor = manager_chat(&rig).unwrap();
    assert_eq!(rig.core.workspace.chat(&child).unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "running-task".into(), operation_id: "run-running-task".into() }).await.unwrap();
    let replay = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    assert_eq!(replay.runs.iter().filter(|run| run.task_id == "running-task").count(), 1);
    assert_eq!(rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Run across Manager handoff")).count(), 1);
    assert_eq!(rig.core.orchestrator_wakes.report_to_parent(&child, "task remains active").await.unwrap().parent_chat_id, successor);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_shutdown_cancels_wait_for_handoff_gate() {
    let rig = rig().await;
    let draft = mast_proto::ScheduledTaskDraft {
        name: "Cancelled scheduled start".into(), prompt: "Resume scheduled task".into(),
        cwd: rig.repo.to_string_lossy().into_owned(), space_id: Some(SPACE.into()),
        config: params(&rig, "unused", "unused").config.unwrap(),
        schedule: mast_proto::TaskSchedule::Cron { expression: "0 8 * * 1-5".into(), timezone: "UTC".into() },
        enabled: false, grace_minutes: 360, cutover_confirmed: false, work_item_id: Some(rig.item.clone()),
        output: Default::default(), after_success: Default::default(), write_to_notion: None, chat_mode: Default::default(), refresh: Default::default(),
    };
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::Save { id: "cancel-task".into(), expected_revision: 0, draft, operation_id: "save-cancel-task".into() }).await.unwrap();
    rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::RunNow { id: "cancel-task".into(), operation_id: "run-cancel-task".into() }).await.unwrap();
    {
        let guard = rig.core.test_work_item_launch_guard().await;
        let tick = rig.core.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis());
        tokio::pin!(tick);
        assert!(tokio::time::timeout(Duration::from_millis(150), &mut tick).await.is_err());
        let shutdown = rig.core.scheduled_tasks.shutdown();
        tokio::pin!(shutdown);
        assert!(tokio::time::timeout(Duration::from_millis(150), &mut shutdown).await.is_err(), "shutdown waits for the active scheduler tick");
        // Cancellation and gate availability are both ready when tick is
        // polled again; neither select ordering may dispatch the occurrence.
        drop(guard);
        let (tick_result, _) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(tick, shutdown) }).await.expect("shutdown must cancel gate wait");
        tick_result.unwrap();
        assert!(!rig.mock.turns().iter().any(|turn| turn.prompt.contains("Resume scheduled task")));
    }
    let snapshot = rig.core.scheduled_tasks.request(mast_proto::ScheduledTasksRequest::List).await.unwrap();
    let pending = snapshot.runs.iter().find(|run| run.task_id == "cancel-task").unwrap();
    assert_eq!(pending.state, mast_proto::ScheduledRunState::Launching);
    if let Some(chat) = pending.chat_id.as_deref() { assert!(rig.core.workspace.chat(chat).unwrap().is_none(), "no chat is created before cancelled dispatch"); }
    let Rig { _tmp, core, mock, client, root, .. } = rig;
    core.shutdown().await;
    drop(client);
    drop(core);
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let reopened = EngineCore::assemble(&root.join("data"), Arc::new(harnesses), HarnessId::Mock, None).unwrap();
    reopened.scheduled_tasks.tick(chrono::Utc::now().timestamp_millis()).await.unwrap();
    wait_for(|| mock.turns().iter().any(|turn| turn.prompt.contains("Resume scheduled task")), "scheduled turn after restart").await;
    assert_eq!(mock.turns().iter().filter(|turn| turn.prompt.contains("Resume scheduled task")).count(), 1);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_send_allows_rowless_chat_but_rejects_incomplete_handoff() {
    let rig = rig().await;
    let rowless = "document-only-send";
    assert!(rig.core.workspace.chat(rowless).unwrap().is_none());
    rig.client.call(methods::QUEUE_MESSAGE, json!({ "chatId": rowless, "messageId": "rowless-message", "text": "Document-only message", "attachments": [], "holdForTurnEnd": true })).await.expect("rowless send remains supported");

    let source = "incomplete-handoff-source";
    rig.core.workspace.create_chat(source, Some(SPACE), None, None, None).unwrap();
    let mut row = rig.core.workspace.chat(source).unwrap().unwrap();
    row.standing_role_successor = Some("missing-successor".into());
    rig.core.workspace.import_chat_row(&row).unwrap();
    let send = json!({ "chatId": source, "messageId": "incomplete-message", "text": "Do not deliver", "attachments": [], "holdForTurnEnd": true });
    assert!(rig.client.call(methods::QUEUE_MESSAGE, send.clone()).await.is_err(), "missing successor must fail closed");

    rig.core.workspace.create_provisional_role_chat("pending-successor", SPACE, None, rig.repo.to_string_lossy().into_owned(), mast_proto::standing_role_provisional_tag(mast_proto::StandingRoleKind::Manager, &rig.item)).unwrap();
    row.standing_role_successor = Some("pending-successor".into());
    rig.core.workspace.import_chat_row(&row).unwrap();
    assert!(rig.client.call(methods::QUEUE_MESSAGE, send).await.is_err(), "provisional successor must fail closed");
    assert!(rig.core.doc_host.open(source).unwrap().doc().read_queue().unwrap().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_unknown_context_window_explains_manual_handoff() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "unknown-window-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    rig.client.call(methods::CONTINUE_CHAT_FOR_WORK_ITEM, json!({ "chatId": source, "prompt": { "text": "Work without a reported window [hold]" }, "operationId": "unknown-window-turn" })).await.unwrap();
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Work without a reported window")), "turn start").await;
    rig.core.doc_host.open(&source).unwrap().doc().update_context_usage(Some(200_000), None).unwrap();
    rig.mock.release();
    wait_for(|| rig.core.doc_host.open(&source).unwrap().doc().read_entries().unwrap().iter().any(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("did not report its context window")))), "visible unknown window explanation").await;
    let entries = rig.core.doc_host.open(&source).unwrap().doc().read_entries().unwrap();
    assert!(entries.iter().any(|entry| entry.role == MessageRole::System && entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("did not report its context window")))));
    assert_eq!(manager_chat(&rig).as_deref(), Some(source.as_str()));
    rig.core.shutdown().await;
}

fn chat_count(rig: &Rig) -> usize {
    rig.core.workspace.work_outline_inputs(SPACE).chats.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn first_manager_message_creates_the_chat_and_the_snapshot_shows_the_role() {
    let rig = rig().await;
    mark_project(&rig).await;
    assert_eq!(manager_chat(&rig), None, "nothing exists before the first message");
    assert_eq!(chat_count(&rig), 0);

    let reply = launch(&rig, &manager_params(&rig, "manager-first", "Plan the launch"))
        .await
        .unwrap();

    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    let chat = rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap();
    assert!(chat.standing_role_provisional().is_none(), "activated: no longer hidden");
    rig.core.doc_host.persist_chat_now(&reply.chat_id).expect("the chat saves");
    let role = rig
        .core
        .workspace
        .standing_role(mast_proto::StandingRoleKind::Manager, &rig.item)
        .unwrap();
    assert_eq!(role.chain, [reply.chat_id.clone()]);
    assert_eq!(
        row_field(&rig, KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &reply.chat_id, "workItemId"),
        Some(json!(rig.item)),
        "the Manager chat sits under the project item"
    );
    let snapshot = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    let shown = snapshot
        .standing_role(mast_proto::StandingRoleKind::Manager, &rig.item)
        .expect("the outline snapshot carries the role for the Manager row");
    assert_eq!(shown.current_chat_id.as_deref(), Some(reply.chat_id.as_str()));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_first_start_hands_off_to_one_successor_without_archiving() {
    let rig = rig().await;
    mark_project(&rig).await;
    let first = manager_params(&rig, "manager-before-handoff", "Start managing");
    let source = launch(&rig, &first).await.unwrap().chat_id;
    let params = json!({
        "chatId": source, "summary": "Continue managing", "archive": false,
        "operationId": "manager-successor",
    });
    let reply = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, params.clone())
        .await
        .unwrap();
    let successor = reply["chatId"].as_str().unwrap();
    let workspace = &rig.core.workspace;
    let predecessor = workspace.chat(&source).unwrap().unwrap();
    assert!(!predecessor.archived);
    assert_eq!(predecessor.standing_role_successor.as_deref(), Some(successor));
    assert_eq!(manager_chat(&rig).as_deref(), Some(successor));
    assert!(workspace.chat(successor).unwrap().unwrap().standing_role_provisional().is_none());

    assert!(launch(&rig, &first).await.is_err());
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params).await.unwrap();
    assert_eq!(retry["chatId"], successor);
    assert_eq!(chat_count(&rig), 2);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Messages that reach a handed-off Manager after its hand-off
// ---------------------------------------------------------------------------

fn left_behind(id: &str, text: &str) -> mast_doc::QueuedMessage {
    mast_doc::QueuedMessage {
        id: id.into(),
        text: text.into(),
        attachments: Vec::new(),
        // Waits for the successor's turn to end, so the row is observed in its queue or transcript.
        hold_for_turn_end: true,
        issued_by: "dev-b".into(),
        issued_at: 1,
        edited_at: None,
        delivery_gate: None,
    }
}

/// How many transcript entries of `chat` carry message `id`.
fn entries_with(core: &EngineCore, chat: &str, id: &str) -> usize {
    core.doc_host.open(chat).unwrap().doc().read_entries().unwrap().iter().filter(|entry| entry.id == id).count()
}

fn queue_ids(core: &EngineCore, chat: &str) -> Vec<String> {
    core.doc_host.open(chat).unwrap().doc().read_queue().unwrap().into_iter().map(|row| row.id).collect()
}

fn mock_turns_with(mock: &InstructionMockHarness, text: &str) -> usize {
    mock.turns().iter().filter(|turn| turn.prompt.contains(text)).count()
}

/// A Manager, settled and holding the role, handed off by hand to a settled successor.
async fn handed_off_manager(rig: &Rig, name: &str) -> (String, String) {
    mark_project(rig).await;
    let source = launch(rig, &manager_params(rig, &format!("{name}-manager"), "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(rig, &source).await;
    let successor = hand_off_by_hand(rig, &source, &format!("{name}-handoff")).await;
    (source, successor)
}

async fn hand_off_by_hand(rig: &Rig, source: &str, operation: &str) -> String {
    let reply = tokio::time::timeout(Duration::from_secs(15), rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": source, "summary": "Continue managing", "archive": true, "operationId": operation,
    }))).await.expect("the manual hand-off does not wait on the source queue").unwrap();
    let successor = reply["chatId"].as_str().unwrap().to_owned();
    wait_for(|| manager_chat(rig).as_deref() == Some(successor.as_str()), "the successor to hold the role").await;
    settle(rig, &successor).await;
    successor
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_handoff_forwards_a_message_left_on_the_previous_manager_once() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "left-behind-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    settle(&rig, &source).await;
    // Another device's row is in the source queue when the hand-off commits:
    // the manual hand-off does not take the source's queue lock the way the
    // automatic one does, so nothing has looked at the row.
    let handle = rig.core.doc_host.open(&source).unwrap();
    let guard = handle.test_queue_drain_guard().await;
    handle.doc().push_queued(&left_behind("left-behind-message", "Decide the release date")).unwrap();
    let successor = hand_off_by_hand(&rig, &source, "left-behind-handoff").await;
    assert_eq!(queue_ids(&rig.core, &source), vec!["left-behind-message".to_string()], "still held by the lock");
    drop(guard);

    wait_for(|| entries_with(&rig.core, &successor, "left-behind-message") == 1, "the message in the successor's transcript").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(entries_with(&rig.core, &successor, "left-behind-message"), 1);
    assert_eq!(entries_with(&rig.core, &source, "left-behind-message"), 0, "the old Manager never ran it");
    assert!(queue_ids(&rig.core, &source).is_empty());
    assert_eq!(mock_turns_with(&rig.mock, "Decide the release date"), 1);

    // A manual retry of the hand-off (same operation) changes nothing for it.
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": source, "summary": "Continue managing", "archive": true, "operationId": "left-behind-handoff",
    })).await.unwrap();
    assert_eq!(retry["chatId"], successor.as_str());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock_turns_with(&rig.mock, "Decide the release date"), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_synced_late_to_the_previous_manager_reaches_the_successor_once_across_a_retry() {
    let rig = rig().await;
    let (source, successor) = handed_off_manager(&rig, "late-sync").await;
    // Another device wrote the row while offline; it merges into the old
    // chat's doc after the hand-off, bypassing every send path.
    rig.core.doc_host.open(&source).unwrap().doc()
        .push_queued(&left_behind("late-sync-message", "Late synced question")).unwrap();
    wait_for(|| entries_with(&rig.core, &successor, "late-sync-message") == 1, "the late message at the successor").await;

    // Its client never saw an acknowledgement, so it sends the same message
    // (same id) to the old chat again: the send follows the hand-off and
    // finds the message already delivered.
    rig.client.call(methods::QUEUE_MESSAGE, json!({
        "chatId": source, "messageId": "late-sync-message", "text": "Late synced question",
        "attachments": [], "holdForTurnEnd": true,
    })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(entries_with(&rig.core, &successor, "late-sync-message"), 1);
    assert_eq!(entries_with(&rig.core, &source, "late-sync-message"), 0);
    assert!(queue_ids(&rig.core, &source).is_empty() && queue_ids(&rig.core, &successor).is_empty());
    assert_eq!(mock_turns_with(&rig.mock, "Late synced question"), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_left_on_the_previous_manager_survives_a_restart_and_reaches_the_successor_once() {
    let rig = rig().await;
    let (source, successor) = handed_off_manager(&rig, "restart-left").await;
    // The forward is held, as if the host stopped before reaching the row.
    rig.core.sessions.agent_tokens().manager_status()
        .set_test_hook(Some(Arc::new(|event, _| event == "handoff_queue_forward")));
    rig.core.doc_host.open(&source).unwrap().doc()
        .push_queued(&left_behind("restart-left-message", "Question across a restart")).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(queue_ids(&rig.core, &source), vec!["restart-left-message".to_string()]);
    rig.core.doc_host.persist_chat_now(&source).unwrap();
    rig.core.workspace.persist_registry_now().unwrap();

    let Rig { _tmp, core, mock, client, root, .. } = rig;
    drop(client);
    core.shutdown().await;
    drop(core);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();
    // Nobody opens the old chat again; the host does, and the message moves.
    wait_for(|| entries_with(&restarted, &successor, "restart-left-message") == 1, "the message at the successor after restart").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(entries_with(&restarted, &successor, "restart-left-message"), 1);
    assert_eq!(entries_with(&restarted, &source, "restart-left-message"), 0);
    assert!(queue_ids(&restarted, &source).is_empty());
    assert_eq!(mock_turns_with(&mock, "Question across a restart"), 1);
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_left_on_a_twice_handed_off_manager_reaches_the_latest_successor_once() {
    let rig = rig().await;
    let (source, first) = handed_off_manager(&rig, "twice").await;
    // A different hand-off operation on the successor extends the chain.
    let second = hand_off_by_hand(&rig, &first, "twice-second-handoff").await;
    assert_ne!(first, second);
    rig.core.doc_host.open(&source).unwrap().doc()
        .push_queued(&left_behind("twice-message", "Question for the newest Manager")).unwrap();
    wait_for(|| entries_with(&rig.core, &second, "twice-message") == 1, "the message at the newest successor").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(entries_with(&rig.core, &second, "twice-message"), 1);
    assert_eq!(entries_with(&rig.core, &first, "twice-message"), 0);
    assert_eq!(entries_with(&rig.core, &source, "twice-message"), 0);
    assert_eq!(mock_turns_with(&rig.mock, "Question for the newest Manager"), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_held_for_an_edit_stays_put_until_the_edit_ends_and_the_successor_is_told() {
    let rig = rig().await;
    let (source, successor) = handed_off_manager(&rig, "held-edit").await;
    let mut row = left_behind("held-edit-message", "Half-edited question");
    row.delivery_gate = Some(mast_doc::QueueDeliveryGate::ReviewRequired {
        previous_lease_id: "lease".into(), owner_device_id: "dev-b".into(), since_ms: 1, base_text_hash: "hash".into(),
    });
    let handle = rig.core.doc_host.open(&source).unwrap();
    handle.doc().push_queued(&row).unwrap();
    wait_for(|| rig.core.doc_host.open(&successor).unwrap().doc().read_entries().unwrap().iter().any(|entry| entry.parts.iter()
        .any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains("waiting on an open edit or review")))),
        "the successor to be told about the held message").await;
    assert_eq!(queue_ids(&rig.core, &source), vec!["held-edit-message".to_string()]);
    assert_eq!(entries_with(&rig.core, &successor, "held-edit-message"), 0);
    assert_eq!(entries_with(&rig.core, &source, "held-edit-message"), 0, "the old Manager does not run it either");

    // The user finishes the edit: the edited text, not the held one, moves.
    handle.doc().set_queued_text("held-edit-message", "Finished question", 2).unwrap();
    handle.doc().set_queued_delivery_gate("held-edit-message", None).unwrap();
    wait_for(|| entries_with(&rig.core, &successor, "held-edit-message") == 1, "the edited message at the successor").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(mock_turns_with(&rig.mock, "Finished question"), 1);
    assert_eq!(mock_turns_with(&rig.mock, "Half-edited question"), 0);
    assert!(queue_ids(&rig.core, &source).is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_the_successor_already_claimed_is_not_forwarded_a_second_time() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let rig = rig().await;
    let (source, successor) = handed_off_manager(&rig, "claimed").await;
    // The successor took the message for delivery and its host died before
    // dispatching it: only the durable claim is left. The old chat still has
    // the same row (a retry or another device wrote it there too).
    let hold = Arc::new(AtomicBool::new(true));
    let crash = Arc::new(AtomicBool::new(true));
    {
        let (hold, crash, successor) = (hold.clone(), crash.clone(), successor.clone());
        rig.core.sessions.agent_tokens().manager_status().set_test_hook(Some(Arc::new(move |event, chat| match event {
            "handoff_queue_forward" => hold.load(Ordering::Acquire),
            "queued_delivery_crash_after_take" => crash.load(Ordering::Acquire) && chat == successor,
            _ => false,
        })));
    }
    let target = rig.core.doc_host.open(&successor).unwrap();
    target.doc().push_queued(&left_behind("claimed-message", "Already claimed question")).unwrap();
    wait_for(|| target.doc().delivery_claims().unwrap().len() == 1, "the successor's claim").await;
    rig.core.doc_host.open(&source).unwrap().doc()
        .push_queued(&left_behind("claimed-message", "Already claimed question")).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(queue_ids(&rig.core, &source), vec!["claimed-message".to_string()]);

    // The host comes back: the forward runs before the successor's own recovery.
    crash.store(false, Ordering::Release);
    hold.store(false, Ordering::Release);
    rig.core.doc_host.open(&source).unwrap().doc()
        .push_queued(&left_behind("claimed-poke", "Poke the forward")).unwrap();
    wait_for(|| queue_ids(&rig.core, &source).is_empty(), "the old chat's queue to be forwarded").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // The claimed message is recovered once, at the head behind its review
    // gate (so the poke waits behind it), and is never run twice.
    let rows = target.doc().read_queue().unwrap();
    assert!(rows.iter().any(|row| row.id == "claimed-poke"), "the rest of the queue was forwarded");
    assert_eq!(rows.iter().filter(|row| row.id == "claimed-message").count(), 1);
    assert!(rows.iter().all(|row| row.id != "claimed-message" || row.delivery_gate.is_some()));
    assert_eq!(entries_with(&rig.core, &successor, "claimed-message"), 0);
    assert_eq!(mock_turns_with(&rig.mock, "Already claimed question"), 0);
    assert!(target.doc().delivery_claims().unwrap().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn handoff_successor_keeps_the_renamed_source_title_across_retry_and_restart() {
    let rig = rig().await;
    let source = launch(&rig, &params(&rig, "title-source", "Start")).await.unwrap().chat_id;
    settle(&rig, &source).await;
    rig.core.workspace.rename_chat(&source, "My renamed chat").unwrap();
    let handoff = json!({
        "chatId": source, "summary": "Continue", "archive": true, "operationId": "title-successor",
    });
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, handoff.clone()).await.unwrap();
    let successor = reply["chatId"].as_str().unwrap().to_owned();
    let title = |core: &EngineCore, id: &str| core.workspace.chat(id).unwrap().unwrap().title;
    assert_eq!(title(&rig.core, &successor).as_deref(), Some("My renamed chat"));

    // A lost reply retried with the same operation, after the user renames the
    // source again, neither re-copies nor overwrites the successor's title.
    rig.core.workspace.rename_chat(&source, "Renamed later").unwrap();
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, handoff).await.unwrap();
    assert_eq!(retry["chatId"], successor.as_str());
    assert_eq!(title(&rig.core, &successor).as_deref(), Some("My renamed chat"));

    rig.core.workspace.persist_registry_now().unwrap();
    let Rig { _tmp, core, mock, client, root, .. } = rig;
    drop(client);
    core.shutdown().await;
    drop(core);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(mock.clone()));
    let restarted = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::Mock, None).unwrap();
    assert_eq!(title(&restarted, &successor).as_deref(), Some("My renamed chat"));
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn handoff_from_a_renamed_worker_keeps_its_title_over_the_item_name() {
    let rig = rig().await;
    let parent = launch(&rig, &params(&rig, "worker-title-parent", "Start")).await.unwrap().chat_id;
    let worker = launch(&rig, &CreateChatForWorkItemParams {
        parent_chat_id: Some(parent),
        ..params(&rig, "worker-title-source", "Work")
    }).await.unwrap().chat_id;
    settle(&rig, &worker).await;
    rig.core.workspace.rename_chat(&worker, "My renamed worker").unwrap();
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": worker, "summary": "Continue", "archive": true, "operationId": "worker-title-successor",
    })).await.unwrap();
    let successor = rig.core.workspace.chat(reply["chatId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(successor.title.as_deref(), Some("My renamed worker"));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn project_document_inventory_remains_available_after_manager_handoff() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "inventory-manager", "Manage project"))
        .await.unwrap().chat_id;
    let initial: serde_json::Value = rig.client.call(methods::PROJECT_DOCUMENT_INVENTORY,
        json!({"spaceId": SPACE, "projectId": rig.item, "scan": true})).await.unwrap();
    rig.client.call(methods::HAND_OFF_ORCHESTRATOR,
        json!({"chatId": source, "summary": "Continue managing", "archive": false, "operationId": "inventory-manager-successor"})).await.unwrap();
    let fresh = mast_rpc::memory_client(rig.core.rpc_service());
    let after: serde_json::Value = fresh.call(methods::READ_PROJECT_DOCUMENT_INVENTORY,
        json!({"spaceId": SPACE, "projectId": rig.item})).await.unwrap();
    assert_eq!(after["inventory"]["taskId"], initial["taskId"]);
    assert!(after["inventory"]["pending"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("out of date")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_manager_starts_and_retries_never_create_a_second_chat() {
    let rig = rig().await;
    mark_project(&rig).await;
    let first = manager_params(&rig, "manager-dup", "Plan the launch");
    let reply = launch(&rig, &first).await.unwrap();

    // A transport retry (same operation id and inputs) returns the same chat.
    let retry = launch(&rig, &first).await.unwrap();
    assert_eq!(retry.chat_id, reply.chat_id);

    // A second click in another dialog (new operation id) is refused, not
    // adopted over, and creates nothing.
    let second = launch(&rig, &manager_params(&rig, "manager-dup-2", "Plan it again"))
        .await
        .unwrap_err();
    assert_eq!(second.code, WorkOutlineErrorCode::OperationConflict);
    assert!(second.message.contains("already started"), "{}", second.message);
    assert!(second.message.contains("wasn't sent"), "{}", second.message);

    assert_eq!(chat_count(&rig), 1);
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("manager-dup-2", roles::CHAT))
            .unwrap()
            .is_none()
    );
    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    let role = rig
        .core
        .workspace
        .standing_role(mast_proto::StandingRoleKind::Manager, &rig.item)
        .unwrap();
    assert_eq!(role.chain.len(), 1);

    // The operation id is bound to the role: replaying it as a plain launch
    // is a conflict rather than a second, unadopted chat.
    let plain = launch(&rig, &params(&rig, "manager-dup", "Plan the launch"))
        .await
        .unwrap_err();
    assert_eq!(plain.code, WorkOutlineErrorCode::OperationConflict);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_around_role_adoption_then_retry_yields_one_chat_holding_the_role() {
    for (index, step) in [LaunchStep::Assigned, LaunchStep::RoleAdopted, LaunchStep::TurnQueued]
        .into_iter()
        .enumerate()
    {
        let rig = rig().await;
        mark_project(&rig).await;
        let launcher = service(&rig);
        let op = format!("manager-crash-{index}");
        let prompt = format!("manager crash {index}");
        let p = manager_params(&rig, &op, &prompt);
        launcher.inject_fault(Some(step));
        assert!(launcher.create_chat_for_work_item(&p).await.is_err(), "{step:?} must fail");

        let reply = launcher
            .create_chat_for_work_item(&p)
            .await
            .unwrap_or_else(|e| panic!("retry after {step:?}: {e}"));
        wait_for(|| turns_with(&rig, &prompt).len() == 1, "the launched turn").await;
        launcher.create_chat_for_work_item(&p).await.unwrap();

        assert_eq!(chat_count(&rig), 1, "one chat after {step:?}");
        assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
        let role = rig
            .core
            .workspace
            .standing_role(mast_proto::StandingRoleKind::Manager, &rig.item)
            .unwrap();
        assert_eq!(role.chain, [reply.chat_id.clone()], "adopted once after {step:?}");
        assert_eq!(turns_with(&rig, &prompt).len(), 1, "one Run after {step:?}");
        rig.core.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_launch_for_a_plain_item_is_refused_before_anything_is_written() {
    let rig = rig().await;
    let error = launch(&rig, &manager_params(&rig, "manager-plain", "Plan"))
        .await
        .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Invalid);
    assert!(error.message.contains("Only a project"), "{}", error.message);
    assert_eq!(chat_count(&rig), 0);
    assert_eq!(manager_chat(&rig), None);
    rig.core.shutdown().await;
}

fn provisional_chats(rig: &Rig) -> Vec<mast_proto::Chat> {
    rig.core
        .workspace
        .provisional_role_chats(mast_proto::StandingRoleKind::Manager, &rig.item)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unstarted_first_chat_is_hidden_takes_no_message_and_does_not_block_a_new_start() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::Assigned));
    let a = manager_params(&rig, "manager-hide-a", "first try");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());

    // Hidden wherever superseded role chats are hidden, and not the Manager.
    let hidden = provisional_chats(&rig);
    assert_eq!(hidden.len(), 1);
    assert!(hidden[0].is_standing_role_predecessor());
    assert_eq!(manager_chat(&rig), None);
    // An ordinary message cannot reach it.
    assert!(rig.core.doc_host.queue_message(&hidden[0].id, "hello", Vec::new()).is_err());

    // A later start makes its own chat and becomes the Manager; the abandoned
    // one stays hidden and never runs anything.
    let b = manager_params(&rig, "manager-hide-b", "second try");
    let reply = launcher.create_chat_for_work_item(&b).await.unwrap();
    assert_ne!(reply.chat_id, hidden[0].id);
    wait_for(|| turns_with(&rig, "second try").len() == 1, "the second message").await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    assert_eq!(provisional_chats(&rig).len(), 1, "the abandoned chat stays hidden");
    assert!(turns_with(&rig, "first try").is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_unstarted_chat_does_not_block_a_restart() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::ChatCreated));
    let a = manager_params(&rig, "manager-del-a", "first try");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    let doomed = provisional_chats(&rig).remove(0).id;
    rig.client
        .call(methods::MUTATE, json!({ "op": "deleteChat", "chatId": doomed }))
        .await
        .unwrap();

    let b = manager_params(&rig, "manager-del-b", "second try");
    let reply = launcher.create_chat_for_work_item(&b).await.unwrap();
    assert_ne!(reply.chat_id, doomed);
    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_queued_its_message_is_surfaced_by_the_next_attempt_which_sends_nothing() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::TurnQueued));
    let a = manager_params(&rig, "manager-rec-a", "queued before the stop");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    wait_for(|| turns_with(&rig, "queued before the stop").len() == 1, "the queued turn").await;
    assert_eq!(manager_chat(&rig), None, "queued but not yet the Manager");
    let first = provisional_chats(&rig).remove(0).id;

    // A different first message is refused, but the chat that is already
    // running becomes the visible Manager, and the new message is not sent.
    let b = manager_params(&rig, "manager-rec-b", "a different message");
    let refused = launcher.create_chat_for_work_item(&b).await.unwrap_err();
    assert_eq!(refused.code, WorkOutlineErrorCode::OperationConflict);
    assert!(refused.message.contains("wasn't sent"), "{}", refused.message);
    assert_eq!(manager_chat(&rig).as_deref(), Some(first.as_str()));
    assert!(provisional_chats(&rig).is_empty());
    assert_eq!(chat_count(&rig), 1);
    assert_eq!(user_messages(&rig, &first), 1);
    assert!(turns_with(&rig, "a different message").is_empty());
    // The recovery's activation is on disk when the refusal returns.
    let disk = registry_on_disk(&rig);
    assert_eq!(
        disk.standing_role_chat(mast_proto::StandingRoleKind::Manager, &rig.item).as_deref(),
        Some(first.as_str())
    );
    assert!(disk.chat(&first).unwrap().unwrap().standing_role_provisional().is_none());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_recovery_of_a_queued_start_survives_an_engine_restart() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::TurnQueued));
    let a = manager_params(&rig, "manager-rst-a", "queued before the restart");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    let first = provisional_chats(&rig).remove(0).id;
    let b = manager_params(&rig, "manager-rst-b", "after the restart");
    rig.core.workspace.flush();
    // Drop the first engine so its data-dir lock is released, as a restart does.
    let old_core = rig.core;
    old_core.shutdown().await;
    drop(old_core);
    drop(launcher);

    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(rig.mock.clone()));
    let core = EngineCore::assemble(&rig.root.join("data"), Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core reassembles");
    assert!(
        core.doc_host
            .has_launch_command(&first, &derive_id("manager-rst-a", roles::COMMAND))
            .unwrap(),
        "the queued message was saved"
    );
    let launcher = WorkItemLaunchService::new(
        core.workspace.clone(),
        core.doc_host.clone(),
        core.sessions.clone(),
        ExecutionScopeService::new(core.workspace.clone(), core.repos.clone()),
    );
    let refused = launcher.create_chat_for_work_item(&b).await.unwrap_err();
    assert_eq!(refused.code, WorkOutlineErrorCode::OperationConflict);
    assert_eq!(
        core.workspace
            .standing_role_chat(mast_proto::StandingRoleKind::Manager, &rig.item)
            .as_deref(),
        Some(first.as_str())
    );
    core.shutdown().await;
}

/// The directory holding the engine's `docs.sqlite3`, wherever the profile
/// layout put it under the rig's temp tree.
fn find_docs_dir(root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_docs_dir(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "docs.sqlite3") {
            return path.parent().map(Path::to_path_buf);
        }
    }
    None
}

fn registry_on_disk(rig: &Rig) -> mast_doc::RegistryDoc {
    let dir = find_docs_dir(&rig.root).expect("the engine's docs database under the rig");
    let store = mast_sync::DocsStore::open(&dir).expect("a second reader opens the database");
    let bytes = store
        .load_snapshot(mast_doc::REGISTRY_DOC_ID)
        .unwrap()
        .expect("a saved registry snapshot");
    mast_doc::RegistryDoc::from_bytes(&bytes, "disk-reader").expect("the snapshot decodes")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisional_chat_takes_no_continue_queue_or_relayed_message() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::Assigned));
    let a = manager_params(&rig, "manager-intake-a", "first try");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    let chat = provisional_chats(&rig).remove(0).id;

    // The work-item continue path.
    assert!(continue_chat(&rig, &chat, "continue-op", "sneak in").await.is_err());
    // A direct queue with a caller-chosen command id (the path agents and
    // scheduled runs use).
    let payload = SessionCommandPayload::Run {
        request: run_request("sneak in", "/tmp"),
        message_id: "sneak-message".into(),
    };
    assert!(rig
        .core
        .doc_host
        .queue_command_with_id(&chat, "sneak-command", payload.clone())
        .is_err());
    // A command relayed from another device.
    let relayed = SessionCommandEntry {
        id: "relay-command".into(),
        payload,
        issued_by: "dev-b".into(),
        issued_at: 0,
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
    };
    assert!(rig.core.doc_host.ingest_relayed_command(&chat, relayed).await.is_err());
    assert_eq!(commands(&rig, &chat).len(), 0, "nothing reached the chat");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_provisional_chat_and_its_binding_are_on_disk_before_the_message_is_queued() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    // Stop right before the first message is queued, with no flush and no
    // graceful shutdown: only what the launch itself saved is on disk.
    launcher.inject_fault(Some(LaunchStep::RegistryPersisted));
    let a = manager_params(&rig, "manager-disk-a", "queued after the save");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    let chat = derive_id("manager-disk-a", roles::CHAT);

    let disk = registry_on_disk(&rig);
    let row = disk.chat(&chat).unwrap().expect("the chat row is on disk");
    assert!(row.is_provisional_for(mast_proto::StandingRoleKind::Manager, &rig.item));
    let inputs = disk.work_outline_inputs(SPACE, true);
    assert!(inputs.assignments.iter().any(|a| a.id == chat), "the assignment is on disk");
    assert!(
        inputs.chat_execution_bindings.iter().any(|b| b.id == chat),
        "the binding is on disk"
    );
    assert!(commands(&rig, &chat).is_empty(), "no message queued yet");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_successful_manager_start_has_its_adoption_on_disk_when_it_returns() {
    let rig = rig().await;
    mark_project(&rig).await;
    let reply = launch(&rig, &manager_params(&rig, "manager-done", "Plan"))
        .await
        .unwrap();
    // No flush and no shutdown first.
    let disk = registry_on_disk(&rig);
    assert_eq!(
        disk.standing_role_chat(mast_proto::StandingRoleKind::Manager, &rig.item).as_deref(),
        Some(reply.chat_id.as_str())
    );
    let row = disk.chat(&reply.chat_id).unwrap().unwrap();
    assert!(row.standing_role_provisional().is_none(), "the marker is cleared on disk too");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_start_reuses_the_clean_worktree_an_interrupted_attempt_created() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::ChatCreated));
    let mut a = manager_params(&rig, "manager-wt-a", "first try");
    a.worktree_choice = Some(WorktreeChoice::CreateNew { name: "manager wt".into() });
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());

    // The next attempt asks for the same name and is not blocked by the
    // worktree the interrupted one left behind.
    let mut b = manager_params(&rig, "manager-wt-b", "second try");
    b.worktree_choice = Some(WorktreeChoice::CreateNew { name: "manager wt".into() });
    let reply = launcher.create_chat_for_work_item(&b).await.unwrap();
    assert!(reply.execution_root.contains(".worktrees"), "{}", reply.execution_root);
    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisional_chat_drains_its_launch_message_even_when_reissued_and_rejects_others() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::Assigned));
    let a = manager_params(&rig, "manager-reissue-a", "first try");
    assert!(launcher.create_chat_for_work_item(&a).await.is_err());
    let chat = provisional_chats(&rig).remove(0).id;
    let handle = rig.core.doc_host.open(&chat).unwrap();
    let entry = |id: &str, message: String, prompt: &str| SessionCommandEntry {
        id: id.into(),
        payload: SessionCommandPayload::Run {
            request: run_request(prompt, &rig.repo.to_string_lossy()),
            message_id: message,
        },
        issued_by: rig.core.workspace.device_id().to_owned(),
        // Recent, or the drain expires it as stale.
        issued_at: chrono::Utc::now().timestamp_millis(),
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
    };
    // What a retry of a rejected or expired launch writes: the launch's
    // message under a NEW command id. And a message nobody authorized.
    let reissue = entry("reissued-command", derive_id("manager-reissue-a", roles::MESSAGE), "reissued launch");
    let stray = entry("stray-command", "stray-message".into(), "stray message");
    handle.doc().queue_command(&reissue).unwrap();
    handle.doc().queue_command(&stray).unwrap();
    rig.core.doc_host.drain_commands(&handle).await;

    wait_for(|| turns_with(&rig, "reissued launch").len() == 1, "the reissued launch message").await;
    assert!(turns_with(&rig, "stray message").is_empty());
    let stray_entry = commands(&rig, &chat).into_iter().find(|c| c.id == "stray-command").unwrap();
    assert_eq!(stray_entry.status, SessionCommandStatus::Rejected);
    assert!(stray_entry.resolution.unwrap_or_default().contains("not started yet"));

    // After the role adopts the chat, a user's Retry Delivery must not
    // re-issue the refused message: the rejection is final.
    rig.core
        .workspace
        .activate_standing_role_chat(mast_proto::StandingRoleKind::Manager, &rig.item, &chat, &[])
        .expect("the provisional chat activates");
    rig.core.doc_host.retry_delivery(&chat).unwrap();
    rig.core.doc_host.drain_commands(&handle).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(turns_with(&rig, "stray message").is_empty(), "the refused message never ran");
    let stray_commands: Vec<_> = commands(&rig, &chat)
        .into_iter()
        .filter(|c| matches!(&c.payload, SessionCommandPayload::Run { message_id, .. } if message_id == "stray-message"))
        .collect();
    assert_eq!(stray_commands.len(), 1, "no re-issued attempt: {stray_commands:?}");
    assert_eq!(stray_commands[0].status, SessionCommandStatus::Rejected);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Project work queues: adding ready work starts or reuses the Manager
// ---------------------------------------------------------------------------

async fn create_child(rig: &Rig, op: &str, title: &str) -> String {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "parentWorkItemId": rig.item,
                    "title": title, "operationId": op }),
        )
        .await
        .unwrap();
    derive_id(op, roles::ITEM)
}

async fn queue_item(rig: &Rig, op: &str, item: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "setQueuePosition", "workItemId": item,
                    "change": { "type": "place", "placement": { "type": "last" } },
                    "operationId": op }),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn queuing_ready_work_starts_the_manager_then_later_work_reuses_it() {
    let rig = rig().await;
    mark_project(&rig).await;
    assert_eq!(manager_chat(&rig), None, "no Manager before anything is queued");

    // The entry point: queue one item in a project that has no Manager.
    let first = create_child(&rig, "queue-item-a", "Ticket A").await;
    queue_item(&rig, "queue-a", &first).await;
    wait_for(|| manager_chat(&rig).is_some(), "the queue to start the Manager").await;
    let manager = manager_chat(&rig).unwrap();
    wait_for(|| user_messages(&rig, &manager) == 1, "the item in the Manager's first message").await;
    let delivered = rig
        .core
        .doc_host
        .open(&manager)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.role == MessageRole::User)
        .flat_map(|entry| entry.parts)
        .filter_map(|part| match part {
            mast_doc::MessagePart::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(delivered.contains(&first) && delivered.contains("Ticket A"), "{delivered}");

    // More ready work waits behind the first and never starts a second Manager.
    let second = create_child(&rig, "queue-item-b", "Ticket B").await;
    queue_item(&rig, "queue-b", &second).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(manager.as_str()));
    assert_eq!(user_messages(&rig, &manager), 1, "Ticket A's offer is not repeated");
    let status = rig.core.orchestrator_wakes.status(&manager);
    assert!(status.held.is_empty(), "B waits for A: {:?}", status.held);

    // Once A is done, the same Manager is offered B as a queue wake.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": first,
                    "changes": { "status": "completed" }, "operationId": "queue-a-done" }),
        )
        .await
        .unwrap();
    let wakes = rig.core.orchestrator_wakes.clone();
    wait_for(
        || {
            wakes
                .status(&manager)
                .held
                .iter()
                .any(|event| event.summary.contains("Ticket B"))
        },
        "a queue wake for Ticket B",
    )
    .await;
    assert_eq!(manager_chat(&rig).as_deref(), Some(manager.as_str()), "the Manager is reused");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_queue_starts_no_manager_until_resumed() {
    let rig = rig().await;
    mark_project(&rig).await;
    let pause = |paused: bool, op: &'static str| {
        rig.client.call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "setQueuePaused", "workItemId": rig.item, "paused": paused, "operationId": op }),
        )
    };
    pause(true, "queue-pause").await.unwrap();
    let item = create_child(&rig, "paused-item", "Held ticket").await;
    queue_item(&rig, "paused-queue", &item).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(manager_chat(&rig), None, "paused: nothing starts the Manager");

    pause(false, "queue-resume").await.unwrap();
    wait_for(|| manager_chat(&rig).is_some(), "resume to start the Manager").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ready_item_is_offered_once_across_repeats_and_restarts() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    // The project already has a Manager, so queued work is offered by wake.
    launch(&rig, &manager_params(&rig, "offer-manager", "Plan")).await.unwrap();
    let item = create_child(&rig, "offer-item", "Offered ticket").await;
    queue_item(&rig, "offer-queue", &item).await;
    let snapshot = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    let store = rig.root.join("offers-test.json");
    let build = |store: Option<PathBuf>| {
        WorkQueueDispatcher::new(
            rig.core.workspace.clone(),
            service(&rig),
            rig.core.orchestrator_wakes.clone(),
            None,
        )
        .with_dispatch_records(records_at(&rig, store))
    };

    // Repeated passes make one offer, whatever the wake core's own dedupe
    // window would let through.
    let first = build(Some(store.clone()));
    first.evaluate(&snapshot).await;
    first.evaluate(&snapshot).await;
    first.evaluate(&snapshot).await;
    assert_eq!(first.offers_made(), 1);
    let saved = std::fs::read_to_string(&store).expect("the offer is saved");
    assert!(saved.contains(&item), "{saved}");

    // A restart loses the wake it rode on, so the unanswered offer is made
    // again, once: attempt 2 on the same record.
    let restarted = build(Some(store.clone()));
    restarted.evaluate(&snapshot).await;
    restarted.evaluate(&snapshot).await;
    assert_eq!(restarted.offers_made(), 1);
    let records = records_at(&rig, Some(store.clone())).records_for_item(&item);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].attempt, 2);

    // Control: with nothing remembered, the same snapshot is offered again.
    let fresh = build(None);
    fresh.evaluate(&snapshot).await;
    assert_eq!(fresh.offers_made(), 1);

    // Once the item stops being ready, its remembered offer is pruned.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": item,
                    "changes": { "status": "completed" }, "operationId": "offer-done" }),
        )
        .await
        .unwrap();
    let after = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    restarted.evaluate(&after).await;
    let pruned = std::fs::read_to_string(&store).expect("the store is rewritten");
    assert!(!pruned.contains(&item), "{pruned}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dependent_is_offered_once_after_its_prerequisites_pull_request_merges() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::work_status::PullRequestFacts;
    use std::sync::Mutex;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "dep-manager", "Plan")).await.unwrap();
    // The implementation is in the project but not queued. The review needs it
    // landed, and an independent item sits ahead of the review in the queue.
    let implementation = create_child(&rig, "dep-impl", "Implementation").await;
    let independent = create_child(&rig, "dep-other", "Independent").await;
    let review = create_child(&rig, "dep-review", "Review").await;
    let second = create_child(&rig, "dep-second", "Second prerequisite").await;
    queue_item(&rig, "dep-q1", &independent).await;
    queue_item(&rig, "dep-q2", &review).await;
    let client = &rig.client;
    let mutate = |body: serde_json::Value| async move { client.call(methods::WORK_OUTLINE_MUTATE, body).await.unwrap() };
    mutate(json!({ "op": "createWorkItemExternalReference", "workItemId": implementation,
        "provider": "github", "referenceType": "pullRequest", "externalId": "acme/repo#21",
        "url": "https://github.com/acme/repo/pull/21", "operationId": "dep-pr" })).await;
    mutate(json!({ "op": "setQueuePosition", "workItemId": implementation,
        "change": { "type": "setCompletion", "completion": "landed" }, "operationId": "dep-landed" })).await;
    mutate(json!({ "op": "setQueuePosition", "workItemId": review,
        "change": { "type": "setPrerequisites", "workItemIds": [implementation] }, "operationId": "dep-requires" })).await;
    let snapshot_now = || {
        mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE))
    };
    let store = rig.root.join("dependent-offers-test.json");
    let facts = Arc::new(Mutex::new(PullRequestFacts { state_known: true, open: true, ..Default::default() }));
    let build = || {
        let source = facts.clone();
        WorkQueueDispatcher::new(
            rig.core.workspace.clone(),
            service(&rig),
            rig.core.orchestrator_wakes.clone(),
            None,
        )
        .with_dispatch_records(records_at(&rig, Some(store.clone())))
        .with_pr_facts(move |url| {
            if url.ends_with("/21") { *source.lock().unwrap() } else { PullRequestFacts::default() }
        })
    };
    let remembered = || std::fs::read_to_string(&store).unwrap_or_default();

    // The independent item is offered, once, and the waiting review is not.
    let first = build();
    first.evaluate(&snapshot_now()).await;
    first.evaluate(&snapshot_now()).await;
    assert_eq!(first.offers_made(), 1);
    assert!(remembered().contains(&independent) && !remembered().contains(&review), "{}", remembered());
    // A restart re-offers what no Manager answered, once.
    let restarted = build();
    restarted.evaluate(&snapshot_now()).await;
    restarted.evaluate(&snapshot_now()).await;
    assert_eq!(restarted.offers_made(), 1);

    // The independent item finishes. The review still waits: its prerequisite's
    // pull request is open, so nothing is offered and nothing is remembered.
    mutate(json!({ "op": "updateWorkItem", "workItemId": independent,
        "changes": { "status": "completed" }, "operationId": "dep-other-done" })).await;
    let second_pass = build();
    second_pass.evaluate(&snapshot_now()).await;
    assert_eq!(second_pass.offers_made(), 0);
    assert!(!remembered().contains(&review), "{}", remembered());
    // The Space is held for the recheck clock, since a merge writes nothing to it.
    assert_eq!(second_pass.held_spaces(), 1);

    // The merge writes nothing to the registry, yet the review is offered once,
    // across a repeat and a restart.
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, merged: true, ..Default::default() };
    second_pass.evaluate(&snapshot_now()).await;
    second_pass.evaluate(&snapshot_now()).await;
    assert_eq!(second_pass.offers_made(), 1);
    assert!(remembered().contains(&review), "{}", remembered());
    // A restart makes the unanswered offer again, once, and no more.
    let after_restart = build();
    after_restart.evaluate(&snapshot_now()).await;
    after_restart.evaluate(&snapshot_now()).await;
    assert_eq!(after_restart.offers_made(), 1);
    // A cold or failed PR cache reads the merge as unknown. The review is held, its
    // record stays, and the answer coming back does not offer it a second time.
    *facts.lock().unwrap() = PullRequestFacts::default();
    after_restart.evaluate(&snapshot_now()).await;
    assert!(remembered().contains(&review), "{}", remembered());
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, merged: true, ..Default::default() };
    after_restart.evaluate(&snapshot_now()).await;
    assert_eq!(after_restart.offers_made(), 1);

    // A prerequisite added before a worker starts puts the review back to
    // waiting and drops its offered record. It is offered again once, when met.
    mutate(json!({ "op": "setQueuePosition", "workItemId": review,
        "change": { "type": "setPrerequisites", "workItemIds": [implementation, second] },
        "operationId": "dep-requires-two" })).await;
    after_restart.evaluate(&snapshot_now()).await;
    assert!(!remembered().contains(&review), "{}", remembered());
    mutate(json!({ "op": "updateWorkItem", "workItemId": second,
        "changes": { "status": "completed" }, "operationId": "dep-second-done" })).await;
    after_restart.evaluate(&snapshot_now()).await;
    after_restart.evaluate(&snapshot_now()).await;
    assert_eq!(after_restart.offers_made(), 2);
    assert!(remembered().contains(&review), "{}", remembered());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dependent_keeps_its_offer_when_a_reported_prerequisite_goes_cold() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::work_status::PullRequestFacts;
    use std::sync::Mutex;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "cold-manager", "Plan")).await.unwrap();
    // The prerequisite needs only to be done, and a merged pull request makes it done.
    let prerequisite = create_child(&rig, "cold-pre", "Prerequisite").await;
    let dependent = create_child(&rig, "cold-dep", "Dependent").await;
    queue_item(&rig, "cold-q", &dependent).await;
    let client = &rig.client;
    let mutate = |body: serde_json::Value| async move { client.call(methods::WORK_OUTLINE_MUTATE, body).await.unwrap() };
    mutate(json!({ "op": "createWorkItemExternalReference", "workItemId": prerequisite,
        "provider": "github", "referenceType": "pullRequest", "externalId": "acme/repo#31",
        "url": "https://github.com/acme/repo/pull/31", "operationId": "cold-pr" })).await;
    mutate(json!({ "op": "setQueuePosition", "workItemId": dependent,
        "change": { "type": "setPrerequisites", "workItemIds": [prerequisite] }, "operationId": "cold-requires" })).await;
    let snapshot = mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE));
    let store = rig.root.join("cold-offers.json");
    let facts = Arc::new(Mutex::new(PullRequestFacts { state_known: true, merged: true, ..Default::default() }));
    let source = facts.clone();
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_dispatch_records(records_at(&rig, Some(store.clone())))
    .with_pr_facts(move |_| *source.lock().unwrap());
    let remembered = || std::fs::read_to_string(&store).unwrap_or_default();

    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 1);
    assert!(remembered().contains(&dependent), "{}", remembered());
    // The cache goes cold: the record stays, and the answer returning offers nothing more.
    *facts.lock().unwrap() = PullRequestFacts::default();
    dispatcher.evaluate(&snapshot).await;
    assert!(remembered().contains(&dependent), "{}", remembered());
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, merged: true, ..Default::default() };
    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_pr_holds_the_offer_and_a_merged_one_is_never_offered() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::work_status::PullRequestFacts;
    use std::sync::Mutex;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "held-manager", "Plan")).await.unwrap();
    let item = create_child(&rig, "held-item", "Landed ticket").await;
    queue_item(&rig, "held-queue", &item).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemExternalReference", "workItemId": item,
                    "provider": "github", "referenceType": "pullRequest",
                    "externalId": "acme/repo#7",
                    "url": "https://github.com/acme/repo/pull/7",
                    "operationId": "held-pr" }),
        )
        .await
        .unwrap();
    let snapshot = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    let store = rig.root.join("held-offers.json");
    let facts = Arc::new(Mutex::new(PullRequestFacts::default()));
    let source = facts.clone();
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_dispatch_records(records_at(&rig, Some(store.clone())))
    .with_pr_facts(move |_| *source.lock().unwrap());
    let remembered = || std::fs::read_to_string(&store).unwrap_or_default();

    // A cold cache: the item is held, and nothing is remembered as offered.
    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 0);
    assert!(!remembered().contains(&item), "{}", remembered());

    // The cache fills with a merge: the item is Done, so it is never offered.
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, merged: true, ..Default::default() };
    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 0);
    assert!(!remembered().contains(&item), "{}", remembered());

    // Control: had it answered open, the same item is offered exactly once.
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, open: true, ..Default::default() };
    dispatcher.evaluate(&snapshot).await;
    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 1);
    assert!(remembered().contains(&item), "{}", remembered());
    rig.core.shutdown().await;
}

/// A project with a Manager and one queued item linked to a pull request.
async fn held_item(rig: &Rig, op: &str) -> String {
    launch(rig, &manager_params(rig, &format!("{op}-manager"), "Plan")).await.unwrap();
    let item = create_child(rig, &format!("{op}-item"), "Held ticket").await;
    queue_item(rig, &format!("{op}-queue"), &item).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemExternalReference", "workItemId": item,
                    "provider": "github", "referenceType": "pullRequest",
                    "externalId": "acme/repo#9",
                    "url": "https://github.com/acme/repo/pull/9",
                    "operationId": format!("{op}-pr") }),
        )
        .await
        .unwrap();
    item
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_item_is_offered_when_the_pr_cache_answers() {
    use mast_engine::pr_ticket_cache::{PrStatus, PrTicketCache};
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    held_item(&rig, "revision").await;
    let cache = PrTicketCache::new();
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_pr_ticket_cache(cache.clone());
    let cancel = tokio_util::sync::CancellationToken::new();
    dispatcher.spawn_watcher(cancel.clone());
    // Cold cache: held, so nothing is offered.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(dispatcher.offers_made(), 0);

    // The cache stores an open answer; the held Space is looked at again.
    let open: PrStatus = serde_json::from_value(json!({
        "number": 9, "url": "https://github.com/acme/repo/pull/9", "state": "OPEN",
        "isDraft": false, "reviewers": [], "mergeable": "mergeable",
        "hasReviewerRequested": false
    }))
    .unwrap();
    cache.publish_pr_url_for_test("https://github.com/acme/repo/pull/9", Some(open));
    wait_for(|| dispatcher.offers_made() == 1, "the held item to be offered").await;
    cancel.cancel();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_space_stops_being_held() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    held_item(&rig, "gone").await;
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_pr_facts(|_| Default::default())
    .with_held_recheck(Duration::from_millis(200));
    let cancel = tokio_util::sync::CancellationToken::new();
    dispatcher.spawn_watcher(cancel.clone());
    wait_for(|| dispatcher.held_spaces() == 1, "the Space to be held").await;
    rig.core.workspace.delete_space(SPACE).unwrap();
    wait_for(|| dispatcher.held_spaces() == 0, "the deleted Space to be dropped").await;
    cancel.cancel();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_worker_does_not_hold_the_queue_and_a_running_one_does() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::ChatActivity;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "stall-manager", "Plan")).await.unwrap();
    let first = create_child(&rig, "stall-a", "First").await;
    let second = create_child(&rig, "stall-b", "Second").await;
    queue_item(&rig, "stall-qa", &first).await;
    queue_item(&rig, "stall-qb", &second).await;
    rig.core.workspace.create_chat("stall-worker", Some(SPACE), None, None, None).unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "stall-worker",
                    "workItemId": first, "operationId": "stall-assign" }),
        )
        .await
        .unwrap();
    let snapshot = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    let dispatcher = |activity: ChatActivity, store: &Path| {
        WorkQueueDispatcher::new(
            rig.core.workspace.clone(),
            service(&rig),
            rig.core.orchestrator_wakes.clone(),
            None,
        )
        .with_dispatch_records(records_at(&rig, Some(store.to_path_buf())))
        .with_chat_activity(move |_| activity)
    };
    let now_ms = chrono::Utc::now().timestamp_millis();

    // An errored worker: the first item is stalled, so the second is offered.
    let stalled_store = rig.root.join("stalled-offers.json");
    let errored = ChatActivity { worker: true, errored: true, last_active_ms: now_ms, ..Default::default() };
    let stalled = dispatcher(errored, &stalled_store);
    stalled.evaluate(&snapshot).await;
    assert_eq!(stalled.offers_made(), 1);
    let offered = std::fs::read_to_string(&stalled_store).unwrap();
    assert!(offered.contains(&second) && !offered.contains(&first), "{offered}");

    // The same worker idle for a few minutes is still running: nothing is
    // offered, and the Space stays on the recheck clock so its idle window
    // can run out without a registry write.
    let running_store = rig.root.join("running-offers.json");
    let idle = ChatActivity { worker: true, last_active_ms: now_ms - 5 * 60_000, ..Default::default() };
    let running = dispatcher(idle, &running_store);
    running.evaluate(&snapshot).await;
    assert_eq!(running.offers_made(), 0);
    assert_eq!(running.held_spaces(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_path_reads_a_fresh_worker_as_running() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "reg-manager", "Plan")).await.unwrap();
    let first = create_child(&rig, "reg-a", "First").await;
    let second = create_child(&rig, "reg-b", "Second").await;
    queue_item(&rig, "reg-qa", &first).await;
    queue_item(&rig, "reg-qb", &second).await;
    // A worker an orchestrator started (it has a parent chat), just created.
    rig.core
        .workspace
        .create_chat_with_parent("reg-worker", Some(SPACE), None, None, None, Some("reg-parent".into()))
        .unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "reg-worker",
                    "workItemId": first, "operationId": "reg-assign" }),
        )
        .await
        .unwrap();
    let snapshot = || {
        mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE))
    };
    // No test override: the dispatcher reads the registry. A worker that was
    // just active keeps its item running, so nothing is offered.
    let running = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    );
    running.evaluate(&snapshot()).await;
    assert_eq!(running.offers_made(), 0);
    assert_eq!(running.held_spaces(), 1, "a running item is on the recheck clock");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_path_does_not_treat_a_plain_chat_as_a_worker() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "plain-manager", "Plan")).await.unwrap();
    let item = create_child(&rig, "plain-a", "Planned").await;
    queue_item(&rig, "plain-qa", &item).await;
    // A chat nobody's orchestrator started (no parent), just created, on the item.
    rig.core.workspace.create_chat("plain-chat", Some(SPACE), None, None, None).unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "plain-chat",
                    "workItemId": item, "operationId": "plain-assign" }),
        )
        .await
        .unwrap();
    let snapshot = mast_doc::materialize_work_outline(
        SPACE,
        &rig.core.workspace.work_outline_inputs(SPACE),
    );
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    );
    // No test override: a planning chat does not make the item running, so the
    // item is still waiting for a worker and is offered to the Manager.
    dispatcher.evaluate(&snapshot).await;
    assert_eq!(dispatcher.offers_made(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_ageing_past_the_window_frees_the_queue_on_the_recheck_clock() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::ChatActivity;
    use std::sync::atomic::{AtomicI64, Ordering};
    let rig = rig().await;
    mark_project(&rig).await;
    launch(&rig, &manager_params(&rig, "age-manager", "Plan")).await.unwrap();
    let first = create_child(&rig, "age-a", "First").await;
    let second = create_child(&rig, "age-b", "Second").await;
    queue_item(&rig, "age-qa", &first).await;
    queue_item(&rig, "age-qb", &second).await;
    rig.core.workspace.create_chat("age-worker", Some(SPACE), None, None, None).unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "age-worker",
                    "workItemId": first, "operationId": "age-assign" }),
        )
        .await
        .unwrap();
    // The worker was last active five minutes ago, and its clock is moved
    // back later without any registry write.
    let last_active = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis() - 5 * 60_000));
    let seen = last_active.clone();
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_chat_activity(move |_| ChatActivity {
        worker: true,
        last_active_ms: seen.load(Ordering::SeqCst),
        ..Default::default()
    })
    .with_held_recheck(Duration::from_millis(100));
    let cancel = tokio_util::sync::CancellationToken::new();
    dispatcher.spawn_watcher(cancel.clone());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(dispatcher.offers_made(), 0, "running within the window");
    last_active.store(chrono::Utc::now().timestamp_millis() - 31 * 60_000, Ordering::SeqCst);
    wait_for(|| dispatcher.offers_made() == 1, "the recheck to offer the next item").await;
    cancel.cancel();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_project_does_not_hold_its_space() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    let rig = rig().await;
    mark_project(&rig).await;
    held_item(&rig, "paused").await;
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_pr_facts(|_| Default::default());
    let snapshot = || {
        mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE))
    };
    // Unpaused with an unanswered PR: the Space is held.
    dispatcher.evaluate(&snapshot()).await;
    assert_eq!(dispatcher.held_spaces(), 1);
    // Paused, nothing is fetched for it, so nothing is waiting on an answer.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "setQueuePaused", "workItemId": rig.item, "paused": true,
                    "operationId": "pause-held" }),
        )
        .await
        .unwrap();
    dispatcher.evaluate(&snapshot()).await;
    assert_eq!(dispatcher.held_spaces(), 0);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_item_is_looked_at_again_on_a_timer_when_nothing_else_changes() {
    use mast_engine::work_queue::WorkQueueDispatcher;
    use mast_proto::work_status::PullRequestFacts;
    use std::sync::Mutex;
    let rig = rig().await;
    mark_project(&rig).await;
    held_item(&rig, "timer").await;
    let facts = Arc::new(Mutex::new(PullRequestFacts::default()));
    let source = facts.clone();
    let dispatcher = WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(&rig),
        rig.core.orchestrator_wakes.clone(),
        None,
    )
    .with_pr_facts(move |_| *source.lock().unwrap())
    .with_held_recheck(Duration::from_millis(200));
    let cancel = tokio_util::sync::CancellationToken::new();
    dispatcher.spawn_watcher(cancel.clone());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(dispatcher.offers_made(), 0);

    // No outline change and no cache publish: only the recheck can notice.
    *facts.lock().unwrap() = PullRequestFacts { state_known: true, open: true, ..Default::default() };
    wait_for(|| dispatcher.offers_made() == 1, "the recheck to offer the item").await;
    cancel.cancel();
    rig.core.shutdown().await;
}


/// The queue dispatcher's offer records. Each call is a new engine run over
/// the same records file; the offers file of earlier versions is `legacy`.
fn offer_run(
    rig: &Rig,
    records: &Path,
    legacy: Option<PathBuf>,
) -> mast_engine::work_queue::WorkQueueDispatcher {
    mast_engine::work_queue::WorkQueueDispatcher::new(
        rig.core.workspace.clone(),
        service(rig),
        rig.core.orchestrator_wakes.clone(),
        legacy,
    )
    .with_dispatch_records(records_at(rig, Some(records.to_path_buf())))
}

fn outline(rig: &Rig) -> mast_proto::WorkOutlineSnapshot {
    mast_doc::materialize_work_outline(SPACE, &rig.core.workspace.work_outline_inputs(SPACE))
}

async fn queued_with_manager(rig: &Rig, op: &str) -> (String, String) {
    mark_project(rig).await;
    let manager = launch(rig, &manager_params(rig, &format!("{op}-manager"), "Plan"))
        .await
        .unwrap()
        .chat_id;
    let item = create_child(rig, &format!("{op}-item"), "Queued ticket").await;
    queue_item(rig, &format!("{op}-queue"), &item).await;
    (manager, item)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_wake_is_offered_again_once_per_restart_and_then_given_up() {
    let rig = rig().await;
    let (_manager, item) = queued_with_manager(&rig, "lost").await;
    let records = rig.root.join("lost-wake-records.json");
    let mut offers = Vec::new();
    for _ in 0..4 {
        let run = offer_run(&rig, &records, None);
        run.evaluate(&outline(&rig)).await;
        run.evaluate(&outline(&rig)).await;
        offers.push(run.offers_made());
    }
    assert_eq!(offers, vec![1, 1, 1, 0], "one offer per restart, none after the third");
    // The Required Action it raised holds the item (blocked), so its record is
    // dropped with the hold, and nothing is offered while the action is open.
    let saved = records_at(&rig, Some(records.clone())).records_for_item(&item);
    assert!(saved.is_empty(), "{saved:?}");
    let blocked = offer_run(&rig, &records, None);
    blocked.evaluate(&outline(&rig)).await;
    assert_eq!(blocked.offers_made(), 0);
    let requests = outline(&rig).user_action_requests.iter().filter(|r| r.work_item_id == item).count();
    assert_eq!(requests, 1, "one Required Action, not one per restart");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_started_worker_is_never_offered_again_after_a_restart() {
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (manager, item) = queued_with_manager(&rig, "answered").await;
    let records = rig.root.join("answered-records.json");
    let first = offer_run(&rig, &records, None);
    first.evaluate(&outline(&rig)).await;
    assert_eq!(first.offers_made(), 1);

    // The Manager starts the worker, by its own create_chat.
    let worker = launch(
        &rig,
        &CreateChatForWorkItemParams {
            work_item_id: item.clone(),
            parent_chat_id: Some(manager.clone()),
            ..params(&rig, "answered-worker", "Build it")
        },
    )
    .await
    .unwrap()
    .chat_id;
    first.evaluate(&outline(&rig)).await;
    let saved = records_at(&rig, Some(records.clone())).records_for_item(&item);
    assert_eq!(saved[0].state, DispatchState::Started { chat_id: worker.clone() });

    let restarted = offer_run(&rig, &records, None);
    restarted.evaluate(&outline(&rig)).await;
    assert_eq!(restarted.offers_made(), 0);
    let saved = records_at(&rig, Some(records)).records_for_item(&item);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].state, DispatchState::Started { chat_id: worker });
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_that_leaves_the_queue_drops_its_record() {
    let rig = rig().await;
    let (_manager, item) = queued_with_manager(&rig, "dropped").await;
    let records = rig.root.join("dropped-records.json");
    let run = offer_run(&rig, &records, None);
    run.evaluate(&outline(&rig)).await;
    assert_eq!(records_at(&rig, Some(records.clone())).records_for_item(&item).len(), 1);
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": item,
                    "changes": { "status": "completed" }, "operationId": "dropped-done" }),
        )
        .await
        .unwrap();
    run.evaluate(&outline(&rig)).await;
    assert!(records_at(&rig, Some(records)).records_for_item(&item).is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offer_in_the_old_offers_file_is_migrated_without_a_wake() {
    let rig = rig().await;
    let (_manager, item) = queued_with_manager(&rig, "legacy").await;
    let records = rig.root.join("legacy-records.json");
    let legacy = rig.root.join("legacy-offers.json");
    let key = outline(&rig).item(&item).unwrap().queue_order_key.clone().unwrap();
    std::fs::write(&legacy, json!({ item.clone(): key }).to_string()).unwrap();

    let run = offer_run(&rig, &records, Some(legacy.clone()));
    run.evaluate(&outline(&rig)).await;
    assert_eq!(run.offers_made(), 0, "already offered before records existed");
    let saved = records_at(&rig, Some(records.clone())).records_for_item(&item);
    assert_eq!((saved.len(), saved[0].attempt), (1, 1));
    assert!(!legacy.exists() && legacy.with_extension("migrated").exists(), "migrated once");

    // An entry for another queue key is stale and seeds nothing.
    let stale = rig.root.join("stale-offers.json");
    std::fs::write(&stale, json!({ item.clone(): "zzzz" }).to_string()).unwrap();
    let other = offer_run(&rig, &rig.root.join("stale-records.json"), Some(stale));
    other.evaluate(&outline(&rig)).await;
    assert_eq!(other.offers_made(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_the_manager_for_a_queued_item_records_it_and_sends_no_wake() {
    use mast_proto::DispatchKind;
    let rig = rig().await;
    mark_project(&rig).await;
    let item = create_child(&rig, "boot-item", "Queued ticket").await;
    queue_item(&rig, "boot-queue", &item).await;
    wait_for(|| manager_chat(&rig).is_some(), "the queue to start the Manager").await;
    let manager = manager_chat(&rig).unwrap();
    wait_for(|| !rig.core.review_dispatch.records_for_item(&item).is_empty(), "the offer record").await;
    let records = rig.core.review_dispatch.records_for_item(&item);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, DispatchKind::Worker);
    assert!(records[0].start_path);
    assert_eq!(records[0].manager_chat_id, manager);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_worker_still_gets_its_review() {
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (manager, item) = queued_with_manager(&rig, "queued-review").await;
    wait_for(|| !rig.core.review_dispatch.records_for_item(&item).is_empty(), "the queue's offer").await;
    let worker = launch(
        &rig,
        &CreateChatForWorkItemParams {
            work_item_id: item.clone(),
            parent_chat_id: Some(manager.clone()),
            ..params(&rig, "queued-review-worker", "Build it")
        },
    )
    .await
    .unwrap()
    .chat_id;
    wait_for(
        || {
            rig.core.review_dispatch.records_for_item(&item).iter().any(|record| {
                matches!(&record.state, DispatchState::Started { chat_id } if *chat_id == worker)
            })
        },
        "the offer to record its worker",
    )
    .await;
    let reply = report_completion(&rig, &worker, "abcdef1234").await;
    let line = reply["reviewDispatch"].as_str().expect("a dispatch line");
    assert!(line.contains("Review dispatched"), "{line}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_started_before_any_offer_gets_no_offer_and_no_record() {
    let rig = rig().await;
    let (manager, item) = queued_with_manager(&rig, "manual").await;
    launch(
        &rig,
        &CreateChatForWorkItemParams {
            work_item_id: item.clone(),
            parent_chat_id: Some(manager),
            ..params(&rig, "manual-worker", "Build it")
        },
    )
    .await
    .unwrap();
    let records = rig.root.join("manual-records.json");
    let run = offer_run(&rig, &records, None);
    run.evaluate(&outline(&rig)).await;
    assert_eq!(run.offers_made(), 0);
    assert!(records_at(&rig, Some(records)).records_for_item(&item).is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_record_store_offers_once_and_a_started_record_is_pruned_when_done() {
    let rig = rig().await;
    let (_manager, item) = queued_with_manager(&rig, "corrupt").await;
    let records = rig.root.join("corrupt-records.json");
    std::fs::write(&records, b"{ not json").unwrap();
    let run = offer_run(&rig, &records, None);
    run.evaluate(&outline(&rig)).await;
    run.evaluate(&outline(&rig)).await;
    assert_eq!(run.offers_made(), 1, "at most one offer for the ready item");

    // A started record is dropped once its item is done.
    let store = records_at(&rig, Some(records.clone()));
    assert_eq!(store.records_for_item(&item).len(), 1);
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": item,
                    "changes": { "status": "completed" }, "operationId": "corrupt-done" }),
        )
        .await
        .unwrap();
    run.evaluate(&outline(&rig)).await;
    assert!(records_at(&rig, Some(records)).records_for_item(&item).is_empty());
    rig.core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

// ---------------------------------------------------------------------------
// Starting a chat marks a pending item active; finished items stay put
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn launch_activates_a_pending_item_but_never_a_completed_one() {
    let rig = rig().await;
    let status = |rig: &Rig, id: &str| row_field(rig, mast_doc::KIND_WORK_ITEMS, id, "status");
    assert_ne!(status(&rig, &rig.item), Some(json!("active")));
    launch(&rig, &params(&rig, "op-activate", "go")).await.expect("launch");
    assert_eq!(status(&rig, &rig.item), Some(json!("active")));

    // A completed item is not reopened by a later launch.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": rig.item,
                    "changes": { "status": "completed" }, "operationId": "op-complete" }),
        )
        .await
        .unwrap();
    launch(&rig, &params(&rig, "op-activate-2", "again")).await.expect("launch");
    assert_eq!(status(&rig, &rig.item), Some(json!("completed")));
}


// ---------------------------------------------------------------------------
// Manager 6: a Manager that predates its role instructions
// ---------------------------------------------------------------------------

async fn add_item_instruction(rig: &Rig, op: &str, text: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                    "scope": { "type": "workItem", "workItemId": rig.item },
                    "text": text, "operationId": op }),
        )
        .await
        .unwrap();
}

/// An ordinary chat on the project (it predates Manager role instructions)
/// that the Manager role then adopts.
async fn pre_upgrade_manager(rig: &Rig, op: &str) -> String {
    mark_project(rig).await;
    let old = launch(rig, &params(rig, op, "start")).await.unwrap();
    settle(rig, &old.chat_id).await;
    rig.core
        .workspace
        .adopt_standing_role(mast_proto::StandingRoleKind::Manager, &rig.item, &old.chat_id, &[])
        .unwrap();
    old.chat_id
}

/// The Execution Session the Turn for `prompt` reached.
fn follow_up_run(rig: &Rig, prompt: &str) -> mast_harness::mock::RecordedInstructionRun {
    let turn = rig
        .mock
        .turns()
        .into_iter()
        .rev()
        .find(|turn| turn.prompt.contains(prompt))
        .unwrap_or_else(|| panic!("no turn for {prompt}"));
    rig.mock.runs()[turn.run_index].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_manager_gets_its_role_on_the_next_send_keeping_its_selected_instructions() {
    let rig = rig().await;
    let chat = pre_upgrade_manager(&rig, "op-old-manager").await;
    // Instructions added after the chat's revision was selected are drift.
    add_item_instruction(&rig, "op-newer-rule", "Newer project rule.").await;
    let cwd = rig.repo.to_string_lossy().into_owned();
    queue_run(&rig, &chat, "follow up", &cwd);
    wait_for(
        || rig.mock.turns().iter().any(|turn| turn.prompt.contains("follow up")),
        "the follow-up run",
    )
    .await;

    let run = follow_up_run(&rig, "follow up");
    let envelope = run.instructions.expect("instructions delivered").envelope.rendered;
    assert!(envelope.contains("You are this project's Manager"), "{envelope}");
    assert!(envelope.contains("Run the tests first."), "the selected rule is kept: {envelope}");
    assert!(!envelope.contains("Newer project rule."), "unapplied drift is not adopted: {envelope}");
    // Read-only whatever the request and the stored config said.
    assert_eq!(run.sandbox, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_manager_with_no_selection_keeps_it_empty_and_adds_only_the_role() {
    let rig = rig_with(InstructionMockHarness::default(), false).await;
    let chat = pre_upgrade_manager(&rig, "op-empty-manager").await;
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_none());
    add_item_instruction(&rig, "op-added-later", "Added after the Manager started.").await;
    let cwd = rig.repo.to_string_lossy().into_owned();
    queue_run(&rig, &chat, "follow up", &cwd);
    wait_for(
        || rig.mock.turns().iter().any(|turn| turn.prompt.contains("follow up")),
        "the follow-up run",
    )
    .await;

    let run = follow_up_run(&rig, "follow up");
    let envelope = run.instructions.expect("instructions delivered").envelope.rendered;
    assert!(envelope.contains("You are this project's Manager"), "{envelope}");
    assert!(!envelope.contains("Added after the Manager started."), "{envelope}");
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_some());
    assert_eq!(run.sandbox, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_chats_are_not_given_the_role_or_restricted() {
    let rig = rig().await;
    mark_project(&rig).await;
    let chat = launch(&rig, &params(&rig, "op-ordinary", "start")).await.unwrap().chat_id;
    settle(&rig, &chat).await;
    let cwd = rig.repo.to_string_lossy().into_owned();
    queue_run(&rig, &chat, "follow up", &cwd);
    wait_for(
        || rig.mock.turns().iter().any(|turn| turn.prompt.contains("follow up")),
        "the follow-up run",
    )
    .await;
    let run = follow_up_run(&rig, "follow up");
    let envelope = run.instructions.map(|d| d.envelope.rendered).unwrap_or_default();
    assert!(!envelope.contains("Manager"), "{envelope}");
    assert_eq!(run.sandbox, SandboxLevel::WorkspaceWrite);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicitly_pinned_message_to_an_existing_manager_keeps_its_pin() {
    let rig = rig().await;
    let chat = pre_upgrade_manager(&rig, "op-pinned-manager").await;
    let legacy = rig
        .core
        .workspace
        .selected_instruction_revision(&chat)
        .expect("the chat's launch selected a revision");
    let cwd = rig.repo.to_string_lossy().into_owned();
    rig.core
        .doc_host
        .queue_command(
            &chat,
            SessionCommandPayload::RunWithInstructions {
                request: run_request("pinned follow up", &cwd),
                message_id: "msg-pinned".into(),
                instructions: legacy.clone(),
                context_reset_confirmed: false,
            },
        )
        .expect("queue pinned run");
    wait_for(
        || rig.mock.turns().iter().any(|turn| turn.prompt.contains("pinned follow up")),
        "the pinned turn",
    )
    .await;
    let run = follow_up_run(&rig, "pinned follow up");
    let envelope = run.instructions.expect("instructions delivered").envelope;
    assert_eq!(envelope.revision_id, legacy.revision_id, "the explicit pin is honored");
    assert!(!envelope.rendered.contains("You are this project's Manager"));
    // Read-only still applies; only instructions follow the pin.
    assert_eq!(run.sandbox, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_composer_send_to_an_existing_manager_gains_the_role_and_queued_pins_stay() {
    let rig = rig().await;
    mark_project(&rig).await;
    // A turn is running, so later sends queue behind it.
    let chat = launch(&rig, &params(&rig, "op-busy-manager", "start [hold]"))
        .await
        .unwrap()
        .chat_id;
    wait_for(|| rig.core.sessions.turn_in_flight(&chat), "the held turn").await;
    // Queued before the role exists: pinned to the chat's legacy selection.
    rig.core.doc_host.queue_message(&chat, "first queued", Vec::new()).unwrap();
    rig.core
        .workspace
        .adopt_standing_role(mast_proto::StandingRoleKind::Manager, &rig.item, &chat, &[])
        .unwrap();
    add_item_instruction(&rig, "op-busy-newer", "Newer project rule.").await;
    // Submitted after: a new send, so it gains the role.
    rig.core.doc_host.queue_message(&chat, "second queued", Vec::new()).unwrap();
    rig.mock.release();
    wait_for(
        || rig.mock.turns().iter().any(|turn| turn.prompt.contains("second queued")),
        "the second queued turn",
    )
    .await;

    let first = follow_up_run(&rig, "first queued")
        .instructions
        .map(|d| d.envelope.rendered)
        .unwrap_or_default();
    assert!(!first.contains("You are this project's Manager"), "the queued pin stays: {first}");
    let second_run = follow_up_run(&rig, "second queued");
    let second = second_run.instructions.expect("instructions delivered").envelope.rendered;
    assert!(second.contains("You are this project's Manager"), "{second}");
    assert!(second.contains("Run the tests first."), "{second}");
    assert!(!second.contains("Newer project rule."), "{second}");
    assert_eq!(second_run.sandbox, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

async fn overflow_the_manager_envelope(rig: &Rig) {
    // Within the effective-instruction budget alone, over it with the role added.
    add_item_instruction(rig, "op-big-1", &"a".repeat(6_000)).await;
    add_item_instruction(rig, "op-big-2", &"b".repeat(2_950)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_start_whose_stacked_envelope_overflows_is_refused_before_anything_is_written() {
    let rig = rig().await;
    mark_project(&rig).await;
    overflow_the_manager_envelope(&rig).await;
    let chats_before = chat_count(&rig);
    let error = launch(&rig, &manager_params(&rig, "manager-overflow", "Plan"))
        .await
        .expect_err("the role plus the project's instructions do not fit");
    assert_eq!(error.code, WorkOutlineErrorCode::LimitExceeded, "{error:?}");
    assert_eq!(chat_count(&rig), chats_before, "nothing was created");
    assert!(manager_chat(&rig).is_none());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_start_retry_reuses_its_stored_revision_when_later_project_text_overflows() {
    let rig = rig().await;
    mark_project(&rig).await;
    let launcher = service(&rig);
    launcher.inject_fault(Some(LaunchStep::RevisionApplied));
    let p = manager_params(&rig, "manager-reuse", "Plan");
    assert!(launcher.create_chat_for_work_item(&p).await.is_err());
    // Project text added after the revision was stored would overflow a fresh one.
    overflow_the_manager_envelope(&rig).await;
    launcher.inject_fault(None);
    let reply = launcher
        .create_chat_for_work_item(&p)
        .await
        .expect("the retry reuses the immutable revision it already wrote");
    assert_eq!(manager_chat(&rig).as_deref(), Some(reply.chat_id.as_str()));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_manager_upgrade_refuses_unsupported_harness_without_mutation() {
    let rig = rig_with(unsupported_mock(), false).await;
    let chat = pre_upgrade_manager(&rig, "op-unsupported-upgrade").await;
    let before = commands(&rig, &chat).len();
    let cwd = rig.repo.to_string_lossy().into_owned();
    let err = rig.core.doc_host.queue_command(&chat, SessionCommandPayload::Run {
        request: run_request("unsupported follow up", &cwd),
        message_id: "unsupported-follow-up".into(),
    }).unwrap_err();
    assert!(err.to_string().contains("instruction-delivery/"), "{err}");
    assert_eq!(commands(&rig, &chat).len(), before);
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_none());
    assert!(applications_of(&rig, &chat).is_empty());
    assert!(rig.core.doc_host.queue_message(&chat, "unsupported queued", Vec::new()).is_err());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_manager_failed_upgrade_does_not_send_or_change_selection() {
    let rig = rig_with(InstructionMockHarness::default(), false).await;
    overflow_the_manager_envelope(&rig).await;
    let chat = pre_upgrade_manager(&rig, "op-overflow-upgrade").await;
    let old = rig.core.workspace.selected_instruction_revision(&chat).unwrap();
    let before = commands(&rig, &chat).len();
    let cwd = rig.repo.to_string_lossy().into_owned();
    let error = rig.core.doc_host.queue_command(&chat, SessionCommandPayload::Run {
        request: run_request("overflow follow up", &cwd), message_id: "overflow-follow-up".into(),
    }).unwrap_err();
    assert!(error.to_string().contains("Manager instructions"), "{error}");
    assert_eq!(commands(&rig, &chat).len(), before);
    assert_eq!(rig.core.workspace.selected_instruction_revision(&chat), Some(old));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_manager_remote_host_requires_capability_for_plain_pinned_and_queue() {
    let rig = rig().await;
    let chat = pre_upgrade_manager(&rig, "op-remote-old-manager").await;
    let pin = rig.core.workspace.selected_instruction_revision(&chat).unwrap();
    rig.core.workspace.set_chat_host(&chat, "older-host").unwrap();
    let cwd = rig.repo.to_string_lossy().into_owned();
    let before = commands(&rig, &chat).len();
    for known in [false, true] {
        if known {
            rig.core.workspace.upsert_device_row(&mast_proto::Device {
                id: "older-host".into(), name: "Older host".into(), platform: "test".into(),
                last_seen_at: None, created_at: None, version: None,
                cursor_sdk_version: None, capabilities: Vec::new(),
            });
        }
        let plain = SessionCommandPayload::Run { request: run_request("plain", &cwd), message_id: "remote-plain".into() };
        let pinned = SessionCommandPayload::RunWithInstructions { request: run_request("pinned", &cwd), message_id: "remote-pin".into(), instructions: pin.clone(), context_reset_confirmed: false };
        for payload in [plain, pinned] {
            let error = rig.core.doc_host.queue_command(&chat, payload).unwrap_err();
            assert!(error.to_string().contains("Update Mast"), "{error}");
        }
        assert!(rig.core.doc_host.queue_message(&chat, "remote queued", Vec::new()).is_err());
    }
    assert_eq!(commands(&rig, &chat).len(), before);
    assert_eq!(rig.core.workspace.selected_instruction_revision(&chat), Some(pin));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_queued_row_stays_put_when_host_changes_before_drain() {
    let rig = rig().await;
    let chat = pre_upgrade_manager(&rig, "op-manager-owner-change").await;
    let handle = rig.core.doc_host.open(&chat).unwrap();
    let guard = handle.test_queue_drain_guard().await;
    let id = rig.core.doc_host.queue_message(&chat, "wait for owner", Vec::new()).unwrap();
    rig.core.workspace.set_chat_host(&chat, "older-host").unwrap();
    drop(guard);
    rig.core.doc_host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert!(rows.iter().any(|row| row.id == id));
    assert!(!rig.mock.turns().iter().any(|turn| turn.prompt.contains("wait for owner")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_sender_to_unsupported_manager_is_rejected_by_host() {
    let rig = rig_with(unsupported_mock(), false).await;
    let chat = pre_upgrade_manager(&rig, "op-legacy-unsupported").await;
    let handle = rig.core.doc_host.open(&chat).unwrap();
    let entry = SessionCommandEntry {
        id: "legacy-unsupported-command".into(),
        payload: SessionCommandPayload::Run {
            request: run_request("legacy unsupported", &rig.repo.to_string_lossy()),
            message_id: "legacy-unsupported-message".into(),
        },
        issued_by: DEVICE.into(),
        issued_at: chrono::Utc::now().timestamp_millis(),
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
    };
    handle.doc().queue_command(&entry).unwrap();
    rig.core.doc_host.drain_commands(&handle).await;
    wait_for(|| commands(&rig, &chat).iter().any(|command|
        command.id == entry.id && command.status == SessionCommandStatus::Rejected),
        "legacy command rejection").await;
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_none());
    assert!(!rig.mock.turns().iter().any(|turn| turn.prompt.contains("legacy unsupported")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_without_selected_revision_cannot_send_to_unknown_remote_host() {
    let rig = rig_with(InstructionMockHarness::default(), false).await;
    let chat = pre_upgrade_manager(&rig, "op-remote-empty-manager").await;
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_none());
    rig.core.workspace.set_chat_host(&chat, "unknown-host").unwrap();
    let before = commands(&rig, &chat).len();
    let cwd = rig.repo.to_string_lossy().into_owned();
    let error = rig.core.doc_host.queue_command(&chat, SessionCommandPayload::Run {
        request: run_request("remote empty", &cwd), message_id: "remote-empty".into(),
    }).unwrap_err();
    assert!(error.to_string().contains("Update Mast"), "{error}");
    assert_eq!(commands(&rig, &chat).len(), before);
    assert!(rig.core.workspace.selected_instruction_revision(&chat).is_none());
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// The per-project "Manager runs read-only" setting
// ---------------------------------------------------------------------------

async fn set_manager_read_only(rig: &Rig, read_only: bool, op: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "setManagerReadOnly", "workItemId": rig.item,
                    "readOnly": read_only, "operationId": op }),
        )
        .await
        .unwrap();
}

async fn manager_run_sandbox(rig: &Rig, chat: &str, prompt: &str) -> SandboxLevel {
    let cwd = rig.repo.to_string_lossy().into_owned();
    queue_run(rig, chat, prompt, &cwd);
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains(prompt)), "the manager run").await;
    settle(rig, chat).await;
    follow_up_run(rig, prompt).sandbox
}

#[tokio::test(flavor = "multi_thread")]
async fn the_manager_read_only_setting_applies_from_the_next_run_and_a_retried_toggle_applies_once() {
    let rig = rig().await;
    mark_project(&rig).await;
    // Chosen before any Manager exists: the first run follows it.
    set_manager_read_only(&rig, false, "ro-off-1").await;
    let chat = launch(&rig, &manager_params(&rig, "ro-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(chat.as_str()), "Manager role").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Start managing")), "first run").await;
    settle(&rig, &chat).await;
    assert_ne!(follow_up_run(&rig, "Start managing").sandbox, SandboxLevel::ReadOnly, "off: the chat's own sandbox");
    let envelope = follow_up_run(&rig, "Start managing").instructions.expect("role delivered").envelope.rendered;
    assert!(envelope.contains("You are this project's Manager"), "role instructions still apply: {envelope}");
    assert!(!envelope.contains("cannot edit files"), "{envelope}");

    assert_ne!(manager_run_sandbox(&rig, &chat, "run two").await, SandboxLevel::ReadOnly);
    set_manager_read_only(&rig, true, "ro-on-1").await;
    assert_eq!(manager_run_sandbox(&rig, &chat, "run three").await, SandboxLevel::ReadOnly);
    // Replaying the earlier off operation changes nothing.
    set_manager_read_only(&rig, false, "ro-off-1").await;
    assert_eq!(manager_run_sandbox(&rig, &chat, "run four").await, SandboxLevel::ReadOnly);

    // A flip during a turn never reaches that turn; the next run follows it.
    rig.mock.hold_matching(Some("held run"));
    let cwd = rig.repo.to_string_lossy().into_owned();
    queue_run(&rig, &chat, "held run", &cwd);
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("held run")), "held turn").await;
    set_manager_read_only(&rig, false, "ro-off-2").await;
    rig.mock.hold_matching(None);
    rig.mock.release();
    settle(&rig, &chat).await;
    assert_eq!(follow_up_run(&rig, "held run").sandbox, SandboxLevel::ReadOnly, "the running turn kept its sandbox");
    assert_ne!(manager_run_sandbox(&rig, &chat, "run six").await, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_hand_off_successor_follows_the_setting_at_its_own_launch() {
    let rig = rig().await;
    mark_project(&rig).await;
    let source = launch(&rig, &manager_params(&rig, "ro-handoff-manager", "Start managing")).await.unwrap().chat_id;
    wait_for(|| manager_chat(&rig).as_deref() == Some(source.as_str()), "Manager role").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Start managing")), "first run").await;
    settle(&rig, &source).await;
    assert_eq!(follow_up_run(&rig, "Start managing").sandbox, SandboxLevel::ReadOnly, "default on");

    set_manager_read_only(&rig, false, "ro-handoff-off").await;
    let first = "ro-handoff-a";
    rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": source, "summary": "Continue off", "operationId": first, "archive": true
    })).await.unwrap();
    let successor = derive_id(first, roles::CHAT);
    wait_for(|| manager_chat(&rig).as_deref() == Some(successor.as_str()), "first successor").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Continue off")), "first successor turn").await;
    settle(&rig, &successor).await;
    assert_ne!(follow_up_run(&rig, "Continue off").sandbox, SandboxLevel::ReadOnly);

    set_manager_read_only(&rig, true, "ro-handoff-on").await;
    let second = "ro-handoff-b";
    rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": successor, "summary": "Continue on", "operationId": second, "archive": true
    })).await.unwrap();
    let next = derive_id(second, roles::CHAT);
    wait_for(|| manager_chat(&rig).as_deref() == Some(next.as_str()), "second successor").await;
    wait_for(|| rig.mock.turns().iter().any(|turn| turn.prompt.contains("Continue on")), "second successor turn").await;
    settle(&rig, &next).await;
    assert_eq!(follow_up_run(&rig, "Continue on").sandbox, SandboxLevel::ReadOnly);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Review dispatch: one reviewer per (work item, tested commit)
// ---------------------------------------------------------------------------

/// A project with a Manager and a worker on its own item, the worker's parent
/// being the Manager. Returns (manager, worker item, worker chat).
async fn manager_with_worker(rig: &Rig, op: &str) -> (String, String, String) {
    mark_project(rig).await;
    let manager = launch(rig, &manager_params(rig, &format!("{op}-manager"), "Plan"))
        .await
        .unwrap()
        .chat_id;
    let item = create_child(rig, &format!("{op}-item"), "Feature").await;
    let worker = launch(
        rig,
        &CreateChatForWorkItemParams {
            work_item_id: item.clone(),
            parent_chat_id: Some(manager.clone()),
            ..params(rig, &format!("{op}-worker"), "Build it")
        },
    )
    .await
    .unwrap()
    .chat_id;
    (manager, item, worker)
}

fn records_at(rig: &Rig, path: Option<PathBuf>) -> mast_engine::review_dispatch::ReviewDispatcher {
    mast_engine::review_dispatch::ReviewDispatcher::new(rig.core.workspace.clone(), service(rig), path)
}

fn completion(sha: &str) -> mast_proto::ReportCompletion {
    mast_proto::ReportCompletion { tested_sha: sha.into(), evidence: "nextest: 120 passed".into() }
}

async fn report_completion(rig: &Rig, worker: &str, sha: &str) -> Value {
    rig.client
        .call(
            methods::REPORT_TO_PARENT,
            json!({ "chatId": worker, "text": "STATUS: done", "completion": completion(sha) }),
        )
        .await
        .unwrap()
}

fn all_chats(rig: &Rig) -> usize {
    rig.core.workspace.read_chats().unwrap().len()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completion_report_starts_one_review_across_duplicates_restart_and_another_worker() {
    use mast_engine::review_dispatch::ReviewDispatcher;
    use mast_proto::{DispatchKind, DispatchState, dispatch_operation_id};
    let rig = rig().await;
    let (manager, item, worker) = manager_with_worker(&rig, "rd").await;
    let before = all_chats(&rig);

    // The real entry point: the engine's own dispatcher behind ReportToParent.
    let reply = report_completion(&rig, &worker, "ABCDEF1234").await;
    let line = reply["reviewDispatch"].as_str().expect("a dispatch line").to_owned();
    let op = dispatch_operation_id(DispatchKind::Review, &item, "abcdef1234");
    let reviewer = derive_id(&op, roles::CHAT);
    assert!(line.contains(&reviewer) && line.contains("abcdef1234"), "{line}");
    assert_eq!(all_chats(&rig), before + 1, "one reviewer chat");
    let chat = rig.core.workspace.chat(&reviewer).unwrap().expect("the reviewer exists");
    assert_eq!(chat.parent_chat_id.as_deref(), Some(manager.as_str()));
    assert_eq!(
        rig.core.workspace.chat_assigned_work_item(&reviewer, SPACE).as_deref(),
        Some(item.as_str()),
        "assigned to the item under review"
    );
    wait_for(|| turns_with(&rig, "Review commit abcdef1234").len() == 1, "the reviewer's first turn").await;
    let turn = &turns_with(&rig, "Review commit abcdef1234")[0];
    assert!(turn.prompt.contains("nextest: 120 passed"), "the evidence reaches the reviewer");

    // The record is queryable by item.
    let records = rig.core.review_dispatch.records_for_item(&item);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worker_chat_id, worker);
    assert_eq!(records[0].operation_id, op);
    assert_eq!(records[0].state, DispatchState::Started { chat_id: reviewer.clone() });

    // The same report again: same answer, nothing new.
    let again = report_completion(&rig, &worker, "abcdef1234").await;
    assert_eq!(again["reviewDispatch"].as_str(), Some(line.as_str()));
    assert_eq!(all_chats(&rig), before + 1);
    assert_eq!(rig.core.review_dispatch.records_for_item(&item).len(), 1);

    // A restart: a new dispatcher over the same store finds the record.
    let store = rig.root.join("review-dispatch-test.json");
    let first = ReviewDispatcher::new(rig.core.workspace.clone(), service(&rig), Some(store.clone()));
    let first_line = first.dispatch(&worker, &completion("1234567")).await.unwrap().unwrap();
    let after_first = all_chats(&rig);
    let restarted = ReviewDispatcher::new(rig.core.workspace.clone(), service(&rig), Some(store));
    assert_eq!(restarted.dispatch(&worker, &completion("1234567")).await.unwrap().unwrap(), first_line);
    assert_eq!(all_chats(&rig), after_first, "restart starts nothing");
    assert_eq!(restarted.records_for_item(&item).len(), 1);

    // Another worker (a handoff reports from a different chat) on the same item
    // and commit reuses the review; a new commit gets its own.
    let second_worker = launch(
        &rig,
        &CreateChatForWorkItemParams {
            work_item_id: item.clone(),
            parent_chat_id: Some(manager.clone()),
            ..params(&rig, "rd-worker-2", "Continue")
        },
    )
    .await
    .unwrap()
    .chat_id;
    let reused = restarted.dispatch(&second_worker, &completion("1234567")).await.unwrap().unwrap();
    assert_eq!(reused, first_line);
    assert_eq!(all_chats(&rig), after_first + 1, "only the worker chat was added");
    restarted.dispatch(&second_worker, &completion("7654321")).await.unwrap();
    assert_eq!(restarted.records_for_item(&item).len(), 2, "a new commit is a new review");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_review_whose_launch_failed_resumes_as_the_same_chat() {
    use mast_engine::review_dispatch::ReviewDispatcher;
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (_, item, worker) = manager_with_worker(&rig, "rf").await;
    let launcher = service(&rig);
    let dispatcher = ReviewDispatcher::new(rig.core.workspace.clone(), launcher.clone(), None);
    launcher.inject_fault(Some(LaunchStep::Assigned));
    let line = dispatcher.dispatch(&worker, &completion("abcdef1")).await.unwrap().unwrap();
    assert!(line.contains("not started yet"), "{line}");
    assert_eq!(dispatcher.records_for_item(&item)[0].state, DispatchState::Claimed);

    // The retry replays the recorded launch and adopts the half-made chat.
    let line = dispatcher.dispatch(&worker, &completion("abcdef1")).await.unwrap().unwrap();
    assert!(line.contains("Review dispatched"), "{line}");
    let records = dispatcher.records_for_item(&item);
    assert_eq!(records.len(), 1);
    let DispatchState::Started { chat_id } = &records[0].state else { panic!("{:?}", records[0].state) };
    assert_eq!(
        rig.core.workspace.read_chats().unwrap().iter().filter(|c| &c.id == chat_id).count(),
        1
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_is_blocked_on_a_synced_workspace() {
    use mast_engine::review_dispatch::ReviewDispatcher;
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (_, item, worker) = manager_with_worker(&rig, "rb").await;
    let dispatcher = ReviewDispatcher::new(rig.core.workspace.clone(), service(&rig), None);
    // Connecting any registry server ends sole authority for this engine.
    rig.core.workspace.connect_registry_url("ws://127.0.0.1:9/never");
    assert!(!rig.core.workspace.is_local_authority());
    let before = all_chats(&rig);
    let line = dispatcher.dispatch(&worker, &completion("abcdef1")).await.unwrap().unwrap();
    assert!(line.contains("blocked (user)"), "{line}");
    assert_eq!(all_chats(&rig), before, "nothing starts");
    let records = dispatcher.records_for_item(&item);
    assert!(matches!(&records[0].state, DispatchState::Blocked { owner, .. } if owner == "user"));
    assert!(records[0].launch.is_none());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_without_a_manager_or_with_a_bad_commit_starts_nothing() {
    use mast_engine::review_dispatch::ReviewDispatcher;
    let rig = rig().await;
    // No project, no Manager: an ordinary report.
    let worker = launch(&rig, &params(&rig, "nm-worker", "Build")).await.unwrap().chat_id;
    let dispatcher = ReviewDispatcher::new(rig.core.workspace.clone(), service(&rig), None);
    assert_eq!(dispatcher.dispatch(&worker, &completion("abcdef1")).await.unwrap(), None);
    assert!(dispatcher.dispatch(&worker, &completion("nothex!")).await.is_err());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_manager_reads_the_dispatch_line_and_a_corrupt_store_is_kept_aside() {
    use mast_engine::review_dispatch::ReviewDispatcher;
    let rig = rig().await;
    let (manager, item, worker) = manager_with_worker(&rig, "rw").await;
    // A draft in the Manager's composer holds the wake, so it can be read.
    rig.core.orchestrator_wakes.note_user_activity(&manager, Some(true)).await;
    let first = report_completion(&rig, &worker, "abcdef1").await;
    let line = first["reviewDispatch"].as_str().unwrap().to_owned();
    let reports = |rig: &Rig| -> Vec<String> {
        rig.core.orchestrator_wakes.status(&manager).held.into_iter().filter_map(|e| e.report).collect()
    };
    assert!(reports(&rig).iter().any(|r| r.contains(&line)), "{:?}", reports(&rig));
    let again = report_completion(&rig, &worker, "abcdef1").await;
    assert_eq!(again["reviewDispatch"].as_str(), Some(line.as_str()));
    let record = rig.core.review_dispatch.records_for_item(&item).remove(0);

    // A reviewer that reports with a completion does not start another review.
    let DispatchStateStarted(reviewer) = started(&record) else { panic!() };
    let before = all_chats(&rig);
    let via_reviewer = rig.core.review_dispatch.dispatch(&reviewer, &completion("fedcba9")).await.unwrap();
    assert_eq!(via_reviewer, None);
    assert_eq!(all_chats(&rig), before);

    // A corrupt store is moved aside, never overwritten.
    let store = rig.root.join("corrupt-store.json");
    std::fs::write(&store, b"{ not json").unwrap();
    let dispatcher = ReviewDispatcher::new(rig.core.workspace.clone(), service(&rig), Some(store.clone()));
    assert!(dispatcher.records_for_item(&item).is_empty());
    assert!(!store.exists());
    let kept = std::fs::read_dir(&rig.root).unwrap().filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with("corrupt-store.corrupt-"));
    assert!(kept, "the bad file is kept");

    // The fresh store dispatches normally.
    let line = dispatcher.dispatch(&worker, &completion("0a0a0a0")).await.unwrap().unwrap();
    assert!(line.contains("Review dispatched"), "{line}");

    // Too-long text and lines keep the dispatch line inside the report cap.
    let long = "x".repeat(6000);
    let combined = mast_engine::review_dispatch::with_dispatch_line_for_tests(&long, &"y".repeat(2000));
    assert!(combined.len() <= mast_proto::MAX_WORKER_REPORT_BYTES, "{}", combined.len());
    assert!(combined.contains("[report cut]"));
    rig.core.shutdown().await;
}

fn reviewer_of(item: &str, sha: &str) -> String {
    let op = mast_proto::dispatch_operation_id(mast_proto::DispatchKind::Review, item, sha);
    derive_id(&op, roles::CHAT)
}

fn dispatch_state(rig: &Rig, item: &str, sha: &str) -> mast_proto::DispatchState {
    rig.core
        .review_dispatch
        .records_for_item(item)
        .into_iter()
        .find(|record| record.commit_sha == sha)
        .unwrap_or_else(|| panic!("no record for {sha}"))
        .state
}

fn dispatch_line(reply: &Value) -> String {
    reply["reviewDispatch"].as_str().expect("a dispatch line").to_owned()
}

/// A review claimed but never given a chat starts over: the retry looks at the
/// Manager and the blockers again instead of replaying a stale launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_claimed_review_with_no_chat_starts_over_on_retry() {
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (manager, item, worker) = manager_with_worker(&rig, "rc").await;
    let before = all_chats(&rig);

    rig.core.test_inject_work_item_launch_fault(LaunchStep::Validated);
    let line = dispatch_line(&report_completion(&rig, &worker, "abcdef1").await);
    assert!(line.contains("not started yet"), "{line}");
    assert_eq!(dispatch_state(&rig, &item, "abcdef1"), DispatchState::Claimed);
    let reviewer = reviewer_of(&item, "abcdef1");
    assert!(rig.core.workspace.chat(&reviewer).unwrap().is_none(), "no chat was made");
    assert_eq!(all_chats(&rig), before);

    // The retry starts the reviewer once, under the Manager.
    let line = dispatch_line(&report_completion(&rig, &worker, "abcdef1").await);
    assert!(line.contains("Review dispatched") && line.contains(&reviewer), "{line}");
    assert_eq!(all_chats(&rig), before + 1);
    assert_eq!(
        rig.core.workspace.chat(&reviewer).unwrap().unwrap().parent_chat_id.as_deref(),
        Some(manager.as_str())
    );
    assert_eq!(dispatch_state(&rig, &item, "abcdef1"), DispatchState::Started { chat_id: reviewer });

    // A claim with no chat meets a blocker that appeared since: it is recorded
    // as blocked and starts nothing.
    rig.core.test_inject_work_item_launch_fault(LaunchStep::Validated);
    let line = dispatch_line(&report_completion(&rig, &worker, "1234567").await);
    assert!(line.contains("not started yet"), "{line}");
    assert_eq!(dispatch_state(&rig, &item, "1234567"), DispatchState::Claimed);
    rig.core.workspace.connect_registry_url("ws://127.0.0.1:9/never");
    let chats = all_chats(&rig);
    let line = dispatch_line(&report_completion(&rig, &worker, "1234567").await);
    assert!(line.contains("blocked (user)"), "{line}");
    assert!(matches!(dispatch_state(&rig, &item, "1234567"), DispatchState::Blocked { .. }));
    assert_eq!(all_chats(&rig), chats, "nothing starts");
    rig.core.shutdown().await;
}

/// Every way a dispatch can go wrong reaches the Manager as a line on the
/// report, and the report itself is still delivered.
#[tokio::test(flavor = "multi_thread")]
async fn dispatch_failures_reach_the_manager_as_lines_on_the_report() {
    use mast_proto::DispatchState;
    let rig = rig().await;
    let (manager, item, worker) = manager_with_worker(&rig, "re").await;
    rig.core.orchestrator_wakes.note_user_activity(&manager, Some(true)).await;
    let reports = |rig: &Rig| -> Vec<String> {
        rig.core.orchestrator_wakes.status(&manager).held.into_iter().filter_map(|e| e.report).collect()
    };
    let before = all_chats(&rig);

    // A completion the engine rejects.
    let reply = rig
        .client
        .call(
            methods::REPORT_TO_PARENT,
            json!({ "chatId": worker, "text": "STATUS: done",
                    "completion": { "testedSha": "nothex!", "evidence": "ok" } }),
        )
        .await
        .unwrap();
    let line = dispatch_line(&reply);
    assert!(line.starts_with("Review not dispatched: testedSha must be"), "{line}");
    assert!(reports(&rig).iter().any(|r| r.contains(&line)), "{:?}", reports(&rig));
    assert!(rig.core.review_dispatch.records_for_item(&item).is_empty());

    // A first save that fails: nothing is recorded or started, and a retry
    // after the disk recovers dispatches.
    // The dispatcher's store is data/review-dispatches.json; its save writes
    // `review-dispatches.tmp` first, so a directory there fails every save.
    let blocked_tmp = rig.root.join("data").join("review-dispatches.tmp");
    std::fs::create_dir(&blocked_tmp).unwrap();
    let line = dispatch_line(&report_completion(&rig, &worker, "abcdef1").await);
    assert!(line.starts_with("Review not dispatched: could not save dispatch records"), "{line}");
    assert!(reports(&rig).iter().any(|r| r.contains(&line)), "{:?}", reports(&rig));
    assert!(rig.core.review_dispatch.records_for_item(&item).is_empty(), "the claim was rolled back");
    assert_eq!(all_chats(&rig), before);
    std::fs::remove_dir(&blocked_tmp).unwrap();

    // A launch that fails leaves a claim and says it is retried.
    rig.core.test_inject_work_item_launch_fault(LaunchStep::Assigned);
    let line = dispatch_line(&report_completion(&rig, &worker, "abcdef1").await);
    assert!(line.starts_with("Review not started yet"), "{line}");
    assert!(line.contains("retried when this report is delivered again"), "{line}");
    assert_eq!(dispatch_state(&rig, &item, "abcdef1"), DispatchState::Claimed);

    // The retry starts the reviewer, but the state cannot be saved: the line
    // says so, and the answer in memory is still the truthful one.
    std::fs::create_dir(&blocked_tmp).unwrap();
    let line = dispatch_line(&report_completion(&rig, &worker, "abcdef1").await);
    let reviewer = reviewer_of(&item, "abcdef1");
    assert!(line.contains("Review dispatched") && line.contains(&reviewer), "{line}");
    assert!(line.contains("The record was not saved") && line.contains("a restart may start it again"), "{line}");
    assert!(reports(&rig).iter().any(|r| r.contains("The record was not saved")), "{:?}", reports(&rig));
    assert_eq!(dispatch_state(&rig, &item, "abcdef1"), DispatchState::Started { chat_id: reviewer });
    assert_eq!(all_chats(&rig), before + 1, "one reviewer");
    std::fs::remove_dir(&blocked_tmp).unwrap();
    rig.core.shutdown().await;
}

struct DispatchStateStarted(String);
fn started(record: &mast_proto::DispatchRecord) -> DispatchStateStarted {
    match &record.state {
        mast_proto::DispatchState::Started { chat_id } => DispatchStateStarted(chat_id.clone()),
        other => panic!("not started: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Required Actions answer delivery: one delivery per operation, whichever path
// (held queue row or idle turn) a retry takes
// ---------------------------------------------------------------------------

fn answer_turns(rig: &Rig) -> usize {
    rig.mock.turns().iter().filter(|turn| turn.prompt.contains("Re: the answer")).count()
}

/// A busy Chat holds the answer as a queue row. A lost reply replays the same
/// row, the idle path (the Chat went idle before the retry) finds that row
/// instead of starting a second turn, even after the user edited the held
/// text; once the row drained the idle path reports the id as taken and the
/// queue send replays it without a second turn.
#[tokio::test(flavor = "multi_thread")]
async fn answer_delivery_is_one_turn_across_queue_and_idle_retries() {
    let rig = rig().await;
    let chat = launch(&rig, &params(&rig, "op-answer-chat", "start")).await.unwrap().chat_id;
    settle(&rig, &chat).await;
    continue_chat(&rig, &chat, "op-answer-busy", "Working [hold]").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&chat), "the busy turn").await;

    let message_id = derive_id("op-answer", roles::MESSAGE);
    let send = json!({ "chatId": chat, "messageId": message_id, "text": "Re: the answer", "attachments": [], "holdForTurnEnd": true });
    let queue = || rig.client.call(methods::QUEUE_MESSAGE, send.clone());
    assert_eq!(queue().await.unwrap()["id"], message_id);
    assert_eq!(queue().await.unwrap()["id"], message_id, "a lost reply replays the row");
    let held = || rig.core.doc_host.open(&chat).unwrap().doc().read_queue().unwrap();
    assert_eq!(held().len(), 1);

    let again = continue_chat(&rig, &chat, "op-answer", "Re: the answer").await.unwrap();
    assert_eq!(again["state"], "queued");
    assert_eq!(again["messageId"], message_id);
    assert_eq!(held().len(), 1, "the idle path did not queue a second copy");
    assert!(
        commands(&rig, &chat).iter().all(|command| !format!("{:?}", command.payload).contains(&message_id)),
        "the idle path did not write a command for the held row"
    );
    // The user edits the held row; the retry still finds it as the delivery.
    rig.client.call(methods::UPDATE_QUEUED_MESSAGE, json!({ "chatId": chat, "id": message_id, "text": "Re: the answer (edited)" })).await.unwrap();
    let edited = continue_chat(&rig, &chat, "op-answer", "Re: the answer").await.unwrap();
    assert_eq!(edited["state"], "queued");

    rig.mock.release();
    wait_for(|| answer_turns(&rig) == 1, "the held answer to run").await;
    settle(&rig, &chat).await;
    let drained = continue_chat(&rig, &chat, "op-answer", "Re: the answer").await.unwrap_err();
    assert!(drained.contains("Cannot verify"), "the idle path does not guess: {drained}");
    assert_eq!(queue().await.unwrap()["id"], message_id, "the queue send replays the drained row");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answer_turns(&rig), 1, "exactly one turn for the answer");
    rig.core.shutdown().await;
}

/// A command the host rejected delivered nothing, so a queue send under its
/// message id says so instead of answering "already sent".
#[tokio::test(flavor = "multi_thread")]
async fn queue_send_does_not_treat_a_rejected_command_as_delivered() {
    let rig = rig().await;
    let chat = launch(&rig, &params(&rig, "op-reject-chat", "start")).await.unwrap().chat_id;
    settle(&rig, &chat).await;
    continue_chat(&rig, &chat, "op-reject-busy", "Working [hold]").await.unwrap();
    wait_for(|| rig.core.sessions.turn_in_flight(&chat), "the busy turn").await;
    let pending = continue_chat(&rig, &chat, "op-rejected", "Re: the answer").await.unwrap();
    let command_id = pending["commandId"].as_str().unwrap().to_owned();
    let doc = rig.core.doc_host.open(&chat).unwrap();
    doc.doc().set_command_status(&command_id, SessionCommandStatus::Rejected, Some("test rejection")).unwrap();
    let message_id = derive_id("op-rejected", roles::MESSAGE);
    let send = json!({ "chatId": chat, "messageId": message_id, "text": "Re: the answer", "attachments": [], "holdForTurnEnd": true });
    let error = rig.client.call(methods::QUEUE_MESSAGE, send).await.unwrap_err().to_string();
    assert!(error.contains("was rejected"), "{error}");
    assert!(doc.doc().read_queue().unwrap().is_empty(), "nothing was queued under the rejected id");
    rig.mock.release();
    rig.core.shutdown().await;
}
