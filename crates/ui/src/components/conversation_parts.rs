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
