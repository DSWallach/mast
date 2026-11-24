//! Store each role change and its hand-off receipt in one atomic replacement.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleSnapshot<T, R> {
    roles: T,
    completed_handoffs: R,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
}

#[derive(Default)]
pub(crate) struct HandoffRoleFile {
    path: Option<PathBuf>,
    completed: HashSet<String>,
}

impl HandoffRoleFile {
    pub(crate) fn load<T: DeserializeOwned>(&mut self, path: PathBuf) -> Option<T> {
        self.load_with_generation(path).map(|(roles, _)| roles)
    }

    pub(crate) fn load_with_generation<T: DeserializeOwned>(&mut self, path: PathBuf) -> Option<(T, Option<String>)> {
        let bytes = std::fs::read(&path).ok();
        self.path = Some(path);
        self.completed.clear();
        let bytes = bytes?;
        if let Ok(snapshot) = serde_json::from_slice::<RoleSnapshot<T, HashSet<String>>>(&bytes) {
            self.completed = snapshot.completed_handoffs;
            Some((snapshot.roles, snapshot.generation))
        } else {
            // Earlier files contain just the grants or assistant designation.
            serde_json::from_slice(&bytes).ok().map(|roles| (roles, None))
        }
    }

    pub(crate) fn contains(&self, successor: &str) -> bool {
        self.completed.contains(successor)
    }

    pub(crate) fn save(&self, roles: impl Serialize) -> io::Result<()> {
        self.save_with_generation(roles, None)
    }

    pub(crate) fn save_with_generation(&self, roles: impl Serialize, generation: Option<&str>) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_json(
            path,
            &RoleSnapshot {
                roles,
                completed_handoffs: &self.completed,
                generation: generation.map(str::to_owned),
            },
        )
    }

    /// The caller updates its in-memory role only after this write succeeds.
    pub(crate) fn complete(&mut self, successor: &str, roles: impl Serialize) -> io::Result<()> {
        self.complete_with_generation(successor, roles, None)
    }

    pub(crate) fn complete_with_generation(&mut self, successor: &str, roles: impl Serialize, generation: Option<&str>) -> io::Result<()> {
        self.completed.insert(successor.to_owned());
        if let Err(error) = self.save_with_generation(roles, generation) {
            self.completed.remove(successor);
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut staged, value).map_err(io::Error::other)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_grant_store_round_trips_without_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grants.json");
        let mut store = HandoffRoleFile::default();
        assert!(store.load::<Vec<String>>(path.clone()).is_none());
        store.complete("successor", vec!["grant".to_owned()]).unwrap();
        let mut reopened = HandoffRoleFile::default();
        assert_eq!(reopened.load::<Vec<String>>(path).unwrap(), vec!["grant"]);
        assert!(reopened.contains("successor"));
    }
}
