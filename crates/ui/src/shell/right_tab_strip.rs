use super::*;

/// The dragged surface-tab payload (strip reorder).
pub(super) struct RightTabDrag {
    pub(super) panel_key: String,
    pub(super) from: usize,
    pub(super) title: SharedString,
    pub(super) workspace_path: Option<WorkspacePathDrag>,
}

/// Live drag-over state for the surface-tab strip — the terminal drawer's
/// [`crate::terminal::panel`] DragState, ported: `epoch` keys the 150ms
/// slide-animation restarts as the hovered slot changes.
pub(super) struct RightTabDragState {
    pub(super) from: usize,
    pub(super) over: usize,
    pub(super) epoch: usize,
    pub(super) prev_over: usize,
}

/// Ghost chip following the pointer while a surface tab drags.
pub(super) struct SurfaceTabGhost {
    pub(super) title: SharedString,
}

impl Render for SurfaceTabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .h(px(24.0))
            .w(px(112.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

pub(super) struct SurfaceTabTooltip {
    pub(super) text: SharedString,
}

impl Render for SurfaceTabTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .max_w(px(380.0))
            .px(px(9.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(theme))
            .text_size(px(10.5))
            .text_color(theme.text_muted)
            .child(self.text.clone());
        crate::frost::frosted(6.0, crate::frost::MENU_BLUR, card)
    }
}
impl Shell {
    pub(super) fn close_right_tab_menu(&mut self, cx: &mut Context<Self>) {
        if self.right_tab_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.right_tab_menu);
        }
        cx.notify();
    }

    pub(super) fn close_right_plus(&mut self, cx: &mut Context<Self>) {
        if self.right_plus.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.right_plus);
        }
        cx.notify();
    }

    /// The titlebar strip over the right pane: one chip per surface tab
    /// (icon · title · ✕) plus the `+` menu — the t3code RightPanelTabs bar,
    /// living in the top row; the diff options moved into the pane below.
    pub(crate) fn render_right_tab_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        /// Fixed chip slot — the terminal drawer's drag mechanics (drop-index
        /// quantisation + slide offsets) assume uniform widths.
        const CHIP_W: f32 = 112.0;
        const CHIP_SLOT: f32 = CHIP_W + 4.0; // + the strip's own gap

        let theme = Theme::of(cx).clone();
        // Heal drag state if the pointer was released outside the strip.
        if self.right_tab_drag.is_some() && !cx.has_active_drag() {
            self.right_tab_drag = None;
        }
        let rows = self.right_surface_rows(cx);
        let count = rows.len();
        let active = self.resolved_right_active(cx);
        let drag = self
            .right_tab_drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));

        // Fade flags from the LAST frame's scroll state (invisible lag).
        // The EdgeFade scope below fades per-pixel on x for glyphs AND
        // quads/images (fork 5d1f83d) — washes dissolve across the band.
        const FADE_WIDTH: f32 = 36.0;
        let scrolled = -f32::from(self.right_tab_scroll.offset().x);
        let max_scroll = f32::from(self.right_tab_scroll.max_offset().x);
        let fade_left = scrolled > 1.0;
        let fade_right = scrolled < max_scroll - 1.0;
        // The old session-tab strip's proven scroll shape: the flex row IS
        // the scroller (id + overflow_x_scroll + track_scroll), wrapped in a
        // relative min_w_0 region below; drop math runs in CONTENT
        // coordinates (viewport-relative x plus the scrolled-off width).
        let scroll_for_drag = self.right_tab_scroll.clone();
        let mut strip = div()
            .id("right-surface-strip")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.right_tab_scroll)
            // Windows caption hit-testing includes the scroll-only hitboxes
            // behind each chip. Stop at the scroller so the titlebar cannot
            // claim tab clicks, while wheel events still reach this scroller.
            .when(cfg!(target_os = "windows"), |strip| strip.occlude());
        // Drag handlers sit on the outer row (built below) so a release in the
        // free space around the trailing `+` still drops into the last slot;
        // its left edge is the scroller's, so `rel_x` is unchanged.
        let drag_move = cx.listener(
            move |this, event: &gpui::DragMoveEvent<RightTabDrag>, _, cx| {
                let payload = event.drag(cx);
                if payload.panel_key != this.panel_key(cx) {
                    return;
                }
                let from = payload.from;
                let rel_x = f32::from(event.event.position.x)
                    - f32::from(event.bounds.left())
                    - f32::from(scroll_for_drag.offset().x);
                let over = crate::terminal::panel::drop_index(rel_x, CHIP_SLOT, count);
                this.update_right_tab_drag_over(from, over, cx);
            },
        );
        let drop = cx.listener(move |this, payload: &RightTabDrag, _, cx| {
            if payload.panel_key != this.panel_key(cx) {
                this.right_tab_drag = None;
                cx.notify();
                return;
            }
            let to = this
                .right_tab_drag
                .as_ref()
                .map(|d| d.over)
                .unwrap_or(payload.from);
            this.right_tab_drag = None;
            this.reorder_right_tabs(payload.from, to, cx);
        });
        for (ix, (surface, title, dirty, detail)) in rows.into_iter().enumerate() {
            let is_active = surface == active;
            let file_identity_path = detail.as_ref().cloned().unwrap_or_else(|| title.clone());
            let icon_path = match surface {
                RightSurface::File(_) => icons::DOCUMENT,
                RightSurface::Diff(id) => self
                    .diffs
                    .get(&id)
                    .map(|changes| {
                        if changes.read(cx).is_history() {
                            icons::GIT_BRANCH
                        } else {
                            icons::LIST
                        }
                    })
                    .unwrap_or(icons::LIST),
                RightSurface::Review => icons::DOCUMENT,
                RightSurface::Subagent(_) => icons::BOT,
                RightSurface::SideChat(_) => icons::CHAT_ROUND_LINE,
                RightSurface::Artifact(id) => self
                    .artifact_tab(id)
                    .and_then(|tab| tab.read(cx).artifact(cx))
                    .map(|artifact| crate::artifacts::model::kind_icon(artifact.kind))
                    .unwrap_or(icons::DOCUMENT),
                RightSurface::Terminal(_) => icons::TERMINAL,
                RightSurface::Browser(_) => icons::GLOBE,
                RightSurface::Picker | RightSurface::Item | RightSurface::Team => icons::PLUS,
            };
            // A live subagent tab swaps its icon for the mini working
            // spinner (the history fetch button's in-flight recipe) — the
            // doc's streaming tail entry IS the run's liveness, so the swap
            // settles by itself when the subagent finishes.
            let browser_favicon = match surface {
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .and_then(|b| b.read(cx).favicon.clone()),
                _ => None,
            };
            let subagent_running = match surface {
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .is_some_and(|b| b.read(cx).page.loading),
                RightSurface::SideChat(id) => self.side_chat_running(id, cx),
                RightSurface::Subagent(id) => self.subagent_tabs.get(&id).is_some_and(|tab| {
                    self.state
                        .read(cx)
                        .sub_transcript(&tab.doc_id)
                        .last()
                        .is_some_and(|e| e.status == Some(mast_doc::MessageStatus::Streaming))
                }),
                _ => false,
            };
            // t3 tab hover: the surface icon swaps IN PLACE for the close ✕
            // (same slot, no width jump) — the ✕ only shows while the tab is
            // hovered (user request).
            let group: SharedString = format!("right-surface-tab-{ix}").into();
            let ghost_title = title.clone();
            let workspace_path = self.workspace_path_for_surface(surface, cx);
            let accessible_name = detail.as_ref().unwrap_or(&title);
            let accessible_label = if dirty {
                format!("{accessible_name}, unsaved changes")
            } else {
                accessible_name.to_string()
            };
            let chip = crate::components::shell_parts::right_surface_tab(
                ix,
                group.clone(),
                CHIP_W,
                accessible_label,
                is_active,
                &theme,
            )
            .when_some(detail, |chip, detail| {
                chip.tooltip(move |_, cx| {
                    cx.new(|_| SurfaceTabTooltip {
                        text: detail.clone(),
                    })
                    .into()
                })
                .tooltip_show_delay(Duration::from_millis(350))
            })
            // The old session-tab strip's solved carve-out: NOT
            // `.occlude()` — a BlockMouse hitbox ends the hit test,
            // so the scroll container behind the tabs never saw
            // wheel events and an overflowing strip could not be
            // scrolled (tabs tile the whole region). ExceptScroll
            // keeps the titlebar drag-region carve-out and lets the
            // strip scroll.
            .block_mouse_except_scroll()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default()
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.set_right_active(surface, cx);
                this.focus_right_file_editor(surface, window, cx);
            }))
            // Right-click: Open in new window / Close.
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if this.can_pop_out(surface) || surface != RightSurface::Review {
                        this.right_tab_menu.open(surface);
                        this.close_right_plus(cx);
                    }
                    cx.notify();
                }),
            )
            // Middle-click closes, like every tab strip.
            .on_mouse_down(
                gpui::MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    this.close_right_surface(surface, window, cx);
                }),
            )
            .when(
                crate::click_activation_drag_enabled() && surface != RightSurface::Review,
                |el| {
                    el.on_drag(
                        RightTabDrag {
                            panel_key: self.panel_key(cx),
                            from: ix,
                            title: ghost_title,
                            workspace_path,
                        },
                        |payload, _point, _, cx| {
                            let title = payload.title.clone();
                            cx.stop_propagation();
                            cx.new(|_| SurfaceTabGhost { title })
                        },
                    )
                },
            )
            .child(
                // Leading slot: icon normally, ✕ on tab hover — two
                // stacked layers opacity-swapped by the group hover.
                crate::components::shell_parts::surface_close_slot(&theme, ix)
                    // The tab owns a drag payload. Claim the close press
                    // before it reaches that parent or GPUI starts a tab
                    // drag instead of delivering the close click.
                    .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .when(surface != RightSurface::Review, |el| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_right_surface(surface, window, cx);
                        }))
                    })
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(surface != RightSurface::Review, |s| {
                                s.group_hover(group.clone(), |s| s.opacity(0.0))
                            })
                            .child(if subagent_running {
                                loaders::mini_glyph_spinner(
                                    format!("subagent-tab-{ix}"),
                                    2.0,
                                    theme.glyph,
                                    cx.entity_id(),
                                    cx,
                                )
                                .into_any_element()
                            } else if let Some(favicon) = browser_favicon {
                                gpui::img(favicon).size(px(12.0)).into_any_element()
                            } else if matches!(surface, RightSurface::File(_)) {
                                crate::file_icons::icon(
                                    crate::file_icons::FileIconIdentity::file(
                                        file_identity_path.as_ref(),
                                    ),
                                    theme.appearance,
                                )
                                .size(px(14.0))
                                .when(!is_active, |icon| icon.opacity(0.78))
                                .into_any_element()
                            } else {
                                icon(icon_path)
                                    .size(px(12.0))
                                    .text_color(if is_active {
                                        theme.text_muted
                                    } else {
                                        theme.text_muted.opacity(0.7)
                                    })
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(0.0)
                            .when(surface != RightSurface::Review, |s| {
                                s.group_hover(group.clone(), |s| s.opacity(1.0))
                            })
                            .child(
                                icon(icons::CLOSE)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            ),
                    ),
            )
            .child(crate::components::shell_parts::surface_tab_title(
                &theme, title, is_active,
            ))
            .when(dirty, |chip| {
                chip.child(crate::components::shell_parts::surface_dirty_indicator(
                    &theme,
                ))
            })
            .when(self.right_tab_menu.get() == Some(&surface), |chip| {
                let closing = self.right_tab_menu.closing_since();
                let can_pop = self.can_pop_out(surface);
                let menu = popover::popover_card(&theme)
                    .w(px(200.0))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_right_tab_menu(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .when(can_pop, |menu| {
                                menu.child(
                                    popover::menu_row(&theme, false, "right-tab-popout")
                                        .id("right-tab-popout-row")
                                        .debug_selector(|| "right-tab-popout-row".into())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.close_right_tab_menu(cx);
                                            this.pop_out_surface(surface, cx);
                                        }))
                                        .child(
                                            icon(icons::ARROW_UP_RIGHT)
                                                .size(px(13.0))
                                                .text_color(theme.text_muted),
                                        )
                                        .child(SharedString::from("Open in new window")),
                                )
                            })
                            .when(surface != RightSurface::Review, |menu| {
                                menu.child(
                                    popover::menu_row(&theme, false, "right-tab-close")
                                        .id("right-tab-close-row")
                                        .debug_selector(|| "right-tab-close-row".into())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.close_right_tab_menu(cx);
                                            this.close_right_surface(surface, window, cx);
                                        }))
                                        .child(
                                            icon(icons::CLOSE)
                                                .size(px(13.0))
                                                .text_color(theme.text_muted),
                                        )
                                        .child(SharedString::from("Close tab")),
                                )
                            }),
                    )
                    .into_any_element();
                chip.relative().child(popover::anchored_menu_below_gap(
                    "right-tab-menu",
                    menu,
                    closing,
                    10.0,
                ))
            });
            // Sliding transform while a sibling drags over (the terminal
            // drawer's exact recipe): animate 150ms between committed
            // offsets; the dragged tab leaves an invisible spacer — the
            // ghost carries it.
            let wrapped: AnyElement = match drag {
                Some((from, over, epoch, prev_over)) if ix != from => {
                    let target = crate::terminal::panel::slide_offset(ix, from, over) * CHIP_SLOT;
                    let start =
                        crate::terminal::panel::slide_offset(ix, from, prev_over) * CHIP_SLOT;
                    div()
                        .relative()
                        .child(chip.with_animation(
                            ("right-tab-slide", (ix as u64) | ((epoch as u64) << 32)),
                            TAB_SLIDE.animation(),
                            move |el, t| el.left(px(motion::lerp(start, target, t))),
                        ))
                        .into_any_element()
                }
                Some((from, ..)) if ix == from => div()
                    .w(px(CHIP_W))
                    .h(px(24.0))
                    .flex_none()
                    .into_any_element(),
                _ => chip.into_any_element(),
            };
            strip = strip.child(wrapped);
        }
        // The `+` — a small menu offering the available surfaces (t3 "Add panel
        // surface"); mirrors the picker cards.
        let plus_open = self.right_plus.get().is_some();
        let mut plus = crate::components::shell_parts::surface_add_button(&theme)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.right_plus.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.right_plus.take_press_was_open() {
                    this.close_right_plus(cx);
                } else {
                    this.right_plus.open(());
                    cx.notify();
                }
            }))
            .child(
                icon(icons::PLUS)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            );
        if plus_open {
            let closing = self.right_plus.closing_since();
            let menu = popover::popover_card(&theme)
                .w(px(168.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_right_plus(cx)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            popover::menu_row(&theme, false, "right-plus-browser")
                                .id("right-plus-browser-row")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_browser_surface(None, window, cx);
                                    this.close_right_plus(cx);
                                }))
                                .child(
                                    icon(icons::GLOBE)
                                        .size(px(13.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Browser")),
                        )
                        .child(
                            popover::menu_row(&theme, false, "right-plus-terminal")
                                .id("right-plus-terminal-row")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_terminal_surface(cx);
                                    this.close_right_plus(cx);
                                }))
                                .child(
                                    icon(icons::TERMINAL)
                                        .size(px(13.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Terminal")),
                        )
                        .when(self.space_git_detected(cx), |menu| {
                            menu.child(
                                popover::menu_row(&theme, false, "right-plus-diff")
                                    .id("right-plus-diff-row")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_diff_surface(cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::LIST)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(SharedString::from("Diffs")),
                            )
                            .child(
                                popover::menu_row(&theme, false, "right-plus-history")
                                    .id("right-plus-history-row")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_history_surface(cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(SharedString::from("History")),
                            )
                        }),
                )
                .into_any_element();
            plus = plus.relative().child(popover::anchored_menu_below_gap(
                "right-plus-menu",
                menu,
                closing,
                10.0,
            ));
        }
        // The picker body already offers every surface, so the `+` hides only
        // while the picker shows. The sticky Item/Team chips are not in
        // `rows`, so a pane on one of them with no other tabs still needs it.
        let show_plus = active != RightSurface::Picker;
        // Edge fades on whichever side hides tabs (flags computed above).
        // Glass: per-glyph EdgeFade scope over the chips' own opacity ramps;
        // opaque: painted gradients in the shell surface tone.
        let glass = theme.is_glass();
        let bar_bg = theme.surface;
        let region = div()
            .relative()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .child(strip)
            .when(fade_left && !glass, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(FADE_WIDTH))
                        .bg(gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(bar_bg, 0.0),
                            gpui::linear_color_stop(bar_bg.opacity(0.0), 1.0),
                        )),
                )
            })
            .when(fade_right && !glass, |el| {
                el.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(FADE_WIDTH))
                        .bg(gpui::linear_gradient(
                            270.0,
                            gpui::linear_color_stop(bar_bg, 0.0),
                            gpui::linear_color_stop(bar_bg.opacity(0.0), 1.0),
                        )),
                )
            });
        let region = if glass {
            crate::edge_fade::edge_faded(FADE_WIDTH, false, false, region)
                .fade_left(fade_left)
                .fade_right(fade_right)
                .into_any_element()
        } else {
            region.into_any_element()
        };
        // The `+` sits after the scroller, not inside it: it follows the last
        // tab, and once the tabs overflow the scroller shrinks so the `+`
        // stays pinned in view.
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .on_drag_move::<RightTabDrag>(drag_move)
            .on_drop::<RightTabDrag>(drop)
            .child(div().flex().min_w_0().h_full().child(region))
            .when(show_plus, |row| row.child(plus))
            .into_any_element()
    }
    /// Toggle the changes-panel takeover (the header's expand button, t3code
    /// parity): the panel grows to fill everything right of the sidebar,
    /// hiding the conversation column; toggling back restores the saved
    /// width. Rides the same width tween as open/close so the jump glides.
    pub(super) fn toggle_right_pane_expand(&mut self, cx: &mut Context<Self>) {
        let from = self.right_target(cx);
        self.right_edge_bounce = None;
        self.right_resize_edge = None;
        self.finish_pane_resize(PaneResizeKind::Right);
        let sidebar_now = self.sidebar_now();
        let from_main = conversation_width(
            self.viewport_width - self.files_reserved_width(cx),
            sidebar_now,
            from,
        );
        self.right_pane_expanded = !self.right_pane_expanded;
        let to = self.right_target(cx);
        let right_transition = WidthTween::new(from, to);
        self.right_tween = Some(right_transition);
        self.right_takeover_content_tween = Some(right_transition);
        self.main_takeover_tween = Some(WidthTween::new(
            from_main,
            conversation_width(
                self.viewport_width - self.files_reserved_width(cx),
                sidebar_now,
                to,
            ),
        ));
        cx.notify();
    }
}
