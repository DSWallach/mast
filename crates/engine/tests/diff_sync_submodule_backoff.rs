//! Repair-tick backoff for checkouts whose probe cannot decide (perf pass 2,
//! 2026-11-17). A workspace root with a dirty submodule makes the cheap probe
//! return "cannot decide", so every repair tick ran a full capture (~10 spawns,
//! more with submodules): on real data these few checkouts cost more than all
//! the status probes together. An unchanged capture now counts as a quiet tick,
//! so they back off like any other checkout; a changed one resets it.
//! Own binary: the spawn counters are process-wide.

#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use mast_harness::git_spawn_count;

const CHECKOUTS: usize = 6;
const TICKS: u64 = 15;

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "protocol.file.allow=always"])
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

fn checksum_of(sync: &CheckoutDiffSync, cwd: &str) -> Option<String> {
    sync.watch_diffs()
        .borrow()
        .iter()
        .find(|diff| diff.cwd == cwd)
        .map(|diff| diff.checksum.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undecidable_probes_back_off_and_a_missed_change_is_still_caught() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    let sub_src = tmp.path().join("sub-src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    std::fs::write(sub_src.join("s.txt"), "s\n").unwrap();
    git(&sub_src, &["add", "."]);
    git(&sub_src, &["commit", "-q", "-m", "sub"]);
    let mut repos = Vec::new();
    for i in 0..CHECKOUTS {
        let repo = tmp.path().join(format!("repo{i}"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        git(&repo, &["submodule", "add", "-q", &sub_src.to_string_lossy(), "sub"]);
        git(&repo, &["commit", "-q", "-m", "add sub"]);
        // Dirty submodule: the parent's status lists it, so the probe cannot decide.
        std::fs::write(repo.join("sub").join("s.txt"), "dirty\n").unwrap();
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
    wait_for("startup settles", || core.diff_sync.is_settled()).await;

    let before = git_spawn_count();
    // One tick's cost while every checkout still runs a full capture.
    core.diff_sync.repair_now().await;
    wait_for("tick completes", || core.diff_sync.is_settled()).await;
    let per_tick = git_spawn_count() - before;
    for _ in 1..TICKS {
        core.diff_sync.repair_now().await;
        wait_for("tick completes", || core.diff_sync.is_settled()).await;
    }
    let spawns = git_spawn_count() - before;
    let unbacked = per_tick * TICKS;
    println!(
        "BENCH diff-sync submodule backoff | {CHECKOUTS} checkouts, {TICKS} ticks | git_spawns={spawns} unbacked~={unbacked}"
    );
    // 3 full ticks, then 1 in 5: 3 + ceil(12/5) = 6 of 15 ticks (+ slack for phase).
    assert!(spawns * 2 <= unbacked, "backoff must at least halve {unbacked}, got {spawns}");

    // A change with no watcher event is still caught within the bound.
    core.diff_sync.drop_watchers_for_test();
    let missed = std::fs::canonicalize(&repos[2]).unwrap().to_string_lossy().into_owned();
    let missed_before = checksum_of(&core.diff_sync, &missed).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(repos[2].join("a.txt"), "one\ntwo\n").unwrap();
    let mut caught = None;
    for tick in 1..=CheckoutDiffSync::max_repair_gap_ticks() {
        core.diff_sync.repair_now().await;
        wait_for("tick completes", || core.diff_sync.is_settled()).await;
        if checksum_of(&core.diff_sync, &missed).is_some_and(|sum| sum != missed_before) {
            caught = Some(tick);
            break;
        }
    }
    println!("BENCH diff-sync submodule backoff | missed edit caught after {caught:?} tick(s)");
    assert!(caught.is_some(), "missed edit must be caught within the bound");
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
