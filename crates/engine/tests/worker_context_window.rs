//! A worker started for a work item on Codex without a context window gets
//! the Space's default (200K) in its `thread/start` params. An explicit
//! choice (even `default`) and a chat that is not a worker keep the model's
//! own window.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{
    CreateChatForWorkItemParams, CreateChatForWorkItemReply, HarnessId, SandboxLevel,
    WorkItemPrompt, derive_id, roles,
};
use mast_rpc::{RpcClient, methods};
use serde_json::{Value, json};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-1";

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let git = |args: &[&str]| {
        let output = mast_harness::git_command()
            .args([
                "-c", "user.name=Zeron Test", "-c", "user.email=test@zeron.invalid",
                "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main",
            ])
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
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
}

async fn rig() -> Rig {
    let tmp = tempfile::Builder::new().prefix("mast-worker-window-").tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    init_repo(&repo);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh");
    let harness = mast_harness::CodexHarness::new()
        .with_executable(fixture)
        .with_account_home(root.join("codex"));
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(harness));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Codex, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(methods::MUTATE, json!({"op":"createSpace","spaceId":SPACE,"deviceId":DEVICE,"path":repo}))
        .await
        .unwrap();
    // The Manager that starts the workers.
    client
        .call(methods::MUTATE, json!({"op":"createChat","chatId":"manager","spaceId":SPACE,"deviceId":DEVICE}))
        .await
        .unwrap();
    Rig { _tmp: tmp, core, client }
}

/// Launch a Codex chat for a fresh work item and return the window its
/// `thread/start` carried (`window:200000`, `window:none`, ...) plus the
/// options stored on the chat.
async fn launch(rig: &Rig, name: &str, parent: Option<&str>, options: Value) -> (String, Value) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({"op":"createWorkItem","spaceId":SPACE,"title":name,"operationId":format!("item-{name}")}),
        )
        .await
        .unwrap();
    let request = CreateChatForWorkItemParams {
        work_item_id: derive_id(&format!("item-{name}"), roles::ITEM),
        initial_prompt: WorkItemPrompt { text: "scenario:context-window".into(), attachments: Vec::new() },
        config: Some(mast_proto::ChatConfig {
            approval_rules: false,
            harness: HarnessId::Codex,
            model: None,
            reasoning: None,
            model_options: options.as_object().unwrap().clone(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
        }),
        execution_override: None,
        worktree_choice: None,
        operation_id: format!("launch-{name}"),
        parent_chat_id: parent.map(str::to_owned),
        target_device_id: None,
        account_id: None,
        standing_role: None,
    };
    let reply: CreateChatForWorkItemReply = serde_json::from_value(
        rig.client
            .call(methods::CREATE_CHAT_FOR_WORK_ITEM, serde_json::to_value(request).unwrap())
            .await
            .unwrap(),
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let reported = loop {
        let text = rig
            .core
            .doc_host
            .open(&reply.chat_id)
            .ok()
            .and_then(|handle| handle.doc().read_entries().ok())
            .into_iter()
            .flatten()
            .flat_map(|entry| entry.parts)
            .find_map(|part| match part {
                mast_doc::MessagePart::Text { text, .. } if text.starts_with("window:") => Some(text),
                _ => None,
            });
        if let Some(text) = text {
            break text;
        }
        assert!(tokio::time::Instant::now() < deadline, "no window report for {name}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let stored = rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().config.unwrap();
    (reported, Value::Object(stored.model_options))
}

#[tokio::test]
async fn codex_worker_started_without_a_window_reports_200k_in_thread_start() {
    let rig = rig().await;

    let (reported, stored) = launch(&rig, "worker", Some("manager"), json!({})).await;
    assert_eq!(reported, "window:200000");
    assert_eq!(stored, json!({"contextWindow": "200000"}));

    // An explicit choice wins, including the model's own window.
    let (reported, stored) =
        launch(&rig, "explicit", Some("manager"), json!({"contextWindow": "default"})).await;
    assert_eq!(reported, "window:none");
    assert_eq!(stored, json!({"contextWindow": "default"}));

    // A chat the user starts is not a worker.
    let (reported, stored) = launch(&rig, "human", None, json!({})).await;
    assert_eq!(reported, "window:none");
    assert_eq!(stored, json!({}));

    rig.core.shutdown().await;
}

async fn set_window(rig: &Rig, key: &str, tokens: Option<u64>) -> Result<Value, mast_rpc::RpcError> {
    let mut params = json!({"op":"setWorkerContextWindow","spaceId":SPACE,"operationId":key});
    if let Some(tokens) = tokens {
        params["tokens"] = json!(tokens);
    }
    rig.client.call(methods::WORK_OUTLINE_MUTATE, params).await
}

#[tokio::test]
async fn the_space_setting_changes_the_window_of_the_next_worker_only() {
    let rig = rig().await;

    let (reported, _) = launch(&rig, "before", Some("manager"), json!({})).await;
    assert_eq!(reported, "window:200000");

    // No cap: the next worker carries no window.
    set_window(&rig, "window-none", Some(0)).await.unwrap();
    let (reported, stored) = launch(&rig, "uncapped", Some("manager"), json!({})).await;
    assert_eq!(reported, "window:none");
    assert_eq!(stored, json!({}));

    // A size.
    set_window(&rig, "window-128k", Some(128_000)).await.unwrap();
    let (reported, stored) = launch(&rig, "sized", Some("manager"), json!({})).await;
    // The fixture only names 200K; any other window reports as "other".
    assert_eq!(reported, "window:other");
    assert_eq!(stored, json!({"contextWindow": "128000"}));

    // Back to Default.
    set_window(&rig, "window-default", None).await.unwrap();
    let (reported, stored) = launch(&rig, "again", Some("manager"), json!({})).await;
    assert_eq!(reported, "window:200000");
    assert_eq!(stored, json!({"contextWindow": "200000"}));

    // A size outside the offered list is refused and changes nothing.
    assert!(set_window(&rig, "window-odd", Some(150_000)).await.is_err());
    let (reported, _) = launch(&rig, "after-refusal", Some("manager"), json!({})).await;
    assert_eq!(reported, "window:200000");

    rig.core.shutdown().await;
}
