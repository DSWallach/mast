//! Local review drafts, grouped by stable review target.
use super::*;
use mast_proto::{ReviewDraft, derive_id};

fn row_id(target: &str) -> String {
    derive_id(target, "review-drafts")
}

impl RegistryDoc {
    /// Accepting a suggestion creates the draft and records its disposition in one operation.
    pub fn accept_review_suggestion(
        &mut self,
        draft: &ReviewDraft,
        suggestion: &str,
    ) -> Result<bool, DocError> {
        if draft.target.trim().is_empty()
            || draft.id.trim().is_empty()
            || suggestion.trim().is_empty()
        {
            return Err(DocError::Schema(
                "review suggestion needs a draft, target and id".into(),
            ));
        }
        let existing = self
            .review_drafts(&draft.target)
            .into_iter()
            .find(|item| item.id == draft.id);
        let dismissed = self
            .dismissed_review_suggestions(&draft.target)
            .iter()
            .any(|id| id == suggestion);
        if existing.as_ref() == Some(draft) && dismissed {
            return Ok(false);
        }
        if existing.is_some() || dismissed {
            return Err(DocError::Schema(
                "review suggestion was already handled".into(),
            ));
        }
        let id = row_id(&draft.target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let mut set = fields([("target", json!(draft.target))]);
        set.insert(
            format!("d:{}", derive_id(&draft.id, "review-draft")),
            json!(serde_json::to_string(draft)?),
        );
        set.insert(
            format!("s:{}", derive_id(suggestion, "review-suggestion")),
            json!(suggestion),
        );
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(true)
    }

    pub fn dismissed_review_suggestions(&self, target: &str) -> Vec<String> {
        let Some(row) = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target)) else {
            return Vec::new();
        };
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return Vec::new();
        }
        let mut ids: Vec<_> = row
            .fields
            .iter()
            .filter(|(key, _)| key.starts_with("s:"))
            .filter_map(|(_, value)| value.as_str().map(str::to_owned))
            .collect();
        ids.sort();
        ids
    }

    pub fn dismiss_review_suggestion(
        &mut self,
        target: &str,
        suggestion: &str,
    ) -> Result<bool, DocError> {
        if target.trim().is_empty() || suggestion.trim().is_empty() {
            return Err(DocError::Schema(
                "review suggestion needs a target and id".into(),
            ));
        }
        if self
            .dismissed_review_suggestions(target)
            .iter()
            .any(|id| id == suggestion)
        {
            return Ok(false);
        }
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let mut set = fields([("target", json!(target))]);
        set.insert(
            format!("s:{}", derive_id(suggestion, "review-suggestion")),
            json!(suggestion),
        );
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(true)
    }

    pub fn review_drafts(&self, target: &str) -> Vec<ReviewDraft> {
        let Some(row) = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target)) else {
            return Vec::new();
        };
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return Vec::new();
        }
        let mut drafts: Vec<ReviewDraft> = row
            .fields
            .iter()
            .filter(|(key, _)| key.starts_with("d:"))
            .filter_map(|(_, value)| value.as_str())
            .filter_map(|value| serde_json::from_str::<ReviewDraft>(value).ok())
            .filter(|draft| draft.target == target)
            .collect();
        drafts.sort_by(|a, b| a.id.cmp(&b.id));
        drafts
    }

    /// The per-target marker nonce embedded in a posted review's body. Persisted so a
    /// restart after a lost response can still find the review it posted.
    pub fn review_nonce(&self, target: &str) -> Option<String> {
        let row = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target))?;
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return None;
        }
        row.fields
            .get("nonce")
            .and_then(Value::as_str)
            .filter(|nonce| !nonce.is_empty())
            .map(str::to_owned)
    }

    /// Return the target's nonce, writing `candidate` first if it has none. The nonce stays
    /// until [`Self::record_posted_review`] confirms a post.
    pub fn ensure_review_nonce(&mut self, target: &str, candidate: &str) -> Result<String, DocError> {
        if target.trim().is_empty() || candidate.trim().is_empty() {
            return Err(DocError::Schema("review nonce needs a target".into()));
        }
        if let Some(nonce) = self.review_nonce(target) {
            return Ok(nonce);
        }
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let mut set = fields([("target", json!(target))]);
        set.insert("nonce".into(), json!(candidate));
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(candidate.to_owned())
    }

    /// Compare-and-set: replace the target's nonce with `candidate` only if it still equals
    /// `expected` (it already belongs to a recorded review). Returns the nonce now in effect,
    /// which is the stored one when someone else renewed first. Clears the saved attempt.
    pub fn renew_review_nonce(&mut self, target: &str, expected: &str, candidate: &str) -> Result<String, DocError> {
        if target.trim().is_empty() || candidate.trim().is_empty() {
            return Err(DocError::Schema("review nonce needs a target".into()));
        }
        if let Some(current) = self.review_nonce(target)
            && current != expected
        {
            return Ok(current);
        }
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let mut set = fields([("target", json!(target))]);
        set.insert("nonce".into(), json!(candidate));
        set.insert("attempt".into(), Value::Null);
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(candidate.to_owned())
    }

    /// The draft ids of the review being posted under `nonce`, saved before the POST. When the
    /// marker later turns up on GitHub these, not a text match, say what was sent.
    pub fn save_review_attempt(&mut self, target: &str, nonce: &str, ids: &[String]) -> Result<(), DocError> {
        if target.trim().is_empty() || nonce.trim().is_empty() {
            return Err(DocError::Schema("review attempt needs a target and nonce".into()));
        }
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let mut set = fields([("target", json!(target))]);
        set.insert("attempt".into(), json!(serde_json::to_string(&json!({"nonce": nonce, "ids": ids}))?));
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(())
    }

    pub fn review_attempt_ids(&self, target: &str, nonce: &str) -> Option<Vec<String>> {
        let row = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target))?;
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return None;
        }
        let saved: Value = serde_json::from_str(row.fields.get("attempt")?.as_str()?).ok()?;
        if saved.get("nonce").and_then(Value::as_str) != Some(nonce) {
            return None;
        }
        saved
            .get("ids")?
            .as_array()
            .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect())
    }

    /// Posted reviews recorded for a target, oldest first.
    pub fn posted_reviews(&self, target: &str) -> Vec<mast_proto::PostedReview> {
        let Some(row) = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target)) else {
            return Vec::new();
        };
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return Vec::new();
        }
        let mut posted: Vec<mast_proto::PostedReview> = row
            .fields
            .iter()
            .filter(|(key, _)| key.starts_with("p:"))
            .filter_map(|(_, value)| value.as_str())
            .filter_map(|value| serde_json::from_str(value).ok())
            .collect();
        posted.sort_by_key(|review| (review.posted_at, review.review_id));
        posted
    }

    /// Keep a local record of a review that reached GitHub and remove the drafts it carried,
    /// in one write. Recording the same review id again is a no-op.
    pub fn record_posted_review(
        &mut self,
        target: &str,
        review: &mast_proto::PostedReview,
        sent_ids: &[String],
        consume_nonce: bool,
    ) -> Result<bool, DocError> {
        if target.trim().is_empty() {
            return Err(DocError::Schema("posted review needs a target".into()));
        }
        let key = format!("p:{}", derive_id(&review.review_id.to_string(), "posted-review"));
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let already = self.posted_reviews(target).iter().any(|r| r.review_id == review.review_id);
        let mut set = fields([("target", json!(target))]);
        if !already {
            set.insert(key, json!(serde_json::to_string(review)?));
        }
        let mut removed = consume_nonce && self.review_nonce(target).is_some();
        if removed {
            // A confirmed post consumes the marker nonce; the next review gets a fresh one.
            set.insert("nonce".into(), Value::Null);
            set.insert("attempt".into(), Value::Null);
        }
        for draft_id in sent_ids {
            if self.review_drafts(target).iter().any(|d| &d.id == draft_id) {
                set.insert(
                    format!("d:{}", derive_id(draft_id, "review-draft")),
                    Value::Null,
                );
                removed = true;
            }
        }
        if already && !removed {
            return Ok(false);
        }
        let hlc = self.mint_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_REVIEW_DRAFTS.into(),
            id,
            op: OpKind::Upsert,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(true)
    }

    /// Compare before writing to avoid needless registry operations.
    pub fn put_review_drafts(
        &mut self,
        target: &str,
        drafts: &[ReviewDraft],
    ) -> Result<bool, DocError> {
        let before = self.review_drafts(target);
        self.update_review_drafts(target, &before, drafts)
    }

    /// Apply only changes from the caller's snapshot, preserving other drafts.
    pub fn update_review_drafts(
        &mut self,
        target: &str,
        before: &[ReviewDraft],
        drafts: &[ReviewDraft],
    ) -> Result<bool, DocError> {
        let mut ids = std::collections::HashSet::new();
        if target.trim().is_empty()
            || drafts.iter().any(|draft| {
                draft.target != target || draft.id.trim().is_empty() || !ids.insert(&draft.id)
            })
            || before
                .iter()
                .any(|draft| draft.target != target || draft.id.trim().is_empty())
        {
            return Err(DocError::Schema(
                "review drafts need matching targets".into(),
            ));
        }
        let current = self.review_drafts(target);
        let old: BTreeMap<_, _> = before
            .iter()
            .map(|draft| (draft.id.clone(), draft))
            .collect();
        let new: BTreeMap<_, _> = drafts
            .iter()
            .map(|draft| (draft.id.clone(), draft))
            .collect();
        if old.len() != before.len() {
            return Err(DocError::Schema("duplicate review draft ids".into()));
        }
        let current: BTreeMap<_, _> = current
            .iter()
            .map(|draft| (draft.id.clone(), draft))
            .collect();
        if old == new {
            return Ok(false);
        }
        let id = row_id(target);
        self.observe_row(KIND_REVIEW_DRAFTS, &id);
        let hlc = self.mint_hlc();
        let mut updates = vec![fields([("target", json!(target))])];
        let keys: std::collections::BTreeSet<String> =
            old.keys().chain(new.keys()).cloned().collect();
        for key in &keys {
            if old.get(key) == new.get(key) {
                continue;
            }
            if current.get(key) != old.get(key) && current.get(key) != new.get(key) {
                return Err(DocError::Schema(
                    "review draft changed elsewhere; reload before editing".into(),
                ));
            }
            let mut fields = BTreeMap::new();
            fields.insert(
                format!("d:{}", derive_id(key, "review-draft")),
                match new.get(key) {
                    Some(draft) => json!(serde_json::to_string(draft)?),
                    None => Value::Null,
                },
            );
            updates.push(fields);
        }
        let ops = updates
            .into_iter()
            .map(|set| RowOp {
                kind: KIND_REVIEW_DRAFTS.into(),
                id: id.clone(),
                op: OpKind::Upsert,
                set: Some(set),
                hlc: hlc.clone(),
                clocks: None,
            })
            .collect();
        self.write_ops(ops)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ReviewAnchor, ReviewDraftSource, ReviewSide};

    #[test]
    fn drafts_round_trip_and_skip_unchanged_writes() {
        let mut registry = RegistryDoc::new("device");
        let target = "pr:owner/repo#42";
        let draft = ReviewDraft {
            id: "draft-1".into(),
            target: target.into(),
            body: "Please check".into(),
            source: ReviewDraftSource::User,
            created_at: 1,
            anchor: ReviewAnchor {
                file: "a.rs".into(),
                side: ReviewSide::Right,
                start_line: 2,
                end_line: 2,
                hunk_hash: Some("hash".into()),
            },
            line_hash: None,
            end_line_hash: None,
        };
        assert!(
            registry
                .put_review_drafts(target, std::slice::from_ref(&draft))
                .unwrap()
        );
        assert_eq!(registry.review_drafts(target), vec![draft.clone()]);
        assert!(!registry.put_review_drafts(target, &[draft]).unwrap());
        assert!(registry.put_review_drafts(target, &[]).unwrap());
        assert!(registry.review_drafts(target).is_empty());
    }

    #[test]
    fn posting_records_the_review_and_removes_only_its_drafts() {
        use mast_proto::{PostedReview, ReviewEvent, SubmitReviewComment};
        let mut registry = RegistryDoc::new("device");
        let target = "pr:owner/repo#42";
        let draft = |id: &str| ReviewDraft {
            id: id.into(),
            target: target.into(),
            body: "note".into(),
            source: ReviewDraftSource::User,
            created_at: 1,
            anchor: ReviewAnchor { file: "a.rs".into(), side: ReviewSide::Right, start_line: 2, end_line: 2, hunk_hash: None },
            line_hash: None,
            end_line_hash: None,
        };
        registry.put_review_drafts(target, &[draft("posted"), draft("kept")]).unwrap();
        let review = PostedReview {
            review_id: 9,
            url: "https://github.com/owner/repo/pull/42#pullrequestreview-9".into(),
            event: ReviewEvent::Comment,
            body: "summary".into(),
            posted_at: 5,
            nonce: None,
            comments: vec![SubmitReviewComment {
                draft_id: "posted".into(), path: "a.rs".into(), line: 2, side: ReviewSide::Right,
                start_line: None, start_side: None, body: "note".into(), line_text: String::new(), start_text: String::new(), context: vec![],
            }],
        };
        assert!(registry.record_posted_review(target, &review, &["posted".to_owned()], true).unwrap());
        assert_eq!(registry.review_drafts(target).iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["kept"]);
        assert_eq!(registry.posted_reviews(target), vec![review.clone()]);
        // Recording it again (a retried submit that found the review) changes nothing.
        assert!(!registry.record_posted_review(target, &review, &["posted".to_owned()], true).unwrap());
        assert_eq!(registry.posted_reviews(target).len(), 1);
    }

    #[test]
    fn nonce_persists_until_a_post_is_recorded() {
        use mast_proto::{PostedReview, ReviewEvent};
        let mut registry = RegistryDoc::new("device");
        let target = "pr:owner/repo#42";
        assert_eq!(registry.review_nonce(target), None);
        assert_eq!(registry.ensure_review_nonce(target, "first").unwrap(), "first");
        // A later candidate (a retry, or a restarted engine) gets the stored one.
        assert_eq!(registry.ensure_review_nonce(target, "second").unwrap(), "first");
        assert_eq!(registry.review_nonce(target).as_deref(), Some("first"));
        let review = PostedReview {
            review_id: 3, url: "u".into(), event: ReviewEvent::Comment, body: String::new(), posted_at: 1, comments: vec![], nonce: None,
        };
        assert!(registry.record_posted_review(target, &review, &["posted".to_owned()], true).unwrap());
        assert_eq!(registry.review_nonce(target), None, "confirmed post consumes the nonce");
        assert_eq!(registry.ensure_review_nonce(target, "third").unwrap(), "third");
        // A partial match records the review but keeps the nonce.
        let partial = PostedReview { review_id: 4, ..review.clone() };
        assert!(registry.record_posted_review(target, &partial, &[], false).unwrap());
        assert_eq!(registry.review_nonce(target).as_deref(), Some("third"));
        // Renewal is compare-and-set: a stale `expected` leaves the stored nonce alone.
        assert_eq!(registry.renew_review_nonce(target, "stale", "x").unwrap(), "third");
        assert_eq!(registry.review_nonce(target).as_deref(), Some("third"));
        registry.save_review_attempt(target, "third", &["a".into(), "b".into()]).unwrap();
        assert_eq!(registry.review_attempt_ids(target, "third"), Some(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(registry.review_attempt_ids(target, "other"), None);
        assert_eq!(registry.renew_review_nonce(target, "third", "fourth").unwrap(), "fourth");
        assert_eq!(registry.review_nonce(target).as_deref(), Some("fourth"));
        assert_eq!(registry.review_attempt_ids(target, "third"), None, "renewal clears the attempt");
    }

    #[test]
    fn many_drafts_use_bounded_operations() {
        let mut registry = RegistryDoc::new("device");
        let target = "branch:owner/repo#feature";
        let drafts: Vec<_> = (0..500)
            .map(|i| ReviewDraft {
                id: format!("draft-{i:04}"),
                target: target.into(),
                body: "x".repeat(200),
                source: ReviewDraftSource::User,
                created_at: 1,
                anchor: ReviewAnchor {
                    file: "a.rs".into(),
                    side: ReviewSide::Right,
                    start_line: i + 1,
                    end_line: i + 1,
                    hunk_hash: Some("hash".into()),
                },
                line_hash: None,
                end_line_hash: None,
            })
            .collect();
        assert!(registry.put_review_drafts(target, &drafts).unwrap());
        assert_eq!(registry.review_drafts(target), drafts);
    }

    #[test]
    fn dismissed_suggestions_survive_draft_updates() {
        let mut registry = RegistryDoc::new("device");
        let target = "pr:o/r#1";
        assert!(
            registry
                .dismiss_review_suggestion(target, "suggestion-1")
                .unwrap()
        );
        assert!(
            !registry
                .dismiss_review_suggestion(target, "suggestion-1")
                .unwrap()
        );
        assert_eq!(
            registry.dismissed_review_suggestions(target),
            vec!["suggestion-1"]
        );
        assert!(!registry.put_review_drafts(target, &[]).unwrap());
        assert_eq!(
            registry.dismissed_review_suggestions(target),
            vec!["suggestion-1"]
        );
    }

    #[test]
    fn accepting_is_atomic_and_stale_draft_updates_preserve_it() {
        let mut registry = RegistryDoc::new("device");
        let target = "pr:o/r#1";
        let draft = ReviewDraft {
            id: "one".into(),
            target: target.into(),
            body: "Check".into(),
            source: ReviewDraftSource::Agent,
            created_at: 1,
            line_hash: None,
            end_line_hash: None,
            anchor: ReviewAnchor {
                file: "a".into(),
                side: ReviewSide::Right,
                start_line: 1,
                end_line: 1,
                hunk_hash: None,
            },
        };
        assert!(
            registry
                .accept_review_suggestion(&draft, "suggestion")
                .unwrap()
        );
        assert_eq!(registry.review_drafts(target), vec![draft.clone()]);
        assert_eq!(
            registry.dismissed_review_suggestions(target),
            vec!["suggestion"]
        );
        assert!(
            !registry
                .accept_review_suggestion(&draft, "suggestion")
                .unwrap()
        );
        let mut other = draft.clone();
        other.id = "two".into();
        assert!(
            registry
                .update_review_drafts(target, &[], &[other.clone()])
                .unwrap()
        );
        assert_eq!(registry.review_drafts(target), vec![draft, other]);
    }
}
