use super::*;
use gpui::{AppContext, TestAppContext};

#[gpui::test]
fn repo_map_new_chat_targets_the_exact_existing_worktree(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, _| {
                state.apply_spaces(vec![mast_proto::Space {
                    id: "repo".into(),
                    device_id: "local".into(),
                    path: "/repo".into(),
                    name: None,
                    git_detected: true,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                }]);
                state.selected_chat = Some("old-chat".into());
            });

            shell.open_new_session_in_worktree(
                "repo".into(),
                "/repo/.worktrees/audit".into(),
                Some("feature/audit".into()),
                cx,
            );

            assert!(matches!(shell.route, Route::Chat));
            assert!(shell.state.read(cx).selected_chat.is_none());
            assert_eq!(shell.state.read(cx).selected_space.as_deref(), Some("repo"));
            let pickers = shell.composer.read(cx).pickers().clone();
            assert_eq!(
                pickers.read(cx).draft_worktree_scope(cx),
                Some(crate::pickers::DraftWorktreeScope {
                    path: "/repo/.worktrees/audit".into(),
                    worktree_name: "audit".into(),
                    repo_name: "repo".into(),
                    branch: "feature/audit".into(),
                }),
                "the blank draft visibly derives the same exact target that send consumes"
            );
            assert_eq!(
                pickers.read(cx).checkout_plan(),
                crate::pickers::CheckoutPlan::ReuseWorktree {
                    path: "/repo/.worktrees/audit".into(),
                    branch: "feature/audit".into(),
                }
            );

            // `+` while already on this unsent draft creates a genuinely
            // fresh tab/draft instead of invisibly carrying the pin.
            shell.open_new_session(cx);
            assert!(pickers.read(cx).draft_worktree_scope(cx).is_none());
        })
        .unwrap();
}

#[gpui::test]
fn appshot_destinations_retain_last_session_and_use_new_canvas_defaults(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, window, cx| {
            shell.state.update(cx, |state, cx| {
                state.spaces = ["a", "b"]
                    .into_iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "deviceId": "local", "path": "/tmp", "gitDetected": false,
                            "createdAt": Utc::now(),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "last", "deviceId": "local", "spaceId": "a", "archived": false,
                        "createdAt": Utc::now(),
                    }))
                    .unwrap(),
                ];
                state.select_chat(Some("last".into()), cx);
            });
            shell.on_state_changed(&shell.state.clone(), cx);
            shell.settings.space_filter = Some("b".into());
            shell.open_new_session(cx);
            shell.on_state_changed(&shell.state.clone(), cx);
            assert!(shell.active_chat.is_empty());
            shell.settings.appshot_destination = crate::appshots::AppshotDestination::LastSession;
            shell.receive_appshot(crate::appshots::tests::shot(), window, cx);
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("last"));
            assert_eq!(shell.composer.read(cx).appshots["last"].len(), 1);
            shell.settings.appshot_destination = crate::appshots::AppshotDestination::NewSession;
            shell.receive_appshot(crate::appshots::tests::shot(), window, cx);
            assert!(shell.state.read(cx).selected_chat.is_none());
            assert_eq!(shell.state.read(cx).selected_space.as_deref(), Some("b"));
            assert_eq!(shell.composer.read(cx).appshots[""].len(), 1);
        })
        .unwrap();
}

#[gpui::test]
fn pane_geometry_uses_one_animation_time_per_frame(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, window, cx| {
            let duration = RESIZE.total().mul_f32(motion::speed_scale());
            let started = std::time::Instant::now() - duration.mul_f32(2.);
            let tween = Some(WidthTween {
                from: 520.,
                to: 0.,
                started,
            });
            shell.reduced_motion = false;
            // The frame started halfway through the transition but rendering
            // crossed its deadline. Every region must still use that frame.
            shell.render_time = Some(started + duration.mul_f32(0.5));
            assert!(shell.tween_active(tween));
            assert_eq!(shell.active_tween_endpoints(tween), Some((520., 0.)));
            let width = shell.eval_tween(tween, 0.);
            assert!(width > 0. && width < 520.);
            assert_eq!(shell.eval_tween(tween, 0.), width);
            shell.active_chat = "preview".into();
            shell.viewport_width = 1000.;
            shell.right_tween = tween;
            shell.toggle_right_pane(cx);
            assert_eq!(
                shell.right_tween.unwrap().from,
                width,
                "reversing must not restart from zero"
            );
            let files = cx.new(|cx| {
                FilesSurface::new(
                    shell.state.clone(),
                    "preview".into(),
                    false,
                    1000,
                    13.0,
                    false,
                    false,
                    cx,
                )
            });
            let key = shell.panel_key(cx);
            shell.file_surfaces.insert(0, files.clone());
            shell
                .right_tabs
                .insert(key.clone(), vec![RightSurface::File(0)]);
            shell
                .panels
                .update(&key, |panel| panel.right_active = RightSurface::File(0));
            assert!(files.read(cx).test_images_visible());
            shell.toggle_right_pane(cx);
            assert!(!shell.right_pane_open(cx));
            assert!(shell.tween_active(shell.right_tween));
            assert!(
                !files.read(cx).test_images_visible(),
                "closing suspends image resources immediately"
            );
            let _ = shell.render_right_pane(window, cx);
            assert!(
                !files.read(cx).test_images_visible(),
                "closing animation must not reactivate images"
            );
            shell.settings.sidebar_collapsed = true;
            shell.sidebar_tween = tween;
            shell.toggle_sidebar(cx);
            assert_eq!(shell.sidebar_tween.unwrap().from, width);
            shell.render_time = None;
            assert!(!shell.tween_active(tween));
            assert_eq!(shell.active_tween_endpoints(tween), None);
            assert_eq!(shell.eval_tween(tween, 0.), 0.);
        })
        .unwrap();
}

#[gpui::test]
fn panel_saves_preserve_settings_selected_outside_the_shell(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    for (index, effect) in settings::NewThreadBackgroundEffect::ALL
        .into_iter()
        .enumerate()
    {
        let open_links_in_zeron = index % 2 == 0;
        let terminal_family = if open_links_in_zeron {
            crate::typography::UiFontFamily::System
        } else {
            crate::typography::UiFontFamily::Geist
        };
        let code_family = if open_links_in_zeron {
            crate::typography::UiFontFamily::Geist
        } else {
            crate::typography::UiFontFamily::System
        };
        let terminal_size = 15.0 + index as f32;
        let code_size = 11.0 + index as f32;
        let transcript_width = 736.0 + 16.0 * index as f32;
        let geometry = Some(settings::WindowGeometry {
            maximized: false,
            display_uuid: Some(uuid::Uuid::from_u128(7)),
            x: 80.0 + index as f32,
            y: 60.0,
            width: 1100.0,
            height: 750.0,
        });
        window
            .update(cx, |shell, _, cx| {
                // Selection changes in Appearance, independently of the shell's
                // cached snapshot. Include a previously queued geometry save.
                shell.settings.sidebar_width = 280.0;
                shell.schedule_save(cx);
                settings::set_new_thread_background_effect(effect, cx);
                settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                    settings.window_geometry = geometry;
                    settings.open_web_links_in_mast = open_links_in_zeron;
                    settings.terminal_font_family = terminal_family.clone();
                    settings.terminal_font_size = terminal_size;
                    settings.code_font_family = code_family.clone();
                    settings.code_font_size = code_size;
                    settings.transcript_width = transcript_width;
                });
                for step in 0..3 {
                    shell.settings.sidebar_width = 290.0 + step as f32;
                    shell.settings.right_pane_width = 540.0 + step as f32;
                    shell.settings.terminal_height = 300.0 + step as f32;
                    shell.schedule_save(cx);
                    let current = settings::current(cx);
                    assert_eq!(current.window_geometry, geometry);
                    assert_eq!(current.new_thread_background_effect, effect);
                    assert_eq!(current.open_web_links_in_mast, open_links_in_zeron);
                    assert_eq!(current.terminal_font_family, terminal_family);
                    assert_eq!(current.terminal_font_size, terminal_size);
                    assert_eq!(current.code_font_family, code_family);
                    assert_eq!(current.code_font_size, code_size);
                    assert_eq!(current.transcript_width, transcript_width);
                }
                settings::flush(cx);
                let loaded = settings::UiSettings::load(dir.path());
                assert_eq!(loaded.window_geometry, geometry);
                assert_eq!(loaded.new_thread_background_effect, effect);
                assert_eq!(loaded.open_web_links_in_mast, open_links_in_zeron);
                assert_eq!(loaded.terminal_font_family, terminal_family);
                assert_eq!(loaded.terminal_font_size, terminal_size);
                assert_eq!(loaded.code_font_family, code_family);
                assert_eq!(loaded.code_font_size, code_size);
                assert_eq!(loaded.transcript_width, transcript_width);
                assert_eq!(loaded.sidebar_width, 292.0);
                assert_eq!(loaded.right_pane_width, 542.0);
                assert_eq!(loaded.terminal_height, 302.0);
            })
            .unwrap();
    }
}

#[gpui::test]
fn opening_terminals_focuses_the_terminal_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    let mut drawer_window = None;
    for embedded in [false, false, true] {
        let panel = window
            .update(cx, |shell, window, cx| {
                shell.open_chat("terminal-session".into(), cx);
                shell.active_chat = "terminal-session".into();
                let panel = if embedded {
                    shell.add_terminal_surface(cx);
                    shell.right_terminal.clone().unwrap()
                } else {
                    shell.toggle_terminal(window, cx);
                    shell.terminal.clone().unwrap()
                };
                assert!(!shell.composer.read(cx).focus_pending);
                panel
            })
            .unwrap();
        let terminal_window = if !embedded && drawer_window.is_some() {
            drawer_window.unwrap()
        } else {
            let handle = cx.update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_, _| panel.clone())
                    .unwrap()
            });
            if !embedded {
                drawer_window = Some(handle);
            }
            handle
        };
        cx.update_window(terminal_window.into(), |_, window, cx| {
            window.draw(cx).clear();
            assert!(
                panel.read(cx).focus_handle().is_focused(window),
                "embedded: {embedded}"
            );
            // Ordinary redraws must not steal focus from another control.
            let other_input = cx.focus_handle();
            window.focus(&other_input, cx);
            window.draw(cx).clear();
            assert!(other_input.is_focused(window));
        })
        .unwrap();
        if !embedded {
            window
                .update(cx, |shell, window, cx| {
                    shell.toggle_terminal(window, cx);
                    assert!(shell.composer.focus_handle(cx).is_focused(window));
                })
                .unwrap();
        }
    }
}

#[gpui::test]
fn session_navigation_focuses_composer_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    // Render the real composer in its own window to exercise mounting
    // without the shell's boot/connection gate hiding the destination.
    let composer_window = cx.update(|cx| {
        let composer = window.read(cx).unwrap().composer.clone();
        cx.open_window(gpui::WindowOptions::default(), |_, _| composer)
            .unwrap()
    });
    for destination in ["initial", "chat", "chat", "new", "new", "back", "settings"] {
        window
            .update(cx, |shell, _, cx| match destination {
                "chat" => shell.open_chat("existing-session".into(), cx),
                "new" => shell.open_new_session(cx),
                "back" => shell.apply_nav(NavEntry::Chat("existing-session".into()), cx),
                "settings" => {
                    shell.open_settings(SettingsSection::Devices, cx);
                    shell.close_settings(cx);
                }
                _ => {}
            })
            .unwrap();
        cx.update_window(composer_window.into(), |composer, window, cx| {
            window.draw(cx).clear();
            assert!(
                composer
                    .downcast::<Composer>()
                    .unwrap()
                    .focus_handle(cx)
                    .is_focused(window),
                "{destination}"
            );
            // Subsequent renders after a click-away must not reclaim it.
            window.blur();
            window.draw(cx).clear();
            assert!(window.focused(cx).is_none(), "{destination}");
        })
        .unwrap();
    }
}

/// The overview's chat panel selects a chat without leaving the route or
/// recording history; "Open full chat →" on that already-selected chat
/// still records the chat entry, so Back returns to the overview.
#[gpui::test]
fn copy_chat_identity_writes_clipboard_and_flashes_header_feedback(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, cx| {
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "c1", "deviceId": "local", "archived": false,
                        "title": "Fix\nlogin", "harnessSessionId": "sess-9",
                        "createdAt": Utc::now(),
                    }))
                    .unwrap(),
                    serde_json::from_value(serde_json::json!({
                        "id": "c2", "deviceId": "local", "archived": false,
                        "createdAt": Utc::now(),
                    }))
                    .unwrap(),
                ];
                cx.notify();
            });
            // Header button: clipboard + transient check, no sidebar notice.
            shell.copy_chat_identity("c1", false, cx);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("Fix login \u{2014} chat c1 \u{2014} session sess-9")
            );
            assert_eq!(shell.copied_chat_identity.as_deref(), Some("c1"));
            assert!(shell.sidebar_notice.is_none());
            // Menu item: no session id -> the session part is omitted.
            shell.copy_chat_identity("c2", true, cx);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("New session \u{2014} chat c2")
            );
            assert_eq!(
                shell.sidebar_notice.as_deref(),
                Some("Chat name + IDs copied")
            );
        })
        .unwrap();
    // The header check clears itself after the flash.
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1300));
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert!(shell.copied_chat_identity.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn overview_chat_preview_keeps_route_and_history(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, _, cx| {
            shell.open_overview(cx);
            shell.show_overview_workspace(cx);
            shell.preview_chat_in_overview("existing-session".into(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert!(matches!(shell.route, Route::Overview));
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("existing-session")
            );
            assert_eq!(shell.active_chat, "existing-session");
            assert_eq!(*shell.nav.current(), NavEntry::Overview);
            shell.open_chat("existing-session".into(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(
                *shell.nav.current(),
                NavEntry::Chat("existing-session".into())
            );
            shell.navigate_back(cx);
            assert!(matches!(shell.route, Route::Overview));
        })
        .unwrap();
}

#[gpui::test]
fn projectless_new_session_restores_opt_out_and_clears_sidebar_filter(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    crate::settings::composer::ComposerDefaults {
        device: Some("remote".into()),
        no_project: true,
        ..Default::default()
    }
    .save(dir.path())
    .unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, _| {
                state.apply_spaces(vec![mast_proto::Space {
                    id: "repo".into(),
                    device_id: "local".into(),
                    path: "/repo".into(),
                    name: None,
                    git_detected: false,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                }]);
                // Boot opened an existing project session after loading defaults.
                state.selected_chat = Some("existing-project-chat".into());
                state.no_project = false;
            });
            shell.settings.space_filter = None;
            shell.open_new_session(cx);
            assert!(shell.state.read(cx).no_project);
            assert!(shell.state.read(cx).selected_space.is_none());
            assert_eq!(
                shell.state.read(cx).effective_device_id().as_deref(),
                Some("remote")
            );

            // An explicit filter may select its project, but opting out again
            // must remove that filter before the first send can hide the chat.
            shell.set_space_filter(Some("repo".into()), cx);
            shell
                .state
                .update(cx, |state, cx| state.select_space(None, cx));
            shell.on_state_changed(&shell.state.clone(), cx);
            assert!(shell.settings.space_filter.is_none());
            assert!(shell.state.read(cx).no_project);
        })
        .unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui::test]
fn session_chips_open_chats_and_reject_invalid_targets(cx: &mut TestAppContext) {
    use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "source".into();
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("source".into());
                state.chats.push(mast_proto::Chat {
                    standing_role_successor: None,
                    hand_off_from: None,
                    id: "destination".into(),
                    scheduled_task_info: None,
                    scheduled_task: None,
                    side_of: None,
                    followup_of: None,
                    side_absorbed_through: None,
                    device_id: "here".into(),
                    title: None,
                    archived: false,
                    cwd: None,
                    branch: None,
                    checkout_id: None,
                    source_context: None,
                    config: None,
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: Utc::now(),
                    harness_session_id: None,
                    harness_session_cwd: None,
                    actual_model: None,
                    parent_chat_id: None,
                    space_id: None,
                    last_seen_at: None,
                    room_gen: None,
                    linked_pr_url: None,
                    linked_pr_source: None,
                    linked_ticket_id: None,
                    linked_ticket_source: None,
                });
            });
            settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                settings.open_web_links_in_mast = false;
            });
            let mut activation = LinkActivation {
                target: LinkTarget::new("Session", "mast://chat/destination"),
                action: LinkAction::Primary,
                source_session: Some("stale".into()),
            };
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.source_session = Some("source".into());
            activation.action = LinkAction::External;
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.action = LinkAction::Primary;
            activation.target = LinkTarget::new("Missing", "mast://chat/missing");
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.target = LinkTarget::new(
                "Other workspace",
                "mast://open/chat/destination?workspace=other",
            );
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.target = LinkTarget::new("Session", "mast://chat/destination");
            let browsers = shell.browsers.len();
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Internal
            );
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("destination")
            );
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.browsers.len(), browsers);
        })
        .unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui::test]
fn transcript_links_open_new_tabs_and_reject_stale_sessions(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    let weak = window
        .update(cx, |shell, window, cx| {
            use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
            shell.active_chat = "first-session".into();
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("first-session".into())
            });
            let mut activation = LinkActivation {
                target: LinkTarget::new("Docs", "https://example.com/docs"),
                action: LinkAction::Primary,
                source_session: Some("first-session".into()),
            };
            assert!(!shell.right_pane_open(cx));
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Internal
            );
            assert!(shell.right_pane_open(cx));
            let first = shell.browser_seq;
            assert_eq!(
                shell.browsers[&first].read(cx).page.url.as_deref(),
                Some("https://example.com/docs")
            );
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(first)
            );
            // The same page again focuses its tab; a page's own new-tab
            // request (target=_blank) still opens another.
            shell.activate_session_link(&activation, window, cx);
            assert_eq!(shell.browsers.len(), 1);
            shell.add_browser_surface(Some("https://example.com/docs".into()), window, cx);
            assert_eq!(shell.browsers.len(), 2);
            settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                settings.open_web_links_in_mast = false;
            });
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::External("https://example.com/docs".into())
            );
            assert_eq!(shell.browsers.len(), 2);
            activation.action = LinkAction::Internal;
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Internal,
                "the explicit internal action ignores the default preference"
            );
            assert_eq!(shell.browsers.len(), 2, "and focuses the open tab");
            activation.action = LinkAction::External;
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::External("https://example.com/docs".into())
            );
            assert_eq!(shell.browsers.len(), 2);
            activation.source_session = Some("other-session".into());
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            activation.source_session = Some("first-session".into());
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("switch-in-progress".into())
            });
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Rejected
            );
            assert_eq!(shell.browsers.len(), 2);
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some("first-session".into())
            });
            shell.add_subagent_surface(
                "first-session".into(),
                "child-doc".into(),
                "Child".into(),
                true,
                cx,
            );
            let child = shell
                .subagent_tabs
                .values()
                .next()
                .unwrap()
                .transcript
                .clone();
            let ui = child.read(cx).link_ui().unwrap();
            assert_eq!(ui.source_session.as_deref(), Some("first-session"));
            activation.action = LinkAction::Internal;
            activation.source_session = ui.source_session;
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Internal
            );
            assert_eq!(shell.browsers.len(), 2);
            let weak = shell.browsers[&first].downgrade();
            shell.close_right_surface(RightSurface::Browser(first), window, cx);
            weak
        })
        .unwrap();
    cx.run_until_parked();
    assert!(weak.upgrade().is_none());
}

#[gpui::test]
fn review_file_link_uses_bound_checkout_root(cx: &mut TestAppContext) {
    use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let root_text = root.to_string_lossy().into_owned();
    let bound = root.join(".worktrees/ui-pr-5767");
    std::fs::create_dir_all(bound.join(".mast/reviews")).unwrap();
    let payload = bound.join(".mast/reviews/pr-5767.review.json");
    std::fs::write(&payload, "{}").unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "viz".into();
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("here".into());
                state.selected_chat = Some("viz".into());
                state.chats.push(mast_proto::Chat {
                    standing_role_successor: None,
                    hand_off_from: None,
                    id: "viz".into(),
                    device_id: "here".into(),
                    title: None,
                    archived: false,
                    cwd: Some(root_text.clone()),
                    branch: None,
                    checkout_id: None,
                    source_context: None,
                    config: None,
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: Utc::now(),
                    harness_session_id: None,
                    harness_session_cwd: None,
                    actual_model: None,
                    parent_chat_id: None,
                    space_id: None,
                    last_seen_at: None,
                    room_gen: None,
                    linked_pr_url: None,
                    linked_pr_source: None,
                    linked_ticket_id: None,
                    linked_ticket_source: None,
                    scheduled_task: None,
                    scheduled_task_info: None,
                    side_of: None,
                    followup_of: None,
                    side_absorbed_through: None,
                });
                state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView {
                    chat_id: "viz".into(),
                    targets: vec![mast_proto::ChatDiffTargetView {
                        path: bound.to_string_lossy().into_owned(),
                        checkout_id: "ui-wt".into(),
                        source: mast_proto::ChatDiffTargetSource::Agent,
                        selected: true,
                        repo_name: "ui".into(),
                        branch: None,
                        head_sha: None,
                    }],
                    ref_targets: vec![],
                }]);
            });
            let activation = LinkActivation {
                target: LinkTarget::new("Saved review", payload.to_str().unwrap()),
                action: LinkAction::Primary,
                source_session: Some("viz".into()),
            };
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                LinkOutcome::Internal
            );
            assert_eq!(shell.file_surface_paths.len(), 1);
            assert_eq!(
                shell.file_surface_paths.values().next().unwrap(),
                ".mast/reviews/pr-5767.review.json"
            );
            assert_eq!(
                shell.active_checkout(cx).unwrap().0,
                bound.to_string_lossy()
            );
            let context =
                crate::files::client::FilesRequestContext::for_chat(shell.state.read(cx), "viz")
                    .unwrap();
            assert_eq!(context.target.checkout_path.as_deref(), bound.to_str());
            assert_eq!(
                shell.state.read(cx).chats[0].cwd.as_deref(),
                Some(root_text.as_str())
            );
        })
        .unwrap();
}

#[gpui::test]
fn visualization_links_render_fragments_and_open_previews_externally(cx: &mut TestAppContext) {
    use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
    let dir = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::write(root.join("work/mock.fragment.html"), "<div id=m>x</div>").unwrap();
    std::fs::write(root.join("work/mock.preview.html"), "<!doctype html>").unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    let root_text = root.to_string_lossy().into_owned();
    let preview_url = crate::browser::file_url(&root.join("work/mock.preview.html")).unwrap();
    let link_ui = window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "viz".into();
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("here".into());
                state.selected_chat = Some("viz".into());
                state.chats.push(mast_proto::Chat {
                    standing_role_successor: None,
                    hand_off_from: None,
                    id: "viz".into(),
                    device_id: "here".into(),
                    title: None,
                    archived: false,
                    cwd: Some(root_text.clone()),
                    branch: None,
                    checkout_id: None,
                    source_context: None,
                    config: None,
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: Utc::now(),
                    harness_session_id: None,
                    harness_session_cwd: None,
                    actual_model: None,
                    scheduled_task: None,
                    scheduled_task_info: None,
                    parent_chat_id: None,
                    side_of: None,
                    followup_of: None,
                    side_absorbed_through: None,
                    space_id: None,
                    last_seen_at: None,
                    room_gen: None,
                    linked_pr_url: None,
                    linked_pr_source: None,
                    linked_ticket_id: None,
                    linked_ticket_source: None,
                });
            });
            let activate = |shell: &mut Shell,
                            original: &str,
                            action: LinkAction,
                            window: &mut Window,
                            cx: &mut Context<Shell>| {
                shell.activate_session_link(
                    &LinkActivation {
                        target: LinkTarget::new("Mockup", original),
                        action,
                        source_session: Some("viz".into()),
                    },
                    window,
                    cx,
                )
            };
            let fragments = |shell: &Shell, cx: &App| {
                shell
                    .browsers
                    .values()
                    .filter_map(|browser| {
                        let view = browser.read(cx).fragment()?;
                        Some((view.source.clone(), view.notice.clone()))
                    })
                    .collect::<Vec<_>>()
            };

            // A fragment link renders instead of opening its source.
            assert_eq!(
                activate(
                    shell,
                    "work/mock.fragment.html",
                    LinkAction::Primary,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            let shown = fragments(shell, cx);
            assert_eq!(shown.len(), 1);
            assert_eq!(shown[0].0.path, "work/mock.fragment.html");
            assert_eq!(shown[0].0.title, "Mockup");
            assert_eq!(shown[0].0.local_root.as_deref(), Some(root.as_path()));
            assert!(shell.file_surface_keys.is_empty());
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(shell.browser_seq)
            );
            let tab = shell.browsers[&shell.browser_seq].clone();
            assert_eq!(tab.read(cx).title().as_ref(), "Mockup");
            // No engine in this test: the read fails with an actionable error.
            let error = tab.read(cx).fragment().unwrap().error.clone().unwrap();
            assert!(error.message().contains("try again"), "{}", error.message());

            // The absolute form reuses (and reloads) the same tab.
            let absolute = format!("{root_text}/work/mock.fragment.html");
            activate(shell, &absolute, LinkAction::Primary, window, cx);
            assert_eq!(fragments(shell, cx).len(), 1);

            // The export opens in the system browser from either file.
            assert_eq!(
                activate(
                    shell,
                    "work/mock.preview.html",
                    LinkAction::Primary,
                    window,
                    cx
                ),
                LinkOutcome::External(preview_url.clone())
            );
            assert_eq!(
                activate(
                    shell,
                    "work/mock.fragment.html",
                    LinkAction::External,
                    window,
                    cx
                ),
                LinkOutcome::External(preview_url.clone())
            );
            // "Open in Mast" on the export shows its fragment.
            assert_eq!(
                activate(
                    shell,
                    "work/mock.preview.html",
                    LinkAction::Internal,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            assert_eq!(fragments(shell, cx).len(), 1);

            // View source opens the editor, not a rendered view.
            assert_eq!(
                activate(
                    shell,
                    "work/mock.fragment.html",
                    LinkAction::Source,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            assert!(
                shell
                    .file_surface_keys
                    .keys()
                    .any(|(_, path)| path == "work/mock.fragment.html")
            );

            // A plain `.html` visualize reference renders wide on request.
            assert_eq!(
                activate(
                    shell,
                    "work/chart.html",
                    LinkAction::Visualize { wide: true },
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            assert!(
                fragments(shell, cx)
                    .iter()
                    .any(|(source, _)| source.path == "work/chart.html" && source.wide)
            );
            // An ordinary `.html` link still opens as a file.
            let tabs = fragments(shell, cx).len();
            activate(shell, "work/page.html", LinkAction::Primary, window, cx);
            assert_eq!(fragments(shell, cx).len(), tabs);

            // Outside the checkout nothing opens as a file: the owning
            // device is offered the artifact import instead.
            let tabs = fragments(shell, cx).len();
            assert_eq!(
                activate(
                    shell,
                    "/etc/x.fragment.html",
                    LinkAction::Primary,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            assert_eq!(fragments(shell, cx).len(), tabs);
            assert!(shell.outside_file_prompt.is_some());
            shell.outside_file_prompt = None;

            // Without an export, the preview action explains what to do.
            std::fs::remove_file(root.join("work/mock.preview.html")).unwrap();
            assert_eq!(
                activate(
                    shell,
                    "work/mock.fragment.html",
                    LinkAction::External,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            let notice = fragments(shell, cx)
                .into_iter()
                .find(|(source, _)| source.path == "work/mock.fragment.html")
                .and_then(|(_, notice)| notice)
                .unwrap();
            assert!(notice.contains("No standalone export"), "{notice}");

            // A chat on another device can't open local files externally.
            shell.state.update(cx, |state, _| {
                state.local_device_id = Some("elsewhere".into())
            });
            assert_eq!(
                activate(
                    shell,
                    "work/mock.preview.html",
                    LinkAction::Primary,
                    window,
                    cx
                ),
                LinkOutcome::Internal
            );
            assert_eq!(
                activate(
                    shell,
                    "work/mock.fragment.html",
                    LinkAction::Reveal,
                    window,
                    cx
                ),
                LinkOutcome::Rejected
            );
            shell
                .state
                .update(cx, |state, _| state.local_device_id = Some("here".into()));
            std::fs::write(root.join("work/mock.preview.html"), "<!doctype html>").unwrap();
            Shell::session_links(Some("viz".into()), cx)
        })
        .unwrap();

    // Transcript cards route through the same handler: their preview
    // action reaches the system opener with the export's file URL.
    cx.update_window(window.into(), |_, window, cx| {
        crate::markdown::render::activate_link(
            LinkTarget::new("Mockup", "work/mock.fragment.html"),
            LinkAction::External,
            Some(&link_ui),
            window,
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.opened_url(), Some(preview_url));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[gpui::test]
fn markdown_preview_events_open_browser_from_tree_and_file_tabs(cx: &mut TestAppContext) {
    use crate::markdown::render::{LinkAction, LinkActivation, LinkTarget};
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    let surfaces = window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "owner".into();
            shell
                .state
                .update(cx, |state, _| state.selected_chat = Some("owner".into()));
            shell.add_files_surface(window, cx);
            shell.add_file_surface("README.md".into(), window, cx);
            [
                shell.files[&shell.panel_key(cx)].clone(),
                shell.file_surfaces[&shell.file_surface_seq].clone(),
            ]
        })
        .unwrap();
    let mut last_external_url = None;
    for (index, surface) in surfaces.iter().enumerate() {
        let mut activation = LinkActivation {
            target: LinkTarget::new("Docs", &format!("https://example.com/preview/{index}")),
            action: LinkAction::Primary,
            source_session: Some("owner".into()),
        };
        surface.update(cx, |_, cx| {
            cx.emit(FilesEvent::OpenWebLink(activation.clone()))
        });
        cx.run_until_parked();
        assert_eq!(cx.opened_url(), last_external_url);
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.right_pane_open(cx));
                assert_eq!(shell.browsers.len(), index + 1);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(shell.browser_seq)
                );
                assert_eq!(
                    shell.browsers[&shell.browser_seq]
                        .read(cx)
                        .page
                        .url
                        .as_deref(),
                    Some(activation.target.original.as_str())
                );
            })
            .unwrap();
        activation.action = LinkAction::External;
        surface.update(cx, |_, cx| {
            cx.emit(FilesEvent::OpenWebLink(activation.clone()))
        });
        cx.run_until_parked();
        assert_eq!(
            cx.opened_url().as_deref(),
            Some(activation.target.original.as_str())
        );
        last_external_url = Some(activation.target.original.clone());
        activation.target = LinkTarget::new("Stale", "https://example.com/stale");
        activation.source_session = Some("stale-owner".into());
        for action in [LinkAction::Internal, LinkAction::External] {
            activation.action = action;
            surface.update(cx, |_, cx| {
                cx.emit(FilesEvent::OpenWebLink(activation.clone()))
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url(), last_external_url);
        }
        window
            .update(cx, |shell, _, _| {
                assert_eq!(shell.browsers.len(), index + 1)
            })
            .unwrap();
    }
}

#[gpui::test]
fn browser_tabs_keep_session_ownership_and_release_on_close(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    let weak = window
        .update(cx, |shell, window, cx| {
            // A fresh-session canvas never accumulates ownerless tabs.
            shell.add_browser_surface(None, window, cx);
            assert!(shell.browsers.is_empty());
            shell.active_chat = "first-session".into();
            shell.add_browser_surface(None, window, cx);
            let first = shell.browser_seq;
            let weak = shell.browsers[&first].downgrade();
            shell.add_browser_surface(None, window, cx);
            let second = shell.browser_seq;
            assert_ne!(first, second);
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(second)
            );
            shell.active_chat = "second-session".into();
            assert!(shell.right_surface_rows(cx).is_empty());
            shell.add_browser_surface(None, window, cx);
            let other = shell.browser_seq;
            shell.active_chat = "first-session".into();
            assert_eq!(shell.right_surface_rows(cx).len(), 2);
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(second)
            );
            // Closing a background tab preserves the selected address input.
            let focus = window.focused(cx);
            shell.close_right_surface(RightSurface::Browser(first), window, cx);
            assert_eq!(window.focused(cx), focus);
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(second)
            );
            shell.close_right_surface(RightSurface::Browser(second), window, cx);
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Picker);
            shell.active_chat = "second-session".into();
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(other)
            );
            shell.close_right_surface(RightSurface::Browser(other), window, cx);
            assert!(shell.browsers.is_empty());
            assert!(shell.browser_subs.is_empty());
            weak
        })
        .unwrap();
    cx.run_until_parked();
    assert!(
        weak.upgrade().is_none(),
        "closed browser retained by callbacks"
    );
}

#[gpui::test]
fn cmd_w_closes_the_active_right_pane_surface_before_the_window(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "session".into();
            shell.toggle_right_pane(cx);
            assert!(shell.right_pane_open(cx));

            shell.add_browser_surface(None, window, cx);
            let first = shell.browser_seq;
            shell.add_browser_surface(None, window, cx);
            let second = shell.browser_seq;
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(second)
            );

            // ⌘W closes the active tab, not the window.
            assert!(shell.close_active_surface(window, cx));
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(first)
            );

            // …and again for the last tab, which also collapses the
            // surface host (nothing left to show).
            assert!(shell.close_active_surface(window, cx));
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Picker);
            assert!(!shell.right_pane_open(cx));

            // A closed pane falls through to the window-close rung
            // instead of being consumed.
            assert!(!shell.close_active_surface(window, cx));

            // So does an open pane with nothing left to close.
            shell.toggle_right_pane(cx);
            assert!(shell.right_pane_open(cx));
            assert!(!shell.close_active_surface(window, cx));
        })
        .unwrap();
}

#[gpui::test]
fn lifecycle_actions_keep_pending_and_failed_file_saves_alive(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
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
        )
    });
    for failed in [false, true] {
        window
            .update(cx, |shell, window, cx| {
                window.activate_window();
                let state = shell.state.clone();
                let files = cx.new(|cx| {
                    let mut files = FilesSurface::new(
                        state,
                        "test".into(),
                        false,
                        1000,
                        13.0,
                        false,
                        false,
                        cx,
                    );
                    files.seed_pending_exit_test_document(failed);
                    files
                });
                shell.file_surfaces.insert(0, files);
                shell
                    .file_surface_keys
                    .insert(("test".into(), "test.rs".into()), 0);
            })
            .unwrap();
        cx.update(|cx| cx.dispatch_action(&crate::app_menus::Quit));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(matches!(shell.pending_exit, Some(PendingExit::Quit)));
                assert!(!shell.all_file_edits_flushed(cx));
                shell.cancel_file_close(RightSurface::File(0), cx);
                assert!(shell.pending_exit.is_none());
            })
            .unwrap();
        cx.update(|cx| cx.dispatch_action(&crate::app_menus::CloseWindow));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(matches!(shell.pending_exit, Some(PendingExit::CloseWindow)));
                shell.quit_for_runtime_change(cx);
                assert!(matches!(
                    shell.pending_exit,
                    Some(PendingExit::RuntimeChange)
                ));
                assert!(shell.runtime_change_task.is_none());
                shell.apply_staged_update(PathBuf::from("must-not-install"), cx);
                assert!(matches!(
                    shell.pending_exit,
                    Some(PendingExit::InstallUpdate(_))
                ));
                assert!(matches!(shell.update_flow, UpdateFlow::Idle));
                shell.cancel_file_close(RightSurface::File(0), cx);
                assert!(shell.pending_exit.is_none());
            })
            .unwrap();
    }
}
