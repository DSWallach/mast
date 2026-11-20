//! Re-review range diff over throwaway repositories: linear push, force-push
//! rebase (interdiff), and an old head that no longer exists anywhere.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests (post-mortem P3)
#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;

use std::path::Path;
use std::process::Command;

use mast_engine::diff_sync::since::{SinceError, capture_since_diff, computes_for, fetch_attempts_for};
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
    let tmp = tempfile::tempdir().unwrap();
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

const TEN: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

fn ancestor(root: &Path, old: &str, new: &str) -> bool {
    Command::new("git").args(["merge-base", "--is-ancestor", old, new]).current_dir(root).status().unwrap().success()
}

#[tokio::test]
async fn force_push_that_drops_a_hunk_or_a_file_shows_the_removals_as_reversions() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\nA\n3\n4\n5\n6\n7\n8\n9\nTEN\n");
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1: two hunks in a.txt and a change to d.txt");
    // Rewrite: keep a.txt's first hunk, drop its second hunk and drop d.txt entirely.
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "a.txt", "1\nA\n3\n4\n5\n6\n7\n8\n9\n10\n");
    write(&root, "c.txt", "1\n2\nC\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f1 rewritten");
    assert!(!ancestor(&root, &old, &new));
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["a.txt", "c.txt", "d.txt"]);
    // The new change is a plain hunk; the carried A hunk is not repeated.
    assert!(snapshot.patch.contains("+C"));
    assert!(!snapshot.patch.contains("-A\n") && !snapshot.patch.contains("+A\n"), "carried hunk absent");
    // The dropped a.txt hunk (10 -> TEN) and the dropped d.txt file come back reversed, labelled.
    let label = mast_engine::diff_sync::since::REMOVED_LABEL;
    assert_eq!(snapshot.patch.matches(label).count(), 2, "one dropped hunk in a.txt, one in d.txt:\n{}", snapshot.patch);
    assert!(snapshot.patch.contains("+10\n-TEN\n"), "{}", snapshot.patch);
    assert!(snapshot.patch.contains("+3\n-D\n"), "{}", snapshot.patch);
}

#[tokio::test]
async fn base_merged_into_the_branch_shows_only_the_authors_changes() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    git(&root, &["checkout", "-q", "main"]);
    write(&root, "c.txt", "1\n2\nC\n4\n5\n6\n7\n8\n9\n10\n");
    commit(&root, "main moves");
    git(&root, &["checkout", "-q", "feature"]);
    git(&root, &["merge", "-q", "--no-ff", "-m", "Update branch", "main"]);
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    assert!(ancestor(&root, &old, &new), "a merge keeps old as an ancestor");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["d.txt"], "the base's c.txt change is not the author's");
}

#[tokio::test]
async fn rename_deletion_and_binary_changes_are_kept_when_new() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    git(&root, &["checkout", "-q", "main"]);
    write(&root, "c.txt", "1\n2\nC\n4\n5\n6\n7\n8\n9\n10\n");
    commit(&root, "main moves");
    git(&root, &["checkout", "-q", "-B", "feature", "main"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    git(&root, &["mv", "d.txt", "moved name.txt"]);
    git(&root, &["rm", "-q", "c.txt"]);
    std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    let new = commit(&root, "f2: rename, delete, binary");
    assert!(!ancestor(&root, &old, &new));
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    let got = paths(&snapshot);
    assert!(got.contains(&"moved name.txt".to_owned()), "{got:?}");
    assert!(got.contains(&"c.txt".to_owned()), "deletion kept: {got:?}");
    assert!(got.contains(&"blob.bin".to_owned()), "binary kept: {got:?}");
    assert!(!got.contains(&"a.txt".to_owned()), "carried hunk dropped: {got:?}");
    assert!(snapshot.patch.contains("moved name.txt"));
}

#[tokio::test]
async fn cached_pair_computes_once_and_a_missing_head_is_not_refetched() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "a.txt", "1\n2\nA\n4\n5\n6\n7\n8\n9\n10\n");
    let old = commit(&root, "f1");
    write(&root, "d.txt", TEN);
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    let before = computes_for(&root);
    capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(computes_for(&root), before + 1);
    capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(computes_for(&root), before + 1, "a cache hit runs no git");
    // An old head that cannot be fetched is asked for once, then backed off.
    let gone = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let attempts = fetch_attempts_for(&root);
    for _ in 0..3 {
        let error = capture_since_diff(&repos, &root, "main", gone, &new).await.unwrap_err();
        assert!(matches!(error, SinceError::OldUnavailable(_)));
    }
    assert_eq!(fetch_attempts_for(&root), attempts + 1);
}

fn long(changes: &[(usize, &str)]) -> String {
    (1..=30)
        .map(|n| match changes.iter().find(|(line, _)| *line == n) {
            Some((_, text)) => format!("{text}\n"),
            None => format!("l{n}\n"),
        })
        .collect()
}

/// A repo whose main also has a 30-line long.txt (so hunks can be far apart).
fn long_repo() -> (tempfile::TempDir, std::path::PathBuf, Repos) {
    let (tmp, root, repos) = repo();
    write(&root, "long.txt", &long(&[]));
    commit(&root, "long file");
    (tmp, root, repos)
}

#[tokio::test]
async fn hunks_that_merge_after_a_nearby_change_are_not_false_removals() {
    let (_tmp, root, repos) = long_repo();
    git(&root, &["checkout", "-qb", "feature"]);
    // Two author hunks, far enough apart to stay separate.
    write(&root, "long.txt", &long(&[(5, "FIVE"), (20, "TWENTY")]));
    let old = commit(&root, "f1");
    // Force-push: the author adds a nearby change between them, so git now prints one merged hunk.
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "long.txt", &long(&[(5, "FIVE"), (11, "ELEVEN"), (17, "SEVENTEEN"), (20, "TWENTY")]));
    let new = commit(&root, "f1 rewritten");
    assert!(!ancestor(&root, &old, &new));
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert!(!snapshot.patch.contains(mast_engine::diff_sync::since::REMOVED_LABEL), "no false removal:\n{}", snapshot.patch);
    assert!(snapshot.patch.contains("+ELEVEN") && snapshot.patch.contains("+SEVENTEEN"), "the new changes show:\n{}", snapshot.patch);
    // The unchanged runs inside the regrouped hunk are not shown as additions.
    assert!(!snapshot.patch.contains("+FIVE") && !snapshot.patch.contains("+TWENTY"), "unchanged runs are not new:\n{}", snapshot.patch);
    let file = snapshot.files.iter().find(|f| f.path == "long.txt").unwrap();
    assert_eq!((file.additions, file.deletions), (2, 2));
}

#[tokio::test]
async fn base_lines_between_author_hunks_are_not_false_removals() {
    let (_tmp, root, repos) = long_repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "long.txt", &long(&[(3, "THREE"), (27, "TWENTY-SEVEN")]));
    let old = commit(&root, "f1");
    git(&root, &["checkout", "-q", "main"]);
    write(&root, "long.txt", &long(&[(15, "BASE-FIFTEEN")]));
    commit(&root, "main edits between the author's hunks");
    git(&root, &["checkout", "-q", "feature"]);
    git(&root, &["merge", "-q", "--no-ff", "-m", "Update branch", "main"]);
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f2");
    assert!(ancestor(&root, &old, &new));
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(paths(&snapshot), vec!["d.txt"], "{}", snapshot.patch);
    assert!(!snapshot.patch.contains(mast_engine::diff_sync::since::REMOVED_LABEL));
}

#[tokio::test]
async fn a_genuinely_dropped_run_is_shown_even_beside_kept_runs() {
    let (_tmp, root, repos) = long_repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "long.txt", &long(&[(5, "FIVE"), (20, "TWENTY")]));
    let old = commit(&root, "f1");
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "long.txt", &long(&[(5, "FIVE")]));
    let new = commit(&root, "f1 without the second change");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    let label = mast_engine::diff_sync::since::REMOVED_LABEL;
    assert_eq!(snapshot.patch.matches(label).count(), 1, "{}", snapshot.patch);
    assert!(snapshot.patch.contains("+l20\n-TWENTY\n"), "reversed, with context around it:\n{}", snapshot.patch);
    assert_eq!(snapshot.patch.matches("diff --git").count(), 1, "one block per file");
    let file = snapshot.files.iter().find(|f| f.path == "long.txt").unwrap();
    assert_eq!((file.additions, file.deletions), (1, 1), "counts include the removed lines");
    assert_eq!((snapshot.additions, snapshot.deletions), (1, 1));
}

#[tokio::test]
async fn kept_and_removed_changes_share_one_block_with_summed_counts() {
    let (_tmp, root, repos) = long_repo();
    git(&root, &["checkout", "-qb", "feature"]);
    write(&root, "long.txt", &long(&[(5, "FIVE")]));
    let old = commit(&root, "f1");
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "long.txt", &long(&[(25, "TWENTY-FIVE")]));
    let new = commit(&root, "f1 moved to another place");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert_eq!(snapshot.patch.matches("diff --git").count(), 1, "{}", snapshot.patch);
    assert!(snapshot.patch.contains("+TWENTY-FIVE") && snapshot.patch.contains("-FIVE"));
    let file = snapshot.files.iter().find(|f| f.path == "long.txt").unwrap();
    assert_eq!((file.additions, file.deletions), (2, 2), "one kept change plus one removed change");
}

#[tokio::test]
async fn a_dropped_binary_file_is_reported_with_a_note() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    let old = commit(&root, "f1 adds a binary");
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "d.txt", "1\n2\nD\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f1 without it");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    let got = paths(&snapshot);
    assert!(got.contains(&"blob.bin".to_owned()) && got.contains(&"d.txt".to_owned()), "{got:?}");
    assert!(snapshot.patch.contains("binary, rename or mode-only change no longer in the PR"), "{}", snapshot.patch);
    assert!(snapshot.patch.contains(mast_engine::diff_sync::since::REMOVED_LABEL));
}

#[tokio::test]
async fn dropping_one_of_two_identical_runs_is_seen() {
    let (_tmp, root, repos) = long_repo();
    git(&root, &["checkout", "-qb", "feature"]);
    // The same one-line insertion in two places: two runs with identical content.
    let with_dup = |lines: &[usize]| -> String {
        (1..=30).flat_map(|n| {
            let mut out = vec![format!("l{n}\n")];
            if lines.contains(&n) {
                out.push("DUP\n".to_owned());
            }
            out
        }).collect()
    };
    write(&root, "long.txt", &with_dup(&[5, 20]));
    let old = commit(&root, "f1");
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "long.txt", &with_dup(&[5]));
    let new = commit(&root, "f1 with one insertion");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    let label = mast_engine::diff_sync::since::REMOVED_LABEL;
    assert_eq!(snapshot.patch.matches(label).count(), 1, "the dropped duplicate is reported once:\n{}", snapshot.patch);
    assert!(snapshot.patch.contains("-DUP"));
}

#[tokio::test]
async fn a_dropped_rename_only_rs_file_is_noted_as_a_meta_line() {
    let (_tmp, root, repos) = repo();
    git(&root, &["checkout", "-qb", "feature"]);
    git(&root, &["mv", "d.txt", "moved.rs"]);
    let old = commit(&root, "f1 renames d.txt to moved.rs");
    git(&root, &["reset", "-q", "--hard", "main"]);
    write(&root, "c.txt", "1\n2\nC\n4\n5\n6\n7\n8\n9\n10\n");
    let new = commit(&root, "f1 without the rename");
    let snapshot = capture_since_diff(&repos, &root, "main", &old, &new).await.unwrap();
    assert!(paths(&snapshot).contains(&"moved.rs".to_owned()), "{:?}", paths(&snapshot));
    // The note is a `\` meta line (no line numbers), never a context line at line 0.
    assert!(snapshot.patch.contains("@@ -0,0 +0,0 @@ Removed since you reviewed\n\\ binary, rename or mode-only change no longer in the PR\n"), "{}", snapshot.patch);
}
