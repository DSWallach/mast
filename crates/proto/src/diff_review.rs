//! Persisted reading state for a PR or an individual diff target.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReviewState {
    pub target: String,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub viewed_hunks: BTreeSet<String>,
    #[serde(default)]
    pub viewed_files: BTreeSet<String>,
    /// PR head when the reviewer last submitted a review or changed a mark.
    /// Unlike `head_sha` it does not follow the PR head: it is the baseline for
    /// re-review ("changes since you reviewed").
    #[serde(default)]
    pub reviewed_head: Option<String>,
    /// Hunk identities of the diff at `reviewed_head`; current hunks outside
    /// this set changed since the reviewer last looked. Bounded by
    /// `REVIEWED_HUNKS_CAP`.
    #[serde(default)]
    pub reviewed_hunks: BTreeSet<String>,
}

/// Upper bound on the reviewed-hunk snapshot kept per target.
pub const REVIEWED_HUNKS_CAP: usize = 4096;

/// Changes relative to the snapshot the viewer read. Concurrent writers
/// touching different marks do not erase one another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReviewUpdate {
    pub before: DiffReviewState,
    pub after: DiffReviewState,
}

/// A nullable state must be wrapped: the RPC transport cannot carry a bare null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReviewStateReply {
    pub state: Option<DiffReviewState>,
}
