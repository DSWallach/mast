//! `WatchBuildJobs`, `CancelBuildJob` and `BuildJobLog`: the Space Builds panel's view of the
//! device's build queue (`crate::build_jobs`). Uses only the public `BuildJobs` API
//! (`list`, `get`, `cancel`, `position`, `quiet_window`) and the proto observation types.

use crate::build_jobs::{BuildJobError, BuildJobs, Job, JobState, ListFilter};
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_proto::{BuildJobState, BuildJobView, BuildJobsSnapshot};
use mast_rpc::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Finished jobs a snapshot keeps besides every unfinished one.
const RECENT_FINISHED: usize = 30;
/// How often the queue is re-read; `BuildJobs` has no change feed.
const POLL: Duration = Duration::from_millis(1000);
const LOG_TAIL_DEFAULT: usize = 200;
const LOG_TAIL_MAX: usize = 2000;
const LOG_READ_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WatchBuildJobsParams {
    pub space_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct BuildJobLogReply {
    pub text: String,
    /// Older lines exist before the first returned line.
    pub truncated: bool,
}

fn state_of(state: JobState) -> BuildJobState {
    match state {
        JobState::AwaitingApproval => BuildJobState::AwaitingApproval,
        JobState::Queued => BuildJobState::Queued,
        JobState::Running => BuildJobState::Running,
        JobState::Succeeded => BuildJobState::Succeeded,
        JobState::Failed => BuildJobState::Failed,
        JobState::Cancelled => BuildJobState::Cancelled,
        JobState::TimedOut => BuildJobState::TimedOut,
        JobState::Lost => BuildJobState::Lost,
    }
}

/// `report` of the exit summary when there is one, else the job's reason.
fn summary_of(job: &Job) -> Option<String> {
    let report = job
        .summary_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|value| value.get("report")?.as_str().map(str::to_owned))
        .filter(|report| !report.trim().is_empty());
    report.or_else(|| {
        job.reason
            .clone()
            .filter(|reason| !reason.trim().is_empty())
    })
}

fn queue_wait_secs(job: &Job) -> Option<u64> {
    let started = job.started_at?;
    Some((started - job.created_at).max(0) as u64 / 1000)
}

/// The proto observation of one job. `position` is `Some` only while it is queued.
pub(super) fn view_of(jobs: &BuildJobs, job: &Job) -> BuildJobView {
    let position = (job.state == JobState::Queued)
        .then(|| jobs.position(&job.id).ok())
        .flatten()
        .filter(|position| *position > 0)
        .map(|position| position as u32);
    BuildJobView {
        job_id: job.id.clone(),
        space_id: job.space_id.clone(),
        chat_id: job.chat_id.clone(),
        device_id: job.device_id.clone(),
        checkout_id: job.worktree_key.clone(),
        cwd: job.cwd.clone(),
        argv: job.argv.clone(),
        state: state_of(job.state),
        created_at: job.created_at,
        label: job.label.clone(),
        head_sha: job.head_sha.clone(),
        position,
        started_at: job.started_at,
        finished_at: job.finished_at,
        exit_code: job.exit_code,
        signal: job.signal,
        summary: summary_of(job),
        queue_wait_secs: queue_wait_secs(job),
    }
}

/// Every unfinished job of the Space plus the newest finished ones, newest first.
pub(super) fn snapshot(
    jobs: &BuildJobs,
    space_id: &str,
) -> Result<BuildJobsSnapshot, BuildJobError> {
    let active = jobs.list(&ListFilter {
        space_id: Some(space_id.to_owned()),
        active_only: true,
        ..Default::default()
    })?;
    let recent = jobs.list(&ListFilter {
        space_id: Some(space_id.to_owned()),
        limit: Some(RECENT_FINISHED + active.len()),
        ..Default::default()
    })?;
    let mut rows = active;
    let mut finished = 0;
    for job in recent {
        if rows.iter().any(|row| row.id == job.id) {
            continue;
        }
        if finished < RECENT_FINISHED {
            finished += 1;
            rows.push(job);
        }
    }
    rows.sort_by_key(|job| std::cmp::Reverse(job.seq));
    let mut hosts: Vec<&str> = rows.iter().map(|job| job.host.as_str()).collect();
    hosts.sort_unstable();
    hosts.dedup();
    let quiet_window_active = hosts
        .iter()
        .any(|host| matches!(jobs.quiet_window(host), Ok(Some(_))));
    Ok(BuildJobsSnapshot {
        space_id: space_id.to_owned(),
        jobs: rows.iter().map(|job| view_of(jobs, job)).collect(),
        quiet_window_active,
    })
}

/// The Space's jobs now, then a frame whenever they change. With no queue attached (the engine
/// has not built one) the stream is one empty snapshot, then stays quiet.
pub(super) fn watch(jobs: Option<BuildJobs>, space_id: String) -> BoxStream<'static, Value> {
    let Some(jobs) = jobs else {
        let empty = BuildJobsSnapshot {
            space_id,
            ..Default::default()
        };
        // Then wait for the subscriber to leave: an ended stream would be resubscribed forever.
        return futures::stream::iter(serde_json::to_value(empty).ok())
            .chain(futures::stream::pending())
            .boxed();
    };
    struct State {
        jobs: BuildJobs,
        space_id: String,
        last: Option<BuildJobsSnapshot>,
    }
    let state = State {
        jobs,
        space_id,
        last: None,
    };
    futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.last.is_some() {
                tokio::time::sleep(POLL).await;
            }
            let (jobs, space_id) = (state.jobs.clone(), state.space_id.clone());
            let read = tokio::task::spawn_blocking(move || snapshot(&jobs, &space_id)).await;
            let snapshot = match read {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) => {
                    tracing::warn!(space = %state.space_id, "build jobs snapshot failed: {error}");
                    state.last.as_ref()?;
                    continue;
                }
                Err(_) => return None,
            };
            if state.last.as_ref() == Some(&snapshot) {
                continue;
            }
            let value = serde_json::to_value(&snapshot).ok()?;
            state.last = Some(snapshot);
            return Some((value, state));
        }
    })
    .boxed()
}

fn failed(error: BuildJobError) -> RpcError {
    RpcError::Failed(error.to_string())
}

/// `CancelBuildJob`: a queued job is cancelled at once; a running one is asked to stop. The
/// request is made on behalf of the chat the panel shows as the job's owner, so `BuildJobs`
/// applies its own owner check.
///
/// Known gap (W3-T1 followup N4, not fixed here): a job that other chats joined as waiters is
/// stopped for all of them when its owner's cancel arrives.
pub(super) fn cancel(jobs: &BuildJobs, job_id: &str) -> Result<BuildJobView, RpcError> {
    let owner = jobs.get(job_id).map_err(failed)?.chat_id;
    let job = jobs.cancel(job_id, Some(&owner)).map_err(failed)?;
    Ok(view_of(jobs, &job))
}

/// The job's log file, only when it is a regular file inside `dir` (symlinks resolved). `None`
/// when there is no log yet; `Err` when the recorded path leads outside the log directory.
fn confined_log(dir: &Path, log_path: &str) -> Result<Option<PathBuf>, RpcError> {
    let Ok(log) = std::fs::canonicalize(log_path) else {
        return Ok(None);
    };
    let dir = std::fs::canonicalize(dir).map_err(|e| RpcError::Failed(e.to_string()))?;
    if !log.starts_with(&dir) || !log.is_file() {
        return Err(RpcError::Failed(
            "the job's log is not inside the build job directory".into(),
        ));
    }
    Ok(Some(log))
}

/// `BuildJobLog`: the last `tail` lines (default 200, at most 2000, from the last 256 KiB) of the
/// job's log. The path comes from the job's own record and must stay inside the build job
/// directory; the client supplies only a job id.
pub(super) fn log_tail(
    jobs: &BuildJobs,
    job_id: &str,
    tail: Option<u32>,
) -> Result<BuildJobLogReply, RpcError> {
    let job = jobs.get(job_id).map_err(failed)?;
    let tail = tail
        .map_or(LOG_TAIL_DEFAULT, |tail| tail as usize)
        .clamp(1, LOG_TAIL_MAX);
    let empty = BuildJobLogReply {
        text: String::new(),
        truncated: false,
    };
    let Some(log) = confined_log(&jobs.config().jobs_dir(), &job.log_path)? else {
        return Ok(empty);
    };
    let Ok(mut file) = std::fs::File::open(&log) else {
        return Ok(empty);
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(LOG_READ_BYTES);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_ok() {
        let _ = file.take(LOG_READ_BYTES).read_to_end(&mut bytes);
    }
    Ok(tail_of(&String::from_utf8_lossy(&bytes), tail, start > 0))
}

/// The last `tail` lines of `text`. `cut_front` says the text already starts mid-file.
fn tail_of(text: &str, tail: usize, cut_front: bool) -> BuildJobLogReply {
    let mut lines: Vec<&str> = text.lines().collect();
    // A read that started mid-file begins with a partial line: drop it.
    if cut_front && !lines.is_empty() {
        lines.remove(0);
    }
    let truncated = cut_front || lines.len() > tail;
    let skip = lines.len().saturating_sub(tail);
    BuildJobLogReply {
        text: lines[skip..].join("\n"),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_log_comes_back_whole() {
        let reply = tail_of("one\ntwo\nthree\n", 10, false);
        assert_eq!(reply.text, "one\ntwo\nthree");
        assert!(!reply.truncated);
    }

    #[test]
    fn a_long_log_keeps_its_last_lines_and_says_it_was_cut() {
        let reply = tail_of("a\nb\nc\nd\n", 2, false);
        assert_eq!(reply.text, "c\nd");
        assert!(reply.truncated);
    }

    #[test]
    fn a_read_that_started_mid_file_drops_its_partial_first_line() {
        let reply = tail_of("tial line\nb\nc\n", 10, true);
        assert_eq!(reply.text, "b\nc");
        assert!(reply.truncated);
    }

    use crate::build_jobs::{BuildJobsConfig, HostProfile, NewJob};

    fn queue() -> (tempfile::TempDir, BuildJobs) {
        let dir = tempfile::tempdir().unwrap();
        let config = BuildJobsConfig::new(
            dir.path().to_owned(),
            vec![HostProfile::local(dir.path().to_owned())],
        );
        let jobs = BuildJobs::builder(config).open().unwrap();
        (dir, jobs)
    }

    fn enqueue(
        jobs: &BuildJobs,
        dir: &std::path::Path,
        space: &str,
        chat: &str,
        test: &str,
    ) -> String {
        jobs.enqueue(NewJob {
            op_id: format!("{chat}:{test}"),
            chat_id: chat.into(),
            space_id: space.into(),
            device_id: "local".into(),
            worktree_key: format!("wt-{chat}"),
            cwd: dir.to_string_lossy().into_owned(),
            argv: vec!["cargo".into(), "test".into(), test.into()],
            ..Default::default()
        })
        .unwrap()
        .job
        .id
    }

    #[test]
    fn a_snapshot_holds_one_spaces_jobs_newest_first_with_queue_positions() {
        let (dir, jobs) = queue();
        let first = enqueue(&jobs, dir.path(), "space-1", "chat-a", "one");
        let second = enqueue(&jobs, dir.path(), "space-1", "chat-b", "two");
        let _other = enqueue(&jobs, dir.path(), "space-2", "chat-c", "three");
        let snapshot = snapshot(&jobs, "space-1").unwrap();
        assert_eq!(snapshot.space_id, "space-1");
        let ids: Vec<_> = snapshot
            .jobs
            .iter()
            .map(|job| job.job_id.as_str())
            .collect();
        assert_eq!(ids, [second.as_str(), first.as_str()]);
        assert!(
            snapshot
                .jobs
                .iter()
                .all(|job| job.state == BuildJobState::Queued)
        );
        let position = |id: &str| {
            snapshot
                .jobs
                .iter()
                .find(|job| job.job_id == id)
                .unwrap()
                .position
        };
        assert_eq!(position(&first), Some(1));
        assert_eq!(position(&second), Some(2));
        assert!(!snapshot.quiet_window_active);
    }

    #[test]
    fn cancelling_a_queued_job_shows_it_cancelled_without_a_position() {
        let (dir, jobs) = queue();
        let id = enqueue(&jobs, dir.path(), "space-1", "chat-a", "one");
        let view = cancel(&jobs, &id).unwrap();
        assert_eq!(view.state, BuildJobState::Cancelled);
        assert_eq!(view.position, None);
        assert!(cancel(&jobs, "no-such-job").is_err());
    }

    #[test]
    fn a_quiet_window_lease_shows_in_the_snapshot() {
        let (dir, jobs) = queue();
        enqueue(&jobs, dir.path(), "space-1", "chat-a", "one");
        jobs.acquire_quiet_window("local", "bench").unwrap();
        assert!(snapshot(&jobs, "space-1").unwrap().quiet_window_active);
    }

    #[test]
    fn the_log_tail_reads_the_end_of_the_jobs_log() {
        let (dir, jobs) = queue();
        let id = enqueue(&jobs, dir.path(), "space-1", "chat-a", "one");
        let none = log_tail(&jobs, &id, None).unwrap();
        assert_eq!((none.text.as_str(), none.truncated), ("", false));
        let log = jobs.get(&id).unwrap().log_path;
        std::fs::create_dir_all(std::path::Path::new(&log).parent().unwrap()).unwrap();
        std::fs::write(&log, "a\nb\nc\n").unwrap();
        let tail = log_tail(&jobs, &id, Some(2)).unwrap();
        assert_eq!((tail.text.as_str(), tail.truncated), ("b\nc", true));
        assert!(log_tail(&jobs, "no-such-job", None).is_err());
    }

    #[tokio::test]
    async fn the_watch_sends_the_current_snapshot_then_only_changes() {
        let (dir, jobs) = queue();
        let id = enqueue(&jobs, dir.path(), "space-1", "chat-a", "one");
        let mut stream = watch(Some(jobs.clone()), "space-1".into());
        let first = stream.next().await.unwrap();
        assert_eq!(first["jobs"][0]["jobId"], id.as_str());
        assert_eq!(first["jobs"][0]["state"], "queued");
        // Nothing changed: no second frame yet.
        assert!(
            tokio::time::timeout(Duration::from_millis(2500), stream.next())
                .await
                .is_err()
        );
        cancel(&jobs, &id).unwrap();
        let next = stream.next().await.unwrap();
        assert_eq!(next["jobs"][0]["state"], "cancelled");
    }

    #[tokio::test]
    async fn without_a_queue_the_watch_is_one_empty_snapshot_and_stays_open() {
        let mut stream = watch(None, "space-1".into());
        let first = stream.next().await.unwrap();
        assert_eq!(first["spaceId"], "space-1");
        assert_eq!(first["jobs"].as_array().map(Vec::len), Some(0));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .is_err()
        );
    }

    #[test]
    fn a_log_outside_the_job_directory_is_refused_and_a_missing_one_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let jobs_dir = dir.path().join("build-jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        let inside = jobs_dir.join("a.log");
        std::fs::write(&inside, "x").unwrap();
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        let link = jobs_dir.join("b.log");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert!(
            confined_log(&jobs_dir, inside.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        assert!(confined_log(&jobs_dir, outside.to_str().unwrap()).is_err());
        assert!(confined_log(&jobs_dir, link.to_str().unwrap()).is_err());
        assert!(confined_log(&jobs_dir, jobs_dir.to_str().unwrap()).is_err());
        assert!(
            confined_log(&jobs_dir, jobs_dir.join("none.log").to_str().unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn states_map_one_to_one() {
        for (job, view) in [
            (JobState::AwaitingApproval, BuildJobState::AwaitingApproval),
            (JobState::Queued, BuildJobState::Queued),
            (JobState::Running, BuildJobState::Running),
            (JobState::Succeeded, BuildJobState::Succeeded),
            (JobState::Failed, BuildJobState::Failed),
            (JobState::Cancelled, BuildJobState::Cancelled),
            (JobState::TimedOut, BuildJobState::TimedOut),
            (JobState::Lost, BuildJobState::Lost),
        ] {
            assert_eq!(state_of(job), view);
        }
    }
}
