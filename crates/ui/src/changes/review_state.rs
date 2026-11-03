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
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update((path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    for line in &hunk.lines {
        let tag = match line.kind {
            LineKind::Add => b'+',
            LineKind::Del => b'-',
            LineKind::Context | LineKind::Meta => continue,
        };
        hash.update([tag]);
        hash.update((line.text.len() as u64).to_be_bytes());
        hash.update(line.text.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(reconcile_review_head(&mut state, Some("new".into())));
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

fn reconcile_review_head(state: &mut mast_proto::DiffReviewState, head: Option<String>) -> bool {
    if head.is_none() || state.head_sha == head {
        return false;
    }
    state.head_sha = head;
    state.viewed_files.clear();
    true
}

impl Changes {
    fn review_identity(&self, cx: &App) -> Option<(String, Option<String>)> {
        if let Some(owner) = &self.owner {
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
            }
            if let Some(pr) = &self.review_pr {
                let head = self
                    .state
                    .read(cx)
                    .selected_execution_member(&owner.chat_id)
                    .and_then(|member| member.head_sha.clone());
                return Some((pr.clone(), head));
            }
            let path = owner.cwd.as_deref()?;
            return Some((
                format!("diff:{}:{}:{}", owner.device_id, path, self.scope_key()),
                None,
            ));
        }
        self.pinned_cwd
            .as_deref()
            .map(|path| (format!("diff:local:{}:{}", path, self.scope_key()), None))
    }

    pub(super) fn ensure_review_state(&mut self, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.owner.as_ref().map(|owner| owner.chat_id.clone()) {
            if self.review_link_chat.as_deref() != Some(&chat_id) {
                self.review_link_chat = Some(chat_id.clone());
                self.review_pr = None;
                let task = self.state.update(cx, |state, cx| {
                    state.chat_diff_rpc(
                        methods::CHAT_LINK_STATUS,
                        serde_json::json!({"chatId": chat_id}),
                        cx,
                    )
                });
                self.review_link_task = Some(cx.spawn(async move |this, cx| {
                    let status = task.await.ok().and_then(|value| {
                        serde_json::from_value::<mast_engine::pr_ticket_cache::ChatLinkStatus>(
                            value,
                        )
                        .ok()
                    });
                    this.update(cx, |this, cx| {
                        if this.review_link_chat.as_deref() != Some(&chat_id) {
                            return;
                        }
                        this.review_link_task = None;
                        this.review_pr = status
                            .and_then(|status| {
                                status.pr_links.into_iter().find(|link| {
                                    link.source == Some(mast_proto::ChatLinkSource::ReviewLauncher)
                                })
                            })
                            .and_then(|link| crate::chat_work_context::parse_pr_url(&link.url))
                            .map(|pr| format!("pr:{}", pr.external_id().to_lowercase()));
                        this.ensure_review_state(cx);
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
        let Some((target, head_sha)) = self.review_identity(cx) else {
            return;
        };
        if self.review_target.as_deref() == Some(&target) {
            if let Some(state) = &mut self.review_state {
                if reconcile_review_head(state, head_sha) {
                    self.review_pending.insert(target, state.clone());
                    self.save_review_state(cx);
                }
            }
            return;
        }
        self.review_target = Some(target.clone());
        self.review_error = None;
        if let Some(mut state) = self.review_pending.get(&target).cloned() {
            if reconcile_review_head(&mut state, head_sha.clone()) {
                self.review_pending.insert(target, state.clone());
            }
            self.review_state = Some(state);
            self.save_review_state(cx);
            return;
        }
        self.review_state = None;
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_DIFF_REVIEW_STATE,
                serde_json::json!({"target": target}),
                cx,
            )
        });
        self.review_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Option<mast_proto::DiffReviewState>>(value)
                    .map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.review_task = None;
                match result {
                    Ok(state) => {
                        let mut state = state.unwrap_or_else(|| mast_proto::DiffReviewState {
                            target: target.clone(),
                            ..Default::default()
                        });
                        if reconcile_review_head(&mut state, head_sha) {
                            this.review_pending.insert(target.clone(), state.clone());
                        }
                        this.review_state = Some(state);
                        this.save_review_state(cx);
                    }
                    Err(error) => this.review_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn retry_review_state(&mut self, cx: &mut Context<Self>) {
        self.review_error = None;
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
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::SET_DIFF_REVIEW_STATE,
                serde_json::to_value(&snapshot).unwrap_or_default(),
                cx,
            )
        });
        self.review_save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.review_save_task = None;
                match result {
                    Ok(_) => {
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
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file))
        else {
            return;
        };
        let Some(hunk) = file.hunks.get(hunk) else {
            return;
        };
        let identity = hunk_identity(&file.path, hunk);
        let Some(state) = self.review_state.as_mut() else {
            return;
        };
        if !state.viewed_hunks.remove(&identity) {
            state.viewed_hunks.insert(identity);
        }
        state.viewed_files.remove(&file.path);
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
        let identities: Vec<String> = file
            .hunks
            .iter()
            .map(|hunk| hunk_identity(&file.path, hunk))
            .collect();
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
            changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 2, deletions: 2, file_count: 1, horizontal, files: Arc::new(files), origins: Arc::new(origins), submodules: Arc::new(submodules), layout: Arc::new(layout) });
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
