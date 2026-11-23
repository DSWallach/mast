//! A worker turn that runs many tool calls gets one checkpoint nudge per
//! Space-configured spacing (default 150), recorded as a usage diagnostic.
//! Other chats and a spacing of 0 get none.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode, ToolCall,
};
use mast_rpc::{RpcClient, methods};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-1";

/// Feed-by-hand harness: each chat's run is served the feed registered under
/// its prompt, and every steer it receives is recorded against that prompt.
/// The auto-titler's side runs get an instantly completed empty stream.
struct FeedHarness {
    feeds: Mutex<HashMap<String, mpsc::UnboundedReceiver<AgentEvent>>>,
    steers: Arc<StdMutex<Vec<(String, String)>>>,
}

#[async_trait]
impl Harness for FeedHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Feed"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
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
        mut controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let Some(mut feed) = self.feeds.lock().await.remove(&request.prompt) else {
            return Ok(futures::stream::iter(vec![Ok(done())]).boxed());
        };
        let key = request.prompt.clone();
        let steers = self.steers.clone();
        let (tx, rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
        tokio::spawn(async move {
            let mut steering_open = true;
            loop {
                tokio::select! {
                    biased;
                    steer = controls.steering.recv(), if steering_open => match steer {
                        Some(message) => {
                            steers.lock().unwrap().push((key.clone(), message.prompt));
                            let boundary = AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            };
                            if tx.send(Ok(boundary)).await.is_err() {
                                return;
                            }
                        }
                        None => steering_open = false,
                    },
                    event = feed.recv() => match event {
                        Some(event) => {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn done() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: Some("hs-n".into()),
    }
}

fn run_request(prompt: &str) -> RunRequest {
    RunRequest {
        scheduled_task: None,
        scheduled_notion_writes: None,
        approval_rules: false,
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        fork_from: None,
        instructions: None,
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    steers: Arc<StdMutex<Vec<(String, String)>>>,
    feeds: HashMap<String, mpsc::UnboundedSender<AgentEvent>>,
}

/// A Manager, a worker under it and a chat the user started, each with its own feed.
async fn rig() -> Rig {
    let tmp = tempfile::Builder::new().prefix("mast-call-nudge-").tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let steers = Arc::new(StdMutex::new(Vec::new()));
    let mut receivers = HashMap::new();
    let mut feeds = HashMap::new();
    for chat in ["manager", "worker", "human"] {
        let (tx, rx) = mpsc::unbounded_channel();
        receivers.insert(chat.to_owned(), rx);
        feeds.insert(chat.to_owned(), tx);
    }
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FeedHarness { feeds: Mutex::new(receivers), steers: steers.clone() }));
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::Mock, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(methods::MUTATE, json!({"op":"createSpace","spaceId":SPACE,"deviceId":DEVICE,"path":root}))
        .await
        .unwrap();
    for (chat, parent) in [("manager", None), ("worker", Some("manager")), ("human", None)] {
        let mut params = json!({"op":"createChat","chatId":chat,"spaceId":SPACE,"deviceId":DEVICE});
        if let Some(parent) = parent {
            params["parentChatId"] = json!(parent);
        }
        client.call(methods::MUTATE, params).await.unwrap();
        // Pre-title so the auto-titler never dispatches a run of its own.
        core.workspace.rename_chat(chat, "Pre-titled").unwrap();
    }
    Rig { _tmp: tmp, core, client, steers, feeds }
}

async fn set_nudge(rig: &Rig, key: &str, calls: Option<u32>) {
    let mut params = json!({"op":"setWorkerCallNudge","spaceId":SPACE,"operationId":key});
    if let Some(calls) = calls {
        params["calls"] = json!(calls);
    }
    rig.client.call(methods::WORK_OUTLINE_MUTATE, params).await.unwrap();
}

async fn start(rig: &Rig, chat: &str) {
    rig.core
        .sessions
        .dispatch(chat, HarnessId::Mock, run_request(chat), None)
        .await
        .expect("dispatch");
}

/// Feed the distinct tool calls numbered `from..to` into the chat's run.
async fn run_calls(rig: &Rig, chat: &str, from: u32, to: u32) {
    for n in from..to {
        rig.feeds[chat]
            .send(AgentEvent::ToolCall {
                id: format!("{chat}-tool-{n}"),
                call: ToolCall::Exec { command: format!("echo {n}") },
            })
            .unwrap();
    }
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !predicate() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn nudges(rig: &Rig, chat: &str) -> Vec<String> {
    rig.steers
        .lock()
        .unwrap()
        .iter()
        .filter(|(key, text)| key == chat && mast_proto::is_relayed_or_system_message(text) && text.contains("tool-call checkpoint"))
        .map(|(_, text)| text.clone())
        .collect()
}

fn ledger_nudges(rig: &Rig) -> u64 {
    let usage = rig.core.sessions.token_usage().expect("token usage is on");
    usage
        .snapshot(&Default::default())
        .unwrap()
        .diagnostics
        .iter()
        .find(|d| d.category == "callNudge")
        .map_or(0, |d| d.observations)
}

#[tokio::test]
async fn worker_turn_gets_one_nudge_at_150_and_another_at_300() {
    let rig = rig().await;
    start(&rig, "worker").await;

    run_calls(&rig, "worker", 0, 149).await;
    // Give the engine time to consume 149 calls: still no nudge.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(nudges(&rig, "worker").is_empty());

    run_calls(&rig, "worker", 149, 150).await;
    wait_for(|| nudges(&rig, "worker").len() == 1, "the nudge at 150").await;
    assert!(nudges(&rig, "worker")[0].contains("150 tool calls"));

    run_calls(&rig, "worker", 150, 299).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(nudges(&rig, "worker").len(), 1, "nothing between thresholds");

    run_calls(&rig, "worker", 299, 300).await;
    wait_for(|| nudges(&rig, "worker").len() == 2, "the nudge at 300").await;
    assert!(nudges(&rig, "worker")[1].contains("300 tool calls"));
    wait_for(|| ledger_nudges(&rig) == 2, "two nudge diagnostics in the usage ledger").await;

    // The turn ends; the count starts again.
    rig.feeds["worker"].send(done()).unwrap();
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_chat_that_is_not_a_worker_gets_no_nudge() {
    let rig = rig().await;
    start(&rig, "human").await;
    start(&rig, "manager").await;
    for chat in ["human", "manager"] {
        run_calls(&rig, chat, 0, 320).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(nudges(&rig, "human").is_empty());
    assert!(nudges(&rig, "manager").is_empty());
    assert_eq!(ledger_nudges(&rig), 0);
    rig.core.shutdown().await;
}

#[tokio::test]
async fn setting_zero_turns_the_nudge_off_and_a_count_sets_the_spacing() {
    let rig = rig().await;
    set_nudge(&rig, "off", Some(0)).await;
    start(&rig, "worker").await;
    run_calls(&rig, "worker", 0, 320).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(nudges(&rig, "worker").is_empty(), "0 disables the nudge");
    rig.feeds["worker"].send(done()).unwrap();
    rig.core.shutdown().await;

    // A chosen count sets the spacing: 100 nudges at 100.
    let rig = rig_with_spacing(100).await;
    start(&rig, "worker").await;
    run_calls(&rig, "worker", 0, 100).await;
    wait_for(|| nudges(&rig, "worker").len() == 1, "the nudge at 100").await;
    assert!(nudges(&rig, "worker")[0].contains("100 tool calls"));
    rig.core.shutdown().await;
}

async fn rig_with_spacing(calls: u32) -> Rig {
    let rig = rig().await;
    set_nudge(&rig, "spacing", Some(calls)).await;
    rig
}

#[tokio::test]
async fn counts_outside_the_offered_list_are_refused() {
    let rig = rig().await;
    let refused = rig
        .client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({"op":"setWorkerCallNudge","spaceId":SPACE,"calls":7,"operationId":"bad"}),
        )
        .await;
    assert!(refused.is_err(), "7 is not an offered count");
    let _: Value = rig
        .client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({"op":"setWorkerCallNudge","spaceId":SPACE,"calls":250,"operationId":"ok"}),
        )
        .await
        .unwrap();
    rig.core.shutdown().await;
}
