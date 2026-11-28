//! Commit-driven staleness, spending no model tokens: a landed commit that
//! changes watched code without changing the document owes it an update.

use serde::{Deserialize, Serialize};

use super::{DocDriftStatus, LivingDocRow, ProjectLedger, repo_path, watch_matches};

/// A row whose drift status a landed commit changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocStatusChange {
    pub doc_id: String,
    pub from: DocDriftStatus,
    pub to: DocDriftStatus,
    /// The landed commit that caused the change.
    pub commit: String,
}

/// Apply one landed commit to `ledger`.
///
/// `changed_paths` are the repo paths the commit changed. `blob_hash_of`
/// answers for the tree *after* the commit: a repo path for `repo:` rows, the
/// whole `artifact:<id>` for artifact rows (which no commit lists), and `None`
/// when there is nothing there.
///
/// A repo document was updated when the commit lists it and its hash differs
/// from the one on the row. An artifact was updated when its hash moved since
/// it was last recorded. An update clears the debt; otherwise a watch-glob
/// match owes one, unless an exempt receipt covers this document and commit.
/// Applying the same commit again changes nothing.
pub fn apply_land(
    ledger: &mut ProjectLedger,
    commit_sha: &str,
    changed_paths: &[String],
    blob_hash_of: impl Fn(&str) -> Option<String>,
) -> Vec<DocStatusChange> {
    let mut changes = Vec::new();
    if commit_sha.is_empty() {
        return changes;
    }
    let ProjectLedger {
        rows,
        exempt_receipts,
        findings,
        ..
    } = ledger;
    for row in rows.iter_mut() {
        // Already settled by this commit; a replay must not owe it again.
        if row.verified_at_commit.as_deref() == Some(commit_sha) {
            continue;
        }
        let from = row.drift_status;
        let in_commit =
            repo_path(row).is_some_and(|path| changed_paths.iter().any(|changed| changed == path));
        let hash = blob_hash_of(repo_path(row).unwrap_or(&row.doc_id));
        if in_commit && hash.is_none() {
            // The commit removed the document: nothing is left to be fresh.
            row.current_hash.clear();
            row.drift_status = DocDriftStatus::DriftDetected;
            row.owed_since_commit
                .get_or_insert_with(|| commit_sha.to_owned());
        } else if updated(row, in_commit, hash.as_deref()) {
            row.current_hash = hash.unwrap_or_default();
            row.verified_at_commit = Some(commit_sha.to_owned());
            row.owed_since_commit = None;
            row.drift_status = DocDriftStatus::Fresh;
            findings.remove(&row.doc_id);
        } else {
            // First sight, or a change that did not come through a land.
            if let Some(hash) = hash {
                row.current_hash = hash;
            }
            let watched = row
                .watch_globs
                .iter()
                .any(|glob| changed_paths.iter().any(|path| watch_matches(glob, path)));
            let exempt = exempt_receipts
                .iter()
                .any(|receipt| receipt.doc_id == row.doc_id && receipt.commit == commit_sha);
            if watched && !exempt {
                row.owed_since_commit
                    .get_or_insert_with(|| commit_sha.to_owned());
                // Detected drift is the stronger statement; keep it.
                if row.drift_status != DocDriftStatus::DriftDetected {
                    row.drift_status = DocDriftStatus::OwedUpdate;
                }
            }
        }
        if row.drift_status != from {
            changes.push(DocStatusChange {
                doc_id: row.doc_id.clone(),
                from,
                to: row.drift_status,
                commit: commit_sha.to_owned(),
            });
        }
    }
    changes
}

fn updated(row: &LivingDocRow, in_commit: bool, hash: Option<&str>) -> bool {
    let moved = hash.is_some_and(|hash| hash != row.current_hash);
    if repo_path(row).is_some() {
        in_commit && moved
    } else {
        // An artifact never seen before has no earlier hash to differ from.
        !row.current_hash.is_empty() && moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_docs::ExemptReceipt;
    use std::collections::HashMap;

    const DOC: &str = "docs/mcp.md";

    fn ledger() -> ProjectLedger {
        ProjectLedger {
            rows: vec![LivingDocRow {
                doc_id: format!("repo:{DOC}"),
                watch_globs: vec!["crates/mcp/src/tools/**".into()],
                current_hash: "d1".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn land(
        ledger: &mut ProjectLedger,
        commit: &str,
        paths: &[&str],
        doc_hash: &str,
    ) -> Vec<DocStatusChange> {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        let hashes = HashMap::from([(DOC.to_owned(), doc_hash.to_owned())]);
        apply_land(ledger, commit, &paths, |path| hashes.get(path).cloned())
    }

    const CODE: &str = "crates/mcp/src/tools/tool_catalog.rs";

    #[test]
    fn a_watched_change_owes_the_doc_and_keeps_the_earliest_commit() {
        let mut ledger = ledger();
        let changes = land(&mut ledger, "c1", &[CODE], "d1");
        assert_eq!(
            changes,
            [DocStatusChange {
                doc_id: "repo:docs/mcp.md".into(),
                from: DocDriftStatus::Fresh,
                to: DocDriftStatus::OwedUpdate,
                commit: "c1".into(),
            }]
        );
        assert!(land(&mut ledger, "c2", &[CODE], "d1").is_empty());
        assert_eq!(ledger.rows[0].owed_since_commit.as_deref(), Some("c1"));
    }

    #[test]
    fn unmatched_paths_are_ignored() {
        let mut ledger = ledger();
        let before = ledger.clone();
        assert!(
            land(
                &mut ledger,
                "c1",
                &["crates/ui/src/lib.rs", "README.md"],
                "d1"
            )
            .is_empty()
        );
        assert_eq!(ledger, before);
    }

    #[test]
    fn a_doc_changed_with_its_code_stays_fresh_even_when_replayed() {
        let mut ledger = ledger();
        for _ in 0..2 {
            assert!(land(&mut ledger, "c1", &[CODE, DOC], "d2").is_empty());
            let row = &ledger.rows[0];
            assert_eq!(row.drift_status, DocDriftStatus::Fresh);
            assert_eq!(row.current_hash, "d2");
            assert_eq!(row.verified_at_commit.as_deref(), Some("c1"));
            assert_eq!(row.owed_since_commit, None);
        }
    }

    #[test]
    fn a_later_doc_update_clears_the_debt() {
        let mut ledger = ledger();
        land(&mut ledger, "c1", &[CODE], "d1");
        let changes = land(&mut ledger, "c2", &[DOC], "d2");
        assert_eq!(changes[0].to, DocDriftStatus::Fresh);
        assert_eq!(ledger.rows[0].verified_at_commit.as_deref(), Some("c2"));
        assert_eq!(ledger.rows[0].owed_since_commit, None);
    }

    #[test]
    fn a_listed_doc_with_the_same_hash_is_not_an_update() {
        let mut ledger = ledger();
        land(&mut ledger, "c1", &[CODE, DOC], "d1");
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::OwedUpdate);
    }

    #[test]
    fn an_unlisted_hash_move_does_not_clear_the_debt() {
        let mut ledger = ledger();
        land(&mut ledger, "c1", &[CODE], "d1");
        land(&mut ledger, "c2", &["README.md"], "d9");
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::OwedUpdate);
        assert_eq!(ledger.rows[0].current_hash, "d9");
    }

    #[test]
    fn an_exempt_receipt_covers_only_its_doc_and_commit() {
        let mut ledger = ledger();
        ledger.exempt_receipts.push(ExemptReceipt {
            doc_id: "repo:docs/mcp.md".into(),
            commit: "c1".into(),
            reason: "rename only".into(),
        });
        assert!(land(&mut ledger, "c1", &[CODE], "d1").is_empty());
        assert_eq!(ledger.rows[0].owed_since_commit, None);
        land(&mut ledger, "c2", &[CODE], "d1");
        assert_eq!(ledger.rows[0].owed_since_commit.as_deref(), Some("c2"));
    }

    #[test]
    fn a_deleted_doc_is_drift() {
        let mut ledger = ledger();
        apply_land(&mut ledger, "c1", &[DOC.to_owned()], |_| None);
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::DriftDetected);
        assert_eq!(ledger.rows[0].current_hash, "");
    }

    #[test]
    fn an_artifact_row_clears_when_its_hash_moves() {
        let mut ledger = ProjectLedger {
            rows: vec![LivingDocRow {
                doc_id: "artifact:plan".into(),
                watch_globs: vec!["crates/**".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let code = [CODE.to_owned()];
        // First sight records the hash and does not count as an update.
        apply_land(&mut ledger, "c1", &code, |id| {
            (id == "artifact:plan").then(|| "r1".into())
        });
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::OwedUpdate);
        apply_land(&mut ledger, "c2", &code, |_| Some("r2".into()));
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::Fresh);
    }
}
