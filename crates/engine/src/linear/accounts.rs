//! Device-local Linear account labels. Credentials stay in the Keychain.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearAccountLabel {
    pub id: String,
    pub label: String,
}

#[derive(Default)]
pub struct AccountCatalog {
    path: Option<PathBuf>,
    accounts: Vec<LinearAccountLabel>,
    unavailable: bool,
}

impl AccountCatalog {
    pub fn open(path: &Path) -> Result<Self, String> {
        let accounts = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| "Could not read Linear accounts".to_owned())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err("Could not read Linear accounts".into()),
        };
        Ok(Self {
            path: Some(path.to_owned()),
            accounts,
            unavailable: false,
        })
    }

    pub fn unavailable() -> Self {
        Self {
            path: None,
            accounts: Vec::new(),
            unavailable: true,
        }
    }

    pub fn list(&self) -> &[LinearAccountLabel] {
        &self.accounts
    }

    pub fn replace(&mut self, accounts: Vec<LinearAccountLabel>) -> Result<(), String> {
        if self.unavailable {
            return Err("Could not save Linear accounts".into());
        }
        if let Some(path) = &self.path {
            let parent = path.parent().ok_or("Invalid Linear account storage path")?;
            std::fs::create_dir_all(parent).map_err(|_| "Could not save Linear accounts")?;
            let bytes =
                serde_json::to_vec(&accounts).map_err(|_| "Could not save Linear accounts")?;
            let temporary = parent.join(format!(".linear-accounts-{}", uuid::Uuid::new_v4()));
            std::fs::write(&temporary, bytes).map_err(|_| "Could not save Linear accounts")?;
            if std::fs::rename(&temporary, path).is_err() {
                let _ = std::fs::remove_file(temporary);
                return Err("Could not save Linear accounts".into());
            }
        }
        self.accounts = accounts;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_persist_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let mut catalog = AccountCatalog::open(&path).unwrap();
        let expected = vec![LinearAccountLabel {
            id: "personal".into(),
            label: "Personal".into(),
        }];
        catalog.replace(expected.clone()).unwrap();
        assert_eq!(AccountCatalog::open(&path).unwrap().list(), expected);
    }
}
