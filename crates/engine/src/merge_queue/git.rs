//! The git commands the merge queue runs.
//!
//! Every command goes through [`mast_harness::git_command`] and the engine's git
//! gate, with hooks, commit signing and rerere off and inherited `GIT_*`
//! location variables removed, so a land runs no repository hook and never acts
//! on another repository than the one named.

use super::MergeQueueError;
use std::{
    path::{Path, PathBuf},
    process::Output,
};

/// The object id `update-ref` reads as "this ref must not exist yet".
pub(super) const ZERO_OID: &str = "0000000000000000000000000000000000000000";

const AUTHOR_NAME: &str = "Mast MergeQueue";
const AUTHOR_EMAIL: &str = "merge-queue@mast.invalid";

fn git_error(args: &[&str], detail: impl std::fmt::Display) -> MergeQueueError {
    MergeQueueError::Git(format!("git {}: {detail}", args.join(" ")))
}

/// Run git in `dir` and return its output whatever the exit status.
pub(super) fn output(dir: &Path, args: &[&str]) -> Result<Output, MergeQueueError> {
    let mut command = mast_harness::git_command();
    command
        .current_dir(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "rerere.enabled=false",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(name);
    }
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL);
    let _permit = crate::git_gate::acquire_blocking("merge_queue:git");
    command.output().map_err(|e| git_error(args, e))
}

fn failure(args: &[&str], out: &Output) -> MergeQueueError {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    git_error(args, format!("{} ({})", detail, out.status))
}

/// Run git in `dir`; a non-zero exit is an error. Returns trimmed stdout.
pub(super) fn run(dir: &Path, args: &[&str]) -> Result<String, MergeQueueError> {
    let out = output(dir, args)?;
    if !out.status.success() {
        return Err(failure(args, &out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn nul_list(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// The repository's common git directory, canonical: every worktree of one
/// repository maps to the same value, so they share one land lane per ref.
pub(super) fn common_dir(repo: &Path) -> Result<PathBuf, MergeQueueError> {
    let text = run(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    std::fs::canonicalize(&text).map_err(|e| MergeQueueError::Git(format!("{text}: {e}")))
}

/// The full commit id `revision` names, or `None` when it names no commit.
pub(super) fn commit_id(dir: &Path, revision: &str) -> Result<Option<String>, MergeQueueError> {
    let spec = format!("{revision}^{{commit}}");
    let out = output(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(failure(&["rev-parse", &spec], &out)),
    }
}

/// Whether `ancestor` is reachable from `descendant`.
pub(super) fn is_ancestor(
    dir: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, MergeQueueError> {
    let args = ["merge-base", "--is-ancestor", ancestor, descendant];
    let out = output(dir, &args)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failure(&args, &out)),
    }
}

/// Merge `theirs` into `ours` in the object database only (no worktree, index
/// or ref is touched). Returns the conflicting paths; empty means clean.
pub(super) fn merge_tree_conflicts(
    dir: &Path,
    ours: &str,
    theirs: &str,
) -> Result<Vec<String>, MergeQueueError> {
    let args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        "-z",
        ours,
        theirs,
    ];
    let out = output(dir, &args)?;
    match out.status.code() {
        Some(0) => Ok(Vec::new()),
        Some(1) => {
            // The first field is the tree id, the rest are conflicted paths.
            let mut paths: Vec<String> = nul_list(&out.stdout).into_iter().skip(1).collect();
            paths.sort();
            paths.dedup();
            Ok(paths)
        }
        _ => Err(failure(&args, &out)),
    }
}

/// Paths left unmerged in a worktree's index.
pub(super) fn unmerged_paths(worktree: &Path) -> Result<Vec<String>, MergeQueueError> {
    let args = ["diff", "--name-only", "--diff-filter=U", "-z"];
    let out = output(worktree, &args)?;
    if !out.status.success() {
        return Err(failure(&args, &out));
    }
    Ok(nul_list(&out.stdout))
}

/// The worktree that has `full_ref` checked out, if any. Moving such a ref
/// would leave that worktree's index and files behind it.
pub(super) fn checked_out_at(
    dir: &Path,
    full_ref: &str,
) -> Result<Option<String>, MergeQueueError> {
    let listing = run(dir, &["worktree", "list", "--porcelain"])?;
    let mut worktree = "";
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktree = path;
        } else if line.strip_prefix("branch ") == Some(full_ref) {
            return Ok(Some(worktree.to_owned()));
        }
    }
    Ok(None)
}

/// Compare-and-swap `full_ref` from `expected_old` to `new`. `Ok(Err(why))`
/// means git refused and the ref was not changed.
pub(super) fn compare_and_swap(
    dir: &Path,
    full_ref: &str,
    new: &str,
    expected_old: &str,
    log: &str,
) -> Result<Result<(), String>, MergeQueueError> {
    let args = [
        "update-ref",
        "--no-deref",
        "-m",
        log,
        full_ref,
        new,
        expected_old,
    ];
    let out = output(dir, &args)?;
    if out.status.success() {
        Ok(Ok(()))
    } else {
        Ok(Err(failure(&args, &out).to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_lists_drop_empty_fields() {
        assert_eq!(nul_list(b"abc\0x y\0\0"), vec!["abc", "x y"]);
    }
}
