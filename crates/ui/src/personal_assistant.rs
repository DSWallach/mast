//! The personal assistant chat, UI side: which chat it is (the engine owns
//! the designation, `mast_proto::personal_assistant`) and the RPC calls.
//! Overview keeps fixed access in its header; Sessions keeps its own section
//! above the space filter and leaves it out of the session lists. Scheduled-task
//! assistants show their task's latest run.

use std::time::{Duration, Instant};

use gpui::Context;
use mast_proto::{Chat, PersonalAssistant, PersonalAssistantReply};
use mast_rpc::methods;

use crate::state::AppState;

/// Until the first read lands, it is retried this often.
const LOAD_RETRY: Duration = Duration::from_secs(5);

pub(crate) const SET_LABEL: &str = "Make personal assistant";
pub(crate) const CLEAR_LABEL: &str = "Remove personal assistant";
pub(crate) const SECTION_LABEL: &str = "Personal assistant";

#[derive(Debug, Default)]
pub(crate) struct PersonalAssistantState {
    assistant: Option<PersonalAssistant>,
    loaded: bool,
    read_at: Option<Instant>,
    in_flight: bool,
}

impl PersonalAssistantState {
    /// The picked chat, or any run of its scheduled task (`chats`: every
    /// chat, where a merged imported task is looked up).
    pub(crate) fn covers(&self, chat: &Chat, chats: &[Chat]) -> bool {
        self.assistant.as_ref().is_some_and(|a| a.covers(chat, chats))
    }

    pub(crate) fn coverage<'a>(&'a self, chats: &'a [Chat]) -> impl Fn(&Chat) -> bool + 'a {
        let covers = self.assistant.as_ref().map(|a| a.coverage(chats));
        move |chat| covers.as_ref().is_some_and(|covers| covers(chat))
    }

    /// The chat the sidebar shows and opens (a task's latest run).
    pub(crate) fn current<'a>(&self, chats: &'a [Chat]) -> Option<&'a Chat> {
        self.assistant.as_ref()?.current(chats)
    }

    /// Include designation changes in view invalidation once, independent of chat count.
    pub(crate) fn hash_designation(&self, hasher: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.assistant
            .as_ref()
            .map(|assistant| {
                (
                    &assistant.chat_id,
                    assistant
                        .scheduled_task
                        .as_ref()
                        .map(|task| (&task.task_id, &task.device_id, &task.space_id)),
                )
            })
            .hash(hasher);
    }

    /// Apply an engine reply; `true` when the designation changed.
    pub(crate) fn apply(&mut self, reply: PersonalAssistantReply) -> bool {
        self.loaded = true;
        let changed = reply.assistant != self.assistant;
        self.assistant = reply.assistant;
        changed
    }

    fn load_due(&self, now: Instant) -> bool {
        !self.loaded
            && !self.in_flight
            && self.read_at.is_none_or(|at| now.duration_since(at) >= LOAD_RETRY)
    }
}

impl AppState {
    pub(crate) fn is_personal_assistant(&self, chat_id: &str) -> bool {
        self.chats
            .iter()
            .find(|c| c.id == chat_id)
            .is_some_and(|c| self.personal_assistant.covers(c, &self.chats))
    }

    /// The 1 s heartbeat: read the designation until the first read lands.
    pub(crate) fn personal_assistant_tick(&mut self, cx: &mut Context<Self>) {
        if self.personal_assistant.load_due(Instant::now()) {
            self.refresh_personal_assistant(cx);
        }
    }

    pub(crate) fn refresh_personal_assistant(&mut self, cx: &mut Context<Self>) {
        self.call_personal_assistant(
            methods::GET_PERSONAL_ASSISTANT,
            serde_json::json!({}),
            cx,
        );
    }

    /// Make `chat_id` the personal assistant (replacing any other). The row
    /// moves at once; the reply is the truth.
    pub(crate) fn set_personal_assistant(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter().find(|c| c.id == chat_id) else {
            return;
        };
        if chat.is_background_task_run() {
            return;
        }
        self.personal_assistant.assistant = Some(PersonalAssistant::for_chat(
            chat,
            chrono::Utc::now().timestamp_millis(),
        ));
        cx.notify();
        self.call_personal_assistant(
            methods::SET_PERSONAL_ASSISTANT,
            serde_json::json!({ "chatId": chat_id }),
            cx,
        );
    }

    pub(crate) fn clear_personal_assistant(&mut self, cx: &mut Context<Self>) {
        self.personal_assistant.assistant = None;
        cx.notify();
        self.call_personal_assistant(
            methods::CLEAR_PERSONAL_ASSISTANT,
            serde_json::json!({}),
            cx,
        );
    }

    fn call_personal_assistant(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        self.personal_assistant.in_flight = true;
        self.personal_assistant.read_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let reply = engine
                .client()
                .call_as::<PersonalAssistantReply>(method, params)
                .await;
            this.update(cx, |state, cx| {
                state.personal_assistant.in_flight = false;
                match reply {
                    Ok(reply) => {
                        if state.personal_assistant.apply(reply) {
                            cx.notify();
                        }
                    }
                    // A refused set or clear: re-read so the sidebar matches the engine.
                    Err(_) if method != methods::GET_PERSONAL_ASSISTANT => {
                        state.refresh_personal_assistant(cx);
                    }
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
    }
}

/// Small role labels shared by the overview list and outline, including compact rows.
pub(crate) fn session_badge(
    state: &AppState,
    chat_id: &str,
    theme: &crate::theme::Theme,
) -> Option<gpui::AnyElement> {
    use gpui::{div, prelude::*, px};
    let assistant = state.is_personal_assistant(chat_id);
    let current = state.selected_chat.as_deref() == Some(chat_id);
    let label = match (assistant, current) {
        (true, true) => "Assistant · Current",
        (true, false) => "Assistant",
        (false, true) => "Current",
        (false, false) => return None,
    };
    Some(
        div()
            .flex_none()
            .px(px(5.0))
            .py(px(1.0))
            .rounded(px(3.0))
            .bg(theme.element_hover)
            .text_color(theme.text_muted)
            .text_size(crate::typography::ui_rems(9.5))
            .child(label)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": "d1", "archived": false, "spaceId": "s1",
            "createdAt": "2026-11-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn reply(chat_id: Option<&str>) -> PersonalAssistantReply {
        PersonalAssistantReply {
            assistant: chat_id.map(|id| PersonalAssistant::for_chat(&chat(id), 1)),
        }
    }

    #[test]
    fn apply_reports_only_changes_and_stops_the_load_retry() {
        let mut state = PersonalAssistantState::default();
        assert!(state.load_due(Instant::now()));
        assert!(!state.apply(reply(None)));
        assert!(!state.load_due(Instant::now()));
        assert!(state.apply(reply(Some("c1"))));
        assert!(state.covers(&chat("c1"), &[]));
        assert!(!state.apply(reply(Some("c1"))));
        assert!(state.apply(reply(None)));
        assert!(!state.covers(&chat("c1"), &[]));
    }
}
