//! Wire types for "Submit review…": one batched GitHub review posted on a
//! human click. Nothing here is reachable from an agent tool.
use serde::{Deserialize, Serialize};

use crate::ReviewSide;

/// The verdict the reviewer picks in the submit dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    /// The `event` value GitHub's reviews endpoint expects.
    pub fn github_event(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// One placed draft, already resolved onto the PR's current diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReviewComment {
    pub draft_id: String,
    pub path: String,
    pub line: u32,
    pub side: ReviewSide,
    /// Set for a range; `line` is then the range's last line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<ReviewSide>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReviewParams {
    /// The local review target the drafts belong to (`pr:owner/repo#n`).
    pub target: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub event: ReviewEvent,
    pub body: String,
    #[serde(default)]
    pub comments: Vec<SubmitReviewComment>,
    /// The head commit the lines were placed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
    /// Stable across retries of one submission; embedded in the review body so
    /// a retry can tell the first POST already landed.
    pub attempt_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmitReviewOutcome {
    /// Posted now.
    Posted,
    /// An earlier attempt had already posted this review; nothing was re-sent.
    AlreadyPosted,
    /// The reviewer has a pending review on the PR (started in the browser);
    /// nothing was sent.
    PendingConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReviewReply {
    pub outcome: SubmitReviewOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<u64>,
    /// The review (or, for a pending conflict, the pull request) on GitHub.
    pub url: String,
}

/// Local record of a review Mast posted, kept per review target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostedReview {
    pub review_id: u64,
    pub url: String,
    pub event: ReviewEvent,
    pub body: String,
    pub posted_at: i64,
    pub comments: Vec<SubmitReviewComment>,
}

/// Marker appended (as an invisible HTML comment) to every review body Mast posts.
pub fn review_marker(attempt_id: &str) -> String {
    format!("<!-- mast-review:{attempt_id} -->")
}
