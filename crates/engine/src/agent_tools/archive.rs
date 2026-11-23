//! `chat_archive_info` / `chat_archive`: the engine side of the MCP
//! `archive_chat`, `list_chats` and `get_chat` "finished worker" support.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use mast_proto::{Chat, ChatIndicator, ChatPrLink, WorkOutlineSnapshot, chat_indicator};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolErrorCode, AgentToolsService, CallContext, lock, parse_args};
use crate::archivable::{
    AgentArchive, Archivable, ArchivableInput, archivable, owns_pr_link, reconcile, requests_of,
    worker_scope,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InfoArgs {
    #[serde(default)]
    chat_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveArgs {
    chat_id: String,
    #[serde(default = "yes")]
    archived: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    reason: Option<String>,
}

fn yes() -> bool {
    true
}

impl AgentToolsService {
    pub fn set_pr_ticket_cache(&self, cache: crate::pr_ticket_cache::PrTicketCache) {
        *lock(&self.pr_ticket_cache) = Some(cache);
    }

    /// The finished rule for one chat, from live engine state.
    pub(super) fn archivable_verdict(
        &self,
        chat: &Chat,
        snapshot: &WorkOutlineSnapshot,
    ) -> Archivable {
        self.archivable_verdict_with_states(chat, snapshot, self.pr_states(chat), Utc::now())
    }

    fn archivable_verdict_with_states(
        &self,
        chat: &Chat,
        snapshot: &WorkOutlineSnapshot,
        pr_states: Vec<String>,
        now: chrono::DateTime<Utc>,
    ) -> Archivable {
        self.verdict_with(chat, snapshot, pr_states, now, archivable)
    }

    /// The automatic rule: confirmed completion only (see [`reconcile`]).
    fn reconcile_verdict_with_states(
        &self,
        chat: &Chat,
        snapshot: &WorkOutlineSnapshot,
        pr_states: Vec<String>,
        now: chrono::DateTime<Utc>,
    ) -> Archivable {
        self.verdict_with(chat, snapshot, pr_states, now, reconcile)
    }

    fn verdict_with(
        &self,
        chat: &Chat,
        snapshot: &WorkOutlineSnapshot,
        pr_states: Vec<String>,
        now: chrono::DateTime<Utc>,
        rule: fn(&ArchivableInput<'_>) -> Archivable,
    ) -> Archivable {
        let live = lock(&self.sessions)
            .as_ref()
            .and_then(|s| s.session_status(&chat.id));
        let indicator = match live.as_ref().map(|s| s.status) {
            Some(mast_proto::SessionStatus::Working) => ChatIndicator::Working,
            Some(mast_proto::SessionStatus::AwaitingInput) => ChatIndicator::AwaitingInput,
            _ => chat_indicator(chat, None),
        };
        let item = snapshot
            .assignments
            .iter()
            .find(|a| a.chat_id == chat.id)
            .and_then(|a| a.work_item_id.as_deref())
            .and_then(|id| snapshot.item(id));
        rule(&ArchivableInput {
            chat,
            indicator,
            queued: self
                .doc_host
                .as_ref()
                .is_some_and(|d| d.queued_count(&chat.id) > 0),
            item,
            requests: requests_of(snapshot, &chat.id),
            pr_states,
            now,
        })
    }

    /// Archive only eligible workers using already cached PR state. This does
    /// not schedule PR refreshes or git probes.
    pub(crate) fn auto_archive_workers(&self, now: chrono::DateTime<Utc>) -> Result<usize, String> {
        let chats = self.workspace.read_chats().map_err(|e| e.to_string())?;
        let sessions_by_chat: HashMap<String, chrono::DateTime<Utc>> = self.workspace.read_sessions().map_err(|e| e.to_string())?
            .into_iter().map(|session| (session.chat_id, session.updated_at)).collect();
        let cache = lock(&self.pr_ticket_cache).clone();
        let mut archived = 0;
        let mut candidates = 0;
        for space in self.workspace.read_spaces().map_err(|e| e.to_string())? {
            if space.device_id != self.workspace.device_id() { continue; }
            let snapshot = self.work_outline.snapshot(&space.id);
            let Some(days) = snapshot.space_settings.archive_workers_after_days else { continue; };
            let assignments: HashMap<&str, &str> = snapshot.assignments.iter()
                .filter(|a| a.state == mast_proto::work_outline::WorkItemChatAssignmentState::Associated)
                .filter_map(|a| a.work_item_id.as_deref().map(|item| (a.chat_id.as_str(), item))).collect();
            let open_items: HashSet<&str> = snapshot.user_action_requests.iter()
                .filter(|request| request.is_open()).map(|request| request.work_item_id.as_str()).collect();
            let owner_items: HashSet<&str> = assignments.values().copied().collect();
            // The chats the Team tab counts: children of a chat, and chats on
            // an item below one that has its own chat (pre-existing included).
            let team_counted = |chat: &Chat| chat.parent_chat_id.is_some() || assignments.get(chat.id.as_str()).is_some_and(|item| {
                let mut parent = snapshot.item(item).and_then(|i| i.parent_work_item_id.as_deref());
                let mut hops = 0;
                while let Some(id) = parent {
                    if owner_items.contains(id) { return true; }
                    hops += 1;
                    if hops > 64 { return false; }
                    parent = snapshot.item(id).and_then(|i| i.parent_work_item_id.as_deref());
                }
                false
            });
            for chat in chats.iter().filter(|chat| chat.space_id.as_deref() == Some(space.id.as_str()) && team_counted(chat)) {
                let last_activity = chat.last_message_at.unwrap_or(chat.created_at)
                    .max(sessions_by_chat.get(&chat.id).copied().unwrap_or(chat.created_at));
                if now.signed_duration_since(last_activity) <= chrono::Duration::days(i64::from(days)) { continue; }
                let Some(item_id) = assignments.get(chat.id.as_str()) else { continue; };
                if open_items.contains(item_id) { continue; }
                let links = self.workspace.chat_pr_links(&chat.id).map_err(|e| e.to_string())?;
                let states = owned_pr_states(&links, |url| cache.as_ref().and_then(|cache| cache.cached_pr_state(url)));
                if !self.reconcile_verdict_with_states(chat, &snapshot, states, now).archivable { continue; }
                // Bound the work (including registry re-materialization) in
                // one daily pass; remaining eligible workers wait for the next.
                if candidates == 50 { return Ok(archived); }
                candidates += 1;
                let sessions = lock(&self.sessions).clone();
                let _dispatch_guard = match sessions.as_ref() {
                    Some(sessions) => match sessions.try_lock_dispatch(&chat.id) {
                        Some(guard) => Some(guard),
                        None => continue,
                    },
                    None => None,
                };
                let states = owned_pr_states(&links, |url| cache.as_ref().and_then(|cache| cache.cached_pr_state(url)));
                let live = sessions.as_ref().and_then(|sessions| sessions.session_status(&chat.id));
                if live.as_ref().is_some_and(|session| now.signed_duration_since(session.updated_at) <= chrono::Duration::days(i64::from(days))) { continue; }
                let indicator = match live.as_ref().map(|session| session.status) {
                    Some(mast_proto::SessionStatus::Working) => ChatIndicator::Working,
                    Some(mast_proto::SessionStatus::AwaitingInput) => ChatIndicator::AwaitingInput,
                    _ => chat_indicator(chat, None),
                };
                let queued = self.doc_host.as_ref().is_some_and(|doc| doc.queued_count(&chat.id) > 0);
                let changed = self.workspace.archive_chat_if(&chat.id, &space.id, |current, current_snapshot| {
                    let item_id = current_snapshot.assignments.iter().find(|a| a.chat_id == current.id && a.state == mast_proto::work_outline::WorkItemChatAssignmentState::Associated)
                        .and_then(|a| a.work_item_id.as_deref());
                    current.last_message_at == chat.last_message_at
                        && current_snapshot.space_settings.archive_workers_after_days == Some(days)
                        && item_id.is_some_and(|id| !current_snapshot.user_action_requests.iter().any(|r| r.work_item_id == id && r.is_open()))
                        && !matches!(chat_indicator(current, None), ChatIndicator::Working | ChatIndicator::AwaitingInput)
                        && reconcile(&ArchivableInput {
                            chat: current, indicator, queued,
                            item: item_id.and_then(|id| current_snapshot.item(id)),
                            requests: requests_of(current_snapshot, &current.id), pr_states: states, now,
                        }).archivable
                }).map_err(|e| e.to_string())?;
                archived += usize::from(changed);
            }
        }
        Ok(archived)
    }

    /// States of the chat's own durable PR links, from the status cache.
    fn pr_states(&self, chat: &Chat) -> Vec<String> {
        let Some(cache) = lock(&self.pr_ticket_cache).clone() else {
            return Vec::new();
        };
        let links = self.workspace.chat_pr_links(&chat.id).unwrap_or_default();
        if links.is_empty() {
            return Vec::new();
        }
        let status = cache.status_for_chat_links(
            chat.space_id.as_deref(),
            chat.cwd.as_deref(),
            None,
            &links,
            None,
        );
        owned_pr_states(&links, |url| {
            status
                .pr_links
                .iter()
                .find(|link| link.url == url)
                .and_then(|link| link.detail.as_ref().map(|detail| detail.state.clone()))
        })
    }

    fn archive_scope(
        &self,
        context: &CallContext<'_>,
        snapshot: &WorkOutlineSnapshot,
    ) -> (Vec<Chat>, HashSet<String>) {
        let chats: Vec<Chat> = self
            .workspace
            .read_chats()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| {
                context.personal_assistant
                    || c.space_id.as_deref() == Some(context.grant.space_id.as_str())
            })
            .collect();
        let mut scope = worker_scope(
            snapshot,
            &chats,
            &context.grant.chat_id,
            context.current_item_id().as_deref(),
        );
        // An admin chat (live user grant on this Space) reaches every chat of
        // it; the personal assistant every chat of every Space.
        if context.personal_assistant
            || context.admin_space.as_deref() == Some(context.grant.space_id.as_str())
        {
            scope.extend(chats.iter().map(|c| c.id.clone()));
            scope.remove(&context.grant.chat_id);
        }
        // A project manager's scope is its project's chats and nothing else
        // (not the workers or children its other routes would add); it still
        // passes the finished-worker check or force-with-reason. An admin
        // grant or the assistant outranks it.
        let wider = context.personal_assistant
            || context.admin_space.as_deref() == Some(context.grant.space_id.as_str());
        if context.manager_scope.is_some() && !wider {
            scope = self.manager_scope_chats(context).into_iter().collect();
        }
        (chats, scope)
    }

    pub(super) fn chat_archive_info(
        &self,
        context: &CallContext<'_>,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let args: InfoArgs = parse_args(arguments)?;
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        let (chats, scope) = self.archive_scope(context, &snapshot);
        // The personal assistant's scope spans Spaces: judge each chat
        // against its own Space's outline.
        let mut other_spaces = std::collections::HashMap::new();
        let mut out = serde_json::Map::new();
        for chat in chats.iter().filter(|c| {
            (args.chat_ids.is_empty() || args.chat_ids.contains(&c.id)) && scope.contains(&c.id)
        }) {
            let space = chat.space_id.as_deref().unwrap_or_default();
            let snapshot = if space == context.grant.space_id {
                &snapshot
            } else {
                other_spaces
                    .entry(space.to_owned())
                    .or_insert_with(|| self.work_outline.snapshot(space))
            };
            let v = self.archivable_verdict(chat, snapshot);
            out.insert(
                chat.id.clone(),
                json!({ "archivable": v.archivable, "reason": v.reason, "inScope": true }),
            );
        }
        Ok(json!({ "chats": out }))
    }

    pub(super) fn chat_archive(
        &self,
        context: &CallContext<'_>,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let args: ArchiveArgs = parse_args(arguments)?;
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        let (chats, scope) = self.archive_scope(context, &snapshot);
        let caller = &context.grant.chat_id;
        let Some(chat) = chats.iter().find(|c| c.id == args.chat_id) else {
            return Err(AgentToolError::refused(
                AgentToolErrorCode::Forbidden,
                "no such chat in this Space",
            ));
        };
        if !scope.contains(&chat.id) {
            return Err(AgentToolError::refused(
                AgentToolErrorCode::Forbidden,
                "agents can only archive chats they created and the workers in their project",
            ));
        }
        let child = chat.parent_chat_id.as_deref() == Some(caller.as_str());
        let mut forced = false;
        let mut reason = String::new();
        let admin = context.admin_space.as_deref() == Some(context.grant.space_id.as_str());
        if args.archived && context.personal_assistant && !child {
            reason = "personal assistant".to_owned();
        } else if args.archived && admin && !child {
            reason = "admin chat".to_owned();
        } else if args.archived && !child {
            let verdict = self.archivable_verdict(chat, &snapshot);
            reason = verdict.reason.clone();
            if !verdict.archivable {
                let why = args.reason.as_deref().map(str::trim).unwrap_or_default();
                if !args.force {
                    return Err(AgentToolError::refused(
                        AgentToolErrorCode::Forbidden,
                        format!(
                            "not archived: {}. Pass force: true with a short reason to archive anyway",
                            verdict.reason
                        ),
                    ));
                }
                if why.is_empty() {
                    return Err(AgentToolError::invalid("force needs a short reason"));
                }
                forced = true;
                reason = why.to_owned();
            }
        }
        if let Some(why) = args
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            && !forced
            && child
        {
            reason = why.to_owned();
        }
        self.workspace
            .set_chat_archived(&chat.id, args.archived)
            .map_err(|e| AgentToolError::invalid(e.to_string()))?;
        if args.archived {
            let orchestrator_title = chats
                .iter()
                .find(|c| c.id == *caller)
                .and_then(|c| c.title.clone());
            self.archive_log.record(AgentArchive {
                // The archived chat's Space, so Recent changes there offers the undo.
                space_id: chat
                    .space_id
                    .clone()
                    .unwrap_or_else(|| context.grant.space_id.clone()),
                chat_id: chat.id.clone(),
                chat_title: chat.title.clone(),
                orchestrator_chat_id: caller.clone(),
                orchestrator_title,
                reason: reason.clone(),
                forced,
                at_ms: Utc::now().timestamp_millis(),
                seq: 0,
            });
        }
        Ok(json!({
            "chatId": chat.id, "title": chat.title, "archived": args.archived,
            "forced": forced, "reason": reason,
        }))
    }

    /// Agent archives after `seq`, for the Recent changes list and undo.
    pub fn agent_archives_since(&self, seq: u64) -> Vec<AgentArchive> {
        self.archive_log.since(seq)
    }
}

fn owned_pr_states(links: &[ChatPrLink], state: impl Fn(&str) -> Option<String>) -> Vec<String> {
    links
        .iter()
        .filter(|link| owns_pr_link(link))
        .map(|link| state(&link.url).unwrap_or_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::ChatLinkSource;

    #[tokio::test]
    async fn automatic_archive_requires_a_finished_idle_worker_and_no_open_request() {
        use crate::workspace_host::WorkspaceHostConfig;
        use mast_proto::{WorkItemStatus, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation};
        let directory = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(mast_sync::DocsStore::open(directory.path()).unwrap());
        let workspace = crate::WorkspaceHost::open(store, WorkspaceHostConfig {
            device_id: "device".into(), device_name: "test".into(), platform: "test".into(),
            org_id: "org".into(), user_id: "user".into(), edge: None,
        }).unwrap();
        workspace.create_space("space", "device", "/tmp/project", None, false).unwrap();
        workspace.create_chat("parent", Some("space"), Some("device"), None, None).unwrap();
        let actor = WorkOutlineActor { kind: WorkOutlineActorKind::System, device_id: "device".into(), chat_id: None, message_id: None };
        let mutate = |m: WorkOutlineMutation| workspace.work_outline_mutate(&m, &actor, &[]).unwrap();
        let now = Utc::now();
        let mut request_item = String::new();
        let mut system_request_item = String::new();
        for name in ["eligible", "recent", "request", "system-request", "working", "awaiting", "recently-idle"] {
            let item = mutate(WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
                space_id: "space".into(), parent_work_item_id: None, placement: Default::default(), title: name.into(),
                description: None, status: Some(WorkItemStatus::Completed), operation_id: format!("item-{name}"),
            })).work_item_id.unwrap();
            workspace.create_chat_with_parent(name, Some("space"), Some("device"), None, None, Some("parent".into())).unwrap();
            mutate(WorkOutlineMutation::AssignChatToWorkItem(mast_proto::AssignChatToWorkItemParams {
                chat_id: name.into(), work_item_id: item.clone(), operation_id: format!("assign-{name}"),
            }));
            let age = if name == "recent" { 2 } else { 4 };
            workspace.set_chat_activity(name, Some((now - chrono::Duration::days(age)).timestamp_millis()), Some((now - chrono::Duration::days(10)).timestamp_millis())).unwrap();
            if name == "request" { request_item = item; }
            else if name == "system-request" { system_request_item = item; }
        }
        mutate(WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
            options: vec![], links: vec![], facts: vec![], kind: None, priority: None, work_item_id: request_item,
            origin: mast_proto::UserActionRequestOrigin { chat_id: "request".into(), ..Default::default() },
            text: "Waiting".into(), info: None, dedupe_key: None, operation_id: "request-open".into(),
        }));
        mutate(WorkOutlineMutation::CreateUserActionRequest(mast_proto::CreateUserActionRequestParams {
            options: vec![], links: vec![], facts: vec![], kind: None, priority: None, work_item_id: system_request_item,
            origin: mast_proto::UserActionRequestOrigin { kind: mast_proto::UserActionOriginKind::System, ..Default::default() },
            text: "System request".into(), info: None, dedupe_key: None, operation_id: "system-request-open".into(),
        }));
        let service = AgentToolsService::new(super::super::AgentTokens::new(), workspace.clone(),
            std::sync::Arc::new(crate::work_outline::WorkOutlineService::new(workspace.clone())), None);
        assert_eq!(service.auto_archive_workers(now).unwrap(), 0);
        mutate(WorkOutlineMutation::SetWorkerAutomation(mast_proto::work_outline::SetWorkerAutomationParams {
            space_id: "space".into(), field: None, worker_worktrees: false, merged_pr_completion: None,
            archive_workers_after_days: Some(3), operation_id: "enable-archive".into(),
        }));
        let sessions = crate::SessionsEngine::new("device".into(),
            std::sync::Arc::new(crate::RunJournal::open(directory.path().join("journal")).unwrap()),
            std::sync::Arc::new(crate::HarnessRegistry::new()));
        sessions.set_test_status("working", mast_proto::SessionStatus::Working);
        sessions.set_test_status("awaiting", mast_proto::SessionStatus::AwaitingInput);
        sessions.set_test_status_time("working", now - chrono::Duration::days(4));
        sessions.set_test_status_time("awaiting", now - chrono::Duration::days(4));
        sessions.set_test_status("recently-idle", mast_proto::SessionStatus::Idle);
        service.set_sessions(sessions.clone());
        let dispatch = sessions.lock_dispatch("eligible").await;
        assert_eq!(service.auto_archive_workers(now).unwrap(), 0);
        drop(dispatch);
        assert_eq!(service.auto_archive_workers(now).unwrap(), 1);
        assert!(workspace.chat("eligible").unwrap().unwrap().archived);
        assert!(!workspace.chat("recent").unwrap().unwrap().archived);
        assert!(!workspace.chat("request").unwrap().unwrap().archived);
        assert!(!workspace.chat("system-request").unwrap().unwrap().archived);
        assert!(!workspace.chat("working").unwrap().unwrap().archived);
        assert!(!workspace.chat("awaiting").unwrap().unwrap().archived);
        assert!(!workspace.chat("recently-idle").unwrap().unwrap().archived);
    }

    #[tokio::test]
    async fn reconcile_pass_archives_confirmed_descendant_chats_and_keeps_the_rest() {
        use crate::workspace_host::WorkspaceHostConfig;
        use mast_proto::{WorkItemStatus, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation};
        let directory = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(mast_sync::DocsStore::open(directory.path()).unwrap());
        let workspace = crate::WorkspaceHost::open(store, WorkspaceHostConfig {
            device_id: "device".into(), device_name: "test".into(), platform: "test".into(),
            org_id: "org".into(), user_id: "user".into(), edge: None,
        }).unwrap();
        workspace.create_space("space", "device", "/tmp/project", None, false).unwrap();
        let actor = WorkOutlineActor { kind: WorkOutlineActorKind::System, device_id: "device".into(), chat_id: None, message_id: None };
        let mutate = |m: WorkOutlineMutation| workspace.work_outline_mutate(&m, &actor, &[]).unwrap();
        let create = |name: &str, parent: Option<String>, status: WorkItemStatus| mutate(WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams {
            space_id: "space".into(), parent_work_item_id: parent, placement: Default::default(), title: name.into(),
            description: None, status: Some(status), operation_id: format!("item-{name}"),
        })).work_item_id.unwrap();
        let assign = |chat: &str, item: &str| mutate(WorkOutlineMutation::AssignChatToWorkItem(mast_proto::AssignChatToWorkItemParams {
            chat_id: chat.into(), work_item_id: item.into(), operation_id: format!("assign-{chat}"),
        }));
        let root = create("root", None, WorkItemStatus::Active);
        workspace.create_chat("orchestrator", Some("space"), Some("device"), None, None).unwrap();
        assign("orchestrator", &root);
        let now = Utc::now();
        for (name, status) in [("done", WorkItemStatus::Completed), ("open", WorkItemStatus::Active)] {
            let item = create(name, Some(root.clone()), status);
            // Pre-existing: no parent chat, only an assignment below the root.
            workspace.create_chat(name, Some("space"), Some("device"), None, None).unwrap();
            assign(name, &item);
            workspace.set_chat_activity(name, Some((now - chrono::Duration::days(30)).timestamp_millis()), Some((now - chrono::Duration::days(30)).timestamp_millis())).unwrap();
        }
        mutate(WorkOutlineMutation::SetWorkerAutomation(mast_proto::work_outline::SetWorkerAutomationParams {
            space_id: "space".into(), field: None, worker_worktrees: false, merged_pr_completion: None,
            archive_workers_after_days: Some(3), operation_id: "enable-archive".into(),
        }));
        let service = AgentToolsService::new(super::super::AgentTokens::new(), workspace.clone(),
            std::sync::Arc::new(crate::work_outline::WorkOutlineService::new(workspace.clone())), None);
        assert_eq!(service.auto_archive_workers(now).unwrap(), 1);
        assert!(workspace.chat("done").unwrap().unwrap().archived);
        assert!(!workspace.chat("open").unwrap().unwrap().archived);
        assert!(!workspace.chat("orchestrator").unwrap().unwrap().archived);
        assert_eq!(service.auto_archive_workers(now).unwrap(), 0);
    }

    #[test]
    fn archivable_pr_states_include_only_owned_links_and_preserve_unknown_states() {
        let links = [
            ("merged", ChatLinkSource::CreatedInChat),
            ("open", ChatLinkSource::Branch),
            ("pending", ChatLinkSource::DiffTarget),
            ("manual", ChatLinkSource::Manual),
            ("mentioned", ChatLinkSource::Mentioned),
            ("ticket", ChatLinkSource::Ticket),
            ("review", ChatLinkSource::ReviewLauncher),
        ]
        .map(|(url, source)| ChatPrLink {
            url: url.into(),
            source,
        });
        let states = owned_pr_states(&links, |url| match url {
            "merged" | "manual" | "mentioned" => Some("MERGED".into()),
            "open" => Some("OPEN".into()),
            _ => None,
        });
        assert_eq!(states, ["MERGED", "OPEN", "", "MERGED"]);
        assert!(owned_pr_states(&links[4..], |_| Some("MERGED".into())).is_empty());
        assert!(owned_pr_states(&[], |_| Some("MERGED".into())).is_empty());
    }
}
