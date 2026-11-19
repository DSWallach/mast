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
        let profile = tempfile::Builder::new().prefix("mast-first-run-").tempdir_in("/private/tmp").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "work_outline_launch::signed_out_claude_launch_reaches_transcript"])
            .env(ISOLATED, "1")
            .env("HOME", profile.path())
            .env("MAST_DATA_DIR", profile.path().join("data"))
            .env("CLAUDE_CONFIG_DIR", profile.path().join("claude"))
            .env("CODEX_HOME", profile.path().join("codex"))
            .env("TMPDIR", "/private/tmp")
            .env("MAST_NO_LOGIN_SHELL", "1")
            .output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"), "child did not run exactly one test: {}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }

    let tmp = tempfile::Builder::new().prefix("mast-first-run-").tempdir_in("/private/tmp").unwrap();
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
        let profile = tempfile::Builder::new().prefix("mast-first-run-").tempdir_in("/private/tmp").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "work_outline_launch::codex_only_launch_spawns_selected_executable"])
            .env(ISOLATED, "1")
            .env("HOME", profile.path())
            .env("MAST_DATA_DIR", profile.path().join("data"))
            .env("CLAUDE_CONFIG_DIR", profile.path().join("claude"))
            .env("CODEX_HOME", profile.path().join("codex"))
            .env("TMPDIR", "/private/tmp")
            .env("MAST_NO_LOGIN_SHELL", "1")
            .output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"), "child did not run exactly one test: {}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let tmp = tempfile::Builder::new().prefix("mast-first-run-").tempdir_in("/private/tmp").unwrap();
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
