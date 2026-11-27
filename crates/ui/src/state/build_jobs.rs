//! The device's build queue as the UI sees it: one `WatchBuildJobs` subscription per watched
//! Space (the same Spaces the Work Outline and artifacts watch), a [`BuildJobsStore`] fed by the
//! frames, and the panel's cancel and log calls. The store answers the sidebar's per-row badge
//! lookups, so with no jobs anywhere a chat row pays one `is_empty` check.

use super::watch_tasks::work_outline_retry_delay;
use super::*;
use crate::build_watch::jobs::BuildJobsStore;

/// The tail `BuildJobLog` returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildLogTail {
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
}

impl AppState {
    pub(crate) fn build_jobs(&self) -> &BuildJobsStore {
        &self.build_jobs
    }

    /// Apply one `WatchBuildJobs` frame; whether anything changed. Frames for Spaces no longer
    /// watched are dropped.
    pub(crate) fn apply_build_jobs_frame(
        &mut self,
        space_id: &str,
        snapshot: mast_proto::BuildJobsSnapshot,
    ) -> bool {
        if snapshot.space_id != space_id || !self.build_job_tasks.contains_key(space_id) {
            return false;
        }
        self.build_jobs.set(space_id, snapshot)
    }

    /// A Space's subscribe failed for good (an engine without the build queue RPCs): show no
    /// builds for it.
    pub(crate) fn apply_build_jobs_failure(&mut self, space_id: &str) -> bool {
        self.build_jobs
            .set(space_id, mast_proto::BuildJobsSnapshot::default())
    }

    /// `WatchBuildJobs` follows the Work Outline watch set exactly.
    pub(super) fn reconcile_build_job_watches(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            self.build_job_tasks.clear();
            return;
        };
        let wanted = self.desired_work_outline_spaces();
        self.build_job_tasks
            .retain(|space_id, _| wanted.contains(space_id));
        let dropped = self.build_jobs.retain(|space_id| wanted.contains(space_id));
        for space_id in wanted {
            if self.build_job_tasks.contains_key(&space_id) {
                continue;
            }
            let task = spawn_build_jobs_watch(cx, handle.clone(), space_id.clone());
            self.build_job_tasks.insert(space_id, task);
        }
        if dropped {
            cx.notify();
        }
    }

    /// `CancelBuildJob`: fire and forget. The next `WatchBuildJobs` frame shows the outcome; a
    /// refusal is logged.
    pub(crate) fn cancel_build_job(&mut self, job_id: &str, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let job_id = job_id.to_owned();
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "jobId": job_id });
            if let Err(err) = handle
                .client()
                .call(methods::CANCEL_BUILD_JOB, params)
                .await
            {
                tracing::warn!(job = %job_id, error = %err, "cancel build job failed");
            }
        })
        .detach();
    }

    /// `BuildJobLog`: the last lines of the job's log. `Err` carries a line to show the user.
    pub(crate) fn fetch_build_log(
        &self,
        job_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<Result<BuildLogTail, String>>> {
        let handle = self.engine.clone()?;
        let job_id = job_id.to_owned();
        Some(cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "jobId": job_id, "tail": 200 });
            handle
                .client()
                .call_as::<BuildLogTail>(methods::BUILD_JOB_LOG, params)
                .await
                .map_err(|err| err.to_string())
        }))
    }

    /// Tests: install a Space's build jobs without a watch.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn set_test_build_jobs(&mut self, snapshot: mast_proto::BuildJobsSnapshot) {
        let space_id = snapshot.space_id.clone();
        self.build_jobs.set(&space_id, snapshot);
    }
}

/// One `WatchBuildJobs` subscription per Space, mirroring `spawn_artifacts_watch`:
/// `UnknownMethod` (an engine without the build queue RPCs) ends the watch; other failures
/// retry with backoff; malformed frames are dropped; the last good snapshot survives gaps.
pub(super) fn spawn_build_jobs_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    space_id: String,
) -> gpui::Task<()> {
    cx.spawn(async move |this, cx| {
        let mut attempt: u32 = 0;
        loop {
            let params = serde_json::json!({ "spaceId": space_id });
            let mut subscription = match handle
                .client()
                .subscribe_checked(methods::WATCH_BUILD_JOBS, params)
                .await
            {
                Ok(subscription) => subscription,
                Err(err) => {
                    let unsupported = matches!(err, RpcError::UnknownMethod(_));
                    tracing::debug!(%space_id, error = %err, unsupported, "build jobs watch unavailable");
                    if unsupported {
                        let _ = this.update(cx, |state, cx| {
                            if state.apply_build_jobs_failure(&space_id) {
                                cx.notify();
                            }
                        });
                        return;
                    }
                    cx.background_executor()
                        .timer(mast_harness::backoff::jittered(work_outline_retry_delay(attempt)))
                        .await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            };
            attempt = 0;
            while let Some(value) = subscription.recv().await {
                let snapshot: mast_proto::BuildJobsSnapshot = match serde_json::from_value(value) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(%space_id, error = %err, "dropping malformed build jobs frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    if state.apply_build_jobs_frame(&space_id, snapshot) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!(%space_id, "build jobs stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor()
                .timer(mast_harness::backoff::jittered(work_outline_retry_delay(0)))
                .await;
        }
    })
}
