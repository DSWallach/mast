//! LivingDocManifest, engine core: which documents a project keeps current,
//! which are owed an update, and which no longer match the code.
//!
//! Nothing here calls a model. Staleness comes from matching a landed commit's
//! paths against each row's watch globs ([`staleness`]), structural drift from
//! reading Markdown against the repo ([`drift`]), and handoffs get a bounded
//! digest ([`summary`]). Rows persist in one engine-local JSON file
//! ([`store`]); syncing them through the project document inventory is
//! deferred. Rows are the wire contracts of
//! `mast_proto::project_document_inventory`; state with no wire field (exempt
//! receipts, drift findings) is engine-local.
//!
//! See `docs/internal/living-docs.md`.

pub mod drift;
pub mod rules;
pub mod staleness;
pub mod store;
pub mod summary;

pub use mast_proto::project_document_inventory::{
    DocDeltaRef, DocDriftStatus, DocLedgerSummary, LivingDocClass, LivingDocRow,
};

pub use drift::{DriftFinding, DriftKind, FsRepoTree, KnownSymbols, RepoTree, check_markdown};
pub use rules::{SeedRule, default_seed_rules, merge_blast_radius, seed_rows, watch_matches};
pub use staleness::DocStatusChange;
pub use store::{ExemptReceipt, LivingDocStore, ProjectLedger};
pub use summary::{DocLedgerDigest, MAX_SUMMARY_TEXT_BYTES, build_doc_ledger_summary};

/// `doc_id` prefix of a document tracked in the repository.
pub const REPO_PREFIX: &str = "repo:";
/// `doc_id` prefix of a Mast artifact.
pub const ARTIFACT_PREFIX: &str = "artifact:";

/// The repository path of a `repo:` document.
pub fn repo_path(row: &LivingDocRow) -> Option<&str> {
    row.doc_id.strip_prefix(REPO_PREFIX)
}

/// Whether a hand-off must carry this row.
pub fn needs_attention(row: &LivingDocRow) -> bool {
    row.drift_status != DocDriftStatus::Fresh || !row.pending_deltas.is_empty()
}
