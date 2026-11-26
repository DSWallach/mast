//! Compose the captured steps in reading order using the headless renderer for captions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext, Context, HeadlessAppContext, Render, Window, div, prelude::*, px, size};
use image::imageops;
use mast_ui::theme::{Appearance, Theme};

use crate::scenario::StepReport;

const FRAME_WIDTH: u32 = 480;
const FRAME_HEIGHT: u32 = 320;
const CAPTION_HEIGHT: u32 = 54;
const HEADER_HEIGHT: u32 = 36;

struct Board {
    stamp: String,
    captions: Vec<String>,
    columns: usize,
}

impl Render for Board {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .size_full()
            .bg(theme.surface)
            .text_color(theme.text)
            .child(
                div()
                    .h(px(HEADER_HEIGHT as f32))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .text_ellipsis()
                    .child(self.stamp.clone()),
            )
            .children(self.captions.chunks(self.columns).map(|row| {
                div()
                    .flex()
                    .h(px((FRAME_HEIGHT + CAPTION_HEIGHT) as f32))
                    .children(row.iter().map(|caption| {
                        div()
                            .w(px(FRAME_WIDTH as f32))
                            .h(px(CAPTION_HEIGHT as f32))
                            .p(px(8.))
                            .bg(theme.surface_raised)
                            .text_size(px(14.))
                            .overflow_hidden()
                            .child(caption.clone())
                    }))
            }))
    }
}

pub fn render(out: &Path, steps: &mut [StepReport], stamp: &str) -> Result<PathBuf, String> {
    let frames: Vec<_> = steps
        .iter_mut()
        .filter(|step| step.frame.is_some())
        .collect();
    if frames.is_empty() {
        return Err("no frames captured".into());
    }
    let columns = frames.len().min(3);
    let width = FRAME_WIDTH * columns as u32;
    let height =
        HEADER_HEIGHT + (FRAME_HEIGHT + CAPTION_HEIGHT) * frames.len().div_ceil(columns) as u32;
    let captions = frames
        .iter()
        .map(|step| {
            let status = if step.ok { "Pass" } else { "Fail" };
            let id = step.criterion.as_deref().map_or_else(
                || format!("Step {}", step.index),
                |criterion| criterion.to_owned(),
            );
            let caption = step.caption.as_deref().unwrap_or(&step.source);
            format!("{id} · {status}  {caption}")
        })
        .collect();
    let stamp = stamp.to_owned();
    let native = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        native.text_system(),
        Arc::new(mast_ui::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        Theme::install(Appearance::Light, cx);
    });
    let window = cx
        .open_window(size(px(width as f32), px(height as f32)), move |_, cx| {
            cx.new(|_| Board {
                stamp,
                captions,
                columns,
            })
        })
        .map_err(|e| e.to_string())?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .map_err(|e| e.to_string())?;
    let canvas = cx
        .capture_screenshot(window.into())
        .map_err(|e| e.to_string())?;
    let mut canvas = imageops::resize(&canvas, width, height, imageops::FilterType::Lanczos3);
    for (index, step) in frames.into_iter().enumerate() {
        let filename = step.frame.as_ref().expect("frames were filtered");
        let source = image::open(out.join(filename))
            .map_err(|e| e.to_string())?
            .into_rgba8();
        let frame = imageops::resize(
            &source,
            FRAME_WIDTH,
            FRAME_HEIGHT,
            imageops::FilterType::Lanczos3,
        );
        let x = (index % columns) as u32 * FRAME_WIDTH;
        let y = HEADER_HEIGHT
            + (index / columns) as u32 * (FRAME_HEIGHT + CAPTION_HEIGHT)
            + CAPTION_HEIGHT;
        imageops::replace(&mut canvas, &frame, x.into(), y.into());
        step.storyboard_region = Some([
            f64::from(x) / f64::from(width),
            f64::from(y - CAPTION_HEIGHT) / f64::from(height),
            f64::from(FRAME_WIDTH) / f64::from(width),
            f64::from(FRAME_HEIGHT + CAPTION_HEIGHT) / f64::from(height),
        ]);
    }
    let path = out.join("storyboard.png");
    canvas.save(&path).map_err(|e| e.to_string())?;
    Ok(path)
}
