use super::*;

impl Overview {
    /// Wrap the List body in its keyboard focus target (Esc / Enter /
    /// Down from the search land on it; List arrows and Enter run here).
    pub(super) fn surface_focus_wrapper(
        &self,
        body: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("overview-surface")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.body_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.list_key_down(event, window, cx)
            }))
            .child(body)
            .into_any_element()
    }

    /// The overview's one clearly-primary action: reuses the exact same
    /// new-session flow as the titlebar `+` / `mod-n` (`Shell::open_new_session`,
    /// via the `open_new_session_from_overview` wrapper — `open_new_session`
    /// itself is private to `crate::shell`). Clicking it leaves the overview
    /// for the blank new-chat composer screen; that's intentional, since the
    /// project/device pickers and the worktree chip live there, not here.
    ///
    /// As the overview becomes home base, THIS is the intended anchor point
    /// for future overview-level new-chat UX (e.g. surfacing the new-chat
    /// screen's worktree chip or pickers without leaving the overview) —
    /// per the user's stated intent. A later pass may grow this into
    /// something richer in place; it should not need a second "new chat"
    /// entrypoint added elsewhere.
    pub(super) fn render_new_chat_button(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let shortcut = self
            .shell
            .upgrade()
            .map(|shell| shell.read(cx).new_session_shortcut_label());
        let tooltip: SharedString = match shortcut {
            Some(combo) => format!("Start a new chat ({combo})").into(),
            None => "Start a new chat".into(),
        };
        popover::btn_primary(theme, "+ New chat")
            .id("overview-new-chat")
            .when(true, |el| {
                crate::components::toolbar_control(el, theme, true)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                let _ = this.shell.update(cx, |shell, cx| {
                    shell.open_new_session_from_overview(cx);
                });
            }))
            .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
            .into_any_element()
    }

    /// "Repos" toolbar control + its dropdown (web `#repo-filter-btn`/
    /// `#repo-filter-panel`, `session_canvas.html:2082-2148`): every repo
    /// seen across rows (pre-filter, so unchecking one never hides its own
    /// checkbox), a checkbox + hash-color swatch per repo, All/None bulk
    /// actions. Closes on any press outside the card (the web's
    /// document-level click listener).
    pub(super) fn render_repo_filter(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hidden = self.hidden_repos.len();
        let active = hidden > 0 || self.repo_filter.is_open();
        let trigger = div()
            .id("overview-repo-filter")
            .relative()
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .bg(theme.element_hover.opacity(0.5))
            .when(active, |el| {
                el.bg(theme.element_active).text_color(theme.text)
            })
            .when(!active, |el| el.text_color(theme.text_muted))
            .hover(|el| el.text_color(theme.text))
            .when(true, |el| {
                crate::components::toolbar_control(el, theme, false)
            })
            .child(SharedString::from(format!(
                "{} ▾",
                repo_filter_label(hidden)
            )))
            .hint("Filter by repository", HintKey::None)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.repo_filter.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.repo_filter.take_press_was_open() {
                    this.close_repo_filter(cx);
                } else {
                    this.repo_filter.open(());
                    cx.notify();
                }
            }));
        if self.repo_filter.get().is_none() {
            return trigger.into_any_element();
        }
        let closing = self.repo_filter.closing_since();

        let popup = theme.for_popup();
        let bulk = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(popup.text_muted)
                .border_1()
                .border_color(popup.border)
                .hover(|el| el.bg(popup.element_hover).text_color(popup.text))
                .child(SharedString::from(label))
        };
        let keys = sorted_repo_keys(&self.known_repos);
        let list: Vec<gpui::AnyElement> = if keys.is_empty() {
            vec![
                div()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(popup.text_muted)
                    .child(SharedString::from("No chats yet"))
                    .into_any_element(),
            ]
        } else {
            keys.into_iter()
                .enumerate()
                .map(|(ix, key)| {
                    let checked = !self.hidden_repos.contains(&key);
                    let color = group_color(GroupDimension::Repo, &key);
                    let label = group_label(GroupDimension::Repo, &key);
                    div()
                        .id(("overview-repo-filter-row", ix))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(popup.element_hover))
                        .child(
                            div()
                                .flex_none()
                                .size(px(12.0))
                                .rounded(px(3.0))
                                .border_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(crate::typography::ui_rems(9.0))
                                .when(checked, |el| {
                                    el.bg(popup.accent)
                                        .border_color(popup.accent)
                                        .text_color(gpui::white())
                                        .child("✓")
                                })
                                .when(!checked, |el| el.border_color(popup.border_strong)),
                        )
                        .child(div().flex_none().size(px(8.0)).rounded(px(2.0)).bg(color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(if checked {
                                    popup.text
                                } else {
                                    popup.text_muted
                                })
                                .child(SharedString::from(label)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            toggle_repo_hidden(&mut this.hidden_repos, &key);
                            this.repo_filter_changed(cx);
                        }))
                        .into_any_element()
                })
                .collect()
        };
        let card = popover::popover_card(&popup)
            .w(px(240.0))
            .max_h(px(360.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_repo_filter(cx)))
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .pb(px(4.0))
                    .child(
                        bulk("overview-repo-filter-all", "All").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.hidden_repos.clear();
                                this.repo_filter_changed(cx);
                            },
                        )),
                    )
                    .child(
                        bulk("overview-repo-filter-none", "None").on_click(cx.listener(
                            |this, _, _, cx| {
                                let known = this.known_repos.clone();
                                this.hidden_repos.extend(known);
                                this.repo_filter_changed(cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .id("overview-repo-filter-list")
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scroll()
                    .children(list),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-repo-filter-menu",
                card.into_any_element(),
                closing,
            ))
            .into_any_element()
    }

    pub(super) fn close_repo_filter(&mut self, cx: &mut Context<Self>) {
        if self.repo_filter.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.repo_filter);
        }
        cx.notify();
    }

    pub(super) fn render_filter_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let trigger = crate::components::outline_filter_trigger(theme)
            .hint("Overview filters", HintKey::None)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.filter_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.filter_menu.take_press_was_open() {
                    this.close_filter_menu(cx);
                } else {
                    this.filter_menu.open(());
                    cx.notify();
                }
            }));
        if self.filter_menu.get().is_none() {
            return trigger.into_any_element();
        }
        let popup = theme.for_popup();
        let row =
            |id, label, checked| crate::components::outline_filter_row(id, label, checked, &popup);
        let card = popover::popover_card(&popup)
            .w(px(180.0))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_filter_menu(cx)))
            .child(
                row("overview-hide-stale", "Hide stale", self.hide_stale).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.hide_stale = !this.hide_stale;
                        this.view_prefs_changed(cx);
                        this.rows_dirty = true;
                        cx.notify();
                    },
                )),
            )
            .child(
                row(
                    "overview-show-archived",
                    "Show archived",
                    self.show_archived,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_archived = !this.show_archived;
                    this.view_prefs_changed(cx);
                    this.rows_dirty = true;
                    cx.notify();
                })),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-filter-menu-popup",
                card.into_any_element(),
                self.filter_menu.closing_since(),
            ))
            .into_any_element()
    }

    pub(super) fn close_filter_menu(&mut self, cx: &mut Context<Self>) {
        if self.filter_menu.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.filter_menu);
        }
        cx.notify();
    }

    /// Web `#empty-message`, adapted: distinguishes "no chats at all" from
    /// "everything is filtered out" and says which filters are active.
    pub(super) fn render_empty_state(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        // No chats at all (as opposed to "everything's filtered out") is
        // exactly when a prominent way to start one belongs right here —
        // same button, same action as the header's.
        let truly_empty = self.unfiltered_count == 0;
        let (headline, detail) = if truly_empty {
            (
                if self.show_archived {
                    "No chats yet."
                } else {
                    "No active chats."
                },
                None,
            )
        } else {
            let mut active: Vec<String> = Vec::new();
            if !self.search.read(cx).text().trim().is_empty() {
                active.push("search".into());
            }
            if !self.hidden_repos.is_empty() {
                active.push(repo_filter_label(self.hidden_repos.len()).to_lowercase());
            }
            if self.hide_stale {
                active.push("hide stale".into());
            }
            let detail =
                (!active.is_empty()).then(|| format!("Active filters: {}.", active.join(", ")));
            ("Every chat is hidden by the current filters.", detail)
        };
        let new_chat_button = truly_empty.then(|| self.render_new_chat_button(theme, cx));
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .p(px(24.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(headline)),
            )
            .when_some(detail, |el, detail| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(detail)),
                )
            })
            .when_some(new_chat_button, |el, button| {
                el.child(div().mt(px(4.0)).child(button))
            })
            .into_any_element()
    }
}
