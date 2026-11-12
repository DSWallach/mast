//! The right-pane side chat: header action, hint lines and composer frame.

use crate::theme::Theme;
use gpui::{AnyElement, Div, SharedString, div, prelude::*, px};

/// Muted one-line status or empty-state text.
pub(crate) fn side_chat_hint(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .px(px(12.0))
        .py(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// The empty pane: what a side chat is, before the first question.
pub(crate) fn side_chat_empty(theme: &Theme, title: &str, body: &str) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .px(px(24.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(title.to_owned())),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .text_center()
                .child(SharedString::from(body.to_owned())),
        )
}

/// "Absorb into main chat": the primary action, dimmed while unavailable.
pub(crate) fn side_chat_absorb_button(
    theme: &Theme,
    label: &str,
    enabled: bool,
) -> gpui::Stateful<Div> {
    crate::popover::btn_primary(theme, label)
        .id("side-chat-absorb")
        .debug_selector(|| "side-chat-absorb".into())
        .px(px(10.0))
        .py(px(4.0))
        .text_size(crate::typography::ui_rems(12.0))
        .when(!enabled, |el| el.opacity(0.4).cursor_default())
}

/// The composer: the input on the conversation input surface with the send
/// key at its trailing edge.
pub(crate) fn side_chat_composer(theme: &Theme, input: AnyElement, send: AnyElement) -> Div {
    let surface = div()
        .w_full()
        .flex()
        .items_end()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .child(div().flex_1().min_w_0().child(input))
        .child(send);
    div()
        .w_full()
        .p(px(10.0))
        .child(super::conversation_input_surface(
            surface,
            theme,
            theme.accent,
            12.0,
            theme.border,
        ))
}
