//! The MCP tools against a real offline engine, authenticated by an
//! engine-minted agent token (Work Outline Milestone 4):
//! - `create_chat` from an agent in a Space creates ONE child Chat per
//!   idempotency key (retries return it, with one queued first Turn), and
//!   the child inherits the parent's work item (acceptance 19);
//! - the Work Outline tools reach the engine's agent surface through MCP;
//! - once the token is revoked, every tool stops working.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_mcp::{Mast, Tools};
use mast_proto::{AgentEvent, DoneStatus, Model, ReasoningLevel, RunRequest, SteeringMode};
use mast_proto::{HarnessId, WorkItemChatAssignmentState, derive_id, roles};
use mast_rpc::methods;
use serde_json::{Value, json};

const SPACE: &str = "space-1";
const PARENT: &str = "chat-parent";

/// Stands in for Claude Code (so the MCP tools see an available harness)
/// and completes every Run at once without spawning anything.
struct FakeClaude;

#[async_trait]
impl Harness for FakeClaude {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Fake Claude"
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
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "fake".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "sess-fake".into(),
                assistant_message_id: "a-1".into(),
                chat_id: None,
                space_id: None,
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-fake".into()),
            }),
        ])
        .boxed())
    }
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn review_publish_validates_input_and_example() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FakeClaude));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .expect("engine core assembles");
    let admin = mast_rpc::memory_client(core.rpc_service());
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": project.to_string_lossy() }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": PARENT, "spaceId": SPACE,
                    "deviceId": core.device_id,
                    "config": { "harness": "claude-code", "model": null, "reasoning": null,
                                "sandbox": "workspace-write" } }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Feature",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    let item = derive_id("op-item", roles::ITEM);
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": PARENT, "workItemId": item,
                    "operationId": "op-assign" }),
        )
        .await
        .unwrap();

    let lease = core
        .sessions
        .agent_tokens()
        .mint(PARENT, "run-1", SPACE, &core.device_id);
    let tools = Tools::new(Arc::new(Mast::with_client(
        mast_rpc::memory_client(core.rpc_service()),
        Some(lease.token().to_owned()),
    )));

    // Exercise the published example and audit failures through MCP and the offline engine.
    let git = |args: &[&str]| {
        let output = mast_harness::git_command().current_dir(&project).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    git(&["init"]);
    git(&["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-m", "base"]);
    let sha = git(&["rev-parse", "HEAD"]);
    let description = tools.list().into_iter().find(|tool| tool.name == "review_publish").unwrap().description;
    let example = description.split_once("Example: ").unwrap().1;
    let mut valid: Value = serde_json::from_str(example).unwrap();
    valid["headSha"] = json!(sha);
    valid["review"]["pr"]["headSha"] = json!(sha);
    valid["review"]["pr"]["baseSha"] = json!(sha);
    let mut missing_head = valid.clone();
    missing_head.as_object_mut().unwrap().remove("headSha");
    let mut missing_review = valid.clone();
    let flattened = missing_review.as_object_mut().unwrap().remove("review").unwrap();
    missing_review.as_object_mut().unwrap().extend(flattened.as_object().unwrap().clone());
    let mut summary_array = valid.clone();
    summary_array["review"]["summary"] = json!(["First line", "Second line"]);
    let mut missing_target = valid.clone();
    missing_target.as_object_mut().unwrap().remove("prUrl");
    let mut missing_all = valid.clone();
    missing_all.as_object_mut().unwrap().remove("prUrl");
    missing_all.as_object_mut().unwrap().remove("headSha");
    let mut missing_pr = valid.clone();
    missing_pr["review"].as_object_mut().unwrap().remove("pr");
    let mut nested_target = valid.clone();
    nested_target["review"]["target"] = json!({"kind":"pr"});
    let mut bad_number = valid.clone();
    bad_number["target"] = json!({"kind":"pr", "repo":"a/b", "number":"one", "headSha":sha, "baseSha":sha});
    let mut bad_head = bad_number.clone();
    bad_head["target"]["number"] = json!(1);
    bad_head["target"]["headSha"] = json!(42);
    for (arguments, expected, detail) in [
        (missing_head, "headSha", "expected"), (missing_review, "review", "expected"),
        (summary_array, "review.summary", "expected a string"), (missing_target, "prUrl or target", "expected"),
        (missing_all, "prUrl or target", "expected"), (missing_pr, "review.pr", "missing field"),
        (nested_target, "review.target", "top-level target"),
        (bad_number, "target.number", "expected u64"), (bad_head, "target.headSha", "expected a string"),
    ] {
        let error = tools.call("review_publish", arguments).await.unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains(detail), "{error}");
    }
    tools.call("review_publish", valid.clone()).await.expect("description example validates");
    valid["review"]["questions"] = json!(["Why?", {"question":"How?"}, {"text":"When?"}]);
    tools.call("review_publish", valid).await.expect("question forms validate");

}

#[tokio::test(flavor = "multi_thread")]
async fn agent_created_child_chats_are_idempotent_and_inherit_the_work_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FakeClaude));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .expect("engine core assembles");
    let admin = mast_rpc::memory_client(core.rpc_service());
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": project.to_string_lossy() }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": PARENT, "spaceId": SPACE,
                    "deviceId": core.device_id,
                    "config": { "harness": "claude-code", "model": null, "reasoning": null,
                                "sandbox": "workspace-write" } }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Feature",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    let item = derive_id("op-item", roles::ITEM);
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": PARENT, "workItemId": item,
                    "operationId": "op-assign" }),
        )
        .await
        .unwrap();

    let lease = core
        .sessions
        .agent_tokens()
        .mint(PARENT, "run-1", SPACE, &core.device_id);
    let tools = Tools::new(Arc::new(Mast::with_client(
        mast_rpc::memory_client(core.rpc_service()),
        Some(lease.token().to_owned()),
    )));

    // The Work Outline tools are listed and reach the engine.
    assert!(tools.has("work_outline_read_current"));
    let current = tools
        .call("work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(current["workItem"]["id"], item);



    let linked_id = Box::pin(check_create_linked_item(&core, &tools)).await;
    Box::pin(check_link_work_item(&core, &tools, &linked_id)).await;
    Box::pin(check_item_chat_title(&core, &tools, &linked_id)).await;

    let args = json!({ "project": SPACE, "harness": "claude-code", "title": "Reviewer",
                       "prompt": "review the diff", "idempotency_key": "spawn-reviewer" });
    let first = tools.call("create_chat", args.clone()).await.unwrap();
    let child = first["chatId"].as_str().unwrap().to_owned();
    assert_eq!(first["parentChatId"], PARENT);
    let second = tools.call("create_chat", args.clone()).await.unwrap();
    assert_eq!(second["chatId"], child);
    assert_eq!(second["replayed"], true);
    // The same key with other inputs: a conflict, and still one child.
    let mut changed = args.clone();
    changed["prompt"] = json!("rewrite everything");
    let err = tools.call("create_chat", changed).await.unwrap_err();
    assert!(err.contains("work-outline/operationConflict"), "{err}");
    // A wider sandbox than the parent's (workspace-write) is refused.
    let err = tools
        .call(
            "create_chat",
            json!({ "project": SPACE, "sandbox": "danger-full-access",
                    "idempotency_key": "wide" }),
        )
        .await
        .unwrap_err();
    assert!(err.contains("more permissive"), "{err}");

    // One child Chat, parented to the caller, holding exactly one first
    // Turn (the key-derived command id), whatever the retries.
    let chats = core.workspace.read_chats().unwrap();
    let children: Vec<_> = chats
        .iter()
        .filter(|c| c.parent_chat_id.as_deref() == Some(PARENT))
        .collect();
    assert_eq!(children.len(), 2);
    assert!(children.iter().any(|c| c.id == child));
    let operation = mast_proto::agent_tools::operation_id(PARENT, "spawn-reviewer");
    let command_id = derive_id(&operation, roles::COMMAND);
    let commands = core
        .doc_host
        .open(&child)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(commands[0].id, command_id);

    // Inheritance (acceptance 19): the child joins the parent's work item.
    wait_for(
        || {
            let inputs = core.workspace.work_outline_inputs(SPACE);
            let snapshot = mast_doc::materialize_work_outline(SPACE, &inputs);
            snapshot.assignments.iter().any(|a| {
                a.chat_id == child
                    && a.work_item_id.as_deref() == Some(item.as_str())
                    && a.state == WorkItemChatAssignmentState::Associated
            })
        },
        "child assignment",
    )
    .await;

    // The Run ends: the token dies and every tool with it.
    drop(lease);
    for (name, arguments) in [
        ("list_chats", json!({})),
        ("work_outline_read_current", json!({})),
        (
            "create_chat",
            json!({ "project": SPACE, "idempotency_key": "late" }),
        ),
    ] {
        let err = tools.call(name, arguments).await.unwrap_err();
        assert!(err.contains("unauthenticated"), "{name}: {err}");
    }
    let _: Value = current;
}

async fn check_create_linked_item(core: &EngineCore, tools: &Tools) -> String {
    let linked = tools.call("work_outline_create_child", json!({
        "title": "Implement feature", "linearIssue": "ENG-123", "idempotencyKey": "create-linked"
    })).await.unwrap();
    let linked_id = linked["workItemId"].as_str().unwrap().to_owned();
    let err = tools.call("work_outline_create_child", json!({
        "title": "Implement feature", "linearIssue": "ENG-999", "idempotencyKey": "create-linked"
    })).await.unwrap_err();
    assert!(err.contains("operationConflict"), "{err}");
    let snapshot = mast_doc::materialize_work_outline(SPACE, &core.workspace.work_outline_inputs(SPACE));
    let issue = snapshot.linear_issue_reference(&linked_id).unwrap();
    assert_eq!(issue.external_id, "ENG-123");
    assert_eq!(issue.provider, mast_proto::linear::LINEAR_PROVIDER);
    linked_id
}

async fn check_link_work_item(core: &EngineCore, tools: &Tools, linked_id: &str) {
    tools.call("work_outline_link", json!({"workItemId": linked_id,
        "pullRequest": "acme/repo#42", "idempotencyKey": "link-pr"})).await.unwrap();
    tools.call("work_outline_link", json!({"workItemId": linked_id,
        "linearIssue": "https://linear.app/acme/issue/ENG-456/description", "idempotencyKey": "link-linear"})).await.unwrap();
    let snapshot = mast_doc::materialize_work_outline(SPACE, &core.workspace.work_outline_inputs(SPACE));
    assert!(snapshot.external_references.iter().any(|r| r.work_item_id == linked_id && r.external_id == "acme/repo#42" && r.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER));
    assert!(snapshot.external_references.iter().any(|r| r.work_item_id == linked_id && r.external_id == "ENG-456" && r.provider == mast_proto::linear::LINEAR_PROVIDER));
    let err = tools.call("work_outline_link", json!({"workItemId": "foreign-item", "linearIssue": "ENG-3", "idempotencyKey": "link-foreign"})).await.unwrap_err();
    assert!(err.contains("unauthorized"), "{err}");
    let err = tools.call("work_outline_create_child", json!({"title": "Invalid", "linearIssue": "not an issue", "idempotencyKey": "invalid-linear"})).await.unwrap_err();
    assert!(err.contains("linearIssue"), "{err}");
}

async fn check_item_chat_title(core: &EngineCore, tools: &Tools, linked_id: &str) {
    let launched = tools.call("create_chat", json!({"workItemId": linked_id,
        "prompt": "Implement the feature", "idempotencyKey": "launch-linked"})).await.unwrap();
    let launched_id = launched["chatId"].as_str().unwrap();
    let launched_chat = core.workspace.chat(launched_id).unwrap().unwrap();
    assert_eq!(launched_chat.title.as_deref(), Some("ENG-123 Implement feature"));
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_pack_tools_need_admin_and_apply_and_undo_through_receipts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FakeClaude));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .expect("engine core assembles");
    let admin = mast_rpc::memory_client(core.rpc_service());
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": project.to_string_lossy() }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": PARENT, "spaceId": SPACE,
                    "deviceId": core.device_id,
                    "config": { "harness": "claude-code", "model": null, "reasoning": null,
                                "sandbox": "workspace-write" } }),
        )
        .await
        .unwrap();
    let pack_path = root.join("pack.json");
    std::fs::write(
        &pack_path,
        json!({ "format": "mast.workflow-pack/v1", "mode": "full",
                "scopes": [{ "itemPath": ["Tasks"], "instructions": ["Run the tests"] }],
                "templates": [] })
        .to_string(),
    )
    .unwrap();
    let lease = core
        .sessions
        .agent_tokens()
        .mint(PARENT, "run-1", SPACE, &core.device_id);
    let tools = Tools::new(Arc::new(Mast::with_client(
        mast_rpc::memory_client(core.rpc_service()),
        Some(lease.token().to_owned()),
    )));
    for name in [
        "work_outline_workflow_pack_export",
        "work_outline_workflow_pack_preview_import",
        "work_outline_workflow_pack_apply",
        "work_outline_workflow_pack_undo",
        "work_outline_workflow_pack_receipts",
    ] {
        assert!(tools.has(name), "{name}");
    }
    let preview_args = json!({ "path": pack_path.to_string_lossy() });
    let err = tools
        .call("work_outline_workflow_pack_preview_import", preview_args.clone())
        .await
        .unwrap_err();
    assert!(err.contains("admin"), "{err}");

    // The user grants admin; the engine re-checks the grant at authenticate.
    core.sessions.agent_tokens().admin().grant(PARENT, SPACE, Some(600), false);
    let lease = core
        .sessions
        .agent_tokens()
        .mint(PARENT, "run-2", SPACE, &core.device_id);
    let tools = Tools::new(Arc::new(Mast::with_client(
        mast_rpc::memory_client(core.rpc_service()),
        Some(lease.token().to_owned()),
    )));
    let exported = tools
        .call("work_outline_workflow_pack_export", json!({}))
        .await
        .unwrap();
    assert_eq!(exported["pack"]["format"], "mast.workflow-pack/v1");
    let preview = tools
        .call("work_outline_workflow_pack_preview_import", preview_args)
        .await
        .unwrap();
    let preview_id = preview["previewId"].as_str().unwrap().to_owned();
    let err = tools
        .call("work_outline_workflow_pack_apply", json!({ "previewId": "unknown" }))
        .await
        .unwrap_err();
    assert!(err.contains("preview"), "{err}");
    let receipt = tools
        .call("work_outline_workflow_pack_apply", json!({ "previewId": preview_id }))
        .await
        .unwrap();
    assert_eq!(receipt["summary"]["instructionsAdded"], 1);
    let snapshot = || {
        mast_doc::materialize_work_outline(SPACE, &core.workspace.work_outline_inputs(SPACE))
    };
    assert!(snapshot().instructions.iter().any(|i| !i.archived() && i.text == "Run the tests"));
    let listed = tools
        .call("work_outline_workflow_pack_receipts", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["receipts"][0]["id"], receipt["id"]);
    let undone = tools
        .call("work_outline_workflow_pack_undo", json!({ "receiptId": receipt["id"] }))
        .await
        .unwrap();
    assert!(undone["undoneAt"].is_number());
    assert!(snapshot().instructions.iter().all(|i| i.archived()));
    let err = tools
        .call("work_outline_workflow_pack_undo", json!({ "receiptId": receipt["id"] }))
        .await
        .unwrap_err();
    assert!(err.contains("already"), "{err}");
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
