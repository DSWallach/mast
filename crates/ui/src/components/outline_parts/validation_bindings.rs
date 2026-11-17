use super::*;

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
        .when(super::is_horizon(theme), |el| el.rounded_full())
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
        .text_color(if super::is_horizon(theme) {
            theme.accent
        } else {
            color
        })
        .when(super::is_horizon(theme), |el| {
            el.child(SharedString::from(suffix.clone()))
        })
        .when(
            !super::is_horizon(theme)
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
            !super::is_horizon(theme)
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
