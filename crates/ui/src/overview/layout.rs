//! Layout boundaries for the Overview navigation and workspace panels.
//!
//! Keep transcript/composer rendering in `render_workspace`. The navigation
//! can then move into the shell's left column without mounting a live chat
//! twice. Both surfaces use the same Overview controller and persisted state.
use super::*;

impl Overview {
    pub(crate) fn render_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        self.ensure_prefs_loaded(cx);
        if std::mem::take(&mut self.outline_focus_pending)
            && self.view_mode == OverviewMode::Outline
        {
            let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
            window.focus(&handle, cx);
        }
        // The chat open in the side panel — or, in peek mode, the parent of
        // the subagent being peeked.
        self.panel_highlight = self
            .subagent_peek
            .as_ref()
            .map(|p| p.chat_id.clone())
            .or_else(|| self.panel_chat_id(cx));
        let rows = self.rows(cx);
        self.declare_outline_spaces(cx);
        if self.view_mode == OverviewMode::Outline {
            self.sync_outline(cx);
        }

        let new_chat_button = self.render_new_chat_button(&theme, cx);

        let title_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(14.0))
                            .text_color(theme.text)
                            .child(SharedString::from("Overview")),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("{} chats", rows.len()))),
                    )
                    .when(
                        self.needs_interaction_count > 0
                            || self.attention_filter == AttentionFilter::NeedsInteraction,
                        |el| {
                            let active = self.attention_filter == AttentionFilter::NeedsInteraction;
                            let color = crate::status_style::status_style(
                                crate::status_style::Status::Waiting,
                                theme.appearance,
                            )
                            .color;
                            let label = if self.needs_interaction_count == 1 {
                                "1 needs interaction".to_string()
                            } else {
                                format!("{} need interaction", self.needs_interaction_count)
                            };
                            el.child(
                                div()
                                    .id("overview-filter-needs-interaction")
                                    .debug_selector(|| "overview-filter-needs-interaction".into())
                                    .px(px(7.0))
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(if active { color } else { color.opacity(0.3) })
                                    .bg(color.opacity(if active { 0.25 } else { 0.12 }))
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(color)
                                    .child(SharedString::from(label))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.attention_filter = if this.attention_filter
                                            == AttentionFilter::NeedsInteraction
                                        {
                                            AttentionFilter::All
                                        } else {
                                            AttentionFilter::NeedsInteraction
                                        };
                                        this.rows_dirty = true;
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .when(
                        self.stalled_count > 0 || self.attention_filter == AttentionFilter::Stalled,
                        |el| {
                            let active = self.attention_filter == AttentionFilter::Stalled;
                            let color = crate::status_style::status_style(
                                crate::status_style::Status::Errored,
                                theme.appearance,
                            )
                            .color;
                            let label = format!("{} stalled", self.stalled_count);
                            el.child(
                                div()
                                    .id("overview-filter-stalled")
                                    .debug_selector(|| "overview-filter-stalled".into())
                                    .px(px(7.0))
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(if active { color } else { color.opacity(0.3) })
                                    .bg(color.opacity(if active { 0.25 } else { 0.12 }))
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(color)
                                    .child(SharedString::from(label))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.attention_filter =
                                            if this.attention_filter == AttentionFilter::Stalled {
                                                AttentionFilter::All
                                            } else {
                                                AttentionFilter::Stalled
                                            };
                                        this.rows_dirty = true;
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .child(
                        div()
                            .id("overview-search")
                            .w(px(260.0))
                            .on_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    this.search_key_down(event, window, cx)
                                },
                            ))
                            .hint("Search", HintKey::Find)
                            .child(crate::components::toolbar_search_frame(
                                &theme,
                                self.search.clone().into_any_element(),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    // Leftmost of the header's action cluster — the overview's
                    // one clearly-primary button (see `render_new_chat_button`).
                    .child(new_chat_button)
                    .child(
                        div()
                            .id("overview-pull-requests")
                            .debug_selector(|| "overview-pull-requests".into())
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .hover(|el| el.text_color(theme.text))
                            .when(true, |el| {
                                crate::components::toolbar_control(el, &theme, false)
                            })
                            .child("Pull requests")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(shell) = this.shell.upgrade() {
                                    shell.update(cx, |shell, cx| {
                                        shell.open_pr_review_chats_focused(window, cx)
                                    });
                                }
                            })),
                    ),
            );

        let filters = self.render_filter_menu(&theme, cx);
        let repo_filter = self.render_repo_filter(&theme, cx);
        let outline_mode = self.view_mode == OverviewMode::Outline;
        let work_item_filter = (!outline_mode).then(|| self.render_work_item_filter(&theme, cx));
        let mut toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(filters)
            .child(repo_filter)
            .when_some(work_item_filter, |el, filter| el.child(filter));
        if outline_mode {
            toolbar = toolbar.children(self.render_outline_toolbar(&theme, cx));
        } else {
            // Composable dimension chips: click toggles membership, order
            // clicked = nesting order (`self.active_groups`). A chip shows
            // its position in that order (1-based) while active, so the
            // nesting is legible without opening a menu.
            let dims: [(GroupDimension, &'static str); 2] = [
                (GroupDimension::Repo, "Repo"),
                (GroupDimension::Ticket, "PR / ticket"),
            ];
            let chips = dims.into_iter().map(|(dim, label)| {
                let order = self.active_groups.iter().position(|&d| d == dim);
                let active = order.is_some();
                let text = match order {
                    Some(ix) => format!("{label} {}", ix + 1),
                    None => label.to_string(),
                };
                crate::components::overview_group_button(
                    SharedString::from(format!("overview-group-{label}")),
                    active,
                    &theme,
                )
                .child(SharedString::from(text))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.toggle_group(dim, cx);
                        cx.notify();
                    }),
                )
            });
            toolbar = toolbar.child(
                crate::components::overview_group_frame(&theme)
                    .child(
                        div()
                            .px(px(5.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(SharedString::from("Group by")),
                    )
                    .children(chips),
            );
        }

        let header = crate::components::outline_overview_header(&theme)
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(title_row)
            .child(toolbar);

        let body = match self.view_mode {
            OverviewMode::List => {
                let body = self.render_list_body(&rows, &theme, cx);
                self.surface_focus_wrapper(body, cx)
            }
            OverviewMode::Outline => self.render_outline_body(cx),
        };
        // A focus the body couldn't use (target filtered out / not a row)
        // is dropped, never replayed on a later frame.
        self.focus_due = None;

        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body);

        main.into_any_element()
    }

    /// Compose workspace panels around the supplied navigation element.
    /// Keeping composition separate lets the shell relocate navigation while
    /// retaining the existing panel lifecycle, sizing, and Escape behavior.
    pub(crate) fn render_workspace(
        &mut self,
        navigation: gpui::AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        // One right-side panel. Work Item Details, while open, renders in the
        // slot without touching the chat panel / peek state underneath, so
        // closing it brings back exactly what was there.
        // The Linear panel is NOT in this slot: it has its own left column
        // (below), so it shows alongside whichever of these is up.
        let (panel, right_w) = if let Some(details) = self.work_item_details.clone() {
            let space_id = details.read(cx).space_id.clone();
            let snapshot = self.outline_snapshot(&space_id, cx);
            let chats = self.outline_chats.clone();
            details.update(cx, |details, _| details.set_data(snapshot, chats));
            let width = self.shown_details_width();
            let panel = self.details_slot(details.into_any_element(), width, &theme, cx);
            (Some(panel), width)
        } else if let Some(list) = self.required_actions_panel.clone() {
            let spaces = self.required_action_spaces(cx);
            let chats = self.outline_chats.clone();
            list.update(cx, |list, _| list.set_data(spaces, chats));
            let width = self.shown_details_width();
            let panel = self.details_slot(list.into_any_element(), width, &theme, cx);
            (Some(panel), width)
        } else {
            let panel = if self.subagent_peek.is_some() {
                self.render_subagent_peek(&theme, cx)
            } else {
                self.render_chat_panel(&theme, cx)
            };
            let width = if panel.is_some() { CHAT_PANEL_W } else { 0.0 };
            (panel, width)
        };
        let linear = self.render_linear_column(right_w, window, cx);
        let (linear_inline, linear_overlay) = match linear {
            Some(linear_host::LinearSlot::Inline(el)) => (Some(el), None),
            Some(linear_host::LinearSlot::Overlay(el)) => (None, Some(el)),
            None => (None, None),
        };

        // `[Linear | main | right panel]`: the side panels are
        // `flex_none` widths and `main` is `flex_1().min_w_0()`, so the
        // list/Outline is what shrinks. The Linear column yields
        // first (down to its minimum, then it overlays `main`) so the body
        // keeps `linear_host::OVERVIEW_BODY_MIN_W`.
        div()
            .size_full()
            .relative()
            .flex()
            .child(self.linear_width_capture())
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<details_width::DetailsPanelResize>, _, cx| {
                    this.details_resize_drag(f32::from(event.event.position.x), cx)
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<linear_host::LinearPanelResize>, _, cx| {
                    this.linear_resize_drag(f32::from(event.event.position.x), cx)
                },
            ))
            // Escape closes a subagent peek from anywhere in the overview
            // (the peek panel's own handler covers focus inside it; this
            // covers focus elsewhere here, e.g. the search box).
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if let Some(details) = this.work_item_details.clone() {
                    details.update(cx, |details, cx| details.request_close(cx));
                    cx.stop_propagation();
                } else if this.required_actions_panel.is_some() {
                    this.close_required_actions(cx);
                    cx.stop_propagation();
                } else if this.subagent_peek.is_some() {
                    this.close_subagent_peek(cx);
                    cx.stop_propagation();
                }
            }))
            .when_some(linear_inline, |el, linear| el.child(linear))
            .child(navigation)
            .when_some(panel, |el, panel| el.child(panel))
            .when_some(linear_overlay, |el, linear| {
                el.child(div().absolute().top_0().bottom_0().left_0().child(linear))
            })
            .children(self.render_project_issue_picker(cx))
            .into_any_element()
    }
}
