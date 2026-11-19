//! Keeps a project's Manager status record current. A manager turn that
//! changed state must end with a new revision of the record; when it does
//! not, the engine asks once.
//!
//! A turn is what the session calls one: it begins when the session turns active
//! (a run's first prompt, or a prompt steered into a parked warm run, which keeps
//! one agent token across many turns) and ends when it settles. The record's
//! revision is read at both ends, so each turn is checked against exactly the
//! revisions that existed while it ran. A checkpoint from an earlier turn, or a
//! later turn's publication, can never satisfy it, however closely the turns
//! follow each other. The ask itself is a turn, checked against the revision of
//! the turn it asks about.
//!
//! A Manager's chat holds the role only after its first turn is queued: a first
//! start carries a provisional marker, and a hand-off's successor is named in
//! the role's reserved hand-off. Either way the project is known before the
//! role is. A turn that ends while its chat holds nothing cannot have published,
//! so it is marked owed when it ends, and such turns of one chat merge into one
//! obligation right then, whenever the watcher gets to them. They are kept for as
//! long as the marker or reservation stays valid, since nothing expires one, and
//! checked once the chat holds the role. They are dropped, with nothing sent,
//! when the reservation is replaced or cleared, the role moves to another chat,
//! or the chat is gone or archived. Only the chat that holds the role is ever
//! reminded. The watcher re-checks on chat changes and on a slow timer, never in
//! a tight loop.
//!
//! Locks: the grants map, the tracker's state and the probe are never held
//! while the probe reads the workspace. Turn boundaries are reported by the
//! session's status funnel and by token drops, neither of which holds a
//! workspace lock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mast_proto::artifacts::ArtifactView;
use mast_proto::{
    MANAGER_STATUS_ROLE, StandingRoleKind, manager_status_nudge_text,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::agent_tools::AgentTokens;
use crate::briefing::{BriefingKind, manager_provenance};
use crate::workspace_host::ManagerClaim;
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
    /// The turn published a revision while it ran.
    Published,
    /// State changed and no revision followed: ask once.
    Nudge { baseline: u32 },
    /// The ask itself ended without a revision. Not asked again.
    Missed { baseline: u32 },
}

/// Where a turn belongs and how far the record has got when it is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunBasis {
    /// `(space id, project item id)`, when the chat manages a project or is
    /// the first start or successor that will.
    pub project: Option<(String, String)>,
    pub revision: u32,
    /// Whether the chat holds the role right now. Read with the project from
    /// one registry snapshot, so a turn that ends before adoption is marked
    /// when it ends, however late anything looks at it.
    pub holds: bool,
}

pub type RunProbe = Arc<dyn Fn(&str) -> RunBasis + Send + Sync>;

/// How often held turns are re-checked when no chat change wakes the watcher.
/// Adoption normally follows within moments and wakes it through the chat
/// change; this is only the net under changes that touch the role alone.
const DEFAULT_RECHECK: Duration = Duration::from_secs(30);

/// One turn in flight: from the session turning active to it settling.
struct Turn {
    /// The agent token's run that made the turn's state-changing calls, so
    /// that run ending closes the turn. A warm session keeps one token across
    /// many turns; the turn boundaries are the session's, not the token's.
    run_id: Option<String>,
    baseline: u32,
    project: (String, String),
    owed: bool,
    ask: bool,
}

/// A turn that ended owing a status revision, waiting to be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedRun {
    pub chat_id: String,
    pub baseline: u32,
    pub end_revision: u32,
    pub project: (String, String),
    pub ask: bool,
    /// The chat did not hold the role when the turn ended, so the turn cannot
    /// have published and is owed whatever the revisions say. Such turns of one
    /// chat share one obligation, merged when they end.
    forced: bool,
}

#[derive(Default)]
struct State {
    turns: HashMap<String, Turn>,
    /// Baseline of an ask that was queued and has not run to its end yet.
    asked: HashMap<String, u32>,
    ended: Vec<EndedRun>,
}

#[derive(Default)]
pub struct StatusTracker {
    state: Mutex<State>,
    probe: Mutex<Option<RunProbe>>,
    wake: Arc<Notify>,
    recheck: Mutex<Option<Duration>>,
}

impl StatusTracker {
    /// How turns are placed in a project and how far the record has got. Until
    /// it is set nothing is tracked.
    pub fn set_probe(&self, probe: RunProbe) {
        *self.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some(probe);
    }

    /// Override how often held turns are re-checked on the timer (tests).
    pub fn set_recheck_interval(&self, interval: Duration) {
        *self.recheck.lock().unwrap_or_else(|e| e.into_inner()) = Some(interval);
    }

    fn recheck_interval(&self) -> Duration {
        self.recheck
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(DEFAULT_RECHECK)
    }

    fn basis(&self, chat_id: &str) -> Option<RunBasis> {
        let probe = self.probe.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        Some(probe(chat_id))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A turn began: the session turned active. Its baseline is the record's
    /// revision right now, or the original baseline when this turn is the ask.
    /// A turn still open (its end was never reported) is closed first.
    pub fn begin(&self, chat_id: &str) {
        let basis = self.basis(chat_id);
        let mut state = self.state();
        if state.turns.contains_key(chat_id) {
            Self::finish(&mut state, chat_id, basis.as_ref());
        }
        let Some(RunBasis { project: Some(project), revision, .. }) = basis else {
            return;
        };
        let (baseline, owed, ask) = match state.asked.get(chat_id) {
            Some(&asked) => (asked, true, true),
            None => (revision, false, false),
        };
        state.turns.insert(
            chat_id.to_owned(),
            Turn { run_id: None, baseline, project, owed, ask },
        );
    }

    /// The run called a state-changing tool, so the turn it is in owes a
    /// revision. A call with no turn open (the session's start was not seen)
    /// opens one; a call from a different run than the open turn's closes it.
    pub fn note_state_change(&self, chat_id: &str, run_id: &str) {
        let stale = self
            .state()
            .turns
            .get(chat_id)
            .is_some_and(|t| t.run_id.as_deref().is_some_and(|open| open != run_id));
        if stale || !self.turn_open(chat_id) {
            self.begin(chat_id);
        }
        if let Some(turn) = self.state().turns.get_mut(chat_id) {
            turn.run_id = Some(run_id.to_owned());
            turn.owed = true;
        }
    }

    /// The session settled: the turn ended.
    pub fn end(&self, chat_id: &str) {
        let basis = self.basis(chat_id);
        {
            let mut state = self.state();
            Self::finish(&mut state, chat_id, basis.as_ref());
        }
        self.wake.notify_one();
    }

    /// A run's token was dropped: the turn that run was in, if still open, ended.
    pub fn end_run(&self, chat_id: &str, run_id: &str) {
        let basis = self.basis(chat_id);
        {
            let mut state = self.state();
            if state.turns.get(chat_id).is_some_and(|t| t.run_id.as_deref() == Some(run_id)) {
                Self::finish(&mut state, chat_id, basis.as_ref());
            }
        }
        self.wake.notify_one();
    }

    fn finish(state: &mut State, chat_id: &str, basis: Option<&RunBasis>) {
        let Some(turn) = state.turns.remove(chat_id) else {
            return;
        };
        if !turn.owed {
            return;
        }
        let ended = EndedRun {
            chat_id: chat_id.to_owned(),
            baseline: turn.baseline,
            end_revision: basis.map_or(turn.baseline, |b| b.revision).max(turn.baseline),
            project: turn.project,
            ask: turn.ask,
            forced: basis.is_some_and(|b| !b.holds),
        };
        Self::push(state, ended);
    }

    /// Queue an ended turn. Forced turns of one chat merge into one obligation:
    /// they ask the same thing, a status now, so a list of them could only grow
    /// without bound while a reservation stands.
    fn push(state: &mut State, ended: EndedRun) {
        if ended.forced
            && let Some(held) = state.ended.iter_mut().find(|e| e.chat_id == ended.chat_id && e.forced)
        {
            held.baseline = held.baseline.max(ended.baseline);
            held.end_revision = held.end_revision.max(ended.end_revision);
            held.ask = held.ask && ended.ask;
            return;
        }
        state.ended.push(ended);
    }

    /// Whether a run is in flight on the chat that is being tracked.
    pub fn turn_open(&self, chat_id: &str) -> bool {
        self.state().turns.contains_key(chat_id)
    }

    /// The baseline and project of the run in flight on the chat.
    pub fn current_run(&self, chat_id: &str) -> Option<(u32, (String, String))> {
        self.state().turns.get(chat_id).map(|r| (r.baseline, r.project.clone()))
    }

    /// Turns that ended and still need their check, for the chat (tests).
    pub fn pending(&self, chat_id: &str) -> usize {
        self.state().ended.iter().filter(|e| e.chat_id == chat_id).count()
    }

    fn take_ended(&self) -> Vec<EndedRun> {
        std::mem::take(&mut self.state().ended)
    }

    /// Put a turn back for a later check: its chat is about to hold the role.
    fn hold(&self, mut ended: EndedRun) {
        ended.forced = true;
        Self::push(&mut self.state(), ended);
    }

    /// Record the outcome of a turn's check.
    pub fn conclude(&self, ended: &EndedRun, published: bool) -> Verdict {
        let mut state = self.state();
        if published {
            state.asked.remove(&ended.chat_id);
            Verdict::Published
        } else if ended.ask {
            state.asked.remove(&ended.chat_id);
            Verdict::Missed { baseline: ended.baseline }
        } else {
            state.asked.insert(ended.chat_id.clone(), ended.baseline);
            Verdict::Nudge { baseline: ended.baseline }
        }
    }

    /// The ask could not be queued; allow the next state-changing turn to ask again.
    pub fn ask_failed(&self, chat_id: &str) {
        self.state().asked.remove(chat_id);
    }

    fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
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

/// Whether `chat_id` published a Manager-stamped revision in `(baseline, end]`:
/// one the turn itself made, not another chat's, an earlier turn's or a later
/// one's.
pub fn published_between(artifact: Option<&ArtifactView>, chat_id: &str, baseline: u32, end: u32) -> bool {
    artifact.is_some_and(|a| {
        a.revisions.iter().any(|r| {
            r.id > baseline
                && r.id <= end
                && manager_provenance(a, r)
                && r.manager_status.as_ref().is_some_and(|stamp| stamp.chat_id == chat_id)
        })
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

/// The project of a chat the Manager role is about to adopt: a first start
/// still carrying its provisional marker, or the successor a hand-off has
/// reserved.
fn pending_project(workspace: &WorkspaceHost, chat_id: &str) -> Option<(String, String)> {
    let item = workspace
        .chat(chat_id)
        .ok()
        .flatten()
        .and_then(|chat| chat.standing_role_provisional_scope(StandingRoleKind::Manager).map(str::to_owned))
        .or_else(|| {
            workspace
                .standing_role_awaiting(chat_id, StandingRoleKind::Manager)
                .map(|role| role.scope_item_id)
        })?;
    let space = workspace.work_item_space(&item)?;
    Some((space, item))
}

/// Where a turn on this chat belongs, how far its project's record has got and
/// whether the chat holds the role.
pub fn run_basis(workspace: &WorkspaceHost, chat_id: &str) -> RunBasis {
    let project = managed_project(workspace, chat_id).or_else(|| pending_project(workspace, chat_id));
    let (revision, holds) = match &project {
        Some((space, item)) => (
            status_revision(workspace, space, item),
            workspace.manager_claim(chat_id, item) == ManagerClaim::Holds,
        ),
        None => (0, false),
    };
    RunBasis { project, revision, holds }
}

enum Check {
    /// Not a manager's turn any more (the role moved on): nothing to check.
    Drop,
    /// The chat is about to hold the role (first start or hand-off successor).
    Wait,
    Ready { published: bool },
}

fn check(workspace: &WorkspaceHost, ended: &EndedRun) -> Check {
    let (space, item) = &ended.project;
    // One snapshot for who holds the role and who is reserved for it, so an
    // adoption cannot land between the two.
    match workspace.manager_claim(&ended.chat_id, item) {
        ManagerClaim::Holds => {
            let artifact = status_artifact(workspace, space, item);
            Check::Ready {
                published: !ended.forced
                    && published_between(artifact.as_ref(), &ended.chat_id, ended.baseline, ended.end_revision),
            }
        }
        ManagerClaim::Pending => Check::Wait,
        ManagerClaim::None => Check::Drop,
    }
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

/// Install the probe and watch for turns that ended owing a revision.
pub fn spawn_watch(tokens: Arc<AgentTokens>, workspace: WorkspaceHost, doc_host: DocHost) -> ManagerStatusWatch {
    let probe_workspace = workspace.clone();
    tokens
        .manager_status()
        .set_probe(Arc::new(move |chat_id| run_basis(&probe_workspace, chat_id)));
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let chat_changes = workspace.watch_chats();
    let handle = tokio::spawn(async move {
        let tracker = tokens.manager_status();
        let wake = tracker.waker();
        let mut chats = chat_changes;
        loop {
            let mut waiting = false;
            for ended in tracker.take_ended() {
                if stop.is_cancelled() {
                    return;
                }
                let published = match check(&workspace, &ended) {
                    Check::Drop => continue,
                    Check::Wait => {
                        waiting = true;
                        tracker.hold(ended);
                        continue;
                    }
                    Check::Ready { published } => published,
                };
                let chat_id = ended.chat_id.clone();
                match tracker.conclude(&ended, published) {
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
                    Verdict::Published => {}
                }
            }
            let recheck = tracker.recheck_interval();
            let retry = async {
                if waiting {
                    tokio::time::sleep(recheck).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = wake.notified() => {}
                changed = chats.changed() => if changed.is_err() { return },
                _ = retry => {}
            }
        }
    });
    ManagerStatusWatch { cancel, handle }
}


#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::artifacts::ArtifactRevision;
    use mast_proto::{ManagerStatusStamp, WorkOutlineActor, WorkOutlineActorKind};

    type Shared = Arc<Mutex<(Option<(String, String)>, u32, bool)>>;

    /// A tracker whose probe reads a project, a revision and whether the chat
    /// holds the role, all of which the test controls.
    fn tracker(project: bool, revision: u32) -> (StatusTracker, Shared) {
        let shared: Shared =
            Arc::new(Mutex::new((project.then(|| ("s".to_owned(), "proj".to_owned())), revision, true)));
        let tracker = StatusTracker::default();
        let probe = shared.clone();
        tracker.set_probe(Arc::new(move |_chat| {
            let (project, revision, holds) = probe.lock().unwrap().clone();
            RunBasis { project, revision, holds }
        }));
        (tracker, shared)
    }

    fn set_revision(shared: &Shared, revision: u32) {
        shared.lock().unwrap().1 = revision;
    }

    fn set_holds(shared: &Shared, holds: bool) {
        shared.lock().unwrap().2 = holds;
    }

    fn status_with_stamped(ids: &[u32]) -> ArtifactView {
        let stamp = Some(ManagerStatusStamp {
            role_id: mast_proto::standing_role_id(StandingRoleKind::Manager, "proj"),
            chat_id: "m".into(),
        });
        let agent = WorkOutlineActor { kind: WorkOutlineActorKind::Agent, ..Default::default() };
        ArtifactView {
            work_item_id: Some("proj".into()),
            revisions: ids
                .iter()
                .rev()
                .map(|&id| ArtifactRevision {
                    id,
                    by: agent.clone(),
                    manager_status: stamp.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn published(tracker: &StatusTracker, ended: &EndedRun, stamped: &[u32]) -> Verdict {
        let artifact = status_with_stamped(stamped);
        let published = published_between(Some(&artifact), &ended.chat_id, ended.baseline, ended.end_revision);
        tracker.conclude(ended, published)
    }

    #[test]
    fn a_turn_that_changed_state_without_a_revision_is_asked_once() {
        let (t, shared) = tracker(true, 2);
        t.begin("m");
        t.note_state_change("m", "r1");
        t.end("m");
        let ended = t.take_ended().remove(0);
        assert_eq!(published(&t, &ended, &[]), Verdict::Nudge { baseline: 2 });
        // The ask is the next run: held to the same baseline, not asked twice.
        t.begin("m");
        t.end("m");
        let ask = t.take_ended().remove(0);
        assert!(ask.ask);
        assert_eq!(published(&t, &ask, &[]), Verdict::Missed { baseline: 2 });
        // Afterwards a fresh state-changing turn can be asked again.
        set_revision(&shared, 2);
        t.begin("m");
        t.note_state_change("m", "r3");
        t.end("m");
        let next = t.take_ended().remove(0);
        assert!(!next.ask);
        assert_eq!(published(&t, &next, &[]), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_turn_that_publishes_while_it_runs_is_satisfied_and_clears_the_ask() {
        let (t, shared) = tracker(true, 1);
        t.begin("m");
        t.note_state_change("m", "r1");
        set_revision(&shared, 2);
        t.end("m");
        let ended = t.take_ended().remove(0);
        assert_eq!((ended.baseline, ended.end_revision), (1, 2));
        assert_eq!(published(&t, &ended, &[2]), Verdict::Published);
    }

    #[test]
    fn consecutive_turns_never_share_a_checkpoint() {
        let (t, shared) = tracker(true, 1);
        // Turn A publishes revision 2, then turn B starts at once, before
        // anything has looked at A's end.
        t.begin("m");
        t.note_state_change("m", "a");
        set_revision(&shared, 2);
        t.end("m");
        t.begin("m");
        t.note_state_change("m", "b");
        t.end("m");
        let ended = t.take_ended();
        assert_eq!(ended.len(), 2);
        let (a, b) = (&ended[0], &ended[1]);
        assert_eq!((a.baseline, a.end_revision), (1, 2));
        assert_eq!((b.baseline, b.end_revision), (2, 2));
        // The only stamped revision is A's. It satisfies A, not B.
        assert_eq!(published(&t, a, &[2]), Verdict::Published);
        assert_eq!(published(&t, b, &[2]), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_later_turns_publication_does_not_satisfy_an_earlier_one() {
        let (t, shared) = tracker(true, 1);
        t.begin("m");
        t.note_state_change("m", "a");
        t.end("m"); // A ended at revision 1, published nothing
        set_revision(&shared, 2);
        t.begin("m");
        set_revision(&shared, 3); // B publishes revisions 3
        t.end("m");
        let a = t.take_ended().remove(0);
        assert_eq!((a.baseline, a.end_revision), (1, 1));
        assert_eq!(published(&t, &a, &[3]), Verdict::Nudge { baseline: 1 });
    }

    #[test]
    fn an_existing_record_seeds_the_baseline() {
        let (t, _shared) = tracker(true, 3);
        t.begin("m");
        assert_eq!(t.current_run("m"), Some((3, ("s".to_owned(), "proj".to_owned()))));
        t.note_state_change("m", "r1");
        t.end("m");
        let ended = t.take_ended().remove(0);
        // The earlier revisions 1..=3 cannot satisfy the turn.
        assert_eq!(published(&t, &ended, &[1, 2, 3]), Verdict::Nudge { baseline: 3 });
    }

    #[test]
    fn a_state_change_opens_the_run_when_its_start_was_not_seen() {
        let (t, _shared) = tracker(true, 4);
        t.note_state_change("m", "r1");
        assert!(t.turn_open("m"));
        t.end("m");
        let ended = t.take_ended().remove(0);
        assert_eq!(ended.baseline, 4);
    }

    #[test]
    fn a_run_that_never_reported_its_end_is_closed_by_the_next_start() {
        let (t, shared) = tracker(true, 1);
        t.begin("m");
        t.note_state_change("m", "a");
        set_revision(&shared, 5);
        t.begin("m");
        let ended = t.take_ended();
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].baseline, ended[0].end_revision), (1, 5));
        assert_eq!(t.current_run("m").unwrap().0, 5);
    }

    #[test]
    fn a_turn_without_a_state_change_or_a_project_owes_nothing() {
        let (t, _s) = tracker(true, 0);
        t.begin("m");
        t.end("m");
        assert!(t.take_ended().is_empty());
        let (t, _s) = tracker(false, 0);
        t.begin("w");
        t.note_state_change("w", "r1");
        assert!(!t.turn_open("w"));
        t.end("w");
        assert!(t.take_ended().is_empty());
        // Without a probe nothing is tracked at all.
        let bare = StatusTracker::default();
        bare.note_state_change("m", "r1");
        assert!(!bare.turn_open("m"));
    }

    #[test]
    fn a_warm_session_ends_each_turn_at_the_session_boundary_not_the_token() {
        let (t, shared) = tracker(true, 1);
        // One token, `tok`, serves two turns of a parked session.
        t.begin("m"); // turn one: the session turns active
        t.note_state_change("m", "tok");
        t.end("m"); // it parks: the turn is over, the token lives on
        set_revision(&shared, 2); // an unrelated publication between the turns
        t.begin("m"); // turn two: a prompt steered into the parked run
        t.note_state_change("m", "tok");
        t.end("m");
        t.end_run("m", "tok"); // the run finally ends: nothing left to close
        let ended = t.take_ended();
        assert_eq!(ended.len(), 2, "two turns, not one interval");
        assert_eq!((ended[0].baseline, ended[0].end_revision), (1, 1));
        assert_eq!((ended[1].baseline, ended[1].end_revision), (2, 2));
        // The publication between them satisfies neither turn's window.
        assert_eq!(published(&t, &ended[0], &[2]), Verdict::Nudge { baseline: 1 });
    }

    #[test]
    fn a_runs_end_closes_only_the_turn_that_run_was_in() {
        let (t, _s) = tracker(true, 0);
        t.begin("m");
        t.note_state_change("m", "old-run");
        t.end("m");
        t.begin("m"); // the next turn belongs to a newer run
        t.note_state_change("m", "new-run");
        t.end_run("m", "old-run"); // the old run's token is dropped late
        assert!(t.turn_open("m"), "the newer run's turn is untouched");
        t.end_run("m", "new-run");
        assert!(!t.turn_open("m"));
    }

    #[test]
    fn turns_that_end_before_adoption_merge_when_they_end_not_when_looked_at() {
        let (t, shared) = tracker(true, 4);
        set_holds(&shared, false);
        for n in 0..30u32 {
            set_revision(&shared, 4 + n);
            t.begin("m");
            t.note_state_change("m", "tok");
            t.end("m");
            // Nothing has looked at them yet, and adoption may land at any time.
            assert_eq!(t.pending("m"), 1, "after {} turns", n + 1);
        }
        set_holds(&shared, true); // adoption lands before the watcher runs
        let held = t.take_ended();
        assert_eq!(held.len(), 1);
        assert!(held[0].forced);
        assert_eq!(held[0].baseline, 4 + 29);
        // A turn that ends while the chat holds the role is its own, checked on its window.
        t.begin("m");
        t.note_state_change("m", "tok");
        t.end("m");
        assert!(!t.take_ended()[0].forced);
    }

    #[test]
    fn a_turn_held_for_adoption_comes_back_owed_outright() {
        let (t, _s) = tracker(true, 0);
        t.begin("m");
        t.note_state_change("m", "r1");
        t.end("m");
        let ended = t.take_ended().remove(0);
        assert_eq!(t.pending("m"), 0);
        assert!(!ended.forced);
        t.hold(ended);
        assert_eq!(t.pending("m"), 1);
        let back = t.take_ended().remove(0);
        assert!(back.forced, "a turn that ended before the chat held the role cannot have published");
    }

    #[test]
    fn many_held_turns_of_one_chat_keep_one_obligation_however_many_there_are() {
        let (t, shared) = tracker(true, 4);
        for n in 0..40u32 {
            set_revision(&shared, 4 + n);
            let run = format!("r{n}");
            t.begin("m");
            t.note_state_change("m", &run);
            t.end("m");
            for ended in t.take_ended() {
                t.hold(ended);
            }
            // Past any fixed list length the obligation is still there, once.
            assert_eq!(t.pending("m"), 1, "after {} turns", n + 1);
        }
        let held = t.take_ended().remove(0);
        assert!(held.forced);
        assert_eq!(held.baseline, 4 + 39, "the newest turn's baseline");
        // Another chat's held turn is its own obligation.
        t.begin("other");
        t.note_state_change("other", "o1");
        t.end("other");
        for ended in t.take_ended() {
            t.hold(ended);
        }
        assert_eq!((t.pending("m"), t.pending("other")), (0, 1));
    }

    #[test]
    fn a_failed_ask_lets_the_next_turn_ask_again() {
        let (t, _s) = tracker(true, 0);
        t.begin("m");
        t.note_state_change("m", "r1");
        t.end("m");
        let ended = t.take_ended().remove(0);
        assert_eq!(published(&t, &ended, &[]), Verdict::Nudge { baseline: 0 });
        t.ask_failed("m");
        t.begin("m");
        assert!(!t.state().runs["m"].ask);
    }

    #[test]
    fn only_manager_stamped_revisions_in_the_window_count() {
        let mut a = status_with_stamped(&[1, 2]);
        a.revisions.insert(
            0,
            ArtifactRevision {
                id: 3,
                by: WorkOutlineActor { kind: WorkOutlineActorKind::Agent, ..Default::default() },
                ..Default::default()
            },
        );
        assert!(published_between(Some(&a), "m", 0, 3));
        assert!(!published_between(Some(&a), "m", 2, 3), "an unstamped revision is not a publication");
        assert!(!published_between(Some(&a), "m", 0, 0));
        assert!(!published_between(None, "m", 0, 9));
        assert!(!published_between(Some(&a), "another-chat", 0, 3), "another chat's publication is not this turn's");
    }

    #[test]
    fn dispatching_a_worker_is_state_changing() {
        assert!(is_state_changing(mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::READ_SUBTREE));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::BRIEFING_READ));
    }
}
