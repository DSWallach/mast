//! Import transcripts with small and larger synthetic histories.

use std::sync::Arc;

use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};

async fn import_history(message_count: usize) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let history: String = (0..message_count)
        .map(|index| {
            format!(
                "{}\n",
                serde_json::json!({
                    "parentUuid": null, "isSidechain": false, "type": "user",
                    "message": {"role": "user", "content": format!("message {index}")},
                    "uuid": format!("user-{index}"), "timestamp": "2026-01-01T00:00:00.000Z",
                    "cwd": "/work/project", "sessionId": "synthetic-session"
                })
            )
        })
        .collect();
    std::fs::write(&path, history).expect("write synthetic transcript");
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir.path(), "dev-org", "dev-user"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .expect("assemble profile");
    let result = core
        .external_import
        .import("smoke-chat", "synthetic-session", &path)
        .expect("import transcript");
    assert!(result.message_count > 0, "expected imported messages");
    assert!(
        core.workspace
            .chat("smoke-chat")
            .expect("read chat")
            .is_some()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn imports_a_small_synthetic_session() {
    import_history(2).await;
}

#[tokio::test]
async fn imports_a_large_synthetic_session() {
    import_history(200).await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
