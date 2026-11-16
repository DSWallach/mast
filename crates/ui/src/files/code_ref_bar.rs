//! Actions for a selected range in the file editor.
use gpui::{div, prelude::*, px};
use crate::theme::Theme;

pub(super) fn action_bar(theme: &Theme) -> gpui::Div {
    div().absolute().right(px(12.0)).top(px(12.0)).flex().gap(px(8.0))
        .px(px(10.0)).py(px(5.0)).rounded(px(8.0))
        .bg(theme.surface_raised).border_1().border_color(theme.border)
        .text_color(theme.text).text_size(crate::typography::ui_rems(12.0))
}
