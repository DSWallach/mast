//! The engine-side ledger: one JSON file under the profile's data directory,
//! holding every project's rows, exempt receipts and drift findings.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ARTIFACT_PREFIX, DocDriftStatus, DocStatusChange, DriftFinding, DriftKind, KnownSymbols,
    LivingDocClass, LivingDocRow, RepoTree, check_markdown, repo_path, staleness::apply_land,
};

/// File name inside the directory given to [`LivingDocStore::open`].
pub const STORE_FILE: &str = "living-docs.json";

/// A declaration that `commit` needs no update to `doc_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExemptReceipt {
    pub doc_id: String,
    pub commit: String,
    pub reason: String,
}

/// One project's living documents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectLedger {
    pub rows: Vec<LivingDocRow>,
    pub exempt_receipts: Vec<ExemptReceipt>,
    /// What the last structural check found, by `doc_id`.
    pub findings: BTreeMap<String, Vec<DriftFinding>>,
    /// Recently applied lands, oldest first.
    pub landed_commits: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LedgerFile {
    projects: BTreeMap<String, ProjectLedger>,
}

/// Decode the file. The wire row enums are strict, so a value written by a
/// newer engine is replaced first, as is `null` or a non-string: such a status reads as pending review
/// (never fresh) and an unknown class as an item doc.
fn parse(bytes: &[u8]) -> serde_json::Result<LedgerFile> {
    let mut file: Value = serde_json::from_slice(bytes)?;
    let rows = file
        .get_mut("projects")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|projects| projects.values_mut())
        .filter_map(|ledger| ledger.get_mut("rows")?.as_array_mut())
        .flatten();
    for row in rows {
        replace_unknown::<LivingDocClass>(row, "class", LivingDocClass::Item)?;
        replace_unknown::<DocDriftStatus>(row, "driftStatus", DocDriftStatus::PendingReview)?;
    }
    serde_json::from_value(file)
}

fn replace_unknown<T: Serialize + DeserializeOwned>(
    row: &mut Value,
    key: &str,
    fallback: T,
) -> serde_json::Result<()> {
    if let Some(value) = row.get_mut(key)
        && T::deserialize(&*value).is_err()
    {
        *value = serde_json::to_value(fallback)?;
    }
    Ok(())
}

/// The persisted ledgers. Every change is written before it is visible, so a
/// failed write leaves both the file and memory as they were. One engine owns
/// the file.
pub struct LivingDocStore {
    path: PathBuf,
    state: Mutex<LedgerFile>,
}

impl LivingDocStore {
    /// Load `dir/living-docs.json`; a missing file is an empty ledger. A file
    /// that cannot be read or parsed is an error, and is left in place.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let path = dir.join(STORE_FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => parse(&bytes)
                .map_err(|error| format!("living docs ledger {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => LedgerFile::default(),
            Err(error) => return Err(format!("living docs ledger {}: {error}", path.display())),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// A copy of one project's ledger (empty when it has none).
    pub fn ledger(&self, project: &str) -> ProjectLedger {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.projects.get(project).cloned().unwrap_or_default()
    }

    /// Change one project's ledger and persist it.
    pub fn update<T>(
        &self,
        project: &str,
        change: impl FnOnce(&mut ProjectLedger) -> T,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let before = state.projects.get(project).cloned();
        let mut ledger = before.clone().unwrap_or_default();
        let result = change(&mut ledger);
        if ledger != before.unwrap_or_default() {
            let mut next = state.clone();
            next.projects.insert(project.to_owned(), ledger);
            self.write(&next)?;
            *state = next;
        }
        Ok(result)
    }

    /// The entry point for a land step: apply the landed commit to the
    /// project's rows. [`apply_land`] states what the arguments must hold.
    /// Record any exempt receipt for the commit before calling this. The
    /// lookups run under the store lock and must not call back in.
    pub fn on_landed(
        &self,
        project: &str,
        commit_sha: &str,
        changed_paths: &[String],
        parent_hash_of: impl Fn(&str) -> Result<Option<String>, String>,
        blob_hash_of: impl Fn(&str) -> Result<Option<String>, String>,
    ) -> Result<Vec<DocStatusChange>, String> {
        self.update(project, |ledger| {
            apply_land(
                ledger,
                commit_sha,
                changed_paths,
                parent_hash_of,
                blob_hash_of,
            )
        })?
    }

    /// Call when an artifact gets a new revision. A hash that differs from the
    /// recorded one updates the document **now**: it clears the debt owed so
    /// far, and later lands can owe it again. An untracked artifact, or the
    /// same hash again, changes nothing.
    pub fn on_artifact_published(
        &self,
        project: &str,
        doc_id: &str,
        hash: &str,
    ) -> Result<Option<DocStatusChange>, String> {
        if !doc_id.starts_with(ARTIFACT_PREFIX) || hash.is_empty() {
            return Err("a publication needs an artifact: document and its hash".to_owned());
        }
        self.update(project, |ledger| {
            let row = ledger
                .rows
                .iter_mut()
                .find(|row| row.doc_id == doc_id && row.current_hash != hash)?;
            let from = row.drift_status;
            row.current_hash = hash.to_owned();
            row.owed_since_commit = None;
            row.drift_status = DocDriftStatus::Fresh;
            ledger.findings.remove(doc_id);
            (from != DocDriftStatus::Fresh).then(|| DocStatusChange {
                doc_id: doc_id.to_owned(),
                from,
                to: DocDriftStatus::Fresh,
                commit: String::new(),
            })
        })
    }

    /// Record that `commit` (the SHA that lands, not the one a worker tested)
    /// needs no update to the tracked document `doc_id`. Returns whether the
    /// receipt can still apply: `false` means that commit already landed, and
    /// any debt it created stays. A receipt never clears earlier debt.
    pub fn record_exempt(
        &self,
        project: &str,
        doc_id: &str,
        commit: &str,
        reason: &str,
    ) -> Result<bool, String> {
        let reason = reason.trim();
        if commit.is_empty() || reason.is_empty() {
            return Err("an exempt receipt needs a commit and a reason".to_owned());
        }
        self.update(project, |ledger| {
            if !ledger.rows.iter().any(|row| row.doc_id == doc_id) {
                return Err(format!("no tracked document {doc_id}"));
            }
            let receipt = ExemptReceipt {
                doc_id: doc_id.to_owned(),
                commit: commit.to_owned(),
                reason: reason.to_owned(),
            };
            match ledger
                .exempt_receipts
                .iter_mut()
                .find(|existing| existing.doc_id == doc_id && existing.commit == commit)
            {
                Some(existing) => *existing = receipt,
                None => ledger.exempt_receipts.push(receipt),
            }
            Ok(!ledger.landed_commits.iter().any(|landed| landed == commit))
        })?
    }

    /// Run the structural check over every row `read_doc` can read as text
    /// (it gets the `doc_id`). Changelogs are history and are skipped. Rows
    /// with findings become `DriftDetected`; a drifted row that now checks
    /// clean returns to `OwedUpdate` if still owed, else `Fresh`. A
    /// `PendingReview` row keeps its status, with findings stored. When a
    /// known set is empty its check does not run, and what it found earlier
    /// stays. Call this after [`Self::on_landed`] with the same commit.
    ///
    /// This blocks on file reads while holding the store lock: call it off
    /// async threads. `tree` and `read_doc` must not call back in.
    pub fn check_drift(
        &self,
        project: &str,
        commit_sha: &str,
        known: &KnownSymbols,
        tree: &dyn RepoTree,
        read_doc: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<DocStatusChange>, String> {
        self.update(project, |ledger| {
            let mut changes = Vec::new();
            for row in &mut ledger.rows {
                if row.class == LivingDocClass::Changelog {
                    continue;
                }
                if repo_path(row).is_some_and(|path| !path.ends_with(".md")) {
                    continue;
                }
                let Some(text) = read_doc(&row.doc_id) else {
                    continue;
                };
                let mut found = check_markdown(&text, repo_path(row), known, tree);
                if let Some(earlier) = ledger.findings.remove(&row.doc_id) {
                    found.extend(earlier.into_iter().filter(|finding| match finding.kind {
                        DriftKind::UnknownTool => known.tool_names.is_empty(),
                        DriftKind::UnknownCliSubcommand => known.cli_subcommands.is_empty(),
                        _ => false,
                    }));
                }
                let from = row.drift_status;
                let clean = found.is_empty();
                if !clean {
                    ledger.findings.insert(row.doc_id.clone(), found);
                }
                // Only a review, not this check, ends a pending review.
                row.drift_status = match from {
                    DocDriftStatus::PendingReview => from,
                    _ if !clean => DocDriftStatus::DriftDetected,
                    DocDriftStatus::DriftDetected if row.owed_since_commit.is_some() => {
                        DocDriftStatus::OwedUpdate
                    }
                    DocDriftStatus::DriftDetected => DocDriftStatus::Fresh,
                    other => other,
                };
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
        })
    }

    fn write(&self, state: &LedgerFile) -> Result<(), String> {
        let fail = |error: &dyn std::fmt::Display| {
            format!("living docs ledger {}: {error}", self.path.display())
        };
        let dir = self
            .path
            .parent()
            .ok_or_else(|| fail(&"no parent directory"))?;
        std::fs::create_dir_all(dir).map_err(|error| fail(&error))?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| fail(&error))?;
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|error| fail(&error))?;
        file.write_all(&bytes).map_err(|error| fail(&error))?;
        file.as_file().sync_all().map_err(|error| fail(&error))?;
        file.persist(&self.path).map_err(|error| fail(&error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_docs::{FsRepoTree, default_seed_rules, seed_rows};

    #[test]
    fn the_ledger_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        store
            .update("p", |ledger| {
                seed_rows(&mut ledger.rows, default_seed_rules(), "p", |_| {
                    Some("h".into())
                });
            })
            .unwrap();
        store
            .record_exempt("p", "repo:ARCHITECTURE.md", "c1", "version bump")
            .unwrap();
        let paths = [
            "crates/ui/Cargo.toml".into(),
            "crates/engine/src/peer_git.rs".into(),
        ];
        let hash = |_: &str| Ok(Some("h".to_owned()));
        let changes = store.on_landed("p", "c1", &paths, hash, hash).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].doc_id, "repo:docs/internal/peer-link.md");
        let before = store.ledger("p");
        drop(store);
        let reopened = LivingDocStore::open(dir.path()).unwrap();
        assert_eq!(reopened.ledger("p"), before);
        assert_eq!(reopened.ledger("other"), ProjectLedger::default());
    }

    #[test]
    fn a_file_without_newer_fields_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STORE_FILE),
            r#"{"projects":{"p":{"rows":[{"docId":"repo:a.md"}]}}}"#,
        )
        .unwrap();
        let ledger = LivingDocStore::open(dir.path()).unwrap().ledger("p");
        assert_eq!(ledger.rows[0].doc_id, "repo:a.md");
        assert!(ledger.exempt_receipts.is_empty() && ledger.findings.is_empty());
    }

    #[test]
    fn unknown_row_enum_values_load_without_reading_as_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = r#"{"projects":{"p":{"laterField":1,"rows":[
            {"docId":"repo:a.md","class":"runbook","driftStatus":"quarantined","laterField":1},
            {"docId":"repo:b.md","class":"spec","driftStatus":"owedUpdate"},
            {"docId":"repo:c.md","class":null,"driftStatus":7}]}}}"#;
        std::fs::write(dir.path().join(STORE_FILE), file).unwrap();
        let rows = LivingDocStore::open(dir.path()).unwrap().ledger("p").rows;
        assert_eq!(
            (rows[0].class, rows[0].drift_status),
            (LivingDocClass::Item, DocDriftStatus::PendingReview)
        );
        assert_eq!(
            (rows[2].class, rows[2].drift_status),
            (LivingDocClass::Item, DocDriftStatus::PendingReview)
        );
        assert_eq!(
            (rows[1].class, rows[1].drift_status),
            (LivingDocClass::Spec, DocDriftStatus::OwedUpdate)
        );
    }

    #[test]
    fn a_corrupt_file_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STORE_FILE), "{nope").unwrap();
        assert!(LivingDocStore::open(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(STORE_FILE)).unwrap(),
            "{nope"
        );
    }

    fn track(store: &LivingDocStore, row: LivingDocRow) {
        store.update("p", |ledger| ledger.rows.push(row)).unwrap();
    }

    fn doc(doc_id: &str) -> LivingDocRow {
        LivingDocRow {
            doc_id: doc_id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_failed_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        let store = LivingDocStore::open(&blocked).unwrap();
        std::fs::write(&blocked, "a file where the directory should be").unwrap();
        assert!(
            store
                .update("p", |ledger| ledger.rows.push(doc("repo:a.md")))
                .is_err()
        );
        assert_eq!(store.ledger("p"), ProjectLedger::default());
    }

    #[test]
    fn an_exempt_receipt_is_checked_recorded_once_and_says_when_it_is_late() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        track(&store, doc("repo:a.md"));
        assert!(store.record_exempt("p", "repo:a.md", "c1", "  ").is_err());
        assert!(
            store
                .record_exempt("p", "repo:typo.md", "c1", "why")
                .is_err()
        );
        assert_eq!(
            store.record_exempt("p", "repo:a.md", "c1", "first"),
            Ok(true)
        );
        assert_eq!(
            store.record_exempt("p", "repo:a.md", "c1", "second"),
            Ok(true)
        );
        let receipts = store.ledger("p").exempt_receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].reason, "second");
        let blob = |_: &str| Ok(Some("h".to_owned()));
        store.on_landed("p", "c1", &[], blob, blob).unwrap();
        assert_eq!(
            store.record_exempt("p", "repo:a.md", "c1", "late"),
            Ok(false)
        );
    }

    #[test]
    fn an_artifact_revised_before_a_watched_land_is_still_owed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        track(
            &store,
            LivingDocRow {
                watch_globs: vec!["crates/**".into()],
                current_hash: "r1".into(),
                ..doc("artifact:plan")
            },
        );
        let paths = ["crates/a.rs".to_owned()];
        let none = |_: &str| Ok(None);
        let publish = |hash| {
            store
                .on_artifact_published("p", "artifact:plan", hash)
                .unwrap()
        };
        store.on_landed("p", "c1", &paths, none, none).unwrap();
        assert_eq!(publish("r1"), None);
        assert_eq!(
            store.ledger("p").rows[0].owed_since_commit.as_deref(),
            Some("c1")
        );
        assert_eq!(
            publish("r2").map(|change| change.to),
            Some(DocDriftStatus::Fresh)
        );
        assert_eq!(store.ledger("p").rows[0].owed_since_commit, None);
        store.on_landed("p", "c9", &paths, none, none).unwrap();
        let row = store.ledger("p").rows.remove(0);
        assert_eq!(row.drift_status, DocDriftStatus::OwedUpdate);
        assert_eq!(row.owed_since_commit.as_deref(), Some("c9"));
        assert_eq!(
            store.on_artifact_published("p", "artifact:other", "h"),
            Ok(None)
        );
        assert!(store.on_artifact_published("p", "repo:a.md", "h").is_err());
    }

    #[test]
    fn the_drift_check_keeps_a_pending_review() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        track(
            &store,
            LivingDocRow {
                drift_status: DocDriftStatus::PendingReview,
                ..doc("repo:a.md")
            },
        );
        let tree = FsRepoTree::new(dir.path());
        for (text, found) in [("[gone](gone.md)", 1), ("fine", 0)] {
            let changes = store
                .check_drift("p", "c1", &KnownSymbols::default(), &tree, |_| {
                    Some(text.into())
                })
                .unwrap();
            assert!(changes.is_empty());
            let ledger = store.ledger("p");
            assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::PendingReview);
            assert_eq!(ledger.findings.len(), found);
        }
    }

    #[test]
    fn a_check_that_did_not_run_keeps_what_it_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        track(&store, doc("repo:a.md"));
        let tree = FsRepoTree::new(dir.path());
        let known = KnownSymbols {
            tool_names: ["read_chat".to_owned()].into(),
            cli_subcommands: ["mcp".to_owned()].into(),
        };
        let text = |_: &str| Some("`work_queue_gone` `mast gone`".to_owned());
        store.check_drift("p", "c1", &known, &tree, text).unwrap();
        let before = store.ledger("p");
        assert_eq!(before.findings["repo:a.md"].len(), 2);
        store
            .check_drift("p", "c2", &KnownSymbols::default(), &tree, text)
            .unwrap();
        assert_eq!(store.ledger("p"), before);
    }

    #[test]
    fn the_drift_check_flags_rows_and_restores_them_when_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        store
            .update("p", |ledger| {
                for (doc_id, class) in [
                    ("repo:a.md", LivingDocClass::Arch),
                    ("repo:log.md", LivingDocClass::Changelog),
                ] {
                    ledger.rows.push(LivingDocRow {
                        doc_id: doc_id.into(),
                        class,
                        owed_since_commit: Some("c0".into()),
                        drift_status: DocDriftStatus::OwedUpdate,
                        ..Default::default()
                    });
                }
            })
            .unwrap();
        let tree = FsRepoTree::new(dir.path());
        let known = KnownSymbols::default();
        let changes = store
            .check_drift("p", "c1", &known, &tree, |_| Some("[gone](gone.md)".into()))
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].to, DocDriftStatus::DriftDetected);
        assert_eq!(store.ledger("p").findings["repo:a.md"][0].symbol, "gone.md");
        store
            .check_drift("p", "c2", &known, &tree, |_| Some("fine".into()))
            .unwrap();
        let ledger = store.ledger("p");
        assert_eq!(ledger.rows[0].drift_status, DocDriftStatus::OwedUpdate);
        assert!(ledger.findings.is_empty());
    }
}
