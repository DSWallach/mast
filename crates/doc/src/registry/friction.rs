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

const COUNTER_PREFIX: &str = "occurrence:";

fn decode(row: &RegistryRow) -> Option<FrictionReport> {
    let value = Value::Object(row.fields.clone().into_iter().collect());
    let mut report: FrictionReport = serde_json::from_value(value).ok()?;
    let counters: Vec<&Value> = row
        .fields
        .iter()
        .filter_map(|(name, value)| name.starts_with(COUNTER_PREFIX).then_some(value))
        .collect();
    if !counters.is_empty() {
        report.occurrences = counters
            .iter()
            .filter_map(|counter| counter.get("count").and_then(Value::as_u64))
            .fold(0, u64::saturating_add);
        report.last_seen = counters
            .iter()
            .filter_map(|counter| counter.get("lastSeen").and_then(Value::as_i64))
            .max()
            .unwrap_or(report.last_seen);
    }
    Some(report)
}

impl RegistryDoc {
    pub fn friction_reports(&self, space_id: &str) -> Vec<FrictionReport> {
        self.overlay_rows(KIND_FRICTION_REPORTS)
            .iter()
            .filter_map(decode)
            .filter(|report| report.provenance.space_id == space_id)
            .collect()
    }

    /// Record an occurrence with a per-device counter, so concurrent devices merge.
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
        let state = self.row_state(KIND_FRICTION_REPORTS, &report.id);
        if state == RowState::Tombstoned {
            return Err(DocError::Schema("friction report was deleted".into()));
        }
        let existing = self.overlay_row(KIND_FRICTION_REPORTS, &report.id);
        let counter_key = format!(
            "{COUNTER_PREFIX}{}",
            derive_id(&self.device_id, "friction-device")
        );
        let counter = existing
            .as_ref()
            .and_then(|row| row.fields.get(&counter_key));
        let count = counter
            .and_then(|v| v.get("count"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(1);
        let last_seen = counter
            .and_then(|v| v.get("lastSeen"))
            .and_then(Value::as_i64)
            .unwrap_or(report.provenance.created_at)
            .max(report.provenance.created_at);
        report.occurrences = 1;
        report.last_seen = report.provenance.created_at;
        report.status = FrictionStatus::Open;
        let mut fields: BTreeMap<String, Value> = if existing.is_some() {
            BTreeMap::new()
        } else {
            let Value::Object(map) = serde_json::to_value(&report)? else {
                unreachable!()
            };
            map.into_iter().collect()
        };
        fields.insert(counter_key, json!({"count": count, "lastSeen": last_seen}));
        self.observe_row(KIND_FRICTION_REPORTS, &report.id);
        let op = RowOp {
            kind: KIND_FRICTION_REPORTS.into(),
            id: report.id.clone(),
            op: if existing.is_some() {
                OpKind::Update
            } else {
                OpKind::Upsert
            },
            set: Some(fields),
            hlc: self.next_hlc(),
            clocks: None,
        };
        check_op_size(&op)?;
        self.write_ops(vec![op])?;
        self.overlay_row(KIND_FRICTION_REPORTS, &report.id)
            .as_ref()
            .and_then(decode)
            .ok_or_else(|| DocError::Schema("friction report could not be read".into()))
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
        assert_eq!(doc.friction_reports("space"), vec![second.clone()]);
        let restored = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "device").unwrap();
        assert_eq!(restored.friction_reports("space"), vec![second]);
    }
    #[test]
    fn friction_concurrent_devices_preserve_occurrences() {
        let mut first = RegistryDoc::new("device-a");
        let mut second = RegistryDoc::new("device-b");
        let a = first
            .record_friction(report("space", "chat-a", "failure", 1))
            .unwrap();
        second
            .record_friction(report("space", "chat-b", "failure", 2))
            .unwrap();
        let other_op = second.pending[0].ops[0].clone();
        let current = first.overlay_row(KIND_FRICTION_REPORTS, &a.id).unwrap();
        let merged = apply_op(Some(&current), &other_op).0.unwrap();
        let merged = decode(&merged).unwrap();
        assert_eq!(merged.occurrences, 2);
        assert_eq!(merged.last_seen, 2);
    }
    #[test]
    fn friction_fingerprint_includes_kind_and_command() {
        let base = report("space", "chat", "failure", 1);
        let mut changed = base.clone();
        changed.kind = FrictionKind::Permission;
        assert_ne!(friction_fingerprint(&base), friction_fingerprint(&changed));
        changed = base.clone();
        changed.approval = Some(mast_proto::FrictionApproval {
            command: "cargo test".into(),
            why: "permission".into(),
        });
        assert_ne!(friction_fingerprint(&base), friction_fingerprint(&changed));
    }
}
