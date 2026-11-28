//! Persisted quota cap settings: the cap mode plus the global soft, hard and
//! freeze caps, stored per profile in `quota-settings.json`.
//!
//! The file holds only what the user chooses. Poll intervals and the
//! reservation TTL stay with [`BrokerConfig`]. Nothing here runs a broker:
//! whoever builds one calls [`load`], applies the caps with
//! [`QuotaSettings::apply_to`] before `QuotaBroker::new`, and passes
//! [`QuotaSettings::policy`] to `QuotaBroker::dispatch`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use mast_proto::ProviderQuotaPolicy;
use serde::{Deserialize, Serialize};

use crate::quota_broker::{BrokerConfig, InvalidBrokerConfig};

const FILE_NAME: &str = "quota-settings.json";

/// Also the wire shape of `GetQuotaSettings` and `SetQuotaSettings`. Every
/// field is required, so a partial save can never silently reset the cap mode;
/// unknown fields are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSettings {
    pub policy: ProviderQuotaPolicy,
    pub soft_cap: f32,
    pub hard_cap: f32,
    pub freeze_cap: f32,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        let caps = BrokerConfig::default();
        Self {
            policy: ProviderQuotaPolicy::default(),
            soft_cap: caps.soft_cap,
            hard_cap: caps.hard_cap,
            freeze_cap: caps.freeze_cap,
        }
    }
}

impl QuotaSettings {
    /// The broker's own ordering rule: `0 < soft < hard < freeze <= 1`.
    pub fn validate(&self) -> Result<(), InvalidBrokerConfig> {
        let mut config = BrokerConfig::default();
        self.apply_to(&mut config);
        config.validate()
    }

    /// Set the three caps on `config`; everything else is left alone.
    pub fn apply_to(&self, config: &mut BrokerConfig) {
        config.soft_cap = self.soft_cap;
        config.hard_cap = self.hard_cap;
        config.freeze_cap = self.freeze_cap;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Invalid(#[from] InvalidBrokerConfig),
    #[error("could not save quota settings: {0}")]
    Io(#[from] io::Error),
}

fn path(store_root: &Path) -> PathBuf {
    store_root.join(FILE_NAME)
}

/// The saved settings, or the defaults when there is no file. A file that
/// cannot be read, parsed or validated also yields the defaults, with a
/// logged warning; it is left on disk until the next save.
pub fn load(store_root: &Path) -> QuotaSettings {
    let path = path(store_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), %error, "quota settings unreadable; using defaults");
            }
            return QuotaSettings::default();
        }
    };
    let parsed = serde_json::from_slice::<QuotaSettings>(&bytes)
        .map_err(|error| error.to_string())
        .and_then(|settings| match settings.validate() {
            Ok(()) => Ok(settings),
            Err(error) => Err(error.to_string()),
        });
    parsed.unwrap_or_else(|error| {
        tracing::warn!(path = %path.display(), %error, "quota settings invalid; using defaults");
        QuotaSettings::default()
    })
}

/// Validate, then replace the file atomically. Invalid caps write nothing.
pub fn save(store_root: &Path, settings: &QuotaSettings) -> Result<(), SaveError> {
    settings.validate()?;
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::from)?;
    // A unique temp file per save, so concurrent saves cannot interleave.
    let mut file = tempfile::NamedTempFile::new_in(store_root)?;
    file.write_all(&json)?;
    file.as_file().sync_all()?;
    file.persist(path(store_root))
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> QuotaSettings {
        QuotaSettings {
            policy: ProviderQuotaPolicy::PinnedFreeze,
            soft_cap: 0.60,
            hard_cap: 0.70,
            freeze_cap: 0.80,
        }
    }

    #[test]
    fn defaults_follow_the_broker_when_there_is_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = load(dir.path());
        let broker = BrokerConfig::default();
        assert_eq!(settings.policy, ProviderQuotaPolicy::BalancedFailover);
        assert_eq!(
            (settings.soft_cap, settings.hard_cap, settings.freeze_cap),
            (broker.soft_cap, broker.hard_cap, broker.freeze_cap)
        );
        assert!(!path(dir.path()).exists(), "loading writes nothing");
    }

    #[test]
    fn save_round_trips_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &custom()).unwrap();
        assert_eq!(load(dir.path()), custom());
        save(dir.path(), &QuotaSettings::default()).unwrap();
        assert_eq!(load(dir.path()), QuotaSettings::default());
        let files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files, 1, "no temp file is left behind");
    }

    #[test]
    fn file_and_wire_use_camel_case() {
        let value = serde_json::to_value(custom()).unwrap();
        assert_eq!(value["policy"], "pinnedFreeze");
        for key in ["softCap", "hardCap", "freezeCap"] {
            assert!(value[key].is_number(), "{key}");
        }
    }

    #[test]
    fn save_rejects_bad_caps_and_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &custom()).unwrap();
        for (soft_cap, hard_cap, freeze_cap) in [
            (0.0, 0.7, 0.8),
            (0.7, 0.7, 0.8),
            (0.6, 0.8, 0.8),
            (0.8, 0.7, 0.9),
            (0.6, 0.7, 1.01),
            (f32::NAN, 0.7, 0.8),
        ] {
            let bad = QuotaSettings {
                soft_cap,
                hard_cap,
                freeze_cap,
                ..custom()
            };
            assert!(matches!(save(dir.path(), &bad), Err(SaveError::Invalid(_))));
        }
        assert_eq!(load(dir.path()), custom());
    }

    #[test]
    fn corrupt_partial_or_invalid_files_load_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            "{not json",
            "",
            "[]",
            r#"{"policy":"pinnedFreeze"}"#,
            r#"{"policy":"somethingElse","softCap":0.6,"hardCap":0.7,"freezeCap":0.8}"#,
            r#"{"policy":"pinnedFreeze","softCap":0.9,"hardCap":0.7,"freezeCap":0.8}"#,
        ] {
            std::fs::write(path(dir.path()), text).unwrap();
            assert_eq!(load(dir.path()), QuotaSettings::default(), "{text}");
            assert_eq!(std::fs::read_to_string(path(dir.path())).unwrap(), text);
        }
        save(dir.path(), &custom()).unwrap();
        assert_eq!(load(dir.path()), custom(), "saving recovers");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"policy":"pinnedFreeze","softCap":0.6,"hardCap":0.7,"freezeCap":0.8,"later":1}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), custom());
    }

    #[test]
    fn apply_to_sets_only_the_caps() {
        let mut config = BrokerConfig::default();
        custom().apply_to(&mut config);
        assert_eq!(
            (config.soft_cap, config.hard_cap, config.freeze_cap),
            (0.60, 0.70, 0.80)
        );
        let defaults = BrokerConfig::default();
        assert_eq!(config.poll_interval, defaults.poll_interval);
        assert_eq!(config.agy_poll_interval, defaults.agy_poll_interval);
        assert_eq!(config.reservation_ttl, defaults.reservation_ttl);
        config.validate().unwrap();
    }
}
