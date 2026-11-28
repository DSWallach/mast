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
