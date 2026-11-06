//! Shared metrics for sidebar surface toolbars and their controls.

use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;

pub(crate) const HEADER_HEIGHT: f32 = Theme::TITLEBAR_HEIGHT;
pub(crate) const CONTROL_SIZE: f32 = 24.0;
pub(crate) const CONTROL_RADIUS: f32 = 6.0;
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const CONTROL_GAP: f32 = 4.0;
pub(crate) const EDGE_INSET: f32 = 8.0;

/// Shared field treatment for the file search and browser address controls.
pub(crate) fn input(theme: &Theme) -> Div {
    div()
        .h(px(CONTROL_SIZE))
        .min_w_0()
        .flex_1()
        .px(px(8.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(CONTROL_RADIUS)))
        .bg(crate::theme::ink(0.035))
        .when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_1().border_color(theme.border))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(11.5))
}

pub(crate) fn toolbar(theme: &Theme) -> Div {
    div()
        .h(px(HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .px(px(EDGE_INSET))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .border_t_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(if theme.is_glass() {
            theme.surface.opacity(0.26)
        } else {
            theme.surface
        })
}
