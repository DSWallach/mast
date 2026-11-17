//! Validation of the filesystem boundary for a driver run.
//!
//! All paths must already exist. Canonicalization is deliberately mandatory: a
//! missing directory or an unresolved symlink is not a safe startup state.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct EnvironmentPaths {
    pub root: PathBuf,
    pub home: PathBuf,
    pub data: PathBuf,
    pub temporary: PathBuf,
}

impl EnvironmentPaths {
    /// Validate paths against the account home captured before clearing the environment.
    pub fn validate(
        root: &Path,
        home: &Path,
        data: &Path,
        temporary: &Path,
        account_home: &Path,
    ) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let home = home.canonicalize()?;
        let data = data.canonicalize()?;
        let temporary = temporary.canonicalize()?;
        let account_home = account_home.canonicalize()?;
        let invalid = |message| io::Error::new(io::ErrorKind::PermissionDenied, message);

        if root.starts_with(&account_home) || account_home.starts_with(&root) {
            return Err(invalid("scratch root overlaps account home"));
        }
        for path in [&home, &data, &temporary] {
            if path == &root || !path.starts_with(&root) || path.starts_with(&account_home) {
                return Err(invalid("environment path is outside scratch root"));
            }
        }
        if home == data
            || home == temporary
            || data == temporary
            || home.starts_with(&data)
            || home.starts_with(&temporary)
            || data.starts_with(&home)
            || data.starts_with(&temporary)
            || temporary.starts_with(&home)
            || temporary.starts_with(&data)
        {
            return Err(invalid("environment paths overlap"));
        }
        Ok(Self {
            root,
            home,
            data,
            temporary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_account_paths_and_symlink_aliases() {
        let base =
            std::env::temp_dir().join(format!("mast-driver-guard-test-{}", std::process::id()));
        fs::create_dir(&base).unwrap();
        let account = base.join("account");
        let root = base.join("scratch");
        for path in [
            &account,
            &root,
            &root.join("home"),
            &root.join("data"),
            &root.join("tmp"),
        ] {
            fs::create_dir(path).unwrap();
        }
        for name in [".zeron", ".mast", ".claude", ".codex"] {
            fs::create_dir(account.join(name)).unwrap();
        }
        let home = root.join("home");
        let data = root.join("data");
        let temporary = root.join("tmp");
        assert!(EnvironmentPaths::validate(&root, &home, &data, &temporary, &account).is_ok());
        for name in [".zeron", ".mast", ".claude", ".codex"] {
            let protected = account.join(name);
            assert!(
                EnvironmentPaths::validate(&root, &home, &protected, &temporary, &account).is_err()
            );
            let alias = root.join(format!("alias-{name}"));
            symlink(&protected, &alias).unwrap();
            assert!(
                EnvironmentPaths::validate(&root, &alias, &data, &temporary, &account).is_err()
            );
        }
        assert!(EnvironmentPaths::validate(&root, &account, &data, &temporary, &account).is_err());
        fs::remove_dir_all(&base).unwrap();
    }
}
