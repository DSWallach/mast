//! Environment variables under their Mast names, with the old `ZERON_*`
//! spelling still honoured so existing shells, launchd/systemd units, `~/.zeron/env`
//! files and scripts keep working.
//!
//! Callers pass the new name (`MAST_DATA_DIR`). The new name wins when both are
//! set. Values written for other programs (child process environments) that
//! user scripts read are exported under both names by the code that spawns them.

use std::env::VarError;
use std::ffi::OsString;

const NEW_PREFIX: &str = "MAST_";
const OLD_PREFIX: &str = "ZERON_";

/// The pre-rename spelling of a `MAST_*` variable (`MAST_X` -> `ZERON_X`).
/// `None` when `name` is not a `MAST_*` name.
pub fn legacy_name(name: &str) -> Option<String> {
    name.strip_prefix(NEW_PREFIX)
        .map(|rest| format!("{OLD_PREFIX}{rest}"))
}

/// `std::env::var`, falling back to the old `ZERON_*` name when the `MAST_*`
/// variable is not set.
pub fn var(name: &str) -> Result<String, VarError> {
    match std::env::var(name) {
        Err(VarError::NotPresent) => match legacy_name(name) {
            Some(old) => std::env::var(old),
            None => Err(VarError::NotPresent),
        },
        found => found,
    }
}

/// `std::env::var_os`, falling back to the old `ZERON_*` name.
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| legacy_name(name).and_then(std::env::var_os))
}

/// Select a home-relative storage directory without moving an existing one.
pub fn home_storage_dir(
    home: &std::path::Path,
    development: bool,
) -> (std::path::PathBuf, &'static str) {
    assert_test_home_isolated(home);
    let (current, legacy) = if development {
        (".mast-dev", ".zeron-dev")
    } else {
        (".mast", ".zeron")
    };
    let current = home.join(current);
    let legacy = home.join(legacy);
    if legacy.exists() && !current.exists() {
        (legacy, "existing legacy directory")
    } else {
        (current, "default Mast directory")
    }
}

/// Reject a test process resolving a home-relative default against its original home.
/// The test initializer sets the marker before changing HOME; ordinary application
/// processes never set it, including in release builds.
pub fn assert_test_home_isolated(home: &std::path::Path) {
    if let Some(original) = std::env::var_os("MAST_TEST_ORIGINAL_HOME") {
        assert!(
            !home.starts_with(std::path::Path::new(&original)),
            "test attempted to use its original home: {}",
            home.display()
        );
        let original =
            std::fs::canonicalize(&original).unwrap_or_else(|_| std::path::PathBuf::from(original));
        let resolved = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        assert!(
            !resolved.starts_with(original),
            "test attempted to use its original home: {}",
            home.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_storage_selection_preserves_existing_legacy_directories() {
        for (development, current, legacy) in [
            (false, ".mast", ".zeron"),
            (true, ".mast-dev", ".zeron-dev"),
        ] {
            let home = tempfile::tempdir().unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development).0,
                home.path().join(current)
            );
            std::fs::create_dir(home.path().join(legacy)).unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development).0,
                home.path().join(legacy)
            );
            std::fs::create_dir(home.path().join(current)).unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development).0,
                home.path().join(current)
            );
            std::fs::remove_dir(home.path().join(legacy)).unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development).0,
                home.path().join(current)
            );
        }
    }

    #[test]
    fn test_process_home_is_isolated() {
        let original = std::env::var_os("MAST_TEST_ORIGINAL_HOME").unwrap();
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_ne!(home, std::path::Path::new(&original));
        assert_eq!(
            std::env::var_os("CLAUDE_CONFIG_DIR"),
            Some(home.join(".claude").into_os_string())
        );
        assert_eq!(
            std::env::var_os("CODEX_HOME"),
            Some(home.join(".codex").into_os_string())
        );
        assert_eq!(
            std::env::var_os("MAST_DATA_DIR"),
            Some(home.join(".zeron").into_os_string())
        );
        assert_test_home_isolated(&home);
        assert!(
            std::panic::catch_unwind(|| assert_test_home_isolated(std::path::Path::new(&original)))
                .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| {
                assert_test_home_isolated(&std::path::PathBuf::from(&original).join(".zeron"));
            })
            .is_err()
        );
    }

    // Each test uses its own variable pair so parallel tests never collide.
    #[test]
    fn the_new_name_wins_over_the_old_one() {
        unsafe {
            std::env::set_var("MAST_ENV_TEST_BOTH", "new");
            std::env::set_var("ZERON_ENV_TEST_BOTH", "old");
        }
        assert_eq!(var("MAST_ENV_TEST_BOTH").unwrap(), "new");
        assert_eq!(var_os("MAST_ENV_TEST_BOTH").unwrap(), "new");
    }

    #[test]
    fn the_old_name_is_still_read() {
        unsafe { std::env::set_var("ZERON_ENV_TEST_OLD_ONLY", "old") };
        assert_eq!(var("MAST_ENV_TEST_OLD_ONLY").unwrap(), "old");
        assert_eq!(var_os("MAST_ENV_TEST_OLD_ONLY").unwrap(), "old");
    }

    #[test]
    fn neither_name_is_not_present() {
        assert_eq!(var("MAST_ENV_TEST_NEITHER"), Err(VarError::NotPresent));
        assert!(var_os("MAST_ENV_TEST_NEITHER").is_none());
    }

    #[test]
    fn legacy_name_maps_only_mast_names() {
        assert_eq!(
            legacy_name("MAST_DATA_DIR").as_deref(),
            Some("ZERON_DATA_DIR")
        );
        assert_eq!(legacy_name("HOME"), None);
        assert_eq!(var("HOME_NOT_SET_ANYWHERE_X"), Err(VarError::NotPresent));
    }
}
