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
    button_with_identity(id, label, variant, size, theme, theme.identity_color(""))
}

/// Use the caller's agent identity for Area's filled primary action.
pub fn button_with_identity(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    size: ButtonSize,
    theme: &Theme,
    identity: gpui::Hsla,
) -> Stateful<Div> {
    let label = label.into();
    let area = theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area;
    let (horizontal, vertical, font, radius) = match size {
        ButtonSize::Sm => (6.0, 1.0, 10.5, 4.0),
        ButtonSize::Md => (12.0, 6.0, 13.0, 8.0),
    };
    let (spacing_scale, font_adjustment, height) = match theme.ui_style.layout {
        crate::ui_style::UiLayout::Minimal => (1.0, 0.0, None),
        crate::ui_style::UiLayout::Compact => (0.75, -1.0, Some(24.0)),
        crate::ui_style::UiLayout::Guided => (1.0, 1.0, Some(34.0)),
    };
    let button = div()
        .id(id)
        .flex_none()
        .px(px(horizontal * spacing_scale))
        .py(px(vertical * spacing_scale))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(radius)))
        .when(area, |el| {
            el.rounded_full().font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .when(size == ButtonSize::Md, |el| {
            el.when_some(height, |el, height| {
                el.min_h(px(height)).flex().items_center()
            })
        })
        .text_size(crate::typography::ui_rems(font + font_adjustment))
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
            let button = if area {
                button
                    .bg(identity)
                    .text_color(theme.identity_text(identity))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
            } else if key {
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
        .when(
            theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area,
            |el| el.rounded_full(),
        )
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
        .when(
            theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area,
            |el| el.rounded_full(),
        )
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

/// A selectable row; selection uses the active theme's semantic role.
pub fn list_row(theme: &Theme, selected: bool) -> Div {
    card_row(theme, true).when(selected, |el| el.bg(theme.element_active))
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

/// Semantic status colour supplied by the active theme.
#[derive(Clone, Copy)]
pub enum Status {
    Success,
    Warning,
    Danger,
}

pub fn status_dot(theme: &Theme, status: Status) -> Div {
    let color = match status {
        Status::Success => theme.success,
        Status::Warning => theme.warning,
        Status::Danger => theme.danger,
    };
    div().size(px(7.0)).rounded_full().bg(color)
}

/// A single selectable segment. The caller owns selection and activation.
pub fn tab(theme: &Theme, label: impl Into<SharedString>, selected: bool) -> Div {
    let area = theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area;
    let identity = theme.identity_color("");
    let item = div()
        .when(selected || area, |el| el.px(px(8.0)).py(px(5.0)))
        .when(selected && !area, |el| el.bg(theme.surface_raised))
        .when(area, |el| {
            el.rounded_full().font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .when(area && selected, |el| {
            el.bg(identity).text_color(theme.identity_text(identity))
        })
        .child(label.into());
    segmented_key(item, theme, selected)
}

/// Render the existing user-message shell; the caller supplies formatted content.
pub fn user_message_bubble(
    theme: &Theme,
    identity: gpui::Hsla,
    width: f32,
    line_height: f32,
    pending: bool,
    content: AnyElement,
) -> AnyElement {
    use crate::ui_style::ConversationVariant;
    let variant = theme.ui_style.conversation;
    let area = variant == ConversationVariant::Area;
    let text = if area {
        theme.identity_text(identity)
    } else {
        theme.text
    };
    let bubble = div()
        .min_w_0()
        .max_w(px(width * 0.8))
        .bg(crate::theme::user_bubble_bg())
        .rounded(px(theme
            .ui_style
            .radius_roles()
            .bubble
            .unwrap_or(Theme::BUBBLE_RADIUS)))
        .px(px(16.0))
        .py(px(10.0))
        .when(variant == ConversationVariant::Industrial, |el| {
            el.bg(theme.surface)
                .py(px(8.0))
                .border_1()
                .border_color(theme.border)
        })
        .when(area, |el| el.bg(identity).max_w_full().w_full())
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().body.unwrap_or(14.0),
        ))
        .line_height(crate::typography::ui_rems(line_height))
        .text_color(text)
        .when(pending, |el| el.opacity(0.65))
        .child(content);
    if variant == ConversationVariant::Industrial {
        div()
            .min_w_0()
            .max_w(px(width * 0.8))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(9.0))
                    .text_color(theme.text_muted)
                    .child("YOU"),
            )
            .child(bubble)
            .into_any_element()
    } else {
        bubble.into_any_element()
    }
}

/// Apply the assistant faceplate for the active conversation treatment.
pub fn agent_message_bubble(
    theme: &Theme,
    first: bool,
    model: Option<String>,
    content: AnyElement,
) -> AnyElement {
    if theme.ui_style.conversation == crate::ui_style::ConversationVariant::Industrial {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .when(first, |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(9.0))
                                .text_color(theme.text_muted)
                                .child("AGENT"),
                        )
                        .when_some(model, |el, model| el.child(display(theme).child(model))),
                )
            })
            .child(
                div()
                    .p(px(10.0))
                    .border_1()
                    .border_color(theme.industrial_colors().rule)
                    .bg(theme.industrial_colors().panel)
                    .child(content),
            )
            .into_any_element()
    } else {
        content
    }
}

/// Frame an existing input entity while retaining its focus and editing behavior.
pub fn text_input(theme: &Theme, input: AnyElement) -> Div {
    div()
        .p(px(8.0))
        .border_1()
        .border_color(theme.border)
        .when_some(theme.ui_style.radius_roles().control, |el, radius| {
            el.rounded(px(radius))
        })
        .when(
            theme.ui_style.controls == crate::ui_style::ControlVariant::Key,
            |el| el.bg(theme.input_bg),
        )
        .when(
            theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area,
            |el| el.bg(theme.identity_color("").opacity(0.12)),
        )
        .child(input)
}

/// Icon action with a layout-controlled optional label.
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: &'static str,
    label: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    let labels = theme.ui_style.behavior.labels == crate::ui_style::LabelMode::All;
    let control = div()
        .id(id)
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(4.0))
        .size(px(theme.ui_style.tokens.control_size))
        .child(crate::icons::icon(icon).size(px(theme.ui_style.tokens.control_icon_size)));
    let control = icon_key(control, theme, false);
    control.when(labels, |el| el.w_auto().child(label))
}

/// Section title with optional guidance for the Guided layout.
pub fn section_header(
    theme: &Theme,
    label: impl Into<SharedString>,
    help: Option<&'static str>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .child(field_label(theme, label))
        .when(theme.ui_style.behavior.help_cards, |el| {
            el.when_some(help, |el, help| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(
                            theme.ui_style.type_roles().secondary.unwrap_or(12.0),
                        ))
                        .text_color(theme.text_muted)
                        .child(help),
                )
            })
        })
}
