use super::*;

pub(super) fn comment_state_key(
    comments: &[ReviewComment],
    draft: Option<&(String, CommentSide, u32)>,
) -> u64 {
    let mut parts: Vec<String> = comments
        .iter()
        .flat_map(|comment| [comment.id.clone(), comment.body.clone()])
        .collect();
    if let Some((path, side, line)) = draft {
        parts.push(format!("draft:{path}:{}:{line}", side.tag()));
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    hash64(&refs)
}

pub(super) fn hash64(parts: &[&str]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for p in parts {
        p.hash(&mut hasher);
    }
    hasher.finish()
}

/// Stable identity for the changed lines of a hunk. Context and line numbers
/// are excluded so an unrelated edit above the hunk does not invalidate it.
pub(super) fn hunk_identity(path: &str, hunk: &Hunk) -> String {
    mast_proto::review_hunk_identity(
        path,
        hunk.lines.iter().filter_map(|line| {
            let tag = match line.kind {
                LineKind::Add => b'+',
                LineKind::Del => b'-',
                LineKind::Context | LineKind::Meta => return None,
            };
            Some((tag, line.text.as_str()))
        }),
    )
}

pub(super) fn hunk_identities(files: &[FileDiff]) -> Vec<Vec<String>> {
    files
        .iter()
        .map(|file| {
            file.hunks
                .iter()
                .map(|hunk| hunk_identity(&file.path, hunk))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_checkout_requires_matching_repository_branch_and_base() {
        let status = mast_proto::PullRequestStatusSummary {
            pr_ref: mast_proto::PullRequestRef::new("Cofactr", "ui", 7),
            head_ref_name: Some("feature".into()),
            base_ref_name: Some("main".into()),
            ..Default::default()
        };
        let mut checkout = mast_proto::DiffCheckoutCandidate {
            path: "/worktree".into(),
            kind: mast_proto::DiffCheckoutKind::Worktree,
            repo_name: "ui".into(),
            repo_slug: Some("cofactr/ui".into()),
            branch: Some("feature".into()),
            head_sha: Some("abc".into()),
            bound: true,
            selected: true,
        };
        assert!(checkout_matches_pr(&checkout, &status, Some("main")));
        checkout.repo_slug = Some("cofactr/other".into());
        assert!(!checkout_matches_pr(&checkout, &status, Some("main")));
        checkout.repo_slug = Some("cofactr/ui".into());
        checkout.branch = Some("other".into());
        assert!(!checkout_matches_pr(&checkout, &status, Some("main")));
        checkout.branch = Some("feature".into());
        assert!(!checkout_matches_pr(&checkout, &status, Some("other")));
    }

    #[test]
    fn seen_requires_dwell_scroll_and_hunk_end() {
        assert!(!seen_after_traversal(
            Duration::from_millis(1999),
            1,
            2,
            8,
            8
        ));
        assert!(!seen_after_traversal(Duration::from_secs(2), 1, 1, 8, 8));
        assert!(!seen_after_traversal(Duration::from_secs(2), 1, 2, 7, 8));
        assert!(seen_after_traversal(Duration::from_secs(2), 1, 2, 8, 8));
    }

    #[test]
    fn head_change_preserves_matching_hunk_marks() {
        let first = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n");
        let moved =
            parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+new\n");
        let changed =
            parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+changed\n");
        let mut state = mast_proto::DiffReviewState {
            target: "pr:o/r#1".into(),
            head_sha: Some("old".into()),
            ..Default::default()
        };
        state
            .viewed_hunks
            .insert(hunk_identity("a", &first[0].hunks[0]));
        state.viewed_files.insert("a".into());
        assert!(reconcile_review_head(
            &mut state,
            Some("new".into()),
            &hunk_identities(&moved)
        ));
        assert!(
            state
                .viewed_hunks
                .contains(&hunk_identity("a", &moved[0].hunks[0]))
        );
        assert!(
            !state
                .viewed_hunks
                .contains(&hunk_identity("a", &changed[0].hunks[0]))
        );
        assert!(state.viewed_files.is_empty());
    }

    #[test]
    fn changed_hunks_do_not_reappear_viewed_if_they_return() {
        let first = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n");
        let changed =
            parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+changed\n");
        let identity = hunk_identity("a", &first[0].hunks[0]);
        let mut state = mast_proto::DiffReviewState {
            target: "pr:o/r#1".into(),
            head_sha: Some("one".into()),
            ..Default::default()
        };
        state.viewed_hunks.insert(identity.clone());
        assert!(reconcile_review_head(
            &mut state,
            Some("two".into()),
            &hunk_identities(&changed)
        ));
        assert!(!state.viewed_hunks.contains(&identity));
        assert!(reconcile_review_head(
            &mut state,
            Some("three".into()),
            &hunk_identities(&first)
        ));
        assert!(!state.viewed_hunks.contains(&identity));
    }

    #[test]
    fn hunk_identity_survives_line_and_context_changes() {
        let first = parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n",
        );
        let moved = parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -20,2 +20,2 @@\n different context\n-old\n+new\n",
        );
        let changed =
            parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -20 +20 @@\n-old\n+other\n");
        let id = hunk_identity("a", &first[0].hunks[0]);
        assert_eq!(id, hunk_identity("a", &moved[0].hunks[0]));
        assert_ne!(id, hunk_identity("a", &changed[0].hunks[0]));
        assert_ne!(id, hunk_identity("b", &first[0].hunks[0]));
    }
}

fn reconcile_review_head(
    state: &mut mast_proto::DiffReviewState,
    head: Option<String>,
    identities: &[Vec<String>],
) -> bool {
    if head.is_none() || state.head_sha == head {
        return false;
    }
    let present: std::collections::HashSet<&str> =
        identities.iter().flatten().map(String::as_str).collect();
    state
        .viewed_hunks
        .retain(|identity| present.contains(identity.as_str()));
    state.head_sha = head;
    state.viewed_files.clear();
    true
}

fn checkout_matches_pr(
    checkout: &mast_proto::DiffCheckoutCandidate,
    status: &mast_proto::PullRequestStatusSummary,
    base: Option<&str>,
) -> bool {
    let slug = format!("{}/{}", status.pr_ref.owner, status.pr_ref.repo).to_lowercase();
    checkout.repo_slug.as_deref() == Some(slug.as_str())
        && checkout
            .branch
            .as_deref()
            .zip(status.head_ref_name.as_deref())
            .is_some_and(|(actual, head)| actual == head)
        && base
            .zip(status.base_ref_name.as_deref())
            .is_some_and(|(actual, expected)| actual == expected)
}

impl Changes {
    fn review_identity(&self, cx: &App) -> Option<(String, Option<String>)> {
        if let Some(owner) = &self.owner {
            if let Some(target @ mast_proto::ReviewTarget::Branch { checkout_path: Some(path), branch, head_sha, .. }) = &self.walkthrough_target
                && owner.cwd.as_deref() == Some(path.as_str())
            {
                let selected = self.state.read(cx).selected_execution_member(&owner.chat_id);
                let head = owner.range.as_ref().and_then(|range| range.head_sha.clone()).or_else(|| {
                    selected.as_ref().filter(|member| member.checkout_path == *path).and_then(|member| member.head_sha.clone())
                });
                let matches = match &owner.range {
                    Some(range) => self.scope == DiffScope::Range && branch.as_deref().map_or_else(
                        || head.as_deref().is_some_and(|head| head.eq_ignore_ascii_case(head_sha)),
                        |branch| range.head_branch == branch),
                    None => self.scope == DiffScope::Branch && branch.as_deref().map_or_else(
                        || head.as_deref().is_some_and(|head| head.eq_ignore_ascii_case(head_sha)),
                        |branch| owner.branch.as_deref() == Some(branch)),
                };
                if matches { return Some((target.state_key(), head)); }
            }
            if let Some(range) = &owner.range {
                if let Some(pr) = range
                    .pr_url
                    .as_deref()
                    .and_then(crate::chat_work_context::parse_pr_url)
                {
                    return Some((
                        format!("pr:{}", pr.external_id().to_lowercase()),
                        range.head_sha.clone(),
                    ));
                }
                if self.scope == DiffScope::Range {
                    let path = owner.cwd.as_ref()?;
                    let repo = self.review_branch_repositories.get(&(owner.chat_id.clone(), path.clone()))?;
                    let target = mast_proto::ReviewTarget::Branch {
                        repo: Some(repo.clone()), checkout_path: Some(path.clone()),
                        branch: Some(range.head_branch.clone()), base_ref: Some(range.base_ref.clone()),
                        base_sha: None, head_sha: range.head_sha.clone().unwrap_or_default(),
                    };
                    return Some((target.state_key(), range.head_sha.clone()));
                }
            }
            if let Some(pr) = &self.review_pr
                && owner.range.is_none()
                && self.scope == DiffScope::Branch
                && self.review_checkout_matches == Some(true)
            {
                let head = self
                    .review_head
                    .as_ref()
                    .filter(|(path, _)| Some(path.as_str()) == owner.cwd.as_deref())
                    .map(|(_, sha)| sha.clone())
                    .or_else(|| {
                        self.state
                            .read(cx)
                            .selected_execution_member(&owner.chat_id)
                            .and_then(|member| member.head_sha.clone())
                    });
                return Some((pr.clone(), head));
            }
            let path = owner.cwd.as_deref()?;
            let target = match &owner.range {
                Some(range) => format!("ref:{}:{}:{}", owner.device_id, path, range.head_ref),
                None => format!("diff:{}:{}", owner.device_id, path),
            };
            return Some((target, None));
        }
        self.pinned_cwd
            .as_deref()
            .map(|path| (format!("diff:local:{path}"), None))
    }

    /// A PR range names its source branch even when the PR status cache is cold.
    fn branch_target_for_pr(&self, target: &str) -> Option<String> {
        let pr = target.strip_prefix("pr:").and_then(mast_proto::PullRequestRef::parse_external_id)?;
        let owner = self.owner.as_ref()?;
        let branch = match &owner.range {
            Some(range) if self.scope == DiffScope::Range
                && (range.head_ref == format!("refs/remotes/origin/{}", range.head_branch)
                    || range.head_ref == format!("refs/heads/{}", range.head_branch))
                && range.pr_url.as_deref()
                .and_then(crate::chat_work_context::parse_pr_url)
                .is_some_and(|candidate| candidate.external_id().eq_ignore_ascii_case(&pr.external_id())) => &range.head_branch,
            None if self.scope == DiffScope::Branch && self.review_checkout_matches == Some(true) => owner.branch.as_ref()?,
            _ => return None,
        };
        if branch.is_empty() { return None; }
        Some(format!("branch:{}/{}#{branch}", pr.owner.to_lowercase(), pr.repo.to_lowercase()))
    }

    /// Resolve launcher reviews and chats attached to a PR review item.
    fn linked_review_pr(&self, cx: &App) -> Option<String> {
        let owner = self.owner.as_ref()?;
        let status = self.review_link_status.as_ref()?;
        let state = self.state.read(cx);
        let space_id = state
            .chats
            .iter()
            .find(|chat| chat.id == owner.chat_id)
            .and_then(|chat| chat.space_id.as_deref());
        let load = space_id.map(|space_id| state.work_outline(space_id));
        let snapshot = load
            .as_ref()
            .and_then(|load| load.snapshot())
            .map(|snapshot| snapshot.as_ref());
        let prs = crate::chat_work_context::chat_prs(status);
        let context = crate::chat_work_context::resolve(&crate::chat_work_context::ResolveInput {
            chat_id: &owner.chat_id,
            snapshot,
            chat_prs: &prs,
            artifacts: None,
        })?;
        match context.variant {
            crate::chat_work_context::Variant::PrReview(review) => review
                .prs
                .first()
                .map(|pr| format!("pr:{}", pr.external_id().to_lowercase())),
            _ => None,
        }
    }

    pub(super) fn ensure_review_state(&mut self, cx: &mut Context<Self>) {
        // A selected ref has its own identity; chat linkage must not delay it.
        if let Some(chat_id) = self
            .owner
            .as_ref()
            .filter(|owner| owner.range.is_none() && self.scope == DiffScope::Branch)
            .map(|owner| owner.chat_id.clone())
        {
            if self.review_link_chat.as_deref() != Some(&chat_id) {
                if self.review_error.is_some() {
                    return;
                }
                self.review_link_chat = Some(chat_id.clone());
                self.review_target = None;
                self.review_state = None;
                self.review_pr = None;
                self.review_link_status = None;
                let task = self.state.update(cx, |state, cx| {
                    state.chat_diff_rpc(
                        methods::CHAT_LINK_STATUS,
                        serde_json::json!({"chatId": chat_id}),
                        cx,
                    )
                });
                self.review_link_task = Some(cx.spawn(async move |this, cx| {
                    let result = task.await.and_then(|value| {
                        serde_json::from_value::<mast_engine::pr_ticket_cache::ChatLinkStatus>(
                            value,
                        )
                        .map_err(|error| format!("Malformed chat PR links: {error}"))
                    });
                    this.update(cx, |this, cx| {
                        if this.review_link_chat.as_deref() != Some(&chat_id) {
                            return;
                        }
                        this.review_link_task = None;
                        match result {
                            Ok(status) => {
                                this.review_link_status = Some(status);
                                this.ensure_review_state(cx);
                            }
                            Err(error) => {
                                this.review_link_chat = None;
                                this.review_error =
                                    Some(format!("Could not identify the review target: {error}"));
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }));
                return;
            }
            if self.review_link_task.is_some() {
                return;
            }
        }
        if self.review_link_status.is_some()
            && self.scope == DiffScope::Branch
            && self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.range.is_none())
        {
            let pr = self.linked_review_pr(cx);
            if self.review_pr != pr {
                self.review_pr = pr;
                self.review_head_for = None;
                self.review_head = None;
                self.review_checkout_matches = None;
            }
        }
        if let Some(pr) = self
            .review_pr
            .as_deref()
            .and_then(|key| key.strip_prefix("pr:"))
            .and_then(mast_proto::PullRequestRef::parse_external_id)
            && self.scope == DiffScope::Branch
            && self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.range.is_none())
        {
            let Some((chat_id, path, revision)) = self.owner.as_ref().and_then(|owner| {
                Some((
                    owner.chat_id.clone(),
                    owner.cwd.clone()?,
                    self.active_diff(cx).map(|diff| self.parse_key(&diff))?,
                ))
            }) else {
                return;
            };
            let lookup_key = format!("{}\0{path}\0{revision}", pr.external_id().to_lowercase());
            if self.review_head_for.as_deref() != Some(&lookup_key) {
                if self.review_error.is_some() {
                    return;
                }
                self.review_head_for = Some(lookup_key.clone());
                self.review_checkout_matches = None;
                self.review_target = None;
                self.review_state = None;
                let base = self.base_ref.clone();
                let expected_slug = format!("{}/{}", pr.owner, pr.repo).to_lowercase();
                let expected_pr = pr.external_id().to_lowercase();
                let (checkout_task, status_task) = self.state.update(cx, |state, cx| {
                    let checkout = state.list_chat_diff_checkouts(&chat_id, cx);
                    let status = state.chat_diff_rpc(methods::PR_STATUS, serde_json::json!({"owner": pr.owner, "repo": pr.repo, "number": pr.number}), cx);
                    (checkout, status)
                });
                self.review_head_task = Some(cx.spawn(async move |this, cx| {
                    let result = async {
                        let checkouts = checkout_task.await?;
                        let checkout = checkouts
                            .into_iter()
                            .find(|checkout| checkout.path == path)
                            .ok_or_else(|| "Selected checkout is unavailable".to_string())?;
                        let Some(slug) = checkout.repo_slug.as_deref() else {
                            return Err(
                                "Selected checkout has no confirmed GitHub origin".to_string()
                            );
                        };
                        if slug != expected_slug {
                            return Ok((checkout.head_sha, false));
                        }
                        if checkout.head_sha.is_none() {
                            return Err("Selected checkout has no confirmed head".to_string());
                        }
                        let status: mast_proto::PullRequestStatusSummary =
                            serde_json::from_value(status_task.await?)
                                .map_err(|error| error.to_string())?;
                        if status.pr_ref.external_id().to_lowercase() != expected_pr {
                            return Err("PR status did not match the requested review".to_string());
                        }
                        if status.head_ref_name.is_none()
                            || status.base_ref_name.is_none()
                            || base.is_none()
                        {
                            return Err("PR head or base branch is unavailable".to_string());
                        }
                        let matches = checkout_matches_pr(&checkout, &status, base.as_deref());
                        Ok::<_, String>((checkout.head_sha, matches))
                    }
                    .await;
                    this.update(cx, |this, cx| {
                        if this.review_head_for.as_deref() != Some(&lookup_key) {
                            return;
                        }
                        this.review_head_task = None;
                        match result {
                            Ok((head, matches)) => {
                                this.review_checkout_matches = Some(matches);
                                this.review_head = head.map(|sha| (path, sha));
                                this.ensure_review_state(cx);
                            }
                            Err(error) => {
                                this.review_head_for = None;
                                this.review_error =
                                    Some(format!("Could not confirm the PR checkout: {error}"));
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }));
                return;
            }
            if self.review_head_task.is_some() {
                return;
            }
        }
        if self.review_identity(cx).is_none()
            && self.scope == DiffScope::Range
            && let Some((chat_id, path)) = self.owner.as_ref().and_then(|owner| {
                owner.range.as_ref()?;
                Some((owner.chat_id.clone(), owner.cwd.clone()?))
            })
        {
            let key = (chat_id.clone(), path.clone());
            if self.review_error.is_some() {
                if self.review_branch_lookup.as_ref() == Some(&key) { return; }
                self.review_error = None;
            }
            if self.review_branch_lookup.as_ref() == Some(&key) && self.review_branch_lookup_task.is_some() { return; }
            self.review_target = None;
            self.review_state = None;
            self.review_task = None;
            self.review_branch_lookup = Some(key.clone());
            let task = self.state.update(cx, |state, cx| state.list_chat_diff_checkouts(&chat_id, cx));
            self.review_branch_lookup_task = Some(cx.spawn(async move |this, cx| {
                let result = task.await.and_then(|checkouts| checkouts.into_iter()
                    .find(|checkout| checkout.path == path)
                    .and_then(|checkout| checkout.repo_slug)
                    .filter(|repo| !repo.is_empty())
                    .ok_or_else(|| "Could not identify the branch repository for review state".to_owned()));
                this.update(cx, |this, cx| {
                    if this.review_branch_lookup.as_ref() != Some(&key) { return; }
                    this.review_branch_lookup = None;
                    this.review_branch_lookup_task = None;
                    match result {
                        Ok(repo) => { this.review_branch_repositories.insert(key, repo); this.ensure_review_state(cx); }
                        Err(error) => {
                            let still_needed = this.scope == DiffScope::Range && this.owner.as_ref().is_some_and(|owner|
                                owner.chat_id == key.0 && owner.cwd.as_deref() == Some(key.1.as_str()) && owner.range.is_some())
                                && this.review_identity(cx).is_none();
                            if still_needed { this.review_branch_lookup = Some(key); this.review_error = Some(error); }
                        }
                    }
                    cx.notify();
                }).ok();
            }));
            return;
        }
        let Some((target, head_sha)) = self.review_identity(cx) else {
            return;
        };
        if self.review_target.as_deref() == Some(&target) {
            self.refresh_review_status(cx);
            if self.review_closed {
                return;
            }
            self.reconcile_current_review_head(head_sha, cx);
            return;
        }
        self.review_target = Some(target.clone());
        self.review_status_task = None;
        self.review_status_checked = None;
        self.review_status_base = None;
        self.review_closed = false;
        self.refresh_review_status(cx);
        self.seen_candidates.clear();
        self.seen_hunks.clear();
        self.review_error = None;
        if let Some(state) = self.review_pending.get(&target).cloned() {
            self.review_state = Some(state);
            self.reconcile_current_review_head(head_sha, cx);
            self.save_review_state(cx);
            return;
        }
        self.review_state = None;
        let branch_target = self.branch_target_for_pr(&target);
        let mut params = serde_json::json!({"target": target});
        if let Some(branch_target) = branch_target {
            params["branchTarget"] = serde_json::json!(branch_target);
        }
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(methods::GET_DIFF_REVIEW_STATE, params, cx)
        });
        self.review_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<mast_proto::DiffReviewStateReply>(value)
                    .map(|reply| reply.state)
                    .map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) || this.review_closed {
                    return;
                }
                this.review_task = None;
                match result {
                    Ok(state) => {
                        let state = state.unwrap_or_else(|| mast_proto::DiffReviewState {
                            target: target.clone(),
                            ..Default::default()
                        });
                        this.review_acknowledged
                            .insert(target.clone(), state.clone());
                        this.review_state = Some(state);
                        // Re-read the current head: it may have moved while the row loaded.
                        let head = this.review_identity(cx).and_then(|(_, head)| head);
                        this.reconcile_current_review_head(head, cx);
                        this.save_review_state(cx);
                    }
                    Err(error) => this.review_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Prune only after the complete patch for the current head has parsed.
    /// Otherwise a slow parse (or a truncated patch) would erase valid marks.
    fn reconcile_current_review_head(&mut self, head: Option<String>, cx: &mut Context<Self>) {
        if head.is_none()
            || self
                .review_state
                .as_ref()
                .is_none_or(|state| state.head_sha.as_deref() == head.as_deref())
            || !matches!(self.scope, DiffScope::Branch | DiffScope::Range)
            || self.scoped_inflight.is_some()
            || !self
                .review_target
                .as_deref()
                .is_some_and(|target| target.starts_with("pr:"))
        {
            return;
        }
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let diff_base = match self.scope {
            DiffScope::Branch => self.base_ref.as_deref(),
            DiffScope::Range => self
                .owner
                .as_ref()
                .and_then(|owner| owner.range.as_ref())
                .map(|range| range.base_branch.as_str()),
            _ => None,
        };
        if diff_base.is_none() || diff_base != self.review_status_base.as_deref() {
            return;
        }
        if parsed.truncated
            || self
                .active_diff(cx)
                .is_none_or(|diff| self.parse_key(&diff) != parsed.key)
        {
            return;
        }
        let Some(state) = self.review_state.as_mut() else {
            return;
        };
        if reconcile_review_head(state, head, &parsed.hunk_identities) {
            self.review_pending
                .insert(state.target.clone(), state.clone());
            self.save_review_state(cx);
        }
    }

    fn refresh_review_status(&mut self, cx: &mut Context<Self>) {
        if self.review_status_task.is_some()
            || self
                .review_status_checked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(120))
        {
            return;
        }
        let Some(target) = self.review_target.clone() else {
            return;
        };
        let Some(pr) = target
            .strip_prefix("pr:")
            .and_then(mast_proto::PullRequestRef::parse_external_id)
        else {
            return;
        };
        self.review_status_checked = Some(std::time::Instant::now());
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::PR_STATUS,
                serde_json::json!({"owner": pr.owner, "repo": pr.repo, "number": pr.number}),
                cx,
            )
        });
        self.review_status_task = Some(cx.spawn(async move |this, cx| {
            let status = task.await.ok().and_then(|value| {
                serde_json::from_value::<mast_proto::PullRequestStatusSummary>(value).ok()
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.review_status_task = None;
                if let Some(status) = status {
                    this.review_status_base = status.base_ref_name.clone();
                    let closed = status.state.is_finished();
                    if closed {
                        this.review_closed = true;
                        this.review_state = None;
                        this.review_task = None;
                        this.review_pending.remove(&target);
                        this.review_acknowledged.remove(&target);
                        this.review_save_task = None;
                        this.save_review_state(cx);
                    } else if this.review_closed {
                        this.review_closed = false;
                        this.review_target = None;
                        this.ensure_review_state(cx);
                    } else {
                        this.ensure_review_state(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn retry_review_state(&mut self, cx: &mut Context<Self>) {
        let stale = self.review_error.as_deref().is_some_and(|error| {
            error.contains("review head changed") || error.contains("review state was cleared")
        });
        self.review_error = None;
        if stale {
            if let Some(target) = &self.review_target {
                self.review_pending.remove(target);
                self.review_acknowledged.remove(target);
            }
            self.review_state = None;
        }
        if self.review_state.is_none() {
            self.review_target = None;
            self.ensure_review_state(cx);
        } else {
            self.save_review_state(cx);
        }
        cx.notify();
    }

    fn save_review_state(&mut self, cx: &mut Context<Self>) {
        if self.review_save_task.is_some() || self.review_error.is_some() {
            return;
        }
        let Some(snapshot) = self.review_pending.values().next().cloned() else {
            return;
        };
        let before = self
            .review_acknowledged
            .get(&snapshot.target)
            .cloned()
            .unwrap_or_else(|| mast_proto::DiffReviewState {
                target: snapshot.target.clone(),
                ..Default::default()
            });
        let update = mast_proto::DiffReviewUpdate {
            before,
            after: snapshot.clone(),
        };
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::SET_DIFF_REVIEW_STATE,
                serde_json::to_value(&update).unwrap_or_default(),
                cx,
            )
        });
        self.review_save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.review_save_task = None;
                match result {
                    Ok(_) => {
                        this.review_acknowledged
                            .insert(snapshot.target.clone(), snapshot.clone());
                        if this.review_pending.get(&snapshot.target) == Some(&snapshot) {
                            this.review_pending.remove(&snapshot.target);
                        }
                        this.save_review_state(cx);
                    }
                    Err(error) => this.review_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn toggle_review_hunk(&mut self, file: usize, hunk: usize, cx: &mut Context<Self>) {
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let Some(file_diff) = parsed.files.get(file) else {
            return;
        };
        let Some(identity) = parsed
            .hunk_identities
            .get(file)
            .and_then(|items| items.get(hunk))
            .cloned()
        else {
            return;
        };
        let Some(state) = self.review_state.as_mut() else {
            return;
        };
        if !state.viewed_hunks.remove(&identity) {
            state.viewed_hunks.insert(identity);
        }
        state.viewed_files.remove(&file_diff.path);
        self.review_pending
            .insert(state.target.clone(), state.clone());
        self.save_review_state(cx);
        cx.notify();
    }

    pub(super) fn toggle_review_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(index))
        else {
            return;
        };
        let Some(state) = self.review_state.as_mut() else {
            return;
        };
        let identities = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.hunk_identities.get(index))
            .cloned()
            .unwrap_or_default();
        let viewed = if identities.is_empty() {
            state.viewed_files.contains(&file.path)
        } else {
            identities.iter().all(|id| state.viewed_hunks.contains(id))
        };
        if viewed {
            for id in identities {
                state.viewed_hunks.remove(&id);
            }
            state.viewed_files.remove(&file.path);
        } else {
            state.viewed_hunks.extend(identities);
            state.viewed_files.insert(file.path.clone());
        }
        self.review_pending
            .insert(state.target.clone(), state.clone());
        self.save_review_state(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    #[gpui::test]
    fn branch_ranges_and_cold_pr_reads_share_the_normalized_branch_key(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
        let captured = calls.clone();
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, params| {
                captured.lock().unwrap().push((method.into(), params));
                match method {
                    methods::LIST_CHAT_DIFF_CHECKOUTS => Ok(serde_json::json!({"candidates":[{
                        "path":"/repo", "kind":"own", "repoName":"r", "repoSlug":"O/R",
                        "branch":"main", "headSha":"abc", "bound":false, "selected":true
                    }]})),
                    methods::GET_DIFF_REVIEW_STATE => Ok(serde_json::json!({"state":null})),
                    _ => Err("not available in test".into()),
                }
            }));
            let state = cx.new(|_| state);
            let owner = ChangesTarget { chat_id: "chat".into(), device_id: "device".into(), cwd: Some("/repo".into()), checkout_id: None, branch: None, unconfirmed: None, exact_checkout: false,
                range: Some(crate::work_outline::execution::RefRange { label: "branch".into(), head_branch: "feature".into(), head_ref: "refs/remotes/origin/feature".into(), head_sha: Some("abc".into()), pr_url: None, base_branch: "main".into(), base_ref: "refs/remotes/origin/main".into() }) };
            let mut changes = Changes::with_owner(state, Some(owner), cx);
            changes.scope = DiffScope::Range;
            changes
        });
        changes.update(vcx, |changes, cx| changes.ensure_review_state(cx));
        vcx.run_until_parked();
        assert!(calls.lock().unwrap().iter().any(|(method, params)| method == methods::GET_DIFF_REVIEW_STATE && params["target"] == "branch:o/r#feature" && params.get("branchTarget").is_none()));
        changes.update(vcx, |changes, cx| {
            changes.owner.as_mut().unwrap().range.as_mut().unwrap().pr_url = Some("https://github.com/o/r/pull/7".into());
            changes.ensure_review_state(cx);
        });
        vcx.run_until_parked();
        assert!(calls.lock().unwrap().iter().any(|(method, params)| method == methods::GET_DIFF_REVIEW_STATE && params["target"] == "pr:o/r#7" && params["branchTarget"] == "branch:o/r#feature"));
        changes.update(vcx, |changes, _| {
            changes.owner.as_mut().unwrap().range.as_mut().unwrap().head_ref = "refs/pull/7/head".into();
            assert!(changes.branch_target_for_pr("pr:o/r#7").is_none());
        });
    }

    #[gpui::test]
    fn unknown_repository_does_not_write_a_guessed_branch_key(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = calls.clone();
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, _| {
                captured.lock().unwrap().push(method.into());
                Ok(serde_json::json!({"candidates":[{"path":"/repo","kind":"own","repoName":"r","bound":false,"selected":true}]}))
            }));
            let state = cx.new(|_| state);
            let owner = ChangesTarget { chat_id: "chat".into(), device_id: "device".into(), cwd: Some("/repo".into()), checkout_id: None, branch: None, unconfirmed: None, exact_checkout: false,
                range: Some(crate::work_outline::execution::RefRange { label: "branch".into(), head_branch: "feature".into(), head_ref: "refs/heads/feature".into(), head_sha: Some("abc".into()), pr_url: None, base_branch: "main".into(), base_ref: "refs/heads/main".into() }) };
            let mut changes = Changes::with_owner(state, Some(owner), cx);
            changes.scope = DiffScope::Range;
            changes
        });
        changes.update(vcx, |changes, cx| changes.ensure_review_state(cx));
        vcx.run_until_parked();
        assert!(changes.read_with(vcx, |changes, _| changes.review_error.is_some()));
        assert!(!calls.lock().unwrap().iter().any(|method| method == methods::GET_DIFF_REVIEW_STATE));
    }

    #[gpui::test]
    fn branch_review_uses_target_key_only_for_matching_checkout_and_branch(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            let state = cx.new(|_| AppState::new());
            let changes = cx.new(|cx| {
                let owner = ChangesTarget { chat_id: "chat".into(), device_id: "device".into(), cwd: Some("/repo".into()), checkout_id: None, branch: Some("feature".into()), unconfirmed: None, exact_checkout: false, range: None };
                let mut changes = Changes::with_owner(state, Some(owner), cx);
                changes.scope = DiffScope::Branch;
                changes.walkthrough_target = Some(mast_proto::ReviewTarget::Branch { repo: Some("O/R".into()), checkout_path: Some("/repo".into()), branch: Some("feature".into()), base_ref: None, base_sha: None, head_sha: "abc".into() });
                changes
            });
            changes.update(cx, |changes, cx| {
                assert_eq!(changes.review_identity(cx).unwrap().0, "branch:o/r#feature");
                changes.owner.as_mut().unwrap().branch = Some("other".into());
                assert_eq!(changes.review_identity(cx).unwrap().0, "diff:device:/repo");
                if let Some(mast_proto::ReviewTarget::Branch { branch, .. }) = &mut changes.walkthrough_target { *branch = None; }
                changes.scope = DiffScope::Range;
                changes.owner.as_mut().unwrap().range = Some(crate::work_outline::execution::RefRange {
                    label: "ref".into(), head_branch: "other".into(), head_ref: "refs/heads/other".into(),
                    head_sha: Some("abc".into()), pr_url: None, base_branch: "main".into(), base_ref: "refs/heads/main".into(),
                });
                assert_eq!(changes.review_identity(cx).unwrap().0, "branch:o/r#abc");
                changes.owner.as_mut().unwrap().range.as_mut().unwrap().head_sha = Some("different".into());
                assert!(changes.review_identity(cx).is_none());
            });
        });
    }

    #[gpui::test]
    fn worktree_and_pr_branch_have_distinct_review_targets(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            let state = cx.new(|_| AppState::new());
            let changes = cx.new(|cx| {
                let owner = ChangesTarget {
                    chat_id: "chat".into(),
                    device_id: "device".into(),
                    cwd: Some("/repo".into()),
                    checkout_id: None,
                    branch: None,
                    unconfirmed: None,
                    exact_checkout: false,
                    range: None,
                };
                let mut changes = Changes::with_owner(state, Some(owner), cx);
                changes.review_link_status = Some(mast_engine::pr_ticket_cache::ChatLinkStatus {
                    pr_links: vec![mast_engine::pr_ticket_cache::PrLinkStatus {
                        url: "https://github.com/o/r/pull/1".into(),
                        source: Some(mast_proto::ChatLinkSource::ReviewLauncher),
                        detail: None,
                    }],
                    ..Default::default()
                });
                changes
            });
            changes.update(cx, |changes, cx| {
                changes.review_pr = changes.linked_review_pr(cx);
                changes.review_checkout_matches = Some(true);
                assert_eq!(changes.review_identity(cx).unwrap().0, "diff:device:/repo");
                changes.scope = DiffScope::Branch;
                assert_eq!(changes.review_identity(cx).unwrap().0, "pr:o/r#1");
                changes.review_checkout_matches = Some(false);
                assert_eq!(changes.review_identity(cx).unwrap().0, "diff:device:/repo");
            });
        });
    }

    #[gpui::test]
    fn head_reconciliation_waits_for_the_current_complete_diff(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Changes::new(state, cx)
        });
        vcx.update(|_, cx| {
            changes.update(cx, |changes, cx| {
                let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
                let files = parse_patch(patch);
                let identity = hunk_identity("a", &files[0].hunks[0]);
                let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
                let horizontal = files.iter().map(FileHorizontalState::new).collect();
                changes.scope = DiffScope::Branch;
                changes.base_ref = Some("main".into());
                changes.scoped = Some(CheckoutDiff {
                    checkout_id: "checkout".into(),
                    device_id: "device".into(),
                    cwd: "/repo".into(),
                    patch: patch.into(),
                    files: Vec::new(),
                    submodules: Vec::new(),
                    additions: 1,
                    deletions: 1,
                    truncated: false,
                    checksum: "current".into(),
                    updated_at: chrono::Utc::now(),
                });
                changes.review_target = Some("pr:o/r#1".into());
                changes.review_status_base = Some("main".into());
                let mut state = mast_proto::DiffReviewState {
                    target: "pr:o/r#1".into(),
                    head_sha: Some("old".into()),
                    ..Default::default()
                };
                state.viewed_hunks.insert(identity.clone());
                state.viewed_hunks.insert("removed".into());
                changes.review_state = Some(state);
                changes.parsed = Some(ParsedDiff {
                    key: "stale".into(),
                    truncated: false,
                    additions: 1,
                    deletions: 1,
                    file_count: 1,
                    horizontal,
                    hunk_identities: Arc::new(hunk_identities(&files)),
                    files: Arc::new(files),
                    origins: Arc::new(origins),
                    submodules: Arc::new(submodules),
                    layout: Arc::new(layout),
                });
                changes.reconcile_current_review_head(Some("new".into()), cx);
                assert_eq!(
                    changes.review_state.as_ref().unwrap().head_sha.as_deref(),
                    Some("old")
                );
                let key = changes.parse_key(changes.scoped.as_ref().unwrap());
                changes.parsed.as_mut().unwrap().key = key;
                changes.parsed.as_mut().unwrap().truncated = true;
                changes.reconcile_current_review_head(Some("new".into()), cx);
                assert_eq!(
                    changes.review_state.as_ref().unwrap().head_sha.as_deref(),
                    Some("old")
                );
                changes.parsed.as_mut().unwrap().truncated = false;
                changes.reconcile_current_review_head(Some("new".into()), cx);
                let state = changes.review_state.as_ref().unwrap();
                assert_eq!(state.head_sha.as_deref(), Some("new"));
                assert!(state.viewed_hunks.contains(&identity));
                assert!(!state.viewed_hunks.contains("removed"));
            })
        });
    }

    #[gpui::test]
    fn file_and_hunk_marks_are_manual_and_shared(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Changes::new(state, cx)
        });
        vcx.update(|_, cx| changes.update(cx, |changes, cx| {
            let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n@@ -10 +10 @@\n-x\n+y\n";
            let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
            let first = hunk_identity("a", &files[0].hunks[0]);
            let second = hunk_identity("a", &files[0].hunks[1]);
            let horizontal = files.iter().map(FileHorizontalState::new).collect();
            changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 2, deletions: 2, file_count: 1, horizontal, hunk_identities: Arc::new(hunk_identities(&files)), files: Arc::new(files), origins: Arc::new(origins), submodules: Arc::new(submodules), layout: Arc::new(layout) });
            changes.review_state = Some(mast_proto::DiffReviewState { target: "test".into(), ..Default::default() });
            changes.toggle_review_hunk(0, 0, cx);
            let state = changes.review_state.as_ref().unwrap();
            assert!(state.viewed_hunks.contains(&first));
            assert!(!state.viewed_hunks.contains(&second));
            changes.toggle_review_file(0, cx);
            let state = changes.review_state.as_ref().unwrap();
            assert!(state.viewed_hunks.contains(&first));
            assert!(state.viewed_hunks.contains(&second));
            changes.toggle_review_file(0, cx);
            assert!(changes.review_state.as_ref().unwrap().viewed_hunks.is_empty());
        }));
    }
}

fn seen_after_traversal(
    elapsed: Duration,
    start_epoch: u64,
    current_epoch: u64,
    last_visible: usize,
    last_row: usize,
) -> bool {
    elapsed >= Duration::from_secs(2) && start_epoch != current_epoch && last_visible >= last_row
}

pub(super) struct SeenCandidate {
    started: std::time::Instant,
    scroll_epoch: u64,
    last_row: usize,
    reached_end: bool,
}

impl Changes {
    pub(super) fn ensure_seen_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.seen_last_render = std::time::Instant::now();
        if self.seen_task.is_some() {
            return;
        }
        self.seen_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        if this.seen_last_render.elapsed() > Duration::from_millis(1500) {
                            this.seen_candidates.clear();
                            this.seen_task = None;
                            return false;
                        }
                        if window.is_window_active() {
                            this.refresh_review_status(cx);
                            this.sample_seen_hunks();
                            cx.notify();
                        } else {
                            this.seen_candidates.clear();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    pub(super) fn note_seen_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.seen_scroll_epoch = self.seen_scroll_epoch.wrapping_add(1);
        if window.is_window_active() {
            self.seen_candidates.retain(|identity, candidate| {
                if candidate.reached_end && candidate.started.elapsed() >= Duration::from_secs(2) {
                    self.seen_hunks.insert(identity.clone());
                    false
                } else {
                    true
                }
            });
        } else {
            self.seen_candidates.clear();
        }
        cx.notify();
    }

    fn sample_seen_hunks(&mut self) {
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let viewport = self.list.viewport_bounds();
        let top = self.list.logical_scroll_top().item_ix;
        let mut visible: HashMap<String, (usize, bool)> = HashMap::new();
        for index in top..self.rows.len() {
            let Some(bounds) = self.list.bounds_for_item(index) else {
                break;
            };
            if bounds.top() >= viewport.bottom() {
                break;
            }
            if bounds.bottom() <= viewport.top() {
                continue;
            }
            let (file, hunk, header) = match self.rows[index] {
                DiffRow::HunkHeader { file, hunk } => (file as usize, hunk as usize, true),
                DiffRow::Line { file, hunk, .. } | DiffRow::SplitLine { file, hunk, .. } => {
                    (file as usize, hunk as usize, false)
                }
                _ => continue,
            };
            let Some(identity) = parsed
                .hunk_identities
                .get(file)
                .and_then(|items| items.get(hunk))
                .cloned()
            else {
                continue;
            };
            visible
                .entry(identity.clone())
                .and_modify(|entry| {
                    entry.0 = index;
                    entry.1 |= header;
                })
                .or_insert((index, header));
            if header
                && !self.seen_hunks.contains(&identity)
                && !self.seen_candidates.contains_key(&identity)
            {
                let end = self
                    .rows
                    .iter()
                    .enumerate()
                    .skip(index + 1)
                    .find_map(|(next, row)| match row {
                        DiffRow::HunkHeader { .. }
                        | DiffRow::FileHeader { .. }
                        | DiffRow::SubmoduleHeader { .. }
                        | DiffRow::BodyPad { .. } => Some(next),
                        _ => None,
                    })
                    .unwrap_or(self.rows.len());
                let last_row = (index..end)
                    .rev()
                    .find(|row_index| {
                        matches!(
                            self.rows[*row_index],
                            DiffRow::HunkHeader { .. }
                                | DiffRow::Line { .. }
                                | DiffRow::SplitLine { .. }
                        )
                    })
                    .unwrap_or(index);
                self.seen_candidates.insert(
                    identity,
                    SeenCandidate {
                        started: std::time::Instant::now(),
                        scroll_epoch: self.seen_scroll_epoch,
                        last_row,
                        reached_end: false,
                    },
                );
            }
        }
        self.seen_candidates.retain(|identity, candidate| {
            let Some((last_visible, _)) = visible.get(identity) else {
                return false;
            };
            candidate.reached_end |= *last_visible >= candidate.last_row;
            if seen_after_traversal(
                candidate.started.elapsed(),
                candidate.scroll_epoch,
                self.seen_scroll_epoch,
                *last_visible,
                candidate.last_row,
            ) {
                self.seen_hunks.insert(identity.clone());
                return false;
            }
            true
        });
    }
}
