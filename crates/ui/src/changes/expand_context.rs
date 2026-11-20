//! Construct unchanged gaps from a verified new-side source snapshot.

use super::{DiffLine, DiffRow, FileDiff, FileStatus, LineKind};
use std::collections::HashMap;

pub(super) const MAX_FILES: usize = 8;

pub(super) enum Entry {
    Loading,
    Ready {
        gaps: Vec<Gap>,
        shown: Vec<usize>,
        max_columns: usize,
        max_line: u32,
    },
    Failed,
}

impl Entry {
    pub fn ready(gaps: Vec<Gap>, shown: Vec<usize>) -> Self {
        let max_columns = gaps
            .iter()
            .flat_map(|gap| gap.columns.iter())
            .copied()
            .max()
            .unwrap_or(0);
        let max_line = gaps
            .last()
            .and_then(|gap| gap.lines.last())
            .map(|line| line.old_no.unwrap_or(0).max(line.new_no.unwrap_or(0)))
            .unwrap_or(0);
        Self::Ready {
            gaps,
            shown,
            max_columns,
            max_line,
        }
    }
}

#[derive(Default)]
pub(super) struct Cache {
    pub entries: HashMap<(String, u32), Entry>,
    pub order: Vec<(String, u32)>,
}

impl Cache {
    pub fn expand(&mut self, key: &(String, u32), gap: usize, all: bool) -> bool {
        self.order.retain(|item| item != key);
        self.order.push(key.clone());
        let Some(Entry::Ready { gaps, shown, .. }) = self.entries.get_mut(key) else {
            return false;
        };
        let (Some(data), Some(count)) = (gaps.get(gap), shown.get_mut(gap)) else {
            return false;
        };
        *count = if all {
            data.lines.len()
        } else {
            count.saturating_add(20).min(data.lines.len())
        };
        true
    }

    pub fn begin(&mut self, key: (String, u32)) -> bool {
        if self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loading))
            .count()
            >= 4
        {
            return false;
        }
        self.order.retain(|item| item != &key);
        if self.order.len() == MAX_FILES {
            let Some(oldest) = self
                .order
                .iter()
                .position(|item| !matches!(self.entries.get(item), Some(Entry::Loading)))
            else {
                return false;
            };
            self.entries.remove(&self.order.remove(oldest));
        }
        self.order.push(key.clone());
        self.entries.insert(key, Entry::Loading);
        true
    }
}

pub(super) fn add_rows(
    rows: Vec<DiffRow>,
    files: &[FileDiff],
    revision: &str,
    cache: &Cache,
) -> (Vec<DiffRow>, Vec<usize>) {
    let mut out = Vec::with_capacity(rows.len());
    let mut indices = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        indices.push(out.len());
        let target = match row {
            DiffRow::HunkHeader { file, hunk } => Some((file, hunk)),
            DiffRow::BodyPad { file } => files
                .get(file as usize)
                .map(|f| (file, f.hunks.len() as u32)),
            _ => None,
        };
        if let Some((file, gap)) = target
            && let Some(diff) = files.get(file as usize)
            && may_have_gap(diff, gap as usize)
        {
            let entry = cache.entries.get(&(revision.to_string(), file));
            match entry {
                Some(Entry::Ready { gaps, shown, .. }) => {
                    if let (Some(data), Some(&count)) =
                        (gaps.get(gap as usize), shown.get(gap as usize))
                    {
                        let count = count.min(data.lines.len());
                        let footer = gap as usize == diff.hunks.len();
                        let start = if footer { 0 } else { data.lines.len() - count };
                        if !footer && count < data.lines.len() {
                            out.push(DiffRow::ContextControl { file, gap });
                        }
                        for line in start..start + count {
                            out.push(DiffRow::ContextLine {
                                file,
                                gap,
                                line: line as u32,
                            });
                        }
                        if footer && count < data.lines.len() {
                            out.push(DiffRow::ContextControl { file, gap });
                        }
                    }
                }
                Some(Entry::Failed) => out.push(DiffRow::ContextControl { file, gap }),
                _ => out.push(DiffRow::ContextControl { file, gap }),
            }
        }
        out.push(row);
    }
    indices.push(out.len());
    (out, indices)
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Gap {
    pub lines: Vec<DiffLine>,
    pub columns: Vec<usize>,
}

fn range(token: &str, prefix: char) -> Option<(u32, u32)> {
    let token = token.strip_prefix(prefix)?;
    let (start, count) = token.split_once(',').unwrap_or((token, "1"));
    let start = start.parse::<u32>().ok()?;
    let count = count.parse::<u32>().ok()?;
    Some((
        if count == 0 {
            start.checked_add(1)?
        } else {
            start.max(1)
        },
        count,
    ))
}

fn starts(hunk: &super::Hunk) -> Option<((u32, u32), (u32, u32))> {
    let mut parts = hunk.header.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    Some((range(parts.next()?, '-')?, range(parts.next()?, '+')?))
}

pub(super) fn known_gap_size(file: &FileDiff, gap: usize) -> Option<usize> {
    let (_, (start, _)) = starts(file.hunks.get(gap)?)?;
    let previous = if gap == 0 { 1 } else {
        let (_, (start, count)) = starts(file.hunks.get(gap - 1)?)?;
        start.checked_add(count)?
    };
    Some(start.checked_sub(previous)? as usize)
}

pub(super) fn may_have_gap(file: &FileDiff, gap: usize) -> bool {
    if file.status != FileStatus::Modified
        || file.binary
        || file.old_path.is_some()
        || file.hunks.is_empty()
    {
        return false;
    }
    if gap == file.hunks.len() {
        return true;
    }
    let Some(((_, _), (new_start, _))) = file.hunks.get(gap).and_then(starts) else {
        return false;
    };
    let previous = if gap == 0 {
        Some(1)
    } else {
        file.hunks
            .get(gap - 1)
            .and_then(starts)
            .and_then(|(_, (start, count))| start.checked_add(count))
    };
    previous.is_some_and(|previous| new_start > previous)
}

pub(super) fn gaps(file: &FileDiff, text: &str) -> Option<Vec<Gap>> {
    if file.status != FileStatus::Modified || file.binary || file.old_path.is_some() {
        return None;
    }
    let source: Vec<&str> = text.lines().take(20_001).collect();
    if source.len() > 20_000 {
        return None;
    }
    let mut result = Vec::with_capacity(file.hunks.len() + 1);
    let (mut old_next, mut new_next) = (1u32, 1u32);
    for hunk in &file.hunks {
        let ((old_start, old_count), (new_start, new_count)) = starts(hunk)?;
        let old_gap = old_start.checked_sub(old_next)?;
        let new_gap = new_start.checked_sub(new_next)?;
        if old_gap != new_gap {
            return None;
        }
        result.push(make_gap(&source, old_next, new_next, new_gap)?);
        for line in &hunk.lines {
            if let Some(number) = line.new_no
                && source.get(number.checked_sub(1)? as usize).copied()? != line.text {
                    return None;
                }
        }
        old_next = old_start.checked_add(old_count)?;
        new_next = new_start.checked_add(new_count)?;
    }
    let end = u32::try_from(source.len()).ok()?.checked_add(1)?;
    let count = end.checked_sub(new_next)?;
    result.push(make_gap(&source, old_next, new_next, count)?);
    Some(result)
}

fn make_gap(source: &[&str], old: u32, new: u32, count: u32) -> Option<Gap> {
    let lines: Vec<DiffLine> = (0..count)
        .map(|offset| {
            Some(DiffLine {
                kind: LineKind::Context,
                old_no: Some(old.checked_add(offset)?),
                new_no: Some(new.checked_add(offset)?),
                text: source
                    .get(new.checked_add(offset)?.checked_sub(1)? as usize)?
                    .to_string(),
            })
        })
        .collect::<Option<_>>()?;
    let columns = lines
        .iter()
        .map(|line| super::visual_columns(&line.text))
        .collect();
    Some(Gap { lines, columns })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_preserve_both_line_numbers_across_insertions() {
        let file = super::super::parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -2,1 +2,2 @@\n two\n+added\n@@ -5,1 +6,1 @@\n five\n").remove(0);
        let result = gaps(&file, "one\ntwo\nadded\nthree\nfour\nfive\nsix\n").unwrap();
        assert_eq!(
            result.iter().map(|gap| gap.lines.len()).collect::<Vec<_>>(),
            [1, 2, 1]
        );
        assert_eq!(
            (result[1].lines[0].old_no, result[1].lines[0].new_no),
            (Some(3), Some(4))
        );
        assert_eq!(
            (result[2].lines[0].old_no, result[2].lines[0].new_no),
            (Some(6), Some(7))
        );
        assert!(gaps(&file, "changed\ntwo\nadded\nthree\nfour\nwrong\nsix\n").is_none());
    }

    #[test]
    fn known_gap_size_uses_the_previous_new_side_range() {
        let file = super::super::parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -3,1 +3,2 @@\n three\n+added\n@@ -8,1 +9,1 @@\n eight\n").remove(0);
        assert_eq!(known_gap_size(&file, 0), Some(2));
        assert_eq!(known_gap_size(&file, 1), Some(4));
        assert_eq!(known_gap_size(&file, 2), None);
    }

    #[test]
    fn expanding_rows_keeps_hunk_rows_and_uses_one_cached_read() {
        let file =
            super::super::parse_patch("diff --git a/a b/a\n@@ -3 +3 @@\n-old\n+new\n").remove(0);
        let original = file.clone();
        let source = "one\ntwo\nnew\nfour\n";
        let mut cache = Cache::default();
        let key = ("revision".to_string(), 0);
        let mut reads = 0;
        for _ in 0..10 {
            if !cache.entries.contains_key(&key) {
                assert!(cache.begin(key.clone()));
                reads += 1;
                let gaps = gaps(&file, source).unwrap();
                cache
                    .entries
                    .insert(key.clone(), Entry::ready(gaps.clone(), vec![0; gaps.len()]));
            }
            assert!(cache.expand(&key, 0, false));
        }
        assert_eq!(reads, 1);
        let rows = super::super::body_rows(0, &file, &[], None, super::super::DiffMode::Unified);
        let (expanded, _) = add_rows(
            rows.clone(),
            std::slice::from_ref(&file),
            "revision",
            &cache,
        );
        let hunk_rows = |items: &[DiffRow]| {
            items
                .iter()
                .copied()
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::HunkHeader { .. }
                            | DiffRow::Line { .. }
                            | DiffRow::SplitLine { .. }
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(hunk_rows(&expanded), hunk_rows(&rows));
        assert_eq!(
            expanded
                .iter()
                .filter(|row| matches!(row, DiffRow::ContextLine { .. }))
                .count(),
            2
        );
        assert_eq!(file, original);
    }

    #[test]
    fn git_patch_context_works_in_both_layouts() {
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let output = mast_harness::git_command()
                .current_dir(repo.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        let old = (1..=50).map(|n| format!("line {n}\n")).collect::<String>();
        std::fs::write(repo.path().join("a.txt"), &old).unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-qm", "fixture"]);
        let new = old.replace("line 25\n", "updated 25\nextra\n");
        std::fs::write(repo.path().join("a.txt"), &new).unwrap();
        let patch = run(&["diff", "--", "a.txt"]);
        let file = super::super::parse_patch(&patch).remove(0);
        let gaps = gaps(&file, &new).unwrap();
        assert_eq!(gaps.len(), 2);
        assert_eq!(
            (gaps[0].lines[0].old_no, gaps[0].lines[0].new_no),
            (Some(1), Some(1))
        );
        let last = gaps[1].lines.last().unwrap();
        assert_eq!((last.old_no, last.new_no), (Some(50), Some(51)));
        let mut cache = Cache::default();
        let key = ("git".to_string(), 0);
        cache.entries.insert(
            key.clone(),
            Entry::ready(
                gaps.clone(),
                gaps.iter().map(|gap| gap.lines.len()).collect(),
            ),
        );
        for mode in [
            super::super::DiffMode::Unified,
            super::super::DiffMode::Split,
        ] {
            let rows = super::super::body_rows(0, &file, &[], None, mode);
            let (expanded, _) = add_rows(rows, std::slice::from_ref(&file), "git", &cache);
            assert!(
                expanded
                    .iter()
                    .any(|row| matches!(row, DiffRow::ContextLine { .. }))
            );
        }
    }

    #[test]
    fn unsupported_files_never_offer_controls() {
        let mut file =
            super::super::parse_patch("diff --git a/a b/a\n@@ -3 +3 @@\n-old\n+new\n").remove(0);
        file.binary = true;
        assert!(!may_have_gap(&file, 0));
        file.binary = false;
        file.status = FileStatus::Deleted;
        assert!(!may_have_gap(&file, 0));
        file.status = FileStatus::Renamed;
        assert!(!may_have_gap(&file, 0));
    }

    #[test]
    fn source_cache_limits_concurrent_reads_and_retained_files() {
        let mut cache = Cache::default();
        for file in 0..4 {
            assert!(cache.begin(("rev".into(), file)));
        }
        assert!(!cache.begin(("rev".into(), 4)));
        for file in 0..4 {
            cache
                .entries
                .insert(("rev".into(), file), Entry::ready(Vec::new(), Vec::new()));
        }
        for file in 4..20 {
            let key = ("rev".into(), file);
            assert!(cache.begin(key.clone()));
            cache
                .entries
                .insert(key, Entry::ready(Vec::new(), Vec::new()));
        }
        assert_eq!(cache.entries.len(), MAX_FILES);
    }
}
