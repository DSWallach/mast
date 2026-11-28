#[tokio::test]
async fn engine_rpc_surfaces_share_handoff_launch_gate() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    let core = crate::EngineCore::assemble(dir.path(), std::sync::Arc::new(HarnessRegistry::new()), HarnessId::Mock, None).unwrap();
    let first = core.rpc_service();
    let second = core.rpc_service();
    let held = first.launch.test_gate().await;
    assert!(tokio::time::timeout(std::time::Duration::from_millis(30), second.launch.test_gate()).await.is_err());
    drop(held);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(1), second.launch.test_gate()).await.is_ok());
    core.shutdown().await;
}

#[tokio::test]
async fn hand_off_config_preserves_and_validates_selection() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(std::sync::Arc::new(mast_harness::mock::MockHarness {
        script: vec![],
    }));
    let mut options = serde_json::Map::new();
    options.insert("old".into(), serde_json::json!(true));
    let source = mast_proto::ChatConfig {
        approval_rules: false,
        harness: HarnessId::Codex,
        model: Some("old-model".into()),
        reasoning: Some(mast_proto::ReasoningLevel::High),
        model_options: options,
        sandbox: mast_proto::SandboxLevel::ReadOnly,
        auto_approve: true,
    };
    let unchanged = hand_off_config(
        &registry,
        dir.path(),
        Some(source.clone()),
        HarnessId::Codex,
        None,
        None,
        None,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(unchanged, Some(source.clone()));
    let changed = hand_off_config(
        &registry,
        dir.path(),
        Some(source.clone()),
        HarnessId::Codex,
        Some(HarnessId::Mock),
        Some("mock-1".into()),
        Some(mast_proto::ReasoningLevel::Medium),
        Default::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(changed.harness, HarnessId::Mock);
    assert_eq!(changed.model.as_deref(), Some("mock-1"));
    assert_eq!(changed.reasoning, Some(mast_proto::ReasoningLevel::Medium));
    assert!(changed.model_options.is_empty());
    assert_eq!(changed.sandbox, source.sandbox);
    assert!(changed.auto_approve);
    assert!(
        hand_off_config(
            &registry,
            dir.path(),
            Some(source),
            HarnessId::Codex,
            Some(HarnessId::Mock),
            Some("unknown".into()),
            None,
            Default::default(),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn hand_off_config_applies_explicit_context_window_only_when_given() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(std::sync::Arc::new(mast_harness::mock::MockHarness { script: vec![] }));
    let source = mast_proto::ChatConfig {
        approval_rules: false,
        harness: HarnessId::Mock,
        model: Some("mock-1".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    };
    let mut picked = serde_json::Map::new();
    picked.insert("contextWindow".into(), serde_json::json!("500k"));
    // Only the window changed: the explicit choice must still reach the chat.
    let explicit = hand_off_config(&registry, dir.path(), Some(source.clone()), HarnessId::Mock, None, None, None, picked)
        .await.unwrap().unwrap();
    assert_eq!(explicit.model_options.get("contextWindow"), Some(&serde_json::json!("500k")));
    // No pick: the config carries no window, so the harness default applies.
    let default = hand_off_config(&registry, dir.path(), Some(source.clone()), HarnessId::Mock, None, None, None, Default::default())
        .await.unwrap();
    assert_eq!(default, Some(source));
}

#[tokio::test]
async fn hand_off_config_keeps_the_picked_window_across_a_harness_switch() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(std::sync::Arc::new(mast_harness::mock::MockHarness { script: vec![] }));
    let mut inherited = serde_json::Map::new();
    inherited.insert("contextWindow".into(), serde_json::json!("1m"));
    inherited.insert("old".into(), serde_json::json!(true));
    let source = mast_proto::ChatConfig {
        approval_rules: false,
        harness: HarnessId::Codex,
        model: Some("old-model".into()),
        reasoning: None,
        model_options: inherited,
        sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    };
    let mut picked = serde_json::Map::new();
    picked.insert("contextWindow".into(), serde_json::json!("500k"));
    let config = hand_off_config(&registry, dir.path(), Some(source), HarnessId::Codex, Some(HarnessId::Mock), Some("mock-1".into()), None, picked)
        .await.unwrap().unwrap();
    assert_eq!(config.harness, HarnessId::Mock);
    // The switch clears the inherited options; the explicit window is applied after.
    let mut expected = serde_json::Map::new();
    expected.insert("contextWindow".into(), serde_json::json!("500k"));
    assert_eq!(config.model_options, expected);
}

#[test]
fn a_harness_switch_keeps_the_named_agent_only_between_agent_harnesses() {
    use super::*;
    let mut options = serde_json::Map::new();
    options.insert("contextWindow".into(), serde_json::json!("1m"));
    options.insert(mast_proto::AGENT_OPTION.into(), serde_json::json!("pr-review"));
    let mut kept = serde_json::Map::new();
    kept.insert(mast_proto::AGENT_OPTION.into(), serde_json::json!("pr-review"));
    for (from, to) in [
        (HarnessId::ClaudeCode, HarnessId::Codex),
        (HarnessId::Codex, HarnessId::Antigravity),
        (HarnessId::Antigravity, HarnessId::ClaudeCode),
    ] {
        assert_eq!(options_across_harness_switch(options.clone(), from, to), kept, "{from:?} -> {to:?}");
    }
    for (from, to) in [
        (HarnessId::ClaudeCode, HarnessId::Cursor),
        (HarnessId::ClaudeCode, HarnessId::Opencode),
        (HarnessId::Opencode, HarnessId::ClaudeCode),
        (HarnessId::Codex, HarnessId::Mock),
    ] {
        assert!(options_across_harness_switch(options.clone(), from, to).is_empty(), "{from:?} -> {to:?}");
    }
}
use super::*;

/// Wire-contract pin: `SET_CHAT_LINK`'s request shape
/// (`{chatId, kind, value}`), including `value: null` decoding to
/// `None` (the unlink case) rather than failing to parse.
#[test]
fn set_chat_link_params_parse_the_documented_wire_shape() {
    let p: SetChatLinkParams = parse_params(serde_json::json!({
        "chatId": "c1",
        "kind": "pr",
        "value": "https://github.com/acme/widgets/pull/1",
    }))
    .unwrap();
    assert_eq!(p.chat_id, "c1");
    assert_eq!(p.kind, "pr");
    assert_eq!(
        p.value.as_deref(),
        Some("https://github.com/acme/widgets/pull/1")
    );
    assert!(p.operation.is_none());

    let cleared: SetChatLinkParams = parse_params(serde_json::json!({
        "chatId": "c1",
        "kind": "ticket",
        "value": null,
    }))
    .unwrap();
    assert_eq!(cleared.kind, "ticket");
    assert!(cleared.value.is_none());
    assert!(cleared.operation.is_none());

    let remove: SetChatLinkParams = parse_params(serde_json::json!({
        "chatId": "c1",
        "kind": "pr",
        "value": "https://github.com/acme/widgets/pull/2",
        "operation": "remove",
    }))
    .unwrap();
    assert_eq!(remove.operation.as_deref(), Some("remove"));
}

/// `CHAT_LINK_STATUS`/`SET_CHAT_LINK` are both device-local (`gh`/CLI
/// state, or a workspace-doc write scoped to this device's chat rows) —
/// neither method is in `forwardable`'s allow-list, so both default to
/// IPC-only. A silent future addition to that list would relay-forward a
/// method whose doc comment explicitly promises otherwise.
#[test]
fn personal_assistant_methods_are_ipc_only() {
    assert!(!forwardable(methods::GET_PERSONAL_ASSISTANT));
    assert!(!forwardable(methods::SET_PERSONAL_ASSISTANT));
    assert!(!forwardable(methods::CLEAR_PERSONAL_ASSISTANT));
}

#[test]
fn chat_link_methods_are_ipc_only() {
    assert!(!forwardable(methods::CHAT_LINK_STATUS));
    assert!(!forwardable(methods::SET_CHAT_LINK));
}

/// Work Outline reads and writes are pure registry operations served by
/// the local replica (like `WATCH_CHATS` and `MUTATE`): never relay-forwarded.
/// Hosts that predate the feature answer `UnknownMethod`, which clients
/// treat as "host does not support the Work Outline".
#[test]
fn work_outline_methods_are_local_not_forwardable() {
    assert!(!forwardable(methods::WATCH_WORK_OUTLINE));
    assert!(!forwardable(methods::WORK_OUTLINE_MUTATE));
    assert!(!forwardable(methods::WORK_OUTLINE_HEALTH));
    assert!(!forwardable(methods::WORK_OUTLINE_HEALTH_ARCHIVE));
    for linear in [
        methods::LINEAR_LIST_ACCOUNTS,
        methods::LINEAR_ADD_ACCOUNT,
        methods::LINEAR_RENAME_ACCOUNT,
        methods::LINEAR_REPLACE_ACCOUNT_KEY,
        methods::LINEAR_REMOVE_ACCOUNT,
        methods::LINEAR_STATUS,
        methods::LINEAR_SET_API_KEY,
        methods::LINEAR_CLEAR_API_KEY,
        methods::LINEAR_ISSUE,
        methods::LINEAR_MY_ISSUES,
        methods::LINEAR_MY_PROJECTS,
        methods::LINEAR_PROJECT_ISSUES,
        methods::LINEAR_PROJECT_DETAIL,
        methods::LINEAR_REFRESH_LINKED_ITEMS,
    ] {
        assert!(!forwardable(linear), "{linear} is device-local");
    }
    for github in [
        methods::PR_STATUS,
        methods::PR_REVIEW_SYNC_NOW,
        methods::PR_MY_OPEN,
        methods::PR_REVIEW_REQUESTED,
    ] {
        assert!(!forwardable(github), "{github} is device-local");
        assert!(!is_stream_method(github));
    }
    assert!(!forwardable(methods::WORK_OUTLINE_QUERY));
    assert!(!forwardable(methods::WORKFLOW_PACK_APPLY));
    assert!(!is_stream_method(methods::WORK_OUTLINE_QUERY));
    assert!(!is_stream_method(methods::WATCH_WORK_OUTLINE));
    // Execution validation runs against the owner's disk.
    assert!(forwardable(methods::VALIDATE_EXECUTION_CONFIGURATION));
    assert!(forwardable(methods::DESCRIBE_EXECUTION_REPOSITORY));
    assert!(!is_stream_method(methods::VALIDATE_EXECUTION_CONFIGURATION));
}

/// Chat diff targets belong to the device hosting the chat: a call for
/// another device's chat fails with `NotLocal` instead of being relayed.
#[test]
fn chat_diff_target_methods_are_local_not_forwardable() {
    for method in [
        methods::LIST_CHAT_DIFF_CHECKOUTS,
        methods::LIST_CHAT_REF_DIFF_CANDIDATES,
        methods::SET_CHAT_DIFF_TARGET,
        methods::WATCH_CHAT_DIFF_TARGETS,
    ] {
        assert!(!forwardable(method), "{method} is device-local");
        assert!(!is_stream_method(method));
    }
}

/// `SCAN_CHAT_SUBAGENTS`/`READ_SUBAGENT_TRANSCRIPT` both read this
/// machine's `~/.claude/projects` on-disk state — device-local, same
/// reasoning as the chat-link methods above.
#[test]
fn subagent_methods_are_ipc_only() {
    assert!(!forwardable(methods::SCAN_CHAT_SUBAGENTS));
    assert!(!forwardable(methods::READ_SUBAGENT_TRANSCRIPT));
}

/// `PLAN_CHAT_WORKSPACE`/`CREATE_CHAT_WORKTREE` are both device-local
/// (a TypeSafe judgment paired 1:1 with the worktree it feeds, and
/// device-local git/filesystem state) — neither is in `forwardable`'s
/// allow-list, same reasoning as `chat_link_methods_are_ipc_only` above.
#[test]
fn chat_workspace_plan_methods_are_ipc_only() {
    assert!(!forwardable(methods::PLAN_CHAT_WORKSPACE));
    assert!(!forwardable(methods::CREATE_CHAT_WORKTREE));
    assert!(!forwardable(methods::PLAN_CHAT_CLOSEOUT));
    assert!(!forwardable(methods::CLOSE_CHAT_WORKTREE));
}

#[test]
fn closeout_params_parse_camel_case() {
    let p: PlanChatCloseoutParams = parse_params(serde_json::json!({
        "chatId": "chat-1",
        "cwd": "/repo/.worktrees/workspace/eng-42",
    }))
    .unwrap();
    assert_eq!(p.chat_id.as_deref(), Some("chat-1"));
    assert_eq!(p.cwd, "/repo/.worktrees/workspace/eng-42");

    let p: PlanChatCloseoutParams = parse_params(serde_json::json!({
        "cwd": "/repo/.worktrees/repo-map-only",
    }))
    .unwrap();
    assert_eq!(p.chat_id, None);

    let p: CloseChatWorktreeParams = parse_params(serde_json::json!({
        "chatId": "chat-1",
        "cwd": "/repo/.worktrees/workspace/eng-42",
        "force": true,
    }))
    .unwrap();
    assert_eq!(p.chat_id.as_deref(), Some("chat-1"));
    assert_eq!(p.cwd, "/repo/.worktrees/workspace/eng-42");
    assert!(p.force);

    let p: CloseChatWorktreeParams = parse_params(serde_json::json!({
        "cwd": "/repo/.worktrees/repo-map-only",
    }))
    .unwrap();
    assert_eq!(p.chat_id, None);
    assert!(!p.force);
}

#[test]
fn closeout_cwd_must_match_the_chat_stored_cwd() {
    let root = tempfile::tempdir().unwrap();
    let stored = root.path().join("stored");
    let other = root.path().join("other");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    let accepted = validated_closeout_cwd(
        "chat-1",
        Some(stored.to_str().unwrap()),
        stored.join(".").to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(accepted, stored.canonicalize().unwrap());

    let err = validated_closeout_cwd(
        "chat-1",
        Some(stored.to_str().unwrap()),
        other.to_str().unwrap(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not match"), "{err}");
}

#[test]
fn closeout_cwd_refuses_a_chat_without_a_stored_cwd() {
    let root = tempfile::tempdir().unwrap();
    let err = validated_closeout_cwd("chat-1", None, root.path().to_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(err.contains("no stored working directory"), "{err}");
}

#[test]
fn plan_chat_workspace_params_parse_camel_case() {
    let p: PlanChatWorkspaceParams = parse_params(serde_json::json!({
        "message": "fix the login bug",
        "cwd": "/repo",
    }))
    .unwrap();
    assert_eq!(p.message, "fix the login bug");
    assert_eq!(p.cwd, "/repo");
}

#[test]
fn create_chat_worktree_params_parse_camel_case() {
    let p: CreateChatWorktreeParams = parse_params(serde_json::json!({
        "chatId": "chat-1",
        "repoPath": "/repo",
        "name": "eng-42-do-a-thing",
    }))
    .unwrap();
    assert_eq!(p.chat_id, "chat-1");
    assert_eq!(p.repo_path, "/repo");
    assert_eq!(p.name, "eng-42-do-a-thing");
}

#[tokio::test]
async fn explicit_install_rpc_verifies_archive_and_refreshes_descriptors() {
    use mast_harness::archive_install::{ArchivePin, ensure_installed, installed_entry};
    use sha2::{Digest, Sha512};
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if mast_proto::env::var_os("MAST_INSTALL_RPC_TEST").is_none() {
        let root = tempfile::tempdir().unwrap();
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rpc::tests::explicit_install_rpc_verifies_archive_and_refreshes_descriptors",
                "--nocapture",
            ])
            .env("MAST_INSTALL_RPC_TEST", "1")
            .env("MAST_ADAPTERS_DIR", root.path())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    if !mast_harness::acp::can_install(HarnessId::Antigravity) {
        return;
    }
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("server", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let digest = Box::leak(format!("{:x}", Sha512::digest(&bytes)).into_boxed_str());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Box::leak(
        format!("http://{}/archive.zip", listener.local_addr().unwrap()).into_boxed_str(),
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        let mut buf = [0; 4096];
        while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buf).await.unwrap();
            assert!(count > 0, "archive request closed before its headers");
            headers.extend_from_slice(&buf[..count]);
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&bytes).await.unwrap();
    });
    let pin = ArchivePin {
        name: "explicit-install-test",
        version: "1",
        url,
        entry: "server",
        sha512: digest,
    };
    let registry = HarnessRegistry::new();
    let descriptor = serde_json::from_value(serde_json::json!({
        "id": "antigravity", "name": "Antigravity", "supportsSteering": true,
        "steeringMode": "turn-boundary", "reasoningLevels": [], "installed": false
    }))
    .unwrap();
    registry.register_lazy(
        descriptor,
        Box::new(move || installed_entry(&pin).is_some()),
        Box::new(|| panic!("installation must not spawn the harness")),
    );
    assert!(!registry.descriptors()[0].installed);
    let result = install_harness_with(&registry, HarnessId::Antigravity, || async {
        ensure_installed(pin, "Test adapter").await.map(|_| ())
    })
    .await
    .unwrap();
    assert!(result[0].installed);
    assert_eq!(result[0].enabled, Some(true));
    assert!(result[0].can_install);
    assert!(installed_entry(&pin).unwrap().is_file());
    server.await.unwrap();
    // The verified marker makes another explicit install idempotent, even
    // after the archive server has stopped.
    install_harness_with(&registry, HarnessId::Antigravity, || async {
        ensure_installed(pin, "Test adapter").await.map(|_| ())
    })
    .await
    .unwrap();
    assert!(
        install_harness_with(&registry, HarnessId::Codex, || async {
            panic!("unsupported harness must not invoke an installer")
        })
        .await
        .is_err()
    );
    assert!(forwardable(methods::INSTALL_HARNESS));
    assert_eq!(
        forward_deadline(methods::INSTALL_HARNESS),
        std::time::Duration::from_secs(15 * 60)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_opening_tail_arrives_before_full_mirror_and_keeps_all_history() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store,
        DocHostConfig {
            device_id: "viewer".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("whale").unwrap();
    handle
        .doc()
        .push_message(&mast_doc::SessionMessageEntry {
            id: "turn".into(),
            role: mast_doc::MessageRole::Assistant,
            parts: (0..500)
                .map(|i| mast_doc::MessagePart::Text {
                    id: format!("part-{i}"),
                    text: "local text".into(),
                })
                .collect(),
            created_at: 0,
            device_id: "host".into(),
            status: None,
            continuation_of: None,
        })
        .unwrap();
    // Hold publication blocked: the opening must not await the full mirror.
    let held = handle.clone();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        held.import_transcript(|| {
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    locked_rx.recv().unwrap();
    let opening = tokio::time::timeout(
        Duration::from_secs(30),
        opening_doc_messages_stream(host.clone(), "whale".into()),
    )
    .await;
    release_tx.send(()).unwrap();
    blocker.join().unwrap();
    let mut stream = opening
        .expect("first paint must not wait for publication")
        .unwrap();
    let preview = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await.expect("preview timed out").unwrap();
    assert_eq!(preview["historyPending"], true);
    assert_eq!(preview["reset"][0]["parts"].as_array().unwrap().len(), 128);
    assert_eq!(preview["reset"][0]["parts"][0]["id"], "part-372");
    // A change after first paint lands in the full reset or, since the
    // attach starts eagerly, in the delta right after it; never lost.
    handle
        .write_user_message("arrived", "new local message", 1)
        .unwrap();
    let full = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await.expect("full mirror timed out").unwrap();
    assert!(full.get("historyPending").is_none());
    assert_eq!(full["reset"][0]["parts"].as_array().unwrap().len(), 500);
    handle
        .write_user_message("live", "after attach", 2)
        .unwrap();
    let mut entries = Vec::new();
    let update: mast_doc::TranscriptUpdate = serde_json::from_value(full).unwrap();
    mast_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
    while entries.len() < 3 {
        let value = tokio::time::timeout(Duration::from_secs(30), stream.next())
            .await.unwrap().unwrap();
        let update: mast_doc::TranscriptUpdate = serde_json::from_value(value).unwrap();
        mast_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
    }
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn", "arrived", "live"]
    );
    host.shutdown_workers().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_chat_attaches_full_history_without_waiting_for_another_ui_poll() {
    use crate::doc_host::{DocHost, DocHostConfig};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap());
    let host = DocHost::new(
        store,
        DocHostConfig {
            device_id: "viewer".into(),
            default_harness: mast_proto::HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("restored").unwrap();
    handle
        .write_user_message("old", "visible immediately after restart", 1)
        .unwrap();

    // Construct the subscription exactly as a restored full-chat tab
    // does, then deliberately do not poll even the preview.  Full-history
    // attachment is eager and therefore cannot depend on a later input
    // event waking the viewport's stream consumer.
    let mut stream = opening_doc_messages_stream(host.clone(), "restored".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while handle.message_watcher_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("authoritative transcript watch attaches without another poll");

    let preview = stream.next().await.unwrap();
    assert_eq!(preview["historyPending"], true);
    let full = stream.next().await.unwrap();
    assert!(full.get("historyPending").is_none());
    assert_eq!(full["reset"][0]["id"], "old");
    host.shutdown_workers().await;
}

#[tokio::test]
async fn antigravity_disable_does_not_launch_the_server() {
    let registry = HarnessRegistry::new();
    let executable = std::env::current_exe().unwrap();
    registry.register(std::sync::Arc::new(
        mast_harness::AcpHarness::grok().with_executable(executable.clone()),
    ));
    registry.register(std::sync::Arc::new(
        mast_harness::AcpHarness::antigravity().with_executable(executable),
    ));
    registry.set_enabled(HarnessId::Antigravity, true).unwrap();

    update_harness_enabled(&registry, HarnessId::Antigravity, false)
        .await
        .unwrap();
    assert!(!registry.enabled_set().contains(&HarnessId::Antigravity));
}

/// The UI's Switch/Forget calls send `{id, accountId, harness}` (+ optional
/// `targetDeviceId`); the extra fields must be tolerated, `accountId` wins.
#[test]
fn list_models_force_is_optional_and_backward_compatible() {
    let old: ListModelsParams =
        serde_json::from_value(serde_json::json!({"harness":"codex"})).unwrap();
    assert!(!old.force);
    assert!(old.chat_id.is_none());
    let forced: ListModelsParams =
        serde_json::from_value(serde_json::json!({"harness":"codex","force":true})).unwrap();
    assert!(forced.force);
}

#[test]
fn list_commands_accepts_chat_identity_for_safe_cwd_resolution() {
    let params: ListModelsParams = serde_json::from_value(serde_json::json!({
        "harness": "claude-code",
        "cwd": "/Users/person",
        "chatId": "chat-legacy-home"
    }))
    .unwrap();
    assert_eq!(params.chat_id.as_deref(), Some("chat-legacy-home"));
    assert_eq!(params.cwd, "/Users/person");
}

#[test]
fn agent_account_params_accept_ui_shape() {
    let p: AgentAccountParams = parse_params(serde_json::json!({
        "id": "acct-1",
        "accountId": "acct-1",
        "harness": "claude-code",
        "targetDeviceId": "dev-2",
    }))
    .expect("ui param shape");
    assert_eq!(p.account_id, "acct-1");
    assert_eq!(p.harness, HarnessId::ClaudeCode);
}

#[test]
fn sidebar_preferences_mutation_accepts_desktop_wire_shape() {
    let p: MutateParams = parse_params(serde_json::json!({
        "op": "changeSidebarPin",
        "change": {"action":"move","sessionId":"chat-b","before":"chat-a","after":null},
    }))
    .expect("sidebar preferences params");
    assert!(matches!(
        p,
        MutateParams::ChangeSidebarPin { change: mast_proto::SidebarPinChange::Move { session_id, before, .. } }
            if session_id == "chat-b" && before.as_deref() == Some("chat-a")
    ));
}

#[test]
fn local_device_is_not_forwardable() {
    assert!(!forwardable(methods::LOCAL_DEVICE));
    assert!(!forwardable(methods::ENGINE_INFO));
    assert!(!forwardable(methods::ENGINE_READY));
    assert!(forwardable(methods::QUEUE_COMMAND));
    assert!(forwardable(methods::SEARCH_FILES));
    assert!(forwardable(methods::SEARCH_GIT_HISTORY));
    assert!(forwardable(methods::FETCH_ALL));
    assert!(forwardable(methods::RESOLVE_GIT_AVATARS));
    assert!(forwardable(methods::WATCH_CHECKOUT_CHANGE_REQUEST));
    assert!(is_stream_method(methods::WATCH_CHECKOUT_CHANGE_REQUEST));
    assert!(forwardable(methods::HAND_OFF_ORCHESTRATOR));
    assert!(!is_stream_method(methods::HAND_OFF_ORCHESTRATOR));
    assert!(forwardable(methods::LIST_WORKSPACE_DIRECTORY));
    assert!(forwardable(methods::SEARCH_WORKSPACE_FILES));
    assert!(forwardable(methods::WATCH_WORKSPACE_SEARCH_FILES));
    assert!(is_stream_method(methods::WATCH_WORKSPACE_SEARCH_FILES));
    assert!(forwardable(methods::SEARCH_WORKSPACE_CONTENT));
    assert!(is_stream_method(methods::SEARCH_WORKSPACE_CONTENT));
    assert!(forwardable(methods::READ_WORKSPACE_FILE));
    assert!(forwardable(methods::READ_WORKSPACE_IMAGE));
    assert!(forwardable(methods::WRITE_WORKSPACE_FILE));
    assert!(forwardable(methods::WATCH_WORKSPACE_FILES));
    assert!(forwardable(methods::WATCH_WORKSPACE_GIT_STATUS));
    assert!(!is_stream_method(methods::LIST_WORKSPACE_DIRECTORY));
    assert!(!is_stream_method(methods::SEARCH_WORKSPACE_FILES));
    assert!(!is_stream_method(methods::READ_WORKSPACE_FILE));
    assert!(!is_stream_method(methods::WRITE_WORKSPACE_FILE));
    assert!(is_stream_method(methods::WATCH_WORKSPACE_FILES));
    assert!(is_stream_method(methods::WATCH_WORKSPACE_GIT_STATUS));
}

/// Every forwardable unary method gets a bounded reply deadline —
/// interactive calls fail fast, network-bound git/update calls get the
/// long leash, and nothing awaits forever (the "Sending…" wedge).
#[test]
fn forward_deadlines_are_tiered_and_bounded() {
    for method in [methods::LIST_MODELS, methods::LIST_COMMANDS] {
        assert_eq!(
            forward_deadline(method),
            std::time::Duration::from_secs(100)
        );
    }
    use std::time::Duration;
    assert_eq!(
        forward_deadline(methods::CREATE_WORKTREE),
        Duration::from_secs(120)
    );
    assert_eq!(
        forward_deadline(methods::CLONE_REPO),
        Duration::from_secs(15 * 60)
    );
    assert_eq!(
        forward_deadline(methods::LIST_BRANCHES),
        Duration::from_secs(30)
    );
    assert_eq!(
        forward_deadline(methods::QUEUE_COMMAND),
        Duration::from_secs(30)
    );
}

#[test]
fn tool_file_paths_keep_workspace_activity_only() {
    assert_eq!(
        tool_file_path(&ToolCall::EditFile {
            path: "src/main.rs".into(),
            old_string: None,
            new_string: None,
        }),
        Some("src/main.rs")
    );
    assert_eq!(
        tool_file_path(&ToolCall::Exec {
            command: "cargo test".into(),
        }),
        None
    );
}

#[tokio::test]
async fn quota_status_reads_account_caches_and_only_the_authorized_chat() {
    use super::*;
    use mast_proto::{
        AgentEvent, AgentUsageWindow, CreateWorkItemParams, RunRequest, StandingRoleKind,
        WorkOutlineMutation,
        agent_tools::{TOOL_CALL_METHOD, tools::QUOTA_STATUS},
        token_usage::{TokenCounts, UsageMeasurement},
    };
    use serde_json::{Value, json};
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let core = crate::EngineCore::assemble(
        root.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let rpc = core.rpc_service();
    core.workspace
        .create_space(
            "space",
            &core.device_id,
            root.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let scope = rpc
        .work_outline()
        .mutate(&WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: "space".into(),
            parent_work_item_id: None,
            placement: Default::default(),
            title: "Quota fixture".into(),
            description: None,
            status: None,
            operation_id: "quota-fixture-scope".into(),
        }))
        .unwrap()
        .work_item_id
        .unwrap();
    for (chat, role) in [
        ("manager", Some(StandingRoleKind::Manager)),
        ("lead", Some(StandingRoleKind::Lead)),
        ("worker", None),
    ] {
        core.workspace
            .create_chat(chat, Some("space"), None, None, None)
            .unwrap();
        if let Some(role) = role {
            core.workspace
                .adopt_standing_role(role, &scope, chat, &[])
                .unwrap();
        }
    }
    let tokens = rpc.agent_tools().tokens();
    let manager = tokens.mint("manager", "manager-run", "space", &core.device_id);
    let lead = tokens.mint("lead", "lead-run", "space", &core.device_id);
    let worker = tokens.mint("worker", "worker-run", "space", &core.device_id);
    let stale = tokens.mint("manager", "old-run", "space", &core.device_id);
    let stale_token = stale.token().to_owned();
    drop(stale);
    let args = |token: &str| json!({"token": token, "tool": QUOTA_STATUS, "arguments": {}});
    let client = mast_rpc::memory_client(rpc.clone());
    let quota = |token: &str| client.call(TOOL_CALL_METHOD, args(token));

    assert!(
        quota(worker.token())
            .await
            .unwrap_err()
            .to_string()
            .contains("agent-tools/forbidden")
    );
    assert!(
        quota(&stale_token)
            .await
            .unwrap_err()
            .to_string()
            .contains("agent-tools/unauthenticated")
    );

    let request: RunRequest = serde_json::from_value(json!({
        "prompt": "fixture", "cwd": root.path(), "sandbox": "read-only", "attachments": [],
    }))
    .unwrap();
    let usage = core.sessions.token_usage().unwrap();
    // The ledger keys turns by Run ID and turn number. Production sessions
    // allocate a distinct Run ID for each run, including across chats.
    usage.begin("manager", "manager-run", &request, "Conversation");
    usage.begin("worker", "worker-run", &request, "Conversation");
    for (chat, scope, input, output) in [
        ("manager", "request", 4, 1),
        ("manager", "turnIncludingChildren", 20, 3),
        ("manager", "resumeBaseline", 600, 500),
        ("worker", "turn", 100, 50),
    ] {
        usage.observe(
            chat,
            &AgentEvent::UsageMeasurement {
                measurement: UsageMeasurement {
                    id: scope.into(),
                    model: Some("quota-model".into()),
                    scope: scope.into(),
                    counts: TokenCounts {
                        input,
                        output,
                        ..Default::default()
                    },
                },
            },
        );
    }

    let window = |label: &str, used_fraction, resets_at| AgentUsageWindow {
        label: label.into(),
        used_fraction,
        resets_at,
    };
    let reset = chrono::DateTime::from_timestamp(1_900_000_000, 0).unwrap();
    let fixtures = [
        (
            HarnessId::Codex,
            vec![
                window("Session", 0.25, Some(reset)),
                window("Week", 0.50, None),
            ],
        ),
        (
            HarnessId::ClaudeCode,
            vec![window("Week · Sonnet", 0.75, Some(reset))],
        ),
        (
            HarnessId::Antigravity,
            vec![
                window("Session", 0.125, Some(reset)),
                window("Week", 0.25, None),
            ],
        ),
    ];
    let managed = core
        .managed_accounts
        .create(HarnessId::Codex, "Managed fixture")
        .unwrap();
    std::fs::write(
        core.managed_accounts
            .home_of(HarnessId::Codex, &managed.id)
            .join("auth.json"),
        r#"{"tokens":{"access_token":"fixture"}}"#,
    )
    .unwrap();
    core.agent_accounts.quota_test_cache(
        HarnessId::Codex,
        Some(&managed.id),
        fixtures[0].1.clone(),
        true,
    );
    let expected: Vec<_> = fixtures
        .into_iter()
        .map(|(harness, windows)| {
            let id = core.agent_accounts.quota_test_slot(harness);
            core.agent_accounts
                .quota_test_cache(harness, None, windows.clone(), false);
            (harness, id, windows)
        })
        .collect();

    // The broker probes the live sign-ins: point it at a closed local port so
    // nothing leaves this machine.
    core.agent_accounts
        .set_usage_urls("http://127.0.0.1:9", "http://127.0.0.1:9");
    let result = quota(manager.token()).await.unwrap();
    assert!(result["broker"]["providers"]["codex"].is_object(), "{result}");
    let broker_bytes = result["broker"].to_string().len();
    assert!(broker_bytes < 550, "{broker_bytes} bytes");
    // No usage could be read, so nothing is over the soft cap.
    assert!(result.get("wake").is_none(), "{result}");
    let headroom = client
        .call(methods::GET_QUOTA_HEADROOM, json!({}))
        .await
        .unwrap();
    assert_eq!(headroom["providers"].as_array().unwrap().len(), 3);
    let accounts = client
        .call(methods::LIST_AGENT_ACCOUNTS, json!({}))
        .await
        .unwrap();
    let managed_accounts = client
        .call(methods::LIST_MANAGED_ACCOUNTS, json!({}))
        .await
        .unwrap();
    assert_eq!(result["accounts"], accounts["accounts"]);
    assert_eq!(result["managedAccounts"], managed_accounts["accounts"]);
    assert_eq!(
        result["managedAccounts"][0]["usageWindows"],
        json!(expected[0].2)
    );
    let find_account = |result: &Value, id: &str| {
        result["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == id)
            .unwrap()
            .clone()
    };
    for (harness, id, windows) in &expected {
        let account = find_account(&result, id);
        assert_eq!(account["harness"], json!(harness));
        assert_eq!(account["usageWindows"], json!(windows));
    }
    assert_eq!(
        result["chatTokenTotals"],
        json!({"chatId":"manager","input":20,"output":3})
    );
    assert!(
        quota(lead.token())
            .await
            .unwrap()
            .get("chatTokenTotals")
            .is_none()
    );

    let (harness, id, windows) = &expected[0];
    core.agent_accounts
        .quota_test_cache(*harness, None, windows.clone(), true);
    let expired = quota(manager.token()).await.unwrap();
    assert_eq!(find_account(&expired, id)["usageWindows"], json!([]));
    assert_eq!(expired["managedAccounts"], result["managedAccounts"]);

    assert!(core.workspace.delete_chat("worker").unwrap());
    assert!(
        quota(worker.token())
            .await
            .unwrap_err()
            .to_string()
            .contains("agent-tools/unauthenticated")
    );
    core.shutdown().await;
}
