//! Mutable reading state, keyed by a PR or diff-target identity.
use super::*;
use mast_proto::{DiffReviewState, derive_id};

fn row_id(target: &str) -> String {
    derive_id(target, "diff-review")
}

impl RegistryDoc {
    pub fn diff_review_state(&self, target: &str) -> Option<DiffReviewState> {
        let row = self.overlay_row(KIND_DIFF_REVIEW_STATE, &row_id(target))?;
        if row.fields.get("target").and_then(Value::as_str) != Some(target) {
            return None;
        }
        let mut state = DiffReviewState {
            target: target.to_owned(),
            head_sha: row
                .fields
                .get("headSha")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Default::default()
        };
        for (field, value) in &row.fields {
            if let Some(hash) = field.strip_prefix("h:") {
                if value.as_bool() == Some(true) {
                    state.viewed_hunks.insert(hash.to_owned());
                }
            } else if field.starts_with("f:") {
                if let Some(path) = value.as_str() {
                    state.viewed_files.insert(path.to_owned());
                }
            }
        }
        Some(state)
    }

    /// Write only changed fields so large reviews remain below the per-op cap.
    pub fn put_diff_review_state(&mut self, state: &DiffReviewState) -> Result<bool, DocError> {
        if state.target.trim().is_empty() {
            return Err(DocError::Schema("diff review state needs a target".into()));
        }
        let before = self
            .diff_review_state(&state.target)
            .unwrap_or_else(|| DiffReviewState {
                target: state.target.clone(),
                ..Default::default()
            });
        let id = row_id(&state.target);
        self.observe_row(KIND_DIFF_REVIEW_STATE, &id);
        let mut updates = Vec::new();
        if self.diff_review_state(&state.target).is_none() || before.head_sha != state.head_sha {
            updates.push(fields([
                ("target", json!(state.target)),
                ("headSha", opt_str(state.head_sha.as_deref())),
            ]));
        }
        for hash in before
            .viewed_hunks
            .symmetric_difference(&state.viewed_hunks)
        {
            let mut set = BTreeMap::new();
            set.insert(
                format!("h:{hash}"),
                json!(state.viewed_hunks.contains(hash)),
            );
            updates.push(set);
        }
        for path in before
            .viewed_files
            .symmetric_difference(&state.viewed_files)
        {
            let mut set = BTreeMap::new();
            set.insert(
                format!("f:{}", derive_id(path, "diff-review-file")),
                if state.viewed_files.contains(path) {
                    json!(path)
                } else {
                    Value::Null
                },
            );
            updates.push(set);
        }
        if updates.is_empty() {
            return Ok(false);
        }
        let hlc = self.mint_hlc();
        let ops = updates
            .into_iter()
            .map(|set| RowOp {
                kind: KIND_DIFF_REVIEW_STATE.into(),
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

    pub fn delete_diff_review_state(&mut self, target: &str) -> bool {
        let id = row_id(target);
        if !self.row_exists(KIND_DIFF_REVIEW_STATE, &id) {
            return false;
        }
        self.delete_row_ops(&[(KIND_DIFF_REVIEW_STATE, &id)]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_review_state_roundtrip() {
        let mut registry = RegistryDoc::new("device");
        let mut state = DiffReviewState {
            target: "pr:owner/repo#42".into(),
            head_sha: Some("abc".into()),
            ..Default::default()
        };
        state.viewed_hunks.insert("a".repeat(64));
        state.viewed_files.insert("src/a.rs".into());
        assert!(registry.put_diff_review_state(&state).unwrap());
        assert_eq!(
            registry.diff_review_state(&state.target),
            Some(state.clone())
        );
        assert!(!registry.put_diff_review_state(&state).unwrap());
        state.head_sha = Some("def".into());
        state.viewed_files.clear();
        assert!(registry.put_diff_review_state(&state).unwrap());
        assert_eq!(
            registry.diff_review_state(&state.target),
            Some(state.clone())
        );
        assert!(registry.delete_diff_review_state(&state.target));
        assert_eq!(registry.diff_review_state(&state.target), None);
    }
    #[test]
    fn large_review_splits_registry_operations() {
        let mut registry = RegistryDoc::new("device");
        let mut state = DiffReviewState {
            target: "pr:owner/repo#42".into(),
            ..Default::default()
        };
        for index in 0..500 {
            state.viewed_hunks.insert(format!("{index:064x}"));
        }
        assert!(registry.put_diff_review_state(&state).unwrap());
        assert_eq!(registry.diff_review_state(&state.target), Some(state));
    }
}
