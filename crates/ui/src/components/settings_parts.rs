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
pub(crate) fn settings_layout_preview(theme: &Theme, layout: crate::ui_style::UiLayout) -> gpui::AnyElement {
    let (description, gap) = match layout {
        crate::ui_style::UiLayout::Minimal => ("Current spacing", 4.0),
        crate::ui_style::UiLayout::Compact => ("More on screen", 2.0),
        crate::ui_style::UiLayout::Guided => ("More labels and guidance", 6.0),
    };
    div().size_full().rounded(px(crate::settings::widgets::OPTION_CARD_RADIUS))
        .bg(theme.surface).p(px(16.0)).flex().flex_col().gap(px(gap))
        .children((0..4).map(|_| div().h(px(12.0)).w_full().rounded(px(3.0)).bg(theme.surface_raised)))
        .child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_muted).child(description))
        .into_any_element()
}
