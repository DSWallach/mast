//! Extract and re-anchor code reference snippets.

use mast_proto::CodeRefRange;
use sha2::{Digest, Sha256};

/// A draft reference and the text captured from its revision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Bound queued reference work across the application, not just per chat.
pub fn try_queue_io() -> Option<tokio::sync::OwnedSemaphorePermit> {
    static QUEUE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    QUEUE
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
        .ok()
}

/// Bound all background workspace reads caused by reference staging and resolution.
pub async fn acquire_io_permit() -> tokio::sync::OwnedSemaphorePermit {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    PERMITS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .expect("reference semaphore remains open")
}

/// Stable cache key computed during message projection, outside the render path.
pub fn resolution_key(reference: &StagedCodeRef) -> String {
    let value = serde_json::to_string(&reference.reference).unwrap_or_default();
    content_hash(&format!("{value}\n{}", reference.snippet))
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
            encoded, reference.location.workspace_path(), range, revision, fence, staged.snippet, fence
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
            .as_chunks::<2>().0.iter()
            .map(|pair| {
                let value = std::str::from_utf8(pair).ok()?;
                u8::from_str_radix(value, 16).ok()
            })
            .collect();
        let reference: mast_proto::CodeRef = serde_json::from_slice(&bytes?).ok()?;
        let (_, block) = tail.split_once('\n')?;
        let (fence, body) = block.split_once('\n')?;
        if fence.len() < 3 || !fence.bytes().all(|byte| byte == b'`') {
            return None;
        }
        let (snippet, rest) = body.split_once(&format!("\n{fence}{END}"))?;
        let snippet = snippet.to_string();
        let staged = StagedCodeRef {
            id: String::new(),
            reference,
            snippet,
        };
        if staged.reference.kind == mast_proto::CodeRefKind::File
            && let Some(span) = spans.iter_mut().find(|span| {
                span.path.as_ref() == staged.reference.location.path && span.code_ref.is_none()
            }) {
                span.resolution_key = Some(resolution_key(&staged).into());
                span.code_ref = Some(std::sync::Arc::new(staged));
                remaining = rest;
                continue;
            }
        if !result.is_empty() {
            result.push(' ');
        }
        let start = result.len();
        result.push_str(&staged.label());
        let end = result.len();
        let path = match staged.reference.range {
            Some(range) => format!(
                "{}:{}",
                staged.reference.location.workspace_path(),
                range.start_line
            ),
            None => staged.reference.location.workspace_path(),
        };
        spans.push(crate::composer::SentMentionSpan {
            range: start..end,
            path: path.into(),
            is_dir: false,
            resolution_key: Some(resolution_key(&staged).into()),
            code_ref: Some(std::sync::Arc::new(staged)),
            resolution: None,
        });
        remaining = rest;
    }
    if !remaining.is_empty() {
        return None;
    }
    Some((result, spans))
}

pub fn pinned_link(reference: &StagedCodeRef) -> String {
    let bytes = serde_json::to_vec(reference).expect("reference serializes");
    format!(
        "mast-code-ref:{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub fn decode_pinned_link(target: &str) -> Option<StagedCodeRef> {
    let encoded = target.strip_prefix("mast-code-ref:")?;
    if encoded.len() % 2 != 0 || encoded.len() > 20_000_000 {
        return None;
    }
    let bytes: Option<Vec<_>> = encoded
        .as_bytes()
        .as_chunks::<2>().0.iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    serde_json::from_slice(&bytes?).ok()
}

/// Hash the full file so an unchanged working tree is recognized cheaply.
pub fn content_hash(contents: &str) -> String {
    format!("{:x}", Sha256::digest(contents.as_bytes()))
}

/// Convert a nonempty UTF-8 byte selection to an inclusive line range.
pub fn selection_lines(contents: &str, selection: std::ops::Range<usize>) -> Option<CodeRefRange> {
    if selection.start >= selection.end
        || selection.end > contents.len()
        || !contents.is_char_boundary(selection.start)
        || !contents.is_char_boundary(selection.end)
    {
        return None;
    }
    let line_at = |offset| {
        contents.as_bytes()[..offset]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u32
            + 1
    };
    Some(CodeRefRange {
        start_line: line_at(selection.start),
        end_line: line_at(selection.end - 1),
    })
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
    if snippet(contents, original).as_deref() == Some(pinned_snippet) {
        return Resolution::Unchanged(original);
    }
    let file: Vec<_> = contents.lines().collect();
    let target: Vec<_> = if pinned_snippet.is_empty() {
        vec![""]
    } else {
        pinned_snippet.lines().collect()
    };
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

/// Resolve the repository identity on the device that owns the workspace.
pub async fn repository_identity(
    engine: &crate::state::EngineHandle,
    context: &crate::files::client::FilesRequestContext,
) -> Option<mast_proto::ExecutionRepositoryIdentity> {
    let params = mast_proto::DescribeExecutionRepositoryParams {
        path: context.cwd.clone(),
        target_device_id: context.target_device_id.clone(),
    };
    let value = engine
        .client()
        .call(
            mast_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY,
            serde_json::to_value(params).ok()?,
        )
        .await
        .ok()?;
    let identity: mast_proto::ExecutionRepositoryIdentity = serde_json::from_value(value).ok()?;
    if identity.repo_id.is_empty()
        || context
            .checkout_id
            .as_ref()
            .is_some_and(|expected| expected != &identity.checkout_id)
    {
        return None;
    }
    Some(identity)
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
                    repo_root: None,
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
        assert_eq!(state.code_refs("chat"), std::slice::from_ref(&staged));
        assert!(state.code_refs("other").is_empty());
        state.remove_code_ref("chat", "reference");
        assert!(state.code_refs("chat").is_empty());
        state.add_code_ref("chat", staged.clone());
        let taken = state.take_code_refs("chat");
        assert_eq!(taken, vec![staged]);
        let pinned = pinned_link(&taken[0]);
        assert_eq!(decode_pinned_link(&pinned), Some(taken[0].clone()));
        assert!(decode_pinned_link("mast-code-ref:xyz").is_none());
        let prompt = with_code_refs("Review", &taken);
        assert!(
            prompt.contains("src/main.rs:2-3 @ working tree\n```\nlet x = 1;\nlet y = 2;\n```")
        );
        let (display, spans) = project_sent_refs(&prompt).unwrap();
        assert_eq!(display, "Review main.rs:2-3");
        assert_eq!(&display[spans[0].range.clone()], "main.rs:2-3");
        assert_eq!(spans[0].path.as_ref(), "src/main.rs:2");
        assert!(project_sent_refs("legacy message").is_none());
        let mut file = taken[0].clone();
        file.reference.kind = mast_proto::CodeRefKind::File;
        file.reference.range = None;
        file.snippet = "text\n<!-- /mast-code-ref -->\n```".into();
        let prompt = with_code_refs("Review [main.rs](mast-file:src/main.rs)", &[file.clone()]);
        let (display, spans) = project_sent_refs(&prompt).unwrap();
        let (legacy_display, _) =
            crate::composer::sent_mention_display("Review [main.rs](mast-file:src/main.rs)")
                .unwrap();
        assert_eq!(display, legacy_display);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].code_ref.as_ref().unwrap().snippet, file.snippet);
    }

    #[test]
    fn line_selection_handles_utf8_and_exclusive_end() {
        assert_eq!(
            selection_lines("a\né\nc", 2..5),
            Some(CodeRefRange {
                start_line: 2,
                end_line: 2
            })
        );
        assert_eq!(
            selection_lines("a\né\nc", 2..6),
            Some(CodeRefRange {
                start_line: 2,
                end_line: 3
            })
        );
        assert_eq!(selection_lines("a\né\nc", 3..4), None);
        assert_eq!(selection_lines("a", 0..0), None);
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
