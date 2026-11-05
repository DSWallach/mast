//! When a worker chat is *finished*: safe to archive without losing anything
//! the user still needs. One pure rule, shared by the Team tab (rows and
//! "Archive finished"), `list_chats` / `get_chat`, and the `archive_chat`
//! agent tool, plus the project-scope test for which chats an orchestrator may
//! archive and a small in-memory log of agent archives.
//!
//! A chat is archivable when it is idle (not working, awaiting input, or with
//! a queued message), has no open Required Actions, and at least one of:
//! its item is completed; all its own linked PRs are merged or closed; it raised a
//! "Ready to merge" that was resolved; it has been idle for more than
//! [`IDLE_DAYS`] days with its item completed or archived.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Duration, Utc};
use mast_proto::{
    Chat, ChatIndicator, UserActionRequestView, WorkItemEffectiveStatus, WorkItemView,
    WorkOutlineSnapshot,
};
use serde::{Deserialize, Serialize};

/// Idle longer than this, with the item completed or archived, is finished.
pub const IDLE_DAYS: i64 = 3;

/// The verdict, shaped for JSON (`archivable` + `reason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archivable {
    pub archivable: bool,
    pub reason: String,
}

impl Archivable {
    fn yes(reason: impl Into<String>) -> Self {
        Self {
            archivable: true,
            reason: reason.into(),
        }
    }

    fn no(reason: impl Into<String>) -> Self {
        Self {
            archivable: false,
            reason: reason.into(),
        }
    }
}

/// Everything the rule reads about one chat.
pub struct ArchivableInput<'a> {
    pub chat: &'a Chat,
    pub indicator: ChatIndicator,
    /// The chat has a queued (not yet sent) message.
    pub queued: bool,
    /// The chat's work item, when it has one.
    pub item: Option<&'a WorkItemView>,
    /// Every Required Action the chat originated (open and resolved).
    pub requests: Vec<&'a UserActionRequestView>,
    /// States of the chat's own linked PRs ("MERGED", "CLOSED", "OPEN", ...).
    pub pr_states: Vec<String>,
    pub now: DateTime<Utc>,
}

/// Whether a durable PR link represents work owned by the chat.
pub fn owns_pr_link(link: &mast_proto::ChatPrLink) -> bool {
    matches!(
        link.source,
        mast_proto::ChatLinkSource::Manual
            | mast_proto::ChatLinkSource::CreatedInChat
            | mast_proto::ChatLinkSource::Branch
            | mast_proto::ChatLinkSource::DiffTarget
    )
}

/// Open handoffs that explicitly identify a branch in a diff link or Branch fact.
pub fn open_handoffs_for_branch<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    branch: &str,
) -> Vec<&'a UserActionRequestView> {
    open_handoffs_for_pr(snapshot, branch, "")
}

/// Open handoffs that explicitly identify this PR or its head branch.
pub fn open_handoffs_for_pr<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    branch: &str,
    pr_url: &str,
) -> Vec<&'a UserActionRequestView> {
    use mast_proto::work_outline_user_actions::{RequestLinkTarget, UserActionKind};
    let canonical = crate::chat_links::canonicalize_pr_url(pr_url);
    let external_id = canonical
        .strip_prefix("https://github.com/")
        .and_then(|path| path.split_once("/pull/"))
        .map(|(repo, number)| format!("{repo}#{number}"));
    snapshot.user_action_requests.iter().filter(|request| {
        let branch_matches = !branch.is_empty() && (request.links.iter().any(|link| {
            matches!(&link.target, RequestLinkTarget::Diff { branch: Some(value) } if value == branch)
        }) || request.facts.iter().any(|fact| {
            fact.label.eq_ignore_ascii_case("branch") && fact.value.trim() == branch
        }));
        let pr_matches = !canonical.is_empty() && (request.links.iter().any(|link| {
            matches!(&link.target, RequestLinkTarget::Pr { url } if crate::chat_links::canonicalize_pr_url(url) == canonical)
        }) || request.facts.iter().any(|fact| {
            fact.label.eq_ignore_ascii_case("pr") && (crate::chat_links::canonicalize_pr_url(&fact.value) == canonical
                || external_id.as_ref().is_some_and(|id| fact.value.trim().eq_ignore_ascii_case(id)))
        }));
        request.is_unresolved() && request.effective_kind() == UserActionKind::Handoff
            && (branch_matches || pr_matches)
    }).collect()
}

/// The requests a chat originated, from the outline snapshot.
pub fn requests_of<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    chat_id: &str,
) -> Vec<&'a UserActionRequestView> {
    snapshot
        .user_action_requests
        .iter()
        .filter(|r| r.chat_id == chat_id && !r.deduped)
        .collect()
}

/// The rule. The first failing gate names the reason; otherwise the first
/// matching finish condition does.
pub fn archivable(input: &ArchivableInput<'_>) -> Archivable {
    if input.chat.archived {
        return Archivable::no("already archived");
    }
    match input.indicator {
        ChatIndicator::Working => return Archivable::no("still working"),
        ChatIndicator::AwaitingInput => return Archivable::no("waiting for input"),
        _ => {}
    }
    if input.queued {
        return Archivable::no("has a queued message");
    }
    if input.requests.iter().any(|r| r.is_open()) {
        return Archivable::no("has an open Required Action");
    }
    let item_completed = input
        .item
        .is_some_and(|i| matches!(i.effective_status, WorkItemEffectiveStatus::Completed));
    if item_completed {
        return Archivable::yes("its item is completed");
    }
    let closed = |s: &String| matches!(s.to_ascii_uppercase().as_str(), "MERGED" | "CLOSED");
    if !input.pr_states.is_empty() && input.pr_states.iter().all(closed) {
        let merged = input
            .pr_states
            .iter()
            .all(|s| s.eq_ignore_ascii_case("MERGED"));
        return Archivable::yes(if merged {
            "its PR is merged"
        } else {
            "its PR is merged or closed"
        });
    }
    if input.requests.iter().any(|r| {
        !r.is_open()
            && r.effective_kind() == mast_proto::work_outline_user_actions::UserActionKind::Handoff
    }) {
        return Archivable::yes("its Ready to merge was resolved");
    }
    let item_archived = input.item.is_some_and(|i| i.archived_at.is_some());
    if item_completed || item_archived {
        let last = input.chat.last_message_at.unwrap_or(input.chat.created_at);
        if input.now - last > Duration::days(IDLE_DAYS) {
            return Archivable::yes(format!("idle for more than {IDLE_DAYS} days"));
        }
    }
    Archivable::no("not finished: item open, no merged PR, no resolved Ready to merge")
}

/// The chats an orchestrator may archive: chats it created (`parent_chat_id`)
/// and chats on items inside its project's worker destination. Never the
/// orchestrator itself.
pub fn worker_scope(
    snapshot: &WorkOutlineSnapshot,
    chats: &[Chat],
    orchestrator: &str,
    orchestrator_item: Option<&str>,
) -> HashSet<String> {
    let mut scope: HashSet<String> = chats
        .iter()
        .filter(|c| c.parent_chat_id.as_deref() == Some(orchestrator))
        .map(|c| c.id.clone())
        .collect();
    if let Some(item) = orchestrator_item
        && let Some(project) = snapshot.project_root(item)
        && let Some(destination) = project.worker_destination_id.as_deref()
    {
        scope.extend(
            snapshot
                .assignments
                .iter()
                .filter(|a| {
                    a.work_item_id
                        .as_deref()
                        .is_some_and(|i| snapshot.is_within(destination, i))
                })
                .map(|a| a.chat_id.clone()),
        );
    }
    scope.remove(orchestrator);
    scope
}

/// One agent archive, for the outline's Recent changes and undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentArchive {
    pub space_id: String,
    pub chat_id: String,
    pub chat_title: Option<String>,
    pub orchestrator_chat_id: String,
    pub orchestrator_title: Option<String>,
    pub reason: String,
    /// Archived with `force: true` although the rule said no.
    pub forced: bool,
    pub at_ms: i64,
    /// Monotonic per process, so a poller can ask for "after N".
    pub seq: u64,
}

const LOG_CAP: usize = 200;

/// In-memory log of agent archives (not persisted).
#[derive(Default)]
pub struct AgentArchiveLog {
    entries: Mutex<(u64, Vec<AgentArchive>)>,
}

impl AgentArchiveLog {
    fn guard(&self) -> MutexGuard<'_, (u64, Vec<AgentArchive>)> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn record(&self, mut entry: AgentArchive) {
        let mut guard = self.guard();
        guard.0 += 1;
        entry.seq = guard.0;
        guard.1.push(entry);
        if guard.1.len() > LOG_CAP {
            guard.1.remove(0);
        }
    }

    /// Entries with `seq > after`, oldest first.
    pub fn since(&self, after: u64) -> Vec<AgentArchive> {
        self.guard()
            .1
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests;
