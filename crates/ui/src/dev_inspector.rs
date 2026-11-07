//! Development-only GPUI element inspector.
use gpui::{App, DivInspectorState, actions, div, prelude::*, px};

actions!(mast_dev, [ToggleInspector]);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ToggleInspector, cx| {
        cx.defer(|cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, cx| window.toggle_inspector(cx));
            }
        });
    });
    cx.register_inspector_element(|_, state: &DivInspectorState, _, cx| {
        let theme = crate::theme::Theme::of(cx);
        let style = &state.base_style;
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .text_color(theme.text)
            .child(format!("Bounds: {:?}", state.bounds))
            .child(format!("Size: {:?}", style.size))
            .child(format!("Padding: {:?}", style.padding))
            .child(format!("Radius: {:?}", style.corner_radii))
            .child(format!("Background: {:?}", style.background))
            .child(format!("Border widths: {:?}", style.border_widths))
            .child(format!("Border color: {:?}", style.border_color))
    });
    cx.set_inspector_renderer(Box::new(|inspector, window, cx| {
        let label = inspector
            .active_element_id()
            .map(|id| {
                format!(
                    "ID: {:?}\nSource: {}",
                    id.path.global_id, id.path.source_location
                )
            })
            .unwrap_or_else(|| "Hover and click an element to inspect it".to_owned());
        let states = inspector.render_inspector_states(window, cx);
        let theme = crate::theme::Theme::of(cx);
        div()
            .size_full()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .bg(theme.surface)
            .text_color(theme.text)
            .text_size(px(12.))
            .child("Inspector — toggle to pick again")
            .child(label)
            .children(states)
            .into_any_element()
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, Window};

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
