//! Development-only GPUI element inspector.
use gpui::{
    App, ClipboardItem, DivInspectorState, InspectorElementId, Rgba, actions, div, prelude::*, px,
};
use std::{cell::RefCell, rc::Rc};

actions!(mast_dev, [ToggleInspector]);

fn relative_source(path: &str) -> &str {
    path.find("crates/")
        .map(|index| &path[index..])
        .unwrap_or(path)
}

fn color_hex(color: gpui::Hsla) -> String {
    let color: Rgba = color.into();
    let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b),
        byte(color.a)
    )
}

fn format_details(id: &InspectorElementId, state: &DivInspectorState, context: &str) -> String {
    let location = id.path.source_location;
    let bounds = state.bounds;
    let style = &state.base_style;
    let mut lines = vec![
        format!(
            "Source: {}:{}:{}",
            relative_source(location.file()),
            location.line(),
            location.column()
        ),
        format!("ID: {}", id.path.global_id),
        format!(
            "Bounds: x={} y={} width={} height={} px",
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height)
        ),
        format!("Theme: {context}"),
    ];
    macro_rules! field {
        ($label:expr, $value:expr) => {
            if let Some(value) = &$value {
                lines.push(format!("{}: {}", $label, value));
            }
        };
    }
    field!("Width", style.size.width);
    field!("Height", style.size.height);
    field!("Min width", style.min_size.width);
    field!("Min height", style.min_size.height);
    field!("Max width", style.max_size.width);
    field!("Max height", style.max_size.height);
    field!("Padding top", style.padding.top);
    field!("Padding right", style.padding.right);
    field!("Padding bottom", style.padding.bottom);
    field!("Padding left", style.padding.left);
    field!("Margin top", style.margin.top);
    field!("Margin right", style.margin.right);
    field!("Margin bottom", style.margin.bottom);
    field!("Margin left", style.margin.left);
    field!("Gap horizontal", style.gap.width);
    field!("Gap vertical", style.gap.height);
    field!("Radius top left", style.corner_radii.top_left);
    field!("Radius top right", style.corner_radii.top_right);
    field!("Radius bottom right", style.corner_radii.bottom_right);
    field!("Radius bottom left", style.corner_radii.bottom_left);
    field!("Border top", style.border_widths.top);
    field!("Border right", style.border_widths.right);
    field!("Border bottom", style.border_widths.bottom);
    field!("Border left", style.border_widths.left);
    if let Some(color) = style.border_color {
        lines.push(format!("Border color: {}", color_hex(color)));
    }
    if let Some(background) = &style.background {
        if let Some(color) = background
            .color()
            .and_then(|background| background.as_solid())
        {
            lines.push(format!("Background: {}", color_hex(color)));
        } else {
            lines.push(format!("Background: {:?}", background));
        }
    }
    if let Some(color) = style.text.color {
        lines.push(format!("Text color: {}", color_hex(color)));
    }
    field!("Text size", style.text.font_size);
    field!("Text weight", style.text.font_weight);
    field!("Font family", style.text.font_family);
    lines.join("\n")
}

fn copy_details(details: &str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(details.to_owned()));
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ToggleInspector, cx| {
        cx.defer(|cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, cx| window.toggle_inspector(cx));
            }
        });
    });
    let copied = Rc::new(RefCell::new(String::new()));
    cx.register_inspector_element(move |id, state: &DivInspectorState, _, cx| {
        let theme = crate::theme::Theme::of(cx);
        let details = format_details(
            &id,
            state,
            &format!("{:?}, {:?}", theme.ui_style, theme.appearance),
        );
        let label = if *copied.borrow() == details {
            "Copied"
        } else {
            "Copy details"
        };
        let copied = copied.clone();
        let clipboard_text = details.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .text_color(theme.text)
            .child(
                div()
                    .id("copy-inspector-details")
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .bg(theme.surface_raised)
                    .cursor_pointer()
                    .child(label)
                    .on_click(move |_, window, cx| {
                        copy_details(&clipboard_text, cx);
                        *copied.borrow_mut() = clipboard_text.clone();
                        window.refresh();
                        let copied = copied.clone();
                        let text = clipboard_text.clone();
                        let handle = window.window_handle();
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(2))
                                .await;
                            if *copied.borrow() == text {
                                copied.borrow_mut().clear();
                                let _ = handle.update(cx, |_, window, _| window.refresh());
                            }
                        })
                        .detach();
                    }),
            )
            .child(details)
    });
    cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
        let states = inspector.render_inspector_states(window, cx);
        let theme = crate::theme::Theme::of(cx);
        div()
            .id("inspector-panel")
            .size_full()
            .overflow_y_scroll()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .bg(theme.surface)
            .text_color(theme.text)
            .text_size(px(12.))
            .child("Inspector — toggle to pick again")
            .when(inspector.active_element_id().is_none(), |element| {
                element.child("Hover and click an element to inspect it")
            })
            .children(states)
            .into_any_element()
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, Window};

    fn sample_details() -> String {
        let id = gpui::InspectorElementId {
            path: std::rc::Rc::new(gpui::InspectorElementPath {
                global_id: Default::default(),
                source_location: std::panic::Location::caller(),
            }),
            instance_id: 0,
        };
        let mut style = gpui::StyleRefinement::default();
        style.border_color = Some(gpui::rgb(0x123456).into());
        let state = DivInspectorState {
            base_style: Box::new(style),
            bounds: Default::default(),
            content_size: Default::default(),
        };
        format_details(&id, &state, "Area, Dark")
    }

    #[test]
    fn details_include_source_and_colors_without_unset_fields() {
        let details = sample_details();
        assert!(details.contains("crates/ui/src/dev_inspector.rs:"));
        assert_eq!(
            relative_source("/checkout/crates/ui/src/file.rs"),
            "crates/ui/src/file.rs"
        );
        assert!(details.contains("#123456ff"));
        assert!(!details.contains("None"));
        assert!(!details.contains("Padding top"));
    }

    struct Host;
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child("Inspect me")
        }
    }

    #[gpui::test]
    fn inspector_opens_in_picking_mode_and_renders(cx: &mut gpui::TestAppContext) {
        let (_, vcx) = cx.add_window_view(|_, _| Host);
        vcx.update(|window, cx| {
            crate::theme::Theme::install(crate::theme::Appearance::Dark, cx);
            init(cx);
            copy_details(&sample_details(), cx);
            assert!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .unwrap()
                    .contains("crates/ui/src/dev_inspector.rs:")
            );
            crate::app_menus::bind_keys(cx);
            window.activate_window();
            window.draw(cx).clear();
            assert!(!window.is_inspector_picking(cx));
        });
        vcx.simulate_keystrokes("cmd-alt-shift-i");
        vcx.update(|window, cx| {
            assert!(window.is_inspector_picking(cx));
            window.draw(cx).clear();
        });
        vcx.simulate_keystrokes("cmd-alt-shift-i");
        vcx.update(|window, cx| assert!(!window.is_inspector_picking(cx)));
    }
}
