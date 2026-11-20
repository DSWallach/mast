use super::*;

impl Overview {

    pub(super) fn ensure_link_status(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let checkout = self.state.read(cx).selected_checkout_key(&chat_id);
        let fresh_enough = self.link_checkouts.get(&chat_id).cloned().unwrap_or(None) == checkout
            && self
                .link_status
                .get(&chat_id)
                .is_some_and(|e| e.fetched_at.elapsed() < LINK_STATUS_REFRESH);
        if fresh_enough || self.link_status_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.link_status_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_LINK_STATUS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.link_status_pending.remove(&chat_id);
                if this.state.read(cx).selected_checkout_key(&chat_id) != checkout {
                    cx.notify();
                    return;
                }
                if let Ok(value) = result
                    && let Ok(status) = serde_json::from_value::<OverviewLinkStatus>(value.clone())
                {
                    let links: Vec<_> = status
                        .pr_links
                        .iter()
                        .filter_map(|link| Some((link.url.clone(), link.source?)))
                        .collect();
                    crate::review_chats::note_chat_links(cx, &chat_id, &links);
                    let diff_stat = value
                        .get("diffStat")
                        .and_then(|v| serde_json::from_value::<DiffStat>(v.clone()).ok());
                    let is_worktree = value.get("isWorktree").and_then(|v| v.as_bool());
                    // `prSource`/`ticketSource` ride the `ChatLinkStatus`
                    // decode itself (both `Option`, so an older engine that
                    // omits them reads as "inferred"). Merged over the
                    // previous entry so a just-set manual link's placeholder
                    // survives the engine's detail-cache miss.
                    let status = merge_refetched_link(
                        this.link_status.get(&chat_id).map(|e| &e.status),
                        status,
                    );
                    let fingerprint = link_fingerprint(&status);
                    this.link_checkouts.insert(chat_id.clone(), checkout);
                    let previous = this
                        .link_status
                        .insert(
                            chat_id,
                            LinkStatusEntry {
                                status,
                                diff_stat,
                                is_worktree,
                                fetched_at: Instant::now(),
                            },
                        )
                        .map(|old| link_fingerprint(&old.status));
                    // `rows()`'s cache bakes ticket/PR number/PR title in
                    // (grouping key + search fields) — only a CHANGE to
                    // those invalidates rows; a
                    // same-data TTL refetch just repaints (badges/detail
                    // read `link_status` live).
                    if previous.as_ref() != Some(&fingerprint) {
                        this.rows_dirty = true;
                    }
                }
                // Fetch failure: the stale entry (if any) keeps rendering;
                // its expired `fetched_at` retries on the next tick.
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open the inline "Add PR / ticket…" editor in `chat_id`'s expanded
    /// detail (expanding it if needed), focused and empty. Also the entry
    /// point for the sidebar chat menu's "Add PR / ticket…", which switches
    /// to the overview first; `reveal` then brings the row into view
    /// the same way a PR-sidebar jump does.
    pub(crate) fn open_link_editor(
        &mut self,
        chat_id: String,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.expanded.insert(chat_id.clone()) {
            self.acknowledge_done(&chat_id, cx);
        }
        self.link_editor_chat = Some(chat_id.clone());
        self.link_submitting = false;
        self.link_error = None;
        self.link_input
            .update(cx, |input, cx| input.set_text("", cx));
        window.focus(
            &gpui::Focusable::focus_handle(self.link_input.read(cx), cx),
            cx,
        );
        if reveal {
            self.focus_due = Some(chat_id);
        }
        cx.notify();
    }

    pub(super) fn close_link_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.link_editor_chat.take() {
            if self
                .link_error
                .as_ref()
                .is_some_and(|(id, _)| *id == chat_id)
            {
                self.link_error = None;
            }
            self.link_submitting = false;
            cx.notify();
        }
    }

    /// Enter in the link editor: parse ([`parse_link_input`]) and, if it's a
    /// PR URL or ticket id, write it via `SET_CHAT_LINK`; otherwise show the
    /// parse error inline and keep the editor open.
    pub(super) fn submit_link_editor(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.link_editor_chat.clone() else {
            return;
        };
        if self.link_submitting {
            return;
        }
        match parse_link_input(self.link_input.read(cx).text()) {
            Err(message) => {
                self.link_error = Some((chat_id, message.to_string()));
                cx.notify();
            }
            Ok(parsed) => {
                self.link_submitting = true;
                self.link_error = None;
                self.set_chat_link(
                    chat_id,
                    parsed.kind,
                    Some(parsed.value),
                    LinkOperation::Add,
                    cx,
                );
            }
        }
    }

    /// `SET_CHAT_LINK` (always a manual write; `value: None` unlinks). On
    /// success: optimistic local update ([`apply_link_reply`]), rows
    /// invalidated (ticket/PR feed grouping, search and the badge row), the
    /// entry's TTL expired and an immediate `CHAT_LINK_STATUS` refetch
    /// kicked (which [`merge_refetched_link`] keeps from blanking the
    /// placeholder). Closes the editor if it was this chat's. On failure:
    /// inline error in the chat's detail.
    pub(super) fn set_chat_link(
        &mut self,
        chat_id: String,
        kind: LinkKind,
        value: Option<String>,
        operation: LinkOperation,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.link_submitting = false;
            self.link_error = Some((chat_id, "Engine not connected".to_string()));
            cx.notify();
            return;
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_CHAT_LINK,
                    serde_json::json!({
                        "chatId": chat_id,
                        "kind": kind.wire(),
                        "value": value,
                        "operation": operation.wire(),
                    }),
                )
                .await;
            this.update(cx, |this, cx| {
                let editor_was_this_chat =
                    this.link_editor_chat.as_deref() == Some(chat_id.as_str());
                if editor_was_this_chat {
                    this.link_submitting = false;
                }
                let reply = result.map_err(|err| err.to_string()).and_then(|value| {
                    serde_json::from_value::<SetChatLinkReply>(value).map_err(|err| err.to_string())
                });
                match reply {
                    Ok(reply) => {
                        let _ = this.shell.update(cx, |shell, _| {
                            shell.invalidate_chat_header_links(&chat_id);
                        });
                        let expired = Instant::now()
                            .checked_sub(LINK_STATUS_REFRESH)
                            .unwrap_or_else(Instant::now);
                        let entry = this.link_status.entry(chat_id.clone()).or_insert_with(|| {
                            LinkStatusEntry {
                                status: OverviewLinkStatus::default(),
                                diff_stat: None,
                                is_worktree: None,
                                fetched_at: expired,
                            }
                        });
                        apply_link_reply(&mut entry.status, kind, &reply);
                        entry.fetched_at = expired;
                        this.row_fetch_due.remove(&chat_id);
                        this.rows_dirty = true;
                        if this
                            .link_error
                            .as_ref()
                            .is_some_and(|(id, _)| *id == chat_id)
                        {
                            this.link_error = None;
                        }
                        if editor_was_this_chat && operation == LinkOperation::Add {
                            if kind == LinkKind::Pr {
                                this.link_input
                                    .update(cx, |input, cx| input.set_text("", cx));
                            } else {
                                this.link_editor_chat = None;
                            }
                        }
                        this.ensure_link_status(chat_id, cx);
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err, "SetChatLink failed");
                        this.link_error = Some((chat_id, format!("Couldn't save link: {err}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
