//! Structured, unpublished review at a particular pull request revision.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSidecar {
    pub schema: String,
    pub pr: ReviewPullRequest,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_hint: Option<String>,
    pub steps: Vec<ReviewStep>,
    pub highlights: Vec<ReviewHighlight>,
    pub questions: Vec<serde_json::Value>,
    pub suggested_comments: Vec<ReviewSuggestedComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPullRequest {
    pub repo: String,
    pub number: u64,
    pub head_sha: String,
    pub base_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStep {
    pub id: String,
    pub title: String,
    pub file: String,
    pub hunks: Vec<ReviewHunk>,
    pub what: String,
    pub why: String,
    pub risk: ReviewRisk,
    pub risk_note: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewHunk {
    pub new_start: u32,
    pub new_end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewRisk {
    None,
    Low,
    Med,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewHighlight {
    pub kind: ReviewHighlightKind,
    pub severity: String,
    pub text: String,
    pub anchor: ReviewAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewHighlightKind {
    Risk,
    MissingTests,
    BehaviourChange,
    Security,
    Migration,
    Perf,
    Style,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAnchor {
    pub file: String,
    pub side: ReviewSide,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSuggestedComment {
    pub anchor: ReviewAnchor,
    pub body: String,
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}
