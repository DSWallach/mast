//! Presentation primitives for changes and review walkthrough surfaces.

use crate::theme::Theme;
use gpui::{prelude::*, px};

pub(crate) fn file_toolbar_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let guided = theme.ui_style.behavior.labels == crate::ui_style::LabelMode::All;
    gpui::div()
        .id(id)
        .size(px(crate::surface_chrome::CONTROL_SIZE))
        .when(super::is_industrial(theme), |element| {
            element.size(px(theme.ui_style.tokens.control_size))
        })
        .flex_none()
        .when(guided, |element| {
            element
                .h(px(theme.ui_style.tokens.control_size))
                .w_auto()
                .min_w(px(theme.ui_style.tokens.control_size))
                .flex_shrink_1()
                .px(px(6.0))
                .gap(px(4.0))
                .flex_row_reverse()
                .child(
                    gpui::div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(label),
                )
        })
        .rounded(px(theme
            .ui_style
            .tokens
            .control_radius
            .unwrap_or(crate::surface_chrome::CONTROL_RADIUS)))
        .when(true, |element| {
            crate::surface_chrome::icon_key(element, theme, false)
        })
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(label)
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default()
        })
        .hover(|style| style.bg(crate::theme::wash(0.14)))
}

pub(crate) fn changes_loading(
    theme: &Theme,
    id: &'static str,
    owner: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {
        crate::status_style::indicator_for_theme(crate::status_style::Status::Working, theme)
            .into_any_element()
    } else {
        crate::loaders::gradient_spinner(id, theme, 2.0, owner, cx).into_any_element()
    }
}

pub(crate) fn explanation_trigger<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .bg(theme.surface_raised)
        .when(super::is_industrial(theme), |element| {
            element.border_1().border_b_2().border_color(theme.border)
        })
}

pub(crate) fn walkthrough_title<T: Styled>(element: T, theme: &Theme) -> T {
    element.text_size(crate::typography::ui_rems(
        theme.ui_style.tokens.title_size.unwrap_or(13.0),
    ))
}

pub(crate) fn walkthrough_chip<T: Styled>(element: T, theme: &Theme) -> T {
    element.rounded(px(theme.ui_style.tokens.chip_radius.unwrap_or(6.0)))
}

pub(crate) fn walkthrough_guidance<T: FluentBuilder>(
    element: T,
    theme: &Theme,
    build: impl FnOnce(T) -> T,
) -> T {
    element.when(theme.ui_style.behavior.help_cards, build)
}

pub(crate) fn walkthrough_hints<T: FluentBuilder>(
    element: T,
    theme: &Theme,
    build: impl FnOnce(T) -> T,
) -> T {
    element.when(
        theme.ui_style.behavior.hints == crate::ui_style::HintMode::Inline,
        build,
    )
}

pub(crate) fn walkthrough_note_more<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(super::is_industrial(theme), |element| {
        element
            .px(px(8.0))
            .py(px(4.0))
            .bg(theme.surface_raised)
            .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
            .border_1()
            .border_color(theme.border)
    })
}

pub(crate) fn walkthrough_note_more_visible(
    theme: &Theme,
    expanded: bool,
    height: f32,
    limit: f32,
) -> bool {
    !super::is_industrial(theme) || expanded || height > limit + 0.5
}

pub(crate) fn walkthrough_note_content(
    what: String,
    why: String,
    risk: String,
    measured: std::rc::Rc<std::cell::Cell<f32>>,
    owner: gpui::EntityId,
    theme: &Theme,
) -> gpui::AnyElement {
    let why = format!("Why: {why}");
    if super::is_industrial(theme) {
        let what = gpui::StyledText::new(what);
        let why = gpui::StyledText::new(why);
        let risk = gpui::StyledText::new(risk);
        let layouts = [
            what.layout().clone(),
            why.layout().clone(),
            risk.layout().clone(),
        ];
        gpui::div()
            .relative()
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, _, _, cx| {
                        let height = layouts
                            .iter()
                            .map(|layout| {
                                let lines: usize = layout
                                    .line_layouts()
                                    .iter()
                                    .map(|line| line.wrap_boundaries.len() + 1)
                                    .sum();
                                lines.max(1) as f32 * f32::from(layout.line_height())
                            })
                            .sum::<f32>()
                            + 8.0;
                        if (measured.get() - height).abs() > 0.5 {
                            measured.set(height);
                            cx.notify(owner);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .flex()
            .flex_col()
            .gap_1()
            .child(what)
            .child(gpui::div().text_color(theme.text_muted).child(why))
            .child(gpui::div().text_color(theme.text_muted).child(risk))
            .into_any_element()
    } else {
        gpui::div()
            .flex()
            .flex_col()
            .gap_1()
            .child(what)
            .child(gpui::div().text_color(theme.text_muted).child(why))
            .child(gpui::div().text_color(theme.text_muted).child(risk))
            .into_any_element()
    }
}

pub(crate) fn walkthrough_empty_decoration(theme: &Theme, empty: bool) -> Option<gpui::Div> {
    (empty && super::is_industrial(theme)).then(|| crate::surface_chrome::grille(theme))
}

pub(crate) fn walkthrough_review_action<T: Styled + ParentElement + FluentBuilder>(
    element: T,
    theme: &Theme,
    reviewed: bool,
) -> T {
    let industrial = super::is_industrial(theme);
    let element = element.when(industrial, |element| {
        element
            .self_start()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(8.0))
    });
    let element = crate::surface_chrome::segmented_key(element, theme, reviewed);
    element
        .when(!reviewed && industrial, |element| {
            let foreground = if crate::theme::contrast_ratio(theme.text, theme.success)
                >= crate::theme::contrast_ratio(theme.bg, theme.success)
            {
                theme.text
            } else {
                theme.bg
            };
            element.bg(theme.success).text_color(foreground)
        })
        .children(crate::surface_chrome::active_key_dot(theme, reviewed))
}

pub(crate) fn walkthrough_progress<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(super::is_industrial(theme), |element| {
        element.font_family(theme.font_mono.clone())
    })
}

pub(crate) fn walkthrough_progress_label(progress: String, theme: &Theme) -> gpui::AnyElement {
    if super::is_industrial(theme) {
        crate::surface_chrome::display(theme)
            .child(progress)
            .into_any_element()
    } else {
        gpui::div().child(progress).into_any_element()
    }
}

pub(crate) fn walkthrough_mode<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(super::is_industrial(theme), |element| {
        element
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(4.0)))
            .bg(theme.surface_raised)
            .border_1()
            .border_b_2()
            .border_color(theme.border)
    })
}

pub(crate) fn walkthrough_step_row<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(super::is_industrial(theme), |row| {
        row.px(px(8.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
    })
}

pub(crate) fn walkthrough_navigation<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(
        theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque
            && theme.ui_style.id != crate::ui_style::UiStyleId::Area,
        |element| {
            element
                .bg(theme.surface)
                .p(px(8.0))
                .border_r_1()
                .border_color(theme.border)
        },
    )
}

pub(crate) fn walkthrough_notes<T: Styled + FluentBuilder>(element: T, theme: &Theme) -> T {
    element.when(
        theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque,
        |element| {
            element
                .bg(theme.surface)
                .p(px(8.0))
                .border_l_1()
                .border_color(theme.border)
        },
    )
}
