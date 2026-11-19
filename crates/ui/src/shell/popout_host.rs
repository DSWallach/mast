//! The main window's side of pop-out windows (`crate::popout`): tear a surface
//! off the right pane into its own window, and take the navigation requests a
//! pop-out forwards back. See docs/internal/plans/2026-11-19-popout-windows-spike.md.
use super::*;

use crate::popout::{PopoutSurface, SubagentPopout};

/// Navigation a pop-out asked for that waits for its owner chat to show.
pub(super) enum PopoutNav {
    File(String),
    Definitions(Vec<(String, mast_syntax::symbols::Symbol)>),
    Commit(mast_proto::GitHistoryCommit),
    Link(crate::markdown::render::LinkActivation),
    Subagent { chat_id: String, doc_id: String, title: String, frozen: bool },
}

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
                let SubagentTab { doc_id, title, transcript, _events } = tab;
                // The pop-out re-subscribes the spawn chips; the main strip's
                // hookup goes with the tab.
                drop(_events);
                (
                    PopoutSurface::Subagent(SubagentPopout { doc_id, transcript }),
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

    /// Switch to the owner chat and run `nav` once `active_chat` has caught up
    /// (the selection lands through the state observer, and a rebuilt main
    /// window starts with no chat at all). The queue drains at render.
    fn queue_popout_nav(&mut self, owner_chat: String, nav: PopoutNav, cx: &mut Context<Self>) {
        self.popout_focus_chat(&owner_chat, cx);
        // Sync `active_chat` now; the observer only runs after this update.
        let state = self.state.clone();
        self.on_state_changed(&state, cx);
        self.pending_popout_nav.push((owner_chat, nav));
        cx.notify();
    }

    /// Run queued pop-out navigation whose chat is now showing. Entries the
    /// user has since navigated away from are dropped.
    pub(super) fn drain_popout_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_popout_nav.is_empty() {
            return;
        }
        let selected = self.state.read(cx).selected_chat.clone();
        for (owner, nav) in std::mem::take(&mut self.pending_popout_nav) {
            if self.active_chat == owner {
                self.run_popout_nav(owner, nav, window, cx);
            } else if selected.as_deref() == Some(owner.as_str()) {
                self.pending_popout_nav.push((owner, nav));
            }
        }
    }

    fn run_popout_nav(&mut self, owner_chat: String, nav: PopoutNav, window: &mut Window, cx: &mut Context<Self>) {
        match nav {
            PopoutNav::File(path) => self.add_file_surface(path, window, cx),
            PopoutNav::Definitions(definitions) => self.open_definition_candidates(definitions, window, cx),
            PopoutNav::Commit(commit) => {
                self.set_surfaces_open(true, cx);
                self.add_commit_diff_surface(commit, Some(owner_chat), cx);
            }
            PopoutNav::Link(activation) => {
                if let crate::markdown::render::LinkOutcome::External(url) =
                    self.activate_session_link(&activation, window, cx)
                {
                    cx.open_url(&url);
                }
            }
            PopoutNav::Subagent { chat_id, doc_id, title, frozen } => {
                self.leave_overview_for_chat(cx);
                self.add_subagent_surface(chat_id, doc_id, title, frozen, cx);
            }
        }
    }

    fn popout_chat_is_showing(&self, owner_chat: &str, cx: &App) -> bool {
        self.active_chat == owner_chat
            && self.state.read(cx).selected_chat.as_deref() == Some(owner_chat)
    }

    pub(crate) fn popout_open_file(
        &mut self,
        owner_chat: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popout_chat_is_showing(&owner_chat, cx) {
            self.add_file_surface(path, window, cx);
        } else {
            self.queue_popout_nav(owner_chat, PopoutNav::File(path), cx);
        }
    }

    pub(crate) fn popout_open_definitions(&mut self, owner_chat: String, definitions: Vec<(String, mast_syntax::symbols::Symbol)>, window: &mut Window, cx: &mut Context<Self>) {
        if self.popout_chat_is_showing(&owner_chat, cx) {
            self.open_definition_candidates(definitions, window, cx);
        } else {
            self.queue_popout_nav(owner_chat, PopoutNav::Definitions(definitions), cx);
        }
    }

    pub(crate) fn popout_open_commit(
        &mut self,
        owner_chat: String,
        commit: mast_proto::GitHistoryCommit,
        cx: &mut Context<Self>,
    ) {
        if self.popout_chat_is_showing(&owner_chat, cx) {
            self.set_surfaces_open(true, cx);
            self.add_commit_diff_surface(commit, Some(owner_chat), cx);
        } else {
            self.queue_popout_nav(owner_chat, PopoutNav::Commit(commit), cx);
        }
    }

    /// A pop-out's save / discard prompt resolved: re-check a pending exit.
    pub(crate) fn popout_exit_progress(&mut self, cancelled: bool, cx: &mut Context<Self>) {
        if cancelled {
            self.pending_exit = None;
        }
        cx.notify();
    }

    pub(crate) fn popout_activate_link(
        &mut self,
        owner_chat: String,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::markdown::render::LinkOutcome {
        if self.popout_chat_is_showing(&owner_chat, cx) {
            return self.activate_session_link(activation, window, cx);
        }
        // Handled once the owner chat is showing; an external URL then opens.
        self.queue_popout_nav(owner_chat, PopoutNav::Link(activation.clone()), cx);
        crate::markdown::render::LinkOutcome::Internal
    }

    pub(crate) fn popout_transcript_event(
        &mut self,
        owner_chat: String,
        transcript: Entity<Transcript>,
        event: &TranscriptEvent,
        cx: &mut Context<Self>,
    ) {
        if let TranscriptEvent::OpenSubagent { chat_id, doc_id, title, frozen } = event
            && !self.popout_chat_is_showing(&owner_chat, cx)
        {
            let nav = PopoutNav::Subagent {
                chat_id: chat_id.clone(),
                doc_id: doc_id.clone(),
                title: title.clone(),
                frozen: *frozen,
            };
            self.queue_popout_nav(owner_chat, nav, cx);
            return;
        }
        self.on_transcript_event(transcript, event, cx)
    }

    pub(crate) fn popout_set_word_wrap(&mut self, word_wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_files_word_wrap(word_wrap, window, cx)
    }

    pub(crate) fn popout_set_show_all(&mut self, show_all: bool, cx: &mut Context<Self>) {
        self.set_files_show_all(show_all, cx)
    }
}
