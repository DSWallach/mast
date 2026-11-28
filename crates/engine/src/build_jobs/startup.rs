//! Opt-in startup of the build queue. It is **disabled** unless `<data dir>/build-queue.json`
//! says `"enabled": true` and describes the host completely; nothing about a host is defaulted,
//! and no host is contacted for a profile that is not configured. Engine assembly calls [`open`]
//! once, and every consumer of the queue (agent tools, RPC surfaces) shares that instance.
//!
//! ```json
//! {
//!   "enabled": true,
//!   "checkout": "/home/me/Work/mast",
//!   "host": {
//!     "id": "desktop",
//!     "sshAlias": "desktop",
//!     "repo": "Work/mast",
//!     "checkoutPrefix": "Work/mast-wt-build-",
//!     "targetPrefix": "Work/mast-target-",
//!     "maxRunners": 3,
//!     "slots": [
//!       {"name": "a", "incremental": true},
//!       {"name": "b", "incremental": false},
//!       {"name": "c", "incremental": false}
//!     ],
//!     "startMode": "admit"
//!   }
//! }
//! ```
//!
//! `checkout` is an absolute path of a Mast checkout on this machine; the queue runs its
//! `scripts/remote-cargo.sh`. `repo`, `checkoutPrefix` and `targetPrefix` are relative to the
//! build host's `$HOME`. `startMode` is `"admit"`, `"plain"` or `{"deviceSerial": "<serial>"}`.
//!
//! Slot ownership is enforced by the host, not by this engine: see [`super::remote_executor`].
//! (This machine runs one engine per data dir, which keeps the queue's database single-writer.)

use super::{
    BuildJobs, BuildJobsConfig, HostKind, HostProfile, ProcessTransport, RemoteHost, SlotConfig,
    StartMode, WakeSink, validate_ssh_alias,
};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// The settings file, in the engine's data dir.
pub const SETTINGS_FILE: &str = "build-queue.json";

/// Why this engine has no build queue. The `build_*` tools answer with this text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    #[error("not configured (there is no {})", .0.display())]
    NotConfigured(PathBuf),
    #[error("disabled in {}", .0.display())]
    Disabled(PathBuf),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
    checkout: Option<PathBuf>,
    host: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSettings {
    id: String,
    ssh_alias: String,
    repo: String,
    checkout_prefix: String,
    target_prefix: String,
    max_runners: usize,
    slots: Vec<SlotSettings>,
    start_mode: StartModeSetting,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotSettings {
    name: String,
    incremental: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StartModeSetting {
    Plain,
    Admit,
    DeviceSerial(String),
}

/// A validated profile: the local script to run and the one host it may reach.
#[derive(Debug, Clone)]
pub struct Profile {
    pub script: PathBuf,
    pub host: HostProfile,
}

fn word(value: &str, extra: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
}

fn home_relative(field: &str, value: &str) -> Result<(), String> {
    if word(value, "-./") && !value.starts_with('/') && !value.split('/').any(|p| p == "..") {
        return Ok(());
    }
    Err(format!(
        "host.{field} must be a path relative to the host's $HOME (letters, digits, '_', '-', '.', '/'; no '..')"
    ))
}

fn host_profile(host: HostSettings) -> Result<HostProfile, String> {
    if !word(&host.id, "-.") {
        return Err("host.id must be letters, digits, '_', '-' or '.'".into());
    }
    validate_ssh_alias(&host.ssh_alias)?;
    home_relative("repo", &host.repo)?;
    home_relative("checkoutPrefix", &host.checkout_prefix)?;
    home_relative("targetPrefix", &host.target_prefix)?;
    if host.slots.is_empty() {
        return Err("host.slots must name at least one slot".into());
    }
    let mut names = std::collections::HashSet::new();
    for slot in &host.slots {
        if !word(&slot.name, "-") || !names.insert(slot.name.as_str()) {
            return Err(format!(
                "host.slots names must be unique words, got {:?}",
                slot.name
            ));
        }
    }
    if host.max_runners == 0 || host.max_runners > host.slots.len() {
        return Err(format!(
            "host.maxRunners must be between 1 and the number of slots ({})",
            host.slots.len()
        ));
    }
    let mode = match host.start_mode {
        StartModeSetting::Plain => StartMode::Plain,
        StartModeSetting::Admit => {
            // `remote-cargo.sh --admit` refuses any other cache directory.
            if host.target_prefix != "Work/mast-target-" {
                return Err(
                    "host.targetPrefix must be \"Work/mast-target-\" with startMode \"admit\""
                        .into(),
                );
            }
            StartMode::Admit
        }
        StartModeSetting::DeviceSerial(serial) => {
            if !word(&serial, "-.") {
                return Err("host.startMode.deviceSerial must be a plain serial".into());
            }
            if host.max_runners != 1 {
                return Err("host.maxRunners must be 1 with startMode deviceSerial".into());
            }
            StartMode::DeviceSerial(serial)
        }
    };
    let slots = host
        .slots
        .into_iter()
        .map(|slot| SlotConfig {
            target_dir: Some(format!("{}{}", host.target_prefix, slot.name)),
            name: slot.name,
            incremental: slot.incremental,
        })
        .collect();
    Ok(HostProfile {
        id: host.id,
        kind: HostKind::Remote(RemoteHost {
            ssh: host.ssh_alias,
            repo: host.repo,
            checkout_prefix: host.checkout_prefix,
            target_prefix: host.target_prefix,
            mode,
        }),
        slots,
        max_runners: host.max_runners,
        disk: Default::default(),
    })
}

/// Read and validate `<data dir>/build-queue.json`.
pub fn load(data_dir: &Path) -> Result<Profile, Unavailable> {
    let path = data_dir.join(SETTINGS_FILE);
    let invalid = |why: String| Unavailable::Invalid(format!("{}: {why}", path.display()));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Unavailable::NotConfigured(path));
        }
        Err(error) => return Err(invalid(error.to_string())),
    };
    let settings: Settings = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if !settings.enabled {
        return Err(Unavailable::Disabled(path));
    }
    let checkout = settings
        .checkout
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("checkout must be an absolute path of a Mast checkout".into()))?;
    let script = checkout.join("scripts/remote-cargo.sh");
    let source = std::fs::read_to_string(&script)
        .map_err(|e| invalid(format!("cannot read {}: {e}", script.display())))?;
    if !source.contains("--checkout") || !source.contains("fence") {
        return Err(invalid(format!(
            "{} is too old for the build queue (it needs `start --checkout` and `fence`); update that checkout",
            script.display()
        )));
    }
    let host = settings
        .host
        .ok_or_else(|| invalid("host is required".into()))?;
    let host: HostSettings = serde_json::from_value(host).map_err(|e| invalid(format!("host: {e}")))?;
    let host = host_profile(host).map_err(invalid)?;
    Ok(Profile { script, host })
}

/// Build the queue this data dir configures. Contacts no host: [`BuildJobs::serve`] does that.
pub fn open(data_dir: &Path, wake: Arc<dyn WakeSink>) -> Result<BuildJobs, Unavailable> {
    open_with(data_dir, wake, ProcessTransport::new)
}

/// [`open`] with the process transport built by the caller (tests substitute `ssh` and `$HOME`).
pub(crate) fn open_with(
    data_dir: &Path,
    wake: Arc<dyn WakeSink>,
    transport: impl FnOnce(PathBuf) -> ProcessTransport,
) -> Result<BuildJobs, Unavailable> {
    let profile = load(data_dir)?;
    BuildJobs::builder(BuildJobsConfig::new(data_dir.to_path_buf(), vec![profile.host]))
        .transport(Arc::new(transport(profile.script)))
        .wake(wake)
        .open()
        .map_err(|e| Unavailable::Invalid(e.to_string()))
}
