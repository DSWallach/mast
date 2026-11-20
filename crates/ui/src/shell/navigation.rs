use super::*;

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
    ScheduledTasks,
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
    ScheduledTasks,
    /// The PR page: dashboards plus the chats launched to review PRs.
    PrReviewChats,
    Review,
}

impl Shell {
    pub(super) fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.command_palette = None;
        // Recreate per visit: the page's ListHarnesses load re-probes which
        // CLIs are installed, so installing one shows up on the next open.
        if section == SettingsSection::Harnesses {
            self.harnesses_page = None;
        }
        if section == SettingsSection::Agents
            && let Some(page) = &self.accounts_page
        {
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
    pub(super) fn leave_overview_for_chat(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn overview_outlet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_overview_page(cx).into_any_element()
    }

    pub(super) fn ensure_overview_page(
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

    pub(super) fn ensure_pr_review_chats_page(
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

    pub(super) fn pr_review_chats_outlet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_pr_review_chats_page(cx).into_any_element()
    }

    /// Chat menu "Add PR / ticket…": the link editor lives in the
    /// overview's expanded detail, so switch there and open it for this
    /// chat (expanded, focused, scrolled/framed into view).
    pub(super) fn link_chat_from_menu(
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

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.focus_composer(cx);
        self.nav.push(NavEntry::Chat(self.active_chat.clone()));
        cx.notify();
    }

    // ---- back/forward (route history) ----

    pub(super) fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    pub(super) fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    pub(super) fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
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
            NavEntry::ScheduledTasks => {
                self.route = Route::ScheduledTasks;
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
}
