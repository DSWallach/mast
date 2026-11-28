//! Commit-driven staleness, spending no model tokens: a landed commit that
//! changes watched code without changing the document owes it an update.

use serde::{Deserialize, Serialize};

use super::{DocDriftStatus, ProjectLedger, repo_path, watch_matches};

/// Landed commits remembered per project, to tell when an exemption is late.
const MAX_LANDED_COMMITS: usize = 512;

/// A row whose drift status a landed commit changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocStatusChange {
    pub doc_id: String,
    pub from: DocDriftStatus,
    pub to: DocDriftStatus,
    /// The landed commit that caused the change; empty for an artifact
    /// publication.
    pub commit: String,
}

/// Apply one landed commit to `ledger`.
///
/// `changed_paths` are the repo paths the commit changed, with **both** sides
/// of every rename: `git diff --no-renames --name-only base..landed`. A rename
/// reported only by its new path hides that the old document is gone and that
/// code left a watched directory.
///
/// `parent_hash_of` and `blob_hash_of` give a repo path's blob id before
/// (`base`) and after (`landed`) the commit; `Ok(None)` means no file. Return
/// `Err` when a lookup fails: the land is then abandoned and `ledger` is left
/// as it was, so a failure never reads as a deletion.
///
/// A repo document was updated when the commit lists it and its two blobs
/// differ, whatever the row last recorded. An update clears the debt;
/// otherwise a watch-glob match owes one, unless an exempt receipt covers this
/// document and commit. Artifacts are never in a commit, so only the watch
/// rule applies to them; their revisions arrive through
/// [`super::LivingDocStore::on_artifact_published`].
///
/// Apply each land once, in landing order. Only an immediate replay of the
/// commit that last verified a row is ignored.
pub fn apply_land(
    ledger: &mut ProjectLedger,
    commit_sha: &str,
    changed_paths: &[String],
    parent_hash_of: impl Fn(&str) -> Result<Option<String>, String>,
    blob_hash_of: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<Vec<DocStatusChange>, String> {
    let mut changes = Vec::new();
    if commit_sha.is_empty() {
        return Ok(changes);
    }
    // Work on a copy: a failed lookup part way must change nothing.
    let mut next = ledger.clone();
    let ProjectLedger {
        rows,
        exempt_receipts,
        findings,
        landed_commits,
    } = &mut next;
    for row in rows.iter_mut() {
        // Already settled by this commit; a replay must not owe it again.
        if row.verified_at_commit.as_deref() == Some(commit_sha) {
            continue;
        }
        let from = row.drift_status;
        let (mut deleted, mut updated) = (false, false);
        if let Some(path) = repo_path(row) {
            let listed = changed_paths.iter().any(|changed| changed == path);
            let after = blob_hash_of(path)?;
            if listed {
                deleted = after.is_none();
                updated = !deleted && parent_hash_of(path)? != after;
            }
            if let Some(after) = after {
                row.current_hash = after;
            }
        }
        if deleted {
            // The commit removed the document: nothing is left to be fresh.
            row.current_hash.clear();
            row.drift_status = DocDriftStatus::DriftDetected;
            row.owed_since_commit
                .get_or_insert_with(|| commit_sha.to_owned());
        } else if updated {
            row.verified_at_commit = Some(commit_sha.to_owned());
            row.owed_since_commit = None;
            row.drift_status = DocDriftStatus::Fresh;
            findings.remove(&row.doc_id);
        } else {
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
    if !landed_commits.iter().any(|landed| landed == commit_sha) {
        landed_commits.push(commit_sha.to_owned());
        let excess = landed_commits.len().saturating_sub(MAX_LANDED_COMMITS);
        landed_commits.drain(..excess);
    }
    *ledger = next;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_docs::{ExemptReceipt, LivingDocRow};
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

    /// Land `commit`, with the doc's blob `before` and `after` it.
    fn land_blobs(
        ledger: &mut ProjectLedger,
        commit: &str,
        paths: &[&str],
        before: &str,
        after: &str,
    ) -> Vec<DocStatusChange> {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        let blob = |hash: &str| {
            let hashes = HashMap::from([(DOC.to_owned(), hash.to_owned())]);
            move |path: &str| Ok(hashes.get(path).cloned())
        };
        apply_land(ledger, commit, &paths, blob(before), blob(after)).unwrap()
    }

    /// A land that finds the doc at the row's blob and leaves it at `after`.
    fn land(
        ledger: &mut ProjectLedger,
        commit: &str,
        paths: &[&str],
        after: &str,
    ) -> Vec<DocStatusChange> {
        let before = ledger.rows[0].current_hash.clone();
        let before = if paths.contains(&DOC) {
            before
        } else {
            after.to_owned()
        };
        land_blobs(ledger, commit, paths, &before, after)
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
        assert_eq!(ledger.rows, before.rows);
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
    fn seeding_after_the_commit_does_not_owe_a_doc_it_changed() {
        let mut ledger = ledger();
        // The row already holds the blob the commit produced.
        ledger.rows[0].current_hash = "d2".into();
        assert!(land_blobs(&mut ledger, "c1", &[CODE, DOC], "d1", "d2").is_empty());
        assert_eq!(ledger.rows[0].verified_at_commit.as_deref(), Some("c1"));
    }

    #[test]
    fn a_deleted_doc_is_drift() {
        let mut ledger = ledger();
        // `--no-renames` lists the old side of a rename.
        let paths = [DOC.to_owned(), "docs/renamed.md".to_owned()];
        apply_land(
            &mut ledger,
            "c1",
            &paths,
            |_| Ok(Some("d1".into())),
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::DriftDetected);
        assert_eq!(ledger.rows[0].current_hash, "");
    }

    #[test]
    fn a_failed_lookup_abandons_the_whole_land() {
        let mut ledger = ledger();
        ledger.rows.push(LivingDocRow {
            doc_id: "repo:docs/second.md".into(),
            ..Default::default()
        });
        let before = ledger.clone();
        let paths = [CODE.to_owned(), DOC.to_owned()];
        let after = |path: &str| match path {
            DOC => Ok(Some("d2".to_owned())),
            _ => Err("git failed".to_owned()),
        };
        assert!(apply_land(&mut ledger, "c1", &paths, |_| Ok(None), after).is_err());
        let parent = |_: &str| Err("git failed".to_owned());
        assert!(apply_land(&mut ledger, "c1", &paths, parent, |_| Ok(Some("d2".into()))).is_err());
        assert_eq!(ledger, before);
    }

    #[test]
    fn only_the_watch_rule_applies_to_an_artifact_on_land() {
        let mut ledger = ProjectLedger {
            rows: vec![LivingDocRow {
                doc_id: "artifact:plan".into(),
                watch_globs: vec!["crates/**".into()],
                current_hash: "r1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let lookup = |path: &str| -> Result<Option<String>, String> {
            panic!("no commit lists an artifact: {path}")
        };
        apply_land(&mut ledger, "c1", &[CODE.to_owned()], lookup, lookup).unwrap();
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::OwedUpdate);
        assert_eq!(ledger.rows[0].current_hash, "r1");
    }
}
