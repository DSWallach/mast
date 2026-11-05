//! Application storage paths. Provider credentials keep their own locations.

use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    resolve_data_dir(|name| std::env::var_os(name), true).0
}

/// Resolve the configured location without performing the legacy migration.
pub fn data_dir_read_only() -> (PathBuf, &'static str) {
    resolve_data_dir(|name| std::env::var_os(name), false)
}

fn resolve_data_dir(
    mut env: impl FnMut(&str) -> Option<OsString>,
    migrate: bool,
) -> (PathBuf, &'static str) {
    if let Some(dir) = env("MAST_DATA_DIR").or_else(|| env("ZERON_DATA_DIR")) {
        return (PathBuf::from(dir), "environment override");
    }
    #[cfg(windows)]
    {
        // Explorer does not set HOME. Do not let a shell-specific HOME select
        // a different workspace from a desktop launch, or migrate credentials
        // between Unix-style and native Windows directories implicitly.
        let local = env("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env("USERPROFILE")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join("AppData").join("Local"))
            })
            .expect("LOCALAPPDATA and USERPROFILE not set; set MAST_DATA_DIR");
        (local.join("Zeron"), "platform default")
    }
    #[cfg(not(windows))]
    {
        let home = PathBuf::from(env("HOME").expect("HOME not set"));
        mast_proto::env::assert_test_home_isolated(&home);
        let dir = home.join(".zeron");
        // One-shot 0.2.0 migration: adopt the pre-rename data dir.
        if migrate && !dir.exists() && !home.join(".mast").exists() {
            let old = home.join(".comet-native");
            if old.exists() && std::fs::rename(&old, &dir).is_ok() {
                eprintln!("migrated data dir {} -> {}", old.display(), dir.display());
            }
        }
        mast_proto::env::home_storage_dir(&home, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> PathBuf {
        resolve_data_dir(
            |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.into())
            },
            true,
        )
        .0
    }

    #[test]
    fn explicit_data_dir_needs_no_home() {
        assert_eq!(
            resolve(&[("MAST_DATA_DIR", "custom data")]),
            PathBuf::from("custom data")
        );
        assert_eq!(
            resolve(&[("ZERON_DATA_DIR", "legacy")]),
            PathBuf::from("legacy")
        );
        assert_eq!(
            resolve(&[("MAST_DATA_DIR", "current"), ("ZERON_DATA_DIR", "legacy")]),
            PathBuf::from("current")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn read_only_resolution_does_not_migrate_legacy_directory() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(".comet-native");
        std::fs::create_dir(&old).unwrap();
        let path = resolve_data_dir(
            |name| (name == "HOME").then(|| home.path().as_os_str().to_owned()),
            false,
        );
        assert_eq!(path.0, home.path().join(".mast"));
        assert!(old.exists());
        assert!(!path.0.exists());
    }

    #[cfg(windows)]
    #[test]
    fn explorer_launch_without_home_uses_local_app_data() {
        assert_eq!(
            resolve(&[("LOCALAPPDATA", r"C:\Users\Test User\AppData\Local")]),
            PathBuf::from(r"C:\Users\Test User\AppData\Local\Zeron"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_profile_fallback_handles_unicode_and_apostrophes() {
        assert_eq!(
            resolve(&[("USERPROFILE", r"C:\Users\O'Brien 日本語")]),
            PathBuf::from(r"C:\Users\O'Brien 日本語\AppData\Local\Zeron"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_does_not_depend_on_shell_home() {
        assert_eq!(
            resolve(&[("HOME", r"D:\msys-home"), ("LOCALAPPDATA", r"C:\Local")]),
            PathBuf::from(r"C:\Local\Zeron"),
        );
    }
}
