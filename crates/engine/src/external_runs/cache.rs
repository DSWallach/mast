//! The observed-state cache file: device-local, never synced. It seeds the
//! in-memory state at start and holds the wake dedupe set. It restores facts
//! (an exit seen, the last state a transition is measured from), never
//! freshness: every run reads "not yet observed" until a tick sees it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use mast_proto::{BoxFact, RunObservation, RunState};
use serde::{Deserialize, Serialize};

pub const CACHE_VERSION: u32 = 1;

/// Ended runs and absent boxes are dropped after this long.
pub const PRUNE_AFTER_S: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityFact {
    pub running: u32,
    pub total: u32,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapState {
    pub at_cap: bool,
    pub episode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    #[serde(flatten)]
    pub obs: RunObservation,
    /// The last state that was not unknown; transitions are measured from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known: Option<RunState>,
    /// A tick saw the run ended; a later live sighting is a reopen.
    #[serde(default)]
    pub ended_seen: bool,
    #[serde(default)]
    pub episode: u32,
    #[serde(default)]
    pub unknown_fired: bool,
    #[serde(default)]
    pub unknown_spells: u32,
    /// Epoch seconds the run's durable record ended, for pruning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CacheFile {
    pub v: u32,
    pub saved_at: i64,
    pub etag: Option<String>,
    pub list_hash: Option<u64>,
    pub capacity: Option<CapacityFact>,
    pub boxes: BTreeMap<String, BoxFact>,
    pub runs: BTreeMap<String, RunRecord>,
    pub notified: BTreeSet<String>,
    pub capacity_state: BTreeMap<String, CapState>,
}

impl CacheFile {
    /// Equal when nothing but timestamps differ.
    pub fn same_facts(&self, other: &CacheFile) -> bool {
        fn scrubbed(file: &CacheFile) -> CacheFile {
            let mut copy = file.clone();
            copy.saved_at = 0;
            if let Some(capacity) = copy.capacity.as_mut() {
                capacity.at = 0;
            }
            for fact in copy.boxes.values_mut() {
                fact.at = 0;
            }
            for record in copy.runs.values_mut() {
                record.obs.seen_at = 0;
            }
            copy
        }
        scrubbed(self) == scrubbed(other)
    }

    pub fn prune(&mut self, now_s: i64) {
        self.runs.retain(|_, record| {
            record.ended_at.is_none_or(|at| now_s - at <= PRUNE_AFTER_S)
        });
    }
}

/// The file's contents, or a fresh file when it is missing, unreadable or from
/// another version. Never an error.
pub fn load(path: &Path) -> CacheFile {
    let Ok(bytes) = std::fs::read(path) else { return fresh() };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(file) if file.v == CACHE_VERSION => file,
        _ => fresh(),
    }
}

fn fresh() -> CacheFile {
    CacheFile { v: CACHE_VERSION, ..CacheFile::default() }
}

/// Temp file and rename, so a crash leaves the old file or the new one.
pub fn save(path: &Path, file: &CacheFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(file).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(state: RunState) -> RunRecord {
        RunRecord {
            obs: RunObservation {
                state,
                since: 10,
                seen_at: 20,
                why: "why".into(),
                exit_code: None,
                idle_s: Some(3),
            },
            last_known: Some(state),
            ended_seen: false,
            episode: 0,
            unknown_fired: false,
            unknown_spells: 0,
            ended_at: None,
        }
    }

    #[test]
    fn a_file_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache/external-runs-observed.json");
        let mut file = fresh();
        file.etag = Some("W/\"x\"".into());
        file.runs.insert("r1".into(), record(RunState::Failed(2)));
        file.notified.insert("r1|failed|0".into());
        save(&path, &file).unwrap();
        assert_eq!(load(&path), file);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn missing_corrupt_and_other_version_files_are_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("c.json");
        assert_eq!(load(&path), fresh());
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), fresh());
        std::fs::write(&path, br#"{"v":99,"etag":"x"}"#).unwrap();
        assert_eq!(load(&path), fresh());
        std::fs::write(&path, br#"{"v":1,"unknownField":true}"#).unwrap();
        assert_eq!(load(&path).v, 1);
    }

    #[test]
    fn timestamps_alone_are_not_a_change() {
        let mut a = fresh();
        a.runs.insert("r".into(), record(RunState::Running));
        let mut b = a.clone();
        b.saved_at = 99;
        b.runs.get_mut("r").unwrap().obs.seen_at = 999;
        assert!(a.same_facts(&b));
        b.runs.get_mut("r").unwrap().obs.state = RunState::Done;
        assert!(!a.same_facts(&b));
    }

    #[test]
    fn long_ended_runs_are_pruned() {
        let mut file = fresh();
        let mut old = record(RunState::Done);
        old.ended_at = Some(0);
        let mut recent = record(RunState::Done);
        recent.ended_at = Some(PRUNE_AFTER_S);
        file.runs.insert("old".into(), old);
        file.runs.insert("recent".into(), recent);
        file.runs.insert("live".into(), record(RunState::Running));
        file.prune(PRUNE_AFTER_S + 1);
        assert_eq!(file.runs.keys().cloned().collect::<Vec<_>>(), ["live", "recent"]);
    }
}
