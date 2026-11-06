//! Shared interface primitives. Callers supply semantic intent; the active
//! appearance determines their treatment.

use crate::motion;
use crate::theme::{Theme, hairline, ink};
use gpui::{AnyElement, Div, ElementId, SharedString, Stateful, div, prelude::*, px};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Danger,
    Ghost,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    Sm,
    Md,
}

/// A button without an event handler. Callers attach their action after building.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    let label = label.into();
    let (horizontal, vertical, font, radius) = match size {
        ButtonSize::Sm => (6.0, 1.0, 10.5, 4.0),
        ButtonSize::Md => (12.0, 6.0, 13.0, 8.0),
    };
    let layout_metrics = crate::ui_style::UiStyle::resolve(
        crate::ui_style::UiStyleId::Default,
        theme.ui_style.layout,
    )
    .tokens;
    let spacing_scale =
        layout_metrics.control_size / crate::ui_style::UiStyle::MINIMAL.tokens.control_size;
    let button = div()
        .id(id)
        .flex_none()
        .px(px(horizontal * spacing_scale))
        .py(px(vertical * spacing_scale))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(radius)))
        .text_size(crate::typography::ui_rems(font))
        .cursor_pointer();
    match variant {
        ButtonVariant::Secondary => {
            let button = button
                .border_1()
                .border_color(theme.border)
                .text_color(theme.text_muted)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.text));
            crate::surface_chrome::segmented_key(button, theme, false).child(label)
        }
        ButtonVariant::Primary => {
            let key = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;
            let button = button
                .bg(theme.text)
                .text_color(theme.on_solid)
                .font_weight(gpui::FontWeight::MEDIUM)
                .hover(|el| el.opacity(0.9));
            let button = if key {
                crate::surface_chrome::segmented_key(button, theme, true)
                    .bg(theme.accent)
                    .text_color(theme.on_accent)
            } else {
                button
            };
            button.child(label)
        }
        ButtonVariant::Danger => button
            .bg(theme.danger_strong)
            .text_color(gpui::white())
            .font_weight(gpui::FontWeight::MEDIUM)
            .hover(|el| el.opacity(0.9))
            .child(label),
        ButtonVariant::Ghost => {
            let button = button
                .text_color(theme.text_muted)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.text));
            crate::surface_chrome::segmented_key(button, theme, false).child(label)
        }
    }
}

pub fn section_card(theme: &Theme) -> gpui::Div {
    div()
        .mt(px(24.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(12.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.card_glass_bg())
        .overflow_hidden()
        .flex()
        .flex_col()
}

pub fn badge(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .when_some(theme.ui_style.tokens.chip_radius, |el, radius| {
            el.rounded(px(radius))
        })
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(10.5))
        .text_color(theme.text_muted)
        .child(label.into())
}

pub fn badge_active(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    let emerald = theme.success;
    let emerald_text = theme.success_muted; // emerald-300
    div()
        .flex_none()
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .when_some(theme.ui_style.tokens.chip_radius, |el, radius| {
            el.rounded(px(radius))
        })
        .bg(emerald.opacity(0.12))
        .text_size(crate::typography::ui_rems(10.5))
        .text_color(emerald_text.opacity(0.9))
        .child(label.into())
}

pub fn toggle_switch(theme: &Theme, on: bool) -> gpui::Div {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        return div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .h(px(24.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.industrial_colors().rule)
            .bg(theme.industrial_colors().key)
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text)
            .child(
                div()
                    .relative()
                    .w(px(36.0))
                    .h(px(12.0))
                    .border_1()
                    .border_color(theme.industrial_colors().rule)
                    .bg(theme.industrial_colors().pressed)
                    .child(
                        div()
                            .absolute()
                            .left(px(5.0))
                            .bottom(px(-4.0))
                            .w(px(1.0))
                            .h(px(3.0))
                            .bg(theme.industrial_colors().rule),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(5.0))
                            .bottom(px(-4.0))
                            .w(px(1.0))
                            .h(px(3.0))
                            .bg(theme.industrial_colors().rule),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(1.0))
                            .left(px(if on { 25.0 } else { 1.0 }))
                            .size(px(8.0))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.industrial_colors().rule)
                            .bg(theme.industrial_colors().key)
                            .when(on, |el| {
                                el.child(
                                    div()
                                        .size(px(4.0))
                                        .m(px(2.0))
                                        .rounded_full()
                                        .bg(theme.accent),
                                )
                            }),
                    ),
            )
            .child(if on { "ON" } else { "OFF" });
    }
    div()
        .flex_none()
        .w(px(32.0))
        .h(px(18.0))
        .rounded_full()
        .bg(if on { theme.text } else { ink(0.15) })
        .relative()
        .child(
            div()
                .absolute()
                .top(px(2.0))
                .left(px(if on { 16.0 } else { 2.0 }))
                .size(px(14.0))
                .rounded_full()
                .bg(if on { theme.on_solid } else { ink(0.7) }),
        )
}

pub fn field_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(label.into())
}

pub fn card_row(theme: &Theme, first: bool) -> gpui::Div {
    div()
        .px(px(20.0))
        .py(px(14.0))
        .when(!first, |el| el.border_t_1().border_color(theme.border))
        .hover(|s| s.bg(ink(0.015)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.0))
}

pub fn menu_row(theme: &Theme, active: bool, fade_key: impl Into<SharedString>) -> gpui::Div {
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(crate::popover::MENU_ITEM_RADIUS)))
        .when_some(theme.ui_style.tokens.grid, |el, grid| el.py(px(grid)))
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.body_size.unwrap_or(13.0),
        ))
        .cursor_pointer();
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        let fade_key = fade_key.into();
        let mut row = row
            .bg(if active {
                theme.element_active
            } else {
                motion::hover_blend(&fade_key, theme.surface, theme.element_hover)
            })
            .text_color(theme.text)
            .children(crate::surface_chrome::active_key_dot(theme, active));
        if !active {
            row.interactivity()
                .on_hover(motion::hover_listener(fade_key));
        }
        row
    } else if active {
        row.bg(crate::theme::card_selected_bg())
            .text_color(theme.text)
    } else {
        let fade_key = fade_key.into();
        let mut row = row
            .text_color(motion::hover_blend(
                &fade_key,
                theme.text.opacity(0.9),
                theme.text,
            ))
            .bg(motion::hover_blend(
                &fade_key,
                crate::theme::wash(0.0),
                crate::theme::card_selected_bg(),
            ));
        // Imperative form — the caller's `.id(...)` makes the element stateful
        // (hover listeners need element state, `.on_hover` needs `Stateful`).
        row.interactivity()
            .on_hover(motion::hover_listener(fade_key));
        row
    }
}

pub fn search_input_frame(theme: &Theme, input: AnyElement) -> gpui::Div {
    div()
        .mb(px(4.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(8.0)))
        .bg(ink(0.04))
        .when(
            theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal,
            |el| el.bg(theme.input_bg).border_1().border_color(theme.border),
        )
        .text_size(crate::typography::ui_rems(13.0))
        .child(input)
}

pub fn dialog_field(theme: &Theme, input: AnyElement) -> gpui::Div {
    div()
        .w_full()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(8.0)))
        .border_1()
        .border_color(hairline(0.08))
        .bg(ink(0.04))
        .when(
            theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal,
            |el| el.bg(theme.input_bg).border_color(theme.border),
        )
        .text_size(crate::typography::ui_rems(14.0))
        .child(input)
}

pub fn segmented_key<T: gpui::Styled + gpui::ParentElement>(
    element: T,
    theme: &Theme,
    active: bool,
) -> T {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        element
            .rounded(px(4.0))
            .bg(if active {
                theme.industrial_colors().pressed
            } else {
                theme.industrial_colors().key
            })
            .border_1()
            .border_color(theme.industrial_colors().rule)
            .shadow(vec![gpui::BoxShadow {
                color: crate::surface_chrome::key_ledge(theme),
                offset: gpui::point(px(0.0), px(2.0)),
                blur_radius: px(0.0),
                spread_radius: px(0.0),
                inset: false,
            }])
    } else {
        element
    }
}

pub fn icon_key<T: gpui::Styled + gpui::ParentElement + gpui::StatefulInteractiveElement>(
    element: T,
    theme: &Theme,
    signal: bool,
) -> T {
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        let colors = theme.industrial_colors();
        let fill = if signal { colors.signal } else { colors.key };
        let ring = crate::theme::flatten(theme.text.opacity(0.18), fill);
        let pressed = if signal {
            gpui::hsla(fill.h, fill.s, fill.l * 0.8, fill.a)
        } else {
            colors.pressed
        };
        element
            .rounded_full()
            .bg(fill)
            .border_1()
            .border_color(colors.rule)
            .relative()
            .active(move |style| style.bg(pressed))
            .child(
                div()
                    .absolute()
                    .inset(px(3.0))
                    .rounded_full()
                    .border_1()
                    .border_color(ring),
            )
    } else {
        element
    }
}

pub fn display(theme: &Theme) -> Div {
    let background = if theme.appearance.is_dark() {
        theme.bg
    } else {
        theme.solid
    };
    div()
        .px(px(8.0))
        .py(px(3.0))
        .border_1()
        .border_color(theme.industrial_colors().rule)
        .bg(background)
        .text_color(if theme.appearance.is_dark() {
            theme.text
        } else {
            theme.on_solid
        })
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(10.0))
}
