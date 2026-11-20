//! Synthetic data generator for `scripts/perf-soak.sh` (not a regression test).
//!
//! Ignored by default. `scripts/perf-soak.sh --synthetic` runs it with
//! `MAST_SOAK_SEED_ROOT` set; it builds, under that root,
//!
//! - `repos/space-N`: real git repos, each with `MAST_SOAK_SEED_WORKTREES` linked
//!   worktrees under `repos/wt-N/`;
//! - `data/`: an engine data dir (Local profile, the one a signed-out Mast
//!   uses) holding one Space per repo and `MAST_SOAK_SEED_CHATS` chats whose
//!   cwd is spread across those worktrees, a few with deleted directories.
//!
//! It never reads or writes anything outside the root.
#![allow(clippy::disallowed_methods)] // fixture git commands are tests (post-mortem P3)

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use mast_engine::{EngineCore, EngineProfile, HarnessRegistry};
use mast_proto::HarnessId;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "soak")
        .env("GIT_AUTHOR_EMAIL", "soak@example.invalid")
        .env("GIT_COMMITTER_NAME", "soak")
        .env("GIT_COMMITTER_EMAIL", "soak@example.invalid")
        .output()
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "generator for scripts/perf-soak.sh; needs MAST_SOAK_SEED_ROOT"]
async fn seed_soak_data() {
    let Some(root) = std::env::var_os("MAST_SOAK_SEED_ROOT").map(PathBuf::from) else {
        panic!("MAST_SOAK_SEED_ROOT is required");
    };
    let spaces = env_usize("MAST_SOAK_SEED_SPACES", 6).max(1);
    let worktrees = env_usize("MAST_SOAK_SEED_WORKTREES", 20);
    let chats = env_usize("MAST_SOAK_SEED_CHATS", 60);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(root.join("repos")).unwrap();

    let profile = EngineProfile::local(&data).expect("local profile");
    let core = EngineCore::assemble_with_profile(
        profile,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine assembles");

    let mut cwds: Vec<String> = Vec::new();
    for s in 0..spaces {
        let repo = root.join("repos").join(format!("space-{s}"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "initial"]);
        core.workspace
            .create_space(
                &format!("space-{s}"),
                &core.device_id,
                &repo.to_string_lossy(),
                Some(format!("space {s}")),
                true,
            )
            .expect("space row");
        for w in 0..worktrees {
            let wt = root.join("repos").join(format!("wt-{s}")).join(format!("w{w:03}"));
            std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
            git(&repo, &["worktree", "add", "-q", "--detach", &wt.to_string_lossy()]);
            cwds.push(wt.to_string_lossy().into_owned());
        }
    }
    // Every 10th chat points at a worktree that is then deleted from disk
    // (git still lists it as prunable): the stale-chat shape from the post-mortem.
    for c in 0..chats {
        let cwd = cwds
            .get(c % cwds.len().max(1))
            .cloned()
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let space = format!("space-{}", c % spaces);
        core.workspace
            .create_chat(&format!("chat-{c:04}"), Some(&space), None, None, Some(cwd.clone()))
            .expect("chat row");
        if c % 10 == 9 {
            let _ = std::fs::remove_dir_all(&cwd);
        }
    }
    println!(
        "soak seed ok: {spaces} spaces, {} worktrees, {chats} chats, device {}",
        cwds.len(),
        core.device_id
    );
}
