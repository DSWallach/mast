use super::*;

fn spawn(id: &str, agent_type: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        call: ToolCall::Unknown {
            name: "Agent: inspect changes".into(),
            input: Some(serde_json::json!({"subagent_type":agent_type})),
        },
    }
}

fn child(spawn: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: spawn.into(),
        event: Box::new(event),
    }
}

#[test]
fn named_nested_agents_reconcile_and_filter_with_models_and_token_types() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("parent", "run", &request(), "Conversation");
    store.set_attribution("parent", "orchestrator".into(), None);
    store.observe("parent", &measurement("main", "model", "turn", 100, 10));
    store.observe("parent", &spawn("worker", "worker"));
    store.observe(
        "parent",
        &child("worker", measurement("child", "model", "turn", 20, 5)),
    );
    store.observe("parent", &child("worker", spawn("review", "pr-review")));
    store.observe(
        "parent",
        &child(
            "worker",
            child("review", measurement("review", "model", "turn", 10, 2)),
        ),
    );
    store.observe("parent", &done());
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(all.total, 147);
    assert_eq!(
        all.agent_types.iter().map(|a| a.tokens).sum::<u64>(),
        all.total
    );
    assert_eq!(
        all.details.len(),
        3,
        "different child types sharing a model remain distinct"
    );
    assert!(
        all.details
            .iter()
            .any(|r| r.agent_type == "pr-review" && r.tokens == 12)
    );
    assert_eq!(
        all.efficiency.completed, 1,
        "one parent turn, not three completed tasks"
    );
    assert_eq!(all.efficiency.completed_tokens, 147);
    let filtered = store
        .snapshot(&TokenUsageQuery {
            agent_type: Some("worker".into()),
            model: Some("model".into()),
            token_kind: TokenKind::CacheRead,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(filtered.total, 10);
    assert_eq!(filtered.details.len(), 1);
    assert_eq!(filtered.details[0].agent_type, "worker");
    assert_eq!(filtered.agent_types[0].tokens, filtered.total);
    assert!(filtered.available_agent_types.contains(&"pr-review".into()));
    let reloaded = TokenUsage::open(dir.path()).unwrap();
    assert_eq!(
        reloaded
            .snapshot(&TokenUsageQuery {
                agent_type: Some("pr-review".into()),
                ..Default::default()
            })
            .unwrap()
            .total,
        12
    );
    reloaded.delete_chat("parent").unwrap();
    assert_eq!(
        reloaded
            .0
            .lock()
            .unwrap()
            .db
            .query_row("SELECT COUNT(*) FROM usage_agents", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn combined_billing_and_observed_child_requests_are_separate() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("worker", "run", &request(), "Work-item automation");
    store.set_attribution("worker", "worker".into(), Some("coordinator".into()));
    store.observe("worker", &spawn("review", "pr-review"));
    let observation = child(
        "review",
        measurement("provider:child-request", "model", "request", 10, 2),
    );
    store.observe("worker", &observation);
    store.observe("worker", &observation);
    store.observe(
        "worker",
        &measurement("aggregate", "model", "turnIncludingChildren", 100, 20),
    );
    store.observe("worker", &done());
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(all.total, 120);
    assert_eq!(all.agent_types.len(), 1);
    assert_eq!(all.agent_types[0].agent_type, "worker");
    assert_eq!(all.agent_types[0].role, "main + subagents");
    assert_eq!(all.observed_agents[0].tokens, 12);
    assert_eq!(all.observed_agents[0].observations, 1);
    assert_eq!(all.observed_agents[0].agent_type, "pr-review");
    assert_eq!(
        all.details[0].context.parent_chat_id.as_deref(),
        Some("coordinator")
    );
    let review = store
        .snapshot(&TokenUsageQuery {
            agent_type: Some("pr-review".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        review.total, 0,
        "do not count child requests on top of the aggregate"
    );
    assert_eq!(review.observed_agents[0].tokens, 12);
    assert!(review.details.is_empty());
}

#[test]
fn repeated_reads_results_and_replayed_events_produce_stable_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("chat", "run", &request(), "Conversation");
    store.set_attribution("chat", "worker".into(), Some("parent".into()));
    for (id, output) in [
        ("a", "private-body"),
        ("b", "private-body"),
        ("c", "changed"),
    ] {
        let call = AgentEvent::ToolCall {
            id: id.into(),
            call: ToolCall::ReadFile {
                path: "/work/private-file".into(),
            },
        };
        store.observe("chat", &call);
        store.observe("chat", &call);
        store.observe(
            "chat",
            &AgentEvent::ToolResult {
                id: id.into(),
                is_error: id == "c",
                output: Some(output.into()),
                exit_code: None,
                diff: None,
            },
        );
    }
    store.observe("chat", &measurement("usage", "model", "turn", 30, 3));
    store.observe("chat", &done());
    let all = store.snapshot(&Default::default()).unwrap();
    let calls = all
        .insights
        .iter()
        .find(|d| d.category == "toolCall")
        .unwrap();
    assert_eq!((calls.occurrences, calls.repeated), (3, 2));
    assert_eq!(calls.agent_type, "worker");
    let results = all
        .insights
        .iter()
        .find(|d| d.category == "retainedToolResult")
        .unwrap();
    assert_eq!(
        (results.occurrences, results.repeated, results.errors),
        (3, 1, 1)
    );
    assert_eq!(results.repeated_bytes, "private-body".len() as u64);
    assert_eq!(results.example_chat_id, "chat");
    assert_eq!(all.total, 33);
    let encoded = serde_json::to_string(&all).unwrap();
    assert!(!encoded.contains("private-body"));
    assert!(!encoded.contains("/work/private-file"));
    let output = store
        .snapshot(&TokenUsageQuery {
            token_kind: TokenKind::Output,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(output.total, 3);
    assert_eq!(
        output
            .insights
            .iter()
            .find(|d| d.category == "retainedToolResult")
            .unwrap()
            .repeated_bytes,
        results.repeated_bytes
    );
    assert!(
        store
            .snapshot(&TokenUsageQuery {
                model: Some("other".into()),
                ..Default::default()
            })
            .unwrap()
            .insights
            .is_empty()
    );
}

#[test]
fn instructions_compare_deliveries_within_the_selection_and_do_not_add_envelope_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for (run, at) in [("first", 10), ("second", 20)] {
        store.begin("chat", run, &request(), "Conversation");
        for (category, bytes) in [("instruction", 100), ("instructionEnvelope", 250)] {
            let event = AgentEvent::UsageDiagnostic {
                diagnostic: UsageDiagnostic {
                    id: category.into(),
                    category: category.into(),
                    label: "Repository rule".into(),
                    revision: Some("revision-1".into()),
                    bytes,
                    stage: "submittedToHarness".into(),
                    method: "bytesOnly".into(),
                    ..Default::default()
                },
            };
            store.observe("chat", &event);
            store.observe("chat", &event);
        }
        store.observe("chat", &measurement("usage", "model", "turn", 10, 1));
        store.observe("chat", &done());
        store.flush().unwrap();
        store
            .0
            .lock()
            .unwrap()
            .db
            .execute(
                "UPDATE turns SET at=?2 WHERE id=?1",
                params![format!("{run}:0"), at],
            )
            .unwrap();
    }
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(all.insights.len(), 1);
    assert_eq!(all.insights[0].category, "instruction");
    assert_eq!(
        (
            all.insights[0].occurrences,
            all.insights[0].repeated,
            all.insights[0].repeated_bytes
        ),
        (2, 1, 100)
    );
    assert!(
        store
            .snapshot(&TokenUsageQuery {
                since: Some(20),
                ..Default::default()
            })
            .unwrap()
            .insights
            .is_empty()
    );
}

#[test]
fn cohorts_keep_work_items_separate_and_include_failed_attempt_cost() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for (run, item, tokens, failed) in [
        ("failure", "a", 50, true),
        ("success", "a", 100, false),
        ("different-task", "b", 10, false),
    ] {
        store.begin("chat", run, &request(), "PR review");
        store.set_context(
            "chat",
            Some("space".into()),
            Some(item.into()),
            Some(run.into()),
        );
        store.observe("chat", &measurement("usage", "model", "turn", tokens, 0));
        store.observe(
            "chat",
            &AgentEvent::Done {
                status: if failed {
                    DoneStatus::Errored
                } else {
                    DoneStatus::Completed
                },
                result: None,
                error: None,
                session_id: None,
            },
        );
    }
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(all.total, 160);
    assert_eq!(
        (
            all.efficiency.completed,
            all.efficiency.failed,
            all.efficiency.failed_tokens
        ),
        (2, 1, 50)
    );
    assert_eq!(all.efficiency.completed_tokens, 110);
    assert_eq!(all.configurations.len(), 2);
    let task = all
        .configurations
        .iter()
        .find(|c| c.work_item_id.as_deref() == Some("a"))
        .unwrap();
    assert_eq!(
        (task.tokens, task.completed, task.failed_tokens),
        (150, 1, 50)
    );
    assert_eq!(task.agent_type, "pr-review");
    assert_eq!(task.space_id.as_deref(), Some("space"));
}

#[test]
fn old_ledgers_and_missing_child_metadata_remain_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    {
        let inner = store.0.lock().unwrap();
        inner
            .db
            .execute(
                "INSERT INTO turns VALUES('old','chat','Conversation','config',0,'completed','{}')",
                [],
            )
            .unwrap();
        inner.db.execute("INSERT INTO measurements VALUES('old','request','model','main','turn',10,2,NULL,NULL,NULL)", []).unwrap();
    }
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(all.total, 12);
    assert_eq!(all.agent_types[0].agent_type, "Unknown agent type");
    store.begin("new", "new", &request(), "Work-item automation");
    store.set_attribution("new", "worker".into(), Some("parent".into()));
    store.observe(
        "new",
        &child("not-observed", measurement("native", "model", "turn", 5, 1)),
    );
    assert_eq!(
        store
            .snapshot(&TokenUsageQuery {
                agent_type: Some("Unknown agent type".into()),
                ..Default::default()
            })
            .unwrap()
            .total,
        18
    );
    assert_eq!(
        store
            .0
            .lock()
            .unwrap()
            .db
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
}

#[test]
fn promotion_changes_main_but_preserves_explicit_review_and_worker_roles() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for kind in ["main", "pr-review", "worker"] {
        store.begin(kind, kind, &request(), "Conversation");
        store.set_attribution(kind, kind.into(), None);
        store.observe(kind, &measurement("usage", "model", "turn", 10, 1));
        store.promote_orchestrator(kind);
        store.observe(kind, &done());
    }
    let all = store.snapshot(&Default::default()).unwrap();
    assert!(!all.available_agent_types.contains(&"main".into()));
    assert!(all.available_agent_types.contains(&"orchestrator".into()));
    assert!(all.available_agent_types.contains(&"pr-review".into()));
    assert!(all.available_agent_types.contains(&"worker".into()));
}

#[test]
fn agent_metadata_is_explicit_and_does_not_guess_from_descriptions_or_commands() {
    use super::super::attribution::spawn_agent_type;
    for key in ["agent_type", "agentType", "subagent_type"] {
        let call = ToolCall::Unknown {
            name: "Agent".into(),
            input: Some(serde_json::json!({key:"explorer"})),
        };
        assert_eq!(spawn_agent_type(&call), Some("explorer"));
    }
    assert!(
        spawn_agent_type(&ToolCall::Exec {
            command: "claude --agent pr-review".into()
        })
        .is_none()
    );
    assert!(
        spawn_agent_type(&ToolCall::Unknown {
            name: "Agent: pr-review".into(),
            input: None
        })
        .is_none()
    );
    assert!(
        spawn_agent_type(&ToolCall::Unknown {
            name: "ordinary-tool".into(),
            input: Some(serde_json::json!({"agent_type":"worker"}))
        })
        .is_none()
    );
}

#[test]
fn early_instruction_metadata_is_relabelled_without_promoting_child_main_agents() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for run in ["one", "two"] {
        store.begin("chat", run, &request(), "Conversation");
        store.observe(
            "chat",
            &AgentEvent::UsageDiagnostic {
                diagnostic: UsageDiagnostic {
                    id: "envelope".into(),
                    category: "instructionEnvelope".into(),
                    label: "Instructions".into(),
                    revision: Some("v1".into()),
                    bytes: 100,
                    stage: "submittedToHarness".into(),
                    method: "bytesOnly".into(),
                    ..Default::default()
                },
            },
        );
        store.set_attribution("chat", "worker".into(), Some("parent".into()));
        store.observe("chat", &measurement("root", "model", "turn", 10, 1));
        store.observe("chat", &done());
    }
    let worker = store
        .snapshot(&TokenUsageQuery {
            agent_type: Some("worker".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(worker.insights[0].agent_type, "worker");
    assert_eq!(worker.insights[0].repeated_bytes, 100);
    store.begin("coordinator", "orchestration", &request(), "Conversation");
    store.observe("coordinator", &spawn("child", "main"));
    store.observe(
        "coordinator",
        &child("child", measurement("child", "model", "turn", 20, 2)),
    );
    store.observe(
        "coordinator",
        &measurement("parent", "model", "turn", 30, 3),
    );
    store.promote_orchestrator("coordinator");
    let all = store.snapshot(&Default::default()).unwrap();
    assert_eq!(
        all.agent_types
            .iter()
            .find(|a| a.agent_type == "main")
            .unwrap()
            .tokens,
        22
    );
    assert_eq!(
        all.agent_types
            .iter()
            .find(|a| a.agent_type == "orchestrator")
            .unwrap()
            .tokens,
        33
    );
}

#[test]
fn observed_child_subtypes_do_not_present_missing_usage_as_measured_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("chat", "run", &request(), "Conversation");
    store.observe("chat", &spawn("child", "pr-review"));
    store.observe(
        "chat",
        &child(
            "child",
            AgentEvent::UsageMeasurement {
                measurement: UsageMeasurement {
                    id: "child-request".into(),
                    model: Some("model".into()),
                    scope: "request".into(),
                    counts: TokenCounts {
                        input: 10,
                        output: 1,
                        ..Default::default()
                    },
                },
            },
        ),
    );
    let all = store
        .snapshot(&TokenUsageQuery {
            token_kind: TokenKind::Reasoning,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.observed_agents[0].tokens, 0);
    assert_eq!(all.observed_agents[0].unknown_breakdown, 1);
    assert_eq!(all.observed_agents[0].observations, 1);
}

#[test]
fn agent_type_is_independent_of_scheduled_review_activity() {
    assert_eq!(
        launch_agent_type("Scheduled task", false, false, true),
        "pr-review"
    );
    assert_eq!(
        launch_agent_type("Work-item automation", true, true, false),
        "worker"
    );
    assert_eq!(
        launch_agent_type("Title generation", false, false, false),
        "title-generator"
    );
}

#[test]
fn original_results_and_retained_copies_are_not_two_repetition_findings() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("chat", "run", &request(), "Conversation");
    for id in ["one", "two"] {
        store.observe(
            "chat",
            &AgentEvent::ToolCall {
                id: id.into(),
                call: ToolCall::Exec {
                    command: "cargo test".into(),
                },
            },
        );
        store.observe(
            "chat",
            &AgentEvent::UsageDiagnostic {
                diagnostic: UsageDiagnostic {
                    id: id.into(),
                    category: "toolResult".into(),
                    label: "raw result".into(),
                    channel: Some("cli".into()),
                    signature: Some("harness:same-result".into()),
                    bytes: 0,
                    stage: "harnessResponse".into(),
                    method: "bytesOnly".into(),
                    ..Default::default()
                },
            },
        );
        store.observe(
            "chat",
            &AgentEvent::ToolResult {
                id: id.into(),
                is_error: false,
                exit_code: Some(0),
                output: Some(String::new()),
                diff: None,
            },
        );
    }
    store.observe("chat", &measurement("usage", "model", "turn", 10, 1));
    let all = store.snapshot(&Default::default()).unwrap();
    let result = all
        .insights
        .iter()
        .find(|i| i.category == "toolResult")
        .unwrap();
    assert_eq!(
        (result.occurrences, result.repeated, result.empty_results),
        (2, 1, 2)
    );
    assert!(
        !all.insights
            .iter()
            .any(|i| i.category == "retainedToolResult")
    );
    assert_eq!(all.total, 11);
}
