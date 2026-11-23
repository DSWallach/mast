//! External runs: work that runs outside a local harness, recorded on a work
//! item. A GitHub Codespace is the first kind. This is the durable, synced
//! record only; what the host can observe about a run (box state, exit code,
//! idle time) is device-local and never lives here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Field limits, in Unicode scalar values.
pub mod limits {
    pub const MAX_RUN_TAG_CHARS: usize = 200;
    pub const MAX_RUN_KIND_CHARS: usize = 64;
    pub const MAX_RUN_NAME_CHARS: usize = 200;
    pub const MAX_RUN_BRANCH_CHARS: usize = 250;
    pub const MAX_RUN_SESSION_ID_CHARS: usize = 200;
    pub const MAX_RUN_PROVIDER_CHARS: usize = 64;
}

/// Kind of a run when the caller names none.
pub const DEFAULT_RUN_KIND: &str = "codespace";

/// How an ended run finished, as reported by an agent or the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExternalRunOutcome {
    Done,
    Failed,
    Abandoned,
}

impl ExternalRunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Row id of an `externalRuns` row: `hex(sha256(workItemId ‖ 0 ‖ runTag))`.
/// Content-derived, so two callers registering the same run converge on one
/// row and a replay is a no-op.
pub fn external_run_id(work_item_id: &str, run_tag: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(work_item_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(run_tag.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One run of a Space, as read from the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRunView {
    /// [`external_run_id`].
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub work_item_id: String,
    /// Open string: a kind this build does not know is kept and shown as-is.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub run_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codespace_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `claude` or `codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_chat_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// The run is supposed to still be going.
    #[serde(default)]
    pub expected_alive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExternalRunOutcome>,
}

/// `upsertExternalRun`. Identity is `(workItemId, runTag)`. Only the fields
/// supplied are written, so two callers updating different fields both
/// survive. Ending a run is `expectedAlive: false` and/or an `outcome`; once
/// a run has ended, `expectedAlive: true` needs `reopen: true`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertExternalRunParams {
    pub work_item_id: String,
    pub run_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codespace_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_alive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExternalRunOutcome>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reopen: bool,
    pub operation_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_id_is_pinned_and_nul_separated() {
        assert_eq!(external_run_id("wi-1", "tag-1").len(), 64);
        assert_eq!(external_run_id("wi-1", "tag-1"), external_run_id("wi-1", "tag-1"));
        assert_ne!(external_run_id("a", "bc"), external_run_id("ab", "c"));
        assert_ne!(external_run_id("wi-1", "tag-1"), external_run_id("wi-2", "tag-1"));
    }

    #[test]
    fn an_unknown_kind_and_missing_fields_decode() {
        let view: ExternalRunView = serde_json::from_value(serde_json::json!({
            "id": "r", "kind": "someFutureKind", "runTag": "t", "futureField": 1
        }))
        .unwrap();
        assert_eq!(view.kind, "someFutureKind");
        assert!(!view.expected_alive);
        assert_eq!(view.ended_at, None);
    }

    #[test]
    fn outcomes_parse_from_the_wire_words() {
        assert_eq!(ExternalRunOutcome::parse("done"), Some(ExternalRunOutcome::Done));
        assert_eq!(ExternalRunOutcome::parse("failed"), Some(ExternalRunOutcome::Failed));
        assert_eq!(ExternalRunOutcome::parse("abandoned"), Some(ExternalRunOutcome::Abandoned));
        assert_eq!(ExternalRunOutcome::parse("nope"), None);
    }
}
