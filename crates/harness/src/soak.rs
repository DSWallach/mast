//! Soak mode (`MAST_SOAK=1`): the headless perf gate (`scripts/perf-soak.sh`).
//!
//! The switch is read here, once, and every engine subsystem that talks to the
//! network, resumes agents or polls GitHub/Linear asks [`enabled`]. The script
//! and the UI driver also isolate the process from outside (a host sandbox,
//! PATH shims, and a scratch HOME). [`preflight`] checks the isolation and
//! the engine refuses to start in soak mode if the check fails.

use std::net::{SocketAddr, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

/// Directory holding the guarded `git` and fail-closed stubs, set by the script.
pub const SHIM_DIR_ENV: &str = "MAST_SOAK_SHIM_DIR";

/// Whether this process runs in soak mode.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| matches!(std::env::var("MAST_SOAK").as_deref(), Ok("1")))
}

/// Verify the isolation the script promised. Called once at startup in soak
/// mode; any failure must stop the engine.
pub fn preflight() -> Result<(), String> {
    let shim = std::env::var_os(SHIM_DIR_ENV)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| format!("{SHIM_DIR_ENV} is not set"))?;
    let git = crate::resolve_program("git");
    if git.parent() != Some(shim.as_path()) {
        return Err(format!(
            "git resolves to {}, not the soak guard in {}",
            git.display(),
            shim.display()
        ));
    }
    // On Linux an offline host can look the same as an isolated process
    // to a TCP probe. Verify the driver created a network namespace first.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;
        let host = std::env::var("MAST_SOAK_HOST_NETNS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("missing host network namespace for Linux soak")?;
        let current = std::fs::metadata("/proc/self/ns/net")
            .map_err(|error| format!("could not read the network namespace: {error}"))?
            .ino();
        if host == current {
            return Err("Linux soak must run in a separate network namespace".into());
        }
    }
    // Outbound network must be impossible, not just unconfigured.
    let target: SocketAddr = ([1, 1, 1, 1], 443).into();
    match TcpStream::connect_timeout(&target, Duration::from_secs(3)) {
        Ok(_) => Err("outbound connect to 1.1.1.1:443 succeeded; sandbox is not active".into()),
        Err(error) => {
            tracing::info!(%error, "soak preflight: outbound connect failed as required");
            Ok(())
        }
    }
}
