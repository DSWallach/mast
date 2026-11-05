//! Durable observations and notification markers for merged PR wakes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::orchestrator_wakes::OrchestratorWakes;
use crate::pr_ticket_cache::PrStatus;
use crate::workspace_host::WorkspaceHost;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Observations {
    states: HashMap<String, String>,
    notified: HashSet<(String, String)>,
}

impl Observations {
    fn should_notify(
        &self,
        url: &str,
        state: &str,
        chat: &str,
        owned: bool,
        enabled: bool,
    ) -> bool {
        owned
            && enabled
            && state.eq_ignore_ascii_case("merged")
            && self
                .states
                .get(url)
                .is_some_and(|previous| previous.eq_ignore_ascii_case("open"))
            && !self.notified.contains(&(chat.to_owned(), url.to_owned()))
    }
}

#[derive(Clone)]
pub(crate) struct PrMergedWakes {
    observations: Arc<Mutex<Observations>>,
    path: PathBuf,
    workspace: WorkspaceHost,
    wakes: OrchestratorWakes,
}

fn save(path: &Path, observations: &Observations) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(observations).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

impl PrMergedWakes {
    pub(crate) fn new(path: PathBuf, workspace: WorkspaceHost, wakes: OrchestratorWakes) -> Self {
        let observations = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            observations: Arc::new(Mutex::new(observations)),
            path,
            workspace,
            wakes,
        }
    }

    /// Observe a successful URL refresh; failures never replace the last known state.
    pub(crate) async fn observe(&self, url: &str, status: &PrStatus) {
        let url = crate::chat_links::canonicalize_pr_url(url);
        let previous = self
            .observations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .states
            .get(&url)
            .cloned();
        if previous
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case("open"))
            && status.state.eq_ignore_ascii_case("merged")
        {
            for chat in self.workspace.read_chats().unwrap_or_default() {
                let owned = self
                    .workspace
                    .chat_pr_links(&chat.id)
                    .unwrap_or_default()
                    .iter()
                    .any(|link| {
                        crate::archivable::owns_pr_link(link)
                            && crate::chat_links::canonicalize_pr_url(&link.url) == url
                    });
                let enabled = self.wakes.accepts_pr_merged(&chat.id);
                let key = (chat.id.clone(), url.clone());
                {
                    let mut observations =
                        self.observations.lock().unwrap_or_else(|e| e.into_inner());
                    if !observations.should_notify(&url, &status.state, &chat.id, owned, enabled) {
                        continue;
                    }
                    let mut updated = observations.clone();
                    updated.notified.insert(key);
                    if let Err(error) = save(&self.path, &updated) {
                        tracing::warn!(%error, "failed to save PR merged wake marker");
                        continue;
                    }
                    *observations = updated;
                }
                let branch = status.branch.as_deref().unwrap_or("unknown branch");
                let repo = url
                    .strip_prefix("https://github.com/")
                    .and_then(|path| path.split_once("/pull/").map(|(repo, _)| repo))
                    .unwrap_or("unknown repository");
                let mut detail = format!(
                    "{repo} #{} {} ({branch}) {url}",
                    status.number,
                    status.title.as_deref().unwrap_or("untitled")
                );
                if let Some(space) = chat.space_id.as_deref() {
                    let snapshot = mast_doc::materialize_work_outline(
                        space,
                        &self.workspace.work_outline_inputs(space),
                    );
                    for request in crate::archivable::open_handoffs_for_branch(&snapshot, branch) {
                        detail
                            .push_str(&format!("; handoff request {} can be resolved", request.id));
                    }
                }
                self.wakes.note_pr_merged(&chat.id, &url, &detail).await;
            }
        }
        let mut observations = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        let mut updated = observations.clone();
        updated.states.insert(url, status.state.clone());
        if let Err(error) = save(&self.path, &updated) {
            tracing::warn!(%error, "failed to save PR state observation");
        } else {
            *observations = updated;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ChatLinkSource, ChatPrLink};

    #[test]
    fn merged_wake_requires_owned_link_and_enabled_setting_and_survives_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pr-merged-wakes.json");
        let url = "https://github.com/a/b/pull/42";
        let mut observations = Observations::default();
        observations.states.insert(url.into(), "OPEN".into());
        let owned = ChatPrLink {
            url: url.into(),
            source: ChatLinkSource::CreatedInChat,
        };
        let mention = ChatPrLink {
            url: url.into(),
            source: ChatLinkSource::Mentioned,
        };
        assert!(!observations.should_notify(
            url,
            "MERGED",
            "worker",
            crate::archivable::owns_pr_link(&mention),
            true
        ));
        assert!(!observations.should_notify(
            url,
            "MERGED",
            "worker",
            crate::archivable::owns_pr_link(&owned),
            false
        ));
        assert!(observations.should_notify(
            url,
            "MERGED",
            "worker",
            crate::archivable::owns_pr_link(&owned),
            true
        ));
        observations.notified.insert(("worker".into(), url.into()));
        save(&path, &observations).unwrap();
        assert!(!observations.should_notify(url, "MERGED", "worker", true, true));
        let loaded: Observations = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(!loaded.should_notify(url, "MERGED", "worker", true, true));
        assert!(loaded.should_notify(url, "MERGED", "another", true, true));
    }
}
