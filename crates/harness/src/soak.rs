//! Soak mode (`MAST_SOAK=1`): the headless perf gate (`scripts/perf-soak.sh`).
//!
//! The switch is read here, once, and every engine subsystem that talks to the
//! network, resumes agents or polls GitHub/Linear asks [`enabled`]. The script
//! also isolates the process from outside (sandbox-exec, PATH shims, a scratch
//! HOME); [`preflight`] checks that isolation actually holds and the engine
//! refuses to start in soak mode when it does not.

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
