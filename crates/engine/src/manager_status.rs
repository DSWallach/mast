//! Tracks changes and publications for a project's Manager status record.
//! Automatic reminders are temporarily disabled; the watcher is not started.
//!
//! A manager's changes and its status are tied together per chat, by the order
//! in which things actually completed, not by turns:
//!
//! - every successful state-changing tool call of the chat advances its
//!   generation (`gen`);
//! - a manager-status publish that actually inserts a new Manager-stamped
//!   revision covers every change that had completed before the publish was
//!   admitted (`covered` is the generation captured at that admission);
//! - the status is owed while `gen` is beyond what is covered, and beyond what a
//!   reminder has already been issued for (`asked_through`).
//!
//! So one update can cover several turns, a checkpoint made before a change
//! never covers it, and a change completing while a publish is in flight stays
//! owed. When the chat is idle and owes, the watcher queues one reminder, for
//! the generation then reached. A reminder whose turn ends without covering it
//! is given up for that generation only; a newer change earns a new one.
//!
//! Only the latest inserted revision counts: readers see it, so its own
//! admission point is the watermark, even if an earlier-admitted publish lands
//! after a later-admitted one. Receipts for superseded revisions are ignored.
//!
//! A Manager's chat holds the role only after its first turn is queued, and a
//! successor only after its hand-off completes. Their changes stay owed, with
//! nothing sent, until the chat holds the role; they are forgiven when it never
//! will. Only the chat that holds the role is ever reminded.
//!
//! Locks: the tracker's state is never held across a workspace or queue read,
//! and every report it takes (a completed call, a closed turn, a confirmed
//! delivery) is in-memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mast_proto::StandingRoleKind;
use mast_proto::artifacts::ArtifactView;
use mast_proto::manager_status_nudge_text;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::agent_tools::AgentTokens;
use crate::briefing::BriefingKind;
use crate::sessions::SessionsEngine;
use crate::workspace_host::ManagerClaim;
use crate::{DocHost, WorkspaceHost};

/// Agent tools that change project state. A manager turn that calls one owes
/// a status revision. Creating a child chat is how a worker is dispatched.
pub const STATE_CHANGING_TOOLS: [&str; 11] = [
    mast_proto::agent_tools::tools::CREATE_CHILD,
    mast_proto::agent_tools::tools::SET_STATUS,
    mast_proto::agent_tools::tools::CREATE_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::RESOLVE_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::ANSWER_USER_ACTION_REQUEST,
    mast_proto::agent_tools::tools::APPLY_CONTAINER_TEMPLATE,
    mast_proto::agent_tools::tools::QUEUE_CHILD_CHAT_TURN,
    mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT,
    mast_proto::agent_tools::tools::CLAIM_CHILD_CHAT,
    mast_proto::agent_tools::tools::CHAT_ARCHIVE,
    mast_proto::agent_tools::tools::PROPOSE_INSTRUCTION,
];

pub fn is_state_changing(tool: &str) -> bool {
    STATE_CHANGING_TOOLS.contains(&tool)
}

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

/// How often chats waiting to hold the role are looked at again when nothing
/// wakes the watcher. Adoption normally wakes it through the chat change; this
/// is only the net under changes that touch the role alone.
const DEFAULT_RECHECK: Duration = Duration::from_secs(30);

/// The reminder reserved for a chat. Its message id is chosen before the
/// reminder exists, so no delivery can outrun the record of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ask {
    /// The generation the reminder was issued to cover.
    target: u64,
    message_id: String,
    /// The queue row has been taken for dispatch and has not settled either way.
    dispatching: bool,
    /// The agent received the reminder, at its Steered boundary or a fresh run.
    delivered: bool,
    /// How many times a dispatch of the reminder failed and its row was restored.
    restores: u32,
}

#[derive(Debug, Default)]
struct ChatState {
    /// Successful state-changing completions, never reset.
    generation: u64,
    /// The latest Manager-stamped revision this chat inserted, and the
    /// generation captured when its publish was admitted.
    latest: Option<(u32, u64)>,
    /// Generations up to here already had a reminder given up on.
    asked_through: u64,
    ask: Option<Ask>,
}

impl ChatState {
    fn covered(&self) -> u64 {
        self.latest.map_or(0, |(_, generation)| generation)
    }

    fn owed(&self) -> bool {
        self.generation > self.covered().max(self.asked_through)
    }
}

/// What the watcher should do for a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eval {
    Nothing,
    /// Queue a reminder under this reserved message id.
    Remind { message_id: String, target: u64 },
    /// A publication made the queued reminder unnecessary: take it back out.
    Withdraw { message_id: String },
}

#[derive(Default)]
pub struct StatusTracker {
    chats: Mutex<HashMap<String, ChatState>>,
    queue: Mutex<Option<QueueProbe>>,
    wake: Arc<Notify>,
    recheck: Mutex<Option<Duration>>,
    test_hook: Mutex<Option<Arc<dyn Fn(&str, &str) -> bool + Send + Sync>>>,
}

impl StatusTracker {
    /// Install a deterministic lifecycle hook for integration tests.
    #[doc(hidden)]
    pub fn set_test_hook(&self, hook: Option<Arc<dyn Fn(&str, &str) -> bool + Send + Sync>>) {
        *self.test_hook.lock().unwrap_or_else(|e| e.into_inner()) = hook;
    }

    #[doc(hidden)]
    pub fn drain_test_wake(&self) {
        use futures::FutureExt;
        let _ = self.wake.notified().now_or_never();
    }

    pub(crate) fn test_hook(&self, event: &str, chat_id: &str) -> bool {
        let hook = self.test_hook.lock().unwrap_or_else(|e| e.into_inner()).clone();
        hook.is_some_and(|hook| hook(event, chat_id))
    }

    /// How the tracker looks up a queued message in its chat's queue.
    pub fn set_queue_probe(&self, probe: QueueProbe) {
        *self.queue.lock().unwrap_or_else(|e| e.into_inner()) = Some(probe);
    }

    /// Override how often waiting chats are looked at again (tests).
    pub fn set_recheck_interval(&self, interval: Duration) {
        *self.recheck.lock().unwrap_or_else(|e| e.into_inner()) = Some(interval);
    }

    fn recheck_interval(&self) -> Duration {
        self.recheck
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(DEFAULT_RECHECK)
    }

    fn queue_row(&self, chat_id: &str, message_id: &str) -> QueueRow {
        let probe = self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone();
        probe.map_or(QueueRow::Unknown, |probe| probe(chat_id, message_id))
    }

    fn chats(&self) -> std::sync::MutexGuard<'_, HashMap<String, ChatState>> {
        self.chats.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A state-changing tool call of the chat completed successfully.
    pub fn changed(&self, chat_id: &str) {
        self.chats().entry(chat_id.to_owned()).or_default().generation += 1;
        self.wake.notify_one();
    }

    /// The generation reached so far: what a publish being admitted now covers.
    pub fn generation(&self, chat_id: &str) -> u64 {
        self.chats().get(chat_id).map_or(0, |c| c.generation)
    }

    /// A publish by this chat inserted Manager-stamped revision `revision`, and
    /// had been admitted at generation `admitted`. The latest inserted revision
    /// is the one readers see, so only it sets the watermark, and it may lower
    /// it; a receipt for an older revision, delivered late, is ignored.
    pub fn published(&self, chat_id: &str, revision: u32, admitted: u64) {
        {
            let mut chats = self.chats();
            let chat = chats.entry(chat_id.to_owned()).or_default();
            if chat.latest.is_some_and(|(latest, _)| revision <= latest) {
                return;
            }
            chat.latest = Some((revision, admitted));
        }
        self.wake.notify_one();
    }

    /// The agent received the reminder `message_id`: its Steered boundary, or
    /// the start of a fresh run. The turn now running is the one that took it.
    /// Another message, or a reminder already settled, changes nothing.
    pub fn reminder_confirmed(&self, chat_id: &str, message_id: &str) {
        if let Some(ask) = self.chats().get_mut(chat_id).and_then(|c| c.ask.as_mut())
            && ask.message_id == message_id
        {
            ask.delivered = true;
            ask.dispatching = false;
        }
    }

    /// Snapshot the delivered reminder at the session transition, under its status lock.
    pub fn delivered_reminder(&self, chat_id: &str) -> Option<String> {
        self.chats().get(chat_id).and_then(|c| c.ask.as_ref())
            .filter(|ask| ask.delivered).map(|ask| ask.message_id.clone())
    }

    /// Settle only the reminder captured when this turn closed.
    pub fn turn_closed(&self, chat_id: &str, delivered: Option<&str>) {
        {
            let mut chats = self.chats();
            if let Some(chat) = chats.get_mut(chat_id)
                && chat.ask.as_ref().is_some_and(|ask| ask.delivered && Some(ask.message_id.as_str()) == delivered)
            {
                let ask = chat.ask.take().expect("checked above");
                if chat.covered() < ask.target {
                    chat.asked_through = chat.asked_through.max(ask.target);
                    tracing::warn!(chat = %chat_id, target = ask.target,
                        "manager status still missing after one reminder");
                }
            }
        }
        self.wake.notify_one();
    }

    /// Chats with something for the watcher to look at.
    fn active_chats(&self) -> Vec<String> {
        self.chats()
            .iter()
            .filter(|(_, chat)| chat.owed() || chat.ask.is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Decide what to do for the chat. A reminder is queued only when the chat
    /// is idle; one withdrawn or released does not need it. The reminder is
    /// reserved here, under its message id, before it is queued, so that two
    /// evaluations ask once. A reminder that was not in flight and has left the
    /// queue was cancelled, and is released.
    pub fn evaluate(&self, chat_id: &str, idle: bool) -> Eval {
        let peeked = self.chats().get(chat_id).and_then(|c| c.ask.clone());
        let row = peeked
            .as_ref()
            .filter(|ask| !ask.dispatching && !ask.delivered)
            .map(|ask| self.queue_row(chat_id, &ask.message_id));
        let mut chats = self.chats();
        let Some(chat) = chats.get_mut(chat_id) else {
            return Eval::Nothing;
        };
        if let (Some(now), Some(before)) = (&chat.ask, &peeked)
            && now.message_id == before.message_id
            && row == Some(QueueRow::Absent)
            && !now.dispatching
            && !now.delivered
        {
            chat.ask = None;
        }
        if let Some(ask) = &chat.ask {
            // A publication covered what it was for: withdraw it if still queued.
            if chat.covered() >= ask.target && !ask.dispatching && !ask.delivered {
                return Eval::Withdraw { message_id: ask.message_id.clone() };
            }
            return Eval::Nothing;
        }
        if chat.owed() && idle {
            let message_id = crate::new_id();
            let target = chat.generation;
            chat.ask = Some(Ask { target, message_id: message_id.clone(), dispatching: false, delivered: false, restores: 0 });
            return Eval::Remind { message_id, target };
        }
        Eval::Nothing
    }

    /// A reminder withdrawn from the queue because a publication made it
    /// unnecessary: it is settled. `removed` is false when it had already left
    /// the queue, in which case it stays reserved until its own turn ends.
    pub fn supersede_done(&self, chat_id: &str, message_id: &str, removed: bool) {
        if removed
            && let Some(chat) = self.chats().get_mut(chat_id)
            && chat.ask.as_ref().is_some_and(|ask| ask.message_id == message_id)
        {
            chat.ask = None;
            self.wake.notify_one();
        }
    }

    /// The reminder could not be queued: release it so the next evaluation asks.
    pub fn ask_failed(&self, chat_id: &str, message_id: &str) {
        self.release(chat_id, message_id);
    }

    fn release(&self, chat_id: &str, message_id: &str) {
        if let Some(chat) = self.chats().get_mut(chat_id)
            && chat.ask.as_ref().is_some_and(|ask| ask.message_id == message_id)
        {
            chat.ask = None;
        }
    }

    /// The queue row `message_id` is being taken for dispatch. If it is the
    /// reminder, it is in flight, not cancelled, until the dispatch settles.
    pub fn mark_dispatching(&self, chat_id: &str, message_id: &str) {
        if let Some(ask) = self.chats().get_mut(chat_id).and_then(|c| c.ask.as_mut())
            && ask.message_id == message_id
        {
            ask.dispatching = true;
        }
    }

    /// The dispatch of `message_id` failed and its row went back to the queue.
    pub fn dispatch_failed(&self, chat_id: &str, message_id: &str) {
        if let Some(ask) = self.chats().get_mut(chat_id).and_then(|c| c.ask.as_mut())
            && ask.message_id == message_id
        {
            ask.dispatching = false;
            ask.restores += 1;
        }
    }

    /// The dispatch of `message_id` failed and its row could not be put back:
    /// the reminder is lost, so it is released and the next evaluation asks.
    pub fn reminder_lost(&self, chat_id: &str, message_id: &str) {
        self.release(chat_id, message_id);
        self.wake.notify_one();
    }

    /// Give up what is owed so far, and any reminder: the chat does not manage a
    /// project, so there is nothing to keep current.
    fn forgive(&self, chat_id: &str) {
        if let Some(chat) = self.chats().get_mut(chat_id) {
            chat.asked_through = chat.generation;
            chat.ask = None;
        }
    }

    /// Forget a chat the registry confirms deleted.
    pub fn forget_chat(&self, chat_id: &str) {
        self.chats().remove(chat_id);
    }

    /// Every chat the tracker holds state for.
    pub fn tracked_chat_ids(&self) -> Vec<String> {
        self.chats().keys().cloned().collect()
    }

    /// Whether the chat owes the status right now (tests).
    pub fn owes(&self, chat_id: &str) -> bool {
        self.chats().get(chat_id).is_some_and(ChatState::owed)
    }

    /// The reminder reserved for the chat, if any (tests).
    pub fn reminder(&self, chat_id: &str) -> Option<String> {
        self.chats().get(chat_id).and_then(|c| c.ask.as_ref()).map(|a| a.message_id.clone())
    }

    /// How many times the chat's reminder failed to dispatch and was put back
    /// in its queue (tests).
    #[doc(hidden)]
    pub fn reminder_restores(&self, chat_id: &str) -> u32 {
        self.chats().get(chat_id).and_then(|c| c.ask.as_ref()).map_or(0, |a| a.restores)
    }

    /// Reserve the chat's reminder directly, for tests that drive delivery
    /// without a tool call to owe it: a change is recorded as made (so the
    /// reminder is for something not yet covered) and the reminder reserved for
    /// it.
    #[doc(hidden)]
    pub fn reserve_reminder(&self, chat_id: &str) -> String {
        let mut chats = self.chats();
        let chat = chats.entry(chat_id.to_owned()).or_default();
        chat.generation += 1;
        let message_id = crate::new_id();
        chat.ask = Some(Ask {
            target: chat.generation,
            message_id: message_id.clone(),
            dispatching: false,
            delivered: false,
            restores: 0,
        });
        message_id
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

/// The project a chat manages, as `(space id, project item id)`.
pub fn managed_project(workspace: &WorkspaceHost, chat_id: &str) -> Option<(String, String)> {
    let role = workspace.standing_role_of_chat(chat_id)?;
    if role.role != StandingRoleKind::Manager {
        return None;
    }
    let space = workspace.work_item_space(&role.scope_item_id)?;
    Some((space, role.scope_item_id))
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

async fn retry_after(waiting: bool, interval: Duration) {
    if waiting {
        tokio::time::sleep(interval).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn queue_failed(tracker: &StatusTracker, chat_id: &str, message_id: &str, waiting: &mut bool) {
    tracker.ask_failed(chat_id, message_id);
    *waiting = true;
}

/// Watch for chats that owe the status and queue the one reminder each earns.
pub fn spawn_watch(
    tokens: Arc<AgentTokens>,
    workspace: WorkspaceHost,
    doc_host: DocHost,
    sessions: SessionsEngine,
) -> ManagerStatusWatch {
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
            let mut waiting = false;
            for chat_id in tracker.active_chats() {
                if stop.is_cancelled() {
                    return;
                }
                // One registry snapshot: whether the chat manages a project, or
                // is about to (a first start or a reserved hand-off successor).
                let Some((space, item, claim)) = workspace.manager_scope(&chat_id) else {
                    if matches!(workspace.chat(&chat_id), Ok(None)) {
                        tracker.forget_chat(&chat_id);
                    } else {
                        tracker.forgive(&chat_id);
                    }
                    continue;
                };
                if claim == ManagerClaim::Pending {
                    waiting = true;
                    continue;
                }
                match tracker.evaluate(&chat_id, !sessions.turn_in_flight(&chat_id)) {
                    Eval::Nothing => {}
                    Eval::Remind { message_id, target } => {
                        if stop.is_cancelled() {
                            tracker.ask_failed(&chat_id, &message_id);
                            return;
                        }
                        let latest = status_revision(&workspace, &space, &item);
                        // The id was reserved by `evaluate`; the row is queued under it.
                        if let Err(error) =
                            doc_host.queue_context_message(&chat_id, &message_id, &manager_status_nudge_text(latest))
                        {
                            tracing::warn!(chat = %chat_id, target, %error, "manager status reminder not queued");
                            queue_failed(tracker, &chat_id, &message_id, &mut waiting);
                        }
                    }
                    Eval::Withdraw { message_id } => {
                        tracker.test_hook("withdraw", &chat_id);
                        let removed = doc_host.withdraw_queued(&chat_id, &message_id).unwrap_or(false);
                        tracker.supersede_done(&chat_id, &message_id, removed);
                        if !removed {
                            waiting = true;
                        }
                    }
                }
            }
            let recheck = tracker.recheck_interval();
            let retry = retry_after(waiting, recheck);
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = wake.notified() => {}
                changed = chats.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    // A chat the list does not show is not necessarily gone (the
                    // list may be older than the chat): only one the registry
                    // confirms absent is forgotten.
                    let live: std::collections::HashSet<String> =
                        chats.borrow_and_update().iter().map(|c| c.id.clone()).collect();
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


#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "m";

    /// A tracker whose queue the test controls.
    fn tracker() -> (StatusTracker, Arc<Mutex<QueueRow>>) {
        let t = StatusTracker::default();
        let row = Arc::new(Mutex::new(QueueRow::Queued));
        let probe = row.clone();
        t.set_queue_probe(Arc::new(move |_chat, _id| *probe.lock().unwrap()));
        (t, row)
    }

    fn remind(t: &StatusTracker) -> (String, u64) {
        match t.evaluate(CHAT, true) {
            Eval::Remind { message_id, target } => (message_id, target),
            other => panic!("expected a reminder, got {other:?}"),
        }
    }

    #[test]
    fn a_change_is_owed_until_a_publish_admitted_after_it_inserts_a_revision() {
        let (t, _row) = tracker();
        assert!(!t.owes(CHAT));
        t.changed(CHAT);
        assert!(t.owes(CHAT));
        // A publish admitted after the change, which inserted a revision, covers it.
        let admitted = t.generation(CHAT);
        t.published(CHAT, 1, admitted);
        assert!(!t.owes(CHAT));
        // A checkpoint never covers a later change.
        t.changed(CHAT);
        assert!(t.owes(CHAT));
    }

    #[test]
    fn one_update_can_cover_several_turns() {
        let (t, _row) = tracker();
        for _ in 0..5 {
            t.changed(CHAT); // five turns' worth of changes, nothing published
        }
        let admitted = t.generation(CHAT);
        t.published(CHAT, 1, admitted);
        assert!(!t.owes(CHAT), "one publish covered them all");
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing);
    }

    #[test]
    fn a_change_completing_while_a_publish_is_in_flight_stays_owed() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let admitted = t.generation(CHAT); // the publish is admitted here...
        t.changed(CHAT); // ...a change completes before it returns...
        t.published(CHAT, 1, admitted); // ...and the publish commits and returns
        assert!(t.owes(CHAT), "the later change is not covered");
        // Completing before the publish was admitted is covered.
        let (t, _row) = tracker();
        t.changed(CHAT);
        t.changed(CHAT);
        t.published(CHAT, 1, t.generation(CHAT));
        assert!(!t.owes(CHAT));
    }

    #[test]
    fn only_the_latest_inserted_revision_sets_the_watermark_whatever_order_receipts_arrive() {
        // Q (admitted later, g1) inserts r1 first; stale P (admitted at g0)
        // inserts r2 after. Readers see r2, so g1 is owed again.
        let (t, _row) = tracker();
        t.changed(CHAT);
        let g0 = t.generation(CHAT);
        t.changed(CHAT);
        let g1 = t.generation(CHAT);
        t.published(CHAT, 1, g1);
        assert!(!t.owes(CHAT));
        t.published(CHAT, 2, g0);
        assert!(t.owes(CHAT), "the latest revision was admitted before the second change");
        // The same two receipts delivered the other way round: r1 is ignored.
        let (t, _row) = tracker();
        t.changed(CHAT);
        let g0 = t.generation(CHAT);
        t.changed(CHAT);
        let g1 = t.generation(CHAT);
        t.published(CHAT, 2, g0);
        t.published(CHAT, 1, g1);
        assert!(t.owes(CHAT), "a superseded revision's late receipt is ignored");
        // In commit order, with the later one admitted later: covered.
        let (t, _row) = tracker();
        t.changed(CHAT);
        let g0 = t.generation(CHAT);
        t.changed(CHAT);
        let g1 = t.generation(CHAT);
        t.published(CHAT, 1, g0);
        t.published(CHAT, 2, g1);
        assert!(!t.owes(CHAT));
    }

    #[test]
    fn a_reminder_is_asked_once_for_a_generation_and_a_missed_one_covers_only_what_it_asked() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        // Not while a turn runs.
        assert_eq!(t.evaluate(CHAT, false), Eval::Nothing);
        let (id, target) = remind(&t);
        assert_eq!(target, 1);
        assert_eq!(t.reminder(CHAT).as_deref(), Some(id.as_str()), "reserved before it is queued");
        // Outstanding: not asked again.
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing);
        // A change lands while the reminder is on its way (generation 2).
        t.changed(CHAT);
        // The reminder is delivered, its turn closes without a publish: Missed
        // for generation 1 only.
        t.reminder_confirmed(CHAT, &id);
        t.turn_closed(CHAT, t.delivered_reminder(CHAT).as_deref());
        assert!(t.reminder(CHAT).is_none());
        assert!(t.owes(CHAT), "generation 2 is newer than what was asked for");
        let (_, second) = remind(&t);
        assert_eq!(second, 2, "one new reminder, for the new generation");
        // Missed again for generation 2: nothing is owed and nothing is asked
        // twice for the same generation.
        t.reminder_confirmed(CHAT, &t.reminder(CHAT).unwrap());
        t.turn_closed(CHAT, t.delivered_reminder(CHAT).as_deref());
        assert!(!t.owes(CHAT));
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing);
        // A later change is owed again.
        t.changed(CHAT);
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { target: 3, .. }));
    }

    #[test]
    fn a_change_after_a_reminders_dispatch_stays_owed_when_the_ask_turn_publishes() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, target) = remind(&t);
        t.reminder_confirmed(CHAT, &id);
        let admitted = t.generation(CHAT); // the ask turn's publish is admitted
        t.changed(CHAT); // a change completes after dispatch and after admission
        t.published(CHAT, 1, admitted);
        t.turn_closed(CHAT, t.delivered_reminder(CHAT).as_deref());
        assert!(t.reminder(CHAT).is_none(), "the reminder settled: it covered {target}");
        assert!(t.owes(CHAT), "the later change is owed");
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { target: 2, .. }));
    }

    #[test]
    fn delayed_old_closure_does_not_settle_the_next_turns_reminder() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let old_delivery = t.delivered_reminder(CHAT);
        let (id, _) = remind(&t);
        t.reminder_confirmed(CHAT, &id);
        t.turn_closed(CHAT, old_delivery.as_deref());
        assert_eq!(t.reminder(CHAT).as_deref(), Some(id.as_str()));
        assert!(t.owes(CHAT));
        t.turn_closed(CHAT, t.delivered_reminder(CHAT).as_deref());
        assert!(!t.owes(CHAT));
    }

    #[test]
    fn an_unrelated_message_never_settles_the_reminder() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        t.reminder_confirmed(CHAT, "user-prompt");
        t.turn_closed(CHAT, t.delivered_reminder(CHAT).as_deref());
        assert_eq!(t.reminder(CHAT).as_deref(), Some(id.as_str()), "only the reminder's own id counts");
    }

    #[test]
    fn a_publication_withdraws_a_queued_reminder_and_leaves_no_duplicate() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        t.published(CHAT, 1, t.generation(CHAT));
        assert_eq!(t.evaluate(CHAT, true), Eval::Withdraw { message_id: id.clone() });
        // Still queued if the withdrawal found the row gone: it stays reserved.
        t.supersede_done(CHAT, &id, false);
        assert_eq!(t.reminder(CHAT).as_deref(), Some(id.as_str()));
        // Withdrawn: settled.
        t.supersede_done(CHAT, &id, true);
        assert!(t.reminder(CHAT).is_none() && !t.owes(CHAT));
        // A change after the withdrawal is asked normally.
        t.changed(CHAT);
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { .. }));
    }

    #[tokio::test]
    async fn failed_queue_is_retried_without_an_unrelated_notification() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        t.wake.notified().await;
        let mut waiting = false;
        queue_failed(&t, CHAT, &id, &mut waiting);
        tokio::time::timeout(Duration::from_secs(1), retry_after(waiting, Duration::from_millis(1)))
            .await.expect("failed queue schedules a retry");
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { target: 1, .. }));
    }

    #[tokio::test]
    async fn withdrawing_a_covered_reminder_wakes_the_watcher_for_newer_work() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        t.published(CHAT, 1, 1);
        t.changed(CHAT);
        // Consume the existing coalesced notification before withdrawal.
        t.wake.notified().await;
        assert_eq!(t.evaluate(CHAT, true), Eval::Withdraw { message_id: id.clone() });
        t.supersede_done(CHAT, &id, true);
        tokio::time::timeout(Duration::from_millis(100), t.wake.notified()).await.expect("withdrawal wakes watcher");
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { target: 2, .. }));
    }

    #[test]
    fn a_cancelled_reminder_is_released_but_one_in_flight_or_unreadable_is_not() {
        let (t, row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        *row.lock().unwrap() = QueueRow::Unknown;
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing, "an unreadable queue says nothing");
        // Taken for dispatch: absent from the queue but in flight.
        t.mark_dispatching(CHAT, &id);
        *row.lock().unwrap() = QueueRow::Absent;
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing);
        // The dispatch fails and the row is restored: still the same reminder.
        t.dispatch_failed(CHAT, &id);
        *row.lock().unwrap() = QueueRow::Queued;
        assert_eq!(t.evaluate(CHAT, true), Eval::Nothing);
        assert_eq!(t.reminder_restores(CHAT), 1);
        // Cancelled by someone else: gone and not in flight. Asked again, anew.
        *row.lock().unwrap() = QueueRow::Absent;
        let Eval::Remind { message_id: again, .. } = t.evaluate(CHAT, true) else {
            panic!("a cancelled reminder is asked again");
        };
        assert_ne!(again, id);
        // A row that cannot be restored is lost: released.
        t.mark_dispatching(CHAT, &again);
        t.reminder_lost(CHAT, &again);
        assert!(t.reminder(CHAT).is_none());
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { .. }));
    }

    #[test]
    fn a_reminder_that_could_not_be_queued_is_released_for_the_next_evaluation() {
        let (t, _row) = tracker();
        t.changed(CHAT);
        let (id, _) = remind(&t);
        t.ask_failed(CHAT, "another-id");
        assert!(t.reminder(CHAT).is_some(), "another message's failure changes nothing");
        t.ask_failed(CHAT, &id);
        assert!(matches!(t.evaluate(CHAT, true), Eval::Remind { .. }));
    }

    #[test]
    fn a_chat_that_manages_nothing_is_forgiven_and_a_deleted_one_forgotten() {
        let (t, _row) = tracker();
        t.changed("worker");
        t.changed("worker");
        assert!(t.owes("worker"));
        t.forgive("worker");
        assert!(!t.owes("worker"), "nothing is kept current for a chat that manages nothing");
        t.changed("worker");
        assert!(t.owes("worker"), "a new change is a new question");
        assert_eq!(t.tracked_chat_ids(), vec!["worker".to_owned()]);
        t.forget_chat("worker");
        assert!(t.tracked_chat_ids().is_empty());
    }

    #[test]
    fn many_threads_changing_and_publishing_never_lose_a_generation() {
        let (t, _row) = tracker();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..200 {
                        t.changed(CHAT);
                    }
                });
            }
            scope.spawn(|| {
                for revision in 1..=50 {
                    let admitted = t.generation(CHAT);
                    t.published(CHAT, revision, admitted);
                }
            });
        });
        assert_eq!(t.generation(CHAT), 1600);
        // Whatever interleaving happened, the watermark never exceeds what existed.
        let admitted = t.generation(CHAT);
        t.published(CHAT, 51, admitted);
        assert!(!t.owes(CHAT));
    }

    #[test]
    fn dispatching_a_worker_is_state_changing() {
        assert!(is_state_changing(mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::READ_SUBTREE));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::BRIEFING_READ));
        assert!(!is_state_changing(mast_proto::agent_tools::tools::ARTIFACT_PUBLISH));
    }
}
