//! "Ask on the side": a side chat forks the main chat's native session on
//! its first run (or is seeded with its transcript when the harness can't
//! fork), then resumes its own session; "Absorb" hands its new exchanges to
//! the main chat once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_doc::{MessagePart, MessageRole, SessionCommandPayload};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AbsorbSideChatReply, AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
    SandboxLevel, SideChatDelivery, SteeringMode,
};

const MAIN: &str = "chat-main";
const SIDE: &str = "chat-side";

/// One process per turn; a run reports `resume` as its session, else a new
/// one, and answers every prompt with "answer N".
struct TurnHarness {
    fork: bool,
    requests: Mutex<Vec<RunRequest>>,
}

#[async_trait]
impl Harness for TurnHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Turns"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn supports_session_fork(&self) -> bool {
        self.fork
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let n = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let session = request.resume.clone().unwrap_or_else(|| format!("s-{n}"));
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: session.clone(),
                assistant_message_id: format!("a-{n}"),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("answer {n}"),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session),
            }),
        ])
        .boxed())
    }
}

struct Rig {
    core: EngineCore,
    harness: Arc<TurnHarness>,
    client: mast_rpc::RpcClient,
    cwd: String,
}

async fn rig(fork: bool) -> Rig {
    let tmp = tempfile::tempdir().unwrap().keep();
    let cwd = tmp.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let harness = Arc::new(TurnHarness {
        fork,
        requests: Mutex::new(Vec::new()),
    });
    let registry = HarnessRegistry::new();
    registry.register(harness.clone());
    let core = EngineCore::assemble(&tmp.join("data"), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    let cwd = cwd.display().to_string();
    core.workspace
        .create_space("space", &core.device_id, &cwd, None, false)
        .expect("create space");
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": MAIN, "spaceId": "space" }),
        )
        .await
        .expect("create main chat");
    core.workspace
        .rename_chat(MAIN, "Main")
        .expect("title main");
    Rig {
        core,
        harness,
        client,
        cwd,
    }
}

impl Rig {
    async fn turn(&self, chat: &str, prompt: &str) {
        let before = self.harness.requests.lock().unwrap().len();
        let request = RunRequest {
            prompt: prompt.into(),
            harness: Some(HarnessId::Mock),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: self.cwd.clone(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            resume: None,
            fork_from: None,
            attachments: Vec::new(),
            worktree: None,
            instructions: None,
        };
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request,
                    message_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .expect("queue run");
        wait_for(
            || self.harness.requests.lock().unwrap().len() > before,
            "the turn to start",
        )
        .await;
        self.settle(chat).await;
    }

    async fn settle(&self, chat: &str) {
        wait_for(
            || !self.core.sessions.turn_in_flight(chat),
            "the turn to settle",
        )
        .await;
    }

    fn requests(&self) -> Vec<RunRequest> {
        self.harness.requests.lock().unwrap().clone()
    }

    fn user_texts(&self, chat: &str) -> Vec<String> {
        let entries = self
            .core
            .doc_host
            .open(chat)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        entries
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .map(|e| {
                e.parts
                    .iter()
                    .filter_map(|p| match p {
                        MessagePart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    async fn open_side_chat(&self) {
        self.client
            .call(
                mast_rpc::methods::MUTATE,
                serde_json::json!({ "op": "createChat", "chatId": SIDE, "sideOf": MAIN }),
            )
            .await
            .expect("create side chat");
    }

    async fn absorb(&self) -> Result<AbsorbSideChatReply, mast_rpc::RpcError> {
        self.client
            .call(
                mast_rpc::methods::ABSORB_SIDE_CHAT,
                serde_json::json!({ "chatId": SIDE }),
            )
            .await
            .map(|value| serde_json::from_value(value).unwrap())
    }
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_chat_forks_once_then_resumes_its_own_session_and_absorbs_once() {
    let rig = rig(true).await;
    rig.turn(MAIN, "build the parser").await;
    assert_eq!(
        rig.core.workspace.chat_harness_session(MAIN).unwrap().0,
        "s-1"
    );

    rig.open_side_chat().await;
    let side = rig
        .core
        .workspace
        .chat(SIDE)
        .unwrap()
        .expect("side chat row");
    assert_eq!(side.side_of.as_deref(), Some(MAIN));
    assert!(side.parent_chat_id.is_none());
    assert_eq!(side.space_id.as_deref(), Some("space"));
    assert_eq!(side.cwd.as_deref(), Some(rig.cwd.as_str()));
    assert_eq!(side.title.as_deref(), Some("Side chat"));

    // First run: a fork of the main chat's session, never a resume of it.
    rig.turn(SIDE, "why a parser?").await;
    let first = &rig.requests()[1];
    assert_eq!(first.fork_from.as_deref(), Some("s-1"));
    assert_eq!(first.resume, None);
    assert_eq!(first.prompt, "why a parser?");
    wait_for(
        || {
            rig.core
                .workspace
                .chat_harness_session(SIDE)
                .is_some_and(|(id, _)| id == "s-2")
        },
        "the forked session to be stored on the side chat",
    )
    .await;
    assert_eq!(
        rig.core.workspace.chat_harness_session(MAIN).unwrap().0,
        "s-1"
    );

    // Later runs resume the side chat's own session.
    rig.turn(SIDE, "and the lexer?").await;
    let second = &rig.requests()[2];
    assert_eq!(second.resume.as_deref(), Some("s-2"));
    assert_eq!(second.fork_from, None);

    // Absorb: both exchanges reach the idle main chat as one context turn.
    let reply = rig.absorb().await.expect("absorb");
    assert_eq!(reply.delivery, SideChatDelivery::Run);
    assert_eq!(reply.exchanges, 2);
    wait_for(
        || rig.requests().len() == 4,
        "the main chat to take the absorbed context",
    )
    .await;
    rig.settle(MAIN).await;
    let absorbed = &rig.requests()[3];
    assert_eq!(absorbed.resume.as_deref(), Some("s-1"));
    assert!(
        absorbed
            .prompt
            .starts_with("[Mast system message, not from the user: side chat context.")
    );
    assert!(
        absorbed
            .prompt
            .contains("**Q:** why a parser?\n\n**A:** answer 2")
    );
    assert!(
        absorbed
            .prompt
            .contains("**Q:** and the lexer?\n\n**A:** answer 3")
    );
    let marker = rig
        .core
        .workspace
        .chat(SIDE)
        .unwrap()
        .unwrap()
        .side_absorbed_through;
    assert_eq!(marker.as_deref(), Some(reply.absorbed_through.as_str()));

    // Nothing new: a second click never duplicates.
    assert!(
        rig.absorb()
            .await
            .unwrap_err()
            .to_string()
            .contains("nothing new")
    );
    rig.turn(SIDE, "one more?").await;
    let again = rig.absorb().await.expect("absorb the new exchange");
    assert_eq!(again.exchanges, 1);
    wait_for(|| rig.requests().len() == 6, "the second absorb to run").await;
    let prompt = &rig.requests()[5].prompt;
    assert!(prompt.contains("one more?") && !prompt.contains("why a parser?"));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_forking_the_first_prompt_carries_the_main_transcript() {
    let rig = rig(false).await;
    rig.turn(MAIN, "build the parser").await;
    rig.open_side_chat().await;
    rig.turn(SIDE, "why a parser?").await;
    let first = &rig.requests()[1];
    assert_eq!(first.fork_from, None);
    assert_eq!(first.resume, None);
    assert!(first.prompt.contains("<main_chat_transcript>\nUser: build the parser\n\nAssistant: answer 1\n</main_chat_transcript>"));
    assert!(first.prompt.ends_with("\n\nwhy a parser?"));
    // The side transcript shows the question alone.
    assert_eq!(rig.user_texts(SIDE), ["why a parser?"]);
    // Its own session is resumed afterwards, unseeded.
    rig.turn(SIDE, "and the lexer?").await;
    let second = &rig.requests()[2];
    assert_eq!(second.resume.as_deref(), Some("s-2"));
    assert_eq!(second.prompt, "and the lexer?");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_side_chat_is_not_a_worker_of_its_main_chat() {
    let rig = rig(true).await;
    rig.open_side_chat().await;
    let chats = rig.core.workspace.read_chats().unwrap();
    assert!(
        chats
            .iter()
            .all(|chat| chat.parent_chat_id.as_deref() != Some(MAIN))
    );
    // A side chat can't open a side chat of its own.
    let nested = rig
        .client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": "nested", "sideOf": SIDE }),
        )
        .await;
    assert!(nested.is_err());
    // A plain chat has nothing to absorb into.
    let plain = rig
        .client
        .call(
            mast_rpc::methods::ABSORB_SIDE_CHAT,
            serde_json::json!({ "chatId": MAIN }),
        )
        .await;
    assert!(plain.unwrap_err().to_string().contains("not a side chat"));
    rig.core.shutdown().await;
}
