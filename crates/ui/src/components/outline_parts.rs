//! Presentation primitives specific to work outlines and item details.

use crate::theme::Theme;
use gpui::{Div, SharedString, Stateful, div, prelude::*, px};

/// Primary actions in the Horizon details surface.
pub fn outline_details_primary(theme: &Theme, label: &str) -> bool {
    super::is_horizon(theme) && matches!(label, "Start Chat" | "Save" | "Add" | "Set date")
}

pub fn overview_group_button(id: SharedString, active: bool, theme: &Theme) -> Stateful<Div> {
    let button = div()
        .id(id)
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(4.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.5))
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.text_color(theme.text_muted));
    if super::is_horizon(theme) {
        super::toolbar_control(button, theme, false)
    } else {
        button
    }
}

pub fn overview_group_frame(theme: &Theme) -> Div {
    let frame = div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(6.0))
        .bg(theme.element_hover.opacity(0.3));
    if super::is_horizon(theme) {
        super::toolbar_control(frame, theme, false)
    } else {
        frame
    }
}

pub fn overview_work_item_filter(theme: &Theme, active: bool) -> Stateful<Div> {
    let trigger = div()
        .id("overview-work-item-filter")
        .relative()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(4.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .bg(theme.element_hover.opacity(0.5))
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.text_color(theme.text_muted))
        .hover(|el| el.text_color(theme.text));
    if super::is_horizon(theme) {
        super::toolbar_control(trigger, theme, false)
    } else {
        trigger
    }
}

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
        .group("outline-section")
        .when(super::is_horizon(theme), |el| el.relative().ml(px(-14.0)).pl(px(14.0)))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            if super::is_horizon(theme) {
                theme.ui_style.tokens.body_size.unwrap_or(13.0)
            } else {
                theme.ui_style.tokens.label_size.unwrap_or(11.0)
            } + super::layout_type_adjustment(theme),
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
            .text_color(theme.text_muted)
            .when(super::is_horizon(theme), |el| {
                el.absolute()
                    .left(px(0.0))
                    .top(px(2.0))
                    .opacity(0.0)
                    .group_hover("outline-section", |el| el.opacity(1.0))
            }),
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
    if super::is_horizon(theme) && status == crate::status_style::Status::Waiting {
        div()
            .size(px(8.0))
            .rounded_full()
            .bg(theme.accent)
            .into_any_element()
    } else if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {
        crate::status_style::indicator_for_theme(status, theme).into_any_element()
    } else if status == crate::status_style::Status::Working {
        crate::loaders::mini_mono_spinner(key, 1.5, working_color, view, cx).into_any_element()
    } else {
        crate::status_style::indicator_for_theme(status, theme).into_any_element()
    }
}

pub fn outline_selected_chat_indicator(
    status: crate::status_style::Status,
    key: impl Into<SharedString>,
    color: gpui::Hsla,
    theme: &Theme,
    selected: bool,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    if super::is_horizon(theme) && selected && theme.ui_style.design_roles.primary_colour.is_some() {
        div()
            .size(px(8.0))
            .rounded_full()
            .bg(theme.identity_text(theme.accent))
            .into_any_element()
    } else {
        outline_chat_indicator(status, key, color, theme, view, cx)
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
        SharedString::from(glyph).into_any_element()
    }
}

/// Status word shown when the layout requests it.
pub fn outline_status_word(label: &'static str, color: gpui::Hsla, theme: &Theme) -> Option<Div> {
    (theme.ui_style.behavior.status_words && !super::is_horizon(theme)).then(|| {
        div()
            .flex_none()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(color)
            .child(label)
    })
}

/// First-use outline action; callers add labels, shortcuts, and handlers.
pub fn outline_first_use_button(id: &'static str, primary: bool, theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0 * scale))
        .px(px(7.0 * scale))
        .py(px(2.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .text_color(theme.text_muted)
        .when(!(primary && theme.ui_style.layout == crate::ui_style::UiLayout::Guided), |el| {
            el.hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        })
        .when(
            primary && theme.ui_style.layout == crate::ui_style::UiLayout::Guided,
            |el| {
                el.h(px(32.0))
                    .bg(theme.solid)
                    .text_color(theme.on_solid)
                    .hover(|el| el.bg(theme.solid).text_color(theme.on_solid).opacity(0.85))
            },
        );
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_horizon(theme), |el| el.rounded_full())
}

/// Worker count and attention indicator for an outline row.
pub fn outline_worker_chip(id: SharedString, count: usize, needs_you: bool, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div().id(id).debug_selector(move || selector.clone())
        .flex_none().flex().items_center().gap(px(4.0)).px(px(5.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0))).bg(theme.element_hover).cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0)).text_color(theme.text_muted)
        .when(needs_you, |el| el.child(div().size(px(5.0)).rounded_full().bg(theme.warning)))
        .child(format!("{count} worker{}", if count == 1 { "" } else { "s" }))
}

/// Start-chat action for an outline row.
pub fn outline_start_chat_button(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let guided = theme.ui_style.layout == crate::ui_style::UiLayout::Guided;
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * scale))
        .px(px(4.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .when(!guided, |el| el.hover(|el| el.bg(theme.element_hover).text_color(theme.text)))
        .when(guided, |el| {
            el.h(px(32.0))
                .px(px(8.0))
                .bg(theme.solid)
                .text_color(theme.on_solid)
                .hover(|el| el.bg(theme.solid).text_color(theme.on_solid).opacity(0.85))
        });
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
    super::toolbar_control(
        crate::surface_chrome::segmented_key(button, theme, active),
        theme,
        false,
    )
    .when(super::is_horizon(theme), |el| el.rounded_full())
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
    super::toolbar_control(
        crate::surface_chrome::segmented_key(button, theme, false),
        theme,
        false,
    )
    .when(super::is_horizon(theme), |el| el.rounded_full())
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

pub fn outline_date_input_frame(
    input: &gpui::Entity<crate::composer::ComposerInput>,
    theme: &Theme,
    focused: bool,
) -> Div {
    outline_input_frame(input, theme).when(super::is_horizon(theme) && !focused, |el| {
        el.border_color(theme.border)
    })
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
        .when(super::is_horizon(theme), |el| {
            el.h(px(theme.ui_style.tokens.control_size))
        })
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .when(true, |el| super::toolbar_control(el, theme, false))
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
    super::toolbar_control(
        crate::surface_chrome::segmented_key(button, theme, primary && enabled),
        theme,
        false,
    )
    .when(super::is_horizon(theme), |el| {
        if enabled && (primary || matches!(label, "Approve" | "Accept rule" | "Send")) {
            el.rounded_full()
                .bg(theme.accent)
                .border_color(theme.accent)
                .text_color(theme.identity_text(theme.accent))
        } else {
            el.rounded_full()
                .border_color(theme.bg)
                .text_color(theme.text_muted)
        }
    })
    .when_some(if enabled { if primary { theme.ui_style.design_roles.primary_colour } else { theme.ui_style.design_roles.secondary_colour } } else { None }, |el, role| {
        let colour = role.colour(theme);
        if super::is_horizon(theme) && enabled && primary {
            el.bg(colour).border_color(colour).text_color(theme.identity_text(colour))
        } else {
            el.border_color(colour).text_color(colour)
        }
    })
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
        .when(true, |el| super::toolbar_control(el, theme, false))
        .when(super::is_horizon(theme) && active, |el| {
            el.bg(theme.accent)
                .text_color(theme.identity_text(theme.accent))
        })
        .when(active && completed && key, |el| {
            el.text_color(theme.success)
        })
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
        .when(super::is_horizon(theme), |el| {
            el.rounded_full().bg(theme.element_hover.opacity(0.3))
        })
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

/// Item-details panel frame.
pub fn outline_details_frame(theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-details")
        .debug_selector(|| "work-item-details".into())
        .flex_none()
        .w_full()
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.border)
        .when(super::is_industrial(theme), |el| {
            el.border_l_2().border_color(theme.industrial_colors().rule)
        })
        .bg(if theme.ui_style.design_roles.details_page_surface.unwrap_or_else(|| super::is_horizon(theme)) {
            theme.bg
        } else {
            theme.surface
        })
        .when(super::is_horizon(theme), |el| el.border_color(theme.bg))
        .when_some(theme.ui_style.design_roles.details_border, |el, visible| {
            el.border_color(if visible { theme.border } else { gpui::transparent_black() })
        })
}

/// Scrollable item-details body.
pub fn outline_details_body(theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-details-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(
            theme.ui_style.tokens.grid.unwrap_or(12.0) * super::layout_spacing(theme)
        ))
        .px(px(if theme.ui_style.tokens.grid.is_some() {
            16.0
        } else {
            14.0
        }))
        .py(px(
            theme.ui_style.tokens.grid.unwrap_or(10.0) * super::layout_spacing(theme)
        ))
}

/// Title in the item panel.
pub fn outline_details_title(title: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-title")
        .debug_selector(|| "work-item-title".into())
        .cursor_text()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.title_size.unwrap_or(15.0) + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(SharedString::from(title))
}

/// Item section heading with a count.
pub fn outline_list_heading(label: String, theme: &Theme) -> Div {
    let industrial = super::is_industrial(theme);
    let (label, count) = section_label(label, industrial);
    div()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .when(super::is_horizon(theme), |el| {
            el.text_size(crate::typography::ui_rems(
                theme.ui_style.tokens.body_size.unwrap_or(13.0)
                    + super::layout_type_adjustment(theme),
            ))
        })
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .when(industrial, |el| el.font_family(theme.font_mono.clone()))
        .child(SharedString::from(label))
        .when_some(count, |el, count| el.child(SharedString::from(count)))
}

/// Decorative Industrial panel marks.
pub fn outline_panel_marks(theme: &Theme) -> Option<Div> {
    super::is_industrial(theme).then(|| {
        div().flex().gap(px(2.0)).children((0..5).map(|_| {
            div()
                .w(px(2.0))
                .h(px(10.0))
                .bg(theme.industrial_colors().rule)
        }))
    })
}

use super::{
    ButtonSize, ButtonVariant, button, design_surface, is_horizon, is_industrial, layout_spacing,
    layout_type_adjustment, toolbar_control,
};

mod rows;
mod guides_proposals;
mod details;
mod chips_links;
mod required_instructions;
mod validation_bindings;
mod pickers;
pub use rows::*;
pub use guides_proposals::*;
pub use details::*;
pub use chips_links::*;
pub use required_instructions::*;
pub use validation_bindings::*;
pub use pickers::*;
pub(crate) use guides_proposals::outline_proposal_diff;
pub(crate) use required_instructions::outline_revision_line;

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
