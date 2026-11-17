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
            loop {
                let usage = usage.clone();
                let workspace = workspace.clone();
                let result =
                    tokio::task::spawn_blocking(move || usage.reconcile_deletions(&workspace))
                        .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!("token usage deletion reconciliation failed; retrying");
                }
                tokio::select! {
                    changed=changes.changed()=>if changed.is_err(){break;},
                    _=tokio::time::sleep(std::time::Duration::from_secs(30))=>{},
                }
            }
        })
    }
}
