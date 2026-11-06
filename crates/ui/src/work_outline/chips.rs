//! Shared element builders for work-item metadata on List rows,
//! Outline rows, and Work Item Details.
//!
//! Attention ("User Action Required") uses the shared waiting status style.
use gpui::{AnyElement, Render, SharedString, Window, div, prelude::*, px};

use super::filter::WorkItemPathLabel;
use super::hints::{HintKey, WithHint};
use super::store::{WorkItemEffectiveStatus, WorkItemStatus};
use crate::theme::Theme;

/// Plain text tooltip (the Overview's own tooltip type is private to it).
pub(crate) struct TextTooltip(pub SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_dialog)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// The attention color: sidebar Awaiting Input.
pub(crate) fn attention_color(theme: &Theme) -> gpui::Hsla {
    crate::status_style::status_style(crate::status_style::Status::Waiting, theme.appearance).color
}

/// Compact work-item path chip. `id` must be unique per frame (tooltips are
/// stateful elements).
pub(crate) fn path_chip(
    id: impl Into<SharedString>,
    path: &WorkItemPathLabel,
    theme: &Theme,
) -> AnyElement {
    let tooltip: SharedString = format!("Work item: {}", path.full).into();
    div()
        .id(id.into())
        .debug_selector(|| "work-item-path-chip".into())
        .min_w_0()
        .max_w(px(160.0))
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.border)
        .truncate()
        .text_size(crate::typography::ui_rems(9.0))
        .text_color(theme.text_muted)
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tooltip.clone())).into())
        .child(SharedString::from(path.short.clone()))
        .into_any_element()
}

/// "User Action Required" indicator: an accent dot, plus the count when more
/// than one request is open. `None` when nothing needs the user.
pub(crate) fn attention_indicator(
    id: impl Into<SharedString>,
    required: bool,
    count: u32,
    theme: &Theme,
) -> Option<AnyElement> {
    if !required && count == 0 {
        return None;
    }
    let color = attention_color(theme);
    let tooltip: SharedString = match count {
        0 | 1 => "User action required".to_string(),
        n => format!("User action required ({n})"),
    }
    .into();
    Some(
        div()
            .id(id.into())
            .debug_selector(|| "work-item-attention".into())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(3.0))
            .text_size(crate::typography::ui_rems(9.5))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(crate::status_style::indicator_for_theme(crate::status_style::Status::Waiting, &theme))
            .when(count > 1, |el| {
                el.child(SharedString::from(count.to_string()))
            })
            .hint(
                tooltip,
                HintKey::Command(super::command::WorkOutlineCommand::OpenRequiredActions),
            )
            .into_any_element(),
    )
}

/// Tooltip shared by every instruction drift mark.
pub(crate) const INSTRUCTION_DRIFT_TOOLTIP: &str =
    "Instructions changed since this Chat\u{2019}s applied revision. Click to compare and apply.";

/// "Instructions changed" mark for a drifted Chat: the warning triangle the
/// Outline chip and chat header use, optionally with the label. `id` doubles
/// as the debug selector (unique per frame). It sits on an
/// existing line (title row), so it never changes a row's size. `on_click`
/// opens Compare; the mark stops mouse-down propagation so the List row does
/// not toggle under it.
pub(crate) fn instruction_drift_mark(
    id: impl Into<SharedString>,
    with_label: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .px(px(2.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                .size(px(10.0))
                .text_color(theme.warning),
        )
        .when(with_label, |el| el.child("Instructions changed"))
        .hint(INSTRUCTION_DRIFT_TOOLTIP, HintKey::None)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .into_any_element()
}

/// The execution-exception mark for a Chat running with a captured
/// configuration that differs from its item's current one (List rows).
/// Compact (no label) so it shares the title line.
pub(crate) fn execution_exception_mark(id: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex_none()
        .flex()
        .items_center()
        .px(px(2.0))
        .child(
            crate::icons::icon(crate::icons::FOLDER_WITH_FILES)
                .size(px(10.0))
                .text_color(theme.accent),
        )
        .tooltip(|_, cx| {
            cx.new(|_| {
                TextTooltip(
                    "This Chat uses a different execution configuration than its work item".into(),
                )
            })
            .into()
        })
        .into_any_element()
}

/// Glyph + label + color for an effective status (Outline rows, details).
pub(crate) fn status_style(
    status: WorkItemEffectiveStatus,
    theme: &Theme,
) -> (&'static str, &'static str, gpui::Hsla) {
    match status {
        WorkItemEffectiveStatus::Pending => ("○", "Pending", theme.text_muted),
        WorkItemEffectiveStatus::Active => ("◐", "Active", theme.busy),
        WorkItemEffectiveStatus::UserActionRequired => {
            (crate::status_style::glyph(crate::status_style::Status::Waiting), "User Action Required", attention_color(theme))
        }
        WorkItemEffectiveStatus::Completed => ("✓", "Completed", theme.success),
    }
}

pub(crate) fn base_status_label(status: WorkItemStatus) -> &'static str {
    match status {
        WorkItemStatus::Pending => "Pending",
        WorkItemStatus::Active => "Active",
        WorkItemStatus::Completed => "Completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_uses_the_waiting_status_color() {
        let theme = Theme::default();
        assert_eq!(attention_color(&theme), crate::status_style::status_style(crate::status_style::Status::Waiting, theme.appearance).color);
        let (_, label, color) = status_style(WorkItemEffectiveStatus::UserActionRequired, &theme);
        assert_eq!(label, "User Action Required");
        assert_eq!(color, attention_color(&theme));
        let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
        assert_ne!(color, critical);
    }
}

/// The "Blueprint changed (rev N → M)" chip of a project whose template moved
/// on, on its Outline row and in its Details Project section.
pub(crate) fn template_chip(selector: String, from: u32, to: u32, theme: &Theme) -> gpui::Div {
    crate::components::outline_metadata_chip(super::template_review::chip_text(from, to), theme)
        .debug_selector(move || selector.clone())
}
