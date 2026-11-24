use super::*;
use mast_proto::{SidebarPinChange, SidebarSectionChange};

const SCHEDULED_SECTION_KEY: &str = "scheduled-section";

/// Drag payload for reordering rows inside the Scheduled section.
#[derive(Clone, PartialEq)]
pub(in crate::shell) struct ScheduledRowDrag {
    task_id: String,
    title: SharedString,
}

struct ScheduledRowGhost {
    title: SharedString,
}

impl Render for ScheduledRowGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .h(px(24.0))
            .max_w(px(180.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded_md()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

fn task_id_of(chat: &mast_proto::Chat) -> Option<&str> {
    chat.scheduled_task_info
        .as_ref()
        .map(|info| info.task_id.as_str())
}

/// `order` with `task` moved onto `target`'s slot: after it when moving down,
/// before it when moving up.
pub(super) fn reorder_onto(order: &[String], task: &str, target: &str) -> Option<Vec<String>> {
    let from = order.iter().position(|id| id == task)?;
    let to = order.iter().position(|id| id == target)?;
    if from == to {
        return None;
    }
    let mut next = order.to_vec();
    let moved = next.remove(from);
    next.insert(to, moved);
    Some(next)
}

/// The Overview group row's summary line: the task count when open, and
/// only the rows a collapse hides when collapsed.
fn scheduled_overview_summary(open: bool, total: usize, hidden: usize) -> String {
    let tasks = format!("{total} task{}", if total == 1 { "" } else { "s" });
    if open {
        tasks
    } else if hidden > 0 {
        format!("{hidden} hidden")
    } else {
        format!("{total} pinned")
    }
}

/// Collapsed headers count only the rows the collapse hides.
fn scheduled_header_label(open: bool, hidden: usize) -> String {
    if open || hidden == 0 {
        "Scheduled".into()
    } else {
        format!("Scheduled ({hidden})")
    }
}

impl Shell {
    /// Task IDs in the user's saved order, with moves still in flight applied.
    pub(in crate::shell) fn scheduled_task_order(&self, cx: &App) -> Vec<String> {
        let mut order = self
            .state
            .read(cx)
            .sidebar_preferences
            .scheduled_task_order
            .clone();
        if self.optimistic_sidebar_pins(cx).is_some()
            && let Some(pending) = &self.sidebar_pin_write
        {
            for change in &pending.queue {
                change.project_scheduled_order(&mut order);
            }
        }
        order
    }

    /// Pinned task IDs, with pin changes still in flight applied.
    pub(in crate::shell) fn scheduled_pinned_tasks(&self, cx: &App) -> Vec<String> {
        let mut pinned = self
            .state
            .read(cx)
            .sidebar_preferences
            .scheduled_pinned_task_ids
            .clone();
        if self.optimistic_sidebar_pins(cx).is_some()
            && let Some(pending) = &self.sidebar_pin_write
        {
            for change in &pending.queue {
                change.project_scheduled_pins(&mut pinned);
            }
        }
        pinned
    }

    /// The scheduled task a native run chat belongs to, if any.
    pub(in crate::shell) fn scheduled_task_of_chat(
        &self,
        chat_id: &str,
        cx: &App,
    ) -> Option<String> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(task_id_of)
            .map(str::to_owned)
    }

    /// Whether the chat shows as pinned: its task for a native scheduled run,
    /// the session otherwise.
    pub(in crate::shell) fn chat_is_pinned(&self, chat_id: &str, cx: &App) -> bool {
        match self.scheduled_task_of_chat(chat_id, cx) {
            Some(task) => self.scheduled_pinned_tasks(cx).contains(&task),
            None => self.active_sidebar_pins(cx).contains(&chat_id.to_owned()),
        }
    }

    pub(in crate::shell) fn set_scheduled_task_pinned(
        &mut self,
        task_id: &str,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        if self.scheduled_pinned_tasks(cx).iter().any(|id| id == task_id) == pinned {
            return;
        }
        let Some(key) = self.active_sidebar_pin_profile_key(cx) else {
            return;
        };
        if !self.state.read(cx).sidebar_preferences.can_edit() {
            self.sidebar_notice = Some("Sidebar is still syncing. Try again shortly.".into());
            cx.notify();
            return;
        }
        self.queue_sidebar_pin_write(
            key,
            SidebarPinChange::Section {
                change: SidebarSectionChange::PinScheduled {
                    task_id: task_id.to_owned(),
                    pinned,
                },
            },
            cx,
        );
    }

    /// Task IDs of the Scheduled rows exactly as drawn, top to bottom.
    pub(in crate::shell) fn scheduled_visible_task_ids(&self, cx: &App) -> Vec<String> {
        let order = self.scheduled_task_order(cx);
        let pinned = self.scheduled_pinned_tasks(cx);
        self.state
            .read(cx)
            .scheduled_sidebar_chats(
                Utc::now(),
                self.settings.space_filter.as_deref(),
                &order,
                &pinned,
            )
            .into_iter()
            .filter_map(|(_, chat)| task_id_of(chat).map(str::to_owned))
            .collect()
    }

    /// Chat ids of the Scheduled rows in display order, for keyboard cycling.
    pub(in crate::shell) fn scheduled_visible_chat_ids(&self, cx: &App) -> Vec<String> {
        let order = self.scheduled_task_order(cx);
        let pinned = self.scheduled_pinned_tasks(cx);
        let always_visible = self.settings.pinned_always_visible;
        self.state
            .read(cx)
            .scheduled_sidebar_chats(
                Utc::now(),
                self.settings.space_filter.as_deref(),
                &order,
                &pinned,
            )
            .into_iter()
            .filter(|(_, chat)| {
                self.scheduled_section_open
                    || (always_visible
                        && task_id_of(chat).is_some_and(|id| pinned.iter().any(|p| p == id)))
            })
            .map(|(_, chat)| chat.id.clone())
            .collect()
    }

    pub(in crate::shell) fn move_scheduled_task(
        &mut self,
        task_id: &str,
        target_id: &str,
        cx: &mut Context<Self>,
    ) {
        // Pinned and unpinned tasks are ordered separately, so a drop across
        // the boundary does nothing.
        let pinned = self.scheduled_pinned_tasks(cx);
        let is_pinned = |id: &str| pinned.iter().any(|p| p == id);
        if is_pinned(task_id) != is_pinned(target_id) {
            return;
        }
        let Some(order) = reorder_onto(&self.scheduled_visible_task_ids(cx), task_id, target_id)
        else {
            return;
        };
        let Some(key) = self.active_sidebar_pin_profile_key(cx) else {
            return;
        };
        if !self.state.read(cx).sidebar_preferences.can_edit() {
            self.sidebar_notice = Some("Sidebar is still syncing. Try again shortly.".into());
            cx.notify();
            return;
        }
        self.queue_sidebar_pin_write(
            key,
            SidebarPinChange::Section {
                change: SidebarSectionChange::MoveScheduled {
                    task_id: task_id.to_owned(),
                    order,
                },
            },
            cx,
        );
    }

    /// A Scheduled row wrapped so it can be dragged within its group.
    fn scheduled_drag_row(
        element: AnyElement,
        task_id: String,
        title: SharedString,
        row_order: Vec<String>,
        accent: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let payload = ScheduledRowDrag {
            task_id: task_id.clone(),
            title,
        };
        let own_id = task_id.clone();
        div()
            .id(SharedString::from(format!("scheduled-row-{task_id}")))
            .debug_selector({
                let task_id = task_id.clone();
                move || format!("scheduled-row-{task_id}")
            })
            .on_drag(payload, |payload, _, _, cx| {
                let title = payload.title.clone();
                cx.new(|_| ScheduledRowGhost { title })
            })
            .drag_over::<ScheduledRowDrag>(move |style, dragged, _, _| {
                // `reorder_onto` lands a downward move after the target, so
                // the insertion line goes below it in that case.
                let from = row_order.iter().position(|id| *id == dragged.task_id);
                let to = row_order.iter().position(|id| *id == own_id);
                if matches!((from, to), (Some(from), Some(to)) if from < to) {
                    style.border_b_2().border_color(accent)
                } else {
                    style.border_t_2().border_color(accent)
                }
            })
            .on_drop(cx.listener(move |this, dragged: &ScheduledRowDrag, _, cx| {
                this.move_scheduled_task(&dragged.task_id, &task_id, cx);
            }))
            .child(element)
    }

    /// The Scheduled section as the Overview draws it: outline rows (group
    /// row, then one Chat row per task) instead of the Sessions list's rows.
    /// Collapsing hides every row, except pinned ones with "Pinned items are
    /// always visible" on.
    pub(in crate::shell) fn render_overview_scheduled_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let order = self.scheduled_task_order(cx);
        let pinned = self.scheduled_pinned_tasks(cx);
        let filter = self.settings.space_filter.clone();
        let rows: Vec<ActiveChatRow> = {
            let state = self.state.read(cx);
            state
                .scheduled_sidebar_chats(Utc::now(), filter.as_deref(), &order, &pinned)
                .into_iter()
                .map(|(status, chat)| self.sidebar_chat_data(status, chat.clone(), state))
                .collect()
        };
        if rows.is_empty() {
            return None;
        }
        let open = self.scheduled_section_open;
        let always_visible = self.settings.pinned_always_visible;
        let is_pinned = |chat: &mast_proto::Chat| {
            task_id_of(chat).is_some_and(|id| pinned.iter().any(|p| p == id))
        };
        let total = rows.len();
        let shown: Vec<ActiveChatRow> = rows
            .into_iter()
            .filter(|row| open || (always_visible && is_pinned(&row.chat)))
            .collect();
        let summary = scheduled_overview_summary(open, total, total - shown.len());
        let height = crate::work_outline::outline_view::sidebar_row_height(
            self.settings.sidebar_compact,
            crate::ui_style::current(cx).tokens,
            cx,
        );
        let header_theme = crate::components::outline_row_theme(theme, false);
        let header = crate::components::outline_row(
            SharedString::from("outline-row-scheduled"),
            height,
            crate::work_outline::outline_view::OutlineView::indent(0),
            &header_theme,
        )
        .cursor_pointer()
        .hover(|el| el.bg(header_theme.element_hover.opacity(0.6)))
        .on_click(cx.listener(|this, _, _, cx| {
            this.scheduled_section_open = !this.scheduled_section_open;
            cx.notify();
        }))
        .child(crate::components::outline_disclosure(
            SharedString::from("outline-disclosure-scheduled"),
            !open,
            &header_theme,
        ))
        .child(crate::work_outline::outline_view::group_body(
            "scheduled",
            "Scheduled",
            summary,
            header_theme.text,
            &header_theme,
        ));
        let selected = self.state.read(cx).selected_chat.clone();
        let row_order: Vec<String> = shown
            .iter()
            .filter_map(|row| task_id_of(&row.chat).map(str::to_owned))
            .collect();
        let view = cx.entity_id();
        let mut list = div().flex().flex_col();
        for row in shown {
            let chat = row.chat;
            let Some(task_id) = task_id_of(&chat).map(str::to_owned) else {
                continue;
            };
            let title: SharedString = chat
                .scheduled_task_info
                .as_ref()
                .map(|info| info.task_name.clone())
                .filter(|name| !name.trim().is_empty())
                .or_else(|| chat.title.clone())
                .map(|name| transcript::single_line(&name))
                .unwrap_or_else(|| "Scheduled task".into())
                .into();
            let is_selected = selected.as_deref() == Some(chat.id.as_str());
            let row_theme = crate::components::outline_row_theme(theme, is_selected);
            let pinned_row = is_pinned(&chat);
            let status_label = match row.status {
                mast_proto::ChatIndicator::Working => "Working",
                mast_proto::ChatIndicator::AwaitingInput => "Needs you",
                mast_proto::ChatIndicator::Errored => "Failed",
                mast_proto::ChatIndicator::Completed => "Done",
                mast_proto::ChatIndicator::Idle => "Idle",
            };
            let mut details = vec![status_label.to_owned()];
            if is_selected {
                details.push("Current".into());
            }
            let body = crate::work_outline::outline_view::chat_summary_body(
                &format!("scheduled-{task_id}"),
                &title,
                crate::status_style::Status::from(row.status),
                details.join(" · "),
                None,
                &row_theme,
                view,
                cx,
            );
            let open_id = chat.id.clone();
            let menu_id = chat.id.clone();
            let element = crate::components::outline_row(
                SharedString::from(format!("outline-row-scheduled-{task_id}")),
                height,
                crate::work_outline::outline_view::OutlineView::indent(1),
                &row_theme,
            )
            .when(pinned_row && !is_selected, |el| el.bg(row_theme.accent_wash))
            .when(is_selected, |el| el.bg(row_theme.element_hover))
            .when(!is_selected, |el| {
                el.hover(|el| el.bg(row_theme.element_hover.opacity(0.6)))
            })
            .when(chat.archived, |el| el.opacity(0.6))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_chat(open_id.clone(), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let closeable = crate::chat_closeout::is_closeout_candidate(
                        this.state
                            .read(cx)
                            .chats
                            .iter()
                            .find(|c| c.id == menu_id)
                            .and_then(|c| c.cwd.as_deref()),
                    );
                    this.chat_menu.open(ChatMenuState {
                        chat_id: menu_id.clone(),
                        position: event.position,
                        page: ChatMenuPage::Root,
                        closeable,
                    });
                    this.load_branch_review_target(menu_id.clone(), cx);
                    cx.notify();
                }),
            )
            .child(crate::components::outline_disclosure_space())
            .when(pinned_row, |el| {
                el.child(
                    div().flex_none().child(
                        crate::icons::icon(crate::icons::PIN)
                            .size(px(12.0))
                            .text_color(if is_selected {
                                row_theme.text
                            } else {
                                row_theme.pinned_accent()
                            }),
                    ),
                )
            })
            .child(body);
            let element = crate::components::outline_row_selection(element, &row_theme, is_selected);
            list = list.child(Self::scheduled_drag_row(
                element.into_any_element(),
                task_id,
                title,
                row_order.clone(),
                theme.accent,
                cx,
            ));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .child(header)
                .child(list)
                .into_any_element(),
        )
    }

    pub(in crate::shell) fn render_scheduled_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let order = self.scheduled_task_order(cx);
        let pinned = self.scheduled_pinned_tasks(cx);
        let filter = self.settings.space_filter.clone();
        let rows: Vec<ActiveChatRow> = {
            let state = self.state.read(cx);
            state
                .scheduled_sidebar_chats(now, filter.as_deref(), &order, &pinned)
                .into_iter()
                .map(|(status, chat)| self.sidebar_chat_data(status, chat.clone(), state))
                .collect()
        };
        if rows.is_empty() {
            return None;
        }
        let open = self.scheduled_section_open;
        // With "Pinned items are always visible", pinned tasks sit under the
        // header whether or not the section is collapsed.
        let always_visible = self.settings.pinned_always_visible;
        let pinned_outside: Vec<bool> = rows
            .iter()
            .map(|row| {
                always_visible
                    && task_id_of(&row.chat).is_some_and(|id| pinned.iter().any(|p| p == id))
            })
            .collect();
        let heights: Vec<f32> = rows
            .iter()
            .zip(&pinned_outside)
            .filter(|(_, outside)| !**outside)
            .map(|(row, _)| {
                sidebar_row_height(
                    self.settings.sidebar_compact,
                    self.settings.sidebar_show_project_label,
                    row.branch.is_some(),
                    row.change_request.is_some(),
                    crate::ui_style::current(cx).tokens,
                )
            })
            .collect();
        let hidden = heights.len();
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + heights.iter().sum::<f32>()
            + SIDEBAR_LIST_GAP * hidden.saturating_sub(1) as f32;
        // The count covers only the rows a collapse hides.
        let label: SharedString = scheduled_header_label(open, hidden).into();
        let chevron = self.sidebar_disclosure_chevron(SCHEDULED_SECTION_KEY, open, theme);
        let header = sidebar_disclosure_header(theme, label, chevron)
            .id("scheduled-section-toggle")
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.scheduled_section_open;
                this.begin_sidebar_disclosure_motion(
                    SCHEDULED_SECTION_KEY,
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.scheduled_section_open = !was_open;
                cx.notify();
            }));
        let selected = self.state.read(cx).selected_chat.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
            .gap(px(SIDEBAR_LIST_GAP));
        let has_pinned_outside = pinned_outside.iter().any(|outside| *outside);
        let has_body_rows = pinned_outside.iter().any(|outside| !*outside);
        let mut pinned_list = div().flex().flex_col().gap(px(SIDEBAR_LIST_GAP));
        let row_order: Vec<String> = rows
            .iter()
            .filter_map(|row| task_id_of(&row.chat).map(str::to_owned))
            .collect();
        for (row, outside) in rows.into_iter().zip(pinned_outside) {
            let chat = row.chat;
            let Some(task_id) = task_id_of(&chat).map(str::to_owned) else {
                continue;
            };
            let title: SharedString = chat
                .scheduled_task_info
                .as_ref()
                .map(|info| info.task_name.clone())
                .filter(|name| !name.trim().is_empty())
                .or_else(|| chat.title.clone())
                .map(|name| transcript::single_line(&name))
                .unwrap_or_else(|| "Scheduled task".into())
                .into();
            let is_selected = selected.as_deref() == Some(chat.id.as_str());
            let harness = self
                .settings
                .sidebar_show_harness
                .then(|| chat.config.as_ref().map(|c| c.harness))
                .flatten();
            let element = self.render_chat_row_with_chat(
                chat.id.clone(),
                None,
                title.clone(),
                format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into(),
                row.folder.into(),
                row.branch.map(SharedString::from),
                row.change_request,
                harness,
                row.status,
                is_selected,
                chat.archived,
                false,
                None,
                None,
                None,
                Some(&chat),
                theme,
                cx,
            );
            let element = Self::scheduled_drag_row(
                element.into_any_element(),
                task_id,
                title,
                row_order.clone(),
                theme.accent,
                cx,
            );
            if outside {
                pinned_list = pinned_list.child(element);
            } else {
                list = list.child(element);
            }
        }
        let body = self.render_sidebar_disclosure_body(
            SCHEDULED_SECTION_KEY,
            open,
            body_height,
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(list)
                .into_any_element(),
        );
        Some(
            div()
                .flex()
                .flex_col()
                .child(header)
                .when(has_pinned_outside, |section| {
                    section.child(pinned_list.pt(px(SIDEBAR_DISCLOSURE_BODY_INSET)))
                })
                .when(has_body_rows, |section| section.child(body))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{reorder_onto, scheduled_header_label, scheduled_overview_summary};

    #[test]
    fn overview_summary_counts_hidden_rows_only_when_collapsed() {
        assert_eq!(scheduled_overview_summary(true, 3, 0), "3 tasks");
        assert_eq!(scheduled_overview_summary(true, 1, 0), "1 task");
        assert_eq!(scheduled_overview_summary(false, 3, 2), "2 hidden");
        assert_eq!(scheduled_overview_summary(false, 2, 0), "2 pinned");
    }

    #[test]
    fn header_count_covers_only_hidden_rows() {
        assert_eq!(scheduled_header_label(true, 3), "Scheduled");
        assert_eq!(scheduled_header_label(false, 3), "Scheduled (3)");
        // Everything left is a visible pin, so nothing is hidden to count.
        assert_eq!(scheduled_header_label(false, 0), "Scheduled");
    }

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn dropping_downward_lands_after_the_target() {
        assert_eq!(
            reorder_onto(&ids(&["a", "b", "c"]), "a", "b"),
            Some(ids(&["b", "a", "c"]))
        );
    }

    #[test]
    fn dropping_upward_lands_before_the_target() {
        assert_eq!(
            reorder_onto(&ids(&["a", "b", "c"]), "c", "a"),
            Some(ids(&["c", "a", "b"]))
        );
    }

    #[test]
    fn dropping_on_itself_or_an_unknown_row_is_ignored() {
        assert_eq!(reorder_onto(&ids(&["a", "b"]), "a", "a"), None);
        assert_eq!(reorder_onto(&ids(&["a", "b"]), "a", "z"), None);
    }
}
