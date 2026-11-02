//! Admin chats: a user-granted, Space-scoped, expiring widening of what one
//! chat's agent tools may reach (`docs/plans/work-outline/m0-contract.md`,
//! "Amendment: admin chats"). Only the user grants or revokes (IPC RPCs);
//! no agent tool does.

use serde::{Deserialize, Serialize};

/// Default grant length when the user does not pick one.
pub const ADMIN_DEFAULT_DURATION_SECS: u64 = 4 * 60 * 60;

/// A tool call touching more items than this returns a dry-run preview and
/// a confirmation token instead of changing anything.
pub const ADMIN_BULK_THRESHOLD: usize = 10;

/// One live admin grant (device-local; never synced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminGrant {
    pub chat_id: String,
    /// The one Space the grant covers (the chat's own Space).
    pub space_id: String,
    pub granted_at_ms: i64,
    /// `None` = until revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    /// The user let this chat skip bulk-change confirmations (off by default).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_bulk_confirmation: bool,
}

impl AdminGrant {
    pub fn is_live(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_none_or(|at| now_ms < at)
    }
}

/// Params of `GrantAdmin` (IPC-only, user action). Renewing = granting again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantAdminParams {
    pub chat_id: String,
    /// `None` = until revoked; otherwise seconds from now.
    #[serde(default)]
    pub duration_secs: Option<u64>,
    /// "Skip bulk confirmations": changes touching more than
    /// [`ADMIN_BULK_THRESHOLD`] items run without the Required Action
    /// approval. Off by default.
    #[serde(default)]
    pub skip_bulk_confirmation: bool,
}

/// One admin turn's audit entry: every action an admin chat took in one Run
/// (a "sweep"), undoable as a whole. Newest last in the list reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditTurn {
    /// Stable id (`<run id>`), the `AdminUndoTurn` argument.
    pub turn_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub space_id: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    /// "by admin chat <title>": one-line label for Recent Changes.
    pub label: String,
    pub actions: Vec<AdminAuditAction>,
    pub undone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditAction {
    pub at_ms: i64,
    /// e.g. "rename", "move", "archive", "instruction edit".
    pub kind: String,
    /// Human-readable, e.g. `Renamed "A" to "B"`.
    pub summary: String,
}

/// Params of `AdminAuditList` (IPC-only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
}

/// Params of `AdminUndoTurn` (IPC-only, user action).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUndoTurnParams {
    pub turn_id: String,
}

/// Reply of `AdminUndoTurn`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUndoTurnReply {
    pub undone: usize,
    /// Steps that could not be undone (target changed since), by summary.
    pub skipped: Vec<String>,
}

/// `dedupeKey` prefix of the Required Action an admin chat's bulk change
/// waits on. Agents (admin or not) can never resolve one.
pub const ADMIN_BULK_DEDUPE_PREFIX: &str = "admin-bulk:";
