//! Bound native watcher instances across engine services. Unwatched entries use
//! their existing periodic repair passes.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Budget {
    used: AtomicUsize,
    limit: usize,
}

impl Budget {
    fn acquire(&'static self) -> Option<Permit> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .ok()
            .map(|_| Permit(self))
    }
}

struct Permit(&'static Budget);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct Watcher {
    watcher: notify::RecommendedWatcher,
    _permit: Permit,
}

impl Watcher {
    pub(crate) fn new(
        callback: impl FnMut(Result<notify::Event, notify::Error>) + Send + 'static,
    ) -> Option<Self> {
        static BUDGET: OnceLock<Budget> = OnceLock::new();
        let budget = BUDGET.get_or_init(|| Budget {
            used: AtomicUsize::new(0),
            limit: watcher_limit(),
        });
        let permit = budget.acquire()?;
        let watcher = notify::recommended_watcher(callback).ok()?;
        Some(Self {
            watcher,
            _permit: permit,
        })
    }

    pub(crate) fn watch(
        &mut self,
        path: &std::path::Path,
        mode: notify::RecursiveMode,
    ) -> notify::Result<()> {
        notify::Watcher::watch(&mut self.watcher, path, mode)
    }
}

fn watcher_limit() -> usize {
    #[cfg(unix)]
    {
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == 0 {
            let soft = unsafe { limit.assume_init() }.rlim_cur;
            return (soft / 2).min(256) as usize;
        }
    }
    128
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permits_are_bounded_and_released() {
        static BUDGET: Budget = Budget {
            used: AtomicUsize::new(0),
            limit: 2,
        };
        let first = BUDGET.acquire().unwrap();
        let second = BUDGET.acquire().unwrap();
        assert!(BUDGET.acquire().is_none());
        drop(first);
        assert!(BUDGET.acquire().is_some());
        drop(second);
    }
}
