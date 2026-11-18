use super::*;

impl Changes {
    pub(super) fn close_scope_menu(&mut self, cx: &mut Context<Self>) {
        if self.scope_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.scope_menu);
        }
    }

    pub(super) fn close_ref_menu(&mut self, cx: &mut Context<Self>) {
        if self.ref_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.ref_menu);
        }
    }

    pub(super) fn close_checkout_menu(&mut self, cx: &mut Context<Self>) {
        if self.checkout_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.checkout_menu);
        }
    }

    /// Handle Escape before focused descendants such as a terminal receive it.
    /// A popup in its exit animation remains a blocker until it unmounts.
    pub(crate) fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.ref_target_menu.is_open() {
            self.close_ref_target_menu(cx);
            return true;
        }
        if self.checkout_menu.is_open() {
            self.close_checkout_menu(cx);
            return true;
        }
        if self.ref_menu.is_open() {
            self.close_ref_menu(cx);
            return true;
        }
        if self.scope_menu.is_open() {
            self.close_scope_menu(cx);
            return true;
        }
        self.scope_menu.get().is_some()
            || self.ref_menu.get().is_some()
            || self.checkout_menu.get().is_some()
            || self.ref_target_menu.get().is_some()
    }

    pub(super) fn open_ref_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search branches…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.ref_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        // The highlight starts ON the current base (query is empty, so the
        // filtered rows are just the branch list).
        let active = self
            .base_ref
            .as_ref()
            .and_then(|base| self.branches.iter().position(|b| b == base))
            .unwrap_or(0);
        self.ref_menu.open(RefMenu {
            search,
            active,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            _search_events: search_events,
        });
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered branch indices for the open ref menu (ranked substring match).
    pub(super) fn ref_menu_rows(&self, cx: &App) -> Vec<usize> {
        let query = self
            .ref_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        popover::filter_indices(&query, &self.branches)
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ picks the highlighted branch, Esc closes.
    pub(super) fn ref_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.ref_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_ref_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.ref_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.ref_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.ref_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self
                    .ref_menu_rows(cx)
                    .get(active)
                    .and_then(|ix| self.branches.get(*ix).cloned());
                if let Some(branch) = pick {
                    self.set_base_ref(branch, cx);
                    self.close_ref_menu(cx);
                }
            }
            _ => {}
        }
    }

    /// Whether the "Diff checkout..." button shows: a chat hosted on this
    /// device with a working directory. Never for a commit view or a pinned
    /// checkout (no chat target to change).
    pub(super) fn checkout_picker_available(&self, cx: &App) -> bool {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return false;
        }
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        let state = self.state.read(cx);
        state.local_device_id.as_deref() == Some(owner.device_id.as_str())
            && state
                .chats
                .iter()
                .find(|chat| chat.id == owner.chat_id)
                .is_some_and(|chat| chat.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()))
    }

    pub(super) fn open_checkout_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else {
            return;
        };
        let ticket = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(crate::diff_targets::chat_ticket_id);
        let search = cx
            .new(|cx| ComposerInput::with_context("Search checkouts\u{2026}", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.checkout_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        let list = self
            .state
            .update(cx, |state, cx| state.list_chat_diff_checkouts(&chat_id, cx));
        let load = cx.spawn(async move |this, cx| {
            let result = list.await;
            this.update(cx, |this, cx| {
                if let Some(menu) = this.checkout_menu.open_mut() {
                    menu.candidates = match result {
                        Ok(candidates) => popover::Loadable::Ready(
                            crate::diff_targets::rank_candidates(candidates, ticket.as_deref()),
                        ),
                        Err(message) => popover::Loadable::Error(message),
                    };
                    menu.active = 0;
                }
                cx.notify();
            })
            .ok();
        });
        self.checkout_menu.open(CheckoutMenu {
            search,
            active: 0,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            candidates: popover::Loadable::Loading,
            _search_events: search_events,
            _load: load,
        });
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered candidate indices for the open picker (ranked substring match
    /// over the label, kind, and path; ticket matches stay ahead when the
    /// query is empty).
    pub(super) fn checkout_menu_rows(&self, cx: &App) -> Vec<usize> {
        let Some(menu) = self.checkout_menu.get() else {
            return Vec::new();
        };
        let Some(rows) = menu.candidates.ready() else {
            return Vec::new();
        };
        let query = menu.search.read(cx).text().to_string();
        let haystacks: Vec<String> = rows
            .iter()
            .map(|row| format!("{} {} {}", row.label(), row.kind_tag(), row.candidate.path))
            .collect();
        popover::filter_indices(&query, &haystacks)
    }

    pub(super) fn checkout_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if !self.checkout_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_checkout_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.checkout_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.checkout_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.checkout_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self.checkout_menu_rows(cx).get(active).and_then(|ix| {
                    self.checkout_menu
                        .get()?
                        .candidates
                        .ready()?
                        .get(*ix)
                        .map(|row| row.candidate.clone())
                });
                if let Some(candidate) = pick {
                    self.choose_checkout(candidate, cx);
                }
            }
            _ => {}
        }
    }

    /// Persist the picked candidate as the chat's diff target: the own
    /// checkout clears the selection, anything else selects its path. The
    /// session's pill pick is dropped first so the persisted selection (the
    /// next `WatchChatDiffTargets` frame) is what shows; a failed write
    /// surfaces as the pane's error notice (`AppState::chat_diff_target_error`).
    pub(super) fn choose_checkout(
        &mut self,
        candidate: mast_proto::DiffCheckoutCandidate,
        cx: &mut Context<Self>,
    ) {
        use mast_proto::{DiffCheckoutKind, SetChatDiffTargetAction};
        if let Some(chat_id) = self.owner_chat_id_owned() {
            self.state.update(cx, |state, cx| {
                state.clear_chat_diff_member_pick(&chat_id);
                let (action, path) = match candidate.kind {
                    DiffCheckoutKind::Own => {
                        // Beside targets the own checkout is a pill: keep it
                        // picked so a bound Chat does not fall back to its
                        // primary member.
                        let own = crate::work_outline::execution::own_member_id(&chat_id);
                        if state
                            .chat_execution_members(&chat_id)
                            .iter()
                            .any(|member| member.repo_id == own)
                        {
                            state.pick_chat_execution_member_locally(&chat_id, &own);
                        }
                        (SetChatDiffTargetAction::ClearSelection, None)
                    }
                    DiffCheckoutKind::Submodule | DiffCheckoutKind::Worktree => {
                        (SetChatDiffTargetAction::Select, Some(candidate.path))
                    }
                };
                state
                    .set_chat_diff_target(&chat_id, action, path, cx)
                    .detach();
                cx.notify();
            });
        }
        self.close_checkout_menu(cx);
    }

    pub(super) fn remove_checkout(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else { return };
        let task = self.state.update(cx, |state, cx| {
            state.set_chat_diff_target(&chat_id, mast_proto::SetChatDiffTargetAction::Unbind, Some(path.clone()), cx)
        });
        cx.spawn(async move |this, cx| {
            if task.await.is_ok() {
                let _ = this.update(cx, |this, cx| {
                    if let Some(menu) = this.checkout_menu.open_mut() {
                        if let popover::Loadable::Ready(rows) = &mut menu.candidates {
                            rows.retain(|row| row.candidate.path != path);
                        }
                        menu.active = 0;
                    }
                    cx.notify();
                });
            }
        }).detach();
    }

}
