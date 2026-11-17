//! Board and table presentation for the outline.
use super::*;
use crate::board::{self, BoardItem, BoardStatus};
use crate::team::{TeamInput, WorkerState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardMode {
    Tree,
    Board,
    Table,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardSort {
    Title,
    Status,
    Worker,
    Branch,
    Pr,
    Diff,
    Age,
    Action,
}

impl OutlineView {
    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_board_mode(&mut self, table: bool) {
        self.board_mode = if table {
            BoardMode::Table
        } else {
            BoardMode::Board
        };
        self.board_space_scope = true;
        self.board_links.insert(
            "reviewer".into(),
            mast_engine::pr_ticket_cache::ChatLinkStatus {
                pr: Some(mast_engine::pr_ticket_cache::PrStatus {
                    number: 41,
                    url: None,
                    state: "OPEN".into(),
                    is_draft: false,
                    review_decision: Some("APPROVED".into()),
                    reviewers: vec![],
                    checks: Some(mast_engine::pr_ticket_cache::ChecksStatus::Passing),
                    title: None,
                    branch: None,
                    head_sha: None,
                    mergeable: "mergeable".into(),
                    has_reviewer_requested: false,
                    merged_at: None,
                }),
                diff_stat: Some(mast_engine::pr_ticket_cache::DiffStat {
                    files_changed: 3,
                    lines_added: 42,
                    lines_removed: 8,
                }),
                ..Default::default()
            },
        );
    }

    pub(crate) fn suspend_board(&mut self, cx: &mut Context<Self>) {
        if self.board_visible {
            self.board_visible = false;
            if let Some(shell) = self.shell.as_ref().and_then(|shell| shell.upgrade()) {
                shell.update(cx, |shell, _| shell.cancel_board_links());
            }
            self.state.update(cx, |state, cx| {
                state.set_board_watch_docs(Default::default(), cx)
            });
        }
    }

    pub(super) fn refresh_board_watches(&mut self, cx: &mut Context<Self>) {
        if self.board_mode != BoardMode::Tree && self.board_visible {
            self.watch_board_links(cx);
            self.watch_board_activity(cx);
        }
    }

    fn refresh_board_data(&mut self, cx: &mut Context<Self>) {
        if self.board_mode != BoardMode::Tree && self.board_visible {
            self.watch_board_links(cx);
            self.watch_board_activity(cx);
        }
    }

    fn watch_board_links(&mut self, cx: &mut Context<Self>) {
        let Some(shell) = self.shell.as_ref().and_then(|shell| shell.upgrade()) else {
            return;
        };
        let ids = board_link_candidates(
            &self.board_rows,
            &self.board_groups,
            self.board_mode,
            self.board_limit,
            &self.board_scroll,
            &self.board_column_scroll,
        );
        shell.update(cx, |shell, cx| shell.request_board_links(&ids, cx));
        let revision = shell.read(cx).board_links_revision();
        if revision != self.board_link_revision {
            self.board_link_revision = revision;
            let versions = shell.read(cx).board_link_versions(&self.board_cached_ids);
            if versions != self.board_link_versions {
                self.board_link_versions = versions;
                self.board_links = shell.read(cx).board_link_statuses(&self.board_cached_ids);
                self.board_rows_dirty = true;
                cx.notify();
            }
        }
        if self.board_shell_observe.is_none() {
            self.board_shell_observe = Some(cx.observe(&shell, |this, shell, cx| {
                if this.board_mode == BoardMode::Tree || !this.board_visible {
                    return;
                }
                let shell = shell.read(cx);
                let revision = shell.board_links_revision();
                if revision == this.board_link_revision {
                    return;
                }
                this.board_link_revision = revision;
                let versions = shell.board_link_versions(&this.board_cached_ids);
                if versions != this.board_link_versions {
                    this.board_link_versions = versions;
                    this.board_links = shell.board_link_statuses(&this.board_cached_ids);
                    this.board_rows_dirty = true;
                    cx.notify();
                }
            }));
        }
    }

    fn watch_board_activity(&mut self, cx: &mut Context<Self>) {
        let visible = board_link_candidates(
            &self.board_rows,
            &self.board_groups,
            self.board_mode,
            self.board_limit,
            &self.board_scroll,
            &self.board_column_scroll,
        );
        let ids: std::collections::HashSet<_> = visible
            .into_iter()
            .filter(|id| self.board_worker_states.get(id) == Some(&WorkerState::Working))
            .collect();
        let mut activity_ids: Vec<_> = ids.iter().cloned().collect();
        activity_ids.sort();
        if self.board_activity_ids != activity_ids {
            self.board_activity_ids = activity_ids;
            self.board_activity.retain(|id, _| ids.contains(id));
        }
        self.state
            .update(cx, |state, cx| state.set_board_watch_docs(ids, cx));
        if self.board_state_observe.is_none() {
            self.board_status_revision = self.state.read(cx).board_status_revision;
            self.board_state_observe = Some(cx.observe(&self.state, |this, state, cx| {
                if this.board_mode == BoardMode::Tree || !this.board_visible {
                    return;
                }
                let (status_revision, transcript_revision, artifact_changed, activity) = {
                    let state = state.read(cx);
                    let mut activity = None;
                    if state.transcript_revision != this.board_transcript_revision {
                        let mut lines = std::collections::HashMap::new();
                        for id in &this.board_activity_ids {
                            let call = state
                                .sub_transcript(id)
                                .iter()
                                .rev()
                                .flat_map(|message| message.parts.iter().rev())
                                .find_map(|part| match part {
                                    mast_doc::MessagePart::Tool {
                                        call,
                                        resolved: false,
                                        ..
                                    } => Some(call),
                                    _ => None,
                                });
                            if let Some(line) = board::activity(call, None) {
                                lines.insert(id.clone(), line);
                            }
                        }
                        activity = Some(lines);
                    }
                    (
                        state.board_status_revision,
                        state.transcript_revision,
                        this.board_artifact_key(state) != this.board_artifact_revision,
                        activity,
                    )
                };
                if status_revision != this.board_status_revision {
                    this.board_status_revision = status_revision;
                    this.board_rows_dirty = true;
                    let view = cx.weak_entity();
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |this, cx| this.watch_board_activity(cx));
                    });
                    cx.notify();
                }
                this.board_transcript_revision = transcript_revision;
                if artifact_changed {
                    cx.notify();
                }
                if let Some(activity) = activity
                    && activity != this.board_activity
                {
                    this.board_activity = activity;
                    cx.notify();
                }
            }));
        }
    }

    pub(super) fn render_board_controls(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(6.0));
        if self.board_mode != BoardMode::Tree {
            let label = self
                .inputs
                .spaces
                .iter()
                .find(|space| self.board_space.as_ref().is_none_or(|id| id == &space.id))
                .map(|space| {
                    self.board_root
                        .as_ref()
                        .filter(|_| !self.board_space_scope)
                        .and_then(|(_, id)| {
                            space.load.snapshot().and_then(|snapshot| snapshot.item(id))
                        })
                        .map(|item| item.title.as_str())
                        .unwrap_or(space.label.as_str())
                })
                .unwrap_or("Space");
            bar = bar.child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Board · {label}")),
            );
        }
        for (mode, label) in [
            (BoardMode::Tree, "Tree"),
            (BoardMode::Board, "Board"),
            (BoardMode::Table, "Table"),
        ] {
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("outline-mode-{label}")))
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(if self.board_mode == mode {
                        theme.surface_raised
                    } else {
                        theme.surface
                    })
                    .text_color(theme.text)
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.board_mode == BoardMode::Tree && mode != BoardMode::Tree {
                            this.board_root = this
                                .selected
                                .as_ref()
                                .and_then(|key| this.model.rows.iter().find(|row| &row.key == key))
                                .and_then(|row| {
                                    row.item()
                                        .map(|item| (item.space_id.clone(), item.item_id.clone()))
                                        .or_else(|| {
                                            row.chat().and_then(|chat| chat.work_item.clone())
                                        })
                                });
                            this.board_space_scope = this.board_root.is_none();
                            this.board_space = this
                                .board_root
                                .as_ref()
                                .map(|(space, _)| space.clone())
                                .or_else(|| this.state.read(cx).selected_space.clone())
                                .or_else(|| {
                                    this.inputs.spaces.first().map(|space| space.id.clone())
                                });
                        }
                        this.board_mode = mode;
                        this.board_visible = mode != BoardMode::Tree;
                        this.board_rows_dirty = true;
                        if mode == BoardMode::Tree {
                            if let Some(shell) =
                                this.shell.as_ref().and_then(|shell| shell.upgrade())
                            {
                                shell.update(cx, |shell, _| shell.cancel_board_links());
                            }
                            this.state.update(cx, |state, cx| {
                                state.set_board_watch_docs(Default::default(), cx)
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        if self.board_mode != BoardMode::Tree {
            for (space, label) in [(false, "Subtree"), (true, "Space")] {
                let enabled = space || self.board_root.is_some();
                bar = bar.child(
                    div()
                        .id(SharedString::from(format!("board-scope-{label}")))
                        .debug_selector(move || format!("board-scope-{label}"))
                        .px(px(6.0))
                        .py(px(4.0))
                        .text_color(if enabled {
                            theme.text
                        } else {
                            theme.text_faint
                        })
                        .bg(if self.board_space_scope == space {
                            theme.surface_raised
                        } else {
                            theme.surface
                        })
                        .child(label)
                        .when(enabled, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.board_space_scope = space;
                                    this.board_show_all = false;
                                    this.board_limit = 12;
                                    this.board_rows_dirty = true;
                                    if let Some(shell) =
                                        this.shell.as_ref().and_then(|shell| shell.upgrade())
                                    {
                                        shell.update(cx, |shell, _| shell.cancel_board_links());
                                    }
                                    cx.notify();
                                }))
                        }),
                );
            }
        }
        bar.into_any_element()
    }

    fn board_artifact_key(&self, state: &AppState) -> Option<(String, u64)> {
        let space = self
            .board_space
            .as_deref()
            .or_else(|| self.inputs.spaces.first().map(|space| space.id.as_str()))?;
        state
            .artifacts()
            .snapshot(space)
            .map(|snapshot| (space.to_owned(), snapshot.revision))
    }

    fn refresh_board_artifacts(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let key = self.board_artifact_key(state);
        if key == self.board_artifact_revision {
            return;
        }
        let mut artifacts: std::collections::HashMap<String, Vec<(String, String)>> =
            Default::default();
        if let Some((space, _)) = &key
            && let Some(snapshot) = state.artifacts().snapshot(space)
        {
            for artifact in &snapshot.artifacts {
                if artifact.archived_at.is_none()
                    && let Some(chat) = &artifact.chat_id
                {
                    let entries = artifacts.entry(chat.clone()).or_default();
                    if entries.len() < 5 {
                        entries.push((artifact.id.clone(), artifact.title.clone()));
                    }
                }
            }
        }
        self.board_artifact_revision = key;
        self.board_artifacts = artifacts;
    }

    pub(super) fn render_board(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.refresh_board_artifacts(cx);
        if !self.board_visible {
            self.board_visible = true;
            self.board_rows_dirty = true;
        }
        if self.board_rows_dirty {
            #[cfg(test)]
            {
                self.board_rebuild_count += 1;
            }
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            let indicators = state.board_indicators(now);
            let links = &self.board_links;
            let mut entries = Vec::new();
            for space in &self.inputs.spaces {
                if self.board_space.as_ref().is_some_and(|id| id != &space.id) {
                    continue;
                }
                let Some(snapshot) = space.load.snapshot() else {
                    continue;
                };
                let input = TeamInput {
                    chat_id: "",
                    chats: &state.chats,
                    indicators: &indicators,
                    snapshot: Some(snapshot),
                    links,
                    now,
                };
                let root = if self.board_space_scope {
                    None
                } else {
                    self.board_root.as_ref().map(|(_, id)| id.as_str())
                };
                entries.extend(
                    board::board(snapshot, root, &input)
                        .into_iter()
                        .map(|item| (space.id.clone(), item)),
                );
            }
            if self.board_mode == BoardMode::Table {
                sort_board_rows(&mut entries, self.board_sort, self.board_sort_desc);
            } else {
                entries.sort_by_key(|(_, item)| (item.status, std::cmp::Reverse(item.updated_at)));
            }
            let ids: Vec<String> = entries
                .iter()
                .filter(|(_, item)| item.status != BoardStatus::Done)
                .filter_map(|(_, item)| item.worker.as_ref().map(|worker| worker.chat_id.clone()))
                .collect();
            if self.board_cached_ids != ids {
                self.board_cached_ids = ids;
                self.board_link_versions.clear();
                self.board_link_revision = u64::MAX;
            }
            self.board_groups = std::array::from_fn(|_| Vec::new());
            self.board_worker_states.clear();
            for (index, (_, item)) in entries.iter().enumerate() {
                self.board_groups[item.status as usize].push(index);
                if let Some(worker) = &item.worker {
                    self.board_worker_states
                        .insert(worker.chat_id.clone(), worker.state);
                }
            }
            self.board_rows = entries;
            self.board_rows_dirty = false;
            let view = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |this, cx| this.refresh_board_data(cx));
            });
        }
        let entries = &self.board_rows;
        let mut body = div()
            .id("coordination-board")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.board_scroll)
            .on_scroll_wheel(cx.listener(|_, _, window, cx| {
                let view = cx.weak_entity();
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |this, cx| this.refresh_board_watches(cx));
                });
            }))
            .p(px(10.0));
        if self.board_mode == BoardMode::Table {
            let mut headings = div()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border);
            for (label, width, sort) in [
                ("Title", 220., Some(BoardSort::Title)),
                ("Status", 100., Some(BoardSort::Status)),
                ("Worker", 140., Some(BoardSort::Worker)),
                ("Branch", 140., Some(BoardSort::Branch)),
                ("PR", 140., Some(BoardSort::Pr)),
                ("Diff (+/−)", 85., Some(BoardSort::Diff)),
                ("Age", 70., Some(BoardSort::Age)),
                ("Action", 120., Some(BoardSort::Action)),
            ] {
                let mut heading = div()
                    .id(SharedString::from(format!("board-sort-{label}")))
                    .w(px(width))
                    .flex_none()
                    .child(format!(
                        "{label}{}",
                        if sort == Some(self.board_sort) {
                            if self.board_sort_desc { " ↓" } else { " ↑" }
                        } else {
                            ""
                        }
                    ));
                if let Some(sort) = sort {
                    heading =
                        heading
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.board_sort == sort {
                                    this.board_sort_desc = !this.board_sort_desc;
                                } else {
                                    this.board_sort = sort;
                                    this.board_sort_desc = false;
                                }
                                this.board_rows_dirty = true;
                                cx.notify();
                            }));
                }
                headings = headings.child(heading);
            }
            body = body.child(headings);
            let count = entries.len();
            for (space, item) in entries.iter().take(self.board_limit * 4).cloned() {
                body = body.child(self.board_table_row(space, item, theme, cx));
            }
            if count > self.board_limit * 4 {
                body = body.child(
                    div()
                        .id("board-table-show-more")
                        .cursor_pointer()
                        .child(format!(
                            "Show {} more",
                            (count - self.board_limit * 4).min(48)
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.board_limit += 12;
                            let view = cx.weak_entity();
                            window.on_next_frame(move |_, cx| {
                                let _ = view.update(cx, |this, cx| this.refresh_board_watches(cx));
                            });
                            cx.notify();
                        })),
                );
            }
        } else {
            let mut columns = div().flex().h_full().min_h_0().gap(px(8.0)).items_start();
            for (status, title) in [
                (BoardStatus::Todo, "TO DO"),
                (BoardStatus::InProgress, "IN PROGRESS"),
                (BoardStatus::InReview, "IN REVIEW"),
                (BoardStatus::Done, "DONE"),
            ] {
                let indices = &self.board_groups[status as usize];
                let count = indices.len();
                let items = limit_board_items(
                    indices.iter().map(|index| &entries[*index]),
                    self.board_limit,
                );
                let mut column = div()
                    .id(SharedString::from(format!("board-column-{title}")))
                    .h_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.board_column_scroll[status as usize])
                    .on_scroll_wheel(cx.listener(|_, _, window, cx| {
                        let view = cx.weak_entity();
                        window.on_next_frame(move |_, cx| {
                            let _ = view.update(cx, |this, cx| this.refresh_board_watches(cx));
                        });
                    }))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(format!("{title}  {count}")),
                    );
                if count == 0 {
                    column = column.child(div().text_color(theme.text_muted).child("No items"));
                }
                for (space, item) in items {
                    column = column.child(self.board_card(space, item, false, theme, cx));
                }
                if count > self.board_limit {
                    column = column.child(
                        div()
                            .id(SharedString::from(format!("board-show-more-{title}")))
                            .cursor_pointer()
                            .child(format!("Show {} more", (count - self.board_limit).min(12)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.board_limit += 12;
                                let view = cx.weak_entity();
                                window.on_next_frame(move |_, cx| {
                                    let _ =
                                        view.update(cx, |this, cx| this.refresh_board_watches(cx));
                                });
                                cx.notify();
                            })),
                    );
                }
                columns = columns.child(column);
            }
            body = body.child(columns);
        }
        body.into_any_element()
    }

    fn board_table_row(
        &self,
        space: String,
        item: BoardItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = item.id.clone();
        let title = div()
            .id(SharedString::from(format!("board-row-title-{id}")))
            .w(px(220.))
            .flex_none()
            .cursor_pointer()
            .child(item.title.clone())
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(OutlineEvent::OpenDetails {
                    space_id: space.clone(),
                    item_id: id.clone(),
                })
            }));
        let worker = item.worker.as_ref();
        let status = if worker.is_some_and(|w| w.state == WorkerState::Waiting) {
            "Waiting"
        } else {
            item.status.label()
        };
        let age = if item.created_at <= 0 {
            "—".to_owned()
        } else {
            format!(
                "{}d",
                (chrono::Utc::now().timestamp_millis() - item.created_at).max(0) / 86_400_000
            )
        };
        let mut actions = div().w(px(120.)).flex_none().flex().gap(px(6.));
        if let Some(worker) = worker {
            if worker.state == WorkerState::Waiting
                && let Some(request) = item.request_id.clone()
            {
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("board-answer-{}", item.id)))
                        .debug_selector({
                            let id = item.id.clone();
                            move || format!("board-answer-{id}")
                        })
                        .cursor_pointer()
                        .bg(theme.accent)
                        .text_color(theme.on_accent)
                        .child("Answer")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(OutlineEvent::OpenRequiredAction(request.clone()))
                        })),
                );
            }
            if worker.branch.is_some()
                || !worker.prs.is_empty()
                || !self
                    .state
                    .read(cx)
                    .chat_diff_targets(&worker.chat_id)
                    .is_empty()
            {
                let chat = worker.chat_id.clone();
                let branch = worker.branch.clone();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("board-review-{}", item.id)))
                        .cursor_pointer()
                        .child("Review")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(shell) = this.shell.as_ref().and_then(|s| s.upgrade()) {
                                shell.update(cx, |shell, cx| {
                                    shell.open_request_diff(chat.clone(), branch.clone(), cx)
                                });
                            }
                        })),
                );
            }
        }
        div()
            .flex()
            .items_center()
            .py(px(6.))
            .border_b_1()
            .border_color(theme.border)
            .child(title)
            .child(div().w(px(100.)).flex_none().child(status))
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .child(item.worker_title.unwrap_or_default()),
            )
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .child(worker.and_then(|w| w.branch.clone()).unwrap_or_default()),
            )
            .child(
                div().w(px(140.)).flex_none().child(
                    worker
                        .and_then(|w| w.prs.first())
                        .map(|pr| pr.label())
                        .unwrap_or_default(),
                ),
            )
            .child(
                div().w(px(85.)).flex_none().child(
                    worker
                        .and_then(|w| w.diff)
                        .map(|(a, r)| format!("+{a} −{r}"))
                        .unwrap_or_default(),
                ),
            )
            .child(div().w(px(70.)).flex_none().child(age))
            .child(actions)
            .into_any_element()
    }

    fn board_card(
        &self,
        space: String,
        item: BoardItem,
        table: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = crate::components::section_card(theme)
            .id(SharedString::from(format!("board-item-{}", item.id)))
            .p(px(8.0))
            .mb(px(4.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_color(theme.text)
            .when(table, |el| el.flex_row().items_center())
            .child(
                div()
                    .id(SharedString::from(format!("board-title-{}", item.id)))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .child(item.title.clone())
                    .on_click(cx.listener({
                        let space = space.clone();
                        let id = item.id.clone();
                        move |_, _, _, cx| {
                            cx.emit(OutlineEvent::OpenDetails {
                                space_id: space.clone(),
                                item_id: id.clone(),
                            })
                        }
                    })),
            );
        if table {
            card = card.child(item.status.label());
        }
        if let Some(ticket) = &item.ticket {
            card = card.child(ticket.clone());
        }
        let item_id = item.id.clone();
        let expanded = self.board_expanded.contains(&item_id);
        card = card.child(
            div()
                .id(SharedString::from(format!("board-expand-{item_id}")))
                .cursor_pointer()
                .child(if expanded { "▾" } else { "▸" })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.board_expanded.remove(&item_id) {
                        this.board_expanded.insert(item_id.clone());
                    }
                    let view = cx.weak_entity();
                    window.on_next_frame(move |_, cx| {
                        let _ = view.update(cx, |this, cx| this.refresh_board_watches(cx));
                    });
                    cx.notify();
                })),
        );
        if let Some(mut worker) = item.worker {
            if let Some(activity) = self.board_activity.get(&worker.chat_id) {
                worker.activity = Some(activity.clone());
            }
            let mut chips = div().flex().flex_wrap().gap(px(4.0)).child(board_chip(
                format!(
                    "● {} · {}",
                    item.worker_title.as_deref().unwrap_or("Worker"),
                    match worker.state {
                        WorkerState::Waiting => "Waiting",
                        WorkerState::Working => "In progress",
                        WorkerState::Done => "Done",
                        WorkerState::Errored => "Errored",
                        WorkerState::Idle => "Idle",
                    }
                ),
                theme,
            ));
            if let Some(branch) = &worker.branch {
                chips = chips.child(board_chip(format!("⎇ {branch}"), theme));
            }
            for pr in &worker.prs {
                chips = chips.child(board_chip(pr.label(), theme));
            }
            card = card.child(chips);
            if let Some(activity) = self.board_activity.get(&worker.chat_id) {
                card = card.child(div().text_color(theme.text_muted).child(activity.clone()));
            }
            let diff = worker
                .diff
                .map(|(added, removed)| board_chip(format!("+{added} −{removed}"), theme));

            if expanded {
                if let Some(report) = &item.report {
                    card = card.child(format!("Last report: {report}"));
                }
                let artifacts = self
                    .board_artifacts
                    .get(&worker.chat_id)
                    .cloned()
                    .unwrap_or_default();
                for (id, title) in artifacts {
                    card = card.child(
                        div()
                            .id(SharedString::from(format!("board-artifact-{id}")))
                            .cursor_pointer()
                            .child(title)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(shell) =
                                    this.shell.as_ref().and_then(|shell| shell.upgrade())
                                {
                                    shell.update(cx, |shell, cx| {
                                        shell.open_artifact(id.clone(), window, cx)
                                    });
                                }
                            })),
                    );
                }
            }
            let waiting = worker.state == WorkerState::Waiting;
            let can_review = worker.branch.is_some()
                || !worker.prs.is_empty()
                || !self
                    .state
                    .read(cx)
                    .chat_diff_targets(&worker.chat_id)
                    .is_empty();
            let chat = worker.chat_id;
            let branch = worker.branch;
            let mut actions = div().flex().items_center().justify_between().children(diff);
            if can_review {
                let id = item.id.clone();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("board-action-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("board-action-{id}")
                        })
                        .cursor_pointer()
                        .text_color(theme.text_muted)
                        .child("Review")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(shell) =
                                this.shell.as_ref().and_then(|shell| shell.upgrade())
                            {
                                shell.update(cx, |shell, cx| {
                                    shell.open_request_diff(chat.clone(), branch.clone(), cx)
                                });
                            }
                        })),
                );
            }
            if waiting && let Some(request_id) = item.request_id {
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!("board-answer-{}", item.id)))
                        .debug_selector({
                            let id = item.id.clone();
                            move || format!("board-answer-{id}")
                        })
                        .cursor_pointer()
                        .bg(theme.accent)
                        .text_color(theme.on_accent)
                        .px(px(8.0))
                        .py(px(4.0))
                        .child("Answer")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(OutlineEvent::OpenRequiredAction(request_id.clone()))
                        })),
                );
            }
            card = card.child(actions);
        }
        card.into_any_element()
    }
}

fn board_chip(label: String, theme: &Theme) -> impl IntoElement {
    div()
        .bg(theme.surface_raised)
        .px(px(5.0))
        .py(px(2.0))
        .text_color(theme.text_muted)
        .child(label)
}

fn limit_board_items<'a>(
    items: impl Iterator<Item = &'a (String, BoardItem)>,
    limit: usize,
) -> Vec<(String, BoardItem)> {
    items.take(limit).cloned().collect()
}

fn board_link_candidates(
    rows: &[(String, BoardItem)],
    groups: &[Vec<usize>; 4],
    mode: BoardMode,
    limit: usize,
    table_scroll: &gpui::ScrollHandle,
    columns: &[gpui::ScrollHandle; 4],
) -> Vec<String> {
    fn visible_range(handle: &gpui::ScrollHandle, count: usize) -> std::ops::Range<usize> {
        if handle.bounds().size.height <= px(0.0) || count == 0 {
            return 0..0;
        }
        let bottom = handle.bottom_item().min(count);
        let start = handle.top_item().max(1).saturating_sub(1).min(bottom);
        start..bottom
    }
    if mode == BoardMode::Table {
        let count = rows.len().min(limit.saturating_mul(4));
        return rows[visible_range(table_scroll, count)]
            .iter()
            .filter_map(|(_, item)| {
                (item.status != BoardStatus::Done)
                    .then(|| item.worker.as_ref().map(|worker| worker.chat_id.clone()))
                    .flatten()
            })
            .collect();
    }
    let mut result = Vec::new();
    for status in [
        BoardStatus::Todo,
        BoardStatus::InProgress,
        BoardStatus::InReview,
    ] {
        let indices = &groups[status as usize];
        let count = indices.len().min(limit);
        for index in &indices[visible_range(&columns[status as usize], count)] {
            if let Some(worker) = &rows[*index].1.worker {
                result.push(worker.chat_id.clone());
            }
        }
    }
    result
}

fn sort_board_rows(entries: &mut Vec<(String, BoardItem)>, sort: BoardSort, descending: bool) {
    match sort {
        BoardSort::Title => entries.sort_by_key(|(_, item)| item.title.to_lowercase()),
        BoardSort::Branch => entries.sort_by_key(|(_, item)| {
            item.worker
                .as_ref()
                .and_then(|w| w.branch.clone())
                .unwrap_or_default()
        }),
        BoardSort::Pr => entries.sort_by_key(|(_, item)| {
            item.worker
                .as_ref()
                .and_then(|w| w.prs.first())
                .map(|pr| pr.number)
                .unwrap_or_default()
        }),
        BoardSort::Diff => entries.sort_by_key(|(_, item)| {
            item.worker
                .as_ref()
                .and_then(|w| w.diff)
                .unwrap_or_default()
        }),
        BoardSort::Action => entries.sort_by_key(|(_, item)| item.request_id.is_some()),
        BoardSort::Status => {
            entries.sort_by_key(|(_, item)| (item.status, std::cmp::Reverse(item.updated_at)))
        }
        BoardSort::Worker => entries.sort_by_key(|(_, item)| {
            item.worker_title
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
        }),
        BoardSort::Age => entries.sort_by_key(|(_, item)| item.created_at),
    }
    if descending {
        entries.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn board_idle_rebuild_budget_at_space_scale(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| { let mut state = AppState::new(); state.chats = (0..50).map(|i| serde_json::from_value(serde_json::json!({"id":format!("chat-{i}"),"deviceId":"local","spaceId":"s","archived":false,"createdAt":"2026-09-08T00:00:00Z"})).unwrap()).collect(); state });
        state.update(cx, |state, _| {
            for i in 0..5 {
                state.begin_pending_send(&format!("chat-{i}"), "message", chrono::Utc::now());
            }
        });
        let artifacts: Vec<_> = (0..500).map(|i| serde_json::json!({"id":format!("artifact-{i}"),"spaceId":"s","chatId":format!("chat-{}", i % 50),"title":format!("Artifact {i}")})).collect();
        state.update(cx, |state, _| {
            state.set_test_artifacts(
                serde_json::from_value(
                    serde_json::json!({"spaceId":"s","revision":1,"artifacts":artifacts}),
                )
                .unwrap(),
            )
        });
        let items: Vec<_> = (0..500).map(|i| serde_json::json!({"id":format!("item-{i}"),"spaceId":"s","title":format!("Item {i}"),"chatIds":if i < 50 { vec![format!("chat-{i}")] } else { vec![] }})).collect();
        let requests: Vec<_> = (0..500).map(|i| serde_json::json!({"id":format!("request-{i}"),"spaceId":"s","workItemId":format!("item-{}", i % 50),"chatId":format!("chat-{}", i % 50),"kind":"notice","state":"open"})).collect();
        let references: Vec<_> = (0..500).map(|i| serde_json::json!({"id":format!("reference-{i}"),"spaceId":"s","workItemId":format!("item-{i}"),"provider":"linear","referenceType":"issue","externalId":format!("TEST-{i}"),"url":"https://example.invalid"})).collect();
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(
            serde_json::json!({"spaceId":"s","available":true,"revision":1,"items":items,"userActionRequests":requests,"externalReferences":references}),
        )
        .unwrap();
        let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state.clone(), cx));
        outline.update(vcx, |outline, cx| {
            outline.set_inputs(
                OutlineInputs {
                    spaces: vec![OutlineSpace {
                        id: "s".into(),
                        label: "Space".into(),
                        load: WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
                        hidden: false,
                    }],
                    ..Default::default()
                },
                cx,
            );
            outline.board_mode = BoardMode::Board;
            for i in 0..50 {
                let pr = mast_engine::pr_ticket_cache::PrStatus {
                    number: i + 1,
                    url: None,
                    state: "OPEN".into(),
                    is_draft: false,
                    review_decision: None,
                    reviewers: Vec::new(),
                    checks: None,
                    title: None,
                    branch: None,
                    head_sha: None,
                    mergeable: String::new(),
                    has_reviewer_requested: false,
                    merged_at: None,
                };
                outline.board_links.insert(
                    format!("chat-{i}"),
                    mast_engine::pr_ticket_cache::ChatLinkStatus {
                        pr: Some(pr),
                        ..Default::default()
                    },
                );
            }
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        outline.update(vcx, |outline, cx| outline.refresh_board_data(cx));
        assert!(!outline.read_with(vcx, |outline, _| outline.board_activity_ids.is_empty()));
        let before = outline.read_with(vcx, |outline, _| outline.board_rebuild_count);
        let started = std::time::Instant::now();
        for _ in 0..50 {
            state.update(vcx, |state, cx| {
                state.transcript_revision += 1;
                cx.notify();
            });
            vcx.run_until_parked();
            outline.update(vcx, |_, cx| cx.notify());
            vcx.update(|window, cx| window.draw(cx).clear());
        }
        let elapsed = started.elapsed();
        assert_eq!(
            outline.read_with(vcx, |outline, _| outline.board_rebuild_count),
            before
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "50 idle redraws took {elapsed:?}"
        );
        outline.update(vcx, |outline, cx| {
            outline.board_mode = BoardMode::Table;
            outline.board_rows_dirty = true;
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        let before = outline.read_with(vcx, |outline, _| outline.board_rebuild_count);
        let table_started = std::time::Instant::now();
        for _ in 0..50 {
            outline.update(vcx, |_, cx| cx.notify());
            vcx.update(|window, cx| window.draw(cx).clear());
        }
        assert_eq!(
            outline.read_with(vcx, |outline, _| outline.board_rebuild_count),
            before,
            "table sort must be cached across redraws"
        );
        assert!(
            table_started.elapsed() < std::time::Duration::from_secs(5),
            "50 table redraws exceeded the budget"
        );
    }

    #[test]
    fn done_column_is_capped_until_more_is_requested() {
        let rows: Vec<_> = (0..15)
            .map(|i| {
                (
                    "s".into(),
                    BoardItem {
                        id: i.to_string(),
                        title: i.to_string(),
                        status: BoardStatus::Done,
                        updated_at: 0,
                        created_at: 0,
                        worker: None,
                        worker_title: None,
                        report: None,
                        ticket: None,
                        request_id: None,
                    },
                )
            })
            .collect();
        let first = limit_board_items(rows.iter(), 12);
        assert_eq!(first.len(), 12);
        let expanded = limit_board_items(rows.iter(), 24);
        assert_eq!(expanded.len(), 15);
    }

    #[gpui::test]
    fn link_candidates_skip_done_and_offscreen_chats(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.chats = (0..15).map(|i| serde_json::from_value(serde_json::json!({"id":format!("worker-{i}"),"deviceId":"local","spaceId":"s","archived":false,"createdAt":"2026-09-08T00:00:00Z"})).unwrap()).collect();
            state
        });
        let items: Vec<_> = (0..15).map(|i| serde_json::json!({"id":format!("item-{i}"),"spaceId":"s","title":format!("Item {i}"),"effectiveStatus":if i == 0 { "completed" } else { "active" },"chatIds":[format!("worker-{i}")]})).collect();
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(
            serde_json::json!({"spaceId":"s","available":true,"items":items}),
        )
        .unwrap();
        let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state, cx));
        outline.update(vcx, |outline, cx| {
            outline.set_inputs(
                OutlineInputs {
                    spaces: vec![OutlineSpace {
                        id: "s".into(),
                        label: "Space".into(),
                        load: WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
                        hidden: false,
                    }],
                    ..Default::default()
                },
                cx,
            );
            outline.board_mode = BoardMode::Board;
        });
        vcx.simulate_resize(gpui::size(px(900.0), px(300.0)));
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        outline.read_with(vcx, |outline, _| {
            let ids = board_link_candidates(
                &outline.board_rows,
                &outline.board_groups,
                outline.board_mode,
                outline.board_limit,
                &outline.board_scroll,
                &outline.board_column_scroll,
            );
            assert!(!ids.is_empty());
            assert!(
                ids.len() < 12,
                "offscreen rows must not be requested: {ids:?}"
            );
            assert!(!ids.iter().any(|id| id == "worker-0"));
        });
    }

    #[test]
    fn table_age_sort_changes_direction() {
        let item = |id: &str, created_at| BoardItem {
            id: id.into(),
            title: id.into(),
            status: BoardStatus::Todo,
            updated_at: 0,
            created_at,
            worker: None,
            worker_title: None,
            report: None,
            ticket: None,
            request_id: None,
        };
        let mut rows = vec![("s".into(), item("new", 20)), ("s".into(), item("old", 10))];
        sort_board_rows(&mut rows, BoardSort::Age, false);
        assert_eq!(rows[0].1.id, "old");
        sort_board_rows(&mut rows, BoardSort::Age, true);
        assert_eq!(rows[0].1.id, "new");
    }

    #[gpui::test]
    fn unrelated_state_changes_do_not_notify_board(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let state = cx.new(|_| { let mut state = AppState::new(); state.chats.push(serde_json::from_value(serde_json::json!({"id":"worker","deviceId":"local","spaceId":"s","archived":false,"createdAt":"2026-09-08T00:00:00Z"})).unwrap()); state.begin_pending_send("worker", "m", chrono::Utc::now()); state });
        let shell = cx.new(|cx| {
            Shell::new(
                state.clone(),
                crate::EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let (outline, vcx) = cx.add_window_view(|_, cx| {
            OutlineView::new(state.clone(), cx).with_shell(shell.downgrade())
        });
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({"spaceId":"s","available":true,"revision":1,"items":[{"id":"item","spaceId":"s","title":"Item","effectiveStatus":"active","chatIds":["worker"]}]})).unwrap();
        outline.update(vcx, |outline, cx| {
            outline.set_inputs(
                OutlineInputs {
                    spaces: vec![OutlineSpace {
                        id: "s".into(),
                        label: "Space".into(),
                        load: WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
                        hidden: false,
                    }],
                    ..Default::default()
                },
                cx,
            );
            outline.board_mode = BoardMode::Board;
            outline.board_status_revision = state.read(cx).board_status_revision;
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        outline.update(vcx, |outline, cx| outline.refresh_board_data(cx));
        vcx.run_until_parked();
        outline.read_with(vcx, |outline, _| {
            assert_eq!(outline.board_activity_ids, vec!["worker"]);
            assert!(outline.board_shell_observe.is_some());
        });
        let notifications = Rc::new(Cell::new(0));
        let seen = notifications.clone();
        vcx.update(|_, cx| {
            cx.observe(&outline, move |_, _| seen.set(seen.get() + 1))
                .detach()
        });
        for _ in 0..10 {
            state.update(vcx, |_, cx| cx.notify());
            shell.update(vcx, |_, cx| cx.notify());
            vcx.run_until_parked();
        }
        assert_eq!(notifications.get(), 0);
    }
    #[gpui::test]
    fn scope_controls_switch_and_reset_expansion(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state, cx));
        outline.update(vcx, |outline, _| {
            outline.board_mode = BoardMode::Board;
            outline.board_root = Some(("s".into(), "root".into()));
            outline.board_show_all = true;
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        let point = vcx.debug_bounds("board-scope-Space").unwrap().center();
        vcx.simulate_click(point, gpui::Modifiers::default());
        vcx.run_until_parked();
        outline.read_with(vcx, |outline, _| {
            assert!(outline.board_space_scope);
            assert!(!outline.board_show_all);
        });
        let point = vcx.debug_bounds("board-scope-Subtree").unwrap().center();
        vcx.simulate_click(point, gpui::Modifiers::default());
        vcx.run_until_parked();
        outline.read_with(vcx, |outline, _| assert!(!outline.board_space_scope));
    }

    #[gpui::test]
    fn answer_opens_the_matching_request(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| { let mut state = AppState::new(); state.chats.push(serde_json::from_value(serde_json::json!({"id":"worker","deviceId":"local","spaceId":"s","archived":false,"createdAt":"2026-09-08T00:00:00Z"})).unwrap()); state });
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({"spaceId":"s","available":true,"items":[{"id":"item","spaceId":"s","title":"Item","effectiveStatus":"userActionRequired","chatIds":["worker"]}],"userActionRequests":[{"id":"request","spaceId":"s","workItemId":"item","chatId":"worker","kind":"ask","state":"open","text":"Choose"}]})).unwrap();
        let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state, cx));
        outline.update(vcx, |outline, cx| {
            outline.set_inputs(
                OutlineInputs {
                    spaces: vec![OutlineSpace {
                        id: "s".into(),
                        label: "Space".into(),
                        load: WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
                        hidden: false,
                    }],
                    ..Default::default()
                },
                cx,
            );
            outline.board_mode = BoardMode::Board;
        });
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&outline, move |_, event: &OutlineEvent, _| {
                sink.lock().unwrap().push(event.clone())
            })
            .detach()
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        let point = vcx
            .debug_bounds("board-answer-item")
            .expect("answer button")
            .center();
        vcx.simulate_click(point, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert!(
            events
                .lock()
                .unwrap()
                .contains(&OutlineEvent::OpenRequiredAction("request".into()))
        );
    }

    #[gpui::test]
    fn review_opens_the_worker_diff(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = calls.clone();
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(std::sync::Arc::new(move |method, params| { captured.lock().unwrap().push((method.to_owned(), params)); Ok(serde_json::json!({})) }));
            state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView { chat_id:"worker".into(), targets:vec![mast_proto::ChatDiffTargetView { path:"/work/worker".into(), checkout_id:"checkout".into(), source:mast_proto::ChatDiffTargetSource::Agent, selected:true, repo_name:"repo".into(), branch:Some("feature/test".into()), head_sha:None }], ref_targets:vec![] }]);
            state.chats.push(serde_json::from_value(serde_json::json!({"id":"worker","deviceId":"local","spaceId":"s","archived":false,"branch":"feature/test","createdAt":"2026-09-08T00:00:00Z"})).unwrap());
            state
        });
        let shell = cx.new(|cx| {
            Shell::new(
                state.clone(),
                crate::EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let snapshot: mast_proto::WorkOutlineSnapshot = serde_json::from_value(serde_json::json!({"spaceId":"s","available":true,"items":[{"id":"item","spaceId":"s","title":"Item","effectiveStatus":"active","chatIds":["worker"]}]})).unwrap();
        let (outline, vcx) = cx.add_window_view(|_, cx| {
            OutlineView::new(state.clone(), cx).with_shell(shell.downgrade())
        });
        outline.update(vcx, |outline, cx| {
            outline.set_inputs(
                OutlineInputs {
                    spaces: vec![OutlineSpace {
                        id: "s".into(),
                        label: "Space".into(),
                        load: WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
                        hidden: false,
                    }],
                    ..Default::default()
                },
                cx,
            );
            outline.board_mode = BoardMode::Board;
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        let point = vcx
            .debug_bounds("board-action-item")
            .expect("review button")
            .center();
        vcx.simulate_click(point, gpui::Modifiers::default());
        vcx.run_until_parked();
        state.read_with(vcx, |state, _| {
            assert_eq!(state.selected_chat.as_deref(), Some("worker"))
        });
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|(_, params)| params.to_string().contains("/work/worker"))
        );
    }
}
