use super::*;

impl Shell {
    // ---- sidebar sections ----

    // ---- space context menu / rename / delete overlays ----

    pub(super) fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    pub(in crate::shell) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Space name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    /// Open the per-project agent account picker (Default + managed accounts).
    pub(in crate::shell) fn open_project_account_picker(
        &mut self,
        space_id: String,
        harness: mast_proto::HarnessId,
        cx: &mut Context<Self>,
    ) {
        self.close_space_menu(cx);
        let Some(device_id) = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.device_id.clone())
        else {
            return;
        };
        let state = self.state.clone();
        let flow = cx.new(|cx| {
            crate::managed_accounts::AccountFlow::new(
                state,
                harness,
                Some(space_id),
                Some(device_id),
                cx,
            )
        });
        self.account_flow_sub = Some(cx.subscribe(&flow, |this: &mut Shell, _, _, cx| {
            this.account_flow = None;
            this.account_flow_sub = None;
            cx.notify();
        }));
        self.account_flow = Some(flow);
        cx.notify();
    }

    pub(in crate::shell) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(in crate::shell) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(in crate::shell) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some((space_id, position)) = self.space_menu.get().cloned() {
            let closing = self.space_menu.closing_since();
            let rename_id = space_id.clone();
            let delete_id = space_id.clone();
            let account_labels: Vec<(mast_proto::HarnessId, String)> = {
                let state = self.state.read(cx);
                let snapshot = state
                    .space_row(&space_id)
                    .and_then(|space| state.managed_accounts_for(Some(&space.device_id)));
                [
                    mast_proto::HarnessId::ClaudeCode,
                    mast_proto::HarnessId::Codex,
                ]
                .into_iter()
                .map(|h| {
                    (
                        h,
                        crate::managed_accounts::menu_label(snapshot, &space_id, h),
                    )
                })
                .collect()
            };
            let account_rows: Vec<AnyElement> = account_labels
                .into_iter()
                .map(|(harness, label)| {
                    let pick_id = space_id.clone();
                    popover::menu_row(&theme, false, format!("space-menu-account-{harness:?}"))
                        .id(SharedString::from(format!(
                            "space-menu-account-{harness:?}"
                        )))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_project_account_picker(pick_id.clone(), harness, cx)
                        }))
                        .child(
                            icon(icons::KEY_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from(label))
                        .into_any_element()
                })
                .collect();
            let menu = popover::popover_card(&theme)
                .w(px(230.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_space_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-rename-{space_id}"))
                        .id("space-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_space(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .children(account_rows)
                .child(popover::menu_separator(&theme))
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-delete-{space_id}"))
                        .id("space-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_space_menu(cx);
                            this.delete_space_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Remove…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "space-context-menu",
                position,
                menu,
                closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_space_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_space_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename Space"))
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
                            popover::btn_ghost(&theme, "Cancel", "rename-space-cancel")
                                .id("rename-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_space_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-space-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_space(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-space-dialog", viewport, card));
        }

        if let Some(flow) = self.account_flow.clone() {
            let card = flow.update(cx, |flow, cx| flow.render_card(&theme, window, cx));
            overlays.push(popover::modal("account-flow-dialog", viewport, card));
        }

        if let Some(space_id) = self.delete_space_confirm.clone() {
            let (name, device, count) = {
                let state = self.state.read(cx);
                let space = state.space_row(&space_id);
                (
                    space
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "this Space".into()),
                    space
                        .and_then(|s| state.device_name(&s.device_id))
                        .unwrap_or("its device")
                        .to_string(),
                    state.chats_in_space(&space_id).len(),
                )
            };
            let copy = delete_space_copy(&name, &device, count);
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Remove Space?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-space-cancel")
                                .id("delete-space-cancel")
                                .debug_selector(|| "delete-space-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_space_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Remove")
                                .id("delete-space-confirm")
                                .debug_selector(|| "delete-space-confirm".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_space(space_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-space-dialog", viewport, card));
        }

        overlays
    }
}
