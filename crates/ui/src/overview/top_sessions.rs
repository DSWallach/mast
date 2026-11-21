//! Fixed entry points to the assistant and selected chat, outside overview filters.
use super::*;

impl Overview {
    pub(super) fn render_top_sessions(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (assistant, current) = {
            let state = self.state.read(cx);
            let assistant = state.personal_assistant.current(&state.chats).cloned();
            let current = state
                .selected_chat_row()
                .filter(|chat| !chat.is_background_task_run())
                .cloned();
            (assistant, current)
        };
        let same = assistant
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(a, c)| a.id == c.id);
        if self.in_sidebar && theme.ui_style.behavior.compact_overview_navigation {
            let show_assistant = self.compact_show_assistant || current.is_none();
            let chosen = if show_assistant {
                assistant.as_ref()
            } else {
                current.as_ref()
            };
            let switcher = div().flex().gap(px(4.0)).children(
                [(false, "Current"), (true, "Assistant")]
                    .into_iter()
                    .map(|(value, label)| {
                        crate::components::outline_toolbar_button(
                            if value {
                                "compact-assistant"
                            } else {
                                "compact-current"
                            },
                            label.to_string(),
                            show_assistant == value,
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.compact_show_assistant = value;
                            cx.notify();
                        }))
                    }),
            );
            let card = match chosen {
                Some(chat) => self.top_session_card(
                    chat,
                    if show_assistant {
                        "Personal assistant"
                    } else {
                        "Current session"
                    },
                    if show_assistant {
                        "overview-assistant"
                    } else {
                        "overview-current-session"
                    },
                    theme,
                    cx,
                ),
                None => self.render_assistant_chooser(theme, cx),
            };
            return div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(switcher)
                .child(card)
                .into_any_element();
        }
        let assistant_card = match assistant {
            Some(chat) => self.top_session_card(
                &chat,
                if same {
                    "Personal assistant · Current session"
                } else {
                    "Personal assistant"
                },
                "overview-assistant",
                theme,
                cx,
            ),
            None => self.render_assistant_chooser(theme, cx),
        };
        div()
            .id("overview-top-sessions")
            .debug_selector(|| "overview-top-sessions".into())
            .flex()
            .flex_wrap()
            .when(self.in_sidebar, |el| el.flex_col())
            .gap(px(8.0))
            .child(assistant_card)
            .when_some(current.filter(|_| !same), |el, chat| {
                el.child(self.top_session_card(
                    &chat,
                    "Current session",
                    "overview-current-session",
                    theme,
                    cx,
                ))
            })
            .into_any_element()
    }

    fn top_session_card(
        &self,
        chat: &Chat,
        label: &'static str,
        id: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let status = state.display_status_for(chat, Utc::now());
        let project = chat
            .space_id
            .as_deref()
            .and_then(|id| state.space_row(id))
            .map(|space| space.display_name().to_string());
        let (_, status_label) = status_style(status);
        let color = crate::status_style::status_style(status.into(), theme.appearance).color;
        let mut detail = vec![status_label.to_string()];
        detail.extend(project);
        detail.extend(chat.branch.clone());
        if self
            .sidebar_scope
            .as_deref()
            .is_some_and(|scope| chat.space_id.as_deref() != Some(scope))
        {
            detail.push("Outside selected Space".into());
        }
        if chat.archived {
            detail.push("Archived".into());
        }
        let chat_id = chat.id.clone();
        let full_id = chat_id.clone();
        let reveal_id = chat_id.clone();
        let menu_id = chat_id.clone();
        let indicator = crate::components::outline_chat_indicator(
            status.into(),
            format!("{id}-status"),
            color,
            theme,
            cx.entity_id(),
            cx,
        );
        let compact = self.in_sidebar && theme.ui_style.behavior.compact_overview_navigation;
        let full_button = crate::components::overview_session_label(theme)
            .id(SharedString::from(format!("{id}-full")))
            .debug_selector(move || format!("{id}-full"))
            .hover(|el| el.text_color(theme.accent))
            .child(if self.in_sidebar { "↗" } else { "Open full chat ↗" })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.leave_work_item_details(DetailsExit::OpenFullChat(full_id.clone()), cx);
            }));
        let mut full_button = Some(full_button);
        div()
            .id(id)
            .debug_selector(move || id.into())
            .when(!self.in_sidebar, |el| el.flex_1())
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .hover(|el| el.bg(theme.element_hover))
            .cursor_pointer()
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.reveal_top_session(&reveal_id, window, cx);
                    } else {
                        this.open_top_session(chat_id.clone(), cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.open_session_menu(menu_id.clone(), event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .when(!compact, |el| el.child(
                div().flex().items_center().justify_between().gap(px(8.0))
                    .child(crate::components::overview_session_label(theme).flex_1().min_w_0().truncate().child(label))
                    .when(!compact, |el| el.children(full_button.take()))
            ))
            .child(
                div().flex().items_center().gap(px(6.0)).child(indicator)
                    .child(crate::components::overview_session_title().flex_1().min_w_0().truncate()
                        .text_color(theme.text).child(SharedString::from(crate::transcript::single_line(
                            chat.title.as_deref().unwrap_or("New session"),
                        ))))
                    .when(compact, |el| el.children(full_button.take()))
            )
            .child(
                crate::components::overview_session_detail()
                    .truncate()
                    .text_color(color)
                    .child(SharedString::from(detail.join(" · "))),
            )
            .into_any_element()
    }

    pub(super) fn reveal_top_session(&mut self, chat_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = OutlineChatReturn { chat_id: chat_id.to_string(), full_route: false };
        self.focus_outline_on_chat(Some(target.clone()), window, cx);
        let key = row_key::chat(chat_id);
        let visible = self.outline.update(cx, |outline, cx| outline.reveal(&key, cx));
        if !visible {
            self.clear_sidebar_filters(cx);
            self.set_sidebar_scope(None, cx);
            if let Some(shell) = self.shell.upgrade() {
                shell.update(cx, |shell, cx| shell.set_space_filter(None, cx));
            }
            self.focus_outline_on_chat(Some(target), window, cx);
        }
    }

    fn open_top_session(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.leave_work_item_details(DetailsExit::OpenChat(chat_id), cx);
    }

    pub(super) fn open_session_menu(
        &self,
        chat_id: String,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_overview_chat_menu(chat_id, position, cx)
        });
    }

    pub(crate) fn assistant_chooser_open(&self) -> bool {
        self.assistant_chooser.is_open()
    }

    pub(crate) fn dismiss_assistant_chooser(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.assistant_chooser.is_open() {
            return false;
        }
        self.close_assistant_chooser(cx);
        true
    }

    pub(super) fn close_assistant_chooser(&mut self, cx: &mut Context<Self>) {
        if self.assistant_chooser.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.assistant_chooser);
        }
        cx.notify();
    }

    fn render_assistant_chooser(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let trigger = crate::components::overview_session_label(theme)
            .id("overview-choose-assistant")
            .debug_selector(|| "overview-choose-assistant".into())
            .when(!self.in_sidebar, |el| el.flex_1())
            .min_w_0()
            .relative()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(4.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover))
            .child("Personal assistant")
            .child(
                crate::components::overview_session_title()
                    .text_color(theme.accent)
                    .child("Choose assistant…"),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.assistant_chooser.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.assistant_chooser.take_press_was_open() {
                    this.close_assistant_chooser(cx);
                } else {
                    this.assistant_chooser.open(());
                    cx.notify();
                }
            }));
        if self.assistant_chooser.get().is_none() {
            return trigger.into_any_element();
        }
        let popup = theme.for_popup();
        let state = self.state.read(cx);
        let candidates: Vec<_> = state
            .overview_chats(Utc::now())
            .into_iter()
            .filter(|(_, chat)| chat.space_id.as_deref().is_some_and(|id| !id.is_empty()))
            .map(|(_, chat)| {
                let chat_id = chat.id.clone();
                let project = chat
                    .space_id
                    .as_deref()
                    .and_then(|id| state.space_row(id))
                    .map(|space| space.display_name())
                    .unwrap_or("");
                popover::menu_row(
                    &popup,
                    false,
                    format!("overview-assistant-choice-{chat_id}"),
                )
                .id(SharedString::from(format!(
                    "overview-assistant-choice-{chat_id}"
                )))
                .debug_selector({
                    let id = chat_id.clone();
                    move || format!("overview-assistant-choice-{id}")
                })
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(SharedString::from(
                            crate::transcript::single_line(
                                chat.title.as_deref().unwrap_or("New session"),
                            ),
                        )))
                        .child(
                            div()
                                .truncate()
                                .text_color(popup.text_muted)
                                .child(SharedString::from(project.to_string())),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_assistant_chooser(cx);
                    this.state
                        .update(cx, |state, cx| state.set_personal_assistant(&chat_id, cx));
                }))
                .into_any_element()
            })
            .collect();
        let empty = candidates.is_empty();
        let card = popover::popover_card(&popup)
            .id("overview-assistant-chooser-card")
            .w(px(300.0))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_assistant_chooser(cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id("overview-assistant-choices")
                    .max_h(px(280.0))
                    .overflow_y_scroll()
                    .children(candidates)
                    .when(empty, |el| {
                        el.child(
                            div()
                                .p(px(8.0))
                                .text_color(popup.text_muted)
                                .child("Start a chat in a Space to choose an assistant."),
                        )
                    }),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-assistant-chooser",
                card.into_any_element(),
                self.assistant_chooser.closing_since(),
            ))
            .into_any_element()
    }
}
