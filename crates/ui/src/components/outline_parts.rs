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
        .bg(theme.surface)
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
    div()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .when(super::is_industrial(theme), |el| {
            el.font_family(theme.font_mono.clone())
        })
        .child(SharedString::from(label))
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
}

/// Row chrome; selection, drag handling and content remain with the caller.
pub fn outline_row(id: SharedString, height: f32, indent: f32, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
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
        .border_color(gpui::transparent_black())
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
    crate::surface_chrome::segmented_key(trigger, theme, false)
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
        .when(super::is_area(theme), |el| {
            el.bg(theme.identity_color("").opacity(0.06))
        })
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
        .when(super::is_area(theme), |el| el.rounded_full())
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
        .text_color(color)
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
                .bg(theme.identity_color("").opacity(0.06))
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
