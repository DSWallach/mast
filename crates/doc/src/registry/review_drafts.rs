//! Local review drafts, grouped by stable review target.
use super::*;
use mast_proto::{ReviewDraft, derive_id};

fn row_id(target: &str) -> String {
    derive_id(target, "review-drafts")
}

impl RegistryDoc {
    pub fn review_drafts(&self, target: &str) -> Vec<ReviewDraft> {
        let Some(row) = self.overlay_row(KIND_REVIEW_DRAFTS, &row_id(target)) else {
            return Vec::new();
        };
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return Vec::new();
        }
        row.fields
            .get("drafts")
            .and_then(Value::as_str)
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default()
    }

    /// Compare before writing to avoid needless registry operations.
    pub fn put_review_drafts(
        &mut self,
        target: &str,
        drafts: &[ReviewDraft],
    ) -> Result<bool, DocError> {
        if target.trim().is_empty() || drafts.iter().any(|draft| draft.target != target) {
            return Err(DocError::Schema(
                "review drafts need matching targets".into(),
            ));
        }
        if self.review_drafts(target) == drafts {
            return Ok(false);
        }
        self.write(
            KIND_REVIEW_DRAFTS,
            &row_id(target),
            OpKind::Upsert,
            fields([
                ("target", json!(target)),
                ("drafts", json!(serde_json::to_string(drafts)?)),
            ]),
        );
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
}
