//! Components used by shell chrome.

use crate::{icons::icon, motion, theme::Theme};
use gpui::{AnyElement, App, IntoElement, MouseButton, SharedString, Window, div, prelude::*, px};

pub(crate) fn sidebar_separator(theme: &Theme) -> gpui::Div {
    div()
        .h(px(if super::is_area(theme) {
            0.0
        } else {
            theme.ui_style.tokens.rule_width.unwrap_or(1.0)
        }))
        .bg(if theme.ui_style.tokens.rule_width.is_some() {
            theme.border
        } else {
            theme.border.opacity(0.6)
        })
}

pub(crate) fn sidebar_section_label(theme: &Theme, label: SharedString) -> gpui::Div {
    let emphasized = super::is_industrial(theme) || super::is_area(theme);
    let label = if emphasized {
        SharedString::from(crate::popover::tracked_upper(&label))
    } else {
        label
    };
    div()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.label_size.unwrap_or(12.0),
        ))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if emphasized {
            theme.text_muted
        } else {
            theme.text_muted.opacity(0.5)
        })
        .child(label)
}

pub(crate) fn chat_header_chip(
    id: SharedString,
    selector: String,
    theme: &Theme,
    border: gpui::Hsla,
    text: gpui::Hsla,
    model: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .max_w(px(230.0))
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(border)
        .bg(theme.element_hover.opacity(0.55))
        .when(
            theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque,
            |el| el.bg(theme.surface_raised),
        )
        .when(super::is_area(theme), |el| el.rounded_full())
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(text)
        .when(model && super::is_industrial(theme), |chip| {
            let dark = theme.appearance.is_dark();
            chip.rounded(px(0.0))
                .bg(if dark { theme.bg } else { theme.solid })
                .border_color(theme.industrial_colors().rule)
                .font_family(theme.font_mono.clone())
                .text_color(if dark { theme.text } else { theme.on_solid })
        })
}

pub(crate) fn chat_header_chip_icon_color(
    theme: &Theme,
    text: gpui::Hsla,
    model: bool,
    link: bool,
) -> gpui::Hsla {
    if model && super::is_industrial(theme) {
        if theme.appearance.is_dark() {
            theme.text
        } else {
            theme.on_solid
        }
    } else if link {
        theme.accent
    } else {
        text
    }
}

/// A size-6 icon button for the titlebar strip (zeron window-controls.tsx:
/// `grid size-6 place-items-center rounded-md text-muted-foreground`).
pub(crate) fn window_control_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let muted = theme.text_muted;
    let fade_key = format!("window-control-{id}");
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .cursor_pointer()
        // zeron window-controls.tsx: `transition-colors` — the wash fades.
        .bg(motion::hover_blend(
            &fade_key,
            theme.glass_hover().opacity(0.0),
            theme.glass_hover(),
        ))
        .when(super::is_area(theme), |el| {
            el.rounded_full().bg(theme.identity_color("").opacity(0.12))
        })
        .when(true, |el| super::icon_key(el, theme, false))
        .on_hover(motion::hover_listener(fade_key))
        // Buttons in/over a titlebar drag strip must be EXCLUDED from the
        // strip's event surface entirely. `.occlude()` (gpui
        // `HitboxBehavior::BlockMouse`) makes the window hit-test STOP at the
        // button, so every `is_hovered`-guarded strip listener — the
        // mouse-down that arms the drag, the mouse-move that hands AppKit a
        // native drag session (`performWindowDragWithEvent:`, whose second
        // quick click zooms NATIVELY on macOS), and the `click_count == 2`
        // zoom handler — never fires with the pointer over a button. It also
        // removes the button's rect from the native Drag control-area
        // hit-test on Windows/Linux. The click-level stop_propagation is
        // zed's ButtonLike belt on top. Double-click on EMPTY strip space
        // still zooms — nothing occludes it there.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

/// A titlebar history button (zeron window-controls.tsx): enabled it is a
/// normal window-control button; disabled it dims to 35% opacity and ignores
/// the pointer (`disabled:pointer-events-none disabled:opacity-35`).
pub(crate) fn nav_history_button(
    id: &'static str,
    icon_path: &'static str,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    if !enabled {
        return div()
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            // Even disabled it reads as a control — occlude so double-clicks
            // on it don't fall through to the titlebar strip's zoom handler.
            .occlude()
            .child(
                icon(icon_path)
                    .size(px(16.0))
                    .text_color(theme.text_muted.opacity(0.35)),
            )
            .into_any_element();
    }
    window_control_button(id, icon_path, theme, on_click).into_any_element()
}

/// A size-7 icon button for the main-panel header (zeron __root.tsx:
/// `grid size-7 place-items-center rounded-md text-muted-foreground`).
pub(crate) fn header_icon_button(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    labels_fit: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let muted = theme.text_muted;
    let show_label =
        labels_fit && theme.ui_style.behavior.labels != crate::ui_style::LabelMode::IconOnly;
    let fade_key = format!("header-icon-{id}");
    let rest = if super::is_area(theme) {
        theme.identity_color("").opacity(0.12)
    } else {
        crate::theme::wash(0.0)
    };
    div()
        .id(id)
        .size(px(theme.ui_style.tokens.control_size))
        .when(show_label, |el| el.w_auto().px(px(6.0)).gap(px(4.0)))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.ui_style.tokens.control_radius.unwrap_or(6.0)))
        .when(super::is_area(theme), |el| {
            el.rounded_full().bg(theme.identity_color("").opacity(0.12))
        })
        .when(true, |el| super::icon_key(el, theme, false))
        .cursor_pointer()
        // zeron __root.tsx header buttons: `transition-colors`.
        .when(
            theme.ui_style.behavior.motion == crate::ui_style::Motion::Soft,
            |el| {
                el.bg(motion::hover_blend(
                    &fade_key,
                    rest,
                    crate::theme::wash(0.11),
                ))
                .on_hover(motion::hover_listener(fade_key))
            },
        )
        .when(
            theme.ui_style.behavior.motion == crate::ui_style::Motion::Instant,
            |el| el.hover(|el| el.bg(crate::theme::wash(0.11))),
        )
        // Same occlusion + click-swallowing as [`window_control_button`]: this
        // button sits inside the chat header's titlebar drag region, so its
        // rect must be carved out of the strip's drag/double-click surface.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(
            icon(icon_path)
                .size(px(theme.ui_style.tokens.control_icon_size))
                .text_color(muted),
        )
        .when(show_label, |el| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(muted)
                    .child(label),
            )
        })
}
