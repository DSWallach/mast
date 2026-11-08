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
        .when(super::is_horizon(theme), |el| {
            el.rounded_full()
                .border_1()
                .border_color(theme.border_strong)
        })
}

pub(crate) fn artifact_revision_navigation(
    theme: &Theme,
    children: Vec<AnyElement>,
) -> Option<AnyElement> {
    super::is_horizon(theme).then(|| {
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
    super::is_horizon(theme).then(|| {
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
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash));
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
        .when(super::is_horizon(theme), |el| el.rounded_full());
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
        .when(super::is_horizon(theme), |el| {
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
        .when(super::is_horizon(theme), |el| el.rounded_full());
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
        .when(super::is_horizon(theme), |el| {
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
        .when(super::is_horizon(theme), |el| el.rounded_full());
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
        .when(super::is_horizon(theme), |el| el.rounded_full());
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
        .when(super::is_horizon(theme), |el| {
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
    let label = if super::is_industrial(theme) {
        SharedString::from(crate::popover::tracked_upper(&label))
    } else {
        label
    };
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
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().when(open, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(button, theme, open)
}

pub(crate) fn artifact_card_frame(id: SharedString, label: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let card = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(10.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .px(px(10.0))
        .py(px(9.0))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(label)
        .hover(|card| card.bg(theme.element_hover))
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash));
    super::segmented_key(card, theme, false)
}

pub(crate) fn artifact_card_icon(glyph: &'static str, theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(28.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(7.0)))
        .bg(theme.accent.opacity(0.10))
        .flex()
        .items_center()
        .justify_center()
        .child(icons::icon(glyph).size(px(15.0)).text_color(theme.accent))
        .when(super::is_horizon(theme), |el| el.rounded_full())
}

pub(crate) fn artifact_card_open(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let button = div()
        .id(id)
        .flex_none()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover))
        .child("Open")
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn browser_empty_icon(theme: &Theme) -> Div {
    div()
        .size(px(44.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(12.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised.opacity(0.5))
        .flex()
        .items_center()
        .justify_center()
        .child(
            icons::icon(icons::GLOBE)
                .size(px(22.0))
                .text_color(theme.text_muted),
        )
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        })
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().key)
                .border_color(theme.industrial_colors().rule)
        })
}

pub(crate) fn browser_empty_action(has_error: bool, theme: &Theme) -> Stateful<Div> {
    let label = if has_error {
        "Retry page"
    } else {
        "Enter an address"
    };
    let button = div()
        .mt(px(6.0))
        .id("browser-empty-action")
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
        .gap(px(8.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text)
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        });
    super::segmented_key(button, theme, false)
}

pub(crate) fn browser_go_button(theme: &Theme) -> Stateful<Div> {
    let button = div()
        .id("browser-go")
        .size(px(18.0))
        .flex_none()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label("Go to address")
        .hover(|s| s.bg(crate::theme::wash(0.10)))
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn artifact_comment_control(
    id: &'static str,
    text: SharedString,
    active: bool,
    enabled: bool,
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
        .gap(px(4.0))
        .when(enabled, |el| el.cursor_pointer())
        .when(!enabled, |el| el.cursor_default().opacity(0.35))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .when(active, |el| el.bg(crate::theme::wash(0.10)))
        .when(enabled, |el| {
            el.hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
        })
        .child(text)
        .when(super::is_horizon(theme), |el| {
            el.rounded_full()
                .when(active, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(button, theme, active)
}

pub(crate) fn artifact_option_row(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let row = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        });
    super::segmented_key(row, theme, false)
}

pub(crate) fn artifact_metadata_chip(text: String, color: gpui::Hsla, theme: &Theme) -> AnyElement {
    let chip = div()
        .flex_none()
        .px(px(5.0))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(color)
        .child(SharedString::from(text))
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(chip, theme, false).into_any_element()
}

pub(crate) fn artifact_comment_card(
    id: SharedString,
    draft: bool,
    focused: bool,
    resolved: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    let card = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(if draft { 6.0 } else { 4.0 }))
        .p(px(8.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(if draft {
            theme.accent.opacity(0.5)
        } else if focused {
            theme.accent
        } else {
            theme.border
        })
        .when(resolved, |el| el.opacity(0.65))
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash));
    super::segmented_key(card, theme, focused)
}

pub(crate) fn artifact_plan_frame(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_card)
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash))
        .when(super::is_industrial(theme), |el| {
            el.border_color(theme.industrial_colors().rule)
                .bg(theme.industrial_colors().key)
        })
}

pub(crate) fn artifact_index_row(
    id: SharedString,
    indent: f32,
    active: bool,
    focused: bool,
    top_level: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    let row = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .w_full()
        .pl(px(indent))
        .pr(px(8.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .truncate()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(if active { theme.text } else { theme.text_muted })
        .when(top_level, |el| el.font_weight(gpui::FontWeight::MEDIUM))
        .when(active, |el| {
            el.bg(crate::theme::wash(if focused { 0.20 } else { 0.10 }))
        })
        .hover(|el| el.bg(crate::theme::wash(0.14)))
        .when(super::is_horizon(theme), |el| {
            el.rounded_full()
                .when(active, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(row, theme, active)
}

pub(crate) fn browser_preview_open(
    element: Div,
    id: SharedString,
    label: String,
    available: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let button = element
        .id(id)
        .h(px(28.0))
        .px(px(6.0))
        .flex_shrink_0()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
        .role(gpui::Role::Button)
        .aria_label(label)
        .when(available, |el| {
            el.cursor_pointer()
                .hover(|style| style.bg(crate::theme::ink(0.05)))
        })
        .when(!available, |el| el.opacity(0.4))
        .child("Open")
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn browser_empty_notice(theme: &Theme) -> Div {
    div()
        .p(px(16.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(8.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(12.0))
        .line_height(px(19.0))
        .text_color(theme.text_muted)
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash))
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().key)
                .border_color(theme.industrial_colors().rule)
        })
}

pub(crate) fn artifact_index_toggle(
    glyph: &'static str,
    icon_size: f32,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id("artifact-index-toggle")
        .debug_selector(|| "artifact-index-toggle".into())
        .size(px(20.0))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(surface_chrome::CONTROL_RADIUS)))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|el| el.bg(crate::theme::wash(0.14)))
        .child(
            icons::icon(glyph)
                .size(px(icon_size))
                .text_color(theme.text_muted),
        )
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(button, theme, false)
}

pub(crate) fn artifact_question_badge(
    id: &'static str,
    label: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let badge = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_none()
        .px(px(7.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(9.0)))
        .border_1()
        .border_color(color.opacity(0.5))
        .bg(color.opacity(0.10))
        .text_size(crate::typography::ui_rems(10.5))
        .text_color(color)
        .child(SharedString::from(label))
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(badge, theme, false)
}

pub(crate) fn artifact_comment_badge(label: &'static str, color: gpui::Hsla, theme: &Theme) -> Div {
    let badge = div()
        .px(px(6.0))
        .h(px(16.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(8.0)))
        .border_1()
        .border_color(color.opacity(0.5))
        .bg(color.opacity(0.10))
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(color)
        .child(label)
        .when(super::is_horizon(theme), |el| el.rounded_full());
    super::segmented_key(badge, theme, false)
}

pub(crate) fn artifact_review_card(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .p(px(10.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(4.0))
        .when(super::is_horizon(theme), |el| el.bg(theme.accent_wash))
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().key)
                .border_color(theme.industrial_colors().rule)
        })
}

pub(crate) fn artifact_review_warning(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "review-stale".into())
        .p(px(8.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.warning)
        .text_color(theme.warning)
        .text_size(crate::typography::ui_rems(12.0))
        .when(super::is_horizon(theme), |el| {
            el.bg(theme.warning.opacity(0.1))
        })
}

pub(crate) fn artifact_plan_state(
    id: SharedString,
    label: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || id.to_string())
        .flex_none()
        .px(px(5.0))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(color)
        .child(SharedString::from(label))
        .when(super::is_horizon(theme), |el| el.rounded_full())
}

pub(crate) fn artifact_progress(fraction: f32, theme: &Theme) -> Div {
    let radius = theme.ui_style.tokens.control_radius.unwrap_or(2.0);
    div()
        .flex_1()
        .h(px(4.0))
        .rounded(px(radius))
        .bg(crate::theme::wash(0.12))
        .child(
            div()
                .h_full()
                .rounded(px(radius))
                .bg(theme.accent)
                .w(gpui::relative(fraction.clamp(0.0, 1.0))),
        )
}

pub(crate) fn artifact_picked_chip(label: String, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "artifact-picked-chip".into())
        .flex_none()
        .px(px(6.0))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.accent)
        .text_color(theme.accent)
        .child(SharedString::from(label))
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        })
}

pub(crate) fn artifact_pick_note(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "artifact-pick-note".into())
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(5.0)))
        .border_1()
        .border_color(theme.border)
        .px(px(6.0))
        .py(px(2.0))
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.accent_wash)
        })
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().key)
                .border_color(theme.industrial_colors().rule)
        })
}

pub(crate) fn artifact_picker_row(
    id: SharedString,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    let row = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .px(px(10.0))
        .py(px(4.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover))
        .when(selected, |el| el.bg(crate::theme::wash(0.08)))
        .when(super::is_horizon(theme), |el| {
            el.rounded_full()
                .when(selected, |el| el.bg(theme.accent_wash))
        });
    super::segmented_key(row, theme, selected)
}

/// Style-specific label for a browser action.
pub(crate) fn browser_action_label(label: &'static str, theme: &Theme) -> SharedString {
    if super::is_industrial(theme) {
        crate::popover::tracked_upper(label).into()
    } else {
        label.into()
    }
}
