use super::*;

impl TokenUsage {
    /// Reconcile against the host's registry, including deletions synced from
    /// another device. Snapshot owners first, then read the live registry: a new
    /// chat created during this pass cannot be mistaken for a deleted owner.
    pub fn reconcile_deletions(&self, workspace: &crate::WorkspaceHost) -> Result<(), EngineError> {
        self.flush()?;
        let owners: Vec<String> = {
            let inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
            inner
                .db
                .prepare("SELECT DISTINCT chat FROM turns")
                .map_err(fail)?
                .query_map([], |r| r.get(0))
                .map_err(fail)?
                .collect::<Result<_, _>>()
                .map_err(fail)?
        };
        let live: std::collections::HashSet<_> =
            workspace.read_chats()?.into_iter().map(|c| c.id).collect();
        for owner in owners.into_iter().filter(|id| !live.contains(id)) {
            self.delete_chat(&owner)?;
        }
        Ok(())
    }

    pub fn follow_deletions(&self, workspace: crate::WorkspaceHost) -> tokio::task::JoinHandle<()> {
        let usage = self.clone();
        // The deduplicated chat list can stay empty across a rapid create/delete.
        // Registry revisions retain that deletion signal even when no list changed.
        let mut changes = workspace.watch_changes();
        tokio::spawn(async move {
            let mut reconciled = None;
            let mut retry_on_tick = false;
            let mut retry_tick = tokio::time::interval(std::time::Duration::from_secs(30));
            retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            retry_tick.tick().await;
            loop {
                let generation = workspace.chats_generation();
                if !retry_on_tick && reconciled != Some(generation) {
                    let worker_usage = usage.clone();
                    let worker_workspace = workspace.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        worker_usage.reconcile_deletions(&worker_workspace)
                    })
                    .await;
                    if matches!(result, Ok(Ok(()))) {
                        reconciled = Some(generation);
                    } else {
                        retry_on_tick = true;
                        tracing::warn!("token usage deletion reconciliation failed; retrying");
                    }
                    #[cfg(test)]
                    {
                        usage
                            .0
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .retention_passes += 1;
                    }
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() { break; },
                    _ = retry_tick.tick() => {
                        reconciled = None;
                        retry_on_tick = false;
                    },
                }
            }
        })
    }
}
