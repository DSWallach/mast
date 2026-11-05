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

    fn resolve_merged_handoffs(&self, url: &str, status: &PrStatus) -> Result<(), String> {
        use mast_proto::work_outline_user_actions::UserActionKind;
        let merged_at = status.merged_at.as_deref().filter(|value| !value.is_empty());
        let repo = url.strip_prefix("https://github.com/")
            .and_then(|path| path.split_once("/pull/").map(|(repo, _)| repo))
            .ok_or_else(|| "invalid merged PR URL".to_owned())?;
        let chats = self.workspace.read_chats().map_err(|error| error.to_string())?;
        for space in self.workspace.read_spaces().map_err(|error| error.to_string())? {
            if space.device_id != self.workspace.device_id() { continue; }
            let snapshot = mast_doc::materialize_work_outline(&space.id, &self.workspace.work_outline_inputs(&space.id));
            if !snapshot.space_settings.resolve_merged_handoffs { continue; }
            let explicit = crate::archivable::open_handoffs_for_pr(&snapshot, "", url);
            let mut space_owns_pr = false;
            let mut branch_ambiguous = false;
            for chat in chats.iter().filter(|chat| chat.space_id.as_deref() == Some(space.id.as_str())) {
                let links = self.workspace.chat_pr_links(&chat.id).map_err(|error| error.to_string())?;
                if links.iter().any(|link| crate::archivable::owns_pr_link(link)
                    && crate::chat_links::canonicalize_pr_url(&link.url) == url) {
                    space_owns_pr = true;
                }
                if status.branch.as_deref().is_some_and(|branch| chat.branch.as_deref() == Some(branch))
                    && links.iter().any(|link| crate::archivable::owns_pr_link(link)
                        && crate::chat_links::canonicalize_pr_url(&link.url) != url) {
                    branch_ambiguous = true;
                }
            }
            for request in crate::archivable::open_handoffs_for_pr(&snapshot, status.branch.as_deref().unwrap_or(""), url) {
                if request.kind != Some(UserActionKind::Handoff) { continue; }
                let matches_pr = explicit.iter().any(|candidate| candidate.id == request.id);
                if !matches_pr {
                    let identifies_another_pr = request.links.iter().any(|link| match &link.target {
                        mast_proto::work_outline_user_actions::RequestLinkTarget::Pr { .. } => true,
                        mast_proto::work_outline_user_actions::RequestLinkTarget::Url { url } => crate::chat_links::canonicalize_pr_url(url).starts_with("https://github.com/") && url.contains("/pull/"),
                        _ => false,
                    })
                        || request.facts.iter().any(|fact| fact.label.eq_ignore_ascii_case("pr") || fact.label.eq_ignore_ascii_case("pull request"));
                    if identifies_another_pr { continue; }
                    if !space_owns_pr || branch_ambiguous { continue; }
                }
                let merged_at = merged_at.ok_or_else(|| "merged PR has no merge timestamp".to_owned())?;
                let actor = mast_proto::WorkOutlineActor {
                    kind: mast_proto::WorkOutlineActorKind::System,
                    device_id: self.workspace.device_id().to_owned(), chat_id: None, message_id: None,
                };
                let mutation = mast_proto::WorkOutlineMutation::ResolveUserActionRequest(
                    mast_proto::ResolveUserActionRequestParams {
                        outcome: Some("done".into()), values: None, via: Some("prMerged".into()),
                        user_action_request_id: request.id.clone(),
                        note: Some(format!("Landed: {repo}#{} merged {merged_at}", status.number)),
                        answer: None,
                        operation_id: mast_proto::work_outline::derive_id(&request.id, "pr-merged"),
                    });
                self.workspace.work_outline_mutate(&mutation, &actor, &[]).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
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
            if let Err(error) = self.resolve_merged_handoffs(&url, status) {
                tracing::warn!(%error, "failed to resolve merged PR handoffs");
                return;
            }
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
                    for request in crate::archivable::open_handoffs_for_pr(&snapshot, status.branch.as_deref().unwrap_or(""), &url) {
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
        let actor = mast_proto::WorkOutlineActor { kind: mast_proto::WorkOutlineActorKind::System, device_id: "device".into(), chat_id: None, message_id: None };
        let mutate = |mutation: mast_proto::WorkOutlineMutation| workspace.work_outline_mutate(&mutation, &actor, &[]).unwrap();
        let mut requests = Vec::new();
        let mut unlinked_request = String::new();
        for (space, enabled) in [("enabled-space", true), ("disabled-space", false)] {
            workspace.create_space(space, "device", &format!("/tmp/{space}"), None, false).unwrap();
            let item = mutate(mast_proto::WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
                space_id: space.into(), parent_work_item_id: None, placement: Default::default(), title: "Work".into(), description: None, status: None, operation_id: format!("item-{space}"),
            })).work_item_id.unwrap();
            workspace.create_chat(&format!("worker-{space}"), Some(space), Some("device"), None, None).unwrap();
            workspace.add_chat_pr_link(&format!("worker-{space}"), url, ChatLinkSource::CreatedInChat).unwrap();
            if !enabled {
                mutate(mast_proto::WorkOutlineMutation::SetResolveMergedHandoffs(mast_proto::work_outline::SetResolveMergedHandoffsParams { space_id: space.into(), enabled: false, operation_id: "setting-off".into() }));
            }
            for kind in [mast_proto::work_outline_user_actions::UserActionKind::Handoff, mast_proto::work_outline_user_actions::UserActionKind::Review] {
                let operation_id = format!("request-{space}-{kind:?}");
                let id = mutate(mast_proto::WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
                    options: vec![], links: vec![mast_proto::work_outline_user_actions::RequestLink { label: "PR".into(), target: mast_proto::work_outline_user_actions::RequestLinkTarget::Pr { url: url.into() } }], facts: vec![], kind: Some(kind), priority: None, work_item_id: item.clone(),
                    origin: mast_proto::UserActionRequestOrigin { kind: mast_proto::UserActionOriginKind::System, ..Default::default() }, text: "Action".into(), info: None, dedupe_key: Some("shared-key".into()), operation_id,
                })).user_action_request_id.unwrap();
                requests.push((space.to_owned(), kind, id));
            }
            let kind = mast_proto::work_outline_user_actions::UserActionKind::Handoff;
            let id = mutate(mast_proto::WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
                options: vec![], links: vec![], facts: vec![mast_proto::work_outline_user_actions::RequestFact { label: "Branch".into(), value: "feature/a".into() }], kind: Some(kind), priority: None, work_item_id: item.clone(),
                origin: mast_proto::UserActionRequestOrigin { kind: mast_proto::UserActionOriginKind::System, ..Default::default() }, text: "Branch handoff".into(), info: None, dedupe_key: None, operation_id: format!("branch-{space}"),
            })).user_action_request_id.unwrap();
            requests.push((space.to_owned(), kind, id));
            if enabled {
                unlinked_request = mutate(mast_proto::WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
                    options: vec![], links: vec![], facts: vec![mast_proto::work_outline_user_actions::RequestFact { label: "Branch".into(), value: "local-only".into() }], kind: Some(kind), priority: None, work_item_id: item,
                    origin: mast_proto::UserActionRequestOrigin { kind: mast_proto::UserActionOriginKind::System, ..Default::default() }, text: "Local branch".into(), info: None, dedupe_key: None, operation_id: "local-only-request".into(),
                })).user_action_request_id.unwrap();
            }
        }
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
            merged_at: None,
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
        status.merged_at = Some("2026-11-05T12:00:00Z".into());
        tracker.observe(url, &status).await;
        for (space, kind, id) in &requests {
            let snapshot = mast_doc::materialize_work_outline(space, &workspace.work_outline_inputs(space));
            let request = snapshot.user_action_request(id).unwrap();
            if space == "enabled-space" && *kind == mast_proto::work_outline_user_actions::UserActionKind::Handoff {
                let resolution = request.resolution.as_ref().unwrap();
                assert_eq!(resolution.outcome.as_deref(), Some("done"));
                assert_eq!(resolution.via.as_deref(), Some("prMerged"));
                assert_eq!(resolution.note.as_deref(), Some("Landed: acme/repo#42 merged 2026-11-05T12:00:00Z"));
            } else { assert!(request.is_open()); }
        }
        let snapshot = mast_doc::materialize_work_outline("enabled-space", &workspace.work_outline_inputs("enabled-space"));
        assert!(snapshot.user_action_request(&unlinked_request).unwrap().is_open());
        let held = wakes.status("orch").held;
        assert_eq!(held.len(), 1);
        assert!(
            held[0]
                .summary
                .contains("acme/repo #42 Example (feature/a)")
        );
        assert!(wakes.status("off").held.is_empty());
        let before = mast_doc::materialize_work_outline("enabled-space", &workspace.work_outline_inputs("enabled-space"));
        let resolved_before = before.user_action_request(&requests[0].2).unwrap().resolution.clone();
        tracker.observe(url, &status).await;
        let reopened = PrMergedWakes::new(path, workspace, wakes.clone());
        reopened.observe(url, &status).await;
        let after = mast_doc::materialize_work_outline("enabled-space", &reopened.workspace.work_outline_inputs("enabled-space"));
        assert_eq!(after.user_action_request(&requests[0].2).unwrap().resolution, resolved_before);
        assert_eq!(wakes.status("orch").held.len(), 1);
    }
}
