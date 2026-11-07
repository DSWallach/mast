//! Conversation-specific presentation shared by transcript surfaces.

use crate::theme::Theme;
use crate::ui_style::ConversationVariant;
use gpui::{AnyElement, Div, Hsla, SharedString, div, prelude::*, px};

fn conversation_industrial_icon_key<
    T: gpui::Styled + gpui::ParentElement + gpui::StatefulInteractiveElement,
>(
    element: T,
    theme: &Theme,
    signal: bool,
) -> T {
    let colors = theme.industrial_colors();
    let fill = if signal { colors.signal } else { colors.key };
    let ring = crate::theme::flatten(theme.text.opacity(0.18), fill);
    let pressed = if signal {
        gpui::hsla(fill.h, fill.s, fill.l * 0.8, fill.a)
    } else {
        colors.pressed
    };
    let radius = theme.ui_style.tokens.control_radius.unwrap_or(4.0);
    element
        .rounded(px(radius))
        .bg(fill)
        .border_1()
        .border_color(colors.rule)
        .relative()
        .active(move |style| style.bg(pressed))
        .child(
            div()
                .absolute()
                .inset(px(3.0))
                .rounded(px((radius - 3.0).max(0.0)))
                .border_1()
                .border_color(ring),
        )
}

pub fn conversation_picker_label(label: SharedString) -> Div {
    div().min_w_0().truncate().child(label)
}

pub fn conversation_picker_suffix(theme: &Theme, label: SharedString, tint: Option<Hsla>) -> Div {
    div()
        .flex_shrink(1000.0)
        .min_w_0()
        .truncate()
        .text_color(tint.unwrap_or(theme.text_muted.opacity(0.7)))
        .child(label)
}

pub fn conversation_picker_footer_content(
    theme: &Theme,
    icon: &'static str,
    label: SharedString,
) -> Vec<AnyElement> {
    vec![
        crate::icons::icon(icon)
            .size(px(12.0))
            .flex_none()
            .text_color(theme.text_muted.opacity(0.7))
            .into_any_element(),
        conversation_picker_label(label).into_any_element(),
        crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
            .size(px(12.0))
            .flex_none()
            .text_color(theme.text_muted.opacity(0.5))
            .into_any_element(),
    ]
}

pub fn conversation_picker_footer_row() -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
}

pub fn conversation_picker_trigger(
    theme: &Theme,
    id: &'static str,
    open: bool,
    set: bool,
) -> gpui::Stateful<Div> {
    let chip = div()
        .id(id)
        .h(px(32.0))
        .max_w(px(248.0))
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(10.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(8.0)))
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(crate::motion::hover_blend(
            id,
            if set {
                theme.text.opacity(0.9)
            } else {
                theme.text_muted
            },
            theme.text,
        ))
        .bg(if open {
            theme.element_hover
        } else {
            crate::motion::hover_blend(id, gpui::transparent_black(), theme.element_hover)
        })
        .on_hover(crate::motion::hover_listener(id))
        .cursor_pointer()
        .when(
            theme.ui_style.layout == crate::ui_style::UiLayout::Guided,
            |el| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(theme.text_muted)
                        .child("Model"),
                )
            },
        );
    super::segmented_key(chip, theme, open)
}

pub fn conversation_picker_footer_chip(
    theme: &Theme,
    id: &'static str,
    open: bool,
) -> gpui::Stateful<Div> {
    let chip = div()
        .id(id)
        .h(px(20.0))
        .max_w(px(280.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(8.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(6.0)))
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(crate::motion::hover_blend(
            id,
            theme.text_muted.opacity(0.7),
            theme.text.opacity(0.8),
        ))
        .bg(if open {
            theme.element_hover
        } else {
            crate::motion::hover_blend(id, gpui::transparent_black(), theme.element_hover)
        })
        .on_hover(crate::motion::hover_listener(id))
        .cursor_pointer();
    super::segmented_key(chip, theme, open)
}

pub fn conversation_picker_approval(
    theme: &Theme,
    auto: bool,
    label: &'static str,
) -> gpui::Stateful<Div> {
    let id = "picker-approval-mode";
    let (icon, text, icon_color, bg) = if auto {
        (
            crate::icons::DANGER_TRIANGLE,
            crate::motion::hover_blend(id, theme.warning.opacity(0.85), theme.warning),
            theme.warning.opacity(0.85),
            crate::motion::hover_blend(
                id,
                theme.warning.opacity(0.10),
                theme.warning.opacity(0.18),
            ),
        )
    } else {
        (
            crate::icons::KEY_MINIMALISTIC,
            crate::motion::hover_blend(id, theme.text_muted.opacity(0.7), theme.text.opacity(0.8)),
            theme.text_muted.opacity(0.7),
            crate::motion::hover_blend(id, gpui::transparent_black(), theme.element_hover),
        )
    };
    let chip = div()
        .id(id)
        .h(px(20.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0 * super::layout_spacing(theme)))
        .px(px(8.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(6.0)))
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(text)
        .bg(bg)
        .on_hover(crate::motion::hover_listener(id))
        .cursor_pointer()
        .child(
            crate::icons::icon(icon)
                .size(px(12.0))
                .flex_none()
                .text_color(icon_color),
        )
        .child(
            if theme.ui_style.layout == crate::ui_style::UiLayout::Guided {
                format!("Approval: {label}")
            } else {
                label.to_string()
            },
        );
    super::segmented_key(chip, theme, auto)
}

pub fn conversation_picker_footer_label(
    icon: &'static str,
    label: SharedString,
    theme: &Theme,
) -> Div {
    div()
        .h(px(20.0))
        .max_w(px(160.0))
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .text_size(crate::typography::ui_rems(12.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_muted.opacity(0.6))
        .child(
            crate::icons::icon(icon)
                .size(px(12.0))
                .text_color(theme.text_muted.opacity(0.6)),
        )
        .child(div().min_w_0().truncate().child(label))
}

pub fn conversation_message_expander(
    theme: &Theme,
    row_id: &str,
    expanded: bool,
    line_height: f32,
) -> gpui::Stateful<Div> {
    let selector = format!("{row_id}-expander");
    let hover_color = theme.text;
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .group("user-message-toggle")
        .role(gpui::Role::Button)
        .aria_label(if expanded {
            "Collapse message"
        } else {
            "Expand message"
        })
        .aria_expanded(expanded)
        .flex()
        .items_center()
        .gap(px(5.0))
        .text_size(crate::typography::ui_rems(14.0))
        .line_height(crate::typography::ui_rems(line_height))
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(move |s| s.text_color(hover_color))
        .child(if expanded { "Show less" } else { "Show more" })
        .child(
            crate::icons::icon(if expanded {
                crate::icons::ALT_ARROW_UP
            } else {
                crate::icons::ALT_ARROW_DOWN
            })
            .size(px(12.0))
            .text_color(theme.text_muted)
            .group_hover("user-message-toggle", move |s| s.text_color(hover_color)),
        )
}

pub fn conversation_appshot_strip(gap: f32, pad_x: f32, pad_top: f32) -> gpui::Stateful<Div> {
    div()
        .id("composer-appshots-strip")
        .flex()
        .flex_row()
        .gap(px(gap))
        .px(px(pad_x))
        .pt(px(pad_top))
        .overflow_x_scroll()
}

pub fn conversation_appshot_card(
    index: usize,
    group: SharedString,
    width: f32,
    height: f32,
) -> gpui::Stateful<Div> {
    div()
        .id(("composer-appshot", index))
        .group(group)
        .relative()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .rounded(px(14.0))
        .overflow_hidden()
        .cursor_pointer()
        .hover(|style| style.bg(crate::theme::ink(0.045)))
}

pub fn conversation_appshot_remove(
    theme: &Theme,
    index: usize,
    group: SharedString,
) -> gpui::Stateful<Div> {
    conversation_reveal(
        div()
            .id(("composer-appshot-remove", index))
            .absolute()
            .top(px(6.0))
            .right(px(6.0))
            .size(px(22.0))
            .rounded_full()
            .bg(theme.bg.opacity(0.92))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .shadow_sm(),
        theme,
        group,
        false,
    )
    .child(
        crate::icons::icon(crate::icons::CLOSE_CIRCLE)
            .size(px(15.0))
            .text_color(theme.text_muted),
    )
}

pub fn conversation_attachment_strip(gap: f32, pad_x: f32, pad_top: f32) -> Div {
    div()
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(gap))
        .px(px(pad_x))
        .pt(px(pad_top))
}

pub fn conversation_attachment_thumb(index: usize, size: f32) -> gpui::Stateful<Div> {
    div()
        .id(("composer-att-thumb", index))
        .size(px(size))
        .rounded(px(8.0))
        .overflow_hidden()
        .border_1()
        .border_color(crate::theme::hairline(0.10))
        .cursor_pointer()
}

pub fn conversation_attachment_remove(
    theme: &Theme,
    index: usize,
    group: SharedString,
) -> gpui::Stateful<Div> {
    conversation_reveal(
        div()
            .id(("composer-att-remove", index))
            .absolute()
            .top(px(-6.0))
            .right(px(-6.0))
            .size(px(18.0))
            .rounded_full()
            .bg(theme.bg)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .shadow_sm(),
        theme,
        group,
        false,
    )
    .child(
        crate::icons::icon(crate::icons::CLOSE_CIRCLE)
            .size(px(14.0))
            .text_color(theme.text_muted),
    )
}

pub fn conversation_tool_file_badge(
    theme: &Theme,
    path: &str,
    name: SharedString,
    failed: bool,
    hover_text: bool,
) -> AnyElement {
    let hover_color = theme.text;
    let radius = theme.ui_style.tokens.chip_radius.unwrap_or(5.0);
    let badge = div()
        .min_w_0()
        .h(px(22.0))
        .flex()
        .items_center()
        .overflow_hidden()
        .gap(px(6.0))
        .rounded(px(radius))
        .bg(theme.ink(0.06))
        .pl(px(1.0))
        .pr(px(6.0))
        .text_color(if failed {
            theme.danger
        } else {
            theme.text.opacity(0.85)
        })
        .child(
            div()
                .size(px(20.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(crate::file_icons::well_bg(theme))
                .child(
                    crate::file_icons::icon(
                        crate::file_icons::FileIconIdentity::file(path),
                        theme.appearance,
                    )
                    .size(px(14.0)),
                ),
        )
        .child(div().min_w_0().truncate().child(name))
        .map(|badge| {
            if hover_text {
                badge
                    .id("tool-file-badge")
                    .group_hover("tool-header", move |style| style.text_color(hover_color))
                    .into_any_element()
            } else {
                badge.into_any_element()
            }
        });
    crate::frost::frosted(radius, 16.0, badge).into_any_element()
}

pub fn conversation_tool_stat_row(
    theme: &Theme,
    path: &str,
    additions: u64,
    deletions: u64,
    height: f32,
) -> Div {
    div()
        .h(px(height))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            crate::file_icons::icon(
                crate::file_icons::FileIconIdentity::file(path),
                theme.appearance,
            )
            .size(px(14.0))
            .flex_none(),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(theme.text_faint)
                .child(SharedString::from(path.to_owned())),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme.success)
                .child(SharedString::from(format!("+{additions}"))),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme.danger)
                .child(SharedString::from(format!("−{deletions}"))),
        )
}

pub fn conversation_subagent_chip_frame(
    id: SharedString,
    rail: bool,
    height: f32,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .when(rail, |el| el.ml(px(12.0)))
        .h(px(height))
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .overflow_hidden()
        .rounded(px(9.0))
        .border_1()
        .border_color(crate::theme::hairline(0.07))
        .bg(crate::theme::ink(0.03))
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::ink(0.05)))
}

pub fn conversation_answer_input(theme: &Theme, selector: String) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface)
        .px(crate::typography::ui_rems(8.0))
        .py(crate::typography::ui_rems(5.0))
}

pub fn conversation_question_id(theme: &Theme, id: SharedString) -> Div {
    div()
        .px(crate::typography::ui_rems(7.0))
        .h(crate::typography::ui_rems(18.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(5.0)))
        .bg(theme.text.opacity(0.10))
        .text_size(crate::typography::ui_rems(11.0))
        .line_height(crate::typography::ui_rems(16.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(id)
}

pub fn conversation_question_card(
    theme: &Theme,
    id: SharedString,
    accent: Hsla,
    dimmed: bool,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .border_l_2()
        .border_color(accent.opacity(0.7))
        .bg(theme.text.opacity(0.04))
        .rounded_tr(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .rounded_br(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .pl(crate::typography::ui_rems(12.0))
        .pr(crate::typography::ui_rems(10.0))
        .py(crate::typography::ui_rems(8.0))
        .text_color(theme.text_muted)
        .when(dimmed, |card| card.opacity(0.75))
}

pub fn conversation_footer_slot(
    height: f32,
    bottom_slot: f32,
    new_opacity: f32,
    session_opacity: f32,
    new_selectors: Option<AnyElement>,
    footer: Option<AnyElement>,
    usage: Option<AnyElement>,
) -> Div {
    div()
        .w_full()
        .h(px(height * bottom_slot))
        .mt(px(-Theme::SPACE_SM * (1.0 - bottom_slot)))
        .mb(px(-Theme::SPACE_SM * bottom_slot))
        .relative()
        .when(new_opacity > 0.0, |slot| {
            slot.child(
                div()
                    .absolute()
                    .inset_0()
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .opacity(new_opacity)
                    .children(new_selectors),
            )
        })
        .when(session_opacity > 0.0, |slot| {
            slot.child(
                div()
                    .absolute()
                    .inset_0()
                    .w_full()
                    .h(px(height))
                    .flex()
                    .items_center()
                    .opacity(session_opacity)
                    .child(div().flex_1().min_w_0().children(footer))
                    .children(usage.map(|content| div().flex_none().pr(px(10.0)).child(content))),
            )
        })
}

pub fn conversation_tool_detail_frame() -> Div {
    div().w_full().min_w_0().flex().flex_col().overflow_hidden()
}

pub fn conversation_tool_output_line(theme: &Theme, height: f32, text: SharedString) -> Div {
    div()
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .text_color(theme.text_faint)
        .child(text)
}

pub fn conversation_thought_line(height: f32) -> Div {
    div().h(px(height)).w_full().min_w_0().flex().items_center()
}

pub fn conversation_tool_more_lines(
    theme: &Theme,
    count: usize,
    height: f32,
    text_size: f32,
) -> Div {
    div()
        .h(px(height))
        .flex()
        .items_center()
        .text_size(px(text_size))
        .text_color(theme.text_faint)
        .child(SharedString::from(format!("… {count} more lines")))
}

pub fn conversation_tool_group_header(
    theme: &Theme,
    id: SharedString,
    height: f32,
    text_size: f32,
    line_height: f32,
) -> gpui::Stateful<Div> {
    let hover_text = theme.text;
    div()
        .id(id)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .pr(px(4.0))
        .h(px(height))
        .cursor_pointer()
        .text_size(px(text_size))
        .line_height(px(line_height))
        .text_color(theme.text_muted)
        .hover(move |s| s.text_color(hover_text))
}

pub fn conversation_tool_chip_frame(
    rail: bool,
    row_height: f32,
    card_height: f32,
    gap: f32,
    reveal: f32,
) -> Div {
    div()
        .when(rail, |el| el.ml(px(gap)))
        .my(px((row_height - card_height) / 2.0))
        .h(px(card_height))
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .overflow_hidden()
        .when(!rail, |card| {
            card.rounded(px(9.0))
                .border_1()
                .border_color(crate::theme::hairline(0.07))
                .bg(crate::theme::ink(0.03))
        })
        .when(rail && reveal < 1.0, |card| {
            card.relative()
                .top(px(4.0 * (1.0 - reveal)))
                .opacity(reveal)
        })
}

pub fn conversation_code_icon_action(
    theme: &Theme,
    id: SharedString,
    icon: &'static str,
    size: f32,
) -> gpui::Stateful<Div> {
    let fade_key = id.to_string();
    div()
        .id(id)
        .size(px(size))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(crate::motion::hover_blend(
            &fade_key,
            gpui::transparent_black(),
            crate::theme::ink(0.08),
        ))
        .on_hover(crate::motion::hover_listener(fade_key))
        .child(
            crate::icons::icon(icon)
                .size(px(13.0))
                .text_color(theme.text_muted),
        )
}

pub fn conversation_code_copy_action(
    theme: &Theme,
    id: SharedString,
    copied: bool,
    size: f32,
) -> gpui::Stateful<Div> {
    let fade_key = id.to_string();
    div()
        .id(id)
        .h(px(size))
        .px(px(6.0))
        .rounded(px(5.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .bg(crate::motion::hover_blend(
            &fade_key,
            gpui::transparent_black(),
            crate::theme::ink(0.08),
        ))
        .on_hover(crate::motion::hover_listener(fade_key))
        .text_size(px(10.5))
        .text_color(theme.text_muted)
        .child(
            crate::icons::icon(if copied {
                crate::icons::CHECK
            } else {
                crate::icons::COPY
            })
            .size(px(12.0))
            .text_color(theme.text_muted),
        )
        .children(conversation_code_copy_label(theme, copied))
}

pub fn conversation_tool_icon_tile(
    theme: &Theme,
    activity: bool,
    icon: &'static str,
) -> Option<Div> {
    (!activity).then(|| {
        div()
            .size(px(18.0))
            .flex_none()
            .rounded(px(5.0))
            .bg(crate::theme::ink(0.08))
            .flex()
            .items_center()
            .justify_center()
            .child(
                crate::icons::icon(icon)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
    })
}

pub fn conversation_tool_header(
    activity: bool,
    height: f32,
    text_size: f32,
    line_height: f32,
) -> Div {
    div()
        .group("tool-header")
        .h(px(height))
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(if activity { 0.0 } else { 8.0 }))
        .text_size(px(text_size))
        .line_height(px(line_height))
}

pub fn conversation_error_chip(theme: &Theme, message: SharedString) -> AnyElement {
    div()
        .py(px(4.0))
        .w_full()
        .child(
            crate::notice::notice_chip(
                theme,
                false,
                "Error",
                message,
                crate::notice::NoticeChipIcon::Tile,
            )
            .overflow_hidden()
            .w_full(),
        )
        .into_any_element()
}

pub fn conversation_add_harness_key(theme: &Theme) -> gpui::Stateful<Div> {
    div()
        .id("area-add-harness")
        .role(gpui::Role::Button)
        .aria_label("Add harness")
        .size(px(28.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text_muted)
        .cursor_pointer()
        .child(
            crate::icons::icon(crate::icons::PLUS)
                .size(px(16.0))
                .text_color(theme.text_muted),
        )
}

pub fn conversation_reveal<T: gpui::Styled + gpui::InteractiveElement>(
    element: T,
    theme: &Theme,
    group: impl Into<SharedString>,
    visible: bool,
) -> T {
    crate::ui_style::reveal_on_hover(element, group, theme.ui_style, visible)
}

pub fn conversation_markdown_gap(theme: &Theme, fallback: f32) -> f32 {
    theme.ui_style.tokens.grid.unwrap_or(fallback)
}

pub fn conversation_markdown_text_size(theme: &Theme, size: f32, body_size: f32) -> f32 {
    if size == body_size {
        theme.ui_style.tokens.body_size.unwrap_or(size)
    } else {
        size
    }
}

pub fn conversation_skill_chip(theme: &Theme, index: usize) -> gpui::Stateful<Div> {
    let area = theme.ui_style.conversation == ConversationVariant::Area;
    div()
        .id(("composer-skill-chip", index))
        .flex_none()
        .h(px(crate::badges::BADGE_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .pl(px(8.0))
        .pr(px(4.0))
        .rounded(px(if area { 12.0 } else { 8.0 }))
        .bg(crate::theme::ink(0.06))
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_muted)
}

pub fn conversation_skill_remove(theme: &Theme, index: usize) -> gpui::Stateful<Div> {
    div()
        .id(("composer-skill-remove", index))
        .role(gpui::Role::Button)
        .aria_label("Remove skill")
        .size(px(16.0))
        .rounded(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::ink(0.08)))
        .child(
            crate::icons::icon(crate::icons::CLOSE)
                .size(px(10.0))
                .text_color(theme.text_muted),
        )
}

pub fn conversation_send_hint(
    theme: &Theme,
    enabled: bool,
    width: f32,
    shortcut: &str,
) -> Option<Div> {
    (theme.ui_style.behavior.hints == crate::ui_style::HintMode::Inline
        && enabled
        && width >= 480.0)
        .then(|| crate::work_outline::hints::kbd_chip(theme, shortcut))
}

pub struct ConversationComposerBody {
    pub pill: Div,
    pub input: AnyElement,
    pub strips: Vec<AnyElement>,
    pub attach: AnyElement,
    pub send: AnyElement,
    pub model: Option<AnyElement>,
    pub area_model: Option<AnyElement>,
    pub send_hint: Option<AnyElement>,
    pub expanded: bool,
    pub pill_height: f32,
    pub textarea_height: f32,
    pub text_padding: f32,
    pub cluster_offset: f32,
    pub expanded_inset: f32,
    pub compact_inset: f32,
    pub text_glide: f32,
    pub surface_width: f32,
}

pub fn conversation_composer_body(theme: &Theme, body: ConversationComposerBody) -> AnyElement {
    use crate::composer::{
        ACTION_PRIMARY_GAP, ACTION_UTILITY_GAP, ACTIONS_ROW_HEIGHT, COMPACT_TOTAL_HEIGHT,
        PILL_BORDER_V,
    };
    let ConversationComposerBody {
        pill,
        input,
        strips,
        attach,
        send,
        model,
        area_model,
        send_hint,
        expanded,
        pill_height,
        textarea_height,
        text_padding,
        cluster_offset,
        expanded_inset,
        compact_inset,
        text_glide,
        surface_width,
    } = body;
    if theme.ui_style.conversation == ConversationVariant::Area {
        return div()
            .flex()
            .flex_col()
            .child(
                pill.h(px(pill_height))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(strips)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .px(px(16.0))
                            .pt(px(text_padding))
                            .child(input),
                    ),
            )
            .child(
                div()
                    .h(px(40.0))
                    .w_full()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .text_color(theme.text_muted)
                            .child(attach)
                            .child("Attach"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(surface_width * 0.55))
                            .children(area_model),
                    )
                    .child(send),
            )
            .into_any_element();
    }
    if expanded {
        pill.h(px(pill_height))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .children(strips)
            .child(
                div()
                    .h(px(textarea_height))
                    .flex_none()
                    .overflow_hidden()
                    .px(px(16.0))
                    .pt(px(text_padding))
                    .pb(px(4.0))
                    .child(input),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-cluster_offset))
                    .h(px(ACTIONS_ROW_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(ACTION_PRIMARY_GAP))
                    .pl(px(12.0))
                    .pr(px(expanded_inset))
                    .pt(px(2.0))
                    .pb(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(ACTION_UTILITY_GAP))
                            .child(attach)
                            .children(model),
                    )
                    .children(send_hint)
                    .child(send),
            )
            .into_any_element()
    } else {
        let has_send_hint = send_hint.is_some();
        pill.h(px(pill_height))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .children(strips)
            .child(
                div()
                    .h(px(COMPACT_TOTAL_HEIGHT - PILL_BORDER_V))
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .pl(px(12.0))
                            .relative()
                            .top(px(-cluster_offset))
                            .child(attach),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(8.0))
                            .relative()
                            .top(px(-text_glide))
                            .child(input),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(surface_width * 0.45))
                            .relative()
                            .top(px(-cluster_offset))
                            .children(model),
                    )
                    .child(
                        div()
                            .flex_none()
                            .pl(px(ACTION_PRIMARY_GAP))
                            .pr(px(compact_inset))
                            .relative()
                            .top(px(-cluster_offset))
                            .when(has_send_hint, |el| el.flex().items_center().gap(px(4.0)))
                            .children(send_hint)
                            .child(send),
                    ),
            )
            .into_any_element()
    }
}

pub fn conversation_input_text_color(theme: &Theme, identity: Hsla) -> Option<Hsla> {
    (theme.ui_style.conversation == ConversationVariant::Area)
        .then(|| theme.identity_text(identity))
}

pub fn conversation_wizard_surface<T: gpui::Styled + gpui::prelude::FluentBuilder>(
    element: T,
    theme: &Theme,
    fallback_radius: f32,
) -> T {
    element
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(fallback_radius)))
        .border_1()
        .border_color(theme.border)
        .bg(theme.input_glass_bg())
        .when(
            !theme.is_frost()
                && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault,
            |el| el.shadow_lg(),
        )
}

pub fn conversation_model_picker(
    theme: &Theme,
    width: f32,
    offset: f32,
    opacity: f32,
    picker: AnyElement,
    measure: AnyElement,
) -> (Option<Div>, Option<Div>) {
    let area = theme.ui_style.conversation == ConversationVariant::Area;
    let picker = div()
        .min_w_0()
        .max_w(px(width * if area { 0.55 } else { 0.45 }))
        .relative()
        .left(px(if area { 0.0 } else { offset }))
        .opacity(if area { 1.0 } else { opacity })
        .when(
            theme.ui_style.conversation == ConversationVariant::Industrial,
            |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(-8.0))
                        .left(px(4.0))
                        .right(px(4.0))
                        .flex()
                        .justify_between()
                        .text_size(crate::typography::ui_rems(8.0))
                        .line_height(crate::typography::ui_rems(8.0))
                        .text_color(theme.text_muted)
                        .child("MODEL")
                        .child("MODE"),
                )
            },
        )
        .child(picker)
        .child(measure);
    if area {
        (None, Some(picker))
    } else {
        (Some(picker), None)
    }
}

pub fn conversation_harness_tabs(
    theme: &Theme,
    tabs: Vec<AnyElement>,
    add: AnyElement,
) -> Option<Div> {
    (theme.ui_style.conversation == ConversationVariant::Area).then(|| {
        div()
            .absolute()
            .left(px(16.0))
            .right(px(16.0))
            .top(px(-28.0))
            .h(px(28.0))
            .flex()
            .items_end()
            .children(tabs)
            .child(div().flex_1())
            .child(add)
    })
}

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
    active_identity: Hsla,
    icon: &'static str,
    selected: bool,
) -> gpui::Stateful<Div> {
    let identity = if selected { active_identity } else { identity };
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
            |el| conversation_industrial_icon_key(el, theme, false),
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
            theme.ui_style.conversation == ConversationVariant::Industrial,
            |el| {
                el.bg(theme.input_bg)
                    .border_color(theme.industrial_colors().rule)
            },
        )
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
        .when(key, |el| conversation_industrial_icon_key(el, theme, true))
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

fn conversation_code_copy_label(theme: &Theme, copied: bool) -> Option<&'static str> {
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
    theme.clone()
}

/// Frame the tool group for the active conversation style.
pub fn conversation_tool_panel(_theme: &Theme, _identity: Hsla, body: AnyElement) -> AnyElement {
    body
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
                .justify_between()
                .px(px(12.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .children(language.map(|lang| SharedString::from(lang.to_string()))),
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
        .when_some(theme.ui_style.tokens.panel_radius, |el, radius| {
            el.rounded(px(radius))
        })
        .bg(crate::theme::ink(0.035))
        .border_1()
        .border_color(theme.border)
        .when(
            theme.ui_style.id == crate::ui_style::UiStyleId::Area,
            |el| {
                el.rounded(px(14.0))
                    .bg(theme.surface_raised)
                    .border_color(theme.surface_raised)
                    .p(px(4.0))
            },
        )
        .overflow_hidden()
        .relative()
        .children(code_block_header(language, actions, theme))
        .child(body)
}

pub fn conversation_question_chip(
    header: SharedString,
    resolved: bool,
    theme: &Theme,
) -> AnyElement {
    let value: SharedString = if resolved {
        header
    } else {
        "Awaiting your answer…".into()
    };
    div()
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .h(px(34.0))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(10.0)))
                .border_1()
                .border_color(crate::theme::hairline(0.08))
                .bg(crate::theme::ink(0.045))
                .px(px(8.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .rounded(px(6.0))
                        .bg(crate::theme::ink(0.09))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::icons::icon(crate::icons::CHAT_ROUND_LINE)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Question")),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.text.opacity(0.9))
                        .child(value),
                ),
        )
        .into_any_element()
}

pub fn conversation_question_pill(
    theme: &Theme,
    label: &str,
    color: Hsla,
    filled: bool,
) -> gpui::Div {
    div()
        .px(crate::typography::ui_rems(7.0))
        .h(crate::typography::ui_rems(18.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(9.0)))
        .border_1()
        .border_color(color.opacity(0.55))
        .when(filled, |pill| pill.bg(color.opacity(0.14)))
        .text_size(crate::typography::ui_rems(11.0))
        .line_height(crate::typography::ui_rems(16.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .child(label.to_owned())
}

pub fn conversation_answer_button(
    id: SharedString,
    selector: String,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let color = if primary {
        theme.accent
    } else {
        theme.text_muted
    };
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .px(crate::typography::ui_rems(8.0))
        .h(crate::typography::ui_rems(20.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(5.0)))
        .cursor_pointer()
        .border_1()
        .border_color(color.opacity(0.5))
        .when(primary, |button| button.bg(color.opacity(0.12)))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(color)
        .hover(|style| style.bg(color.opacity(0.2)))
        .child(label)
}

pub fn conversation_tool_patch_header(
    file: &crate::changes::FileDiff,
    theme: &Theme,
    height: f32,
    text_size: f32,
) -> AnyElement {
    div()
        .h(px(height))
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(7.0))
        .px(px(8.0))
        .border_b_1()
        .border_color(crate::theme::hairline(0.07))
        .bg(crate::theme::ink(0.025))
        .font_family(theme.font_sans_fixed.clone())
        .text_size(px(text_size))
        .child(
            crate::file_icons::icon(
                crate::file_icons::FileIconIdentity::file(&file.path),
                theme.appearance,
            )
            .size(px(14.0))
            .flex_none(),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(theme.text.opacity(0.9))
                .child(SharedString::from(file.path.clone())),
        )
        .when(file.additions > 0, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_color(theme.success)
                    .child(SharedString::from(format!("+{}", file.additions))),
            )
        })
        .when(file.deletions > 0, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_color(theme.danger)
                    .child(SharedString::from(format!("−{}", file.deletions))),
            )
        })
        .into_any_element()
}
