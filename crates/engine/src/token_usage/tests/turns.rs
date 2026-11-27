use super::*;

fn counts(id: &str, model: Option<&str>, scope: &str, c: TokenCounts) -> AgentEvent {
    AgentEvent::UsageMeasurement {
        measurement: UsageMeasurement {
            id: id.into(),
            model: model.map(str::to_owned),
            scope: scope.into(),
            counts: c,
        },
    }
}
fn cached(input: u64, read: u64, write: u64, output: u64) -> TokenCounts {
    TokenCounts {
        input,
        output,
        cache_read: Some(read),
        cache_write: Some(write),
        reasoning: None,
    }
}
fn analysis(store: &TokenUsage) -> (TokenUsageSnapshot, TurnAnalysis) {
    let snapshot = store.snapshot(&TokenUsageQuery::default()).unwrap();
    let analysis = snapshot.turn_analysis.clone().expect("turn analysis");
    (snapshot, analysis)
}

#[test]
fn turn_profiles_reconcile_with_totals_and_record_session_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let resumed = RunRequest {
        resume: Some("native-session".into()),
        ..request()
    };
    store.begin("claude", "run", &resumed, "Conversation");
    let created = crate::now_ms() - 2 * 3_600_000;
    store.set_context("claude", None, None, None, Some(created));
    for (id, read) in [
        ("provider:claude-request:a", 900),
        ("provider:claude-request:b", 1900),
    ] {
        store.observe(
            "claude",
            &counts(
                id,
                Some("large"),
                "request",
                cached(1000 + read - 900, read, 100, 10),
            ),
        );
    }
    store.observe(
        "claude",
        &counts(
            "provider:claude-result:r:large",
            Some("large"),
            "turnIncludingChildren",
            cached(3200, 2800, 200, 30),
        ),
    );
    store.observe(
        "claude",
        &AgentEvent::ContextUsage {
            tokens: Some(2100),
            window: Some(200_000),
        },
    );
    store.observe(
        "claude",
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "rg needle".into(),
            },
        },
    );
    store.observe(
        "claude",
        &AgentEvent::UsageDiagnostic {
            diagnostic: UsageDiagnostic {
                id: "t".into(),
                category: "toolResult".into(),
                label: "Tool result".into(),
                bytes: 4096,
                stage: "harnessResponse".into(),
                method: "bytesOnly".into(),
                ..Default::default()
            },
        },
    );
    store.observe("claude", &done());
    // A second turn in the same process carries the accumulated context.
    store.activate("claude");
    store.observe(
        "claude",
        &counts(
            "provider:claude-result:s:large",
            Some("large"),
            "turnIncludingChildren",
            cached(500, 450, 50, 5),
        ),
    );
    store.observe("claude", &done());

    let codex = RunRequest {
        harness: Some(mast_proto::HarnessId::Codex),
        ..request()
    };
    store.begin("codex", "codex-run", &codex, "Conversation");
    // Request rows written before the model is known must not add to the turn.
    store.observe(
        "codex",
        &counts(
            "provider:codex-request:x:100:1",
            None,
            "request",
            cached(100, 80, 0, 1),
        ),
    );
    store.observe(
        "codex",
        &AgentEvent::SessionStarted {
            harness: mast_proto::HarnessId::Codex,
            model: "gpt".into(),
            tools: Vec::new(),
            cwd: "/test".into(),
            session_id: "s".into(),
            assistant_message_id: "m".into(),
            chat_id: None,
            space_id: None,
        },
    );
    store.observe(
        "codex",
        &counts(
            "provider:codex-request:x:250:3",
            None,
            "request",
            cached(150, 120, 0, 2),
        ),
    );
    store.observe(
        "codex",
        &counts("provider:codex:x", None, "turn", cached(250, 200, 0, 3)),
    );
    store.observe("codex", &done());

    let (snapshot, analysis) = analysis(&store);
    let turn_sum: u64 = analysis
        .turns
        .iter()
        .map(|t| t.counts.input + t.counts.output)
        .sum();
    assert_eq!(snapshot.total, 3230 + 505 + 253);
    assert_eq!(
        turn_sum, snapshot.total,
        "turn profiles reconcile with the total"
    );

    let first = analysis
        .turns
        .iter()
        .find(|t| t.turn_id == "run:0")
        .unwrap();
    assert_eq!(first.session_start.as_deref(), Some("resumed"));
    assert_eq!(first.scope, "turnIncludingChildren");
    assert_eq!(first.counts.cache_read, Some(2800));
    assert_eq!(first.requests, Some(2));
    assert_eq!(first.subagent_requests, Some(0));
    assert_eq!(first.peak_context, Some(2100));
    assert_eq!((first.tool_calls, first.tool_result_bytes), (1, 4096));
    assert!(
        first
            .chat_age_ms
            .is_some_and(|age| (2 * 3_600_000..3 * 3_600_000).contains(&age))
    );

    let second = analysis
        .turns
        .iter()
        .find(|t| t.turn_id == "run:1")
        .unwrap();
    assert_eq!(second.session_start.as_deref(), Some("continued"));
    assert_eq!(
        second.requests, None,
        "no request rows is unknown, not zero"
    );

    let codex_turn = analysis
        .turns
        .iter()
        .find(|t| t.turn_id == "codex-run:0")
        .unwrap();
    assert_eq!(codex_turn.session_start.as_deref(), Some("fresh"));
    assert_eq!(codex_turn.counts.input, 250);
    assert_eq!(codex_turn.requests, Some(2));
    assert_eq!(codex_turn.chat_age_ms, None);

    // The completion stamp reads the same billing rules per chat.
    let stamp = store.chat_digest_tokens("claude").unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(stamp).unwrap(),
        serde_json::json!({"inputTokens": 3700, "cachedInputTokens": 3250, "outputTokens": 35, "turns": 2, "toolCalls": 1})
    );
    let codex_stamp = store.chat_digest_tokens("codex").unwrap().unwrap();
    assert_eq!((codex_stamp.input_tokens, codex_stamp.cached_input_tokens, codex_stamp.output_tokens), (250, 200, 3));
    assert_eq!((codex_stamp.turns, codex_stamp.tool_calls), (1, 0));
    assert_eq!(store.chat_digest_tokens("never-ran").unwrap(), None);

    let claude = analysis
        .workflows
        .iter()
        .find(|g| g.label == "ClaudeCode · main · resumed session")
        .unwrap();
    assert_eq!(
        (claude.turns, claude.cache_read, claude.cache_known_input),
        (1, 2800, 3200)
    );
    assert_eq!((claude.requests, claude.request_known_input), (2, 3200));
    assert!(
        analysis
            .chat_ages
            .iter()
            .any(|g| g.label == "Chat 1–6 hours old" && g.turns == 2)
    );
    let codex_coverage = analysis
        .harnesses
        .iter()
        .find(|h| h.harness == "Codex")
        .unwrap();
    assert_eq!(
        (codex_coverage.turns, codex_coverage.request_count_turns),
        (1, 1)
    );
    assert!(codex_coverage.semantics.contains("cache writes"));
}

#[test]
fn legacy_only_turns_keep_cache_and_requests_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let cursor = RunRequest {
        harness: Some(mast_proto::HarnessId::Cursor),
        ..request()
    };
    store.begin("chat", "run", &cursor, "Conversation");
    store.observe(
        "chat",
        &AgentEvent::Usage {
            input_tokens: 70,
            output_tokens: 7,
        },
    );
    store.observe("chat", &done());
    let (snapshot, analysis) = analysis(&store);
    assert_eq!(snapshot.total, 77);
    let turn = &analysis.turns[0];
    assert_eq!(turn.scope, "legacy");
    assert_eq!(
        (
            turn.counts.cache_read,
            turn.counts.cache_write,
            turn.requests
        ),
        (None, None, None)
    );
    let group = &analysis.workflows[0];
    assert_eq!(
        (
            group.cache_known_turns,
            group.cache_known_input,
            group.request_known_turns
        ),
        (0, 0, 0)
    );
    let coverage = &analysis.harnesses[0];
    assert_eq!(
        (coverage.harness.as_str(), coverage.legacy_only_turns),
        ("Cursor", 1)
    );
    assert!(coverage.semantics.contains("not reported"));
}

#[test]
fn filters_apply_to_turn_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("chat", "run", &request(), "Conversation");
    store.observe(
        "chat",
        &measurement("a", "large", "turnIncludingChildren", 100, 10),
    );
    store.observe(
        "chat",
        &measurement("b", "small", "turnIncludingChildren", 40, 4),
    );
    store.observe("chat", &done());
    let snapshot = store
        .snapshot(&TokenUsageQuery {
            model: Some("small".into()),
            ..Default::default()
        })
        .unwrap();
    let turns = snapshot.turn_analysis.unwrap().turns;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].models, vec!["small".to_owned()]);
    assert_eq!(
        turns[0].counts.input + turns[0].counts.output,
        snapshot.total
    );
}

#[test]
fn percentiles_use_nearest_rank_and_shown_turns_are_bounded() {
    let profiles = (1..=120)
        .map(|n| TurnProfile {
            turn_id: format!("t{n}"),
            chat_id: "c".into(),
            harness: Some("Codex".into()),
            agent_type: "main".into(),
            counts: TokenCounts {
                input: n,
                ..Default::default()
            },
            ..Default::default()
        })
        .collect();
    let analysis = super::super::turns::analyze(profiles);
    assert_eq!(analysis.turns.len(), 50);
    assert_eq!(analysis.turns[0].counts.input, 120);
    let group = &analysis.workflows[0];
    assert_eq!(
        (group.turns, group.median_input, group.p90_input),
        (120, 60, 108)
    );
    assert_eq!(group.label, "Codex · main · start unrecorded");
}
