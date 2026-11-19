use super::*;

impl Changes {
    /// Cloned because rendering borrows `self` mutably a moment later.
    pub(super) fn staged_comments(&self, cx: &App) -> Vec<ReviewComment> {
        // Explicit-checkout panes are read-only inspection surfaces. They may
        // represent an unmatched worktree, so the selected chat's composer
        // key is unrelated and must never leak comments into this diff.
        if self.pinned_cwd.is_some() {
            return Vec::new();
        }
        let key = self
            .owner
            .as_ref()
            .map(|owner| owner.chat_id.as_str())
            .unwrap_or("");
        let state = self.state.read(cx);
        let mut comments: Vec<_> = state
            .review_comments(key)
            .iter()
            .filter(|comment| {
                self.draft
                    .as_ref()
                    .and_then(|draft| draft.editing_id.as_ref())
                    != Some(&comment.id)
            })
            .cloned()
            .collect();
        if let Some(local) = self.review_target.as_ref().and_then(|target| self.local_draft_comments.get(target)) {
            comments.extend(local.iter().filter(|comment| self.draft.as_ref().and_then(|draft| draft.editing_id.as_ref()) != Some(&comment.id)).cloned());
        }
        if self.suggestion_target.as_ref() == self.review_target.as_ref() {
            comments.extend(self.suggestion_comments.iter().cloned());
        }
        comments
    }

    pub(super) fn comments_for(&self, path: &str, _cx: &App) -> Vec<ReviewComment> {
        self.comment_index.get(path).cloned().unwrap_or_default()
    }

    /// The parsed diff's pre-rename path for `path`, when the file moved.
    pub(super) fn old_path_of(&self, path: &str) -> Option<String> {
        self.parsed
            .as_ref()?
            .files
            .iter()
            .find(|file| file.path == path)?
            .old_path
            .clone()
    }

    /// A draft belongs to the checkout it was opened over. Chat navigation
    /// swaps both the diff under it and the composer it would stage onto, so
    /// the half-written note is dropped rather than following the user across.
    pub(super) fn discard_stale_draft(&mut self, cx: &mut Context<Self>) {
        let key = self
            .owner
            .as_ref()
            .map(|owner| owner.chat_id.clone())
            .unwrap_or_default();
        if self.draft.as_ref().is_some_and(|draft| draft.key != key) {
            if self.draft.as_ref().is_some_and(|draft| draft.local.is_some()) { self.commit_draft(cx); }
            else { self.draft = None; }
            self.sync_comment_rows(cx);
            cx.notify();
        }
    }

    pub(super) fn draft_anchor(&self) -> Option<(String, CommentSide, u32)> {
        self.draft
            .as_ref()
            .filter(|draft| !draft.outdated)
            .map(|draft| (draft.path.clone(), draft.side, draft.line))
    }

    pub(super) fn draft_anchor_in(&self, path: &str) -> Option<(CommentSide, u32)> {
        self.draft
            .as_ref()
            .filter(|draft| !draft.outdated && draft.path == path)
            .map(|draft| (draft.side, draft.line))
    }

    pub(super) fn sync_comment_rows(&mut self, cx: &mut Context<Self>) {
        if self.parsed.is_none() {
            return;
        }
        let sync_key = CommentSyncKey {
            revision: self.state.read(cx).review_comments_revision,
            owner: self.owner_chat_id_owned(), target: self.review_target.clone(), pinned: self.pinned_cwd.is_some(),
            editor: self.draft.as_ref().map(|draft| (draft.path.clone(), draft.side, draft.line, draft.editing_id.clone())),
        };
        if self.comment_sync_key.as_ref() == Some(&sync_key) { return; }
        self.comment_sync_key = Some(sync_key);
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let key = comment_state_key(&staged, draft.as_ref());
        if key == self.comment_key {
            return;
        }
        self.comment_key = key;
        self.comment_index.clear();
        for comment in &staged {
            if !comment.is_file() { self.comment_index.entry(comment.path.clone()).or_default().push(comment.clone()); }
        }
        let Some(parsed) = &self.parsed else {
            return;
        };
        let files = parsed.files.clone();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let Some(range) = self.row_ranges[file_ix].clone() else {
                continue;
            };
            let file = &files[file_ix];
            // A mid-tween stand-in is the settle sweep's to replace.
            if self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed)
            {
                continue;
            }
            if self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                })
            {
                continue;
            }
            let comments = self.comment_index.get(&file.path).cloned().unwrap_or_default();
            let body = body_rows(
                file_ix as u32,
                file,
                &comments,
                self.draft_anchor_in(&file.path),
                self.mode,
            );
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
    }

    pub(super) fn set_hover(
        &mut self,
        path: &str,
        anchor: Option<(CommentSide, u32)>,
        cx: &mut Context<Self>,
    ) {
        if self.pinned_cwd.is_some() {
            if self.hover.take().is_some() {
                cx.notify();
            }
            return;
        }
        let next = anchor.map(|(side, line)| HoverRow {
            path: path.to_string(),
            side,
            line,
        });
        if next != self.hover {
            self.hover = next;
            cx.notify();
        }
    }

    pub(super) fn hovering(&self, path: &str, anchor: (CommentSide, u32)) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|hover| hover.path == path && (hover.side, hover.line) == anchor)
    }

    pub(super) fn clear_hover_at(&mut self, path: &str, anchor: (CommentSide, u32), cx: &mut Context<Self>) {
        if self.hovering(path, anchor) {
            self.hover = None;
            cx.notify();
        }
    }

    pub(super) fn open_draft(
        &mut self,
        path: String,
        side: CommentSide,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_draft_internal(path, side, line, true, window, cx);
    }

    pub(super) fn open_draft_internal(
        &mut self, path: String, side: CommentSide, line: u32, local_allowed: bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        if self.pinned_cwd.is_some() {
            return;
        }
        if let Some(reason) = self.comment_block_for_line(&path, side, line) {
            self.local_draft_error = Some(reason.into());
            cx.notify();
            return;
        }
        if self.draft.is_some() { self.commit_draft(cx); }
        let local = if local_allowed && matches!(self.scope, DiffScope::Branch | DiffScope::Range) {
            if let Some(target) = self.review_target.as_deref() {
                let Some(draft) = self.new_local_draft(target, &path, side, line) else {
                    self.local_draft_error = Some("The review draft is still loading or this line is no longer in the diff.".into());
                    cx.notify();
                    return;
                };
                Some(review_drafts::LocalDraftEdit { draft, original: None })
            } else { None }
        } else { None };
        let input = cx.new(|cx| ComposerInput::new("Request a change…", cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.commit_draft(cx),
            ComposerInputEvent::Edited => this.local_draft_edited(cx),
            _ => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        let key = self.owner_chat_id_owned().unwrap_or_default();
        let old_path = self.old_path_of(&path);
        self.draft = Some(CommentDraft {
            editing_id: None,
            local,
            outdated: false,
            key,
            path,
            old_path,
            side,
            line,
            input,
            _events: events,
        });
        window.focus(&handle, cx);
        self.sync_comment_rows(cx);
        cx.notify();
    }

    pub(super) fn edit_comment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let local = self.review_target.as_ref().and_then(|target| self.local_review_drafts.get(target))
            .and_then(|drafts| drafts.iter().find(|draft| draft.id == id)).cloned();
        let Some(comment) = self.staged_comments(cx)
            .iter()
            .find(|comment| comment.id == id && !comment.is_file())
            .cloned()
        else {
            return;
        };
        let Some((side, line)) = comment.diff_anchor() else {
            return;
        };
        self.open_draft_internal(comment.path.clone(), side, line, local.is_some(), window, cx);
        let Some(draft) = self.draft.as_mut() else { return; };
        draft.editing_id = Some(comment.id);
        draft.local = local.map(|draft| review_drafts::LocalDraftEdit { original: Some(draft.clone()), draft });
        if let comments::CommentSource::Diff { old_path, .. } = comment.source {
            draft.old_path = old_path;
        }
        draft
            .input
            .update(cx, |input, cx| input.set_text(comment.body, cx));
        self.sync_comment_rows(cx);
        cx.notify();
    }

    pub(super) fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.take()
            && let Some(local) = draft.local { self.cancel_local_edit(local, cx); }
        self.sync_comment_rows(cx);
        cx.notify();
    }

    pub(super) fn commit_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let body = draft.input.read(cx).text().trim().to_string();
        if let Some(mut local) = draft.local {
            if body.is_empty() { self.cancel_local_edit(local, cx); }
            else { local.draft.body = body; self.put_local_draft(local.draft, true, cx); self.rebuild_draft_index(cx); }
            self.sync_comment_rows(cx);
            cx.notify();
            return;
        }
        if body.is_empty() {
            self.sync_comment_rows(cx);
            cx.notify();
            return;
        }

        // `draft.key`, not the live one: the note stages onto the composer it
        // was written against even if the selection moved under it.
        let key = draft.key;
        self.state.update(cx, |state, cx| {
            if let Some(id) = draft.editing_id {
                state.update_review_comment_body(&key, &id, body);
            } else {
                let comment = ReviewComment::new(draft.path, draft.side, draft.line, body)
                    .renamed_from(draft.old_path);
                state.add_review_comment(&key, comment);
            }
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

    pub(super) fn remove_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.review_target.clone().filter(|target| self.local_review_drafts.get(target).is_some_and(|drafts| drafts.iter().any(|draft| draft.id == id))) {
            self.remove_local_draft(&target, id, cx);
            return;
        }
        let key = self.owner_chat_id_owned().unwrap_or_default();
        self.state.update(cx, |state, cx| {
            state.remove_review_comment(&key, id);
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

}
