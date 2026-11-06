//! Presentation primitives specific to work outlines and item details.

use crate::theme::Theme;
use gpui::{Div, SharedString, Stateful, div, prelude::*, px};

fn section_label(label: String, industrial: bool) -> (String, Option<String>) {
    if !industrial {
        return (label, None);
    }
    match label.rsplit_once(" (") {
        Some((title, suffix))
            if suffix.ends_with(')')
                && suffix[..suffix.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
        {
            (
                crate::popover::tracked_upper(title),
                Some(format!("({suffix}")),
            )
        }
        _ => (crate::popover::tracked_upper(&label), None),
    }
}

/// A collapsible item-details section heading; the caller attaches its action.
pub fn outline_section_header(
    id: &'static str,
    label: String,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;
    let (label, count) = section_label(label, industrial);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(4.0 * super::layout_spacing(theme)))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(
            theme.ui_style.tokens.label_size.unwrap_or(11.0) + super::layout_type_adjustment(theme),
        ))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .when(industrial, |el| el.font_family(theme.font_sans.clone()))
        .text_color(theme.text_muted)
        .hover(|el| el.text_color(theme.text))
        .child(
            crate::icons::icon(if open {
                crate::icons::ALT_ARROW_DOWN
            } else {
                crate::icons::ALT_ARROW_RIGHT
            })
            .size(px(10.0))
            .text_color(theme.text_muted),
        )
        .child(SharedString::from(label))
        .when_some(count, |el, count| {
            el.child(
                div()
                    .font_family(theme.font_mono.clone())
                    .child(SharedString::from(count)),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::section_label;

    #[test]
    fn only_industrial_tracks_section_labels() {
        assert_eq!(
            section_label("Child items (12)".into(), false),
            ("Child items (12)".into(), None)
        );
        let (label, count) = section_label("Child items (12)".into(), true);
        assert_eq!(label, crate::popover::tracked_upper("Child items"));
        assert_eq!(count.as_deref(), Some("(12)"));
    }
}
