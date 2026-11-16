//! Extract and re-anchor code reference snippets.

use mast_proto::CodeRefRange;
use sha2::{Digest, Sha256};

/// A draft reference and the text captured from its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCodeRef {
    pub id: String,
    pub reference: mast_proto::CodeRef,
    pub snippet: String,
}

impl StagedCodeRef {
    pub fn label(&self) -> String {
        let path = self
            .reference
            .location
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&self.reference.location.path);
        match self.reference.range {
            Some(range) if range.start_line != range.end_line => {
                format!("{path}:{}-{}", range.start_line, range.end_line)
            }
            Some(range) => format!("{path}:{}", range.start_line),
            None => path.to_string(),
        }
    }
}

/// Hash the full file so an unchanged working tree is recognized cheaply.
pub fn content_hash(contents: &str) -> String {
    format!("{:x}", Sha256::digest(contents.as_bytes()))
}

/// Extract inclusive, one-based lines. Invalid or incomplete ranges are rejected.
pub fn snippet(contents: &str, range: CodeRefRange) -> Option<String> {
    if range.start_line == 0 || range.end_line < range.start_line {
        return None;
    }
    let lines: Vec<_> = contents.lines().collect();
    let start = (range.start_line - 1) as usize;
    let end = range.end_line as usize;
    Some(lines.get(start..end)?.join("\n"))
}

/// Read a file at a pinned commit. Call this on a background thread.
pub fn read_commit_file(
    repo: &std::path::Path,
    commit: &str,
    path: &str,
) -> std::io::Result<String> {
    if commit.is_empty()
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid revision or path",
        ));
    }
    let output = mast_harness::git_command()
        .current_dir(repo)
        .args(["show", &format!("{commit}:{path}")])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found at revision",
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Unchanged(CodeRefRange),
    Moved(CodeRefRange),
    Changed,
}

/// Search complete lines only. Ambiguous matches are treated as changed rather
/// than silently navigating to an arbitrary occurrence.
pub fn reanchor(contents: &str, original: CodeRefRange, pinned_snippet: &str) -> Resolution {
    if pinned_snippet.is_empty() {
        return Resolution::Changed;
    }
    if snippet(contents, original).as_deref() == Some(pinned_snippet) {
        return Resolution::Unchanged(original);
    }
    let file: Vec<_> = contents.lines().collect();
    let target: Vec<_> = pinned_snippet.lines().collect();
    let matches: Vec<_> = file
        .windows(target.len())
        .enumerate()
        .filter_map(|(index, lines)| (lines == target.as_slice()).then_some(index))
        .take(2)
        .collect();
    if let [index] = matches.as_slice() {
        let start_line = *index as u32 + 1;
        Resolution::Moved(CodeRefRange {
            start_line,
            end_line: start_line + target.len() as u32 - 1,
        })
    } else {
        Resolution::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_at_commit_after_worktree_changes() {
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let output = mast_harness::git_command()
                .current_dir(repo.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(repo.path().join("file.rs"), "before\n").unwrap();
        run(&["add", "file.rs"]);
        run(&["commit", "-qm", "fixture"]);
        let commit = run(&["rev-parse", "HEAD"]);
        std::fs::write(repo.path().join("file.rs"), "after\n").unwrap();
        assert_eq!(
            read_commit_file(repo.path(), &commit, "file.rs").unwrap(),
            "before\n"
        );
        assert!(read_commit_file(repo.path(), &commit, "../file.rs").is_err());
    }

    #[test]
    fn resolves_unchanged_shifted_edited_and_deleted_lines() {
        let range = CodeRefRange {
            start_line: 2,
            end_line: 3,
        };
        let text = "one\ntwo\nthree\nfour\n";
        let pinned = snippet(text, range).unwrap();
        assert_eq!(reanchor(text, range, &pinned), Resolution::Unchanged(range));
        assert_eq!(
            reanchor(&format!("zero\n{text}"), range, &pinned),
            Resolution::Moved(CodeRefRange {
                start_line: 3,
                end_line: 4
            })
        );
        assert_eq!(
            reanchor("one\ntwo\nchanged\nfour", range, &pinned),
            Resolution::Changed
        );
        assert_eq!(reanchor("one\nfour", range, &pinned), Resolution::Changed);
        assert_ne!(content_hash(text), content_hash("one\nfour"));
    }
}
