//! Engine-wide bound on concurrently running `git` child processes.
//!
//! Watchers, discovery scans and RPC handlers all shell out to git. Without a
//! bound, a burst forks dozens of processes at once and the spawns contend on
//! the process-wide fork lock. Every engine git spawn takes a permit here, so
//! bursts queue instead. Permits are held only for the lifetime of one child
//! process, so nothing can wait on a permit while holding another.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Semaphore, SemaphorePermit};

/// Maximum git children alive at once across the engine.
pub(crate) const MAX_CONCURRENT_GIT: usize = 4;

static GIT_SPAWNS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_GIT);
static GIT_SPAWN_COUNT: AtomicU64 = AtomicU64::new(0);

/// Wait for a spawn slot and count the spawn. Hold the permit until the child
/// has exited.
pub(crate) async fn acquire() -> SemaphorePermit<'static> {
    // The semaphore is never closed, so acquisition cannot fail.
    let permit = GIT_SPAWNS
        .acquire()
        .await
        .expect("git spawn semaphore is never closed");
    GIT_SPAWN_COUNT.fetch_add(1, Ordering::Relaxed);
    permit
}

/// Total git spawns since process start (diagnostics and tests).
pub fn spawn_count() -> u64 {
    GIT_SPAWN_COUNT.load(Ordering::Relaxed)
}
