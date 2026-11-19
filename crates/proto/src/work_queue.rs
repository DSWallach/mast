//! Project work queues: the one derivation of a queue's entries and their
//! state, shared by the engine's Manager auto-start, the agent tools and the
//! project page. A queue is not stored; it is the queued items
//! (`workItems.queueOrderKey`) of one project in `(key, id)` order.

use serde::{Deserialize, Serialize};

use crate::work_outline::{WorkItemStatus, WorkItemView, WorkOutlineSnapshot};
use crate::work_outline_order::valid_order_key;

/// Queue-started workers that may run at once in one project.
pub const QUEUE_MAX_ACTIVE: usize = 1;

/// Where a queued item stands. Derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueEntryState {
    /// Nothing is working on it and nothing holds it back.
    Ready,
    /// A worker Chat is attached and the item is not waiting on anyone.
    Running,
    /// An open Required Action or a future `waitingUntil` holds it.
    Blocked,
    /// Completed or archived.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub work_item_id: String,
    pub title: String,
    pub state: QueueEntryState,
}

fn entry_state(item: &WorkItemView, now_ms: i64) -> QueueEntryState {
    if item.archived_at.is_some() || item.status == WorkItemStatus::Completed {
        QueueEntryState::Done
    } else if item.rollup.unresolved_user_action_count > 0
        || item.waiting_until.is_some_and(|until| until > now_ms)
    {
        QueueEntryState::Blocked
    } else if !item.chat_ids.is_empty() {
        QueueEntryState::Running
    } else {
        QueueEntryState::Ready
    }
}

impl WorkOutlineSnapshot {
    /// The queue of `project_id`, in order, `Done` entries included.
    pub fn work_queue(&self, project_id: &str, now_ms: i64) -> Vec<QueueEntry> {
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
            .map(|item| QueueEntry {
                work_item_id: item.id.clone(),
                title: item.title.clone(),
                state: entry_state(item, now_ms),
            })
            .collect()
    }

    /// The entry to offer the project's Manager: the first `Ready` one, and
    /// only while fewer than [`QUEUE_MAX_ACTIVE`] queued items are `Running`.
    pub fn next_queue_offer(&self, project_id: &str, now_ms: i64) -> Option<QueueEntry> {
        let queue = self.work_queue(project_id, now_ms);
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
    }
}
