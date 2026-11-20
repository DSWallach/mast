use super::*;

impl Overview {

    pub(super) fn open(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.acknowledge_done(&chat_id, cx);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_chat(chat_id, cx);
        });
    }

    /// Open the interactive chat panel for `chat_id` alongside the overview,
    /// instead of navigating away — see `chat_panel_open`'s doc comment.
    /// Selecting the chat is what wires the Shell's transcript/composer to
    /// it; re-clicking the already-shown chat just refocuses the composer.
    pub(crate) fn open_chat_panel(&mut self, chat_id: String, cx: &mut Context<Self>) {
        if self.in_sidebar {
            self.subagent_peek = None;
            self.chat_panel_open = false;
            self.outline_chat_return = Some(OutlineChatReturn {
                chat_id: chat_id.clone(),
                full_route: true,
            });
            self.open(chat_id, cx);
            return;
        }
        // Web `openTranscriptPanel` → `acknowledgeDone`.
        self.acknowledge_done(&chat_id, cx);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.preview_chat_in_overview(chat_id, cx);
        });
        // The live panel replaces a subagent peek (one right-side panel).
        self.subagent_peek = None;
        self.chat_panel_open = true;
        cx.notify();
    }
    /// Hide the panel. The chat stays selected (like any sidebar pick) —
    /// deselecting would drop the user back onto the blank new-session
    /// canvas the next time they visit the chat route.
    pub(super) fn close_chat_panel(&mut self, cx: &mut Context<Self>) {
        if self.chat_panel_open {
            self.chat_panel_open = false;
            cx.notify();
        }
    }

    /// Copy the same debugging identity exposed by the full-chat header and
    /// sidebar menu, but from the overview's embedded live-chat panel.
    pub(super) fn copy_panel_chat_identity(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(text) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map(crate::shell::chat_identity_for)
        else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_panel_identity = Some(chat_id.clone());
        self.copied_panel_identity_seq = self.copied_panel_identity_seq.wrapping_add(1);
        let seq = self.copied_panel_identity_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            this.update(cx, |this, cx| {
                if this.copied_panel_identity_seq == seq {
                    this.copied_panel_identity = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The chat the panel currently shows — `Some` only while it's open and
    /// something is selected (see [`panel_chat_id`]).
    pub(super) fn panel_chat_id(&self, cx: &Context<Self>) -> Option<String> {
        panel_chat_id(
            self.chat_panel_open,
            self.state.read(cx).selected_chat.as_deref(),
        )
    }

    /// "Close out worktree…": the confirmation dialog is Shell-owned (it's a
    /// window modal, shared with the sidebar chat menu's entry point).
    pub(super) fn open_closeout(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_chat_closeout(chat_id, cx);
        });
    }

    pub(super) fn set_archived(&mut self, chat_id: String, archived: bool, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.set_chat_archived(chat_id, archived, cx);
        });
    }

    /// The interactive chat panel — `Some` only while `chat_panel_open` is
    /// set and a chat is selected. Header ("Open full chat →" / ×), the
    /// Shell's live transcript, and the Shell's composer docked BELOW it in
    /// a flex column (not overlaid, unlike the chat route — so the
    /// transcript's bottom clearance is zero here; `Shell::render` sets that
    /// per frame on this route).
    pub(super) fn render_chat_panel(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let Some(chat_id) = self.panel_chat_id(cx) else {
            // Selection cleared under the open panel (chat deleted/archived,
            // new-session): nothing to show, so the panel closes itself.
            self.chat_panel_open = false;
            return None;
        };
        self.ensure_link_status(chat_id.clone(), cx);
        let (chat, space) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|chat| chat.id == chat_id)?.clone();
            let space = chat
                .space_id
                .as_deref()
                .and_then(|id| state.space_row(id))
                .cloned();
            (chat, space)
        };
        let title: SharedString = chat
            .title
            .clone()
            .unwrap_or_else(|| "Chat".to_string())
            .into();
        let links = self
            .link_status
            .get(&chat_id)
            .map(|entry| header_links(&entry.status));
        let mut metadata = crate::chat_metadata::metadata_items(
            &chat,
            space.as_ref(),
            links.as_ref(),
            self.state.read(cx).selected_chat_diff_target(&chat_id),
        );
        crate::chat_metadata::insert_account_item(
            &mut metadata,
            self.state.read(cx).chat_account_badge(&chat_id),
        );
        if let Some(path) = self
            .work_item_index
            .item_for_chat(&chat_id)
            .and_then(|item| self.work_item_index.path_label(item))
        {
            metadata.push(crate::chat_metadata::work_item_item(
                &path.short,
                &path.full,
            ));
        }
        let drift_mark = self
            .work_item_index
            .instruction_drift_for_chat(&chat_id)
            .then(|| {
                self.instruction_drift_mark(
                    "overview-chat-panel-instruction-drift".into(),
                    &chat_id,
                    true,
                    theme,
                )
            });
        // The chat route drives these per frame (`render_main`); it doesn't
        // run on this route, so the panel does. Without the settled-docked
        // frame, arriving from the blank new-session screen would leave the
        // composer in its big centered hero layout.
        self.composer.update(cx, |composer, cx| {
            composer.set_dock_frame(crate::composer_dock::DockFrame::settled(true), cx);
            composer.set_available_width(CHAT_PANEL_W - 2.0 * CHAT_PANEL_COMPOSER_PAD, cx);
        });
        let jump_pill = self.transcript.read(cx).jump_button_shown().then(|| {
            let transcript = self.transcript.clone();
            div()
                .absolute()
                .bottom(px(10.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("overview-chat-panel-jump")
                        .h(px(26.0))
                        .px(px(11.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface_raised)
                        .shadow_md()
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .hover(|el| el.bg(theme.element_hover))
                        .on_click(move |_, _, cx| {
                            transcript.update(cx, |transcript, cx| transcript.jump_to_bottom(cx));
                        })
                        .child(SharedString::from("↓ Scroll to bottom")),
                )
        });
        let full_chat_id = chat_id.clone();
        let copy_chat_id = chat_id.clone();
        let copied = self.copied_panel_identity.as_deref() == Some(chat_id.as_str());
        Some(
            div()
                .id("overview-chat-panel")
                .debug_selector(|| "overview-chat-panel".into())
                .flex_none()
                .when(self.in_sidebar, |el| el.flex_1().min_w_0())
                .when(!self.in_sidebar, |el| el.w(px(CHAT_PANEL_W)))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                // Same attachment path as the chat route's dropzone.
                .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    this.composer
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.text)
                                        .child(title),
                                )
                                .children(drift_mark)
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.0))
                                        .child(
                                            div()
                                                .id("overview-chat-panel-copy")
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .px(px(6.0))
                                                .py(px(2.0))
                                                .rounded(px(4.0))
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(if copied {
                                                    theme.success
                                                } else {
                                                    theme.text_muted
                                                })
                                                .hover(|el| {
                                                    el.bg(theme.element_hover)
                                                        .text_color(theme.text)
                                                })
                                                .child(
                                                    crate::icons::icon(if copied {
                                                        crate::icons::CHECK
                                                    } else {
                                                        crate::icons::COPY
                                                    })
                                                    .size(px(11.0)),
                                                )
                                                .child(SharedString::from(if copied {
                                                    "Copied"
                                                } else {
                                                    "Copy name + IDs"
                                                }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.copy_panel_chat_identity(
                                                        copy_chat_id.clone(),
                                                        cx,
                                                    );
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-outline")
                                                .debug_selector(|| {
                                                    "overview-chat-panel-outline".into()
                                                })
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(theme.text_muted)
                                                .hover(|el| el.text_color(theme.text))
                                                .child(SharedString::from("Outline"))
                                                .hint(
                                                    "Show this Chat in the Outline",
                                                    HintKey::Command(
                                                        WorkOutlineCommand::ToggleOutlineFocus,
                                                    ),
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    let Some(shell) = this.shell.upgrade() else {
                                                        return;
                                                    };
                                                    // The Shell updates this Overview.
                                                    window.defer(cx, move |window, cx| {
                                                        shell.update(cx, |shell, cx| {
                                                            shell.toggle_outline_focus(window, cx)
                                                        });
                                                    });
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-open-full")
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(theme.accent)
                                                .child(SharedString::from("Open full chat →"))
                                                .hint(
                                                    "Open this Chat as the full chat",
                                                    HintKey::Command(
                                                        WorkOutlineCommand::OpenFullChat,
                                                    ),
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    // `open` acknowledges the
                                                    // unread done; the key path
                                                    // is `prepare_full_chat`.
                                                    this.close_chat_panel(cx);
                                                    this.open(full_chat_id.clone(), cx);
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-close")
                                                .cursor_pointer()
                                                .text_color(theme.text_muted)
                                                .child(SharedString::from("×"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.close_chat_panel(cx);
                                                })),
                                        ),
                                ),
                        )
                        .child(crate::chat_metadata::metadata_strip(
                            "overview-chat-panel",
                            metadata,
                            theme,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(self.transcript.clone())
                        .children(jump_pill),
                )
                .child(
                    div()
                        .flex_none()
                        .w_full()
                        .px(px(CHAT_PANEL_COMPOSER_PAD))
                        .pb(px(8.0))
                        .child(self.composer.clone()),
                )
                .into_any_element(),
        )
    }
}
