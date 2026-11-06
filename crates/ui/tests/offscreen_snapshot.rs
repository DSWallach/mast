#![cfg(target_os = "macos")]
use gpui::{div, prelude::*, px, size, HeadlessAppContext};
use mast_ui::ui_style::{self, UiStyle, UiStyleId};
use std::sync::Arc;

struct StyleRow(UiStyleId);
impl Render for StyleRow {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let style = UiStyle::of(self.0);
        div().size_full().bg(gpui::rgb(0x16181b)).child(
            ui_style::reveal_on_hover(div().id("action").h(px(style.tokens.control_size)).w(px(100.)).bg(gpui::rgb(0x4384a3)).child("Action"), "row", style, false)
        )
    }
}

#[test]
#[ignore = "requires Metal"]
fn render_style_rows_offscreen() {
    let native = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(native.text_system(), Arc::new(()), gpui_platform::current_headless_renderer);
    for (name, style) in [("minimal", UiStyleId::Minimal), ("dense", UiStyleId::Dense)] {
        let window = cx.open_window(size(px(200.), px(60.)), |_, cx| cx.new(|_| StyleRow(style))).unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        let image = cx.capture_screenshot(window.into()).unwrap();
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-clean/snapshots");
        std::fs::create_dir_all(&output).unwrap();
        image.save(output.join(format!("{name}.png"))).unwrap();
    }
}
