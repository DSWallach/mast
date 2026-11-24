//! Presentation for the quota switch dialog.
use crate::theme::Theme;
use gpui::{Div, Stateful, div, prelude::*, px};

pub fn empty_message(theme: &Theme) -> Div {
    div()
        .py(px(10.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
}

pub fn duration(ix: usize, active: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("quota-duration", ix))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if active { theme.accent } else { theme.border })
        .bg(if active {
            theme.glass_hover()
        } else {
            theme.surface_raised
        })
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(if active { theme.text } else { theme.text_muted })
        .cursor_pointer()
}

pub fn heading(theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(14.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text)
}

pub fn description(theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(theme.text_muted)
}

pub fn field_label(theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_dim)
}
