//! Board and table presentation for the outline.
use super::*;
use crate::board::{self, BoardItem, BoardStatus};
use crate::team::{TeamInput, WorkerState};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardMode {
    Tree,
    Board,
    Table,
}

impl OutlineView {
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
                                .and_then(|row| row.item())
                                .map(|item| (item.space_id.clone(), item.item_id.clone()));
                        }
                        this.board_mode = mode;
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
        let links = HashMap::new();
        let mut entries = Vec::new();
        for space in &self.inputs.spaces {
            if self
                .board_root
                .as_ref()
                .is_some_and(|(id, _)| id != &space.id)
            {
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
                links: &links,
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
            entries.sort_by_key(|(_, item)| (item.status, std::cmp::Reverse(item.updated_at)));
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
            .child(item.title.clone());
        if table {
            card = card.child(format!("{:?}", item.status));
        }
        if let Some(worker) = item.worker {
            card = card.child(format!("{} · {}", worker.title, worker.state.label()));
            if let Some(branch) = &worker.branch {
                card = card.child(format!("⎇ {branch}"));
            }
            for pr in &worker.prs {
                card = card.child(pr.label());
            }
            if let Some(activity) = &worker.activity {
                card = card.child(activity.clone());
            }
            if let Some((added, removed)) = worker.diff {
                card = card.child(format!("+{added} −{removed}"));
            }
            let waiting = worker.state == WorkerState::Waiting;
            let chat = worker.chat_id;
            let branch = worker.branch;
            card = card.child(
                div()
                    .id(SharedString::from(format!("board-action-{}", item.id)))
                    .cursor_pointer()
                    .text_color(theme.text)
                    .child(if waiting { "Answer" } else { "Review" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if waiting {
                            cx.emit(OutlineEvent::OpenRequiredActions);
                        } else if let Some(shell) =
                            this.shell.as_ref().and_then(|shell| shell.upgrade())
                        {
                            shell.update(cx, |shell, cx| {
                                shell.open_request_diff(chat.clone(), branch.clone(), cx)
                            });
                        }
                    })),
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
