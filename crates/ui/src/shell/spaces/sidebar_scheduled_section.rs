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
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(crate::typography::ui_rems(12.0))
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

impl Shell {
    /// Task IDs in the user's saved order, with moves still in flight applied.
    fn scheduled_task_order(&self, cx: &App) -> Vec<String> {
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

    /// Task IDs of the Scheduled rows exactly as drawn, top to bottom.
    pub(in crate::shell) fn scheduled_visible_task_ids(&self, cx: &App) -> Vec<String> {
        let order = self.scheduled_task_order(cx);
        self.state
            .read(cx)
            .scheduled_sidebar_chats(Utc::now(), self.settings.space_filter.as_deref(), &order)
            .into_iter()
            .filter_map(|(_, chat)| task_id_of(chat).map(str::to_owned))
            .collect()
    }

    /// Chat ids of the Scheduled rows in display order, for keyboard cycling.
    pub(in crate::shell) fn scheduled_visible_chat_ids(&self, cx: &App) -> Vec<String> {
        if !self.scheduled_section_open {
            return Vec::new();
        }
        let order = self.scheduled_task_order(cx);
        self.state
            .read(cx)
            .scheduled_sidebar_chats(Utc::now(), self.settings.space_filter.as_deref(), &order)
            .into_iter()
            .map(|(_, chat)| chat.id.clone())
            .collect()
    }

    pub(in crate::shell) fn move_scheduled_task(
        &mut self,
        task_id: &str,
        target_id: &str,
        cx: &mut Context<Self>,
    ) {
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

    pub(in crate::shell) fn render_scheduled_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let order = self.scheduled_task_order(cx);
        let filter = self.settings.space_filter.clone();
        let rows: Vec<ActiveChatRow> = {
            let state = self.state.read(cx);
            state
                .scheduled_sidebar_chats(now, filter.as_deref(), &order)
                .into_iter()
                .map(|(status, chat)| self.sidebar_chat_data(status, chat.clone(), state))
                .collect()
        };
        if rows.is_empty() {
            return None;
        }
        let open = self.scheduled_section_open;
        let heights: Vec<f32> = rows
            .iter()
            .map(|row| {
                sidebar_row_height(
                    self.settings.sidebar_compact,
                    self.settings.sidebar_show_project_label,
                    row.branch.is_some(),
                    row.change_request.is_some(),
                    crate::ui_style::current(cx).tokens,
                )
            })
            .collect();
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + heights.iter().sum::<f32>()
            + SIDEBAR_LIST_GAP * rows.len().saturating_sub(1) as f32;
        let total = rows.len();
        let label: SharedString = if open {
            "Scheduled".into()
        } else {
            format!("Scheduled ({total})").into()
        };
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
        let row_order: Vec<String> = rows
            .iter()
            .filter_map(|row| task_id_of(&row.chat).map(str::to_owned))
            .collect();
        for row in rows {
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
            let payload = ScheduledRowDrag {
                task_id: task_id.clone(),
                title: title.clone(),
            };
            let accent = theme.accent;
            let row_order = row_order.clone();
            let own_id = task_id.clone();
            list = list.child(
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
                    .child(element),
            );
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
        Some(div().flex().flex_col().child(header).child(body).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::reorder_onto;

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
