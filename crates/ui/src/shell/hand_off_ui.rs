//! Coordinator hand-off dialog.
use super::*;
use super::work_outline_ui::WorkOutlineModal;
use crate::work_outline::actions::OperationSlot;

pub(super) struct HandOffDialog {
    chat_id: String,
    input: Entity<ComposerInput>,
    workers: Vec<String>,
    archive: bool,
    submitting: bool,
    error: Option<String>,
    operation: OperationSlot,
}

impl HandOffDialog {
    pub(super) fn input_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Shell {
    pub(super) fn has_hand_off_workers(&self, chat_id: &str, cx: &App) -> bool {
        self.state.read(cx).chats.iter().any(|chat| !chat.archived && chat.parent_chat_id.as_deref() == Some(chat_id))
    }

    pub(super) fn open_hand_off(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else { return };
        let Some(space_id) = chat.space_id.as_deref() else { return };
        let load = state.work_outline(space_id);
        let Some(snapshot) = load.snapshot() else { return };
        let item = crate::work_outline::instructions::assigned_chats(snapshot).remove(&chat_id)
            .and_then(|id| snapshot.item(&id));
        let Some(item) = item else { return };
        let summary = format!("Continue as coordinator for {}. Read the work item, its instructions, and the worker list below.", item.title);
        let workers: Vec<String> = state.chats.iter().filter(|worker| !worker.archived && worker.parent_chat_id.as_deref() == Some(chat_id.as_str())).map(|worker| {
            let status = state.sessions.iter().find(|session| session.chat_id == worker.id)
                .map(|session| format!("{:?}", session.status)).unwrap_or_else(|| "Unknown".into());
            let requests = snapshot.user_action_requests.iter().filter(|request| request.chat_id == worker.id && request.is_unresolved() && !request.deduped).count();
            format!("{} · {} · {} open requests", worker.title.as_deref().unwrap_or("Untitled"), status, requests)
        }).collect();
        if workers.is_empty() { return; }
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new("Summary", cx).with_text_metrics(12.5, 18.0).with_auto_grow();
            input.set_text(summary, cx);
            input
        });
        self.begin_work_outline_modal(WorkOutlineModal::HandOff(HandOffDialog {
            chat_id, input, workers, archive: false, submitting: false, error: None, operation: OperationSlot::default(),
        }), cx);
    }

    fn submit_hand_off(&mut self, cx: &mut Context<Self>) {
        let actions = self.state.read(cx).work_outline_actions();
        let Some(WorkOutlineModal::HandOff(dialog)) = self.work_outline_modal.as_mut() else { return };
        if dialog.submitting { return; }
        let summary = dialog.input.read(cx).text().trim().to_owned();
        if summary.is_empty() { dialog.error = Some("Enter a summary".into()); cx.notify(); return; }
        let source = dialog.chat_id.clone();
        let operation_id = dialog.operation.id_for(&summary);
        let task = actions.hand_off_orchestrator(mast_proto::HandOffOrchestratorParams { chat_id: source.clone(), summary, operation_id });
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
                        let archive = dialog.archive;
                        this.work_outline_modal = None;
                        this.work_outline_modal_focus_pending = false;
                        this.work_outline_prior_focus = None;
                        if archive { this.archive_chat(source, cx); }
                        this.open_chat(reply.chat_id, cx);
                    }
                    Ok(reply) => { dialog.error = Some(reply.detail.unwrap_or_else(|| "The host rejected the first turn".into())); cx.notify(); }
                    Err(error) => { dialog.error = Some(error.message); cx.notify(); }
                }
            }).ok();
        }).detach();
    }
}

pub(super) fn hand_off_card(dialog: &HandOffDialog, theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    let mut workers = div().flex().flex_col().gap(px(4.0));
    for worker in &dialog.workers { workers = workers.child(div().child(SharedString::from(worker.clone()))); }
    popover::dialog_card(theme)
        .debug_selector(|| "hand-off-dialog".into())
        .child(popover::dialog_title(theme, "Hand off to new chat"))
        .child(div().mt(px(12.0)).child(popover::dialog_field(dialog.input.clone().into_any_element())))
        .child(div().mt(px(12.0)).child(SharedString::from("Workers")))
        .child(div().id("hand-off-workers").max_h(px(180.0)).overflow_y_scroll().text_color(theme.text_muted).child(workers))
        .child(div().id("hand-off-archive").mt(px(12.0)).cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
            if let Some(WorkOutlineModal::HandOff(dialog)) = this.work_outline_modal.as_mut() { dialog.archive = !dialog.archive; cx.notify(); }
        })).child(SharedString::from(if dialog.archive { "☑ Archive this chat after hand-off" } else { "☐ Archive this chat after hand-off" })))
        .when_some(dialog.error.clone(), |card, error| card.child(div().mt(px(8.0)).text_color(theme.warning).child(SharedString::from(error))))
        .child(div().mt(px(16.0)).flex().justify_end().gap(px(8.0))
            .child(popover::btn_ghost(theme, "Cancel", "hand-off-cancel").id("hand-off-cancel").on_click(cx.listener(|this, _, window, cx| { this.resolve_work_outline_modal(false, window, cx); })))
            .child(popover::btn_primary(theme, if dialog.submitting { "Handing off…" } else { "Hand off" }).id("hand-off-submit").on_click(cx.listener(|this, _, _, cx| this.submit_hand_off(cx)))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::{CHAT_API, CHAT_UI};

    #[gpui::test]
    fn dialog_lists_live_workers_and_prefills_summary(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::chat_item_tests::mount(cx, CHAT_API, false, |_| {});
        shell.update(cx, |shell, cx| {
            assert!(!shell.has_hand_off_workers(CHAT_API, cx));
            shell.state.update(cx, |state, _| {
                let worker = state.chats.iter_mut().find(|chat| chat.id == CHAT_UI).unwrap();
                worker.parent_chat_id = Some(CHAT_API.into());
            });
            assert!(shell.has_hand_off_workers(CHAT_API, cx));
            shell.open_hand_off(CHAT_API.into(), cx);
            let Some(WorkOutlineModal::HandOff(dialog)) = &shell.work_outline_modal else { panic!("hand-off dialog not opened") };
            assert!(dialog.input.read(cx).text().starts_with("Continue as coordinator for "));
            assert!(dialog.input.read(cx).text().ends_with("Read the work item, its instructions, and the worker list below."));
            assert_eq!(dialog.workers.len(), 1);
            assert!(dialog.workers[0].contains("Title of"));
            assert!(dialog.workers[0].contains("0 open requests"));
            assert!(!dialog.archive);
        });
    }
}
