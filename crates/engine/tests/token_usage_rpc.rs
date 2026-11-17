use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use mast_proto::{AgentEvent, RunRequest, SandboxLevel, token_usage::*};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn token_usage_queries_filter_and_never_fall_back_to_local_for_an_unknown_device() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "test-org", "test-user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace.create_chat("fixture",None,Some(&core.device_id),None,Some(dir.path().display().to_string())).unwrap();
    let usage = core
        .sessions
        .token_usage()
        .expect("ledger wired at assembly");
    let request = RunRequest {
        prompt: "fixture".into(),
        harness: Some(HarnessId::Mock),
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: dir.path().display().to_string(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        approval_rules: false,
        scheduled_task: None,
        resume: None,
        fork_from: None,
        attachments: vec![],
        worktree: None,
        instructions: None,
    };
    usage.begin("fixture", "run", &request, "Conversation");
    usage.observe(
        "fixture",
        &AgentEvent::UsageMeasurement {
            measurement: UsageMeasurement {
                id: "request".into(),
                model: Some("model-a".into()),
                scope: "turn".into(),
                counts: TokenCounts {
                    input: 100,
                    output: 20,
                    cache_read: Some(60),
                    cache_write: Some(0),
                    reasoning: Some(5),
                },
            },
        },
    );
    let client = mast_rpc::memory_client(core.rpc_service());
    let value = client
        .call(
            mast_rpc::methods::TOKEN_USAGE,
            serde_json::json!({"model":"model-a","tokenKind":"cacheRead"}),
        )
        .await
        .unwrap();
    let snapshot: TokenUsageSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(snapshot.total, 60);
    assert_eq!(snapshot.details[0].chat_id, "fixture");
    let absent = client
        .call(
            mast_rpc::methods::TOKEN_USAGE,
            serde_json::json!({"model":"model-b"}),
        )
        .await
        .unwrap();
    assert_eq!(absent["total"], 0);
    assert!(
        client
            .call(
                mast_rpc::methods::TOKEN_USAGE,
                serde_json::json!({"targetDeviceId":"missing-device"})
            )
            .await
            .is_err()
    );
    core.shutdown().await;
}
