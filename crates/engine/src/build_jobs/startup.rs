//! Opt-in startup of the build queue. Disabled unless `<data dir>/build-queue.json` says
//! `"enabled": true`; no remote host is ever contacted for a profile that is not configured.
//!
//! ```json
//! {"enabled": true, "ssh": "desktop", "script": "/abs/path/scripts/remote-cargo.sh"}
//! ```
//!
//! Ownership: one engine per data dir holds an exclusive OS lock on `build-queue.lock` for the
//! life of the process, so a second engine on the same profile gets "unavailable". The lock is
//! local; it does NOT stop a second *device* from using the same desktop. The desktop's own
//! `remote-cargo.sh --admit` locks (slot, device, quiet-window) are what keep two owners' jobs
//! from sharing a slot, and a lost connection leaves the job to recovery (`recover`) rather than
//! to this lock.

use super::{BuildJobs, BuildJobsConfig, HostProfile, ProcessTransport, WakeSink};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildQueueSettings {
    #[serde(default)]
    pub enabled: bool,
    /// The ssh alias of the build desktop (never a raw address).
    pub ssh: Option<String>,
    /// Absolute path of `scripts/remote-cargo.sh`.
    pub script: Option<PathBuf>,
}

static OWNER_LOCKS: OnceLock<Mutex<Vec<std::fs::File>>> = OnceLock::new();

/// `Ok(None)`: not configured (or disabled). `Err`: configured but unusable; the tools then
/// answer "build queue is unavailable" and the reason is logged.
pub fn open(data_dir: &Path, wake: Arc<dyn WakeSink>) -> Result<Option<BuildJobs>, String> {
    let path = data_dir.join("build-queue.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let settings: BuildQueueSettings = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if !settings.enabled {
        return Ok(None);
    }
    let ssh = settings.ssh.filter(|s| !s.is_empty()).ok_or("build-queue.json: ssh is required")?;
    let script = settings.script.filter(|p| p.is_absolute() && p.is_file()).ok_or("build-queue.json: script must be an existing absolute path")?;
    let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false)
        .open(data_dir.join("build-queue.lock")).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|_| "another engine owns this profile's build queue".to_string())?;
    let cfg = BuildJobsConfig::new(data_dir.to_path_buf(), vec![HostProfile::desktop(&ssh)]);
    let jobs = BuildJobs::builder(cfg)
        .transport(Arc::new(ProcessTransport::new(script)))
        .wake(wake)
        .open()
        .map_err(|e| e.to_string())?;
    jobs.recover().map_err(|e| e.to_string())?;
    OWNER_LOCKS.get_or_init(Default::default).lock().unwrap().push(lock);
    Ok(Some(jobs))
}
