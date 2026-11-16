//! Indices for review annotations, rebuilt when the review revision changes.
use super::{
    AnyElement, Changes, Context, DiffRow, LineKind, SharedString, Theme, div, methods, px,
};
use crate::comments::CommentSide;
use gpui::prelude::*;
use mast_proto::{ReviewDraft, ReviewSide, ReviewThread};

impl Changes {
    pub(super) fn load_review_threads(&mut self, target: &str, cx: &mut Context<Self>) {
        self.review_threads_task = None;
        self.review_threads.clear();
        self.review_thread_index = ThreadIndex::default();
        self.review_threads_error = None;
        self.refresh_thread_rows(cx);
        let Some(pr) = target
            .strip_prefix("pr:")
            .and_then(mast_proto::PullRequestRef::parse_external_id)
        else {
            return;
        };
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_REVIEW_THREADS,
                serde_json::json!({"owner": pr.owner, "repo": pr.repo, "number": pr.number}),
                cx,
            )
        });
        self.review_threads_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<ReviewThread>>(value)
                    .map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.review_threads_task = None;
                match result {
                    Ok(threads) => {
                        this.review_thread_index = ThreadIndex::new(&threads);
                        this.review_threads = threads;
                        this.refresh_thread_rows(cx);
                    }
                    Err(error) => {
                        this.review_threads_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    pub(super) fn refresh_thread_rows(&mut self, cx: &mut Context<Self>) {
        self.comment_key = self.comment_key.wrapping_add(1);
        self.sync_comment_rows(cx);
    }

    pub(super) fn add_thread_rows(&self, file_ix: usize, rows: Vec<DiffRow>) -> Vec<DiffRow> {
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file_ix))
        else {
            return rows;
        };
        if self.review_threads.is_empty() {
            return rows;
        }
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(row);
            let mut anchors = Vec::new();
            let mut append = |hunk: u32, line: u32| {
                let Some(line) = file
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line as usize))
                else {
                    return;
                };
                if line.kind != LineKind::Add {
                    if let Some(n) = line.old_no {
                        anchors.push((CommentSide::Old, n));
                    }
                }
                if line.kind != LineKind::Del {
                    if let Some(n) = line.new_no {
                        anchors.push((CommentSide::New, n));
                    }
                }
            };
            match row {
                DiffRow::Line { hunk, line, .. } => append(hunk, line),
                DiffRow::SplitLine {
                    hunk, left, right, ..
                } => {
                    if let Some(line) = left {
                        append(hunk, line);
                    }
                    if let Some(line) = right.filter(|right| Some(*right) != left) {
                        append(hunk, line);
                    }
                }
                _ => {}
            }
            for (side, line) in anchors {
                for &thread in self.review_thread_index.at(&file.path, side, line) {
                    let Some(item) = self.review_threads.get(thread) else {
                        continue;
                    };
                    let height = 30.0
                        + item
                            .comments
                            .iter()
                            .map(|comment| crate::comments::card_height(&comment.body))
                            .sum::<f32>();
                    result.push(DiffRow::ReviewThread {
                        file: file_ix as u32,
                        thread: thread as u32,
                        height: height.ceil() as u32,
                    });
                }
            }
        }
        result
    }

    pub(super) fn render_review_thread(&self, ix: usize, height: u32, theme: &Theme) -> AnyElement {
        let Some(thread) = self.review_threads.get(ix) else {
            return gpui::Empty.into_any_element();
        };
        let label = match (thread.resolved, thread.outdated) {
            (true, true) => "Resolved · Outdated",
            (true, false) => "Resolved",
            (false, true) => "Outdated",
            (false, false) => "Review thread",
        };
        let mut card = div()
            .w_full()
            .h(px(height as f32))
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(Theme::SPACE_LG))
            .bg(crate::theme::ink(0.05))
            .child(
                div()
                    .h(px(30.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(label),
            );
        for comment in &thread.comments {
            card = card.child(
                div()
                    .h(px(crate::comments::card_height(&comment.body)))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(comment.author.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_dim)
                            .child(SharedString::from(comment.body.clone())),
                    ),
            );
        }
        card.into_any_element()
    }
}
use std::collections::HashMap;

/// Maps unchanged hunk and line content to a unique line on a new revision.
/// Ambiguous repeated lines are deliberately left outdated.
#[derive(Default)]
pub(super) struct DraftAnchorIndex {
    lines: HashMap<(String, String, CommentSide, String), Vec<u32>>,
}

fn line_hash(path: &str, side: CommentSide, text: &str) -> String {
    let tag = match side {
        CommentSide::Old => b'-',
        CommentSide::New => b'+',
    };
    mast_proto::review_hunk_identity(path, [(tag, text)])
}

impl DraftAnchorIndex {
    pub(super) fn new(files: &[super::FileDiff], identities: &[Vec<String>]) -> Self {
        let mut index = Self::default();
        for (file_ix, file) in files.iter().enumerate() {
            for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
                let Some(hash) = identities.get(file_ix).and_then(|hunks| hunks.get(hunk_ix))
                else {
                    continue;
                };
                for line in &hunk.lines {
                    for (side, number) in [
                        (CommentSide::Old, line.old_no),
                        (CommentSide::New, line.new_no),
                    ] {
                        if let Some(number) = number {
                            index
                                .lines
                                .entry((
                                    file.path.clone(),
                                    hash.clone(),
                                    side,
                                    line_hash(&file.path, side, &line.text),
                                ))
                                .or_default()
                                .push(number);
                        }
                    }
                }
            }
        }
        index
    }

    pub(super) fn resolve(&self, draft: &ReviewDraft) -> Option<u32> {
        if draft.anchor.start_line != draft.anchor.end_line {
            return None;
        }
        let side = match draft.anchor.side {
            ReviewSide::Left => CommentSide::Old,
            ReviewSide::Right => CommentSide::New,
        };
        let key = (
            draft.anchor.file.clone(),
            draft.anchor.hunk_hash.clone()?,
            side,
            draft.line_hash.clone()?,
        );
        let lines = self.lines.get(&key)?;
        (lines.len() == 1).then_some(lines[0])
    }
}

#[derive(Default)]
pub(super) struct ThreadIndex {
    by_anchor: HashMap<String, HashMap<(CommentSide, u32), Vec<usize>>>,
}

impl ThreadIndex {
    pub(super) fn new(threads: &[ReviewThread]) -> Self {
        let mut index = Self::default();
        for (ix, thread) in threads.iter().enumerate() {
            let (Some(side), Some(line)) = (thread.side, thread.line) else {
                continue;
            };
            let side = match side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            index
                .by_anchor
                .entry(thread.path.clone())
                .or_default()
                .entry((side, line))
                .or_default()
                .push(ix);
        }
        index
    }

    pub(super) fn at(&self, path: &str, side: CommentSide, line: u32) -> &[usize] {
        self.by_anchor
            .get(path)
            .and_then(|file| file.get(&(side, line)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draft_reanchors_only_to_a_unique_matching_line() {
        let moved = super::super::parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+new\n",
        );
        let identities = super::super::hunk_identities(&moved);
        let draft = ReviewDraft {
            id: "one".into(),
            target: "pr:o/r#1".into(),
            body: "Check".into(),
            source: mast_proto::ReviewDraftSource::User,
            created_at: 1,
            anchor: mast_proto::ReviewAnchor {
                file: "a".into(),
                side: ReviewSide::Right,
                start_line: 1,
                end_line: 1,
                hunk_hash: Some(identities[0][0].clone()),
            },
            line_hash: Some(line_hash("a", CommentSide::New, "new")),
        };
        assert_eq!(
            DraftAnchorIndex::new(&moved, &identities).resolve(&draft),
            Some(10)
        );
        let changed = super::super::parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+different\n",
        );
        assert_eq!(
            DraftAnchorIndex::new(&changed, &super::super::hunk_identities(&changed))
                .resolve(&draft),
            None
        );
    }
    #[test]
    fn many_threads_have_direct_anchor_lookups() {
        let threads: Vec<_> = (0..5000)
            .map(|i| ReviewThread {
                id: i.to_string(),
                path: "src/a.rs".into(),
                line: Some(i + 1),
                side: Some(ReviewSide::Right),
                outdated: false,
                resolved: false,
                comments: vec![],
            })
            .collect();
        let index = ThreadIndex::new(&threads);
        assert_eq!(index.by_anchor["src/a.rs"].len(), 5000);
        for i in 0..5000 {
            assert_eq!(index.at("src/a.rs", CommentSide::New, i + 1), &[i as usize]);
        }
        assert!(index.at("src/other.rs", CommentSide::New, 1).is_empty());
    }
}
