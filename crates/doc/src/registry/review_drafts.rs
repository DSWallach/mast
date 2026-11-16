//! Local review drafts, grouped by stable review target.
use super::*;
use mast_proto::{ReviewDraft, derive_id};

fn row_id(target: &str) -> String {
    derive_id(target, "review-drafts")
}

impl RegistryDoc {
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

    /// Compare before writing to avoid needless registry operations.
    pub fn put_review_drafts(
        &mut self,
        target: &str,
        drafts: &[ReviewDraft],
    ) -> Result<bool, DocError> {
        let mut ids = std::collections::HashSet::new();
        if target.trim().is_empty()
            || drafts.iter().any(|draft| {
                draft.target != target || draft.id.trim().is_empty() || !ids.insert(&draft.id)
            })
        {
            return Err(DocError::Schema(
                "review drafts need matching targets".into(),
            ));
        }
        let before = self.review_drafts(target);
        let old: BTreeMap<_, _> = before
            .iter()
            .map(|draft| (draft.id.clone(), draft))
            .collect();
        let new: BTreeMap<_, _> = drafts
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
                .put_review_drafts(target, &[draft.clone()])
                .unwrap()
        );
        assert_eq!(registry.review_drafts(target), vec![draft.clone()]);
        assert!(!registry.put_review_drafts(target, &[draft]).unwrap());
        assert!(registry.put_review_drafts(target, &[]).unwrap());
        assert!(registry.review_drafts(target).is_empty());
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
}
