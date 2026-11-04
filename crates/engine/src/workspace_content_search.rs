//! In-process, bounded content search for a resolved workspace checkout.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{SearcherBuilder, sinks::UTF8};
use ignore::{WalkBuilder, overrides::OverrideBuilder};

pub const MAX_MATCHES: usize = 2_000;
pub const MAX_FILES: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct ContentSearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Restrict results to workspace-relative paths, when supplied.
    pub changed_files: Option<std::collections::HashSet<PathBuf>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSearchMatch {
    pub path: PathBuf,
    pub line: u64,
    pub preview: String,
    /// Byte ranges in `preview`.
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentSearchSummary {
    pub matches: usize,
    pub files: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Calls `emit` as matches arrive. Returning false stops the search.
pub fn search_workspace_content(
    root: &Path,
    query: &ContentSearchQuery,
    cancelled: &AtomicBool,
    mut emit: impl FnMut(ContentSearchMatch) -> bool,
) -> Result<ContentSearchSummary, String> {
    if query.text.is_empty() {
        return Ok(ContentSearchSummary::default());
    }
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!query.case_sensitive)
        .word(query.whole_word)
        .fixed_strings(!query.regex)
        .build(&query.text)
        .map_err(|error| error.to_string())?;
    let mut overrides = OverrideBuilder::new(root);
    for glob in &query.include {
        overrides.add(glob).map_err(|error| error.to_string())?;
    }
    for glob in &query.exclude {
        overrides
            .add(&format!("!{glob}"))
            .map_err(|error| error.to_string())?;
    }
    let overrides = overrides.build().map_err(|error| error.to_string())?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .heap_limit(Some(1024 * 1024))
        .binary_detection(grep_searcher::BinaryDetection::quit(b'\0'))
        .build();
    let mut summary = ContentSearchSummary::default();
    let mut stopped = false;
    for entry in WalkBuilder::new(root)
        .overrides(overrides)
        .follow_links(false)
        .hidden(false)
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
        .build()
    {
        if cancelled.load(Ordering::Relaxed) || stopped {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        if relative
            .to_str()
            .and_then(|path| crate::workspace_files::WorkspaceRelativePath::file(path).ok())
            .is_none()
        {
            continue;
        }
        if query
            .changed_files
            .as_ref()
            .is_some_and(|files| !files.contains(relative))
        {
            continue;
        }
        let mut file_has_matches = false;
        let _ = searcher.search_path(
            &matcher,
            entry.path(),
            UTF8(|line_number, line| {
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let preview = line.trim_end_matches(['\r', '\n']);
                let mut ranges = Vec::new();
                let mut exceeded_limit = false;
                matcher
                    .find_iter(preview.as_bytes(), |found| {
                        if summary.matches + ranges.len() >= MAX_MATCHES {
                            exceeded_limit = true;
                            return false;
                        }
                        ranges.push((found.start(), found.end()));
                        true
                    })
                    .map_err(std::io::Error::other)?;
                if ranges.is_empty() {
                    if exceeded_limit {
                        summary.truncated = true;
                        stopped = true;
                        return Ok(false);
                    }
                    return Ok(true);
                }
                if summary.matches >= MAX_MATCHES
                    || (!file_has_matches && summary.files >= MAX_FILES)
                {
                    summary.truncated = true;
                    stopped = true;
                    return Ok(false);
                }
                if !file_has_matches {
                    summary.files += 1;
                    file_has_matches = true;
                }
                summary.matches += ranges.len();
                let mut start = ranges[0].0.saturating_sub(512);
                while !preview.is_char_boundary(start) {
                    start -= 1;
                }
                let mut end = (start + 4096).min(preview.len());
                while !preview.is_char_boundary(end) {
                    end -= 1;
                }
                let display_ranges = ranges
                    .iter()
                    .filter_map(|&(a, b)| {
                        (a >= start && b <= end).then_some((a - start, b - start))
                    })
                    .collect();
                if !emit(ContentSearchMatch {
                    path: relative.to_path_buf(),
                    line: line_number,
                    preview: preview[start..end].to_owned(),
                    ranges: display_ranges,
                }) {
                    stopped = true;
                    return Ok(false);
                }
                if exceeded_limit {
                    summary.truncated = true;
                    stopped = true;
                    return Ok(false);
                }
                Ok(true)
            }),
        );
    }
    summary.cancelled = cancelled.load(Ordering::Relaxed);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn collect(
        root: &Path,
        query: &ContentSearchQuery,
    ) -> (Vec<ContentSearchMatch>, ContentSearchSummary) {
        let mut matches = Vec::new();
        let summary = search_workspace_content(root, query, &AtomicBool::new(false), |item| {
            matches.push(item);
            true
        })
        .unwrap();
        (matches, summary)
    }

    #[test]
    fn content_search_options_and_globs() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.rs"), "Alpha alphabet\nalpha\n").unwrap();
        fs::write(root.path().join("b.txt"), "alpha\n").unwrap();
        let mut query = ContentSearchQuery {
            text: "alpha".into(),
            ..Default::default()
        };
        assert_eq!(collect(root.path(), &query).0.len(), 3);
        query.case_sensitive = true;
        query.whole_word = true;
        query.include = vec!["*.rs".into()];
        let (matches, _) = collect(root.path(), &query);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].ranges, vec![(0, 5)]);
        query.regex = true;
        query.text = "a.ph.".into();
        query.exclude = vec!["a.rs".into()];
        assert!(collect(root.path(), &query).0.is_empty());
    }

    #[test]
    fn content_search_caps_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("many"), "match\n".repeat(MAX_MATCHES + 1)).unwrap();
        let query = ContentSearchQuery {
            text: "match".into(),
            ..Default::default()
        };
        let (matches, summary) = collect(root.path(), &query);
        assert_eq!(matches.len(), MAX_MATCHES);
        assert!(summary.truncated);
        let cancelled = AtomicBool::new(true);
        let summary = search_workspace_content(root.path(), &query, &cancelled, |_| {
            panic!("cancelled search emitted a result")
        })
        .unwrap();
        assert!(summary.cancelled);
    }

    #[test]
    fn content_search_file_limit_and_changed_files() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..=MAX_FILES {
            fs::write(root.path().join(format!("{index}.txt")), "match\n").unwrap();
        }
        let mut query = ContentSearchQuery {
            text: "match".into(),
            ..Default::default()
        };
        let (_, summary) = collect(root.path(), &query);
        assert_eq!(summary.files, MAX_FILES);
        assert!(summary.truncated);
        query.changed_files = Some([PathBuf::from("12.txt")].into_iter().collect());
        let (matches, summary) = collect(root.path(), &query);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, Path::new("12.txt"));
        assert!(!summary.truncated);
    }

    #[test]
    fn content_search_counts_multiple_matches_and_cancels_during_emission() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("many"),
            format!("{}\n", "hit ".repeat(MAX_MATCHES + 1)),
        )
        .unwrap();
        let query = ContentSearchQuery {
            text: "hit".into(),
            ..Default::default()
        };
        let (matches, summary) = collect(root.path(), &query);
        assert_eq!(summary.matches, MAX_MATCHES);
        assert_eq!(matches.len(), 1);
        assert!(summary.truncated);
        let cancelled = AtomicBool::new(false);
        let summary = search_workspace_content(root.path(), &query, &cancelled, |_| {
            cancelled.store(true, Ordering::Relaxed);
            true
        })
        .unwrap();
        assert!(summary.cancelled);
    }

    #[test]
    fn content_search_rejects_invalid_regex_and_respects_ignore() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".ignore"), "ignored.txt\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "needle\n").unwrap();
        fs::write(root.path().join(".visible"), "needle\n").unwrap();
        let mut query = ContentSearchQuery { text: "needle".into(), ..Default::default() };
        let (matches, _) = collect(root.path(), &query);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, Path::new(".visible"));
        query.regex = true;
        query.text = "(".into();
        assert!(search_workspace_content(root.path(), &query, &AtomicBool::new(false), |_| true).is_err());
    }
}
