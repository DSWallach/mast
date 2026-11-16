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

#[derive(Clone, Copy)]
pub(super) enum BoardSort {
    Status,
    Worker,
    Age,
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

    fn board_chat_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for space in &self.inputs.spaces {
            if self.board_space.as_ref().is_some_and(|id| id != &space.id) {
                continue;
            }
            let Some(snapshot) = space.load.snapshot() else {
                continue;
            };
            let scope = self
                .board_root
                .as_ref()
                .filter(|_| !self.board_space_scope)
                .map(|(_, root)| {
                    let mut scope = model::descendants(snapshot, root);
                    scope.insert(root.clone());
                    scope
                });
            for item in &snapshot.items {
                if item.archived_at.is_none()
                    && scope.as_ref().is_none_or(|scope| scope.contains(&item.id))
                {
                    ids.extend(item.chat_ids.iter().cloned());
                }
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    pub(super) fn refresh_board_watches(&mut self, cx: &mut Context<Self>) {
        if self.board_mode != BoardMode::Tree {
            self.watch_board_links(cx);
            self.watch_board_activity(cx);
        }
    }

    fn watch_board_links(&mut self, cx: &mut Context<Self>) {
        let Some(shell) = self.shell.as_ref().and_then(|shell| shell.upgrade()) else {
            return;
        };
        let ids = self.board_chat_ids();
        shell.update(cx, |shell, cx| shell.request_board_links(&ids, cx));
        if self.board_shell_observe.is_none() {
            self.board_shell_observe = Some(cx.observe(&shell, |this, shell, cx| {
                if this.board_mode == BoardMode::Tree {
                    return;
                }
                let links = shell.read(cx).board_link_statuses(&this.board_chat_ids());
                let mut entries: Vec<_> = links.iter().collect();
                entries.sort_by_key(|(id, _)| *id);
                let fingerprint = serde_json::to_string(&entries).unwrap_or_default();
                if fingerprint != this.board_link_fingerprint {
                    this.board_link_fingerprint = fingerprint;
                    this.board_links = links;
                    cx.notify();
                }
            }));
        }
    }

    fn watch_board_activity(&mut self, cx: &mut Context<Self>) {
        let now = chrono::Utc::now();
        let state = self.state.read(cx);
        let ids: std::collections::HashSet<_> = self
            .board_chat_ids()
            .into_iter()
            .filter(|id| {
                state
                    .chats
                    .iter()
                    .find(|chat| &chat.id == id)
                    .is_some_and(|chat| {
                        state.display_status_for(chat, now) == mast_proto::ChatIndicator::Working
                    })
            })
            .collect();
        self.state
            .update(cx, |state, cx| state.set_board_watch_docs(ids, cx));
        if self.board_state_observe.is_none() {
            self.board_state_observe = Some(cx.observe(&self.state, |this, state, cx| {
                if this.board_mode == BoardMode::Tree {
                    return;
                }
                let state = state.read(cx);
                let mut activity = std::collections::HashMap::new();
                for id in this.board_chat_ids() {
                    let call = state
                        .sub_transcript(&id)
                        .iter()
                        .rev()
                        .flat_map(|message| message.parts.iter().rev())
                        .find_map(|part| match part {
                            mast_doc::MessagePart::Tool { call, .. } => Some(call),
                            _ => None,
                        });
                    if let Some(line) = board::activity(call, None) {
                        activity.insert(id, line);
                    }
                }
                if activity != this.board_activity {
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
                .map(|space| space.label.as_str())
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
                    .rounded(px(4.0))
                    .bg(if self.board_mode == mode {
                        theme.surface_raised
                    } else {
                        theme.surface
                    })
                    .text_color(theme.text)
                    .text_size(crate::typography::ui_rems(11.0))
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
                        if mode != BoardMode::Tree {
                            this.watch_board_links(cx);
                            this.watch_board_activity(cx);
                        } else {
                            this.state.update(cx, |state, cx| {
                                state.set_board_watch_docs(Default::default(), cx)
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        if self.board_mode != BoardMode::Tree {
            let space = self.board_space_scope;
            bar = bar.child(
                div()
                    .id("board-scope")
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(if space { "Whole Space" } else { "Subtree" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.board_space_scope = !this.board_space_scope;
                        this.refresh_board_watches(cx);
                        cx.notify();
                    })),
            );
        }
        bar.into_any_element()
    }

    pub(super) fn render_board(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let now = chrono::Utc::now();
        let indicators = state
            .chats
            .iter()
            .map(|chat| (chat.id.clone(), state.display_status_for(chat, now)))
            .collect();
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
        let mut body = div()
            .id("coordination-board")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(10.0));
        if self.board_mode == BoardMode::Table {
            match self.board_sort {
                BoardSort::Status => entries
                    .sort_by_key(|(_, item)| (item.status, std::cmp::Reverse(item.updated_at))),
                BoardSort::Worker => entries.sort_by_key(|(_, item)| {
                    item.worker_title
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                }),
                BoardSort::Age => entries.sort_by_key(|(_, item)| item.created_at),
            }
            let mut headings = div().flex().gap(px(12.0));
            for (sort, label) in [
                (BoardSort::Status, "Status"),
                (BoardSort::Worker, "Worker"),
                (BoardSort::Age, "Age"),
            ] {
                headings = headings.child(
                    div()
                        .id(SharedString::from(format!("board-sort-{label}")))
                        .cursor_pointer()
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.board_sort = sort;
                            cx.notify();
                        })),
                );
            }
            body = body.child(headings);
            for (space, item) in entries {
                body = body.child(self.board_card(space, item, true, theme, cx));
            }
        } else {
            let mut columns = div().flex().gap(px(8.0)).items_start();
            for (status, title) in [
                (BoardStatus::Todo, "TO DO"),
                (BoardStatus::InProgress, "IN PROGRESS"),
                (BoardStatus::InReview, "IN REVIEW"),
                (BoardStatus::Done, "DONE"),
            ] {
                let mut items: Vec<_> = entries
                    .iter()
                    .filter(|(_, item)| item.status == status)
                    .cloned()
                    .collect();
                items.sort_by_key(|(_, item)| std::cmp::Reverse(item.updated_at));
                let count = items.len();
                if status == BoardStatus::Done && !self.board_show_all {
                    items.truncate(12);
                }
                let mut column = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .text_size(crate::typography::ui_rems(10.0))
                            .child(format!("{title}  {count}")),
                    );
                for (space, item) in items {
                    column = column.child(self.board_card(space, item, false, theme, cx));
                }
                if status == BoardStatus::Done && count > 12 && !self.board_show_all {
                    column = column.child(
                        div()
                            .id("board-show-all")
                            .cursor_pointer()
                            .child("Show all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.board_show_all = true;
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

    fn board_card(
        &self,
        space: String,
        item: BoardItem,
        table: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = div()
            .id(SharedString::from(format!("board-item-{}", item.id)))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .bg(theme.surface_card)
            .p(px(8.0))
            .mb(px(4.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .when(table, |el| el.flex_row().items_center())
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(item.title.clone()),
            );
        if table {
            card = card.child(format!("{:?}", item.status));
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
                .child(if expanded { "Less" } else { "Details" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.board_expanded.remove(&item_id) {
                        this.board_expanded.insert(item_id.clone());
                    }
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
                    worker.state.label()
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
            if let Some(activity) = &worker.activity {
                card = card.child(div().text_color(theme.text_muted).child(activity.clone()));
            }
            let diff = worker
                .diff
                .map(|(added, removed)| board_chip(format!("+{added} −{removed}"), theme));
            if let Some(ticket) = &worker.ticket {
                card = card.child(format!(
                    "{} {}",
                    ticket.identifier,
                    ticket.state.as_deref().unwrap_or("")
                ));
            }
            if expanded {
                if let Some(report) = &item.report {
                    card = card.child(format!("Last report: {report}"));
                }
                let artifacts = self
                    .state
                    .read(cx)
                    .artifacts()
                    .snapshot(&space)
                    .map(|snapshot| {
                        snapshot
                            .for_chat(&worker.chat_id)
                            .into_iter()
                            .take(5)
                            .map(|artifact| (artifact.id.clone(), artifact.title.clone()))
                            .collect::<Vec<_>>()
                    })
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
            let request_id = item.request_id.clone();
            let chat = worker.chat_id;
            let branch = worker.branch;
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .children(diff)
                    .child(
                        div()
                            .id(SharedString::from(format!("board-action-{}", item.id)))
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .bg(theme.accent)
                            .text_color(theme.on_accent)
                            .child(if waiting { "Answer" } else { "Review" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if waiting {
                                    if let Some(id) = &request_id {
                                        cx.emit(OutlineEvent::OpenRequiredAction(id.clone()));
                                    } else {
                                        cx.emit(OutlineEvent::OpenRequiredActions);
                                    }
                                } else if let Some(shell) =
                                    this.shell.as_ref().and_then(|shell| shell.upgrade())
                                {
                                    shell.update(cx, |shell, cx| {
                                        shell.open_request_diff(chat.clone(), branch.clone(), cx)
                                    });
                                }
                            })),
                    ),
            );
        } else {
            let id = item.id;
            card = card.child(
                div()
                    .id(SharedString::from(format!("board-action-{id}")))
                    .cursor_pointer()
                    .child("Open")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OutlineEvent::OpenDetails {
                            space_id: space.clone(),
                            item_id: id.clone(),
                        })
                    })),
            );
        }
        card.into_any_element()
    }
}

fn board_chip(label: String, theme: &Theme) -> impl IntoElement {
    div()
        .rounded(px(4.0))
        .bg(theme.surface_raised)
        .px(px(5.0))
        .py(px(2.0))
        .text_color(theme.text_muted)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn unrelated_state_changes_do_not_notify_board(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state.clone(), cx));
        outline.update(vcx, |outline, cx| {
            outline.board_mode = BoardMode::Board;
            outline.watch_board_activity(cx);
        });
        let notifications = Rc::new(Cell::new(0));
        let seen = notifications.clone();
        vcx.update(|_, cx| {
            cx.observe(&outline, move |_, _| seen.set(seen.get() + 1))
                .detach()
        });
        for _ in 0..10 {
            state.update(vcx, |_, cx| cx.notify());
            vcx.run_until_parked();
        }
        assert_eq!(notifications.get(), 0);
    }
}
