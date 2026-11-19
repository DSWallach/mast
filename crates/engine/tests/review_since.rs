//! Re-review range diff over throwaway repositories: linear push, force-push
//! rebase (interdiff), and an old head that no longer exists anywhere.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests (post-mortem P3)
use std::path::Path;
use std::process::Command;

use mast_engine::diff_sync::since::{SinceError, capture_since_diff};
use mast_engine::repos::Repos;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@t.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn repo() -> (tempfile::TempDir, std::path::PathBuf, Repos) {
    let tmp = tempfile::tempdir_in("/private/tmp").unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    for name in ["a.txt", "c.txt", "d.txt"] {
        write(&root, name, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    }
    commit(&root, "base");
    let repos = Repos::new(&tmp.path().join("data"), "device");
    (tmp, root, repos)
}

fn paths(snapshot: &mast_engine::diff_sync::DiffSnapshot) -> Vec<String> {
    let mut paths: Vec<String> = snapshot.files.iter().map(|f| f.path.clone()).collect();
    paths.sort();
    paths
}

#[tokio::test]
async fn linear_push_diffs_old_to_new() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["d.txt"]);
    assert_eq!((snapshot.additions, snapshot.deletions), (1, 1));
    // Cached: the same pair answers identically.
    let again = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(again.checksum, snapshot.checksum);
}

#[tokio::test]
async fn force_push_rebase_shows_only_new_hunks() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    // The base moves on, and the PR is rebased onto it with one more change.
    git(&root, &["checkout", "-q", "main"]);
    write(&root, "c.txt", "1\n2\nC\n4\n5\n6\n7\n8\n9\n10\n");
    commit(&root, "main moves");
    git(&root, &["checkout", "-q", "-B", "feature", "main"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    commit(&root, "f1 rebased");
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    let ancestor = Command::new("git").args(["merge-base", "--is-ancestor", &old, &new]).current_dir(&root).status().unwrap();
    assert!(!ancestor.success(), "old must not be an ancestor after the rebase");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["d.txt"], "carried hunks and the base's own change are dropped");
    assert!(snapshot.patch.contains("+D"));
    assert!(!snapshot.patch.contains("+C") && !snapshot.patch.contains("+A"));
    assert_eq!((snapshot.additions, snapshot.deletions), (1, 1));
}

#[tokio::test]
async fn amended_hunk_in_a_force_push_is_kept() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    write(&root, "a.txt", "1\n2\nA2\n4\n5\n6\n7\n8\n9\n10\n");
    git(&root, &["commit", "-qa", "--amend", "-m", "f1 amended"]);
    let new = git(&root, &["rev-parse", "HEAD"]);
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["a.txt"]);
    assert!(snapshot.patch.contains("+A2"));
}

#[tokio::test]
async fn missing_old_head_is_reported_for_fallback() {
    let (_tmp, root, repos) = repo();
    let new = git(&root, &["rev-parse", "HEAD"]);
    let gone = "1234567890abcdef1234567890abcdef12345678";
    let error = capture_since_diff(&repos, &root, "main", gone, &new).await.unwrap_err();
    assert_eq!(error, SinceError::OldUnavailable(gone.into()));
    assert!(error.to_string().starts_with("old-head-unavailable"));
}

#[tokio::test]
async fn missing_heads_are_fetched_from_origin() {
    let (tmp, root, repos) = repo();
    let bare = tmp.path().join("origin.git");
    git(tmp.path(), &["clone", "-q", "--bare", root.to_str().unwrap(), bare.to_str().unwrap()]);
    let clone = tmp.path().join("clone");
    git(tmp.path(), &["clone", "-q", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    git(&root, &["push", "-q", bare.to_str().unwrap(), "feature"]);
    // The clone has neither commit; both come through the fetch path.
    assert!(!Command::new("git").args(["cat-file", "-e", &old]).current_dir(&clone).status().unwrap().success());
    let snapshot = capture_since_diff(&repos, &clone, "origin/main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["d.txt"]);
}
