//! Indices for review annotations, rebuilt when the review revision changes.
use super::{
    AnyElement, Changes, Context, DiffRow, LineKind, SharedString, Theme, div, methods, px,
};
use crate::comments::CommentSide;
use gpui::prelude::*;
use mast_proto::{ReviewDraft, ReviewSide, ReviewThread};

pub(super) struct LocalDraftEdit {
    pub(super) draft: ReviewDraft,
    pub(super) original: Option<ReviewDraft>,
}

impl Changes {
    pub(super) fn is_local_draft(&self, id: &str) -> bool {
        self.local_draft_ids.contains(id)
    }

    pub(super) fn render_local_draft_card(
        &self,
        comment: &crate::comments::ReviewComment,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let edit_id = comment.id.clone();
        let remove_id = comment.id.clone();
        let stage_id = comment.id.clone();
        let button = |id: String, label: &'static str| {
            div()
                .id(SharedString::from(id))
                .cursor_pointer()
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(label)
        };
        div()
            .w_full()
            .h(px(crate::comments::card_height(&comment.body)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(Theme::SPACE_LG))
            .bg(crate::theme::ink(0.05))
            .child(
                div()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child("Draft"),
                    )
                    .child(
                        button(format!("draft-stage-{}", comment.id), "Add to chat").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.stage_local_draft(&stage_id, cx)
                            }),
                        ),
                    )
                    .child(
                        button(format!("draft-edit-{}", comment.id), "Edit").on_click(cx.listener(
                            move |this, _, window, cx| this.edit_comment(&edit_id, window, cx),
                        )),
                    )
                    .child(
                        button(format!("draft-delete-{}", comment.id), "Delete").on_click(
                            cx.listener(move |this, _, _, cx| this.remove_comment(&remove_id, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(comment.body.clone())),
            )
            .into_any_element()
    }

    fn stage_local_draft(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(mut comment) = self
            .review_target
            .as_ref()
            .and_then(|target| self.local_draft_comments.get(target))
            .and_then(|comments| comments.iter().find(|comment| comment.id == id))
            .cloned()
        else {
            return;
        };
        let Some(chat) = self.owner_chat_id_owned() else {
            return;
        };
        comment.id = uuid::Uuid::new_v4().to_string();
        self.state.update(cx, |state, cx| {
            state.add_review_comment(&chat, comment);
            cx.notify();
        });
        self.sync_comment_rows(cx);
    }

    pub(super) fn load_local_drafts(&mut self, target: &str, cx: &mut Context<Self>) {
        self.local_draft_load_task = None;
        self.rebuild_draft_index(cx);
        if self.local_review_drafts.contains_key(target) {
            return;
        }
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_REVIEW_DRAFTS,
                serde_json::json!({"target": target}),
                cx,
            )
        });
        self.local_draft_load_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<ReviewDraft>>(value).map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.local_draft_load_task = None;
                match result {
                    Ok(drafts) => {
                        this.local_review_drafts.entry(target).or_insert(drafts);
                        this.rebuild_draft_index(cx);
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    pub(super) fn retry_local_drafts(&mut self, cx: &mut Context<Self>) {
        self.local_draft_error = None;
        if !self.local_draft_pending.is_empty() {
            self.local_draft_save_ready = true;
            self.save_local_drafts(cx);
        } else if let Some(target) = self.review_target.clone() {
            self.load_local_drafts(&target, cx);
        }
    }

    pub(super) fn render_outdated_drafts(
        &self,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(target) = self.review_target.as_ref() else {
            return Vec::new();
        };
        self.local_outdated_drafts
            .iter()
            .filter(|draft| &draft.target == target)
            .map(|draft| {
                let id = draft.id.clone();
                let label = format!(
                    "Outdated · {}:{}",
                    draft.anchor.file, draft.anchor.start_line
                );
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .px(px(Theme::SPACE_LG))
                    .py(px(6.0))
                    .bg(crate::theme::ink(0.05))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("outdated-draft-delete-{id}")))
                                    .cursor_pointer()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_comment(&id, cx)
                                    }))
                                    .child("Delete"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_dim)
                            .child(SharedString::from(draft.body.clone())),
                    )
                    .into_any_element()
            })
            .collect()
    }

    pub(super) fn rebuild_draft_index(&mut self, cx: &mut Context<Self>) {
        let drafts = self
            .review_target
            .as_ref()
            .and_then(|target| self.local_review_drafts.get(target))
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.draft_anchor_index = self
            .parsed
            .as_ref()
            .map(|parsed| DraftAnchorIndex::new(&parsed.files, &parsed.hunk_identities, drafts))
            .unwrap_or_default();
        self.rebuild_draft_comments(cx);
    }

    pub(super) fn rebuild_draft_comments(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.review_target.clone() else {
            return;
        };
        let mut comments = Vec::new();
        self.local_draft_ids.clear();
        self.local_outdated_drafts.clear();
        for draft in self.local_review_drafts.get(&target).into_iter().flatten() {
            self.local_draft_ids.insert(draft.id.clone());
            let Some(line) = self.draft_anchor_index.resolve(draft) else {
                self.local_outdated_drafts.push(draft.clone());
                continue;
            };
            let side = match draft.anchor.side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            let mut comment =
                crate::comments::ReviewComment::new(&draft.anchor.file, side, line, &draft.body);
            comment.id = draft.id.clone();
            if let Some(old_path) = self.draft_anchor_index.old_paths.get(&draft.anchor.file) {
                comment = comment.renamed_from(Some(old_path.clone()));
            }
            comments.push(comment);
        }
        self.local_draft_comments.insert(target, comments);
        self.refresh_thread_rows(cx);
    }

    pub(super) fn new_local_draft(
        &self,
        target: &str,
        path: &str,
        side: CommentSide,
        line: u32,
    ) -> Option<ReviewDraft> {
        if !self.local_review_drafts.contains_key(target) {
            return None;
        }
        let parsed = self.parsed.as_ref()?;
        let (file_ix, file) = parsed
            .files
            .iter()
            .enumerate()
            .find(|(_, file)| file.path == path)?;
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            if let Some(found) = hunk.lines.iter().find(|candidate| match side {
                CommentSide::Old => candidate.old_no == Some(line),
                CommentSide::New => candidate.new_no == Some(line),
            }) {
                return Some(ReviewDraft {
                    id: uuid::Uuid::new_v4().to_string(),
                    target: target.to_owned(),
                    body: String::new(),
                    source: mast_proto::ReviewDraftSource::User,
                    created_at: chrono::Utc::now().timestamp_millis(),
                    anchor: mast_proto::ReviewAnchor {
                        file: path.to_owned(),
                        side: match side {
                            CommentSide::Old => ReviewSide::Left,
                            CommentSide::New => ReviewSide::Right,
                        },
                        start_line: line,
                        end_line: line,
                        hunk_hash: parsed
                            .hunk_identities
                            .get(file_ix)
                            .and_then(|ids| ids.get(hunk_ix))
                            .cloned(),
                    },
                    line_hash: Some(line_hash(path, side, &found.text)),
                });
            }
        }
        None
    }

    pub(super) fn put_local_draft(
        &mut self,
        draft: ReviewDraft,
        immediate: bool,
        cx: &mut Context<Self>,
    ) {
        let target = draft.target.clone();
        let drafts = self.local_review_drafts.entry(target.clone()).or_default();
        if let Some(existing) = drafts.iter_mut().find(|item| item.id == draft.id) {
            *existing = draft;
        } else {
            drafts.push(draft);
        }
        self.queue_local_drafts(&target, immediate, cx);
    }

    pub(super) fn local_draft_edited(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let Some(local) = draft.local.as_ref() else {
            cx.notify();
            return;
        };
        let mut value = local.draft.clone();
        value.body = draft.input.read(cx).text().to_owned();
        if value.body.trim().is_empty() {
            let target = value.target.clone();
            if let Some(drafts) = self.local_review_drafts.get_mut(&target) {
                let before = drafts.len();
                drafts.retain(|draft| draft.id != value.id);
                if drafts.len() != before {
                    self.queue_local_drafts(&target, false, cx);
                }
            }
        } else {
            self.put_local_draft(value, false, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_local_edit(&mut self, edit: LocalDraftEdit, cx: &mut Context<Self>) {
        if let Some(original) = edit.original {
            self.put_local_draft(original, true, cx);
        } else if self
            .local_review_drafts
            .get(&edit.draft.target)
            .is_some_and(|drafts| drafts.iter().any(|draft| draft.id == edit.draft.id))
        {
            self.remove_local_draft(&edit.draft.target, &edit.draft.id, cx);
        }
        self.rebuild_draft_comments(cx);
    }

    pub(super) fn remove_local_draft(&mut self, target: &str, id: &str, cx: &mut Context<Self>) {
        if let Some(drafts) = self.local_review_drafts.get_mut(target) {
            drafts.retain(|draft| draft.id != id);
        }
        self.queue_local_drafts(target, true, cx);
        self.rebuild_draft_comments(cx);
    }

    fn queue_local_drafts(&mut self, target: &str, immediate: bool, cx: &mut Context<Self>) {
        let Some(drafts) = self.local_review_drafts.get(target).cloned() else {
            return;
        };
        self.local_draft_pending.insert(target.to_owned(), drafts);
        self.local_draft_error = None;
        self.local_draft_debounce_task = None;
        if immediate {
            self.local_draft_save_ready = true;
            self.save_local_drafts(cx);
        } else {
            self.local_draft_save_ready = false;
            self.local_draft_debounce_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                this.update(cx, |this, cx| {
                    this.local_draft_debounce_task = None;
                    this.local_draft_save_ready = true;
                    this.save_local_drafts(cx);
                })
                .ok();
            }));
        }
    }

    fn save_local_drafts(&mut self, cx: &mut Context<Self>) {
        if !self.local_draft_save_ready
            || self.local_draft_save_task.is_some()
            || self.local_draft_error.is_some()
        {
            return;
        }
        let Some((target, snapshot)) = self
            .local_draft_pending
            .iter()
            .next()
            .map(|(key, value)| (key.clone(), value.clone()))
        else {
            return;
        };
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::SET_REVIEW_DRAFTS,
                serde_json::json!({"target": target, "drafts": snapshot}),
                cx,
            )
        });
        self.local_draft_save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.local_draft_save_task = None;
                match result {
                    Ok(_) => {
                        if this.local_draft_pending.get(&target) == Some(&snapshot) {
                            this.local_draft_pending.remove(&target);
                        }
                        this.save_local_drafts(cx);
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
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
    old_paths: HashMap<String, String>,
}

fn line_hash(path: &str, side: CommentSide, text: &str) -> String {
    let tag = match side {
        CommentSide::Old => b'-',
        CommentSide::New => b'+',
    };
    mast_proto::review_hunk_identity(path, [(tag, text)])
}

impl DraftAnchorIndex {
    pub(super) fn new(
        files: &[super::FileDiff],
        identities: &[Vec<String>],
        drafts: &[ReviewDraft],
    ) -> Self {
        let mut index = Self::default();
        let wanted: std::collections::HashSet<_> = drafts
            .iter()
            .filter_map(|draft| {
                Some((
                    draft.anchor.file.as_str(),
                    draft.anchor.hunk_hash.as_deref()?,
                ))
            })
            .collect();
        for (file_ix, file) in files.iter().enumerate() {
            if let Some(old_path) = &file.old_path {
                index.old_paths.insert(file.path.clone(), old_path.clone());
            }
            for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
                let Some(hash) = identities.get(file_ix).and_then(|hunks| hunks.get(hunk_ix))
                else {
                    continue;
                };
                if !wanted.contains(&(file.path.as_str(), hash.as_str())) {
                    continue;
                }
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
    #[gpui::test]
    fn typing_coalesces_registry_writes(cx: &mut gpui::TestAppContext) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let writes = Arc::new(AtomicUsize::new(0));
        let captured = writes.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = super::super::AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, _| {
                if method == methods::SET_REVIEW_DRAFTS {
                    captured.fetch_add(1, Ordering::SeqCst);
                }
                Ok(serde_json::json!(true))
            }));
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        changes.update(vcx, |changes, cx| {
            for n in 0..100 {
                changes.put_local_draft(
                    ReviewDraft {
                        id: "one".into(),
                        target: "pr:o/r#1".into(),
                        body: format!("typed {n}"),
                        source: mast_proto::ReviewDraftSource::User,
                        created_at: 1,
                        anchor: mast_proto::ReviewAnchor {
                            file: "a".into(),
                            side: ReviewSide::Right,
                            start_line: 1,
                            end_line: 1,
                            hunk_hash: Some("h".into()),
                        },
                        line_hash: Some("l".into()),
                    },
                    false,
                    cx,
                );
            }
        });
        vcx.run_until_parked();
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }
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
            DraftAnchorIndex::new(&moved, &identities, &[draft.clone()]).resolve(&draft),
            Some(10)
        );
        let changed = super::super::parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+different\n",
        );
        assert_eq!(
            DraftAnchorIndex::new(
                &changed,
                &super::super::hunk_identities(&changed),
                &[draft.clone()]
            )
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
