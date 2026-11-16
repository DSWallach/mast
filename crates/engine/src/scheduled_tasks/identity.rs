//! Run identity backfill: imported runs join the native task imported from
//! the same Claude task, and runs whose task is gone are retired. Only the
//! host device rewrites its own chats.
use mast_proto::{Chat, ScheduledChat, ScheduledTask};

/// The registry task name a native task was imported from (`…json#name`).
fn source_name(task: &ScheduledTask) -> Option<&str> {
    task.import_source
        .as_deref()?
        .rsplit_once('#')
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
}

/// The `scheduledTaskInfo` rewrites that bring `device`'s runs in line with
/// its task ledger. Empty once applied, so repeating it writes nothing.
pub(crate) fn plan(
    chats: &[Chat],
    tasks: &[ScheduledTask],
    device: &str,
) -> Vec<(String, ScheduledChat)> {
    let tasks: Vec<&ScheduledTask> = tasks.iter().filter(|t| t.device_id == device).collect();
    let mut updates = Vec::new();
    for chat in chats.iter().filter(|c| c.device_id == device) {
        let Some(info) = &chat.scheduled_task_info else {
            continue;
        };
        let mut next = info.clone();
        match tasks.iter().find(|t| t.id == info.task_id) {
            Some(task) => next.retired = task.deleted,
            None => {
                // Never by display name alone: the name must be the import
                // source's task name, and only one live task may claim it.
                let mut claims = tasks.iter().filter(|t| {
                    !t.deleted && source_name(t) == Some(info.task_name.as_str())
                });
                match (claims.next(), claims.next()) {
                    (Some(task), None) => {
                        next.legacy_task_id = info
                            .legacy_task_id
                            .clone()
                            .or_else(|| Some(info.task_id.clone()));
                        next.task_id = task.id.clone();
                        next.retired = false;
                    }
                    (None, _) => next.retired = true,
                    (Some(_), Some(_)) => next.retired = false,
                }
            }
        }
        if &next != info {
            updates.push((chat.id.clone(), next));
        }
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ChatConfig, HarnessId, SandboxLevel, ScheduledTaskDraft, TaskSchedule};

    fn task(id: &str, device: &str, source: Option<&str>, deleted: bool) -> ScheduledTask {
        ScheduledTask {
            id: id.into(),
            revision: 1,
            device_id: device.into(),
            draft: ScheduledTaskDraft {
                name: "Display".into(),
                prompt: "p".into(),
                cwd: "/tmp".into(),
                space_id: None,
                config: ChatConfig {
                    approval_rules: true,
                    harness: HarnessId::ClaudeCode,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                },
                schedule: TaskSchedule::Once { at: 0 },
                enabled: false,
                grace_minutes: 0,
                cutover_confirmed: false,
                work_item_id: None,
            },
            created_at: 0,
            updated_at: 0,
            next_due: None,
            deleted,
            import_source: source.map(str::to_owned),
            source_handoff_at: None,
        }
    }

    fn run(id: &str, device: &str, task_id: &str, name: &str) -> Chat {
        let mut chat: Chat = serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": device, "archived": false,
            "createdAt": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        chat.scheduled_task_info = Some(ScheduledChat {
            task_id: task_id.into(),
            task_name: name.into(),
            run_id: id.into(),
            ..Default::default()
        });
        chat
    }

    fn apply(chats: &mut [Chat], updates: Vec<(String, ScheduledChat)>) {
        for (id, info) in updates {
            chats.iter_mut().find(|c| c.id == id).unwrap().scheduled_task_info = Some(info);
        }
    }

    #[test]
    fn legacy_runs_join_their_native_task_once() {
        let tasks = vec![task("native", "d", Some("/a/scheduled-tasks.json#stall-watch"), false)];
        let mut chats = vec![
            run("legacy", "d", "fafa", "stall-watch"),
            run("native-run", "d", "native", "Codespace Stall Watch"),
            run("plain-name", "d", "zzz", "Codespace Stall Watch"),
        ];
        let updates = plan(&chats, &tasks, "d");
        apply(&mut chats, updates);
        let legacy = chats[0].scheduled_task_info.as_ref().unwrap();
        assert_eq!(legacy.task_id, "native");
        assert_eq!(legacy.legacy_task_id.as_deref(), Some("fafa"));
        assert!(!legacy.retired);
        assert!(chats[1].scheduled_task_info.as_ref().unwrap().legacy_task_id.is_none());
        // A display name that is not the import source's task name never merges.
        let display = chats[2].scheduled_task_info.as_ref().unwrap();
        assert_eq!(display.task_id, "zzz");
        assert!(display.retired);
        assert!(plan(&chats, &tasks, "d").is_empty(), "second pass writes nothing");
    }

    #[test]
    fn same_name_from_two_sources_is_left_alone() {
        let tasks = vec![
            task("one", "d", Some("/account-a/scheduled-tasks.json#slack-briefing"), false),
            task("two", "d", Some("/account-b/scheduled-tasks.json#slack-briefing"), false),
        ];
        let chats = vec![run("legacy", "d", "old", "slack-briefing")];
        assert!(plan(&chats, &tasks, "d").is_empty());
    }

    #[test]
    fn other_devices_are_never_rewritten_or_matched() {
        let tasks = vec![task("native", "a", Some("/r.json#briefing"), false)];
        let chats = vec![run("peer", "b", "old", "briefing")];
        assert!(plan(&chats, &tasks, "a").is_empty());
        let updates = plan(&chats, &tasks, "b");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].1.task_id, "old", "no task on b claims it");
        assert!(updates[0].1.retired);
    }

    #[test]
    fn retired_and_deleted_tasks_hide_until_a_live_task_returns() {
        let mut tasks = vec![
            task("deleted", "d", Some("/r.json#manager"), true),
            task("kept", "d", None, false),
        ];
        let mut chats = vec![
            run("of-deleted", "d", "deleted", "Manager"),
            run("retired-routine", "d", "legacy", "daily-priority-briefing"),
            run("of-kept", "d", "kept", "Kept"),
        ];
        let updates = plan(&chats, &tasks, "d");
        apply(&mut chats, updates);
        let retired = |c: &Chat| c.scheduled_task_info.as_ref().unwrap().retired;
        assert!(retired(&chats[0]) && retired(&chats[1]) && !retired(&chats[2]));
        assert!(plan(&chats, &tasks, "d").is_empty());
        // Re-importing the task revives the same identity.
        tasks[0].deleted = false;
        let updates = plan(&chats, &tasks, "d");
        apply(&mut chats, updates);
        assert!(!retired(&chats[0]));
    }
}
