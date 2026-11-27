//! Application storage paths. Provider credentials keep their own locations.

use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    resolve_data_dir(|name| std::env::var_os(name)).0
}

/// Resolve the configured storage location and the reason it was selected.
pub fn data_dir_read_only() -> (PathBuf, &'static str) {
    resolve_data_dir(|name| std::env::var_os(name))
}

fn resolve_data_dir(mut env: impl FnMut(&str) -> Option<OsString>) -> (PathBuf, &'static str) {
    if let Some(dir) = env("MAST_DATA_DIR").filter(|value| !value.is_empty()) {
        return (PathBuf::from(dir), "environment override");
    }
    let home = PathBuf::from(env("HOME").expect("HOME not set"));
    mast_proto::env::home_storage_dir(&home, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_ui::settings::UiSettings;
    use mast_ui::ui_style::UiStyleId;

    fn resolve(vars: &[(&str, &str)]) -> (PathBuf, &'static str) {
        resolve_data_dir(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        })
    }

    #[test]
    fn explicit_mast_data_dir_overrides_home_and_ignores_legacy_env() {
        assert_eq!(
            resolve(&[("MAST_DATA_DIR", "custom data")]),
            (PathBuf::from("custom data"), "environment override")
        );
        assert_eq!(
            resolve(&[("MAST_DATA_DIR", "current"), ("ZERON_DATA_DIR", "legacy")]),
            (PathBuf::from("current"), "environment override")
        );

        let home = tempfile::tempdir().unwrap();
        let home_str = home.path().to_str().unwrap();
        assert_eq!(
            resolve(&[("HOME", home_str), ("ZERON_DATA_DIR", "legacy")]),
            (home.path().join(".mast"), "default Mast directory")
        );
        assert_eq!(
            resolve(&[("HOME", home_str), ("MAST_DATA_DIR", "")]),
            (home.path().join(".mast"), "default Mast directory")
        );
        assert_eq!(
            resolve(&[("HOME", home_str)]),
            (home.path().join(".mast"), "default Mast directory")
        );
    }

    #[test]
    fn native_only_zeron_only_and_both_present_homes_select_mast_root_without_touching_legacy() {
        // 1. Native-only home: loads native Mast settings from .mast.
        let native_home = tempfile::tempdir().unwrap();
        let native_mast = native_home.path().join(".mast");
        std::fs::create_dir(&native_mast).unwrap();
        std::fs::write(
            UiSettings::path(&native_mast),
            r#"{"uiStyle": "industrial", "openWebLinksInMast": false}"#,
        )
        .unwrap();
        let resolved = resolve_data_dir(|name| {
            (name == "HOME").then(|| native_home.path().as_os_str().to_owned())
        });
        assert_eq!(resolved, (native_mast.clone(), "default Mast directory"));
        let settings = UiSettings::load(&resolved.0);
        assert_eq!(settings.ui_style, UiStyleId::Industrial);
        assert!(!settings.open_web_links_in_mast);

        // 2. Zeron-only home (plus pre-rename .comet-native): resolves to .mast,
        // never adopts or mutates legacy dirs, and loads initial Mast defaults.
        let legacy_home = tempfile::tempdir().unwrap();
        let legacy_zeron = legacy_home.path().join(".zeron");
        let legacy_comet = legacy_home.path().join(".comet-native");
        std::fs::create_dir(&legacy_zeron).unwrap();
        std::fs::create_dir(&legacy_comet).unwrap();
        let legacy_settings = r#"{"uiStyle": "horizon", "openWebLinksInZeron": false}"#;
        std::fs::write(UiSettings::path(&legacy_zeron), legacy_settings).unwrap();
        std::fs::write(legacy_comet.join("marker"), b"comet-legacy").unwrap();
        let resolved = resolve_data_dir(|name| {
            (name == "HOME").then(|| legacy_home.path().as_os_str().to_owned())
        });
        assert_eq!(
            resolved,
            (legacy_home.path().join(".mast"), "default Mast directory")
        );
        assert!(!resolved.0.exists());
        let settings = UiSettings::load(&resolved.0);
        assert_eq!(settings.ui_style, UiStyleId::Default);
        assert!(settings.open_web_links_in_mast);
        assert_eq!(
            std::fs::read_to_string(UiSettings::path(&legacy_zeron)).unwrap(),
            legacy_settings
        );
        assert_eq!(
            std::fs::read(legacy_comet.join("marker")).unwrap(),
            b"comet-legacy"
        );

        // 3. Both-present home: selects .mast, loads Mast theme, leaves .zeron untouched.
        let both_home = tempfile::tempdir().unwrap();
        let both_mast = both_home.path().join(".mast");
        let both_zeron = both_home.path().join(".zeron");
        std::fs::create_dir(&both_mast).unwrap();
        std::fs::create_dir(&both_zeron).unwrap();
        std::fs::write(UiSettings::path(&both_mast), r#"{"uiStyle": "industrial"}"#).unwrap();
        std::fs::write(UiSettings::path(&both_zeron), legacy_settings).unwrap();
        let resolved = resolve_data_dir(|name| {
            (name == "HOME").then(|| both_home.path().as_os_str().to_owned())
        });
        assert_eq!(resolved, (both_mast.clone(), "default Mast directory"));
        let settings = UiSettings::load(&resolved.0);
        assert_eq!(settings.ui_style, UiStyleId::Industrial);
        assert_eq!(
            std::fs::read_to_string(UiSettings::path(&both_zeron)).unwrap(),
            legacy_settings
        );
    }

    #[tokio::test]
    async fn doctor_reports_mast_root_across_native_legacy_and_both_homes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        for (create_mast, create_zeron) in [(true, false), (false, true), (true, true)] {
            let home = tempfile::tempdir().unwrap();
            let mast_dir = home.path().join(".mast");
            let zeron_dir = home.path().join(".zeron");
            if create_mast {
                std::fs::create_dir(&mast_dir).unwrap();
            }
            if create_zeron {
                std::fs::create_dir(&zeron_dir).unwrap();
                std::fs::write(zeron_dir.join("sentinel"), b"untouched").unwrap();
            }

            let (data_dir, reason) = resolve_data_dir(|name| {
                (name == "HOME").then(|| home.path().as_os_str().to_owned())
            });
            assert_eq!(data_dir, mast_dir);
            assert_eq!(reason, "default Mast directory");

            let report = crate::doctor::inspect(data_dir, reason, port).await;
            let json = serde_json::to_value(&report).unwrap();
            let checks = json["checks"].as_array().unwrap();
            let data_check = checks
                .iter()
                .find(|check| check["id"] == "data directory")
                .expect("data directory check present");
            let message = data_check["message"].as_str().unwrap();
            assert!(
                message.starts_with(&format!("{} (", mast_dir.display())),
                "expected Mast root in doctor report, got: {message}"
            );
            assert!(
                message.ends_with("; selected by default Mast directory"),
                "expected default Mast directory reason, got: {message}"
            );
            assert!(!message.contains(".zeron"), "unexpected legacy root: {message}");

            if !create_mast {
                assert!(!mast_dir.exists(), "doctor must stay read-only");
            }
            if create_zeron {
                assert_eq!(
                    std::fs::read(zeron_dir.join("sentinel")).unwrap(),
                    b"untouched"
                );
            }
        }
    }
}
