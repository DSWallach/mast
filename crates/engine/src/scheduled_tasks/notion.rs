//! The Write to Notion setting's share of a scheduled run's prompt contract.
//! The same value reaches the agent as `MAST_SCHEDULED_WRITE_TO_NOTION`.

use mast_proto::ScheduledTaskDraft;

/// The prompt a run starts with: the task's instructions, then the Notion
/// contract. A task saved before the setting existed adds nothing.
pub(crate) fn run_prompt(draft: &ScheduledTaskDraft) -> String {
    let contract = match draft.write_to_notion {
        None => return draft.prompt.clone(),
        Some(true) => {
            "Write to Notion is on for this scheduled task. Steps that write to Notion may run (`MAST_SCHEDULED_WRITE_TO_NOTION=1`)."
        }
        Some(false) => {
            "Write to Notion is off for this scheduled task (`MAST_SCHEDULED_WRITE_TO_NOTION=0`). Skip every step that creates, updates, moves, comments on or deletes Notion content, and say which steps you skipped. Reading Notion is still allowed."
        }
    };
    format!("{}\n\n---\n{contract}", draft.prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(write_to_notion: Option<bool>) -> ScheduledTaskDraft {
        let mut draft: ScheduledTaskDraft = serde_json::from_value(serde_json::json!({
            "name": "Daily", "prompt": "Do the thing", "cwd": "/tmp", "spaceId": null,
            "config": {"harness": "claude-code", "sandbox": "workspace-write"},
            "schedule": {"type": "cron", "expression": "0 8 * * *", "timezone": "UTC"},
            "graceMinutes": 0
        }))
        .unwrap();
        draft.write_to_notion = write_to_notion;
        draft
    }

    #[test]
    fn missing_field_keeps_the_prompt_and_wire_shape() {
        let legacy = draft(None);
        assert_eq!(run_prompt(&legacy), "Do the thing");
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("writeToNotion")
                .is_none()
        );
    }

    #[test]
    fn off_and_on_state_the_setting() {
        let off = run_prompt(&draft(Some(false)));
        assert!(
            off.starts_with("Do the thing\n\n---\n")
                && off.contains("is off")
                && off.contains("=0")
        );
        let on = run_prompt(&draft(Some(true)));
        assert!(on.contains("is on") && on.contains("=1"));
        let json = serde_json::to_value(draft(Some(false))).unwrap();
        assert_eq!(json["writeToNotion"], false);
    }
}
