//! An edit made after the initial capture and before watch registration is re-probed.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};

fn git(cwd: &Path, args: &[&str]) {
    let output = mast_harness::git_command().current_dir(cwd).args(args)
        .env("GIT_AUTHOR_NAME", "test").env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test").env("GIT_COMMITTER_EMAIL", "test@test")
        .output().unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

async fn wait_for(sync: &CheckoutDiffSync, mut ready: impl FnMut() -> bool) {
    let mut changes = sync.watch_completion_for_test();
    tokio::time::timeout(Duration::from_secs(600), async {
        while !ready() {
            changes.changed().await.expect("diff-sync completion channel closed");
        }
    }).await.expect("timed out waiting for diff-sync");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_before_registration_is_caught_by_attach_reprobe() {
    let pause = CheckoutDiffSync::pause_watch_setups_for_test().await;
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let core = EngineCore::assemble(&data, Arc::new(HarnessRegistry::new()), mast_proto::HarnessId::Mock, None).unwrap();
    let cwd = std::fs::canonicalize(&repo).unwrap().to_string_lossy().into_owned();
    core.workspace.create_space("space", &core.device_id, &cwd, None, true).unwrap();
    core.workspace.create_chat("chat", Some("space"), None, None, None).unwrap();
    core.diff_sync.reconcile_now().await;
    wait_for(&core.diff_sync, || !core.diff_sync.watch_diffs().borrow().is_empty()).await;
    assert!(!core.diff_sync.is_attached(&cwd));
    let initial = core.diff_sync.watch_diffs().borrow()[0].checksum.clone();
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    drop(pause);
    wait_for(&core.diff_sync, || core.diff_sync.is_attached(&cwd) && core.diff_sync.watch_diffs().borrow().first().is_some_and(|diff| diff.checksum != initial)).await;
    core.shutdown().await;
}

#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
