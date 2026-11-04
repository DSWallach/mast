//! Per-Space friction reports.
use super::*;
use mast_proto::{FrictionReport, FrictionStatus, derive_id};

pub const KIND_FRICTION_REPORTS: &str = "frictionReports";

/// Fingerprint independent of the originating chat and Space.
pub fn friction_fingerprint(report: &FrictionReport) -> String {
    let summary = report
        .summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let command = report
        .approval
        .as_ref()
        .map(|a| a.command.as_str())
        .unwrap_or("");
    let kind = serde_json::to_string(&report.kind).unwrap_or_default();
    derive_id(
        &format!("{kind}\0{summary}\0{command}"),
        "friction-fingerprint",
    )
}

impl RegistryDoc {
    pub fn friction_reports(&self, space_id: &str) -> Vec<FrictionReport> {
        self.overlay_rows(KIND_FRICTION_REPORTS)
            .iter()
            .filter_map(|row| {
                let value = Value::Object(row.fields.clone().into_iter().collect());
                serde_json::from_value::<FrictionReport>(value).ok()
            })
            .filter(|report| report.provenance.space_id == space_id)
            .collect()
    }

    /// Record one occurrence, preserving the first report's details.
    pub fn record_friction(
        &mut self,
        mut report: FrictionReport,
    ) -> Result<FrictionReport, DocError> {
        if report.summary.trim().is_empty()
            || report.summary.chars().count() > 140
            || report.what_happened.trim().is_empty()
            || report.provenance.space_id.is_empty()
        {
            return Err(DocError::Schema(
                "friction requires a Space, summary of at most 140 characters, and what happened"
                    .into(),
            ));
        }
        report.fingerprint = friction_fingerprint(&report);
        report.id = derive_id(
            &format!("{}\0{}", report.provenance.space_id, report.fingerprint),
            "friction-report",
        );
        let existing = self
            .friction_reports(&report.provenance.space_id)
            .into_iter()
            .find(|r| r.id == report.id);
        let update = existing.is_some();
        if let Some(mut previous) = existing {
            previous.occurrences = previous.occurrences.saturating_add(1);
            previous.last_seen = previous.last_seen.max(report.provenance.created_at);
            report = previous;
        } else {
            report.occurrences = 1;
            report.last_seen = report.provenance.created_at;
            report.status = FrictionStatus::Open;
        }
        if self.row_state(KIND_FRICTION_REPORTS, &report.id) == RowState::Tombstoned {
            return Err(DocError::Schema("friction report was deleted".into()));
        }
        self.observe_row(KIND_FRICTION_REPORTS, &report.id);
        let Value::Object(map) = serde_json::to_value(&report)? else {
            unreachable!()
        };
        let op = RowOp {
            kind: KIND_FRICTION_REPORTS.into(),
            id: report.id.clone(),
            op: if update {
                OpKind::Update
            } else {
                OpKind::Upsert
            },
            set: Some(map.into_iter().collect()),
            hlc: self.next_hlc(),
            clocks: None,
        };
        check_op_size(&op)?;
        self.write_ops(vec![op])?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{FrictionKind, FrictionProvenance, FrictionSource};

    fn report(space: &str, chat: &str, summary: &str, time: i64) -> FrictionReport {
        FrictionReport {
            id: String::new(),
            kind: FrictionKind::Bug,
            summary: summary.into(),
            what_happened: "Failure".into(),
            workaround: None,
            suggestion: None,
            approval: None,
            severity: None,
            links: vec![],
            provenance: FrictionProvenance {
                chat_id: chat.into(),
                harness: "codex".into(),
                model: None,
                space_id: space.into(),
                work_item_id: None,
                branch: None,
                created_at: time,
                source: FrictionSource::Agent,
            },
            fingerprint: String::new(),
            occurrences: 0,
            last_seen: 0,
            status: FrictionStatus::Open,
        }
    }

    #[test]
    fn friction_round_trip_and_deduplication() {
        let mut doc = RegistryDoc::new("device");
        let first = doc
            .record_friction(report("space", "chat-1", "Test  failure", 1))
            .unwrap();
        assert_eq!(doc.friction_reports("space"), vec![first.clone()]);
        let second = doc
            .record_friction(report("space", "chat-2", " test failure ", 2))
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.occurrences, 2);
        assert_eq!(second.last_seen, 2);
        assert_eq!(second.provenance.chat_id, "chat-1");
        let other = doc
            .record_friction(report("other", "chat-2", "test failure", 3))
            .unwrap();
        assert_ne!(second.id, other.id);
        assert_eq!(doc.friction_reports("space"), vec![second]);
    }
}
