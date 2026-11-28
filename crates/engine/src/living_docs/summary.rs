//! The documentation digest a Manager hand-off carries: the canonical pins and
//! every document still owed work, bounded in size and in a stable order.

use mast_proto::project_document_inventory::{ResyncJob, ResyncStatus};
use serde::{Deserialize, Serialize};

use super::{DocDriftStatus, DocLedgerSummary, LivingDocRow, needs_attention};

/// Most bytes [`DocLedgerSummary::text`] takes.
pub const MAX_SUMMARY_TEXT_BYTES: usize = 1536;

/// Bytes kept free for the "more rows" line.
const TAIL_RESERVE: usize = 96;
/// Bytes, so even four-byte characters cannot push the header past the cap.
const MAX_FIELD_BYTES: usize = 128;

/// The ledger snapshot and its bounded rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DocLedgerDigest {
    /// `pending_rows` holds every row needing attention, most urgent first,
    /// then by `doc_id`. `resync_job` is set only while a job is checking,
    /// awaiting review or applying.
    pub summary: DocLedgerSummary,
    /// Rows left out of `text` to keep it within the cap.
    pub omitted_from_text: usize,
    pub text: String,
}

/// Summarize the rows that are not fresh or have queued deltas. `pins` carries
/// the canonical pin fields; its rows and job are replaced.
pub fn build_doc_ledger_summary(
    pins: DocLedgerSummary,
    rows: &[LivingDocRow],
    resync_job: Option<&ResyncJob>,
) -> DocLedgerDigest {
    let mut pending_rows: Vec<LivingDocRow> = rows
        .iter()
        .filter(|row| needs_attention(row))
        .cloned()
        .collect();
    pending_rows.sort_by(|a, b| {
        urgency(a.drift_status)
            .cmp(&urgency(b.drift_status))
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
    let resync_job = resync_job
        .filter(|job| {
            matches!(
                job.status,
                ResyncStatus::Checking | ResyncStatus::ReadyForReview | ResyncStatus::Applying
            )
        })
        .cloned();
    let summary = DocLedgerSummary {
        pending_rows,
        resync_job,
        ..pins
    };
    let (text, omitted_from_text) = render(&summary, MAX_SUMMARY_TEXT_BYTES);
    DocLedgerDigest {
        summary,
        omitted_from_text,
        text,
    }
}

/// The digest a hand-off prompt carries: every row owed work and every queued
/// delta, with no size cap. A successor must not lose a row to the bounded
/// [`DocLedgerSummary`] text, and the row count is the number of tracked
/// documents, which is small.
pub fn render_handoff_section(summary: &DocLedgerSummary) -> String {
    let (mut text, _) = render(summary, usize::MAX);
    for row in &summary.pending_rows {
        for delta in &row.pending_deltas {
            text.push_str(&format!(
                "  delta for {}: from {}: {}\n",
                field(&row.doc_id),
                field(&delta.work_item_id),
                field(&delta.summary)
            ));
        }
    }
    text
}

fn urgency(status: DocDriftStatus) -> u8 {
    match status {
        DocDriftStatus::DriftDetected => 0,
        DocDriftStatus::OwedUpdate => 1,
        DocDriftStatus::PendingReview => 2,
        DocDriftStatus::Fresh => 3,
    }
}

fn render(pins: &DocLedgerSummary, cap: usize) -> (String, usize) {
    let rows = &pins.pending_rows;
    let mut text = String::from("Documentation ledger\n");
    let revision = |rev: Option<u32>| rev.map(|rev| format!(" rev {rev}")).unwrap_or_default();
    if let Some(id) = &pins.spec_artifact_id {
        let hash = pins
            .spec_sha256
            .as_deref()
            .map(|hash| format!(" sha256 {}", field(hash)))
            .unwrap_or_default();
        text.push_str(&format!("Spec: artifact:{}{hash}\n", field(id)));
    }
    if let Some(id) = &pins.primary_plan_id {
        text.push_str(&format!(
            "Plan: artifact:{}{}\n",
            field(id),
            revision(pins.primary_plan_rev)
        ));
    }
    if let Some(id) = &pins.matrix_artifact_id {
        text.push_str(&format!(
            "Matrix: artifact:{}{}\n",
            field(id),
            revision(pins.matrix_rev)
        ));
    }
    if let Some(status) = pins.resync_job.as_ref().map(|job| job.status) {
        let status = match status {
            ResyncStatus::Checking => "checking",
            ResyncStatus::ReadyForReview => "ready for review",
            _ => "applying",
        };
        text.push_str(&format!("Re-sync: {status}\n"));
    }
    if rows.is_empty() {
        text.push_str("All tracked documents are fresh.\n");
        return (text, 0);
    }
    text.push_str(&format!("Documents owed work: {}\n", rows.len()));
    for (index, row) in rows.iter().enumerate() {
        let status = match row.drift_status {
            DocDriftStatus::DriftDetected => "drift",
            DocDriftStatus::OwedUpdate => "owed",
            DocDriftStatus::PendingReview => "review",
            DocDriftStatus::Fresh => "deltas",
        };
        let mut line = format!("- [{status}] {}", field(&row.doc_id));
        if let Some(commit) = &row.owed_since_commit {
            let short: String = field(commit).chars().take(10).collect();
            line.push_str(&format!(" since {short}"));
        }
        if !row.pending_deltas.is_empty() {
            line.push_str(&format!(" ({} pending deltas)", row.pending_deltas.len()));
        }
        line.push('\n');
        let reserve = if index + 1 == rows.len() {
            0
        } else {
            TAIL_RESERVE
        };
        if text.len() + line.len() + reserve > cap {
            let omitted = rows.len() - index;
            text.push_str(&format!(
                "... and {omitted} more; read the ledger for the rest.\n"
            ));
            return (text, omitted);
        }
        text.push_str(&line);
    }
    (text, 0)
}

/// One bounded line: an id never adds lines to the digest.
fn field(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if out.len() + c.len_utf8() > MAX_FIELD_BYTES {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_docs::DocDeltaRef;

    fn row(id: &str, status: DocDriftStatus) -> LivingDocRow {
        LivingDocRow {
            doc_id: id.into(),
            drift_status: status,
            ..Default::default()
        }
    }

    fn pins() -> DocLedgerSummary {
        DocLedgerSummary {
            spec_artifact_id: Some("spec".into()),
            spec_sha256: Some("ab".repeat(32)),
            primary_plan_id: Some("plan".into()),
            primary_plan_rev: Some(4),
            matrix_artifact_id: Some("matrix".into()),
            matrix_rev: Some(2),
            ..Default::default()
        }
    }

    #[test]
    fn only_rows_needing_attention_appear_in_a_stable_order() {
        let mut queued = row("repo:q.md", DocDriftStatus::Fresh);
        queued.pending_deltas.push(DocDeltaRef::default());
        let mut rows = vec![
            queued,
            row("repo:b.md", DocDriftStatus::OwedUpdate),
            row("repo:fresh.md", DocDriftStatus::Fresh),
            row("repo:a.md", DocDriftStatus::OwedUpdate),
            row("repo:z.md", DocDriftStatus::DriftDetected),
        ];
        let first = build_doc_ledger_summary(pins(), &rows, None);
        rows.reverse();
        assert_eq!(build_doc_ledger_summary(pins(), &rows, None), first);
        let ids: Vec<&str> = first
            .summary
            .pending_rows
            .iter()
            .map(|r| r.doc_id.as_str())
            .collect();
        assert_eq!(ids, ["repo:z.md", "repo:a.md", "repo:b.md", "repo:q.md"]);
        assert!(!first.text.contains("fresh.md"));
        assert!(first.text.contains("Plan: artifact:plan rev 4"));
        assert!(first.text.contains("[deltas] repo:q.md (1 pending deltas)"));
        assert_eq!(first.omitted_from_text, 0);
    }

    #[test]
    fn the_text_stays_within_the_cap_and_counts_what_it_left_out() {
        let rows: Vec<LivingDocRow> = (0..200)
            .map(|n| {
                let mut row = row(
                    &format!("repo:docs/{n:03}-{}.md", "é".repeat(200)),
                    DocDriftStatus::OwedUpdate,
                );
                row.owed_since_commit = Some("0123456789abcdef".into());
                row
            })
            .collect();
        let mut huge = pins();
        huge.spec_artifact_id = Some("x\n".repeat(5000));
        huge.spec_sha256 = Some("𝔼".repeat(5000));
        huge.primary_plan_id = Some("𝔼".repeat(5000));
        huge.matrix_artifact_id = Some("𝔼".repeat(5000));
        for pins in [pins(), huge] {
            let summary = build_doc_ledger_summary(pins, &rows, None);
            assert!(
                summary.text.len() <= MAX_SUMMARY_TEXT_BYTES,
                "{}",
                summary.text.len()
            );
            assert_eq!(summary.summary.pending_rows.len(), 200);
            let listed = summary
                .text
                .lines()
                .filter(|line| line.starts_with("- ["))
                .count();
            assert_eq!(listed + summary.omitted_from_text, 200);
            assert!(summary.text.contains("more; read the ledger"));
        }
    }

    #[test]
    fn only_a_job_in_flight_is_reported() {
        let job = |status| ResyncJob {
            status,
            ..Default::default()
        };
        let summary =
            build_doc_ledger_summary(pins(), &[], Some(&job(ResyncStatus::ReadyForReview)));
        assert!(summary.text.contains("Re-sync: ready for review"));
        assert!(summary.text.contains("All tracked documents are fresh."));
        let summary = build_doc_ledger_summary(pins(), &[], Some(&job(ResyncStatus::Applied)));
        assert_eq!(summary.summary.resync_job, None);
    }
}
