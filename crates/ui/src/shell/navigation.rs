use super::*;

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

impl Shell {
    pub(super) fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
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
}
