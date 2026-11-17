//! The read side of planning artifacts: the latest `ArtifactsSnapshot` per
//! Space plus its load state. `AppState` owns one [`ArtifactStore`] and feeds
//! it from `WatchArtifacts` (`state.rs` `reconcile_artifact_watches`), which
//! watches exactly the Spaces the Work Outline reconciler watches. Frames
//! replace a Space's snapshot wholesale. Views join artifacts to the outline
//! by `workItemId` / `chatId`.
use std::collections::HashMap;
use std::sync::Arc;

use mast_proto::{ArtifactView, ArtifactsSnapshot};

/// Per-Space load state. `Unsupported` is a host that answers `unknown
/// method` to `WatchArtifacts` (the engine branch has not landed there yet).
#[derive(Debug, Clone)]
pub(crate) enum ArtifactsLoad {
    Loading,
    Ready(Arc<ArtifactsSnapshot>),
    Unsupported,
    Failed(String),
}

impl ArtifactsLoad {
    pub(crate) fn snapshot(&self) -> Option<&Arc<ArtifactsSnapshot>> {
        match self {
            Self::Ready(snapshot) => Some(snapshot),
            _ => None,
        }
    }

    /// The load state a failed watch maps to: `unknown method` is an
    /// unsupported host; everything else is a retryable failure.
    pub(crate) fn from_watch_error(message: &str) -> Self {
        if message.to_ascii_lowercase().contains("unknown method") {
            Self::Unsupported
        } else {
            Self::Failed(message.to_owned())
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Loading, Self::Loading) | (Self::Unsupported, Self::Unsupported) => true,
            // Frames differ only by their per-stream counter: not a change.
            (Self::Ready(a), Self::Ready(b)) => {
                Arc::ptr_eq(a, b) || (a.space_id == b.space_id && a.artifacts == b.artifacts)
            }
            (Self::Failed(a), Self::Failed(b)) => a == b,
            _ => false,
        }
    }
}

/// Shown wherever an artifact RPC answers `unknown method`.
pub(crate) const UNSUPPORTED_TEXT: &str = "Artifacts aren\u{2019}t available on this host yet";

/// Latest snapshot and load state per Space id.
#[derive(Debug, Default)]
pub(crate) struct ArtifactStore {
    loads: HashMap<String, ArtifactsLoad>,
    ids: HashMap<String, (String, usize)>,
    paths: HashMap<String, Vec<(String, usize)>>,
}

impl ArtifactStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn load(&self, space_id: &str) -> ArtifactsLoad {
        self.loads
            .get(space_id)
            .cloned()
            .unwrap_or(ArtifactsLoad::Loading)
    }

    pub(crate) fn snapshot(&self, space_id: &str) -> Option<&Arc<ArtifactsSnapshot>> {
        self.loads.get(space_id).and_then(ArtifactsLoad::snapshot)
    }

    /// Replace a Space's state; whether anything changed. A transient
    /// failure keeps the last good snapshot (the UI stays stale, not blank).
    pub(crate) fn set(&mut self, space_id: &str, load: ArtifactsLoad) -> bool {
        if matches!(load, ArtifactsLoad::Failed(_)) && self.snapshot(space_id).is_some() {
            return false;
        }
        if self
            .loads
            .get(space_id)
            .is_some_and(|old| old.same_as(&load))
        {
            return false;
        }
        self.loads.insert(space_id.to_owned(), load);
        self.rebuild_index();
        true
    }

    /// Drop Spaces no longer watched; whether anything was dropped.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) -> bool {
        let before = self.loads.len();
        self.loads.retain(|space_id, _| keep(space_id));
        let changed = before != self.loads.len();
        if changed {
            self.rebuild_index();
        }
        changed
    }

    /// Order-independent hash of every Space's load (kind and snapshot
    /// identity), for views that gate on "did any snapshot change".
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut total = 0u64;
        for (space_id, load) in &self.loads {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            space_id.hash(&mut h);
            match load {
                ArtifactsLoad::Loading => 0u8.hash(&mut h),
                ArtifactsLoad::Ready(snapshot) => {
                    1u8.hash(&mut h);
                    (std::sync::Arc::as_ptr(snapshot) as usize).hash(&mut h);
                }
                ArtifactsLoad::Unsupported => 2u8.hash(&mut h),
                ArtifactsLoad::Failed(reason) => (3u8, reason).hash(&mut h),
            }
            total ^= h.finish();
        }
        total
    }

    pub(crate) fn space_ids(&self) -> impl Iterator<Item = &str> {
        self.loads.keys().map(String::as_str)
    }

    fn all(&self) -> impl Iterator<Item = &ArtifactView> {
        self.loads
            .values()
            .filter_map(ArtifactsLoad::snapshot)
            .flat_map(|snapshot| snapshot.artifacts.iter())
    }

    /// Rebuild once per snapshot change, keeping transcript lookups constant-time.
    fn rebuild_index(&mut self) {
        self.ids.clear();
        self.paths.clear();
        for (space, load) in &self.loads {
            let Some(snapshot) = load.snapshot() else {
                continue;
            };
            for (index, artifact) in snapshot.artifacts.iter().enumerate() {
                self.ids
                    .entry(artifact.id.clone())
                    .or_insert_with(|| (space.clone(), index));
                if artifact.archived_at.is_some() {
                    continue;
                }
                for path in [
                    Some(artifact.stored_path.as_str()),
                    artifact.origin_path.as_deref(),
                    artifact.preview_path.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    self.paths
                        .entry(normalize(path))
                        .or_default()
                        .push((space.clone(), index));
                }
            }
        }
    }

    fn indexed(&self, location: &(String, usize)) -> Option<&ArtifactView> {
        self.snapshot(&location.0)?.artifacts.get(location.1)
    }

    /// An artifact by id, in any watched Space.
    pub(crate) fn artifact(&self, artifact_id: &str) -> Option<&ArtifactView> {
        self.indexed(self.ids.get(artifact_id)?)
    }

    /// A live artifact by stored, origin, or preview path; the named Space wins.
    pub(crate) fn artifact_by_path(
        &self,
        path: &str,
        cwd: Option<&str>,
        space_id: Option<&str>,
    ) -> Option<&ArtifactView> {
        let candidates: Vec<_> = candidate_paths(path, cwd)
            .into_iter()
            .map(|path| normalize(&path))
            .collect();
        let locations = || {
            candidates
                .iter()
                .filter_map(|path| self.paths.get(path))
                .flatten()
        };
        space_id
            .and_then(|space| locations().find(|location| location.0 == space))
            .and_then(|location| self.indexed(location))
            .or_else(|| locations().find_map(|location| self.indexed(location)))
    }

    /// The Details list of one work item: the pinned plan first, then the
    /// rest newest first (`updatedAt`), archived artifacts excluded.
    pub(crate) fn for_work_item(&self, space_id: &str, work_item_id: &str) -> Vec<&ArtifactView> {
        let Some(snapshot) = self.snapshot(space_id) else {
            return Vec::new();
        };
        details_order(snapshot.for_work_item(work_item_id))
    }

    /// Live artifacts of a work item (the Outline row's count).
    pub(crate) fn count_for_work_item(&self, space_id: &str, work_item_id: &str) -> usize {
        self.snapshot(space_id)
            .map(|snapshot| snapshot.for_work_item(work_item_id).len())
            .unwrap_or(0)
    }

    /// The pinned plan of a work item.
    pub(crate) fn plan_of(&self, space_id: &str, work_item_id: &str) -> Option<&ArtifactView> {
        self.snapshot(space_id)?.plan_of(work_item_id)
    }
}

/// Pinned plan first, then `updatedAt` descending, then title for a stable
/// order. Pure.
pub(crate) fn details_order(mut artifacts: Vec<&ArtifactView>) -> Vec<&ArtifactView> {
    artifacts.sort_by(|a, b| {
        b.pinned_as_plan
            .cmp(&a.pinned_as_plan)
            .then(b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
    artifacts
}

fn candidate_paths(path: &str, cwd: Option<&str>) -> Vec<String> {
    let path = path.trim();
    let mut out = vec![path.to_owned()];
    if !path.starts_with('/')
        && let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty())
    {
        out.push(format!("{}/{}", cwd.trim_end_matches('/'), path));
    }
    out
}

/// Lexical path equality: `.` segments and repeated or trailing slashes do
/// not matter; `..` pops a segment.
fn same_path(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}
