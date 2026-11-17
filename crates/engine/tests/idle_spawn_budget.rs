//! Idle-budget guardrail (post-mortem 2026-11-16, P1).
//!
//! The release-95 regression was invisible at toy scale: `change_request_root`
//! cost ~5 `git` spawns per Space per call, and the UI retried a rejected watch
//! (a deleted worktree) every 2 s forever. This test runs the real RPC path at
//! Mikey's scale and asserts how many `git` processes an idle engine may start.
//!
//! Scenario: one git Space with 100 linked worktrees (`git worktree add
//! --detach`), 50 `WatchCheckoutChangeRequest` watches — 40 on live worktrees,
//! 10 on worktrees whose directories were deleted — then the retry traffic of
//! a few minutes of UI backoff replayed back-to-back, then an idle hold.
//!
//! Budget (spawns counted by `mast_harness::git_spawn_count`, which counts every
//! engine `git` command):
//!
//! - Resolving the cwd is O(1) in git per Space: one `git worktree list` fills a
//!   cache with a 60 s TTL (single-flight). A lookup miss may refresh it at most
//!   once per 3 s (`CHECKOUT_SET_MISS_REFRESH`), so those spawns are bounded by
//!   `SPACES × (ceil(elapsed/3 s) + 1)`, independent of watches × retries.
//! - Each *accepted* watch pays `checkout_identity`, which now reads the git dir
//!   from disk (was 2 × `git rev-parse`; `ACCEPT_COST` is 0). Falls back to git
//!   only for layouts it cannot read.
//! - `SLACK = 4` covers scheduling noise.
//!
//!   budget = SPACES × (ceil(elapsed/3) + 1) + SLACK
//!          = 1 × 4 + 4 = 8 for a ~9 s run.
//!
//!   Observed before the spawn-free `checkout_identity`: 84–87 (the old budget
//!   added 2 per accepted watch). Release 94 spent ~5 git spawns per Space
//!   per call on every watch and every retry (≈ 5 × 50 × (1 + 8 retries) ≈ 2,250
//!   here), which blows this budget by more than 25×.
//! - A rejected watch (path no longer resolves) never reaches git: zero spawns
//!   for every retry after (and in fact including) the first lookup.
//! - Holding the 40 live watches open with nothing changing spawns zero git.
//!
//! The UI half (a rejected watch backs off 2 s → 5 min) is covered by
//! `state::tests::change_request_retry_*` in `mast-ui`; the schedule replayed
//! here comes from the same `mast_harness::backoff` helper.
#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mast_engine::{
    BranchHeadContext, ChangeRequestError, CheckoutChangeRequestLookup, CheckoutChangeRequests,
    CheckoutSourceContext, EngineCore, HarnessRegistry,
};
use mast_harness::{backoff, git_spawn_count};
use mast_proto::{ChangeRequestState, ChangeRequestSummary, HarnessId};
use mast_rpc::methods;

const WORKTREES: usize = 100;
const LIVE_WATCHES: usize = 40;
const DEAD_WATCHES: usize = 10;
const SPACES: u64 = 1;
const SLACK: u64 = 4;
/// `Repos::checkout_identity` reads the git dir from disk: no spawn per accepted watch.
const ACCEPT_COST: u64 = 0;
/// Simulated minutes of UI retry traffic replayed for rejected watches.
const SIMULATED_MINUTES: u64 = 5;

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// No network, no git: the lookup's own work must not add spawns to the count.
struct StaticLookup(CheckoutSourceContext);

#[async_trait]
impl CheckoutChangeRequestLookup for StaticLookup {
    async fn inspect_checkout(
        &self,
        _cwd: &Path,
    ) -> Result<CheckoutSourceContext, ChangeRequestError> {
        Ok(self.0.clone())
    }

    async fn resolve_github_source(
        &self,
        _source: &CheckoutSourceContext,
    ) -> Result<Option<ChangeRequestSummary>, ChangeRequestError> {
        Ok(Some(ChangeRequestSummary {
            provider: "github".into(),
            number: 1,
            title: "idle budget".into(),
            url: "https://github.com/acme/mast/pull/1".into(),
            state: ChangeRequestState::Open,
            base_ref: "main".into(),
            head_ref: "feature".into(),
        }))
    }
}

/// Number of retries a watch makes in `minutes` of backoff (2 s base).
fn retries_within(minutes: u64, ceiling: Duration) -> Vec<Duration> {
    let limit = Duration::from_secs(minutes * 60);
    let mut elapsed = Duration::ZERO;
    let mut attempts = Vec::new();
    for attempt in 0.. {
        attempts.push(elapsed);
        elapsed += backoff::delay_for_attempt(Duration::from_secs(2), ceiling, attempt);
        if elapsed > limit {
            break;
        }
    }
    attempts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_engine_stays_within_its_git_spawn_budget() {
    let started = Instant::now();
    let dirs = tempfile::tempdir().expect("tempdir");
    let space = dirs.path().join("space");
    std::fs::create_dir_all(&space).unwrap();
    git(&space, &["init", "-q", "-b", "main"]);
    git(&space, &["commit", "-q", "--allow-empty", "-m", "initial"]);
    let worktrees: Vec<PathBuf> = (0..WORKTREES)
        .map(|i| dirs.path().join("wt").join(format!("w{i:03}")))
        .collect();
    std::fs::create_dir_all(dirs.path().join("wt")).unwrap();
    for worktree in &worktrees {
        git(
            &space,
            &["worktree", "add", "-q", "--detach", &worktree.to_string_lossy()],
        );
    }

    let registry = HarnessRegistry::new();
    let mut core = EngineCore::assemble(
        &dirs.path().join("engine"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine assembles");
    core.workspace
        .create_space("space-big", &core.device_id, &space.to_string_lossy(), None, true)
        .expect("space row");
    let root = std::fs::canonicalize(&space).unwrap();
    core.change_requests = CheckoutChangeRequests::new(
        core.repos.clone(),
        &core.device_id.clone(),
        Arc::new(StaticLookup(CheckoutSourceContext {
            checkout_root: root,
            branch: BranchHeadContext::resolve(
                "feature",
                Some("origin/feature"),
                Some("origin"),
                Some("https://github.com/acme/mast.git"),
            ),
            default_branch: Some("main".into()),
        })),
    );
    let client = mast_rpc::memory_client(core.rpc_service());

    // Delete the last DEAD_WATCHES worktree directories (git still lists them,
    // as prunable). The live watches use the first LIVE_WATCHES.
    let dead: Vec<PathBuf> = worktrees[WORKTREES - DEAD_WATCHES..].to_vec();
    let live: Vec<PathBuf> = worktrees[..LIVE_WATCHES].to_vec();
    for path in &dead {
        std::fs::remove_dir_all(path).expect("delete worktree dir");
    }
    assert_eq!(live.len() + dead.len(), 50);

    let baseline = git_spawn_count();

    // Open every watch once: live ones stream, dead ones are rejected.
    let mut streams = Vec::new();
    for path in &live {
        let mut stream = client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::json!({ "cwd": path }),
            )
            .await
            .expect("live worktree watch is accepted");
        tokio::time::timeout(Duration::from_secs(10), stream.recv())
            .await
            .expect("initial frame")
            .expect("frame");
        streams.push(stream);
    }
    let after_live = git_spawn_count();
    let mut rejected_first = 0u64;
    for path in &dead {
        let before = git_spawn_count();
        let error = match client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::json!({ "cwd": path }),
            )
            .await
        {
            Ok(_) => panic!("deleted worktree must be rejected"),
            Err(error) => error,
        };
        // Over the in-memory client the engine's BadParams arrives as `Failed`.
        assert!(
            error.to_string().contains("not a known checkout"),
            "{error:?}"
        );
        rejected_first += git_spawn_count() - before;
    }

    // Replay the UI's retry traffic for the rejected watches: BadParams backs
    // off 2 s → 5 min, so a few simulated minutes is a handful of attempts per
    // watch. Every retry after the first lookup must be free.
    let schedule = retries_within(SIMULATED_MINUTES, Duration::from_secs(300));
    assert!(
        schedule.len() <= 9,
        "a rejected watch retried {} times in {SIMULATED_MINUTES} min: {schedule:?}",
        schedule.len()
    );
    let before_retries = git_spawn_count();
    let mut retries = 0u64;
    for _ in schedule.iter().skip(1) {
        for path in &dead {
            assert!(
                client
                    .subscribe_checked(
                        methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                        serde_json::json!({ "cwd": path }),
                    )
                    .await
                    .is_err()
            );
            retries += 1;
        }
    }
    assert!(retries >= DEAD_WATCHES as u64);
    assert_eq!(
        git_spawn_count() - before_retries,
        0,
        "rejected watches spawned git on retry ({retries} retries)"
    );

    // Live watches with nothing changing: zero git while idle.
    let before_idle = git_spawn_count();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(git_spawn_count() - before_idle, 0, "idle live watches spawned git");

    let total = git_spawn_count() - baseline;
    let elapsed = started.elapsed();
    let budget = SPACES * (elapsed.as_secs().div_ceil(3) + 1)
        + live.len() as u64 * ACCEPT_COST
        + SLACK;
    eprintln!(
        "idle budget: {total} git spawns (live watches {}, rejected first lookups {rejected_first}, \
         retries {retries}, budget {budget}, elapsed {elapsed:?})",
        after_live - baseline
    );
    assert!(total <= budget, "{total} git spawns exceeds budget {budget}");
    assert_eq!(rejected_first, 0, "a deleted worktree must not reach git at all");
    assert!(
        elapsed < Duration::from_secs(60),
        "keep this test under a minute (took {elapsed:?})"
    );
    drop(streams);
    core.shutdown().await;
}
