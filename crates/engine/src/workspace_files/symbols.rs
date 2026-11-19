//! Checkout symbol snapshots, driven by the shared file index and watcher.

use super::*;
use mast_syntax::symbols::{FileChange, Symbol, SymbolIndex};
use std::collections::BTreeMap;

const MAX_INDEX_FILES: usize = 50_000;
const MAX_INDEX_SYMBOLS: usize = 200_000;
const MAX_FILE_SYMBOLS: usize = 5_000;

pub(super) type SymbolResult = Result<Arc<BTreeMap<String, Vec<Symbol>>>, String>;

impl WorkspaceFiles {
    pub async fn subscribe_symbols(
        &self,
        target: WorkspaceTarget,
    ) -> Result<tokio::sync::watch::Receiver<Option<SymbolResult>>, WorkspaceFilesError> {
        let workspace = self.resolve_target(&target).await?;
        let (receiver, sender) = {
            let mut indexes = lock(&self.inner.symbol_indexes);
            if let Some(sender) = indexes
                .get(&workspace.checkout_id)
                .filter(|sender| sender.receiver_count() > 0)
            {
                (sender.subscribe(), None)
            } else {
                let (sender, receiver) = tokio::sync::watch::channel(None);
                indexes.insert(workspace.checkout_id.clone(), sender.clone());
                (receiver, Some(sender))
            }
        };
        if let Some(sender) = sender {
            let service = self.clone();
            tokio::spawn(async move {
                let result = run_index(&service, &workspace, target, &sender).await;
                if let Err(error) = result {
                    sender.send_replace(Some(Err(error)));
                }
                let mut indexes = lock(&service.inner.symbol_indexes);
                if indexes
                    .get(&workspace.checkout_id)
                    .is_some_and(|current| current.same_channel(&sender))
                {
                    indexes.remove(&workspace.checkout_id);
                }
            });
        }
        Ok(receiver)
    }
}

async fn run_index(
    service: &WorkspaceFiles,
    workspace: &ResolvedWorkspace,
    target: WorkspaceTarget,
    sender: &tokio::sync::watch::Sender<Option<SymbolResult>>,
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
    let mut index = SymbolIndex::new(MAX_INDEX_FILES, MAX_INDEX_SYMBOLS, MAX_FILE_SYMBOLS);
    let mut revision = 0u64;
    let mut first = true;
    loop {
        let mut updates = HashSet::new();
        let mut removed = HashSet::new();
        let mut paths_updated = first;
        tokio::select! {
            _ = sender.closed() => return Ok(()),
            _ = service.inner.cancel.cancelled() => return Ok(()),
            result = paths.changed(), if !first => {
                if result.is_err() { return Err("workspace file index stopped".into()); }
                paths_updated = true;
            }
            event = changes.recv(), if !first => {
                let Some(event) = event else { return Err("workspace file watcher stopped".into()); };
                if event.resync_required { updates.extend(known.iter().cloned()); }
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
                    let next: HashSet<_> = paths
                        .into_iter()
                        .filter(|path| mast_syntax::symbols::supports_path(path))
                        .take(MAX_INDEX_FILES)
                        .collect();
                    updates.extend(next.difference(&known).cloned());
                    removed.extend(known.difference(&next).cloned());
                    known = next;
                    first = false;
                }
                Some(Err(error)) => return Err(error),
                None => {
                    tokio::select! {
                        _ = sender.closed() => return Ok(()),
                        _ = service.inner.cancel.cancelled() => return Ok(()),
                        result = paths.changed() => if result.is_err() { return Err("workspace file index stopped".into()); },
                    }
                    continue;
                }
            }
        }
        if updates.is_empty() && removed.is_empty() {
            if sender.borrow().is_none() {
                sender.send_replace(Some(Ok(Arc::new(BTreeMap::new()))));
            }
            continue;
        }
        let root = workspace.root.clone();
        static WORKERS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let semaphore = WORKERS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone();
        let permit = tokio::select! {
            _ = sender.closed() => return Ok(()),
            _ = service.inner.cancel.cancelled() => return Ok(()),
            permit = semaphore.acquire_owned() => permit.map_err(|error| error.to_string())?,
        };
        let cancellation = service.inner.cancel.clone();
        let subscribers = sender.clone();
        let (next, next_revision) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut removed: Vec<_> = removed.into_iter().collect();
            let mut updates: Vec<_> = updates.into_iter().collect();
            removed.sort_unstable();
            updates.sort_unstable();
            for path in removed {
                if cancellation.is_cancelled() || subscribers.receiver_count() == 0 {
                    break;
                }
                revision += 1;
                index.apply(revision, &[FileChange::Removed { path: &path }]);
            }
            for path in updates {
                if cancellation.is_cancelled() || subscribers.receiver_count() == 0 {
                    break;
                }
                revision += 1;
                let source = WorkspaceRelativePath::file(&path)
                    .ok()
                    .and_then(|relative| {
                        let metadata = checked_file_metadata(&root, &relative).ok()?;
                        if metadata.len() > mast_syntax::DEFAULT_MAX_SOURCE_BYTES as u64 {
                            return None;
                        }
                        read_file_blocking(&root, &relative, &AtomicBool::new(false))
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
            }
            (index, revision)
        })
        .await
        .map_err(|error| error.to_string())?;
        index = next;
        revision = next_revision;
        let snapshot = Arc::new(index.files().clone());
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
