//! Conversation-specific presentation shared by transcript surfaces.

use crate::theme::Theme;
use crate::ui_style::ConversationVariant;
use gpui::{AnyElement, Div, Hsla, div, prelude::*, px};

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
