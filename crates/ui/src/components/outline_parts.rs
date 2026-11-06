//! Presentation primitives specific to work outlines and item details.

use crate::theme::Theme;
use gpui::{Div, SharedString, Stateful, div, prelude::*, px};

fn section_label(label: String, industrial: bool) -> (String, Option<String>) {
    if !industrial {
        return (label, None);
    }
    match label.rsplit_once(" (") {
        Some((title, suffix))
            if suffix.ends_with(')')
                && suffix[..suffix.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
        {
            (
                crate::popover::tracked_upper(title),
                Some(format!("({suffix}")),
            )
        }
        _ => (crate::popover::tracked_upper(&label), None),
    }
}

/// A collapsible item-details section heading; the caller attaches its action.
pub fn outline_section_header(
    id: &'static str,
    label: String,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;
    let (label, count) = section_label(label, industrial);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.label_size.unwrap_or(11.0) + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .when(industrial, |el| el.font_family(theme.font_sans.clone()))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(
            crate::icons::icon(if open {
                crate::icons::ALT_ARROW_DOWN
            } else {
                crate::icons::ALT_ARROW_RIGHT
            })
            .size(px(10.0))
            .text_color(theme.text_muted),
        )
        .child(SharedString::from(label))
        .when_some(count, |el, count| {
            el.child(
                div()
                    .font_family(theme.font_mono.clone())
                    .child(SharedString::from(count)),
            )
        })
}

/// Animated chat marker using the supplied theme rather than global appearance state.
pub fn outline_chat_indicator(
    status: crate::status_style::Status,
    key: impl Into<SharedString>,
    working_color: gpui::Hsla,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {
        crate::status_style::indicator_for_theme(status, theme).into_any_element()
    } else if status == crate::status_style::Status::Working {
        crate::loaders::mini_mono_spinner(key, 1.5, working_color, view, cx).into_any_element()
    } else {
        crate::status_style::indicator_for_theme(status, theme).into_any_element()
    }
}

/// Work-item glyph that follows the status-indicator variant.
pub fn outline_item_indicator(
    glyph: &'static str,
    color: gpui::Hsla,
    theme: &Theme,
) -> gpui::AnyElement {
    if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {
        div()
            .size(px(8.0))
            .rounded_full()
            .bg(color)
            .into_any_element()
    } else {
        div()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(color)
            .child(glyph)
            .into_any_element()
    }
}

/// Status word shown when the layout requests it.
pub fn outline_status_word(label: &'static str, color: gpui::Hsla, theme: &Theme) -> Option<Div> {
    theme.ui_style.behavior.status_words.then(|| {
        div()
            .flex_none()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(color)
            .child(label)
    })
}

/// First-use outline action; callers add labels, shortcuts, and handlers.
pub fn outline_first_use_button(id: &'static str, primary: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .when(
            primary && theme.ui_style.layout == crate::ui_style::UiLayout::Guided,
            |el| {
                el.h(px(32.0))
                    .bg(theme.solid)
                    .text_color(theme.on_solid)
                    .hover(|el| el.bg(theme.solid).text_color(theme.on_solid).opacity(0.85))
            },
        )
}

/// Start-chat action for an outline row.
pub fn outline_start_chat_button(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let guided = theme.ui_style.layout == crate::ui_style::UiLayout::Guided;
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .when(guided, |el| {
            el.h(px(32.0))
                .px(px(8.0))
                .bg(theme.solid)
                .text_color(theme.on_solid)
                .hover(|el| el.bg(theme.solid).text_color(theme.on_solid).opacity(0.85))
        })
        .child(
            crate::icons::icon(crate::icons::ACTION_PLAY)
                .size(px(10.0))
                .text_color(if guided {
                    theme.on_solid
                } else {
                    theme.text_muted
                }),
        )
        .child("Start Chat")
}

/// Selectable action in the outline toolbar.
pub fn outline_toolbar_button(
    id: &'static str,
    label: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(id)
        .px(px(8.0 * scale))
        .py(px(3.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .bg(theme.element_hover.opacity(0.5))
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.text_color(theme.text_muted))
        .hover(|el| el.text_color(theme.text));
    crate::surface_chrome::segmented_key(button, theme, active)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(SharedString::from(label))
}

/// Keyboard shortcut action in the outline toolbar.
pub fn outline_shortcuts_button(theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let button = div()
        .id("outline-shortcuts")
        .debug_selector(|| "outline-shortcuts".into())
        .aria_label("Keyboard shortcuts")
        .flex()
        .items_center()
        .px(px(6.0 * scale))
        .py(px(4.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .bg(theme.element_hover.opacity(0.5))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text));
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(crate::icons::icon(crate::icons::KEYBOARD).size(px(12.0)))
        .child("?")
}

/// Container for descendant status totals.
pub fn outline_status_totals(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.0))
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_faint)
        .when(super::is_industrial(theme), |el| {
            let dark = theme.appearance.is_dark();
            el.font_family(theme.font_mono.clone())
                .px(px(4.0))
                .border_1()
                .border_color(theme.industrial_colors().rule)
                .bg(if dark { theme.bg } else { theme.solid })
                .text_color(if dark { theme.text } else { theme.on_solid })
        })
}

/// Frame for an inline item-details editor.
pub fn outline_input_frame(
    input: &gpui::Entity<crate::composer::ComposerInput>,
    theme: &Theme,
) -> Div {
    let industrial = super::is_industrial(theme);
    div()
        .w_full()
        .min_h(px(24.0))
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(if industrial {
            theme.border
        } else {
            theme.accent
        })
        .bg(if industrial {
            theme.input_bg
        } else {
            theme.surface
        })
        .child(input.clone())
}

/// Close action for item details, including the Guided label.
pub fn outline_details_close(theme: &Theme) -> Stateful<Div> {
    let labels = theme.ui_style.behavior.labels == crate::ui_style::LabelMode::All;
    div()
        .id("work-item-details-close")
        .flex_none()
        .size(px(22.0))
        .when(labels, |el| {
            el.h(px(32.0)).w_auto().px(px(6.0)).gap(px(4.0))
        })
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::CLOSE)
                .size(px(12.0))
                .text_color(theme.text_muted),
        )
        .when(labels, |el| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("Close"),
            )
        })
}

/// Required-action control with a disabled state.
pub fn outline_action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    enabled: bool,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(id)
        .flex_none()
        .px(px(6.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .when(enabled && primary, |el| {
            el.cursor_pointer()
                .border_color(theme.accent.opacity(0.5))
                .bg(theme.accent.opacity(0.12))
                .text_color(theme.accent)
                .hover(|el| el.bg(theme.accent.opacity(0.22)))
        })
        .when(enabled && !primary, |el| {
            el.cursor_pointer()
                .border_color(theme.border)
                .text_color(theme.text_muted)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        })
        .when(!enabled, |el| {
            el.border_color(theme.border).text_color(theme.text_faint)
        });
    crate::surface_chrome::segmented_key(button, theme, primary && enabled)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(label)
}

/// A work-item status segment; the caller attaches its action.
pub fn outline_status_segment(
    label: &'static str,
    active: bool,
    completed: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let key = super::is_industrial(theme);
    let button = div()
        .id(SharedString::from(format!(
            "work-item-status-{}",
            label.to_lowercase()
        )))
        .px(px(8.0 * super::layout_spacing(theme)))
        .py(px(2.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.text_color(theme.text_muted))
        .when(key, |el| el.flex().items_center().gap(px(8.0)));
    crate::surface_chrome::segmented_key(button, theme, active)
        .when(active && completed && key, |el| {
            let foreground = if crate::theme::contrast_ratio(theme.text, theme.success)
                >= crate::theme::contrast_ratio(theme.bg, theme.success)
            {
                theme.text
            } else {
                theme.bg
            };
            el.bg(theme.success).text_color(foreground)
        })
        .when(super::is_area(theme), |el| el.rounded_full())
        .children(crate::surface_chrome::active_key_dot(
            theme,
            active && !completed,
        ))
        .hover(|el| el.text_color(theme.text))
        .child(label)
}

/// Group surface for the status segments.
pub fn outline_status_group(theme: &Theme) -> Div {
    div()
        .flex()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(6.0))
        .bg(theme.element_hover.opacity(0.5))
        .when(
            theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque,
            |el| el.bg(theme.surface),
        )
}

/// Optional Industrial status readout.
pub fn outline_status_readout(label: &'static str, theme: &Theme) -> Option<Div> {
    super::is_industrial(theme).then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(9.0))
                    .text_color(theme.text_muted)
                    .child("STATUS"),
            )
            .child(
                crate::surface_chrome::display(theme)
                    .child(SharedString::from(label.to_uppercase())),
            )
    })
}

#[cfg(test)]
mod tests {
    use super::section_label;

    #[test]
    fn only_industrial_tracks_section_labels() {
        assert_eq!(
            section_label("Child items (12)".into(), false),
            ("Child items (12)".into(), None)
        );
        let (label, count) = section_label("Child items (12)".into(), true);
        assert_eq!(label, crate::popover::tracked_upper("Child items"));
        assert_eq!(count.as_deref(), Some("(12)"));
    }
}
