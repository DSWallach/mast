//! Checkout symbol snapshots, driven by the shared file index and watcher.

use super::*;
use mast_syntax::symbols::{FileChange, Symbol, SymbolIndex};
use std::collections::BTreeMap;

const MAX_INDEX_FILES: usize = 50_000;
const MAX_INDEX_SYMBOLS: usize = 200_000;
const MAX_FILE_SYMBOLS: usize = 5_000;
const MAX_CHECKOUT_INDEXES: usize = 2;
const FRESHNESS_BATCH_SIZE: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRevision { size: u64, modified: std::time::SystemTime }

fn file_revision(root: &Path, path: &str) -> Option<FileRevision> {
    let relative = WorkspaceRelativePath::file(path).ok()?;
    let metadata = checked_file_metadata(root, &relative).ok()?;
    Some(FileRevision { size: metadata.len(), modified: metadata.modified().ok()? })
}

#[derive(Clone, PartialEq, Eq)]
pub struct SymbolSnapshot {
    pub files: Arc<BTreeMap<String, Vec<Symbol>>>,
    pub truncated: bool,
    pub updating: bool,
}
pub type SymbolResult = Result<Arc<SymbolSnapshot>, String>;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshSymbolsRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    #[serde(default)]
    pub force: bool,
}

pub(super) struct SymbolIndexSlot {
    sender: tokio::sync::watch::Sender<Option<SymbolResult>>,
    cancel: CancellationToken,
    accessed: u64,
    refresh: Arc<Notify>,
    force_refresh: Arc<AtomicBool>,
}

impl WorkspaceFiles {
    pub async fn subscribe_symbols(
        &self,
        target: WorkspaceTarget,
    ) -> Result<tokio::sync::watch::Receiver<Option<SymbolResult>>, WorkspaceFilesError> {
        let workspace = self.resolve_target(&target).await?;
        let accessed = self.inner.symbol_index_access.fetch_add(1, Ordering::Relaxed);
        let (receiver, sender) = {
            let mut indexes = lock(&self.inner.symbol_indexes);
            if let Some(slot) = indexes.get_mut(&workspace.checkout_id) {
                slot.accessed = accessed;
                (slot.sender.subscribe(), None)
            } else {
                if indexes.len() >= MAX_CHECKOUT_INDEXES {
                    let idle = indexes.iter().filter(|(_, slot)| slot.sender.receiver_count() == 0).min_by_key(|(_, slot)| slot.accessed).map(|(key, _)| key.clone());
                    let Some(idle) = idle else { return Err(WorkspaceFilesError::Unsupported("Symbol indexes are busy in other checkouts".into())); };
                    if let Some(slot) = indexes.remove(&idle) { slot.cancel.cancel(); }
                }
                let (sender, receiver) = tokio::sync::watch::channel(None);
                let cancel = self.inner.cancel.child_token();
                let refresh = Arc::new(Notify::new());
                let force_refresh = Arc::new(AtomicBool::new(false));
                indexes.insert(workspace.checkout_id.clone(), SymbolIndexSlot { sender: sender.clone(), cancel: cancel.clone(), accessed, refresh: refresh.clone(), force_refresh: force_refresh.clone() });
                (receiver, Some((sender, cancel, refresh, force_refresh)))
            }
        };
        if let Some((sender, cancel, refresh, force_refresh)) = sender {
            let service = self.clone();
            tokio::spawn(async move {
                let result = run_index(&service, &workspace, target, &sender, &cancel, refresh, force_refresh).await;
                if let Err(error) = result {
                    sender.send_replace(Some(Err(error)));
                }
                let mut indexes = lock(&service.inner.symbol_indexes);
                if indexes
                    .get(&workspace.checkout_id)
                    .is_some_and(|current| current.sender.same_channel(&sender))
                {
                    indexes.remove(&workspace.checkout_id);
                }
            });
        }
        Ok(receiver)
    }

    pub async fn refresh_symbols(&self, target: WorkspaceTarget, force: bool) -> Result<(), WorkspaceFilesError> {
        let receiver = self.subscribe_symbols(target.clone()).await?;
        let workspace = self.resolve_target(&target).await?;
        let indexes = lock(&self.inner.symbol_indexes);
        let slot = indexes.get(&workspace.checkout_id).ok_or_else(|| WorkspaceFilesError::Io("symbol index stopped".into()))?;
        if force { slot.force_refresh.store(true, Ordering::Release); }
        slot.refresh.notify_one();
        drop(receiver);
        Ok(())
    }
}

async fn run_index(
    service: &WorkspaceFiles,
    workspace: &ResolvedWorkspace,
    target: WorkspaceTarget,
    sender: &tokio::sync::watch::Sender<Option<SymbolResult>>,
    cancel: &CancellationToken,
    refresh: Arc<Notify>,
    force_refresh: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut changes = service
        .watch_files(WatchWorkspaceFilesRequest {
            target: target.clone(),
        })
        .await
        .map_err(|error| error.to_string())?;
    let mut paths = service
        .subscribe_search_files(target)
        .await
        .map_err(|error| error.to_string())?;
    // The watch was registered first. Discard its baseline; later events are
    // queued while the first snapshot is built.
    let _ = changes.recv().await;
    let mut known = HashSet::new();
    let mut paths_truncated = false;
    let mut index = SymbolIndex::new(MAX_INDEX_FILES, MAX_INDEX_SYMBOLS, MAX_FILE_SYMBOLS);
    let mut revision = 0u64;
    let mut file_revisions = HashMap::new();
    let mut first = true;
    loop {
        let mut updates = HashSet::new();
        let mut removed = HashSet::new();
        let mut paths_updated = first;
        let mut check_freshness = false;
        let mut force = false;
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = service.inner.cancel.cancelled() => return Ok(()),
            result = paths.changed(), if !first => {
                if result.is_err() { return Err("workspace file index stopped".into()); }
                paths_updated = true;
            }
            _ = refresh.notified(), if !first => {
                check_freshness = true;
                force = force_refresh.swap(false, Ordering::AcqRel);
            }
            event = changes.recv(), if !first => {
                let Some(event) = event else { return Err("workspace file watcher stopped".into()); };
                if event.resync_required { check_freshness = true; }
                for change in event.changes {
                    if let Some(old) = change.old_path && known.contains(&old) { removed.insert(old); }
                    if known.contains(&change.path) {
                        if change.kind == WorkspaceFileChangeKind::Removed { removed.insert(change.path); }
                        else { updates.insert(change.path); }
                    }
                }
            }
            _ = async {}, if first => {}
        }
        if paths_updated {
            let snapshot = paths.borrow_and_update().clone();
            match snapshot {
                Some(Ok(paths)) => {
                    let next: Vec<_> = paths
                        .into_iter()
                        .filter(|path| mast_syntax::symbols::supports_path(path))
                        .take(MAX_INDEX_FILES + 1)
                        .collect();
                    paths_truncated = next.len() > MAX_INDEX_FILES;
                    let next: HashSet<_> = next.into_iter().take(MAX_INDEX_FILES).collect();
                    updates.extend(next.difference(&known).cloned());
                    removed.extend(known.difference(&next).cloned());
                    known = next;
                    first = false;
                }
                Some(Err(error)) => return Err(error),
                None => {
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        _ = service.inner.cancel.cancelled() => return Ok(()),
                        result = paths.changed() => if result.is_err() { return Err("workspace file index stopped".into()); },
                    }
                    continue;
                }
            }
        }
        if updates.is_empty() && removed.is_empty() && !check_freshness {
            let truncated = paths_truncated || index.truncated();
            let publish = sender
                .borrow()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .is_none_or(|snapshot| snapshot.truncated != truncated);
            if publish {
                sender.send_replace(Some(Ok(Arc::new(SymbolSnapshot {
                    files: Arc::new(index.files().clone()),
                    truncated,
                    updating: false,
                }))));
            }
            continue;
        }
        let previous = sender.borrow().as_ref().and_then(|result| result.as_ref().ok()).cloned();
        if let Some(previous) = &previous && !previous.updating {
            sender.send_replace(Some(Ok(Arc::new(SymbolSnapshot { files: previous.files.clone(), truncated: previous.truncated, updating: true }))));
        }
        let root = workspace.root.clone();
        static WORKERS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let semaphore = WORKERS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone();
        let permit = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = service.inner.cancel.cancelled() => return Ok(()),
            permit = semaphore.acquire_owned() => permit.map_err(|error| error.to_string())?,
        };
        let cancellation = cancel.clone();
        let (next, next_revision, next_file_revisions, next_known, snapshot) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut updates = updates;
            if check_freshness {
                let mut paths: Vec<_> = known.iter().cloned().collect();
                paths.sort_unstable();
                for batch in paths.chunks(FRESHNESS_BATCH_SIZE) {
                    if cancellation.is_cancelled() { break; }
                    updates.extend(changed_files(&root, batch, &file_revisions, force));
                }
            }
            let had_removals = !removed.is_empty();
            let (index, revision, file_revisions, reads) = update_paths(&root, index, revision, file_revisions, removed, updates, || cancellation.is_cancelled());
            let files = if reads == 0 && !had_removals { previous.map(|snapshot| snapshot.files.clone()).unwrap_or_else(|| Arc::new(index.files().clone())) } else { Arc::new(index.files().clone()) };
            let snapshot = Arc::new(SymbolSnapshot {
                files,
                truncated: paths_truncated || index.truncated(),
                updating: false,
            });
            (index, revision, file_revisions, known, snapshot)
        })
        .await
        .map_err(|error| error.to_string())?;
        index = next;
        revision = next_revision;
        file_revisions = next_file_revisions;
        known = next_known;
        if sender
            .borrow()
            .as_ref()
            .and_then(|value| value.as_ref().ok())
            .is_none_or(|previous| **previous != *snapshot)
        {
            sender.send_replace(Some(Ok(snapshot)));
        }
    }
}

fn update_paths(
    root: &Path,
    mut index: SymbolIndex,
    mut revision: u64,
    mut file_revisions: HashMap<String, FileRevision>,
    removed: HashSet<String>,
    updates: HashSet<String>,
    cancelled: impl Fn() -> bool,
) -> (SymbolIndex, u64, HashMap<String, FileRevision>, usize) {
    let mut removed: Vec<_> = removed.into_iter().collect();
    let mut updates: Vec<_> = updates.into_iter().collect();
    removed.sort_unstable();
    updates.sort_unstable();
    let mut reads = 0;
    for path in removed {
        if cancelled() {
            break;
        }
        revision += 1;
        file_revisions.remove(&path);
        index.apply(revision, &[FileChange::Removed { path: &path }]);
    }
    for path in updates {
        if cancelled() {
            break;
        }
        revision += 1;
        reads += 1;
        let current_revision = file_revision(root, &path);
        let source = WorkspaceRelativePath::file(&path)
            .ok()
            .and_then(|relative| {
                let metadata = checked_file_metadata(root, &relative).ok()?;
                if metadata.len() > mast_syntax::DEFAULT_MAX_SOURCE_BYTES as u64 {
                    return None;
                }
                read_file_blocking(root, &relative, &AtomicBool::new(false))
                    .ok()?
                    .text
            });
        match source {
            Some(source) => {
                index.apply(
                    revision,
                    &[FileChange::Changed {
                        path: &path,
                        source: &source,
                    }],
                );
            }
            None => {
                index.apply(revision, &[FileChange::Removed { path: &path }]);
            }
        }
        if let Some(current_revision) = current_revision { file_revisions.insert(path, current_revision); }
        else { file_revisions.remove(&path); }
    }
    (index, revision, file_revisions, reads)
}

fn changed_files(root: &Path, paths: &[String], revisions: &HashMap<String, FileRevision>, force: bool) -> HashSet<String> {
    paths.iter().filter(|path| force || match file_revision(root, path) {
        Some(revision) => revisions.get(*path) != Some(&revision),
        None => true,
    }).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn peak_resident_bytes() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the output on success, checked below.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful call initialized the structure.
        let resident = unsafe { usage.assume_init() }.ru_maxrss as u64;
        if cfg!(target_os = "macos") {
            resident
        } else {
            resident.saturating_mul(1024)
        }
    }

    #[test]
    fn checkout_index_budget_and_no_changes() {
        let root = tempfile::tempdir().unwrap();
        let source = (0..20)
            .map(|number| format!("fn function_{number}() {{}}\n"))
            .collect::<String>();
        let mut paths = HashSet::new();
        for number in 0..1000 {
            let path = format!("file_{number}.rs");
            std::fs::write(root.path().join(&path), &source).unwrap();
            paths.insert(path);
        }
        #[cfg(unix)]
        let before_peak = peak_resident_bytes();
        let started = Instant::now();
        let (index, revision, file_revisions, reads) = update_paths(
            root.path(),
            SymbolIndex::new(MAX_INDEX_FILES, MAX_INDEX_SYMBOLS, MAX_FILE_SYMBOLS),
            0,
            HashMap::new(),
            HashSet::new(),
            paths,
            || false,
        );
        let elapsed = started.elapsed();
        let bytes = index.estimated_bytes();
        eprintln!(
            "checkout index: {reads} files, {} symbols, {bytes} estimated payload bytes, {elapsed:?}",
            index.symbol_count()
        );
        #[cfg(unix)]
        {
            let peak_growth = peak_resident_bytes().saturating_sub(before_peak);
            eprintln!("checkout index peak resident growth: {peak_growth} bytes");
            assert!(peak_growth < 128 * 1024 * 1024);
        }
        assert_eq!(reads, 1000);
        assert_eq!(index.symbol_count(), 20_000);
        assert!(bytes < 4 * 1024 * 1024);
        assert!(elapsed < Duration::from_secs(20));
        let first_batch: Vec<_> = file_revisions.keys().take(FRESHNESS_BATCH_SIZE).cloned().collect();
        let check_started = Instant::now();
        let unchanged = changed_files(root.path(), &first_batch, &file_revisions, false);
        let check_elapsed = check_started.elapsed();
        eprintln!("unchanged open check: {} files in {:?}", first_batch.len(), check_elapsed);
        assert!(unchanged.is_empty());
        assert!(check_elapsed < Duration::from_millis(20));
        std::fs::write(root.path().join("file_0.rs"), "fn changed() {}\n").unwrap();
        let update_started = Instant::now();
        let (index, revision, file_revisions, changed_reads) = update_paths(
            root.path(),
            index,
            revision,
            file_revisions,
            HashSet::new(),
            HashSet::from(["file_0.rs".into()]),
            || false,
        );
        eprintln!("single-file index update: {:?}", update_started.elapsed());
        assert_eq!(changed_reads, 1);
        assert_eq!(index.files()["file_0.rs"][0].name, "changed");
        assert_eq!(index.files()["file_1.rs"].len(), 20);
        assert!(update_started.elapsed() < Duration::from_secs(2));
        let (_, unchanged_revision, _, reads) = update_paths(
            root.path(),
            index,
            revision,
            file_revisions,
            HashSet::new(),
            HashSet::new(),
            || false,
        );
        assert_eq!(reads, 0);
        assert_eq!(unchanged_revision, revision);
    }
}
