use super::*;

impl AppState {
    // ---- queries ----

    /// Non-archived, top-level chats in sidebar order. Chats spawned by
    /// another chat (`parent_chat_id`, the Mast MCP's orchestration link)
    /// are the parent's workers, not sessions the user started: they stay
    /// reachable by id/deep link but never take a sidebar row or jump slot.
    pub fn visible_chats(&self) -> impl Iterator<Item = &Chat> {
        self.chats
            .iter()
            .filter(|c| !c.archived && c.parent_chat_id.is_none() && !c.is_side_chat())
    }

    pub(crate) fn restore_composer_target(
        &mut self,
        defaults: &crate::settings::composer::ComposerDefaults,
    ) {
        if self.selected_chat.is_some() || self.no_project {
            return;
        }
        if self.selected_device.is_none() {
            self.selected_device = defaults.device.clone();
        }
        if self.selected_space.is_none() {
            self.no_project = defaults.no_project;
            self.selected_space = if defaults.no_project {
                None
            } else {
                defaults.project.clone()
            };
        }
    }

    pub fn selected_space_row(&self) -> Option<&Space> {
        if self.no_project {
            return None;
        }
        let id = self.selected_space.as_deref()?;
        self.spaces.iter().find(|s| s.id == id)
    }

    /// The device the new-session canvas targets: the picked project's host
    /// when one is selected, else the explicit device pick, else this device.
    pub fn effective_device_id(&self) -> Option<String> {
        if let Some(space) = self.selected_space_row() {
            return Some(space.device_id.clone());
        }
        self.selected_device
            .clone()
            .or_else(|| self.local_device_id.clone())
    }

    /// Pick the composer's target device. Keeps the project pick consistent:
    /// a project on another device can't survive the switch — fall back to
    /// the first project on the new device, else "no project".
    pub fn select_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let project_moves = self
            .selected_space_row()
            .is_some_and(|s| s.device_id != device_id);
        if project_moves {
            let first = self
                .spaces_sorted()
                .iter()
                .find(|s| s.device_id == device_id)
                .map(|s| s.id.clone());
            self.no_project = first.is_none();
            self.selected_space = first;
        }
        self.selected_device = Some(device_id);
        cx.notify();
    }

    pub fn space_row(&self, space_id: &str) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == space_id)
    }

    /// Spaces in display order — case-insensitive alphabetical, the order
    /// both space selectors (sidebar filter, composer picker) list rows in.
    /// Ties break on id so the order is stable across renders.
    pub fn spaces_sorted(&self) -> Vec<&Space> {
        let mut spaces: Vec<&Space> = self.spaces.iter().collect();
        spaces.sort_by_key(|s| (s.display_name().to_lowercase(), s.id.clone()));
        spaces
    }

    pub fn space_for_chat(&self, chat: &Chat) -> Option<&Space> {
        self.space_row(chat.space_id.as_deref()?)
    }

    /// Non-archived chats of a space in tab (creation) order. Chats with a
    /// dangling/missing `space_id` are invisible by construction.
    pub fn chats_in_space(&self, space_id: &str) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = self
            .visible_chats()
            .filter(|c| c.space_id.as_deref() == Some(space_id))
            .collect();
        sort_tabs(&mut chats);
        chats
    }

    pub fn device_name(&self, device_id: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.id == device_id)
            .map(|d| d.name.as_str())
    }

    /// Host-presence check: is this device's 15s presence heartbeat fresh?
    /// Distinguishes "host offline" (its queued work syncs when it returns)
    /// from slow sync. The local device is trivially online; unknown devices
    /// get the benefit of the doubt (no evidence — don't cry wolf).
    pub fn device_online(&self, device_id: &str, now: DateTime<Utc>) -> bool {
        if self.local_device_id.as_deref() == Some(device_id) {
            return true;
        }
        match self.devices.iter().find(|d| d.id == device_id) {
            Some(d) => crate::settings::devices::device_online(d.last_seen_at, now),
            None => true,
        }
    }

    /// The "@ device" tag for a space — shared by the space pickers' rows,
    /// the sidebar filter trigger, and the composer's space chip. Returns
    /// `(tag, offline)`; staleness renders as a disconnected GLYPH at the
    /// call sites (user request), never words in the tag.
    pub fn space_device_tag(&self, space: &Space, now: DateTime<Utc>) -> (String, bool) {
        let offline = !self.device_online(&space.device_id, now);
        let device = self
            .device_name(&space.device_id)
            .unwrap_or("Unknown device");
        (format!("@ {device}"), offline)
    }

    /// Does the selected space's folder have git? Drives the branch picker and
    /// the diff sidebar (owner-stamped, synced — no RPC).
    pub fn selected_space_git(&self) -> bool {
        self.selected_space_row().is_some_and(|s| s.git_detected)
    }

    /// Full display status for a chat (tab dots, Active list). A send in
    /// flight ([`Self::begin_pending_send`]) reads as Working — the queued
    /// command is as good as running.
    /// Board status lookup in one pass over sessions and chats.
    pub(crate) fn board_indicators(&self, now: DateTime<Utc>) -> HashMap<String, ChatIndicator> {
        let mut sessions = HashMap::new();
        for session in &self.sessions {
            sessions.entry(session.chat_id.as_str()).or_insert(session);
        }
        self.chats
            .iter()
            .map(|chat| {
                let indicator = if self.send_pending(&chat.id, now) {
                    ChatIndicator::Working
                } else {
                    display_status(chat, sessions.get(chat.id.as_str()).copied(), now)
                };
                (chat.id.clone(), indicator)
            })
            .collect()
    }

    /// Build one lookup for a batch of status derivations, preserving the
    /// first session when a malformed snapshot contains duplicate chat IDs.
    pub(super) fn sessions_by_chat(&self) -> HashMap<&str, &Session> {
        let mut sessions = HashMap::with_capacity(self.sessions.len());
        for session in &self.sessions {
            #[cfg(test)]
            crate::shell::perf_counts::SESSION_SCANS.with(|c| c.set(c.get() + 1));
            sessions.entry(session.chat_id.as_str()).or_insert(session);
        }
        sessions
    }

    pub(super) fn display_status_with_session(
        &self,
        chat: &Chat,
        session: Option<&Session>,
        now: DateTime<Utc>,
    ) -> ChatIndicator {
        if self.send_pending(&chat.id, now) {
            ChatIndicator::Working
        } else {
            display_status(chat, session, now)
        }
    }

    pub fn display_status_for(&self, chat: &Chat, now: DateTime<Utc>) -> ChatIndicator {
        if self.send_pending(&chat.id, now) {
            return ChatIndicator::Working;
        }
        display_status(chat, self.session_for(&chat.id), now)
    }

    /// The sidebar's Sessions list: every non-archived chat of a LIVE space,
    /// on any device — idle included — in pure recency order (status drives
    /// the dot, never the position; see [`sort_active`]).
    pub fn overview_chats(&self, now: DateTime<Utc>) -> Vec<(ChatIndicator, &Chat)> {
        self.overview_chats_with_archived(now, false)
    }

    /// [`Self::overview_chats`], optionally including archived rows too (the
    /// multichat overview's "Show archived" toggle — matches the reference
    /// `session_canvas` tool's own default-off `showArchived` behavior).
    pub fn overview_chats_with_archived(
        &self,
        now: DateTime<Utc>,
        include_archived: bool,
    ) -> Vec<(ChatIndicator, &Chat)> {
        let mut rows = self.overview_chats_unsorted(now, include_archived);
        #[cfg(test)]
        crate::shell::perf_counts::SIDEBAR_SORTS.with(|c| c.set(c.get() + 1));
        sort_active(&mut rows);
        rows
    }

    fn overview_chats_unsorted(
        &self,
        now: DateTime<Utc>,
        include_archived: bool,
    ) -> Vec<(ChatIndicator, &Chat)> {
        let sessions = self.sessions_by_chat();
        let latest = mast_proto::latest_task_chat_ids(&self.chats);
        self.chats
            .iter()
            .filter(|c| c.parent_chat_id.is_none() && !c.is_side_chat())
            .filter(|c| c.scheduled_task_info.is_none() || latest.contains(c.id.as_str()))
            .filter(|c| include_archived || !c.archived)
            .filter(|c| match c.space_id.as_deref() {
                None => true,
                Some(id) => self.space_row(id).is_some(),
            })
            .map(|c| {
                (
                    self.display_status_with_session(c, sessions.get(c.id.as_str()).copied(), now),
                    c,
                )
            })
            .collect()
    }

    /// Rows for callers that apply their own sidebar sort order.
    pub(crate) fn sidebar_chats_unsorted(
        &self,
        now: DateTime<Utc>,
        space_filter: Option<&str>,
    ) -> Vec<(ChatIndicator, &Chat)> {
        self.overview_chats_unsorted(now, false)
            .into_iter()
            .filter(|(_, chat)| !self.personal_assistant.covers(chat))
            .filter(|(_, chat)| {
                space_filter.is_none_or(|space_id| chat.space_id.as_deref() == Some(space_id))
            })
            .collect()
    }

    /// The sidebar's active list exactly as it is drawn: [`Self::overview_chats`]
    /// narrowed to the current project filter. The jump shortcuts and their
    /// hints both count positions here, so neither can drift from the rows on
    /// screen.
    pub fn sidebar_chats(
        &self,
        now: DateTime<Utc>,
        space_filter: Option<&str>,
    ) -> Vec<(ChatIndicator, &Chat)> {
        self.overview_chats(now)
            .into_iter()
            .filter(|(_, chat)| !self.personal_assistant.covers(chat))
            .filter(|(_, chat)| match space_filter {
                Some(space_id) => chat.space_id.as_deref() == Some(space_id),
                None => true,
            })
            .collect()
    }

    pub fn session_for(&self, chat_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| {
            #[cfg(test)]
            crate::shell::perf_counts::SESSION_SCANS.with(|c| c.set(c.get() + 1));
            s.chat_id == chat_id
        })
    }

    /// Staleness-checked status dot for a chat row. A send in flight reads as
    /// Working (see [`Self::display_status_for`]).
    pub fn indicator_for(&self, chat_id: &str, now: DateTime<Utc>) -> Indicator {
        if self.send_pending(chat_id, now) {
            return Indicator::Working;
        }
        effective_indicator(self.session_for(chat_id), now)
    }

    pub fn selected_chat_row(&self) -> Option<&Chat> {
        let id = self.selected_chat.as_deref()?;
        self.chats.iter().find(|c| c.id == id)
    }

    /// The chat the Archive session shortcut acts on: the selected one, unless
    /// it is already archived. The shortcut archives and never unarchives, so
    /// an archived chat is left alone. Pure.
    pub fn archivable_selected_chat(&self) -> Option<&str> {
        self.selected_chat_row()
            .filter(|chat| !chat.archived)
            .map(|chat| chat.id.as_str())
    }

    /// Latest valid PR for a chat, rechecked against device, checkout, cwd and branch.
    pub fn change_request_for_chat(&self, chat: &Chat) -> Option<&ChangeRequestSummary> {
        let key = crate::change_requests::checkout_watch_key(
            chat,
            self.selected_chat_diff_target(&chat.id),
        )?;
        self.change_requests.change_request_for_key(&key)
    }
}
