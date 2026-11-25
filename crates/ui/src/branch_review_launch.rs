//! Request construction for reviews of committed work in a bound checkout.

use std::collections::HashMap;

use mast_proto::{
    DiffCheckoutCandidate, ReviewTarget, StartBranchReviewParams,
    StartBranchReviewReply,
};

use crate::state::EngineHandle;

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

/// Ignore stale checkout data and branches without commits beyond the base.
pub(crate) fn target_for_resolved_checkout(
    checkout: &DiffCheckoutCandidate,
    range: &mast_proto::ResolveBranchReviewBaseReply,
) -> Option<ReviewTarget> {
    if range.ahead_count == 0 || checkout.head_sha.as_deref() != Some(range.head_sha.as_str()) {
        return None;
    }
    target_for_checkout(checkout, &range.base_ref, &range.base_sha)
}

/// Reuse an operation id for retries with identical launch parameters.
#[derive(Default)]
pub(crate) struct BranchReviewOperations {
    ids: HashMap<String, String>,
}

impl BranchReviewOperations {
    /// Give `params` the operation id of its request: every field but the
    /// operation id is the request's identity.
    pub(crate) fn assign(&mut self, mut params: StartBranchReviewParams) -> StartBranchReviewParams {
        params.operation_id = String::new();
        let key = serde_json::to_string(&params).unwrap_or_default();
        params.operation_id = self
            .ids
            .entry(key)
            .or_insert_with(|| format!("branch-review-{}", uuid::Uuid::new_v4()))
            .clone();
        params
    }

    pub(crate) fn complete(&mut self, operation_id: &str) {
        self.ids.retain(|_, id| id != operation_id);
    }
}

/// Start the review on the Space's owning engine.
pub(crate) async fn start(
    engine: EngineHandle,
    params: StartBranchReviewParams,
) -> Result<StartBranchReviewReply, String> {
    engine
        .client()
        .start_branch_review(&params)
        .await
        .map_err(|error| error.to_string())
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
    fn resolved_range_requires_current_head_and_ahead_commits() {
        let candidate = checkout();
        let mut range = mast_proto::ResolveBranchReviewBaseReply {
            base_ref: "main".into(),
            base_sha: "base".into(),
            head_sha: "head".into(),
            ahead_count: 1,
            review_range_id: "review-id".into(),
            existing_chat_id: None,
        };
        assert!(target_for_resolved_checkout(&candidate, &range).is_some());
        range.head_sha = "stale".into();
        assert!(target_for_resolved_checkout(&candidate, &range).is_none());
        range.head_sha = "head".into();
        range.ahead_count = 0;
        assert!(target_for_resolved_checkout(&candidate, &range).is_none());
    }

    #[test]
    fn launch_retries_reuse_operation_id_for_same_range() {
        let mut operations = BranchReviewOperations::default();
        let target = target_for_checkout(&checkout(), "main", "base").unwrap();
        let inputs = StartBranchReviewParams {
            space_id: "space".into(),
            work_item_id: Some("item".into()),
            target,
            config: None,
            account_id: None,
            merge_gate: false,
            operation_id: String::new(),
            target_device_id: None,
        };
        let first = operations.assign(inputs.clone());
        let retry = operations.assign(inputs.clone());
        assert_eq!(first, retry);
        assert!(first.operation_id.len() <= 64);
        assert_eq!(first.target.state_key(), "branch:owner/repo#feature");
        let other = operations.assign(StartBranchReviewParams {
            target: target_for_checkout(&checkout(), "main", "another-base").unwrap(),
            ..inputs.clone()
        });
        assert_ne!(first.operation_id, other.operation_id);
        let gate = operations.assign(StartBranchReviewParams {
            merge_gate: true,
            ..inputs.clone()
        });
        assert_ne!(gate.operation_id, first.operation_id);
        assert_eq!(
            operations.assign(StartBranchReviewParams {
                merge_gate: true,
                ..inputs.clone()
            }),
            gate
        );
        operations.complete(&gate.operation_id);
        let new_gate = operations.assign(StartBranchReviewParams {
            merge_gate: true,
            ..inputs
        });
        assert_ne!(new_gate.operation_id, gate.operation_id);
    }
}
