//! The compact observed-state summary in `work_item_external_run_list`.

use mast_proto::{ExternalRunView, ExternalRunsObserved, format_age};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObservedDetail {
    Off,
    Compact,
    Full,
}

fn obs(observed: Option<&ExternalRunsObserved>, run: &ExternalRunView, detail: ObservedDetail, now_s: i64) -> Option<Value> {
    let Some(observed) = observed.filter(|o| o.hosted) else {
        return Some(json!({ "state": "unknown", "why": "not observed on this device" }));
    };
    let Some(seen) = observed.runs.get(&run.id) else {
        if run.ended_at.is_some() {
            return None;
        }
        return Some(json!({ "state": "unknown", "why": "not yet observed" }));
    };
    let age = |at: i64| format_age((now_s - at).max(0) as u64);
    let mut value = json!({
        "state": seen.state.word(),
        "for": age(seen.since),
        "seen": age(seen.seen_at),
        "why": seen.why,
    });
    if detail == ObservedDetail::Full {
        if let Some(code) = seen.exit_code {
            value["exitCode"] = json!(code);
        }
        if let Some(idle) = seen.idle_s {
            value["idleS"] = json!(idle);
        }
        if let Some(problem) = &observed.gh_problem {
            value["ghProblem"] = json!(problem);
        }
    }
    Some(value)
}

/// Adds `capacity` and one `obs` per run to a list reply.
pub(super) fn attach(
    reply: &mut Value,
    runs: &[&ExternalRunView],
    observed: Option<&ExternalRunsObserved>,
    detail: ObservedDetail,
    now_s: i64,
) {
    if let Some(capacity) = observed.filter(|o| o.hosted).and_then(|o| o.capacity) {
        reply["capacity"] = json!(capacity.line());
    }
    if let Some(entries) = reply["runs"].as_array_mut() {
        for (entry, run) in entries.iter_mut().zip(runs) {
            if let Some(value) = obs(observed, run, detail, now_s) {
                entry["obs"] = value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{Capacity, RunObservation, RunState};

    fn view(id: &str, ended: bool) -> ExternalRunView {
        serde_json::from_value(json!({
            "id": id, "runTag": format!("tag-{id}"), "workItemId": "item", "kind": "codespace",
            "endedAt": if ended { json!(5) } else { Value::Null }
        }))
        .unwrap()
    }

    fn observed(ids: &[&str]) -> ExternalRunsObserved {
        ExternalRunsObserved {
            hosted: true,
            capacity: Some(Capacity { running: 3, cap: 5, total: 6, at: 1_000 }),
            gh_problem: None,
            runs: ids
                .iter()
                .map(|id| {
                    (
                        (*id).to_owned(),
                        RunObservation {
                            state: RunState::Running,
                            since: 1_000 - 7_800,
                            seen_at: 1_000 - 40,
                            why: "tmux up, no exit file".into(),
                            exit_code: Some(0),
                            idle_s: Some(42),
                        },
                    )
                })
                .collect(),
        }
    }

    fn reply(runs: &[ExternalRunView]) -> Value {
        json!({ "workItemId": "item", "runs": runs })
    }

    #[test]
    fn five_runs_add_a_small_summary() {
        let runs: Vec<_> = (1..=5).map(|n| view(&format!("r{n}"), false)).collect();
        let refs: Vec<_> = runs.iter().collect();
        let ids = ["r1", "r2", "r3", "r4", "r5"];
        let plain = reply(&runs);
        let mut with = plain.clone();
        attach(&mut with, &refs, Some(&observed(&ids)), ObservedDetail::Compact, 1_000);
        let added = with.to_string().len() - plain.to_string().len();
        let tokens = added as f64 / 3.5;
        println!(
            "five runs: observed summary adds {added} chars (~{tokens:.0} tokens); whole reply {} chars (~{:.0} tokens)",
            with.to_string().len(),
            with.to_string().len() as f64 / 3.5
        );
        let summary = json!({
            "capacity": with["capacity"],
            "runs": with["runs"].as_array().unwrap().iter()
                .map(|run| json!({"tag": run["runTag"], "obs": run["obs"]}))
                .collect::<Vec<_>>(),
        });
        let summary_tokens = summary.to_string().len() as f64 / 3.5;
        println!("five runs: capacity + tag + obs alone is {} chars (~{summary_tokens:.0} tokens)", summary.to_string().len());
        assert!(summary_tokens < 220.0, "{summary_tokens}");
        assert!(tokens < 220.0, "{tokens}");
        assert_eq!(with["capacity"], "3/5 running, 6 boxes");
        assert_eq!(
            with["runs"][0]["obs"],
            json!({"state":"running","for":"2h10m","seen":"40s","why":"tmux up, no exit file"})
        );
    }

    #[test]
    fn full_adds_detail_and_compact_does_not() {
        let runs = [view("r1", false)];
        let refs: Vec<_> = runs.iter().collect();
        let mut compact = reply(&runs);
        attach(&mut compact, &refs, Some(&observed(&["r1"])), ObservedDetail::Compact, 1_000);
        assert!(compact["runs"][0]["obs"].get("exitCode").is_none());
        let mut full = reply(&runs);
        attach(&mut full, &refs, Some(&observed(&["r1"])), ObservedDetail::Full, 1_000);
        assert_eq!(full["runs"][0]["obs"]["exitCode"], 0);
        assert_eq!(full["runs"][0]["obs"]["idleS"], 42);
    }

    #[test]
    fn an_unhosted_device_and_an_unseen_run_say_so() {
        let runs = [view("r1", false), view("r2", true)];
        let refs: Vec<_> = runs.iter().collect();
        let mut unhosted = reply(&runs);
        attach(&mut unhosted, &refs, Some(&ExternalRunsObserved::default()), ObservedDetail::Compact, 0);
        assert_eq!(unhosted["runs"][0]["obs"]["why"], "not observed on this device");
        assert!(unhosted.get("capacity").is_none());
        let mut unseen = reply(&runs);
        attach(&mut unseen, &refs, Some(&observed(&[])), ObservedDetail::Compact, 0);
        assert_eq!(unseen["runs"][0]["obs"]["why"], "not yet observed");
        assert!(unseen["runs"][1].get("obs").is_none());
    }
}
