//! Shared interface primitives. Callers supply semantic intent; the active
//! appearance determines their treatment.

use crate::motion;
use crate::theme::{Theme, hairline, ink};
use gpui::{AnyElement, Div, ElementId, SharedString, Stateful, div, prelude::*, px};

mod conversation_parts;
pub use conversation_parts::{
    ConversationComposerBody, agent_message_theme, conversation_add_harness_key,
    conversation_attach_key, conversation_code_copy_action, conversation_code_frame,
    conversation_code_icon_action, conversation_code_theme,
    conversation_code_token_color, conversation_column, conversation_composer_body,
    conversation_composer_content_height, conversation_composer_expanded,
    conversation_composer_text_size, conversation_composer_width, conversation_error_chip, conversation_footer_slot,
    conversation_harness_tab, conversation_harness_tabs, conversation_input_surface,
    conversation_input_text_color,
    conversation_markdown_gap, conversation_markdown_text_size, conversation_message_copy_button,
    conversation_model_picker, conversation_panel_radius, conversation_question_chip,
    conversation_reveal, conversation_send_hint, conversation_send_key, conversation_skill_chip,
    conversation_skill_remove, conversation_thought_line, conversation_tool_chip_frame,
    conversation_tool_detail_frame, conversation_tool_group_header, conversation_tool_more_lines,
    conversation_tool_output_line,
    conversation_tool_header, conversation_tool_icon_tile,
    conversation_tool_panel, conversation_tool_theme, conversation_user_bubble,
    conversation_wizard_surface, user_message_theme,
};

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

fn is_area(theme: &Theme) -> bool {
    theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area
}

/// Preserve a red danger role; neutral theme danger roles fall back to the
/// theme's red terminal role, then the existing Cofactr signal colour.
fn danger_fill(theme: &Theme) -> gpui::Hsla {
    fn is_red(color: gpui::Hsla) -> bool {
        color.s >= 0.25 && (color.h <= 0.10 || color.h >= 0.90)
    }
    [theme.danger_strong, theme.terminal.ansi[1]]
        .into_iter()
        .find(|color| is_red(*color))
        .unwrap_or_else(|| gpui::rgb(0xfe4b25).into())
}

fn identity_foreground_on(theme: &Theme, identity: gpui::Hsla, surface: gpui::Hsla) -> gpui::Hsla {
    let foreground = theme.identity_foreground(identity);
    if crate::theme::contrast_ratio(foreground, surface) >= 4.5 {
        foreground
    } else {
        theme.text
    }
}

fn is_industrial(theme: &Theme) -> bool {
    theme.ui_style.controls == crate::ui_style::ControlVariant::Key
}

fn layout_type_adjustment(theme: &Theme) -> f32 {
    match theme.ui_style.layout {
        crate::ui_style::UiLayout::Minimal => 0.0,
        crate::ui_style::UiLayout::Compact => -1.0,
        crate::ui_style::UiLayout::Guided => 1.0,
    }
}

fn layout_spacing(theme: &Theme) -> f32 {
    theme.ui_style.spacing_scale().small / crate::surface_chrome::CONTROL_GAP
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
        ButtonVariant::Danger => {
            let fill = danger_fill(theme);
            button
                .bg(fill)
                .text_color(theme.identity_text(fill))
                .font_weight(gpui::FontWeight::MEDIUM)
                .hover(|el| el.opacity(0.9))
                .child(label)
        }
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
        .when(is_area(theme), |el| {
            el.bg(theme.identity_color("").opacity(0.08))
                .border_color(gpui::transparent_black())
        })
        .when(is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().panel)
                .border_color(theme.industrial_colors().rule)
        })
        .overflow_hidden()
        .flex()
        .flex_col()
}

/// A card with an optional Industrial faceplate label.
pub fn section_card_with_label(theme: &Theme, label: &'static str) -> Div {
    section_card(theme).when(is_industrial(theme), |el| {
        el.child(
            div()
                .text_size(crate::typography::ui_rems(
                    10.0 + layout_type_adjustment(theme),
                ))
                .font_family(theme.font_mono.clone())
                .text_color(theme.text_muted)
                .child(label),
        )
    })
}

pub fn badge(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .px(px(8.0 * layout_spacing(theme)))
        .py(px(2.0 * layout_spacing(theme)))
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
        .text_size(crate::typography::ui_rems(
            10.5 + layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .when(is_area(theme), |el| {
            el.bg(theme.identity_color("").opacity(0.12))
                .border_color(gpui::transparent_black())
                .text_color(identity_foreground_on(
                    theme,
                    theme.identity_color(""),
                    crate::theme::flatten(theme.identity_color("").opacity(0.12), theme.surface),
                ))
        })
        .child(label.into())
}

pub fn badge_active(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    let emerald = theme.success;
    let emerald_text = theme.success_muted; // emerald-300
    div()
        .flex_none()
        .px(px(8.0 * layout_spacing(theme)))
        .py(px(2.0 * layout_spacing(theme)))
        .rounded_full()
        .when_some(theme.ui_style.tokens.chip_radius, |el, radius| {
            el.rounded(px(radius))
        })
        .when(
            theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area,
            |el| el.rounded_full(),
        )
        .bg(emerald.opacity(0.12))
        .text_size(crate::typography::ui_rems(
            10.5 + layout_type_adjustment(theme),
        ))
        .text_color(emerald_text.opacity(0.9))
        .child(label.into())
}

pub fn toggle_switch(theme: &Theme, on: bool) -> gpui::Div {
    let scale = layout_spacing(theme);
    if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {
        return div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0 * scale))
            .px(px(8.0 * scale))
            .h(px(24.0 * scale))
            .rounded(px(4.0 * scale))
            .border_1()
            .border_color(theme.industrial_colors().rule)
            .bg(theme.industrial_colors().key)
            .text_size(crate::typography::ui_rems(
                10.0 + layout_type_adjustment(theme),
            ))
            .text_color(theme.text)
            .child(
                div()
                    .relative()
                    .w(px(36.0 * scale))
                    .h(px(12.0 * scale))
                    .border_1()
                    .border_color(theme.industrial_colors().rule)
                    .bg(theme.industrial_colors().pressed)
                    .child(
                        div()
                            .absolute()
                            .left(px(5.0 * scale))
                            .bottom(px(-4.0))
                            .w(px(1.0 * scale))
                            .h(px(3.0 * scale))
                            .bg(theme.industrial_colors().rule),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(5.0 * scale))
                            .bottom(px(-4.0))
                            .w(px(1.0 * scale))
                            .h(px(3.0 * scale))
                            .bg(theme.industrial_colors().rule),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(1.0 * scale))
                            .left(px(if on { 25.0 * scale } else { 1.0 * scale }))
                            .size(px(8.0 * scale))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.industrial_colors().rule)
                            .bg(theme.industrial_colors().key)
                            .when(on, |el| {
                                el.child(
                                    div()
                                        .size(px(4.0 * scale))
                                        .m(px(2.0 * scale))
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
        .w(px(32.0 * scale))
        .h(px(18.0 * scale))
        .rounded_full()
        .bg(if on && is_area(theme) {
            theme.identity_color("")
        } else if on {
            theme.text
        } else {
            ink(0.15)
        })
        .relative()
        .child(
            div()
                .absolute()
                .top(px(2.0 * scale))
                .left(px(if on { 16.0 * scale } else { 2.0 * scale }))
                .size(px(14.0 * scale))
                .rounded_full()
                .bg(if on { theme.on_solid } else { ink(0.7) }),
        )
}

pub fn field_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(
            13.0 + layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(label.into())
}

pub fn card_row(theme: &Theme, first: bool) -> gpui::Div {
    div()
        .px(px(20.0))
        .py(px(14.0 * layout_spacing(theme)))
        .when(!first, |el| el.border_t_1().border_color(theme.border))
        .hover(|s| s.bg(ink(0.015)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.0 * layout_spacing(theme)))
}

/// A selectable row; selection uses the active theme's semantic role.
pub fn list_row(theme: &Theme, selected: bool) -> Div {
    let identity = theme.identity_color("");
    let white = gpui::hsla(0.0, 0.0, 1.0, 1.0);
    let selected_fill = if selected && is_area(theme) {
        (0..=100)
            .map(|step| {
                gpui::hsla(
                    identity.h,
                    identity.s,
                    identity.l * (1.0 - step as f32 / 100.0),
                    identity.a,
                )
            })
            .find(|fill| crate::theme::contrast_ratio(*fill, white) >= 4.5)
            .unwrap_or(identity)
    } else {
        identity
    };
    let colors = theme.industrial_colors();
    card_row(theme, true)
        .px(px(12.0 * layout_spacing(theme)))
        .when(selected && !is_area(theme) && !is_industrial(theme), |el| {
            el.bg(theme.element_active)
        })
        .when(selected && is_area(theme), |el| {
            el.rounded_full()
                .bg(selected_fill)
                .text_color(white)
                .child(div().size(px(7.0)).rounded_full().bg(white))
        })
        .when(selected && is_industrial(theme), |el| {
            el.bg(colors.pressed)
                .child(div().size(px(7.0)).rounded_full().bg(colors.signal))
        })
}

pub fn menu_row(theme: &Theme, active: bool, fade_key: impl Into<SharedString>) -> gpui::Div {
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0 * layout_spacing(theme)))
        .px(px(8.0 * layout_spacing(theme)))
        .py(px(6.0 * layout_spacing(theme)))
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(crate::popover::MENU_ITEM_RADIUS)))
        .when_some(theme.ui_style.tokens.grid, |el, grid| {
            el.py(px(grid * layout_spacing(theme)))
        })
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.body_size.unwrap_or(13.0) + layout_type_adjustment(theme),
        ))
        .when(is_area(theme), |el| el.rounded_full())
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
    let base = div()
        .px(px(8.0 * layout_spacing(theme)))
        .py(px(3.0 * layout_spacing(theme)))
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().label.unwrap_or(10.0) + layout_type_adjustment(theme),
        ));
    if is_industrial(theme) {
        let background = if theme.appearance.is_dark() {
            theme.bg
        } else {
            theme.solid
        };
        base.border_1()
            .border_color(theme.industrial_colors().rule)
            .bg(background)
            .text_color(if theme.appearance.is_dark() {
                theme.text
            } else {
                theme.on_solid
            })
    } else {
        base.rounded_full()
            .bg(theme.element_hover)
            .text_color(theme.text_muted)
    }
}

/// Semantic status colour supplied by the active theme.
#[derive(Clone, Copy)]
pub enum Status {
    Success,
    Warning,
    Danger,
}

pub fn status_dot(theme: &Theme, status: Status) -> Div {
    let color = if is_area(theme) {
        theme.identity_color("")
    } else {
        match status {
            Status::Success => theme.success,
            Status::Warning => theme.warning,
            Status::Danger => theme.danger,
        }
    };
    if is_industrial(theme) {
        div()
            .size(px(11.0))
            .rounded_full()
            .border_1()
            .border_color(theme.industrial_colors().rule)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(7.0)).rounded_full().bg(color))
    } else {
        div().size(px(7.0)).rounded_full().bg(color)
    }
}

/// Status dot with the word revealed by the active layout.
pub fn status_indicator(theme: &Theme, status: Status, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(status_dot(theme, status))
        .when(theme.ui_style.behavior.status_words, |el| el.child(label))
}

fn tab_horizontal_inset(style: crate::ui_style::UiStyle) -> f32 {
    if style.id == crate::ui_style::UiStyleId::Default
        && style.layout == crate::ui_style::UiLayout::Minimal
    {
        8.0
    } else {
        12.0 * style.spacing_scale().small / crate::surface_chrome::CONTROL_GAP
    }
}

/// A single selectable segment. The caller owns selection and activation.
pub fn tab(theme: &Theme, label: impl Into<SharedString>, selected: bool) -> Div {
    let area = theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area;
    let identity = theme.identity_color("");
    let horizontal_inset = tab_horizontal_inset(theme.ui_style);
    let item = div()
        .when(selected || area, |el| {
            el.px(px(horizontal_inset))
                .py(px(5.0 * layout_spacing(theme)))
        })
        .when(selected && !area, |el| el.bg(theme.surface_raised))
        .when(area, |el| {
            el.rounded_full().font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .when(area && selected, |el| {
            el.bg(identity).text_color(theme.identity_text(identity))
        })
        .when(
            theme.ui_style.layout != crate::ui_style::UiLayout::Minimal,
            |el| {
                el.text_size(crate::typography::ui_rems(
                    13.0 + layout_type_adjustment(theme),
                ))
            },
        )
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
        .px(px(16.0 * layout_spacing(theme)))
        .py(px(10.0 * layout_spacing(theme)))
        .when(variant == ConversationVariant::Industrial, |el| {
            el.bg(theme.surface)
                .py(px(8.0 * layout_spacing(theme)))
                .border_1()
                .border_color(theme.border)
        })
        .when(area, |el| el.bg(identity).max_w_full().w_full())
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().body.unwrap_or(14.0) + layout_type_adjustment(theme),
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
    identity: gpui::Hsla,
    first: bool,
    first_block: bool,
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
                    .p(px(10.0 * layout_spacing(theme)))
                    .border_1()
                    .border_color(theme.industrial_colors().rule)
                    .bg(theme.industrial_colors().panel)
                    .child(content),
            )
            .into_any_element()
    } else if theme.ui_style.conversation == crate::ui_style::ConversationVariant::Area {
        div()
            .flex()
            .items_baseline()
            .gap(px(8.0))
            .min_w_0()
            .when(first_block, |el| {
                el.child(div().flex_none().size(px(10.0)).rounded_full().bg(identity))
            })
            .font_family("EB Garamond")
            .text_size(crate::typography::ui_rems(
                16.0 + layout_type_adjustment(theme),
            ))
            .text_color(identity_foreground_on(theme, identity, theme.surface))
            .child(content)
            .into_any_element()
    } else {
        content
    }
}

/// Frame an existing input entity while retaining its focus and editing behavior.
pub fn text_input(theme: &Theme, input: AnyElement) -> Div {
    div()
        .p(px(8.0 * layout_spacing(theme)))
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
        .justify_center()
        .gap(px(4.0))
        .size(px(theme.ui_style.tokens.control_size))
        .when(is_area(theme), |el| {
            el.rounded_full().bg(theme.identity_color("").opacity(0.12))
        })
        .text_color(theme.text)
        .child(
            crate::icons::icon(icon)
                .size(px(theme.ui_style.tokens.control_icon_size))
                .text_color(theme.text),
        );
    let control = icon_key(control, theme, false);
    control.when(labels, |el| {
        el.w_auto().px(px(8.0 * layout_spacing(theme))).child(label)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_inset_preserves_default_minimal_and_scales_all_other_combinations() {
        use crate::ui_style::{UiLayout, UiStyle, UiStyleId};
        for (style, expected) in [
            (UiStyleId::Default, [8.0, 9.0, 15.0]),
            (UiStyleId::Industrial, [12.0, 9.0, 15.0]),
            (UiStyleId::Area, [12.0, 9.0, 15.0]),
        ] {
            for (&layout, inset) in UiLayout::ALL.iter().zip(expected) {
                assert_eq!(tab_horizontal_inset(UiStyle::resolve(style, layout)), inset);
            }
        }
    }

    #[test]
    fn dark_theme_component_foregrounds_contrast() {
        for id in ["mast-dark", "industrial-dark", "area-dark"] {
            let theme = Theme::for_selection(
                crate::theme::Appearance::Dark,
                id,
                Default::default(),
                mast_theme::SurfacePreference::Opaque,
            );
            let fill = danger_fill(&theme);
            assert!(
                fill.s >= 0.25 && (fill.h <= 0.10 || fill.h >= 0.90),
                "danger hue in {id}"
            );
            let danger_text = theme.identity_text(fill);
            assert!(
                crate::theme::contrast_ratio(danger_text, fill) >= 4.5,
                "danger in {id}"
            );
            let identity = theme.identity_color("");
            let chip = crate::theme::flatten(identity.opacity(0.12), theme.surface);
            assert!(
                crate::theme::contrast_ratio(identity_foreground_on(&theme, identity, chip), chip)
                    >= 4.5,
                "chip in {id}"
            );
            assert!(
                crate::theme::contrast_ratio(
                    identity_foreground_on(&theme, identity, theme.surface),
                    theme.surface
                ) >= 4.5,
                "agent in {id}"
            );
        }
    }
}
