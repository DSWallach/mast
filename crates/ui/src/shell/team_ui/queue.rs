//! The project queue in the Tracker: the ordered "Up next" rows and the pause
//! toggle. The entries come from `WorkOutlineSnapshot::work_queue`, the same
//! derivation the Manager's auto-start reads, so a merged PR leaves this list
//! exactly when the dispatcher stops offering it.
use super::*;
use mast_engine::pr_ticket_cache::PrStatus;
use mast_proto::work_status::PullRequestFacts;
use mast_proto::{QueueChange, QueueEntry, QueueEntryState, WorkOutlinePlacement};

pub(super) struct TrackerQueue {
    pub project_id: String,
    pub paused: bool,
    /// Every queued entry in order, `Done` ones included.
    pub entries: Vec<QueueEntry>,
}

impl TrackerQueue {
    /// Entries still waiting or running: what "Up next" lists.
    pub fn live(&self) -> impl Iterator<Item = &QueueEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.state != QueueEntryState::Done)
    }
}

/// Facts of a linked pull request from the Tracker's per-item PR cache. A PR
/// the cache has not answered for stays unknown.
fn pr_facts<'a>(
    item_prs: &'a std::collections::HashMap<String, Vec<PrStatus>>,
) -> impl Fn(&str) -> PullRequestFacts + 'a {
    move |url| {
        item_prs
            .values()
            .flatten()
            .find(|pr| {
                pr.url
                    .as_deref()
                    .is_some_and(|known| known.eq_ignore_ascii_case(url))
            })
            .map(|pr| PullRequestFacts {
                state_known: !pr.state.is_empty(),
                open: pr.state.eq_ignore_ascii_case("open"),
                merged: pr.state.eq_ignore_ascii_case("merged"),
                ..PullRequestFacts::default()
            })
            .unwrap_or_default()
    }
}

/// The queue of the project the chat works in, when it has one worth showing:
/// something live is queued, or auto-start is paused.
pub(super) fn tracker_queue(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    chat_id: &str,
    item_prs: &std::collections::HashMap<String, Vec<PrStatus>>,
    now_ms: i64,
) -> Option<TrackerQueue> {
    let item = snapshot
        .items
        .iter()
        .find(|item| item.chat_ids.iter().any(|id| id == chat_id))?;
    let project = snapshot.project_root(&item.id)?;
    let entries = snapshot.work_queue(&project.id, now_ms, &pr_facts(item_prs));
    let queue = TrackerQueue {
        project_id: project.id.clone(),
        paused: project.queue_paused,
        entries,
    };
    (queue.paused || queue.live().next().is_some()).then_some(queue)
}

fn state_label(entry: &QueueEntry) -> &'static str {
    match (entry.state, entry.facts_pending) {
        (QueueEntryState::Ready, true) => "checking PR",
        (QueueEntryState::Ready, false) => "ready",
        (QueueEntryState::Running, _) => "running",
        (QueueEntryState::Blocked, _) => "blocked",
        (QueueEntryState::Done, _) => "done",
    }
}

impl Shell {
    /// The queue of the project the active chat belongs to, from the current
    /// outline snapshot.
    pub(super) fn tracker_queue_for(&self, chat_id: &str, cx: &App) -> Option<TrackerQueue> {
        let (_, _, space, _) = self.tracker.chat_scope.as_ref()?;
        let state = self.state.read(cx);
        let snapshot = state.work_outline(space.as_deref()?).snapshot()?.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        tracker_queue(&snapshot, chat_id, &self.tracker.health_item_prs, now_ms)
    }

    fn set_tracker_queue(&mut self, item_id: String, change: QueueChange, cx: &mut Context<Self>) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_queue_position(mast_proto::SetQueuePositionParams {
                work_item_id: item_id,
                change,
                operation_id: crate::work_outline::actions::new_operation_id(),
            });
        self.finish_tracker_queue_task(task, cx);
    }

    fn set_tracker_queue_paused(
        &mut self,
        project_id: String,
        paused: bool,
        cx: &mut Context<Self>,
    ) {
        let task = self.state.read(cx).work_outline_actions().set_queue_paused(
            mast_proto::SetQueuePausedParams {
                work_item_id: project_id,
                paused,
                operation_id: crate::work_outline::actions::new_operation_id(),
            },
        );
        self.finish_tracker_queue_task(task, cx);
    }

    fn finish_tracker_queue_task(
        &mut self,
        task: crate::work_outline::actions::ActionTask,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            let _ = shell.update(cx, |shell, cx| {
                if let Err(error) = result {
                    shell.sidebar_notice = Some(error.message.into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_tracker_queue(
        &self,
        queue: &TrackerQueue,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let live: Vec<&QueueEntry> = queue.live().collect();
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_XS))
            .debug_selector(|| "tracker-queue".into());
        let paused = queue.paused;
        let project_id = queue.project_id.clone();
        section = section.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(crate::components::shell_parts::tracker_group_heading(
                    theme,
                    "Up next",
                    live.len(),
                ))
                .child(
                    team_button(
                        theme,
                        "tracker-queue-pause-toggle",
                        if paused {
                            "Resume auto-start"
                        } else {
                            "Pause auto-start"
                        }
                        .into(),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_tracker_queue_paused(project_id.clone(), !paused, cx)
                    })),
                ),
        );
        if paused {
            section = section.child(
                crate::components::shell_parts::tracker_metadata(
                    theme,
                    "Auto-start is paused: the Manager is not offered queued work. A running worker continues.".into(),
                )
                .debug_selector(|| "tracker-queue-paused".into()),
            );
        }
        for (ix, entry) in live.iter().enumerate() {
            let id = entry.work_item_id.clone();
            let up_to = ix.checked_sub(1).map(|i| live[i].work_item_id.clone());
            let down_to = live.get(ix + 1).map(|next| next.work_item_id.clone());
            let (up_id, down_id, remove_id) = (id.clone(), id.clone(), id.clone());
            section = section.child(
                crate::components::shell_parts::tracker_row(
                    theme,
                    format!("tracker-queue-row-{id}"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(crate::components::shell_parts::worker_title(
                            theme,
                            format!("{}. {}", ix + 1, entry.title),
                        ))
                        .child(crate::components::shell_parts::tracker_metadata(
                            theme,
                            state_label(entry).into(),
                        ))
                        .child(
                            team_button(
                                theme,
                                format!("tracker-queue-up-{ix}"),
                                "Up".into(),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(anchor) = up_to.clone() {
                                        this.set_tracker_queue(
                                            up_id.clone(),
                                            QueueChange::Place {
                                                placement: WorkOutlinePlacement::Before {
                                                    sibling_work_item_id: anchor,
                                                },
                                            },
                                            cx,
                                        );
                                    }
                                },
                            )),
                        )
                        .child(
                            team_button(
                                theme,
                                format!("tracker-queue-down-{ix}"),
                                "Down".into(),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(anchor) = down_to.clone() {
                                        this.set_tracker_queue(
                                            down_id.clone(),
                                            QueueChange::Place {
                                                placement: WorkOutlinePlacement::After {
                                                    sibling_work_item_id: anchor,
                                                },
                                            },
                                            cx,
                                        );
                                    }
                                },
                            )),
                        )
                        .child(
                            team_button(
                                theme,
                                format!("tracker-queue-remove-{ix}"),
                                "Remove".into(),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.set_tracker_queue(
                                        remove_id.clone(),
                                        QueueChange::Remove,
                                        cx,
                                    )
                                },
                            )),
                        ),
                ),
            );
        }
        section
    }
}
