//! Adapters for rendering shared components with fixture data in the gallery.
use crate::{components::shell_parts, theme::Theme};
use gpui::{AnyElement, div, prelude::*, px};

/// The same row frame and colors used by the session sidebar.
pub fn sidebar_row(theme: &Theme, id: String, label: &str, selected: bool) -> AnyElement {
    let (text, selected_background, dot) =
        shell_parts::sidebar_row_colors(theme, "fixture", selected);
    let background = if selected {
        selected_background
    } else {
        gpui::transparent_black()
    };
    shell_parts::sidebar_row_frame(
        theme,
        id.clone(),
        theme.ui_style.tokens.sidebar_row_compact_h,
        false,
        &id,
        text,
        theme.text,
        background,
        theme.element_hover,
    )
    .child(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children(dot)
            .child(label.to_owned()),
    )
    .into_any_element()
}
