use super::*;

/// Gaps between attempts to scroll an opened chat to a turn: 0 + these sum to ~15 s.
enum TurnScroll {
    Done,
    Waiting,
    Abandoned,
}

const TURN_SCROLL_RETRY_MS: [u64; 8] = [0, 250, 500, 1000, 2000, 3000, 4000, 4000];

impl Shell {
    pub(super) fn right_terminal_panel(&mut self, cx: &mut Context<Self>) -> Entity<TerminalPanel> {
        if let Some(terminal) = &self.right_terminal {
            return terminal.clone();
        }
        let terminal = cx.new(|cx| TerminalPanel::new_embedded(self.state.clone(), cx));
        self.right_terminal = Some(terminal.clone());
        terminal
    }

    /// The right pane's surface tabs in the STORED (drag-reorderable) order —
    /// `(surface, title)`; entries whose backing tab/entity is gone are
    /// skipped.
    pub(super) fn right_surface_rows(
        &self,
        cx: &App,
    ) -> Vec<(RightSurface, SharedString, bool, Option<SharedString>)> {
        let key = self.panel_key(cx);
        let stored: &[RightSurface] = self
            .right_tabs
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let terminals: Vec<(u64, SharedString, bool)> = self
            .right_terminal
            .as_ref()
            .map(|t| t.read(cx).tab_summaries(cx))
            .unwrap_or_default();
        let mut rows: Vec<_> = stored
            .iter()
            .filter_map(|surface| match surface {
                RightSurface::File(id) => self.file_surfaces.get(id).map(|file| {
                    let path = self.file_surface_paths.get(id);
                    let title = path
                        .map(|path| workspace_file_title(path))
                        .unwrap_or_else(|| SharedString::from("File"));
                    (
                        *surface,
                        title,
                        file.read(cx).has_unsaved_changes(),
                        path.cloned().map(Into::into),
                    )
                }),
                RightSurface::Diff(id) => self
                    .diffs
                    .get(id)
                    // Contextual title (user request): the pane's scope
                    // label, or the pinned commit's subject.
                    .map(|changes| (*surface, changes.read(cx).tab_title(), false, None)),
                RightSurface::Terminal(tab) => terminals
                    .iter()
                    .find(|(k, _, _)| k == tab)
                    .map(|(_, title, _)| (*surface, title.clone(), false, None)),
                RightSurface::Subagent(id) => self
                    .subagent_tabs
                    .get(id)
                    .map(|tab| (*surface, tab.title.clone(), false, None)),
                RightSurface::Artifact(id) => self
                    .artifact_tab_title(*id, cx)
                    .map(|title| (*surface, title, false, None)),
                RightSurface::SideChat(id) => self
                    .side_chat_pane(*id)
                    .map(|_| (*surface, "Side chat".into(), false, None)),
                RightSurface::Browser(id) => self.browsers.get(id).map(|browser| {
                    let browser = browser.read(cx);
                    (
                        *surface,
                        browser.title(),
                        false,
                        browser.page.url.clone().map(Into::into),
                    )
                }),
                RightSurface::Picker
                | RightSurface::Item
                | RightSurface::Team
                | RightSurface::Review => None,
            })
            .collect();
        if self.review_artifact(cx).is_some() {
            rows.push((RightSurface::Review, "Review".into(), false, None));
        }
        rows
    }
    pub(super) fn workspace_path_for_surface(
        &self,
        surface: RightSurface,
        _cx: &App,
    ) -> Option<WorkspacePathDrag> {
        let path = match surface {
            RightSurface::File(id) => self.file_surface_paths.get(&id)?.clone(),
            RightSurface::Picker
            | RightSurface::Item
            | RightSurface::Team
            | RightSurface::Review
            | RightSurface::Diff(_)
            | RightSurface::Terminal(_)
            | RightSurface::Subagent(_)
            | RightSurface::Artifact(_)
            | RightSurface::SideChat(_)
            | RightSurface::Browser(_) => {
                return None;
            }
        };
        Some(WorkspacePathDrag::new(path, false))
    }
    /// Drag-reorder a surface tab within this chat's strip.
    pub(super) fn reorder_right_tabs(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let key = self.panel_key(cx);
        if let Some(tabs) = self.right_tabs.get_mut(&key)
            && from < tabs.len()
            && to < tabs.len()
            && from != to
        {
            let surface = tabs.remove(from);
            tabs.insert(to, surface);
            cx.notify();
        }
    }

    /// Track the hovered drop slot mid-drag (the terminal drawer's
    /// `update_drag_over`, ported: epoch bumps restart the slide tween).
    pub(super) fn update_right_tab_drag_over(
        &mut self,
        from: usize,
        over: usize,
        cx: &mut Context<Self>,
    ) {
        match &mut self.right_tab_drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                self.right_tab_drag = Some(RightTabDragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }

    /// The surface that actually renders: the stored pick when it still
    /// exists, else the first remaining tab, else the picker. Terminal keys
    /// go stale when their tab closes/exits — never render a dead surface.
    pub(super) fn resolved_right_active(&self, cx: &App) -> RightSurface {
        let picked = self.panels.get(&self.panel_key(cx)).right_active;
        if matches!(picked, RightSurface::Item | RightSurface::Team) {
            return picked;
        }
        let rows = self.right_surface_rows(cx);
        let exists = match picked {
            RightSurface::Picker => false,
            surface => rows.iter().any(|(s, _, _, _)| *s == surface),
        };
        if exists {
            picked
        } else {
            rows.first()
                .map(|(s, _, _, _)| *s)
                .unwrap_or(RightSurface::Picker)
        }
    }

    pub(super) fn suspend_file_images(&mut self, cx: &mut Context<Self>) {
        for files in self.files.values().chain(self.file_surfaces.values()) {
            files.update(cx, |files, cx| files.suspend_images(cx));
        }
    }

    pub(super) fn set_right_active(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        if surface != RightSurface::Team {
            self.stop_tracker_sync();
        }
        if self.resolved_right_active(cx) != surface {
            self.suspend_file_images(cx);
        }
        let key = self.panel_key(cx);
        self.panels.update(&key, |p| p.right_active = surface);
        match surface {
            RightSurface::File(id) => {
                if let Some(file) = self.file_surfaces.get(&id).cloned() {
                    file.update(cx, |file, cx| file.ensure_loaded(cx));
                }
            }
            RightSurface::Terminal(tab) => {
                let panel = self.right_terminal_panel(cx);
                self.composer
                    .update(cx, |composer, _| composer.focus_pending = false);
                panel.update(cx, |panel, cx| {
                    panel.select_tab_by_key(tab, cx);
                    panel.request_focus(cx);
                });
            }
            RightSurface::Diff(id) => {
                if let Some(changes) = self.diffs.get(&id).cloned() {
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                }
            }
            // The tab's feed (watch or snapshot) runs from open to close —
            // activation needs no revalidation.
            RightSurface::Subagent(_)
            | RightSurface::Browser(_)
            | RightSurface::Artifact(_)
            | RightSurface::SideChat(_) => {}
            RightSurface::Team => {
                let chat_id = self.active_chat.clone();
                self.invalidate_tracker_review();
                self.load_tracker_project(&chat_id, false, cx);
                self.watch_tracker_sync(&chat_id, cx);
            }
            RightSurface::Picker | RightSurface::Item | RightSurface::Review => {}
        }
        self.sync_explorer_selection(cx);
        cx.notify();
    }

    pub(super) fn focus_right_file_editor(
        &mut self,
        surface: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let RightSurface::Browser(id) = surface {
            if let Some(browser) = self.browsers.get(&id).cloned() {
                browser.update(cx, |browser, cx| browser.focus_address(window, cx));
            }
            return;
        }
        let files = match surface {
            RightSurface::File(id) => self.file_surfaces.get(&id).cloned(),
            _ => None,
        };
        if let Some(files) = files {
            files.update(cx, |files, cx| {
                files.focus_editor(window, cx);
            });
        }
    }

    /// Show or hide the blame gutter in every open file surface (and the ones
    /// opened later).
    pub(crate) fn set_blame_gutter_everywhere(
        &mut self,
        enabled: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.blame_gutter = enabled;
        let surfaces = self.file_surfaces.values().cloned().collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| surface.set_blame_gutter(enabled, cx));
        }
        cx.notify();
    }

    /// Open `chat_id`, then scroll its transcript to the turn (the user message
    /// `turn_id`). The transcript may still be hydrating, so retry with growing
    /// gaps for up to ~15 s, then say the turn wasn't found (the chat stays
    /// open).
    pub(crate) fn open_chat_at_turn(
        &mut self,
        chat_id: String,
        turn_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.open_chat(chat_id.clone(), cx);
        let Some(turn_id) = turn_id else { return };
        self.turn_scroll_task = Some(cx.spawn(async move |this, cx| {
            let mut found = false;
            for (attempt, delay_ms) in TURN_SCROLL_RETRY_MS.into_iter().enumerate() {
                if delay_ms > 0 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(delay_ms))
                        .await;
                }
                // Done, still waiting, or the user went elsewhere (stop quietly).
                let outcome = this
                    .update(cx, |this, cx| {
                        if attempt > 0 && this.active_chat != chat_id {
                            return TurnScroll::Abandoned;
                        }
                        let transcript = this.transcript.clone();
                        transcript.update(cx, |transcript, cx| {
                            if transcript.chat_id() != Some(chat_id.as_str()) {
                                return TurnScroll::Waiting;
                            }
                            match transcript
                                .rows()
                                .iter()
                                .position(|row| row.entry_id.as_ref() == turn_id.as_str())
                            {
                                Some(index) => {
                                    transcript.scroll_to_row(index, cx);
                                    TurnScroll::Done
                                }
                                None => TurnScroll::Waiting,
                            }
                        })
                    })
                    .unwrap_or(TurnScroll::Abandoned);
                match outcome {
                    TurnScroll::Done => {
                        found = true;
                        break;
                    }
                    TurnScroll::Abandoned => return,
                    TurnScroll::Waiting => {}
                }
            }
            if !found {
                let _ = this.update(cx, |this, cx| {
                    if this.active_chat == chat_id {
                        this.sidebar_notice = Some("Couldn't find that turn".into());
                        cx.notify();
                    }
                });
            }
        }));
    }

    pub(super) fn set_files_word_wrap(
        &mut self,
        word_wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.files_word_wrap = word_wrap;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_word_wrap(word_wrap, cx));
        }
        let surfaces = self.file_surfaces.values().cloned().collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_word_wrap(word_wrap, window, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Push a new code size into every open file surface. Called by the
    /// Appearance settings page, which owns the control. The typography
    /// global is the canonical store and persists on its own; this only
    /// propagates the change to already-open surfaces.
    pub(crate) fn set_code_font_size(&mut self, code_font_size: f32, cx: &mut Context<Self>) {
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_editor_font_size(code_font_size, cx)
            });
        }
        cx.notify();
    }

    pub(super) fn set_files_show_all(&mut self, show_all_files: bool, cx: &mut Context<Self>) {
        self.settings.files_show_all = show_all_files;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_show_all_files(show_all_files, cx));
        }
        let surfaces = self
            .file_surfaces
            .values()
            .chain(self.files.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_show_all_files(show_all_files, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn session_links(
        source_session: Option<String>,
        cx: &Context<Self>,
    ) -> crate::markdown::render::LinkUi {
        let shell = cx.weak_entity();
        crate::markdown::render::LinkUi {
            source_session,
            handler: std::rc::Rc::new(move |activation, window, cx| {
                shell
                    .update(cx, |shell, cx| {
                        shell.activate_session_link(activation, window, cx)
                    })
                    .unwrap_or(crate::markdown::render::LinkOutcome::Rejected)
            }),
        }
    }

    pub(super) fn activate_session_link(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::markdown::render::LinkOutcome {
        use crate::markdown::render::{LinkAction, LinkOutcome};
        if self.active_chat.is_empty()
            || activation.source_session.as_deref() != Some(self.active_chat.as_str())
            || self.state.read(cx).selected_chat.as_deref() != Some(self.active_chat.as_str())
        {
            return LinkOutcome::Rejected;
        }
        if let Some(outcome) = self.activate_pinned_code_ref(activation, window, cx) {
            return outcome;
        }
        // Session references always stay in Mast, independent of web-link settings.
        if activation.target.is_session() {
            if !matches!(
                activation.action,
                LinkAction::Primary | LinkAction::Internal
            ) {
                return LinkOutcome::Rejected;
            }
            let chat_id = if let Some(id) =
                mast_proto::session_link::parse_session_link(&activation.target.original)
            {
                id
            } else {
                let Ok(link) =
                    crate::links::parse_mast_conversation_link(&activation.target.original)
                else {
                    return LinkOutcome::Rejected;
                };
                let state = self.state.read(cx);
                if crate::links::workspace_locator(
                    state.workspace_scope,
                    state.auth.as_ref(),
                    state.local_device_id.as_deref(),
                )
                .as_deref()
                    != Some(link.workspace.as_str())
                {
                    self.sidebar_notice =
                        Some("This conversation link belongs to another workspace".into());
                    cx.notify();
                    return LinkOutcome::Rejected;
                }
                link.chat_id
            };
            if !self
                .state
                .read(cx)
                .chats
                .iter()
                .any(|chat| chat.id == chat_id)
            {
                self.sidebar_notice = Some("The linked conversation was not found".into());
                cx.notify();
                return LinkOutcome::Rejected;
            }
            self.open_chat(chat_id, cx);
            return LinkOutcome::Internal;
        }
        if activation.target.navigation.is_err() {
            if crate::artifacts::model::link_target(&activation.target.original).is_some() {
                if activation.action == LinkAction::Copy {
                    return LinkOutcome::Rejected;
                }
                self.leave_overview_for_chat(cx);
                self.open_artifact_link(&activation.target.original, window, cx);
                return LinkOutcome::Internal;
            }
            if let Some(outcome) = self.activate_workspace_artifact(activation, window, cx) {
                if outcome != LinkOutcome::Rejected {
                    self.leave_overview_for_chat(cx);
                }
                return outcome;
            }
            if !matches!(
                activation.action,
                LinkAction::Primary | LinkAction::Internal
            ) {
                return LinkOutcome::Rejected;
            }
            if self.open_workspace_file_link(&activation.target.original, window, cx) {
                self.leave_overview_for_chat(cx);
                return LinkOutcome::Internal;
            }
            // A local path outside the chat's project: the file viewer refuses
            // it, so offer the artifact import (owner) or explain (remote).
            return if self.show_outside_file_prompt(&activation.target.original, cx) {
                self.leave_overview_for_chat(cx);
                LinkOutcome::Internal
            } else {
                LinkOutcome::Rejected
            };
        }
        let mut resolved = activation.clone();
        if resolved.action == LinkAction::Primary {
            resolved.action = if crate::settings::current(cx).open_web_links_in_mast {
                LinkAction::Internal
            } else {
                LinkAction::External
            };
        }
        let outcome = resolved.web_outcome(cfg!(any(target_os = "macos", target_os = "linux")));
        if outcome == LinkOutcome::Internal {
            self.leave_overview_for_chat(cx);
            match activation.target.navigation.clone().ok() {
                Some(url) => self.open_browser_url(url, window, cx),
                None => {
                    self.set_surfaces_open(true, cx);
                    self.add_browser_surface(None, window, cx);
                }
            }
        }
        outcome
    }
    /// Show `url` in the current session's side browser: focus the tab that
    /// already shows it, else open a new one. Opens the pane if it is closed.
    pub(super) fn open_browser_url(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_surfaces_open(true, cx);
        if let Some(id) = self.browser_tab_showing(&url, cx) {
            self.set_right_active(RightSurface::Browser(id), cx);
            cx.notify();
            return;
        }
        self.add_browser_surface(Some(url), window, cx);
    }

    /// The current session's browser tab already on `url`, if any.
    pub(super) fn browser_tab_showing(&self, url: &str, cx: &App) -> Option<u64> {
        self.right_tabs
            .get(&self.panel_key(cx))?
            .iter()
            .find_map(|surface| {
                let RightSurface::Browser(id) = surface else {
                    return None;
                };
                let page = self.browsers.get(id)?.read(cx).page.url.clone()?;
                crate::browser::model::same_page(&page, url).then_some(*id)
            })
    }

    /// Browser tabs are independent instances owned by the current session.
    pub(super) fn add_browser_surface(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context = self.browser_context.clone();
        let Some(browser) = self.create_browser_surface(context, window, cx) else {
            return;
        };
        if let Some(handle) = self.state.read(cx).engine().cloned() {
            let chat_id = self.active_chat.clone();
            browser.update(cx, |browser, cx| {
                browser.watch_previews(handle, chat_id, cx)
            });
        }
        browser.update(cx, |browser, cx| {
            if let Some(url) = url {
                browser.navigate(&url, window, cx);
            } else {
                browser.focus_address(window, cx);
            }
        });
    }

    /// A new, selected Browser tab in the current session's right pane.
    pub(super) fn create_browser_surface(
        &mut self,
        context: crate::browser::BrowserContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::browser::BrowserSurface>> {
        if self.active_chat.is_empty() {
            return None;
        }
        let key = self.panel_key(cx);
        let remote = {
            let state = self.state.read(cx);
            state.selected_chat_row().is_some_and(|chat| {
                Some(chat.device_id.as_str()) != state.local_device_id.as_deref()
            })
        };
        self.browser_seq += 1;
        let id = self.browser_seq;
        let browser = cx.new(|cx| crate::browser::BrowserSurface::new(context, remote, window, cx));
        let owner = key.clone();
        let sub = cx.subscribe_in(&browser, window, move |this, _, event, window, cx| {
            match event {
                crate::browser::BrowserEvent::Changed => cx.notify(),
                crate::browser::BrowserEvent::NewTab(url) => {
                    // A background page cannot open a tab in the wrong session.
                    if this.panel_key(cx) == owner
                        && this.resolved_right_active(cx) == RightSurface::Browser(id)
                    {
                        this.add_browser_surface(url.clone(), window, cx);
                    }
                }
                crate::browser::BrowserEvent::ViewSource(path) => {
                    if this.panel_key(cx) == owner {
                        this.add_file_surface(path.clone(), window, cx);
                    }
                }
                crate::browser::BrowserEvent::Close => {
                    this.close_right_surface(RightSurface::Browser(id), window, cx)
                }
                // Only static documents (artifacts) report raw links.
                crate::browser::BrowserEvent::DocumentLink(_)
                | crate::browser::BrowserEvent::DocumentMessage(_) => {}
            }
        });
        self.browsers.insert(id, browser.clone());
        self.browser_subs.insert(id, sub);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Browser(id));
        self.set_right_active(RightSurface::Browser(id), cx);
        Some(browser)
    }

    /// The active chat's checkout root and whether it lives on this device.
    pub(super) fn active_checkout(&self, cx: &App) -> Option<(String, bool)> {
        let state = self.state.read(cx);
        let chat = state
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)?;
        let root =
            crate::files::client::FilesRequestContext::for_chat(state, &self.active_chat)?.cwd;
        let local = state.local_device_id.as_deref() == Some(chat.device_id.as_str());
        Some((root, local))
    }
    /// Route Codex visualization files and the explicit file actions. `None`
    /// leaves the link to ordinary workspace-file handling.
    pub(super) fn activate_workspace_artifact(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::markdown::render::LinkOutcome> {
        use crate::markdown::render::{LinkAction, LinkOutcome};
        use mast_proto::visualize::{
            HtmlArtifactKind, default_title, fragment_sibling, html_artifact_kind, preview_sibling,
        };
        let (root, local) = self.active_checkout(cx)?;
        let link = resolve_workspace_file_link(&activation.target.original, &root)?;
        let local_root = local.then(|| std::path::PathBuf::from(&root));
        let label = activation.target.label.trim();
        let title_for = |path: &str| {
            if !label.is_empty() && label != activation.target.original && !label.ends_with(".html")
            {
                label.to_owned()
            } else {
                default_title(path).unwrap_or_else(|| "Visualization".into())
            }
        };
        let source = |path: String, wide: bool| crate::browser::FragmentSource {
            title: title_for(&path),
            path,
            wide,
            local_root: local_root.clone(),
        };
        let local_file = |path: &str| {
            local_root
                .as_ref()
                .map(|root| root.join(path))
                .filter(|path| path.is_file())
        };
        let kind = html_artifact_kind(&link.path);
        match (activation.action, kind) {
            (LinkAction::Source, _) => {
                self.set_surfaces_open(true, cx);
                self.add_file_surface_at(link.path, link.line, link.column, window, cx);
                Some(LinkOutcome::Internal)
            }
            (LinkAction::Reveal, _) => Some(match local_file(&link.path) {
                Some(path) => {
                    cx.reveal_path(&path);
                    LinkOutcome::Internal
                }
                None => LinkOutcome::Rejected,
            }),
            (LinkAction::Visualize { wide }, _) => {
                let source = source(link.path, wide);
                self.open_fragment_surface(source, None, window, cx);
                Some(LinkOutcome::Internal)
            }
            (LinkAction::Primary | LinkAction::Internal, Some(HtmlArtifactKind::Fragment)) => {
                let source = source(link.path, false);
                self.open_fragment_surface(source, None, window, cx);
                Some(LinkOutcome::Internal)
            }
            (LinkAction::External, Some(HtmlArtifactKind::Fragment)) => {
                let preview = preview_sibling(&link.path);
                if let Some(url) = preview
                    .as_deref()
                    .and_then(local_file)
                    .and_then(|path| crate::browser::file_url(&path))
                {
                    return Some(LinkOutcome::External(url));
                }
                let notice = match (preview, local) {
                    (Some(preview), true) => format!(
                        "No standalone export at {preview}. Ask the agent to export one with the visualize skill’s render.py."
                    ),
                    _ => "Previews open in a browser on the device that has the file.".into(),
                };
                let source = source(link.path, false);
                self.open_fragment_surface(source, Some(notice), window, cx);
                Some(LinkOutcome::Internal)
            }
            (
                LinkAction::Primary | LinkAction::External | LinkAction::Internal,
                Some(HtmlArtifactKind::Preview),
            ) => {
                if activation.action != LinkAction::Internal
                    && let Some(url) =
                        local_file(&link.path).and_then(|path| crate::browser::file_url(&path))
                {
                    return Some(LinkOutcome::External(url));
                }
                let fragment = fragment_sibling(&link.path)?;
                let notice = if activation.action == LinkAction::Internal {
                    None
                } else if local {
                    Some(format!(
                        "{} wasn’t found, so its fragment is shown instead.",
                        link.path
                    ))
                } else {
                    Some(
                        "The preview is on another device, so its fragment is shown instead."
                            .into(),
                    )
                };
                let source = source(fragment, false);
                self.open_fragment_surface(source, notice, window, cx);
                Some(LinkOutcome::Internal)
            }
            _ => None,
        }
    }

    /// Show a visualization in the right pane, reusing an open tab for the
    /// same file (which reloads it).
    pub(crate) fn open_fragment_surface(
        &mut self,
        source: crate::browser::FragmentSource,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        self.set_surfaces_open(true, cx);
        let key = self.panel_key(cx);
        let existing = self.right_tabs.get(&key).and_then(|tabs| {
            tabs.iter().find_map(|tab| match tab {
                RightSurface::Browser(id) => self
                    .browsers
                    .get(id)
                    .filter(|browser| {
                        browser
                            .read(cx)
                            .fragment()
                            .is_some_and(|view| view.source.path == source.path)
                    })
                    .map(|browser| (*id, browser.clone())),
                _ => None,
            })
        });
        if let Some((id, browser)) = existing {
            self.set_right_active(RightSurface::Browser(id), cx);
            browser.update(cx, |browser, cx| {
                browser.set_fragment_notice(notice, cx);
                browser.reload_fragment(window, cx);
            });
            return;
        }
        let reader = {
            let state = self.state.read(cx);
            crate::files::client::FilesRequestContext::for_chat(state, &self.active_chat).and_then(
                |context| {
                    let engine = state.engine()?.clone();
                    let target = context.target.clone();
                    Some((
                        crate::files::client::WorkspaceFilesClient::new(engine, context),
                        target,
                    ))
                },
            )
        };
        // Untrusted fragments get their own ephemeral website data store:
        // nothing they load shares cookies or storage with Browser tabs.
        let Some(browser) =
            self.create_browser_surface(crate::browser::BrowserContext::default(), window, cx)
        else {
            return;
        };
        browser.update(cx, |browser, cx| {
            browser.open_fragment(source, reader, notice, window, cx)
        });
    }

    pub(crate) fn open_request_file(
        &mut self,
        chat_id: String,
        path: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chat(chat_id.clone(), cx);
        cx.defer_in(window, move |this, window, cx| {
            if this.active_chat == chat_id {
                let target = match line {
                    Some(line) => format!("{path}:{line}"),
                    None => path,
                };
                this.open_workspace_file_link(&target, window, cx);
            }
        });
    }

    pub(crate) fn open_request_diff(
        &mut self,
        chat_id: String,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let checkout = {
            let state = self.state.read(cx);
            let targets = state.chat_diff_targets(&chat_id);
            targets
                .iter()
                .find(|target| {
                    branch
                        .as_ref()
                        .is_some_and(|branch| target.branch.as_ref() == Some(branch))
                })
                .or_else(|| targets.iter().find(|target| target.selected))
                .or_else(|| targets.first())
                .map(|target| target.path.clone())
        };
        if let Some(path) = checkout {
            let task = self.state.update(cx, |state, cx| {
                state.clear_chat_diff_member_pick(&chat_id);
                state.set_chat_diff_target(
                    &chat_id,
                    mast_proto::SetChatDiffTargetAction::Select,
                    Some(path),
                    cx,
                )
            });
            task.detach();
        } else if let Some(branch) = branch {
            let task = self.state.update(cx, |state, cx| {
                state.clear_chat_diff_member_pick(&chat_id);
                state.bind_chat_ref_target(&chat_id, branch, None, cx)
            });
            task.detach();
        }
        self.open_chat(chat_id.clone(), cx);
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            this.update(cx, |this, cx| {
                this.set_surfaces_open(true, cx);
                let changes = cx.new(|cx| Changes::for_chat(this.state.clone(), chat_id, cx));
                changes.update(cx, |changes, cx| {
                    changes.set_scope(crate::changes::DiffScope::Branch, cx)
                });
                this.register_diff_surface(changes, cx);
            })
            .ok();
        });
    }
    pub(super) fn add_diff_surface(&mut self, cx: &mut Context<Self>) {
        let chat_id = self.active_chat.clone();
        let changes = cx.new(|cx| Changes::for_chat(self.state.clone(), chat_id, cx));
        self.register_diff_surface(changes, cx);
    }

    /// Open or focus a session-owned editor tab. The explorer is independent.
    pub(super) fn add_file_surface(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_file_surface_at(path, None, None, window, cx);
    }

    fn active_file_location(&self, cx: &App) -> Option<super::file_navigation::FileLocation> {
        let RightSurface::File(id) = self.resolved_right_active(cx) else {
            return None;
        };
        let file = self.file_surfaces.get(&id)?;
        Some(super::file_navigation::FileLocation {
            path: self.file_surface_paths.get(&id)?.clone(),
            line: file.read(cx).current_code_line(cx),
        })
    }

    pub(super) fn record_file_navigation(
        &mut self,
        from: Option<super::file_navigation::FileLocation>,
        to: super::file_navigation::FileLocation,
        cx: &App,
    ) {
        if self.replaying_file_navigation {
            return;
        }
        let key = self.workspace_recent_key(cx);
        if !self.file_navigation.contains_key(&key) && self.file_navigation.len() >= 32 {
            if let Some(oldest) = self.file_navigation.keys().next().cloned() {
                self.file_navigation.remove(&oldest);
            }
        }
        let history = self.file_navigation.entry(key).or_default();
        if let Some(from) = from {
            history.record(from);
        }
        history.record(to);
    }

    pub(super) fn move_file_navigation(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.workspace_recent_key(cx);
        let Some(location) = self
            .file_navigation
            .get_mut(&key)
            .and_then(|history| history.move_by(forward))
        else {
            return;
        };
        self.replaying_file_navigation = true;
        self.add_file_surface_at(location.path, Some(location.line), None, window, cx);
        self.replaying_file_navigation = false;
    }

    pub(super) fn add_file_surface_at(
        &mut self,
        path: String,
        line: Option<u32>,
        column: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        // A pop-out already holds this file: focus it rather than opening a
        // second editor over the same buffer.
        if crate::popout::focus_file(
            &self.active_chat,
            &path,
            line.map(|line| (line, column)),
            cx,
        ) {
            return;
        }
        let from = self.active_file_location(cx);
        let target_line = line.unwrap_or_else(|| {
            let lookup = (self.panel_key(cx), path.clone());
            self.file_surface_keys
                .get(&lookup)
                .and_then(|id| self.file_surfaces.get(id))
                .map(|file| file.read(cx).current_code_line(cx))
                .unwrap_or(1)
        });
        self.record_file_navigation(
            from,
            super::file_navigation::FileLocation {
                path: path.clone(),
                line: target_line,
            },
            cx,
        );
        let recent_key = self.workspace_recent_key(cx);
        let recent = self.workspace_file_recents.entry(recent_key).or_default();
        recent.retain(|item| item != &path);
        recent.insert(0, path.clone());
        recent.truncate(100);
        self.set_surfaces_open(true, cx);
        let panel_key = self.panel_key(cx);
        let lookup = (panel_key.clone(), path.clone());
        if let Some(id) = self.file_surface_keys.get(&lookup).copied() {
            let surface = RightSurface::File(id);
            if let Some(file) = self.file_surfaces.get(&id) {
                file.update(cx, |file, cx| file.clear_pinned_reference(cx));
            }
            self.set_right_active(surface, cx);
            if let Some(line) = line
                && let Some(file) = self.file_surfaces.get(&id).cloned()
            {
                file.update(cx, |file, cx| file.reveal_editor_location(line, column, cx));
            }
            self.focus_right_file_editor(surface, window, cx);
            return;
        }

        self.file_surface_seq += 1;
        let id = self.file_surface_seq;
        let file = cx.new(|cx| {
            FilesSurface::new_editor(
                self.state.clone(),
                self.active_chat.clone(),
                path.clone(),
                self.settings.files_autosave_enabled,
                self.settings.files_autosave_delay_ms,
                crate::typography::code_font_size(cx),
                self.settings.files_word_wrap,
                self.settings.files_show_all,
                cx,
            )
        });
        if self.blame_gutter {
            file.update(cx, |file, cx| file.set_blame_gutter(true, cx));
        }
        let event_panel_key = panel_key.clone();
        let sub = cx.subscribe_in(
            &file,
            window,
            move |this: &mut Self, source, event, window, cx| {
                if matches!(
                    event,
                    FilesEvent::OpenFile(_)
                        | FilesEvent::RevealFile(_)
                        | FilesEvent::Jumped { .. }
                        | FilesEvent::OpenCommit(_)
                ) && !this.accepts_file_navigation(&event_panel_key, source, cx)
                {
                    return;
                }
                match event {
                    FilesEvent::OpenFile(path) => this.add_file_surface(path.clone(), window, cx),
                    FilesEvent::Jumped {
                        path,
                        from_line,
                        line,
                    } => {
                        this.record_file_navigation(
                            Some(super::file_navigation::FileLocation {
                                path: path.clone(),
                                line: *from_line,
                            }),
                            super::file_navigation::FileLocation {
                                path: path.clone(),
                                line: *line,
                            },
                            cx,
                        );
                    }
                    FilesEvent::OpenCommit(commit) => this.add_commit_diff_surface(
                        commit.clone(),
                        Some(this.active_chat.clone()),
                        cx,
                    ),
                    FilesEvent::RevealFile(path) => {
                        this.add_files_surface(window, cx);
                        if let Some(files) = this.files.get(&this.panel_key(cx)).cloned() {
                            files.update(cx, |files, cx| {
                                files.reveal_file_explicit(path.clone(), cx)
                            });
                        }
                    }
                    FilesEvent::OpenWebLink(activation) => {
                        if let crate::markdown::render::LinkOutcome::External(url) =
                            this.activate_session_link(activation, window, cx)
                        {
                            cx.open_url(&url);
                        }
                    }
                    FilesEvent::OpenChat { chat_id, turn_id } => {
                        this.open_chat_at_turn(chat_id.clone(), turn_id.clone(), cx)
                    }
                    FilesEvent::BlameGutterChanged(enabled) => {
                        this.set_blame_gutter_everywhere(*enabled, window, cx)
                    }
                    FilesEvent::TitleChanged => cx.notify(),
                    FilesEvent::FileRenamed { old_path, new_path } => {
                        this.rename_file_surface(id, &event_panel_key, old_path, new_path, cx)
                    }
                    FilesEvent::WordWrapChanged(word_wrap) => {
                        this.set_files_word_wrap(*word_wrap, window, cx)
                    }
                    FilesEvent::ShowAllFilesChanged(show_all_files) => {
                        this.set_files_show_all(*show_all_files, cx)
                    }
                    FilesEvent::CloseReady => {
                        this.on_file_close_ready(RightSurface::File(id), &event_panel_key, cx)
                    }
                    FilesEvent::CloseCancelled => {
                        this.cancel_file_close(RightSurface::File(id), cx)
                    }
                }
            },
        );
        if let Some(line) = line {
            file.update(cx, |file, cx| file.reveal_editor_location(line, column, cx));
        }
        self.file_surfaces.insert(id, file);
        self.file_surface_paths.insert(id, path);
        self.file_surface_keys.insert(lookup, id);
        self.file_surface_subs.insert(id, sub);
        push_unique_right_surface(
            self.right_tabs.entry(panel_key).or_default(),
            RightSurface::File(id),
        );
        self.set_right_active(RightSurface::File(id), cx);
    }
    pub(super) fn open_workspace_file_link(
        &mut self,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Resolve with the same checkout used by FilesSurface. Resolving
        // against chat.cwd first would prefix a bound-worktree link twice.
        let Some(context) = crate::files::client::FilesRequestContext::for_chat(
            self.state.read(cx),
            &self.active_chat,
        ) else {
            return false;
        };
        let Some(link) = resolve_workspace_file_link(target, &context.cwd) else {
            return false;
        };

        let key = self.panel_key(cx);
        let was_open = self.panels.get(&key).changes_open;
        let from = self.right_target(cx);
        self.panels.update(&key, |panel| panel.changes_open = true);
        if !was_open {
            self.right_tween = Some(WidthTween::new(from, self.right_target(cx)));
        }
        self.add_file_surface_at(link.path, link.line, link.column, window, cx);
        true
    }
    pub(super) fn rename_file_surface(
        &mut self,
        id: u64,
        panel_key: &str,
        old_path: &str,
        new_path: &str,
        cx: &mut Context<Self>,
    ) {
        if self.file_surface_paths.get(&id).map(String::as_str) != Some(old_path) {
            return;
        }
        self.file_surface_paths.insert(id, new_path.to_string());
        self.file_surface_keys
            .remove(&(panel_key.to_string(), old_path.to_string()));
        self.file_surface_keys
            .entry((panel_key.to_string(), new_path.to_string()))
            .or_insert(id);
        cx.notify();
    }

    /// The dedicated History surface. Keeping it as its own tab preserves its
    /// graph/search state while Diff tabs retain their ordinary scope picker.
    pub(super) fn add_history_surface(&mut self, cx: &mut Context<Self>) {
        let chat_id = self.active_chat.clone();
        let history = cx.new(|cx| Changes::history_for_chat(self.state.clone(), chat_id, cx));
        self.register_diff_surface(history, cx);
    }

    /// A History row click: the commit opens as its own pinned diff tab
    /// (user request).
    pub(super) fn add_commit_diff_surface(
        &mut self,
        commit: mast_proto::GitHistoryCommit,
        owner_chat_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let chat_id = owner_chat_id.unwrap_or_else(|| self.active_chat.clone());
        let changes =
            cx.new(|cx| Changes::for_commit_for_chat(self.state.clone(), commit, chat_id, cx));
        self.register_diff_surface(changes, cx);
    }

    pub(super) fn register_diff_surface(
        &mut self,
        changes: Entity<Changes>,
        cx: &mut Context<Self>,
    ) {
        self.diff_seq += 1;
        let id = self.diff_seq;
        let sub = cx.subscribe(&changes, |this: &mut Self, source, event, cx| match event {
            ChangesEvent::OpenCommit(commit) => {
                let owner_chat_id = source.read(cx).owner_chat_id().map(str::to_string);
                this.add_commit_diff_surface(commit.clone(), owner_chat_id, cx);
            }
        });
        self.diffs.insert(id, changes);
        self.diff_subs.insert(id, sub);
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Diff(id));
        self.set_right_active(RightSurface::Diff(id), cx);
    }

    /// The picker's Terminal card / the `+` menu's Terminal row: every click
    /// opens a fresh embedded terminal tab.
    pub(super) fn add_terminal_surface(&mut self, cx: &mut Context<Self>) {
        let panel = self.right_terminal_panel(cx);
        let opened = panel.update(cx, |panel, cx| {
            panel.set_open(true, cx);
            panel.open_tab_for_selected(cx)
        });
        if let Some(tab) = opened {
            let key = self.panel_key(cx);
            self.right_tabs
                .entry(key)
                .or_default()
                .push(RightSurface::Terminal(tab));
            self.set_right_active(RightSurface::Terminal(tab), cx);
        }
    }

    /// Spawn-chip events from the primary transcript AND from subagent-tab
    /// transcripts (nested spawns open their own tabs).
    pub(super) fn on_transcript_event(
        &mut self,
        _: Entity<Transcript>,
        event: &TranscriptEvent,
        cx: &mut Context<Self>,
    ) {
        self.handle_transcript_event(event, cx);
    }
    pub(super) fn sign_in_for_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some((device, harness)) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| {
                chat.config
                    .as_ref()
                    .map(|config| (chat.device_id.clone(), config.harness))
            })
        else {
            return;
        };
        let rpc = self.state.read(cx).accounts_rpc(Some(&device));
        self.open_settings(SettingsSection::Agents, cx);
        if self.accounts_page.is_none() {
            let state = self.state.clone();
            self.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
        }
        let page = self
            .accounts_page
            .clone()
            .expect("accounts page initialized");
        cx.spawn(async move |_, cx| {
            let snapshot = rpc
                .call(
                    methods::LIST_MANAGED_ACCOUNTS,
                    crate::managed_accounts::list_params(false),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::ManagedAccountsSnapshot>(value)
                        .map_err(|error| error.to_string())
                });
            let result = snapshot.and_then(|snapshot| {
                snapshot
                    .chat_pins
                    .iter()
                    .find(|pin| pin.chat_id == chat_id && pin.harness == harness)
                    .map(|pin| {
                        let account_id = pin.account_id.clone();
                        let label = account_id
                            .as_ref()
                            .and_then(|id| {
                                snapshot.accounts.iter().find(|account| &account.id == id)
                            })
                            .map(|account| account.label.clone());
                        (account_id, label, snapshot.clone())
                    })
                    .ok_or_else(|| "Could not find the account pinned to this chat.".to_string())
            });
            page.update(cx, |page, cx| match result {
                Ok((account_id, label, snapshot)) => {
                    page.sign_in_for_chat(device, harness, account_id, label, snapshot, cx)
                }
                Err(error) => page.sign_in_error(error, cx),
            });
        })
        .detach();
    }

    /// A spawn chip's "Open subagent": focus the existing tab for that doc,
    /// or open one. `frozen` (subagent done/failed) tries the uploaded
    /// transcript blob first and falls back to the live doc watch; running
    /// subagents watch the doc directly.
    pub(super) fn add_subagent_surface(
        &mut self,
        chat_id: String,
        doc_id: String,
        title: String,
        frozen: bool,
        cx: &mut Context<Self>,
    ) {
        // The chip lives in the conversation column — the pane it opens into
        // may still be closed.
        self.set_surfaces_open(true, cx);
        if let Some((&id, _)) = self
            .subagent_tabs
            .iter()
            .find(|(_, tab)| tab.doc_id == doc_id)
        {
            self.set_right_active(RightSurface::Subagent(id), cx);
            return;
        }
        self.subagent_seq += 1;
        let id = self.subagent_seq;
        // A live subagent follows its streaming end (main-transcript feel);
        // a frozen one reads top-down.
        let transcript =
            cx.new(|cx| Transcript::for_doc(self.state.clone(), doc_id.clone(), !frozen, cx));
        let links = Self::session_links(Some(self.active_chat.clone()), cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        let events = cx.subscribe(&transcript, Self::on_transcript_event);
        // The first holder starts the feed (live watch or frozen blob fetch);
        // the state owns it, so it outlives any one window.
        let first = !self
            .state
            .update(cx, |s, _| s.acquire_subagent_doc(&doc_id));
        if first {
            if frozen {
                self.state.update(cx, |s, cx| {
                    s.start_subagent_snapshot_fetch(&chat_id, &doc_id, cx)
                });
            } else {
                self.state
                    .update(cx, |s, cx| s.watch_subagent_doc(doc_id.clone(), cx));
            }
        }
        self.subagent_tabs.insert(
            id,
            SubagentTab {
                doc_id,
                title: title.into(),
                transcript,
                _events: events,
            },
        );
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Subagent(id));
        self.set_right_active(RightSurface::Subagent(id), cx);
    }

    /// A surface tab's ✕. The active fallback happens naturally through
    /// [`Self::resolved_right_active`] on the next frame.
    pub(super) fn close_right_surface(
        &mut self,
        surface: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if surface == RightSurface::Review {
            return;
        }
        let was_active = self.resolved_right_active(cx) == surface;
        let key = self.panel_key(cx);
        let files = match surface {
            RightSurface::File(id) => self.file_surfaces.get(&id).cloned(),
            _ => None,
        };
        if let Some(files) = files {
            match files.update(cx, |files, cx| files.prepare_close(cx)) {
                FilesCloseDisposition::Allow => {
                    self.complete_file_close(surface, &key, cx);
                }
                FilesCloseDisposition::Pending | FilesCloseDisposition::Blocked => {
                    self.pending_file_closes.insert(surface);
                    self.set_right_active(surface, cx);
                }
            }
            return;
        }
        if let Some(tabs) = self.right_tabs.get_mut(&key) {
            tabs.retain(|s| *s != surface);
        }
        match surface {
            RightSurface::File(_) => {}
            RightSurface::Browser(id) => {
                if let Some(browser) = self.browsers.remove(&id) {
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.browser_subs.remove(&id);
                if was_active {
                    window.focus(&self.composer.focus_handle(cx), cx);
                }
            }
            RightSurface::Diff(id) => {
                // Dropping the entity tears down its diff watch.
                self.diffs.remove(&id);
                self.diff_subs.remove(&id);
            }
            RightSurface::Terminal(tab) => {
                let panel = self.right_terminal_panel(cx);
                panel.update(cx, |panel, cx| panel.close_tab_by_key(tab, window, cx));
            }
            RightSurface::Subagent(id) => {
                // Unwatch drops the watch task — that cancels the engine-side
                // watch and unpins the subagent doc from the engine LRU.
                if let Some(tab) = self.subagent_tabs.remove(&id) {
                    self.state
                        .update(cx, |s, _| s.unwatch_subagent_doc(&tab.doc_id));
                }
            }
            RightSurface::Artifact(id) => self.close_artifact_tab(id, cx),
            RightSurface::SideChat(id) => self.drop_side_chat(id, cx),
            RightSurface::Picker
            | RightSurface::Item
            | RightSurface::Team
            | RightSurface::Review => {}
        }
        self.panels.update(&key, |p| {
            if p.right_active == surface {
                p.right_active = RightSurface::Picker;
            }
        });
        self.collapse_surfaces_if_empty(&key, cx);
        cx.notify();
    }
    pub(super) fn on_file_close_ready(
        &mut self,
        surface: RightSurface,
        panel_key: &str,
        cx: &mut Context<Self>,
    ) {
        if self.pending_file_closes.contains(&surface) {
            self.complete_file_close(surface, panel_key, cx);
        } else {
            if self.pending_exit.is_some() {
                self.reveal_unsaved_file(cx);
            }
            cx.notify();
        }
    }

    pub(super) fn cancel_file_close(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        self.pending_file_closes.remove(&surface);
        self.pending_exit = None;
        cx.notify();
    }

    pub fn prepare_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::CloseWindow, cx)
    }

    /// The first rung of `⌘W` / Window > Close Window: when the right pane is
    /// open on a real surface (the file / diff / terminal / browser tab the
    /// user just opened), close THAT and leave the window alone. Returns true
    /// when the close was consumed by the pane.
    ///
    /// The pane's empty picker state and a closed pane both yield false, so the
    /// caller falls through to [`Self::prepare_window_close`] and the window
    /// closes — the same cascade browsers use. The native traffic-light close
    /// deliberately skips this rung: it always closes the window.
    pub fn close_active_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(surface) = self.closable_right_surface(cx) else {
            return false;
        };
        self.close_right_surface(surface, window, cx);
        true
    }

    /// The right pane's closable surface for the `⌘W` cascade: the resolved
    /// active surface while the pane is open, or `None` on the picker empty
    /// state / a closed pane / the new-session canvas.
    pub(super) fn closable_right_surface(&self, cx: &App) -> Option<RightSurface> {
        if !self.right_pane_open(cx) {
            return None;
        }
        match self.resolved_right_active(cx) {
            RightSurface::Picker
            | RightSurface::Item
            | RightSurface::Team
            | RightSurface::Review => None,
            surface => Some(surface),
        }
    }
    pub fn prepare_quit(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::Quit, cx)
    }

    pub(super) fn prepare_exit(&mut self, action: PendingExit, cx: &mut Context<Self>) -> bool {
        // Pop-outs hold files too: every exit path (quit, close window, update
        // install, relaunch, runtime change) answers to them as well.
        // Closing the main window leaves pop-outs open, so only its own files gate it.
        let popouts_ready =
            matches!(action, PendingExit::CloseWindow) || crate::popout::prepare_exit(false, cx);
        let surfaces = self.file_surfaces.values().cloned().collect::<Vec<_>>();
        if surfaces
            .iter()
            .all(|surface| !surface.read(cx).has_unsaved_changes())
        {
            self.pending_exit = if popouts_ready { None } else { Some(action) };
            cx.notify();
            return popouts_ready;
        }
        self.pending_exit = Some(action);
        let mut shell_ready = true;
        for surface in surfaces {
            let disposition = surface.update(cx, |surface, cx| surface.prepare_close(cx));
            shell_ready &= disposition == FilesCloseDisposition::Allow;
        }
        let all_ready = shell_ready && popouts_ready;
        if all_ready {
            self.pending_exit = None;
        } else if !shell_ready {
            self.reveal_unsaved_file(cx);
        }
        cx.notify();
        all_ready
    }

    pub(super) fn reveal_unsaved_file(&mut self, cx: &mut Context<Self>) {
        let editors = self.file_surface_keys.iter().filter_map(|((key, _), id)| {
            self.file_surfaces
                .get(id)
                .filter(|files| files.read(cx).has_unsaved_changes())
                .map(|_| (key.clone(), RightSurface::File(*id)))
        });
        let current = self.panel_key(cx);
        let mut dirty = editors.collect::<Vec<_>>();
        dirty.sort_by_key(|(key, _)| (key != &current, key.clone()));
        if let Some((key, surface)) = dirty.into_iter().next() {
            self.panels.update(&key, |panel| {
                panel.changes_open = true;
                panel.right_active = surface;
            });
            self.apply_nav(NavEntry::Chat(key), cx);
        }
    }

    /// Whether the pending exit may proceed: the main window's files are
    /// saved and, unless only the window is closing, so are the pop-outs'.
    pub(super) fn pending_exit_ready(&self, cx: &App) -> bool {
        self.file_surfaces
            .values()
            .all(|surface| !surface.read(cx).has_unsaved_changes())
            && (matches!(self.pending_exit, Some(PendingExit::CloseWindow))
                || crate::popout::all_flushed(cx))
    }

    pub(super) fn all_file_edits_flushed(&self, cx: &App) -> bool {
        self.file_surfaces
            .values()
            .all(|surface| !surface.read(cx).has_unsaved_changes())
            && crate::popout::all_flushed(cx)
    }

    pub(super) fn complete_file_close(
        &mut self,
        surface: RightSurface,
        panel_key: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(tabs) = self.right_tabs.get_mut(panel_key) {
            tabs.retain(|candidate| *candidate != surface);
        }
        match surface {
            RightSurface::File(id) => {
                self.file_surfaces.remove(&id);
                self.file_surface_paths.remove(&id);
                self.file_surface_subs.remove(&id);
                self.file_surface_keys.retain(|_, value| *value != id);
            }
            _ => return,
        }
        self.pending_file_closes.remove(&surface);
        self.panels.update(panel_key, |panel| {
            if panel.right_active == surface {
                panel.right_active = RightSurface::Picker;
            }
        });
        self.collapse_surfaces_if_empty(panel_key, cx);
        cx.notify();
    }

    /// Right pane — the surface host (t3code RightPanelTabs): hidden by
    /// default, drag-resizable. Content is the ACTIVE surface — the Diff
    /// page (its options row + the lazy [`Changes`] viewer), workspace Files,
    /// an embedded terminal, or the surface picker when no tabs exist.
    pub(super) fn render_right_pane(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(feature = "startup-bench")]
        let _render_span = crate::startup_measurement::MeasurementSpan::new("side_panel");
        let theme = Theme::of(cx).clone();
        let content: AnyElement = if self.right_pane_open(cx) || self.tween_active(self.right_tween)
        {
            match self.resolved_right_active(cx) {
                // Rendering a Files surface activates its image. Keep it unmounted
                // throughout the closing animation after suspending its resources.
                RightSurface::File(_) if !self.right_pane_open(cx) => {
                    gpui::Empty.into_any_element()
                }
                RightSurface::File(id) => {
                    if let Some(file) = self.file_surfaces.get(&id).cloned() {
                        file.update(cx, |file, cx| file.ensure_loaded(cx));
                        file.into_any_element()
                    } else {
                        self.render_surface_picker(cx)
                    }
                }
                RightSurface::Diff(id) if self.diffs.contains_key(&id) => {
                    let changes = self.diffs.get(&id).cloned().expect("checked");
                    // Idempotent — also covers a persisted-open pane on boot.
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                    // The diff options (scope dropdown, ref selector,
                    // fold-all) moved DOWN from the titlebar band — the
                    // surface tabs own that row now; the expand/close
                    // buttons stayed up there (user request).
                    let controls =
                        changes.update(cx, |changes, cx| changes.render_header_controls(cx));
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(crate::surface_chrome::toolbar(&theme).child(controls))
                        .child(div().flex_1().min_h_0().child(changes))
                        .into_any_element()
                }
                RightSurface::Review => self.render_review(false, cx),
                RightSurface::Item => self.render_item_tab(cx),
                RightSurface::Team => self.render_team_tab(cx),
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .cloned()
                    .map(|browser| browser.into_any_element())
                    .unwrap_or_else(|| self.render_surface_picker(cx)),
                RightSurface::Artifact(id) => self
                    .artifact_tab(id)
                    .map(|tab| tab.into_any_element())
                    .unwrap_or_else(|| self.render_surface_picker(cx)),
                RightSurface::SideChat(id) => self
                    .side_chat_pane(id)
                    .cloned()
                    .map(|pane| pane.into_any_element())
                    .unwrap_or_else(|| self.render_surface_picker(cx)),
                RightSurface::Terminal(tab) => {
                    let panel = self.right_terminal_panel(cx);
                    // Keep the embedded panel's own active tab aligned with
                    // the resolved surface (fallbacks can move it).
                    let resize_suspended = self.tween_active(self.right_tween)
                        || self.tween_active(self.files_tween)
                        || self.tween_active(self.sidebar_tween);
                    panel.update(cx, |panel, cx| {
                        panel.set_resize_suspended(resize_suspended);
                        panel.select_tab_by_key(tab, cx);
                    });
                    panel.into_any_element()
                }
                RightSurface::Subagent(id) if self.subagent_tabs.contains_key(&id) => {
                    let transcript = self
                        .subagent_tabs
                        .get(&id)
                        .expect("checked")
                        .transcript
                        .clone();
                    // The pane hosts its own jump pill: the conversation
                    // overlay's is bound to the PRIMARY transcript, and this
                    // one anchors to the pane (no composer stack to clear).
                    let pill = transcript.read(cx).jump_button_shown().then(|| {
                        div()
                            .absolute()
                            .bottom(px(16.0))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(self.jump_pill(
                                "subagent-jump-to-bottom",
                                "subagent-jump-pill",
                                transcript.clone(),
                                cx,
                            ))
                    });
                    // Read-only surface: the transcript fills the pane — no
                    // composer, no status strip.
                    div()
                        .size_full()
                        .relative()
                        .flex()
                        .flex_col()
                        .child(div().flex_1().min_h_0().child(transcript))
                        .children(pill)
                        .into_any_element()
                }
                _ => self.render_surface_picker(cx),
            }
        } else {
            gpui::Empty.into_any_element()
        };
        // Flush panel (user request — the inset card is gone): full window
        // height with a left hairline, glass-friendly like the terminal dock
        // (translucent over the frost; solid otherwise). The resize grabber
        // lives outside this clipped container, on the root layout's seam.
        let panel =
            crate::components::shell_parts::right_panel_frame(&theme, self.right_pane_expanded)
                // With the explorer undocked the panel's right edge IS the
                // window's right edge: it carries the CSD window's rounded corners
                // directly (gpui cannot clip children rounded — each full-bleed
                // layer rounds itself; see [`Self::window_corner_radius`]). Docked,
                // the explorer column is the rightmost layer and rounds instead.
                .when(
                    Self::window_corner_radius(window) > 0.0 && self.files_visible_width(cx) <= 0.0,
                    |el| {
                        let corner = Self::window_corner_radius(window);
                        el.rounded_tr(px(corner)).rounded_br(px(corner))
                    },
                )
                .overflow_hidden()
                // The titlebar is a glass overlay over the full-height content
                // row; the panel's own chrome starts below it.
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .child(content);
        let target = self.right_target(cx);
        let edge_offset = self.eval_resize_edge_bounce(
            self.right_edge_bounce,
            self.right_pane_open(cx) && !self.right_pane_expanded,
        );
        self.right_pane_container(
            self.right_tween,
            target,
            self.right_visible_width(cx),
            edge_offset,
            div().h_full().relative().child(panel).into_any_element(),
        )
    }
    /// The right pane's empty state: a compact vertical list of surface rows
    /// (icon + label). The old two-card grid clipped in narrow panes and
    /// wasted short ones.
    pub(super) fn render_surface_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let text = theme.text;
        let muted = theme.text_muted;
        let border = theme.border;
        let border_strong = theme.border_strong;
        let row = |id: &'static str, icon_path: &'static str, title: &'static str| {
            div()
                .id(id)
                .w_full()
                .h(px(44.0))
                .px(px(14.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(border)
                .bg(crate::theme::ink(0.02))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .hover(move |s| s.bg(crate::theme::ink(0.05)).border_color(border_strong))
                .child(icon(icon_path).size(px(15.0)).flex_none().text_color(muted))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(text)
                        .child(SharedString::from(title)),
                )
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(280.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        row("surface-card-browser", icons::GLOBE, "Browser").on_click(cx.listener(
                            |this, _, window, cx| this.add_browser_surface(None, window, cx),
                        )),
                    )
                    .child(
                        row("surface-card-terminal", icons::TERMINAL, "Terminal").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.add_terminal_surface(cx);
                            }),
                        ),
                    )
                    // Git surfaces only where there IS git — the pane itself
                    // no longer gates on it (terminals work anywhere).
                    .when(self.space_git_detected(cx), |el| {
                        el.child(row("surface-card-diffs", icons::LIST, "Diffs").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.add_diff_surface(cx);
                            }),
                        ))
                        .child(
                            row("surface-card-history", icons::GIT_BRANCH, "History").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.add_history_surface(cx);
                                }),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn collapse_surfaces_if_empty(&mut self, panel_key: &str, cx: &mut Context<Self>) {
        if panel_key == self.panel_key(cx)
            && self.right_tabs.get(panel_key).is_none_or(Vec::is_empty)
        {
            self.set_surfaces_open(false, cx);
        }
    }

    /// Show or hide the surface host portion of the right pane. A no-op when
    /// already in the requested state, so programmatic opens (a file, a
    /// browser link, a subagent chip) never close a pane the user has open.
    pub(super) fn set_surfaces_open(&mut self, open: bool, cx: &mut Context<Self>) {
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
}
