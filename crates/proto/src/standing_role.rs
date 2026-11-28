//! Standing roles: a long-lived manager or assistant that outlives any one
//! chat. The role, not a chat id, is what the UI and other chats address; the
//! chat behind it rotates on hand-off.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StandingRoleKind {
    Manager,
    Assistant,
    Lead,
}

impl StandingRoleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manager => "manager",
            Self::Assistant => "assistant",
            Self::Lead => "lead",
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

/// Snapshot display can omit a role introduced by a newer peer. Keep typed
/// role requests strict so an unrecognized role never acquires privileges.
pub(crate) fn deserialize_snapshot_roles<'de, D>(
    deserializer: D,
) -> Result<Vec<StandingRole>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rows = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut roles = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(kind @ serde_json::Value::String(_)) = row.get("role")
            && StandingRoleKind::deserialize(kind).is_err()
        {
            continue;
        }
        roles.push(serde_json::from_value(row).map_err(serde::de::Error::custom)?);
    }
    Ok(roles)
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

// ── Lead tier (Manager → Lead → Worker) ──────────────────────────────────

/// Deepest chat in a Lead hierarchy, counting the Manager as depth 0: the Lead
/// is 1 and its Workers are 2. A launch that would make depth 3 is refused.
pub const MAX_LEAD_HIERARCHY_DEPTH: u8 = 2;
/// Live Leads one project may have.
pub const MAX_LEADS_PER_PROJECT: usize = 4;
/// Live Workers one Lead may have.
pub const MAX_WORKERS_PER_LEAD: usize = 6;
/// Largest context a Lead is launched with (slice down).
pub const MAX_LEAD_CONTEXT_TOKENS: u32 = 8_000;
/// Largest digest a Lead sends its Manager (summary up).
pub const MAX_LEAD_DIGEST_TOKENS: u32 = 350;

/// One token per four bytes, rounded up: the estimate every Lead cap uses.
pub fn approx_tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

/// Cut `text` to at most `max_bytes` on a char boundary, ending in `…` when cut.
fn clip(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// What a Lead is told when it starts or rotates: its slice of the plan, never
/// the whole project. Sent down; the wire form is the same on a paired peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LeadContextPacket {
    pub lead_item_id: String,
    pub goal: String,
    /// The subtree the Lead's path lease covers; its Workers' paths must sit inside.
    pub owned_paths: Vec<String>,
    pub req_ids: Vec<String>,
    /// Budget carved from the Manager's; the Lead carves its Workers' from this.
    pub budget: crate::ItemBudget,
    /// Free text from the Manager (decisions, contracts the slice depends on).
    pub brief: String,
}

impl LeadContextPacket {
    /// The packet as the Lead reads it, cut so it never exceeds
    /// [`MAX_LEAD_CONTEXT_TOKENS`]. Only the free-text brief is ever cut.
    pub fn render(&self) -> String {
        let head = format!(
            "[Lead context for work item {}]\nGoal: {}\nOwned paths: {}\nRequirements: {}\nBudget: {} tokens, {} turns\n",
            self.lead_item_id,
            self.goal,
            self.owned_paths.join(", "),
            self.req_ids.join(", "),
            self.budget.max_tokens.map_or("unspecified".into(), |n| n.to_string()),
            self.budget.max_turns.map_or("unspecified".into(), |n| n.to_string()),
        );
        let cap = MAX_LEAD_CONTEXT_TOKENS as usize * 4;
        let room = cap.saturating_sub(head.len() + "Brief: ".len());
        let out = format!("{head}Brief: {}", clip(&self.brief, room));
        clip(&out, cap)
    }
}

/// How a Lead's subtree stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeadDigestStatus {
    #[default]
    InProgress,
    Blocked,
    Done,
}

/// The only thing a Manager hears from a Lead's subtree. Built by the engine
/// from the Lead's report; leaf Workers' events never reach the Manager.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LeadDigest {
    pub lead_item_id: String,
    pub lead_chat_id: String,
    pub status: LeadDigestStatus,
    pub summary: String,
    pub blockers: Vec<String>,
    pub prs: Vec<String>,
}

impl LeadDigest {
    /// A digest of a Lead's free-text report: its first lines, cut to fit.
    pub fn from_report(lead_item_id: &str, lead_chat_id: &str, report: &str) -> Self {
        Self {
            lead_item_id: lead_item_id.to_owned(),
            lead_chat_id: lead_chat_id.to_owned(),
            summary: report.trim().to_owned(),
            ..Self::default()
        }
        .bounded()
    }

    /// The digest as the Manager reads it, within [`MAX_LEAD_DIGEST_TOKENS`].
    pub fn render(&self) -> String {
        let mut out = format!(
            "[Lead digest, item {}, chat {}] status: {:?}\n{}",
            self.lead_item_id, self.lead_chat_id, self.status, self.summary
        );
        for blocker in &self.blockers {
            out.push_str("\nBlocker: ");
            out.push_str(blocker);
        }
        for pr in &self.prs {
            out.push_str("\nPR: ");
            out.push_str(pr);
        }
        out
    }

    /// This digest with its summary, blockers and PRs trimmed until the
    /// rendered text fits [`MAX_LEAD_DIGEST_TOKENS`]. Lists are cut first.
    pub fn bounded(mut self) -> Self {
        let cap = MAX_LEAD_DIGEST_TOKENS as usize * 4;
        self.blockers.iter_mut().for_each(|b| *b = clip(b, 200));
        self.prs.iter_mut().for_each(|p| *p = clip(p, 120));
        self.blockers.truncate(3);
        self.prs.truncate(3);
        while self.render().len() > cap && (!self.blockers.is_empty() || !self.prs.is_empty()) {
            if self.blockers.pop().is_none() {
                self.prs.pop();
            }
        }
        let overflow = self.render().len().saturating_sub(cap);
        if overflow > 0 {
            self.summary = clip(&self.summary, self.summary.len().saturating_sub(overflow));
        }
        self
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

    #[test]
    fn a_lead_digest_and_context_packet_never_exceed_their_caps() {
        let long = "word ".repeat(5_000);
        let digest = LeadDigest {
            blockers: vec![long.clone(); 9],
            prs: vec![long.clone(); 9],
            summary: long.clone(),
            ..LeadDigest::default()
        }
        .bounded();
        assert!(approx_tokens(&digest.render()) <= MAX_LEAD_DIGEST_TOKENS);
        assert!(LeadDigest::from_report("i", "c", &long).render().len() <= 1400);
        let short = LeadDigest::from_report("i", "c", "done");
        assert!(short.render().ends_with("done"));
        let packet = LeadContextPacket { brief: long.repeat(20), ..LeadContextPacket::default() };
        assert!(approx_tokens(&packet.render()) <= MAX_LEAD_CONTEXT_TOKENS);
        assert!(clip("héllo wörld", 4).is_char_boundary(clip("héllo wörld", 4).len()));
    }

    #[test]
    fn lead_wire_types_tolerate_missing_fields() {
        let digest: LeadDigest = serde_json::from_str(r#"{"leadItemId":"i"}"#).unwrap();
        assert_eq!(digest.status, LeadDigestStatus::InProgress);
        let packet: LeadContextPacket = serde_json::from_str("{}").unwrap();
        assert!(packet.owned_paths.is_empty());
    }
}
