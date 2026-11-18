//! Scoped assembly for the read-only Work Outline health check.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use mast_proto::{ChatIndicator, SessionStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolsService, CallContext, in_subtree, lock, non_empty, parse_args};
use crate::work_outline_health::{HealthChat, HealthPullRequest, branch_issue, check_health, health_items, reconcile_health_chats, subtree_items};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthArgs {
    #[serde(default)]
    work_item_id: Option<String>,
}

impl AgentToolsService {
    pub(super) fn health(&self, context: &CallContext<'_>, arguments: &Value) -> Result<Value, AgentToolError> {
        let args: HealthArgs = parse_args(arguments)?;
        let root = context.require_item("checking its subtree")?;
        let target = non_empty(args.work_item_id)
            .map(|id| self.resolve_work_item_id(context, &id)).transpose()?
            .unwrap_or_else(|| root.clone());
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        if snapshot.item(&target).is_none() || !in_subtree(&snapshot, &root, &target) {
            return Err(mast_proto::WorkOutlineError::unauthorized("Agents may only check their own work item and descendants").into());
        }
        tokio::runtime::Handle::current().block_on(Box::pin(self.health_snapshot(&snapshot, &target)))
    }

    /// Assemble once per request from registry and already cached PR facts.
    /// Unknown facts are explicit, so a missing cache entry never becomes a
    /// false assertion that a PR does not exist.
    pub async fn health_snapshot(&self, snapshot: &mast_proto::WorkOutlineSnapshot, root: &str) -> Result<Value, AgentToolError> {
        let scoped = subtree_items(snapshot, root);
        let mut linear_completion = HashMap::new();
        let mut linear_states = HashMap::new();
        let mut item_references = HashMap::<&str, &mast_proto::WorkItemExternalReferenceView>::new();
        let scoped_ids: HashSet<_> = scoped.iter().map(|item| item.id.as_str()).collect();
        let mut ticket_ids: HashSet<_> = snapshot.external_references.iter().filter(|reference|
            scoped_ids.contains(reference.work_item_id.as_str()) && reference.provider == mast_proto::linear::LINEAR_PROVIDER
                && reference.reference_type == mast_proto::linear::LINEAR_ISSUE_REFERENCE)
            .map(|reference| reference.external_id.to_ascii_uppercase()).collect();
        for reference in snapshot.external_references.iter().filter(|reference|
            scoped_ids.contains(reference.work_item_id.as_str()) && reference.provider == mast_proto::linear::LINEAR_PROVIDER
                && reference.reference_type == mast_proto::linear::LINEAR_ISSUE_REFERENCE) {
            item_references.entry(reference.work_item_id.as_str()).and_modify(|current| {
                if (reference.created_at, &reference.id) < (current.created_at, &current.id) { *current = reference; }
            }).or_insert(reference);
        }
        let item_issues: HashMap<_, _> = item_references.into_iter().map(|(id, reference)| (id.to_owned(), reference.external_id.to_ascii_uppercase())).collect();
        let project = snapshot.linear_project_reference(root);
        let mut unknown_linear = project.is_some() && snapshot.space_settings.linear_enabled;
        let linear = lock(&self.linear).clone().map(|linear| {
            let account = self.workspace.space_linear_account_id(&snapshot.space_id);
            linear.for_account(account.as_deref())
        });
        if let (Some(project), Some(linear)) = (project, linear)
            && snapshot.space_settings.linear_enabled {
            let projects = linear.my_projects(false, true).await.ok();
            let project_id = projects.as_ref().and_then(|projects| projects.iter().find(|candidate| {
                candidate.id == project.external_id || (!project.url.is_empty() && candidate.url.trim_end_matches('/') == project.url.trim_end_matches('/'))
                    || candidate.url.trim_end_matches('/').rsplit('/').next() == Some(project.external_id.as_str())
            })).map_or(project.external_id.as_str(), |candidate| candidate.id.as_str());
            if let Ok(issues) = linear.project_issues(project_id, true, false, true).await {
                linear_states = issues.iter().map(|issue| (issue.identifier.to_ascii_uppercase(), issue.state.state_type.is_closed())).collect();
                ticket_ids.extend(linear_states.keys().cloned());
                for (item_id, issue) in &item_issues {
                    if let Some(completed) = linear_states.get(issue) {
                        linear_completion.insert(item_id.clone(), *completed);
                    }
                }
                unknown_linear = false;
            }
        }
        let mut items = health_items(snapshot, root, &linear_completion);
        let assignments: HashMap<_, _> = scoped.iter().flat_map(|item|
            item.chat_ids.iter().map(move |id| (id.as_str(), item.id.as_str()))).collect();
        let scoped_items: HashMap<_, _> = scoped.iter().map(|item| (item.id.as_str(), *item)).collect();
        let bindings: HashMap<_, _> = snapshot.chat_execution_bindings.iter().map(|binding| (binding.chat_id.as_str(), binding)).collect();
        let mut requests = HashMap::<&str, Vec<&mast_proto::UserActionRequestView>>::new();
        for request in snapshot.user_action_requests.iter().filter(|request| !request.deduped) {
            requests.entry(&request.chat_id).or_default().push(request);
        }
        let chats = self.workspace.read_chats().map_err(|error| AgentToolError::invalid(error.to_string()))?;
        // Unassigned child chats belong to the orchestration tree too. Do not
        // follow a chat assigned outside this subtree, even if it is a child.
        let assigned_elsewhere: HashSet<_> = snapshot.items.iter().filter(|item| !scoped_ids.contains(item.id.as_str()))
            .flat_map(|item| item.chat_ids.iter().map(String::as_str)).collect();
        let mut children = HashMap::<&str, Vec<&mast_proto::Chat>>::new();
        for chat in chats.iter().filter(|chat| chat.space_id.as_deref() == Some(snapshot.space_id.as_str())) {
            if let Some(parent) = chat.parent_chat_id.as_deref() { children.entry(parent).or_default().push(chat); }
        }
        let mut scoped_chats: HashSet<&str> = assignments.keys().copied().collect();
        let mut queue: Vec<_> = scoped_chats.iter().copied().collect();
        while let Some(parent) = queue.pop() {
            for child in children.get(parent).into_iter().flatten() {
                if !assigned_elsewhere.contains(child.id.as_str()) && scoped_chats.insert(&child.id) {
                    queue.push(&child.id);
                }
            }
        }
        let cache = lock(&self.pr_ticket_cache).clone();
        let sessions = lock(&self.sessions).clone();
        let target_views = lock(&self.diff_sync).as_ref().map(|sync| {
            let receiver = sync.watch_chat_diff_targets();
            let views = receiver.borrow().clone();
            views
        }).unwrap_or_default();
        let target_views: HashMap<_, _> = target_views.iter().map(|view| (view.chat_id.as_str(), view)).collect();
        let mut unknown_prs = false;
        let mut prs = Vec::new();
        for reference in snapshot.external_references.iter().filter(|reference|
            scoped_ids.contains(reference.work_item_id.as_str())
                && reference.provider == mast_proto::pr_review_sync::GITHUB_PROVIDER
                && reference.reference_type == mast_proto::pr_review_sync::GITHUB_PULL_REQUEST_REFERENCE) {
            match cache.as_ref().and_then(|cache| cache.peek_pr_url(&reference.url)) {
                Some(Some(detail)) => prs.push(HealthPullRequest { url: reference.url.clone(), item_id: Some(reference.work_item_id.clone()),
                    linked_to_ticket: unknown_linear || detail.branch.as_deref().is_some_and(|branch| branch_issue(branch, &ticket_ids).is_some()),
                    open: detail.state.eq_ignore_ascii_case("open"), merged: detail.state.eq_ignore_ascii_case("merged") }),
                Some(None) => {},
                None => unknown_prs = true,
            }
        }
        let default_branch = match snapshot.execution_space.as_ref() {
            Some(space) if space.device_id == self.workspace.device_id() => self.health_branches.get(&space.path).await,
            _ => None,
        };
        let mut facts = Vec::new();
        let mut chat_issues = HashMap::new();
        for chat in chats.iter().filter(|chat| scoped_chats.contains(chat.id.as_str()) && chat.space_id.as_deref() == Some(snapshot.space_id.as_str())) {
            let item_id = assignments.get(chat.id.as_str()).copied();
            let item = item_id.and_then(|id| scoped_items.get(id).copied());
            let branch_ticket = chat.linked_ticket_id.clone().or_else(|| chat.branch.as_deref().and_then(|branch|
                branch_issue(branch, &ticket_ids).or_else(|| unknown_linear.then(|| crate::pr_ticket_cache::extract_ticket_id(branch)).flatten())));
            if !chat.archived && let Some(issue) = &branch_ticket { chat_issues.insert(chat.id.clone(), issue.to_ascii_uppercase()); }
            let linked_to_ticket = unknown_linear || branch_ticket.as_ref().is_some_and(|id| ticket_ids.contains(&id.to_ascii_uppercase()));
            let links = self.workspace.chat_pr_links(&chat.id).map_err(|error| AgentToolError::invalid(error.to_string()))?;
            let mut states = Vec::new();
            let mut owned_states = Vec::new();
            let mut known = true;
            let mut owned_known = true;
            if links.is_empty() && chat.branch.is_some() {
                match cache.as_ref().and_then(|cache| chat.cwd.as_deref().zip(chat.branch.as_deref())
                    .and_then(|(cwd, branch)| cache.peek_pr_branch(cwd, branch, false))) {
                    Some(Some(detail)) => {
                        states.push(detail.state.clone());
                        owned_states.push(detail.state.clone());
                        if let Some(url) = detail.url { prs.push(HealthPullRequest { url, item_id: item_id.map(str::to_owned), linked_to_ticket,
                            open: detail.state.eq_ignore_ascii_case("open"), merged: detail.state.eq_ignore_ascii_case("merged") }); }
                    }
                    Some(None) => {},
                    None => { known = false; owned_known = false; },
                }
            } else if !links.is_empty() {
                for link in &links {
                    match cache.as_ref().and_then(|cache| cache.peek_pr_url(&link.url)) {
                        Some(Some(detail)) => {
                            states.push(detail.state.clone());
                            if crate::archivable::owns_pr_link(link) { owned_states.push(detail.state.clone()); }
                            prs.push(HealthPullRequest { url: link.url.clone(), item_id: item_id.map(str::to_owned), linked_to_ticket,
                                open: detail.state.eq_ignore_ascii_case("open"), merged: detail.state.eq_ignore_ascii_case("merged") });
                        }
                        Some(None) => {},
                        None => { known = false; if crate::archivable::owns_pr_link(link) { owned_known = false; } },
                    }
                }
                // A branch can also have a distinct inferred PR. Treat an
                // unavailable branch lookup as unknown instead of declaring a
                // chat stale solely because its durable links are closed.
                if let Some((cwd, branch)) = chat.cwd.as_deref().zip(chat.branch.as_deref()) {
                    match cache.as_ref().and_then(|cache| cache.peek_pr_branch(cwd, branch, false)) {
                        Some(Some(detail)) => {
                            states.push(detail.state.clone());
                            owned_states.push(detail.state.clone());
                            if let Some(url) = detail.url { prs.push(HealthPullRequest { url, item_id: item_id.map(str::to_owned), linked_to_ticket,
                                open: detail.state.eq_ignore_ascii_case("open"), merged: detail.state.eq_ignore_ascii_case("merged") }); }
                        }
                        Some(None) => {},
                        None => { known = false; owned_known = false; },
                    }
                } else if chat.branch.is_some() {
                    known = false;
                    owned_known = false;
                }
            }
            let stored = self.workspace.chat_diff_targets(&chat.id).map_err(|error| AgentToolError::invalid(error.to_string()))?;
            let view = target_views.get(chat.id.as_str()).copied();
            let mut targets = HashMap::<&str, Option<&str>>::new();
            if let Some(stored) = &stored {
                for target in &stored.targets {
                    let branch = view.and_then(|view| view.targets.iter().find(|candidate| candidate.path == target.path))
                        .and_then(|target| target.branch.as_deref());
                    targets.insert(&target.path, branch);
                }
            }
            if let Some(binding) = bindings.get(chat.id.as_str()) {
                for repo in &binding.resolved.configuration.bindings {
                    if let Some(path) = repo.worktree_path.as_deref() {
                        targets.entry(path).or_insert_with(|| repo.expected_ref.as_deref().map(|name| name.strip_prefix("refs/heads/").unwrap_or(name)));
                    }
                }
            }
            let bound = !targets.is_empty();
            for (path, branch) in targets {
                match branch.and_then(|branch| cache.as_ref().and_then(|cache| cache.peek_pr_branch(path, branch, true))) {
                    Some(Some(detail)) => {
                        states.push(detail.state.clone());
                        owned_states.push(detail.state.clone());
                        let linked = branch.and_then(|branch| branch_issue(branch, &ticket_ids)).is_some() || linked_to_ticket;
                        if let Some(url) = detail.url { prs.push(HealthPullRequest { url, item_id: item_id.map(str::to_owned), linked_to_ticket: linked,
                            open: detail.state.eq_ignore_ascii_case("open"), merged: detail.state.eq_ignore_ascii_case("merged") }); }
                    }
                    Some(None) => {},
                    None => { known = false; owned_known = false; },
                }
            }
            unknown_prs |= !known;
            let indicator = match sessions.as_ref().and_then(|sessions| sessions.session_status(&chat.id)).map(|session| session.status) {
                Some(SessionStatus::Working) => ChatIndicator::Working,
                Some(SessionStatus::AwaitingInput) => ChatIndicator::AwaitingInput,
                Some(SessionStatus::Errored) => ChatIndicator::Errored,
                _ => mast_proto::chat_indicator(chat, None),
            };
            let chat_requests = requests.get(chat.id.as_str()).cloned().unwrap_or_default();
            let waiting = chat_requests.iter().any(|request| request.is_open());
            let queued = self.doc_host.as_ref().is_some_and(|doc| doc.queued_count(&chat.id) > 0);
            let finished = crate::archivable::archivable(&crate::archivable::ArchivableInput {
                chat, indicator, queued,
                item, requests: chat_requests, pr_states: if owned_known { owned_states } else { Vec::new() }, now: Utc::now(),
            }).archivable;
            let last = chat.last_message_at.unwrap_or(chat.created_at);
            facts.push(HealthChat { id: chat.id.clone(), item_id: item_id.map(str::to_owned), archived: chat.archived, finished,
                stale: !waiting && !queued && !matches!(indicator, ChatIndicator::Working | ChatIndicator::AwaitingInput | ChatIndicator::Errored) && Utc::now() - last > Duration::days(3),
                has_open_pr: if states.iter().any(|state| state.eq_ignore_ascii_case("open")) { Some(true) } else if known { Some(false) } else { None },
                on_default_branch: default_branch.as_deref().zip(chat.branch.as_deref()).is_some_and(|(a, b)| a == b),
                has_bound_worktree: bound });
        }
        if project.is_some() && !unknown_linear {
            reconcile_health_chats(&mut items, &mut facts, &item_issues, &chat_issues, &linear_states);
        }
        let entries = check_health(&items, &facts, &prs);
        Ok(json!({ "workItemId": root, "entries": entries,
            "unknown": { "pullRequests": unknown_prs, "defaultBranch": default_branch.is_none(),
                "linearCompletion": unknown_linear } }))
    }
}
