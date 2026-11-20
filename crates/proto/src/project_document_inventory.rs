//! Synced observations for a project documentation inventory task.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryArtifact {
    pub id: String,
    pub owner_item_id: String,
    pub title: String,
    pub revision: u32,
    pub primary_plan: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentationInventory {
    pub project_id: String,
    pub task_id: String,
    pub destination_id: String,
    pub owner_device_id: String,
    pub observed_at: i64,
    pub artifacts: Vec<InventoryArtifact>,
    pub pending: Vec<String>,
}
