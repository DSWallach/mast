//! Presentation for code reference actions.
use crate::theme::Theme;
use gpui::{div, prelude::*, px};

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
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(8.0))
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border)
        .text_color(theme.text)
        .text_size(crate::typography::ui_rems(12.0))
}
