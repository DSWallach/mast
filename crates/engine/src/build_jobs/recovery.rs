//! Engine-restart recovery and process exit watching.
//!
//! For every row still `running` when the engine starts:
//!
//! 1. an exit file exists: the job finishes from it, whoever was watching;
//! 2. else the recorded PID is alive with the recorded start time: the detached supervisor is
//!    **re-adopted** and watched until it exits;
//! 3. else the job is `lost` (a PID the host reused for another process reads as lost with
//!    `pid <n> now belongs to another process (reused)`) and the owner is woken. Nothing retries.
//!
//! Rows that are `queued` or `awaiting_approval` carry on untouched, and wakes that were not
//! delivered stay `pending` in `waiters.wake_state`, so they go out on the next tick.

use super::{BuildJobError, BuildJobs, HostKind, JobState, Probe, runner::proc_start_time};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// Finished from an exit file.
    pub finished: Vec<String>,
    /// Still running; the engine watches them again.
    pub adopted: Vec<String>,
    /// `lost` (or `cancelled` when a stop had been requested).
    pub lost: Vec<String>,
}

pub(super) fn recover(jobs: &BuildJobs) -> Result<Recovered, BuildJobError> {
    let now = jobs.now();
    let mut out = Recovered::default();
    for job in jobs.inner.store.all_running()? {
        let Some(executor) = jobs.inner.executors.get(&job.host) else {
            jobs.inner.store.finish(
                &job.id,
                JobState::Lost,
                None,
                None,
                Some("unknown build host"),
                None,
                now,
            )?;
            out.lost.push(job.id);
            continue;
        };
        match executor.probe(&job) {
            Probe::Exited(record) => {
                jobs.finish_exited(&job, &record, now);
                out.finished.push(job.id);
            }
            Probe::Running { .. } | Probe::Unknown(_) => {
                let local = jobs
                    .inner
                    .cfg
                    .host(&job.host)
                    .is_some_and(|h| matches!(h.kind, HostKind::Local { .. }));
                if let (true, Some(pid), Some(start)) =
                    (local, job.pid, job.proc_start_time.clone())
                {
                    let notify = jobs.inner.notify.clone();
                    watch_exit(pid, start, Arc::new(move || notify.notify_one()));
                }
                out.adopted.push(job.id);
            }
            Probe::Gone(why) => {
                let (state, reason) = match job.stop_reason.as_deref() {
                    Some("cancel") => (JobState::Cancelled, "cancelled".to_string()),
                    _ => (JobState::Lost, why),
                };
                jobs.inner
                    .store
                    .finish(&job.id, state, None, None, Some(&reason), None, now)?;
                out.lost.push(job.id);
            }
        }
    }
    Ok(out)
}

/// Call `on_exit` once the process with this PID and start time is gone. Uses a `pidfd` on Linux
/// (readable when the process exits, so there is no PID-reuse window once the descriptor is
/// open) and polls the start time elsewhere, or when `pidfd_open` is unavailable.
pub fn watch_exit(pid: i64, start_time: String, on_exit: Arc<dyn Fn() + Send + Sync>) {
    std::thread::spawn(move || {
        #[cfg(target_os = "linux")]
        if wait_pidfd(pid, &start_time) {
            on_exit();
            return;
        }
        while proc_start_time(pid).as_deref() == Some(start_time.as_str()) {
            std::thread::sleep(Duration::from_millis(500));
        }
        on_exit();
    });
}

/// `true` once the process exited; `false` when no pidfd could be opened for it.
#[cfg(target_os = "linux")]
fn wait_pidfd(pid: i64, start_time: &str) -> bool {
    // SAFETY: pidfd_open takes a pid and flags and returns a descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) } as i32;
    if fd < 0 {
        return false;
    }
    // The descriptor now pins this process; make sure it is the one that was recorded.
    if proc_start_time(pid).as_deref() != Some(start_time) {
        // SAFETY: fd was returned by pidfd_open above.
        unsafe { libc::close(fd) };
        return true; // already gone (or reused): nothing left to wait for
    }
    loop {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` is a valid pollfd for one descriptor.
        let ready = unsafe { libc::poll(&mut poll, 1, 5000) };
        if ready > 0
            || (ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted)
        {
            break;
        }
    }
    // SAFETY: fd was returned by pidfd_open above.
    unsafe { libc::close(fd) };
    true
}
