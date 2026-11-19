//! Keeps a project's Manager status record current. A manager turn that
//! changed state must end with a new revision of the record; when it does
//! not, the engine asks once.
//!
//! The revision is read at the start of each turn and the check compares
//! against that number, so a checkpoint published earlier in the same chat can
//! never satisfy a later turn. The ask itself is a turn, and is checked
//! against the revision of the turn it is asking about.

use std::collections::HashMap;
use std::sync::Mutex;

use std::sync::Arc;

use mast_proto::artifacts::ArtifactView;
use mast_proto::{
    MANAGER_STATUS_ROLE, SessionStatus, StandingRoleKind, WorkOutlineActorKind,
    manager_status_nudge_text,
};

use crate::agent_tools::AgentTokens;
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
    /// Nothing owed: the turn changed no state, or it was not a turn we saw start.
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

#[derive(Default)]
pub struct StatusTracker {
    chats: Mutex<HashMap<String, Entry>>,
}

impl StatusTracker {
    /// A turn began on a manager chat; `revision` is the record's current revision.
    pub fn turn_started(&self, chat_id: &str, revision: u32) {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let entry = chats.entry(chat_id.to_owned()).or_default();
        if entry.in_turn {
            return;
        }
        entry.in_turn = true;
        match entry.asked {
            Some(baseline) => {
                entry.baseline = baseline;
                entry.owed = true;
            }
            None => {
                entry.baseline = revision;
                entry.owed = false;
            }
        }
    }

    /// The chat called a state-changing tool. Ignored outside a tracked turn.
    pub fn note_state_change(&self, chat_id: &str) {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = chats.get_mut(chat_id).filter(|e| e.in_turn) {
            entry.owed = true;
        }
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

    /// The ask could not be queued; allow the next state-changing turn to ask again.
    pub fn ask_failed(&self, chat_id: &str) {
        if let Some(entry) = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get_mut(chat_id) {
            entry.asked = None;
        }
    }
}

/// The status artifact of a project item, if it has one.
pub fn status_artifact(workspace: &WorkspaceHost, space_id: &str, item_id: &str) -> Option<ArtifactView> {
    workspace
        .artifacts_of_space(space_id)
        .into_iter()
        .filter(|a| a.archived_at.is_none() && a.work_item_id.as_deref() == Some(item_id))
        .find(|a| a.role.as_deref().is_some_and(mast_proto::is_manager_status_role))
}

/// Whether an agent published a revision of `artifact` newer than `baseline`.
pub fn published_after(artifact: Option<&ArtifactView>, baseline: u32) -> bool {
    artifact.is_some_and(|a| {
        a.revisions
            .iter()
            .any(|r| r.id > baseline && r.by.kind == WorkOutlineActorKind::Agent)
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

/// Watches session status and runs the check on manager chats.
pub fn spawn_watch(
    tokens: Arc<AgentTokens>,
    workspace: WorkspaceHost,
    doc_host: DocHost,
    sessions: SessionsEngine,
) {
    let mut rx = sessions.watch_sessions();
    tokio::spawn(async move {
        let tracker = tokens.manager_status();
        let mut last: HashMap<String, bool> = HashMap::new();
        loop {
            let snapshot = rx.borrow_and_update().clone();
            for session in &snapshot {
                let working = matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                );
                let was = last.insert(session.chat_id.clone(), working).unwrap_or(false);
                if working == was {
                    continue;
                }
                let Some((space, item)) = managed_project(&workspace, &session.chat_id) else {
                    continue;
                };
                let artifact = status_artifact(&workspace, &space, &item);
                if working {
                    tracker.turn_started(&session.chat_id, artifact.as_ref().map_or(0, |a| a.current_revision));
                    continue;
                }
                match tracker.turn_finished(&session.chat_id, |b| published_after(artifact.as_ref(), b)) {
                    Verdict::Nudge { baseline } => {
                        let id = crate::new_id();
                        if let Err(error) =
                            doc_host.queue_context_message(&session.chat_id, &id, &manager_status_nudge_text(baseline))
                        {
                            tracing::warn!(chat = %session.chat_id, %error, "manager status nudge not queued");
                            tracker.ask_failed(&session.chat_id);
                        }
                    }
                    Verdict::Missed { baseline } => {
                        tracing::warn!(chat = %session.chat_id, baseline, role = MANAGER_STATUS_ROLE,
                            "manager status still missing after one ask");
                    }
                    Verdict::NotOwed | Verdict::Published => {}
                }
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_that_changed_state_without_a_revision_is_asked_once() {
        let t = StatusTracker::default();
        t.turn_started("m", 2);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |b| b > 2), Verdict::Nudge { baseline: 2 });
        // The ask is a turn: it is held to the same baseline, and not asked twice.
        t.turn_started("m", 2);
        assert_eq!(t.turn_finished("m", |b| b > 2), Verdict::Missed { baseline: 2 });
        // Afterwards a fresh state-changing turn can be asked again.
        t.turn_started("m", 2);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_revision_published_in_the_turn_satisfies_it() {
        let t = StatusTracker::default();
        t.turn_started("m", 1);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |b| b == 1), Verdict::Published);
    }

    #[test]
    fn an_earlier_checkpoint_cannot_satisfy_a_later_turn() {
        let t = StatusTracker::default();
        // Turn one checkpoints: the record goes 1 -> 2.
        t.turn_started("m", 1);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |b| b < 2), Verdict::Published);
        // Turn two starts at revision 2, changes state, publishes nothing.
        t.turn_started("m", 2);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |b| b < 2), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_turn_without_state_change_owes_nothing() {
        let t = StatusTracker::default();
        t.turn_started("m", 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::NotOwed);
        t.note_state_change("m"); // outside a turn: ignored
        t.turn_started("m", 0);
        assert_eq!(t.turn_finished("m", |_| false), Verdict::NotOwed);
    }

    #[test]
    fn a_failed_ask_lets_the_next_turn_ask_again() {
        let t = StatusTracker::default();
        t.turn_started("m", 0);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 0 });
        t.ask_failed("m");
        t.turn_started("m", 0);
        t.note_state_change("m");
        assert_eq!(t.turn_finished("m", |_| false), Verdict::Nudge { baseline: 0 });
    }

    #[test]
    fn only_agent_revisions_after_the_baseline_count() {
        use mast_proto::artifacts::ArtifactRevision;
        let mut a = ArtifactView::default();
        let rev = |id, kind| ArtifactRevision {
            id,
            by: mast_proto::WorkOutlineActor { kind, ..Default::default() },
            ..Default::default()
        };
        a.revisions = vec![rev(1, WorkOutlineActorKind::Agent), rev(2, WorkOutlineActorKind::User)];
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
