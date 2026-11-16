use super::*;
use mast_proto::{HarnessId, SandboxLevel};

fn request() -> RunRequest {
    RunRequest {
        prompt: "test".into(),
        harness: Some(HarnessId::ClaudeCode),
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/test".into(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        approval_rules: false,
        scheduled_task: None,
        resume: None,
        fork_from: None,
        attachments: Vec::new(),
        worktree: None,
        instructions: None,
    }
}
fn measurement(id: &str, model: &str, scope: &str, input: u64, output: u64) -> AgentEvent {
    AgentEvent::UsageMeasurement {
        measurement: UsageMeasurement {
            id: id.into(),
            model: Some(model.into()),
            scope: scope.into(),
            counts: TokenCounts {
                input,
                output,
                cache_read: Some(input / 2),
                cache_write: Some(0),
                reasoning: Some(output / 2),
            },
        },
    }
}
fn done() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: None,
    }
}

#[test]
fn aggregates_replace_requests_and_legacy_without_counting_children_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("chat", "run", &request(), "Conversation");
    store.observe("chat", &measurement("request", "large", "request", 10, 5));
    store.observe(
        "chat",
        &AgentEvent::Subagent {
            parent_tool_use_id: "child".into(),
            event: Box::new(measurement("child-request", "small", "request", 20, 10)),
        },
    );
    store.observe(
        "chat",
        &measurement("result:large", "large", "turnIncludingChildren", 100, 50),
    );
    store.observe(
        "chat",
        &measurement("result:small", "small", "turnIncludingChildren", 200, 100),
    );
    store.observe(
        "chat",
        &AgentEvent::Usage {
            input_tokens: 999,
            output_tokens: 999,
        },
    );
    store.observe("chat", &done());
    let all = store.snapshot(&TokenUsageQuery::default()).unwrap();
    assert_eq!(all.total, 450);
    assert_eq!(all.runs, 1);
    assert_eq!(all.roles[0].label, "main + subagents");
    let q = TokenUsageQuery {
        model: Some("small".into()),
        token_kind: TokenKind::CacheRead,
        ..Default::default()
    };
    let filtered = store.snapshot(&q).unwrap();
    assert_eq!(filtered.total, 100);
    assert_eq!(filtered.details.len(), 1);
    let reloaded = TokenUsage::open(dir.path()).unwrap();
    assert_eq!(
        reloaded
            .snapshot(&TokenUsageQuery::default())
            .unwrap()
            .total,
        450
    );
    reloaded.delete_chat("chat").unwrap();
    assert_eq!(
        reloaded
            .snapshot(&TokenUsageQuery::default())
            .unwrap()
            .total,
        0
    );
}

#[test]
fn snapshots_dedupe_and_unknown_subtypes_are_not_reported_as_measured_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r", &request(), "Scheduled task");
    for amount in [10, 20, 20, 15] {
        store.observe("a", &measurement("snapshot", "model", "turn", amount, 4));
    }
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 24);
    store.observe("a", &done());
    store.observe(
        "a",
        &AgentEvent::Usage {
            input_tokens: 30,
            output_tokens: 5,
        },
    );
    let result = store
        .snapshot(&TokenUsageQuery {
            token_kind: TokenKind::Reasoning,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.total, 2);
    assert_eq!(result.unknown_breakdown, 1);
}

#[test]
fn tool_diagnostics_keep_stages_separate_and_do_not_charge_usage_to_both_channels() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r", &request(), "Conversation");
    for (id, call) in [
        (
            "m",
            ToolCall::Mcp {
                server: "files".into(),
                tool: "read".into(),
                input: None,
            },
        ),
        (
            "c",
            ToolCall::Exec {
                command: "cat /private/input.txt".into(),
            },
        ),
    ] {
        store.observe(
            "a",
            &AgentEvent::ToolCall {
                id: id.into(),
                call,
            },
        );
        let d = AgentEvent::UsageDiagnostic {
            diagnostic: UsageDiagnostic {
                id: id.into(),
                category: "toolResult".into(),
                label: "result".into(),
                bytes: 1000,
                stage: "harnessResponse".into(),
                method: "bytesOnly".into(),
                ..Default::default()
            },
        };
        store.observe("a", &d);
        store.observe("a", &d);
        store.observe(
            "a",
            &AgentEvent::ToolResult {
                id: id.into(),
                is_error: false,
                exit_code: None,
                output: Some("retained".into()),
                diff: None,
            },
        );
    }
    store.observe("a", &measurement("final", "model", "turn", 100, 50));
    let result = store.snapshot(&Default::default()).unwrap();
    assert_eq!(result.total, 150);
    let returned: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == "toolResult")
        .collect();
    assert_eq!(returned.len(), 2);
    assert_eq!(returned[0].observations, 1);
    assert_eq!(returned[0].bytes, 1000);
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("/private/input"));
}

#[test]
fn time_ranges_pagination_and_title_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let key = store.begin_title("owner", &request());
    store.observe(&key, &measurement("final", "small", "turn", 2, 1));
    store.observe(&key, &done());
    store.finish_title(&key);
    let result = store.snapshot(&Default::default()).unwrap();
    assert_eq!(result.details[0].chat_id, "owner");
    assert_eq!(result.activities[0].label, "Title generation");
    let at = result.details[0].at;
    assert_eq!(
        store
            .snapshot(&TokenUsageQuery {
                until: Some(at),
                ..Default::default()
            })
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        store
            .snapshot(&TokenUsageQuery {
                since: Some(at),
                ..Default::default()
            })
            .unwrap()
            .total,
        3
    );
    assert!(
        store
            .snapshot(&TokenUsageQuery {
                offset: 1,
                ..Default::default()
            })
            .unwrap()
            .details
            .is_empty()
    );
}

#[test]
fn repeated_rule_observations_are_deduped_per_turn_but_not_across_turns() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r", &request(), "Conversation");
    let diagnostic = AgentEvent::UsageDiagnostic {
        diagnostic: UsageDiagnostic {
            id: "rule".into(),
            category: "instruction".into(),
            label: "rule".into(),
            revision: Some("v1".into()),
            bytes: 10,
            stage: "submittedToHarness".into(),
            method: "bytesOnly".into(),
            ..Default::default()
        },
    };
    store.observe("a", &diagnostic);
    store.observe("a", &diagnostic);
    store.observe("a", &done());
    store.observe("a", &diagnostic);
    let result = store.snapshot(&Default::default()).unwrap();
    let row = result
        .diagnostics
        .iter()
        .find(|d| d.category == "instruction")
        .unwrap();
    assert_eq!(row.bytes, 20);
    assert_eq!(row.observations, 2);
}

#[test]
fn provider_ids_dedupe_late_delivery_across_turns_and_engine_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r1", &request(), "Conversation");
    let event = measurement("provider:request-1", "model", "turn", 10, 2);
    store.observe("a", &event);
    store.observe("a", &done());
    store.observe("a", &event);
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 12);
    drop(store);
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r2", &request(), "Conversation");
    store.observe("a", &event);
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 12);
}

#[test]
fn rule_inventory_is_not_injection_and_context_is_not_billed_usage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "secret rule one").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "secret rule two").unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let mut request = request();
    request.cwd = dir.path().display().to_string();
    store.begin("a", "r", &request, "Conversation");
    store.observe(
        "a",
        &AgentEvent::ContextUsage {
            tokens: Some(10000),
            window: Some(200000),
        },
    );
    let result = store.snapshot(&Default::default()).unwrap();
    assert_eq!(result.total, 0);
    assert_eq!(result.context_samples[0].tokens, Some(10000));
    let rules: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == "instructionFile")
        .collect();
    assert_eq!(rules.len(), 2);
    assert!(rules.iter().all(|r| r.stage == "availableOnDisk"));
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("secret rule")
    );
}

#[test]
fn large_history_aggregates_in_sql_and_bounds_detail_payload() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    {
        let mut inner = store.0.lock().unwrap();
        let tx = inner.db.transaction().unwrap();
        for index in 0..20000 {
            let id = format!("turn-{index}");
            tx.execute(
                "INSERT INTO turns VALUES(?1,'chat','Conversation','config',?2,'completed','{}')",
                params![id, index],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO measurements VALUES(?1,?2,'model','main','turn',100,20,40,0,5)",
                params![id, format!("provider:request-{index}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let started = std::time::Instant::now();
    let result = store.snapshot(&Default::default()).unwrap();
    eprintln!("20,000-turn usage query: {:?}", started.elapsed());
    assert_eq!(result.total, 2400000);
    assert_eq!(result.runs, 20000);
    assert_eq!(result.details.len(), 50);
    assert!(result.has_more);
    assert_eq!(result.configurations.len(), 1);
}
