//! Code → chat provenance: which Mast chat (and turn range) produced a commit,
//! and the blame wire types the code viewer's gutter renders.

use serde::{Deserialize, Serialize};

/// Most commits one HEAD move records.
pub const MAX_PROVENANCE_WALK: usize = 200;

/// How a provenance record came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProvenanceSource {
    /// Recorded when the commit first appeared on a bound worktree's HEAD.
    Recorded,
    /// Copied to a rewritten commit (rebase / amend) by `git patch-id`.
    Remapped,
}

impl ProvenanceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Remapped => "remapped",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recorded" => Some(Self::Recorded),
            "remapped" => Some(Self::Remapped),
            _ => None,
        }
    }
}

/// One `commitProvenance` registry row: commit → chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitProvenance {
    pub sha: String,
    /// `git patch-id --stable` of the commit; empty for empty/merge commits.
    pub patch_id: String,
    pub chat_id: String,
    /// First and last user-message ids (turns) of the chat that ran while the
    /// commit was being made. `None` when the transcript was unavailable.
    pub turn_start: Option<String>,
    pub turn_end: Option<String>,
    /// Epoch millis the record was written.
    pub recorded_at: i64,
    pub source: ProvenanceSource,
    /// The sha this record was copied from, for remapped records.
    pub remapped_from: Option<String>,
}

/// How sure the engine is about the chat behind a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlameChatConfidence {
    Recorded,
    /// Branch-level guess: a chat bound to a branch containing the commit.
    Probably,
}

/// The chat behind a commit, with turn precision when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameChat {
    pub chat_id: String,
    pub title: String,
    pub confidence: BlameChatConfidence,
    /// The turn (user-message id) in the recorded range whose edit calls
    /// touched this path.
    pub turn_id: Option<String>,
}

/// Per-commit facts shared by every line run of that commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    /// Epoch seconds (author time).
    pub author_time: i64,
    pub subject: String,
    /// Path of the file at that commit (differs after renames).
    pub path: String,
    pub previous_path: Option<String>,
    pub chat: Option<BlameChat>,
}

/// A run of consecutive final lines attributed to one commit.
/// `commit == None` means "Not committed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameRun {
    /// 1-based first final line.
    pub start: u32,
    pub len: u32,
    /// Index into [`Blame::commits`].
    pub commit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blame {
    pub revision: String,
    pub commits: Vec<BlameCommit>,
    pub runs: Vec<BlameRun>,
}
