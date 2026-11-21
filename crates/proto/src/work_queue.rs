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

/// Where a queued item stands: a projection of the item's `WorkStatus`.
/// Derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueEntryState {
    /// Nothing is working on it and nothing holds it back.
    Ready,
    /// A worker Chat is attached and the item is not waiting on anyone.
    Running,
    /// An open Required Action or a future `waitingUntil` holds it.
    Blocked,
    /// Completed, archived, or its pull request merged.
    Done,
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
}

/// Answers what is known about a pull request by URL. A lookup that has not
/// answered returns the default, whose `state_known` is false.
pub type PullRequestLookup<'a> = &'a dyn Fn(&str) -> PullRequestFacts;

fn queue_entry(
    snapshot: &WorkOutlineSnapshot,
    item: &WorkItemView,
    now_ms: i64,
    pull_requests: PullRequestLookup<'_>,
) -> QueueEntry {
    let completed = item.status == WorkItemStatus::Completed;
    let flags = ItemFlags {
        archived: item.archived_at.is_some(),
        user_action_open: item.rollup.unresolved_user_action_count > 0,
        waiting: item.waiting_until.is_some_and(|until| until > now_ms),
    };
    let chats = vec![ChatFacts::default(); item.chat_ids.len()];
    let prs: Vec<PullRequestFacts> = snapshot
        .github_pull_request_references(&item.id)
        .into_iter()
        .map(|reference| pull_requests(&reference.url))
        .collect();
    let status = derive_ticket_status(&chats, &prs, Some(completed), None, flags);
    // An open pull request with no chat attached stays `Ready`: only a merge
    // changes what the queue offers.
    let state = if status.stage == WorkStage::Done || completed {
        QueueEntryState::Done
    } else if status.hold.is_some() {
        QueueEntryState::Blocked
    } else if !chats.is_empty() {
        QueueEntryState::Running
    } else {
        QueueEntryState::Ready
    };
    QueueEntry {
        work_item_id: item.id.clone(),
        title: item.title.clone(),
        state,
        facts_pending: state == QueueEntryState::Ready && status.facts_pending,
    }
}

impl WorkOutlineSnapshot {
    /// The queue of `project_id`, in order, `Done` entries included.
    /// `pull_requests` supplies the facts of each linked pull request.
    pub fn work_queue(
        &self,
        project_id: &str,
        now_ms: i64,
        pull_requests: PullRequestLookup<'_>,
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
        queued
            .into_iter()
            .map(|item| queue_entry(self, item, now_ms, pull_requests))
            .collect()
    }

    /// The entry to offer the project's Manager: the first `Ready` one, and
    /// only while fewer than [`QUEUE_MAX_ACTIVE`] queued items are `Running`.
    /// A `Ready` entry whose pull request facts are pending is held, and so is
    /// everything behind it, until the facts arrive.
    pub fn next_queue_offer(
        &self,
        project_id: &str,
        now_ms: i64,
        pull_requests: PullRequestLookup<'_>,
    ) -> Option<QueueEntry> {
        let queue = self.work_queue(project_id, now_ms, pull_requests);
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
