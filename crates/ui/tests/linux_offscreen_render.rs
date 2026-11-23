//! The Linux offscreen renderer draws real, correctly coloured pixels with no window system.
//! Run through `scripts/ui-preview.sh`'s environment: a Vulkan driver (lavapipe) is enough.
#![cfg(all(target_os = "linux", feature = "snapshot"))]
#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use gpui::{Hsla, Rgba, div, prelude::*, px, rgb, size};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{UiLayout, UiStyleId},
};
use std::{cell::Cell, rc::Rc};

/// Records the theme surface it paints so the test can compare pixels with it.
struct Swatches(Rc<Cell<Option<Hsla>>>);
impl Render for Swatches {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        self.0.set(Some(theme.surface));
        // Narrower than the 360px window: the strip to its right is never painted.
        div()
            .w(px(300.))
            .h_full()
            .bg(theme.surface)
            .text_color(theme.text)
            .flex()
            .gap(px(8.))
            // Pure red catches a swapped blue/red channel order in the BGRA readback.
            .child(div().m(px(10.)).w(px(40.)).h(px(40.)).bg(rgb(0xff0000)))
            .child(div().m(px(10.)).w(px(40.)).h(px(40.)).bg(rgb(0x0000ff)))
            .child(div().m(px(10.)).child("Offscreen text sample"))
    }
}

fn render(name: &str, width: f32, height: f32) -> anyhow::Result<(image::RgbaImage, Hsla)> {
    let surface = Rc::new(Cell::new(None));
    let painted = surface.clone();
    let path = mast_ui::snapshot::render_to_png_with_layout(
        name,
        size(px(width), px(height)),
        UiStyleId::Default,
        UiLayout::Minimal,
        Appearance::Light,
        move |_, cx| cx.new(|_| Swatches(painted)),
    )?;
    let surface = surface.get().expect("the view was never rendered");
    Ok((image::open(path)?.into_rgba8(), surface))
}

fn near(actual: image::Rgba<u8>, expected: [u8; 3]) -> bool {
    (0..3).all(|channel| actual[channel].abs_diff(expected[channel]) <= 1) && actual[3] == 255
}

fn surface_rgb(light: Hsla) -> [u8; 3] {
    let color = Rgba::from(light);
    [color.r, color.g, color.b].map(|channel| (channel * 255.0).round() as u8)
}

#[test]
#[ignore = "requires a Vulkan driver (software lavapipe is sufficient)"]
fn offscreen_render_has_theme_background_channel_order_and_text() -> anyhow::Result<()> {
    let (image, surface) = render("linux-offscreen-swatches", 360., 80.)?;
    // Headless windows render at a 2x scale factor: coordinates below are in logical pixels.
    let scale = image.width() / 360;
    assert_eq!(image.dimensions(), (360 * scale, 80 * scale));

    // The preview must never leave a transparent hole. Nothing paints the strip right of the
    // view, so it pins how this renderer reads back an unpainted pixel: opaque black. That is
    // this renderer's own contract; it is not asserted against the Metal path.
    assert!(image.pixels().all(|pixel| pixel[3] == 255), "found a non-opaque pixel");
    let unpainted = *image.get_pixel(330 * scale, 40 * scale);
    assert_eq!(unpainted.0, [0, 0, 0, 255], "unpainted strip is {unpainted:?}");

    assert_eq!(surface.a, 1.0, "the light theme surface is expected to be opaque");
    let background = *image.get_pixel(300 * scale - 1, 80 * scale - 1);
    let expected = surface_rgb(surface);
    assert!(
        near(background, expected),
        "view corner is {background:?}, theme surface is {expected:?}"
    );

    let red = *image.get_pixel(30 * scale, 30 * scale);
    let blue = *image.get_pixel(88 * scale, 30 * scale);
    assert!(near(red, [255, 0, 0]), "red swatch is {red:?}");
    assert!(near(blue, [0, 0, 255]), "blue swatch is {blue:?}");

    // Text glyphs go through the sprite atlas: the label region has anti-aliased greys.
    let mut distinct = std::collections::HashSet::new();
    for y in 0..80 * scale {
        for x in 120 * scale..300 * scale {
            distinct.insert(image.get_pixel(x, y).0);
        }
    }
    assert!(distinct.len() > 8, "text region has only {} colours", distinct.len());
    Ok(())
}

#[test]
#[ignore = "requires a Vulkan driver (software lavapipe is sufficient)"]
fn offscreen_render_handles_two_sizes_and_repeats_exactly() -> anyhow::Result<()> {
    let (first, _) = render("linux-offscreen-repeat-a", 360., 80.)?;
    let (wide, _) = render("linux-offscreen-repeat-wide", 500., 120.)?;
    let (again, _) = render("linux-offscreen-repeat-b", 360., 80.)?;
    assert_eq!(wide.dimensions(), (first.width() * 500 / 360, first.height() * 120 / 80));
    assert_eq!(first.as_raw(), again.as_raw(), "the same view rendered differently on a second pass");
    Ok(())
}
