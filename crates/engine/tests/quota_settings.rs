//! `GetQuotaSettings` / `SetQuotaSettings` through the engine's RPC surface.

use mast_engine::profile::EngineProfile;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::HarnessId;
use mast_rpc::methods;
use serde_json::{Value, json};
use std::sync::Arc;

fn assemble(profile: EngineProfile) -> EngineCore {
    EngineCore::assemble_with_profile(
        profile,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap()
}

fn assert_settings(value: &Value, policy: &str, caps: [f64; 3]) {
    assert_eq!(value["policy"], policy, "{value}");
    for (key, expected) in ["softCap", "hardCap", "freezeCap"].into_iter().zip(caps) {
        let actual = value[key].as_f64().unwrap();
        assert!((actual - expected).abs() < 1e-6, "{key} in {value}");
    }
}

#[tokio::test]
async fn quota_settings_rpc_defaults_saves_and_rejects_bad_caps() {
    let dir = tempfile::tempdir().unwrap();
    let profile = EngineProfile::local(dir.path()).unwrap();
    let core = assemble(profile.clone());
    let client = mast_rpc::memory_client(core.rpc_service());

    let defaults = client
        .call(methods::GET_QUOTA_SETTINGS, json!({}))
        .await
        .unwrap();
    assert_settings(&defaults, "balancedFailover", [0.78, 0.85, 0.90]);

    let saved = client
        .call(
            methods::SET_QUOTA_SETTINGS,
            json!({"policy":"pinnedFreeze","softCap":0.6,"hardCap":0.7,"freezeCap":0.8,"later":true,
                "targetDeviceId":core.workspace.device_id()}),
        )
        .await
        .unwrap();
    assert_settings(&saved, "pinnedFreeze", [0.6, 0.7, 0.8]);

    for bad in [
        // Out of order, then past 100%.
        json!({"policy":"balancedFailover","softCap":0.9,"hardCap":0.7,"freezeCap":0.8}),
        json!({"policy":"balancedFailover","softCap":0.6,"hardCap":0.7,"freezeCap":1.2}),
        // A partial record must not reset the cap mode.
        json!({"softCap":0.6,"hardCap":0.7,"freezeCap":0.8}),
        json!({}),
    ] {
        let error = client
            .call(methods::SET_QUOTA_SETTINGS, bad.clone())
            .await
            .expect_err(&bad.to_string());
        assert!(error.to_string().contains("bad params"), "{error}");
    }

    let kept = client
        .call(methods::GET_QUOTA_SETTINGS, json!({}))
        .await
        .unwrap();
    assert_settings(&kept, "pinnedFreeze", [0.6, 0.7, 0.8]);

    // Stored in the profile store, where the next broker owner will load it.
    let loaded = mast_engine::quota_settings::load(profile.store_root());
    assert_eq!(loaded.hard_cap, 0.7);
    assert_eq!(loaded.policy, mast_proto::ProviderQuotaPolicy::PinnedFreeze);
}
