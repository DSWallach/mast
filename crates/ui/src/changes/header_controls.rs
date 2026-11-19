use super::*;

impl Changes {
    /// A small hover-washed icon button for the pane header. The header lives
    /// inside the titlebar drag strip, so the button occludes and swallows the
    /// mouse-down (same discipline as the shell's `header_icon_button`).
    pub(super) fn header_button(
        id: &'static str,
        icon_path: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        Self::header_toggle(id, icon_path, false, theme)
    }

    /// [`Self::header_button`] with a latched look: an `active` toggle holds
    /// the hover wash and the full text tone, so the pane says which layout
    /// it is in without a label.
    pub(super) fn header_toggle(
        id: &'static str,
        icon_path: &'static str,
        active: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        crate::components::changes_header_toggle(id, icon_path, active, theme)
    }

    /// The unified ⇄ split layout toggle (both the scoped and the
    /// commit-pinned toolbars carry it).
    pub(super) fn split_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-split",
            crate::icons::SPLIT_COLUMNS,
            self.mode.is_split(),
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_mode(cx);
        }))
        .into_any_element()
    }

    pub(super) fn wrap_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-wrap",
            crate::icons::WRAP_TEXT,
            self.wrap_lines,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_wrap(cx);
        }))
        .tooltip(|_, cx| {
            cx.new(|_| DiffHeaderTooltip("Wrap long lines".into()))
                .into()
        })
        .tooltip_show_delay(Duration::from_millis(350))
        .into_any_element()
    }

    pub(super) fn word_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle("changes-word-diff", crate::icons::PEN, self.word_diff, theme)
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.word_diff = !this.word_diff;
                let enabled = this.word_diff;
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| settings.diff_word = enabled);
                cx.notify();
            }))
            .tooltip(|_, cx| cx.new(|_| DiffHeaderTooltip("Highlight changed text".into())).into())
            .tooltip_show_delay(Duration::from_millis(350))
            .into_any_element()
    }

    /// The pane-header controls: scope dropdown, `{branch} → {base ⌄}` ref
    /// selector (branch scope), fold-all. Rendered BY THE SHELL inside the
    /// session titlebar's trailing section (the band above the pane) — the
    /// titlebar overlay owns that strip's hit-testing, so controls mounted
    /// under it would never see a click. The expand and close buttons ride
    /// alongside, shell-owned (they mutate shell state).
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // An unconfirmed bound-Chat member has no diff, so no diff actions.
        if self.pinned_cwd.is_none()
            && self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.unconfirmed.is_some())
        {
            return div()
                .debug_selector(|| "changes-header-unconfirmed".into())
                .into_any_element();
        }
        let theme = Theme::of(cx).clone();
        // Commit-pinned pane: the pin never changes, so a fixed identity
        // chip (mono short sha + subject) replaces the scope dropdown;
        // fold-all still trails.
        if let Some(commit) = self.commit.clone() {
            let short: String = commit.sha.chars().take(7).collect();
            return div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .flex_none()
                        .h(px(crate::surface_chrome::CONTROL_SIZE))
                        .px(px(Theme::SPACE_SM))
                        .when(true, |el| crate::components::changes_revision_label(el, &theme))
                        .flex()
                        .items_center()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(short)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(commit.subject.clone())),
                )
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element();
        }
        // Repo Map inspection is deliberately pinned to one checkout. Do not
        // expose the normal scope menu, which would imply it can follow the
        // currently selected chat or latest-turn state.
        if self.pinned_cwd.is_some() {
            return div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child("Uncommitted changes"),
                )
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element();
        }
        let scope = self.scope;
        let history_branch = (scope == DiffScope::History)
            .then(|| self.owner_branch().unwrap_or_else(|| "HEAD".to_string()));
        let history_count = (scope == DiffScope::History).then(|| self.history_count(cx));
        let history_search_control =
            (scope == DiffScope::History).then(|| self.history_search_control(cx));
        let history_fetch_button =
            (scope == DiffScope::History).then(|| self.history_fetch_button(cx));
        let history_view_button =
            (scope == DiffScope::History).then(|| self.history_view_button(cx));
        let scope_trigger = div()
            .id("changes-scope-trigger")
            .debug_selector(|| "changes-scope-trigger".into())
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(Theme::SPACE_SM))
            .max_w(px(250.0))
            .min_w_0()
            .flex_shrink(1.0)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-scope-trigger",
                crate::theme::wash(0.05),
                crate::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("changes-scope-trigger"))
            .when(true, |element| crate::components::changes_selector(element, &theme, crate::components::ChangesSelector::Scope))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.scope_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.scope_menu.take_press_was_open() {
                    this.close_scope_menu(cx);
                } else {
                    this.scope_menu.open(());
                }
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .when(true, crate::components::changes_body_text)
                    .line_height(px(14.0))
                    .text_color(theme.text)
                    .child(SharedString::from(scope.label())),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if scope == DiffScope::History {
            // The tab already identifies this as History; use the fixed title
            // slot for the current branch instead of repeating the surface name.
            div()
                .id("history-surface-title")
                .min_w_0()
                .max_w(px(160.0))
                .truncate()
                .h(px(crate::surface_chrome::CONTROL_SIZE))
                .px(px(Theme::SPACE_SM))
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.5))
                .line_height(px(14.0))
                .text_color(theme.text_dim)
                .child(SharedString::from(history_branch.unwrap_or_default()))
                .into_any_element()
        } else if scope == DiffScope::Range {
            // A branch / PR target has exactly one scope: name it ("PR #n" /
            // "branch <name>") instead of offering the scope menu.
            let label = self
                .owner
                .as_ref()
                .and_then(|owner| owner.range.as_ref())
                .map(|range| range.label.clone())
                .unwrap_or_else(|| scope.label().to_owned());
            div()
                .id("changes-range-title")
                .debug_selector(|| "changes-range-title".into())
                .h(px(crate::surface_chrome::CONTROL_SIZE))
                .px(px(Theme::SPACE_SM))
                .max_w(px(250.0))
                .min_w_0()
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .child(
                    crate::icons::icon(crate::icons::GIT_BRANCH)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when(true, crate::components::changes_body_text)
                        .line_height(px(14.0))
                        .text_color(theme.text)
                        .child(SharedString::from(label)),
                )
                .into_any_element()
        } else if self.scope_menu.get().is_some() {
            let closing = self.scope_menu.closing_since();
            let menu = self.render_scope_menu(&theme, cx);
            scope_trigger
                .relative()
                .child(popover::anchored_menu_below_gap(
                    "changes-scope-menu",
                    menu,
                    closing,
                    10.0,
                ))
                .into_any_element()
        } else {
            scope_trigger.into_any_element()
        };

        let trailing: AnyElement = if scope == DiffScope::History {
            div()
                .min_w_0()
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .children(history_search_control)
                .children(history_fetch_button)
                .children(history_view_button)
                .child(
                    Self::header_button("history-refresh", crate::icons::REFRESH, &theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.history_pane(cx)
                                .update(cx, |history, cx| history.refresh(cx));
                        }),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::surface_chrome::CONTROL_GAP))
            .child(trigger)
            .when_some(history_count, |element, count| {
                element.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .h(px(crate::surface_chrome::CONTROL_SIZE))
                        .flex()
                        .items_center()
                        .child(count),
                )
            })
            .children(self.render_ref_selector(&theme, cx))
            .when(scope != DiffScope::History, |element| {
                element.child(div().flex_1())
            })
            .child(trailing)
            .into_any_element()
    }

    pub(super) fn render_scope_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let current = self.scope;
        popover::popover_card(theme)
            .w(px(310.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_scope_menu(cx)))
            .child(
                // The 2px row gap every other menu carries — rows straight on
                // the card abutted, adjacent washes read as one slab (user
                // report).
                div().flex().flex_col().gap(px(Theme::SPACE_XS)).children(
                    DiffScope::ALL.into_iter().enumerate().map(|(ix, scope)| {
                        popover::menu_row(
                            theme,
                            scope == current,
                            format!("changes-scope-row-{ix}"),
                        )
                        .id(("changes-scope-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_scope(scope, cx);
                            this.close_scope_menu(cx);
                        }))
                        .child(div().flex_1().child(SharedString::from(scope.label())))
                    }),
                ),
            )
            .into_any_element()
    }

    /// `{branch} → {base ⌄}` — which ref the branch scope compares against
    /// (t3code's ref strip), inlined into the pane header. Branch scope only.
    pub(super) fn render_ref_selector(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.scope == DiffScope::Range {
            // Fixed pair, no base picker: `head → base` of the target.
            let range = self.owner.as_ref().and_then(|owner| owner.range.as_ref())?;
            return Some(
                div()
                    .id("changes-range-refs")
                    .debug_selector(|| "changes-range-refs".into())
                    .min_w_0()
                    .truncate()
                    .h(px(crate::surface_chrome::CONTROL_SIZE))
                    .flex()
                    .items_center()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(format!(
                        "{} \u{2192} {}",
                        range.head_branch, range.base_branch
                    )))
                    .into_any_element(),
            );
        }
        if self.scope != DiffScope::Branch {
            return None;
        }
        let branch = self.owner_branch().unwrap_or_else(|| "HEAD".to_string());
        let base = self.base_ref.clone().unwrap_or_else(|| "…".to_string());
        // Even truncation: taffy shrinks flex items ∝ factor × basis, and the
        // default factor of 1 splits the deficit proportionally to content —
        // a long branch stayed near-whole while a short base ("main") read as
        // a bare ellipsis (user report). Weighting each side's factor by its
        // own length SQUARED (mono font, so chars ∝ px) lands the deficit
        // ~cubically on the longer name: the short side's loss stays
        // sub-pixel even under a big deficit (a linear weight still cost it
        // a char), while equal lengths still split evenly.
        let branch_weight = (branch.chars().count().max(1) as f32).powi(2);
        let base_weight = (base.chars().count().max(1) as f32).powi(2);
        let trigger = div()
            .id("changes-ref-trigger")
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(6.0))
            // Shrinkable, like the branch label beside it — a flex_none
            // trigger with a long base name plowed over the header buttons
            // (user report); both sides truncate instead.
            .min_w_0()
            .flex_shrink(base_weight)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-ref-trigger",
                crate::theme::wash(0.0),
                crate::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-ref-trigger"))
            .when(true, |element| crate::components::changes_selector(element, theme, crate::components::ChangesSelector::Reference))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.ref_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.ref_menu.take_press_was_open() {
                    this.close_ref_menu(cx);
                    cx.notify();
                } else {
                    this.open_ref_menu(window, cx);
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(theme.text)
                    .child(SharedString::from(base)),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if self.ref_menu.get().is_some() {
            let closing = self.ref_menu.closing_since();
            let menu = self.render_ref_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_gap(
                "changes-ref-menu",
                menu,
                closing,
                10.0,
            ))
        } else {
            trigger
        };
        Some(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                // Extra room off the scope dropdown (row gap alone read
                // cramped — user report).
                .ml(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .min_w_0()
                        .flex_shrink(branch_weight)
                        .truncate()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.5))
                        .text_color(theme.text_dim)
                        .child(SharedString::from(branch)),
                )
                .child(
                    crate::icons::icon(crate::icons::ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint),
                )
                .child(trigger)
                .into_any_element(),
        )
    }

    pub(super) fn render_ref_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll) = {
            let Some(menu) = self.ref_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
            )
        };
        let rows = self.ref_menu_rows(cx);
        let current = self.base_ref.clone();
        let branches = self.branches.clone();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(px(Theme::SPACE_SM))
                .py(px(Theme::SPACE_SM))
                .when(true, crate::components::changes_body_text)
                .text_color(theme.text_faint)
                .child(SharedString::from(if branches.is_empty() {
                    "No branches"
                } else {
                    "No matching branches"
                }))
                .into_any_element()
        } else {
            div()
                .id("changes-ref-list")
                .flex()
                .flex_col()
                .gap(px(Theme::SPACE_XS))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, branch_ix)| {
                    let name = branches[branch_ix].clone();
                    let selected = current.as_deref() == Some(name.as_str());
                    let label = name.clone();
                    popover::menu_row_nav(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-ref-row-{row_ix}"),
                    )
                    .id(("changes-ref-row", row_ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_base_ref(name.clone(), cx);
                        this.close_ref_menu(cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.font_mono.clone())
                            .when(true, crate::components::changes_body_text)
                            .child(SharedString::from(label)),
                    )
                }))
                .into_any_element()
        };

        popover::popover_card(theme)
            .w(px(240.0))
            .track_focus(&focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.ref_menu_key(event, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_ref_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    /// The "Diff checkout..." button of the context chrome and, while open,
    /// its picker (`None` when the chat cannot pick a checkout).
    pub(super) fn render_checkout_picker(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.checkout_picker_available(cx) {
            return None;
        }
        let trigger = div()
            .id("changes-checkout-trigger")
            .debug_selector(|| "changes-checkout-trigger".into())
            .flex_none()
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .bg(motion::hover_blend(
                "changes-checkout-trigger",
                crate::theme::wash(0.0),
                crate::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-checkout-trigger"))
            .when(true, |element| crate::components::changes_selector(element, theme, crate::components::ChangesSelector::Target))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.checkout_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.checkout_menu.take_press_was_open() {
                    this.close_checkout_menu(cx);
                    cx.notify();
                } else {
                    this.open_checkout_menu(window, cx);
                }
            }))
            .child(
                crate::icons::icon(crate::icons::GIT_BRANCH)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from("Diff checkout\u{2026}"));
        let trigger = if self.checkout_menu.get().is_some() {
            let closing = self.checkout_menu.closing_since();
            let menu = self.render_checkout_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_end(
                "changes-checkout-menu",
                menu,
                closing,
            ))
        } else {
            trigger
        };
        Some(trigger.into_any_element())
    }

    pub(super) fn render_checkout_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll, state) = {
            let Some(menu) = self.checkout_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
                menu.candidates.clone(),
            )
        };
        let rows = self.checkout_menu_rows(cx);
        let note = |text: &'static str| {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(text))
                .into_any_element()
        };
        let list: AnyElement = match &state {
            popover::Loadable::Idle | popover::Loadable::Loading => {
                note("Looking for checkouts\u{2026}")
            }
            popover::Loadable::Error(message) => div()
                .p(px(4.0))
                .child(crate::error_notice::ErrorNotice::from_text(
                    "changes-checkout-list-error",
                    theme,
                    message,
                ))
                .into_any_element(),
            popover::Loadable::Ready(ranked) if rows.is_empty() => note(if ranked.is_empty() {
                "No checkouts found"
            } else {
                "No matching checkouts"
            }),
            popover::Loadable::Ready(ranked) => div()
                .id("changes-checkout-list")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(280.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, ranked_ix)| {
                    let row = ranked[ranked_ix].clone();
                    let tip: SharedString = row.candidate.path.clone().into();
                    let candidate = row.candidate.clone();
                    let selected = row.candidate.selected;
                    let removable = row.candidate.bound && row.candidate.kind != mast_proto::DiffCheckoutKind::Own;
                    let remove_path = row.candidate.path.clone();
                    popover::menu_row_nav(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-checkout-row-{row_ix}"),
                    )
                    .id(("changes-checkout-row", row_ix))
                    .debug_selector(move || format!("changes-checkout-row-{row_ix}"))
                    .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tip.clone())).into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_checkout(candidate.clone(), cx);
                    }))
                    .child(
                        // Check mark = the selected checkout; dot = bound.
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |el| {
                                el.child(
                                    crate::icons::icon(crate::icons::CHECK)
                                        .size(px(12.0))
                                        .text_color(theme.accent),
                                )
                            })
                            .when(!selected && row.candidate.bound, |el| {
                                el.child(
                                    div()
                                        .size(px(5.0))
                                        .rounded_full()
                                        .bg(theme.text_muted.opacity(0.7)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(12.0))
                                            .child(SharedString::from(row.label())),
                                    )
                                    .when(row.suggested, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .px(px(5.0))
                                                .when(true, |el| crate::components::changes_suggested_badge(el, theme))
                                                .text_size(px(9.5))
                                                .child("suggested"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(9.5))
                                            .text_color(theme.text_faint)
                                            .child(row.kind_tag()),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(10.5))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(crate::diff_targets::short_path(
                                        &row.candidate.path,
                                    ))),
                            ),
                    )
                    .when(removable, |element| element.child(
                        div().id(("changes-checkout-remove", row_ix))
                            .debug_selector(move || format!("changes-checkout-remove-{row_ix}"))
                            .flex_none().px(px(6.0)).text_size(px(11.0))
                            .text_color(theme.text_muted).child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_checkout(remove_path.clone(), cx);
                            }))
                    ))
                }))
                .into_any_element(),
        };

        popover::popover_card(theme)
            .w(px(340.0))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.checkout_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_checkout_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    /// The last failed `SetChatDiffTarget` write as a copyable error notice.
    pub(super) fn render_diff_target_error(&self, theme: &Theme, cx: &App) -> Option<AnyElement> {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return None;
        }
        let chat_id = self.owner_chat_id_owned()?;
        let message = self.state.read(cx).chat_diff_target_error(&chat_id)?;
        let state = self.state.clone();
        Some(
            div()
                .debug_selector(|| "changes-diff-target-error".into())
                .flex_none()
                .px(px(Theme::SPACE_LG))
                .py(px(Theme::SPACE_XS))
                .child(
                    crate::error_notice::ErrorNotice::from_text(
                        "changes-diff-target-error",
                        theme,
                        message,
                    )
                    .on_dismiss(move |_, cx| {
                        let chat_id = chat_id.clone();
                        state.update(cx, |state, cx| {
                            state.dismiss_chat_diff_target_error(&chat_id, cx)
                        });
                    }),
                )
                .into_any_element(),
        )
    }

    /// The scope label, plus the re-review count: "3 files changed since you
    /// reviewed" in the full diff, "Since abc1234" in the delta.
    fn header_label(&self, parsed: &ParsedDiff) -> String {
        let label = scope_label(self.scope, parsed.file_count, self.base_ref.as_deref());
        if let Some((old, _)) = self.rereview_range() {
            let files = if parsed.file_count == 1 { "file" } else { "files" };
            let short: String = old.chars().take(7).collect();
            return format!("{} {files} changed since {short}", parsed.file_count);
        }
        let changed = self.review_state.as_ref().map_or(0, |state| {
            changed_since_files(state, state.head_sha.as_deref(), &parsed.hunk_identities).len()
        });
        if changed > 0 {
            format!("{label} \u{b7} {changed} changed since you reviewed")
        } else {
            label
        }
    }

    pub(super) fn render_header_strip(&self, theme: &Theme) -> Option<AnyElement> {
        let parsed = self.parsed.as_ref()?;
        Some(
            div()
                .flex_none()
                .h(px(crate::surface_chrome::HEADER_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .px(px(Theme::SPACE_LG))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(self.header_label(parsed))),
                )
                .child(
                    div()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{}", parsed.additions))),
                )
                .child(
                    div()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{}", parsed.deletions))),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!(
                            "{} hunks",
                            logical_hunk_count(&parsed.layout, &parsed.files)
                        ))),
                )
                .child(div().flex_1())
                .when(parsed.truncated, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(px(10.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .when(true, |el| crate::components::changes_warning_badge(el, theme, crate::components::ChangesWarning::Partial))
                            .child(SharedString::from("Partial snapshot")),
                    )
                })
                .into_any_element(),
        )
    }

    pub(super) fn context_details(&self, cx: &App) -> Option<(String, String, String, String)> {
        let (repo, worktree, branch, path) = if let Some(owner) = &self.owner {
            let (repo, worktree) = owner.repo_and_worktree();
            (
                repo,
                worktree,
                owner.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                owner
                    .cwd
                    .clone()
                    .unwrap_or_else(|| "Path unavailable".to_string()),
            )
        } else if let Some(cwd) = &self.pinned_cwd {
            let target = ChangesTarget {
                unconfirmed: None,
                exact_checkout: false,
                range: None,
                chat_id: String::new(),
                device_id: String::new(),
                cwd: Some(cwd.clone()),
                checkout_id: None,
                branch: None,
            };
            let (repo, worktree) = target.repo_and_worktree();
            (repo, worktree, "HEAD".to_string(), cwd.clone())
        } else {
            return None;
        };
        let freshness = self
            .active_diff(cx)
            .map(|diff| {
                format!(
                    "Snapshot age {}",
                    crate::state::format_time_ago(diff.updated_at, chrono::Utc::now())
                )
            })
            .unwrap_or_else(|| "Snapshot pending".to_string());
        Some((repo, worktree, branch, format!("{path}\n{freshness}")))
    }

    /// Bound Chats: one pill per primary/member repository with its branch
    /// or HEAD; picking one retargets the diff (shared with the composer's
    /// branch indicator through `AppState`).
    pub(super) fn render_execution_selector(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return None;
        }
        let chat_id = self.owner_chat_id_owned()?;
        let state = self.state.read(cx);
        let members = state.chat_execution_members(&chat_id);
        if members.is_empty() {
            return None;
        }
        let selected = state
            .selected_execution_member(&chat_id)
            .map(|member| member.repo_id);
        let status = state.chat_execution_state(&chat_id);
        let loading = status.is_some_and(|status| status.loading);
        let error = status.and_then(|status| status.error.clone()).or_else(|| {
            status
                .and_then(|status| status.reply.as_ref())
                .filter(|reply| !reply.ok)
                .and_then(|reply| {
                    crate::work_outline::execution::validation_issues(reply)
                        .into_iter()
                        .next()
                })
                .map(|issue| format!("{} {}", issue.message, issue.remediation))
        });
        Some(
            div()
                .debug_selector(|| "changes-execution-repositories".into())
                .flex_none()
                .px(px(Theme::SPACE_LG))
                .py(px(4.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(4.0))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .children(members.into_iter().enumerate().map(|(ix, member)| {
                    let active = selected.as_deref() == Some(member.repo_id.as_str());
                    let repo_id = member.repo_id.clone();
                    let chat_id = chat_id.clone();
                    let tip: SharedString = match &member.range {
                        Some(range) => format!(
                            "{} \u{2192} {}\nread from {}",
                            range.head_branch, range.base_branch, member.checkout_path
                        )
                        .into(),
                        None => member.checkout_path.clone().into(),
                    };
                    div()
                        .id(("changes-execution-repo", ix))
                        .debug_selector(move || format!("changes-execution-repo-{ix}"))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .when(true, |el| crate::components::changes_repository_chip(el, theme, active))
                        .cursor_pointer()
                        .text_size(px(11.0))
                        .text_color(if active { theme.text } else { theme.text_muted })
                        .hover(|el| el.bg(theme.element_hover))
                        .child(
                            crate::icons::icon(crate::icons::GIT_BRANCH)
                                .size(px(10.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from(member.label()))
                        .when(member.primary, |el| {
                            el.child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(theme.text_faint)
                                    .child("primary"),
                            )
                        })
                        .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tip.clone())).into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.select_chat_execution_member(&chat_id, &repo_id, cx)
                            });
                        }))
                }))
                .when(loading, |el| {
                    el.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child("Reading branches\u{2026}"),
                    )
                })
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .w_full()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(SharedString::from(error)),
                    )
                })
                .into_any_element(),
        )
    }

    pub(super) fn render_context_chrome(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (repo, worktree, branch, tooltip) = self.context_details(cx)?;
        let picker = self.render_checkout_picker(theme, cx);
        let ref_picker = self.render_ref_target_picker(theme, cx);
        let submit_review = self.render_submit_review(theme, cx);
        let rereview = self.render_rereview_controls(theme, cx);
        let freshness = tooltip
            .rsplit_once('\n')
            .map(|(_, freshness)| freshness)
            .unwrap_or("Snapshot pending")
            .to_string();
        Some(
            div()
                .id("changes-context-chrome")
                .flex_none()
                .h(px(30.0))
                .px(px(Theme::SPACE_LG))
                .flex()
                .items_center()
                .gap(px(7.0))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .bg(crate::theme::wash(0.025))
                .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tooltip.clone().into())).into())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(SharedString::from(format!("{repo} / {worktree}"))),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(branch)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(freshness)),
                )
                .children(rereview)
                .children(submit_review)
                .children(ref_picker)
                .children(picker)
                .into_any_element(),
        )
    }
}
