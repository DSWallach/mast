//! Shared text context menu for rendered content and Mast's custom inputs.
use std::rc::Rc;

use gpui::{
    App, ClipboardItem, IntoElement, MouseButton, Pixels, Point, SharedString, Window, div,
    prelude::*, px,
};

#[derive(Clone)]
pub(crate) struct Entry {
    label: &'static str,
    enabled: bool,
    run: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl Entry {
    pub(crate) fn new(
        label: &'static str,
        enabled: bool,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label,
            enabled,
            run: Rc::new(run),
        }
    }
    pub(crate) fn add_to_session(
        enabled: bool,
        selection: impl Fn() -> Option<String> + 'static,
    ) -> Self {
        Self::new("Add to session", enabled, move |window, cx| {
            if let Some(text) = selection().filter(|text| !text.is_empty()) {
                window.dispatch_action(Box::new(crate::selection_chip::AddToSession(text)), cx);
            }
        })
    }
}

#[derive(IntoElement)]
pub(crate) struct TextMenu {
    id: SharedString,
    child: gpui::AnyElement,
    entries: Vec<Entry>,
    fill: bool,
    on_open: Option<Rc<dyn Fn()>>,
}

impl TextMenu {
    pub(crate) fn new(
        id: impl Into<SharedString>,
        child: impl IntoElement,
        entries: Vec<Entry>,
    ) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
            entries,
            fill: false,
            on_open: None,
        }
    }
    pub(crate) fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

struct MenuState {
    entries: Vec<Entry>,
    position: Option<Point<Pixels>>,
    focus: Vec<gpui::FocusHandle>,
    previous_focus: Option<gpui::FocusHandle>,
    pending_focus: bool,
}

fn close(state: &gpui::Entity<MenuState>, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.position = None;
        state.entries.clear();
        if let Some(focus) = &state.previous_focus {
            window.focus(focus, cx);
        }
        cx.notify();
    });
}

impl RenderOnce for TextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| MenuState {
            entries: Vec::new(),
            position: None,
            focus: self.entries.iter().map(|_| cx.focus_handle()).collect(),
            previous_focus: None,
            pending_focus: false,
        });
        let position = state.read(cx).position;
        let open = state.clone();
        let open_entries = self.entries.clone();
        let on_open = self.on_open;
        #[cfg(all(target_os = "macos", not(test)))]
        let native_entries = self.entries.clone();
        let mut root = div()
            .id(self.id.clone())
            .min_w_0()
            .when(self.fill, |root| root.size_full())
            .child(self.child)
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                if let Some(on_open) = &on_open {
                    on_open();
                }
                #[cfg(all(target_os = "macos", not(test)))]
                if macos::show(native_entries.clone(), event.position, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                let previous_focus = window.focused(cx);
                open.update(cx, |state, cx| {
                    state.entries = open_entries.clone();
                    state.position = Some(event.position);
                    state.previous_focus = previous_focus;
                    state.pending_focus = true;
                    cx.notify();
                });
                cx.stop_propagation();
            });
        if let Some(position) = position {
            let entries = state.read(cx).entries.clone();
            let theme = crate::theme::Theme::of(cx).for_popup();
            let dismiss = state.clone();
            let keyboard = state.clone();
            let mount = state.clone();
            let enabled: Vec<_> = entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| entry.enabled.then_some(index))
                .collect();
            let first = enabled
                .first()
                .copied()
                .or_else(|| (!entries.is_empty()).then_some(0));
            let mut card = crate::popover::popover_card(&theme)
                .id("text-context-actions")
                .w(px(170.0))
                .flex()
                .flex_col()
                .child(
                    gpui::canvas(
                        move |_, window, cx| {
                            mount.update(cx, |state, cx| {
                                if std::mem::take(&mut state.pending_focus) {
                                    if let Some(index) = first {
                                        window.focus(&state.focus[index], cx);
                                    }
                                }
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .w(px(0.0))
                    .h(px(0.0)),
                )
                .on_mouse_down_out(move |_, window, cx| close(&dismiss, window, cx))
                .on_key_down(move |event, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => close(&keyboard, window, cx),
                        "up" | "down" | "tab" if !enabled.is_empty() => {
                            let state = keyboard.read(cx);
                            let current = enabled
                                .iter()
                                .position(|index| state.focus[*index].is_focused(window))
                                .unwrap_or(0);
                            let backwards = event.keystroke.key == "up"
                                || (event.keystroke.key == "tab"
                                    && event.keystroke.modifiers.shift);
                            let next = if backwards {
                                (current + enabled.len() - 1) % enabled.len()
                            } else {
                                (current + 1) % enabled.len()
                            };
                            let focus = state.focus[enabled[next]].clone();
                            window.focus(&focus, cx);
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                });
            for (index, entry) in entries.into_iter().enumerate() {
                let dismiss = state.clone();
                let keyboard = state.clone();
                let run = entry.run.clone();
                card = card.child(
                    crate::popover::menu_row(&theme, false, format!("text-menu-{index}"))
                        .id(entry.label)
                        .debug_selector(move || format!("text-menu-{}", entry.label))
                        .role(gpui::Role::MenuItem)
                        .aria_label(entry.label)
                        .track_focus(&state.read(cx).focus[index])
                        .focus_visible(|style| style.bg(crate::theme::card_selected_bg()))
                        .when(!entry.enabled, |row| row.opacity(0.38).cursor_default())
                        .when(entry.enabled, |row| {
                            row.on_click(move |_, window, cx| {
                                close(&dismiss, window, cx);
                                (entry.run)(window, cx);
                            })
                            .on_key_down(move |event, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    close(&keyboard, window, cx);
                                    run(window, cx);
                                    cx.stop_propagation();
                                }
                            })
                        })
                        .child(entry.label),
                );
            }
            root = root.child(crate::popover::menu_at(
                format!("{}-popup", self.id),
                position,
                card.into_any_element(),
                None,
            ));
        }
        root
    }
}

pub(crate) fn read_only(
    key: impl Into<SharedString>,
    text: SharedString,
    child: impl IntoElement,
) -> TextMenu {
    let key = key.into();
    let has_selection = crate::markdown::selection::wash_range(&key).is_some();
    let selected = Rc::new(std::cell::RefCell::new(None::<String>));
    let snapshot = selected.clone();
    let for_session = selected.clone();
    let select_key = key.clone();
    let mut menu = TextMenu::new(
        format!("text-menu-{key}"),
        child,
        vec![
            Entry::new("Copy", has_selection, move |_, cx| {
                if let Some(text) = &*selected.borrow() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }
            }),
            Entry::add_to_session(has_selection, move || for_session.borrow().clone()),
            Entry::new("Select All", !text.is_empty(), move |window, _| {
                crate::markdown::selection::begin_with_span(&select_key, &text, 0..text.len());
                crate::markdown::selection::end_drag(&select_key);
                window.refresh();
            }),
        ],
    );
    // Snapshot only when opening a menu, rather than joining the whole
    // selection once for every selected paragraph on every drag frame.
    menu.on_open = Some(Rc::new(move || {
        *snapshot.borrow_mut() = crate::markdown::selection::selected_text();
    }));
    menu
}

#[cfg(all(target_os = "macos", not(test)))]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
    use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
    use objc2_foundation::{NSPoint, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::Cell;

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "MastTextMenuTarget"]
        #[ivars = Cell<isize>]
        struct Target;
        impl Target {
            #[unsafe(method(selectTextAction:))]
            fn select(&self, sender: &NSMenuItem) { self.ivars().set(sender.tag()); }
        }
    );

    pub(super) fn show(
        entries: Vec<Entry>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Ok(raw) = HasWindowHandle::window_handle(window) else {
            return false;
        };
        let RawWindowHandle::AppKit(raw) = raw.as_raw() else {
            return false;
        };
        // Keep the view alive through the foreground task and AppKit tracking loop.
        let Some(view) = (unsafe { Retained::retain(raw.ns_view.as_ptr().cast::<NSView>()) })
        else {
            return false;
        };
        let handle = Window::window_handle(window);
        // AppKit runs a nested event loop. Release GPUI's app borrow before entering it.
        cx.spawn(async move |cx| {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let target = Target::alloc().set_ivars(Cell::new(-1));
            let target: Retained<Target> = unsafe { msg_send![super(target), init] };
            let menu = NSMenu::new(mtm);
            menu.setAutoenablesItems(false);
            for (index, entry) in entries.iter().enumerate() {
                let item = NSMenuItem::new(mtm);
                item.setTitle(&NSString::from_str(entry.label));
                item.setEnabled(entry.enabled);
                item.setTag(index as isize);
                unsafe {
                    item.setTarget(Some(&*target as &AnyObject));
                    item.setAction(Some(sel!(selectTextAction:)));
                }
                menu.addItem(&item);
            }
            let location = NSPoint::new(
                f32::from(position.x) as f64,
                if view.isFlipped() {
                    f32::from(position.y) as f64
                } else {
                    view.bounds().size.height - f32::from(position.y) as f64
                },
            );
            menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&view));
            let selected = target.ivars().get();
            let _ = cx.update(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if let Some(entry) = usize::try_from(selected)
                        .ok()
                        .and_then(|index| entries.get(index))
                        .filter(|entry| entry.enabled)
                    {
                        (entry.run)(window, cx);
                    }
                    window.refresh();
                });
            });
        })
        .detach();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Context, Modifiers, MouseDownEvent, MouseUpEvent, Render, TestAppContext, point, size,
    };

    struct Content {
        added: Vec<String>,
    }
    impl Render for Content {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("selection-host")
                .on_action(cx.listener(
                    |this, action: &crate::selection_chip::AddToSession, _, _| {
                        this.added.push(action.0.clone())
                    },
                ))
                .size_full()
                .text_size(px(14.0))
                .line_height(px(22.0))
                .child(div().debug_selector(|| "selectable-content".into()).child(
                    crate::markdown::render::plain_text("hello world", crate::theme::Theme::of(cx)),
                ))
        }
    }

    #[gpui::test]
    fn text_menu_preserves_selection_copies_and_selects_all(cx: &mut TestAppContext) {
        let _selection = crate::markdown::selection::test_state_lock();
        cx.update(|cx| cx.set_global(crate::theme::Theme::dark()));
        let (_, cx) = cx.add_window_view(|_, _| Content { added: Vec::new() });
        cx.simulate_resize(size(px(640.0), px(240.0)));
        cx.run_until_parked();
        let position =
            cx.debug_bounds("selectable-content").unwrap().origin + point(px(5.0), px(9.0));
        cx.simulate_event(MouseDownEvent {
            position,
            button: MouseButton::Left,
            click_count: 2,
            ..Default::default()
        });
        cx.simulate_event(MouseUpEvent {
            position,
            button: MouseButton::Left,
            ..Default::default()
        });
        assert_eq!(
            crate::markdown::selection::selected_text().as_deref(),
            Some("hello")
        );
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            crate::markdown::selection::selected_text().as_deref(),
            Some("hello")
        );
        let copy = cx
            .debug_bounds("text-menu-Copy")
            .expect("copy menu item")
            .center();
        cx.simulate_mouse_down(copy, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(copy, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("hello")
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("text-menu-Copy").is_none());

        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        // Copy is disabled after clicking away from the original selection;
        // keyboard navigation still reaches Select All.
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert_eq!(
            crate::markdown::selection::selected_text().as_deref(),
            Some("hello world")
        );
        assert!(cx.debug_bounds("text-menu-Select All").is_none());

        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("text-menu-Copy").is_none());
    }
    #[gpui::test]
    fn text_menu_adds_selection_snapshot_to_session(cx: &mut TestAppContext) {
        let _selection = crate::markdown::selection::test_state_lock();
        cx.update(|cx| cx.set_global(crate::theme::Theme::dark()));
        let (host, cx) = cx.add_window_view(|_, _| Content { added: Vec::new() });
        cx.run_until_parked();
        let position =
            cx.debug_bounds("selectable-content").unwrap().origin + point(px(5.0), px(9.0));
        cx.simulate_event(MouseDownEvent {
            position,
            button: MouseButton::Left,
            click_count: 3,
            ..Default::default()
        });
        cx.simulate_event(MouseUpEvent {
            position,
            button: MouseButton::Left,
            ..Default::default()
        });
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        let add = cx
            .debug_bounds("text-menu-Add to session")
            .expect("selection action")
            .center();
        cx.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("unrelated clipboard".into()))
        });
        cx.simulate_mouse_down(add, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(add, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        host.read_with(cx, |host, _| assert_eq!(host.added, ["hello world"]));
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("unrelated clipboard")
            )
        });
    }
}
