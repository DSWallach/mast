//! Validate a review against the locally available pull request diff.
use std::path::Path;

use mast_proto::{ReviewAnchor, ReviewSide, ReviewSidecar};
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct Hunk {
    old_start: u32,
    old_end: u32,
    new_start: u32,
    new_end: u32,
    hash: String,
}

fn git(repository: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = mast_harness::git_command()
        .current_dir(repository)
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn commit(repository: &Path, sha: &str) -> Result<String, String> {
    if !(sha.len() == 40 || sha.len() == 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid full commit SHA: {sha}"));
    }
    let value = git(
        repository,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
    )?;
    Ok(String::from_utf8_lossy(&value).trim().to_owned())
}

fn range(value: &str) -> Option<(u32, u32)> {
    let (start, count) = match value.split_once(',') {
        Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
        None => (value.parse::<u32>().ok()?, 1),
    };
    Some((
        start,
        if count == 0 {
            start.saturating_sub(1)
        } else {
            start.checked_add(count - 1)?
        },
    ))
}

fn parse_hunks(patch: &str) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current: Option<(Hunk, String)> = None;
    for line in patch.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("@@ -") {
            if let Some((mut hunk, lines)) = current.take() {
                hunk.hash = format!("{:x}", Sha256::digest(lines.as_bytes()));
                hunks.push(hunk);
            }
            let mut fields = header.split_whitespace();
            let parsed = fields.next().and_then(range).zip(
                fields
                    .next()
                    .and_then(|s| s.strip_prefix('+'))
                    .and_then(range),
            );
            if let Some(((old_start, old_end), (new_start, new_end))) = parsed {
                current = Some((
                    Hunk {
                        old_start,
                        old_end,
                        new_start,
                        new_end,
                        hash: String::new(),
                    },
                    String::new(),
                ));
            }
        } else if let Some((_, lines)) = current.as_mut() {
            if line.starts_with([' ', '+', '-', '\\']) {
                lines.push_str(line);
            }
        }
    }
    if let Some((mut hunk, lines)) = current {
        hunk.hash = format!("{:x}", Sha256::digest(lines.as_bytes()));
        hunks.push(hunk);
    }
    hunks
}

fn matching_hunk(hunks: &[Hunk], side: ReviewSide, start: u32, end: u32) -> Option<&Hunk> {
    if start == 0 || end < start {
        return None;
    }
    hunks.iter().find(|hunk| {
        let (first, last) = match side {
            ReviewSide::Left => (hunk.old_start, hunk.old_end),
            ReviewSide::Right => (hunk.new_start, hunk.new_end),
        };
        first <= start && end <= last
    })
}

/// Verify the review's commit identity and all ranges, and stamp hashes derived
/// from the diff, never trusting hashes supplied by the publishing agent.
pub fn validate_review(
    repository: &Path,
    review: &mut ReviewSidecar,
    expected_head: &str,
) -> Result<(), String> {
    if review.schema != "mast.review/v1" {
        return Err("schema must be mast.review/v1".into());
    }
    let expected = commit(repository, expected_head)?;
    let head = commit(repository, &review.pr.head_sha)?;
    if head != expected {
        return Err(format!(
            "review head {} does not match requested head {expected}",
            review.pr.head_sha
        ));
    }
    let base = commit(repository, &review.pr.base_sha)?;
    let diff = format!("{base}...{head}");
    let files = git(
        repository,
        &[
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--name-only",
            "-z",
            &diff,
            "--",
        ],
    )?;
    let mut entries = Vec::new();
    for file in files
        .split(|byte| *byte == 0)
        .filter(|file| !file.is_empty())
    {
        let file = std::str::from_utf8(file)
            .map_err(|_| "diff contains a non-UTF-8 path")?
            .to_owned();
        let pathspec = format!(":(literal){file}");
        let patch = git(
            repository,
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--no-renames",
                "--unified=3",
                &diff,
                "--",
                &pathspec,
            ],
        )?;
        entries.push((file, parse_hunks(&String::from_utf8_lossy(&patch))));
    }
    let valid = || {
        let mut text = String::from("Valid hunks per file:");
        for (file, hunks) in &entries {
            text.push_str(&format!("\n{file}:"));
            for hunk in hunks {
                text.push_str(&format!(
                    " left {}-{}, right {}-{};",
                    hunk.old_start, hunk.old_end, hunk.new_start, hunk.new_end
                ));
            }
            if hunks.is_empty() {
                text.push_str(" (no text hunks)");
            }
        }
        text
    };
    for step in &mut review.steps {
        for hunk in &mut step.hunks {
            let found = entries
                .iter()
                .find(|(file, _)| file == &step.file)
                .and_then(|(_, hunks)| {
                    matching_hunk(hunks, ReviewSide::Right, hunk.new_start, hunk.new_end)
                });
            let found = found.ok_or_else(|| {
                format!(
                    "Invalid step hunk {}:{}-{}. {}",
                    step.file,
                    hunk.new_start,
                    hunk.new_end,
                    valid()
                )
            })?;
            hunk.hunk_hash = Some(found.hash.clone());
        }
    }
    let validate_anchor = |anchor: &mut ReviewAnchor| -> Result<(), String> {
        let found = entries
            .iter()
            .find(|(file, _)| file == &anchor.file)
            .and_then(|(_, hunks)| {
                matching_hunk(hunks, anchor.side, anchor.start_line, anchor.end_line)
            });
        let found = found.ok_or_else(|| {
            format!(
                "Invalid anchor {}:{}-{} ({:?}). {}",
                anchor.file,
                anchor.start_line,
                anchor.end_line,
                anchor.side,
                valid()
            )
        })?;
        anchor.hunk_hash = Some(found.hash.clone());
        Ok(())
    };
    for highlight in &mut review.highlights {
        validate_anchor(&mut highlight.anchor)?;
    }
    for comment in &mut review.suggested_comments {
        validate_anchor(&mut comment.anchor)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ReviewAnchor, ReviewHighlight, ReviewHighlightKind, ReviewPullRequest};

    fn fixture() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q"]).unwrap();
        git(path, &["config", "user.email", "test@example.invalid"]).unwrap();
        git(path, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(path.join("file.txt"), "one\ntwo\nthree\n").unwrap();
        git(path, &["add", "file.txt"]).unwrap();
        git(path, &["commit", "-qm", "base"]).unwrap();
        let base = String::from_utf8(git(path, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        std::fs::write(path.join("file.txt"), "one\nchanged\nthree\n").unwrap();
        git(path, &["commit", "-qam", "head"]).unwrap();
        let head = String::from_utf8(git(path, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        (dir, base, head)
    }

    fn review(base: &str, head: &str) -> ReviewSidecar {
        ReviewSidecar {
            schema: "mast.review/v1".into(),
            pr: ReviewPullRequest {
                repo: "owner/repo".into(),
                number: 1,
                head_sha: head.into(),
                base_sha: base.into(),
            },
            summary: "Change a line".into(),
            verdict_hint: None,
            steps: vec![],
            highlights: vec![ReviewHighlight {
                kind: ReviewHighlightKind::Risk,
                severity: "low".into(),
                text: "Check".into(),
                anchor: ReviewAnchor {
                    file: "file.txt".into(),
                    side: ReviewSide::Right,
                    start_line: 2,
                    end_line: 2,
                    hunk_hash: None,
                },
            }],
            questions: vec![],
            suggested_comments: vec![],
        }
    }

    #[test]
    fn review_anchor_validation() {
        let (dir, base, head) = fixture();
        let mut good = review(&base, &head);
        validate_review(dir.path(), &mut good, &head).unwrap();
        assert_eq!(
            good.highlights[0].anchor.hunk_hash.as_ref().unwrap().len(),
            64
        );
        let mut bad = review(&base, &head);
        bad.highlights[0].anchor.file = "missing".into();
        let error = validate_review(dir.path(), &mut bad, &head).unwrap_err();
        assert!(error.contains("Valid hunks per file:") && error.contains("file.txt"));
        bad.highlights[0].anchor.file = "file.txt".into();
        bad.highlights[0].anchor.start_line = 100;
        bad.highlights[0].anchor.end_line = 100;
        assert!(
            validate_review(dir.path(), &mut bad, &head)
                .unwrap_err()
                .contains("right 1-3")
        );
        let mut wrong = review(&base, &base);
        assert!(
            validate_review(dir.path(), &mut wrong, &head)
                .unwrap_err()
                .contains("does not match")
        );
    }
}
