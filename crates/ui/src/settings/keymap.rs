use super::*;

// ---------------------------------------------------------------------------
// Keymap (customizable shortcuts, §1.4)
// ---------------------------------------------------------------------------

/// Toggle Outline Focus's first default. A stored binding still equal to it
/// was never customized, so loading moves it to the current default.
pub(super) const TOGGLE_OUTLINE_FOCUS_LEGACY_DEFAULT: &str = "mod-\\";

/// How many sidebar rows the jump shortcuts reach (t3code's
/// `THREAD_JUMP_KEYBINDING_COMMANDS`, nine slots).
pub const JUMP_SLOTS: usize = 9;

/// Default combo per jump slot, and the label the shortcuts table shows.
pub(super) const JUMP_DEFAULTS: [&str; JUMP_SLOTS] = [
    "mod-1", "mod-2", "mod-3", "mod-4", "mod-5", "mod-6", "mod-7", "mod-8", "mod-9",
];
pub(super) const JUMP_LABELS: [&str; JUMP_SLOTS] = [
    "Jump to session 1",
    "Jump to session 2",
    "Jump to session 3",
    "Jump to session 4",
    "Jump to session 5",
    "Jump to session 6",
    "Jump to session 7",
    "Jump to session 8",
    "Jump to session 9",
];

/// Work Outline commands registered as rebindable shortcuts (every
/// `WorkOutlineCommand`, including the ones whose palette entry waits for a
/// later milestone).
pub const WORK_OUTLINE_SHORTCUTS: usize = 18;

/// The rebindable app shortcuts. `JumpSession(slot)` is zero-based; a slot at
/// or past [`JUMP_SLOTS`] has no combo and no label, so it reads as unbound
/// rather than panicking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutId {
    CaptureAppshot,
    SaveFile,
    BrowserReload,
    ToggleSidebar,
    ToggleChanges,
    ToggleFiles,
    ToggleTerminal,
    OpenMyPrs,
    QuickOpen,
    QuickOpenSymbols,
    GoToDefinition,
    GoToDefinitionAlternate,
    FileNavigationBack,
    FileNavigationForward,
    AddCodeToChat,
    ReviewNextStep,
    ReviewPreviousStep,
    ReviewNextStepArrow,
    ReviewPreviousStepArrow,
    ReviewNextHunk,
    ReviewPreviousHunk,
    ReviewMarkReviewed,
    ReviewToggleMode,
    ExplainDiff,
    SearchWorkspaceContent,
    GoToLine,
    ReplaceFile,
    NewSession,
    NewProject,
    OpenModelPicker,
    NextSession,
    PrevSession,
    ArchiveSession,
    OpenOutline,
    ToggleOutlineFocus,
    OpenFullChat,
    CreateWorkItem,
    CreateChildWorkItem,
    RenameWorkItem,
    MoveWorkItem,
    AssociateChatWithWorkItem,
    OpenEffectiveInstructions,
    CreateInstructionFromChat,
    OpenRequiredActions,
    StartChatForWorkItem,
    MoveWorkItemUp,
    MoveWorkItemDown,
    IndentWorkItem,
    OutdentWorkItem,
    ShowOutlineShortcuts,
    OpenLinear,
    OpenTeamTab,
    OpenTeamTabFromPanel,
    PopOutSurface,
    Outline(usize),
    JumpSession(usize),
}

impl ShortcutId {
    pub const ALL: [ShortcutId; 34 + WORK_OUTLINE_SHORTCUTS + 25 + JUMP_SLOTS] = [
        ShortcutId::CaptureAppshot,
        ShortcutId::SaveFile,
        ShortcutId::BrowserReload,
        ShortcutId::ToggleSidebar,
        ShortcutId::ToggleChanges,
        ShortcutId::ToggleFiles,
        ShortcutId::ToggleTerminal,
        ShortcutId::OpenMyPrs,
        ShortcutId::QuickOpen,
        ShortcutId::QuickOpenSymbols,
        ShortcutId::GoToDefinition,
        ShortcutId::GoToDefinitionAlternate,
        ShortcutId::FileNavigationBack,
        ShortcutId::FileNavigationForward,
        ShortcutId::ReviewNextStep,
        ShortcutId::ReviewPreviousStep,
        ShortcutId::ReviewNextStepArrow,
        ShortcutId::ReviewPreviousStepArrow,
        ShortcutId::ReviewNextHunk,
        ShortcutId::ReviewPreviousHunk,
        ShortcutId::ReviewMarkReviewed,
        ShortcutId::ReviewToggleMode,
        ShortcutId::ExplainDiff,
        ShortcutId::SearchWorkspaceContent,
        ShortcutId::GoToLine,
        ShortcutId::ReplaceFile,
        ShortcutId::NewSession,
        ShortcutId::NewProject,
        ShortcutId::OpenModelPicker,
        ShortcutId::NextSession,
        ShortcutId::PrevSession,
        ShortcutId::ArchiveSession,
        ShortcutId::OpenOutline,
        ShortcutId::AddCodeToChat,
        ShortcutId::ToggleOutlineFocus,
        ShortcutId::OpenFullChat,
        ShortcutId::CreateWorkItem,
        ShortcutId::CreateChildWorkItem,
        ShortcutId::RenameWorkItem,
        ShortcutId::MoveWorkItem,
        ShortcutId::AssociateChatWithWorkItem,
        ShortcutId::OpenEffectiveInstructions,
        ShortcutId::CreateInstructionFromChat,
        ShortcutId::OpenRequiredActions,
        ShortcutId::StartChatForWorkItem,
        ShortcutId::MoveWorkItemUp,
        ShortcutId::MoveWorkItemDown,
        ShortcutId::IndentWorkItem,
        ShortcutId::OutdentWorkItem,
        ShortcutId::ShowOutlineShortcuts,
        ShortcutId::OpenLinear,
        ShortcutId::OpenTeamTab,
        ShortcutId::OpenTeamTabFromPanel,
        ShortcutId::PopOutSurface,
        ShortcutId::Outline(0),
        ShortcutId::Outline(1),
        ShortcutId::Outline(2),
        ShortcutId::Outline(3),
        ShortcutId::Outline(4),
        ShortcutId::Outline(5),
        ShortcutId::Outline(6),
        ShortcutId::Outline(7),
        ShortcutId::Outline(8),
        ShortcutId::Outline(9),
        ShortcutId::Outline(10),
        ShortcutId::Outline(11),
        ShortcutId::Outline(12),
        ShortcutId::Outline(13),
        ShortcutId::Outline(14),
        ShortcutId::Outline(15),
        ShortcutId::Outline(16),
        ShortcutId::Outline(17),
        ShortcutId::Outline(18),
        ShortcutId::Outline(19),
        ShortcutId::Outline(20),
        ShortcutId::Outline(21),
        ShortcutId::Outline(22),
        ShortcutId::JumpSession(0),
        ShortcutId::JumpSession(1),
        ShortcutId::JumpSession(2),
        ShortcutId::JumpSession(3),
        ShortcutId::JumpSession(4),
        ShortcutId::JumpSession(5),
        ShortcutId::JumpSession(6),
        ShortcutId::JumpSession(7),
        ShortcutId::JumpSession(8),
    ];

    pub fn available(self) -> bool {
        self != Self::CaptureAppshot || crate::appshots::is_desktop()
    }

    /// Row label (zeron lib/shortcuts.ts `SHORTCUT_DEFINITIONS`, verbatim).
    pub fn label(self) -> &'static str {
        match self {
            ShortcutId::CaptureAppshot => "Capture Appshot",
            ShortcutId::SaveFile => "Save file",
            ShortcutId::BrowserReload => "Reload browser page",
            ShortcutId::ToggleSidebar => "Toggle left sidebar",
            ShortcutId::ToggleChanges => "Toggle right sidebar",
            ShortcutId::ToggleFiles => "Toggle files panel",
            ShortcutId::ToggleTerminal => "Toggle terminal",
            ShortcutId::OpenMyPrs => "Open PR page",
            ShortcutId::ReplaceFile => "Replace in file",
            ShortcutId::GoToLine => "Go to line",
            ShortcutId::SearchWorkspaceContent => "Search in files",
            ShortcutId::QuickOpen => "Quick open",
            ShortcutId::QuickOpenSymbols => "Search symbols",
            ShortcutId::GoToDefinition => "Go to definition",
            ShortcutId::GoToDefinitionAlternate => "Go to definition (alternate)",
            ShortcutId::FileNavigationBack => "Navigate back",
            ShortcutId::FileNavigationForward => "Navigate forward",
            ShortcutId::AddCodeToChat => "Add code to chat",
            ShortcutId::ReviewNextStep => "Next review step",
            ShortcutId::ReviewPreviousStep => "Previous review step",
            ShortcutId::ReviewNextStepArrow => "Next review step (arrow)",
            ShortcutId::ReviewPreviousStepArrow => "Previous review step (arrow)",
            ShortcutId::ReviewNextHunk => "Next review hunk",
            ShortcutId::ReviewPreviousHunk => "Previous review hunk",
            ShortcutId::ReviewMarkReviewed => "Mark review hunk viewed",
            ShortcutId::ReviewToggleMode => "Toggle review mode",
            ShortcutId::ExplainDiff => "Explain diff selection",

            ShortcutId::NewSession => "New session",
            ShortcutId::NewProject => "New Space",
            ShortcutId::OpenModelPicker => "Open model picker",
            ShortcutId::NextSession => "Next session",
            ShortcutId::PrevSession => "Previous session",
            ShortcutId::ArchiveSession => "Archive session",
            ShortcutId::OpenOutline => "Open Outline",
            ShortcutId::ToggleOutlineFocus => "Toggle Outline focus",
            ShortcutId::OpenFullChat => "Open full chat",
            ShortcutId::CreateWorkItem => "Create work item",
            ShortcutId::CreateChildWorkItem => "Create child work item",
            ShortcutId::RenameWorkItem => "Rename work item",
            ShortcutId::MoveWorkItem => "Move work item",
            ShortcutId::AssociateChatWithWorkItem => "Associate chat with work item",
            ShortcutId::OpenEffectiveInstructions => "Open effective instructions",
            ShortcutId::CreateInstructionFromChat => "Create instruction from chat",
            ShortcutId::OpenRequiredActions => "Open required actions",
            ShortcutId::StartChatForWorkItem => "Start chat in work item",
            ShortcutId::MoveWorkItemUp => "Move work item up",
            ShortcutId::MoveWorkItemDown => "Move work item down",
            ShortcutId::IndentWorkItem => "Indent work item",
            ShortcutId::OutdentWorkItem => "Outdent work item",
            ShortcutId::ShowOutlineShortcuts => "Show Outline shortcuts",
            ShortcutId::OpenLinear => "Toggle Inbox column (Linear, GitHub)",
            ShortcutId::OpenTeamTab => "Open Tracker tab",
            ShortcutId::OpenTeamTabFromPanel => "Open Tracker tab from side panel",
            ShortcutId::PopOutSurface => "Open tab in new window",
            ShortcutId::Outline(index) => crate::shell::work_outline_ui::OutlineNavKey::ALL
                .get(index)
                .map_or("", |key| key.label()),
            ShortcutId::JumpSession(slot) => JUMP_LABELS.get(slot).copied().unwrap_or(""),
        }
    }

    pub fn default_combo(self) -> &'static str {
        self.default_combo_on(cfg!(target_os = "macos"))
    }

    /// `default_combo` for an explicit platform, so the spelling invariant is
    /// testable for both from any machine (see the tests below — the mismatch
    /// this guards against only exists off macOS).
    pub fn default_combo_on(self, mac: bool) -> &'static str {
        match self {
            ShortcutId::CaptureAppshot if mac => "ctrl-alt-space",
            ShortcutId::CaptureAppshot => "mod-alt-space",
            ShortcutId::SaveFile => "mod-s",
            ShortcutId::BrowserReload => "mod-shift-r",
            ShortcutId::ToggleSidebar => "mod-b",
            ShortcutId::ToggleChanges => "mod-r",
            ShortcutId::ToggleFiles => "mod-e",
            ShortcutId::ToggleTerminal => "mod-j",
            ShortcutId::OpenMyPrs => "mod-p",
            ShortcutId::ReplaceFile => "mod-alt-f",
            ShortcutId::GoToLine => "mod-g",
            ShortcutId::SearchWorkspaceContent => "mod-shift-f",
            ShortcutId::QuickOpen => "mod-o",
            ShortcutId::QuickOpenSymbols => "mod-alt-shift-o",
            ShortcutId::GoToDefinition => "f12",
            ShortcutId::GoToDefinitionAlternate => "mod-alt-b",
            ShortcutId::FileNavigationBack => if mac { "ctrl--" } else { "mod--" },
            ShortcutId::FileNavigationForward => if mac { "ctrl-shift--" } else { "mod-shift--" },
            ShortcutId::AddCodeToChat => "mod-shift-l",
            ShortcutId::ReviewNextStep => "n",
            ShortcutId::ReviewPreviousStep => "p",
            ShortcutId::ReviewNextStepArrow => "down",
            ShortcutId::ReviewPreviousStepArrow => "up",
            ShortcutId::ReviewNextHunk => "]",
            ShortcutId::ReviewPreviousHunk => "[",
            ShortcutId::ReviewMarkReviewed => "v",
            ShortcutId::ReviewToggleMode => "t",
            ShortcutId::ExplainDiff => "e",

            ShortcutId::NewSession => "mod-n",
            ShortcutId::NewProject => "mod-shift-n",
            ShortcutId::OpenModelPicker => "mod-/",
            // Ctrl+Tab on every platform — but spelled the way THAT platform's
            // recorder spells ctrl (see `combo_from_keystroke`). Off macOS
            // ctrl IS the primary and stores as "mod"; on macOS it is its own
            // modifier, and "mod" would mean Cmd+Tab, which the OS app
            // switcher eats.
            //
            // Off macOS "ctrl-tab" and "mod-tab" resolve to the same keystroke
            // through `platform_combo`, but conflict detection compares the
            // STORED spelling — so a default the recorder cannot reproduce
            // would let a rebind onto that same physical key pass as
            // conflict-free, bind twice, and silently kill one shortcut.
            ShortcutId::NextSession if mac => "ctrl-tab",
            ShortcutId::NextSession => "mod-tab",
            ShortcutId::PrevSession if mac => "ctrl-shift-tab",
            ShortcutId::PrevSession => "mod-shift-tab",
            // Mod+A is the composer's Select all, so archiving takes the
            // shifted combo.
            ShortcutId::ArchiveSession => "mod-shift-a",
            // Work Outline. Only the three global entry points ship bound: a
            // chord here fires even while a text field has focus, and `mod-l`,
            // `mod-i` and the other free letters are one step from composer or
            // browser chords. Everything else stays reachable from the command
            // palette (and the outline's own context keys) and is unbound here
            // until a user records a chord; `mod-shift-l` / `mod-shift-i` clash
            // with no default, fixed combo or editing binding. The four
            // restructuring commands already have Outline-scoped keys
            // (Shift+Arrows, Tab, Shift+Tab in
            // `OUTLINE_NAVIGATION_KEYS`) that never fire in a text field, so
            // their global chords also ship unbound.
            ShortcutId::OpenOutline => "mod-shift-l",
            // Chat <-> Outline tree. Must fire from the composer (where Enter
            // on a Chat row lands focus), so it is a primary chord. Not
            // `mod-\`: 1Password claims Cmd+\ system-wide for autofill and
            // macOS delivers it there first (see
            // `TOGGLE_OUTLINE_FOCUS_LEGACY_DEFAULT`). `mod-;` clashes with no
            // default, fixed (`app_menus`, palette, overview), composer,
            // browser or gpui-base editing binding, and no common macOS
            // utility registers it globally.
            ShortcutId::ToggleOutlineFocus => "mod-;",
            // Overview chat panel / selected Chat row -> the full chat route.
            // Must fire from the panel's composer, so it is a primary chord;
            // `mod-shift-enter` clashes with no default or fixed binding and
            // is not the composer's send (`mod-enter`, reserved) or newline
            // (`shift-enter`). Off the Overview it does nothing.
            ShortcutId::OpenFullChat => "mod-shift-enter",
            ShortcutId::CreateWorkItem => "mod-shift-i",
            ShortcutId::CreateChildWorkItem
            | ShortcutId::RenameWorkItem
            | ShortcutId::MoveWorkItem
            | ShortcutId::AssociateChatWithWorkItem
            | ShortcutId::OpenEffectiveInstructions
            | ShortcutId::CreateInstructionFromChat
            | ShortcutId::OpenRequiredActions
            | ShortcutId::StartChatForWorkItem
            | ShortcutId::MoveWorkItemUp
            | ShortcutId::MoveWorkItemDown
            | ShortcutId::IndentWorkItem
            | ShortcutId::OutdentWorkItem
            // The reference opens with `?` inside the Outline; the global
            // chord is opt-in.
            | ShortcutId::ShowOutlineShortcuts => "",
            // Linear panel toggle. Must fire from the chat panel composer and
            // the panel's own search, so it is a primary chord. ⌥⌘L clashes
            // with no default, fixed (`app_menus`, palette, overview),
            // composer, browser or gpui-base binding. Off macOS Ctrl+Alt+L
            // locks the screen on common Linux desktops, so it takes the
            // free Ctrl+Shift+E there. It only acts while some Space has
            // Linear on.
            ShortcutId::OpenLinear if mac => "mod-alt-l",
            ShortcutId::OpenLinear => "mod-shift-e",
            ShortcutId::OpenTeamTab => "mod-alt-t",
            ShortcutId::OpenTeamTabFromPanel => "t",
            // The active right-pane tab becomes its own window. Free of every
            // default, fixed and browser chord; also the tab context menu row.
            ShortcutId::PopOutSurface => "mod-alt-n",
            ShortcutId::Outline(index) => crate::shell::work_outline_ui::OutlineNavKey::ALL.get(index).and_then(|key| crate::shell::work_outline_ui::OUTLINE_NAVIGATION_KEYS.iter().find(|(_, candidate)| candidate == key).map(|(combo, _)| *combo)).unwrap_or(""),
            ShortcutId::JumpSession(slot) => JUMP_DEFAULTS.get(slot).copied().unwrap_or(""),
        }
    }

    /// The sidebar row this id jumps to, if it is a jump shortcut.
    pub fn jump_slot(self) -> Option<usize> {
        match self {
            ShortcutId::JumpSession(slot) if slot < JUMP_SLOTS => Some(slot),
            _ => None,
        }
    }
}

/// Persisted shortcut combos. Stored platform-neutral ("mod-s"); translated to
/// "cmd-s"/"ctrl-s" at bind time by [`platform_combo`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KeymapConfig {
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), serde(skip))]
    pub capture_appshot: String,
    pub save_file: String,
    pub browser_reload: String,
    pub toggle_sidebar: String,
    pub toggle_changes: String,
    pub toggle_files: String,
    pub toggle_terminal: String,
    pub open_my_prs: String,
    pub replace_file: String,
    pub go_to_line: String,
    pub search_workspace_content: String,
    pub quick_open: String,
    pub quick_open_symbols: String,
    pub go_to_definition: String,
    pub go_to_definition_alternate: String,
    pub file_navigation_back: String,
    pub file_navigation_forward: String,
    pub add_code_to_chat: String,
    pub review_next_step: String,
    pub review_previous_step: String,
    pub review_next_step_arrow: String,
    pub review_previous_step_arrow: String,
    pub review_next_hunk: String,
    pub review_previous_hunk: String,
    pub review_mark_reviewed: String,
    pub review_toggle_mode: String,
    pub explain_diff: String,

    pub new_session: String,
    pub new_project: String,
    pub open_model_picker: String,
    pub next_session: String,
    pub prev_session: String,
    pub archive_session: String,
    pub open_outline: String,
    pub toggle_outline_focus: String,
    pub open_full_chat: String,
    pub create_work_item: String,
    pub create_child_work_item: String,
    pub rename_work_item: String,
    pub move_work_item: String,
    pub associate_chat_with_work_item: String,
    pub open_effective_instructions: String,
    pub create_instruction_from_chat: String,
    pub open_required_actions: String,
    pub start_chat_for_work_item: String,
    pub move_work_item_up: String,
    pub move_work_item_down: String,
    pub indent_work_item: String,
    pub outdent_work_item: String,
    pub show_outline_shortcuts: String,
    pub open_linear: String,
    pub open_team_tab: String,
    pub open_team_tab_from_panel: String,
    pub pop_out_surface: String,
    /// Overrides for outline actions; absent entries retain all default aliases.
    pub outline_shortcuts: HashMap<String, Vec<String>>,
    /// One combo per jump slot, in slot order. A list rather than nine fields:
    /// [`UiSettings::load`] discards the WHOLE file on a parse error, so a
    /// fixed-length array would let one malformed entry reset every unrelated
    /// setting. [`Self::healed`] restores the length instead.
    pub jump_session: Vec<String>,
}

/// Stable key for device-local preferences that belong to one workspace
/// profile. Authentication may arrive after `EngineInfo`, so callers must
/// treat `None` as "identity not ready" and avoid destructive cleanup.
pub fn sidebar_pin_profile_key(
    scope: Option<WorkspaceScope>,
    auth: Option<&AuthState>,
    development_org_id: Option<&str>,
) -> Option<String> {
    match scope? {
        WorkspaceScope::Local => Some("local".to_string()),
        WorkspaceScope::Synced => {
            let AuthState::SignedIn {
                user,
                org_id: Some(org_id),
            } = auth?
            else {
                return None;
            };
            Some(format!("synced:{org_id}:{}", user.id))
        }
        WorkspaceScope::Development => {
            let AuthState::SignedIn { user, .. } = auth? else {
                return None;
            };
            let (user_id, token_org_id) = user
                .id
                .split_once('@')
                .map_or((user.id.as_str(), None), |(user_id, org_id)| {
                    (user_id, (!org_id.is_empty()).then_some(org_id))
                });
            if user_id.is_empty() {
                return None;
            }
            let org_id = token_org_id
                .or(development_org_id.filter(|org_id| !org_id.is_empty()))
                .unwrap_or(mast_engine::DEFAULT_ORG_ID);
            Some(format!("development:{org_id}:{user_id}"))
        }
    }
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            capture_appshot: ShortcutId::CaptureAppshot.default_combo().into(),
            save_file: ShortcutId::SaveFile.default_combo().into(),
            browser_reload: ShortcutId::BrowserReload.default_combo().into(),
            toggle_sidebar: ShortcutId::ToggleSidebar.default_combo().into(),
            toggle_changes: ShortcutId::ToggleChanges.default_combo().into(),
            toggle_files: ShortcutId::ToggleFiles.default_combo().into(),
            toggle_terminal: ShortcutId::ToggleTerminal.default_combo().into(),
            open_my_prs: ShortcutId::OpenMyPrs.default_combo().into(),
            replace_file: ShortcutId::ReplaceFile.default_combo().into(),
            go_to_line: ShortcutId::GoToLine.default_combo().into(),
            search_workspace_content: ShortcutId::SearchWorkspaceContent.default_combo().into(),
            quick_open: ShortcutId::QuickOpen.default_combo().into(),
            quick_open_symbols: ShortcutId::QuickOpenSymbols.default_combo().into(),
            go_to_definition: ShortcutId::GoToDefinition.default_combo().into(),
            go_to_definition_alternate: ShortcutId::GoToDefinitionAlternate.default_combo().into(),
            file_navigation_back: ShortcutId::FileNavigationBack.default_combo().into(),
            file_navigation_forward: ShortcutId::FileNavigationForward.default_combo().into(),
            add_code_to_chat: ShortcutId::AddCodeToChat.default_combo().into(),
            review_next_step: ShortcutId::ReviewNextStep.default_combo().into(),
            review_previous_step: ShortcutId::ReviewPreviousStep.default_combo().into(),
            review_next_step_arrow: ShortcutId::ReviewNextStepArrow.default_combo().into(),
            review_previous_step_arrow: ShortcutId::ReviewPreviousStepArrow.default_combo().into(),
            review_next_hunk: ShortcutId::ReviewNextHunk.default_combo().into(),
            review_previous_hunk: ShortcutId::ReviewPreviousHunk.default_combo().into(),
            review_mark_reviewed: ShortcutId::ReviewMarkReviewed.default_combo().into(),
            review_toggle_mode: ShortcutId::ReviewToggleMode.default_combo().into(),
            explain_diff: ShortcutId::ExplainDiff.default_combo().into(),

            new_session: ShortcutId::NewSession.default_combo().into(),
            new_project: ShortcutId::NewProject.default_combo().into(),
            open_model_picker: ShortcutId::OpenModelPicker.default_combo().into(),
            next_session: ShortcutId::NextSession.default_combo().into(),
            prev_session: ShortcutId::PrevSession.default_combo().into(),
            archive_session: ShortcutId::ArchiveSession.default_combo().into(),
            open_outline: ShortcutId::OpenOutline.default_combo().into(),
            toggle_outline_focus: ShortcutId::ToggleOutlineFocus.default_combo().into(),
            open_full_chat: ShortcutId::OpenFullChat.default_combo().into(),
            create_work_item: ShortcutId::CreateWorkItem.default_combo().into(),
            create_child_work_item: ShortcutId::CreateChildWorkItem.default_combo().into(),
            rename_work_item: ShortcutId::RenameWorkItem.default_combo().into(),
            move_work_item: ShortcutId::MoveWorkItem.default_combo().into(),
            associate_chat_with_work_item: ShortcutId::AssociateChatWithWorkItem
                .default_combo()
                .into(),
            open_effective_instructions: ShortcutId::OpenEffectiveInstructions
                .default_combo()
                .into(),
            create_instruction_from_chat: ShortcutId::CreateInstructionFromChat
                .default_combo()
                .into(),
            open_required_actions: ShortcutId::OpenRequiredActions.default_combo().into(),
            start_chat_for_work_item: ShortcutId::StartChatForWorkItem.default_combo().into(),
            move_work_item_up: ShortcutId::MoveWorkItemUp.default_combo().into(),
            move_work_item_down: ShortcutId::MoveWorkItemDown.default_combo().into(),
            indent_work_item: ShortcutId::IndentWorkItem.default_combo().into(),
            outdent_work_item: ShortcutId::OutdentWorkItem.default_combo().into(),
            show_outline_shortcuts: ShortcutId::ShowOutlineShortcuts.default_combo().into(),
            open_linear: ShortcutId::OpenLinear.default_combo().into(),
            open_team_tab: ShortcutId::OpenTeamTab.default_combo().into(),
            open_team_tab_from_panel: ShortcutId::OpenTeamTabFromPanel.default_combo().into(),
            pop_out_surface: ShortcutId::PopOutSurface.default_combo().into(),
            outline_shortcuts: HashMap::new(),
            jump_session: JUMP_DEFAULTS.iter().map(|c| (*c).to_string()).collect(),
        }
    }
}

impl KeymapConfig {
    pub fn get(&self, id: ShortcutId) -> &str {
        match id {
            ShortcutId::CaptureAppshot => &self.capture_appshot,
            ShortcutId::SaveFile => &self.save_file,
            ShortcutId::BrowserReload => &self.browser_reload,
            ShortcutId::ToggleSidebar => &self.toggle_sidebar,
            ShortcutId::ToggleChanges => &self.toggle_changes,
            ShortcutId::ToggleFiles => &self.toggle_files,
            ShortcutId::ToggleTerminal => &self.toggle_terminal,
            ShortcutId::OpenMyPrs => &self.open_my_prs,
            ShortcutId::ReplaceFile => &self.replace_file,
            ShortcutId::GoToLine => &self.go_to_line,
            ShortcutId::SearchWorkspaceContent => &self.search_workspace_content,
            ShortcutId::QuickOpen => &self.quick_open,
            ShortcutId::QuickOpenSymbols => &self.quick_open_symbols,
            ShortcutId::GoToDefinition => &self.go_to_definition,
            ShortcutId::GoToDefinitionAlternate => &self.go_to_definition_alternate,
            ShortcutId::FileNavigationBack => &self.file_navigation_back,
            ShortcutId::FileNavigationForward => &self.file_navigation_forward,
            ShortcutId::AddCodeToChat => &self.add_code_to_chat,
            ShortcutId::ReviewNextStep => &self.review_next_step,
            ShortcutId::ReviewPreviousStep => &self.review_previous_step,
            ShortcutId::ReviewNextStepArrow => &self.review_next_step_arrow,
            ShortcutId::ReviewPreviousStepArrow => &self.review_previous_step_arrow,
            ShortcutId::ReviewNextHunk => &self.review_next_hunk,
            ShortcutId::ReviewPreviousHunk => &self.review_previous_hunk,
            ShortcutId::ReviewMarkReviewed => &self.review_mark_reviewed,
            ShortcutId::ReviewToggleMode => &self.review_toggle_mode,
            ShortcutId::ExplainDiff => &self.explain_diff,

            ShortcutId::NewSession => &self.new_session,
            ShortcutId::NewProject => &self.new_project,
            ShortcutId::OpenModelPicker => &self.open_model_picker,
            ShortcutId::NextSession => &self.next_session,
            ShortcutId::PrevSession => &self.prev_session,
            ShortcutId::ArchiveSession => &self.archive_session,
            ShortcutId::OpenOutline => &self.open_outline,
            ShortcutId::ToggleOutlineFocus => &self.toggle_outline_focus,
            ShortcutId::OpenFullChat => &self.open_full_chat,
            ShortcutId::CreateWorkItem => &self.create_work_item,
            ShortcutId::CreateChildWorkItem => &self.create_child_work_item,
            ShortcutId::RenameWorkItem => &self.rename_work_item,
            ShortcutId::MoveWorkItem => &self.move_work_item,
            ShortcutId::AssociateChatWithWorkItem => &self.associate_chat_with_work_item,
            ShortcutId::OpenEffectiveInstructions => &self.open_effective_instructions,
            ShortcutId::CreateInstructionFromChat => &self.create_instruction_from_chat,
            ShortcutId::OpenRequiredActions => &self.open_required_actions,
            ShortcutId::StartChatForWorkItem => &self.start_chat_for_work_item,
            ShortcutId::MoveWorkItemUp => &self.move_work_item_up,
            ShortcutId::MoveWorkItemDown => &self.move_work_item_down,
            ShortcutId::IndentWorkItem => &self.indent_work_item,
            ShortcutId::OutdentWorkItem => &self.outdent_work_item,
            ShortcutId::ShowOutlineShortcuts => &self.show_outline_shortcuts,
            ShortcutId::OpenLinear => &self.open_linear,
            ShortcutId::OpenTeamTab => &self.open_team_tab,
            ShortcutId::OpenTeamTabFromPanel => &self.open_team_tab_from_panel,
            ShortcutId::PopOutSurface => &self.pop_out_surface,
            ShortcutId::Outline(index) => {
                self.outline_combos(index).into_iter().next().unwrap_or("")
            }
            ShortcutId::JumpSession(slot) => self
                .jump_session
                .get(slot)
                .map(String::as_str)
                .unwrap_or(""),
        }
    }

    pub fn set(&mut self, id: ShortcutId, combo: String) {
        match id {
            ShortcutId::CaptureAppshot => self.capture_appshot = combo,
            ShortcutId::SaveFile => self.save_file = combo,
            ShortcutId::BrowserReload => self.browser_reload = combo,
            ShortcutId::ToggleSidebar => self.toggle_sidebar = combo,
            ShortcutId::ToggleChanges => self.toggle_changes = combo,
            ShortcutId::ToggleFiles => self.toggle_files = combo,
            ShortcutId::ToggleTerminal => self.toggle_terminal = combo,
            ShortcutId::OpenMyPrs => self.open_my_prs = combo,
            ShortcutId::ReplaceFile => self.replace_file = combo,
            ShortcutId::GoToLine => self.go_to_line = combo,
            ShortcutId::SearchWorkspaceContent => self.search_workspace_content = combo,
            ShortcutId::QuickOpen => self.quick_open = combo,
            ShortcutId::QuickOpenSymbols => self.quick_open_symbols = combo,
            ShortcutId::GoToDefinition => self.go_to_definition = combo,
            ShortcutId::GoToDefinitionAlternate => self.go_to_definition_alternate = combo,
            ShortcutId::FileNavigationBack => self.file_navigation_back = combo,
            ShortcutId::FileNavigationForward => self.file_navigation_forward = combo,
            ShortcutId::AddCodeToChat => self.add_code_to_chat = combo,
            ShortcutId::ReviewNextStep => self.review_next_step = combo,
            ShortcutId::ReviewPreviousStep => self.review_previous_step = combo,
            ShortcutId::ReviewNextStepArrow => self.review_next_step_arrow = combo,
            ShortcutId::ReviewPreviousStepArrow => self.review_previous_step_arrow = combo,
            ShortcutId::ReviewNextHunk => self.review_next_hunk = combo,
            ShortcutId::ReviewPreviousHunk => self.review_previous_hunk = combo,
            ShortcutId::ReviewMarkReviewed => self.review_mark_reviewed = combo,
            ShortcutId::ReviewToggleMode => self.review_toggle_mode = combo,
            ShortcutId::ExplainDiff => self.explain_diff = combo,

            ShortcutId::NewSession => self.new_session = combo,
            ShortcutId::NewProject => self.new_project = combo,
            ShortcutId::OpenModelPicker => self.open_model_picker = combo,
            ShortcutId::NextSession => self.next_session = combo,
            ShortcutId::PrevSession => self.prev_session = combo,
            ShortcutId::ArchiveSession => self.archive_session = combo,
            ShortcutId::OpenOutline => self.open_outline = combo,
            ShortcutId::ToggleOutlineFocus => self.toggle_outline_focus = combo,
            ShortcutId::OpenFullChat => self.open_full_chat = combo,
            ShortcutId::CreateWorkItem => self.create_work_item = combo,
            ShortcutId::CreateChildWorkItem => self.create_child_work_item = combo,
            ShortcutId::RenameWorkItem => self.rename_work_item = combo,
            ShortcutId::MoveWorkItem => self.move_work_item = combo,
            ShortcutId::AssociateChatWithWorkItem => self.associate_chat_with_work_item = combo,
            ShortcutId::OpenEffectiveInstructions => self.open_effective_instructions = combo,
            ShortcutId::CreateInstructionFromChat => self.create_instruction_from_chat = combo,
            ShortcutId::OpenRequiredActions => self.open_required_actions = combo,
            ShortcutId::StartChatForWorkItem => self.start_chat_for_work_item = combo,
            ShortcutId::MoveWorkItemUp => self.move_work_item_up = combo,
            ShortcutId::MoveWorkItemDown => self.move_work_item_down = combo,
            ShortcutId::IndentWorkItem => self.indent_work_item = combo,
            ShortcutId::OutdentWorkItem => self.outdent_work_item = combo,
            ShortcutId::ShowOutlineShortcuts => self.show_outline_shortcuts = combo,
            ShortcutId::OpenLinear => self.open_linear = combo,
            ShortcutId::OpenTeamTab => self.open_team_tab = combo,
            ShortcutId::OpenTeamTabFromPanel => self.open_team_tab_from_panel = combo,
            ShortcutId::PopOutSurface => self.pop_out_surface = combo,
            ShortcutId::Outline(index) => {
                if let Some(key) = crate::shell::work_outline_ui::OutlineNavKey::ALL.get(index) {
                    self.outline_shortcuts.insert(
                        key.storage_key().to_string(),
                        if combo.is_empty() {
                            Vec::new()
                        } else {
                            vec![combo]
                        },
                    );
                }
            }
            ShortcutId::JumpSession(slot) => {
                if slot < JUMP_SLOTS {
                    if self.jump_session.len() < JUMP_SLOTS {
                        self.heal_jump_slots();
                    }
                    self.jump_session[slot] = combo;
                }
            }
        }
    }

    pub(super) fn outline_combos(&self, index: usize) -> Vec<&str> {
        use crate::shell::work_outline_ui::{OUTLINE_NAVIGATION_KEYS, OutlineNavKey};
        let Some(key) = OutlineNavKey::ALL.get(index) else {
            return Vec::new();
        };
        if let Some(combos) = self.outline_shortcuts.get(key.storage_key()) {
            return combos.iter().map(String::as_str).collect();
        }
        OUTLINE_NAVIGATION_KEYS
            .iter()
            .filter_map(|(combo, candidate)| (candidate == key).then_some(*combo))
            .collect()
    }

    pub fn combos(&self, id: ShortcutId) -> Vec<&str> {
        match id {
            ShortcutId::Outline(index) => self.outline_combos(index),
            _ => {
                let combo = self.get(id);
                if combo.is_empty() {
                    Vec::new()
                } else {
                    vec![combo]
                }
            }
        }
    }

    pub fn reset(&mut self, id: ShortcutId) {
        if let ShortcutId::Outline(index) = id {
            if let Some(key) = crate::shell::work_outline_ui::OutlineNavKey::ALL.get(index) {
                self.outline_shortcuts.remove(key.storage_key());
            }
        } else {
            self.set(id, id.default_combo().to_string());
        }
    }

    /// Restore the jump list to exactly [`JUMP_SLOTS`] entries: a hand-edited
    /// or older file may carry a short, long or absent list. Surviving entries
    /// keep their slot; missing ones take the default.
    pub fn heal_jump_slots(&mut self) {
        self.jump_session.truncate(JUMP_SLOTS);
        while self.jump_session.len() < JUMP_SLOTS {
            self.jump_session
                .push(JUMP_DEFAULTS[self.jump_session.len()].to_string());
        }
    }

    /// Cmd/Ctrl+Enter belongs to the composer on every send mode. Older
    /// settings could assign it to an app shortcut while plain Enter was the
    /// configured sender; restore only those newly-conflicting rows to their
    /// defaults and preserve every unrelated customization.
    pub(super) fn heal_reserved_composer_shortcuts(&mut self) {
        for id in ShortcutId::ALL {
            if !matches!(
                id,
                ShortcutId::Outline(_) | ShortcutId::OpenTeamTabFromPanel
            ) && self.get(id) == "mod-enter"
            {
                self.reset(id);
            }
        }
    }
}

/// Build a combo string from a recorded keystroke. The primary modifier
/// (cmd on macOS, ctrl elsewhere) becomes "mod"; bare modifier presses record
/// nothing.
pub fn combo_from_keystroke(
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    combo_from_keystroke_on(cfg!(target_os = "macos"), ctrl, alt, shift, cmd, key)
}

/// [`combo_from_keystroke`] for an explicit platform — the ctrl spelling is
/// platform-dependent, so both paths need to be exercisable from one machine.
pub fn combo_from_keystroke_on(
    mac: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    let key = key.trim().to_lowercase();
    if key.is_empty()
        || matches!(
            key.as_str(),
            "ctrl" | "control" | "alt" | "shift" | "cmd" | "platform" | "fn"
        )
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    // On macOS ctrl stays its own modifier rather than folding into "mod":
    // re-recording Ctrl+Tab as Cmd+Tab would hand the combo to the OS app
    // switcher, which never delivers it to the window.
    let ctrl_is_primary = ctrl && !mac;
    if cmd || ctrl_is_primary {
        parts.push("mod");
    }
    if ctrl && !ctrl_is_primary {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

/// Outline and side-panel shortcuts cannot be active at the same time.
pub fn shortcut_contexts_overlap(left: ShortcutId, right: ShortcutId) -> bool {
    let review = |id| matches!(id, ShortcutId::ReviewNextStep | ShortcutId::ReviewPreviousStep | ShortcutId::ReviewNextStepArrow | ShortcutId::ReviewPreviousStepArrow | ShortcutId::ReviewNextHunk | ShortcutId::ReviewPreviousHunk | ShortcutId::ReviewMarkReviewed | ShortcutId::ReviewToggleMode | ShortcutId::ExplainDiff);
    if (review(left) && matches!(right, ShortcutId::Outline(_) | ShortcutId::OpenTeamTabFromPanel)) || (review(right) && matches!(left, ShortcutId::Outline(_) | ShortcutId::OpenTeamTabFromPanel)) { return false; }
    !matches!(
        (left, right),
        (ShortcutId::AddCodeToChat, ShortcutId::OpenOutline)
            | (ShortcutId::OpenOutline, ShortcutId::AddCodeToChat)
            | (ShortcutId::Outline(_), ShortcutId::OpenTeamTabFromPanel)
            | (ShortcutId::OpenTeamTabFromPanel, ShortcutId::Outline(_))
    )
}

/// Compare physical keystrokes so aliases such as ctrl and mod conflict off macOS.
pub fn shortcut_combos_conflict(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    match (
        gpui::Keystroke::parse(&platform_combo(left)),
        gpui::Keystroke::parse(&platform_combo(right)),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Shortcut ids whose combos collide with another shortcut (conflict detection).
pub fn conflicted_shortcuts(keymap: &KeymapConfig) -> Vec<ShortcutId> {
    ShortcutId::ALL
        .into_iter()
        .filter(|&id| {
            id.available()
                && keymap.combos(id).into_iter().any(|combo| {
                    ShortcutId::ALL.into_iter().any(|other| {
                        other.available()
                            && other != id
                            && shortcut_contexts_overlap(id, other)
                            && keymap
                                .combos(other)
                                .into_iter()
                                .any(|other_combo| shortcut_combos_conflict(combo, other_combo))
                    })
                })
        })
        .collect()
}

/// The modifiers a stored combo carries, as `(mod, alt, shift)`. Everything
/// before the final segment is a modifier; the final segment is the key.
pub fn combo_modifiers(combo: &str) -> (bool, bool, bool) {
    let mut parts: Vec<&str> = combo.split('-').collect();
    parts.pop();
    (
        parts.contains(&"mod"),
        parts.contains(&"alt"),
        parts.contains(&"shift"),
    )
}

/// Whether the sidebar should show its jump hints for the currently held
/// modifiers (t3code `shouldShowThreadJumpHintsForModifiers`). The held set
/// must match a jump combo EXACTLY, so adding Shift or Alt hides the hints and
/// a chord like Cmd+Shift+4 never flashes the overlay. `primary` is the held
/// "mod" key — cmd on macOS, ctrl elsewhere.
///
/// A jump combo with no modifiers at all never shows hints: it would otherwise
/// match the resting state and pin the overlay open. Pure.
pub fn jump_hints_visible(keymap: &KeymapConfig, primary: bool, alt: bool, shift: bool) -> bool {
    if !(primary || alt || shift) {
        return false;
    }
    ShortcutId::ALL
        .into_iter()
        .filter(|id| id.jump_slot().is_some())
        .any(|id| combo_modifiers(keymap.get(id)) == (primary, alt, shift))
}

/// Cmd on macOS and Ctrl elsewhere reveals the composer's modified-submit
/// hint only while that modifier is held by itself.
pub fn modifier_send_hint_visible(primary: bool, alt: bool, shift: bool) -> bool {
    primary && !alt && !shift
}

/// Translate a stored combo into a bindable keystroke for this platform.
pub fn platform_combo(combo: &str) -> String {
    platform_combo_on(cfg!(target_os = "macos"), combo)
}

/// [`platform_combo`] for an explicit platform (see [`combo_from_keystroke_on`]).
pub fn platform_combo_on(mac: bool, combo: &str) -> String {
    let primary = if mac { "cmd" } else { "ctrl" };
    combo
        .split('-')
        .map(|part| if part == "mod" { primary } else { part })
        .collect::<Vec<_>>()
        .join("-")
}

/// Human-readable combo for the shortcuts table ("mod-s" → "Cmd+S"/"Ctrl+S").
pub fn display_combo(combo: &str) -> String {
    display_combo_on(cfg!(target_os = "macos"), combo)
}

/// [`display_combo`] for an explicit platform (see [`combo_from_keystroke_on`]).
pub fn display_combo_on(mac: bool, combo: &str) -> String {
    let minus = combo.ends_with("--");
    let combo = if minus { &combo[..combo.len() - 1] } else { combo };
    let result = combo
        .split('-')
        .map(|part| match part {
            "mod" => if mac { "Cmd" } else { "Ctrl" }.to_string(),
            "alt" => if mac { "Opt" } else { "Alt" }.to_string(),
            "shift" => "Shift".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+");
    if minus { format!("{result}-") } else { result }
}

/// Compact combo for badge surfaces (the sidebar jump hints): macOS spells
/// the modifiers as their key glyphs in canonical ⌃⌥⇧⌘ order and drops the
/// separators ("⌘1", "⇧⌘A") — the form the model picker's ⌘N chips already
/// use — while other platforms keep the textual [`display_combo`] ("Ctrl+1").
pub fn badge_combo(combo: &str) -> String {
    badge_combo_on(cfg!(target_os = "macos"), combo)
}

/// [`badge_combo`] for an explicit platform (see [`combo_from_keystroke_on`]).
pub fn badge_combo_on(mac: bool, combo: &str) -> String {
    if !mac {
        return display_combo_on(false, combo);
    }
    let mut parts: Vec<&str> = combo.split('-').collect();
    let key = if combo.ends_with("--") { parts.pop(); parts.pop(); "-" } else { parts.pop().unwrap_or("") };
    let mut out = String::new();
    for glyph in ["ctrl", "alt", "shift", "mod"]
        .iter()
        .zip(['⌃', '⌥', '⇧', '⌘'])
        .filter_map(|(name, glyph)| parts.contains(name).then_some(glyph))
    {
        out.push(glyph);
    }
    let mut chars = key.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}
