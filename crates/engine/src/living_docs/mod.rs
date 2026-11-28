//! LivingDocManifest, engine core: which documents a project keeps current,
//! which are owed an update, and which no longer match the code.
//!
//! Nothing here calls a model. Staleness comes from matching a landed commit's
//! paths against each row's watch globs ([`staleness`]), structural drift from
//! reading Markdown against the repo ([`drift`]), and handoffs get a bounded
//! digest ([`summary`]). Rows persist in one engine-local JSON file
//! ([`store`]); syncing them through the project document inventory is
//! deferred, so the types here are engine-local. They share the JSON shape of
//! the contracts in `mast_proto::project_document_inventory` and convert into
//! them, but read unknown enum values instead of failing.
//!
//! See `docs/internal/living-docs.md`.

pub mod drift;
pub mod rules;
pub mod staleness;
pub mod store;
pub mod summary;

use mast_proto::project_document_inventory as wire;
use serde::{Deserialize, Serialize};

pub use drift::{DriftFinding, DriftKind, FsRepoTree, KnownSymbols, RepoTree, check_markdown};
pub use rules::{SeedRule, default_seed_rules, merge_blast_radius, seed_rows, watch_matches};
pub use staleness::DocStatusChange;
pub use store::{ExemptReceipt, LivingDocStore, ProjectLedger};
pub use summary::{
    CanonicalPins, DocLedgerSummary, MAX_SUMMARY_TEXT_BYTES, build_doc_ledger_summary,
};

/// `doc_id` prefix of a document tracked in the repository.
pub const REPO_PREFIX: &str = "repo:";
/// `doc_id` prefix of a Mast artifact.
pub const ARTIFACT_PREFIX: &str = "artifact:";

/// The document class, which decides who writes it. An unknown value reads as
/// [`LivingDocClass::Item`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LivingDocClass {
    Spec,
    Arch,
    Subsystem,
    Changelog,
    #[default]
    #[serde(other)]
    Item,
}

/// Where a document stands. An unknown value reads as
/// [`DocDriftStatus::PendingReview`], so a newer writer's debt is never read
/// as fresh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocDriftStatus {
    #[default]
    Fresh,
    OwedUpdate,
    DriftDetected,
    #[serde(other)]
    PendingReview,
}

/// A queued worker note about a shared documentation update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DocDeltaRef {
    pub work_item_id: String,
    pub summary: String,
}

/// One tracked document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LivingDocRow {
    /// `repo:<path>` or `artifact:<id>`.
    pub doc_id: String,
    pub class: LivingDocClass,
    /// The project root or Lead container item that owns the document.
    pub owner_scope_item_id: String,
    pub watch_globs: Vec<String>,
    pub linked_req_ids: Vec<String>,
    /// Git blob id of a repo document, or an artifact revision's hash. Empty
    /// until first observed.
    pub current_hash: String,
    pub verified_at_commit: Option<String>,
    /// The earliest landed commit that changed watched code without the doc.
    pub owed_since_commit: Option<String>,
    pub pending_deltas: Vec<DocDeltaRef>,
    pub drift_status: DocDriftStatus,
}

impl LivingDocRow {
    /// The repository path of a `repo:` document.
    pub fn repo_path(&self) -> Option<&str> {
        self.doc_id.strip_prefix(REPO_PREFIX)
    }

    /// Whether a handoff must carry this row.
    pub fn needs_attention(&self) -> bool {
        self.drift_status != DocDriftStatus::Fresh || !self.pending_deltas.is_empty()
    }
}

impl From<LivingDocClass> for wire::LivingDocClass {
    fn from(value: LivingDocClass) -> Self {
        match value {
            LivingDocClass::Spec => Self::Spec,
            LivingDocClass::Arch => Self::Arch,
            LivingDocClass::Subsystem => Self::Subsystem,
            LivingDocClass::Changelog => Self::Changelog,
            LivingDocClass::Item => Self::Item,
        }
    }
}

impl From<DocDriftStatus> for wire::DocDriftStatus {
    fn from(value: DocDriftStatus) -> Self {
        match value {
            DocDriftStatus::Fresh => Self::Fresh,
            DocDriftStatus::OwedUpdate => Self::OwedUpdate,
            DocDriftStatus::DriftDetected => Self::DriftDetected,
            DocDriftStatus::PendingReview => Self::PendingReview,
        }
    }
}

impl From<LivingDocRow> for wire::LivingDocRow {
    fn from(row: LivingDocRow) -> Self {
        Self {
            doc_id: row.doc_id,
            class: row.class.into(),
            owner_scope_item_id: row.owner_scope_item_id,
            watch_globs: row.watch_globs,
            linked_req_ids: row.linked_req_ids,
            current_hash: row.current_hash,
            verified_at_commit: row.verified_at_commit,
            owed_since_commit: row.owed_since_commit,
            pending_deltas: row
                .pending_deltas
                .into_iter()
                .map(|delta| wire::DocDeltaRef {
                    work_item_id: delta.work_item_id,
                    summary: delta.summary,
                })
                .collect(),
            drift_status: row.drift_status.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_read_missing_fields_and_unknown_enum_values() {
        let row: LivingDocRow = serde_json::from_str(r#"{"docId":"repo:a.md"}"#).unwrap();
        assert_eq!(row.class, LivingDocClass::Item);
        assert_eq!(row.drift_status, DocDriftStatus::Fresh);
        let row: LivingDocRow = serde_json::from_str(
            r#"{"class":"runbook","driftStatus":"quarantined","laterField":1}"#,
        )
        .unwrap();
        assert_eq!(row.class, LivingDocClass::Item);
        assert_eq!(row.drift_status, DocDriftStatus::PendingReview);
    }

    #[test]
    fn rows_share_the_wire_json_shape() {
        let row = LivingDocRow {
            doc_id: "repo:docs/mcp.md".into(),
            class: LivingDocClass::Arch,
            verified_at_commit: Some("abc".into()),
            pending_deltas: vec![DocDeltaRef {
                work_item_id: "w".into(),
                summary: "s".into(),
            }],
            drift_status: DocDriftStatus::OwedUpdate,
            ..Default::default()
        };
        let wire_row = wire::LivingDocRow::from(row.clone());
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::to_value(&wire_row).unwrap()
        );
    }
}
