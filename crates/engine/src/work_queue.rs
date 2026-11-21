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

/// What the dispatcher knows about a linked pull request, by URL.
type PrFactsSource = Arc<dyn Fn(&str) -> PullRequestFacts + Send + Sync>;

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
}

/// Facts of a linked pull request from the PR cache. A lookup that came back
/// empty for a URL that is linked cannot mean "no such PR", so it stays
/// unknown, as does a cold cache; either way the cache is asked to fetch it.
fn cached_pr_facts(cache: &PrTicketCache, url: &str) -> PullRequestFacts {
    let known = |state: &str| PullRequestFacts {
        state_known: !state.is_empty(),
        open: state.eq_ignore_ascii_case("open"),
        merged: state.eq_ignore_ascii_case("merged"),
        ..PullRequestFacts::default()
    };
    // A merged PR cannot reopen, so a stale merged entry still counts.
    if let Some(state) = cache
        .cached_pr_state(url)
        .filter(|state| state.eq_ignore_ascii_case("merged"))
    {
        return known(&state);
    }
    match cache.request_pr_url(url) {
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
        }
    }

    /// Read linked pull requests from the PR cache, and re-evaluate when it
    /// stores new results.
    pub fn with_pr_ticket_cache(mut self, cache: PrTicketCache) -> Self {
        self.pr_revision = Some(cache.watch_pr_revision());
        self.pr_facts = Some(Arc::new(move |url| cached_pr_facts(&cache, url)));
        self
    }

    /// Read linked pull requests from `source` (tests).
    #[doc(hidden)]
    pub fn with_pr_facts(
        mut self,
        source: impl Fn(&str) -> PullRequestFacts + Send + Sync + 'static,
    ) -> Self {
        self.pr_facts = Some(Arc::new(source));
        self
    }

    fn pr_facts(&self, url: &str) -> PullRequestFacts {
        self.pr_facts
            .as_ref()
            .map_or_else(PullRequestFacts::default, |source| source(url))
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
            loop {
                for space in this.workspace.read_spaces().unwrap_or_default() {
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
                let pr_changed = async {
                    match pr_revision.as_mut() {
                        Some(revision) => revision.changed().await.is_ok(),
                        None => std::future::pending().await,
                    }
                };
                let pr_alive = tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = changes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        None
                    }
                    alive = pr_changed => Some(alive),
                };
                match pr_alive {
                    // The outline did not change, but a held item's pull
                    // request may now have an answer: look at every Space again.
                    Some(true) => markers.clear(),
                    Some(false) => pr_revision = None,
                    None => {}
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
        let still_ready: HashSet<String> = snapshot
            .items
            .iter()
            .filter(|item| item.is_project)
            .flat_map(|project| snapshot.work_queue(&project.id, now, &|url| self.pr_facts(url)))
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
        let Some(entry) = snapshot.next_queue_offer(&project.id, now, &|url| self.pr_facts(url))
        else {
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
