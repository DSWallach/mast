//! Presentation primitives for Linear surfaces.

use crate::theme::Theme;
use gpui::{SharedString, div, prelude::*, px};
use mast_proto::linear::LinearIssueSummary;

pub(crate) fn space_chip(multiple: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let chip = div()
        .id("linear-panel-space")
        .debug_selector(|| "linear-panel-space".into())
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().label.unwrap_or(11.0)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted);
    if multiple {
        let chip = chip
            .px(px(4.0))
            .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
            .cursor_pointer()
            .hover(|el| el.text_color(theme.text).bg(theme.element_hover));
        super::toolbar_control(chip, theme, false)
    } else {
        chip
    }
}

pub(crate) fn section_header(
    selector: String,
    label: String,
    open: bool,
    focused: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let header = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .mt(px(6.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .px(px(2.0))
        .py(px(2.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().label.unwrap_or(11.0)
                + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .when(focused, |el| {
            el.bg(theme.element_hover).text_color(theme.text)
        })
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
        .child(SharedString::from(label));
    if super::is_horizon(theme) {
        super::toolbar_control(header, theme, false)
    } else {
        header
    }
}

/// Action in Linear connection settings.
pub(crate) fn settings_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let action = div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .px(px(12.0 * super::layout_spacing(theme)))
        .py(px(5.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().body.unwrap_or(12.0) + super::layout_type_adjustment(theme),
        ))
        .when(primary, |el| el.bg(theme.element_active))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover));
    super::toolbar_control(action, theme, primary).child(label)
}

/// The issue text block every issue row shows: identifier + title, then the
/// state dot and state · priority · assignee · project.
pub(crate) fn issue_summary(issue: &LinearIssueSummary, theme: &Theme) -> gpui::Div {
    let state_color = crate::linear::hex_color(&issue.state.color, theme.text_muted);
    let mut meta = vec![issue.state.name.clone()];
    meta.extend(crate::linear::priority_text(
        issue.priority,
        &issue.priority_label,
    ));
    meta.extend(issue.assignee.as_ref().map(crate::linear::user_name));
    meta.extend(issue.project.as_ref().map(|p| p.name.clone()));
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().body.unwrap_or(12.0)
                        + super::layout_type_adjustment(theme),
                ))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(issue.identifier.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text)
                        .child(SharedString::from(issue.title.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().label.unwrap_or(10.5)
                        + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text_faint)
                .child(
                    div()
                        .flex_none()
                        .size(px(7.0))
                        .rounded_full()
                        .bg(state_color),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(meta.join(" \u{00b7} "))),
                ),
        )
}

/// An issue row's checkbox (`disabled`: checked and inert, e.g. an issue
/// already added). The caller adds the click.
pub(crate) fn issue_checkbox(
    selector: String,
    checked: bool,
    disabled: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let id = selector.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .size(px(13.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(3.0)))
        .border_1()
        .border_color(if checked { theme.accent } else { theme.border })
        .when(checked, |el| el.bg(theme.accent))
        .when(disabled, |el| el.opacity(0.45))
        .when(!disabled, |el| el.cursor_pointer())
}
