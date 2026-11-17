//! Idle-budget guardrail for diff-sync (post-mortem 2026-11-16, P1; follow-up
//! "idle git rate", 2026-11-16).
//!
//! Soak on a real data copy measured ~200 git spawns/min with nothing
//! happening: the 120 s repair tick re-ran a full capture for every checkout
//! (the probe was never baselined after the first capture) and every identity
//! revalidation spawned two `git rev-parse`. This test holds `CHECKOUTS`
//! checkouts open, settles them, then replays `REPAIR_PASSES` repair ticks
//! (10 simulated minutes at the 120 s interval) with nothing changing.
//!
//! Budget per idle pass per checkout: one `git status` probe, nothing else
//! (identity revalidation reads the git dir from disk). So
//!   budget = REPAIR_PASSES × CHECKOUTS + SLACK.
//! The old behaviour was ≥ 6 per checkout per pass (full capture), > 5× over.
//!
//! Also asserted: every diff-sync spawn goes through the engine git gate (the
//! gate's counter, which feeds the per-minute warn, equals the harness counter),
//! and a commit in one checkout is still noticed by the next repair pass.
//! Own binary: the spawn counters are process-wide.

#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry, git_gate};
use mast_harness::git_spawn_count;

const CHECKOUTS: usize = 12;
const REPAIR_PASSES: u64 = 5;
const SLACK: u64 = 6;

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git spawns");
    assert!(output.status.success(), "git {args:?} failed");
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn checksums(sync: &CheckoutDiffSync) -> Vec<(String, String)> {
    sync.watch_diffs()
        .borrow()
        .iter()
        .map(|diff| (diff.cwd.clone(), diff.checksum.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_repair_ticks_stay_within_budget_and_a_head_change_is_still_seen() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();

    let mut repos = Vec::new();
    for i in 0..CHECKOUTS {
        let repo = tmp.path().join(format!("repo{i}"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        // A modified and an untracked file so each diff is non-trivial.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
        core.workspace
            .create_space(&format!("space-{i}"), &core.device_id, &repo.to_string_lossy(), None, true)
            .unwrap();
        core.workspace
            .create_chat(&format!("chat-{i}"), Some(&format!("space-{i}")), None, None, None)
            .unwrap();
        repos.push(repo);
    }
    core.diff_sync.reconcile_now().await;
    wait_for("every checkout publishes a diff", || {
        core.diff_sync.watch_diffs().borrow().len() == CHECKOUTS
    })
    .await;
    // Startup is done when every checkout has its watchers attached and no kick
    // is pending (initial capture, attach re-check). Event-driven, not a
    // wall-clock quiet guess, so a loaded box cannot leave attach work for the
    // measured window.
    wait_for("startup settles", || core.diff_sync.is_settled()).await;

    let harness_before = git_spawn_count();
    let gate_before = git_gate::spawn_count();
    for _ in 0..REPAIR_PASSES {
        core.diff_sync.repair_now().await;
        wait_for("repair pass completes", || core.diff_sync.is_settled()).await;
    }
    let harness = git_spawn_count() - harness_before;
    let gate = git_gate::spawn_count() - gate_before;
    let budget = REPAIR_PASSES * CHECKOUTS as u64 + SLACK;
    println!(
        "BENCH diff-sync idle | {CHECKOUTS} checkouts, {REPAIR_PASSES} repair passes | git_spawns={harness} budget={budget}"
    );
    assert!(harness <= budget, "idle repair passes spawned {harness} git, budget {budget}");
    assert_eq!(gate, harness, "every diff-sync spawn is counted by the git gate");

    // A commit moves HEAD: the next repair pass must publish a new diff for
    // that checkout only.
    let before = checksums(&core.diff_sync);
    tokio::time::sleep(Duration::from_millis(1100)).await; // distinct mtimes
    git(&repos[3], &["commit", "-q", "-a", "-m", "second"]);
    let moved = std::fs::canonicalize(&repos[3]).unwrap().to_string_lossy().into_owned();
    core.diff_sync.repair_now().await;
    wait_for("repair pass completes", || core.diff_sync.is_settled()).await;
    wait_for("HEAD change is published", || {
        let now = checksums(&core.diff_sync);
        now.iter().any(|(cwd, sum)| {
            *cwd == moved && before.iter().any(|(c, s)| c == cwd && s != sum)
        })
    })
    .await;
    let after = checksums(&core.diff_sync);
    for (cwd, sum) in &after {
        if *cwd != moved {
            assert!(before.iter().any(|(c, s)| c == cwd && s == sum), "{cwd} changed unexpectedly");
        }
    }
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
