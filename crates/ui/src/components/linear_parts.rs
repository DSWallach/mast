//! Presentation primitives for Linear surfaces.

use crate::theme::Theme;
use gpui::{SharedString, div, prelude::*, px};
use mast_proto::linear::LinearIssueSummary;

pub(crate) fn settings_status(
    theme: &Theme,
    title: String,
    detail: Option<String>,
    color: gpui::Hsla,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(div().size(px(7.0)).rounded_full().bg(color))
                .child(
                    crate::settings::widgets::row_title(theme, title)
                        .debug_selector(|| "linear-settings-status".into()),
                ),
        )
        .when_some(detail, |el, detail| {
            el.child(settings_detail(theme, SharedString::from(detail)))
        })
}

pub(crate) fn settings_key_row(theme: &Theme) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(8.0 * super::layout_spacing(theme)))
}

pub(crate) fn settings_key_help(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().secondary.unwrap_or(12.0)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child("Create one in Linear under Settings → Security & access.")
}

pub(crate) fn settings_key_link(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-settings-open-keys")
        .debug_selector(|| "linear-settings-open-keys".into())
        .flex()
        .items_center()
        .gap(px(2.0))
        .cursor_pointer()
        .text_color(theme.accent)
        .child("Open Linear")
        .child(
            crate::icons::icon(crate::icons::ARROW_UP_RIGHT)
                .size(px(11.0))
                .text_color(theme.accent),
        )
}

pub(crate) fn picker_backdrop(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-add-issues-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .justify_center()
        .items_start()
        .pt(px(64.0))
        .bg(theme.scrim())
        .occlude()
}

pub(crate) fn inbox_header(theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(14.0))
        .pt(px(8.0))
        .child(
            div()
                .flex_1()
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().label.unwrap_or(10.5)
                        + super::layout_type_adjustment(theme),
                ))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_faint)
                .child("INBOX"),
        )
}

pub(crate) fn inbox_close(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let button = div()
        .id("inbox-column-close")
        .debug_selector(|| "inbox-column-close".into())
        .flex_none()
        .size(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::CLOSE)
                .size(px(11.0))
                .text_color(theme.text_muted),
        );
    super::toolbar_control(button, theme, false)
}

pub(crate) fn space_menu_card(theme: &Theme) -> gpui::Div {
    crate::popover::popover_card(theme)
        .debug_selector(|| "linear-space-menu".into())
        .w(px(220.0))
}

pub(crate) fn space_menu_list(body: impl gpui::IntoElement) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-space-menu-list")
        .max_h(px(280.0))
        .overflow_y_scroll()
        .child(body)
}

pub(crate) fn parent_row(
    index: usize,
    selector: String,
    label: String,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("linear-parent", index))
        .debug_selector(move || selector.clone())
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
        .cursor_pointer()
        .truncate()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().secondary.unwrap_or(11.5)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover))
        .child(SharedString::from(label))
}

pub(crate) fn parent_picker(theme: &Theme) -> gpui::Div {
    div()
        .debug_selector(|| "linear-parent-picker".into())
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(theme.ui_style.radius_roles().panel.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.accent.opacity(0.6))
}

pub(crate) fn parent_picker_title(count: usize, theme: &Theme) -> gpui::Div {
    div()
        .flex_1()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().label.unwrap_or(11.0)
                + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(SharedString::from(if count == 1 {
            "Create under…".to_owned()
        } else {
            format!("Create {count} items under…")
        }))
}

pub(crate) fn parent_list(
    rows: impl IntoIterator<Item = gpui::Stateful<gpui::Div>>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-parent-list")
        .max_h(px(220.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .children(rows)
}

pub(crate) fn drag_ghost(title: SharedString, theme: &Theme) -> gpui::Div {
    div()
        .max_w(px(260.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_raised)
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().body.unwrap_or(12.0) + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .truncate()
        .child(title)
}

pub(crate) fn panel_header(theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(14.0))
        .pt(px(10.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().title.unwrap_or(13.0)
                        + super::layout_type_adjustment(theme),
                ))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child("Linear"),
        )
}

pub(crate) fn panel_search(theme: &Theme) -> gpui::Div {
    search_frame(theme).mx(px(14.0)).mt(px(8.0))
}

pub(crate) fn panel_body(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-panel-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .px(px(14.0))
        .py(px(8.0))
}

pub(crate) fn project_summary(name: String, metadata: String, theme: &Theme) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .child(
            div()
                .truncate()
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().body.unwrap_or(12.0)
                        + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .truncate()
                .text_size(crate::typography::ui_rems(
                    theme.ui_style.type_roles().label.unwrap_or(10.5)
                        + super::layout_type_adjustment(theme),
                ))
                .text_color(theme.text_faint)
                .child(SharedString::from(metadata)),
        )
}

pub(crate) fn linked_marker(id: String, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.0))
        .px(px(4.0))
        .rounded(px(theme.ui_style.radius_roles().chip.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().label.unwrap_or(10.0)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .hover(|el| el.bg(theme.element_hover))
        .child("Linked · Reveal")
}

pub(crate) fn team_summary(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

pub(crate) fn team_column(theme: &Theme, compact: bool) -> gpui::Div {
    div().flex().flex_col().gap(px(
        (if compact { 4.0 } else { 8.0 }) * super::layout_spacing(theme)
    ))
}

pub(crate) fn team_worker_header(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
}

pub(crate) fn team_worker_metadata(theme: &Theme, wrap: bool) -> gpui::Div {
    div()
        .pl(px(13.0))
        .flex()
        .items_center()
        .when(wrap, |el| el.flex_wrap())
        .gap(px(
            (if wrap { 4.0 } else { 6.0 }) * super::layout_spacing(theme)
        ))
}

pub(crate) fn team_archive_block(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.0 * super::layout_spacing(theme)))
}

pub(crate) fn team_archive_actions(theme: &Theme, confirmation: bool) -> gpui::Div {
    div()
        .flex()
        .when(!confirmation, |el| el.items_center())
        .gap(px(
            (if confirmation { 6.0 } else { 8.0 }) * super::layout_spacing(theme)
        ))
}

pub(crate) fn picker_frame(theme: &Theme, width: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-add-issues")
        .debug_selector(|| "linear-add-issues".into())
        .occlude()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .p(px(10.0))
        .rounded(px(theme.ui_style.radius_roles().panel.unwrap_or(8.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_dialog)
        .shadow_lg()
}

pub(crate) fn quiet_message(
    theme: &Theme,
    text: impl Into<SharedString>,
    compact: bool,
) -> gpui::Div {
    div()
        .py(px(
            (if compact { 4.0 } else { 6.0 }) * super::layout_spacing(theme)
        ))
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().secondary.unwrap_or(11.5)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(text.into())
}

pub(crate) fn settings_detail(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().secondary.unwrap_or(12.0)
                + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(text.into())
}

pub(crate) fn settings_error(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    settings_detail(theme, text)
        .debug_selector(|| "linear-settings-save-error".into())
        .text_color(theme.danger)
}

pub(crate) fn key_input(focused: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("linear-key-input")
        .debug_selector(|| "linear-key-input".into())
        .key_context("LinearKeyInput")
        .flex_1()
        .min_w_0()
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(6.0)))
        .border_1()
        .border_color(if focused { theme.accent } else { theme.border })
        .bg(theme.surface)
        .cursor_text()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.type_roles().body.unwrap_or(12.5) + super::layout_type_adjustment(theme),
        ))
}

pub(crate) fn input_cursor(theme: &Theme) -> gpui::Div {
    div().ml(px(1.0)).w(px(1.0)).h(px(14.0)).bg(theme.accent)
}

pub(crate) fn search_frame(theme: &Theme) -> gpui::Div {
    let frame = div()
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border);
    if super::is_horizon(theme) || super::is_industrial(theme) {
        super::toolbar_control(frame, theme, false)
    } else {
        frame
    }
}

pub(crate) fn header_icon(
    id: &'static str,
    icon: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let control = div()
        .id(id)
        .flex_none()
        .size(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(icon)
                .size(px(12.0))
                .text_color(theme.text_muted),
        );
    super::toolbar_control(control, theme, false)
}

pub(crate) fn issue_row(selector: String, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(4.0))
        .py(px(3.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.radius_roles().control.unwrap_or(4.0)))
}

pub(crate) fn inbox_toolbar_button(active: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let control = div()
        .id("outline-linear")
        .debug_selector(|| "outline-linear".into())
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(4.0))
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
    super::toolbar_control(control, theme, false).child("Inbox")
}

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
