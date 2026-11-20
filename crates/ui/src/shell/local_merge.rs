use super::*;

impl Shell {
    pub(super) fn handle_transcript_event(
        &mut self,
        event: &TranscriptEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TranscriptEvent::OpenChat(chat_id) => self.open_chat(chat_id.clone(), cx),
            TranscriptEvent::SignInAgain(chat_id) => self.sign_in_for_chat(chat_id.clone(), cx),
            TranscriptEvent::RetryErroredTurn {
                chat_id,
                error_entry_id,
            } => {
                let Some(engine) = self.state.read(cx).engine().cloned() else {
                    return;
                };
                let device = self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|chat| chat.id == *chat_id)
                    .map(|chat| chat.device_id.clone());
                let chat_id = chat_id.clone();
                let error_entry_id = error_entry_id.clone();
                cx.spawn(async move |this, cx| {
                    let result = engine.client().call(mast_rpc::methods::RETRY_ERRORED_TURN,
                        serde_json::json!({ "chatId": chat_id, "errorEntryId": error_entry_id, "targetDeviceId": device })).await;
                    if let Err(error) = result {
                        this.update(cx, |shell, cx| {
                            shell.set_sidebar_error(format!("Retry failed: {error}"));
                            cx.notify();
                        }).ok();
                    }
                }).detach();
            }
            TranscriptEvent::OpenSubagent {
                chat_id,
                doc_id,
                title,
                frozen,
            } => {
                self.leave_overview_for_chat(cx);
                self.add_subagent_surface(
                    chat_id.clone(),
                    doc_id.clone(),
                    title.clone(),
                    *frozen,
                    cx,
                );
            }
        }
    }

    pub(super) fn ensure_scheduled_tasks_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::scheduled_tasks::ScheduledTasksPage> {
        if let Some(page) = &self.scheduled_tasks_page {
            return page.clone();
        }
        let state = self.state.clone();
        let page = cx.new(|cx| crate::scheduled_tasks::ScheduledTasksPage::new(state, cx));
        self.scheduled_tasks_events = Some(cx.subscribe(&page, |shell, _, event, cx| {
            let crate::scheduled_tasks::TaskPageEvent::OpenChat(id) = event;
            shell.route = Route::Chat;
            shell
                .state
                .update(cx, |s, cx| s.select_chat(Some(id.clone()), cx));
            cx.notify();
        }));
        self.scheduled_tasks_page = Some(page.clone());
        page
    }

    pub(super) fn open_scheduled_tasks(&mut self, history: Option<String>, cx: &mut Context<Self>) {
        self.route = Route::ScheduledTasks;
        self.nav.push(NavEntry::ScheduledTasks);
        let page = self.ensure_scheduled_tasks_page(cx);
        if let Some(id) = history {
            let device = self
                .state
                .read(cx)
                .selected_chat_row()
                .filter(|c| {
                    c.scheduled_task_info
                        .as_ref()
                        .is_some_and(|t| t.task_id == id)
                })
                .map(|c| c.device_id.clone());
            page.update(cx, |p, cx| p.show_task(id, device, cx));
        } else {
            page.update(cx, |p, cx| p.show_tasks(cx));
        }
        cx.notify();
    }
}
