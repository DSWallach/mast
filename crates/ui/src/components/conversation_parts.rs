//! Conversation-specific presentation shared by transcript surfaces.

use crate::theme::Theme;
use crate::ui_style::ConversationVariant;
use gpui::{AnyElement, Div, Hsla, SharedString, div, prelude::*, px};

pub fn conversation_message_copy_button(
    theme: &Theme,
    entry_id: &str,
    copied: bool,
) -> gpui::Stateful<Div> {
    let fade_key = format!("copy-message-hover-{entry_id}");
    div()
        .id(SharedString::from(format!("copy-message-{entry_id}")))
        .size(px(Theme::SPACE_MD * 2.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(Theme::CONTROL_RADIUS))
        .cursor_pointer()
        .bg(crate::motion::hover_blend(
            &fade_key,
            gpui::transparent_black(),
            crate::theme::ink(0.08),
        ))
        .on_hover(crate::motion::hover_listener(fade_key))
        .child(
            crate::icons::icon(if copied {
                crate::icons::CHECK
            } else {
                crate::icons::COPY
            })
            .size(px(14.0))
            .text_color(theme.text_muted),
        )
}

pub fn conversation_harness_tab(
    theme: &Theme,
    id: SharedString,
    identity: Hsla,
    icon: &'static str,
    selected: bool,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size(px(28.0))
        .rounded_t(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(identity)
        .text_color(theme.identity_text(identity))
        .when(!selected, |el| el.opacity(0.75))
        .cursor_pointer()
        .child(
            crate::icons::icon(icon)
                .size(px(16.0))
                .text_color(theme.identity_text(identity)),
        )
}

pub fn conversation_attach_key(theme: &Theme) -> gpui::Stateful<Div> {
    div()
        .id("composer-attach")
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(crate::motion::hover_blend(
            "composer-attach",
            gpui::transparent_black(),
            crate::theme::ink(0.10),
        ))
        .when(
            theme.ui_style.controls == crate::ui_style::ControlVariant::Key,
            |el| crate::surface_chrome::icon_key(el, theme, false),
        )
        .on_hover(crate::motion::hover_listener("composer-attach"))
        .child(
            crate::icons::icon(crate::icons::PAPERCLIP)
                .size(px(16.0))
                .relative()
                .left(px(1.0))
                .text_color(theme.text_muted),
        )
}

pub fn conversation_composer_content_height(theme: &Theme, content: f32, line: f32) -> f32 {
    if theme.ui_style.conversation == ConversationVariant::Area {
        content.max(3.0 * line)
    } else {
        content
    }
}

pub fn conversation_composer_expanded(theme: &Theme, expanded: bool) -> bool {
    expanded || theme.ui_style.conversation == ConversationVariant::Area
}

pub fn conversation_composer_text_size(theme: &Theme, size: f32) -> f32 {
    if theme.ui_style.conversation == ConversationVariant::Area {
        15.0
    } else {
        size
    }
}

pub fn conversation_panel_radius(theme: &Theme, fallback: f32) -> f32 {
    theme.ui_style.tokens.panel_radius.unwrap_or(fallback)
}

pub fn conversation_input_surface<T: gpui::Styled + gpui::prelude::FluentBuilder>(
    element: T,
    theme: &Theme,
    identity: Hsla,
    fallback_radius: f32,
    border: Hsla,
) -> T {
    let area = theme.ui_style.conversation == ConversationVariant::Area;
    element
        .rounded(px(conversation_panel_radius(theme, fallback_radius)))
        .border_1()
        .border_color(if area { identity } else { border })
        .bg(if area {
            identity
        } else if theme.is_frost() {
            theme.composer_sidebar_tint()
        } else {
            theme.input_glass_bg()
        })
        .when(
            !area
                && !theme.is_frost()
                && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault,
            |el| el.shadow_lg(),
        )
}

pub fn conversation_composer_width(theme: &Theme, width: f32, gutter: f32) -> f32 {
    if theme.ui_style.conversation == ConversationVariant::Area {
        width.min(720.0 + 2.0 * gutter)
    } else {
        width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_composer_has_three_lines_and_matches_conversation_width() {
        let mut theme = Theme::light();
        theme.ui_style = crate::ui_style::UiStyle::AREA;
        assert!(conversation_composer_expanded(&theme, false));
        assert_eq!(
            conversation_composer_content_height(&theme, 22.0, 22.0),
            66.0
        );
        assert_eq!(conversation_composer_width(&theme, 768.0, 16.0), 752.0);
        theme.ui_style = crate::ui_style::UiStyle::MINIMAL;
        assert!(!conversation_composer_expanded(&theme, false));
        assert_eq!(
            conversation_composer_content_height(&theme, 22.0, 22.0),
            22.0
        );
        assert_eq!(conversation_composer_width(&theme, 768.0, 16.0), 768.0);
    }
}

/// Send, queue, and interrupt control; callers attach the corresponding action.
pub fn conversation_send_key(
    theme: &Theme,
    identity: Hsla,
    stop: bool,
    queue: bool,
) -> gpui::Stateful<Div> {
    use crate::ui_style::{ConversationVariant, UiLayout};
    let area = theme.ui_style.conversation == ConversationVariant::Area;
    let key = theme.ui_style.conversation == ConversationVariant::Industrial;
    let guided = theme.ui_style.layout == UiLayout::Guided;
    let fill = if area {
        crate::theme::flatten(theme.text_muted.opacity(0.28), identity)
    } else {
        theme.text
    };
    let text = if area {
        theme.identity_text(fill)
    } else if key {
        theme.on_accent
    } else {
        theme.bg
    };
    let label = if stop {
        "Stop"
    } else if queue {
        "Queue"
    } else {
        "Send"
    };
    let icon = if stop {
        div()
            .size(px(11.0))
            .rounded(px(3.0))
            .bg(text)
            .into_any_element()
    } else {
        crate::icons::icon(crate::icons::ARROW_UP)
            .size(px(14.0))
            .text_color(text)
            .into_any_element()
    };
    div()
        .id(if stop {
            "composer-stop"
        } else {
            "composer-send"
        })
        .size(px(28.0))
        .when(guided, |el| {
            el.h(px(32.0)).w_auto().px(px(10.0)).gap(px(5.0))
        })
        .when(area, |el| el.h(px(32.0)).w(px(96.0)).gap(px(6.0)))
        .flex_none()
        .rounded_full()
        .when_some(theme.ui_style.tokens.control_radius, |el, radius| {
            el.rounded(px(radius))
        })
        .bg(fill)
        .when(key, |el| crate::surface_chrome::icon_key(el, theme, true))
        .flex()
        .items_center()
        .justify_center()
        .child(icon)
        .when(guided || area, |el| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(text)
                    .child(if area && !stop {
                        label.to_uppercase()
                    } else {
                        label.to_string()
                    }),
            )
        })
}

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
