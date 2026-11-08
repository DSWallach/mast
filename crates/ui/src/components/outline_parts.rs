//! Presentation primitives specific to work outlines and item details.

use crate::theme::Theme;
use gpui::{Div, SharedString, Stateful, div, prelude::*, px};

/// Primary actions in the Area details surface.
pub fn outline_details_primary(theme: &Theme, label: &str) -> bool {
    super::is_area(theme) && matches!(label, "Start Chat" | "Save" | "Add" | "Set date")
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
    if super::is_area(theme) {
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
    if super::is_area(theme) {
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
    if super::is_area(theme) {
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
        .when(super::is_area(theme), |el| el.relative().ml(px(-14.0)).pl(px(14.0)))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            if super::is_area(theme) {
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
            .when(super::is_area(theme), |el| {
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
    if super::is_area(theme) && status == crate::status_style::Status::Waiting {
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
    if super::is_area(theme) && selected {
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
    (theme.ui_style.behavior.status_words && !super::is_area(theme)).then(|| {
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
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
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
        .when(super::is_area(theme), |el| el.rounded_full())
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
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .when(guided, |el| {
            el.h(px(32.0))
                .px(px(8.0))
                .bg(theme.solid)
                .text_color(theme.on_solid)
                .hover(|el| el.bg(theme.solid).text_color(theme.on_solid).opacity(0.85))
        });
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_area(theme), |el| el.rounded_full())
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
    super::toolbar_control(
        crate::surface_chrome::segmented_key(button, theme, false),
        theme,
        false,
    )
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

pub fn outline_date_input_frame(
    input: &gpui::Entity<crate::composer::ComposerInput>,
    theme: &Theme,
    focused: bool,
) -> Div {
    outline_input_frame(input, theme).when(super::is_area(theme) && !focused, |el| {
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
        .when(super::is_area(theme), |el| {
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
    .when(super::is_area(theme), |el| {
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
        if super::is_area(theme) && enabled && primary {
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
        .when(super::is_area(theme) && active, |el| {
            el.bg(theme.accent)
                .text_color(theme.identity_text(theme.accent))
        })
        .when(active && completed && key, |el| {
            el.text_color(theme.success)
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
        .when(super::is_area(theme), |el| {
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
        .bg(if theme.ui_style.design_roles.details_page_surface.unwrap_or_else(|| super::is_area(theme)) {
            theme.bg
        } else {
            theme.surface
        })
        .when(super::is_area(theme), |el| el.border_color(theme.bg))
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
        .when(super::is_area(theme), |el| {
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

/// Root surface for the outline tree.
pub fn outline_tree(theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-outline")
        .relative()
        .debug_selector(|| "work-outline".into())
        .when(
            theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque,
            |el| el.bg(theme.surface),
        )
        .when(super::is_area(theme), |el| el.bg(theme.bg))
        .when_some(theme.ui_style.design_roles.outline_page_surface, |el, page| el.bg(if page { theme.bg } else { theme.surface }))
}

pub fn outline_overview_header(theme: &Theme) -> Div {
    div().when(super::is_area(theme), |el| el.bg(theme.bg))
        .when_some(theme.ui_style.design_roles.outline_page_surface, |el, page| el.bg(if page { theme.bg } else { theme.surface }))
}

/// Area presents descendants inside a continuous rounded surface per ancestor.
pub fn outline_uses_rounded_nesting(theme: &Theme) -> bool {
    super::is_area(theme)
}

pub fn outline_nested_row(key: &str, depth: u16, previous: u16, next: u16, row: gpui::AnyElement, theme: &Theme) -> gpui::AnyElement {
    let offset = |level: u16| level as f32 * 20.0;
    let backgrounds = (1..=depth).map(|level| {
        let selector = format!("area-outline-nesting-{key}-{level}");
        div().id(selector.clone()).debug_selector(move || selector.clone())
            .absolute().top_0().bottom_0().right_0().left(px(offset(level)))
            .bg(theme.text.opacity(0.035))
            .when(previous < level, |el| el.rounded_tl(px(12.)).rounded_tr(px(12.)))
            .when(next < level, |el| el.rounded_bl(px(12.)).rounded_br(px(12.)))
    });
    div().relative().w_full().children(backgrounds)
        .child(div().w_full().flex().pl(px(offset(depth))).child(row))
        .into_any_element()
}

/// Row chrome; selection, drag handling and content remain with the caller.
pub fn outline_row(id: SharedString, height: f32, indent: f32, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let row = div()
        .id(id)
        .group("outline-row")
        .debug_selector(move || selector.clone())
        .h(px(height))
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme.ui_style.tokens.grid.unwrap_or(4.0)))
        .pl(px(indent))
        .pr(px(8.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.body_size.unwrap_or(12.5),
        ))
        .border_1()
        .border_color(gpui::transparent_black());
    super::design_surface(row, theme, crate::style_overrides::DesignSurface::OutlineRow)
}

pub fn outline_row_theme(theme: &Theme, selected: bool) -> Theme {
    let mut result = theme.clone();
    if selected && (super::is_area(theme) || theme.ui_style.design_roles.primary_colour.is_some()) {
        let colour = theme.ui_style.design_roles.primary_colour.map(|role| role.colour(theme)).unwrap_or(theme.accent);
        result.text = if theme.ui_style.design_roles.surface(crate::style_overrides::DesignSurface::OutlineRow).page_colour.is_some() { theme.text } else { theme.identity_text(colour) };
        result.text_muted = result.text;
        result.text_faint = result.text.opacity(0.8);
    }
    result
}

pub fn outline_row_selection<T: gpui::Styled>(element: T, theme: &Theme, selected: bool) -> T {
    let element = if super::is_area(theme) && selected {
        element
            .max_w(px(400.0))
            .rounded_full()
            .bg(theme.accent)
            .border_color(theme.accent)
    } else {
        element
    };
    let element = if let Some(radius) = theme.ui_style.design_roles.panel_radius { element.rounded(px(radius)) } else { element };
    let element = if let Some(role) = theme.ui_style.design_roles.primary_colour.filter(|_| selected) {
        let colour = role.colour(theme); element.bg(colour).border_color(colour)
    } else { element };
    super::design_surface(element, theme, crate::style_overrides::DesignSurface::OutlineRow)
}

pub fn outline_row_extras(theme: &Theme) -> bool {
    !super::is_area(theme)
}

pub fn outline_attention_color(
    theme: &Theme,
    needs_attention: bool,
    color: gpui::Hsla,
) -> gpui::Hsla {
    if super::is_area(theme) && needs_attention {
        theme.accent
    } else {
        color
    }
}

pub fn outline_selected_dot_color(theme: &Theme, selected: bool, color: gpui::Hsla) -> gpui::Hsla {
    if super::is_area(theme) && selected {
        theme.identity_text(theme.accent)
    } else {
        color
    }
}

pub fn outline_row_highlight(theme: &Theme) -> gpui::Hsla {
    if super::is_area(theme) {
        gpui::transparent_black()
    } else {
        theme.accent.opacity(0.6)
    }
}

pub fn outline_space_dot(
    theme: &Theme,
    id: &str,
    needs_attention: bool,
    selected: bool,
) -> Option<Div> {
    super::is_area(theme).then(|| {
        div()
            .flex_none()
            .size(px(8.0))
            .rounded_full()
            .bg(outline_selected_dot_color(
                theme,
                selected,
                if needs_attention {
                    theme.accent
                } else {
                    theme.identity_color(id)
                },
            ))
    })
}

pub fn outline_missing_chat(theme: &Theme, id: &str) -> Option<gpui::AnyElement> {
    super::is_area(theme).then(|| {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme.text_muted)
            .child(SharedString::from(format!("Missing chat · {id}")))
            .into_any_element()
    })
}

/// Overview filter menu trigger.
pub fn outline_filter_trigger(theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let trigger = div()
        .id("overview-filter-menu")
        .debug_selector(|| "overview-filter-menu".into())
        .relative()
        .px(px(8.0 * scale))
        .py(px(3.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_color(theme.text_muted)
        .bg(theme.element_hover.opacity(0.5));
    super::toolbar_control(
        crate::surface_chrome::segmented_key(trigger, theme, false),
        theme,
        false,
    )
    .when(super::is_area(theme), |el| el.rounded_full())
    .child("⋯")
}

/// Checkable overview filter menu row.
pub fn outline_filter_row(
    id: &'static str,
    label: &'static str,
    checked: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let row = div()
        .id(id)
        .debug_selector(move || id.into())
        .px(px(8.0 * scale))
        .py(px(5.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(row, theme, checked)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(format!("{}  {}", if checked { "✓" } else { "　" }, label))
}

/// Compact metadata chip used on outline rows.
pub fn outline_metadata_chip(label: String, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .flex_none()
        .px(px(5.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(SharedString::from(label))
}

/// A compact path label for an associated work item.
pub fn outline_path_chip(id: SharedString, label: String, theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(|| "work-item-path-chip".into())
        .min_w_0()
        .max_w(px(160.0))
        .px(px(5.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .truncate()
        .text_size(crate::typography::ui_rems(
            9.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(label))
}

/// Status marker and count for a pending user action.
pub fn outline_attention(id: SharedString, count: u32, theme: &Theme) -> Stateful<Div> {
    let color =
        crate::status_style::status_style(crate::status_style::Status::Waiting, theme.appearance)
            .color;
    div()
        .id(id)
        .debug_selector(|| "work-item-attention".into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(crate::status_style::indicator_for_theme(
            crate::status_style::Status::Waiting,
            theme,
        ))
        .when(count > 1, |el| {
            el.child(SharedString::from(count.to_string()))
        })
}

/// Required-action row surface.
pub fn outline_required_action_row(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(2.0 * scale))
        .px(px(6.0 * scale))
        .py(px(4.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
        .when(super::is_industrial(theme), |el| {
            el.border_1().border_color(theme.industrial_colors().rule)
        })
        .when(super::is_area(theme), |el| el.py(px(8.0 * scale)))
}

/// Template editor node header.
pub fn outline_template_header(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

/// Template editor node and its nested descendants.
pub fn outline_template_node(depth: usize, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .when(depth > 0, |el| {
            el.ml(px(10.0))
                .pl(px(6.0))
                .border_l_1()
                .border_color(theme.border)
        })
}

/// Proposal content row in item details.
pub fn outline_proposal_row(selector: String, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(4.0 * scale))
        .pl(px(14.0))
        .py(px(2.0 * scale))
}

/// Proposal provenance metadata line.
pub fn outline_proposal_provenance(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
}

/// Waiting-date section and its label.
pub fn outline_waiting_section(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-waiting-control".into())
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .child(
            div()
                .text_color(theme.text_muted)
                .text_size(crate::typography::ui_rems(
                    11.0 + super::layout_type_adjustment(theme),
                ))
                .when(super::is_area(theme), |el| {
                    el.text_size(crate::typography::ui_rems(
                        theme.ui_style.tokens.body_size.unwrap_or(13.0)
                            + super::layout_type_adjustment(theme),
                    ))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .child("Waiting until…"),
        )
}

/// Preserve the original Default Minimal footer while sizing Archive in other appearances.
pub fn outline_details_footer(archived: bool, theme: &Theme) -> Div {
    let original = theme.ui_style.id == crate::ui_style::UiStyleId::Default
        && theme.ui_style.layout == crate::ui_style::UiLayout::Minimal;
    div().when(!archived && !original, |el| el.flex().items_start())
}

/// Artifact count marker in an outline row.
pub fn outline_artifact_count(id: SharedString, count: usize, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(crate::icons::icon(crate::icons::DOCUMENT).size(px(10.0)))
        .child(SharedString::from(count.to_string()))
}

/// Instruction drift action in an outline row.
pub fn outline_drift_button(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                .size(px(10.0))
                .text_color(theme.warning),
        )
        .child("Instructions changed")
}

/// Proposal count action in an outline row.
pub fn outline_proposal_button(id: SharedString, count: usize, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * scale))
        .px(px(3.0 * scale))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            crate::icons::icon(crate::icons::DOCUMENT)
                .size(px(10.0))
                .text_color(theme.text_muted),
        )
        .child(SharedString::from(format!("{count} proposed")))
}

/// One segment of an outline tree guide.
pub fn outline_guide_segment(top: f32, bottom: f32, color: gpui::Hsla) -> Div {
    div()
        .absolute()
        .left(px(3.0))
        .top(px(top))
        .w(px(1.0))
        .h(px(bottom - top))
        .bg(color)
}

/// Horizontal arm of an outline tree guide.
pub fn outline_guide_arm(selector: String, top: f32, width: f32, color: gpui::Hsla) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(3.0))
        .top(px(top))
        .w(px(width))
        .h(px(1.0))
        .bg(color)
}

/// Hit target containing the segments of a tree guide.
pub fn outline_guide_cell(id: SharedString, x: f32, height: f32) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(x - 3.0))
        .top(px(-1.0))
        .w(px(7.0))
        .h(px(height))
}

/// Insertion line shown while moving an outline row.
pub fn outline_drop_line(selector: String, left: f32, before: bool, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(left))
        .right(px(4.0))
        .h(px(2.0))
        .rounded(px(1.0))
        .bg(theme.accent)
        .when(before, |el| el.top(px(-1.0)))
        .when(!before, |el| el.bottom(px(-1.0)))
}

/// Label shown beside an outline drop indicator.
pub fn outline_drop_label(text: String, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "outline-drop-label".into())
        .absolute()
        .right(px(6.0))
        .top(px(2.0))
        .max_w(px(220.0))
        .px(px(5.0))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.surface_raised)
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .truncate()
        .child(SharedString::from(text))
}

/// Transient highlight after moving a row.
pub fn outline_move_flash(selector: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .inset_0()
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .bg(theme.accent_wash)
}

/// Metadata row beneath a required action.
pub fn outline_request_attention(theme: &Theme, color: gpui::Hsla) -> gpui::Hsla {
    if super::is_area(theme) {
        theme.text_muted
    } else {
        color
    }
}

pub fn outline_action_metadata(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .min_w_0()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
}

/// A linked target on a required action.
pub fn outline_request_link(id: SharedString, label: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .px(px(6.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .text_color(theme.accent)
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_area(theme), |el| {
            el.rounded_full()
                .border_color(theme.bg)
                .text_color(theme.text_muted)
        })
        .child(SharedString::from(label))
}

/// Artifact link on a required action.
pub fn outline_artifact_link(id: SharedString, label: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(220.0))
        .flex()
        .items_center()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .hover(|el| el.bg(theme.element_hover))
        .child(crate::icons::icon(crate::icons::DOCUMENT).size(px(10.0)))
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}

/// Heading for a Needs-you group.
pub fn outline_needs_heading(
    label: &'static str,
    count: usize,
    color: gpui::Hsla,
    theme: &Theme,
) -> Div {
    let displayed = if super::is_industrial(theme) {
        crate::popover::tracked_upper(label)
    } else {
        label.to_owned()
    };
    div()
        .debug_selector(move || format!("required-actions-group-{label}"))
        .px(px(6.0))
        .pt(px(6.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if super::is_area(theme) {
            theme.text_muted
        } else {
            color
        })
        .child(SharedString::from(displayed))
        .child(SharedString::from(format!("· {count}")))
}

/// Command approval card in Needs-you.
pub fn outline_approval_card(selector: String, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(4.0 * scale))
        .px(px(6.0 * scale))
        .py(px(4.0 * scale))
        .border_1()
        .border_color(theme.border)
        .when(super::is_industrial(theme), |el| {
            el.bg(theme.industrial_colors().panel)
                .border_color(theme.industrial_colors().rule)
        })
        .when(super::is_area(theme), |el| {
            el.rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(12.0)))
                .border_color(theme.bg)
                .bg(theme.bg)
                .py(px(8.0 * scale))
        })
}

/// Proposal section in item details.
pub fn outline_proposal_section(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-proposals".into())
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

/// Collapsed proposal history control.
pub fn outline_proposal_history(count: usize, open: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-proposals-history-toggle")
        .debug_selector(|| "work-item-proposals-history-toggle".into())
        .pl(px(14.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(SharedString::from(format!(
            "{} History ({count})",
            if open { "▾" } else { "▸" }
        )))
}

/// Indented explanation or error inside a details section.
pub fn outline_section_message(text: String, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .pl(px(14.0))
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .child(SharedString::from(text))
}

/// Blueprint list section.
pub fn outline_blueprints_section(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "space-templates".into())
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .child(outline_list_heading("Blueprints".into(), theme))
}

/// Explanation in a blueprint section.
pub fn outline_blueprint_explanation(text: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(text)
}

/// Blueprint list row.
pub fn outline_blueprint_row(selector: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
}

/// Expandable blueprint label.
pub fn outline_blueprint_toggle(
    id: impl Into<gpui::ElementId>,
    label: String,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .truncate()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.text_color(theme.text_muted))
        .child(SharedString::from(label))
}

/// Blueprint preview container and one line of its tree.
pub fn outline_blueprint_preview(selector: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(super::layout_spacing(theme)))
        .pl(px(12.0))
}
pub fn outline_blueprint_line(depth: usize, text: String, dim: bool, theme: &Theme) -> Div {
    div()
        .pl(px(12.0 * depth as f32))
        .truncate()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(if dim { theme.text_muted } else { theme.text })
        .child(SharedString::from(text))
}

/// Inline word diff for an instruction proposal.
pub(crate) fn outline_proposal_diff(
    runs: &[(crate::work_outline::proposal_diff::DiffKind, String)],
    selector: String,
    theme: &Theme,
) -> gpui::AnyElement {
    let (text, ranges) = crate::work_outline::proposal_diff::diff_layout(runs);
    let added = gpui::HighlightStyle {
        background_color: Some(theme.diff_add.opacity(0.22)),
        ..Default::default()
    };
    let removed = gpui::HighlightStyle {
        color: Some(theme.diff_del),
        strikethrough: Some(gpui::StrikethroughStyle {
            thickness: px(1.0),
            color: Some(theme.diff_del),
        }),
        ..Default::default()
    };
    let highlights = ranges.into_iter().map(|(range, kind)| {
        (
            range,
            if kind == crate::work_outline::proposal_diff::DiffKind::Added {
                added
            } else {
                removed
            },
        )
    });
    div()
        .debug_selector(move || selector.clone())
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(gpui::StyledText::new(text).with_highlights(highlights))
        .into_any_element()
}

/// Secondary explanatory text in a details panel.
pub fn outline_secondary_text(text: String, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(text))
}

/// Horizontal group of details actions.
pub fn outline_action_row(theme: &Theme) -> Div {
    div().flex().gap(px(6.0 * super::layout_spacing(theme)))
}

/// Section column with a selector for the host surface.
pub fn outline_section(selector: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

/// Heading with room for a trailing action.
pub fn outline_section_action_heading(label: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .when(super::is_area(theme), |el| {
            el.text_size(crate::typography::ui_rems(
                theme.ui_style.tokens.body_size.unwrap_or(13.0)
                    + super::layout_type_adjustment(theme),
            ))
        })
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .flex()
        .items_center()
        .justify_between()
        .child(label)
}

/// Vertical stack of item detail content.
pub fn outline_detail_section(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

/// Horizontal row of item detail content.
pub fn outline_detail_row(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
}

/// Section label that fills the available row width.
pub fn outline_detail_heading(label: &'static str, theme: &Theme) -> Div {
    div()
        .flex_1()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .when(super::is_area(theme), |el| {
            el.text_size(crate::typography::ui_rems(
                theme.ui_style.tokens.body_size.unwrap_or(13.0)
                    + super::layout_type_adjustment(theme),
            ))
        })
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(label)
}

/// Truncated primary detail in a row.
pub fn outline_detail_row_text(text: String, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(text))
}

/// Small action with a bounded label in an item detail row.
pub fn outline_small_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    super::button(
        id,
        label,
        super::ButtonVariant::Secondary,
        super::ButtonSize::Sm,
        theme,
    )
    .max_w(px(180.0))
    .truncate()
}

/// Interactive artifact row in item details.
pub fn outline_artifact_row(id: SharedString, open: bool, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(6.0 * scale))
        .px(px(4.0 * scale))
        .py(px(2.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .when(open, |el| el.bg(theme.element_hover))
}

/// Artifact title and compact metadata.
pub fn outline_artifact_title(title: String, theme: &Theme) -> Div {
    div()
        .min_w_0()
        .flex_1()
        .truncate()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(SharedString::from(title))
}
pub fn outline_artifact_detail(detail: String, theme: &Theme) -> Div {
    div()
        .flex_none()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(SharedString::from(detail))
}
pub fn outline_plan_badge(theme: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child("Plan")
}

/// Inheritance checkbox in item instructions.
pub fn outline_inheritance_toggle(inherit: bool, theme: &Theme) -> Stateful<Div> {
    let check = div()
        .size(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.border_strong)
        .when(inherit, |el| {
            el.child(
                crate::icons::icon(crate::icons::CHECK)
                    .size(px(9.0))
                    .text_color(theme.text),
            )
        });
    div()
        .id("work-item-inherit-toggle")
        .debug_selector(|| "work-item-inherit-toggle".into())
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(check)
        .child("Inherit parent instructions")
}

/// Instruction card and its action header.
pub fn outline_instruction_row(index: usize, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || format!("work-item-instruction-{index}"))
        .flex()
        .flex_col()
        .gap(px(2.0 * scale))
        .px(px(4.0 * scale))
        .py(px(3.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
}
pub fn outline_instruction_header(index: usize, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("work-item-instruction-header-{index}"))
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
}

/// Effective instruction entry and provenance marker.
pub fn outline_effective_entry(index: usize, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || format!("work-item-effective-entry-{index}"))
        .flex()
        .flex_col()
        .gap(px(scale))
        .px(px(4.0 * scale))
        .py(px(2.0 * scale))
}
pub fn outline_effective_marker(index: usize, local: bool, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("work-item-effective-marker-{index}"))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .when(local, |el| {
            el.bg(theme.accent_wash).text_color(theme.accent)
        })
        .when(!local, |el| {
            el.bg(theme.element_hover.opacity(0.7))
                .text_color(theme.text_muted)
        })
        .child(if local { "Local" } else { "Inherited" })
}
pub fn outline_effective_text(text: String, theme: &Theme) -> Div {
    div()
        .pl(px(14.0))
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(SharedString::from(text))
}

/// One line in an execution configuration summary.
pub fn outline_execution_line(text: String, size: f32, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            size + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .child(SharedString::from(text))
}

/// Repository binding row in an execution summary.
pub fn outline_execution_repository(
    index: usize,
    name: String,
    summary: String,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("work-item-execution-repo", index))
        .debug_selector(move || format!("work-item-execution-repo-{index}"))
        .flex()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .child(
            div()
                .flex_none()
                .text_color(theme.text)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.text_muted)
                .child(SharedString::from(summary)),
        )
}

/// Checkbox or radio mark for execution settings.
pub fn outline_checkbox(checked: bool, round: bool, theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(if round {
            6.0
        } else {
            theme.ui_style.tokens.control_radius.unwrap_or(3.0)
        }))
        .border_1()
        .border_color(theme.border_strong)
        .when(checked && round, |el| {
            el.child(div().size(px(6.0)).rounded_full().bg(theme.accent))
        })
        .when(checked && !round, |el| {
            el.child(
                crate::icons::icon(crate::icons::CHECK)
                    .size(px(9.0))
                    .text_color(theme.text),
            )
        })
}
pub fn outline_muted_line(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// Repository binding editor card.
pub fn outline_binding_card(index: usize, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || format!("work-item-execution-binding-{index}"))
        .flex()
        .flex_col()
        .gap(px(3.0 * scale))
        .p(px(6.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
}

/// Checkout choice in a binding editor.
pub fn outline_binding_choice(
    index: usize,
    choice: usize,
    label: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = format!("work-item-execution-worktree-{index}-{choice}");
    let button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .px(px(5.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(if active { theme.accent } else { theme.border })
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(if active { theme.text } else { theme.text_muted })
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(button, theme, active)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(SharedString::from(label))
}

/// Count of execution exceptions on an item.
pub fn outline_execution_exception_marker(count: usize, theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-execution-exceptions-marker")
        .debug_selector(|| "work-item-execution-exceptions-marker".into())
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(
            crate::icons::icon(crate::icons::FOLDER_WITH_FILES)
                .size(px(10.0))
                .text_color(theme.accent),
        )
        .child(SharedString::from(format!(
            "{count} exception{}",
            if count == 1 { "" } else { "s" }
        )))
}

/// A blueprint review change row.
pub fn outline_template_review_row(
    selector: String,
    sign: &'static str,
    text: String,
    theme: &Theme,
) -> Div {
    outline_detail_row(theme)
        .debug_selector(move || selector.clone())
        .child(
            div()
                .flex_none()
                .w(px(10.0))
                .text_size(crate::typography::ui_rems(
                    10.5 + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text_muted)
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(crate::typography::ui_rems(
                    10.5 + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text)
                .child(SharedString::from(text)),
        )
}
pub fn outline_template_review_status(status: SharedString, theme: &Theme) -> Div {
    div()
        .flex_none()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(status)
}

/// Selectable blueprint in the conversion flow.
pub fn outline_conversion_choice(
    index: usize,
    selector: String,
    label: String,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let button = div()
        .id(("convert-template", index))
        .debug_selector(move || selector.clone())
        .px(px(6.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(if selected { theme.accent } else { theme.border })
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(button, theme, selected)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(SharedString::from(label))
}

/// Child placement control in the conversion flow.
pub fn outline_conversion_child(index: usize, label: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("convert-child", index))
        .debug_selector(move || format!("convert-child-{index}"))
        .cursor_pointer()
        .truncate()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.text_color(theme.text_muted))
        .child(SharedString::from(label))
}

/// Conversion preview lines.
pub fn outline_conversion_preview(lines: Vec<String>, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "convert-preview".into())
        .flex()
        .flex_col()
        .gap(px(super::layout_spacing(theme)))
        .children(
            lines
                .into_iter()
                .map(|line| outline_execution_line(format!("• {line}"), 10.5, theme.text, theme)),
        )
}

/// Row in an item-details list.
pub fn outline_details_list_row(
    id: impl Into<gpui::ElementId>,
    height: f32,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .h(px(height * scale))
        .flex()
        .items_center()
        .gap(px(6.0 * scale))
        .px(px(4.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .hover(|el| el.bg(theme.element_hover))
}

/// Open-chat label in an item-details list.
pub fn outline_details_chat_label(
    index: usize,
    label: String,
    missing: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("work-item-chat-open", index))
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(if missing && theme.appearance.is_dark() {
            theme.text_muted
        } else if missing {
            theme.text_faint
        } else {
            theme.text
        })
        .when(missing && !super::is_area(theme), |el| el.italic())
        .when(!missing, |el| el.cursor_pointer())
        .child(SharedString::from(label))
}

/// Selectable chat in the association list.
pub fn outline_associate_chat(index: usize, title: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("work-item-associate", index))
        .debug_selector(move || format!("work-item-associate-{index}"))
        .h(px(22.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .truncate()
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(SharedString::from(title))
}

/// Child item title in a details list.
pub fn outline_child_title(title: String, theme: &Theme) -> Div {
    div()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(SharedString::from(title))
}

/// Selectable worker destination on a project.
pub fn outline_destination_choice(
    index: usize,
    title: String,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let choice = div()
        .id(("work-item-worker-destination", index))
        .px(px(6.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(if selected { theme.accent } else { theme.border })
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(choice, theme, selected)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(SharedString::from(title))
}

/// External reference row and link.
pub fn outline_reference_row(theme: &Theme) -> Div {
    div()
        .h(px(22.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
}
pub fn outline_reference_link(index: usize, label: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("work-item-reference", index))
        .flex_1()
        .min_w_0()
        .truncate()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(SharedString::from(label))
}

/// Compact list in an item-details section.
pub fn outline_details_list(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
}

/// Expand/collapse control in an outline row.
pub fn outline_disclosure(id: SharedString, collapsed: bool, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .size(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(if collapsed {
                crate::icons::ALT_ARROW_RIGHT
            } else {
                crate::icons::ALT_ARROW_DOWN
            })
            .size(px(10.0))
            .text_color(theme.text_muted)
            .when(super::is_area(theme), |el| {
                el.opacity(0.65)
                    .group_hover("outline-row", |el| el.opacity(1.0))
            }),
        )
}
pub fn outline_disclosure_space() -> Div {
    div().flex_none().size(px(14.0))
}

/// Small inline action in an outline row.
pub fn outline_inline_action(
    id: SharedString,
    selector: String,
    label: &'static str,
    color: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text));
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(label)
}

/// Waiting date displayed in an outline row.
pub fn outline_waiting_chip(label: String, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-waiting-chip".into())
        .flex_none()
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(label))
}

/// Icon and count in an outline row.
pub fn outline_count_marker(
    id: SharedString,
    icon: &'static str,
    count: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .child(crate::icons::icon(icon).size(px(10.0)).text_color(color))
        .child(SharedString::from(count))
}

/// Compact glyph marker in an outline row.
pub fn outline_glyph_marker(id: SharedString, glyph: &'static str, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(glyph)
}

/// External reference marker in an outline row.
pub fn outline_reference_marker(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .child(
            crate::icons::icon(crate::icons::GLOBAL)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
}

/// Harness badge on an outline chat row.
pub fn outline_harness_badge(harness: String, theme: &Theme) -> Div {
    div()
        .flex_none()
        .max_w(px(100.0))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .bg(theme.element_hover.opacity(0.7))
        .truncate()
        .text_size(crate::typography::ui_rems(
            9.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted.opacity(0.85))
        .child(SharedString::from(harness))
}

/// Branch or pull-request marker on an outline chat row.
pub fn outline_chat_reference(
    icon: &'static str,
    label: String,
    max_width: Option<f32>,
    theme: &Theme,
) -> Div {
    div()
        .flex_none()
        .when_some(max_width, |el, width| el.max_w(px(width)))
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(
            crate::icons::icon(icon)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
        .when(max_width.is_some(), |el| {
            el.child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(label.clone())),
            )
        })
        .when(max_width.is_none(), |el| {
            el.child(SharedString::from(label))
        })
}

/// Warning chip for a Linear link requiring repair.
pub fn outline_linear_fix(id: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child("Fix link")
}

/// Linked Linear issue chip.
pub fn outline_linear_issue(
    id: String,
    text: String,
    dot: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(180.0))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
        .child(div().min_w_0().truncate().child(SharedString::from(text)))
}

/// Linked Linear project chip.
pub fn outline_linear_project(id: String, name: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(140.0))
        .flex()
        .items_center()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
        .child(div().min_w_0().truncate().child(SharedString::from(name)))
}

/// Linear destination and retained-item markers.
pub fn outline_linear_destination(id: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .px(px(3.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(10.0))
                .text_color(theme.accent),
        )
        .child("↓")
}
pub fn outline_linear_retained(id: String, text: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(160.0))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.warning)
        .child(div().truncate().child(SharedString::from(text)))
}

/// Heading for an overview list lane.
pub fn outline_lane_heading(text: String, color: gpui::Hsla, depth: usize, theme: &Theme) -> Div {
    let top = depth == 0;
    let label = if top && !super::is_area(theme) {
        text.to_uppercase()
    } else {
        text
    };
    div()
        .flex_none()
        .text_color(color)
        .border_color(color)
        .when(top, |el| {
            el.text_size(crate::typography::ui_rems(
                13.0 + super::layout_type_adjustment(theme),
            ))
            .font_weight(gpui::FontWeight::BOLD)
            .pb(px(6.0 * super::layout_spacing(theme)))
            .border_b_2()
        })
        .when(!top, |el| {
            el.text_size(crate::typography::ui_rems(
                10.0 + super::layout_type_adjustment(theme),
            ))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .pb(px(3.0 * super::layout_spacing(theme)))
            .border_b_1()
        })
        .child(SharedString::from(label))
}

/// Required Actions panel shell.
pub fn outline_required_panel(theme: &Theme) -> Stateful<Div> {
    div()
        .id("required-actions-panel")
        .debug_selector(|| "required-actions-panel".into())
        .flex_none()
        .w_full()
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.border)
        .bg(if super::is_area(theme) {
            theme.bg
        } else {
            theme.surface
        })
        .when(super::is_area(theme), |el| el.border_color(theme.bg))
        .when(super::is_industrial(theme), |el| {
            el.border_l_2().border_color(theme.industrial_colors().rule)
        })
}
pub fn outline_required_close(theme: &Theme) -> Stateful<Div> {
    div()
        .id("required-actions-close")
        .debug_selector(|| "required-actions-close".into())
        .flex_none()
        .size(px(22.0))
        .when(super::is_area(theme), |el| {
            el.h(px(theme.ui_style.tokens.control_size))
        })
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::CLOSE)
                .size(px(12.0))
                .text_color(theme.text_muted),
        )
        .when(
            theme.ui_style.behavior.labels == crate::ui_style::LabelMode::All,
            |el| el.w_auto().px(px(6.0)).gap(px(4.0)).child("Close"),
        )
}
pub fn outline_required_header(count: usize, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(14.0))
        .pt(px(10.0))
        .pb(px(6.0 * super::layout_spacing(theme)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(crate::typography::ui_rems(
                    15.0 + super::layout_type_adjustment(theme),
                ))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child("Required Actions"),
        )
        .when(count > 0, |el| {
            el.child(
                div()
                    .debug_selector(|| "required-actions-count".into())
                    .flex_none()
                    .text_size(crate::typography::ui_rems(
                        11.0 + super::layout_type_adjustment(theme),
                    ))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(SharedString::from(count.to_string())),
            )
        })
}
pub fn outline_required_body(theme: &Theme) -> Stateful<Div> {
    div()
        .id("required-actions-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(8.0 * super::layout_spacing(theme)))
        .pb(px(10.0 * super::layout_spacing(theme)))
}

/// Inline reply input on a required-action card.
pub fn outline_reply_input(
    input: gpui::Entity<crate::composer::ComposerInput>,
    theme: &Theme,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .child(input)
}

/// Note input for resolving a required action.
pub fn outline_resolve_input(
    input: gpui::Entity<crate::composer::ComposerInput>,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(|| "required-actions-note".into())
        .w_full()
        .min_h(px(24.0))
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.surface)
        .child(input)
}

/// Provenance badges in an instruction header.
pub fn outline_instruction_template_badge(index: usize, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("work-item-instruction-template-{index}"))
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child("From child template")
}
pub fn outline_instruction_provenance(
    index: usize,
    label: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("work-item-instruction-badge", index))
        .debug_selector(move || format!("work-item-instruction-badge-{index}"))
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .bg(theme.element_hover.opacity(0.7))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(label)
}

/// Instruction content, fold control, and source excerpt.
pub fn outline_instruction_text(
    index: usize,
    text: String,
    line_height: f32,
    collapsed_height: Option<f32>,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || format!("work-item-instruction-text-{index}"))
        .ml(px(14.0))
        .min_w_0()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .line_height(crate::typography::ui_rems(line_height))
        .text_color(theme.text)
        .when_some(collapsed_height, |el, height| {
            el.max_h(crate::typography::ui_rems(height))
                .overflow_hidden()
        })
        .child(SharedString::from(text))
}
pub fn outline_instruction_fold(index: usize, expanded: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("work-item-instruction-fold", index))
        .debug_selector(move || format!("work-item-instruction-fold-{index}"))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(if expanded { "Show less" } else { "Show more" })
}
pub fn outline_instruction_excerpt(index: usize, excerpt: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("work-item-instruction-excerpt-{index}"))
        .ml(px(14.0))
        .pl(px(6.0))
        .border_l_2()
        .border_color(theme.border_strong)
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .italic()
        .child(SharedString::from(excerpt))
}

/// Summary of the instruction a proposal group changes.
pub fn outline_proposal_summary(
    selector: String,
    count: usize,
    current: String,
    theme: &Theme,
) -> Div {
    outline_detail_section(theme)
        .debug_selector(move || selector.clone())
        .pl(px(14.0))
        .child(outline_secondary_text(
            if count > 1 {
                format!("Instruction · {count} proposed changes")
            } else {
                "Instruction".into()
            },
            theme,
        ))
        .child(
            div()
                .max_h(px(54.0))
                .overflow_hidden()
                .text_size(crate::typography::ui_rems(
                    11.5 + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text_faint)
                .child(SharedString::from(current)),
        )
}
pub fn outline_proposal_rationale(selector: String, rationale: String, theme: &Theme) -> Div {
    outline_secondary_text(format!("Why: {rationale}"), theme)
        .debug_selector(move || selector.clone())
        .italic()
}

/// Body of a proposal modification.
pub fn outline_proposal_body(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0 * super::layout_spacing(theme)))
}

/// Instruction revision diff row.
fn code_size(theme: &Theme) -> f32 {
    crate::typography::clamp_font_size(theme.code_font_size * 0.92)
}

pub(crate) fn outline_revision_line(
    line: &crate::work_outline::revision_diff::PresentedLine,
    theme: &Theme,
) -> gpui::AnyElement {
    use crate::work_outline::revision_diff::PresentedLine;
    use mast_proto::TextDiffKind;
    let size = code_size(theme);
    let height = (size * 1.55).round();
    let PresentedLine::Line {
        kind,
        old_no,
        new_no,
        text,
    } = line
    else {
        let PresentedLine::Folded(count) = line else {
            unreachable!()
        };
        return div()
            .h(px(height))
            .pl(px(12.0))
            .flex()
            .items_center()
            .text_size(crate::typography::ui_rems(
                10.5 + super::layout_type_adjustment(theme),
            ))
            .text_color(theme.text_faint)
            .child(SharedString::from(format!(
                "\u{22ef} {count} unchanged line{}",
                if *count == 1 { "" } else { "s" }
            )))
            .into_any_element();
    };
    let (marker, color) = match kind {
        TextDiffKind::Added => ("+", Some(theme.diff_add)),
        TextDiffKind::Removed => ("\u{2212}", Some(theme.diff_del)),
        TextDiffKind::Same => ("\u{b7}", None),
    };
    let gutter = |no: Option<u32>| {
        div()
            .w(px(26.0))
            .flex_none()
            .flex()
            .justify_end()
            .pr(px(6.0))
            .text_color(color.map_or(theme.text_faint.opacity(0.8), |c| c.opacity(0.9)))
            .child(SharedString::from(
                no.map(|n| n.to_string()).unwrap_or_default(),
            ))
    };
    div()
        .min_h(px(height))
        .w_full()
        .flex()
        .items_start()
        .font_family(theme.font_mono.clone())
        .text_size(px(size))
        .line_height(px(height))
        .when_some(color, |el, color| {
            let mut wash = color;
            wash.a = 0.055;
            el.bg(wash)
        })
        .child(
            div()
                .w(px(2.0))
                .h(px(height))
                .flex_none()
                .when_some(color, |el, color| el.bg(color.opacity(0.55))),
        )
        .child(gutter(*old_no))
        .child(gutter(*new_no))
        .child(
            div()
                .w(px(12.0))
                .flex_none()
                .text_color(color.unwrap_or(theme.text_faint.opacity(0.5)))
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme.text.opacity(0.92))
                .child(SharedString::from(text.clone())),
        )
        .into_any_element()
}

/// Container and overflow notice for an instruction revision diff.
pub fn outline_revision_frame(selector: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.into())
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
}
pub fn outline_revision_overflow(hidden: usize, theme: &Theme) -> Div {
    div()
        .px(px(12.0))
        .py(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(SharedString::from(format!("{hidden} more lines not shown")))
}

/// Entry-level summary in an instruction revision comparison.
pub fn outline_revision_summary(
    selector: String,
    label: String,
    rows: Vec<(Option<mast_proto::TextDiffKind>, String)>,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .child(outline_list_heading(label, theme))
        .children(rows.into_iter().map(|(kind, text)| {
            let color = match kind {
                Some(mast_proto::TextDiffKind::Added) => theme.diff_add,
                Some(mast_proto::TextDiffKind::Removed) => theme.diff_del,
                _ => theme.text,
            };
            div()
                .pl(px(8.0))
                .text_size(crate::typography::ui_rems(
                    11.5 + super::layout_type_adjustment(theme),
                ))
                .text_color(color)
                .child(SharedString::from(text))
        }))
}
pub fn outline_revision_comparison(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "instruction-comparison".into())
        .flex()
        .flex_col()
        .gap(px(10.0 * super::layout_spacing(theme)))
}

/// Execution validation results container.
pub fn outline_validation_panel(theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(|| "work-item-execution-validation".into())
        .flex()
        .flex_col()
        .gap(px(3.0 * scale))
        .p(px(6.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .bg(theme.element_hover.opacity(0.4))
}
pub fn outline_validation_headline(
    kind: &'static str,
    text: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> Div {
    outline_execution_line(text, 11.5, color, theme)
        .debug_selector(move || format!("work-item-execution-validation-{kind}"))
        .font_weight(gpui::FontWeight::SEMIBOLD)
}
pub fn outline_validation_issue(
    index: usize,
    message: String,
    remediation: String,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || format!("work-item-execution-issue-{index}"))
        .flex()
        .flex_col()
        .child(outline_execution_line(message, 11.0, theme.text, theme))
        .child(outline_execution_line(
            remediation,
            10.5,
            theme.text_muted,
            theme,
        ))
}

/// Execution exceptions list and row.
pub fn outline_exceptions_section(theme: &Theme) -> Div {
    outline_details_list(theme)
        .debug_selector(|| "work-item-execution-exceptions".into())
        .child(outline_list_heading("Exceptions".into(), theme))
}
pub fn outline_exception_row(
    index: usize,
    title: String,
    summary: String,
    missing: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("work-item-execution-exception", index))
        .debug_selector(move || format!("work-item-execution-exception-{index}"))
        .flex()
        .flex_col()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .hover(|el| el.bg(theme.element_hover))
        .when(!missing, |el| el.cursor_pointer())
        .child(
            outline_action_row(theme)
                .child(
                    outline_execution_line(title, 12.0, theme.text, theme)
                        .min_w_0()
                        .truncate(),
                )
                .child(
                    outline_execution_line(
                        "uses a different configuration".into(),
                        10.5,
                        theme.text_muted,
                        theme,
                    )
                    .flex_none(),
                ),
        )
        .child(outline_execution_line(summary, 10.5, theme.text_faint, theme).truncate())
}

/// Repository binding heading.
pub fn outline_binding_heading(name: String, path: String, theme: &Theme) -> Div {
    outline_detail_row(theme)
        .child(
            outline_execution_line(name, 12.0, theme.text, theme)
                .flex_none()
                .font_weight(gpui::FontWeight::SEMIBOLD),
        )
        .child(
            outline_execution_line(path, 10.5, theme.text_faint, theme)
                .flex_1()
                .min_w_0()
                .truncate(),
        )
}
pub fn outline_binding_toggle_group(theme: &Theme) -> Div {
    div()
        .flex()
        .gap(px(12.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
}
pub fn outline_binding_toggle(
    index: usize,
    primary: bool,
    checked: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (id, label) = if primary {
        ("work-item-execution-primary", "Primary")
    } else {
        ("work-item-execution-member", "Member")
    };
    div()
        .id((id, index))
        .debug_selector(move || format!("{id}-{index}"))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .hover(|el| el.text_color(theme.text))
        .child(outline_checkbox(checked, primary, theme))
        .child(label)
}
pub fn outline_binding_pinned(label: String, theme: &Theme) -> Div {
    div()
        .px(px(5.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.accent)
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(SharedString::from(label))
}

/// Expandable friction report heading.
pub fn outline_friction_heading(
    index: usize,
    icon: &'static str,
    summary: String,
    count_selector: String,
    count: u64,
    age: String,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("friction-row", index))
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .child(crate::icons::icon(icon).size(px(12.0)))
        .child(div().flex_1().min_w_0().child(SharedString::from(summary)))
        .child(
            div()
                .debug_selector(move || count_selector.clone())
                .child(SharedString::from(format!("×{count}"))),
        )
        .child(
            div()
                .text_color(theme.text_muted)
                .child(SharedString::from(age)),
        )
}
pub fn outline_friction_badge(label: String, theme: &Theme) -> Div {
    div()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .bg(theme.surface_overlay)
        .text_color(theme.text_muted)
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .child(SharedString::from(label))
}
pub fn outline_friction_details(selector: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .pl(px(18.0))
        .flex()
        .flex_col()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
}

/// Destination picker row for PR synchronization.
pub fn outline_pr_destination(
    id: String,
    label: String,
    checked: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .h(px(22.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(if checked {
            theme.text
        } else {
            theme.text_muted
        })
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            div()
                .flex_none()
                .w(px(10.0))
                .text_color(theme.accent)
                .child(if checked { "✓" } else { "" }),
        )
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}
pub fn outline_pr_destination_picker(theme: &Theme) -> Stateful<Div> {
    div()
        .id("space-pr-sync-picker")
        .debug_selector(|| "space-pr-sync-picker".into())
        .max_h(px(220.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .pl(px(8.0 * super::layout_spacing(theme)))
}

/// Enable-Linear suggestion in item details.
pub fn outline_linear_hint(theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(|| "work-item-linear-hint".into())
        .flex()
        .items_center()
        .gap(px(6.0 * scale))
        .px(px(8.0 * scale))
        .py(px(6.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .child(
            outline_execution_line(
                "Linear link saved as a plain link. Enable Linear for this Space?".into(),
                11.5,
                theme.text_muted,
                theme,
            )
            .flex_1(),
        )
}

/// Marker for a field still synchronized from Linear.
pub fn outline_linear_sync_marker(id: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child("Synced from Linear")
}

/// Inline artifact menu and its actions.
pub fn outline_artifact_menu(theme: &Theme) -> Div {
    div()
        .ml(px(18.0))
        .p(px(3.0 * super::layout_spacing(theme)))
        .flex()
        .flex_col()
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(5.0)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
}
pub fn outline_artifact_menu_entry(id: &str, label: &'static str, theme: &Theme) -> Stateful<Div> {
    let selector = format!("work-item-artifact-menu-{id}");
    let button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .px(px(6.0 * super::layout_spacing(theme)))
        .py(px(2.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(label)
}

/// Status or explanatory text in item details.
pub fn outline_detail_text(text: String, size: f32, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(
            size + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .child(SharedString::from(text))
}

/// Metadata row above an effective instruction.
pub fn outline_effective_header(index: usize, theme: &Theme) -> Div {
    outline_detail_row(theme)
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .child(
            div()
                .text_color(theme.text_faint)
                .child(SharedString::from(format!("{}.", index + 1))),
        )
}
pub fn outline_effective_source(index: usize, title: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("work-item-effective-source", index))
        .min_w_0()
        .truncate()
        .cursor_pointer()
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(SharedString::from(title))
}

/// Space-root details heading.
pub fn outline_space_header(label: String, theme: &Theme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(14.0))
        .pt(px(10.0))
        .child(
            outline_detail_text(label, 10.5, theme.text_muted, theme)
                .flex_1()
                .min_w_0()
                .truncate(),
        )
}
pub fn outline_space_title(theme: &Theme) -> Div {
    outline_detail_text("Space root".into(), 15.0, theme.text, theme)
        .font_weight(gpui::FontWeight::SEMIBOLD)
}

/// Breadcrumb container in an item-details header.
pub fn outline_breadcrumbs(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-breadcrumbs".into())
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .overflow_hidden()
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
}

/// Effective user-action status beside the item status control.
pub fn outline_details_attention(
    glyph: &'static str,
    label: &'static str,
    color: gpui::Hsla,
    count: u32,
    theme: &Theme,
) -> Div {
    let suffix = if count > 1 {
        format!("{label} ({count})")
    } else {
        label.to_owned()
    };
    div()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(if super::is_area(theme) {
            theme.accent
        } else {
            color
        })
        .when(super::is_area(theme), |el| {
            el.child(SharedString::from(suffix.clone()))
        })
        .when(
            !super::is_area(theme)
                && theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot,
            |el| {
                el.flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(crate::status_style::indicator_for_theme(
                        crate::status_style::Status::Waiting,
                        theme,
                    ))
                    .child(SharedString::from(suffix.clone()))
            },
        )
        .when(
            !super::is_area(theme)
                && theme.ui_style.status_indicator != crate::ui_style::StatusIndicator::Dot,
            |el| el.child(SharedString::from(format!("{glyph} {suffix}"))),
        )
}

/// Editable item description.
pub fn outline_description(text: String, has_text: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-item-description")
        .cursor_text()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(if has_text {
            theme.text
        } else {
            theme.text_faint
        })
        .child(SharedString::from(text))
}

/// Primary text of a Required Action.
pub fn outline_required_text(
    selector: String,
    text: String,
    open: bool,
    expanded: bool,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .flex_1()
        .min_w_0()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(if open { theme.text } else { theme.text_muted })
        .when(open && !expanded, |el| el.line_clamp(2))
        .child(SharedString::from(text))
}
pub fn outline_required_lifecycle(selector: String, label: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .px(px(5.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(label))
}
pub fn outline_required_fact(
    selector: String,
    text: String,
    emphasized: bool,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .pl(px(12.0))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .when(emphasized, |el| {
            el.text_color(theme.text)
                .font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .child(SharedString::from(text))
}
pub fn outline_required_info(selector: String, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .pl(px(12.0))
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
}

/// Outline drag preview.
pub fn outline_drag_preview(title: SharedString, theme: &Theme) -> Div {
    div()
        .max_w(px(260.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_raised)
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .truncate()
        .child(title)
}
pub fn outline_first_use(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "outline-first-use".into())
        .flex_none()
        .mx(px(14.0))
        .my(px(6.0))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child("No work items yet — group Chats and share instructions under one.")
}
pub fn outline_editor_frame(submitting: bool, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(20.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.surface)
        .overflow_hidden()
        .when(submitting, |el| el.opacity(0.6))
}

/// Outline move/associate picker chrome.
pub fn outline_picker(theme: &Theme) -> Stateful<Div> {
    div()
        .id("outline-picker")
        .debug_selector(|| "outline-picker".into())
        .mx(px(8.0))
        .mb(px(6.0))
        .p(px(6.0 * super::layout_spacing(theme)))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_raised)
}
pub fn outline_picker_entry(
    index: usize,
    depth: u32,
    label: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("outline-picker-entry", index))
        .debug_selector(move || format!("outline-picker-entry-{index}"))
        .h(px(24.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .pl(px(8.0 + depth as f32 * 12.0))
        .pr(px(8.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .when(active, |el| el.bg(theme.element_active))
        .hover(|el| el.bg(theme.element_hover))
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}
pub fn outline_picker_place(label: &'static str, active: bool, theme: &Theme) -> Stateful<Div> {
    let selector = format!("outline-picker-place-{}", label.to_lowercase());
    let button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .px(px(6.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.hover(|el| el.bg(theme.element_hover)));
    crate::surface_chrome::segmented_key(button, theme, active)
        .when(super::is_area(theme), |el| el.rounded_full())
        .child(label)
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
