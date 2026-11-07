//! Presentation primitives for settings pages.

use crate::appearance::AppearanceMode;
use crate::theme::ink;
use crate::theme::{Appearance, Theme};
use gpui::{AnyElement, Hsla, SharedString, div, prelude::*, px};
use mast_theme::{AccentSelection, SurfacePreference, ThemeSelection};

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
        .rounded(px(theme
            .ui_style
            .radius_roles()
            .panel
            .unwrap_or(crate::settings::widgets::OPTION_CARD_RADIUS)))
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

/// Shortcut recording key; the page owns its keyboard and pointer actions.
pub(crate) fn settings_shortcut_key(
    theme: &Theme,
    index: usize,
    recording: bool,
) -> gpui::Stateful<gpui::Div> {
    let area = super::is_area(theme);
    let industrial = super::is_industrial(theme);
    let key = div()
        .id(("shortcut-combo", index))
        .min_w(px(96.0))
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(8.0)))
        .when(area, |el| el.rounded_full())
        .border_1()
        .when(industrial, |el| el.border_b_2())
        .flex()
        .justify_center()
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(12.0))
        .cursor_pointer();
    if recording {
        key.border_color(theme.text.opacity(0.3))
            .bg(theme.text)
            .text_color(theme.on_solid)
    } else {
        key.border_color(theme.border)
            .bg(if area {
                theme.accent.opacity(0.1)
            } else {
                theme.bg
            })
            .text_color(theme.text)
            .hover(|s| {
                s.border_color(theme.text.opacity(0.2))
                    .bg(crate::theme::ink(0.03))
            })
    }
}

/// A destination option in a settings row.
pub(crate) fn settings_destination(
    theme: &Theme,
    index: usize,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let area = super::is_area(theme);
    div()
        .id(("appshot-destination", index))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(7.0)))
        .when(area, |el| el.rounded_full())
        .border_1()
        .when(super::is_industrial(theme), |el| el.border_b_2())
        .border_color(if selected {
            theme.text.opacity(0.24)
        } else {
            theme.border
        })
        .bg(if area {
            theme.accent.opacity(if selected { 0.2 } else { 0.08 })
        } else if selected {
            ink(0.09)
        } else {
            gpui::transparent_black()
        })
        .text_size(px(11.0))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .cursor_pointer()
}

/// Picker chrome, leaving focus and menu behavior to its owner.
pub(crate) fn settings_picker(
    theme: &Theme,
    id: SharedString,
    width: f32,
    open: bool,
    palette: bool,
) -> gpui::Stateful<gpui::Div> {
    let area = super::is_area(theme);
    let industrial = super::is_industrial(theme);
    let control = div()
        .id(id)
        .relative()
        .when(palette, |el| el.flex_none())
        .w(px(width))
        .h(px(if palette { 34.0 } else { 36.0 }))
        .px(px(if palette { 10.0 } else { 11.0 }))
        .rounded(px(theme
            .ui_style
            .radius_roles()
            .control
            .unwrap_or(if palette { 8.0 } else { 9.0 })))
        .when(area, |el| el.rounded_full())
        .border_1()
        .when(industrial, |el| el.border_b_2())
        .border_color(if open {
            theme.border_strong
        } else {
            theme.border
        })
        .bg(if area {
            theme.accent.opacity(if open { 0.16 } else { 0.08 })
        } else if palette {
            theme.surface_raised.opacity(if open { 0.75 } else { 0.42 })
        } else {
            ink(0.025)
        })
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer();
    if palette && !open {
        control.hover(|style| style.bg(theme.surface_raised_hover))
    } else {
        control
    }
}

fn bar(fraction: f32, tone: Hsla) -> gpui::Div {
    div()
        .h(px(5.0))
        .w(gpui::relative(fraction))
        .rounded(px(3.0))
        .bg(tone)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Corners {
    All,
    Left,
    Right,
}

fn miniature(theme: &Theme, corners: Corners, radius: f32) -> AnyElement {
    let line = theme.text.opacity(0.22);
    let strong = theme.text.opacity(0.34);
    let r = px(radius);
    let root = div().size_full().flex().flex_row().bg(theme.surface);
    let root = match corners {
        Corners::All => root.rounded(r),
        Corners::Left => root.rounded_tl(r).rounded_bl(r),
        Corners::Right => root.rounded_tr(r).rounded_br(r),
    };
    root.child(
        div()
            .w(px(44.0))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .px(px(8.0))
            .pt(px(14.0))
            .child(bar(0.70, strong))
            .child(bar(1.0, line))
            .child(bar(0.85, line))
            .child(bar(1.0, line)),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .my(px(8.0))
            .mr(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .p(px(10.0))
            .child(bar(0.62, strong))
            .child(bar(0.88, line))
            .child(bar(0.76, line))
            .child(bar(0.52, line)),
    )
    .into_any_element()
}

fn miniature_split(
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
    radius: f32,
) -> AnyElement {
    let light = Theme::for_selection(Appearance::Light, &themes.light, accent, surface);
    let dark = Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface);
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(div().w_1_2().h_full().overflow_hidden().child(miniature(
            &light,
            Corners::Left,
            radius,
        )))
        .child(div().w_1_2().h_full().overflow_hidden().child(miniature(
            &dark,
            Corners::Right,
            radius,
        )))
        .into_any_element()
}

pub(crate) fn settings_appearance_preview(
    active_theme: &Theme,
    mode: AppearanceMode,
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
) -> AnyElement {
    let radius = active_theme
        .ui_style
        .radius_roles()
        .panel
        .unwrap_or(crate::settings::widgets::OPTION_CARD_RADIUS);
    match mode {
        AppearanceMode::System => miniature_split(themes, accent, surface, radius),
        AppearanceMode::Light => miniature(
            &Theme::for_selection(Appearance::Light, &themes.light, accent, surface),
            Corners::All,
            radius,
        ),
        AppearanceMode::Dark => miniature(
            &Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface),
            Corners::All,
            radius,
        ),
    }
}

/// Illustration of a style choice inside the appearance picker.
pub(crate) fn settings_style_preview(theme: &Theme, id: crate::ui_style::UiStyleId) -> AnyElement {
    div()
        .size_full()
        .rounded(px(theme
            .ui_style
            .radius_roles()
            .panel
            .unwrap_or(crate::settings::widgets::OPTION_CARD_RADIUS)))
        .bg(theme.surface)
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(id.preview_gap()))
        .children((0..4).map(|_| {
            div()
                .h(px(12.0))
                .w_full()
                .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(3.0)))
                .bg(theme.surface_raised)
        }))
        .child(
            div()
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(id.description()),
        )
        .into_any_element()
}

/// Theme import sample with its original preview frame.
pub(crate) fn settings_theme_miniature(theme: &Theme) -> AnyElement {
    miniature(
        theme,
        Corners::All,
        crate::settings::widgets::OPTION_CARD_RADIUS,
    )
}
