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
/// How long a commit that could not be fetched is not asked for again.
const FETCH_BACKOFF: std::time::Duration = std::time::Duration::from_secs(300);

fn counters() -> &'static Mutex<std::collections::HashMap<String, (u64, u64)>> {
    static COUNTERS: OnceLock<Mutex<std::collections::HashMap<String, (u64, u64)>>> = OnceLock::new();
    COUNTERS.get_or_init(Default::default)
}

fn bump(root: &Path, computes: u64, fetches: u64) {
    let mut counters = counters().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = counters.entry(root.display().to_string()).or_default();
    entry.0 += computes;
    entry.1 += fetches;
}

/// Range diffs computed (cache misses) for this repository since startup; tests assert hits compute nothing.
pub fn computes_for(root: &Path) -> u64 {
    counters().lock().unwrap_or_else(|p| p.into_inner()).get(&root.display().to_string()).map_or(0, |c| c.0)
}

/// Network fetches attempted for missing commits in this repository since startup.
pub fn fetch_attempts_for(root: &Path) -> u64 {
    counters().lock().unwrap_or_else(|p| p.into_inner()).get(&root.display().to_string()).map_or(0, |c| c.1)
}

fn failed_fetches() -> &'static Mutex<std::collections::HashMap<(String, String), std::time::Instant>> {
    static FAILED: OnceLock<Mutex<std::collections::HashMap<(String, String), std::time::Instant>>> = OnceLock::new();
    FAILED.get_or_init(Default::default)
}

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
    let key = (root.display().to_string(), sha.to_owned());
    let recently_failed = failed_fetches()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .is_some_and(|at| at.elapsed() < FETCH_BACKOFF);
    if recently_failed {
        return false;
    }
    bump(root, 0, 1);
    let (repo, wanted) = (root.to_path_buf(), sha.to_owned());
    let fetched = tokio::task::spawn_blocking(move || crate::diff_targets::fetch_commit(&repo, &wanted))
        .await
        .is_ok_and(|result| result.is_ok());
    let present = fetched && has_commit(root, sha).await;
    let mut failed = failed_fetches().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if present {
        failed.remove(&key);
    } else {
        failed.insert(key, std::time::Instant::now());
    }
    present
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
///
/// A plain `old..new` diff is only right when `old` is an ancestor of `new` and both
/// share the same merge-base with the base: otherwise (force-push, rebase, or the base
/// merged into the branch) the base's own changes would drown the author's, so the
/// interdiff is used.
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
    if base_ref.starts_with('-') || base_ref.is_empty() {
        return Err(SinceError::Failed("invalid base ref".into()));
    }
    // Answer from the cache before any git runs.
    let key = format!("{}\0{old}\0{new}\0{base_ref}", root.display());
    if let Some(hit) = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, snapshot)| snapshot.clone())
    {
        return Ok(hit);
    }
    if !ensure_commit(root, new).await {
        return Err(SinceError::Failed(format!("{new} is not available in this repository")));
    }
    if !ensure_commit(root, old).await {
        return Err(SinceError::OldUnavailable(old.to_owned()));
    }
    bump(root, 1, 0);
    let ancestor = capture_git(root, &["merge-base", "--is-ancestor", old, new], 64)
        .await
        .is_ok();
    let old_base = rev(root, &["merge-base", "--end-of-options", base_ref, old]).await;
    let new_base = rev(root, &["merge-base", "--end-of-options", base_ref, new]).await;
    let same_base = matches!((&old_base, &new_base), (Ok(a), Ok(b)) if a == b);
    let snapshot = if ancestor && same_base {
        capture_revision_pair_diff(repos, root, old, new).await.map_err(failed)?
    } else {
        let (old_base, new_base) = (old_base?, new_base?);
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
            let path = chunk_path(chunk);
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

/// Undo git's C-style quoting of a path (`"a\\tb"`, octal escapes for non-ASCII bytes).
fn unquote(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return raw.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'0'..=b'7' => {
                let end = (i + 3).min(bytes.len());
                let digits = &inner[i..end];
                let take = digits.bytes().take_while(|b| (b'0'..=b'7').contains(b)).count();
                out.push(u8::from_str_radix(&digits[..take], 8).unwrap_or(b'?'));
                i += take - 1;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file's new path from the chunk's own header lines (which carry the quoting git
/// applies), not by splitting `diff --git a/x b/y`, which is ambiguous for spaces.
fn chunk_path(chunk: &str) -> String {
    let header_end = chunk.find("\n@@ ").unwrap_or(chunk.len());
    let mut fallback = None;
    for line in chunk[..header_end].lines() {
        if let Some(path) = line.strip_prefix("rename to ") {
            return unquote(path);
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            if path != "/dev/null" {
                let path = unquote(path);
                return path.strip_prefix("b/").unwrap_or(&path).to_owned();
            }
        } else if let Some(path) = line.strip_prefix("--- ") {
            if path != "/dev/null" {
                let path = unquote(path);
                fallback = Some(path.strip_prefix("a/").unwrap_or(&path).to_owned());
            }
        }
    }
    fallback.unwrap_or_else(|| {
        let first = chunk.lines().next().unwrap_or_default();
        unquote(first.rsplit_once(" b/").map_or(first, |(_, p)| p))
    })
}

#[cfg(test)]
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

/// Label on every hunk of a removal group; the UI keys its "Removed since you reviewed" group on it.
pub const REMOVED_LABEL: &str = "Removed since you reviewed";

/// `@@ -a[,b] +c[,d] @@ ...` as ((a, b), (c, d)).
fn parse_hunk_ranges(header: &str) -> Option<((u32, u32), (u32, u32))> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    Some((range(old)?, range(new)?))
}

/// A contiguous block of changed lines inside a hunk, with the context git printed around it.
/// Runs, not hunks, are the unit of "removed": a hunk's grouping depends on its neighbours,
/// a run's content does not.
struct Run<'a> {
    key: [u8; 32],
    before: Vec<&'a str>,
    lines: Vec<&'a str>,
    after: Vec<&'a str>,
    /// Start lines (base side, head side) of the run including its leading context.
    old_start: u32,
    new_start: u32,
}

fn is_change(line: &str) -> bool {
    matches!(line.as_bytes().first(), Some(b'+' | b'-'))
}

fn runs_of<'a>(path: &str, hunk: &'a str) -> Vec<Run<'a>> {
    let mut lines = hunk.lines();
    let Some(((old_start, _), (new_start, _))) = lines.next().and_then(parse_hunk_ranges) else {
        return Vec::new();
    };
    let body: Vec<&str> = lines.collect();
    let (mut o, mut n) = (old_start, new_start);
    let mut runs = Vec::new();
    let mut i = 0;
    while i < body.len() {
        if !is_change(body[i]) {
            if !body[i].starts_with('\\') {
                o += 1;
                n += 1;
            }
            i += 1;
            continue;
        }
        let (run_o, run_n, first) = (o, n, i);
        while i < body.len() && (is_change(body[i]) || body[i].starts_with('\\')) {
            match body[i].as_bytes().first() {
                Some(b'-') => o += 1,
                Some(b'+') => n += 1,
                _ => {}
            }
            i += 1;
        }
        let mut before: Vec<&str> = body[..first].iter().rev().take_while(|l| l.starts_with(' ')).take(3).copied().collect();
        before.reverse();
        let after: Vec<&str> = body[i..].iter().take_while(|l| l.starts_with(' ')).take(3).copied().collect();
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        for line in &body[first..i] {
            if is_change(line) {
                hasher.update([0u8]);
                hasher.update(line.as_bytes());
            }
        }
        runs.push(Run {
            key: hasher.finalize().into(),
            old_start: run_o - before.len() as u32,
            new_start: run_n - before.len() as u32,
            before,
            lines: body[first..i].to_vec(),
            after,
        });
    }
    runs
}

/// A run the new head no longer has, as a reversion: additions become deletions (and the
/// reverse), the ranges swap, and the header carries [`REMOVED_LABEL`]. Returns the hunk and
/// its (additions, deletions) as shown.
fn reversed_run(run: &Run<'_>) -> (String, u32, u32) {
    let (mut adds, mut dels) = (0u32, 0u32);
    let mut body = String::new();
    for line in &run.before {
        body.push_str(line);
        body.push('\n');
    }
    for line in &run.lines {
        match line.as_bytes().first() {
            Some(b'+') => {
                dels += 1;
                body.push('-');
                body.push_str(&line[1..]);
            }
            Some(b'-') => {
                adds += 1;
                body.push('+');
                body.push_str(&line[1..]);
            }
            _ => body.push_str(line),
        }
        body.push('\n');
    }
    for line in &run.after {
        body.push_str(line);
        body.push('\n');
    }
    let context = (run.before.len() + run.after.len()) as u32;
    // Reversed: the head's lines (the old '+' side) are what the patch removes.
    let header = format!(
        "@@ -{},{} +{},{} @@ {REMOVED_LABEL}\n",
        run.new_start,
        context + dels,
        run.old_start,
        context + adds
    );
    (header + &body, adds, dels)
}

/// A run as an ordinary hunk (used when git regrouped a hunk and only some of its runs are new).
fn forward_run(run: &Run<'_>) -> String {
    let adds = run.lines.iter().filter(|l| l.starts_with('+')).count() as u32;
    let dels = run.lines.iter().filter(|l| l.starts_with('-')).count() as u32;
    let context = (run.before.len() + run.after.len()) as u32;
    let mut out = format!("@@ -{},{} +{},{} @@\n", run.old_start, context + dels, run.new_start, context + adds);
    for line in run.before.iter().chain(&run.lines).chain(&run.after) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn count_changes(hunk: &str) -> (u32, u32) {
    hunk.lines().skip(1).fold((0, 0), |(adds, dels), line| match line.as_bytes().first() {
        Some(b'+') => (adds + 1, dels),
        Some(b'-') => (adds, dels + 1),
        _ => (adds, dels),
    })
}

/// What changed between two heads that were each diffed against their own merge-base:
/// the new head's hunks the old head did not have, plus (as reversions labelled
/// [`REMOVED_LABEL`], in the same per-file block) the old head's changed-line runs the new
/// head no longer has. A dropped file with no hunks (binary, rename-only, mode-only) is
/// reported with a one-line note.
fn interdiff(before: &DiffSnapshot, mut after: DiffSnapshot) -> DiffSnapshot {
    let old_chunks = split_patch(&before.patch);
    let new_chunks = split_patch(&after.patch);
    // Runs as multisets: dropping one of two identical runs must still be seen.
    let mut old_runs: std::collections::HashMap<[u8; 32], u32> = std::collections::HashMap::new();
    for file in &old_chunks {
        for run in file.hunks.iter().flat_map(|hunk| runs_of(&file.path, hunk)) {
            *old_runs.entry(run.key).or_default() += 1;
        }
    }
    let mut present_runs: std::collections::HashMap<[u8; 32], u32> = std::collections::HashMap::new();
    for file in &new_chunks {
        for run in file.hunks.iter().flat_map(|hunk| runs_of(&file.path, hunk)) {
            *present_runs.entry(run.key).or_default() += 1;
        }
    }
    let before_headers: HashSet<&str> = old_chunks
        .iter()
        .filter(|file| file.hunks.is_empty())
        .map(|file| file.header)
        .collect();
    let new_paths: HashSet<&str> = new_chunks.iter().map(|file| file.path.as_str()).collect();

    // Removed runs per path (old-head order), each with its shown counts.
    let mut removed: Vec<(String, Vec<String>, u32, u32)> = Vec::new();
    for file in &old_chunks {
        let mut hunks = Vec::new();
        let (mut adds, mut dels) = (0u32, 0u32);
        for hunk in &file.hunks {
            for run in runs_of(&file.path, hunk) {
                if let Some(count) = present_runs.get_mut(&run.key).filter(|count| **count > 0) {
                    *count -= 1;
                    continue;
                }
                let (text, a, d) = reversed_run(&run);
                hunks.push(text);
                adds += a;
                dels += d;
            }
        }
        if file.hunks.is_empty() && !new_paths.contains(file.path.as_str()) {
            hunks.push(format!(
                "@@ -0,0 +0,0 @@ {REMOVED_LABEL}\n\\ binary, rename or mode-only change no longer in the PR\n"
            ));
        }
        if !hunks.is_empty() {
            removed.push((file.path.clone(), hunks, adds, dels));
        }
    }

    let mut patch = String::new();
    let mut counts: std::collections::HashMap<String, (u32, u32)> = std::collections::HashMap::new();
    for file in &new_chunks {
        // A new hunk is kept whole when none of its runs existed before; when only some are
        // new (git regrouped them), just the new runs are kept, so unchanged ones are not
        // shown as additions.
        let mut kept: Vec<String> = Vec::new();
        for hunk in &file.hunks {
            let runs = runs_of(&file.path, hunk);
            let fresh: Vec<bool> = runs
                .iter()
                .map(|run| match old_runs.get_mut(&run.key).filter(|count| **count > 0) {
                    Some(count) => {
                        *count -= 1;
                        false
                    }
                    None => true,
                })
                .collect();
            if fresh.iter().all(|new| *new) {
                if !runs.is_empty() {
                    kept.push((*hunk).to_owned());
                }
            } else {
                kept.extend(runs.iter().zip(&fresh).filter(|(_, new)| **new).map(|(run, _)| forward_run(run)));
            }
        }
        let headerless_change = file.hunks.is_empty() && !before_headers.contains(file.header);
        let dropped = removed.iter().position(|(path, ..)| *path == file.path);
        if kept.is_empty() && !headerless_change && dropped.is_none() {
            continue;
        }
        if kept.is_empty() && !headerless_change {
            // Only removals under this path: a minimal header, not the new head's.
            patch.push_str(&format!("diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n", file.path));
        } else {
            patch.push_str(file.header);
        }
        let (mut adds, mut dels) = (0u32, 0u32);
        for hunk in kept {
            patch.push_str(&hunk);
            let (a, d) = count_changes(&hunk);
            adds += a;
            dels += d;
        }
        if let Some(index) = dropped {
            let (_, hunks, a, d) = removed.remove(index);
            for hunk in hunks {
                patch.push_str(&hunk);
            }
            adds += a;
            dels += d;
        }
        counts.insert(file.path.clone(), (adds, dels));
    }
    // Paths only the old head touched: one block each.
    let mut removed_only: Vec<(String, u32, u32)> = Vec::new();
    for (path, hunks, adds, dels) in removed {
        patch.push_str(&format!("diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n", path));
        for hunk in hunks {
            patch.push_str(&hunk);
        }
        removed_only.push((path, adds, dels));
    }
    if patch.len() > MAX_PATCH_BYTES {
        let mut cut = MAX_PATCH_BYTES;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        let boundary = patch[..cut].rfind('\n').unwrap_or(0);
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
    // Files that exist only as removals (and removal-only blocks) still need a summary.
    for (path, additions, deletions) in removed_only {
        after.files.push(mast_proto::DiffFileSummary {
            path,
            old_path: None,
            status: "M".into(),
            additions,
            deletions,
            binary: false,
        });
    }
    for (path, (additions, deletions)) in &counts {
        if !after.files.iter().any(|file| &file.path == path) {
            after.files.push(mast_proto::DiffFileSummary {
                path: path.clone(),
                old_path: None,
                status: "M".into(),
                additions: *additions,
                deletions: *deletions,
                binary: false,
            });
        }
    }
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

    #[test]
    fn quoted_and_spaced_paths_are_parsed_from_the_headers() {
        assert_eq!(unquote("\"a\\tb\\\"c\""), "a\tb\"c");
        assert_eq!(unquote("\"caf\\303\\251.txt\""), "caf\u{e9}.txt");
        let spaced = "diff --git a/my file.txt b/my file.txt\n--- a/my file.txt\n+++ b/my file.txt\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(split_patch(spaced)[0].path, "my file.txt");
        let quoted = "diff --git \"a/t\\tb\" \"b/t\\tb\"\n--- \"a/t\\tb\"\n+++ \"b/t\\tb\"\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(split_patch(quoted)[0].path, "t\tb");
        let rename = "diff --git a/x b/y\nsimilarity index 90%\nrename from x\nrename to y\n";
        assert_eq!(split_patch(rename)[0].path, "y");
        let deleted = "diff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        assert_eq!(split_patch(deleted)[0].path, "gone");
    }

    fn snapshot(patch: String) -> DiffSnapshot {
        DiffSnapshot {
            git_status: None,
            branch: "b".into(),
            head_sha: Some("h".into()),
            patch,
            files: Vec::new(),
            submodules: Vec::new(),
            additions: 0,
            deletions: 0,
            truncated: false,
            checksum: String::new(),
        }
    }

    #[test]
    fn oversized_multibyte_patches_truncate_on_a_char_boundary() {
        // Three-byte characters, so the byte cap lands inside a character.
        let body = "\u{20ac}".repeat(1000);
        let mut text = String::from("diff --git a/f b/f\n--- a/f\n+++ b/f\n");
        for n in 0..(MAX_PATCH_BYTES / 2000 + 50) {
            text.push_str(&format!("@@ -{n} +{n} @@\n+{body}{n}\n"));
        }
        let out = interdiff(&snapshot(String::new()), snapshot(text));
        assert!(out.truncated);
        assert!(out.patch.len() <= MAX_PATCH_BYTES + 64);
        assert!(out.patch.ends_with("# Comet diff truncated\n"));
    }

    #[test]
    fn dropped_hunks_are_reversed_and_labelled() {
        let old = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -3,2 +3,2 @@\n ctx\n-a\n+fix\n";
        let out = interdiff(&snapshot(old.into()), snapshot(String::new()));
        assert!(out.patch.contains(&format!("@@ -3,2 +3,2 @@ {REMOVED_LABEL}\n ctx\n+a\n-fix\n")), "{}", out.patch);
        assert_eq!(out.files.len(), 1);
        assert_eq!((out.additions, out.deletions), (1, 1));
    }
}
