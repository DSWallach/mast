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
    Box::pin(check_link_work_item(&core, &tools, &item, &linked_id)).await;
    let launched_id = Box::pin(check_item_chat_title(&core, &tools, &linked_id)).await;
    Box::pin(check_chat_reuse(&core, &tools, &linked_id, &launched_id)).await;
    Box::pin(check_concurrent_chat_reuse(&core, &tools, &linked_id, &launched_id)).await;
    Box::pin(check_reuse_without_prompt(&tools, &linked_id, &launched_id)).await;
    Box::pin(check_force_chat(&core, &tools, &linked_id, &launched_id)).await;
    Box::pin(check_unlinked_item_title(&core, &tools)).await;
    Box::pin(check_planning_item_title(&core, &tools)).await;

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
    assert_eq!(children.len(), 5);
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

async fn check_link_work_item(core: &EngineCore, tools: &Tools, root_id: &str, linked_id: &str) {
    tools.call("work_outline_link", json!({"workItemId": linked_id,
        "pullRequest": "acme/repo#42", "idempotencyKey": "link-pr"})).await.unwrap();
    tools.call("work_outline_link", json!({"workItemId": linked_id,
        "linearIssue": "https://linear.app/acme/issue/ENG-456/description", "idempotencyKey": "link-linear"})).await.unwrap();
    tools.call("work_outline_link", json!({"workItemId": root_id,
        "linearProject": "12345678-1234-1234-1234-123456789abc", "idempotencyKey": "link-project-id"})).await.unwrap();
    tools.call("work_outline_link", json!({"workItemId": linked_id,
        "linearProject": "https://linear.app/example/project/harbor-123abc/overview", "idempotencyKey": "link-project-url"})).await.unwrap();
    let snapshot = mast_doc::materialize_work_outline(SPACE, &core.workspace.work_outline_inputs(SPACE));
    assert!(snapshot.external_references.iter().any(|r| r.work_item_id == linked_id && r.external_id == "acme/repo#42" && r.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER));
    assert!(snapshot.external_references.iter().any(|r| r.work_item_id == linked_id && r.external_id == "ENG-456" && r.provider == mast_proto::linear::LINEAR_PROVIDER));
    assert_eq!(snapshot.linear_project_reference(root_id).unwrap().external_id, "12345678-1234-1234-1234-123456789abc");
    assert_eq!(snapshot.linear_project_reference(linked_id).unwrap().external_id, "harbor-123abc");
    let err = tools.call("work_outline_link", json!({"workItemId": "foreign-item", "linearIssue": "ENG-3", "idempotencyKey": "link-foreign"})).await.unwrap_err();
    assert!(err.contains("unauthorized"), "{err}");
    let err = tools.call("work_outline_link", json!({"workItemId": "foreign-item", "linearProject": "project-id", "idempotencyKey": "project-foreign"})).await.unwrap_err();
    assert!(err.contains("unauthorized"), "{err}");
    let err = tools.call("work_outline_link", json!({"workItemId": linked_id, "linearProject": "project-id", "linearIssue":"ENG-1", "idempotencyKey": "project-conflict"})).await.unwrap_err();
    assert!(err.contains("exactly one"), "{err}");
    let err = tools.call("work_outline_link", json!({"workItemId": linked_id, "linearProject": "https://example.com/project/foo", "idempotencyKey": "project-invalid"})).await.unwrap_err();
    assert!(err.contains("linearProject"), "{err}");
    let err = tools.call("work_outline_create_child", json!({"title": "Invalid", "linearIssue": "not an issue", "idempotencyKey": "invalid-linear"})).await.unwrap_err();
    assert!(err.contains("linearIssue"), "{err}");
}

async fn check_item_chat_title(core: &EngineCore, tools: &Tools, linked_id: &str) -> String {
    let launched = tools.call("create_chat", json!({"workItemId": linked_id,
        "prompt": "Implement the feature", "idempotencyKey": "launch-linked"})).await.unwrap();
    let launched_id = launched["chatId"].as_str().unwrap();
    let launched_chat = core.workspace.chat(launched_id).unwrap().unwrap();
    assert_eq!(launched_chat.title.as_deref(), Some("ENG-123 Implement feature"));
    launched_id.to_owned()
}

async fn check_chat_reuse(core: &EngineCore, tools: &Tools, item_id: &str, chat_id: &str) {
    let args = json!({"workItemId": item_id, "prompt": "Follow up", "idempotencyKey": "reuse-chat"});
    let reused = tools.call("create_chat", args.clone()).await.unwrap();
    assert_eq!(reused["chatId"], chat_id);
    assert_eq!(reused["reusedExisting"], true);
    let retry = tools.call("create_chat", args).await.unwrap();
    assert_eq!(retry["chatId"], chat_id);
    assert_eq!(retry["replayed"], true);
    let operation = mast_proto::agent_tools::operation_id(PARENT, "reuse-chat");
    let command_id = derive_id(&operation, roles::COMMAND);
    let commands = core.doc_host.open(chat_id).unwrap().doc().read_commands().unwrap();
    let command = commands.iter().find(|command| command.id == command_id).unwrap();
    assert_eq!(commands.iter().filter(|command| command.id == command_id).count(), 1);
    match &command.payload {
        mast_doc::SessionCommandPayload::AutoMessage { request, .. } => assert!(request.prompt.contains("Follow up")),
        other => panic!("unexpected command: {other:?}"),
    }
}

async fn check_reuse_without_prompt(tools: &Tools, item_id: &str, chat_id: &str) {
    let result = tools.call("create_chat", json!({"workItemId": item_id, "idempotencyKey": "reuse-without-prompt"})).await.unwrap();
    assert_eq!(result["chatId"], chat_id);
    assert_eq!(result["reusedExisting"], true);
    let err = tools.call("create_chat", json!({"workItemId": item_id, "prompt": "Different", "idempotencyKey": "reuse-without-prompt"})).await.unwrap_err();
    assert!(err.contains("operationConflict"), "{err}");
}

async fn check_concurrent_chat_reuse(core: &EngineCore, tools: &Tools, item_id: &str, chat_id: &str) {
    let first = json!({"workItemId": item_id, "prompt": "Concurrent first", "idempotencyKey": "concurrent-first"});
    let second = json!({"workItemId": item_id, "prompt": "Concurrent second", "idempotencyKey": "concurrent-second"});
    let (a, b) = tokio::join!(tools.call("create_chat", first.clone()), tools.call("create_chat", second.clone()));
    for reply in [a.unwrap(), b.unwrap()] {
        assert_eq!(reply["chatId"], chat_id);
        assert_eq!(reply["reusedExisting"], true);
    }
    tools.call("create_chat", first).await.unwrap();
    tools.call("create_chat", second).await.unwrap();
    let commands = core.doc_host.open(chat_id).unwrap().doc().read_commands().unwrap();
    for (key, prompt) in [("concurrent-first", "Concurrent first"), ("concurrent-second", "Concurrent second")] {
        let operation = mast_proto::agent_tools::operation_id(PARENT, key);
        let command_id = derive_id(&operation, roles::COMMAND);
        let message_id = derive_id(&operation, roles::MESSAGE);
        let matching: Vec<_> = commands.iter().filter(|command| command.id == command_id).collect();
        assert_eq!(matching.len(), 1);
        assert!(matches!(&matching[0].payload,
            mast_doc::SessionCommandPayload::AutoMessage { request, message_id: id, .. }
                if request.prompt.contains(prompt) && id == &message_id));
    }
    wait_for(|| {
        let entries = core.doc_host.open(chat_id).unwrap().doc().read_entries().unwrap();
        ["Concurrent first", "Concurrent second"].iter().all(|prompt|
            entries.iter().filter(|entry| entry.role == mast_doc::MessageRole::User
                && entry.parts.iter().any(|part| matches!(part, mast_doc::MessagePart::Text { text, .. } if text.contains(prompt)))).count() == 1)
    }, "each reused prompt to be delivered once").await;
    wait_for(|| {
        let commands = core.doc_host.open(chat_id).unwrap().doc().read_commands().unwrap();
        ["concurrent-first", "concurrent-second"].iter().all(|key| {
            let operation = mast_proto::agent_tools::operation_id(PARENT, key);
            let id = derive_id(&operation, roles::COMMAND);
            commands.iter().any(|command| command.id == id
                && command.status == mast_doc::SessionCommandStatus::Applied
                && matches!(command.resolution.as_deref(), Some("run" | "steer" | "queue")))
        })
    }, "automatic delivery modes").await;
}

async fn check_force_chat(core: &EngineCore, tools: &Tools, item_id: &str, chat_id: &str) {
    let forced = tools.call("create_chat", json!({"workItemId": item_id,
        "prompt": "Independent work", "newChat": true, "idempotencyKey": "force-chat"})).await.unwrap();
    assert_ne!(forced["chatId"], chat_id);
    let chat = core.workspace.chat(forced["chatId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(chat.title.as_deref(), Some("ENG-123 Implement feature"));
}

async fn check_unlinked_item_title(core: &EngineCore, tools: &Tools) {
    let item = tools.call("work_outline_create_child", json!({"title": "Review behavior", "idempotencyKey": "create-unlinked"})).await.unwrap();
    let chat = tools.call("create_chat", json!({"workItemId": item["workItemId"], "prompt": "Review the change", "idempotencyKey": "launch-unlinked"})).await.unwrap();
    let stored = core.workspace.chat(chat["chatId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(stored.title.as_deref(), Some("Review behavior"));
}

async fn check_planning_item_title(core: &EngineCore, tools: &Tools) {
    let item = tools.call("work_outline_create_child", json!({"title": "Planning", "idempotencyKey": "create-planning"})).await.unwrap();
    let chat = tools.call("create_chat", json!({"workItemId": item["workItemId"], "prompt": "Plan the change", "idempotencyKey": "launch-planning"})).await.unwrap();
    let stored = core.workspace.chat(chat["chatId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(stored.title.as_deref(), Some("Feature Planning"));
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

/// The engine's role gate must not refuse the calls `mast-mcp` makes on its own while serving a
/// listed tool. A real engine, so the gate, the scope replies and the MCP reach rules all run.
#[tokio::test(flavor = "multi_thread")]
async fn workers_and_verifiers_keep_the_reach_mcp_resolves_through_internal_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FakeClaude));
    let core = EngineCore::assemble(&root.join("data"), Arc::new(registry), HarnessId::ClaudeCode, None)
        .expect("engine core assembles");
    let admin = mast_rpc::memory_client(core.rpc_service());
    admin
        .call(methods::MUTATE, json!({ "op": "createSpace", "spaceId": SPACE,
            "deviceId": core.device_id, "path": project.to_string_lossy() }))
        .await
        .unwrap();
    let config = mast_proto::ChatConfig {
        harness: HarnessId::ClaudeCode,
        ..serde_json::from_value(json!({ "harness": "claude-code", "model": null,
            "reasoning": null, "sandbox": "workspace-write" })).unwrap()
    };
    let chat = |id: &str, parent: Option<&str>| {
        core.workspace
            .create_chat_with_parent(id, Some(SPACE), Some(&core.device_id), Some(config.clone()),
                Some(project.to_string_lossy().into_owned()), parent.map(str::to_owned))
            .unwrap();
    };
    let outline = async |mutation: Value| {
        admin.call(methods::WORK_OUTLINE_MUTATE, mutation).await.unwrap();
    };
    outline(json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Project", "operationId": "p" })).await;
    let top = derive_id("p", roles::ITEM);
    outline(json!({ "op": "updateWorkItem", "workItemId": top, "changes": { "isProject": true }, "operationId": "p-mark" })).await;
    let item = async |op: &str, role: Option<&str>| {
        outline(json!({ "op": "createWorkItem", "spaceId": SPACE, "title": op,
            "parentWorkItemId": top, "operationId": op })).await;
        let id = derive_id(op, roles::ITEM);
        if let Some(role) = role {
            outline(json!({ "op": "updateWorkItem", "workItemId": id, "operationId": format!("{op}-spec"),
                "changes": { "taskSpec": { "requiredReport": true, "taskClass": { "role": role } } } })).await;
        }
        id
    };
    let task = item("task", Some("implementer")).await;
    let review = item("review", Some("reviewer")).await;
    let other = item("other", None).await;
    chat(PARENT, None);
    for (id, parent, assigned) in [
        ("chat-predecessor", Some(PARENT), &task),
        ("chat-reviewer", Some(PARENT), &review),
        ("chat-peer", None, &other),
    ] {
        chat(id, parent);
        outline(json!({ "op": "assignChatToWorkItem", "chatId": id, "workItemId": assigned,
            "operationId": format!("assign-{id}") })).await;
    }
    // A real hand-off: the successor is the Worker, and its predecessor is archived.
    let handed = admin
        .call(methods::HAND_OFF_ORCHESTRATOR, json!({ "chatId": "chat-predecessor",
            "summary": "Continue the task", "operationId": "worker-handoff" }))
        .await
        .unwrap();
    let successor = handed["chatId"].as_str().unwrap().to_owned();
    core.workspace.set_chat_archived("chat-predecessor", true).unwrap();
    let agent = |chat_id: &str| {
        let lease = core.sessions.agent_tokens().mint(chat_id, "run-1", SPACE, &core.device_id);
        let tools = Tools::new(Arc::new(Mast::with_client(
            mast_rpc::memory_client(core.rpc_service()), Some(lease.token().to_owned()))));
        (lease, tools)
    };
    let (worker_lease, worker) = agent(&successor);
    let (reviewer_lease, reviewer) = agent("chat-reviewer");

    // Both are really held to their role lists.
    for tools in [&worker, &reviewer] {
        let refused = tools.call("work_outline_set_status", json!({ "status": "active", "idempotencyKey": "s" })).await.unwrap_err();
        assert!(refused.contains("not available to this role"), "{refused}");
    }
    // Project reach (`chat_project_scope_chats`).
    for tools in [&worker, &reviewer] {
        let seen = tools.call("read_chat", json!({ "chat": "chat-peer" })).await.expect("a project chat");
        assert!(seen.to_string().contains("chat-peer"), "{seen}");
    }

    // An archived hand-off predecessor is reachable only through the engine's scope reply.
    worker.call("read_chat", json!({ "chat": "chat-predecessor", "limit": 2 })).await
        .expect("a Worker reads its hand-off predecessor");

    // The RPC surface dispatches some tools itself; the role gate covers those too, whatever
    // the MCP catalog would have shown (`mast agent-tool` does not consult it).
    let chats_before = core.workspace.read_chats().unwrap().len();
    for token in [worker_lease.token(), reviewer_lease.token()] {
        for (tool, arguments) in [
            ("chat_launch_child_for_item", json!({ "idempotencyKey": "launch", "workItemId": task,
                "prompt": "go", "config": config })),
            ("hand_off_orchestrator", json!({ "idempotencyKey": "hand", "summary": "take over" })),
            ("chat_queue_child_turn", json!({ "chatId": "x", "idempotencyKey": "turn", "prompt": "go" })),
            ("quota_status", json!({})),
            ("mast_manage", json!({})),
        ] {
            let refused = admin
                .call(mast_proto::agent_tools::TOOL_CALL_METHOD,
                    json!({ "token": token, "tool": tool, "arguments": arguments }))
                .await
                .unwrap_err()
                .to_string();
            assert!(refused.contains("not available to this role"), "{tool}: {refused}");
        }
        // `final_handback` is behind the same gate and let through: it fails on its own checks.
        let handback = admin
            .call(mast_proto::agent_tools::TOOL_CALL_METHOD, json!({ "token": token,
                "tool": "final_handback", "arguments": {} }))
            .await
            .unwrap_err()
            .to_string();
        assert!(!handback.contains("not available to this role"), "{handback}");
    }
    // Through the CLI's dispatcher too.
    for tools in [&worker, &reviewer] {
        let refused = tools.call("hand_off_orchestrator", json!({ "summary": "take over", "idempotencyKey": "cli" })).await.unwrap_err();
        assert!(refused.contains("not available to this role"), "{refused}");
    }
    assert_eq!(core.workspace.read_chats().unwrap().len(), chats_before, "nothing was started");
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
