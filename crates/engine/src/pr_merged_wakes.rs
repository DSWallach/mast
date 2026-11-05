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

    fn reserve_notification(
        &mut self,
        path: &Path,
        url: &str,
        state: &str,
        chat: &str,
        owned: bool,
        enabled: bool,
    ) -> std::io::Result<bool> {
        if !self.should_notify(url, state, chat, owned, enabled) {
            return Ok(false);
        }
        let mut updated = self.clone();
        updated.notified.insert((chat.to_owned(), url.to_owned()));
        save(path, &updated)?;
        *self = updated;
        Ok(true)
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
            let chats = match self.workspace.read_chats() {
                Ok(chats) => chats,
                Err(error) => {
                    tracing::warn!(%error, "failed to read chats for PR merged wake");
                    return;
                }
            };
            let mut retry = false;
            for chat in chats {
                let links = match self.workspace.chat_pr_links(&chat.id) {
                    Ok(links) => links,
                    Err(error) => {
                        tracing::warn!(%error, "failed to read PR links for merged wake");
                        retry = true;
                        continue;
                    }
                };
                let owned = links.iter().any(|link| {
                    crate::archivable::owns_pr_link(link)
                        && crate::chat_links::canonicalize_pr_url(&link.url) == url
                });
                let enabled = self.wakes.accepts_pr_merged(&chat.id);
                {
                    let mut observations =
                        self.observations.lock().unwrap_or_else(|e| e.into_inner());
                    match observations.reserve_notification(
                        &self.path,
                        &url,
                        &status.state,
                        &chat.id,
                        owned,
                        enabled,
                    ) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(error) => {
                            tracing::warn!(%error, "failed to save PR merged wake marker");
                            retry = true;
                            continue;
                        }
                    }
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
            if retry {
                return;
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
        let mut emitted = Vec::new();
        for (chat, link, enabled) in [
            ("mentioned", &mention, true),
            ("disabled", &owned, false),
            ("worker", &owned, true),
            ("worker", &owned, true),
        ] {
            if observations
                .reserve_notification(
                    &path,
                    url,
                    "MERGED",
                    chat,
                    crate::archivable::owns_pr_link(link),
                    enabled,
                )
                .unwrap()
            {
                emitted.push(chat);
            }
        }
        assert_eq!(emitted, ["worker"]);
        let mut loaded: Observations =
            serde_json::from_slice(&std::fs::read(path.clone()).unwrap()).unwrap();
        assert!(
            !loaded
                .reserve_notification(&path, url, "MERGED", "worker", true, true)
                .unwrap()
        );
        assert!(
            loaded
                .reserve_notification(&path, url, "MERGED", "another", true, true)
                .unwrap()
        );
    }
    #[tokio::test]
    async fn refresh_queues_only_owned_enabled_pr_once_across_reload() {
        use crate::workspace_host::WorkspaceHostConfig;
        use crate::{DocHost, DocHostConfig, HarnessRegistry, RunJournal, SessionsEngine};
        use mast_proto::{HarnessId, WakeSettings};
        use mast_sync::DocsStore;
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(directory.path()).unwrap());
        let workspace = WorkspaceHost::open(
            store.clone(),
            WorkspaceHostConfig {
                device_id: "device".into(),
                device_name: "test".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        for (id, parent) in [
            ("orch", None),
            ("off", None),
            ("owned", Some("orch")),
            ("mention", Some("orch")),
            ("disabled", Some("off")),
        ] {
            workspace
                .create_chat_with_parent(
                    id,
                    None,
                    Some("device"),
                    None,
                    None,
                    parent.map(str::to_owned),
                )
                .unwrap();
        }
        let url = "https://github.com/acme/repo/pull/42";
        for (chat, source) in [
            ("owned", ChatLinkSource::CreatedInChat),
            ("mention", ChatLinkSource::Mentioned),
            ("disabled", ChatLinkSource::CreatedInChat),
        ] {
            workspace.add_chat_pr_link(chat, url, source).unwrap();
        }
        let sessions = SessionsEngine::new(
            "device".into(),
            Arc::new(RunJournal::open(directory.path().join("journal")).unwrap()),
            Arc::new(HarnessRegistry::new()),
        );
        let doc_host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "device".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let wakes = OrchestratorWakes::new(
            sessions,
            doc_host,
            workspace.clone(),
            None,
            Arc::new(|| 1_000_000),
        );
        wakes.ingest_chats(&workspace.read_chats().unwrap()).await;
        wakes
            .set_settings(
                "orch",
                WakeSettings {
                    prs: true,
                    ..Default::default()
                },
            )
            .await;
        let path = directory.path().join("cache/pr-merged-wakes.json");
        let tracker = PrMergedWakes::new(path.clone(), workspace.clone(), wakes.clone());
        let mut status = PrStatus {
            number: 42,
            url: Some(url.into()),
            state: "OPEN".into(),
            is_draft: false,
            review_decision: None,
            reviewers: vec![],
            checks: None,
            title: Some("Example".into()),
            branch: Some("feature/a".into()),
            head_sha: None,
            mergeable: "unknown".into(),
            has_reviewer_requested: false,
        };
        tracker.observe(url, &status).await;
        assert!(wakes.status("orch").held.is_empty());
        let tracker = PrMergedWakes::new(path.clone(), workspace.clone(), wakes.clone());
        status.state = "MERGED".into();
        tracker.observe(url, &status).await;
        let held = wakes.status("orch").held;
        assert_eq!(held.len(), 1);
        assert!(
            held[0]
                .summary
                .contains("acme/repo #42 Example (feature/a)")
        );
        assert!(wakes.status("off").held.is_empty());
        tracker.observe(url, &status).await;
        let reopened = PrMergedWakes::new(path, workspace, wakes.clone());
        reopened.observe(url, &status).await;
        assert_eq!(wakes.status("orch").held.len(), 1);
    }
}
