mod screen_stubs;
#[cfg(target_os = "macos")]
mod screens;
mod stories;

use gpui::{App, Context, IntoElement, Render, Window, div, prelude::*, px};
use mast_ui::{appearance, badges, icons, settings, theme::Theme, typography, ui_style};

// This initial story exercises the actual message badge, including its hover card.
struct Gallery {
    focus: gpui::FocusHandle,
    input: gpui::Entity<mast_ui::composer::ComposerInput>,
    transcript: gpui::Entity<mast_ui::transcript::Transcript>,
    composer: gpui::Entity<mast_ui::composer::Composer>,
    story: Story,
    #[cfg(target_os = "macos")]
    screen_view: Option<(screens::Screen, gpui::AnyView)>,
    data_dir: std::path::PathBuf,
    appearance_page: gpui::Entity<settings::appearance::AppearancePage>,
    toggled: bool,
    menu_open: bool,
    tab: usize,
    dark: bool,
    themes: mast_theme::ThemeSelection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Story {
    Controls,
    Chat,
    Outline,
    Walkthrough,
    Appearance,
    Board,
    Component(stories::Story),
    #[cfg(target_os = "macos")]
    Screen(screens::Screen),
}

const STORIES: [(Story, &str); 6] = [
    (Story::Controls, "Controls"),
    (Story::Chat, "Chat"),
    (Story::Outline, "Work outline"),
    (Story::Walkthrough, "PR walkthrough"),
    (Story::Appearance, "Settings → Appearance"),
    (Story::Board, "Board"),
];

struct GalleryState {
    story: Story,
    style: ui_style::UiStyleId,
    layout: ui_style::UiLayout,
    dark: bool,
    themes: mast_theme::ThemeSelection,
}

impl GalleryState {
    fn load() -> Self {
        Self::load_from(
            std::env::var_os("MAST_GALLERY_STATE")
                .as_deref()
                .map(std::path::Path::new),
        )
    }

    fn load_from(path: Option<&std::path::Path>) -> Self {
        let settings = settings::UiSettings::default();
        let mut result = Self {
            story: Story::Controls,
            style: settings.ui_style,
            layout: settings.ui_layout,
            dark: false,
            themes: settings.theme_selection,
        };
        let Some(path) = path else {
            return result;
        };
        let Ok(contents) = std::fs::read_to_string(path) else {
            return result;
        };
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "story" => {
                    if let Some(&(story, _)) = STORIES.iter().find(|(_, label)| *label == value) {
                        result.story = story;
                    }
                    #[cfg(target_os = "macos")]
                    if let Some(&(screen, _)) =
                        screens::SCREENS.iter().find(|(_, label)| *label == value)
                    {
                        result.story = Story::Screen(screen);
                    }
                    if let Some(&(story, _)) =
                        stories::STORIES.iter().find(|(_, label)| *label == value)
                    {
                        result.story = Story::Component(story);
                    }
                }
                "style" => {
                    if let Some(&style) =
                        ui_style::UiStyleId::ALL.iter().find(|s| s.label() == value)
                    {
                        result.style = style;
                    }
                }
                "layout" => {
                    if let Some(&layout) =
                        ui_style::UiLayout::ALL.iter().find(|l| l.label() == value)
                    {
                        result.layout = layout;
                    }
                }
                "dark" => result.dark = value == "true",
                "light_theme" => result.themes.light = value.to_owned(),
                "dark_theme" => result.themes.dark = value.to_owned(),
                _ => {}
            }
        }
        result
    }

    fn save(&self) {
        let Some(path) = std::env::var_os("MAST_GALLERY_STATE") else {
            return;
        };
        if let Err(error) = self.save_to(std::path::Path::new(&path)) {
            eprintln!("Cannot save gallery state: {error}");
        }
    }

    fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let label = match self.story {
            #[cfg(target_os = "macos")]
            Story::Screen(screen) => screens::SCREENS
                .iter()
                .find(|(s, _)| *s == screen)
                .map(|(_, label)| *label),
            Story::Component(story) => stories::STORIES
                .iter()
                .find(|(s, _)| *s == story)
                .map(|(_, label)| *label),
            story => STORIES
                .iter()
                .find(|(s, _)| *s == story)
                .map(|(_, label)| *label),
        }
        .unwrap_or("Controls");
        let contents = format!(
            "story={label}\nstyle={}\nlayout={}\ndark={}\nlight_theme={}\ndark_theme={}\n",
            self.style.label(),
            self.layout.label(),
            self.dark,
            self.themes.light,
            self.themes.dark
        );
        std::fs::write(path, contents)
    }
}

fn gallery_styles() -> &'static [ui_style::UiStyleId] {
    ui_style::UiStyleId::ALL
}

fn switch_style(id: ui_style::UiStyleId, window: &mut Window, cx: &mut App) {
    ui_style::set_style_in_memory(id, cx);
    window.set_rem_size(px(typography::effective_rem_px(cx)));
}

#[cfg(test)]
fn switch_appearance(mode: appearance::AppearanceMode, cx: &mut App) {
    let settings = settings::current(cx);
    apply_themes(mode, settings.theme_selection, cx);
}

fn apply_themes(
    mode: appearance::AppearanceMode,
    themes: mast_theme::ThemeSelection,
    cx: &mut App,
) {
    let settings = settings::current(cx);
    appearance::init(mode, themes, settings.accent, settings.surface, cx);
    cx.refresh_windows();
}

impl Gallery {
    fn save_state(&self, cx: &App) {
        let style = ui_style::current(cx);
        GalleryState {
            story: self.story,
            style: style.id,
            layout: style.layout,
            dark: self.dark,
            themes: self.themes.clone(),
        }
        .save();
    }
    fn mode(&self) -> appearance::AppearanceMode {
        if self.dark {
            appearance::AppearanceMode::Dark
        } else {
            appearance::AppearanceMode::Light
        }
    }

    fn set_dark(&mut self, dark: bool, cx: &mut App) {
        self.dark = dark;
        apply_themes(self.mode(), self.themes.clone(), cx);
        self.save_state(cx);
    }

    /// Cycles the active appearance through every built-in theme variant.
    fn cycle_theme(&mut self, cx: &mut App) {
        let appearance = if self.dark {
            mast_theme::Appearance::Dark
        } else {
            mast_theme::Appearance::Light
        };
        let ids: Vec<String> = mast_theme::ThemeRegistry::builtin()
            .variants_for(appearance)
            .map(|variant| variant.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let current = self.themes.variant_id(appearance).to_owned();
        let next = ids
            .iter()
            .position(|id| *id == current)
            .map_or(0, |ix| (ix + 1) % ids.len());
        self.themes.set_variant(appearance, ids[next].clone());
        apply_themes(self.mode(), self.themes.clone(), cx);
        self.save_state(cx);
    }

    fn theme_label(&self) -> String {
        let appearance = if self.dark {
            mast_theme::Appearance::Dark
        } else {
            mast_theme::Appearance::Light
        };
        format!("Theme: {} (T)", self.themes.variant_id(appearance))
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(target_os = "macos")]
        if let Story::Screen(screen) = self.story {
            if self
                .screen_view
                .as_ref()
                .is_none_or(|(current, _)| *current != screen)
            {
                self.screen_view = Some((
                    screen,
                    screens::build(screen, self.data_dir.clone(), window, cx),
                ));
            }
        }
        let overrides = mast_ui::style_overrides::current(cx);
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
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "l" => this.set_dark(false, cx),
                    "d" => this.set_dark(true, cx),
                    "t" => {
                        this.cycle_theme(cx);
                        cx.notify();
                    }
                    key => {
                        let key = match key {
                            "!" => "1",
                            "@" => "2",
                            "#" => "3",
                            other => other,
                        };
                        let Some(index) = key.parse::<usize>().ok().filter(|n| (1..=3).contains(n))
                        else {
                            return;
                        };
                        if event.keystroke.modifiers.shift {
                            let Some(&layout) = ui_style::UiLayout::ALL.get(index - 1) else {
                                return;
                            };
                            ui_style::set_layout_in_memory(layout, cx);
                            window.set_rem_size(px(typography::effective_rem_px(cx)));
                        } else {
                            let Some(&style) = gallery_styles().get(index - 1) else {
                                return;
                            };
                            switch_style(style, window, cx);
                        }
                    }
                }
                this.save_state(cx);
                cx.notify();
            }))
            .child(
                div()
                    .id("story-list")
                    .overflow_y_scroll()
                    .w(px(200.))
                    .h_full()
                    .p(px(20.))
                    .border_r_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child("Inspector: ⌘⌥⇧I")
                    .child("Screens")
                    .children({
                        #[cfg(target_os = "macos")]
                        {
                            screens::SCREENS
                                .iter()
                                .enumerate()
                                .map(|(index, &(screen, label))| {
                                    div()
                                        .id(("screen", index))
                                        .cursor_pointer()
                                        .px(px(8.))
                                        .py(px(6.))
                                        .when(self.story == Story::Screen(screen), |el| {
                                            el.rounded(px(5.)).bg(theme.surface_raised)
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.story = Story::Screen(screen);
                                            this.save_state(cx);
                                            cx.notify();
                                        }))
                                        .child(label)
                                })
                                .collect::<Vec<_>>()
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            Vec::<gpui::Div>::new()
                        }
                    })
                    .child("Stories")
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
                                        this.save_state(cx);
                                        cx.notify();
                                    }))
                                    .child(label)
                            }),
                    )
                    .children(stories::STORIES.iter().enumerate().map(
                        |(index, &(story, label))| {
                            div()
                                .id(("component-story", index))
                                .cursor_pointer()
                                .px(px(8.))
                                .py(px(6.))
                                .when(self.story == Story::Component(story), |el| {
                                    el.rounded(px(5.)).bg(theme.surface_raised)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.story = Story::Component(story);
                                    this.save_state(cx);
                                    cx.notify();
                                }))
                                .child(label)
                        },
                    )),
            )
            .child(
                div()
                    .id("story-content")
                    .overflow_y_scroll()
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
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            switch_style(id, window, cx);
                                            this.save_state(cx);
                                            cx.notify();
                                        }))
                                        .child(format!("{} {:?}", index + 1, id))
                                },
                            ))
                            .children(ui_style::UiLayout::ALL.iter().copied().enumerate().map(
                                |(index, layout)| {
                                    div()
                                        .id(("layout", index))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            ui_style::set_layout_in_memory(layout, cx);
                                            window
                                                .set_rem_size(px(typography::effective_rem_px(cx)));
                                            this.save_state(cx);
                                            cx.notify();
                                        }))
                                        .child(format!("⇧{} {}", index + 1, layout.label()))
                                },
                            ))
                            .child(
                                div()
                                    .id("light")
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.set_dark(false, cx)),
                                    )
                                    .child("Light (L)"),
                            )
                            .child(
                                div()
                                    .id("dark")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.set_dark(true, cx)))
                                    .child("Dark (D)"),
                            )
                            .child(
                                div()
                                    .id("theme-cycle")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cycle_theme(cx);
                                        cx.notify();
                                    }))
                                    .child(self.theme_label()),
                            ),
                    )
                    .when_some(
                        {
                            #[cfg(target_os = "macos")]
                            {
                                if let Story::Screen(screen) = self.story {
                                    self.screen_view
                                        .as_ref()
                                        .filter(|(current, _)| *current == screen)
                                        .map(|(_, view)| view.clone())
                                } else {
                                    None
                                }
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                None::<gpui::AnyView>
                            }
                        },
                        |el, view| el.child(div().w_full().h(px(900.)).child(view)),
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
                    .when(self.story == Story::Board, |el| {
                        el.child(stories::board(theme, &overrides))
                    })
                    .when_some(
                        match self.story {
                            Story::Component(story) => Some(story),
                            _ => None,
                        },
                        |el, story| el.child(stories::grid(story, theme, &overrides)),
                    )
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

fn gallery(
    focus: gpui::FocusHandle,
    data_dir: std::path::PathBuf,
    cx: &mut Context<Gallery>,
) -> Gallery {
    use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
    let saved = GalleryState::load();
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
        story: saved.story,
        #[cfg(target_os = "macos")]
        screen_view: None,
        data_dir,
        appearance_page: cx.new(settings::appearance::AppearancePage::new),
        toggled: false,
        menu_open: false,
        tab: 0,
        dark: saved.dark,
        themes: saved.themes,
    }
}

fn init(cx: &mut App, data_dir: &std::path::Path, override_dir: &std::path::Path) {
    gpui_base::init(cx);
    mast_ui::dev_inspector::init(cx);
    mast_ui::app_menus::bind_keys(cx);
    let settings = settings::UiSettings::default();
    let saved = GalleryState::load();
    settings::init(settings.clone(), data_dir, cx);
    let fonts = typography::register_fonts(cx);
    ui_style::init_with_layout(saved.style, saved.layout, cx);
    mast_ui::style_overrides::init_read_only(override_dir.to_path_buf(), cx);
    mast_ui::composer::init(cx, settings.composer_send_behavior);
    mast_ui::history::init(
        settings.git_history_columns,
        settings.git_history_column_widths,
        settings.git_history_column_order,
        settings.git_history_author_display,
        cx,
    );
    mast_ui::terminal::panel::init(cx);
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
        if saved.dark {
            appearance::AppearanceMode::Dark
        } else {
            appearance::AppearanceMode::Light
        },
        saved.themes,
        settings.accent,
        settings.surface,
        cx,
    );
}

/// Resolve the app's settings directory without creating or modifying it.
fn gallery_override_dir() -> std::path::PathBuf {
    if let Some(dir) =
        std::env::var_os("MAST_DATA_DIR").or_else(|| std::env::var_os("ZERON_DATA_DIR"))
    {
        return dir.into();
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|home| {
                    std::path::PathBuf::from(home)
                        .join("AppData")
                        .join("Local")
                        .into_os_string()
                })
            })
            .expect("LOCALAPPDATA or USERPROFILE required");
        std::path::PathBuf::from(local).join("Zeron")
    }
    #[cfg(not(windows))]
    {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME required"));
        mast_proto::env::home_storage_dir(&home, false).0
    }
}

fn main() {
    let data_dir = tempfile::tempdir().expect("gallery temporary settings directory");
    let override_dir = gallery_override_dir();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            init(cx, data_dir.path(), &override_dir);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    gallery(focus, data_dir.path().to_owned(), cx)
                })
            })
            .expect("gallery window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "macos", feature = "snapshot"))]
    #[test]
    #[ignore = "requires Metal and macOS services"]
    fn render_gallery_snapshots() {
        struct Snapshot(Story);
        impl Render for Snapshot {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let overrides = mast_ui::style_overrides::current(cx);
                let theme = Theme::of(cx);
                div()
                    .size_full()
                    .p(px(16.))
                    .bg(theme.surface)
                    .text_color(theme.text)
                    .child(match self.0 {
                        Story::Component(story) => {
                            stories::grid(story, theme, &overrides).into_any_element()
                        }
                        Story::Board => stories::board(theme, &overrides).into_any_element(),
                        _ => div().into_any_element(),
                    })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        for (name, story) in [
            ("gallery-buttons", Story::Component(stories::Story::Button)),
            (
                "gallery-outline",
                Story::Component(stories::Story::OutlineRow),
            ),
            ("gallery-board", Story::Board),
        ] {
            mast_ui::snapshot::render_to_png(
                name,
                gpui::size(px(2200.), px(800.)),
                ui_style::UiStyleId::Default,
                mast_ui::theme::Appearance::Light,
                |_, cx| {
                    init(cx, dir.path(), dir.path());
                    cx.new(|_| Snapshot(story))
                },
            )
            .unwrap();
        }
        for (name, screen) in [
            ("gallery-screen-chat", screens::Screen::Chat),
            ("gallery-screen-overview", screens::Screen::Overview),
            ("gallery-screen-walkthrough", screens::Screen::Walkthrough),
        ] {
            mast_ui::snapshot::render_to_png(
                name,
                gpui::size(px(1600.), px(1000.)),
                ui_style::UiStyleId::Default,
                mast_ui::theme::Appearance::Light,
                |window, cx| {
                    init(cx, dir.path(), dir.path());
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        let mut gallery = gallery(focus, dir.path().to_owned(), cx);
                        gallery.story = Story::Screen(screen);
                        gallery
                    })
                },
            )
            .unwrap();
        }
    }

    #[test]
    fn gallery_state_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut state = GalleryState::load_from(None);
        state.story = Story::Component(stories::Story::Code);
        state.style = ui_style::UiStyleId::Area;
        state.layout = ui_style::UiLayout::Guided;
        state.dark = true;
        state.themes.light = "github-light".into();
        state.themes.dark = "github-dark".into();
        state.save_to(&path).unwrap();
        let restored = GalleryState::load_from(Some(&path));
        assert!(restored.story == state.story);
        assert_eq!(restored.style, state.style);
        assert_eq!(restored.layout, state.layout);
        assert_eq!(restored.dark, state.dark);
        assert_eq!(restored.themes.light, state.themes.light);
        assert_eq!(restored.themes.dark, state.themes.dark);
        #[cfg(target_os = "macos")]
        for (screen, _) in screens::SCREENS {
            state.story = Story::Screen(screen);
            state.save_to(&path).unwrap();
            assert!(GalleryState::load_from(Some(&path)).story == state.story);
        }
    }

    #[gpui::test]
    fn views_render_independent_styles(cx: &mut gpui::TestAppContext) {
        struct Comparison;
        impl Render for Comparison {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                use mast_ui::components::{ButtonSize, ButtonVariant, button};
                let base = Theme::of(cx);
                let default = ui_style::theme_for_view(
                    base,
                    ui_style::UiStyleId::Default,
                    ui_style::UiLayout::Minimal,
                    &mast_ui::style_overrides::current(cx),
                );
                let area = ui_style::theme_for_view(
                    base,
                    ui_style::UiStyleId::Area,
                    ui_style::UiLayout::Minimal,
                    &mast_ui::style_overrides::current(cx),
                );
                assert_ne!(default.ui_style.conversation, area.ui_style.conversation);
                assert_eq!(default.ui_style.tokens.control_radius, None);
                assert_eq!(area.ui_style.tokens.control_radius, Some(23.0));
                assert_eq!(ui_style::current(cx).id, ui_style::UiStyleId::Default);
                div()
                    .flex()
                    .child(button(
                        "default-button",
                        "Default",
                        ButtonVariant::Primary,
                        ButtonSize::Md,
                        &default,
                    ))
                    .child(button(
                        "area-button",
                        "Area",
                        ButtonVariant::Primary,
                        ButtonSize::Md,
                        &area,
                    ))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            mast_ui::style_overrides::Overrides::path(dir.path()),
            r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":23}}}"#,
        )
        .unwrap();
        cx.update(|cx| {
            init(cx, dir.path(), dir.path());
            assert_eq!(
                mast_ui::style_overrides::save(&mast_ui::style_overrides::current(cx), cx)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
        });
        let window = cx.add_window(|_, _| Comparison);
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
    }

    #[gpui::test]
    fn saved_overrides_refresh_each_view_without_changing_gallery_settings(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| init(cx, dir.path(), dir.path()));
        cx.run_until_parked();
        std::fs::write(mast_ui::style_overrides::Overrides::path(dir.path()), r#"{"version":"mast.style-overrides/v1","styles":{"area":{"controlRadius":25,"surfaces":{"composer":{"border":true}},"layouts":{"compact":{"controlRadius":19}}},"industrial":{"controlRadius":3}}}"#).unwrap();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        cx.run_until_parked();
        cx.update(|cx| {
            let base = Theme::of(cx);
            let overrides = mast_ui::style_overrides::current(cx);
            let area = ui_style::theme_for_view(
                base,
                ui_style::UiStyleId::Area,
                ui_style::UiLayout::Compact,
                &overrides,
            );
            let industrial = ui_style::theme_for_view(
                base,
                ui_style::UiStyleId::Industrial,
                ui_style::UiLayout::Compact,
                &overrides,
            );
            let default = ui_style::theme_for_view(
                base,
                ui_style::UiStyleId::Default,
                ui_style::UiLayout::Compact,
                &overrides,
            );
            assert_eq!(area.ui_style.tokens.control_radius, Some(19.0));
            let surface = mast_ui::style_overrides::DesignSurface::Composer;
            assert_eq!(
                area.ui_style.design_roles.surface(surface).border,
                Some(true)
            );
            assert_eq!(
                industrial.ui_style.design_roles.surface(surface).border,
                None
            );
            assert_eq!(industrial.ui_style.tokens.control_radius, Some(3.0));
            assert_eq!(default.ui_style.tokens.control_radius, None);
            assert_eq!(ui_style::current(cx).id, ui_style::UiStyleId::Default);
        });
    }

    #[gpui::test]
    fn stories_render_in_each_style_and_appearance(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| init(cx, dir.path(), dir.path()));
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            gallery(focus, dir.path().to_owned(), cx)
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
                for &(story, _) in stories::STORIES {
                    window
                        .update(cx, |gallery, _, cx| {
                            gallery.story = Story::Component(story);
                            cx.notify();
                        })
                        .unwrap();
                    cx.update_window(window.into(), |_, window, cx| {
                        window.refresh();
                        window.draw(cx).clear();
                    })
                    .unwrap();
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn screens_render_with_preview_fixtures(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| init(cx, dir.path(), dir.path()));
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            gallery(focus, dir.path().to_owned(), cx)
        });
        for (screen, _) in screens::SCREENS {
            window
                .update(cx, |gallery, _, cx| {
                    gallery.story = Story::Screen(screen);
                    cx.notify();
                })
                .unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
        }
    }
}
