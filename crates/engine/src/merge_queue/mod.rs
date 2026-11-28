//! Durable per-project merge queue (`merge-queue.sqlite3`, W3-T5).
//!
//! A worker's tested commit becomes a [`LandRequest`]. Requests for one
//! `(repository, target ref)` land one at a time:
//!
//! 1. **Early conflict check** on submit: `git merge-tree --write-tree` of the
//!    tested commit against the current target. It touches no worktree.
//! 2. **Prepare**: merge the tested commit onto the target in a scratch
//!    worktree the queue owns, fold the changelog fragment into that same merge
//!    commit ([`changelog`]), save the commit on the request, and enqueue the
//!    build through [`BuildJobs::enqueue`] under an operation id derived from
//!    it, so a crash never leaves a second, untracked build. The commit that is
//!    built is the commit that lands.
//! 3. **Build**: wait for the job to reach a terminal state.
//! 4. **Review**: wait for an approving [`ReviewVerdict`].
//! 5. **Land**: `git update-ref <target> <candidate> <expected old>`. If the
//!    target moved, the request goes back to the queue and is merged and built
//!    again; the ref is never overwritten.
//!
//! A failed step leaves the target ref untouched and records why on the
//! request. The queue refuses to move a ref that is checked out in any
//! worktree, including one paused in a rebase or bisect (it would leave that
//! checkout stale), and waits until it is free. A checkout made between that
//! check and the ref update is not detected.
//!
//! The pipeline is a state machine: [`MergeQueue::tick`] makes whatever
//! progress is possible and returns; it never sleeps. Calls block on git and
//! SQLite, so async callers use `spawn_blocking`. One engine owns a queue.
//!
//! [`MergeQueue::landed`] is the "landed" signal that later ends
//! `QueueWaitReason::NotLanded`. Nothing is wired to agent tools yet.
//!
//! Trust: the caller supplies `build.argv` already validated against the
//! approval rules (or sets `awaiting_approval`), and is the one that decides
//! whose verdict to record.

pub mod changelog;
mod git;
mod store;

use crate::build_jobs::{BuildJobError, BuildJobs, JobState, NewJob};
use crate::review_dispatch::ReviewerChoice;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// Times a request is re-merged because the target moved before it fails.
pub const MAX_ATTEMPTS: u32 = 5;
/// Most doc deltas one request carries.
const MAX_DOC_DELTAS: usize = 32;
/// Longest doc delta note and review summary kept, in bytes.
const MAX_NOTE_BYTES: usize = 4096;
/// Transitions one lane may make in one [`MergeQueue::tick`].
const MAX_STEPS_PER_TICK: usize = 8;
const SCRATCH_DIR: &str = "merge-queue-worktrees";
const REF_PREFIX: &str = "refs/mast/merge-queue";

#[derive(Debug, thiserror::Error)]
pub enum MergeQueueError {
    #[error("merge queue store: {0}")]
    Store(String),
    #[error("merge queue: {0}")]
    Git(String),
    #[error("no such land request: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

impl From<MergeQueueError> for crate::EngineError {
    fn from(error: MergeQueueError) -> Self {
        crate::EngineError::Other(error.to_string())
    }
}

/// A note for a shared document the worker may not edit itself. The queue
/// carries it to the [`LandedRecord`]; it does not edit the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocDelta {
    pub path: String,
    pub note: String,
}

/// How the merged tree is built and tested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// No approval rule matched `argv`: the job waits for approval.
    #[serde(default)]
    pub awaiting_approval: bool,
}

/// Who the build job belongs to. Taken from the agent token, never from input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildOwner {
    pub chat_id: String,
    pub space_id: String,
    pub device_id: String,
}

/// What a caller hands to [`MergeQueue::submit`].
#[derive(Debug, Clone)]
pub struct NewLandRequest {
    /// Any checkout of the repository.
    pub repo: PathBuf,
    pub project_id: String,
    pub work_item_id: String,
    /// The worker's branch. Informational: only `tested_sha` is merged.
    pub branch: String,
    pub tested_sha: String,
    /// What the work started from. A `mast/int/...` target that does not exist
    /// yet is created from it.
    pub base_ref: String,
    /// `main` or `mast/int/<project>/<lead>`, with or without `refs/heads/`.
    pub target_ref: String,
    pub changelog_fragment: Option<String>,
    pub doc_deltas: Vec<DocDelta>,
    pub build: BuildSpec,
    pub owner: BuildOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandState {
    Queued,
    Building,
    AwaitingReview,
    Landed,
    /// The tested commit does not merge into the target.
    Conflicted,
    /// Preparing, building or advancing the ref failed.
    Failed,
    /// The review rejected the change.
    Rejected,
    Cancelled,
}

impl LandState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Building => "building",
            Self::AwaitingReview => "awaiting_review",
            Self::Landed => "landed",
            Self::Conflicted => "conflicted",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Building | Self::AwaitingReview)
    }
}

/// The result of merging a request against the target as it was then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictCheck {
    pub target_sha: String,
    /// Conflicting paths; empty means the merge is clean.
    pub paths: Vec<String>,
}

impl ConflictCheck {
    pub fn is_clean(&self) -> bool {
        self.paths.is_empty()
    }
}

/// A reviewer's answer about one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewVerdict {
    pub approved: bool,
    /// The tested commit, or the request's current merge commit.
    pub reviewed_sha: String,
    pub reviewer_chat_id: String,
    pub summary: String,
}

/// One land request and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandRequest {
    pub id: String,
    pub seq: i64,
    /// The repository's canonical common git directory.
    pub repo: String,
    pub project_id: String,
    pub work_item_id: String,
    pub branch: String,
    /// Full id of the tested commit.
    pub tested_sha: String,
    pub base_ref: String,
    /// Full ref name.
    pub target_ref: String,
    pub changelog_fragment: Option<String>,
    pub doc_deltas: Vec<DocDelta>,
    pub build: BuildSpec,
    pub owner: BuildOwner,
    /// The target ref has been seen to exist, so it is never created again.
    #[serde(default)]
    pub target_existed: bool,
    pub state: LandState,
    /// Why the request ended, was requeued, or is waiting.
    pub reason: Option<String>,
    pub conflict_check: Option<ConflictCheck>,
    /// The target commit the merge commit sits on.
    pub expected_target: Option<String>,
    /// The target ref did not exist when the merge was made.
    #[serde(default)]
    pub creates_target: bool,
    /// The merge commit being built, reviewed and landed.
    pub candidate_sha: Option<String>,
    pub build_job_id: Option<String>,
    /// Counts the merge commits given up. It is part of the build's operation
    /// id, so even an identical commit made again gets a new job.
    #[serde(default)]
    pub generation: u32,
    /// The saved merge commit is being given up and its build was asked to
    /// stop. Neither is used again, even if the target moves back.
    #[serde(default)]
    pub discarding: bool,
    pub reviewer: Option<ReviewerChoice>,
    pub verdict: Option<ReviewVerdict>,
    /// Times the request was requeued because the target moved.
    pub attempts: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A work item's change reached a target ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandedRecord {
    pub request_id: String,
    pub project_id: String,
    pub work_item_id: String,
    pub repo: String,
    pub target_ref: String,
    pub tested_sha: String,
    pub landed_sha: String,
    /// Notes still to be applied to shared documents.
    pub doc_deltas: Vec<DocDelta>,
    pub landed_at: i64,
}

/// What one [`MergeQueue::step`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The lane has no unfinished request.
    Idle,
    /// The lane's request cannot move until something outside the queue does.
    Waiting { request_id: String, reason: String },
    /// The request changed state.
    Advanced {
        request_id: String,
        state: LandState,
    },
}

#[derive(Clone)]
pub struct MergeQueue {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    store: store::Store,
    builds: BuildJobs,
    /// Serializes every change, so a verdict or a cancel never races a step.
    changes: Mutex<()>,
    /// Called after each landing is saved.
    land_hook: Mutex<Option<LandHook>>,
}

/// Runs after a landing is recorded, under the queue's change lock: keep it
/// short and never call back into the queue. A hook cannot fail the land.
pub type LandHook = Arc<dyn Fn(&LandRequest, &LandedRecord) + Send + Sync>;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn invalid(message: impl Into<String>) -> MergeQueueError {
    MergeQueueError::Invalid(message.into())
}

fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A work item id is used in file and ref names.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The full ref a land may advance: `refs/heads/main` or
/// `refs/heads/mast/int/<project>/<lead>`.
pub fn normalize_target_ref(target: &str) -> Result<String, MergeQueueError> {
    let short = target.strip_prefix("refs/heads/").unwrap_or(target);
    let allowed = short == "main"
        || short.strip_prefix("mast/int/").is_some_and(|rest| {
            let parts: Vec<&str> = rest.split('/').collect();
            parts.len() == 2
                && parts.iter().all(|part| {
                    !part.is_empty()
                        && !part.starts_with(['.', '-'])
                        && !part.ends_with('.')
                        && !part.ends_with(".lock")
                        && !part.contains("..")
                        && part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                })
        });
    if !allowed {
        return Err(invalid(format!(
            "target ref {target:?} must be main or mast/int/<project>/<lead>"
        )));
    }
    Ok(format!("refs/heads/{short}"))
}

impl MergeQueue {
    /// Open (or create) the queue under `root`, which also holds its scratch
    /// worktrees. Builds run through `builds`, whose own loop the caller drives.
    pub fn open(root: &Path, builds: BuildJobs) -> Result<Self, MergeQueueError> {
        std::fs::create_dir_all(root).map_err(|e| MergeQueueError::Store(e.to_string()))?;
        let root =
            std::fs::canonicalize(root).map_err(|e| MergeQueueError::Store(e.to_string()))?;
        let store = store::Store::open(&root)?;
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                store,
                builds,
                changes: Mutex::new(()),
                land_hook: Mutex::new(None),
            }),
        })
    }

    /// Run `hook` after every landing from now on (replacing any earlier one).
    pub fn set_land_hook(&self, hook: LandHook) {
        *self.inner.land_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    fn changes(&self) -> MutexGuard<'_, ()> {
        self.inner.changes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a land request after the early conflict check. A repeat of an
    /// unfinished, landed or rejected request returns that request; one that
    /// conflicted, failed or was cancelled may be submitted again.
    pub fn submit(&self, new: NewLandRequest) -> Result<LandRequest, MergeQueueError> {
        if !safe_id(&new.work_item_id) {
            return Err(invalid("workItemId must be letters, digits, '-' or '_'"));
        }
        let target_ref = normalize_target_ref(&new.target_ref)?;
        let sha = new.tested_sha.trim().to_ascii_lowercase();
        if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("testedSha must be 7 to 64 hex digits"));
        }
        if new.build.argv.is_empty() {
            return Err(invalid("a land request needs a build command"));
        }
        let fragment = new.changelog_fragment.filter(|f| !f.trim().is_empty());
        if fragment
            .as_ref()
            .is_some_and(|f| f.len() > changelog::MAX_FRAGMENT_BYTES)
        {
            return Err(invalid("the changelog fragment is too large"));
        }
        if new.doc_deltas.len() > MAX_DOC_DELTAS
            || new
                .doc_deltas
                .iter()
                .any(|d| d.path.len() > MAX_NOTE_BYTES || d.note.len() > MAX_NOTE_BYTES)
        {
            return Err(invalid("the doc deltas are too large"));
        }
        let repo = git::common_dir(&new.repo)?;
        let tested = git::commit_id(&repo, &sha)?
            .ok_or_else(|| invalid(format!("testedSha {sha} is not a commit here")))?;
        if !tested.starts_with(&sha) {
            return Err(invalid(format!(
                "testedSha {sha} does not name a commit id"
            )));
        }
        let repo_key = repo.to_string_lossy().into_owned();

        let _changes = self.changes();
        if let Some(existing) =
            self.inner
                .store
                .latest_matching(&repo_key, &target_ref, &new.work_item_id, &tested)?
            && !matches!(
                existing.state,
                LandState::Conflicted | LandState::Failed | LandState::Cancelled
            )
        {
            let same = existing.project_id == new.project_id
                && existing.branch == new.branch
                && existing.base_ref == new.base_ref
                && existing.changelog_fragment == fragment
                && existing.doc_deltas == new.doc_deltas
                && existing.build == new.build;
            if !same {
                return Err(invalid(format!(
                    "land request {} ({}) already covers this commit with different inputs",
                    existing.id,
                    existing.state.as_str()
                )));
            }
            return Ok(existing);
        }
        let existed = self.inner.store.has_landed(&repo_key, &target_ref)?;
        let (target_sha, creates) = current_target(&repo, &target_ref, &new.base_ref, existed)?;
        let check = ConflictCheck {
            paths: git::merge_tree_conflicts(&repo, &target_sha, &tested)?,
            target_sha,
        };
        let now = now_ms();
        let mut request = LandRequest {
            id: crate::new_id(),
            seq: 0,
            repo: repo_key,
            project_id: new.project_id,
            work_item_id: new.work_item_id,
            branch: new.branch,
            tested_sha: tested,
            base_ref: new.base_ref,
            target_ref,
            changelog_fragment: fragment,
            doc_deltas: new.doc_deltas,
            build: new.build,
            owner: new.owner,
            target_existed: !creates,
            state: if check.is_clean() {
                LandState::Queued
            } else {
                LandState::Conflicted
            },
            reason: (!check.is_clean()).then(|| conflict_reason(&check.paths)),
            conflict_check: Some(check),
            expected_target: None,
            creates_target: false,
            candidate_sha: None,
            build_job_id: None,
            generation: 0,
            discarding: false,
            reviewer: None,
            verdict: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
        };
        if request.state == LandState::Queued {
            // Keep the tested commit alive even if its branch is deleted.
            git::run(
                &repo,
                &[
                    "update-ref",
                    &pin(&request.id, "tested"),
                    &request.tested_sha,
                ],
            )?;
        }
        self.inner.store.insert(&mut request)?;
        Ok(request)
    }

    pub fn get(&self, id: &str) -> Result<LandRequest, MergeQueueError> {
        self.inner
            .store
            .get(id)?
            .ok_or_else(|| MergeQueueError::NotFound(id.to_owned()))
    }

    /// Every request, oldest first.
    pub fn list(&self) -> Result<Vec<LandRequest>, MergeQueueError> {
        self.inner.store.all()
    }

    /// The latest landing of a work item: the "landed" signal. A caller that
    /// needs a particular target ref checks `target_ref` (or uses
    /// [`Self::landings`]).
    pub fn landed(&self, work_item_id: &str) -> Result<Option<LandedRecord>, MergeQueueError> {
        Ok(self.landings(work_item_id)?.into_iter().next())
    }

    /// Every landing of a work item, newest first.
    pub fn landings(&self, work_item_id: &str) -> Result<Vec<LandedRecord>, MergeQueueError> {
        self.inner.store.landings(work_item_id)
    }

    /// Record which reviewer was chosen, including why it shares the
    /// implementer's model family when it does.
    pub fn assign_reviewer(
        &self,
        id: &str,
        reviewer: ReviewerChoice,
    ) -> Result<LandRequest, MergeQueueError> {
        let _changes = self.changes();
        let mut request = self.open_request(id)?;
        request.reviewer = Some(reviewer);
        self.save(&mut request)?;
        Ok(request)
    }

    /// Record a review. An approval of the tested commit stands if the request
    /// is requeued; an approval of one merge commit does not. A rejection ends
    /// the request and leaves the target ref alone.
    pub fn record_verdict(
        &self,
        id: &str,
        mut verdict: ReviewVerdict,
    ) -> Result<LandRequest, MergeQueueError> {
        let _changes = self.changes();
        let mut request = self.open_request(id)?;
        if self.recover_landed(&mut request)? {
            return Err(invalid(format!("land request {id} already landed")));
        }
        let reviewed = verdict.reviewed_sha.trim().to_ascii_lowercase();
        let matches = |sha: &str| reviewed.len() >= 7 && sha.starts_with(&reviewed);
        let full = if matches(&request.tested_sha) {
            request.tested_sha.clone()
        } else if let Some(candidate) = request.candidate_sha.as_ref().filter(|c| matches(c)) {
            candidate.clone()
        } else {
            return Err(invalid(format!(
                "the review is for {reviewed:?}, which is neither the tested commit nor the \
                 current merge commit of this request"
            )));
        };
        verdict.reviewed_sha = full;
        verdict.summary = cut(&verdict.summary, MAX_NOTE_BYTES);
        let approved = verdict.approved;
        let summary = verdict.summary.clone();
        request.verdict = Some(verdict);
        if approved {
            self.save(&mut request)?;
        } else {
            self.cancel_build(&mut request);
            self.finish(
                &mut request,
                LandState::Rejected,
                format!("review rejected: {summary}"),
            )?;
        }
        Ok(request)
    }

    /// Withdraw an unfinished request, freeing its lane. A request whose merge
    /// commit is already in the target is recorded as landed instead.
    pub fn cancel(&self, id: &str, reason: &str) -> Result<LandRequest, MergeQueueError> {
        let _changes = self.changes();
        let mut request = self.open_request(id)?;
        // A crash may have left the ref advanced and the row not yet updated.
        if self.recover_landed(&mut request)? {
            return Ok(request);
        }
        self.cancel_build(&mut request);
        self.finish(
            &mut request,
            LandState::Cancelled,
            cut(reason, MAX_NOTE_BYTES),
        )?;
        Ok(request)
    }

    /// [`Self::cancel`] for a lane that is stuck because the repository cannot
    /// be read (moved or deleted). When whether the change landed cannot be
    /// checked, the request is cancelled and its reason says so. Any other
    /// error, such as one saving a landing that was found, is returned.
    pub fn force_cancel(&self, id: &str, reason: &str) -> Result<LandRequest, MergeQueueError> {
        let _changes = self.changes();
        let mut request = self.open_request(id)?;
        let reason = match self.recover_landed(&mut request) {
            Ok(true) => return Ok(request),
            Ok(false) => reason.to_owned(),
            Err(error @ MergeQueueError::Git(_)) => {
                format!("landing status unverifiable ({error}); {reason}")
            }
            Err(error) => return Err(error),
        };
        self.cancel_build(&mut request);
        self.finish(
            &mut request,
            LandState::Cancelled,
            cut(&reason, MAX_NOTE_BYTES),
        )?;
        Ok(request)
    }

    /// Advance every lane as far as it can go now.
    pub fn tick(&self) -> Vec<StepOutcome> {
        self.sweep_worktrees();
        let lanes = match self.inner.store.open_lanes() {
            Ok(lanes) => lanes,
            Err(error) => {
                tracing::warn!(%error, "merge queue: could not list lanes");
                return Vec::new();
            }
        };
        let mut outcomes = Vec::new();
        for (repo, target_ref) in lanes {
            for _ in 0..MAX_STEPS_PER_TICK {
                match self.step(&repo, &target_ref) {
                    Ok(StepOutcome::Idle) => break,
                    Ok(outcome) => {
                        let stop = matches!(
                            outcome,
                            StepOutcome::Waiting { .. }
                                // A requeued request is merged again next tick.
                                | StepOutcome::Advanced { state: LandState::Queued, .. }
                        );
                        outcomes.push(outcome);
                        if stop {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%repo, %target_ref, %error, "merge queue: step failed");
                        break;
                    }
                }
            }
        }
        outcomes
    }

    /// Make at most one transition on the lane of `target_ref` in `repo` (the
    /// repository key stored on a request).
    pub fn step(&self, repo: &str, target_ref: &str) -> Result<StepOutcome, MergeQueueError> {
        let _changes = self.changes();
        let Some(mut request) = self.inner.store.lane_head(repo, target_ref)? else {
            return Ok(StepOutcome::Idle);
        };
        match request.state {
            LandState::Queued => self.prepare(&mut request),
            LandState::Building => self.check_build(&mut request),
            LandState::AwaitingReview => self.try_land(&mut request),
            _ => Ok(StepOutcome::Idle),
        }
    }

    fn open_request(&self, id: &str) -> Result<LandRequest, MergeQueueError> {
        let request = self.get(id)?;
        if request.state.is_terminal() {
            return Err(invalid(format!(
                "land request {id} already ended ({})",
                request.state.as_str()
            )));
        }
        Ok(request)
    }

    fn save(&self, request: &mut LandRequest) -> Result<(), MergeQueueError> {
        request.updated_at = now_ms();
        self.inner.store.save(request)
    }

    fn advanced(request: &LandRequest) -> StepOutcome {
        StepOutcome::Advanced {
            request_id: request.id.clone(),
            state: request.state,
        }
    }

    /// Record why the request waits; its state does not change.
    fn wait(
        &self,
        request: &mut LandRequest,
        reason: String,
    ) -> Result<StepOutcome, MergeQueueError> {
        if request.reason.as_deref() != Some(reason.as_str()) {
            request.reason = Some(reason.clone());
            self.save(request)?;
        }
        Ok(StepOutcome::Waiting {
            request_id: request.id.clone(),
            reason,
        })
    }

    /// End a request without landing it.
    fn finish(
        &self,
        request: &mut LandRequest,
        state: LandState,
        reason: String,
    ) -> Result<StepOutcome, MergeQueueError> {
        request.state = state;
        request.reason = Some(reason);
        self.save(request)?;
        self.clean_up(request);
        Ok(Self::advanced(request))
    }

    /// Send a request whose target moved back to the queue, in its old place.
    fn requeue(
        &self,
        request: &mut LandRequest,
        why: String,
    ) -> Result<StepOutcome, MergeQueueError> {
        self.cancel_build(request);
        request.attempts += 1;
        if request.attempts >= MAX_ATTEMPTS {
            let reason = format!("gave up after {} attempts: {why}", request.attempts);
            return self.finish(request, LandState::Failed, reason);
        }
        self.remove_worktree(request);
        request.state = LandState::Queued;
        request.reason = Some(why);
        forget_candidate(request);
        self.save(request)?;
        Ok(Self::advanced(request))
    }

    fn scratch_path(&self, request: &LandRequest) -> PathBuf {
        self.inner.root.join(SCRATCH_DIR).join(&request.id)
    }

    /// Remove a request's scratch worktree and only its registration.
    fn remove_worktree(&self, request: &LandRequest) {
        let path = self.scratch_path(request);
        let repo = Path::new(&request.repo);
        let path_text = path.to_string_lossy();
        let remove = ["worktree", "remove", "--force", &path_text];
        let removed = git::run(repo, &remove);
        if let Err(error) = removed
            && path.exists()
        {
            tracing::warn!(%error, "merge queue: removing a scratch worktree by hand");
            // The path is the queue's own, under its root.
            if let Err(error) = std::fs::remove_dir_all(&path) {
                tracing::warn!(%error, "merge queue: scratch worktree left behind");
            }
            // With the folder gone this drops just this registration.
            let _ = git::run(repo, &remove);
        }
    }

    /// Whether the request's build can no longer write to its worktree.
    fn build_ended(&self, request: &LandRequest) -> bool {
        let Some(job_id) = &request.build_job_id else {
            return true;
        };
        match self.inner.builds.get(job_id) {
            Ok(job) => job.state.is_terminal(),
            Err(BuildJobError::NotFound(_)) => true,
            Err(_) => false,
        }
    }

    /// Remove the worktrees of ended requests whose cancelled build has now
    /// stopped.
    fn sweep_worktrees(&self) {
        let Ok(entries) = std::fs::read_dir(self.inner.root.join(SCRATCH_DIR)) else {
            return;
        };
        let _changes = self.changes();
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if let Ok(Some(request)) = self.inner.store.get(&id)
                && request.state.is_terminal()
                && self.build_ended(&request)
            {
                self.remove_worktree(&request);
            }
        }
    }

    fn clean_up(&self, request: &LandRequest) {
        // A build asked to stop may still be writing; the sweep comes back.
        if self.build_ended(request) {
            self.remove_worktree(request);
        }
        let repo = Path::new(&request.repo);
        for name in ["tested", "candidate"] {
            let _ = git::run(repo, &["update-ref", "-d", &pin(&request.id, name)]);
        }
    }

    /// The build of the request's merge commit. Replaying the same operation
    /// returns the same job.
    fn build_job(&self, request: &LandRequest, candidate: &str) -> NewJob {
        let worktree = self.scratch_path(request).to_string_lossy().into_owned();
        NewJob {
            op_id: format!(
                "merge-queue:{}:{}:{candidate}",
                request.id, request.generation
            ),
            chat_id: request.owner.chat_id.clone(),
            space_id: request.owner.space_id.clone(),
            device_id: request.owner.device_id.clone(),
            worktree_key: worktree.clone(),
            cwd: worktree,
            branch: Some(request.target_ref.clone()),
            head_sha: Some(candidate.to_owned()),
            argv: request.build.argv.clone(),
            label: Some(format!("merge queue: {}", request.work_item_id)),
            // The queue polls the job itself.
            wake: false,
            timeout_secs: request.build.timeout_secs,
            integration: true,
            awaiting_approval: request.build.awaiting_approval,
            ..NewJob::default()
        }
    }

    /// Learn the id of a build that may have been enqueued just before a crash,
    /// by replaying its operation. If none had been, the replay makes one, which
    /// the caller then cancels. An error means it is unknown whether one exists.
    fn adopt_job(&self, request: &mut LandRequest) -> Result<(), BuildJobError> {
        if request.state != LandState::Queued || request.build_job_id.is_some() {
            return Ok(());
        }
        let Some(candidate) = request.candidate_sha.clone() else {
            return Ok(());
        };
        match self
            .inner
            .builds
            .enqueue(self.build_job(request, &candidate))
        {
            Ok(enqueued) => {
                request.build_job_id = Some(enqueued.job.id);
                Ok(())
            }
            // A replay is answered before the limits, so no job exists.
            Err(BuildJobError::Limit(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn cancel_build(&self, request: &mut LandRequest) {
        if let Err(error) = self.adopt_job(request) {
            tracing::warn!(%error, "merge queue: could not look for an earlier build");
        }
        let Some(job_id) = &request.build_job_id else {
            return;
        };
        if !self.build_ended(request)
            && let Err(error) = self.inner.builds.cancel(job_id, None)
        {
            tracing::warn!(%error, job = %job_id, "merge queue: could not cancel a build");
        }
    }

    fn prepare(&self, request: &mut LandRequest) -> Result<StepOutcome, MergeQueueError> {
        // A merge commit saved by an earlier try (a full build queue, or a
        // crash around the enqueue) is used again while the target is where it
        // was.
        if let Some(candidate) = request.candidate_sha.clone() {
            let repo = PathBuf::from(&request.repo);
            let current = self.target_for(&repo, request);
            let unmoved = matches!(
                &current,
                Ok((head, creates)) if Some(head) == request.expected_target.as_ref()
                    && *creates == request.creates_target
            );
            if unmoved && !request.discarding {
                if let Err(error) = self.ensure_worktree(request, &candidate) {
                    self.cancel_build(request);
                    let reason = format!("could not restore the merge: {error}");
                    return self.finish(request, LandState::Failed, reason);
                }
                return self.enqueue_build(request, &candidate);
            }
            // The old build must have stopped before its folder is used again.
            if let Err(error) = self.adopt_job(request) {
                return self.wait(request, format!("an earlier build is unknown: {error}"));
            }
            // Saved before the cancel, so a crash cannot bring the job back.
            request.discarding = true;
            self.save(request)?;
            self.cancel_build(request);
            if !self.build_ended(request) {
                return self.wait(
                    request,
                    "waiting for the build of an outdated merge to stop".into(),
                );
            }
            self.remove_worktree(request);
            forget_candidate(request);
            self.save(request)?;
        }
        match self.merge_candidate(request) {
            Ok(None) => {}
            Ok(Some((state, reason))) => return self.finish(request, state, reason),
            Err(error) => {
                let reason = format!("could not prepare the merge: {error}");
                return self.finish(request, LandState::Failed, reason);
            }
        }
        let candidate = request
            .candidate_sha
            .clone()
            .ok_or_else(|| invalid("the merge produced no commit"))?;
        // Saved before the enqueue, so a crash after it finds the same job.
        self.save(request)?;
        self.enqueue_build(request, &candidate)
    }

    fn enqueue_build(
        &self,
        request: &mut LandRequest,
        candidate: &str,
    ) -> Result<StepOutcome, MergeQueueError> {
        match self
            .inner
            .builds
            .enqueue(self.build_job(request, candidate))
        {
            Ok(enqueued) => {
                request.build_job_id = Some(enqueued.job.id);
                request.state = LandState::Building;
                request.reason = None;
                self.save(request)?;
                Ok(Self::advanced(request))
            }
            // The queue is full or its store is unwell: try again later.
            Err(error @ (BuildJobError::Limit(_) | BuildJobError::Store(_))) => {
                self.wait(request, format!("the build could not be queued: {error}"))
            }
            Err(error) => {
                let reason = format!("the build was refused: {error}");
                self.finish(request, LandState::Failed, reason)
            }
        }
    }

    fn target_for(
        &self,
        repo: &Path,
        request: &LandRequest,
    ) -> Result<(String, bool), MergeQueueError> {
        let existed = request.target_existed
            || self
                .inner
                .store
                .has_landed(&request.repo, &request.target_ref)?;
        current_target(repo, &request.target_ref, &request.base_ref, existed)
    }

    /// The scratch worktree at `candidate`, made again if it is gone.
    fn ensure_worktree(
        &self,
        request: &LandRequest,
        candidate: &str,
    ) -> Result<(), MergeQueueError> {
        let path = self.scratch_path(request);
        if path.exists() && git::commit_id(&path, "HEAD")?.as_deref() == Some(candidate) {
            return Ok(());
        }
        self.remove_worktree(request);
        self.add_worktree(request, candidate)
    }

    fn add_worktree(&self, request: &LandRequest, commit: &str) -> Result<(), MergeQueueError> {
        let worktree = self.scratch_path(request);
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MergeQueueError::Git(e.to_string()))?;
        }
        let worktree = worktree.to_string_lossy();
        // `--force`: a crash can leave the path registered with its folder gone.
        git::run(
            Path::new(&request.repo),
            &["worktree", "add", "--force", "--detach", &worktree, commit],
        )?;
        Ok(())
    }

    /// Make the merge commit in a fresh scratch worktree and note it on the
    /// request. `Some` is a reason the request cannot land.
    fn merge_candidate(
        &self,
        request: &mut LandRequest,
    ) -> Result<Option<(LandState, String)>, MergeQueueError> {
        let repo = PathBuf::from(&request.repo);
        let (target, creates) = self.target_for(&repo, request)?;
        request.target_existed |= !creates;
        let paths = git::merge_tree_conflicts(&repo, &target, &request.tested_sha)?;
        let conflicted = !paths.is_empty();
        let reason = conflicted.then(|| conflict_reason(&paths));
        request.conflict_check = Some(ConflictCheck {
            target_sha: target.clone(),
            paths,
        });
        if let Some(reason) = reason {
            return Ok(Some((LandState::Conflicted, reason)));
        }
        if git::is_ancestor(&repo, &request.tested_sha, &target)? {
            return Ok(Some((
                LandState::Failed,
                "the tested commit is already in the target".into(),
            )));
        }
        // A crash may have left an earlier attempt's worktree behind.
        self.remove_worktree(request);
        self.add_worktree(request, &target)?;
        let worktree = self.scratch_path(request);
        let merge = git::output(
            &worktree,
            &["merge", "--no-ff", "--no-commit", &request.tested_sha],
        )?;
        if !merge.status.success() {
            let paths = git::unmerged_paths(&worktree)?;
            if paths.is_empty() {
                return Err(MergeQueueError::Git(format!(
                    "git merge failed: {}",
                    String::from_utf8_lossy(&merge.stderr).trim()
                )));
            }
            let reason = conflict_reason(&paths);
            request.conflict_check = Some(ConflictCheck {
                target_sha: target,
                paths,
            });
            return Ok(Some((LandState::Conflicted, reason)));
        }
        for path in changelog::fold(
            &worktree,
            &request.work_item_id,
            request.changelog_fragment.as_deref(),
        )? {
            let path = path.to_string_lossy();
            // `-f`: an ignored changelog path must still be part of the commit.
            git::run(&worktree, &["add", "-A", "-f", "--", &path])?;
        }
        let message = format!(
            "Land {} ({})\n\nMast-Work-Item: {}\nMast-Tested-Commit: {}",
            request.work_item_id, request.branch, request.work_item_id, request.tested_sha
        );
        git::run(&worktree, &["commit", "--no-verify", "-m", &message])?;
        let candidate = git::run(&worktree, &["rev-parse", "HEAD"])?;
        git::run(
            &repo,
            &["update-ref", &pin(&request.id, "candidate"), &candidate],
        )?;
        request.candidate_sha = Some(candidate);
        request.expected_target = Some(target);
        request.creates_target = creates;
        Ok(None)
    }

    fn check_build(&self, request: &mut LandRequest) -> Result<StepOutcome, MergeQueueError> {
        let Some(job_id) = request.build_job_id.clone() else {
            return self.finish(
                request,
                LandState::Failed,
                "the build job is unknown".into(),
            );
        };
        let job = match self.inner.builds.get(&job_id) {
            Ok(job) => job,
            Err(error @ BuildJobError::NotFound(_)) => {
                return self.finish(request, LandState::Failed, error.to_string());
            }
            Err(error) => return self.wait(request, error.to_string()),
        };
        if !job.state.is_terminal() {
            return self.wait(
                request,
                format!("build job {job_id} is {}", job.state.as_str()),
            );
        }
        if job.state != JobState::Succeeded {
            let mut reason = format!("build job {job_id} ended {}", job.state.as_str());
            if let Some(code) = job.exit_code {
                reason.push_str(&format!(" (exit {code})"));
            }
            if let Some(why) = &job.reason {
                reason.push_str(&format!(": {why}"));
            }
            return self.finish(request, LandState::Failed, reason);
        }
        // The merge commit stays pinned; the files are no longer needed.
        self.remove_worktree(request);
        request.state = LandState::AwaitingReview;
        request.reason = None;
        self.save(request)?;
        Ok(Self::advanced(request))
    }

    /// Finish a request whose ref already holds its merge commit (a crash
    /// between the ref update and the store write).
    fn recover_landed(&self, request: &mut LandRequest) -> Result<bool, MergeQueueError> {
        if request.state != LandState::AwaitingReview || !approved(request) {
            return Ok(false);
        }
        let Some(candidate) = request.candidate_sha.clone() else {
            return Ok(false);
        };
        let repo = PathBuf::from(&request.repo);
        let contains = match git::commit_id(&repo, &request.target_ref)? {
            Some(head) => git::is_ancestor(&repo, &candidate, &head)?,
            None => false,
        };
        if contains {
            self.mark_landed(request, candidate)?;
        }
        Ok(contains)
    }

    fn mark_landed(
        &self,
        request: &mut LandRequest,
        landed_sha: String,
    ) -> Result<(), MergeQueueError> {
        request.state = LandState::Landed;
        request.reason = None;
        request.updated_at = now_ms();
        let landing = LandedRecord {
            request_id: request.id.clone(),
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            repo: request.repo.clone(),
            target_ref: request.target_ref.clone(),
            tested_sha: request.tested_sha.clone(),
            landed_sha,
            doc_deltas: request.doc_deltas.clone(),
            landed_at: request.updated_at,
        };
        self.inner.store.save_landed(request, &landing)?;
        let hook = self.inner.land_hook.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(hook) = hook {
            hook(request, &landing);
        }
        self.clean_up(request);
        Ok(())
    }

    fn try_land(&self, request: &mut LandRequest) -> Result<StepOutcome, MergeQueueError> {
        if !approved(request) {
            return self.wait(request, "waiting for a review verdict".into());
        }
        if self.recover_landed(request)? {
            return Ok(Self::advanced(request));
        }
        let (Some(candidate), Some(expected)) = (
            request.candidate_sha.clone(),
            request.expected_target.clone(),
        ) else {
            return self.finish(
                request,
                LandState::Failed,
                "the merge commit is unknown".into(),
            );
        };
        let repo = PathBuf::from(&request.repo);
        if let Some(user) = git::branch_in_use(&repo, &request.target_ref)? {
            let reason = format!(
                "{} is {user}; moving it would leave that worktree stale",
                request.target_ref
            );
            return self.wait(request, reason);
        }
        // All zeros, as long as this repository's ids: "must not exist yet".
        let absent = "0".repeat(candidate.len());
        let old = if request.creates_target {
            &absent
        } else {
            &expected
        };
        let log = format!("mast merge-queue: land {}", request.work_item_id);
        match git::compare_and_swap(&repo, &request.target_ref, &candidate, old, &log)? {
            Ok(()) => {
                self.mark_landed(request, candidate)?;
                Ok(Self::advanced(request))
            }
            Err(refusal) => {
                let now = git::commit_id(&repo, &request.target_ref)?;
                let unmoved = match &now {
                    Some(head) => !request.creates_target && *head == expected,
                    None => request.creates_target,
                };
                if unmoved {
                    // For example a stale ref lock: nothing to merge again.
                    self.wait(request, format!("the ref could not be updated: {refusal}"))
                } else {
                    let why = format!(
                        "{} moved from {expected} to {}; merging again",
                        request.target_ref,
                        now.as_deref().unwrap_or("nothing")
                    );
                    self.requeue(request, why)
                }
            }
        }
    }
}

fn pin(request_id: &str, name: &str) -> String {
    format!("{REF_PREFIX}/{request_id}/{name}")
}

/// Whether an approval covers what would land now: the tested commit, or the
/// current merge commit.
fn approved(request: &LandRequest) -> bool {
    request.verdict.as_ref().is_some_and(|v| {
        v.approved
            && (v.reviewed_sha == request.tested_sha
                || Some(&v.reviewed_sha) == request.candidate_sha.as_ref())
    })
}

/// Give up the merge commit, along with a review of that commit alone.
fn forget_candidate(request: &mut LandRequest) {
    if request
        .verdict
        .as_ref()
        .is_some_and(|v| v.reviewed_sha != request.tested_sha)
    {
        request.verdict = None;
    }
    request.generation += 1;
    request.discarding = false;
    request.candidate_sha = None;
    request.expected_target = None;
    request.creates_target = false;
    request.build_job_id = None;
}

/// The commit a land would build on, and whether the target ref has to be
/// created. Only a `mast/int/...` branch that never `existed` may be created:
/// recreating a deleted one from a base could drop changes recorded as landed.
fn current_target(
    repo: &Path,
    target_ref: &str,
    base_ref: &str,
    existed: bool,
) -> Result<(String, bool), MergeQueueError> {
    if let Some(head) = git::commit_id(repo, target_ref)? {
        return Ok((head, false));
    }
    if existed {
        return Err(invalid(format!(
            "target ref {target_ref} was deleted; restore it before landing on it"
        )));
    }
    if !target_ref.starts_with("refs/heads/mast/int/") {
        return Err(invalid(format!("target ref {target_ref} does not exist")));
    }
    let base = git::commit_id(repo, base_ref)?
        .ok_or_else(|| invalid(format!("baseRef {base_ref:?} is not a commit here")))?;
    Ok((base, true))
}

fn conflict_reason(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(10).map(String::as_str).collect();
    let more = paths.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!("merge conflicts in {}{suffix}", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_main_and_integration_branches_may_be_targets() {
        for (given, full) in [
            ("main", "refs/heads/main"),
            ("refs/heads/main", "refs/heads/main"),
            ("mast/int/proj/lead-1", "refs/heads/mast/int/proj/lead-1"),
        ] {
            assert_eq!(normalize_target_ref(given).unwrap(), full);
        }
        for bad in [
            "",
            "master",
            "/main",
            "refs/tags/main",
            "mast/int/proj",
            "mast/int/proj/lead/extra",
            "mast/int/../main",
            "mast/int/proj/-x",
            "mast/int/proj/x.lock",
            "mast/int/proj/a b",
        ] {
            assert!(normalize_target_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn work_item_ids_must_be_safe_file_names() {
        assert!(safe_id("cadfc6b3-bfed_1"));
        for bad in ["", "../x", "a/b", "a.md", &"a".repeat(129)] {
            assert!(!safe_id(bad), "{bad}");
        }
    }
}
