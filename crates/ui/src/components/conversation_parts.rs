//! Conversation-specific presentation shared by transcript surfaces.

use crate::theme::Theme;
use crate::ui_style::ConversationVariant;
use gpui::{AnyElement, Div, Hsla, SharedString, div, prelude::*, px};

pub fn conversation_code_theme(theme: &Theme) -> Theme {
    let mut result = theme.clone();
    if theme.ui_style.id == crate::ui_style::UiStyleId::Area {
        let panel = if theme.appearance.is_dark() {
            theme.surface_raised
        } else {
            theme.solid
        };
        result.bg = panel;
        result.text = theme.identity_text(panel);
        result.text_muted = result.text;
        result.text_faint = result.text;
    }
    result
}

pub fn conversation_code_copy_label(theme: &Theme, copied: bool) -> Option<&'static str> {
    if copied {
        Some("Copied")
    } else if theme.ui_style.id == crate::ui_style::UiStyleId::Area {
        Some("Copy")
    } else {
        None
    }
}

pub fn conversation_code_token_color(theme: &Theme, color: Hsla) -> Hsla {
    if theme.ui_style.id == crate::ui_style::UiStyleId::Area {
        (0..=100)
            .map(|step| crate::theme::flatten(color.opacity(1.0 - step as f32 / 100.0), theme.text))
            .find(|candidate| crate::theme::contrast_ratio(*candidate, theme.bg) >= 7.0)
            .unwrap_or(theme.text)
    } else {
        color
    }
}

/// Theme used while rendering the contents of a user message.
pub fn user_message_theme(theme: &Theme, identity: Hsla) -> Theme {
    let mut result = theme.clone();
    if theme.ui_style.conversation == ConversationVariant::Area {
        result.text = theme.identity_text(identity);
        result.code_wash = result.text;
        result.code_text = identity;
    }
    result
}

/// Theme used while rendering the contents of an agent message.
pub fn agent_message_theme(theme: &Theme, identity: Hsla) -> Theme {
    let mut result = theme.clone();
    if theme.ui_style.conversation == ConversationVariant::Area {
        result.font_sans = "EB Garamond".into();
        result.text = theme.identity_foreground(identity);
        result.ui_style.tokens.body_size = Some(17.5);
    }
    result
}

/// Render a user bubble with conversation-specific type metrics.
pub fn conversation_user_bubble(
    theme: &Theme,
    identity: Hsla,
    width: f32,
    line_height: f32,
    pending: bool,
    content: AnyElement,
) -> AnyElement {
    let mut bubble_theme = theme.clone();
    if theme.ui_style.conversation == ConversationVariant::Area {
        bubble_theme.ui_style.tokens.body_size = Some(16.5);
    }
    super::user_message_bubble(
        &bubble_theme,
        identity,
        width,
        line_height,
        pending,
        content,
    )
}

/// The centered content column shared by a transcript row and its metadata.
pub fn conversation_column(theme: &Theme, width: f32) -> Div {
    let width = if theme.ui_style.conversation == ConversationVariant::Area {
        width.min(720.0)
    } else {
        width
    };
    div().w_full().max_w(px(width)).min_w_0()
}

/// Theme for the tool panel and its contents.
pub fn conversation_tool_theme(theme: &Theme) -> Theme {
    let mut result = theme.clone();
    if theme.ui_style.conversation == ConversationVariant::Area {
        let panel = if theme.appearance.is_dark() {
            theme.surface_raised
        } else {
            theme.solid
        };
        let foreground = theme.identity_text(panel);
        result.bg = panel;
        result.surface = panel;
        result.surface_raised = panel;
        result.text = foreground;
        result.text_muted = foreground.opacity(0.8);
        result.text_faint = foreground.opacity(0.7);
    }
    result
}

/// Frame the tool group for the active conversation style.
pub fn conversation_tool_panel(theme: &Theme, identity: Hsla, body: AnyElement) -> AnyElement {
    if theme.ui_style.conversation != ConversationVariant::Area {
        return body;
    }
    div()
        .rounded(px(14.0))
        .bg(theme.bg)
        .px(px(16.0))
        .py(px(12.0))
        .child(
            div()
                .w_full()
                .flex()
                .justify_center()
                .items_center()
                .gap(px(6.0))
                .text_color(theme.identity_foreground(identity))
                .font_family("EB Garamond")
                .text_size(crate::typography::ui_rems(16.0))
                .child(div().size(px(10.0)).rounded_full().bg(identity))
                .child("Tools"),
        )
        .child(body)
        .into_any_element()
}

fn code_block_header(
    language: Option<&str>,
    actions: Vec<AnyElement>,
    theme: &Theme,
) -> Option<gpui::Div> {
    if language.is_none() && actions.is_empty() {
        return None;
    }
    if theme.ui_style.id == crate::ui_style::UiStyleId::Area {
        return Some(
            div()
                .h(crate::typography::ui_rems(28.0 + 8.0))
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .font_family("EB Garamond")
                        .text_size(px(16.0))
                        .text_color(theme.text)
                        .child(SharedString::from(language.map_or_else(
                            || "Code".to_string(),
                            |lang| format!("Code · {lang}"),
                        ))),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(8.0))
                        .top(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .children(actions),
                ),
        );
    }
    Some(
        div()
            .h(crate::typography::ui_rems(28.0))
            .flex_none()
            .pl(crate::typography::ui_rems(12.0))
            .pr(crate::typography::ui_rems(5.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(crate::theme::ink(0.02))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .min_w_0()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .children(language.map(|lang| SharedString::from(lang.to_string()))),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .children(actions),
            ),
    )
}

/// Shared visual shell for ordinary code and generated diagram fences.
pub fn conversation_code_frame(
    id: SharedString,
    language: Option<&str>,
    actions: Vec<AnyElement>,
    body: AnyElement,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .rounded(crate::typography::ui_rems(10.0))
        .bg(crate::theme::ink(0.035))
        .border_1()
        .border_color(theme.border)
        .when(
            theme.ui_style.id == crate::ui_style::UiStyleId::Area,
            |el| {
                el.rounded(px(14.0))
                    .bg(theme.bg)
                    .border_color(theme.bg)
                    .p(px(4.0))
            },
        )
        .overflow_hidden()
        .relative()
        .children(code_block_header(language, actions, theme))
        .child(body)
}
