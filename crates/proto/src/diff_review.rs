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
}
