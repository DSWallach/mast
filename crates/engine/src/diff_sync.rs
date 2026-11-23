//! CheckoutDiffSync — checkout-scoped working-tree diff production (feature-inventory
//! §3.5; port of zeron's `checkout-diff-sync.ts` + `git-metadata-sync.ts`).
//!
//! Chats do not own working-tree state: a concrete Git checkout does. This service
//! groups this device's chats by their canonical checkout identity (`chat.cwd` →
//! [`Repos::checkout_identity`]), computes one bounded atomic snapshot per checkout,
//! and publishes it three ways:
//!
//! - the local `WatchCheckoutDiffs` stream (a watch channel of every checkout's
//!   latest [`CheckoutDiff`]);
//! - a [`DiffSidecar`] JSON `POST {edge}/diff/{chatId}` for every syncing chat of
//!   the checkout (bearer = engine edge token), so "review pending changes while
//!   the host sleeps" works;
//! Checkout snapshots remain live checkout state. Conversation branch identity
//! is captured by the command host and is never rewritten from this watcher;
//! otherwise one checkout change would relabel every chat sharing that folder.
//!
//! Fast recursive `notify` watchers (debounced [`WATCH_DEBOUNCE`]) are backed by a
//! slow 2-minute repair tick because native watchers may coalesce or drop events.
//! Snapshots carry a sha256 checksum; an unchanged checksum publishes nothing.
//!
//! Repair-probe backoff: a fully watched checkout whose repair probe found
//! nothing for [`REPAIR_QUIET_TICKS_BEFORE_BACKOFF`] ticks is probed every
//! [`REPAIR_BACKOFF_STRIDE`] ticks (phase-staggered per checkout). A change the
//! watcher missed is therefore caught within 120 s normally and within 600 s
//! (stride x interval) once backed off; any watcher event, new chat, or probe
//! hit resets it. Unwatched / over-budget checkouts never back off.
//!
//! Reconcile is deliberately damped, because it runs on *every* workspace chat
//! row change — including the `checkoutId` writes reconcile itself makes.
//! Checkout identities are memoized per cwd (chat-watch reconciles spawn
//! no git; the repair tick revalidates), and an entry whose chats vanish is torn
//! down only after [`REPAIR_INTERVAL`] of continuous absence rather than on the
//! first pass that misses it. See [`resolve_identity`] for the incident this
//! guards against.
//!
//! Beyond the working-tree watch, the service also answers one-shot
//! `GetCheckoutDiff` captures for the Changes pane's scopes: *branch changes*
//! (vs `merge-base(baseRef, HEAD)`, same capture path with the base overridden)
//! and *latest turn* (vs a temp-index `write-tree` snapshot taken when a turn
//! dispatches — see [`CheckoutDiffSync::note_turn_start`]).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use mast_proto::{
    Chat, ChatDiffTargetSource, ChatDiffTargetView, ChatDiffTargetsRow, ChatDiffTargetsView,
    ChatRefTargetView, CheckoutDiff, DiffFileSummary, RefDiffKind, StoredChatDiffTarget,
    StoredRefDiffTarget, SubmoduleDiffSummary,
};

use crate::EngineError;
use crate::diff_targets::DiffTargets;
use crate::doc_host::EdgeConfig;
use crate::repos::{CheckoutIdentity, Repos};
use crate::workspace_host::WorkspaceHost;

mod git_status;
pub mod since;

/// Hard cap on the unified patch (plus untracked hunks) — "Partial snapshot".
pub const MAX_PATCH_BYTES: usize = 3 * 1024 * 1024;
pub const MAX_DIFF_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Recursive gitlink inspection is deliberately shallow and repository-bounded.
const MAX_SUBMODULE_DEPTH: usize = 2;
/// Each expanded repository costs up to seven short-lived Git processes. Keep
/// watcher-driven refreshes comfortably below the previous ~250-process worst
/// case while still covering ordinary multi-submodule workspaces.
const MAX_EXPANDED_SUBMODULES: usize = 8;
/// Shared across every nested name-status, numstat, and raw-tree capture.
const MAX_SUBMODULE_METADATA_BYTES: usize = 2 * 1024 * 1024;
/// Trailing debounce after a filesystem event burst.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
/// Slow repair pass: re-reconcile + re-sync every checkout.
const REPAIR_INTERVAL: Duration = Duration::from_secs(120);
/// Repair-probe backoff. A fully watched checkout whose last
/// `REPAIR_QUIET_TICKS_BEFORE_BACKOFF` repair probes all found nothing changed
/// is probed only every `REPAIR_BACKOFF_STRIDE` ticks (120 s -> 600 s). Any
/// watcher event, new chat, or probe that finds a change resets it to every
/// tick. Probes are phase-staggered per checkout so they spread over the
/// stride instead of bursting on one tick.
///
/// Detection bound for a change the watcher missed: at most one
/// `REPAIR_INTERVAL` (120 s) before backoff starts and at most
/// `REPAIR_BACKOFF_STRIDE * REPAIR_INTERVAL` (600 s) once backed off, plus the
/// debounce and probe time. Checkouts with no live root watcher (over the
/// watch budget, or watcher setup failed) are never backed off: their working
/// tree edits only ride the repair tick.
const REPAIR_QUIET_TICKS_BEFORE_BACKOFF: u32 = 3;
const REPAIR_BACKOFF_STRIDE: u64 = 5;
/// How long the first sync may wait for the watchers to attach, so its probe is
/// taken after attach and the capture-to-attach re-probe is skipped. Zero by
/// default: measured FSEvents registration takes ~1-2.5 s per checkout (two at
/// a time), so at startup the first sync would almost always time out and the
/// first diff would only be delayed. When attach does win the race (or a
/// test raises this) the re-probe is skipped either way.
static INITIAL_SYNC_ATTACH_WAIT_MS: AtomicU64 = AtomicU64::new(0);
/// Max subdirectories a checkout may have before we skip its live recursive
/// watch (one OS watch per dir; past this the watcher thread's own bookkeeping
/// costs more than instant diffs are worth). A normal source tree is well
/// under this; a node_modules/vendored tree blows past it. The repair tick
/// still covers skipped checkouts.
const MAX_WATCH_DIRS: usize = 8_000;
/// `git hash-object -t tree /dev/null` — diff base for repos with no commits yet.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Latest-only diff sidecar published to each chat's session DO slot
/// (`POST /diff/{chatId}`; shape: edge/src/session-doc/sidecar.ts).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSidecar {
    pub chat_id: String,
    pub device_id: String,
    pub checkout_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub patch: String,
    pub files: Vec<DiffFileSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submodules: Vec<SubmoduleDiffSummary>,
    pub additions: u32,
    pub deletions: u32,
    pub truncated: bool,
    /// Epoch millis.
    pub published_at: i64,
}

/// One bounded atomic snapshot of a checkout's working tree.
#[derive(Debug, Clone)]
pub struct DiffSnapshot {
    pub git_status: Option<(Vec<mast_proto::GitFileStatus>, bool)>,
    pub branch: String,
    pub head_sha: Option<String>,
    pub patch: String,
    pub files: Vec<DiffFileSummary>,
    pub submodules: Vec<SubmoduleDiffSummary>,
    pub additions: u32,
    pub deletions: u32,
    pub truncated: bool,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileTextPair {
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_content_hash: Option<String>,
    pub new_content_hash: Option<String>,
    pub binary: bool,
    pub truncated: bool,
}

struct CheckoutEntry {
    identity: CheckoutIdentity,
    chats: Mutex<Vec<Chat>>,
    /// Last published checksum — unchanged snapshots publish nothing.
    checksum: Mutex<Option<String>>,
    /// Set when a reconcile pass finds no chats for this checkout; cleared the
    /// moment chats reappear. The entry (watchers, checksum state, published
    /// diff) is only torn down after `orphan_grace` of *continuous* absence:
    /// tearing down and re-adding restarts a full capture, which costs seconds
    /// of CPU on a big checkout, so a single flapping chat-watch emission or
    /// transient identity failure must never destroy a live entry.
    orphaned_since: Mutex<Option<std::time::Instant>>,
    /// Kick channel into the entry's debounce/sync task.
    kick_tx: Kicker,
    /// Kick count already covered by a finished sync (see [`Kicker`]).
    handled_kicks: AtomicU64,
    /// Set once the fs watchers are attached (or setup was abandoned).
    watch_attached: AtomicBool,
    attach_finished_for_test: AtomicBool,
    /// Most recent chat activity, updated even while setup is queued.
    watch_priority: Arc<AtomicI64>,
    /// Debug-only measurement of the post-attachment kick.
    attach_timing: Mutex<Option<(std::time::Instant, u64)>>,
    /// Keeps the recursive fs watchers alive; dropped on entry close. Filled
    /// asynchronously — watcher setup (budget walk + FSEvents registration) can
    /// block for seconds, so [`add_entry`] does it off the runtime and attaches
    /// the result here once ready.
    watchers: Mutex<Vec<crate::watch_budget::Watcher>>,
    /// Fingerprint of the last cheap repair probe (see [`repair_probe`]).
    probe: Mutex<Option<u64>>,
    /// Set by the repair tick: the next sync may be skipped when the probe
    /// shows nothing changed. Watcher and new-chat kicks leave it clear.
    repair_kick: AtomicBool,
    /// Consecutive repair probes that found nothing changed (see
    /// [`REPAIR_BACKOFF_STRIDE`]); reset by watcher kicks, new chats, changes.
    quiet_ticks: AtomicU32,
    /// Repair ticks this entry has seen; with `probe_phase` picks which tick of
    /// a backed-off stride probes.
    repair_ticks_seen: AtomicU64,
    probe_phase: u64,
    /// True once every watch target is attached; only then may the repair
    /// probe back off.
    fully_watched: AtomicBool,
    /// The first sync has been kicked (by attach, or by the attach-wait timeout).
    initial_kicked: AtomicBool,
    /// A sync started its probe after the watchers attached, so changes after
    /// that point are watcher-visible and the attach re-probe is redundant.
    probed_after_attach: AtomicBool,
    /// HEAD sha and epoch-ms of the last capture that saw it; a change is a
    /// HEAD move (see [`HeadMove`]). In memory only.
    last_head: Mutex<Option<(String, i64)>>,
    /// Branch and remote-tracking ref tips as of the last capture of this
    /// (bound) checkout; the "before" picture for the next HEAD move.
    last_tips: Mutex<Option<Arc<Vec<String>>>>,
}

/// Kick channel into an entry's sync task that also counts sends, so
/// "nothing is pending" is observable ([`CheckoutDiffSync::is_settled`]).
#[derive(Clone)]
struct Kicker {
    tx: mpsc::UnboundedSender<()>,
    sent: Arc<AtomicU64>,
}

impl Kicker {
    fn send(&self) {
        self.sent.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(());
    }
}

impl CheckoutEntry {
    /// Kick from the repair tick. Entries whose chats are all archived have
    /// nothing watching them (no sidecar consumer, no visible diff), so they
    /// are left alone; member/target entries carry no chats and still count.
    fn kick_for_repair(&self) {
        let idle = {
            let chats = lock(&self.chats);
            !chats.is_empty() && chats.iter().all(|chat| chat.archived)
        };
        if idle {
            return;
        }
        if self.fully_watched.load(Ordering::SeqCst)
            && self.quiet_ticks.load(Ordering::Relaxed) >= REPAIR_QUIET_TICKS_BEFORE_BACKOFF
        {
            let tick = self.repair_ticks_seen.fetch_add(1, Ordering::Relaxed);
            if !(tick + self.probe_phase).is_multiple_of(REPAIR_BACKOFF_STRIDE) {
                return;
            }
        }
        self.repair_kick.store(true, Ordering::Relaxed);
        self.kick_tx.send();
    }

    /// Watcher activity or a new chat: probe at the base cadence again.
    fn reset_repair_backoff(&self) {
        self.quiet_ticks.store(0, Ordering::Relaxed);
    }
}

/// Working-tree snapshot recorded when a chat's turn dispatches — the diff
/// base for the Changes pane's "Latest turn" scope. In-memory only: after an
/// engine restart the scope is unavailable until the next turn.
#[derive(Debug, Clone)]
pub struct TurnSnapshot {
    /// Canonical checkout root the tree was captured in.
    pub root: PathBuf,
    /// `git write-tree` sha of the tracked + untracked (unignored) tree.
    pub tree: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// One stored chat diff target that currently resolves to a live checkout of
/// the repository it was bound in.
#[derive(Debug, Clone)]
struct ResolvedTarget {
    identity: CheckoutIdentity,
    source: ChatDiffTargetSource,
    selected: bool,
    repo_name: String,
}

/// One stored branch / PR range target whose repository checkout still is the
/// bound one and whose head ref still resolves.
#[derive(Debug, Clone)]
struct ResolvedRefTarget {
    identity: CheckoutIdentity,
    stored: StoredRefDiffTarget,
    selected: bool,
    repo_name: String,
    head_sha: String,
}

#[derive(Debug, Clone)]
struct ResolvedChatTargets {
    chat_id: String,
    targets: Vec<ResolvedTarget>,
    ref_targets: Vec<ResolvedRefTarget>,
}

struct RepositoryTips {
    cache: Option<crate::provenance::TipsCache>,
}

struct PackedRefs {
    inode: u64,
    modified: Option<SystemTime>,
    length: u64,
    refs: HashMap<String, String>,
    #[cfg(test)]
    parses: u64,
}

struct DiffSyncInner {
    repos: Repos,
    repository_tips: Mutex<HashMap<PathBuf, RepositoryTips>>,
    packed_refs: Mutex<HashMap<PathBuf, PackedRefs>>,
    workspace: WorkspaceHost,
    device_id: String,
    edge: Option<EdgeConfig>,
    http: reqwest::Client,
    entries: Mutex<HashMap<String, Arc<CheckoutEntry>>>,
    /// Serializes [`reconcile`] passes. Concurrent passes (chat-watch task vs.
    /// `reconcile_now`) can both observe a checkout as missing and both
    /// `add_entry` it — the second insert silently replaces the first entry,
    /// discarding its checksum state and kicking a redundant full capture.
    reconcile_gate: tokio::sync::Mutex<()>,
    /// cwd → resolved checkout identity. See [`resolve_identity`].
    identities: Mutex<HashMap<String, CheckoutIdentity>>,
    /// How long an entry may sit chat-less before reconcile removes it.
    orphan_grace: Duration,
    completion_tx: watch::Sender<u64>,
    diffs_tx: watch::Sender<Vec<CheckoutDiff>>,
    statuses_tx: watch::Sender<Vec<mast_proto::CheckoutGitStatus>>,
    /// (chat id, checkout root) → turn-start tree (see [`TurnSnapshot`]): the
    /// chat's `cwd` checkout plus every resolvable diff target.
    turn_trees: Mutex<HashMap<(String, PathBuf), TurnSnapshot>>,
    /// Chat diff targets (`chatDiffTargets` rows); attached once by engine
    /// assembly via [`CheckoutDiffSync::set_diff_targets`].
    diff_targets: Mutex<Option<DiffTargets>>,
    /// Every chat's resolved targets as of the last reconcile pass.
    resolved_targets: Mutex<Vec<ResolvedChatTargets>>,
    targets_tx: watch::Sender<Vec<ChatDiffTargetsView>>,
    /// The tasks hold `Weak` refs, but an in-flight iteration holds an
    /// upgraded Arc — the token cuts it so no sidecar HTTP outlives shutdown.
    cancel: CancellationToken,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Notified when a checkout bound as a chat diff target moves HEAD.
    head_move_listener: Mutex<Option<crate::provenance::HeadMoveListener>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct CheckoutDiffSync {
    inner: Arc<DiffSyncInner>,
}

/// Holds all watch setup slots for deterministic attachment tests.
#[doc(hidden)]
pub struct WatchSetupPause { _permits: Vec<WatchSetupPermit> }

impl CheckoutDiffSync {
    #[doc(hidden)]
    pub async fn pause_watch_setups_for_test() -> WatchSetupPause {
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_WATCH_SETUPS {
            permits.push(watch_setup_queue().acquire(Arc::new(AtomicI64::new(0))).await.unwrap());
        }
        WatchSetupPause { _permits: permits }
    }
    /// Build and start the sync loop: follows the workspace chat watch and runs the
    /// 2-minute repair tick. Requires a tokio runtime.
    pub fn start(
        repos: Repos,
        workspace: WorkspaceHost,
        device_id: &str,
        edge: Option<EdgeConfig>,
    ) -> Self {
        // Grace = one repair interval: an entry must survive at least one full
        // fresh revalidation pass before reconcile may tear it down.
        Self::start_with_orphan_grace(repos, workspace, device_id, edge, REPAIR_INTERVAL)
    }

    /// [`CheckoutDiffSync::start`] with an explicit orphan grace — test hook so
    /// removal-after-grace is exercisable without waiting out [`REPAIR_INTERVAL`].
    #[doc(hidden)]
    pub fn start_with_orphan_grace(
        repos: Repos,
        workspace: WorkspaceHost,
        device_id: &str,
        edge: Option<EdgeConfig>,
        orphan_grace: Duration,
    ) -> Self {
        let (diffs_tx, _) = watch::channel(Vec::new());
        let sync = Self {
            inner: Arc::new(DiffSyncInner {
                repos,
                repository_tips: Mutex::new(HashMap::new()),
                packed_refs: Mutex::new(HashMap::new()),
                workspace: workspace.clone(),
                device_id: device_id.to_string(),
                edge,
                http: reqwest::Client::new(),
                entries: Mutex::new(HashMap::new()),
                reconcile_gate: tokio::sync::Mutex::new(()),
                identities: Mutex::new(HashMap::new()),
                orphan_grace,
                completion_tx: watch::channel(0).0,
                diffs_tx,
                statuses_tx: watch::channel(Vec::new()).0,
                turn_trees: Mutex::new(HashMap::new()),
                diff_targets: Mutex::new(None),
                resolved_targets: Mutex::new(Vec::new()),
                targets_tx: watch::channel(Vec::new()).0,
                cancel: CancellationToken::new(),
                supervisor: Mutex::new(None),
                head_move_listener: Mutex::new(None),
            }),
        };
        let task = tokio::spawn(diff_sync_task(
            Arc::downgrade(&sync.inner),
            workspace.watch_chats(),
            sync.inner.cancel.clone(),
        ));
        *lock(&sync.inner.supervisor) = Some(task);
        sync
    }

    /// Stop the sync graph and wait for the supervisor to exit. Per-entry
    /// tasks observe the same token, so an in-flight `sync_entry` drops its
    /// sidecar POST instead of finishing it under a replaced runtime.
    /// Idempotent.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let task = lock(&self.inner.supervisor).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// `WatchCheckoutDiffs` source: every tracked checkout's latest diff.
    pub fn watch_diffs(&self) -> watch::Receiver<Vec<CheckoutDiff>> {
        self.inner.diffs_tx.subscribe()
    }

    /// Consumers filter this shared cache; subscribing never starts another Git scan.
    pub fn watch_git_statuses(&self) -> watch::Receiver<Vec<mast_proto::CheckoutGitStatus>> {
        self.inner.statuses_tx.subscribe()
    }

    /// Attach the chat diff targets service: every reconcile pass then reads
    /// its rows, and a bind/select/unbind triggers a prompt reconcile (and a
    /// turn-snapshot base for a newly appeared target). Idempotent per
    /// service; the first call wins.
    /// Notify `listener` when a checkout bound as a chat diff target moves HEAD.
    pub fn set_head_move_listener(&self, listener: crate::provenance::HeadMoveListener) {
        *lock(&self.inner.head_move_listener) = Some(listener);
    }

    pub fn set_diff_targets(&self, targets: DiffTargets) {
        {
            let mut slot = lock(&self.inner.diff_targets);
            if slot.is_some() {
                return;
            }
            *slot = Some(targets.clone());
        }
        tokio::spawn(diff_targets_task(
            Arc::downgrade(&self.inner),
            targets.changes(),
            self.inner.cancel.clone(),
        ));
    }

    /// The attached chat diff targets service, if any.
    pub fn diff_targets(&self) -> Option<DiffTargets> {
        lock(&self.inner.diff_targets).clone()
    }

    /// The attached service, attaching `make()` first when none is.
    pub fn ensure_diff_targets(&self, make: impl FnOnce() -> DiffTargets) -> DiffTargets {
        if let Some(existing) = self.diff_targets() {
            return existing;
        }
        self.set_diff_targets(make());
        self.diff_targets()
            .expect("diff targets were attached just above")
    }

    /// `WatchChatDiffTargets` source: one view per chat with at least one
    /// resolved target (current value first, republished only on change).
    pub fn watch_chat_diff_targets(&self) -> watch::Receiver<Vec<ChatDiffTargetsView>> {
        self.inner.targets_tx.subscribe()
    }

    /// True when every tracked checkout has its watchers attached and no kick
    /// is waiting for a sync. Lets tests wait for startup to finish instead of
    /// guessing from wall-clock quiet.
    pub fn is_settled(&self) -> bool {
        lock(&self.inner.entries).values().all(|entry| {
            entry.attach_finished_for_test.load(Ordering::SeqCst)
                && entry.handled_kicks.load(Ordering::SeqCst)
                    >= entry.kick_tx.sent.load(Ordering::SeqCst)
        })
    }

    /// Subscribe before starting work, then recheck the desired state after each change.
    #[doc(hidden)]
    pub fn watch_completion_for_test(&self) -> watch::Receiver<u64> {
        self.inner.completion_tx.subscribe()
    }

    /// Whether setup has finished for this cwd, including fallback to repair.
    /// A completed setup does not necessarily have live root coverage.
    pub fn is_attached(&self, cwd: &str) -> bool {
        let id = lock(&self.inner.identities).get(cwd).map(|identity| identity.id.clone());
        let entries = lock(&self.inner.entries);
        id.as_ref().and_then(|id| entries.get(id))
            .or_else(|| entries.values().find(|entry| entry.identity.root == Path::new(cwd)))
            .is_some_and(|entry| entry.watch_attached.load(Ordering::SeqCst))
    }

    /// Whether this checkout's working-tree root has a live native watcher, the
    /// condition for its repair probe to back off. A host that has run out of
    /// inotify instances leaves checkouts on repair polling. Test hook.
    #[doc(hidden)]
    pub fn is_fully_watched_for_test(&self, cwd: &str) -> bool {
        let id = lock(&self.inner.identities).get(cwd).map(|identity| identity.id.clone());
        let entries = lock(&self.inner.entries);
        id.as_ref().and_then(|id| entries.get(id))
            .or_else(|| entries.values().find(|entry| entry.identity.root == Path::new(cwd)))
            .is_some_and(|entry| entry.fully_watched.load(Ordering::SeqCst))
    }

    /// Regroup this device's chats by checkout identity, then (re)build watchers.
    /// Public for tests (the background task calls it on every chat change).
    pub async fn reconcile_now(&self) {
        let chats = self.inner.workspace.watch_chats().borrow().clone();
        reconcile(&self.inner, chats, false).await;
    }

    /// Repair-tick path for tests: fresh identity revalidation, then kick every
    /// tracked checkout.
    #[doc(hidden)]
    pub async fn repair_now(&self) {
        let chats = self.inner.workspace.watch_chats().borrow().clone();
        reconcile(&self.inner, chats, true).await;
        for entry in lock(&self.inner.entries).values() {
            entry.kick_for_repair();
        }
    }

    /// How long a new entry's first sync may wait for its watchers to attach
    /// (default 0: no wait). Process-wide; test hook.
    #[doc(hidden)]
    pub fn set_initial_sync_attach_wait_for_test(wait: Duration) {
        INITIAL_SYNC_ATTACH_WAIT_MS.store(wait.as_millis() as u64, Ordering::Relaxed);
    }

    /// Ticks between probes of a backed-off, fully watched checkout: the detection
    /// bound for a missed change is this many repair intervals. Test hook.
    #[doc(hidden)]
    pub fn max_repair_gap_ticks() -> u64 {
        REPAIR_BACKOFF_STRIDE
    }

    /// Drop every checkout's fs watchers while leaving them marked fully
    /// watched, simulating events the OS never delivered. Test hook.
    #[doc(hidden)]
    pub fn drop_watchers_for_test(&self) {
        for entry in lock(&self.inner.entries).values() {
            lock(&entry.watchers).clear();
        }
    }

    /// Kick an immediate sync of every tracked checkout (repair-tick path).
    pub fn sync_all(&self) {
        for entry in lock(&self.inner.entries).values() {
            entry.kick_tx.send();
        }
    }

    /// A turn is starting for `chat_id` in `cwd`: snapshot the checkout's tree
    /// (and every currently resolvable diff target of the chat) in the
    /// background so "Latest turn" has a base. Best-effort — failures only
    /// log; a chat outside a checkout simply records nothing for its cwd.
    pub fn note_turn_start(&self, chat_id: &str, cwd: &str) {
        let inner = Arc::downgrade(&self.inner);
        let chat_id = chat_id.to_string();
        let cwd = PathBuf::from(cwd);
        tokio::spawn(async move {
            let Some(inner) = inner.upgrade() else { return };
            let mut roots: Vec<PathBuf> = Vec::new();
            if let Ok(identity) = inner.repos.checkout_identity(&cwd).await {
                roots.push(identity.root);
            }
            for target in resolve_chat_targets(&inner, &chat_id).await {
                if !roots.contains(&target.identity.root) {
                    roots.push(target.identity.root);
                }
            }
            if roots.is_empty() {
                return; // not a checkout, no targets
            }
            let trees =
                futures::future::join_all(roots.iter().map(|root| snapshot_tree(root))).await;
            let at = chrono::Utc::now();
            let mut turn_trees = lock(&inner.turn_trees);
            // Roots this chat no longer works in (an unbound target) drop out.
            turn_trees.retain(|(chat, root), _| *chat != chat_id || roots.contains(root));
            for (root, tree) in roots.into_iter().zip(trees) {
                match tree {
                    Ok(tree) => {
                        turn_trees.insert(
                            (chat_id.clone(), root.clone()),
                            TurnSnapshot { root, tree, at },
                        );
                    }
                    Err(err) => {
                        tracing::debug!(chat = %chat_id, error = %err,
                            "diff-sync: turn snapshot failed");
                    }
                }
            }
        });
    }

    /// The recorded turn-start snapshot of `chat_id` in the checkout rooted at
    /// `root`, if any turn dispatched (or the target was bound) since boot.
    pub fn turn_snapshot(&self, chat_id: &str, root: &Path) -> Option<TurnSnapshot> {
        lock(&self.inner.turn_trees)
            .get(&(chat_id.to_string(), root.to_path_buf()))
            .cloned()
    }

    /// Record a snapshot for (`chat_id`, the checkout at `path`) only when none
    /// exists yet — a freshly bound worktree gets a "Latest turn" base even
    /// though no turn has started in it. Best-effort.
    pub async fn snapshot_target_now(&self, chat_id: &str, path: &str) {
        let Some(identity) = resolve_identity(&self.inner, path, false).await else {
            return;
        };
        snapshot_root_if_missing(&self.inner, chat_id, &identity.root).await;
    }
}

async fn snapshot_root_if_missing(inner: &Arc<DiffSyncInner>, chat_id: &str, root: &Path) {
    let key = (chat_id.to_string(), root.to_path_buf());
    if lock(&inner.turn_trees).contains_key(&key) {
        return;
    }
    match snapshot_tree(root).await {
        Ok(tree) => {
            lock(&inner.turn_trees)
                .entry(key)
                .or_insert_with(|| TurnSnapshot {
                    root: root.to_path_buf(),
                    tree,
                    at: chrono::Utc::now(),
                });
        }
        Err(err) => {
            tracing::debug!(chat = %chat_id, error = %err,
                "diff-sync: target snapshot failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Reconcile: chats ⇄ checkout entries
// ---------------------------------------------------------------------------

/// Resolve `cwd` to its canonical checkout identity, preferring the memo.
///
/// [`Repos::checkout_identity`] spawns `git rev-parse` twice, and reconcile
/// runs on *every* workspace chat row change — including the `branch` and
/// `checkoutId` writes [`sync_entry`] itself makes. Resolving fresh on every
/// pass had two failure modes that combined into a runaway loop on a checkout
/// whose captures cost seconds of CPU:
///
/// 1. every publish fanned out into a storm of `git` spawns (fd pressure);
/// 2. one transient spawn failure (e.g. EMFILE) made the chat ungroupable, so
///    reconcile tore its entry down and re-added it on the next pass — and
///    every re-add kicks a full capture, whose row writes trigger the next
///    reconcile. Back-to-back `git diff` forever, on an idle tree.
///
/// So: chat-watch reconciles reuse the memo (no git at all), the repair tick
/// revalidates (`fresh`), and a failed fresh resolve keeps the memo unless the
/// directory is actually gone.
async fn resolve_identity(
    inner: &Arc<DiffSyncInner>,
    cwd: &str,
    fresh: bool,
) -> Option<CheckoutIdentity> {
    // Historical imports can legitimately retain HOME as their cwd. They are
    // conversation context, not authorization for boot-time Git discovery or
    // recursive FSEvents setup across a person's files.
    if crate::repos::is_automatic_access_blocked(Path::new(cwd)) {
        lock(&inner.identities).remove(cwd);
        return None;
    }
    if !fresh && let Some(identity) = lock(&inner.identities).get(cwd).cloned() {
        return Some(identity);
    }
    match inner.repos.checkout_identity(Path::new(cwd)).await {
        Ok(identity) => {
            lock(&inner.identities).insert(cwd.to_string(), identity.clone());
            Some(identity)
        }
        Err(err) => {
            if !Path::new(cwd).exists() {
                lock(&inner.identities).remove(cwd);
                tracing::debug!(cwd = %cwd, error = %err, "diff-sync: checkout gone");
                return None;
            }
            let cached = lock(&inner.identities).get(cwd).cloned();
            match &cached {
                Some(_) => tracing::debug!(cwd = %cwd, error = %err,
                    "diff-sync: identity resolve failed; keeping memoized identity"),
                None => tracing::debug!(cwd = %cwd, error = %err, "diff-sync: not a checkout"),
            }
            cached
        }
    }
}

/// Whether the checkout found at a member binding's path is the bound one:
/// the pinned worktree's exact checkout id, else the binding's repository
/// (topology id of the common dir) at that checkout's top level. No `git`
/// spawn: the common dir is read from the git dir's `commondir` file.
pub(crate) fn member_identity_matches(
    binding: &mast_proto::RepositoryBinding,
    identity: &CheckoutIdentity,
    device_id: &str,
) -> bool {
    if let Some(worktree_id) = &binding.worktree_id {
        return identity.id == *worktree_id;
    }
    let common_dir = common_dir_of(&identity.git_dir);
    crate::execution_scope::repository_id(device_id, &common_dir) == binding.repo_id
        && crate::execution_scope::canonical_lenient(&identity.root)
            == crate::execution_scope::canonical_lenient(Path::new(binding.checkout_path()))
}

/// The canonical common dir of a git dir: the `commondir` file of a linked
/// worktree's git dir, else the git dir itself. No `git` spawn.
fn common_dir_of(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let joined = git_dir.join(text.trim());
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
        Err(_) => std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf()),
    }
}

/// Whether the checkout found at a stored diff target's path is the bound
/// one: same checkout id, same repository (topology id of the common dir),
/// same canonical top level, and still on disk (the identity memo outlives a
/// removed worktree until the next repair pass).
fn target_identity_matches(
    target: &StoredChatDiffTarget,
    identity: &CheckoutIdentity,
    device_id: &str,
) -> bool {
    identity.id == target.worktree_id
        && identity.git_dir.is_dir()
        && crate::execution_scope::repository_id(device_id, &common_dir_of(&identity.git_dir))
            == target.repo_id
        && crate::execution_scope::canonical_lenient(&identity.root)
            == crate::execution_scope::canonical_lenient(Path::new(&target.path))
}

/// Display name of a repository: the basename of its main checkout
/// (`<main>/.git` common dir, or a submodule's `core.worktree`), else of the
/// checkout itself. Reads the config file; no `git` spawn.
fn repo_display_name(common_dir: &Path, fallback_root: &Path) -> String {
    let main = if common_dir.file_name().is_some_and(|name| name == ".git") {
        common_dir.parent().map(Path::to_path_buf)
    } else {
        std::fs::read_to_string(common_dir.join("config"))
            .ok()
            .and_then(|config| {
                config.lines().find_map(|line| {
                    let (key, value) = line.trim().split_once('=')?;
                    (key.trim() == "worktree").then(|| value.trim().to_owned())
                })
            })
            .filter(|worktree| !worktree.is_empty())
            .map(|worktree| crate::execution_scope::canonical_lenient(&common_dir.join(worktree)))
    };
    main.as_deref()
        .unwrap_or(fallback_root)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Resolve one chat row's stored targets through the identity memo, keeping
/// only those whose checkout still is the bound one. Stored order.
async fn resolve_row_targets(
    inner: &Arc<DiffSyncInner>,
    row: &ChatDiffTargetsRow,
    resolved: &mut HashMap<String, Option<CheckoutIdentity>>,
    fresh: bool,
) -> Vec<ResolvedTarget> {
    let mut out = Vec::new();
    for target in &row.targets {
        let identity = match resolved.get(&target.path) {
            Some(identity) => identity.clone(),
            None => {
                let identity = resolve_identity(inner, &target.path, fresh).await;
                resolved.insert(target.path.clone(), identity.clone());
                identity
            }
        };
        let Some(identity) = identity else { continue };
        if !target_identity_matches(target, &identity, &inner.device_id) {
            tracing::debug!(path = %target.path,
                "diff-sync: diff target checkout differs from its binding");
            continue;
        }
        let repo_name = repo_display_name(&common_dir_of(&identity.git_dir), &identity.root);
        out.push(ResolvedTarget {
            selected: row.selected.as_deref() == Some(target.path.as_str()),
            source: target.source,
            repo_name,
            identity,
        });
    }
    out
}

/// The commit a full ref name points at, read from the repository's files
/// (loose ref, then `packed-refs`) — no spawn. `None` for a missing ref or a
/// symbolic one.
pub(crate) fn read_ref_sha(common_dir: &Path, reference: &str) -> Option<String> {
    if valid_object_id(reference) {
        return Some(reference.to_owned());
    }
    if reference.contains("..") || reference.starts_with('/') {
        return None;
    }
    if let Ok(text) = std::fs::read_to_string(common_dir.join(reference)) {
        let sha = text.trim();
        return valid_object_id(sha).then(|| sha.to_owned());
    }
    let packed = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (sha, name) = line.split_once(' ')?;
        (name.trim() == reference && valid_object_id(sha)).then(|| sha.to_owned())
    })
}

/// Resolve one chat row's stored ref targets: the repository checkout must
/// still be the bound repository (same topology id, same canonical top level)
/// and the head ref must still exist. Stored order.
async fn resolve_row_ref_targets(
    inner: &Arc<DiffSyncInner>,
    row: &ChatDiffTargetsRow,
    resolved: &mut HashMap<String, Option<CheckoutIdentity>>,
    fresh: bool,
) -> Vec<ResolvedRefTarget> {
    let mut out = Vec::new();
    for stored in &row.ref_targets {
        let identity = match resolved.get(&stored.path) {
            Some(identity) => identity.clone(),
            None => {
                let identity = resolve_identity(inner, &stored.path, fresh).await;
                resolved.insert(stored.path.clone(), identity.clone());
                identity
            }
        };
        let Some(identity) = identity else { continue };
        let common_dir = common_dir_of(&identity.git_dir);
        if !identity.git_dir.is_dir()
            || crate::execution_scope::repository_id(&inner.device_id, &common_dir)
                != stored.repo_id
            || crate::execution_scope::canonical_lenient(&identity.root)
                != crate::execution_scope::canonical_lenient(Path::new(&stored.path))
        {
            tracing::debug!(path = %stored.path,
                "diff-sync: ref target repository differs from its binding");
            continue;
        }
        let Some(head_sha) = read_ref_sha(&common_dir, &stored.head_ref) else {
            continue;
        };
        out.push(ResolvedRefTarget {
            selected: row.selected_ref.as_deref() == Some(stored.id.as_str()),
            repo_name: repo_display_name(&common_dir, &identity.root),
            identity,
            stored: stored.clone(),
            head_sha,
        });
    }
    out
}

/// The chat's currently resolvable targets (memoized identities; no fresh
/// revalidation).
async fn resolve_chat_targets(inner: &Arc<DiffSyncInner>, chat_id: &str) -> Vec<ResolvedTarget> {
    let Some(targets) = lock(&inner.diff_targets).clone() else {
        return Vec::new();
    };
    let Some(row) = targets
        .rows()
        .into_iter()
        .find(|row| row.chat_id == chat_id)
    else {
        return Vec::new();
    };
    resolve_row_targets(inner, &row, &mut HashMap::new(), false).await
}

/// Branch (`None` when detached or unreadable) and HEAD sha of a checkout,
/// read from the git dir's files (HEAD, loose ref, `packed-refs`) — no spawn.
fn read_head(inner: &DiffSyncInner, identity: &CheckoutIdentity) -> (Option<String>, Option<String>) {
    let Ok(head) = std::fs::read_to_string(identity.git_dir.join("HEAD")) else {
        return (None, None);
    };
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return (None, valid_object_id(head).then(|| head.to_owned()));
    };
    let branch = reference.strip_prefix("refs/heads/").map(str::to_owned);
    let common_dir = common_dir_of(&identity.git_dir);
    let loose = [&common_dir, &identity.git_dir]
        .into_iter()
        .find_map(|dir| {
            let text = std::fs::read_to_string(dir.join(reference)).ok()?;
            let sha = text.trim();
            valid_object_id(sha).then(|| sha.to_owned())
        });
    let sha = loose.or_else(|| {
        let path = common_dir.join("packed-refs");
        let metadata = std::fs::metadata(&path).ok()?;
        let inode = crate::provenance::metadata_inode(&metadata);
        let modified = metadata.modified().ok();
        let length = metadata.len();
        let mut caches = lock(&inner.packed_refs);
        if !caches.contains_key(&common_dir) && caches.len() >= 256 {
            caches.clear();
        }
        let stale = caches.get(&common_dir)
            .is_none_or(|cache| cache.inode != inode || cache.modified != modified || cache.length != length);
        if stale {
            let packed = std::fs::read_to_string(path).ok()?;
            let refs = packed.lines().take(100_000).filter_map(|line| {
                let (sha, name) = line.split_once(' ')?;
                valid_object_id(sha).then(|| (name.trim().to_owned(), sha.to_owned()))
            }).collect();
            #[cfg(test)]
            let parses = caches.get(&common_dir).map_or(1, |cache| cache.parses + 1);
            caches.insert(common_dir.clone(), PackedRefs {
                inode, modified, length, refs,
                #[cfg(test)]
                parses,
            });
        }
        caches.get(&common_dir)?.refs.get(reference).cloned()
    });
    (branch, sha)
}

/// "PR #n" for a PR target, "branch <name>" otherwise.
pub(crate) fn ref_target_label(target: &StoredRefDiffTarget) -> String {
    match (target.kind, target.pr_number) {
        (RefDiffKind::Pr, Some(number)) => format!("PR #{number}"),
        _ => format!("branch {}", target.head_branch),
    }
}

/// Rebuild and publish the `WatchChatDiffTargets` list from the last
/// reconcile's resolved targets plus the checkouts' current HEADs. Publishes
/// only when the list changed.
fn publish_targets(inner: &Arc<DiffSyncInner>) {
    let resolved = lock(&inner.resolved_targets).clone();
    let mut views: Vec<ChatDiffTargetsView> = resolved
        .iter()
        .filter(|chat| !chat.targets.is_empty() || !chat.ref_targets.is_empty())
        .map(|chat| ChatDiffTargetsView {
            chat_id: chat.chat_id.clone(),
            ref_targets: chat
                .ref_targets
                .iter()
                .map(|target| ChatRefTargetView {
                    id: target.stored.id.clone(),
                    kind: target.stored.kind,
                    label: ref_target_label(&target.stored),
                    path: target.identity.root.to_string_lossy().into_owned(),
                    checkout_id: target.identity.id.clone(),
                    repo_name: target.repo_name.clone(),
                    head_branch: target.stored.head_branch.clone(),
                    head_ref: target.stored.head_ref.clone(),
                    base_branch: target.stored.base_branch.clone(),
                    base_ref: target.stored.base_ref.clone(),
                    head_sha: Some(target.head_sha.clone()),
                    pr_number: target.stored.pr_number,
                    pr_url: target.stored.pr_url.clone(),
                    selected: target.selected,
                })
                .collect(),
            targets: chat
                .targets
                .iter()
                .map(|target| {
                    let (branch, head_sha) = read_head(inner, &target.identity);
                    ChatDiffTargetView {
                        path: target.identity.root.to_string_lossy().into_owned(),
                        checkout_id: target.identity.id.clone(),
                        source: target.source,
                        selected: target.selected,
                        repo_name: target.repo_name.clone(),
                        branch,
                        head_sha,
                    }
                })
                .collect(),
        })
        .collect();
    views.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    inner.targets_tx.send_if_modified(|current| {
        if *current == views {
            false
        } else {
            *current = views;
            true
        }
    });
}

async fn reconcile(inner: &Arc<DiffSyncInner>, chats: Vec<Chat>, fresh: bool) {
    // One pass at a time — see `reconcile_gate`.
    let _gate = inner.reconcile_gate.lock().await;
    // Group this device's cwd-bearing chats by canonical checkout identity.
    let mut groups: HashMap<String, (CheckoutIdentity, Vec<Chat>)> = HashMap::new();
    // Dedupe resolution within this pass — many chats share one checkout.
    let mut resolved: HashMap<String, Option<CheckoutIdentity>> = HashMap::new();
    // Work Outline execution bindings (Milestone 3): a bound Chat's member
    // and primary repositories each get their own checkout entry, so the
    // checkout-scoped watch (`WatchCheckoutDiffs {cwd, checkoutId}`) serves
    // one diff per member. Member entries carry no Chats: the Chat's row and
    // edge sidecar stay keyed to its execution root (`chat.cwd`).
    let bindings = inner.workspace.chat_execution_bindings();
    let mut member_paths: Vec<(String, mast_proto::RepositoryBinding, i64)> = Vec::new();
    let chat_priorities: HashMap<String, i64> = chats.iter()
        .map(|chat| (chat.id.clone(), chat_watch_priority(std::slice::from_ref(chat))))
        .collect();
    let mut checkout_priorities: HashMap<String, i64> = HashMap::new();
    let local_chat_ids: HashSet<String> = chats
        .iter()
        .filter(|chat| chat.device_id == inner.device_id)
        .map(|chat| chat.id.clone())
        .collect();
    for chat in chats {
        if chat.device_id != inner.device_id {
            continue;
        }
        if let Some(binding) = bindings.get(&chat.id)
            && binding.resolved.device_id == inner.device_id
        {
            member_paths.extend(
                binding
                    .resolved
                    .configuration
                    .bindings
                    .iter()
                    .filter(|b| b.member || b.primary)
                    .map(|b| (b.checkout_path().to_owned(), b.clone(), chat_priorities[&chat.id])),
            );
        }
        let Some(cwd) = chat.cwd.clone() else {
            continue;
        };
        let identity = match resolved.get(&cwd) {
            Some(identity) => identity.clone(),
            None => {
                let identity = resolve_identity(inner, &cwd, fresh).await;
                resolved.insert(cwd.clone(), identity.clone());
                identity
            }
        };
        let Some(identity) = identity else {
            continue;
        };
        // Stamp the row's checkoutId so every device groups this chat correctly.
        if chat.checkout_id.as_deref() != Some(identity.id.as_str())
            && let Err(err) =
                inner
                    .workspace
                    .set_chat_checkout_for_cwd(&chat.id, &cwd, &identity.id)
        {
            tracing::debug!(chat = %chat.id, error = %err, "diff-sync: checkoutId write failed");
        }
        groups
            .entry(identity.id.clone())
            .or_insert_with(|| (identity, Vec::new()))
            .1
            .push(chat);
    }
    for (path, binding, priority) in member_paths {
        let identity = match resolved.get(&path) {
            Some(identity) => identity.clone(),
            None => {
                let identity = resolve_identity(inner, &path, fresh).await;
                resolved.insert(path.clone(), identity.clone());
                identity
            }
        };
        // A missing member checkout gets no entry; validation reports it.
        // Neither does a checkout at the path that is not the bound one (a
        // pinned worktree replaced by another checkout, another repository,
        // or a folder inside one): the Changes pane must never show it.
        if let Some(identity) = identity {
            if !member_identity_matches(&binding, &identity, &inner.device_id) {
                tracing::debug!(path = %path, "diff-sync: member checkout identity differs from its binding");
                continue;
            }
            checkout_priorities.entry(identity.id.clone()).and_modify(|p| *p = (*p).max(priority)).or_insert(priority);
            groups
                .entry(identity.id.clone())
                .or_insert_with(|| (identity, Vec::new()));
        }
    }

    // Chat diff targets: each resolvable target of a local chat gets a
    // checkout entry like a member entry (no Chats attached) and a place in
    // the `WatchChatDiffTargets` list.
    let diff_targets = lock(&inner.diff_targets).clone();
    let mut resolved_chats: Vec<ResolvedChatTargets> = Vec::new();
    if let Some(diff_targets) = diff_targets {
        for row in diff_targets.rows() {
            if (row.targets.is_empty() && row.ref_targets.is_empty())
                || !local_chat_ids.contains(&row.chat_id)
            {
                continue;
            }
            let targets = resolve_row_targets(inner, &row, &mut resolved, fresh).await;
            let ref_targets = resolve_row_ref_targets(inner, &row, &mut resolved, fresh).await;
            for identity in targets
                .iter()
                .map(|target| &target.identity)
                .chain(ref_targets.iter().map(|target| &target.identity))
            {
                let priority = chat_priorities.get(&row.chat_id).copied().unwrap_or(0);
                checkout_priorities.entry(identity.id.clone()).and_modify(|p| *p = (*p).max(priority)).or_insert(priority);
                groups
                    .entry(identity.id.clone())
                    .or_insert_with(|| (identity.clone(), Vec::new()));
            }
            if !targets.is_empty() || !ref_targets.is_empty() {
                resolved_chats.push(ResolvedChatTargets {
                    chat_id: row.chat_id,
                    targets,
                    ref_targets,
                });
            }
        }
    }
    *lock(&inner.resolved_targets) = resolved_chats;
    publish_targets(inner);

    // Close entries whose checkout has had no chats for a full grace period;
    // drop their published diff. A single pass that misses a checkout only
    // *marks* it — teardown is expensive to undo (re-add kicks a capture), so
    // absence must be sustained before we act on it.
    let removed: Vec<String> = {
        let now = std::time::Instant::now();
        let mut entries = lock(&inner.entries);
        let mut removed = Vec::new();
        for (id, entry) in entries.iter() {
            if groups.contains_key(id) {
                *lock(&entry.orphaned_since) = None;
                continue;
            }
            let mut orphaned = lock(&entry.orphaned_since);
            match *orphaned {
                None => *orphaned = Some(now),
                Some(since) if now.duration_since(since) >= inner.orphan_grace => {
                    removed.push(id.clone());
                }
                Some(_) => {}
            }
        }
        for id in &removed {
            // Dropping the entry drops watchers + ends its task; its budget
            // decision is forgotten so a later re-add re-measures.
            if let Some(entry) = entries.remove(id) {
                forget_watch_budget(&entry.identity.root);
                forget_watch_budget(&entry.identity.git_dir);
            }
        }
        removed
    };
    if !removed.is_empty() {
        publish_watch(inner);
    }

    // Update surviving entries; add new ones (initial sync kicked on add).
    for (checkout_id, (identity, chats)) in groups {
        let priority = chat_watch_priority(&chats).max(checkout_priorities.get(&checkout_id).copied().unwrap_or(0));
        let existing = lock(&inner.entries).get(&checkout_id).cloned();
        match existing {
            Some(entry) => {
                entry.watch_priority.store(priority, Ordering::Relaxed);
                let has_new = {
                    let mut held = lock(&entry.chats);
                    let previous: HashSet<String> = held.iter().map(|c| c.id.clone()).collect();
                    let has_new = chats.iter().any(|c| !previous.contains(&c.id));
                    *held = chats;
                    has_new
                };
                if has_new {
                    entry.reset_repair_backoff();
                    entry.kick_tx.send(); // new chat needs a sidecar now
                }
            }
            None => add_entry(inner, identity, chats, priority),
        }
    }
}

/// Directory names that mark a heavy dependency or build tree. Finding one
/// anywhere in the walk means the checkout is over budget at once; the walk
/// never descends into it.
const HEAVY_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "dist",
    "build",
    ".next",
    "vendor",
];

/// At most this many budget walks plus watcher setups run at once. Startup adds
/// one entry per checkout (~100+ worktrees); unbounded, they all walk together
/// and pin every core for minutes.
const MAX_CONCURRENT_WATCH_SETUPS: usize = 2;

fn chat_watch_priority(chats: &[Chat]) -> i64 {
    chats.iter().filter(|chat| !chat.archived)
        .map(|chat| chat.last_message_at.unwrap_or(chat.created_at).timestamp_millis())
        .max().unwrap_or(0)
}

struct WatchSetupWaiter {
    priority: Arc<AtomicI64>,
    sender: tokio::sync::oneshot::Sender<WatchSetupPermit>,
}

struct WatchSetupQueueState {
    available: usize,
    priority_turn: bool,
    waiters: std::collections::VecDeque<WatchSetupWaiter>,
}

struct WatchSetupQueue(Mutex<WatchSetupQueueState>);

struct WatchSetupPermit(Arc<WatchSetupQueue>);

impl Drop for WatchSetupPermit {
    fn drop(&mut self) {
        let mut state = lock(&self.0.0);
        state.available += 1;
        drop(state);
        self.0.dispatch();
    }
}

impl WatchSetupQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(WatchSetupQueueState {
            available: MAX_CONCURRENT_WATCH_SETUPS,
            priority_turn: true,
            waiters: std::collections::VecDeque::new(),
        })))
    }

    async fn acquire(self: &Arc<Self>, priority: Arc<AtomicI64>) -> Option<WatchSetupPermit> {
        self.enqueue(priority).await.ok()
    }

    fn enqueue(self: &Arc<Self>, priority: Arc<AtomicI64>) -> tokio::sync::oneshot::Receiver<WatchSetupPermit> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lock(&self.0).waiters.push_back(WatchSetupWaiter { priority, sender });
        self.dispatch();
        receiver
    }

    fn dispatch(self: &Arc<Self>) {
        let mut rejected = Vec::new();
        let mut state = lock(&self.0);
        while state.available > 0 && !state.waiters.is_empty() {
            // Alternate the most recent checkout with the oldest waiter, so
            // a stream of new activity cannot starve the background queue.
            let index = if state.priority_turn {
                state.waiters.iter().enumerate()
                    .max_by_key(|(index, waiter)| (waiter.priority.load(Ordering::Relaxed), std::cmp::Reverse(*index)))
                    .map(|(index, _)| index).unwrap_or(0)
            } else { 0 };
            state.priority_turn = !state.priority_turn;
            let waiter = state.waiters.remove(index).unwrap();
            state.available -= 1;
            if let Err(permit) = waiter.sender.send(WatchSetupPermit(self.clone())) {
                rejected.push(permit);
            }
        }
        drop(state);
        drop(rejected);
    }
}

fn watch_setup_queue() -> &'static Arc<WatchSetupQueue> {
    static QUEUE: OnceLock<Arc<WatchSetupQueue>> = OnceLock::new();
    QUEUE.get_or_init(WatchSetupQueue::new)
}

fn watch_setup_gate() -> &'static Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_WATCH_SETUPS)))
}

/// Runs blocking `work` on the blocking pool once `gate` has a free permit, so
/// queued setups wait as cheap async tasks rather than parked threads.
#[cfg(test)]
async fn run_gated_blocking<F>(gate: Arc<tokio::sync::Semaphore>, work: F)
where
    F: FnOnce() + Send + 'static,
{
    let Ok(_permit) = gate.acquire_owned().await else {
        return;
    };
    let _ = tokio::task::spawn_blocking(work).await;
}

/// Budget decision per root path, kept for the process lifetime so the same
/// root is never walked twice. Forgotten when its entry is torn down (which
/// only happens after the orphan grace, [`REPAIR_INTERVAL`] by default).
fn budget_cache() -> &'static Mutex<HashMap<PathBuf, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forget_watch_budget(root: &Path) {
    lock(budget_cache()).remove(root);
}

/// Cached [`walk_exceeds_watch_budget`].
fn exceeds_watch_budget(root: &Path) -> bool {
    if let Some(&decided) = lock(budget_cache()).get(root) {
        return decided;
    }
    let exceeds = walk_exceeds_watch_budget(root);
    lock(budget_cache()).insert(root.to_path_buf(), exceeds);
    exceeds
}

fn timed_watch_budget(identity: &CheckoutIdentity, path: &Path) -> bool {
    let start = tracing::enabled!(target: "diff_sync::attach", tracing::Level::DEBUG)
        .then(std::time::Instant::now);
    let exceeds = exceeds_watch_budget(path);
    if let Some(start) = start {
        tracing::debug!(target: "diff_sync::attach", checkout = %identity.root.display(), path = %path.display(), stage = "budget", elapsed_us = start.elapsed().as_micros() as u64, skipped = exceeds);
    }
    exceeds
}

/// True if `root`'s directory tree exceeds [`MAX_WATCH_DIRS`] — the signal that
/// a live recursive watch would cost more than it's worth. Bounded BFS: stops
/// the moment the budget is blown (never walks a whole node_modules), returns
/// at once on a heavy dependency/build dir ([`HEAVY_DIR_NAMES`]) without
/// descending into it, skips symlinks (a symlinked dep cycle must not send this
/// into a spin), and treats unreadable dirs as leaves. `.git` internal churn is
/// real diff signal, so it counts toward the budget rather than being skipped.
fn walk_exceeds_watch_budget(root: &Path) -> bool {
    if crate::repos::is_automatic_access_blocked(root) {
        return true;
    }
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut seen = 0usize;
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type()` on the dirent does NOT follow symlinks — a symlinked
            // directory reports as a symlink and is skipped, so cyclic deps
            // (pnpm/npm) can't blow up the walk.
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| HEAVY_DIR_NAMES.contains(&name))
                {
                    return true;
                }
                seen += 1;
                if seen > MAX_WATCH_DIRS {
                    return true;
                }
                queue.push_back(entry.path());
            }
        }
    }
    false
}

/// Which paths a checkout's entry live-watches.
///
/// A recursive `notify` watch installs one OS watch per subdirectory and has
/// no way to prune subtrees. On a checkout carrying big dependency trees
/// (node_modules, target/, vendored deps) that is tens of thousands of
/// watches: the watcher thread pegs a core just maintaining them — even with
/// the tree completely idle — which starved a real device's whole async
/// runtime (presence heartbeats and IPC stalled; it showed permanently
/// offline). So the worktree root is only watched when it fits
/// [`MAX_WATCH_DIRS`].
///
/// An over-budget root still gets its GIT DIR watched (a bounded tree —
/// objects fanout + refs): commits, index moves, and branch switches then
/// refresh the diff instantly, and only raw working-tree edits wait for the
/// repair tick. Without this, a commit right after an edit left the pane
/// showing the pre-commit diff for up to two minutes (user report — the
/// dev checkout's target/ alone blows the budget). Linked worktrees keep
/// their git dir outside the root, so it rides along whenever the root's
/// watch doesn't already cover it.
fn watch_targets(identity: &CheckoutIdentity) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let root_fits = !timed_watch_budget(identity, &identity.root);
    if root_fits {
        targets.push(identity.root.clone());
    } else {
        tracing::info!(path = %identity.root.display(),
            "diff-sync: tree too large to watch live; watching the git dir, edits ride the repair tick");
    }
    let git_covered = root_fits && identity.git_dir.starts_with(&identity.root);
    if !git_covered && !timed_watch_budget(identity, &identity.git_dir) {
        targets.push(identity.git_dir.clone());
    }
    targets
}

fn add_entry(inner: &Arc<DiffSyncInner>, identity: CheckoutIdentity, chats: Vec<Chat>, priority: i64) {
    let (tx, kick_rx) = mpsc::unbounded_channel();
    let kick_tx = Kicker { tx, sent: Arc::new(AtomicU64::new(0)) };
    let probe_phase = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        identity.id.hash(&mut hasher);
        hasher.finish() % REPAIR_BACKOFF_STRIDE
    };
    let priority = Arc::new(AtomicI64::new(priority));
    let entry = Arc::new(CheckoutEntry {
        identity,
        chats: Mutex::new(chats),
        checksum: Mutex::new(None),
        orphaned_since: Mutex::new(None),
        kick_tx: kick_tx.clone(),
        handled_kicks: AtomicU64::new(0),
        watch_attached: AtomicBool::new(false),
        attach_finished_for_test: AtomicBool::new(false),
        watch_priority: priority.clone(),
        attach_timing: Mutex::new(None),
        watchers: Mutex::new(Vec::new()),
        probe: Mutex::new(None),
        repair_kick: AtomicBool::new(false),
        quiet_ticks: AtomicU32::new(0),
        repair_ticks_seen: AtomicU64::new(0),
        probe_phase,
        fully_watched: AtomicBool::new(false),
        initial_kicked: AtomicBool::new(false),
        probed_after_attach: AtomicBool::new(false),
        last_head: Mutex::new(None),
        last_tips: Mutex::new(None),
    });
    lock(&inner.entries).insert(entry.identity.id.clone(), entry.clone());
    tokio::spawn(entry_task(
        Arc::downgrade(inner),
        Arc::downgrade(&entry),
        kick_rx,
        inner.cancel.clone(),
    ));
    // The initial snapshot normally goes at once; with a non-zero
    // INITIAL_SYNC_ATTACH_WAIT_MS it waits for the watcher attach (below) so its
    // probe lands after attach, falling back to going ahead after the wait.
    let attach_wait = Duration::from_millis(INITIAL_SYNC_ATTACH_WAIT_MS.load(Ordering::Relaxed));
    if attach_wait.is_zero() {
        entry.initial_kicked.store(true, Ordering::SeqCst);
        kick_tx.send(); // initial snapshot — must not wait for watchers
    } else {
        let weak = Arc::downgrade(&entry);
        let kick_tx = kick_tx.clone();
        let cancel = inner.cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(attach_wait) => {}
            }
            if let Some(entry) = weak.upgrade()
                && !entry.initial_kicked.swap(true, Ordering::SeqCst)
            {
                kick_tx.send();
            }
        });
    }

    // Watcher setup is genuinely blocking: the budget walk reads up to
    // MAX_WATCH_DIRS directory entries and FSEvents stream registration stalls
    // for seconds when fseventsd is contended. Doing it inline starved the whole
    // runtime (workspace watches, presence) whenever entries were (re)built, so
    // it runs on the blocking pool and attaches to the entry when ready. Events
    // occurring before attachment are covered by the initial sync when that ran
    // after attach; when the attach-wait timeout let the initial sync go first,
    // one probe-gated kick after attachment closes the capture->attach gap
    // (one `git status` when nothing moved).
    // At most MAX_CONCURRENT_WATCH_SETUPS run at once (startup builds one per
    // checkout).
    let weak = Arc::downgrade(&entry);
    let completion_tx = inner.completion_tx.clone();
    let enqueued = tracing::enabled!(target: "diff_sync::attach", tracing::Level::DEBUG)
        .then(std::time::Instant::now);
    let checkout = enqueued.map(|_| entry.identity.root.clone());
    tokio::spawn(async move {
        let Some(_priority_permit) = watch_setup_queue().acquire(priority).await else { return };
        let Ok(_permit) = watch_setup_gate().clone().acquire_owned().await else { return };
        if let (Some(start), Some(checkout)) = (enqueued, checkout) {
            tracing::debug!(target: "diff_sync::attach", checkout = %checkout.display(), stage = "permit", elapsed_us = start.elapsed().as_micros() as u64);
        }
        let _ = tokio::task::spawn_blocking(move || {
        let Some(entry) = weak.upgrade() else {
            return; // entry removed before watchers were ready
        };
        let (watchers, fully_watched) = build_watchers(&entry.identity, &kick_tx);
        *lock(&entry.watchers) = watchers;
        entry.fully_watched.store(fully_watched, Ordering::SeqCst);
        entry.watch_attached.store(true, Ordering::SeqCst);
        // Hold the timing lock through the send so the entry task cannot finish
        // the kick before its measurement is recorded.
        let mut timing = enqueued.map(|_| lock(&entry.attach_timing));
        let attach_time = enqueued.map(|_| std::time::Instant::now());
        let kicked = if !entry.initial_kicked.swap(true, Ordering::SeqCst) {
            // First sync, probed after attach: nothing to close.
            kick_tx.send();
            true
        } else if !entry.probed_after_attach.load(Ordering::SeqCst) {
            entry.repair_kick.store(true, Ordering::Relaxed);
            kick_tx.send();
            true
        } else {
            false
        };
        if kicked && let (Some(timing), Some(start)) = (timing.as_mut(), attach_time) {
            **timing = Some((start, kick_tx.sent.load(Ordering::SeqCst)));
        }
        entry.attach_finished_for_test.store(true, Ordering::SeqCst);
        completion_tx.send_modify(|generation| *generation = generation.wrapping_add(1));
        }).await;
    });
}

/// Recursive watchers on the worktree root (budget permitting) and the git
/// dir — HEAD/index churn and file edits both land here. Failures are fine:
/// the initial + repair sync still keep the snapshot correct. Blocking — call
/// from the blocking pool.
fn build_watchers(
    identity: &CheckoutIdentity,
    kick_tx: &Kicker,
) -> (Vec<crate::watch_budget::Watcher>, bool) {
    let mut watchers = Vec::new();
    let targets = watch_targets(identity);
    let target_count = targets.len();
    // Fully watched: the working tree root itself has a live watcher.
    let mut root_watched = false;
    for target in targets {
        let is_root = target == identity.root;
        let tx = kick_tx.clone();
        let git_dir = identity.git_dir.clone();
        let start = tracing::enabled!(target: "diff_sync::attach", tracing::Level::DEBUG)
            .then(std::time::Instant::now);
        let watcher =
            crate::watch_budget::Watcher::new(move |event: Result<notify::Event, notify::Error>| {
                if event.as_ref().is_ok_and(|event| {
                    is_checkout_change(event) && kick_relevant(&event.paths, &git_dir)
                }) {
                    tx.send();
                }
            });
        if let Some(start) = start {
            tracing::debug!(target: "diff_sync::attach", checkout = %identity.root.display(), path = %target.display(), stage = "watcher_new", elapsed_us = start.elapsed().as_micros() as u64, success = watcher.is_some());
        }
        match watcher {
            Some(mut watcher) => {
                let start = tracing::enabled!(target: "diff_sync::attach", tracing::Level::DEBUG)
                    .then(std::time::Instant::now);
                let result = watcher.watch(&target, notify::RecursiveMode::Recursive);
                if let Some(start) = start {
                    tracing::debug!(target: "diff_sync::attach", checkout = %identity.root.display(), path = %target.display(), stage = "watch", elapsed_us = start.elapsed().as_micros() as u64, success = result.is_ok());
                }
                match result {
                    Ok(()) => {
                        root_watched |= is_root;
                        watchers.push(watcher)
                    }
                    Err(err) => {
                        tracing::debug!(path = %target.display(), error = %err, "diff-sync: watch failed")
                    }
                }
            }
            None => tracing::debug!("diff-sync: watcher unavailable; using repair polling"),
        }
    }
    tracing::debug!(target: "diff_sync::attach", checkout = %identity.root.display(), stage = "coverage", successful = watchers.len(), failed = target_count - watchers.len(), root_watched);
    (watchers, root_watched)
}

/// Whether a watch event can change this checkout's diff. A superproject's
/// `.git` also holds every submodule's and linked worktree's git dir, whose
/// index, object and reflog churn would otherwise re-capture the parent on each
/// foreign git command. A submodule's HEAD/refs still count: they move the
/// parent's gitlink. Anything not known to be irrelevant counts, and the repair
/// tick covers what a skipped event could have missed.
fn kick_relevant(paths: &[PathBuf], git_dir: &Path) -> bool {
    if paths.is_empty() {
        return true;
    }
    paths.iter().any(|path| {
        let Ok(relative) = path.strip_prefix(git_dir) else {
            return true;
        };
        let parts: Vec<_> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect();
        if parts.last().is_some_and(|name| name.ends_with(".lock")) {
            return false;
        }
        match parts.first().map(|part| part.as_ref()) {
            Some("objects" | "logs" | "worktrees" | "FETCH_HEAD" | "ORIG_HEAD") => false,
            Some("modules") if parts.len() > 2 => {
                parts[2..].iter().any(|part| {
                    matches!(part.as_ref(), "HEAD" | "refs" | "packed-refs")
                })
            }
            _ => true,
        }
    })
}

/// Reads cannot change a diff, and Git's own reads must not trigger another scan.
fn is_checkout_change(event: &notify::Event) -> bool {
    !matches!(event.kind, notify::EventKind::Access(_))
}

#[cfg(test)]
mod checkout_change_tests {
    use super::is_checkout_change;
    use notify::event::{AccessKind, AccessMode, DataChange, EventKind, ModifyKind};

    #[test]
    fn reads_do_not_trigger_scans_but_writes_do() {
        for kind in [
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
        ] {
            assert!(!is_checkout_change(&notify::Event::new(kind)));
        }
        for kind in [EventKind::Modify(ModifyKind::Data(DataChange::Any)), EventKind::Any] {
            assert!(is_checkout_change(&notify::Event::new(kind)));
        }
    }
}

/// Per-checkout task: trailing-debounce fs kicks, then compute + publish. Runs
/// syncs sequentially — kicks during a sync accumulate and trigger another pass.
async fn entry_task(
    inner: Weak<DiffSyncInner>,
    entry: Weak<CheckoutEntry>,
    mut kick_rx: mpsc::UnboundedReceiver<()>,
    cancel: CancellationToken,
) {
    while kick_rx.recv().await.is_some() {
        // Trailing debounce: wait for the burst to settle.
        loop {
            match tokio::time::timeout(WATCH_DEBOUNCE, kick_rx.recv()).await {
                Ok(Some(())) => continue,
                Ok(None) => return, // entry closed mid-burst
                Err(_) => break,
            }
        }
        let (Some(inner), Some(entry)) = (inner.upgrade(), entry.upgrade()) else {
            return;
        };
        // Everything sent up to here is covered by this sync; later sends
        // leave the entry unsettled until the next one.
        let seen = entry.kick_tx.sent.load(Ordering::SeqCst);
        // The upgraded Arc would let a sync outlive shutdown — race the token
        // so an in-flight sidecar POST is dropped, not completed.
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = sync_entry(&inner, &entry) => {}
        }
        entry.handled_kicks.fetch_max(seen, Ordering::SeqCst);
        inner.completion_tx.send_modify(|generation| *generation = generation.wrapping_add(1));
        if tracing::enabled!(target: "diff_sync::attach", tracing::Level::DEBUG) {
            let mut timing = lock(&entry.attach_timing);
            if timing.as_ref().is_some_and(|(_, kick)| seen >= *kick)
                && let Some((start, _)) = timing.take()
            {
                tracing::debug!(target: "diff_sync::attach", checkout = %entry.identity.root.display(), stage = "handled", elapsed_us = start.elapsed().as_micros() as u64);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot + publish
// ---------------------------------------------------------------------------

/// Cheap "did anything change" fingerprint for repair-tick kicks: one
/// `git status --porcelain=v2 --branch` (HEAD id, branch, every changed and
/// untracked path) plus the HEAD/index file stamps and the size+mtime of each
/// listed path, which catches an edit to a file that was already modified.
/// `None` (cannot decide: git failed, too many paths, or a submodule is
/// involved) means "run the full capture".
/// Also hands back the `status --porcelain=v2` bytes it read
/// (when complete), so the capture that follows in the same sync can derive its
/// own file list from them instead of spawning a second `git status`.
async fn repair_probe_with_status(identity: &CheckoutIdentity) -> (Option<u64>, Option<Vec<u8>>) {
    let Some(status) = probe_status(identity).await else {
        return (None, None);
    };
    let hash = probe_hash(identity, &status);
    let bytes = (!status.truncated).then_some(status.stdout);
    (hash, bytes)
}

async fn probe_status(identity: &CheckoutIdentity) -> Option<Capture> {
    let status = capture_git(
        &identity.root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
        2 * 1024 * 1024,
    )
    .await
    .ok()?;
    Some(status)
}

fn probe_hash(identity: &CheckoutIdentity, status: &Capture) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    const MAX_PROBED_PATHS: usize = 2_000;
    if status.truncated {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    status.stdout.hash(&mut hasher);
    let stamp = |path: &Path, hasher: &mut std::collections::hash_map::DefaultHasher| {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                meta.len().hash(hasher);
                meta.modified().ok().hash(hasher);
            }
            Err(_) => 0u8.hash(hasher),
        }
    };
    stamp(&identity.git_dir.join("HEAD"), &mut hasher);
    stamp(&identity.git_dir.join("index"), &mut hasher);
    let mut probed = 0usize;
    for record in status.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let path = match record.chars().next()? {
            '#' => continue,
            '?' => record.get(2..)?,
            '1' | 'u' => {
                // `1 XY sub mH mI mW hH hI path` / `u XY sub m1 m2 m3 mW h1 h2 h3 path`
                let fields = if record.starts_with('1') { 9 } else { 11 };
                let sub = record.split(' ').nth(2)?;
                if sub.starts_with('S') {
                    return None; // submodule state lives outside this stat view
                }
                record.splitn(fields, ' ').last()?
            }
            '2' => {
                if record.split(' ').nth(2)?.starts_with('S') {
                    return None;
                }
                record.splitn(10, ' ').last()?
            }
            _ => &record, // a rename's origin path
        };
        probed += 1;
        if probed > MAX_PROBED_PATHS {
            return None;
        }
        stamp(&identity.root.join(path), &mut hasher);
    }
    Some(hasher.finish())
}

async fn sync_entry(inner: &Arc<DiffSyncInner>, entry: &Arc<CheckoutEntry>) {
    // The probe is taken BEFORE the capture and stored only once the capture
    // succeeded. A change landing between the two then leaves the stored probe
    // stale, so the next repair/attach probe sees it and recaptures. (Probing
    // after the capture baked such a change into the baseline and hid it until
    // the checkout changed again.) Every sync probes once, so an idle repair
    // costs exactly one `git status` and no capture ever starts without a
    // baseline being stored with its result.
    let repair = entry.repair_kick.swap(false, Ordering::Relaxed);
    // Read before the probe starts: a probe that begins after attach covers
    // everything the watchers did not see (see `probed_after_attach`).
    if entry.watch_attached.load(Ordering::SeqCst) {
        entry.probed_after_attach.store(true, Ordering::SeqCst);
    }
    if !repair {
        entry.reset_repair_backoff(); // watcher or chat activity
    }
    let (probe, probe_status_v2) = repair_probe_with_status(&entry.identity).await;
    if repair
        && lock(&entry.checksum).is_some()
        && let Some(now) = probe
        && lock(&entry.probe).replace(now) == Some(now)
    {
        entry.quiet_ticks.fetch_add(1, Ordering::Relaxed);
        return; // one `git status` said nothing moved: skip the full capture
    }
    // Spawn savers for the full capture: HEAD comes from the git dir's files and
    // the file list from the probe's status (taken just before, see above).
    // Either falls back to its own git call when unavailable.
    let hints = CaptureHints {
        head: read_head(inner, &entry.identity).1,
        status_v1: probe_status_v2
            .as_deref()
            .and_then(git_status::v2_to_v1),
    };
    let snapshot = match Box::pin(capture_diff_hinted(
        &inner.repos,
        &entry.identity.root,
        None,
        hints,
    ))
    .await
    {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(checkout = %entry.identity.root.display(), error = %err,
                "diff-sync: capture failed");
            git_status::publish(inner, entry, Vec::new(), false);
            return;
        }
    };
    *lock(&entry.probe) = probe;
    if repair {
        // An undecidable probe (a dirty submodule) runs this full capture every
        // tick; an unchanged result counts as quiet so those checkouts back
        // off too, under the same detection bound. A changed one resets.
        if lock(&entry.checksum).as_deref() == Some(snapshot.checksum.as_str()) {
            entry.quiet_ticks.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.reset_repair_backoff();
        }
    }
    if let Some((files, complete)) = &snapshot.git_status {
        git_status::publish(inner, entry, files.clone(), *complete);
    }
    // A branch switch or commit in a target checkout republishes its view.
    publish_targets(inner);
    if lock(&entry.checksum).as_deref() == Some(snapshot.checksum.as_str()) {
        return; // unchanged — publish nothing
    }
    *lock(&entry.checksum) = Some(snapshot.checksum.clone());
    note_head(inner, entry, snapshot.head_sha.as_deref());

    let diff = CheckoutDiff {
        checkout_id: entry.identity.id.clone(),
        device_id: inner.device_id.clone(),
        cwd: entry.identity.root.to_string_lossy().to_string(),
        patch: snapshot.patch.clone(),
        files: snapshot.files.clone(),
        submodules: snapshot.submodules.clone(),
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        truncated: snapshot.truncated,
        checksum: snapshot.checksum.clone(),
        updated_at: chrono::Utc::now(),
    };
    {
        let entries = lock(&inner.entries);
        if !entries.contains_key(&entry.identity.id) {
            return; // closed while computing
        }
    }
    publish_watch_with(inner, Some(diff));

    // Latest-only sidecar to every syncing chat's session DO slot.
    let chats = lock(&entry.chats).clone();
    if let Some(edge) = &inner.edge {
        for chat in &chats {
            let sidecar = DiffSidecar {
                chat_id: chat.id.clone(),
                device_id: inner.device_id.clone(),
                checkout_path: entry.identity.root.to_string_lossy().to_string(),
                branch: Some(snapshot.branch.clone()),
                head_sha: snapshot.head_sha.clone(),
                patch: snapshot.patch.clone(),
                files: snapshot.files.clone(),
                submodules: snapshot.submodules.clone(),
                additions: snapshot.additions,
                deletions: snapshot.deletions,
                truncated: snapshot.truncated,
                published_at: chrono::Utc::now().timestamp_millis(),
            };
            let url = format!("{}/diff/{}", edge.url.trim_end_matches('/'), chat.id);
            // Fresh bearer per request — never the boot-time snapshot.
            let Ok(bearer) = edge.bearer().await else {
                tracing::debug!(chat = %chat.id, "diff-sync: sidecar skipped (token unavailable)");
                continue;
            };
            let result = inner
                .http
                .post(&url)
                .bearer_auth(&bearer)
                .json(&sidecar)
                .send()
                .await;
            match result {
                Ok(response) if !response.status().is_success() => {
                    tracing::debug!(chat = %chat.id, status = %response.status(),
                        "diff-sync: sidecar publish rejected");
                }
                Err(err) => {
                    tracing::debug!(chat = %chat.id, error = %err, "diff-sync: sidecar publish failed");
                }
                Ok(_) => {}
            }
        }
    }
}

fn repository_tips(inner: &DiffSyncInner, git_dir: &Path) -> Arc<Vec<String>> {
    let common_dir = common_dir_of(git_dir);
    let mut caches = lock(&inner.repository_tips);
    if !caches.contains_key(&common_dir) && caches.len() >= 256 {
        caches.clear();
    }
    let entry = caches.entry(common_dir.clone()).or_insert_with(|| RepositoryTips {
        cache: None,
    });
    let tips = crate::provenance::TipsCache::refresh(&mut entry.cache, &common_dir);
    tips
}

/// Compare the captured HEAD with the last one seen for this checkout. A change
/// in a checkout that some chat has bound as a diff target is a HEAD move: hand
/// it to the listener (commit provenance), together with the ref tips as of the
/// PREVIOUS capture, so a push or a new branch made right after a commit cannot
/// hide it. Ref tips are read from the git dir's files (no spawn) and only for
/// bound checkouts. Free when nothing moved and no chat is bound.
fn note_head(inner: &Arc<DiffSyncInner>, entry: &Arc<CheckoutEntry>, head: Option<&str>) {
    let Some(head) = head else { return };
    let now = chrono::Utc::now().timestamp_millis();
    let previous = {
        let mut last = lock(&entry.last_head);
        let previous = last.clone();
        if previous.as_ref().is_some_and(|(sha, _)| sha == head) {
            drop(last);
            // Same HEAD: refresh the tips snapshot for the next move.
            if is_bound_target(inner, entry) && lock(&inner.head_move_listener).is_some() {
                *lock(&entry.last_tips) = Some(repository_tips(inner, &entry.identity.git_dir));
            }
            return;
        }
        *last = Some((head.to_owned(), now));
        previous
    };
    let prior_tips = lock(&entry.last_tips).clone()
        .map(|tips| (*tips).clone())
        .unwrap_or_default();
    let bound = is_bound_target(inner, entry);
    if bound && lock(&inner.head_move_listener).is_some() {
        *lock(&entry.last_tips) = Some(repository_tips(inner, &entry.identity.git_dir));
    }
    let Some((old, since_ms)) = previous else { return };
    let Some(listener) = lock(&inner.head_move_listener).clone() else { return };
    let chats: Vec<String> = lock(&inner.resolved_targets)
        .iter()
        .filter(|chat| {
            chat.targets
                .iter()
                .any(|target| target.identity.id == entry.identity.id)
        })
        .map(|chat| chat.chat_id.clone())
        .collect();
    if chats.is_empty() {
        return;
    }
    listener(crate::provenance::HeadMove {
        root: entry.identity.root.clone(),
        git_dir: entry.identity.git_dir.clone(),
        old,
        new: head.to_owned(),
        chats,
        since_ms,
        prior_tips,
    });
}

fn is_bound_target(inner: &Arc<DiffSyncInner>, entry: &Arc<CheckoutEntry>) -> bool {
    lock(&inner.resolved_targets).iter().any(|chat| {
        chat.targets
            .iter()
            .any(|target| target.identity.id == entry.identity.id)
    })
}

/// Re-emit the watch channel from the current entries' cached diffs, replacing (or
/// inserting) `updated`.
fn publish_watch_with(inner: &Arc<DiffSyncInner>, updated: Option<CheckoutDiff>) {
    let live: HashSet<String> = lock(&inner.entries).keys().cloned().collect();
    inner.diffs_tx.send_modify(|diffs| {
        diffs.retain(|d| live.contains(&d.checkout_id));
        if let Some(updated) = updated {
            match diffs
                .iter_mut()
                .find(|d| d.checkout_id == updated.checkout_id)
            {
                Some(slot) => *slot = updated,
                None => diffs.push(updated),
            }
        }
        diffs.sort_by(|a, b| a.checkout_id.cmp(&b.checkout_id));
    });
}

fn publish_watch(inner: &Arc<DiffSyncInner>) {
    publish_watch_with(inner, None);
    let live: HashSet<String> = lock(&inner.entries).keys().cloned().collect();
    inner.statuses_tx.send_if_modified(|statuses| {
        let before = statuses.len();
        statuses.retain(|status| live.contains(&status.checkout_id));
        before != statuses.len()
    });
}

/// Follows `DiffTargets::changes()`: a bind, selection or unbind reconciles
/// promptly, and a target that newly resolved gets a "Latest turn" base when
/// the chat has none for that checkout (covers binds from the agent tool).
async fn diff_targets_task(
    inner: Weak<DiffSyncInner>,
    mut changes: watch::Receiver<u64>,
    cancel: CancellationToken,
) {
    if let Some(inner) = inner.upgrade() {
        let chats = inner.workspace.watch_chats().borrow().clone();
        reconcile(&inner, chats, false).await;
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
                changes.borrow_and_update();
                let Some(inner) = inner.upgrade() else { break };
                let known = resolved_target_keys(&inner);
                let chats = inner.workspace.watch_chats().borrow().clone();
                reconcile(&inner, chats, false).await;
                for (chat_id, root) in resolved_target_keys(&inner).difference(&known) {
                    snapshot_root_if_missing(&inner, chat_id, root).await;
                }
            }
        }
    }
}

fn resolved_target_keys(inner: &Arc<DiffSyncInner>) -> HashSet<(String, PathBuf)> {
    lock(&inner.resolved_targets)
        .iter()
        .flat_map(|chat| {
            chat.targets
                .iter()
                .map(|target| (chat.chat_id.clone(), target.identity.root.clone()))
        })
        .collect()
}

/// Chat-watch follower + repair tick. Holds only weak handles so dropping the
/// service tears the loop down; the token ends it eagerly on shutdown.
async fn diff_sync_task(
    inner: Weak<DiffSyncInner>,
    mut chats_rx: watch::Receiver<Vec<Chat>>,
    cancel: CancellationToken,
) {
    let mut repair = tokio::time::interval(REPAIR_INTERVAL);
    repair.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    repair.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            changed = chats_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(inner) = inner.upgrade() else { break };
                let chats = chats_rx.borrow_and_update().clone();
                // Memoized identities only: chat rows change constantly (the
                // sync itself writes them) and must never fan out into git.
                reconcile(&inner, chats, false).await;
            }
            _ = repair.tick() => {
                let Some(inner) = inner.upgrade() else { break };
                let chats = chats_rx.borrow().clone();
                // Fresh: revalidate every memoized identity against git.
                reconcile(&inner, chats, true).await;
                for entry in lock(&inner.entries).values() {
                    entry.kick_for_repair();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Diff capture (exposed for tests)
// ---------------------------------------------------------------------------

struct Capture {
    stdout: Vec<u8>,
    truncated: bool,
}

/// Run git capturing stdout under a hard byte ceiling — the child is killed once
/// the cap is hit, so an arbitrarily large repository diff never buffers fully.
fn capture_git<'a>(
    cwd: &'a Path,
    args: &'a [&'a str],
    max_bytes: usize,
) -> BoxFuture<'a, Result<Capture, EngineError>> {
    Box::pin(async move {
        let mut cmd = mast_harness::git_command_async();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(0x08000000);
        }
        cmd.arg("-C").arg(cwd).args(args);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Engine-wide git gate; held until the child is done. Leaf call: no
        // caller holds another permit while waiting on this one.
        let _permit = crate::git_gate::acquire("diff_sync").await;
        let mut child = cmd
            .spawn()
            .map_err(|e| EngineError::Other(format!("git spawn failed: {e}")))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Other("git stdout unavailable".into()))?;
        let mut out: Vec<u8> = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        let mut truncated = false;
        loop {
            let n = stdout
                .read(&mut buf)
                .await
                .map_err(|e| EngineError::Other(format!("git read failed: {e}")))?;
            if n == 0 {
                break;
            }
            let remaining = max_bytes.saturating_sub(out.len());
            if n > remaining {
                out.extend_from_slice(&buf[..remaining]);
                truncated = true;
                let _ = child.start_kill();
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| EngineError::Other(format!("git wait failed: {e}")))?;
        if !output.status.success() && !truncated {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr.trim();
            return Err(EngineError::Other(if message.is_empty() {
                format!("git exited {}", output.status)
            } else {
                format!("git: {message}")
            }));
        }
        Ok(Capture {
            stdout: out,
            truncated,
        })
    })
}

fn split_z(value: &[u8]) -> Vec<String> {
    value
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect()
}

/// Number of NUL-separated path fields that follow a raw record's header
/// for status letter `status` (renames and copies carry two).
fn raw_path_fields(status: &str) -> usize {
    if status.starts_with('R') || status.starts_with('C') {
        2
    } else {
        1
    }
}

/// Split the output of `git diff --raw --numstat -z` into its raw block and
/// its numstat block (raw records start with `:`; a record's path fields are
/// consumed by count, so a path that begins with `:` cannot end the block).
fn split_raw_numstat(value: &[u8]) -> (&[u8], &[u8]) {
    let mut offset = 0usize;
    while value.get(offset) == Some(&b':') {
        let Some(end) = value[offset..].iter().position(|b| *b == 0) else {
            return (value, &[]);
        };
        let header = String::from_utf8_lossy(&value[offset..offset + end]);
        let status = header.rsplit(' ').next().unwrap_or_default().to_owned();
        offset += end + 1;
        for _ in 0..raw_path_fields(&status) {
            match value[offset.min(value.len())..].iter().position(|b| *b == 0) {
                Some(end) => offset += end + 1,
                None => return (value, &[]),
            }
        }
    }
    (&value[..offset], &value[offset..])
}

/// `--name-status -z` bytes rebuilt from a `--raw -z` block.
fn raw_to_name_status(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() / 4);
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(header) = fields.next() {
        let header = String::from_utf8_lossy(header);
        let status = header.rsplit(' ').next().unwrap_or_default();
        out.extend_from_slice(status.as_bytes());
        out.push(0);
        for _ in 0..raw_path_fields(status) {
            if let Some(path) = fields.next() {
                out.extend_from_slice(path);
                out.push(0);
            }
        }
    }
    out
}

fn parse_name_status(value: &[u8]) -> Vec<DiffFileSummary> {
    let fields = split_z(value);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < fields.len() {
        let raw = fields[i].clone();
        i += 1;
        let code = raw.chars().next().unwrap_or('M');
        let Some(first) = fields.get(i).cloned() else {
            break;
        };
        i += 1;
        let renamed = code == 'R' || code == 'C';
        let second = if renamed {
            let s = fields.get(i).cloned();
            i += 1;
            s
        } else {
            None
        };
        let status = match code {
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            'U' => "unmerged",
            _ => "modified",
        };
        out.push(DiffFileSummary {
            path: second.clone().unwrap_or_else(|| first.clone()),
            old_path: second.is_some().then_some(first),
            status: status.to_string(),
            additions: 0,
            deletions: 0,
            binary: false,
        });
    }
    out
}

fn apply_numstat(files: &mut [DiffFileSummary], value: &[u8]) {
    // With -z, a rename record is `adds<TAB>dels<TAB><NUL>old<NUL>new<NUL>`.
    let records: Vec<String> = value
        .split(|b| *b == 0)
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect();
    let mut i = 0usize;
    while i < records.len() {
        let record = &records[i];
        if record.is_empty() {
            i += 1;
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let adds = parts.next().unwrap_or_default().to_string();
        let dels = parts.next().unwrap_or_default().to_string();
        let inline_path = parts.next().unwrap_or_default().to_string();
        let path = if inline_path.is_empty() {
            // Rename: the next two records are old, new.
            let new_path = records.get(i + 2).cloned().unwrap_or_default();
            i += 2;
            new_path
        } else {
            inline_path
        };
        i += 1;
        if let Some(file) = files.iter_mut().find(|f| f.path == path) {
            file.additions = adds.parse().unwrap_or(0);
            file.deletions = dels.parse().unwrap_or(0);
            file.binary = adds == "-" || dels == "-";
        }
    }
}

#[derive(Debug, Clone)]
struct RawDiffEntry {
    old_mode: u32,
    new_mode: u32,
    old_oid: String,
    new_oid: String,
    status: char,
    old_path: Option<String>,
    path: String,
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn zero_object_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte == b'0')
}

/// Parse `git diff --raw -z --no-abbrev`. With `-z`, the header, old path,
/// and (for rename/copy) new path are separate NUL records.
fn parse_raw_diff(value: &[u8]) -> Result<Vec<RawDiffEntry>, EngineError> {
    let records: Vec<&[u8]> = value.split(|byte| *byte == 0).collect();
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i < records.len() {
        let header = records[i];
        i += 1;
        if header.is_empty() {
            continue;
        }
        let header = std::str::from_utf8(header)
            .map_err(|_| EngineError::Other("invalid raw diff header".into()))?;
        let header = header
            .strip_prefix(':')
            .ok_or_else(|| EngineError::Other("invalid raw diff record".into()))?;
        let mut fields = header.split_ascii_whitespace();
        let old_mode = u32::from_str_radix(fields.next().unwrap_or_default(), 8)
            .map_err(|_| EngineError::Other("invalid raw diff old mode".into()))?;
        let new_mode = u32::from_str_radix(fields.next().unwrap_or_default(), 8)
            .map_err(|_| EngineError::Other("invalid raw diff new mode".into()))?;
        let old_oid = fields.next().unwrap_or_default().to_ascii_lowercase();
        let new_oid = fields.next().unwrap_or_default().to_ascii_lowercase();
        let status_field = fields.next().unwrap_or_default();
        let status = status_field.chars().next().unwrap_or('M');
        if !valid_object_id(&old_oid) || !valid_object_id(&new_oid) {
            return Err(EngineError::Other("invalid raw diff object id".into()));
        }
        let first = records
            .get(i)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| EngineError::Other("raw diff path missing".into()))?;
        i += 1;
        let first = String::from_utf8_lossy(first).into_owned();
        let renamed = matches!(status, 'R' | 'C');
        let second = if renamed {
            let path = records
                .get(i)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| EngineError::Other("raw diff destination missing".into()))?;
            i += 1;
            Some(String::from_utf8_lossy(path).into_owned())
        } else {
            None
        };
        entries.push(RawDiffEntry {
            old_mode,
            new_mode,
            old_oid,
            new_oid,
            status,
            old_path: renamed.then_some(first.clone()),
            path: second.unwrap_or(first),
        });
    }
    Ok(entries)
}

fn join_git_path(parent: Option<&str>, child: &str) -> String {
    match parent {
        Some(parent) if !parent.is_empty() => format!("{parent}/{child}"),
        _ => child.to_string(),
    }
}

async fn declared_submodule(parent_root: &Path, path: &str, target_revision: Option<&str>) -> bool {
    let config = if let Some(target_revision) = target_revision {
        if !valid_object_id(target_revision) {
            return false;
        }
        let blob = format!("{target_revision}:.gitmodules");
        Box::pin(capture_git(
            parent_root,
            &[
                "config",
                "--null",
                "--blob",
                &blob,
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ],
            256 * 1024,
        ))
        .await
    } else {
        let modules = parent_root.join(".gitmodules");
        let Ok(metadata) = tokio::fs::symlink_metadata(&modules).await else {
            return false;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return false;
        }
        let modules = modules.to_string_lossy().into_owned();
        Box::pin(capture_git(
            parent_root,
            &[
                "config",
                "--null",
                "--file",
                &modules,
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ],
            256 * 1024,
        ))
        .await
    };
    let Ok(config) = config else {
        return false;
    };
    config.stdout.split(|byte| *byte == 0).any(|record| {
        record
            .iter()
            .position(|byte| *byte == b'\n')
            .is_some_and(|separator| &record[separator + 1..] == path.as_bytes())
    })
}

/// Resolve an initialized nested repository without following any symlink and
/// without allowing an uninitialized directory to fall through to its parent.
async fn resolve_submodule_root(
    checkout_root: &Path,
    repository_path: &str,
) -> Result<PathBuf, EngineError> {
    let relative = validate_diff_path(repository_path)?;
    let canonical_checkout = tokio::fs::canonicalize(checkout_root)
        .await
        .map_err(|error| EngineError::Other(format!("canonical checkout: {error}")))?;
    let mut candidate = checkout_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(EngineError::Other("submodule path escapes checkout".into()));
        };
        candidate.push(component);
        let metadata = tokio::fs::symlink_metadata(&candidate)
            .await
            .map_err(|error| EngineError::Other(format!("submodule unavailable: {error}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(EngineError::Other(
                "submodule path is not a real directory".into(),
            ));
        }
    }
    let canonical = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|error| EngineError::Other(format!("canonical submodule: {error}")))?;
    if canonical == canonical_checkout || !canonical.starts_with(&canonical_checkout) {
        return Err(EngineError::Other("submodule escapes checkout".into()));
    }
    let top = Box::pin(capture_git(
        &canonical,
        &["rev-parse", "--show-toplevel"],
        16 * 1024,
    ))
    .await?;
    let top = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim().to_string());
    let canonical_top = tokio::fs::canonicalize(top)
        .await
        .map_err(|error| EngineError::Other(format!("canonical submodule root: {error}")))?;
    if canonical_top != canonical {
        return Err(EngineError::Other(
            "submodule repository root does not match its path".into(),
        ));
    }
    Ok(canonical)
}

async fn commit_exists(root: &Path, revision: &str) -> bool {
    if !valid_object_id(revision) || zero_object_id(revision) {
        return false;
    }
    let spec = format!("{revision}^{{commit}}");
    Box::pin(capture_git(root, &["cat-file", "-e", &spec], 1))
        .await
        .is_ok()
}

struct SubmoduleBudget {
    remaining_patch: usize,
    remaining_metadata: usize,
    sections: usize,
    truncated: bool,
}

impl SubmoduleBudget {
    fn metadata_cap(&self) -> usize {
        self.remaining_metadata
    }

    fn consume_metadata(&mut self, capture: &Capture) {
        self.remaining_metadata = self.remaining_metadata.saturating_sub(capture.stdout.len());
        if capture.truncated {
            self.remaining_metadata = 0;
            self.truncated = true;
        }
    }
}

struct PendingSubmodules {
    parent_root: PathBuf,
    parent_repository_path: Option<String>,
    entries: Vec<RawDiffEntry>,
    worktree_target: bool,
    declaration_target: Option<String>,
    depth: usize,
}

async fn capture_submodule_sections(
    checkout_root: &Path,
    raw: &Capture,
    worktree_target: bool,
    target_revision: Option<&str>,
    remaining_patch: usize,
) -> Result<(Vec<SubmoduleDiffSummary>, bool), EngineError> {
    if raw.truncated {
        return Ok((Vec::new(), true));
    }
    let mut budget = SubmoduleBudget {
        remaining_patch,
        remaining_metadata: MAX_SUBMODULE_METADATA_BYTES,
        sections: 0,
        truncated: false,
    };
    let entries = match parse_raw_diff(&raw.stdout) {
        Ok(entries) => entries,
        Err(_) => return Ok((Vec::new(), true)),
    };
    let mut pending = std::collections::VecDeque::from([PendingSubmodules {
        parent_root: checkout_root.to_path_buf(),
        parent_repository_path: None,
        entries,
        worktree_target,
        declaration_target: target_revision.map(str::to_string),
        depth: 1,
    }]);
    let mut sections = Vec::new();

    while let Some(batch) = pending.pop_front() {
        for entry in batch.entries {
            if entry.old_mode != 0o160000 && entry.new_mode != 0o160000 {
                continue;
            }
            if budget.sections >= MAX_EXPANDED_SUBMODULES {
                budget.truncated = true;
                break;
            }
            budget.sections += 1;
            let repository_path =
                join_git_path(batch.parent_repository_path.as_deref(), &entry.path);
            let old_revision = (!zero_object_id(&entry.old_oid)).then(|| entry.old_oid.clone());
            let mut new_revision = (!zero_object_id(&entry.new_oid)).then(|| entry.new_oid.clone());
            let mut section = SubmoduleDiffSummary {
                repository_path: repository_path.clone(),
                parent_repository_path: batch.parent_repository_path.clone(),
                old_revision,
                new_revision: new_revision.clone(),
                expanded: false,
                patch: String::new(),
                files: Vec::new(),
                additions: 0,
                deletions: 0,
                truncated: false,
            };

            // Added/removed/type-changed/renamed gitlinks remain SHA-only.
            if entry.old_mode != 0o160000
                || entry.new_mode != 0o160000
                || entry.old_path.is_some()
                || !matches!(entry.status, 'M' | 'T')
                || batch.depth > MAX_SUBMODULE_DEPTH
            {
                if batch.depth > MAX_SUBMODULE_DEPTH {
                    budget.truncated = true;
                }
                sections.push(section);
                continue;
            }
            if !Box::pin(declared_submodule(
                &batch.parent_root,
                &entry.path,
                batch.declaration_target.as_deref(),
            ))
            .await
            {
                sections.push(section);
                continue;
            }
            let nested_root =
                match Box::pin(resolve_submodule_root(&batch.parent_root, &entry.path)).await {
                    Ok(root) => root,
                    Err(_) => {
                        sections.push(section);
                        continue;
                    }
                };
            if batch.worktree_target {
                new_revision = Box::pin(capture_git(
                    &nested_root,
                    &["rev-parse", "--verify", "HEAD"],
                    256,
                ))
                .await
                .ok()
                .map(|capture| String::from_utf8_lossy(&capture.stdout).trim().to_string())
                .filter(|revision| valid_object_id(revision));
                section.new_revision = new_revision.clone();
            }
            let (Some(old_revision), Some(new_revision)) =
                (section.old_revision.as_deref(), new_revision.as_deref())
            else {
                sections.push(section);
                continue;
            };
            if old_revision == new_revision {
                sections.push(section);
                continue;
            }

            if budget.remaining_metadata == 0 {
                section.truncated = true;
                budget.truncated = true;
                sections.push(section);
                continue;
            }

            let names = match Box::pin(capture_git(
                &nested_root,
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    "--find-renames",
                    old_revision,
                    new_revision,
                    "--",
                ],
                budget.metadata_cap(),
            ))
            .await
            {
                Ok(value) => value,
                Err(_) => {
                    sections.push(section);
                    continue;
                }
            };
            budget.consume_metadata(&names);
            let nums = if budget.remaining_metadata == 0 {
                None
            } else {
                match Box::pin(capture_git(
                    &nested_root,
                    &[
                        "diff",
                        "--numstat",
                        "-z",
                        "--find-renames",
                        old_revision,
                        new_revision,
                        "--",
                    ],
                    budget.metadata_cap(),
                ))
                .await
                {
                    Ok(value) => {
                        budget.consume_metadata(&value);
                        Some(value)
                    }
                    Err(_) => {
                        sections.push(section);
                        continue;
                    }
                }
            };
            let patch = match Box::pin(capture_git(
                &nested_root,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-color",
                    "--find-renames",
                    "--unified=3",
                    old_revision,
                    new_revision,
                    "--",
                ],
                budget.remaining_patch,
            ))
            .await
            {
                Ok(value) => value,
                Err(_) => {
                    sections.push(section);
                    continue;
                }
            };
            let nested_raw = if budget.remaining_metadata == 0 {
                None
            } else {
                let capture = Box::pin(capture_git(
                    &nested_root,
                    &[
                        "diff",
                        "--raw",
                        "-z",
                        "--no-abbrev",
                        "--find-renames",
                        old_revision,
                        new_revision,
                        "--",
                    ],
                    budget.metadata_cap(),
                ))
                .await
                .ok();
                if let Some(capture) = capture.as_ref() {
                    budget.consume_metadata(capture);
                }
                capture
            };

            section.files = parse_name_status(&names.stdout);
            if let Some(nums) = nums.as_ref() {
                apply_numstat(&mut section.files, &nums.stdout);
            }
            section.patch = String::from_utf8_lossy(&patch.stdout).into_owned();
            section.truncated = names.truncated
                || nums.is_none()
                || nums.as_ref().is_some_and(|nums| nums.truncated)
                || patch.truncated
                || nested_raw.is_none()
                || nested_raw.as_ref().is_some_and(|raw| raw.truncated);
            if patch.truncated {
                let boundary = section.patch.rfind('\n').unwrap_or(0);
                section.patch.truncate(boundary);
            }
            budget.remaining_patch = budget.remaining_patch.saturating_sub(section.patch.len());
            section.additions = section.files.iter().map(|file| file.additions).sum();
            section.deletions = section.files.iter().map(|file| file.deletions).sum();
            section.expanded = true;
            budget.truncated |= section.truncated;

            if let Some(raw) = nested_raw.filter(|raw| !raw.truncated) {
                match parse_raw_diff(&raw.stdout) {
                    Ok(entries) if !entries.is_empty() => {
                        pending.push_back(PendingSubmodules {
                            parent_root: nested_root,
                            parent_repository_path: Some(repository_path),
                            entries,
                            worktree_target: false,
                            declaration_target: Some(new_revision.to_string()),
                            depth: batch.depth + 1,
                        });
                    }
                    Ok(_) => {}
                    Err(_) => {
                        section.truncated = true;
                        budget.truncated = true;
                    }
                }
            }
            sections.push(section);
        }
    }
    Ok((sections, budget.truncated))
}

fn quote_patch_path(path: &str) -> String {
    if path
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""))
    } else {
        path.to_string()
    }
}

/// Synthesize a new-file hunk for an untracked file (git diff never shows them).
fn untracked_patch(path: &str, content: &str) -> String {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let body: String = lines
        .iter()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let a = quote_patch_path(&format!("a/{path}"));
    let b = quote_patch_path(&format!("b/{path}"));
    format!(
        "diff --git {a} {b}\nnew file mode 100644\n--- /dev/null\n+++ {b}\n@@ -0,0 +1,{} @@\n{body}\n",
        lines.len()
    )
}

fn validate_diff_path(path: &str) -> Result<&Path, EngineError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EngineError::Other("diff path escapes checkout".into()));
    }
    Ok(path)
}

fn decode_diff_source(
    bytes: Vec<u8>,
) -> Result<(Option<String>, Option<String>, bool), EngineError> {
    if bytes.contains(&0) {
        return Ok((None, None, true));
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return Ok((None, None, true)),
    };
    let hash = crate::repos::hex(&Sha256::digest(text.as_bytes()));
    Ok((Some(text), Some(hash), false))
}

async fn read_worktree_source(root: &Path, path: &Path) -> Result<Capture, EngineError> {
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| EngineError::Other(format!("canonical checkout: {error}")))?;
    let full = root.join(path);
    let metadata = tokio::fs::symlink_metadata(&full)
        .await
        .map_err(|error| EngineError::Other(format!("read diff file metadata: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(EngineError::Other(
            "diff source is not a regular checkout file".into(),
        ));
    }
    let canonical = tokio::fs::canonicalize(&full)
        .await
        .map_err(|error| EngineError::Other(format!("canonical diff file: {error}")))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(EngineError::Other("diff source escapes checkout".into()));
    }
    if metadata.len() > MAX_DIFF_SOURCE_BYTES as u64 {
        return Ok(Capture {
            stdout: Vec::new(),
            truncated: true,
        });
    }
    let stdout = tokio::fs::read(&canonical)
        .await
        .map_err(|error| EngineError::Other(format!("read diff file: {error}")))?;
    Ok(Capture {
        stdout,
        truncated: false,
    })
}

async fn read_git_source(root: &Path, revision: &str, path: &Path) -> Result<Capture, EngineError> {
    let spec = format!("{revision}:{}", path.to_string_lossy());
    capture_git(root, &["cat-file", "blob", &spec], MAX_DIFF_SOURCE_BYTES).await
}

/// Read the exact old/new documents for one file in a previously captured diff.
/// Paths must come from that snapshot's file summary; callers still recheck the
/// snapshot checksum after this read to close the filesystem race.
pub async fn read_diff_file_text(
    root: &Path,
    base: &str,
    file: &DiffFileSummary,
) -> Result<DiffFileTextPair, EngineError> {
    read_diff_file_text_at(root, base, None, file).await
}

/// Read the exact old/new documents for one file in a diff between `base` and
/// an optional committed target. Without a target, the new source is the live
/// working tree; with one, both sources are immutable Git blobs.
pub(crate) async fn read_diff_file_text_at(
    root: &Path,
    base: &str,
    target: Option<&str>,
    file: &DiffFileSummary,
) -> Result<DiffFileTextPair, EngineError> {
    let new_path = validate_diff_path(&file.path)?;
    let old_path = validate_diff_path(file.old_path.as_deref().unwrap_or(&file.path))?;

    let old = if file.status == "added" {
        None
    } else {
        Some(read_git_source(root, base, old_path).await?)
    };
    let new = if file.status == "deleted" {
        None
    } else if let Some(target) = target {
        Some(read_git_source(root, target, new_path).await?)
    } else {
        Some(read_worktree_source(root, new_path).await?)
    };
    let truncated = old.as_ref().is_some_and(|source| source.truncated)
        || new.as_ref().is_some_and(|source| source.truncated);
    if truncated {
        return Ok(DiffFileTextPair {
            old_text: None,
            new_text: None,
            old_content_hash: None,
            new_content_hash: None,
            binary: false,
            truncated: true,
        });
    }
    let (old_text, old_content_hash, old_binary) = match old {
        Some(source) => decode_diff_source(source.stdout)?,
        None => (None, None, false),
    };
    let (new_text, new_content_hash, new_binary) = match new {
        Some(source) => decode_diff_source(source.stdout)?,
        None => (None, None, false),
    };
    let binary = old_binary || new_binary || file.binary;
    Ok(DiffFileTextPair {
        old_text: (!binary).then_some(old_text).flatten(),
        new_text: (!binary).then_some(new_text).flatten(),
        old_content_hash: (!binary).then_some(old_content_hash).flatten(),
        new_content_hash: (!binary).then_some(new_content_hash).flatten(),
        binary,
        truncated: false,
    })
}

/// Read a file from a server-produced submodule section. The repository path
/// and revisions are revalidated locally; callers must still perform the same
/// before/after snapshot checksum checks as root-file reads.
pub(crate) async fn read_submodule_diff_file_text(
    checkout_root: &Path,
    section: &SubmoduleDiffSummary,
    file: &DiffFileSummary,
) -> Result<DiffFileTextPair, EngineError> {
    if !section.expanded {
        return Err(EngineError::Other("submodule diff is unavailable".into()));
    }
    let old = section
        .old_revision
        .as_deref()
        .filter(|revision| valid_object_id(revision) && !zero_object_id(revision))
        .ok_or_else(|| EngineError::Other("submodule old revision unavailable".into()))?;
    let new = section
        .new_revision
        .as_deref()
        .filter(|revision| valid_object_id(revision) && !zero_object_id(revision))
        .ok_or_else(|| EngineError::Other("submodule new revision unavailable".into()))?;
    let root = Box::pin(resolve_submodule_root(
        checkout_root,
        &section.repository_path,
    ))
    .await?;
    if !Box::pin(commit_exists(&root, old)).await || !Box::pin(commit_exists(&root, new)).await {
        return Err(EngineError::Other(
            "submodule diff objects are unavailable".into(),
        ));
    }
    Box::pin(read_diff_file_text_at(&root, old, Some(new), file)).await
}

/// Resolve the parent used as a commit diff's old side. Root commits compare
/// against Git's canonical empty tree.
pub(crate) async fn commit_diff_base(root: &Path, sha: &str) -> String {
    let parent_spec = format!("{sha}^");
    let parent = capture_git(root, &["rev-parse", "--verify", &parent_spec], 256)
        .await
        .map(|capture| String::from_utf8_lossy(&capture.stdout).trim().to_string())
        .unwrap_or_default();
    if parent.is_empty() {
        EMPTY_TREE_SHA.to_string()
    } else {
        parent
    }
}

pub async fn working_diff_base(root: &Path) -> Result<String, EngineError> {
    let head = capture_git(root, &["rev-parse", "--verify", "HEAD"], 256)
        .await
        .map(|capture| String::from_utf8_lossy(&capture.stdout).trim().to_string())
        .unwrap_or_default();
    Ok(if head.is_empty() {
        EMPTY_TREE_SHA.into()
    } else {
        head
    })
}

/// One bounded atomic snapshot: tracked diff vs HEAD (or the empty tree) with
/// renames, plus untracked files (via `git status --porcelain`, index untouched)
/// as synthesized new-file hunks. 3MiB patch cap with a `truncated` flag; sha256
/// checksum over branch ‖ head ‖ patch ‖ files ‖ truncated.
pub async fn capture_diff(repos: &Repos, root: &Path) -> Result<DiffSnapshot, EngineError> {
    Box::pin(capture_diff_against(repos, root, None)).await
}

/// [`capture_diff`] with the diff base overridable: `None` keeps the
/// working-tree behavior (vs HEAD / the empty tree); `Some(committish)` diffs
/// the working tree against that base instead ("Branch changes" passes the
/// merge-base with the comparison ref). Untracked files synthesize as new
/// either way — they are new relative to any committed base.
pub async fn capture_diff_against(
    repos: &Repos,
    root: &Path,
    base_override: Option<&str>,
) -> Result<DiffSnapshot, EngineError> {
    Box::pin(capture_diff_hinted(repos, root, base_override, CaptureHints::default())).await
}

/// Answers the caller already has, so the capture can skip the matching git call.
#[derive(Default)]
struct CaptureHints {
    /// HEAD commit id read from the git dir's files.
    head: Option<String>,
    /// `status --porcelain=v1 -z --untracked-files=all` bytes (complete, untruncated).
    status_v1: Option<Vec<u8>>,
}

async fn capture_diff_hinted(
    repos: &Repos,
    root: &Path,
    base_override: Option<&str>,
    hints: CaptureHints,
) -> Result<DiffSnapshot, EngineError> {
    let head = match hints.head.filter(|h| valid_object_id(h)) {
        Some(head) => head,
        None => capture_git(root, &["rev-parse", "--verify", "HEAD"], 256)
            .await
            .map(|c| String::from_utf8_lossy(&c.stdout).trim().to_string())
            .unwrap_or_default(),
    };
    let base: &str = match base_override {
        Some(base) => base,
        None if head.is_empty() => EMPTY_TREE_SHA,
        None => &head,
    };
    let branch = repos
        .current_branch(root)
        .await
        .unwrap_or_else(|_| "HEAD".into());

    // One process answers three questions: `--raw` (modes, ids, status), its
    // name-status view and `--numstat`. Git prints the raw block first, then
    // the numstat block.
    let combined = capture_git(
        root,
        &[
            "diff",
            "--raw",
            "--numstat",
            "-z",
            "--no-abbrev",
            "--find-renames",
            base,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let (raw_bytes, num_bytes) = split_raw_numstat(&combined.stdout);
    let names = Capture {
        stdout: raw_to_name_status(raw_bytes),
        truncated: combined.truncated,
    };
    let nums = Capture {
        stdout: num_bytes.to_vec(),
        truncated: combined.truncated,
    };
    let raw = Capture {
        stdout: raw_bytes.to_vec(),
        truncated: combined.truncated,
    };
    let tracked = capture_git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            "--unified=3",
            base,
            "--",
        ],
        MAX_PATCH_BYTES,
    )
    .await?;
    // Untracked listing via porcelain status; `--no-optional-locks` keeps this
    // read-only (a status-triggered index refresh would re-kick our own watcher).
    let status = match hints.status_v1 {
        Some(stdout) => Capture { stdout, truncated: false },
        None => {
            capture_git(
                root,
                &[
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                ],
                2 * 1024 * 1024,
            )
            .await?
        }
    };

    let mut files = parse_name_status(&names.stdout);
    apply_numstat(&mut files, &nums.stdout);
    let mut patch = String::from_utf8_lossy(&tracked.stdout).to_string();
    let mut truncated =
        tracked.truncated || names.truncated || nums.truncated || raw.truncated || status.truncated;

    if tracked.truncated {
        let boundary = patch.rfind('\n').unwrap_or(0);
        patch.truncate(boundary);
        patch.push_str("\n# Mast diff truncated\n");
    }

    // `?? path` records; rename records (`R  new\0old`) consume their extra field.
    let mut untracked: Vec<String> = Vec::new();
    let records = split_z(&status.stdout);
    let mut i = 0usize;
    while i < records.len() {
        let record = &records[i];
        i += 1;
        if record.len() < 3 {
            continue;
        }
        let (code, path) = record.split_at(2);
        if code.starts_with('R') || code.starts_with('C') {
            i += 1; // skip the origin-path field
        }
        if code == "??" {
            untracked.push(path.trim_start().to_string());
        }
    }
    untracked.sort();

    // Status now enumerates new directories as individual paths. Keep content
    // work bounded even when thousands of those files are binary or too large
    // to fit in the patch; their Git statuses remain complete independently.
    let mut untracked_budget = MAX_PATCH_BYTES;
    for path in untracked {
        if untracked_budget == 0 {
            truncated = true;
            break;
        }
        let full = root.join(&path);
        let binary;
        let mut additions = 0u32;
        let Ok(metadata) = tokio::fs::symlink_metadata(&full).await else {
            continue;
        };
        let size = metadata.len();
        if size > MAX_PATCH_BYTES as u64 {
            binary = true;
            truncated = true;
        } else {
            let content = if metadata.file_type().is_symlink() {
                // A Git symlink's content is its target path, never the target file.
                tokio::fs::read_link(&full)
                    .await
                    .map(|path| path.to_string_lossy().into_owned().into_bytes())
            } else {
                async {
                    let file = tokio::fs::File::open(&full).await?;
                    let mut bytes = Vec::new();
                    file.take(untracked_budget as u64 + 1)
                        .read_to_end(&mut bytes)
                        .await?;
                    Ok::<_, std::io::Error>(bytes)
                }
                .await
            };
            match content {
                Ok(bytes) => {
                    if bytes.len() > untracked_budget {
                        truncated = true;
                        break;
                    }
                    untracked_budget = untracked_budget.saturating_sub(bytes.len().max(1));
                    binary = bytes.contains(&0);
                    if !binary {
                        let text = String::from_utf8_lossy(&bytes).to_string();
                        additions = if text.is_empty() {
                            0
                        } else {
                            (text.split('\n').count() - usize::from(text.ends_with('\n'))) as u32
                        };
                        let addition = untracked_patch(&path, &text);
                        if patch.len() + addition.len() <= MAX_PATCH_BYTES {
                            if !patch.is_empty() && !patch.ends_with('\n') {
                                patch.push('\n');
                            }
                            patch.push_str(&addition);
                        } else {
                            truncated = true;
                        }
                    }
                }
                Err(_) => continue, // vanished between status and read
            }
        }
        files.push(DiffFileSummary {
            path,
            old_path: None,
            status: "added".to_string(),
            additions,
            deletions: 0,
            binary,
        });
    }

    let (submodules, submodules_truncated) = Box::pin(capture_submodule_sections(
        root,
        &raw,
        true,
        None,
        MAX_PATCH_BYTES.saturating_sub(patch.len()),
    ))
    .await?;
    truncated |= submodules_truncated;
    let additions: u32 = files.iter().map(|f| f.additions).sum();
    let deletions: u32 = files.iter().map(|f| f.deletions).sum();
    let files_json = serde_json::to_string(&files)
        .map_err(|e| EngineError::Other(format!("diff files serialize: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(branch.as_bytes());
    hasher.update([0u8]);
    hasher.update(head.as_bytes());
    hasher.update([0u8]);
    hasher.update(patch.as_bytes());
    hasher.update([0u8]);
    hasher.update(files_json.as_bytes());
    hasher.update([0u8]);
    let submodules_json = serde_json::to_string(&submodules)
        .map_err(|e| EngineError::Other(format!("submodule diffs serialize: {e}")))?;
    hasher.update(submodules_json.as_bytes());
    hasher.update(if truncated { b"1" } else { b"0" });
    let checksum = crate::repos::hex(&hasher.finalize());

    Ok(DiffSnapshot {
        git_status: Some(git_status::parse(&status.stdout, status.truncated)),
        branch,
        head_sha: (!head.is_empty()).then_some(head),
        patch,
        files,
        submodules,
        additions,
        deletions,
        truncated,
        checksum,
    })
}

/// Snapshot of one COMMIT's changes: first-parent (or the empty tree for a
/// root commit) diffed against the commit itself — the History pane's
/// per-commit tab. Commit-to-commit only: no working tree, no untracked
/// synthesis.
pub async fn capture_commit_diff(
    repos: &Repos,
    root: &Path,
    sha: &str,
) -> Result<DiffSnapshot, EngineError> {
    let base = commit_diff_base(root, sha).await;
    capture_revision_pair_diff(repos, root, &base, sha).await
}

/// The two commits a "Diff branch / PR…" range compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeShas {
    /// `merge-base(base_ref, head_ref)`.
    pub merge_base: String,
    /// The head ref's commit.
    pub head: String,
}

/// Resolve `merge-base(base_ref, head_ref)...head_ref` to commits. Read-only;
/// errors when a ref is unknown or the histories are unrelated.
pub async fn resolve_range(
    root: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<RangeShas, EngineError> {
    let verify = |name: &str| format!("{name}^{{commit}}");
    let head = capture_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &verify(head_ref),
        ],
        256,
    )
    .await
    .map(|c| String::from_utf8_lossy(&c.stdout).trim().to_string())
    .map_err(|_| EngineError::Other(format!("{head_ref} does not exist in this repository")))?;
    let base = capture_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &verify(base_ref),
        ],
        256,
    )
    .await
    .map(|c| String::from_utf8_lossy(&c.stdout).trim().to_string())
    .map_err(|_| EngineError::Other(format!("{base_ref} does not exist in this repository")))?;
    let merged = capture_git(root, &["merge-base", &base, &head], 256)
        .await
        .map(|c| String::from_utf8_lossy(&c.stdout).trim().to_string())
        .unwrap_or_default();
    if merged.is_empty() {
        return Err(EngineError::Other(format!(
            "no merge base between {base_ref} and {head_ref}"
        )));
    }
    Ok(RangeShas {
        merge_base: merged,
        head,
    })
}

/// Resolve a diff range and read one immutable source file at either revision.
pub async fn resolve_code_ref_source(root: &Path, base_ref: &str, head_ref: &str, path: &str, head: bool) -> Result<(String, String), EngineError> {
    let path = validate_diff_path(path)?;
    let range = resolve_range(root, base_ref, head_ref).await?;
    let revision = if head { range.head } else { range.merge_base };
    let source = read_git_source(root, &revision, path).await?;
    if source.truncated { return Err(EngineError::Other("code reference source exceeds size limit".into())); }
    let text = String::from_utf8(source.stdout).map_err(|_| EngineError::Other("code reference source is not UTF-8".into()))?;
    Ok((revision, text))
}

/// Read one source file at an exact commit of a nested (submodule) repository.
/// `repo_root` is checkout-relative and must be a real directory inside the
/// checkout; `path` is relative to the nested repository.
pub async fn resolve_nested_code_ref_source(root: &Path, repo_root: &str, revision: &str, path: &str) -> Result<(String, String), EngineError> {
    if !valid_object_id(revision) {
        return Err(EngineError::Other("invalid code reference revision".into()));
    }
    let path = validate_diff_path(path)?;
    let nested = resolve_submodule_root(root, repo_root).await?;
    let source = read_git_source(&nested, revision, path).await?;
    if source.truncated { return Err(EngineError::Other("code reference source exceeds size limit".into())); }
    let text = String::from_utf8(source.stdout).map_err(|_| EngineError::Other("code reference source is not UTF-8".into()))?;
    Ok((revision.to_ascii_lowercase(), text))
}

/// Resolve a branch against the local remote-default ref, without fetching.
pub async fn resolve_branch_review_base(
    root: &Path,
    branch: &str,
    device_id: &str,
) -> Result<mast_proto::ResolveBranchReviewBaseReply, EngineError> {
    if branch.is_empty() || branch.starts_with('-') {
        return Err(EngineError::Other("branch name is required".into()));
    }
    let base_ref = match capture_git(root, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"], 1024).await {
        Ok(result) => {
            let remote = String::from_utf8_lossy(&result.stdout).trim().to_owned();
            let local = remote.strip_prefix("refs/remotes/origin/").map(|name| format!("refs/heads/{name}"));
            match local {
                Some(local) if capture_git(root, &["rev-parse", "--verify", &local], 256).await.is_ok() => local,
                _ => remote,
            }
        },
        Err(_) => {
            let mut found = None;
            for candidate in ["refs/heads/main", "refs/heads/master"] {
                if capture_git(root, &["rev-parse", "--verify", candidate], 256).await.is_ok() {
                    found = Some(candidate.to_owned());
                    break;
                }
            }
            found.ok_or_else(|| EngineError::Other("No default branch is configured".into()))?
        }
    };
    let head_ref = format!("refs/heads/{branch}");
    let range = resolve_range(root, &base_ref, &head_ref).await?;
    let range_spec = format!("{}..{}", range.merge_base, range.head);
    let count = capture_git(root, &["rev-list", "--count", &range_spec], 256).await?;
    let ahead_count = String::from_utf8_lossy(&count.stdout).trim().parse()
        .map_err(|_| EngineError::Other("invalid commit count".into()))?;
    let common = capture_git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"], 4096).await?;
    let common_path = std::path::PathBuf::from(String::from_utf8_lossy(&common.stdout).trim());
    let common_path = std::fs::canonicalize(&common_path).unwrap_or(common_path);
    let repository = crate::execution_scope::repository_id(device_id, &common_path);
    let review_range_id = format!("review-{}", mast_proto::derive_id(&format!("{repository}\0{branch}"), "review-range"));
    Ok(mast_proto::ResolveBranchReviewBaseReply {
        base_ref,
        base_sha: range.merge_base,
        head_sha: range.head,
        ahead_count,
        review_range_id,
        existing_chat_id: None,
    })
}

/// "Diff branch / PR…" capture: `merge-base(base_ref, head_ref)` against
/// `head_ref`, read from refs only (no working tree, no untracked files).
pub async fn capture_range_diff(
    repos: &Repos,
    root: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<(DiffSnapshot, RangeShas), EngineError> {
    let range = resolve_range(root, base_ref, head_ref).await?;
    let snapshot = capture_revision_pair_diff(repos, root, &range.merge_base, &range.head).await?;
    Ok((snapshot, range))
}

/// Snapshot of the changes from commit-ish `base` to commit-ish `sha` (shared
/// by the per-commit and range captures).
pub(crate) async fn capture_revision_pair_diff(
    repos: &Repos,
    root: &Path,
    base: &str,
    sha: &str,
) -> Result<DiffSnapshot, EngineError> {
    let base = base.to_owned();
    let branch = repos
        .current_branch(root)
        .await
        .unwrap_or_else(|_| "HEAD".into());
    let names = capture_git(
        root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            &base,
            sha,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let nums = capture_git(
        root,
        &[
            "diff",
            "--numstat",
            "-z",
            "--find-renames",
            &base,
            sha,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let raw = capture_git(
        root,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-abbrev",
            "--find-renames",
            &base,
            sha,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let tracked = capture_git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            "--unified=3",
            &base,
            sha,
            "--",
        ],
        MAX_PATCH_BYTES,
    )
    .await?;
    let mut files = parse_name_status(&names.stdout);
    apply_numstat(&mut files, &nums.stdout);
    let mut patch = String::from_utf8_lossy(&tracked.stdout).to_string();
    let mut truncated = tracked.truncated || names.truncated || nums.truncated || raw.truncated;
    if tracked.truncated {
        let boundary = patch.rfind('\n').unwrap_or(0);
        patch.truncate(boundary);
        patch.push_str("\n# Comet diff truncated\n");
    }
    let (submodules, submodules_truncated) = Box::pin(capture_submodule_sections(
        root,
        &raw,
        false,
        Some(sha),
        MAX_PATCH_BYTES.saturating_sub(patch.len()),
    ))
    .await?;
    truncated |= submodules_truncated;
    let additions: u32 = files.iter().map(|f| f.additions).sum();
    let deletions: u32 = files.iter().map(|f| f.deletions).sum();
    let files_json = serde_json::to_string(&files)
        .map_err(|e| EngineError::Other(format!("diff files serialize: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(branch.as_bytes());
    hasher.update([0u8]);
    hasher.update(sha.as_bytes());
    hasher.update([0u8]);
    hasher.update(patch.as_bytes());
    hasher.update([0u8]);
    hasher.update(files_json.as_bytes());
    hasher.update([0u8]);
    let submodules_json = serde_json::to_string(&submodules)
        .map_err(|e| EngineError::Other(format!("submodule diffs serialize: {e}")))?;
    hasher.update(submodules_json.as_bytes());
    hasher.update(if truncated { b"1" } else { b"0" });
    let checksum = crate::repos::hex(&hasher.finalize());
    Ok(DiffSnapshot {
        branch,
        head_sha: Some(sha.to_string()),
        git_status: None,
        patch,
        files,
        submodules,
        additions,
        deletions,
        truncated,
        checksum,
    })
}

/// `git merge-base <base_ref> HEAD` — the diff base for "Branch changes".
/// Errors when the ref is unknown or the histories are unrelated.
pub async fn merge_base(root: &Path, base_ref: &str) -> Result<String, EngineError> {
    let capture = capture_git(root, &["merge-base", base_ref, "HEAD"], 256).await?;
    let sha = String::from_utf8_lossy(&capture.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(EngineError::Other(format!("no merge base with {base_ref}")));
    }
    Ok(sha)
}

/// Write the checkout's current tracked + untracked (unignored) tree into the
/// object db via a throwaway index: `git add -A` under `GIT_INDEX_FILE`, then
/// `git write-tree`. The real index is never touched. Costs one full hash pass
/// over the working tree (no stat cache in a fresh index) — run once per turn
/// dispatch, that is the same cost class as the untracked-file reads the watch
/// capture already does.
pub async fn snapshot_tree(root: &Path) -> Result<String, EngineError> {
    let index = std::env::temp_dir().join(format!(
        "zeron-turn-index-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_micros()
    ));
    let run = |args: &[&str]| {
        let mut cmd = mast_harness::git_command_async();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(0x08000000);
        }
        cmd.arg("-C").arg(root).args(args);
        cmd.env("GIT_INDEX_FILE", &index);
        cmd.stdin(std::process::Stdio::null());
        async move {
            let _permit = crate::git_gate::acquire("diff_sync").await;
            cmd.output().await
        }
    };
    let added = run(&["add", "-A", "--ignore-errors", "."])
        .await
        .map_err(|e| EngineError::Other(format!("git add failed: {e}")))?;
    if !added.status.success() {
        let _ = tokio::fs::remove_file(&index).await;
        return Err(EngineError::Other(format!(
            "git add: {}",
            String::from_utf8_lossy(&added.stderr).trim()
        )));
    }
    let written = run(&["write-tree"])
        .await
        .map_err(|e| EngineError::Other(format!("git write-tree failed: {e}")));
    let _ = tokio::fs::remove_file(&index).await;
    let written = written?;
    if !written.status.success() {
        return Err(EngineError::Other(format!(
            "git write-tree: {}",
            String::from_utf8_lossy(&written.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&written.stdout).trim().to_string())
}

/// "Latest turn" capture: tree-to-tree diff from the turn-start snapshot to a
/// fresh [`snapshot_tree`] of the current state. Both trees carry untracked
/// (unignored) files, so no synthesis is needed and a file that was already
/// untracked at turn start diffs correctly (the watch capture's synthesis
/// would misreport it as entirely new).
pub async fn capture_turn_diff(
    repos: &Repos,
    root: &Path,
    turn_tree: &str,
) -> Result<DiffSnapshot, EngineError> {
    let current = snapshot_tree(root).await?;
    let head = capture_git(root, &["rev-parse", "--verify", "HEAD"], 256)
        .await
        .map(|c| String::from_utf8_lossy(&c.stdout).trim().to_string())
        .unwrap_or_default();
    let branch = repos
        .current_branch(root)
        .await
        .unwrap_or_else(|_| "HEAD".into());

    let names = capture_git(
        root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            turn_tree,
            &current,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let nums = capture_git(
        root,
        &[
            "diff",
            "--numstat",
            "-z",
            "--find-renames",
            turn_tree,
            &current,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let raw = capture_git(
        root,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-abbrev",
            "--find-renames",
            turn_tree,
            &current,
            "--",
        ],
        2 * 1024 * 1024,
    )
    .await?;
    let tracked = capture_git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            "--unified=3",
            turn_tree,
            &current,
            "--",
        ],
        MAX_PATCH_BYTES,
    )
    .await?;

    let mut files = parse_name_status(&names.stdout);
    apply_numstat(&mut files, &nums.stdout);
    let mut patch = String::from_utf8_lossy(&tracked.stdout).to_string();
    let mut truncated = tracked.truncated || names.truncated || nums.truncated || raw.truncated;
    if tracked.truncated {
        let boundary = patch.rfind('\n').unwrap_or(0);
        patch.truncate(boundary);
        patch.push_str("\n# Mast diff truncated\n");
    }

    let (submodules, submodules_truncated) = Box::pin(capture_submodule_sections(
        root,
        &raw,
        false,
        Some(&current),
        MAX_PATCH_BYTES.saturating_sub(patch.len()),
    ))
    .await?;
    truncated |= submodules_truncated;
    let additions: u32 = files.iter().map(|f| f.additions).sum();
    let deletions: u32 = files.iter().map(|f| f.deletions).sum();
    let files_json = serde_json::to_string(&files)
        .map_err(|e| EngineError::Other(format!("diff files serialize: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(branch.as_bytes());
    hasher.update([0u8]);
    hasher.update(head.as_bytes());
    hasher.update([0u8]);
    hasher.update(patch.as_bytes());
    hasher.update([0u8]);
    hasher.update(files_json.as_bytes());
    hasher.update([0u8]);
    let submodules_json = serde_json::to_string(&submodules)
        .map_err(|e| EngineError::Other(format!("submodule diffs serialize: {e}")))?;
    hasher.update(submodules_json.as_bytes());
    hasher.update(if truncated { b"1" } else { b"0" });
    let checksum = crate::repos::hex(&hasher.finalize());

    Ok(DiffSnapshot {
        branch,
        head_sha: (!head.is_empty()).then_some(head),
        git_status: None,
        patch,
        files,
        submodules,
        additions,
        deletions,
        truncated,
        checksum,
    })
}

#[cfg(test)]
mod watch_budget_tests {
    use super::{
        Capture, CheckoutIdentity, MAX_EXPANDED_SUBMODULES, MAX_SUBMODULE_METADATA_BYTES,
        MAX_CONCURRENT_WATCH_SETUPS, MAX_WATCH_DIRS, SubmoduleBudget, exceeds_watch_budget,
        forget_watch_budget, kick_relevant, parse_raw_diff, run_gated_blocking,
        walk_exceeds_watch_budget, watch_targets,
    };
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[tokio::test]
    async fn watch_setup_priority_changes_and_fifo_prevents_starvation() {
        use std::sync::atomic::AtomicI64;
        use super::WatchSetupQueue;
        let queue = WatchSetupQueue::new();
        let priority = |value| Arc::new(AtomicI64::new(value));
        let first = queue.acquire(priority(0)).await.unwrap();
        let second = queue.acquire(priority(0)).await.unwrap();
        let mut oldest = queue.enqueue(priority(1));
        let changed = priority(0);
        let mut recent = queue.enqueue(changed.clone());
        let mut next = queue.enqueue(priority(2));
        changed.store(10, Ordering::Relaxed);

        drop(first);
        let recent_permit = recent.try_recv().expect("updated priority goes first");
        assert!(oldest.try_recv().is_err());
        drop(second);
        let oldest_permit = oldest.try_recv().expect("oldest waiter gets the next slot");
        // Even new, higher-priority arrivals cannot prevent FIFO progress.
        let mut later = queue.enqueue(priority(20));
        drop(recent_permit);
        let later_permit = later.try_recv().expect("priority turn");
        drop(oldest_permit);
        let next_permit = next.try_recv().expect("remaining waiter is not starved");
        drop((later_permit, next_permit));
    }

    #[tokio::test]
    async fn branch_review_base_counts_commits_without_fetching() {
        let directory = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = mast_harness::git_command()
                .current_dir(directory.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        git(&["commit", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-b", "feature"]);
        git(&["commit", "--allow-empty", "-m", "change"]);
        let head = git(&["rev-parse", "HEAD"]);
        let result = super::resolve_branch_review_base(directory.path(), "feature", "device").await.unwrap();
        assert_eq!(result.base_ref, "refs/heads/main");
        assert_eq!(result.base_sha, base);
        assert_eq!(result.head_sha, head);
        assert_eq!(result.ahead_count, 1);
        assert_eq!(super::resolve_branch_review_base(directory.path(), "main", "device").await.unwrap().ahead_count, 0);
    }

    #[tokio::test]
    async fn code_reference_base_reads_the_merge_base_file() {
        let directory = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = mast_harness::git_command().current_dir(directory.path()).args(args).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(directory.path().join("file.rs"), "before\n").unwrap();
        git(&["add", "file.rs"]);
        git(&["commit", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-b", "feature"]);
        git(&["rm", "file.rs"]);
        git(&["commit", "-m", "delete"]);
        let (sha, text) = super::resolve_code_ref_source(directory.path(), "main", "HEAD", "file.rs", false).await.unwrap();
        assert_eq!(sha, base);
        assert_eq!(text, "before\n");
        assert!(super::resolve_code_ref_source(directory.path(), "main", "HEAD", "../file.rs", false).await.is_err());
    }

    #[tokio::test]
    async fn nested_code_reference_reads_the_submodule_revision() {
        let directory = tempfile::tempdir().unwrap();
        let sub = directory.path().join("sub-origin");
        let parent = directory.path().join("parent");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let output = mast_harness::git_command().current_dir(dir).args(args).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        for dir in [&sub, &parent] {
            git(dir, &["init", "-b", "main"]);
            git(dir, &["config", "user.email", "test@example.invalid"]);
            git(dir, &["config", "user.name", "Test"]);
        }
        std::fs::write(sub.join("lib.rs"), "one\ntwo\n").unwrap();
        git(&sub, &["add", "."]);
        git(&sub, &["commit", "-m", "sub"]);
        let first = git(&sub, &["rev-parse", "HEAD"]);
        // `git submodule add` stalls for minutes on some hosts; a local clone plus a
        // gitlink and `.gitmodules` entry produces the same repository layout.
        git(&parent, &["clone", "-q", sub.to_str().unwrap(), "vendor/ui"]);
        git(&parent, &["config", "-f", ".gitmodules", "submodule.vendor/ui.path", "vendor/ui"]);
        git(&parent, &["config", "-f", ".gitmodules", "submodule.vendor/ui.url", sub.to_str().unwrap()]);
        git(&parent, &["add", ".gitmodules", "vendor/ui"]);
        git(&parent, &["commit", "-m", "add submodule"]);
        let nested = parent.join("vendor/ui");
        git(&nested, &["config", "user.email", "test@example.invalid"]);
        git(&nested, &["config", "user.name", "Test"]);
        std::fs::write(nested.join("lib.rs"), "zero\none\ntwo\n").unwrap();
        git(&nested, &["commit", "-am", "edit"]);
        let second = git(&nested, &["rev-parse", "HEAD"]);
        let (sha, text) = super::resolve_nested_code_ref_source(&parent, "vendor/ui", &first, "lib.rs").await.unwrap();
        assert_eq!((sha.as_str(), text.as_str()), (first.as_str(), "one\ntwo\n"));
        let (sha, text) = super::resolve_nested_code_ref_source(&parent, "vendor/ui", &second, "lib.rs").await.unwrap();
        assert_eq!((sha.as_str(), text.as_str()), (second.as_str(), "zero\none\ntwo\n"));
        assert!(super::resolve_nested_code_ref_source(&parent, "vendor/ui", "HEAD", "lib.rs").await.is_err());
        assert!(super::resolve_nested_code_ref_source(&parent, "../x", &first, "lib.rs").await.is_err());
        assert!(super::resolve_nested_code_ref_source(&parent, "vendor/ui", &first, "../lib.rs").await.is_err());
    }

    #[test]
    fn fixed_range_head_resolves_without_a_ref_file() {
        let sha = "a".repeat(40);
        assert_eq!(super::read_ref_sha(std::path::Path::new("/missing"), &sha), Some(sha));
        assert_eq!(super::read_ref_sha(std::path::Path::new("/missing"), "../head"), None);
    }

    #[test]
    fn submodule_metadata_budget_is_shared_and_hard_capped() {
        assert!(MAX_EXPANDED_SUBMODULES <= 8);
        let mut budget = SubmoduleBudget {
            remaining_patch: 100,
            remaining_metadata: MAX_SUBMODULE_METADATA_BYTES,
            sections: 0,
            truncated: false,
        };
        budget.consume_metadata(&Capture {
            stdout: vec![0; MAX_SUBMODULE_METADATA_BYTES - 4],
            truncated: false,
        });
        assert_eq!(budget.metadata_cap(), 4);
        budget.consume_metadata(&Capture {
            stdout: vec![0; 4],
            truncated: true,
        });
        assert_eq!(budget.metadata_cap(), 0);
        assert!(budget.truncated);
    }

    #[test]
    fn raw_gitlink_records_keep_modes_oids_and_rename_paths() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let modified = format!(":160000 160000 {old} {new} M\0deps/lib\0");
        let parsed = parse_raw_diff(modified.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].old_mode, 0o160000);
        assert_eq!(parsed[0].new_mode, 0o160000);
        assert_eq!(parsed[0].path, "deps/lib");
        assert_eq!(parsed[0].old_path, None);

        let renamed = format!(":160000 160000 {old} {new} R100\0old/lib\0new/lib\0");
        let parsed = parse_raw_diff(renamed.as_bytes()).unwrap();
        assert_eq!(parsed[0].old_path.as_deref(), Some("old/lib"));
        assert_eq!(parsed[0].path, "new/lib");
    }

    #[test]
    fn malformed_raw_diff_is_rejected() {
        assert!(parse_raw_diff(b":160000 160000 bad bad M\0deps/lib\0").is_err());
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let missing_path = format!(":160000 160000 {old} {new} M\0");
        assert!(parse_raw_diff(missing_path.as_bytes()).is_err());
    }

    #[test]
    fn small_tree_is_watchable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/a/b")).unwrap();
        std::fs::create_dir_all(root.join("src/c")).unwrap();
        std::fs::write(root.join("src/a/f.txt"), "x").unwrap();
        assert!(!exceeds_watch_budget(root));
    }

    #[test]
    fn budget_is_exceeded_and_probe_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // One flat directory of MAX_WATCH_DIRS + 50 subdirs trips the budget;
        // the BFS must stop right after the threshold, not enumerate the rest.
        for i in 0..(MAX_WATCH_DIRS + 50) {
            std::fs::create_dir(root.join(format!("d{i}"))).unwrap();
        }
        assert!(exceeds_watch_budget(root));
    }

    fn identity(root: &std::path::Path, git_dir: &std::path::Path) -> CheckoutIdentity {
        CheckoutIdentity {
            id: "test".into(),
            root: root.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
        }
    }

    #[test]
    fn foreign_git_churn_does_not_kick_a_capture() {
        let git_dir = Path::new("/repo/.git");
        let kicks = |paths: &[&str]| {
            kick_relevant(
                &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
                git_dir,
            )
        };
        assert!(kicks(&[]), "rescans count");
        assert!(kicks(&["/repo/src/main.rs"]), "working-tree edits count");
        assert!(kicks(&["/repo/.git/HEAD"]));
        assert!(kicks(&["/repo/.git/index"]));
        assert!(kicks(&["/repo/.git/refs/heads/main"]));
        assert!(kicks(&["/repo/.git/info/exclude"]), "unknown git files count");
        assert!(kicks(&["/repo/.git/modules/ui/HEAD"]), "submodule HEAD moves the gitlink");
        assert!(kicks(&["/repo/.git/modules/ui/refs/heads/main"]));
        for module in ["components/ui", "outer/modules/inner"] {
            for reference in ["HEAD", "refs/heads/main", "packed-refs"] {
                assert!(kicks(&[&format!(
                    "/repo/.git/modules/{module}/{reference}"
                )]));
            }
            for noise in ["index", "objects/ab/cdef", "refs/heads/main.lock"] {
                assert!(!kicks(&[&format!("/repo/.git/modules/{module}/{noise}")]));
            }
        }
        assert!(!kicks(&["/repo/.git/modules/ui/index"]));
        assert!(!kicks(&["/repo/.git/modules/flagship/worktrees/wt/index"]));
        assert!(!kicks(&["/repo/.git/worktrees/wt/HEAD"]));
        assert!(!kicks(&["/repo/.git/objects/ab/cdef"]));
        assert!(!kicks(&["/repo/.git/logs/HEAD"]));
        assert!(!kicks(&["/repo/.git/index.lock"]));
        assert!(!kicks(&["/repo/.git/FETCH_HEAD"]));
        assert!(
            kicks(&["/repo/.git/objects/ab/cdef", "/repo/.git/HEAD"]),
            "any relevant path in a batch counts"
        );
    }

    #[test]
    fn small_checkout_watches_root_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git/refs")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // The root watch covers the inline .git — no second watcher.
        assert_eq!(
            watch_targets(&identity(root, &root.join(".git"))),
            vec![root.to_path_buf()]
        );
    }

    #[test]
    fn linked_worktree_watches_root_and_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wt");
        let git_dir = tmp.path().join("main/.git/worktrees/wt");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        assert_eq!(
            watch_targets(&identity(&root, &git_dir)),
            vec![root.clone(), git_dir]
        );
    }

    #[test]
    fn over_budget_root_falls_back_to_the_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git/refs")).unwrap();
        // Blow the budget with a flat dependency-tree stand-in.
        for i in 0..(MAX_WATCH_DIRS + 50) {
            std::fs::create_dir(root.join(format!("d{i}"))).unwrap();
        }
        // Commits/index churn must still watch live even though edits can't.
        assert_eq!(
            watch_targets(&identity(root, &root.join(".git"))),
            vec![root.join(".git")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dir_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("real/inner")).unwrap();
        // A self-referential symlink cycle must not send the walk into a spin.
        std::os::unix::fs::symlink(root.join("real"), root.join("real/inner/loop")).unwrap();
        assert!(!exceeds_watch_budget(root)); // terminates, under budget
    }

    #[test]
    fn top_level_heavy_dir_is_over_budget_without_walking_into_it() {
        for name in ["node_modules", "target", ".venv", "dist", "build", ".next", "vendor"] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            let heavy = root.join(name);
            std::fs::create_dir_all(&heavy).unwrap();
            // Unreadable: a walk that descended would treat it as a leaf, so
            // the verdict can only come from the name.
            assert!(exceeds_watch_budget(root), "{name}");
        }
        // Nested a few levels down also trips it.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("packages/app/node_modules")).unwrap();
        assert!(exceeds_watch_budget(tmp.path()));
    }

    #[test]
    fn heavy_dir_contents_are_never_enumerated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Far more dirs than the budget inside node_modules; the heavy name
        // verdict must come first, so under the budget the walk is trivial.
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        assert!(walk_exceeds_watch_budget(root));
        let small = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(small.path().join("src")).unwrap();
        assert!(!walk_exceeds_watch_budget(small.path()));
    }

    #[test]
    fn repeated_root_uses_the_cached_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        assert!(!exceeds_watch_budget(root));
        // Tree changes underneath; the cached verdict stands (no second walk).
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        assert!(!exceeds_watch_budget(root));
        // Tearing the entry down forgets it.
        forget_watch_budget(root);
        assert!(exceeds_watch_budget(root));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn watch_setups_are_bounded_to_the_gate() {
        use std::sync::atomic::AtomicUsize;
        let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_WATCH_SETUPS));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..20 {
            let (running, peak, done) = (running.clone(), peak.clone(), done.clone());
            tasks.push(tokio::spawn(run_gated_blocking(gate.clone(), move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(15));
                running.fetch_sub(1, Ordering::SeqCst);
                done.fetch_add(1, Ordering::SeqCst);
            })));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(done.load(Ordering::SeqCst), 20);
        assert!(peak.load(Ordering::SeqCst) <= MAX_CONCURRENT_WATCH_SETUPS);
        assert!(peak.load(Ordering::SeqCst) >= 1);
    }
}

#[cfg(test)]
mod repository_ref_budget_tests {
    use super::*;
    use crate::{EngineCore, HarnessRegistry};

    static REF_BUDGET_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn git(path: &Path, args: &[&str]) -> String {
        let output = mast_harness::git_command()
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args).current_dir(path).env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_repository_ref_read_budget() {
        let _guard = lock(&REF_BUDGET_TEST_LOCK);
        let initial_reads = crate::provenance::REF_TIP_READS.load(Ordering::SeqCst);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("file"), "content").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let mut paths = Vec::new();
        for index in 0..20 {
            let path = root.join(format!("checkout-{index}"));
            git(&repo, &["worktree", "add", "-q", "-b", &format!("branch-{index}"), path.to_str().unwrap()]);
            paths.push(path);
        }
        git(&repo, &["pack-refs", "--all"]);
        let refs = repo.join(".git/refs/heads");
        std::fs::create_dir_all(&refs).unwrap();
        for index in 0..300 {
            std::fs::write(refs.join(format!("extra-{index}")), format!("{head}\n")).unwrap();
        }
        let data = root.join("engine");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("device-id"), "test-device").unwrap();
        let core = EngineCore::assemble(&data, Arc::new(HarnessRegistry::new()), mast_proto::HarnessId::Mock, None).unwrap();
        for (index, path) in paths.iter().enumerate() {
            let chat = format!("chat-{index}");
            core.workspace.create_chat(&chat, None, Some("test-device"), None, Some(repo.to_string_lossy().into_owned())).unwrap();
            core.diff_targets.bind(&chat, &path.to_string_lossy(), ChatDiffTargetSource::Agent, None).unwrap();
        }
        core.diff_sync.set_head_move_listener(Arc::new(|_| {}));
        core.diff_sync.reconcile_now().await;
        let inner = &core.diff_sync.inner;
        let entries: Vec<_> = lock(&inner.entries).values().filter(|entry| paths.contains(&entry.identity.root)).cloned().collect();
        assert_eq!(entries.len(), 20);
        for capture in 0..10 {
            if capture == 5 { git(&repo, &["branch", "additional", "HEAD"]); }
            for entry in &entries {
                std::fs::write(entry.identity.root.join("file"), format!("edit {capture}\n")).unwrap();
                sync_entry(inner, entry).await;
            }
        }
        let common = common_dir_of(&repo.join(".git"));
        let reads = crate::provenance::REF_TIP_READS.load(Ordering::SeqCst) - initial_reads;
        let parses = lock(&inner.packed_refs).get(&common).unwrap().parses;
        assert!(reads <= 2, "tip reads: {reads}");
        assert!(parses <= 2, "packed-refs parses: {parses}");
        assert_eq!(reads, 2);
        assert_eq!(parses, 1);
        #[cfg(unix)]
        {
            let packed = common.join("packed-refs");
            let metadata = std::fs::metadata(&packed).unwrap();
            let stamp = crate::provenance::refs_stamp(&common);
            assert_eq!(read_head(inner, &entries[0].identity).1.as_deref(), Some(head.as_str()));
            let replacement = common.join("packed-refs.lock");
            let changed = std::fs::read_to_string(&packed).unwrap().replace(&head, &"0".repeat(head.len()));
            std::fs::write(&replacement, changed).unwrap();
            std::fs::File::options().write(true).open(&replacement).unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap())).unwrap();
            std::fs::rename(&replacement, &packed).unwrap();
            let updated = std::fs::metadata(&packed).unwrap();
            assert_eq!(metadata.len(), updated.len());
            assert_eq!(metadata.modified().unwrap(), updated.modified().unwrap());
            assert_ne!(crate::provenance::metadata_inode(&metadata), crate::provenance::metadata_inode(&updated));
            assert_ne!(stamp, crate::provenance::refs_stamp(&common));
            assert_eq!(read_head(inner, &entries[0].identity).1, Some("0".repeat(head.len())));
            assert_eq!(lock(&inner.packed_refs).get(&common).unwrap().parses, 2);
        }
        core.diff_sync.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prior_tips_remain_specific_to_each_checkout() {
        let _guard = lock(&REF_BUDGET_TEST_LOCK);
        let _pause = CheckoutDiffSync::pause_watch_setups_for_test().await;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("file"), "initial\n").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let paths: Vec<_> = ["a", "b"].iter().map(|name| {
            let path = root.join(name);
            git(&repo, &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()]);
            path
        }).collect();
        let data = root.join("engine");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("device-id"), "test-device").unwrap();
        let core = EngineCore::assemble(&data, Arc::new(HarnessRegistry::new()), mast_proto::HarnessId::Mock, None).unwrap();
        for (index, path) in paths.iter().enumerate() {
            let chat = format!("chat-{index}");
            core.workspace.create_chat(&chat, None, Some("test-device"), None, Some(repo.to_string_lossy().into_owned())).unwrap();
            core.diff_targets.bind(&chat, &path.to_string_lossy(), ChatDiffTargetSource::Agent, None).unwrap();
        }
        let moves = Arc::new(Mutex::new(Vec::new()));
        let recorded = moves.clone();
        core.diff_sync.set_head_move_listener(Arc::new(move |event| lock(&recorded).push(event)));
        core.diff_sync.reconcile_now().await;
        let inner = &core.diff_sync.inner;
        let entries: Vec<_> = paths.iter().map(|path| {
            lock(&inner.entries).values().find(|entry| entry.identity.root == *path).unwrap().clone()
        }).collect();
        // The initial background capture must finish before this test manually
        // captures tips; otherwise it can overwrite B's prior snapshot midway.
        let mut completed = core.diff_sync.watch_completion_for_test();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if entries.iter().all(|entry| {
                    let sent = entry.kick_tx.sent.load(Ordering::SeqCst);
                    sent > 0 && entry.initial_kicked.load(Ordering::SeqCst)
                        && entry.handled_kicks.load(Ordering::SeqCst) >= sent
                }) { break; }
                completed.changed().await.expect("diff sync completion channel");
            }
        }).await.expect("initial checkout captures finish");
        for entry in &entries { sync_entry(inner, entry).await; }
        std::fs::write(repo.join("file"), "commit\n").unwrap();
        git(&repo, &["commit", "-qam", "new commit"]);
        let tip = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["branch", "additional", &tip]);
        std::fs::write(paths[0].join("file"), "capture a\n").unwrap();
        sync_entry(inner, &entries[0]).await;
        assert!(lock(&entries[0].last_tips).as_ref().unwrap().contains(&tip));
        assert!(!lock(&entries[1].last_tips).as_ref().unwrap().contains(&tip));
        git(&paths[1], &["reset", "--hard", &tip]);
        sync_entry(inner, &entries[1]).await;
        let events = lock(&moves);
        let event = events.iter().find(|event| event.root == paths[1] && event.new == tip).expect("B HEAD move");
        assert!(!event.prior_tips.contains(&tip), "B must retain its previous capture's tips");
        drop(events);
        core.diff_sync.shutdown().await;
    }

}
