use super::*;

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
        .when(super::is_horizon(theme), |el| {
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
        .when(super::is_horizon(theme), |el| {
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
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
        .when(missing && !super::is_horizon(theme), |el| el.italic())
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
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
