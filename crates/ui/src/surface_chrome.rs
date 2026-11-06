//! Shared metrics for sidebar surface toolbars and their controls.

use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;

pub(crate) const HEADER_HEIGHT: f32 = Theme::TITLEBAR_HEIGHT;
pub(crate) const CONTROL_SIZE: f32 = 24.0;
pub(crate) const CONTROL_RADIUS: f32 = 6.0;
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const CONTROL_GAP: f32 = 4.0;
pub(crate) const EDGE_INSET: f32 = 8.0;

/// Apply the active look's segmented-key treatment while preserving legacy tabs.
pub(crate) fn key_ledge(theme: &Theme) -> gpui::Hsla {
    theme.industrial_colors().key_ledge
}

pub(crate) fn segmented_key<T: gpui::Styled + gpui::ParentElement>(element: T, theme: &Theme, active: bool) -> T {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        element.rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
            .bg(if active { theme.industrial_colors().pressed } else { theme.industrial_colors().key })
            .border_1().border_color(theme.industrial_colors().rule).relative()
            .child(div().absolute().left_0().right_0().bottom_0().h(px(2.0)).bg(key_ledge(theme)))
    } else {
        element
    }
}

pub(crate) fn active_key_dot(theme: &Theme, active: bool) -> Option<Div> {
    (active && theme.ui_style.controls == crate::ui_style::ControlVariant::Key)
        .then(|| div().size(px(6.0)).rounded_full().bg(theme.industrial_colors().signal))
}

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
