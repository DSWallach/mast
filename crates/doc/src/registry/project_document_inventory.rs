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
        value: &DocumentationInventory,
    ) -> Result<bool, DocError> {
        if value.task_id.is_empty() || value.project_id.is_empty() {
            return Err(DocError::Schema(
                "Documentation inventory needs a task and project".into(),
            ));
        }
        let row_id = mast_proto::derive_id(&value.task_id, &value.owner_device_id);
        if self
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
            fields([("payload", json!(serde_json::to_string(value)?))]),
        );
        Ok(true)
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
            pending: vec!["Pending inspection".into()],
        };
        assert!(doc.put_project_document_inventory(&value).unwrap());
        assert!(!doc.put_project_document_inventory(&value).unwrap());
        let other = DocumentationInventory {
            owner_device_id: "other-device".into(),
            ..value.clone()
        };
        assert!(doc.put_project_document_inventory(&other).unwrap());
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
            pending: vec!["Creation pending".into()],
        };
        doc.put_project_document_inventory(&value).unwrap();
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
            pending: vec![format!("{owner} gap")],
        };
        let mut a = RegistryDoc::new("device-a");
        let mut b = RegistryDoc::new("device-b");
        a.put_project_document_inventory(&claim("device-a"))
            .unwrap();
        b.put_project_document_inventory(&claim("device-b"))
            .unwrap();
        let rows = b.overlay_rows(KIND_PROJECT_DOCUMENT_INVENTORY);
        assert!(a.apply_rows(1, rows));
        let reopened = RegistryDoc::from_bytes(&a.to_bytes().unwrap(), "device-a").unwrap();
        assert_eq!(
            reopened.project_document_inventories("task"),
            vec![claim("device-a"), claim("device-b")]
        );
    }
}
