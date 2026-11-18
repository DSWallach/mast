//! Startup spawn budget for diff-sync (perf pass 2026-11-17).
//!
//! Soak on a real data copy: ~385 git spawns in the first 180 s, ~250 of them in the
//! first 30 s, for ~44 checkouts. Each checkout's first sync ran `status --porcelain=v2`
//! (probe), then a capture of `rev-parse --verify HEAD`, `diff --raw --numstat`,
//! `diff --unified`, and a second `status --porcelain=v1`, then one more probe `status`
//! after the watchers attached. HEAD is readable from the git dir and the second status
//! duplicates the probe, so the budget is 4 spawns per checkout (probe, 2 diffs,
//! attach re-probe); the old code needed 6.
//! Own binary: the spawn counters are process-wide.

#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use mast_harness::git_spawn_count;

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


const CHECKOUTS: usize = 12;
const PER_CHECKOUT_BUDGET: u64 = 4;
const SLACK: u64 = 6;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_sync_of_many_checkouts_stays_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    for i in 0..CHECKOUTS {
        let repo = tmp.path().join(format!("repo{i}"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
        core.workspace
            .create_space(&format!("space-{i}"), &core.device_id, &repo.to_string_lossy(), None, true)
            .unwrap();
        core.workspace
            .create_chat(&format!("chat-{i}"), Some(&format!("space-{i}")), None, None, None)
            .unwrap();
    }
    let harness_before = git_spawn_count();
    core.diff_sync.reconcile_now().await;
    wait_for(&core.diff_sync, "every checkout publishes a diff", || {
        core.diff_sync.watch_diffs().borrow().len() == CHECKOUTS
    })
    .await;
    wait_for(&core.diff_sync, "startup settles", || core.diff_sync.is_settled()).await;
    let spawns = git_spawn_count() - harness_before;
    let budget = PER_CHECKOUT_BUDGET * CHECKOUTS as u64 + SLACK;
    println!(
        "BENCH diff-sync startup | {CHECKOUTS} checkouts | git_spawns={spawns} budget={budget}"
    );
    assert!(spawns <= budget, "startup spawned {spawns} git, budget {budget}");

    // The cheaper capture must still describe the checkout: modified + untracked.
    let diffs = core.diff_sync.watch_diffs().borrow().clone();
    for diff in &diffs {
        assert!(diff.patch.contains("+two"), "tracked change in {}", diff.cwd);
        assert!(diff.patch.contains("+untracked"), "untracked file in {}", diff.cwd);
    }
    let statuses = core.diff_sync.watch_git_statuses().borrow().clone();
    assert_eq!(statuses.len(), CHECKOUTS);
    for status in &statuses {
        assert!(status.files.iter().any(|f| f.path == "new.txt"), "untracked listed");
        assert!(status.files.iter().any(|f| f.path == "a.txt"), "modified listed");
    }
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
