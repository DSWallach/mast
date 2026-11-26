//! Shell placement and routing for the unified Overview column.
use super::*;

impl Shell {
    pub(crate) fn keep_outline_navigation_focus(&mut self, cx: &mut Context<Self>) {
        // The row's mouse handler already focused the tree. A click should
        // display the chat without replaying the composer's pending focus.
        self.composer
            .update(cx, |composer, _| composer.focus_pending = false);
    }

    pub(super) fn sidebar_is_overlay(&self) -> bool {
        self.viewport_width > 0.0
            && self.viewport_width < 780.0
            && !matches!(self.route, Route::Settings(_))
    }

    pub(super) fn dismiss_overview_overlay(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_is_overlay() && !self.overview_overlay_dismissed {
            self.overview_overlay_dismissed = true;
            cx.notify();
        }
    }

    pub(super) fn place_overview_sidebar(
        &mut self,
        sidebar: AnyElement,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>) {
        if !self.sidebar_is_overlay() {
            return (sidebar, None);
        }
        let floating = if !self.settings.sidebar_collapsed && !self.overview_overlay_dismissed {
            let theme = Theme::of(cx).clone();
            Some(
                div()
                    .id("overview-sidebar-overlay")
                    .debug_selector(|| "overview-sidebar-overlay".into())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(
                        div()
                            .id("overview-sidebar-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.text.opacity(0.14))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.dismiss_overview_overlay(cx);
                                    cx.stop_propagation();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(self.sidebar_shown_width()))
                            .bg(theme.surface_raised)
                            .child(sidebar),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        (div().w(px(0.0)).flex_none().into_any_element(), floating)
    }

    pub(super) fn sidebar_shows_overview(&self) -> bool {
        #[cfg(test)]
        if self.legacy_sessions_for_test {
            return false;
        }
        true
    }

    pub(super) fn sidebar_chat_navigation_order(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        #[cfg(test)]
        if self.legacy_sessions_for_test {
            return self.sidebar_visible_order(cx);
        }

        self.ensure_overview_navigation(cx);
        let page = self.ensure_overview_page(cx);
        let outline = page.update(cx, |page, cx| page.sidebar_outline_for_navigation(cx));
        let mut seen = std::collections::HashSet::new();
        outline
            .read(cx)
            .model()
            .rows
            .iter()
            .filter_map(|row| {
                row.open_chat()
                    .filter(|row| !row.dangling)
                    .map(|row| row.chat.id.clone())
                    .or_else(|| row.manager().and_then(|row| row.chat_id.clone()))
            })
            .chain(self.scheduled_visible_chat_ids(cx))
            .filter(|id| seen.insert(id.clone()))
            .collect()
    }

    pub(crate) fn overview_navigation_visible(&self) -> bool {
        self.sidebar_shows_overview()
            && !self.settings.sidebar_collapsed
            && !(self.sidebar_is_overlay() && self.overview_overlay_dismissed)
            && !matches!(self.route, Route::Settings(_))
    }

    pub(super) fn reveal_overview_navigation(&mut self, cx: &mut Context<Self>) {
        let changed = self.settings.sidebar_collapsed;
        #[cfg(test)]
        {
            self.legacy_sessions_for_test = false;
        }
        self.overview_overlay_dismissed = false;
        self.settings.sidebar_collapsed = false;
        if matches!(self.route, Route::Settings(_)) {
            self.route = Route::Chat;
        }
        self.ensure_overview_navigation(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |page, cx| page.request_navigation_focus(cx));
        if changed {
            self.schedule_save(cx);
        }
        cx.notify();
    }

    pub(crate) fn show_overview_workspace(&mut self, cx: &mut Context<Self>) {
        self.dismiss_overview_overlay(cx);
        if self.route != Route::Overview {
            self.route = Route::Overview;
            self.nav.push(NavEntry::Overview);
            cx.notify();
        }
    }

    pub(crate) fn return_from_overview_workspace(&mut self, cx: &mut Context<Self>) {
        if self.route == Route::Overview {
            self.route = Route::Chat;
            self.nav.push(NavEntry::Chat(self.active_chat.clone()));
            cx.notify();
        }
    }

    pub(super) fn ensure_overview_navigation(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::overview::OverviewNavigation> {
        let page = self.ensure_overview_page(cx);
        page.update(cx, |page, cx| {
            page.set_sidebar_scope(self.settings.space_filter.clone(), cx)
        });
        let pins = self.active_sidebar_pins(cx).into_iter().collect();
        page.read(cx).outline_entity().update(cx, |outline, cx| {
            outline.set_pinned_chats(pins, cx);
        });
        self.overview_navigation
            .get_or_insert_with(|| cx.new(|cx| crate::overview::OverviewNavigation::new(page, cx)))
            .clone()
    }

    pub(super) fn render_overview_filter_notice(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ids = self.overview_visible_chat_ids.as_ref()?;
        let page = self.overview_page.clone()?;
        let state = self.state.read(cx);
        let hidden = state
            .chats
            .iter()
            .filter(|chat| {
                chat.parent_chat_id.is_none()
                    && !chat.is_side_chat()
                    && !crate::state::chat_hidden_by_standing_role(chat)
                    && chat
                        .space_id
                        .as_deref()
                        .is_none_or(|id| state.space_row(id).is_some())
                    && self
                        .settings
                        .space_filter
                        .as_deref()
                        .is_none_or(|scope| chat.space_id.as_deref() == Some(scope))
                    && !ids.contains(&chat.id)
            })
            .count();
        Some(
            div()
                .id("overview-clear-filters")
                .min_w_0()
                .whitespace_normal()
                .debug_selector(|| "overview-clear-filters".into())
                .mx(px(8.0))
                .py(px(4.0))
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .child(format!("{hidden} chats filtered out · Clear filters"))
                .on_click(move |_, _, cx| {
                    page.update(cx, |page, cx| page.clear_sidebar_filters(cx))
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
#[path = "../../tests/support/overview_unit.rs"]
pub(super) mod tests;
