//! The right pane's "Settings" tab, opened from the `+` menu. On a project's
//! Manager chat it holds the Manager's settings; on any other chat it shows
//! that chat's own model, reasoning, sandbox and approvals.
use super::*;
use crate::components::shell_parts::section_label;
use crate::pickers::approval::ApprovalMode;

fn sandbox_label(level: mast_proto::SandboxLevel) -> &'static str {
    match level {
        mast_proto::SandboxLevel::ReadOnly => "Read-only",
        mast_proto::SandboxLevel::WorkspaceWrite => "Workspace write",
        mast_proto::SandboxLevel::DangerFullAccess => "Full access",
    }
}

fn reasoning_label(level: mast_proto::ReasoningLevel) -> &'static str {
    use mast_proto::ReasoningLevel::*;
    match level {
        Minimal => "Minimal",
        Low => "Low",
        Medium => "Medium",
        High => "High",
        XHigh => "Extra high",
        Max => "Max",
        Ultra => "Ultra",
        Ultracode => "Ultracode",
        Ultrathink => "Ultrathink",
    }
}

/// The chat's own settings as (label, value) rows. A chat with no stored
/// config shows only the model.
pub(crate) fn chat_setting_rows(model: String, config: Option<&mast_proto::ChatConfig>) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Model", model)];
    if let Some(config) = config {
        rows.push(("Reasoning", config.reasoning.map(reasoning_label).unwrap_or("Default").to_owned()));
        rows.push(("Sandbox", sandbox_label(config.sandbox).into()));
        rows.push(("Approvals", ApprovalMode::from_config(config).label().into()));
    }
    rows
}

impl Shell {
    /// The `+` menu's Settings row: one Settings tab per chat, activated if it
    /// is already open.
    pub(super) fn add_settings_surface(&mut self, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        let tabs = self.right_tabs.entry(key).or_default();
        push_unique_right_surface(tabs, RightSurface::Settings);
        self.set_right_active(RightSurface::Settings, cx);
    }

    pub(super) fn render_settings_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let chat_id = self.active_chat.clone();
        let Some(facts) = self.chat_item_facts(&chat_id, cx) else {
            return div().size_full().into_any_element();
        };
        let manager = facts.item.as_ref().and_then(|item| item.manager.as_ref().map(|manager| (item, manager)));
        let body = match manager {
            Some((item, manager)) => self.render_item_tab_manager(item, manager, &theme, cx),
            None => {
                let config = self.state.read(cx).chats.iter().find(|chat| chat.id == chat_id).and_then(|chat| chat.config.clone());
                let model = facts.model.label.clone().unwrap_or_else(|| "default".into());
                let mut block = div()
                    .debug_selector(|| "chat-settings".into())
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(section_label(&theme, crate::icons::SETTINGS_MINIMALISTIC, "Chat", "chat-settings-icon".into()));
                for (label, value) in chat_setting_rows(model, config.as_ref()) {
                    let selector = format!("chat-settings-row:{label}:{value}");
                    block = block.child(
                        div()
                            .debug_selector(move || selector.clone())
                            .flex()
                            .gap(px(6.0))
                            .child(crate::components::shell_parts::panel_text(&theme, label.into(), true))
                            .child(crate::components::shell_parts::panel_text(&theme, value, false)),
                    );
                }
                block.into_any_element()
            }
        };
        let focus = self.team_panel_focus(cx);
        self.panel_root("chat-settings-tab", focus, body, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(auto_approve: bool, reasoning: Option<mast_proto::ReasoningLevel>) -> mast_proto::ChatConfig {
        mast_proto::ChatConfig {
            harness: mast_proto::HarnessId::ClaudeCode,
            model: None,
            reasoning,
            model_options: Default::default(),
            sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
            auto_approve,
            approval_rules: false,
        }
    }

    #[test]
    fn a_chat_shows_its_own_settings_and_no_manager_rows() {
        let rows = chat_setting_rows("Sonnet 5.5".into(), Some(&config(true, Some(mast_proto::ReasoningLevel::High))));
        assert_eq!(
            rows,
            [
                ("Model", "Sonnet 5.5".to_owned()),
                ("Reasoning", "High".to_owned()),
                ("Sandbox", "Workspace write".to_owned()),
                ("Approvals", "Auto".to_owned()),
            ]
        );
    }

    #[test]
    fn a_chat_without_a_stored_config_shows_only_its_model() {
        assert_eq!(chat_setting_rows("default".into(), None), [("Model", "default".to_owned())]);
        let rows = chat_setting_rows("m".into(), Some(&config(false, None)));
        assert_eq!(rows[1], ("Reasoning", "Default".to_owned()));
        assert_eq!(rows[3], ("Approvals", "Ask".to_owned()));
    }
}
