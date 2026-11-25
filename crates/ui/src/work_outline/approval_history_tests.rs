// Approval history in the Space-root Friction section: the Approvals filter,
// search, outcome and provenance, and Create rule…, which only opens a draft.
// Included from `overview/logic_tests.rs` so it shares the Overview fixtures.
use super::*;

type HistoryCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

const COMMAND: &str = "cargo nextest run -p mast-engine --test engine";

/// A Permission row the engine recorded. `exact` says whether it holds the
/// structured command; `answer` is the last outcome.
fn permission(id: &str, command: &str, exact: bool, answer: Option<bool>) -> serde_json::Value {
    serde_json::json!({
        "id": id, "kind": "permission", "summary": "Approve command requested",
        "whatHappened": format!("Codex wants to run `{command}`. Allow it? Reason: sandbox"),
        "workaround": null, "suggestion": null,
        "approval": {"command": command, "why": "sandbox", "answer": answer, "exact": exact},
        "provenance": {"chatId": "chat-1", "harness": "codex", "model": null, "spaceId": "s1", "workItemId": "w-api", "createdAt": 1, "source": "auto"},
        "fingerprint": id, "occurrences": 2, "lastSeen": 1, "status": "open"
    })
}

fn bug(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "kind": "bug", "summary": "Build failed", "whatHappened": "It failed",
        "workaround": null, "suggestion": null, "approval": null,
        "provenance": {"chatId": "chat-1", "harness": "codex", "model": null, "spaceId": "s1", "createdAt": 1, "source": "agent"},
        "fingerprint": id, "occurrences": 1, "lastSeen": 1, "status": "open"
    })
}

fn history_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    reports: Vec<serde_json::Value>,
) -> (Entity<Overview>, HistoryCalls, &'a mut gpui::VisualTestContext) {
    history_overview_with_ledger(cx, dir, reports, serde_json::json!({"entries": [], "total": 0}))
}

/// A per-request ledger entry as the engine lists it.
fn ledger_entry(id: &str, command: &str, decision: &str, decider: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "spaceId": "s1", "chatId": "chat-1", "requestId": format!("req-{id}"), "workItemId": "w-api",
        "harness": "codex", "command": command, "source": "prompt", "requestedAt": 1,
        "decision": decision, "decider": decider
    })
}

fn history_overview_with_ledger<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    reports: Vec<serde_json::Value>,
    ledger: serde_json::Value,
) -> (Entity<Overview>, HistoryCalls, &'a mut gpui::VisualTestContext) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    let calls: HistoryCalls = Default::default();
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.spaces[0].path = dir.to_string_lossy().into_owned();
        state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(
            std::sync::Arc::new(move |method, params| {
                sink.lock().unwrap().push((method.to_owned(), params.clone()));
                match method {
                    mast_rpc::methods::GET_FRICTION_SETTINGS => Ok(serde_json::json!({"autoCapture": true})),
                    mast_rpc::methods::LIST_FRICTION_REPORTS => Ok(serde_json::json!(reports)),
                    mast_rpc::methods::LIST_APPROVAL_LEDGER => {
                        let Some(entries) = ledger["entries"].as_array() else {
                            return Ok(ledger.clone());
                        };
                        let query = params["query"].as_str().unwrap_or_default();
                        let offset = params["offset"].as_u64().unwrap_or_default() as usize;
                        let limit = params["limit"].as_u64().unwrap_or(50) as usize;
                        let matches: Vec<_> = entries.iter().filter(|entry| {
                            serde_json::from_value::<mast_proto::ApprovalLedgerEntry>((*entry).clone())
                                .unwrap().matches_query(query)
                        }).collect();
                        let total = matches.len();
                        let entries: Vec<_> = matches.into_iter().skip(offset).take(limit).collect();
                        Ok(serde_json::json!({"total": total, "entries": entries}))
                    }
                    _ => Ok(serde_json::json!({})),
                }
            }),
        ));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(None), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    (overview, calls, vcx)
}

fn click_selector(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let at = row_bounds(vcx, selector.to_owned()).unwrap_or_else(|| panic!("{selector} renders")).center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

/// Click a row's heading (its top edge), which stays put when the row expands.
fn click_heading(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let bounds = row_bounds(vcx, selector.to_owned()).unwrap_or_else(|| panic!("{selector} renders"));
    vcx.simulate_click(bounds.origin + gpui::point(gpui::px(24.0), gpui::px(10.0)), gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn rule_saves(calls: &HistoryCalls) -> Vec<serde_json::Value> {
    calls.lock().unwrap().iter()
        .filter(|(method, params)| method == "WorkOutlineMutate" && params["op"] == "setApprovalRule")
        .map(|(_, params)| params.clone()).collect()
}

fn open_history(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
) -> Entity<WorkItemDetails> {
    let details = open_details(overview, vcx, "");
    vcx.run_until_parked();
    draw(vcx);
    details
}

#[gpui::test]
fn the_approvals_filter_and_search_narrow_the_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let reports = vec![
        permission("nextest", COMMAND, true, Some(true)),
        permission("pgrep", "pgrep cargo", true, Some(false)),
        bug("build"),
    ];
    let (overview, _calls, vcx) = history_overview(cx, dir.path(), reports);
    let details = open_history(&overview, vcx);
    for id in ["nextest", "pgrep", "build"] {
        assert!(row_bounds(vcx, format!("friction-row-{id}")).is_some(), "{id} listed");
    }
    click_selector(vcx, "friction-filter-Approvals");
    assert!(row_bounds(vcx, "friction-row-build".into()).is_none(), "a bug is not an approval");
    assert!(row_bounds(vcx, "friction-row-nextest".into()).is_some());
    details.update(vcx, |details, cx| details.set_friction_search_for_test("NEXTEST engine", cx));
    draw(vcx);
    assert!(row_bounds(vcx, "friction-row-nextest".into()).is_some(), "every word matches, in any case");
    assert!(row_bounds(vcx, "friction-row-pgrep".into()).is_none());
    details.update(vcx, |details, cx| details.set_friction_search_for_test("denied", cx));
    draw(vcx);
    assert!(row_bounds(vcx, "friction-row-pgrep".into()).is_some(), "the outcome is searchable");
    assert!(row_bounds(vcx, "friction-row-nextest".into()).is_none());
    details.update(vcx, |details, cx| details.set_friction_search_for_test("no such command", cx));
    draw(vcx);
    assert!(row_bounds(vcx, "friction-no-match".into()).is_some());
}

#[gpui::test]
fn a_past_approved_command_becomes_a_reviewed_rule_only_when_saved(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = history_overview(cx, dir.path(), vec![permission("nextest", COMMAND, true, Some(true))]);
    let details = open_history(&overview, vcx);
    click_heading(vcx, "friction-row-nextest");
    assert!(row_bounds(vcx, "friction-workitem-nextest".into()).is_some(), "the first work item is linked");
    click_selector(vcx, "friction-rule-nextest");
    assert!(rule_saves(&calls).is_empty(), "opening the draft saves nothing");
    assert!(row_bounds(vcx, "approval-rule-note".into()).is_some(), "the draft explains its scope");
    details.update(vcx, |details, cx| details.save_approval_rule(cx));
    vcx.run_until_parked();
    let saved = rule_saves(&calls);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["rule"]["argvPrefix"], serde_json::json!(["cargo", "nextest", "run", "-p", "mast-engine", "--test", "engine"]));
    assert_eq!(saved[0]["rule"]["enabled"], true);
    assert_eq!(saved[0]["rule"]["harness"], "codex");
    assert!(saved[0]["rule"]["cwdScope"].is_null(), "the Space root only");
    let regex = saved[0]["rule"]["commandRegex"].as_str().expect("an anchored regex");
    assert!(regex.starts_with('^') && regex.ends_with('$'), "only the recorded command: {regex}");
    assert_eq!(saved[0]["editEnvironment"], false);
}

#[gpui::test]
fn rows_that_cannot_be_exact_or_were_not_approved_offer_no_rule(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let reports = vec![
        permission("denied", COMMAND, true, Some(false)),
        permission("unanswered", COMMAND, true, None),
        permission("secret", "cargo test --token [REDACTED]", true, Some(true)),
        permission("cut", "cargo test ", false, Some(true)),
        permission("compound", "cargo test && cargo build", true, Some(true)),
    ];
    let (overview, calls, vcx) = history_overview(cx, dir.path(), reports);
    let _details = open_history(&overview, vcx);
    for id in ["denied", "unanswered", "secret", "cut", "compound"] {
        click_heading(vcx, &format!("friction-row-{id}"));
        assert!(row_bounds(vcx, format!("friction-rule-refused-{id}")).is_some(), "{id} says why");
        assert!(row_bounds(vcx, format!("friction-rule-{id}")).is_none(), "{id} has no Create rule");
        click_heading(vcx, &format!("friction-row-{id}"));
    }
    assert!(rule_saves(&calls).is_empty());
}

#[gpui::test]
fn the_approvals_view_lists_every_request_and_the_search_narrows_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // The same command asked twice, answered differently: two rows, though friction shows one.
    let ledger = serde_json::json!({"total": 3, "entries": [
        ledger_entry("a", COMMAND, "allowed", "user"),
        ledger_entry("b", COMMAND, "cancelled", "teardown"),
        ledger_entry("c", "pgrep cargo", "denied", "space_rule"),
    ]});
    let (overview, calls, vcx) = history_overview_with_ledger(cx, dir.path(), vec![permission("nextest", COMMAND, true, Some(true))], ledger);
    let details = open_history(&overview, vcx);
    assert!(row_bounds(vcx, "approval-ledger".into()).is_none(), "the ledger loads with the Approvals filter");
    click_selector(vcx, "friction-filter-Approvals");
    assert!(calls.lock().unwrap().iter().any(|(method, params)| method == mast_rpc::methods::LIST_APPROVAL_LEDGER && params["spaceId"] == "s1"));
    for id in ["a", "b", "c"] {
        assert!(row_bounds(vcx, format!("ledger-row-{id}")).is_some(), "{id} listed");
    }
    assert!(row_bounds(vcx, "ledger-workitem-a".into()).is_some(), "the work item is linked");
    details.update(vcx, |details, cx| details.set_friction_search_for_test("cancelled", cx));
    vcx.run_until_parked();
    draw(vcx);
    assert!(row_bounds(vcx, "ledger-row-b".into()).is_some(), "the outcome is searchable");
    assert!(row_bounds(vcx, "ledger-row-a".into()).is_none());
    details.update(vcx, |details, cx| details.set_friction_search_for_test("space rule pgrep", cx));
    vcx.run_until_parked();
    draw(vcx);
    assert!(row_bounds(vcx, "ledger-row-c".into()).is_some(), "the decider is searchable");
    details.update(vcx, |details, cx| details.set_friction_search_for_test("no such command", cx));
    vcx.run_until_parked();
    draw(vcx);
    assert!(row_bounds(vcx, "approval-ledger-no-match".into()).is_some());
}

#[gpui::test]
fn approval_history_can_page_through_retained_requests(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<_> = (0..51).map(|index| ledger_entry(&format!("page-{index}"), COMMAND, "allowed", "user")).collect();
    let ledger = serde_json::json!({"entries": entries, "total": 51});
    let (overview, calls, vcx) = history_overview_with_ledger(cx, dir.path(), vec![], ledger);
    open_history(&overview, vcx);
    click_selector(vcx, "friction-filter-Approvals");
    assert!(row_bounds(vcx, "ledger-row-page-0".into()).is_some());
    assert!(row_bounds(vcx, "ledger-row-page-50".into()).is_none());
    click_selector(vcx, "ledger-next");
    assert!(calls.lock().unwrap().iter().any(|(method, params)| method == mast_rpc::methods::LIST_APPROVAL_LEDGER && params["offset"] == 50));
    assert!(row_bounds(vcx, "ledger-row-page-50".into()).is_some());
    click_selector(vcx, "ledger-previous");
    assert!(row_bounds(vcx, "ledger-row-page-0".into()).is_some());
}

#[gpui::test]
fn an_engine_without_the_ledger_leaves_the_approvals_view_as_it_was(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // An older engine answers an unknown method with nothing usable.
    let (overview, _calls, vcx) = history_overview_with_ledger(cx, dir.path(), vec![permission("nextest", COMMAND, true, Some(true))], serde_json::json!({}));
    let _details = open_history(&overview, vcx);
    click_selector(vcx, "friction-filter-Approvals");
    assert!(row_bounds(vcx, "approval-ledger".into()).is_none());
    assert!(row_bounds(vcx, "friction-row-nextest".into()).is_some(), "the aggregated rows still show");
}
