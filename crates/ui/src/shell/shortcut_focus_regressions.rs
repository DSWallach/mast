use super::*;
use gpui::{AppContext, TestAppContext};

struct ShortcutHost {
    root: FocusHandle,
    unfocused: FocusHandle,
    editor: FocusHandle,
    show_editor: bool,
    jumps: usize,
}

impl Render for ShortcutHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.root.clone();
        let unfocused = self.unfocused.clone();
        let preferred = self.editor.clone();
        window.defer(cx, move |window, cx| {
            restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
        });
        div()
            .size_full()
            .track_focus(&self.root)
            .child(div().track_focus(&self.unfocused))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // Exercise mouse focus handoffs, hiding a focused pane,
                    // and clicking a control that explicitly clears focus.
                    this.show_editor = event.position.x < px(100.0);
                    if event.position.x < px(200.0) {
                        window.focus(&this.editor, cx);
                    } else {
                        window.blur();
                    }
                    cx.notify();
                }),
            )
            .on_action(cx.listener(|this, _: &JumpSession, _, _| this.jumps += 1))
            .when(self.show_editor, |el| {
                el.child(div().track_focus(&self.editor))
            })
    }
}

#[gpui::test]
fn explicit_blur_does_not_refocus_a_mounted_input(cx: &mut TestAppContext) {
    let host = cx.add_window(|_, cx| ShortcutHost {
        root: cx.focus_handle(),
        unfocused: cx.focus_handle(),
        editor: cx.focus_handle(),
        show_editor: true,
        jumps: 0,
    });
    cx.run_until_parked();
    cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    host.update(cx, |host, window, cx| {
        window.focus(&host.editor, cx);
        window.blur();
        restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
        assert!(host.unfocused.is_focused(window));
        // Subsequent renders must keep the neutral shortcut focus too.
        restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
        assert!(host.unfocused.is_focused(window));
    })
    .unwrap();
}

#[gpui::test]
fn shortcuts_recover_from_retained_editor_focus(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new(
            &platform_combo("mod-2"),
            JumpSession(1),
            None,
        )]);
    });
    let host = cx.add_window(|_, cx| ShortcutHost {
        root: cx.focus_handle(),
        unfocused: cx.focus_handle(),
        editor: cx.focus_handle(),
        show_editor: true,
        jumps: 0,
    });
    for show_editor in [true, false, true, false] {
        host.update(cx, |host, window, cx| {
            host.show_editor = show_editor;
            // Keep the editor handle alive and focused even when hidden.
            window.focus(&host.editor, cx);
            cx.notify();
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        host.update(cx, |host, window, cx| {
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.root.contains_focused(window, cx));
            assert_eq!(host.editor.is_focused(window), show_editor);
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
    }
    host.update(cx, |host, window, cx| {
        assert_eq!(host.jumps, 4);
        window.blur();
        restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
        assert!(host.unfocused.is_focused(window));
    })
    .unwrap();
}

#[gpui::test]
fn shortcuts_work_after_mouse_focus_changes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new(
            &platform_combo("mod-2"),
            JumpSession(1),
            None,
        )]);
    });
    let host = cx.add_window(|_, cx| ShortcutHost {
        root: cx.focus_handle(),
        unfocused: cx.focus_handle(),
        editor: cx.focus_handle(),
        show_editor: true,
        jumps: 0,
    });
    for (index, x) in [50.0, 150.0, 250.0, 50.0, 150.0, 250.0]
        .into_iter()
        .enumerate()
    {
        cx.update_window(host.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    position: gpui::point(px(x), px(20.0)),
                    button: MouseButton::Left,
                    modifiers: gpui::Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        })
        .unwrap();
        // Dispatch immediately after the mouse event; no manual recovery.
        cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
        host.update(cx, |host, window, cx| {
            assert_eq!(
                host.jumps,
                index + 1,
                "shortcut failed after mouse click at {x}"
            );
            assert!(host.root.contains_focused(window, cx));
            assert_eq!(host.editor.is_focused(window), x < 100.0);
        })
        .unwrap();
    }
}
