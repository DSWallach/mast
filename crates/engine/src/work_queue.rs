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
//! Each item is offered once per time it becomes ready. The Manager decides
//! how to brief and start the worker, so the existing claim on
//! `(workItemId, inputs)` still guards against a second worker.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use mast_proto::work_outline::{derive_id, roles};
use mast_proto::work_status::PullRequestFacts;
use mast_proto::{
    CreateChatForWorkItemParams, QueueEntry, StandingRoleKind, WorkItemPrompt, WorkItemView,
    WorkOutlineSnapshot,
};

use crate::orchestrator_wakes::OrchestratorWakes;
use crate::pr_ticket_cache::PrTicketCache;
use crate::work_outline_launch::WorkItemLaunchService;
use crate::workspace_host::WorkspaceHost;

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

#[derive(Clone)]
pub struct WorkQueueDispatcher {
    workspace: WorkspaceHost,
    launch: WorkItemLaunchService,
    wakes: OrchestratorWakes,
    state: Arc<Mutex<State>>,
    /// Where offers are remembered across restarts (absent in tests that
    /// build the dispatcher without a data directory).
    store: Option<std::path::PathBuf>,
    /// Without a source every linked pull request is unknown, so an item that
    /// has one is held rather than offered on a guess.
    pr_facts: Option<PrFactsSource>,
    /// Ticks when the PR cache stores results, to re-evaluate held items.
    pr_revision: Option<tokio::sync::watch::Receiver<u64>>,
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
    /// Item id -> queue key at the time it was offered.
    offered: HashMap<String, String>,
    /// Projects with a Manager start in flight.
    launching: HashSet<String>,
    /// Offers made to a hosted Manager (tests).
    offers_made: usize,
    /// Spaces with a ready item whose pull request has no answer yet.
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
    "Start a worker on it with create_chat (workItemId = that item), writing the brief and \
     choosing the model yourself. If it should not start now, say why in your reply; the next \
     ready item is offered after this one starts or is removed from the queue."
}

impl WorkQueueDispatcher {
    pub fn new(
        workspace: WorkspaceHost,
        launch: WorkItemLaunchService,
        wakes: OrchestratorWakes,
        store: Option<std::path::PathBuf>,
    ) -> Self {
        let offered = store
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            workspace,
            launch,
            wakes,
            state: Arc::new(Mutex::new(State { offered, ..State::default() })),
            store,
            pr_facts: None,
            pr_revision: None,
            held_recheck: HELD_RECHECK,
        }
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

    /// How many Spaces have an item waiting on a pull request answer (tests).
    #[doc(hidden)]
    pub fn held_spaces(&self) -> usize {
        lock(&self.state).held_spaces.len()
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

    /// Remember the offers so a restart does not repeat them.
    fn save_offered(&self) {
        let Some(path) = &self.store else { return };
        let bytes = serde_json::to_vec(&lock(&self.state).offered).unwrap_or_default();
        // Write beside the file and rename, so a crash never leaves a torn file.
        let tmp = path.with_extension("tmp");
        if let Err(error) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)) {
            tracing::warn!(%error, "work queue: could not save offered items");
        }
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
                let queue = snapshot
                    .work_queue(&project.id, now, &|url| self.pr_facts(url, PrLookup::Peek));
                (fetched, queue)
            })
            .collect();
        let held = queues
            .iter()
            .any(|(fetched, queue)| *fetched && queue.iter().any(|entry| entry.facts_pending));
        {
            let mut state = lock(&self.state);
            if held {
                state.held_spaces.insert(snapshot.space_id.clone());
            } else {
                state.held_spaces.remove(&snapshot.space_id);
            }
        }
        let still_ready: HashSet<String> = queues
            .into_iter()
            .flat_map(|(_, queue)| queue)
            .filter(|entry| entry.state == mast_proto::QueueEntryState::Ready)
            .map(|entry| entry.work_item_id)
            .collect();
        let in_space: HashSet<&str> = snapshot.items.iter().map(|item| item.id.as_str()).collect();
        let changed = {
            let mut state = lock(&self.state);
            let before = state.offered.len();
            state
                .offered
                .retain(|id, _| {
                    still_ready.contains(id)
                        || (!in_space.contains(id.as_str())
                            && self.workspace.work_item_space(id).is_some())
                });
            state.offered.len() != before
        };
        if changed {
            self.save_offered();
        }
    }

    async fn evaluate_project(&self, snapshot: &WorkOutlineSnapshot, project: &WorkItemView, now: i64) {
        if project.queue_paused {
            return;
        }
        let Some(entry) = snapshot.next_queue_offer(&project.id, now, &|url| {
            self.pr_facts(url, PrLookup::Fetch)
        }) else {
            return;
        };
        let key = snapshot
            .item(&entry.work_item_id)
            .and_then(|item| item.queue_order_key.clone())
            .unwrap_or_default();
        if lock(&self.state).offered.get(&entry.work_item_id) == Some(&key) {
            return;
        }
        match self.workspace.standing_role_chat(StandingRoleKind::Manager, &project.id) {
            Some(manager) => {
                let summary = queue_wake_summary(project, &entry);
                // Not hosted here: the hosting device makes the offer. Once made,
                // it is not repeated until the item stops being ready.
                if self.wakes.note_queue_ready(&manager, &entry.work_item_id, &summary).await {
                    let mut state = lock(&self.state);
                    state.offers_made += 1;
                    state.offered.insert(entry.work_item_id.clone(), key);
                    drop(state);
                    self.save_offered();
                }
            }
            None => self.start_manager(project, entry, key).await,
        }
    }

    async fn start_manager(&self, project: &WorkItemView, entry: QueueEntry, key: String) {
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
        let mut saved = false;
        {
            let mut state = lock(&self.state);
            state.launching.remove(&project.id);
            match &result {
                Ok(_) => {
                    state.offered.insert(entry.work_item_id.clone(), key);
                    saved = true;
                }
                // Someone else's Manager start won the race: it reads the
                // queue on its own, and the next pass offers through a wake.
                Err(error) => tracing::info!(
                    project = %project.id,
                    item = %entry.work_item_id,
                    error = %error.to_wire_message(),
                    "work queue: Manager start did not go through"
                ),
            }
        }
        if saved {
            self.save_offered();
        }
    }
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

    #[test]
    fn a_url_no_lookup_can_answer_is_known_not_merged() {
        let cache = PrTicketCache::new();
        let facts = facts(&cache, "https://git.example.com/acme/repo/pull/7", PrLookup::Fetch);
        assert!(facts.state_known && !facts.merged && !facts.open);
    }
}
