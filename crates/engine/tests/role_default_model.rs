//! A Claude Code chat started for a work item without a model gets its role's
//! default: the Space's worker default for a child chat, its Manager default
//! for a standing-role launch. An explicit model wins, and a replayed launch
//! keeps the model its first run chose.

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
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mast_harness::ClaudeHarness::new().with_executable(fixture)));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::ClaudeCode, None).unwrap();
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

async fn set_model(rig: &Rig, key: &str, role: &str, model: Option<&str>) {
    let mut params = json!({"op":"setRoleDefaultModel","spaceId":SPACE,"role":role,"operationId":key});
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    rig.client.call(methods::WORK_OUTLINE_MUTATE, params).await.unwrap();
}

/// Launch a Claude Code chat for a fresh work item and return its stored model.
async fn launch(rig: &Rig, name: &str, parent: Option<&str>, model: Option<&str>) -> String {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({"op":"createWorkItem","spaceId":SPACE,"title":name,"operationId":format!("item-{name}")}),
        )
        .await
        .unwrap();
    launch_item(rig, name, name, parent, model).await
}

async fn launch_item(rig: &Rig, item: &str, op: &str, parent: Option<&str>, model: Option<&str>) -> String {
    let request = CreateChatForWorkItemParams {
        work_item_id: derive_id(&format!("item-{item}"), roles::ITEM),
        initial_prompt: WorkItemPrompt { text: "go".into(), attachments: Vec::new() },
        config: Some(mast_proto::ChatConfig {
            approval_rules: false,
            harness: HarnessId::ClaudeCode,
            model: model.map(str::to_owned),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        }),
        execution_override: None,
        worktree_choice: None,
        operation_id: format!("launch-{op}"),
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
    rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap().config.unwrap().model.unwrap()
}

#[tokio::test]
async fn a_worker_without_a_model_starts_on_the_worker_default_and_a_replay_keeps_it() {
    let rig = rig().await;

    // A Space with no setting: workers get the built-in worker default.
    assert_eq!(launch(&rig, "w1", Some("manager"), None).await, "claude-sonnet-5-5");
    // A chat the user starts is not a worker.
    assert_eq!(launch(&rig, "human", None, None).await, "claude-opus-5-5");
    // An explicit model wins.
    assert_eq!(launch(&rig, "explicit", Some("manager"), Some("claude-opus-5-5")).await, "claude-opus-5-5");

    // The setting reaches the next worker only; a replay of the first launch
    // after the change keeps the model it started on.
    set_model(&rig, "set-worker", "worker", Some("claude-haiku-4-5-20251001")).await;
    assert_eq!(launch(&rig, "w2", Some("manager"), None).await, "claude-haiku-4-5-20251001");
    assert_eq!(launch_item(&rig, "w1", "w1", Some("manager"), None).await, "claude-sonnet-5-5");

    // Clearing it returns to the built-in.
    set_model(&rig, "clear-worker", "worker", None).await;
    assert_eq!(launch(&rig, "w3", Some("manager"), None).await, "claude-sonnet-5-5");

    rig.core.shutdown().await;
}
