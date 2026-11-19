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
    fn predecessors_exclude_the_current_chat() {
        let role = StandingRole {
            id: "manager:p".into(),
            role: StandingRoleKind::Manager,
            scope_item_id: "p".into(),
            current_chat_id: Some("c".into()),
            chain: vec!["a".into(), "b".into(), "c".into()],
            fallback_chain: Vec::new(),
            state: StandingRoleState::Idle,
        };
        assert_eq!(role.predecessors(), ["a", "b"]);
    }
}
