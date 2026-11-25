use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{
    FrictionKind, FrictionProvenance, FrictionReport, FrictionSource, FrictionStatus, HarnessId,
};
use mast_rpc::methods;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn friction_rpc_list_dismiss_promote_and_propose() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":"space","deviceId":core.workspace.device_id(),"path":"/tmp"})).await.unwrap();
    let parent = client.call(methods::WORK_OUTLINE_MUTATE, json!({"op":"createWorkItem","spaceId":"space","title":"Parent","operationId":"parent"})).await.unwrap()["workItemId"].as_str().unwrap().to_owned();
    let report = FrictionReport {
        space_name: None,
        id: String::new(),
        kind: FrictionKind::Bug,
        summary: "Failure".into(),
        what_happened: "Details".into(),
        workaround: None,
        suggestion: Some("Use a setting".into()),
        approval: None,
        severity: None,
        links: vec![],
        provenance: FrictionProvenance {
            chat_id: "chat".into(),
            harness: "codex".into(),
            model: Some("model".into()),
            space_id: "space".into(),
            work_item_id: None,
            branch: None,
            created_at: 100,
            source: FrictionSource::Agent,
            file_name: None,
        },
        fingerprint: String::new(),
        occurrences: 0,
        occurrence_history: Vec::new(),
        last_seen: 0,
        status: FrictionStatus::Open,
        promoted_item_id: None,
        promoted_at: None,
    };
    let row = core.workspace.record_friction(report.clone()).unwrap();
    core.workspace.record_friction(report).unwrap();
    let list = client
        .call(
            methods::LIST_FRICTION_REPORTS,
            json!({"spaceId":"space","harness":"codex","model":"model","since":100}),
        )
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["occurrences"], 2);
    assert!(
        client
            .call(
                methods::LIST_FRICTION_REPORTS,
                json!({"spaceId":"space","harness":"other"})
            )
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    client
        .call(
            methods::SET_FRICTION_STATUS,
            json!({"spaceId":"space","fingerprint":row.fingerprint,"status":"dismissed"}),
        )
        .await
        .unwrap();
    assert!(
        client
            .call(methods::LIST_FRICTION_REPORTS, json!({"spaceId":"space"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let proposal = client
        .call(
            methods::PROPOSE_FRICTION_INSTRUCTION,
            json!({"spaceId":"space","fingerprint":row.fingerprint}),
        )
        .await
        .unwrap();
    let proposal_id = proposal["proposalId"].as_str().unwrap();
    let proposal_row = core
        .workspace
        .registry_row(
            mast_doc::work_outline::KIND_WORK_ITEM_INSTRUCTION_PROPOSALS,
            proposal_id,
        )
        .unwrap();
    assert!(!proposal_row.fields.contains_key("review"));
    assert_eq!(
        proposal_row.fields.get("text"),
        Some(&json!("Use a setting"))
    );
    let item = client
        .call(
            methods::PROMOTE_FRICTION_TO_ITEM,
            json!({"spaceId":"space","fingerprint":row.fingerprint,"parentItemId":parent}),
        )
        .await
        .unwrap();
    let item_id = item["workItemId"].as_str().unwrap();
    let item_row = core
        .workspace
        .registry_row(mast_doc::work_outline::KIND_WORK_ITEMS, item_id)
        .unwrap();
    assert_eq!(
        item_row.fields.get("parentWorkItemId"),
        Some(&json!(parent))
    );
    let stored = core.workspace.friction_reports("space").pop().unwrap();
    assert_eq!(stored.promoted_item_id.as_deref(), Some(item_id));
    assert_eq!(stored.status, FrictionStatus::Promoted);
    assert!(stored.promoted_at.is_some());
}

#[tokio::test]
async fn friction_settings_persist_across_engine_restart() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":"space","deviceId":core.workspace.device_id(),"path":"/tmp/space"})).await.unwrap();
    client.call(methods::SET_FRICTION_READ_GRANT, json!({"spaceId":"space", "enabled":true})).await.unwrap();
    assert_eq!(
        client
            .call(methods::GET_FRICTION_SETTINGS, json!({}))
            .await
            .unwrap()["autoCapture"],
        true
    );
    client
        .call(
            methods::SET_FRICTION_SETTINGS,
            json!({"autoCapture": false, "allSpacesReadGrants": []}),
        )
        .await
        .unwrap();
    assert!(!core.sessions.auto_capture_friction());
    assert!(core.registry.friction_settings().all_spaces_read_grants.contains("space"));
    drop(client);
    drop(core);
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    assert_eq!(
        client
            .call(methods::GET_FRICTION_SETTINGS, json!({}))
            .await
            .unwrap()["autoCapture"],
        false
    );
    assert!(!core.sessions.auto_capture_friction());
    assert!(core.registry.friction_settings().all_spaces_read_grants.contains("space"));
}

#[tokio::test]
async fn friction_import_deduplicates_and_redacts() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":"space","deviceId":core.workspace.device_id(),"path":"/tmp"})).await.unwrap();
    let existing = FrictionReport {
        space_name: None,
        id: String::new(),
        kind: FrictionKind::Workflow,
        summary: "Build approval".into(),
        what_happened: "Existing report".into(),
        workaround: None,
        suggestion: None,
        approval: Some(mast_proto::FrictionApproval {
            command: "different approval".into(),
            why: "Network access".into(),
            answer: None,
            exact: false,
        }),
        severity: None,
        links: vec![],
        provenance: FrictionProvenance {
            chat_id: "chat".into(),
            harness: "codex".into(),
            model: None,
            space_id: "space".into(),
            work_item_id: None,
            branch: None,
            created_at: 1,
            source: FrictionSource::Agent,
            file_name: None,
        },
        fingerprint: String::new(),
        occurrences: 0,
        occurrence_history: Vec::new(),
        last_seen: 0,
        status: FrictionStatus::Open,
        promoted_item_id: None,
        promoted_at: None,
    };
    core.workspace.record_friction(existing).unwrap();
    let markdown = include_str!("fixtures/friction-log.md");
    let params = json!({"spaceId":"space", "fileName":"friction.md", "markdown":markdown});
    let first = client
        .call(methods::IMPORT_FRICTION, params.clone())
        .await
        .unwrap();
    assert_eq!(first["added"], 1);
    assert_eq!(first["merged"], 1);
    assert_eq!(first["skipped"].as_array().unwrap().len(), 2);
    drop(client);
    drop(core);
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    let second = client.call(methods::IMPORT_FRICTION, params).await.unwrap();
    assert_eq!(second["added"], 0);
    assert_eq!(second["merged"], 0);
    assert_eq!(second["skipped"].as_array().unwrap().len(), 4);
    let reports = core.workspace.friction_reports("space");
    assert_eq!(reports.len(), 2);
    assert_eq!(reports.iter().map(|r| r.occurrences).sum::<u64>(), 3);
    assert!(
        reports
            .iter()
            .any(|r| r.provenance.source == FrictionSource::Import)
    );
    assert!(
        reports
            .iter()
            .any(|r| r.provenance.file_name.as_deref() == Some("friction.md"))
    );
    assert!(!format!("{reports:?}").contains("synthetic-secret"));
}

#[tokio::test]
async fn approval_ledger_rpc_filters_by_text_and_outcome_and_reports_the_total() {
    use mast_engine::workspace_host::ApprovalLedgerInput;
    use mast_proto::{LedgerDecider, LedgerDecision, LedgerSource};
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(HarnessRegistry::new()), HarnessId::Mock, None).unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    client.call(methods::MUTATE, json!({"op":"createSpace","spaceId":"space","deviceId":core.workspace.device_id(),"path":"/tmp"})).await.unwrap();
    client.call(methods::MUTATE, json!({"op":"createChat","chatId":"chat","spaceId":"space","deviceId":core.workspace.device_id()})).await.unwrap();
    let record = |request_id: &str, command: &str, decided: Option<(LedgerDecision, LedgerDecider, Option<String>)>| {
        assert!(core.workspace.record_approval_ledger(&[ApprovalLedgerInput {
            chat_id: "chat", request_id, command, cwd: None, harness: "codex".into(), source: LedgerSource::Prompt, decided,
        }]).unwrap());
    };
    record("r1", "cargo nextest run", Some((LedgerDecision::Allowed, LedgerDecider::SpaceRule, Some("rule-1".into()))));
    record("r2", "cargo nextest run", None);
    record("r3", "pgrep cargo", Some((LedgerDecision::Denied, LedgerDecider::User, None)));
    let list = |params: serde_json::Value| client.call(methods::LIST_APPROVAL_LEDGER, params);
    let all = list(json!({"spaceId":"space"})).await.unwrap();
    assert_eq!(all["total"], 3);
    assert_eq!(all["entries"].as_array().unwrap().len(), 3, "two rows for one command, as asked twice");
    let nextest = list(json!({"spaceId":"space","query":"NEXTEST"})).await.unwrap();
    assert_eq!(nextest["total"], 2);
    let rule = list(json!({"spaceId":"space","query":"rule-1"})).await.unwrap();
    assert_eq!(rule["total"], 1);
    let pending = list(json!({"spaceId":"space","decision":"pending"})).await.unwrap();
    assert_eq!(pending["entries"][0]["command"], "cargo nextest run");
    assert_eq!(pending["total"], 1);
    let limited = list(json!({"spaceId":"space","limit":1})).await.unwrap();
    assert_eq!((limited["entries"].as_array().unwrap().len(), limited["total"].clone()), (1, json!(3)));
    let next = list(json!({"spaceId":"space","limit":1,"offset":1})).await.unwrap();
    assert_ne!(limited["entries"][0]["id"], next["entries"][0]["id"]);
    let outcome_text = list(json!({"spaceId":"space","query":"allowed Space rule","limit":1})).await.unwrap();
    assert_eq!(outcome_text["entries"][0]["requestId"], "r1");
    assert_eq!(list(json!({"spaceId":"other"})).await.unwrap()["total"], 0);

    let rule = "tool --token synthetic-secret";
    record("redacted-rule", rule, Some((LedgerDecision::Allowed, LedgerDecider::UserRule, Some(rule.into()))));
    record("redacted-finalize", rule, None);
    record("redacted-finalize", rule, Some((LedgerDecision::Allowed, LedgerDecider::UserRule, Some(rule.into()))));
    let rows = list(json!({"spaceId":"space","query":"tool --token"})).await.unwrap();
    assert_eq!(rows["total"], 2);
    assert!(!rows.to_string().contains("synthetic-secret"));
    assert!(rows["entries"].as_array().unwrap().iter().all(|row| row["redacted"] == true));
}
