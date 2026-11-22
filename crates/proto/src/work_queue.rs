//! Project work queues: the one derivation of a queue's entries and their
//! state, shared by the engine's Manager auto-start and the project page. A queue is not stored; it is the queued items
//! (`workItems.queueOrderKey`) of one project in `(key, id)` order.

use serde::{Deserialize, Serialize};

use crate::work_outline::{WorkItemStatus, WorkItemView, WorkOutlineSnapshot};
use crate::work_outline_order::valid_order_key;
use crate::work_status::{
    ChatFacts, ItemFlags, PullRequestFacts, WorkStage, derive_ticket_status,
};

/// Queue-started workers that may run at once in one project.
pub const QUEUE_MAX_ACTIVE: usize = 1;

/// How long after its last activity an unfinished worker still counts as
/// running. Workers routinely end a turn while a background build or test
/// suite runs, so a bare idle chat does not free the queue slot at once.
pub const QUEUE_ACTIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Where a queued item stands: a projection of the item's `WorkStatus`.
/// Derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueEntryState {
    /// Nothing is working on it and nothing holds it back.
    Ready,
    /// A worker is plausibly still working: a turn is in flight, or its last
    /// activity is within [`QUEUE_ACTIVE_WINDOW_MS`] and it did not error.
    Running,
    /// An open Required Action or a future `waitingUntil` holds it.
    Blocked,
    /// A worker is attached but nothing is working: it errored, or it has been
    /// idle past the window without reporting done. It is not offered again
    /// and does not use the queue's slot.
    Stalled,
    /// Completed, archived, a final handback, or its pull request merged.
    Done,
}

/// Why a [`QueueEntryState::Stalled`] entry is stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueStall {
    /// The last turn errored.
    Errored,
    /// The last error was a credits or usage limit.
    OutOfCredits,
    /// Idle past the window; `minutes` is how long, when known.
    Idle { minutes: Option<u32> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub work_item_id: String,
    pub title: String,
    pub state: QueueEntryState,
    /// A linked pull request has not been looked up yet, so `Ready` may be
    /// premature: the item could already be merged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub facts_pending: bool,
    /// Set exactly when `state` is `Stalled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<QueueStall>,
}

/// Answers what is known about a pull request by URL. A lookup that has not
/// answered returns the default, whose `state_known` is false.
pub type PullRequestLookup<'a> = &'a dyn Fn(&str) -> PullRequestFacts;

/// What a caller knows about one chat attached to a queued item. The default
/// is a chat that has done nothing: not a worker, not working, never active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatActivity {
    /// Started by an orchestrator as a worker. A planning or other chat does
    /// not keep an item running by sitting idle.
    pub worker: bool,
    pub archived: bool,
    /// A turn is in flight, or it is waiting for the user's answer.
    pub working: bool,
    /// The last turn errored.
    pub errored: bool,
    /// The last error was a credits or usage limit.
    pub out_of_credits: bool,
    /// Unix milliseconds of the last activity; 0 when unknown.
    pub last_active_ms: i64,
}

/// Answers what is known about a chat by id.
pub type ChatLookup<'a> = &'a dyn Fn(&str) -> ChatActivity;

/// Whether this chat keeps its item running at `now_ms`.
fn keeps_running(chat: &ChatActivity, now_ms: i64) -> bool {
    if chat.archived || chat.errored {
        return false;
    }
    chat.working
        || (chat.worker
            && chat.last_active_ms > 0
            && now_ms.saturating_sub(chat.last_active_ms) <= QUEUE_ACTIVE_WINDOW_MS)
}

/// The stall reason of an item none of whose chats keep it running: read from
/// the most recently active worker chat that is not archived.
fn stall_reason(chats: &[ChatActivity], now_ms: i64) -> QueueStall {
    let latest = chats
        .iter()
        .filter(|chat| chat.worker && !chat.archived)
        .max_by_key(|chat| chat.last_active_ms);
    match latest {
        Some(chat) if chat.errored && chat.out_of_credits => QueueStall::OutOfCredits,
        Some(chat) if chat.errored => QueueStall::Errored,
        Some(chat) if chat.last_active_ms > 0 => QueueStall::Idle {
            minutes: Some((now_ms.saturating_sub(chat.last_active_ms) / 60_000).max(0) as u32),
        },
        _ => QueueStall::Idle { minutes: None },
    }
}

fn queue_entry(
    snapshot: &WorkOutlineSnapshot,
    item: &WorkItemView,
    pull_request_urls: &[&str],
    now_ms: i64,
    pull_requests: PullRequestLookup<'_>,
    chat_lookup: ChatLookup<'_>,
) -> QueueEntry {
    let completed = item.status == WorkItemStatus::Completed;
    let flags = ItemFlags {
        archived: item.archived_at.is_some(),
        user_action_open: item.rollup.unresolved_user_action_count > 0,
        waiting: item.waiting_until.is_some_and(|until| until > now_ms),
    };
    // Chats of scheduled-task runs are not work on the item.
    let activity: Vec<ChatActivity> = snapshot
        .listed_chat_ids(item)
        .map(|id| chat_lookup(id))
        .collect();
    let chats = vec![ChatFacts::default(); activity.len()];
    // An archived or completed item is Done whatever its PRs say, so it never
    // asks the lookup, which may be fetching.
    let prs: Vec<PullRequestFacts> = if flags.archived || completed {
        Vec::new()
    } else {
        pull_request_urls.iter().map(|url| pull_requests(url)).collect()
    };
    let status = derive_ticket_status(&chats, &prs, Some(completed), None, flags);
    // A worker that recorded its final handback reported the item done; the
    // snapshot carries that fact, so every caller agrees on it.
    let reported_done = snapshot
        .listed_chat_ids(item)
        .any(|id| snapshot.handed_back_chat_ids.binary_search(id).is_ok());
    let running = activity.iter().any(|chat| keeps_running(chat, now_ms));
    // An open pull request with no chat attached stays `Ready`: only a merge
    // changes what the queue offers.
    let state = if status.stage == WorkStage::Done || completed || reported_done {
        QueueEntryState::Done
    } else if status.hold.is_some() {
        QueueEntryState::Blocked
    } else if running {
        QueueEntryState::Running
    } else if activity.iter().any(|chat| chat.worker && !chat.archived) {
        QueueEntryState::Stalled
    } else {
        QueueEntryState::Ready
    };
    QueueEntry {
        work_item_id: item.id.clone(),
        title: item.title.clone(),
        state,
        facts_pending: state == QueueEntryState::Ready && status.facts_pending,
        stall: (state == QueueEntryState::Stalled).then(|| stall_reason(&activity, now_ms)),
    }
}

impl WorkOutlineSnapshot {
    /// The queue of `project_id`, in order, `Done` entries included.
    /// `pull_requests` supplies the facts of each linked pull request and
    /// `chats` what each attached chat is doing.
    pub fn work_queue(
        &self,
        project_id: &str,
        now_ms: i64,
        pull_requests: PullRequestLookup<'_>,
        chats: ChatLookup<'_>,
    ) -> Vec<QueueEntry> {
        let mut queued: Vec<&WorkItemView> = self
            .items
            .iter()
            .filter(|item| {
                item.queue_order_key.is_some()
                    && self
                        .project_root(&item.id)
                        .is_some_and(|project| project.id == project_id && item.id != project_id)
            })
            .collect();
        // An invalid key sorts last, the same rule sibling order uses.
        queued.sort_by(|a, b| {
            fn rank(item: &WorkItemView) -> Option<&str> {
                item.queue_order_key.as_deref().filter(|key| valid_order_key(key))
            }
            (rank(a).is_none(), rank(a), &a.id).cmp(&(rank(b).is_none(), rank(b), &b.id))
        });
        // One pass over the references, not one per queued item.
        let mut urls: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for reference in &self.external_references {
            if reference.provider == crate::pr_review_sync::GITHUB_PROVIDER
                && reference.reference_type == crate::pr_review_sync::GITHUB_PULL_REQUEST_REFERENCE
            {
                urls.entry(reference.work_item_id.as_str())
                    .or_default()
                    .push(reference.url.as_str());
            }
        }
        queued
            .into_iter()
            .map(|item| {
                let urls = urls.get(item.id.as_str()).map_or(&[][..], Vec::as_slice);
                queue_entry(self, item, urls, now_ms, pull_requests, chats)
            })
            .collect()
    }

    /// The entry to offer the project's Manager: the first `Ready` one, and
    /// only while fewer than [`QUEUE_MAX_ACTIVE`] queued items are `Running`.
    /// `Stalled` entries neither count as running nor are offered again. A
    /// `Ready` entry whose pull request facts are pending is held, and so is
    /// everything behind it, until the facts arrive.
    pub fn next_queue_offer(
        &self,
        project_id: &str,
        now_ms: i64,
        pull_requests: PullRequestLookup<'_>,
        chats: ChatLookup<'_>,
    ) -> Option<QueueEntry> {
        let queue = self.work_queue(project_id, now_ms, pull_requests, chats);
        let running = queue
            .iter()
            .filter(|entry| entry.state == QueueEntryState::Running)
            .count();
        if running >= QUEUE_MAX_ACTIVE {
            return None;
        }
        queue
            .into_iter()
            .find(|entry| entry.state == QueueEntryState::Ready)
            .filter(|entry| !entry.facts_pending)
    }
}
