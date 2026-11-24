//! Standing roles: a long-lived manager or assistant that outlives any one
//! chat. The role, not a chat id, is what the UI and other chats address; the
//! chat behind it rotates on hand-off.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StandingRoleKind {
    Manager,
    Assistant,
}

impl StandingRoleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manager => "manager",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StandingRoleState {
    #[default]
    Idle,
    HandingOff,
}

/// One fallback entry. Strings, not enums, so a newer peer's harness id
/// survives a round trip through an older client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandingRoleModel {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Synced record of one role in one scope. `chain` is every chat that has
/// held the role, oldest first; `current_chat_id` is its last entry, or
/// `None` while the role is a placeholder with no chat yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandingRole {
    pub id: String,
    pub role: StandingRoleKind,
    pub scope_item_id: String,
    pub current_chat_id: Option<String>,
    pub chain: Vec<String>,
    pub fallback_chain: Vec<StandingRoleModel>,
    pub state: StandingRoleState,
    /// Model the Manager's workers start on when its `create_chat` call
    /// names none. `None` means [`default_manager_worker_model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<StandingRoleModel>,
    /// Whether the Manager's runs are read-only. `None` (a project that never
    /// chose) means on; see [`StandingRole::manager_runs_read_only`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

impl StandingRole {
    /// Whether a project's Manager runs read-only: on unless the project
    /// turned it off. A project with no role record yet is on.
    pub fn manager_runs_read_only(role: Option<&Self>) -> bool {
        role.and_then(|role| role.read_only).unwrap_or(true)
    }
}

/// Most fallback entries one role record may carry, and the longest harness
/// or model string in one. The role row is a single registry op, so these
/// keep it far under the op size limit.
pub const MAX_FALLBACK_CHAIN: usize = 8;
pub const MAX_FALLBACK_FIELD_LEN: usize = 128;

impl StandingRoleModel {
    pub fn validate_chain(chain: &[Self]) -> Result<(), String> {
        if chain.len() > MAX_FALLBACK_CHAIN {
            return Err(format!(
                "a role takes at most {MAX_FALLBACK_CHAIN} fallback models"
            ));
        }
        for entry in chain {
            let too_long = entry.harness.len() > MAX_FALLBACK_FIELD_LEN
                || entry
                    .model
                    .as_deref()
                    .is_some_and(|m| m.len() > MAX_FALLBACK_FIELD_LEN);
            if entry.harness.is_empty() || too_long {
                return Err("a fallback model needs a harness and short names".into());
            }
        }
        Ok(())
    }
}

/// A hand-off the source's host has committed to but not yet finished. It is
/// recorded before the launch creates anything and cleared by the rotation,
/// so a different successor cannot start while this one is part-done, and the
/// same operation can be retried to completion after a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingStandingHandOff {
    pub from_chat_id: String,
    pub to_chat_id: String,
    pub operation_id: String,
}

/// Worker model of a project whose Manager has no setting: Claude Sonnet 5.5.
pub fn default_manager_worker_model() -> StandingRoleModel {
    StandingRoleModel {
        harness: "claude-code".into(),
        model: Some("claude-sonnet-5-5".into()),
    }
}

/// Instruction id of the built-in Manager entry in a revision.
pub const MANAGER_ROLE_INSTRUCTION_ID: &str = "builtin:manager-role";

/// The rule that keeps the Manager status record current. Mast does not remind
/// a Manager that forgets, so this instruction is the only per-turn enforcement.
pub fn manager_status_record_rule() -> &'static str {
    crate::MANAGER_STATUS_RULE
}

/// Built-in instructions of a project's Manager, delivered through the
/// instruction envelope and stacked with the project's own effective
/// instructions (never part of a prompt).
pub fn manager_role_instructions(worker_model: &StandingRoleModel) -> String {
    let model = worker_model.model.as_deref().unwrap_or("the harness default");
    let mut text = format!(
        "You are this project's Manager. You coordinate; you never do the work yourself.\n\
- Turn every task the user asks for into a new work item under the project's worker destination \
(`work_outline_create_child` with no parent), then start a worker on it with `create_chat` for that \
item. One task, one item, one worker. Workers start on {} ({model}) unless the user asks for another model.\n\
- Do not do the project's work yourself: do not edit files or run builds and tests, unless the \
project's own instructions tell you to. If a tool is refused, do not try to work around it. Use your \
tools to answer questions and check on workers.\n\
- Never block on a worker: do not send with `wait: true` and do not call `wait_for_turn`. Workers wake you \
with `report_to_parent` when they finish or need you.\n\
- Keep replies short. When a worker raises a Required Action, relay it to the user in one or two lines \
and say which worker and item it belongs to.",
        worker_model.harness
    );
    text.push_str("\n- ");
    text.push_str(manager_status_record_rule());
    text
}

/// Row id: one role of one kind per scope item.
pub fn standing_role_id(role: StandingRoleKind, scope_item_id: &str) -> String {
    format!("{}:{scope_item_id}", role.as_str())
}

impl StandingRole {
    /// Whether a rotation from `from` to `to` is already recorded: the role
    /// holds `to` and `from` held it immediately before.
    pub fn replays_rotation(&self, from: &str, to: &str) -> bool {
        self.current_chat_id.as_deref() == Some(to)
            && self.chain.iter().rev().nth(1).map(String::as_str) == Some(from)
    }

    /// Chats that held the role before the current one, oldest first.
    pub fn predecessors(&self) -> &[String] {
        match self.chain.split_last() {
            Some((_, rest)) => rest,
            None => &[],
        }
    }

    /// Follow `chat_id` forward to the chat now holding its role, using only
    /// the `standing_role_successor` markers on `chats`. Returns `chat_id`
    /// itself when it was never superseded, and stops at a missing or cyclic
    /// link rather than looping.
    pub fn resolve_current<'a>(chats: &'a [crate::Chat], chat_id: &'a str) -> &'a str {
        let mut current = chat_id;
        for _ in 0..=chats.len() {
            let Some(next) = chats
                .iter()
                .find(|c| c.id == current)
                .and_then(|c| c.standing_role_successor.as_deref())
                .filter(|next| chats.iter().any(|c| c.id == *next))
            else {
                return current;
            };
            current = next;
        }
        chat_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, successor: Option<&str>) -> crate::Chat {
        let mut value = serde_json::json!({
            "id": id, "deviceId": "dev", "archived": false,
            "createdAt": "2026-01-01T00:00:00Z",
        });
        if let Some(next) = successor {
            value["standingRoleSuccessor"] = serde_json::json!(next);
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn role_id_is_per_kind_and_scope() {
        assert_eq!(
            standing_role_id(StandingRoleKind::Manager, "p1"),
            "manager:p1"
        );
        assert_ne!(
            standing_role_id(StandingRoleKind::Manager, "p1"),
            standing_role_id(StandingRoleKind::Assistant, "p1")
        );
    }

    #[test]
    fn resolve_current_follows_the_chain_and_survives_cycles() {
        let chats = [chat("a", Some("b")), chat("b", Some("c")), chat("c", None)];
        assert_eq!(StandingRole::resolve_current(&chats, "a"), "c");
        assert_eq!(StandingRole::resolve_current(&chats, "c"), "c");
        assert_eq!(StandingRole::resolve_current(&chats, "unknown"), "unknown");
        let cyclic = [chat("x", Some("y")), chat("y", Some("x"))];
        assert_eq!(StandingRole::resolve_current(&cyclic, "x"), "x");
        let dangling = [chat("d", Some("gone"))];
        assert_eq!(StandingRole::resolve_current(&dangling, "d"), "d");
    }

    #[test]
    fn manager_instructions_name_the_worker_model_and_forbid_blocking() {
        let text = manager_role_instructions(&default_manager_worker_model());
        assert!(text.contains("claude-sonnet-5-5"), "{text}");
        assert!(text.contains("wait: true"));
        assert!(text.contains("work_outline_create_child"));
        assert!(text.contains("create_chat"));
        assert!(text.contains("Required Action"));
        assert!(text.contains(manager_status_record_rule()));
        assert!(text.contains("Updated:"));
        for section in crate::MANAGER_STATUS_SECTIONS {
            assert!(text.contains(section), "{section}");
        }
        assert!(text.chars().count() < crate::limits::MAX_INSTRUCTION_TEXT_CHARS);
    }

    #[test]
    fn predecessors_exclude_the_current_chat() {
        let role = StandingRole {
            id: "manager:p".into(),
            role: StandingRoleKind::Manager,
            scope_item_id: "p".into(),
            current_chat_id: Some("c".into()),
            chain: vec!["a".into(), "b".into(), "c".into()],
            fallback_chain: Vec::new(),
            state: StandingRoleState::Idle,
            worker_model: None,
            read_only: None,
        };
        assert_eq!(role.predecessors(), ["a", "b"]);
    }
}
