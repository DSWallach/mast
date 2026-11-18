use super::*;

impl Composer {
    pub(super) fn update_slash(
        &mut self,
        token: Option<MentionToken>,
        still_dismissed: bool,
        cx: &mut Context<Self>,
    ) {
        if token.is_none() && self.slash.token.is_none() {
            return;
        }
        if still_dismissed {
            self.slash.token = None;
            self.sync_mention_controls(cx);
            return;
        }
        self.slash.dismissed = None;
        let harness = self.pickers.read(cx).resolved(cx).harness;
        let reuse_worktree = match self.pickers.read(cx).checkout_plan() {
            crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } => Some(path),
            _ => None,
        };
        let cwd = {
            let state = self.state.read(cx);
            let chat = state.selected_chat_row();
            let chat_space = chat
                .and_then(|chat| chat.space_id.as_deref())
                .and_then(|id| state.space_row(id));
            slash_discovery_cwd(chat, chat_space, state.selected_space_row(), reuse_worktree)
        };
        let key_changed = self.slash.harness != harness || self.slash.cwd != cwd;
        if token == self.slash.token && !key_changed {
            return;
        }
        self.slash.token = token.clone();
        self.slash.harness = harness;
        self.slash.cwd = cwd.clone();
        self.slash.error = None;
        if token.is_none() {
            self.slash.active = None;
            self.sync_mention_controls(cx);
            return;
        }
        // No resolved harness (catalog still loading): empty popup, no fetch.
        let Some(harness) = harness else {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        };
        let key: SlashCacheKey = (harness, cwd.clone());
        let now = Instant::now();
        let fresh = self
            .slash_cache
            .get(&key)
            .is_some_and(|entry| slash_cache_entry_is_fresh(&key, entry.fetched_at, &key, now));
        if fresh {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        }
        // First open for this (harness, cwd) — or a stale/evicted entry: one
        // ListCommands, targeted like file search (the chat/space host
        // device owns the agent binary). A chat switch that lands back on an
        // already-cached (harness, cwd) within the TTL never re-fetches.
        self.slash.request = self.slash.request.wrapping_add(1);
        self.slash.loading = true;
        self.refilter_slash(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.slash.loading = false;
            return;
        };
        let (target, chat_id) = {
            let state = self.state.read(cx);
            match state.selected_chat_row() {
                Some(chat) => (Some(chat.device_id.clone()), Some(chat.id.clone())),
                None => (
                    state
                        .selected_space_row()
                        .map(|space| space.device_id.clone()),
                    None,
                ),
            }
        };
        let request = self.slash.request;
        let params = list_commands_params(harness, &key.1, target.as_deref(), chat_id.as_deref());
        self.slash_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_COMMANDS, params).await;
            this.update(cx, |composer, cx| {
                if composer.slash.request != request {
                    return;
                }
                composer.slash.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => {
                            composer.slash_cache.insert(
                                key,
                                SlashCacheEntry {
                                    commands,
                                    fetched_at: Instant::now(),
                                },
                            );
                        }
                        Err(err) => tracing::warn!(%err, "slash command decode failed"),
                    },
                    Err(err) => {
                        tracing::debug!(%err, "slash command discovery failed");
                        composer.slash.error = Some(slash_error_message(&err));
                    }
                }
                composer.refilter_slash(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// The cache key for the popup's currently-tracked (harness, cwd), if a
    /// harness is resolved.
    pub(super) fn slash_key(&self) -> Option<SlashCacheKey> {
        self.slash
            .harness
            .map(|harness| (harness, self.slash.cwd.clone()))
    }

    /// Re-rank the cached list for the current query (pure local filter).
    pub(super) fn refilter_slash(&mut self, cx: &mut Context<Self>) {
        let query = self
            .slash
            .token
            .as_ref()
            .map(|t| t.query.clone())
            .unwrap_or_default();
        let commands = self
            .slash_key()
            .and_then(|key| self.slash_cache.get(&key))
            .map(|entry| entry.commands.as_slice())
            .unwrap_or_default();
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        self.slash.filtered = crate::popover::filter_indices(&query, &names);
        self.slash.active = (!self.slash.filtered.is_empty()).then_some(0);
        // A fresh query/reopen restarts the row stack at the top.
        crate::popover::reset_menu_scroll(&self.slash_scroll, &mut self.popup_bar);
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.slash.active =
            crate::popover::menu_step(self.slash.active, self.slash.filtered.len(), delta);
        if let Some(active) = self.slash.active {
            // Keep the keyboard cursor visible in the scrolled row stack.
            self.slash_scroll.scroll_to_item(active);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn dismiss_slash(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.slash.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_slash(dismissed, cx);
        cx.notify();
    }

    /// The command under the popup's keyboard cursor.
    pub(super) fn active_slash_command(&self) -> Option<SlashCommand> {
        self.slash
            .active
            .and_then(|active| self.slash.filtered.get(active))
            .and_then(|&ix| {
                self.slash_key()
                    .and_then(|key| self.slash_cache.get(&key))
                    .and_then(|entry| entry.commands.get(ix))
            })
            .cloned()
    }

    /// Enter/Tab/click: replace the typed token with `/name `. At offset 0
    /// that is the whole-prompt command the CLI expands natively (unchanged
    /// behavior); anywhere else the same text is an inline skill mention the
    /// model reads — no private URI scheme, it is just text.
    pub(super) fn accept_slash(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(command) = self.active_slash_command() else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &format!("/{}", command.name), cx)
        });
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Cmd/Ctrl+Enter or the row's ＋: attach the command as a skill chip
    /// and delete the typed `/query` token. While a queued row is being
    /// edited there is no send to carry a preamble, so it inserts instead.
    pub(super) fn attach_slash(&mut self, cx: &mut Context<Self>) {
        if self.editing_queued.is_some() {
            self.accept_slash(cx);
            return;
        }
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(command) = self.active_slash_command() else {
            return;
        };
        let key = self.current_key.clone();
        attach_skill(self.skills.entry(key).or_default(), &command.name);
        let removal = {
            let text = self.input.read(cx).text();
            (token.range.end <= text.len()).then(|| attach_removal_range(text, token.range))
        };
        if let Some(removal) = removal {
            self.input
                .update(cx, |input, cx| input.remove_plain_range(removal, cx));
        }
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Tear down the slash completion (mirrors [`Self::reset_mention`]).
    pub(super) fn reset_slash(&mut self, dismissed: Option<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let request = self.slash.request.wrapping_add(1);
        self.slash_task = None;
        self.slash = SlashState {
            request,
            dismissed,
            harness: self.slash.harness,
            cwd: self.slash.cwd.clone(),
            ..SlashState::default()
        };
        self.sync_mention_controls(cx);
    }

    pub(super) fn render_slash_popup(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &theme.for_popup();
        // Only while a slash token is active.
        self.slash.token.as_ref()?;
        let commands = self
            .slash_key()
            .and_then(|key| self.slash_cache.get(&key))
            .map(|entry| entry.commands.as_slice())
            .unwrap_or_default();
        // Full pill width at the mention card's height budget — both composer
        // completions share the same surface shape.
        let mut card = crate::components::conversation_menu_card(theme, false)
            .w_full()
            .max_h(px(320.0))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_slash(cx)));
        if self.slash.loading && commands.is_empty() {
            card = card.child(crate::popover::skeleton_rows(
                "slash-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(error) = self.slash.error.clone() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.danger_muted)
                    .child(error),
            );
        } else if self.slash.filtered.is_empty() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(if commands.is_empty() {
                        "This agent has no slash commands"
                    } else {
                        "No matching commands"
                    }),
            );
        } else {
            // A queued-row edit has no send to carry a preamble (attach falls
            // back to insert there), so the affordance is hidden.
            let can_attach = self.editing_queued.is_none();
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.slash.filtered.len());
            for (row_ix, &cmd_ix) in self.slash.filtered.iter().enumerate() {
                let Some(command) = commands.get(cmd_ix) else {
                    continue;
                };
                let selected = self.slash.active == Some(row_ix);
                let row_group: SharedString = format!("slash-row-{row_ix}").into();
                rows.push(
                    crate::components::conversation_menu_row(theme, selected, format!("slash-result-{row_ix}"))
                        .group(row_group.clone())
                        .id(("slash-result", row_ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.slash.active = Some(row_ix);
                            this.accept_slash(cx);
                        }))
                        .child(slash_row_body(theme, command).when(can_attach, |row| {
                            row.child(Self::render_slash_attach_button(
                                theme, row_ix, selected, row_group, cx,
                            ))
                        }))
                        .into_any_element(),
                );
            }
            // Overflowing rows wheel-scroll inside a bounded viewport; the
            // floating rail mirrors the model-list scrollbar treatment.
            card = card.child(
                div()
                    .id("slash-scroll-host")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("slash-list")
                            .max_h(px(if can_attach {
                                312.0 - SLASH_FOOTER_HEIGHT
                            } else {
                                312.0
                            }))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.slash_scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(self, "slash-scrollbar", theme, cx)),
            );
            if can_attach {
                card = card.child(Self::render_slash_footer(theme));
            }
        }
        // Full pill width above the composer, matching the file-mention popup.
        Some(crate::popover::full_width_menu_above(
            "slash-popup",
            card.into_any_element(),
            None,
        ))
    }

    /// The row's secondary action: attach this command as a skill chip
    /// instead of inserting it. Visible on the keyboard-selected row and on
    /// hover; the click never reaches the row's own insert handler.
    pub(super) fn render_slash_attach_button(
        theme: &Theme,
        row_ix: usize,
        selected: bool,
        row_group: SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(("slash-attach", row_ix))
            .role(gpui::Role::Button)
            .aria_label("Attach skill to chat")
            .flex_none()
            .size(px(20.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(true, |el| crate::components::conversation_reveal(el, theme, row_group, selected))
            .hover(|s| s.bg(crate::theme::ink(0.08)))
            .tooltip(|_, cx| {
                cx.new(|_| {
                    crate::workspace_chip::WorkspaceChipTooltip(
                        format!("Attach to chat ({})", slash_attach_key_label()).into(),
                    )
                })
                .into()
            })
            .tooltip_show_delay(Duration::from_millis(350))
            // Mouse-down bubbles to the card, which keeps the input focused
            // (an unfocused input tears the popup down on the next render).
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.slash.active = Some(row_ix);
                this.attach_slash(cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }))
            .child(
                crate::icons::icon(crate::icons::PLUS)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .into_any_element()
    }

    /// One-line legend under the slash rows: Enter inserts, the modified
    /// Enter attaches.
    pub(super) fn render_slash_footer(theme: &Theme) -> gpui::Div {
        div()
            .h(px(SLASH_FOOTER_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .child(crate::popover::key_hint_text(theme, "↵", "Insert"))
            .child(crate::popover::key_hint_text(
                theme,
                slash_attach_key_label(),
                "Attach to chat",
            ))
    }

    /// The popup whose rows a scrollbar drag is moving — the tokens are
    /// mutually exclusive, so at most one exists.
    pub(super) fn active_popup_scroll(&self) -> Option<gpui::ScrollHandle> {
        if self.slash.token.is_some() {
            Some(self.slash_scroll.clone())
        } else if self.mention.token.is_some() {
            Some(self.mention_scroll.clone())
        } else {
            None
        }
    }

    pub(super) fn on_popup_list_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup_bar.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Reconcile composer-owned state with `AppState` (draft swap on
    /// navigation, question panel, queue-edit and interrupt bookkeeping), and
    /// repaint only when that changed something visible: the slice of state
    /// the composer renders differs, or the reconcile itself moved composer
    /// state. Streaming frames and other chats' updates do neither.
}
