//! The personal assistant chat, UI side: which chat it is (the engine owns
//! the designation, `mast_proto::personal_assistant`) and the RPC calls.
//! The sidebar draws it in its own section above the space filter and
//! leaves it out of the session lists.

use std::time::{Duration, Instant};

use gpui::Context;
use mast_proto::PersonalAssistantReply;
use mast_rpc::methods;

use crate::state::AppState;

/// Until the first read lands, it is retried this often.
const LOAD_RETRY: Duration = Duration::from_secs(5);

pub(crate) const SET_LABEL: &str = "Make personal assistant";
pub(crate) const CLEAR_LABEL: &str = "Remove personal assistant";
pub(crate) const SECTION_LABEL: &str = "Personal assistant";

#[derive(Debug, Default)]
pub(crate) struct PersonalAssistantState {
    chat_id: Option<String>,
    loaded: bool,
    read_at: Option<Instant>,
    in_flight: bool,
}

impl PersonalAssistantState {
    pub(crate) fn is(&self, chat_id: &str) -> bool {
        self.chat_id.as_deref() == Some(chat_id)
    }

    /// Apply an engine reply; `true` when the chat changed.
    pub(crate) fn apply(&mut self, reply: PersonalAssistantReply) -> bool {
        self.loaded = true;
        let chat_id = reply.assistant.map(|a| a.chat_id);
        let changed = chat_id != self.chat_id;
        self.chat_id = chat_id;
        changed
    }

    fn load_due(&self, now: Instant) -> bool {
        !self.loaded
            && !self.in_flight
            && self.read_at.is_none_or(|at| now.duration_since(at) >= LOAD_RETRY)
    }
}

impl AppState {
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
        self.personal_assistant.chat_id = Some(chat_id.to_owned());
        cx.notify();
        self.call_personal_assistant(
            methods::SET_PERSONAL_ASSISTANT,
            serde_json::json!({ "chatId": chat_id }),
            cx,
        );
    }

    pub(crate) fn clear_personal_assistant(&mut self, cx: &mut Context<Self>) {
        self.personal_assistant.chat_id = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::PersonalAssistant;

    fn reply(chat_id: Option<&str>) -> PersonalAssistantReply {
        PersonalAssistantReply {
            assistant: chat_id.map(|id| PersonalAssistant {
                chat_id: id.to_owned(),
                designated_at_ms: 1,
            }),
        }
    }

    #[test]
    fn apply_reports_only_changes_and_stops_the_load_retry() {
        let mut state = PersonalAssistantState::default();
        assert!(state.load_due(Instant::now()));
        assert!(!state.apply(reply(None)));
        assert!(!state.load_due(Instant::now()));
        assert!(state.apply(reply(Some("c1"))));
        assert!(state.is("c1"));
        assert!(!state.apply(reply(Some("c1"))));
        assert!(state.apply(reply(None)));
        assert!(!state.is("c1"));
    }
}
