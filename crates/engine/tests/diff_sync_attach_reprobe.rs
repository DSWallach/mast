//! Post-attach re-probe (perf pass 2, 2026-11-17): when a checkout's first sync
//! probes after its watchers attached, the capture-to-attach `git status` re-probe
//! is skipped. Own binary: the spawn counters and attach-wait knob are process-wide.

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

/// With the first sync held until the watchers attach, it probes after attach and the extra capture-to-attach `status` is skipped: a
/// settled checkout cost exactly one probe plus the capture's two diffs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_sync_after_attach_skips_the_reprobe() {
    // Attach takes ~1-2 s per checkout here (FSEvents registration); hold the
    // first sync long enough that every probe is taken after attach.
    CheckoutDiffSync::set_initial_sync_attach_wait_for_test(Duration::from_secs(600));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();
    const N: usize = 8;
    for i in 0..N {
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
    }
    let before = git_spawn_count();
    core.diff_sync.reconcile_now().await;
    wait_for(&core.diff_sync, "every checkout publishes a diff", || {
        core.diff_sync.watch_diffs().borrow().len() == N
    })
    .await;
    wait_for(&core.diff_sync, "startup settles", || core.diff_sync.is_settled()).await;
    let spawns = git_spawn_count() - before;
    // probe + `diff --raw --numstat` + `diff --unified` per checkout = 3; the old
    // flow added a fourth per checkout (the attach re-probe), i.e. 4 * N.
    let budget = 3 * N as u64 + 2;
    println!("BENCH diff-sync first sync | {N} checkouts | git_spawns={spawns} budget={budget}");
    assert!(spawns <= budget, "first sync spawned {spawns} git, budget {budget}");
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
