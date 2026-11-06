mod screen_stubs;

use gpui::{App, Context, IntoElement, Render, Window, div, prelude::*, px};
use mast_ui::{appearance, badges, icons, settings, theme::Theme, typography, ui_style};

// This initial story exercises the actual message badge, including its hover card.
struct Gallery {
    focus: gpui::FocusHandle,
    input: gpui::Entity<mast_ui::composer::ComposerInput>,
    transcript: gpui::Entity<mast_ui::transcript::Transcript>,
    composer: gpui::Entity<mast_ui::composer::Composer>,
    story: Story,
    appearance_page: gpui::Entity<settings::appearance::AppearancePage>,
    toggled: bool,
    menu_open: bool,
    tab: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Story {
    Controls,
    Chat,
    Outline,
    Walkthrough,
    Appearance,
}

const STORIES: [(Story, &str); 5] = [
    (Story::Controls, "Controls"),
    (Story::Chat, "Chat"),
    (Story::Outline, "Work outline"),
    (Story::Walkthrough, "PR walkthrough"),
    (Story::Appearance, "Settings → Appearance"),
];

fn gallery_styles() -> &'static [ui_style::UiStyleId] {
    // TODO: Return UiStyleId::ALL when the shared registry lands.
    &[
        ui_style::UiStyleId::Minimal,
        ui_style::UiStyleId::Dense,
        ui_style::UiStyleId::Guided,
    ]
}

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
                    "l" => switch_appearance(appearance::AppearanceMode::Light, cx),
                    "d" => switch_appearance(appearance::AppearanceMode::Dark, cx),
                    key => {
                        let Some(index) = key.parse::<usize>().ok().filter(|n| (1..=9).contains(n))
                        else {
                            return;
                        };
                        let Some(&style) = gallery_styles().get(index - 1) else {
                            return;
                        };
                        switch_style(style, window, cx);
                    }
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
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(
                        STORIES
                            .into_iter()
                            .enumerate()
                            .map(|(index, (story, label))| {
                                div()
                                    .id(("story", index))
                                    .cursor_pointer()
                                    .px(px(8.))
                                    .py(px(6.))
                                    .when(self.story == story, |el| {
                                        el.rounded(px(5.)).bg(theme.surface_raised)
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.story = story;
                                        cx.notify();
                                    }))
                                    .child(label)
                            }),
                    ),
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
                            .children(gallery_styles().iter().copied().enumerate().map(
                                |(index, id)| {
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
                                },
                            ))
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
                    .when(self.story == Story::Chat, |el| {
                        el.child(
                            div()
                                .flex()
                                .h(px(500.))
                                .child(screen_stubs::sidebar(theme))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .child(div().flex_1().child(self.transcript.clone()))
                                        .child(self.composer.clone()),
                                ),
                        )
                    })
                    .when(self.story == Story::Outline, |el| {
                        el.child(screen_stubs::outline(theme))
                    })
                    .when(self.story == Story::Walkthrough, |el| {
                        el.child(screen_stubs::walkthrough(theme))
                    })
                    .when(self.story == Story::Appearance, |el| {
                        el.child(self.appearance_page.clone())
                    })
                    .when(self.story == Story::Controls, |el| {
                        el.child("Buttons and toggles")
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .items_center()
                                    .child(mast_ui::popover::btn_primary(theme, "Primary"))
                                    .child(mast_ui::popover::btn_ghost(
                                        theme,
                                        "Secondary",
                                        "gallery-secondary",
                                    ))
                                    .child(mast_ui::popover::btn_danger(theme, "Delete"))
                                    .child(
                                        div()
                                            .id("toggle")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggled = !this.toggled;
                                                cx.notify();
                                            }))
                                            .child(mast_ui::settings::widgets::toggle_switch(
                                                theme,
                                                self.toggled,
                                            )),
                                    )
                                    .child(mast_ui::settings::widgets::badge(theme, "Draft"))
                                    .child(mast_ui::settings::widgets::badge_active(
                                        theme, "Active",
                                    )),
                            )
                            .child("Icon buttons, tabs and status")
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .id("icon-plus")
                                            .cursor_pointer()
                                            .p(px(6.))
                                            .rounded(px(5.))
                                            .bg(theme.surface_raised)
                                            .child(
                                                icons::icon(icons::PLUS)
                                                    .size(px(16.))
                                                    .text_color(theme.text),
                                            ),
                                    )
                                    .children(["Overview", "Activity"].into_iter().enumerate().map(
                                        |(index, label)| {
                                            div()
                                                .id(("tab", index))
                                                .cursor_pointer()
                                                .px(px(8.))
                                                .py(px(5.))
                                                .when(self.tab == index, |el| {
                                                    el.bg(theme.surface_raised).rounded(px(5.))
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.tab = index;
                                                    cx.notify();
                                                }))
                                                .child(label)
                                        },
                                    ))
                                    .children(
                                        [
                                            (theme.success, "Ready"),
                                            (theme.warning, "Waiting"),
                                            (theme.danger, "Error"),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(color, label)| {
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(4.))
                                                    .child(
                                                        div().size(px(7.)).rounded_full().bg(color),
                                                    )
                                                    .child(label)
                                            },
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("menu-toggle")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.menu_open = !this.menu_open;
                                        cx.notify();
                                    }))
                                    .child("Menu ▾"),
                            )
                            .when(self.menu_open, |el| {
                                el.child(
                                    mast_ui::popover::popover_card(theme)
                                        .w(px(200.))
                                        .child(mast_ui::popover::menu_heading(theme, "Actions"))
                                        .child(
                                            mast_ui::popover::menu_row(
                                                theme,
                                                false,
                                                "gallery-menu",
                                            )
                                            .child("Open"),
                                        ),
                                )
                            })
                            .child("Input")
                            .child(
                                div()
                                    .w(px(360.))
                                    .p(px(8.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .rounded(px(6.))
                                    .child(self.input.clone()),
                            )
                            .child("Message badge")
                            .child(badges::render("gallery-badge", &badge, theme))
                    }),
            )
    }
}

fn gallery(focus: gpui::FocusHandle, cx: &mut Context<Gallery>) -> Gallery {
    use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
    let state = cx.new(|_| {
        let mut state = mast_ui::state::AppState::new();
        let entry = |id: &str, role, text: &str| SessionMessageEntry {
            id: id.into(), role, parts: vec![MessagePart::Text { id: format!("{id}-text"), text: text.into() }],
            created_at: 0, device_id: "fixture".into(), status: Some(MessageStatus::Complete), continuation_of: None,
        };
        state.set_subagent_snapshot("gallery".into(), vec![
            entry("user", MessageRole::User, "Can you review the changes?"),
            entry("assistant", MessageRole::Assistant, "I found **two improvements**.\n\n- Simplify the state update\n- Add a regression test"),
        ]);
        state
    });
    Gallery {
        focus,
        input: cx.new(|cx| mast_ui::composer::ComposerInput::new("Type a message…", cx)),
        transcript: cx.new(|cx| {
            mast_ui::transcript::Transcript::for_doc(state.clone(), "gallery".into(), false, cx)
        }),
        composer: cx.new(|cx| mast_ui::composer::Composer::new(state.clone(), cx)),
        story: Story::Controls,
        appearance_page: cx.new(settings::appearance::AppearancePage::new),
        toggled: false,
        menu_open: false,
        tab: 0,
    }
}

fn init(cx: &mut App, data_dir: &std::path::Path) {
    gpui_base::init(cx);
    let settings = settings::UiSettings::default();
    settings::init(settings.clone(), data_dir, cx);
    let fonts = typography::register_fonts(cx);
    ui_style::init(settings.ui_style, cx);
    mast_ui::composer::init(cx, settings.composer_send_behavior);
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
                    gallery(focus, cx)
                })
            })
            .expect("gallery window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn stories_render_in_each_style_and_appearance(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| init(cx, dir.path()));
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            gallery(focus, cx)
        });
        for &style in gallery_styles() {
            for mode in [
                appearance::AppearanceMode::Light,
                appearance::AppearanceMode::Dark,
            ] {
                cx.update(|cx| switch_appearance(mode, cx));
                for (story, _) in STORIES {
                    window
                        .update(cx, |gallery, _, cx| {
                            gallery.story = story;
                            gallery.menu_open = story == Story::Controls;
                            gallery.toggled = true;
                            cx.notify();
                        })
                        .unwrap();
                    cx.update_window(window.into(), |_, window, cx| {
                        switch_style(style, window, cx);
                        window.refresh();
                        window.draw(cx).clear();
                    })
                    .unwrap();
                }
            }
        }
    }
}
