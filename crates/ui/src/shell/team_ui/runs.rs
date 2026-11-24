//! The runs recorded on the coordinator's items: work that executes outside a
//! local harness, such as a codespace. This lists the durable record only (what
//! was registered, whether it is still expected alive, how it ended); nothing
//! here is observed or fetched.
use super::*;
use mast_proto::{ExternalRunOutcome, ExternalRunView, ExternalRunsObserved, format_age};

/// Ended runs shown after the live ones, so a long history does not push the
/// rest of the Tracker off screen.
const ENDED_RUNS_SHOWN: usize = 3;

pub(super) struct TrackerRun {
    pub item_title: String,
    pub run: ExternalRunView,
}

/// The runs on the chat's item and its descendants: live ones first, newest
/// update first, then a few of the most recently ended.
pub(super) fn tracker_runs(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    chat_id: &str,
) -> Vec<TrackerRun> {
    let Some(root) = snapshot
        .items
        .iter()
        .find(|item| item.chat_ids.iter().any(|id| id == chat_id))
    else {
        return Vec::new();
    };
    let mut live: Vec<&ExternalRunView> = Vec::new();
    let mut ended: Vec<&ExternalRunView> = Vec::new();
    for run in snapshot
        .external_runs
        .iter()
        .filter(|run| snapshot.is_within(&root.id, &run.work_item_id))
    {
        if run.ended_at.is_some() {
            ended.push(run);
        } else {
            live.push(run);
        }
    }
    let newest_first = |a: &&ExternalRunView, b: &&ExternalRunView| {
        (b.updated_at, &b.id).cmp(&(a.updated_at, &a.id))
    };
    live.sort_by(newest_first);
    ended.sort_by(newest_first);
    live.into_iter()
        .chain(ended.into_iter().take(ENDED_RUNS_SHOWN))
        .map(|run| TrackerRun {
            item_title: snapshot
                .item(&run.work_item_id)
                .map(|item| item.title.clone())
                .unwrap_or_default(),
            run: run.clone(),
        })
        .collect()
}

/// What this device observed about a live run, in words. `None` for an ended
/// run and before the first answer, so the recorded label stands.
pub(super) fn observed_label(
    run: &ExternalRunView,
    observed: Option<&ExternalRunsObserved>,
    now_s: i64,
) -> Option<String> {
    if run.ended_at.is_some() {
        return None;
    }
    let observed = observed?;
    if !observed.hosted {
        return Some("not observed on this device".into());
    }
    let Some(seen) = observed.runs.get(&run.id) else {
        return Some("unknown · not yet observed".into());
    };
    let age = format_age((now_s - seen.since).max(0) as u64);
    Some(format!("{} · {age}", seen.state.word()))
}

/// The reason line under a run, from the observation.
pub(super) fn observed_reason(run: &ExternalRunView, observed: Option<&ExternalRunsObserved>) -> Option<String> {
    let seen = observed.filter(|o| o.hosted)?.runs.get(&run.id)?;
    (!seen.why.is_empty() && run.ended_at.is_none()).then(|| seen.why.clone())
}

/// One header line: capacity, and why observation is blind when it is.
pub(super) fn capacity_header(observed: Option<&ExternalRunsObserved>) -> Option<String> {
    let observed = observed.filter(|o| o.hosted)?;
    let mut parts = Vec::new();
    if let Some(capacity) = observed.capacity {
        parts.push(format!("Capacity {}", capacity.line()));
    }
    parts.extend(observed.gh_problem.clone());
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// What the record says about the run, in words.
pub(super) fn run_state_label(run: &ExternalRunView) -> String {
    match (run.ended_at, run.outcome) {
        (Some(_), Some(ExternalRunOutcome::Done)) => "ended: done".into(),
        (Some(_), Some(ExternalRunOutcome::Failed)) => "ended: failed".into(),
        (Some(_), Some(ExternalRunOutcome::Abandoned)) => "ended: abandoned".into(),
        (Some(_), None) => "ended".into(),
        (None, _) if run.expected_alive => "expected alive".into(),
        (None, _) => "not expected alive".into(),
    }
}

fn run_title(run: &ExternalRunView) -> String {
    run.display_name
        .as_deref()
        .or(run.codespace_name.as_deref())
        .unwrap_or(&run.run_tag)
        .to_owned()
}

impl Shell {
    /// Follow the engine's observed-run revision and re-read this device's
    /// observation of the active Space whenever it moves. The engine's poller
    /// does the polling; this only reconnects a broken stream.
    pub(super) fn watch_external_runs(&mut self, cx: &mut Context<Self>) {
        let Some((_, _, Some(space), _)) = self.tracker.chat_scope.clone() else {
            self.tracker.observed_task = None;
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.tracker.observed_task = Some(cx.spawn(async move |shell, cx| {
            let mut attempt = 0;
            loop {
                let mut subscription = match engine
                    .client()
                    .subscribe_checked(mast_rpc::methods::WATCH_EXTERNAL_RUNS, serde_json::json!({}))
                    .await
                {
                    Ok(subscription) => subscription,
                    Err(mast_rpc::RpcError::UnknownMethod(_)) => return,
                    Err(_) => {
                        let delay = mast_harness::backoff::delay_for_attempt(std::time::Duration::from_secs(5), std::time::Duration::from_secs(300), attempt);
                        attempt = attempt.saturating_add(1);
                        cx.background_executor().timer(mast_harness::backoff::jittered(delay)).await;
                        continue;
                    }
                };
                while subscription.recv().await.is_some() {
                    attempt = 0;
                    let reply = engine
                        .client()
                        .call(mast_rpc::methods::EXTERNAL_RUNS_OBSERVED, serde_json::json!({ "spaceId": space }))
                        .await;
                    let Some(observed) = reply.ok().and_then(|value| serde_json::from_value::<ExternalRunsObserved>(value).ok()) else { continue };
                    let space = space.clone();
                    if shell.update(cx, |shell, cx| {
                        shell.tracker.observed_runs = Some((space, observed));
                        cx.notify();
                    }).is_err() { return; }
                }
                let delay = mast_harness::backoff::delay_for_attempt(std::time::Duration::from_secs(5), std::time::Duration::from_secs(300), attempt);
                attempt = attempt.saturating_add(1);
                cx.background_executor().timer(mast_harness::backoff::jittered(delay)).await;
            }
        }));
    }

    /// The runs of the project the active chat coordinates, from the current
    /// outline snapshot.
    pub(super) fn tracker_runs_for(&self, chat_id: &str, cx: &App) -> Vec<TrackerRun> {
        let Some((_, _, Some(space), _)) = self.tracker.chat_scope.as_ref() else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        let outline = state.work_outline(space);
        let Some(snapshot) = outline.snapshot() else {
            return Vec::new();
        };
        tracker_runs(snapshot, chat_id)
    }

    pub(super) fn render_tracker_runs(
        &self,
        runs: &[TrackerRun],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let observed = self.tracker.observed_runs.as_ref().map(|(_, observed)| observed);
        let now_s = chrono::Utc::now().timestamp();
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_XS))
            .debug_selector(|| "tracker-runs".into())
            .child(self.tracker_section_heading("runs", "Runs", runs.len(), false, theme, cx));
        let collapsed = self.tracker_section_collapsed("runs", false);
        if !collapsed && let Some(header) = capacity_header(observed) {
            section = section.child(
                div()
                    .debug_selector(|| "tracker-runs-capacity".into())
                    .child(crate::components::shell_parts::tracker_metadata(theme, header)),
            );
        }
        let shown = if collapsed { &[][..] } else { runs };
        for entry in shown {
            let run = &entry.run;
            let mut detail = vec![run.kind.clone()];
            detail.extend(run.provider.clone());
            detail.extend(run.branch.clone());
            if !entry.item_title.is_empty() {
                detail.push(entry.item_title.clone());
            }
            detail.extend(observed_reason(run, observed));
            section = section.child(
                crate::components::shell_parts::tracker_row(
                    theme,
                    format!("tracker-run-{}", run.id),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(crate::components::shell_parts::worker_title(
                            theme,
                            run_title(run),
                        ))
                        .child(crate::components::shell_parts::tracker_metadata(
                            theme,
                            observed_label(run, observed, now_s).unwrap_or_else(|| run_state_label(run)),
                        )),
                )
                .child(crate::components::shell_parts::tracker_metadata(
                    theme,
                    detail.join(" · "),
                )),
            );
        }
        section
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, item: &str, updated_at: i64, ended_at: Option<i64>) -> ExternalRunView {
        ExternalRunView {
            id: id.into(),
            space_id: "s".into(),
            work_item_id: item.into(),
            kind: "codespace".into(),
            run_tag: id.into(),
            codespace_name: None,
            display_name: None,
            provider: None,
            session_id: None,
            branch: None,
            owner_chat_id: None,
            created_at: 0,
            updated_at,
            expected_alive: ended_at.is_none(),
            ended_at,
            outcome: None,
        }
    }

    #[test]
    fn the_state_label_reads_the_record() {
        let mut ended = run("r", "i", 1, Some(2));
        assert_eq!(run_state_label(&ended), "ended");
        ended.outcome = Some(ExternalRunOutcome::Failed);
        assert_eq!(run_state_label(&ended), "ended: failed");
        let mut live = run("r", "i", 1, None);
        assert_eq!(run_state_label(&live), "expected alive");
        live.expected_alive = false;
        assert_eq!(run_state_label(&live), "not expected alive");
    }

    fn observed(hosted: bool) -> ExternalRunsObserved {
        use mast_proto::{Capacity, RunObservation, RunState};
        ExternalRunsObserved {
            hosted,
            capacity: Some(Capacity { running: 3, cap: 5, total: 6, box_limit: None, at: 0 }),
            gh_problem: None,
            runs: [(
                "r".to_owned(),
                RunObservation {
                    state: RunState::Orphaned,
                    since: 100,
                    seen_at: 200,
                    why: "no exit seen; box shut down".into(),
                    exit_code: None,
                    idle_s: None,
                },
            )]
            .into(),
        }
    }

    #[test]
    fn observed_state_replaces_the_recorded_label_for_a_live_run_only() {
        let live = run("r", "i", 1, None);
        assert_eq!(observed_label(&live, None, 5_000), None);
        assert_eq!(
            observed_label(&live, Some(&observed(true)), 7_900).as_deref(),
            Some("orphaned · 2h10m")
        );
        assert_eq!(
            observed_reason(&live, Some(&observed(true))).as_deref(),
            Some("no exit seen; box shut down")
        );
        assert_eq!(
            observed_label(&live, Some(&observed(false)), 0).as_deref(),
            Some("not observed on this device")
        );
        let other = run("other", "i", 1, None);
        assert_eq!(
            observed_label(&other, Some(&observed(true)), 0).as_deref(),
            Some("unknown · not yet observed")
        );
        let ended = run("r", "i", 1, Some(2));
        assert_eq!(observed_label(&ended, Some(&observed(true)), 0), None);
        assert_eq!(observed_reason(&ended, Some(&observed(true))), None);
    }

    #[test]
    fn the_capacity_header_shows_only_for_a_hosting_device() {
        assert_eq!(
            capacity_header(Some(&observed(true))).as_deref(),
            Some("Capacity 3/5 running, 6 boxes")
        );
        assert_eq!(capacity_header(Some(&observed(false))), None);
        assert_eq!(capacity_header(None), None);
        let mut blind = observed(true);
        blind.capacity = None;
        blind.gh_problem = Some("gh not signed in".into());
        assert_eq!(capacity_header(Some(&blind)).as_deref(), Some("gh not signed in"));
    }

    #[test]
    fn a_run_is_titled_by_display_name_then_codespace_then_tag() {
        let mut r = run("tag-1", "i", 1, None);
        assert_eq!(run_title(&r), "tag-1");
        r.codespace_name = Some("box".into());
        assert_eq!(run_title(&r), "box");
        r.display_name = Some("Nice name".into());
        assert_eq!(run_title(&r), "Nice name");
    }
}
