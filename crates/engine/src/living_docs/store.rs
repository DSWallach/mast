//! The engine-side ledger: one JSON file under the profile's data directory,
//! holding every project's rows, exempt receipts and drift findings.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::{
    DocDriftStatus, DocStatusChange, DriftFinding, KnownSymbols, LivingDocClass, LivingDocRow,
    RepoTree, check_markdown, staleness::apply_land,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LedgerFile {
    projects: BTreeMap<String, ProjectLedger>,
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
            Ok(bytes) => serde_json::from_slice(&bytes)
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
    /// project's rows. See [`apply_land`] for what `blob_hash_of` answers.
    /// Record any exempt receipt for the commit before calling this.
    /// `blob_hash_of` runs under the store lock and must not call back in.
    pub fn on_landed(
        &self,
        project: &str,
        commit_sha: &str,
        changed_paths: &[String],
        blob_hash_of: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<DocStatusChange>, String> {
        self.update(project, |ledger| {
            apply_land(ledger, commit_sha, changed_paths, blob_hash_of)
        })
    }

    /// Record that `commit` needs no update to `doc_id`. It suppresses the
    /// debt when that commit lands; it does not clear an earlier one.
    pub fn record_exempt(
        &self,
        project: &str,
        doc_id: &str,
        commit: &str,
        reason: &str,
    ) -> Result<(), String> {
        let reason = reason.trim();
        if doc_id.is_empty() || commit.is_empty() || reason.is_empty() {
            return Err("an exempt receipt needs a document, a commit and a reason".to_owned());
        }
        self.update(project, |ledger| {
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
        })
    }

    /// Run the structural check over every row `read_doc` can read as text
    /// (it gets the `doc_id`). Changelogs are history and are skipped. Rows
    /// with findings become `DriftDetected`; a drifted row that now checks
    /// clean returns to `OwedUpdate` if still owed, else `Fresh`. Call it
    /// after [`Self::on_landed`] with the same commit. `tree` and `read_doc`
    /// run under the store lock and must not call back in.
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
                if row.repo_path().is_some_and(|path| !path.ends_with(".md")) {
                    continue;
                }
                let Some(text) = read_doc(&row.doc_id) else {
                    continue;
                };
                let found = check_markdown(&text, row.repo_path(), known, tree);
                let from = row.drift_status;
                if !found.is_empty() {
                    row.drift_status = DocDriftStatus::DriftDetected;
                    ledger.findings.insert(row.doc_id.clone(), found);
                } else {
                    ledger.findings.remove(&row.doc_id);
                    if from == DocDriftStatus::DriftDetected {
                        row.drift_status = if row.owed_since_commit.is_some() {
                            DocDriftStatus::OwedUpdate
                        } else {
                            DocDriftStatus::Fresh
                        };
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
        let changes = store
            .on_landed(
                "p",
                "c1",
                &[
                    "crates/ui/Cargo.toml".into(),
                    "crates/engine/src/peer_git.rs".into(),
                ],
                |_| Some("h".into()),
            )
            .unwrap();
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
    fn a_corrupt_file_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STORE_FILE), "{nope").unwrap();
        assert!(LivingDocStore::open(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(STORE_FILE)).unwrap(),
            "{nope"
        );
    }

    #[test]
    fn a_failed_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        let store = LivingDocStore::open(&blocked).unwrap();
        std::fs::write(&blocked, "a file where the directory should be").unwrap();
        assert!(store.record_exempt("p", "repo:a.md", "c1", "why").is_err());
        assert_eq!(store.ledger("p"), ProjectLedger::default());
    }

    #[test]
    fn an_exempt_receipt_needs_a_reason_and_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = LivingDocStore::open(dir.path()).unwrap();
        assert!(store.record_exempt("p", "repo:a.md", "c1", "  ").is_err());
        store
            .record_exempt("p", "repo:a.md", "c1", "first")
            .unwrap();
        store
            .record_exempt("p", "repo:a.md", "c1", "second")
            .unwrap();
        let receipts = store.ledger("p").exempt_receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].reason, "second");
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
