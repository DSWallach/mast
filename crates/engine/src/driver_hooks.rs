//! Seams for the offscreen UI driver (`mast-ui-driver`), whose app embeds its own engine. They
//! exist only with the `ui-driver-hooks` feature, which that test-only crate alone enables;
//! without it every lookup is `None`.

use crate::build_jobs::BuildJobs;

#[cfg(feature = "ui-driver-hooks")]
static BUILD_QUEUES: std::sync::Mutex<Vec<(String, BuildJobs)>> =
    std::sync::Mutex::new(Vec::new());

/// Serve `jobs` from every RPC surface the engine for `device_id` assembles after this call.
#[cfg(feature = "ui-driver-hooks")]
pub fn attach_build_jobs(device_id: &str, jobs: BuildJobs) {
    let mut queues = BUILD_QUEUES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    queues.retain(|(id, _)| id != device_id);
    queues.push((device_id.to_owned(), jobs));
}

pub(crate) fn build_jobs_for(device_id: &str) -> Option<BuildJobs> {
    #[cfg(feature = "ui-driver-hooks")]
    {
        let queues = BUILD_QUEUES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queues
            .iter()
            .find(|(id, _)| id == device_id)
            .map(|(_, jobs)| jobs.clone())
    }
    #[cfg(not(feature = "ui-driver-hooks"))]
    {
        let _ = device_id;
        None
    }
}

/// A stand-in for the providers' usage services.
#[derive(Debug, Clone)]
pub struct FakeUsage {
    /// Claude is read from `{url}/claude` and Codex from `{url}/codex`.
    pub url: String,
    /// Holds `claude/.credentials.json` and `codex/auth.json`, read instead of
    /// the real sign-ins.
    pub sign_ins: std::path::PathBuf,
}

#[cfg(feature = "ui-driver-hooks")]
static FAKE_USAGE: std::sync::Mutex<Option<FakeUsage>> = std::sync::Mutex::new(None);

/// Make every account service built after this call hermetic: it reads only
/// the fake sign-ins, probes only the fake service and never runs `agy`.
#[cfg(feature = "ui-driver-hooks")]
pub fn use_fake_usage(fake: FakeUsage) {
    *FAKE_USAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fake);
}

pub(crate) fn fake_usage() -> Option<FakeUsage> {
    #[cfg(feature = "ui-driver-hooks")]
    {
        FAKE_USAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    #[cfg(not(feature = "ui-driver-hooks"))]
    {
        None
    }
}
