//! Production views with isolated preview data.
use gpui::{AnyView, App, AppContext, Window};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Chat,
    Details,
    Overview,
    Walkthrough,
    Appearance,
    Browser,
}

pub const SCREENS: [(Screen, &str); 6] = [
    (Screen::Chat, "Chat and sidebar"),
    (Screen::Details, "Item details"),
    (Screen::Overview, "Overview and outline"),
    (Screen::Walkthrough, "Changes and walkthrough"),
    (Screen::Appearance, "Appearance"),
    (Screen::Browser, "Browser"),
];

pub fn build(screen: Screen, data_dir: PathBuf, window: &mut Window, cx: &mut App) -> AnyView {
    match screen {
        Screen::Chat => mast_ui::snapshot::chat_fixture(data_dir, cx).into(),
        Screen::Details => mast_ui::snapshot::outline_fixture(data_dir, cx).into(),
        Screen::Overview => mast_ui::snapshot::overview_fixture(cx).into(),
        Screen::Walkthrough => mast_ui::snapshot::populated_walkthrough_fixture(cx).into(),
        Screen::Appearance => cx
            .new(mast_ui::settings::appearance::AppearancePage::new)
            .into(),
        Screen::Browser => cx
            .new(|cx| {
                mast_ui::browser::BrowserSurface::new(
                    mast_ui::browser::BrowserContext::default(),
                    false,
                    window,
                    cx,
                )
            })
            .into(),
    }
}
