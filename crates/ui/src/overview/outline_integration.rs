use super::*;

impl Overview {
    /// Switch the main column's presentation and persist it.
    pub(super) fn set_mode(&mut self, mode: OverviewMode, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.view_mode = mode;
        self.hovered_row = None;
        self.outline_dirty = true;
        self.view_prefs_changed(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn mode(&self) -> OverviewMode {
        self.view_mode
    }

    #[cfg(test)]
    /// The search field's focus handle (tests of context-scoped keys).
    #[cfg(test)]
    pub(crate) fn search_focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        gpui::Focusable::focus_handle(self.search.read(cx), cx)
    }

    pub(crate) fn outline_entity(&self) -> Entity<OutlineView> {
        self.outline.clone()
    }

    #[cfg(test)]
    pub(crate) fn has_work_item_details(&self) -> bool {
        self.work_item_details.is_some()
    }

    /// Tell the watch reconciler which Spaces' outlines the Overview needs:
    /// all of them (path chips and attention on the List need them too).
    /// Only on change, so it never loops with the `AppState` observer.
    pub(super) fn declare_outline_spaces(&mut self, cx: &mut Context<Self>) {
        if self.outline_fixture.is_some() {
            return;
        }
        let ids: Vec<String> = self
            .state
            .read(cx)
            .spaces
            .iter()
            .map(|space| space.id.clone())
            .collect();
        if self.declared_outline_spaces.as_ref() == Some(&ids) {
            return;
        }
        self.declared_outline_spaces = Some(ids.clone());
        self.state
            .update(cx, |state, cx| state.declare_work_outline_visible(ids, cx));
    }

    /// Push fresh inputs into the Outline when `rows()` rebuilt or an
    /// Outline-only option changed.
    pub(super) fn sync_outline(&mut self, cx: &mut Context<Self>) {
        if !self.outline_dirty {
            return;
        }
        self.outline_dirty = false;
        let inputs = OutlineInputs {
            spaces: self.outline_spaces(cx),
            chats: (*self.outline_chats).clone(),
            options: OutlineOptions {
                hide_completed: self.hide_stale && !self.outline_show_completed,
                show_archived: self.show_archived,
                query: self.search.read(cx).text().trim().to_lowercase(),
                all_spaces: true,
                // The Outline owns the reveal (`OutlineView`).
                show_hidden_spaces: false,
                // Likewise: the view compacts single-chat items itself.
                compact_single_chat: false,
            },
        };
        self.outline
            .update(cx, |outline, cx| outline.set_inputs(inputs, cx));
    }

    pub(super) fn render_outline_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let details_key = self.work_item_details.as_ref().map(|details| {
            let details = details.read(cx);
            row_key::item(&details.space_id, &details.item_id)
        });
        let panel_chat = self.panel_highlight.clone();
        self.outline.update(cx, |outline, _| {
            outline.set_highlights(details_key, panel_chat)
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.outline.clone())
            .into_any_element()
    }

    /// Outline-only toolbar controls: the inline "New item" entry point, the
    /// reveal for Spaces hidden from the Outline, and the explicit reveal
    /// for completed items hidden by "Hide stale".
    pub(super) fn render_outline_toolbar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let pill =
            |id, label, active| crate::components::outline_toolbar_button(id, label, active, theme);
        let mut out = vec![
            pill("outline-new-item", "New item".into(), false)
                .debug_selector(|| "outline-new-item".into())
                .hint(
                    "Create a work item",
                    HintKey::Command(WorkOutlineCommand::CreateWorkItem),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run_work_outline_command(WorkOutlineCommand::CreateWorkItem, window, cx);
                }))
                .into_any_element(),
        ];
        // Required Actions: always reachable; the count uses the attention token.
        let open = crate::work_outline::required_actions::open_request_count(
            &self.required_action_spaces(cx),
        );
        out.push(
            pill(
                "outline-required-actions",
                if open > 0 {
                    format!("Required actions ({open})")
                } else {
                    "Required actions".to_string()
                },
                self.required_actions_panel.is_some(),
            )
            .debug_selector(|| "outline-required-actions".into())
            .hint(
                "Requests waiting on you",
                HintKey::Command(WorkOutlineCommand::OpenRequiredActions),
            )
            .when(open > 0, |el| {
                el.text_color(crate::work_outline::chips::attention_color(theme))
            })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.required_actions_panel.is_some() {
                    this.close_required_actions(cx);
                } else {
                    this.open_required_actions(cx);
                }
            }))
            .into_any_element(),
        );
        // Linear (only while some Space has Linear on).
        out.extend(self.linear_toolbar_button(theme, cx));
        // The shortcut reference, on demand (also `?` in the Outline and the
        // palette's Show Outline Shortcuts).
        out.push(
            crate::components::outline_shortcuts_button(theme)
                .hint(
                    "Keyboard shortcuts",
                    HintKey::Outline(crate::shell::work_outline_ui::OutlineNavKey::ShowShortcuts),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(shell) = this.shell.upgrade() {
                        shell.update(cx, |shell, cx| shell.open_outline_shortcuts(cx));
                    }
                }))
                .into_any_element(),
        );
        // Projects hidden from the Outline (multi-Space only): reveal them as
        // dimmed rows with Unhide.
        let (hidden_spaces, showing_hidden) = {
            let outline = self.outline.read(cx);
            (
                outline.hidden_space_count(),
                outline.showing_hidden_spaces(),
            )
        };
        if hidden_spaces > 0 {
            out.push(
                pill(
                    "outline-hidden-spaces",
                    format!("Hidden Spaces ({hidden_spaces})"),
                    showing_hidden,
                )
                .debug_selector(|| "outline-hidden-spaces".into())
                .hint(
                    if showing_hidden {
                        "Stop showing the Spaces hidden from the Outline"
                    } else {
                        "Show the Spaces hidden from the Outline, to unhide them"
                    },
                    HintKey::None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.outline.update(cx, |outline, cx| {
                        outline.set_show_hidden_spaces(!showing_hidden, cx)
                    });
                }))
                .into_any_element(),
            );
        }
        if self.hide_stale {
            let hidden = self.outline.read(cx).hidden_completed();
            let label = if self.outline_show_completed {
                "Completed shown".to_string()
            } else {
                format!("Completed hidden ({hidden})")
            };
            out.push(
                pill("outline-show-completed", label, self.outline_show_completed)
                    .hint(
                        if self.outline_show_completed {
                            "Hide completed work items again"
                        } else {
                            "Show the completed work items Hide stale hides"
                        },
                        HintKey::None,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.outline_show_completed = !this.outline_show_completed;
                        this.outline_dirty = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    /// The List work-item filter dropdown (same shape as "Repos").
    pub(super) fn render_work_item_filter(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = !self.work_item_filter.is_empty() || self.work_item_filter_popup.is_open();
        let trigger = crate::components::overview_work_item_filter(theme, active)
            .child(SharedString::from(format!(
                "{} ▾",
                work_item_filter::filter_label(&self.work_item_filter)
            )))
            .hint("Filter by work item", HintKey::None)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.work_item_filter_popup.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.work_item_filter_popup.take_press_was_open() {
                    this.close_work_item_filter(cx);
                } else {
                    this.work_item_filter_popup.open(());
                    cx.notify();
                }
            }));
        if self.work_item_filter_popup.get().is_none() {
            return trigger.into_any_element();
        }
        let closing = self.work_item_filter_popup.closing_since();
        let popup = theme.for_popup();
        let check_row = |id: gpui::ElementId, label: String, depth: u32, checked: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(7.0))
                .pl(px(6.0 + depth as f32 * 12.0))
                .pr(px(6.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(popup.element_hover))
                .child(
                    div()
                        .flex_none()
                        .size(px(12.0))
                        .rounded(px(3.0))
                        .border_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(crate::typography::ui_rems(9.0))
                        .when(checked, |el| {
                            el.bg(popup.accent)
                                .border_color(popup.accent)
                                .text_color(gpui::white())
                                .child("✓")
                        })
                        .when(!checked, |el| el.border_color(popup.border_strong)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(if checked {
                            popup.text
                        } else {
                            popup.text_muted
                        })
                        .child(SharedString::from(label)),
                )
        };
        let choices = self.work_item_index.choices();
        let multi_space = self.state.read(cx).spaces.len() > 1;
        let mut list: Vec<gpui::AnyElement> = Vec::new();
        let unassociated = work_item_filter::UNASSOCIATED_KEY.to_string();
        list.push(
            check_row(
                "overview-work-item-filter-unassociated".into(),
                "Unassociated chats".into(),
                0,
                self.work_item_filter.contains(&unassociated),
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.toggle_work_item_filter(&unassociated, cx)),
            )
            .into_any_element(),
        );
        let mut last_space: Option<String> = None;
        for (ix, choice) in choices.into_iter().enumerate() {
            if multi_space && last_space.as_deref() != Some(choice.space_id.as_str()) {
                last_space = Some(choice.space_id.clone());
                let label = self
                    .state
                    .read(cx)
                    .space_row(&choice.space_id)
                    .map(|space| space.display_name().to_string())
                    .unwrap_or_default();
                list.push(
                    div()
                        .px(px(6.0))
                        .pt(px(6.0))
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(popup.text_muted)
                        .child(SharedString::from(label))
                        .into_any_element(),
                );
            }
            let key = choice.item_id.clone();
            list.push(
                check_row(
                    ("overview-work-item-filter-row", ix).into(),
                    choice.title,
                    choice.depth,
                    self.work_item_filter.contains(&key),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_work_item_filter(&key, cx)))
                .into_any_element(),
            );
        }
        let card = popover::popover_card(&popup)
            .w(px(260.0))
            .max_h(px(380.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_work_item_filter(cx)))
            .child(
                div().flex().gap(px(6.0)).pb(px(4.0)).child(
                    div()
                        .id("overview-work-item-filter-clear")
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(popup.text_muted)
                        .border_1()
                        .border_color(popup.border)
                        .hover(|el| el.bg(popup.element_hover).text_color(popup.text))
                        .child("All chats")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.work_item_filter.clear();
                            this.work_item_filter_changed(cx);
                        })),
                ),
            )
            .child(
                div()
                    .id("overview-work-item-filter-list")
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scroll()
                    .children(list),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-work-item-filter-menu",
                card.into_any_element(),
                closing,
            ))
            .into_any_element()
    }

    pub(super) fn close_work_item_filter(&mut self, cx: &mut Context<Self>) {
        if self.work_item_filter_popup.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.work_item_filter_popup);
        }
        cx.notify();
    }

    pub(super) fn toggle_work_item_filter(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.work_item_filter.remove(key) {
            self.work_item_filter.insert(key.to_string());
        }
        self.work_item_filter_changed(cx);
    }

    /// Persist and re-derive rows. Filtering only changes which rows exist.
    pub(super) fn work_item_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.work_item_filter_needs_validation = false;
        self.save_string_set(
            overview_positions::WORK_ITEM_FILTER_FILE,
            &self.work_item_filter,
            cx,
        );
        self.rows_dirty = true;
        cx.notify();
    }

    /// Open Work Item Details in the right slot (replacing a details surface
    /// for another item; the chat panel / peek state is left as is).
    pub(crate) fn open_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.work_item_details.as_ref().is_some_and(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            return;
        }
        self.leave_work_item_details(DetailsExit::OpenItem { space_id, item_id }, cx);
    }

    /// Open Work Item Details for `item_id` (`""` = the Space root), then run
    /// `then` on them. The Outline context menu reaches the details' own
    /// actions (status, archive, restore, instructions, execution) this way,
    /// so they keep their confirmations, in-flight guards, and error line.
    /// With an unsaved draft in other details, `then` runs only after
    /// Discard; Keep Editing drops it.
    pub(crate) fn with_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        window: &Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut WorkItemDetails, &mut Window, &mut Context<WorkItemDetails>) + 'static,
    ) {
        let handle = window.window_handle();
        // Deferred: callers are mid-update of the Shell and this Overview.
        let run = move |this: &Self, cx: &mut Context<Self>| {
            let Some(details) = this.work_item_details.clone() else {
                return;
            };
            let space_id = details.read(cx).space_id.clone();
            let snapshot = this.outline_snapshot(&space_id, cx);
            let chats = this.outline_chats.clone();
            details.update(cx, |details, _| details.set_data(snapshot, chats));
            cx.defer(move |cx| {
                handle
                    .update(cx, |_, window, cx| {
                        details.update(cx, |details, cx| then(details, window, cx))
                    })
                    .ok();
            });
        };
        let current = self.work_item_details.clone();
        if current.as_ref().is_some_and(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            run(self, cx);
            return;
        }
        let Some(current) = current else {
            self.replace_work_item_details(space_id, item_id, cx);
            run(self, cx);
            return;
        };
        let overview = cx.entity().downgrade();
        current.update(cx, |current, cx| {
            current.confirm_discard(cx, move |cx| {
                overview
                    .update(cx, |this, cx| {
                        this.replace_work_item_details(space_id, item_id, cx);
                        run(this, cx);
                    })
                    .ok();
            })
        });
    }

    /// Leave the current Work Item Details (switch item, open a Chat, or
    /// close) without silently dropping a draft: with unsaved edits this asks
    /// "Keep Editing / Discard" and acts only after Discard.
    pub(super) fn leave_work_item_details(&mut self, exit: DetailsExit, cx: &mut Context<Self>) {
        let Some(details) = self.work_item_details.clone() else {
            self.apply_details_exit(exit, cx);
            return;
        };
        let overview = cx.entity().downgrade();
        details.update(cx, |details, cx| {
            details.confirm_discard(cx, move |cx| {
                overview
                    .update(cx, |this, cx| this.apply_details_exit(exit, cx))
                    .ok();
            })
        });
    }

    pub(super) fn apply_details_exit(&mut self, exit: DetailsExit, cx: &mut Context<Self>) {
        match exit {
            DetailsExit::Close => self.close_work_item_details(cx),
            DetailsExit::OpenChat(chat_id) => {
                self.close_work_item_details(cx);
                self.close_required_actions(cx);
                self.outline_chat_return = Some(OutlineChatReturn {
                    chat_id: chat_id.clone(),
                    full_route: false,
                });
                self.open_chat_panel(chat_id, cx);
            }
            DetailsExit::OpenTracker(chat_id) => {
                self.close_work_item_details(cx);
                if let Some(shell) = self.shell.upgrade() {
                    cx.defer(move |cx| shell.update(cx, |shell, cx| shell.open_outline_tracker(chat_id, cx)));
                }
            }
            DetailsExit::OpenItem { space_id, item_id } => {
                self.replace_work_item_details(space_id, item_id, cx)
            }
        }
    }

    pub(super) fn replace_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        let key = row_key::item(&space_id, &item_id);
        let chat_ids = self
            .state
            .read(cx)
            .work_outline(&space_id)
            .snapshot()
            .and_then(|snapshot| snapshot.items.iter().find(|item| item.id == item_id))
            .map(|item| item.chat_ids.clone())
            .unwrap_or_default();
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            shell
                .update(cx, |shell, cx| {
                    for chat_id in chat_ids {
                        shell.load_branch_review_target(chat_id, cx);
                    }
                })
                .ok();
        });
        let details = cx.new(|cx| {
            WorkItemDetails::new(
                self.state.clone(),
                self.shell.clone(),
                space_id,
                item_id,
                cx,
            )
        });
        let events = cx.subscribe(&details, |this: &mut Self, _, event, cx| match event {
            // `request_close` already confirmed any discard.
            DetailsEvent::Close => this.close_work_item_details(cx),
            DetailsEvent::OpenChat(chat_id) => {
                this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
            }
            DetailsEvent::OpenItem { space_id, item_id } => {
                this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
            }
            DetailsEvent::OpenProposals { space_id, item_id } => {
                this.open_proposals(space_id.clone(), item_id.clone(), cx);
            }
            DetailsEvent::ReassociateChat {
                space_id,
                chat_id,
                work_item_id,
            } => {
                this.outline.update(cx, |outline, cx| {
                    outline.reassociate_chat(space_id, chat_id, work_item_id.clone(), cx)
                });
            }
            DetailsEvent::StartChat { space_id, item_id } => {
                if let Err(notice) = this.start_chat_for_item(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            DetailsEvent::StartPrReview {
                space_id,
                item_id,
                action,
            } => {
                if let Err(notice) =
                    this.start_pr_review_for_item(space_id.clone(), item_id.clone(), *action, cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            DetailsEvent::NewFromTemplate {
                space_id,
                template_id,
            } => {
                let mut flow = crate::work_outline::outline_view::templates::TemplateFlow::new(
                    space_id.clone(),
                    None,
                );
                flow.template_id = template_id.clone();
                this.outline.update(cx, |outline, cx| {
                    outline.request_new_from_template(flow, cx)
                });
            }
            DetailsEvent::Notice { text, error } => {
                this.outline.update(cx, |outline, cx| {
                    if *error {
                        outline.show_error(text, cx)
                    } else {
                        outline.show_notice(text.clone(), cx)
                    }
                });
            }
            DetailsEvent::AddLinearIssues { space_id, item_id } => {
                if let Err(notice) =
                    this.open_project_issue_picker(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_notice(notice, cx));
                }
            }
        });
        self.outline
            .update(cx, |outline, cx| outline.select(&key, cx));
        if let Some((space, item)) = self.pending_effective.take() {
            details.update(cx, |details, cx| {
                if details.space_id == space && details.item_id == item {
                    details.open_effective(cx);
                }
            });
        }
        if let Some((space, item)) = self.pending_proposals.take() {
            details.update(cx, |details, cx| {
                if details.space_id == space && details.item_id == item {
                    details.open_proposals(cx);
                }
            });
        }
        self.work_item_details = Some(details);
        self._details_events = Some(events);
        cx.notify();
    }

    /// Close Work Item Details; the right slot falls back to whatever the
    /// chat panel / peek state says (restoring the previous layout).
    pub(crate) fn close_work_item_details(&mut self, cx: &mut Context<Self>) {
        if self.work_item_details.take().is_some() {
            self._details_events = None;
            cx.notify();
        }
    }

    /// Run one Work Outline command in the Overview: switch to Outline mode
    /// and focus it, resolve the selected work item or Chat, then start the
    /// inline create / rename, open the move or associate picker. With no
    /// valid target it shows a short notice in the Outline; it never silently
    /// does nothing (m0-contract §7). Called by `Shell::run_work_outline_command`.
    pub(crate) fn run_work_outline_command(
        &mut self,
        command: WorkOutlineCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Load saved view prefs before changing mode: `set_mode` persists the
        // in-memory prefs, which would overwrite the saved file with defaults.
        self.ensure_prefs_loaded(cx);
        // The Linear toggle reads where focus is now, and keeps the mode: the
        // column sits beside every body. Only its notice needs the Outline.
        if command == WorkOutlineCommand::OpenLinear {
            if let Err(notice) = self.toggle_linear_panel(window, cx) {
                self.set_mode(OverviewMode::Outline, cx);
                self.outline
                    .update(cx, |outline, cx| outline.show_notice(notice, cx));
            }
            cx.notify();
            return;
        }
        self.set_mode(OverviewMode::Outline, cx);
        let _ = self.rows(cx);
        self.sync_outline(cx);
        let outline = self.outline.clone();
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
        let chat_hint = self.state.read(cx).selected_chat.clone();
        let result: Result<(), &'static str> = match command {
            WorkOutlineCommand::OpenOutline => Ok(()),
            // Handled by the Shell (`toggle_outline_focus`), which needs the
            // route; reaching here just opens and focuses the Outline.
            WorkOutlineCommand::ToggleOutlineFocus => Ok(()),
            // Handled by the Shell (`open_full_chat`) before routing here.
            WorkOutlineCommand::OpenFullChat => Ok(()),
            // Normally handled by the Shell before routing here; the
            // reference is a Shell overlay, so ask for it after this update.
            WorkOutlineCommand::ShowOutlineShortcuts => {
                if let Some(shell) = self.shell.upgrade() {
                    cx.defer(move |cx| {
                        shell.update(cx, |shell, cx| shell.open_outline_shortcuts(cx))
                    });
                }
                Ok(())
            }
            WorkOutlineCommand::CreateWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_create(false, window, cx))
            }
            WorkOutlineCommand::CreateChildWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_create(true, window, cx))
            }
            WorkOutlineCommand::RenameWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_rename(window, cx))
            }
            WorkOutlineCommand::MoveWorkItem => {
                outline.update(cx, |outline, cx| outline.open_move_picker(window, cx))
            }
            WorkOutlineCommand::AssociateChatWithWorkItem => outline.update(cx, |outline, cx| {
                outline.open_associate_picker(chat_hint, window, cx)
            }),
            WorkOutlineCommand::OpenEffectiveInstructions => {
                match self.effective_instructions_target(chat_hint.as_deref(), cx) {
                    Some((space_id, item_id)) => {
                        self.open_effective_instructions(space_id, item_id, cx);
                        Ok(())
                    }
                    None => Err("Select a work item first"),
                }
            }
            // Handled by the Shell where the Chat is; reaching here means no
            // Chat (or no excerpt) was available.
            WorkOutlineCommand::CreateInstructionFromChat => {
                Err("Open a Chat and select text to create an instruction from it")
            }
            WorkOutlineCommand::OpenRequiredActions => {
                self.open_required_actions(cx);
                Ok(())
            }
            // Handled above, before the Outline takes focus.
            WorkOutlineCommand::OpenLinear => Ok(()),
            WorkOutlineCommand::StartChatForWorkItem => {
                match self.outline.read(cx).selected_item() {
                    Some((space_id, item_id)) => self.start_chat_for_item(space_id, item_id, cx),
                    None => Err("Select a work item first"),
                }
            }
            WorkOutlineCommand::MoveWorkItemUp
            | WorkOutlineCommand::MoveWorkItemDown
            | WorkOutlineCommand::IndentWorkItem
            | WorkOutlineCommand::OutdentWorkItem => match command.restructure_op() {
                Some(op) => outline.update(cx, |outline, cx| outline.restructure_selection(op, cx)),
                None => Ok(()),
            },
        };
        if let Err(notice) = result {
            outline.update(cx, |outline, cx| outline.show_notice(notice, cx));
        }
        cx.notify();
    }

    /// Toggle Outline Focus, Chat → tree: switch to Outline mode, reveal and
    /// select `chat`'s row (expanding collapsed ancestors, scrolled into
    /// view) and focus the tree. `chat` also becomes the return target. A
    /// Chat the Outline does not show (projectless, another Space, filtered
    /// out) leaves the selection alone; the tree is focused either way.
    pub(crate) fn focus_outline_on_chat(
        &mut self,
        chat: Option<OutlineChatReturn>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(OverviewMode::Outline, cx);
        let _ = self.rows(cx);
        self.sync_outline(cx);
        if let Some(chat) = chat {
            let key = row_key::chat(&chat.chat_id);
            self.outline
                .update(cx, |outline, cx| outline.reveal(&key, cx));
            self.outline_chat_return = Some(chat);
        }
        let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
        window.focus(&handle, cx);
        self.outline_focus_pending = true;
        cx.notify();
    }

    /// Toggle Outline Focus's return target (see `outline_chat_return`).
    pub(crate) fn outline_chat_return(&self) -> Option<&OutlineChatReturn> {
        self.outline_chat_return.as_ref()
    }

    /// Whether the Outline tree (or its inline editor) holds focus on screen.
    pub(crate) fn outline_tree_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.view_mode == OverviewMode::Outline
            && gpui::Focusable::focus_handle(self.outline.read(cx), cx).contains_focused(window, cx)
    }

    /// The Chat the panel shows right now, if it is open.
    pub(crate) fn shown_panel_chat(&self, cx: &gpui::App) -> Option<String> {
        panel_chat_id(
            self.chat_panel_open,
            self.state.read(cx).selected_chat.as_deref(),
        )
    }

    /// Toggle Outline Focus, tree → Chat panel: reopen the panel on
    /// `chat_id` the way Enter on its row does (Work Item Details close
    /// through the discard prompt first), which focuses the composer.
    pub(crate) fn return_to_outline_chat_panel(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.leave_work_item_details(DetailsExit::OpenChat(chat_id), cx);
    }

    /// The Chat Open Full Chat opens: the Outline's selected Chat row while
    /// the tree has focus, else the Chat the panel shows (the highlighted
    /// List row; its composer may have focus), else the Outline's
    /// selected Chat row. A panel hidden under Work Item Details or Required
    /// Actions does not count; neither does a Chat row whose Chat is gone.
    pub(crate) fn full_chat_target(&self, window: &Window, cx: &gpui::App) -> Option<String> {
        let selected_row = || {
            (self.view_mode == OverviewMode::Outline)
                .then(|| self.outline.read(cx).selected_row())
                .flatten()
                .and_then(|row| row.open_chat())
                .filter(|row| !row.dangling)
                .map(|row| row.chat.id.clone())
        };
        if self.outline_tree_focused(window, cx)
            && let Some(chat_id) = selected_row()
        {
            return Some(chat_id);
        }
        // The Linear panel has its own column; it never hides the chat panel.
        let panel_visible =
            self.work_item_details.is_none() && self.required_actions_panel.is_none();
        panel_visible
            .then(|| self.shown_panel_chat(cx))
            .flatten()
            .or_else(selected_row)
    }

    /// Preview-on-select: the user moved the Outline selection. On a Chat
    /// row, show that Chat in the chat panel once the selection has rested
    /// for [`OUTLINE_PREVIEW_DEBOUNCE`] (a newer move restarts the wait). On
    /// any other row, cancel a pending switch: the panel keeps the last
    /// previewed Chat.
    pub(super) fn schedule_outline_preview(&mut self, cx: &mut Context<Self>) {
        if self.outline.read(cx).selected_open_chat().is_none() {
            self.outline_preview = None;
            return;
        }
        self.outline_preview = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(OUTLINE_PREVIEW_DEBOUNCE)
                .await;
            this.update(cx, |this, cx| this.preview_outline_selection(cx))
                .ok();
        }));
    }

    /// Show the selected Outline Chat row in the chat panel WITHOUT moving
    /// focus (the tree keeps it, so arrows keep browsing). Viewing it the
    /// way the panel does: its unread done is acknowledged and it becomes
    /// the selected Chat, which is what the panel shows; it also becomes
    /// Toggle Outline Focus's return target. Never the full chat route. A
    /// slot held by Work Item Details, Required Actions or a subagent peek
    /// is left alone (they are never closed by browsing). The switch itself
    /// has no animation, so reduced motion needs nothing more here.
    pub(super) fn preview_outline_selection(&mut self, cx: &mut Context<Self>) {
        self.outline_preview = None;
        if self.view_mode != OverviewMode::Outline
            || self.work_item_details.is_some()
            || self.required_actions_panel.is_some()
            || self.subagent_peek.is_some()
        {
            return;
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if !shell.read(cx).is_overview_route() {
            return;
        }
        let Some(chat_id) = self.outline.read(cx).selected_open_chat() else {
            return;
        };
        if self.shown_panel_chat(cx).as_deref() == Some(chat_id.as_str()) {
            return;
        }
        self.acknowledge_done(&chat_id, cx);
        self.outline_chat_return = Some(OutlineChatReturn {
            chat_id: chat_id.clone(),
            full_route: false,
        });
        shell.update(cx, |shell, cx| {
            shell.show_chat_in_overview_panel(chat_id, cx)
        });
        self.chat_panel_open = true;
        cx.notify();
    }

    /// Tests: whether a preview-on-select is waiting out its debounce.
    #[cfg(test)]
    pub(crate) fn outline_preview_pending(&self) -> bool {
        self.outline_preview.is_some()
    }

    /// Tests: switch the view mode (after loading saved prefs, as the real
    /// toggle does).
    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&mut self, mode: OverviewMode, cx: &mut Context<Self>) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(mode, cx);
    }

    /// Tests: the search field's text.
    #[cfg(test)]
    pub(crate) fn search_text(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().to_string()
    }

    /// Tests: set the search text (emits an edit, as typing does).
    #[cfg(test)]
    pub(crate) fn set_search_text_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
    }

    /// Tests: set (or clear) the List selection.
    #[cfg(test)]
    pub(crate) fn set_list_selection_for_test(
        &mut self,
        id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.list_selected = id;
        cx.notify();
    }

    /// Tests: whether the List surface holds focus.
    #[cfg(test)]
    pub(crate) fn body_focused(&self, window: &Window) -> bool {
        self.body_focus.is_focused(window)
    }

    /// Tests: the keyboard-selected List row and the rendered row order.
    #[cfg(test)]
    pub(crate) fn list_selection_for_test(&self) -> (Option<String>, Vec<String>) {
        (
            self.list_selected.clone(),
            self.list_order.iter().map(|(id, _)| id.clone()).collect(),
        )
    }

    /// Tests: switch to List mode and open the chat panel on `chat_id` (the
    /// row's "View full chat →"), which highlights that row.
    #[cfg(test)]
    pub(crate) fn show_list_with_panel_for_test(
        &mut self,
        chat_id: String,
        cx: &mut Context<Self>,
    ) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(OverviewMode::List, cx);
        self.open_chat_panel(chat_id, cx);
    }

    /// Open Full Chat with nothing to open: a notice in the Outline (the
    /// List has no notice line; there the command does nothing).
    pub(crate) fn show_full_chat_notice(&mut self, cx: &mut Context<Self>) {
        if self.view_mode == OverviewMode::Outline {
            self.show_outline_notice("Select a Chat or open one beside the Outline first", cx);
        }
    }

    /// The Overview half of leaving for the full chat route (the panel's
    /// "Open full chat →"): close the panel and acknowledge the Chat's unread
    /// done, as opening it from a row does. The Shell then opens the route.
    pub(crate) fn prepare_full_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.close_chat_panel(cx);
        self.acknowledge_done(chat_id, cx);
    }

    /// Show a short notice in the Outline.
    pub(crate) fn show_outline_notice(&mut self, text: &'static str, cx: &mut Context<Self>) {
        self.outline
            .update(cx, |outline, cx| outline.show_notice(text, cx));
    }

    /// The item Open Effective Instructions targets: the selected work item,
    /// else the selected Chat row's item, else the app's selected Chat's item.
    pub(super) fn effective_instructions_target(
        &self,
        chat_hint: Option<&str>,
        cx: &gpui::App,
    ) -> Option<(String, String)> {
        let outline = self.outline.read(cx);
        if let Some(target) = outline.selected_item() {
            return Some(target);
        }
        if let Some(target) = outline
            .selected_row()
            .and_then(|row| row.chat())
            .and_then(|chat| chat.work_item.clone())
        {
            return Some(target);
        }
        let chat_id = chat_hint?;
        outline
            .model()
            .rows
            .iter()
            .filter_map(|row| row.chat())
            .find(|chat| chat.chat.id == chat_id)
            .and_then(|chat| chat.work_item.clone())
    }

    /// Start Chat in a work item (Outline row action, details button,
    /// palette). When the item's instructions changed since its last launch,
    /// Effective Instructions open in details first (the pre-launch check);
    /// the Shell's Start Chat dialog then shows them expanded too.
    pub(crate) fn start_chat_for_item(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot = self
            .outline_snapshot(&space_id, cx)
            .ok_or("This Space's outline is not loaded")?;
        let item = snapshot.item(&item_id).ok_or("That work item is gone")?;
        if item.archived_at.is_some() {
            return Err("Restore the work item to start a Chat in it");
        }
        // A PR review item (PR review sync) starts a code review of its PR,
        // filed under the item, instead of the generic Start Chat dialog.
        if crate::github::pr_review_item(&snapshot, &item_id).is_some() {
            return self.start_pr_review_for_item(
                space_id,
                item_id,
                mast_proto::PrReviewAction::CodeReview,
                cx,
            );
        }
        if crate::work_outline::launch::instructions_changed_since_last_launch(&snapshot, &item_id)
        {
            self.open_effective_instructions(space_id.clone(), item_id.clone(), cx);
        }
        #[cfg(test)]
        if self.shell.upgrade().is_none() {
            self.captured_start_chat = Some((space_id, item_id));
            return Ok(());
        }
        let shell = self
            .shell
            .upgrade()
            .ok_or("Starting a Chat needs the app window")?;
        // Deferred: the palette path runs inside a Shell update.
        cx.defer(move |cx| {
            shell.update(cx, |shell, cx| {
                shell.open_start_chat_or_notice(&space_id, &item_id, cx)
            });
        });
        Ok(())
    }

    /// Open Work Item Details for the item with Effective Instructions
    /// expanded (the explicit open path; Milestone 3's pre-launch check
    /// reuses it).
    pub(crate) fn open_effective_instructions(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(details) = self.work_item_details.as_ref().filter(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            details.update(cx, |details, cx| details.open_effective(cx));
            return;
        }
        self.pending_effective = Some((space_id.clone(), item_id.clone()));
        self.open_work_item_details(space_id, item_id, cx);
    }

    /// Open Work Item Details (`item_id` empty = Space root) with Proposed
    /// instructions expanded.
    pub(crate) fn open_proposals(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(details) = self.work_item_details.as_ref().filter(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            details.update(cx, |details, cx| details.open_proposals(cx));
            return;
        }
        self.pending_proposals = Some((space_id.clone(), item_id.clone()));
        self.open_work_item_details(space_id, item_id, cx);
    }

    /// Every loaded Space's snapshot, in Outline Space order (the global
    /// Required Actions view).
    pub(super) fn required_action_spaces(
        &self,
        cx: &gpui::App,
    ) -> Vec<crate::work_outline::required_actions::SpaceSnapshot> {
        self.outline_spaces(cx)
            .into_iter()
            .filter_map(|space| {
                Some(crate::work_outline::required_actions::SpaceSnapshot {
                    snapshot: space.load.snapshot()?.clone(),
                    space_id: space.id,
                    label: space.label,
                })
            })
            .collect()
    }

    /// Open the global Required Actions surface (palette command, shortcut,
    /// Outline toolbar). Work Item Details close first (through the discard
    /// policy) so the surface is visible.
    pub(crate) fn open_required_actions(&mut self, cx: &mut Context<Self>) {
        if self.required_actions_panel.is_none() {
            use crate::work_outline::required_actions_view::{
                RequiredActionsEvent, RequiredActionsList,
            };
            let shell = self.shell.clone();
            let list = cx.new(|cx| {
                let mut list = RequiredActionsList::new(
                    self.state.clone(),
                    crate::work_outline::required_actions::RequestScope::Global,
                    true,
                    cx,
                );
                list.shell = Some(shell);
                list
            });
            let events = cx.subscribe(&list, |this: &mut Self, _, event, cx| match event {
                RequiredActionsEvent::Close => this.close_required_actions(cx),
                RequiredActionsEvent::OpenChat(chat_id) => {
                    this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
                }
                RequiredActionsEvent::OpenItem { space_id, item_id } => {
                    this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
                }
            });
            self.required_actions_panel = Some(list);
            self._required_actions_events = Some(events);
        }
        if self.work_item_details.is_some() {
            self.leave_work_item_details(DetailsExit::Close, cx);
        }
        cx.notify();
    }

    pub(crate) fn open_required_action(&mut self, request_id: String, cx: &mut Context<Self>) {
        self.open_required_actions(cx);
        if let Some(panel) = &self.required_actions_panel {
            panel.update(cx, |panel, cx| panel.select_request(request_id, cx));
        }
    }

    pub(crate) fn decide_required_action(
        &mut self,
        request_id: String,
        decision: crate::menu_bar::RequestDecision,
        via: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.open_required_actions(cx);
        let spaces = self.required_action_spaces(cx);
        let chats = self.outline_chats.clone();
        if let Some(panel) = &self.required_actions_panel {
            panel.update(cx, |panel, cx| {
                panel.set_data(spaces, chats);
                panel.select_request(request_id.clone(), cx);
                panel.external_decision(request_id, decision, via, cx);
            });
        }
    }

    pub(crate) fn close_required_actions(&mut self, cx: &mut Context<Self>) {
        if self.required_actions_panel.take().is_some() {
            self._required_actions_events = None;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn required_actions_panel(
        &self,
    ) -> Option<Entity<crate::work_outline::required_actions_view::RequiredActionsList>> {
        self.required_actions_panel.clone()
    }

    /// Replace the per-Space outline loads (tests and the offline fixture;
    /// the app reads `AppState::work_outline`).
    pub(crate) fn set_outline_loads(
        &mut self,
        loads: HashMap<String, WorkOutlineLoad>,
        cx: &mut Context<Self>,
    ) {
        self.outline_fixture = Some(loads);
        self.rows_dirty = true;
        self.outline_dirty = true;
        cx.notify();
    }

    /// Offline fixture hook (`examples/outline-fixture.rs`): every Space gets
    /// the store fixture (ids suffixed per Space after the first), and the
    /// Overview switches to Outline.
    #[doc(hidden)]
    pub fn show_work_outline_fixture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut ids: Vec<(String, String)> = self
            .state
            .read(cx)
            .spaces
            .iter()
            .map(|space| (space.display_name().to_lowercase(), space.id.clone()))
            .collect();
        ids.sort();
        let loads = ids
            .into_iter()
            .enumerate()
            .map(|(ix, (_, id))| {
                let snapshot = fixture_snapshot_for(&id, ix);
                (id, WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)))
            })
            .collect();
        self.set_outline_loads(loads, cx);
        self.set_mode(OverviewMode::Outline, cx);
        let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
        window.focus(&handle, cx);
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn show_snapshot_outline(&mut self, cx: &mut Context<Self>) {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("fixture-space");
        snapshot.external_references.clear();
        self.set_outline_loads(
            [(
                "fixture-space".into(),
                WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
            )]
            .into_iter()
            .collect(),
            cx,
        );
        self.set_mode(OverviewMode::Outline, cx);
        self.sync_outline(cx);
        self.outline.update(cx, |outline, cx| outline.select(&row_key::item("fixture-space", "w-api"), cx));
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn show_snapshot_linear(&mut self, cx: &mut Context<Self>) {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("fixture-space");
        snapshot.external_references.clear();
        snapshot.space_settings.linear_enabled = true;
        self.set_outline_loads([("fixture-space".into(), WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)))].into_iter().collect(), cx);
        self.set_mode(OverviewMode::Outline, cx);
        self.sync_outline(cx);
        self.open_snapshot_linear(cx);
    }

    /// Outline navigation bindings for fixtures that run without a Shell.
    #[doc(hidden)]
    pub fn work_outline_key_bindings() -> Vec<gpui::KeyBinding> {
        crate::work_outline::outline_view::key_bindings()
    }
}

