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
fn plain(input: u64, output: u64) -> TokenCounts {
    TokenCounts {
        input,
        output,
        ..Default::default()
    }
}
fn scheduled(task: &str) -> RunRequest {
    RunRequest {
        scheduled_task: Some(task.into()),
        ..request()
    }
}

#[test]
fn cache_columns_stay_apart_and_each_turn_counts_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("c1", "run", &request(), "Conversation");
    store.observe(
        "c1",
        &counts(
            "provider:r:1",
            Some("m"),
            "request",
            cached(1000, 900, 50, 10),
        ),
    );
    store.observe(
        "c1",
        &counts(
            "provider:res:1",
            Some("m"),
            "turnIncludingChildren",
            cached(3200, 2800, 200, 30),
        ),
    );
    store.observe("c1", &done());
    // A turn with no cache detail adds input, but never an uncached figure.
    store.begin("c2", "run2", &request(), "Conversation");
    store.observe(
        "c2",
        &counts("legacy-ish", Some("m"), "turn", plain(500, 5)),
    );
    store.observe("c2", &done());

    let report = store.attribution_report(None, &[]).unwrap();
    let total = &report.total;
    assert_eq!(total.turns, 2);
    assert_eq!(total.input, 3700, "the request row is replaced, not added");
    assert_eq!(total.cache_read, 2800);
    assert_eq!(total.cache_write, 200);
    assert_eq!(total.uncached_input, 200);
    assert_eq!(total.cache_unreported_input, 500);
    assert_eq!(total.output, 35);
    assert_eq!(total.cache_read_share(), Some(2800.0 / 3200.0));

    let scope = |name: &str| {
        report
            .reconciliation
            .iter()
            .find(|s| s.scope == name)
            .unwrap_or_else(|| panic!("scope {name}"))
    };
    assert_eq!(scope("request").all_rows_tokens, 1010);
    assert_eq!(scope("request").accounted_tokens, 0);
    assert_eq!(scope("turnIncludingChildren").accounted_tokens, 3230);
}

#[test]
fn plain_provider_sum_is_reported_beside_the_accounted_total() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("c1", "run", &request(), "Conversation");
    store.observe(
        "c1",
        &counts(
            "provider:r:1",
            Some("m"),
            "request",
            cached(1000, 900, 50, 10),
        ),
    );
    store.observe(
        "c1",
        &counts(
            "provider:res:1",
            Some("m"),
            "turnIncludingChildren",
            cached(3200, 2800, 200, 30),
        ),
    );
    store.observe("c1", &done());
    let report = store.attribution_report(None, &[]).unwrap();
    let row = report
        .activity_reconciliation
        .iter()
        .find(|a| a.activity == "Conversation")
        .unwrap();
    assert_eq!(row.all_provider_rows_tokens, 1010 + 3230);
    assert_eq!(row.accounted_tokens, 3230);
    assert_eq!(
        row.all_provider_rows_uncounted_input,
        (1000 - 900) + (3200 - 2800)
    );
}

#[test]
fn scheduled_turns_after_the_run_finished_are_follow_ups_counted_as_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for (chat, task) in [
        ("follow", "Daily summary"),
        ("live", "Daily summary"),
        ("running", "Daily summary"),
        ("orphan", "Gone"),
    ] {
        store.begin(
            chat,
            &format!("run-{chat}"),
            &scheduled(task),
            "Scheduled task",
        );
        store.observe(
            chat,
            &counts(
                &format!("provider:res:{chat}"),
                Some("m"),
                "turnIncludingChildren",
                cached(1000, 900, 50, 10),
            ),
        );
        store.observe(chat, &done());
    }
    let now = crate::now_ms();
    let runs = [
        ScheduledRun {
            chat_id: "follow".into(),
            task: "Daily summary".into(),
            started_at: now - 20_000,
            finished_at: Some(now - 10_000),
        },
        ScheduledRun {
            chat_id: "live".into(),
            task: "Daily summary".into(),
            started_at: now - 20_000,
            finished_at: Some(now + 3_600_000),
        },
        // Still running: no finish time, so none of its turns are follow-ups.
        ScheduledRun {
            chat_id: "running".into(),
            task: "Daily summary".into(),
            started_at: now - 20_000,
            finished_at: None,
        },
    ];
    let report = store.attribution_report(None, &runs).unwrap();
    let find = |rows: &[AttributionRow], key: &str| {
        rows.iter().find(|r| r.key == key).map(|r| r.columns.turns)
    };
    assert_eq!(find(&report.categories, "Scheduled-run follow-up"), Some(1));
    assert_eq!(find(&report.categories, "Scheduled task"), Some(2));
    // Unattended runs are not attributed to a person, whatever the prompt says.
    assert_eq!(
        find(&report.triggers, "main · Follow-up in scheduled chat"),
        Some(1)
    );
    assert_eq!(find(&report.triggers, "main · Scheduled run"), Some(2));
    assert_eq!(
        find(&report.triggers, "main · Scheduled chat (no run history)"),
        Some(1)
    );
    assert_eq!(
        find(&report.categories, "Scheduled task (no run history)"),
        Some(1)
    );
    assert_eq!(find(&report.activities, "Conversation"), Some(1));
    let task = report
        .scheduled_tasks
        .iter()
        .find(|t| t.task == "Daily summary")
        .unwrap();
    assert_eq!(
        (task.run.turns, task.follow_up.turns, task.run_chats),
        (2, 1, 3)
    );
    // The turn recorded its own task name, so a run missing from history is
    // still attributed to the right task.
    assert!(report.scheduled_tasks.iter().any(|t| t.task == "Gone"));
}

#[test]
fn unknown_model_turns_say_what_is_known_about_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let codex = |model: Option<&str>| RunRequest {
        harness: Some(mast_proto::HarnessId::Codex),
        model: model.map(str::to_owned),
        ..request()
    };
    store.begin(
        "configured",
        "r1",
        &codex(Some("gpt-configured")),
        "Conversation",
    );
    store.observe(
        "configured",
        &counts("provider:codex:a", None, "turn", plain(100, 1)),
    );
    store.observe("configured", &done());
    store.begin("sibling", "r2", &codex(None), "Conversation");
    store.observe(
        "sibling",
        &counts(
            "provider:codex:b",
            Some("gpt-sibling"),
            "turn",
            plain(400, 4),
        ),
    );
    store.observe("sibling", &done());
    store.activate("sibling");
    store.observe(
        "sibling",
        &counts("provider:codex:c", None, "turn", plain(200, 2)),
    );
    store.observe("sibling", &done());
    store.begin("alone", "r3", &codex(None), "Conversation");
    store.observe(
        "alone",
        &counts("provider:codex:d", None, "turn", plain(50, 1)),
    );
    store.observe("alone", &done());

    let report = store.attribution_report(None, &[]).unwrap();
    let unknown = report
        .models
        .iter()
        .filter(|m| m.key == "Unknown model")
        .map(|m| (m.detail.as_str(), m.columns.input))
        .collect::<Vec<_>>();
    assert_eq!(unknown.len(), 3, "{unknown:?}");
    let has = |needle: &str, input: u64| {
        unknown
            .iter()
            .any(|(detail, tokens)| detail.contains(needle) && *tokens == input)
    };
    assert!(has("configured model gpt-configured", 100), "{unknown:?}");
    assert!(has("other turns use gpt-sibling", 200), "{unknown:?}");
    assert!(
        has("no other turn in this chat names one", 50),
        "{unknown:?}"
    );
    assert!(report.models.iter().any(|m| m.key == "gpt-sibling"));
}

#[test]
fn scheduled_task_name_is_recorded_on_the_turn_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("c", "run", &scheduled("Nightly"), "Scheduled task");
    store.observe(
        "c",
        &counts("provider:res:c", Some("m"), "turn", plain(10, 1)),
    );
    store.observe("c", &done());
    let report = store.attribution_report(None, &[]).unwrap();
    assert_eq!(report.scheduled_tasks.len(), 1);
    assert_eq!(report.scheduled_tasks[0].task, "Nightly");
}

#[test]
fn turns_record_what_started_them_and_old_rows_stay_unrecorded() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for (chat, prompt) in [
        ("human", "please fix the bug"),
        (
            "wake",
            "[Mast system message: worker updates] Worker updates: done",
        ),
    ] {
        let req = RunRequest {
            prompt: prompt.into(),
            ..request()
        };
        store.begin(chat, &format!("run-{chat}"), &req, "Conversation");
        store.set_trigger(chat, trigger_label(prompt), false);
        store.observe(
            chat,
            &counts(
                &format!("provider:t:{chat}"),
                Some("m"),
                "turn",
                plain(10, 1),
            ),
        );
        store.observe(chat, &done());
    }
    let report = store.attribution_report(None, &[]).unwrap();
    let keys = report
        .triggers
        .iter()
        .map(|r| r.key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"main · Human"), "{keys:?}");
    assert!(keys.contains(&"main · Worker updates"), "{keys:?}");
}

#[test]
fn a_continued_turn_gets_its_own_trigger_and_a_mid_turn_steer_never_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    let measure = |id: &str| counts(id, Some("m"), "turn", plain(10, 1));
    store.begin("c", "run", &request(), "Conversation");
    store.set_trigger("c", "Human".into(), false);
    // A steer into the running turn does not label the turn that follows it.
    store.set_trigger("c", "Other chat".into(), true);
    store.observe("c", &measure("provider:t:1"));
    store.observe("c", &done());
    // The next message arrives in the parked process and labels its own turn.
    store.set_trigger("c", "Worker updates".into(), true);
    store.activate("c");
    store.observe("c", &measure("provider:t:2"));
    store.observe("c", &done());
    // A turn whose message was never classified stays unrecorded.
    store.activate("c");
    store.observe("c", &measure("provider:t:3"));
    store.observe("c", &done());

    let report = store.attribution_report(None, &[]).unwrap();
    let turns = |key: &str| {
        report
            .triggers
            .iter()
            .find(|r| r.key == key)
            .map(|r| r.columns.turns)
    };
    assert_eq!(turns("main · Human"), Some(1));
    assert_eq!(turns("main · Worker updates"), Some(1));
    assert_eq!(turns("main · Trigger not recorded"), Some(1));
    assert_eq!(turns("main · Other chat"), None);
}

#[test]
fn cache_fields_count_only_when_both_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    store.begin("c", "run", &request(), "Conversation");
    store.observe(
        "c",
        &counts(
            "provider:t:1",
            Some("m"),
            "turn",
            TokenCounts {
                input: 400,
                output: 4,
                cache_read: Some(300),
                cache_write: None,
                reasoning: None,
            },
        ),
    );
    store.observe("c", &done());
    let total = store.attribution_report(None, &[]).unwrap().total;
    assert_eq!(total.input, 400);
    assert_eq!(total.cache_read, 0);
    assert_eq!(total.cache_unreported_input, 400);
    assert_eq!(total.cache_read_share(), None);
}

#[test]
fn report_totals_equal_the_snapshot_totals() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenUsage::open(dir.path()).unwrap();
    for (chat, activity) in [("a", "Conversation"), ("b", "Work-item automation")] {
        store.begin(chat, &format!("run-{chat}"), &request(), activity);
        store.observe(
            chat,
            &counts(
                &format!("provider:r:{chat}"),
                Some("m"),
                "request",
                cached(900, 800, 50, 9),
            ),
        );
        store.observe(
            chat,
            &counts(
                &format!("provider:res:{chat}"),
                Some("m"),
                "turnIncludingChildren",
                cached(1800, 1600, 100, 18),
            ),
        );
        store.observe(chat, &done());
    }
    let report = store.attribution_report(None, &[]).unwrap();
    let snapshot = store.snapshot(&TokenUsageQuery::default()).unwrap();
    assert_eq!(
        report.total.input + report.total.output,
        snapshot.total,
        "the report and the engine query must account the same ledger the same way"
    );
    assert_eq!(snapshot.total, 2 * (1800 + 18));
}
