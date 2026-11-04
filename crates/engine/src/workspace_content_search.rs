//! In-process, bounded content search for a resolved workspace checkout.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, atomic::AtomicUsize};

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
    pub skipped_files: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Search an already-filtered checkout index with parallel workers.
pub fn search_indexed_content(
    root: &Path,
    paths: &[String],
    query: &ContentSearchQuery,
    cancelled: &AtomicBool,
    emit: impl FnMut(ContentSearchMatch) -> bool + Send,
    progress: impl FnMut(usize) + Send,
) -> Result<ContentSearchSummary, String> {
    if query.text.is_empty() || paths.is_empty() {
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
    let state = Mutex::new((ContentSearchSummary::default(), emit));
    let progress = Mutex::new((0usize, progress));
    let examined = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let first_match = Mutex::new(None);
    // Names often identify the symbol being searched. Check those candidates
    // first, while still searching the complete index for every query.
    let mut ordered = Vec::with_capacity(paths.len());
    let mut remaining = Vec::new();
    if !query.regex && query.text.is_ascii() && query.text.len() >= 4 {
        let needle = query.text.to_ascii_lowercase();
        for path in paths {
            if cancelled.load(Ordering::Relaxed) {
                return Ok(ContentSearchSummary {
                    cancelled: true,
                    ..Default::default()
                });
            }
            let mut wanted = needle.bytes();
            let mut next = wanted.next();
            for byte in path.bytes() {
                if Some(byte.to_ascii_lowercase()) == next {
                    next = wanted.next();
                }
                if next.is_none() {
                    break;
                }
            }
            if next.is_none() {
                ordered.push(path);
            } else {
                remaining.push(path);
            }
        }
        ordered.extend(remaining);
    } else {
        ordered.extend(paths.iter());
    }
    // WalkParallel stats every root before starting workers. Partition the
    // index so one long root-list setup cannot delay the first result.
    let workers = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(8);
    std::thread::scope(|scope| {
        for partition in ordered.chunks(ordered.len().div_ceil(workers)) {
            let (state, progress, examined, stopped, first_match, matcher, overrides) = (
                &state,
                &progress,
                &examined,
                &stopped,
                &first_match,
                &matcher,
                &overrides,
            );
            scope.spawn(move || {
                let (first, rest) = partition.split_at(partition.len().min(64));
                for chunk in std::iter::once(first).chain(rest.chunks(512)) {
                    if cancelled.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    // WalkParallel uses a LIFO work stack; add each batch in
                    // reverse so priority candidates are searched first.
                    let mut paths = chunk.iter().rev();
                    let mut builder = WalkBuilder::new(root.join(paths.next().unwrap()));
                    for path in paths {
                        builder.add(root.join(path));
                    }
                    builder
                        .standard_filters(false)
                        .follow_links(false)
                        .hidden(false)
                        .threads(1);
                    builder.build_parallel().run(|| {
                        let matcher = matcher.clone();
                        let mut searcher = SearcherBuilder::new()
                            .line_number(true)
                            .heap_limit(Some(1024 * 1024))
                            .binary_detection(grep_searcher::BinaryDetection::quit(b'\0'))
                            .build();
                        Box::new(move |entry| {
                            if cancelled.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed)
                            {
                                return ignore::WalkState::Quit;
                            }
                            let Ok(entry) = entry else {
                                return ignore::WalkState::Continue;
                            };
                            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                                return ignore::WalkState::Continue;
                            }
                            if overrides.matched(entry.path(), false).is_ignore() {
                                return ignore::WalkState::Continue;
                            }
                            let Ok(relative) = entry.path().strip_prefix(root) else {
                                return ignore::WalkState::Continue;
                            };
                            if query
                                .changed_files
                                .as_ref()
                                .is_some_and(|files| !files.contains(relative))
                            {
                                return ignore::WalkState::Continue;
                            }
                            let count = examined.fetch_add(1, Ordering::Relaxed) + 1;
                            if count == 1 || count % 64 == 0 {
                                let mut progress = progress.lock().unwrap();
                                if count > progress.0 {
                                    progress.0 = count;
                                    (progress.1)(count);
                                }
                            }
                            if entry
                                .metadata()
                                .map_or(true, |metadata| metadata.len() > 8 * 1024 * 1024)
                            {
                                return ignore::WalkState::Continue;
                            }
                            let mut file_has_matches = false;
                            let result = searcher.search_path(
                                &matcher,
                                entry.path(),
                                UTF8(|line_number, line| {
                                    if cancelled.load(Ordering::Relaxed)
                                        || stopped.load(Ordering::Relaxed)
                                    {
                                        return Ok(false);
                                    }
                                    let preview = line.trim_end_matches(['\r', '\n']);
                                    let mut ranges = Vec::new();
                                    matcher
                                        .find_iter(preview.as_bytes(), |found| {
                                            ranges.push((found.start(), found.end()));
                                            ranges.len() <= MAX_MATCHES
                                        })
                                        .map_err(std::io::Error::other)?;
                                    if ranges.is_empty() {
                                        return Ok(true);
                                    }
                                    let mut state = state.lock().unwrap();
                                    if cancelled.load(Ordering::Relaxed)
                                        || stopped.load(Ordering::Relaxed)
                                    {
                                        return Ok(false);
                                    }
                                    let (summary, emit) = &mut *state;
                                    if summary.matches >= MAX_MATCHES
                                        || (!file_has_matches && summary.files >= MAX_FILES)
                                    {
                                        summary.truncated = true;
                                        stopped.store(true, Ordering::Relaxed);
                                        return Ok(false);
                                    }
                                    if !file_has_matches {
                                        summary.files += 1;
                                        file_has_matches = true;
                                    }
                                    let remaining = MAX_MATCHES - summary.matches;
                                    if ranges.len() > remaining {
                                        ranges.truncate(remaining);
                                        summary.truncated = true;
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
                                        .into_iter()
                                        .filter_map(|(a, b)| {
                                            (a >= start && b <= end)
                                                .then_some((a - start, b - start))
                                        })
                                        .collect();
                                    let mut first = first_match.lock().unwrap();
                                    if first.is_none() {
                                        *first = Some(started.elapsed());
                                    }
                                    drop(first);
                                    if !emit(ContentSearchMatch {
                                        path: relative.to_path_buf(),
                                        line: line_number,
                                        preview: preview[start..end].to_owned(),
                                        ranges: display_ranges,
                                    }) || summary.truncated
                                    {
                                        stopped.store(true, Ordering::Relaxed);
                                        return Ok(false);
                                    }
                                    Ok(true)
                                }),
                            );
                            if result.is_err() {
                                state.lock().unwrap().0.skipped_files += 1;
                            }
                            ignore::WalkState::Continue
                        })
                    });
                }
            });
        }
    });
    let mut summary = state.into_inner().unwrap().0;
    summary.cancelled = cancelled.load(Ordering::Relaxed);
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        first_match_ms = first_match
            .into_inner()
            .unwrap()
            .map(|duration: std::time::Duration| duration.as_millis()),
        files_searched = examined.load(Ordering::Relaxed),
        matches = summary.matches,
        "indexed content search completed"
    );
    Ok(summary)
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
    let started = std::time::Instant::now();
    let mut first_match = None;
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
        let result = searcher.search_path(
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
                if first_match.is_none() {
                    first_match = Some(started.elapsed());
                }
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
        if result.is_err() {
            summary.skipped_files += 1;
        }
    }
    summary.cancelled = cancelled.load(Ordering::Relaxed);
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        first_match_ms = first_match.map(|duration| duration.as_millis()),
        matches = summary.matches,
        files = summary.files,
        "workspace content search completed"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn indexed_search_respects_globs_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.rs"), "alpha\n").unwrap();
        fs::write(root.path().join("b.txt"), "alpha\n").unwrap();
        let paths = vec!["a.rs".into(), "b.txt".into()];
        let query = ContentSearchQuery {
            text: "alpha".into(),
            include: vec!["*.rs".into()],
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let mut results = Vec::new();
        let summary = search_indexed_content(
            root.path(),
            &paths,
            &query,
            &cancel,
            |item| {
                results.push(item);
                true
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.matches, 1);
        assert_eq!(results[0].path, Path::new("a.rs"));
        cancel.store(true, Ordering::Relaxed);
        let summary = search_indexed_content(
            root.path(),
            &paths,
            &query,
            &cancel,
            |_| panic!("cancelled search emitted a match"),
            |_| {},
        )
        .unwrap();
        assert!(summary.cancelled);
        cancel.store(false, Ordering::Relaxed);
        let query = ContentSearchQuery {
            text: "alpha".into(),
            ..Default::default()
        };
        let mut emitted = 0;
        let summary = search_indexed_content(
            root.path(),
            &paths,
            &query,
            &cancel,
            |_| {
                emitted += 1;
                cancel.store(true, Ordering::Relaxed);
                true
            },
            |_| {},
        )
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(emitted, 1);
    }

    #[test]
    fn indexed_search_reports_progress_and_skips_large_and_binary_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("text.txt"), "needle\n").unwrap();
        fs::write(root.path().join("binary.bin"), b"\0needle\n").unwrap();
        let large = fs::File::create(root.path().join("large.txt")).unwrap();
        large.set_len(8 * 1024 * 1024 + 1).unwrap();
        let paths = vec!["text.txt".into(), "binary.bin".into(), "large.txt".into()];
        let query = ContentSearchQuery {
            text: "needle".into(),
            ..Default::default()
        };
        let mut results = Vec::new();
        let mut progress = Vec::new();
        let summary = search_indexed_content(
            root.path(),
            &paths,
            &query,
            &AtomicBool::new(false),
            |item| {
                results.push(item);
                true
            },
            |count| progress.push(count),
        )
        .unwrap();
        assert_eq!(summary.matches, 1);
        assert_eq!(results[0].path, Path::new("text.txt"));
        assert!(!progress.is_empty());
    }

    #[test]
    fn indexed_search_enforces_match_limit() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("matches.txt"),
            "needle ".repeat(MAX_MATCHES + 1),
        )
        .unwrap();
        let paths = vec!["matches.txt".into()];
        let query = ContentSearchQuery {
            text: "needle".into(),
            ..Default::default()
        };
        let summary = search_indexed_content(
            root.path(),
            &paths,
            &query,
            &AtomicBool::new(false),
            |_| true,
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.matches, MAX_MATCHES);
        assert!(summary.truncated);
    }

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
        assert!(
            mast_harness::git_command()
                .arg("init")
                .arg("-q")
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "needle\n").unwrap();
        fs::write(root.path().join(".visible"), "needle\n").unwrap();
        let mut query = ContentSearchQuery {
            text: "needle".into(),
            ..Default::default()
        };
        let (matches, _) = collect(root.path(), &query);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, Path::new(".visible"));
        query.regex = true;
        query.text = "(".into();
        assert!(
            search_workspace_content(root.path(), &query, &AtomicBool::new(false), |_| true)
                .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn content_search_excludes_symlinks_and_git_metadata() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "needle").unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/config"), "needle").unwrap();
        symlink(outside.path().join("secret"), root.path().join("link")).unwrap();
        let query = ContentSearchQuery {
            text: "needle".into(),
            ..Default::default()
        };
        assert!(collect(root.path(), &query).0.is_empty());
    }

    #[test]
    fn content_search_reports_files_exceeding_buffer_limit() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("long"), "x".repeat(1024 * 1024 + 1)).unwrap();
        let query = ContentSearchQuery {
            text: "needle".into(),
            ..Default::default()
        };
        let (_, summary) = collect(root.path(), &query);
        assert_eq!(summary.skipped_files, 1);
    }
}
