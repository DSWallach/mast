//! Presentation primitives for settings pages.

use crate::theme::Theme;
use crate::theme::ink;
use gpui::{AnyElement, SharedString, div, prelude::*, px};

pub(crate) fn settings_ghost_action(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(8.0)))
        .when(super::is_area(theme), |el| {
            el.rounded_full()
                .bg(theme.accent.opacity(0.12))
                .text_color(theme.accent)
        })
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
        .text_color(if super::is_area(theme) {
            theme.accent
        } else {
            theme.text_muted
        })
        .cursor_pointer()
}

pub(crate) fn settings_effective_surface(
    surface: mast_theme::SurfacePreference,
    style: crate::ui_style::UiStyle,
) -> mast_theme::SurfacePreference {
    match style.surface {
        crate::ui_style::SurfaceModel::ThemeDefault => surface,
        crate::ui_style::SurfaceModel::Opaque => mast_theme::SurfacePreference::Opaque,
        crate::ui_style::SurfaceModel::Glass => mast_theme::SurfacePreference::Frosted,
    }
}

/// Illustrates the spacing and guidance selected by a layout.
pub(crate) fn settings_layout_preview(
    theme: &Theme,
    layout: crate::ui_style::UiLayout,
) -> gpui::AnyElement {
    let (description, gap) = match layout {
        crate::ui_style::UiLayout::Minimal => ("Current spacing", 4.0),
        crate::ui_style::UiLayout::Compact => ("More on screen", 2.0),
        crate::ui_style::UiLayout::Guided => ("More labels and guidance", 6.0),
    };
    div()
        .size_full()
        .rounded(px(crate::settings::widgets::OPTION_CARD_RADIUS))
        .bg(theme.surface)
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(gap))
        .children((0..4).map(|_| {
            div()
                .h(px(12.0))
                .w_full()
                .rounded(px(3.0))
                .bg(theme.surface_raised)
        }))
        .child(
            div()
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(description),
        )
        .into_any_element()
}

pub(crate) fn settings_page_header(theme: &Theme, title: &str, count: Option<usize>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(10.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(16.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(SharedString::from(title.to_string())),
        )
        .when_some(count, |el, count| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(SharedString::from(format!("{count}"))),
            )
        })
}

pub(crate) fn settings_option_card(
    theme: &Theme,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    preview: AnyElement,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .child(
            div()
                .h(px(crate::settings::widgets::OPTION_CARD_HEIGHT))
                .w_full()
                .rounded(px(theme
                    .ui_style
                    .radius_roles()
                    .panel
                    .unwrap_or(crate::settings::widgets::OPTION_CARD_RADIUS)))
                .overflow_hidden()
                .border_1()
                .border_color(if selected { theme.accent } else { theme.border })
                .when(super::is_industrial(theme), |el| el.border_b_2())
                .child(preview),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if selected {
                    theme.accent
                } else {
                    theme.text_muted
                })
                .child(crate::icons::icon(icon_path).size(px(16.0)).flex_none())
                .child(label.into()),
        )
}

pub(crate) fn settings_row_tile(theme: &Theme, icon_path: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .size(px(36.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(10.0)))
        .when(super::is_area(theme), |el| el.rounded_full())
        .border_1()
        .border_color(theme.border)
        .bg(if super::is_area(theme) {
            theme.accent.opacity(0.12)
        } else {
            ink(0.03)
        })
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.surface_raised).border_b_2()
        })
        .flex()
        .items_center()
        .justify_center()
        .child(
            crate::icons::icon(icon_path)
                .size(px(16.0))
                .text_color(if super::is_area(theme) {
                    theme.accent
                } else {
                    theme.text_muted
                }),
        )
}

/// Selection control for a surface or background effect.
pub(crate) fn settings_choice(
    theme: &Theme,
    id: SharedString,
    label: &'static str,
    selected: bool,
    compact: bool,
) -> gpui::Stateful<gpui::Div> {
    let industrial = super::is_industrial(theme);
    let area = super::is_area(theme);
    let label = if industrial {
        crate::popover::tracked_upper(label)
    } else {
        label.to_owned()
    };
    div()
        .id(id)
        .h(px(if compact { 28.0 } else { 30.0 }))
        .px(px(if compact { 9.0 } else { 10.0 }))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(7.0)))
        .when(area, |el| el.rounded_full())
        .border_1()
        .when(industrial, |el| el.border_b_2())
        .border_color(if selected { theme.accent } else { theme.border })
        .bg(if selected {
            theme.accent_wash
        } else if area {
            theme.accent.opacity(0.08)
        } else {
            theme.surface_raised.opacity(0.28)
        })
        .text_size(crate::typography::ui_rems(if compact {
            11.0
        } else {
            11.5
        }))
        .font_weight(if selected {
            gpui::FontWeight::MEDIUM
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(if selected {
            theme.accent
        } else {
            theme.text_muted
        })
        .flex()
        .items_center()
        .cursor_pointer()
        .when(!selected, |control| {
            control.hover(|style| style.bg(theme.surface_raised_hover))
        })
        .child(SharedString::from(label))
}
