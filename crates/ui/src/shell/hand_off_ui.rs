//! Continue work in a fresh chat.
use super::*;
use super::work_outline_ui::WorkOutlineModal;
use crate::work_outline::actions::OperationSlot;
use crate::pickers::Pickers;

pub(super) struct HandOffDialog {
    chat_id: String,
    input: Entity<ComposerInput>,
    default_summary: String,
    pickers: Entity<Pickers>,
    _picker_observe: Subscription,
    workers: Vec<String>,
    details_available: bool,
    archive: bool,
    archive_locked: bool,
    submitting: bool,
    error: Option<String>,
    operation: OperationSlot,
}

impl HandOffDialog {
    pub(super) fn is_submitting(&self) -> bool { self.submitting }
    pub(super) fn input_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Shell {
    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_snapshot_hand_off(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, _| {
            let solo = std::env::var("MAST_PREVIEW_HANDOFF_SOLO").is_ok_and(|value| value == "1");
            for chat in &mut state.chats {
                if solo && chat.parent_chat_id.as_deref() == Some("fixture-chat") {
                    chat.parent_chat_id = None;
                } else if !solo && chat.id == "fixture-design" {
                    chat.parent_chat_id = Some("fixture-chat".into());
                }
            }
        });
        self.open_hand_off("fixture-chat".into(), cx);
    }

    pub(super) fn hand_off_pickers(&self) -> Option<Entity<Pickers>> {
        match self.work_outline_modal.as_ref() {
            Some(WorkOutlineModal::HandOff(dialog)) => Some(dialog.pickers.clone()),
            _ => None,
        }
    }

    pub(super) fn can_hand_off(&self, chat_id: &str, cx: &App) -> bool {
        self.state.read(cx).chats.iter().any(|chat| {
            chat.id == chat_id && !chat.archived && !chat.is_standing_role_predecessor()
        })
    }

    pub(super) fn open_hand_off(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id && !chat.archived && !chat.is_standing_role_predecessor()) else { return };
        let load = chat.space_id.as_deref().map(|space_id| state.work_outline(space_id));
        let snapshot = load.as_ref().and_then(|load| load.snapshot());
        let item = snapshot.and_then(|snapshot| crate::work_outline::instructions::assigned_chats(snapshot).remove(&chat_id)
            .and_then(|id| snapshot.item(&id)));
        let summary = hand_off_summary(item.map(|item| item.title.as_str()).or(chat.title.as_deref()));
        let details_available = item.is_some();
        let config = chat.config.clone();
        let device = chat.space_id.as_deref().and_then(|id| state.space_row(id)).map(|space| space.device_id.clone());
        let workers: Vec<String> = state.chats.iter().filter(|worker| !worker.archived && worker.parent_chat_id.as_deref() == Some(chat_id.as_str())).map(|worker| {
            let status = state.sessions.iter().find(|session| session.chat_id == worker.id)
                .map(|session| match session.status {
                    mast_proto::SessionStatus::Idle => "Idle",
                    mast_proto::SessionStatus::Working => "Working",
                    mast_proto::SessionStatus::AwaitingInput => "Awaiting input",
                    mast_proto::SessionStatus::Errored => "Errored",
                }).unwrap_or("Unknown");
            let requests = snapshot.map(|snapshot| snapshot.user_action_requests.iter().filter(|request| request.chat_id == worker.id && request.is_unresolved() && !request.deduped).count());
            let requests = requests.map(|count| format!("{count} open requests")).unwrap_or_else(|| "Request count unavailable".into());
            format!("{} · {} · {}", worker.title.as_deref().unwrap_or("Untitled"), status, requests)
        }).collect();
        let composer_pickers = self.composer.read(cx).pickers().clone();
        let pickers = cx.new(|cx| {
            let mut pickers = Pickers::new_chat_launcher(&composer_pickers, device, cx);
            pickers.seed_hand_off(config.as_ref(), cx);
            pickers
        });
        let picker_observe = cx.observe(&pickers, |_, _, cx| cx.notify());
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new("Summary", cx).with_text_metrics(12.5, 18.0).with_auto_grow();
            input.set_text(summary.clone(), cx);
            input
        });
        if !details_available {
            // Selecting the source makes its Space's outline watch active.
            self.open_chat(chat_id.clone(), cx);
        }
        self.begin_work_outline_modal(WorkOutlineModal::HandOff(HandOffDialog {
            chat_id, input, default_summary: summary, pickers, _picker_observe: picker_observe, workers, details_available, archive: true, archive_locked: false, submitting: false,
            error: None,
            operation: OperationSlot::default(),
        }), cx);
    }

    pub(super) fn refresh_hand_off_details(&mut self, cx: &mut Context<Self>) {
        let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_ref() else { return };
        if dialog.details_available { return; }
        let chat_id = dialog.chat_id.clone();
        let state = self.state.read(cx);
        let Some(space_id) = state.chats.iter().find(|chat| chat.id == chat_id).and_then(|chat| chat.space_id.as_deref()) else { return };
        let load = state.work_outline(space_id);
        let Some(snapshot) = load.snapshot() else { return };
        let Some(item) = crate::work_outline::instructions::assigned_chats(snapshot).remove(&chat_id).and_then(|id| snapshot.item(&id)) else { return };
        let summary = hand_off_summary(Some(&item.title));
        let workers = state.chats.iter().filter(|worker| !worker.archived && worker.parent_chat_id.as_deref() == Some(chat_id.as_str())).map(|worker| {
            let status = state.sessions.iter().find(|session| session.chat_id == worker.id).map(|session| match session.status {
                mast_proto::SessionStatus::Idle => "Idle", mast_proto::SessionStatus::Working => "Working",
                mast_proto::SessionStatus::AwaitingInput => "Awaiting input", mast_proto::SessionStatus::Errored => "Errored",
            }).unwrap_or("Unknown");
            let requests = snapshot.user_action_requests.iter().filter(|request| request.chat_id == worker.id && request.is_unresolved() && !request.deduped).count();
            format!("{} · {} · {} open requests", worker.title.as_deref().unwrap_or("Untitled"), status, requests)
        }).collect();
        let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_mut() else { return };
        dialog.workers = workers;
        dialog.details_available = true;
        dialog.input.update(cx, |input, cx| {
            if input.text() == dialog.default_summary {
                input.set_text(summary.clone(), cx);
            }
        });
        dialog.default_summary = summary;
    }

    pub(super) fn note_local_pin_handoff_edit(&mut self, chat_id: &str) -> bool {
        for intent in &mut self.settings.local_pin_handoffs {
            if intent.cancelled {
                continue;
            }
            if intent.source == chat_id || intent.requested_successor == chat_id {
                intent.cancelled = true;
            } else if !intent.edited_chats.iter().any(|id| id == chat_id) {
                // A resumed hand-off can return a different successor. Save
                // edits until the completion identifies which chat it used.
                intent.edited_chats.push(chat_id.to_owned());
            }
        }
        // Save the pin order even if this edit repeats a cancellation.
        // Otherwise a restart can keep the earlier pin but lose the unpin.
        !self.settings.local_pin_handoffs.is_empty()
    }

    fn record_local_pin_handoff(
        &mut self,
        profile: &str,
        source: &str,
        requested: &str,
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        let intent = settings::LocalPinHandoff {
            source: source.to_owned(),
            requested_successor: requested.to_owned(),
            cancelled: false,
            edited_chats: Vec::new(),
        };
        if self.settings.local_pin_handoffs.iter().any(|item| item.requested_successor == requested) {
            return settings::flush_checked(cx);
        }
        if !self.settings.sidebar_pins(profile).iter().any(|id| id == source)
            || self.state.read(cx).sidebar_preferences.local_handoffs.iter().any(|entry| intent.matches(entry))
        {
            return Ok(());
        }
        self.settings.local_pin_handoffs.push(intent);
        self.schedule_save(cx);
        settings::flush_checked(cx)
    }

    pub(super) fn reconcile_local_pin_handoffs(&mut self, profile: &str, cx: &mut Context<Self>) {
        let completions = self
            .state
            .read(cx)
            .sidebar_preferences
            .local_handoffs
            .clone();
        for completion in completions {
            if completion.automatic
                && !self
                    .settings
                    .local_pin_handoffs
                    .iter()
                    .any(|intent| intent.matches(&completion))
            {
                self.transfer_local_pin(profile, &completion, cx);
            } else if !self.settings.local_pin_handoffs.is_empty() {
                self.complete_local_pin_handoff(profile, &completion, cx);
            }
        }
    }

    /// An agent or automatic hand-off recorded no UI intent. The pin follows
    /// the role to the successor unless the successor is already pinned.
    fn transfer_local_pin(
        &mut self,
        profile: &str,
        completion: &mast_proto::CompletedSidebarHandoff,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{}>{}", completion.source, completion.successor);
        if self.settings.applied_automatic_handoffs.contains(&key) {
            return;
        }
        // Applied or not, this completion is consumed: a later pin edit wins.
        self.settings.applied_automatic_handoffs.push(key);
        let excess = self.settings.applied_automatic_handoffs.len().saturating_sub(200);
        self.settings.applied_automatic_handoffs.drain(..excess);
        self.schedule_save(cx);
        let Some(pins) = self
            .settings
            .sidebar_pinned_session_ids_by_profile
            .get_mut(profile)
        else {
            return;
        };
        if pins.contains(&completion.successor) {
            return;
        }
        let Some(pin) = pins.iter_mut().find(|pin| **pin == completion.source) else {
            return;
        };
        *pin = completion.successor.clone();
        if !self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == completion.successor)
        {
            self.unobserved_local_handoff_pins
                .insert(completion.successor.clone());
        }
    }

    fn complete_local_pin_handoff(
        &mut self,
        profile: &str,
        completion: &mast_proto::CompletedSidebarHandoff,
        cx: &mut Context<Self>,
    ) {
        let mut matching = self
            .settings
            .local_pin_handoffs
            .iter()
            .filter(|intent| intent.matches(completion));
        let Some(first) = matching.next() else {
            return;
        };
        let unchanged = |intent: &settings::LocalPinHandoff| {
            !intent.cancelled && !intent.edited_chats.contains(&completion.successor)
        };
        let transfer = unchanged(first) && matching.all(unchanged);
        self.settings
            .local_pin_handoffs
            .retain(|intent| !intent.matches(completion));
        if transfer
            && let Some(pins) = self
                .settings
                .sidebar_pinned_session_ids_by_profile
                .get_mut(profile)
            && !pins.contains(&completion.successor)
            && let Some(pin) = pins.iter_mut().find(|pin| **pin == completion.source)
        {
            *pin = completion.successor.clone();
            // A completion watch or RPC may reach the shell before the chat.
            if !self
                .state
                .read(cx)
                .chats
                .iter()
                .any(|chat| chat.id == completion.successor)
            {
                self.unobserved_local_handoff_pins
                    .insert(completion.successor.clone());
            }
        }
        // Persist consumption with the pin update. A crash can replay the
        // old pair of values or load the new pair, which is already applied.
        self.schedule_save(cx);
        settings::flush(cx);
    }

    fn submit_hand_off(&mut self, cx: &mut Context<Self>) {
        let actions = self.state.read(cx).work_outline_actions();
        let local_pin_profile = (self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local))
            .then(|| self.active_sidebar_pin_profile_key(cx))
            .flatten();
        let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_mut() else { return };
        if dialog.submitting { return; }
        let summary = dialog.input.read(cx).text().trim().to_owned();
        if summary.is_empty() { dialog.error = Some("Enter a summary".into()); cx.notify(); return; }
        let source = dialog.chat_id.clone();
        let choice = dialog.pickers.read(cx).resolved(cx);
        let harness = choice.harness;
        let model = choice.model;
        let reasoning = choice.reasoning;
        let archive = dialog.archive;
        let operation_id = dialog.operation.id_for(&format!(
            "{summary}|{harness:?}|{model:?}|{reasoning:?}|{archive}"
        ));
        let requested = mast_proto::derive_id(&operation_id, mast_proto::roles::CHAT);
        if let Some(profile) = local_pin_profile.as_deref()
            && let Err(err) = self.record_local_pin_handoff(profile, &source, &requested, cx)
        {
            if let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_mut() {
                dialog.error = Some(format!("Could not save the hand-off pin: {err}"));
            }
            cx.notify();
            return;
        }
        let task = actions.hand_off_orchestrator(mast_proto::HandOffOrchestratorParams {
            chat_id: source.clone(),
            summary,
            operation_id,
            archive: Some(archive),
            harness,
            model,
            reasoning,
        });
        let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_mut() else {
            return;
        };
        dialog.archive_locked = true;
        dialog.submitting = true;
        dialog.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(WorkOutlineModal::HandOff(dialog)) = this.work_outline_modal.as_mut() else { return };
                if dialog.chat_id != source { return; }
                dialog.submitting = false;
                match result {
                    Ok(reply) if reply.state != mast_proto::WorkItemLaunchState::Rejected => {
                        this.work_outline_modal = None;
                        this.work_outline_modal_focus_pending = false;
                        this.work_outline_prior_focus = None;
                        if let Some(profile) = local_pin_profile.as_deref() {
                            this.complete_local_pin_handoff(
                                profile,
                                &mast_proto::CompletedSidebarHandoff {
                                    source: source.clone(),
                                    successor: reply.chat_id.clone(),
                                    retry_chat_ids: vec![requested],
                                    automatic: false,
                                },
                                cx,
                            );
                        }
                        this.state.update(cx, |state, cx| {
                            state.refresh_admin_grants(cx);
                            state.refresh_personal_assistant(cx);
                        });
                        this.open_chat(reply.chat_id, cx);
                    }
                    Ok(reply) => { dialog.error = Some(reply.detail.unwrap_or_else(|| "The host rejected the first turn".into())); cx.notify(); }
                    Err(error) => { dialog.error = Some(error.message); cx.notify(); }
                }
            }).ok();
        }).detach();
    }
}

fn hand_off_summary(title: Option<&str>) -> String {
    format!("Continue {}. Read the source chat and work item for context, instructions, and any active workers.", title.unwrap_or("this work"))
}

pub(super) fn hand_off_card(dialog: &HandOffDialog, theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    let mut workers = div().flex().flex_col().gap(px(4.0));
    for worker in &dialog.workers { workers = workers.child(div().child(SharedString::from(worker.clone()))); }
    popover::dialog_card(theme)
        .debug_selector(|| "hand-off-dialog".into())
        .child(popover::dialog_title(theme, "Hand off to new chat"))
        .child(div().mt(px(12.0)).child(popover::dialog_field(theme, dialog.input.clone().into_any_element())))
        .child(div().mt(px(12.0)).child(dialog.pickers.clone()))
        .when(!dialog.workers.is_empty(), |card| card
            .child(div().mt(px(12.0)).child(SharedString::from("Workers")))
            .child(div().id("hand-off-workers").max_h(px(180.0)).overflow_y_scroll().text_color(theme.text_muted).child(workers)))
        .child(div().id("hand-off-archive").debug_selector(|| "hand-off-archive".into()).mt(px(12.0)).when(!dialog.archive_locked, |row| row.cursor_pointer()).on_click(cx.listener(|this, _, _, cx| {
            if let Some(WorkOutlineModal::HandOff(dialog)) = this.work_outline_modal.as_mut()
                && !dialog.archive_locked
            {
                dialog.archive = !dialog.archive;
                cx.notify();
            }
        })).child(SharedString::from(if dialog.archive { "☑ Archive old chat" } else { "☐ Archive old chat" })))
        .when(dialog.archive_locked && !dialog.submitting && dialog.error.is_some(), |card| {
            card.child(div().mt(px(4.0)).text_color(theme.text_muted).child("Retry uses the original archive choice."))
        })
        .when_some(dialog.error.clone(), |card, error| card.child(div().mt(px(8.0)).text_color(theme.warning).child(SharedString::from(error))))
        .child(div().mt(px(16.0)).flex().justify_end().gap(px(8.0))
            .child(popover::btn_ghost(theme, "Cancel", "hand-off-cancel").id("hand-off-cancel").on_click(cx.listener(|this, _, window, cx| { this.cancel_work_outline_modal(window, cx); })))
            .child(popover::btn_primary(theme, if dialog.submitting { "Handing off…" } else { "Hand off" }).id("hand-off-submit").on_click(cx.listener(|this, _, _, cx| this.submit_hand_off(cx)))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::{CHAT_API, CHAT_UI};
    use std::sync::{Arc, Mutex};

    #[gpui::test]
    fn solo_chat_can_hand_off_without_loaded_item_details(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(
            move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params));
                Err(mast_rpc::RpcError::Failed("host unavailable".into()))
            },
        ));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.set_work_outline_for_test(mast_proto::WorkOutlineSnapshot::unavailable(
                    "s1", false, 0,
                ));
                state.work_outline_actions_override = Some(actions);
            });
            assert!(shell.can_hand_off(CHAT_API, cx));
            shell.open_hand_off(CHAT_API.into(), cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else {
                panic!()
            };
            assert!(dialog.workers.is_empty());
            assert!(!dialog.details_available);
            assert!(dialog.error.is_none());
            shell.submit_hand_off(cx);
        });
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 1);
        shell.update(cx, |shell, cx| {
            shell.work_outline_modal = None;
            shell.state.update(cx, |state, _| {
                state
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == CHAT_API)
                    .unwrap()
                    .archived = true;
            });
            assert!(!shell.can_hand_off(CHAT_API, cx));
            shell.open_hand_off(CHAT_API.into(), cx);
            assert!(shell.work_outline_modal.is_none());
        });
    }

    #[gpui::test]
    fn dialog_lists_live_workers_and_prefills_summary(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            assert!(shell.can_hand_off(CHAT_API, cx));
            shell.state.update(cx, |state, _| {
                let worker = state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap();
                worker.parent_chat_id = Some(CHAT_API.into());
            });
            assert!(shell.can_hand_off(CHAT_API, cx));
            shell.open_hand_off(CHAT_API.into(), cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else { panic!("hand-off dialog not opened") };
            assert!(dialog.input.read(cx).text().starts_with("Continue "));
            assert!(dialog.input.read(cx).text().ends_with("Read the source chat and work item for context, instructions, and any active workers."));
            assert_eq!(dialog.workers.len(), 1);
            assert!(dialog.workers[0].contains("Title of"));
            assert!(dialog.workers[0].contains("0 open requests"));
            assert!(dialog.archive);
        });
    }

    #[gpui::test]
    fn dialog_refreshes_after_outline_arrives(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap().parent_chat_id = Some(CHAT_API.into());
                state.set_work_outline_for_test(mast_proto::WorkOutlineSnapshot::unavailable("s1", false, 0));
            });
            shell.open_hand_off(CHAT_API.into(), cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else { panic!() };
            assert!(!dialog.details_available);
            shell.state.update(cx, |state, _| state.set_work_outline_for_test(crate::work_outline::store::fixture_snapshot("s1")));
            shell.refresh_hand_off_details(cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else { panic!() };
            assert!(dialog.details_available);
            assert!(dialog.error.is_none());
            assert!(dialog.workers[0].contains("0 open requests"));
        });
    }

    #[gpui::test]
    fn hand_off_submit_sends_summary_and_default_archive_choice(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(move |method, params| {
            recorded.lock().unwrap().push((method.to_owned(), params));
            Ok(serde_json::json!({"chatId":"successor", "messageId":"m", "commandId":"c", "executionRoot":"/work", "state":"queued"}))
        }));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap().parent_chat_id = Some(CHAT_API.into());
                state.chats.iter_mut().find(|chat| chat.id == CHAT_API).unwrap().config = Some(mast_proto::ChatConfig { approval_rules: false,
                    harness: mast_proto::HarnessId::Codex, model: Some("source-model".into()), reasoning: None,
                    model_options: Default::default(), sandbox: mast_proto::SandboxLevel::WorkspaceWrite, auto_approve: false,
                });
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.work_outline_actions_override = Some(actions);
            });
            let profile = shell.active_sidebar_pin_profile_key(cx).unwrap();
            *shell.settings.sidebar_pins_mut(profile) = vec![CHAT_API.into(), CHAT_UI.into()];
            shell.open_hand_off(CHAT_API.into(), cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = shell.work_outline_modal.as_mut() else { panic!() };
            dialog.pickers.update(cx, |pickers, cx| {
                let selected = pickers.resolved(cx);
                assert_eq!(selected.harness, Some(mast_proto::HarnessId::Codex));
                assert_eq!(selected.model.as_deref(), Some("source-model"));
                pickers.pick_harness(mast_proto::HarnessId::ClaudeCode, cx);
                pickers.pick_model_for_tests("claude-opus-5-5", cx);
            });
            dialog.input.update(cx, |input, cx| input.set_text("Continue coordinating", cx));
            shell.submit_hand_off(cx);
        });
        cx.run_until_parked();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, mast_rpc::methods::HAND_OFF_ORCHESTRATOR);
        assert_eq!(calls[0].1["chatId"], CHAT_API);
        assert_eq!(calls[0].1["summary"], "Continue coordinating");
        assert_eq!(calls[0].1["archive"], true);
        assert_eq!(calls[0].1["harness"], "claude-code");
        assert_eq!(calls[0].1["model"], "claude-opus-5-5");
        assert!(calls[0].1["operationId"].as_str().is_some());
        shell.update(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            // A success before the watch sees the successor must survive pin cleanup.
            shell.state.update(cx, |state, _| state.chats_synced = true);
            shell.reconcile_sidebar_pins(cx);
            let profile = shell.active_sidebar_pin_profile_key(cx).unwrap();
            assert_eq!(
                shell.settings.sidebar_pins(&profile),
                ["successor", CHAT_UI]
            );
            assert!(
                !shell
                    .mutations
                    .iter()
                    .any(|mutation| mutation["op"] == "setChatArchived")
            );
        });
    }

    #[gpui::test]
    fn hand_off_sends_unchecked_archive_choice(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(
            move |_, params| {
                recorded.lock().unwrap().push(params);
                Ok(
                    serde_json::json!({"chatId":"successor", "messageId":"m", "commandId":"c",
                    "executionRoot":"/work", "state":"queued"}),
                )
            },
        ));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.work_outline_actions_override = Some(actions)
            });
            shell.open_hand_off(CHAT_API.into(), cx);
        });
        super::super::chat_item_tests::draw(cx);
        let archive = cx.debug_bounds("hand-off-archive").unwrap().center();
        cx.simulate_click(archive, gpui::Modifiers::default());
        shell.update(cx, |shell, cx| shell.submit_hand_off(cx));
        cx.run_until_parked();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["archive"], false);
    }

    #[gpui::test]
    fn hand_off_retry_keeps_the_archive_choice_after_a_lost_reply(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(
            move |_, params| {
                let mut recorded = recorded.lock().unwrap();
                recorded.push(params);
                if recorded.len() <= crate::work_outline::actions::MAX_ATTEMPTS as usize {
                    Err(mast_rpc::RpcError::Transport("reply lost".into()))
                } else {
                    Ok(
                        serde_json::json!({"chatId":"successor", "messageId":"m", "commandId":"c",
                    "executionRoot":"/work", "state":"queued"}),
                    )
                }
            },
        ));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.work_outline_actions_override = Some(actions)
            });
            shell.open_hand_off(CHAT_API.into(), cx);
            shell.submit_hand_off(cx);
        });
        cx.run_until_parked();
        super::super::chat_item_tests::draw(cx);
        let archive = cx.debug_bounds("hand-off-archive").unwrap().center();
        cx.simulate_click(archive, gpui::Modifiers::default());
        shell.update(cx, |shell, cx| shell.submit_hand_off(cx));
        cx.run_until_parked();
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            crate::work_outline::actions::MAX_ATTEMPTS as usize + 1
        );
        assert_eq!(calls[0]["archive"], true);
        assert!(calls.iter().all(|request| request == &calls[0]));
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }

    #[gpui::test]
    fn agent_handoff_moves_the_local_pin_once(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.sidebar_preferences.local_handoffs =
                    vec![mast_proto::CompletedSidebarHandoff {
                        source: CHAT_API.into(),
                        successor: CHAT_UI.into(),
                        retry_chat_ids: Vec::new(),
                        automatic: true,
                    }];
            });
            *shell.settings.sidebar_pins_mut("local".into()) = vec![CHAT_API.into()];
            shell.reconcile_sidebar_pins(cx);
            assert_eq!(shell.settings.sidebar_pins("local"), [CHAT_UI]);
            // The user re-pins the old chat and unpins the new one: the
            // replayed completion stays inert.
            *shell.settings.sidebar_pins_mut("local".into()) = vec![CHAT_API.into()];
            shell.reconcile_sidebar_pins(cx);
            assert_eq!(shell.settings.sidebar_pins("local"), [CHAT_API]);
        });
    }

    #[gpui::test]
    fn local_handoff_pin_recovers_from_disk_without_the_dialog_callback(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::store::fixture_ids::CHAT_LOOSE_A;
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let actions =
            crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(|_, _| {
                Err(mast_rpc::RpcError::Transport("reply lost".into()))
            }));
        let (boot, chats) = shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.work_outline_actions_override = Some(actions);
            });
            *shell.settings.sidebar_pins_mut("local".into()) =
                vec![CHAT_LOOSE_A.into(), CHAT_API.into(), CHAT_UI.into()];
            shell.open_hand_off(CHAT_API.into(), cx);
            shell.submit_hand_off(cx);
            (shell.boot.clone(), shell.state.read(cx).chats.clone())
        });
        cx.run_until_parked();
        let saved = settings::UiSettings::load(&boot.data_dir);
        assert_eq!(saved.local_pin_handoffs.len(), 1);
        let intent = saved.local_pin_handoffs[0].clone();
        let completion = mast_proto::CompletedSidebarHandoff {
            source: CHAT_API.into(),
            successor: "successor".into(),
            retry_chat_ids: vec![intent.requested_successor],
            automatic: false,
        };
        cx.update(|_, cx| {
            settings::init(saved, boot.data_dir.as_path(), cx);
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.chats = chats;
                state.sidebar_preferences.local_handoffs = vec![completion];
            });
            let restarted = cx.new(|cx| Shell::new(state, boot.clone(), cx));
            restarted.update(cx, |shell, cx| {
                assert!(shell.work_outline_modal.is_none());
                shell.reconcile_sidebar_pins(cx);
                assert_eq!(
                    shell.settings.sidebar_pins("local"),
                    [CHAT_LOOSE_A, "successor", CHAT_UI]
                );
                // A user can unpin the successor and pin the original chat
                // after observing the transfer. Replaying the watch is inert.
                *shell.settings.sidebar_pins_mut("local".into()) =
                    vec![CHAT_API.into(), CHAT_UI.into()];
                shell.schedule_save(cx);
                settings::flush(cx);
                shell.reconcile_sidebar_pins(cx);
                let saved_again = settings::UiSettings::load(&boot.data_dir);
                assert_eq!(saved_again.sidebar_pins("local"), [CHAT_API, CHAT_UI]);
                assert!(saved_again.local_pin_handoffs.is_empty());
            });
        });
    }

    #[gpui::test]
    fn local_handoff_pin_preserves_successor_unpin_across_settings_reload(
        cx: &mut gpui::TestAppContext,
    ) {
        use mast_proto::SidebarPinChange;

        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local)
            });
            for requested in [CHAT_UI, "fresh-request"] {
                shell.state.update(cx, |state, _| {
                    state.sidebar_preferences.local_handoffs.clear()
                });
                *shell.settings.sidebar_pins_mut("local".into()) = vec![CHAT_API.into()];
                shell
                    .record_local_pin_handoff("local", CHAT_API, requested, cx)
                    .unwrap();
                assert!(shell.apply_sidebar_pin_change(
                    "local".into(),
                    SidebarPinChange::Pin {
                        session_id: CHAT_UI.into(),
                        after: None,
                        before: None,
                    },
                    cx
                ));
                assert!(shell.apply_sidebar_pin_change(
                    "local".into(),
                    SidebarPinChange::Unpin {
                        session_id: CHAT_UI.into(),
                    },
                    cx
                ));

                let data_dir = shell.boot.data_dir.clone();
                shell.settings = settings::UiSettings::load(&data_dir);
                assert_eq!(shell.settings.local_pin_handoffs.len(), 1);
                assert_eq!(shell.settings.sidebar_pins("local"), [CHAT_API]);
                shell.state.update(cx, |state, _| {
                    state.sidebar_preferences.local_handoffs =
                        vec![mast_proto::CompletedSidebarHandoff {
                            source: CHAT_API.into(),
                            successor: CHAT_UI.into(),
                            retry_chat_ids: if requested == CHAT_UI {
                                Vec::new()
                            } else {
                                vec![requested.into()]
                            },
                            automatic: false,
                        }];
                });
                shell.reconcile_sidebar_pins(cx);
                shell.reconcile_sidebar_pins(cx);
                let saved = settings::UiSettings::load(&data_dir);
                assert_eq!(
                    saved.sidebar_pins("local"),
                    [CHAT_API],
                    "pin edit survived completion of {requested}"
                );
                assert!(saved.local_pin_handoffs.is_empty());
            }
        });
    }

    #[gpui::test]
    fn submit_error_keeps_dialog_open(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        let actions = crate::work_outline::actions::WorkOutlineActions::scripted(Arc::new(|_, _| Err(mast_rpc::RpcError::Failed("transfer failed".into()))));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap().parent_chat_id = Some(CHAT_API.into());
                state.work_outline_actions_override = Some(actions);
            });
            shell.open_hand_off(CHAT_API.into(), cx);
            shell.submit_hand_off(cx);
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else { panic!() };
            assert!(dialog.error.as_deref().unwrap().contains("transfer failed"));
            assert!(!dialog.submitting);
        });
    }
}
