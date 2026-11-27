#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

#[test]
fn doctor_reports_missing_prerequisites_with_fixes() {
    let profile = tempfile::Builder::new()
        .prefix("mast-first-run-")
        .tempdir()
        .unwrap();
    let root = profile.path();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mast"))
        .args(["doctor", "--json"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("MAST_DATA_DIR", root.join("data"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_EXECUTABLE", root.join("missing-claude"))
        .env("CODEX_EXECUTABLE", root.join("missing-codex"))
        .env("MAST_NO_LOGIN_SHELL", "1")
        .env("PATH", root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["exit_code"], 1);
    let checks = report["checks"].as_array().unwrap();
    for id in [
        "claude",
        "codex",
        "agent CLI",
        "gh",
        "git",
        "cargo",
        "rustc",
    ] {
        let check = checks.iter().find(|check| check["id"] == id).unwrap();
        assert_ne!(check["status"], "ok", "{check}");
        assert!(
            check["fix"].as_str().is_some_and(|fix| !fix.is_empty()),
            "{check}"
        );
    }
    let agent = checks
        .iter()
        .find(|check| check["id"] == "agent CLI")
        .unwrap();
    assert_eq!(agent["status"], "error");
    assert!(agent["fix"].as_str().unwrap().contains("sign in"));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_tool_uses_mcp_dispatch_and_engine_idempotency() {
    use mast_engine::{EngineCore, HarnessRegistry, work_outline::ledger_operation_key};
    use mast_proto::agent_tools::{derived_idempotency_key, operation_id, tools};
    use mast_proto::work_outline::{CreateWorkItemParams, WorkOutlineMutation};
    use serde_json::{Value, json};
    use std::sync::Arc;

    let profile = tempfile::tempdir().unwrap();
    let root = profile.path().to_str().unwrap();
    let core = EngineCore::assemble(
        profile.path(),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace
        .create_space("space", &core.device_id, root, None, false)
        .unwrap();
    core.workspace
        .create_chat("chat", Some("space"), None, None, None)
        .unwrap();
    let service = core.rpc_service();
    let lease = service
        .agent_tools()
        .tokens()
        .mint("chat", "run", "space", &core.device_id);
    let item = service
        .work_outline()
        .mutate(&WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: "space".into(),
            parent_work_item_id: None,
            placement: Default::default(),
            title: "CLI parity".into(),
            description: None,
            status: None,
            operation_id: "create-cli-test-item".into(),
        }))
        .unwrap();
    let item_id = item.work_item_id.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(mast_rpc::serve_ws_listener(listener, service));
    let url = format!("ws://{addr}");
    let mcp = mast_mcp::Tools::new(Arc::new(mast_mcp::Mast::new(
        url.clone(),
        Some(lease.token().into()),
    )));
    let cli = |tool: &str, arguments: Value| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mast"))
            .args(["agent-tool", tool, "--json", &arguments.to_string()])
            .current_dir(root)
            .env_clear()
            .env("HOME", root)
            .env("MAST_IPC_PORT", addr.port().to_string())
            .env("MAST_AGENT_TOKEN", lease.token())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    let definition = mcp
        .list()
        .into_iter()
        .find(|tool| tool.name == tools::QUOTA_STATUS)
        .unwrap();
    assert!(serde_json::to_vec(&definition).unwrap().len() < 450);
    let quota = cli(tools::QUOTA_STATUS, json!({}));
    assert_eq!(
        quota,
        mcp.call(tools::QUOTA_STATUS, json!({})).await.unwrap()
    );
    let rpc = mast_mcp::Mast::new(url, None);
    let accounts = rpc
        .call(mast_rpc::methods::LIST_AGENT_ACCOUNTS, json!({}))
        .await
        .unwrap();
    let managed = rpc
        .call(mast_rpc::methods::LIST_MANAGED_ACCOUNTS, json!({}))
        .await
        .unwrap();
    assert_eq!(quota["accounts"], accounts["accounts"]);
    assert_eq!(quota["warnings"], accounts["warnings"]);
    assert_eq!(quota["managedAccounts"], managed["accounts"]);
    assert!(quota.get("chatTokenTotals").is_none());
    assert!(
        mcp.call(tools::QUOTA_STATUS, json!({"forceUsage":true}))
            .await
            .is_err()
    );

    let who = cli("whoami", json!({}));
    assert_eq!(who, mcp.call("whoami", json!({})).await.unwrap());
    assert_eq!(who["authenticated"], true);
    assert_eq!(who["chat"]["id"], "chat");

    let tool = tools::ASSOCIATE_CURRENT_CHAT;
    let key = derived_idempotency_key(tool, &["chat", &item_id, ""]);
    let expected_operation = operation_id("chat", &key);
    let reply = cli(tool, json!({"work_item_id": item_id}));
    // Operation marks become durable with the registry snapshot.
    core.workspace.persist_registry_now().unwrap();
    assert!(
        core.workspace
            .ledger_holds(&ledger_operation_key(&expected_operation))
    );
    assert_eq!(
        reply,
        mcp.call(
            tool,
            json!({
                "workItemId": item_id, "idempotencyKey": key,
            })
        )
        .await
        .unwrap()
    );

    let supplied = "cli-explicit-key";
    assert_eq!(
        reply,
        cli(
            tool,
            json!({
                "workItemId": item_id, "idempotency_key": supplied,
            })
        )
    );
    core.workspace.persist_registry_now().unwrap();
    assert!(
        core.workspace
            .ledger_holds(&ledger_operation_key(&operation_id("chat", supplied)))
    );
    assert_eq!(
        derived_idempotency_key("set", &["item", "completed", "7"]),
        "auto:6b6ddde7d337685e55be0f45a6c0446e"
    );

    server.abort();
    core.shutdown().await;
}
