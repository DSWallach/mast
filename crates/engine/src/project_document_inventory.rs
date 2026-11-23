//! Read-only inventory for a project's documentation re-sync.
//!
//! An inventory is evidence for a later reconciliation, not a statement that
//! the documents agree. In particular, an incomplete registry or an unknown
//! source must remain visible to the caller as pending.
//!
//! The initial scan assumes a stable Space owner. Registry replicas do not
//! provide a cross-device ownership fence during concurrent transfer. Claims
//! from different devices are kept separately and surfaced for review; this
//! milestone does not provide global exactly-once creation or rescan.

use mast_proto::project_document_inventory::{DocumentationInventory, InventoryArtifact};
use mast_proto::{ArtifactView, WorkOutlineSnapshot};
use std::collections::HashSet;
use std::path::Path;

/// The destination must be live and remain within this project, without
/// crossing into a nested project.
pub fn valid_destination(snapshot: &WorkOutlineSnapshot, project_id: &str, destination_id: &str) -> bool {
    let mut current = Some(destination_id);
    while let Some(id) = current {
        let Some(item) = snapshot.items.iter().find(|item| item.id == id && item.archived_at.is_none()) else { return false; };
        if id == project_id { return item.is_project; }
        if item.is_project { return false; }
        current = item.parent_work_item_id.as_deref();
    }
    false
}

/// Enumerate only this project's items. A nested project owns its own
/// documents and is deliberately excluded, including its descendants.
pub fn inventory(
    snapshot: &WorkOutlineSnapshot,
    project_id: &str,
    artifacts: &[ArtifactView],
    task_id: &str,
    destination_id: &str,
    owner_device_id: &str,
) -> Result<DocumentationInventory, String> {
    let project = snapshot
        .items
        .iter()
        .find(|item| item.id == project_id)
        .filter(|item| item.is_project && item.archived_at.is_none())
        .ok_or_else(|| "Project not found".to_owned())?;
    let mut owned = HashSet::from([project.id.clone()]);
    // Snapshot order is depth-first with parents first.
    for item in &snapshot.items {
        if item.archived_at.is_none()
            && !item.is_project
            && item
                .parent_work_item_id
                .as_ref()
                .is_some_and(|id| owned.contains(id))
        {
            owned.insert(item.id.clone());
        }
    }
    let mut pending = Vec::new();
    if !snapshot.available || !snapshot.synced {
        pending.push("Project outline has not fully synced".to_owned());
    }
    let mut found: Vec<_> = artifacts
        .iter()
        .filter_map(|artifact| {
            let owner = artifact.work_item_id.as_ref()?;
            if artifact.archived_at.is_some() || !owned.contains(owner) {
                return None;
            }
            Some(InventoryArtifact {
                id: artifact.id.clone(),
                owner_item_id: owner.clone(),
                title: artifact.title.clone(),
                revision: artifact.current_revision,
                primary_plan: owner == project_id && artifact.pinned_as_plan,
            })
        })
        .collect();
    for artifact in artifacts.iter().filter(|artifact| {
        artifact.archived_at.is_none()
            && artifact
                .work_item_id
                .as_ref()
                .is_some_and(|id| owned.contains(id))
    }) {
        if artifact.stored_path.is_empty() || !Path::new(&artifact.stored_path).is_file() {
            pending.push(format!(
                "Artifact requires inspection: {} ({})",
                artifact.title, artifact.id
            ));
        }
    }
    found.sort_by(|a, b| {
        b.primary_plan
            .cmp(&a.primary_plan)
            .then_with(|| a.id.cmp(&b.id))
    });
    match found.iter().filter(|a| a.primary_plan).count() {
        1 => {}
        0 => pending.push("The project has no pinned primary plan".to_owned()),
        _ => pending.push("The project has more than one pinned primary plan".to_owned()),
    }
    for reference in &snapshot.external_references {
        if owned.contains(&reference.work_item_id) {
            pending.push(format!(
                "External source requires inspection: {}",
                reference.url
            ));
        }
    }
    pending.push(
        "Document contents, instructions and decision history have not been reconciled".to_owned(),
    );
    Ok(DocumentationInventory {
        project_id: project_id.to_owned(),
        task_id: task_id.to_owned(),
        destination_id: destination_id.to_owned(),
        owner_device_id: owner_device_id.to_owned(),
        observed_at: chrono::Utc::now().timestamp_millis(),
        artifacts: found,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inventory_stops_at_nested_projects_and_reports_gaps() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(json!({
            "spaceId": "space", "available": true, "synced": true,
            "items": [
                {"id": "project", "spaceId": "space", "isProject": true},
                {"id": "child", "spaceId": "space", "parentWorkItemId": "project"},
                {"id": "other", "spaceId": "space", "parentWorkItemId": "project", "isProject": true},
                {"id": "other-child", "spaceId": "space", "parentWorkItemId": "other"}
            ],
            "externalReferences": [{"id": "ref", "workItemId": "child", "url": "https://example.test/doc"}]
        })).unwrap();
        let artifact = |id: &str, owner: &str, pinned| ArtifactView {
            id: id.into(),
            work_item_id: Some(owner.into()),
            pinned_as_plan: pinned,
            ..Default::default()
        };
        let result = inventory(
            &snapshot,
            "project",
            &[
                artifact("nested", "other-child", true),
                artifact("child", "child", false),
                artifact("plan", "project", true),
            ],
            "task",
            "project",
            "device",
        )
        .unwrap();
        assert_eq!(
            result
                .artifacts
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["plan", "child"]
        );
        assert!(
            result
                .pending
                .iter()
                .any(|p| p.contains("https://example.test/doc"))
        );
        assert!(
            result
                .pending
                .iter()
                .any(|p| p.contains("not been reconciled"))
        );
        assert!(!result.pending.iter().any(|p| p.contains("no pinned")));
    }

    #[test]
    fn missing_pin_and_unsynced_outline_are_pending() {
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(json!({
            "spaceId": "space", "available": true, "synced": false,
            "items": [{"id": "project", "spaceId": "space", "isProject": true}]
        }))
        .unwrap();
        let result = inventory(&snapshot, "project", &[], "task", "project", "device").unwrap();
        assert!(
            result
                .pending
                .iter()
                .any(|p| p.contains("not fully synced"))
        );
        assert!(result.pending.iter().any(|p| p.contains("no pinned")));
    }
}
