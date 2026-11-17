//! Structured, unpublished review at a particular pull request revision.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hash a path and its ordered added/deleted lines, excluding diff context.
/// Tags are the literal `+` or `-`; line text excludes the tag and newline.
pub fn review_hunk_identity<'a>(
    path: &str,
    lines: impl IntoIterator<Item = (u8, &'a str)>,
) -> String {
    let mut hash = Sha256::new();
    hash.update((path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    for (tag, text) in lines {
        hash.update([tag]);
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

/// The committed diff covered by a review. Branch targets do not require a
/// checkout of the head: the checkout only supplies the repository objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReviewTarget {
    #[serde(rename = "pr")]
    PullRequest {
        repo: String,
        number: u64,
        head_sha: String,
        base_sha: String,
    },
    Branch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, alias = "checkout", skip_serializing_if = "Option::is_none")]
        checkout_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_sha: Option<String>,
        head_sha: String,
    },
}

impl ReviewTarget {
    /// Match the existing PR state keys and retain branch state across pushes.
    pub fn state_key(&self) -> String {
        match self {
            Self::PullRequest { repo, number, .. } => {
                format!("pr:{}#{number}", repo.to_lowercase())
            }
            Self::Branch {
                repo,
                checkout_path,
                branch,
                head_sha,
                ..
            } => {
                let name = branch
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(head_sha);
                let repository = repo
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|repo| {
                        if repo.starts_with('/') {
                            repo.to_owned()
                        } else {
                            repo.to_lowercase()
                        }
                    })
                    .or_else(|| checkout_path.clone())
                    .unwrap_or_default();
                format!("branch:{repository}#{name}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSidecar {
    pub schema: String,
    pub pr: ReviewPullRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ReviewTarget>,
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
    /// Stable across republishes so a dismissed suggestion stays dismissed.
    /// Older sidecars did not include an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub anchor: ReviewAnchor,
    pub body: String,
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl ReviewSuggestedComment {
    /// Older sidecars have no id; derive one without requiring the checkout.
    pub fn stable_id(&self) -> String {
        self.id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| {
                let seed = format!(
                    "{}\n{:?}\n{}\n{}",
                    self.anchor.file,
                    self.anchor.side,
                    self.anchor.hunk_hash.as_deref().unwrap_or_default(),
                    self.body
                );
                crate::derive_id(&seed, "review-suggestion")
            })
    }
}

/// A local review comment, keyed by the review target rather than its head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDraft {
    pub id: String,
    pub target: String,
    pub anchor: ReviewAnchor,
    /// Hash of the anchored line, used to locate it within an unchanged hunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line_hash: Option<String>,
    pub body: String,
    pub source: ReviewDraftSource,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewDraftSource {
    User,
    Agent,
}

/// A GitHub review thread as a read-only snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u32>,
    pub side: Option<ReviewSide>,
    pub outdated: bool,
    pub resolved: bool,
    pub comments: Vec<ReviewThreadComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThreadComment {
    pub id: String,
    pub author: String,
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::ReviewTarget;

    #[test]
    fn legacy_suggestion_without_id_still_decodes() {
        let comment: super::ReviewSuggestedComment = serde_json::from_value(serde_json::json!({
            "anchor": {"file":"a.rs", "side":"right", "startLine":1, "endLine":1},
            "body":"Check this", "severity":"low"
        }))
        .unwrap();
        assert_eq!(comment.id, None);
        assert!(serde_json::to_value(comment).unwrap().get("id").is_none());
    }

    #[test]
    fn review_thread_round_trips() {
        let thread = super::ReviewThread {
            id: "thread".into(),
            path: "a.rs".into(),
            line: Some(4),
            side: Some(super::ReviewSide::Right),
            outdated: false,
            resolved: true,
            comments: vec![super::ReviewThreadComment {
                id: "comment".into(),
                author: "reviewer".into(),
                body: "Why?".into(),
            }],
        };
        let json = serde_json::to_value(&thread).unwrap();
        assert_eq!(
            serde_json::from_value::<super::ReviewThread>(json).unwrap(),
            thread
        );
    }

    #[test]
    fn review_target_state_keys_survive_head_changes() {
        let mut target = ReviewTarget::Branch {
            repo: Some("Owner/Repo".into()),
            checkout_path: Some("/checkout".into()),
            branch: Some("feature/one".into()),
            base_ref: Some("main".into()),
            base_sha: None,
            head_sha: "first".into(),
        };
        assert_eq!(target.state_key(), "branch:owner/repo#feature/one");
        if let ReviewTarget::Branch { head_sha, .. } = &mut target {
            *head_sha = "second".into();
        }
        assert_eq!(target.state_key(), "branch:owner/repo#feature/one");
        let decoded: ReviewTarget =
            serde_json::from_value(serde_json::to_value(&target).unwrap()).unwrap();
        assert_eq!(decoded, target);
        if let ReviewTarget::Branch { branch, .. } = &mut target {
            *branch = None;
        }
        assert_eq!(target.state_key(), "branch:owner/repo#second");
        let pr = ReviewTarget::PullRequest {
            repo: "Owner/Repo".into(),
            number: 42,
            head_sha: "head".into(),
            base_sha: "base".into(),
        };
        assert_eq!(pr.state_key(), "pr:owner/repo#42");
    }
}
