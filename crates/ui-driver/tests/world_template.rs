//! B2b: the small world materialized into real repos and engine stores, the
//! snapshot hash, the template and reset.
#![allow(clippy::disallowed_methods)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use std::path::PathBuf;

use mast_ui_driver::environment::ScratchDirectories;
use mast_ui_driver::materialize::{self, main_repo, snapshot_hash, worktree_path};
use mast_ui_driver::template::Template;
use mast_ui_driver::world::SmallWorld;

fn account() -> PathBuf {
    // Never touched: only used to prove the scratch root does not overlap it.
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn finish(scratch: ScratchDirectories) {
    // The scratch root is a mktemp directory this test created.
    std::fs::remove_dir_all(scratch.root()).unwrap();
}

#[test]
fn same_seed_same_hash_different_seed_different_hash() {
    let rt = rt();
    let hash_for = |seed: u64| {
        let scratch = ScratchDirectories::create(&account()).unwrap();
        let world = SmallWorld::small(seed);
        let hash = rt.block_on(async {
            materialize::materialize(&scratch, &world).await.unwrap();
            let core = materialize::open_core(scratch.data()).unwrap();
            let hash = snapshot_hash(&scratch, &world, &core).unwrap();
            if std::env::var_os("MAST_DRIVER_DEBUG_LINES").is_some() {
                for line in materialize::snapshot_lines(&scratch, &world, &core).unwrap() {
                    eprintln!("LINE {line}");
                }
            }
            core.shutdown().await;
            hash
        });
        // Real content: commits, three worktrees (one dirty, one deleted).
        assert!(main_repo(&scratch, &world).join(".git").is_dir());
        assert!(worktree_path(&scratch, "worktree-0").is_dir());
        assert!(
            worktree_path(&scratch, "worktree-1")
                .join("scratch.txt")
                .is_file()
        );
        assert!(!worktree_path(&scratch, "worktree-2").exists());
        let log = scratch
            .git(
                &main_repo(&scratch, &world),
                &["rev-list", "--count", "main"],
                &[],
            )
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "4");
        assert!(
            std::fs::read_to_string(scratch.git_log())
                .unwrap()
                .contains("git worktree add")
        );
        finish(scratch);
        hash
    };
    let first = hash_for(7);
    let again = hash_for(7);
    assert_eq!(first, again);
    assert_ne!(first, hash_for(8));
}

#[test]
fn template_clones_and_reset_restores_it() {
    let rt = rt();
    let scratch = ScratchDirectories::create(&account()).unwrap();
    let world = SmallWorld::small(11);
    let template = rt.block_on(Template::build(&scratch, &world)).unwrap();
    assert!(
        template
            .header()
            .starts_with("preset=small seed=11 template=")
    );

    // Dirty the run: new commit, new file, and an engine write.
    let repo = main_repo(&scratch, &world);
    std::fs::write(repo.join("extra.txt"), "x").unwrap();
    scratch.git(&repo, &["add", "extra.txt"], &[]).unwrap();
    scratch
        .git(&repo, &["commit", "-q", "-m", "extra"], &[])
        .unwrap();
    rt.block_on(async {
        let core = materialize::open_core(scratch.data()).unwrap();
        core.workspace
            .rename_chat(&world.chats[0].id, "renamed")
            .unwrap();
        let changed = snapshot_hash(&scratch, &world, &core).unwrap();
        assert_ne!(changed, template.hash());
        core.shutdown().await;
    });

    template.restore(&scratch).unwrap();
    let again = rt.block_on(async {
        let core = materialize::open_core(scratch.data()).unwrap();
        let hash = snapshot_hash(&scratch, &world, &core).unwrap();
        core.shutdown().await;
        hash
    });
    assert_eq!(again, template.hash());
    assert!(!repo.join("extra.txt").exists());
    // A second restore after more changes is stable too.
    std::fs::write(repo.join("again.txt"), "y").unwrap();
    template.restore(&scratch).unwrap();
    assert!(!repo.join("again.txt").exists());
    finish(scratch);
}
