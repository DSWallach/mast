//! Manager auto-start for project work queues.
//!
//! The queue itself is data on the work items (`queueOrderKey`, see
//! `mast_proto::work_queue`). This service watches the Work Outline and, for
//! each project whose queue has a ready entry:
//!
//! - with no Manager, starts the project's Manager through the ordinary
//!   Manager launch (`standingRole: manager`) and delivers the item in its
//!   first message, on the device that owns the Space;
//! - with a Manager, hands the item to it as a queue wake through the
//!   orchestrator wake path, on the device that hosts the Manager chat.
//!
//! Each item is offered once per time it becomes ready, and the offer is a
//! Worker record in the dispatch store (`review_dispatch`), keyed by the item
//! and its queue key. A record left unanswered by an earlier engine run, or
//! addressed to a Manager that has since been replaced, is offered again
//! because the wake it rode on lived only in memory. The Manager decides how
//! to brief and start the worker, so the existing claim on
//! `(workItemId, inputs)` still guards against a second worker.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use mast_proto::work_outline::{derive_id, roles};
use mast_proto::work_status::PullRequestFacts;
use mast_proto::{
    ChatActivity, CreateChatForWorkItemParams, DispatchKind, DispatchRecord, DispatchState,
    QueueEntry, StandingRoleKind, WorkItemPrompt, WorkItemView, WorkOutlineSnapshot,
    dispatch_operation_id,
};

use crate::orchestrator_wakes::OrchestratorWakes;
use crate::pr_ticket_cache::PrTicketCache;
use crate::review_dispatch::ReviewDispatcher;
use crate::work_outline_launch::WorkItemLaunchService;
use crate::workspace_host::WorkspaceHost;

/// Offers of one item to the same Manager before the queue stops and asks the
/// user, so an unanswered item is not offered on every restart for ever.
const MAX_OFFER_ATTEMPTS: u32 = 3;

/// How long a Space with a held item waits before looking at it again when
/// nothing else changes. A failed lookup is not retried until its cache entry
/// expires, so the wait is what lets an unattended queue recover.
const HELD_RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a lookup may ask the PR cache to fetch what it does not have.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrLookup {
    Fetch,
    Peek,
}

/// Why the watcher woke up.
enum Wake {
    Outline,
    Pr(bool),
    Recheck,
}

/// When held Spaces are next looked at. The deadline is set when something
/// first becomes held and then left alone, so a stream of unrelated registry
/// writes cannot keep pushing the recheck back. Only the recheck itself
/// consumes it, and nothing held means no deadline.
struct HeldRecheck {
    interval: std::time::Duration,
    deadline: Option<tokio::time::Instant>,
}

impl HeldRecheck {
    fn new(interval: std::time::Duration) -> Self {
        Self { interval, deadline: None }
    }

    /// After a pass over every Space.
    fn after_pass(&mut self, held: bool, now: tokio::time::Instant) {
        self.deadline = if held {
            self.deadline.or(Some(now + self.interval))
        } else {
            None
        };
    }

    fn on_wake(&mut self, wake: &Wake) {
        if matches!(wake, Wake::Recheck) {
            self.deadline = None;
        }
    }
}

/// What the dispatcher knows about a linked pull request, by URL.
type PrFactsSource = Arc<dyn Fn(&str, PrLookup) -> PullRequestFacts + Send + Sync>;

/// What the dispatcher knows about a chat attached to a queued item, by id.
type ChatActivitySource = Arc<dyn Fn(&str) -> ChatActivity + Send + Sync>;

/// What one chat is doing, read from the registry. A chat that is gone counts
/// as archived. A worker is a chat an orchestrator started.
pub(crate) fn registry_chat_activity(workspace: &WorkspaceHost, chat_id: &str) -> ChatActivity {
    let chat = match workspace.chat(chat_id) {
        Ok(Some(chat)) => chat,
        Ok(None) => return ChatActivity { archived: true, ..ChatActivity::default() },
        // A failed read says nothing about the chat: keep it running for this
        // pass rather than offer its item to a second worker.
        Err(error) => {
            tracing::warn!(%error, chat = chat_id, "work queue: could not read a chat");
            return ChatActivity { working: true, ..ChatActivity::default() };
        }
    };
    let session = workspace.session(chat_id);
    ChatActivity::from_records(&chat, session.as_ref(), chrono::Utc::now(), |_| false)
}

#[derive(Clone)]
pub struct WorkQueueDispatcher {
    workspace: WorkspaceHost,
    launch: WorkItemLaunchService,
    wakes: OrchestratorWakes,
    state: Arc<Mutex<State>>,
    /// Offers and started workers, shared with the review dispatcher.
    records: ReviewDispatcher,
    /// Names this engine run on the records it writes.
    boot_id: String,
    /// The offers file of earlier versions, read once to seed records.
    legacy_store: Option<std::path::PathBuf>,
    /// Without a source every linked pull request is unknown, so an item that
    /// has one is held rather than offered on a guess.
    pr_facts: Option<PrFactsSource>,
    /// Overrides the registry as the source of what attached chats are doing
    /// (tests).
    chat_activity: Option<ChatActivitySource>,
    /// Ticks when the PR cache stores results, to re-evaluate held items.
    pr_revision: Option<tokio::sync::watch::Receiver<crate::pr_ticket_cache::PrCacheUpdate>>,
    held_recheck: std::time::Duration,
}

/// Facts of a linked pull request from the PR cache. A lookup that came back
/// empty for a URL that is linked cannot mean "no such PR", so it stays
/// unknown, as does a cold cache; with `Fetch` the cache is asked to fetch it.
/// A URL no lookup can ever answer is known not to be merged, so it cannot
/// hold the queue for good.
pub(crate) fn cached_pr_facts(cache: &PrTicketCache, url: &str, lookup: PrLookup) -> PullRequestFacts {
    let known = |state: &str| PullRequestFacts {
        state_known: !state.is_empty(),
        open: state.eq_ignore_ascii_case("open"),
        merged: state.eq_ignore_ascii_case("merged"),
        ..PullRequestFacts::default()
    };
    let Some(pr) = crate::work_outline_pr_links::pull_request_from_url(url) else {
        return PullRequestFacts { state_known: true, ..PullRequestFacts::default() };
    };
    // A merged PR cannot reopen, so a stale merged entry still counts, under
    // the URL as written or its canonical form.
    if let Some(state) = [url, pr.url.as_str()]
        .into_iter()
        .filter_map(|candidate| cache.cached_pr_state(candidate))
        .find(|state| state.eq_ignore_ascii_case("merged"))
    {
        return known(&state);
    }
    let answer = match lookup {
        PrLookup::Fetch => cache.request_pr_url(url),
        PrLookup::Peek => cache.peek_pr_url(url).or_else(|| cache.peek_pr_url(&pr.url)),
    };
    match answer {
        Some(Some(status)) => known(&status.state),
        Some(None) | None => PullRequestFacts::default(),
    }
}

#[derive(Default)]
struct State {
    /// Offers from the earlier offers file (item id -> queue key) not yet
    /// seeded into a record or found stale.
    legacy: HashMap<String, String>,
    /// A record could not be saved in these Spaces: look again on the recheck.
    retry_spaces: HashSet<String>,
    /// Projects with a Manager start in flight.
    launching: HashSet<String>,
    /// Offers made to a hosted Manager (tests).
    offers_made: usize,
    /// Spaces to look at again on the recheck clock: a ready item whose pull
    /// request has no answer yet, or a running item whose idle window can run
    /// out with no registry write.
    held_spaces: HashSet<String>,
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The Manager's first message when the queue starts it.
pub(crate) fn manager_start_prompt(project: &WorkItemView, entry: &QueueEntry) -> String {
    format!(
        "You are the Manager of the project \"{}\". The project's work queue just got its first \
         ready item, so you were started to deliver it: \"{}\" (work item {}).\n\n{}",
        project.title,
        entry.title,
        entry.work_item_id,
        queue_instruction(entry)
    )
}

/// The one-line wake a running Manager gets.
pub(crate) fn queue_wake_summary(project: &WorkItemView, entry: &QueueEntry) -> String {
    format!(
        "Work queue of \"{}\" — ready: \"{}\" (work item {}). {}",
        project.title,
        entry.title,
        entry.work_item_id,
        queue_instruction(entry)
    )
}

fn queue_instruction(_entry: &QueueEntry) -> &'static str {
    "First check the item has no worker already (read_chat, work_outline_read_subtree); you may \
     have started one before this offer reached you. If it has none, start a worker on it with \
     create_chat (workItemId = that item), writing the brief and \
     choosing the model yourself. If it should not start now, say why in your reply; the next \
     ready item is offered after this one starts or is removed from the queue."
}

impl WorkQueueDispatcher {
    pub fn new(
        workspace: WorkspaceHost,
        launch: WorkItemLaunchService,
        wakes: OrchestratorWakes,
        legacy_store: Option<std::path::PathBuf>,
    ) -> Self {
        static BOOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let legacy = legacy_store
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let records = ReviewDispatcher::new(workspace.clone(), launch.clone(), None);
        Self {
            workspace,
            launch,
            wakes,
            state: Arc::new(Mutex::new(State { legacy, ..State::default() })),
            records,
            boot_id: format!(
                "{}-{}-{}",
                now_ms(),
                std::process::id(),
                BOOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            legacy_store,
            pr_facts: None,
            chat_activity: None,
            pr_revision: None,
            held_recheck: HELD_RECHECK,
        }
    }

    /// Keep offers in `records`, the store the review dispatcher also uses.
    /// Without it they live in memory only (tests).
    pub fn with_dispatch_records(mut self, records: ReviewDispatcher) -> Self {
        self.records = records;
        self
    }

    /// Read linked pull requests from the PR cache, and re-evaluate when it
    /// stores new results.
    pub fn with_pr_ticket_cache(mut self, cache: PrTicketCache) -> Self {
        self.pr_revision = Some(cache.watch_pr_revision());
        self.pr_facts = Some(Arc::new(move |url, lookup| cached_pr_facts(&cache, url, lookup)));
        self
    }

    /// Read linked pull requests from `source` (tests).
    #[doc(hidden)]
    pub fn with_pr_facts(
        mut self,
        source: impl Fn(&str) -> PullRequestFacts + Send + Sync + 'static,
    ) -> Self {
        self.pr_facts = Some(Arc::new(move |url, _| source(url)));
        self
    }

    /// How many Spaces are on the recheck clock (tests).
    #[doc(hidden)]
    pub fn held_spaces(&self) -> usize {
        lock(&self.state).held_spaces.len()
    }

    /// Read what attached chats are doing from `source` (tests).
    #[doc(hidden)]
    pub fn with_chat_activity(
        mut self,
        source: impl Fn(&str) -> ChatActivity + Send + Sync + 'static,
    ) -> Self {
        self.chat_activity = Some(Arc::new(source));
        self
    }

    fn chat_activity(&self, chat_id: &str) -> ChatActivity {
        match &self.chat_activity {
            Some(source) => source(chat_id),
            None => registry_chat_activity(&self.workspace, chat_id),
        }
    }

    /// How long a Space with a held item waits before looking again (tests).
    #[doc(hidden)]
    pub fn with_held_recheck(mut self, interval: std::time::Duration) -> Self {
        self.held_recheck = interval;
        self
    }

    fn pr_facts(&self, url: &str, lookup: PrLookup) -> PullRequestFacts {
        self.pr_facts
            .as_ref()
            .map_or_else(PullRequestFacts::default, |source| source(url, lookup))
    }

    /// How many offers this dispatcher made to a Manager hosted here.
    #[doc(hidden)]
    pub fn offers_made(&self) -> usize {
        lock(&self.state).offers_made
    }

    /// Watch the Work Outline of every Space and evaluate on each change.
    pub fn spawn_watcher(&self, cancel: tokio_util::sync::CancellationToken) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut changes = this.workspace.watch_changes();
            let mut pr_revision = this.pr_revision.clone();
            let mut markers: HashMap<String, (u64, u64, bool)> = HashMap::new();
            let mut recheck_clock = HeldRecheck::new(this.held_recheck);
            loop {
                let spaces = match this.workspace.read_spaces() {
                    Ok(spaces) => {
                        // A Space that is gone can no longer be held or marked.
                        let live: HashSet<&str> =
                            spaces.iter().map(|space| space.id.as_str()).collect();
                        lock(&this.state)
                            .held_spaces
                            .retain(|id| live.contains(id.as_str()));
                        markers.retain(|id, _| live.contains(id.as_str()));
                        spaces
                    }
                    // Without a list nothing is pruned: an empty one would
                    // forget every held Space.
                    Err(error) => {
                        tracing::warn!(%error, "work queue: could not list Spaces");
                        Vec::new()
                    }
                };
                for space in spaces {
                    let marker = this.workspace.work_outline_inputs_marker(&space.id);
                    if markers.get(&space.id) == Some(&marker) {
                        continue;
                    }
                    let workspace = this.workspace.clone();
                    let space_id = space.id.clone();
                    let Ok((marker, snapshot)) = tokio::task::spawn_blocking(move || {
                        let (marker, inputs) = workspace.work_outline_inputs_marked(&space_id);
                        (marker, mast_doc::materialize_work_outline(&space_id, &inputs))
                    })
                    .await
                    else {
                        continue;
                    };
                    markers.insert(space.id.clone(), marker);
                    this.evaluate(&snapshot).await;
                }
                recheck_clock.after_pass(
                    !lock(&this.state).held_spaces.is_empty(),
                    tokio::time::Instant::now(),
                );
                let pr_changed = async {
                    match pr_revision.as_mut() {
                        Some(revision) => revision.changed().await.is_ok(),
                        None => std::future::pending().await,
                    }
                };
                let deadline = recheck_clock.deadline;
                let recheck = async move {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                };
                let wake = tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = changes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        Wake::Outline
                    }
                    alive = pr_changed => Wake::Pr(alive),
                    _ = recheck => Wake::Recheck,
                };
                recheck_clock.on_wake(&wake);
                match wake {
                    Wake::Outline => {}
                    Wake::Pr(false) => pr_revision = None,
                    // The outline did not change, but a held Space is looked
                    // at again; Spaces with nothing held are left alone.
                    Wake::Pr(true) | Wake::Recheck => {
                        for space in lock(&this.state).held_spaces.iter() {
                            markers.remove(space);
                        }
                    }
                }
                // Coalesce a burst of registry writes.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                changes.borrow_and_update();
            }
        });
    }

    /// Evaluate every project of one Space snapshot.
    pub async fn evaluate(&self, snapshot: &WorkOutlineSnapshot) {
        if !snapshot.available {
            return;
        }
        let now = now_ms();
        for project in snapshot
            .items
            .iter()
            .filter(|item| item.is_project && item.archived_at.is_none())
        {
            self.evaluate_project(snapshot, project, now).await;
        }
        // An item that stopped being ready may be offered again if it returns.
        // This pass only reads the cache: it must not queue fetches for the
        // history of paused or archived projects.
        let queues: Vec<(bool, Vec<QueueEntry>)> = snapshot
            .items
            .iter()
            .filter(|item| item.is_project)
            .map(|project| {
                // Only projects `evaluate_project` fetches for can be waiting on
                // an answer; a paused or archived one never asks for it.
                let fetched = !project.queue_paused && project.archived_at.is_none();
                let queue = snapshot.work_queue(
                    &project.id,
                    now,
                    &|url| self.pr_facts(url, PrLookup::Peek),
                    &|chat| self.chat_activity(chat),
                );
                (fetched, queue)
            })
            .collect();
        // A Space is looked at again on the recheck clock while an entry in it
        // depends on something that can change without a registry write: a
        // pull request still waiting for an answer, or a running worker whose
        // idle window is about to run out.
        let held = queues.iter().any(|(fetched, queue)| {
            *fetched
                && queue.iter().any(|entry| {
                    entry.facts_pending
                        || entry.state == mast_proto::QueueEntryState::Running
                        // A wait a merge or a completion can end without a registry write.
                        || (entry.state == mast_proto::QueueEntryState::Waiting
                            && entry.waiting_on.iter().any(|wait| {
                                matches!(
                                    wait.reason,
                                    mast_proto::QueueWaitReason::NotDone
                                        | mast_proto::QueueWaitReason::NotLanded
                                )
                            }))
                })
        });
        {
            let mut state = lock(&self.state);
            // A record that could not be saved is retried on the recheck.
            let retry = state.retry_spaces.remove(&snapshot.space_id);
            if held || retry {
                state.held_spaces.insert(snapshot.space_id.clone());
            } else {
                state.held_spaces.remove(&snapshot.space_id);
            }
        }
        let queues: Vec<Vec<QueueEntry>> = queues.into_iter().map(|(_, queue)| queue).collect();
        self.start_noted_workers(snapshot, &queues);
        // An offer is kept while its item can still be answered: ready, with a
        // worker, or a dependent waiting only for a pull request nobody has
        // looked up yet (it was ready, and a cold cache must not offer it
        // twice). An item that stopped being any of these is offered again if
        // it returns, and one that is done, archived or gone drops its record.
        use mast_proto::QueueEntryState as S;
        let kept: HashSet<&str> = queues
            .iter()
            .flatten()
            .filter(|entry| {
                matches!(entry.state, S::Ready | S::Running | S::Stalled)
                    || (entry.state == S::Waiting && entry.facts_pending)
            })
            .map(|entry| entry.work_item_id.as_str())
            .collect();
        let in_space: HashSet<&str> = snapshot.items.iter().map(|item| item.id.as_str()).collect();
        let retired: Vec<String> = self
            .records
            .worker_items()
            .into_iter()
            .filter(|id| {
                !kept.contains(id.as_str())
                    && (in_space.contains(id.as_str()) || self.workspace.work_item_space(id).is_none())
            })
            .collect();
        if !retired.is_empty()
            && let Err(error) = self.records.retire_workers(&retired)
        {
            tracing::warn!(%error, "work queue: could not drop offer records");
            lock(&self.state).retry_spaces.insert(snapshot.space_id.clone());
        }
        // An offers-file entry is finished once it is seeded into a record or
        // its item moved to another queue key; a paused project's entry waits.
        // One for an item that no longer exists never seeds.
        let mut state = lock(&self.state);
        state.legacy.retain(|id, key| {
            if in_space.contains(id.as_str()) {
                let current = snapshot.item(id).and_then(|item| item.queue_order_key.as_deref());
                let seeded = self
                    .records
                    .worker_record(id)
                    .is_some_and(|record| record.queue_key.as_deref() == Some(key.as_str()));
                current == Some(key.as_str()) && !seeded
            } else {
                self.workspace.work_item_space(id).is_some()
            }
        });
        if state.legacy.is_empty()
            && let Some(path) = &self.legacy_store
            && path.exists()
        {
            let _ = std::fs::rename(path, path.with_extension("migrated"));
        }
    }

    /// An offered item whose worker now exists is started. Any chat an
    /// orchestrator started on the item counts, wherever it runs.
    fn start_noted_workers(&self, snapshot: &WorkOutlineSnapshot, queues: &[Vec<QueueEntry>]) {
        for entry in queues.iter().flatten() {
            if self.records.worker_record(&entry.work_item_id).is_none() {
                continue;
            }
            let Some(chat) = self.worker_chat(snapshot, &entry.work_item_id, None) else { continue };
            if let Some(error) = self.records.mark_worker_started(&entry.work_item_id, &chat) {
                tracing::warn!(%error, "work queue: started worker not persisted");
            }
        }
    }

    /// A live worker chat on `item_id` other than `manager`.
    fn worker_chat(&self, snapshot: &WorkOutlineSnapshot, item_id: &str, manager: Option<&str>) -> Option<String> {
        let item = snapshot.item(item_id)?;
        item.chat_ids
            .iter()
            .filter(|chat| Some(chat.as_str()) != manager)
            .find(|chat| {
                let activity = self.chat_activity(chat);
                activity.worker && !activity.archived
            })
            .cloned()
    }

    async fn evaluate_project(&self, snapshot: &WorkOutlineSnapshot, project: &WorkItemView, now: i64) {
        if project.queue_paused {
            return;
        }
        let Some(entry) = snapshot.next_queue_offer(
            &project.id,
            now,
            &|url| self.pr_facts(url, PrLookup::Fetch),
            &|chat| self.chat_activity(chat),
        ) else {
            return;
        };
        let item = entry.work_item_id.clone();
        let key = snapshot
            .item(&item)
            .and_then(|view| view.queue_order_key.clone())
            .unwrap_or_default();
        let manager = self.workspace.standing_role_chat(StandingRoleKind::Manager, &project.id);
        // Only the device hosting the Manager makes the offer; another one
        // holding a record for a Manager that moved away drops it.
        if let Some(manager) = &manager
            && !self.wakes.hosts(manager)
        {
            self.retire(&project.space_id, &item);
            return;
        }
        // A worker already on the item is the answer to any offer.
        if let Some(chat) = self.worker_chat(snapshot, &item, manager.as_deref()) {
            if let Some(error) = self.records.mark_worker_started(&item, &chat) {
                tracing::warn!(%error, "work queue: started worker not persisted");
            }
            return;
        }
        let operation_id = dispatch_operation_id(DispatchKind::Worker, &item, &key);
        let record = self
            .records
            .worker_record(&item)
            .filter(|record| record.queue_key.as_deref() == Some(key.as_str()));
        let legacy = lock(&self.state).legacy.get(&item) == Some(&key);
        let attempt = match decide(&self.boot_id, record.as_ref(), manager.as_deref()) {
            Decision::Skip => return,
            Decision::Fresh if legacy && record.is_none() => {
                // Offered before records existed: remember it, say nothing.
                let seeded = self.record(&item, &operation_id, &key, manager.as_deref().unwrap_or_default(), 1, false);
                if self.records.put_worker(seeded).is_ok() {
                    lock(&self.state).legacy.remove(&item);
                } else {
                    lock(&self.state).retry_spaces.insert(project.space_id.clone());
                }
                return;
            }
            Decision::Fresh => 1,
            Decision::Again(attempt) => attempt,
            Decision::GiveUp(record) => {
                self.give_up(project, &entry, record).await;
                return;
            }
        };
        match manager {
            Some(manager) => {
                let offered = self.record(&item, &operation_id, &key, &manager, attempt, false);
                if let Err(error) = self.records.put_worker(offered) {
                    tracing::warn!(%error, item = %item, "work queue: offer not recorded, so not sent");
                    lock(&self.state).retry_spaces.insert(project.space_id.clone());
                    return;
                }
                let summary = queue_wake_summary(project, &entry);
                let nonce = format!("{attempt}:{}", now_ms());
                if self.wakes.note_queue_ready(&manager, &item, &summary, &nonce).await {
                    lock(&self.state).offers_made += 1;
                } else {
                    self.retire(&project.space_id, &item);
                }
            }
            None => self.start_manager(project, entry, key, operation_id, attempt).await,
        }
    }

    fn record(
        &self,
        item: &str,
        operation_id: &str,
        key: &str,
        manager: &str,
        attempt: u32,
        start_path: bool,
    ) -> DispatchRecord {
        DispatchRecord {
            kind: DispatchKind::Worker,
            work_item_id: item.to_owned(),
            commit_sha: String::new(),
            operation_id: operation_id.to_owned(),
            worker_chat_id: String::new(),
            manager_chat_id: manager.to_owned(),
            evidence: String::new(),
            queue_key: Some(key.to_owned()),
            attempt,
            boot_id: self.boot_id.clone(),
            start_path,
            launch: None,
            state: DispatchState::Claimed,
            updated_at: now_ms(),
        }
    }

    fn retire(&self, space_id: &str, item: &str) {
        if let Err(error) = self.records.retire_workers(&[item.to_owned()]) {
            tracing::warn!(%error, item, "work queue: could not drop an offer record");
            lock(&self.state).retry_spaces.insert(space_id.to_owned());
        }
    }

    /// Stop offering an item no Manager answered, and ask the user once.
    async fn give_up(&self, project: &WorkItemView, entry: &QueueEntry, mut record: DispatchRecord) {
        use mast_proto::work_outline_user_actions::{UserActionKind, UserActionPriority};
        use mast_proto::{
            CreateUserActionRequestParams, UserActionOriginKind, UserActionRequestOrigin,
            WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation,
        };
        let reason = format!(
            "\"{}\" was offered to the Manager {} times and no worker started.",
            entry.title, record.attempt
        );
        let operation_id = derive_id(
            &format!("queue-offer-unanswered\0{}\0{}", entry.work_item_id, record.operation_id),
            "queue-offer-unanswered",
        );
        let mutation = WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
            options: vec![],
            links: vec![],
            facts: vec![],
            kind: Some(UserActionKind::Ask),
            priority: Some(UserActionPriority::Blocking),
            work_item_id: entry.work_item_id.clone(),
            origin: UserActionRequestOrigin { kind: UserActionOriginKind::System, ..Default::default() },
            text: format!("{reason} Start it by hand, or remove it from the queue."),
            info: None,
            dedupe_key: Some(operation_id.clone()),
            operation_id,
        });
        let workspace = self.workspace.clone();
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::System,
            device_id: workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        // Asked first, under a dedupe key that makes a retry safe, so a failure
        // never leaves a given-up item that nobody was told about.
        let asked = tokio::task::spawn_blocking(move || workspace.work_outline_mutate(&mutation, &actor, &[])).await;
        if !matches!(asked, Ok(Ok(_))) {
            tracing::warn!(item = %entry.work_item_id, "work queue: could not ask the user about an unanswered offer");
            lock(&self.state).retry_spaces.insert(project.space_id.clone());
            return;
        }
        record.state = DispatchState::Blocked { reason: reason.clone(), owner: "user".into() };
        record.updated_at = now_ms();
        if let Err(error) = self.records.put_worker(record.clone()) {
            tracing::warn!(%error, "work queue: could not record that an item was given up");
            lock(&self.state).retry_spaces.insert(project.space_id.clone());
            return;
        }
    }

    async fn start_manager(
        &self,
        project: &WorkItemView,
        entry: QueueEntry,
        key: String,
        record_operation_id: String,
        attempt: u32,
    ) {
        let owns_space = self
            .workspace
            .space(&project.space_id)
            .ok()
            .flatten()
            .is_some_and(|space| space.device_id == self.workspace.device_id());
        if !owns_space {
            return;
        }
        {
            let mut state = lock(&self.state);
            if !state.launching.insert(project.id.clone()) {
                return;
            }
        }
        // Stable per (project, item, prompt): a retry after a crash adopts the
        // same Manager instead of starting a second one. The launch binds an
        // operation id to its prompt, so the prompt is part of the id; a
        // renamed item (a different prompt) starts a fresh attempt instead of
        // conflicting with a half-done one.
        let prompt = manager_start_prompt(project, &entry);
        let operation_id = format!(
            "qm-{}",
            derive_id(
                &format!("queue-manager:{}:{}:{prompt}", project.id, entry.work_item_id),
                roles::CHAT
            )
        );
        // Recorded before the launch under the chat the launch will make, so a
        // crash between the two never leaves an offer nobody knows about.
        let manager_chat = derive_id(&operation_id, roles::CHAT);
        let offered = self.record(&entry.work_item_id, &record_operation_id, &key, &manager_chat, attempt, true);
        if let Err(error) = self.records.put_worker(offered) {
            tracing::warn!(%error, "work queue: offer not recorded, so Manager not started");
            let mut state = lock(&self.state);
            state.launching.remove(&project.id);
            state.retry_spaces.insert(project.space_id.clone());
            return;
        }
        let params = CreateChatForWorkItemParams {
            work_item_id: project.id.clone(),
            initial_prompt: WorkItemPrompt {
                text: prompt,
                attachments: Vec::new(),
            },
            config: None,
            execution_override: None,
            worktree_choice: None,
            operation_id,
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: Some(StandingRoleKind::Manager),
        };
        let result = self.launch.create_chat_for_work_item(&params).await;
        lock(&self.state).launching.remove(&project.id);
        match result {
            Ok(reply) => {
                if reply.chat_id != manager_chat {
                    let adopted = self.record(&entry.work_item_id, &record_operation_id, &key, &reply.chat_id, attempt, true);
                    let _ = self.records.put_worker(adopted);
                }
            }
            // Someone else's Manager start won the race: it reads the
            // queue on its own, and the next pass offers through a wake.
            Err(error) => {
                self.retire(&project.space_id, &entry.work_item_id);
                tracing::info!(
                    project = %project.id,
                    item = %entry.work_item_id,
                    error = %error.to_wire_message(),
                    "work queue: Manager start did not go through"
                );
            }
        }
    }
}

/// Whether the ready item must be offered now, given what was recorded.
fn decide(boot_id: &str, record: Option<&DispatchRecord>, manager: Option<&str>) -> Decision {
    let Some(record) = record else { return Decision::Fresh };
    match &record.state {
        // The worker is gone and the item is ready again.
        DispatchState::Started { .. } => Decision::Fresh,
        DispatchState::Blocked { .. } => Decision::Skip,
        DispatchState::Claimed => {
            let same_manager = manager.is_some_and(|m| m == record.manager_chat_id);
            if manager.is_some() && !same_manager {
                // A new Manager has not been told. The old Manager's wake
                // cannot be taken back; a worker it started suppresses this.
                return Decision::Fresh;
            }
            if record.boot_id == boot_id {
                return Decision::Skip;
            }
            // The Manager's first message carries the item in its session,
            // not in a wake that could be lost.
            if record.start_path && same_manager {
                return Decision::Skip;
            }
            if record.attempt >= MAX_OFFER_ATTEMPTS {
                return Decision::GiveUp(record.clone());
            }
            Decision::Again(record.attempt + 1)
        }
    }
}

/// What to do about a ready item given what was recorded for it.
enum Decision {
    Skip,
    /// Offer it as a new offer.
    Fresh,
    /// Offer it again: this is the given attempt.
    Again(u32),
    /// Offered too often with no answer.
    GiveUp(DispatchRecord),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const URL: &str = "https://github.com/acme/repo/pull/7";

    fn status(state: &str) -> crate::pr_ticket_cache::PrStatus {
        serde_json::from_value(serde_json::json!({
            "number": 7, "url": URL, "state": state, "isDraft": false, "reviewers": [],
            "mergeable": "mergeable", "hasReviewerRequested": false
        }))
        .unwrap()
    }

    fn facts(cache: &PrTicketCache, url: &str, lookup: PrLookup) -> PullRequestFacts {
        cached_pr_facts(cache, url, lookup)
    }

    #[test]
    fn a_cold_cache_or_an_empty_answer_for_a_linked_pr_is_unknown() {
        let cache = PrTicketCache::new();
        assert!(!facts(&cache, URL, PrLookup::Fetch).state_known);
        assert!(!facts(&cache, URL, PrLookup::Peek).state_known);
        // A fresh empty result means the fetch failed, not that the PR is gone.
        cache.publish_pr_url_for_test(URL, None);
        assert!(!facts(&cache, URL, PrLookup::Fetch).state_known);
    }

    #[test]
    fn peeking_never_asks_the_cache_to_fetch_but_fetching_does() {
        let cache = PrTicketCache::new();
        let _ = facts(&cache, URL, PrLookup::Peek);
        assert_eq!(cache.pending_pr_count_for_test(), 0);
        let _ = facts(&cache, URL, PrLookup::Fetch);
        assert_eq!(cache.pending_pr_count_for_test(), 1);
    }

    #[test]
    fn an_answer_is_read_under_the_canonical_or_the_written_url() {
        let cache = PrTicketCache::new();
        cache.publish_pr_url_for_test(URL, Some(status("OPEN")));
        let open = facts(&cache, &format!("{URL}/files#diff"), PrLookup::Fetch);
        assert!(open.state_known && open.open && !open.merged);
        let peeked = facts(&cache, &format!("{URL}/"), PrLookup::Peek);
        assert!(peeked.state_known && peeked.open);
    }

    #[test]
    fn a_stale_merged_entry_still_counts_but_a_stale_open_one_does_not() {
        let cache = PrTicketCache::new();
        cache.insert_pr_url_aged_for_test(URL, Some(status("MERGED")), Duration::from_secs(3600));
        assert!(facts(&cache, URL, PrLookup::Peek).merged);
        assert!(facts(&cache, &format!("{URL}/files"), PrLookup::Peek).merged);
        let other = "https://github.com/acme/repo/pull/8";
        cache.insert_pr_url_aged_for_test(other, Some(status("OPEN")), Duration::from_secs(3600));
        assert!(!facts(&cache, other, PrLookup::Peek).state_known);
    }

    #[test]
    fn the_held_deadline_is_set_once_and_not_pushed_back_by_unrelated_wakes() {
        let start = tokio::time::Instant::now();
        let interval = Duration::from_secs(60);
        let mut clock = HeldRecheck::new(interval);
        clock.after_pass(true, start);
        assert_eq!(clock.deadline, Some(start + interval));
        // Registry writes and PR cache publishes wake the loop and run another
        // pass, but neither moves the deadline.
        for (step, wake) in [Wake::Outline, Wake::Pr(true), Wake::Outline].iter().enumerate() {
            clock.on_wake(wake);
            clock.after_pass(true, start + Duration::from_secs(10 * (step as u64 + 1)));
            assert_eq!(clock.deadline, Some(start + interval));
        }
    }

    #[test]
    fn only_the_recheck_consumes_the_deadline_and_the_next_pass_sets_a_fresh_one() {
        let start = tokio::time::Instant::now();
        let interval = Duration::from_secs(60);
        let mut clock = HeldRecheck::new(interval);
        clock.after_pass(true, start);
        let fired = start + interval;
        clock.on_wake(&Wake::Recheck);
        assert_eq!(clock.deadline, None);
        // Still held after the recheck: a fresh deadline, not the elapsed one,
        // so held Spaces are looked at every interval and not on every pass.
        clock.after_pass(true, fired);
        assert_eq!(clock.deadline, Some(fired + interval));
    }

    #[test]
    fn nothing_held_means_no_deadline_and_a_new_hold_starts_a_new_one() {
        let start = tokio::time::Instant::now();
        let interval = Duration::from_secs(60);
        let mut clock = HeldRecheck::new(interval);
        clock.after_pass(true, start);
        clock.after_pass(false, start + Duration::from_secs(5));
        assert_eq!(clock.deadline, None);
        let later = start + Duration::from_secs(30);
        clock.after_pass(true, later);
        assert_eq!(clock.deadline, Some(later + interval));
    }

    fn claimed(boot: &str, manager: &str, attempt: u32, start_path: bool) -> DispatchRecord {
        DispatchRecord {
            kind: DispatchKind::Worker,
            work_item_id: "item".into(),
            commit_sha: String::new(),
            operation_id: "op".into(),
            worker_chat_id: String::new(),
            manager_chat_id: manager.into(),
            evidence: String::new(),
            queue_key: Some("a0".into()),
            attempt,
            boot_id: boot.into(),
            start_path,
            launch: None,
            state: DispatchState::Claimed,
            updated_at: 0,
        }
    }

    #[test]
    fn an_offer_is_repeated_only_when_its_wake_may_be_lost() {
        let is = |d: Decision| match d {
            Decision::Skip => "skip".to_owned(),
            Decision::Fresh => "fresh".to_owned(),
            Decision::Again(n) => format!("again {n}"),
            Decision::GiveUp(_) => "give up".to_owned(),
        };
        // Nothing recorded: a new offer.
        assert_eq!(is(decide("b2", None, Some("m1"))), "fresh");
        // Made by this run to this Manager: its wake is still held or delivered.
        assert_eq!(is(decide("b1", Some(&claimed("b1", "m1", 1, false)), Some("m1"))), "skip");
        // Made by an earlier run: the in-memory wake is gone.
        assert_eq!(is(decide("b2", Some(&claimed("b1", "m1", 1, false)), Some("m1"))), "again 2");
        assert_eq!(is(decide("b2", Some(&claimed("b1", "m1", 2, false)), Some("m1"))), "again 3");
        // Three offers and still no worker: stop and ask.
        assert_eq!(is(decide("b2", Some(&claimed("b1", "m1", 3, false)), Some("m1"))), "give up");
        // A different Manager has not been told, even within one run.
        assert_eq!(is(decide("b1", Some(&claimed("b1", "m1", 3, false)), Some("m2"))), "fresh");
        // The Manager's first message holds the item in its session, not in a wake.
        assert_eq!(is(decide("b2", Some(&claimed("b1", "m1", 1, true)), Some("m1"))), "skip");
        // The Manager it was started for is gone and none runs: start again.
        assert_eq!(is(decide("b2", Some(&claimed("b1", "m1", 1, true)), None)), "again 2");
        assert_eq!(is(decide("b1", Some(&claimed("b1", "m1", 1, true)), None)), "skip");
        // A started worker that left the item ready again is a new offer.
        let mut started = claimed("b1", "m1", 2, false);
        started.state = DispatchState::Started { chat_id: "w".into() };
        assert_eq!(is(decide("b1", Some(&started), Some("m1"))), "fresh");
        // Given up: waits for the user, not offered again.
        let mut blocked = claimed("b1", "m1", 3, false);
        blocked.state = DispatchState::Blocked { reason: "r".into(), owner: "user".into() };
        assert_eq!(is(decide("b2", Some(&blocked), Some("m1"))), "skip");
    }

    #[test]
    fn a_url_no_lookup_can_answer_is_known_not_merged() {
        let cache = PrTicketCache::new();
        let facts = facts(&cache, "https://git.example.com/acme/repo/pull/7", PrLookup::Fetch);
        assert!(facts.state_known && !facts.merged && !facts.open);
    }
}
