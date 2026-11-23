//! The project queue in the Tracker: the ordered "Queue" rows and the pause
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
    /// Each chat mapped to the item it is assigned to, so a not-queued worker
    /// row knows the item its Add would queue.
    pub item_for_chat: std::collections::HashMap<String, String>,
    /// Each chat assigned to a live queued item or to a descendant of one,
    /// mapped to that queued item: those workers show in its queue entry.
    pub queued_owner_of_chat: std::collections::HashMap<String, String>,
}

impl TrackerQueue {
    /// Whether a work item has a live row in the queue.
    pub fn holds_item(&self, item_id: &str) -> bool {
        self.live().any(|entry| entry.work_item_id == item_id)
    }

    /// The queued item whose entry a worker's chat belongs to: its own item or
    /// the nearest queued ancestor of the item it works on.
    pub fn queued_item_of_chat(&self, chat_id: &str) -> Option<&str> {
        self.queued_owner_of_chat.get(chat_id).map(String::as_str)
    }

    /// Entries still waiting or running: what the Queue lists.
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
    // The live descendants of the project, found by walking down from it: the
    // items the queue accepts, so only their workers get an Add button.
    let mut item_for_chat = std::collections::HashMap::new();
    let mut pending: Vec<&str> = project.child_ids.iter().map(String::as_str).collect();
    while let Some(id) = pending.pop() {
        let Some(node) = snapshot.item(id).filter(|node| node.archived_at.is_none()) else {
            continue;
        };
        item_for_chat.extend(node.chat_ids.iter().map(|chat| (chat.clone(), node.id.clone())));
        pending.extend(node.child_ids.iter().map(String::as_str));
    }
    let queued: std::collections::HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.state != QueueEntryState::Done)
        .map(|entry| entry.work_item_id.as_str())
        .collect();
    let mut queued_owner_of_chat = std::collections::HashMap::new();
    for &root in &queued {
        // A descendant that is queued itself keeps its own entry and its own workers.
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let Some(node) = snapshot.item(id) else { continue };
            for chat in &node.chat_ids {
                queued_owner_of_chat.insert(chat.clone(), root.to_owned());
            }
            pending.extend(
                node.child_ids.iter().map(String::as_str).filter(|id| !queued.contains(id)),
            );
        }
    }
    let queue = TrackerQueue {
        project_id: project.id.clone(),
        paused: project.queue_paused,
        entries,
        item_for_chat,
        queued_owner_of_chat,
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

    /// The Queue heading with its pause toggle, and the notices for a paused
    /// or held queue. The rows follow it in the same list.
    fn render_tracker_queue_header(
        &self,
        queue: &TrackerQueue,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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
                    "Queue",
                    queue.live().count(),
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
        section
    }

    /// The Queue: its header, then one row per queued entry. Each row is the
    /// same row the Tracker shows for any other work, the worker's row or the
    /// item's own, with the queue controls on its header.
    pub(super) fn render_tracker_queue(
        &self,
        queue: &TrackerQueue,
        team: &TeamFacts,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let live: Vec<&QueueEntry> = queue.live().collect();
        let mut section = self.render_tracker_queue_header(queue, theme, cx);
        let reports = self
            .wakes
            .statuses
            .get(chat_id)
            .map(crate::orchestrator_wakes::latest_reports)
            .unwrap_or_default();
        let unseen = self
            .wakes
            .statuses
            .get(chat_id)
            .map(crate::orchestrator_wakes::unseen_by_worker)
            .unwrap_or_default();
        let filter = self.tracker.filter;
        for (ix, entry) in live.iter().enumerate() {
            let controls = self.queue_controls(&live, ix, theme, cx);
            let owned = |worker: &&WorkerFacts| {
                queue.queued_item_of_chat(&worker.chat_id) == Some(entry.work_item_id.as_str())
                    && self
                        .tracker
                        .lifecycle_rows
                        .get(&worker.chat_id)
                        .is_none_or(|row| tracker_matches(row, filter))
            };
            // The workers on the queued item itself come first, then those of its
            // descendants, each as the ordinary worker row.
            let (own, descendant): (Vec<&WorkerFacts>, Vec<&WorkerFacts>) =
                team.live().filter(owned).partition(|worker| {
                    queue.item_for_chat.get(&worker.chat_id) == Some(&entry.work_item_id)
                });
            let item = LifecycleStage::ALL.into_iter().find_map(|stage| {
                let items = self.tracker.unassigned_groups.get(&(TrackerFilter::All, stage))?;
                items
                    .iter()
                    .find(|item| item.id == entry.work_item_id)
                    .map(|item| (item, stage))
            });
            let item_matches = self
                .tracker
                .unassigned_rows
                .get(&entry.work_item_id)
                .is_none_or(|row| tracker_matches(row, filter));
            let mut controls = Some(controls);
            // The controls ride the queued item's own row: its worker, else its item
            // row, else a plain row. A descendant's worker never carries them, so
            // under a filter that hides the item's own row the controls are not shown.
            if own.is_empty() {
                match item {
                    Some((item, stage)) if item_matches => {
                        section = section.child(self.render_unassigned_tracker_item(
                            item,
                            stage,
                            controls.take(),
                            theme,
                            cx,
                        ));
                    }
                    // Without a lifecycle row, such as an item whose PR status is
                    // still unavailable, the entry keeps its place in a plain row.
                    None if filter == TrackerFilter::All => {
                        section = section.child(
                            crate::components::shell_parts::tracker_row(
                                theme,
                                format!("tracker-queue-item-{}", entry.work_item_id),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(Theme::SPACE_SM))
                                    .child(crate::components::shell_parts::worker_title(
                                        theme,
                                        entry.title.clone(),
                                    ))
                                    .children(controls.take()),
                            )
                            .into_any_element(),
                        );
                    }
                    _ => {}
                }
            }
            let own_count = own.len();
            for (index, worker) in own.into_iter().chain(descendant).enumerate() {
                section = section.child(self.render_tracker_worker(
                    worker,
                    reports.get(&worker.chat_id),
                    unseen.get(&worker.chat_id),
                    controls.take().filter(|_| index == 0 && own_count > 0),
                    theme,
                    cx,
                ));
            }
        }
        section
    }

    /// Position, state, Up, Down and Remove for one queued entry. Each button
    /// stops the click so the row's own click, opening its chat, does not fire.
    fn queue_controls(
        &self,
        live: &[&QueueEntry],
        ix: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = live[ix];
        let id = entry.work_item_id.clone();
        let up_to = ix.checked_sub(1).map(|i| live[i].work_item_id.clone());
        let down_to = live.get(ix + 1).map(|next| next.work_item_id.clone());
        let (up_id, down_id, remove_id) = (id.clone(), id.clone(), id.clone());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .ml_auto()
            .debug_selector({
                let id = id.clone();
                move || format!("tracker-queue-row-{id}")
            })
            .child(crate::components::shell_parts::tracker_metadata(
                theme,
                format!("#{} · {}", ix + 1, state_label(entry)),
            ))
            .child(
                team_button(theme, format!("tracker-queue-up-{ix}"), "Up".into(), false).on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
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
                    }),
                ),
            )
            .child(
                team_button(theme, format!("tracker-queue-down-{ix}"), "Down".into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
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
                    })),
            )
            .child(
                team_button(theme, format!("tracker-queue-remove-{ix}"), "Remove".into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_tracker_queue(remove_id.clone(), QueueChange::Remove, cx)
                    })),
            )
            .into_any_element()
    }

    /// The Add button on a row that is not queued: it joins the end of the queue.
    pub(super) fn queue_add_control(
        &self,
        item_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_none()
            .ml_auto()
            .child(
                team_button(theme, format!("tracker-queue-add-{item_id}"), "Add".into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_tracker_queue(
                            item_id.clone(),
                            QueueChange::Place { placement: WorkOutlinePlacement::Last },
                            cx,
                        );
                    })),
            )
            .into_any_element()
    }
}
