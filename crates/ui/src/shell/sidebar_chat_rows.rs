use super::*;

/// Delivery state takes precedence over a chat's activity indicator.
pub(super) fn sidebar_delivery_status(
    queued: bool,
    undelivered: bool,
) -> Option<(crate::status_style::Status, &'static str)> {
    use crate::status_style::Status;

    if undelivered {
        Some((Status::Errored, "Failed"))
    } else if queued {
        Some((Status::Waiting, "Queued"))
    } else {
        None
    }
}

/// Sidebar render identity lets transcript/caret frames reuse its GPUI scene.
/// State and event handlers stay on Shell. Explicit Shell notifications still
/// invalidate the sidebar, including selection, menus, theme and navigation.
/// Test-only render counters for the sidebar perf budgets (thread-local: gpui
/// tests run on one thread).
#[cfg(test)]
pub(crate) mod perf_counts {
    use std::cell::Cell;
    thread_local! {
        pub(crate) static SHELL_RENDERS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static CHAT_ROW_BUILDS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static CHAT_ROW_SCANS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static SESSION_SCANS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static SIDEBAR_SORTS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static FINGERPRINT_CHATS: Cell<u64> = const { Cell::new(0) };
        pub(crate) static CHAT_ROW_BY_ID: std::cell::RefCell<std::collections::HashMap<String, u64>> = std::cell::RefCell::new(Default::default());
    }
}

/// Everything `render_chat_row` needs that the sidebar computes per row.
#[derive(Clone, PartialEq)]
pub(super) struct ChatRowArgs {
    pub(super) row_scope: Option<String>,
    pub(super) chat: mast_proto::Chat,
    pub(super) id: String,
    pub(super) title: SharedString,
    pub(super) time_ago: SharedString,
    pub(super) space_name: SharedString,
    pub(super) branch: Option<SharedString>,
    pub(super) change_request: Option<mast_proto::ChangeRequestSummary>,
    pub(super) harness: Option<mast_proto::HarnessId>,
    pub(super) status: mast_proto::ChatIndicator,
    pub(super) selected: bool,
    pub(super) drag: Option<SidebarSessionDrag>,
    pub(super) jump_label: Option<SharedString>,
}

/// One sidebar chat row as its own cached view. The pane rebuilds the args each
/// pass and only notifies this view when they changed, so a working row's
/// spinner tick re-renders just that row instead of every row in the list.
pub(super) struct ChatRowView {
    shell: gpui::WeakEntity<Shell>,
    args: ChatRowArgs,
    hover_animation_started: Option<std::time::Instant>,
    _observation: Subscription,
}

impl Render for ChatRowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let args = self.args.clone();
        if self.hover_animation_started.is_some_and(|started| {
            !motion::reduced_motion(cx)
                && started.elapsed() < motion::HOVER_FADE.total().mul_f32(motion::speed_scale())
        }) {
            window.request_animation_frame();
        } else {
            self.hover_animation_started = None;
        }
        // A view root is laid out against its bounds but sizes to its content, so
        // restore the full-width stretch the row had as a direct flex child.
        let row = shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            shell.render_chat_row_with_chat(
                args.id,
                args.row_scope.as_deref(),
                args.title,
                args.time_ago,
                args.space_name,
                args.branch,
                args.change_request,
                args.harness,
                args.status,
                args.selected,
                args.chat.archived,
                false,
                args.drag,
                args.jump_label,
                None,
                Some(&args.chat),
                &theme,
                cx,
            )
        });
        div().w_full().child(row).into_any_element()
    }
}

impl Shell {
    /// The cached row view for one live sidebar row. `height` is the slot height.
    pub(super) fn cached_chat_row(
        &mut self,
        args: ChatRowArgs,
        height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = args.row_scope.as_ref().map_or_else(|| args.id.clone(), |scope| format!("{scope}-chat-{}", args.id));
        self.sidebar_row_views_seen.insert(key.clone());
        let view = match self.sidebar_row_views.get(&key) {
            Some(view) => {
                let view = view.clone();
                view.update(cx, |row, cx| {
                    if row.args != args {
                        row.args = args;
                        cx.notify();
                    }
                });
                view
            }
            None => {
                let shell = cx.entity();
                let id = key;
                let view = cx.new(|cx| ChatRowView {
                    shell: shell.downgrade(),
                    args,
                    hover_animation_started: None,
                    _observation: cx.observe(&shell, |_, _, cx| cx.notify()),
                });
                self.sidebar_row_views.insert(id, view.clone());
                view
            }
        };
        // Width stays auto so the row stretches like the uncached row did.
        view.cached(gpui::StyleRefinement::default().h(px(height.max(0.0))))
            .into_any_element()
    }

    pub(super) fn update_chat_row_hover(
        &mut self,
        row_id: &str,
        fade_key: &str,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = row_id.strip_prefix("chat-").unwrap_or(row_id);
        let row = self.sidebar_row_views.get(key)
            .cloned();
        if let Some(row) = row {
            motion::set_hover(fade_key, hovered, motion::reduced_motion(cx));
            if hovered {
                self.chat_status_hover = Some(row_id.to_string());
            } else if self.chat_status_hover.as_deref() == Some(row_id) {
                self.chat_status_hover = None;
            }
            row.update(cx, |row, cx| {
                row.hover_animation_started = Some(std::time::Instant::now());
                cx.notify();
            });
        } else {
            motion::hover_listener(fade_key.to_owned())(&hovered, window, cx);
            if hovered {
                self.chat_status_hover = Some(row_id.to_string());
            } else if self.chat_status_hover.as_deref() == Some(row_id) {
                self.chat_status_hover = None;
            } else {
                return;
            }
            cx.notify();
        }
    }

    /// Drop row views for chats that left the list; called once per sidebar pass.
    pub(super) fn prune_sidebar_row_views(&mut self) {
        let seen = std::mem::take(&mut self.sidebar_row_views_seen);
        self.sidebar_row_views.retain(|id, _| seen.contains(id));
    }
}

pub(super) struct SidebarPane {
    #[cfg(test)]
    pub(super) renders: usize,
    pub(super) shell: gpui::WeakEntity<Shell>,
    pub(super) _observation: Subscription,
    pub(super) _overview_observation: Option<Subscription>,
}

impl Render for SidebarPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        #[cfg(feature = "startup-bench")]
        let _render_span = crate::startup_measurement::MeasurementSpan::new("sidebar");
        crate::transcript::record_view_frame("sidebar");
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let inner = shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            match shell.route {
                Route::Settings(section) => shell.render_settings_nav(section, &theme, cx),
                // Overview keeps the normal chat sidebar.
                Route::Chat
                | Route::Overview
                | Route::ScheduledTasks
                | Route::PrReviewChats
                | Route::Review => shell.render_chat_sidebar(&theme, cx),
            }
        });
        div().size_full().child(inner).into_any_element()
    }
}

impl Shell {
    pub(super) fn render_chat_row(
        &self,
        id: String,
        row_scope: Option<&str>,
        title: SharedString,
        time_ago: SharedString,
        space_name: SharedString,
        branch: Option<SharedString>,
        change_request: Option<mast_proto::ChangeRequestSummary>,
        harness: Option<mast_proto::HarnessId>,
        status: mast_proto::ChatIndicator,
        selected: bool,
        archived: bool,
        preview: bool,
        drag: Option<SidebarSessionDrag>,
        jump_label: Option<SharedString>,
        search_query: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .cloned();
        self.render_chat_row_with_chat(
            id,
            row_scope,
            title,
            time_ago,
            space_name,
            branch,
            change_request,
            harness,
            status,
            selected,
            archived,
            preview,
            drag,
            jump_label,
            search_query,
            chat.as_ref(),
            theme,
            cx,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_chat_row_with_chat(
        &self,
        id: String,
        row_scope: Option<&str>,
        title: SharedString,
        time_ago: SharedString,
        space_name: SharedString,
        branch: Option<SharedString>,
        change_request: Option<mast_proto::ChangeRequestSummary>,
        harness: Option<mast_proto::HarnessId>,
        status: mast_proto::ChatIndicator,
        selected: bool,
        archived: bool,
        preview: bool,
        drag: Option<SidebarSessionDrag>,
        // This row's jump combo while the hint overlay is up. It takes the
        // corner outright — above hover and above the status word — so all
        // nine chips appear together instead of leaving a hole on whichever
        // row is busy or under the pointer.
        jump_label: Option<SharedString>,
        search_query: Option<&str>,
        chat: Option<&mast_proto::Chat>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(test)]
        {
            perf_counts::CHAT_ROW_BUILDS.with(|c| c.set(c.get() + 1));
            perf_counts::CHAT_ROW_BY_ID
                .with(|m| *m.borrow_mut().entry(id.clone()).or_default() += 1);
        }
        // Activity, not position (t3code Sidebar): status is a small colored
        // word + glyph in the row's top-right corner — Working animates the
        // composer-strip spinner, Done wears a check; Idle rows show the
        // relative time instead. Hovering the ROW swaps the corner for the
        // ARCHIVE button. Compact rows keep status first and elapsed time last;
        // their archive control occupies the remote-icon slot on hover.
        // A chat can appear on both surfaces at once. Namespace every hover
        // key and child id so the palette never animates the sidebar copy.
        let row_id = if search_query.is_some() {
            format!("palette-chat-{id}")
        } else if let Some(scope) = row_scope {
            format!("{scope}-chat-{id}")
        } else {
            format!("chat-{id}")
        };
        // Send-truth overrides: a send unadopted past the grace window is
        // FAILED (explicit, with the transcript's retry affordance); a send
        // whose delivery path is degraded is QUEUED, not Working — the
        // pending pill tells the truth instead of faking a spinner.
        let (queued, undelivered) = {
            let now = Utc::now();
            let state = self.state.read(cx);
            (
                state.send_queued(&id, now),
                state.send_undelivered(&id, now),
            )
        };
        let status_color = if undelivered {
            theme.danger
        } else if queued {
            theme.warning
        } else {
            spaces::status_dot_color(status, theme)
        };
        let (resolved_status, status_label) = match sidebar_delivery_status(queued, undelivered) {
            Some((status, label)) => (status, Some(label)),
            None => (
                status.into(),
                match status {
                    mast_proto::ChatIndicator::Working => Some("Working"),
                    mast_proto::ChatIndicator::AwaitingInput => {
                        Some(crate::components::shell_parts::awaiting_input_label(theme))
                    }
                    mast_proto::ChatIndicator::Errored => Some("Failed"),
                    mast_proto::ChatIndicator::Completed => Some("Done"),
                    mast_proto::ChatIndicator::Idle => None,
                },
            ),
        };
        let identity = {
            let config = chat.and_then(|chat| chat.config.as_ref());
            theme.agent_identity_color(
                config.map(|config| config.harness).or(harness),
                config
                    .and_then(|config| config.model.as_deref())
                    .or_else(|| chat.and_then(|chat| chat.actual_model.as_deref())),
            )
        };
        let horizon_hovered =
            !preview && self.chat_status_hover.as_deref() == Some(row_id.as_str());
        let archive_id = id.clone();
        if let Some(row) = crate::components::shell_parts::horizon_sidebar_chat_row(
            theme,
            row_id.clone(),
            &id,
            identity,
            title.clone(),
            resolved_status,
            status_label.unwrap_or("Idle"),
            selected,
            search_query.is_some(),
            self.state.read(cx).chat_account_badge(&id),
            change_request.clone(),
            jump_label.clone(),
            preview,
            horizon_hovered,
            archived,
            cx.listener(move |this, _, _, cx| {
                this.set_chat_archived(archive_id.clone(), !archived, cx)
            }),
        ) {
            let select_id = id.clone();
            let menu_id = id.clone();
            return row
                .debug_selector({
                    let selector = row_id.clone();
                    move || selector
                })
                .role(gpui::Role::Button)
                .aria_label(title.clone())
                .aria_selected(selected)
                .when(!preview, |el| {
                    el.cursor_pointer()
                        .on_hover({
                            let hover_id = row_id.clone();
                            cx.listener(move |this, hovered: &bool, window, cx| {
                                this.update_chat_row_hover(
                                    &hover_id,
                                    &format!("{hover_id}-hover"),
                                    *hovered,
                                    window,
                                    cx,
                                );
                            })
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.open_chat(select_id.clone(), cx)
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                let closeable = crate::chat_closeout::is_closeout_candidate(
                                    this.state
                                        .read(cx)
                                        .chats
                                        .iter()
                                        .find(|chat| chat.id == menu_id)
                                        .and_then(|chat| chat.cwd.as_deref()),
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
                })
                .when_some(drag, |el, payload| {
                    let shell = cx.entity();
                    el.on_drag(payload, move |payload, point, window, cx| {
                        shell.update(cx, |shell, cx| {
                            shell.begin_sidebar_session_transfer(payload, point, window, cx)
                        });
                        cx.stop_propagation();
                        cx.new(|_| DragGhost)
                    })
                })
                .into_any_element();
        }
        let compact = search_query.is_none() && self.settings.sidebar_compact;
        let show_label = search_query.is_some() || self.settings.sidebar_show_project_label;
        let remote = chat.is_some_and(|chat| {
            self.state.read(cx).local_device_id.as_deref() != Some(chat.device_id.as_str())
        });
        let project_icon = (search_query.is_none() && self.settings.sidebar_show_project_icon)
            .then(|| self.render_project_icon(&id, SIDEBAR_ACTIVE_HARNESS_ICON_SIZE, selected, cx));
        let corner_hovered = !preview && self.chat_status_hover.as_deref() == Some(row_id.as_str());
        let archived_muted = archived && search_query.is_none() && !selected && !corner_hovered;
        let project_icon = project_icon.map(|icon| {
            div()
                .flex_none()
                .opacity(if archived_muted { 0.4 } else { 1.0 })
                .child(icon)
                .into_any_element()
        });
        let content_id = row_scope.map_or_else(|| id.clone(), |scope| format!("{scope}-{id}"));
        let shows_metadata = branch.is_some() || change_request.is_some();
        let queued = queued && !undelivered;
        let working = status == mast_proto::ChatIndicator::Working && !queued && !undelivered;
        let compact_status = compact.then(|| {
            let resolved_status = if undelivered {
                crate::status_style::Status::Errored
            } else if queued {
                crate::status_style::Status::Waiting
            } else {
                status.into()
            };
            let glyph = crate::components::shell_parts::sidebar_status_glyph(
                theme,
                resolved_status,
                working,
                status == mast_proto::ChatIndicator::Completed && !queued && !undelivered,
                format!("{row_id}-working"),
                status_color,
                self.sidebar_pane.entity_id(),
                cx,
            );
            crate::components::shell_parts::compact_sidebar_status(
                theme,
                format!("{row_id}-status"),
                format!("chat-status-{id}"),
                status_label.unwrap_or("Idle"),
                status_color,
                glyph,
            )
        });
        let compact_jump_label = compact.then(|| jump_label.clone()).flatten();
        let corner_body: AnyElement = if let Some(label) = jump_label.filter(|_| !compact) {
            // The jump hint replaces the status/time corner while the modifier
            // is held, cut to the sidebar PR badge's exact cloth
            // (`pull_request_badge`, Sidebar surface): pinned 16px, px 4,
            // rounded 4, borderless 0.08-fill with 0.85 text of one tone —
            // neutral here — and the label in the badge's mono at 10 MEDIUM.
            // Any other geometry reads as a second badge system on the row.
            crate::components::shell_parts::sidebar_jump_badge(theme, label)
        } else if corner_hovered {
            crate::components::shell_parts::sidebar_archive_glyph(
                theme,
                compact,
                archived,
                SIDEBAR_ACTIVE_HARNESS_ICON_SIZE,
            )
        } else if compact {
            if remote {
                icon(icons::REMOTE_SERVER)
                    .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                    .text_color(theme.text_muted.opacity(0.5))
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        } else {
            match status_label {
                Some(label) => {
                    // Glyph slot: Working wears the preset's animated pixel
                    // glyph beside its label, Done wears the check, and the
                    // remaining statuses use a compact dot.
                    let resolved_status = if undelivered {
                        crate::status_style::Status::Errored
                    } else if queued {
                        crate::status_style::Status::Waiting
                    } else {
                        status.into()
                    };
                    let glyph = crate::components::shell_parts::sidebar_status_glyph(
                        theme,
                        resolved_status,
                        working,
                        status == mast_proto::ChatIndicator::Completed,
                        format!("{row_id}-working"),
                        status_color,
                        self.sidebar_pane.entity_id(),
                        cx,
                    );
                    crate::components::shell_parts::sidebar_status_word(label, status_color, glyph)
                }
                None => crate::components::shell_parts::sidebar_idle_time(theme, time_ago.clone()),
            }
        };
        // One stable wrapper across both states (identity keeps the hover
        // from flickering as the content swaps); the swap is driven by the
        // ROW's hover (user request — corner-only felt undiscoverable), but
        // archiving only clicks on the corner itself, so the row's own click
        // stays the selector.
        let corner: AnyElement = {
            let archive_id = id.clone();
            div()
                .id(SharedString::from(format!("{row_id}-corner")))
                .aria_label(if corner_hovered {
                    if archived { "Unarchive" } else { "Archive" }
                } else {
                    if compact {
                        if remote {
                            "Remote session"
                        } else {
                            "Session actions"
                        }
                    } else {
                        status_label.unwrap_or("Idle")
                    }
                })
                .when(compact, |el| el.w(px(18.0)).justify_center())
                .flex_none()
                // Pin the corner to line 1's text height so the archive pill
                // (taller, padded) overflows vertically instead of growing the
                // row — the swap must not shift the card's content.
                // NO occlude: the ROW's hover drives the swap, and an
                // occluding corner un-hovered the row underneath it —
                // pill mounts, steals the pointer, row un-hovers, pill
                // unmounts, repeat (user-reported flicker). The pill's
                // stop_propagation click is separation enough.
                .h(px(14.0))
                .flex()
                .items_center()
                .when(!preview, |el| el.cursor_pointer())
                .when(corner_hovered, |el| {
                    el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_chat_archived(archive_id.clone(), !archived, cx);
                        }))
                })
                .child(corner_body)
                .into_any_element()
        };
        let mut corner = Some(corner);
        let (text, selected_wash, identity_dot) =
            crate::components::shell_parts::sidebar_row_colors(theme, identity, selected);
        let hover = theme.glass_hover();
        let subline = if search_query.is_some() {
            theme.text_muted
        } else {
            theme.text_muted.opacity(0.5)
        };
        let select_id = id.clone();
        let menu_id = id.clone();
        // Hover fades over transition-colors (zeron session-row.tsx) — both
        // the wash and the title brighten ride the same 150ms blend.
        let fade_key = format!("{row_id}-hover");
        let rest_bg = if selected {
            selected_wash
        } else {
            crate::theme::wash(0.0)
        };
        // A selected row must NOT drift toward the hover wash: in dark the two
        // fills are identical so the blend is a no-op, but light's hover sits
        // below its near-opaque selected fill, and blending toward it visibly
        // dimmed the active row under the pointer (user report).
        let hover_bg = if selected { selected_wash } else { hover };
        let rest_text = if selected || search_query.is_some() {
            text
        } else if archived {
            text.opacity(0.55)
        } else {
            text.opacity(0.8)
        };
        crate::components::shell_parts::sidebar_row_frame(
            theme,
            row_id.clone(),
            sidebar_row_height(
                compact,
                show_label,
                branch.is_some(),
                change_request.is_some(),
                theme.ui_style.tokens,
            ),
            search_query.is_some(),
            &fade_key,
            rest_text,
            text,
            rest_bg,
            hover_bg,
        )
        .role(gpui::Role::Button)
        .aria_label(title.clone())
        .aria_selected(selected)
        // No selection ring (user request) — the wash alone marks the
        // active row.
        // Row hover drives BOTH the wash blend and the corner's
        // status→Archive swap (one listener — gpui allows a single
        // hover listener per element).
        .when(!preview, |el| {
            el.on_hover({
                let hover_id = row_id.clone();
                cx.listener(move |this, hovered: &bool, window, cx| {
                    this.update_chat_row_hover(&hover_id, &fade_key, *hovered, window, cx);
                })
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_chat(select_id.clone(), cx);
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
        })
        .when_some(drag, |el, payload| {
            let shell = cx.entity();
            el.on_drag(payload, move |payload, point, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.begin_sidebar_session_transfer(payload, point, window, cx);
                });
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
        })
        // Line 1: "Space @ device", status word / time-ago right.
        .when(!compact && show_label, |el| {
            el.child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(sidebar_faded_label(
                        format!("chat-device-{content_id}").into(),
                        true,
                        crate::components::shell_parts::sidebar_context_text(
                            theme,
                            subline,
                            popover::search_highlight(space_name, search_query, theme),
                        ),
                    ))
                    .child(div().text_color(subline).children(corner.take())),
            )
        })
        // Line 2: harness identity belongs directly with the title,
        // instead of floating as unrelated metadata below it.
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(if compact {
                    4.0
                } else {
                    SIDEBAR_ACTIVE_HARNESS_TITLE_GAP
                }))
                .children(identity_dot)
                .children(compact_status)
                .when_some(
                    harness.map(crate::pickers::harness_brand_icon),
                    |el, (path, tint)| {
                        el.child(
                            icon(path)
                                .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                                .flex_none()
                                .text_color(tint.unwrap_or(subline).opacity(if archived_muted {
                                    0.4
                                } else {
                                    0.8
                                })),
                        )
                    },
                )
                .children(project_icon)
                .child(sidebar_faded_label(
                    format!("chat-title-{content_id}").into(),
                    true,
                    crate::components::shell_parts::sidebar_title_text(
                        theme,
                        popover::search_highlight(title, search_query, theme),
                    ),
                ))
                .when_some(self.state.read(cx).chat_account_badge(&id), |el, label| {
                    el.child(
                        crate::managed_accounts::account_pill(theme, label).debug_selector({
                            let id = id.clone();
                            move || format!("chat-account-{id}")
                        }),
                    )
                })
                .when(!compact && !show_label && remote, |el| {
                    el.child(
                        icon(icons::REMOTE_SERVER)
                            .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                            .flex_none()
                            .text_color(subline),
                    )
                })
                .when(
                    if compact {
                        remote || corner_hovered
                    } else {
                        !show_label
                    },
                    |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_color(subline)
                                .children(corner.take()),
                        )
                    },
                )
                .when(compact, |el| {
                    el.children(change_request.clone().map(|summary| {
                        if preview {
                            crate::change_requests::pull_request_badge_preview(
                                format!("{row_id}-compact-pr").into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                                theme,
                            )
                        } else {
                            crate::change_requests::pull_request_badge(
                                format!("{row_id}-compact-pr").into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                                theme,
                            )
                        }
                    }))
                })
                .when(compact, |el| {
                    el.child(crate::components::shell_parts::sidebar_compact_time(
                        theme,
                        id.clone(),
                        subline,
                        compact_jump_label.unwrap_or(time_ago),
                    ))
                }),
        )
        // Line 3 is structural, not reserved whitespace: compact states
        // omit it completely when both Branch and Pull request are hidden.
        .when(!compact && shows_metadata, |row| {
            row.child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .when_some(branch, |el, branch| {
                        el.child(
                            icon(icons::GIT_BRANCH)
                                .size(px(11.0))
                                .flex_none()
                                .text_color(subline),
                        )
                        .child(sidebar_faded_label(
                            format!("chat-branch-{content_id}").into(),
                            false,
                            crate::components::shell_parts::sidebar_context_text(
                                theme,
                                subline,
                                popover::search_highlight(branch, search_query, theme),
                            ),
                        ))
                    })
                    // Stable invisible spring keeps the optional PR badge
                    // pinned right without changing no-PR paint.
                    .child(div().flex_1().min_w_0())
                    .when_some(change_request, |el, summary| {
                        el.child(if preview {
                            crate::change_requests::pull_request_badge_preview(
                                format!("{row_id}-pr").into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                                theme,
                            )
                        } else {
                            crate::change_requests::pull_request_badge_with_query(
                                format!("{row_id}-pr").into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                                search_query,
                                theme,
                            )
                        })
                    }),
            )
        })
        .into_any_element()
    }

    pub(super) fn render_pinned_session_group(
        items: Vec<AnyElement>,
        extra_gap: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = items.len();
        let row_centers = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Pixels>::new()));
        div()
            .on_children_prepainted({
                let row_centers = row_centers.clone();
                move |bounds, _, _| {
                    *row_centers.borrow_mut() = bounds.iter().map(|row| row.center().y).collect();
                }
            })
            .id("sidebar-pinned-sessions")
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .on_drag_move::<SidebarSessionDrag>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<SidebarSessionDrag>, _, cx| {
                    let payload = event.drag(cx).clone();
                    if !event.bounds.contains(&event.event.position)
                        || !this.sidebar_scroll.bounds().contains(&event.event.position)
                        || payload.visible_ids.len() != count
                    {
                        return;
                    }
                    let rel_y = f32::from(event.event.position.y) - f32::from(event.bounds.top());
                    if !payload.visible_ids.contains(&payload.chat_id) {
                        let top =
                            f32::from(event.bounds.bottom() - this.sidebar_scroll.bounds().top())
                                - extra_gap
                                + SIDEBAR_LIST_GAP
                                - f32::from(this.sidebar_scroll.offset().y);
                        if let Some(drag) = this.sidebar_session_transfer.as_mut() {
                            drag.preview = Some(SidebarSessionGap {
                                group: "pinned".into(),
                                index: count,
                                pinned: true,
                                top,
                            });
                        }
                    }
                    if payload.visible_ids.contains(&payload.chat_id)
                        && let Some(over) =
                            spaces::row_drop_index(rel_y, &this.sidebar_pinned_heights, false)
                    {
                        this.update_pinned_session_drag(&payload, over, cx);
                    }
                },
            ))
            .on_drop::<SidebarSessionDrag>(cx.listener(
                move |this, payload: &SidebarSessionDrag, window, cx| {
                    if payload.visible_ids.len() == count {
                        let index = this
                            .sidebar_session_transfer
                            .as_ref()
                            .filter(|_| !payload.visible_ids.contains(&payload.chat_id))
                            .and_then(|drag| drag.preview.as_ref())
                            .filter(|gap| gap.pinned)
                            .map(|gap| gap.index)
                            .or_else(|| this.pinned_session_drag.as_ref().map(|drag| drag.over))
                            .unwrap_or_else(|| {
                                row_centers
                                    .borrow()
                                    .iter()
                                    .position(|center| window.mouse_position().y < *center)
                                    .unwrap_or(count)
                            });
                        this.finish_sidebar_session_transfer(
                            payload,
                            SidebarSessionDrop::Pinned(index),
                            cx,
                        );
                    } else {
                        this.cancel_sidebar_session_transfer(cx);
                    }
                },
            ))
            .children(items)
            .when(extra_gap > 0.0, |el| {
                let height = extra_gap - if count > 0 { SIDEBAR_LIST_GAP } else { 0.0 };
                el.child(
                    div()
                        .flex_none()
                        .h(px(height.max(0.0)))
                        .mb(px(height.min(0.0))),
                )
            })
            .into_any_element()
    }
}
