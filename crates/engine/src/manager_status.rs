//! Keeps a project's Manager status record current. A manager turn that
//! changed state must end with a new revision of the record; when it does
//! not, the engine asks once.
//!
//! The revision is read at the start of each turn and the check compares
//! against that number, so a checkpoint published earlier in the same chat can
//! never satisfy a later turn. The ask itself is a turn, and is checked
//! against the revision of the turn it is asking about.
//!
//! A turn is tracked from whichever comes first: the watcher seeing the run
//! in flight, or the run's first state-changing tool call. It ends when the
//! live session is no longer in flight, whatever status snapshot woke the
//! watcher, so a turn too fast for the watcher to see still gets checked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mast_proto::artifacts::ArtifactView;
use mast_proto::{
    MANAGER_STATUS_ROLE, StandingRoleKind, manager_status_nudge_text,
};
use tokio_util::sync::CancellationToken;

use crate::agent_tools::AgentTokens;
use crate::briefing::{BriefingKind, manager_provenance};
use crate::sessions::SessionsEngine;
use crate::{DocHost, WorkspaceHost};

/// Agent tools that change project state. A manager turn that calls one owes
/// a status revision. Creating a child chat is how a worker is dispatched.
pub const STATE_CHANGING_TOOLS: [&str; 12] = [
    mast_proto::agent_tools::tools::CREATE_CHILD,
    mast_proto::agent_tools::tools::SET_STATUS,
    mast_proto::agent_tools::tools::CREATE_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::RESOLVE_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::ANSWER_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::APPLY_CONTAINER_TEMPLATE,
    mast_proto::agent_tools::tools::ADMIN,
    mast_proto::agent_tools::tools::QUEUE_CHILD_CHAT_TURN,
    mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT,
    mast_proto::agent_tools::tools::CLAIM_CHILD_CHAT,
    mast_proto::agent_tools::tools::CHAT_ARCHIVE,
    mast_proto::agent_tools::tools::PROPOSE_INSTRUCTION,
];

pub fn is_state_changing(tool: &str) -> bool {
    STATE_CHANGING_TOOLS.contains(&tool)
}

/// What the check decided at the end of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing owed: the turn changed no state, or it was not tracked.
    NotOwed,
    /// The turn published a revision after its baseline.
    Published,
    /// State changed and no revision followed: ask once.
    Nudge { baseline: u32 },
    /// The ask itself ended without a revision. Not asked again.
    Missed { baseline: u32 },
}

#[derive(Default)]
struct Entry {
    in_turn: bool,
    baseline: u32,
    owed: bool,
    /// Baseline of an ask that was queued and has not run to its end yet.
    asked: Option<u32>,
}

impl Entry {
    fn open(&mut self, revision: u32) {
        self.in_turn = true;
        match self.asked {
            Some(baseline) => {
                self.baseline = baseline;
                self.owed = true;
            }
            None => {
                self.baseline = revision;
                self.owed = false;
            }
        }
    }
}

#[derive(Default)]
pub struct StatusTracker {
    chats: Mutex<HashMap<String, Entry>>,
}

impl StatusTracker {
    /// A turn began on a manager chat; `revision` is the record's current
    /// revision. A turn already opened by a tool call keeps its baseline.
    pub fn turn_started(&self, chat_id: &str, revision: u32) {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let entry = chats.entry(chat_id.to_owned()).or_default();
        if !entry.in_turn {
            entry.open(revision);
        }
    }

    /// Whether a tracked turn is in flight on the chat.
    pub fn turn_open(&self, chat_id: &str) -> bool {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(chat_id)
            .is_some_and(|e| e.in_turn)
    }

    /// The chat called a state-changing tool during a run. Opens the turn when
    /// the watcher has not yet seen it start; `revision` is read only then.
    pub fn note_state_change(&self, chat_id: &str, revision: impl FnOnce() -> u32) {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let entry = chats.entry(chat_id.to_owned()).or_default();
        if !entry.in_turn {
            entry.open(revision());
        }
        entry.owed = true;
    }

    /// The turn ended. `published_after` tells whether the manager published a
    /// revision newer than the given baseline.
    pub fn turn_finished(&self, chat_id: &str, published_after: impl FnOnce(u32) -> bool) -> Verdict {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = chats.get_mut(chat_id).filter(|e| e.in_turn) else {
            return Verdict::NotOwed;
        };
        entry.in_turn = false;
        let baseline = entry.baseline;
        let was_ask = entry.asked.is_some();
        if !entry.owed {
            return Verdict::NotOwed;
        }
        entry.owed = false;
        if published_after(baseline) {
            entry.asked = None;
            return Verdict::Published;
        }
        if was_ask {
            entry.asked = None;
            Verdict::Missed { baseline }
        } else {
            entry.asked = Some(baseline);
            Verdict::Nudge { baseline }
        }
    }

    /// Drop a turn that belongs to a chat that is not a manager.
    pub fn forget(&self, chat_id: &str) {
        self.chats.lock().unwrap_or_else(|e| e.into_inner()).remove(chat_id);
    }

    /// The ask could not be queued; allow the next state-changing turn to ask again.
    pub fn ask_failed(&self, chat_id: &str) {
        if let Some(entry) = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get_mut(chat_id) {
            entry.asked = None;
        }
    }

    fn open_chats(&self) -> Vec<String> {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, e)| e.in_turn)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// The status artifact of a project item that readers would serve: the newest
/// one on the item with a revision stamped by a Manager. The turn check and
/// `briefing_read` both go through this stamp, so they agree on what counts.
pub fn status_artifact(workspace: &WorkspaceHost, space_id: &str, item_id: &str) -> Option<ArtifactView> {
    workspace
        .artifacts_of_space(space_id)
        .into_iter()
        .filter(|a| a.archived_at.is_none() && a.work_item_id.as_deref() == Some(item_id))
        .filter(|a| BriefingKind::ManagerStatus.newest_trusted(workspace, a).is_some())
        .max_by_key(|a| a.current().map_or(0, |r| r.at))
}

/// The revision number a turn on this project starts from.
pub fn status_revision(workspace: &WorkspaceHost, space_id: &str, item_id: &str) -> u32 {
    status_artifact(workspace, space_id, item_id).map_or(0, |a| a.current_revision)
}

/// Whether a Manager-stamped revision newer than `baseline` exists.
pub fn published_after(artifact: Option<&ArtifactView>, baseline: u32) -> bool {
    artifact.is_some_and(|a| {
        a.revisions
            .iter()
            .any(|r| r.id > baseline && manager_provenance(a, r))
    })
}

/// The project a chat manages, as `(space id, project item id)`.
pub fn managed_project(workspace: &WorkspaceHost, chat_id: &str) -> Option<(String, String)> {
    let role = workspace.standing_role_of_chat(chat_id)?;
    if role.role != StandingRoleKind::Manager {
        return None;
    }
    let space = workspace.work_item_space(&role.scope_item_id)?;
    Some((space, role.scope_item_id))
}

/// The running watcher, owned by the engine so shutdown can stop it before it
/// queues anything else.
pub struct ManagerStatusWatch {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl ManagerStatusWatch {
    /// Cancel the watcher and wait for it to finish.
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

/// Watches live session state and runs the check on manager chats. Idle
/// snapshots only wake it: whether a turn is in flight is read live.
pub fn spawn_watch(
    tokens: Arc<AgentTokens>,
    workspace: WorkspaceHost,
    doc_host: DocHost,
    sessions: SessionsEngine,
) -> ManagerStatusWatch {
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let mut rx = sessions.watch_sessions();
    let handle = tokio::spawn(async move {
        let tracker = tokens.manager_status();
        loop {
            let snapshot = rx.borrow_and_update().clone();
            let mut chats: Vec<String> = tracker.open_chats();
            chats.extend(
                snapshot
                    .iter()
                    .filter(|s| sessions.turn_in_flight(&s.chat_id))
                    .map(|s| s.chat_id.clone()),
            );
            chats.sort();
            chats.dedup();
            for chat_id in chats {
                if stop.is_cancelled() {
                    return;
                }
                let live = sessions.turn_in_flight(&chat_id);
                let open = tracker.turn_open(&chat_id);
                if live == open {
                    continue;
                }
                let Some((space, item)) = managed_project(&workspace, &chat_id) else {
                    if open && !live {
                        tracker.forget(&chat_id);
                    }
                    continue;
                };
                if live {
                    tracker.turn_started(&chat_id, status_revision(&workspace, &space, &item));
                    continue;
                }
                let artifact = status_artifact(&workspace, &space, &item);
                match tracker.turn_finished(&chat_id, |b| published_after(artifact.as_ref(), b)) {
                    Verdict::Nudge { baseline } => {
                        if stop.is_cancelled() {
                            return;
                        }
                        let id = crate::new_id();
                        if let Err(error) =
                            doc_host.queue_context_message(&chat_id, &id, &manager_status_nudge_text(baseline))
                        {
                            tracing::warn!(chat = %chat_id, %error, "manager status nudge not queued");
                            tracker.ask_failed(&chat_id);
                        }
                    }
                    Verdict::Missed { baseline } => {
                        tracing::warn!(chat = %chat_id, baseline, role = MANAGER_STATUS_ROLE,
                            "manager status still missing after one ask");
                    }
                    Verdict::NotOwed | Verdict::Published => {}
                }
            }
            tokio::select! {
                _ = stop.cancelled() => return,
                changed = rx.changed() => if changed.is_err() { return },
            }
        }
    });
    ManagerStatusWatch { cancel, handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_that_changed_state_without_a_revision_is_asked_once() {
        let t = StatusTracker::default();
        t.turn_started("m", 2);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |b| b > 2), Verdict::Nudge { baseline: 2 });
        // The ask is a turn: it is held to the same baseline, and not asked twice.
        t.turn_started("m", 2);
        assert_eq!(t.turn_finished("m", |b| b > 2), Verdict::Missed { baseline: 2 });
        // Afterwards a fresh state-changing turn can be asked again.
        t.turn_started("m", 2);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_revision_published_in_the_turn_satisfies_it() {
        let t = StatusTracker::default();
        t.turn_started("m", 1);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |b| b == 1), Verdict::Published);
    }

    #[test]
    fn an_earlier_checkpoint_cannot_satisfy_a_later_turn() {
        let t = StatusTracker::default();
        // Turn one checkpoints: the record goes 1 -> 2.
        t.turn_started("m", 1);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |b| b < 2), Verdict::Published);
        // Turn two starts at revision 2, changes state, publishes nothing.
        t.turn_started("m", 2);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |b| b < 2), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_turn_without_state_change_owes_nothing() {
        let t = StatusTracker::default();
        t.turn_started("m", 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::NotOwed);
        t.turn_started("m", 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::NotOwed);
    }

    #[test]
    fn a_turn_too_fast_for_the_watcher_is_opened_by_its_tool_call() {
        let t = StatusTracker::default();
        // No turn_started: the first state-changing call opens the turn, with
        // the revision read at that moment.
        t.note_state_change("m", || 4);
        assert!(t.turn_open("m"));
        // A later call in the same turn does not move the baseline.
        t.note_state_change("m", || 9);
        assert_eq!(t.turn_finished("m", |b| b > 4), Verdict::Nudge { baseline: 4 });
        // The watcher arriving late keeps the baseline the call recorded.
        t.note_state_change("n", || 7);
        t.turn_started("n", 8);
        assert_eq!(t.turn_finished("n", |b| b == 7), Verdict::Published);
    }

    #[test]
    fn a_turn_on_a_chat_that_is_not_a_manager_can_be_dropped() {
        let t = StatusTracker::default();
        t.note_state_change("worker", || 0);
        t.forget("worker");
        assert!(!t.turn_open("worker"));
        assert_eq!(t.turn_finished("worker", |_| false), Verdict::NotOwed);
    }

    #[test]
    fn a_failed_ask_lets_the_next_turn_ask_again() {
        let t = StatusTracker::default();
        t.turn_started("m", 0);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 0 });
        t.ask_failed("m");
        t.turn_started("m", 0);
        t.note_state_change("m", || 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 0 });
    }

    #[test]
    fn only_manager_stamped_revisions_after_the_baseline_count() {
        use mast_proto::artifacts::ArtifactRevision;
        use mast_proto::{ManagerStatusStamp, WorkOutlineActor, WorkOutlineActorKind};
        let mut a = ArtifactView { work_item_id: Some("proj".into()), ..Default::default() };
        let stamp = Some(ManagerStatusStamp {
            role_id: mast_proto::standing_role_id(StandingRoleKind::Manager, "proj"),
            chat_id: "c".into(),
        });
        let rev = |id, kind, manager_status| ArtifactRevision {
            id,
            by: WorkOutlineActor { kind, ..Default::default() },
            manager_status,
            ..Default::default()
        };
        a.revisions = vec![
            rev(3, WorkOutlineActorKind::Agent, None),
            rev(2, WorkOutlineActorKind::User, stamp.clone()),
            rev(1, WorkOutlineActorKind::Agent, stamp),
        ];
        // An unstamped agent revision, or a stamp on a user's, is not a publication.
        assert!(!published_after(Some(&a), 1));
        assert!(published_after(Some(&a), 0));
        assert!(!published_after(None, 0));
    }

    #[test]
    fn dispatching_a_worker_is_state_changing() {
        assert!(is_state_changing(mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::READ_SUBTREE));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::BRIEFING_READ));
    }
}
