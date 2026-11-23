//! The runs recorded on the coordinator's items: work that executes outside a
//! local harness, such as a codespace. This lists the durable record only (what
//! was registered, whether it is still expected alive, how it ended); nothing
//! here is observed or fetched.
use super::*;
use mast_proto::{ExternalRunOutcome, ExternalRunView};

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

    pub(super) fn render_tracker_runs(&self, runs: &[TrackerRun], theme: &Theme) -> gpui::Div {
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_XS))
            .debug_selector(|| "tracker-runs".into())
            .child(crate::components::shell_parts::tracker_group_heading(
                theme,
                "Runs",
                runs.len(),
            ));
        for entry in runs {
            let run = &entry.run;
            let mut detail = vec![run.kind.clone()];
            detail.extend(run.provider.clone());
            detail.extend(run.branch.clone());
            if !entry.item_title.is_empty() {
                detail.push(entry.item_title.clone());
            }
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
                            run_state_label(run),
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
