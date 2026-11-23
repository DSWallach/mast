//! The caption above the composer on a scheduled run's chat: where a reply
//! goes (a new follow-up chat by default, or in place), the switch between
//! the two, and links to follow-ups already opened from the run.

use super::*;

impl Composer {
    /// Persist the run chat's choice to continue replies in place.
    fn set_followups_in_place(&mut self, chat_id: String, in_place: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            self.failure_key = None;
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                methods::MUTATE,
                serde_json::json!({
                    "op": "setScheduledFollowupsInPlace",
                    "chatId": chat_id,
                    "inPlace": in_place,
                }),
                Duration::from_secs(10),
            )
            .await;
            if let Err(error) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Couldn't change where replies go: {error}").into());
                    composer.failure_key = Some(chat_id.clone());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn render_followup_notice(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (chat_id, in_place, links) = {
            let state = self.state.read(cx);
            let id = state.selected_chat.clone()?;
            let chat = state.selected_chat_row()?;
            let task = chat.scheduled_task_info.as_ref()?;
            let opens_new = state
                .followup_reply_source(&id, chrono::Utc::now())
                .is_some();
            if !opens_new && !task.followups_in_place {
                return None;
            }
            let links: Vec<(String, String)> = state
                .followups_of(&id)
                .into_iter()
                .map(|chat| {
                    (
                        chat.id.clone(),
                        chat.title.clone().unwrap_or_else(|| "Follow-up".into()),
                    )
                })
                .collect();
            (id, task.followups_in_place, links)
        };
        let (caption, action) = if in_place {
            ("Replies continue in this chat.", "Open new chats instead")
        } else {
            (
                "A reply opens a new chat that starts from this run's result.",
                "Continue in this chat",
            )
        };
        let toggle_id = chat_id.clone();
        let mut row = div()
            .id("composer-followup-notice")
            .debug_selector(|| "composer-followup-notice".into())
            .mx(px(8.0))
            .mt(px(6.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.0))
            .line_height(px(14.0))
            .text_color(theme.text_faint)
            .child(div().child(caption))
            .child(
                div()
                    .id("composer-followup-toggle")
                    .debug_selector(|| "composer-followup-toggle".into())
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child(action)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_followups_in_place(toggle_id.clone(), !in_place, cx);
                    })),
            );
        for (index, (id, title)) in links.into_iter().enumerate() {
            row = row.child(
                div()
                    .id(("composer-followup-link", index))
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.state.update(cx, |s, cx| s.select_chat(Some(id), cx));
                    })),
            );
        }
        Some(row.into_any_element())
    }
}
