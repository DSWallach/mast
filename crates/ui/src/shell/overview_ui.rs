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

    pub(crate) fn overview_navigation_visible(&self) -> bool {
        self.settings.sidebar_overview
            && !self.settings.sidebar_collapsed
            && !(self.sidebar_is_overlay() && self.overview_overlay_dismissed)
            && !matches!(self.route, Route::Settings(_))
    }

    pub(super) fn reveal_overview_navigation(&mut self, cx: &mut Context<Self>) {
        let changed = !self.settings.sidebar_overview || self.settings.sidebar_collapsed;
        self.settings.sidebar_overview = true;
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
        self.overview_navigation
            .get_or_insert_with(|| cx.new(|cx| crate::overview::OverviewNavigation::new(page, cx)))
            .clone()
    }

    pub(super) fn render_overview_switch(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .gap(px(4.0))
            .px(px(Theme::SPACE_SM))
            .pb(px(4.0))
            .children([(true, "Overview"), (false, "Sessions")].into_iter().map(
                |(outline, label)| {
                    crate::components::outline_toolbar_button(
                        if outline {
                            "sidebar-overview"
                        } else {
                            "sidebar-sessions"
                        },
                        label.to_string(),
                        self.settings.sidebar_overview == outline,
                        theme,
                    )
                    .debug_selector(move || {
                        if outline {
                            "sidebar-overview"
                        } else {
                            "sidebar-sessions"
                        }
                        .into()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.sidebar_overview = outline;
                        this.cancel_sidebar_session_transfer(cx);
                        this.cancel_pinned_session_drag(cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }))
                },
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    struct Host {
        shell: Entity<Shell>,
        _observation: Subscription,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.shell.update(cx, |shell, cx| {
                shell.viewport_width = f32::from(window.viewport_size().width);
                let theme = Theme::of(cx).clone();
                let sidebar = shell.render_chat_sidebar(&theme, cx);
                let (sidebar, floating) = shell.place_overview_sidebar(sidebar, cx);
                let main = shell.render_main(window, 800.0, 800.0, cx);
                div()
                    .size_full()
                    .relative()
                    .child(div().size_full().flex().child(sidebar).child(main))
                    .children(floating)
            })
        }
    }

    fn setup(
        cx: &mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            cx.set_global(Theme::default());
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::app_menus::init(cx);
        });
        let shell = cx.new(|cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.data_dir = Some(dir.path().into());
                state.auto_selected = true;
                state.no_project = true;
                state.chats_synced = true;
                state.selected_chat = Some("chat".into());
                state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "chat", "deviceId": "local", "title": "Existing chat",
                        "createdAt": "2026-11-01T00:00:00Z", "archived": false
                    }))
                    .unwrap(),
                ];
                state
            });
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.splash = SplashPhase::Gone;
            shell.debug_gate = Some(GatePhase::Ready);
            shell
        });
        let view = shell.clone();
        let (_, vcx) = cx.add_window_view(move |_, cx| Host {
            _observation: cx.observe(&view, |_, _, cx| cx.notify()),
            shell: view,
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        (dir, shell, vcx)
    }

    #[gpui::test]
    fn overview_column_stays_beside_chat_and_workspace_destinations(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        let navigation = vcx
            .debug_bounds("overview-navigation")
            .expect("persistent navigation");
        assert!(f32::from(navigation.left()) < 360.0);
        assert!(
            vcx.debug_bounds("overview-chat-panel").is_none(),
            "Shell hosts the live chat"
        );
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        page.update(vcx, |page, cx| page.open_required_actions(cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        shell.read_with(vcx, |shell, cx| {
            assert_eq!(shell.route, Route::Overview);
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
        });
        assert!(vcx.debug_bounds("overview-navigation").is_some());
        assert!(vcx.debug_bounds("overview-chat-panel").is_none());
        page.update(vcx, |page, cx| page.close_required_actions(cx));
        vcx.run_until_parked();
        shell.read_with(vcx, |shell, cx| {
            assert_eq!(shell.route, Route::Chat);
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
        });
    }

    #[gpui::test]
    fn pinned_collections_have_height_beside_the_outline(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        shell.update(vcx, |shell, cx| {
            let key = shell.active_sidebar_pin_profile_key(cx).unwrap();
            *shell.settings.sidebar_pins_mut(key) = vec!["chat".into()];
            shell.pinned_open = true;
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let bounds = vcx.debug_bounds("overview-shortcut-collections").unwrap();
        assert!(f32::from(bounds.size.height) > 20.0);
        assert!(vcx.debug_bounds("overview-navigation").is_some());
    }

    #[gpui::test]
    fn narrow_overview_overlays_then_restores_the_saved_wide_layout(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        let width = shell.read_with(vcx, |shell, _| shell.settings.sidebar_width);
        vcx.simulate_resize(gpui::size(px(600.0), px(900.0)));
        shell.update(vcx, |shell, cx| shell.open_overview(cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("overview-sidebar-overlay").is_some());
        shell.read_with(vcx, |shell, _| assert_eq!(shell.sidebar_target(), 0.0));
        shell.update(vcx, |shell, cx| shell.open_chat("chat".into(), cx));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("overview-sidebar-overlay").is_none());
        vcx.simulate_resize(gpui::size(px(1200.0), px(900.0)));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("overview-navigation").is_some());
        shell.read_with(vcx, |shell, _| {
            assert_eq!(shell.settings.sidebar_width, width);
            assert!(!shell.settings.sidebar_collapsed);
        });
    }

    #[gpui::test]
    fn overview_search_filters_pins_but_sessions_keeps_its_own_presentation(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        shell.update(vcx, |shell, cx| {
            let key = shell.active_sidebar_pin_profile_key(cx).unwrap();
            *shell.settings.sidebar_pins_mut(key) = vec!["chat".into()];
        });
        page.update(vcx, |page, cx| {
            page.set_search_text_for_test("no matching chat", cx)
        });
        shell.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert_eq!(
            f32::from(
                vcx.debug_bounds("overview-shortcut-collections")
                    .unwrap()
                    .size
                    .height
            ),
            0.0
        );
        shell.update(vcx, |shell, cx| {
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
            shell.settings.sidebar_overview = false;
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        shell.read_with(vcx, |shell, _| {
            assert!(shell.overview_visible_chat_ids.is_none())
        });
        assert!(
            f32::from(
                vcx.debug_bounds("overview-shortcut-collections")
                    .unwrap()
                    .size
                    .height
            ) > 20.0
        );
    }

    #[gpui::test]
    fn sessions_switch_and_open_overview_preserve_the_workspace(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        let button = vcx.debug_bounds("sidebar-sessions").unwrap().center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("overview-navigation").is_none());
        shell.update(vcx, |shell, cx| {
            assert!(!shell.settings.sidebar_overview);
            shell.settings.sidebar_collapsed = true;
            shell.open_overview(cx);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("overview-navigation").is_some());
        shell.read_with(vcx, |shell, cx| {
            assert!(shell.settings.sidebar_overview);
            assert!(!shell.settings.sidebar_collapsed);
            assert_eq!(shell.route, Route::Chat);
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
        });
    }
}
