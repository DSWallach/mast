//! B3: a11y tree + bounds enumeration on the headless platform, and a
//! locator-driven click. Needs the zui test-support patch
//! (Pluto8195/zui branch mast/test-support, see MAST-PATCH.md there); runs
//! without a GPU (no renderer).
#![cfg(target_os = "macos")]
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, AppContext, HeadlessAppContext, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PlatformInput, px, size,
};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{self, UiLayout, UiStyleId},
};

fn headless(data: &std::path::Path) -> (HeadlessAppContext, AnyWindowHandle) {
    let native = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        native.text_system(),
        Arc::new(mast_ui::icons::Assets),
        || None,
    );
    cx.set_a11y_enabled(true);
    let dd = data.to_path_buf();
    let window = {
        let dd2 = dd.clone();
        cx.update(move |cx| {
            gpui_base::init(cx);
            // Settle: animations jump to their end state.
            cx.set_reduce_motion(true);
            let mut s = mast_ui::settings::UiSettings::default();
            s.ui_style = UiStyleId::Default;
            s.ui_layout = UiLayout::Minimal;
            s.appearance = mast_ui::appearance::AppearanceMode::Dark;
            mast_ui::settings::init(s.clone(), &dd2, cx);
            let fonts = mast_ui::typography::register_fonts(cx);
            mast_ui::typography::init(
                s.ui_font_family.clone(), s.ui_font_size, s.terminal_font_family.clone(),
                s.terminal_font_size, s.code_font_family.clone(), s.code_font_size, fonts, cx,
            );
            mast_ui::history::init(
                s.git_history_columns, s.git_history_column_widths, s.git_history_column_order,
                s.git_history_author_display, cx,
            );
            mast_ui::composer::init(cx, s.composer_send_behavior);
            mast_ui::terminal::panel::init(cx);
            Theme::install_selection(
                Appearance::Dark, "default",
                mast_theme::AccentSelection::ThemeDefault,
                mast_theme::SurfacePreference::default(), cx,
            );
            let _ = ui_style::UiStyleId::Default;
        });
        cx.open_window(size(px(1200.), px(800.)), move |_, cx| mast_ui::snapshot::chat_fixture(dd, cx))
            .expect("open window")
    };
    (cx, window.into())
}

fn draw(cx: &mut HeadlessAppContext, h: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(h, |_, w, c| w.draw(c).clear()).unwrap();
}

#[test]
fn dump_tree_and_bounds() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();
    let data = tempfile::tempdir().unwrap();
    let (mut cx, h) = headless(data.path());
    draw(&mut cx, h);
    draw(&mut cx, h);
    let (active, tree, bounds) = cx
        .update_window(h, |_, w, _| (w.is_a11y_active(), w.debug_a11y_tree_json(), w.debug_bounds_all()))
        .unwrap();
    assert!(active && tree.is_some() && !bounds.is_empty());
    println!("active={active} tree_len={:?} bounds={}", tree.as_ref().map(|t| t.len()), bounds.len());
    if let Some(t) = &tree { std::fs::write(std::env::temp_dir().join("b3-tree.json"), t).unwrap(); }
    for (k, b) in bounds.iter().take(5) { println!("bounds {k} {b:?}"); }
}

/// Locator = a `debug_selector` name. Click the centre of its bounds with the
/// public `Window::dispatch_event`.
fn click_selector(cx: &mut HeadlessAppContext, h: AnyWindowHandle, selector: &str) {
    let b = cx
        .update_window(h, |_, w, _| w.debug_bounds_all())
        .unwrap()
        .into_iter()
        .find(|(k, _)| k == selector)
        .unwrap_or_else(|| panic!("no element with selector {selector}"))
        .1;
    let p = b.center();
    cx.update_window(h, |_, w, c| {
        let m = Modifiers::default();
        w.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: None, modifiers: m }), c);
        w.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: p, modifiers: m, click_count: 1, first_mouse: false }), c);
        w.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: p, modifiers: m, click_count: 1 }), c);
    })
    .unwrap();
    draw(cx, h);
}

fn selectors(cx: &mut HeadlessAppContext, h: AnyWindowHandle) -> Vec<String> {
    cx.update_window(h, |_, w, _| w.debug_bounds_all()).unwrap().into_iter().map(|(k, _)| k).collect()
}

#[test]
fn click_element_by_locator() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();
    let data = tempfile::tempdir().unwrap();
    let (mut cx, h) = headless(data.path());
    draw(&mut cx, h);
    draw(&mut cx, h);
    let before = selectors(&mut cx, h);
    assert!(before.iter().any(|k| k == "chat-item-identifiers-toggle"));
    assert!(!before.iter().any(|k| k == "chat-item-id-chat"));
    click_selector(&mut cx, h, "chat-item-identifiers-toggle");
    let after = selectors(&mut cx, h);
    assert!(
        after.iter().any(|k| k == "chat-item-id-chat") && after.iter().any(|k| k == "chat-item-id-workspace"),
        "identifiers section did not expand: {after:?}"
    );

    // The tree names the same control: element id, source location, role, label.
    let tree = cx.update_window(h, |_, w, _| w.debug_a11y_tree_json()).unwrap().expect("a11y tree");
    let v: serde_json::Value = serde_json::from_str(&tree).unwrap();
    let copy = v["nodes"].as_object().unwrap().values()
        .find(|n| n["element_id"].as_str() == Some("Name(\"chat-identity-copy\")"))
        .expect("chat-identity-copy node");
    assert_eq!(copy["aria"]["role"], "Button");
    assert!(copy["source_location"].as_str().unwrap().contains("shell_parts.rs"));
}
