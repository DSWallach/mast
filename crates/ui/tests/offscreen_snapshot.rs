#![cfg(all(target_os = "macos", feature = "snapshot"))]
use gpui::{div, prelude::*, px, size};
use mast_ui::{theme::{Appearance, Theme}, ui_style::{self, UiStyleId}};
use std::path::Path;

struct MenuRow;
impl Render for MenuRow {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div().size_full().bg(theme.surface).child(
            mast_ui::popover::menu_row(theme, true, "snapshot-row")
                .h(px(theme.ui_style.tokens.sidebar_row_compact_h))
                .child("Work outline")
                .child(ui_style::reveal_on_hover(div().id("action").child("Edit"), "row", theme.ui_style, false)),
        )
    }
}
#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_style_rows_offscreen() -> anyhow::Result<()> {
    let baselines = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&baselines)?;
    for (style_name, style) in [("minimal", UiStyleId::Minimal), ("dense", UiStyleId::Dense), ("guided", UiStyleId::Guided)] {
        for (appearance_name, appearance) in [("dark", Appearance::Dark), ("light", Appearance::Light)] {
            let name = format!("menu-row-{style_name}-{appearance_name}");
            let path = mast_ui::snapshot::render_to_png(&name, size(px(240.), px(60.)), style, appearance, |_, cx| cx.new(|_| MenuRow))?;
            let baseline = baselines.join(format!("{name}.png"));
            if std::env::var_os("MAST_UPDATE_SNAPSHOTS").is_some() { std::fs::copy(&path, &baseline)?; }
            let actual = image::open(&path)?.into_rgba8();
            let expected = image::open(&baseline)?.into_rgba8();
            let diff = image_compare::rgba_hybrid_compare(&expected, &actual)?;
            diff.image.to_color_map().save(path.with_file_name(format!("{name}.diff.png")))?;
            assert!(diff.score >= 0.9999, "{name} similarity: {}", diff.score);
        }
    }
    Ok(())
}
