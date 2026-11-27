//! Environment variables under their standalone `MAST_*` names.
//!
//! Legacy `ZERON_*` environment variables are not read as fallbacks.

use std::env::VarError;
use std::ffi::OsString;

/// Read an environment variable without legacy `ZERON_*` fallback.
pub fn var(name: &str) -> Result<String, VarError> {
    std::env::var(name)
}

/// Read an OS environment variable without legacy `ZERON_*` fallback.
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Select Mast's home-relative storage directory without adopting legacy directories.
pub fn home_storage_dir(
    home: &std::path::Path,
    development: bool,
) -> (std::path::PathBuf, &'static str) {
    assert_test_home_isolated(home);
    let current = if development { ".mast-dev" } else { ".mast" };
    (home.join(current), "default Mast directory")
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
    fn home_storage_selection_always_uses_mast_directory() {
        for (development, current, legacy) in [
            (false, ".mast", ".zeron"),
            (true, ".mast-dev", ".zeron-dev"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let expected = home.path().join(current);
            // Empty home selects Mast root.
            assert_eq!(
                home_storage_dir(home.path(), development),
                (expected.clone(), "default Mast directory")
            );
            // Legacy-only home still selects Mast root and leaves legacy untouched.
            let legacy_dir = home.path().join(legacy);
            std::fs::create_dir(&legacy_dir).unwrap();
            std::fs::write(legacy_dir.join("sentinel"), b"keep").unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development),
                (expected.clone(), "default Mast directory")
            );
            assert_eq!(std::fs::read(legacy_dir.join("sentinel")).unwrap(), b"keep");
            assert!(!expected.exists());
            // Both-present home selects Mast root and leaves legacy untouched.
            std::fs::create_dir(&expected).unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development),
                (expected.clone(), "default Mast directory")
            );
            assert_eq!(std::fs::read(legacy_dir.join("sentinel")).unwrap(), b"keep");
            // Native-only home selects Mast root.
            std::fs::remove_dir_all(&legacy_dir).unwrap();
            assert_eq!(
                home_storage_dir(home.path(), development),
                (expected, "default Mast directory")
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
    fn reads_mast_name_when_both_are_set() {
        unsafe {
            std::env::set_var("MAST_ENV_TEST_BOTH", "new");
            std::env::set_var("ZERON_ENV_TEST_BOTH", "old");
        }
        assert_eq!(var("MAST_ENV_TEST_BOTH").unwrap(), "new");
        assert_eq!(var_os("MAST_ENV_TEST_BOTH").unwrap(), "new");
    }

    #[test]
    fn zeron_prefixed_fallback_is_ignored() {
        unsafe {
            std::env::remove_var("MAST_ENV_TEST_OLD_ONLY");
            std::env::set_var("ZERON_ENV_TEST_OLD_ONLY", "old");
        }
        assert_eq!(var("MAST_ENV_TEST_OLD_ONLY"), Err(VarError::NotPresent));
        assert!(var_os("MAST_ENV_TEST_OLD_ONLY").is_none());
    }

    #[test]
    fn neither_name_is_not_present() {
        assert_eq!(var("MAST_ENV_TEST_NEITHER"), Err(VarError::NotPresent));
        assert!(var_os("MAST_ENV_TEST_NEITHER").is_none());
        assert_eq!(var("HOME_NOT_SET_ANYWHERE_X"), Err(VarError::NotPresent));
    }
}
