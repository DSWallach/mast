//! Re-review mode: the Changes pane of a PR the reviewer already reviewed shows
//! only what changed since the head they last reviewed ("Since abc1234").
//!
//! The baseline lives in the review-state row (`reviewed_head`, stamped on a
//! successful submit, on mark changes once caught up, and by "Mark all
//! reviewed"). The range diff itself comes from the engine (`mode: "since"`),
//! once per (old head, new head) pair: the scoped-diff request key includes the
//! pair, so nothing re-runs per render or keystroke.
//!
//! While the mode is on, marks are read-only (the delta's hunks are not the full
//! diff's hunks, so their identities would not match) and the stored marks are
//! never reconciled against it.

use super::*;

pub(super) const REMOVED_COMMENT_MESSAGE: &str = "Removed code can't be commented on GitHub";
pub(super) const LEFT_COMMENT_MESSAGE: &str = "Left-side comments aren't available in the Since view: its old side is the previous head, not the base. Switch to the full diff to comment on removed lines.";

impl Changes {
    /// `(old, new)` heads of the re-review range, when this review has a
    /// baseline that differs from the current head and that is still fetchable.
    pub(super) fn rereview_baseline(&self) -> Option<(String, String)> {
        let state = self.review_state.as_ref()?;
        if self.review_closed
            || !state.target.starts_with("pr:")
            || !matches!(self.scope, DiffScope::Branch | DiffScope::Range)
        {
            return None;
        }
        let old = state.reviewed_head.as_deref()?;
        let new = state.head_sha.as_deref()?;
        if old.eq_ignore_ascii_case(new) || self.rereview_unavailable.as_deref() == Some(old) {
            return None;
        }
        Some((old.to_owned(), new.to_owned()))
    }

    /// The range to request while the mode is on.
    pub(super) fn rereview_range(&self) -> Option<(String, String)> {
        if self.rereview { self.rereview_baseline() } else { None }
    }

    /// A delta is (or is still) what the pane shows: marks are read-only, drafts anchor
    /// to the full diff, and nothing here counts as the full diff's identities.
    pub(super) fn rereview_active(&self) -> bool {
        self.rereview_range().is_some() || self.scoped_is_since
    }

    /// The old head could not be fetched: fall back to the full diff and say so.
    pub(super) fn note_rereview_unavailable(&mut self) {
        self.rereview_unavailable = self.review_state.as_ref().and_then(|s| s.reviewed_head.clone());
        self.rereview = false;
    }

    pub(super) fn set_rereview(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.rereview == on {
            return;
        }
        self.rereview = on && self.rereview_baseline().is_some();
        // Layout and scroll belong to the other diff.
        self.ensure_scoped(cx);
        cx.notify();
    }

    /// Remember the identities and files of the full diff for the current head,
    /// so a baseline stamped (and drafts anchored) while the delta is on screen
    /// use the full diff. Fills in a baseline snapshot that was waiting for it.
    pub(super) fn note_full_identities(&mut self, cx: &mut Context<Self>) {
        let current = self.review_identity(cx).and_then(|(_, head)| head);
        let key_matches = self
            .parsed
            .as_ref()
            .zip(self.active_diff(cx))
            .is_some_and(|(parsed, diff)| self.parse_key(diff) == parsed.key);
        self.note_full(current, key_matches, cx);
    }

    /// The head a parsed diff may be remembered as the full diff of: the head it was
    /// requested at, still the current head, no newer request in flight, not a delta,
    /// and it is the diff now shown. A stale parse after the head moved is never
    /// recorded as the new head's.
    pub(super) fn full_head_to_record(&self, current: Option<&str>, key_matches: bool) -> Option<String> {
        let requested = self.scoped_head.as_deref()?;
        let current = current?;
        (key_matches
            && self.scoped_inflight.is_none()
            && !self.rereview_active()
            && requested.eq_ignore_ascii_case(current))
        .then(|| current.to_owned())
    }

    pub(super) fn note_full(&mut self, current: Option<String>, key_matches: bool, cx: &mut Context<Self>) {
        let Some(head) = self.full_head_to_record(current.as_deref(), key_matches) else {
            return;
        };
        let Some(parsed) = self.parsed.as_ref().filter(|parsed| !parsed.truncated) else {
            return;
        };
        self.full_files = Some((head.clone(), parsed.files.clone()));
        self.full_identities = Some((head.clone(), parsed.hunk_identities.clone()));
        let Some((target, wanted)) = self.pending_snapshot.clone() else {
            return;
        };
        if !wanted.eq_ignore_ascii_case(&head) || self.review_target.as_deref() != Some(target.as_str()) {
            return;
        }
        self.pending_snapshot = None;
        let identities = self.full_identities.as_ref().map(|(_, ids)| ids.clone());
        if let (Some(state), Some(ids)) = (self.review_state.as_mut(), identities)
            && state.reviewed_head.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&wanted))
        {
            fill_snapshot(state, &ids);
            self.review_pending.insert(state.target.clone(), state.clone());
            self.save_review_state(cx);
        }
    }

    /// The files and hunk identities reviews anchor to: the full PR diff, even
    /// while the delta is shown. `None` while the full diff is still loading.
    pub(super) fn review_files(&self) -> Option<(Arc<Vec<FileDiff>>, Arc<Vec<Vec<String>>>)> {
        if !self.rereview_active() {
            let parsed = self.parsed.as_ref()?;
            return Some((parsed.files.clone(), parsed.hunk_identities.clone()));
        }
        let head = self.review_state.as_ref()?.head_sha.as_deref()?;
        let (files_head, files) = self.full_files.as_ref()?;
        let (ids_head, ids) = self.full_identities.as_ref()?;
        (files_head.eq_ignore_ascii_case(head) && ids_head.eq_ignore_ascii_case(head))
            .then(|| (files.clone(), ids.clone()))
    }

    /// Full-diff identities for `head`, if known.
    pub(super) fn full_identities_for(&self, head: &str) -> Option<&Vec<Vec<String>>> {
        self.full_identities
            .as_ref()
            .filter(|(known, _)| known.eq_ignore_ascii_case(head))
            .map(|(_, identities)| identities.as_ref())
    }

    /// Why a comment cannot be added here, if it cannot: removed code (a hunk the PR no longer
    /// has, shown as a reversion) is not on GitHub's diff, and the Since view's old side is the
    /// previous head, not the base.
    pub(super) fn adder_block(&self, file: u32, hunk: u32, side: CommentSide) -> Option<&'static str> {
        if !self.rereview_active() {
            return None;
        }
        let removed = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file as usize))
            .and_then(|file| file.hunks.get(hunk as usize))
            .is_some_and(|hunk| hunk.header.contains(mast_engine::diff_sync::since::REMOVED_LABEL));
        if removed {
            Some(REMOVED_COMMENT_MESSAGE)
        } else if side == CommentSide::Old {
            Some(LEFT_COMMENT_MESSAGE)
        } else {
            None
        }
    }

    /// The same question by coordinates (a draft being opened): a line that only exists in
    /// removed hunks is blocked; a line also shown in a regular hunk is not.
    pub(super) fn comment_block_for_line(&self, path: &str, side: CommentSide, line: u32) -> Option<&'static str> {
        if !self.rereview_active() {
            return None;
        }
        let matches = |l: &DiffLine| match side {
            CommentSide::Old => l.old_no == Some(line),
            CommentSide::New => l.new_no == Some(line),
        };
        let (mut in_removed, mut in_regular) = (false, false);
        for file in self.parsed.iter().flat_map(|p| p.files.iter()).filter(|f| f.path == path) {
            for hunk in &file.hunks {
                if hunk.lines.iter().any(matches) {
                    if hunk.header.contains(mast_engine::diff_sync::since::REMOVED_LABEL) {
                        in_removed = true;
                    } else {
                        in_regular = true;
                    }
                }
            }
        }
        if in_removed && !in_regular {
            Some(REMOVED_COMMENT_MESSAGE)
        } else if side == CommentSide::Old {
            Some(LEFT_COMMENT_MESSAGE)
        } else {
            None
        }
    }

    /// "Mark all reviewed": the current head becomes the baseline.
    pub(super) fn mark_all_reviewed(&mut self, cx: &mut Context<Self>) {
        let Some(head) = self.review_state.as_ref().and_then(|s| s.head_sha.clone()) else {
            return;
        };
        let Some(target) = self.review_target.clone() else {
            return;
        };
        self.stamp_reviewed_head(&target, &head, cx);
    }

    pub(super) fn render_rereview_controls(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(state) = self.review_state.as_ref().filter(|s| s.target.starts_with("pr:") && !self.review_closed) else {
            return Vec::new();
        };
        let mut controls = Vec::new();
        let unavailable = state
            .reviewed_head
            .as_deref()
            .is_some_and(|old| self.rereview_unavailable.as_deref() == Some(old));
        if let Some((old, _)) = self.rereview_baseline() {
            let short: String = old.chars().take(7).collect();
            let age = state
                .reviewed_at
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                .map(|at| format!(" \u{b7} {}", mast_proto::view::format_time_ago(at, chrono::Utc::now())))
                .unwrap_or_default();
            let on = self.rereview;
            controls.push(
                div()
                    .id("changes-rereview-toggle")
                    .debug_selector(|| "changes-rereview-toggle".into())
                    .flex_none()
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(11.0))
                    .text_color(if on { theme.text } else { theme.text_muted })
                    .when(true, |el| crate::components::review_text_action(el, theme))
                    .occlude()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_rereview(!on, cx)))
                    .child(SharedString::from(format!("Since {short}{age}")))
                    .into_any_element(),
            );
        }
        if self.rereview_baseline().is_some() || unavailable {
            controls.push(
                div()
                    .id("changes-mark-all-reviewed")
                    .debug_selector(|| "changes-mark-all-reviewed".into())
                    .flex_none()
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .when(true, |el| crate::components::review_text_action(el, theme))
                    .occlude()
                    .on_click(cx.listener(|this, _, _, cx| this.mark_all_reviewed(cx)))
                    .child("Mark all reviewed")
                    .into_any_element(),
            );
        }
        if unavailable {
            controls.push(
                div()
                    .id("changes-rereview-unavailable")
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme.warning)
                    .child("Last reviewed commit is gone; showing the full diff")
                    .into_any_element(),
            );
        }
        controls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes_with_review<'a>(
        cx: &'a mut gpui::TestAppContext,
        head: &str,
        reviewed: Option<&str>,
    ) -> (Entity<Changes>, &'a mut gpui::VisualTestContext) {
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
                let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
                let horizontal = files.iter().map(FileHorizontalState::new).collect();
                changes.scope = DiffScope::Branch;
                changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 1, deletions: 1, file_count: 1, horizontal, hunk_identities: Arc::new(hunk_identities(&files)), files: Arc::new(files), origins: Arc::new(origins), submodules: Arc::new(submodules), layout: Arc::new(layout) });
                changes.review_target = Some("pr:o/r#1".into());
                changes.review_state = Some(mast_proto::DiffReviewState {
                    target: "pr:o/r#1".into(),
                    head_sha: Some(head.into()),
                    reviewed_head: reviewed.map(Into::into),
                    reviewed_at: reviewed.map(|_| chrono::Utc::now().timestamp_millis() - 3 * 3_600_000),
                    ..Default::default()
                });
                cx.notify();
            });
        });
        (changes, vcx)
    }

    #[gpui::test]
    fn toggle_needs_a_baseline_that_differs_from_the_head(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("aaaaaaa1"));
        let count = |vcx: &mut gpui::VisualTestContext, changes: &Entity<Changes>| {
            vcx.update(|_, cx| changes.update(cx, |c, cx| c.render_rereview_controls(&Theme::dark(), cx).len()))
        };
        assert_eq!(count(vcx, &changes), 2, "Since toggle and Mark all reviewed");
        changes.update(vcx, |c, _| c.review_state.as_mut().unwrap().reviewed_head = Some("bbbbbbb1".into()));
        assert_eq!(count(vcx, &changes), 0, "reviewed head is the current head");
        changes.update(vcx, |c, _| c.review_state.as_mut().unwrap().reviewed_head = None);
        assert_eq!(count(vcx, &changes), 0, "never reviewed");
    }

    #[gpui::test]
    fn rereview_is_read_only_for_marks_and_falls_back_when_the_head_is_gone(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("aaaaaaa1"));
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                c.set_rereview(true, cx);
                assert_eq!(c.rereview_range(), Some(("aaaaaaa1".into(), "bbbbbbb1".into())));
                c.toggle_review_hunk(0, 0, cx);
                c.toggle_review_file(0, cx);
                assert!(c.review_state.as_ref().unwrap().viewed_hunks.is_empty(), "marks are read-only in the delta");
                // The engine reports the old head as garbage-collected.
                c.note_rereview_unavailable();
                assert!(!c.rereview_active());
                assert!(c.rereview_baseline().is_none(), "no range diff is offered for a missing head");
                assert_eq!(c.render_rereview_controls(&Theme::dark(), cx).len(), 2, "Mark all reviewed and the notice");
            });
        });
    }

    #[gpui::test]
    fn mark_all_reviewed_makes_the_current_head_the_baseline(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("aaaaaaa1"));
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                c.full_identities = Some(("bbbbbbb1".into(), c.parsed.as_ref().unwrap().hunk_identities.clone()));
                c.set_rereview(true, cx);
                c.mark_all_reviewed(cx);
                let state = c.review_state.as_ref().unwrap();
                assert_eq!(state.reviewed_head.as_deref(), Some("bbbbbbb1"));
                assert_eq!(state.reviewed_hunks.len(), 1, "snapshot comes from the full diff, not the delta");
                assert!(!c.rereview && c.rereview_baseline().is_none());
                assert!(c.review_pending.contains_key("pr:o/r#1"));
            });
        });
    }

    #[gpui::test]
    fn a_merged_pr_clears_the_baseline_through_the_real_status_refresh(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, serde_json::Value)>::new()));
        let log = calls.clone();
        let rpc: crate::diff_targets::FakeRpc = std::sync::Arc::new(move |method, params| {
            log.lock().unwrap().push((method.to_owned(), params.clone()));
            if method == mast_rpc::methods::PR_STATUS {
                let status = mast_proto::PullRequestStatusSummary {
                    pr_ref: mast_proto::PullRequestRef::new("o", "r", 1),
                    state: mast_proto::PullRequestState::Merged,
                    ..Default::default()
                };
                return Ok(serde_json::to_value(status).unwrap());
            }
            Ok(serde_json::json!({"state": null}))
        });
        let (changes, vcx) = cx.add_window_view(move |_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(rpc);
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        changes.update(vcx, |c, cx| {
            c.scope = DiffScope::Branch;
            c.review_target = Some("pr:o/r#1".into());
            c.review_state = Some(mast_proto::DiffReviewState {
                target: "pr:o/r#1".into(),
                head_sha: Some("bbbbbbb1".into()),
                reviewed_head: Some("aaaaaaa1".into()),
                reviewed_hunks: ["h".to_owned()].into(),
                ..Default::default()
            });
            c.rereview = true;
            assert!(c.rereview_active());
            c.review_pending.insert("pr:o/r#1".into(), c.review_state.clone().unwrap());
            c.review_status_checked = None;
            c.refresh_review_status(cx);
        });
        vcx.run_until_parked();
        changes.read_with(&*vcx, |c, _| {
            assert!(c.review_closed && c.review_state.is_none(), "the row is dropped locally");
            assert!(!c.rereview_active());
            assert!(!c.review_pending.contains_key("pr:o/r#1"));
        });
    }

    #[gpui::test]
    fn a_stale_parse_is_not_recorded_as_the_new_heads_full_diff(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("aaaaaaa1"));
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                // Parsed for H1 (requested at H1) after the PR moved to H2.
                c.scoped_head = Some("h1".into());
                assert!(c.full_head_to_record(Some("h2"), true).is_none());
                c.note_full(Some("h2".into()), true, cx);
                assert!(c.full_identities.is_none() && c.full_files.is_none());
                // A parse that is not the diff now shown, or while a newer request runs, is skipped too.
                c.scoped_head = Some("h2".into());
                c.note_full(Some("h2".into()), false, cx);
                assert!(c.full_identities.is_none());
                c.scoped_inflight = Some("newer request".into());
                c.note_full(Some("h2".into()), true, cx);
                assert!(c.full_identities.is_none());
                c.scoped_inflight = None;
                c.note_full(Some("h2".into()), true, cx);
                assert_eq!(c.full_identities.as_ref().map(|(head, _)| head.as_str()), Some("h2"));
            });
        });
    }

    #[gpui::test]
    fn a_delta_parse_after_mark_all_reviewed_does_not_fill_the_snapshot(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("bbbbbbb1"));
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                // Mark all reviewed at the current head, with the full diff not on hand: the
                // snapshot is pending. The Since diff still on screen must not fill it.
                c.pending_snapshot = Some(("pr:o/r#1".into(), "bbbbbbb1".into()));
                c.scoped_is_since = true;
                c.scoped_head = Some("bbbbbbb1".into());
                c.note_full(Some("bbbbbbb1".into()), true, cx);
                assert!(c.review_state.as_ref().unwrap().reviewed_hunks.is_empty());
                assert!(c.pending_snapshot.is_some());
                // The full diff arrives: now it fills.
                c.scoped_is_since = false;
                c.note_full(Some("bbbbbbb1".into()), true, cx);
                assert_eq!(c.review_state.as_ref().unwrap().reviewed_hunks.len(), 1);
                assert!(c.pending_snapshot.is_none());
            });
        });
    }

    #[gpui::test]
    fn a_posted_head_that_is_not_current_with_no_full_diff_is_marked_truncated(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", Some("aaaaaaa1"));
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                c.stamp_reviewed_head("pr:o/r#1", "ccccccc1", cx);
                let state = c.review_state.as_ref().unwrap();
                assert_eq!(state.reviewed_head.as_deref(), Some("ccccccc1"));
                assert!(state.reviewed_truncated && state.reviewed_hunks.is_empty());
                assert!(c.pending_snapshot.is_none(), "no snapshot is awaited for a head that is not current");
                // The current head waits for its snapshot instead.
                c.stamp_reviewed_head("pr:o/r#1", "bbbbbbb1", cx);
                assert!(!c.review_state.as_ref().unwrap().reviewed_truncated);
                assert!(c.pending_snapshot.is_some());
            });
        });
    }

    #[gpui::test]
    fn marking_on_a_truncated_parse_does_not_stamp(cx: &mut gpui::TestAppContext) {
        let (changes, vcx) = changes_with_review(cx, "bbbbbbb1", None);
        vcx.update(|_, cx| {
            changes.update(cx, |c, cx| {
                c.parsed.as_mut().unwrap().truncated = true;
                c.toggle_review_hunk(0, 0, cx);
                c.toggle_review_file(0, cx);
                assert!(c.review_state.as_ref().unwrap().reviewed_head.is_none());
            });
        });
    }
}
