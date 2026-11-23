//! Dispatch records: which worker or review the engine started for a work
//! item at a commit.
//!
//! A record is keyed by `(work item, kind, commit)` and carries an operation
//! id derived from that key alone, never from a Manager chat. A duplicate
//! report, a restart or a successor Manager therefore finds the same record
//! and the same launch instead of making a second one.

use serde::{Deserialize, Serialize};

use crate::CreateChatForWorkItemParams;

/// Longest test evidence kept on a record, in bytes.
pub const MAX_DISPATCH_EVIDENCE_BYTES: usize = 4096;

/// A worker's typed completion, sent beside its `report_to_parent` text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportCompletion {
    /// The commit the worker tested: 7 to 64 hex digits.
    pub tested_sha: String,
    /// What was run and what it showed.
    pub evidence: String,
}

impl ReportCompletion {
    /// The commit lowercased and the evidence trimmed and capped, or why the
    /// completion is unusable.
    pub fn normalized(&self) -> Result<Self, String> {
        let sha = self.tested_sha.trim().to_ascii_lowercase();
        if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("testedSha must be 7 to 64 hex digits".into());
        }
        let evidence = self.evidence.trim();
        if evidence.is_empty() {
            return Err("evidence is required".into());
        }
        let mut end = evidence.len().min(MAX_DISPATCH_EVIDENCE_BYTES);
        while !evidence.is_char_boundary(end) {
            end -= 1;
        }
        Ok(Self { tested_sha: sha, evidence: evidence[..end].to_owned() })
    }
}

/// What the engine started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DispatchKind {
    /// A reviewer on a worker's tested commit.
    Review,
}

/// Progress of one dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DispatchState {
    /// Recorded, launch not yet confirmed. A retry replays `launch`.
    Claimed,
    /// The chat exists.
    Started { chat_id: String },
    /// Nothing was started. Re-evaluated on the next delivery.
    Blocked { reason: String, owner: String },
}

/// One dispatch, durable across restart and Manager handoff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRecord {
    pub kind: DispatchKind,
    pub work_item_id: String,
    pub commit_sha: String,
    /// Derived from `(kind, work item, commit)` only.
    pub operation_id: String,
    /// The worker whose completion triggered this.
    pub worker_chat_id: String,
    /// The Manager that was the reviewer's parent when it was recorded.
    pub manager_chat_id: String,
    pub evidence: String,
    /// The exact launch a retry replays. Absent while blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<CreateChatForWorkItemParams>,
    pub state: DispatchState,
    pub updated_at: i64,
}

/// Operation id for a dispatch of `kind` at `commit_sha` on `work_item_id`.
pub fn dispatch_operation_id(kind: DispatchKind, work_item_id: &str, commit_sha: &str) -> String {
    let kind = match kind {
        DispatchKind::Review => "review",
    };
    crate::work_outline::derive_id(
        &format!("mast-dispatch\u{0}{kind}\u{0}{work_item_id}\u{0}{commit_sha}"),
        "dispatch-operation",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_is_normalized() {
        let ok = ReportCompletion { tested_sha: " ABCDEF1 ".into(), evidence: " ok ".into() };
        let ok = ok.normalized().unwrap();
        assert_eq!((ok.tested_sha.as_str(), ok.evidence.as_str()), ("abcdef1", "ok"));
        for bad in ["", "abc", "xyzxyzxyz", &"a".repeat(65)] {
            let c = ReportCompletion { tested_sha: bad.into(), evidence: "e".into() };
            assert!(c.normalized().is_err(), "{bad}");
        }
        let c = ReportCompletion { tested_sha: "abcdef1".into(), evidence: "  ".into() };
        assert!(c.normalized().is_err());
        let c = ReportCompletion { tested_sha: "abcdef1".into(), evidence: "é".repeat(3000) };
        assert!(c.normalized().unwrap().evidence.len() <= MAX_DISPATCH_EVIDENCE_BYTES);
    }

    #[test]
    fn operation_id_ignores_the_manager() {
        let a = dispatch_operation_id(DispatchKind::Review, "item", "abcdef1");
        assert_eq!(a, dispatch_operation_id(DispatchKind::Review, "item", "abcdef1"));
        assert_ne!(a, dispatch_operation_id(DispatchKind::Review, "item", "abcdef2"));
        assert_ne!(a, dispatch_operation_id(DispatchKind::Review, "other", "abcdef1"));
    }
}
