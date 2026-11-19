//! The briefing index reaches the agent with a chat's first turn only: never
//! in the transcript, never ahead of a leading slash command, and not again
//! on later turns.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_doc::SessionCommandPayload;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    ScheduledChat, ScheduledRunResults, ScheduledTaskOutput, ScheduledTasksRequest, SteeringMode,
    derive_id, roles,
};
use mast_rpc::methods;
use serde_json::json;

const NOTICE: &str = "Latest briefings: Slack 05:31. Read them with briefing_read.\n\n";

/// Records the prompt each run is handed.
struct Recorder(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Harness for Recorder {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Recorder"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let turn = {
            let mut prompts = self.0.lock().unwrap();
            prompts.push(request.prompt.clone());
            prompts.len()
        };
        let session = format!("hs-{turn}");
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: session.clone(),
                assistant_message_id: format!("a-{turn}"),
                chat_id: None,
                space_id: None,
            }),
            Ok(AgentEvent::TextDelta { text: "ok".into() }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session),
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    prompts: Arc<Mutex<Vec<String>>>,
    cwd: String,
}

fn rig(chats: &[&str]) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Recorder(prompts.clone())));
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let cwd = tmp.path().to_string_lossy().into_owned();
    core.workspace
        .create_space("space-bd", &core.device_id, &cwd, None, false)
        .unwrap();
    for chat in chats {
        core.workspace
            .create_chat(chat, Some("space-bd"), None, None, None)
            .unwrap();
        // Titled up front so the auto-titler stays out of the way.
        core.workspace.rename_chat(chat, "Pre-titled").unwrap();
    }
    Rig {
        _tmp: tmp,
        core,
        prompts,
        cwd,
    }
}

fn run(rig: &Rig, chat: &str, prompt: &str, message_id: &str) {
    rig.core
        .doc_host
        .queue_command(
            chat,
            SessionCommandPayload::Run {
                request: RunRequest {
                    scheduled_task: None,
                    scheduled_notion_writes: None,
                    approval_rules: false,
                    prompt: prompt.into(),
                    harness: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: rig.cwd.clone(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: true,
                    attachments: Vec::new(),
                    worktree: None,
                    resume: None,
                    fork_from: None,
                    instructions: None,
                },
                message_id: message_id.into(),
            },
        )
        .expect("queue run");
}

async fn wait_for_prompts(rig: &Rig, count: usize) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let prompts = rig.prompts.lock().unwrap().clone();
        if prompts.len() >= count {
            return prompts;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for run {count}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Wait until the chat's `turns`-th assistant reply is complete.
async fn wait_for_turns(rig: &Rig, chat: &str, turns: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let done = rig
            .core
            .doc_host
            .open(chat)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .filter(|e| {
                e.role == mast_doc::MessageRole::Assistant
                    && e.status == Some(mast_doc::MessageStatus::Complete)
            })
            .count();
        if done >= turns {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for turn {turns}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// The user text the transcript holds for each user entry.
fn transcript_user_texts(rig: &Rig, chat: &str) -> Vec<String> {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .iter()
        .filter(|entry| entry.role == mast_doc::MessageRole::User)
        .flat_map(|entry| {
            entry.parts.iter().filter_map(|part| match part {
                mast_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

fn install_notice(rig: &Rig) {
    rig.core
        .sessions
        .set_chat_start_notice(mast_engine::sessions::ChatStartNotice {
            enabled: Arc::new(|| true),
            text: Arc::new(|_chat: &str| Some(NOTICE.to_owned())),
        });
}

#[tokio::test]
async fn the_index_leads_the_first_turn_only_and_never_enters_the_transcript() {
    let rig = rig(&["chat-a"]);
    install_notice(&rig);
    run(&rig, "chat-a", "hello", "msg-1");
    let prompts = wait_for_prompts(&rig, 1).await;
    assert_eq!(prompts[0], format!("{NOTICE}hello"));
    wait_for_turns(&rig, "chat-a", 1).await;
    run(&rig, "chat-a", "and again", "msg-2");
    let prompts = wait_for_prompts(&rig, 2).await;
    assert_eq!(prompts[1], "and again", "later turns carry no index");
    assert_eq!(
        transcript_user_texts(&rig, "chat-a"),
        ["hello", "and again"]
    );
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_first_turn_slash_command_keeps_its_leading_slash() {
    let rig = rig(&["chat-b"]);
    install_notice(&rig);
    run(&rig, "chat-b", "/compact", "msg-1");
    let prompts = wait_for_prompts(&rig, 1).await;
    assert_eq!(prompts[0], "/compact");
    rig.core.shutdown().await;
}

#[tokio::test]
async fn an_index_switched_on_after_the_first_turn_never_reaches_that_chat() {
    let rig = rig(&["chat-c"]);
    run(&rig, "chat-c", "hello", "msg-1");
    wait_for_prompts(&rig, 1).await;
    wait_for_turns(&rig, "chat-c", 1).await;
    install_notice(&rig);
    run(&rig, "chat-c", "again", "msg-2");
    let prompts = wait_for_prompts(&rig, 2).await;
    assert_eq!(prompts[1], "again");
    rig.core.shutdown().await;
}

#[tokio::test]
async fn the_wired_notice_stays_off_until_the_setting_is_on() {
    let rig = rig(&["chat-d"]);
    // `rpc_service` installs the engine's own notice callback.
    let _rpc = rig.core.rpc_service();
    run(&rig, "chat-d", "hello", "msg-1");
    let prompts = wait_for_prompts(&rig, 1).await;
    assert_eq!(prompts[0], "hello");
    rig.core.shutdown().await;
}

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

/// The assembled engine, the setting on, and a genuine result published by a
/// Background run: a normal chat gets the index, a Background run and a side
/// chat do not, and the transcript never shows it.
#[tokio::test(flavor = "multi_thread")]
async fn the_assembled_index_reaches_a_normal_chat_but_not_background_runs_or_side_chats() {
    let rig = rig(&["chat-n", "chat-bg", "run-pub"]);
    let root = std::path::PathBuf::from(&rig.cwd);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "t"]);
    std::fs::write(root.join("README.md"), "readme\n").unwrap();
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-q", "-m", "root"]);

    let rpc = rig.core.rpc_service();
    let client = mast_rpc::memory_client(rpc.clone());
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": "space-bd", "title": "Results",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    let item = derive_id("op-item", roles::ITEM);
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": "run-pub", "workItemId": item,
                    "operationId": "assign-pub" }),
        )
        .await
        .unwrap();
    for (chat, task, name) in [
        ("run-pub", "task-slack", "Slack"),
        ("chat-bg", "task-other", "Other"),
    ] {
        rig.core
            .workspace
            .set_chat_scheduled_task_info(
                chat,
                &ScheduledChat {
                    task_id: task.into(),
                    task_name: name.into(),
                    run_id: chat.into(),
                    output: ScheduledTaskOutput::Background,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let lease =
        rig.core
            .sessions
            .agent_tokens()
            .mint("run-pub", "run-x", "space-bd", &rig.core.device_id);
    std::fs::write(root.join("slack.md"), "Run: r1\n# Slack\nall quiet\n").unwrap();
    client
        .call(
            mast_proto::agent_tools::TOOL_CALL_METHOD,
            json!({ "token": lease.token(), "tool": "artifact_publish", "arguments": {
                "path": root.join("slack.md").to_string_lossy(), "title": "Results: Slack",
                "role": ScheduledRunResults::role_for("task-slack"), "note": "run",
                "idempotencyKey": "scheduled-run-r1" } }),
        )
        .await
        .unwrap();
    rig.core
        .scheduled_tasks
        .request(ScheduledTasksRequest::SetBriefingIndex { enabled: true })
        .await
        .unwrap();

    run(&rig, "chat-n", "hello", "m1");
    let prompts = wait_for_prompts(&rig, 1).await;
    assert!(
        prompts[0].starts_with("Latest briefings: Slack "),
        "{}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("briefing_read") && prompts[0].ends_with("hello"),
        "{}",
        prompts[0]
    );
    assert_eq!(transcript_user_texts(&rig, "chat-n"), ["hello"]);
    wait_for_turns(&rig, "chat-n", 1).await;

    run(&rig, "chat-bg", "hello", "m2");
    let prompts = wait_for_prompts(&rig, 2).await;
    assert_eq!(prompts[1], "hello", "a Background run gets no index");

    rig.core
        .workspace
        .create_side_chat("chat-side", "chat-n")
        .unwrap();
    run(&rig, "chat-side", "aside", "m3");
    let prompts = wait_for_prompts(&rig, 3).await;
    assert!(
        !prompts[2].contains("Latest briefings:"),
        "a side chat gets no index: {}",
        prompts[2]
    );
    drop(lease);
    drop(rpc);
    rig.core.shutdown().await;
}
