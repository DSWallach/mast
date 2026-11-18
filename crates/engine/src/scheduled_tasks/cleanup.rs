//! Reversible cleanup of superseded editions and Desktop imports.
use super::{ScheduledTasks, identity, import};
use crate::EngineError;
use mast_proto::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Use the scheduler's stable identities, including legacy imports whose
/// registry row has yet to be reconciled.
pub(crate) struct DesktopRuns {
    tasks: Vec<ScheduledTask>,
    task_files: HashMap<String, String>,
}

impl DesktopRuns {
    pub(super) fn new(tasks: Vec<ScheduledTask>) -> Self {
        let task_files = tasks
            .iter()
            .filter_map(|task| {
                let file = import::registry_file(task.import_source.as_deref()?)?;
                Some((task.id.clone(), file))
            })
            .collect();
        Self { tasks, task_files }
    }

    pub(crate) fn should_archive(&self, device: &str, info: &ScheduledChat) -> bool {
        if let Some(task) = self
            .tasks
            .iter()
            .find(|t| t.device_id == device && t.id == info.task_id)
        {
            return task.deleted || task.source_handoff_at.is_some();
        }
        let Some(file) = &info.source_file else {
            return info.retired;
        };
        // Older provisional ids may not have been rewritten yet, and a
        // deleted task is no longer a candidate for identity reconciliation.
        let mut claims = self.tasks.iter().filter(|task| {
            task.device_id == device
                && identity::source_name(task) == Some(info.task_name.as_str())
                && self.task_files.get(&task.id) == Some(file)
        });
        match (claims.next(), claims.next()) {
            (Some(task), None) => task.deleted || task.source_handoff_at.is_some(),
            // An unclaimed Desktop run is retired when its definition is gone.
            (None, _) => import::desktop_source_retired(file, &info.task_name),
            (Some(_), Some(_)) => false,
        }
    }

    fn project_identities(
        &self,
        chats: &mut [Chat],
        device: &str,
        source_missing: impl Fn(&str) -> bool,
    ) {
        let updates: HashMap<_, _> =
            identity::plan(chats, &self.tasks, &self.task_files, device, source_missing)
                .into_iter()
                .collect();
        for chat in chats {
            if let Some(info) = updates.get(&chat.id) {
                chat.scheduled_task_info = Some(info.clone());
            }
        }
    }
}

impl ScheduledTasks {
    pub(crate) fn desktop_runs(&self) -> Result<DesktopRuns, EngineError> {
        Ok(DesktopRuns::new(self.0.store.tasks()?))
    }

    pub(super) async fn clean_up_runs(
        &self,
        mode: ScheduledRunCleanupMode,
    ) -> Result<ScheduledRunCleanup, EngineError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = this.0.gate.blocking_lock();
            this.clean_up_runs_blocking(mode)
        })
        .await
        .map_err(super::fail)?
    }

    fn should_archive_desktop_run(
        &self,
        desktop: &DesktopRuns,
        chat: &Chat,
        info: &ScheduledChat,
    ) -> bool {
        let imported = info.source_file.is_some()
            || info.legacy_task_id.is_some()
            || crate::external_import::import_cursor_exists(
                std::path::Path::new(self.0.store.scope()),
                &chat.id,
            );
        imported && desktop.should_archive(&chat.device_id, info)
    }

    fn clean_up_runs_blocking(
        &self,
        mode: ScheduledRunCleanupMode,
    ) -> Result<ScheduledRunCleanup, EngineError> {
        let workspace = &self.0.workspace;
        if !workspace.registry_synced() {
            return Err(super::fail(
                "Wait for sessions and pins to finish syncing, then try again",
            ));
        }
        let marker = workspace.scheduled_run_cleanup_marker();
        let desktop = self.desktop_runs()?;
        let mut chats = workspace.read_chats()?;
        desktop.project_identities(&mut chats, workspace.device_id(), |id| {
            self.0.missing_sources.join(id).is_file()
        });
        let latest = latest_task_chat_ids_including_retired(&chats);
        let pins = workspace.sidebar_preferences_snapshot().pinned_session_ids;
        let assistant = self.0.sessions.agent_tokens().personal_assistant().get();
        let covers = assistant
            .as_ref()
            .map(|assistant| assistant.coverage(&chats));
        let active: HashSet<_> = self
            .0
            .store
            .active()?
            .into_iter()
            .filter_map(|run| run.chat_id)
            .collect();
        let actions = workspace.open_user_action_origin_chats();
        let mut groups = BTreeMap::<_, Vec<&Chat>>::new();
        for chat in &chats {
            let Some(info) = &chat.scheduled_task_info else {
                continue;
            };
            if chat.device_id != workspace.device_id()
                || chat.archived
                || pins.contains(&chat.id)
                || covers.as_ref().is_some_and(|covers| covers(chat))
                || active.contains(&chat.id)
                || actions.contains(&chat.id)
                || (latest.contains(chat.id.as_str())
                    && !self.should_archive_desktop_run(&desktop, chat, info))
                || self.0.sessions.turn_in_flight(&chat.id)
                || self.0.host.has_queued_turn(&chat.id)?
            {
                continue;
            }
            groups
                .entry((&chat.space_id, &info.task_id))
                .or_default()
                .push(chat);
        }

        let mut result = ScheduledRunCleanup::default();
        for ((space_id, task_id), mut runs) in groups {
            runs.sort_by_key(|chat| task_chat_order(chat));
            let task_name = desktop
                .tasks
                .iter()
                .find(|task| task.device_id == workspace.device_id() && &task.id == task_id)
                .map_or_else(
                    || {
                        runs.last()
                            .unwrap()
                            .scheduled_task_info
                            .as_ref()
                            .unwrap()
                            .task_name
                            .clone()
                    },
                    |task| task.draft.name.clone(),
                );
            result.tasks.push(ScheduledRunCleanupTask {
                task_id: task_id.clone(),
                task_name,
                space_id: space_id.clone(),
                count: runs.len(),
                chat_ids: runs.into_iter().map(|chat| chat.id.clone()).collect(),
            });
        }
        result.tasks.sort_by(|a, b| a.task_name.cmp(&b.task_name));

        if let ScheduledRunCleanupMode::Apply { expected_chat_ids } = mode {
            let expected: HashSet<_> = expected_chat_ids.iter().collect();
            let ids: HashSet<_> = result.chat_ids().collect();
            if expected == ids {
                if ids.is_empty() {
                    result.applied = true;
                } else if workspace
                    .archive_scheduled_runs(marker, result.chat_ids().map(String::as_str))?
                {
                    result.applied = true;
                } else {
                    return self.clean_up_runs_blocking(ScheduledRunCleanupMode::Preview);
                }
            }
        }
        Ok(result)
    }
}
