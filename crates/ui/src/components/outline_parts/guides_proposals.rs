use super::*;

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
    if super::is_horizon(theme) {
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
        .when(super::is_horizon(theme), |el| {
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
        .text_color(if super::is_horizon(theme) {
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
        .when(super::is_horizon(theme), |el| {
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
