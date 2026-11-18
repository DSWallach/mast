use super::*;

impl AppState {
    pub fn begin_code_ref_load(&mut self, key: &str) -> bool {
        let count = self.code_ref_pending.entry(key.to_string()).or_default();
        if *count >= 32 { return false; }
        *count += 1;
        true
    }
    pub fn end_code_ref_load(&mut self, key: &str) {
        if let Some(count) = self.code_ref_pending.get_mut(key) { *count = count.saturating_sub(1); if *count == 0 { self.code_ref_pending.remove(key); } }
    }
    pub fn code_ref_load_pending(&self, key: &str) -> bool { self.code_ref_pending.get(key).copied().unwrap_or(0) > 0 }

    pub fn code_refs(&self, key: &str) -> &[crate::code_ref::StagedCodeRef] {
        self.code_refs.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn add_code_ref(&mut self, key: &str, reference: crate::code_ref::StagedCodeRef) {
        self.code_refs.entry(key.to_string()).or_default().push(reference);
    }

    pub fn remove_code_ref(&mut self, key: &str, id: &str) {
        if let Some(references) = self.code_refs.get_mut(key) {
            references.retain(|reference| reference.id != id);
        }
    }

    pub fn take_code_refs(&mut self, key: &str) -> Vec<crate::code_ref::StagedCodeRef> {
        self.code_refs.remove(key).unwrap_or_default()
    }

    pub fn review_comments(&self, key: &str) -> &[ReviewComment] {
        self.review_comments
            .get(key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn add_review_comment(&mut self, key: &str, comment: ReviewComment) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        self.review_comments
            .entry(key.to_string())
            .or_default()
            .push(comment);
    }

    pub fn remove_review_comment(&mut self, key: &str, id: &str) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        if let Some(list) = self.review_comments.get_mut(key) {
            list.retain(|c| c.id != id);
            if list.is_empty() {
                self.review_comments.remove(key);
                self.review_comment_flushes.remove(key);
            }
        }
    }

    /// Update only a staged comment's body. A stale editor must never recreate
    /// a comment that has already been removed or sent to the agent.
    pub fn update_review_comment_body(&mut self, key: &str, id: &str, body: String) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        if let Some(comment) = self
            .review_comments
            .get_mut(key)
            .and_then(|comments| comments.iter_mut().find(|comment| comment.id == id))
        {
            comment.body = body;
        }
    }

    pub fn update_review_comment_line(&mut self, key: &str, id: &str, line: u32) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        if let Some(comment) = self
            .review_comments
            .get_mut(key)
            .and_then(|comments| comments.iter_mut().find(|comment| comment.id == id))
        {
            comment.line = line;
        }
    }

    pub fn rename_review_comment_path(&mut self, key: &str, old_path: &str, new_path: &str) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        if let Some(comments) = self.review_comments.get_mut(key) {
            for comment in comments
                .iter_mut()
                .filter(|comment| comment.is_file() && comment.path == old_path)
            {
                comment.path = new_path.to_string();
            }
        }
    }

    pub fn take_review_comments(&mut self, key: &str) -> Vec<ReviewComment> {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        self.review_comment_flushes.remove(key);
        self.review_comments.remove(key).unwrap_or_default()
    }

    pub fn purge_review_comments(&mut self, key: &str) {
        self.review_comments_revision = self.review_comments_revision.wrapping_add(1);
        self.review_comment_flushes.remove(key);
        self.review_comments.remove(key);
    }

    pub fn begin_review_comment_flush(&mut self, key: &str, source: u64) {
        self.review_comment_flushes
            .entry(key.to_string())
            .or_default()
            .insert(source);
    }

    pub fn finish_review_comment_flush(&mut self, key: &str, source: u64) {
        let remove_key = self
            .review_comment_flushes
            .get_mut(key)
            .is_some_and(|sources| {
                sources.remove(&source);
                sources.is_empty()
            });
        if remove_key {
            self.review_comment_flushes.remove(key);
        }
    }

    pub fn review_comment_flush_pending(&self, key: &str) -> bool {
        self.review_comment_flushes.contains_key(key)
    }

}
