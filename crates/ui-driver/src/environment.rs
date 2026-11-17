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

/// Directories allocated for a single run. Allocation alone does not establish
/// a sandbox; callers must not start the driver until isolation is verified.
#[derive(Debug)]
pub struct ScratchDirectories {
    pub paths: EnvironmentPaths,
    pub repositories: PathBuf,
    pub shims: PathBuf,
    pub stub_log: PathBuf,
}

impl ScratchDirectories {
    pub fn create(account_home: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/private/tmp/mast-drive.XXXXXX"])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("could not allocate scratch root"));
        }
        let output = String::from_utf8(output.stdout)
            .map_err(|_| io::Error::other("mktemp returned a non-UTF-8 path"))?;
        let root = PathBuf::from(output.trim());
        let home = root.join("home");
        let data = root.join("data");
        let temporary = root.join("tmp");
        let repositories = root.join("repos");
        let shims = root.join("shims");
        for path in [&home, &data, &temporary, &repositories, &shims] {
            std::fs::create_dir(path)?;
        }
        let paths = EnvironmentPaths::validate(&root, &home, &data, &temporary, account_home)?;
        let stub_log = root.join("stub.log");
        std::fs::write(&stub_log, b"")?;
        for program in ["gh", "claude", "codex"] {
            let path = shims.join(program);
            // The log location comes from the cleared child environment, not a
            // caller-supplied command argument.
            std::fs::write(&path, b"#!/bin/sh\nprintf '%s\\n' \"${0##*/}\" >> \"${MAST_DRIVER_STUB_LOG:?}\"\necho 'driver: command disabled' >&2\nexit 127\n")?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            paths,
            repositories,
            shims,
            stub_log,
        })
    }

    /// Write a restrictive profile for sandbox-exec. This does not itself
    /// enter the sandbox or prove that the profile was enforced.
    pub fn write_sandbox_profile(&self, account_home: &Path) -> io::Result<PathBuf> {
        fn literal(path: &Path) -> io::Result<String> {
            let value = path
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 path"))?;
            if value.chars().any(char::is_control) {
                return Err(io::Error::other("control character in sandbox path"));
            }
            Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let root = literal(&self.paths.root)?;
        let account_home = account_home.canonicalize()?;
        let mut profile = format!(
            "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write* (subpath \"{root}\") (literal \"/dev/null\"))\n(deny file-read*\n"
        );
        for name in [
            ".zeron",
            ".mast",
            ".claude",
            ".codex",
            ".config/gh",
            "Library/Keychains",
        ] {
            profile.push_str(&format!(
                "  (subpath \"{}\")\n",
                literal(&account_home.join(name))?
            ));
        }
        profile.push_str(")\n(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\"))\n");
        let path = self.paths.root.join("sandbox.sb");
        std::fs::write(&path, profile)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn creates_scratch_directories_and_fail_closed_stubs() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/private/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let scratch = ScratchDirectories::create(&account).unwrap();
        let profile = fs::read_to_string(scratch.write_sandbox_profile(&account).unwrap()).unwrap();
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains(".claude"));
        for program in ["gh", "claude", "codex"] {
            let status = std::process::Command::new(scratch.shims.join(program))
                .env_clear()
                .env("MAST_DRIVER_STUB_LOG", &scratch.stub_log)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(127));
        }
        assert_eq!(
            fs::read_to_string(&scratch.stub_log).unwrap(),
            "gh\nclaude\ncodex\n"
        );
        fs::remove_dir_all(&scratch.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    #[test]
    fn rejects_account_paths_and_symlink_aliases() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/private/tmp/mast-driver-guard-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let base = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
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
