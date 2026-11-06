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
        element.rounded(px(4.0))
            .bg(if active { theme.industrial_colors().pressed } else { theme.industrial_colors().key })
            .border_1().border_color(theme.industrial_colors().rule).relative()
            .child(div().absolute().left(px(5.0)).right(px(5.0)).bottom_0().h(px(2.0)).bg(key_ledge(theme)))
    } else {
        element
    }
}

/// Circular key with a recessed inner ring for icon-only actions.
pub(crate) fn icon_key<T: gpui::Styled + gpui::ParentElement + gpui::StatefulInteractiveElement>(element: T, theme: &Theme, signal: bool) -> T {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        let colors = theme.industrial_colors();
        let fill = if signal { colors.signal } else { colors.key };
        let ring = crate::theme::flatten(theme.text.opacity(0.18), fill);
        let pressed = if signal { gpui::hsla(fill.h, fill.s, fill.l * 0.8, fill.a) } else { colors.pressed };
        element.rounded_full().bg(fill).border_1().border_color(colors.rule).relative()
            .active(move |style| style.bg(pressed))
            .child(div().absolute().inset(px(3.0)).rounded_full().border_1().border_color(ring))
    } else { element }
}

pub(crate) fn display(theme: &Theme) -> Div {
    let background = if theme.appearance.is_dark() { theme.bg } else { theme.solid };
    div().px(px(8.0)).py(px(3.0)).border_1().border_color(theme.industrial_colors().rule)
        .bg(background).text_color(if theme.appearance.is_dark() { theme.text } else { theme.on_solid })
        .font_family(theme.font_mono.clone()).text_size(crate::typography::ui_rems(10.0))
}

/// Low-contrast perforations for otherwise empty faceplate areas.
pub(crate) fn grille(theme: &Theme) -> Div {
    let color = theme.text.opacity(0.12);
    div().flex().flex_col().gap(px(4.0)).children((0..6).map(move |_| {
        div().flex().gap(px(4.0)).children((0..18).map(move |_| div().size(px(2.0)).rounded_full().bg(color)))
    }))
}

pub(crate) fn confirm_key<T: gpui::Styled>(element: T, theme: &Theme) -> T {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        let fg = if crate::theme::contrast_ratio(theme.text, theme.success) >= crate::theme::contrast_ratio(theme.bg, theme.success) { theme.text } else { theme.bg };
        element.bg(theme.success).text_color(fg).border_1().border_color(theme.industrial_colors().rule)
    } else { element }
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
