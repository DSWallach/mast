//! Engine-wide bound on concurrently running `git` child processes.
//!
//! Watchers, discovery scans and RPC handlers all shell out to git. Without a
//! bound, a burst forks dozens of processes at once and the spawns contend on
//! the process-wide fork lock. Every engine git spawn takes a permit here, so
//! bursts queue instead. Permits are held only for the lifetime of one child
//! process, so nothing can wait on a permit while holding another.

use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};

/// Maximum git children alive at once across the engine.
pub(crate) const MAX_CONCURRENT_GIT: usize = 4;

static GIT_SPAWNS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_GIT);
static GIT_SPAWN_COUNT: AtomicU64 = AtomicU64::new(0);

/// Local git spawns per window above which the engine logs one `warn`
/// (post-mortem 2026-11-16, P2). An idle Mast does a few dozen per minute.
pub(crate) const SPAWN_WARN_THRESHOLD: u64 = 120;
const SPAWN_WINDOW: Duration = Duration::from_secs(60);
/// Distinct caller tags tracked per window; later ones count as `"other"`.
const MAX_TAGS: usize = 48;

tokio::task_local! {
    static CALLER: &'static str;
}

/// Attribute every gated git spawn made by `fut` (on this task) to `tag`
/// instead of the default per-subcommand tag.
pub(crate) async fn scope<F: Future>(tag: &'static str, fut: F) -> F::Output {
    CALLER.scope(tag, fut).await
}

/// Default tag for a spawn nobody scoped: `<origin>:<subcommand>` from a fixed set.
pub(crate) fn default_tag(origin: &'static str, args: &[&str]) -> &'static str {
    let mut args = args.iter();
    let mut sub = "";
    while let Some(arg) = args.next() {
        match *arg {
            // Options that take a separate value.
            "-C" | "-c" => {
                args.next();
            }
            arg if arg.starts_with('-') => {}
            arg => {
                sub = arg;
                break;
            }
        }
    }
    macro_rules! tags {
        ($($sub:literal),*) => {
            match (origin, sub) {
                $( ("repos", $sub) => concat!("repos:", $sub),
                   ("source_control", $sub) => concat!("source_control:", $sub), )*
                ("repos", _) => "repos:other",
                _ => "source_control:other",
            }
        };
    }
    tags!(
        "branch", "symbolic-ref", "worktree", "status", "diff", "rev-parse", "log", "show",
        "for-each-ref", "ls-files", "config", "remote", "merge-base", "cat-file", "add", "commit",
        "checkout", "fetch", "clone", "push", "pull", "init", "reset", "stash", "rev-list"
    )
}

#[derive(Debug, Default)]
struct SpawnWindow {
    start: Option<Instant>,
    total: u64,
    tags: Vec<(&'static str, u64)>,
    warned: bool,
}

/// What a window logs when it crosses the threshold.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SpawnReport {
    pub total: u64,
    /// Highest first, at most five.
    pub top: Vec<(&'static str, u64)>,
}

impl SpawnWindow {
    /// Count one spawn at `now`. Returns a report the first time the window's
    /// total exceeds `threshold`: at most one per window, so at most one `warn`
    /// per minute however hard the storm.
    fn record(&mut self, now: Instant, tag: &'static str, threshold: u64) -> Option<SpawnReport> {
        if self.start.is_none_or(|start| now.duration_since(start) >= SPAWN_WINDOW) {
            self.start = Some(now);
            self.total = 0;
            self.tags.clear();
            self.warned = false;
        }
        self.total += 1;
        let key = if self.tags.iter().any(|(known, _)| *known == tag) || self.tags.len() < MAX_TAGS {
            tag
        } else {
            "other"
        };
        match self.tags.iter_mut().find(|(known, _)| *known == key) {
            Some((_, count)) => *count += 1,
            None => self.tags.push((key, 1)),
        }
        if self.total > threshold && !self.warned {
            self.warned = true;
            let mut top = self.tags.clone();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            top.truncate(5);
            return Some(SpawnReport { total: self.total, top });
        }
        None
    }
}

static WINDOW: Mutex<SpawnWindow> = Mutex::new(SpawnWindow {
    start: None,
    total: 0,
    tags: Vec::new(),
    warned: false,
});
static WARNINGS: AtomicU64 = AtomicU64::new(0);

fn record_spawn(tag: &'static str) {
    let report = WINDOW
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .record(Instant::now(), tag, SPAWN_WARN_THRESHOLD);
    if let Some(report) = report {
        WARNINGS.fetch_add(1, Ordering::Relaxed);
        let top = report
            .top
            .iter()
            .map(|(tag, count)| format!("{tag}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            spawns = report.total,
            threshold = SPAWN_WARN_THRESHOLD,
            window_secs = SPAWN_WINDOW.as_secs(),
            top_callers = %top,
            "engine is spawning git at a high rate"
        );
    }
}

/// Wait for a spawn slot and count the spawn under the current [`scope`] tag,
/// or `default_tag` when none is set. Hold the permit until the child has exited.
pub(crate) async fn acquire(default_tag: &'static str) -> SemaphorePermit<'static> {
    // The semaphore is never closed, so acquisition cannot fail.
    let permit = GIT_SPAWNS
        .acquire()
        .await
        .expect("git spawn semaphore is never closed");
    GIT_SPAWN_COUNT.fetch_add(1, Ordering::Relaxed);
    record_spawn(CALLER.try_with(|tag| *tag).unwrap_or(default_tag));
    permit
}

/// Spawn-rate `warn`s logged since process start (tests).
pub fn spawn_rate_warnings() -> u64 {
    WARNINGS.load(Ordering::Relaxed)
}

/// Total git spawns since process start (diagnostics and tests).
pub fn spawn_count() -> u64 {
    GIT_SPAWN_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_once_per_window_with_top_callers() {
        let mut window = SpawnWindow::default();
        let t0 = Instant::now();
        for _ in 0..120 {
            assert_eq!(window.record(t0, "refs", 120), None);
        }
        let report = window.record(t0, "change_request_root", 120).expect("121st spawn warns");
        assert_eq!(report.total, 121);
        assert_eq!(report.top, vec![("refs", 120), ("change_request_root", 1)]);
        // Same window: no second warning, however many more spawns.
        for _ in 0..500 {
            assert_eq!(window.record(t0 + Duration::from_secs(30), "refs", 120), None);
        }
        // Next window starts fresh and can warn again.
        let t1 = t0 + SPAWN_WINDOW;
        for _ in 0..120 {
            assert_eq!(window.record(t1, "diff_sync", 120), None);
        }
        assert!(window.record(t1, "diff_sync", 120).is_some());
    }

    #[test]
    fn tag_map_is_bounded() {
        let mut window = SpawnWindow::default();
        let now = Instant::now();
        let names: Vec<&'static str> = (0..MAX_TAGS + 10)
            .map(|i| &*Box::leak(format!("t{i}").into_boxed_str()))
            .collect();
        for name in names {
            window.record(now, name, u64::MAX);
        }
        assert!(window.tags.len() <= MAX_TAGS + 1);
    }

    #[test]
    fn default_tags_come_from_a_fixed_set() {
        assert_eq!(default_tag("repos", &["-C", "x", "worktree", "list"]), "repos:worktree");
        assert_eq!(default_tag("repos", &["frobnicate"]), "repos:other");
        assert_eq!(default_tag("source_control", &["status"]), "source_control:status");
    }

    #[tokio::test]
    async fn scope_overrides_the_default_tag() {
        let tag = scope("change_request_root", async {
            CALLER.try_with(|tag| *tag).unwrap_or("default")
        })
        .await;
        assert_eq!(tag, "change_request_root");
        assert!(CALLER.try_with(|tag| *tag).is_err());
    }
}
