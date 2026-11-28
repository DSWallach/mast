//! Containment crate for the agent-only headless UI driver.
//! The driver uses the same offscreen Metal and Vulkan renderers as UI previews.
#![cfg(any(target_os = "macos", target_os = "linux"))]
#![allow(clippy::disallowed_methods)] // test-only tool; spawns scratch helpers directly

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

pub mod environment;
mod fake_usage;
pub mod materialize;
pub mod scenario;
pub mod serve;
pub mod session;
mod storyboard;
pub mod template;
pub mod tree;
pub mod world;

/// Reuse the engine's soak gate before assembling the in-process driver.
pub fn preflight() -> Result<(), String> {
    if !mast_harness::soak::enabled() {
        return Err("driver requires MAST_SOAK=1".to_owned());
    }
    mast_harness::soak::preflight()
}
