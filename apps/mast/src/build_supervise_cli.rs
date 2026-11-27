//! `mast build-job-supervise <job-id>`: the detached supervisor of one queued build job.
//!
//! The engine spawns it as a session leader (`setsid`), so it outlives an engine restart. It reads
//! `<jobs dir>/<job-id>.spec.json`, runs the command with stdout and stderr in `<job-id>.log`,
//! enforces the job's timeout and writes `<job-id>.exit` atomically when the command ends. The
//! logic lives in `mast_engine::build_jobs::supervise`; this is only the entry point.
//!
//! The jobs directory is `MAST_BUILD_JOBS_DIR` (set by the engine when it spawns the supervisor),
//! else `<data dir>/build-jobs`.

use std::path::PathBuf;

use crate::paths;

fn jobs_dir() -> PathBuf {
    match std::env::var_os("MAST_BUILD_JOBS_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => paths::data_dir().join("build-jobs"),
    }
}

/// Run the supervisor to completion; the return value is the process exit code.
pub fn run(job_id: &str) -> i32 {
    mast_engine::build_jobs::supervise(&jobs_dir(), job_id)
}
