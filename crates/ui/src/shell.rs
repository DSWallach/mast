//! The app shell (zeron `__root.tsx`): sidebar column + main panel + optional
//! right "Changes" pane, plus the boot splash and the connection gate.
//!
//! Layout is zeron's: collapsible drag-resizable sidebar (224–400px, default
//! 256) with a 200ms ease-out width transition; main panel with an h-11 header,
//! content outlet, and a reserved h-6 status strip so later content never
//! shifts; right pane scaffold (360px floor, default 520), hidden by default.
//! Widths/collapsed state persist to `ui-settings.json` (debounced).
//!
//! Resize handles use gpui's drag-and-drop pattern (an `on_drag` with an empty
//! ghost view + `on_drag_move::<Marker>` on the root), the same idiom as Zed's
//! dock. Double-clicking a handle resets that pane to its default width.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, Empty, Entity, FocusHandle, Focusable as _,
    IntoElement, KeyBinding, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, Subscription, Task, Window,
    WindowControlArea, actions, div, prelude::*, px,
};

use gpui_tokio::Tokio;
use mast_engine::{InstanceLock, pr_ticket_cache::ChatLinkStatus};
use mast_proto::{AuthState, WorkspaceScope};
use mast_rpc::methods;

use crate::changes::{Changes, ChangesEvent};
#[cfg(test)]
use crate::components::shell_parts::chat_row_height;
use crate::components::shell_parts::{
    header_icon_button, linux_caption_button, nav_history_button, sidebar_faded_label,
    sidebar_row_height, window_control_button, windows_caption_button,
};
use crate::composer::{Composer, ComposerEvent, ComposerInput, ComposerInputEvent};
use crate::files::{FilesCloseDisposition, FilesEvent, FilesSurface, WorkspacePathDrag};
use crate::icons::{self, icon};
use crate::loaders;
use crate::motion::{self, AnimationExt as _, MotionSpec, RESIZE, SPLASH_OUT, TAB_SLIDE};
use crate::popover::{self, Loadable};
use crate::rail;
use crate::settings::accounts::AccountsPage;
use crate::settings::appearance::{AppearancePage, AppearanceSettingsEvent};
use crate::settings::archived::ArchivedPage;
use crate::settings::devices::DevicesPage;
use crate::settings::files::{FilesSettingsEvent, FilesSettingsPage};
use crate::settings::harnesses::HarnessesPage;
use crate::settings::notifications::{NotificationsEvent, NotificationsPage};
use crate::settings::shortcuts::{ShortcutsEvent, ShortcutsPage};
use crate::settings::{
    self, CHAT_PANEL_MIN, ComposerSendBehavior, JUMP_SLOTS, KeymapConfig, RIGHT_PANE_DEFAULT,
    RIGHT_PANE_MIN, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, SavePolicy, ShortcutId,
    SidebarOrganization, SidebarSort, TERMINAL_DEFAULT_HEIGHT, TERMINAL_MAX_VH,
    TERMINAL_MIN_HEIGHT, UiSettings, badge_combo, jump_hints_visible, modifier_send_hint_visible,
    platform_combo, sidebar_pin_profile_key,
};
use crate::state::{
    AppState, ConnectionStatus, EngineBootConfig, EngineMode, GatePhase, Indicator, OrgRow,
    format_time_ago, org_name_valid, parse_orgs, sort_memberships,
};
use crate::terminal::panel::{TerminalPanel, ToggleTerminal, clamp_terminal_height};
use crate::theme::Theme;
use crate::transcript::{self, Transcript, TranscriptEvent};
use crate::workspace_links::resolve_workspace_file_link;

mod code_ref;
mod actions_ui;
mod account_synchronization;
pub(crate) mod artifacts_ui;
#[cfg(test)]
mod chat_item_tests;
#[cfg(test)]
mod admin_tests;
mod chat_item_ui;
mod closeout_ui;
mod command_palette;
mod workspace_search;
mod files_panel;
mod github_ui;
mod hand_off_ui;
mod instructions_ui;
mod linear_ui;
mod key_routing;
mod main_content;
mod outside_file_ui;
mod overlays;
mod pane_geometry;
#[cfg(test)]
mod perf_tests;
mod pr_review_ui;
mod branch_review_ui;
mod review_ui;
mod right_tab_strip;
mod right_surfaces;
mod project_icon;
mod required_actions_ui;
mod menu_bar_ui;
mod running_ui;
mod sidebar_pins;
mod sidebar_resort;
mod sidebar_chat_rows;
use sidebar_chat_rows::{ChatRowArgs, ChatRowView, SidebarPane};
#[cfg(test)]
pub(crate) use sidebar_chat_rows::perf_counts;
mod sidebar_sections;
pub(crate) mod spaces;
mod start_chat_ui;
mod tabs;
mod titlebar;
mod team_ui;
mod wake_ui;
mod admin_ui;
mod work_outline_menu_ui;
pub(crate) mod work_outline_ui;
pub(crate) use tabs::chat_identity_for;

use spaces::{AddSpaceFlow, RenameSpaceDialog};

const CHAT_HEADER_LINK_REFRESH: Duration = Duration::from_secs(15);
const CHAT_HEADER_METADATA_CLEARANCE: f32 = 52.0;

struct ChatHeaderLinkCache {
    checkout: Option<(String, String, Option<String>)>,
    status: Option<ChatLinkStatus>,
    fetched_at: Instant,
}

actions!(
    shell,
    [
        SaveFile,
        ToggleSidebar,
        ToggleChanges,
        ToggleFiles,
        OpenMyPrs,
        QuickOpen,
        AddCodeToChat,
        SearchWorkspaceContent,
        GoToLine,
        AddSpacePalette,
        ToggleCommandPalette,
        OpenModelPicker,
        NewSession,
        OpenSettings,
        NextSession,
        PrevSession,
        ArchiveSession,
        OpenOverview,
        FocusOverviewSearch,
        OpenPrReviewChats,
        OpenItemTab,
        OpenTeamTab,
        OpenTeamTabFromPanel,
        ToggleItemCard,
        ToggleRunningChats
    ]
);

/// Find (⌘F / Ctrl+F): focus the Overview's search field and select its text.
/// Bound with no context so it works from an unfocused Overview; deeper
/// contexts that own Find themselves (the file editor's `Search`) still win,
/// and every other route ignores it.
pub(crate) const FOCUS_OVERVIEW_SEARCH_COMBO: &str = "mod-f";
pub(crate) const OPEN_PR_REVIEW_CHATS_COMBO: &str = "mod-shift-p";
/// Open the side panel's Item tab (fixed, listed in the `?` reference).
pub(crate) const OPEN_ITEM_TAB_COMBO: &str = "mod-alt-;";
/// Open the side panel's Team tab. ⌘T is the browser's New tab inside the side
/// panel (`Browser` context), so the Team tab takes ⌥⌘T, next to ⌥⌘; for Item.
#[cfg(test)]
pub(crate) const OPEN_TEAM_TAB_COMBO: &str = "mod-alt-t";
/// Pin the header's context card open / release it.
pub(crate) const TOGGLE_ITEM_CARD_COMBO: &str = "mod-alt-i";

/// Restore a default focus only after an in-flight handoff has had a frame to
/// claim the window. A synchronous focus-lost fallback can otherwise steal
/// focus from controls that are mounting in response to the same input event.
#[cfg(test)]
pub(crate) fn restore_focus_if_empty_on_next_frame<T: 'static>(
    focus: FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    window.on_next_frame(move |window, cx| {
        if window.focused(cx).is_none() {
            window.focus(&focus, cx);
        }
    });
    cx.notify();
}

/// Check the completed dispatch tree, not just the lifetime of the focused
/// handle: a hidden editor can stay alive after its element has unmounted.
pub(crate) fn restore_mounted_focus(
    root: &FocusHandle,
    preferred: &FocusHandle,
    unfocused: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let preferred_mounted = root.contains(preferred, window);
    if !root.contains_focused(window, cx) || (root.is_focused(window) && preferred_mounted) {
        // Explicit blur keeps shortcuts active without returning the caret to
        // an input. The root remains the temporary fallback for stale handles.
        let target = if window.focused(cx).is_none() {
            unfocused
        } else if preferred_mounted {
            preferred
        } else {
            root
        };
        window.focus(target, cx);
    }
}

#[derive(Clone, Copy)]
enum ChatMenuPage {
    Root,
    Copy,
}

#[derive(Clone)]
struct ChatMenuState {
    chat_id: String,
    position: Point<Pixels>,
    page: ChatMenuPage,
    /// The chat's cwd cheaply looks like a linked worktree — offer "Close out
    /// worktree…" and let the engine plan make the authoritative eligibility
    /// decision. Checked once at open, not per frame.
    closeable: bool,
}

/// Interruptible height tween for the sidebar's device/archive disclosures.
/// The rendered element owns the frame clock; this state preserves the current
/// interpolated height when a second click reverses an in-flight transition.
#[derive(Clone, Copy)]
pub(super) struct SidebarDisclosureMotion {
    pub(super) epoch: u64,
    pub(super) from: f32,
    pub(super) to: f32,
    started: std::time::Instant,
}

impl SidebarDisclosureMotion {
    fn new(epoch: u64, from: f32, to: f32) -> Self {
        Self {
            epoch,
            from,
            to,
            started: std::time::Instant::now(),
        }
    }

    fn current(self) -> f32 {
        let total = motion::COLLAPSE.total().as_secs_f32();
        let raw = if total > 0.0 {
            self.started.elapsed().as_secs_f32() / total
        } else {
            1.0
        };
        motion::lerp(self.from, self.to, motion::COLLAPSE.progress(raw))
    }

    fn animating(self) -> bool {
        self.started.elapsed() < motion::COLLAPSE.total() + spaces::SIDEBAR_DISCLOSURE_TWEEN_GRACE
    }
}

/// Vertical pane resize hitboxes yield the global titlebar. Keeping this in
/// the shared constructor makes left/right seams mirror each other and avoids
/// relying on paint order when chrome crosses an animated pane boundary.
const PANE_RESIZE_HITBOX_HALF_WIDTH: f32 = 10.0;
const PANE_RESIZE_HITBOX_TOP: f32 = Theme::TITLEBAR_HEIGHT;
/// Corner radius of the floating Linux CSD window (macOS gets its native
/// curve from the platform; maximized/tiled Linux windows go square).
/// Chrome layers that paint full-bleed at a window edge round their own
/// backgrounds with it — see [`Shell::window_corner_radius`].
pub(crate) const LINUX_WINDOW_CORNER_RADIUS: f32 = 10.0;
const TERMINAL_RESIZE_HITBOX_HEIGHT: f32 = 10.0;

fn stable_panel_content_width(target: f32, transition: Option<(f32, f32)>) -> f32 {
    transition.map(|(from, to)| from.max(to)).unwrap_or(target)
}

fn right_panel_content_width(
    target: f32,
    transition: Option<(f32, f32)>,
    takeover_width: Option<f32>,
) -> f32 {
    takeover_width.unwrap_or_else(|| stable_panel_content_width(target, transition))
}

fn conversation_width(viewport: f32, sidebar: f32, right: f32) -> f32 {
    (viewport - sidebar - right).max(0.0)
}

fn titlebar_new_session_alpha(is_chat_route: bool, has_selected_chat: bool) -> f32 {
    if is_chat_route && has_selected_chat {
        1.0
    } else {
        0.0
    }
}

/// Open the session at `slot` (zero-based) of the sidebar's active list. One
/// action carrying the slot, rather than nine near-identical action types.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = shell, no_json)]
pub struct JumpSession(pub usize);

// ---------------------------------------------------------------------------
// Traffic-light-aware titlebar layout (feature-inventory §1.1)
// ---------------------------------------------------------------------------

/// Where the top-left window-control cluster starts, in px from the window's
/// left edge (zeron window-controls.tsx: `left: fullscreen ? 12 : 88`). The
/// frameless hiddenInset chrome puts the macOS traffic lights at {14,15};
/// fullscreen hides them and the cluster reclaims the inset.
pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen { 12.0 } else { 88.0 }
}

/// Width of the spacer ahead of the control cluster for a strip that already
/// carries `container_pad` px of its own left padding. macOS only — on
/// Linux/Windows there are no traffic lights and the cluster hugs the edge.
pub fn titlebar_spacer_width(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    if !is_macos {
        return 0.0;
    }
    (titlebar_cluster_start(fullscreen) - container_pad).max(0.0)
}

/// Within-group rhythm for Back/Forward.
pub const TITLEBAR_CONTROL_GAP: f32 = 2.0;
/// Structural separation between titlebar groups: sidebar, navigation,
/// transcript identity, and trailing actions.
pub const TITLEBAR_GROUP_GAP: f32 = Theme::SPACE_SM;
/// Breathing room between the navigation cluster and transcript identity.
pub const TITLEBAR_IDENTITY_GAP: f32 = Theme::SPACE_MD;
/// A 28px action centered in the 38px titlebar with its 2px downward optical
/// shift lands 6px from the top; use the same inset at the trailing edge.
pub const TITLEBAR_ACTION_EDGE_INSET: f32 = 6.0;
/// Width of the persistent top-left button cluster itself: a 24px sidebar
/// trigger, an 8px group gap, then two 24px history buttons on a 2px rhythm.
pub const CLUSTER_BUTTONS_WIDTH: f32 = 24.0 * 3.0 + TITLEBAR_GROUP_GAP + TITLEBAR_CONTROL_GAP;
/// Extra width consumed when the collapsed-sidebar New Session action joins
/// the left controls as its own group.
pub const TITLEBAR_ACTION_SLOT_WIDTH: f32 = TITLEBAR_GROUP_GAP + 24.0;
/// The `+` and running-chats lamp slots that follow the cluster buttons. Both
/// are always reserved (the `+` slot stays empty off the chat route) so the
/// lamp never moves and no title sits under either.
pub const TITLEBAR_TRAILING_SLOTS_WIDTH: f32 = TITLEBAR_ACTION_SLOT_WIDTH * 2.0;
/// Horizontal inset owned by the titlebar control row itself. Keep this value
/// paired with [`Self::titlebar_spacer`]: using a different number for the
/// spacer shifts every control while leaving the declared cluster geometry
/// unchanged.
const TITLEBAR_CLUSTER_PAD: f32 = 10.0;

/// Width of a row of `count` Linux caption buttons, drawn at the cluster's
/// own 24px-button / 2px-gap rhythm.
pub fn caption_buttons_width(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * 24.0 + (count as f32 - 1.0) * 2.0
}

/// Where the cluster's first button starts, from the window's left edge.
/// `linux_left_captions` is the number of caption buttons zeron draws at the
/// top-left on Linux (GNOME `close:…` layouts) — the app cluster follows them
/// at the shared 2px rhythm.
pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool, linux_left_captions: usize) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else if linux_left_captions > 0 {
        10.0 + caption_buttons_width(linux_left_captions) + 2.0
    } else {
        10.0
    }
}

/// Left clearance a full-bleed header (collapsed sidebar) needs so its content
/// starts past the overlay cluster, given the header's own `container_pad`.
pub fn cluster_clearance(
    is_macos: bool,
    fullscreen: bool,
    linux_left_captions: usize,
    container_pad: f32,
) -> f32 {
    (cluster_buttons_start(is_macos, fullscreen, linux_left_captions)
        + CLUSTER_BUTTONS_WIDTH
        + TITLEBAR_TRAILING_SLOTS_WIDTH
        + 8.0
        - container_pad)
        .max(0.0)
}

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
        (ShortcutId::AddCodeToChat, Box::new(AddCodeToChat) as Box<dyn Action>),
        (ShortcutId::SearchWorkspaceContent, Box::new(SearchWorkspaceContent) as Box<dyn Action>),
        (ShortcutId::GoToLine, Box::new(GoToLine) as Box<dyn Action>),
        (ShortcutId::ReplaceFile, Box::new(gpui_base::input::Replace) as Box<dyn Action>),
    ] {
        let combo = keymap.get(id);
        if combo.is_empty() { continue; }
        let context = if id == ShortcutId::AddCodeToChat { Some("Files") } else { (id == ShortcutId::ReplaceFile).then_some("Input") };
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

/// The settings sections (feature-inventory §1.5 routes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Devices,
    /// Which harnesses the composer offers (enable/disable toggles).
    Harnesses,
    /// Per-provider CLI accounts (login, usage) — labeled "Accounts".
    Agents,
    Appearance,
    Developer,
    Files,
    Notifications,
    /// Linear integration: API key and connection status.
    Linear,
    Shortcuts,
    Appshots,
    Archived,
    /// Export and import a Space's instructions and templates.
    WorkflowPack,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 12] = [
        SettingsSection::Devices,
        SettingsSection::Harnesses,
        SettingsSection::Agents,
        SettingsSection::Appearance,
        SettingsSection::Developer,
        SettingsSection::Files,
        SettingsSection::Notifications,
        SettingsSection::Linear,
        SettingsSection::Shortcuts,
        SettingsSection::Appshots,
        SettingsSection::Archived,
        SettingsSection::WorkflowPack,
    ];

    /// Sidebar + header label (zeron settings-sidebar.tsx SECTIONS / __root.tsx
    /// `settingsTitle` — the same strings in both places).
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::Devices => "Devices",
            SettingsSection::Harnesses => "Agents",
            SettingsSection::Agents => "Accounts",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Developer => "Developer",
            SettingsSection::Files => "Files",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Linear => "Linear",
            SettingsSection::Shortcuts => "Shortcuts",
            SettingsSection::Appshots => "Appshots",
            SettingsSection::Archived => "Archived sessions",
            SettingsSection::WorkflowPack => "Workflow pack",
        }
    }
}

/// What the main outlet shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Settings(SettingsSection),
    /// Multi-chat overview (ticket 002): every chat at once, not one at a time.
    Overview,
    /// The PR page: dashboards plus the chats launched to review PRs.
    PrReviewChats,
    Review,
}

/// Maximum width the right pane may occupy while retaining the conversation
/// floor. On unusually small windows this deliberately falls below the right
/// pane's preferred minimum: the chat remains usable and the side surface
/// yields the scarce space.
fn right_pane_max_width(viewport: f32, sidebar: f32, chat_floor: f32) -> f32 {
    (viewport - sidebar - chat_floor).max(0.0)
}

/// Width used by right-pane takeover. Unlike manual resizing, takeover is
/// intentionally allowed to consume the conversation column completely.
fn right_pane_takeover_width(viewport: f32, sidebar: f32) -> f32 {
    (viewport - sidebar).max(0.0)
}

/// One right-pane surface tab: a workspace browser, an individual workspace
/// file editor, a Git diff or history page, an embedded terminal, or a
/// subagent transcript. `Picker` is the empty surface chooser.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RightSurface {
    #[default]
    Picker,
    File(u64),
    Browser(u64),
    Diff(u64),
    Review,
    Terminal(u64),
    /// A subagent's transcript, read-only (per-subagent viz) — the handle
    /// keys [`Shell::subagent_tabs`].
    Subagent(u64),
    /// A planning artifact (`mast-artifact:` links) — keys
    /// `Shell::artifact_tabs`.
    Artifact(u64),
    /// The sticky "Item" tab: what the chat is for (work item, instructions,
    /// where it runs, model, account, open asks). Always first in the strip,
    /// never stored in `right_tabs`, never closed.
    Item,
    /// The sticky "Team" tab, right after Item: one row per worker of an
    /// orchestrating chat. Present only while the chat has workers; never
    /// stored in `right_tabs`, never closed.
    Team,
}

fn push_unique_right_surface(tabs: &mut Vec<RightSurface>, surface: RightSurface) -> bool {
    if tabs.contains(&surface) {
        false
    } else {
        tabs.push(surface);
        true
    }
}

fn workspace_file_title(path: &str) -> SharedString {
    path.rsplit('/').next().unwrap_or(path).to_string().into()
}

/// Per-chat panel open flags (zeron parity: `sessionPanels` — the terminal and
/// changes panels open *per session*, in memory only; heights and every other
/// persisted setting stay global).
///
/// Everything defaults CLOSED — the right pane included (user request,
/// revising the earlier default-open: it popped open on every session you
/// visited). Opening is an explicit act, remembered per chat for the rest of
/// the app run; a fresh open with no surface tabs lands on the picker.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChatPanels {
    /// The explorer portion of the right pane is docked.
    pub files_open: bool,
    pub terminal_open: bool,
    /// The surface host portion of the right pane is visible (historically
    /// the Changes pane). The pane itself shows when either portion does.
    pub changes_open: bool,
    /// Which surface tab renders; validated against the live tab list each
    /// frame (a closed tab falls back gracefully).
    pub right_active: RightSurface,
}

/// The session-scoped panel map. Keys are chat ids; the new-chat canvas uses
/// the empty key. Not persisted — a fresh app starts with everything closed.
#[derive(Debug, Default)]
pub struct SessionPanels {
    map: std::collections::HashMap<String, ChatPanels>,
}

impl SessionPanels {
    pub fn get(&self, key: &str) -> ChatPanels {
        self.map.get(key).copied().unwrap_or_default()
    }

    /// Flip the terminal flag for `key`; returns the new value.
    pub fn toggle_terminal(&mut self, key: &str) -> bool {
        let entry = self.map.entry(key.to_string()).or_default();
        entry.terminal_open = !entry.terminal_open;
        entry.terminal_open
    }

    /// Flip the changes flag for `key`; returns the new value.
    pub fn toggle_changes(&mut self, key: &str) -> bool {
        let entry = self.map.entry(key.to_string()).or_default();
        entry.changes_open = !entry.changes_open;
        entry.changes_open
    }

    /// Mutate `key`'s flags in place (right-pane surface bookkeeping).
    pub fn update(&mut self, key: &str, f: impl FnOnce(&mut ChatPanels)) {
        f(self.map.entry(key.to_string()).or_default());
    }
}

/// One route-history entry (zeron parity: the renderer's TanStack memory
/// history — every route the user visited, browser-style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    /// A chat route; the id of the selected chat ("" = the new-chat canvas).
    Chat(String),
    Settings(SettingsSection),
    Overview,
    PrReviewChats,
}

/// Browser-style navigation history for the titlebar back/forward buttons
/// (zeron window-controls.tsx semantics): every route change pushes an entry;
/// Back/Forward walk the stack without changing it; pushing while behind the
/// tip truncates the entries ahead (a new branch, exactly like a browser).
#[derive(Debug)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
}

impl NavHistory {
    pub fn new(initial: NavEntry) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    pub fn current(&self) -> &NavEntry {
        &self.entries[self.index]
    }

    /// Record a route change. Re-navigating to the current route is a no-op
    /// (selecting the already-selected chat never happened as a navigation);
    /// otherwise any forward branch is truncated and the entry appended.
    pub fn push(&mut self, entry: NavEntry) {
        if *self.current() == entry {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(entry);
        self.index += 1;
    }

    /// Swap the current entry in place without growing the stack — the native
    /// equivalent of a `replace: true` navigation (zeron's boot redirect from
    /// `/` into the last-used chat leaves no dead Back target behind).
    pub fn replace(&mut self, entry: NavEntry) {
        self.entries[self.index] = entry;
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    /// Memory history keeps every entry, so "behind the last entry" is exactly
    /// "can go forward" (zeron window-controls.tsx).
    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<NavEntry> {
        if !self.can_back() {
            return None;
        }
        self.index -= 1;
        Some(self.current().clone())
    }

    pub fn forward(&mut self) -> Option<NavEntry> {
        if !self.can_forward() {
            return None;
        }
        self.index += 1;
        Some(self.current().clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Sidebar resort glide (feature-inventory §1.6): 260ms
/// `cubic-bezier(0.22,1,0.36,1)` per-row translate, the View Transitions
/// equivalent.
pub const RESORT: MotionSpec = MotionSpec::new(260, motion::EASE_RESORT);

/// FLIP diff for a keyed list: given the previously rendered order and the new
/// order (key + row height), return each surviving key's paint-only start
/// offset `old_y - new_y` (only keys whose position actually moved). `gap` is
/// the flex gap between rows. Pure — drives the sidebar resort glide.
pub fn resort_offsets(
    old: &[(String, f32)],
    new: &[(String, f32)],
    gap: f32,
) -> std::collections::HashMap<String, f32> {
    let mut old_y = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in old {
        old_y.insert(key.as_str(), y);
        y += height + gap;
    }
    let mut offsets = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in new {
        if let Some(prev) = old_y.get(key.as_str()) {
            let dy = prev - y;
            if dy.abs() > 0.5 {
                offsets.insert(key.clone(), dy);
            }
        }
        y += height + gap;
    }
    offsets
}

/// Height changes do not constitute a list reorder. In particular, sidebar
/// disclosures animate their own height and must not also trigger FLIP offsets
/// on every following keyed section.
fn sidebar_key_order_changed(old: &[(String, f32)], new: &[(String, f32)]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|((old_key, _), (new_key, _))| old_key != new_key)
}

/// Flex gap between sidebar list items.
const SIDEBAR_LIST_GAP: f32 = 2.0;
/// Fixed vertical slot occupied by one active sidebar card.
#[cfg(test)]
const SIDEBAR_SESSION_SLOT: f32 = 61.0 + SIDEBAR_LIST_GAP;
const SIDEBAR_DRAG_SCROLL_BAND: f32 = 48.0;
const SIDEBAR_DRAG_SCROLL_MAX: f32 = 12.0;
const SIDEBAR_DRAG_SCROLL_FRAME_MS: u64 = 16;
const SIDEBAR_LIST_PAD_TOP: f32 = 4.0;

/// Active and archived sessions share harness/title geometry.
const SIDEBAR_ACTIVE_HARNESS_ICON_SIZE: f32 = 13.0;
const SIDEBAR_ACTIVE_HARNESS_TITLE_GAP: f32 = Theme::SPACE_SM;

/// Ramp height of the sidebar's scroll-edge fade (the gpui
/// [`gpui::EdgeFade`] scope — per-primitive, so text fades per glyph).
const SIDEBAR_GLASS_FADE_BAND: f32 = 24.0;

/// New-thread controls float over the tail of a top-anchored image hero. The
/// hero reaches below the composer, giving its lower mask room to dissolve
/// gradually into the otherwise empty lower canvas.
const NEW_THREAD_BACKGROUND_FROSTED_OPACITY: f32 = 0.84;
const NEW_THREAD_BACKGROUND_VIEWPORT_RATIO: f32 = 0.72;
const NEW_THREAD_BACKGROUND_MAX_HEIGHT: f32 = 760.0;

/// Drag marker for the sidebar resize handle.
struct SidebarResize;
/// Drag marker for the right-pane resize handle.
struct RightPaneResize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneResizeKind {
    Sidebar,
    Right,
    Files,
    Terminal,
}

/// Resolve one pointer sample while keeping the persisted width legal. The
/// edge is latched by the caller, so a held pointer produces one nudge rather
/// than restarting the animation for every drag event.
fn sidebar_drag_sample(
    pointer_x: f32,
    latched_edge: Option<motion::ResizeEdge>,
    reduced_motion: bool,
) -> motion::ResizeDragSample {
    motion::resize_drag_sample(
        pointer_x,
        SIDEBAR_MIN,
        SIDEBAR_MAX,
        latched_edge,
        reduced_motion,
    )
}

/// `width` clamped to what a `viewport`-wide window can give the sidebar while
/// the conversation keeps `CHAT_PANEL_MIN`; never below `SIDEBAR_MIN`.
fn sidebar_width_for_viewport(width: f32, viewport: f32) -> f32 {
    width.min((viewport - CHAT_PANEL_MIN).max(SIDEBAR_MIN))
}

/// The dragged surface-tab payload (strip reorder).
struct RightTabDrag {
    panel_key: String,
    from: usize,
    title: SharedString,
    workspace_path: Option<WorkspacePathDrag>,
}

/// Live drag-over state for the surface-tab strip — the terminal drawer's
/// [`crate::terminal::panel`] DragState, ported: `epoch` keys the 150ms
/// slide-animation restarts as the hovered slot changes.
struct RightTabDragState {
    from: usize,
    over: usize,
    epoch: usize,
    prev_over: usize,
}

/// Sidebar-only drag payload. Regular sessions never acquire a manual order.
#[derive(Clone, PartialEq)]
struct SidebarSessionDrag {
    chat_id: String,
    visible_ids: std::sync::Arc<Vec<String>>,
    filter: Option<String>,
    profile_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarSessionDrop {
    Pinned(usize),
    Regular,
    Section(String),
}

struct SidebarSessionTransfer {
    payload: SidebarSessionDrag,
    origin: std::rc::Rc<std::cell::Cell<Point<Pixels>>>,
    cursor_offset: Point<Pixels>,
    pointer: Point<Pixels>,
    viewport: Option<gpui::Bounds<Pixels>>,
    slide: SidebarSessionSlide,
    preview: Option<SidebarSessionGap>,
    source_group: String,
    source_index: usize,
    row_height: f32,
    source_collapse: SidebarSessionSlide,
    collapsed_height: f32,
    section_gaps: std::collections::HashMap<String, SidebarSessionSlide>,
    siblings: std::collections::HashMap<String, SidebarSessionSlide>,
}

#[derive(Clone)]
struct SidebarSessionGap {
    group: String,
    index: usize,
    pinned: bool,
    top: f32,
}

/// The same slot-to-slot easing as pinned reordering, applied to the actual row.
struct SidebarSessionSlide {
    from: f32,
    to: f32,
    epoch: u64,
    started: std::time::Instant,
}

impl SidebarSessionSlide {
    fn current(&self) -> f32 {
        let progress = TAB_SLIDE
            .progress(self.started.elapsed().as_secs_f32() / TAB_SLIDE.total().as_secs_f32());
        motion::lerp(self.from, self.to, progress)
    }

    fn retarget(&mut self, target: f32) {
        if (target - self.to).abs() < 0.5 {
            return;
        }
        self.from = self.current();
        self.to = target;
        self.epoch = self.epoch.wrapping_add(1);
        self.started = std::time::Instant::now();
    }
}

struct SidebarSessionReturn {
    transfer: SidebarSessionTransfer,
    epoch: u64,
    started: std::time::Instant,
}

/// Live destination for a pinned-session drag. The real row remains clipped
/// to the sidebar and slides between slots with its pinned siblings.
struct PinnedSessionDragState {
    chat_id: String,
    visible_ids: std::sync::Arc<Vec<String>>,
    from: usize,
    over: usize,
    prev_over: usize,
    epoch: usize,
    filter: Option<String>,
    profile_key: String,
    pointer_y: Option<f32>,
    viewport_top: f32,
    viewport_bottom: f32,
    generation: u64,
    autoscroll_active: bool,
}

type SidebarKeyedRow = (String, f32, AnyElement);

struct SidebarSessionRows {
    custom_count: usize,
    regular_count: usize,
    rows: Vec<SidebarKeyedRow>,
    pinned_count: usize,
    moving_row: Option<(AnyElement, f32)>,
}

/// Ghost chip following the pointer while a surface tab drags.
struct SurfaceTabGhost {
    title: SharedString,
}

impl Render for SurfaceTabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .h(px(24.0))
            .w(px(112.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

struct SurfaceTabTooltip {
    text: SharedString,
}

impl Render for SurfaceTabTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .max_w(px(380.0))
            .px(px(9.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(theme))
            .text_size(px(10.5))
            .text_color(theme.text_muted)
            .child(self.text.clone());
        crate::frost::frosted(6.0, crate::frost::MENU_BLUR, card)
    }
}
/// Drag marker for the terminal-panel height handle.
struct TerminalResize;

/// Invisible drag ghost — resize drags and contained pinned-session reorders
/// render nothing at the cursor.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A oneshot width tween (200ms ease-out), driven MANUALLY from render via
/// [`Shell::eval_tween`] — never through a `with_animation` wrapper. gpui keys
/// an animation element's start time by its full global element-id path, so a
/// wrapper that mounts/remounts (route swap, or an ancestor animation keyed by
/// a fresh epoch) silently REPLAYS the tween from t=0. Manual evaluation keeps
/// the element tree's shape constant: a finished or stale tween is exactly the
/// steady state, no matter how the tree around it remounts (round-6 §1–3).
#[derive(Debug, Clone, Copy)]
struct WidthTween {
    from: f32,
    to: f32,
    started: std::time::Instant,
}

impl WidthTween {
    fn new(from: f32, to: f32) -> Self {
        Self {
            from,
            to,
            started: std::time::Instant::now(),
        }
    }
}

fn titlebar_island_vertical_geometry(progress: f32) -> (f32, f32) {
    // Match the padded flex row's center, not the raw titlebar center.
    // Keep the native 24px controls untouched and give them 4px of air.
    let height = 28.0 + 4.0 * progress.clamp(0.0, 1.0);
    let center = (Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD) * 0.5;
    (center - height * 0.5, height)
}

fn bottom_stack_measurement_matches(
    measured_has_composer: bool,
    expected_has_composer: bool,
) -> bool {
    measured_has_composer == expected_has_composer
}

fn new_thread_background_opacity(is_frost: bool) -> f32 {
    if is_frost {
        NEW_THREAD_BACKGROUND_FROSTED_OPACITY
    } else {
        1.0
    }
}

fn new_thread_background_height(viewport_height: f32) -> f32 {
    (viewport_height.max(0.0) * NEW_THREAD_BACKGROUND_VIEWPORT_RATIO)
        .min(NEW_THREAD_BACKGROUND_MAX_HEIGHT)
}

fn new_thread_background(
    artwork: Option<std::sync::Arc<gpui::RenderImage>>,
    viewport_height: f32,
    hero_width: f32,
    composer_bounds: crate::new_thread_background_mask::SurfaceBounds,
    dissolve: f32,
    opacity: f32,
) -> AnyElement {
    let Some(artwork) = artwork else {
        return Empty.into_any_element();
    };
    let hero_height = new_thread_background_height(viewport_height);
    let dissolve = dissolve.clamp(0.0, 1.0);
    // Image and treatment share a fixed crop and fade together in place.
    // The hero uses the full conversation canvas even while the destination
    // right pane clips it. Navigation must never rescale the artwork.
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(hero_width))
        .h(px(hero_height))
        .overflow_hidden()
        .opacity((1.0 - dissolve) * opacity)
        // Alpha resolves into the real canvas, including translucent themes;
        // no theme-colored overlay bleaches or darkens the source pixels.
        .children([false, true].into_iter().map(|cutout| {
            let artwork = artwork.clone();
            let composer_bounds = composer_bounds.clone();
            div()
                .absolute()
                .inset_0()
                .opacity(if cutout {
                    1.0
                } else {
                    crate::new_thread_background_mask::CUTOUT_REVEAL_OPACITY
                })
                .child(
                    gpui::canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _cx| {
                            if let Some(composer) = composer_bounds.get() {
                                crate::new_thread_background_mask::paint(
                                    artwork.clone(),
                                    bounds,
                                    composer,
                                    cutout,
                                    window,
                                );
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
        }))
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplashPhase {
    Visible,
    FadingOut,
    Gone,
}

/// The chat-row Rename dialog.
struct RenameChatDialog {
    chat_id: String,
    input: Entity<ComposerInput>,
    /// Focus the input on the dialog's first paint (opened without window access).
    focus_pending: bool,
    _events: Subscription,
}

/// In-app update lifecycle (macOS bundle installs; see `render_update_strip`).
enum UpdateFlow {
    Idle,
    Downloading,
    /// Staged bundle ready to swap in — one click restarts into it.
    Ready(PathBuf),
    Failed(SharedString),
}

/// Account lifecycle owned by this process. Sign-in on a local workspace
/// flows through the in-place switch wizard (offer → switch → import → done);
/// `RestartPending` survives only as the fallback when the in-place swap
/// fails and a full quit is the safe way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncFlow {
    Idle,
    Enabling,
    Canceling,
    /// Signed in on a local runtime: the wizard's choice step (bring local
    /// work / start fresh / later). `notice_open: false` = postponed, badge
    /// in the account menu.
    SwitchOffer {
        notice_open: bool,
    },
    /// Stopping the local runtime and bootstrapping the synced one in-place.
    Switching {
        import: bool,
    },
    /// The one-time import stream is running on the new synced runtime.
    Importing {
        done: usize,
        total: usize,
    },
    /// Import finished; the success step stays until dismissed.
    ImportDone {
        imported: usize,
        skipped: usize,
    },
    /// The import stream reported errors or died early. Explicit retry step —
    /// structural idempotence makes re-running safe (only missing rows copy).
    /// Details ride `runtime_change_error`. `notice_open: false` = postponed:
    /// the dialog is hidden but the failure stays pending, reachable through
    /// the account menu — dismissal must never discard the only retry
    /// entry point (under Synced scope the menu otherwise offers just
    /// Sign out, and the local rows would be unreachable).
    ImportFailed {
        notice_open: bool,
    },
    RestartPending {
        notice_open: bool,
    },
    SignOutConfirm,
    SigningOut,
    SignedOutRestartRequired,
}

impl SyncFlow {
    /// States the in-place switch driver owns end-to-end — auth/scope edges
    /// must not reset them while the runtime is being replaced under the UI.
    fn is_switch_lifecycle(self) -> bool {
        matches!(
            self,
            SyncFlow::Switching { .. }
                | SyncFlow::Importing { .. }
                | SyncFlow::ImportDone { .. }
                | SyncFlow::ImportFailed { .. }
        )
    }

    fn has_visible_overlay(self) -> bool {
        match self {
            SyncFlow::Idle
            | SyncFlow::SwitchOffer { notice_open: false }
            | SyncFlow::ImportFailed { notice_open: false }
            | SyncFlow::RestartPending { notice_open: false }
            | SyncFlow::SignedOutRestartRequired => false,
            SyncFlow::Enabling
            | SyncFlow::Canceling
            | SyncFlow::SwitchOffer { notice_open: true }
            | SyncFlow::Switching { .. }
            | SyncFlow::Importing { .. }
            | SyncFlow::ImportDone { .. }
            | SyncFlow::ImportFailed { notice_open: true }
            | SyncFlow::RestartPending { notice_open: true }
            | SyncFlow::SignOutConfirm
            | SyncFlow::SigningOut => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellEscapeOutcome {
    OtherKey,
    Blocked,
    InterruptChat(String),
    Ignored,
}

fn resolve_shell_escape(
    key: &str,
    blocking_overlay: bool,
    escape_stops_active_agent: bool,
    route: Route,
    selected_chat: Option<&str>,
    indicator: Indicator,
    interrupting: bool,
) -> ShellEscapeOutcome {
    if key != "escape" {
        ShellEscapeOutcome::OtherKey
    } else if blocking_overlay {
        ShellEscapeOutcome::Blocked
    } else if !escape_stops_active_agent || !matches!(route, Route::Chat) || interrupting {
        ShellEscapeOutcome::Ignored
    } else if matches!(indicator, Indicator::Working | Indicator::AwaitingInput) {
        selected_chat
            .map(|chat_id| ShellEscapeOutcome::InterruptChat(chat_id.to_owned()))
            .unwrap_or(ShellEscapeOutcome::Ignored)
    } else {
        ShellEscapeOutcome::Ignored
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountMenuAction {
    EnableSync,
    SyncInProgress,
    /// Postponed switch wizard (or legacy restart fallback) — reopen it.
    RestartPending,
    SignOut,
}

const RUNTIME_CHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const RUNTIME_CHANGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Wait until a stopped daemon can no longer win the next bootstrap probe and
/// has released the data directory for the replacement runtime.
async fn wait_for_remote_engine_shutdown(
    ipc_port: u16,
    data_dir: &std::path::Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let port_closed = !matches!(
            tokio::time::timeout(
                Duration::from_millis(200),
                tokio::net::TcpStream::connect(("127.0.0.1", ipc_port)),
            )
            .await,
            Ok(Ok(_))
        );
        if port_closed && InstanceLock::holder(data_dir).is_none() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the daemon did not finish stopping within {} seconds",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(RUNTIME_CHANGE_POLL_INTERVAL).await;
    }
}

/// Stop the engine that owns the synced profile and wait until a local runtime
/// can safely acquire both its IPC port and data-directory lock.
async fn stop_synced_runtime(
    engine: crate::state::EngineHandle,
    ipc_port: u16,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    let stop_error = if matches!(engine.mode(), EngineMode::Remote { .. }) {
        engine
            .client()
            .call(methods::STOP_ENGINE, serde_json::json!({}))
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    engine.shutdown().await;
    match wait_for_remote_engine_shutdown(ipc_port, data_dir, RUNTIME_CHANGE_TIMEOUT).await {
        Ok(()) => Ok(()),
        Err(error) => match stop_error {
            Some(stop_error) => Err(format!("{stop_error}; {error}")),
            None => Err(error),
        },
    }
}

/// What an import-summary stream item means for the wizard: `Ok((imported,
/// skipped))` only when the engine reported zero errors; otherwise the
/// user-facing failure message. Pure so the partial-failure path is testable.
fn import_summary_outcome(item: &serde_json::Value) -> Result<(usize, usize), String> {
    let count = |key: &str| item.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let errors: Vec<&str> = item
        .get("errors")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if errors.is_empty() {
        return Ok((count("importedChats"), count("skippedChats")));
    }
    let first = errors.first().copied().unwrap_or("unknown error");
    Err(if errors.len() == 1 {
        format!("{} imported, 1 failure: {first}", count("importedChats"))
    } else {
        format!(
            "{} imported, {} failures — first: {first}",
            count("importedChats"),
            errors.len()
        )
    })
}

/// The offer step's description of what a switch would bring along, or `None`
/// when the local profile holds nothing importable. Spaces count as work:
/// a projects-only profile must get the import choice too.
fn local_work_phrase(chats: usize, spaces: usize) -> Option<String> {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match (chats, spaces) {
        (0, 0) => None,
        (c, 0) => Some(format!("the {}", plural(c, "session"))),
        (0, s) => Some(format!("the {}", plural(s, "project"))),
        (c, s) => Some(format!(
            "the {} and {}",
            plural(c, "session"),
            plural(s, "project")
        )),
    }
}

fn account_menu_action(scope: Option<WorkspaceScope>, flow: SyncFlow) -> Option<AccountMenuAction> {
    match scope {
        Some(WorkspaceScope::Local) => match flow {
            SyncFlow::Idle => Some(AccountMenuAction::EnableSync),
            SyncFlow::Enabling | SyncFlow::Canceling => Some(AccountMenuAction::SyncInProgress),
            SyncFlow::SwitchOffer { .. } | SyncFlow::RestartPending { .. } => {
                Some(AccountMenuAction::RestartPending)
            }
            SyncFlow::ImportFailed { .. } => Some(AccountMenuAction::RestartPending),
            SyncFlow::Switching { .. }
            | SyncFlow::Importing { .. }
            | SyncFlow::ImportDone { .. } => Some(AccountMenuAction::SyncInProgress),
            SyncFlow::SignOutConfirm
            | SyncFlow::SigningOut
            | SyncFlow::SignedOutRestartRequired => None,
        },
        Some(WorkspaceScope::Synced) => match flow {
            SyncFlow::SignedOutRestartRequired => None,
            // A pending import failure must stay reachable: this is the only
            // surface that can reopen the retry dialog on a synced runtime.
            SyncFlow::ImportFailed { .. } => Some(AccountMenuAction::RestartPending),
            _ if flow.is_switch_lifecycle() => Some(AccountMenuAction::SyncInProgress),
            _ => Some(AccountMenuAction::SignOut),
        },
        Some(WorkspaceScope::Development) | None => None,
    }
}

fn sync_flow_after_auth(
    flow: SyncFlow,
    scope: Option<WorkspaceScope>,
    auth: Option<&AuthState>,
) -> SyncFlow {
    match scope {
        Some(WorkspaceScope::Local) => match (flow, auth) {
            // The in-place switch owns its own lifecycle once started.
            (flow, _) if flow.is_switch_lifecycle() => flow,
            // AuthStatus belongs to the runtime, not to the Shell that opened
            // the browser. Every attached viewport must advertise the pending
            // profile switch once any of them completes sign-in.
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::Canceling, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (_, Some(AuthState::SignedIn { .. })) => SyncFlow::SwitchOffer { notice_open: true },
            _ => flow,
        },
        Some(WorkspaceScope::Synced) => match auth {
            // AuthStatus is shared by every viewport attached to the runtime.
            // Once a synced store loses its credentials, every Shell must stop:
            // letting another viewport sign in would authenticate a new account
            // while the engine still serves the previous account's fixed store.
            Some(AuthState::SignedOut) => SyncFlow::SignedOutRestartRequired,
            _ => match flow {
                SyncFlow::SignOutConfirm
                | SyncFlow::SigningOut
                | SyncFlow::SignedOutRestartRequired => flow,
                flow if flow.is_switch_lifecycle() => flow,
                _ => SyncFlow::Idle,
            },
        },
        Some(WorkspaceScope::Development) => SyncFlow::Idle,
        None => flow,
    }
}

/// The "Create your workspace" gate (feature-inventory §1.2 OrgGate).
struct OrgGateUi {
    name_input: Entity<ComposerInput>,
    orgs: Loadable<Vec<OrgRow>>,
    submitting: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    _events: Subscription,
}

/// One right-pane subagent tab: the doc it shows, its strip title, and the
/// read-only transcript entity whose drop tears the view down.
struct SubagentTab {
    doc_id: String,
    title: SharedString,
    transcript: Entity<Transcript>,
    /// Keeps a frozen-blob fetch alive (it falls back to a live doc watch).
    _fetch: Option<Task<()>>,
    /// Spawn chips INSIDE the subagent transcript open their own tabs.
    _events: Subscription,
}

#[derive(Debug, Clone)]
enum PendingExit {
    CloseWindow,
    Quit,
    RuntimeChange,
    InstallUpdate(PathBuf),
    Relaunch,
}

pub struct Shell {
    state: Entity<AppState>,
    sidebar_pane: Entity<SidebarPane>,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    /// Measured height of the bottom chrome stack (status strip + composer +
    /// terminal dock) the full-height transcript scrolls under. Paint-time
    /// measurement schedules another frame whenever this value changes.
    bottom_stack: std::rc::Rc<std::cell::Cell<f32>>,
    /// Whether `bottom_stack` was measured with the session composer present.
    /// A newly selected transcript stays hidden until this matches its route,
    /// preventing one frame at the blank canvas's stale bottom clearance.
    bottom_stack_has_composer: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared route clock and measured prepaint geometry for the persistent composer.
    composer_dock: crate::composer_dock::SharedDock,
    new_thread_artwork_ready: crate::new_thread_background_effects::Readiness,
    /// Session-transient disclosure state, matching the Archived shelf.
    pub(super) pinned_open: bool,
    pub(super) sessions_open: bool,
    /// The sidebar's archived accordion (t3code Sidebar): OPEN by default
    /// (user request), session-transient. `archived_shown` pages the
    /// expanded list ("Show more" reveals another page).
    pub(super) archived_open: bool,
    pub(super) archived_shown: usize,
    /// Ephemeral collapsed project/device sections, keyed by organization + id.
    pub(super) sidebar_collapsed_groups: std::collections::HashSet<String>,
    /// In-flight disclosure tweens, shared by device groups, Pinned and Archived.
    pub(super) sidebar_disclosure_motion:
        std::collections::HashMap<String, SidebarDisclosureMotion>,
    /// The jump-hint overlay: true while the held modifiers exactly match a
    /// jump shortcut, which swaps the first nine rows' time-ago for their
    /// key-cap chip (t3code's `showJumpHints`). Frame-transient — window
    /// deactivation clears it, so a chip cannot stick after an app switch
    /// swallows the key-up.
    pub(super) jump_hints: bool,
    /// Lazy panes: no entity (and no RPC) until first opened.
    terminal: Option<Entity<TerminalPanel>>,
    /// Embedded terminal host for right-pane Terminal surfaces — a SEPARATE
    /// entity from the bottom drawer's (own PTYs, own grid geometry; one
    /// panel can only size one visible grid at a time).
    right_terminal: Option<Entity<TerminalPanel>>,
    /// The surface-tab strip's `+` menu (Browser / Terminal / Diffs / History rows).
    right_plus: popover::Popup<()>,
    /// Host-owned project Actions cached per (device, space).
    project_actions: crate::project_actions::ProjectActionsController,
    /// Diff surfaces by id — each tab its own [`Changes`] viewer with its own
    /// scope/base pick and diff watch (multiple diff panels, user request).
    diffs: std::collections::HashMap<u64, Entity<Changes>>,
    review_hosts: std::collections::HashMap<String, (String, u32, Entity<crate::changes::walkthrough::ReviewHost>)>,
    /// One workspace browser per chat/panel key. Dropping the entity closes
    /// its file watcher and every in-flight workspace request.
    files: std::collections::HashMap<String, Entity<FilesSurface>>,
    files_subs: std::collections::HashMap<String, Subscription>,
    /// One independent editor per opened workspace file. IDs are global
    /// while the lookup key keeps a file tab scoped to its chat panel.
    file_surfaces: std::collections::HashMap<u64, Entity<FilesSurface>>,
    file_surface_paths: std::collections::HashMap<u64, String>,
    file_surface_keys: std::collections::HashMap<(String, String), u64>,
    file_surface_subs: std::collections::HashMap<u64, Subscription>,
    file_surface_seq: u64,
    pending_file_closes: std::collections::HashSet<RightSurface>,
    pending_exit: Option<PendingExit>,
    /// Event hookups for [`Self::diffs`] (History rows opening commit tabs).
    diff_subs: std::collections::HashMap<u64, Subscription>,
    diff_seq: u64,
    /// Subagent transcript surfaces by id — each tab a read-only
    /// [`Transcript`] pinned to its subagent doc.
    subagent_tabs: std::collections::HashMap<u64, SubagentTab>,
    subagent_seq: u64,
    /// Planning artifact tabs (`shell/artifacts_ui.rs`).
    pub(crate) artifact_tabs: artifacts_ui::ArtifactTabs,
    browsers: std::collections::HashMap<u64, Entity<crate::browser::BrowserSurface>>,
    browser_subs: std::collections::HashMap<u64, Subscription>,
    browser_seq: u64,
    browser_context: crate::browser::BrowserContext,
    browser_profile: Option<String>,
    /// Ordered surface tabs per panel key (drag-reorderable; stale entries —
    /// closed terminals/diffs — are skipped at read time).
    right_tabs: std::collections::HashMap<String, Vec<RightSurface>>,
    /// In-flight surface-tab drag (slide animation state).
    right_tab_drag: Option<RightTabDragState>,
    /// Surface-tab strip scroll (the strip overflows horizontally, t3
    /// ScrollArea-style; drag drop-math reads the offset back out).
    right_tab_scroll: gpui::ScrollHandle,
    /// Chat outlet vs settings pages.
    route: Route,
    /// Route history behind the titlebar back/forward buttons (§ nav history).
    nav: NavHistory,
    devices_page: Option<Entity<DevicesPage>>,
    overview_page: Option<Entity<crate::overview::Overview>>,
    pr_review_chats_page: Option<Entity<crate::chat_type_view::ChatTypeView>>,
    /// `StartPrReview` operation ids (one per PR + action + agent).
    pr_review_ops: crate::pr_review_launch::PrReviewOperations,
    branch_review_ops: crate::branch_review_launch::BranchReviewOperations,
    branch_review_targets: std::collections::HashMap<String, mast_proto::ReviewTarget>,
    branch_review_range_ids: std::collections::HashMap<String, String>,
    branch_review_loads: std::collections::HashMap<String, uuid::Uuid>,
    branch_review_chats: std::collections::HashMap<String, String>,
    #[cfg(test)]
    branch_review_script: Option<pr_review_ui::PrReviewScript>,
    /// Tests: answer `StartPrReview` here instead of the engine.
    #[cfg(test)]
    pr_review_script: Option<pr_review_ui::PrReviewScript>,
    /// Tests: answer `PrDashboard` here instead of the engine.
    #[cfg(test)]
    pub(crate) pr_dash_script: Option<crate::pr_page::PrDashScript>,
    archived_page: Option<Entity<ArchivedPage>>,
    workflow_pack_page: Option<Entity<crate::settings::workflow_pack::WorkflowPackPage>>,
    linear_settings_page: Option<Entity<crate::linear::settings_page::LinearSettingsPage>>,
    appearance_page: Option<Entity<AppearancePage>>,
    design_page: Option<Entity<crate::settings::design_panel::DesignPanel>>,
    files_settings_page: Option<Entity<FilesSettingsPage>>,
    notifications_page: Option<Entity<NotificationsPage>>,
    shortcuts_page: Option<Entity<ShortcutsPage>>,
    accounts_page: Option<Entity<AccountsPage>>,
    harnesses_page: Option<Entity<HarnessesPage>>,
    shortcuts_sub: Option<Subscription>,
    notifications_sub: Option<Subscription>,
    files_settings_sub: Option<Subscription>,
    appearance_settings_sub: Option<Subscription>,
    /// Session-row context menu, including the Copy submenu.
    chat_menu: popover::Popup<ChatMenuState>,
    rename_dialog: Option<RenameChatDialog>,
    /// Chat id awaiting delete confirmation.
    delete_confirm: Option<String>,
    /// Work Outline confirmation modal (`work_outline_ui`), `Some` while open.
    work_outline_modal: Option<work_outline_ui::WorkOutlineModal>,
    /// Modals replaced by a newer request; their cancel is delivered at the
    /// next render, when a window is at hand.
    work_outline_superseded: Vec<work_outline_ui::WorkOutlineModal>,
    /// Focus target of the Work Outline modal, claimed at the next render
    /// after it opens so keys cannot reach the outline underneath.
    work_outline_modal_focus: FocusHandle,
    /// Claim `work_outline_modal_focus` at the next render.
    work_outline_modal_focus_pending: bool,
    /// What had focus before the modal opened; restored when it closes.
    work_outline_prior_focus: Option<FocusHandle>,
    /// Outline row context menu (`work_outline_menu_ui`).
    work_outline_menu: popover::Popup<work_outline_menu_ui::WorkOutlineMenu>,
    /// Per-Chat "Apply updated instructions" progress (`instructions_ui`).
    instruction_apply: std::collections::HashMap<String, instructions_ui::ApplyState>,
    /// The chat close-out dialog (`closeout_ui`), `Some` while open.
    closeout_dialog: Option<closeout_ui::CloseoutDialog>,
    /// Monotonic id per close-out dialog open (stale-reply guard).
    closeout_seq: u64,
    /// Space-row context menu (dropdown rows): (space id, window position).
    space_menu: popover::Popup<(String, Point<Pixels>)>,
    rename_space_dialog: Option<RenameSpaceDialog>,
    /// Per-project agent account picker / add-account sign-in.
    account_flow: Option<Entity<crate::managed_accounts::AccountFlow>>,
    account_flow_sub: Option<Subscription>,
    sidebar_section_migration: Option<(String, crate::state::EngineHandle)>,
    section_dialog: Option<sidebar_sections::SectionDialog>,
    section_menu: Option<(String, Point<Pixels>)>,
    section_header_hover: Option<String>,
    section_menu_focus: FocusHandle,
    section_menu_active: Option<usize>,
    /// Space id awaiting delete confirmation (hard delete + session cascade).
    delete_space_confirm: Option<String>,
    /// The add-space palette (device tabs + folder search), `Some`
    /// while open.
    add_space: Option<AddSpaceFlow>,
    command_palette: Option<command_palette::CommandPalette>,
    workspace_search: Option<workspace_search::WorkspaceSearch>,
    workspace_file_recents: std::collections::HashMap<String, Vec<String>>,
    workspace_search_indexes:
        std::collections::HashMap<String, std::sync::Arc<Vec<workspace_search::IndexedPath>>>,
    workspace_search_index_key: Option<String>,
    workspace_search_index_engine: Option<crate::state::EngineHandle>,
    workspace_search_index_error: Option<(String, String)>,
    workspace_search_index_task: Option<Task<()>>,
    /// The sidebar's space-filter dropdown.
    spaces_menu: popover::Popup<spaces::SpacesMenu>,
    /// Hover/drag + scroll-linger state of the dropdown's floating rail.
    spaces_menu_bar: popover::MenuScrollbarState,
    /// Persisted organization/sort/metadata controls beside the project filter.
    sidebar_view_menu: popover::Popup<spaces::SidebarViewMenu>,
    /// Natural-tab-order focus target for the icon-only view-options button.
    sidebar_view_trigger_focus: gpui::FocusHandle,
    /// Current pinned-row metrics shared by hit testing and displacement animations.
    sidebar_pinned_heights: Vec<f32>,
    project_icons:
        std::cell::RefCell<std::collections::HashMap<String, Entity<project_icon::ProjectIcon>>>,
    /// Hovered row whose status is replaced by the archive control.
    chat_status_hover: Option<String>,
    /// Scroll position of the sidebar lists region (drives its edge fades).
    sidebar_scroll: gpui::ScrollHandle,
    sidebar_row_extents: spaces::SidebarRowExtents,
    /// One cached view per visible sidebar chat row, so a spinner tick (or any
    /// change confined to one row) re-renders that row alone.
    sidebar_row_views: std::collections::HashMap<String, Entity<ChatRowView>>,
    sidebar_row_views_seen: std::collections::HashSet<String>,
    /// In-flight reorder for the pinned section only.
    pinned_session_drag: Option<PinnedSessionDragState>,
    pinned_session_drag_generation: u64,
    sidebar_session_transfer: Option<SidebarSessionTransfer>,
    sidebar_session_return: Option<SidebarSessionReturn>,
    /// Pending pin intents are scoped to the active profile and engine attachment.
    sidebar_pin_write: Option<sidebar_pins::PendingSidebarPins>,
    sidebar_pin_write_generation: u64,
    sidebar_pin_write_notice: Option<SharedString>,
    /// `settings.last_space_id` applied once after the first spaces frame.
    space_boot_applied: bool,
    /// Last seen session status per chat — the chime trigger compares against
    /// it (a row's FIRST appearance never chimes, so boot stays silent).
    sound_prev: std::collections::HashMap<String, crate::sound::SessionNotificationState>,
    /// Work Outline user action requests (Milestone 4): the per-work-item
    /// zero-to-nonzero detector, fed every watched Space's snapshot.
    user_action_notifier: crate::work_outline::required_actions::UserActionNotifier,
    /// The status light the in-app logo shows; `None` while "Live app icon" is off.
    status_light: Option<crate::status_light::LightSummary>,
    dock_sync: crate::status_light::DockSync,
    menu_bar: menu_bar_ui::MenuBarUi,
    /// Spaces this Shell asked `AppState` to watch (so notifications and
    /// Required Actions work before the Overview exists).
    user_action_watch: Option<Vec<String>>,
    /// Startup-aware durable connectivity notification baseline.
    connectivity_notifications: crate::sound::ConnectivityNotificationState,
    /// Persistent across AppState observer callbacks so simultaneous session
    /// failures and connectivity degradation produce one attention sound.
    attention_sound_gate: crate::sound::AttentionSoundGate,
    user_menu: popover::Popup<()>,
    /// Inline sidebar error strip (mutation failures); click dismisses.
    sidebar_notice: Option<SharedString>,
    /// `AppState::shell_fingerprint` as of the last observed change.
    state_fingerprint: Option<u64>,
    /// A `sidebar_notice` text that was raised as an explicit failure (so it
    /// renders as the copyable error card even when its wording is unusual).
    sidebar_error_text: Option<SharedString>,
    /// Inline "outside the Space" strip for a clicked local file path.
    outside_file_prompt: Option<outside_file_ui::OutsideFilePrompt>,
    /// Chat whose header copy-identity button is showing its transient
    /// "copied" check; cleared by `copied_chat_identity_clear` after ~1.2s.
    copied_chat_identity: Option<String>,
    copied_chat_identity_clear: Option<Task<()>>,
    /// Presentation cache for the full-chat metadata strip. The durable
    /// values remain owned by the workspace doc and are read through the
    /// existing multi-PR `CHAT_LINK_STATUS` endpoint.
    chat_header_links: std::collections::HashMap<String, ChatHeaderLinkCache>,
    board_links_revision: u64,
    board_links_generation: u64,
    board_link_versions: std::collections::HashMap<String, u64>,
    chat_header_links_pending: std::collections::HashSet<String>,
    /// The header's hover context card (show / hide / ⌥⌘I pin) and the chat
    /// it belongs to.
    chat_item_card: crate::chat_item::CardState,
    running: running_ui::RunningUi,
    chat_item_card_chat: String,
    /// The Item tab's open accordion rows, its embedded Needs-you list, and
    /// the Team tab's focus target (plain `t` acts only while it is focused).
    item_tab_open: std::collections::HashSet<&'static str>,
    item_needs: Option<chat_item_ui::ItemNeeds>,
    team_panel_focus: Option<FocusHandle>,
    tracker: team_ui::TrackerState,
    /// Orchestrator wakes: per-chat status cache and refresh loops.
    wakes: wake_ui::WakeUi,
    /// Admin chats: the grant dialog.
    admin_ui: admin_ui::AdminUi,
    /// What the full-chat header adds below the metadata line (the work-context
    /// strip's line), so the transcript's top fade clears it.
    chat_header_extra_clearance: f32,
    /// Local lifecycle of an in-app update (macOS bundle swap) — the engine's
    /// UpdateStatus stream says WHETHER one exists; this says how far the
    /// download/stage of it has come in this process.
    update_flow: UpdateFlow,
    update_task: Option<Task<()>>,
    /// Version whose update strip the user dismissed (advisory installs only —
    /// a newer release shows the strip again).
    update_dismissed: Option<String>,
    /// How this binary was installed — decides the strip's click behavior.
    /// Cached: `detect_install` stats `current_exe` and this renders per frame.
    install: mast_update::InstallKind,
    org: Option<OrgGateUi>,
    sync_flow: SyncFlow,
    mutate_task: Option<Task<()>>,
    /// The one-time session_canvas bulk-import migration stream, if running —
    /// a dedicated field rather than reusing `mutate_task`/`import_task`
    /// (those belong to unrelated single-call actions and the local→synced
    /// wizard respectively) since this is its own long-running stream.
    bulk_import_task: Option<Task<()>>,
    auth_task: Option<Task<()>>,
    runtime_change_task: Option<Task<()>>,
    runtime_change_error: Option<SharedString>,
    /// The one-time local→synced import stream (switch wizard progress step).
    import_task: Option<Task<()>>,
    /// Title of the chat the import stream is copying right now.
    import_current: Option<SharedString>,
    /// Kept for the failed-gate "Retry" action.
    boot: EngineBootConfig,
    data_dir: PathBuf,
    settings: UiSettings,
    /// Session-scoped panel open flags (terminal / changes per chat; §1.10-1.11
    /// parity — heights stay in [`UiSettings`]).
    panels: SessionPanels,
    /// The panel key of the chat currently shown ("" = new-chat canvas).
    active_chat: String,
    /// Last selected session survives opening the blank Appshot destination.
    last_appshot_chat: Option<String>,
    /// Last rendered sidebar order (key + estimated height) — the FLIP baseline
    /// for the §1.6 resort glide.
    sidebar_prev_order: Vec<(String, f32)>,
    /// Per-key paint offsets of the resort in flight, keyed elements restart on
    /// `resort_epoch` bumps.
    sidebar_resort: std::collections::HashMap<String, f32>,
    /// Keys that just appeared in a live list (fade in, no glide).
    sidebar_new_keys: std::collections::HashSet<String>,
    resort_epoch: usize,
    /// Last observed `window.is_window_active()` — rising edge fires a
    /// ProbeSync so a broadcast-deaf room heals as the user looks at the app.
    was_window_active: bool,
    /// Dev/testing knobs (`MAST_OPEN_DIALOG`, `MAST_FORCE_GATE`,
    /// `MAST_DEMO_UPLOAD`) — see [`Shell::new`].
    debug_dialog: Option<String>,
    debug_gate: Option<GatePhase>,
    debug_upload: Option<String>,
    sidebar_tween: Option<WidthTween>,
    files_tween: Option<WidthTween>,
    sidebar_edge_bounce: Option<motion::ResizeEdgeBounce>,
    /// Boundary currently held during a sidebar drag. Cleared on re-entry or
    /// release so the next genuine edge crossing can acknowledge the limit.
    sidebar_resize_edge: Option<motion::ResizeEdge>,
    /// Gesture-owned resize feedback. Unlike hover, this stays active while
    /// the seam moves away from the pointer and clears only on release or when
    /// a constrained edge takes over with its bounce cue.
    pane_resize_active: Option<PaneResizeKind>,
    pane_resize_dragging: Option<PaneResizeKind>,
    right_tween: Option<WidthTween>,
    right_edge_bounce: Option<motion::ResizeEdgeBounce>,
    right_resize_edge: Option<motion::ResizeEdge>,
    /// Mirrors `right_tween` only for takeover entry/exit, allowing the visible
    /// right-panel contents to resize with their outer frame in that mode.
    right_takeover_content_tween: Option<WidthTween>,
    /// Conversation-width tween used only while entering/leaving right-pane
    /// takeover. Normal right-pane open/close keeps the upstream flex behavior.
    main_takeover_tween: Option<WidthTween>,
    /// Changes-panel takeover (the header's expand button): the panel fills
    /// everything right of the sidebar and the conversation column collapses
    /// to zero. Session-local view state — never persisted, reset on close.
    right_pane_expanded: bool,
    /// Viewport width stamped each frame at render — the expanded panel's
    /// width target and the physical ceiling for free-form resizing
    /// ([`Self::right_target`] has no `Window`).
    viewport_width: f32,
    viewport_height: f32,
    terminal_tween: Option<WidthTween>,
    /// Last observed `window.is_fullscreen()` (`None` before first paint) —
    /// flips key the traffic-light inset tween.
    fullscreen: Option<bool>,
    /// 200ms ease-out tween of the cluster start on fullscreen toggles.
    titlebar_tween: Option<WidthTween>,
    titlebar_island: Option<WidthTween>,
    /// Armed by mouse-down on a titlebar strip; the next mouse-move hands the
    /// drag to the compositor (zed's platform-titlebar pattern).
    titlebar_should_move: bool,
    /// The caption buttons zeron itself draws on Linux under client-side
    /// decorations, per side, already filtered to what the compositor
    /// supports — `None` off Linux or under server decorations (where the WM
    /// draws real buttons). Re-resolved every frame at the top of `render`.
    linux_captions: Option<gpui::WindowButtonLayout>,
    /// Re-renders when the desktop's button layout changes (GNOME
    /// `button-layout` gsetting). Registered on first paint — [`Shell::new`]
    /// has no window.
    button_layout_sub: Option<Subscription>,
    /// Clears the height tween once it completes (so a closed panel unmounts).
    terminal_tween_task: Option<Task<()>>,
    /// Height-drag anchor: (pointer y, height) at mouse-down on the handle.
    terminal_drag_anchor: Option<(f32, f32)>,
    /// `motion::reduced_motion` snapshot, refreshed at the top of each render
    /// pass so [`Shell::eval_tween`] (called from `&self` render helpers) can
    /// snap without a `cx`.
    reduced_motion: bool,
    /// Set by [`Shell::eval_tween`] when any tween is mid-flight this frame;
    /// render schedules the next animation frame off it.
    motion_active: std::cell::Cell<bool>,
    /// All pane masks and chrome evaluate animation at the same frame time.
    /// A slow render must not give the native page and its titlebar different widths.
    render_time: Option<std::time::Instant>,
    splash: SplashPhase,
    splash_task: Option<Task<()>>,
    /// Focus fallback (registered on first paint — [`Shell::new`] has no
    /// window): keyboard shortcuts dispatch through the window focus chain, so
    /// with missing or unmounted focus they go dead. Recover after handoffs
    /// settle, preserving focus on mounted controls.
    focus_sub: Option<Subscription>,
    shortcut_focus: FocusHandle,
    /// Every `MUTATE` request, in order (tests read what was asked for).
    #[cfg(test)]
    pub(crate) mutations: Vec<serde_json::Value>,
    /// Neutral shortcut target after clicking away from an input.
    unfocused: FocusHandle,
    /// Clears the jump hints when the window deactivates: a Cmd+Tab away
    /// swallows the key-up, so without this the chips stay on screen for good.
    activation_sub: Option<Subscription>,
    /// 1s heartbeat re-rendering the working indicator (elapsed + flavour word).
    _ticker: Task<()>,
    _state_observation: Subscription,
    _composer_events: Subscription,
    /// The primary transcript's spawn-chip events (subagent tabs).
    _transcript_events: Subscription,
    _transcript_invalidation: Subscription,
}

impl Shell {
    pub fn new(state: Entity<AppState>, boot: EngineBootConfig, cx: &mut Context<Self>) -> Self {
        let observation = cx.observe(&state, |this: &mut Shell, state, cx| {
            // The reconcile always runs; the repaint (which cascades to the
            // transcript, sidebar and every other child the shell renders)
            // only when the state the shell reads, or the shell's own
            // outcome of reconciling it, changed. Streaming frames for the
            // transcript and composer reach those views directly.
            let fingerprint = state.read(cx).shell_fingerprint(Utc::now());
            let before = this.reconcile_signature();
            this.on_state_changed(&state, cx);
            if this.state_fingerprint != Some(fingerprint) || before != this.reconcile_signature() {
                this.state_fingerprint = Some(fingerprint);
                cx.notify();
            }
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, _| transcript.retain_for_route_exit());
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let links = Self::session_links(None, cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        // Every send glides the prompt to the viewport top and reserves the
        // reply's space below it (notes-app parity).
        let composer_events = cx.subscribe(&composer, {
            let transcript = transcript.clone();
            move |this: &mut Shell, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::SignIn { device, harness, account, snapshot } => {
                    let label = account.as_ref().and_then(|id| snapshot.accounts.iter().find(|a| &a.id == id)).map(|a| a.label.clone());
                    this.open_settings(SettingsSection::Agents, cx);
                    if this.accounts_page.is_none() {
                        let state = this.state.clone();
                        this.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
                    }
                    this.accounts_page.as_ref().unwrap().update(cx, |page, cx| page.sign_in_for_chat(device.clone(), *harness, account.clone(), label, snapshot.clone(), cx));
                }
                ComposerEvent::NewThreadTransitionStarted => {
                    // Route observation drives the dock once selection commits.
                    cx.notify();
                }
                ComposerEvent::Sent {
                    chat_id,
                    message_id,
                } => {
                    transcript.update(cx, |t, cx| {
                        t.on_own_send(chat_id.clone(), message_id.clone(), cx)
                    });
                }
                ComposerEvent::WorktreeSetup {
                    chat_id,
                    setup_action,
                    setup_error,
                    target_device_id,
                } => this.attach_worktree_setup(
                    chat_id.clone(),
                    setup_action.clone(),
                    setup_error.clone(),
                    target_device_id.clone(),
                    cx,
                ),
                ComposerEvent::Queued {
                    chat_id,
                    message_id,
                } => {
                    transcript.update(cx, |t, cx| {
                        t.on_own_queued_send(chat_id.clone(), message_id.clone(), cx)
                    });
                }
            }
        });
        // Spawn chips open their subagent's transcript as a right-pane tab.
        let transcript_events = cx.subscribe(&transcript, Self::on_transcript_event);
        // Working-indicator heartbeat: notify once a second while a session is
        // live so elapsed time and the flavour word stay fresh.
        let ticker = cx.spawn(async move |this, cx| {
            let mut displayed_minute = Utc::now().timestamp().div_euclid(60);
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let minute = Utc::now().timestamp().div_euclid(60);
                let minute_changed = minute != displayed_minute;
                displayed_minute = minute;
                let alive = this.update(cx, |shell: &mut Shell, cx| {
                    shell.refresh_heartbeat(Utc::now(), minute_changed, cx);
                    // Staleness ages sessions out with no state change, and the
                    // Dock debounce needs a clock: a once-a-second check does both.
                    shell.sync_status_light(cx);
                    shell.running_tick(cx);
                    shell.admin_tick(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let data_dir = boot.data_dir.clone();
        let settings = settings::current(cx);
        state.update(cx, |state, cx| {
            state.set_change_requests_visible(settings.sidebar_show_pull_request, cx)
        });
        crate::appshots::set_enabled(settings.appshots_enabled);
        crate::appshots::set_capture_sound_enabled(settings.appshot_sound_enabled);
        // Bind the customizable shortcuts from the persisted keymap.
        tracing::info_span!("startup_apply_keymap")
            .in_scope(|| apply_keymap(cx, &settings.keymap, settings.composer_send_behavior));
        // Dev/testing knob: `MAST_OPEN_ROUTE=settings[/<section>]` boots
        // straight into a settings section — these pages have no deep link and
        // synthetic input can't reach them on headless compositors.
        let route = match mast_proto::env::var("MAST_OPEN_ROUTE").ok().as_deref() {
            Some("settings") | Some("settings/devices") => {
                Route::Settings(SettingsSection::Devices)
            }
            Some("settings/agents") => Route::Settings(SettingsSection::Agents),
            Some("settings/harnesses") => Route::Settings(SettingsSection::Harnesses),
            Some("settings/appearance") => Route::Settings(SettingsSection::Appearance),
            Some("settings/notifications") => Route::Settings(SettingsSection::Notifications),
            Some("settings/linear") => Route::Settings(SettingsSection::Linear),
            Some("settings/shortcuts") => Route::Settings(SettingsSection::Shortcuts),
            Some("settings/appshots") => Route::Settings(SettingsSection::Appshots),
            Some("settings/archived") => Route::Settings(SettingsSection::Archived),
            Some("settings/workflow-pack") => Route::Settings(SettingsSection::WorkflowPack),
            Some("pr-reviews") | Some("pr-review-chats") => Route::PrReviewChats,
            // `new` pins the new-chat canvas (suppresses boot auto-select).
            Some("new") => {
                state.update(cx, |s, _| s.auto_selected = true);
                Route::Chat
            }
            _ => Route::Chat,
        };
        // More capture knobs of the same kind: `MAST_OPEN_DIALOG=rename|delete`
        // opens that dialog for the first chat once chats land; `=model` pops
        // the combined harness/model menu once the shell is Ready;
        // `MAST_FORCE_GATE=signin|org|failed` renders that gate regardless of
        // real auth state (display-only — for styling passes).
        let debug_dialog = mast_proto::env::var("MAST_OPEN_DIALOG").ok();
        // `MAST_DEMO_UPLOAD=<pct>:<image path>` fabricates an in-flight image
        // send on the selected chat (echo bubble + frozen thumbnail progress
        // ring) — display-only; a real upload can't be paused for a capture.
        let debug_upload = mast_proto::env::var("MAST_DEMO_UPLOAD").ok();
        let debug_gate = match mast_proto::env::var("MAST_FORCE_GATE").ok().as_deref() {
            Some("signin") => Some(GatePhase::SignIn),
            Some("org") => Some(GatePhase::OrgGate),
            Some("failed") => Some(GatePhase::Failed(
                "Could not reach the Mast engine on port 27901".into(),
            )),
            _ => None,
        };
        let nav = NavHistory::new(match route {
            Route::Chat => NavEntry::Chat(String::new()),
            Route::Settings(section) => NavEntry::Settings(section),
            Route::Overview => NavEntry::Overview,
            Route::PrReviewChats => NavEntry::PrReviewChats,
            Route::Review => NavEntry::Chat(String::new()),
        });
        // Parent notifications carry presentation changes (session status,
        // elapsed labels, menus); sibling animation/caret ticks do not.
        let transcript_invalidation = cx.observe_self(|shell, cx| {
            shell.transcript.update(cx, |_, cx| cx.notify());
        });
        let shell = cx.entity();
        let sidebar_pane = cx.new(|cx| SidebarPane {
            #[cfg(test)]
            renders: 0,
            shell: shell.downgrade(),
            _observation: cx.observe(&shell, |_, _, cx| cx.notify()),
        });
        let install = mast_update::detect_install();
        Self {
            state,
            sidebar_pane,
            transcript,
            composer,
            // Seed with the compact composer stack's rough height so the
            // first frame's clearance isn't zero (the measure corrects it).
            bottom_stack: std::rc::Rc::new(std::cell::Cell::new(120.0)),
            bottom_stack_has_composer: std::rc::Rc::new(std::cell::Cell::new(false)),
            composer_dock: Default::default(),
            new_thread_artwork_ready: Default::default(),
            archived_open: true,
            pinned_open: true,
            sessions_open: true,
            archived_shown: 0,
            sidebar_collapsed_groups: std::collections::HashSet::new(),
            sidebar_disclosure_motion: std::collections::HashMap::new(),
            jump_hints: false,
            terminal: None,
            right_terminal: None,
            right_plus: popover::Popup::default(),
            project_actions: crate::project_actions::ProjectActionsController::default(),
            diffs: std::collections::HashMap::new(),
            files: std::collections::HashMap::new(),
            files_subs: std::collections::HashMap::new(),
            file_surfaces: std::collections::HashMap::new(),
            file_surface_paths: std::collections::HashMap::new(),
            file_surface_keys: std::collections::HashMap::new(),
            file_surface_subs: std::collections::HashMap::new(),
            file_surface_seq: 0,
            pending_file_closes: std::collections::HashSet::new(),
            pending_exit: None,
            diff_subs: std::collections::HashMap::new(),
            diff_seq: 0,
            subagent_tabs: std::collections::HashMap::new(),
            artifact_tabs: Default::default(),
            subagent_seq: 0,
            browsers: std::collections::HashMap::new(),
            browser_subs: std::collections::HashMap::new(),
            browser_seq: 0,
            browser_context: crate::browser::BrowserContext::default(),
            browser_profile: None,
            right_tabs: std::collections::HashMap::new(),
            right_tab_drag: None,
            right_tab_scroll: gpui::ScrollHandle::new(),
            route,
            nav,
            devices_page: None,
            overview_page: None,
            pr_review_chats_page: None,
            review_hosts: Default::default(),
            pr_review_ops: Default::default(),
            branch_review_ops: Default::default(),
            branch_review_targets: Default::default(),
            branch_review_range_ids: Default::default(),
            branch_review_loads: Default::default(),
            branch_review_chats: Default::default(),
            #[cfg(test)]
            branch_review_script: None,
            #[cfg(test)]
            pr_review_script: None,
            #[cfg(test)]
            pr_dash_script: None,
            archived_page: None,
            workflow_pack_page: None,
            linear_settings_page: None,
            appearance_page: None,
            design_page: None,
            files_settings_page: None,
            notifications_page: None,
            shortcuts_page: None,
            accounts_page: None,
            harnesses_page: None,
            shortcuts_sub: None,
            notifications_sub: None,
            files_settings_sub: None,
            appearance_settings_sub: None,
            chat_menu: popover::Popup::default(),
            rename_dialog: None,
            delete_confirm: None,
            work_outline_modal: None,
            work_outline_superseded: Vec::new(),
            work_outline_modal_focus: cx.focus_handle(),
            work_outline_modal_focus_pending: false,
            work_outline_prior_focus: None,
            work_outline_menu: popover::Popup::default(),
            instruction_apply: std::collections::HashMap::new(),
            closeout_dialog: None,
            closeout_seq: 0,
            space_menu: popover::Popup::default(),
            rename_space_dialog: None,
            account_flow: None,
            account_flow_sub: None,
            sidebar_section_migration: None,
            section_dialog: None,
            section_menu: None,
            section_header_hover: None,
            section_menu_focus: cx.focus_handle(),
            section_menu_active: None,
            delete_space_confirm: None,
            add_space: None,
            command_palette: None,
            workspace_search: None,
            workspace_file_recents: std::collections::HashMap::new(),
            workspace_search_indexes: std::collections::HashMap::new(),
            workspace_search_index_key: None,
            workspace_search_index_engine: None,
            workspace_search_index_error: None,
            workspace_search_index_task: None,
            spaces_menu: popover::Popup::default(),
            spaces_menu_bar: popover::MenuScrollbarState::default(),
            sidebar_view_menu: popover::Popup::default(),
            sidebar_view_trigger_focus: cx.focus_handle().tab_stop(true),
            sidebar_pinned_heights: Vec::new(),
            project_icons: Default::default(),
            chat_status_hover: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            sidebar_row_extents: Default::default(),
            sidebar_row_views: Default::default(),
            sidebar_row_views_seen: Default::default(),
            pinned_session_drag: None,
            pinned_session_drag_generation: 0,
            sidebar_session_transfer: None,
            sidebar_session_return: None,
            sidebar_pin_write: None,
            sidebar_pin_write_generation: 0,
            sidebar_pin_write_notice: None,
            space_boot_applied: false,
            sound_prev: std::collections::HashMap::new(),
            user_action_notifier: Default::default(),
            status_light: None,
            dock_sync: Default::default(),
            menu_bar: Default::default(),
            user_action_watch: None,
            connectivity_notifications: Default::default(),
            attention_sound_gate: Default::default(),
            user_menu: popover::Popup::default(),
            sidebar_notice: None,
            state_fingerprint: None,
            sidebar_error_text: None,
            outside_file_prompt: None,
            copied_chat_identity: None,
            copied_chat_identity_clear: None,
            chat_header_links: Default::default(),
            board_links_revision: 0,
            board_links_generation: 0,
            board_link_versions: Default::default(),
            chat_header_links_pending: Default::default(),
            chat_item_card: Default::default(),
            running: running_ui::RunningUi::new(
                matches!(install, mast_update::InstallKind::MacApp { .. }),
                cx,
            ),
            chat_item_card_chat: String::new(),
            item_tab_open: Default::default(),
            item_needs: None,
            team_panel_focus: None,
            tracker: Default::default(),
            wakes: Default::default(),
            admin_ui: Default::default(),
            chat_header_extra_clearance: 0.0,
            update_flow: UpdateFlow::Idle,
            update_task: None,
            update_dismissed: None,
            install,
            org: None,
            sync_flow: SyncFlow::Idle,
            mutate_task: None,
            bulk_import_task: None,
            auth_task: None,
            runtime_change_task: None,
            runtime_change_error: None,
            import_task: None,
            import_current: None,
            boot,
            data_dir,
            settings,
            panels: SessionPanels::default(),
            active_chat: String::new(),
            last_appshot_chat: None,
            sidebar_prev_order: Vec::new(),
            sidebar_resort: std::collections::HashMap::new(),
            sidebar_new_keys: std::collections::HashSet::new(),
            resort_epoch: 0,
            was_window_active: false,
            debug_dialog,
            debug_gate,
            debug_upload,
            sidebar_tween: None,
            files_tween: None,
            sidebar_edge_bounce: None,
            sidebar_resize_edge: None,
            pane_resize_active: None,
            pane_resize_dragging: None,
            right_tween: None,
            right_edge_bounce: None,
            right_resize_edge: None,
            right_takeover_content_tween: None,
            main_takeover_tween: None,
            right_pane_expanded: false,
            viewport_width: 1280.0,
            viewport_height: 880.0,
            terminal_tween: None,
            fullscreen: None,
            titlebar_tween: None,
            titlebar_island: None,
            titlebar_should_move: false,
            linux_captions: None,
            button_layout_sub: None,
            terminal_tween_task: None,
            terminal_drag_anchor: None,
            reduced_motion: false,
            motion_active: std::cell::Cell::new(false),
            render_time: None,
            splash: SplashPhase::Visible,
            splash_task: None,
            focus_sub: None,
            shortcut_focus: cx.focus_handle(),
            #[cfg(test)]
            mutations: Vec::new(),
            unfocused: cx.focus_handle(),
            activation_sub: None,
            _ticker: ticker,
            _state_observation: observation,
            _composer_events: composer_events,
            _transcript_events: transcript_events,
            _transcript_invalidation: transcript_invalidation,
        }
    }

    fn refresh_heartbeat(&mut self, now: chrono::DateTime<Utc>, minute_changed: bool, cx: &mut Context<Self>) {
        let (live, degraded, fingerprint) = {
            let state = self.state.read(cx);
            (
                state.selected_chat.as_deref().is_some_and(|id| {
                    state.indicator_for(id, now) != Indicator::None
                }),
                matches!(
                    state.connectivity.state,
                    mast_proto::ConnectivityState::Offline | mast_proto::ConnectivityState::Reconnecting
                ),
                state.shell_fingerprint(now),
            )
        };
        let state_changed = self.state_fingerprint != Some(fingerprint);
        self.state_fingerprint = Some(fingerprint);
        // Time alone can expire a session or a pending-send overlay. The
        // fingerprint catches those transitions, while relative sidebar times
        // and the connection retry countdown keep their existing cadence.
        if state_changed || minute_changed || degraded || (live && self.running.open) {
            cx.notify();
        } else if live {
            self.transcript.update(cx, |_, cx| cx.notify());
        }
    }

    /// Route a completed viewer-side capture only after its source window is
    /// safely captured. The explicit target key avoids relying on the
    /// state-observation/draft-swap effect ordering when opening the canvas.
    pub fn receive_appshot(
        &mut self,
        appshot: crate::appshots::CapturedAppshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::appshots::AppshotDestination;

        let selected = self.state.read(cx).selected_chat.clone();
        let target = match self.settings.appshot_destination {
            AppshotDestination::Automatic if selected.is_some() => selected,
            AppshotDestination::LastSession if selected.is_some() => selected,
            AppshotDestination::LastSession => self
                .last_appshot_chat
                .clone()
                .filter(|id| self.state.read(cx).chats.iter().any(|chat| &chat.id == id)),
            AppshotDestination::Automatic | AppshotDestination::NewSession => None,
        };
        if let Some(chat_id) = &target {
            self.open_chat(chat_id.clone(), cx);
        } else if self.settings.appshot_destination == AppshotDestination::NewSession
            || self.state.read(cx).selected_chat.is_some()
        {
            // Reuse upstream's project-filter and device defaults for a new
            // canvas. Automatic capture on an existing canvas keeps its pick.
            self.open_new_session(cx);
        } else {
            self.route = Route::Chat;
        }
        let key = target.unwrap_or_default();
        self.composer.update(cx, |composer, cx| {
            composer.stage_appshot_for(key, appshot, cx)
        });
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    pub fn show_appshot_error(
        &mut self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.route = Route::Chat;
        self.composer
            .update(cx, |composer, cx| composer.show_appshot_error(message, cx));
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// Raise `message` as a failure: the sidebar shows it as the persistent,
    /// copyable error card until dismissed.
    pub(crate) fn set_sidebar_error(&mut self, message: impl Into<SharedString>) {
        let message = message.into();
        self.sidebar_error_text = Some(message.clone());
        self.sidebar_notice = Some(message);
    }

    /// Whether the sidebar notice is a failure (explicit, or worded like one).
    pub(crate) fn sidebar_notice_is_error(&self, notice: &SharedString) -> bool {
        self.sidebar_error_text.as_ref() == Some(notice)
            || crate::error_notice::text_reports_failure(notice)
    }

    // ---- splash ----

    /// The shell-owned fields `on_state_changed` can change.
    fn reconcile_signature(&self) -> (Route, Option<SharedString>, SyncFlow, SplashPhase) {
        (
            self.route,
            self.sidebar_notice.clone(),
            self.sync_flow,
            self.splash,
        )
    }

    fn on_state_changed(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        self.prune_file_explorers(cx);
        self.sync_status_light(cx);
        if let Some(notice) = state.update(cx, |state, _| state.take_deep_link_notice()) {
            self.sidebar_notice = Some(notice.into());
        }
        let next_sync_flow = {
            let state = state.read(cx);
            sync_flow_after_auth(self.sync_flow, state.workspace_scope, state.auth.as_ref())
        };
        if next_sync_flow != self.sync_flow {
            self.sync_flow = next_sync_flow;
            if matches!(
                self.sync_flow,
                SyncFlow::RestartPending { .. } | SyncFlow::SwitchOffer { .. }
            ) {
                self.org = None;
            }
        }
        // The in-place local→synced switch: once the replacement runtime is
        // attached and Ready, kick the import (or finish) from here.
        self.drive_sync_switch(cx);
        let signed_out_synced = {
            let state = state.read(cx);
            state.workspace_scope == Some(WorkspaceScope::Synced)
                && matches!(state.auth, Some(AuthState::SignedOut))
        };
        // AuthStatus is shared by every viewport. Whichever viewport owns the
        // embedded runtime drains it; remote viewports request daemon shutdown
        // and all of them independently reattach to the new local runtime.
        if signed_out_synced && self.runtime_change_task.is_none() {
            self.start_local_runtime_transition(false, cx);
        }
        // Capture knob: the add-space palette needs only the device registry.
        if self.debug_dialog.as_deref() == Some("add-space") && !state.read(cx).devices.is_empty() {
            self.debug_dialog = None;
            self.open_add_space(cx);
        }
        // Capture knob: pop the requested dialog once chats have landed.
        if let Some(which) = self.debug_dialog.clone()
            && let Some(first) = state.read(cx).chats.first().map(|c| c.id.clone())
        {
            self.debug_dialog = None;
            match which.as_str() {
                "rename" => self.open_rename_chat(first, cx),
                "delete" => {
                    self.delete_confirm = Some(first);
                }
                _ => {}
            }
        }
        // Capture knob: `MAST_DEMO_UPLOAD=<pct>:<image path>` — once a chat
        // is selected, push a fake sending echo carrying that image as a
        // pending attachment and freeze upload progress at <pct>, so the
        // thumbnail progress ring can be styled/screenshotted (a real upload
        // is too fast to pause).
        if let Some(spec) = self.debug_upload.clone()
            && let Some(chat_id) = state.read(cx).selected_chat.clone()
        {
            self.debug_upload = None;
            if let Some((pct, img_path)) = spec.split_once(':')
                && let Ok(pct) = pct.parse::<u64>()
                && let Ok(att) = crate::attachments::stage_file(std::path::Path::new(img_path))
            {
                let pending_path = format!("pending/{}/{}", att.id, att.name);
                let device_ids: Vec<String> = {
                    let s = state.read(cx);
                    s.selected_chat_row()
                        .map(|c| c.device_id.clone())
                        .into_iter()
                        .chain(s.local_device_id.clone())
                        .chain(Some("local".to_string()))
                        .collect()
                };
                for device_id in &device_ids {
                    crate::attachments::seed_attachment(
                        device_id,
                        &pending_path,
                        &att.name,
                        att.image.clone(),
                    );
                }
                let text = crate::attachments::with_attachments(
                    "Here is the screenshot of the bug.",
                    std::slice::from_ref(&pending_path),
                );
                let echo = mast_doc::SessionMessageEntry {
                    id: "demo-upload-echo".into(),
                    role: mast_doc::MessageRole::User,
                    parts: vec![mast_doc::MessagePart::Text {
                        id: "t0".into(),
                        text,
                    }],
                    created_at: chrono::Utc::now().timestamp_millis(),
                    device_id: "local".into(),
                    status: None,
                    continuation_of: None,
                };
                state.update(cx, |s, cx| {
                    s.push_echo(&chat_id, echo);
                    s.begin_upload_progress(
                        100,
                        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(pct)),
                    );
                    cx.notify();
                });
            }
        }
        // Banners and chimes share one session detector. Completion markers survive
        // queue handoffs and never advance for interrupts or stale activity.
        // A row's first appearance seeds the baseline silently (boot/replay).
        // Pending sends consume completion changes silently, while questions
        // still ring immediately. Output settings do not affect the baseline.
        {
            let now = Utc::now();
            type Ping = (
                String,
                crate::sound::SessionNotificationState,
                bool,
                Option<String>,
            );
            let (sessions, connectivity, connectivity_observed) = {
                let state = state.read(cx);
                let sessions: Vec<Ping> = state
                    .sessions
                    .iter()
                    .map(|s| {
                        let status = crate::sound::SessionNotificationState::new(s, now);
                        let send_pending = state.send_pending(&s.chat_id, now);
                        let title = state
                            .chats
                            .iter()
                            .find(|c| c.id == s.chat_id)
                            .and_then(|c| c.title.clone());
                        (s.chat_id.clone(), status, send_pending, title)
                    })
                    .collect();
                (
                    sessions,
                    state.connectivity.state,
                    state.connectivity_observed,
                )
            };
            // Background-only banners: `active_window()` is app-level (any
            // Mast window being key), so a ping for a *background chat* in a
            // focused app still stays a chime — you're already looking at
            // Mast; the sidebar dot carries the rest.
            let app_focused = cx.active_window().is_some();
            for (chat_id, status, send_pending, title) in sessions {
                let prev = self.sound_prev.insert(chat_id.clone(), status.clone());
                if let Some(prev) = prev
                    && let Some(sound) = status.sound_since(&prev, send_pending)
                {
                    if self.settings.session_sound_enabled(sound) {
                        let should_play = sound != crate::sound::Sound::Attention
                            || self
                                .attention_sound_gate
                                .should_play(std::time::Instant::now());
                        if should_play {
                            crate::sound::play(sound);
                        }
                    }
                    if self.settings.notifications_enabled
                        && !(self.settings.notifications_background_only && app_focused)
                    {
                        let title = title.unwrap_or_else(|| "New session".into());
                        let body = match sound {
                            crate::sound::Sound::Done => "Run finished",
                            crate::sound::Sound::Request => "Waiting on your input",
                            crate::sound::Sound::Attention => "Run failed",
                        };
                        crate::notify::post(&title, body, Some(&chat_id));
                    }
                }
            }
            self.notify_user_actions(state, app_focused, cx);
            if let Some(sound) = self.connectivity_notifications.update(
                connectivity,
                connectivity_observed,
                std::time::Instant::now(),
            ) {
                if self.settings.session_sound_enabled(sound)
                    && self
                        .attention_sound_gate
                        .should_play(std::time::Instant::now())
                {
                    crate::sound::play(sound);
                }
                if self.settings.notifications_enabled
                    && !(self.settings.notifications_background_only && app_focused)
                {
                    let body = match connectivity {
                        mast_proto::ConnectivityState::Offline => "Your device is offline",
                        _ => "Mast is trying to reconnect",
                    };
                    crate::notify::post("Connection unavailable", body, None);
                }
            }
        }
        // An explicit projectless canvas must be visible in the sidebar:
        // retaining a project filter would hide the session on its first send.
        if state.read(cx).no_project
            && state.read(cx).selected_chat.is_none()
            && self.settings.space_filter.take().is_some()
        {
            self.schedule_save(cx);
        }
        // Boot: restore the last selected space once the first spaces frame
        // lands (a still-existing row wins over the auto-selected first one;
        // the boot-auto-selected chat's own space wins over both — selecting a
        // chat implies its space, which `select_chat` already applied).
        if !self.space_boot_applied && !state.read(cx).spaces.is_empty() {
            self.space_boot_applied = true;
            if state.read(cx).selected_chat.is_none() {
                // A set sidebar filter is an explicit standing choice — the
                // canvas defaults (project AND its device) follow it, even
                // over a remembered "no Space" opt-out. Otherwise the last
                // selected project stands, unless opted out.
                let exists = |id: &String| state.read(cx).space_row(id).is_some();
                let filter = self.settings.space_filter.clone().filter(&exists);
                let target = match filter {
                    Some(filter) => Some(filter),
                    None if !state.read(cx).no_project => {
                        self.settings.last_space_id.clone().filter(&exists)
                    }
                    None => None,
                };
                if target.is_some() {
                    state.update(cx, |s, cx| s.select_space(target, cx));
                }
            }
        }
        // Persist the selected space (the new-tab fallback under "All").
        {
            let selected_space = state.read(cx).selected_space.clone();
            if selected_space != self.settings.last_space_id && selected_space.is_some() {
                self.settings.last_space_id = selected_space;
                self.schedule_save(cx);
            }
        }
        // Boot landing: the most recent session once the first chats frame
        // syncs (manual selection wins).
        self.boot_select_chat(cx);
        // Heal a dangling sidebar filter (space deleted, possibly elsewhere):
        // fall back to "All" rather than filtering everything out.
        if state.read(cx).spaces_synced
            && let Some(filter) = self.settings.space_filter.clone()
            && state.read(cx).space_row(&filter).is_none()
        {
            self.settings.space_filter = None;
            self.schedule_save(cx);
        }
        self.reconcile_sidebar_pins(cx);
        if !self.pinned_session_drag_is_valid(cx) {
            self.cancel_pinned_session_drag(cx);
        }
        // Chat switch: restore THAT chat's panel state (per-session open flags;
        // snap, no tween — the panels belong to the destination chat).
        let selected = state.read(cx).selected_chat.clone().unwrap_or_default();
        if !selected.is_empty() {
            self.last_appshot_chat = Some(selected.clone());
        }
        if selected != self.active_chat {
            self.suspend_file_images(cx);
            self.active_chat = selected;
            // Route history: a chat switch is a navigation. The very first
            // selection off the untouched boot canvas REPLACES that entry —
            // zeron's `/` route redirected into the last-used chat, leaving no
            // dead Back target. Walking history lands here too, but the
            // destination already equals `current()`, so the push dedups.
            if matches!(self.route, Route::Chat) {
                let entry = NavEntry::Chat(self.active_chat.clone());
                if self.nav.len() == 1 && *self.nav.current() == NavEntry::Chat(String::new()) {
                    self.nav.replace(entry);
                } else {
                    self.nav.push(entry);
                }
            }
            self.files_tween = None;
            self.right_tween = None;
            self.right_takeover_content_tween = None;
            self.main_takeover_tween = None;
            self.terminal_tween = None;
            let panels = self.panels.get(&self.panel_key(cx));
            if let Some(panel) = self.terminal.clone() {
                panel.update(cx, |panel, cx| panel.set_open(panels.terminal_open, cx));
            }
            if panels.changes_open
                && let RightSurface::Diff(id) = self.resolved_right_active(cx)
                && let Some(changes) = self.diffs.get(&id).cloned()
            {
                changes.update(cx, |changes, cx| changes.ensure_content(cx));
            }
            if panels.changes_open && self.resolved_right_active(cx) == RightSurface::Team {
                let chat_id = self.active_chat.clone();
                self.load_tracker_project(&chat_id, false, cx);
                self.watch_tracker_sync(&chat_id, cx);
            } else {
                self.stop_tracker_sync();
            }
        }
        self.refresh_tracker_source_if_changed(cx);
        self.prewarm_workspace_search(cx);
        match state.read(cx).connection {
            ConnectionStatus::Ready => {
                if self.splash == SplashPhase::Visible {
                    self.splash = SplashPhase::FadingOut;
                    self.splash_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(SPLASH_OUT.total() + Duration::from_millis(30))
                            .await;
                        this.update(cx, |shell, cx| {
                            shell.splash = SplashPhase::Gone;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            // Reveal the gate card immediately; the splash never returns mid-session.
            ConnectionStatus::Failed(_) => self.splash = SplashPhase::Gone,
            ConnectionStatus::Connecting => {}
        }
    }

    // ---- layout state ----

    fn sidebar_target(&self) -> f32 {
        if self.settings.sidebar_collapsed {
            0.0
        } else {
            self.sidebar_shown_width()
        }
    }

    /// The saved sidebar width clamped so the conversation column keeps
    /// `CHAT_PANEL_MIN` in this window. The saved preference is untouched,
    /// so widening the window restores it.
    fn sidebar_shown_width(&self) -> f32 {
        sidebar_width_for_viewport(self.settings.sidebar_width, self.viewport_width)
    }

    /// Does the selected space's folder have git? Owner-stamped and synced —
    /// gates the Changes pane, its toggle, and Cmd-B with zero RPCs.
    fn space_git_detected(&self, cx: &App) -> bool {
        self.state.read(cx).selected_space_git()
    }

    /// The current chat's changes-pane flag (per-session, in-memory), gated on
    /// the space having git at all: a stale per-chat open flag must not reopen
    /// the pane after switching into a non-git space.
    /// The per-session panel key. The new-chat canvas (no selection) keys per
    /// SPACE — one shared "" key made a canvas toggle read as global state
    /// (user report).
    fn panel_key(&self, cx: &App) -> String {
        if self.active_chat.is_empty() {
            let space = self
                .state
                .read(cx)
                .selected_space
                .clone()
                .unwrap_or_default();
            format!("space-canvas:{space}")
        } else {
            self.active_chat.clone()
        }
    }

    /// Whether the right pane shows. NOT gated on git any more: the pane is
    /// a surface HOST now (terminals work in any space), so only the Git
    /// surface rows check `space_git_detected`. Still hidden on the
    /// new-session canvas, where the titlebar carries no toggle to close it
    /// again (an earlier user request).
    fn right_pane_open(&self, cx: &App) -> bool {
        !self.active_chat.is_empty() && self.panels.get(&self.panel_key(cx)).changes_open
    }

    /// The current chat's terminal flag (per-session, in-memory).
    fn terminal_open(&self, cx: &App) -> bool {
        self.panels.get(&self.panel_key(cx)).terminal_open
    }

    fn right_target(&self, cx: &App) -> f32 {
        if !self.right_pane_open(cx) {
            0.0
        } else {
            // Manual sizing preserves a usable conversation column. Takeover
            // intentionally consumes it completely. Both ride the sidebar
            // tween so toggling it remains seamless.
            let sidebar_now = self.sidebar_now();
            if self.right_pane_expanded {
                right_pane_takeover_width(
                    self.viewport_width - self.files_reserved_width(cx),
                    sidebar_now,
                )
            } else {
                self.settings
                    .right_pane_width
                    .min(self.surface_max_width(cx))
            }
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let from = self.sidebar_now();
        self.sidebar_edge_bounce = None;
        self.sidebar_resize_edge = None;
        self.pane_resize_active = None;
        self.pane_resize_dragging = None;
        self.settings.sidebar_collapsed = !self.settings.sidebar_collapsed;
        self.sidebar_tween = Some(WidthTween::new(from, self.sidebar_target()));
        self.schedule_save(cx);
        cx.notify();
    }

    /// The user's pane toggle (titlebar button, keyboard). It drives only the
    /// surface host portion of the right pane: with just the explorer docked
    /// it opens the surface host beside it, and it never hides the explorer —
    /// only the explorer's own toggle undocks that portion.
    fn toggle_right_pane(&mut self, cx: &mut Context<Self>) {
        self.set_surfaces_open(!self.right_pane_open(cx), cx);
    }

    /// Closing the last surface tab closes the surface host; a docked
    /// explorer keeps the pane open on its own.
    fn collapse_surfaces_if_empty(&mut self, panel_key: &str, cx: &mut Context<Self>) {
        if panel_key == self.panel_key(cx)
            && self.right_tabs.get(panel_key).is_none_or(Vec::is_empty)
        {
            self.set_surfaces_open(false, cx);
        }
    }

    /// Show or hide the surface host portion of the right pane. A no-op when
    /// already in the requested state, so programmatic opens (a file, a
    /// browser link, a subagent chip) never close a pane the user has open.
    fn set_surfaces_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() || self.right_pane_open(cx) == open {
            return;
        }
        // Reverse from the visible width when toggled during an animation.
        let from = self.right_visible_width(cx);
        self.right_edge_bounce = None;
        self.right_resize_edge = None;
        self.finish_pane_resize(PaneResizeKind::Right);
        let sidebar_now = self.sidebar_now();
        let from_main = conversation_width(
            self.viewport_width - self.files_reserved_width(cx),
            sidebar_now,
            from,
        );
        let was_expanded = self.right_pane_expanded;
        let key = self.panel_key(cx);
        self.panels.update(&key, |p| p.changes_open = open);
        if !open {
            self.stop_tracker_sync();
            self.suspend_file_images(cx);
            // Closing always leaves takeover mode — reopening at full bleed
            // with the conversation gone read as a broken chat.
            self.right_pane_expanded = false;
        } else if self.resolved_right_active(cx) == RightSurface::Team {
            let chat_id = self.active_chat.clone();
            self.invalidate_tracker_review();
            self.load_tracker_project(&chat_id, false, cx);
            self.watch_tracker_sync(&chat_id, cx);
        }
        let to = self.right_target(cx);
        self.right_tween = Some(WidthTween::new(from, to));
        self.right_takeover_content_tween = None;
        self.main_takeover_tween = was_expanded.then(|| {
            WidthTween::new(
                from_main,
                conversation_width(
                    self.viewport_width - self.files_reserved_width(cx),
                    sidebar_now,
                    to,
                ),
            )
        });
        if open
            && let RightSurface::Diff(id) = self.resolved_right_active(cx)
            && let Some(changes) = self.diffs.get(&id).cloned()
        {
            // Reopening onto a diff tab revalidates its watch.
            changes.update(cx, |changes, cx| changes.ensure_content(cx));
        }
        cx.notify();
    }

    fn sync_status_light(&mut self, cx: &mut Context<Self>) {
        let shown = self
            .settings
            .live_app_icon
            .then(|| crate::status_light::summarize_state(self.state.read(cx), Utc::now()));
        if self.status_light != shown {
            self.status_light = shown;
            cx.notify();
        }
        let target = shown.map_or(
            crate::status_light::DockTarget::Default,
            crate::status_light::DockTarget::Live,
        );
        let now = std::time::Instant::now();
        if let Some(ops) = self
            .dock_sync
            .observe(target, now)
            .or_else(|| self.dock_sync.take_due(now))
        {
            crate::status_light::apply(ops);
        }
        self.sync_menu_bar(false, cx);
    }

    /// The Mast logo, lit by the status light unless "Live app icon" is off.
    fn logo(&self, appearance: crate::theme::Appearance) -> gpui::Img {
        icons::logo_lit(appearance, self.status_light.map(|summary| summary.light))
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.git_history_columns = crate::history::configured_columns(cx);
        self.settings.git_history_column_widths = crate::history::configured_column_widths(cx);
        self.settings.git_history_column_order = crate::history::configured_column_order(cx);
        self.settings.git_history_author_display = crate::history::configured_author_display(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.sync_independent_settings(cx);
        settings::replace(self.settings.clone(), SavePolicy::Debounced, cx);
    }

    /// Controls outside the Shell mutate these choices directly. A geometry
    /// save must never publish the Shell's older values over those selections.
    /// The typography globals own the font choices but persist every change
    /// immediately, so the central store is an equally canonical read and
    /// keeps this block on a single source.
    fn sync_independent_settings(&mut self, cx: &App) {
        let current = settings::current(cx);
        self.settings.window_geometry = current.window_geometry;
        self.settings.new_thread_composer_background = current.new_thread_composer_background;
        self.settings.new_thread_background_effect = current.new_thread_background_effect;
        self.settings.open_web_links_in_mast = current.open_web_links_in_mast;
        self.settings.ui_font_family = current.ui_font_family;
        self.settings.ui_font_size = current.ui_font_size;
        self.settings.ui_style = current.ui_style;
        self.settings.dismissed_help_cards = current.dismissed_help_cards.clone();
        self.settings.terminal_font_family = current.terminal_font_family;
        self.settings.terminal_font_size = current.terminal_font_size;
        self.settings.code_font_family = current.code_font_family;
        self.settings.code_font_size = current.code_font_size;
        self.settings.transcript_width = current.transcript_width;
    }

    fn retry_engine(&mut self, cx: &mut Context<Self>) {
        AppState::bootstrap(self.state.clone(), self.boot.clone(), cx);
    }

    // ---- routes / settings ----

    /// Close the user menu through the exit animation (no-op when closed).
    fn close_user_menu(&mut self, cx: &mut Context<Self>) {
        if self.user_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.user_menu);
            cx.notify();
        }
    }

    /// Close the session-row context menu through the exit animation.
    fn close_chat_menu(&mut self, cx: &mut Context<Self>) {
        if self.chat_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.chat_menu);
            cx.notify();
        }
    }

    fn open_chat_copy_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.chat_menu.open_mut() {
            menu.page = ChatMenuPage::Copy;
            cx.notify();
        }
    }

    fn copy_mast_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = {
            let state = self.state.read(cx);
            crate::links::workspace_locator(
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
            .map(|workspace| crate::links::zeron_conversation_link(chat_id, &workspace))
        };
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link));
            self.sidebar_notice = Some("Mast conversation link copied".into());
        } else {
            self.sidebar_notice = Some("Conversation link is not ready yet".into());
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(crate::links::harness_conversation_link);
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link.url));
            self.sidebar_notice = Some(format!("{} copied", link.label).into());
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_session_id(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.harness_session_id.clone());
        if let Some(id) = id.filter(|id| !id.trim().is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(id));
            self.sidebar_notice = Some("Harness session ID copied".into());
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Copy `<title> — chat <id> — session <harness session id>` for
    /// debugging ([`tabs::chat_identity_string`]). From the sidebar menu the
    /// feedback is the sidebar notice (like its sibling copy rows); from the
    /// header button the icon flips to a check for ~1.2s (the code-block
    /// "Copied" idiom).
    pub(crate) fn copy_chat_identity(
        &mut self,
        chat_id: &str,
        from_menu: bool,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map(tabs::chat_identity_for);
        let Some(text) = text else {
            if from_menu {
                self.close_chat_menu(cx);
            }
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if from_menu {
            self.sidebar_notice = Some("Chat name + IDs copied".into());
            self.close_chat_menu(cx);
        } else {
            self.copied_chat_identity = Some(chat_id.to_string());
            self.copied_chat_identity_clear = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1200))
                    .await;
                this.update(cx, |this, cx| {
                    this.copied_chat_identity = None;
                    this.copied_chat_identity_clear = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        cx.notify();
    }

    fn ensure_chat_header_links(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let checkout = self.state.read(cx).selected_checkout_key(&chat_id);
        if self.chat_header_links.get(&chat_id).is_some_and(|entry| {
            entry.checkout == checkout && entry.fetched_at.elapsed() < CHAT_HEADER_LINK_REFRESH
        }) || self.chat_header_links_pending.contains(&chat_id)
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.chat_header_links_pending.insert(chat_id.clone());
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_LINK_STATUS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.chat_header_links_pending.remove(&chat_id);
                if this.state.read(cx).selected_checkout_key(&chat_id) != checkout {
                    cx.notify();
                    return;
                }
                if let Some(status) = result
                    .ok()
                    .and_then(|value| serde_json::from_value::<ChatLinkStatus>(value).ok())
                {
                    crate::review_chats::note_chat_links(
                        cx,
                        &chat_id,
                        &crate::review_chats::links_of_status(&status),
                    );
                    this.board_links_revision = this.board_links_revision.wrapping_add(1);
                    this.board_link_versions.insert(chat_id.clone(), this.board_links_revision);
                    this.chat_header_links.insert(
                        chat_id,
                        ChatHeaderLinkCache {
                            checkout,
                            status: Some(status),
                            fetched_at: Instant::now(),
                        },
                    );
                    if this.right_pane_open(cx) && this.resolved_right_active(cx) == RightSurface::Team {
                        let active_chat = this.active_chat.clone();
                        this.load_tracker_health(&active_chat, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read an already cached PR head; never starts a request.
    pub(crate) fn cached_review_head(&self, chat_id: &str, pr: &mast_proto::ReviewPullRequest) -> Option<String> {
        let status = self.chat_header_links.get(chat_id)?.status.as_ref()?;
        crate::artifacts::model::cached_review_head(status, pr).map(str::to_owned)
    }

    pub(crate) fn invalidate_chat_header_links(&mut self, chat_id: &str) {
        self.chat_header_links.remove(chat_id);
        self.board_links_revision = self.board_links_revision.wrapping_add(1);
        self.board_link_versions.insert(chat_id.to_owned(), self.board_links_revision);
    }

    fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.command_palette = None;
        // Recreate per visit: the page's ListHarnesses load re-probes which
        // CLIs are installed, so installing one shows up on the next open.
        if section == SettingsSection::Harnesses {
            self.harnesses_page = None;
        }
        if section == SettingsSection::Agents
            && let Some(page) = &self.accounts_page {
                page.update(cx, |page, cx| page.refresh_managed_accounts(cx));
            }
        self.route = Route::Settings(section);
        self.nav.push(NavEntry::Settings(section));
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Whether the overview is the route on screen right now — lets the
    /// overview's own idle-refresh timer skip ticks while it's mounted but
    /// hidden behind another route.
    pub(crate) fn is_overview_route(&self) -> bool {
        matches!(self.route, Route::Overview)
    }

    /// The PR page is on screen (its refresh timer skips ticks otherwise).
    pub(crate) fn is_pr_review_chats_route(&self) -> bool {
        matches!(self.route, Route::PrReviewChats)
    }

    /// The overview's "View full chat →": select `chat_id` so the shared
    /// transcript/composer bind to it, WITHOUT leaving `Route::Overview`.
    /// Sending, queueing, the question wizard, interrupt and resume all key
    /// off the selection, not the route. No history entry: the nav push in
    /// `on_state_changed` is gated on `Route::Chat`.
    pub(crate) fn preview_chat_in_overview(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.show_chat_in_overview_panel(chat_id, cx);
        self.focus_composer(cx);
    }

    /// [`Self::preview_chat_in_overview`] without focusing the composer: the
    /// Outline's preview-on-select, which keeps the keyboard in the tree.
    /// The selection change is the same one the panel makes, so the Chat is
    /// marked seen exactly as viewing it there marks it.
    pub(crate) fn show_chat_in_overview_panel(&mut self, chat_id: String, cx: &mut Context<Self>) {
        // A focus request still pending from before the panel mounted the
        // composer (its first render, a picker closing) would land on that
        // render and take the keyboard from the tree.
        self.composer
            .update(cx, |composer, _| composer.focus_pending = false);
        if self.state.read(cx).selected_chat.as_deref() != Some(chat_id.as_str()) {
            self.state
                .update(cx, |s, cx| s.select_chat(Some(chat_id), cx));
        }
        cx.notify();
    }

    /// Something the overview's chat panel triggered (subagent tab, file or
    /// web link) opens in the right pane, which only exists on the chat
    /// route — go there first so it isn't opened somewhere nobody can see.
    fn leave_overview_for_chat(&mut self, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Overview) {
            self.route = Route::Chat;
            self.nav.push(NavEntry::Chat(self.active_chat.clone()));
            cx.notify();
        }
    }

    pub(crate) fn open_overview(&mut self, cx: &mut Context<Self>) {
        self.command_palette = None;
        self.route = Route::Overview;
        self.nav.push(NavEntry::Overview);
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the overview entity on first visit, same pattern as
    /// `settings_outlet`'s per-section pages.
    fn overview_outlet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_overview_page(cx).into_any_element()
    }

    fn ensure_overview_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::overview::Overview> {
        if let Some(page) = &self.overview_page {
            return page.clone();
        }
        let state = self.state.clone();
        let weak = cx.entity().downgrade();
        // The overview's chat panel renders these SAME entities (never a
        // second transcript/composer) — safe because routes are exclusive:
        // `render_main` returns before the chat stack on this route.
        let transcript = self.transcript.clone();
        let composer = self.composer.clone();
        let page =
            cx.new(|cx| crate::overview::Overview::new(state, weak, transcript, composer, cx));
        self.overview_page = Some(page.clone());
        page
    }

    pub(crate) fn open_pr_review_chats(&mut self, cx: &mut Context<Self>) {
        self.command_palette = None;
        self.route = Route::PrReviewChats;
        self.nav.push(NavEntry::PrReviewChats);
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    pub(crate) fn open_pr_review_chats_focused(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_pr_review_chats(cx);
        let page = self.ensure_pr_review_chats_page(cx);
        let focus = page.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    fn ensure_pr_review_chats_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::chat_type_view::ChatTypeView> {
        if let Some(page) = &self.pr_review_chats_page {
            return page.clone();
        }
        let state = self.state.clone();
        let shell = cx.entity().downgrade();
        let page = cx.new(|cx| {
            crate::chat_type_view::ChatTypeView::new(
                state,
                shell,
                crate::chat_type_view::ChatViewKind::PrReview,
                cx,
            )
        });
        self.pr_review_chats_page = Some(page.clone());
        page
    }

    fn pr_review_chats_outlet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_pr_review_chats_page(cx).into_any_element()
    }

    /// Chat menu "Add PR / ticket…": the link editor lives in the
    /// overview's expanded detail, so switch there and open it for this
    /// chat (expanded, focused, scrolled/framed into view).
    fn link_chat_from_menu(
        &mut self,
        chat_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.open_overview(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.open_link_editor(chat_id, true, window, cx)
        });
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.focus_composer(cx);
        self.nav.push(NavEntry::Chat(self.active_chat.clone()));
        cx.notify();
    }

    // ---- back/forward (route history) ----

    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
        self.suspend_file_images(cx);
        match entry {
            NavEntry::Chat(chat_id) => {
                self.route = Route::Chat;
                self.focus_composer(cx);
                let target = (!chat_id.is_empty()).then_some(chat_id);
                if self.state.read(cx).selected_chat != target {
                    self.state.update(cx, |s, cx| s.select_chat(target, cx));
                }
            }
            NavEntry::Settings(section) => {
                self.route = Route::Settings(section);
            }
            NavEntry::Overview => {
                self.route = Route::Overview;
            }
            NavEntry::PrReviewChats => {
                self.route = Route::PrReviewChats;
            }
        }
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the entity for a settings section and return it renderable.
    fn settings_outlet(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match section {
            SettingsSection::Devices => {
                if self.devices_page.is_none() {
                    let state = self.state.clone();
                    self.devices_page = Some(cx.new(|cx| DevicesPage::new(state, cx)));
                }
                match &self.devices_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Harnesses => {
                if self.harnesses_page.is_none() {
                    let state = self.state.clone();
                    self.harnesses_page = Some(cx.new(|cx| HarnessesPage::new(state, cx)));
                }
                match &self.harnesses_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Agents => {
                if self.accounts_page.is_none() {
                    let state = self.state.clone();
                    self.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
                }
                match &self.accounts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Appearance => {
                if self.appearance_page.is_none() {
                    let page = cx.new(AppearancePage::new);
                    self.appearance_settings_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &AppearanceSettingsEvent, cx| match *event {
                            AppearanceSettingsEvent::CompactSingleChatChanged(value) => {
                                this.settings.compact_single_chat = value;
                                if let Some(overview) = &this.overview_page {
                                    overview.update(cx, |overview, cx| overview.set_compact_single_chat(value, cx));
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            AppearanceSettingsEvent::TeamTabVisibilityChanged(value) => {
                                this.settings.team_tab_visibility = value;
                                if value == crate::settings::TeamTabVisibility::Hide && this.resolved_right_active(cx) == RightSurface::Team {
                                    this.set_right_active(RightSurface::Item, cx);
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            AppearanceSettingsEvent::CodeFontSizeChanged(size) => {
                                this.set_code_font_size(size, cx);
                            }
                        },
                    ));
                    self.appearance_page = Some(page);
                }
                match &self.appearance_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Developer => {
                let page = self.design_page.get_or_insert_with(|| cx.new(crate::settings::design_panel::DesignPanel::new));
                page.clone().into_any_element()
            }
            SettingsSection::Files => {
                if self.files_settings_page.is_none() {
                    let page = cx.new(|cx| {
                        FilesSettingsPage::new(
                            self.settings.files_autosave_enabled,
                            self.settings.files_autosave_delay_ms,
                            self.settings.files_word_wrap,
                            self.settings.files_show_all,
                            cx,
                        )
                    });
                    self.files_settings_sub = Some(cx.subscribe_in(
                        &page,
                        window,
                        |this: &mut Shell, _, event: &FilesSettingsEvent, window, cx| match *event {
                            FilesSettingsEvent::AutosaveChanged(autosave_enabled) => {
                                this.settings.files_autosave_enabled = autosave_enabled;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_enabled(autosave_enabled, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::AutosaveDelayChanged(autosave_delay_ms) => {
                                this.settings.files_autosave_delay_ms = autosave_delay_ms;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_delay_ms(autosave_delay_ms, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::WordWrapChanged(word_wrap) => {
                                this.set_files_word_wrap(word_wrap, window, cx);
                            }
                            FilesSettingsEvent::ShowAllFilesChanged(show_all_files) => {
                                this.set_files_show_all(show_all_files, cx);
                            }
                        },
                    ));
                    self.files_settings_page = Some(page);
                }
                match &self.files_settings_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Notifications => {
                if self.notifications_page.is_none() {
                    let page = cx.new(|cx| {
                        NotificationsPage::new(
                            self.settings.sound_enabled,
                            self.settings.sound_completion_enabled,
                            self.settings.sound_input_enabled,
                            self.settings.sound_attention_enabled,
                            self.settings.notifications_enabled,
                            self.settings.notifications_background_only,
                            self.settings.live_app_icon,
                            self.settings.show_menu_bar_item,
                            self.settings.menu_bar_needs_you,
                            cx,
                        )
                    });
                    // Persist the flags whenever the page flips one.
                    self.notifications_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &NotificationsEvent, cx| {
                            let NotificationsEvent::Changed {
                                sound,
                                completion_sound,
                                input_sound,
                                attention_sound,
                                desktop,
                                background_only,
                                live_app_icon,
                                menu_bar_item,
                                menu_bar_needs_you,
                            } = *event;
                            this.settings.sound_enabled = sound;
                            this.settings.sound_completion_enabled = completion_sound;
                            this.settings.sound_input_enabled = input_sound;
                            this.settings.sound_attention_enabled = attention_sound;
                            this.settings.notifications_enabled = desktop;
                            this.settings.notifications_background_only = background_only;
                            this.settings.live_app_icon = live_app_icon;
                            this.settings.show_menu_bar_item = menu_bar_item;
                            this.settings.menu_bar_needs_you = menu_bar_needs_you;
                            this.sync_status_light(cx);
                            this.sync_menu_bar(true, cx);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.notifications_page = Some(page);
                }
                match &self.notifications_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Shortcuts | SettingsSection::Appshots => {
                if self.shortcuts_page.is_none() {
                    let state = self.state.clone();
                    let keymap = self.settings.keymap.clone();
                    let escape_stops_active_agent = self.settings.escape_stops_active_agent;
                    let composer_send_behavior = self.settings.composer_send_behavior;
                    let appshots_enabled = self.settings.appshots_enabled;
                    let appshot_sound_enabled = self.settings.appshot_sound_enabled;
                    let appshot_destination = self.settings.appshot_destination;
                    let page = cx.new(|cx| {
                        ShortcutsPage::new(
                            state,
                            keymap,
                            escape_stops_active_agent,
                            composer_send_behavior,
                            appshots_enabled,
                            appshot_sound_enabled,
                            appshot_destination,
                            cx,
                        )
                    });
                    // Persist + re-apply shortcut preferences whenever the page changes them.
                    self.shortcuts_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &ShortcutsEvent, cx| {
                            match event {
                                ShortcutsEvent::KeymapChanged(keymap) => {
                                    this.settings.keymap = keymap.clone();
                                }
                                ShortcutsEvent::EscapeStopsActiveAgentChanged(enabled) => {
                                    this.settings.escape_stops_active_agent = *enabled;
                                }
                                ShortcutsEvent::ComposerSendBehaviorChanged(behavior) => {
                                    this.settings.composer_send_behavior = *behavior;
                                }
                                ShortcutsEvent::AppshotsChanged {
                                    enabled,
                                    sound_enabled,
                                    destination,
                                } => {
                                    this.settings.appshots_enabled = *enabled;
                                    this.settings.appshot_sound_enabled = *sound_enabled;
                                    crate::appshots::set_capture_sound_enabled(*sound_enabled);
                                    this.settings.appshot_destination = *destination;
                                    crate::appshots::set_enabled(*enabled);
                                }
                            }
                            apply_keymap(
                                cx,
                                &this.settings.keymap,
                                this.settings.composer_send_behavior,
                            );
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.shortcuts_page = Some(page);
                }
                match &self.shortcuts_page {
                    Some(page) => {
                        page.update(cx, |page, _| {
                            page.show_appshots(section == SettingsSection::Appshots)
                        });
                        page.clone().into_any_element()
                    }
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Linear => self.linear_settings_outlet(cx),
            SettingsSection::Archived => {
                if self.archived_page.is_none() {
                    let state = self.state.clone();
                    self.archived_page = Some(cx.new(|cx| ArchivedPage::new(state, cx)));
                }
                match &self.archived_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::WorkflowPack => {
                if self.workflow_pack_page.is_none() {
                    let state = self.state.clone();
                    self.workflow_pack_page =
                        Some(cx.new(|cx| crate::settings::workflow_pack::WorkflowPackPage::new(state, cx)));
                }
                match &self.workflow_pack_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
        }
    }

    // ---- sidebar mutations ----

    /// Fire a Mutate op; failures surface in the sidebar notice strip.
    fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.mutations.push(params.clone());
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    pub(crate) fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let current = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            ComposerInput::new("Session title", cx).with_accessibility_role(gpui::Role::TextInput)
        });
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_chat(cx);
            }
        });
        self.rename_dialog = Some(RenameChatDialog {
            chat_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename_chat(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_dialog.take() else {
            return;
        };
        let title = dialog.input.read(cx).text().trim().to_string();
        if !title.is_empty() {
            self.persist_chat_title(dialog.chat_id, title, cx);
        }
        cx.notify();
    }

    /// Rename through the single durable `renameChat` mutation while updating
    /// the shared chat row optimistically. Sidebar, full-chat titlebar, Overview,
    /// and every other title consumer repaint from that same row immediately.
    fn persist_chat_title(&mut self, chat_id: String, title: String, cx: &mut Context<Self>) {
        let previous = self
            .state
            .update(cx, |state, cx| {
                let previous = state.replace_chat_title(&chat_id, Some(title.clone()));
                cx.notify();
                previous
            })
            .flatten();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.state.update(cx, |state, cx| {
                state.replace_chat_title(&chat_id, previous);
                cx.notify();
            });
            self.sidebar_notice = Some("Engine not connected".into());
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::MUTATE,
                    serde_json::json!({ "op": "renameChat", "chatId": chat_id, "title": title }),
                )
                .await;
            if let Err(err) = result {
                this.update(cx, |shell, cx| {
                    // Do not roll a newer local/remote rename backward.
                    shell.state.update(cx, |state, cx| {
                        let still_optimistic = state
                            .chats
                            .iter()
                            .find(|chat| chat.id == chat_id)
                            .and_then(|chat| chat.title.as_deref())
                            == Some(title.as_str());
                        if still_optimistic {
                            state.replace_chat_title(&chat_id, previous);
                            cx.notify();
                        }
                    });
                    shell.sidebar_notice = Some(format!("Couldn’t rename chat: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    /// Catch an imported chat up on new messages its source session
    /// accumulated since the last import/sync (`SyncExternalSession`).
    /// Only meaningful for a chat that came from the external-session import
    /// flow — for any other chat the RPC just fails server-side (no import
    /// cursor recorded), which we surface as a plain notice rather than
    /// trying to predict client-side which chats qualify.
    fn sync_external_session(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sidebar_notice = Some("Syncing…".into());
        cx.notify();
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SYNC_EXTERNAL_SESSION,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                shell.sidebar_notice = Some(match result {
                    Ok(value) => {
                        let new_count = value
                            .get("newMessageCount")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        if new_count == 0 {
                            "No new messages".into()
                        } else if new_count == 1 {
                            "Synced — 1 new message".into()
                        } else {
                            format!("Synced — {new_count} new messages").into()
                        }
                    }
                    Err(err) => format!("Sync failed: {err}").into(),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// One-time migration: import every Claude Code session `session_canvas`
    /// (the `agent-mode-tools` stopgap tool this overview effort replaces)
    /// still has on disk, carrying over its archive state so already-dead
    /// sessions don't flood the new overview as active chats
    /// (`BulkImportSessionCanvasSessions`). Streamed — this can process
    /// hundreds of files — progress rides `sidebar_notice` rather than a
    /// dedicated progress surface, matching `sync_external_session`'s
    /// transient-feedback convention.
    pub(crate) fn bulk_import_session_canvas_sessions(&mut self, cx: &mut Context<Self>) {
        if self.bulk_import_task.is_some() {
            return; // one run at a time
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sidebar_notice = Some("Importing session_canvas sessions…".into());
        cx.notify();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(
                    methods::BULK_IMPORT_SESSION_CANVAS_SESSIONS,
                    serde_json::json!({}),
                )
                .await
                .map_err(|error| error.to_string())?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        self.bulk_import_task = Some(cx.spawn(async move |this, cx| {
            let _stream = stream; // keep the subscribing task alive until the loop below ends
            loop {
                let Some(item) = rx.recv().await else {
                    this.update(cx, |shell, cx| {
                        shell.bulk_import_task = None;
                        // A stream that ends without a summary item (dropped
                        // connection, engine restart mid-run) is a failure —
                        // idempotent, so just say so; re-running picks up
                        // wherever it left off.
                        shell.sidebar_notice =
                            Some("Bulk import stream ended before finishing.".into());
                        cx.notify();
                    })
                    .ok();
                    return;
                };
                let is_summary =
                    item.get("kind").and_then(serde_json::Value::as_str) == Some("summary");
                let ended = this
                    .update(cx, |shell, cx| {
                        match item.get("kind").and_then(serde_json::Value::as_str) {
                            Some("item") => {
                                let index = item
                                    .get("index")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0)
                                    + 1;
                                let total = item
                                    .get("total")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0);
                                shell.sidebar_notice =
                                    Some(format!("Importing {index} of {total}…").into());
                            }
                            Some("summary") => {
                                let get_u64 = |k: &str| {
                                    item.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0)
                                };
                                let imported = get_u64("imported");
                                let archived = get_u64("archived");
                                let failed = get_u64("failed");
                                shell.sidebar_notice = Some(
                                    format!(
                                        "Imported {imported} ({archived} archived, {failed} failed)"
                                    )
                                    .into(),
                                );
                                shell.bulk_import_task = None;
                            }
                            _ => {}
                        }
                        cx.notify();
                    })
                    .is_err();
                if ended || is_summary {
                    return;
                }
            }
        }));
    }

    fn reconcile_sidebar_pins(&mut self, cx: &mut Context<Self>) {
        self.discard_stale_sidebar_pin_writes(cx);
        self.migrate_sidebar_sections(cx);
        let Some(profile_key) = self.active_sidebar_pin_profile_key(cx) else {
            return;
        };
        let state = self.state.read(cx);
        if state.workspace_scope == Some(WorkspaceScope::Local) {
            if !state.chats_synced {
                return;
            }
            let known = state.chats.iter().map(|chat| chat.id.clone()).collect();
            let changed = self
                .settings
                .sidebar_pinned_session_ids_by_profile
                .get_mut(&profile_key)
                .is_some_and(|pins| spaces::retain_known_pins(pins, &known));
            if changed {
                self.schedule_save(cx);
            }
        }
        // Remote pins come exclusively from per-pin registry records.
    }

    fn active_sidebar_pin_profile_key(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        sidebar_pin_profile_key(
            state.workspace_scope,
            state.auth.as_ref(),
            self.boot.org_id.as_deref(),
        )
    }

    fn active_sidebar_pins(&self, cx: &App) -> Vec<String> {
        let mut pins = self.raw_sidebar_pins(cx);
        let sections = self.active_sidebar_sections(cx);
        pins.retain(|id| {
            !sections
                .iter()
                .any(|section| section.session_ids.contains(id))
        });
        pins
    }

    fn raw_sidebar_pins(&self, cx: &App) -> Vec<String> {
        if let Some(pins) = self.optimistic_sidebar_pins(cx) {
            return pins;
        }
        let state = self.state.read(cx);
        match state.workspace_scope {
            Some(WorkspaceScope::Local) => self
                .active_sidebar_pin_profile_key(cx)
                .map(|key| self.settings.sidebar_pins(&key).to_vec())
                .unwrap_or_default(),
            Some(WorkspaceScope::Synced | WorkspaceScope::Development) => {
                state.sidebar_preferences.pinned_session_ids.clone()
            }
            None => Vec::new(),
        }
    }

    fn sidebar_pins_for_profile(&self, profile_key: &str, cx: &App) -> Vec<String> {
        if self.active_sidebar_pin_profile_key(cx).as_deref() != Some(profile_key) {
            return Vec::new();
        }
        self.active_sidebar_pins(cx)
    }

    fn validate_sidebar_pin_change(
        &mut self,
        profile_key: &str,
        pins: &[String],
        cx: &mut Context<Self>,
    ) -> bool {
        if self.active_sidebar_pin_profile_key(cx).as_deref() != Some(profile_key) {
            return false;
        }
        let state = self.state.read(cx);
        let remote = matches!(
            state.workspace_scope,
            Some(WorkspaceScope::Synced | WorkspaceScope::Development)
        );
        let result = if remote && !state.sidebar_preferences.can_edit() {
            Err("Pins are still syncing")
        } else {
            let current = self.active_sidebar_pins(cx);
            mast_proto::validate_sidebar_pin_update(&current, pins)
        };
        if let Err(message) = result {
            self.sidebar_notice = Some(message.into());
            cx.notify();
            return false;
        }
        true
    }

    fn apply_sidebar_pin_change(
        &mut self,
        profile_key: String,
        change: mast_proto::SidebarPinChange,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut pinned_session_ids = self.raw_sidebar_pins(cx);
        change.project(&mut pinned_session_ids);
        if !self.validate_sidebar_pin_change(&profile_key, &pinned_session_ids, cx)
            || self.raw_sidebar_pins(cx) == pinned_session_ids
        {
            return false;
        }
        match self.state.read(cx).workspace_scope {
            Some(WorkspaceScope::Local) => {
                if pinned_session_ids.is_empty() {
                    self.settings
                        .sidebar_pinned_session_ids_by_profile
                        .remove(&profile_key);
                } else {
                    self.settings
                        .sidebar_pinned_session_ids_by_profile
                        .insert(profile_key, pinned_session_ids);
                }
                self.schedule_save(cx);
            }
            Some(WorkspaceScope::Synced | WorkspaceScope::Development) => {
                return self.queue_sidebar_pin_write(profile_key, change, cx);
            }
            None => return false,
        }
        true
    }

    fn set_chat_pinned(&mut self, chat_id: String, pinned: bool, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        self.cancel_pinned_session_drag(cx);
        let Some(profile_key) = self.active_sidebar_pin_profile_key(cx) else {
            return;
        };
        if !self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == chat_id)
        {
            return;
        }
        let pins = self.active_sidebar_pins(cx);
        if pins.contains(&chat_id) == pinned {
            return;
        }
        if pinned && self.raw_sidebar_pins(cx).contains(&chat_id) {
            self.assign_sidebar_section(&chat_id, None, cx);
            cx.notify();
            return;
        }
        let change = if pinned {
            mast_proto::SidebarPinChange::Pin {
                session_id: chat_id.clone(),
                after: pins.last().cloned(),
                before: None,
            }
        } else {
            mast_proto::SidebarPinChange::Unpin {
                session_id: chat_id.clone(),
            }
        };
        if self.apply_sidebar_pin_change(profile_key, change, cx)
            && pinned
            && self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local)
        {
            self.assign_sidebar_section(&chat_id, None, cx);
        }
        cx.notify();
    }

    /// The Archive session shortcut. With no chat open, or with an already
    /// archived one, it does nothing — the shortcut archives, it never
    /// unarchives.
    fn archive_selected_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self
            .state
            .read(cx)
            .archivable_selected_chat()
            .map(str::to_string)
        else {
            return;
        };
        self.archive_chat(chat_id, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": archived }),
            cx,
        );
        cx.notify();
    }

    /// A jump shortcut: open the sidebar row at `slot`. A slot past the end of
    /// a short list does nothing. Reads the DISPLAYED order — sort and
    /// grouping view options permute the list, and the chip on a row must
    /// name the key that opens it.
    fn jump_to_session(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(chat_id) = self.sidebar_visible_order(cx).into_iter().nth(slot) else {
            return;
        };
        // Same path a click on that row takes.
        self.open_chat(chat_id, cx);
    }

    /// Whether an overlay that owns the keyboard is up — the add-space
    /// palette or a composer picker popover (model selector, traits, repo,
    /// branch…). Session-nav shortcuts (cycle/jump/archive) go quiet
    /// underneath one: gpui runs a matched binding before any `on_key_down`,
    /// so an unguarded jump would switch sessions UNDER the open popover,
    /// stranding it over a session the user never picked.
    pub(super) fn overlay_owns_keyboard(&self, cx: &App) -> bool {
        self.command_palette.is_some()
            || self.workspace_search.is_some()
            || self.section_dialog.is_some()
            || self.section_menu.is_some()
            || self.running.open
            || self.add_space.is_some()
            || self.work_outline_modal_open()
            || self.work_outline_menu.is_open()
            || self.composer.read(cx).pickers().read(cx).is_open()
    }

    /// Track held modifiers for sidebar jump hints and the queue's submit hint.
    /// Only a visibility change repaints; modifier traffic is otherwise constant.
    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        self.update_jump_hints(&event.modifiers, cx);
    }

    fn update_jump_hints(&mut self, mods: &gpui::Modifiers, cx: &mut Context<Self>) {
        let primary = if cfg!(target_os = "macos") {
            mods.platform
        } else {
            mods.control
        };
        // No hints while an overlay owns the keyboard — the jumps they
        // advertise are suppressed there.
        let visible = matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard(cx)
            && jump_hints_visible(&self.settings.keymap, primary, mods.alt, mods.shift);
        self.set_jump_hints(visible, cx);
        let queue_shortcut_revealed = matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard(cx)
            && modifier_send_hint_visible(primary, mods.alt, mods.shift);
        self.composer.update(cx, |composer, cx| {
            composer.set_queue_shortcut_revealed(queue_shortcut_revealed, cx)
        });
    }

    pub(super) fn set_jump_hints(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.jump_hints != visible {
            self.jump_hints = visible;
            cx.notify();
        }
    }

    fn delete_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        if let Some(tabs) = self.right_tabs.get(&chat_id) {
            for surface in tabs {
                if let RightSurface::Browser(id) = surface {
                    if let Some(browser) = self.browsers.remove(id) {
                        browser.update(cx, |browser, cx| browser.close(cx));
                    }
                    self.browser_subs.remove(id);
                }
            }
            let artifacts: Vec<u64> = tabs
                .iter()
                .filter_map(|surface| match surface {
                    RightSurface::Artifact(id) => Some(*id),
                    _ => None,
                })
                .collect();
            for id in artifacts {
                self.close_artifact_tab(id, cx);
            }
        }
        if self.state.read(cx).selected_chat.as_deref() == Some(chat_id.as_str()) {
            self.state.update(cx, |s, cx| s.select_chat(None, cx));
        }
        self.composer
            .update(cx, |composer, cx| composer.purge_chat(&chat_id, cx));
        self.mutate(
            serde_json::json!({ "op": "deleteChat", "chatId": chat_id }),
            cx,
        );
        cx.notify();
    }

    fn render_settings_nav(
        &mut self,
        section: SettingsSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_icon = |item: SettingsSection| match item {
            SettingsSection::Devices => icons::MONITOR,
            SettingsSection::Harnesses => icons::WIDGET,
            SettingsSection::Agents => icons::KEY_MINIMALISTIC,
            SettingsSection::Appearance => icons::TUNING,
            SettingsSection::Developer => icons::TUNING,
            SettingsSection::Files => icons::FOLDER,
            SettingsSection::Notifications => icons::BELL,
            SettingsSection::Linear => icons::CHECKLIST,
            SettingsSection::Shortcuts => icons::KEYBOARD,
            SettingsSection::Appshots => icons::MONITOR,
            SettingsSection::Archived => icons::ARCHIVE_MINIMALISTIC,
            SettingsSection::WorkflowPack => icons::DOCUMENT_ADD,
        };
        // Match the user's dragged sidebar width — the pane container clips to
        // it, so a hardcoded default here left hover washes stopping short of
        // the sidebar's right edge (user-reported). Device identity lives on
        // the Accounts page now — the one surface where the device matters.
        div()
            .w(px(self.sidebar_shown_width()))
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_col()
                    .child(
                        crate::components::shell_parts::settings_sidebar_heading(theme),
                    )
                    .child(
                        div().flex().flex_col().gap(px(2.0)).children(
                            SettingsSection::ALL
                                .into_iter()
                                .filter(|item| {
                                    *item != SettingsSection::Appshots
                                        || crate::appshots::is_desktop()
                                })
                                .map(|item| {
                                    let selected = item == section;
                                    crate::components::shell_parts::settings_sidebar_row(theme, SharedString::from(format!("settings-nav-{}", item.label())), selected)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_settings(item, cx)
                                        }))
                                        .child(
                                            icon(section_icon(item))
                                                .size(px(16.0))
                                                .text_color(theme.text_muted),
                                        )
                                        .child(SharedString::from(item.label()))
                                }),
                        ),
                    ),
            )
            // Back pinned to the bottom (zeron settings-sidebar.tsx).
            .child(
                div().px(px(Theme::SPACE_SM)).pb(px(12.0)).child(
                    crate::components::shell_parts::settings_sidebar_back(theme)
                        .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
                        .child(
                            // AltArrowLeft chevron (zeron settings-sidebar.tsx),
                            // not the straight history arrow.
                            icon(icons::ALT_ARROW_LEFT)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from("Back")),
                ),
            )
            .into_any_element()
    }

    /// One session row: context + status on line one, harness + title on line
    /// two, and source metadata below. Working uses the live thread glyph in
    /// the status corner. Click selects; right-click opens the context menu.
    #[allow(clippy::too_many_arguments)]
    fn render_connection_pill(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        use mast_proto::ConnectivityState as S;
        let conn = self.state.read(cx).connectivity.clone();
        let (label, glyph): (SharedString, AnyElement) = match conn.state {
            S::Disabled | S::Connected => return None,
            S::Offline => (
                "Offline — sends are saved".into(),
                crate::components::shell_parts::offline_indicator(theme),
            ),
            S::Reconnecting => (
                "Reconnecting…".into(),
                loaders::mini_mono_spinner(
                    "connection-spinner",
                    2.0,
                    theme.text_muted,
                    self.sidebar_pane.entity_id(),
                    cx,
                )
                .into_any_element(),
            ),
        };
        Some(crate::components::shell_parts::connection_line(theme, label, glyph))
    }

    fn render_update_strip(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = self.state.read(cx).update.clone()?;
        if !status.update_available {
            return None;
        }
        let latest = status.latest_version.clone()?;
        if self.update_dismissed.as_deref() == Some(latest.as_str()) {
            return None;
        }
        let desktop_update = self.install.supports_desktop_update();

        let (label, clickable): (SharedString, bool) = if desktop_update {
            match &self.update_flow {
                UpdateFlow::Idle => (format!("Update available — v{latest}").into(), true),
                UpdateFlow::Downloading => (format!("Downloading v{latest}…").into(), false),
                UpdateFlow::Ready(_) => ("Update ready — restart to apply".into(), true),
                UpdateFlow::Failed(message) => (format!("Update failed: {message}").into(), true),
            }
        } else {
            (
                format!("Update available — v{latest} · run `zeron update`").into(),
                true,
            )
        };
        let failed = matches!(self.update_flow, UpdateFlow::Failed(_));
        let mut strip = crate::components::shell_parts::update_strip(theme, label, failed, clickable);
        if clickable {
            strip = strip
                .on_click(cx.listener(move |this, _, _, cx| this.on_update_strip_click(cx)));
        }
        Some(strip.into_any_element())
    }

    /// Idle → download; Ready → swap + relaunch; Failed → retry; advisory
    /// installs → dismiss for this version.
    fn on_update_strip_click(&mut self, cx: &mut Context<Self>) {
        if !self.install.supports_desktop_update() {
            self.update_dismissed = self
                .state
                .read(cx)
                .update
                .as_ref()
                .and_then(|s| s.latest_version.clone());
            cx.notify();
            return;
        }
        match std::mem::replace(&mut self.update_flow, UpdateFlow::Idle) {
            UpdateFlow::Idle | UpdateFlow::Failed(_) => self.begin_update_download(cx),
            UpdateFlow::Downloading => self.update_flow = UpdateFlow::Downloading,
            UpdateFlow::Ready(staged) => self.apply_staged_update(staged, cx),
        }
    }

    /// Fetch the manifest and stage the new Mast desktop bundle under the data dir
    /// (tokio — reqwest); the strip flips to "restart to apply" when done.
    fn begin_update_download(&mut self, cx: &mut Context<Self>) {
        let edge_url = self.boot.edge_url.clone();
        let data_dir = self.data_dir.clone();
        let install = self.install.clone();
        self.update_flow = UpdateFlow::Downloading;
        let download = Tokio::spawn(cx, async move {
            let manifest = mast_update::fetch_latest(&edge_url).await?;
            install.stage_desktop(&edge_url, &manifest, &data_dir).await
        });
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let outcome = match download.await {
                Ok(Ok(staged)) => Ok(staged),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.update_flow = match outcome {
                    Ok(staged) => UpdateFlow::Ready(staged),
                    Err(message) => {
                        tracing::warn!(%message, "update download failed");
                        UpdateFlow::Failed(message.into())
                    }
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Swap the staged bundle over the installed one, arm the detached
    /// relauncher, and quit — the relauncher `open`s the new bundle once this
    /// process (and its engine lock / IPC port) is gone.
    fn apply_staged_update(&mut self, staged: PathBuf, cx: &mut Context<Self>) {
        if !self.prepare_exit(PendingExit::InstallUpdate(staged.clone()), cx) {
            return;
        }
        match self.install.apply_desktop(&staged) {
            Ok(()) => {
                crate::app_menus::quit_after_save(cx);
            }
            Err(err) => {
                tracing::error!(error = %err, "update apply failed");
                self.update_flow = UpdateFlow::Failed(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    /// Scope-aware sidebar identity and account menu. Local runtimes advertise
    /// their storage boundary and offer sync; synced runtimes offer sign-out.
    fn resize_handle<T>(
        &self,
        id: &'static str,
        kind: PaneResizeKind,
        marker: fn() -> T,
        reset: fn(&mut Shell, &mut Context<Shell>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div>
    where
        T: 'static,
    {
        let theme = Theme::of(cx);
        let fade_key = format!("pane-resize-{id}");
        let hover_highlight = motion::hover_blend(
            &fade_key,
            theme.border_strong.opacity(0.0),
            theme.border_strong,
        );
        let active = self.pane_resize_active == Some(kind);
        let constrained = self.pane_resize_dragging == Some(kind) && !active;
        let highlight = if constrained {
            theme.border_strong.opacity(0.0)
        } else if active {
            theme.border_strong
        } else {
            hover_highlight
        };
        let clear = highlight.opacity(0.0);
        let release_key = fade_key.clone();
        let release_out_key = fade_key.clone();
        div()
            .id(id)
            .absolute()
            .top(px(PANE_RESIZE_HITBOX_TOP))
            .bottom_0()
            .w(px(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0))
            .flex_none()
            .occlude()
            .cursor_col_resize()
            .on_hover(motion::hover_listener(fade_key))
            // Codex-style seam feedback: the existing 1px panel border stays
            // visible at rest; hover adds a stronger center highlight that
            // fades back into that border toward both ends.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(PANE_RESIZE_HITBOX_HALF_WIDTH))
                    .w(px(1.0))
                    .flex()
                    .flex_col()
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(clear, 0.0),
                        gpui::linear_color_stop(highlight, 1.0),
                    )))
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(highlight, 0.0),
                        gpui::linear_color_stop(clear, 1.0),
                    ))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.pane_resize_dragging = Some(kind);
                    this.pane_resize_active = Some(kind);
                    cx.notify();
                }),
            )
            .on_drag(marker(), |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, window, cx| {
                    if event.click_count == 2 {
                        reset(this, cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, window, _| {
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_out_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
    }

    /// Terminal panel dock at the main-column bottom: a 5px height-drag handle
    /// over the panel, the whole container height-animated 200 ms on toggle.
    fn render_terminal_container(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let target = self.terminal_target(cx);
        let tween = self.terminal_tween;
        if target <= 0.0 && tween.is_none() {
            return gpui::Empty.into_any_element();
        }
        // Defensive: an open flag needs its entity (and set_open) even if
        // toggle_terminal never created one.
        if self.terminal_open(cx) && self.terminal.is_none() {
            let panel = self.terminal_panel(cx);
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
        }
        let Some(panel) = self.terminal.clone() else {
            return gpui::Empty.into_any_element();
        };
        // The dock spans the main column's bottom: when the sidebar is fully
        // closed the column IS the window's left edge (and likewise the right
        // edge when the right pane is closed) — the panel's fill then carries
        // the CSD window's bottom corners.
        let window_corner = Self::window_corner_radius(window) > 0.0;
        {
            let bl = window_corner && self.sidebar_now() < 0.5;
            let br = window_corner && !self.right_pane_open(cx) && !self.files_panel_open(cx);
            panel.update(cx, |panel, cx| panel.set_window_corners(bl, br, cx));
        }
        let border = Theme::of(cx).border;
        let handle_key = "pane-resize-terminal-resize";
        let handle_hover = motion::hover_blend(
            handle_key,
            Theme::of(cx).border_strong.opacity(0.0),
            Theme::of(cx).border_strong,
        );
        let terminal_active = self.pane_resize_active == Some(PaneResizeKind::Terminal);
        let terminal_constrained =
            self.pane_resize_dragging == Some(PaneResizeKind::Terminal) && !terminal_active;
        let handle_highlight = if terminal_constrained {
            Theme::of(cx).border_strong.opacity(0.0)
        } else if terminal_active {
            Theme::of(cx).border_strong
        } else {
            handle_hover
        };
        let height = self.settings.terminal_height;

        let handle = div()
            .id("terminal-resize")
            .h(px(TERMINAL_RESIZE_HITBOX_HEIGHT))
            .w_full()
            .flex_none()
            .cursor_row_resize()
            .on_hover(motion::hover_listener(handle_key))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    .bg(handle_highlight),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.terminal_drag_anchor =
                        Some((f32::from(event.position.y), this.settings.terminal_height));
                    this.pane_resize_dragging = Some(PaneResizeKind::Terminal);
                    this.pane_resize_active = Some(PaneResizeKind::Terminal);
                    cx.notify();
                }),
            )
            .on_drag(TerminalResize, |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if event.click_count == 2 {
                        this.settings.terminal_height = TERMINAL_DEFAULT_HEIGHT;
                        this.schedule_save(cx);
                        cx.notify();
                    }
                    this.finish_pane_resize(PaneResizeKind::Terminal);
                    motion::set_hover(handle_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    this.finish_pane_resize(PaneResizeKind::Terminal);
                    motion::set_hover(handle_key, false, this.reduced_motion);
                    window.refresh();
                }),
            );

        // Fixed-height inner clipped by the animated container: content never
        // reflows mid-transition (same trick as the side panes). The handle
        // FLOATS over the panel's top edge (painted after, so it wins hit
        // testing) instead of stacking above it — stacked, its hitbox would read as
        // dead air between the seam and the tab bar (user report).
        let inner = div()
            .h(px(height))
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(panel))
            .child(handle.absolute().top_0().left_0().right_0());

        div()
            .w_full()
            .flex_none()
            .overflow_hidden()
            .border_t_1()
            .border_color(border)
            .h(px(self.eval_tween(tween, target)))
            .child(inner)
            .into_any_element()
    }

    /// Working indicator strip: gradient spinner + rotating flavour word (7s,
    /// seeded per chat) + elapsed, staleness-gated via [`Indicator`]; falls back
    /// to a "Sending…" bridge and then the engine mode line.
    fn render_status_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let state = self.state.read(cx);

        // Aligned with the composer column: centered, same max width, small
        // inner gutter (zeron's `mx-auto h-6 max-w-3xl px-2`).
        let strip = div()
            .h(px(Theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .w_full()
            .max_w(px(768.0))
            .mx_auto()
            .flex()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .px(px(Theme::SPACE_LG + 8.0))
            .text_size(crate::typography::ui_rems(11.0));

        let Some(chat_id) = state.selected_chat.clone() else {
            return strip.into_any_element();
        };
        let indicator = state.indicator_for(&chat_id, now);
        // Timer base: the freshest of the session row's turn start and the
        // in-flight send. During the send→ack window the row (if any) still
        // carries the PREVIOUS turn's start, and using it opened the timer at
        // the old turn's elapsed instead of 0:00.
        let started = state
            .session_for(&chat_id)
            .and_then(|s| s.started_at)
            .into_iter()
            .chain(state.pending_send_started(&chat_id, now))
            .max();
        let elapsed_secs = started
            .map(|t| now.signed_duration_since(t).num_seconds().max(0))
            .unwrap_or(0);
        let sending = self.composer.read(cx).is_sending();

        // Unused here since the Working loader moved into the transcript
        // (its trailer computes its own elapsed).
        let _ = elapsed_secs;
        match indicator {
            // The working loader lives in the TRANSCRIPT now, under the
            // streaming reply (user request) — the strip stays empty (its
            // reserved height still steadies the composer).
            Indicator::Working => strip.into_any_element(),
            // No label: the QuestionPanel right below IS the awaiting-input
            // surface — a strip caption above it was redundant (user request).
            Indicator::AwaitingInput => strip.into_any_element(),
            Indicator::Errored => strip
                .text_color(theme.danger)
                .child(SharedString::from("Run failed"))
                .into_any_element(),
            Indicator::None if sending => strip
                .child(loaders::gradient_spinner(
                    "sending-indicator",
                    &theme,
                    2.5,
                    cx.entity_id(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Sending…")),
                )
                .into_any_element(),
            Indicator::None => strip.into_any_element(),
        }
    }

    /// Right pane — the surface host (t3code RightPanelTabs): hidden by
    /// default, drag-resizable. Content is the ACTIVE surface — the Diff
    /// page (its options row + the lazy [`Changes`] viewer), workspace Files,
    /// an embedded terminal, or the surface picker when no tabs exist.
    fn render_signed_out_restart(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let runtime_change_label = if self.runtime_change_task.is_some() {
            "Stopping engine…"
        } else {
            "Retry local mode"
        };
        let card = div()
            .w(px(380.0))
            .px(px(32.0))
            .py(px(40.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .shadow_lg()
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .child(
                self.logo(theme.appearance)
                    .w(px(24.9))
                    .h(px(36.0)),
            )
            .child(
                div()
                    .mt(px(24.0))
                    .text_size(crate::typography::ui_rems(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Signed out")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .mb(px(24.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "Mast removed your credentials but could not finish closing the previous synced workspace. Retry before continuing in local mode.",
                    )),
            )
            .when_some(self.runtime_change_error.clone(), |card, error| {
                card.child(
                    div()
                        .mb(px(16.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(px(17.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(
                popover::btn_primary(&theme, runtime_change_label)
                    .id("signed-out-quit")
                    .when(self.runtime_change_task.is_some(), |button| {
                        button.opacity(0.6)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_local_runtime_transition(false, cx)
                    })),
            );

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::fade_in("signed-out-restart", card)),
            )
            .into_any_element()
    }


}

/// The sign-in gate's faint grid backdrop (zeron styles.css `.bg-grid`):
/// 44px hairlines at white 3.5%, with the radial mask approximated by edge
/// gradients back into the page background (gpui has no mask-image).
fn grid_backdrop(theme: &Theme) -> AnyElement {
    let line = crate::theme::hairline(0.035);
    let bg = theme.bg;
    const STEP: f32 = 44.0;
    const SPAN: f32 = 2640.0;
    let verticals = (1..(SPAN / STEP) as usize).map(|i| {
        div()
            .absolute()
            .left(px(i as f32 * STEP))
            .top_0()
            .bottom_0()
            .w(px(1.0))
            .bg(line)
    });
    let horizontals = (1..((SPAN * 0.75) / STEP) as usize).map(|i| {
        div()
            .absolute()
            .top(px(i as f32 * STEP))
            .left_0()
            .right_0()
            .h(px(1.0))
            .bg(line)
    });
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children(verticals)
        .children(horizontals)
        // Mask approximation: fade the grid back into the background toward
        // the window edges (the original masks to an ellipse at 50% / 40%).
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(120.0))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(260.0))
                .bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    270.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .into_any_element()
}

const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 36.0;
const WINDOWS_CAPTION_WIDTH: f32 = WINDOWS_CAPTION_BUTTON_WIDTH * 3.0;

/// Right padding for titlebar content: past the native Windows caption
/// cluster, or past zeron's own Linux caption buttons (10px edge inset +
/// the button row) when the layout puts any on the right.
fn titlebar_right_padding(is_windows: bool, linux_right_captions: usize, base: f32) -> f32 {
    base + if is_windows {
        WINDOWS_CAPTION_WIDTH
    } else if linux_right_captions > 0 {
        10.0 + caption_buttons_width(linux_right_captions)
    } else {
        0.0
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        perf_counts::SHELL_RENDERS.with(|c| c.set(c.get() + 1));
        self.ensure_diagram_observer(window, cx);
        #[cfg(feature = "startup-bench")]
        let _render_span = crate::startup_measurement::MeasurementSpan::new("shell");
        self.render_time = Some(std::time::Instant::now());
        if self.all_file_edits_flushed(cx)
            && let Some(action) = self.pending_exit.take()
        {
            let shell = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                if matches!(action, PendingExit::Quit) {
                    crate::app_menus::request_quit(cx);
                } else {
                    shell
                        .update(cx, |shell, cx| match action {
                            PendingExit::CloseWindow => {
                                if shell.prepare_window_close(cx) {
                                    window.remove_window();
                                }
                            }
                            PendingExit::RuntimeChange => shell.quit_for_runtime_change(cx),
                            PendingExit::InstallUpdate(staged) => {
                                shell.apply_staged_update(staged, cx)
                            }
                            PendingExit::Relaunch => shell.relaunch_now(cx),
                            PendingExit::Quit => unreachable!(),
                        })
                        .ok();
                }
            });
        }
        crate::transcript::record_view_frame("shell");
        let viewport = f32::from(window.viewport_size().width);
        if (self.viewport_width - viewport).abs() > 1.0 {
            self.files_tween = None;
            self.right_tween = None;
            self.right_takeover_content_tween = None;
            self.main_takeover_tween = None;
        }
        self.viewport_width = viewport;
        // Appearance actions persist independently of the shell. Mirror the
        // globals before any later debounced settings save can overwrite them.
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.sync_independent_settings(cx);
        let theme = Theme::of(cx);
        // The shell frost sits over native desktop blur on macOS and Windows.
        // Content surfaces add their own backgrounds over this shared tint.
        let (frost, text, font) = (theme.glass(), theme.text, theme.font_sans.clone());
        let (workspace_scope, auth) = {
            let state = self.state.read(cx);
            (state.workspace_scope, state.auth.clone())
        };
        self.sync_flow = sync_flow_after_auth(self.sync_flow, workspace_scope, auth.as_ref());
        let restart_required = self.sync_flow == SyncFlow::SignedOutRestartRequired;
        let gate = self
            .debug_gate
            .clone()
            .unwrap_or_else(|| self.state.read(cx).gate());

        let browser_profile = {
            let state = self.state.read(cx);
            crate::links::workspace_locator(
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
        };
        if browser_profile.is_some() && browser_profile != self.browser_profile {
            if self.browser_profile.is_some() {
                for browser in self.browsers.values() {
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.browsers.clear();
                self.browser_subs.clear();
                self.browser_context = crate::browser::BrowserContext::default();
            }
            self.browser_profile = browser_profile;
        }
        let browser_active = matches!(gate, GatePhase::Ready)
            && !restart_required
            && matches!(self.route, Route::Chat)
            && (self.right_pane_open(cx) || self.tween_active(self.right_tween));
        // Native clipping follows the animated GPUI mask. Drags only transfer
        // pointer ownership; the browser continues rendering and reflowing.
        let browser_dragging = cx.has_active_drag();
        #[cfg(target_os = "macos")]
        let browser_resize_inset = if self.right_pane_open(cx)
            && !self.right_pane_expanded
            && !self.tween_active(self.right_tween)
        {
            // The browser starts inside the panel's one-point left border.
            px(PANE_RESIZE_HITBOX_HALF_WIDTH - 1.0)
        } else {
            px(0.0)
        };
        #[cfg(target_os = "macos")]
        let browser_overlay_width = px(if self.files_visible_width(cx) > 0.0 {
            self.files_visible_width(cx) + PANE_RESIZE_HITBOX_HALF_WIDTH
        } else {
            0.0
        });
        let selected_surface = self.resolved_right_active(cx);
        for (id, browser) in &self.browsers {
            let presentation = crate::browser::model::presentation(
                browser_active && selected_surface == RightSurface::Browser(*id),
                browser_dragging,
            );
            browser.update(cx, |browser, cx| {
                #[cfg(target_os = "macos")]
                {
                    browser.set_resize_inset(browser_resize_inset, cx);
                    browser.set_right_occlusion(browser_overlay_width, cx);
                }
                browser.set_shortcuts(&self.settings.keymap);
                browser.set_presentation(presentation, cx);
            });
        }
        // Artifact tabs embed a web view the same way.
        for (id, browser, shows_web) in self.artifact_browsers(cx) {
            let presentation = crate::browser::model::presentation(
                browser_active && shows_web && selected_surface == RightSurface::Artifact(id),
                browser_dragging,
            );
            browser.update(cx, |browser, cx| {
                #[cfg(target_os = "macos")]
                {
                    browser.set_resize_inset(browser_resize_inset, cx);
                    browser.set_right_occlusion(browser_overlay_width, cx);
                }
                browser.set_shortcuts(&self.settings.keymap);
                browser.set_presentation(presentation, cx);
            });
        }

        // Fullscreen hides the macOS traffic lights — reflow the control
        // cluster with a 200ms ease-out tween (§1.1). A fullscreen transition
        // resizes the window, which re-renders us, so polling here is exact.
        let fullscreen = window.is_fullscreen();
        if self.fullscreen != Some(fullscreen) {
            if self.fullscreen.is_some() && cfg!(target_os = "macos") {
                self.titlebar_tween = Some(WidthTween::new(
                    titlebar_cluster_start(!fullscreen),
                    titlebar_cluster_start(fullscreen),
                ));
            }
            self.fullscreen = Some(fullscreen);
        }
        // Linux CSD: (re-)resolve which caption buttons we draw and on which
        // side — decorations can flip server↔client at runtime and the
        // desktop's button layout is user configuration.
        self.linux_captions = Self::resolve_linux_captions(window, cx);
        if cfg!(target_os = "linux") && self.button_layout_sub.is_none() {
            self.button_layout_sub =
                Some(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));
        }
        // Manual tween drive bookkeeping for this pass (see [`WidthTween`]).
        self.reduced_motion = motion::reduced_motion(cx);
        self.motion_active.set(false);

        if self.activation_sub.is_none() {
            self.activation_sub = Some(cx.observe_window_activation(
                window,
                |this: &mut Shell, window, cx| {
                    if !window.is_window_active() {
                        this.reset_command_palette_key_state();
                        this.set_jump_hints(false, cx);
                        this.composer.update(cx, |composer, cx| {
                            composer.set_queue_shortcut_revealed(false, cx)
                        });
                    }
                },
            ));
        }

        // A live handle can refer to an unmounted element. Recover against
        // the completed frame so newly mounted dialogs can claim focus first.
        if self.focus_sub.is_none() {
            self.focus_sub = Some(cx.on_focus_lost(window, |this: &mut Shell, window, cx| {
                let root = this.shortcut_focus.clone();
                let unfocused = this.unfocused.clone();
                let preferred = this.composer.focus_handle(cx);
                window.on_next_frame(move |window, cx| {
                    restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
                });
                cx.notify();
            }));
        }
        let shortcut_focus = self.shortcut_focus.clone();
        let unfocused = self.unfocused.clone();
        let preferred_focus = self.composer.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            restore_mounted_focus(&shortcut_focus, &preferred_focus, &unfocused, window, cx);
        });

        // Modifier events follow focus too. Reconcile from the window's input
        // snapshot so a missed release (or pointer event after it) heals hints.
        if !window.is_window_active() {
            self.set_jump_hints(false, cx);
        } else if self.jump_hints {
            // Only a fresh modifier event may turn hints on: activation can
            // retain the snapshot from before Cmd+Tab.
            self.update_jump_hints(&window.modifiers(), cx);
        }

        let root = div()
            .id("shell-root")
            .track_focus(&self.shortcut_focus)
            .child(div().track_focus(&self.unfocused))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(frost)
            // Linux CSD: a floating window rounds its corners against the
            // desktop (the window composites with alpha — see
            // `Theme::window_background_appearance`); tiled/maximized keeps
            // square edges flush with the screen. Everything, including the
            // splash and caption chrome, clips to the curve.
            .when(Self::linux_window_floating(window), |el| {
                el.rounded(px(LINUX_WINDOW_CORNER_RADIUS)).overflow_hidden()
            })
            .text_color(text)
            .font_family(font)
            .text_size(crate::typography::ui_rems(14.0))
            .on_drag_move::<SidebarSessionDrag>(cx.listener(Self::contain_pinned_session_drag))
            .on_drop::<SidebarSessionDrag>(cx.listener(|this, _, _, cx| {
                this.cancel_sidebar_session_transfer(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.sidebar_session_transfer.is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.sidebar_session_transfer.is_some() {
                        cx.notify();
                    }
                }),
            )
            .capture_key_down(cx.listener(Self::on_key_down_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drag_move(cx.listener(Self::on_sidebar_drag))
            .on_drag_move(cx.listener(Self::on_right_pane_drag))
            .on_drag_move(cx.listener(Self::on_files_panel_drag))
            .on_drag_move(cx.listener(Self::on_terminal_drag))
            // The panel shortcuts are chat-scoped chrome: in Settings they are
            // no-ops (zeron __root.tsx gates the hotkey on `!isSettings`, and
            // the terminal panel is only mounted on session routes). The
            // sidebar toggle stays live everywhere, as in the original.
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_terminal(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                if matches!(this.route, Route::Chat) && this.right_pane_open(cx) {
                    let file = match this.resolved_right_active(cx) {
                        RightSurface::File(id) => this.file_surfaces.get(&id).cloned(),
                        _ => None,
                    };
                    if let Some(file) = file {
                        file.update(cx, |file, cx| file.save_active_document(cx));
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(cx)))
            // Native Settings menu item and the platform convention (Cmd+, on
            // macOS, Ctrl+, elsewhere) always land on the default section.
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings(SettingsSection::Devices, cx)
            }))
            // Chat-scoped, unlike new-session — `cycle_session` holds the guard
            // and says why.
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.cycle_session(true, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.cycle_session(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_right_pane(cx);
                    if !this.right_pane_open(cx) {
                        // The hidden editor can retain a focus handle after unmounting.
                        // Restore a mounted target so the next shortcut can reopen it.
                        window.focus(&this.composer.focus_handle(cx), cx);
                    }
                }
            }))
            // The explorer's own toggle (the titlebar tree button): docks or
            // undocks the explorer portion without touching the surface host.
            .on_action(cx.listener(|this, _: &ToggleFiles, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_files_panel(window, cx);
                }
            }))
            // Chat-scoped like the panel toggles: Settings has no current
            // session to archive. Quiet under an open popover, like the other
            // session-nav shortcuts.
            .on_action(cx.listener(|this, _: &ArchiveSession, window, cx| {
                if this.overlay_owns_keyboard(cx) {
                    return;
                }
                // In the Outline tree it archives the selected row instead.
                if this.archive_selected_outline_row(window, cx) {
                    return;
                }
                if matches!(this.route, Route::Chat) {
                    this.archive_selected_chat(cx)
                }
            }))
            // A jump routes back to chat itself, so Settings is not a dead
            // spot — the same call a click on that sidebar row makes. But an
            // open picker/palette owns the keyboard: no jumping underneath
            // it. The MODEL menu advertises these same slots on its rows and
            // this matched binding beats its key handler to the dispatch —
            // forward the slot instead of eating it.
            .on_action(cx.listener(|this, jump: &JumpSession, _, cx| {
                // Start Chat's model menu advertises the same slots.
                let pickers = this
                    .start_chat_pickers()
                    .or_else(|| this.hand_off_pickers())
                    .filter(|pickers| pickers.read(cx).is_open())
                    .unwrap_or_else(|| this.composer.read(cx).pickers().clone());
                let handled = pickers.update(cx, |pickers, cx| pickers.jump_model_slot(jump.0, cx));
                if !handled && !this.overlay_owns_keyboard(cx) {
                    this.jump_to_session(jump.0, cx)
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, event, _, cx| this.on_modifiers_changed(event, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| this.open_workspace_search(workspace_search::SearchMode::Files, window, cx)))
            .on_action(cx.listener(|this, _: &SearchWorkspaceContent, window, cx| this.open_workspace_search(workspace_search::SearchMode::Content, window, cx)))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.open_workspace_search(workspace_search::SearchMode::Line, window, cx)))
            // Works from any route, like New session: the overview is a
            // top-level surface, not chat-scoped. Quiet under an open
            // picker/palette so it never navigates out from underneath one.
            .on_action(cx.listener(|this, _: &OpenOverview, _, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_overview(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusOverviewSearch, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    match this.route {
                        Route::Overview => {
                            let page = this.ensure_overview_page(cx);
                            page.update(cx, |overview, cx| {
                                overview.focus_search(window, cx);
                            });
                        }
                        Route::PrReviewChats => {
                            let page = this.ensure_pr_review_chats_page(cx);
                            page.update(cx, |page, cx| page.focus_search(window, cx));
                        }
                        // Not a search surface: let a deeper owner of ⌘F (an
                        // Artifact tab's find bar) have the key.
                        _ => cx.propagate(),
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &OpenPrReviewChats, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_pr_review_chats_focused(window, cx);
                }
            }))
            .on_action(cx.listener(
                |this, action: &work_outline_ui::RunWorkOutlineCommand, window, cx| {
                    if !this.overlay_owns_keyboard(cx) {
                        this.run_work_outline_command(action.0, window, cx);
                    }
                },
            ))
            .on_action(cx.listener(
                |this,
                 _: &crate::work_outline::outline_view::context_menu::OpenContextMenu,
                 window,
                 cx| { this.open_work_outline_menu_from_keyboard(window, cx) },
            ))
            // ⌘P (customizable) is the PR page, like the fixed ⇧⌘P alias.
            .on_action(cx.listener(|this, _: &OpenMyPrs, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_pr_review_chats_focused(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenItemTab, _, cx| this.open_item_tab(cx)))
            .on_action(cx.listener(|this, _: &OpenTeamTab, _, cx| this.open_team_tab(cx)))
            .on_action(cx.listener(|this, _: &ToggleRunningChats, window, cx| {
                this.toggle_running_chats(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleItemCard, _, cx| this.toggle_chat_item_card_pin(cx)),
            )
            .on_action(cx.listener(|this, _: &OpenModelPicker, window, cx| {
                // Inside Start Chat the shortcut opens the dialog's picker.
                if let Some(pickers) = this.start_chat_pickers().or_else(|| this.hand_off_pickers()) {
                    pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                    return;
                }
                if matches!(this.route, Route::Chat) && !this.overlay_owns_keyboard(cx) {
                    let pickers = this.composer.read(cx).pickers().clone();
                    pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if this.add_space.is_some() {
                    this.add_space = None;
                    cx.notify();
                } else {
                    this.open_add_space(cx);
                }
            }));

        let render_gate = if restart_required {
            GatePhase::Loading
        } else {
            gate.clone()
        };
        let root = match &render_gate {
            GatePhase::Ready => {
                // Focus is a sync signal: on the rising edge of window
                // activation, nudge every open room to verify liveness — a
                // broadcast-deaf socket (accepted writes, runtime pongs,
                // nothing delivered; 2026-08-04 incident) then heals within
                // seconds of the user looking at the app rather than waiting
                // out the background probe cadence.
                let window_active = window.is_window_active();
                if window_active && !self.was_window_active {
                    self.state.update(cx, |s, cx| s.probe_sync(cx));
                }
                self.was_window_active = window_active;
                // A run finishing while you're LOOKING at the session must not
                // badge "completed" until you leave and return — mark it seen
                // live while the window is active (idempotent guard inside;
                // one extra frame settles it).
                if window_active {
                    let unseen_selected = {
                        let s = self.state.read(cx);
                        s.selected_chat_row()
                            .filter(|c| c.unseen())
                            .map(|c| c.id.clone())
                    };
                    if let Some(chat_id) = unseen_selected {
                        self.state
                            .update(cx, |s, cx| s.mark_chat_seen(&chat_id, cx));
                    }
                }
                // Capture knob: `MAST_OPEN_DIALOG=model` pops the combined
                // harness/model menu (needs `window`, so it fires here rather
                // than in `on_state_changed`).
                if self.debug_dialog.as_deref() == Some("model") {
                    self.debug_dialog = None;
                    self.composer
                        .update(cx, |c, cx| c.debug_open_model_menu(window, cx));
                }
                // MessageRail width gate: hide below 48rem of main-panel width.
                let viewport = f32::from(window.viewport_size().width);
                self.viewport_height = f32::from(window.viewport_size().height);
                // Stamped for `right_target` — the expanded changes panel
                // sizes itself to the viewport.
                self.viewport_width = viewport;
                let on_chat = matches!(self.route, Route::Chat);
                let right_target_width = if on_chat {
                    self.right_visible_width(cx)
                } else {
                    0.0
                };
                let panel_handoff = self.composer_dock.borrow_mut().observe_pane(
                    self.state.read(cx).selected_chat.is_some(),
                    right_target_width,
                    on_chat && !self.reduced_motion,
                    self.render_time.unwrap_or_else(std::time::Instant::now),
                );
                if panel_handoff {
                    self.motion_active.set(true);
                }
                let main_target_width = conversation_width(
                    viewport - self.files_reserved_width(cx),
                    self.sidebar_target(),
                    right_target_width,
                );
                let main_transition = self.active_tween_endpoints(self.main_takeover_tween);
                let main_content_width =
                    stable_panel_content_width(main_target_width, main_transition);
                let transcript_width = self.composer_dock.borrow_mut().transcript_width(
                    main_content_width,
                    self.state.read(cx).selected_chat.is_some(),
                    panel_handoff,
                );
                let main_width = (transcript_width - 10.0).max(0.0);
                // Clearance excludes the terminal dock: the transcript
                // viewport ends at the dock's top (see the underlay in
                // `render_main`), so only the chrome above it overlaps.
                let term_h = self.eval_tween(self.terminal_tween, self.terminal_target(cx));
                let stack_h = (self.bottom_stack.get() - term_h).max(0.0);
                let expected_has_composer = {
                    let state = self.state.read(cx);
                    (!state.spaces.is_empty() || state.no_project) && state.selected_chat.is_some()
                };
                let bottom_stack_ready = bottom_stack_measurement_matches(
                    self.bottom_stack_has_composer.get(),
                    expected_has_composer,
                );
                let on_overview = matches!(self.route, Route::Overview);
                self.transcript.update(cx, |t, cx| {
                    if on_overview {
                        // The overview's chat panel: a narrow column with the
                        // composer BELOW the transcript, not overlaying it.
                        // Its header sits above the transcript (nothing
                        // overlays the top), so no titlebar inset either.
                        t.set_rail_enabled(false, cx);
                        t.set_bottom_clearance(0.0, cx);
                        t.set_top_inset(crate::overview::CHAT_PANEL_TRANSCRIPT_TOP_INSET, cx);
                    } else {
                        t.set_top_inset(
                            crate::transcript::OWN_SEND_TOP_INSET_PX
                                + CHAT_HEADER_METADATA_CLEARANCE,
                            cx,
                        );
                        t.set_rail_enabled(rail::rail_visible(main_width), cx);
                        if bottom_stack_ready && expected_has_composer {
                            t.set_bottom_clearance(stack_h, cx);
                        }
                    }
                });

                let sidebar = self.render_sidebar(cx);
                let sidebar_handle = self.resize_handle(
                    "sidebar-resize",
                    PaneResizeKind::Sidebar,
                    || SidebarResize,
                    |shell, _| {
                        shell.settings.sidebar_width = SIDEBAR_DEFAULT;
                        shell.sidebar_edge_bounce = None;
                    },
                    cx,
                );
                let main = self.render_main(window, main_content_width, transcript_width, cx);
                // The Changes pane is chat-scoped chrome: the Settings route
                // never renders it (zeron __root.tsx `!isSettings && activeChat`
                // around the diff column) — the per-session open flags stay
                // intact for the return trip.
                let right_open = on_chat && self.right_pane_open(cx);
                // Takeover mode derives its width from the viewport, so a
                // manual drag handle would fight the expanded target.
                let right_handle = (right_open
                    && !panel_handoff
                    && !self.right_pane_expanded
                    && !self.tween_active(self.right_tween))
                .then(|| {
                    self.resize_handle(
                        "right-pane-resize",
                        PaneResizeKind::Right,
                        || RightPaneResize,
                        |shell, _| {
                            shell.settings.right_pane_width = RIGHT_PANE_DEFAULT;
                            shell.right_edge_bounce = None;
                        },
                        cx,
                    )
                    // A forgiving transparent hit target centered on the
                    // seam; the panel's 1px border remains the visual divider.
                    .left(px(-PANE_RESIZE_HITBOX_HALF_WIDTH))
                });
                let right: AnyElement = if on_chat {
                    self.render_right_pane(window, cx)
                } else {
                    Empty.into_any_element()
                };
                let files_panel = self.render_files_panel(window, cx);
                let overlays = self.render_overlays(window.viewport_size(), window, cx);
                // Copied out (not held) — `render_title_bar` needs `cx` mutable.
                let theme = Theme::of(cx).clone();
                // No inset cards (user request): the conversation column sits
                // flush and unbordered, the transcript directly on the frost
                // glass; the changes pane is a flush left-bordered glass panel
                // (built inside `render_right_pane`).
                let main = if main_transition.is_some() {
                    div()
                        .h_full()
                        .w(px(main_content_width))
                        .flex_none()
                        .flex()
                        .child(main)
                        .into_any_element()
                } else {
                    main
                };
                let card = crate::components::shell_parts::main_panel(&theme, main);
                // The whole app page is one keyed `animate-in` entrance (zeron
                // App.tsx `<div key={phase} className="animate-in h-full">`):
                // arriving from the splash or any gate fades the page in; the
                // splash-out crossfades over it on boot.
                // The sidebar resize handle FLOATS over the sidebar/card seam
                // (zero layout width, same idiom as the changes-pane grabber)
                // so the sidebar's right gutter stays exactly as wide as its
                // left one — a 5px flex child here read as lopsided spacing.
                let sidebar_seam = div()
                    .w(px(0.0))
                    .h_full()
                    .flex_none()
                    .relative()
                    .child(sidebar_handle.left(px(-PANE_RESIZE_HITBOX_HALF_WIDTH)));
                // Keep the right resize target outside the pane's
                // overflow-hidden width container. This mirrors the sidebar
                // seam and lets the target straddle both adjacent panes.
                // Paint it after the page so page input cannot occlude the
                // inner half. A deferred draw would capture all native input.
                let right_seam: AnyElement = if let Some(handle) = right_handle {
                    div()
                        .w(px(0.0))
                        .h_full()
                        .flex_none()
                        .absolute()
                        .left_0()
                        .top_0()
                        .child(handle)
                        .into_any_element()
                } else {
                    Empty.into_any_element()
                };
                let title_bar = self.render_title_bar(window.viewport_size().height, cx);
                // Sidebar tone: a slightly lighter column behind the sidebar,
                // spanning the FULL window height (under the traffic lights,
                // through the titlebar, down to the bottom edge). Its width
                // rides the same tween as the sidebar, so the tone melts away
                // with the collapse instead of vanishing in a frame.
                let sidebar_now = self.sidebar_now();
                // Hairline on its right edge — full height like the tone,
                // so the sidebar column reads as its own surface.
                // The tone carries the window's left corners when the CSD
                // window floats — with one caveat: a corner radius is
                // clamped to the element's own size, and the COLLAPSED
                // sidebar is a ~1px border sliver (the grab affordance).
                // macOS trims that hairline with the window server's native
                // corner clip; we reproduce the same trim by insetting the
                // sliver vertically to where the curve begins, so its tips
                // never float over the transparent corner cutouts.
                let window_corner = Self::window_corner_radius(window);
                let sidebar_tone = crate::components::shell_parts::sidebar_tone(&theme, sidebar_now, window_corner);
                // The content row spans the FULL window height — the titlebar
                // overlays it (glass, no fill), so the transcript can scroll
                // under the header and fade out at its edge. Columns that
                // must NOT underlap (sidebar content, the changes panel,
                // settings) pad themselves down by the titlebar height.
                let page = div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(sidebar)
                            .child(sidebar_seam)
                            .child(card)
                            // The right pane is ONE container: the surface
                            // host column and the docked explorer column
                            // sit side by side under a shared titlebar
                            // strip; the resize seam straddles its left edge.
                            .child(
                                div()
                                    .h_full()
                                    .flex_none()
                                    .relative()
                                    .child(
                                        div()
                                            .h_full()
                                            .flex()
                                            .flex_row()
                                            .child(right)
                                            .child(files_panel),
                                    )
                                    .child(right_seam),
                            ),
                    )
                    .child(crate::components::shell_parts::titlebar_overlay(&theme, title_bar))
                    .child(self.render_titlebar_cluster(cx))
                    .children(overlays);
                root.child(sidebar_tone)
                    .child(motion::fade_in("phase-app", page))
            }
            GatePhase::Loading => root, // splash overlay covers boot
            GatePhase::OrgGate => {
                let card = self.render_org_gate(cx);
                root.child(card)
            }
            phase @ (GatePhase::Failed(_) | GatePhase::SignIn) => {
                let card = self.render_gate_card(phase, cx);
                root.child(card)
            }
        };
        let root = if restart_required {
            let restart = self.render_signed_out_restart(cx);
            root.child(restart)
        } else {
            root
        };

        // A manually-driven tween is mid-flight: keep frames coming (the same
        // scheduling `with_animation` would have requested). Hover color fades
        // ride the same clock; their once-per-frame tick lives here (this is
        // the window's root render — it runs exactly once per frame).
        if self.motion_active.get() | motion::hover_fades_active() {
            window.request_animation_frame();
        }

        // Boot splash overlay: visible → crossfades out on Ready → removed.
        let root = match self.splash {
            SplashPhase::Visible => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, false, cx.entity_id(), cx))
            }
            SplashPhase::FadingOut => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, true, cx.entity_id(), cx))
            }
            SplashPhase::Gone => root,
        };

        // Caption controls are shell-level chrome, not Ready-page content:
        // keep them above the splash and every auth/org/error gate as well as
        // the full application. Gate pages also need a drag surface because
        // they do not render the unified tabs/settings titlebar — on Windows
        // the native `Drag` control area, on Linux the explicit
        // `start_window_move` strip (the control-area hit-test is inert
        // there); macOS drags gate windows natively.
        let root = if (!restart_required && matches!(gate, GatePhase::Ready))
            || cfg!(target_os = "macos")
        {
            root
        } else {
            root.child(
                self.titlebar_drag_region(
                    "gate-titlebar-drag",
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(Theme::TITLEBAR_HEIGHT)),
                    cx,
                ),
            )
        };
        let root = root
            .children(self.render_windows_caption_controls(window, cx))
            .children(self.render_linux_caption_controls(window, cx))
            // Last so the invisible CSD resize strips sit above every other
            // element at the window edges.
            .children(Self::render_linux_resize_borders(window));
        self.render_time = None;
        root
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod exit_regressions;

/// Native browser regression fixture hooks are excluded from shipped builds.
#[cfg(feature = "browser-fixture")]
impl Shell {
    pub fn fixture_focus_mounted(&self, window: &Window, cx: &App) -> bool {
        self.shortcut_focus.contains_focused(window, cx)
    }
    pub fn fixture_active_browser(
        &self,
        cx: &App,
    ) -> Option<(u64, Entity<crate::browser::BrowserSurface>)> {
        let RightSurface::Browser(id) = self.resolved_right_active(cx) else {
            return None;
        };
        self.browsers.get(&id).cloned().map(|browser| (id, browser))
    }
    pub fn fixture_open_browser(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (u64, Entity<crate::browser::BrowserSurface>) {
        self.set_surfaces_open(true, cx);
        // Hosted Macs can expose only a 1024px desktop. Use the app's
        // normal collapsed-sidebar layout to keep both conversation and
        // preview readable in that real window.
        if f32::from(window.viewport_size().width) < 1200.0 {
            self.settings.sidebar_collapsed = true;
        }
        self.add_browser_surface(url, window, cx);
        (self.browser_seq, self.browsers[&self.browser_seq].clone())
    }
    pub fn fixture_select_browser(&mut self, id: u64, cx: &mut Context<Self>) {
        self.set_right_active(RightSurface::Browser(id), cx);
    }
    pub fn fixture_close_browser(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.close_right_surface(RightSurface::Browser(id), window, cx);
    }
    pub fn fixture_browser_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.right_plus.open(());
            cx.notify();
        } else {
            self.close_right_plus(cx);
        }
    }
    pub fn fixture_browser_menu_mounted(&self) -> bool {
        self.right_plus.get().is_some()
    }
    pub fn fixture_expand_browser(&mut self, cx: &mut Context<Self>) {
        self.toggle_right_pane_expand(cx);
    }
    pub fn fixture_blur_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Settings(SettingsSection::Devices);
        window.blur();
        cx.notify();
    }
    pub fn fixture_toggle_sidebar(&mut self, right: bool, cx: &mut Context<Self>) {
        if right {
            // The fixtures drive the surface host only; the explorer stays put.
            self.set_surfaces_open(!self.right_pane_open(cx), cx);
        } else {
            self.toggle_sidebar(cx);
        }
    }
    pub fn fixture_resize_browser(&mut self, width: f32, cx: &mut Context<Self>) {
        self.settings.right_pane_width = width;
        cx.notify();
    }
}

#[cfg(test)]
mod right_tab_mouse_regressions;

#[cfg(test)]
mod shortcut_focus_regressions;

#[cfg(feature = "snapshot")]
impl Shell {
    /// Skip the startup splash for deterministic offscreen fixtures.
    pub fn fixture_snapshot_ready(&mut self) {
        self.splash = SplashPhase::Gone;
    }

    /// Boot the real in-process engine behind a fixture shell. Call it only
    /// after the window has opened and drawn once: attaching fires the state
    /// observer, which cannot run while the window is still being created.
    pub(crate) fn fixture_boot_params(&self) -> (Entity<AppState>, EngineBootConfig) {
        (self.state.clone(), self.boot.clone())
    }

    /// One line of shell state for the driver's header and for diagnosing a
    /// blank tree.
    pub fn fixture_status(&self, cx: &App) -> String {
        let state = self.state.read(cx);
        format!(
            "connection={:?} chats={} selected={} splash={}",
            state.connection,
            state.chats.len(),
            state.selected_chat.as_deref().unwrap_or("-"),
            if matches!(self.splash, SplashPhase::Gone) { "gone" } else { "visible" },
        )
    }

    /// The engine is attached, the connection is up and its chats have arrived.
    pub fn fixture_engine_ready(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        state.engine().is_some()
            && matches!(state.connection, crate::state::ConnectionStatus::Ready)
            && !state.chats.is_empty()
    }

    pub fn fixture_snapshot_model_catalog(&mut self, cx: &mut Context<Self>) {
        self.composer.read(cx).pickers().clone().update(cx, |pickers, cx| pickers.fixture_model_catalog(cx));
    }

    pub fn fixture_snapshot_attachment(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, _| composer.fixture_attachment(bytes));
    }

    pub fn fixture_snapshot_overview(&mut self, cx: &mut Context<Self>) {
        self.open_overview(cx);
    }

    pub fn fixture_snapshot_item_panel(&mut self, cx: &mut Context<Self>) {
        self.active_chat = "fixture-chat".into();
        let key = self.panel_key(cx);
        self.panels.update(&key, |panel| {
            panel.changes_open = true;
            panel.right_active = RightSurface::Item;
        });
    }

    pub fn fixture_snapshot_expand_tool_group(&mut self, cx: &mut Context<Self>) {
        self.transcript.update(cx, |transcript, cx| transcript.fixture_expand_tool_group(cx));
    }
}

/// Native visual QA uses the production shell with isolated fixture data.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_appshots_settings(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.open_settings(SettingsSection::Appshots, cx);
        } else {
            self.close_settings(cx);
        }
    }
    pub fn fixture_appshots_composer(&self) -> Entity<Composer> {
        self.composer.clone()
    }
    pub fn fixture_appshots_sidebar(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_collapsed = collapsed;
        cx.notify();
    }
    pub fn fixture_appshots_transcript_start(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |t, cx| t.fixture_appshots_start(cx));
    }
}

#[cfg(test)]
mod titlebar_lamp_layout;
