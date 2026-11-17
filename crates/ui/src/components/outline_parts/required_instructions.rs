use super::*;

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
        .bg(if super::is_horizon(theme) {
            theme.bg
        } else {
            theme.surface
        })
        .when(super::is_horizon(theme), |el| el.border_color(theme.bg))
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
        .when(super::is_horizon(theme), |el| {
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
