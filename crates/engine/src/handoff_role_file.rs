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
}

#[derive(Default)]
pub(crate) struct HandoffRoleFile {
    path: Option<PathBuf>,
    completed: HashSet<String>,
}

impl HandoffRoleFile {
    pub(crate) fn load<T: DeserializeOwned>(&mut self, path: PathBuf) -> Option<T> {
        let bytes = std::fs::read(&path).ok();
        self.path = Some(path);
        self.completed.clear();
        let bytes = bytes?;
        if let Ok(snapshot) = serde_json::from_slice::<RoleSnapshot<T, HashSet<String>>>(&bytes) {
            self.completed = snapshot.completed_handoffs;
            Some(snapshot.roles)
        } else {
            // Earlier files contain just the grants or assistant designation.
            serde_json::from_slice(&bytes).ok()
        }
    }

    pub(crate) fn contains(&self, successor: &str) -> bool {
        self.completed.contains(successor)
    }

    pub(crate) fn save(&self, roles: impl Serialize) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            &mut staged,
            &RoleSnapshot {
                roles,
                completed_handoffs: &self.completed,
            },
        )
        .map_err(io::Error::other)?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|err| err.error)?;
        Ok(())
    }

    /// The caller updates its in-memory role only after this write succeeds.
    pub(crate) fn complete(&mut self, successor: &str, roles: impl Serialize) -> io::Result<()> {
        self.completed.insert(successor.to_owned());
        if let Err(error) = self.save(roles) {
            self.completed.remove(successor);
            return Err(error);
        }
        Ok(())
    }
}
