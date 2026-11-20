use super::*;

/// Flex gap between sidebar list items.
pub(super) const SIDEBAR_LIST_GAP: f32 = 2.0;
/// Fixed vertical slot occupied by one active sidebar card.
#[cfg(test)]
pub(super) const SIDEBAR_SESSION_SLOT: f32 = 61.0 + SIDEBAR_LIST_GAP;
pub(super) const SIDEBAR_DRAG_SCROLL_BAND: f32 = 48.0;
pub(super) const SIDEBAR_DRAG_SCROLL_MAX: f32 = 12.0;
pub(super) const SIDEBAR_DRAG_SCROLL_FRAME_MS: u64 = 16;
pub(super) const SIDEBAR_LIST_PAD_TOP: f32 = 4.0;

/// Active and archived sessions share harness/title geometry.
pub(super) const SIDEBAR_ACTIVE_HARNESS_ICON_SIZE: f32 = 13.0;
pub(super) const SIDEBAR_ACTIVE_HARNESS_TITLE_GAP: f32 = Theme::SPACE_SM;

/// Ramp height of the sidebar's scroll-edge fade (the gpui
/// [`gpui::EdgeFade`] scope — per-primitive, so text fades per glyph).
pub(super) const SIDEBAR_GLASS_FADE_BAND: f32 = 24.0;

/// Sidebar-only drag payload. Regular sessions never acquire a manual order.
#[derive(Clone, PartialEq)]
pub(super) struct SidebarSessionDrag {
    pub(super) chat_id: String,
    pub(super) visible_ids: std::sync::Arc<Vec<String>>,
    pub(super) filter: Option<String>,
    pub(super) profile_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SidebarSessionDrop {
    Pinned(usize),
    Regular,
    Section(String),
}

pub(super) struct SidebarSessionTransfer {
    pub(super) payload: SidebarSessionDrag,
    pub(super) origin: std::rc::Rc<std::cell::Cell<Point<Pixels>>>,
    pub(super) cursor_offset: Point<Pixels>,
    pub(super) pointer: Point<Pixels>,
    pub(super) viewport: Option<gpui::Bounds<Pixels>>,
    pub(super) slide: SidebarSessionSlide,
    pub(super) preview: Option<SidebarSessionGap>,
    pub(super) source_group: String,
    pub(super) source_index: usize,
    pub(super) row_height: f32,
    pub(super) source_collapse: SidebarSessionSlide,
    pub(super) collapsed_height: f32,
    pub(super) section_gaps: std::collections::HashMap<String, SidebarSessionSlide>,
    pub(super) siblings: std::collections::HashMap<String, SidebarSessionSlide>,
}

#[derive(Clone)]
pub(super) struct SidebarSessionGap {
    pub(super) group: String,
    pub(super) index: usize,
    pub(super) pinned: bool,
    pub(super) top: f32,
}

/// The same slot-to-slot easing as pinned reordering, applied to the actual row.
pub(super) struct SidebarSessionSlide {
    pub(super) from: f32,
    pub(super) to: f32,
    pub(super) epoch: u64,
    pub(super) started: std::time::Instant,
}

impl SidebarSessionSlide {
    pub(super) fn current(&self) -> f32 {
        let progress = TAB_SLIDE
            .progress(self.started.elapsed().as_secs_f32() / TAB_SLIDE.total().as_secs_f32());
        motion::lerp(self.from, self.to, progress)
    }

    pub(super) fn retarget(&mut self, target: f32) {
        if (target - self.to).abs() < 0.5 {
            return;
        }
        self.from = self.current();
        self.to = target;
        self.epoch = self.epoch.wrapping_add(1);
        self.started = std::time::Instant::now();
    }
}

pub(super) struct SidebarSessionReturn {
    pub(super) transfer: SidebarSessionTransfer,
    pub(super) epoch: u64,
    pub(super) started: std::time::Instant,
}

/// Live destination for a pinned-session drag. The real row remains clipped
/// to the sidebar and slides between slots with its pinned siblings.
pub(super) struct PinnedSessionDragState {
    pub(super) chat_id: String,
    pub(super) visible_ids: std::sync::Arc<Vec<String>>,
    pub(super) from: usize,
    pub(super) over: usize,
    pub(super) prev_over: usize,
    pub(super) epoch: usize,
    pub(super) filter: Option<String>,
    pub(super) profile_key: String,
    pub(super) pointer_y: Option<f32>,
    pub(super) viewport_top: f32,
    pub(super) viewport_bottom: f32,
    pub(super) generation: u64,
    pub(super) autoscroll_active: bool,
}

pub(super) type SidebarKeyedRow = (String, f32, AnyElement);

pub(super) struct SidebarSessionRows {
    pub(super) custom_count: usize,
    pub(super) regular_count: usize,
    pub(super) rows: Vec<SidebarKeyedRow>,
    pub(super) pinned_count: usize,
    pub(super) moving_row: Option<(AnyElement, f32)>,
}

/// Sidebar resort glide (feature-inventory §1.6): 260ms
/// `cubic-bezier(0.22,1,0.36,1)` per-row translate, the View Transitions
/// equivalent.
pub const RESORT: MotionSpec = MotionSpec::new(260, motion::EASE_RESORT);

/// FLIP diff for a keyed list: given the previously rendered order and the new
/// order (key + row height), return each surviving key's paint-only start
/// offset `old_y - new_y` (only keys whose position actually moved). `gap` is
/// the flex gap between rows. Pure — drives the sidebar resort glide.
pub fn resort_offsets(
    old: &[(String, f32)],
    new: &[(String, f32)],
    gap: f32,
) -> std::collections::HashMap<String, f32> {
    let mut old_y = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in old {
        old_y.insert(key.as_str(), y);
        y += height + gap;
    }
    let mut offsets = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in new {
        if let Some(prev) = old_y.get(key.as_str()) {
            let dy = prev - y;
            if dy.abs() > 0.5 {
                offsets.insert(key.clone(), dy);
            }
        }
        y += height + gap;
    }
    offsets
}

/// Height changes do not constitute a list reorder. In particular, sidebar
/// disclosures animate their own height and must not also trigger FLIP offsets
/// on every following keyed section.
pub(super) fn sidebar_key_order_changed(old: &[(String, f32)], new: &[(String, f32)]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|((old_key, _), (new_key, _))| old_key != new_key)
}

impl Shell {
    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let width = self.sidebar_now();
        if width <= 0.0 {
            return div().w(px(0.0)).h_full().flex_none().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        // The sidebar is part of the resolved theme. A second fixed-Zeron
        // palette here made imported families look split in half and froze
        // activity/glyph personality independently of the selected variant.
        // While a working chat's spinner is on screen, the pane must NOT be a
        // `.cached()` view: a spinner tick dirties the pane as an ancestor, and
        // gpui re-renders a dirty cached view with `refreshing` set for its whole
        // subtree, which defeats the per-row cached views below it (every row
        // was rebuilt and re-prepainted each tick). Uncached, the pane re-emits
        // cheap row stubs and only the rows that changed re-render. With nothing
        // animating the cached pane keeps skipping Shell renders it does not care
        // about (streaming deltas, composer typing).
        let animating = self
            .state
            .read(cx)
            .sessions
            .iter()
            .any(|session| session.status == mast_proto::SessionStatus::Working);
        let pane = self.sidebar_pane.clone();
        let inner: AnyElement = if animating {
            div()
                .w(px(self.sidebar_shown_width()))
                .h_full()
                .flex_none()
                .child(pane)
                .into_any_element()
        } else {
            pane.cached(
                gpui::StyleRefinement::default()
                    .w(px(self.sidebar_shown_width()))
                    .h_full()
                    .flex_none(),
            )
            .into_any_element()
        };
        // Transparent — the sidebar sits directly on the frost shell; the main
        // card's own border provides the separation. The content row spans the
        // full window height (the titlebar overlays it), so the column pads
        // itself below the chrome.
        crate::components::shell_parts::sidebar_frame(
            &theme,
            width,
            inner,
            cx.listener(|this, _, _, cx| this.open_new_session(cx)),
        )
    }

    /// Settings-mode sidebar (zeron settings-sidebar.tsx): window-control
    /// strip, "Settings" heading, icon section rows styled like session rows,
    /// and a Back row pinned to the bottom.
    pub(super) fn render_chat_sidebar(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(feature = "startup-bench")]
        let _render_span = crate::startup_measurement::MeasurementSpan::new("chat_sidebar");
        if self.sidebar_session_transfer.as_ref().is_some_and(|drag| {
            !cx.has_active_drag() || !self.sidebar_session_transfer_is_valid(&drag.payload, cx)
        }) {
            self.cancel_sidebar_session_transfer(cx);
        }
        // A release outside the sidebar ends GPUI's drag without calling the
        // sidebar drop handler. Heal the ephemeral slide state here.
        if self.pinned_session_drag.is_some()
            && (!cx.has_active_drag() || !self.pinned_session_drag_is_valid(cx))
        {
            self.cancel_pinned_session_drag(cx);
        }
        let (user, workspace_scope) = {
            let state = self.state.read(cx);
            (state.auth_user().cloned(), state.workspace_scope)
        };

        // Keyed rows: (stable key, estimated height, element) — the key + height
        // list drives the §1.6 resort FLIP diff below (attention-bucket
        // promotions glide; cleared rows just go).
        let session_rows = self.render_active_rows(theme, cx);
        self.prune_sidebar_row_views();
        let moving_row = session_rows
            .moving_row
            .map(|(row, height)| self.render_moving_sidebar_session(row, height, theme));
        let pinned_count = session_rows.pinned_count;
        let custom_count = session_rows.custom_count;
        let regular_start = pinned_count + custom_count;
        let keyed = session_rows.rows;
        let regular_count = session_rows.regular_count;
        let ungrouped = self.settings.sidebar_organization == SidebarOrganization::InOneList;
        let regular_body_height = spaces::SIDEBAR_DISCLOSURE_BODY_INSET
            + keyed
                .iter()
                .skip(regular_start)
                .map(|(_, height, _)| height)
                .sum::<f32>()
            + SIDEBAR_LIST_GAP * keyed.len().saturating_sub(regular_start + 1) as f32;
        let regular_body_height =
            if keyed.len() == regular_start && self.sidebar_session_transfer.is_some() {
                spaces::SIDEBAR_DISCLOSURE_BODY_INSET
                    + 48.0
                    + self.sidebar_transfer_extra_gap("regular")
            } else {
                regular_body_height
            };
        let pinned_body_height = spaces::SIDEBAR_DISCLOSURE_BODY_INSET
            + self.sidebar_transfer_extra_gap("pinned")
            + keyed
                .iter()
                .take(pinned_count)
                .map(|(_, height, _)| height)
                .sum::<f32>()
            + SIDEBAR_LIST_GAP * pinned_count.saturating_sub(1) as f32;

        // Resort glide (§1.6 View Transitions parity): when the ORDER of a live
        // list changes (new activity resort, grouping flip), surviving rows
        // glide from their old y to the new one — layout is already at the new
        // position; the offset is a paint-only relative inset animated to 0
        // over 260ms cubic-bezier(0.22,1,0.36,1). New rows fade in; removals
        // just go (matching the original). First fill and chat switches (which
        // don't reorder) never animate.
        let mut order: Vec<(String, f32)> = Vec::with_capacity(keyed.len() + 2);
        let show_pinned_section = pinned_count > 0 || self.sidebar_session_transfer.is_some();
        if show_pinned_section {
            order.push((
                "sidebar-pinned-header".to_string(),
                spaces::SIDEBAR_DISCLOSURE_HEADER_HEIGHT
                    + if self.pinned_open {
                        spaces::SIDEBAR_DISCLOSURE_BODY_INSET - SIDEBAR_LIST_GAP
                    } else {
                        0.0
                    },
            ));
        }
        for (ix, (key, height, _)) in keyed.iter().enumerate() {
            if ungrouped && ix == regular_start {
                order.push((
                    "sidebar-sessions-header".into(),
                    spaces::SIDEBAR_DISCLOSURE_HEADER_HEIGHT
                        + if show_pinned_section || custom_count > 0 {
                            12.0
                        } else {
                            0.0
                        }
                        + if self.sessions_open {
                            spaces::SIDEBAR_DISCLOSURE_BODY_INSET - SIDEBAR_LIST_GAP
                        } else {
                            0.0
                        },
                ));
            }
            if ungrouped && ix >= regular_start && !self.sessions_open {
                continue;
            }
            if ix < pinned_count && !self.pinned_open {
                continue;
            }
            order.push((key.clone(), *height));
        }
        if self.pinned_session_drag.is_none()
            && self.sidebar_session_transfer.is_none()
            && self.sidebar_prev_order != order
        {
            let key_order_changed = sidebar_key_order_changed(&self.sidebar_prev_order, &order);
            if !self.sidebar_prev_order.is_empty() {
                // A disclosure already animates its own body height. Applying
                // FLIP offsets when only keyed heights change double-counts
                // that movement, leaving gaps and momentary overlaps between
                // the first group, following groups, and Archived.
                let offsets = if key_order_changed {
                    resort_offsets(&self.sidebar_prev_order, &order, SIDEBAR_LIST_GAP)
                } else {
                    std::collections::HashMap::new()
                };
                let prev_keys: std::collections::HashSet<&str> = self
                    .sidebar_prev_order
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                let new_keys: std::collections::HashSet<String> = order
                    .iter()
                    .filter(|(k, _)| !prev_keys.contains(k.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                if key_order_changed && (!offsets.is_empty() || !new_keys.is_empty()) {
                    self.resort_epoch += 1;
                    self.sidebar_resort = offsets;
                    self.sidebar_new_keys = new_keys;
                }
            }
            self.sidebar_prev_order = order;
        }
        let epoch = self.resort_epoch;
        let pinned_drag = self
            .pinned_session_drag
            .as_ref()
            .filter(|_| {
                self.sidebar_session_transfer
                    .as_ref()
                    .is_none_or(|drag| drag.preview.as_ref().is_none_or(|gap| gap.pinned))
            })
            .map(|drag| (drag.from, drag.over, drag.prev_over, drag.epoch));
        let list_items: Vec<AnyElement> = keyed
            .into_iter()
            .enumerate()
            .map(|(ix, (key, _, element))| {
                if ix < pinned_count
                    && let Some((from, over, prev_over, drag_epoch)) = pinned_drag
                {
                    // Its actual row is drawn once in the sidebar's movement layer.
                    if ix == from {
                        return element;
                    }
                    let start = crate::terminal::panel::slide_offset(ix, from, prev_over)
                        * (self
                            .sidebar_pinned_heights
                            .get(from)
                            .copied()
                            .unwrap_or(61.0)
                            + SIDEBAR_LIST_GAP);
                    let target = crate::terminal::panel::slide_offset(ix, from, over)
                        * (self
                            .sidebar_pinned_heights
                            .get(from)
                            .copied()
                            .unwrap_or(61.0)
                            + SIDEBAR_LIST_GAP);
                    if self.reduced_motion {
                        return div()
                            .relative()
                            .top(px(target))
                            .child(element)
                            .into_any_element();
                    }
                    return div()
                        .child(element)
                        .with_animation(
                            (
                                "pinned-session-slide",
                                (ix as u64) | ((drag_epoch as u64) << 32),
                            ),
                            TAB_SLIDE.animation(),
                            move |el, t| el.relative().top(px(motion::lerp(start, target, t))),
                        )
                        .into_any_element();
                }
                if self.sidebar_session_transfer.is_some() {
                    return element;
                }
                if let Some(dy) = self.sidebar_resort.get(&key).copied() {
                    let id = SharedString::from(format!("resort-{epoch}-{key}"));
                    div()
                        .child(element)
                        .with_animation(id, RESORT.animation(), move |el, t| {
                            el.relative().top(px(dy * (1.0 - t)))
                        })
                        .into_any_element()
                } else if self.sidebar_new_keys.contains(&key) {
                    let id = SharedString::from(format!("row-in-{epoch}-{key}"));
                    motion::fade_quick(id, div().child(element)).into_any_element()
                } else {
                    element
                }
            })
            .collect();

        // t3code's archived accordion, below the active list.
        let archived_section = self.render_archived_section(theme, cx);

        let (user_line, menu_identity): (SharedString, SharedString) = match workspace_scope {
            Some(WorkspaceScope::Local) => {
                let line = if matches!(self.sync_flow, SyncFlow::RestartPending { .. }) {
                    "Sync ready after restart"
                } else {
                    "Local only"
                };
                (line.into(), "Stored on this device".into())
            }
            Some(WorkspaceScope::Development) => {
                ("Development".into(), "Authentication disabled".into())
            }
            Some(WorkspaceScope::Synced) | None => {
                let line: SharedString = user
                    .as_ref()
                    .map(|u| u.name.clone().unwrap_or_else(|| u.email.clone()).into())
                    .unwrap_or_else(|| "Not signed in".into());
                let email = user
                    .as_ref()
                    .map(|u| SharedString::from(u.email.clone()))
                    .unwrap_or_else(|| line.clone());
                (line, email)
            }
        };
        let user_menu = self.render_user_menu(user_line, menu_identity, theme, cx);

        // The space filter lives ABOVE the scroll region (fixed) so its
        // dropdown can float without being clipped by the list's overflow.
        let filter_row = self.render_spaces_filter(theme, cx);
        let assistant_section = self.render_personal_assistant_section(theme, cx);
        let active_list = if !list_items.is_empty() {
            let mut pinned_items = list_items;
            let mut custom_items = pinned_items.split_off(pinned_count);
            let regular_items = custom_items.split_off(custom_count);
            let regular_empty = regular_items.is_empty();
            let pinned_group = show_pinned_section
                .then(|| self.render_pinned_section(pinned_items, pinned_body_height, theme, cx));
            div()
                .id("sidebar-active-sessions")
                .flex()
                .flex_col()
                .gap(px(SIDEBAR_LIST_GAP))
                .pb(px(Theme::SPACE_SM))
                .when_some(pinned_group, |el, group| el.child(group))
                .children(custom_items)
                .when(
                    !regular_items.is_empty() || self.sidebar_session_transfer.is_some(),
                    |el| {
                        el.child(
                            self.render_sessions_section(
                                div()
                                    .id("sidebar-regular-sessions")
                                    .debug_selector(|| "sidebar-regular-sessions".into())
                                    .on_drag_move::<SidebarSessionDrag>(cx.listener(
                                        move |this,
                                              event: &gpui::DragMoveEvent<SidebarSessionDrag>,
                                              _,
                                              _| {
                                            if regular_empty
                                                && event.bounds.contains(&event.event.position)
                                            {
                                                let top = f32::from(
                                                    event.bounds.top()
                                                        - this.sidebar_scroll.bounds().top()
                                                        - this.sidebar_scroll.offset().y,
                                                );
                                                if let Some(drag) =
                                                    this.sidebar_session_transfer.as_mut()
                                                {
                                                    drag.preview = Some(SidebarSessionGap {
                                                        group: "regular".into(),
                                                        index: 0,
                                                        pinned: false,
                                                        top,
                                                    });
                                                }
                                                return;
                                            }
                                            if event.bounds.contains(&event.event.position)
                                                && let Some(drag) =
                                                    this.sidebar_session_transfer.as_mut()
                                                && drag
                                                    .preview
                                                    .as_ref()
                                                    .is_some_and(|gap| gap.pinned)
                                            {
                                                drag.preview = None;
                                            }
                                        },
                                    ))
                                    .on_drop::<SidebarSessionDrag>(cx.listener(
                                        |this, payload, _, cx| {
                                            this.finish_sidebar_session_transfer(
                                                payload,
                                                SidebarSessionDrop::Regular,
                                                cx,
                                            );
                                        },
                                    ))
                                    .flex()
                                    .flex_col()
                                    .gap(px(SIDEBAR_LIST_GAP))
                                    .when(regular_items.is_empty(), |el| {
                                        el.h(px(48.0 + self.sidebar_transfer_extra_gap("regular")))
                                            .justify_center()
                                            .px(px(10.0))
                                            .text_color(theme.text_muted)
                                            .text_size(crate::typography::ui_rems(12.0))
                                            .child("Drop here to unpin")
                                    })
                                    .children(regular_items)
                                    .into_any_element(),
                                regular_body_height,
                                regular_count,
                                show_pinned_section || custom_count > 0,
                                theme,
                                cx,
                            ),
                        )
                    },
                )
                .into_any_element()
        } else {
            div()
                .px(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_SM))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from("No sessions yet"))
                .into_any_element()
        };

        // The (filtered) Sessions list scrolls inside an EdgeFade scope —
        // a true per-glyph gradient at active overflow edges. Glass-safe
        // (no painted overlay can fade content over see-through blur) and
        // equivalent on opaque themes: alpha→0 reveals the surface tone
        // underneath, same as the gradient overlays it replaced. Overflow
        // is read at PAINT time via the scroll handle — render-time gating
        // rode the previous frame's offset, so the last frame of a content
        // shrink (row archived while scrolled) left a phantom fade stuck
        // over an unscrollable list (user report).
        let sidebar_lists = crate::edge_fade::edge_faded(
            SIDEBAR_GLASS_FADE_BAND,
            true,
            true,
            div().relative().flex_1().min_h_0().child(
                div()
                    .id("sidebar-lists")
                    .relative()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .on_drag_move::<SidebarSessionDrag>(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<SidebarSessionDrag>, _, cx| {
                            if let Some(transfer) = this.sidebar_session_transfer.as_mut() {
                                transfer.viewport = Some(event.bounds);
                            }
                            if !event.bounds.contains(&event.event.position) {
                                if let Some(transfer) = this.sidebar_session_transfer.as_mut() {
                                    transfer.preview = None;
                                }
                                return;
                            }
                            let payload = event.drag(cx).clone();
                            this.track_pinned_session_drag_pointer(
                                payload,
                                f32::from(event.event.position.y),
                                f32::from(event.bounds.top()),
                                f32::from(event.bounds.bottom()),
                                cx,
                            );
                        },
                    ))
                    // Empty space and Archived are not transfer targets.
                    .on_drop::<SidebarSessionDrag>(cx.listener(
                        |this, _: &SidebarSessionDrag, _, cx| {
                            this.cancel_sidebar_session_transfer(cx);
                        },
                    ))
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_col()
                    // No "Sessions" header (user request) — the list
                    // is the whole column; a little air stands in.
                    .pt(px(SIDEBAR_LIST_PAD_TOP))
                    .child(active_list)
                    .children(archived_section)
                    .children(moving_row),
            ),
        )
        .fade_overflow_y(&self.sidebar_scroll);

        div()
            .w(px(self.sidebar_shown_width()))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .children(assistant_section)
            .child(filter_row)
            .children(crate::ui_style::help_card(
                "sidebar",
                "Chats show current activity and items needing your attention.",
                theme,
                cx,
            ))
            .child(sidebar_lists)
            .children(crate::components::shell_parts::sidebar_grille(theme))
            // Global connection pill (durable-by-design UI truth): appears
            // whenever the edge posture is degraded; hidden while healthy —
            // appearing IS the signal.
            .when_some(self.render_connection_pill(theme, cx), |el, pill| {
                el.child(pill)
            })
            // Update strip (above the user menu; below the lists).
            .when_some(self.render_update_strip(theme, cx), |el, strip| {
                el.child(strip)
            })
            // Inline mutation-failure notice.
            .when_some(
                self.sidebar_notice
                    .clone()
                    .filter(|notice| self.sidebar_notice_is_error(notice)),
                |el, notice| {
                    // Failures get the persistent, copyable error card.
                    el.child(
                        div()
                            .id("sidebar-notice")
                            .debug_selector(|| "sidebar-error-notice".into())
                            .mx(px(Theme::SPACE_SM))
                            .mb(px(Theme::SPACE_SM))
                            .child(
                                crate::error_notice::ErrorNotice::from_text(
                                    "sidebar-error-card",
                                    theme,
                                    &notice,
                                )
                                .on_dismiss({
                                    let shell = cx.entity();
                                    move |_, cx| {
                                        shell.update(cx, |this, cx| {
                                            this.sidebar_notice = None;
                                            cx.notify();
                                        })
                                    }
                                }),
                            ),
                    )
                },
            )
            .when_some(
                self.sidebar_notice
                    .clone()
                    .filter(|notice| !self.sidebar_notice_is_error(notice)),
                |el, notice| {
                    el.child(
                        div()
                            .id("sidebar-notice")
                            .mx(px(Theme::SPACE_SM))
                            .mb(px(Theme::SPACE_SM))
                            .px(px(Theme::SPACE_SM))
                            .py(px(4.0))
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.danger)
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.danger)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_notice = None;
                                cx.notify();
                            }))
                            .child(notice),
                    )
                },
            )
            .child(div().p(px(Theme::SPACE_SM)).flex_none().child(user_menu))
            .into_any_element()
    }
}
