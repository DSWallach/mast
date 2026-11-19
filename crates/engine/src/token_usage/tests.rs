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
        scheduled_notion_writes: None,
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
    assert_eq!(
        store
            .snapshot(&TokenUsageQuery {
                token_kind: TokenKind::CacheRead,
                ..Default::default()
            })
            .unwrap()
            .total,
        10
    );
    store.observe("a", &done());
    store.activate("a");
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
    store.activate("a");
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

#[test]
fn activity_uses_explicit_origins_and_scheduling_has_precedence() {
    let mut request = request();
    let manual = mast_proto::ChatPrLink {
        url: "https://example.invalid/pr/1".into(),
        source: mast_proto::ChatLinkSource::Manual,
    };
    assert_eq!(
        classify_activity(&request, &[manual], false),
        "Conversation"
    );
    let review = mast_proto::ChatPrLink {
        url: "https://example.invalid/pr/1".into(),
        source: mast_proto::ChatLinkSource::ReviewLauncher,
    };
    assert_eq!(
        classify_activity(&request, std::slice::from_ref(&review), true),
        "PR review"
    );
    request.scheduled_task = Some("task".into());
    assert_eq!(
        classify_activity(&request, &[review], true),
        "Scheduled task"
    );
    request.scheduled_task = None;
    assert_eq!(
        classify_activity(&request, &[], true),
        "Work-item automation"
    );
    request.prompt = "[Mast system message: auto-continue] Continue".into();
    assert_eq!(
        classify_activity(&request, &[], false),
        "Work-item automation"
    );
}

#[test]
fn settled_late_events_do_not_create_phantom_turns_and_guards_release_capture() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r", &request(), "Conversation");
    let guard = store.guard("a", "r");
    store.observe("a", &measurement("one", "model", "turn", 10, 1));
    store.observe("a", &done());
    store.observe(
        "a",
        &AgentEvent::ContextUsage {
            tokens: Some(100),
            window: Some(1000),
        },
    );
    store.flush().unwrap();
    assert_eq!(
        store
            .0
            .lock()
            .unwrap()
            .db
            .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    drop(guard);
    store.observe("a", &measurement("two", "model", "turn", 20, 1));
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 11);
    store.begin("a", "new", &request(), "Conversation");
    let stale = store.guard("a", "old");
    drop(stale);
    store.flush().unwrap();
    assert!(store.0.lock().unwrap().active.contains_key("a"));
}

#[test]
fn inventory_keeps_one_size_per_revision_and_skips_title_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "rule contents").unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let mut request = request();
    request.cwd = dir.path().display().to_string();
    for run in ["one", "two", "three"] {
        store.begin("a", run, &request, "Conversation");
    }
    let snapshot = store.snapshot(&Default::default()).unwrap();
    let inventory = snapshot
        .diagnostics
        .iter()
        .find(|d| d.stage == "availableOnDisk")
        .unwrap();
    assert_eq!(inventory.bytes, 13);
    assert_eq!(inventory.observations, 3);
    let title = store.begin_title("a", &request);
    let snapshot = store
        .snapshot(&TokenUsageQuery {
            activity: Some("Title generation".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        !snapshot
            .diagnostics
            .iter()
            .any(|d| d.stage == "availableOnDisk")
    );
    store.finish_title(&title);
}

#[test]
fn diagnostics_limits_do_not_hide_call_counts_or_rules_behind_large_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "r", &request(), "Conversation");
    for index in 0..210 {
        store.observe(
            "a",
            &AgentEvent::UsageDiagnostic {
                diagnostic: UsageDiagnostic {
                    id: index.to_string(),
                    label: format!("tool-{index}"),
                    category: "toolResult".into(),
                    stage: "harnessResponse".into(),
                    method: "bytesOnly".into(),
                    bytes: 1000,
                    ..Default::default()
                },
            },
        );
    }
    store.observe(
        "a",
        &AgentEvent::UsageDiagnostic {
            diagnostic: UsageDiagnostic {
                id: "rule".into(),
                label: "small rule".into(),
                category: "instruction".into(),
                stage: "submittedToHarness".into(),
                bytes: 1,
                ..Default::default()
            },
        },
    );
    store.observe(
        "a",
        &AgentEvent::ToolCall {
            id: "call".into(),
            call: ToolCall::Exec {
                command: "rg thing".into(),
            },
        },
    );
    let result = store.snapshot(&Default::default()).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.category == "instruction")
    );
    assert!(result.diagnostics.iter().any(|d| d.category == "toolCall"));
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|d| d.category == "toolResult")
            .count(),
        200
    );
}

#[test]
fn resume_baselines_are_retained_for_audit_and_excluded_from_billed_usage() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "run", &request(), "Conversation");
    store.observe(
        "a",
        &measurement(
            "provider:baseline",
            "historical-model",
            "resumeBaseline",
            10000,
            2000,
        ),
    );
    let result = store.snapshot(&Default::default()).unwrap();
    assert_eq!(result.total, 0);
    assert_eq!(result.runs, 0);
    assert!(result.models.is_empty());
    store.observe(
        "a",
        &AgentEvent::Usage {
            input_tokens: 10,
            output_tokens: 2,
        },
    );
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 12);
    let rows: u64 = store
        .0
        .lock()
        .unwrap()
        .db
        .query_row(
            "SELECT COUNT(*) FROM measurements WHERE scope='resumeBaseline'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_deletion_is_reconciled_and_ledger_failure_cannot_skip_doc_purge() {
    let dir = tempfile::tempdir().unwrap();
    let profile = crate::EngineProfile::development(dir.path(), "org", "user");
    let root = profile.store_root().to_path_buf();
    let core = crate::EngineCore::assemble_with_profile(
        profile,
        Arc::new(crate::default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace
        .create_chat(
            "remote-deleted",
            None,
            Some(&core.device_id),
            None,
            Some(dir.path().display().to_string()),
        )
        .unwrap();
    let usage = core.sessions.token_usage().unwrap();
    usage.begin("remote-deleted", "r", &request(), "Conversation");
    usage.observe(
        "remote-deleted",
        &measurement("req", "model", "turn", 10, 1),
    );
    // This is the same registry tombstone the host receives through sync,
    // without executing the host's DeleteChat RPC.
    core.workspace.delete_chat("remote-deleted").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if usage.snapshot(&Default::default()).unwrap().total == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    core.workspace
        .create_chat(
            "local-deleted",
            None,
            Some(&core.device_id),
            None,
            Some(dir.path().display().to_string()),
        )
        .unwrap();
    let store = mast_sync::DocsStore::open(&root).unwrap();
    let doc = mast_doc::SessionDoc::init("local-deleted").unwrap();
    store
        .save_snapshot("local-deleted", &doc.export_snapshot().unwrap())
        .unwrap();
    assert!(store.has_snapshot("local-deleted").unwrap());
    usage
        .0
        .lock()
        .unwrap()
        .db
        .execute_batch("DROP TABLE diagnostics")
        .unwrap();
    let client = mast_rpc::memory_client(core.rpc_service());
    assert!(
        client
            .call(
                mast_rpc::methods::MUTATE,
                serde_json::json!({"op":"deleteChat","chatId":"local-deleted"})
            )
            .await
            .is_ok()
    );
    assert!(!store.has_snapshot("local-deleted").unwrap());
    core.shutdown().await;
}

#[test]
fn duplicate_claude_results_cannot_replace_a_billed_delta_with_a_resume_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "first", &request(), "Conversation");
    store.observe(
        "a",
        &measurement(
            "provider:claude-result:id:model",
            "model",
            "turnIncludingChildren",
            10,
            2,
        ),
    );
    store.observe("a", &done());
    store.begin("a", "resumed", &request(), "Conversation");
    store.observe(
        "a",
        &measurement(
            "provider:claude-result:id:model",
            "model",
            "resumeBaseline",
            10000,
            2000,
        ),
    );
    assert_eq!(store.snapshot(&Default::default()).unwrap().total, 12);
}

#[test]
fn aggregate_replaces_request_even_when_provider_model_labels_differ() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "run", &request(), "Conversation");
    store.observe(
        "a",
        &measurement("request", "model-alias", "request", 10, 2),
    );
    store.observe(
        "a",
        &measurement("result", "model-version", "turnIncludingChildren", 20, 4),
    );
    let result = store.snapshot(&Default::default()).unwrap();
    assert_eq!(result.total, 24);
    assert_eq!(
        store
            .snapshot(&TokenUsageQuery {
                model: Some("model-alias".into()),
                ..Default::default()
            })
            .unwrap()
            .total,
        0
    );
}

#[test]
fn slow_writer_does_not_block_collection_and_queue_overflow_is_visible() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("a", "run", &request(), "Conversation");
    store.flush().unwrap();
    let lock = store.0.lock().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let producer = store.clone();
    let thread = std::thread::spawn(move || {
        for n in 0..1000 {
            producer.observe(
                "a",
                &measurement(&format!("m{n}"), "model", "request", 1, 0),
            );
        }
        sent.send(()).unwrap();
    });
    let finished = received.recv_timeout(std::time::Duration::from_secs(5));
    drop(lock);
    thread.join().unwrap();
    finished.expect("collector must not wait for the database mutex");
    let result = store.snapshot(&Default::default()).unwrap();
    assert!(
        result
            .coverage
            .iter()
            .any(|line| line.contains("telemetry writes failed"))
    );
}

mod efficiency;
