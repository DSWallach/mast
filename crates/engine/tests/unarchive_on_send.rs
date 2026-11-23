//! Sending a message to an archived chat revives it: `queue_command` flips the
//! workspace row's `archived` flag back off for message-bearing commands (Run,
//! Steer) — and only those; an Interrupt leaves the archive state alone.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_doc::{MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};

const CHAT: &str = "chat-unarchive";

/// Completes a one-line turn for any request.
struct AckHarness;

#[async_trait]
impl Harness for AckHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Ack"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-ua".into(),
                assistant_message_id: "a-1".into(),
                chat_id: None,
                space_id: None,
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("ack: {}", request.prompt),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-ua".into()),
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn complete_assistant_count(core: &EngineCore) -> usize {
    let entries: Vec<SessionMessageEntry> = core
        .doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default();
    entries
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

fn run_payload(message_id: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: false,
            prompt: "back from the archive".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "~".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            fork_from: None,
            instructions: None,
        },
        message_id: message_id.into(),
    }
}

fn archived(core: &EngineCore) -> bool {
    core.workspace
        .chat(CHAT)
        .expect("read chat row")
        .expect("chat row exists")
        .archived
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_a_message_unarchives_the_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(AckHarness));
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");

    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "createChat",
                "chatId": CHAT,
                "deviceId": core.device_id,
            }),
        )
        .await
        .expect("createChat");
    // Pre-title so the auto-titler's harness request stays out of the flow.
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");

    core.workspace
        .set_chat_archived(CHAT, true)
        .expect("archive chat");
    assert!(archived(&core), "precondition: chat is archived");

    // A Run command (the composer's send) revives the row synchronously.
    core.doc_host
        .queue_command(CHAT, run_payload("msg-ua-1"))
        .expect("queue run command");
    assert!(
        !archived(&core),
        "sending a message must unarchive the chat"
    );
    wait_for(|| complete_assistant_count(&core) == 1, "turn to complete").await;

    // A non-message command must NOT revive it.
    core.workspace
        .set_chat_archived(CHAT, true)
        .expect("re-archive chat");
    core.doc_host
        .queue_command(CHAT, SessionCommandPayload::Interrupt {})
        .expect("queue interrupt");
    assert!(
        archived(&core),
        "an interrupt is not a message and must not unarchive"
    );

    core.shutdown().await;
}

async fn rig_with_chat(tmp: &tempfile::TempDir) -> (EngineCore, mast_rpc::RpcClient) {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(AckHarness));
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace.rename_chat(CHAT, "Pre-titled").expect("rename chat");
    (core, client)
}

fn fail_command_inserts(core: &EngineCore, fail: bool) {
    core.sessions
        .agent_tokens()
        .manager_status()
        .set_test_hook(fail.then(|| {
            Arc::new(|event: &str, _: &str| event == "command_insert_failure")
                as Arc<dyn Fn(&str, &str) -> bool + Send + Sync>
        }));
}

/// A send whose command never lands must not leave the chat revived: the user
/// sees an error and the chat stays where it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_command_insert_leaves_the_chat_archived() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, _client) = rig_with_chat(&tmp).await;
    core.workspace.set_chat_archived(CHAT, true).expect("archive chat");

    fail_command_inserts(&core, true);
    let failed = core.doc_host.queue_command(CHAT, run_payload("msg-lost"));
    assert!(failed.is_err(), "the injected insert failure surfaces");
    assert!(archived(&core), "a send that never landed leaves the chat archived");
    assert!(
        core.doc_host.open(CHAT).unwrap().doc().read_commands().unwrap().is_empty(),
        "nothing was queued"
    );

    // A chat the user had already revived is not archived by a failed send.
    core.workspace.set_chat_archived(CHAT, false).expect("unarchive chat");
    assert!(core.doc_host.queue_command(CHAT, run_payload("msg-lost-2")).is_err());
    assert!(!archived(&core), "a failed send never archives a chat it did not revive");

    // The retry with the same message succeeds and revives, exactly once.
    fail_command_inserts(&core, false);
    core.workspace.set_chat_archived(CHAT, true).expect("archive chat");
    core.doc_host
        .queue_command(CHAT, run_payload("msg-lost"))
        .expect("the retried send lands");
    assert!(!archived(&core), "the retried send revives the chat");
    wait_for(|| complete_assistant_count(&core) == 1, "turn to complete").await;
    core.shutdown().await;
}

/// Another message already waiting for the chat keeps it in use: a failed send
/// beside it must not archive the chat under that message.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_insert_beside_queued_work_keeps_the_chat_revived() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, _client) = rig_with_chat(&tmp).await;
    core.workspace.set_chat_archived(CHAT, true).expect("archive chat");
    // Hold the drain so the host does not execute the waiting row meanwhile.
    core.sessions.agent_tokens().manager_status().set_test_hook(Some(Arc::new(
        |event: &str, _: &str| matches!(event, "command_insert_failure" | "hold_command_drain"),
    )));
    // A pending Run from a viewer that no host drain will pick up on its own.
    let handle = core.doc_host.open(CHAT).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    handle
        .doc()
        .queue_command(&mast_doc::SessionCommandEntry {
            id: "waiting".into(),
            payload: run_payload("msg-waiting"),
            issued_by: "someone-else".into(),
            issued_at: now,
            based_on: None,
            expires_at: Some(now + 3_600_000),
            status: mast_doc::SessionCommandStatus::Pending,
            resolution: None,
        })
        .unwrap();
    assert!(core.doc_host.queue_command(CHAT, run_payload("msg-failed")).is_err());
    assert!(
        !archived(&core),
        "queued work for the chat keeps it revived after a failed send"
    );
    fail_command_inserts(&core, false);
    core.shutdown().await;
}
