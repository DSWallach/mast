//! Pop-out windows: one right-pane surface in its own window.
//!
//! A `PopoutWindow` is a light root view, not a second [`Shell`]. It owns the
//! surface entity the main window handed over (a tear-off: the tab leaves the
//! main strip, so there is still exactly one editor buffer, one diff watch, one
//! subagent feed) and renders it under a small drag strip. Everything global —
//! menu bar, dock, status item, sounds, banners, deep links, update checks, the
//! 1 s ticker — stays with the main `Shell`, which is the only window that
//! follows `AppState::selected_chat`. Navigation the surface asks for (open a
//! file, open a commit, follow a link, a spawn chip) is forwarded to the main
//! window. See docs/internal/plans/2026-11-19-popout-windows-spike.md.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, Global, MouseButton,
    Pixels, SharedString, Subscription, Task, TitlebarOptions, Window, WindowBounds,
    WindowControlArea, WindowHandle, WindowOptions, div, prelude::*, px, size,
};

use crate::changes::{Changes, ChangesEvent};
use crate::files::{FilesCloseDisposition, FilesEvent, FilesSurface};
use crate::shell::Shell;
use crate::state::AppState;
use crate::theme::Theme;
use crate::transcript::{Transcript, TranscriptEvent};

/// Frames of popped-out windows, remembered for the session only (no
/// persistence across restarts yet), keyed by [`PopoutSurface::frame_key`].
#[derive(Default)]
struct PopoutFrames(HashMap<String, Bounds<Pixels>>);

impl Global for PopoutFrames {}

/// A subagent transcript handed over from a right-pane tab.
pub(crate) struct SubagentPopout {
    pub(crate) doc_id: String,
    pub(crate) transcript: Entity<Transcript>,
    /// Keeps a frozen-blob fetch alive (it falls back to a live doc watch).
    pub(crate) _fetch: Option<Task<()>>,
}

/// What a pop-out shows. Each variant is the same entity the right pane held.
pub(crate) enum PopoutSurface {
    File {
        surface: Entity<FilesSurface>,
        path: String,
    },
    Diff(Entity<Changes>),
    Subagent(SubagentPopout),
}

impl PopoutSurface {
    fn frame_key(&self) -> String {
        match self {
            Self::File { path, .. } => format!("file:{path}"),
            Self::Diff(changes) => format!("diff:{}", changes.entity_id().as_u64()),
            Self::Subagent(sub) => format!("subagent:{}", sub.doc_id),
        }
    }
}

pub struct PopoutWindow {
    state: Entity<AppState>,
    surface: PopoutSurface,
    /// The chat whose workspace the surface belongs to; navigation forwarded
    /// to the main window opens that chat first.
    owner_chat: String,
    fallback_title: SharedString,
    shown_title: Option<SharedString>,
    focus: FocusHandle,
    /// A dirty file's close is waiting on the save / discard prompt.
    closing: bool,
    titlebar_should_move: bool,
    _subscriptions: Vec<Subscription>,
}

const DEFAULT_SIZE: (f32, f32) = (900.0, 720.0);

/// Open a window over `state` showing `surface`. Cheap: no engine call.
pub(crate) fn open(
    state: Entity<AppState>,
    surface: PopoutSurface,
    owner_chat: String,
    title: SharedString,
    cx: &mut App,
) -> Option<WindowHandle<PopoutWindow>> {
    let key = surface.frame_key();
    let bounds = cx
        .try_global::<PopoutFrames>()
        .and_then(|frames| frames.0.get(&key).copied())
        .unwrap_or_else(|| Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx));
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(320.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.clone()),
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::point(px(14.), px(14.))),
                }),
                app_owns_titlebar_drag: true,
                window_decorations: cfg!(target_os = "linux").then_some(gpui::WindowDecorations::Client),
                window_background: Theme::of(cx).window_background_appearance(),
                app_id: Some("zeron".into()),
                ..Default::default()
            },
            move |window, cx| {
                window.set_rem_size(px(crate::typography::effective_rem_px(cx)));
                crate::appearance::observe_window(window, cx).detach();
                let view = cx.new(|cx| PopoutWindow::new(state, surface, owner_chat, title, window, cx));
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |popout, cx| popout.prepare_close(cx)).unwrap_or(true)
                });
                view
            },
        )
        .ok()?;
    crate::appearance::reapply_window_background(cx);
    Some(handle)
}

/// The window showing the main `Shell`, if ⌘W has not closed it.
fn main_shell(cx: &App) -> Option<WindowHandle<Shell>> {
    cx.windows().into_iter().find_map(|window| window.downcast::<Shell>())
}

impl PopoutWindow {
    fn new(
        state: Entity<AppState>,
        surface: PopoutSurface,
        owner_chat: String,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = surface.frame_key();
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe_window_bounds(window, move |_, window, cx| {
            if window.is_fullscreen() {
                return;
            }
            if let WindowBounds::Windowed(bounds) = window.window_bounds() {
                cx.default_global::<PopoutFrames>().0.insert(key.clone(), bounds);
            }
        }));
        match &surface {
            PopoutSurface::File { surface, .. } => {
                subscriptions.push(cx.subscribe_in(surface, window, Self::on_files_event));
                surface.update(cx, |file, cx| file.ensure_loaded(cx));
            }
            PopoutSurface::Diff(changes) => {
                subscriptions.push(cx.subscribe(changes, Self::on_changes_event));
                changes.update(cx, |changes, cx| changes.ensure_content(cx));
            }
            PopoutSurface::Subagent(sub) => {
                subscriptions.push(cx.subscribe(&sub.transcript, Self::on_transcript_event));
            }
        }
        // Closing the window drops the surface. A subagent feed is pinned in
        // `AppState`, so release it explicitly.
        cx.on_release(|this, cx| {
            if let PopoutSurface::Subagent(sub) = &this.surface {
                let doc_id = sub.doc_id.clone();
                this.state.update(cx, |state, _| state.unwatch_subagent_doc(&doc_id));
            }
        })
        .detach();
        Self {
            state,
            surface,
            owner_chat,
            fallback_title: title,
            shown_title: None,
            focus: cx.focus_handle(),
            closing: false,
            titlebar_should_move: false,
            _subscriptions: subscriptions,
        }
    }

    /// The surface title: the tab title the surface shows in the main strip.
    pub fn title(&self, cx: &App) -> SharedString {
        match &self.surface {
            PopoutSurface::File { surface, .. } => surface.read(cx).tab_title(),
            PopoutSurface::Diff(changes) => changes.read(cx).tab_title(),
            PopoutSurface::Subagent(_) => self.fallback_title.clone(),
        }
    }

    fn with_main(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>),
    ) {
        if let Some(main) = main_shell(cx) {
            main.update(cx, |shell, window, cx| {
                window.activate_window();
                f(shell, window, cx)
            })
            .ok();
        }
    }

    fn on_files_event(
        &mut self,
        _: &Entity<FilesSurface>,
        event: &FilesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = self.owner_chat.clone();
        match event {
            FilesEvent::OpenFile(path) => {
                let path = path.clone();
                self.with_main(cx, move |shell, window, cx| {
                    shell.popout_open_file(owner, path, window, cx)
                });
            }
            FilesEvent::OpenCommit(commit) => {
                let commit = commit.clone();
                self.with_main(cx, move |shell, _, cx| shell.popout_open_commit(owner, commit, cx));
            }
            FilesEvent::OpenWebLink(activation) => {
                let activation = activation.clone();
                if let Some(main) = main_shell(cx) {
                    let outcome = main
                        .update(cx, |shell, window, cx| shell.popout_activate_link(&activation, window, cx))
                        .ok();
                    if let Some(crate::markdown::render::LinkOutcome::External(url)) = outcome {
                        cx.open_url(&url);
                    }
                }
            }
            FilesEvent::FileRenamed { new_path, .. } => {
                if let PopoutSurface::File { path, .. } = &mut self.surface {
                    *path = new_path.clone();
                }
                cx.notify();
            }
            FilesEvent::WordWrapChanged(word_wrap) => {
                let word_wrap = *word_wrap;
                self.with_main(cx, move |shell, window, cx| shell.popout_set_word_wrap(word_wrap, window, cx));
            }
            FilesEvent::ShowAllFilesChanged(show_all) => {
                let show_all = *show_all;
                self.with_main(cx, move |shell, _, cx| shell.popout_set_show_all(show_all, cx));
            }
            FilesEvent::TitleChanged => cx.notify(),
            FilesEvent::CloseReady => {
                if self.closing {
                    window.remove_window();
                }
            }
            FilesEvent::CloseCancelled => self.closing = false,
            // Navigation history and the explorer live in the main window.
            FilesEvent::Jumped { .. } | FilesEvent::RevealFile(_) => {}
        }
    }

    fn on_changes_event(&mut self, _: Entity<Changes>, event: &ChangesEvent, cx: &mut Context<Self>) {
        let ChangesEvent::OpenCommit(commit) = event;
        let (owner, commit) = (self.owner_chat.clone(), commit.clone());
        self.with_main(cx, move |shell, _, cx| shell.popout_open_commit(owner, commit, cx));
    }

    fn on_transcript_event(&mut self, transcript: Entity<Transcript>, event: &TranscriptEvent, cx: &mut Context<Self>) {
        let event = event.clone();
        self.with_main(cx, move |shell, _, cx| shell.popout_transcript_event(transcript, &event, cx));
    }

    /// Whether the window may close now. A file with unsaved edits raises its
    /// own save / discard prompt and closes on `CloseReady`.
    pub fn prepare_close(&mut self, cx: &mut Context<Self>) -> bool {
        let PopoutSurface::File { surface, .. } = &self.surface else {
            return true;
        };
        match surface.update(cx, |file, cx| file.prepare_close(cx)) {
            FilesCloseDisposition::Allow => true,
            FilesCloseDisposition::Pending | FilesCloseDisposition::Blocked => {
                self.closing = true;
                cx.notify();
                false
            }
        }
    }

    /// App quit: a dirty file blocks it and brings its window forward.
    pub fn prepare_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let PopoutSurface::File { surface, .. } = &self.surface else {
            return true;
        };
        if !surface.read(cx).has_unsaved_changes() {
            return true;
        }
        let allowed = surface.update(cx, |file, cx| file.prepare_close(cx)) == FilesCloseDisposition::Allow;
        if !allowed {
            window.activate_window();
        }
        allowed
    }

    /// ⌘W on a pop-out: close just this window (after the unsaved-file gate).
    pub fn close_from_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prepare_close(cx) {
            window.remove_window();
        }
    }

    pub fn has_unsaved_changes(&self, cx: &App) -> bool {
        matches!(&self.surface, PopoutSurface::File { surface, .. } if surface.read(cx).has_unsaved_changes())
    }

    fn render_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.surface {
            PopoutSurface::File { surface, .. } => {
                surface.update(cx, |file, cx| file.ensure_loaded(cx));
                surface.clone().into_any_element()
            }
            PopoutSurface::Diff(changes) => {
                let changes = changes.clone();
                changes.update(cx, |changes, cx| changes.ensure_content(cx));
                let controls = changes.update(cx, |changes, cx| changes.render_header_controls(cx));
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(crate::surface_chrome::toolbar(theme).child(controls))
                    .child(div().flex_1().min_h_0().child(changes))
                    .into_any_element()
            }
            PopoutSurface::Subagent(sub) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().child(sub.transcript.clone()))
                .into_any_element(),
        }
    }

    fn render_strip(&self, title: SharedString, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inset = if cfg!(target_os = "macos") {
            crate::shell::titlebar_cluster_start(window.is_fullscreen())
        } else {
            12.0
        };
        div()
            .id("popout-titlebar")
            .window_control_area(WindowControlArea::Drag)
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(inset))
            .pr(px(inset))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.titlebar_should_move = true))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                if this.titlebar_should_move && event.pressed_button == Some(MouseButton::Left) {
                    this.titlebar_should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
            .child(div().min_w_0().truncate().child(title))
    }
}

impl Focusable for PopoutWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PopoutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let title = self.title(cx);
        if self.shown_title.as_ref() != Some(&title) {
            window.set_window_title(&title);
            self.shown_title = Some(title.clone());
        }
        let body = self.render_body(&theme, cx);
        div()
            .id("popout-root")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.glass())
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .text_size(crate::typography::ui_rems(14.0))
            .on_action(cx.listener(|this, _: &crate::shell::SaveFile, _, cx| {
                if let PopoutSurface::File { surface, .. } = &this.surface {
                    surface.update(cx, |file, cx| file.save_active_document(cx));
                }
            }))
            .child(self.render_strip(title, &theme, window, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// ⌘W / Window > Close Window with a pop-out focused. Returns whether the
/// window was a pop-out.
pub(crate) fn close_active(window: gpui::AnyWindowHandle, cx: &mut App) -> bool {
    let Some(handle) = window.downcast::<PopoutWindow>() else {
        return false;
    };
    handle.update(cx, |popout, window, cx| popout.close_from_shortcut(window, cx)).ok();
    true
}

/// App quit: every pop-out must agree. Returns `false` when one holds
/// unsaved edits (that window is brought forward).
pub(crate) fn prepare_quit(cx: &mut App) -> bool {
    let mut ready = true;
    for window in cx.windows() {
        if let Some(window) = window.downcast::<PopoutWindow>() {
            ready &= window
                .update(cx, |popout, window, cx| popout.prepare_quit(window, cx))
                .unwrap_or(true);
        }
    }
    ready
}

/// Pop-outs currently open.
#[cfg(test)]
pub(crate) fn open_count(cx: &App) -> usize {
    cx.windows().into_iter().filter(|window| window.downcast::<PopoutWindow>().is_some()).count()
}
