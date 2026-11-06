//! Offscreen Metal snapshots for test fixtures. No native window is created.
use std::{path::PathBuf, sync::Arc};

use gpui::{App, Entity, HeadlessAppContext, Pixels, Render, Size, Window};

use crate::{
    theme::{Appearance, Theme},
    ui_style::{self, UiStyleId},
};

/// Render a fixture into `$CARGO_TARGET_DIR/snapshots/<name>.png`.
///
/// The builder may install fixture-specific globals and construct the root view.
/// Names are single path components so snapshots cannot escape the output directory.
/// A Metal device and access to macOS services are required.
pub fn render_to_png<V: Render + 'static>(
    name: &str,
    size: Size<Pixels>,
    style: UiStyleId,
    appearance: Appearance,
    build_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "invalid snapshot name"
    );
    let native = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        native.text_system(),
        Arc::new(crate::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        ui_style::init(style, cx);
        Theme::install(appearance, cx);
    });
    let window = cx.open_window(size, build_view)?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    // GPUI animation elements use wall-clock time; settle entrance animations.
    std::thread::sleep(std::time::Duration::from_millis(800));
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    let image = cx.capture_screenshot(window.into())?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let directory = target.join("snapshots");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{name}.png"));
    image.save(&path)?;
    Ok(path)
}
