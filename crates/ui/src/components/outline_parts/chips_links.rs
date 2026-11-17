use super::*;

/// Compact list in an item-details section.
pub fn outline_details_list(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0 * super::layout_spacing(theme)))
}

/// Expand/collapse control in an outline row.
pub fn outline_disclosure(id: SharedString, collapsed: bool, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .size(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(if collapsed {
                crate::icons::ALT_ARROW_RIGHT
            } else {
                crate::icons::ALT_ARROW_DOWN
            })
            .size(px(10.0))
            .text_color(theme.text_muted)
            .when(super::is_horizon(theme), |el| {
                el.opacity(0.65)
                    .group_hover("outline-row", |el| el.opacity(1.0))
            }),
        )
}
pub fn outline_disclosure_space() -> Div {
    div().flex_none().size(px(14.0))
}

/// Small inline action in an outline row.
pub fn outline_inline_action(
    id: SharedString,
    selector: String,
    label: &'static str,
    color: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text));
    crate::surface_chrome::segmented_key(button, theme, false)
        .when(super::is_horizon(theme), |el| el.rounded_full())
        .child(label)
}

/// Waiting date displayed in an outline row.
pub fn outline_waiting_chip(label: String, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-waiting-chip".into())
        .flex_none()
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(label))
}

/// Icon and count in an outline row.
pub fn outline_count_marker(
    id: SharedString,
    icon: &'static str,
    count: String,
    color: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(color)
        .child(crate::icons::icon(icon).size(px(10.0)).text_color(color))
        .child(SharedString::from(count))
}

/// Compact glyph marker in an outline row.
pub fn outline_glyph_marker(id: SharedString, glyph: &'static str, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(glyph)
}

/// External reference marker in an outline row.
pub fn outline_reference_marker(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .child(
            crate::icons::icon(crate::icons::GLOBAL)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
}

/// Harness badge on an outline chat row.
pub fn outline_harness_badge(harness: String, theme: &Theme) -> Div {
    div()
        .flex_none()
        .max_w(px(100.0))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .bg(theme.element_hover.opacity(0.7))
        .truncate()
        .text_size(crate::typography::ui_rems(
            9.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted.opacity(0.85))
        .child(SharedString::from(harness))
}

/// Branch or pull-request marker on an outline chat row.
pub fn outline_chat_reference(
    icon: &'static str,
    label: String,
    max_width: Option<f32>,
    theme: &Theme,
) -> Div {
    div()
        .flex_none()
        .when_some(max_width, |el, width| el.max_w(px(width)))
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
        .child(
            crate::icons::icon(icon)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
        .when(max_width.is_some(), |el| {
            el.child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(label.clone())),
            )
        })
        .when(max_width.is_none(), |el| {
            el.child(SharedString::from(label))
        })
}

/// Warning chip for a Linear link requiring repair.
pub fn outline_linear_fix(id: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child("Fix link")
}

/// Linked Linear issue chip.
pub fn outline_linear_issue(
    id: String,
    text: String,
    dot: gpui::Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(180.0))
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
        .child(div().min_w_0().truncate().child(SharedString::from(text)))
}

/// Linked Linear project chip.
pub fn outline_linear_project(id: String, name: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(140.0))
        .flex()
        .items_center()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
        .child(div().min_w_0().truncate().child(SharedString::from(name)))
}

/// Linear destination and retained-item markers.
pub fn outline_linear_destination(id: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * super::layout_spacing(theme)))
        .px(px(3.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(10.0))
                .text_color(theme.accent),
        )
        .child("↓")
}
pub fn outline_linear_retained(id: String, text: String, theme: &Theme) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(160.0))
        .px(px(4.0 * super::layout_spacing(theme)))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(3.0)))
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .text_size(crate::typography::ui_rems(
            10.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.warning)
        .child(div().truncate().child(SharedString::from(text)))
}

/// Heading for an overview list lane.
pub fn outline_lane_heading(text: String, color: gpui::Hsla, depth: usize, theme: &Theme) -> Div {
    let top = depth == 0;
    let label = if top && !super::is_horizon(theme) {
        text.to_uppercase()
    } else {
        text
    };
    div()
        .flex_none()
        .text_color(color)
        .border_color(color)
        .when(top, |el| {
            el.text_size(crate::typography::ui_rems(
                13.0 + super::layout_type_adjustment(theme),
            ))
            .font_weight(gpui::FontWeight::BOLD)
            .pb(px(6.0 * super::layout_spacing(theme)))
            .border_b_2()
        })
        .when(!top, |el| {
            el.text_size(crate::typography::ui_rems(
                10.0 + super::layout_type_adjustment(theme),
            ))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .pb(px(3.0 * super::layout_spacing(theme)))
            .border_b_1()
        })
        .child(SharedString::from(label))
}
