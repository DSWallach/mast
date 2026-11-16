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
        match (self.reference.kind, self.reference.range) {
            (mast_proto::CodeRefKind::Hunk, Some(range)) => {
                format!("Hunk · {path}:{}", range.start_line)
            }
            (_, Some(range)) if range.start_line != range.end_line => {
                format!("{path}:{}-{}", range.start_line, range.end_line)
            }
            (_, Some(range)) => format!("{path}:{}", range.start_line),
            (_, None) => path.to_string(),
        }
    }
}

/// Append a readable snapshot and structured reference metadata to a prompt.
pub fn with_code_refs(text: &str, references: &[StagedCodeRef]) -> String {
    use std::fmt::Write as _;
    let mut output = text.to_owned();
    for staged in references {
        let reference = &staged.reference;
        let revision = match &reference.at {
            mast_proto::CodeRefRevision::Commit { commit } => commit.as_str(),
            mast_proto::CodeRefRevision::WorkingTree { .. } => "working tree",
        };
        let range = reference
            .range
            .map(|range| format!(":{}-{}", range.start_line, range.end_line))
            .unwrap_or_default();
        let metadata = serde_json::to_vec(reference).expect("code reference serializes");
        let encoded: String = metadata.iter().map(|byte| format!("{byte:02x}")).collect();
        let fence = "`".repeat(
            staged
                .snippet
                .split(|character| character != '`')
                .map(str::len)
                .max()
                .unwrap_or(0)
                .max(2)
                + 1,
        );
        let _ = write!(
            output,
            "\n\n<!-- mast-code-ref:{} -->\n{}{} @ {}\n{}\n{}\n{}\n<!-- /mast-code-ref -->",
            encoded, reference.location.path, range, revision, fence, staged.snippet, fence
        );
    }
    output
}

/// Project encoded reference blocks into transcript labels, leaving legacy text untouched.
pub fn project_sent_refs(raw: &str) -> Option<(String, Vec<crate::composer::SentMentionSpan>)> {
    const OPEN: &str = "\n\n<!-- mast-code-ref:";
    const END: &str = "\n<!-- /mast-code-ref -->";
    let first = raw.find(OPEN)?;
    let prefix = &raw[..first];
    let (mut result, mut spans) = crate::composer::sent_mention_display(prefix)
        .unwrap_or_else(|| (prefix.to_string(), Vec::new()));
    let mut remaining = &raw[first..];
    while let Some(tail) = remaining.strip_prefix(OPEN) {
        let (encoded, tail) = tail.split_once(" -->\n")?;
        if encoded.len() % 2 != 0 {
            return None;
        }
        let bytes: Option<Vec<_>> = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let value = std::str::from_utf8(pair).ok()?;
                u8::from_str_radix(value, 16).ok()
            })
            .collect();
        let reference: mast_proto::CodeRef = serde_json::from_slice(&bytes?).ok()?;
        let (block, rest) = tail.split_once(END)?;
        let (_, block) = block.split_once('\n')?;
        let (fence, body) = block.split_once('\n')?;
        if fence.len() < 3 || !fence.bytes().all(|byte| byte == b'`') {
            return None;
        }
        let snippet = body.strip_suffix(&format!("\n{fence}"))?.to_string();
        if !result.is_empty() {
            result.push(' ');
        }
        let staged = StagedCodeRef {
            id: String::new(),
            reference,
            snippet,
        };
        let start = result.len();
        result.push_str(&staged.label());
        let end = result.len();
        let path = match staged.reference.range {
            Some(range) => format!("{}:{}", staged.reference.location.path, range.start_line),
            None => staged.reference.location.path.clone(),
        };
        spans.push(crate::composer::SentMentionSpan {
            range: start..end,
            path: path.into(),
            is_dir: false,
            code_ref: Some(staged),
        });
        remaining = rest;
    }
    if !remaining.is_empty() {
        return None;
    }
    Some((result, spans))
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
    fn prompt_preserves_reference_and_projects_transcript_chip() {
        let staged = StagedCodeRef {
            id: "reference".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation {
                    repo_id: "repo".into(),
                    worktree_id: None,
                    path: "src/main.rs".into(),
                },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(CodeRefRange {
                    start_line: 2,
                    end_line: 3,
                }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree {
                    working_tree: true,
                    content_hash: "hash".into(),
                },
            },
            snippet: "let x = 1;\nlet y = 2;".into(),
        };
        let mut state = crate::state::AppState::new();
        state.add_code_ref("chat", staged.clone());
        assert_eq!(state.code_refs("chat"), &[staged.clone()]);
        assert!(state.code_refs("other").is_empty());
        state.remove_code_ref("chat", "reference");
        assert!(state.code_refs("chat").is_empty());
        state.add_code_ref("chat", staged.clone());
        let taken = state.take_code_refs("chat");
        assert_eq!(taken, vec![staged]);
        let prompt = with_code_refs("Review", &taken);
        assert!(
            prompt.contains("src/main.rs:2-3 @ working tree\n```\nlet x = 1;\nlet y = 2;\n```")
        );
        let (display, spans) = project_sent_refs(&prompt).unwrap();
        assert_eq!(display, "Review main.rs:2-3");
        assert_eq!(&display[spans[0].range.clone()], "main.rs:2-3");
        assert_eq!(spans[0].path.as_ref(), "src/main.rs:2");
        assert!(project_sent_refs("legacy message").is_none());
    }

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
