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
        let width = if self.sidebar_is_overlay() {
            if self.settings.sidebar_collapsed || self.overview_overlay_dismissed {
                0.0
            } else {
                self.sidebar_shown_width()
            }
        } else {
            self.sidebar_now()
        };
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
        self.overview_visible_chat_ids = if self.sidebar_shows_overview() {
            let page = self.ensure_overview_page(cx);
            page.update(cx, |page, cx| {
                page.set_sidebar_scope(self.settings.space_filter.clone(), cx);
                page.sidebar_filter_ids(cx)
            })
        } else {
            None
        };
        let (user, workspace_scope) = {
            let state = self.state.read(cx);
            (state.auth_user().cloned(), state.workspace_scope)
        };

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
        let assistant_section = if self.sidebar_shows_overview() {
            None
        } else {
            self.render_personal_assistant_section(theme, cx)
        };
        let sidebar_lists = if self.sidebar_shows_overview() {
            None
        } else {
            Some(self.render_session_sidebar_lists(theme, cx))
        };
        self.prune_sidebar_row_views();

        div()
            .w(px(self.sidebar_shown_width()))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .children(assistant_section)
            .child(filter_row)
            .when(theme.ui_style.layout != crate::ui_style::UiLayout::Compact, |el| {
                el.child(self.render_overview_switch(theme, cx))
            })
            .children(self.render_overview_filter_notice(theme, cx))
            .children(crate::ui_style::help_card(
                "sidebar",
                "Chats show current activity and items needing your attention.",
                theme,
                cx,
            ))
            .children(sidebar_lists)
            .when(self.sidebar_shows_overview(), |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(self.ensure_overview_navigation(cx)),
                )
            })
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
