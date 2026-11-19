//! The briefing index reaches the agent with a chat's first turn only: never
//! in the transcript, never ahead of a leading slash command, and not again
//! on later turns.

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
    SteeringMode,
};

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
        .set_chat_start_notice(Arc::new(|_chat: &str| Some(NOTICE.to_owned())));
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
