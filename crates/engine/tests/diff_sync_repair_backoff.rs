//! Repair-tick backoff budget (perf pass 2, 2026-11-17).
//!
//! Real data showed a steady ~40-55 git spawns/min: the 120 s repair tick probes
//! every checkout with `git status`. Fully watched checkouts whose probes keep
//! coming back unchanged now back off to one probe per `max_repair_gap_ticks()`
//! ticks, phase-staggered. `repair_now()` is one simulated tick, so 15 ticks is
//! 30 simulated minutes at the 120 s interval; no wall-clock waiting.
//!
//! Asserted: spawns for 50 idle checkouts over 30 minutes stay under budget (the
//! unbacked-off cost is 15 x 50), a HEAD change made while the watchers deliver
//! nothing is still published within the documented bound and a change that does
//! reach a watcher is published without any repair tick.
//! Own binary: the spawn counters are process-wide.

#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use mast_harness::git_spawn_count;

const CHECKOUTS: usize = 50;
const TICKS: u64 = 15; // 30 simulated minutes
const QUIET_TICKS: u64 = 3; // probes at the base cadence before backoff
const SLACK: u64 = 10;

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

async fn wait_for(sync: &CheckoutDiffSync, what: &str, mut ready: impl FnMut() -> bool) {
    let mut changes = sync.watch_completion_for_test();
    tokio::time::timeout(Duration::from_secs(600), async {
        while !ready() {
            changes.changed().await.expect("diff-sync completion channel closed");
        }
    }).await.unwrap_or_else(|_| panic!("timed out: {what}"));
}

fn checksum_of(sync: &CheckoutDiffSync, cwd: &str) -> Option<String> {
    sync.watch_diffs()
        .borrow()
        .iter()
        .find(|diff| diff.cwd == cwd)
        .map(|diff| diff.checksum.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_checkouts_back_off_and_a_missed_head_change_is_caught_within_the_bound() {
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
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        core.workspace
            .create_space(&format!("space-{i}"), &core.device_id, &repo.to_string_lossy(), None, true)
            .unwrap();
        core.workspace
            .create_chat(&format!("chat-{i}"), Some(&format!("space-{i}")), None, None, None)
            .unwrap();
        repos.push(repo);
    }
    core.diff_sync.reconcile_now().await;
    wait_for(&core.diff_sync, "every checkout publishes a diff", || {
        core.diff_sync.watch_diffs().borrow().len() == CHECKOUTS
    })
    .await;
    wait_for(&core.diff_sync, "startup settles", || core.diff_sync.is_settled()).await;

    let stride = CheckoutDiffSync::max_repair_gap_ticks();
    let before = git_spawn_count();
    for _ in 0..TICKS {
        core.diff_sync.repair_now().await;
        wait_for(&core.diff_sync, "repair tick completes", || core.diff_sync.is_settled()).await;
    }
    let spawns = git_spawn_count() - before;
    let backed_off_ticks = TICKS - QUIET_TICKS;
    let per_checkout = QUIET_TICKS + backed_off_ticks.div_ceil(stride);
    let budget = CHECKOUTS as u64 * per_checkout + SLACK;
    let unbacked = CHECKOUTS as u64 * TICKS;
    println!(
        "BENCH diff-sync repair backoff | {CHECKOUTS} checkouts, {TICKS} ticks | git_spawns={spawns} budget={budget} unbacked={unbacked}"
    );
    assert!(spawns <= budget, "idle ticks spawned {spawns} git, budget {budget}");
    assert!(spawns < unbacked / 2, "backoff must at least halve the {unbacked} spawns");

    // A change the watcher reports is published with no repair tick at all.
    let watched = std::fs::canonicalize(&repos[5]).unwrap().to_string_lossy().into_owned();
    let watched_before = checksum_of(&core.diff_sync, &watched).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await; // distinct mtimes
    git(&repos[5], &["commit", "-q", "-a", "-m", "watched"]);
    wait_for(&core.diff_sync, "watcher-driven change is published", || {
        checksum_of(&core.diff_sync, &watched).is_some_and(|sum| sum != watched_before)
    })
    .await;
    wait_for(&core.diff_sync, "settles", || core.diff_sync.is_settled()).await;

    // Events the OS never delivered: HEAD moves, nothing kicks. The repair tick
    // must still publish it within `stride` ticks.
    core.diff_sync.drop_watchers_for_test();
    let missed = std::fs::canonicalize(&repos[3]).unwrap().to_string_lossy().into_owned();
    let missed_before = checksum_of(&core.diff_sync, &missed).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    git(&repos[3], &["commit", "-q", "-a", "-m", "missed"]);
    let mut caught_after = None;
    for tick in 1..=stride {
        core.diff_sync.repair_now().await;
        wait_for(&core.diff_sync, "repair tick completes", || core.diff_sync.is_settled()).await;
        if checksum_of(&core.diff_sync, &missed).is_some_and(|sum| sum != missed_before) {
            caught_after = Some(tick);
            break;
        }
    }
    let caught = caught_after.expect("missed HEAD change must be caught within the bound");
    println!("BENCH diff-sync repair backoff | missed change caught after {caught} tick(s), bound {stride}");
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
