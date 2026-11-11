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
    SandboxLevel, SideChatDelivery, SteeringMode, UserInputQuestion,
};

const MAIN: &str = "chat-main";
const SIDE: &str = "chat-side";

/// One process per turn; a run reports `resume` as its session, else a new
/// one, and answers every prompt with "answer N". `[hold]` keeps the turn
/// open until released (or interrupted); `[ask]` asks a yes/no question
/// first. With `fail_fork`, a forked run errors like a broken session.
struct TurnHarness {
    fork: bool,
    fail_fork: bool,
    requests: Mutex<Vec<RunRequest>>,
    answers: Mutex<Vec<Vec<String>>>,
    release: Arc<tokio::sync::Notify>,
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
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let n = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        if request.prompt.starts_with("[ask]") {
            let answers = (controls.request_input)(vec![UserInputQuestion {
                id: "q".into(),
                header: "Approve command".into(),
                question: "Run rm?".into(),
                options: vec!["Yes".into(), "No".into()],
                multi_select: false,
                allow_custom: false,
                command_approval: None,
            }])
            .await
            .unwrap_or_default();
            self.answers
                .lock()
                .unwrap()
                .push(answers.into_iter().flat_map(|a| a.labels).collect());
        }
        let session = request.resume.clone().unwrap_or_else(|| format!("s-{n}"));
        let started = AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: session.clone(),
            assistant_message_id: format!("a-{n}"),
        };
        if self.fail_fork && request.fork_from.is_some() {
            return Ok(futures::stream::iter(vec![
                Ok(started),
                Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some("tool_use ids were found without tool_result blocks".into()),
                    session_id: Some(session),
                }),
            ])
            .boxed());
        }
        let hold = request.prompt.starts_with("[hold]");
        let release = self.release.clone();
        let interrupt = controls.interrupt.clone();
        let done = futures::stream::once(async move {
            let mut status = DoneStatus::Completed;
            if hold {
                tokio::select! {
                    _ = release.notified() => {}
                    _ = interrupt.cancelled() => status = DoneStatus::Interrupted,
                }
            }
            Ok(AgentEvent::Done { status, result: None, error: None, session_id: Some(session) })
        });
        Ok(futures::stream::iter(vec![
            Ok(started),
            Ok(AgentEvent::TextDelta { text: format!("answer {n}") }),
        ])
        .chain(done)
        .boxed())
    }
}

struct Rig {
    core: EngineCore,
    harness: Arc<TurnHarness>,
    client: mast_rpc::RpcClient,
    cwd: String,
    data: std::path::PathBuf,
}

async fn rig(fork: bool) -> Rig {
    rig_with(fork, false).await
}

fn harness(fork: bool, fail_fork: bool) -> Arc<TurnHarness> {
    Arc::new(TurnHarness {
        fork,
        fail_fork,
        requests: Mutex::new(Vec::new()),
        answers: Mutex::new(Vec::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    })
}

async fn rig_with(fork: bool, fail_fork: bool) -> Rig {
    let tmp = tempfile::tempdir().unwrap().keep();
    let cwd = tmp.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let harness = harness(fork, fail_fork);
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
            serde_json::json!({
                "op": "createChat", "chatId": MAIN, "spaceId": "space",
                "config": { "harness": "mock", "sandbox": "workspace-write", "autoApprove": false },
            }),
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
        data: tmp.join("data"),
    }
}

impl Rig {
    async fn turn(&self, chat: &str, prompt: &str) {
        self.start(chat, prompt).await;
        self.settle(chat).await;
    }

    /// Queue a turn and wait for it to reach the harness (not to finish).
    async fn start(&self, chat: &str, prompt: &str) {
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
    assert!(absorbed.prompt.contains("Q: why a parser?\nA: answer 2"));
    assert!(absorbed.prompt.contains("Q: and the lexer?\nA: answer 3"));
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

fn queue_ids(rig: &Rig, chat: &str) -> Vec<String> {
    rig.core.doc_host.open(chat).unwrap().doc().read_queue().unwrap().into_iter().map(|row| row.id).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn side_chat_runs_read_only_and_never_waits_on_a_question() {
    let rig = rig(true).await;
    rig.turn(MAIN, "build the parser").await;
    rig.open_side_chat().await;
    let config = rig.core.workspace.chat(SIDE).unwrap().unwrap().config.unwrap();
    assert_eq!(config.sandbox, SandboxLevel::ReadOnly);
    assert!(config.auto_approve);
    // Its own run is confined whatever the request said, and the question
    // is declined at once instead of parking the turn.
    rig.turn(SIDE, "[ask] may I delete it?").await;
    let run = &rig.requests()[1];
    assert_eq!(run.sandbox, SandboxLevel::ReadOnly);
    assert!(run.auto_approve);
    assert_eq!(*rig.harness.answers.lock().unwrap(), [vec!["No".to_string()]]);
    // A fork is a first-run affair: follow-ups rebuilt from the last request
    // never fork again.
    assert_eq!(rig.core.sessions.last_request(SIDE).unwrap().fork_from, None);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_side_chat_interrupts_its_turn_and_drops_its_queue() {
    let rig = rig(true).await;
    rig.open_side_chat().await;
    rig.start(SIDE, "[hold] a long question").await;
    rig.core.doc_host.queue_message(SIDE, "a follow-up", Vec::new()).unwrap();
    assert_eq!(queue_ids(&rig, SIDE).len(), 1);
    rig.client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "setChatArchived", "chatId": SIDE, "archived": true }),
        )
        .await
        .unwrap();
    wait_for(
        || !rig.core.sessions.turn_in_flight(SIDE) && queue_ids(&rig, SIDE).is_empty(),
        "the archived side chat to stop",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.requests().len(), 1, "the follow-up never ran");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_archives_side_chats_left_without_a_pane() {
    let rig = rig(true).await;
    rig.open_side_chat().await;
    let data = rig.data.clone();
    rig.core.shutdown().await;
    drop(rig);
    let registry = HarnessRegistry::new();
    registry.register(harness(true, false));
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::Mock, None).unwrap();
    assert!(core.workspace.chat(SIDE).unwrap().unwrap().archived);
    assert!(!core.workspace.chat(MAIN).unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absorb_into_a_busy_main_chat_queues_one_row_and_deleting_it_rolls_the_marker_back() {
    let rig = rig(true).await;
    rig.turn(MAIN, "build the parser").await;
    rig.open_side_chat().await;
    rig.turn(SIDE, "why a parser?").await;
    rig.start(MAIN, "[hold] keep going").await;

    let first = rig.absorb().await.unwrap();
    assert_eq!(first.delivery, SideChatDelivery::Queue);
    assert_eq!(queue_ids(&rig, MAIN).len(), 1);
    // A second absorb while that row waits extends it rather than adding one.
    rig.turn(SIDE, "and the lexer?").await;
    let second = rig.absorb().await.unwrap();
    assert_eq!((second.delivery, second.exchanges), (SideChatDelivery::Queue, 2));
    let rows = rig.core.doc_host.open(MAIN).unwrap().doc().read_queue().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].text.contains("why a parser?") && rows[0].text.contains("and the lexer?"));

    // Deleting the row before delivery: nothing was absorbed after all.
    assert!(rig.core.doc_host.remove_queued_message(MAIN, &rows[0].id).await.unwrap());
    assert_eq!(rig.core.workspace.chat(SIDE).unwrap().unwrap().side_absorbed_through, None);
    let again = rig.absorb().await.unwrap();
    assert_eq!(again.exchanges, 2);

    // The held turn ends; the row delivers as the main chat's next turn.
    rig.harness.release.notify_one();
    wait_for(
        || rig.requests().iter().any(|r| r.prompt.contains("side chat context")),
        "the absorbed row to deliver",
    )
    .await;
    rig.settle(MAIN).await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_main_chat_mid_turn_is_never_forked() {
    let rig = rig(true).await;
    rig.turn(MAIN, "build the parser").await;
    rig.start(MAIN, "[hold] keep going").await;
    rig.open_side_chat().await;
    rig.turn(SIDE, "why a parser?").await;
    let first = &rig.requests()[2];
    assert_eq!(first.fork_from, None);
    assert!(first.prompt.contains("<main_chat_transcript>"));
    assert!(first.prompt.ends_with("\n\nwhy a parser?"));
    rig.harness.release.notify_one();
    rig.settle(MAIN).await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_fork_retries_once_seeded_with_the_main_transcript() {
    let rig = rig_with(true, true).await;
    rig.turn(MAIN, "build the parser").await;
    rig.open_side_chat().await;
    rig.start(SIDE, "why a parser?").await;
    wait_for(|| rig.requests().len() == 3, "the seeded retry").await;
    rig.settle(SIDE).await;
    let (forked, seeded) = (&rig.requests()[1], &rig.requests()[2]);
    assert_eq!(forked.fork_from.as_deref(), Some("s-1"));
    assert_eq!(seeded.fork_from, None);
    assert_eq!(seeded.resume, None);
    assert!(seeded.prompt.contains("<main_chat_transcript>"));
    assert!(seeded.prompt.ends_with("\n\nwhy a parser?"));
    assert_eq!(rig.user_texts(SIDE), ["why a parser?"]);
    wait_for(
        || rig.core.workspace.chat_harness_session(SIDE).is_some_and(|(id, _)| id == "s-3"),
        "the seeded session to be stored",
    )
    .await;
    rig.core.shutdown().await;
}
