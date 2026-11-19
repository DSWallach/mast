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
    /// The text of the anchored line (end line for a range), used only to check
    /// the placement against GitHub's diff; not sent in the review.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub line_text: String,
    /// The text of a range's first line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub start_text: String,
    /// Every line of the local hunk on the comment's side within the anchored lines and
    /// up to two lines either side of them. GitHub's patch must agree with all of them,
    /// or the file differs from the PR head there and the draft is held back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<ContextLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLine {
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnplacedDraft {
    pub draft_id: String,
    pub reason: String,
}

/// `PrepareReviewSubmit`: validate drafts against GitHub's own diff at the PR head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReviewParams {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    #[serde(default)]
    pub comments: Vec<SubmitReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReviewReply {
    /// The PR's head commit on GitHub; the review is pinned to it.
    pub head_sha: String,
    pub placed: Vec<String>,
    pub unplaced: Vec<UnplacedDraft>,
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
    /// The PR head the lines were validated against (from `PrepareReviewSubmit`).
    /// Required: the engine refuses to post without it, and refuses if GitHub's
    /// head has moved since.
    pub commit_id: String,
    /// Only look for a review this PR's marker already posted; never POST.
    #[serde(default)]
    pub check_only: bool,
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
    /// `check_only` found no review carrying this PR's marker.
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReviewReply {
    pub outcome: SubmitReviewOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<u64>,
    /// The review (or, for a pending conflict, the pull request) on GitHub.
    pub url: String,
    /// Set when the review reached GitHub but the local record could not be
    /// written. The drafts stay and the next attempt finds the review and records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_record_error: Option<String>,
    /// Draft ids that are on GitHub because of this review (all of them after a
    /// post; the ones matched in the existing review after "already posted").
    #[serde(default)]
    pub sent_draft_ids: Vec<String>,
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
    /// The marker nonce this review was posted (or found) under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

/// Marker appended (as an invisible HTML comment) to every review body Mast posts.
pub fn review_marker(attempt_id: &str) -> String {
    format!("<!-- mast-review:{attempt_id} -->")
}
