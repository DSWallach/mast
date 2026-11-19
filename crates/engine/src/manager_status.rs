//! Keeps a project's Manager status record current. A manager turn that
//! changed state must end with a new revision of the record; when it does
//! not, the engine asks once.
//!
//! A turn is a run, and a run's boundaries are its agent token's: minted when
//! the run starts, dropped when it ends. The record's revision is read at the
//! first and again at the second, so each turn is checked against exactly the
//! revisions that existed while it ran. A checkpoint from an earlier turn, or
//! a later turn's publication, can never satisfy it, however closely the runs
//! follow each other. The ask itself is a turn, checked against the revision of
//! the turn it asks about.
//!
//! A Manager's chat holds the role only after its first turn is queued: a first
//! start carries a provisional marker, and a hand-off's successor is named in
//! the role's reserved hand-off. Either way the project is known before the
//! role is, and a turn that ends first is held for as long as that marker or
//! reservation stays valid: nothing expires a reservation, so neither does the
//! obligation. It is checked once the chat holds the role, and dropped, with
//! nothing sent, when the reservation is replaced or cleared, the role moves to
//! another chat, or the chat is gone or archived. Only the chat that holds the
//! role is ever reminded. The watcher re-checks on chat changes and on a slow
//! timer, never in a tight loop.
//!
//! Locks: the grants map, the tracker's state and the probe are never held
//! while the probe reads the workspace, and a token is minted and dropped from
//! run tasks and RPC handlers, which hold no workspace lock.

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

/// The project a run belongs to and the record's revision when it is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunBasis {
    /// `(space id, project item id)`, when the chat manages a project or is
    /// the first start that will.
    pub project: Option<(String, String)>,
    pub revision: u32,
}

pub type RunProbe = Arc<dyn Fn(&str) -> RunBasis + Send + Sync>;

/// How often held turns are re-checked when no chat change wakes the watcher.
/// Adoption normally follows within moments and wakes it through the chat
/// change; this is only the net under changes that touch the role alone.
const DEFAULT_RECHECK: Duration = Duration::from_secs(30);

struct Run {
    run_id: String,
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
    /// Held for a chat that did not hold the role when the turn ended, so the
    /// turn cannot have published and is owed whatever the revisions say.
    forced: bool,
}

#[derive(Default)]
struct State {
    runs: HashMap<String, Run>,
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
    /// How runs are placed in a project and how far the record has got. Until
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

    /// A run began: its token was minted. The turn's baseline is the record's
    /// revision right now, or the original baseline when this run is the ask.
    pub fn run_started(&self, chat_id: &str, run_id: &str) {
        let basis = self.basis(chat_id);
        let mut state = self.state();
        if let Some(previous) = state.runs.get(chat_id) {
            if previous.run_id == run_id {
                return;
            }
            // The earlier run never reported its end.
            let end = basis.as_ref().map_or(previous.baseline, |b| b.revision);
            Self::finish(&mut state, chat_id, end);
        }
        let Some(RunBasis { project: Some(project), revision }) = basis else {
            return;
        };
        let (baseline, owed, ask) = match state.asked.get(chat_id) {
            Some(&asked) => (asked, true, true),
            None => (revision, false, false),
        };
        state.runs.insert(
            chat_id.to_owned(),
            Run { run_id: run_id.to_owned(), baseline, project, owed, ask },
        );
    }

    /// The run called a state-changing tool, so its turn owes a revision.
    pub fn note_state_change(&self, chat_id: &str, run_id: &str) {
        if !self.is_current(chat_id, run_id) {
            self.run_started(chat_id, run_id);
        }
        if let Some(run) = self.state().runs.get_mut(chat_id).filter(|r| r.run_id == run_id) {
            run.owed = true;
        }
    }

    fn is_current(&self, chat_id: &str, run_id: &str) -> bool {
        self.state().runs.get(chat_id).is_some_and(|r| r.run_id == run_id)
    }

    /// The run ended: its token was dropped.
    pub fn run_ended(&self, chat_id: &str, run_id: &str) {
        let end = self.basis(chat_id).map_or(0, |b| b.revision);
        {
            let mut state = self.state();
            if state.runs.get(chat_id).is_some_and(|r| r.run_id == run_id) {
                Self::finish(&mut state, chat_id, end);
            }
        }
        self.wake.notify_one();
    }

    fn finish(state: &mut State, chat_id: &str, end_revision: u32) {
        let Some(run) = state.runs.remove(chat_id) else {
            return;
        };
        if run.owed {
            state.ended.push(EndedRun {
                chat_id: chat_id.to_owned(),
                baseline: run.baseline,
                end_revision: end_revision.max(run.baseline),
                project: run.project,
                ask: run.ask,
                forced: false,
            });
        }
    }

    /// Whether a run is in flight on the chat that is being tracked.
    pub fn turn_open(&self, chat_id: &str) -> bool {
        self.state().runs.contains_key(chat_id)
    }

    /// The baseline and project of the run in flight on the chat.
    pub fn current_run(&self, chat_id: &str) -> Option<(u32, (String, String))> {
        self.state().runs.get(chat_id).map(|r| (r.baseline, r.project.clone()))
    }

    /// Turns that ended and still need their check, for the chat (tests).
    pub fn pending(&self, chat_id: &str) -> usize {
        self.state().ended.iter().filter(|e| e.chat_id == chat_id).count()
    }

    fn take_ended(&self) -> Vec<EndedRun> {
        std::mem::take(&mut self.state().ended)
    }

    /// Keep a turn for a chat that is about to hold the role. Such a turn ended
    /// while the chat held nothing, so it cannot have published: it is owed
    /// outright. Every owed turn of one chat asks the same thing, a status now,
    /// so they share one obligation rather than a list that could grow without
    /// bound while a reservation stands.
    fn hold(&self, mut ended: EndedRun) {
        ended.forced = true;
        let mut state = self.state();
        match state.ended.iter_mut().find(|e| e.chat_id == ended.chat_id && e.forced) {
            Some(held) => {
                held.baseline = held.baseline.max(ended.baseline);
                held.end_revision = held.end_revision.max(ended.end_revision);
                held.ask = held.ask && ended.ask;
            }
            None => state.ended.push(ended),
        }
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

/// Where a run on this chat belongs and how far its project's record has got.
pub fn run_basis(workspace: &WorkspaceHost, chat_id: &str) -> RunBasis {
    let project = managed_project(workspace, chat_id).or_else(|| pending_project(workspace, chat_id));
    let revision = project
        .as_ref()
        .map_or(0, |(space, item)| status_revision(workspace, space, item));
    RunBasis { project, revision }
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
    let holds = workspace
        .standing_role(StandingRoleKind::Manager, item)
        .is_some_and(|role| role.current_chat_id.as_deref() == Some(ended.chat_id.as_str()));
    if holds {
        let artifact = status_artifact(workspace, space, item);
        return Check::Ready {
            published: !ended.forced
                && published_between(artifact.as_ref(), &ended.chat_id, ended.baseline, ended.end_revision),
        };
    }
    // A chat that is gone or archived will never hold the role.
    let Some(chat) = workspace.chat(&ended.chat_id).ok().flatten().filter(|chat| !chat.archived) else {
        return Check::Drop;
    };
    let pending = chat.is_provisional_for(StandingRoleKind::Manager, item)
        || workspace
            .standing_role_awaiting(&ended.chat_id, StandingRoleKind::Manager)
            .is_some_and(|role| role.scope_item_id == *item);
    if pending { Check::Wait } else { Check::Drop }
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

    type Shared = Arc<Mutex<(Option<(String, String)>, u32)>>;

    /// A tracker whose probe reads a project and a revision the test controls.
    fn tracker(project: bool, revision: u32) -> (StatusTracker, Shared) {
        let shared: Shared =
            Arc::new(Mutex::new((project.then(|| ("s".to_owned(), "proj".to_owned())), revision)));
        let tracker = StatusTracker::default();
        let probe = shared.clone();
        tracker.set_probe(Arc::new(move |_chat| {
            let (project, revision) = probe.lock().unwrap().clone();
            RunBasis { project, revision }
        }));
        (tracker, shared)
    }

    fn set_revision(shared: &Shared, revision: u32) {
        shared.lock().unwrap().1 = revision;
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
        t.run_started("m", "r1");
        t.note_state_change("m", "r1");
        t.run_ended("m", "r1");
        let ended = t.take_ended().remove(0);
        assert_eq!(published(&t, &ended, &[]), Verdict::Nudge { baseline: 2 });
        // The ask is the next run: held to the same baseline, not asked twice.
        t.run_started("m", "r2");
        t.run_ended("m", "r2");
        let ask = t.take_ended().remove(0);
        assert!(ask.ask);
        assert_eq!(published(&t, &ask, &[]), Verdict::Missed { baseline: 2 });
        // Afterwards a fresh state-changing turn can be asked again.
        set_revision(&shared, 2);
        t.run_started("m", "r3");
        t.note_state_change("m", "r3");
        t.run_ended("m", "r3");
        let next = t.take_ended().remove(0);
        assert!(!next.ask);
        assert_eq!(published(&t, &next, &[]), Verdict::Nudge { baseline: 2 });
    }

    #[test]
    fn a_turn_that_publishes_while_it_runs_is_satisfied_and_clears_the_ask() {
        let (t, shared) = tracker(true, 1);
        t.run_started("m", "r1");
        t.note_state_change("m", "r1");
        set_revision(&shared, 2);
        t.run_ended("m", "r1");
        let ended = t.take_ended().remove(0);
        assert_eq!((ended.baseline, ended.end_revision), (1, 2));
        assert_eq!(published(&t, &ended, &[2]), Verdict::Published);
    }

    #[test]
    fn consecutive_turns_never_share_a_checkpoint() {
        let (t, shared) = tracker(true, 1);
        // Turn A publishes revision 2, then turn B starts at once, before
        // anything has looked at A's end.
        t.run_started("m", "a");
        t.note_state_change("m", "a");
        set_revision(&shared, 2);
        t.run_ended("m", "a");
        t.run_started("m", "b");
        t.note_state_change("m", "b");
        t.run_ended("m", "b");
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
        t.run_started("m", "a");
        t.note_state_change("m", "a");
        t.run_ended("m", "a"); // A ended at revision 1, published nothing
        set_revision(&shared, 2);
        t.run_started("m", "b");
        set_revision(&shared, 3); // B publishes revisions 3
        t.run_ended("m", "b");
        let a = t.take_ended().remove(0);
        assert_eq!((a.baseline, a.end_revision), (1, 1));
        assert_eq!(published(&t, &a, &[3]), Verdict::Nudge { baseline: 1 });
    }

    #[test]
    fn an_existing_record_seeds_the_baseline() {
        let (t, _shared) = tracker(true, 3);
        t.run_started("m", "r1");
        assert_eq!(t.current_run("m"), Some((3, ("s".to_owned(), "proj".to_owned()))));
        t.note_state_change("m", "r1");
        t.run_ended("m", "r1");
        let ended = t.take_ended().remove(0);
        // The earlier revisions 1..=3 cannot satisfy the turn.
        assert_eq!(published(&t, &ended, &[1, 2, 3]), Verdict::Nudge { baseline: 3 });
    }

    #[test]
    fn a_state_change_opens_the_run_when_its_start_was_not_seen() {
        let (t, _shared) = tracker(true, 4);
        t.note_state_change("m", "r1");
        assert!(t.turn_open("m"));
        t.run_ended("m", "r1");
        let ended = t.take_ended().remove(0);
        assert_eq!(ended.baseline, 4);
    }

    #[test]
    fn a_run_that_never_reported_its_end_is_closed_by_the_next_start() {
        let (t, shared) = tracker(true, 1);
        t.run_started("m", "a");
        t.note_state_change("m", "a");
        set_revision(&shared, 5);
        t.run_started("m", "b");
        let ended = t.take_ended();
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].baseline, ended[0].end_revision), (1, 5));
        assert_eq!(t.current_run("m").unwrap().0, 5);
    }

    #[test]
    fn a_turn_without_a_state_change_or_a_project_owes_nothing() {
        let (t, _s) = tracker(true, 0);
        t.run_started("m", "r1");
        t.run_ended("m", "r1");
        assert!(t.take_ended().is_empty());
        let (t, _s) = tracker(false, 0);
        t.run_started("w", "r1");
        t.note_state_change("w", "r1");
        assert!(!t.turn_open("w"));
        t.run_ended("w", "r1");
        assert!(t.take_ended().is_empty());
        // Without a probe nothing is tracked at all.
        let bare = StatusTracker::default();
        bare.note_state_change("m", "r1");
        assert!(!bare.turn_open("m"));
    }

    #[test]
    fn a_turn_held_for_adoption_comes_back_owed_outright() {
        let (t, _s) = tracker(true, 0);
        t.run_started("m", "r1");
        t.note_state_change("m", "r1");
        t.run_ended("m", "r1");
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
            t.run_started("m", &run);
            t.note_state_change("m", &run);
            t.run_ended("m", &run);
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
        t.run_started("other", "o1");
        t.note_state_change("other", "o1");
        t.run_ended("other", "o1");
        for ended in t.take_ended() {
            t.hold(ended);
        }
        assert_eq!((t.pending("m"), t.pending("other")), (0, 1));
    }

    #[test]
    fn a_failed_ask_lets_the_next_turn_ask_again() {
        let (t, _s) = tracker(true, 0);
        t.run_started("m", "r1");
        t.note_state_change("m", "r1");
        t.run_ended("m", "r1");
        let ended = t.take_ended().remove(0);
        assert_eq!(published(&t, &ended, &[]), Verdict::Nudge { baseline: 0 });
        t.ask_failed("m");
        t.run_started("m", "r2");
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
