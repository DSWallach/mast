//! `commitProvenance` rows: commit sha → the Mast chat (and turn range) that
//! produced it. One row per commit, id = `derive_id(sha, "commit-provenance")`.
//! Written compare-before-write on HEAD moves of bound worktrees; never
//! deleted with the chat (the UI shows a record whose chat is gone as such).

use super::*;
use mast_proto::{CommitProvenance, ProvenanceSource, derive_id};

const ROW_VERSION: i64 = 1;
/// Rows kept per repository (oldest dropped first).
pub const MAX_COMMIT_PROVENANCE_ROWS_PER_REPO: usize = 5_000;
/// Pruning waits until a repo is this far over its cap, so a steady stream of
/// records deletes in batches instead of one row per write.
const PRUNE_SLACK: usize = 250;

fn row_id(sha: &str) -> String {
    derive_id(sha, "commit-provenance")
}

fn decode(row: &RegistryRow) -> Option<CommitProvenance> {
    let text = |key: &str| row.fields.get(key).and_then(Value::as_str);
    let sha = text("sha").filter(|v| !v.is_empty())?.to_owned();
    let chat_id = text("chatId").filter(|v| !v.is_empty())?.to_owned();
    Some(CommitProvenance {
        sha,
        repo_id: text("repoId").unwrap_or_default().to_owned(),
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

    /// Store `record` for a sha that has none. First writer wins: an existing
    /// row (recorded or remapped, for any chat) is never replaced, so a
    /// coordinator merging worker branches cannot take over their commits, and
    /// a remap only ever lands on new shas. Returns `false` and enqueues
    /// nothing when a row exists.
    pub fn put_commit_provenance(&mut self, record: &CommitProvenance) -> Result<bool, DocError> {
        if record.sha.is_empty() || record.chat_id.is_empty() {
            return Err(DocError::Schema(
                "commitProvenance needs a sha and a chat id".into(),
            ));
        }
        if self.commit_provenance(&record.sha).is_some() {
            return Ok(false);
        }
        self.write(
            KIND_COMMIT_PROVENANCE,
            &row_id(&record.sha),
            OpKind::Upsert,
            fields([
                ("sha", json!(record.sha)),
                ("repoId", json!(record.repo_id)),
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

    /// Drop the oldest records of `repo_id` beyond
    /// [`MAX_COMMIT_PROVENANCE_ROWS_PER_REPO`], in one batched tombstone op.
    /// A scan without a sort unless the repo is over its cap by the slack.
    /// Returns how many were deleted.
    pub fn prune_commit_provenance(&mut self, repo_id: &str) -> usize {
        let mut rows: Vec<(i64, String)> = self
            .overlay_rows(KIND_COMMIT_PROVENANCE)
            .into_iter()
            .filter(|row| {
                row.fields.get("repoId").and_then(Value::as_str).unwrap_or_default() == repo_id
            })
            .map(|row| {
                let at = row.fields.get("recordedAt").and_then(Value::as_i64).unwrap_or(0);
                (at, row.id)
            })
            .collect();
        if rows.len() <= MAX_COMMIT_PROVENANCE_ROWS_PER_REPO + PRUNE_SLACK {
            return 0;
        }
        rows.sort();
        let drop = rows.len() - MAX_COMMIT_PROVENANCE_ROWS_PER_REPO;
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
            repo_id: "repo-1".into(),
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
    fn first_writer_wins_for_any_chat_and_source() {
        // A coordinator merging worker branches must not take their commits.
        let mut doc = doc();
        doc.put_commit_provenance(&record("bbb", "worker-1")).unwrap();
        assert!(!doc.put_commit_provenance(&record("bbb", "coordinator")).unwrap());
        let mut remap = record("bbb", "worker-2");
        remap.source = ProvenanceSource::Remapped;
        remap.remapped_from = Some("aaa".into());
        assert!(!doc.put_commit_provenance(&remap).unwrap());
        assert_eq!(doc.commit_provenance("bbb").unwrap().chat_id, "worker-1");
        // A remap still lands on a new sha.
        remap.sha = "ccc".into();
        assert!(doc.put_commit_provenance(&remap).unwrap());
    }

    #[test]
    fn row_id_fits_operation_limit_and_rejects_empty() {
        assert!(row_id(&"f".repeat(64)).len() <= 64);
        let mut doc = doc();
        assert!(doc.put_commit_provenance(&record("", "c")).is_err());
        assert!(doc.put_commit_provenance(&record("aaa", "")).is_err());
    }

    #[test]
    fn prune_is_per_repo_oldest_first_and_batched_past_the_slack() {
        let mut doc = doc();
        let total = MAX_COMMIT_PROVENANCE_ROWS_PER_REPO + PRUNE_SLACK + 10;
        for i in 0..total {
            let mut r = record(&format!("{i:040x}"), "chat-1");
            r.recorded_at = i as i64;
            doc.put_commit_provenance(&r).unwrap();
        }
        let mut other = record(&format!("{:040x}", 999_999), "chat-2");
        other.repo_id = "repo-2".into();
        doc.put_commit_provenance(&other).unwrap();
        // Another repo is under its cap: nothing deleted.
        assert_eq!(doc.prune_commit_provenance("repo-2"), 0);
        let dropped = doc.prune_commit_provenance("repo-1");
        assert_eq!(dropped, total - MAX_COMMIT_PROVENANCE_ROWS_PER_REPO);
        assert!(doc.commit_provenance(&format!("{:040x}", 0)).is_none(), "oldest dropped");
        assert!(doc.commit_provenance(&format!("{:040x}", total - 1)).is_some());
        assert!(doc.commit_provenance(&format!("{:040x}", 999_999)).is_some());
        // Within the slack again: no more work.
        assert_eq!(doc.prune_commit_provenance("repo-1"), 0);
    }
}
