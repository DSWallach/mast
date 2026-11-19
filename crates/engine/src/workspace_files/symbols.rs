//! Checkout symbol snapshots, driven by the shared file index and watcher.

use super::*;
use mast_syntax::symbols::{FileChange, Symbol, SymbolIndex};

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
    pub revision: u64,
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySymbolsRequest {
    #[serde(flatten)] pub target: WorkspaceTarget,
    pub query: String,
    pub limit: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolMatch { pub path: String, pub symbol: Symbol }

pub(super) struct SymbolIndexSlot {
    sender: tokio::sync::watch::Sender<Option<SymbolResult>>,
    cancel: CancellationToken,
    accessed: u64,
    refresh: Arc<Notify>,
    force_refresh: Arc<AtomicBool>,
    index: Arc<std::sync::Mutex<SymbolIndex>>,
    idle_checks: u8,
}

pub(super) fn retire_idle_symbol_index(indexes: &std::sync::Mutex<HashMap<String, SymbolIndexSlot>>, checkout_id: &str) {
    let mut indexes = lock(indexes);
    let Some(slot) = indexes.get_mut(checkout_id) else { return; };
    if slot.sender.receiver_count() == 0 { slot.idle_checks = slot.idle_checks.saturating_add(1); } else { slot.idle_checks = 0; }
    if slot.idle_checks >= 2 {
        if let Some(slot) = indexes.remove(checkout_id) { slot.cancel.cancel(); }
    }
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
            if indexes.get(&workspace.checkout_id).is_some_and(|slot| slot.cancel.is_cancelled()) {
                indexes.remove(&workspace.checkout_id);
            }
            if let Some(slot) = indexes.get_mut(&workspace.checkout_id) {
                slot.accessed = accessed;
                slot.idle_checks = 0;
                (slot.sender.subscribe(), None)
            } else {
                if indexes.len() >= MAX_CHECKOUT_INDEXES {
                    let idle = indexes.iter().min_by_key(|(_, slot)| slot.accessed).map(|(key, _)| key.clone()).expect("full cache");
                    if let Some(slot) = indexes.remove(&idle) { slot.cancel.cancel(); }
                }
                let (sender, receiver) = tokio::sync::watch::channel(None);
                let cancel = self.inner.cancel.child_token();
                let refresh = Arc::new(Notify::new());
                let force_refresh = Arc::new(AtomicBool::new(false));
                let index = Arc::new(std::sync::Mutex::new(SymbolIndex::new(MAX_INDEX_FILES, MAX_INDEX_SYMBOLS, MAX_FILE_SYMBOLS)));
                indexes.insert(workspace.checkout_id.clone(), SymbolIndexSlot { sender: sender.clone(), cancel: cancel.clone(), accessed, refresh: refresh.clone(), force_refresh: force_refresh.clone(), index: index.clone(), idle_checks: 0 });
                (receiver, Some((sender, cancel, refresh, force_refresh, index)))
            }
        };
        if let Some((sender, cancel, refresh, force_refresh, index)) = sender {
            let service = self.clone();
            tokio::spawn(async move {
                let result = run_index(&service, &workspace, target, &sender, &cancel, refresh, force_refresh, index).await;
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

    pub async fn query_symbols(&self, request: QuerySymbolsRequest) -> Result<Vec<SymbolMatch>, WorkspaceFilesError> {
        if request.query.chars().count() > 256 { return Err(WorkspaceFilesError::BadParams("symbol query too long".into())); }
        let receiver = self.subscribe_symbols(request.target.clone()).await?;
        let workspace = self.resolve_target(&request.target).await?;
        let index = lock(&self.inner.symbol_indexes).get(&workspace.checkout_id).ok_or_else(|| WorkspaceFilesError::Io("symbol index stopped".into()))?.index.clone();
        let query = request.query.to_lowercase();
        let limit = request.limit.min(200);
        let results = tokio::task::spawn_blocking(move || {
            let index = lock(&index);
            let mut best = Vec::with_capacity(limit + 1);
            for (path, symbols) in index.files() {
                let lower_path = path.to_lowercase();
                for symbol in symbols {
                    let Some(score) = symbol_score(&symbol.name.to_lowercase(), &lower_path, &query) else { continue; };
                    let position = best.partition_point(|(existing_score, existing_path, existing_symbol): &(i64, &String, &Symbol)| existing_score > &score || (existing_score == &score && (existing_path.as_str(), existing_symbol.start_line) <= (path.as_str(), symbol.start_line)));
                    if position < limit {
                        best.insert(position, (score, path, symbol));
                        if best.len() > limit { best.pop(); }
                    }
                }
            }
            best.into_iter().map(|(_, path, symbol)| SymbolMatch { path: path.clone(), symbol: symbol.clone() }).collect()
        }).await.map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
        drop(receiver);
        Ok(results)
    }
}

fn symbol_score(name: &str, path: &str, query: &str) -> Option<i64> {
    if query.is_empty() { return Some(0); }
    if name == query { return Some(10_000); }
    if name.starts_with(query) { return Some(8_000 - name.len() as i64); }
    if let Some(i) = name.find(query) { return Some(6_000 - i as i64 - name.len() as i64); }
    if let Some(i) = path.find(query) { return Some(4_000 - i as i64 - path.len() as i64); }
    let mut wanted = query.chars();
    let mut current = wanted.next()?;
    let mut gaps = 0;
    for c in name.chars().chain(path.chars()) {
        if c == current { match wanted.next() { Some(next) => current = next, None => return Some(2_000 - gaps - name.len() as i64) } } else { gaps += 1; }
    }
    None
}

async fn run_index(
    service: &WorkspaceFiles,
    workspace: &ResolvedWorkspace,
    target: WorkspaceTarget,
    sender: &tokio::sync::watch::Sender<Option<SymbolResult>>,
    cancel: &CancellationToken,
    refresh: Arc<Notify>,
    force_refresh: Arc<AtomicBool>,
    shared_index: Arc<std::sync::Mutex<SymbolIndex>>,
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
    let mut revision = 0u64;
    let mut file_revisions = HashMap::new();
    let mut recent = Vec::<String>::new();
    let mut first = true;
    loop {
        let mut updates = HashSet::new();
        let mut removed = HashSet::new();
        let mut paths_updated = first;
        let mut check_freshness = false;
        let mut force = false;
        let mut repair = false;
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
                let mut next = Some(event);
                while let Some(event) = next {
                    if event.resync_required { check_freshness = true; repair = true; }
                    for change in event.changes {
                        if let Some(old) = change.old_path && known.contains(&old) { removed.insert(old); }
                        if known.contains(&change.path) {
                            if change.kind == WorkspaceFileChangeKind::Removed { updates.remove(&change.path); removed.insert(change.path); }
                            else { removed.remove(&change.path); updates.insert(change.path); }
                        }
                    }
                    next = changes.try_recv();
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
        if revision != 0 {
            for path in &updates {
                recent.retain(|candidate| candidate != path);
                recent.push(path.clone());
                if recent.len() > FRESHNESS_BATCH_SIZE { recent.remove(0); }
            }
        }
        if updates.is_empty() && removed.is_empty() && !check_freshness {
            let truncated = paths_truncated || lock(&shared_index).truncated();
            let publish = sender
                .borrow()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .is_none_or(|snapshot| snapshot.truncated != truncated);
            if publish {
                sender.send_replace(Some(Ok(Arc::new(SymbolSnapshot {
                    revision,
                    truncated,
                    updating: false,
                }))));
            }
            continue;
        }
        let previous = sender.borrow().as_ref().and_then(|result| result.as_ref().ok()).cloned();
        if let Some(previous) = &previous && !previous.updating {
            sender.send_replace(Some(Ok(Arc::new(SymbolSnapshot { revision: previous.revision, truncated: previous.truncated, updating: true }))));
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
        let worker_index = shared_index.clone();
        let mut recent = recent.clone();
        let initial_build = revision == 0;
        let (next_revision, next_file_revisions, next_known, next_recent, snapshot) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut updates = updates;
            if check_freshness {
                let paths = freshness_paths(&known, &recent, force || repair);
                for batch in paths.chunks(FRESHNESS_BATCH_SIZE) {
                    if cancellation.is_cancelled() { break; }
                    updates.extend(changed_files(&root, batch, &file_revisions, force));
                }
            }
            let mut guard = lock(&worker_index);
            let index = std::mem::replace(&mut *guard, SymbolIndex::new(MAX_INDEX_FILES, MAX_INDEX_SYMBOLS, MAX_FILE_SYMBOLS));
            let (index, revision, file_revisions, _) = update_paths(&root, index, revision, file_revisions, removed, updates, || cancellation.is_cancelled());
            if initial_build {
                let mut newest: Vec<_> = file_revisions.iter().collect();
                newest.sort_unstable_by_key(|(_, revision)| std::cmp::Reverse(revision.modified));
                recent = newest.into_iter().take(FRESHNESS_BATCH_SIZE).map(|(path, _)| path.clone()).collect();
            }
            let snapshot = Arc::new(SymbolSnapshot {
                revision,
                truncated: paths_truncated || index.truncated(),
                updating: false,
            });
            *guard = index;
            (revision, file_revisions, known, recent, snapshot)
        })
        .await
        .map_err(|error| error.to_string())?;
        revision = next_revision;
        file_revisions = next_file_revisions;
        known = next_known;
        recent = next_recent;
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

fn freshness_paths(known: &HashSet<String>, recent: &[String], all: bool) -> Vec<String> {
    if all { known.iter().cloned().collect() } else { recent.iter().take(FRESHNESS_BATCH_SIZE).cloned().collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_symbol_cache_reuses_file_index_retirement_ticks() {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let cancel = CancellationToken::new();
        let slots = std::sync::Mutex::new(HashMap::from([("checkout".into(), SymbolIndexSlot {
            sender, cancel: cancel.clone(), accessed: 0, refresh: Arc::new(Notify::new()), force_refresh: Arc::new(AtomicBool::new(false)), index: Arc::new(std::sync::Mutex::new(SymbolIndex::new(1, 1, 1))), idle_checks: 0,
        })]));
        retire_idle_symbol_index(&slots, "checkout");
        assert!(lock(&slots).contains_key("checkout"));
        drop(receiver);
        retire_idle_symbol_index(&slots, "checkout");
        assert!(lock(&slots).contains_key("checkout"));
        retire_idle_symbol_index(&slots, "checkout");
        assert!(!lock(&slots).contains_key("checkout"));
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn open_freshness_sample_is_bounded_for_large_checkout() {
        let known: HashSet<_> = (0..50_000).map(|number| format!("file_{number}.rs")).collect();
        let recent: Vec<_> = known.iter().take(FRESHNESS_BATCH_SIZE).cloned().collect();
        let started = Instant::now();
        let paths = freshness_paths(&known, &recent, false);
        assert_eq!(paths.len(), 128);
        assert!(started.elapsed() < Duration::from_millis(5));
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
        assert_eq!(reads, 1000);
        assert_eq!(index.symbol_count(), 20_000);
        assert!(bytes < 4 * 1024 * 1024);
        assert!(elapsed < Duration::from_millis(750));
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
