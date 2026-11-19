use super::*;

/// (Re-)apply the whole app keymap: clears every binding, restores the composer
/// map, then binds the customizable shortcuts from `keymap` (feature-inventory
/// §1.4). Invalid persisted combos fall back to that shortcut's default.
pub fn apply_keymap(
    cx: &mut App,
    keymap: &KeymapConfig,
    composer_send_behavior: ComposerSendBehavior,
) {
    fn valid_or_default(combo: &str, fallback: &str) -> String {
        let candidate = platform_combo(combo);
        if Keystroke::parse(&candidate).is_ok() {
            candidate
        } else {
            tracing::warn!(%combo, "unparseable shortcut combo; using default");
            platform_combo(fallback)
        }
    }
    crate::appshots::set_shortcut(&keymap.capture_appshot);
    cx.clear_key_bindings();
    // `clear_key_bindings` also removes the contextual editing actions that
    // gpui-base installed at startup. Reinitialize the component layer before
    // rebuilding Mast's bindings so the file editor keymap remains active.
    gpui_base::init(cx);
    crate::files::code_find::bind_keys(cx);
    crate::composer::init(cx, composer_send_behavior);
    // Fixed app-level shortcuts (Settings on every platform; ⌘Q quit, ⌘W
    // close, ⌘M minimize, ⌘H hide on macOS) — these back the native menu
    // key equivalents and must survive keymap re-application.
    crate::app_menus::bind_keys(cx);
    crate::changes::walkthrough::bind_keys(cx, keymap);
    // gpui-base binds Replace to Cmd+Shift+F in every Input. Remove that
    // binding before installing the user-configured search and replace keys.
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-shift-f", gpui::Unbind("input::Replace".into()), Some("Input"))]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-h", gpui::Unbind("input::Replace".into()), Some("Input"))]);
    for (id, action) in [
        (ShortcutId::QuickOpen, Box::new(QuickOpen) as Box<dyn Action>),
        (ShortcutId::QuickOpenSymbols, Box::new(QuickOpenSymbols) as Box<dyn Action>),
        (ShortcutId::FileNavigationBack, Box::new(FileNavigationBack) as Box<dyn Action>),
        (ShortcutId::FileNavigationForward, Box::new(FileNavigationForward) as Box<dyn Action>),
        (ShortcutId::AddCodeToChat, Box::new(AddCodeToChat) as Box<dyn Action>),
        (ShortcutId::SearchWorkspaceContent, Box::new(SearchWorkspaceContent) as Box<dyn Action>),
        (ShortcutId::GoToLine, Box::new(GoToLine) as Box<dyn Action>),
        (ShortcutId::ToggleBlameGutter, Box::new(ToggleBlameGutter) as Box<dyn Action>),
        (ShortcutId::ReplaceFile, Box::new(gpui_base::input::Replace) as Box<dyn Action>),
    ] {
        let combo = keymap.get(id);
        if combo.is_empty() { continue; }
        let context = if matches!(id, ShortcutId::AddCodeToChat | ShortcutId::FileNavigationBack | ShortcutId::FileNavigationForward) { Some("Files") } else { (id == ShortcutId::ReplaceFile).then_some("Input") };
        let predicate = context.map(|value| std::rc::Rc::new(gpui::KeyBindingContextPredicate::parse(value).unwrap()));
        if let Ok(binding) = KeyBinding::load(&valid_or_default(combo, id.default_combo()), action, predicate, false, None, &gpui::DummyKeyboardMapper) {
            cx.bind_keys([binding]);
        }
    }
    cx.bind_keys([
        KeyBinding::new(
            &valid_or_default(&keymap.save_file, "mod-s"),
            SaveFile,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_sidebar, "mod-b"),
            ToggleSidebar,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_changes, "mod-r"),
            ToggleChanges,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_files, "mod-e"),
            ToggleFiles,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_terminal, "mod-j"),
            ToggleTerminal,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.open_my_prs, "mod-p"),
            OpenMyPrs,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.new_session, "mod-n"),
            NewSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(
                &keymap.next_session,
                crate::settings::ShortcutId::NextSession.default_combo(),
            ),
            NextSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(
                &keymap.prev_session,
                crate::settings::ShortcutId::PrevSession.default_combo(),
            ),
            PrevSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.archive_session, "mod-shift-a"),
            ArchiveSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.new_project, ShortcutId::NewProject.default_combo()),
            AddSpacePalette,
            None,
        ),
        // Fixed: ⌘K summons the command palette.
        // Pressing it again dismisses.
        KeyBinding::new(&platform_combo("mod-k"), ToggleCommandPalette, None),
        // Fixed: ⌘⇧O (Ctrl+Shift+O elsewhere) opens the multi-chat overview.
        // Not (yet) a customizable `ShortcutId` — same tier as ⌘K above.
        KeyBinding::new(&platform_combo("mod-shift-o"), OpenOverview, None),
        // Contextual overview navigation: Find targets the Overview search
        // (List and Outline) and ⌘⇧G opens the repository/worktree map.
        KeyBinding::new(
            &platform_combo(FOCUS_OVERVIEW_SEARCH_COMBO),
            FocusOverviewSearch,
            None,
        ),
        KeyBinding::new(
            &platform_combo(OPEN_PR_REVIEW_CHATS_COMBO),
            OpenPrReviewChats,
            None,
        ),
        KeyBinding::new(&platform_combo(OPEN_ITEM_TAB_COMBO), OpenItemTab, None),
        KeyBinding::new(
            &platform_combo(running_ui::TOGGLE_RUNNING_CHATS_COMBO),
            ToggleRunningChats,
            None,
        ),
        KeyBinding::new(
            &platform_combo(TOGGLE_ITEM_CARD_COMBO),
            ToggleItemCard,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.open_model_picker, "mod-/"),
            OpenModelPicker,
            None,
        ),
    ]);
    if !keymap.open_team_tab.is_empty() {
        cx.bind_keys([KeyBinding::new(
            &valid_or_default(&keymap.open_team_tab, ShortcutId::OpenTeamTab.default_combo()),
            OpenTeamTab,
            None,
        )]);
    }
    if !keymap.open_team_tab_from_panel.is_empty() {
        cx.bind_keys([KeyBinding::new(
            &valid_or_default(&keymap.open_team_tab_from_panel, ShortcutId::OpenTeamTabFromPanel.default_combo()),
            OpenTeamTabFromPanel,
            Some("TeamPanel && !Composer && !MessageComposer && !PaletteSearch"),
        )]);
    }
    if !keymap.pop_out_surface.is_empty() {
        cx.bind_keys([KeyBinding::new(
            &valid_or_default(&keymap.pop_out_surface, ShortcutId::PopOutSurface.default_combo()),
            PopOutSurface,
            None,
        )]);
    }
    crate::browser::bind_keys(cx, keymap);
    // ⌘F / ⌘G / ⇧⌘G inside an Artifact tab (after the global ⌘F above).
    crate::artifacts::tab::bind_keys(cx);
    // ⌘1..⌘9 open the sidebar's first nine rows. A slot left unbound (an empty
    // combo in a hand-edited file) binds nothing rather than falling back —
    // the user cleared it on purpose.
    cx.bind_keys((0..JUMP_SLOTS).filter_map(|slot| {
        let id = ShortcutId::JumpSession(slot);
        let combo = keymap.get(id);
        if combo.is_empty() {
            return None;
        }
        Some(KeyBinding::new(
            &valid_or_default(combo, id.default_combo()),
            JumpSession(slot),
            None,
        ))
    }));
    // Work Outline commands (empty combos stay unbound).
    cx.bind_keys(work_outline_ui::work_outline_bindings(keymap));
    // Outline navigation keys, context `WorkOutline` minus text inputs.
    work_outline_ui::bind_work_outline_navigation(cx, keymap);
}

impl Shell {
    pub(super) fn active_changes(&self, cx: &App) -> Option<Entity<Changes>> {
        if !self.right_pane_open(cx) {
            return None;
        }
        let RightSurface::Diff(id) = self.resolved_right_active(cx) else {
            return None;
        };
        self.diffs.get(&id).cloned()
    }

    /// Resolve shell-owned Escape surfaces in capture phase, before focused
    /// descendants such as an integrated terminal can consume the key.
    pub(super) fn capture_escape_surface(&mut self, cx: &mut Context<Self>) -> bool {
        // Modals and context menus sit above the rest of the shell. Preserve
        // their existing behavior: only surfaces that already have a Cancel
        // path close here; the others remain explicit blockers.
        if self.closeout_dialog.is_some() {
            self.close_chat_closeout(cx);
            return true;
        }
        if self.admin_ui.is_open() {
            self.close_admin_dialog(cx);
            return true;
        }
        if self.sync_flow.has_visible_overlay()
            || self.delete_confirm.is_some()
            || self.work_outline_modal_open()
            || self.delete_space_confirm.is_some()
            || self.chat_menu.get().is_some()
            || self.space_menu.get().is_some()
            || self.user_menu.get().is_some()
            || self.work_outline_menu.get().is_some()
        {
            return true;
        }
        if self.rename_dialog.is_some() {
            self.rename_dialog = None;
            cx.notify();
            return true;
        }
        if self.rename_space_dialog.is_some() {
            self.rename_space_dialog = None;
            cx.notify();
            return true;
        }
        if let Some(flow) = self.account_flow.clone() {
            flow.update(cx, |flow, cx| flow.cancel(cx));
            return true;
        }
        if self.add_space.is_some() {
            self.add_space = None;
            cx.notify();
            return true;
        }
        if self.spaces_menu.is_open() {
            self.close_spaces_menu(cx);
            return true;
        }
        if self.spaces_menu.get().is_some() {
            return true;
        }

        if self.right_plus.is_open() {
            self.close_right_plus(cx);
            return true;
        }
        if self.right_plus.get().is_some() {
            return true;
        }
        self.active_changes(cx)
            .is_some_and(|changes| changes.update(cx, |changes, cx| changes.handle_escape(cx)))
    }

    pub(super) fn on_key_down_capture(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.route, Route::Overview) {
            work_outline_ui::log_restructure_keystroke("capture", event, window, cx);
        }
        if event.keystroke.key == "escape" && self.sidebar_session_transfer.is_some() {
            cx.stop_active_drag(window);
            self.cancel_sidebar_session_transfer(cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.work_outline_modal_open() {
            let picker_closed = self.hand_off_pickers().is_some_and(|pickers| pickers.update(cx, |pickers, cx| pickers.close_open_popover(cx)));
            if !picker_closed && !self.dismiss_work_outline_modal_skills(cx) {
                self.cancel_work_outline_modal(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.work_outline_menu.is_open() {
            self.dismiss_work_outline_menu(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.workspace_search.is_some() {
            self.close_workspace_search(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.capture_escape_surface(cx) {
            cx.stop_propagation();
        }
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Reaching this bubble listener means no bound action handled the key.
        if matches!(self.route, Route::Overview) {
            work_outline_ui::log_restructure_keystroke("unhandled", event, window, cx);
        }
        // Inputs and completion menus consume Tab first. Unhandled Tab walks
        // accessible controls, including individual transcript link ranges.
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key == "tab"
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
        {
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        let selected_chat = self.state.read(cx).selected_chat.clone();
        let indicator = selected_chat
            .as_deref()
            .map(|chat_id| self.state.read(cx).indicator_for(chat_id, Utc::now()))
            .unwrap_or(Indicator::None);
        let interrupting = selected_chat
            .as_deref()
            .is_some_and(|chat_id| self.composer.read(cx).is_interrupting(chat_id));
        let escape_stops_active_agent = self.settings.escape_stops_active_agent;

        match resolve_shell_escape(
            &event.keystroke.key,
            false,
            escape_stops_active_agent,
            self.route,
            selected_chat.as_deref(),
            indicator,
            interrupting,
        ) {
            ShellEscapeOutcome::Blocked => cx.stop_propagation(),
            ShellEscapeOutcome::InterruptChat(chat_id) => {
                cx.stop_propagation();
                self.composer
                    .update(cx, |composer, cx| composer.interrupt_chat(chat_id, cx));
            }
            ShellEscapeOutcome::OtherKey | ShellEscapeOutcome::Ignored => {}
        }
    }

}
