//! `commitProvenance` rows: commit sha → the Mast chat (and turn range) that
//! produced it. One row per commit, id = `derive_id(sha, "commit-provenance")`.
//! Written compare-before-write on HEAD moves of bound worktrees; never
//! deleted with the chat (the UI shows a record whose chat is gone as such).

use super::*;
use mast_proto::{CommitProvenance, ProvenanceSource, derive_id};

const ROW_VERSION: i64 = 1;
/// Records are small; the profile keeps at most this many (oldest dropped on
/// write) so a long-lived profile cannot grow without bound.
pub const MAX_COMMIT_PROVENANCE_ROWS: usize = 20_000;

fn row_id(sha: &str) -> String {
    derive_id(sha, "commit-provenance")
}

fn decode(row: &RegistryRow) -> Option<CommitProvenance> {
    let text = |key: &str| row.fields.get(key).and_then(Value::as_str);
    let sha = text("sha").filter(|v| !v.is_empty())?.to_owned();
    let chat_id = text("chatId").filter(|v| !v.is_empty())?.to_owned();
    Some(CommitProvenance {
        sha,
        patch_id: text("patchId").unwrap_or_default().to_owned(),
        chat_id,
        turn_start: text("turnStart").map(str::to_owned),
        turn_end: text("turnEnd").map(str::to_owned),
        recorded_at: row
            .fields
            .get("recordedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        source: text("source")
            .and_then(ProvenanceSource::parse)
            .unwrap_or(ProvenanceSource::Recorded),
        remapped_from: text("remappedFrom").map(str::to_owned),
    })
}

impl RegistryDoc {
    /// The record of one commit; `None` when absent or malformed.
    pub fn commit_provenance(&self, sha: &str) -> Option<CommitProvenance> {
        self.overlay_row(KIND_COMMIT_PROVENANCE, &row_id(sha))
            .and_then(|row| decode(&row))
    }

    /// Every decodable record, by sha. Not for hot paths: use
    /// [`commit_provenance`](Self::commit_provenance) per commit.
    pub fn all_commit_provenance(&self) -> Vec<CommitProvenance> {
        let mut rows: Vec<_> = self
            .overlay_rows(KIND_COMMIT_PROVENANCE)
            .iter()
            .filter_map(decode)
            .collect();
        rows.sort_by(|a, b| a.sha.cmp(&b.sha));
        rows
    }

    /// Store `record`. Compare-before-write (ignoring `recorded_at`): returns
    /// `false` and enqueues nothing when an identical record exists. An
    /// existing `Recorded` record is never replaced by a `Remapped` one.
    pub fn put_commit_provenance(&mut self, record: &CommitProvenance) -> Result<bool, DocError> {
        if record.sha.is_empty() || record.chat_id.is_empty() {
            return Err(DocError::Schema(
                "commitProvenance needs a sha and a chat id".into(),
            ));
        }
        if let Some(current) = self.commit_provenance(&record.sha) {
            let mut same = current.clone();
            same.recorded_at = record.recorded_at;
            if same == *record
                || (current.source == ProvenanceSource::Recorded
                    && record.source == ProvenanceSource::Remapped)
            {
                return Ok(false);
            }
        }
        self.write(
            KIND_COMMIT_PROVENANCE,
            &row_id(&record.sha),
            OpKind::Upsert,
            fields([
                ("sha", json!(record.sha)),
                ("patchId", json!(record.patch_id)),
                ("chatId", json!(record.chat_id)),
                ("turnStart", opt_str(record.turn_start.as_deref())),
                ("turnEnd", opt_str(record.turn_end.as_deref())),
                ("recordedAt", json!(record.recorded_at)),
                ("source", json!(record.source.as_str())),
                ("remappedFrom", opt_str(record.remapped_from.as_deref())),
                ("v", json!(ROW_VERSION)),
            ]),
        );
        Ok(true)
    }

    /// Drop the oldest records beyond [`MAX_COMMIT_PROVENANCE_ROWS`].
    /// Returns how many were deleted.
    pub fn prune_commit_provenance(&mut self) -> usize {
        let mut rows: Vec<_> = self
            .overlay_rows(KIND_COMMIT_PROVENANCE)
            .into_iter()
            .map(|row| {
                let at = row.fields.get("recordedAt").and_then(Value::as_i64).unwrap_or(0);
                (at, row.id)
            })
            .collect();
        if rows.len() <= MAX_COMMIT_PROVENANCE_ROWS {
            return 0;
        }
        rows.sort();
        let drop = rows.len() - MAX_COMMIT_PROVENANCE_ROWS;
        let keys: Vec<(&str, &str)> = rows[..drop]
            .iter()
            .map(|(_, id)| (KIND_COMMIT_PROVENANCE, id.as_str()))
            .collect();
        self.delete_row_ops(&keys);
        drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sha: &str, chat: &str) -> CommitProvenance {
        CommitProvenance {
            sha: sha.into(),
            patch_id: "pid".into(),
            chat_id: chat.into(),
            turn_start: Some("m1".into()),
            turn_end: Some("m3".into()),
            recorded_at: 10,
            source: ProvenanceSource::Recorded,
            remapped_from: None,
        }
    }

    fn doc() -> RegistryDoc {
        RegistryDoc::new("dev-a")
    }

    #[test]
    fn put_read_roundtrip_and_compare_before_write() {
        let mut doc = doc();
        assert!(doc.commit_provenance("aaa").is_none());
        assert!(doc.put_commit_provenance(&record("aaa", "chat-1")).unwrap());
        assert_eq!(doc.commit_provenance("aaa"), Some(record("aaa", "chat-1")));
        let mut again = record("aaa", "chat-1");
        again.recorded_at = 99;
        assert!(!doc.put_commit_provenance(&again).unwrap());
        assert_eq!(doc.all_commit_provenance().len(), 1);
    }

    #[test]
    fn remapped_never_replaces_recorded() {
        let mut doc = doc();
        doc.put_commit_provenance(&record("bbb", "chat-1")).unwrap();
        let mut remap = record("bbb", "chat-2");
        remap.source = ProvenanceSource::Remapped;
        remap.remapped_from = Some("aaa".into());
        assert!(!doc.put_commit_provenance(&remap).unwrap());
        assert_eq!(doc.commit_provenance("bbb").unwrap().chat_id, "chat-1");
    }

    #[test]
    fn row_id_fits_operation_limit_and_rejects_empty() {
        assert!(row_id(&"f".repeat(64)).len() <= 64);
        let mut doc = doc();
        assert!(doc.put_commit_provenance(&record("", "c")).is_err());
        assert!(doc.put_commit_provenance(&record("aaa", "")).is_err());
    }
}
