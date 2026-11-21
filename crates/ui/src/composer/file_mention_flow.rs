use super::*;

impl Composer {
    pub(super) fn sync_mention_controls(&mut self, cx: &mut Context<Self>) {
        let open = self.mention.token.is_some() || self.slash.token.is_some();
        let has_selection = if self.slash.token.is_some() {
            self.slash.active.is_some()
        } else {
            self.mention.active.is_some()
        };
        self.input.update(cx, |input, cx| {
            input.set_mention_controls(open, has_selection, cx)
        });
    }

    /// Tear down the entire completion lifecycle. Advancing the generation is
    /// important even when the spawned task is dropped: an RPC response may
    /// already be queued for delivery on the UI executor.
    pub(super) fn reset_mention(
        &mut self,
        dismissed: Option<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        let request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        self.mention = FileMentionState {
            request,
            dismissed,
            ..FileMentionState::default()
        };
        self.sync_mention_controls(cx);
    }

    /// Tell the engine the user is acting in this composer's chat. Fire and
    /// forget; throttled by the reporter unless the draft flag flipped.
    pub(crate) fn report_user_activity(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.next_activity_ping(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Ok(value) = serde_json::to_value(&params) else {
            return;
        };
        cx.spawn(async move |_, _| {
            let _ = engine
                .client()
                .call(methods::NOTE_CHAT_USER_ACTIVITY, value)
                .await;
        })
        .detach();
    }

    /// The ping to send now, if the throttle allows one.
    pub(super) fn next_activity_ping(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<mast_proto::NoteChatUserActivityParams> {
        let draft = !self.input.read(cx).text().is_empty();
        self.activity
            .note(&self.current_key, Some(draft), Instant::now())
    }

    pub(super) fn on_input_edited(&mut self, cx: &mut Context<Self>) {
        self.report_user_activity(cx);
        if self.wizard.is_some() {
            if self.mention.token.is_some() || self.mention_task.is_some() {
                self.reset_mention(None, cx);
            }
            if self.slash.token.is_some() || self.slash_task.is_some() {
                self.reset_slash(None, cx);
            }
            cx.notify();
            return;
        }
        let (slash, slash_dismissed, mention, mention_dismissed) = {
            let input = self.input.read(cx);
            let text = input.text();
            let cursor = input.cursor_offset();
            let slash = slash_token(text, cursor);
            let mention = mention_token(text, cursor);
            let slash_dismissed = slash.as_ref().is_some_and(|token| {
                self.slash.dismissed.as_ref().is_some_and(|(range, value)| {
                    token.range == *range && text.get(range.clone()) == Some(value.as_str())
                })
            });
            let mention_dismissed = mention.as_ref().is_some_and(|token| {
                self.mention
                    .dismissed
                    .as_ref()
                    .is_some_and(|(range, value)| {
                        token.range == *range && text.get(range.clone()) == Some(value.as_str())
                    })
            });
            (slash, slash_dismissed, mention, mention_dismissed)
        };
        self.update_slash(slash, slash_dismissed, cx);
        // A live slash token owns the completion keys; never open the file
        // popup on top of it (defensive — the shapes are already disjoint).
        let token = if self.slash.token.is_some() {
            None
        } else {
            mention
        };
        let still_dismissed = token.is_some() && mention_dismissed;
        if still_dismissed {
            self.mention.token = None;
            self.mention_task = None;
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.dismissed = None;
        if token == self.mention.token {
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        // Refining an open menu keeps the stale rows visible until the new
        // response lands — clearing here made the popup bounce through the
        // skeleton (and a different height) on every keystroke.
        let refining = self.mention.token.is_some() && token.is_some();
        self.mention.token = token.clone();
        if !refining {
            self.mention.results.clear();
            self.mention.active = None;
            // Fresh open: the row stack restarts at the top.
            crate::popover::reset_menu_scroll(&self.mention_scroll, &mut self.popup_bar);
        }
        self.mention.error = None;
        self.mention.loading = token.is_some();
        self.sync_mention_controls(cx);
        let Some(token) = token else {
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.mention.loading = false;
            cx.notify();
            return;
        };
        let selected_worktree = match self.pickers.read(cx).checkout_plan() {
            crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } => Some(path),
            _ => None,
        };
        let (params, target) = {
            let state = self.state.read(cx);
            let mut params = serde_json::Map::new();
            params.insert("query".into(), token.query.clone().into());
            let target = if let Some(chat) = state.selected_chat_row() {
                params.insert("chatId".into(), chat.id.clone().into());
                Some(chat.device_id.clone())
            } else if let Some(space) = state.selected_space_row() {
                params.insert("spaceId".into(), space.id.clone().into());
                if let Some(path) = selected_worktree {
                    params.insert("path".into(), path.into());
                }
                Some(space.device_id.clone())
            } else {
                None
            };
            if let Some(target) = &target {
                params.insert("targetDeviceId".into(), target.clone().into());
            }
            (serde_json::Value::Object(params), target)
        };
        if target.is_none() {
            self.mention.loading = false;
            cx.notify();
            return;
        }
        let request = self.mention.request;
        self.mention_task = Some(cx.spawn(async move |this, cx| {
            // A short debounce prevents one full workspace walk per keystroke
            // during normal typing. The generation check below still guards
            // requests that were already in flight when the query changed.
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let mut result = engine
                .client()
                .call(methods::SEARCH_FILES, params.clone())
                .await;
            if matches!(result, Err(RpcError::Transport(_)) | Err(RpcError::Closed)) {
                // One retry rides out a cold relay dial to the host device
                // (the diffs pane retries forever; a keystroke-scoped search
                // gets a single second chance).
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                result = engine.client().call(methods::SEARCH_FILES, params).await;
            }
            this.update(cx, |composer, cx| {
                if !mention_response_is_current(&composer.mention, request) {
                    return;
                }
                composer.mention.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<FileSearchMatch>>(value) {
                        Ok(results) => {
                            composer.mention.error = None;
                            composer.mention.active = (!results.is_empty()).then_some(0);
                            composer.mention.results = results;
                            // New result set: the row stack restarts at the top.
                            crate::popover::reset_menu_scroll(
                                &composer.mention_scroll,
                                &mut composer.popup_bar,
                            );
                        }
                        Err(err) => tracing::warn!(%err, "file mention response decode failed"),
                    },
                    Err(err) => {
                        tracing::warn!(%err, "file mention search failed");
                        composer.mention.results.clear();
                        composer.mention.active = None;
                        composer.mention.error = Some(mention_error_message(&err));
                    }
                }
                composer.sync_mention_controls(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn move_mention(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.mention.active =
            crate::popover::menu_step(self.mention.active, self.mention.results.len(), delta);
        if let Some(active) = self.mention.active {
            // Keep the keyboard cursor visible in the scrolled row stack.
            self.mention_scroll.scroll_to_item(active);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn dismiss_mention(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.mention.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_mention(dismissed, cx);
        cx.notify();
    }

    pub(super) fn accept_mention(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.mention.token.clone() else {
            return;
        };
        let Some((path, is_dir)) = self
            .mention
            .active
            .and_then(|active| self.mention.results.get(active))
            .map(|result| (result.path.clone(), result.is_dir))
        else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.replace_mention(token.range, &path, is_dir, cx)
        });
        if !is_dir {
            self.stage_workspace_file(&path, cx);
        }
        self.reset_mention(None, cx);
        cx.notify();
    }

    pub(super) fn render_file_mention_popup(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &theme.for_popup();
        let token = self.mention.token.as_ref()?;
        let mut card = crate::components::conversation_menu_card(theme, false)
            .w_full()
            .max_h(px(320.0))
            .overflow_hidden()
            // Completion choices belong to the input. Keep it focused until
            // mouse-up can accept a choice (or while dragging the scrollbar).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_mention(cx)));
        if self.mention.loading && self.mention.results.is_empty() {
            card = card.child(crate::popover::skeleton_rows(
                "file-mention-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(error) = self.mention.error.clone() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.danger_muted)
                    .child(error),
            );
        } else if self.mention.results.is_empty() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(if token.query.is_empty() {
                        "No files available"
                    } else {
                        "No matching files"
                    }),
            );
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.mention.results.len());
            for (ix, result) in self.mention.results.iter().enumerate() {
                let selected = self.mention.active == Some(ix);
                let (directory, name) = match result.path.rsplit_once('/') {
                    Some((directory, name)) => (directory.to_string(), name.to_string()),
                    None => (String::new(), result.path.clone()),
                };
                rows.push(
                    crate::components::conversation_menu_row(
                        theme,
                        selected,
                        format!("file-mention-result-{ix}"),
                    )
                    .id(("file-mention-result", ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mention.active = Some(ix);
                        this.accept_mention(cx);
                    }))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                crate::file_icons::icon(
                                    if result.is_dir {
                                        crate::file_icons::FileIconIdentity::directory(
                                            &result.path,
                                            false,
                                        )
                                    } else {
                                        crate::file_icons::FileIconIdentity::file(&result.path)
                                    },
                                    theme.appearance,
                                )
                                .size(px(14.0))
                                .flex_none(),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(13.0))
                                    .text_color(theme.text)
                                    .child(name),
                            )
                            .when(!directory.is_empty(), |row| {
                                row.child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(theme.text_muted)
                                        .child(directory),
                                )
                            }),
                    )
                    .into_any_element(),
                );
            }
            // Overflowing rows wheel-scroll inside a bounded viewport; the
            // floating rail mirrors the model-list scrollbar treatment.
            card = card.child(
                div()
                    .id("mention-scroll-host")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("mention-list")
                            .max_h(px(312.0))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.mention_scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(self, "mention-scrollbar", theme, cx)),
            );
        }
        Some(crate::popover::full_width_menu_above(
            "file-mention-popup",
            card.into_any_element(),
            None,
        ))
    }

    pub(super) fn render_input_with_completion(&self) -> gpui::Div {
        div().relative().child(self.input.clone())
    }

    // ---- slash commands ---------------------------------------------------
}
