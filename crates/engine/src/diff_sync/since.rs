//! Re-review range diff: what a PR changed between the head a reviewer last
//! looked at (`old`) and the current head (`new`).
//!
//! - `old` is an ancestor of `new` (a plain push): `git diff old new`.
//! - Otherwise (force-push, rebase): an interdiff. Each head is diffed against
//!   its own merge-base with the PR base, and only the hunks of the new head
//!   whose changed lines do not appear in the old head's diff for the same file
//!   are kept, so a rebase onto a newer base does not drown the real changes.
//!
//! Missing commits are fetched once through the existing read-only fetch path
//! ([`crate::diff_targets::fetch_commit`]); a commit that cannot be fetched
//! (garbage-collected after a force-push) is reported as
//! [`SinceError::OldUnavailable`] so the caller can fall back to the full diff.
//! Results are cached per (repository, old, new, merge-bases) and bounded by
//! [`MAX_PATCH_BYTES`].

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use super::{DiffSnapshot, MAX_PATCH_BYTES, capture_git, capture_revision_pair_diff, valid_object_id};
use crate::repos::Repos;

/// Prefix of the RPC error when the old head cannot be found or fetched.
pub const OLD_UNAVAILABLE: &str = "old-head-unavailable";
const CACHE_ENTRIES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinceError {
    /// The previously reviewed head is not in the repository and could not be fetched.
    OldUnavailable(String),
    Failed(String),
}

impl std::fmt::Display for SinceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OldUnavailable(sha) => write!(f, "{OLD_UNAVAILABLE}: {sha} is no longer available"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

fn failed(error: impl std::fmt::Display) -> SinceError {
    SinceError::Failed(error.to_string())
}

fn cache() -> &'static Mutex<Vec<(String, DiffSnapshot)>> {
    static CACHE: OnceLock<Mutex<Vec<(String, DiffSnapshot)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

async fn has_commit(root: &Path, sha: &str) -> bool {
    capture_git(root, &["cat-file", "-e", &format!("{sha}^{{commit}}")], 64)
        .await
        .is_ok()
}

async fn ensure_commit(root: &Path, sha: &str) -> bool {
    if has_commit(root, sha).await {
        return true;
    }
    let (repo, wanted) = (root.to_path_buf(), sha.to_owned());
    let fetched = tokio::task::spawn_blocking(move || crate::diff_targets::fetch_commit(&repo, &wanted))
        .await
        .is_ok_and(|result| result.is_ok());
    fetched && has_commit(root, sha).await
}

async fn rev(root: &Path, args: &[&str]) -> Result<String, SinceError> {
    let capture = capture_git(root, args, 256).await.map_err(failed)?;
    let value = String::from_utf8_lossy(&capture.stdout).trim().to_owned();
    if value.is_empty() {
        return Err(SinceError::Failed(format!("git {args:?} returned nothing")));
    }
    Ok(value)
}

/// Diff `old` to `new` for a PR whose base is `base_ref` (a ref or sha).
pub async fn capture_since_diff(
    repos: &Repos,
    root: &Path,
    base_ref: &str,
    old: &str,
    new: &str,
) -> Result<DiffSnapshot, SinceError> {
    if !valid_object_id(old) || !valid_object_id(new) {
        return Err(SinceError::Failed("heads must be full commit ids".into()));
    }
    if !ensure_commit(root, new).await {
        return Err(SinceError::Failed(format!("{new} is not available in this repository")));
    }
    if !ensure_commit(root, old).await {
        return Err(SinceError::OldUnavailable(old.to_owned()));
    }
    let ancestor = capture_git(root, &["merge-base", "--is-ancestor", old, new], 64)
        .await
        .is_ok();
    let (old_base, new_base) = if ancestor {
        (String::new(), String::new())
    } else {
        (
            rev(root, &["merge-base", base_ref, old]).await?,
            rev(root, &["merge-base", base_ref, new]).await?,
        )
    };
    let key = format!("{}\0{old}\0{new}\0{old_base}\0{new_base}", root.display());
    if let Some(hit) = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, snapshot)| snapshot.clone())
    {
        return Ok(hit);
    }
    let snapshot = if ancestor {
        capture_revision_pair_diff(repos, root, old, new).await.map_err(failed)?
    } else {
        let before = capture_revision_pair_diff(repos, root, &old_base, old).await.map_err(failed)?;
        let after = capture_revision_pair_diff(repos, root, &new_base, new).await.map_err(failed)?;
        interdiff(&before, after)
    };
    let mut cache = cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= CACHE_ENTRIES {
        cache.remove(0);
    }
    cache.push((key, snapshot.clone()));
    Ok(snapshot)
}

struct FileChunk<'a> {
    path: String,
    header: &'a str,
    hunks: Vec<&'a str>,
}

fn split_patch(patch: &str) -> Vec<FileChunk<'_>> {
    let mut starts: Vec<usize> = patch.match_indices("diff --git ").map(|(i, _)| i).filter(|&i| i == 0 || patch.as_bytes()[i - 1] == b'\n').collect();
    starts.push(patch.len());
    starts
        .windows(2)
        .map(|w| {
            let chunk = &patch[w[0]..w[1]];
            let first_line = chunk.lines().next().unwrap_or_default();
            let path = first_line.rsplit_once(" b/").map_or(first_line, |(_, p)| p).to_owned();
            let mut hunk_starts: Vec<usize> = chunk
                .match_indices("@@ ")
                .map(|(i, _)| i)
                .filter(|&i| i > 0 && chunk.as_bytes()[i - 1] == b'\n')
                .collect();
            let header_end = hunk_starts.first().copied().unwrap_or(chunk.len());
            hunk_starts.push(chunk.len());
            FileChunk {
                path,
                header: &chunk[..header_end],
                hunks: hunk_starts.windows(2).map(|h| &chunk[h[0]..h[1]]).collect(),
            }
        })
        .collect()
}

/// Identity of a hunk's changed lines under a path (line numbers and context ignored).
fn hunk_key(path: &str, hunk: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    for line in hunk.lines().skip(1) {
        if matches!(line.as_bytes().first(), Some(b'+' | b'-')) {
            hasher.update([0u8]);
            hasher.update(line.as_bytes());
        }
    }
    hasher.finalize().into()
}

fn interdiff(before: &DiffSnapshot, mut after: DiffSnapshot) -> DiffSnapshot {
    let seen: HashSet<[u8; 32]> = split_patch(&before.patch)
        .iter()
        .flat_map(|file| file.hunks.iter().map(|hunk| hunk_key(&file.path, hunk)))
        .collect();
    let before_headers: HashSet<&str> = split_patch(&before.patch)
        .iter()
        .filter(|file| file.hunks.is_empty())
        .map(|file| file.header)
        .collect();
    let mut patch = String::new();
    let mut counts: std::collections::HashMap<String, (u32, u32)> = std::collections::HashMap::new();
    for file in split_patch(&after.patch) {
        let kept: Vec<&str> = file
            .hunks
            .iter()
            .copied()
            .filter(|hunk| !seen.contains(&hunk_key(&file.path, hunk)))
            .collect();
        let headerless_change = file.hunks.is_empty() && !before_headers.contains(file.header);
        if kept.is_empty() && !headerless_change {
            continue;
        }
        patch.push_str(file.header);
        let (mut adds, mut dels) = (0u32, 0u32);
        for hunk in kept {
            patch.push_str(hunk);
            for line in hunk.lines().skip(1) {
                match line.as_bytes().first() {
                    Some(b'+') => adds += 1,
                    Some(b'-') => dels += 1,
                    _ => {}
                }
            }
        }
        counts.insert(file.path, (adds, dels));
    }
    if patch.len() > MAX_PATCH_BYTES {
        let boundary = patch[..MAX_PATCH_BYTES].rfind('\n').unwrap_or(0);
        patch.truncate(boundary);
        patch.push_str("\n# Comet diff truncated\n");
        after.truncated = true;
    }
    after.files.retain_mut(|file| match counts.get(&file.path) {
        Some(&(adds, dels)) => {
            if !file.binary {
                file.additions = adds;
                file.deletions = dels;
            }
            true
        }
        None => false,
    });
    after.additions = after.files.iter().map(|f| f.additions).sum();
    after.deletions = after.files.iter().map(|f| f.deletions).sum();
    after.submodules.clear();
    after.patch = patch;
    let mut hasher = Sha256::new();
    hasher.update(b"since");
    hasher.update(after.head_sha.as_deref().unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(after.patch.as_bytes());
    after.checksum = crate::repos::hex(&hasher.finalize());
    after
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n ctx\n-a\n+b\n@@ -20 +20 @@\n-x\n+y\n";

    #[test]
    fn splits_files_and_hunks_and_ignores_line_numbers() {
        let files = split_patch(A);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "f");
        assert_eq!(files[0].hunks.len(), 2);
        let moved = "@@ -90,2 +90,2 @@\n other ctx\n-a\n+b\n";
        assert_eq!(hunk_key("f", files[0].hunks[0]), hunk_key("f", moved));
        assert_ne!(hunk_key("f", files[0].hunks[0]), hunk_key("g", moved));
    }
}
