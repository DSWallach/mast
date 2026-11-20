use super::*;

impl Changes {
    pub(super) fn sync_owner(&mut self, cx: &App) -> bool {
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        let state = self.state.read(cx);
        let Some(chat) = state.chats.iter().find(|chat| chat.id == owner.chat_id) else {
            return false;
        };
        let next = ChangesTarget::for_chat_in(chat, state);
        if self.owner.as_ref() == Some(&next) {
            return false;
        }
        self.owner = Some(next);
        self.sync_scope_with_owner();
        self.diffs.clear();
        self.parsed = None;
        self.explanation_selection = None;
        self.explanation_unified_rows = None;
        self.cancel_explanation();
        self.rows.clear();
        self.row_ranges.clear();
        self.section_headers.clear();
        self.list.reset(0);
        self.branches_for = None;
        self.branches.clear();
        self.scoped = None;
        self.scoped_for = None;
        self.scoped_inflight = None;
        self.scoped_task = None;
        self.error = None;
        self.watch_task = None;
        self.started = false;
        self.watch_for = None;
        true
    }

    /// A pane is in [`DiffScope::Range`] exactly while its owner's selected
    /// pill is a branch / PR target. Leaving one restores the working tree
    /// scope; History and commit panes never change.
    pub(super) fn sync_scope_with_owner(&mut self) {
        let ranged = self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.range.is_some());
        if ranged {
            if matches!(
                self.scope,
                DiffScope::WorkingTree | DiffScope::Branch | DiffScope::LatestTurn
            ) {
                self.scope = DiffScope::Range;
            }
        } else if self.scope == DiffScope::Range {
            self.scope = DiffScope::WorkingTree;
        }
    }

    pub(super) fn owner_chat_id_owned(&self) -> Option<String> {
        self.owner.as_ref().map(|owner| owner.chat_id.clone())
    }

    pub(super) fn owner_branch(&self) -> Option<String> {
        self.owner.as_ref().and_then(|owner| owner.branch.clone())
    }

    /// The owning chat's host device when it differs from the connected
    /// engine's own — diffs are produced where the checkout lives, so a
    /// remote chat's watch must relay-forward (`targetDeviceId`) to its host.
    /// Without this the local stream simply never carries the remote checkout
    /// and the pane sits on "Preparing diff…" forever (user report).
    pub(super) fn desired_target(&self, cx: &App) -> Option<String> {
        if self.pinned_cwd.is_some() {
            return None;
        }
        let state = self.state.read(cx);
        let device = self.owner.as_ref()?.device_id.clone();
        if device.is_empty() {
            return None;
        }
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    /// Start the `WatchCheckoutDiffs` subscription (idempotent per target).
    /// Retries with a flat 2 s delay if the stream fails or ends; the last
    /// content stays visible under an error banner meanwhile.
    pub fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        if self.pinned_cwd.is_none()
            && self.owner.as_ref().is_none_or(|owner| {
                owner.device_id.is_empty()
                    || (owner.checkout_id.is_none() && owner.cwd.as_deref().is_none())
            })
        {
            return;
        }
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let cwd = self
            .pinned_cwd
            .clone()
            .or_else(|| self.owner.as_ref().and_then(|owner| owner.cwd.clone()));
        let watch_for = format!(
            "{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            checkout_id.as_deref().unwrap_or(""),
            cwd.as_deref().unwrap_or("")
        );
        if self.started && self.watch_for.as_deref() == Some(watch_for.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            // Engine still booting — retry on the next state change via sync().
            return;
        };
        // Retarget: the old task (and its stream) drop; rows from the previous
        // device would resolve against the wrong checkouts, so clear them.
        if self.started {
            self.diffs.clear();
            self.error = None;
        }
        self.started = true;
        self.watch_target = target.clone();
        self.watch_for = Some(watch_for);
        self.watch_task = Some(Self::spawn_watch(engine, target, checkout_id, cwd, cx));
    }

    pub(super) fn spawn_watch(
        engine: EngineHandle,
        target: Option<String>,
        checkout_id: Option<String>,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut backoff = mast_harness::backoff::Backoff::new(
                Duration::from_secs(2),
                Duration::from_secs(30),
            );
            loop {
                let params = watch_checkout_params(
                    target.as_deref(),
                    checkout_id.as_deref(),
                    cwd.as_deref(),
                );
                let subscribed = engine
                    .client()
                    .subscribe_checked(methods::WATCH_CHECKOUT_DIFFS, params)
                    .await;
                match subscribed {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            backoff.reset();
                            let alive = this.update(cx, |changes, cx| {
                                changes.error = None;
                                if apply_diff_frame(&mut changes.diffs, value) {
                                    changes.sync(cx);
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        // Stream ended (engine restart / reconnect): banner + retry.
                        if this
                            .update(cx, |changes, cx| {
                                changes.error = Some("Diff stream interrupted — retrying".into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        if this
                            .update(cx, |changes, cx| {
                                changes.error =
                                    Some(format!("Diff watch unavailable: {err}").into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(backoff.next_delay()).await;
            }
        })
    }

    pub(super) fn resolved(&self, _cx: &App) -> Option<&CheckoutDiff> {
        if self.pinned_cwd.is_some() {
            // A pinned watch is server-scoped by canonical cwd, so its frame
            // can contain at most the intended checkout. This snapshot drives
            // one-shot refreshes without consulting global chat selection.
            return self.diffs.first();
        }
        self.owner.as_ref()?.resolve(&self.diffs)
    }

    /// The checkout root the scoped RPCs address: the watch-resolved diff's
    /// canonical cwd when available, else the chat row's own.
    pub(super) fn scoped_cwd(&self, cx: &App) -> Option<String> {
        if let Some(cwd) = &self.pinned_cwd {
            return Some(cwd.clone());
        }
        if let Some(diff) = self.resolved(cx) {
            return Some(diff.cwd.clone());
        }
        self.owner.as_ref()?.cwd.clone()
    }

    /// The diff the pane currently displays: the watch stream for the working
    /// tree, the one-shot scoped capture otherwise.
    pub(super) fn active_diff(&self, cx: &App) -> Option<&CheckoutDiff> {
        match self.scope {
            DiffScope::WorkingTree if self.pinned_cwd.is_some() => self.scoped.as_ref(),
            DiffScope::WorkingTree => self.resolved(cx),
            DiffScope::Branch | DiffScope::LatestTurn | DiffScope::Commit | DiffScope::Range => {
                self.scoped.as_ref()
            }
            DiffScope::History => None,
        }
    }

    /// Scope discriminant folded into the parse key, so a scope or base
    /// switch re-parses even when checksums collide.
    pub(super) fn scope_key(&self) -> String {
        match self.scope {
            DiffScope::WorkingTree => "wt".to_string(),
            DiffScope::Branch => format!("br:{}", self.base_ref.as_deref().unwrap_or("")),
            DiffScope::LatestTurn => "turn".to_string(),
            DiffScope::Range => match self.owner.as_ref().and_then(|o| o.range.as_ref()) {
                Some(range) => format!("range:{}:{}", range.base_ref, range.head_ref),
                None => "range".to_string(),
            },
            DiffScope::History => "history".to_string(),
            DiffScope::Commit => format!(
                "commit:{}",
                self.commit.as_ref().map(|c| c.sha.as_str()).unwrap_or("")
            ),
        }
    }

    pub(super) fn parse_key(&self, diff: &CheckoutDiff) -> String {
        format!(
            "{}:{}:{}",
            diff.checkout_id,
            diff.checksum,
            self.scope_key()
        )
    }

    /// Fetch the branch list for the selected chat's checkout (idempotent per
    /// device+cwd); the repo's default branch (first entry) becomes the
    /// comparison base unless the user already picked one that still exists.
    pub(super) fn ensure_branches(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let key = format!(
            "{}:{}:{}",
            target.as_deref().unwrap_or("local"),
            checkout_id.as_deref().unwrap_or(""),
            cwd
        );
        if self.branches_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.branches_for = Some(key.clone());
        self.branches_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("repoPath".into(), serde_json::Value::String(cwd));
            if let Some(checkout_id) = checkout_id {
                params.insert("checkoutId".into(), serde_json::Value::String(checkout_id));
            }
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(methods::LIST_BRANCHES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |changes, cx| {
                if changes.branches_for.as_deref() != Some(key.as_str()) {
                    return; // superseded by a chat/device switch
                }
                match result {
                    Ok(value) => {
                        changes.branches =
                            serde_json::from_value::<Vec<String>>(value).unwrap_or_default();
                        let keep = changes
                            .base_ref
                            .as_ref()
                            .is_some_and(|base| changes.branches.contains(base));
                        if !keep {
                            let current = changes.owner_branch();
                            changes.base_ref =
                                default_base_ref(&changes.branches, current.as_deref());
                        }
                        changes.sync(cx);
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "changes: branch list failed");
                        // Allow a retry on the next state change.
                        changes.branches_for = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keep the one-shot scoped capture fresh. The fetch key folds in the
    /// watch checksum, so any working-tree change (or commit — HEAD rides the
    /// checksum) re-captures; a context change (chat/scope/base) clears the
    /// stale content first so the pane shows the spinner, while a
    /// checksum-only refresh keeps the old diff visible until the new one
    /// lands.
    pub(super) fn ensure_scoped(&mut self, cx: &mut Context<Self>) {
        if self.scope == DiffScope::History
            || (self.scope == DiffScope::WorkingTree && self.pinned_cwd.is_none())
        {
            self.scoped_inflight = None;
            self.scoped_task = None;
            return;
        }
        let chat_id = self.owner_chat_id_owned();
        if self.scope == DiffScope::LatestTurn && chat_id.is_none() {
            return;
        }
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let base = match self.scope {
            DiffScope::Branch => match &self.base_ref {
                Some(base) => Some(base.clone()),
                None => return, // branch list still loading
            },
            _ => None,
        };
        let commit_sha = match self.scope {
            DiffScope::Commit => match &self.commit {
                Some(commit) => Some(commit.sha.clone()),
                None => return, // a commit pane without its pin never fetches
            },
            _ => None,
        };
        // A range pane reads two refs of the repository checkout.
        let range = match self.scope {
            DiffScope::Range => match self.owner.as_ref().and_then(|o| o.range.clone()) {
                Some(range) => Some(range),
                None => return,
            },
            _ => None,
        };
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let context = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            chat_id.as_deref().unwrap_or(""),
            checkout_id.as_deref().unwrap_or(""),
            cwd,
            self.scope.mode(),
            base.as_deref().unwrap_or(""),
            commit_sha.as_deref().unwrap_or("")
        );
        let context = match &range {
            Some(range) => format!(
                "{context}|{}|{}|{}",
                range.base_ref,
                range.head_ref,
                range.head_sha.as_deref().unwrap_or("")
            ),
            None => context,
        };
        let since = self.rereview_range();
        // Refs only, or a range between two commits: the working tree is irrelevant.
        let watch_sum = if range.is_some() || since.is_some() {
            String::new()
        } else {
            self.resolved(cx).map(|d| d.checksum.clone()).unwrap_or_default()
        };
        let context = match &since {
            Some((old, new)) => format!("{context}|since|{old}|{new}"),
            None => context,
        };
        let key = format!("{context}|{watch_sum}");
        if self.scoped_for.as_deref() == Some(key.as_str())
            || self.scoped_inflight.as_deref() == Some(key.as_str())
        {
            return;
        }
        if self
            .scoped_for
            .as_deref()
            .is_none_or(|prev| {
                !prev.starts_with(&format!("{context}|")) || prev.contains("|since|") != since.is_some()
            })
        {
            // Also when entering or leaving the Since view: the old diff is the other kind.
            self.scoped = None;
            self.scoped_error = None;
            self.scoped_is_since = false;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mode = self.scope.mode();
        let was_since = since.is_some();
        let request_head = self.review_identity(cx).and_then(|(_, head)| head);
        self.scoped_inflight = Some(key.clone());
        self.scoped_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("cwd".into(), serde_json::Value::String(cwd));
            if let Some(checkout_id) = checkout_id {
                params.insert("checkoutId".into(), serde_json::Value::String(checkout_id));
            }
            params.insert("mode".into(), serde_json::Value::String(mode.to_string()));
            if let Some(chat_id) = chat_id {
                params.insert("chatId".into(), serde_json::Value::String(chat_id));
            }
            if let Some(base) = base {
                params.insert("baseRef".into(), serde_json::Value::String(base));
            }
            if let Some(sha) = commit_sha {
                params.insert("commitSha".into(), serde_json::Value::String(sha));
            }
            if let Some(range) = range {
                params.insert("baseRef".into(), serde_json::Value::String(range.base_ref));
                params.insert("headRef".into(), serde_json::Value::String(range.head_ref));
            }
            if let Some((old, new)) = since {
                // Re-review: the engine diffs old..new (interdiff after a force-push).
                params.insert("mode".into(), serde_json::Value::String("since".into()));
                params.insert("commitSha".into(), serde_json::Value::String(old));
                params.insert("headRef".into(), serde_json::Value::String(new));
            }
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(
                    methods::GET_CHECKOUT_DIFF,
                    serde_json::Value::Object(params),
                )
                .await;
            this.update(cx, |changes, cx| {
                if changes.scoped_inflight.as_deref() != Some(key.as_str()) {
                    return; // superseded
                }
                changes.scoped_inflight = None;
                match result.and_then(|value| {
                    serde_json::from_value::<CheckoutDiff>(value)
                        .map_err(|e| mast_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(diff) => {
                        changes.scoped = Some(diff);
                        changes.scoped_error = None;
                        changes.scoped_is_since = was_since;
                        changes.scoped_head = request_head;
                    }
                    Err(err) if err.to_string().contains("old-head-unavailable") => {
                        // The reviewed head was garbage-collected: say so and show the full diff.
                        changes.note_rereview_unavailable();
                        changes.scoped = None;
                        changes.scoped_error = None;
                        changes.scoped_is_since = false;
                    }
                    Err(err) => {
                        changes.scoped = None;
                        changes.scoped_error = Some(err.to_string().into());
                        changes.scoped_is_since = false;
                    }
                }
                changes.scoped_for = Some(key);
                changes.sync(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn reset_horizontal_scroll(&self) {
        if let Some(parsed) = &self.parsed {
            for file in &parsed.horizontal {
                reset_horizontal_scroll(&file.scroll);
            }
        }
    }

    pub(crate) fn set_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.explanation_selection = None;
            self.explanation_unified_rows = None;
            self.cancel_explanation();
            self.reset_horizontal_scroll();
            if scope == DiffScope::History {
                self.history_pane(cx)
                    .update(cx, |history, cx| history.ensure_loaded(cx));
            }
            self.sync(cx);
        }
        cx.notify();
    }

    pub(super) fn history_pane(&mut self, cx: &mut Context<Self>) -> Entity<GitHistory> {
        if let Some(history) = &self.history {
            return history.clone();
        }
        let state = self.state.clone();
        let owner_chat_id = self.owner_chat_id_owned();
        let history = cx.new(|cx| match owner_chat_id {
            Some(chat_id) => GitHistory::for_chat(state, chat_id, cx),
            None => GitHistory::new(state, cx),
        });
        self.history_events =
            Some(
                cx.subscribe(&history, |this: &mut Self, _, event, cx| match event {
                    GitHistoryEvent::OpenCommit(commit) => {
                        // Bubble to the host — the surface strip opens the tab.
                        cx.emit(ChangesEvent::OpenCommit(commit.clone()));
                    }
                    GitHistoryEvent::FetchSucceeded => {
                        // Remote refs affect branch choices and every scoped diff
                        // based on a ref. Force fresh reads after the engine has
                        // also kicked its checkout-status watcher.
                        this.branches_for = None;
                        this.scoped_for = None;
                        this.scoped_inflight = None;
                        this.scoped_task = None;
                        this.ensure_branches(cx);
                        if this.scope != DiffScope::History {
                            this.ensure_scoped(cx);
                        }
                        cx.notify();
                    }
                }),
            );
        self.history = Some(history.clone());
        history
    }

    pub(super) fn history_count(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryCount> {
        if let Some(count) = &self.history_count {
            return count.clone();
        }
        let history = self.history_pane(cx);
        let count = cx.new(|cx| GitHistoryCount::new(history, cx));
        self.history_count = Some(count.clone());
        count
    }

    pub(super) fn history_fetch_button(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryFetchButton> {
        if let Some(button) = &self.history_fetch_button {
            return button.clone();
        }
        let history = self.history_pane(cx);
        let button = cx.new(|cx| GitHistoryFetchButton::new(history, cx));
        self.history_fetch_button = Some(button.clone());
        button
    }

    pub(super) fn history_search_control(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<GitHistorySearchControl> {
        if let Some(control) = &self.history_search_control {
            return control.clone();
        }
        let history = self.history_pane(cx);
        let control = cx.new(|cx| GitHistorySearchControl::new(history, cx));
        self.history_search_control = Some(control.clone());
        control
    }

    pub(super) fn history_view_button(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryViewButton> {
        if let Some(button) = &self.history_view_button {
            return button.clone();
        }
        let history = self.history_pane(cx);
        let button = cx.new(|cx| GitHistoryViewButton::new(history, cx));
        self.history_view_button = Some(button.clone());
        button
    }

    pub(super) fn set_base_ref(&mut self, base: String, cx: &mut Context<Self>) {
        if self.base_ref.as_deref() != Some(base.as_str()) {
            self.base_ref = Some(base);
            self.sync(cx);
        }
        cx.notify();
    }

    /// Everything the pane needs kicked when (re)shown: the watch plus the
    /// scope-specific loads (branches, scoped/commit capture, history) — the
    /// shell's hook for freshly-mounted surface tabs.
    pub fn ensure_content(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
    }

}
