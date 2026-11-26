use super::*;

impl Shell {
    pub(in crate::shell) fn close_sidebar_view_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.sidebar_view_menu.open_mut() {
            menu.hover_intent.reset();
        }
        if self.sidebar_view_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.sidebar_view_menu);
            cx.notify();
        }
    }

    pub(super) fn open_sidebar_view_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_spaces_menu(cx);
        let focus = cx.focus_handle();
        self.sidebar_view_menu.open(SidebarViewMenu {
            submenu: None,
            submenu_active: None,
            submenu_bounds: None,
            submenu_on_left: false,
            hover_intent: popover::HoverIntent::default(),
            active: None,
            focus: focus.clone(),
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn activate_sidebar_view_row(&mut self, row: SidebarViewRow, cx: &mut Context<Self>) {
        if let Some(menu) = self.sidebar_view_menu.open_mut() {
            menu.hover_intent.cancel();
        }
        self.cancel_sidebar_session_transfer(cx);
        self.cancel_pinned_session_drag(cx);
        match row {
            SidebarViewRow::ShowProjectLabel => {
                self.settings.sidebar_show_project_label = !self.settings.sidebar_show_project_label
            }
            SidebarViewRow::Compact => {
                self.settings.sidebar_compact = !self.settings.sidebar_compact
            }
            SidebarViewRow::LastUpdated => self.settings.sidebar_sort = SidebarSort::LastUpdated,
            SidebarViewRow::Created => self.settings.sidebar_sort = SidebarSort::Created,
            SidebarViewRow::ShowBranch => {
                self.settings.sidebar_show_branch = !self.settings.sidebar_show_branch
            }
            SidebarViewRow::ShowPullRequest => {
                self.settings.sidebar_show_pull_request = !self.settings.sidebar_show_pull_request;
                let visible = self.settings.sidebar_show_pull_request;
                self.state.update(cx, |state, cx| {
                    state.set_change_requests_visible(visible, cx)
                });
            }
            SidebarViewRow::ShowHarness => {
                self.settings.sidebar_show_harness = !self.settings.sidebar_show_harness
            }
        }
        self.schedule_save(cx);
        if row.closes_submenu()
            && let Some(menu) = self.sidebar_view_menu.open_mut()
        {
            menu.submenu = None;
            menu.submenu_active = None;
            menu.submenu_bounds = None;
            menu.hover_intent.cancel();
        }
        cx.notify();
    }

    pub(super) fn open_sidebar_view_submenu(&mut self, group: usize, keyboard: bool, cx: &mut Context<Self>) {
        if let Some(menu) = self.sidebar_view_menu.open_mut() {
            menu.hover_intent.reset();
            menu.active = Some(group);
            menu.submenu = (group < SIDEBAR_VIEW_GROUPS.len()).then_some(group);
            menu.submenu_active = keyboard.then_some(0);
            menu.submenu_bounds = None;
            cx.notify();
        }
    }

    pub(super) fn hover_sidebar_view_group(
        &mut self,
        group: usize,
        pointer: gpui::Point<gpui::Pixels>,
        moved: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.sidebar_view_menu.open_mut() else {
            return;
        };
        let action = if moved {
            menu.hover_intent.moved(
                menu.submenu.as_ref(),
                &group,
                pointer,
                menu.submenu_bounds,
                menu.submenu_on_left,
            )
        } else {
            menu.hover_intent.enter(
                menu.submenu.as_ref(),
                &group,
                pointer,
                menu.submenu_bounds,
                menu.submenu_on_left,
            )
        };
        match action {
            popover::HoverAction::None => {}
            popover::HoverAction::Open => {
                self.open_sidebar_view_submenu(group, false, cx);
                if let Some(menu) = self.sidebar_view_menu.open_mut() {
                    menu.hover_intent.record_origin(pointer);
                }
            }
            popover::HoverAction::Defer => {
                let source = menu.submenu;
                menu.hover_intent.defer(cx, move |this, cx| {
                    if this.sidebar_view_menu.open_mut().is_some_and(|menu| {
                        menu.submenu == source && menu.hover_intent.pending() == Some(&group)
                    }) {
                        this.open_sidebar_view_submenu(group, false, cx);
                        if let Some(menu) = this.sidebar_view_menu.open_mut() {
                            menu.hover_intent.record_origin(pointer);
                        }
                    }
                });
            }
        }
    }

    pub(super) fn sidebar_view_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let Some(menu) = self.sidebar_view_menu.open_mut() else {
            return;
        };
        menu.hover_intent.cancel();
        let key = match popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            popover::MenuKey::Up => "up",
            popover::MenuKey::Down => "down",
            _ => event.keystroke.key.as_str(),
        };
        match key {
            "escape" | "left" => {
                if menu.submenu.take().is_some() {
                    menu.submenu_active = None;
                    menu.submenu_bounds = None;
                    cx.notify();
                } else if key == "escape" {
                    self.close_sidebar_view_menu(cx);
                }
            }
            "up" | "down" => {
                let delta = if key == "up" { -1 } else { 1 };
                if let Some(group) = menu.submenu {
                    menu.submenu_active = popover::menu_step(
                        menu.submenu_active,
                        SIDEBAR_VIEW_GROUPS[group].1.len(),
                        delta,
                    );
                } else {
                    menu.active =
                        popover::menu_step(menu.active, SIDEBAR_VIEW_GROUPS.len() + 1, delta);
                }
                cx.notify();
            }
            "right" | "enter" | "space" => {
                if let Some(group) = menu.submenu {
                    if key == "enter"
                        && let Some(choice) = menu.submenu_active
                    {
                        let row = SIDEBAR_VIEW_ROWS[SIDEBAR_VIEW_GROUPS[group].1.start + choice];
                        self.activate_sidebar_view_row(row, cx);
                    }
                } else {
                    let group = menu.active.unwrap_or(0);
                    if group == SIDEBAR_VIEW_GROUPS.len() {
                        if key != "right" && !event.is_held {
                            self.activate_sidebar_view_row(SidebarViewRow::Compact, cx);
                        }
                    } else if key != "space" {
                        self.open_sidebar_view_submenu(group, true, cx);
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_sidebar_view_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let Some(menu_state) = self.sidebar_view_menu.get() else {
            return div().into_any_element();
        };
        let active = menu_state.active;
        let submenu = menu_state.submenu;
        let submenu_active = menu_state.submenu_active;
        let on_left = menu_state.submenu_on_left;
        let focus = menu_state.focus.clone();
        let sort = self.settings.sidebar_sort;
        let show_harness = self.settings.sidebar_show_harness;
        let show_branch = self.settings.sidebar_show_branch;
        let show_pr = self.settings.sidebar_show_pull_request;

        let labels = [
            "Last updated",
            "Created",
            "Branch",
            "Pull request",
            "Harness",
            "Location",
        ];
        let icons = [
            icons::CLOCK_CIRCLE,
            icons::CALENDAR,
            icons::GIT_BRANCH,
            icons::PULL_REQUEST,
            icons::BOT,
            icons::FOLDER,
        ];
        let selected = [
            sort == SidebarSort::LastUpdated,
            sort == SidebarSort::Created,
            show_branch,
            show_pr,
            show_harness,
            self.settings.sidebar_show_project_label,
        ];
        let mut groups: Vec<AnyElement> = Vec::new();
        for (group, (label, range)) in SIDEBAR_VIEW_GROUPS.iter().enumerate() {
            let open = submenu == Some(group);
            let entity = cx.entity().downgrade();
            let exit_entity = cx.entity().downgrade();
            let mut trigger = popover::menu_row_nav(
                theme,
                open,
                active == Some(group),
                format!("sidebar-view-group-{group}"),
            )
            .id(("sidebar-view-group", group))
            .relative()
            .h(px(30.0))
            .py(px(0.0))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Match model settings: hover opens; clicking dismisses, including
                // a sibling crossed during hover grace, without delayed reopening.
                if let Some(menu) = this.sidebar_view_menu.open_mut() {
                    menu.active = Some(group);
                    menu.submenu = None;
                    menu.submenu_active = None;
                    menu.submenu_bounds = None;
                    menu.hover_intent.reset();
                }
                cx.notify();
                cx.stop_propagation();
            }))
            .child(div().flex_1().child(*label))
            .when(group == 0, |el| {
                el.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(if selected[0] { labels[0] } else { labels[1] }),
                )
            })
            .child(
                icon(icons::ALT_ARROW_RIGHT)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let left = bounds.right() + px(244.0) > window.viewport_size().width;
                        let _ = entity.update(cx, |this, cx| {
                            if let Some(menu) = this.sidebar_view_menu.open_mut()
                                && menu.submenu_on_left != left
                            {
                                menu.submenu_on_left = left;
                                cx.notify();
                            }
                        });
                    },
                    move |trigger, _, window, _| {
                        if !open {
                            return;
                        }
                        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                            if phase != gpui::DispatchPhase::Bubble {
                                return;
                            }
                            let _ = exit_entity.update(cx, |this, cx| {
                                let Some(menu) = this.sidebar_view_menu.open_mut() else {
                                    return;
                                };
                                if menu.submenu != Some(group)
                                    || menu.hover_intent.contains_pointer(
                                        trigger,
                                        menu.submenu_bounds,
                                        event.position,
                                        menu.submenu_on_left,
                                    )
                                {
                                    return;
                                }
                                menu.hover_intent.reset();
                                menu.submenu = None;
                                menu.submenu_bounds = None;
                                menu.submenu_active = None;
                                menu.active = None;
                                cx.notify();
                            });
                        });
                    },
                )
                .absolute()
                .inset_0(),
            );
            if open {
                let mut choices = Vec::new();
                for (choice, ix) in range.clone().enumerate() {
                    let row = SIDEBAR_VIEW_ROWS[ix];
                    choices.push(
                        popover::menu_row_nav(
                            theme,
                            selected[ix],
                            submenu_active == Some(choice),
                            format!("sidebar-view-row-{ix}"),
                        )
                        .id(("sidebar-view-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_sidebar_view_row(row, cx);
                            cx.stop_propagation();
                        }))
                        .child(
                            icon(icons[ix])
                                .size(px(15.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(div().flex_1().child(labels[ix]))
                        .child(div().w(px(14.0)).flex_none().when(selected[ix], |el| {
                            el.child(
                                icon(icons::CHECK)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            )
                        })),
                    );
                }
                let entity = cx.entity().downgrade();
                let child = popover::popover_card(theme)
                    .w(px(232.0))
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(popover::menu_heading(theme, label))
                    .children(choices)
                    .child(
                        gpui::canvas(
                            move |bounds, _, cx| {
                                let _ = entity.update(cx, |this, _| {
                                    if let Some(menu) = this.sidebar_view_menu.open_mut() {
                                        menu.submenu_bounds = Some(bounds);
                                    }
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    );
                trigger = trigger.child(popover::nested_menu(
                    format!("sidebar-view-child-{group}"),
                    child.into_any_element(),
                    on_left,
                ));
            }
            groups.push(
                div()
                    .id(("sidebar-view-hover", group))
                    .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                        if *hovered {
                            this.hover_sidebar_view_group(
                                group,
                                window.mouse_position(),
                                false,
                                cx,
                            );
                        } else if let Some(menu) = this.sidebar_view_menu.open_mut() {
                            menu.hover_intent.leave(&group);
                        }
                    }))
                    .on_mouse_move(
                        cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                            this.hover_sidebar_view_group(group, event.position, true, cx);
                        }),
                    )
                    .child(trigger)
                    .into_any_element(),
            );
        }
        groups.push(popover::menu_separator(theme).into_any_element());
        groups.push(
            div()
                .id("sidebar-view-compact-hover")
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if *hovered {
                        this.hover_sidebar_view_group(
                            SIDEBAR_VIEW_GROUPS.len(),
                            window.mouse_position(),
                            false,
                            cx,
                        );
                    } else if let Some(menu) = this.sidebar_view_menu.open_mut() {
                        menu.hover_intent.leave(&SIDEBAR_VIEW_GROUPS.len());
                    }
                }))
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                    this.hover_sidebar_view_group(
                        SIDEBAR_VIEW_GROUPS.len(),
                        event.position,
                        true,
                        cx,
                    );
                }))
                .child(
                    popover::menu_row_nav(
                        theme,
                        false,
                        active == Some(SIDEBAR_VIEW_GROUPS.len()),
                        "sidebar-view-compact",
                    )
                    .id("sidebar-view-compact")
                    .h(px(30.0))
                    .py(px(0.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_sidebar_view_submenu(SIDEBAR_VIEW_GROUPS.len(), false, cx);
                        this.activate_sidebar_view_row(SidebarViewRow::Compact, cx);
                        cx.stop_propagation();
                    }))
                    .child(div().flex_1().child("Compact"))
                    .child(crate::settings::widgets::toggle_switch(
                        theme,
                        self.settings.sidebar_compact,
                    )),
                )
                .into_any_element(),
        );
        popover::popover_card(theme)
            .w(px(self.settings.sidebar_width - 2.0 * Theme::SPACE_SM))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.sidebar_view_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if !this
                    .sidebar_view_menu
                    .get()
                    .and_then(|m| m.submenu_bounds)
                    .is_some_and(|bounds| bounds.contains(&event.position))
                {
                    this.close_sidebar_view_menu(cx);
                }
            }))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(groups)
            .into_any_element()
    }

}
