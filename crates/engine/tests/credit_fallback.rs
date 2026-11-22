//! Credit fallback: a worker that hits a usage or credits limit keeps going.
//! A per-model Claude limit steps the chat down one model and re-runs the same
//! turn; an account-wide limit hands the work to a successor on another
//! harness; Cursor's transient `rate_limit` does nothing. Each is exactly once
//! across a lost reply, a manual Retry and a restart.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, ChatConfig, CreateChatForWorkItemParams, CreateChatForWorkItemReply, DoneStatus, HarnessId,
    Model, ReasoningLevel, RunRequest, SandboxLevel, WorkItemPrompt, derive_id, roles,
};
use mast_rpc::{RpcClient, methods};
use serde_json::json;

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-1";
const OPUS: &str = "claude-opus-5-5";
const SONNET: &str = "claude-sonnet-5-5";
const HAIKU: &str = "claude-haiku-4-5-20251001";

type Runs = Arc<Mutex<Vec<(HarnessId, Option<String>, String)>>>;

/// Which limit (if any) a run hits: decided from the harness and model.
type Limits = Arc<dyn Fn(HarnessId, Option<&str>) -> Option<&'static str> + Send + Sync>;

/// A harness that records every run and fails it with a scripted limit.
struct Scripted {
    id: HarnessId,
    runs: Runs,
    limits: Limits,
}

#[async_trait]
impl Harness for Scripted {
    fn id(&self) -> HarnessId { self.id }
    fn display_name(&self) -> &str { "Scripted" }
    fn reasoning_levels(&self) -> &[ReasoningLevel] { &[ReasoningLevel::Medium] }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> { Ok(Vec::new()) }
    async fn run(
        &self, request: RunRequest, _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.lock().unwrap().push((self.id, request.model.clone(), request.prompt.clone()));
        let mut events = Vec::new();
        let limit = (self.limits)(self.id, request.model.as_deref());
        if let Some(code) = limit {
            events.push(AgentEvent::Error { message: format!("limit {code}"), code: Some(code.into()), resets_at: None });
        } else {
            events.push(AgentEvent::TextDelta { text: "ok".into() });
        }
        events.push(AgentEvent::Done {
            status: if limit.is_some() { DoneStatus::Errored } else { DoneStatus::Completed },
            result: None,
            error: None,
            session_id: None,
        });
        Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let git = |args: &[&str]| {
        let output = mast_harness::git_command()
            .args(["-c", "user.name=Zeron Test", "-c", "user.email=test@zeron.invalid",
                "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(path.join("README"), "README\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "README"]);
}

struct Rig {
    core: EngineCore,
    client: RpcClient,
    runs: Runs,
}

/// One host over `root`'s data directory (call again for a restart).
async fn host(root: &Path, harnesses: &[HarnessId], limits: Limits, first_boot: bool) -> Rig {
    let repo = root.join("repo");
    let data = root.join("data");
    let runs: Runs = Arc::default();
    let registry = HarnessRegistry::new();
    for id in harnesses {
        registry.register(Arc::new(Scripted { id: *id, runs: runs.clone(), limits: limits.clone() }));
    }
    if first_boot {
        init_repo(&repo);
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("device-id"), DEVICE).unwrap();
    }
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::ClaudeCode, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    if first_boot {
        client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":SPACE,"deviceId":DEVICE,"path":repo})).await.unwrap();
        client.call(methods::MUTATE, json!({"op":"createChat","chatId":"manager","spaceId":SPACE,"deviceId":DEVICE})).await.unwrap();
        client.call(methods::WORK_OUTLINE_MUTATE,
            json!({"op":"createWorkItem","spaceId":SPACE,"title":"w","operationId":"item-w"})).await.unwrap();
    }
    Rig { core, client, runs }
}

fn limits(rule: impl Fn(HarnessId, Option<&str>) -> Option<&'static str> + Send + Sync + 'static) -> Limits {
    Arc::new(rule)
}

/// Start a worker for the work item on `harness` / `model`; returns its chat id.
async fn worker(rig: &Rig, harness: HarnessId, model: &str) -> String {
    let request = CreateChatForWorkItemParams {
        work_item_id: derive_id("item-w", roles::ITEM),
        initial_prompt: WorkItemPrompt { text: "do the task".into(), attachments: Vec::new() },
        config: Some(ChatConfig {
            approval_rules: false,
            harness,
            model: Some(model.into()),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        }),
        execution_override: None,
        worktree_choice: None,
        operation_id: "launch-w".into(),
        parent_chat_id: Some("manager".into()),
        target_device_id: None,
        account_id: None,
        standing_role: None,
    };
    let reply: CreateChatForWorkItemReply = serde_json::from_value(
        rig.client.call(methods::CREATE_CHAT_FOR_WORK_ITEM, serde_json::to_value(request).unwrap()).await.unwrap(),
    ).unwrap();
    reply.chat_id
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ready() { return; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Let anything that is going to happen happen, so "exactly once" is observed.
async fn settle() { tokio::time::sleep(Duration::from_millis(2500)).await; }

fn worker_runs(rig: &Rig) -> Vec<Option<String>> {
    rig.runs.lock().unwrap().iter()
        .filter(|(_, _, prompt)| !prompt.contains("Credit fallback:") && !prompt.contains("Worker updates"))
        .map(|(_, model, _)| model.clone())
        .collect()
}

/// How many error entries of `chat` had a line sent to the Manager.
fn reports(rig: &Rig, chat: &str) -> usize {
    let entries = rig.core.doc_host.open(chat).unwrap().doc().read_entries().unwrap();
    entries.iter()
        .filter(|entry| entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Error { .. })))
        .filter(|entry| rig.core.workspace.ledger_holds(
            &format!("work-outline:credit-fallback:{chat}:{}:reported", entry.id)))
        .count()
}

fn stored_model(rig: &Rig, chat: &str) -> Option<String> {
    rig.core.workspace.chat(chat).unwrap().unwrap().config.unwrap().model
}

fn ledger_entry(rig: &Rig, chat: &str) -> Option<String> {
    rig.core.workspace.ledger_find_prefix(&format!("work-outline:credit-fallback:{chat}:")).unwrap()
}

fn entry_of(key: &str) -> String {
    key.split("credit-fallback:").nth(1).unwrap().split(':').nth(1).unwrap().to_owned()
}

fn hold(rig: &Rig, event: &'static str) {
    rig.core.sessions.agent_tokens().manager_status()
        .set_test_hook(Some(Arc::new(move |name, _| name == event)));
}

fn release(rig: &Rig) {
    rig.core.sessions.agent_tokens().manager_status().set_test_hook(None);
}

fn children_of_manager(rig: &Rig) -> Vec<String> {
    rig.core.workspace.read_chats().unwrap().into_iter()
        .filter(|chat| chat.parent_chat_id.as_deref() == Some("manager"))
        .map(|chat| chat.id)
        .collect()
}

#[tokio::test]
async fn a_per_model_limit_steps_down_in_place_and_reruns_the_same_turn_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rig = host(&root, &[HarnessId::ClaudeCode], limits(|_, model| {
        matches!(model, Some(m) if m.contains("opus") || m.contains("sonnet")).then_some("usage_weekly_model")
    }), true).await;
    let chat = worker(&rig, HarnessId::ClaudeCode, OPUS).await;

    // Opus fails, Sonnet fails too, Haiku completes: one step per limit.
    until("the turn to finish on Haiku", || worker_runs(&rig).len() >= 3 && stored_model(&rig, &chat).as_deref() == Some(HAIKU)).await;
    settle().await;
    assert_eq!(worker_runs(&rig), vec![Some(OPUS.into()), Some(SONNET.into()), Some(HAIKU.into())]);
    assert_eq!(reports(&rig, &chat), 2, "one line per switch");
    // The user's message is not duplicated by the re-runs.
    let entries = rig.core.doc_host.open(&chat).unwrap().doc().read_entries().unwrap();
    assert_eq!(entries.iter().filter(|e| e.role == mast_doc::MessageRole::User).count(), 1);
}

#[tokio::test]
async fn a_manual_retry_before_the_requeue_does_not_switch_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rig = host(&root, &[HarnessId::ClaudeCode], limits(|_, model| {
        model.is_some_and(|m| m.contains("opus")).then_some("usage_weekly_model")
    }), true).await;
    hold(&rig, "credit_fallback_requeue");
    let chat = worker(&rig, HarnessId::ClaudeCode, OPUS).await;
    until("the model switch", || stored_model(&rig, &chat).as_deref() == Some(SONNET)).await;
    let entry = entry_of(&ledger_entry(&rig, &chat).unwrap());

    // The user clicks Retry before the automatic requeue ran: it uses the new model.
    assert!(rig.core.sessions.retry_errored_turn(&chat, &entry).await.unwrap());
    until("the manual retry", || worker_runs(&rig).len() >= 2).await;
    release(&rig);
    settle().await;

    assert_eq!(worker_runs(&rig), vec![Some(OPUS.into()), Some(SONNET.into())], "no duplicate turn");
    assert_eq!(stored_model(&rig, &chat).as_deref(), Some(SONNET), "no second step down");
    // A retry removes the error entry from the transcript; the record keeps it.
    assert!(rig.core.workspace.ledger_holds(&format!("work-outline:credit-fallback:{chat}:{entry}:reported")));
}

#[tokio::test]
async fn a_restart_between_the_switch_and_the_requeue_requeues_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rule = || limits(|_, model| model.is_some_and(|m| m.contains("opus")).then_some("usage_weekly_model"));
    let first = host(&root, &[HarnessId::ClaudeCode], rule(), true).await;
    hold(&first, "credit_fallback_requeue");
    let chat = worker(&first, HarnessId::ClaudeCode, OPUS).await;
    until("the model switch", || stored_model(&first, &chat).as_deref() == Some(SONNET)).await;
    settle().await;
    assert_eq!(worker_runs(&first), vec![Some(OPUS.into())], "held before the requeue");
    first.core.shutdown().await;
    drop(first);

    let second = host(&root, &[HarnessId::ClaudeCode], rule(), false).await;
    until("the requeue after the restart", || !worker_runs(&second).is_empty()).await;
    settle().await;
    assert_eq!(worker_runs(&second), vec![Some(SONNET.into())]);
    assert_eq!(stored_model(&second, &chat).as_deref(), Some(SONNET));
    assert_eq!(reports(&second, &chat), 1);
}

#[tokio::test]
async fn a_codex_credits_error_yields_one_sonnet_successor_through_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rule = || limits(|harness, _| (harness == HarnessId::Codex).then_some("usage_limit"));
    let all = [HarnessId::ClaudeCode, HarnessId::Codex];
    let first = host(&root, &all, rule(), true).await;
    hold(&first, "credit_fallback_successor");
    let stalled = worker(&first, HarnessId::Codex, "gpt-5").await;
    until("the decision", || ledger_entry(&first, &stalled).is_some()).await;
    settle().await;
    assert_eq!(children_of_manager(&first), vec![stalled.clone()], "held before the successor exists");
    first.core.shutdown().await;
    drop(first);

    let second = host(&root, &all, rule(), false).await;
    until("the successor", || children_of_manager(&second).len() == 2).await;
    settle().await;
    let successor = children_of_manager(&second).into_iter().find(|id| *id != stalled).unwrap();
    let config = second.core.workspace.chat_config(&successor).unwrap();
    assert_eq!((config.harness, config.model.as_deref()), (HarnessId::ClaudeCode, Some(SONNET)));
    // It runs the recovery prompt that names the stalled chat.
    let prompts: Vec<String> = second.runs.lock().unwrap().iter().map(|(_, _, p)| p.clone()).collect();
    assert_eq!(prompts.iter().filter(|p| p.contains(&format!("Chat {stalled} was working"))).count(), 1);
    // The stalled chat is kept, not archived.
    assert!(!second.core.workspace.chat(&stalled).unwrap().unwrap().archived);
    assert_eq!(reports(&second, &stalled), 1);

    // A manual Retry of the stalled chat does not start a second successor.
    let entry = entry_of(&ledger_entry(&second, &stalled).unwrap());
    let _ = second.core.sessions.retry_errored_turn(&stalled, &entry).await;
    settle().await;
    assert_eq!(children_of_manager(&second).len(), 2);
}

#[tokio::test]
async fn a_claude_5h_limit_on_sonnet_goes_to_another_harness_or_reports_when_none_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rule = || limits(|harness, _| (harness == HarnessId::ClaudeCode).then_some("usage_5h"));
    let rig = host(&root, &[HarnessId::ClaudeCode, HarnessId::Codex], rule(), true).await;
    let stalled = worker(&rig, HarnessId::ClaudeCode, SONNET).await;
    until("the successor", || children_of_manager(&rig).len() == 2).await;
    settle().await;
    let successor = children_of_manager(&rig).into_iter().find(|id| *id != stalled).unwrap();
    assert_eq!(rig.core.workspace.chat_config(&successor).unwrap().harness, HarnessId::Codex);
    assert_eq!(stored_model(&rig, &stalled).as_deref(), Some(SONNET), "an account limit never switches in place");
    assert_eq!(reports(&rig, &stalled), 1);

    let alone_dir = tempfile::tempdir().unwrap();
    let alone_root = std::fs::canonicalize(alone_dir.path()).unwrap();
    let alone = host(&alone_root, &[HarnessId::ClaudeCode], rule(), true).await;
    let stalled = worker(&alone, HarnessId::ClaudeCode, SONNET).await;
    until("the report", || reports(&alone, &stalled) == 1).await;
    settle().await;
    assert_eq!(children_of_manager(&alone), vec![stalled.clone()], "no harness left: report only");
    assert_eq!(reports(&alone, &stalled), 1);
}

#[tokio::test]
async fn a_cursor_rate_limit_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rig = host(&root, &[HarnessId::ClaudeCode, HarnessId::Cursor],
        limits(|harness, _| (harness == HarnessId::Cursor).then_some("rate_limit")), true).await;
    let chat = worker(&rig, HarnessId::Cursor, "auto").await;
    until("the run", || !worker_runs(&rig).is_empty()).await;
    settle().await;
    assert_eq!(worker_runs(&rig).len(), 1);
    assert_eq!(children_of_manager(&rig), vec![chat.clone()]);
    assert_eq!(stored_model(&rig, &chat).as_deref(), Some("auto"));
    assert_eq!(reports(&rig, &chat), 0);
}

#[tokio::test]
async fn a_chat_that_was_already_errored_before_the_upgrade_is_not_acted_on() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let all = [HarnessId::ClaudeCode, HarnessId::Codex];
    let rule = || limits(|harness, _| (harness == HarnessId::Codex).then_some("usage_limit"));
    // The first host never decides, as before the feature existed.
    let first = host(&root, &all, rule(), true).await;
    hold(&first, "credit_fallback_decide");
    let stalled = worker(&first, HarnessId::Codex, "gpt-5").await;
    until("the errored run", || !worker_runs(&first).is_empty()).await;
    settle().await;
    first.core.shutdown().await;
    drop(first);

    let second = host(&root, &all, rule(), false).await;
    settle().await;
    assert_eq!(children_of_manager(&second), vec![stalled.clone()]);
    assert!(ledger_entry(&second, &stalled).is_none());
    assert_eq!(reports(&second, &stalled), 0);
}

#[tokio::test]
async fn a_model_the_user_picks_after_the_switch_is_kept_for_the_requeue() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let rig = host(&root, &[HarnessId::ClaudeCode], limits(|_, model| {
        model.is_some_and(|m| m.contains("opus")).then_some("usage_weekly_model")
    }), true).await;
    hold(&rig, "credit_fallback_requeue");
    let chat = worker(&rig, HarnessId::ClaudeCode, OPUS).await;
    until("the model switch", || stored_model(&rig, &chat).as_deref() == Some(SONNET)).await;
    let mut config = rig.core.workspace.chat_config(&chat).unwrap();
    config.model = Some(HAIKU.into());
    rig.core.workspace.set_chat_config(&chat, &config).unwrap();
    release(&rig);
    until("the requeue", || worker_runs(&rig).len() >= 2).await;
    settle().await;
    assert_eq!(worker_runs(&rig), vec![Some(OPUS.into()), Some(HAIKU.into())]);
    assert_eq!(stored_model(&rig, &chat).as_deref(), Some(HAIKU));
}

#[tokio::test]
async fn a_new_error_after_a_hand_retry_is_acted_on_even_for_a_chat_errored_at_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let all = [HarnessId::ClaudeCode, HarnessId::Codex];
    let rule = || limits(|harness, _| (harness == HarnessId::Codex).then_some("usage_limit"));
    let first = host(&root, &all, rule(), true).await;
    hold(&first, "credit_fallback_decide");
    let stalled = worker(&first, HarnessId::Codex, "gpt-5").await;
    until("the errored run", || !worker_runs(&first).is_empty()).await;
    settle().await;
    first.core.shutdown().await;
    drop(first);

    let second = host(&root, &all, rule(), false).await;
    settle().await;
    assert_eq!(children_of_manager(&second), vec![stalled.clone()], "the pre-upgrade error is left alone");
    // The user retries by hand and it fails again: that is a new error.
    let (entry, _) = second.core.sessions.errored_limit_for_test(&stalled).unwrap();
    assert!(second.core.sessions.retry_errored_turn(&stalled, &entry).await.unwrap());
    until("the successor", || children_of_manager(&second).len() == 2).await;
}
