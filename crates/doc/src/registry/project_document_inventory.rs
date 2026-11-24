//! Synced, system-owned inventory observations, separate from task edits.

use super::*;
use mast_proto::project_document_inventory::DocumentationInventory;

impl RegistryDoc {
    pub fn project_document_inventories(&self, task_id: &str) -> Vec<DocumentationInventory> {
        let mut values: Vec<_> = self
            .overlay_rows(KIND_PROJECT_DOCUMENT_INVENTORY)
            .iter()
            .filter_map(|row| {
                let payload = row.fields.get("payload")?.as_str()?;
                let value: DocumentationInventory = serde_json::from_str(payload).ok()?;
                (value.task_id == task_id).then_some(value)
            })
            .collect();
        values.sort_by(|a, b| a.owner_device_id.cmp(&b.owner_device_id));
        values
    }

    pub fn put_project_document_inventory(
        &mut self,
        space_id: &str,
        value: &DocumentationInventory,
    ) -> Result<bool, DocError> {
        if value.task_id.is_empty() || value.project_id.is_empty() {
            return Err(DocError::Schema(
                "Documentation inventory needs a task and project".into(),
            ));
        }
        let row_id = mast_proto::derive_id(&value.task_id, &value.owner_device_id);
        if self.overlay_row(KIND_PROJECT_DOCUMENT_INVENTORY, &row_id).and_then(|row| row.fields.get("spaceId").and_then(Value::as_str).map(str::to_owned)).as_deref() == Some(space_id) && self
            .project_document_inventories(&value.task_id)
            .iter()
            .any(|old| old == value)
        {
            return Ok(false);
        }
        self.write(
            KIND_PROJECT_DOCUMENT_INVENTORY,
            &row_id,
            OpKind::Upsert,
            fields([("spaceId", json!(space_id)), ("payload", json!(serde_json::to_string(value)?))]),
        );
        Ok(true)
    }

    pub(super) fn project_document_inventory_rows_of_space(&self, space_id: &str, project_ids: &[&str]) -> Vec<String> {
        self.overlay_rows(KIND_PROJECT_DOCUMENT_INVENTORY).into_iter().filter(|row| {
            if row.fields.get("spaceId").and_then(Value::as_str) == Some(space_id) { return true; }
            // Older inventory rows carried only the project inside the payload.
            if row.fields.get("spaceId").and_then(Value::as_str).is_some() { return false; }
            row.fields.get("payload").and_then(Value::as_str)
                .and_then(|payload| serde_json::from_str::<DocumentationInventory>(payload).ok())
                .is_some_and(|value| {
                    let op = format!("project-doc-inventory-{}", mast_proto::derive_id(space_id, &value.project_id));
                    project_ids.contains(&value.project_id.as_str())
                        || value.task_id == mast_proto::derive_id(&op, mast_proto::roles::ITEM)
                })
        }).map(|row| row.id).collect()
    }

    pub fn project_document_inventory_orphans(&self) -> Vec<String> {
        // Legacy rows omitted spaceId. After authoritative sync, their task ID
        // must match a live Space, even if a deleted Space's tombstone expired.
        let spaces: Vec<String> = self.overlay_rows(KIND_SPACES).into_iter().map(|row| row.id).collect();
        self.overlay_rows(KIND_PROJECT_DOCUMENT_INVENTORY).into_iter().filter(|row| {
            if let Some(space) = row.fields.get("spaceId").and_then(Value::as_str) {
                return !self.row_exists(KIND_SPACES, space);
            }
            row.fields.get("payload").and_then(Value::as_str)
                .and_then(|payload| serde_json::from_str::<DocumentationInventory>(payload).ok())
                .is_some_and(|value| !spaces.iter().any(|space| {
                    let op = format!("project-doc-inventory-{}", mast_proto::derive_id(space, &value.project_id));
                    value.task_id == mast_proto::derive_id(&op, mast_proto::roles::ITEM)
                }))
        }).map(|row| row.id).collect()
    }

    pub fn sweep_project_document_inventory_orphans(&mut self) -> usize {
        let ids = self.project_document_inventory_orphans();
        let keys: Vec<_> = ids.iter().map(|id| (KIND_PROJECT_DOCUMENT_INVENTORY, id.as_str())).collect();
        self.delete_row_ops(&keys);
        ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_survives_registry_reload_and_is_idempotent() {
        let mut doc = RegistryDoc::new("device");
        let value = DocumentationInventory {
            project_id: "project".into(),
            task_id: "task".into(),
            destination_id: "destination".into(),
            owner_device_id: "device".into(),
            observed_at: 1,
            artifacts: vec![],
            pending: vec!["Pending inspection".into()], ..Default::default()
        };
        assert!(doc.put_project_document_inventory("space", &value).unwrap());
        assert!(!doc.put_project_document_inventory("space", &value).unwrap());
        let other = DocumentationInventory {
            owner_device_id: "other-device".into(),
            ..value.clone()
        };
        assert!(doc.put_project_document_inventory("space", &other).unwrap());
        let bytes = doc.to_bytes().unwrap();
        let reopened = RegistryDoc::from_bytes(&bytes, "device").unwrap();
        assert_eq!(
            reopened.project_document_inventories("task"),
            vec![value, other]
        );
    }

    #[test]
    fn pending_reservation_survives_reload() {
        let mut doc = RegistryDoc::new("device");
        let value = DocumentationInventory {
            project_id: "project".into(),
            task_id: "task".into(),
            destination_id: "destination".into(),
            owner_device_id: "device".into(),
            observed_at: 0,
            artifacts: vec![],
            pending: vec!["Creation pending".into()], ..Default::default()
        };
        doc.put_project_document_inventory("space", &value).unwrap();
        let reopened = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "device").unwrap();
        assert_eq!(reopened.project_document_inventories("task"), vec![value]);
    }

    #[test]
    fn divergent_owner_claims_survive_merge_and_reload() {
        let claim = |owner: &str| DocumentationInventory {
            project_id: "project".into(),
            task_id: "task".into(),
            destination_id: "destination".into(),
            owner_device_id: owner.into(),
            observed_at: 5,
            artifacts: vec![],
            pending: vec![format!("{owner} gap")], ..Default::default()
        };
        let mut a = RegistryDoc::new("device-a");
        let mut b = RegistryDoc::new("device-b");
        a.put_project_document_inventory("space", &claim("device-a"))
            .unwrap();
        b.put_project_document_inventory("space", &claim("device-b"))
            .unwrap();
        let rows = b.overlay_rows(KIND_PROJECT_DOCUMENT_INVENTORY);
        assert!(a.apply_rows(1, rows));
        let reopened = RegistryDoc::from_bytes(&a.to_bytes().unwrap(), "device-a").unwrap();
        assert_eq!(
            reopened.project_document_inventories("task"),
            vec![claim("device-a"), claim("device-b")]
        );
    }

    #[test]
    fn space_deletion_removes_legacy_and_current_inventory_rows() {
        let mut doc = RegistryDoc::new("device");
        let project = "project";
        let op = format!("project-doc-inventory-{}", mast_proto::derive_id("space", project));
        let value = DocumentationInventory {
            project_id: project.into(),
            task_id: mast_proto::derive_id(&op, mast_proto::roles::ITEM),
            destination_id: project.into(),
            owner_device_id: "legacy".into(),
            observed_at: 1,
            artifacts: vec![],
            pending: vec!["private gap".into()], ..Default::default()
        };
        let row_id = mast_proto::derive_id(&value.task_id, &value.owner_device_id);
        doc.write(KIND_PROJECT_DOCUMENT_INVENTORY, &row_id, OpKind::Upsert,
            fields([("payload", json!(serde_json::to_string(&value).unwrap()))]));
        let current = DocumentationInventory { owner_device_id: "current".into(), ..value.clone() };
        doc.put_project_document_inventory("space", &current).unwrap();
        let other = DocumentationInventory { project_id: "other".into(), task_id: "other-task".into(), ..value.clone() };
        doc.write(KIND_SPACES, "other-space", OpKind::Upsert, fields([]));
        doc.put_project_document_inventory("other-space", &other).unwrap();
        doc.delete_space("space").unwrap();
        // An old peer may send a legacy observation after the cascade.
        doc.write(KIND_PROJECT_DOCUMENT_INVENTORY, &row_id, OpKind::Upsert,
            fields([("payload", json!(serde_json::to_string(&value).unwrap()))]));
        let forgotten = DocumentationInventory {
            project_id: "forgotten-project".into(),
            task_id: mast_proto::derive_id(&format!("project-doc-inventory-{}", mast_proto::derive_id("forgotten-space", "forgotten-project")), mast_proto::roles::ITEM),
            ..value.clone()
        };
        let forgotten_id = mast_proto::derive_id(&forgotten.task_id, &forgotten.owner_device_id);
        doc.write(KIND_PROJECT_DOCUMENT_INVENTORY, &forgotten_id, OpKind::Upsert,
            fields([("payload", json!(serde_json::to_string(&forgotten).unwrap()))]));
        let live_legacy = DocumentationInventory {
            project_id: "live-project".into(),
            task_id: mast_proto::derive_id(&format!("project-doc-inventory-{}", mast_proto::derive_id("other-space", "live-project")), mast_proto::roles::ITEM),
            ..value.clone()
        };
        let live_id = mast_proto::derive_id(&live_legacy.task_id, &live_legacy.owner_device_id);
        doc.write(KIND_PROJECT_DOCUMENT_INVENTORY, &live_id, OpKind::Upsert,
            fields([("payload", json!(serde_json::to_string(&live_legacy).unwrap()))]));
        assert_eq!(doc.sweep_project_document_inventory_orphans(), 2);
        let reopened = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "device").unwrap();
        assert!(reopened.project_document_inventories(&value.task_id).is_empty());
        assert_eq!(reopened.project_document_inventories("other-task"), vec![other]);
        assert_eq!(reopened.project_document_inventories(&live_legacy.task_id), vec![live_legacy]);
    }
}
