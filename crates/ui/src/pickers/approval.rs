//! The composer's per-chat approval chip: `Ask` / `Rules` / `Auto`.
//!
//! Persisted on the chat row as `ChatConfig.autoApprove` plus
//! `ChatConfig.approvalRules` (both default `false` = Ask) and resolved into
//! the `RunRequest` on every send. Ask means the harness surfaces permission
//! prompts in the UI; Rules answers them from `~/.mast/settings.json` and
//! asks about the rest; Auto means the agent never asks (full access), so it
//! renders in the theme's warning accent. A click cycles it: on an existing
//! chat that has a config it writes `Mutate setChatAutoApprove` (patches just
//! those fields); on a config-less chat it writes a full `setChatConfig` the
//! way the harness/model pickers do; on the new-chat canvas it's a draft
//! carried onto `createChat`.
//!
//! Pure logic only — the chip view lives in `Pickers`.

use mast_proto::ChatConfig;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// The agent requests approval for risky actions (the default).
    #[default]
    Ask,
    /// Requests matching the user's permission rules run without asking.
    Rules,
    /// The agent never asks — full access.
    Auto,
}

impl ApprovalMode {
    pub fn from_flags(auto_approve: bool, approval_rules: bool) -> Self {
        match (auto_approve, approval_rules) {
            (true, _) => Self::Auto,
            (false, true) => Self::Rules,
            (false, false) => Self::Ask,
        }
    }

    pub fn from_config(config: &ChatConfig) -> Self {
        Self::from_flags(config.auto_approve, config.approval_rules)
    }

    pub fn auto_approve(self) -> bool {
        self == Self::Auto
    }

    pub fn approval_rules(self) -> bool {
        self == Self::Rules
    }

    /// Three states: a click cycles Ask → Rules → Auto.
    pub fn next(self) -> Self {
        match self {
            Self::Ask => Self::Rules,
            Self::Rules => Self::Auto,
            Self::Auto => Self::Ask,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Rules => "Rules",
            Self::Auto => "Auto",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Ask => "Ask: the agent requests approval for risky actions",
            Self::Rules => "Rules: requests allowed in ~/.mast/settings.json run without asking; the rest ask",
            Self::Auto => "Auto-approve: the agent never asks \u{2014} full access",
        }
    }
}

/// The approval mode the next send uses (and the chip shows).
///
/// - New-chat canvas (`selected_chat: None`): the draft pick.
/// - Selected chat with a config row: the row's persisted value.
/// - Selected chat whose row hasn't synced a config yet: the value carried
///   over from the new-chat draft that minted THIS chat (so the moment
///   between send and the row landing doesn't flash back to Ask), else Ask.
pub fn effective_mode(
    selected_chat: Option<&str>,
    row_config: Option<&ChatConfig>,
    carried: Option<(&str, ApprovalMode)>,
    draft: ApprovalMode,
) -> ApprovalMode {
    let Some(chat_id) = selected_chat else {
        return draft;
    };
    if let Some(config) = row_config {
        return ApprovalMode::from_config(config);
    }
    carried
        .filter(|(carried_id, _)| *carried_id == chat_id)
        .map_or(ApprovalMode::Ask, |(_, mode)| mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{HarnessId, SandboxLevel};

    fn config(auto_approve: bool) -> ChatConfig {
        ChatConfig { approval_rules: false,
            harness: HarnessId::ClaudeCode,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve,
        }
    }

    #[test]
    fn mode_defaults_to_ask_and_cycles() {
        use ApprovalMode::*;
        assert_eq!(ApprovalMode::default(), Ask);
        assert_eq!(Ask.next(), Rules);
        assert_eq!(Rules.next(), Auto);
        assert_eq!(Auto.next(), Ask);
        assert_eq!(ApprovalMode::from_flags(true, true), Auto);
        assert_eq!(ApprovalMode::from_flags(false, true), Rules);
        assert_eq!(ApprovalMode::from_flags(false, false), Ask);
        assert!(Rules.approval_rules() && !Rules.auto_approve());
        assert_eq!(Rules.label(), "Rules");
        assert!(Auto.tooltip().starts_with("Auto-approve:"));
    }

    #[test]
    fn effective_mode_prefers_the_row_then_the_carried_draft() {
        use ApprovalMode::*;
        // New-chat canvas: the draft.
        assert_eq!(effective_mode(None, None, None, Rules), Rules);
        // A configured row wins over anything carried.
        let yolo = config(true);
        let mut rules = config(false);
        rules.approval_rules = true;
        assert_eq!(effective_mode(Some("c"), Some(&yolo), None, Ask), Auto);
        assert_eq!(effective_mode(Some("c"), Some(&rules), Some(("c", Auto)), Auto), Rules);
        // Config-less row: only a draft carried onto THIS chat applies.
        assert_eq!(effective_mode(Some("c"), None, Some(("c", Rules)), Ask), Rules);
        assert_eq!(effective_mode(Some("c"), None, Some(("other", Auto)), Ask), Ask);
        assert_eq!(effective_mode(Some("c"), None, None, Auto), Ask);
    }
}
