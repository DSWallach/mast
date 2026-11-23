use super::*;

impl AppState {
    pub(crate) fn can_ask_on_side(&self, chat_id: &str) -> bool {
        use mast_proto::capabilities::SIDE_CHAT_V1;
        let local = match self.engine() {
            Some(engine) => engine.engine_info().supports(SIDE_CHAT_V1),
            None => self
                .local_device_id
                .as_deref()
                .is_some_and(|device| self.device_supports(device, SIDE_CHAT_V1)),
        };
        local
            && self
                .chats
                .iter()
                .any(|chat| chat.id == chat_id && !chat.archived && !chat.is_side_chat())
            && self.chat_host_supports(chat_id, SIDE_CHAT_V1)
    }

    pub(crate) fn running_subagents(
        &self,
        chat_id: &str,
    ) -> Vec<crate::transcript::RunningSubagent> {
        if self.selected_chat.as_deref() != Some(chat_id) {
            return Vec::new();
        }
        crate::transcript::running_subagents(chat_id, &self.transcript)
    }

    pub(crate) fn running_activity(
        &self,
        chat_id: &str,
    ) -> Vec<crate::transcript::RunningActivity> {
        if self.selected_chat.as_deref() != Some(chat_id) {
            return Vec::new();
        }
        crate::transcript::running_activity(chat_id, &self.transcript)
    }

    pub(crate) fn set_sidebar_outline_sections(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.sidebar_outline_sections != enabled {
            self.sidebar_outline_sections = enabled;
            self.reconcile_work_outline_watches(cx);
        }
    }
}
