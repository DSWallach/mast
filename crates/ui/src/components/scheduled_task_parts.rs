use gpui::{Div, Styled, div, px};

pub(crate) fn task_row() -> Div {
    div().rounded(px(6.0))
}

pub(crate) fn task_kind() -> Div {
    div().text_size(crate::typography::ui_rems(9.5))
}

pub(crate) fn task_details() -> Div {
    div().text_size(crate::typography::ui_rems(11.0))
}

pub(crate) fn task_time() -> Div {
    div().text_size(crate::typography::ui_rems(10.0))
}

pub(crate) fn task_history() -> Div {
    div().text_size(crate::typography::ui_rems(10.0)).rounded(px(4.0))
}


pub(crate) fn cleanup_panel(theme: &crate::theme::Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .p(px(12.0))
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.0))
}
