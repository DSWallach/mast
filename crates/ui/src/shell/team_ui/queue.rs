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

    /// The entry the dispatcher is waiting on: the first ready one, when its
    /// pull request has no answer yet. Nothing is offered until it does.
    pub fn held(&self) -> Option<&QueueEntry> {
        self.live()
            .find(|entry| entry.state == QueueEntryState::Ready)
            .filter(|entry| entry.facts_pending)
    }
}

/// Facts of a linked pull request from the Tracker's per-item PR cache. A PR
/// the cache has not answered for stays unknown; a URL no parser accepts is
/// known not to be merged.
fn pr_facts(
    item_prs: &std::collections::HashMap<String, Vec<PrStatus>>,
) -> impl Fn(&str) -> PullRequestFacts {
    // Keyed by `owner/repo#number` through the parser the engine's dispatcher
    // uses, so a trailing path, fragment or scheme-less URL finds the same
    // answer in both places.
    let key = |url: &str| {
        mast_engine::work_outline_pr_links::pull_request_from_url(url).map(|pr| {
            format!("{}#{}", mast_engine::work_outline_pr_links::repo_slug(&pr), pr.number)
        })
    };
    let known: std::collections::HashMap<String, PullRequestFacts> = item_prs
        .values()
        .flatten()
        .filter_map(|pr| {
            let facts = PullRequestFacts {
                state_known: !pr.state.is_empty(),
                open: pr.state.eq_ignore_ascii_case("open"),
                merged: pr.state.eq_ignore_ascii_case("merged"),
                ..PullRequestFacts::default()
            };
            Some((key(pr.url.as_deref()?)?, facts))
        })
        .collect();
    move |url| match key(url) {
        Some(key) => known.get(&key).copied().unwrap_or_default(),
        // No lookup can answer a URL the parser rejects, so it is known not
        // to be merged, as the dispatcher treats it, and cannot hold the queue.
        None => PullRequestFacts { state_known: true, ..PullRequestFacts::default() },
    }
}

/// The queue of the project the chat works in, when it has one worth showing:
/// something live is queued, or auto-start is paused.
pub(super) fn tracker_queue(
    snapshot: &mast_proto::WorkOutlineSnapshot,
    chat_id: &str,
    item_prs: &std::collections::HashMap<String, Vec<PrStatus>>,
    chats: mast_proto::ChatLookup<'_>,
    now_ms: i64,
) -> Option<TrackerQueue> {
    let item = snapshot
        .items
        .iter()
        .find(|item| item.chat_ids.iter().any(|id| id == chat_id))?;
    // The PR cache covers the chat's own subtree. Only the project's own chat
    // (its Manager) sees the whole queue, so a chat further down shows none
    // rather than entries it has no facts for.
    let project = snapshot.project_root(&item.id).filter(|project| project.id == item.id)?;
    let entries = snapshot.work_queue(&project.id, now_ms, &pr_facts(item_prs), chats);
    let queue = TrackerQueue {
        project_id: project.id.clone(),
        paused: project.queue_paused,
        entries,
    };
    (queue.paused || queue.live().next().is_some()).then_some(queue)
}

fn state_label(entry: &QueueEntry) -> String {
    match (entry.state, entry.facts_pending) {
        (QueueEntryState::Ready, true) => "waiting for PR status".into(),
        (QueueEntryState::Ready, false) => "ready".into(),
        (QueueEntryState::Running, _) => "running".into(),
        (QueueEntryState::Blocked, _) => "blocked".into(),
        (QueueEntryState::Stalled, _) => match entry.stall {
            Some(stall) => format!("stalled: {}", crate::queue_activity::stall_text(stall)),
            None => "stalled".into(),
        },
        (QueueEntryState::Done, _) => "done".into(),
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
        let chats = crate::queue_activity::chat_activity_lookup(&state.chats, &state.sessions);
        tracker_queue(&snapshot, chat_id, &self.tracker.health_item_prs, &chats, now_ms)
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
                by: None,
                reason: None,
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
        if let Some(held) = queue.held() {
            section = section.child(
                crate::components::shell_parts::tracker_metadata(
                    theme,
                    format!(
                        "Held: waiting for the PR status of \"{}\". Nothing is offered until it answers; Remove clears it from the queue.",
                        held.title
                    ),
                )
                .debug_selector(|| "tracker-queue-held".into()),
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
                            state_label(entry),
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
