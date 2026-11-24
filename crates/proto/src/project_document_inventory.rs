//! Synced observations for a project documentation inventory task, and the
//! re-sync job state recorded beside them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryArtifact {
    pub id: String,
    pub owner_item_id: String,
    pub title: String,
    pub revision: u32,
    pub primary_plan: bool,
    /// Hash of the current revision's stored bytes. Empty on rows recorded
    /// before re-sync existed.
    #[serde(default)]
    pub content_hash: String,
}

/// Where a re-sync job stands. `Checking` and `Applying` are recoverable: a
/// restart resumes or fails them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResyncStatus {
    #[default]
    Idle,
    Checking,
    Aligned,
    ReadyForReview,
    Applying,
    Applied,
    Declined,
    Failed,
}

/// One re-sync run. The proposal body lives in an engine-local file keyed by
/// `proposal_hash`; only its hash and bounded metadata are synced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResyncJob {
    pub status: ResyncStatus,
    /// Fingerprint of the inputs this run started from.
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_hash: Option<String>,
    #[serde(default)]
    pub target_artifact_id: String,
    #[serde(default)]
    pub base_revision: u32,
    #[serde(default)]
    pub base_hash: String,
    /// Operation id the apply publishes under; stored before the write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_revision: Option<u32>,
    /// Proposals the user declined; never raised again for the same hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declined_hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentationInventory {
    pub project_id: String,
    pub task_id: String,
    pub destination_id: String,
    pub owner_device_id: String,
    pub observed_at: i64,
    pub artifacts: Vec<InventoryArtifact>,
    pub pending: Vec<String>,
    /// False while any source could not be read or is external. Older rows
    /// read as incomplete, so they never count as aligned.
    #[serde(default)]
    pub coverage_complete: bool,
    /// Digest of the inputs (membership, pin, revision hashes).
    #[serde(default)]
    pub fingerprint: String,
    /// The fingerprint last reviewed or verified as aligned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<ResyncJob>,
}

/// Digest of the inputs a re-sync depends on: which artifacts the project
/// owns, which one is the primary plan, and each current revision's hash.
pub fn fingerprint(artifacts: &[InventoryArtifact]) -> String {
    let mut lines: Vec<String> = artifacts
        .iter()
        .map(|a| {
            format!(
                "{}|{}|{}|{}|{}",
                a.id, a.owner_item_id, a.revision, a.content_hash, a.primary_plan
            )
        })
        .collect();
    lines.sort();
    crate::artifacts::content_hash(lines.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, revision: u32, hash: &str, primary: bool) -> InventoryArtifact {
        InventoryArtifact {
            id: id.into(),
            owner_item_id: "project".into(),
            title: id.into(),
            revision,
            primary_plan: primary,
            content_hash: hash.into(),
        }
    }

    #[test]
    fn fingerprint_ignores_order_and_follows_content_and_pin() {
        let a = artifact("a", 1, "h1", true);
        let b = artifact("b", 2, "h2", false);
        assert_eq!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[b.clone(), a.clone()])
        );
        assert_ne!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[artifact("a", 1, "h1x", true), b.clone()])
        );
        assert_ne!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[artifact("a", 1, "h1", false), b])
        );
    }

    #[test]
    fn rows_without_resync_fields_still_deserialize_and_read_incomplete() {
        let old = r#"{"projectId":"p","taskId":"t","destinationId":"p","ownerDeviceId":"d","observedAt":5,
            "artifacts":[{"id":"a","ownerItemId":"p","title":"A","revision":2,"primaryPlan":true}],"pending":[]}"#;
        let row: DocumentationInventory = serde_json::from_str(old).unwrap();
        assert!(!row.coverage_complete);
        assert!(row.reviewed_fingerprint.is_none());
        assert!(row.job.is_none());
        assert_eq!(row.artifacts[0].content_hash, "");
    }

    #[test]
    fn job_round_trips() {
        let job = ResyncJob {
            status: ResyncStatus::ReadyForReview,
            proposal_hash: Some("p".into()),
            declined_hashes: vec!["x".into()],
            ..Default::default()
        };
        let text = serde_json::to_string(&job).unwrap();
        assert_eq!(serde_json::from_str::<ResyncJob>(&text).unwrap(), job);
    }
}
