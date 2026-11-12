//! `chat_archive_info` / `chat_archive`: the engine side of the MCP
//! `archive_chat`, `list_chats` and `get_chat` "finished worker" support.

use std::collections::HashSet;

use chrono::Utc;
use mast_proto::{Chat, ChatIndicator, ChatPrLink, WorkOutlineSnapshot, chat_indicator};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AgentToolError, AgentToolErrorCode, AgentToolsService, CallContext, lock, parse_args};
use crate::archivable::{
    AgentArchive, Archivable, ArchivableInput, archivable, owns_pr_link, requests_of, worker_scope,
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
        let pr_states = self.pr_states(chat);
        archivable(&ArchivableInput {
            chat,
            indicator,
            queued: self
                .doc_host
                .as_ref()
                .is_some_and(|d| d.queued_count(&chat.id) > 0),
            item,
            requests: requests_of(snapshot, &chat.id),
            pr_states,
            now: Utc::now(),
        })
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
