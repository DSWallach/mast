//! Budget guardrails for code → chat provenance (area E).
//!
//! - A blame cache hit costs at most the one `git rev-parse HEAD` that keys the
//!   cache: the porcelain blame, the per-commit `branch --contains` fallback and
//!   the registry lookups are not repeated.
//! - With a blame shown and nothing changing, the engine starts no git at all:
//!   recording work happens only on HEAD moves of bound worktrees, never on a
//!   timer.
//!
//! Own binary: the spawn counter is process-wide.
#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::git_spawn_count;
use mast_proto::HarnessId;
use mast_rpc::methods;

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
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_cache_hit_and_idle_stay_within_the_git_budget() {
    let dirs = tempfile::tempdir().unwrap();
    let repo = dirs.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let mut text = String::new();
    for round in 0..6 {
        text.push_str(&format!("line {round}\n"));
        std::fs::write(repo.join("src/lib.rs"), &text).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", &format!("round {round}")]);
    }
    git(&repo, &["checkout", "-q", "-b", "feature-x"]);
    let core = EngineCore::assemble(
        &dirs.path().join("engine"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine assembles");
    core.workspace
        .create_space("space-budget", &core.device_id, &repo.to_string_lossy(), None, true)
        .unwrap();
    core.workspace.create_chat("chat-budget", Some("space-budget"), None, None, None).unwrap();
    core.workspace.create_chat("chat-feature", Some("space-budget"), None, None, None).unwrap();
    core.workspace.set_chat_branch("chat-feature", "feature-x").unwrap();
    // Creating the chats starts diff-sync's own startup capture of the checkout
    // (a git status probe plus the capture's own commands) on a background task.
    // That is not the blame path, and it can land inside the idle window below,
    // so let it finish before measuring. Idle diff-sync cost has its own budget
    // in diff_sync_idle_budget.
    core.diff_sync.reconcile_now().await;
    let mut completions = core.diff_sync.watch_completion_for_test();
    let checkout = repo.to_string_lossy().into_owned();
    tokio::time::timeout(Duration::from_secs(60), async {
        // `is_settled` is vacuously true until the checkout's entry exists, and `reconcile_now` can return
        // before it does: the entry's initial snapshot and attach re-probe (3 git spawns) would then land
        // inside the idle window below. Require the entry's watchers to be attached as well.
        while !(core.diff_sync.is_attached(&checkout) && core.diff_sync.is_settled()) {
            completions.changed().await.expect("diff-sync completion channel closed");
        }
    })
    .await
    .expect("diff-sync startup settles");
    let client = mast_rpc::memory_client(core.rpc_service());
    let params = serde_json::json!({"chatId": "chat-budget", "path": "src/lib.rs"});

    let first = client.call(methods::GET_WORKSPACE_FILE_BLAME, params.clone()).await.unwrap();
    assert_eq!(first["commits"].as_array().unwrap().len(), 6);

    let before_hit = git_spawn_count();
    for _ in 0..20 {
        let hit = client.call(methods::GET_WORKSPACE_FILE_BLAME, params.clone()).await.unwrap();
        assert_eq!(hit, first);
    }
    let per_hit = (git_spawn_count() - before_hit) as f64 / 20.0;
    assert!(per_hit <= 1.0, "blame cache hit spawned {per_hit} git per call (budget 1: rev-parse HEAD)");

    let before_idle = git_spawn_count();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(git_spawn_count(), before_idle, "idle engine with a blame shown must not spawn git");
}
