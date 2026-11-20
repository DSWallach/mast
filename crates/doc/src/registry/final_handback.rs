//! Durable, explicitly final worker handback intents.
use super::*;
use serde::{Deserialize, Serialize};

pub const KIND_FINAL_HANDBACKS: &str = "finalHandbacks";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalHandbackIntent {
    pub operation_id: String,
    pub space_id: String,
    pub worker_chat_id: String,
    pub parent_chat_id: String,
    pub project_id: String,
    pub assignment_generation: String,
    pub report: String,
}

impl RegistryDoc {
    pub fn has_final_handback_rows(&self) -> bool {
        !self.overlay_rows(KIND_FINAL_HANDBACKS).is_empty()
    }

    pub fn final_handback_intents(&self) -> Vec<FinalHandbackIntent> {
        self.overlay_rows(KIND_FINAL_HANDBACKS)
            .iter()
            .filter_map(|row| {
                serde_json::from_value(Value::Object(row.fields.clone().into_iter().collect())).ok()
            })
            .collect()
    }
    /// Called while the registry is locked. The worker identity is supplied by
    /// the authenticated engine grant, and the generation is compare-only.
    pub fn prepare_final_handback(
        &mut self,
        operation_id: &str,
        worker_chat_id: &str,
        expected_space: &str,
        expected_generation: &str,
        report: &str,
    ) -> Result<FinalHandbackIntent, DocError> {
        if operation_id.is_empty() || expected_generation.is_empty() || report.trim().is_empty() {
            return Err(DocError::Schema(
                "handback identity, generation and report are required".into(),
            ));
        }
        if let Some(row) = self.overlay_row(KIND_FINAL_HANDBACKS, operation_id) {
            let existing: FinalHandbackIntent =
                serde_json::from_value(Value::Object(row.fields.into_iter().collect()))?;
            if existing.worker_chat_id != worker_chat_id
                || existing.space_id != expected_space
                || existing.assignment_generation != expected_generation
                || existing.report != report
            {
                return Err(DocError::Schema(
                    "final handback operation conflicts with its original request".into(),
                ));
            }
            return Ok(existing);
        }
        let worker = self
            .chat(worker_chat_id)?
            .ok_or_else(|| DocError::Schema("worker chat not found".into()))?;
        let space_id = worker
            .space_id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| DocError::Schema("worker has no Space".into()))?;
        if space_id != expected_space {
            return Err(DocError::Schema(
                "worker no longer belongs to the granted Space".into(),
            ));
        }
        let parent_chat_id = worker
            .parent_chat_id
            .filter(|id| id != worker_chat_id)
            .ok_or_else(|| DocError::Schema("worker has no parent".into()))?;
        let parent = self
            .chat(&parent_chat_id)?
            .ok_or_else(|| DocError::Schema("parent chat not found".into()))?;
        if parent.space_id.as_deref() != Some(space_id.as_str()) || parent.archived {
            return Err(DocError::Schema(
                "parent is unavailable in this Space".into(),
            ));
        }
        let snapshot = self.work_outline_snapshot(&space_id, true);
        let assignment = snapshot
            .assignments
            .iter()
            .find(|a| a.chat_id == worker_chat_id)
            .ok_or_else(|| DocError::Schema("worker assignment is unknown".into()))?;
        if assignment.state != mast_proto::WorkItemChatAssignmentState::Associated {
            return Err(DocError::Schema(
                "worker assignment is not associated".into(),
            ));
        }
        if assignment.generation.as_deref() != Some(expected_generation) {
            return Err(DocError::Schema(
                "worker assignment generation changed or is unknown".into(),
            ));
        }
        let item = assignment
            .work_item_id
            .as_deref()
            .ok_or_else(|| DocError::Schema("worker assignment is absent".into()))?;
        let project_id = snapshot
            .project_root(item)
            .ok_or_else(|| DocError::Schema("worker has no project".into()))?
            .id
            .clone();
        let intent = FinalHandbackIntent {
            operation_id: operation_id.to_owned(),
            space_id,
            worker_chat_id: worker_chat_id.to_owned(),
            parent_chat_id,
            project_id,
            assignment_generation: expected_generation.to_owned(),
            report: report.to_owned(),
        };
        let fields = serde_json::to_value(&intent)?
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        self.observe_row(KIND_FINAL_HANDBACKS, operation_id);
        let op = RowOp {
            kind: KIND_FINAL_HANDBACKS.into(),
            id: operation_id.into(),
            op: OpKind::Upsert,
            set: Some(fields),
            hlc: self.next_hlc(),
            clocks: None,
        };
        self.write_ops(vec![op])?;
        Ok(intent)
    }
}
