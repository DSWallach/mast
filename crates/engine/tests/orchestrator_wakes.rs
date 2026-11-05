//! Worker-event wakes for orchestrator chats, against a fake harness and a
//! fake clock: batching, idle-only delivery, the user-first holds, the
//! interrupt of an event-started turn, settings, dedupe and non-children.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_engine::orchestrator_wakes::{Clock, OrchestratorWakes, USER_QUIET_MS, WAKE_WINDOW_MS};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, Session, SessionStatus,
    SteeringMode, WakeEventKind, WakeSettings, WorkOutlineSnapshot,
};

const ORCH: &str = "orch";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_reparents_workers_and_moves_held_reports() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("handoff-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("source", None), ("successor", None), ("worker", Some("source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("handoff-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    let settings = WakeSettings { auto_continue: true, ..WakeSettings::default() };
    rig.wakes.set_settings("source", settings).await;
    rig.wakes.report_to_parent("worker", "before hand-off").await.unwrap();
    assert_eq!(rig.wakes.hand_off_orchestrator("source", "successor", "source").await.unwrap(), vec!["worker"]);
    assert_eq!(rig.wakes.hand_off_orchestrator("source", "successor", "source").await.unwrap(), vec!["worker"]);
    assert_eq!(rig.core.workspace.chat("worker").unwrap().unwrap().parent_chat_id.as_deref(), Some("successor"));
    assert_eq!(rig.wakes.settings("successor"), settings);
    assert!(rig.wakes.status("source").held.is_empty());
    assert_eq!(rig.wakes.status("successor").held.len(), 1);
    let result = rig.wakes.report_to_parent("worker", "after hand-off").await.unwrap();
    assert_eq!(result.parent_chat_id, "successor");
    assert_eq!(rig.wakes.status("successor").held.len(), 2);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_reclaims_a_queued_worker_report() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("queued-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("queued-source", None), ("queued-successor", None), ("queued-worker", Some("queued-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("queued-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.note_user_activity("queued-successor", Some(true)).await;
    rig.core.doc_host.queue_message("queued-source", "opening", Vec::new()).unwrap();
    rig.wait_for(|| rig.core.sessions.turn_in_flight("queued-source"), "source turn").await;
    let handle = rig.core.doc_host.open("queued-source").unwrap();
    handle.doc().push_queued(&mast_doc::QueuedMessage {
        id: "queued-wake".into(), text: "Worker updates: queued report".into(), attachments: vec![],
        hold_for_turn_end: true, issued_by: rig.core.device_id.clone(), issued_at: 1,
        edited_at: None, delivery_gate: None,
    }).unwrap();
    rig.core.doc_host.event_turns().register("queued-source", "queued-wake".into(), vec![
        mast_engine::orchestrator_wakes::WakeEvent {
            id: "event-1".into(), key: "report-1".into(), worker_chat_id: "queued-worker".into(),
            worker_title: None, kind: WakeEventKind::WorkerReport, summary: "queued report".into(),
            report: Some("report body".into()), at_ms: 1,
        }
    ]);
    rig.wakes.hand_off_orchestrator("queued-source", "queued-successor", "queued-source").await.unwrap();
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert!(rig.wakes.status("queued-source").held.is_empty());
    assert_eq!(rig.wakes.status("queued-successor").held.len(), 1);
    rig.wakes.note_user_activity("queued-successor", Some(false)).await;
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate("queued-successor", false).await;
    rig.wait_for(|| rig.wake_prompts().iter().any(|p| p.contains("report body")), "transferred report").await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_finishes_after_registry_write() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("retry-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("retry-source", None), ("retry-successor", None), ("retry-worker", Some("retry-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("retry-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    rig.wakes.report_to_parent("retry-worker", "pending report").await.unwrap();
    rig.core.workspace.hand_off_chat_children("retry-source", "retry-successor", "retry-source", 1).unwrap();
    rig.wakes.hand_off_orchestrator("retry-source", "retry-successor", "retry-source").await.unwrap();
    assert!(rig.wakes.status("retry-source").held.is_empty());
    assert_eq!(rig.wakes.status("retry-successor").held.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_after_restart_copies_settings_once() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("restart-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for (id, parent) in [("restart-source", None), ("restart-successor", None), ("restart-worker", Some("restart-source"))] {
        rig.core.workspace.create_chat_with_parent(id, Some("restart-space"), Some(&rig.core.device_id), None, None, parent.map(str::to_owned)).unwrap();
    }
    let settings = WakeSettings { auto_continue: true, turn_finished: true, ..WakeSettings::default() };
    rig.wakes.set_settings("restart-source", settings).await;
    rig.core.workspace.hand_off_chat_children("restart-source", "restart-successor", "restart-source", 1).unwrap();
    let tick = rig.clock.clone();
    let restarted = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(rig.store.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    assert_eq!(restarted.hand_off_orchestrator("restart-source", "restart-successor", "restart-source").await.unwrap(), vec!["restart-worker"]);
    assert_eq!(restarted.settings("restart-successor"), settings);
    let changed = WakeSettings { auto_continue: false, ..settings };
    restarted.set_settings("restart-successor", changed).await;
    restarted.hand_off_orchestrator("restart-source", "restart-successor", "restart-source").await.unwrap();
    assert_eq!(restarted.settings("restart-successor"), changed);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_off_retry_after_settings_write_failure() {
    let rig = Rig::new().await;
    rig.core.workspace.create_space("write-space", &rig.core.device_id, "/tmp", None, false).unwrap();
    for id in ["write-source", "write-successor"] {
        rig.core.workspace.create_chat_with_parent(id, Some("write-space"), Some(&rig.core.device_id), None, None, None).unwrap();
    }
    let directory = rig.store.parent().unwrap().join("missing-settings-directory");
    let path = directory.join("wakes.json");
    let tick = rig.clock.clone();
    let wakes = OrchestratorWakes::new(rig.core.sessions.clone(), rig.core.doc_host.clone(),
        rig.core.workspace.clone(), Some(path.clone()), Arc::new(move || tick.load(Ordering::SeqCst)));
    let settings = WakeSettings { auto_continue: true, ..WakeSettings::default() };
    wakes.set_settings("write-source", settings).await;
    assert!(wakes.hand_off_orchestrator("write-source", "write-successor", "write-source").await.is_err());
    std::fs::create_dir(&directory).unwrap();
    wakes.hand_off_orchestrator("write-source", "write-successor", "write-source").await.unwrap();
    assert_eq!(wakes.settings("write-successor"), settings);
    assert!(path.exists());
    rig.core.shutdown().await;
}

/// A turn that ends only when the test says so.
struct HeldHarness {
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Harness for HeldHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Held"
    }
    fn supports_steering(&self) -> bool {
        true
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
        self.prompts.lock().unwrap().push(request.prompt.clone());
        let mut finish = self.finish.subscribe();
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: "sess-wake".into(),
            assistant_message_id: format!("a-{}", request.prompt.len()),
        })]);
        let done = futures::stream::once(async move {
            let _ = finish.recv().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-wake".into()),
            })
        });
        Ok(started.chain(done).boxed())
    }
}

struct Rig {
    core: EngineCore,
    wakes: OrchestratorWakes,
    clock: Arc<AtomicI64>,
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
    store: std::path::PathBuf,
}

impl Rig {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.keep();
        let (finish, _) = tokio::sync::broadcast::channel(16);
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(HeldHarness {
            finish: finish.clone(),
            prompts: prompts.clone(),
        }));
        let core = EngineCore::assemble(
            &path.join("data"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
        )
        .expect("engine core assembles");
        let store = path.join("wakes.json");
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let tick = clock.clone();
        let clock_fn: Clock = Arc::new(move || tick.load(Ordering::SeqCst));
        // A separate, inert service on the fake clock; the core's own one
        // keeps watching on the real clock and never reaches its window.
        let wakes = OrchestratorWakes::new(
            core.sessions.clone(),
            core.doc_host.clone(),
            core.workspace.clone(),
            Some(store.clone()),
            clock_fn,
        );
        let rig = Self {
            core,
            wakes,
            clock,
            finish,
            prompts,
            store,
        };
        rig.chat(ORCH, None);
        rig.chat("w1", Some(ORCH));
        rig.chat("w2", Some(ORCH));
        rig.chat("stranger", None);
        rig.wakes
            .ingest_chats(&rig.core.workspace.read_chats().unwrap())
            .await;
        rig.wakes
            .ingest_sessions(&[
                session("w1", SessionStatus::Working),
                session("w2", SessionStatus::Working),
                session("stranger", SessionStatus::Working),
            ])
            .await;
        rig
    }

    fn chat(&self, id: &str, parent: Option<&str>) {
        self.core
            .workspace
            .create_chat_with_parent(
                id,
                None,
                Some(&self.core.device_id),
                None,
                None,
                parent.map(str::to_owned),
            )
            .unwrap();
        self.core
            .workspace
            .rename_chat(id, &format!("Title {id}"))
            .unwrap();
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    async fn worker_errors(&self, id: &str) {
        self.wakes
            .ingest_sessions(&[session(id, SessionStatus::Errored)])
            .await;
    }

    fn wake_prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("Worker updates:"))
            .cloned()
            .collect()
    }

    async fn wait_for<F: FnMut() -> bool>(&self, mut f: F, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn session(chat: &str, status: SessionStatus) -> Session {
    Session {
        last_completed_turn: None,
        awaiting_input: None,
        last_turn_tool_failures: None,
        chat_id: chat.into(),
        device_id: "d".into(),
        status,
        started_at: None,
        updated_at: chrono::Utc::now(),
    }
}

fn auto_continue_worker(rig: &Rig, chat: &str, status: &str) -> String {
    let space = "space-auto";
    rig.core
        .workspace
        .create_space(space, &rig.core.device_id, "/tmp", None, false)
        .unwrap();
    rig.core
        .workspace
        .create_chat_with_parent(chat, Some(space), None, None, None, Some(ORCH.into()))
        .unwrap();
    rig.core.workspace.rename_chat(chat, chat).unwrap();
    let actor = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::User,
        device_id: rig.core.device_id.clone(),
        ..Default::default()
    };
    let operation = format!("create-{chat}");
    let item = mast_proto::derive_id(&operation, mast_proto::roles::ITEM);
    let create: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "createWorkItem", "spaceId": space, "title": "Worker item", "status": status, "operationId": operation
    })).unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&create, &actor, &[])
        .unwrap();
    let assign: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "assignChatToWorkItem", "chatId": chat, "workItemId": item, "operationId": format!("assign-{chat}")
    })).unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&assign, &actor, &[])
        .unwrap();
    item
}

async fn auto_continue_finish(rig: &Rig, chat: &str) {
    rig.wakes
        .ingest_sessions(&[session(chat, SessionStatus::Working)])
        .await;
    let mut done = session(chat, SessionStatus::Idle);
    done.last_completed_turn = Some(format!("completed-{chat}"));
    rig.wakes.ingest_sessions(&[done]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_only_queues_for_an_open_unblocked_item() {
    let rig = Rig::new().await;
    auto_continue_worker(&rig, "off-worker", "active");
    auto_continue_finish(&rig, "off-worker").await;
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                auto_continue: true,
                ..Default::default()
            },
        )
        .await;
    auto_continue_worker(&rig, "completed-worker", "completed");
    auto_continue_finish(&rig, "completed-worker").await;
    let item = auto_continue_worker(&rig, "blocked-worker", "active");
    let actor = mast_proto::WorkOutlineActor {
        kind: mast_proto::WorkOutlineActorKind::Agent,
        device_id: rig.core.device_id.clone(),
        chat_id: Some("blocked-worker".into()),
        ..Default::default()
    };
    let action: mast_proto::WorkOutlineMutation = serde_json::from_value(serde_json::json!({
        "op": "createUserActionRequest", "workItemId": item, "origin": {"chatId":"blocked-worker"},
        "text": "Need input", "operationId": "blocked-action"
    }))
    .unwrap();
    rig.core
        .workspace
        .work_outline_mutate(&action, &actor, &[])
        .unwrap();
    auto_continue_finish(&rig, "blocked-worker").await;
    auto_continue_worker(&rig, "user-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("user-worker", SessionStatus::Working)])
        .await;
    rig.core
        .doc_host
        .open("user-worker")
        .unwrap()
        .write_user_message(
            "user-during-turn",
            "Please wait",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let mut done = session("user-worker", SessionStatus::Idle);
    done.last_completed_turn = Some("user-worker-turn".into());
    rig.wakes.ingest_sessions(&[done]).await;
    auto_continue_worker(&rig, "failed-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("failed-worker", SessionStatus::Working)])
        .await;
    let mut failed = session("failed-worker", SessionStatus::Idle);
    failed.last_completed_turn = Some("failed-worker-turn".into());
    failed.last_turn_tool_failures = Some(mast_proto::ToolFailures {
        count: 1,
        tools: vec!["tool".into()],
    });
    rig.wakes.ingest_sessions(&[failed]).await;
    auto_continue_worker(&rig, "awaiting-worker", "active");
    rig.wakes
        .ingest_sessions(&[session("awaiting-worker", SessionStatus::Working)])
        .await;
    rig.wakes
        .ingest_sessions(&[session("awaiting-worker", SessionStatus::AwaitingInput)])
        .await;
    rig.settle().await;
    assert!(
        !rig.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("auto-continue"))
    );
    auto_continue_worker(&rig, "open-worker", "active");
    auto_continue_finish(&rig, "open-worker").await;
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("auto-continue 1/3]"))
        },
        "worker continuation",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_stops_after_three_and_resets_on_a_user_message() {
    let rig = Rig::new().await;
    let worker = "capped-worker";
    auto_continue_worker(&rig, worker, "active");
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                auto_continue: true,
                ..Default::default()
            },
        )
        .await;
    auto_continue_finish(&rig, worker).await;
    let sessions = rig.core.sessions.watch_sessions();
    for number in 1..=3 {
        rig.wait_for(
            || {
                rig.prompts
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|prompt| prompt.contains(&format!("auto-continue {number}/3]")))
            },
            "continuation prompt",
        )
        .await;
        rig.wait_for(
            || {
                sessions
                    .borrow()
                    .iter()
                    .any(|s| s.chat_id == worker && s.status == SessionStatus::Working)
            },
            "working worker",
        )
        .await;
        let running = sessions
            .borrow()
            .iter()
            .find(|s| s.chat_id == worker)
            .unwrap()
            .clone();
        rig.wakes.ingest_sessions(&[running]).await;
        let _ = rig.finish.send(());
        rig.wait_for(
            || {
                sessions.borrow().iter().any(|s| {
                    s.chat_id == worker
                        && s.status == SessionStatus::Idle
                        && s.last_completed_turn.is_some()
                })
            },
            "completed worker",
        )
        .await;
        let done = sessions
            .borrow()
            .iter()
            .find(|s| s.chat_id == worker)
            .unwrap()
            .clone();
        rig.wakes.ingest_sessions(&[done]).await;
    }
    let stopped = rig
        .wakes
        .status(ORCH)
        .held
        .into_iter()
        .filter(|event| event.kind == WakeEventKind::AutoContinueStopped)
        .count();
    assert_eq!(stopped, 1);
    let mut running = session(worker, SessionStatus::Working);
    running.last_completed_turn = sessions
        .borrow()
        .iter()
        .find(|s| s.chat_id == worker)
        .unwrap()
        .last_completed_turn
        .clone();
    rig.wakes.ingest_sessions(&[running]).await;
    let mut done = session(worker, SessionStatus::Idle);
    done.last_completed_turn = Some("another-completed-turn".into());
    rig.wakes.ingest_sessions(&[done]).await;
    assert_eq!(
        rig.wakes
            .status(ORCH)
            .held
            .iter()
            .filter(|event| event.kind == WakeEventKind::AutoContinueStopped)
            .count(),
        1
    );
    rig.core
        .doc_host
        .open(worker)
        .unwrap()
        .write_user_message(
            "new-user-message",
            "Continue",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let last = sessions
        .borrow()
        .iter()
        .find(|s| s.chat_id == worker)
        .unwrap()
        .last_completed_turn
        .clone();
    let mut running = session(worker, SessionStatus::Working);
    running.last_completed_turn = last;
    rig.wakes.ingest_sessions(&[running]).await;
    let mut done = session(worker, SessionStatus::Idle);
    done.last_completed_turn = Some("after-user-message".into());
    rig.wakes.ingest_sessions(&[done]).await;
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .filter(|prompt| prompt.contains("auto-continue 1/3]"))
                .count()
                == 2
        },
        "reset continuation",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_orchestrator_override_disables_global_default() {
    let mut rig = Rig::new().await;
    std::fs::write(
        &rig.store,
        r#"{"autoContinue":true,"orchestrators":{"orch":{"autoContinue":false}}}"#,
    )
    .unwrap();
    rig.wakes = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 1_000_000),
    );
    rig.wakes.ingest_sessions(&[]).await;
    auto_continue_worker(&rig, "override-worker", "active");
    auto_continue_finish(&rig, "override-worker").await;
    rig.settle().await;
    assert!(
        !rig.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|prompt| prompt.contains("auto-continue"))
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_errors_batch_into_one_system_message_after_the_window() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(10_000);
    rig.worker_errors("w2").await;
    // Not before the window ends.
    rig.advance(10_000);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 2);

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the wake message")
        .await;
    let prompts = rig.wake_prompts();
    assert_eq!(prompts.len(), 1, "ONE message for both workers");
    assert!(prompts[0].contains("Title w1") && prompts[0].contains("Title w2"));
    assert!(prompts[0].contains("not from the user"));
    assert!(rig.wakes.status(ORCH).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_children_wake_their_orchestrator() {
    let rig = Rig::new().await;
    rig.worker_errors("stranger").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    let status = rig.wakes.status(ORCH);
    assert!(status.is_orchestrator);
    assert!(status.held.is_empty() && status.recent.is_empty());
    assert!(!rig.wakes.status("w1").is_orchestrator);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_turn_holds_the_batch_until_it_ends() {
    let rig = Rig::new().await;
    rig.core
        .doc_host
        .queue_message(ORCH, "opening", Vec::new())
        .unwrap();
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the orchestrator's turn",
    )
    .await;
    rig.worker_errors("w1").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty(), "never delivered mid-turn");
    // Even Deliver now cannot interrupt the turn.
    rig.wakes.deliver_now(ORCH).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);

    let _ = rig.finish.send(());
    rig.wait_for(
        || !rig.core.sessions.turn_in_flight(ORCH),
        "the turn to end",
    )
    .await;
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "delivery after the turn")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draft_and_recent_activity_hold_delivery() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.advance(10 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "a draft holds however long it sits"
    );
    assert_eq!(
        rig.wakes.status(ORCH).hold_reason,
        Some(mast_proto::WakeHoldReason::Draft)
    );

    rig.advance(1_000);
    rig.wakes.note_user_activity(ORCH, Some(false)).await;
    rig.advance(USER_QUIET_MS / 2);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "the user was here a minute ago"
    );

    rig.advance(USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "delivery once the user is gone",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliver_now_skips_the_window_and_the_draft() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.wakes.deliver_now(ORCH).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the forced delivery")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_message_interrupts_an_event_started_turn_and_runs_next() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the event turn to start")
        .await;
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the event turn in flight",
    )
    .await;
    // The wake service sees the turn running (the session watch does this).
    rig.wakes
        .ingest_sessions(&[session(ORCH, SessionStatus::Working)])
        .await;

    // Mikey types: the event turn is interrupted (it never finished — the
    // test never sent `finish`), and his message is the very next turn.
    rig.core
        .doc_host
        .queue_message(ORCH, "hello from the user", Vec::new())
        .unwrap();
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "hello from the user")
        },
        "the user's message to run next",
    )
    .await;
    let prompts = rig.prompts.lock().unwrap().clone();
    let wake = prompts
        .iter()
        .position(|p| p.contains("Worker updates:"))
        .unwrap();
    let user = prompts
        .iter()
        .position(|p| p == "hello from the user")
        .unwrap();
    assert!(wake < user);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_default_and_gate_event_kinds() {
    let rig = Rig::new().await;
    assert_eq!(rig.wakes.settings(ORCH), WakeSettings::default());
    // Turn-finished is off by default: it is logged (unseen mark) not held.
    rig.wakes
        .ingest_sessions(&[session("w1", SessionStatus::Idle)])
        .await;
    let status = rig.wakes.status(ORCH);
    assert!(status.held.is_empty());
    assert_eq!(status.recent.len(), 1);
    assert_eq!(status.recent[0].kind, WakeEventKind::TurnFinished);
    assert!(status.recent[0].unseen);

    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                turn_finished: true,
                ..WakeSettings::default()
            },
        )
        .await;
    rig.wakes
        .ingest_sessions(&[session("w2", SessionStatus::Idle)])
        .await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);

    // Master switch off clears what was held and holds nothing new.
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                enabled: false,
                ..WakeSettings::default()
            },
        )
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.worker_errors("w1").await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn awaiting_input_and_failed_tool_calls_wake_the_parent() {
    use mast_engine::sessions::WorkerSignal;
    let rig = Rig::new().await;
    rig.wakes
        .ingest_worker_signal(WorkerSignal::Input {
            chat_id: "w1".into(),
            input: mast_proto::AwaitingInput {
                request_id: "request-1".into(),
                questions: vec![mast_proto::UserInputQuestion {
                    id: "q1".into(),
                    header: "Approval".into(),
                    question: "Run cargo clean?".into(),
                    options: vec![],
                    multi_select: false,
                }],
            },
        })
        .await;
    rig.wakes
        .ingest_worker_signal(WorkerSignal::ToolFailures {
            chat_id: "w2".into(),
            failures: mast_proto::ToolFailures {
                count: 3,
                tools: vec!["bind_chat_worktree".into()],
            },
        })
        .await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 2);
    assert_eq!(held[0].kind, WakeEventKind::AwaitingInput);
    assert!(held[0].summary.contains("Run cargo clean?"));
    assert_eq!(held[1].kind, WakeEventKind::ToolCallsFailed);
    assert!(
        held[1]
            .summary
            .contains("3 failed Mast tool calls (bind_chat_worktree)")
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_persist_across_service_restarts() {
    let rig = Rig::new().await;
    let custom = WakeSettings {
        prs: true,
        errors: false,
        ..WakeSettings::default()
    };
    rig.wakes.set_settings(ORCH, custom).await;
    let again = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 0),
    );
    assert_eq!(again.settings(ORCH), custom);
    assert_eq!(again.settings("someone-else"), WakeSettings::default());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_defaults_and_orchestrator_overrides_persist() {
    let rig = Rig::new().await;
    assert!(!rig.wakes.settings(ORCH).auto_continue);
    std::fs::write(
        &rig.store,
        r#"{"autoContinue":true,"orchestrators":{"orch":{"autoContinue":false}}}"#,
    )
    .unwrap();
    let reload = || {
        OrchestratorWakes::new(
            rig.core.sessions.clone(),
            rig.core.doc_host.clone(),
            rig.core.workspace.clone(),
            Some(rig.store.clone()),
            Arc::new(|| 0),
        )
    };
    let wakes = reload();
    assert!(!wakes.settings(ORCH).auto_continue);
    assert!(wakes.settings("another").auto_continue);
    wakes.set_settings(ORCH, wakes.settings(ORCH)).await;
    let again = reload();
    assert!(!again.status(ORCH).settings.auto_continue);
    assert!(again.status("another").settings.auto_continue);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_continue_loads_legacy_wake_settings() {
    let rig = Rig::new().await;
    std::fs::write(&rig.store, r#"{"orch":{"errors":false,"prs":true}}"#).unwrap();
    let wakes = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 0),
    );
    let settings = wakes.settings(ORCH);
    assert!(!settings.auto_continue);
    assert!(!settings.errors);
    assert!(settings.prs);
    rig.core.shutdown().await;
}

fn snapshot(requests: serde_json::Value) -> WorkOutlineSnapshot {
    serde_json::from_value(serde_json::json!({
        "spaceId": "s1", "available": true, "synced": true,
        "userActionRequests": requests,
    }))
    .unwrap()
}

fn request(id: &str, chat: &str, text: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "spaceId": "s1", "workItemId": "item1", "chatId": chat,
        "text": text, "dedupeKey": id, "createdAt": 1, "state": state,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_actions_raised_wake_once_and_dedupe() {
    let rig = Rig::new().await;
    // The first snapshot only seeds what already exists.
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r0", "w1", "old", "open"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());

    let raised = snapshot(serde_json::json!([
        request("r0", "w1", "old", "open"),
        request("r1", "w1", "Ready to merge: parser-fix", "open"),
        request("r2", "stranger", "not mine", "open"),
    ]));
    rig.wakes.ingest_snapshot(&raised).await;
    rig.wakes.ingest_snapshot(&raised).await; // a re-read of the same state
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(
        held.len(),
        1,
        "one event; strangers and repeats are dropped"
    );
    assert!(held[0].summary.contains("Ready to merge: parser-fix"));
    assert!(held[0].summary.contains("request r1"));

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "the Required Action wake",
    )
    .await;
    assert!(rig.wake_prompts()[0].contains("Ready to merge: parser-fix"));

    // Re-raised after delivery: the same action is not news again.
    let resolved = snapshot(serde_json::json!([request(
        "r1",
        "w1",
        "Ready to merge: parser-fix",
        "resolved"
    )]));
    rig.wakes.ingest_snapshot(&resolved).await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].kind, WakeEventKind::ActionResolved);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolution_wakes_only_other_actors() {
    let rig = Rig::new().await;
    let mut requests = vec![
        request("r1", "w1", "One", "open"),
        request("r2", "w1", "Two", "open"),
        request("r3", "w1", "Three", "open"),
    ];
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!(requests)))
        .await;
    for (request, actor) in requests.iter_mut().zip([
        serde_json::json!({"kind":"agent", "chatId":ORCH}),
        serde_json::json!({"kind":"agent", "chatId":"w2"}),
        serde_json::json!({"kind":"user", "chatId":ORCH}),
    ]) {
        request["state"] = "resolved".into();
        request["resolution"] = serde_json::json!({"by":actor, "at":1});
    }
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!(requests)))
        .await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 2);
    assert!(held.iter().any(|event| event.summary.contains("Two")));
    assert!(held.iter().any(|event| event.summary.contains("Three")));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raised_and_resolved_inside_one_window_say_nothing() {
    let rig = Rig::new().await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([])))
        .await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "open"
        )])))
        .await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "resolved"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_batch_with_other_events_and_say_what_the_worker_said() {
    let rig = Rig::new().await;
    let sent = rig
        .wakes
        .report_to_parent("w1", "Done: fixed the bug\nPR is up.")
        .await
        .unwrap();
    assert_eq!(sent.parent_chat_id, ORCH);
    assert_eq!(sent.delivery, "queuedForParent");
    assert_eq!(sent.held_reason, Some(mast_proto::WakeHoldReason::Window));
    assert!(!sent.truncated);
    rig.worker_errors("w2").await;
    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the wake message")
        .await;
    let prompts = rig.wake_prompts();
    assert_eq!(prompts.len(), 1, "ONE message for the report and the error");
    assert!(prompts[0].contains("worker report: Done: fixed the bug"));
    assert!(
        prompts[0].contains("| PR is up."),
        "full text follows the line"
    );
    assert!(prompts[0].contains("Title w2"));
    assert!(rig.wakes.status(ORCH).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_evaluations_queue_one_report() {
    let rig = Rig::new().await;
    rig.wakes.report_to_parent("w1", "finished").await.unwrap();
    rig.advance(WAKE_WINDOW_MS);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let wakes = rig.wakes.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            wakes.evaluate(ORCH, false).await;
        }));
    }
    barrier.wait().await;
    for task in tasks {
        task.await.unwrap();
    }
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "report delivery")
        .await;
    rig.settle().await;
    assert_eq!(rig.wake_prompts().len(), 1);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_follow_the_user_first_holds() {
    let rig = Rig::new().await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.advance(10 * USER_QUIET_MS);
    let sent = rig.wakes.report_to_parent("w1", "all done").await.unwrap();
    assert_eq!(sent.held_reason, Some(mast_proto::WakeHoldReason::Draft));
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty(), "held while the user drafts");
    rig.wakes.note_user_activity(ORCH, Some(false)).await;
    rig.advance(2 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "delivery after the draft",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_reports_can_be_turned_off() {
    let rig = Rig::new().await;
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                worker_reports: false,
                ..WakeSettings::default()
            },
        )
        .await;
    let sent = rig.wakes.report_to_parent("w1", "all done").await.unwrap();
    assert_eq!(sent.delivery, "parentReportsOff");
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_reports_are_cut_and_marked_and_orphans_cannot_report() {
    let rig = Rig::new().await;
    let long = "x".repeat(10_000);
    let sent = rig.wakes.report_to_parent("w1", &long).await.unwrap();
    assert!(sent.truncated);
    let status = rig.wakes.status(ORCH);
    let report = status.held[0].report.as_deref().unwrap();
    assert!(report.len() < 4_300 && report.contains("[truncated: first 4096 of 10000 bytes"));

    let err = rig
        .wakes
        .report_to_parent("stranger", "hi")
        .await
        .unwrap_err();
    assert!(err.contains("no parent chat"), "{err}");
    assert!(rig.wakes.report_to_parent("w1", "  ").await.is_err());
    rig.core.shutdown().await;
}
