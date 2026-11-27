use mast_proto::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn round_trip<T>(wire: Value) -> Value
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let decoded = serde_json::from_value::<T>(wire).unwrap();
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(
        serde_json::from_value::<T>(encoded.clone()).unwrap(),
        decoded
    );
    encoded
}

#[test]
fn task_and_model_fit_contracts_round_trip() {
    let task = round_trip::<WorkItemTaskSpec>(json!({
        "goal": "Add scoped wire types",
        "reqIds": ["REQ-1"],
        "contractRefs": ["schema"],
        "taskClass": {"role": "implementer", "domain": "proto_or_doc_schema", "complexity": "standard"},
        "ownedPaths": ["crates/proto/**"],
        "readOnlyPaths": ["docs/**"],
        "touchesHotPaths": true,
        "docRequirements": {"policy": {"kind": "exempt", "reason": "Consumer docs land separately"}},
        "acceptanceCommands": ["cargo nextest run -p mast-proto"],
        "requiredReport": true,
        "approvalTimeoutSecs": 600,
        "contextBudget": {"launchTokens": 6000},
        "itemBudget": {"maxTurns": 5},
        "experimentPinned": true
    }));
    assert_eq!(task["experimentPinned"], true);
    assert_eq!(task["docRequirements"]["policy"]["kind"], "exempt");
    let default_task: WorkItemTaskSpec = serde_json::from_value(json!({})).unwrap();
    assert!(!default_task.required_report);
    assert!(default_task.doc_requirements.policy.is_none());

    let fit = round_trip::<ModelFitPolicy>(json!({
        "spaceId": "space",
        "taskClass": task["taskClass"],
        "minTier": "medium",
        "blockedModels": [{"harness": "codex", "model": "example"}]
    }));
    assert_eq!(fit["minTier"], "medium");
    for role in ["worker", "lead", "manager", "verifier", "admin"] {
        assert_eq!(round_trip::<AgentRoleProfile>(json!(role)), role);
    }
}

#[test]
fn build_and_quota_contracts_round_trip_with_defaults() {
    let request = round_trip::<BuildJobEnqueueRequest>(json!({
        "argv": ["cargo", "nextest", "run"], "cwd": "/repo", "idempotencyKey": "build-1"
    }));
    assert_eq!(request["wake"], true);
    let result =
        json!({"jobId": "build-1", "state": "awaiting_approval", "approvalRequestId": "approval"});
    assert_eq!(round_trip::<BuildJobEnqueueResult>(result.clone()), result);
    let snapshot = round_trip::<BuildJobsSnapshot>(json!({
        "spaceId": "space",
        "jobs": [{"jobId": "build-1", "state": "timed_out", "queueWaitSecs": 5}]
    }));
    assert_eq!(snapshot["jobs"][0]["queueWaitSecs"], 5);

    let old = json!({"label": "Week", "usedFraction": 0.5, "resetsAt": "2026-11-27T00:00:00Z"});
    let scoped = round_trip::<ScopedUsageWindow>(old);
    assert_eq!(scoped["scope"], "accountWide");
    assert!(scoped.get("creditBudgetFraction").is_none());
    let old_reader = serde_json::from_value::<AgentUsageWindow>(scoped).unwrap();
    assert_eq!(old_reader.used_fraction, 0.5);

    let model = round_trip::<ScopedUsageWindow>(json!({
        "label": "Week", "usedFraction": 0.5, "scope": {"modelFamily": "Opus"},
        "creditBudgetFraction": 0.75
    }));
    assert_eq!(model["scope"]["modelFamily"], "Opus");
    assert_eq!(model["creditBudgetFraction"], 0.75);
    for mode in ["balancedFailover", "pinnedFreeze"] {
        assert_eq!(round_trip::<ProviderQuotaPolicy>(json!(mode)), mode);
    }
}

#[test]
fn living_doc_handoff_keeps_pending_work() {
    let ledger = round_trip::<DocLedgerSummary>(json!({
        "specArtifactId": "spec",
        "primaryPlanRev": 4,
        "pendingRows": [{
            "docId": "repo:docs/mcp.md",
            "class": "arch",
            "watchGlobs": ["crates/mcp/**"],
            "owedSinceCommit": "abc",
            "driftStatus": "owedUpdate",
            "pendingDeltas": [{"workItemId": "item", "summary": "Update the catalog table"}]
        }]
    }));
    assert_eq!(
        ledger["pendingRows"][0]["pendingDeltas"][0]["workItemId"],
        "item"
    );
    assert_eq!(ledger["pendingRows"][0]["driftStatus"], "owedUpdate");
}

#[test]
fn outline_snapshot_decodes_lead_and_skips_future_standing_roles() {
    let value = round_trip::<WorkOutlineSnapshot>(json!({
        "spaceId": "space",
        "standingRoles": [
            {"role": "future"},
            {"id": "lead:scope", "role": "lead", "scopeItemId": "scope",
             "chain": [], "fallbackChain": [], "state": "idle"}
        ]
    }));
    assert_eq!(value["standingRoles"].as_array().unwrap().len(), 1);
    assert_eq!(value["standingRoles"][0]["role"], "lead");
    assert!(serde_json::from_value::<StandingRoleKind>(json!("future")).is_err());
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
