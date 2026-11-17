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
pub(super) mod tests {
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
                    .capture_key_down(cx.listener(Shell::on_key_down_capture))
                    .child(div().size_full().flex().child(sidebar).child(main))
                    .children(floating)
                    .children(shell.render_work_outline_modal(window.viewport_size(), window, cx))
            })
        }
    }

    pub(in crate::shell) fn setup(
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
    fn shortcut_button_matches_other_outline_toolbar_controls(cx: &mut TestAppContext) {
        let (_dir, _shell, vcx) = setup(cx);
        let shortcut = vcx.debug_bounds("outline-shortcuts").unwrap();
        let new_item = vcx.debug_bounds("outline-new-item").unwrap();
        assert!((f32::from(shortcut.size.height) - f32::from(new_item.size.height)).abs() < 0.5);
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
    fn filters_include_archived_and_historical_task_runs_and_can_be_cleared(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let mut archived = state.chats[0].clone();
                archived.id = "archived".into();
                archived.title = Some("needle archived".into());
                archived.archived = true;
                let mut historical = state.chats[0].clone();
                historical.id = "historical".into();
                historical.title = Some("needle historical".into());
                historical.scheduled_task = Some("Task".into());
                historical.scheduled_task_info = Some(mast_proto::ScheduledChat {
                    task_id: "task".into(),
                    task_name: "Task".into(),
                    run_id: "old".into(),
                    session_created_at: Some(1),
                    ..Default::default()
                });
                let mut latest = historical.clone();
                latest.id = "latest".into();
                latest.title = Some("Latest run".into());
                latest
                    .scheduled_task_info
                    .as_mut()
                    .unwrap()
                    .session_created_at = Some(2);
                state.chats.extend([archived, historical, latest]);
                cx.notify();
            });
            cx.notify();
        });
        page.update(vcx, |page, cx| {
            page.set_search_text_for_test("needle", cx);
            let ids = page.sidebar_filter_ids(cx).unwrap();
            assert!(ids.contains("archived"));
            assert!(ids.contains("historical"));
            assert!(!ids.contains("latest"));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        shell.update(vcx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            let rows = shell.render_sidebar_collection_rows(&theme, true, cx);
            assert_eq!(
                rows.regular_count, 1,
                "matching historical run remains in collections"
            );
        });
        let clear = vcx.debug_bounds("overview-clear-filters").unwrap().center();
        vcx.simulate_click(clear, gpui::Modifiers::default());
        vcx.run_until_parked();
        page.update(vcx, |page, cx| {
            assert!(page.sidebar_filter_ids(cx).is_none())
        });
    }

    #[gpui::test]
    fn narrow_overlay_starts_closed_and_escape_and_new_chat_dismiss_it(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        vcx.simulate_resize(gpui::size(px(600.0), px(900.0)));
        vcx.run_until_parked();
        shell.read_with(vcx, |shell, _| assert!(shell.overview_overlay_dismissed));
        shell.update(vcx, |shell, cx| shell.open_overview(cx));
        vcx.run_until_parked();
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        page.update(vcx, |page, cx| page.set_search_text_for_test("needle", cx));
        vcx.update(|window, cx| {
            assert!(page.update(cx, |page, cx| page.focus_search(window, cx)));
        });
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        page.update(vcx, |page, cx| {
            assert!(page.sidebar_filter_ids(cx).is_none())
        });
        shell.read_with(vcx, |shell, _| assert!(!shell.overview_overlay_dismissed));
        page.update(vcx, |page, cx| page.request_navigation_focus(cx));
        vcx.run_until_parked();
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        shell.read_with(vcx, |shell, _| assert!(shell.overview_overlay_dismissed));
        shell.update(vcx, |shell, cx| {
            shell.open_overview(cx);
            shell.open_new_session(cx);
            assert!(shell.overview_overlay_dismissed);
            assert!(shell.settings.sidebar_overview);
        });
    }

    #[gpui::test]
    fn overview_collections_keep_custom_membership_without_duplicating_outline_groups(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        shell.update(vcx, |shell, cx| {
            let key = shell.active_sidebar_pin_profile_key(cx).unwrap();
            shell.settings.sidebar_sections_by_profile.insert(key, vec![crate::settings::SidebarSection {
                id: "custom".into(), name: "Custom".into(), collapsed: false, session_ids: vec!["chat".into()],
            }]);
            let mut snapshot = mast_proto::WorkOutlineSnapshot::unavailable("space", true, 1);
            snapshot.available = true;
            snapshot.items = serde_json::from_value(serde_json::json!([
                {"id":"item","spaceId":"space","title":"Item","chatIds":["chat"]}
            ])).unwrap();
            shell.state.update(cx, |state, cx| {
                state.chats[0].space_id = Some("space".into());
                state.spaces = vec![serde_json::from_value(serde_json::json!({
                    "id":"space","deviceId":"local","path":"/workspace","createdAt":"2026-09-20T00:00:00Z"
                })).unwrap()];
                state.set_work_outline_for_test(snapshot);
                cx.notify();
            });
            shell.settings.sidebar_outline_groups = true;
            let custom = shell.custom_sidebar_sections(cx);
            assert_eq!(custom.len(), 1);
            assert_eq!(custom[0].session_ids, vec!["chat"]);
            let theme = Theme::of(cx).clone();
            assert_eq!(shell.render_sidebar_collection_rows(&theme, true, cx).custom_count, 1);
        });
    }

    #[gpui::test]
    fn session_jump_preserves_the_overview_preference(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        shell.update(vcx, |shell, cx| {
            shell.jump_to_session(0, cx);
            assert!(shell.settings.sidebar_overview);
        });
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

    fn seed_assistant(shell: &Entity<Shell>, vcx: &mut VisualTestContext) {
        shell.update(vcx, |shell, cx| shell.state.update(cx, |state, cx| {
            state.spaces = vec![serde_json::from_value(serde_json::json!({
                "id":"assistant-space", "deviceId":"local", "path":"/fixture/assistant", "createdAt":"2026-11-01T00:00:00Z"
            })).unwrap()];
            state.chats.push(serde_json::from_value(serde_json::json!({
                "id":"assistant", "deviceId":"local", "spaceId":"assistant-space", "title":"Daily assistant",
                "createdAt":"2026-11-01T00:00:00Z", "archived":false
            })).unwrap());
            state.personal_assistant.apply(mast_proto::PersonalAssistantReply {
                assistant: Some(mast_proto::PersonalAssistant::for_chat(state.chats.last().unwrap(), 0)),
            });
            cx.notify();
        }));
        redraw_assistant(vcx);
    }

    fn redraw_assistant(vcx: &mut VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }

    #[gpui::test]
    fn needs_interaction_empty_hint_wraps_inside_narrow_overview(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        seed_assistant(&shell, vcx);
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_work_outline_frame(
                    "assistant-space",
                    mast_proto::WorkOutlineSnapshot {
                        space_id: "assistant-space".into(),
                        available: true,
                        synced: true,
                        ..Default::default()
                    },
                );
                cx.notify();
            })
        });
        redraw_assistant(vcx);
        let trigger = vcx.debug_bounds("overview-filter-menu").unwrap().center();
        vcx.simulate_click(trigger, gpui::Modifiers::default());
        redraw_assistant(vcx);
        let filter = vcx
            .debug_bounds("overview-filter-needs-interaction-menu")
            .unwrap()
            .center();
        vcx.simulate_click(filter, gpui::Modifiers::default());
        redraw_assistant(vcx);
        // Dismiss the filter menu so its floating layer cannot mask overflow.
        vcx.simulate_click(
            gpui::point(px(700.0), px(300.0)),
            gpui::Modifiers::default(),
        );
        for width in [240.0, 300.0, 360.0] {
            shell.update(vcx, |shell, cx| {
                shell.settings.sidebar_width = width;
                cx.notify();
            });
            redraw_assistant(vcx);
            let nav = vcx.debug_bounds("overview-navigation").unwrap();
            let copy = vcx
                .debug_bounds("outline-first-use-copy")
                .expect("first-use hint with no matching chats");
            assert!(
                copy.left() >= nav.left() && copy.right() <= nav.right(),
                "hint stays inside {width}px navigation: {copy:?} vs {nav:?}"
            );
            assert!(
                f32::from(copy.size.height) > 20.0,
                "long hint wraps: {copy:?}"
            );
        }
    }

    #[gpui::test]
    fn assistant_and_current_session_live_inside_overview_and_ignore_filters(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        seed_assistant(&shell, vcx);
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        shell.update(vcx, |shell, cx| {
            shell.settings.space_filter = Some("missing-space".into());
            cx.notify();
        });
        page.update(vcx, |page, cx| {
            page.set_search_text_for_test("no matching chats", cx)
        });
        redraw_assistant(vcx);
        let nav = vcx.debug_bounds("overview-navigation").unwrap();
        for selector in ["overview-assistant", "overview-current-session"] {
            let card = vcx.debug_bounds(selector).unwrap();
            assert!(card.left() >= nav.left() && card.right() <= nav.right());
            assert!(card.top() >= nav.top() && card.bottom() <= nav.bottom());
        }
        assert!(vcx.debug_bounds("sidebar-personal-assistant").is_none());
        assert!(vcx.debug_bounds("sidebar-current-session").is_none());
        page.update(vcx, |page, cx| page.open_required_actions(cx));
        redraw_assistant(vcx);
        let card = vcx.debug_bounds("overview-assistant").unwrap().center();
        vcx.simulate_click(card, gpui::Modifiers::default());
        redraw_assistant(vcx);
        shell.read_with(vcx, |shell, cx| {
            assert_eq!(shell.route, Route::Chat);
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("assistant")
            );
        });
        assert!(
            vcx.debug_bounds("overview-current-session").is_none(),
            "same chat gets one card"
        );
        assert!(
            vcx.debug_bounds("overview-chat-panel").is_none(),
            "one live transcript host"
        );
        page.read_with(vcx, |page, _| {
            assert!(page.required_actions_panel().is_none())
        });
    }

    #[gpui::test]
    fn assistant_chooser_filters_candidates_assigns_and_escape_closes(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        seed_assistant(&shell, vcx);
        shell.update(vcx, |shell, cx| {
            shell
                .state
                .update(cx, |state, cx| state.clear_personal_assistant(cx))
        });
        redraw_assistant(vcx);
        assert!(vcx.debug_bounds("overview-assistant").is_none());
        let choose = vcx
            .debug_bounds("overview-choose-assistant")
            .unwrap()
            .center();
        vcx.simulate_click(choose, gpui::Modifiers::default());
        redraw_assistant(vcx);
        assert!(
            vcx.debug_bounds("overview-assistant-choice-assistant")
                .is_some()
        );
        assert!(
            vcx.debug_bounds("overview-assistant-choice-chat").is_none(),
            "projectless chat is ineligible"
        );
        let choice = vcx
            .debug_bounds("overview-assistant-choice-assistant")
            .unwrap()
            .center();
        vcx.simulate_click(choice, gpui::Modifiers::default());
        redraw_assistant(vcx);
        shell.read_with(vcx, |shell, cx| {
            assert!(shell.state.read(cx).is_personal_assistant("assistant"));
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat"),
                "assignment does not open the chat"
            );
        });
        assert!(vcx.debug_bounds("overview-assistant").is_some());
        shell.update(vcx, |shell, cx| {
            shell
                .state
                .update(cx, |state, cx| state.clear_personal_assistant(cx))
        });
        redraw_assistant(vcx);
        let choose = vcx
            .debug_bounds("overview-choose-assistant")
            .unwrap()
            .center();
        vcx.simulate_click(choose, gpui::Modifiers::default());
        redraw_assistant(vcx);
        vcx.simulate_keystrokes("escape");
        redraw_assistant(vcx);
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        page.read_with(vcx, |page, _| assert!(!page.assistant_chooser_open()));
    }

    #[gpui::test]
    fn an_arriving_assistant_closes_an_open_chooser(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        let choose = vcx
            .debug_bounds("overview-choose-assistant")
            .unwrap()
            .center();
        vcx.simulate_click(choose, gpui::Modifiers::default());
        redraw_assistant(vcx);
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        page.read_with(vcx, |page, _| assert!(page.assistant_chooser_open()));
        // This applies a reply directly, bypassing the chooser's assign handler.
        seed_assistant(&shell, vcx);
        page.read_with(vcx, |page, _| assert!(!page.assistant_chooser_open()));
        assert!(vcx.debug_bounds("overview-assistant").is_some());
    }

    #[gpui::test]
    fn fixed_full_chat_link_preserves_drafts_until_discard_then_opens_the_chat(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        seed_assistant(&shell, vcx);
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_work_outline_frame(
                    "assistant-space",
                    crate::work_outline::store::fixture_snapshot("assistant-space"),
                );
                cx.notify();
            })
        });
        let page = shell.read_with(vcx, |shell, _| shell.overview_page.clone().unwrap());
        page.update(vcx, |page, cx| {
            page.open_work_item_details(
                "assistant-space".into(),
                crate::work_outline::store::fixture_ids::API.into(),
                cx,
            )
        });
        redraw_assistant(vcx);
        let details = page.read_with(vcx, |page, _| page.work_item_details_for_test().unwrap());
        vcx.update(|window, cx| {
            details.update(cx, |details, cx| {
                details.begin_title_draft("Unsaved title", window, cx)
            })
        });
        for (button, discard) in [
            ("work-outline-confirm-cancel", false),
            ("work-outline-confirm-accept", true),
        ] {
            redraw_assistant(vcx);
            let full = vcx
                .debug_bounds("overview-assistant-full")
                .unwrap()
                .center();
            vcx.simulate_click(full, gpui::Modifiers::default());
            redraw_assistant(vcx);
            shell.read_with(vcx, |shell, cx| {
                assert_eq!(shell.route, Route::Overview);
                assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
            });
            let decision = vcx.debug_bounds(button).unwrap().center();
            vcx.simulate_click(decision, gpui::Modifiers::default());
            redraw_assistant(vcx);
            if discard {
                shell.read_with(vcx, |shell, cx| {
                    assert_eq!(shell.route, Route::Chat);
                    assert_eq!(
                        shell.state.read(cx).selected_chat.as_deref(),
                        Some("assistant")
                    );
                });
                page.read_with(vcx, |page, _| assert!(!page.has_work_item_details()));
            } else {
                assert!(details.read_with(vcx, |details, cx| details.has_unsaved_draft(cx)));
                shell.read_with(vcx, |shell, cx| {
                    assert_eq!(shell.route, Route::Overview);
                    assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat"));
                });
            }
        }
    }

    #[gpui::test]
    fn scheduled_assistant_tracks_latest_run_and_current_card_survives_without_assistant(
        cx: &mut TestAppContext,
    ) {
        let (_dir, shell, vcx) = setup(cx);
        seed_assistant(&shell, vcx);
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let old = state
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == "assistant")
                    .unwrap();
                old.scheduled_task_info = Some(serde_json::from_value(serde_json::json!({
                "taskId":"briefing", "taskName":"Briefing", "runId":"old", "sessionCreatedAt":1
            })).unwrap());
                let designation = mast_proto::PersonalAssistant::for_chat(old, 0);
                let mut latest = old.clone();
                latest.id = "latest-assistant".into();
                latest
                    .scheduled_task_info
                    .as_mut()
                    .unwrap()
                    .session_created_at = Some(2);
                state.chats.push(latest);
                state
                    .personal_assistant
                    .apply(mast_proto::PersonalAssistantReply {
                        assistant: Some(designation),
                    });
                cx.notify();
            })
        });
        redraw_assistant(vcx);
        let card = vcx.debug_bounds("overview-assistant").unwrap().center();
        vcx.simulate_click(card, gpui::Modifiers::default());
        redraw_assistant(vcx);
        shell.read_with(vcx, |shell, cx| {
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("latest-assistant")
            )
        });
        assert!(vcx.debug_bounds("overview-current-session").is_none());
        shell.update(vcx, |shell, cx| {
            shell
                .state
                .update(cx, |state, cx| state.clear_personal_assistant(cx))
        });
        redraw_assistant(vcx);
        assert!(vcx.debug_bounds("overview-current-session").is_some());
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.selected_chat = None;
                cx.notify();
            })
        });
        redraw_assistant(vcx);
        assert!(vcx.debug_bounds("overview-current-session").is_none());
    }
    #[gpui::test]
    fn sidebar_board_watchers_stay_active_on_chat_routes(cx: &mut TestAppContext) {
        let (_dir, shell, vcx) = setup(cx);
        shell.read_with(vcx, |shell, _| assert_eq!(shell.route, Route::Chat));
        let board = vcx.debug_bounds("outline-mode-Board").unwrap().center();
        vcx.simulate_click(board, gpui::Modifiers::default());
        redraw_assistant(vcx);
        let generation = shell.read_with(vcx, |shell, _| shell.board_links_generation);
        for _ in 0..3 {
            redraw_assistant(vcx);
        }
        shell.read_with(vcx, |shell, _| {
            assert_eq!(shell.route, Route::Chat);
            assert_eq!(
                shell.board_links_generation, generation,
                "a visible sidebar board must keep its watchers between frames"
            );
        });
        shell.update(vcx, |shell, cx| {
            shell.settings.sidebar_collapsed = true;
            cx.notify();
        });
        redraw_assistant(vcx);
        vcx.run_until_parked();
        shell.read_with(vcx, |shell, _| {
            assert!(
                shell.board_links_generation > generation,
                "hiding the sidebar must suspend the board watchers"
            );
        });
    }
}
