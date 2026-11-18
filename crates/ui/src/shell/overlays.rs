use super::*;

impl Shell {
    pub(super) fn render_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some(menu_state) = self.chat_menu.get().cloned() {
            let chat_id = menu_state.chat_id;
            let position = menu_state.position;
            let chat_menu_closing = self.chat_menu.closing_since();
            let is_pinned = self.active_sidebar_pins(cx).contains(&chat_id);
            let rename_id = chat_id.clone();
            let pin_id = chat_id.clone();
            let archive_id = chat_id.clone();
            let delete_id = chat_id.clone();
            let sync_id = chat_id.clone();
            let link_id = chat_id.clone();
            let closeout_id = chat_id.clone();
            let identity_id = chat_id.clone();
            let admin_id = chat_id.clone();
            let revoke_id = chat_id.clone();
            let hand_off_id = chat_id.clone();
            let branch_review_id = chat_id.clone();
            let branch_review_available = self.branch_review_targets.contains_key(&chat_id);
            let branch_review_exists = self.branch_review_targets.get(&chat_id)
                .and_then(|target| self.existing_branch_review(&chat_id, target, cx)).is_some();
            let has_workers = self.has_hand_off_workers(&chat_id, cx);
            let admin_space = self.admin_space_of(&chat_id, cx).is_some();
            let is_admin = self.chat_is_admin(&chat_id, cx);
            let closeable = menu_state.closeable;
            let menu = popover::popover_card(&theme)
                .w(px(216.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_chat_menu(cx);
                }))
                .flex()
                .flex_col();
            let menu = match menu_state.page {
                ChatMenuPage::Root => menu
                    .when(branch_review_available, |menu| menu.child(
                        popover::menu_row(&theme, false, format!("chat-menu-review-work-{chat_id}"))
                            .id("chat-menu-review-work")
                            .debug_selector(|| "chat-menu-review-work".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_chat_menu(cx);
                                this.open_branch_review_for_chat(branch_review_id.clone(), cx);
                            }))
                            .child(SharedString::from(if branch_review_exists { "Open review" } else { "Review this work…" }))
                    ))
                    .when(has_workers, |menu| menu.child(
                        popover::menu_row(&theme, false, format!("chat-menu-hand-off-{chat_id}"))
                            .id("chat-menu-hand-off")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_chat_menu(cx);
                                this.open_hand_off(hand_off_id.clone(), cx);
                            }))
                            .child(icon(icons::CHAT_ROUND_LINE).size(px(16.0)).text_color(theme.text_muted))
                            .child(SharedString::from("Hand off to new chat…")),
                    ))
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-rename-{chat_id}"))
                            .id("chat-menu-rename")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_rename_chat(rename_id.clone(), cx)
                            }))
                            .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                            .child(SharedString::from("Rename…")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-pin-{chat_id}"))
                            .id("chat-menu-pin")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_chat_pinned(pin_id.clone(), !is_pinned, cx)
                            }))
                            .child(icon(icons::PIN).size(px(16.0)).text_color(theme.text_muted))
                            .child(SharedString::from(if is_pinned { "Unpin" } else { "Pin" })),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-archive-{chat_id}"))
                            .id("chat-menu-archive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.archive_chat(archive_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::ARCHIVE_MINIMALISTIC)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Archive")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-sync-{chat_id}"))
                            .id("chat-menu-sync")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_chat_menu(cx);
                                this.sync_external_session(sync_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::REFRESH)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Sync imported session")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-link-{chat_id}"))
                            .id("chat-menu-link")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.link_chat_from_menu(link_id.clone(), window, cx)
                            }))
                            .child(
                                icon(icons::PAPERCLIP)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Add PR / ticket…")),
                    )
                    .when(admin_space, |menu| {
                        menu.child(
                            popover::menu_row(&theme, false, format!("chat-menu-admin-{chat_id}"))
                                .id("chat-menu-admin")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_chat_menu(cx);
                                    this.open_admin_dialog(&admin_id, cx)
                                }))
                                .child(
                                    icon(icons::SHIELD)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from(crate::admin::grant_entry_label(is_admin))),
                        )
                        .when(is_admin, |menu| {
                            menu.child(
                                popover::menu_row(
                                    &theme,
                                    false,
                                    format!("chat-menu-admin-revoke-{chat_id}"),
                                )
                                .id("chat-menu-admin-revoke")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_chat_menu(cx);
                                    this.revoke_chat_admin(&revoke_id, cx)
                                }))
                                .child(
                                    icon(icons::SHIELD)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from(crate::admin::REVOKE_LABEL)),
                            )
                        })
                    })
                    .when(closeable, |menu| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-menu-closeout-{chat_id}"),
                            )
                            .id("chat-menu-closeout")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chat_closeout(closeout_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::GIT_BRANCH)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Close out worktree…")),
                        )
                    })
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-identity-{chat_id}"))
                            .id("chat-menu-identity")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_chat_identity(&identity_id, true, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Copy name + IDs")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-copy-{chat_id}"))
                            .id("chat-menu-copy")
                            .on_click(cx.listener(|this, _, _, cx| this.open_chat_copy_menu(cx)))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(div().flex_1().child(SharedString::from("Copy")))
                            .child(
                                icon(icons::ALT_ARROW_RIGHT)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(popover::menu_separator(&theme))
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-delete-{chat_id}"))
                            .id("chat-menu-delete")
                            .text_color(theme.danger)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_chat_menu(cx);
                                this.delete_confirm = Some(delete_id.clone());
                                cx.notify();
                            }))
                            .child(
                                icon(icons::TRASH_BIN_MINIMALISTIC)
                                    .size(px(16.0))
                                    .text_color(theme.danger),
                            )
                            .child(SharedString::from("Delete…")),
                    ),
                ChatMenuPage::Copy => {
                    let chat = self
                        .state
                        .read(cx)
                        .chats
                        .iter()
                        .find(|chat| chat.id == chat_id)
                        .cloned();
                    let harness_link = chat
                        .as_ref()
                        .and_then(crate::links::harness_conversation_link);
                    let session_id = chat
                        .as_ref()
                        .and_then(|chat| chat.harness_session_id.as_deref())
                        .is_some_and(|id| !id.trim().is_empty());
                    let zeron_id = chat_id.clone();
                    let harness_id = chat_id.clone();
                    let session_chat_id = chat_id.clone();
                    menu.child(
                        popover::menu_row(&theme, false, format!("chat-copy-back-{chat_id}"))
                            .id("chat-copy-back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(menu) = this.chat_menu.open_mut() {
                                    menu.page = ChatMenuPage::Root;
                                    cx.notify();
                                }
                            }))
                            .child(
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Back")),
                    )
                    .child(popover::menu_separator(&theme))
                    .child(
                        popover::menu_row(&theme, false, format!("chat-copy-zeron-{chat_id}"))
                            .id("chat-copy-zeron")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_mast_conversation_link(&zeron_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Mast conversation link")),
                    )
                    .when_some(harness_link, |menu, link| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-copy-harness-{chat_id}"),
                            )
                            .id("chat-copy-harness")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_harness_conversation_link(&harness_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from(link.label)),
                        )
                    })
                    .when(session_id, |menu| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-copy-session-{chat_id}"),
                            )
                            .id("chat-copy-session")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_harness_session_id(&session_chat_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Harness session ID")),
                        )
                    })
                }
            }
            .into_any_element();
            overlays.push(popover::menu_at(
                "chat-context-menu",
                position,
                menu,
                chat_menu_closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename session"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(&theme, input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-chat-cancel")
                                .id("rename-chat-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-chat-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_chat(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-chat-dialog", viewport, card));
        }

        overlays.extend(self.render_space_overlays(viewport, window, cx));
        overlays.extend(self.render_section_overlays(viewport, window, cx));
        overlays.extend(self.render_running_overlay(window, cx));
        if let Some(overlay) = self.render_work_outline_menu(window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_command_palette(viewport, window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_workspace_search(viewport, window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_add_space_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_project_action_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }

        if let Some(chat_id) = self.delete_confirm.clone() {
            let title = transcript::single_line(
                &self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|c| c.id == chat_id)
                    .and_then(|c| c.title.clone())
                    .unwrap_or_else(|| "New session".into()),
            );
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Delete session?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    format!("\u{201C}{title}\u{201D} will be permanently deleted. This can\u{2019}t be undone."),
                )))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-chat-cancel")
                                .id("delete-chat-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Delete")
                                .id("delete-chat-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_chat(chat_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-chat-dialog", viewport, card));
        }

        if let Some(overlay) = self.render_admin_dialog(viewport, cx) {
            overlays.push(overlay);
        }

        if let Some(overlay) = self.render_closeout_overlay(viewport, cx) {
            overlays.push(overlay);
        }

        if let Some(overlay) = self.render_work_outline_modal(viewport, window, cx) {
            overlays.push(overlay);
        }

        if let Some(sync) = self.render_sync_overlay(viewport, cx) {
            overlays.push(sync);
        }

        overlays
    }

}
