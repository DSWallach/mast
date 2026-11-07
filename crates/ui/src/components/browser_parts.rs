//! Presentation primitives for browser and artifact surfaces.

use crate::{icons, surface_chrome, theme::Theme};
use gpui::{AnyElement, Div, SharedString, Stateful, div, prelude::*, px};

pub(crate) fn browser_toolbar_button(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    crate::files::toolbar_button(id, label, theme)
        .debug_selector(move || id.into())
        .when(!enabled, |el| el.cursor_default().opacity(0.35))
        .child(
            icons::icon(glyph)
                .size(px(surface_chrome::ICON_SIZE))
                .text_color(theme.text_muted),
        )
        .when(super::is_area(theme), |el| {
            el.rounded_full()
                .border_1()
                .border_color(theme.border_strong)
        })
}

pub(crate) fn artifact_revision_navigation(
    theme: &Theme,
    children: Vec<AnyElement>,
) -> Option<AnyElement> {
    super::is_area(theme).then(|| {
        div()
            .flex()
            .items_center()
            .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(12.0)))
            .border_1()
            .border_color(theme.border_strong)
            .overflow_hidden()
            .children(children)
            .into_any_element()
    })
}

pub(crate) fn artifact_revision_button(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    enabled: bool,
    previous: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .aria_label(label)
        .size(px(26.0))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled && previous, |el| el.bg(theme.accent))
        .when(!enabled, |el| el.opacity(0.35))
        .child(
            icons::icon(glyph)
                .size(px(12.0))
                .text_color(if enabled && previous {
                    theme.identity_text(theme.accent)
                } else {
                    theme.text_muted
                }),
        )
}

pub(crate) fn artifact_footer(
    theme: &Theme,
    author: SharedString,
    actions: Vec<AnyElement>,
) -> Option<AnyElement> {
    super::is_area(theme).then(|| {
        div()
            .w_full()
            .h(px(40.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_color(theme.text_muted)
            .text_size(crate::typography::ui_rems(11.0))
            .child(div().flex_1().min_w_0().truncate().child(author))
            .children(actions)
            .into_any_element()
    })
}

pub(crate) fn artifact_footer_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px(px(10.0))
        .py(px(5.0))
        .rounded_full()
        .cursor_pointer()
        .when(primary, |el| {
            el.bg(theme.accent)
                .text_color(theme.identity_text(theme.accent))
        })
        .when(!primary, |el| {
            el.border_1().border_color(theme.border_strong)
        })
        .child(label)
}

/// Local preview service row. The caller supplies its contents and action.
pub(crate) fn browser_preview_row(
    id: SharedString,
    available: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let border_strong = theme.border_strong;
    let row = div()
        .id(id)
        .w_full()
        .h(px(56.0))
        .px(px(14.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(10.0)))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::ink(0.02))
        .flex()
        .items_center()
        .gap(px(10.0))
        .when(available, |el| {
            el.cursor_pointer().hover(move |style| {
                style
                    .bg(crate::theme::ink(0.05))
                    .border_color(border_strong)
            })
        })
        .when(super::is_area(theme), |el| el.bg(theme.accent_wash));
    super::segmented_key(row, theme, false)
}

pub(crate) fn artifact_action_button(
    id: SharedString,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let debug = id.to_string();
    let button = div()
        .id(id)
        .debug_selector(move || debug.clone())
        .px(px(8.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(5.0)))
        .border_1()
        .border_color(if primary { theme.accent } else { theme.border })
        .when(primary, |el| {
            el.bg(theme.accent).text_color(theme.on_accent)
        })
        .when(!primary, |el| el.text_color(theme.text_muted))
        .text_size(crate::typography::ui_rems(11.0))
        .cursor_pointer()
        .child(label)
        .when(super::is_area(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, primary)
}

pub(crate) fn artifact_compare_button(
    id: SharedString,
    label: SharedString,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    let button = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .h(px(surface_chrome::CONTROL_SIZE))
        .px(px(7.0))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(surface_chrome::CONTROL_RADIUS)))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(if active { theme.text } else { theme.text_muted })
        .when(active, |el| el.bg(crate::theme::wash(0.12)))
        .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
        .child(label)
        .when(super::is_area(theme), |el| {
            el.rounded_full()
                .when(active, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(button, theme, active)
}

pub(crate) fn artifact_option_button(
    id: String,
    label: SharedString,
    primary: bool,
    busy: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.clone();
    let button = div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(5.0)))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.5))
        .when(primary, |el| el.bg(theme.accent).text_color(theme.surface))
        .when(!primary, |el| {
            el.border_1()
                .border_color(theme.border)
                .text_color(theme.text_muted)
                .hover(|el| el.text_color(theme.text).bg(crate::theme::wash(0.10)))
        })
        .when(busy, |el| el.opacity(0.5))
        .child(label)
        .when(super::is_area(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, primary)
}

pub(crate) fn artifact_option_toggle(
    id: String,
    label: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.clone();
    let button = div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .px(px(6.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .border_1()
        .border_color(if active { theme.accent } else { theme.border })
        .text_color(if active { theme.text } else { theme.text_muted })
        .child(SharedString::from(label))
        .when(super::is_area(theme), |el| {
            el.rounded_full()
                .when(active, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(button, theme, active)
}

pub(crate) fn artifact_find_button(
    id: &'static str,
    glyph: &'static str,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .debug_selector(move || id.into())
        .size(px(20.0))
        .flex_none()
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(surface_chrome::CONTROL_RADIUS)))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!enabled, |el| el.opacity(0.35))
        .hover(|el| el.bg(crate::theme::wash(0.14)))
        .child(
            icons::icon(glyph)
                .size(px(11.0))
                .text_color(theme.text_muted),
        )
        .when(super::is_area(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn artifact_zoom_button(
    id: &'static str,
    text: SharedString,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(surface_chrome::CONTROL_SIZE))
        .min_w(px(surface_chrome::CONTROL_SIZE))
        .px(px(4.0))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(surface_chrome::CONTROL_RADIUS)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
        })
        .when(!enabled, |el| el.cursor_default().opacity(0.35))
        .child(text)
        .when(super::is_area(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn browser_status_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .h(px(28.0))
        .px(px(10.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(label)
        .hover(|style| style.bg(crate::theme::wash(0.10)))
        .flex()
        .items_center()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text)
        .child(label)
        .when(super::is_area(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        });
    super::segmented_key(button, theme, false)
}

pub(crate) fn artifact_picker_button(
    id: &'static str,
    label: SharedString,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(surface_chrome::CONTROL_SIZE))
        .px(px(6.0))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(surface_chrome::CONTROL_RADIUS)))
        .flex()
        .items_center()
        .gap(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .when(open, |el| el.bg(crate::theme::wash(0.10)))
        .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
        .child(label)
        .child(icons::icon(icons::ALT_ARROW_DOWN).size(px(9.0)))
        .when(super::is_area(theme), |el| {
            el.rounded_full().when(open, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(button, theme, open)
}
