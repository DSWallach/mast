use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext};

// Render the production strip and use its real Shell callbacks, without
// starting an engine or rendering the rest of the desktop application.
struct TabHost {
    shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for TabHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            let tabs = shell.render_right_tab_strip(cx);
            shell.titlebar_drag_region(
                "right-tab-test-titlebar",
                div().w(px(400.)).h(px(40.)).child(tabs),
                cx,
            )
        })
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
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
            shell.active_chat = "parent".into();
            for id in ["first", "second"] {
                shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
            }
            shell
        });
        TabHost {
            shell,
            _data_dir: dir,
        }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    cx.update(|window, cx| window.draw(cx).clear());
    (shell, cx)
}

#[gpui::test]
fn subagent_close_press_does_not_start_parent_drag(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let start = cx.debug_bounds("right-surface-close-0").unwrap().center();
    let end = start + gpui::point(px(6.), px(0.));
    cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    cx.update(|_, cx| assert!(!cx.has_active_drag(), "close press started a tab drag"));
    cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    shell.read_with(cx, |shell, cx| {
        assert!(!shell.subagent_tabs.contains_key(&1));
        assert!(shell.subagent_tabs.contains_key(&2));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(2));
    });
}

#[gpui::test]
fn tab_strip_scrolls_over_chips_inside_titlebar(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        for id in ["third", "fourth", "fifth", "sixth"] {
            shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
        }
    });
    cx.update(|window, cx| window.draw(cx).clear());
    let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: start,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(-100.), px(0.))),
                modifiers: gpui::Modifiers::default(),
                touch_phase: gpui::TouchPhase::Moved,
            }),
            cx,
        );
    });
    shell.read_with(cx, |shell, _| {
        assert!(
            shell.right_tab_scroll.offset().x < px(0.),
            "tab strip did not scroll"
        );
    });
}

#[gpui::test]
fn add_button_stays_in_view_when_tabs_overflow(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        for id in ["third", "fourth", "fifth", "sixth"] {
            shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
        }
    });
    cx.update(|window, cx| window.draw(cx).clear());
    let plus = cx
        .debug_bounds("right-surface-add")
        .expect("overflowing strip hid the + button");
    assert!(plus.right() <= px(400.), "+ button fell outside the strip");
}

#[gpui::test]
fn add_button_follows_last_tab_without_overflow(cx: &mut TestAppContext) {
    let (_shell, cx) = setup(cx);
    let last = cx.debug_bounds("right-surface-tab-1").unwrap();
    let plus = cx.debug_bounds("right-surface-add").unwrap();
    assert!(plus.left() >= last.right(), "+ overlaps the last tab");
    assert!(
        plus.left() - last.right() <= px(8.),
        "+ drifted away from the last tab"
    );
}

#[gpui::test]
fn tab_dropped_past_add_button_moves_to_last_slot(cx: &mut TestAppContext) {
    if !crate::click_activation_drag_enabled() {
        return;
    }
    let (shell, cx) = setup(cx);
    let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    let plus = cx.debug_bounds("right-surface-add").unwrap();
    // Chips and the `+` block the outer row's drop hitbox, so a drop
    // lands in the free space beside them.
    let target = gpui::point(plus.right() + px(24.), plus.center().y);
    cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(
        start + gpui::point(px(8.), px(0.)),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| window.draw(cx).clear());
    cx.simulate_mouse_move(target, Some(MouseButton::Left), gpui::Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear());
    cx.simulate_mouse_up(target, MouseButton::Left, gpui::Modifiers::default());
    shell.read_with(cx, |shell, cx| {
        assert_eq!(
            shell
                .right_tabs
                .get(&shell.panel_key(cx))
                .unwrap()
                .as_slice(),
            &[RightSurface::Subagent(2), RightSurface::Subagent(1)]
        );
    });
}

#[gpui::test]
fn add_button_shows_on_item_tab_without_surface_tabs(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        shell.subagent_tabs.clear();
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(
        cx.debug_bounds("right-surface-add").is_none(),
        "the picker already offers every surface"
    );
    shell.update(cx, |shell, cx| {
        shell.set_right_active(RightSurface::Item, cx)
    });
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(
        cx.debug_bounds("right-surface-add").is_some(),
        "Item tab with no other tabs had no + button"
    );
}

#[gpui::test]
fn subagent_tab_body_still_selects_drags_and_middle_closes(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(start, gpui::Modifiers::default());
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(1));
    });
    cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(
        start + gpui::point(px(8.), px(0.)),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.update(|_, cx| {
        assert_eq!(
            cx.has_active_drag(),
            crate::click_activation_drag_enabled(),
            "tab drag policy does not match the current platform"
        )
    });
    cx.simulate_mouse_up(start, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_down(start, MouseButton::Middle, gpui::Modifiers::default());
    cx.simulate_mouse_up(start, MouseButton::Middle, gpui::Modifiers::default());
    shell.read_with(cx, |shell, _| {
        assert!(!shell.subagent_tabs.contains_key(&1));
        assert!(shell.subagent_tabs.contains_key(&2));
    });
}

#[cfg(target_os = "windows")]
#[gpui::test]
fn subagent_tab_click_jitter_selects_without_starting_a_drag(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    let end = start + gpui::point(px(8.), px(0.));

    cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    cx.update(|_, cx| {
        assert!(
            !cx.has_active_drag(),
            "ordinary Windows click jitter started a tab drag"
        )
    });
    cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());

    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(1));
    });
}
