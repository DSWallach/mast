use super::*;

impl Overview {
    pub(crate) fn set_compact_single_chat(&mut self, value: bool, cx: &mut Context<Self>) {
        self.outline
            .update(cx, |outline, cx| outline.set_compact_single_chat(value, cx));
    }

    /// Cmd/Ctrl+F follows the platform Find convention in every Overview
    /// mode (List and Outline share the toolbar search, which also
    /// filters the Outline). Selecting the existing query makes a repeated
    /// search immediately replaceable.
    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let focus = gpui::Focusable::focus_handle(self.search.read(cx), cx);
        window.focus(&focus, cx);
        window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
        true
    }

    /// Whether the toolbar search field holds focus.
    pub(crate) fn search_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        gpui::Focusable::focus_handle(self.search.read(cx), cx).is_focused(window)
    }

    /// Hand focus from the search field to the body of the current mode:
    /// the Outline tree, or the List surface. With `first_row` (Enter /
    /// Down) the first matching row is selected as well, so the arrow keys
    /// work at once.
    pub(super) fn leave_search(&mut self, first_row: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.view_mode {
            OverviewMode::Outline => {
                self.outline.update(cx, |outline, cx| {
                    if first_row {
                        outline.focus_first_row(window, cx);
                    } else {
                        outline.focus(window, cx);
                    }
                });
            }
            OverviewMode::List => {
                window.focus(&self.body_focus, cx);
                if first_row {
                    self.select_list_row(0, cx);
                }
            }
        }
        cx.notify();
    }

    /// Select the `ix`-th rendered List row and scroll it into view.
    pub(super) fn select_list_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some((chat_id, child)) = self.list_order.get(ix).cloned() else {
            return;
        };
        self.reveal_list_item(child);
        self.list_selected = Some(chat_id);
        cx.notify();
    }

    /// Unmeasured rows have no height in GPUI's list summary, so a distant
    /// target must be anchored by index before it can be revealed.
    pub(super) fn reveal_list_item(&self, index: usize) {
        if self.list.bounds_for_item(index).is_some() {
            self.list.scroll_to_reveal_item(index);
        } else {
            self.list.scroll_to(gpui::ListOffset {
                item_ix: index,
                offset_in_item: px(0.0),
            });
        }
    }

    /// Keys on the search field. Esc clears a non-empty query, else returns
    /// focus to the mode's body; Enter / Down move to the first matching
    /// row. Anything else (typing, editing) is left to the field. Esc with
    /// an empty query while a side panel is open falls through to the
    /// Overview's own handler, which closes the panel.
    pub(super) fn search_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if !self.search.read(cx).text().is_empty() {
                    self.search.update(cx, |search, cx| search.set_text("", cx));
                } else if self.work_item_details.is_some()
                    || self.required_actions_panel.is_some()
                    || self.subagent_peek.is_some()
                {
                    return;
                } else {
                    self.leave_search(false, window, cx);
                }
            }
            "enter" | "down" => self.leave_search(true, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    /// Keys on the focused List surface (not on an input inside a row):
    /// Up / Down move the selection (Up from the first row returns to the
    /// search), Enter opens the selected chat in the side panel.
    pub(super) fn list_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view_mode != OverviewMode::List
            || !self.body_focus.is_focused(window)
            || event.keystroke.modifiers.modified()
        {
            return;
        }
        let current = self
            .list_selected
            .as_ref()
            .and_then(|id| self.list_order.iter().position(|(row, _)| row == id));
        match event.keystroke.key.as_str() {
            "down" => match current {
                Some(ix) if ix + 1 < self.list_order.len() => self.select_list_row(ix + 1, cx),
                Some(_) => {}
                None => self.select_list_row(0, cx),
            },
            "up" => match current {
                Some(ix) if ix > 0 => self.select_list_row(ix - 1, cx),
                _ => {
                    self.focus_search(window, cx);
                }
            },
            "enter" => match self.list_selected.clone() {
                Some(chat_id) if current.is_some() => self.open_chat_panel(chat_id, cx),
                _ => self.select_list_row(0, cx),
            },
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    /// Click-to-expand (list row). Expanding also acknowledges an
    /// unread done — web `toggleExpanded` → `acknowledgeDone`.
    pub(super) fn toggle_expanded(&mut self, chat_id: &str, cx: &Context<Self>) {
        if !self.expanded.remove(chat_id) {
            self.expanded.insert(chat_id.to_string());
            self.acknowledge_done(chat_id, cx);
        }
    }

    /// Web `acknowledgeDone`: only meaningful while the chat is actually
    /// done — acknowledging a non-done chat would be reconciled straight
    /// back out on the next row rebuild anyway, so it's skipped up front.
    pub(super) fn acknowledge_done(&mut self, chat_id: &str, cx: &Context<Self>) {
        let is_done = self
            .rows_cache
            .iter()
            .any(|r| r.chat.id == chat_id && r.status == ChatIndicator::Completed);
        if is_done && self.acknowledged_done.insert(chat_id.to_string()) {
            self.save_string_set(
                overview_positions::ACKNOWLEDGED_DONE_FILE,
                &self.acknowledged_done,
                cx,
            );
        }
    }

    /// One idle-refresh tick (see [`IDLE_REFRESH`]). Skipped entirely while
    /// the overview entity is alive but another route is on screen. Re-runs
    /// every visible row's TTL-gated fetches (no-ops when fresh), then
    /// repaints for relative times. Does NOT mark rows dirty (except the ~60s
    /// staleness re-derive) — a fetch that lands with changed data does that
    /// itself.
    pub(super) fn refresh_tick(&mut self, cx: &mut Context<Self>) {
        let visible = self.shell.upgrade().is_some_and(|shell| {
            let shell = shell.read(cx);
            shell.is_overview_route() || shell.overview_navigation_visible()
        });
        if !visible {
            return;
        }
        self.refresh_ticks = self.refresh_ticks.wrapping_add(1);
        let targets: Vec<(String, bool)> = self
            .rows_cache
            .iter()
            .map(|r| (r.chat.id.clone(), r.chat.harness_session_id.is_some()))
            .collect();
        for (chat_id, imported) in targets {
            self.ensure_row_fetches(chat_id, imported, cx);
        }
        if self
            .refresh_ticks
            .is_multiple_of(STALENESS_REDERIVE_EVERY_TICKS)
        {
            self.rows_dirty = true;
        }
        cx.notify();
    }
    /// Every per-row TTL-gated lookup, in one place so `rows()` and
    /// `refresh_tick` can't drift. Subagents only apply to chats with a
    /// harness session (anything else has no transcript to scan, per
    /// `SCAN_CHAT_SUBAGENTS`'s contract).
    /// Context usage applies to every chat: its live source is the engine's
    /// own doc store, not an import cursor.
    pub(super) fn ensure_row_fetches(
        &mut self,
        chat_id: String,
        has_harness_session: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .row_fetch_due
            .get(&chat_id)
            .is_some_and(|(had_session, due)| {
                *had_session == has_harness_session && Instant::now() < *due
            })
        {
            return;
        }
        self.perf.fetch_checks += 1;
        if has_harness_session {
            self.ensure_subagents(chat_id.clone(), cx);
        }
        self.ensure_context_usage(chat_id.clone(), cx);
        self.ensure_link_status(chat_id.clone(), cx);
        match self.next_row_fetch_due(&chat_id, has_harness_session) {
            Some(due) => {
                self.row_fetch_due
                    .insert(chat_id, (has_harness_session, due));
            }
            None => {
                self.row_fetch_due.remove(&chat_id);
            }
        }
    }

    /// When the soonest of a chat's row fetches goes stale. `None` when any
    /// is in flight or has no recorded result yet (so it must be re-checked
    /// next time rather than skipped).
    pub(super) fn next_row_fetch_due(&self, chat_id: &str, has_harness_session: bool) -> Option<Instant> {
        if self.link_status_pending.contains(chat_id)
            || self.context_usage_pending.contains(chat_id)
            || self.subagent_pending.contains(chat_id)
        {
            return None;
        }
        let link = self.link_status.get(chat_id)?.fetched_at + LINK_STATUS_REFRESH;
        let context = *self.context_usage_fetched_at.get(chat_id)? + CONTEXT_USAGE_REFRESH;
        let mut due = link.min(context);
        if has_harness_session {
            let ttl = if self
                .subagents
                .get(chat_id)
                .is_some_and(|subs| subs.iter().any(|s| s.status == SubagentStatus::Running))
            {
                SUBAGENTS_RUNNING_REFRESH
            } else {
                SUBAGENTS_REFRESH
            };
            due = due.min(*self.subagents_fetched_at.get(chat_id)? + ttl);
        }
        Some(due)
    }

    pub(super) fn toggle_group(&mut self, dim: GroupDimension, cx: &Context<Self>) {
        if let Some(ix) = self.active_groups.iter().position(|&d| d == dim) {
            self.active_groups.remove(ix);
        } else {
            self.active_groups.push(dim);
        }
        // Cache key includes `active_groups`, so this alone would naturally
        // miss on the next read anyway — dropped explicitly for clarity.
        self.view_prefs_changed(cx);
    }

    /// Fetch/refresh one chat's context-window fraction on
    /// `CONTEXT_USAGE_REFRESH`. The attempt time is recorded on success and
    /// failure alike, so an engine without the
    /// method just retries on the TTL. A landed value that rounds to the same
    /// whole percent is a no-op: nothing visible changes, so rows stay
    /// cached (see [`context_pct_changed`]). A real change dirties them.
    pub(super) fn ensure_context_usage(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let fresh_enough = self
            .context_usage_fetched_at
            .get(&chat_id)
            .is_some_and(|at| at.elapsed() < CONTEXT_USAGE_REFRESH);
        if fresh_enough || self.context_usage_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.context_usage_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_CONTEXT_USAGE,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.context_usage_pending.remove(&chat_id);
                this.context_usage_fetched_at
                    .insert(chat_id.clone(), Instant::now());
                if let Ok(value) = result {
                    let pct = value
                        .get("contextPct")
                        .and_then(|v| v.as_f64())
                        .map(|f| f as f32);
                    let previous = this.context_usage.insert(chat_id, pct);
                    if context_pct_changed(previous, pct) {
                        this.rows_dirty = true;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}
