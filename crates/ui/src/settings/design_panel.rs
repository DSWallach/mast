//! Developer controls for the current style's live override values.
use crate::{
    settings::{self, SavePolicy},
    style_overrides::{self, ColourRole, HeadingFamily, HeadingWeight, TokenOverrides},
    theme::Theme,
    ui_style::{UiLayout, UiStyle, UiStyleId},
};
use gpui::{Bounds, Context, Pixels, Render, SharedString, Window, div, prelude::*, px};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy)]
enum Token {
    ControlRadius,
    PanelRadius,
    ChipRadius,
    BubbleRadius,
    ControlHeight,
    RowHeight,
    Grid,
    RuleWidth,
}
impl Token {
    const ALL: [Self; 8] = [
        Self::ControlRadius,
        Self::PanelRadius,
        Self::ChipRadius,
        Self::BubbleRadius,
        Self::ControlHeight,
        Self::RowHeight,
        Self::Grid,
        Self::RuleWidth,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::ControlRadius => "Control radius",
            Self::PanelRadius => "Panel radius",
            Self::ChipRadius => "Chip radius",
            Self::BubbleRadius => "Bubble radius",
            Self::ControlHeight => "Control height",
            Self::RowHeight => "Sidebar row height",
            Self::Grid => "Grid",
            Self::RuleWidth => "Rule width",
        }
    }
    fn max(self) -> f32 {
        match self {
            Self::ControlHeight | Self::RowHeight => 80.0,
            Self::RuleWidth => 8.0,
            _ => 40.0,
        }
    }
    fn get(self, t: &TokenOverrides) -> Option<f32> {
        match self {
            Self::ControlRadius => t.control_radius,
            Self::PanelRadius => t.panel_radius,
            Self::ChipRadius => t.chip_radius,
            Self::BubbleRadius => t.bubble_radius,
            Self::ControlHeight => t.control_height,
            Self::RowHeight => t.row_height,
            Self::Grid => t.grid,
            Self::RuleWidth => t.rule_width,
        }
    }
    fn set(self, t: &mut TokenOverrides, value: Option<f32>) {
        match self {
            Self::ControlRadius => t.control_radius = value,
            Self::PanelRadius => t.panel_radius = value,
            Self::ChipRadius => t.chip_radius = value,
            Self::BubbleRadius => t.bubble_radius = value,
            Self::ControlHeight => t.control_height = value,
            Self::RowHeight => t.row_height = value,
            Self::Grid => t.grid = value,
            Self::RuleWidth => t.rule_width = value,
        }
    }
    fn base_description(self, style: UiStyle) -> String {
        let t = style.tokens;
        let value = match self {
            Self::ControlRadius => t.control_radius,
            Self::PanelRadius => t.panel_radius,
            Self::ChipRadius => t.chip_radius,
            Self::BubbleRadius => t.bubble_radius,
            Self::Grid => t.grid,
            Self::RuleWidth => t.rule_width,
            Self::ControlHeight => Some(t.control_size),
            Self::RowHeight => Some(t.sidebar_row_compact_h),
        };
        value
            .map(|v| format!("{v}px"))
            .unwrap_or_else(|| "component default".into())
    }
    fn base(self, style: UiStyle) -> f32 {
        let t = style.tokens;
        match self {
            Self::ControlRadius => t.control_radius.unwrap_or(8.0),
            Self::PanelRadius => t.panel_radius.unwrap_or(12.0),
            Self::ChipRadius => t.chip_radius.unwrap_or(8.0),
            Self::BubbleRadius => t.bubble_radius.unwrap_or(12.0),
            Self::ControlHeight => t.control_size,
            Self::RowHeight => t.sidebar_row_compact_h,
            Self::Grid => t.grid.unwrap_or(8.0),
            Self::RuleWidth => t.rule_width.unwrap_or(1.0),
        }
    }
}
fn style_name(id: UiStyleId) -> &'static str {
    match id {
        UiStyleId::Default => "default",
        UiStyleId::Industrial => "industrial",
        UiStyleId::Area => "area",
    }
}
fn layout_name(layout: UiLayout) -> &'static str {
    match layout {
        UiLayout::Minimal => "minimal",
        UiLayout::Compact => "compact",
        UiLayout::Guided => "guided",
    }
}
#[derive(Clone)]
struct SliderDrag;
struct SliderGhost;
impl Render for SliderGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct DesignPanel {
    bounds: Vec<Rc<Cell<Option<Bounds<Pixels>>>>>,
    message: Option<String>,
}
impl DesignPanel {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            bounds: Token::ALL
                .iter()
                .map(|_| Rc::new(Cell::new(None)))
                .collect(),
            message: None,
        }
    }
    fn set(&mut self, token: Token, value: Option<f32>, cx: &mut Context<Self>) {
        let mut overrides = style_overrides::current(cx);
        let style = crate::ui_style::current(cx);
        let entry = overrides
            .styles
            .entry(style_name(style.id).into())
            .or_default();
        token.set(&mut entry.tokens, value);
        // A layout-specific value would otherwise mask the change in this view.
        if let Some(layout) = entry.layouts.get_mut(layout_name(style.layout)) {
            token.set(layout, None);
        }
        style_overrides::preview(overrides, cx);
        cx.notify();
    }
    fn set_role(index: usize, value: Option<bool>, cx: &mut Context<Self>) {
        let style = crate::ui_style::current(cx);
        let mut overrides = style_overrides::current(cx);
        let entry = overrides
            .styles
            .entry(style_name(style.id).into())
            .or_default();
        if index == 0 {
            entry.tokens.details_border = value;
        } else {
            entry.tokens.details_page_surface = value;
        }
        if let Some(layout) = entry.layouts.get_mut(layout_name(style.layout)) {
            if index == 0 {
                layout.details_border = None;
            } else {
                layout.details_page_surface = None;
            }
        }
        style_overrides::preview(overrides, cx);
        cx.notify();
    }
    fn set_colour(primary: bool, value: Option<ColourRole>, cx: &mut Context<Self>) {
        let style = crate::ui_style::current(cx);
        let mut overrides = style_overrides::current(cx);
        let entry = overrides
            .styles
            .entry(style_name(style.id).into())
            .or_default();
        if primary {
            entry.tokens.primary_colour = value;
        } else {
            entry.tokens.secondary_colour = value;
        }
        if let Some(layout) = entry.layouts.get_mut(layout_name(style.layout)) {
            if primary {
                layout.primary_colour = None;
            } else {
                layout.secondary_colour = None;
            }
        }
        style_overrides::preview(overrides, cx);
        cx.notify();
    }
    fn set_heading(kind: usize, option: usize, cx: &mut Context<Self>) {
        let style = crate::ui_style::current(cx);
        let mut overrides = style_overrides::current(cx);
        let entry = overrides
            .styles
            .entry(style_name(style.id).into())
            .or_default();
        let apply = |tokens: &mut TokenOverrides, option| match kind {
            0 => {
                tokens.heading_family = match option {
                    1 => Some(HeadingFamily::Sans),
                    2 => Some(HeadingFamily::Mono),
                    _ => None,
                }
            }
            1 => {
                tokens.heading_weight = match option {
                    1 => Some(HeadingWeight::Regular),
                    2 => Some(HeadingWeight::Medium),
                    3 => Some(HeadingWeight::Semibold),
                    4 => Some(HeadingWeight::Bold),
                    _ => None,
                }
            }
            _ => {
                tokens.tracked_caps = match option {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                }
            }
        };
        apply(&mut entry.tokens, option);
        if let Some(layout) = entry.layouts.get_mut(layout_name(style.layout)) {
            apply(layout, 0);
        }
        style_overrides::preview(overrides, cx);
        cx.notify();
    }
    fn drag(&mut self, index: usize, token: Token, x: Pixels, cx: &mut Context<Self>) {
        if let Some(bounds) = self.bounds[index].get() {
            let fraction = (f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.0))
                .clamp(0.0, 1.0);
            self.set(token, Some((fraction * token.max()).round()), cx);
        }
    }
    fn button(label: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(label)
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(theme.surface_raised)
            .cursor_pointer()
            .child(label)
    }
}
impl Render for DesignPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let enabled = settings::current(cx).developer_design_enabled;
        let current = crate::ui_style::current(cx);
        let mut page = div()
            .id("developer-settings")
            .size_full()
            .overflow_y_scroll()
            .child(div().text_size(px(20.0)).child("Developer"));
        page = page.child(
            Self::button(
                if enabled {
                    "Disable Design"
                } else {
                    "Enable Design"
                },
                &theme,
            )
            .on_click(cx.listener(|_, _, _, cx| {
                settings::update(SavePolicy::Immediate, cx, |s| {
                    s.developer_design_enabled = !s.developer_design_enabled
                });
                cx.notify();
            })),
        );
        if !enabled {
            return page;
        }
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(format!("Design · {}", current.id.label()));
        for (index, token) in Token::ALL.into_iter().enumerate() {
            let value = token.base(current);
            let fraction = (value / token.max()).clamp(0.0, 1.0);
            let bounds = self.bounds[index].clone();
            let slider = div()
                .id(SharedString::from(format!("design-slider-{index}")))
                .relative()
                .w(px(220.0))
                .h(px(20.0))
                .cursor_pointer()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                        this.drag(index, token, e.position.x, cx)
                    }),
                )
                .on_drag(SliderDrag, |drag, _, _, cx| {
                    let _ = drag;
                    cx.new(|_| SliderGhost)
                })
                .on_drag_move(cx.listener(
                    move |this, e: &gpui::DragMoveEvent<SliderDrag>, _, cx| {
                        this.drag(index, token, e.event.position.x, cx)
                    },
                ))
                .child(
                    gpui::canvas(move |rect, _, _| bounds.set(Some(rect)), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(8.0))
                        .w_full()
                        .h(px(4.0))
                        .bg(theme.border)
                        .child(div().w(gpui::relative(fraction)).h_full().bg(theme.accent)),
                );
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().w(px(170.0)).child(token.label()))
                    .child(slider)
                    .child(format!("{value:.0}px"))
                    .child(
                        Self::button("Reset", &theme)
                            .id(SharedString::from(format!("reset-{index}")))
                            .on_click(cx.listener(move |this, _, _, cx| this.set(token, None, cx))),
                    ),
            );
        }
        for (index, label) in [(0, "Details border"), (1, "Details uses page colour")] {
            let roles = current.design_roles;
            let selected = if index == 0 {
                roles
                    .details_border
                    .unwrap_or(current.id != UiStyleId::Area)
            } else {
                roles
                    .details_page_surface
                    .unwrap_or(current.id == UiStyleId::Area)
            };
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().w(px(200.0)).child(label))
                    .child(
                        Self::button(if selected { "On" } else { "Off" }, &theme)
                            .id(SharedString::from(format!("design-role-{index}")))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                Self::set_role(index, Some(!selected), cx);
                            })),
                    )
                    .child(
                        Self::button("Reset", &theme)
                            .id(SharedString::from(format!("design-role-reset-{index}")))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                Self::set_role(index, None, cx);
                            })),
                    ),
            );
        }
        for (primary, label) in [
            (true, "Primary action colour"),
            (false, "Secondary action colour"),
        ] {
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(200.0)).child(label));
            for role in [
                ColourRole::Accent,
                ColourRole::Text,
                ColourRole::Muted,
                ColourRole::Success,
                ColourRole::Danger,
            ] {
                let selected = if primary {
                    current.design_roles.primary_colour
                } else {
                    current.design_roles.secondary_colour
                } == Some(role);
                let button = Self::button(role.label(), &theme)
                    .id(SharedString::from(format!(
                        "colour-{primary}-{}",
                        role.label()
                    )))
                    .when(selected, |el| el.border_1().border_color(theme.accent));
                row = row.child(button.on_click(
                    cx.listener(move |_, _, _, cx| Self::set_colour(primary, Some(role), cx)),
                ));
            }
            row = row.child(
                Self::button("Reset", &theme)
                    .id(SharedString::from(format!("colour-reset-{primary}")))
                    .on_click(cx.listener(move |_, _, _, cx| Self::set_colour(primary, None, cx))),
            );
            content = content.child(row);
        }
        for (kind, label, options) in [
            (0, "Settings heading family", &["Reset", "Sans", "Mono"][..]),
            (
                1,
                "Settings heading weight",
                &["Reset", "Regular", "Medium", "Semibold", "Bold"][..],
            ),
            (
                2,
                "Settings heading tracked caps",
                &["Reset", "Off", "On"][..],
            ),
        ] {
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(200.0)).child(label));
            for (option, &name) in options.iter().enumerate() {
                row = row.child(
                    Self::button(name, &theme)
                        .id(SharedString::from(format!("heading-{kind}-{option}")))
                        .on_click(
                            cx.listener(move |_, _, _, cx| Self::set_heading(kind, option, cx)),
                        ),
                );
            }
            content = content.child(row);
        }
        content = content.child(
            div()
                .flex()
                .gap(px(8.0))
                .child(
                    Self::button("Reset style", &theme).on_click(cx.listener(|_, _, _, cx| {
                        let mut overrides = style_overrides::current(cx);
                        overrides
                            .styles
                            .remove(style_name(crate::ui_style::current(cx).id));
                        style_overrides::preview(overrides, cx);
                        cx.notify();
                    })),
                )
                .child(
                    Self::button("Save", &theme).on_click(cx.listener(|this, _, _, cx| {
                        this.message = Some(
                            match style_overrides::save(&style_overrides::current(cx), cx) {
                                Ok(()) => "Saved".into(),
                                Err(error) => format!("Save failed: {error}"),
                            },
                        );
                        cx.notify();
                    })),
                )
                .child(
                    Self::button("Copy patch", &theme).on_click(cx.listener(|_, _, _, cx| {
                        let style = crate::ui_style::current(cx);
                        let defaults = UiStyle::resolve(style.id, style.layout);
                        let overrides = style_overrides::current(cx);
                        let tokens = overrides
                            .styles
                            .get(style_name(style.id))
                            .map(|s| s.tokens.clone())
                            .unwrap_or_default();
                        let mut patch = format!("Style: {}\n", style.id.label());
                        for token in Token::ALL {
                            if let Some(value) = token.get(&tokens) {
                                patch.push_str(&format!(
                                    "{}: {} → {}\n",
                                    token.label(),
                                    token.base_description(defaults),
                                    value
                                ));
                            }
                        }
                        if let Some(value) = tokens.details_border {
                            patch.push_str(&format!(
                                "Details border: {} → {}\n",
                                style.id != UiStyleId::Area,
                                value
                            ));
                        }
                        if let Some(value) = tokens.details_page_surface {
                            patch.push_str(&format!(
                                "Details uses page colour: {} → {}\n",
                                style.id == UiStyleId::Area,
                                value
                            ));
                        }
                        if let Some(role) = tokens.primary_colour {
                            patch.push_str(&format!(
                                "Primary action colour: component default → {}\n",
                                role.label()
                            ));
                        }
                        if let Some(role) = tokens.secondary_colour {
                            patch.push_str(&format!(
                                "Secondary action colour: component default → {}\n",
                                role.label()
                            ));
                        }
                        if let Some(v) = tokens.heading_family {
                            patch.push_str(&format!(
                                "Settings heading family: component default → {v:?}\n"
                            ));
                        }
                        if let Some(v) = tokens.heading_weight {
                            patch.push_str(&format!("Settings heading weight: Semibold → {v:?}\n"));
                        }
                        if let Some(v) = tokens.tracked_caps {
                            patch.push_str(&format!(
                                "Settings heading tracked caps: {} → {v}\n",
                                style.id == UiStyleId::Industrial
                            ));
                        }
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(patch));
                    })),
                ),
        );
        if let Some(message) = &self.message {
            content = content.child(message.clone());
        }
        page.child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn developer_panel_renders_with_and_without_design_enabled(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            settings::init(Default::default(), directory.path(), cx);
        });
        let window = cx.add_window(|_, cx| DesignPanel::new(cx));
        window
            .update(cx, |panel, window, cx| {
                let _ = panel.render(window, cx).into_any_element();
            })
            .unwrap();
        cx.update(|cx| {
            settings::update(SavePolicy::Immediate, cx, |settings| {
                settings.developer_design_enabled = true
            });
        });
        window
            .update(cx, |panel, window, cx| {
                let _ = panel.render(window, cx).into_any_element();
            })
            .unwrap();
    }
}
