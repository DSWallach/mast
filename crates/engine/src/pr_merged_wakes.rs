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
    #[serde(default)]
    observed_merges: HashMap<String, HashSet<String>>,
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CompletionObservation {
    Url(String),
    Branch(PathBuf, String),
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
    /// Candidate PRs for opted-in, unfinished items. This is a registry-only
    /// read; the existing PR sweep performs bounded, cached refreshes.
    pub(crate) fn completion_observations(&self) -> Result<Vec<CompletionObservation>, String> {
        let chats = self.workspace.read_chats().map_err(|e| e.to_string())?;
        let mut keys = std::collections::BTreeSet::new();
        for space in self.workspace.read_spaces().map_err(|e| e.to_string())? {
            if space.device_id != self.workspace.device_id() { continue; }
            let snapshot = mast_doc::materialize_work_outline(&space.id, &self.workspace.work_outline_inputs(&space.id));
            if snapshot.space_settings.merged_pr_completion.is_none() { continue; }
            let active: HashSet<&str> = snapshot.items.iter().filter(|item| item.archived_at.is_none() && item.status != mast_proto::WorkItemStatus::Completed)
                .map(|item| item.id.as_str()).collect();
            let pending_items: HashSet<&str> = snapshot.user_action_requests.iter()
                .filter(|request| request.is_open() && request.dedupe_key.as_deref().is_some_and(|key| key.starts_with("merged-pr-completion:")))
                .map(|request| request.work_item_id.as_str()).collect();
            let pending_prs: HashSet<(&str, &str)> = snapshot.user_action_requests.iter()
                .filter_map(|request| request.dedupe_key.as_deref().map(|key| (request.work_item_id.as_str(), key))).collect();
            for reference in &snapshot.external_references {
                if active.contains(reference.work_item_id.as_str())
                    && reference.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER
                    && reference.reference_type == mast_proto::pr_review_sync::GITHUB_PULL_REQUEST_REFERENCE
                    && let Some(pr) = mast_proto::PullRequestRef::parse_external_id(&reference.external_id) {
                        let key = format!("merged-pr-completion:{}", pr.key());
                        if !pending_prs.contains(&(reference.work_item_id.as_str(), key.as_str())) {
                            keys.insert(CompletionObservation::Url(pr.url));
                        }
                    }
            }
            let assigned: HashSet<&str> = snapshot.assignments.iter().filter(|a| a.state == mast_proto::work_outline::WorkItemChatAssignmentState::Associated
                && a.work_item_id.as_deref().is_some_and(|id| active.contains(id) && !pending_items.contains(id)))
                .map(|a| a.chat_id.as_str()).collect();
            for chat in chats.iter().filter(|chat| chat.space_id.as_deref() == Some(space.id.as_str())
                && chat.parent_chat_id.is_some() && assigned.contains(chat.id.as_str())) {
                if let (Some(cwd), Some(branch)) = (chat.cwd.as_deref(), chat.branch.as_deref())
                    && !cwd.is_empty() && !branch.is_empty()
                    && !crate::repos::is_automatic_access_blocked(Path::new(cwd)) {
                        keys.insert(CompletionObservation::Branch(PathBuf::from(cwd), branch.to_owned()));
                    }
            }
        }
        Ok(keys.into_iter().collect())
    }
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

    fn complete_merged_items(&self, url: &str, status: &PrStatus, checkout: Option<(&Path, &str)>, allowed_spaces: Option<&HashSet<String>>, first_merge_at: Option<i64>) -> Result<HashSet<String>, String> {
        use mast_proto::work_outline::MergedPrCompletion;
        use mast_proto::work_outline_user_actions::{RequestLink, RequestLinkTarget, UserActionKind, UserActionOriginKind, UserActionRequestOrigin};
        let pr = crate::work_outline_pr_links::pull_request_from_url(url).ok_or("invalid merged PR URL")?;
        let chats = self.workspace.read_chats().map_err(|e| e.to_string())?;
        let mut enabled_spaces = HashSet::new();
        for space in self.workspace.read_spaces().map_err(|e| e.to_string())? {
            if space.device_id != self.workspace.device_id() { continue; }
            if allowed_spaces.is_some_and(|allowed| !allowed.contains(&space.id)) { continue; }
            let snapshot = mast_doc::materialize_work_outline(&space.id, &self.workspace.work_outline_inputs(&space.id));
            let Some(mode) = snapshot.space_settings.merged_pr_completion else { continue; };
            // GitHub's merge timestamp can have second precision while the
            // setting uses milliseconds; allow the remainder of that second.
            if first_merge_at.is_some_and(|at| snapshot.space_settings.merged_pr_completion_since.is_none_or(|since| at.saturating_add(999) < since)) { continue; }
            enabled_spaces.insert(space.id.clone());
            let mut items = HashSet::new();
            let mut branch_items = HashSet::new();
            for reference in &snapshot.external_references {
                if reference.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER
                    && reference.reference_type == mast_proto::pr_review_sync::GITHUB_PULL_REQUEST_REFERENCE
                    && mast_proto::PullRequestRef::parse_external_id(&reference.external_id).is_some_and(|r| r.key() == pr.key()) {
                    items.insert(reference.work_item_id.clone());
                }
            }
            let assignments: HashMap<&str, &str> = snapshot.assignments.iter()
                .filter(|a| a.state == mast_proto::work_outline::WorkItemChatAssignmentState::Associated)
                .filter_map(|a| a.work_item_id.as_deref().map(|item| (a.chat_id.as_str(), item))).collect();
            for chat in chats.iter().filter(|c| c.space_id.as_deref() == Some(space.id.as_str()) && c.parent_chat_id.is_some()) {
                let Some(item) = assignments.get(chat.id.as_str()) else { continue; };
                let matches_branch = checkout.is_some_and(|(path, branch)| status.branch.as_deref() == Some(branch) && chat.cwd.as_deref().is_some_and(|cwd| Path::new(cwd) == path)
                    && chat.branch.as_deref() == Some(branch));
                let links = self.workspace.chat_pr_links(&chat.id).map_err(|e| e.to_string())?;
                let matches_link = status.branch.as_deref().is_some_and(|branch| chat.branch.as_deref() == Some(branch))
                    && links.iter().any(|link| crate::archivable::owns_pr_link(link)
                        && crate::chat_links::canonicalize_pr_url(&link.url) == url);
                if matches_link { items.insert((*item).to_owned()); }
                if matches_branch { branch_items.insert((*item).to_owned()); }
            }
            if branch_items.len() == 1 { items.extend(branch_items); }
            let actor = mast_proto::WorkOutlineActor { kind: mast_proto::WorkOutlineActorKind::System,
                device_id: self.workspace.device_id().to_owned(), chat_id: None, message_id: None };
            for item_id in items {
                let Some(item) = snapshot.item(&item_id) else { continue; };
                if item.archived_at.is_some() || item.status == mast_proto::WorkItemStatus::Completed { continue; }
                let op = mast_proto::work_outline::derive_id(&format!("{item_id}\0{}", pr.key()), "merged-item");
                let mutation = match mode {
                    MergedPrCompletion::Ask => mast_proto::WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
                        options: vec![], links: vec![RequestLink { label: "PR".into(), target: RequestLinkTarget::Pr { url: url.into() } }], facts: vec![],
                        kind: Some(UserActionKind::Approve), priority: None, work_item_id: item_id,
                        origin: UserActionRequestOrigin { kind: UserActionOriginKind::System, ..Default::default() },
                        text: format!("Mark done? {} — PR #{} merged", item.title, status.number), info: None,
                        dedupe_key: Some(format!("merged-pr-completion:{}", pr.key())), operation_id: op,
                    }),
                    MergedPrCompletion::Automatic => mast_proto::WorkOutlineMutation::UpdateWorkItem(mast_proto::UpdateWorkItemParams {
                        work_item_id: item_id, changes: mast_proto::WorkItemChanges { status: Some(mast_proto::WorkItemStatus::Completed), ..Default::default() }, operation_id: op,
                    }),
                };
                if let Err(error) = self.workspace.work_outline_mutate(&mutation, &actor, &[]) {
                    tracing::warn!(%error, "failed to apply merged PR item automation");
                }
            }
        }
        Ok(enabled_spaces)
    }

    /// Observe a successful URL refresh; failures never replace the last known state.
    #[cfg(test)]
    pub(crate) async fn observe(&self, url: &str, status: &PrStatus) {
        self.observe_with_checkout(url, status, None).await;
    }

    pub(crate) async fn observe_with_checkout(&self, url: &str, status: &PrStatus, checkout: Option<(&Path, &str)>) {
        let url = crate::chat_links::canonicalize_pr_url(url);
        let (previous, observed_merge) = {
            let observations = self.observations.lock().unwrap_or_else(|e| e.into_inner());
            (observations.states.get(&url).cloned(), observations.observed_merges.get(&url).cloned())
        };
        let transition = previous
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case("open"))
            && status.state.eq_ignore_ascii_case("merged");
        let merge_at = if status.state.eq_ignore_ascii_case("merged") {
            status.merged_at.as_deref().and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()).map(|at| at.timestamp_millis())
        } else { None };
        let first_merge = previous.is_none() && merge_at.is_some();
        if transition {
            if let Err(error) = self.resolve_merged_handoffs(&url, status) {
                tracing::warn!(%error, "failed to resolve merged PR handoffs");
                return;
            }
        }
        let mut enabled_spaces = None;
        if status.state.eq_ignore_ascii_case("merged") && (transition || first_merge || (observed_merge.is_some() && checkout.is_some())) {
            match self.complete_merged_items(&url, status, checkout, if transition || first_merge { None } else { observed_merge.as_ref() }, merge_at) {
                Ok(spaces) => enabled_spaces = Some(spaces),
                Err(error) => { tracing::warn!(%error, "failed to complete merged PR items"); return; }
            }
        }
        if transition {
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
        if (transition || first_merge) && checkout.is_none() && let Some(spaces) = enabled_spaces && !spaces.is_empty() {
            if updated.observed_merges.len() >= 2_048
                && let Some(old) = updated.observed_merges.keys().next().cloned() {
                    updated.observed_merges.remove(&old);
                }
            updated.observed_merges.insert(url.clone(), spaces);
        }
        if checkout.is_some() { updated.observed_merges.remove(&url); }
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
        let mut completion_items = Vec::new();
        for (space, mode) in [("completion-off", None), ("completion-ask", Some(mast_proto::work_outline::MergedPrCompletion::Ask)), ("completion-auto", Some(mast_proto::work_outline::MergedPrCompletion::Automatic))] {
            workspace.create_space(space, "device", &format!("/tmp/{space}"), None, false).unwrap();
            let item = mutate(mast_proto::WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
                space_id: space.into(), parent_work_item_id: None, placement: Default::default(), title: "Feature".into(), description: None, status: None, operation_id: format!("create-{space}"),
            })).work_item_id.unwrap();
            mutate(mast_proto::WorkOutlineMutation::CreateWorkItemExternalReference(mast_proto::CreateWorkItemExternalReferenceParams {
                work_item_id: item.clone(), provider: "github".into(), reference_type: "pullRequest".into(), external_id: "acme/repo#42".into(), url: url.into(), cached_title: None, operation_id: format!("link-{space}"),
            }));
            if mode.is_none() {
                mutate(mast_proto::WorkOutlineMutation::CreateWorkItemExternalReference(mast_proto::CreateWorkItemExternalReferenceParams {
                    work_item_id: item.clone(), provider: "github".into(), reference_type: "pullRequest".into(), external_id: "acme/repo#43".into(), url: "https://github.com/acme/repo/pull/43".into(), cached_title: None, operation_id: "off-only-link".into(),
                }));
            }
            mutate(mast_proto::WorkOutlineMutation::SetWorkerAutomation(mast_proto::work_outline::SetWorkerAutomationParams {
                space_id: space.into(), field: None, worker_worktrees: false, merged_pr_completion: mode, archive_workers_after_days: None, operation_id: format!("configure-{space}"),
            }));
            completion_items.push((space, mode, item));
        }
        workspace.create_space("branch-space", "device", "/tmp/branch-space", None, false).unwrap();
        let branch_item = mutate(mast_proto::WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
            space_id: "branch-space".into(), parent_work_item_id: None, placement: Default::default(), title: "Branch item".into(), description: None, status: None, operation_id: "branch-item".into(),
        })).work_item_id.unwrap();
        workspace.create_chat("branch-parent", Some("branch-space"), Some("device"), None, None).unwrap();
        workspace.create_chat_with_parent("branch-worker", Some("branch-space"), Some("device"), None, Some("/tmp/branch-space".into()), Some("branch-parent".into())).unwrap();
        workspace.set_chat_branch("branch-worker", "feature/a").unwrap();
        mutate(mast_proto::WorkOutlineMutation::AssignChatToWorkItem(mast_proto::AssignChatToWorkItemParams {
            chat_id: "branch-worker".into(), work_item_id: branch_item.clone(), operation_id: "branch-assign".into(),
        }));
        let unrelated_item = mutate(mast_proto::WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
            space_id: "branch-space".into(), parent_work_item_id: None, placement: Default::default(), title: "Other branch".into(), description: None, status: None, operation_id: "unrelated-item".into(),
        })).work_item_id.unwrap();
        workspace.create_chat_with_parent("unrelated-worker", Some("branch-space"), Some("device"), None, Some("/tmp/other".into()), Some("branch-parent".into())).unwrap();
        workspace.set_chat_branch("unrelated-worker", "other-branch").unwrap();
        workspace.add_chat_pr_link("unrelated-worker", url, ChatLinkSource::Manual).unwrap();
        mutate(mast_proto::WorkOutlineMutation::AssignChatToWorkItem(mast_proto::AssignChatToWorkItemParams {
            chat_id: "unrelated-worker".into(), work_item_id: unrelated_item.clone(), operation_id: "unrelated-assign".into(),
        }));
        mutate(mast_proto::WorkOutlineMutation::SetWorkerAutomation(mast_proto::work_outline::SetWorkerAutomationParams {
            space_id: "branch-space".into(), field: None, worker_worktrees: false,
            merged_pr_completion: Some(mast_proto::work_outline::MergedPrCompletion::Ask), archive_workers_after_days: None, operation_id: "branch-setting".into(),
        }));
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
        let observations = tracker.completion_observations().unwrap();
        assert!(observations.contains(&CompletionObservation::Url(url.into())));
        assert!(!observations.contains(&CompletionObservation::Url("https://github.com/acme/repo/pull/43".into())));
        assert!(observations.contains(&CompletionObservation::Branch(PathBuf::from("/tmp/branch-space"), "feature/a".into())));
        let mut status = PrStatus {
            additions: None, deletions: None,
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
        let merged_at = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
        status.merged_at = Some(merged_at.clone());
        tracker.observe(url, &status).await;
        tracker.observe_with_checkout(url, &status, Some((Path::new("/tmp/branch-space"), "feature/a"))).await;
        let branch_snapshot = mast_doc::materialize_work_outline("branch-space", &workspace.work_outline_inputs("branch-space"));
        assert!(branch_snapshot.user_action_requests.iter().any(|r| r.work_item_id == branch_item && r.is_open()));
        assert!(!branch_snapshot.user_action_requests.iter().any(|r| r.work_item_id == unrelated_item && r.is_open()));
        for (space, mode, item) in &completion_items {
            let snapshot = mast_doc::materialize_work_outline(space, &workspace.work_outline_inputs(space));
            let completed = snapshot.item(item).unwrap().status == mast_proto::WorkItemStatus::Completed;
            let request = snapshot.user_action_requests.iter().find(|r| r.work_item_id == *item && r.is_open());
            match mode {
                None => { assert!(!completed); assert!(request.is_none()); }
                Some(mast_proto::work_outline::MergedPrCompletion::Ask) => { assert!(!completed); assert!(request.unwrap().text.contains("PR #42 merged")); }
                Some(mast_proto::work_outline::MergedPrCompletion::Automatic) => { assert!(completed); assert!(request.is_none()); }
            }
        }
        for (space, kind, id) in &requests {
            let snapshot = mast_doc::materialize_work_outline(space, &workspace.work_outline_inputs(space));
            let request = snapshot.user_action_request(id).unwrap();
            if space == "enabled-space" && *kind == mast_proto::work_outline_user_actions::UserActionKind::Handoff {
                let resolution = request.resolution.as_ref().unwrap();
                assert_eq!(resolution.outcome.as_deref(), Some("done"));
                assert_eq!(resolution.via.as_deref(), Some("prMerged"));
                assert_eq!(resolution.note.as_deref(), Some(format!("Landed: acme/repo#42 merged {merged_at}").as_str()));
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

        // A PR merged after opt-in but before its first refresh still counts.
        let ask_item = &completion_items.iter().find(|(space, _, _)| *space == "completion-ask").unwrap().2;
        let first_url = "https://github.com/acme/repo/pull/44";
        reopened.workspace.work_outline_mutate(&mast_proto::WorkOutlineMutation::CreateWorkItemExternalReference(mast_proto::CreateWorkItemExternalReferenceParams {
            work_item_id: ask_item.clone(), provider: "github".into(), reference_type: "pullRequest".into(), external_id: "acme/repo#44".into(), url: first_url.into(), cached_title: None, operation_id: "first-merge-link".into(),
        }), &actor, &[]).unwrap();
        status.number = 44;
        status.url = Some(first_url.into());
        status.merged_at = Some((chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339());
        reopened.observe(first_url, &status).await;
        let after = mast_doc::materialize_work_outline("completion-ask", &reopened.workspace.work_outline_inputs("completion-ask"));
        assert!(after.user_action_requests.iter().any(|r| r.work_item_id == *ask_item && r.text.contains("PR #44 merged")));
        let old_url = "https://github.com/acme/repo/pull/45";
        reopened.workspace.work_outline_mutate(&mast_proto::WorkOutlineMutation::CreateWorkItemExternalReference(mast_proto::CreateWorkItemExternalReferenceParams {
            work_item_id: ask_item.clone(), provider: "github".into(), reference_type: "pullRequest".into(), external_id: "acme/repo#45".into(), url: old_url.into(), cached_title: None, operation_id: "historical-merge-link".into(),
        }), &actor, &[]).unwrap();
        status.number = 45;
        status.url = Some(old_url.into());
        status.merged_at = Some("2020-01-01T00:00:00Z".into());
        reopened.observe(old_url, &status).await;
        let after = mast_doc::materialize_work_outline("completion-ask", &reopened.workspace.work_outline_inputs("completion-ask"));
        assert!(!after.user_action_requests.iter().any(|r| r.work_item_id == *ask_item && r.text.contains("PR #45 merged")));
    }
}
