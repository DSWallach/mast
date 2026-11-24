//! A reply to a finished scheduled run opens a follow-up chat seeded with the
//! run's final result instead of continuing on the run's full context.

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

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

const RUN: &str = "chat-run";
const FOLLOW: &str = "chat-follow";

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
            let answers = (controls.request_input)(vec![UserInputQuestion { permission: None,
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
            chat_id: None,
            space_id: None,
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
                "op": "createChat", "chatId": RUN, "spaceId": "space",
                "config": { "harness": "mock", "sandbox": "workspace-write", "autoApprove": false },
            }),
        )
        .await
        .expect("create run chat");
    core.workspace
        .set_chat_scheduled_task_info(
            RUN,
            &mast_proto::ScheduledChat {
                task_id: "task".into(),
                task_name: "Daily summary".into(),
                run_id: "run-1".into(),
                ..Default::default()
            },
        )
        .expect("stamp the run");
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
        self.idle(chat).await;
    }

    /// Wait for the chat's session row to report Idle: what a finished run looks like.
    async fn idle(&self, chat: &str) {
        wait_for(
            || {
                self.core
                    .workspace
                    .session(chat)
                    .is_some_and(|session| session.status == mast_proto::SessionStatus::Idle)
            },
            "the session row to read Idle",
        )
        .await;
    }

    /// Queue a turn and wait for it to reach the harness (not to finish).
    async fn start(&self, chat: &str, prompt: &str) {
        let before = self.harness.requests.lock().unwrap().len();
        let request = RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: false,
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

    async fn open_followup(&self, chat: &str, run: &str) -> Result<(), mast_rpc::RpcError> {
        self.client
            .call(
                mast_rpc::methods::MUTATE,
                serde_json::json!({ "op": "createChat", "chatId": chat, "followupOf": run }),
            )
            .await
            .map(drop)
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
async fn a_follow_up_starts_from_the_result_and_is_not_the_scheduled_task() {
    let rig = rig(false).await;
    rig.turn(RUN, "write the daily summary").await;
    assert_eq!(
        rig.requests()[0].scheduled_task.as_deref(),
        Some("Daily summary"),
        "the run itself is the scheduled task"
    );

    // A retried create with the same chat id leaves one follow-up chat.
    rig.open_followup(FOLLOW, RUN).await.expect("create follow-up");
    rig.open_followup(FOLLOW, RUN).await.expect("retried create");
    let followups: Vec<_> = rig
        .core
        .workspace
        .read_chats()
        .unwrap()
        .into_iter()
        .filter(|chat| chat.followup_of.as_deref() == Some(RUN))
        .collect();
    assert_eq!(followups.len(), 1);
    let follow = &followups[0];
    assert_eq!(follow.id, FOLLOW);
    assert!(follow.scheduled_task_info.is_none() && follow.scheduled_task.is_none());
    assert_eq!(follow.space_id.as_deref(), Some("space"));
    assert_eq!(follow.cwd.as_deref(), Some(rig.cwd.as_str()));
    assert!(follow.parent_chat_id.is_none());

    rig.turn(FOLLOW, "compare with last week").await;
    let request = &rig.requests()[1];
    assert!(request.resume.is_none(), "no resume of the run's session");
    assert!(request.scheduled_task.is_none(), "not stamped as the task");
    assert!(request.scheduled_notion_writes.is_none());
    assert!(request.prompt.contains("Daily summary"), "{}", request.prompt);
    assert!(request.prompt.contains(RUN), "links back to the run");
    assert!(request.prompt.contains("answer 1"), "carries the final result");
    assert!(request.prompt.ends_with("compare with last week"));
    assert!(
        !request.prompt.contains("write the daily summary"),
        "not the run's own context"
    );
    assert_eq!(rig.user_texts(FOLLOW), ["compare with last week"]);

    // Only the first turn is seeded.
    rig.turn(FOLLOW, "and the week before").await;
    assert_eq!(rig.requests()[2].prompt, "and the week before");
    assert!(rig.requests()[2].scheduled_task.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_in_place_is_a_per_run_setting_and_only_for_scheduled_runs() {
    let rig = rig(false).await;
    rig.turn(RUN, "write the daily summary").await;
    let set = |chat: &str, in_place: bool| {
        rig.client.call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "setScheduledFollowupsInPlace", "chatId": chat, "inPlace": in_place }),
        )
    };
    set(RUN, true).await.expect("set in place");
    let run = rig.core.workspace.chat(RUN).unwrap().unwrap();
    assert!(run.scheduled_task_info.unwrap().followups_in_place);
    set(RUN, false).await.expect("clear");
    let run = rig.core.workspace.chat(RUN).unwrap().unwrap();
    assert!(!run.scheduled_task_info.unwrap().followups_in_place);

    // Set to continue in place, the host refuses to open a follow-up.
    set(RUN, true).await.unwrap();
    assert!(rig.open_followup("chat-refused", RUN).await.is_err());
    assert!(rig.core.workspace.chat("chat-refused").unwrap().is_none());
    set(RUN, false).await.unwrap();

    // A follow-up chat is not a scheduled run: neither setting nor chaining.
    rig.open_followup(FOLLOW, RUN).await.unwrap();
    assert!(set(FOLLOW, true).await.is_err());
    assert!(rig.open_followup("chat-nested", FOLLOW).await.is_err());
    assert!(rig.core.workspace.chat("chat-nested").unwrap().is_none());

    // An id already used by an unrelated chat is not silently reused.
    rig.client
        .call(
            mast_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": "chat-other", "spaceId": "space" }),
        )
        .await
        .unwrap();
    assert!(rig.open_followup("chat-other", RUN).await.is_err());
}

#[test]
fn a_run_without_a_final_message_still_seeds_the_link() {
    let text = mast_engine::sessions::followup_seed_text("Daily summary", "run-x", None);
    assert!(text.contains("run-x") && text.contains("no final message"));
    let long = "é".repeat(20_000);
    let capped = mast_engine::sessions::followup_seed_text("t", "r", Some(&long));
    assert!(capped.contains("[result truncated]") && capped.len() < 17_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_has_not_finished_keeps_replies_in_place() {
    let rig = rig(false).await;
    // No session row at all: the run has not reported anything.
    assert!(rig.core.workspace.session(RUN).is_none());
    assert!(rig.open_followup("chat-early", RUN).await.is_err(), "no session row is not finished");
    assert!(rig.core.workspace.chat("chat-early").unwrap().is_none());

    // Working: the host refuses whatever the sender saw.
    rig.start(RUN, "[hold] write the daily summary").await;
    wait_for(
        || {
            rig.core
                .workspace
                .session(RUN)
                .is_some_and(|session| session.status == mast_proto::SessionStatus::Working)
        },
        "the run to read Working",
    )
    .await;
    assert!(rig.open_followup("chat-busy", RUN).await.is_err(), "a working run is not finished");
    assert!(rig.core.workspace.chat("chat-busy").unwrap().is_none());

    // Once it finishes the same request opens the follow-up.
    rig.harness.release.notify_one();
    rig.settle(RUN).await;
    rig.idle(RUN).await;
    rig.open_followup("chat-busy", RUN).await.expect("a finished run opens a follow-up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_that_opens_with_a_slash_command_leaves_the_seed_for_the_next_message() {
    let rig = rig(false).await;
    rig.turn(RUN, "write the daily summary").await;
    rig.open_followup(FOLLOW, RUN).await.expect("create follow-up");

    rig.turn(FOLLOW, "/compact").await;
    assert_eq!(rig.requests()[1].prompt, "/compact", "a command stays first and unchanged");

    rig.turn(FOLLOW, "compare with last week").await;
    let request = &rig.requests()[2];
    assert!(request.prompt.contains(RUN), "the next message carries the seed: {}", request.prompt);
    assert!(request.prompt.contains("answer 1"), "with the run's final result");
    assert!(request.prompt.ends_with("compare with last week"));

    rig.turn(FOLLOW, "and the week before").await;
    assert_eq!(rig.requests()[3].prompt, "and the week before", "seeded once");
}
