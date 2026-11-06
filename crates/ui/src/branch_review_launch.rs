//! Request construction for reviews of committed work in a bound checkout.

use std::collections::HashMap;

use mast_proto::{ChatConfig, DiffCheckoutCandidate, ReviewTarget, StartBranchReviewParams};

/// Construct a target only for a bound branch with commits beyond its merge base.
/// The caller resolves the base locally; no refs are fetched by this function.
pub(crate) fn target_for_checkout(
    checkout: &DiffCheckoutCandidate,
    base_ref: &str,
    base_sha: &str,
) -> Option<ReviewTarget> {
    let branch = checkout.branch.as_deref().filter(|s| !s.is_empty())?;
    let head = checkout.head_sha.as_deref().filter(|s| !s.is_empty())?;
    if !checkout.bound
        || checkout.path.is_empty()
        || base_ref.is_empty()
        || base_sha.is_empty()
        || base_sha == head
    {
        return None;
    }
    Some(ReviewTarget::Branch {
        repo: checkout.repo_slug.clone(),
        checkout_path: Some(checkout.path.clone()),
        branch: Some(branch.to_owned()),
        base_ref: Some(base_ref.to_owned()),
        base_sha: Some(base_sha.to_owned()),
        head_sha: head.to_owned(),
    })
}

/// Reuse an operation id for retries with identical launch parameters.
#[derive(Default)]
pub(crate) struct BranchReviewOperations {
    ids: HashMap<String, String>,
}

impl BranchReviewOperations {
    pub(crate) fn params(
        &mut self,
        space_id: String,
        work_item_id: Option<String>,
        target: ReviewTarget,
        config: Option<ChatConfig>,
        target_device_id: Option<String>,
    ) -> StartBranchReviewParams {
        let key = serde_json::json!([space_id, work_item_id, target, config, target_device_id])
            .to_string();
        let operation_id = self
            .ids
            .entry(key)
            .or_insert_with(|| format!("branch-review-{}", uuid::Uuid::new_v4()))
            .clone();
        StartBranchReviewParams {
            space_id,
            work_item_id,
            target,
            config,
            operation_id,
            target_device_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::DiffCheckoutKind;

    fn checkout() -> DiffCheckoutCandidate {
        DiffCheckoutCandidate {
            path: "/repo/worktree".into(),
            kind: DiffCheckoutKind::Worktree,
            repo_name: "repo".into(),
            repo_slug: Some("owner/repo".into()),
            branch: Some("feature".into()),
            head_sha: Some("head".into()),
            bound: true,
            selected: true,
        }
    }

    #[test]
    fn target_requires_bound_branch_and_commits_ahead() {
        let mut candidate = checkout();
        assert!(target_for_checkout(&candidate, "main", "base").is_some());
        assert!(target_for_checkout(&candidate, "main", "head").is_none());
        candidate.bound = false;
        assert!(target_for_checkout(&candidate, "main", "base").is_none());
        candidate.bound = true;
        candidate.branch = None;
        assert!(target_for_checkout(&candidate, "main", "base").is_none());
    }

    #[test]
    fn launch_retries_reuse_operation_id_for_same_range() {
        let mut operations = BranchReviewOperations::default();
        let target = target_for_checkout(&checkout(), "main", "base").unwrap();
        let first = operations.params(
            "space".into(),
            Some("item".into()),
            target.clone(),
            None,
            None,
        );
        let retry = operations.params("space".into(), Some("item".into()), target, None, None);
        assert_eq!(first, retry);
        assert!(first.operation_id.len() <= 64);
        assert_eq!(first.target.state_key(), "branch:owner/repo#feature");
        let other = operations.params(
            "space".into(),
            Some("item".into()),
            target_for_checkout(&checkout(), "main", "another-base").unwrap(),
            None,
            None,
        );
        assert_ne!(first.operation_id, other.operation_id);
    }
}
