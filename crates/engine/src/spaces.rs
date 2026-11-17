//! SpacesSync — owner-side upkeep of space rows (git presence) plus the
//! orphan-chat repair sweep and the worktree-Space fold.
//!
//! A space is a synced (device, folder) pair; the folder need NOT be a git
//! repo. This service watches the workspace `spaces` rows owned by THIS device
//! and keeps their `gitDetected`/`checkoutId` stamps truthful:
//!
//! - recheck on boot / when a space row is first observed;
//! - a non-recursive `notify` watcher on the space folder — `.git` appearing or
//!   vanishing (git init / de-git) kicks a recheck;
//! - a slow 2-minute repair tick (native watchers coalesce/drop events).
//!
//! Stamps are written ONLY on change, so steady state never grows the oplog.
//! Remote devices read `space.git_detected` straight from the doc — branch
//! pickers and the diff sidebar gate on it with zero RPCs.
//!
//! The repair tick also runs the orphan sweep: a chat created concurrently
//! with a `deleteSpace` on another device can sync in after the cascade ran,
//! leaving a dangling `spaceId`. The HOST device deletes its own such chats
//! (writer discipline — we never touch other devices' rows). The sweep only
//! runs once the registry has applied a server state this boot
//! ([`WorkspaceHost::registry_synced`]): on an un-synced replica a missing
//! space row means "we haven't heard yet", not "deleted".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use mast_proto::Space;

use crate::repos::Repos;
use crate::workspace_host::WorkspaceHost;

/// Trailing debounce after a filesystem event burst.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
/// Slow repair pass: recheck every owned space + orphan sweep.
const REPAIR_INTERVAL: Duration = Duration::from_secs(120);

struct SpaceEntry {
    path: PathBuf,
    kick_tx: mpsc::UnboundedSender<()>,
    /// Keeps the folder watcher alive; dropped on entry close. Filled
    /// asynchronously — FSEvents registration blocks, so [`reconcile`] builds
    /// it off the runtime and attaches it here once ready.
    folder_watch: Mutex<Option<crate::watch_budget::Watcher>>,
}

struct SpacesSyncInner {
    repos: Repos,
    workspace: WorkspaceHost,
    device_id: String,
    entries: Mutex<HashMap<String, Arc<SpaceEntry>>>,
    /// Ends the supervisor loop eagerly on shutdown (weak refs alone only end
    /// it once the whole graph drops).
    cancel: CancellationToken,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Spaces another service still launches into (scheduled tasks); the
    /// worktree fold never deletes them. Unset or failing = no fold.
    space_holds: Mutex<Option<SpaceHolds>>,
}

/// Space ids that must not be folded away, or `Err` when unknown.
pub type SpaceHolds = Arc<dyn Fn() -> Result<HashSet<String>, crate::EngineError> + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct SpacesSync {
    inner: Arc<SpacesSyncInner>,
}

impl SpacesSync {
    /// Build and start the sync loop: follows the workspace spaces watch and
    /// runs the repair tick. Requires a tokio runtime.
    pub fn start(repos: Repos, workspace: WorkspaceHost, device_id: &str) -> Self {
        let sync = Self {
            inner: Arc::new(SpacesSyncInner {
                repos,
                workspace: workspace.clone(),
                device_id: device_id.to_string(),
                entries: Mutex::new(HashMap::new()),
                cancel: CancellationToken::new(),
                supervisor: Mutex::new(None),
                space_holds: Mutex::new(None),
            }),
        };
        let task = tokio::spawn(spaces_task(
            Arc::downgrade(&sync.inner),
            workspace.watch_spaces(),
            sync.inner.cancel.clone(),
        ));
        *lock(&sync.inner.supervisor) = Some(task);
        sync
    }

    /// Register the Spaces the worktree fold must keep; until this is set the
    /// fold doesn't run.
    pub fn set_space_holds(&self, holds: SpaceHolds) {
        *lock(&self.inner.space_holds) = Some(holds);
    }

    /// Stop the supervisor loop and wait for it to exit (per-space tasks are
    /// purely local and end when their entries drop). Idempotent.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let task = lock(&self.inner.supervisor).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// Reconcile + recheck now (tests / opportunistic callers).
    pub async fn reconcile_now(&self) {
        let spaces = self.inner.workspace.watch_spaces().borrow().clone();
        reconcile(&self.inner, &spaces);
        for entry in lock(&self.inner.entries).values() {
            let _ = entry.kick_tx.send(());
        }
    }
}

/// (Re)build the entry set for the spaces THIS device owns.
fn reconcile(inner: &Arc<SpacesSyncInner>, spaces: &[Space]) {
    let owned: HashMap<&str, &Space> = spaces
        .iter()
        .filter(|s| s.device_id == inner.device_id)
        .map(|s| (s.id.as_str(), s))
        .collect();

    let mut entries = lock(&inner.entries);
    entries.retain(|id, _| owned.contains_key(id.as_str()));
    for (id, space) in owned {
        if entries.contains_key(id) {
            continue; // deviceId/path are immutable — nothing to refresh
        }
        let (kick_tx, kick_rx) = mpsc::unbounded_channel();
        let entry = Arc::new(SpaceEntry {
            path: PathBuf::from(&space.path),
            kick_tx: kick_tx.clone(),
            folder_watch: Mutex::new(None),
        });
        entries.insert(id.to_string(), entry.clone());
        tokio::spawn(entry_task(
            Arc::downgrade(inner),
            id.to_string(),
            Arc::downgrade(&entry),
            kick_rx,
        ));
        let _ = kick_tx.send(()); // initial check (boot / first observed)

        // Non-recursive watcher on the space folder: `.git` appearing/vanishing
        // among the direct children is exactly the signal we need. Watch
        // failures are fine — the repair tick still converges. Built off the
        // runtime: FSEvents registration blocks, and reconcile runs on the
        // spaces-watch task.
        //
        // A saved home/root or privacy-managed space deliberately skips this
        // watcher. FSEvents is subtree-oriented on macOS and registering it
        // against HOME, Music, Pictures, Documents, etc. can trip a privacy
        // prompt even with `NonRecursive`. `check_space` is guarded too, so
        // its initial/repair kicks only clear stale Git stamps without touching
        // the filesystem.
        if crate::repos::is_automatic_access_blocked(&entry.path) {
            tracing::info!(
                path = %entry.path.display(),
                "spaces: automatic filesystem checks disabled for privacy-managed path"
            );
            continue;
        }
        let weak = Arc::downgrade(&entry);
        tokio::task::spawn_blocking(move || {
            let Some(entry) = weak.upgrade() else {
                return; // entry removed before the watcher was ready
            };
            let tx = entry.kick_tx.clone();
            let result =
                crate::watch_budget::Watcher::new(move |event: Result<notify::Event, notify::Error>| {
                    let Ok(event) = event else { return };
                    if event
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == ".git"))
                    {
                        let _ = tx.send(());
                    }
                });
            match result {
                Some(mut watcher) => {
                    match watcher.watch(&entry.path, notify::RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            *lock(&entry.folder_watch) = Some(watcher);
                            // Close the check→attach gap: a `.git` change while
                            // unwatched gets caught by this recheck.
                            let _ = entry.kick_tx.send(());
                        }
                        Err(err) => {
                            tracing::debug!(path = %entry.path.display(), error = %err, "spaces: watch failed");
                        }
                    }
                }
                None => tracing::debug!("spaces: watcher unavailable; using repair polling"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::repos::{is_automatic_access_blocked, is_broad_workspace_root};
    use std::path::Path;

    #[test]
    fn native_watch_skips_the_live_home_root() {
        let home = crate::repos::home_dir();
        assert!(is_broad_workspace_root(&home));
        assert!(is_automatic_access_blocked(&home.join("Music")));
        assert!(is_automatic_access_blocked(
            &home.join("Pictures").join("Photos Library.photoslibrary")
        ));
        assert!(is_automatic_access_blocked(
            &home.join("Documents").join("project")
        ));
        assert!(is_broad_workspace_root(Path::new("/")));
        assert!(is_broad_workspace_root(Path::new("relative/project")));
    }

    /// A worktree at `wt` linked to `<repo>/.git` the way `git worktree add`
    /// lays it out.
    fn link_worktree(repo: &Path, wt: &Path) {
        let admin = repo.join(".git/worktrees").join(wt.file_name().unwrap());
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(wt).unwrap();
        let admin = std::fs::canonicalize(&admin).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    }

    #[tokio::test]
    async fn repair_folds_worktree_spaces_and_rehomes_their_orphans_only_when_allowed() {
        use crate::workspace_host::{WorkspaceHost, WorkspaceHostConfig};
        use mast_doc::{REGISTRY_DOC_ID, RegistryDoc};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let repo = base.join("repo");
        let wt = repo.join(".worktrees/feature");
        link_worktree(&repo, &wt);
        let space = |id: &str, path: &Path| mast_proto::Space {
            id: id.into(),
            device_id: "dev".into(),
            path: path.to_string_lossy().into_owned(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: chrono::Utc::now(),
        };
        let mut doc = RegistryDoc::new("dev");
        let prompts = mast_proto::PrReviewPrompts::neutral();
        doc.create_space_with_pr_prompts(&space("repo", &repo), &prompts)
            .unwrap();
        doc.create_space_with_pr_prompts(&space("wt", &wt), &prompts)
            .unwrap();
        let now = chrono::Utc::now();
        doc.claim_chat("in-wt", wt.to_str(), Some("wt"), now);
        doc.claim_chat("orphan-wt", wt.to_str(), Some("gone"), now);
        doc.claim_chat("orphan-plain", base.to_str(), Some("gone"), now);
        let store = Arc::new(mast_sync::DocsStore::open(&dir.path().join("store")).unwrap());
        store
            .save_snapshot(REGISTRY_DOC_ID, &doc.to_bytes().unwrap())
            .unwrap();
        let host = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: "dev".into(),
                device_name: "Test".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        let space_of = |chat: &str| host.chat(chat).unwrap().map(|c| c.space_id.unwrap());
        let spaces = || {
            let mut ids: Vec<String> = host
                .read_spaces()
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
            ids.sort();
            ids
        };

        let none = std::collections::HashSet::new();
        // Not synced: nothing at all, not even the orphan deletes.
        super::repair_spaces(&host, "dev", false, false, Some(&none));
        assert_eq!(spaces(), ["repo", "wt"]);
        assert_eq!(space_of("orphan-plain").as_deref(), Some("gone"));

        // Outline writes switched off: no fold, but orphans still resolve.
        super::repair_spaces(&host, "dev", true, true, Some(&none));
        assert_eq!(spaces(), ["repo", "wt"]);
        assert_eq!(space_of("in-wt").as_deref(), Some("wt"));
        assert_eq!(space_of("orphan-wt").as_deref(), Some("repo"));
        assert_eq!(space_of("orphan-plain"), None);

        // Held Spaces unknown, or a scheduled task still launching into the
        // worktree Space: no fold.
        super::repair_spaces(&host, "dev", true, false, None);
        assert_eq!(spaces(), ["repo", "wt"]);
        let held = std::collections::HashSet::from(["wt".to_string()]);
        super::repair_spaces(&host, "dev", true, false, Some(&held));
        assert_eq!(spaces(), ["repo", "wt"]);
        assert_eq!(space_of("in-wt").as_deref(), Some("wt"));

        super::repair_spaces(&host, "dev", true, false, Some(&none));
        assert_eq!(spaces(), ["repo"]);
        assert_eq!(space_of("in-wt").as_deref(), Some("repo"));
    }
}

/// Per-space task: trailing-debounce kicks, then recheck git presence.
async fn entry_task(
    inner: Weak<SpacesSyncInner>,
    space_id: String,
    entry: Weak<SpaceEntry>,
    mut kick_rx: mpsc::UnboundedReceiver<()>,
) {
    while kick_rx.recv().await.is_some() {
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
        check_space(&inner, &space_id, &entry.path).await;
    }
}

/// Probe git presence and stamp the row — write only on change.
async fn check_space(inner: &Arc<SpacesSyncInner>, space_id: &str, path: &Path) {
    // A broad historical Space is display state, not permission to touch HOME
    // every two minutes. Avoid even spawning `git -C HOME` here; besides being
    // unnecessary, filesystem metadata probes by descendants can raise macOS
    // protected-folder prompts. The watcher is skipped in `reconcile` too.
    let automatic_access_blocked = crate::repos::is_automatic_access_blocked(path);
    let detected = !automatic_access_blocked && inner.repos.is_repo(path).await;
    let checkout_id = if detected {
        match inner.repos.checkout_identity(path).await {
            Ok(identity) => Some(identity.id),
            Err(err) => {
                tracing::debug!(space = %space_id, error = %err, "spaces: checkout identity failed");
                None
            }
        }
    } else {
        None
    };
    let current = match inner.workspace.read_spaces() {
        Ok(spaces) => spaces.into_iter().find(|s| s.id == space_id),
        Err(err) => {
            tracing::warn!(space = %space_id, error = %err, "spaces: row read failed");
            return;
        }
    };
    let Some(current) = current else {
        return; // deleted while checking
    };
    if current.git_detected == detected && current.checkout_id == checkout_id {
        return; // unchanged — no oplog growth
    }
    match inner
        .workspace
        .set_space_git(space_id, detected, checkout_id.as_deref())
    {
        Ok(_) => {
            tracing::info!(space = %space_id, git = detected, "space git presence updated");
        }
        Err(err) => {
            tracing::warn!(space = %space_id, error = %err, "spaces: git stamp failed");
        }
    }
}

/// Host-side repair: delete OUR chats whose `spaceId` dangles (create-vs-delete
/// race). Chats hosted by other devices are left alone.
fn sweep_orphans(inner: &Arc<SpacesSyncInner>) {
    let holds = lock(&inner.space_holds).clone();
    let held = holds
        .map(|holds| holds())
        .transpose()
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "spaces: worktree fold skipped (held Spaces unknown)");
            None
        });
    repair_spaces(
        &inner.workspace,
        &inner.device_id,
        inner.workspace.registry_synced(),
        crate::work_outline::writes_disabled(),
        held.as_ref(),
    );
}

/// [`sweep_orphans`] with its gates passed in. `held` lists Spaces the fold
/// must keep; `None` (unknown) skips the fold.
fn repair_spaces(
    workspace: &WorkspaceHost,
    device_id: &str,
    synced: bool,
    writes_disabled: bool,
    held: Option<&HashSet<String>>,
) {
    // NEVER sweep off a replica that hasn't heard from the server this boot:
    // a gapped/pre-heal/offline-at-boot view is missing rows, and "space row
    // absent" on it is not evidence the space was deleted — sweeping there
    // globally destroys live chats (2026-08-23 missing-sessions audit).
    if !synced {
        tracing::debug!("spaces: orphan sweep skipped (registry not yet synced this boot)");
        return;
    }
    // Worktree Spaces (minted before claims and imports resolved worktrees to
    // their checkout's Space) fold into that Space; same synced gate, since
    // the fold deletes the emptied Space.
    if !writes_disabled && let Some(held) = held {
        let folded = workspace.fold_worktree_spaces(held);
        if folded > 0 {
            tracing::info!(spaces = folded, "folded worktree spaces");
        }
    }
    let spaces = workspace.watch_spaces().borrow().clone();
    let live: std::collections::HashSet<&str> = spaces.iter().map(|s| s.id.as_str()).collect();
    let chats = workspace.watch_chats().borrow().clone();
    for chat in chats {
        if chat.device_id != device_id {
            continue;
        }
        let Some(space_id) = chat.space_id.as_deref() else {
            continue;
        };
        if live.contains(space_id) {
            continue;
        }
        if workspace.rehome_worktree_chat(&chat) {
            tracing::info!(chat = %chat.id, space = %space_id, "re-homed orphaned worktree chat");
            continue;
        }
        tracing::info!(chat = %chat.id, space = %space_id, "deleting orphaned chat (space gone)");
        if let Err(err) = workspace.delete_chat(&chat.id) {
            tracing::warn!(chat = %chat.id, error = %err, "spaces: orphan delete failed");
        }
    }
    // Work Outline rows of Spaces that are gone (a concurrent create the
    // `delete_space` cascade missed, or a peer that predates the cascade).
    // Any device may sweep: the dead Space's owner cannot be consulted. Same
    // synced gate as above; the write kill switch covers deletes too.
    if !writes_disabled {
        let swept = workspace.sweep_work_outline_orphans();
        if swept > 0 {
            tracing::info!(
                rows = swept,
                "tombstoned orphaned work outline rows (space gone)"
            );
        }
    }
}

/// Spaces-watch follower + repair tick. Weak handles so dropping the service
/// tears the loop down; the token ends it eagerly on shutdown.
async fn spaces_task(
    inner: Weak<SpacesSyncInner>,
    mut spaces_rx: watch::Receiver<Vec<Space>>,
    cancel: CancellationToken,
) {
    let mut repair = tokio::time::interval(REPAIR_INTERVAL);
    repair.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    repair.tick().await; // consume the immediate first tick
    {
        let Some(inner) = inner.upgrade() else { return };
        let spaces = spaces_rx.borrow().clone();
        reconcile(&inner, &spaces);
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            changed = spaces_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(inner) = inner.upgrade() else { break };
                let spaces = spaces_rx.borrow_and_update().clone();
                reconcile(&inner, &spaces);
            }
            _ = repair.tick() => {
                let Some(inner) = inner.upgrade() else { break };
                let spaces = spaces_rx.borrow().clone();
                reconcile(&inner, &spaces);
                for entry in lock(&inner.entries).values() {
                    let _ = entry.kick_tx.send(());
                }
                sweep_orphans(&inner);
            }
        }
    }
}
