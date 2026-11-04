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
        },
        fingerprint: String::new(),
        occurrences: 0,
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
            json!({"autoCapture": false}),
        )
        .await
        .unwrap();
    assert!(!core.sessions.auto_capture_friction());
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
}
