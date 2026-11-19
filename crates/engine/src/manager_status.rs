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

use std::collections::{HashMap, HashSet, VecDeque};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The turn published a revision while it ran. `supersede` names a reminder
    /// that may still be queued, which the publication has made unnecessary.
    Published { supersede: Option<String> },
    /// State changed and no revision followed: ask once, with the message id
    /// the tracker has already reserved for the reminder.
    Nudge { baseline: u32, message_id: String },
    /// A reminder for the chat is already on its way; asking again adds nothing.
    Outstanding,
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

/// Where a queued message stands in its chat's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueRow {
    Queued,
    /// The row is not in the queue.
    Absent,
    /// The queue could not be read: nothing is known.
    Unknown,
}

pub type QueueProbe = Arc<dyn Fn(&str, &str) -> QueueRow + Send + Sync>;

/// How often held turns are re-checked when no chat change wakes the watcher.
/// Adoption normally follows within moments and wakes it through the chat
/// change; this is only the net under changes that touch the role alone.
const DEFAULT_RECHECK: Duration = Duration::from_secs(30);

/// One turn in flight: from the session turning active to it settling.
struct Turn {
    /// The session's number for the turn. Turns of a chat are numbered in the
    /// order the session started them, so a late report about an earlier turn
    /// can never close a later one. `0` for a turn with no session behind it.
    seq: u64,
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

/// A turn number, and the queued message it carries, before its turn is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Numbered {
    seq: u64,
    source: Option<String>,
    /// The session has taken the number (it reserved it before the prompt was
    /// shown to the agent; it is consumed when the status turns active).
    consumed: bool,
}

/// What is needed to settle a call that completes after its turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClosedTurn {
    baseline: u32,
    /// The record's revision when the turn actually ended.
    end_revision: u32,
    project: (String, String),
    ask: bool,
    /// The chat did not hold the role when the turn ended.
    forced: bool,
}

/// How many deleted chats are remembered, oldest forgotten first.
const MAX_GONE: usize = 4096;

/// The reminder the tracker is waiting on for a chat. Its message id is chosen
/// before the reminder exists, so no delivery can outrun the record of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ask {
    baseline: u32,
    message_id: String,
    /// The queue row has been taken for dispatch and has not settled either way.
    dispatching: bool,
    /// The turn that delivered the reminder has started.
    running: bool,
    /// How many times a dispatch of the reminder failed and its row was restored.
    restores: u32,
}

#[derive(Default)]
struct State {
    turns: HashMap<String, Turn>,
    /// The latest turn number handed out per chat, and the highest one whose
    /// end has been reported.
    seq: HashMap<String, u64>,
    closed: HashMap<String, u64>,
    /// Numbers handed out for turns whose start has not been reported yet,
    /// oldest first: reserved for a prompt about to reach the agent, or taken by
    /// the session. Each carries the queued message that started the turn, and
    /// stays until the turn's start is reported, so a tool call that lands in
    /// between can still find who started it.
    pending: HashMap<String, VecDeque<Numbered>>,
    /// Tool calls admitted into a turn and not yet settled, per chat and turn.
    inflight: HashMap<(String, u64), usize>,
    /// A turn that ended while a call admitted into it was still running, kept,
    /// with its actual end revision, until every such call has settled, however
    /// many turns have ended since.
    closed_turns: HashMap<(String, u64), ClosedTurn>,
    /// Chats confirmed deleted. A late report about one creates nothing.
    gone: HashSet<String>,
    gone_order: VecDeque<String>,
    asked: HashMap<String, Ask>,
    /// Finished turns of a holder, awaiting their check.
    ended: Vec<EndedRun>,
    /// One obligation per chat that is about to hold the role: its turns that
    /// ended without it. It stays here, and keeps absorbing further turns,
    /// until the chat holds the role and the obligation is taken for good.
    held: HashMap<String, EndedRun>,
}

#[derive(Default)]
pub struct StatusTracker {
    state: Mutex<State>,
    probe: Mutex<Option<RunProbe>>,
    queue: Mutex<Option<QueueProbe>>,
    wake: Arc<Notify>,
    recheck: Mutex<Option<Duration>>,
}

impl StatusTracker {
    /// How turns are placed in a project and how far the record has got. Until
    /// it is set nothing is tracked.
    pub fn set_probe(&self, probe: RunProbe) {
        *self.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some(probe);
    }

    /// How the tracker looks up a queued message in its chat's queue.
    pub fn set_queue_probe(&self, probe: QueueProbe) {
        *self.queue.lock().unwrap_or_else(|e| e.into_inner()) = Some(probe);
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

    fn queue_row(&self, chat_id: &str, message_id: &str) -> QueueRow {
        let probe = self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone();
        probe.map_or(QueueRow::Unknown, |probe| probe(chat_id, message_id))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number the turn that a prompt about to reach the agent will start, and
    /// note the queued message it carries. Done before the agent can see the
    /// prompt, so the agent's first tool call already finds its turn. Asking
    /// again for the same message changes nothing.
    pub fn reserve_turn(&self, chat_id: &str, source: Option<&str>) {
        let mut state = self.state();
        if state.gone.contains(chat_id) {
            return;
        }
        if let Some(id) = source
            && state.pending.get(chat_id).is_some_and(|q| q.iter().any(|n| n.source.as_deref() == Some(id)))
        {
            return;
        }
        let seq = state.seq.entry(chat_id.to_owned()).or_default();
        *seq += 1;
        let seq = *seq;
        state.pending.entry(chat_id.to_owned()).or_default().push_back(Numbered {
            seq,
            source: source.map(str::to_owned),
            consumed: false,
        });
    }

    /// The prompt was not delivered after all, or its delivery has finished:
    /// drop the reservation if the session has not taken the number. The number
    /// itself is spent: numbers are never reused, so a call admitted into this
    /// turn can never be confused with a later one. A number the session has
    /// taken stays until its turn's start is reported.
    pub fn release_turn(&self, chat_id: &str, source: &str) {
        let mut state = self.state();
        if let Some(queue) = state.pending.get_mut(chat_id)
            && let Some(at) = queue.iter().position(|n| !n.consumed && n.source.as_deref() == Some(source))
        {
            queue.remove(at);
        }
    }

    /// The session has no more prompts coming (its run is gone): drop numbers
    /// reserved for prompts that never started a turn.
    pub fn clear_pending(&self, chat_id: &str) {
        let mut state = self.state();
        if let Some(queue) = state.pending.get_mut(chat_id) {
            queue.retain(|n| n.consumed);
        }
    }

    /// Number the next turn of the chat: the oldest reserved number, or a new
    /// one. The session calls this under its own status lock, in the same step
    /// that publishes the status, so the numbers follow the order the session
    /// started its turns in. The number stays on record until the turn's start
    /// is reported.
    pub fn next_turn(&self, chat_id: &str) -> (u64, Option<String>) {
        let mut state = self.state();
        if state.gone.contains(chat_id) {
            return (0, None);
        }
        let queue = state.pending.entry(chat_id.to_owned()).or_default();
        if let Some(reserved) = queue.iter_mut().find(|n| !n.consumed) {
            reserved.consumed = true;
            return (reserved.seq, reserved.source.clone());
        }
        let seq = state.seq.entry(chat_id.to_owned()).or_default();
        *seq += 1;
        let seq = *seq;
        state
            .pending
            .entry(chat_id.to_owned())
            .or_default()
            .push_back(Numbered { seq, source: None, consumed: true });
        (seq, None)
    }

    /// The number of the chat's latest turn (`0` before any).
    pub fn current_turn(&self, chat_id: &str) -> u64 {
        self.state().seq.get(chat_id).copied().unwrap_or(0)
    }

    /// The turn a tool call belongs to as it is admitted: the newest turn that
    /// is live, that is, in flight or numbered and waiting to be reported, else
    /// the latest that ever was. A number is reserved or taken before the agent
    /// can see its prompt, and only for a prompt that starts a turn, so this is
    /// the turn the call is in, even while the previous turn's end, or this
    /// turn's start, has not been reported. A number that was reserved and then
    /// released is no longer live, so calls in the turn still running are not
    /// attributed to it.
    pub fn admit(&self, chat_id: &str) -> u64 {
        let state = self.state();
        Self::live_turn(&state, chat_id)
    }

    fn live_turn(state: &State, chat_id: &str) -> u64 {
        let open = state.turns.get(chat_id).map_or(0, |t| t.seq);
        let waiting = state
            .pending
            .get(chat_id)
            .and_then(|q| q.iter().map(|n| n.seq).max())
            .unwrap_or(0);
        match open.max(waiting) {
            0 => state.seq.get(chat_id).copied().unwrap_or(0),
            live => live,
        }
    }

    /// A state-changing call is being admitted: it is placed in its turn, which
    /// is returned, and counted as running until it settles, so that turn's
    /// record is kept for as long as the call can still complete.
    pub fn admit_call(&self, chat_id: &str) -> u64 {
        let mut state = self.state();
        let turn = Self::live_turn(&state, chat_id);
        if turn > 0 && !state.gone.contains(chat_id) {
            *state.inflight.entry((chat_id.to_owned(), turn)).or_default() += 1;
        }
        turn
    }

    /// The call admitted into `turn` has finished. When it succeeded the turn
    /// owes a revision, whatever has happened to the turn since. The turn's
    /// record is released once its last call has settled.
    pub fn complete_call(&self, chat_id: &str, run_id: &str, turn: u64, succeeded: bool) {
        if succeeded {
            self.note_state_change(chat_id, run_id, turn);
        }
        let mut state = self.state();
        let key = (chat_id.to_owned(), turn);
        if let Some(count) = state.inflight.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                state.inflight.remove(&key);
                state.closed_turns.remove(&key);
            }
        }
    }

    /// Number a turn and begin it (the session's start, as one step).
    pub fn start_turn(&self, chat_id: &str) -> u64 {
        let (seq, source) = self.next_turn(chat_id);
        self.begin(chat_id, seq, source.as_deref());
        seq
    }

    /// End the chat's latest turn (the session's settle, as one step).
    pub fn finish_turn(&self, chat_id: &str) {
        self.end(chat_id, self.current_turn(chat_id));
    }

    /// Turn `seq` began: the session turned active. Its baseline is the
    /// record's revision right now, or the original baseline when this turn
    /// is the one that delivered the reminder (its source is the reminder's
    /// message). An earlier turn still open (its end not reported yet) is
    /// closed first; a report for a turn that is already open, or already
    /// over, changes nothing.
    pub fn begin(&self, chat_id: &str, seq: u64, source: Option<&str>) {
        let basis = self.basis(chat_id);
        let mut state = self.state();
        if state.gone.contains(chat_id) {
            return;
        }
        // The number's start is being reported: it no longer needs its record.
        // A number still only reserved stays, for the session to take: a tool
        // call that lands first opens the turn but does not use the number up.
        let recorded = state
            .pending
            .get_mut(chat_id)
            .and_then(|q| q.iter().position(|n| n.seq == seq).map(|at| (q, at)))
            .and_then(|(q, at)| {
                if q[at].consumed {
                    q.remove(at).and_then(|n| n.source)
                } else {
                    q[at].source.clone()
                }
            });
        let source = source.map(str::to_owned).or(recorded);
        let source = source.as_deref();
        if seq > 0 && seq <= state.closed.get(chat_id).copied().unwrap_or(0) {
            return;
        }
        if let Some(open) = state.turns.get(chat_id) {
            if seq > 0 && open.seq >= seq {
                return;
            }
            Self::finish(&mut state, chat_id, basis.as_ref());
        }
        let Some(RunBasis { project: Some(project), revision, .. }) = basis else {
            return;
        };
        let claims = source.is_some()
            && state.asked.get(chat_id).is_some_and(|ask| source == Some(ask.message_id.as_str()));
        let (baseline, owed, ask) = if claims {
            let ask = state.asked.get_mut(chat_id).expect("checked above");
            ask.running = true;
            ask.dispatching = false;
            (ask.baseline, true, true)
        } else {
            (revision, false, false)
        };
        state.turns.insert(
            chat_id.to_owned(),
            Turn { seq, run_id: None, baseline, project, owed, ask },
        );
    }

    /// A state-changing tool call completed successfully. It belongs to `turn`,
    /// the turn it was admitted in, whatever has happened since. When that
    /// turn's start has not been reported yet this opens it, and the late
    /// report finds it open. When that turn already ended, the call is settled
    /// against it all the same: the turn owes a revision after all.
    pub fn note_state_change(&self, chat_id: &str, run_id: &str, turn: u64) {
        enum Step {
            Owe,
            Open(Option<String>),
            Late,
        }
        let step = {
            let state = self.state();
            if state.gone.contains(chat_id) {
                return;
            }
            let open = state.turns.get(chat_id).map(|t| t.seq);
            let closed = state.closed.get(chat_id).copied().unwrap_or(0);
            match open {
                Some(open) if turn == 0 || open == turn => Step::Owe,
                _ if turn > 0 && turn <= closed => Step::Late,
                Some(open) if open > turn => return,
                _ => Step::Open(
                    state
                        .pending
                        .get(chat_id)
                        .and_then(|q| q.iter().find(|n| n.seq == turn))
                        .and_then(|n| n.source.clone()),
                ),
            }
        };
        match step {
            Step::Owe => {}
            Step::Open(source) => self.begin(chat_id, turn, source.as_deref()),
            Step::Late => {
                // The turn ended before this call completed: it owes after all,
                // judged on the revisions as they were when the turn ended.
                let mut state = self.state();
                if let Some(closed) = state.closed_turns.remove(&(chat_id.to_owned(), turn)) {
                    let ended = EndedRun {
                        chat_id: chat_id.to_owned(),
                        baseline: closed.baseline,
                        end_revision: closed.end_revision,
                        project: closed.project,
                        ask: closed.ask,
                        forced: closed.forced,
                    };
                    if ended.forced {
                        Self::absorb(&mut state, ended);
                    } else {
                        state.ended.push(ended);
                    }
                    drop(state);
                    self.wake.notify_one();
                }
                return;
            }
        }
        if let Some(open) = self.state().turns.get_mut(chat_id).filter(|t| turn == 0 || t.seq == turn) {
            open.run_id = Some(run_id.to_owned());
            open.owed = true;
        }
    }

    /// A prompt carrying queued message `source` was accepted into a turn
    /// already running. That turn is the one that delivered it: if the message
    /// is the reminder, the turn is the ask. When the session has numbered the
    /// turn but its start is not reported yet, the message is bound to that
    /// number, and the start report claims it. When there is no such turn, the
    /// message is not tied to anything yet and nothing is marked.
    pub fn joined_active(&self, chat_id: &str, source: &str) {
        let mut state = self.state();
        let Some(baseline) = state
            .asked
            .get(chat_id)
            .filter(|a| a.message_id == source)
            .map(|a| a.baseline)
        else {
            return;
        };
        if state.turns.contains_key(chat_id) {
            let ask = state.asked.get_mut(chat_id).expect("checked above");
            ask.running = true;
            ask.dispatching = false;
            if let Some(turn) = state.turns.get_mut(chat_id) {
                turn.ask = true;
                turn.owed = true;
                turn.baseline = baseline;
            }
            return;
        }
        // Numbered, not reported: bind the message to the newest such turn.
        if let Some(numbered) = state
            .pending
            .get_mut(chat_id)
            .and_then(|q| q.iter_mut().filter(|n| n.consumed).max_by_key(|n| n.seq))
            && numbered.source.is_none()
        {
            numbered.source = Some(source.to_owned());
            let ask = state.asked.get_mut(chat_id).expect("checked above");
            ask.dispatching = false;
        }
    }

    /// Turn `seq` ended: the session settled. Ends the open turn only when it
    /// is that turn or an earlier one; a late report about an earlier turn
    /// never touches a later one.
    pub fn end(&self, chat_id: &str, seq: u64) {
        let basis = self.basis(chat_id);
        {
            let mut state = self.state();
            if state.gone.contains(chat_id) {
                return;
            }
            if seq > 0 && state.seq.contains_key(chat_id) {
                let closed = state.closed.entry(chat_id.to_owned()).or_default();
                *closed = (*closed).max(seq);
            }
            if state.turns.get(chat_id).is_some_and(|open| seq == 0 || open.seq <= seq) {
                Self::finish(&mut state, chat_id, basis.as_ref());
            }
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
        let end_revision = basis.map_or(turn.baseline, |b| b.revision).max(turn.baseline);
        let forced = basis.is_some_and(|b| !b.holds);
        if !turn.owed {
            // A call admitted into this turn may still complete: keep what it
            // would need, as the turn actually ended.
            if state.inflight.contains_key(&(chat_id.to_owned(), turn.seq)) {
                state.closed_turns.insert(
                    (chat_id.to_owned(), turn.seq),
                    ClosedTurn { baseline: turn.baseline, end_revision, project: turn.project, ask: turn.ask, forced },
                );
            }
            return;
        }
        let ended = EndedRun {
            chat_id: chat_id.to_owned(),
            baseline: turn.baseline,
            end_revision,
            project: turn.project,
            ask: turn.ask,
            forced,
        };
        if ended.forced {
            Self::absorb(state, ended);
        } else {
            state.ended.push(ended);
        }
    }

    /// Fold a turn that ended without the role into the chat's one obligation.
    /// They ask the same thing, a status now, so a list of them could only grow
    /// without bound while a reservation stands.
    fn absorb(state: &mut State, ended: EndedRun) {
        match state.held.get_mut(&ended.chat_id) {
            Some(held) => {
                held.baseline = held.baseline.max(ended.baseline);
                held.end_revision = held.end_revision.max(ended.end_revision);
                held.ask = held.ask && ended.ask;
            }
            None => {
                state.held.insert(ended.chat_id.clone(), ended);
            }
        }
    }

    /// Whether a turn is in flight on the chat that is being tracked.
    pub fn turn_open(&self, chat_id: &str) -> bool {
        self.state().turns.contains_key(chat_id)
    }

    /// The baseline and project of the turn in flight on the chat.
    pub fn current_run(&self, chat_id: &str) -> Option<(u32, (String, String))> {
        self.state().turns.get(chat_id).map(|r| (r.baseline, r.project.clone()))
    }

    /// Turns that ended and still need their check, for the chat (tests).
    pub fn pending(&self, chat_id: &str) -> usize {
        let state = self.state();
        state.ended.iter().filter(|e| e.chat_id == chat_id).count()
            + usize::from(state.held.contains_key(chat_id))
    }

    /// The reminder tracked for the chat, if any (tests).
    pub fn reminder(&self, chat_id: &str) -> Option<String> {
        self.state().asked.get(chat_id).map(|ask| ask.message_id.clone())
    }

    fn take_ended(&self) -> Vec<EndedRun> {
        std::mem::take(&mut self.state().ended)
    }

    /// A holder's turn found to belong to a chat that is about to hold the
    /// role: it joins that chat's obligation.
    fn hold(&self, mut ended: EndedRun) {
        ended.forced = true;
        Self::absorb(&mut self.state(), ended);
    }

    fn held_chats(&self) -> Vec<String> {
        self.state().held.keys().cloned().collect()
    }

    fn held(&self, chat_id: &str) -> Option<EndedRun> {
        self.state().held.get(chat_id).cloned()
    }

    /// Take the chat's obligation for good, whatever it has absorbed up to this
    /// instant. `None` when something else already took it.
    fn take_held(&self, chat_id: &str) -> Option<EndedRun> {
        self.state().held.remove(chat_id)
    }

    fn discard_held(&self, chat_id: &str) {
        self.state().held.remove(chat_id);
    }

    /// Record the outcome of a turn's check. A reminder is reserved here, under
    /// its message id, before it is queued, so that two turns concluding at once
    /// ask once and no delivery can outrun the record of it. A reminder stays
    /// reserved until it settles: its own turn ends, or it is withdrawn or
    /// cancelled before it was dispatched.
    pub fn conclude(&self, ended: &EndedRun, published: bool) -> Verdict {
        // Where the reminder's row stands is read before the lock is taken.
        let chat = &ended.chat_id;
        let peeked = self.state().asked.get(chat).cloned();
        let row = peeked
            .as_ref()
            .filter(|ask| !ask.dispatching && !ask.running)
            .map(|ask| self.queue_row(chat, &ask.message_id));
        let mut state = self.state();
        if ended.ask {
            state.asked.remove(chat);
            return if published {
                Verdict::Published { supersede: None }
            } else {
                Verdict::Missed { baseline: ended.baseline }
            };
        }
        let current = state.asked.get(chat).cloned();
        // A reminder that was not in flight and has left the queue was cancelled.
        if let (Some(now), Some(before)) = (&current, &peeked)
            && now.message_id == before.message_id
            && row == Some(QueueRow::Absent)
            && !now.dispatching
            && !now.running
        {
            state.asked.remove(chat);
        }
        let current = state.asked.get(chat).cloned();
        if published {
            // The reminder is no longer needed. Withdraw it if it is still queued.
            let supersede = current
                .filter(|ask| !ask.dispatching && !ask.running)
                .map(|ask| ask.message_id);
            return Verdict::Published { supersede };
        }
        if current.is_some() {
            return Verdict::Outstanding;
        }
        let message_id = Self::reserve(&mut state, chat, ended.baseline);
        Verdict::Nudge { baseline: ended.baseline, message_id }
    }

    fn reserve(state: &mut State, chat_id: &str, baseline: u32) -> String {
        let message_id = crate::new_id();
        state.asked.insert(
            chat_id.to_owned(),
            Ask { baseline, message_id: message_id.clone(), dispatching: false, running: false, restores: 0 },
        );
        message_id
    }

    /// Reserve the chat's reminder directly, as `conclude` does when a turn
    /// owes a revision. For tests that drive delivery without a turn to owe it.
    #[doc(hidden)]
    pub fn reserve_reminder(&self, chat_id: &str, baseline: u32) -> String {
        Self::reserve(&mut self.state(), chat_id, baseline)
    }

    /// A reminder withdrawn from the queue because a publication made it
    /// unnecessary: it is settled. `removed` is false when it had already left
    /// the queue, in which case it stays reserved until its own turn ends.
    pub fn supersede_done(&self, chat_id: &str, message_id: &str, removed: bool) {
        let mut state = self.state();
        if removed && state.asked.get(chat_id).is_some_and(|ask| ask.message_id == message_id) {
            state.asked.remove(chat_id);
        }
    }

    /// The reminder could not be queued: release it so the next owing turn asks.
    pub fn ask_failed(&self, chat_id: &str, message_id: &str) {
        let mut state = self.state();
        if state.asked.get(chat_id).is_some_and(|ask| ask.message_id == message_id) {
            state.asked.remove(chat_id);
        }
    }

    /// The queue row `message_id` is being taken for dispatch. If it is the
    /// reminder, it is in flight, not cancelled, until the dispatch settles.
    pub fn mark_dispatching(&self, chat_id: &str, message_id: &str) {
        if let Some(ask) = self.state().asked.get_mut(chat_id).filter(|a| a.message_id == message_id) {
            ask.dispatching = true;
        }
    }

    /// The dispatch of `message_id` failed and the row went back to the queue.
    pub fn dispatch_failed(&self, chat_id: &str, message_id: &str) {
        let mut state = self.state();
        if let Some(ask) = state.asked.get_mut(chat_id).filter(|a| a.message_id == message_id) {
            ask.dispatching = false;
            ask.restores += 1;
        }
    }

    /// How many times the chat's reminder failed to dispatch and was put back
    /// in its queue (tests).
    #[doc(hidden)]
    pub fn reminder_restores(&self, chat_id: &str) -> u32 {
        self.state().asked.get(chat_id).map_or(0, |ask| ask.restores)
    }

    /// The dispatch of `message_id` failed and its row could not be put back:
    /// the reminder is lost, so it is released and the next owing turn asks.
    pub fn reminder_lost(&self, chat_id: &str, message_id: &str) {
        let mut state = self.state();
        if state.asked.get(chat_id).is_some_and(|a| a.message_id == message_id) {
            state.asked.remove(chat_id);
        }
    }

    /// Every chat the tracker holds state for.
    pub fn tracked_chat_ids(&self) -> Vec<String> {
        let state = self.state();
        let mut ids: HashSet<&String> = HashSet::new();
        ids.extend(state.seq.keys());
        ids.extend(state.turns.keys());
        ids.extend(state.pending.keys());
        ids.extend(state.asked.keys());
        ids.extend(state.held.keys());
        ids.into_iter().cloned().collect()
    }

    /// Chats remembered as deleted.
    pub fn gone_chat_ids(&self) -> Vec<String> {
        self.state().gone.iter().cloned().collect()
    }

    /// The chat is confirmed deleted: forget its numbers, reservations,
    /// obligation and reminder, and remember it so that a late report about it
    /// creates nothing.
    pub fn forget_chat(&self, chat_id: &str) {
        let mut state = self.state();
        state.turns.remove(chat_id);
        state.seq.remove(chat_id);
        state.closed.remove(chat_id);
        state.pending.remove(chat_id);
        state.asked.remove(chat_id);
        state.held.remove(chat_id);
        state.ended.retain(|e| e.chat_id != chat_id);
        state.inflight.retain(|(chat, _), _| chat != chat_id);
        state.closed_turns.retain(|(chat, _), _| chat != chat_id);
        if state.gone.insert(chat_id.to_owned()) {
            state.gone_order.push_back(chat_id.to_owned());
        }
        while state.gone_order.len() > MAX_GONE {
            if let Some(oldest) = state.gone_order.pop_front() {
                state.gone.remove(&oldest);
            }
        }
    }

    /// The chat exists after all (it is listed again): a chat again.
    pub fn unforget_chat(&self, chat_id: &str) {
        let mut state = self.state();
        if state.gone.remove(chat_id) {
            state.gone_order.retain(|c| c != chat_id);
        }
    }

    /// How many chats the tracker holds state for (tests).
    pub fn tracked_chats(&self) -> usize {
        let state = self.state();
        state.seq.len()
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

/// Where a turn on this chat belongs, how far its project's record has got and
/// whether the chat holds the role: the project and the claim come from one
/// registry snapshot, so adoption cannot land between them.
pub fn run_basis(workspace: &WorkspaceHost, chat_id: &str) -> RunBasis {
    let Some((space, item, claim)) = workspace.manager_scope(chat_id) else {
        return RunBasis::default();
    };
    RunBasis {
        revision: status_revision(workspace, &space, &item),
        holds: claim == ManagerClaim::Holds,
        project: Some((space, item)),
    }
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

/// Where the queued message stands in its chat's real queue. A queue that
/// cannot be read is `Unknown`, never mistaken for the row having left it.
pub(crate) fn queue_row(doc_host: &DocHost, chat_id: &str, message_id: &str) -> QueueRow {
    match doc_host.open(chat_id).ok().and_then(|handle| handle.doc().read_queue().ok()) {
        Some(queue) if queue.iter().any(|row| row.id == message_id) => QueueRow::Queued,
        Some(_) => QueueRow::Absent,
        None => QueueRow::Unknown,
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

/// Install the probes and watch for turns that ended owing a revision.
pub fn spawn_watch(tokens: Arc<AgentTokens>, workspace: WorkspaceHost, doc_host: DocHost) -> ManagerStatusWatch {
    let probe_workspace = workspace.clone();
    tokens
        .manager_status()
        .set_probe(Arc::new(move |chat_id| run_basis(&probe_workspace, chat_id)));
    let probe_docs = doc_host.clone();
    tokens
        .manager_status()
        .set_queue_probe(Arc::new(move |chat_id, message_id| queue_row(&probe_docs, chat_id, message_id)));
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let chat_changes = workspace.watch_chats();
    let handle = tokio::spawn(async move {
        let tracker = tokens.manager_status();
        let wake = tracker.waker();
        let mut chats = chat_changes;
        loop {
            // A holder's finished turns are checked once.
            for ended in tracker.take_ended() {
                if stop.is_cancelled() {
                    return;
                }
                match check(&workspace, &ended) {
                    Check::Drop => {}
                    Check::Wait => tracker.hold(ended),
                    Check::Ready { published } => {
                        if !conclude_and_remind(tracker, &doc_host, &ended, published, &stop) {
                            return;
                        }
                    }
                }
            }
            // A chat about to hold the role has one obligation, checked in
            // place: it is taken only once the chat holds the role, with
            // everything it has absorbed by then, and not before.
            let mut waiting = false;
            for chat_id in tracker.held_chats() {
                let Some(held) = tracker.held(&chat_id) else {
                    continue;
                };
                match check(&workspace, &held) {
                    Check::Drop => tracker.discard_held(&chat_id),
                    Check::Wait => waiting = true,
                    Check::Ready { .. } => {
                        let Some(taken) = tracker.take_held(&chat_id) else {
                            continue;
                        };
                        if !conclude_and_remind(tracker, &doc_host, &taken, false, &stop) {
                            return;
                        }
                    }
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
                changed = chats.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    // A chat the list does not show is not necessarily gone (the
                    // list may be older than the chat): only one the registry
                    // confirms absent is forgotten, and a listed one is a chat.
                    let live: HashSet<String> = chats.borrow_and_update().iter().map(|c| c.id.clone()).collect();
                    for chat_id in tracker.gone_chat_ids() {
                        if live.contains(&chat_id) {
                            tracker.unforget_chat(&chat_id);
                        }
                    }
                    for chat_id in tracker.tracked_chat_ids() {
                        if !live.contains(&chat_id) && matches!(workspace.chat(&chat_id), Ok(None)) {
                            tracker.forget_chat(&chat_id);
                        }
                    }
                }
                _ = retry => {}
            }
        }
    });
    ManagerStatusWatch { cancel, handle }
}

/// Conclude a checked turn and queue the one reminder it earns, or withdraw the
/// one a publication made unnecessary. `false` when the watcher was stopped and
/// must end.
fn conclude_and_remind(
    tracker: &StatusTracker,
    doc_host: &DocHost,
    ended: &EndedRun,
    published: bool,
    stop: &CancellationToken,
) -> bool {
    let chat_id = &ended.chat_id;
    match tracker.conclude(ended, published) {
        Verdict::Nudge { baseline, message_id } => {
            if stop.is_cancelled() {
                tracker.ask_failed(chat_id, &message_id);
                return false;
            }
            // The id was reserved by `conclude`; the row is queued under it.
            if let Err(error) =
                doc_host.queue_context_message(chat_id, &message_id, &manager_status_nudge_text(baseline))
            {
                tracing::warn!(chat = %chat_id, %error, "manager status nudge not queued");
                tracker.ask_failed(chat_id, &message_id);
            }
        }
        Verdict::Published { supersede: Some(message_id) } => {
            let removed = doc_host.withdraw_queued(chat_id, &message_id).unwrap_or(false);
            tracker.supersede_done(chat_id, &message_id, removed);
        }
        Verdict::Missed { baseline } => {
            tracing::warn!(chat = %chat_id, baseline, role = MANAGER_STATUS_ROLE,
                "manager status still missing after one ask");
        }
        Verdict::Published { supersede: None } | Verdict::Outstanding => {}
    }
    true
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

    /// A state-changing call admitted and completed in the turn in flight.
    fn note(t: &StatusTracker, chat: &str, run: &str) {
        t.note_state_change(chat, run, t.admit(chat));
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
        t.start_turn("m");
        note(&t, "m", "r1");
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        let Verdict::Nudge { baseline: 2, message_id } = published(&t, &ended, &[]) else {
            panic!("the first owing turn is asked");
        };
        assert_eq!(t.reminder("m").as_deref(), Some(message_id.as_str()), "reserved before it is queued");
        // The ask is the turn that delivers the reminder: held to the same
        // baseline, not asked twice.
        t.reserve_turn("m", Some(&message_id));
        t.start_turn("m");
        t.finish_turn("m");
        let ask = t.take_ended().remove(0);
        assert!(ask.ask);
        assert!(matches!(published(&t, &ask, &[]), Verdict::Missed { baseline: 2 }));
        assert!(t.reminder("m").is_none(), "settled");
        // Afterwards a fresh state-changing turn can be asked again.
        set_revision(&shared, 2);
        t.start_turn("m");
        note(&t, "m", "r3");
        t.finish_turn("m");
        let next = t.take_ended().remove(0);
        assert!(!next.ask);
        assert!(matches!(published(&t, &next, &[]), Verdict::Nudge { baseline: 2, .. }));
    }

    #[test]
    fn a_turn_that_publishes_while_it_runs_is_satisfied_and_clears_the_ask() {
        let (t, shared) = tracker(true, 1);
        t.start_turn("m");
        note(&t, "m", "r1");
        set_revision(&shared, 2);
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        assert_eq!((ended.baseline, ended.end_revision), (1, 2));
        assert!(matches!(published(&t, &ended, &[2]), Verdict::Published { .. }));
    }

    #[test]
    fn consecutive_turns_never_share_a_checkpoint() {
        let (t, shared) = tracker(true, 1);
        // Turn A publishes revision 2, then turn B starts at once, before
        // anything has looked at A's end.
        t.start_turn("m");
        note(&t, "m", "a");
        set_revision(&shared, 2);
        t.finish_turn("m");
        t.start_turn("m");
        note(&t, "m", "b");
        t.finish_turn("m");
        let ended = t.take_ended();
        assert_eq!(ended.len(), 2);
        let (a, b) = (&ended[0], &ended[1]);
        assert_eq!((a.baseline, a.end_revision), (1, 2));
        assert_eq!((b.baseline, b.end_revision), (2, 2));
        // The only stamped revision is A's. It satisfies A, not B.
        assert!(matches!(published(&t, a, &[2]), Verdict::Published { .. }));
        assert!(matches!(published(&t, b, &[2]), Verdict::Nudge { baseline: 2, .. }));
    }

    #[test]
    fn a_later_turns_publication_does_not_satisfy_an_earlier_one() {
        let (t, shared) = tracker(true, 1);
        t.start_turn("m");
        note(&t, "m", "a");
        t.finish_turn("m"); // A ended at revision 1, published nothing
        set_revision(&shared, 2);
        t.start_turn("m");
        set_revision(&shared, 3); // B publishes revisions 3
        t.finish_turn("m");
        let a = t.take_ended().remove(0);
        assert_eq!((a.baseline, a.end_revision), (1, 1));
        assert!(matches!(published(&t, &a, &[3]), Verdict::Nudge { baseline: 1, .. }));
    }

    #[test]
    fn an_existing_record_seeds_the_baseline() {
        let (t, _shared) = tracker(true, 3);
        t.start_turn("m");
        assert_eq!(t.current_run("m"), Some((3, ("s".to_owned(), "proj".to_owned()))));
        note(&t, "m", "r1");
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        // The earlier revisions 1..=3 cannot satisfy the turn.
        assert!(matches!(published(&t, &ended, &[1, 2, 3]), Verdict::Nudge { baseline: 3, .. }));
    }

    #[test]
    fn a_state_change_opens_the_run_when_its_start_was_not_seen() {
        let (t, _shared) = tracker(true, 4);
        note(&t, "m", "r1");
        assert!(t.turn_open("m"));
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        assert_eq!(ended.baseline, 4);
    }

    #[test]
    fn a_run_that_never_reported_its_end_is_closed_by_the_next_start() {
        let (t, shared) = tracker(true, 1);
        t.start_turn("m");
        note(&t, "m", "a");
        set_revision(&shared, 5);
        t.start_turn("m");
        let ended = t.take_ended();
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].baseline, ended[0].end_revision), (1, 5));
        assert_eq!(t.current_run("m").unwrap().0, 5);
    }

    #[test]
    fn a_turn_without_a_state_change_or_a_project_owes_nothing() {
        let (t, _s) = tracker(true, 0);
        t.start_turn("m");
        t.finish_turn("m");
        assert!(t.take_ended().is_empty());
        let (t, _s) = tracker(false, 0);
        t.start_turn("w");
        note(&t, "w", "r1");
        assert!(!t.turn_open("w"));
        t.finish_turn("w");
        assert!(t.take_ended().is_empty());
        // Without a probe nothing is tracked at all.
        let bare = StatusTracker::default();
        note(&bare, "m", "r1");
        assert!(!bare.turn_open("m"));
    }

    #[test]
    fn a_warm_session_ends_each_turn_at_the_session_boundary_not_the_token() {
        let (t, shared) = tracker(true, 1);
        // One token, `tok`, serves two turns of a parked session.
        t.start_turn("m"); // turn one: the session turns active
        note(&t, "m", "tok");
        t.finish_turn("m"); // it parks: the turn is over, the token lives on
        set_revision(&shared, 2); // an unrelated publication between the turns
        t.start_turn("m"); // turn two: a prompt steered into the parked run
        note(&t, "m", "tok");
        t.finish_turn("m");
        t.end_run("m", "tok"); // the run finally ends: nothing left to close
        let ended = t.take_ended();
        assert_eq!(ended.len(), 2, "two turns, not one interval");
        assert_eq!((ended[0].baseline, ended[0].end_revision), (1, 1));
        assert_eq!((ended[1].baseline, ended[1].end_revision), (2, 2));
        // The publication between them satisfies neither turn's window.
        assert!(matches!(published(&t, &ended[0], &[2]), Verdict::Nudge { baseline: 1, .. }));
    }

    #[test]
    fn a_runs_end_closes_only_the_turn_that_run_was_in() {
        let (t, _s) = tracker(true, 0);
        t.start_turn("m");
        note(&t, "m", "old-run");
        t.finish_turn("m");
        t.start_turn("m"); // the next turn belongs to a newer run
        note(&t, "m", "new-run");
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
            t.start_turn("m");
            note(&t, "m", "tok");
            t.finish_turn("m");
            // Nothing has looked at them yet, and adoption may land at any time.
            assert_eq!(t.pending("m"), 1, "after {} turns", n + 1);
        }
        set_holds(&shared, true); // adoption lands before the watcher runs
        assert!(t.take_ended().is_empty());
        let held = t.take_held("m").expect("one obligation");
        assert!(held.forced);
        assert_eq!(held.baseline, 4 + 29);
        // A turn that ends while the chat holds the role is its own, checked on its window.
        t.start_turn("m");
        note(&t, "m", "tok");
        t.finish_turn("m");
        assert!(!t.take_ended()[0].forced);
    }

    #[test]
    fn the_adoption_obligation_survives_the_watchers_look_and_absorbs_turns_ending_meanwhile() {
        let (t, shared) = tracker(true, 4);
        set_holds(&shared, false);
        t.start_turn("m");
        note(&t, "m", "tok");
        t.finish_turn("m");
        // The watcher looks at the obligation; looking does not take it.
        let looked = t.held("m").unwrap();
        // Another turn ends before the watcher acts, then adoption lands.
        set_revision(&shared, 6);
        t.start_turn("m");
        note(&t, "m", "tok");
        t.finish_turn("m");
        set_holds(&shared, true);
        // The watcher takes it for good: it has absorbed the second turn.
        let taken = t.take_held("m").unwrap();
        assert_eq!((looked.baseline, taken.baseline), (4, 6));
        assert!(matches!(t.conclude(&taken, false), Verdict::Nudge { baseline: 6, .. }));
        assert!(t.take_held("m").is_none());
        // A straggler that ended just before adoption but is reported after it
        // finds a reminder already on its way.
        t.start_turn("m");
        note(&t, "m", "tok");
        t.finish_turn("m");
        let straggler = t.take_ended().remove(0);
        assert!(matches!(t.conclude(&straggler, false), Verdict::Outstanding));
    }

    #[test]
    fn a_late_end_of_one_turn_never_closes_the_next() {
        let (t, _s) = tracker(true, 0);
        let a = t.next_turn("m").0;
        t.begin("m", a, None);
        note(&t, "m", "tok");
        // The session publishes A idle and starts B before A's end is reported.
        let b = t.next_turn("m").0;
        t.begin("m", b, None); // closes A
        note(&t, "m", "tok"); // a call in B
        t.end("m", a); // A's late report
        assert!(t.turn_open("m"), "B is untouched");
        assert_eq!(t.take_ended().len(), 1, "A was closed once, owing");
        t.end("m", b);
        assert!(!t.turn_open("m"));
        assert_eq!(t.take_ended().len(), 1, "and B is checked on its own");
    }

    #[test]
    fn a_start_report_that_arrives_after_its_end_opens_nothing() {
        let (t, _s) = tracker(true, 0);
        let a = t.next_turn("m").0;
        t.end("m", a);
        t.begin("m", a, None);
        assert!(!t.turn_open("m"));
    }

    #[test]
    fn a_tool_call_that_beats_its_turns_start_report_keeps_what_it_owes() {
        let (t, _s) = tracker(true, 3);
        let a = t.next_turn("m").0; // the session started the turn
        note(&t, "m", "tok"); // the agent's first call lands first
        assert_eq!(t.current_run("m").unwrap().0, 3);
        t.begin("m", a, None); // the late start report finds it open
        t.finish_turn("m");
        assert_eq!(t.take_ended().len(), 1);
    }

    #[test]
    fn a_warm_prompt_whose_tool_call_beats_the_previous_turns_end_report_is_its_own_turn() {
        let (t, shared) = tracker(true, 1);
        let a = t.next_turn("m").0;
        t.begin("m", a, None);
        note(&t, "m", "tok");
        // A settled and the next prompt was steered in; B's first call arrives
        // before either report does.
        set_revision(&shared, 2);
        t.reserve_turn("m", Some("steer-b")); // reserved before the agent sees the prompt
        note(&t, "m", "tok");
        let b = t.next_turn("m").0;
        assert_eq!(t.current_run("m").unwrap().0, 2, "B starts from the revision now");
        assert_eq!(t.take_ended().len(), 1, "A was closed with its own window");
        t.end("m", a); // late
        assert!(t.turn_open("m"));
        t.end("m", b);
        assert_eq!(t.take_ended().len(), 1);
    }

    fn owed_turn(t: &StatusTracker) -> EndedRun {
        t.start_turn("m");
        note(t, "m", "tok");
        t.finish_turn("m");
        t.take_ended().remove(0)
    }

    /// A reminder reserved after an owing turn, plus the queue the tests control.
    fn reminded(revision: u32) -> (StatusTracker, Shared, Arc<Mutex<QueueRow>>, String) {
        let (t, shared) = tracker(true, revision);
        let row = Arc::new(Mutex::new(QueueRow::Queued));
        let probe = row.clone();
        t.set_queue_probe(Arc::new(move |_chat, _id| *probe.lock().unwrap()));
        let first = owed_turn(&t);
        let Verdict::Nudge { message_id, .. } = t.conclude(&first, false) else {
            panic!("asked");
        };
        (t, shared, row, message_id)
    }

    #[test]
    fn an_unrelated_prompt_does_not_consume_the_reminder() {
        let (t, _s, _row, id) = reminded(3);
        // A prompt ahead of the reminder in the queue runs first: its source is
        // some other message.
        t.reserve_turn("m", Some("user-prompt"));
        t.start_turn("m");
        note(&t, "m", "tok");
        t.finish_turn("m");
        let unrelated = t.take_ended().remove(0);
        assert!(!unrelated.ask);
        assert!(matches!(t.conclude(&unrelated, false), Verdict::Outstanding));
        // Then the reminder itself is delivered: the turn it starts is the ask.
        t.reserve_turn("m", Some(&id));
        t.start_turn("m");
        t.finish_turn("m");
        let ask = t.take_ended().remove(0);
        assert!(ask.ask);
        assert_eq!(ask.baseline, 3, "held to the original turn's baseline");
        assert!(matches!(t.conclude(&ask, false), Verdict::Missed { baseline: 3 }));
        // Only that one turn was the ask.
        assert!(!owed_turn(&t).ask);
    }

    #[test]
    fn a_publication_while_the_reminder_is_queued_withdraws_it_and_leaves_no_duplicate() {
        let (t, _s, _row, id) = reminded(3);
        // An unrelated turn publishes before the reminder is delivered.
        let other = owed_turn(&t);
        let Verdict::Published { supersede: Some(withdrawn) } = t.conclude(&other, true) else {
            panic!("the queued reminder is no longer needed");
        };
        assert_eq!(withdrawn, id);
        // The reservation stays until the exact message settles.
        assert_eq!(t.reminder("m").as_deref(), Some(id.as_str()));
        // A later owing turn meanwhile is not asked a second time.
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // The withdrawal succeeded: the reminder is settled.
        t.supersede_done("m", &id, true);
        assert!(t.reminder("m").is_none());
        // If it had already left the queue, the reservation would stay.
        let (t, _s, _row, id) = reminded(3);
        let other = owed_turn(&t);
        assert!(matches!(t.conclude(&other, true), Verdict::Published { supersede: Some(_) }));
        t.supersede_done("m", &id, false);
        assert_eq!(t.reminder("m").as_deref(), Some(id.as_str()));
    }

    #[test]
    fn a_cancelled_reminder_is_released_but_one_in_flight_or_unreadable_is_not() {
        // Cancelled: gone from the queue and never dispatched.
        let (t, _s, row, id) = reminded(3);
        *row.lock().unwrap() = QueueRow::Absent;
        let next = owed_turn(&t);
        let Verdict::Nudge { message_id, .. } = t.conclude(&next, false) else {
            panic!("a cancelled reminder is asked again");
        };
        assert_ne!(message_id, id);
        // A queue that cannot be read says nothing: still outstanding.
        let (t, _s, row, _id) = reminded(3);
        *row.lock().unwrap() = QueueRow::Unknown;
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // Taken for dispatch: absent from the queue but in flight, not cancelled.
        let (t, _s, row, id) = reminded(3);
        t.mark_dispatching("m", &id);
        *row.lock().unwrap() = QueueRow::Absent;
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // The dispatch fails and the row goes back: queued again, still reserved.
        t.dispatch_failed("m", &id);
        *row.lock().unwrap() = QueueRow::Queued;
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // Retried: delivered by the exact message, it is the ask.
        t.reserve_turn("m", Some(&id));
        t.start_turn("m");
        t.finish_turn("m");
        assert!(t.take_ended().remove(0).ask);
    }

    #[test]
    fn a_prompt_reserved_before_the_agent_sees_it_has_its_turn_when_the_first_call_lands() {
        let (t, _s) = tracker(true, 3);
        let a = t.start_turn("m");
        t.finish_turn("m"); // the session parked after turn A
        // The next prompt is steered into the parked run: reserved first.
        t.reserve_turn("m", Some("steer-1"));
        let admitted = t.admit("m");
        assert!(admitted > a, "the arriving prompt's turn, not the one that ended");
        // The agent's first call is admitted and completes before the session
        // reports the turn started.
        t.note_state_change("m", "tok", admitted);
        assert!(t.turn_open("m"));
        // The session then numbers the turn: the reserved number, same source.
        let (seq, source) = t.next_turn("m");
        assert_eq!((seq, source.as_deref()), (admitted, Some("steer-1")));
        t.begin("m", seq, source.as_deref());
        t.end("m", seq);
        assert_eq!(t.take_ended().len(), 1, "the call was attributed to its own turn");
        // A reservation that is released leaves nothing behind.
        t.reserve_turn("m", Some("steer-2"));
        t.release_turn("m", "steer-2");
        let (_, source) = t.next_turn("m");
        assert_eq!(source, None);
    }

    #[test]
    fn a_calls_completion_belongs_to_the_turn_it_was_admitted_in() {
        let (t, _s) = tracker(true, 0);
        let a = t.next_turn("m").0;
        t.begin("m", a, None);
        let admitted = t.admit_call("m");
        assert_eq!(admitted, a);
        // The call is slow: turn A ends and turn B starts while it runs.
        t.end("m", a);
        let b = t.next_turn("m").0;
        t.begin("m", b, None);
        // Its completion is settled against A, which is over: A owes after all,
        // and B owes nothing for it.
        t.complete_call("m", "tok", admitted, true);
        assert_eq!(t.take_ended().len(), 1, "A's call is not lost");
        t.end("m", b);
        assert!(t.take_ended().is_empty(), "B owes nothing for A's call");
    }

    #[test]
    fn a_late_call_is_judged_on_its_turns_own_window_however_many_turns_have_ended_since() {
        let (t, shared) = tracker(true, 1);
        let a = t.next_turn("m").0;
        t.begin("m", a, None);
        let admitted = t.admit_call("m"); // A's mutation starts and is slow
        // A ends at revision 1. Then turn B publishes revision 2 and ends; then
        // turn C starts and ends too. A's record must survive all of them.
        t.end("m", a);
        set_revision(&shared, 2);
        for _ in 0..2 {
            let turn = t.next_turn("m").0;
            t.begin("m", turn, None);
            t.end("m", turn);
        }
        // A's mutation finally commits.
        t.complete_call("m", "tok", admitted, true);
        let late = t.take_ended().remove(0);
        assert_eq!((late.baseline, late.end_revision), (1, 1), "A's end, not the revision now");
        // B's publication (revision 2) is outside A's window: A is not satisfied.
        assert!(matches!(published(&t, &late, &[2]), Verdict::Nudge { baseline: 1, .. }));
        // The record is gone once the call has settled: nothing is kept for ever.
        assert!(t.state().closed_turns.is_empty() && t.state().inflight.is_empty());
    }

    #[test]
    fn a_turn_record_is_kept_only_while_a_call_admitted_into_it_is_running() {
        let (t, _s) = tracker(true, 0);
        let a = t.next_turn("m").0;
        t.begin("m", a, None);
        t.end("m", a); // no call was admitted: nothing is kept
        assert!(t.state().closed_turns.is_empty());
        let b = t.next_turn("m").0;
        t.begin("m", b, None);
        let call = t.admit_call("m");
        t.end("m", b);
        assert_eq!(t.state().closed_turns.len(), 1);
        // The call fails: the turn owes nothing, and the record is released.
        t.complete_call("m", "tok", call, false);
        assert!(t.state().closed_turns.is_empty() && t.take_ended().is_empty());
    }

    #[test]
    fn a_released_number_is_never_issued_again() {
        let (t, _s) = tracker(true, 0);
        let a = t.start_turn("m");
        t.reserve_turn("m", Some("lost-delivery"));
        let admitted = t.admit_call("m"); // the agent saw the prompt and called a tool
        assert!(admitted > a);
        t.release_turn("m", "lost-delivery"); // then the delivery's cleanup ran
        t.reserve_turn("m", Some("next"));
        let (next, _) = t.next_turn("m");
        assert!(next > admitted, "the number the call holds is not given to another turn");
        // Calls in the turn still running are not attributed to the released number.
        t.complete_call("m", "tok", admitted, false);
        assert_eq!(t.admit("m"), next, "the latest live turn");
    }

    #[test]
    fn an_early_steer_into_a_numbered_turn_binds_to_it_until_its_start_is_reported() {
        let (t, _s, _row, id) = reminded(3);
        // Working is visible: the session numbered the turn, but its start has
        // not been reported yet. The reminder is steered into it in that gap.
        let (b, source) = t.next_turn("m");
        t.joined_active("m", &id);
        // The start report arrives: the turn claims the reminder.
        t.begin("m", b, source.as_deref());
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        assert!(ended.ask, "the turn that took the reminder is the ask");
        assert!(matches!(t.conclude(&ended, false), Verdict::Missed { .. }));
        assert!(t.reminder("m").is_none(), "not stranded");
    }

    #[test]
    fn a_steer_into_no_turn_at_all_marks_nothing() {
        let (t, _s, _row, id) = reminded(3);
        t.joined_active("m", &id); // no turn open, none numbered
        // The reminder is still just queued: a later owing turn is not asked again.
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        assert_eq!(t.reminder("m").as_deref(), Some(id.as_str()));
    }

    #[test]
    fn state_for_chats_that_no_longer_exist_is_reclaimed_and_late_reports_do_not_bring_it_back() {
        let (t, _s) = tracker(true, 0);
        for chat in ["gone", "kept"] {
            let a = t.next_turn(chat).0;
            t.begin(chat, a, None);
            t.note_state_change(chat, "tok", a);
        }
        t.reserve_turn("gone", Some("m1"));
        assert_eq!(t.tracked_chats(), 2);
        t.forget_chat("gone");
        assert_eq!(t.tracked_chats(), 1);
        assert!(!t.turn_open("gone") && t.turn_open("kept"));
        // A late end for the deleted chat does not recreate its state.
        t.end("gone", 1);
        assert_eq!(t.tracked_chats(), 1);
    }

    #[test]
    fn a_failed_ask_lets_the_next_turn_ask_again() {
        let (t, _s) = tracker(true, 0);
        t.start_turn("m");
        note(&t, "m", "r1");
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        assert!(matches!(published(&t, &ended, &[]), Verdict::Nudge { baseline: 0, .. }));
        let id = t.reminder("m").expect("reserved");
        t.ask_failed("m", &id);
        assert!(t.reminder("m").is_none());
        t.start_turn("m");
        assert!(!t.state().turns["m"].ask);
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
    /// The tracker against a chat's real queue: a publication withdraws a
    /// queued reminder, a cancellation releases it, and a row in flight is
    /// neither.
    #[tokio::test]
    async fn the_reminder_follows_the_real_queue() {
        let dir = tempfile::tempdir().unwrap();
        let host = DocHost::new(
            Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap()),
            crate::DocHostConfig {
                device_id: "dev-test".into(),
                default_harness: mast_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let (t, _s) = tracker(true, 3);
        let probe_host = host.clone();
        t.set_queue_probe(Arc::new(move |chat, id| queue_row(&probe_host, chat, id)));
        let remind = |t: &StatusTracker| {
            let ended = owed_turn(t);
            let Verdict::Nudge { message_id, .. } = t.conclude(&ended, false) else {
                panic!("asked");
            };
            host.queue_context_message("m", &message_id, "remind").unwrap();
            message_id
        };

        // A publication while the row is queued withdraws it for real.
        let id = remind(&t);
        assert_eq!(queue_row(&host, "m", &id), QueueRow::Queued);
        let other = owed_turn(&t);
        let Verdict::Published { supersede: Some(withdrawn) } = t.conclude(&other, true) else {
            panic!("withdraw");
        };
        assert!(host.withdraw_queued("m", &withdrawn).unwrap());
        t.supersede_done("m", &withdrawn, true);
        assert_eq!(queue_row(&host, "m", &id), QueueRow::Absent);
        assert!(t.reminder("m").is_none());
        assert!(!host.withdraw_queued("m", &id).unwrap(), "nothing left to withdraw");

        // Cancelled by someone else: released, and asked again, once.
        let id = remind(&t);
        let handle = host.open("m").unwrap();
        assert!(handle.doc().take_queued(&id).unwrap().is_some());
        let Verdict::Nudge { message_id: again, .. } = t.conclude(&owed_turn(&t), false) else {
            panic!("a cancelled reminder is asked again");
        };
        assert_ne!(again, id);

        // Taken for dispatch: absent from the queue, in flight, not cancelled.
        host.queue_context_message("m", &again, "remind").unwrap();
        t.mark_dispatching("m", &again);
        assert!(handle.doc().take_queued(&again).unwrap().is_some());
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // The dispatch fails and the row is restored: still the same reminder.
        t.dispatch_failed("m", &again);
        host.queue_context_message("m", &again, "remind").unwrap();
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Outstanding));
        // Retried and delivered by the exact message: it is the ask.
        t.mark_dispatching("m", &again);
        assert!(handle.doc().take_queued(&again).unwrap().is_some());
        t.reserve_turn("m", Some(&again));
        t.start_turn("m");
        t.finish_turn("m");
        assert!(t.take_ended().remove(0).ask);
    }
    #[test]
    fn a_call_between_the_session_taking_a_number_and_reporting_the_turn_is_in_the_new_turn() {
        let (t, _s) = tracker(true, 1);
        let a = t.start_turn("m"); // turn A is open
        // A settled and the next prompt was steered in: its number is reserved,
        // then taken by the session when the status turns active...
        t.reserve_turn("m", Some("steer-b"));
        let (b, source) = t.next_turn("m");
        assert!(b > a);
        // ...and before the start is reported, with A's end not reported either,
        // the agent's first call is admitted and completes.
        let admitted = t.admit("m");
        assert_eq!(admitted, b, "the number already taken, not the open turn A");
        t.note_state_change("m", "tok", admitted);
        assert_eq!(t.take_ended().len(), 0, "A owed nothing");
        // The late start report finds the turn open with its source.
        t.begin("m", b, source.as_deref());
        t.finish_turn("m");
        assert_eq!(t.take_ended().len(), 1, "the call was owed by B");
    }

    #[test]
    fn a_released_reservation_gives_its_number_back_so_no_call_is_admitted_into_a_phantom_turn() {
        let (t, _s) = tracker(true, 0);
        let a = t.start_turn("m");
        t.reserve_turn("m", Some("never-delivered"));
        assert!(t.admit("m") > a);
        t.release_turn("m", "never-delivered");
        assert_eq!(t.admit("m"), a, "calls in the turn still running belong to it");
        // A number the session took is not released: it stays for its start report.
        t.reserve_turn("m", Some("taken"));
        let (taken, _) = t.next_turn("m");
        t.release_turn("m", "taken");
        assert_eq!(t.admit("m"), taken);
    }

    #[test]
    fn a_reminder_steered_into_a_turn_already_running_is_settled_by_that_turn() {
        let (t, _s, _row, id) = reminded(3);
        t.start_turn("m"); // an ordinary turn is running
        t.joined_active("m", &id);
        t.finish_turn("m");
        let ended = t.take_ended().remove(0);
        assert!(ended.ask, "the turn that took the reminder is the ask");
        assert_eq!(ended.baseline, 3);
        assert!(matches!(t.conclude(&ended, false), Verdict::Missed { baseline: 3 }));
        assert!(t.reminder("m").is_none(), "not stranded");
        // Another message joining a turn is nothing to the tracker.
        t.start_turn("m");
        t.joined_active("m", "some-other-message");
        t.finish_turn("m");
        assert!(t.take_ended().is_empty());
    }

    #[test]
    fn a_reminder_whose_row_could_not_be_restored_is_released() {
        let (t, _s, _row, id) = reminded(3);
        t.mark_dispatching("m", &id);
        t.reminder_lost("m", &id);
        assert!(t.reminder("m").is_none());
        // The next owing turn asks again.
        assert!(matches!(t.conclude(&owed_turn(&t), false), Verdict::Nudge { .. }));
        // A different message's loss changes nothing.
        let current = t.reminder("m").unwrap();
        t.reminder_lost("m", "unrelated");
        assert_eq!(t.reminder("m"), Some(current));
    }

    #[test]
    fn nothing_is_recreated_for_a_deleted_chat_by_a_late_start_or_reservation() {
        let (t, _s) = tracker(true, 0);
        let a = t.next_turn("gone").0;
        t.begin("gone", a, None);
        assert_eq!(t.tracked_chats(), 1);
        t.forget_chat("gone");
        assert_eq!(t.tracked_chats(), 0);
        t.reserve_turn("gone", Some("late"));
        let (seq, _) = t.next_turn("gone");
        t.begin("gone", 7, None);
        t.note_state_change("gone", "tok", 7);
        t.end("gone", 7);
        assert_eq!((seq, t.tracked_chats()), (0, 0), "no counters, no turn");
        assert!(!t.turn_open("gone"));
        // A chat that turns out to exist after all is a chat again.
        t.unforget_chat("gone");
        assert!(t.start_turn("gone") > 0);
    }
}
