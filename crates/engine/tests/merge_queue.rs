//! The merge queue against real temporary git repositories and a real build
//! queue whose executor is a fake (no Cargo, no network).

#![allow(clippy::disallowed_methods)] // test repositories are built with plain `git`

use mast_engine::build_jobs::{
    BuildJobs, BuildJobsConfig, DiskProbe, Executor, ExitRecord, HostProfile, Job, ListFilter,
    NewJob, Probe, StartCtx, StartError, Started, StopError,
};
use mast_engine::merge_queue::{
    BuildOwner, BuildSpec, DocDelta, LandRequest, LandState, MergeQueue, NewLandRequest,
    ReviewVerdict, StepOutcome,
};
use mast_engine::review_dispatch::{ReviewerCandidate, select_reviewer};
use mast_proto::HarnessId;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LEAD: &str = "mast/int/proj/lead";
const LEAD_REF: &str = "refs/heads/mast/int/proj/lead";

struct Disk;
impl DiskProbe for Disk {
    fn free_gib(&self, _: &HostProfile) -> Result<f64, String> {
        Ok(1000.0)
    }
}

/// Ends every job with `exit` once one is set, and records the files each job
/// was started over.
#[derive(Default)]
struct Builder {
    exit: Mutex<Option<i32>>,
    /// The `CHANGELOG.md` (or empty) of every tree a job started in.
    trees: Mutex<Vec<(PathBuf, String)>>,
}

impl Executor for Builder {
    fn start(&self, ctx: &StartCtx<'_>) -> Result<Started, StartError> {
        let cwd = PathBuf::from(&ctx.job.cwd);
        let changelog = std::fs::read_to_string(cwd.join("CHANGELOG.md")).unwrap_or_default();
        self.trees.lock().unwrap().push((cwd, changelog));
        Ok(Started {
            pid: Some(1),
            ..Started::default()
        })
    }
    fn probe(&self, _: &Job) -> Probe {
        match *self.exit.lock().unwrap() {
            Some(code) => Probe::Exited(ExitRecord {
                version: 1,
                exit_code: Some(code),
                ..ExitRecord::default()
            }),
            None => Probe::Running {
                waiting_for_lock: false,
                pid: None,
            },
        }
    }
    fn stop(&self, _: &Job, _: u32) -> Result<(), StopError> {
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
    base: String,
    builder: Arc<Builder>,
    builds: BuildJobs,
    queue: MergeQueue,
}

fn open(root: &Path, builder: &Arc<Builder>) -> (BuildJobs, MergeQueue) {
    let config = BuildJobsConfig::new(
        root.join("builds"),
        vec![HostProfile::local(root.to_path_buf())],
    );
    let builds = BuildJobs::builder(config)
        .executor("local", builder.clone())
        .disk(Arc::new(Disk))
        .open()
        .unwrap();
    let queue = MergeQueue::open(&root.join("queue"), builds.clone()).unwrap();
    (builds, queue)
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let root = temp.path().join("engine");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let mut all = vec![("a.txt", "one\n")];
        all.extend_from_slice(files);
        let base = commit(&repo, &all, "base");
        let builder = Arc::new(Builder::default());
        let (builds, queue) = open(&root, &builder);
        Self {
            _temp: temp,
            repo,
            root,
            base,
            builder,
            builds,
            queue,
        }
    }

    /// A commit on a new branch off the base; `main` stays checked out.
    fn branch(&self, name: &str, files: &[(&str, &str)]) -> String {
        git(&self.repo, &["checkout", "-q", "-b", name, &self.base]);
        let sha = commit(&self.repo, files, name);
        git(&self.repo, &["checkout", "-q", "main"]);
        sha
    }

    fn request(&self, item: &str, sha: &str, fragment: Option<&str>) -> NewLandRequest {
        NewLandRequest {
            repo: self.repo.clone(),
            project_id: "proj".into(),
            work_item_id: item.into(),
            branch: format!("work/{item}"),
            tested_sha: sha.into(),
            base_ref: self.base.clone(),
            target_ref: LEAD.into(),
            changelog_fragment: fragment.map(str::to_owned),
            doc_deltas: vec![DocDelta {
                path: "ARCHITECTURE.md".into(),
                note: format!("note from {item}"),
            }],
            build: BuildSpec {
                argv: vec!["true".into()],
                timeout_secs: None,
                awaiting_approval: false,
            },
            owner: BuildOwner {
                chat_id: "chat".into(),
                space_id: "space".into(),
                device_id: "device".into(),
            },
        }
    }

    /// Run the merge queue and the build queue until neither moves.
    fn drive(&self) {
        for _ in 0..4 {
            self.queue.tick();
            self.builds.tick();
            self.builds.tick();
        }
    }

    fn state(&self, request: &LandRequest) -> LandRequest {
        self.queue.get(&request.id).unwrap()
    }

    fn target(&self) -> Option<String> {
        let out = git(
            &self.repo,
            &["for-each-ref", "--format=%(objectname)", LEAD_REF],
        );
        (!out.is_empty()).then_some(out)
    }

    fn show(&self, revision: &str) -> String {
        git(&self.repo, &["show", revision])
    }

    /// Nothing of the queue's is left in the repository.
    fn assert_clean(&self) {
        let worktrees = git(&self.repo, &["worktree", "list", "--porcelain"]);
        assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
        assert_eq!(git(&self.repo, &["for-each-ref", "refs/mast"]), "");
        assert_eq!(git(&self.repo, &["status", "--porcelain"]), "");
    }
}

fn approve(sha: &str) -> ReviewVerdict {
    ReviewVerdict {
        approved: true,
        reviewed_sha: sha.into(),
        reviewer_chat_id: "reviewer".into(),
        summary: "looks right".into(),
    }
}

#[test]
fn a_clean_merge_lands_after_build_and_review() {
    let f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f
        .queue
        .submit(f.request("item-1", &sha[..12], None))
        .unwrap();
    assert_eq!(request.state, LandState::Queued);
    assert_eq!(request.tested_sha, sha);
    assert!(request.conflict_check.as_ref().unwrap().is_clean());

    f.drive();
    let building = f.state(&request);
    assert_eq!(building.state, LandState::Building, "{:?}", building.reason);
    assert_eq!(f.target(), None);

    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    let waiting = f.state(&request);
    assert_eq!(
        waiting.state,
        LandState::AwaitingReview,
        "{:?}",
        waiting.reason
    );
    assert_eq!(f.target(), None, "no verdict, no land");
    assert_eq!(f.queue.landed("item-1").unwrap(), None);

    assert!(
        f.queue
            .record_verdict(&request.id, approve("abcdef1234"))
            .is_err()
    );
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    f.drive();
    let landed = f.state(&request);
    assert_eq!(landed.state, LandState::Landed, "{:?}", landed.reason);
    let candidate = landed.candidate_sha.clone().unwrap();
    assert_eq!(f.target().as_deref(), Some(candidate.as_str()));
    assert_eq!(
        git(&f.repo, &["log", "-1", "--format=%P", &candidate]),
        format!("{} {sha}", f.base)
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), f.base);
    let record = f.queue.landed("item-1").unwrap().unwrap();
    assert_eq!(record.landed_sha, candidate);
    assert_eq!(record.target_ref, LEAD_REF);
    assert_eq!(record.doc_deltas[0].note, "note from item-1");
    assert!(f.queue.cancel(&request.id, "late").is_err());
    // Submitting the landed commit again is the same request.
    let again = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    assert_eq!(again.id, request.id);
    f.assert_clean();
}

#[test]
fn a_conflict_is_reported_on_submit_without_touching_anything() {
    let f = Fixture::new(&[]);
    let theirs = f.branch("theirs", &[("a.txt", "theirs\n")]);
    let ours = f.branch("ours", &[("a.txt", "ours\n")]);
    git(&f.repo, &["update-ref", LEAD_REF, &ours]);
    let request = f.queue.submit(f.request("item-1", &theirs, None)).unwrap();
    assert_eq!(request.state, LandState::Conflicted);
    let check = request.conflict_check.clone().unwrap();
    assert_eq!(check.paths, vec!["a.txt".to_string()]);
    assert_eq!(check.target_sha, ours);
    assert!(request.reason.unwrap().contains("a.txt"));
    assert!(f.queue.tick().is_empty());
    assert_eq!(f.target(), Some(ours));
    assert!(f.builds.list(&ListFilter::default()).unwrap().is_empty());
    f.assert_clean();
}

#[test]
fn a_moved_target_fails_the_swap_and_requeues_without_clobbering() {
    let f = Fixture::new(&[]);
    git(&f.repo, &["update-ref", LEAD_REF, &f.base]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let other = f.branch("other", &[("other.txt", "2\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    let waiting = f.state(&request);
    assert_eq!(waiting.state, LandState::AwaitingReview);
    let stale = waiting.candidate_sha.clone().unwrap();

    // Someone else advances the target after the build.
    git(&f.repo, &["update-ref", LEAD_REF, &other]);
    // A review of the stale merge commit does not carry over to the next one.
    f.queue
        .record_verdict(&request.id, approve(&stale))
        .unwrap();
    let outcomes = f.queue.tick();
    assert_eq!(
        outcomes,
        vec![StepOutcome::Advanced {
            request_id: request.id.clone(),
            state: LandState::Queued
        }]
    );
    assert_eq!(f.target(), Some(other.clone()), "the ref is not clobbered");
    let queued = f.state(&request);
    assert_eq!((queued.attempts, queued.verdict.clone()), (1, None));
    assert!(queued.reason.unwrap().contains("moved"));

    f.drive();
    assert_eq!(f.state(&request).state, LandState::AwaitingReview);
    assert_eq!(f.target(), Some(other.clone()));
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    f.drive();
    let landed = f.state(&request);
    assert_eq!(landed.state, LandState::Landed, "{:?}", landed.reason);
    let head = f.target().unwrap();
    assert_ne!(head, stale);
    assert_eq!(f.show(&format!("{head}:one.txt")), "1");
    assert_eq!(f.show(&format!("{head}:other.txt")), "2");
    f.assert_clean();
}

#[test]
fn a_failed_build_leaves_the_target_and_frees_the_lane() {
    let f = Fixture::new(&[]);
    git(&f.repo, &["update-ref", LEAD_REF, &f.base]);
    let bad = f.branch("bad", &[("bad.txt", "x\n")]);
    let next = f.branch("next", &[("next.txt", "y\n")]);
    let first = f.queue.submit(f.request("item-1", &bad, None)).unwrap();
    let second = f.queue.submit(f.request("item-2", &next, None)).unwrap();
    // An early approval does not excuse the build.
    f.queue.record_verdict(&first.id, approve(&bad)).unwrap();
    f.drive();
    assert_eq!(f.state(&first).state, LandState::Building);
    assert_eq!(
        f.state(&second).state,
        LandState::Queued,
        "one land per target"
    );

    *f.builder.exit.lock().unwrap() = Some(101);
    f.builds.tick();
    f.queue.tick();
    let failed = f.state(&first);
    assert_eq!(failed.state, LandState::Failed);
    assert!(failed.reason.unwrap().contains("exit 101"));
    assert_eq!(f.target(), Some(f.base.clone()));
    assert_eq!(f.queue.landed("item-1").unwrap(), None);
    assert_eq!(f.state(&second).state, LandState::Building);
    // A failed commit may be tried again.
    let retry = f.queue.submit(f.request("item-1", &bad, None)).unwrap();
    assert_ne!(retry.id, first.id);
    f.queue.cancel(&retry.id, "not needed").unwrap();
    f.queue.cancel(&second.id, "not needed").unwrap();
    f.assert_clean();
}

#[test]
fn a_rejected_review_leaves_the_target() {
    let f = Fixture::new(&[]);
    git(&f.repo, &["update-ref", LEAD_REF, &f.base]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    let choice = select_reviewer(
        HarnessId::ClaudeCode,
        None,
        &[ReviewerCandidate {
            harness: HarnessId::ClaudeCode,
            model: None,
        }],
    )
    .unwrap();
    f.queue
        .assign_reviewer(&request.id, choice.clone())
        .unwrap();
    assert_eq!(f.state(&request).reviewer, Some(choice));
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    let mut verdict = approve(&sha);
    verdict.approved = false;
    verdict.summary = "drops data".into();
    f.queue.record_verdict(&request.id, verdict).unwrap();
    f.drive();
    let rejected = f.state(&request);
    assert_eq!(rejected.state, LandState::Rejected);
    assert!(rejected.reason.unwrap().contains("drops data"));
    assert_eq!(f.target(), Some(f.base.clone()));
    assert_eq!(f.queue.landed("item-1").unwrap(), None);
    // Submitting it again does not escape the rejection.
    let again = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    assert_eq!(
        (again.id, again.state),
        (request.id.clone(), LandState::Rejected)
    );
    assert!(f.queue.record_verdict(&request.id, approve(&sha)).is_err());
    f.assert_clean();
}

#[test]
fn two_changelog_fragments_fold_into_the_built_commits() {
    let f = Fixture::new(&[(
        "CHANGELOG.md",
        "# Changelog\n\n## Unreleased\n\n## 0.1\n\n- Old\n",
    )]);
    // One fragment comes with the request, one as the branch's own file.
    let one = f.branch("one", &[("one.txt", "1\n")]);
    let two = f.branch(
        "two",
        &[
            ("two.txt", "2\n"),
            ("docs/changelog.d/item-2.md", "Add two.\n"),
        ],
    );
    let first = f
        .queue
        .submit(f.request("item-1", &one, Some("Add one.")))
        .unwrap();
    let second = f.queue.submit(f.request("item-2", &two, None)).unwrap();
    for (request, sha) in [(&first, &one), (&second, &two)] {
        f.queue.record_verdict(&request.id, approve(sha)).unwrap();
    }
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    f.drive();
    for request in [&first, &second] {
        let landed = f.state(request);
        assert_eq!(landed.state, LandState::Landed, "{:?}", landed.reason);
    }
    let head = f.target().unwrap();
    let expected = "# Changelog\n\n## Unreleased\n\n- Add one.\n- Add two.\n\n## 0.1\n\n- Old";
    assert_eq!(f.show(&format!("{head}:CHANGELOG.md")), expected);
    assert_eq!(
        git(&f.repo, &["ls-tree", "-r", "--name-only", &head, "docs"]),
        "",
        "the folded fragment file is removed"
    );
    // What was built already had the folded changelog.
    let trees = f.builder.trees.lock().unwrap();
    assert_eq!(trees.len(), 2);
    assert_eq!(trees[1].1.trim_end(), expected);
    drop(trees);
    f.assert_clean();
}

#[test]
fn a_repository_without_a_changelog_keeps_fragment_files() {
    let f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f
        .queue
        .submit(f.request("item-1", &sha, Some("Add one.")))
        .unwrap();
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    let head = f.target().unwrap();
    assert_eq!(
        f.show(&format!("{head}:docs/changelog.d/item-1.md")),
        "- Add one."
    );
    f.assert_clean();
}

#[test]
fn the_queue_survives_a_reopen() {
    let mut f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.drive();
    let before = f.state(&request);
    assert_eq!(before.state, LandState::Building);

    let (builds, queue) = open(&f.root, &f.builder);
    f.builds = builds;
    f.queue = queue;
    assert_eq!(f.queue.list().unwrap(), vec![before]);
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();

    let (builds, queue) = open(&f.root, &f.builder);
    f.builds = builds;
    f.queue = queue;
    f.drive();
    assert_eq!(f.state(&request).state, LandState::Landed);

    let (_, queue) = open(&f.root, &f.builder);
    let record = queue.landed("item-1").unwrap().unwrap();
    assert_eq!(Some(record.landed_sha), f.target());
    f.assert_clean();
}

#[test]
fn a_checked_out_target_is_not_moved_until_it_is_free() {
    let f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let mut new = f.request("item-1", &sha, None);
    new.target_ref = "main".into();
    let request = f.queue.submit(new).unwrap();
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    let held = f.state(&request);
    assert_eq!(held.state, LandState::AwaitingReview);
    assert!(held.reason.unwrap().contains("checked out"));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), f.base);

    git(&f.repo, &["checkout", "-q", "--detach"]);
    f.drive();
    let landed = f.state(&request);
    assert_eq!(landed.state, LandState::Landed, "{:?}", landed.reason);
    assert_eq!(
        Some(git(&f.repo, &["rev-parse", "main"])),
        landed.candidate_sha
    );
    f.assert_clean();
}

/// Whatever the exit status.
fn try_git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap()
        .status
        .success()
}

/// Put the row back to what it was between the enqueue and the store write.
fn forget_the_enqueue(f: &Fixture, id: &str) {
    let db = rusqlite::Connection::open(f.root.join("queue/merge-queue.sqlite3")).unwrap();
    let changed = db
        .execute(
            "UPDATE requests SET state='queued',
               data=json_set(data, '$.state', 'queued', '$.buildJobId', NULL) WHERE id=?1",
            [id],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

#[test]
fn a_crash_after_the_enqueue_finds_the_same_build() {
    let mut f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.queue.tick();
    let before = f.state(&request);
    assert_eq!(before.state, LandState::Building);
    forget_the_enqueue(&f, &request.id);

    let (builds, queue) = open(&f.root, &f.builder);
    f.builds = builds;
    f.queue = queue;
    f.queue.tick();
    let after = f.state(&request);
    assert_eq!(after.state, LandState::Building, "{:?}", after.reason);
    assert_eq!(after.candidate_sha, before.candidate_sha);
    assert_eq!(after.build_job_id, before.build_job_id);
    assert_eq!(f.builds.list(&ListFilter::default()).unwrap().len(), 1);
    f.queue.cancel(&request.id, "done").unwrap();
    f.assert_clean();
}

#[test]
fn an_outdated_build_stops_before_its_folder_is_used_again() {
    let f = Fixture::new(&[]);
    git(&f.repo, &["update-ref", LEAD_REF, &f.base]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let other = f.branch("other", &[("other.txt", "2\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.queue.tick();
    f.builds.tick();
    let before = f.state(&request);
    let old_job = before.build_job_id.clone().unwrap();
    let scratch = f.builder.trees.lock().unwrap()[0].0.clone();
    forget_the_enqueue(&f, &request.id);
    git(&f.repo, &["update-ref", LEAD_REF, &other]);

    f.queue.tick();
    let waiting = f.state(&request);
    assert_eq!(waiting.state, LandState::Queued);
    assert!(waiting.reason.unwrap().contains("outdated merge to stop"));
    assert_eq!(waiting.candidate_sha, before.candidate_sha);
    assert!(scratch.exists());
    assert_eq!(f.builds.list(&ListFilter::default()).unwrap().len(), 1);

    *f.builder.exit.lock().unwrap() = Some(1);
    f.builds.tick();
    assert!(f.builds.get(&old_job).unwrap().state.is_terminal());
    *f.builder.exit.lock().unwrap() = None;
    f.queue.tick();
    let rebuilt = f.state(&request);
    assert_eq!(rebuilt.state, LandState::Building, "{:?}", rebuilt.reason);
    assert_ne!(rebuilt.candidate_sha, before.candidate_sha);
    assert_ne!(rebuilt.build_job_id, Some(old_job));
    f.queue.cancel(&request.id, "done").unwrap();
    f.assert_clean();
}

#[test]
fn a_cancelled_builds_folder_stays_until_the_build_ends() {
    let f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.queue.tick();
    f.builds.tick();
    let scratch = f.builder.trees.lock().unwrap()[0].0.clone();
    f.queue.cancel(&request.id, "withdrawn").unwrap();
    f.queue.tick();
    assert!(scratch.exists(), "the build may still be writing");
    *f.builder.exit.lock().unwrap() = Some(1);
    f.builds.tick();
    f.queue.tick();
    assert!(!scratch.exists());
    f.assert_clean();
}

#[test]
fn a_full_build_queue_is_retried_with_the_same_merge() {
    let f = Fixture::new(&[]);
    let fillers: Vec<String> = (0..4)
        .map(|i| {
            f.builds
                .enqueue(NewJob {
                    op_id: format!("filler-{i}"),
                    chat_id: "chat".into(),
                    space_id: "space".into(),
                    device_id: "device".into(),
                    worktree_key: format!("filler-{i}"),
                    cwd: f.repo.to_string_lossy().into_owned(),
                    argv: vec!["true".into(), i.to_string()],
                    ..NewJob::default()
                })
                .unwrap()
                .job
                .id
        })
        .collect();
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.queue.tick();
    let held = f.state(&request);
    assert_eq!(held.state, LandState::Queued);
    assert!(held.reason.unwrap().contains("could not be queued"));
    for id in fillers {
        f.builds.cancel(&id, None).unwrap();
    }
    f.queue.tick();
    let building = f.state(&request);
    assert_eq!(building.state, LandState::Building, "{:?}", building.reason);
    assert_eq!(building.candidate_sha, held.candidate_sha);
    f.queue.cancel(&request.id, "done").unwrap();
    f.assert_clean();
}

/// A request built, approved and ready to move `LEAD_REF`.
fn ready_to_land(f: &Fixture) -> LandRequest {
    git(&f.repo, &["update-ref", LEAD_REF, &f.base]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    f.state(&request)
}

#[test]
fn a_ref_moved_before_the_store_write_is_recorded_as_landed() {
    let f = Fixture::new(&[]);
    let ready = ready_to_land(&f);
    let candidate = ready.candidate_sha.clone().unwrap();
    git(&f.repo, &["update-ref", LEAD_REF, &candidate]);
    // Even a cancel cannot deny what is already in the target.
    let after = f.queue.cancel(&ready.id, "too late").unwrap();
    assert_eq!(after.state, LandState::Landed);
    assert_eq!(
        f.queue.landed("item-1").unwrap().unwrap().landed_sha,
        candidate
    );
    f.assert_clean();
}

#[test]
fn a_refused_ref_update_waits_when_the_target_has_not_moved() {
    let f = Fixture::new(&[]);
    let ready = ready_to_land(&f);
    let lock = f.repo.join(".git").join(format!("{LEAD_REF}.lock"));
    std::fs::write(&lock, "").unwrap();
    f.queue.tick();
    let held = f.state(&ready);
    assert_eq!((held.state, held.attempts), (LandState::AwaitingReview, 0));
    assert!(held.reason.unwrap().contains("could not be updated"));
    std::fs::remove_file(lock).unwrap();
    f.queue.tick();
    assert_eq!(f.state(&ready).state, LandState::Landed);
    f.assert_clean();
}

#[test]
fn a_branch_held_by_a_rebase_or_bisect_is_not_moved() {
    let f = Fixture::new(&[]);
    let lead = f.branch("lead-work", &[("a.txt", "lead\n")]);
    f.branch("clash", &[("a.txt", "clash\n")]);
    git(&f.repo, &["update-ref", LEAD_REF, &lead]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    f.queue.record_verdict(&request.id, approve(&sha)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);

    let other = f.root.join("user-worktree");
    git(
        &f.repo,
        &["worktree", "add", "-q", other.to_str().unwrap(), LEAD],
    );
    assert!(!try_git(&other, &["rebase", "clash"]), "the rebase pauses");
    let listing = git(&f.repo, &["worktree", "list", "--porcelain"]);
    assert!(!listing.contains(LEAD_REF), "{listing}");
    f.drive();
    let held = f.state(&request);
    assert_eq!(held.state, LandState::AwaitingReview, "{:?}", held.reason);
    assert!(held.reason.unwrap().contains("rebase"));
    assert_eq!(f.target(), Some(lead.clone()));

    git(&other, &["rebase", "--abort"]);
    git(&other, &["bisect", "start"]);
    git(&other, &["checkout", "-q", "--detach"]);
    f.drive();
    assert!(f.state(&request).reason.unwrap().contains("bisect"));
    assert_eq!(f.target(), Some(lead));

    git(
        &f.repo,
        &["worktree", "remove", "--force", other.to_str().unwrap()],
    );
    f.drive();
    let landed = f.state(&request);
    assert_eq!(landed.state, LandState::Landed, "{:?}", landed.reason);
    f.assert_clean();
}

#[test]
fn a_conflict_that_appears_after_an_earlier_land_is_reported() {
    let f = Fixture::new(&[]);
    let one = f.branch("one", &[("a.txt", "first\n")]);
    let two = f.branch("two", &[("a.txt", "second\n")]);
    let first = f.queue.submit(f.request("item-1", &one, None)).unwrap();
    let second = f.queue.submit(f.request("item-2", &two, None)).unwrap();
    assert_eq!(second.state, LandState::Queued);
    f.queue.record_verdict(&first.id, approve(&one)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    f.drive();
    assert_eq!(f.state(&first).state, LandState::Landed);
    let late = f.state(&second);
    assert_eq!(late.state, LandState::Conflicted, "{:?}", late.reason);
    assert_eq!(
        late.conflict_check.unwrap().paths,
        vec!["a.txt".to_string()]
    );
    assert_eq!(f.target(), f.state(&first).candidate_sha);
    f.assert_clean();
}

#[test]
fn a_deleted_integration_branch_is_not_made_again() {
    let f = Fixture::new(&[]);
    let one = f.branch("one", &[("one.txt", "1\n")]);
    let two = f.branch("two", &[("two.txt", "2\n")]);
    let first = f.queue.submit(f.request("item-1", &one, None)).unwrap();
    let second = f.queue.submit(f.request("item-2", &two, None)).unwrap();
    f.queue.record_verdict(&first.id, approve(&one)).unwrap();
    *f.builder.exit.lock().unwrap() = Some(0);
    for _ in 0..6 {
        if f.state(&first).state == LandState::Landed {
            break;
        }
        // One step at a time, so the second request has not been merged yet.
        let repo = first.repo.clone();
        f.queue.step(&repo, LEAD_REF).unwrap();
        f.builds.tick();
        f.builds.tick();
    }
    assert_eq!(f.state(&first).state, LandState::Landed);
    git(&f.repo, &["update-ref", "-d", LEAD_REF]);
    f.drive();
    let failed = f.state(&second);
    assert_eq!(failed.state, LandState::Failed, "{:?}", failed.reason);
    assert!(failed.reason.unwrap().contains("was deleted"));
    assert_eq!(f.target(), None);
    assert!(f.queue.submit(f.request("item-2", &two, None)).is_err());
    f.assert_clean();
}

#[test]
fn a_repeat_submit_with_other_inputs_is_refused() {
    let f = Fixture::new(&[]);
    let sha = f.branch("one", &[("one.txt", "1\n")]);
    let request = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    let same = f.queue.submit(f.request("item-1", &sha, None)).unwrap();
    assert_eq!(same.id, request.id);
    let error = f
        .queue
        .submit(f.request("item-1", &sha, Some("New words")))
        .unwrap_err();
    assert!(error.to_string().contains("different inputs"), "{error}");
    let mut other_build = f.request("item-1", &sha, None);
    other_build.build.argv = vec!["false".into()];
    assert!(f.queue.submit(other_build).is_err());
    assert_eq!(f.queue.list().unwrap().len(), 1);
}

#[test]
fn a_lane_whose_repository_is_gone_can_be_forced_free() {
    let f = Fixture::new(&[]);
    let ready = ready_to_land(&f);
    std::fs::rename(&f.repo, f.repo.with_file_name("moved")).unwrap();
    f.queue.tick();
    assert_eq!(f.state(&ready).state, LandState::AwaitingReview);
    assert!(f.queue.cancel(&ready.id, "stuck").is_err());
    let forced = f.queue.force_cancel(&ready.id, "stuck").unwrap();
    assert_eq!(forced.state, LandState::Cancelled);
    let reason = forced.reason.unwrap();
    assert!(reason.contains("landing status unverifiable"), "{reason}");
}

#[test]
fn two_lands_of_one_item_keep_both_entries() {
    let f = Fixture::new(&[]);
    let one = f.branch("one", &[("one.txt", "1\n")]);
    let two = f.branch("two", &[("two.txt", "2\n")]);
    *f.builder.exit.lock().unwrap() = Some(0);
    for (sha, entry) in [(&one, "The feature."), (&two, "A follow-up.")] {
        let request = f
            .queue
            .submit(f.request("item-1", sha, Some(entry)))
            .unwrap();
        f.queue.record_verdict(&request.id, approve(sha)).unwrap();
        f.drive();
        assert_eq!(f.state(&request).state, LandState::Landed);
    }
    let head = f.target().unwrap();
    assert_eq!(
        f.show(&format!("{head}:docs/changelog.d/item-1.md")),
        "- The feature.\n- A follow-up."
    );
    assert_eq!(f.queue.landings("item-1").unwrap().len(), 2);
    f.assert_clean();
}
