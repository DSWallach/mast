//! Presentation for code reference actions.
use crate::theme::Theme;
use gpui::{div, prelude::*, px};

/// Keep the selection actions within the editor viewport, above the selection
/// when there is room and below it otherwise.
pub(crate) fn selection_action_position(
    selection: gpui::Bounds<gpui::Pixels>,
    viewport: gpui::Bounds<gpui::Pixels>,
) -> (gpui::Pixels, gpui::Pixels) {
    let width = 160.0;
    let height = 32.0;
    let x = f32::from(selection.origin.x - viewport.origin.x);
    let y = f32::from(selection.origin.y - viewport.origin.y);
    let below = y + f32::from(selection.size.height);
    let top = if y >= height { y - height } else { below };
    (px(top.clamp(0.0, (f32::from(viewport.size.height) - height).max(0.0))),
     px(x.clamp(0.0, (f32::from(viewport.size.width) - width).max(0.0))))
}

pub(crate) fn selection_action_bar(
    theme: &Theme,
    top: gpui::Pixels,
    left: gpui::Pixels,
) -> gpui::Div {
    div()
        .absolute()
        .left(left)
        .top(top)
        .flex()
        .gap(px(Theme::SPACE_SM))
        .px(px(Theme::SPACE_SM))
        .py(px(Theme::SPACE_XS))
        .rounded(px(Theme::SPACE_SM))
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border)
        .text_color(theme.text)
        .text_size(crate::typography::ui_rems(12.0))
}
