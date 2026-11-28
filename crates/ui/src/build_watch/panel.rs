//! The Space Builds panel: GPUI for a [`PanelModel`]. It is a block inside the running-chats
//! popover (the titlebar lamp), so it lives next to the chats it explains. Pure layout: every
//! click is reported as a [`PanelAction`] through one callback the host supplies; the host owns
//! the RPCs (`CancelBuildJob`, `BuildJobLog`) and the log text.

use super::jobs::{PanelModel, PanelRow, Tone};
use crate::components::shell_parts::running_button;
use crate::status_style::{Status, status_style};
use crate::theme::Theme;
use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};

/// What a click in the panel asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    /// Open the chat that owns the job.
    OpenChat(String),
    /// Cancel a queued or running job.
    Cancel(String),
    /// Show or hide the job's log tail under its row.
    ToggleLog(String),
}

/// The log tail the host fetched for one expanded job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogView {
    pub job_id: String,
    pub body: LogBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogBody {
    Loading,
    Text { text: String, truncated: bool },
    Failed(String),
}

/// A Cancel that the engine refused or never received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelError {
    pub job_id: String,
    pub error: String,
}

const LOG_MAX_HEIGHT: f32 = 140.0;

fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    let status = match tone {
        Tone::Building => Status::Working,
        Tone::Waiting => Status::Waiting,
        Tone::Good => Status::Done,
        Tone::Bad => Status::Errored,
        Tone::Muted => Status::Idle,
    };
    match tone {
        Tone::Muted => theme.text_faint,
        _ => status_style(status, theme.appearance).color,
    }
}

/// The whole block: heading, active rows, recent rows. `on` receives every click.
pub(crate) fn render<V: 'static>(
    model: &PanelModel,
    log: Option<&LogView>,
    cancel_error: Option<&CancelError>,
    theme: &Theme,
    cx: &mut Context<V>,
    on: fn(&mut V, PanelAction, &mut Window, &mut Context<V>),
) -> AnyElement {
    let mut block = div()
        .id("builds-panel")
        .debug_selector(|| "builds-panel".into())
        .mx(px(4.0))
        .mb(px(4.0))
        .py(px(6.0))
        .flex()
        .flex_col()
        .rounded(px(crate::popover::MENU_ITEM_RADIUS))
        .border_1()
        .border_color(theme.border_strong)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .pb(px(2.0))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Builds"),
                )
                .child(
                    div()
                        .id("builds-headline")
                        .role(gpui::Role::Status)
                        .aria_label(SharedString::from(model.headline()))
                        .debug_selector(|| "builds-headline".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(if model.quiet_window {
                            theme.warning
                        } else {
                            theme.text_faint
                        })
                        .child(SharedString::from(model.headline())),
                ),
        );
    for row in &model.active {
        block = block.child(render_row(row, log, cancel_error, theme, cx, on));
    }
    if !model.recent.is_empty() {
        block = block.child(
            div()
                .px(px(8.0))
                .pt(px(6.0))
                .pb(px(2.0))
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_faint)
                .child(SharedString::from(format!(
                    "Recent \u{b7} {}",
                    model.recent.len()
                ))),
        );
        for row in &model.recent {
            block = block.child(render_row(row, log, cancel_error, theme, cx, on));
        }
    }
    block.into_any_element()
}

fn render_row<V: 'static>(
    row: &PanelRow,
    log: Option<&LogView>,
    cancel_error: Option<&CancelError>,
    theme: &Theme,
    cx: &mut Context<V>,
    on: fn(&mut V, PanelAction, &mut Window, &mut Context<V>),
) -> AnyElement {
    let color = tone_color(row.tone, theme);
    let job_id = row.job_id.clone();
    let open_log = log.filter(|log| log.job_id == row.job_id);
    let (chat_id, cancel_id, log_id) =
        (row.chat_id.clone(), row.job_id.clone(), row.job_id.clone());
    let chat_title = row
        .chat_title
        .clone()
        .unwrap_or_else(|| "Unknown chat".to_owned());
    let mut el = div()
        .id(SharedString::from(format!("build-row-{job_id}")))
        .debug_selector({
            let selector = format!("build-row-{job_id}");
            move || selector.clone()
        })
        .role(gpui::Role::Group)
        .aria_label(SharedString::from(format!(
            "Build {}: {}",
            row.title, row.status
        )))
        .px(px(8.0))
        .py(px(3.0))
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .debug_selector({
                            let selector = format!("build-status-{job_id}");
                            move || selector.clone()
                        })
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(color)
                        .child(SharedString::from(row.status.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(SharedString::from(row.title.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(row.timing.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .id(SharedString::from(format!("build-chat-{job_id}")))
                        .role(gpui::Role::Button)
                        .aria_label(SharedString::from(format!(
                            "Open the chat that owns build {}",
                            row.title
                        )))
                        .debug_selector({
                            let selector = format!("build-chat-{job_id}");
                            move || selector.clone()
                        })
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_dim)
                        .hover(|el| el.text_color(theme.text))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            on(this, PanelAction::OpenChat(chat_id.clone()), window, cx);
                        }))
                        .child(SharedString::from(chat_title)),
                )
                .child(
                    running_button(
                        SharedString::from(format!("build-log-{job_id}")),
                        if open_log.is_some() {
                            "Hide log"
                        } else {
                            "Log"
                        },
                        false,
                        theme,
                    )
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!(
                        "{} of build {}",
                        if open_log.is_some() {
                            "Hide log"
                        } else {
                            "Log"
                        },
                        row.title
                    )))
                    .debug_selector({
                        let selector = format!("build-log-{job_id}");
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        on(this, PanelAction::ToggleLog(log_id.clone()), window, cx);
                    })),
                )
                .children(row.can_cancel.then(|| {
                    let selector = format!("build-cancel-{job_id}");
                    running_button(
                        SharedString::from(format!("build-cancel-{job_id}")),
                        "Cancel",
                        false,
                        theme,
                    )
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("Cancel build {}", row.title)))
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        on(this, PanelAction::Cancel(cancel_id.clone()), window, cx);
                    }))
                })),
        );
    if let Some(detail) = &row.detail {
        el = el.child(
            div()
                .truncate()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(detail.clone())),
        );
    }
    if let Some(failed) = cancel_error.filter(|failed| failed.job_id == row.job_id) {
        let selector = format!("build-cancel-error-{job_id}");
        let message = format!("Couldn\u{2019}t cancel: {}", failed.error);
        el = el.child(
            div()
                .id(SharedString::from(selector.clone()))
                .role(gpui::Role::Alert)
                .aria_label(SharedString::from(message.clone()))
                .debug_selector(move || selector.clone())
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.danger)
                .child(SharedString::from(message)),
        );
    }
    if let Some(log) = open_log {
        el = el.child(render_log(log, &job_id, theme));
    }
    el.into_any_element()
}

fn render_log(log: &LogView, job_id: &str, theme: &Theme) -> AnyElement {
    let (text, color) = match &log.body {
        LogBody::Loading => ("Loading log\u{2026}".to_owned(), theme.text_muted),
        LogBody::Failed(error) => (error.clone(), theme.danger),
        LogBody::Text { text, .. } if text.is_empty() => {
            ("The log is empty.".to_owned(), theme.text_muted)
        }
        LogBody::Text { text, truncated } => (
            if *truncated {
                format!("\u{2026}\n{text}")
            } else {
                text.clone()
            },
            theme.text,
        ),
    };
    div()
        .id(SharedString::from(format!("build-log-body-{job_id}")))
        .role(gpui::Role::Log)
        .aria_label(SharedString::from(text.clone()))
        .debug_selector({
            let selector = format!("build-log-body-{job_id}");
            move || selector.clone()
        })
        .mt(px(4.0))
        .p(px(6.0))
        .max_h(px(LOG_MAX_HEIGHT))
        .overflow_y_scroll()
        .rounded(px(4.0))
        .bg(theme.glass_hover())
        .font_family(theme.font_mono.clone())
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(color)
        .child(SharedString::from(text))
        .into_any_element()
}
