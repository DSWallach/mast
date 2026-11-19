use super::*;

impl Shell {
    pub(super) fn bind_shell_actions(
        &mut self,
        root: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        root.on_drag_move::<SidebarSessionDrag>(cx.listener(Self::contain_pinned_session_drag))
            .on_drop::<SidebarSessionDrag>(cx.listener(|this, _, _, cx| {
                this.cancel_sidebar_session_transfer(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.sidebar_session_transfer.is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.sidebar_session_transfer.is_some() {
                        cx.notify();
                    }
                }),
            )
            .capture_key_down(cx.listener(Self::on_key_down_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drag_move(cx.listener(Self::on_sidebar_drag))
            .on_drag_move(cx.listener(Self::on_right_pane_drag))
            .on_drag_move(cx.listener(Self::on_files_panel_drag))
            .on_drag_move(cx.listener(Self::on_terminal_drag))
            // The panel shortcuts are chat-scoped chrome: in Settings they are
            // no-ops (zeron __root.tsx gates the hotkey on `!isSettings`, and
            // the terminal panel is only mounted on session routes). The
            // sidebar toggle stays live everywhere, as in the original.
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_terminal(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                if matches!(this.route, Route::Chat) && this.right_pane_open(cx) {
                    let file = match this.resolved_right_active(cx) {
                        RightSurface::File(id) => this.file_surfaces.get(&id).cloned(),
                        _ => None,
                    };
                    if let Some(file) = file {
                        file.update(cx, |file, cx| file.save_active_document(cx));
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(cx)))
            // Native Settings menu item and the platform convention (Cmd+, on
            // macOS, Ctrl+, elsewhere) always land on the default section.
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings(SettingsSection::Devices, cx)
            }))
            // Chat-scoped, unlike new-session — `cycle_session` holds the guard
            // and says why.
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.cycle_session(true, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.cycle_session(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_right_pane(cx);
                    if !this.right_pane_open(cx) {
                        // The hidden editor can retain a focus handle after unmounting.
                        // Restore a mounted target so the next shortcut can reopen it.
                        window.focus(&this.composer.focus_handle(cx), cx);
                    }
                }
            }))
            // The explorer's own toggle (the titlebar tree button): docks or
            // undocks the explorer portion without touching the surface host.
            .on_action(cx.listener(|this, _: &ToggleFiles, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_files_panel(window, cx);
                }
            }))
            // Chat-scoped like the panel toggles: Settings has no current
            // session to archive. Quiet under an open popover, like the other
            // session-nav shortcuts.
            .on_action(cx.listener(|this, _: &ArchiveSession, window, cx| {
                if this.overlay_owns_keyboard(cx) {
                    return;
                }
                // In the Outline tree it archives the selected row instead.
                if this.archive_selected_outline_row(window, cx) {
                    return;
                }
                if matches!(this.route, Route::Chat) {
                    this.archive_selected_chat(cx)
                }
            }))
            // A jump routes back to chat itself, so Settings is not a dead
            // spot — the same call a click on that sidebar row makes. But an
            // open picker/palette owns the keyboard: no jumping underneath
            // it. The MODEL menu advertises these same slots on its rows and
            // this matched binding beats its key handler to the dispatch —
            // forward the slot instead of eating it.
            .on_action(cx.listener(|this, jump: &JumpSession, _, cx| {
                // Start Chat's model menu advertises the same slots.
                let pickers = this
                    .start_chat_pickers()
                    .or_else(|| this.hand_off_pickers())
                    .filter(|pickers| pickers.read(cx).is_open())
                    .unwrap_or_else(|| this.composer.read(cx).pickers().clone());
                let handled = pickers.update(cx, |pickers, cx| pickers.jump_model_slot(jump.0, cx));
                if !handled && !this.overlay_owns_keyboard(cx) {
                    this.jump_to_session(jump.0, cx)
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, event, _, cx| this.on_modifiers_changed(event, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.open_workspace_search(workspace_search::SearchMode::Files, window, cx)
            }))
            .on_action(cx.listener(|this, _: &QuickOpenSymbols, window, cx| {
                this.open_workspace_search(workspace_search::SearchMode::Symbols, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FileNavigationBack, window, cx| this.move_file_navigation(false, window, cx)))
            .on_action(cx.listener(|this, _: &FileNavigationForward, window, cx| this.move_file_navigation(true, window, cx)))
            .on_action(cx.listener(|this, _: &SearchWorkspaceContent, window, cx| {
                this.open_workspace_search(workspace_search::SearchMode::Content, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBlameGutter, window, cx| {
                this.set_blame_gutter_everywhere(!this.blame_gutter, window, cx)
            }))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| {
                this.open_workspace_search(workspace_search::SearchMode::Line, window, cx)
            }))
            // Works from any route, like New session: the overview is a
            // top-level surface, not chat-scoped. Quiet under an open
            // picker/palette so it never navigates out from underneath one.
            .on_action(cx.listener(|this, _: &OpenOverview, _, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_overview(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusOverviewSearch, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    match this.route {
                        Route::Overview => {
                            let page = this.ensure_overview_page(cx);
                            page.update(cx, |overview, cx| {
                                overview.focus_search(window, cx);
                            });
                        }
                        Route::PrReviewChats => {
                            let page = this.ensure_pr_review_chats_page(cx);
                            page.update(cx, |page, cx| page.focus_search(window, cx));
                        }
                        // Not a search surface: let a deeper owner of ⌘F (an
                        // Artifact tab's find bar) have the key.
                        _ => cx.propagate(),
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &OpenPrReviewChats, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_pr_review_chats_focused(window, cx);
                }
            }))
            .on_action(cx.listener(
                |this, action: &work_outline_ui::RunWorkOutlineCommand, window, cx| {
                    if !this.overlay_owns_keyboard(cx) {
                        this.run_work_outline_command(action.0, window, cx);
                    }
                },
            ))
            .on_action(cx.listener(
                |this,
                 _: &crate::work_outline::outline_view::context_menu::OpenContextMenu,
                 window,
                 cx| { this.open_work_outline_menu_from_keyboard(window, cx) },
            ))
            // ⌘P (customizable) is the PR page, like the fixed ⇧⌘P alias.
            .on_action(cx.listener(|this, _: &OpenMyPrs, window, cx| {
                if !this.overlay_owns_keyboard(cx) {
                    this.open_pr_review_chats_focused(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenItemTab, _, cx| this.open_item_tab(cx)))
            .on_action(cx.listener(|this, _: &OpenTeamTab, _, cx| this.open_team_tab(cx)))
            .on_action(cx.listener(|this, _: &PopOutSurface, _, cx| {
                this.pop_out_active_surface(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRunningChats, window, cx| {
                this.toggle_running_chats(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleItemCard, _, cx| this.toggle_chat_item_card_pin(cx)),
            )
            .on_action(cx.listener(|this, _: &OpenModelPicker, window, cx| {
                // Inside Start Chat the shortcut opens the dialog's picker.
                if let Some(pickers) = this
                    .start_chat_pickers()
                    .or_else(|| this.hand_off_pickers())
                {
                    pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                    return;
                }
                if matches!(this.route, Route::Chat) && !this.overlay_owns_keyboard(cx) {
                    let pickers = this.composer.read(cx).pickers().clone();
                    pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if this.add_space.is_some() {
                    this.add_space = None;
                    cx.notify();
                } else {
                    this.open_add_space(cx);
                }
            }))
    }
}
