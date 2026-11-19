//! The main window's side of pop-out windows (`crate::popout`): tear a surface
//! off the right pane into its own window, and take the navigation requests a
//! pop-out forwards back. See docs/internal/plans/2026-11-19-popout-windows-spike.md.
use super::*;

use crate::popout::{PopoutSurface, SubagentPopout};

impl Shell {
    /// Whether `surface` can leave the main window today: File, Diff and
    /// Subagent tabs. Browser and artifact web views are positioned by this
    /// window's native presentation pass, the terminal is one panel per Shell,
    /// and Review/Item/Tracker render from Shell state.
    pub(crate) fn can_pop_out(&self, surface: RightSurface) -> bool {
        match surface {
            RightSurface::File(id) => self.file_surfaces.contains_key(&id),
            RightSurface::Diff(id) => self.diffs.contains_key(&id),
            RightSurface::Subagent(id) => self.subagent_tabs.contains_key(&id),
            _ => false,
        }
    }

    /// Move a right-pane tab into its own window over the same `AppState`.
    /// The entity moves (it is not copied), so edits, the diff watch and the
    /// subagent feed stay single. Returns whether a window opened.
    pub(crate) fn pop_out_surface(
        &mut self,
        surface: RightSurface,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_pop_out(surface) {
            return false;
        }
        let key = self.panel_key(cx);
        let owner = self.active_chat.clone();
        let (popout, title) = match surface {
            RightSurface::File(id) => {
                let Some(file) = self.file_surfaces.remove(&id) else { return false };
                let path = self.file_surface_paths.remove(&id).unwrap_or_default();
                self.file_surface_subs.remove(&id);
                self.file_surface_keys.retain(|_, value| *value != id);
                self.pending_file_closes.remove(&surface);
                let title = workspace_file_title(&path);
                (PopoutSurface::File { surface: file, path }, title)
            }
            RightSurface::Diff(id) => {
                let Some(changes) = self.diffs.remove(&id) else { return false };
                self.diff_subs.remove(&id);
                let title = changes.read(cx).tab_title();
                (PopoutSurface::Diff(changes), title)
            }
            RightSurface::Subagent(id) => {
                let Some(tab) = self.subagent_tabs.remove(&id) else { return false };
                let SubagentTab { doc_id, title, transcript, _fetch, _events } = tab;
                // The pop-out re-subscribes the spawn chips; the main strip's
                // hookup goes with the tab.
                drop(_events);
                (
                    PopoutSurface::Subagent(SubagentPopout { doc_id, transcript, fetch: _fetch }),
                    title,
                )
            }
            _ => return false,
        };
        if let Some(tabs) = self.right_tabs.get_mut(&key) {
            tabs.retain(|candidate| *candidate != surface);
        }
        self.panels.update(&key, |panel| {
            if panel.right_active == surface {
                panel.right_active = RightSurface::Picker;
            }
        });
        self.collapse_surfaces_if_empty(&key, cx);
        crate::popout::open(self.state.clone(), popout, owner, title, cx);
        cx.notify();
        true
    }

    /// ⌥⌘N: pop out the active tab, when it can leave.
    pub(crate) fn pop_out_active_surface(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.route, Route::Chat) || !self.right_pane_open(cx) {
            return false;
        }
        let surface = self.resolved_right_active(cx);
        self.pop_out_surface(surface, cx)
    }

    fn popout_focus_chat(&mut self, owner_chat: &str, cx: &mut Context<Self>) {
        if self.active_chat != owner_chat {
            self.open_chat(owner_chat.to_string(), cx);
        }
    }

    pub(crate) fn popout_open_file(
        &mut self,
        owner_chat: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.popout_focus_chat(&owner_chat, cx);
        self.add_file_surface(path, window, cx);
    }

    pub(crate) fn popout_open_commit(
        &mut self,
        owner_chat: String,
        commit: mast_proto::GitHistoryCommit,
        cx: &mut Context<Self>,
    ) {
        self.popout_focus_chat(&owner_chat, cx);
        self.set_surfaces_open(true, cx);
        self.add_commit_diff_surface(commit, Some(owner_chat), cx);
    }

    pub(crate) fn popout_activate_link(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::markdown::render::LinkOutcome {
        self.activate_session_link(activation, window, cx)
    }

    pub(crate) fn popout_transcript_event(
        &mut self,
        transcript: Entity<Transcript>,
        event: &TranscriptEvent,
        cx: &mut Context<Self>,
    ) {
        self.on_transcript_event(transcript, event, cx)
    }

    pub(crate) fn popout_set_word_wrap(&mut self, word_wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_files_word_wrap(word_wrap, window, cx)
    }

    pub(crate) fn popout_set_show_all(&mut self, show_all: bool, cx: &mut Context<Self>) {
        self.set_files_show_all(show_all, cx)
    }
}
