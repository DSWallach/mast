use super::*;

/// Root surface for the outline tree.
pub fn outline_tree(theme: &Theme) -> Stateful<Div> {
    div()
        .id("work-outline")
        .relative()
        .debug_selector(|| "work-outline".into())
        .when(
            theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque,
            |el| el.bg(theme.surface),
        )
        .when(super::is_horizon(theme), |el| el.bg(theme.bg))
        .when_some(theme.ui_style.design_roles.outline_page_surface, |el, page| el.bg(if page { theme.bg } else { theme.surface }))
}

pub fn outline_overview_header(theme: &Theme) -> Div {
    div().when(super::is_horizon(theme), |el| el.bg(theme.bg))
        .when_some(theme.ui_style.design_roles.outline_page_surface, |el, page| el.bg(if page { theme.bg } else { theme.surface }))
}

/// Horizon presents descendants inside a continuous rounded surface per ancestor.
pub fn outline_uses_rounded_nesting(theme: &Theme) -> bool {
    super::is_horizon(theme)
}

pub fn outline_nested_row(key: &str, depth: u16, previous: u16, next: u16, row: gpui::AnyElement, theme: &Theme) -> gpui::AnyElement {
    let offset = |level: u16| level as f32 * 20.0;
    let backgrounds = (1..=depth).map(|level| {
        let selector = format!("horizon-outline-nesting-{key}-{level}");
        div().id(selector.clone()).debug_selector(move || selector.clone())
            .absolute().top_0().bottom_0().right_0().left(px(offset(level)))
            .when(level <= 2, |el| el.bg(theme.text.opacity(0.025)))
            .when(previous < level, |el| el.rounded_tl(px(12.)).rounded_tr(px(12.)))
            .when(next < level, |el| el.rounded_bl(px(12.)).rounded_br(px(12.)))
    });
    div().relative().w_full().children(backgrounds)
        .child(div().w_full().flex().pl(px(offset(depth))).child(row))
        .into_any_element()
}

/// Row chrome; selection, drag handling and content remain with the caller.
pub fn outline_row(id: SharedString, height: f32, indent: f32, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let row = div()
        .id(id)
        .group("outline-row")
        .debug_selector(move || selector.clone())
        .h(px(height))
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme.ui_style.tokens.grid.unwrap_or(4.0)))
        .pl(px(indent))
        .pr(px(8.0))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.body_size.unwrap_or(12.5),
        ))
        .border_1()
        .border_color(gpui::transparent_black());
    super::design_surface(row, theme, crate::style_overrides::DesignSurface::OutlineRow)
}

pub fn outline_row_theme(theme: &Theme, selected: bool) -> Theme {
    let mut result = theme.clone();
    if selected && theme.ui_style.design_roles.primary_colour.is_some() {
        let colour = theme.ui_style.design_roles.primary_colour.map(|role| role.colour(theme)).unwrap_or(theme.accent);
        result.text = if theme.ui_style.design_roles.surface(crate::style_overrides::DesignSurface::OutlineRow).page_colour.is_some() { theme.text } else { theme.identity_text(colour) };
        result.text_muted = result.text;
        result.text_faint = result.text.opacity(0.8);
    }
    result
}

pub fn outline_row_selection<T: gpui::Styled>(element: T, theme: &Theme, selected: bool) -> T {
    let element = if super::is_horizon(theme) && selected {
        element
            .max_w(px(400.0))
            .rounded_full()
            .bg(theme.text.opacity(0.10))
            .border_color(gpui::transparent_black())
    } else {
        element
    };
    let element = if let Some(radius) = theme.ui_style.design_roles.panel_radius { element.rounded(px(radius)) } else { element };
    let element = if let Some(role) = theme.ui_style.design_roles.primary_colour.filter(|_| selected) {
        let colour = role.colour(theme); element.bg(colour).border_color(colour)
    } else { element };
    super::design_surface(element, theme, crate::style_overrides::DesignSurface::OutlineRow)
}

pub fn outline_row_extras(theme: &Theme) -> bool {
    !super::is_horizon(theme)
}

pub fn outline_attention_color(
    theme: &Theme,
    needs_attention: bool,
    color: gpui::Hsla,
) -> gpui::Hsla {
    if super::is_horizon(theme) && needs_attention {
        theme.accent
    } else {
        color
    }
}

pub fn outline_selected_dot_color(theme: &Theme, selected: bool, color: gpui::Hsla) -> gpui::Hsla {
    if super::is_horizon(theme) && selected && theme.ui_style.design_roles.primary_colour.is_some() {
        theme.identity_text(theme.accent)
    } else {
        color
    }
}

pub fn outline_row_highlight(theme: &Theme) -> gpui::Hsla {
    if super::is_horizon(theme) {
        gpui::transparent_black()
    } else {
        theme.accent.opacity(0.6)
    }
}

pub fn outline_space_dot(
    theme: &Theme,
    id: &str,
    needs_attention: bool,
    selected: bool,
) -> Option<Div> {
    super::is_horizon(theme).then(|| {
        div()
            .flex_none()
            .size(px(8.0))
            .rounded_full()
            .bg(outline_selected_dot_color(
                theme,
                selected,
                if needs_attention {
                    theme.accent
                } else {
                    theme.identity_color(id)
                },
            ))
    })
}

pub fn outline_missing_chat(theme: &Theme, id: &str) -> Option<gpui::AnyElement> {
    super::is_horizon(theme).then(|| {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(theme.text_muted)
            .child(SharedString::from(format!("Missing chat · {id}")))
            .into_any_element()
    })
}

/// Overview filter menu trigger.
pub fn outline_filter_trigger(theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let trigger = div()
        .id("overview-filter-menu")
        .debug_selector(|| "overview-filter-menu".into())
        .relative()
        .px(px(8.0 * scale))
        .py(px(3.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_color(theme.text_muted)
        .bg(theme.element_hover.opacity(0.5));
    super::toolbar_control(
        crate::surface_chrome::segmented_key(trigger, theme, false),
        theme,
        false,
    )
    .when(super::is_horizon(theme), |el| el.rounded_full())
    .child("⋯")
}

/// Checkable overview filter menu row.
pub fn outline_filter_row(
    id: &'static str,
    label: &'static str,
    checked: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    let row = div()
        .id(id)
        .debug_selector(move || id.into())
        .px(px(8.0 * scale))
        .py(px(5.0 * scale))
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            11.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text)
        .hover(|el| el.bg(theme.element_hover));
    crate::surface_chrome::segmented_key(row, theme, checked)
        .when(super::is_horizon(theme), |el| el.rounded_full())
        .child(format!("{}  {}", if checked { "✓" } else { "　" }, label))
}

/// Compact metadata chip used on outline rows.
pub fn outline_metadata_chip(label: String, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .flex_none()
        .px(px(5.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.accent)
        .child(SharedString::from(label))
}

/// A compact path label for an associated work item.
pub fn outline_path_chip(id: SharedString, label: String, theme: &Theme) -> Stateful<Div> {
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(|| "work-item-path-chip".into())
        .min_w_0()
        .max_w(px(160.0))
        .px(px(5.0 * scale))
        .py(px(scale))
        .rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(4.0)))
        .border_1()
        .border_color(theme.border)
        .truncate()
        .text_size(crate::typography::ui_rems(
            9.0 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_muted)
        .child(SharedString::from(label))
}

/// Status marker and count for a pending user action.
pub fn outline_attention(id: SharedString, count: u32, theme: &Theme) -> Stateful<Div> {
    let color =
        crate::status_style::status_style(crate::status_style::Status::Waiting, theme.appearance)
            .color;
    div()
        .id(id)
        .debug_selector(|| "work-item-attention".into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            9.5 + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(crate::status_style::indicator_for_theme(
            crate::status_style::Status::Waiting,
            theme,
        ))
        .when(count > 1, |el| {
            el.child(SharedString::from(count.to_string()))
        })
}

/// Required-action row surface.
pub fn outline_required_action_row(id: SharedString, theme: &Theme) -> Stateful<Div> {
    let selector = id.to_string();
    let scale = super::layout_spacing(theme);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(2.0 * scale))
        .px(px(6.0 * scale))
        .py(px(4.0 * scale))
        .rounded(px(theme.ui_style.tokens.panel_radius.unwrap_or(4.0)))
        .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
        .when(super::is_industrial(theme), |el| {
            el.border_1().border_color(theme.industrial_colors().rule)
        })
        .when(super::is_horizon(theme), |el| el.py(px(8.0 * scale)))
}

/// Template editor node header.
pub fn outline_template_header(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
}

/// Template editor node and its nested descendants.
pub fn outline_template_node(depth: usize, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0 * super::layout_spacing(theme)))
        .when(depth > 0, |el| {
            el.ml(px(10.0))
                .pl(px(6.0))
                .border_l_1()
                .border_color(theme.border)
        })
}

/// Proposal content row in item details.
pub fn outline_proposal_row(selector: String, theme: &Theme) -> Div {
    let scale = super::layout_spacing(theme);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(4.0 * scale))
        .pl(px(14.0))
        .py(px(2.0 * scale))
}

/// Proposal provenance metadata line.
pub fn outline_proposal_provenance(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .text_size(crate::typography::ui_rems(
            10.5 + super::layout_type_adjustment(theme),
        ))
        .text_color(theme.text_faint)
}

/// Waiting-date section and its label.
pub fn outline_waiting_section(theme: &Theme) -> Div {
    div()
        .debug_selector(|| "work-item-waiting-control".into())
        .flex()
        .flex_col()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .child(
            div()
                .text_color(theme.text_muted)
                .text_size(crate::typography::ui_rems(
                    11.0 + super::layout_type_adjustment(theme),
                ))
                .when(super::is_horizon(theme), |el| {
                    el.text_size(crate::typography::ui_rems(
                        theme.ui_style.tokens.body_size.unwrap_or(13.0)
                            + super::layout_type_adjustment(theme),
                    ))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .child("Waiting until…"),
        )
}

/// Preserve the original Default Minimal footer while sizing Archive in other appearances.
pub fn outline_details_footer(archived: bool, theme: &Theme) -> Div {
    let original = theme.ui_style.id == crate::ui_style::UiStyleId::Default
        && theme.ui_style.layout == crate::ui_style::UiLayout::Minimal;
    div().when(!archived && !original, |el| el.flex().items_start())
}
