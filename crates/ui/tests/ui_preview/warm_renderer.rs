//! Reuse a headless window and its Metal renderer across preview cells.
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext, Context, Entity, HeadlessAppContext, IntoElement,
    Pixels, Render, Size, Window, div, prelude::*,
};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{self, UiLayout, UiStyleId},
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, sync::Arc};

struct Root(AnyView);
impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

struct Renderer {
    cx: HeadlessAppContext,
    windows: HashMap<(u32, u32), AnyWindowHandle>,
}

thread_local! { static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) }; }

pub fn render<V: Render + 'static>(
    name: &str,
    size: Size<Pixels>,
    style: UiStyleId,
    layout: UiLayout,
    appearance: Appearance,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<PathBuf> {
    RENDERER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let renderer = slot.get_or_insert_with(|| {
            let native = gpui_platform::current_platform(true);
            let mut cx = HeadlessAppContext::with_platform(
                native.text_system(),
                Arc::new(mast_ui::icons::Assets),
                gpui_platform::current_headless_renderer,
            );
            cx.update(gpui_base::init);
            Renderer {
                cx,
                windows: HashMap::new(),
            }
        });
        renderer.cx.update(|cx| {
            ui_style::init_with_layout(style, layout, cx);
            Theme::install(appearance, cx);
        });
        let key = (f32::from(size.width) as u32, f32::from(size.height) as u32);
        let window = if let Some(window) = renderer.windows.get(&key).copied() {
            renderer.cx.update_window(window, |_, window, cx| {
                let view = build(window, cx).into();
                window.replace_root(cx, |_, _| Root(view));
            })?;
            window
        } else {
            let window = renderer
                .cx
                .open_window(size, |window, cx| {
                    let view = build(window, cx).into();
                    cx.new(|_| Root(view))
                })?
                .into();
            renderer.windows.insert(key, window);
            window
        };
        renderer
            .cx
            .update_window(window, |_, window, cx| window.draw(cx).clear())?;
        std::thread::sleep(std::time::Duration::from_millis(800));
        renderer.cx.run_until_parked();
        renderer
            .cx
            .update_window(window, |_, window, cx| window.draw(cx).clear())?;
        let image = renderer.cx.capture_screenshot(window)?;
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let directory = target.join("snapshots");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{name}.png"));
        image.save(&path)?;
        Ok(path)
    })
}
