//! Shared interface primitives. Callers supply semantic intent; the active
//! appearance determines their treatment.

use crate::theme::Theme;
use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px};

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
