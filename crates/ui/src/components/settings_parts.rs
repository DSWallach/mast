//! Presentation primitives for settings pages.

use crate::theme::Theme;
use gpui::{div, prelude::*, px};

pub(crate) fn settings_ghost_action(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(8.0)))
        .when(
            theme.ui_style.controls == crate::ui_style::ControlVariant::Key,
            |el| {
                el.bg(theme.surface_raised)
                    .border_1()
                    .border_b_2()
                    .border_color(theme.border)
            },
        )
        .px(px(10.0))
        .py(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
        .cursor_pointer()
}
