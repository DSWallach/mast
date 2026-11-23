//! Work Outline agent tools (Milestone 4) against a real offline engine:
//! - a Run of a Chat in a Space gets the Mast MCP injection with a token
//!   bound to that Chat and Run, revoked when the Run ends; a projectless
//!   Chat's Run gets plain `run()` and no token;
//! - `AgentToolCall` authenticates by token only (forged / absent / revoked
//!   tokens are refused) and enforces the agent policy per tool;
//! - mutations run as `Actor { kind: agent, chatId }` and dedupe by
//!   `(token Chat, idempotencyKey)` across Runs.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use mast_doc::SessionCommandPayload;
use mast_engine::agent_tools::AgentToolsLaunch;
use mast_engine::diff_targets::DiffTargets;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{AgentToolsInjection, Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode, WorkOutlineActorKind, WorkOutlineSnapshot, derive_id, roles,
};
use mast_rpc::{RpcClient, methods};

const SPACE: &str = "space-1";
const CHAT: &str = "chat-agent";
const OTHER_CHAT: &str = "chat-other";
const PROJECTLESS: &str = "chat-projectless";

#[derive(Debug, Clone)]
enum Spawn {
    Plain {
        prompt: String,
    },
    WithTools {
        prompt: String,
        tools: AgentToolsInjection,
    },
}

/// Records how each Run was spawned and holds the Run open until released.
#[derive(Clone, Default)]
struct GatedHarness {
    spawns: Arc<Mutex<Vec<Spawn>>>,
    /// Every Run request as the harness received it.
    requests: Arc<Mutex<Vec<RunRequest>>>,
    release: Arc<Notify>,
    questions: Arc<Mutex<Vec<mast_proto::UserInputQuestion>>>,
    events: Arc<Mutex<Vec<AgentEvent>>>,
    fake_codex: bool,
    answers: Arc<Mutex<Vec<Vec<mast_proto::UserInputAnswer>>>>,
    /// A steerable, parking harness fed by hand, for warm-session schedules.
    warm: Option<Arc<Warm>>,
}

/// The hand-fed side of a warm [`GatedHarness`]: the current run's event feed
/// and every message the run received (its prompt, then each steer).
#[derive(Default)]
struct Warm {
    feed: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    received: Mutex<Vec<String>>,
    /// Runs the instant the run's mailbox yields a steered prompt, before the
    /// harness confirms it: what the agent could do the moment it sees it.
    on_steer: Mutex<Option<Box<dyn Fn() + Send>>>,
    hold_steer_confirmation: std::sync::atomic::AtomicBool,
    fail_first_start: Mutex<Option<Arc<Notify>>>,
    fail_before_session_start: Mutex<Option<Arc<Notify>>>,
}

impl GatedHarness {
    fn stream(&self, request: &RunRequest) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
        let release = self.release.clone();
        let cwd = request.cwd.clone();
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock".into(),
            tools: vec![],
            cwd,
            session_id: "sess-agent".into(),
            assistant_message_id: "a-1".into(),
            chat_id: None,
            space_id: None,
        })]);
        let done = futures::stream::once(async move {
            release.notified().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-agent".into()),
            })
        });
        let events = futures::stream::iter(self.events.lock().unwrap().clone().into_iter().map(Ok));
        started.chain(events).chain(done).boxed()
    }
}

#[async_trait]
impl Harness for GatedHarness {
    fn id(&self) -> HarnessId {
        if self.fake_codex { HarnessId::Codex } else { HarnessId::Mock }
    }
    fn display_name(&self) -> &str {
        "Gated"
    }
    fn supports_steering(&self) -> bool {
        self.warm.is_some()
    }
    fn steering_mode(&self) -> SteeringMode {
        if self.warm.is_some() { SteeringMode::StepBoundary } else { SteeringMode::TurnBoundary }
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    fn supports_agent_tools(&self) -> bool {
        true
    }
    fn instruction_support(&self) -> mast_harness::InstructionSupport {
        mast_harness::InstructionSupport::Supported {
            channel: mast_harness::InstructionChannel::Test,
            replace_strategy: mast_harness::InstructionReplaceStrategy::RespawnResume,
            max_bytes: 64 * 1024,
        }
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.spawns.lock().unwrap().push(Spawn::Plain {
            prompt: request.prompt.clone(),
        });
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.stream(&request))
    }
    async fn run_with_agent_tools(
        &self,
        request: RunRequest,
        mut controls: RunControls,
        tools: AgentToolsInjection,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if let Some(warm) = self.warm.clone() {
            self.spawns.lock().unwrap().push(Spawn::WithTools { prompt: request.prompt.clone(), tools });
            self.requests.lock().unwrap().push(request.clone());
            let fail = warm.fail_first_start.lock().unwrap().take();
            if let Some(fail) = fail {
                fail.notified().await;
                return Err(HarnessError::Protocol("harness stopped during startup".into()));
            }
            let fail = warm.fail_before_session_start.lock().unwrap().take();
            if let Some(fail) = fail {
                return Ok(futures::stream::once(async move {
                    fail.notified().await;
                    Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some("temporary child startup failure".into()),
                        session_id: None,
                    })
                }).boxed());
            }
            warm.received.lock().unwrap().push(request.prompt.clone());
            let (feed_tx, mut feed_rx) = mpsc::unbounded_channel::<AgentEvent>();
            *warm.feed.lock().unwrap() = Some(feed_tx);
            let (out_tx, out_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
            let started = AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-warm".into(),
                assistant_message_id: "a-warm".into(),
                chat_id: None,
                space_id: None,
            };
            tokio::spawn(async move {
                if out_tx.send(Ok(started)).await.is_err() {
                    return;
                }
                loop {
                    tokio::select! {
                        // A steer is confirmed with a Steered boundary before any
                        // event the test feeds after it.
                        biased;
                        _ = controls.interrupt.cancelled() => {
                            let _ = out_tx.send(Ok(AgentEvent::Done {
                                status: DoneStatus::Interrupted,
                                result: None,
                                error: None,
                                session_id: None,
                            })).await;
                            return;
                        }
                        steer = controls.steering.recv() => match steer {
                            Some(message) => {
                                if let Some(act) = warm.on_steer.lock().unwrap().as_ref() {
                                    act();
                                }
                                warm.received.lock().unwrap().push(message.prompt);
                                if warm.hold_steer_confirmation.load(std::sync::atomic::Ordering::SeqCst) {
                                    continue;
                                }
                                let boundary = AgentEvent::Steered {
                                    assistant_message_id: None,
                                    next_assistant_message_id: None,
                                };
                                if out_tx.send(Ok(boundary)).await.is_err() {
                                    return;
                                }
                            }
                            None => return,
                        },
                        event = feed_rx.recv() => match event {
                            Some(event) => {
                                if out_tx.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                            None => return,
                        },
                    }
                }
            });
            return Ok(futures::stream::unfold(out_rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed());
        }
        let questions = self.questions.lock().unwrap().clone();
        if !questions.is_empty() {
            let answer = (controls.request_input)(questions);
            let answers = self.answers.clone();
            tokio::spawn(async move { if let Ok(answer) = answer.await { answers.lock().unwrap().push(answer); } });
        }
        self.spawns.lock().unwrap().push(Spawn::WithTools {
            prompt: request.prompt.clone(),
            tools,
        });
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.stream(&request))
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    harness: GatedHarness,
    space_path: String,
    /// The service behind `client` (its agent archive log lives here).
    service: Arc<mast_engine::EngineRpc>,
}

async fn rig() -> Rig {
    rig_for(false).await
}

async fn rig_for(fake_codex: bool) -> Rig {
    rig_with_harness(GatedHarness { fake_codex, ..Default::default() }).await
}

async fn rig_with_harness(harness: GatedHarness) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let space_path = root.join("project");
    std::fs::create_dir_all(&space_path).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness.clone()));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    // No IPC port is served here: name one explicitly (the default dials the
    // port the engine bound, and injects nothing without one).
    core.sessions.set_agent_tools_launch(Some(AgentToolsLaunch {
        command: "/opt/zeron/bin/zeron".into(),
        args: vec!["mcp".into()],
        ipc_port: Some(27999),
    }));
    let service = core.rpc_service();
    let client = mast_rpc::memory_client(service.clone());
    let space_path = space_path.to_string_lossy().into_owned();
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": space_path }),
        )
        .await
        .unwrap();
    // Legacy unassociated chats exercise the authorization boundary. Tests
    // for current user creation go through Mutate below.
    for chat in [CHAT, OTHER_CHAT] {
        core.workspace.create_chat(chat, Some(SPACE), Some(&core.device_id), None, None).unwrap();
    }
    core.workspace.create_chat(PROJECTLESS, None, Some(&core.device_id), None, None).unwrap();
    Rig {
        _tmp: tmp,
        core,
        client,
        harness,
        space_path,
        service,
    }
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn run_command(prompt: &str, cwd: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: false,
            prompt: prompt.into(),
            harness: Some(HarnessId::Mock),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            resume: None,
            fork_from: None,
            attachments: Vec::new(),
            worktree: None,
            instructions: None,
        },
        message_id: uuid::Uuid::new_v4().to_string(),
    }
}

async fn create_item(rig: &Rig, op: &str, title: &str, parent: Option<&str>) -> String {
    let mut params = json!({ "op": "createWorkItem", "spaceId": SPACE, "title": title,
                             "operationId": op });
    if let Some(parent) = parent {
        params["parentWorkItemId"] = json!(parent);
    }
    rig.client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .unwrap();
    derive_id(op, roles::ITEM)
}

async fn tool(rig: &Rig, token: &str, name: &str, arguments: Value) -> Result<Value, String> {
    rig.client
        .call(
            mast_proto::agent_tools::TOOL_CALL_METHOD,
            json!({ "token": token, "tool": name, "arguments": arguments }),
        )
        .await
        .map_err(|e| e.to_string())
}

fn snapshot(rig: &Rig) -> WorkOutlineSnapshot {
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    mast_doc::materialize_work_outline(SPACE, &inputs)
}

#[tokio::test(flavor = "multi_thread")]
async fn new_and_recovered_chats_get_run_scoped_tools() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();

    rig.core
        .doc_host
        .queue_command(CHAT, run_command("hello", &rig.space_path))
        .unwrap();
    wait_for(
        || !rig.harness.spawns.lock().unwrap().is_empty(),
        "space run",
    )
    .await;
    let spawn = rig.harness.spawns.lock().unwrap()[0].clone();
    let Spawn::WithTools { prompt, tools } = spawn else {
        panic!("a Chat in a Space must get agent tools: {spawn:?}");
    };
    assert_eq!(prompt, "hello");
    assert_eq!(tools.server_name, "mast-agent");
    assert_eq!(tools.args, vec!["mcp".to_owned()]);
    let token = tools.env["MAST_AGENT_TOKEN"].clone();
    assert_eq!(tools.env["MAST_IPC_PORT"], "27999");
    assert_eq!(tokens.live_count(), 1);

    let origin = rig
        .client
        .call(
            mast_proto::agent_tools::AUTHENTICATE_METHOD,
            json!({ "token": token }),
        )
        .await
        .expect("a live Run's token authenticates");
    assert_eq!(origin["chatId"], CHAT);
    assert_eq!(origin["spaceId"], SPACE);
    assert_eq!(origin["deviceId"], rig.core.device_id);
    assert!(!origin["runId"].as_str().unwrap().is_empty());

    // The Run ends: its token is revoked with it.
    rig.harness.release.notify_one();
    wait_for(|| tokens.live_count() == 0, "token revocation").await;
    let err = rig
        .client
        .call(
            mast_proto::agent_tools::AUTHENTICATE_METHOD,
            json!({ "token": token }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("agent-tools/unauthenticated"), "{err}");
    let err = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/unauthenticated"), "{err}");

    // First-command recovery gives a folderless chat work ownership too.
    rig.core
        .doc_host
        .queue_command(PROJECTLESS, run_command("solo", "~"))
        .unwrap();
    wait_for(
        || rig.harness.spawns.lock().unwrap().len() == 2,
        "projectless run",
    )
    .await;
    let spawn = rig.harness.spawns.lock().unwrap()[1].clone();
    assert!(
        matches!(&spawn, Spawn::WithTools { prompt, .. } if prompt == "solo"),
        "{spawn:?}"
    );
    assert_eq!(tokens.live_count(), 1);
    let personal = rig.core.workspace.chat(PROJECTLESS).unwrap().unwrap().space_id.unwrap();
    assert!(rig.core.workspace.chat_assigned_work_item(PROJECTLESS, &personal).is_some());
    rig.harness.release.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_tool_calls_are_recorded_on_the_completed_turn() {
    let rig = rig().await;
    rig.core.doc_host.queue_command(CHAT, run_command("hello", &rig.space_path)).unwrap();
    wait_for(|| !rig.harness.spawns.lock().unwrap().is_empty(), "run").await;
    let Spawn::WithTools { tools, .. } = rig.harness.spawns.lock().unwrap()[0].clone() else { panic!("missing tools") };
    let token = &tools.env["MAST_AGENT_TOKEN"];
    assert!(tool(&rig, token, "missing_tool", json!({})).await.is_err());
    assert!(tool(&rig, token, "missing_tool", json!({})).await.is_err());
    rig.harness.release.notify_one();
    wait_for(|| rig.core.sessions.session_status(CHAT).is_some_and(|s| s.status == mast_proto::SessionStatus::Idle), "completed turn").await;
    let session = rig.core.sessions.session_status(CHAT).unwrap();
    let failures = session.last_turn_tool_failures.unwrap();
    assert_eq!(failures.count, 2);
    assert_eq!(failures.tools, ["missing_tool"]);
    rig.core.doc_host.queue_command(CHAT, run_command("next", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 2, "second run").await;
    rig.harness.release.notify_one();
    wait_for(|| rig.core.sessions.session_status(CHAT).is_some_and(|s| s.status == mast_proto::SessionStatus::Idle && s.last_turn_tool_failures.is_none()), "clean completed turn").await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_or_absent_tokens_reach_no_tool() {
    let rig = rig().await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    for token in ["", "zat_forged", &format!("{}0", lease.token())] {
        let err = tool(&rig, token, "work_outline_read_current", json!({}))
            .await
            .unwrap_err();
        assert!(
            err.contains("agent-tools/unauthenticated"),
            "{token:?}: {err}"
        );
    }
    // A missing token field is a malformed call, never an anonymous one.
    assert!(
        rig.client
            .call(
                mast_proto::agent_tools::TOOL_CALL_METHOD,
                json!({ "tool": "work_outline_read_current" }),
            )
            .await
            .is_err()
    );
    // Unknown tools are refused even with a valid token.
    let err = tool(&rig, lease.token(), "delete_everything", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/unsupported"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn external_run_tools_record_update_end_and_list_runs_within_the_agents_subtree() {
    let rig = rig().await;
    let root = create_item(&rig, "run-root", "Feature", None).await;
    let child = create_item(&rig, "run-child", "Step", Some(&root)).await;
    let other = create_item(&rig, "run-other", "Elsewhere", None).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let call = |name: &'static str, args: Value| tool(&rig, &token, name, args);
    call("work_outline_associate_current_chat", json!({ "workItemId": root, "idempotencyKey": "assoc" }))
        .await
        .unwrap();

    // Recording defaults to the agent's own item, and repeating the call
    // converges on the same run instead of adding another.
    let first = call("work_item_external_run_upsert", json!({ "runTag": "run-1", "codespaceName": "box-1", "provider": "claude" }))
        .await
        .unwrap();
    assert_eq!(first["workItemId"], root);
    assert_eq!(first["runId"], mast_proto::external_run_id(&root, "run-1"));
    call("work_item_external_run_upsert", json!({ "runTag": "run-1", "codespaceName": "box-1", "provider": "claude" }))
        .await
        .unwrap();
    assert_eq!(snapshot(&rig).external_runs.len(), 1);

    // A descendant is in scope, with an explicit key; another subtree is not.
    call("work_item_external_run_upsert", json!({ "workItemId": child, "runTag": "run-2", "branch": "feature", "idempotencyKey": "k2" }))
        .await
        .unwrap();
    let denied = call("work_item_external_run_upsert", json!({ "workItemId": other, "runTag": "x" }))
        .await
        .unwrap_err();
    assert!(denied.contains("work-outline/unauthorized"), "{denied}");
    assert_eq!(snapshot(&rig).external_runs.len(), 2);

    // A later call changes only the fields it names.
    call("work_item_external_run_upsert", json!({ "runTag": "run-1", "branch": "main" })).await.unwrap();
    let run = snapshot(&rig).external_runs.into_iter().find(|r| r.run_tag == "run-1").unwrap();
    assert_eq!(run.codespace_name.as_deref(), Some("box-1"));
    assert_eq!(run.branch.as_deref(), Some("main"));

    // Listing covers the subtree and hides ended runs unless asked.
    let listed = call("work_item_external_run_list", json!({})).await.unwrap();
    assert_eq!(listed["runs"].as_array().unwrap().len(), 2);
    let ended = call("work_item_external_run_upsert", json!({ "runTag": "run-1", "outcome": "done" })).await;
    ended.unwrap();
    let live = call("work_item_external_run_list", json!({})).await.unwrap();
    assert_eq!(live["runs"].as_array().unwrap().len(), 1);
    let all = call("work_item_external_run_list", json!({ "includeEnded": true })).await.unwrap();
    assert_eq!(all["runs"].as_array().unwrap().len(), 2);
    let run = snapshot(&rig).external_runs.into_iter().find(|r| r.run_tag == "run-1").unwrap();
    assert_eq!(run.outcome, Some(mast_proto::ExternalRunOutcome::Done));
    assert!(!run.expected_alive && run.ended_at.is_some());

    // An ended run needs reopen; a bad outcome is refused; reading elsewhere is refused.
    let revived = call("work_item_external_run_upsert", json!({ "runTag": "run-1", "expectedAlive": true }))
        .await
        .unwrap_err();
    assert!(revived.contains("reopen"), "{revived}");
    call("work_item_external_run_upsert", json!({ "runTag": "run-1", "expectedAlive": true, "reopen": true }))
        .await
        .unwrap();
    assert!(snapshot(&rig).external_runs.iter().any(|r| r.run_tag == "run-1" && r.expected_alive && r.ended_at.is_none()));
    let bad = call("work_item_external_run_upsert", json!({ "runTag": "run-1", "outcome": "exploded" }))
        .await
        .unwrap_err();
    assert!(bad.contains("outcome"), "{bad}");
    let foreign = call("work_item_external_run_list", json!({ "workItemId": other })).await.unwrap_err();
    assert!(foreign.contains("work-outline/unauthorized"), "{foreign}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tool_is_allowed_or_denied_per_the_agent_policy() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Feature A", None).await;
    let root_b = create_item(&rig, "op-b", "Feature B", None).await;
    let a_child = create_item(&rig, "op-a1", "A step", Some(&root_a)).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // Unassociated: reads show no item; subtree tools are refused.
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"], Value::Null);
    assert_eq!(read["chatId"], CHAT);
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "x", "parentWorkItemId": root_a, "idempotencyKey": "k0" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");

    // Associate ONLY the calling Chat.
    let associated = tool(
        &rig,
        &token,
        "work_outline_associate_current_chat",
        json!({ "workItemId": root_a, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();
    assert_eq!(associated["chatId"], CHAT);
    let snap = snapshot(&rig);
    let assignment = snap.assignments.iter().find(|a| a.chat_id == CHAT).unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(root_a.as_str()));
    let actor = assignment.assigned_by.as_ref().unwrap();
    assert_eq!(actor.kind, WorkOutlineActorKind::Agent);
    assert_eq!(actor.chat_id.as_deref(), Some(CHAT));
    assert!(snap.assignments.iter().all(|a| a.chat_id != OTHER_CHAT));
    let service = rig.core.rpc_service();
    let err = service
        .work_outline()
        .authorize_agent(
            CHAT,
            &serde_json::from_value(json!({ "op": "assignChatToWorkItem",
                "chatId": OTHER_CHAT, "workItemId": root_a, "operationId": "x" }))
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        err.code,
        mast_proto::WorkOutlineErrorCode::Unauthorized,
        "{err:?}"
    );

    // Read: the item, its path, and effective instructions.
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"]["id"], root_a);
    assert_eq!(read["workItem"]["children"][0]["id"], a_child);
    assert_eq!(read["effectiveInstructions"]["workItemId"], root_a);

    // Create a child under the Chat's item (default parent) and under a
    // descendant; never under another subtree or at the Space root.
    let created = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Agent step", "idempotencyKey": "child-1" }),
    )
    .await
    .unwrap();
    let child = created["workItemId"].as_str().unwrap().to_owned();
    assert_eq!(created["parentWorkItemId"], root_a);
    let view = snapshot(&rig).item(&child).cloned().unwrap();
    let creator = view.created_by.unwrap();
    assert_eq!(creator.kind, WorkOutlineActorKind::Agent);
    assert_eq!(creator.chat_id.as_deref(), Some(CHAT));
    tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Deeper", "parentWorkItemId": a_child, "idempotencyKey": "child-2",
                "status": "active" }),
    )
    .await
    .unwrap();
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Elsewhere", "parentWorkItemId": root_b, "idempotencyKey": "child-3" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let err = service
        .work_outline()
        .authorize_agent(
            CHAT,
            &serde_json::from_value(json!({ "op": "createWorkItem", "spaceId": SPACE,
                "title": "Root", "operationId": "x" }))
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        err.code,
        mast_proto::WorkOutlineErrorCode::Unauthorized,
        "{err:?}"
    );
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Done already", "status": "completed", "idempotencyKey": "child-4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");

    // Status: active/completed inside the subtree only.
    tool(&rig, &token, "work_outline_set_status", json!({
        "waitingUntil": 1800000000000_i64, "waitingReason": "Observe", "idempotencyKey": "wait-1"
    })).await.unwrap();
    assert_eq!(snapshot(&rig).item(&root_a).unwrap().waiting_until, Some(1800000000000));
    assert_eq!(snapshot(&rig).item(&root_a).unwrap().waiting_reason.as_deref(), Some("Observe"));
    assert!(tool(&rig, &token, "work_outline_set_status", json!({"waitingUntil": null})).await.is_err());
    tool(&rig, &token, "work_outline_set_status", json!({
        "waitingUntil": null, "waitingReason": null, "idempotencyKey": "wait-clear"
    })).await.unwrap();
    assert_eq!(snapshot(&rig).item(&root_a).unwrap().waiting_until, None);
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "active", "idempotencyKey": "s1" }),
    )
    .await
    .unwrap();
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "workItemId": child, "status": "completed", "idempotencyKey": "s2" }),
    )
    .await
    .unwrap();
    let snap = snapshot(&rig);
    assert_eq!(
        snap.item(&root_a).unwrap().status,
        mast_proto::WorkItemStatus::Active
    );
    assert_eq!(
        snap.item(&child).unwrap().status,
        mast_proto::WorkItemStatus::Completed
    );
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "workItemId": root_b, "status": "active", "idempotencyKey": "s3" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "pending", "idempotencyKey": "s4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");

    // Everything outside the allow-list is refused by the policy.
    for forbidden in [
        json!({ "op": "updateWorkItem", "workItemId": root_a,
                "changes": { "title": "Renamed" }, "operationId": "x" }),
        json!({ "op": "moveWorkItem", "workItemId": a_child, "operationId": "x" }),
        json!({ "op": "archiveWorkItem", "workItemId": a_child,
                "descendantArchivePolicy": { "type": "archiveDescendants" }, "operationId": "x" }),
        json!({ "op": "removeChatFromWorkItem", "chatId": CHAT, "operationId": "x" }),
        json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                "scope": { "type": "spaceRoot" }, "text": "x", "operationId": "x" }),
        json!({ "op": "configureExecution", "scope": { "type": "workItem", "workItemId": root_a },
                "configuration": null, "operationId": "x" }),
    ] {
        let mutation = serde_json::from_value(forbidden.clone()).unwrap();
        let err = service
            .work_outline()
            .authorize_agent(CHAT, &mutation)
            .unwrap_err();
        assert_eq!(
            err.code,
            mast_proto::WorkOutlineErrorCode::Unauthorized,
            "{forbidden}"
        );
    }

    // Requests: created on the Chat's item with the Chat as origin; an
    // item with an open request cannot be completed (never auto-resolved);
    // only the originating Chat resolves it.
    let request = tool(
        &rig,
        &token,
        "work_outline_create_user_action_request",
        json!({ "request": "Approve the plan?", "detail": "Two options", "idempotencyKey": "r1" }),
    )
    .await
    .unwrap();
    let request_id = request["userActionRequestId"].as_str().unwrap().to_owned();
    assert_eq!(request["workItemId"], root_a);
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"]["effectiveStatus"], "userActionRequired");
    assert_eq!(read["openUserActionRequests"][0]["id"], request_id);
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "s5" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/openUserActionRequests"), "{err}");
    let snap = snapshot(&rig);
    assert_eq!(
        snap.item(&root_a).unwrap().status,
        mast_proto::WorkItemStatus::Active
    );
    let other =
        rig.core
            .sessions
            .agent_tokens()
            .mint(OTHER_CHAT, "run-9", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        other.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "r2" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "resolution": "User approved in chat",
                "idempotencyKey": "r2" }),
    )
    .await
    .unwrap();
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "s6" }),
    )
    .await
    .unwrap();

    // Proposals: for the item, its parent, or the Space root; pending until
    // the user reviews them (no instruction is written).
    let proposal = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "target": "spaceRoot", "text": "Prefer small PRs", "idempotencyKey": "r3" }),
    )
    .await
    .unwrap();
    assert!(proposal["proposalId"].is_string(), "{proposal}");
    assert!(snapshot(&rig).instructions.is_empty());
    let err = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "target": "parent", "text": "x", "idempotencyKey": "r4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no parent"), "{err}");
    for name in [
        "work_outline_create_user_action_request",
        "work_outline_resolve_user_action_request",
        "work_outline_propose_instruction",
    ] {
        let err = tool(&rig, &token, name, json!({})).await.unwrap_err();
        assert!(err.contains("agent-tools/invalid"), "{name}: {err}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_with_one_key_dedupe_across_runs_and_conflict_on_new_inputs() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Root", None).await;
    let tokens = rig.core.sessions.agent_tokens();
    let first = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    tool(
        &rig,
        first.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();

    let err = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        json!({ "title": "Step" }),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("agent-tools/invalid") && err.contains("idempotencyKey"),
        "{err}"
    );

    let args = json!({ "title": "Step", "idempotencyKey": "step-1" });
    let a = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        args.clone(),
    )
    .await
    .unwrap();
    let b = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        args.clone(),
    )
    .await
    .unwrap();
    assert_eq!(a["workItemId"], b["workItemId"]);
    let count = |rig: &Rig| snapshot(rig).items.len();
    assert_eq!(count(&rig), 2);

    // A later Run of the same Chat retrying the same key: same item.
    drop(first);
    let second = tokens.mint(CHAT, "run-2", SPACE, &rig.core.device_id);
    let c = tool(&rig, second.token(), "work_outline_create_child", args)
        .await
        .unwrap();
    assert_eq!(a["workItemId"], c["workItemId"]);
    assert_eq!(count(&rig), 2);

    // Same key, different inputs: a conflict, never a second item.
    let err = tool(
        &rig,
        second.token(),
        "work_outline_create_child",
        json!({ "title": "Other", "idempotencyKey": "step-1" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/operationConflict"), "{err}");
    assert_eq!(count(&rig), 2);

    // Another Chat using the same key gets its own operation.
    let other = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    tool(
        &rig,
        other.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();
    let d = tool(
        &rig,
        other.token(),
        "work_outline_create_child",
        json!({ "title": "Step", "idempotencyKey": "step-1" }),
    )
    .await
    .unwrap();
    assert_ne!(a["workItemId"], d["workItemId"]);
    assert_eq!(count(&rig), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_first_turn_runs_with_the_child_chat_config_not_the_callers() {
    let rig = rig().await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let key = "spawn-reviewer";
    let child = derive_id(
        &mast_proto::agent_tools::operation_id(CHAT, key),
        roles::CHAT,
    );
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": child, "spaceId": SPACE,
                    "deviceId": rig.core.device_id, "parentChatId": CHAT,
                    "config": { "harness": "mock", "model": null, "reasoning": null,
                                "sandbox": "read-only", "autoApprove": false } }),
        )
        .await
        .unwrap();
    // A caller trying to smuggle a wider Run: only the prompt survives.
    let queued = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({
            "chatId": child,
            "idempotencyKey": key,
            "prompt": "review the diff",
            "command": { "type": "run", "messageId": "",
                "request": { "prompt": "x", "cwd": "/", "sandbox": "danger-full-access",
                             "autoApprove": true } },
            "sandbox": "danger-full-access",
            "autoApprove": true,
            "cwd": "/",
        }),
    )
    .await
    .unwrap();
    assert_eq!(queued["chatId"], child);
    wait_for(
        || !rig.harness.requests.lock().unwrap().is_empty(),
        "child run",
    )
    .await;
    let request = rig.harness.requests.lock().unwrap()[0].clone();
    assert_eq!(request.prompt, "review the diff");
    assert_eq!(request.sandbox, SandboxLevel::ReadOnly);
    assert!(!request.auto_approve);
    assert_eq!(request.cwd, rig.space_path);
    assert!(request.worktree.is_none());

    // Not a child of the caller, or the wrong key: refused.
    let err = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({ "chatId": OTHER_CHAT, "idempotencyKey": key, "prompt": "hi" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/forbidden"), "{err}");
    let err = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({ "chatId": child, "idempotencyKey": key, "prompt": "   " }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");
    rig.harness.release.notify_one();
}

async fn add_instruction(rig: &Rig, op: &str, scope: Value, text: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemInstruction", "spaceId": SPACE, "scope": scope,
                    "text": text, "operationId": op }),
        )
        .await
        .unwrap();
}

async fn assign_chat(rig: &Rig, op: &str, chat: &str, item: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": chat, "workItemId": item,
                    "operationId": op }),
        )
        .await
        .unwrap();
}

fn launch_args(key: &str, item: &str, sandbox: &str) -> Value {
    json!({
        "idempotencyKey": key, "workItemId": item, "prompt": "go",
        "config": { "harness": "mock", "model": null, "reasoning": null,
                    "modelOptions": {}, "sandbox": sandbox, "autoApprove": false },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn user_created_chats_own_an_item_and_retries_preserve_it() {
    let rig = rig().await;
    let params = json!({"op":"createChat", "chatId":"new-user-chat", "spaceId":SPACE});
    rig.client
        .call(methods::MUTATE, params.clone())
        .await
        .unwrap();
    let item = rig
        .core
        .workspace
        .chat_assigned_work_item("new-user-chat", SPACE)
        .unwrap();
    let count = snapshot(&rig).items.len();
    rig.client.call(methods::MUTATE, params).await.unwrap();
    assert_eq!(snapshot(&rig).items.len(), count);
    assert_eq!(
        rig.core
            .workspace
            .chat_assigned_work_item("new-user-chat", SPACE)
            .as_deref(),
        Some(item.as_str())
    );
    rig.client
        .call(
            methods::MUTATE,
            json!({"op":"renameChat", "chatId":"new-user-chat", "title":"Fix login"}),
        )
        .await
        .unwrap();
    assert_eq!(snapshot(&rig).item(&item).unwrap().title, "Fix login");
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"updateWorkItem", "workItemId":item, "changes":{"title":"Release project"}, "operationId":"custom-item-title"})).await.unwrap();
    rig.client
        .call(
            methods::MUTATE,
            json!({"op":"renameChat", "chatId":"new-user-chat", "title":"Investigate login"}),
        )
        .await
        .unwrap();
    assert_eq!(snapshot(&rig).item(&item).unwrap().title, "Release project");
    for params in [
        json!({"op":"createChat", "chatId":"new-child", "spaceId":SPACE, "parentChatId":"new-user-chat"}),
        json!({"op":"createChat", "chatId":"new-side", "sideOf":"new-user-chat"}),
    ] {
        rig.client
            .call(methods::MUTATE, params.clone())
            .await
            .unwrap();
        assert_eq!(
            rig.core
                .workspace
                .chat_assigned_work_item(params["chatId"].as_str().unwrap(), SPACE)
                .as_deref(),
            Some(item.as_str())
        );
    }
    assert_eq!(snapshot(&rig).items.len(), count);
}

#[tokio::test(flavor = "multi_thread")]
async fn folderless_user_chat_hands_off_in_its_own_scratch_directory() {
    let rig = rig().await;
    for id in ["personal-one", "personal-two"] {
        rig.client
            .call(
                methods::MUTATE,
                json!({"op":"createChat", "chatId":id, "deviceId":rig.core.device_id}),
            )
            .await
            .unwrap();
    }
    let source = rig.core.workspace.chat("personal-one").unwrap().unwrap();
    let other = rig.core.workspace.chat("personal-two").unwrap().unwrap();
    assert_eq!(source.space_id, other.space_id);
    assert!(source.space_id.is_some());
    let space = source.space_id.unwrap();
    let item = rig
        .core
        .workspace
        .chat_assigned_work_item("personal-one", &space)
        .unwrap();
    assert_ne!(
        rig.core
            .workspace
            .chat_assigned_work_item("personal-two", &space)
            .unwrap(),
        item
    );
    let params = json!({"chatId":"personal-one", "summary":"Continue this task", "operationId":"solo-personal-handoff"});
    let reply = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, params.clone())
        .await
        .unwrap();
    let successor = reply["chatId"].as_str().unwrap();
    let root = reply["executionRoot"].as_str().unwrap();
    assert!(root.ends_with("/projectless/personal-one"), "{root}");
    assert_eq!(
        rig.core
            .workspace
            .chat(successor)
            .unwrap()
            .unwrap()
            .cwd
            .as_deref(),
        Some(root)
    );
    assert_eq!(
        rig.core
            .workspace
            .chat_assigned_work_item(successor, &space)
            .as_deref(),
        Some(item.as_str())
    );
    let retry = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, params)
        .await
        .unwrap();
    assert_eq!(retry["chatId"], successor);
    assert!(
        !rig.core
            .workspace
            .chat("personal-one")
            .unwrap()
            .unwrap()
            .archived
    );
    wait_for(
        || {
            rig.harness
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.prompt.contains("Continue this task"))
        },
        "personal successor",
    )
    .await;
    let requests = rig.harness.requests.lock().unwrap();
    let request = requests
        .iter()
        .find(|r| r.prompt.contains("Continue this task"))
        .unwrap();
    assert_eq!(request.cwd, root);
    assert!(request.prompt.contains("source chat personal-one"));
    drop(requests);
    rig.core
        .workspace
        .set_chat_archived("personal-one", true)
        .unwrap();
    let next = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({"chatId":successor, "summary":"Continue again", "operationId":"second-personal-handoff"})).await.unwrap();
    assert_eq!(next["executionRoot"], root);
    let next_id = next["chatId"].as_str().unwrap();
    rig.core
        .workspace
        .set_chat_archived(successor, true)
        .unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(next_id, "read-predecessors", &space, &rig.core.device_id);
    let scope = tool(&rig, lease.token(), "chat_project_scope_chats", json!({}))
        .await
        .unwrap();
    assert_eq!(
        scope["handoffSourceChatIds"],
        json!([successor, "personal-one"])
    );
    assert!(
        !scope["handoffSourceChatIds"]
            .as_array()
            .unwrap()
            .contains(&json!("personal-two"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_solo_handoff_creates_an_item_and_keeps_its_checkout() {
    let rig = rig().await;
    let checkout = Path::new(&rig.space_path).join("existing-worktree");
    std::fs::create_dir_all(&checkout).unwrap();
    rig.core
        .workspace
        .set_chat_cwd(CHAT, checkout.to_str().unwrap())
        .unwrap();
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item(CHAT, SPACE)
            .is_none()
    );
    rig.core
        .managed_accounts
        .pin_chat(CHAT, HarnessId::Mock, "")
        .unwrap();
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({"chatId":CHAT, "summary":"Continue investigating", "operationId":"legacy-solo-handoff"})).await.unwrap();
    assert_eq!(reply["executionRoot"], checkout.to_str().unwrap());
    assert_eq!(
        rig.core
            .managed_accounts
            .chat_pin(reply["chatId"].as_str().unwrap(), HarnessId::Mock)
            .as_deref(),
        Some("")
    );
    let item = rig
        .core
        .workspace
        .chat_assigned_work_item(CHAT, SPACE)
        .unwrap();
    assert_eq!(
        rig.core
            .workspace
            .chat_assigned_work_item(reply["chatId"].as_str().unwrap(), SPACE)
            .as_deref(),
        Some(item.as_str())
    );
    rig.core
        .workspace
        .set_chat_archived(OTHER_CHAT, true)
        .unwrap();
    assert!(
        rig.client
            .call(
                methods::HAND_OFF_ORCHESTRATOR,
                json!({"chatId":OTHER_CHAT, "summary":"No", "operationId":"archived-handoff"})
            )
            .await
            .is_err()
    );
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item(OTHER_CHAT, SPACE)
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn first_command_recovers_work_item_without_create_chat_rpc() {
    let rig = rig().await;
    rig.core
        .doc_host
        .queue_command(
            "first-run-only",
            run_command("recover first run", &rig.space_path),
        )
        .unwrap();
    wait_for(
        || !rig.harness.requests.lock().unwrap().is_empty(),
        "recovered run",
    )
    .await;
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item("first-run-only", SPACE)
            .is_some()
    );
    let count = snapshot(&rig).items.len();
    rig.core
        .workspace
        .ensure_chat_work_item("first-run-only")
        .unwrap();
    assert_eq!(snapshot(&rig).items.len(), count);
}

#[tokio::test(flavor = "multi_thread")]
async fn assignment_failure_does_not_break_creation_retries_or_runs() {
    let rig = rig().await;
    // The chat task can drain the run the moment it lands in the doc. Assignment
    // must never be attempted while the chat is still archived, or the revived
    // chat runs without a work item.
    let archived_at_assignment = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let workspace = rig.core.workspace.clone();
        let seen = archived_at_assignment.clone();
        rig.core.workspace.set_before_ensure_chat_work_item(Arc::new(move |chat_id| {
            if chat_id == "archived-without-item"
                && workspace.chat(chat_id).ok().flatten().is_some_and(|chat| chat.archived)
            {
                seen.lock().unwrap().push(chat_id.to_string());
            }
            Ok(())
        }));
    }
    rig.core
        .workspace
        .create_chat("archived-without-item", Some(SPACE), None, None, None)
        .unwrap();
    rig.core
        .workspace
        .set_chat_archived("archived-without-item", true)
        .unwrap();
    rig.client
        .call(
            methods::MUTATE,
            json!({"op":"createChat", "chatId":"archived-without-item", "spaceId":SPACE}),
        )
        .await
        .unwrap();
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item("archived-without-item", SPACE)
            .is_none()
    );
    // createChat's own best-effort assignment legitimately hit the archived chat.
    archived_at_assignment.lock().unwrap().clear();
    rig.core
        .doc_host
        .queue_command(
            "archived-without-item",
            run_command("run despite assignment failure", &rig.space_path),
        )
        .unwrap();
    wait_for(
        || {
            rig.harness
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.prompt == "run despite assignment failure")
        },
        "run despite assignment failure",
    )
    .await;
    assert!(
        !rig.core
            .workspace
            .chat("archived-without-item")
            .unwrap()
            .unwrap()
            .archived,
        "sending a Run revives an archived chat via unarchive_on_send"
    );
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item("archived-without-item", SPACE)
            .is_some(),
        "revived chat receives a work item"
    );
    assert!(
        archived_at_assignment.lock().unwrap().is_empty(),
        "work item assignment was attempted while the chat was still archived"
    );

    // Per-workspace assignment failure during a Run must still not break the run.
    rig.core
        .workspace
        .create_chat("run-assignment-fails", Some(SPACE), None, None, None)
        .unwrap();
    rig.core
        .workspace
        .set_before_ensure_chat_work_item(Arc::new(|chat_id| {
            if chat_id == "run-assignment-fails" {
                Err("injected assignment failure".into())
            } else {
                Ok(())
            }
        }));
    rig.core
        .doc_host
        .queue_command(
            "run-assignment-fails",
            run_command("run despite injected assignment failure", &rig.space_path),
        )
        .unwrap();
    wait_for(
        || {
            rig.harness
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.prompt == "run despite injected assignment failure")
        },
        "run despite injected assignment failure",
    )
    .await;
    assert!(
        rig.core
            .workspace
            .chat_assigned_work_item("run-assignment-fails", SPACE)
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_folderless_chat_waits_for_its_host_to_initialize_work() {
    let rig = rig().await;
    let count = rig.core.workspace.read_spaces().unwrap().len();
    rig.client
        .call(
            methods::MUTATE,
            json!({"op":"createChat", "chatId":"remote-solo", "deviceId":"other-host"}),
        )
        .await
        .unwrap();
    assert!(
        rig.core
            .workspace
            .chat("remote-solo")
            .unwrap()
            .unwrap()
            .space_id
            .is_none()
    );
    assert!(rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({"chatId":"remote-solo", "summary":"Continue remotely", "operationId":"remote-solo-handoff"})).await.is_err());
    assert!(
        rig.core
            .workspace
            .chat("remote-solo")
            .unwrap()
            .unwrap()
            .space_id
            .is_none()
    );
    assert_eq!(rig.core.workspace.read_spaces().unwrap().len(), count);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hand_off_rpc_transfers_workers_and_replays() {
    let rig = rig().await;
    let item = create_item(&rig, "user-handoff-item", "Coordinator", None).await;
    assign_chat(&rig, "user-handoff-assignment", CHAT, &item).await;
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":"user-handoff-worker", "spaceId":SPACE,
        "deviceId":rig.core.device_id, "parentChatId":CHAT})).await.unwrap();
    let params = json!({"chatId": CHAT, "summary":"Continue coordinating", "operationId":"user-handoff"});
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params.clone()).await.unwrap();
    let successor = reply["chatId"].as_str().unwrap();
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params).await.unwrap();
    assert_eq!(retry["chatId"], successor);
    assert_eq!(rig.core.workspace.chat("user-handoff-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(successor));
    let source = rig.core.doc_host.open(CHAT).unwrap();
    let entries = source.doc().read_entries().unwrap();
    let marker = format!("[Mast hand-off: {successor}]");
    assert_eq!(entries.iter().filter(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text == &marker))).count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hand_off_moves_pin_and_roles_with_either_archive_choice() {
    use mast_proto::{SidebarPinChange, StandingRoleKind};
    for archive in [false, true] {
        let (rig, item) = role_rig("handoff-preferences").await;
        let workspace = &rig.core.workspace;
        let tokens = rig.core.sessions.agent_tokens();
        let grant = tokens.admin().grant(CHAT, SPACE, Some(600), true);
        let assistant = tokens
            .personal_assistant()
            .set(&workspace.chat(CHAT).unwrap().unwrap());
        let pins = || workspace.sidebar_preferences_snapshot().pinned_session_ids;
        let pin = |id: &str| {
            workspace
                .change_sidebar_pin(&SidebarPinChange::Pin {
                    session_id: id.into(),
                    before: None,
                    after: None,
                })
                .unwrap()
        };
        pin(OTHER_CHAT);
        pin(CHAT);
        let original = pins();

        let params = json!({
            "chatId": CHAT, "summary": "Continue as manager", "operationId": "handoff-preferences",
            "archive": archive,
        });
        let reply = rig
            .client
            .call(methods::HAND_OFF_ORCHESTRATOR, params.clone())
            .await
            .unwrap();
        let successor = reply["chatId"].as_str().unwrap();
        assert_eq!(workspace.chat(CHAT).unwrap().unwrap().archived, archive);
        assert_eq!(
            workspace
                .standing_role_chat(StandingRoleKind::Manager, &item)
                .as_deref(),
            Some(successor)
        );
        assert_eq!(
            pins(),
            original
                .into_iter()
                .map(|id| if id == CHAT { successor.into() } else { id })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tokens.personal_assistant().get(),
            Some(mast_proto::PersonalAssistant {
                chat_id: successor.into(),
                ..assistant
            })
        );
        assert!(tokens.admin().live(CHAT).is_none());
        assert_eq!(
            tokens.admin().live(successor),
            Some(mast_proto::AdminGrant {
                chat_id: successor.into(),
                ..grant
            })
        );
        for (chat_id, is_successor) in [(CHAT, false), (successor, true)] {
            let lease = tokens.mint(chat_id, "handoff-auth", SPACE, &rig.core.device_id);
            let origin = rig
                .client
                .call(
                    mast_proto::agent_tools::AUTHENTICATE_METHOD,
                    json!({"token": lease.token()}),
                )
                .await
                .unwrap();
            assert_eq!(
                origin["personalAssistant"].as_bool().unwrap_or(false),
                is_successor
            );
            assert_eq!(
                origin["adminSpaceId"].as_str(),
                is_successor.then_some(SPACE)
            );
        }

        // Replays preserve edits made after the original hand-off.
        workspace.set_chat_archived(CHAT, false).unwrap();
        pin(CHAT);
        tokens
            .personal_assistant()
            .set(&workspace.chat(CHAT).unwrap().unwrap());
        let renewed = tokens.admin().grant(CHAT, SPACE, None, false);
        let edited_pins = pins();
        let retry = rig
            .client
            .call(methods::HAND_OFF_ORCHESTRATOR, params)
            .await
            .unwrap();
        assert_eq!(retry["chatId"], successor);
        assert!(!workspace.chat(CHAT).unwrap().unwrap().archived);
        assert_eq!(pins(), edited_pins);
        assert_eq!(tokens.admin().live(CHAT), Some(renewed));
        assert_eq!(tokens.personal_assistant().get().unwrap().chat_id, CHAT);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hand_off_retries_a_failed_role_write_after_restart() {
    for (file_name, error_text, admin_moved) in [
        (
            "admin-grants.json",
            "Could not save the admin hand-off",
            false,
        ),
        (
            "personal-assistant.json",
            "Could not save the personal assistant hand-off",
            true,
        ),
    ] {
        let rig = rig().await;
        let data = std::fs::canonicalize(rig._tmp.path()).unwrap().join("data");
        let tokens = rig.core.sessions.agent_tokens();
        let source = rig.core.workspace.chat(CHAT).unwrap().unwrap();
        let grant = tokens.admin().grant(CHAT, SPACE, None, true);
        let assistant = tokens.personal_assistant().set(&source);
        let role_file = data.join(file_name);
        let backup = role_file.with_extension("backup");
        std::fs::rename(&role_file, &backup).unwrap();
        std::fs::create_dir(&role_file).unwrap();

        let params = json!({
            "chatId": CHAT, "summary": "Continue with the same roles",
            "operationId": "handoff-role-write", "archive": true,
        });
        let successor = derive_id("handoff-role-write", roles::CHAT);
        let err = rig
            .client
            .call(methods::HAND_OFF_ORCHESTRATOR, params)
            .await
            .unwrap_err();
        assert!(err.to_string().contains(error_text), "{err}");
        assert_eq!(tokens.admin().live(CHAT).is_none(), admin_moved);
        assert_eq!(tokens.admin().live(&successor).is_some(), admin_moved);
        assert_eq!(tokens.personal_assistant().get(), Some(assistant.clone()));

        // A committed store must keep later edits even while the other
        // store's original transfer still needs to be retried.
        let renewed = admin_moved.then(|| tokens.admin().grant(CHAT, SPACE, None, false));
        drop(tokens);
        let Rig {
            _tmp,
            core,
            client,
            service,
            ..
        } = rig;
        drop(client);
        drop(service);
        core.shutdown().await;
        drop(core);
        std::fs::remove_dir(&role_file).unwrap();
        std::fs::rename(backup, role_file).unwrap();

        let registry = HarnessRegistry::new();
        registry.register(Arc::new(GatedHarness::default()));
        let restarted =
            EngineCore::assemble(&data, Arc::new(registry), HarnessId::Mock, None).unwrap();
        // A new client needs neither the summary nor the old operation ID.
        let tokens = restarted.sessions.agent_tokens();
        assert!(restarted.workspace.chat(CHAT).unwrap().unwrap().archived);
        assert_eq!(tokens.admin().live(CHAT), renewed);
        assert_eq!(
            tokens.admin().live(&successor),
            Some(mast_proto::AdminGrant {
                chat_id: successor.clone(),
                ..grant
            })
        );
        assert_eq!(
            tokens.personal_assistant().get(),
            Some(mast_proto::PersonalAssistant {
                chat_id: successor,
                ..assistant
            })
        );
        restarted.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hand_off_archives_plain_chat_without_assigning_roles_or_pin() {
    let rig = rig().await;
    let workspace = &rig.core.workspace;
    let tokens = rig.core.sessions.agent_tokens();
    let other = tokens
        .personal_assistant()
        .set(&workspace.chat(OTHER_CHAT).unwrap().unwrap());
    tokens.admin().grant(CHAT, SPACE, Some(1), false);
    tokens.admin().advance_clock_ms(1_000);

    let params = json!({
        "chatId": CHAT, "summary": "Continue plainly", "operationId": "plain-archive", "archive": true,
    });
    let reply = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, params.clone())
        .await
        .unwrap();
    let successor = reply["chatId"].as_str().unwrap();
    assert!(workspace.chat(CHAT).unwrap().unwrap().archived);
    assert!(
        workspace
            .sidebar_preferences_snapshot()
            .pinned_session_ids
            .is_empty()
    );
    assert!(tokens.admin().live(successor).is_none());
    assert_eq!(tokens.personal_assistant().get(), Some(other));
    let mut changed = params.clone();
    changed["archive"] = json!(false);
    let conflict = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, changed)
        .await
        .unwrap_err();
    assert!(
        conflict.to_string().contains("operationConflict"),
        "{conflict}"
    );
    assert!(workspace.chat(CHAT).unwrap().unwrap().archived);
    let retry = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, params)
        .await
        .unwrap();
    assert_eq!(retry["chatId"], successor);
    let refused = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": CHAT, "summary": "Another chat", "operationId": "archived-new-operation", "archive": false,
    })).await;
    assert!(refused.is_err());
}

async fn role_rig(prefix: &str) -> (Rig, String) {
    role_rig_in(rig().await, prefix).await
}

async fn role_rig_in(rig: Rig, prefix: &str) -> (Rig, String) {
    use mast_proto::StandingRoleKind;
    let item = create_item(&rig, &format!("{prefix}-item"), "Manager", None).await;
    assign_chat(&rig, &format!("{prefix}-assignment"), CHAT, &item).await;
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":format!("{prefix}-worker"), "spaceId":SPACE,
        "deviceId":rig.core.device_id, "parentChatId":CHAT})).await.unwrap();
    rig.core.workspace.adopt_standing_role(StandingRoleKind::Manager, &item, CHAT, &[]).unwrap();
    (rig, item)
}

#[tokio::test(flavor = "multi_thread")]
async fn role_hand_off_rotates_the_role_replays_and_refuses_a_second_successor() {
    use mast_proto::StandingRoleKind;
    let (rig, item) = role_rig("role-seq").await;
    let params = json!({"chatId": CHAT, "summary":"Continue as manager", "operationId":"role-handoff-a"});
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params.clone()).await.unwrap();
    let successor = reply["chatId"].as_str().unwrap().to_owned();
    let workspace = &rig.core.workspace;
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(successor.as_str()));
    let source = workspace.chat(CHAT).unwrap().unwrap();
    assert!(source.archived && source.is_standing_role_predecessor());
    assert_eq!(workspace.chat("role-seq-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));

    // The retry of the same operation still replays after the source was archived.
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params).await.unwrap();
    assert_eq!(retry["chatId"], successor.as_str());

    // A different successor for the replaced source creates, moves and queues nothing.
    let chats_before = workspace.read_chats().unwrap().len();
    let other = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": CHAT, "summary":"Continue elsewhere", "operationId":"role-handoff-b"
    })).await;
    assert!(other.is_err());
    assert_eq!(workspace.read_chats().unwrap().len(), chats_before);
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(successor.as_str()));
    assert_eq!(workspace.chat("role-seq-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Continue as manager")), "successor turn").await;
    assert!(!rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Continue elsewhere")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_off_replays_after_its_successor_handed_off_again() {
    use mast_proto::StandingRoleKind;
    let (rig, item) = role_rig("role-chain").await;
    let first = json!({"chatId": CHAT, "summary":"Chain step one", "operationId":"chain-a-to-b"});
    let b = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, first.clone()).await.unwrap()["chatId"].as_str().unwrap().to_owned();
    let second = json!({"chatId": b, "summary":"Chain step two", "operationId":"chain-b-to-c"});
    let c = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, second).await.unwrap()["chatId"].as_str().unwrap().to_owned();
    let workspace = &rig.core.workspace;
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(c.as_str()));
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Chain step two")), "second turn").await;
    let prompts = || rig.harness.requests.lock().unwrap().iter().filter(|r| r.prompt.contains("Chain step")).count();
    let (chats, turns) = (workspace.read_chats().unwrap().len(), prompts());

    // The retry of A to B returns B though B is archived and has handed off.
    let retry = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, first).await.unwrap();
    assert_eq!(retry["chatId"], b.as_str());
    assert_eq!(workspace.read_chats().unwrap().len(), chats, "no extra chat");
    assert_eq!(prompts(), turns, "no extra turn");
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(c.as_str()));
    assert_eq!(workspace.chat("role-chain-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(c.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_off_interrupted_after_queueing_is_finished_by_its_own_retry() {
    use mast_engine::work_outline_launch::LaunchStep;
    use mast_proto::StandingRoleKind;
    let (rig, item) = role_rig("role-crash").await;
    let workspace = &rig.core.workspace;
    let tokens = rig.core.sessions.agent_tokens();
    let grant = tokens.admin().grant(CHAT, SPACE, None, false);
    let assistant = tokens
        .personal_assistant()
        .set(&workspace.chat(CHAT).unwrap().unwrap());
    workspace
        .change_sidebar_pin(&mast_proto::SidebarPinChange::Pin {
            session_id: CHAT.into(),
            before: None,
            after: None,
        })
        .unwrap();
    let params = json!({"chatId": CHAT, "summary":"Crash-safe hand-off", "operationId":"crash-a", "archive":true});
    rig.service.inject_launch_fault(Some(LaunchStep::TurnQueued));
    assert!(rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params.clone()).await.is_err());

    // Workers and the first turn moved, but the role has not: the source still holds it,
    // and its reservation names the successor.
    let pending = workspace.pending_standing_hand_off(StandingRoleKind::Manager, &item).expect("reservation persisted");
    let successor = pending.to_chat_id.clone();
    assert_eq!(tokens.admin().live(CHAT), Some(grant));
    assert!(tokens.admin().live(&successor).is_none());
    assert_eq!(tokens.personal_assistant().get(), Some(assistant));
    assert_eq!(workspace.sidebar_preferences_snapshot().pinned_session_ids, [CHAT]);
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(CHAT));
    assert_eq!(workspace.chat("role-crash-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(successor.as_str()));
    assert!(!workspace.chat(CHAT).unwrap().unwrap().archived);

    // A different successor is refused before it creates a chat.
    let chats = workspace.read_chats().unwrap().len();
    let other = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": CHAT, "summary":"Different hand-off", "operationId":"crash-b"
    })).await;
    assert!(other.is_err());
    assert_eq!(workspace.read_chats().unwrap().len(), chats);

    let mut changed = params.clone();
    changed["archive"] = json!(false);
    let conflict = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, changed)
        .await
        .unwrap_err();
    assert!(
        conflict.to_string().contains("operationConflict"),
        "{conflict}"
    );
    assert!(!workspace.chat(CHAT).unwrap().unwrap().archived);

    // The same operation finishes: one successor, role moved, reservation cleared.
    let reply = rig.client.call(methods::HAND_OFF_ORCHESTRATOR, params).await.unwrap();
    assert_eq!(reply["chatId"], successor.as_str());
    assert!(tokens.admin().live(CHAT).is_none());
    assert!(tokens.admin().live(&successor).is_some());
    assert_eq!(
        tokens.personal_assistant().get().unwrap().chat_id,
        successor
    );
    assert_eq!(
        workspace.sidebar_preferences_snapshot().pinned_session_ids,
        [successor.as_str()]
    );
    assert!(workspace.chat(CHAT).unwrap().unwrap().archived);
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(successor.as_str()));
    assert_eq!(workspace.pending_standing_hand_off(StandingRoleKind::Manager, &item), None);
    assert!(workspace.chat(CHAT).unwrap().unwrap().is_standing_role_predecessor());
    assert_eq!(workspace.read_chats().unwrap().len(), chats);
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Crash-safe hand-off")), "successor turn").await;
    let turns = rig.harness.requests.lock().unwrap().iter().filter(|r| r.prompt.contains("Crash-safe hand-off")).count();
    assert_eq!(turns, 1);
    assert!(!rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Different hand-off")));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_hand_off_resumes_under_a_new_operation_id_with_the_same_inputs() {
    use mast_engine::work_outline_launch::LaunchStep;
    use mast_proto::StandingRoleKind;
    let (rig, item) = role_rig("role-resume").await;
    let workspace = &rig.core.workspace;
    rig.service.inject_launch_fault(Some(LaunchStep::TurnQueued));
    let first = json!({
        "chatId": CHAT, "summary": "Resume me", "operationId": "resume-a", "archive": false,
    });
    assert!(rig.client.call(methods::HAND_OFF_ORCHESTRATOR, first).await.is_err());
    let successor = workspace.pending_standing_hand_off(StandingRoleKind::Manager, &item).unwrap().to_chat_id;
    let chats = workspace.read_chats().unwrap().len();

    let changed = json!({
        "chatId": CHAT, "summary": "Resume me", "operationId": "resume-changed", "archive": true,
    });
    assert!(rig.client.call(methods::HAND_OFF_ORCHESTRATOR, changed).await.is_err());
    assert_eq!(workspace.read_chats().unwrap().len(), chats);

    // A reopened dialog or a restarted Mast sends a fresh operation id, same inputs.
    let again = json!({
        "chatId": CHAT, "summary": "Resume me", "operationId": "resume-fresh-id", "archive": false,
    });
    let reply = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, again.clone())
        .await
        .unwrap();
    assert_eq!(reply["chatId"], successor.as_str());
    let retry = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, again)
        .await
        .unwrap();
    assert_eq!(retry["chatId"], successor.as_str());
    let row = workspace.registry_row(mast_doc::KIND_CHATS, &successor).unwrap();
    assert_eq!(row.fields["userHandOffRetryChatIds"], json!([derive_id("resume-fresh-id", roles::CHAT)]));
    assert_eq!(workspace.read_chats().unwrap().len(), chats, "no second successor");
    assert!(!workspace.chat(CHAT).unwrap().unwrap().archived);
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(successor.as_str()));
    assert_eq!(workspace.pending_standing_hand_off(StandingRoleKind::Manager, &item), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_role_hand_offs_from_one_source_have_one_winner() {
    use mast_proto::StandingRoleKind;
    let (rig, item) = role_rig("role-race").await;
    let chats_before = rig.core.workspace.read_chats().unwrap().len();
    let call = |op: &str, summary: &str| rig.client.call(methods::HAND_OFF_ORCHESTRATOR, json!({
        "chatId": CHAT, "summary": summary, "operationId": op
    }));
    let (a, b) = tokio::join!(call("race-a", "Race winner A"), call("race-b", "Race winner B"));
    assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "{a:?} / {b:?}");
    let (winner, winning_prompt, losing_prompt) = match (&a, &b) {
        (Ok(reply), _) => (reply["chatId"].as_str().unwrap().to_owned(), "Race winner A", "Race winner B"),
        (_, Ok(reply)) => (reply["chatId"].as_str().unwrap().to_owned(), "Race winner B", "Race winner A"),
        _ => unreachable!(),
    };
    let workspace = &rig.core.workspace;
    assert_eq!(workspace.standing_role_chat(StandingRoleKind::Manager, &item).as_deref(), Some(winner.as_str()));
    assert_eq!(workspace.read_chats().unwrap().len(), chats_before + 1);
    assert_eq!(workspace.chat("role-race-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(winner.as_str()));
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains(winning_prompt)), "winning turn").await;
    assert!(!rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains(losing_prompt)));
}

#[tokio::test(flavor = "multi_thread")]
async fn hand_off_orchestrator_starts_successor_with_workers_and_actions() {
    let rig = rig().await;
    let item = create_item(&rig, "handoff-item", "Coordinator", None).await;
    assign_chat(&rig, "handoff-assignment", CHAT, &item).await;
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":"handoff-worker", "spaceId":SPACE,
        "deviceId":rig.core.device_id, "parentChatId":CHAT})).await.unwrap();
    assign_chat(&rig, "handoff-worker-assignment", "handoff-worker", &item).await;
    let tokens = rig.core.sessions.agent_tokens();
    let worker = tokens.mint("handoff-worker", "worker-run", SPACE, &rig.core.device_id);
    tool(&rig, worker.token(), "work_outline_create_user_action_request", json!({
        "request":"Review the result", "idempotencyKey":"review-result"
    })).await.unwrap();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let grant = tokens.admin().grant(CHAT, SPACE, None, false);
    let assistant = tokens
        .personal_assistant()
        .set(&rig.core.workspace.chat(CHAT).unwrap().unwrap());
    let args = json!({"summary":"Continue the release", "idempotencyKey":"handoff-one", "harness":"mock", "model":"mock-1", "reasoning":"medium", "archive":true});
    let reply = tool(&rig, lease.token(), "hand_off_orchestrator", args.clone()).await.unwrap();
    let successor = reply["chatId"].as_str().unwrap();
    assert_eq!(tokens.admin().live(CHAT), Some(grant));
    assert!(tokens.admin().live(successor).is_none());
    assert_eq!(tokens.personal_assistant().get(), Some(assistant));
    let config = rig.core.workspace.chat(successor).unwrap().unwrap().config.unwrap();
    assert_eq!(config.harness, HarnessId::Mock);
    assert_eq!(config.model.as_deref(), Some("mock-1"));
    assert_eq!(config.reasoning, Some(ReasoningLevel::Medium));
    let retry = tool(&rig, lease.token(), "hand_off_orchestrator", args).await.unwrap();
    assert_eq!(retry["chatId"], successor);
    assert!(tool(&rig, lease.token(), "hand_off_orchestrator", json!({
        "summary":"Changed summary", "idempotencyKey":"handoff-one"
    })).await.is_err());
    assert_eq!(rig.core.workspace.chat("handoff-worker").unwrap().unwrap().parent_chat_id.as_deref(), Some(successor));
    assert!(!rig.core.workspace.chat(CHAT).unwrap().unwrap().archived);
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|r| r.prompt.contains("Continue the release")), "successor turn").await;
    let requests = rig.harness.requests.lock().unwrap();
    let prompt = &requests.iter().find(|r| r.prompt.contains("Continue the release")).unwrap().prompt;
    assert!(prompt.starts_with("[Mast system message, not from the user: coordinator hand-off.]\n"));
    assert!(prompt.contains("handoff-worker"));
    assert!(prompt.contains("Review the result"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_started_for_a_descendant_item_gets_that_items_instructions() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Coordinator", None).await;
    let root_b = create_item(&rig, "op-b", "Elsewhere", None).await;
    let a_child = create_item(&rig, "op-a1", "Step", Some(&root_a)).await;
    let a_grand = create_item(&rig, "op-a11", "Leaf", Some(&a_child)).await;
    let b_child = create_item(&rig, "op-b1", "Other step", Some(&root_b)).await;
    add_instruction(
        &rig,
        "ins-root",
        json!({ "type": "workItem", "workItemId": root_a }),
        "Coordinator rule.",
    )
    .await;
    add_instruction(
        &rig,
        "ins-leaf",
        json!({ "type": "workItem", "workItemId": a_grand }),
        "Leaf rule.",
    )
    .await;
    add_instruction(
        &rig,
        "ins-other",
        json!({ "type": "workItem", "workItemId": b_child }),
        "Other subtree rule.",
    )
    .await;
    assign_chat(&rig, "assign-caller", CHAT, &root_a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // The child is started FOR the descendant: attached to it, parented to
    // the caller, with the leaf's effective instructions (its own plus the
    // inherited ones), never the caller's item's alone.
    let key = "leaf-worker";
    let reply = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args(key, &a_grand, "workspace-write"),
    )
    .await
    .unwrap();
    let child = derive_id(
        &mast_proto::agent_tools::operation_id(CHAT, key),
        roles::CHAT,
    );
    assert_eq!(reply["chatId"], child);
    let snap = snapshot(&rig);
    let assignment = snap
        .assignments
        .iter()
        .find(|a| a.chat_id == child)
        .unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(a_grand.as_str()));
    let row = rig.core.workspace.chat(&child).unwrap().unwrap();
    assert_eq!(row.parent_chat_id.as_deref(), Some(CHAT));
    // An agent-started child is Ask unless an Auto caller asks for Auto.
    assert!(!row.config.unwrap().auto_approve);
    let pin = rig
        .core
        .workspace
        .selected_instruction_revision(&child)
        .expect("the launch selected an instruction revision");
    let revision = rig
        .core
        .workspace
        .instruction_revision(&pin.revision_id)
        .unwrap();
    assert_eq!(revision.work_item_id, a_grand);
    let texts: Vec<&str> = revision
        .body
        .entries
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(texts, ["Coordinator rule.", "Leaf rule."]);
    wait_for(
        || !rig.harness.requests.lock().unwrap().is_empty(),
        "child run",
    )
    .await;
    let request = rig.harness.requests.lock().unwrap()[0].clone();
    assert_eq!(request.prompt, "go");
    let rendered = request.instructions.unwrap().envelope.rendered;
    assert!(rendered.contains("Leaf rule."), "{rendered}");
    assert!(!rendered.contains("Other subtree rule."), "{rendered}");

    // A retry with the same key and inputs is the same Chat, started once.
    let again = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args(key, &a_grand, "workspace-write"),
    )
    .await
    .unwrap();
    assert_eq!(again["chatId"], child);
    assert_eq!(again["commandId"], reply["commandId"]);
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .filter(|a| a.chat_id == child)
            .count(),
        1
    );
    // The caller's own item is a valid target too.
    let own = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("own-item", &root_a, "read-only"),
    )
    .await
    .unwrap();
    let own_chat = own["chatId"].as_str().unwrap().to_owned();
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .find(|a| a.chat_id == own_chat)
            .and_then(|a| a.work_item_id.clone()),
        Some(root_a.clone())
    );

    // Outside the caller's subtree (another root, a sibling subtree's
    // descendant, an unknown id): unauthorized, nothing created.
    let chats_before = rig.core.workspace.read_chats().unwrap().len();
    for target in [root_b.as_str(), b_child.as_str(), "no-such-item"] {
        let err = tool(
            &rig,
            &token,
            "chat_launch_child_for_item",
            launch_args(&format!("out-{target}"), target, "workspace-write"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("work-outline/unauthorized"), "{target}: {err}");
    }
    // Never a wider sandbox than the caller's, and a prompt is required.
    let err = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("wide", &a_grand, "danger-full-access"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/forbidden"), "{err}");
    let mut empty = launch_args("empty", &a_grand, "workspace-write");
    empty["prompt"] = json!("  ");
    let err = tool(&rig, &token, "chat_launch_child_for_item", empty)
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");
    assert_eq!(rig.core.workspace.read_chats().unwrap().len(), chats_before);

    // The re-association rule is NOT relaxed: the started child keeps the
    // item it was started for.
    let child_lease = tokens.mint(&child, "run-2", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        child_lease.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root_a, "idempotencyKey": "reassoc" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(err.contains("re-associate"), "{err}");

    // The coordinator closes what its own child raised; an unrelated Chat
    // still cannot.
    let ready = tool(
        &rig,
        child_lease.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Ready to merge: leaf-1", "idempotencyKey": "ready" }),
    )
    .await
    .unwrap();
    let ready_id = ready["userActionRequestId"].as_str().unwrap().to_owned();
    let other = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        other.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ready_id, "idempotencyKey": "close-other" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ready_id, "resolution": "Merged",
                "idempotencyKey": "close-parent" }),
    )
    .await
    .unwrap();
    assert!(
        snapshot(&rig)
            .user_action_requests
            .iter()
            .find(|r| r.id == ready_id)
            .is_some_and(|r| r.state == mast_proto::UserActionRequestState::Resolved),
        "the coordinator resolved its child's request"
    );

    // An unassociated caller has no subtree to start chats in.
    let err = tool(
        &rig,
        other.token(),
        "chat_launch_child_for_item",
        launch_args("nobody", &a_grand, "workspace-write"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    rig.harness.release.notify_waiters();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_lists_descendants_chats_and_requests_within_the_callers_subtree() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Coordinator", None).await;
    let root_b = create_item(&rig, "op-b", "Elsewhere", None).await;
    let a_child = create_item(&rig, "op-a1", "Step", Some(&root_a)).await;
    let a_grand = create_item(&rig, "op-a11", "Leaf", Some(&a_child)).await;
    let a_deep = create_item(&rig, "op-a111", "Deep", Some(&a_grand)).await;
    let a_archived = create_item(&rig, "op-a2", "Old", Some(&root_a)).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "archiveWorkItem", "workItemId": a_archived,
                    "descendantArchivePolicy": { "type": "archiveDescendants" },
                    "operationId": "arch" }),
        )
        .await
        .unwrap();
    assign_chat(&rig, "assign-caller", CHAT, &root_a).await;
    assign_chat(&rig, "assign-worker", OTHER_CHAT, &a_grand).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createUserActionRequest", "workItemId": a_grand,
                    "origin": { "chatId": OTHER_CHAT }, "text": "Ready to merge",
                    "info": "branch leaf-1", "operationId": "req-1" }),
        )
        .await
        .unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // Default: the caller's item, depth 4 (so the grandchild is in), archived
    // items left out.
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(read["rootWorkItemId"], root_a);
    assert_eq!(read["truncated"], false);
    assert_eq!(read["itemCount"], 4);
    let tree = &read["tree"];
    assert_eq!(tree["id"], root_a);
    assert_eq!(tree["children"].as_array().unwrap().len(), 1);
    let step = &tree["children"][0];
    assert_eq!(step["id"], a_child);
    assert_eq!(
        step["effectiveStatus"],
        snapshot(&rig)
            .item(&a_child)
            .map(|i| json!(i.effective_status))
            .unwrap()
    );
    let leaf = &step["children"][0];
    assert_eq!(leaf["id"], a_grand);
    assert_eq!(leaf["chats"][0]["id"], OTHER_CHAT);
    assert_eq!(leaf["chats"][0]["url"], "mast://chat/chat-other");
    assert_eq!(leaf["chats"][0]["status"], "idle");
    assert!(leaf["chats"][0].get("lastMessageAt").is_some());
    let request = &leaf["openUserActionRequests"][0];
    assert_eq!(request["text"], "Ready to merge");
    assert_eq!(request["detail"], "branch leaf-1");
    assert_eq!(request["originChatId"], OTHER_CHAT);
    assert!(request["id"].as_str().is_some() && request["createdAt"].is_number());
    assert_eq!(leaf["children"][0]["id"], a_deep);
    // The caller's own chat is listed on its own item.
    assert_eq!(tree["chats"][0]["id"], CHAT);

    // maxDepth limits how far down it goes and says what it left out.
    let shallow = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "maxDepth": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(shallow["itemCount"], 2);
    assert_eq!(shallow["tree"]["children"][0]["id"], a_child);
    assert_eq!(shallow["tree"]["children"][0]["children"], json!([]));
    assert_eq!(shallow["tree"]["children"][0]["childrenOmitted"], 1);
    // A descendant as the root of the read; maxDepth is capped at 8.
    let from_leaf = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "workItemId": a_grand, "maxDepth": 99 }),
    )
    .await
    .unwrap();
    assert_eq!(from_leaf["maxDepth"], 8);
    assert_eq!(from_leaf["tree"]["id"], a_grand);
    assert_eq!(from_leaf["itemCount"], 2);
    // includeArchived adds the archived item.
    let with_archived = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "includeArchived": true }),
    )
    .await
    .unwrap();
    assert_eq!(with_archived["itemCount"], 5);
    let archived = with_archived["tree"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == a_archived)
        .unwrap();
    assert_eq!(archived["archived"], true);

    // Outside the subtree (another root, an ancestor-side item, unknown) and
    // unassociated callers are refused.
    for target in [root_b.as_str(), "no-such-item"] {
        let err = tool(
            &rig,
            &token,
            "work_outline_read_subtree",
            json!({ "workItemId": target }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("work-outline/unauthorized"), "{target}: {err}");
    }
    let worker = tokens.mint(OTHER_CHAT, "run-2", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        worker.token(),
        "work_outline_read_subtree",
        json!({ "workItemId": root_a }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    // A leaf's chat reads its own subtree only, never upward.
    let err = tool(
        &rig,
        worker.token(),
        "work_outline_read_subtree",
        json!({ "workItemId": a_child }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let own = tool(&rig, worker.token(), "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(own["tree"]["id"], a_grand);
}

#[tokio::test(flavor = "multi_thread")]
async fn health_is_read_only_and_restricted_to_the_callers_subtree() {
    let rig = rig().await;
    let root = create_item(&rig, "health-root", "Root", None).await;
    let child = create_item(&rig, "health-child", "Child", Some(&root)).await;
    let sibling = create_item(&rig, "health-sibling", "Sibling", Some(&root)).await;
    let elsewhere = create_item(&rig, "health-elsewhere", "Elsewhere", None).await;
    update_item(&rig, "health-project", &root, json!({ "isProject": true })).await.unwrap();
    assign_chat(&rig, "health-caller", CHAT, &root).await;
    assign_chat(&rig, "health-worker", OTHER_CHAT, &child).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "health-run", SPACE, &rig.core.device_id);
    let asking = rig.core.sessions.agent_tokens().mint(OTHER_CHAT, "health-ask-run", SPACE, &rig.core.device_id);
    tool(&rig, asking.token(), "work_outline_create_user_action_request", json!({
        "request": "Which branch?", "idempotencyKey": "health-ask"
    })).await.unwrap();
    let before = snapshot(&rig).revision;
    let result = tool(&rig, lease.token(), "work_outline_health", json!({})).await.unwrap();
    assert_eq!(result["workItemId"], root);
    // Every chat in the subtree reports its idle time, and a chat with an open
    // Required Action is marked as one a cleanup must not archive.
    assert!(result["chats"][CHAT]["idleSeconds"].as_i64().unwrap() >= 0);
    assert_eq!(result["chats"][OTHER_CHAT]["blocked"], "openRequest");
    assert!(result["entries"].as_array().unwrap().iter().any(|entry| entry["kind"] == "chatOnContainer" && entry["chatId"] == CHAT));
    let tracker_result = rig.client.call(methods::WORK_OUTLINE_HEALTH,
        json!({ "spaceId": SPACE, "workItemId": root })).await.unwrap();
    assert_eq!(tracker_result["entries"], result["entries"]);
    assert_eq!(snapshot(&rig).revision, before);
    let child_result = tool(&rig, lease.token(), "work_outline_health", json!({ "workItemId": child })).await.unwrap();
    assert_eq!(child_result["workItemId"], child);
    let error = tool(&rig, lease.token(), "work_outline_health", json!({ "workItemId": elsewhere })).await.unwrap_err();
    assert!(error.contains("work-outline/unauthorized"), "{error}");
    let worker = rig.core.sessions.agent_tokens().mint(OTHER_CHAT, "health-worker-run", SPACE, &rig.core.device_id);
    let error = tool(&rig, worker.token(), "work_outline_health", json!({ "workItemId": sibling })).await.unwrap_err();
    assert!(error.contains("work-outline/unauthorized"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn health_archive_keeps_every_chat_that_is_not_safe_and_archives_the_rest() {
    let rig = rig().await;
    let root = create_item(&rig, "ha-root", "Root", None).await;
    let child = create_item(&rig, "ha-child", "Child", Some(&root)).await;
    assign_chat(&rig, "ha-caller", CHAT, &root).await;
    assign_chat(&rig, "ha-worker", OTHER_CHAT, &child).await;
    let asking = rig.core.sessions.agent_tokens().mint(OTHER_CHAT, "ha-ask-run", SPACE, &rig.core.device_id);
    tool(&rig, asking.token(), "work_outline_create_user_action_request", json!({
        "request": "Which branch?", "idempotencyKey": "ha-ask"
    })).await.unwrap();
    let client = &rig.client;
    let root_id = &root;
    let archive = |chats: Value| async move {
        client.call(methods::WORK_OUTLINE_HEALTH_ARCHIVE, json!({ "spaceId": SPACE, "workItemId": root_id, "chats": chats })).await.unwrap()
    };
    let archived = |rig: &Rig, chat: &str| rig.core.workspace.chat(chat).unwrap().unwrap().archived;
    // An open Required Action, a chat outside the subtree and a chat active
    // since the plan was made are all kept, and nothing is archived.
    let kept = archive(json!([
        { "chatId": OTHER_CHAT, "minIdleSeconds": 0 },
        { "chatId": "not-in-this-tracker", "minIdleSeconds": 0 },
        { "chatId": CHAT, "minIdleSeconds": 100_000_000 },
    ])).await;
    assert_eq!(kept["results"][0]["archived"], false);
    assert_eq!(kept["results"][0]["reason"], "has an open Required Action");
    assert_eq!(kept["results"][1]["reason"], "not part of this Tracker");
    assert_eq!(kept["results"][2]["reason"], "active since the plan was made");
    assert!(!archived(&rig, OTHER_CHAT) && !archived(&rig, CHAT));
    // An idle chat with nothing open is archived, and archiving it twice is harmless.
    let done = archive(json!([{ "chatId": CHAT, "minIdleSeconds": 0 }])).await;
    assert_eq!(done["results"][0]["archived"], true, "{done}");
    assert!(archived(&rig, CHAT) && !archived(&rig, OTHER_CHAT));
    let again = archive(json!([{ "chatId": CHAT, "minIdleSeconds": 0 }])).await;
    assert_eq!(again["results"][0]["archived"], false);
    assert_eq!(again["results"][0]["reason"], mast_proto::HEALTH_ALREADY_ARCHIVED);
    // Leaving out the idle time is refused rather than read as "no check".
    assert!(client.call(methods::WORK_OUTLINE_HEALTH_ARCHIVE, json!({ "spaceId": SPACE, "workItemId": root_id, "chats": [{ "chatId": OTHER_CHAT }] })).await.is_err());
    assert!(!archived(&rig, OTHER_CHAT));
    // A Space owned by another device is refused: its chats run there, out of this engine's sight.
    rig.core.workspace.create_space("remote-space", "other-device", "/tmp/remote", None, false).unwrap();
    let remote = client.call(methods::WORK_OUTLINE_HEALTH_ARCHIVE, json!({
        "spaceId": "remote-space", "workItemId": "anything", "chats": [{ "chatId": OTHER_CHAT, "minIdleSeconds": 0 }],
    })).await;
    assert!(remote.unwrap_err().to_string().contains("another device"));
    assert!(!archived(&rig, OTHER_CHAT));
}

#[tokio::test(flavor = "multi_thread")]
async fn health_archive_keeps_a_chat_that_still_has_live_child_chats() {
    let rig = rig().await;
    let root = create_item(&rig, "hc-root", "Root", None).await;
    assign_chat(&rig, "hc-parent", CHAT, &root).await;
    rig.core.workspace.create_chat_with_parent("hc-kid", Some(SPACE), Some(&rig.core.device_id), None, None, Some(CHAT.to_owned())).unwrap();
    let request = json!({ "spaceId": SPACE, "workItemId": root, "chats": [{ "chatId": CHAT, "minIdleSeconds": 0 }] });
    let kept = rig.client.call(methods::WORK_OUTLINE_HEALTH_ARCHIVE, request.clone()).await.unwrap();
    assert_eq!(kept["results"][0]["reason"], "is coordinating chats that are still live");
    assert!(!rig.core.workspace.chat(CHAT).unwrap().unwrap().archived);
    // Once the child is archived the parent can go.
    rig.client.call(methods::MUTATE, json!({ "op": "setChatArchived", "chatId": "hc-kid", "archived": true })).await.unwrap();
    let done = rig.client.call(methods::WORK_OUTLINE_HEALTH_ARCHIVE, request).await.unwrap();
    assert_eq!(done["results"][0]["archived"], true, "{done}");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_caps_its_output_and_says_so() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Big", None).await;
    for i in 0..310 {
        create_item(
            &rig,
            &format!("op-leaf-{i}"),
            &format!("Leaf {i}"),
            Some(&root),
        )
        .await;
    }
    assign_chat(&rig, "assign-caller", CHAT, &root).await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let read = tool(&rig, lease.token(), "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(read["truncated"], true);
    assert_eq!(read["itemCount"], 300);
    assert_eq!(read["tree"]["children"].as_array().unwrap().len(), 299);
    assert_eq!(read["tree"]["childrenOmitted"], 11);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_omits_completed_branches_and_preserves_requests() {
    let rig = rig().await;
    let root = create_item(&rig, "compact-root", "Coordinator", None).await;
    for i in 0..170 {
        let item = create_item(&rig, &format!("compact-create-{i}"), &format!("Completed work item {i} with a descriptive title"), Some(&root)).await;
        update_item(&rig, &format!("compact-complete-{i}"), &item, json!({"status": "completed"})).await.unwrap();
    }
    let parent = create_item(&rig, "compact-parent", "Completed parent", Some(&root)).await;
    let leaf = create_item(&rig, "compact-leaf", "Request", Some(&parent)).await;
    update_item(&rig, "compact-parent-done", &parent, json!({"status": "completed"})).await.unwrap();
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op": "createUserActionRequest", "workItemId": leaf, "origin": {"chatId": OTHER_CHAT}, "text": "Review", "operationId": "compact-request"})).await.unwrap();
    assign_chat(&rig, "compact-assign", CHAT, &root).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "compact-run", SPACE, &rig.core.device_id);
    let compact = tool(&rig, lease.token(), "work_outline_read_subtree", json!({})).await.unwrap();
    let full = tool(&rig, lease.token(), "work_outline_read_subtree", json!({"includeCompleted": true})).await.unwrap();
    assert_eq!(compact["itemCount"], 3);
    assert_eq!(compact["tree"]["completedOmitted"], 170);
    assert_eq!(compact["tree"]["children"][0]["children"][0]["openUserActionRequests"][0]["text"], "Review");
    assert_eq!(full["itemCount"], 173);
    assert_eq!(full["tree"]["completedOmitted"], 0);
    let shallow = tool(&rig, lease.token(), "work_outline_read_subtree", json!({"maxDepth": 0})).await.unwrap();
    assert_eq!(shallow["tree"]["completedOmitted"], 170);
    assert_eq!(shallow["tree"]["childrenOmitted"], 1);
    let selected = tool(&rig, lease.token(), "work_outline_read_subtree", json!({"workItemId": parent})).await.unwrap();
    assert_eq!(selected["tree"]["id"], parent);
    let before = serde_json::to_vec(&full).unwrap().len();
    let after = serde_json::to_vec(&compact).unwrap().len();
    // Baseline for this 173-item fixture: 46,499 bytes before, 1,220 after.
    println!("subtree fixture: {before} bytes before, {after} bytes after");
    assert!(after * 4 < before, "before={before}, after={after}");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_preserves_completed_items_with_working_chats() {
    let rig = rig().await;
    let root = create_item(&rig, "working-root", "Coordinator", None).await;
    let completed = create_item(&rig, "working-item", "Completed", Some(&root)).await;
    update_item(&rig, "working-complete", &completed, json!({"status": "completed"})).await.unwrap();
    assign_chat(&rig, "working-caller", CHAT, &root).await;
    assign_chat(&rig, "working-worker", OTHER_CHAT, &completed).await;
    rig.core.doc_host.queue_command(OTHER_CHAT, run_command("work", &rig.space_path)).unwrap();
    wait_for(|| rig.core.sessions.session_status(OTHER_CHAT).is_some_and(|s| s.status == mast_proto::SessionStatus::Working), "worker running").await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "working-run", SPACE, &rig.core.device_id);
    let read = tool(&rig, lease.token(), "work_outline_read_subtree", json!({})).await.unwrap();
    assert_eq!(read["tree"]["children"][0]["id"], completed);
    assert_eq!(read["tree"]["children"][0]["chats"][0]["status"], "working");
    rig.harness.release.notify_one();
}

// ---- bind_chat_worktree -----------------------------------------------------
//
// Real temp git repositories: a superproject with a `flagship` submodule, a
// linked worktree of the SUBMODULE in the superproject's gitignored
// `.worktrees/` folder (the cb4e2158 layout), and one of the root repository.

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Zeron Test",
            "-c",
            "user.email=test@zeron.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
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
    std::fs::write(path.join(file), "x\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "init"]);
}

struct BindRig {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    sub_wt: PathBuf,
    root_wt: PathBuf,
    other: PathBuf,
    core: EngineCore,
    rpc: Arc<mast_engine::rpc::EngineRpc>,
    targets: DiffTargets,
    token: String,
    /// Keeps the token's Run alive.
    _lease: mast_engine::agent_tools::AgentTokenLease,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bind_rig(attach: bool) -> BindRig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    init_repo(&root.join("sources/flagship"), "flagship.txt");
    let ws = root.join("ws");
    init_repo(&ws, "README");
    let source = root.join("sources/flagship");
    git(&ws, &["submodule", "add", "-q", &text(&source), "flagship"]);
    std::fs::write(ws.join(".gitignore"), ".worktrees/\n").unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "members"]);
    let sub_wt = ws.join(".worktrees/flagship/eng-3012");
    std::fs::create_dir_all(sub_wt.parent().unwrap()).unwrap();
    git(
        &ws.join("flagship"),
        &[
            "worktree",
            "add",
            "-q",
            "../.worktrees/flagship/eng-3012",
            "-b",
            "feat-eng-3012",
        ],
    );
    let root_wt = ws.join(".worktrees/ws/root-feature");
    std::fs::create_dir_all(root_wt.parent().unwrap()).unwrap();
    git(
        &ws,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "root-feature",
            &text(&root_wt),
        ],
    );
    let other = root.join("other");
    init_repo(&other, "other.txt");

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), "dev-a").unwrap();
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    core.workspace
        .create_space(SPACE, "dev-a", &text(&ws), None, true)
        .unwrap();
    core.workspace
        .create_chat(CHAT, Some(SPACE), Some("dev-a"), None, Some(text(&ws)))
        .unwrap();
    let rpc = core.rpc_service();
    let targets = DiffTargets::new(core.repos.clone(), core.workspace.clone(), "dev-a");
    if attach {
        rpc.agent_tools().set_diff_targets(targets.clone());
    }
    let lease = core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, "dev-a");
    BindRig {
        token: lease.token().to_owned(),
        _lease: lease,
        sub_wt: std::fs::canonicalize(&sub_wt).unwrap(),
        root_wt: std::fs::canonicalize(&root_wt).unwrap(),
        _tmp: tmp,
        ws,
        other,
        core,
        rpc,
        targets,
    }
}

fn bind_call(
    rig: &BindRig,
    token: &str,
    arguments: Value,
) -> Result<Value, mast_engine::agent_tools::AgentToolError> {
    rig.rpc
        .agent_tools()
        .call(&mast_proto::agent_tools::AgentToolCallParams {
            token: token.to_owned(),
            tool: mast_proto::agent_tools::tools::BIND_CHAT_WORKTREE.to_owned(),
            arguments,
        })
}

fn bind_ok(rig: &BindRig, path: &Path, key: &str) -> Value {
    bind_call(
        rig,
        &rig.token,
        json!({ "path": text(path), "idempotencyKey": key }),
    )
    .unwrap_or_else(|e| panic!("bind {}: {e}", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn review_publish_uses_bound_submodule_worktree_without_moving_chat() {
    let rig = bind_rig(true);
    let base = git(&rig.sub_wt, &["rev-parse", "HEAD"]);
    std::fs::write(rig.sub_wt.join("flagship.txt"), "reviewed change\n").unwrap();
    git(&rig.sub_wt, &["commit", "-qam", "review change"]);
    let head = git(&rig.sub_wt, &["rev-parse", "HEAD"]);
    let parent_head = git(&rig.ws, &["rev-parse", "HEAD"]);
    let parent_branch = git(&rig.ws, &["branch", "--show-current"]);
    let bound = bind_ok(&rig, &rig.sub_wt, "review-bind");
    assert_eq!(bound["selected"], true);
    let review = json!({
        "schema": "mast.review/v1",
        "pr": {"repo": "owner/flagship", "number": 42, "baseSha": base, "headSha": head},
        "summary": "Review the submodule change.",
        "steps": [{"id": "change", "title": "Change", "file": "flagship.txt",
            "hunks": [{"newStart": 1, "newEnd": 1}], "what": "Changes text", "why": "Update",
            "risk": "low", "riskNote": "Text only", "tags": []}],
        "highlights": [], "questions": [], "suggestedComments": []
    });
    let call = |arguments| {
        rig.rpc
            .agent_tools()
            .call(&mast_proto::agent_tools::AgentToolCallParams {
                token: rig.token.clone(),
                tool: "review_publish".into(),
                arguments,
            })
    };
    // Both public PR calling forms must resolve the selected submodule checkout.
    for arguments in [
        json!({"prUrl": "https://github.com/owner/flagship/pull/42", "headSha": head, "review": review}),
        json!({"target": {"kind": "pr", "repo": "owner/flagship", "number": 42,
            "baseSha": base, "headSha": head}, "review": review}),
    ] {
        let reply = call(arguments).unwrap();
        let id = reply["artifactId"].as_str().unwrap();
        assert_eq!(reply["url"], format!("mast-artifact:{id}"));
        assert!(rig.core.workspace.artifact(id).is_some());
        let stored: Value =
            serde_json::from_slice(&std::fs::read(reply["storedPath"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(stored["pr"]["headSha"], head);
        assert_eq!(
            stored["steps"][0]["hunks"][0]["hunkHash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    }
    let missing = "a".repeat(40);
    let mut invalid = review.clone();
    invalid["pr"]["headSha"] = json!(missing);
    let error = call(json!({"prUrl": "https://github.com/owner/flagship/pull/42",
        "headSha": missing, "review": invalid}))
    .unwrap_err()
    .to_string();
    assert!(error.contains(&text(&rig.sub_wt)), "{error}");
    assert!(
        error.contains("rev-parse") && error.contains("--verify"),
        "{error}"
    );
    assert!(error.contains(&format!("{missing}^{{commit}}")), "{error}");
    assert_eq!(
        rig.core.workspace.chat(CHAT).unwrap().unwrap().cwd,
        Some(text(&rig.ws))
    );
    assert_eq!(git(&rig.ws, &["rev-parse", "HEAD"]), parent_head);
    assert_eq!(git(&rig.ws, &["branch", "--show-current"]), parent_branch);
    assert_eq!(git(&rig.sub_wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&rig.sub_wt, &["branch", "--show-current"]),
        "feat-eng-3012"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_records_and_selects_a_submodule_worktree() {
    let rig = bind_rig(true);
    let reply = bind_ok(&rig, &rig.sub_wt, "bind-flagship");
    assert_eq!(reply["chatId"], CHAT);
    assert_eq!(reply["path"], text(&rig.sub_wt));
    assert_eq!(reply["branch"], "feat-eng-3012");
    assert_eq!(
        reply["headSha"],
        git(&rig.sub_wt, &["rev-parse", "HEAD"]).as_str()
    );
    assert_eq!(reply["repoName"], "flagship");
    assert_eq!(reply["selected"], true);
    assert_eq!(reply["replayed"], false);
    assert_eq!(
        reply["targets"],
        json!([{ "path": text(&rig.sub_wt), "source": "agent", "selected": true }])
    );
    // The row is persisted, agent-sourced, with the derived operation id.
    let stored = rig.targets.stored(CHAT);
    assert_eq!(stored.targets.len(), 1);
    assert!(stored.targets[0].selected);
    let row = rig.targets.rows().into_iter().next().unwrap();
    assert_eq!(row.chat_id, CHAT);
    assert_eq!(
        row.ops,
        vec![mast_proto::agent_tools::operation_id(CHAT, "bind-flagship")]
    );
    assert!(row.ops[0].len() <= 64);
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_retries_replay_and_new_keys_do_not_duplicate() {
    let rig = bind_rig(true);
    bind_ok(&rig, &rig.sub_wt, "k1");
    let before = *rig.targets.changes().borrow();
    let retry = bind_ok(&rig, &rig.sub_wt, "k1");
    assert_eq!(retry["replayed"], true);
    assert_eq!(retry["targets"].as_array().unwrap().len(), 1);
    assert_eq!(*rig.targets.changes().borrow(), before, "no second write");

    // Same key, different path: the key is already spent.
    let err = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.root_wt), "idempotencyKey": "k1" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);

    // A new key for the same path refreshes the selection, no duplicate.
    let again = bind_ok(&rig, &rig.sub_wt, "k2");
    assert_eq!(again["replayed"], false);
    assert_eq!(again["targets"].as_array().unwrap().len(), 1);
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_binds_each_repository_family_and_selects_the_last() {
    let rig = bind_rig(true);
    bind_ok(&rig, &rig.sub_wt, "a");
    let reply = bind_ok(&rig, &rig.root_wt, "b");
    assert_eq!(reply["repoName"], "ws");
    assert_eq!(reply["branch"], "root-feature");
    assert_eq!(
        reply["targets"],
        json!([
            { "path": text(&rig.sub_wt), "source": "agent", "selected": false },
            { "path": text(&rig.root_wt), "source": "agent", "selected": true },
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_refuses_paths_outside_the_chats_repositories() {
    let rig = bind_rig(true);
    for (key, path) in [
        ("outside", text(rig.ws.parent().unwrap())),
        ("other", text(&rig.other)),
        ("relative", "flagship".to_owned()),
        ("missing", text(&rig.ws.join("nope"))),
    ] {
        let err = bind_call(
            &rig,
            &rig.token,
            json!({ "path": path, "idempotencyKey": key }),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid", "{key}: {err}");
    }
    let subdir = rig.sub_wt.join("src");
    std::fs::create_dir_all(&subdir).unwrap();
    let err = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&subdir), "idempotencyKey": "subdir" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid", "{err}");
    // A chatId argument is ignored: the token's chat is the only one bound.
    core_chat_other(&rig);
    let reply = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.sub_wt), "idempotencyKey": "forge", "chatId": OTHER_CHAT }),
    )
    .unwrap();
    assert_eq!(reply["chatId"], CHAT);
    assert!(rig.targets.stored(OTHER_CHAT).targets.is_empty());
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

fn core_chat_other(rig: &BindRig) {
    rig.core
        .workspace
        .create_chat(
            OTHER_CHAT,
            Some(SPACE),
            Some("dev-a"),
            None,
            Some(text(&rig.ws)),
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_validates_arguments_and_authentication() {
    let rig = bind_rig(true);
    let path = text(&rig.sub_wt);
    for (arguments, what) in [
        (json!({ "idempotencyKey": "k" }), "missing path"),
        (json!({ "path": "", "idempotencyKey": "k" }), "empty path"),
        (json!({ "path": 7, "idempotencyKey": "k" }), "bad path type"),
        (json!(null), "no arguments"),
    ] {
        let err = bind_call(&rig, &rig.token, arguments).unwrap_err();
        assert_eq!(err.code(), "invalid", "{what}: {err}");
    }
    assert!(rig.targets.stored(CHAT).targets.is_empty());

    // A key is optional for a bind (blank counts as omitted).
    for key in [None, Some(""), Some("  ")] {
        let mut arguments = json!({ "path": path });
        if let Some(key) = key {
            arguments["idempotencyKey"] = json!(key);
        }
        bind_call(&rig, &rig.token, arguments)
            .unwrap_or_else(|e| panic!("bind with key {key:?}: {e}"));
    }

    let good = json!({ "path": path, "idempotencyKey": "k" });
    for token in ["", "zat_forged", &format!("{}0", rig.token)] {
        let err = bind_call(&rig, token, good.clone()).unwrap_err();
        assert_eq!(err.code(), "unauthenticated", "{token:?}");
    }
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

/// Replay of today's failure (Mikey's ENG-1655 review chat and the
/// pr-page-parity child): `bind_chat_worktree` without an `idempotencyKey`
/// binds, and rebinding a path later re-selects it even after another bind.
#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_without_a_key_binds_and_rebinding_reselects() {
    let rig = bind_rig(true);
    let call = |path: &Path| {
        bind_call(&rig, &rig.token, json!({ "path": text(path) })).unwrap_or_else(|e| panic!("{e}"))
    };
    let selected = || -> Option<String> {
        rig.targets
            .stored(CHAT)
            .targets
            .into_iter()
            .find(|t| t.selected)
            .map(|t| t.path)
    };
    let first = call(&rig.sub_wt);
    assert_eq!(first["selected"], true);
    assert_eq!(first["replayed"], false);
    // The same call again is harmless: still one target.
    let again = call(&rig.sub_wt);
    assert_eq!(again["targets"].as_array().unwrap().len(), 1);
    assert_eq!(again["replayed"], false);
    // Bind another path, then the first again: the first is selected, not
    // replayed as a stale no-op.
    call(&rig.root_wt);
    assert_eq!(selected().as_deref(), Some(text(&rig.root_wt).as_str()));
    call(&rig.sub_wt);
    assert_eq!(selected().as_deref(), Some(text(&rig.sub_wt).as_str()));
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 2);
    // No replay ledger entries were written for the keyless calls.
    assert!(rig.targets.rows().into_iter().all(|row| row.ops.is_empty()));

    // `snake_case` keys are accepted too.
    let reply = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.sub_wt), "idempotency_key": "snake" }),
    )
    .unwrap();
    assert_eq!(reply["replayed"], false);
    let row = rig.targets.rows().into_iter().next().unwrap();
    assert_eq!(
        row.ops,
        vec![mast_proto::agent_tools::operation_id(CHAT, "snake")]
    );

    // A missing path lists the parameters and an example.
    let err = bind_call(&rig, &rig.token, json!({}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("path is required"), "{err}");
    assert!(err.contains("Usage of bind_chat_worktree"), "{err}");
    assert!(err.contains("example:"), "{err}");
}

/// Engine assembly attaches the shared `DiffTargets` itself: a bind works
/// without any test-side `set_diff_targets`.
#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_is_wired_on_the_assembled_engine() {
    let rig = bind_rig(false);
    let reply = bind_ok(&rig, &rig.sub_wt, "k");
    assert_eq!(reply["selected"], true);
    assert_eq!(reply["path"], text(&rig.sub_wt));
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_refuses_a_chat_hosted_on_another_device() {
    let rig = bind_rig(true);
    // A Space on another device: its chats are hosted there.
    rig.core
        .workspace
        .create_space("space-b", "dev-b", &text(&rig.ws), None, true)
        .unwrap();
    rig.core
        .workspace
        .create_chat(
            "chat-remote",
            Some("space-b"),
            Some("dev-b"),
            None,
            Some(text(&rig.ws)),
        )
        .unwrap();
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint("chat-remote", "run-2", "space-b", "dev-b");
    let err = bind_call(
        &rig,
        lease.token(),
        json!({ "path": text(&rig.sub_wt), "idempotencyKey": "k" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "forbidden", "{err}");
}

/// What a coordinator's resolve of its child's request must leave behind.
fn request_is_resolved(snapshot: &WorkOutlineSnapshot, id: &str) -> bool {
    snapshot
        .user_action_requests
        .iter()
        .find(|r| r.id == id)
        .is_some_and(|r| r.state == mast_proto::UserActionRequestState::Resolved)
}

fn open_request_ids(read: &Value) -> Vec<String> {
    fn walk(node: &Value, out: &mut Vec<String>) {
        for request in node["openUserActionRequests"]
            .as_array()
            .into_iter()
            .flatten()
        {
            out.push(request["id"].as_str().unwrap_or_default().to_owned());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&read["tree"], &mut out);
    out
}

/// A child's request is raised with a `dedupeKey` (the tool guidance tells
/// agents to pass one per topic). The coordinator's resolve used to select the
/// dedupe group by the CALLER's Chat id, so the child's request fell out of
/// its own group, nothing was written, and the reply still said `resolved`.
#[tokio::test(flavor = "multi_thread")]
async fn a_coordinator_resolving_its_childs_deduped_request_persists_across_reads_and_restart() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Coordinator", None).await;
    let leaf = create_item(&rig, "op-leaf", "Leaf", Some(&root)).await;
    assign_chat(&rig, "assign-caller", CHAT, &root).await;
    let tokens = rig.core.sessions.agent_tokens();
    let coordinator = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = coordinator.token().to_owned();

    let launched = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("leaf-worker", &leaf, "workspace-write"),
    )
    .await
    .unwrap();
    let child = launched["chatId"].as_str().unwrap().to_owned();
    let child_lease = tokens.mint(&child, "run-2", SPACE, &rig.core.device_id);
    let raised = tool(
        &rig,
        child_lease.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Ready to merge: leaf", "dedupeKey": "ready-leaf",
                "idempotencyKey": "ready" }),
    )
    .await
    .unwrap();
    let request_id = raised["userActionRequestId"].as_str().unwrap().to_owned();
    let open = |read: &Value| open_request_ids(read).contains(&request_id);
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert!(open(&read), "the child's request starts open: {read}");

    // An unrelated Chat is refused loudly and the request stays open.
    let stranger = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        stranger.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "close-other" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(!request_is_resolved(&snapshot(&rig), &request_id));

    let reply = tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "resolution": "Merged",
                "idempotencyKey": "close-parent" }),
    )
    .await
    .unwrap();
    assert_eq!(reply["resolved"], true);

    // The reply is backed by the row, by the subtree read, and by the row's
    // own fields.
    assert!(request_is_resolved(&snapshot(&rig), &request_id));
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert!(!open(&read), "still open after the resolve: {read}");
    let row = rig
        .core
        .workspace
        .registry_row("userActionRequests", &request_id)
        .expect("the request row exists");
    assert_eq!(row.fields.get("state"), Some(&json!("resolved")), "{row:?}");
    assert_eq!(rig.core.workspace.registry_pending_len(), 0);

    // Re-assembling the engine from the same profile keeps it resolved.
    rig.harness.release.notify_waiters();
    let Rig {
        _tmp, core, client, ..
    } = rig;
    drop(client);
    // A graceful restart flushes the debounced registry snapshot first.
    core.workspace.shutdown();
    drop(core);
    let root_dir = std::fs::canonicalize(_tmp.path()).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(GatedHarness::default()));
    let again = EngineCore::assemble(
        &root_dir.join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core re-assembles from the same profile");
    let inputs = again.workspace.work_outline_inputs(SPACE);
    let after = mast_doc::materialize_work_outline(SPACE, &inputs);
    assert!(
        request_is_resolved(&after, &request_id),
        "the resolution did not survive a restart"
    );
}

// ---- forgiving calls ----------------------------------------------------------

/// Replays of the calls agents got wrong on their first try (Claude Code loads
/// MCP tools lazily, so they guess parameters): missing keys on naturally
/// idempotent tools and guessed field names now succeed; the create-style
/// tools name the missing field and show an example.
#[tokio::test(flavor = "multi_thread")]
async fn first_guess_calls_succeed_or_say_exactly_what_to_pass() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Feature", None).await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let call = |name: &'static str, arguments: Value| {
        let (rig, token) = (&rig, token.clone());
        async move { tool(rig, &token, name, arguments).await }
    };

    // associate_current_chat: no key, `workItem` for `workItemId`.
    call(
        "work_outline_associate_current_chat",
        json!({ "workItem": root }),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .find(|a| a.chat_id == CHAT)
            .and_then(|a| a.work_item_id.clone())
            .as_deref(),
        Some(root.as_str())
    );
    // The same call again is fine.
    call(
        "work_outline_associate_current_chat",
        json!({ "itemId": root }),
    )
    .await
    .unwrap();

    // work_outline_set_status: no key; completing, reopening and completing
    // again each take effect (a derived key must not replay a stale result).
    let status = |rig: &Rig| snapshot(rig).item(&root).unwrap().status;
    for (wanted, expected) in [
        ("completed", mast_proto::WorkItemStatus::Completed),
        ("active", mast_proto::WorkItemStatus::Active),
        ("completed", mast_proto::WorkItemStatus::Completed),
        ("completed", mast_proto::WorkItemStatus::Completed),
    ] {
        // Updates are stamped with the clock; keep the stamps apart.
        tokio::time::sleep(Duration::from_millis(3)).await;
        call("work_outline_set_status", json!({ "status": wanted }))
            .await
            .unwrap();
        assert_eq!(status(&rig), expected, "after {wanted}");
    }
    // Also by an explicit item and a snake_case key.
    call(
        "work_outline_set_status",
        json!({ "status": "active", "work_item_id": root, "idempotency_key": "reopen" }),
    )
    .await
    .unwrap();
    assert_eq!(status(&rig), mast_proto::WorkItemStatus::Active);

    // create_user_action_request: `text` for `request`, a dedupeKey as the key.
    let created = call(
        "work_outline_create_user_action_request",
        json!({ "text": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    let request_id = created["userActionRequestId"].as_str().unwrap().to_owned();
    // Repeating the call returns the same open request, not a duplicate.
    let repeated = call(
        "work_outline_create_user_action_request",
        json!({ "title": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    assert_eq!(repeated["userActionRequestId"], request_id.as_str());
    let updated = call(
        "work_outline_create_user_action_request",
        json!({ "request": "Which release branch?", "detail": "Choose one", "dedupeKey": "branch", "facts": [{"label":"Version", "value":"2"}], "links": [{"label":"Reference", "target":{"type":"url", "url":"https://example.com/release"}}] }),
    ).await.unwrap();
    assert_eq!(updated["userActionRequestId"], request_id.as_str());
    let updated_snapshot = snapshot(&rig);
    let updated_request = updated_snapshot.user_action_requests.iter().find(|r| r.id == request_id).unwrap();
    assert_eq!(updated_request.text, "Which release branch?");
    assert_eq!(updated_request.info.as_deref(), Some("Choose one"));
    assert_eq!(updated_request.facts[0].value, "2");
    assert_eq!(updated_request.links[0].label, "Reference");
    assert_eq!(
        snapshot(&rig)
            .user_action_requests
            .iter()
            .filter(|r| r.is_open() && !r.deduped)
            .count(),
        1
    );
    let explicit = |text: &str, key: &str| json!({"request":text, "dedupeKey":"explicit-update", "idempotencyKey":key});
    let created_explicit = call("work_outline_create_user_action_request", explicit("Review this", "explicit-create")).await.unwrap();
    let repeated_explicit = call("work_outline_create_user_action_request", explicit("Review this", "explicit-create")).await.unwrap();
    assert_eq!(created_explicit["userActionRequestId"], repeated_explicit["userActionRequestId"]);
    let changed_explicit = call("work_outline_create_user_action_request", explicit("Review revision", "explicit-update")).await.unwrap();
    let repeated_change = call("work_outline_create_user_action_request", explicit("Review revision", "explicit-update")).await.unwrap();
    assert_eq!(created_explicit["userActionRequestId"], changed_explicit["userActionRequestId"]);
    assert_eq!(changed_explicit["userActionRequestId"], repeated_change["userActionRequestId"]);
    assert!(call("work_outline_create_user_action_request", explicit("Different content", "explicit-update")).await.is_err());
    call("work_outline_resolve_user_action_request", json!({"requestId":created_explicit["userActionRequestId"]})).await.unwrap();
    let retry_create = call("work_outline_create_user_action_request", explicit("Review this", "explicit-create")).await.unwrap();
    let retry_update = call("work_outline_create_user_action_request", explicit("Review revision", "explicit-update")).await.unwrap();
    assert_eq!(retry_create["userActionRequestId"], created_explicit["userActionRequestId"]);
    assert_eq!(retry_update["userActionRequestId"], created_explicit["userActionRequestId"]);
    assert!(request_is_resolved(&snapshot(&rig), created_explicit["userActionRequestId"].as_str().unwrap()));
    // `question` works as well, with an explicit key.
    let other = call(
        "work_outline_create_user_action_request",
        json!({ "question": "Ship it?", "idempotencyKey": "ship" }),
    )
    .await
    .unwrap();
    assert_ne!(other["userActionRequestId"], request_id.as_str());

    // resolve: `id` for `userActionRequestId`, no key; a retry is fine.
    for _ in 0..2 {
        call(
            "work_outline_resolve_user_action_request",
            json!({ "id": request_id, "resolution": "main" }),
        )
        .await
        .unwrap();
    }
    assert!(request_is_resolved(&snapshot(&rig), &request_id));
    call(
        "work_outline_resolve_user_action_request",
        json!({ "requestId": other["userActionRequestId"] }),
    )
    .await
    .unwrap();

    // The same dedupeKey after a resolve is a new request, not a replay.
    let reasked = call(
        "work_outline_create_user_action_request",
        json!({ "request": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    assert_ne!(reasked["userActionRequestId"], request_id.as_str());
    assert!(!request_is_resolved(
        &snapshot(&rig),
        reasked["userActionRequestId"].as_str().unwrap()
    ));

    // Create-style tools keep their key but say exactly what is missing.
    let err = call(
        "work_outline_create_user_action_request",
        json!({ "request": "No key, no dedupeKey" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("idempotencyKey is required"), "{err}");
    assert!(err.contains("dedupeKey"), "{err}");
    assert!(
        err.contains("Usage of work_outline_create_user_action_request"),
        "{err}"
    );
    assert!(err.contains("example:"), "{err}");
    let err = call("work_outline_create_user_action_request", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("missing field `request`"), "{err}");
    assert!(err.contains("required: request"), "{err}");
    assert!(err.contains("optional: kind"), "{err}");
    assert!(err.contains("detail (context for the user)"), "{err}");
    assert!(err.contains("example: {\"request\""), "{err}");
    let err = call("work_outline_create_child", json!({ "title": "x" }))
        .await
        .unwrap_err();
    assert!(err.contains("idempotencyKey is required"), "{err}");
    assert!(err.contains("Usage of work_outline_create_child"), "{err}");
    let err = call(
        "work_outline_propose_instruction",
        json!({ "idempotencyKey": "k" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("text is empty"), "{err}");
    assert!(
        err.contains("Usage of work_outline_propose_instruction"),
        "{err}"
    );
    let err = call("work_outline_resolve_user_action_request", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("missing field `userActionRequestId`"), "{err}");
    assert!(err.contains("example:"), "{err}");
    let err = call("work_outline_set_status", json!({ "status": "done" }))
        .await
        .unwrap_err();
    assert!(err.contains("status must be active or completed"), "{err}");
    assert!(err.contains("Usage of work_outline_set_status"), "{err}");

    // `parentId` stands for `parentWorkItemId` on create_child; the key stays
    // required, and `idempotency_key` is accepted for it.
    let child = call(
        "work_outline_create_child",
        json!({ "title": "Child", "parentId": root, "idempotency_key": "child-1" }),
    )
    .await
    .unwrap();
    assert_eq!(child["parentWorkItemId"], root.as_str());
    // A retry with the snake_case key replays the same child.
    let retry = call(
        "work_outline_create_child",
        json!({ "title": "Child", "parent": root, "idempotencyKey": "child-1" }),
    )
    .await
    .unwrap();
    assert_eq!(retry["workItemId"], child["workItemId"]);
    let null_reference = call(
        "work_outline_create_child",
        json!({ "title": "Child", "parent": root, "idempotencyKey": "child-1", "linearIssue": null }),
    )
    .await
    .unwrap();
    assert_eq!(null_reference["workItemId"], child["workItemId"]);
}

// ---- project scope and worker destination ------------------------------------

async fn update_item(rig: &Rig, op: &str, item: &str, changes: Value) -> Result<Value, String> {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "updateWorkItem", "workItemId": item, "changes": changes,
                    "operationId": op }),
        )
        .await
        .map_err(|e| e.to_string())
}

/// `Project` (isProject, destination = Tickets) with `Planning` and `Tickets`
/// children, a second project `Other` with its own `Other tickets`, and a
/// bystander root.
struct ProjectRig {
    project: String,
    planning: String,
    tickets: String,
    ticket: String,
    other: String,
    other_tickets: String,
    bystander: String,
}

async fn project_rig(rig: &Rig) -> ProjectRig {
    let project = create_item(rig, "p-proj", "Project", None).await;
    let planning = create_item(rig, "p-plan", "Planning", Some(&project)).await;
    let tickets = create_item(rig, "p-tix", "Tickets", Some(&project)).await;
    let ticket = create_item(rig, "p-t1", "Ticket 1", Some(&tickets)).await;
    let other = create_item(rig, "p-other", "Other", None).await;
    let other_tickets = create_item(rig, "p-otix", "Other tickets", Some(&other)).await;
    let bystander = create_item(rig, "p-by", "Bystander", None).await;
    update_item(rig, "p-mark", &project, json!({ "isProject": true }))
        .await
        .unwrap();
    update_item(
        rig,
        "p-dest",
        &project,
        json!({ "workerDestination": tickets }),
    )
    .await
    .unwrap();
    update_item(rig, "p-mark2", &other, json!({ "isProject": true }))
        .await
        .unwrap();
    ProjectRig {
        project,
        planning,
        tickets,
        ticket,
        other,
        other_tickets,
        bystander,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hand_off_refuses_different_projects_in_the_same_space() {
    let rig = rig().await;
    let projects = project_rig(&rig).await;
    assign_chat(&rig, "handoff-project-a", CHAT, &projects.planning).await;
    assign_chat(&rig, "handoff-project-b", OTHER_CHAT, &projects.other_tickets).await;
    let wakes = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        rig.core.sessions.clone(), rig.core.doc_host.clone(), rig.core.workspace.clone(), None, Arc::new(|| 1_000_000),
    );
    let error = wakes.hand_off_orchestrator(CHAT, OTHER_CHAT, CHAT).await.unwrap_err();
    assert!(error.contains("same project"), "{error}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn project_destination_worker_prompt_wakes_without_watcher_delay() {
    let rig = rig().await;
    let project = project_rig(&rig).await;
    assign_chat(&rig, "wake-orchestrator", CHAT, &project.planning).await;
    assign_chat(&rig, "wake-worker", OTHER_CHAT, &project.ticket).await;
    let wakes = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        rig.core.sessions.clone(), rig.core.doc_host.clone(), rig.core.workspace.clone(), None, Arc::new(|| 1_000_000),
    );
    wakes.ingest_worker_signal(mast_engine::sessions::WorkerSignal::Input {
        chat_id: OTHER_CHAT.into(), input: mast_proto::AwaitingInput { request_id: "r1".into(), questions: vec![mast_proto::UserInputQuestion { permission: None,
            command_approval: None,
            allow_custom: false,
            id: "q1".into(), header: "Approval".into(), question: "Run build?".into(), options: vec![], multi_select: false,
        }] },
    }).await;
    let held = wakes.status(CHAT).held;
    assert_eq!(held.len(), 1);
    assert!(held[0].summary.contains("waiting for approval: Run build?"));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_records_its_worker_destination_and_validates_it() {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    let snap = snapshot(&rig);
    let project = snap.item(&p.project).unwrap();
    assert!(project.is_project);
    assert_eq!(project.worker_destination_id.as_deref(), Some(p.tickets.as_str()));
    assert!(!snap.item(&p.planning).unwrap().is_project);
    // A destination must be inside the project, and needs a project.
    for (op, item, dest) in [
        ("bad-1", &p.project, &p.other_tickets),
        ("bad-2", &p.project, &p.bystander),
        ("bad-3", &p.planning, &p.ticket),
    ] {
        let error = update_item(&rig, op, item, json!({ "workerDestination": dest }))
            .await
            .unwrap_err();
        assert!(error.contains("invalid"), "{error}");
    }
    // Unmarking keeps clearing possible; clearing works.
    update_item(&rig, "clear", &p.project, json!({ "workerDestination": null }))
        .await
        .unwrap();
    assert_eq!(snapshot(&rig).item(&p.project).unwrap().worker_destination_id, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_planning_chat_creates_workers_in_the_project_destination() {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    assign_chat(&rig, "assign-plan", CHAT, &p.planning).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // No parent: the project's destination, though Tickets is a sibling.
    let made = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "ENG-1", "idempotencyKey": "k1" }),
    )
    .await
    .unwrap();
    assert_eq!(made["parentWorkItemId"], p.tickets);
    assert_eq!(made["resolvedToWorkerDestination"], true);
    // read_current tells the chat (and create_chat) where workers go.
    let current = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(current["workerDestinationId"], p.tickets);
    // An explicit parent wins: the Planning item itself, a sibling, a ticket.
    for (key, parent) in [("k2", &p.planning), ("k3", &p.tickets), ("k4", &p.ticket)] {
        let made = tool(
            &rig,
            &token,
            "work_outline_create_child",
            json!({ "title": "x", "idempotencyKey": key, "parentWorkItemId": parent }),
        )
        .await
        .unwrap();
        assert_eq!(made["parentWorkItemId"], *parent);
        assert_eq!(made["resolvedToWorkerDestination"], false);
    }
    // The project scope ends at the project: nothing outside it is reachable.
    for (key, parent) in [
        ("k5", &p.other),
        ("k6", &p.other_tickets),
        ("k7", &p.bystander),
    ] {
        let error = tool(
            &rig,
            &token,
            "work_outline_create_child",
            json!({ "title": "x", "idempotencyKey": key, "parentWorkItemId": parent }),
        )
        .await
        .unwrap_err();
        assert!(error.contains("unauthorized"), "{key}: {error}");
    }
    // Reads follow the same scope; the project's siblings stay hidden.
    let read = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "workItemId": p.tickets }),
    )
    .await
    .unwrap();
    assert_eq!(read["workItemId"], p.tickets);
    let error = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "workItemId": p.other_tickets }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("unauthorized"), "{error}");
    // Everything else keeps the own-item rule: no status change on a sibling.
    let error = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "active", "workItemId": p.tickets, "idempotencyKey": "s1" }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("unauthorized"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_project_the_scope_is_still_own_item_and_descendants() {
    let rig = rig().await;
    let root = create_item(&rig, "n-root", "Coordinator", None).await;
    let step = create_item(&rig, "n-step", "Step", Some(&root)).await;
    let sibling = create_item(&rig, "n-sib", "Sibling", Some(&root)).await;
    assign_chat(&rig, "assign-step", CHAT, &step).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let made = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "x", "idempotencyKey": "k1" }),
    )
    .await
    .unwrap();
    assert_eq!(made["parentWorkItemId"], step);
    assert_eq!(made["resolvedToWorkerDestination"], false);
    let error = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "x", "idempotencyKey": "k2", "parentWorkItemId": sibling }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("unauthorized"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_inside_the_destination_keeps_its_sub_items_under_itself() {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    assign_chat(&rig, "assign-worker", CHAT, &p.ticket).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let made = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "sub", "idempotencyKey": "k1" }),
    )
    .await
    .unwrap();
    assert_eq!(made["parentWorkItemId"], p.ticket);
    assert_eq!(made["resolvedToWorkerDestination"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_chat_for_a_project_starts_in_its_destination() {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    assign_chat(&rig, "assign-plan", CHAT, &p.planning).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    let key = "to-project";
    tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args(key, &p.project, "workspace-write"),
    )
    .await
    .unwrap();
    let child = derive_id(
        &mast_proto::agent_tools::operation_id(CHAT, key),
        roles::CHAT,
    );
    let snap = snapshot(&rig);
    let assignment = snap.assignments.iter().find(|a| a.chat_id == child).unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(p.tickets.as_str()));
    // A named item inside the project (the Planning item's sibling ticket) wins.
    tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("to-ticket", &p.ticket, "workspace-write"),
    )
    .await
    .unwrap();
    let child = derive_id(
        &mast_proto::agent_tools::operation_id(CHAT, "to-ticket"),
        roles::CHAT,
    );
    let snap = snapshot(&rig);
    let assignment = snap.assignments.iter().find(|a| a.chat_id == child).unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(p.ticket.as_str()));
    // Outside the project: refused.
    for (key, item) in [("out-1", &p.other_tickets), ("out-2", &p.bystander)] {
        let error = tool(
            &rig,
            &token,
            "chat_launch_child_for_item",
            launch_args(key, item, "workspace-write"),
        )
        .await
        .unwrap_err();
        assert!(error.contains("unauthorized"), "{key}: {error}");
    }
}

// ---- project templates ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn saving_a_project_template_freezes_the_subtree_and_agents_cannot() {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    add_instruction(&rig, "tpl-i1", json!({"type":"workItem","workItemId": p.project}), "Be careful").await;
    add_instruction(&rig, "tpl-i2", json!({"type":"workItem","workItemId": p.planning}), "Write a plan").await;
    add_instruction(&rig, "tpl-i3", json!({"type":"workItem","workItemId": p.ticket}), "ticket only").await;
    let reply = rig
        .client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "saveWorkItemTemplate", "spaceId": SPACE,
                    "projectWorkItemId": p.project, "name": "Standard", "operationId": "tpl-save" }),
        )
        .await
        .unwrap();
    let template_id = derive_id("tpl-save", roles::TEMPLATE);
    assert_eq!(reply["templateId"], json!(template_id));

    let snap = snapshot(&rig);
    assert_eq!(snap.templates.len(), 1);
    let template = &snap.templates[0];
    assert_eq!((template.name.as_str(), template.revision), ("Standard", 1));
    let body = &template.body;
    assert!(body.is_project);
    assert_eq!(body.instructions, vec!["Be careful"]);
    let titles: Vec<&str> = body.children.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Planning", "Tickets"]);
    assert_eq!(body.children[0].instructions, vec!["Write a plan"]);
    assert!(body.children[1].is_worker_destination);
    assert!(body.children[1].children.is_empty(), "tickets are dropped");
    let item = snap.item(&p.project).unwrap();
    assert_eq!(item.template_id.as_deref(), Some(template_id.as_str()));
    assert_eq!(item.template_revision, Some(1));

    // A retry with the same key replays; a re-save bumps the revision.
    let replay = rig
        .client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "saveWorkItemTemplate", "spaceId": SPACE,
                    "projectWorkItemId": p.project, "name": "Standard", "operationId": "tpl-save" }),
        )
        .await
        .unwrap();
    assert_eq!(replay["templateId"], json!(template_id));
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "saveWorkItemTemplate", "spaceId": SPACE, "projectWorkItemId": p.project,
                    "name": "Standard", "templateId": template_id, "operationId": "tpl-resave" }),
        )
        .await
        .unwrap();
    assert_eq!(snapshot(&rig).templates[0].revision, 2);

    // The WORK_OUTLINE_MUTATE path reports a duplicate name as invalid.
    let duplicate = rig
        .client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "saveWorkItemTemplate", "spaceId": SPACE, "projectWorkItemId": p.other,
                    "name": "standard", "operationId": "tpl-dup" }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(duplicate.contains("invalid"), "{duplicate}");

    // Deleting tombstones it; the project keeps its link.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "deleteWorkItemTemplate", "templateId": template_id,
                    "operationId": "tpl-del" }),
        )
        .await
        .unwrap();
    let snap = snapshot(&rig);
    assert!(snap.templates.is_empty());
    assert_eq!(snap.item(&p.project).unwrap().template_id.as_deref(), Some(template_id.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn propose_edit_and_remove_target_instructions_the_chat_can_read() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Feature A", None).await;
    let root_b = create_item(&rig, "op-b", "Feature B", None).await;
    add_instruction(
        &rig,
        "ins-a",
        json!({ "type": "workItem", "workItemId": root_a }),
        "Run the linter.",
    )
    .await;
    add_instruction(
        &rig,
        "ins-b",
        json!({ "type": "workItem", "workItemId": root_b }),
        "Elsewhere rule.",
    )
    .await;
    assign_chat(&rig, "assign-caller", CHAT, &root_a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // read_current lists instruction ids with their text.
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    let entry = &read["effectiveInstructions"]["entries"][0];
    assert_eq!(entry["text"], "Run the linter.");
    let id = entry["instructionId"].as_str().unwrap().to_owned();
    let foreign = snapshot(&rig)
        .instructions
        .iter()
        .find(|i| i.text == "Elsewhere rule.")
        .unwrap()
        .id
        .clone();

    let edit = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "action": "edit", "instructionId": id, "text": "Run the linter and tests.",
                "rationale": "CI runs both", "idempotencyKey": "e1" }),
    )
    .await
    .unwrap();
    assert_eq!(edit["action"], "edit");
    let proposal_id = edit["proposalId"].as_str().unwrap().to_owned();
    let snap = snapshot(&rig);
    let proposal = snap.instruction_proposal(&proposal_id).unwrap();
    assert_eq!(proposal.base_text.as_deref(), Some("Run the linter."));
    assert_eq!(proposal.rationale.as_deref(), Some("CI runs both"));
    assert_eq!(snap.instructions.len(), 2, "nothing changes until accepted");

    let remove = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "action": "remove", "instructionId": id, "rationale": "obsolete",
                "idempotencyKey": "r1" }),
    )
    .await
    .unwrap();
    assert_eq!(remove["action"], "remove");

    // Validation: unknown id, an id outside the chat's reach, missing id,
    // remove with text, edit without text.
    let err = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "action": "edit", "instructionId": "nope", "text": "x", "idempotencyKey": "v1" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("not found") || err.contains("notFound"), "{err}");
    let err = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "action": "remove", "instructionId": foreign, "idempotencyKey": "v2" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("nauthorized") || err.contains("inherit"), "{err}");
    for (key, args) in [
        ("v3", json!({ "action": "edit", "text": "x" })),
        ("v4", json!({ "action": "remove", "instructionId": id, "text": "x" })),
        ("v5", json!({ "action": "edit", "instructionId": id })),
        ("v6", json!({ "action": "bogus", "instructionId": id })),
    ] {
        let mut args = args;
        args["idempotencyKey"] = json!(key);
        let err = tool(&rig, &token, "work_outline_propose_instruction", args)
            .await
            .unwrap_err();
        assert!(err.contains("agent-tools/invalid"), "{key}: {err}");
    }

    // Accepting the edit rewrites the same instruction.
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "approveWorkItemInstructionProposal", "proposalId": proposal_id,
                    "approvedText": "Run the linter and tests.", "operationId": "approve-1" }),
        )
        .await
        .unwrap();
    let snap = snapshot(&rig);
    let instruction = snap.instructions.iter().find(|i| i.id == id).unwrap();
    assert_eq!(instruction.text, "Run the linter and tests.");
    assert_eq!(snap.instructions.len(), 2);
}

// ---- answering a request with the user's own message -------------------------------

const OUTSIDER: &str = "chat-outsider";

struct AnswerRig {
    rig: Rig,
    p: ProjectRig,
    /// The orchestrator's token (CHAT, on the project item).
    orchestrator: String,
    /// The planner's token (OTHER_CHAT, on Planning).
    planner: String,
    /// A chat on another project's item.
    outsider: String,
    request: String,
    /// Tokens stop working when their lease drops.
    _leases: [mast_engine::agent_tools::AgentTokenLease; 3],
}

async fn answer_rig() -> AnswerRig {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": OUTSIDER, "spaceId": SPACE,
                    "deviceId": rig.core.device_id }),
        )
        .await
        .unwrap();
    assign_chat(&rig, "assign-orch", CHAT, &p.project).await;
    assign_chat(&rig, "assign-plan", OTHER_CHAT, &p.planning).await;
    assign_chat(&rig, "assign-out", OUTSIDER, &p.other_tickets).await;
    let tokens = rig.core.sessions.agent_tokens();
    let leases = [
        tokens.mint(CHAT, "run-o", SPACE, &rig.core.device_id),
        tokens.mint(OTHER_CHAT, "run-p", SPACE, &rig.core.device_id),
        tokens.mint(OUTSIDER, "run-x", SPACE, &rig.core.device_id),
    ];
    let [orchestrator, planner, outsider] = [0, 1, 2].map(|i| leases[i].token().to_owned());
    let raised = tool(
        &rig,
        &planner,
        "work_outline_create_user_action_request",
        json!({ "request": "Approve plan and create tickets?", "dedupeKey": "approve-plan",
                "idempotencyKey": "raise" }),
    )
    .await
    .unwrap();
    let request = raised["userActionRequestId"].as_str().unwrap().to_owned();
    AnswerRig {
        rig,
        p,
        orchestrator,
        planner,
        outsider,
        request,
        _leases: leases,
    }
}

fn request_created_at(rig: &Rig, request: &str) -> i64 {
    snapshot(rig)
        .user_action_requests
        .iter()
        .find(|r| r.id == request)
        .unwrap()
        .created_at
}

/// Write a transcript message into `chat`'s doc.
fn push_message(rig: &Rig, chat: &str, id: &str, role: mast_doc::MessageRole, text: &str, at: i64) {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .push_message(&mast_doc::SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![mast_doc::MessagePart::Text {
                id: format!("{id}-part"),
                text: text.into(),
            }],
            created_at: at,
            device_id: rig.core.device_id.clone(),
            status: None,
            continuation_of: None,
        })
        .unwrap();
}

/// Every text delivered to `chat` (queued rows and transcript messages).
fn delivered_texts(rig: &Rig, chat: &str) -> Vec<String> {
    let handle = rig.core.doc_host.open(chat).unwrap();
    let mut out: Vec<String> = handle
        .doc()
        .read_queue()
        .unwrap()
        .into_iter()
        .map(|q| q.text)
        .collect();
    // An idle chat drains the row straight into a Run command.
    for command in handle.doc().read_commands().unwrap() {
        if let SessionCommandPayload::Run { request, .. } = command.payload {
            out.push(request.prompt);
        }
    }
    for entry in handle.doc().read_entries().unwrap() {
        for part in entry.parts {
            if let mast_doc::MessagePart::Text { text, .. } = part {
                out.push(text);
            }
        }
    }
    out
}

/// Every Re: reply the planner received, whether still queued, drained into a
/// command, or already started as a Run (polled: an idle chat drains async).
async fn replies_to_planner(rig: &Rig) -> Vec<String> {
    let snapshot_of = || -> Vec<String> {
        // The same message shows in the doc and as the Run the harness got,
        // so take whichever view has more rather than adding them.
        let in_doc: Vec<String> = delivered_texts(rig, OTHER_CHAT)
            .into_iter()
            .filter(|t| t.starts_with("Re:"))
            .collect();
        let started: Vec<String> = rig
            .harness
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.prompt.clone())
            .filter(|t| t.starts_with("Re:"))
            .collect();
        if started.len() > in_doc.len() {
            started
        } else {
            in_doc
        }
    };
    for _ in 0..200 {
        if !snapshot_of().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Let a duplicate delivery, if any, show up before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    snapshot_of()
}

fn answer_args(request: &str, message: &str, key: &str) -> Value {
    json!({ "requestId": request, "userMessageId": message, "idempotencyKey": key })
}

const ANSWER: &str = "answer_user_action_request";

#[tokio::test(flavor = "multi_thread")]
async fn answering_with_the_users_own_message_delivers_it_and_resolves_as_theirs() {
    let a = answer_rig().await;
    let at = request_created_at(&a.rig, &a.request) + 1_000;
    push_message(
        &a.rig,
        CHAT,
        "u-1",
        mast_doc::MessageRole::User,
        "  make linear tickets  ",
        at,
    );
    let reply = tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        answer_args(&a.request[..8], "u-1", "ans-1"),
    )
    .await
    .unwrap();
    assert_eq!(reply["resolved"], true);
    assert_eq!(reply["answeredBy"], "user");
    assert_eq!(reply["delivered"], true);
    assert_eq!(reply["sourceChatId"], CHAT);
    assert_eq!(reply["sourceMessageId"], "u-1");

    // The planner gets the user's reply, not a relay.
    let expected = "Re: \"Approve plan and create tickets?\": make linear tickets";
    let texts = replies_to_planner(&a.rig).await;
    assert_eq!(texts, [expected], "{texts:?}");

    // The row records the user's answer with its source.
    let snap = snapshot(&a.rig);
    let request = snap
        .user_action_requests
        .iter()
        .find(|r| r.id == a.request)
        .unwrap();
    let resolution = request.resolution.as_ref().unwrap();
    assert!(resolution.answered_by_user());
    assert_eq!(resolution.note.as_deref(), Some("make linear tickets"));
    let answer = resolution.answer.as_ref().unwrap();
    assert_eq!(answer.chat_id, CHAT);
    assert_eq!(answer.message_id, "u-1");
    assert_eq!(answer.text, "make linear tickets");
    assert!(request_is_resolved(&snap, &a.request));

    // A replay with the same key neither delivers twice nor fails.
    let again = tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        answer_args(&a.request[..8], "u-1", "ans-1"),
    )
    .await
    .unwrap();
    assert_eq!(again["resolved"], true);
    assert_eq!(replies_to_planner(&a.rig).await, [expected]);
}

#[tokio::test(flavor = "multi_thread")]
async fn answering_refuses_anything_but_a_human_message_newer_than_the_request() {
    let a = answer_rig().await;
    let created = request_created_at(&a.rig, &a.request);
    let later = created + 1_000;
    use mast_doc::MessageRole::{Assistant, System, User};
    push_message(&a.rig, CHAT, "m-assistant", Assistant, "go ahead", later);
    push_message(&a.rig, CHAT, "m-system", System, "go ahead", later);
    push_message(
        &a.rig,
        CHAT,
        "m-relay",
        User,
        "[Message from Mast chat Orchestrator (abc). Reply to it with the Mast `send_message` tool, chat abc.]\n\nMichael, verbatim: make linear tickets",
        later,
    );
    push_message(
        &a.rig,
        CHAT,
        "m-wake",
        User,
        "[Mast system message, not from the user: worker updates for you, the orchestrator.]\nWorker updates:\n- x",
        later,
    );
    push_message(&a.rig, CHAT, "m-old", User, "make linear tickets", created - 1_000);
    push_message(&a.rig, CHAT, "m-same", User, "make linear tickets", created);
    push_message(&a.rig, OTHER_CHAT, "m-foreign", User, "make linear tickets", later);
    let long = "x".repeat(1_001);
    push_message(&a.rig, CHAT, "m-long", User, &long, later);
    let cases = [
        ("m-assistant", "assistant message"),
        ("m-system", "system message"),
        ("m-relay", "from an agent or Mast"),
        ("m-wake", "from an agent or Mast"),
        ("m-old", "older than the request"),
        ("m-same", "older than the request"),
        // Another chat's message is not in the caller's own transcript.
        ("m-foreign", "not a message in this chat"),
        ("nope", "not a message in this chat"),
        ("m-long", "longer than"),
    ];
    for (i, (message, needle)) in cases.iter().enumerate() {
        let err = tool(
            &a.rig,
            &a.orchestrator,
            ANSWER,
            answer_args(&a.request, message, &format!("bad-{i}")),
        )
        .await
        .unwrap_err();
        assert!(err.contains(needle), "{message}: {err}");
    }
    // Nothing was delivered or resolved by any of those.
    assert!(!request_is_resolved(&snapshot(&a.rig), &a.request));
    assert!(
        delivered_texts(&a.rig, OTHER_CHAT)
            .iter()
            .all(|t| !t.starts_with("Re:"))
    );
    // The caller cannot supply the text itself.
    let err = tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        json!({ "requestId": a.request, "userMessageId": "m-old", "text": "go",
                "idempotencyKey": "extra" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("older than the request"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn answering_refuses_resolved_and_out_of_reach_requests() {
    let a = answer_rig().await;
    let later = request_created_at(&a.rig, &a.request) + 1_000;
    push_message(&a.rig, CHAT, "u-1", mast_doc::MessageRole::User, "go", later);
    push_message(&a.rig, OUTSIDER, "u-x", mast_doc::MessageRole::User, "go", later);
    // A chat in another project cannot answer this project's request.
    let err = tool(
        &a.rig,
        &a.outsider,
        ANSWER,
        answer_args(&a.request, "u-x", "out-1"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(!request_is_resolved(&snapshot(&a.rig), &a.request));
    // An unknown request.
    let err = tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        answer_args("no-such-request", "u-1", "k-missing"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("not found") || err.contains("notFound"), "{err}");
    // The planner's own request can be answered by the planner's own user
    // message too (its own chat is in reach), but only once.
    tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        answer_args(&a.request, "u-1", "ans-1"),
    )
    .await
    .unwrap();
    let err = tool(
        &a.rig,
        &a.orchestrator,
        ANSWER,
        answer_args(&a.request, "u-1", "ans-2"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("already resolved"), "{err}");
    // Only one reply ever reached the planner.
    assert_eq!(replies_to_planner(&a.rig).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn project_scope_reaches_other_chats_requests_and_nothing_wider() {
    let a = answer_rig().await;
    // The orchestrator did not create the planner, yet resolves its request.
    let reach = tool(&a.rig, &a.orchestrator, "chat_project_scope_chats", json!({}))
        .await
        .unwrap();
    let ids: Vec<&str> = reach["chatIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(ids.contains(&OTHER_CHAT) && ids.contains(&CHAT), "{ids:?}");
    assert!(!ids.contains(&OUTSIDER), "{ids:?}");
    // The planner is under the project too, but a chat outside it sees only
    // its own project.
    let out = tool(&a.rig, &a.outsider, "chat_project_scope_chats", json!({}))
        .await
        .unwrap();
    let out_ids: Vec<&str> = out["chatIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(out_ids.contains(&OUTSIDER) && !out_ids.contains(&OTHER_CHAT), "{out_ids:?}");
    // The outsider cannot resolve the planner's request.
    let err = tool(
        &a.rig,
        &a.outsider,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": a.request, "idempotencyKey": "x-resolve" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(!request_is_resolved(&snapshot(&a.rig), &a.request));
    // The orchestrator can.
    let done = tool(
        &a.rig,
        &a.orchestrator,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": a.request, "resolution": "handled",
                "idempotencyKey": "o-resolve" }),
    )
    .await
    .unwrap();
    assert_eq!(done["resolved"], true);
    let snap = snapshot(&a.rig);
    let resolution = snap
        .user_action_requests
        .iter()
        .find(|r| r.id == a.request)
        .unwrap()
        .resolution
        .clone()
        .unwrap();
    // A plain resolve is the agent's own, never marked as the user's.
    assert!(!resolution.answered_by_user());
    assert_eq!(resolution.by.kind, WorkOutlineActorKind::Agent);
    // The planner's own scope without a project keeps the old rule: a chat on
    // a project's Tickets item reaches only inside the project it belongs to
    // (the bystander has no project at all).
    let _ = (&a.planner, &a.p.bystander);
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_scope_without_a_project_includes_descendants_and_excludes_siblings() {
    let rig = rig().await;
    let root = create_item(&rig, "np-root", "Plain", None).await;
    let leaf = create_item(&rig, "np-leaf", "Leaf", Some(&root)).await;
    let sibling = create_item(&rig, "np-sib", "Sibling", None).await;
    assign_chat(&rig, "np-a", CHAT, &leaf).await;
    assign_chat(&rig, "np-b", OTHER_CHAT, &sibling).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let other = tokens.mint(OTHER_CHAT, "run-2", SPACE, &rig.core.device_id);
    let raised = tool(
        &rig,
        other.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Q?", "idempotencyKey": "q" }),
    )
    .await
    .unwrap();
    let request = raised["userActionRequestId"].as_str().unwrap().to_owned();
    let reach = tool(&rig, lease.token(), "chat_project_scope_chats", json!({}))
        .await
        .unwrap();
    assert_eq!(reach["chatIds"], json!([CHAT]));
    let err = tool(
        &rig,
        lease.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request, "idempotencyKey": "r" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assign_chat(&rig, "np-root-assign", CHAT, &root).await;
    assign_chat(&rig, "np-leaf-assign", OTHER_CHAT, &leaf).await;
    let reach = tool(&rig, lease.token(), "chat_project_scope_chats", json!({})).await.unwrap();
    let ids = reach["chatIds"].as_array().unwrap();
    assert!(ids.contains(&json!(CHAT)) && ids.contains(&json!(OTHER_CHAT)));
}

/// A coordinator on a root item resolves requests raised by chats another
/// coordinator created, on descendant items; never on items outside its subtree.
#[tokio::test]
async fn resolve_reaches_requests_on_descendant_items_but_not_outside_the_subtree() {
    let rig = rig().await;
    let root = create_item(&rig, "rs-root", "Root", None).await;
    let feature = create_item(&rig, "rs-feature", "Feature", Some(&root)).await;
    let outside = create_item(&rig, "rs-outside", "Outside", None).await;
    assign_chat(&rig, "rs-a", CHAT, &root).await;
    assign_chat(&rig, "rs-b", OTHER_CHAT, &outside).await;
    // Two chats created by chat B (not by A), one under A's subtree, one outside.
    for (chat, item, op) in [
        ("chat-rs-inside", &feature, "rs-c1"),
        ("chat-rs-outside", &outside, "rs-c2"),
    ] {
        rig.client
            .call(
                methods::MUTATE,
                json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE,
                        "deviceId": rig.core.device_id, "parentChatId": OTHER_CHAT }),
            )
            .await
            .unwrap();
        assign_chat(&rig, op, chat, item).await;
    }
    let tokens = rig.core.sessions.agent_tokens();
    let a = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let mut ids = Vec::new();
    for (chat, key) in [("chat-rs-inside", "q-in"), ("chat-rs-outside", "q-out")] {
        let lease = tokens.mint(chat, "run-x", SPACE, &rig.core.device_id);
        let raised = tool(
            &rig,
            lease.token(),
            "work_outline_create_user_action_request",
            json!({ "request": "Q?", "idempotencyKey": key }),
        )
        .await
        .unwrap();
        ids.push(raised["userActionRequestId"].as_str().unwrap().to_owned());
    }
    let err = tool(
        &rig,
        a.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ids[1], "idempotencyKey": "r-out" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(!request_is_resolved(&snapshot(&rig), &ids[1]));
    tool(
        &rig,
        a.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ids[0], "idempotencyKey": "r-in" }),
    )
    .await
    .unwrap();
    assert!(request_is_resolved(&snapshot(&rig), &ids[0]));
}

// ---- admin chats ---------------------------------------------------------------

const OTHER_SPACE: &str = "space-2";

/// A second Space with one item, for "never another Space" checks.
async fn other_space_item(rig: &Rig) -> String {
    let path = std::path::Path::new(&rig.space_path).join("other-space");
    std::fs::create_dir_all(&path).unwrap();
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": OTHER_SPACE, "deviceId": rig.core.device_id,
                    "path": path.to_string_lossy() }),
        )
        .await
        .unwrap();
    let op = "op-foreign";
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": OTHER_SPACE, "title": "Foreign",
                    "operationId": op }),
        )
        .await
        .unwrap();
    derive_id(op, roles::ITEM)
}

async fn grant(rig: &Rig, chat: &str, secs: Option<u64>, skip_bulk: bool) -> Value {
    rig.client
        .call(
            methods::GRANT_ADMIN,
            json!({ "chatId": chat, "durationSecs": secs, "skipBulkConfirmation": skip_bulk }),
        )
        .await
        .unwrap()
}

fn title_of(rig: &Rig, id: &str) -> String {
    snapshot(rig).item(id).map(|i| i.title.clone()).unwrap_or_default()
}

async fn admin(rig: &Rig, token: &str, args: Value) -> Result<Value, String> {
    tool(rig, token, "work_outline_admin", args).await
}

async fn auth_origin(rig: &Rig, token: &str) -> Value {
    rig.client
        .call(mast_proto::agent_tools::AUTHENTICATE_METHOD, json!({ "token": token }))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_grants_expire_revoke_and_are_user_only() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    assert!(auth_origin(&rig, &token).await.get("adminSpaceId").is_none());
    let err = admin(&rig, &token, json!({ "read": true })).await.unwrap_err();
    assert!(err.contains("not an admin chat"), "{err}");

    // A projectless chat cannot be admin.
    assert!(
        rig.client
            .call(methods::GRANT_ADMIN, json!({ "chatId": PROJECTLESS }))
            .await
            .is_err()
    );

    // No agent tool grants: unknown tool names are refused.
    for name in ["grant_admin", "work_outline_grant_admin"] {
        assert!(tool(&rig, &token, name, json!({ "chatId": OTHER_CHAT })).await.is_err());
    }

    let granted = grant(&rig, CHAT, Some(3600), false).await;
    assert_eq!(granted["spaceId"], SPACE);
    assert_eq!(auth_origin(&rig, &token).await["adminSpaceId"], SPACE);
    let listed = rig.client.call(methods::LIST_ADMIN_GRANTS, json!({})).await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(admin(&rig, &token, json!({ "read": true })).await.is_ok());

    // Expiry: the grant stops working at once.
    tokens.admin().advance_clock_ms(3_600_001);
    assert!(auth_origin(&rig, &token).await.get("adminSpaceId").is_none());
    assert!(admin(&rig, &token, json!({ "read": true })).await.is_err());
    let listed = rig.client.call(methods::LIST_ADMIN_GRANTS, json!({})).await.unwrap();
    assert!(listed.as_array().unwrap().is_empty());

    // Renew, then revoke.
    grant(&rig, CHAT, None, false).await;
    assert_eq!(auth_origin(&rig, &token).await["adminSpaceId"], SPACE);
    rig.client.call(methods::REVOKE_ADMIN, json!({ "chatId": CHAT })).await.unwrap();
    assert!(auth_origin(&rig, &token).await.get("adminSpaceId").is_none());
    assert!(admin(&rig, &token, json!({ "read": true })).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_changes_anything_in_its_space_but_nothing_outside_it() {
    let rig = rig().await;
    let foreign = other_space_item(&rig).await;
    let a = create_item(&rig, "op-a", "Alpha", None).await;
    let b = create_item(&rig, "op-b", "Beta", None).await;
    let child = create_item(&rig, "op-c", "Child", Some(&b)).await;
    assign_chat(&rig, "assign-admin", CHAT, &a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // Without a grant the same tool call is refused and Beta is untouched.
    assert!(admin(&rig, &token, json!({ "idempotencyKey": "k0",
        "steps": [{ "op": "rename", "itemId": b, "title": "Nope" }] })).await.is_err());
    // The ordinary tool still only reaches the chat's own subtree.
    assert!(tool(&rig, &token, "work_outline_set_status",
        json!({ "workItemId": b, "status": "active" })).await.is_err());

    grant(&rig, CHAT, Some(3600), false).await;
    let done = admin(&rig, &token, json!({ "idempotencyKey": "k1", "steps": [
        { "op": "rename", "itemId": &b[..8], "title": "Beta 2" },
        { "op": "status", "itemId": child, "status": "active" },
        { "op": "move", "itemId": child, "parentId": a },
        { "op": "create", "parentId": a, "title": "New", "ref": "n" },
        { "op": "instruction_add", "itemId": "$n", "text": "Be careful." },
        { "op": "project", "itemId": b, "isProject": true },
        { "op": "archive", "itemId": child },
    ]})).await.unwrap();
    assert_eq!(done["applied"], 7, "{done}");
    assert_eq!(title_of(&rig, &b), "Beta 2");
    let snap = snapshot(&rig);
    assert!(snap.item(&b).unwrap().is_project);
    assert!(snap.item(&child).unwrap().archived_at.is_some());
    assert!(snap.instructions.iter().any(|i| i.text == "Be careful."));

    // Widened existing tools: set_status anywhere in the Space.
    assert!(tool(&rig, &token, "work_outline_set_status",
        json!({ "workItemId": b, "status": "active" })).await.is_ok());

    // Other Space: refused for every op shape.
    for step in [
        json!({ "op": "rename", "itemId": foreign, "title": "Hacked" }),
        json!({ "op": "move", "itemId": a, "parentId": foreign }),
        json!({ "op": "create", "parentId": foreign, "title": "x" }),
        json!({ "op": "archive", "itemId": foreign }),
    ] {
        let err = admin(&rig, &token, json!({ "idempotencyKey": format!("f{step}"),
            "steps": [step] })).await.unwrap_err();
        assert!(err.contains("nauthorized") || err.contains("own Space"), "{err}");
    }
    let inputs = rig.core.workspace.work_outline_inputs(OTHER_SPACE);
    let other = mast_doc::materialize_work_outline(OTHER_SPACE, &inputs);
    assert_eq!(other.item(&foreign).unwrap().title, "Foreign");

    // Unsupported ops (Linear/GitHub, delete, grant) are not in the vocabulary.
    for op in ["linear_link", "github_comment", "delete_item", "grant_admin"] {
        let err = admin(&rig, &token, json!({ "idempotencyKey": format!("u{op}"),
            "steps": [{ "op": op }] })).await.unwrap_err();
        assert!(err.contains("unknown op"), "{err}");
    }

    // Revoke: the next call is refused.
    rig.client.call(methods::REVOKE_ADMIN, json!({ "chatId": CHAT })).await.unwrap();
    assert!(admin(&rig, &token, json!({ "idempotencyKey": "k9",
        "steps": [{ "op": "rename", "itemId": b, "title": "Late" }] })).await.is_err());
    assert_eq!(title_of(&rig, &b), "Beta 2");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_resolves_requests_and_accepts_proposals_with_its_own_attribution() {
    let rig = rig().await;
    let a = create_item(&rig, "op-a", "Alpha", None).await;
    let b = create_item(&rig, "op-b", "Beta", None).await;
    assign_chat(&rig, "assign-admin", CHAT, &a).await;
    assign_chat(&rig, "assign-worker", OTHER_CHAT, &b).await;
    add_instruction(&rig, "ins-b", json!({ "type": "workItem", "workItemId": b }), "Old rule.").await;
    let tokens = rig.core.sessions.agent_tokens();
    let admin_lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let worker_lease = tokens.mint(OTHER_CHAT, "run-2", SPACE, &rig.core.device_id);
    let raised = tool(&rig, worker_lease.token(), "work_outline_create_user_action_request",
        json!({ "request": "Which db?", "dedupeKey": "db", "idempotencyKey": "q" })).await.unwrap();
    let request_id = raised["userActionRequestId"].as_str().unwrap().to_owned();
    let rule = snapshot(&rig).instructions[0].id.clone();
    let proposal = tool(&rig, worker_lease.token(), "work_outline_propose_instruction",
        json!({ "action": "edit", "instructionId": rule, "text": "New rule.",
                "idempotencyKey": "p" })).await.unwrap();
    let proposal_id = proposal["proposalId"].as_str().unwrap().to_owned();

    // Not admin: another chat's request is off limits.
    assert!(tool(&rig, admin_lease.token(), "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "x" })).await.is_err());

    grant(&rig, CHAT, Some(3600), false).await;
    let done = admin(&rig, admin_lease.token(), json!({ "idempotencyKey": "k", "steps": [
        { "op": "resolve_request", "requestId": &request_id[..8], "note": "Postgres" },
        { "op": "proposal_accept", "proposalId": proposal_id },
    ]})).await.unwrap();
    assert_eq!(done["applied"], 2, "{done}");
    let snap = snapshot(&rig);
    let resolved = snap.user_action_requests.iter().find(|r| r.id == request_id).unwrap();
    let resolution = resolved.resolution.as_ref().unwrap();
    assert_eq!(resolution.by.kind, WorkOutlineActorKind::Agent, "never the user");
    assert_eq!(resolution.by.chat_id.as_deref(), Some(CHAT));
    assert!(resolution.note.as_deref().unwrap().contains("by admin chat"));
    assert_eq!(snap.instructions.iter().find(|i| i.id == rule).unwrap().text, "New rule.");
}

#[tokio::test(flavor = "multi_thread")]
async fn bulk_changes_need_the_users_own_approval_with_a_single_use_token() {
    let rig = rig().await;
    let a = create_item(&rig, "op-a", "Alpha", None).await;
    let mut ids = Vec::new();
    for n in 0..11 {
        ids.push(create_item(&rig, &format!("op-i{n}"), &format!("Item {n}"), None).await);
    }
    assign_chat(&rig, "assign-admin", CHAT, &a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    grant(&rig, CHAT, Some(36000), false).await;
    let steps: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "op": "rename", "itemId": id, "title": "Renamed" }))
        .collect();

    let preview = admin(&rig, &token, json!({ "idempotencyKey": "b1", "steps": steps })).await.unwrap();
    assert_eq!(preview["needsApproval"], true, "{preview}");
    assert_eq!(preview["count"], 11);
    assert!(ids.iter().all(|id| title_of(&rig, id) != "Renamed"), "nothing ran");
    let confirm = preview["confirmToken"].as_str().unwrap().to_owned();
    let request_id = preview["requestId"].as_str().unwrap().to_owned();
    let snap = snapshot(&rig);
    let request = snap.user_action_requests.iter().find(|r| r.id == request_id).unwrap();
    assert!(request.text.starts_with("Approve bulk change"), "{}", request.text);

    // Not yet approved.
    let with_token = |key: &str, token: &str| {
        json!({ "idempotencyKey": key, "steps": steps, "confirmToken": token })
    };
    // The token belongs to the execution that previewed it (same key, same steps).
    let err = admin(&rig, &token, with_token("b1", &confirm)).await.unwrap_err();
    assert!(err.contains("not approved"), "{err}");
    // The agent (admin or not) cannot resolve it itself.
    let err = tool(&rig, &token, "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "self" })).await.unwrap_err();
    assert!(err.contains("nauthorized") || err.contains("approve"), "{err}");
    let err = admin(&rig, &token, json!({ "idempotencyKey": "b3",
        "steps": [{ "op": "resolve_request", "requestId": request_id }] })).await.unwrap_err();
    assert!(err.contains("nauthorized") || err.contains("approve"), "{err}");
    // Different steps than the user saw: a different batch, refused.
    let mut other = steps.clone();
    other[0]["title"] = json!("Sneaky");
    let err = admin(&rig, &token, json!({ "idempotencyKey": "b1", "steps": other,
        "confirmToken": confirm })).await.unwrap_err();
    assert!(err.contains("another batch"), "{err}");

    // The user resolves it.
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({
        "op": "resolveUserActionRequest", "userActionRequestId": request_id,
        "outcome": "approved", "via": "card", "operationId": "user-approves" })).await.unwrap();
    let done = admin(&rig, &token, with_token("b1", &confirm)).await.unwrap();
    assert_eq!(done["applied"], 11, "{done}");
    assert!(ids.iter().all(|id| title_of(&rig, id) == "Renamed"));

    // One batch only: another execution cannot use it, and the same one replays.
    let err = admin(&rig, &token, with_token("b6", &confirm)).await.unwrap_err();
    assert!(err.contains("another batch"), "{err}");
    let again = admin(&rig, &token, with_token("b1", &confirm)).await.unwrap();
    assert_eq!(again["applied"], 11, "a retry of the same batch replays: {again}");

    // Expiry of an unused token.
    let steps2: Vec<Value> = ids.iter().map(|id| json!({ "op": "rename", "itemId": id, "title": "Again" })).collect();
    let p2 = admin(&rig, &token, json!({ "idempotencyKey": "b7", "steps": steps2 })).await.unwrap();
    let t2 = p2["confirmToken"].as_str().unwrap().to_owned();
    tokens.admin().advance_clock_ms(31 * 60 * 1000);
    grant(&rig, CHAT, None, false).await;
    let err = admin(&rig, &token, json!({ "idempotencyKey": "b7", "steps": steps2,
        "confirmToken": t2 })).await.unwrap_err();
    assert!(err.contains("expired"), "{err}");

    // "Skip bulk confirmations" runs the sweep directly.
    grant(&rig, CHAT, None, true).await;
    let done = admin(&rig, &token, json!({ "idempotencyKey": "b9", "steps": steps2 })).await.unwrap();
    assert_eq!(done["applied"], 11);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_turn_is_audited_as_one_entry_and_undone_in_one_step() {
    let rig = rig().await;
    let a = create_item(&rig, "op-a", "Alpha", None).await;
    let b = create_item(&rig, "op-b", "Beta", None).await;
    assign_chat(&rig, "assign-admin", CHAT, &a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-7", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    grant(&rig, CHAT, Some(3600), false).await;
    for (key, steps) in [
        ("t1", json!([{ "op": "rename", "itemId": b, "title": "Beta 2" }])),
        ("t2", json!([{ "op": "move", "itemId": b, "parentId": a },
                      { "op": "group", "title": "Box", "itemIds": [a] }])),
    ] {
        admin(&rig, &token, json!({ "idempotencyKey": key, "steps": steps })).await.unwrap();
    }
    let turns = rig.client.call(methods::ADMIN_AUDIT_LIST, json!({ "spaceId": SPACE })).await.unwrap();
    let turns = turns.as_array().unwrap();
    assert_eq!(turns.len(), 1, "both calls are one turn (one run): {turns:?}");
    assert_eq!(turns[0]["turnId"], "run-7");
    assert!(turns[0]["label"].as_str().unwrap().starts_with("by admin chat"));
    assert_eq!(turns[0]["actions"].as_array().unwrap().len(), 4);
    assert_eq!(snapshot(&rig).item(&b).unwrap().parent_work_item_id.as_deref(), Some(a.as_str()));

    let reply = rig.client.call(methods::ADMIN_UNDO_TURN, json!({ "turnId": "run-7" })).await.unwrap();
    assert_eq!(reply["undone"], 4, "{reply}");
    let snap = snapshot(&rig);
    assert_eq!(snap.item(&b).unwrap().title, "Beta");
    assert_eq!(snap.item(&b).unwrap().parent_work_item_id, None);
    assert_eq!(snap.item(&a).unwrap().parent_work_item_id, None);
    assert!(rig.client.call(methods::ADMIN_UNDO_TURN, json!({ "turnId": "run-7" })).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn child_template_proposal_resolves_an_unlinked_direct_child() {
    let rig = rig().await;
    let container = create_item(&rig, "proposal-container", "Projects", None).await;
    let child = create_item(&rig, "proposal-child", "Project", Some(&container)).await;
    assign_chat(&rig, "proposal-chat", CHAT, &child).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "proposal-run", SPACE, &rig.core.device_id);
    let token = lease.token();
    let missing = tool(&rig, token, "work_outline_propose_instruction", json!({
        "target": "childTemplate", "text": "Rule", "idempotencyKey": "missing"
    })).await.unwrap_err();
    assert!(missing.contains("not a direct child of a template container"), "{missing}");
    grant(&rig, CHAT, Some(3600), false).await;
    admin(&rig, token, json!({ "idempotencyKey": "set-template", "steps": [
        { "op": "template_set", "itemId": container, "template": { "root": {} } }
    ]})).await.unwrap();
    let result = tool(&rig, token, "work_outline_propose_instruction", json!({
        "target": "childTemplate", "containerWorkItemId": container, "text": "Rule", "idempotencyKey": "proposal"
    })).await.unwrap();
    let snap = snapshot(&rig);
    let proposal = snap.instruction_proposals.iter().find(|p| Some(p.id.as_str()) == result["proposalId"].as_str()).unwrap();
    let root = &snap.item(&container).unwrap().container_template.as_ref().unwrap().root.id;
    assert_eq!(proposal.target.rule_node_id.as_deref(), Some(root.as_str()));
    assert_eq!(result["target"]["ruleNodeId"].as_str(), Some(root.as_str()));
    assert_eq!(proposal.target.work_item_id.as_deref(), Some(child.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_configures_a_container_template_and_applies_it_to_existing_items() {
    let rig = rig().await;
    let admin_item = create_item(&rig, "op-admin", "Admin work", None).await;
    let container = create_item(&rig, "op-box", "Projects", None).await;
    let p1 = create_item(&rig, "op-p1", "Alpha", Some(&container)).await;
    let p2 = create_item(&rig, "op-p2", "Beta", Some(&container)).await;
    assign_chat(&rig, "assign-admin", CHAT, &admin_item).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let template = json!({ "root": { "children": [
        { "title": "Planning", "instructions": [{ "text": "Plan first." }] },
        { "title": "Tickets", "isWorkerDestination": true }
    ]}});

    // Non-admin: editing template rules (and applying them elsewhere) is refused.
    let err = admin(&rig, &token, json!({ "idempotencyKey": "t0",
        "steps": [{ "op": "template_set", "itemId": container, "template": template }] }))
        .await
        .unwrap_err();
    assert!(err.contains("not an admin chat"), "{err}");
    let err = tool(&rig, &token, "work_outline_apply_container_template",
        json!({ "workItemId": container })).await.unwrap_err();
    assert!(err.contains("nauthorized"), "{err}");
    assert!(snapshot(&rig).item(&container).unwrap().container_template.is_none());

    // Admin: configure, then apply to every existing child (two items).
    grant(&rig, CHAT, Some(3600), false).await;
    let done = admin(&rig, &token, json!({ "idempotencyKey": "t1", "steps": [
        { "op": "template_set", "itemId": container, "template": template },
        { "op": "template_apply", "itemId": container },
    ]})).await.unwrap();
    assert_eq!(done["applied"], 2, "{done}");
    let snap = snapshot(&rig);
    assert!(snap.item(&container).unwrap().container_template.is_some());
    for project in [&p1, &p2] {
        let titles: Vec<String> = snap
            .item(project)
            .unwrap()
            .child_ids
            .iter()
            .map(|c| snap.item(c).unwrap().title.clone())
            .collect();
        assert!(titles.contains(&"Planning".to_owned()) && titles.contains(&"Tickets".to_owned()), "{titles:?}");
    }

    // The template edit is undoable as part of the turn.
    let reply = rig.client.call(methods::ADMIN_UNDO_TURN, json!({ "turnId": "run-1" })).await.unwrap();
    assert_eq!(reply["skipped"].as_array().unwrap().len(), 1, "apply cannot be undone: {reply}");
    assert!(snapshot(&rig).item(&container).unwrap().container_template.is_none());
}

// ---- archiving finished workers ------------------------------------------------

const ARCHIVE_CHILD: &str = "chat-archive-child";
const ARCHIVE_OUTSIDER: &str = "chat-archive-outsider";

struct ArchiveRig {
    rig: Rig,
    p: ProjectRig,
    orchestrator: String,
    /// CHAT is the orchestrator on Planning; OTHER_CHAT works on a ticket.
    _lease: mast_engine::agent_tools::AgentTokenLease,
}

async fn archive_rig() -> ArchiveRig {
    let rig = rig().await;
    let p = project_rig(&rig).await;
    assign_chat(&rig, "ar-orch", CHAT, &p.planning).await;
    assign_chat(&rig, "ar-worker", OTHER_CHAT, &p.ticket).await;
    for (chat, parent) in [(ARCHIVE_CHILD, Some(CHAT)), (ARCHIVE_OUTSIDER, None)] {
        let mut params = json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE,
                                 "deviceId": rig.core.device_id });
        if let Some(parent) = parent {
            params["parentChatId"] = json!(parent);
        }
        rig.client.call(methods::MUTATE, params).await.unwrap();
    }
    assign_chat(&rig, "ar-out", ARCHIVE_OUTSIDER, &p.other_tickets).await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    ArchiveRig {
        orchestrator: lease.token().to_owned(),
        rig,
        p,
        _lease: lease,
    }
}

fn chat_archived(rig: &Rig, chat: &str) -> bool {
    rig.core.workspace.chat(chat).unwrap().unwrap().archived
}

#[tokio::test(flavor = "multi_thread")]
async fn final_handback_preparation_uses_grant_identity_and_replays_original() {
    let a = archive_rig().await;
    assign_chat(&a.rig, "final-child-assign", ARCHIVE_CHILD, &a.p.ticket).await;
    let generation = snapshot(&a.rig).assignments.iter()
        .find(|assignment| assignment.chat_id == ARCHIVE_CHILD).unwrap().generation.clone().unwrap();
    let lease = a.rig.core.sessions.agent_tokens().mint(ARCHIVE_CHILD, "final-run", SPACE, &a.rig.core.device_id);
    // Establish a fresh typed history before any final intent exists.
    let settings = a.rig._tmp.path().join("final-wakes-settings.json");
    let _fresh = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings.clone()), Arc::new(|| 1_000_000),
    );
    let call = |key: &str, expected: &str, report: &str| mast_proto::agent_tools::AgentToolCallParams {
        token: lease.token().to_owned(), tool: "final_handback".into(),
        arguments: json!({"idempotencyKey": key, "expectedGeneration": expected, "report": report, "chatId": OTHER_CHAT}),
    };
    let first = a.rig.service.agent_tools().prepare_final_handback(&call("final-op", &generation, "Done")).unwrap();
    assert_eq!(first.worker_chat_id, ARCHIVE_CHILD, "caller-supplied chatId is never authority");
    assert_eq!(first.parent_chat_id, CHAT);
    let tuple = serde_json::to_string(&first).unwrap();
    let marker = format!("final-handback:{}", mast_proto::derive_id(&tuple, "authenticated-intent"));
    assert!(a.rig.core.workspace.ledger_holds(&marker), "acknowledgement follows durable local marker");
    let mut forged = first.clone();
    forged.report = "forged".into();
    let forged_marker = format!("final-handback:{}", mast_proto::derive_id(&serde_json::to_string(&forged).unwrap(), "authenticated-intent"));
    assert!(!a.rig.core.workspace.ledger_holds(&forged_marker));
    assert!(a.rig.core.orchestrator_wakes.report_final_handback(&forged).await.unwrap_err().contains("no local authentication receipt"));
    let recovered = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings.clone()), Arc::new(|| 1_000_000),
    );
    recovered.recover_final_handbacks().await;
    assert_eq!(recovered.status(CHAT).held.len(), 1, "authenticated intent is recovered once");
    recovered.recover_final_handbacks().await;
    assert_eq!(recovered.status(CHAT).held.len(), 1, "recovery retry does not duplicate the wake");
    // Existing authenticated intent plus corrupt wake history must not be
    // mistaken for a new store, even after the service is restarted again.
    let events = settings.with_file_name("orchestrator-wake-events.json");
    std::fs::write(&events, b"{").unwrap();
    let restarted = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings.clone()), Arc::new(|| 1_000_000),
    );
    let error = restarted.report_final_handback(&first).await.unwrap_err();
    assert!(error.contains("history is unavailable"), "{error}");
    restarted.recover_final_handbacks().await;
    assert!(restarted.status(CHAT).held.is_empty(), "corrupt history cannot produce a wake");
    let restarted_again = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings.clone()), Arc::new(|| 1_000_000),
    );
    assert!(restarted_again.report_final_handback(&first).await.unwrap_err().contains("history is unavailable"));
    std::fs::remove_file(events).unwrap();
    let missing_history = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings), Arc::new(|| 1_000_000),
    );
    assert!(missing_history.report_final_handback(&first).await.unwrap_err().contains("history is unavailable"));
    assert!(!chat_archived(&a.rig, ARCHIVE_CHILD));
    assign_chat(&a.rig, "final-child-move", ARCHIVE_CHILD, &a.p.other_tickets).await;
    let replay = a.rig.service.agent_tools().prepare_final_handback(&call("final-op", &generation, "Done")).unwrap();
    assert_eq!(replay, first);
    assert!(a.rig.service.agent_tools().prepare_final_handback(&call("new-final", &generation, "Done")).is_err());
    assert!(a.rig.service.agent_tools().prepare_final_handback(&call("final-op", &generation, "Changed")).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn final_handback_receipt_requires_recipient_transcript() {
    let a = archive_rig().await;
    assign_chat(&a.rig, "receipt-child-assign", ARCHIVE_CHILD, &a.p.ticket).await;
    let generation = snapshot(&a.rig).assignments.iter()
        .find(|assignment| assignment.chat_id == ARCHIVE_CHILD).unwrap().generation.clone().unwrap();
    let settings = a.rig._tmp.path().join("receipt-wake-settings.json");
    let events = settings.with_file_name("orchestrator-wake-events.json");
    let wakes = mast_engine::orchestrator_wakes::OrchestratorWakes::new(
        a.rig.core.sessions.clone(), a.rig.core.doc_host.clone(), a.rig.core.workspace.clone(),
        Some(settings), Arc::new(|| 1_000_000),
    );
    let lease = a.rig.core.sessions.agent_tokens().mint(ARCHIVE_CHILD, "receipt-run", SPACE, &a.rig.core.device_id);
    let intent = a.rig.service.agent_tools().prepare_final_handback(&mast_proto::agent_tools::AgentToolCallParams {
        token: lease.token().to_owned(), tool: "final_handback".into(),
        arguments: json!({"idempotencyKey":"receipt-op", "expectedGeneration":generation, "report":"Final result"}),
    }).unwrap();
    wakes.report_final_handback(&intent).await.unwrap();
    let before: Value = serde_json::from_slice(&std::fs::read(&events).unwrap()).unwrap();
    assert!(before["delivered_handbacks"].as_object().unwrap().is_empty());
    wakes.evaluate(CHAT, true).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !a.rig.harness.requests.lock().unwrap().iter().any(|request| request.prompt.contains("Final result")) {
        assert!(tokio::time::Instant::now() < deadline, "final handback did not reach the parent");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        a.rig.core.doc_host.flush_all();
        wakes.flush_receipts().await;
        let after: Value = serde_json::from_slice(&std::fs::read(&events).unwrap()).unwrap();
        if after["delivered_handbacks"][&intent.operation_id].is_object() { break; }
        assert!(tokio::time::Instant::now() < deadline, "receipt was not saved: {after}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(wakes.report_final_handback(&intent).await.unwrap().delivery, "delivered");
}

async fn await_handback_receipt(core: &EngineCore, data_dir: &Path, operation_id: &str) {
    let path = data_dir.join("orchestrator-wake-events.json");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        core.doc_host.flush_all();
        core.orchestrator_wakes.flush_receipts().await;
        let store: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if store["delivered_handbacks"][operation_id].is_object() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "handback receipt was not saved");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_rpc_recovers_held_report_and_archives_once() {
    let a = archive_rig().await;
    let rig = a.rig;
    assign_chat(&rig, "handoff-work", ARCHIVE_CHILD, &a.p.ticket).await;
    rig.core.doc_host.queue_command(
        ARCHIVE_CHILD, run_command("Do the assigned work", &rig.space_path),
    ).unwrap();
    wait_for(|| !rig.harness.spawns.lock().unwrap().is_empty(), "worker run").await;
    let lease = rig.core.sessions.agent_tokens().mint(
        ARCHIVE_CHILD, "final-handback-test", SPACE, &rig.core.device_id,
    );
    let current = tool(&rig, lease.token(), "work_outline_read_current", json!({})).await.unwrap();
    let arguments = json!({
        "expectedGeneration": current["assignmentGeneration"],
        "idempotencyKey": "lost-reply",
        "report": "Unique final handback result",
    });
    rig.core.orchestrator_wakes.note_user_activity(CHAT, Some(true)).await;
    let lost_reply = tool(&rig, lease.token(), "final_handback", arguments.clone()).await.unwrap();
    assert_eq!(lost_reply["delivery"], "queuedForParent");
    assert_eq!(rig.core.orchestrator_wakes.status(CHAT).held.len(), 1);
    assert!(!chat_archived(&rig, ARCHIVE_CHILD));

    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(ARCHIVE_CHILD), "worker turn end").await;
    rig.core.shutdown().await;
    drop(lease);
    let Rig { _tmp, core, client, service, space_path, .. } = rig;
    drop((core, client, service));

    let harness = GatedHarness::default();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness.clone()));
    let core = EngineCore::assemble(
        &_tmp.path().join("data"), Arc::new(registry), HarnessId::Mock, None,
    ).unwrap();
    wait_for(|| !core.orchestrator_wakes.status(CHAT).held.is_empty(), "recovered held report").await;
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    let lease = core.sessions.agent_tokens().mint(
        ARCHIVE_CHILD, "retry-after-restart", SPACE, &core.device_id,
    );
    let rpc = mast_rpc::memory_client(core.rpc_service());
    let retry = json!({ "token": lease.token(), "tool": "final_handback", "arguments": arguments });
    let reply: Value = rpc.call(mast_proto::agent_tools::TOOL_CALL_METHOD, retry.clone()).await.unwrap();
    assert_eq!(reply["delivery"], "queuedForParent");
    assert_eq!(core.orchestrator_wakes.status(CHAT).held.iter()
        .filter(|event| event.kind == mast_proto::WakeEventKind::WorkerReport).count(), 1);

    let worker = core.doc_host.open(ARCHIVE_CHILD).unwrap();
    worker.doc().push_queued(&mast_doc::QueuedMessage {
        id: "pending-worker-message".into(),
        text: "Queued follow-up pending review".into(),
        attachments: vec![],
        hold_for_turn_end: false,
        issued_by: core.device_id.clone(),
        issued_at: 1,
        edited_at: None,
        delivery_gate: Some(mast_doc::QueueDeliveryGate::ReviewRequired {
            previous_lease_id: "held-edit".into(),
            owner_device_id: core.device_id.clone(),
            since_ms: 1,
            base_text_hash: String::new(),
        }),
    }).unwrap();
    core.orchestrator_wakes.evaluate(CHAT, true).await;
    wait_for(|| harness.requests.lock().unwrap().iter().any(|request|
        request.prompt.contains("Unique final handback result")), "manager receives handback").await;
    let operation_id = mast_proto::agent_tools::operation_id(ARCHIVE_CHILD, "lost-reply");
    await_handback_receipt(&core, &_tmp.path().join("data"), &operation_id).await;
    let delivered: Value = rpc.call(mast_proto::agent_tools::TOOL_CALL_METHOD, retry.clone()).await.unwrap();
    assert_eq!(delivered["delivery"], "delivered");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    let profile = mast_engine::EngineProfile::development(
        &_tmp.path().join("data"),
        &std::env::var("MAST_ORG_ID").unwrap_or_else(|_| "dev-org".into()),
        &std::env::var("MAST_USER_ID").unwrap_or_else(|_| "dev-user".into()),
    );
    let journal = mast_engine::RunJournal::open(profile.store_root().join("journals")).unwrap();
    journal.note_interrupted(ARCHIVE_CHILD, mast_proto::InterruptedOutcome::Resumed, 1);
    // take_queued refuses gated rows by design; remove the review-gated row.
    assert!(worker.doc().remove_queued("pending-worker-message").unwrap());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    core.sessions.dismiss_interrupted(ARCHIVE_CHILD);
    wait_for(|| core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived, "automatic archive").await;
    let reply: Value = rpc.call(mast_proto::agent_tools::TOOL_CALL_METHOD, retry).await.unwrap();
    assert_eq!(reply["delivery"], "delivered");
    let messages = core.doc_host.open(CHAT).unwrap().doc().read_entries().unwrap();
    assert_eq!(messages.iter().flat_map(|entry| &entry.parts).filter(|part| {
        matches!(part, mast_doc::MessagePart::Text { text, .. }
            if text.contains("Unique final handback result"))
    }).count(), 1);

    // New work can arrive over the relay without a pending command in the
    // local doc. It restores a chat whose visible state was set by handback.
    let relay_result = core.doc_host.ingest_relayed_command(ARCHIVE_CHILD, mast_doc::SessionCommandEntry {
        id: "new-work-after-archive".into(),
        payload: run_command("A new request arrived from another device", &space_path),
        issued_by: "other-device".into(),
        issued_at: chrono::Utc::now().timestamp_millis(),
        based_on: None,
        expires_at: None,
        status: mast_doc::SessionCommandStatus::Pending,
        resolution: None,
    }).await.unwrap();
    assert_eq!(relay_result, "executed");
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    wait_for(|| harness.requests.lock().unwrap().iter().any(|request|
        request.prompt == "A new request arrived from another device"), "relayed worker turn").await;
    harness.release.notify_waiters();
    wait_for(|| !core.sessions.turn_in_flight(ARCHIVE_CHILD), "relayed worker turn end").await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_delivery_respects_keep_and_reassignment() {
    for keep in [false, true] {
        let a = archive_rig().await;
        let rig = &a.rig;
        assign_chat(rig, "handoff-original", ARCHIVE_CHILD, &a.p.ticket).await;
        rig.core.doc_host.open(ARCHIVE_CHILD).unwrap();
        let lease = rig.core.sessions.agent_tokens().mint(
            ARCHIVE_CHILD, "final-handback-guard", SPACE, &rig.core.device_id,
        );
        let current = tool(rig, lease.token(), "work_outline_read_current", json!({})).await.unwrap();
        rig.core.orchestrator_wakes.note_user_activity(CHAT, Some(true)).await;
        let args = json!({
            "idempotencyKey": "protected-worker", "expectedGeneration": current["assignmentGeneration"],
            "report": "Guarded final report", "keepChat": keep,
        });
        let reply = tool(rig, lease.token(), "final_handback", args.clone()).await.unwrap();
        assert_eq!(reply["delivery"], "queuedForParent");
        if !keep {
            assign_chat(rig, "handoff-reassigned", ARCHIVE_CHILD, &a.p.other_tickets).await;
        }
        rig.core.orchestrator_wakes.evaluate(CHAT, true).await;
        wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|request|
            request.prompt.contains("Guarded final report")), "protected worker report").await;
        let operation_id = mast_proto::agent_tools::operation_id(ARCHIVE_CHILD, "protected-worker");
        await_handback_receipt(&rig.core, &rig._tmp.path().join("data"), &operation_id).await;
        let reply = tool(rig, lease.token(), "final_handback", args).await.unwrap();
        assert_eq!(reply["delivery"], "delivered");
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!chat_archived(rig, ARCHIVE_CHILD));
        rig.harness.release.notify_one();
        rig.core.shutdown().await;
    }
}

#[cfg(debug_assertions)]
const HANDOFF_ORIGINAL: &str = "Unedited report for the Manager";
#[cfg(debug_assertions)]
const HANDOFF_REPLACEMENT: &str = "A different queued message";

#[cfg(debug_assertions)]
async fn queue_final_handback_for_test(rig: &Rig, key: &str) -> (Value, mast_doc::QueuedMessage) {
    let lease = rig.core.sessions.agent_tokens().mint(
        ARCHIVE_CHILD, "handback-queue-test", SPACE, &rig.core.device_id,
    );
    let current = tool(rig, lease.token(), "work_outline_read_current", json!({})).await.unwrap();
    let arguments = json!({
        "idempotencyKey": key,
        "expectedGeneration": current["assignmentGeneration"],
        "report": HANDOFF_ORIGINAL,
    });
    rig.core.orchestrator_wakes.note_user_activity(CHAT, Some(true)).await;
    tool(rig, lease.token(), "final_handback", arguments.clone()).await.unwrap();
    rig.core.orchestrator_wakes.evaluate(CHAT, true).await;
    let row = rig.core.doc_host.open(CHAT).unwrap().doc().read_queue().unwrap()
        .into_iter().find(|row| row.text.contains(HANDOFF_ORIGINAL)).unwrap();
    (arguments, row)
}

#[cfg(debug_assertions)]
async fn retry_final_handback(core: &EngineCore, arguments: Value) -> Result<Value, String> {
    let lease = core.sessions.agent_tokens().mint(
        ARCHIVE_CHILD, "handback-queue-retry", SPACE, &core.device_id,
    );
    mast_rpc::memory_client(core.rpc_service()).call(
        mast_proto::agent_tools::TOOL_CALL_METHOD,
        json!({ "token": lease.token(), "tool": "final_handback", "arguments": arguments }),
    ).await.map_err(|error| error.to_string())
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_edited_queue_row_cannot_prove_delivery() {
    let a = archive_rig().await;
    let rig = &a.rig;
    assign_chat(rig, "handoff-queue-edit", ARCHIVE_CHILD, &a.p.ticket).await;
    rig.core.doc_host.open(ARCHIVE_CHILD).unwrap();
    let parent = rig.core.doc_host.open(CHAT).unwrap();
    let guard = parent.test_queue_drain_guard().await;
    let (arguments, queued) = queue_final_handback_for_test(rig, "queue-edit").await;
    assert!(rig.core.doc_host.test_update_queued_message_locked(&parent, &guard, &queued.id, HANDOFF_REPLACEMENT).unwrap());
    drop(guard);
    wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|request|
        request.prompt == HANDOFF_REPLACEMENT), "edited row reaches the Manager").await;
    let operation_id = mast_proto::agent_tools::operation_id(ARCHIVE_CHILD, "queue-edit");
    let path = rig._tmp.path().join("data/orchestrator-wake-events.json");
    wait_for(|| {
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        saved["cancelled_handbacks"].as_array().is_some_and(|ids|
            ids.iter().any(|id| id == &operation_id))
    }, "edited wake is excluded from archive evidence").await;
    assert!(retry_final_handback(&rig.core, arguments).await.unwrap_err().contains("removed or edited"));
    assert!(!chat_archived(rig, ARCHIVE_CHILD));
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_restart_checks_transcript_before_disabling_or_discarding() {
    for edited in [false, true] {
        let a = archive_rig().await;
        let rig = a.rig;
        assign_chat(&rig, "handoff-disabled-recipient", ARCHIVE_CHILD, &a.p.ticket).await;
        let parent = rig.core.doc_host.open(CHAT).unwrap();
        let guard = parent.test_queue_drain_guard().await;
        let (arguments, queued) = queue_final_handback_for_test(&rig, "before-disabled").await;
        // Simulate stopping after the recipient transcript write and before
        // the wake service can persist its delivery acknowledgement.
        assert!(parent.doc().take_queued(&queued.id).unwrap().is_some());
        let text = if edited { HANDOFF_REPLACEMENT } else { &queued.text };
        parent.write_user_message(&queued.id, text, 1).unwrap();
        rig.core.doc_host.flush_all();
        let mut settings = rig.core.orchestrator_wakes.settings(CHAT);
        settings.enabled = false;
        rig.core.orchestrator_wakes.set_settings(CHAT, settings).await;
        drop(guard);
        rig.core.shutdown().await;
        let Rig { _tmp, core, client, service, .. } = rig;
        drop((core, client, service, parent));

        let harness = GatedHarness::default();
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(harness.clone()));
        let core = EngineCore::assemble(
            &_tmp.path().join("data"), Arc::new(registry), HarnessId::Mock, None,
        ).unwrap();
        let mut settings = core.orchestrator_wakes.settings(CHAT);
        settings.enabled = true;
        core.orchestrator_wakes.set_settings(CHAT, settings).await;
        let retry = retry_final_handback(&core, arguments).await;
        if edited {
            assert!(retry.unwrap_err().contains("removed or edited"));
            assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
        } else {
            assert_eq!(retry.unwrap()["delivery"], "delivered");
        }
        core.orchestrator_wakes.evaluate(CHAT, true).await;
        assert!(core.orchestrator_wakes.status(CHAT).held.is_empty());
        assert!(core.doc_host.open(CHAT).unwrap().doc().read_queue().unwrap().is_empty());
        assert!(harness.requests.lock().unwrap().is_empty());
        core.shutdown().await;
    }
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_explicit_queue_removal_survives_retry_and_restart() {
    let a = archive_rig().await;
    let rig = a.rig;
    assign_chat(&rig, "handoff-remove", ARCHIVE_CHILD, &a.p.ticket).await;
    let parent = rig.core.doc_host.open(CHAT).unwrap();
    let guard = parent.test_queue_drain_guard().await;
    let (arguments, queued) = queue_final_handback_for_test(&rig, "queue-remove").await;
    parent.doc().set_queued_delivery_gate(&queued.id, Some(&mast_doc::QueueDeliveryGate::ReviewRequired {
        previous_lease_id: "edit".into(),
        owner_device_id: rig.core.device_id.clone(),
        since_ms: 1,
        base_text_hash: String::new(),
    })).unwrap();
    drop(guard);
    assert!(rig.core.doc_host.remove_queued_message(CHAT, &queued.id).await.unwrap());
    assert!(retry_final_handback(&rig.core, arguments.clone()).await.unwrap_err()
        .contains("removed or edited"));
    assert!(!chat_archived(&rig, ARCHIVE_CHILD));
    rig.core.shutdown().await;
    let Rig { _tmp, core, client, service, .. } = rig;
    drop((core, client, service, parent));

    let registry = HarnessRegistry::new();
    registry.register(Arc::new(GatedHarness::default()));
    let core = EngineCore::assemble(
        &_tmp.path().join("data"), Arc::new(registry), HarnessId::Mock, None,
    ).unwrap();
    assert!(retry_final_handback(&core, arguments).await.unwrap_err().contains("removed or edited"));
    assert!(core.orchestrator_wakes.status(CHAT).held.is_empty());
    assert!(core.doc_host.open(CHAT).unwrap().doc().read_queue().unwrap().is_empty());
    assert!(!core.workspace.chat(ARCHIVE_CHILD).unwrap().unwrap().archived);
    core.shutdown().await;
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_handback_worker_reports_toggle_retracts_queued_report() {
    for manager_pulls_report in [false, true] {
        let a = archive_rig().await;
        let rig = a.rig;
        assign_chat(&rig, "handoff-toggle-reports", ARCHIVE_CHILD, &a.p.ticket).await;
        let parent = rig.core.doc_host.open(CHAT).unwrap();
        let guard = parent.test_queue_drain_guard().await;
        let (arguments, queued) = queue_final_handback_for_test(&rig, "toggle-reports").await;
        let held_id = if manager_pulls_report {
            // The real Manager send path pulls back a pending wake without
            // waiting for the drain. Keep the new user message from dispatching
            // while the Worker Reports setting is changed.
            let id = rig.core.doc_host.queue_message(CHAT, "Manager sends input first", vec![]).unwrap();
            assert!(!parent.doc().read_queue().unwrap().iter().any(|row| row.id == queued.id));
            id
        } else {
            queued.id
        };
        parent.doc().set_queued_delivery_gate(&held_id, Some(&mast_doc::QueueDeliveryGate::ReviewRequired {
            previous_lease_id: "held".into(),
            owner_device_id: rig.core.device_id.clone(),
            since_ms: 1,
            base_text_hash: String::new(),
        })).unwrap();
        drop(guard);
        let mut settings = rig.core.orchestrator_wakes.settings(CHAT);
        settings.worker_reports = false;
        rig.core.orchestrator_wakes.set_settings(CHAT, settings).await;
        if manager_pulls_report {
            assert!(rig.core.doc_host.remove_queued_message(CHAT, &held_id).await.unwrap());
        }
        assert!(parent.doc().read_queue().unwrap().is_empty(), "pulls={manager_pulls_report} queue={:?}", parent.doc().read_queue().unwrap().iter().map(|r| (r.id.clone(), r.text.clone())).collect::<Vec<_>>());
        assert!(rig.harness.requests.lock().unwrap().is_empty());
        assert!(!chat_archived(&rig, ARCHIVE_CHILD));
        assert!(retry_final_handback(&rig.core, arguments.clone()).await.unwrap_err().contains("reports are disabled"));

        settings.worker_reports = true;
        rig.core.orchestrator_wakes.set_settings(CHAT, settings).await;
        rig.core.orchestrator_wakes.evaluate(CHAT, true).await;
        wait_for(|| rig.harness.requests.lock().unwrap().iter().any(|request|
            request.prompt.contains(HANDOFF_ORIGINAL)), "re-enabled final report").await;
        let op = mast_proto::agent_tools::operation_id(ARCHIVE_CHILD, "toggle-reports");
        await_handback_receipt(&rig.core, &rig._tmp.path().join("data"), &op).await;
        assert_eq!(retry_final_handback(&rig.core, arguments).await.unwrap()["delivery"], "delivered");
        assert_eq!(parent.doc().read_entries().unwrap().iter().filter(|entry|
            entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. }
                if text.contains(HANDOFF_ORIGINAL)))).count(), 1);
        rig.harness.release.notify_one();
        rig.core.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn archive_info_rates_children_and_project_workers_only() {
    let a = archive_rig().await;
    let info = tool(&a.rig, &a.orchestrator, "chat_archive_info", json!({}))
        .await
        .unwrap();
    let chats = info["chats"].as_object().unwrap();
    assert!(chats.contains_key(OTHER_CHAT) && chats.contains_key(ARCHIVE_CHILD), "{chats:?}");
    // Not itself, nor a chat in another project.
    assert!(!chats.contains_key(CHAT) && !chats.contains_key(ARCHIVE_OUTSIDER), "{chats:?}");
    // The ticket is open: not finished yet.
    assert_eq!(chats[OTHER_CHAT]["archivable"], false);
    update_item(&a.rig, "ar-done", &a.p.ticket, json!({ "status": "completed" }))
        .await
        .unwrap();
    let info = tool(&a.rig, &a.orchestrator, "chat_archive_info", json!({ "chatIds": [OTHER_CHAT] }))
        .await
        .unwrap();
    assert_eq!(info["chats"][OTHER_CHAT]["archivable"], true);
    assert_eq!(info["chats"][OTHER_CHAT]["reason"], "its item is completed");
    assert_eq!(info["chats"].as_object().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unfinished_worker_is_refused_unless_forced_with_a_reason() {
    let a = archive_rig().await;
    let err = tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": OTHER_CHAT }))
        .await
        .unwrap_err();
    assert!(err.contains("not archived") && err.contains("force"), "{err}");
    assert!(!chat_archived(&a.rig, OTHER_CHAT));
    let err = tool(
        &a.rig,
        &a.orchestrator,
        "chat_archive",
        json!({ "chatId": OTHER_CHAT, "force": true }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("short reason"), "{err}");
    assert!(!chat_archived(&a.rig, OTHER_CHAT));
    let done = tool(
        &a.rig,
        &a.orchestrator,
        "chat_archive",
        json!({ "chatId": OTHER_CHAT, "force": true, "reason": "superseded by ENG-9" }),
    )
    .await
    .unwrap();
    assert_eq!((done["archived"].clone(), done["forced"].clone()), (json!(true), json!(true)));
    assert!(chat_archived(&a.rig, OTHER_CHAT));
    // Logged, attributed to the orchestrator, with the reason.
    let log = a.rig.service.agent_tools().agent_archives_since(0);
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].chat_id.as_str(), log[0].orchestrator_chat_id.as_str(), log[0].reason.as_str(), log[0].forced),
        (OTHER_CHAT, CHAT, "superseded by ENG-9", true)
    );
    // Reversible: unarchive needs neither the rule nor a reason.
    tool(
        &a.rig,
        &a.orchestrator,
        "chat_archive",
        json!({ "chatId": OTHER_CHAT, "archived": false }),
    )
    .await
    .unwrap();
    assert!(!chat_archived(&a.rig, OTHER_CHAT));
    assert_eq!(a.rig.service.agent_tools().agent_archives_since(0).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_worker_archives_without_force_and_is_logged() {
    let a = archive_rig().await;
    update_item(&a.rig, "ar-done", &a.p.ticket, json!({ "status": "completed" }))
        .await
        .unwrap();
    let done = tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": OTHER_CHAT }))
        .await
        .unwrap();
    assert_eq!(done["forced"], false);
    assert_eq!(done["reason"], "its item is completed");
    assert!(chat_archived(&a.rig, OTHER_CHAT));
    let log = a.rig.service.agent_tools().agent_archives_since(0);
    assert_eq!(log.len(), 1);
    assert!(!log[0].forced);
    assert_eq!(log[0].space_id, SPACE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_with_an_open_required_action_is_never_finished() {
    let a = archive_rig().await;
    update_item(&a.rig, "ar-done", &a.p.ticket, json!({ "status": "completed" }))
        .await
        .unwrap();
    // The worker raised a question that nobody answered.
    let worker = a.rig.core.sessions.agent_tokens().mint(OTHER_CHAT, "run-w", SPACE, &a.rig.core.device_id);
    tool(
        &a.rig,
        worker.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Which colour?", "dedupeKey": "colour" }),
    )
    .await
    .unwrap();
    let err = tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": OTHER_CHAT }))
        .await
        .unwrap_err();
    assert!(err.contains("open Required Action"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_reaches_children_and_never_the_orchestrator_or_other_projects() {
    let a = archive_rig().await;
    // A child, as today (no finished rule for the caller's own chats).
    tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": ARCHIVE_CHILD }))
        .await
        .unwrap();
    assert!(chat_archived(&a.rig, ARCHIVE_CHILD));
    for target in [ARCHIVE_OUTSIDER, CHAT, PROJECTLESS] {
        let err = tool(
            &a.rig,
            &a.orchestrator,
            "chat_archive",
            json!({ "chatId": target, "force": true, "reason": "try" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("forbidden") || err.contains("no such chat"), "{target}: {err}");
        assert!(!chat_archived(&a.rig, target), "{target}");
    }
    // The outsider's own orchestrator-less token reaches nothing of ours.
    let outsider = a.rig.core.sessions.agent_tokens().mint(ARCHIVE_OUTSIDER, "run-o", SPACE, &a.rig.core.device_id);
    let err = tool(
        &a.rig,
        outsider.token(),
        "chat_archive",
        json!({ "chatId": OTHER_CHAT, "force": true, "reason": "try" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("forbidden"), "{err}");
    assert!(!chat_archived(&a.rig, OTHER_CHAT));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_chat_archives_any_chat_of_its_space_without_the_rule_and_it_is_logged() {
    let a = archive_rig().await;
    grant(&a.rig, CHAT, Some(3600), false).await;
    // An unfinished worker and a chat in another project: no rule, no force.
    for target in [OTHER_CHAT, ARCHIVE_OUTSIDER] {
        tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": target }))
            .await
            .unwrap();
        assert!(chat_archived(&a.rig, target), "{target}");
    }
    let log = a.rig.service.agent_tools().agent_archives_since(0);
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|e| e.orchestrator_chat_id == CHAT && !e.forced));
    // Still never itself, nor a projectless chat of another Space.
    for target in [CHAT, PROJECTLESS] {
        assert!(tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": target }))
            .await
            .is_err());
        assert!(!chat_archived(&a.rig, target), "{target}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_personal_assistant_archives_chats_of_every_space_and_it_is_logged() {
    const ELSEWHERE: &str = "chat-elsewhere";
    let a = archive_rig().await;
    let path = std::path::Path::new(&a.rig.space_path).join("elsewhere");
    std::fs::create_dir_all(&path).unwrap();
    for params in [
        json!({ "op": "createSpace", "spaceId": OTHER_SPACE, "deviceId": a.rig.core.device_id,
                "path": path.to_string_lossy() }),
        json!({ "op": "createChat", "chatId": ELSEWHERE, "spaceId": OTHER_SPACE,
                "deviceId": a.rig.core.device_id }),
    ] {
        a.rig.client.call(methods::MUTATE, params).await.unwrap();
    }
    a.rig
        .client
        .call(methods::SET_PERSONAL_ASSISTANT, json!({ "chatId": CHAT }))
        .await
        .unwrap();
    let origin = a
        .rig
        .client
        .call(mast_proto::agent_tools::AUTHENTICATE_METHOD, json!({ "token": a.orchestrator }))
        .await
        .unwrap();
    assert_eq!(origin["personalAssistant"], true);
    // An unfinished worker, another Space, no Space: no rule, no force.
    for target in [OTHER_CHAT, ELSEWHERE, PROJECTLESS] {
        tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": target }))
            .await
            .unwrap();
        assert!(chat_archived(&a.rig, target), "{target}");
    }
    let log = a.rig.service.agent_tools().agent_archives_since(0);
    assert!(log.iter().all(|e| !e.forced && e.reason == "personal assistant"), "{log:?}");
    assert!(log.iter().any(|e| e.chat_id == ELSEWHERE && e.space_id == OTHER_SPACE));
    // Never itself.
    assert!(tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": CHAT }))
        .await
        .is_err());
    // Cleared: the next call is back to the ordinary reach.
    a.rig
        .client
        .call(methods::CLEAR_PERSONAL_ASSISTANT, json!({}))
        .await
        .unwrap();
    tool(&a.rig, &a.orchestrator, "chat_archive", json!({ "chatId": ELSEWHERE, "archived": false }))
        .await
        .unwrap_err();
}

#[tokio::test(flavor = "multi_thread")]
async fn bulk_change_rejects_non_approval_resolutions() {
    for outcome in [
        None,
        Some("dismissed"),
        Some("declined"),
        Some("changesRequested"),
        Some("answered"),
    ] {
        let rig = rig().await;
        let item = create_item(&rig, "admin-item", "Admin", None).await;
        let mut ids = Vec::new();
        for n in 0..11 {
            ids.push(create_item(&rig, &format!("item-{n}"), &format!("Item {n}"), None).await);
        }
        assign_chat(&rig, "assign-admin", CHAT, &item).await;
        let tokens = rig.core.sessions.agent_tokens();
        let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
        let token = lease.token().to_owned();
        grant(&rig, CHAT, Some(36000), false).await;
        let steps: Vec<Value> = ids
            .iter()
            .map(|id| json!({"op":"rename", "itemId":id, "title":"Changed"}))
            .collect();
        let preview = admin(
            &rig,
            &token,
            json!({"idempotencyKey":"preview", "steps":steps}),
        )
        .await
        .unwrap();
        let request_id = preview["requestId"].as_str().unwrap();
        rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"resolveUserActionRequest", "userActionRequestId":request_id, "outcome":outcome, "note":"no", "via":"card", "operationId":"user-declines"})).await.unwrap();
        let err = admin(&rig, &token, json!({"idempotencyKey":"preview", "steps":steps, "confirmToken":preview["confirmToken"]})).await.unwrap_err();
        assert!(err.contains("not approved"), "{outcome:?}: {err}");
        assert!(ids.iter().all(|id| title_of(&rig, id) != "Changed"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_request_outcomes_are_readable_by_request_and_current() {
    let rig = rig().await;
    let item = create_item(&rig, "outcome-item", "Outcomes", None).await;
    assign_chat(&rig, "outcome-assign", CHAT, &item).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token();
    for (index, (kind, outcome)) in [
        ("approve", "approved"),
        ("approve", "declined"),
        ("review", "approved"),
        ("review", "changesRequested"),
        ("handoff", "approved"),
        ("handoff", "changesRequested"),
        ("ask", "answered"),
        ("choose", "chosen"),
        ("notice", "dismissed"),
    ]
    .iter()
    .enumerate()
    {
        let raised = tool(&rig, token, "work_outline_create_user_action_request", json!({
            "request": format!("Request {index}"), "kind": kind, "idempotencyKey": format!("create-{index}")
        })).await.unwrap();
        let id = raised["userActionRequestId"].as_str().unwrap();
        rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"resolveUserActionRequest", "userActionRequestId":id,
            "outcome":outcome, "values":{"choice":"a"}, "via":"card", "operationId":format!("resolve-{index}")})).await.unwrap();
        let result = tool(
            &rig,
            token,
            "get_user_action_request",
            json!({"requestId":id}),
        )
        .await
        .unwrap();
        assert_eq!(result["request"]["kind"], *kind);
        assert_eq!(result["request"]["resolution"]["outcome"], *outcome);
        assert_eq!(result["request"]["resolution"]["values"]["choice"], "a");
    }
    let current = tool(&rig, token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(current["userActionRequests"].as_array().unwrap().len(), 9);
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_provenance_deduplication_and_filters() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();
    let first = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let second = tokens.mint(OTHER_CHAT, "run-2", SPACE, &rig.core.device_id);
    rig.core.workspace.set_chat_actual_model(CHAT, "model-actual");
    let input = json!({"kind":"bug", "summary":"Tool failed", "whatHappened":"It failed"});
    let row = tool(&rig, first.token(), "report_friction", input.clone()).await.unwrap();
    assert_eq!(row["report"]["provenance"]["chatId"], CHAT);
    assert_eq!(row["report"]["provenance"]["spaceId"], SPACE);
    assert_eq!(row["report"]["provenance"]["source"], "agent");
    assert_eq!(row["report"]["provenance"]["model"], "model-actual");
    let next = tool(&rig, second.token(), "report_friction", input).await.unwrap();
    assert_eq!(next["report"]["id"], row["report"]["id"]);
    assert_eq!(next["report"]["occurrences"], 2);
    let list = tool(&rig, first.token(), "list_friction", json!({"kind":"bug", "status":"open", "since":0, "limit":1})).await.unwrap();
    assert_eq!(list["reports"].as_array().unwrap().len(), 1);
    assert_eq!(list["reports"][0]["occurrences"], 2);
    let long = "x".repeat(1000);
    tool(&rig, first.token(), "report_friction", json!({"kind":"docs", "summary":"Long", "whatHappened":long})).await.unwrap();
    let trimmed = tool(&rig, first.token(), "list_friction", json!({"kind":"docs"})).await.unwrap();
    assert_eq!(trimmed["reports"][0]["whatHappened"].as_str().unwrap().len(), 300);
    assert_eq!(trimmed["reports"][0]["whatHappenedTruncated"], true);
    let full = tool(&rig, first.token(), "list_friction", json!({"kind":"docs", "full":true})).await.unwrap();
    assert_eq!(full["reports"][0]["whatHappened"].as_str().unwrap().len(), 1000);
    for filter in [json!({"kind":"tool_failure"}), json!({"status":"dismissed"}), json!({"since":i64::MAX})] {
        let list = tool(&rig, first.token(), "list_friction", filter).await.unwrap();
        assert!(list["reports"].as_array().unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_agent_reports_redact_secrets_before_storage() {
    let rig = rig().await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "friction-redaction", SPACE, &rig.core.device_id);
    let value = tool(&rig, lease.token(), "report_friction", json!({
        "kind":"bug", "summary":"ghp_fake_summary", "whatHappened":"ghp_fake_detail",
        "workaround":"ghp_fake_workaround", "suggestion":"ghp_fake_suggestion",
        "approval":{"command":"ghp_fake_command", "why":"ghp_fake_reason"}
    })).await.unwrap();
    let stored = rig.core.workspace.friction_reports(SPACE).pop().unwrap();
    let stored = serde_json::to_string(&stored).unwrap();
    assert!(!stored.contains("ghp_fake"), "{stored}");
    assert!(!value.to_string().contains("ghp_fake"));
    assert_eq!(stored.matches("[REDACTED]").count(), 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_all_spaces_requires_grant_and_filters_foreign_reports() {
    let rig = rig().await;
    rig.client.call(methods::MUTATE, json!({"op":"createSpace", "spaceId":OTHER_SPACE, "deviceId":rig.core.device_id, "path":"/tmp/other-space"})).await.unwrap();
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "friction-scope", SPACE, &rig.core.device_id);
    let token = lease.token();
    let local = tool(&rig, token, "report_friction", json!({"kind":"environment", "summary":"Local", "whatHappened":"Local failure"})).await.unwrap();
    let mut foreign: mast_proto::FrictionReport = serde_json::from_value(local["report"].clone()).unwrap();
    foreign.id.clear(); foreign.fingerprint.clear(); foreign.provenance.space_id = OTHER_SPACE.into();
    foreign.kind = mast_proto::FrictionKind::ToolFailure;
    foreign.provenance.source = mast_proto::FrictionSource::Auto;
    foreign.summary = "review_publish refused: failed ghp_fake_summary".into();
    foreign.what_happened = "Legacy report ghp_fake_detail".into();
    foreign.approval = Some(mast_proto::FrictionApproval { command:"private command".into(), why:"private".into(), answer:None });
    foreign.links.push(mast_proto::RequestLink { label:"private".into(), target:mast_proto::RequestLinkTarget::Chat { id:"private-chat".into() } });
    foreign.provenance.work_item_id = Some("private-item".into());
    foreign.provenance.branch = Some("private-branch".into());
    let included = rig.core.workspace.record_friction(foreign.clone()).unwrap();
    foreign.id.clear(); foreign.fingerprint.clear(); foreign.kind = mast_proto::FrictionKind::Environment;
    foreign.summary = "Other environment".into();
    rig.core.workspace.record_friction(foreign).unwrap();
    let default_list = tool(&rig, token, "list_friction", json!({})).await.unwrap();
    assert_eq!(default_list["reports"].as_array().unwrap().len(), 1);
    let space_list = tool(&rig, token, "list_friction", json!({"scope":"space"})).await.unwrap();
    assert_eq!(space_list, default_list);
    let error = tool(&rig, token, "list_friction", json!({"scope":"allSpaces"})).await.unwrap_err();
    assert!(error.contains("Read Mast friction from all Spaces"), "{error}");
    let mut settings = rig.core.registry.friction_settings();
    settings.all_spaces_read_grants.insert(SPACE.into());
    rig.core.registry.set_friction_settings(settings).unwrap();
    let list = tool(&rig, token, "list_friction", json!({"scope":"allSpaces"})).await.unwrap();
    let reports = list["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    let found = reports.iter().find(|row| row["id"] == included.id).unwrap();
    assert_eq!(found["spaceId"], OTHER_SPACE);
    assert_eq!(found["spaceName"], "other-space");
    assert_eq!(found["summary"], "review_publish refused: failed [REDACTED]");
    assert_eq!(found["whatHappened"], "Legacy report [REDACTED]");
    let rpc_rows = rig.client.call(methods::LIST_FRICTION_REPORTS, json!({"spaceId":SPACE,"allSpaces":true})).await.unwrap();
    let rpc_foreign = rpc_rows.as_array().unwrap().iter().find(|row| row["id"] == included.id).unwrap();
    assert_eq!(rpc_foreign["summary"], "review_publish refused: failed [REDACTED]");
    assert_eq!(rpc_foreign["whatHappened"], "Legacy report [REDACTED]");
    for key in ["approval", "links", "provenance", "chatId", "workItemId", "branch"] { assert!(found.get(key).is_none(), "{key} leaked"); }
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":"scope-child", "spaceId":SPACE, "deviceId":rig.core.device_id, "parentChatId":CHAT})).await.unwrap();
    let error = tool(&rig, token, "dismiss_friction", json!({"id":included.id,"reason":"Test"})).await.unwrap_err();
    assert!(error.contains("another Space"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_captures_live_run_harness() {
    let rig = rig().await;
    rig.core.doc_host.queue_command(CHAT, run_command("test", &rig.space_path)).unwrap();
    wait_for(|| !rig.harness.spawns.lock().unwrap().is_empty(), "live run").await;
    let Spawn::WithTools { tools, .. } = rig.harness.spawns.lock().unwrap()[0].clone() else { panic!("missing tools") };
    let row = tool(&rig, &tools.env["MAST_AGENT_TOKEN"], "report_friction", json!({"kind":"bug", "summary":"Failure", "whatHappened":"Failed"})).await.unwrap();
    assert_eq!(row["report"]["provenance"]["harness"], "mock");
    rig.harness.release.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_rule_proposal_only_creates_a_review_request() {
    let rig = rig().await;
    let root = create_item(&rig, "proposal-root", "Root", None).await;
    let tokens = rig.core.sessions.agent_tokens();
    let token = tokens.mint(CHAT, "proposal-run", SPACE, &rig.core.device_id);
    tool(&rig, token.token(), "work_outline_associate_current_chat", json!({"workItemId": root, "idempotencyKey": "proposal-associate"})).await.unwrap();
    let result = tool(&rig, token.token(), "propose_approval_rule", json!({
        "label": "Rust tests", "argvPrefix": ["cargo", "test"], "harness": "codex", "idempotencyKey": "proposal-rule"
    })).await.unwrap();
    assert!(result["userActionRequestId"].is_string());
    let first = snapshot(&rig);
    assert!(first.space_settings.approval_rules.is_empty());
    assert!(first.user_action_requests.iter().any(|request| request.text.contains("Rust tests")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::path::Path::new(&rig.space_path).join("scripts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(tool(&rig, token.token(), "propose_approval_rule", json!({
        "label": "Alternation", "argvPrefix": ["scripts/run.sh"], "harness": "codex",
        "cwdScope": rig.space_path, "commandRegex": "^a|.*$",
        "repositoryExecutable": "scripts/run.sh", "idempotencyKey": "proposal-alternation"
    })).await.is_err());
    let script = tool(&rig, token.token(), "propose_approval_rule", json!({
        "label": "Release script", "argvPrefix": ["scripts/run.sh"], "harness": "codex",
        "cwdScope": rig.space_path, "commandRegex": "^scripts/run\\.sh [0-9a-f]+$",
        "repositoryExecutable": "scripts/run.sh", "idempotencyKey": "proposal-script"
    })).await.unwrap();
    assert!(script["userActionRequestId"].is_string());
    let proposed = snapshot(&rig).user_action_requests.into_iter().find(|request| request.text.contains("Release script")).unwrap();
    let info = proposed.info.unwrap();
    assert!(info.contains("Pinned contents: sha256 ") && info.contains("\"sha256\"") && info.contains("#!/bin/sh"), "{info}");
    assert!(tool(&rig, token.token(), "propose_approval_rule", json!({
        "label": "Forbidden", "argvPrefix": ["cargo", "test"],
        "environment": ["CARGO_INCREMENTAL=0"], "idempotencyKey": "proposal-env"
    })).await.is_err());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_captures_tool_failures_and_can_be_disabled() {
    let rig = rig().await;
    rig.core.doc_host.queue_command(CHAT, run_command("test", &rig.space_path)).unwrap();
    wait_for(|| !rig.harness.spawns.lock().unwrap().is_empty(), "run").await;
    let Spawn::WithTools { tools, .. } = rig.harness.spawns.lock().unwrap()[0].clone() else { panic!("missing tools") };
    let token = &tools.env["MAST_AGENT_TOKEN"];
    for _ in 0..2 {
        assert!(tool(&rig, token, "missing_tool", json!({})).await.is_err());
    }
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].occurrences, 2);
    assert_eq!(rows[0].provenance.source, mast_proto::FrictionSource::Auto);
    assert!(tool(&rig, token, "token=fake-credential", json!({})).await.is_err());
    let rows = rig.core.workspace.friction_reports(SPACE);
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(!serialized.contains("fake-credential"));
    assert!(serialized.contains("[REDACTED]"));
    rig.core.sessions.set_auto_capture_friction(false);
    assert!(tool(&rig, token, "another_missing_tool", json!({})).await.is_err());
    assert_eq!(rig.core.workspace.friction_reports(SPACE).len(), 2);
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_captures_approval_and_answer() {
    let rig = rig().await;
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion { permission: None, command_approval: None, allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Codex wants to run `echo token=fake-secret`. Allow it? Reason: sandbox".into(), options: vec!["Yes".into(), "No".into()], multi_select: false }];
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(CHAT, run_command("test", &rig.space_path)).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { input, .. } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() { break input; }
    };
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows.len(), 1);
    let approval = rows[0].approval.as_ref().unwrap();
    assert_eq!(approval.command, "echo token=[REDACTED]");
    assert_eq!(approval.why, "sandbox");
    assert_eq!(approval.answer, None);
    assert!(rig.core.sessions.respond_input(CHAT, &input.request_id, vec![mast_proto::UserInputAnswer { question_id: "approval".into(), labels: vec!["No".into()] }]).unwrap());
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows[0].approval.as_ref().unwrap().answer, Some(false));
    assert_eq!(rows[0].occurrences, 1);
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn command_rule_answers_fake_codex_without_worker_wake() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let rule = ApprovalRule {
        id: "rule-1".into(), label: "Tests".into(), created_by: "user".into(),
        harness: ApprovalRuleHarness::Codex, enabled: true, argv_prefix: vec!["cargo".into(), "test".into()],
        command_regex: None, cwd_scope: None, require_isolated_home: false, constraints: None, hit_count: 0, last_used_at: None,
    };
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: "approval-rule".into(),
    edit_environment: false, }), &WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None }, &[]).unwrap();
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion { permission: None,
        command_approval: Some(mast_proto::CommandApproval { command: json!(["cargo", "test", "--offline"]), cwd: Some(rig.space_path.clone()), reason: Some("sandbox".into()), sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Codex wants to run `cargo test --offline`. Allow it? Reason: sandbox".into(),
        options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    rig.core.sessions.set_auto_capture_friction(false);
    let mut signals = rig.core.sessions.watch_worker_signals();
    let mut command = run_command("test", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut command { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, command).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "automatic answer").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    assert!(signals.try_recv().is_err());
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].summary.contains("Auto-approved by rule rule-1"));
    assert_eq!(rows[0].approval.as_ref().unwrap().answer, Some(true));
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    let snapshot = mast_doc::materialize_work_outline(SPACE, &inputs);
    assert_eq!(snapshot.space_settings.approval_rules[0].hit_count, 1);
    assert!(snapshot.space_settings.approval_rules[0].last_used_at.is_some());
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn command_approval_audits_each_restart_handoff_rule() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    for (id, prefix) in [
        ("df-rule", vec!["df", "-Pk", "."]),
        ("git-rule", vec!["git", "worktree", "list"]),
        ("ps-rule", vec!["ps", "-axo", "pid,command"]),
    ] {
        let rule = ApprovalRule {
            id: id.into(), label: id.into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
            enabled: true, argv_prefix: prefix.into_iter().map(str::to_owned).collect(), command_regex: None,
            cwd_scope: Some(rig.space_path.clone()), require_isolated_home: false, constraints: None,
            hit_count: 0, last_used_at: None,
        };
        rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
            space_id: SPACE.into(), rule, operation_id: format!("save-{id}"), edit_environment: false,
        }), &actor, &[]).unwrap();
    }
    let command = "df -Pk .\ngit worktree list | rg 'approval-matcher|worker-a|worker-b|handoff|rolling'\nps -axo pid,command | rg 'cargo-nextest|cargo test' | cut -c1-200 | head -n 8";
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!(["/bin/zsh", "-lc", command]), cwd: Some(rig.space_path.clone()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve restart checks".into(),
        options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("restart checks", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, run).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "restart rule answer").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    let reports = rig.core.workspace.friction_reports(SPACE);
    let summary = &reports[0].summary;
    for id in ["df-rule", "git-rule", "ps-rule"] { assert!(summary.contains(id), "{summary}"); }
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_rule_approves_exact_inventory_preview_through_codex() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let rule = ApprovalRule {
        id: "inventory-preview".into(), label: "Inventory preview".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["scripts/ui-preview.sh".into(), "--screens".into(), "details".into(), "--styles".into(), "default,industrial".into(), "--layouts".into(), "minimal".into()],
        command_regex: None, cwd_scope: Some(rig.space_path.clone()), require_isolated_home: false,
        constraints: Some(ApprovalRuleConstraints { environment: Some(vec!["CARGO_TARGET_DIR=/tmp/mast-target-b".into(), "CARGO_INCREMENTAL=0".into(), "MAST_PREVIEW_QUEUE=project".into()]), repository_executable: None }),
        hit_count: 0, last_used_at: None,
    };
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: "save-inventory-preview".into(), edit_environment: true,
    }), &actor, &[]).unwrap();
    let command = "CARGO_TARGET_DIR=/tmp/mast-target-b CARGO_INCREMENTAL=0 MAST_PREVIEW_QUEUE=project scripts/ui-preview.sh --screens details --styles default,industrial --layouts minimal > /tmp/project-doc-b-preview.log 2>&1";
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!(["/bin/zsh", "-lc", command]), cwd: Some(rig.space_path.clone()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve inventory preview".into(), options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("preview", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, run).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "preview approval").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    assert!(rig.core.workspace.friction_reports(SPACE)[0].summary.contains("inventory-preview"));
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn configured_script_digest_must_match_the_file_and_an_editor_save_repins() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, RepositoryApprovalExecutable, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    use std::os::unix::fs::PermissionsExt;
    let rig = rig_for(true).await;
    let scripts = std::path::Path::new(&rig.space_path).join("scripts");
    std::fs::create_dir(&scripts).unwrap();
    let path = scripts.join("pinned.sh");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let rule = |digest: Option<String>| ApprovalRule {
        id: "pinned".into(), label: "Pinned".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["scripts/pinned.sh".into()], command_regex: Some("^scripts/pinned\\.sh$".into()),
        cwd_scope: Some(rig.space_path.clone()), require_isolated_home: false,
        constraints: Some(ApprovalRuleConstraints { environment: Some(vec![]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/pinned.sh".into(), sha256: digest }) }),
        hit_count: 0, last_used_at: None,
    };
    let save = |rule: ApprovalRule, id: &str| rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: id.into(), edit_environment: true,
    }), &actor, &[]);
    let digest_of = |rig: &Rig| snapshot(rig).space_settings.approval_rules.into_iter().find(|rule| rule.id == "pinned").unwrap()
        .constraints.unwrap().repository_executable.unwrap().sha256.unwrap();
    // A digest the client sends for a new rule must be the file's current digest.
    assert!(save(rule(Some("0".repeat(64))), "pin-wrong").is_err());
    let first = crate_digest(&path);
    save(rule(Some(first.clone())), "pin-proposal").unwrap();
    assert_eq!(digest_of(&rig), first);
    // A client-sent digest that differs from the stored one is rejected.
    std::fs::write(&path, b"#!/bin/sh\nexit 1\n").unwrap();
    assert!(save(rule(Some("1".repeat(64))), "pin-differs").is_err());
    assert_eq!(digest_of(&rig), first);
    // An explicit editor save (no digest) pins the current contents.
    save(rule(None), "pin-editor-save").unwrap();
    assert_ne!(digest_of(&rig), first);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_does_not_host_the_space_cannot_pin_a_script() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, RepositoryApprovalExecutable, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let remote = std::path::Path::new(&rig.space_path).join("remote-checkout");
    std::fs::create_dir_all(remote.join("scripts")).unwrap();
    rig.core.workspace.create_space("remote-space", "dev-remote", remote.to_str().unwrap(), None, true).unwrap();
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let rule = ApprovalRule {
        id: "remote-script".into(), label: "Remote".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["scripts/run.sh".into()], command_regex: Some("^scripts/run\\.sh$".into()),
        cwd_scope: Some(remote.to_str().unwrap().into()), require_isolated_home: false,
        constraints: Some(ApprovalRuleConstraints { environment: Some(vec![]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/run.sh".into(), sha256: None }) }),
        hit_count: 0, last_used_at: None,
    };
    let error = rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: "remote-space".into(), rule, operation_id: "remote-pin".into(), edit_environment: true,
    }), &actor, &[]).unwrap_err();
    assert!(error.message.contains("Approve a repository executable on"), "{}", error.message);
}

#[cfg(unix)]
fn crate_digest(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn configured_script_is_fingerprinted_and_approved_through_codex() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, RepositoryApprovalExecutable, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    use std::os::unix::fs::PermissionsExt;
    let rig = rig_for(true).await;
    let scripts = std::path::Path::new(&rig.space_path).join("scripts");
    std::fs::create_dir(&scripts).unwrap();
    let path = scripts.join("run_release_tests.sh");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let rule = ApprovalRule {
        id: "release-script".into(), label: "Release script".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["scripts/run_release_tests.sh".into()],
        command_regex: Some(r"^scripts/run_release_tests\.sh [0-9a-f]{40}$".into()), cwd_scope: Some(rig.space_path.clone()), require_isolated_home: false,
        constraints: Some(ApprovalRuleConstraints { environment: Some(vec!["CARGO_INCREMENTAL=0".into(), "PYTHONDONTWRITEBYTECODE=1".into()]), repository_executable: Some(RepositoryApprovalExecutable { path: "scripts/run_release_tests.sh".into(), sha256: None }) }), hit_count: 0, last_used_at: None,
    };
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: "save-release-script".into(), edit_environment: true,
    }), &actor, &[]).unwrap();
    let stored = snapshot(&rig).space_settings.approval_rules.into_iter().find(|rule| rule.id == "release-script").unwrap();
    let fingerprint = stored.constraints.as_ref().unwrap().repository_executable.as_ref().unwrap().sha256.clone();
    assert!(fingerprint.is_some());
    std::fs::write(&path, b"#!/bin/sh\nexit 1\n").unwrap();
    let mut unrelated_edit = stored.clone();
    unrelated_edit.label = "Release script renamed".into();
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule: unrelated_edit, operation_id: "rename-release-script".into(), edit_environment: false,
    }), &actor, &[]).unwrap();
    let after = snapshot(&rig).space_settings.approval_rules.into_iter().find(|rule| rule.id == "release-script").unwrap();
    assert_eq!(after.constraints.unwrap().repository_executable.unwrap().sha256, fingerprint);
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!("CARGO_INCREMENTAL=0 PYTHONDONTWRITEBYTECODE=1 scripts/run_release_tests.sh a8f9da05ec64ff4636e33199bf66a7776dc233a0 > /private/tmp/mast-release-test.log 2>&1"), cwd: Some(rig.space_path.clone()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve release tests".into(), options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("release", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, run).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "script approval").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    assert!(rig.core.workspace.friction_reports(SPACE)[0].summary.contains("release-script"));
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn external_linked_worktree_requires_selected_checkout_and_scoped_rule() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let git = |args: &[&str]| {
        let output = mast_harness::git_command().args(args).current_dir(&rig.space_path).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    git(&["init"]);
    git(&["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "--allow-empty", "-m", "base"]);
    let external = rig._tmp.path().join("linked-worktree");
    git(&["worktree", "add", "-b", "approval-test", external.to_str().unwrap()]);
    let external = external.canonicalize().unwrap();
    rig.core.workspace.set_chat_cwd(CHAT, &rig.space_path).unwrap();
    rig.core.diff_targets.bind(CHAT, external.to_str().unwrap(), mast_proto::ChatDiffTargetSource::Agent, Some("bind-approval-test")).unwrap();
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let rule = ApprovalRule {
        id: "external-rule".into(), label: "External checkout".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["cargo".into(), "test".into()], command_regex: None,
        cwd_scope: Some(external.display().to_string()), require_isolated_home: false, constraints: None, hit_count: 0, last_used_at: None,
    };
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: "external-rule-save".into(), edit_environment: false,
    }), &actor, &[]).unwrap();
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!("cargo test"), cwd: Some(external.display().to_string()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve tests".into(), options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("external", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, run.clone()).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "external worktree approval").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(OTHER_CHAT, run).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { chat_id, input } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() {
            if chat_id == OTHER_CHAT { break input; }
        }
    };
    assert_eq!(input.questions[0].id, "approval");
    rig.harness.release.notify_waiters();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_command_rule_survives_restart_and_disable_applies_live() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let root = rig.space_path.clone();
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let mut rule = ApprovalRule {
        id: "env-live".into(), label: "Tests".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["cargo".into(), "nextest".into(), "run".into()], command_regex: None,
        cwd_scope: Some(root.clone()), require_isolated_home: false,
        constraints: Some(ApprovalRuleConstraints { environment: Some(vec!["CARGO_INCREMENTAL=0".into(), "PYTHONDONTWRITEBYTECODE=1".into()]), repository_executable: None }),
        hit_count: 0, last_used_at: None,
    };
    rig.core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule: rule.clone(), operation_id: "env-live-create".into(), edit_environment: true,
    }), &actor, &[]).unwrap();
    let Rig { _tmp, core, client, service, .. } = rig;
    drop(client); drop(service);
    core.workspace.shutdown();
    drop(core);
    let harness = GatedHarness { fake_codex: true, ..Default::default() };
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness.clone()));
    let data = std::fs::canonicalize(_tmp.path()).unwrap().join("data");
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::Mock, None).unwrap();
    core.sessions.set_agent_tools_launch(Some(AgentToolsLaunch { command: "/opt/zeron/bin/zeron".into(), args: vec!["mcp".into()], ipc_port: Some(27999) }));
    *harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!("CARGO_INCREMENTAL=0 PYTHONDONTWRITEBYTECODE=1 cargo nextest run --profile release -p mast-engine -E 'test(repair_tick_skips_unchanged_checkouts_and_a_capture_is_four_processes) | test(idle_checkouts_back_off_and_a_missed_head_change_is_caught_within_the_bound)' --no-fail-fast"), cwd: Some(root.clone()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve tests".into(), options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("test after restart", &root);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    core.doc_host.queue_command(CHAT, run.clone()).unwrap();
    wait_for(|| !harness.answers.lock().unwrap().is_empty(), "persisted rule answer").await;
    assert_eq!(harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    rule.enabled = false;
    core.workspace.work_outline_mutate(&WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: "env-live-disable".into(), edit_environment: false,
    }), &actor, &[]).unwrap();
    let mut signals = core.sessions.watch_worker_signals();
    core.doc_host.queue_command(OTHER_CHAT, run).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { chat_id, input, .. } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() {
            if chat_id == OTHER_CHAT { break input; }
        }
    };
    assert_eq!(input.questions[0].id, "approval");
    harness.release.notify_waiters();
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_environment_assignment_prompts_on_next_request() {
    use mast_proto::work_outline::{ApprovalRule, ApprovalRuleConstraints, ApprovalRuleHarness, SetApprovalRuleParams, WorkOutlineActor, WorkOutlineMutation};
    let rig = rig_for(true).await;
    let actor = WorkOutlineActor { kind: WorkOutlineActorKind::User, device_id: rig.core.device_id.clone(), chat_id: None, message_id: None };
    let mut rule = ApprovalRule {
        id: "removal-rule".into(), label: "Tests".into(), created_by: "user".into(), harness: ApprovalRuleHarness::Codex,
        enabled: true, argv_prefix: vec!["cargo".into(), "test".into()], command_regex: None, cwd_scope: Some(rig.space_path.clone()),
        require_isolated_home: false, constraints: Some(ApprovalRuleConstraints { environment: Some(vec!["CARGO_INCREMENTAL=0".into()]), repository_executable: None }), hit_count: 0, last_used_at: None,
    };
    let save = |rule, operation_id: &str| WorkOutlineMutation::SetApprovalRule(SetApprovalRuleParams {
        space_id: SPACE.into(), rule, operation_id: operation_id.into(), edit_environment: true,
    });
    rig.core.workspace.work_outline_mutate(&save(rule.clone(), "add-assignment"), &actor, &[]).unwrap();
    *rig.harness.questions.lock().unwrap() = vec![mast_proto::UserInputQuestion {
        permission: None, command_approval: Some(mast_proto::CommandApproval { command: json!("CARGO_INCREMENTAL=0 cargo test"), cwd: Some(rig.space_path.clone()), reason: None, sandbox_permissions: None }),
        allow_custom: false, id: "approval".into(), header: "Approve command".into(), question: "Approve tests".into(), options: vec!["Yes".into(), "No".into()], multi_select: false,
    }];
    let mut run = run_command("tests", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut run { request.harness = Some(HarnessId::Codex); request.auto_approve = false; }
    rig.core.doc_host.queue_command(CHAT, run.clone()).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "initial automatic answer").await;
    rule.constraints.as_mut().unwrap().environment = Some(vec![]);
    rig.core.workspace.work_outline_mutate(&save(rule, "remove-assignment"), &actor, &[]).unwrap();
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(OTHER_CHAT, run).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { chat_id, input } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() {
            if chat_id == OTHER_CHAT { break input; }
        }
    };
    assert_eq!(input.questions[0].id, "approval");
    rig.harness.release.notify_waiters();
    rig.core.shutdown().await;
}

fn rules_run(rig: &Rig, approval_rules: bool) -> SessionCommandPayload {
    let mut command = run_command("test", &rig.space_path);
    if let SessionCommandPayload::Run { request, .. } = &mut command {
        request.auto_approve = false;
        request.approval_rules = approval_rules;
    }
    command
}

fn permission_question(permission: mast_proto::PermissionRequest) -> mast_proto::UserInputQuestion {
    mast_proto::UserInputQuestion {
        permission: Some(permission), command_approval: None, allow_custom: false,
        id: "approval".into(), header: "Approve tool".into(),
        question: "The agent wants to run `x`. Allow it?".into(),
        options: vec!["Yes".into(), "No".into()], multi_select: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_mode_answers_from_mast_settings_and_audits() {
    let rig = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, json!({"permissions": {"allow": ["mcp__claude_ai_Notion__notion-update-page"]}}).to_string()).unwrap();
    rig.core.sessions.set_permission_rules_path(settings);
    *rig.harness.questions.lock().unwrap() = vec![permission_question(mast_proto::PermissionRequest::McpTool {
        server: "notion".into(), tool: "notion-update-page".into(),
    })];
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(CHAT, rules_run(&rig, true)).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "rule answer").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    assert!(signals.try_recv().is_err(), "an allowed request never reaches the user");
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert!(rows[0].summary.contains("Allowed by rule mcp__claude_ai_Notion__notion-update-page"), "{}", rows[0].summary);
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_mode_denies_from_mast_settings() {
    let rig = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, json!({"permissions": {"allow": ["Bash(git:*)"], "deny": ["Bash(git push:*)"]}}).to_string()).unwrap();
    rig.core.sessions.set_permission_rules_path(settings);
    *rig.harness.questions.lock().unwrap() = vec![permission_question(mast_proto::PermissionRequest::Command { command: json!("git push origin main") })];
    rig.core.doc_host.queue_command(CHAT, rules_run(&rig, true)).unwrap();
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "rule answer").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["No"]);
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_mode_offers_always_allow_and_saves_the_rule() {
    let rig = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("mast/settings.json");
    rig.core.sessions.set_permission_rules_path(settings.clone());
    *rig.harness.questions.lock().unwrap() = vec![permission_question(mast_proto::PermissionRequest::Command { command: json!("gh_read pulls 12") })];
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(CHAT, rules_run(&rig, true)).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { input, .. } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() { break input; }
    };
    assert_eq!(input.questions[0].options, ["Yes", "No", "Always allow: Bash(gh_read pulls:*)"]);
    assert!(rig.core.sessions.respond_input(CHAT, &input.request_id, vec![mast_proto::UserInputAnswer {
        question_id: "approval".into(), labels: vec!["Always allow: Bash(gh_read pulls:*)".into()],
    }]).unwrap());
    wait_for(|| !rig.harness.answers.lock().unwrap().is_empty(), "answer").await;
    assert_eq!(rig.harness.answers.lock().unwrap()[0][0].labels, ["Yes"]);
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(saved["permissions"]["allow"], json!(["Bash(gh_read pulls:*)"]));
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_mode_ignores_rules_and_offers_no_always_allow() {
    let rig = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, json!({"permissions": {"allow": ["Bash(gh_read:*)"]}}).to_string()).unwrap();
    rig.core.sessions.set_permission_rules_path(settings);
    *rig.harness.questions.lock().unwrap() = vec![permission_question(mast_proto::PermissionRequest::Command { command: json!("gh_read pulls") })];
    let mut signals = rig.core.sessions.watch_worker_signals();
    rig.core.doc_host.queue_command(CHAT, rules_run(&rig, false)).unwrap();
    let input = loop {
        if let mast_engine::sessions::WorkerSignal::Input { input, .. } = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await.unwrap().unwrap() { break input; }
    };
    assert_eq!(input.questions[0].options, ["Yes", "No"]);
    assert!(input.questions[0].permission.is_none());
    let refused = rig.core.sessions.respond_input(CHAT, &input.request_id, vec![mast_proto::UserInputAnswer {
        question_id: "approval".into(), labels: vec!["Always allow: Bash(gh_read:*)".into()],
    }]);
    assert!(refused.is_err(), "Always allow isn't an option outside Rules mode");
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_captures_rejected_server_requests() {
    let rig = rig().await;
    *rig.harness.events.lock().unwrap() = vec![AgentEvent::ServerRequestRejected { method: "method/token=fake-secret".into(), server_name: "example".into() }, AgentEvent::ServerRequestRejected { method: "method/token=fake-secret".into(), server_name: "example".into() }];
    rig.core.doc_host.queue_command(CHAT, run_command("test", &rig.space_path)).unwrap();
    wait_for(|| rig.core.workspace.friction_reports(SPACE).first().is_some_and(|r| r.occurrences == 2), "friction report").await;
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows[0].kind, mast_proto::FrictionKind::ToolFailure);
    assert!(!serde_json::to_string(&rows).unwrap().contains("fake-secret"));
    rig.harness.release.notify_one();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_captures_errored_turn() {
    let rig = rig().await;
    *rig.harness.events.lock().unwrap() = vec![AgentEvent::Error { code: None, resets_at: None, message: "password=fake-secret".into() }, AgentEvent::Done { status: DoneStatus::Errored, result: None, error: None, session_id: None }];
    rig.core.doc_host.queue_command(CHAT, run_command("test", &rig.space_path)).unwrap();
    wait_for(|| !rig.core.workspace.friction_reports(SPACE).is_empty(), "friction report").await;
    let rows = rig.core.workspace.friction_reports(SPACE);
    assert_eq!(rows[0].kind, mast_proto::FrictionKind::Environment);
    assert_eq!(rows[0].what_happened, "password=[REDACTED]");
    rig.core.shutdown().await;
}


#[tokio::test(flavor = "multi_thread")]
async fn prefixes_and_truncated_request_fields_are_accepted() {
    let rig = rig().await;
    let item = create_item(&rig, "prefix-item", "Prefix item", None).await;
    let outsider = create_item(&rig, "outside-item", "Outside", None).await;
    assign_chat(&rig, "prefix-assignment", CHAT, &item).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "prefix-run", SPACE, &rig.core.device_id);
    let token = lease.token();
    let created = tool(&rig, token, "work_outline_create_user_action_request", json!({
        "request": "Review", "dedupeKey": "prefix-test",
        "facts": [{"label": "é".repeat(41), "value": "🙂".repeat(121)}],
        "links": [{"label": "L".repeat(41), "target": {"type":"url", "url":"https://example.com"}}]
    })).await.unwrap();
    assert_eq!(created["warnings"].as_array().unwrap().len(), 3);
    let id = created["userActionRequestId"].as_str().unwrap();
    let fetched = tool(&rig, token, "get_user_action_request", json!({"requestId": &id[..8]})).await.unwrap();
    assert_eq!(fetched["request"]["facts"][0]["value"].as_str().unwrap().chars().count(), 120);
    assert!(fetched["request"]["facts"][0]["value"].as_str().unwrap().ends_with('…'));
    assert!(fetched["request"]["links"][0]["label"].as_str().unwrap().ends_with('…'));
    let missing = tool(&rig, token, "get_user_action_request", json!({"requestId":"0000000000000000"})).await.unwrap_err();
    assert!(missing.contains("open requests in your scope") && missing.contains(id), "{missing}");
    let denied = tool(&rig, token, "work_outline_set_status", json!({"workItemId": &outsider[..8], "status":"active"})).await.unwrap_err();
    assert!(denied.contains(&item) && denied.contains(&outsider) && denied.contains("allowed subtree"), "{denied}");
    tool(&rig, token, "work_outline_resolve_user_action_request", json!({"userActionRequestId": &id[..8]})).await.unwrap();
    let completed = tool(&rig, token, "work_outline_set_status", json!({"workItemId": &item[..8], "status":"completed"})).await.unwrap();
    assert_eq!(completed["workItemId"], item);
}

#[tokio::test(flavor = "multi_thread")]
async fn friction_dismissal_requires_orchestrator_and_records_reason() {
    let rig = rig().await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "dismiss-run", SPACE, &rig.core.device_id);
    let token = lease.token();
    let report = tool(&rig, token, "report_friction", json!({"kind":"bug","summary":"dismiss test","whatHappened":"failed"})).await.unwrap();
    let id = report["report"]["id"].as_str().unwrap();
    let args = json!({"id":id, "reason":"Fixed in branch example"});
    let denied = tool(&rig, token, "dismiss_friction", args.clone()).await.unwrap_err();
    assert!(denied.contains("unauthorized"), "{denied}");
    rig.client.call(methods::MUTATE, json!({"op":"createChat", "chatId":"dismiss-child", "spaceId":SPACE, "deviceId":rig.core.device_id, "parentChatId":CHAT})).await.unwrap();
    let result = tool(&rig, token, "dismiss_friction", args).await.unwrap();
    assert_eq!(result["report"]["status"], "dismissed");
    let list = tool(&rig, token, "list_friction", json!({"status":"dismissed"})).await.unwrap();
    assert_eq!(list["reports"][0]["dismissalReason"], "Fixed in branch example");
}

#[tokio::test(flavor = "multi_thread")]
async fn project_orchestrator_can_dismiss_friction() {
    let rig = rig().await;
    let project = project_rig(&rig).await;
    assign_chat(&rig, "dismiss-project-orch", CHAT, &project.project).await;
    assign_chat(&rig, "dismiss-project-worker", OTHER_CHAT, &project.ticket).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "project-dismiss-run", SPACE, &rig.core.device_id);
    let row = tool(&rig, lease.token(), "report_friction", json!({"kind":"bug","summary":"project worker","whatHappened":"failed"})).await.unwrap();
    let id = row["report"]["id"].as_str().unwrap();
    let result = tool(&rig, lease.token(), "dismiss_friction", json!({"id":id,"reason":"Fixed in branch"})).await.unwrap();
    assert_eq!(result["report"]["status"], "dismissed");
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_management_checks_designation_and_groups_sessions_across_spaces() {
    let rig = rig().await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "manage", SPACE, &rig.core.device_id);
    let token = lease.token();
    let manage = mast_proto::mast_management::TOOL;
    assert!(
        tool(&rig, token, manage, json!({}))
            .await
            .unwrap_err()
            .contains("unauthorized")
    );
    rig.client
        .call(methods::SET_PERSONAL_ASSISTANT, json!({"chatId":CHAT}))
        .await
        .unwrap();
    let other_lease =
        rig.core
            .sessions
            .agent_tokens()
            .mint(OTHER_CHAT, "ordinary", SPACE, &rig.core.device_id);
    assert!(
        tool(&rig, other_lease.token(), manage, json!({}))
            .await
            .unwrap_err()
            .contains("unauthorized")
    );
    let guide = tool(&rig, token, manage, json!({})).await.unwrap();
    assert!(guide.get("ScheduledTasks").is_some());
    assert!(guide.get("WatchSidebarPreferences").is_some());
    let tasks = tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{"action":"list"}, "view":{"fields":["tasks"], "maxBytes":4096}})).await.unwrap();
    assert!(tasks.get("tasks").is_some() && tasks.get("runs").is_none(), "{tasks}");
    let runs = tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{"action":"list"}, "view":{"path":"runs", "limit":3}})).await.unwrap();
    assert_eq!(runs["items"].as_array().unwrap().len(), 0);
    assert!(tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{"action":"list"}, "view":{"bogus":1}})).await.is_err());
    // Paging a work outline one row at a time reassembles the uncapped item list.
    for n in 0..3 {
        tool(&rig, token, manage, json!({"method":"WorkOutlineMutate", "params":{"op":"createWorkItem", "spaceId":SPACE, "title":format!("Paged {n}"), "operationId":format!("page-{n}")}})).await.unwrap();
    }
    let outline = json!({"method":"WatchWorkOutline", "params":{"spaceId":SPACE}});
    let whole = tool(&rig, token, manage, json!({"method":outline["method"], "params":outline["params"], "view":{"full":true}})).await.unwrap();
    let mut ids = vec![];
    let mut offset = 0;
    loop {
        let page = tool(&rig, token, manage, json!({"method":outline["method"], "params":outline["params"], "view":{"path":"items", "fields":["id"], "limit":1, "offset":offset}})).await.unwrap();
        ids.extend(page["items"].as_array().unwrap().iter().map(|i| i["id"].clone()));
        match page.get("nextOffset").and_then(|n| n.as_u64()) {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    let expected: Vec<_> = whole["items"].as_array().unwrap().iter().map(|i| i["id"].clone()).collect();
    assert!(expected.len() >= 3 && ids == expected, "{ids:?} vs {expected:?}");
    assert!(
        tool(
            &rig,
            token,
            manage,
            json!({"method":"GrantAdmin", "params":{}})
        )
        .await
        .is_err()
    );
    assert!(
        tool(
            &rig,
            token,
            manage,
            json!({"method":"AgentToolCall", "params":{}})
        )
        .await
        .is_err()
    );
    // No Space: this chat is outside the caller's ordinary project scope.
    for params in [
        json!({"op":"renameChat", "chatId":PROJECTLESS, "title":"Managed"}),
        json!({"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"create", "id":"group", "name":"Follow up"}}}),
        json!({"op":"changeSidebarPin", "change":{"action":"section", "change":{"action":"assign", "sessionId":PROJECTLESS, "sectionId":"group"}}}),
    ] {
        tool(
            &rig,
            token,
            manage,
            json!({"method":"Mutate", "params":params}),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        rig.core
            .workspace
            .chat(PROJECTLESS)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Managed")
    );
    let prefs = tool(
        &rig,
        token,
        manage,
        json!({"method":"WatchSidebarPreferences"}),
    )
    .await
    .unwrap();
    assert!(prefs.to_string().contains(PROJECTLESS));
    assert!(prefs.to_string().contains("Follow up"));
    rig.client
        .call(methods::CLEAR_PERSONAL_ASSISTANT, json!({}))
        .await
        .unwrap();
    assert!(tool(&rig, token, manage, json!({"method":"Mutate", "params":{"op":"renameChat", "chatId":PROJECTLESS, "title":"Refused"}})).await.unwrap_err().contains("unauthorized"));
    assert_eq!(
        rig.core
            .workspace
            .chat(PROJECTLESS)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Managed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_management_edits_tasks_and_reruns_with_revision_and_replay_checks() {
    let rig = rig().await;
    let lease =
        rig.core
            .sessions
            .agent_tokens()
            .mint(CHAT, "manage-tasks", SPACE, &rig.core.device_id);
    let token = lease.token();
    rig.client
        .call(methods::SET_PERSONAL_ASSISTANT, json!({"chatId":CHAT}))
        .await
        .unwrap();
    let manage = mast_proto::mast_management::TOOL;
    let mut draft = json!({
        "name":"Briefing", "prompt":"Original prompt", "cwd":rig.space_path,
        "spaceId":SPACE, "config":{"harness":"mock", "sandbox":"workspace-write", "autoApprove":false},
        "schedule":{"type":"cron", "expression":"0 8 * * 1-5", "timezone":"America/New_York"},
        "enabled":false, "graceMinutes":60
    });
    tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{
        "action":"save", "id":"managed-task", "expectedRevision":0, "draft":draft, "operationId":"create-managed-task"
    }})).await.unwrap();
    let first = tool(
        &rig,
        token,
        manage,
        json!({"method":"ScheduledTasks", "params":{"action":"list"}}),
    )
    .await
    .unwrap();
    assert_eq!(first["tasks"][0]["draft"]["prompt"], "Original prompt");
    draft["prompt"] = json!("Updated prompt");
    tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{
        "action":"save", "id":"managed-task", "expectedRevision":1, "draft":draft, "operationId":"edit-managed-task"
    }})).await.unwrap();
    assert!(tool(&rig, token, manage, json!({"method":"ScheduledTasks", "params":{
        "action":"save", "id":"managed-task", "expectedRevision":1, "draft":draft, "operationId":"stale-edit"
    }})).await.is_err());
    for _ in 0..2 {
        tool(
            &rig,
            token,
            manage,
            json!({"method":"ScheduledTasks", "params":{
                "action":"runNow", "id":"managed-task", "operationId":"same-rerun"
            }}),
        )
        .await
        .unwrap();
    }
    let history = tool(
        &rig,
        token,
        manage,
        json!({"method":"ScheduledTasks", "params":{"action":"history", "id":"managed-task"}}),
    )
    .await
    .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["runs"][0]["snapshot"]["prompt"], "Updated prompt");
}

fn status_document(stamp: &str) -> String {
    let sections: String = mast_proto::MANAGER_STATUS_SECTIONS
        .iter()
        .map(|s| format!("## {s}\nNone\n\n"))
        .collect();
    format!("Updated: {stamp}\n\n{sections}")
}

fn run_token(rig: &Rig, run: usize) -> String {
    match rig.harness.spawns.lock().unwrap()[run].clone() {
        Spawn::WithTools { tools, .. } => tools.env["MAST_AGENT_TOKEN"].clone(),
        other => panic!("run {run} had no agent tools: {other:?}"),
    }
}

fn nudges(rig: &Rig) -> usize {
    rig.harness
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.prompt.contains("did not publish a new Manager status"))
        .count()
}

/// A real state-changing Manager turn must not create an automatic reminder;
/// manual publication remains available in the same turn.
#[tokio::test(flavor = "multi_thread")]
async fn manager_changes_do_not_create_reminders_and_manual_publish_still_works() {
    let (rig, _item) = role_rig("status-reminders-disabled").await;
    rig.core.doc_host.queue_command(CHAT, run_command("change and publish", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && rig.core.sessions.turn_in_flight(CHAT), "manager turn").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Worker task", "idempotencyKey": "disabled-w1" })).await.unwrap();
    let published = tool(&rig, &token, "artifact_publish", json!({
        "title": "Manager status", "role": "manager-status", "note": "n", "idempotencyKey": "disabled-s1",
        "content": status_document("09:00")
    })).await.unwrap();
    assert_eq!(published["revision"], 1);
    assert!(rig.core.sessions.agent_tokens().manager_status().tracked_chat_ids().is_empty());
    // An additional uncovered mutation makes the no-reminder check meaningful.
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Later task", "idempotencyKey": "disabled-w2" })).await.unwrap();
    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(CHAT), "manager turn to close").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(nudges(&rig), 0);
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 1);
    assert!(rig.core.doc_host.open(CHAT).unwrap().doc().read_queue().unwrap().is_empty());
}

/// Disabling reminders must not discard an ordinary queued message, even if
/// the user copied the old reminder text into it.
#[tokio::test(flavor = "multi_thread")]
async fn disabled_reminders_preserve_ordinary_queue_rows() {
    let (rig, _item) = role_rig("status-disabled-queue").await;
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    rig.core.doc_host.pause_all_queues();
    let text = mast_proto::manager_status_nudge_text(0);
    rig.core.doc_host.queue_message_with_id(CHAT, "user-copy", &text, Vec::new(), true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "user-copy");
    assert_eq!(rows[0].text, text);
    assert_eq!(nudges(&rig), 0);
}

/// Successful calls by many non-Managers must not leave tracker entries after
/// their chats are deleted while automatic reminders are disabled.
#[tokio::test(flavor = "multi_thread")]
async fn deleted_non_manager_chat_churn_does_not_accumulate_status_tracking() {
    let rig = rig().await;
    let item = create_item(&rig, "status-churn-root", "Churn", None).await;
    let tokens = rig.core.sessions.agent_tokens();
    for n in 0..24 {
        let chat = format!("status-churn-{n}");
        rig.core.workspace.create_chat(&chat, Some(SPACE), Some(&rig.core.device_id), None, None).unwrap();
        assign_chat(&rig, &format!("status-churn-assign-{n}"), &chat, &item).await;
        let lease = tokens.mint(&chat, &format!("status-churn-run-{n}"), SPACE, &rig.core.device_id);
        tool(&rig, lease.token(), "work_outline_create_child", json!({
            "title": format!("Child {n}"), "idempotencyKey": format!("status-churn-child-{n}")
        })).await.unwrap();
        assert!(rig.core.workspace.delete_chat(&chat).unwrap());
    }
    assert!(tokens.manager_status().tracked_chat_ids().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_admin_read_and_dry_run_do_not_owe_a_reminder() {
    let (rig, item) = role_rig("status-admin-read").await;
    rig.core.doc_host.queue_command(CHAT, run_command("inspect", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "manager run").await;
    let token = run_token(&rig, 0);
    let tracker = rig.core.sessions.agent_tokens();
    admin(&rig, &token, json!({"read": true})).await.unwrap();
    admin(&rig, &token, json!({"dryRun": true, "idempotencyKey": "preview", "steps": [
        {"op": "create", "parentId": item, "title": "Preview only"}
    ]})).await.unwrap();
    assert_eq!(tracker.manager_status().generation(CHAT), 0);
    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(CHAT), "read-only turn to close").await;
    assert!(!tracker.manager_status().owes(CHAT));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(nudges(&rig), 0);
}

/// A fast tool call and turn close do not start a reminder.
#[tokio::test(flavor = "multi_thread")]
async fn a_fast_manager_tool_call_does_not_create_a_reminder() {
    let (rig, _item) = role_rig("status-fast").await;
    rig.core.doc_host.queue_command(CHAT, run_command("add a worker", &rig.space_path)).unwrap();
    // No wait for the tracker: call as soon as the run exists.
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "the run").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Worker task", "idempotencyKey": "f1" }))
        .await
        .unwrap();
    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(CHAT), "turn to close").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(nudges(&rig), 0);
}

fn nudge_prompts(rig: &Rig) -> Vec<String> {
    rig.harness
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.prompt.contains("did not publish a new Manager status"))
        .map(|r| r.prompt.clone())
        .collect()
}

async fn plant_stamped_status(rig: &Rig, id: &str, item: &str, revisions: u32) {
    let role_id = mast_proto::standing_role_id(mast_proto::StandingRoleKind::Manager, item);
    let agent = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::Agent,
        chat_id: Some("earlier-manager".into()),
        ..Default::default()
    };
    let view = mast_proto::ArtifactView {
        id: id.into(),
        space_id: SPACE.into(),
        device_id: rig.core.device_id.clone(),
        work_item_id: Some(item.into()),
        title: "Manager status".into(),
        role: Some("manager-status".into()),
        kind: mast_proto::ArtifactKind::Markdown,
        current_revision: revisions,
        revisions: (1..=revisions)
            .rev()
            .map(|n| mast_proto::ArtifactRevision {
                id: n,
                hash: format!("{n:064x}"),
                at: i64::from(n),
                by: agent.clone(),
                manager_status: Some(mast_proto::ManagerStatusStamp {
                    role_id: role_id.clone(),
                    chat_id: "earlier-manager".into(),
                }),
                ..Default::default()
            })
            .collect(),
        created_at: 1,
        updated_at: 1,
        ..Default::default()
    };
    rig.core.workspace.put_artifact(&view).unwrap();
}

/// An existing status record does not cause a reminder.
#[tokio::test(flavor = "multi_thread")]
async fn an_existing_status_record_does_not_trigger_a_reminder() {
    let (rig, item) = role_rig("status-seeded").await;
    plant_stamped_status(&rig, "status-seeded-doc", &item, 3).await;
    rig.core.doc_host.queue_command(CHAT, run_command("add a worker", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "the turn").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Worker task", "idempotencyKey": "s1" }))
        .await
        .unwrap();
    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(CHAT), "turn to close").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(nudges(&rig), 0);
}

/// Consecutive turns do not cause a reminder after the second changes state.
#[tokio::test(flavor = "multi_thread")]
async fn consecutive_manager_turns_do_not_trigger_reminders() {
    let (rig, _item) = role_rig("status-consecutive").await;
    rig.core.doc_host.queue_command(CHAT, run_command("first", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "turn A").await;
    // B is queued before A ends, so it starts as soon as A is idle.
    rig.core.doc_host.queue_command(CHAT, run_command("second", &rig.space_path)).unwrap();
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "A", "idempotencyKey": "a1" }))
        .await
        .unwrap();
    tool(
        &rig,
        &token,
        "artifact_publish",
        json!({ "title": "Manager status", "role": "manager-status", "note": "n", "idempotencyKey": "sa",
                "content": status_document("09:00") }),
    )
    .await
    .unwrap();
    rig.harness.release.notify_one();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 2, "turn B").await;
    assert_eq!(nudges(&rig), 0, "A published, so A owes nothing");
    let token = run_token(&rig, 1);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "B", "idempotencyKey": "b1" }))
        .await
        .unwrap();
    rig.harness.release.notify_one();
    wait_for(|| !rig.core.sessions.turn_in_flight(CHAT), "turn to close").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(nudges(&rig), 0);
}

/// A provisional Manager can change state before adoption without a reminder.
#[tokio::test(flavor = "multi_thread")]
async fn adopting_a_manager_does_not_trigger_a_reminder() {
    use mast_proto::StandingRoleKind;
    let rig = rig().await;
    let item = create_item(&rig, "status-first-item", "Manager", None).await;
    plant_stamped_status(&rig, "status-first-doc", &item, 2).await;
    let first = "first-manager-chat";
    rig.core
        .workspace
        .create_provisional_role_chat(
            first,
            SPACE,
            None,
            rig.space_path.clone(),
            mast_proto::standing_role_provisional_marker(StandingRoleKind::Manager, &item, "op-first"),
        )
        .unwrap();
    assign_chat(&rig, "status-first-assignment", first, &item).await;

    // The first turn: its token is minted while the chat is provisional.
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(first, "run-first", SPACE, &rig.core.device_id);
    tool(&rig, lease.token(), "work_outline_create_child", json!({ "title": "Worker", "idempotencyKey": "p1" }))
        .await
        .unwrap();
    // The call had no turn behind it: it is its own turn, placed in the project
    // from the provisional marker, and owed outright because the chat holds nothing.
    assert!(!tokens.manager_status().owes(first), "disabled tracking retains no obligation");
    // The turn ends before the chat is adopted.
    drop(lease);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0, "nothing is asked before adoption");

    rig.core.workspace.activate_standing_role_chat(StandingRoleKind::Manager, &item, first, &[]).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(nudge_prompts(&rig).is_empty());
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
}

/// A successor can change state before rotation without a reminder.
#[tokio::test(flavor = "multi_thread")]
async fn rotating_a_manager_does_not_trigger_a_reminder() {
    let (rig, item) = role_rig("status-successor").await;
    plant_stamped_status(&rig, "status-successor-doc", &item, 4).await;
    let successor = "successor-chat";
    rig.core
        .workspace
        .create_chat(successor, Some(SPACE), Some(&rig.core.device_id), None, Some(rig.space_path.clone()))
        .unwrap();
    assign_chat(&rig, "status-successor-chat-assignment", successor, &item).await;
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, successor, "op-handoff", None).unwrap();

    // The successor's first turn runs while the old manager still holds the role.
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(successor, "run-successor", SPACE, &rig.core.device_id);
    tool(&rig, lease.token(), "work_outline_create_child", json!({ "title": "Worker", "idempotencyKey": "h1" }))
        .await
        .unwrap();
    // The call had no turn behind it: the reserved hand-off places it in the
    // project, and it is held for the successor because the chat holds nothing yet.
    assert!(!tokens.manager_status().owes(successor), "disabled tracking retains no obligation");
    drop(lease);
    // Nothing expires the reservation, so a rotation long after the turn ended
    // (here, dozens of re-checks later) is still honored.
    tokens.manager_status().set_recheck_interval(Duration::from_millis(40));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0, "the successor holds nothing yet");

    rig.core.workspace.rotate_standing_role_for_chat(CHAT, successor).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(nudge_prompts(&rig).is_empty());
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
}

/// Many successor changes followed by rotation do not create reminders.
#[tokio::test(flavor = "multi_thread")]
async fn many_turns_before_rotation_do_not_trigger_reminders() {
    let (rig, item) = role_rig("status-many").await;
    plant_stamped_status(&rig, "status-many-doc", &item, 4).await;
    let successor = "many-successor";
    rig.core
        .workspace
        .create_chat(successor, Some(SPACE), Some(&rig.core.device_id), None, Some(rig.space_path.clone()))
        .unwrap();
    assign_chat(&rig, "status-many-chat-assignment", successor, &item).await;
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, successor, "op-many", None).unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_recheck_interval(Duration::from_millis(40));

    // Past any fixed number of held turns.
    for n in 0..24 {
        let lease = tokens.mint(successor, &format!("run-many-{n}"), SPACE, &rig.core.device_id);
        tool(
            &rig,
            lease.token(),
            "work_outline_create_child",
            json!({ "title": format!("Worker {n}"), "idempotencyKey": format!("many-{n}") }),
        )
        .await
        .unwrap();
        drop(lease);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0, "the successor holds nothing yet");

    rig.core.workspace.rotate_standing_role_for_chat(CHAT, successor).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(nudge_prompts(&rig).is_empty());
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
}

/// A hand-off that never completes leaves its successor a non-holder: the
/// obligation is kept while the reservation stands, and nothing is sent to a
/// chat that does not hold the role. When the successor is gone the obligation
/// ends with it, and a later rotation sends nothing for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_handoff_never_nudges_a_non_holder() {
    let (rig, item) = role_rig("status-failed-handoff").await;
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_recheck_interval(Duration::from_millis(40));
    let successor = "failed-successor";
    rig.core
        .workspace
        .create_chat(successor, Some(SPACE), Some(&rig.core.device_id), None, Some(rig.space_path.clone()))
        .unwrap();
    assign_chat(&rig, "status-failed-assignment", successor, &item).await;
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, successor, "op-failed", None).unwrap();

    let lease = tokens.mint(successor, "run-failed", SPACE, &rig.core.device_id);
    tool(&rig, lease.token(), "work_outline_create_child", json!({ "title": "Worker", "idempotencyKey": "x1" }))
        .await
        .unwrap();
    drop(lease);
    // The hand-off never rotates: the reservation stands, nothing is sent.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
    assert_eq!(nudges(&rig), 0);
    assert_eq!(
        rig.core.workspace.standing_role_chat(mast_proto::StandingRoleKind::Manager, &item).as_deref(),
        Some(CHAT),
        "the old manager still holds the role"
    );

    // The successor is deleted: that is a confirmed failure, the obligation ends.
    assert!(rig.core.workspace.delete_chat(successor).unwrap());

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
    assert_eq!(nudges(&rig), 0);
}

/// The reservation going to a different successor ends the first one's claim
/// at once, without waiting for the cutoff.
#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_handoff_reservation_drops_the_first_successors_turn() {
    let (rig, item) = role_rig("status-replaced-handoff").await;
    let tokens = rig.core.sessions.agent_tokens();
    for (name, op) in [("first-successor", "op-r1"), ("second-successor", "op-r2")] {
        rig.core
            .workspace
            .create_chat(name, Some(SPACE), Some(&rig.core.device_id), None, Some(rig.space_path.clone()))
            .unwrap();
        assign_chat(&rig, &format!("assign-{name}"), name, &item).await;
        if name == "first-successor" {
            rig.core.workspace.reserve_standing_role_hand_off(CHAT, name, op, None).unwrap();
        }
    }
    let lease = tokens.mint("first-successor", "run-r1", SPACE, &rig.core.device_id);
    tool(&rig, lease.token(), "work_outline_create_child", json!({ "title": "Worker", "idempotencyKey": "r1" }))
        .await
        .unwrap();
    // The first reservation is replaced before its turn ends.
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, "second-successor", "op-r2", None).unwrap();
    drop(lease);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
    assert_eq!(nudges(&rig), 0);
}

/// A warm session keeps one agent token across many turns: it parks after each
/// and the next prompt is steered into the same run. Each turn is checked on its
/// own, whatever the token does. The reminder is itself a steered turn.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "automatic Manager reminders are temporarily disabled"]
async fn warm_turns_sharing_one_token_are_checked_separately() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-warm").await;
    let feed_done = || {
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("sess-warm".into()),
        };
        warm.feed.lock().unwrap().as_ref().expect("a live run").send(done).unwrap();
    };
    let reminders = || {
        warm.received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("did not publish a new Manager status"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let working = || rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(mast_proto::SessionStatus::Working);
    let idle = || rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(mast_proto::SessionStatus::Idle);

    // Turn one dispatches and publishes nothing; the session then parks.
    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && working(), "turn one").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "One", "idempotencyKey": "w1" }))
        .await
        .unwrap();
    feed_done();
    // The token is still live, yet the turn is over: the reminder is steered
    // into the parked run.
    wait_for(|| reminders().len() == 1, "the reminder for turn one").await;
    assert!(reminders()[0].contains("no revision exists yet"), "{}", reminders()[0]);

    // The reminder is turn two: it publishes on the same token.
    wait_for(working, "the reminder turn").await;
    let published = tool(
        &rig,
        &token,
        "artifact_publish",
        json!({ "title": "Manager status", "role": "manager-status", "note": "n", "idempotencyKey": "warm-1",
                "content": status_document("09:00") }),
    )
    .await
    .unwrap();
    assert_eq!(published["revision"], 1);
    feed_done();
    wait_for(idle, "turn two to park").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(reminders().len(), 1, "turn two published, so nothing more is asked");

    // Turn three is the user's next prompt, steered in. Turn two's publication
    // does not cover it.
    rig.core.sessions.steer(CHAT, "another", None).await.unwrap();
    wait_for(working, "turn three").await;
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Three", "idempotencyKey": "w3" }))
        .await
        .unwrap();
    feed_done();
    wait_for(|| reminders().len() == 2, "the reminder for turn three").await;
    assert!(reminders()[1].contains("latest revision is 1"), "{}", reminders()[1]);
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 1, "one run, one token, throughout");
}

/// A prompt steered into a parked run is numbered before the mailbox can show
/// it to the agent. The hook runs the instant the harness receives the prompt,
/// before the session has published Working: the call it makes is admitted into
/// the new turn, and completes into it, not into the turn that just ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_steered_into_a_parked_run_is_numbered_before_the_agent_can_act() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-prenumber").await;
    let tokens = rig.core.sessions.agent_tokens();
    let feed_done = || {
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("sess-warm".into()),
        };
        warm.feed.lock().unwrap().as_ref().expect("a live run").send(done).unwrap();
    };
    let reminders = || {
        warm.received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("did not publish a new Manager status"))
            .count()
    };
    let status_is = |want: mast_proto::SessionStatus| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(want);

    // Turn one changes nothing, then parks.
    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && status_is(mast_proto::SessionStatus::Working), "turn one").await;
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the park").await;
    // The agent's first state-changing call completes the instant the harness is
    // handed the prompt, before the session has published Working for it.
    let seen: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    {
        let tokens = tokens.clone();
        let seen = seen.clone();
        *warm.on_steer.lock().unwrap() = Some(Box::new(move || {
            tokens.manager_status().changed(CHAT);
            *seen.lock().unwrap() = true;
        }));
    }
    assert!(!tokens.manager_status().owes(CHAT));
    rig.core.sessions.steer(CHAT, "next", None).await.unwrap();
    wait_for(|| *seen.lock().unwrap(), "the harness to receive the prompt").await;
    wait_for(|| status_is(mast_proto::SessionStatus::Working), "the steered turn").await;
    assert!(tokens.manager_status().owes(CHAT));

    // However the call and the boundary interleaved, the change is owed: when the
    // turn settles without a status, one reminder is steered in.
    *warm.on_steer.lock().unwrap() = None;
    feed_done();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reminders(), 0);
}

/// The reminder is steered into a turn that is already running. No new turn
/// starts for it, so the turn that took it is the ask. Its reservation does not
/// strand: when that turn settles, the next owing turn is asked as usual.
#[tokio::test(flavor = "multi_thread")]
async fn a_reminder_steered_into_an_active_turn_does_not_strand_later_reminders() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-active-steer").await;
    let tokens = rig.core.sessions.agent_tokens();
    let feed_done = || {
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("sess-warm".into()),
        };
        warm.feed.lock().unwrap().as_ref().expect("a live run").send(done).unwrap();
    };
    let reminders = || {
        warm.received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("did not publish a new Manager status"))
            .count()
    };
    let status_is = |want: mast_proto::SessionStatus| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(want);

    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && status_is(mast_proto::SessionStatus::Working), "turn one").await;
    // A reminder is owed, and its message is steered into the turn in flight.
    let id = tokens.manager_status().reserve_reminder(CHAT);
    let outcome = rig.core.sessions.steer(CHAT, "status, please", Some(id.clone())).await.unwrap();
    assert!(matches!(outcome, mast_engine::SteerOutcome::Accepted));
    assert!(status_is(mast_proto::SessionStatus::Working));
    // The turn settles without a status: that was the ask, and it is settled.
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the turn to park").await;
    wait_for(|| tokens.manager_status().reminder(CHAT).is_none(), "the reminder to settle").await;

    // The next turn changes state and publishes nothing: it is asked normally.
    rig.core.sessions.steer(CHAT, "another", None).await.unwrap();
    wait_for(|| status_is(mast_proto::SessionStatus::Working), "the next turn").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Next", "idempotencyKey": "as1" }))
        .await
        .unwrap();
    feed_done();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reminders(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_old_session_closure_cannot_settle_a_newly_dispatched_reminder() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (rig, _) = role_rig("status-old-close").await;
    let tokens = rig.core.sessions.agent_tokens();
    let tracker = tokens.manager_status();
    rig.core.doc_host.queue_command(CHAT, run_command("first turn", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "first run").await;
    let entered = Arc::new(AtomicBool::new(false));
    let hook_entered = entered.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let hook_completed = completed.clone();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    tracker.set_test_hook(Some(Arc::new(move |event, chat| {
        if event == "close" && chat == CHAT {
            hook_entered.store(true, Ordering::SeqCst);
            resume_rx.lock().unwrap().recv().unwrap();
        } else if event == "closed" && chat == CHAT {
            hook_completed.store(true, Ordering::SeqCst);
        }
        false
    })));
    rig.harness.release.notify_one();
    wait_for(|| entered.load(Ordering::SeqCst), "old closure paused").await;
    let id = tracker.reserve_reminder(CHAT);
    rig.core.doc_host.queue_message_with_id(CHAT, &id, "reminder", Vec::new(), true).await.unwrap();
    wait_for(|| tracker.delivered_reminder(CHAT).as_deref() == Some(id.as_str()), "new run to confirm reminder").await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 2);
    resume_tx.send(()).unwrap();
    wait_for(|| completed.load(Ordering::SeqCst), "old closure report to finish").await;
    tracker.set_test_hook(None);
    assert_eq!(tracker.reminder(CHAT).as_deref(), Some(id.as_str()));
    assert!(tracker.owes(CHAT));
    rig.harness.release.notify_one();
    wait_for(|| tracker.reminder(CHAT).is_none(), "new turn to settle its reminder").await;
    assert!(!tracker.owes(CHAT));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "automatic Manager reminders are temporarily disabled"]
async fn watcher_retries_a_real_context_queue_failure_without_other_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (rig, _) = role_rig("status-queue-retry").await;
    let tokens = rig.core.sessions.agent_tokens();
    let tracker = tokens.manager_status();
    tracker.set_recheck_interval(Duration::from_millis(30));
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    let hook_tokens = tokens.clone();
    tracker.set_test_hook(Some(Arc::new(move |event, chat| {
        if event == "queue" && chat == CHAT && seen.fetch_add(1, Ordering::SeqCst) == 0 {
            hook_tokens.manager_status().drain_test_wake();
            return true;
        }
        false
    })));
    tracker.changed(CHAT);
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "retry to deliver reminder").await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(nudges(&rig), 1);
    tracker.set_test_hook(None);
    rig.harness.release.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "automatic Manager reminders are temporarily disabled"]
async fn watcher_withdraws_covered_queue_row_and_wakes_for_newer_generation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (rig, _) = role_rig("status-withdraw-wake").await;
    let tokens = rig.core.sessions.agent_tokens();
    let tracker = tokens.manager_status();
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    rig.core.doc_host.pause_all_queues();
    let withdrawing = Arc::new(AtomicBool::new(false));
    let entered = withdrawing.clone();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let hook_tokens = tokens.clone();
    tracker.set_test_hook(Some(Arc::new(move |event, chat| {
        if event == "withdraw" && chat == CHAT {
            hook_tokens.manager_status().drain_test_wake();
            entered.store(true, Ordering::SeqCst);
            resume_rx.lock().unwrap().recv().unwrap();
        }
        false
    })));
    tracker.changed(CHAT);
    wait_for(|| !handle.doc().read_queue().unwrap().is_empty(), "first reminder queued").await;
    let first = tracker.reminder(CHAT).unwrap();
    tracker.published(CHAT, 1, 1);
    tracker.changed(CHAT);
    wait_for(|| withdrawing.load(Ordering::SeqCst), "watcher to begin withdrawal").await;
    resume_tx.send(()).unwrap();
    wait_for(|| tracker.reminder(CHAT).is_some_and(|id| id != first), "newer reminder reserved").await;
    wait_for(|| handle.doc().read_queue().unwrap().iter().any(|row| row.id != first), "newer reminder queued").await;
    let queue = handle.doc().read_queue().unwrap();
    assert!(!queue.iter().any(|row| row.id == first));
    assert_eq!(queue.len(), 1);
    tracker.set_test_hook(None);
}

/// A Manager's send needs its role instructions delivered, so a harness the
/// engine does not have refuses the send before any row exists. The reminder
/// stays reserved and nothing reaches a harness.
#[tokio::test(flavor = "multi_thread")]
async fn a_manager_send_to_a_missing_harness_is_refused_and_keeps_the_reminder_reservation() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();
    // A chat configured for a harness the engine does not have: dispatching to
    // it fails.
    let chat = "restore-chat";
    let config = mast_proto::ChatConfig {
        approval_rules: false,
        harness: HarnessId::Codex,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    };
    rig.core
        .workspace
        .create_chat(chat, Some(SPACE), Some(&rig.core.device_id), Some(config), Some(rig.space_path.clone()))
        .unwrap();
    // It is a manager's chat: only those are tracked.
    let item = create_item(&rig, "restore-item", "Restore project", None).await;
    rig.core
        .workspace
        .adopt_standing_role(mast_proto::StandingRoleKind::Manager, &item, chat, &[])
        .unwrap();
    let id = tokens.manager_status().reserve_reminder(chat);
    let error = rig
        .core
        .doc_host
        .queue_message_with_id(chat, &id, "remind", Vec::new(), true)
        .await
        .expect_err("the Manager's harness cannot deliver its role instructions");
    assert!(error.to_string().contains("binary not found"), "{error}");
    let handle = rig.core.doc_host.open(chat).unwrap();
    assert!(handle.doc().read_queue().unwrap().is_empty(), "no row was queued");
    assert_eq!(tokens.manager_status().reminder(chat).as_deref(), Some(id.as_str()));
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0, "nothing was delivered");
}

fn warm_fixture(
    warm: &Arc<Warm>,
) -> (impl Fn() + '_, impl Fn() -> usize + '_) {
    let feed_done = move || {
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("sess-warm".into()),
        };
        warm.feed.lock().unwrap().as_ref().expect("a live run").send(done).unwrap();
    };
    let reminders = move || {
        warm.received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("did not publish a new Manager status"))
            .count()
    };
    (feed_done, reminders)
}

/// A reminder that reaches an active turn through the queue's own steering
/// (`steer_queued_now`) is settled by that turn, like a direct steer.
#[tokio::test(flavor = "multi_thread")]
async fn a_queued_reminder_steered_into_an_active_turn_is_settled_by_it() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-queued-steer").await;
    let tokens = rig.core.sessions.agent_tokens();
    let (feed_done, reminders) = warm_fixture(&warm);
    let status_is = |want: mast_proto::SessionStatus| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(want);

    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && status_is(mast_proto::SessionStatus::Working), "turn one").await;
    // The reminder is queued behind the running turn, then promoted into it.
    let id = tokens.manager_status().reserve_reminder(CHAT);
    rig.core.doc_host.queue_message_with_id(CHAT, &id, "status, please", Vec::new(), true).await.unwrap();
    assert!(rig.core.doc_host.steer_queued_now(CHAT, &id).await.unwrap(), "the row was steered in");
    wait_for(|| warm.received.lock().unwrap().iter().any(|m| m.contains("status, please")), "the harness to receive it").await;
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the turn to park").await;
    wait_for(|| tokens.manager_status().reminder(CHAT).is_none(), "the reminder to settle").await;

    // Later owing turns are asked as usual.
    rig.core.sessions.steer(CHAT, "another", None).await.unwrap();
    wait_for(|| status_is(mast_proto::SessionStatus::Working), "the next turn").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Next", "idempotencyKey": "qs1" }))
        .await
        .unwrap();
    feed_done();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reminders(), 0);
}

/// A transcript-write failure drops the reserved mailbox slot before the
/// prompt becomes visible to the harness.
#[tokio::test(flavor = "multi_thread")]
async fn a_document_error_before_mailbox_acceptance_does_not_deliver_the_prompt() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-mailbox-write").await;
    let (feed_done, reminders) = warm_fixture(&warm);
    let status_is = |want: mast_proto::SessionStatus| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(want);

    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && status_is(mast_proto::SessionStatus::Working), "turn one").await;
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the park").await;
    rig.core.sessions.fail_next_user_write(CHAT);
    assert!(rig.core.sessions.steer(CHAT, "failed write", None).await.is_err());
    assert!(status_is(mast_proto::SessionStatus::Idle));

    // The next prompt is delivered and its change is tracked as usual.
    rig.core.sessions.steer(CHAT, "next", None).await.unwrap();
    wait_for(|| status_is(mast_proto::SessionStatus::Working), "the next turn").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "After", "idempotencyKey": "pm1" }))
        .await
        .unwrap();
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the next turn to park").await;
    assert!(!warm.received.lock().unwrap().iter().any(|message| message == "failed write"));
    assert_eq!(reminders(), 0);
}

// Blocking hooks mirror the narrow interleavings between the engine's
// synchronous operations without relying on sleeps or holding the fence.
struct StatusBarrier {
    entered: std::sync::atomic::AtomicBool,
    resumed: std::sync::atomic::AtomicBool,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl StatusBarrier {
    fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Arc::new(Self { entered: false.into(), resumed: false.into(), resume: Mutex::new(rx) }), tx)
    }

    fn pause_once(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.entered.swap(true, SeqCst) {
            let resumed = tokio::task::block_in_place(|| {
                self.resume.lock().unwrap().recv_timeout(Duration::from_secs(10)).is_ok()
            });
            self.resumed.store(resumed, SeqCst);
        }
    }

    async fn wait(&self) {
        wait_for(|| self.entered.load(std::sync::atomic::Ordering::SeqCst), "status barrier").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_queued_steer_keeps_working_when_the_old_turn_parks() {
    accepted_queued_steer_outcome(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn later_steer_error_does_not_replace_prior_outcome() {
    accepted_queued_steer_outcome(true).await;
}

async fn accepted_queued_steer_outcome(later_error: bool) {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let rig = rig_with_harness(harness).await;
    let (feed_done, _) = warm_fixture(&warm);
    let status = || rig.core.sessions.session_status(CHAT).unwrap();
    let first = run_command("first", &rig.space_path);
    let first_id = match &first { SessionCommandPayload::Run { message_id, .. } => message_id.clone(), _ => unreachable!() };
    rig.core.doc_host.queue_command(CHAT, first).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "warm run to start").await;

    let (before, release_before) = StatusBarrier::new();
    let (after, release_after) = StatusBarrier::new();
    let before_hook = before.clone();
    let after_hook = after.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        if chat == CHAT && event == "before_park_fence" { before_hook.pause_once(); }
        if chat == CHAT && event == "after_park_fence" { after_hook.pause_once(); }
        false
    })));
    feed_done();
    before.wait().await;
    let host = &rig.core.doc_host;
    let id = host.queue_message_with_id(CHAT, "queued-steer", "follow-up", Vec::new(), true).await.unwrap();
    assert!(host.steer_queued_now(CHAT, &id).await.unwrap());
    release_before.send(()).unwrap();
    after.wait().await;
    let parked = status();
    release_after.send(()).unwrap();
    tokens.manager_status().set_test_hook(None);
    assert_eq!(parked.status, mast_proto::SessionStatus::Working);
    assert_eq!(parked.last_completed_turn, None);
    assert_eq!(rig.core.sessions.test_message_turn_outcome(CHAT, &first_id).unwrap(), None);
    wait_for(|| before.resumed.load(std::sync::atomic::Ordering::SeqCst)
        && after.resumed.load(std::sync::atomic::Ordering::SeqCst), "both barriers to resume").await;
    wait_for(|| warm.received.lock().unwrap().iter().any(|p| p == "follow-up"), "follow-up to reach harness").await;
    if later_error {
        warm.feed.lock().unwrap().as_ref().unwrap().send(AgentEvent::Done {
            status: DoneStatus::Errored, result: None, error: Some("later error".into()), session_id: Some("sess-warm".into()),
        }).unwrap();
    } else {
        feed_done();
    }
    let expected = if later_error { DoneStatus::Errored } else { DoneStatus::Completed };
    wait_for(|| rig.core.sessions.test_message_turn_outcome(CHAT, &first_id).ok().flatten() == Some(DoneStatus::Completed)
        && rig.core.sessions.test_message_turn_outcome(CHAT, &id).ok().flatten() == Some(expected), "both accepted messages to have terminal evidence").await;
    if !later_error { assert!(status().last_completed_turn.is_some()); }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_warm_queue_send_cannot_overwrite_its_completed_status() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let rig = rig_with_harness(harness).await;
    let (feed_done, _) = warm_fixture(&warm);
    let status = || rig.core.sessions.session_status(CHAT).unwrap();
    let host = &rig.core.doc_host;
    host.queue_command(CHAT, run_command("first", &rig.space_path)).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "warm run to start").await;
    feed_done();
    wait_for(|| status().status == mast_proto::SessionStatus::Idle, "first turn to park").await;
    let first = status().last_completed_turn.unwrap();

    let (barrier, release) = StatusBarrier::new();
    let hook = barrier.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        if chat == CHAT && event == "after_steer_fence" { hook.pause_once(); }
        false
    })));
    host.queue_message_with_id(CHAT, "warm-next", "warm follow-up", Vec::new(), true).await.unwrap();
    barrier.wait().await;
    wait_for(|| warm.received.lock().unwrap().iter().any(|p| p == "warm follow-up"), "warm queue send to reach harness").await;
    feed_done();
    wait_for(|| status().status == mast_proto::SessionStatus::Idle
        && status().last_completed_turn.as_ref().is_some_and(|id| id != &first),
        "warm follow-up to finish before sender returns").await;
    release.send(()).unwrap();
    let handle = host.open(CHAT).unwrap();
    // The paused warm sender still owns the drain lock until it returns.
    let _drain = handle.test_queue_drain_guard().await;
    tokens.manager_status().set_test_hook(None);
    assert!(barrier.resumed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(status().status, mast_proto::SessionStatus::Idle);
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_steer_recovery_survives_cancelled_drain_and_partial_persistence() {
    use mast_engine::doc_host::{BeginQueueEditOutcome, FinishQueueEditAction, FinishQueueEditOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    let warm = Arc::new(Warm::default());
    warm.hold_steer_confirmation.store(true, SeqCst);
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let rig = rig_with_harness(harness).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    host.queue_command(CHAT, run_command("first", &rig.space_path)).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "warm run to start").await;

    let first = host.queue_message_with_id(CHAT, "recover-first", "first queued", Vec::new(), true).await.unwrap();
    let second = host.queue_message_with_id(CHAT, "recover-second", "second queued", Vec::new(), true).await.unwrap();
    let later = host.queue_message_with_id(CHAT, "queued-later", "original later row", Vec::new(), true).await.unwrap();
    let original = handle.doc().read_queue().unwrap();
    assert!(host.steer_queued_now(CHAT, &first).await.unwrap());
    let warm_id = "warm-run-follow-up";
    let mut request = rig.harness.requests.lock().unwrap()[0].clone();
    request.prompt = "warm run text".into();
    rig.core.sessions.dispatch(CHAT, HarnessId::Mock, request, Some(warm_id.into())).await.unwrap();
    assert!(host.steer_queued_now(CHAT, &second).await.unwrap());
    wait_for(|| warm.received.lock().unwrap().len() == 4, "all three steers to reach the unconfirming harness").await;

    let stage = Arc::new(AtomicUsize::new(0));
    let hook_stage = stage.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        chat == CHAT && match hook_stage.load(SeqCst) {
            0 => event == "queued_steer_recovery_row",
            1 => event == "queued_steer_recovery_flush",
            _ => false,
        }
    })));
    let held = handle.test_queue_drain_guard().await;
    // An explicit Stop must return even while the queue holds its own lock.
    assert!(tokio::time::timeout(Duration::from_secs(5), rig.core.sessions.interrupt(CHAT))
        .await.unwrap().unwrap());
    assert!(!rig.core.sessions.has_live_run(CHAT));
    assert_eq!(handle.doc().read_queue().unwrap().len(), 1);

    let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
    let recovery_host = host.clone();
    let recovery_handle = handle.clone();
    let recovery = tokio::spawn(async move {
        let attempt = recovery_host.drain_queue(&recovery_handle);
        tokio::pin!(attempt);
        assert!(futures::poll!(&mut attempt).is_pending());
        waiting_tx.send(()).unwrap();
        attempt.await;
    });
    waiting_rx.await.unwrap();
    recovery.abort();
    assert!(recovery.await.unwrap_err().is_cancelled());
    drop(held);

    host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec![second.as_str(), later.as_str()]);
    assert!(rows[0].delivery_gate.is_some());
    assert!(host.begin_queued_message_edit(CHAT, &second, "device", "client").await.is_err());
    assert!(!rig.core.sessions.resume_interrupted(CHAT));
    assert!(host.send_queued_now(CHAT, &later).await.is_err());
    assert!(host.update_queued_message(CHAT, &later, "edited during recovery").await.unwrap());

    stage.store(1, SeqCst);
    host.drain_queue(&handle).await;
    assert!(host.remove_queued_message(CHAT, &first).await.is_err());
    assert!(host.steer_queued_now(CHAT, &second).await.is_err());
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 1);

    stage.store(2, SeqCst);
    wait_for(|| host.move_queued_message(CHAT, &first, 0).is_ok(),
        "automatic recovery retry to release the retained owner").await;
    host.drain_queue(&handle).await;
    tokens.manager_status().set_test_hook(None);
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec![first.as_str(), warm_id, second.as_str(), later.as_str()]);
    for (row, before) in [&rows[0], &rows[2]].into_iter().zip(&original) {
        assert!(matches!(row.delivery_gate, Some(mast_doc::QueueDeliveryGate::ReviewRequired { .. })));
        let mut restored = row.clone();
        restored.delivery_gate = None;
        assert_eq!(&restored, before);
    }
    assert_eq!(rows[1].text, "warm run text");
    assert!(rows[1].delivery_gate.is_some());
    assert_eq!(rows[3].text, "edited during recovery");
    let profile = mast_engine::EngineProfile::development(
        &rig._tmp.path().join("data"),
        &std::env::var("MAST_ORG_ID").unwrap_or_else(|_| "dev-org".into()),
        &std::env::var("MAST_USER_ID").unwrap_or_else(|_| "dev-user".into()),
    );
    let store = mast_sync::DocsStore::open(profile.store_root()).unwrap();
    let persisted = loro::LoroDoc::new();
    persisted.import(&store.load_snapshot(CHAT).unwrap().unwrap()).unwrap();
    assert_eq!(mast_doc::SessionDoc::from_doc(persisted).read_queue().unwrap(), rows);
    assert!(!rig.core.sessions.resume_interrupted(CHAT));
    assert_eq!(warm.received.lock().unwrap().len(), 4);

    let BeginQueueEditOutcome::Acquired { lease_id, .. } = host
        .begin_queued_message_edit(CHAT, &first, "device", "client").await.unwrap()
    else { panic!("durably recovered row could not be reviewed"); };
    assert!(matches!(
        host.finish_queued_message_edit(
            CHAT, &first, &lease_id, FinishQueueEditAction::ReleaseUnchanged, None, None,
        ).await.unwrap(),
        FinishQueueEditOutcome::Released
    ));
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 2, "reviewed row to dispatch").await;
    assert_eq!(handle.doc().read_entries().unwrap().iter().filter(|entry| entry.id == first).count(), 1);
    assert_eq!(handle.doc().read_queue().unwrap()[0].id, warm_id);
}

async fn begin_recovered_review(client: &RpcClient, id: &str) -> (String, String) {
    let result = client.call(methods::BEGIN_QUEUED_MESSAGE_EDIT, json!({
        "chatId": CHAT, "id": id, "editorDeviceId": "device", "editorInstanceId": "client"
    })).await.unwrap();
    assert_eq!(result["outcome"], "acquired");
    (result["leaseId"].as_str().unwrap().to_owned(), result["baseTextHash"].as_str().unwrap().to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_failure_recovers_direct_steer_and_warm_run_for_one_reviewed_dispatch() {
    use mast_engine::sessions::SteerOutcome;

    let warm = Arc::new(Warm::default());
    let fail = Arc::new(Notify::new());
    *warm.fail_first_start.lock().unwrap() = Some(fail.clone());
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    host.queue_command(CHAT, run_command("primary", &rig.space_path)).unwrap();
    wait_for(|| !rig.harness.requests.lock().unwrap().is_empty(), "startup to wait for failure").await;

    let direct_id = "startup-direct";
    let warm_id = "startup-warm";
    let discarded_id = "startup-discarded";
    let removed_id = "startup-removed";
    assert_eq!(
        rig.core.sessions.steer(CHAT, "direct follow-up", Some(direct_id.into())).await.unwrap(),
        SteerOutcome::Accepted
    );
    let mut request = rig.harness.requests.lock().unwrap()[0].clone();
    request.prompt = "warm follow-up".into();
    rig.core.sessions.dispatch(CHAT, HarnessId::Mock, request, Some(warm_id.into())).await.unwrap();
    rig.core.sessions.steer(CHAT, "discard this private draft", Some(discarded_id.into())).await.unwrap();
    rig.core.sessions.steer(CHAT, "remove this private draft", Some(removed_id.into())).await.unwrap();
    fail.notify_one();
    wait_for(|| !rig.core.sessions.has_live_run(CHAT), "primary startup to fail").await;
    host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), [direct_id, warm_id, discarded_id, removed_id]);
    assert!(rows.iter().all(|row| row.delivery_gate.is_some()));
    assert_eq!(rows[0].text, "direct follow-up");
    assert_eq!(rows[1].text, "warm follow-up");
    assert!(warm.received.lock().unwrap().is_empty());

    let (cancelled, _) = begin_recovered_review(&rig.client, direct_id).await;
    // Escape and chat switch both issue this ordinary composer RPC.
    let cancel = |lease: &str| json!({
        "chatId": CHAT, "id": direct_id, "leaseId": lease, "action": "cancel", "text": "unsaved draft"
    });
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, cancel(&cancelled))
        .await.unwrap()["outcome"], "cancelled");
    assert_eq!(handle.doc().read_queue().unwrap()[0].text, "direct follow-up");
    assert!(matches!(handle.doc().read_queue().unwrap()[0].delivery_gate,
        Some(mast_doc::QueueDeliveryGate::ReviewRequired { .. })));
    assert!(warm.received.lock().unwrap().is_empty());

    host.pause_all_queues();
    let (drop_lease, _) = begin_recovered_review(&rig.client, discarded_id).await;
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, chat| {
        event == "recovered_steer_disposition_flush" && chat == CHAT
    })));
    let discard = json!({
        "chatId": CHAT, "id": discarded_id, "leaseId": drop_lease, "action": "discard"
    });
    assert!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, discard.clone()).await.is_err());
    tokens.manager_status().set_test_hook(None);
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, discard)
        .await.unwrap()["outcome"], "discarded");
    let (warm_lease, _) = begin_recovered_review(&rig.client, warm_id).await;
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, json!({
        "chatId": CHAT, "id": warm_id, "leaseId": warm_lease, "action": "releaseUnchanged"
    })).await.unwrap()["outcome"], "released");

    let (remove_lease, _) = begin_recovered_review(&rig.client, removed_id).await;
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, json!({
        "chatId": CHAT, "id": removed_id, "leaseId": remove_lease, "action": "releaseUnchanged"
    })).await.unwrap()["outcome"], "released");

    let (before_restart, _) = begin_recovered_review(&rig.client, direct_id).await;
    let Rig { _tmp, core, client, harness, service, .. } = rig;
    drop(handle);
    drop(client);
    drop(service);
    core.shutdown().await;
    drop(core);

    let registry = HarnessRegistry::new();
    let requests = harness.requests.clone();
    registry.register(Arc::new(harness));
    let restarted = EngineCore::assemble(
        &_tmp.path().join("data"), Arc::new(registry), HarnessId::Mock, None,
    ).unwrap();
    restarted.sessions.set_agent_tools_launch(Some(AgentToolsLaunch {
        command: "/opt/zeron/bin/zeron".into(), args: vec!["mcp".into()], ipc_port: Some(27999),
    }));
    let host = &restarted.doc_host;
    let handle = host.open(CHAT).unwrap();
    // Approved warm follow-up still owns its transcript identity after
    // restart. Legacy whitespace removal must discard the same User row.
    let changed = "new warm draft after restart";
    assert!(host.update_queued_message(CHAT, warm_id, changed).await.unwrap());
    assert_eq!(handle.doc().read_entries().unwrap().iter()
        .find(|entry| entry.id == warm_id).unwrap().parts[0],
        mast_doc::MessagePart::Text { id: "t0".into(), text: changed.into() });
    assert!(host.update_queued_message(CHAT, warm_id, "   ").await.unwrap());
    let client = mast_rpc::memory_client(restarted.rpc_service());
    let tokens = restarted.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, chat| {
        event == "recovered_steer_disposition_flush" && chat == CHAT
    })));
    let remove = json!({ "chatId": CHAT, "id": removed_id });
    assert!(client.call(methods::REMOVE_QUEUED_MESSAGE, remove.clone()).await.is_err());
    tokens.manager_status().set_test_hook(None);
    assert_eq!(client.call(methods::REMOVE_QUEUED_MESSAGE, remove).await.unwrap()["removed"], true);
    for id in [warm_id, discarded_id, removed_id] {
        let entries = handle.doc().read_entries().unwrap();
        let discarded = entries.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(discarded.role, mast_doc::MessageRole::System);
        assert_eq!(discarded.parts[0], mast_doc::MessagePart::Text {
            id: "t0".into(), text: "[Mast system message: queued follow-up discarded.]".into(),
        });
    }

    // Exercise the saved lease after a fresh engine owns the queue.
    let release = |lease: &str| json!({
        "chatId": CHAT, "id": direct_id, "leaseId": lease, "action": "releaseUnchanged"
    });
    assert_eq!(client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, release(&cancelled))
        .await.unwrap()["outcome"], "lost");
    assert_eq!(client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, cancel(&before_restart))
        .await.unwrap()["outcome"], "cancelled");

    let (expired, _) = begin_recovered_review(&client, direct_id).await;
    let row = &handle.doc().read_queue().unwrap()[0];
    let mut gate = row.delivery_gate.clone().unwrap();
    let mast_doc::QueueDeliveryGate::Editing { expires_at_ms, .. } = &mut gate else { panic!("edit lease missing"); };
    *expires_at_ms = 1;
    handle.doc().set_queued_delivery_gate(direct_id, Some(&gate)).unwrap();
    assert_eq!(client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, release(&expired))
        .await.unwrap()["outcome"], "lost");
    assert!(matches!(handle.doc().read_queue().unwrap()[0].delivery_gate,
        Some(mast_doc::QueueDeliveryGate::ReviewRequired { .. })));
    assert!(warm.received.lock().unwrap().is_empty());

    // A restarted chat can retain a paused queue. Saving review clears the
    // row's gate, then the user's Send Now releases exactly that row.
    host.pause_all_queues();
    let (lease, hash) = begin_recovered_review(&client, direct_id).await;
    let attachment = _tmp.path().join("new-reference.png");
    std::fs::write(&attachment, b"fixture").unwrap();
    let attachments = vec![attachment.to_string_lossy().into_owned()];
    let approved = "Use the revised instructions and inspect the new attachment";
    let expected = mast_doc::queued_message_prompt(approved, &attachments);
    let commit = json!({
        "chatId": CHAT, "id": direct_id, "leaseId": lease, "action": "commit",
        "text": approved, "expectedTextHash": hash, "attachments": attachments
    });
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, chat| {
        event == "recovered_steer_disposition_flush" && chat == CHAT
    })));
    assert!(client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, commit.clone()).await.is_err());
    assert!(host.send_queued_now(CHAT, direct_id).await.is_err());
    tokens.manager_status().set_test_hook(None);
    assert_eq!(client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, commit)
        .await.unwrap()["outcome"], "committed");
    assert!(host.send_queued_now(CHAT, direct_id).await.unwrap());
    wait_for(|| !warm.received.lock().unwrap().is_empty(), "reviewed follow-up to dispatch").await;
    assert_eq!(&*warm.received.lock().unwrap(), &[expected.clone()]);
    assert_eq!(requests.lock().unwrap().last().unwrap().attachments, attachments);
    let entries = handle.doc().read_entries().unwrap();
    let mut canonical = entries.iter().filter(|entry| entry.id == direct_id);
    assert_eq!(canonical.next().unwrap().parts[0], mast_doc::MessagePart::Text { id: "t0".into(), text: expected });
    assert!(canonical.next().is_none());
    assert!(handle.doc().read_queue().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewed_queue_dispatch_can_retry_before_the_queue_caller_returns() {
    use std::sync::atomic::Ordering::SeqCst;

    let warm = Arc::new(Warm::default());
    warm.hold_steer_confirmation.store(true, SeqCst);
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    host.queue_command(CHAT, run_command("open the native session", &rig.space_path)).unwrap();
    wait_for(|| rig.core.workspace.chat(CHAT).unwrap().is_some_and(|chat|
        chat.harness_session_id.as_deref() == Some("sess-warm")), "first native session").await;

    let id = "review-startup-retry";
    rig.core.sessions.steer(CHAT, "unconfirmed original prompt", Some(id.into())).await.unwrap();
    wait_for(|| warm.received.lock().unwrap().len() == 2, "the unconfirmed steer").await;
    assert!(rig.core.sessions.interrupt(CHAT).await.unwrap());
    host.pause_all_queues();
    host.drain_queue(&handle).await;
    let approved = "reviewed prompt that should retry";
    let (lease, hash) = begin_recovered_review(&rig.client, id).await;
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, json!({
        "chatId": CHAT, "id": id, "leaseId": lease, "action": "commit",
        "text": approved, "expectedTextHash": hash,
    })).await.unwrap()["outcome"], "committed");

    let fail_start = Arc::new(Notify::new());
    *warm.fail_before_session_start.lock().unwrap() = Some(fail_start.clone());
    let (paused, resume) = StatusBarrier::new();
    let hook = paused.clone();
    let tokens = rig.core.sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        if event == "queued_dispatch_return" && chat == CHAT {
            hook.pause_once();
        }
        false
    })));
    let dispatch_host = host.clone();
    let send = tokio::spawn(async move { dispatch_host.send_queued_now(CHAT, id).await });
    paused.wait().await;
    fail_start.notify_one();
    wait_for(|| rig.harness.requests.lock().unwrap().len() == 3 || rig.core.sessions.session_status(CHAT)
        .is_some_and(|session| session.status == mast_proto::SessionStatus::Errored),
        "startup retry while the original queue call is paused").await;
    let requests = rig.harness.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3, "the accepted run must still own its startup retry");
    assert_eq!(requests[1].resume.as_deref(), Some("sess-warm"));
    assert_eq!(requests[2].resume.as_deref(), Some("sess-warm"));
    wait_for(|| warm.received.lock().unwrap().iter().any(|message| message == approved),
        "the reviewed prompt to reach the retry's harness").await;
    assert!(!send.is_finished());
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert_eq!(handle.doc().read_entries().unwrap().iter().filter(|entry| entry.id == id).count(), 1);
    assert_eq!(warm.received.lock().unwrap().iter().filter(|message| *message == approved).count(), 1);
    resume.send(()).unwrap();
    assert!(send.await.unwrap().unwrap());
    tokens.manager_status().set_test_hook(None);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewed_steer_returns_to_the_queue_until_fresh_or_warm_admission_succeeds() {
    use std::sync::atomic::Ordering::SeqCst;

    let warm = Arc::new(Warm::default());
    warm.hold_steer_confirmation.store(true, SeqCst);
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    let sessions = &rig.core.sessions;
    host.queue_command(CHAT, run_command("opening turn", &rig.space_path)).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "first native session").await;

    let id = "review-admission-failure";
    sessions.steer(CHAT, "original queued prompt", Some(id.into())).await.unwrap();
    wait_for(|| warm.received.lock().unwrap().len() == 2, "the unconfirmed steer").await;
    assert!(sessions.interrupt(CHAT).await.unwrap());
    host.pause_all_queues();
    host.drain_queue(&handle).await;
    let approved = "the reviewed follow-up";
    let (lease, hash) = begin_recovered_review(&rig.client, id).await;
    assert_eq!(rig.client.call(methods::FINISH_QUEUED_MESSAGE_EDIT, json!({
        "chatId": CHAT, "id": id, "leaseId": lease, "action": "commit",
        "text": approved, "expectedTextHash": hash,
    })).await.unwrap()["outcome"], "committed");

    let tokens = sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, chat| {
        event == "recovered_steer_admission" && chat == CHAT
    })));
    assert!(host.send_queued_now(CHAT, id).await.unwrap_err().to_string().contains("ownership transfer failure"));
    assert!(!sessions.has_live_run(CHAT));
    assert!(handle.doc().has_queued_id(id));
    assert!(handle.doc().is_recovered_steer(id));
    assert_eq!(rig.harness.requests.lock().unwrap().len(), 1);

    let mut request = sessions.last_request(CHAT).unwrap();
    request.prompt = "start an unrelated warm turn".into();
    sessions.dispatch(CHAT, HarnessId::Mock, request, Some("new-warm-turn".into())).await.unwrap();
    wait_for(|| warm.received.lock().unwrap().len() == 3, "unrelated warm turn to start").await;
    assert!(host.steer_queued_now(CHAT, id).await.unwrap_err().to_string().contains("ownership transfer failure"));
    assert!(handle.doc().has_queued_id(id));
    assert!(handle.doc().is_recovered_steer(id));
    assert!(warm.received.lock().unwrap().iter().all(|message| message != approved));

    tokens.manager_status().set_test_hook(None);
    assert!(host.steer_queued_now(CHAT, id).await.unwrap());
    wait_for(|| warm.received.lock().unwrap().iter().any(|message| message == approved),
        "the approved follow-up to reach the warm mailbox").await;
    assert!(!handle.doc().has_queued_id(id));
    assert!(!handle.doc().is_recovered_steer(id));
    assert!(sessions.interrupt(CHAT).await.unwrap());
    host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].text, approved);
    assert!(rows[0].delivery_gate.is_some());
    assert!(handle.doc().is_recovered_steer(id));
    assert_eq!(handle.doc().read_entries().unwrap().iter().filter(|entry| entry.id == id).count(), 1);
    assert_eq!(warm.received.lock().unwrap().iter().filter(|message| *message == approved).count(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_update_checks_provenance_after_a_concurrent_recovery() {
    use std::sync::atomic::Ordering::SeqCst;

    let warm = Arc::new(Warm::default());
    warm.hold_steer_confirmation.store(true, SeqCst);
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    let sessions = &rig.core.sessions;
    host.queue_command(CHAT, run_command("before the legacy edit", &rig.space_path)).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "warm harness").await;
    let id = "legacy-concurrent-recovery";
    sessions.steer(CHAT, "original text", Some(id.into())).await.unwrap();
    wait_for(|| warm.received.lock().unwrap().len() == 2, "unconfirmed queue input").await;
    assert!(!handle.doc().is_recovered_steer(id));

    let (paused, resume) = StatusBarrier::new();
    let hook = paused.clone();
    let tokens = sessions.agent_tokens();
    tokens.manager_status().set_test_hook(Some(Arc::new(move |event, chat| {
        if chat != CHAT { return false; }
        if event == "before_queue_text_edit_drain" {
            hook.pause_once();
        }
        event == "recovered_steer_disposition_flush"
    })));
    let updated = "updated during concurrent recovery";
    let update = json!({ "chatId": CHAT, "id": id, "text": updated });
    let client = mast_rpc::memory_client(rig.core.rpc_service());
    let in_flight = update.clone();
    let rpc = tokio::spawn(async move { client.call(methods::UPDATE_QUEUED_MESSAGE, in_flight).await });
    paused.wait().await;
    assert!(sessions.interrupt(CHAT).await.unwrap());
    host.drain_queue(&handle).await;
    assert!(handle.doc().is_recovered_steer(id));
    resume.send(()).unwrap();
    let error = rpc.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("disposition flush failure"));

    tokens.manager_status().set_test_hook(None);
    rig.client.call(methods::UPDATE_QUEUED_MESSAGE, update).await.unwrap();
    let row = &handle.doc().read_queue().unwrap()[0];
    assert_eq!(row.id, id);
    assert_eq!(row.text, updated);
    assert!(row.delivery_gate.is_some());
    let entries = handle.doc().read_entries().unwrap();
    assert_eq!(entries.iter().filter(|entry| entry.id == id).count(), 1);
    assert_eq!(entries.iter().find(|entry| entry.id == id).unwrap().parts[0],
        mast_doc::MessagePart::Text { id: "t0".into(), text: updated.into() });
    assert_eq!(warm.received.lock().unwrap().len(), 2);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_warm_exit_interrupts_prior_turn_and_retains_pending_steer() {
    use std::sync::atomic::Ordering::SeqCst;
    let warm = Arc::new(Warm::default());
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let first = run_command("primary", &rig.space_path);
    let first_id = match &first { SessionCommandPayload::Run { message_id, .. } => message_id.clone(), _ => unreachable!() };
    host.queue_command(CHAT, first).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "primary run to start").await;
    warm.hold_steer_confirmation.store(true, SeqCst);
    let id = host.queue_message_with_id(CHAT, "exit-pending", "pending follow-up", Vec::new(), true).await.unwrap();
    assert!(host.steer_queued_now(CHAT, &id).await.unwrap());
    wait_for(|| warm.received.lock().unwrap().iter().any(|p| p == "pending follow-up"), "steer to reach harness").await;
    warm.feed.lock().unwrap().as_ref().unwrap().send(AgentEvent::Done {
        status: DoneStatus::Errored, result: None, error: Some("exit".into()), session_id: None,
    }).unwrap();
    wait_for(|| rig.core.sessions.test_message_turn_outcome(CHAT, &first_id).ok().flatten() == Some(DoneStatus::Interrupted), "retirement to commit interruption").await;
    let handle = host.open(CHAT).unwrap();
    host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert!(rows.iter().any(|row| row.id == id && row.delivery_gate.is_some()));
    assert_eq!(warm.received.lock().unwrap().iter().filter(|p| *p == "pending follow-up").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_driver_recovers_only_unconfirmed_steers() {
    use std::sync::atomic::Ordering::SeqCst;

    let warm = Arc::new(Warm::default());
    let rig = rig_with_harness(GatedHarness { warm: Some(warm.clone()), ..Default::default() }).await;
    let host = &rig.core.doc_host;
    let handle = host.open(CHAT).unwrap();
    host.queue_command(CHAT, run_command("primary", &rig.space_path)).unwrap();
    wait_for(|| warm.feed.lock().unwrap().is_some(), "primary run to start").await;
    let sessions = &rig.core.sessions;
    let first_start = sessions.session_status(CHAT).unwrap().started_at;
    sessions.steer(CHAT, "confirmed steer", Some("confirmed-steer".into())).await.unwrap();
    let tokens = sessions.agent_tokens();
    // The confirmed steer has its own work-start timestamp, assigned by the
    // engine only after it has retired that entry from the ledger.
    wait_for(|| sessions.session_status(CHAT).unwrap().started_at != first_start,
        "the engine to confirm the first steer").await;
    warm.hold_steer_confirmation.store(true, SeqCst);
    sessions.steer(CHAT, "needs review", Some("unconfirmed-steer".into())).await.unwrap();
    wait_for(|| warm.received.lock().unwrap().len() == 3, "unconfirmed steer to reach the harness").await;
    tokens.manager_status().set_test_hook(Some(Arc::new(|event, chat| {
        if event == "before_park_fence" && chat == CHAT {
            panic!("injected driver failure");
        }
        false
    })));
    warm_fixture(&warm).0();
    wait_for(|| !sessions.has_live_run(CHAT), "the unwound driver to retire").await;
    tokens.manager_status().set_test_hook(None);
    host.drain_queue(&handle).await;
    let rows = handle.doc().read_queue().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "unconfirmed-steer");
    assert!(rows[0].delivery_gate.is_some());
    assert_eq!(warm.received.lock().unwrap().len(), 3);
}

/// A warm run keeps its token while parked. A change made with it between turns
/// belongs to no turn, yet it is still a change the status must follow: it owes,
/// and the reminder is steered into the parked run.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_on_a_parked_runs_token_between_turns_still_owes_a_status() {
    let warm = Arc::new(Warm::default());
    let harness = GatedHarness { warm: Some(warm.clone()), ..Default::default() };
    let (rig, _item) = role_rig_in(rig_with_harness(harness).await, "status-idle-token").await;
    let (feed_done, reminders) = warm_fixture(&warm);
    let status_is = |want: mast_proto::SessionStatus| rig.core.sessions.session_status(CHAT).map(|s| s.status) == Some(want);

    rig.core.doc_host.queue_command(CHAT, run_command("turn one", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1 && status_is(mast_proto::SessionStatus::Working), "turn one").await;
    feed_done();
    wait_for(|| status_is(mast_proto::SessionStatus::Idle), "the park").await;
    assert_eq!(reminders(), 0);

    // No turn is running, yet the parked run's token creates a work item.
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Between turns", "idempotencyKey": "it1" }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reminders(), 0);
}

/// Rotation immediately after successor changes does not create reminders.
#[tokio::test(flavor = "multi_thread")]
async fn immediate_rotation_does_not_trigger_a_reminder() {
    let (rig, item) = role_rig("status-adopt-fast").await;
    plant_stamped_status(&rig, "status-adopt-fast-doc", &item, 4).await;
    let successor = "fast-successor";
    rig.core
        .workspace
        .create_chat(successor, Some(SPACE), Some(&rig.core.device_id), None, Some(rig.space_path.clone()))
        .unwrap();
    assign_chat(&rig, "status-fast-chat-assignment", successor, &item).await;
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, successor, "op-fast", None).unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    for n in 0..12 {
        let lease = tokens.mint(successor, &format!("run-fast-{n}"), SPACE, &rig.core.device_id);
        tool(
            &rig,
            lease.token(),
            "work_outline_create_child",
            json!({ "title": format!("Worker {n}"), "idempotencyKey": format!("fast-{n}") }),
        )
        .await
        .unwrap();
        drop(lease);
    }
    // No pause: the role moves while the turns may still be unexamined.
    rig.core.workspace.rotate_standing_role_for_chat(CHAT, successor).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(nudge_prompts(&rig).is_empty());
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 0);
}

/// A worker chat's own dispatches are not a manager's: no reminder, and the
/// tracker does not keep the chat's turn open afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_that_is_not_a_manager_is_never_reminded() {
    let rig = rig().await;
    let item = create_item(&rig, "status-plain-item", "Plain", None).await;
    assign_chat(&rig, "status-plain-assignment", CHAT, &item).await;
    rig.core.doc_host.queue_command(CHAT, run_command("go", &rig.space_path)).unwrap();
    wait_for(|| rig.harness.spawns.lock().unwrap().len() == 1, "the run").await;
    let token = run_token(&rig, 0);
    tool(&rig, &token, "work_outline_create_child", json!({ "title": "Child", "idempotencyKey": "n1" }))
        .await
        .unwrap();
    rig.harness.release.notify_one();
    let tokens = rig.core.sessions.agent_tokens();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.harness.spawns.lock().unwrap().len(), 1);
    assert_eq!(nudges(&rig), 0);
}

const HAND_CHAT: &str = "chat-hand-started";
const OUTSIDE_CHAT: &str = "chat-other-project";

/// CHAT holds the manager role of the project; HAND_CHAT was started by hand
/// on a ticket inside it and OUTSIDE_CHAT on a ticket of the other project.
async fn manager_rig() -> (Rig, ProjectRig) {
    use mast_proto::StandingRoleKind;
    let rig = rig().await;
    let p = project_rig(&rig).await;
    assign_chat(&rig, "mg-manager", CHAT, &p.planning).await;
    for (chat, item) in [(HAND_CHAT, &p.ticket), (OUTSIDE_CHAT, &p.other_tickets)] {
        rig.client
            .call(
                methods::MUTATE,
                json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE, "deviceId": rig.core.device_id }),
            )
            .await
            .unwrap();
        assign_chat(&rig, &format!("mg-{chat}"), chat, item).await;
    }
    rig.core.workspace.adopt_standing_role(StandingRoleKind::Manager, &p.project, CHAT, &[]).unwrap();
    (rig, p)
}

fn mint_token(rig: &Rig, chat: &str) -> (mast_engine::agent_tools::AgentTokenLease, String) {
    let lease = rig.core.sessions.agent_tokens().mint(chat, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    (lease, token)
}

fn ids(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_manager_cannot_start_commands_until_the_project_turns_it_off() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let call = |key: &'static str| {
        tool(&rig, &token, "run_command", json!({ "command": "true", "idempotencyKey": key }))
    };
    let error = call("mgr-cmd").await.expect_err("a Manager's commands would bypass its read-only sandbox");
    assert!(error.to_string().contains("read-only Manager"), "{error}");

    let set = |read_only: bool, op: &str| rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({
        "op": "setManagerReadOnly", "workItemId": p.project, "readOnly": read_only, "operationId": op,
    }));
    set(false, "cmd-off").await.unwrap();
    // The chat's own sandbox and approvals now decide, as for any chat.
    let config = mast_proto::ChatConfig {
        approval_rules: false,
        harness: HarnessId::Mock,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
    };
    rig.core.workspace.set_chat_config(CHAT, &config).unwrap();
    call("mgr-cmd-off").await.expect("with the setting off the Manager can start a command");
    set(true, "cmd-on").await.unwrap();
    let error = call("mgr-cmd-on").await.expect_err("back on");
    assert!(error.to_string().contains("read-only Manager"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_projects_worker_model_applies_to_the_managers_next_worker_and_survives_a_hand_off() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let args = |key: &str| json!({
        "idempotencyKey": key, "workItemId": p.ticket, "prompt": "go",
        "harnessOmitted": true, "modelOmitted": true,
        "config": { "harness": "mock", "model": null, "reasoning": null,
                    "modelOptions": {}, "sandbox": "read-only", "autoApprove": false },
    });
    let child_model = |key: &str| {
        let child = derive_id(&mast_proto::agent_tools::operation_id(CHAT, key), roles::CHAT);
        rig.core.workspace.chat_config(&child).map(|config| (config.harness, config.model))
    };
    let set = |model: Value, op: &str| rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({
        "op": "setManagerWorkerModel", "workItemId": p.project, "workerModel": model, "operationId": op,
    }));
    // The rig installs only the mock harness, so the project picks a mock model.
    set(json!({ "harness": "mock", "model": "model-a" }), "wm-a").await.unwrap();
    tool(&rig, &token, "chat_launch_child_for_item", args("wm-a-1")).await.unwrap();
    assert_eq!(child_model("wm-a-1"), Some((HarnessId::Mock, Some("model-a".into()))));
    set(json!({ "harness": "mock", "model": "model-b" }), "wm-b").await.unwrap();
    tool(&rig, &token, "chat_launch_child_for_item", args("wm-b-1")).await.unwrap();
    assert_eq!(child_model("wm-b-1"), Some((HarnessId::Mock, Some("model-b".into()))));
    assert_eq!(child_model("wm-a-1"), Some((HarnessId::Mock, Some("model-a".into()))), "earlier workers keep theirs");

    // The successor holds the same role record, so the setting carries over.
    rig.core.workspace.reserve_standing_role_hand_off(CHAT, HAND_CHAT, "wm-handoff", None).unwrap();
    rig.core.workspace.rotate_standing_role_for_chat(CHAT, HAND_CHAT).unwrap();
    let (_lease2, token2) = mint_token(&rig, HAND_CHAT);
    tool(&rig, &token2, "chat_launch_child_for_item", args("wm-b-2")).await.unwrap();
    let child = derive_id(&mast_proto::agent_tools::operation_id(HAND_CHAT, "wm-b-2"), roles::CHAT);
    let config = rig.core.workspace.chat_config(&child).unwrap();
    assert_eq!(config.model.as_deref(), Some("model-b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_reaches_the_chats_of_its_project_and_no_others() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    assert_eq!(auth_origin(&rig, &token).await["managerScopeItemId"], p.project.as_str());

    let scope = tool(&rig, &token, "chat_project_scope_chats", json!({})).await.unwrap();
    let managed = ids(&scope["managerChatIds"]);
    assert!(managed.contains(&HAND_CHAT.to_owned()), "{managed:?}");
    assert!(!managed.contains(&OUTSIDE_CHAT.to_owned()) && !managed.contains(&CHAT.to_owned()), "{managed:?}");

    let info = tool(&rig, &token, "chat_archive_info", json!({})).await.unwrap();
    assert!(info["chats"].get(HAND_CHAT).is_some() && info["chats"].get(OUTSIDE_CHAT).is_none(), "{info}");
    let refused = tool(&rig, &token, "chat_archive", json!({ "chatId": OUTSIDE_CHAT })).await.unwrap_err();
    assert!(refused.contains("can only archive"), "{refused}");
    // In scope: the finished-worker check applies, not the scope refusal.
    match tool(&rig, &token, "chat_archive", json!({ "chatId": HAND_CHAT })).await {
        Ok(_) => {}
        Err(e) => assert!(e.contains("not archived:"), "{e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_with_no_manager_role_gets_no_project_reach() {
    let (rig, _p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, OTHER_CHAT);
    assert!(auth_origin(&rig, &token).await.get("managerScopeItemId").is_none());
    let scope = tool(&rig, &token, "chat_project_scope_chats", json!({})).await.unwrap();
    assert!(ids(&scope["managerChatIds"]).is_empty());
    let err = admin(&rig, &token, json!({ "read": true })).await.unwrap_err();
    assert!(err.contains("does not hold a project manager role"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_edits_only_its_projects_outline() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let step = |op: Value| json!({ "idempotencyKey": format!("mg-{}", op), "steps": [op] });

    admin(&rig, &token, step(json!({ "op": "rename", "itemId": p.ticket, "title": "Renamed" }))).await.unwrap();
    assert_eq!(title_of(&rig, &p.ticket), "Renamed");
    // A ref created inside the project is inside.
    admin(&rig, &token, json!({ "idempotencyKey": "mg-create", "steps": [
        { "op": "create", "parentId": p.project, "title": "New", "ref": "n" },
        { "op": "rename", "itemId": "$n", "title": "New 2" },
    ]})).await.unwrap();

    for (what, op) in [
        ("rename outside", json!({ "op": "rename", "itemId": p.other_tickets, "title": "X" })),
        ("rename root item", json!({ "op": "rename", "itemId": p.bystander, "title": "X" })),
        ("create at the root", json!({ "op": "create", "title": "X" })),
        ("create elsewhere", json!({ "op": "create", "parentId": p.other, "title": "X" })),
        ("move out", json!({ "op": "move", "itemId": p.ticket, "parentId": p.other })),
        ("move to the root", json!({ "op": "move", "itemId": p.ticket, "parentId": null })),
        ("archive the project", json!({ "op": "archive", "itemId": p.project })),
        ("space-wide instruction", json!({ "op": "instruction_add", "itemId": null, "text": "x" })),
        ("steal a chat", json!({ "op": "assign_chat", "chatId": OUTSIDE_CHAT, "itemId": p.ticket })),
        ("accept a proposal", json!({ "op": "proposal_accept", "proposalId": "abcdef123" })),
    ] {
        let err = admin(&rig, &token, step(op)).await.unwrap_err();
        assert!(!err.is_empty(), "{what}");
    }
    assert_eq!(title_of(&rig, &p.other_tickets), "Other tickets");
    assert_eq!(title_of(&rig, &p.bystander), "Bystander");
    assert!(snapshot(&rig).item(&p.ticket).unwrap().archived_at.is_none());
    let err = admin(&rig, &token, step(json!({ "op": "rename", "itemId": p.other_tickets, "title": "X" }))).await.unwrap_err();
    assert!(err.contains("outside your project"), "{err}");

    let view = admin(&rig, &token, json!({ "read": true })).await.unwrap();
    assert_eq!(view["roots"].as_array().unwrap().len(), 1);
    assert_eq!(view["roots"][0]["id"], p.project.as_str());
    assert!(!view.to_string().contains("Bystander"), "{view}");
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_admin_batch_retries_without_status_tracking() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let tokens = rig.core.sessions.agent_tokens();
    let tracker = tokens.manager_status();
    let start = tracker.generation(CHAT);
    let steps = json!({"idempotencyKey":"interrupted-status", "steps":[
        {"op":"rename", "itemId":p.ticket, "title":"First committed"},
        {"op":"rename", "itemId":p.tickets, "title":"Second committed"}
    ]});
    tracker.set_test_hook(Some(Arc::new(|event, _| event == "admin_after_commit")));
    assert!(admin(&rig, &token, steps.clone()).await.is_err());
    tracker.set_test_hook(None);
    assert_eq!(tracker.generation(CHAT), start);
    assert_eq!(title_of(&rig, &p.ticket), "First committed");
    rig.core.workspace.flush();
    rig.service.work_outline().clear_recent_for_test();
    admin(&rig, &token, steps.clone()).await.unwrap();
    assert_eq!(tracker.generation(CHAT), start, "disabled tracking retains no generation");
    admin(&rig, &token, steps).await.unwrap();
    assert_eq!(tracker.generation(CHAT), start, "durable replay retains no generation");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_commits_and_approval_do_not_create_status_tracking() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let tokens = rig.core.sessions.agent_tokens();
    let generation = || tokens.manager_status().generation(CHAT);
    let start = generation();
    let rename = json!({ "idempotencyKey": "status-rename", "steps": [
        {"op": "rename", "itemId": p.ticket, "title": "Committed"}
    ]});
    admin(&rig, &token, rename.clone()).await.unwrap();
    assert_eq!(generation(), start);
    // A lost response and retry reaches the durable replay boundary.
    admin(&rig, &token, rename).await.unwrap();
    assert_eq!(generation(), start);
    admin(&rig, &token, json!({ "idempotencyKey": "same-title", "steps": [
        {"op": "rename", "itemId": p.ticket, "title": "Committed"}
    ]})).await.unwrap();
    assert_eq!(generation(), start, "a no-op creates no tracking state");
    let steps: Vec<Value> = (0..11).map(|i| json!({"op":"rename", "itemId":p.ticket, "title":format!("Bulk {i}")})).collect();
    let preview = json!({"idempotencyKey":"status-bulk", "dryRun":true, "steps":steps});
    let asked = admin(&rig, &token, preview.clone()).await.unwrap();
    assert_eq!(asked["needsApproval"], true);
    assert_eq!(generation(), start, "creating a Required Action creates no tracking state");
    let repeated = admin(&rig, &token, preview).await.unwrap();
    assert_eq!(asked["requestId"], repeated["requestId"]);
    assert_eq!(generation(), start, "replayed approval creates no tracking state");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_managers_bulk_change_still_needs_the_users_approval() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let steps: Vec<Value> = (0..11)
        .map(|i| json!({ "op": "rename", "itemId": p.ticket, "title": format!("Bulk {i}") }))
        .collect();
    let reply = admin(&rig, &token, json!({ "idempotencyKey": "mg-bulk", "steps": steps })).await.unwrap();
    assert_eq!(reply["needsApproval"], true, "{reply}");
    assert_eq!(title_of(&rig, &p.ticket), "Ticket 1");
    assert!(snapshot(&rig).user_action_requests.iter().any(|r| r.text.contains("by project manager")));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_manager_grant_moves_to_the_successor_and_leaves_the_predecessor() {
    let (rig, p) = manager_rig().await;
    let (_old_lease, old) = mint_token(&rig, CHAT);
    let reply = rig
        .client
        .call(methods::HAND_OFF_ORCHESTRATOR, json!({ "chatId": CHAT, "summary": "Continue", "operationId": "mg-handoff" }))
        .await
        .unwrap();
    let successor = reply["chatId"].as_str().unwrap().to_owned();
    let (_new_lease, new) = mint_token(&rig, &successor);

    // The predecessor keeps its (still valid) token but not the reach.
    assert!(auth_origin(&rig, &old).await.get("managerScopeItemId").is_none());
    let err = admin(&rig, &old, json!({ "read": true })).await.unwrap_err();
    assert!(err.contains("does not hold a project manager role"), "{err}");
    let old_scope = tool(&rig, &old, "chat_project_scope_chats", json!({})).await.unwrap();
    assert!(ids(&old_scope["managerChatIds"]).is_empty());

    assert_eq!(auth_origin(&rig, &new).await["managerScopeItemId"], p.project.as_str());
    let new_scope = tool(&rig, &new, "chat_project_scope_chats", json!({})).await.unwrap();
    let managed = ids(&new_scope["managerChatIds"]);
    assert!(managed.contains(&HAND_CHAT.to_owned()));
    // The archived predecessor stays reachable through its role's chain.
    assert!(managed.contains(&CHAT.to_owned()), "{managed:?}");
    admin(&rig, &new, json!({ "idempotencyKey": "mg-after", "steps": [
        { "op": "rename", "itemId": p.ticket, "title": "After hand-off" }
    ]})).await.unwrap();
    assert_eq!(title_of(&rig, &p.ticket), "After hand-off");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_managers_child_filed_in_another_project_is_not_reachable() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "chat-moved-child", "spaceId": SPACE,
                    "deviceId": rig.core.device_id, "parentChatId": CHAT }),
        )
        .await
        .unwrap();
    assign_chat(&rig, "mg-moved", "chat-moved-child", &p.other_tickets).await;
    let scope = tool(&rig, &token, "chat_project_scope_chats", json!({})).await.unwrap();
    assert!(!ids(&scope["managerChatIds"]).contains(&"chat-moved-child".to_owned()));
    let info = tool(&rig, &token, "chat_archive_info", json!({})).await.unwrap();
    assert!(info["chats"].get("chat-moved-child").is_none(), "{info}");
    let refused = tool(&rig, &token, "chat_archive", json!({ "chatId": "chat-moved-child" })).await.unwrap_err();
    assert!(refused.contains("can only archive"), "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_writer_rechecks_the_manager_guard_under_its_lock() {
    use mast_engine::work_outline::ManagerGuard;
    use mast_proto::{ArchiveWorkItemParams, DescendantArchivePolicy, WorkOutlineMutation};
    let (rig, p) = manager_rig().await;
    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let archive = |op: &str, item: &str| {
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: item.to_owned(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: op.to_owned(),
        })
    };
    let guard = |chat: &str, inside: Vec<String>, strict: Vec<String>| ManagerGuard {
        chat_id: chat.to_owned(),
        root: p.project.clone(),
        inside,
        strict,
        ..Default::default()
    };
    // A target that is outside the project (it moved out after the check).
    let err = outline
        .mutate_guarded(&archive("g-out", &p.ticket), &actor, Some(SPACE), Some(&guard(CHAT, vec![], vec![p.other_tickets.clone()])))
        .unwrap_err();
    assert!(err.to_wire_message().contains("no longer inside"), "{err:?}");
    // The project item itself where a strict descendant is required.
    let err = outline
        .mutate_guarded(&archive("g-root", &p.ticket), &actor, Some(SPACE), Some(&guard(CHAT, vec![], vec![p.project.clone()])))
        .unwrap_err();
    assert!(err.to_wire_message().contains("no longer inside"), "{err:?}");
    // A chat that does not hold the role (a predecessor or a role-less chat).
    let err = outline
        .mutate_guarded(&archive("g-holder", &p.ticket), &actor, Some(SPACE), Some(&guard(OTHER_CHAT, vec![], vec![p.ticket.clone()])))
        .unwrap_err();
    assert!(err.to_wire_message().contains("handed off or ended"), "{err:?}");
    assert!(snapshot(&rig).item(&p.ticket).unwrap().archived_at.is_none());
    // The same write passes when the holder and the target are right.
    outline
        .mutate_guarded(&archive("g-ok", &p.ticket), &actor, Some(SPACE), Some(&guard(CHAT, vec![p.tickets.clone()], vec![p.ticket.clone()])))
        .unwrap();
    assert!(snapshot(&rig).item(&p.ticket).unwrap().archived_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_never_edits_the_project_item_but_adds_under_it() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for (what, op) in [
        ("rename", json!({ "op": "rename", "itemId": p.project, "title": "X" })),
        ("describe", json!({ "op": "describe", "itemId": p.project, "description": "X" })),
        ("status", json!({ "op": "status", "itemId": p.project, "status": "completed" })),
        ("destination", json!({ "op": "destination", "itemId": p.project, "destinationId": null })),
        ("instruction", json!({ "op": "instruction_add", "itemId": p.project, "text": "x" })),
        ("template", json!({ "op": "template_set", "itemId": p.project, "template": null })),
    ] {
        let err = admin(&rig, &token, json!({ "idempotencyKey": format!("mg-root-{what}"), "steps": [op] })).await.unwrap_err();
        assert!(err.contains("project item itself"), "{what}: {err}");
    }
    assert_eq!(title_of(&rig, &p.project), "Project");
    admin(&rig, &token, json!({ "idempotencyKey": "mg-under-root", "steps": [
        { "op": "create", "parentId": p.project, "title": "Added under the project" }
    ]})).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_managers_bulk_count_covers_a_cascading_archive_and_anchors_to_the_project() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..10 {
        create_item(&rig, &format!("mg-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    // One archive step, 12 items in its subtree: it needs approval.
    let reply = admin(&rig, &token, json!({ "idempotencyKey": "mg-cascade", "steps": [
        { "op": "archive", "itemId": p.tickets }
    ]})).await.unwrap();
    assert_eq!(reply["needsApproval"], true, "{reply}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none());
    // The request sits on the project item, not on wherever the manager chat is filed.
    let request = snapshot(&rig).user_action_requests.iter().find(|r| r.text.contains("by project manager")).cloned().unwrap();
    assert_eq!(request.work_item_id, p.project);
    // Recent changes label the manager, not an admin grant.
    admin(&rig, &token, json!({ "idempotencyKey": "mg-label", "steps": [
        { "op": "rename", "itemId": p.ticket, "title": "Labeled" }
    ]})).await.unwrap();
    let turns = rig.core.sessions.agent_tokens().admin().turns(Some(SPACE));
    assert!(turns.iter().all(|t| t.label.starts_with("by project manager")), "{turns:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guard_rechecks_chat_assignments_and_the_effect_budget_under_the_lock() {
    use mast_engine::work_outline::ManagerGuard;
    use mast_proto::{ArchiveWorkItemParams, DescendantArchivePolicy, WorkOutlineMutation};
    let (rig, p) = manager_rig().await;
    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let archive = |op: &str, item: &str| {
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: item.to_owned(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: op.to_owned(),
        })
    };
    let guard = ManagerGuard { chat_id: CHAT.into(), root: p.project.clone(), ..Default::default() };
    // A chat moved out of the project since the opening snapshot.
    let err = outline
        .mutate_guarded(&archive("g2-chat", &p.ticket), &actor, Some(SPACE),
            Some(&ManagerGuard { chats: vec![OUTSIDE_CHAT.into()], ..guard.clone() }))
        .unwrap_err();
    assert!(err.to_wire_message().contains("no longer assigned"), "{err:?}");
    // The subtree grew past what was approved: tickets now holds 1 + 11 items.
    for i in 0..11 {
        create_item(&rig, &format!("g2-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    let err = outline
        .mutate_guarded(&archive("g2-grew", &p.tickets), &actor, Some(SPACE),
            Some(&ManagerGuard { effects: vec![("archive".into(), p.tickets.clone(), String::new())], budget: Some(mast_engine::admin::BatchBudget::new(10)), ..guard.clone() }))
        .unwrap_err();
    assert!(err.to_wire_message().contains("more items than were approved"), "{err:?}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none());
    // A budget that covers it passes.
    outline
        .mutate_guarded(&archive("g2-ok", &p.tickets), &actor, Some(SPACE),
            Some(&ManagerGuard { effects: vec![("archive".into(), p.tickets.clone(), String::new())], budget: Some(mast_engine::admin::BatchBudget::new(13)), ..guard }))
        .unwrap();
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_admin_grant_wins_over_the_manager_role_everywhere() {
    let (rig, p) = manager_rig().await;
    grant(&rig, CHAT, None, false).await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let origin = auth_origin(&rig, &token).await;
    assert_eq!(origin["adminSpaceId"], SPACE);
    assert!(origin.get("managerScopeItemId").is_none(), "{origin}");
    // The manager's project list is empty, and the admin sees the whole Space.
    let scope = tool(&rig, &token, "chat_project_scope_chats", json!({})).await.unwrap();
    assert!(ids(&scope["managerChatIds"]).is_empty());
    let info = tool(&rig, &token, "chat_archive_info", json!({})).await.unwrap();
    assert!(info["chats"].get(OUTSIDE_CHAT).is_some(), "admin reaches every chat: {info}");
    let view = admin(&rig, &token, json!({ "read": true })).await.unwrap();
    assert!(view.to_string().contains("Bystander"), "admin reads the whole Space: {view}");
    admin(&rig, &token, json!({ "idempotencyKey": "mg-admin-wins", "steps": [
        { "op": "rename", "itemId": p.other_tickets, "title": "Admin renamed" }
    ]})).await.unwrap();
    assert_eq!(title_of(&rig, &p.other_tickets), "Admin renamed");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_batch_budget_is_debited_under_the_lock_across_steps_and_retries() {
    use mast_engine::admin::BatchBudget;
    use mast_engine::work_outline::ManagerGuard;
    use mast_proto::{ArchiveWorkItemParams, DescendantArchivePolicy, WorkOutlineMutation};
    let (rig, p) = manager_rig().await;
    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let mut kids = Vec::new();
    for i in 0..4 {
        kids.push(create_item(&rig, &format!("bb-kid-{i}"), &format!("Budget kid {i}"), Some(&p.tickets)).await);
    }
    let budget = BatchBudget::new(3);
    let step = |op: &str, kid: &str| {
        let guard = ManagerGuard {
            chat_id: CHAT.into(),
            root: p.project.clone(),
            effects: vec![("archive".into(), kid.to_owned(), String::new())],
            budget: Some(budget.clone()),
            ..Default::default()
        };
        let mutation = WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: kid.to_owned(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: op.to_owned(),
        });
        outline.mutate_guarded(&mutation, &actor, Some(SPACE), Some(&guard))
    };
    // Each committed step debits what it affected; the fourth would pass 3.
    for (i, kid) in kids.iter().take(3).enumerate() {
        step(&format!("bb-op-{i}"), kid).unwrap();
    }
    assert_eq!(budget.remaining(), 0);
    let err = step("bb-op-3", &kids[3]).unwrap_err();
    assert!(err.to_wire_message().contains("more items than were approved"), "{err:?}");
    assert!(snapshot(&rig).item(&kids[3]).unwrap().archived_at.is_none());
    // A retry replays the committed step without charging it again.
    step("bb-op-0", &kids[0]).unwrap();
    assert_eq!(budget.remaining(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_whose_preview_grew_gets_its_own_approval_and_the_old_token_cannot_authorize_it() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..10 {
        create_item(&rig, &format!("ap-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    let steps = json!([{ "op": "archive", "itemId": p.tickets }]);
    let first = admin(&rig, &token, json!({ "idempotencyKey": "ap-grow", "steps": steps })).await.unwrap();
    assert_eq!(first["needsApproval"], true, "{first}");
    assert_eq!(first["count"], 12);
    // The tree grows, then the same steps are previewed again.
    for i in 0..2 {
        create_item(&rig, &format!("ap-more-{i}"), &format!("More {i}"), Some(&p.tickets)).await;
    }
    let second = admin(&rig, &token, json!({ "idempotencyKey": "ap-grow", "steps": steps })).await.unwrap();
    assert_eq!(second["needsApproval"], true, "{second}");
    assert_eq!(second["count"], 14);
    assert_ne!(first["requestId"], second["requestId"], "a changed preview is a new request");
    // The earlier token cannot authorize the larger batch.
    let stale = admin(&rig, &token, json!({
        "idempotencyKey": "ap-grow", "steps": steps, "confirmToken": first["confirmToken"]
    })).await.unwrap_err();
    assert!(stale.contains("preview changed"), "{stale}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn step_effects_counts_a_plain_create_as_one_and_an_archive_as_its_subtree() {
    use mast_engine::work_outline::step_effects;
    let (rig, p) = manager_rig().await;
    // With no template on the parent a create is one item.
    let snap = snapshot(&rig);
    assert_eq!(step_effects(&snap, "create", &p.tickets), 1);
    assert_eq!(step_effects(&snap, "rename", &p.ticket), 1);
    assert_eq!(step_effects(&snap, "archive", &p.tickets), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_under_a_container_template_is_charged_its_whole_expansion() {
    use mast_engine::admin::BatchBudget;
    use mast_engine::work_outline::{ManagerGuard, step_effects};
    use mast_proto::WorkOutlineMutation;
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    // Every child of Tickets follows: the child itself plus Planning and Tickets (3 items).
    admin(&rig, &token, json!({ "idempotencyKey": "tpl-set", "steps": [{
        "op": "template_set", "itemId": p.tickets,
        "template": { "root": { "children": [{ "title": "Planning" }, { "title": "Tickets" }] } }
    }]})).await.unwrap();
    assert_eq!(step_effects(&snapshot(&rig), "create", &p.tickets), 3);
    // The preview counts the expansion: four creates are 12 items, over the
    // threshold, though they are only four steps.
    let steps: Vec<Value> = (0..4)
        .map(|i| json!({ "op": "create", "parentId": p.tickets, "title": format!("Preview {i}") }))
        .collect();
    let preview = admin(&rig, &token, json!({ "idempotencyKey": "tpl-preview", "steps": steps })).await.unwrap();
    assert_eq!(preview["needsApproval"], true, "{preview}");
    assert_eq!(preview["count"], 12, "{preview}");

    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let create = |op: &str, title: &str| -> WorkOutlineMutation {
        serde_json::from_value(json!({
            "op": "createWorkItem", "spaceId": SPACE, "title": title,
            "parentWorkItemId": p.tickets, "operationId": op
        })).unwrap()
    };
    let guard = |budget: &std::sync::Arc<BatchBudget>| ManagerGuard {
        chat_id: CHAT.into(),
        root: p.project.clone(),
        inside: vec![p.tickets.clone()],
        effects: vec![("create".into(), p.tickets.clone(), String::new())],
        budget: Some(budget.clone()),
        ..Default::default()
    };
    let children = |rig: &Rig| snapshot(rig).item(&p.tickets).unwrap().child_ids.len();
    let before = children(&rig);

    // An approval of 2 cannot cover a create that expands to 3: refused, nothing written.
    let small = BatchBudget::new(2);
    let err = outline
        .mutate_guarded(&create("tpl-small", "Too big"), &actor, Some(SPACE), Some(&guard(&small)))
        .unwrap_err();
    assert!(err.to_wire_message().contains("more items than were approved"), "{err:?}");
    assert_eq!(children(&rig), before, "no item was created");
    assert_eq!(small.remaining(), 2, "a refused step charges nothing");

    // A sufficient budget creates it and is charged the actual subtree size.
    let enough = BatchBudget::new(5);
    let reply = outline
        .mutate_guarded(&create("tpl-ok", "Fits"), &actor, Some(SPACE), Some(&guard(&enough)))
        .unwrap();
    let created = reply.work_item_id.expect("the new item id");
    let snap = snapshot(&rig);
    fn subtree(snap: &WorkOutlineSnapshot, id: &str) -> usize {
        1 + snap.item(id).unwrap().child_ids.iter().map(|c| subtree(snap, c)).sum::<usize>()
    }
    let actual = subtree(&snap, &created);
    assert_eq!(actual, 3, "the child and its two template items");
    assert_eq!(5 - enough.remaining(), actual, "charged exactly the subtree it created");
}

#[test]
fn a_batch_budget_survives_a_restart_and_a_fresh_approval_adds_to_what_is_spent() {
    use mast_engine::admin::AdminGrants;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("admin-grants.json");
    let first = AdminGrants::default();
    first.set_store(store.clone());
    let budget = first.batch_budget("exec-1", 10);
    assert!(budget.reserve(4));
    assert!(!budget.reserve(7), "only 6 are left");
    assert_eq!(budget.remaining(), 6);
    drop((budget, first));

    // A restart keeps what the first attempt committed.
    let second = AdminGrants::default();
    second.set_store(store);
    let budget = second.batch_budget("exec-1", 10);
    assert_eq!(budget.remaining(), 6, "committed charges survive a restart");
    // A fresh approval is for 12 more, on top of the 4 already spent.
    budget.approve(12, &[]).unwrap();
    assert_eq!(budget.remaining(), 12);
    assert!(budget.reserve(12));
    assert_eq!(budget.remaining(), 0);
    // A reservation whose write did not commit is given back.
    budget.refund(5);
    assert_eq!(budget.remaining(), 5);
}

#[test]
fn an_approval_is_claimed_atomically_by_one_batch_for_one_resolved_preview() {
    use mast_engine::admin::AdminGrants;
    let grants = AdminGrants::default();
    let token = grants.open_approval("chat", "exec-a", "hash-a", "request-1", 12);
    // The same count with a different resolved preview is not what was shown.
    let err = grants.claim_approval(&token, "chat", "exec-a", "hash-b", None, &[]).unwrap_err();
    assert!(err.contains("preview changed"), "{err}");
    // Another chat or another batch cannot use it.
    assert!(grants.claim_approval(&token, "other-chat", "exec-a", "hash-a", None, &[]).is_err());
    assert!(grants.claim_approval(&token, "chat", "exec-b", "hash-a", None, &[]).unwrap_err().contains("another batch"));
    // The first claim is fresh; a retry of that batch resumes it even though its
    // own earlier steps moved the preview; no other batch can take it over.
    assert_eq!(grants.claim_approval(&token, "chat", "exec-a", "hash-a", None, &[]).unwrap(), ("request-1".into(), 12, true));
    assert_eq!(grants.claim_approval(&token, "chat", "exec-a", "hash-moved", None, &[]).unwrap(), ("request-1".into(), 12, false));
    assert!(grants.claim_approval(&token, "chat", "exec-b", "hash-a", None, &[]).is_err());

    // Racing claims from many batches: exactly one gets it.
    let token = grants.open_approval("chat", "exec-0", "hash", "request-2", 11);
    let wins = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (grants, token) = (&grants, &token);
                scope.spawn(move || grants.claim_approval(token, "chat", &format!("exec-{i}"), "hash", None, &[]).is_ok())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count()
    });
    assert_eq!(wins, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_is_priced_in_order_before_its_first_write() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    // Priced against the opening tree this is 1 + 1 + 4 = 6 items. In order, the
    // template set by step 2 makes each later create expand to 3: 1 + 1 + 12.
    let mut steps = vec![
        json!({ "op": "create", "parentId": p.project, "title": "Boxed", "ref": "b" }),
        json!({ "op": "template_set", "itemId": "$b",
                "template": { "root": { "children": [{ "title": "Planning" }, { "title": "Tickets" }] } } }),
    ];
    for i in 0..4 {
        steps.push(json!({ "op": "create", "parentId": "$b", "title": format!("Box item {i}") }));
    }
    let reply = admin(&rig, &token, json!({ "idempotencyKey": "seq-1", "steps": steps })).await.unwrap();
    assert_eq!(reply["needsApproval"], true, "{reply}");
    assert_eq!(reply["count"], 14, "{reply}");
    assert!(
        !snapshot(&rig).items.iter().any(|i| i.title == "Boxed"),
        "nothing was written before approval"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_distinct_execution_of_the_same_steps_gets_its_own_approval_and_a_committed_retry_replays() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..10 {
        create_item(&rig, &format!("ex-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    let steps = json!([{ "op": "archive", "itemId": p.tickets }]);
    let one = admin(&rig, &token, json!({ "idempotencyKey": "exec-one", "steps": steps })).await.unwrap();
    let two = admin(&rig, &token, json!({ "idempotencyKey": "exec-two", "steps": steps })).await.unwrap();
    assert_ne!(one["requestId"], two["requestId"], "each execution asks on its own");
    // One execution's token cannot authorize another.
    let err = admin(&rig, &token, json!({
        "idempotencyKey": "exec-two", "steps": steps, "confirmToken": one["confirmToken"]
    })).await.unwrap_err();
    assert!(err.contains("another batch"), "{err}");

    // A committed batch retried with the same key replays instead of charging again.
    let rename = json!([{ "op": "rename", "itemId": p.ticket, "title": "Once" }]);
    admin(&rig, &token, json!({ "idempotencyKey": "exec-rename", "steps": rename })).await.unwrap();
    admin(&rig, &token, json!({ "idempotencyKey": "exec-rename", "steps": rename })).await.unwrap();
    assert_eq!(title_of(&rig, &p.ticket), "Once");
}

async fn user_approves(rig: &Rig, request_id: &str, operation: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "resolveUserActionRequest", "userActionRequestId": request_id,
                    "outcome": "approved", "via": "card", "operationId": operation }),
        )
        .await
        .unwrap();
}

/// The execution key `work_outline_admin` derives for plain (ungrouped) steps.
fn exec_key_of(chat: &str, key: &str, steps: &Value) -> String {
    use sha2::{Digest, Sha256};
    let actions: Vec<serde_json::Map<String, Value>> =
        steps.as_array().unwrap().iter().map(|s| s.as_object().unwrap().clone()).collect();
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&actions).unwrap()));
    format!("{chat}:{key}:{}", &hash[..16])
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_batch_fails_closed_when_its_budget_cannot_be_saved_or_proven() {
    // The budget cannot be written: the step is refused before it writes.
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let data = std::fs::canonicalize(rig._tmp.path()).unwrap().join("data");
    std::fs::create_dir_all(data.join("admin-batch-budgets.json")).unwrap();
    let err = admin(&rig, &token, json!({ "idempotencyKey": "fc-store", "steps": [
        { "op": "rename", "itemId": p.ticket, "title": "Never written" }
    ]})).await.unwrap_err();
    assert!(err.contains("budget could not be saved"), "{err}");
    assert_eq!(title_of(&rig, &p.ticket), "Ticket 1", "nothing was written");

    // A batch whose earlier steps already committed, with no budget record to
    // prove what they spent, is not started over.
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let one = json!([{ "op": "rename", "itemId": p.ticket, "title": "FC one" }]);
    admin(&rig, &token, json!({ "idempotencyKey": "fc-prefix", "steps": one })).await.unwrap();
    let both = json!([
        { "op": "rename", "itemId": p.ticket, "title": "FC one" },
        { "op": "rename", "itemId": p.tickets, "title": "FC two" },
    ]);
    let err = admin(&rig, &token, json!({ "idempotencyKey": "fc-prefix", "steps": both })).await.unwrap_err();
    assert!(err.contains("budget record is gone"), "{err}");
    assert_eq!(title_of(&rig, &p.tickets), "Tickets", "the unproven remainder did not run");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_needs_fresh_approval_when_the_remaining_work_exceeds_the_remaining_allowance() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    admin(&rig, &token, json!({ "idempotencyKey": "ra-template", "steps": [{
        "op": "template_set", "itemId": p.tickets,
        "template": { "root": { "children": [{ "title": "Planning" }, { "title": "Tickets" }] } }
    }]})).await.unwrap();
    // Three creates under the template are 9 items: under the default of 10,
    // but the batch has only 3 left of its allowance after 7 were spent.
    let steps = json!([
        { "op": "create", "parentId": p.tickets, "title": "RA one" },
        { "op": "create", "parentId": p.tickets, "title": "RA two" },
        { "op": "create", "parentId": p.tickets, "title": "RA three" },
    ]);
    let tokens = rig.core.sessions.agent_tokens();
    let grants = tokens.admin();
    let exec = exec_key_of(CHAT, "ra-batch", &steps);
    assert!(grants.batch_budget(&exec, 10).reserve(7));
    assert_eq!(grants.batch_remaining(&exec), Some(3));
    let ask = admin(&rig, &token, json!({ "idempotencyKey": "ra-batch", "steps": steps })).await.unwrap();
    assert_eq!(ask["needsApproval"], true, "9 > the 3 that are left: {ask}");
    assert_eq!(ask["count"], 9);
    user_approves(&rig, ask["requestId"].as_str().unwrap(), "ra-approve").await;
    let done = admin(&rig, &token, json!({
        "idempotencyKey": "ra-batch", "steps": steps, "confirmToken": ask["confirmToken"]
    })).await.unwrap();
    assert_eq!(done["applied"], 3, "{done}");
    // The approval was for 9 more on top of the 7 spent, and all of it was used.
    assert_eq!(grants.batch_remaining(&exec), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_is_bound_to_what_the_user_was_shown_and_to_the_items_affected() {
    use mast_engine::work_outline::{ManagerGuard, step_plan_digest};
    use mast_proto::{ArchiveWorkItemParams, DescendantArchivePolicy, WorkOutlineMutation};
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..10 {
        create_item(&rig, &format!("pb-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    let steps = json!([{ "op": "archive", "itemId": p.tickets }]);
    let shown = admin(&rig, &token, json!({ "idempotencyKey": "pb-title", "steps": steps })).await.unwrap();
    user_approves(&rig, shown["requestId"].as_str().unwrap(), "pb-approve-1").await;
    // A displayed detail changes (the item's title), though the mutation and the
    // count do not: the user approved something else.
    admin(&rig, &token, json!({ "idempotencyKey": "pb-rename", "steps": [
        { "op": "rename", "itemId": p.tickets, "title": "Renamed since shown" }
    ]})).await.unwrap();
    let err = admin(&rig, &token, json!({
        "idempotencyKey": "pb-title", "steps": steps, "confirmToken": shown["confirmToken"]
    })).await.unwrap_err();
    assert!(err.contains("preview changed"), "{err}");

    // The same count over different items: a kid replaced by another.
    let shown = admin(&rig, &token, json!({ "idempotencyKey": "pb-ids", "steps": steps })).await.unwrap();
    user_approves(&rig, shown["requestId"].as_str().unwrap(), "pb-approve-2").await;
    let first_kid = snapshot(&rig).item(&p.tickets).unwrap().child_ids.iter()
        .find(|id| **id != p.ticket).cloned().unwrap();
    admin(&rig, &token, json!({ "idempotencyKey": "pb-swap", "steps": [
        { "op": "archive", "itemId": first_kid },
        { "op": "create", "parentId": p.tickets, "title": "Replacement" },
    ]})).await.unwrap();
    let err = admin(&rig, &token, json!({
        "idempotencyKey": "pb-ids", "steps": steps, "confirmToken": shown["confirmToken"]
    })).await.unwrap_err();
    assert!(err.contains("preview changed"), "{err}");

    // Under the writer's lock the affected items are checked again.
    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let stale = step_plan_digest(&snapshot(&rig), "archive", &p.tickets);
    create_item(&rig, "pb-late", "Added after", Some(&p.tickets)).await;
    let guard = ManagerGuard {
        chat_id: CHAT.into(),
        root: p.project.clone(),
        strict: vec![p.tickets.clone()],
        effects: vec![("archive".into(), p.tickets.clone(), stale)],
        budget: Some(mast_engine::admin::BatchBudget::new(100)),
        ..Default::default()
    };
    let archive = WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
        work_item_id: p.tickets.clone(),
        descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
        operation_id: "pb-guard".into(),
    });
    let err = outline.mutate_guarded(&archive, &actor, Some(SPACE), Some(&guard)).unwrap_err();
    assert!(err.to_wire_message().contains("changed since the preview"), "{err:?}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_is_claimed_only_when_the_approved_batch_runs_with_its_allowance() {
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..10 {
        create_item(&rig, &format!("cl-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    // One archive step that affects 12 items: over the default allowance of 10.
    let steps = json!([{ "op": "archive", "itemId": p.tickets }]);
    let ask = admin(&rig, &token, json!({ "idempotencyKey": "cl-batch", "steps": steps })).await.unwrap();
    let confirm = ask["confirmToken"].clone();
    let with_token = |extra: Value| {
        let mut args = json!({ "idempotencyKey": "cl-batch", "steps": steps, "confirmToken": confirm });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    // Before the user approves, trying the token claims nothing.
    let err = admin(&rig, &token, with_token(json!({}))).await.unwrap_err();
    assert!(err.contains("not approved"), "{err}");
    user_approves(&rig, ask["requestId"].as_str().unwrap(), "cl-approve").await;
    // A dry run validates the approved batch and claims nothing either.
    let dry = admin(&rig, &token, with_token(json!({ "dryRun": true }))).await.unwrap();
    assert_eq!(dry["approved"], true, "{dry}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none());
    // The real run claims it and installs the allowance in one step, so the
    // 12-item step is within budget and runs.
    let done = admin(&rig, &token, with_token(json!({}))).await.unwrap();
    assert_eq!(done["applied"], 1, "{done}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partial_retry_is_checked_against_the_plan_the_user_approved() {
    use mast_engine::work_outline::step_plan_digest;
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    for i in 0..2 {
        create_item(&rig, &format!("pr-kid-{i}"), &format!("Kid {i}"), Some(&p.tickets)).await;
    }
    // The first attempt was admitted (and, say, crashed) with this plan: archive
    // the Tickets subtree, 4 items, well inside the allowance.
    let steps = json!([{ "op": "archive", "itemId": p.tickets }]);
    let grants_owner = rig.core.sessions.agent_tokens();
    let grants = grants_owner.admin();
    let exec = exec_key_of(CHAT, "pr-batch", &steps);
    let approved = step_plan_digest(&snapshot(&rig), "archive", &p.tickets);
    grants.admit_batch(&exec, 10, &[approved], false).unwrap();
    // The target changes before the retry; it still costs less than the allowance left.
    create_item(&rig, "pr-late", "Added meanwhile", Some(&p.tickets)).await;
    let ask = admin(&rig, &token, json!({ "idempotencyKey": "pr-batch", "steps": steps })).await.unwrap();
    assert_eq!(ask["needsApproval"], true, "the unfinished step no longer matches its plan: {ask}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_none(), "nothing ran");
    // Approving the changed plan replaces it, and the step then runs.
    user_approves(&rig, ask["requestId"].as_str().unwrap(), "pr-approve").await;
    let done = admin(&rig, &token, json!({
        "idempotencyKey": "pr-batch", "steps": steps, "confirmToken": ask["confirmToken"]
    })).await.unwrap();
    assert_eq!(done["applied"], 1, "{done}");
    assert!(snapshot(&rig).item(&p.tickets).unwrap().archived_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_same_size_template_substitution_changes_what_an_approval_is_bound_to() {
    use mast_engine::work_outline::step_plan_digest;
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    let set = |key: &str, titles: [&str; 2]| {
        json!({ "idempotencyKey": key, "steps": [{
            "op": "template_set", "itemId": p.tickets,
            "template": { "root": { "children": [{ "title": titles[0] }, { "title": titles[1] }] } }
        }]})
    };
    admin(&rig, &token, set("ts-one", ["Planning", "Tickets"])).await.unwrap();
    let before = snapshot(&rig);
    let create_before = step_plan_digest(&before, "create", &p.tickets);
    let apply_before = step_plan_digest(&before, "template_apply", &p.tickets);
    // Same node count, different rules.
    admin(&rig, &token, set("ts-two", ["Design", "Build"])).await.unwrap();
    let after = snapshot(&rig);
    assert_eq!(
        mast_engine::work_outline::step_effects(&after, "create", &p.tickets),
        mast_engine::work_outline::step_effects(&before, "create", &p.tickets),
        "the count did not change"
    );
    assert_ne!(step_plan_digest(&after, "create", &p.tickets), create_before);
    assert_ne!(step_plan_digest(&after, "template_apply", &p.tickets), apply_before);
    // A row of the subtree replaced by another of the same size changes an apply too.
    let apply_now = step_plan_digest(&after, "template_apply", &p.tickets);
    admin(&rig, &token, json!({ "idempotencyKey": "ts-rename", "steps": [
        { "op": "rename", "itemId": p.ticket, "title": "Same size, other title" }
    ]})).await.unwrap();
    assert_ne!(step_plan_digest(&snapshot(&rig), "template_apply", &p.tickets), apply_now);

    // End to end: an approval for four creates is void once the rules are swapped
    // for a same-size set.
    let steps: Vec<Value> = (0..4)
        .map(|i| json!({ "op": "create", "parentId": p.tickets, "title": format!("TS {i}") }))
        .collect();
    let ask = admin(&rig, &token, json!({ "idempotencyKey": "ts-batch", "steps": steps })).await.unwrap();
    assert_eq!(ask["needsApproval"], true, "{ask}");
    user_approves(&rig, ask["requestId"].as_str().unwrap(), "ts-approve").await;
    admin(&rig, &token, set("ts-three", ["Alpha", "Beta"])).await.unwrap();
    let err = admin(&rig, &token, json!({
        "idempotencyKey": "ts-batch", "steps": steps, "confirmToken": ask["confirmToken"]
    })).await.unwrap_err();
    assert!(err.contains("preview changed"), "{err}");
}

#[test]
fn batch_admission_fails_closed_on_an_unreadable_store_a_full_store_and_a_failed_claim() {
    use mast_engine::admin::AdminGrants;
    // An unreadable budget file: nothing is admitted and nothing spends.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("admin-batch-budgets.json"), b"not json").unwrap();
    let broken = AdminGrants::default();
    broken.set_store(dir.path().join("admin-grants.json"));
    let err = broken.admit_batch("any", 10, &[], false).err().unwrap();
    assert!(err.contains("unreadable"), "{err}");

    // An unfinished batch is never forgotten, however many finished ones follow.
    let grants = AdminGrants::default();
    let keep = grants.admit_batch("unfinished", 10, &["plan".into()], false).unwrap();
    for i in 0..700 {
        grants.admit_batch(&format!("done-{i}"), 10, &[], false).unwrap().finish();
    }
    assert_eq!(grants.batch_state("unfinished").unwrap().1, vec!["plan".to_string()]);
    assert!(keep.reserve(3));
    // A batch that committed steps with no record is refused, not restarted.
    let err = grants.admit_batch("forgotten", 10, &[], true).err().unwrap();
    assert!(err.contains("budget record is gone"), "{err}");
    // A store full of unfinished batches refuses new ones.
    let full = AdminGrants::default();
    for i in 0..4096 {
        full.admit_batch(&format!("open-{i}"), 10, &[], false).unwrap();
    }
    assert!(full.admit_batch("one-too-many", 10, &[], false).err().unwrap().contains("too many"));

    // A claim whose allowance cannot be saved claims nothing.
    let dir = tempfile::tempdir().unwrap();
    let grants = AdminGrants::default();
    grants.set_store(dir.path().join("admin-grants.json"));
    let budget = grants.admit_batch("exec", 10, &[], false).unwrap();
    let file = dir.path().join("admin-batch-budgets.json");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let token = grants.open_approval("chat", "exec", "hash", "request", 12);
    let err = grants.claim_approval(&token, "chat", "exec", "hash", Some(&budget), &[]).unwrap_err();
    assert!(err.contains("could not be saved"), "{err}");
    assert!(!budget.reserve(1), "and nothing can spend while it cannot be saved");
    assert_eq!(
        grants.peek_approval(&token, "chat", "exec", "hash").unwrap(),
        ("request".into(), 12, true),
        "the token is still unclaimed"
    );
}

#[test]
fn a_stale_execution_cannot_spend_a_newer_approvals_allowance() {
    use mast_engine::admin::AdminGrants;
    let grants = AdminGrants::default();
    let stale = grants.admit_batch("exec", 10, &["old".into()], false).unwrap();
    let newer = grants.admit_batch("exec", 10, &[], false).unwrap();
    // A concurrent execution's fresh approval replaces the plan and allowance.
    newer.approve(20, &["new".into()]).unwrap();
    assert!(!stale.reserve(1), "its plan was replaced: it spends nothing, even though allowance exists");
    assert_eq!(newer.remaining(), 20);
    assert!(newer.reserve(5));
    assert_eq!(stale.remaining(), 15, "and none of the new allowance was taken");
    assert_eq!(newer.plan(), vec!["new".to_string()]);
    // An execution admitted after the approval is current.
    let current = grants.admit_batch("exec", 10, &[], false).unwrap();
    assert!(current.reserve(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_template_digest_binds_live_structure_sibling_order_and_archive_status() {
    use mast_engine::work_outline::step_plan_digest;
    let (rig, p) = manager_rig().await;
    let (_lease, token) = mint_token(&rig, CHAT);
    admin(&rig, &token, json!({ "idempotencyKey": "ds-template", "steps": [{
        "op": "template_set", "itemId": p.tickets,
        "template": { "root": { "children": [{ "title": "Planning" }, { "title": "Tickets" }] } }
    }]})).await.unwrap();
    let a = create_item(&rig, "ds-a", "Row A", Some(&p.tickets)).await;
    let b = create_item(&rig, "ds-b", "Row B", Some(&p.tickets)).await;
    let digests = |rig: &Rig| {
        let snap = snapshot(rig);
        (step_plan_digest(&snap, "template_apply", &p.tickets), step_plan_digest(&snap, "create", &p.tickets))
    };
    let (apply0, create0) = digests(&rig);

    // Reparent: the same rows and count, one of them now under another.
    admin(&rig, &token, json!({ "idempotencyKey": "ds-reparent", "steps": [
        { "op": "move", "itemId": a, "parentId": b }
    ]})).await.unwrap();
    let (apply1, create1) = digests(&rig);
    assert_ne!(apply1, apply0, "reparenting changes what an apply walks");
    assert_ne!(create1, create0, "and the parent's live children");

    // Sibling order: the same rows, one moved to the front.
    admin(&rig, &token, json!({ "idempotencyKey": "ds-order", "steps": [
        { "op": "move", "itemId": b, "parentId": p.tickets, "position": "first" }
    ]})).await.unwrap();
    let (apply2, create2) = digests(&rig);
    assert_ne!(apply2, apply1, "sibling order is an input of an apply");
    assert_ne!(create2, create1, "and of where a create lands");

    // Archive status: the planner only matches live rows.
    admin(&rig, &token, json!({ "idempotencyKey": "ds-archive", "steps": [
        { "op": "archive", "itemId": a }
    ]})).await.unwrap();
    let (apply3, _) = digests(&rig);
    assert_ne!(apply3, apply2, "an archived row is not the same plan");
}

async fn associated_token(rig: &Rig, op: &str) -> (String, String) {
    let root = create_item(rig, op, "Feature", None).await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-friction", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    tool(rig, &token, "work_outline_associate_current_chat",
        json!({ "workItemId": root, "idempotencyKey": format!("{op}-assoc") })).await.unwrap();
    std::mem::forget(lease);
    (root, token)
}

#[tokio::test(flavor = "multi_thread")]
async fn completing_ignores_own_handoff_but_not_asks() {
    let rig = rig().await;
    let (root, token) = associated_token(&rig, "op-handoff").await;
    let handoff = tool(&rig, &token, "work_outline_create_user_action_request",
        json!({ "request": "Ready to merge: x", "kind": "handoff", "dedupeKey": "merge" })).await.unwrap();
    let ask = tool(&rig, &token, "work_outline_create_user_action_request",
        json!({ "request": "Which option?", "kind": "ask", "dedupeKey": "ask" })).await.unwrap();
    let err = tool(&rig, &token, "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "c1" })).await.unwrap_err();
    assert!(err.contains("work-outline/openUserActionRequests"), "{err}");
    assert!(err.contains("1 open user action request"), "only the ask counts: {err}");
    tool(&rig, &token, "work_outline_resolve_user_action_request",
        json!({ "requestId": ask["userActionRequestId"], "resolution": "done", "idempotencyKey": "r1" }))
        .await.unwrap();
    tool(&rig, &token, "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "c2" })).await.unwrap();
    let snap = snapshot(&rig);
    assert_eq!(snap.item(&root).unwrap().status, mast_proto::WorkItemStatus::Completed);
    let handoff_id = handoff["userActionRequestId"].as_str().unwrap();
    assert!(snap.user_action_requests.iter().any(|r| r.id == handoff_id && r.is_open()),
        "completing leaves the handoff open");
}

#[tokio::test(flavor = "multi_thread")]
async fn long_request_detail_is_truncated_not_refused() {
    let rig = rig().await;
    let (_root, token) = associated_token(&rig, "op-long").await;
    let detail = "é".repeat(9_000);
    let reply = tool(&rig, &token, "work_outline_create_user_action_request",
        json!({ "request": "Review", "detail": detail, "dedupeKey": "long" })).await.unwrap();
    assert_eq!(reply["detailTruncated"], true);
    let id = reply["userActionRequestId"].as_str().unwrap();
    let snap = snapshot(&rig);
    let info = snap.user_action_requests.iter().find(|r| r.id == id).unwrap().info.clone().unwrap();
    assert!(info.chars().count() <= 4_000, "{}", info.chars().count());
    assert!(info.ends_with("… (full text in the chat)"));
    let short = tool(&rig, &token, "work_outline_create_user_action_request",
        json!({ "request": "Short", "detail": "ok", "dedupeKey": "short" })).await.unwrap();
    assert_eq!(short["detailTruncated"], false);
}

// --- run_command / command_status -------------------------------------------

fn command_config(auto_approve: bool, approval_rules: bool, sandbox: SandboxLevel) -> mast_proto::ChatConfig {
    mast_proto::ChatConfig {
        approval_rules,
        harness: HarnessId::Mock,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox,
        auto_approve,
    }
}

fn configure_command_chat(rig: &Rig, chat: &str, config: mast_proto::ChatConfig) {
    rig.core.workspace.set_chat_config(chat, &config).unwrap();
    rig.core.workspace.set_chat_cwd(chat, &rig.space_path).unwrap();
}

fn unique_key(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::new_v4().simple())
}

fn wake_message_id(chat: &str, key: &str) -> String {
    let operation = mast_proto::agent_tools::operation_id(chat, key);
    derive_id(&derive_id(&operation, "command-run"), "command-wake")
}

/// How many times the wake message for this run exists in the chat (queued or
/// already delivered).
fn wake_count(rig: &Rig, chat: &str, id: &str) -> usize {
    let doc = rig.core.doc_host.open(chat).unwrap();
    let doc = doc.doc();
    doc.read_queue().unwrap().iter().filter(|row| row.id == id).count()
        + doc.read_entries().unwrap().iter().filter(|entry| entry.id == id).count()
}

fn wake_text_of(rig: &Rig, chat: &str, id: &str) -> Option<String> {
    let doc = rig.core.doc_host.open(chat).unwrap();
    let doc = doc.doc();
    let text = doc.read_queue().unwrap().iter().find(|row| row.id == id).map(|row| format!("{row:?}"));
    text.or_else(|| doc.read_entries().unwrap().iter().find(|entry| entry.id == id).map(|entry| format!("{entry:?}")))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_command_marks_the_chat_waiting_until_it_exits_and_its_wake_still_arrives() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-wait", SPACE, &rig.core.device_id);
    let key = unique_key("waiting");
    assert!(!mast_engine::agent_tools::chat_has_running_command(CHAT));
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "sleep 1" })).await.unwrap();
    assert!(mast_engine::agent_tools::chat_has_running_command(CHAT));
    let id = wake_message_id(CHAT, &key);
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the exit wake").await;
    assert!(!mast_engine::agent_tools::chat_has_running_command(CHAT));
}

#[tokio::test(flavor = "multi_thread")]
async fn await_background_declares_a_capped_wait_for_this_chat() {
    let rig = rig().await;
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "await-bg", SPACE, &rig.core.device_id);
    let now = chrono::Utc::now().timestamp_millis();
    assert!(tool(&rig, lease.token(), "await_background", json!({ "reason": " ", "maxMinutes": 5 })).await.is_err());
    let reply = tool(&rig, lease.token(), "await_background", json!({ "reason": "background build", "maxMinutes": 500 })).await.unwrap();
    assert_eq!(reply["maxMinutes"], 60);
    assert!(mast_engine::agent_tools::is_awaiting_background(CHAT, now + 59 * 60_000));
    assert!(!mast_engine::agent_tools::is_awaiting_background(CHAT, now + 61 * 60_000));
    mast_engine::agent_tools::clear_awaiting_background(CHAT);
    assert!(!mast_engine::agent_tools::is_awaiting_background(CHAT, now));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_wakes_the_chat_once_with_the_exit_code_and_failure_summary() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-cmd", SPACE, &rig.core.device_id);
    let key = unique_key("suite");
    let start = json!({ "idempotencyKey": key, "command": "echo starting; echo 'test engine::broken ... FAILED'; sleep 1; exit 3" });
    let first = tool(&rig, lease.token(), "run_command", start.clone()).await.unwrap();
    assert_eq!(first["state"], "running");
    // A retry of the start call (lost reply) attaches to the same run.
    let again = tool(&rig, lease.token(), "run_command", start).await.unwrap();
    assert_eq!(again["logPath"], first["logPath"]);
    let id = wake_message_id(CHAT, &key);
    assert_eq!(wake_count(&rig, CHAT, &id), 0, "no wake while the command runs");
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the exit wake").await;
    let text = wake_text_of(&rig, CHAT, &id).unwrap();
    assert!(text.contains("exit code 3") && text.contains("engine::broken"), "{text}");
    let status = tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": key })).await.unwrap();
    assert_eq!((status["state"].as_str(), status["exitCode"].as_i64()), (Some("exited"), Some(3)));
    assert!(status["failureSummary"].as_str().unwrap().contains("FAILED"));
    // Attaching after the exit neither reruns the command nor wakes again.
    let attached = tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "echo starting; echo 'test engine::broken ... FAILED'; sleep 1; exit 3" })).await.unwrap();
    assert_eq!(attached["state"], "exited");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_count(&rig, CHAT, &id), 1);
    // The same key with another command is a conflict.
    let conflict = tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "true" })).await.unwrap_err();
    assert!(conflict.contains("different command"), "{conflict}");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_is_refused_unless_the_chat_would_run_it_without_asking() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-refuse", SPACE, &rig.core.device_id);
    let try_run = |label: &'static str| {
        let token = lease.token().to_owned();
        let rig = &rig;
        async move { tool(rig, &token, "run_command", json!({ "idempotencyKey": unique_key(label), "command": "true" })).await }
    };
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::ReadOnly));
    let read_only = try_run("ro").await.unwrap_err();
    assert!(read_only.contains("read-only"), "{read_only}");
    configure_command_chat(&rig, CHAT, command_config(false, false, SandboxLevel::WorkspaceWrite));
    let ask = try_run("ask").await.unwrap_err();
    assert!(ask.contains("would ask"), "{ask}");
    // Rules mode with no allow rule for the command still asks.
    configure_command_chat(&rig, CHAT, command_config(false, true, SandboxLevel::WorkspaceWrite));
    let rules = try_run("rules").await.unwrap_err();
    assert!(rules.contains("would ask"), "{rules}");
    // Auto-approve runs it.
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    assert_eq!(try_run("auto").await.unwrap()["state"], "running");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_does_not_wake_an_archived_chat_but_the_command_finishes() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-archive", SPACE, &rig.core.device_id);
    let key = unique_key("archived");
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "sleep 1; echo done" })).await.unwrap();
    rig.core.workspace.set_chat_archived(CHAT, true).unwrap();
    let status = || async { tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": key })).await.unwrap() };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while status().await["state"] != "exited" {
        assert!(tokio::time::Instant::now() < deadline, "the command never finished");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_count(&rig, CHAT, &wake_message_id(CHAT, &key)), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_left_by_a_dead_engine_is_interrupted_with_exactly_one_wake() {
    let rig = rig().await;
    let key = unique_key("restart");
    let operation = mast_proto::agent_tools::operation_id(CHAT, &key);
    let run_id = derive_id(&operation, "command-run");
    // A previous engine process started the run and left no exit code. A
    // spawned and reaped child gives a pid that is certainly dead.
    #[allow(clippy::disallowed_methods)]
    let dead_pid = { let mut child = Command::new("true").spawn().unwrap(); child.wait().unwrap(); child.id() };
    let dir = std::env::temp_dir().join(format!("mast-command-runs-{}", rig.core.device_id));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{run_id}.log")), "partial output\n").unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "sleep 100", "enginePid": dead_pid }).to_string(),
    )
    .unwrap();
    let service = |rig: &Rig| {
        let service = mast_engine::agent_tools::AgentToolsService::new(
            rig.core.sessions.agent_tokens(),
            rig.core.workspace.clone(),
            Arc::new(mast_engine::work_outline::WorkOutlineService::new(rig.core.workspace.clone())),
            Some(rig.core.doc_host.clone()),
        );
        service.set_sessions(rig.core.sessions.clone());
        service
    };
    let id = wake_message_id(CHAT, &key);
    service(&rig).recover_command_runs();
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the interrupted wake").await;
    assert!(wake_text_of(&rig, CHAT, &id).unwrap().contains("interrupted"));
    // A second restart finds the marker and wakes nobody again.
    service(&rig).recover_command_runs();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_count(&rig, CHAT, &id), 1);
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-restart", SPACE, &rig.core.device_id);
    let status = tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": key })).await.unwrap();
    assert_eq!(status["state"], "interrupted");
    assert!(status.get("exitCode").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_exited_while_no_engine_watched_wakes_with_its_result_once() {
    let rig = rig().await;
    let key = unique_key("orphan-exit");
    let run_id = derive_id(&mast_proto::agent_tools::operation_id(CHAT, &key), "command-run");
    #[allow(clippy::disallowed_methods)]
    let dead_pid = { let mut child = Command::new("true").spawn().unwrap(); child.wait().unwrap(); child.id() };
    let dir = std::env::temp_dir().join(format!("mast-command-runs-{}", rig.core.device_id));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{run_id}.log")), "all good\n").unwrap();
    std::fs::write(dir.join(format!("{run_id}.exit")), "0").unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "true", "enginePid": dead_pid }).to_string(),
    )
    .unwrap();
    let service = mast_engine::agent_tools::AgentToolsService::new(
        rig.core.sessions.agent_tokens(),
        rig.core.workspace.clone(),
        Arc::new(mast_engine::work_outline::WorkOutlineService::new(rig.core.workspace.clone())),
        Some(rig.core.doc_host.clone()),
    );
    service.set_sessions(rig.core.sessions.clone());
    let id = wake_message_id(CHAT, &key);
    service.recover_command_runs();
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the wake for the unobserved exit").await;
    assert!(wake_text_of(&rig, CHAT, &id).unwrap().contains("succeeded"));
    wait_for(|| dir.join(format!("{run_id}.woken")).exists(), "the delivered marker").await;
    service.recover_command_runs();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_count(&rig, CHAT, &id), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_starts_with_one_key_run_the_command_once_and_cwd_stays_in_the_chat_root() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-race", SPACE, &rig.core.device_id);
    let key = unique_key("race");
    let counter = std::path::Path::new(&rig.space_path).join(format!("{key}.count"));
    let command = format!("echo x >> {}; sleep 1", counter.display());
    let start = json!({ "idempotencyKey": key, "command": command });
    let (a, b) = tokio::join!(
        tool(&rig, lease.token(), "run_command", start.clone()),
        tool(&rig, lease.token(), "run_command", start.clone()),
    );
    assert_eq!((a.unwrap()["state"].as_str().map(str::to_owned), b.unwrap()["state"].as_str().map(str::to_owned)), (Some("running".into()), Some("running".into())));
    wait_for(|| counter.exists(), "the command to write").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1, "the command ran once");
    let outside = tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": unique_key("cwd"), "command": "true", "cwd": "/" })).await.unwrap_err();
    assert!(outside.contains("inside the Chat's working directory"), "{outside}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_marker_beats_a_late_exit_file_on_retry() {
    let rig = rig().await;
    let key = unique_key("late-exit");
    let run_id = derive_id(&mast_proto::agent_tools::operation_id(CHAT, &key), "command-run");
    #[allow(clippy::disallowed_methods)]
    let dead_pid = { let mut child = Command::new("true").spawn().unwrap(); child.wait().unwrap(); child.id() };
    let dir = std::env::temp_dir().join(format!("mast-command-runs-{}", rig.core.device_id));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{run_id}.log")), "x\n").unwrap();
    std::fs::write(dir.join(format!("{run_id}.exit")), "0").unwrap();
    std::fs::write(dir.join(format!("{run_id}.interrupted")), "").unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "true", "enginePid": dead_pid }).to_string(),
    )
    .unwrap();
    let service = mast_engine::agent_tools::AgentToolsService::new(
        rig.core.sessions.agent_tokens(),
        rig.core.workspace.clone(),
        Arc::new(mast_engine::work_outline::WorkOutlineService::new(rig.core.workspace.clone())),
        Some(rig.core.doc_host.clone()),
    );
    service.set_sessions(rig.core.sessions.clone());
    let id = wake_message_id(CHAT, &key);
    service.recover_command_runs();
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the retried wake").await;
    assert!(wake_text_of(&rig, CHAT, &id).unwrap().contains("interrupted"));
}

fn recovery_service(rig: &Rig) -> mast_engine::agent_tools::AgentToolsService {
    let service = mast_engine::agent_tools::AgentToolsService::new(
        rig.core.sessions.agent_tokens(),
        rig.core.workspace.clone(),
        Arc::new(mast_engine::work_outline::WorkOutlineService::new(rig.core.workspace.clone())),
        Some(rig.core.doc_host.clone()),
    );
    service.set_sessions(rig.core.sessions.clone());
    service
}

fn runs_dir_of(rig: &Rig) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mast-command-runs-{}", rig.core.device_id));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_id_of(chat: &str, key: &str) -> String {
    derive_id(&mast_proto::agent_tools::operation_id(chat, key), "command-run")
}

fn dead_engine_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    child.id()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_may_have_only_a_few_commands_running_but_can_still_attach_to_them() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-limit", SPACE, &rig.core.device_id);
    let keys: Vec<String> = (0..4).map(|i| unique_key(&format!("limit-{i}"))).collect();
    for key in &keys {
        tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "sleep 2" })).await.unwrap();
    }
    let extra = unique_key("limit-extra");
    let refused = tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": extra, "command": "sleep 2" })).await.unwrap_err();
    assert!(refused.contains("limit is 4"), "{refused}");
    // The refused command was never started, so its key stays free.
    let unknown = tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": extra })).await.unwrap_err();
    assert!(unknown.contains("no run was started"), "{unknown}");
    // A repeat of a running command attaches to it instead of counting again.
    let again = tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": keys[0], "command": "sleep 2" })).await.unwrap();
    assert_eq!(again["state"], "running");
    // Another chat is not held back by this one's runs.
    configure_command_chat(&rig, OTHER_CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let other = rig.core.sessions.agent_tokens().mint(OTHER_CHAT, "run-limit-other", SPACE, &rig.core.device_id);
    tool(&rig, other.token(), "run_command", json!({ "idempotencyKey": unique_key("limit-other"), "command": "true" })).await.unwrap();
    // Once one finishes there is room again.
    let first = wake_message_id(CHAT, &keys[0]);
    wait_for(|| wake_count(&rig, CHAT, &first) == 1, "a command to finish").await;
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": extra, "command": "true" })).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_follows_the_manager_hand_off_chain_to_the_successor_once() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-handoff", SPACE, &rig.core.device_id);
    let key = unique_key("handoff");
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "sleep 1; echo built" })).await.unwrap();
    // The Manager hands its role over while the command runs: the starting
    // chat is archived and points at its successor.
    let successor = "chat-successor";
    rig.core.workspace.create_chat(successor, Some(SPACE), None, None, None).unwrap();
    let mut row = rig.core.workspace.chat(CHAT).unwrap().unwrap();
    row.archived = true;
    row.standing_role_successor = Some(successor.to_owned());
    rig.core.workspace.import_chat_row(&row).unwrap();
    let id = wake_message_id(CHAT, &key);
    wait_for(|| wake_count(&rig, successor, &id) == 1, "the wake at the successor").await;
    let text = wake_text_of(&rig, successor, &id).unwrap();
    assert!(text.contains("handed to you") && text.contains("built"), "{text}");
    assert_eq!(wake_count(&rig, CHAT, &id), 0, "the archived predecessor gets nothing");
    // A restart finds the wake delivered and sends nothing more.
    wait_for(|| runs_dir_of(&rig).join(format!("{}.woken", run_id_of(CHAT, &key))).exists(), "the delivered marker").await;
    recovery_service(&rig).recover_command_runs();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_count(&rig, successor, &id), 1);
    assert_eq!(wake_count(&rig, CHAT, &id), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_never_revives_an_archived_end_of_the_hand_off_chain() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-retired", SPACE, &rig.core.device_id);
    let key = unique_key("retired");
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": "sleep 1" })).await.unwrap();
    // The role moved to a successor that was then archived (the project ended).
    let successor = "chat-retired-successor";
    rig.core.workspace.create_chat(successor, Some(SPACE), None, None, None).unwrap();
    rig.core.workspace.set_chat_archived(successor, true).unwrap();
    let mut row = rig.core.workspace.chat(CHAT).unwrap().unwrap();
    row.archived = true;
    row.standing_role_successor = Some(successor.to_owned());
    rig.core.workspace.import_chat_row(&row).unwrap();
    let id = wake_message_id(CHAT, &key);
    wait_for(|| runs_dir_of(&rig).join(format!("{}.woken", run_id_of(CHAT, &key))).exists(), "the deliberate skip to be recorded").await;
    assert_eq!(wake_count(&rig, successor, &id), 0);
    assert_eq!(wake_count(&rig, CHAT, &id), 0);
    assert!(rig.core.workspace.chat(successor).unwrap().unwrap().archived, "the archived successor stays archived");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archived_chat_that_never_handed_over_is_still_not_woken_after_a_restart_recovery() {
    let rig = rig().await;
    let key = unique_key("archived-recover");
    let run_id = run_id_of(CHAT, &key);
    let dir = runs_dir_of(&rig);
    std::fs::write(dir.join(format!("{run_id}.log")), "ok\n").unwrap();
    std::fs::write(dir.join(format!("{run_id}.exit")), "0").unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "true", "enginePid": dead_engine_pid() }).to_string(),
    )
    .unwrap();
    rig.core.workspace.set_chat_archived(CHAT, true).unwrap();
    recovery_service(&rig).recover_command_runs();
    wait_for(|| dir.join(format!("{run_id}.woken")).exists(), "the deliberate skip to be recorded").await;
    assert_eq!(wake_count(&rig, CHAT, &wake_message_id(CHAT, &key)), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sigpipe_exit_left_by_a_dead_engine_is_reported_as_interrupted_not_as_a_failure() {
    let rig = rig().await;
    let dir = runs_dir_of(&rig);
    let key = unique_key("sigpipe");
    let run_id = run_id_of(CHAT, &key);
    // The engine died, the command wrote to the closed log pipe and the
    // wrapper recorded 141 (128 + SIGPIPE).
    std::fs::write(dir.join(format!("{run_id}.log")), "partial\n").unwrap();
    std::fs::write(dir.join(format!("{run_id}.exit")), "141").unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "cargo test", "enginePid": dead_engine_pid() }).to_string(),
    )
    .unwrap();
    let id = wake_message_id(CHAT, &key);
    recovery_service(&rig).recover_command_runs();
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the interrupted wake").await;
    let text = wake_text_of(&rig, CHAT, &id).unwrap();
    assert!(text.contains("interrupted") && !text.contains("failed with exit code"), "{text}");
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-sigpipe", SPACE, &rig.core.device_id);
    let status = tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": key })).await.unwrap();
    assert_eq!(status["state"], "interrupted");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn recovery_stops_the_command_an_engine_left_running_and_only_that_command() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let rig = rig().await;
    let dir = runs_dir_of(&rig);
    let key = unique_key("orphan-kill");
    let run_id = run_id_of(CHAT, &key);
    // What a dead engine leaves: a wrapper shell of its own process group,
    // named for the run, with no exit code recorded.
    let mut orphan = Command::new("sh")
        .arg("-c")
        .arg("sleep 30; exit 0")
        .arg(format!("mast-run-command-{run_id}"))
        .process_group(0)
        .spawn()
        .unwrap();
    std::fs::write(dir.join(format!("{run_id}.log")), "partial\n").unwrap();
    std::fs::write(dir.join(format!("{run_id}.pid")), orphan.id().to_string()).unwrap();
    std::fs::write(
        dir.join(format!("{run_id}.meta")),
        json!({ "chatId": CHAT, "key": key, "command": "sleep 30", "enginePid": dead_engine_pid() }).to_string(),
    )
    .unwrap();
    // Another run's pid file names a process that is not its wrapper (the pid
    // was recycled): it must be left alone.
    let bystander_key = unique_key("orphan-bystander");
    let bystander_run = run_id_of(CHAT, &bystander_key);
    let mut bystander = Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
    std::fs::write(dir.join(format!("{bystander_run}.log")), "x\n").unwrap();
    std::fs::write(dir.join(format!("{bystander_run}.pid")), bystander.id().to_string()).unwrap();
    std::fs::write(
        dir.join(format!("{bystander_run}.meta")),
        json!({ "chatId": CHAT, "key": bystander_key, "command": "sleep 30", "enginePid": dead_engine_pid() }).to_string(),
    )
    .unwrap();
    recovery_service(&rig).recover_command_runs();
    let (id, bystander_id) = (wake_message_id(CHAT, &key), wake_message_id(CHAT, &bystander_key));
    wait_for(|| wake_count(&rig, CHAT, &id) == 1 && wake_count(&rig, CHAT, &bystander_id) == 1, "both interrupted wakes").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = orphan.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "the orphaned command was never stopped");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.signal(), Some(15), "stopped by SIGTERM: {status:?}");
    assert!(bystander.try_wait().unwrap().is_none(), "an unrelated process was stopped");
    bystander.kill().unwrap();
    bystander.wait().unwrap();
    assert!(wake_text_of(&rig, CHAT, &id).unwrap().contains("interrupted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runaway_log_is_capped_and_the_wake_still_reads_the_end() {
    let rig = rig().await;
    configure_command_chat(&rig, CHAT, command_config(true, false, SandboxLevel::WorkspaceWrite));
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-cap", SPACE, &rig.core.device_id);
    let key = unique_key("cap");
    let command = "yes 0123456789abcdef | head -c 40000000; echo; echo 'test engine::broken ... FAILED'; echo done-marker; exit 1";
    tool(&rig, lease.token(), "run_command", json!({ "idempotencyKey": key, "command": command })).await.unwrap();
    let id = wake_message_id(CHAT, &key);
    wait_for(|| wake_count(&rig, CHAT, &id) == 1, "the exit wake").await;
    let run_id = run_id_of(CHAT, &key);
    let dir = runs_dir_of(&rig);
    let size = |ext: &str| std::fs::metadata(dir.join(format!("{run_id}.{ext}"))).map(|m| m.len()).unwrap();
    let cap = 16 * 1024 * 1024 + 64 * 1024;
    assert!(size("log") <= cap && size("log.old") <= cap, "{} {}", size("log"), size("log.old"));
    assert!(size("log") + size("log.old") < 40_000_000, "the oldest output was dropped");
    let log = std::fs::read_to_string(dir.join(format!("{run_id}.log"))).unwrap();
    assert!(log.ends_with("done-marker\n"));
    let text = wake_text_of(&rig, CHAT, &id).unwrap();
    assert!(text.contains("failed with exit code 1") && text.contains("done-marker"), "{text}");
    // Tens of megabytes are not left in the temp directory.
    let _ = std::fs::remove_file(dir.join(format!("{run_id}.log")));
    let _ = std::fs::remove_file(dir.join(format!("{run_id}.log.old")));
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_whose_wake_was_delivered_long_ago_are_cleaned_up_at_the_next_start() {
    let rig = rig().await;
    let dir = runs_dir_of(&rig);
    let age = |path: &Path, days: u64| {
        let when = std::time::SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    };
    let make = |label: &str, extensions: &[&str], days: u64| {
        let key = unique_key(label);
        let run_id = run_id_of(CHAT, &key);
        std::fs::write(
            dir.join(format!("{run_id}.meta")),
            json!({ "chatId": CHAT, "key": key, "command": "true", "enginePid": std::process::id() }).to_string(),
        )
        .unwrap();
        for extension in extensions {
            std::fs::write(dir.join(format!("{run_id}.{extension}")), "0").unwrap();
        }
        for extension in extensions.iter().chain(&["meta"]) {
            age(&dir.join(format!("{run_id}.{extension}")), days);
        }
        (key, run_id)
    };
    let (old_key, old_run) = make("old", &["log", "log.old", "exit", "woken"], 8);
    let (recent_key, recent_run) = make("recent", &["log", "exit", "woken"], 2);
    recovery_service(&rig).recover_command_runs();
    for extension in ["log", "log.old", "exit", "woken", "meta"] {
        assert!(!dir.join(format!("{old_run}.{extension}")).exists(), "{extension} of the old run remains");
    }
    assert!(dir.join(format!("{recent_run}.log")).exists() && dir.join(format!("{recent_run}.meta")).exists());
    let lease = rig.core.sessions.agent_tokens().mint(CHAT, "run-prune", SPACE, &rig.core.device_id);
    let gone = tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": old_key })).await.unwrap_err();
    assert!(gone.contains("no run was started"), "{gone}");
    assert_eq!(tool(&rig, lease.token(), "command_status", json!({ "idempotencyKey": recent_key })).await.unwrap()["state"], "exited");
}

// ---- Manager queue tools --------------------------------------------------

fn queue_ids(read: &Value) -> Vec<String> {
    read["entries"].as_array().unwrap().iter().map(|e| e["workItemId"].as_str().unwrap().to_owned()).collect()
}

async fn queue_rig() -> (Rig, ProjectRig, [String; 2]) {
    let (rig, p) = manager_rig().await;
    let t2 = create_item(&rig, "q-t2", "Ticket 2", Some(&p.tickets)).await;
    let t3 = create_item(&rig, "q-t3", "Ticket 3", Some(&p.tickets)).await;
    (rig, p, [t2, t3])
}

async fn qset(rig: &Rig, token: &str, item: &str, action: &str, key: &str, extra: Value) -> Result<Value, String> {
    let mut args = json!({ "workItemId": item, "action": action, "idempotencyKey": key });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        args[k] = v;
    }
    tool(rig, token, "work_queue_set", args).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_orders_its_queue_and_everyone_else_is_refused() {
    let (rig, p, [t2, t3]) = queue_rig().await;
    let (_lease, manager) = mint_token(&rig, CHAT);
    qset(&rig, &manager, &p.ticket, "place", "k-a", json!({})).await.unwrap();
    qset(&rig, &manager, &t2, "place", "k-b", json!({})).await.unwrap();
    let read = qset(&rig, &manager, &t3, "place", "k-c", json!({ "position": { "before": t2 } })).await.unwrap();
    assert_eq!(queue_ids(&read), vec![p.ticket.clone(), t3.clone(), t2.clone()]);
    assert_eq!(read["entries"][0]["position"], 1);
    // Offloaded work reads blocked with its reason; resuming clears it.
    let read = qset(&rig, &manager, &t3, "offload", "k-d", json!({ "reason": "waiting on QA" })).await.unwrap();
    assert_eq!(read["entries"][1]["state"], "blocked");
    assert_eq!(read["entries"][1]["waitingReason"], "waiting on QA");
    let read = qset(&rig, &manager, &t3, "resume", "k-e", json!({})).await.unwrap();
    assert_eq!(read["entries"][1]["state"], "ready");
    let before = tool(&rig, &manager, "work_queue_read", json!({})).await.unwrap();

    // A worker in the project, another project's holder and a role-less chat.
    for chat in [HAND_CHAT, OUTSIDE_CHAT, OTHER_CHAT] {
        let (_l, token) = mint_token(&rig, chat);
        let err = qset(&rig, &token, &t2, "place", "k-w", json!({ "position": "first" })).await.unwrap_err();
        assert!(err.contains("only the project's Manager"), "{chat}: {err}");
        assert!(tool(&rig, &token, "work_queue_read", json!({})).await.is_err());
        assert!(tool(&rig, &token, "work_queue_pause", json!({ "paused": true, "reason": "x", "idempotencyKey": "k-w2" })).await.is_err());
    }
    // The Manager may not reach across projects, by item, anchor or project.
    let outside = qset(&rig, &manager, &p.other_tickets, "place", "k-o1", json!({})).await.unwrap_err();
    assert!(outside.contains("outside your project"), "{outside}");
    let anchor = qset(&rig, &manager, &t2, "place", "k-o2", json!({ "position": { "after": p.other_tickets } })).await.unwrap_err();
    assert!(anchor.contains("outside your project"), "{anchor}");
    let project = tool(&rig, &manager, "work_queue_read", json!({ "projectId": p.other })).await.unwrap_err();
    assert!(project.contains("own project"), "{project}");
    // Nothing moved, and a fresh token reads the same order after the writes.
    let (_l2, fresh) = mint_token(&rig, CHAT);
    assert_eq!(tool(&rig, &fresh, "work_queue_read", json!({})).await.unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_off_moves_the_queue_permission_at_once() {
    let (rig, p, [t2, _t3]) = queue_rig().await;
    let (_lease, old) = mint_token(&rig, CHAT);
    qset(&rig, &old, &p.ticket, "place", "h-a", json!({})).await.unwrap();
    qset(&rig, &old, &t2, "place", "h-b", json!({})).await.unwrap();
    rig.core.workspace.rotate_standing_role_for_chat(CHAT, HAND_CHAT).unwrap();
    // The superseded chat loses it on its next call, same token.
    let err = qset(&rig, &old, &t2, "place", "h-c", json!({ "position": "first" })).await.unwrap_err();
    assert!(err.contains("only the project's Manager"), "{err}");
    let (_l, new) = mint_token(&rig, HAND_CHAT);
    let read = qset(&rig, &new, &t2, "place", "h-d", json!({ "position": "first" })).await.unwrap();
    assert_eq!(queue_ids(&read), vec![t2.clone(), p.ticket.clone()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_queue_operation_does_not_move_twice() {
    let (rig, p, [t2, _t3]) = queue_rig().await;
    let (_lease, manager) = mint_token(&rig, CHAT);
    qset(&rig, &manager, &p.ticket, "place", "r-a", json!({})).await.unwrap();
    qset(&rig, &manager, &t2, "place", "r-b", json!({})).await.unwrap();
    // Lost reply: the same call again after another move changes nothing.
    qset(&rig, &manager, &p.ticket, "place", "r-c", json!({ "position": { "after": t2 } })).await.unwrap();
    qset(&rig, &manager, &p.ticket, "place", "r-d", json!({ "position": "first" })).await.unwrap();
    let read = qset(&rig, &manager, &p.ticket, "place", "r-c", json!({ "position": { "after": t2 } })).await.unwrap();
    assert_eq!(queue_ids(&read), vec![p.ticket.clone(), t2.clone()]);
    // The same key with other inputs is a conflict, a new key is a new move.
    let err = qset(&rig, &manager, &t2, "place", "r-c", json!({ "position": "first" })).await.unwrap_err();
    assert!(err.contains("operationId") || err.contains("idempotencyKey") || err.contains("different"), "{err}");
    let read = qset(&rig, &manager, &t2, "place", "r-e", json!({ "position": "first" })).await.unwrap();
    assert_eq!(queue_ids(&read), vec![t2, p.ticket]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_queues_prerequisites_and_a_dependent_waits_with_its_reason() {
    let (rig, p, [t2, t3]) = queue_rig().await;
    let (_lease, manager) = mint_token(&rig, CHAT);
    qset(&rig, &manager, &t2, "place", "p-a", json!({})).await.unwrap();
    // A prerequisite need not be queued. The dependent waits and says why.
    let read = qset(&rig, &manager, &t2, "prerequisites", "p-b", json!({ "prerequisites": [p.ticket] })).await.unwrap();
    assert_eq!(read["entries"][0]["state"], "waiting");
    assert_eq!(read["entries"][0]["waitingOn"][0]["workItemId"], json!(p.ticket));
    assert_eq!(read["entries"][0]["waitingOn"][0]["reason"], "notDone");
    // A lost reply: the same key and list again changes nothing and succeeds.
    let again = qset(&rig, &manager, &t2, "prerequisites", "p-b", json!({ "prerequisites": [p.ticket] })).await.unwrap();
    assert_eq!(again, read);
    // The same key with another list is a conflict.
    let err = qset(&rig, &manager, &t2, "prerequisites", "p-b", json!({ "prerequisites": [t3] })).await.unwrap_err();
    assert!(err.contains("operationId") || err.contains("idempotencyKey") || err.contains("different"), "{err}");
    // Landing as the criterion reads "to land" once the prerequisite requires it.
    let read = qset(&rig, &manager, &p.ticket, "completion", "p-c", json!({ "completion": "landed" })).await.unwrap();
    assert_eq!(read["entries"][0]["waitingOn"][0]["reason"], "notLanded");
    // The entry reads back what was set, and an item with neither set shows nothing extra.
    assert_eq!(read["entries"][0]["prerequisites"], json!([p.ticket]));
    assert!(read["entries"][0].get("completion").is_none());
    let bad = qset(&rig, &manager, &p.ticket, "completion", "p-d", json!({ "completion": "soon" })).await.unwrap_err();
    assert!(bad.contains("reported or landed"), "{bad}");
    // Items outside the project, the item itself and a mutual wait are refused.
    let outside = qset(&rig, &manager, &t2, "prerequisites", "p-e", json!({ "prerequisites": [p.other_tickets] })).await.unwrap_err();
    assert!(outside.contains("outside your project"), "{outside}");
    let many: Vec<String> = (0..9).map(|i| format!("no-such-{i}")).collect();
    let too_many = qset(&rig, &manager, &t2, "prerequisites", "p-many", json!({ "prerequisites": many })).await.unwrap_err();
    assert!(too_many.contains("at most 8"), "{too_many}");
    let own = qset(&rig, &manager, &t2, "prerequisites", "p-f", json!({ "prerequisites": [t2] })).await.unwrap_err();
    assert!(own.contains("its own prerequisite"), "{own}");
    let cycle = qset(&rig, &manager, &p.ticket, "prerequisites", "p-g", json!({ "prerequisites": [t2] })).await.unwrap_err();
    assert!(cycle.contains("wait on each other"), "{cycle}");
    // A worker, another project's holder and a superseded Manager cannot edit them.
    for chat in [HAND_CHAT, OUTSIDE_CHAT, OTHER_CHAT] {
        let (_l, token) = mint_token(&rig, chat);
        let err = qset(&rig, &token, &t2, "prerequisites", "p-h", json!({ "prerequisites": [] })).await.unwrap_err();
        assert!(err.contains("only the project's Manager"), "{chat}: {err}");
    }
    rig.core.workspace.rotate_standing_role_for_chat(CHAT, HAND_CHAT).unwrap();
    let superseded = qset(&rig, &manager, &t2, "prerequisites", "p-i", json!({ "prerequisites": [] })).await.unwrap_err();
    assert!(superseded.contains("only the project's Manager"), "{superseded}");
    // An empty list clears them, and the entry is ready again.
    let (_l, new) = mint_token(&rig, HAND_CHAT);
    let read = qset(&rig, &new, &t2, "prerequisites", "p-j", json!({ "prerequisites": [] })).await.unwrap();
    assert_eq!(read["entries"][0]["state"], "ready");
    assert!(read["entries"][0].get("waitingOn").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_may_lift_only_a_pause_a_manager_set() {
    let (rig, p, _) = queue_rig().await;
    let (_lease, manager) = mint_token(&rig, CHAT);
    let pause = |token: &str, paused: bool, reason: Option<&str>, key: &str| {
        let mut args = json!({ "paused": paused, "idempotencyKey": key });
        if let Some(reason) = reason {
            args["reason"] = json!(reason);
        }
        let token = token.to_owned();
        let rig = &rig;
        async move { tool(rig, &token, "work_queue_pause", args).await }
    };
    // A reason is required, and shows with who paused.
    assert!(pause(&manager, true, None, "p-0").await.is_err());
    let read = pause(&manager, true, Some("release freeze"), "p-1").await.unwrap();
    assert_eq!(read["paused"], true);
    assert_eq!(read["pause"]["by"], "manager");
    assert_eq!(read["pause"]["reason"], "release freeze");
    assert_eq!(read["pause"]["chatId"], CHAT);
    assert!(pause(&manager, false, Some(""), "p-2").await.is_err());
    assert_eq!(pause(&manager, false, Some("freeze over"), "p-3").await.unwrap()["paused"], false);

    // The user pauses from the desktop: the Manager can neither lift nor replace it.
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "setQueuePaused", "workItemId": p.project, "paused": true, "operationId": "u-pause" })).await.unwrap();
    let err = pause(&manager, false, Some("go"), "p-4").await.unwrap_err();
    assert!(err.contains("paused by a user or admin"), "{err}");
    assert!(pause(&manager, true, Some("mine now"), "p-5").await.is_err());
    assert_eq!(snapshot(&rig).item(&p.project).unwrap().queue_pause.as_ref().map(|n| n.by), Some(mast_proto::QueuePausedBy::User));
    assert!(snapshot(&rig).item(&p.project).unwrap().queue_paused);
    // An admin chat may lift it, and its own pause is also the Manager's to leave alone.
    rig.core.sessions.agent_tokens().admin().grant(OTHER_CHAT, SPACE, Some(600), true);
    let (_l, admin_token) = mint_token(&rig, OTHER_CHAT);
    let lifted = tool(&rig, &admin_token, "work_queue_pause", json!({ "projectId": p.project, "paused": false, "reason": "ok", "idempotencyKey": "a-1" })).await.unwrap();
    assert_eq!(lifted["paused"], false);
    let paused = tool(&rig, &admin_token, "work_queue_pause", json!({ "projectId": p.project, "paused": true, "reason": "audit", "idempotencyKey": "a-2" })).await.unwrap();
    assert_eq!(paused["pause"]["by"], "admin");
    assert!(pause(&manager, false, Some("go"), "p-6").await.is_err());
    // An admin needs no reason to resume or pause; the pause is then recorded without one.
    let lifted = tool(&rig, &admin_token, "work_queue_pause", json!({ "projectId": p.project, "paused": false, "idempotencyKey": "a-3" })).await.unwrap();
    assert_eq!(lifted["paused"], false);
    let paused = tool(&rig, &admin_token, "work_queue_pause", json!({ "projectId": p.project, "paused": true, "idempotencyKey": "a-4" })).await.unwrap();
    assert_eq!(paused["pause"]["by"], "admin");
    assert!(paused["pause"].get("reason").is_none_or(|reason| reason.is_null()), "{paused}");
    // The user lifts anything.
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "setQueuePaused", "workItemId": p.project, "paused": false, "operationId": "u-resume" })).await.unwrap();
    assert!(!snapshot(&rig).item(&p.project).unwrap().queue_paused);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_and_personal_assistant_queue_paths_still_work() {
    let (rig, p, [t2, _t3]) = queue_rig().await;
    let order = |rig: &Rig| {
        let now = chrono_now();
        snapshot(rig)
            .work_queue(&p.project, now, &|_| Default::default(), &|_| Default::default())
            .into_iter()
            .map(|e| e.work_item_id)
            .collect::<Vec<_>>()
    };
    // Desktop (user actor).
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "setQueuePosition", "workItemId": p.ticket, "change": { "type": "place", "placement": { "type": "last" } }, "operationId": "d-1" })).await.unwrap();
    assert_eq!(order(&rig), vec![p.ticket.clone()]);
    // Personal assistant through mast_manage.
    rig.client.call(methods::SET_PERSONAL_ASSISTANT, json!({ "chatId": OTHER_CHAT })).await.unwrap();
    let (_l, pa) = mint_token(&rig, OTHER_CHAT);
    tool(&rig, &pa, mast_proto::mast_management::TOOL, json!({ "method": "WorkOutlineMutate", "params": { "op": "setQueuePosition", "workItemId": t2, "change": { "type": "place", "placement": { "type": "first" } }, "operationId": "pa-1" } })).await.unwrap();
    assert_eq!(order(&rig), vec![t2.clone(), p.ticket.clone()]);
    tool(&rig, &pa, mast_proto::mast_management::TOOL, json!({ "method": "WorkOutlineMutate", "params": { "op": "setQueuePaused", "workItemId": p.project, "paused": true, "operationId": "pa-2" } })).await.unwrap();
    assert!(snapshot(&rig).item(&p.project).unwrap().queue_paused);
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_writer_rechecks_the_queue_guard_under_its_lock() {
    use mast_engine::work_outline::ManagerGuard;
    use mast_proto::{QueueChange, SetQueuePositionParams, WorkOutlineMutation, WorkOutlinePlacement};
    let (rig, p, _) = queue_rig().await;
    let outline = rig.service.work_outline();
    let actor = outline.agent_actor(CHAT, None);
    let place = |op: &str, item: &str| {
        WorkOutlineMutation::SetQueuePosition(SetQueuePositionParams {
            work_item_id: item.to_owned(),
            change: QueueChange::Place { placement: WorkOutlinePlacement::Last },
            operation_id: op.to_owned(),
        })
    };
    let guard = |chat: &str, inside: Vec<String>| ManagerGuard { chat_id: chat.to_owned(), root: p.project.clone(), inside, ..Default::default() };
    // A non-holder (a predecessor after a hand-off) and a target that moved out.
    let err = outline.mutate_guarded(&place("lg-1", &p.ticket), &actor, Some(SPACE), Some(&guard(OTHER_CHAT, vec![p.ticket.clone()]))).unwrap_err();
    assert!(err.to_wire_message().contains("handed off or ended"), "{err:?}");
    let err = outline.mutate_guarded(&place("lg-2", &p.ticket), &actor, Some(SPACE), Some(&guard(CHAT, vec![p.other_tickets.clone()]))).unwrap_err();
    assert!(err.to_wire_message().contains("no longer inside"), "{err:?}");
    assert!(snapshot(&rig).item(&p.ticket).unwrap().queue_order_key.is_none());
    outline.mutate_guarded(&place("lg-3", &p.ticket), &actor, Some(SPACE), Some(&guard(CHAT, vec![p.ticket.clone()]))).unwrap();
    assert!(snapshot(&rig).item(&p.ticket).unwrap().queue_order_key.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forged_pause_source_is_ignored_and_pause_edge_cases_hold() {
    let (rig, p, _) = queue_rig().await;
    let (_lease, manager) = mint_token(&rig, CHAT);
    // A desktop request claiming to be a Manager pause is still the user's.
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "setQueuePaused", "workItemId": p.project, "paused": true, "by": "manager", "operationId": "f-1" })).await.unwrap();
    assert_eq!(snapshot(&rig).item(&p.project).unwrap().queue_pause.as_ref().map(|n| n.by), Some(mast_proto::QueuePausedBy::User));
    let args = |paused: bool, reason: &str, key: &str, project: Option<&str>| {
        let mut a = json!({ "paused": paused, "reason": reason, "idempotencyKey": key });
        if let Some(project) = project {
            a["projectId"] = json!(project);
        }
        a
    };
    assert!(tool(&rig, &manager, "work_queue_pause", args(false, "go", "f-2", None)).await.is_err());
    rig.client.call(methods::WORK_OUTLINE_MUTATE, json!({ "op": "setQueuePaused", "workItemId": p.project, "paused": false, "operationId": "f-3" })).await.unwrap();
    // Not paused: a Manager resume is a no-op; another project is refused; a long reason is refused.
    assert_eq!(tool(&rig, &manager, "work_queue_pause", args(false, "fine", "f-4", None)).await.unwrap()["paused"], false);
    assert!(tool(&rig, &manager, "work_queue_pause", args(true, "x", "f-5", Some(&p.other))).await.is_err());
    assert!(tool(&rig, &manager, "work_queue_pause", args(true, &"x".repeat(501), "f-6", None)).await.is_err());
    // A retried pause with the same key leaves the one record.
    for _ in 0..2 {
        assert_eq!(tool(&rig, &manager, "work_queue_pause", args(true, "freeze", "f-7", None)).await.unwrap()["pause"]["reason"], "freeze");
    }
    // The admin replaces nothing a Manager can then undo; the Manager cannot replace an admin pause.
    rig.client.call(methods::SET_PERSONAL_ASSISTANT, json!({ "chatId": OTHER_CHAT })).await.unwrap();
    let (_l, pa) = mint_token(&rig, OTHER_CHAT);
    tool(&rig, &pa, mast_proto::mast_management::TOOL, json!({ "method": "WorkOutlineMutate", "params": { "op": "setQueuePaused", "workItemId": p.project, "paused": false, "operationId": "f-8" } })).await.unwrap();
    tool(&rig, &pa, mast_proto::mast_management::TOOL, json!({ "method": "WorkOutlineMutate", "params": { "op": "setQueuePaused", "workItemId": p.project, "paused": true, "operationId": "f-9" } })).await.unwrap();
    assert_eq!(snapshot(&rig).item(&p.project).unwrap().queue_pause.as_ref().map(|n| n.by), Some(mast_proto::QueuePausedBy::User));
    assert!(tool(&rig, &manager, "work_queue_pause", args(false, "go", "f-10", None)).await.is_err());
}
