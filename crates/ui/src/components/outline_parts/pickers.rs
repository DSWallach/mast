use super::*;

/// Outline drag preview.
pub fn outline_drag_preview(title: SharedString, theme: &Theme) -> Div {
    div()
        .max_w(px(260.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_raised)
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .truncate()
        .child(title)
}
pub fn outline_first_use(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "outline-first-use".into())
        .flex_none()
        .w_full()
        .min_w_0()
        .px(px(14.0))
        .my(px(6.0))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            11.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(
            div()
                .debug_selector(|| "outline-first-use-copy".into())
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .child("No work items yet — group Chats and share instructions under one."),
        )
}
pub fn outline_editor_frame(submitting: bool, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(20.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.surface)
        .overflow_hidden()
        .when(submitting, |el| el.opacity(0.6))
}

/// Outline move/associate picker chrome.
pub fn outline_picker(theme: &Theme) -> Stateful<Div> {
    div()
        .id("outline-picker")
        .debug_selector(|| "outline-picker".into())
        .mx(px(8.0))
        .mb(px(6.0))
        .p(px(6.0 * super::layout_spacing(theme)))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(6.0)))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.surface_raised)
}
pub fn outline_picker_entry(
    index: usize,
    depth: u32,
    label: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(("outline-picker-entry", index))
        .debug_selector(move || format!("outline-picker-entry-{index}"))
        .h(px(24.0 * super::layout_spacing(theme)))
        .flex()
        .items_center()
        .pl(px(8.0 + depth as f32 * 12.0))
        .pr(px(8.0))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            12.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .when(active, |el| el.bg(theme.element_active))
        .hover(|el| el.bg(theme.element_hover))
        .child(div().min_w_0().truncate().child(SharedString::from(label)))
}
pub fn outline_picker_place(label: &'static str, active: bool, theme: &Theme) -> Stateful<Div> {
    let selector = format!("outline-picker-place-{}", label.to_lowercase());
    let button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .px(px(6.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .when(active, |el| {
            el.bg(theme.element_active).text_color(theme.text)
        })
        .when(!active, |el| el.hover(|el| el.bg(theme.element_hover)));
    crate::surface_chrome::segmented_key(button, theme, active)
        .when(super::is_horizon(theme), |el| el.rounded_full())
        .child(label)
}
