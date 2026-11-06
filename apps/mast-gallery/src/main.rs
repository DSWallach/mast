use gpui::{App, Context, IntoElement, Render, Window, div, prelude::*, px};
use mast_ui::{appearance, badges, icons, settings, theme::Theme, typography, ui_style};

// This initial story exercises the actual message badge, including its hover card.
struct Gallery {
    focus: gpui::FocusHandle,
}

const STYLES: [ui_style::UiStyleId; 3] = [
    ui_style::UiStyleId::Minimal,
    ui_style::UiStyleId::Dense,
    ui_style::UiStyleId::Guided,
];

fn switch_style(id: ui_style::UiStyleId, window: &mut Window, cx: &mut App) {
    ui_style::set_style_in_memory(id, cx);
    window.set_rem_size(px(typography::effective_rem_px(cx)));
}

fn switch_appearance(mode: appearance::AppearanceMode, cx: &mut App) {
    let settings = settings::current(cx);
    appearance::init(
        mode,
        settings.theme_selection,
        settings.accent,
        settings.surface,
        cx,
    );
    cx.refresh_windows();
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let badge = badges::MessageBadge {
            icon: icons::GIT_BRANCH,
            label: "ui-gallery".into(),
            details: vec![badges::BadgeDetail {
                location: "src/main.rs:42".into(),
                tag: Some("Review".into()),
                body: "Fixture detail".into(),
            }],
        };
        div()
            .size_full()
            .flex()
            .bg(theme.surface)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "1" => switch_style(STYLES[0], window, cx),
                    "2" => switch_style(STYLES[1], window, cx),
                    "3" => switch_style(STYLES[2], window, cx),
                    "l" => switch_appearance(appearance::AppearanceMode::Light, cx),
                    "d" => switch_appearance(appearance::AppearanceMode::Dark, cx),
                    _ => return,
                }
                cx.notify();
            }))
            .child(
                div()
                    .w(px(200.))
                    .h_full()
                    .p(px(20.))
                    .border_r_1()
                    .border_color(theme.border)
                    .child("Controls"),
            )
            .child(
                div()
                    .flex_1()
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .children(STYLES.into_iter().enumerate().map(|(index, id)| {
                                div()
                                    .id(("style", index))
                                    .px(px(8.))
                                    .py(px(4.))
                                    .rounded(px(4.))
                                    .bg(theme.surface_raised)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        switch_style(id, window, cx);
                                        cx.notify();
                                    }))
                                    .child(format!("{} {:?}", index + 1, id))
                            }))
                            .child(
                                div()
                                    .id("light")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        switch_appearance(appearance::AppearanceMode::Light, cx)
                                    }))
                                    .child("Light (L)"),
                            )
                            .child(
                                div()
                                    .id("dark")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        switch_appearance(appearance::AppearanceMode::Dark, cx)
                                    }))
                                    .child("Dark (D)"),
                            ),
                    )
                    .child("Message badge")
                    .child(badges::render("gallery-badge", &badge, theme)),
            )
    }
}

fn init(cx: &mut App, data_dir: &std::path::Path) {
    gpui_base::init(cx);
    let settings = settings::UiSettings::default();
    settings::init(settings.clone(), data_dir, cx);
    let fonts = typography::register_fonts(cx);
    ui_style::init(settings.ui_style, cx);
    typography::init(
        settings.ui_font_family,
        settings.ui_font_size,
        settings.terminal_font_family,
        settings.terminal_font_size,
        settings.code_font_family,
        settings.code_font_size,
        fonts,
        cx,
    );
    appearance::init(
        settings.appearance,
        settings.theme_selection,
        settings.accent,
        settings.surface,
        cx,
    );
}

fn main() {
    let data_dir = tempfile::tempdir().expect("gallery temporary settings directory");
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            init(cx, data_dir.path());
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    Gallery { focus }
                })
            })
            .expect("gallery window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_story_renders_headlessly() {
        gpui_platform::headless().run(|cx| {
            let dir = tempfile::tempdir().unwrap();
            init(cx, dir.path());
            let window = cx
                .open_window(Default::default(), |window, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        Gallery { focus }
                    })
                })
                .unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .unwrap();
        });
    }
}
