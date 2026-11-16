//! Bounded intra-line changes, indexed by the original hunk line.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use super::{DiffLine, LineKind};

pub(super) const MAX_HUNK_LINES: usize = 400;
const MAX_LINE_BYTES: usize = 2_000;
const MAX_CACHED_HUNKS: usize = 128;
const MAX_PENDING_HUNKS: usize = 8;

pub(super) type Key = (String, u32, u32);
pub(super) type HunkSpans = Arc<Vec<Vec<Range<usize>>>>;

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<Key, (HunkSpans, u64)>,
    clock: u64,
    pending: HashSet<Key>,
}

impl Cache {
    pub(super) fn get(&mut self, key: &Key) -> Option<HunkSpans> {
        let (spans, used) = self.entries.get_mut(key)?;
        self.clock = self.clock.wrapping_add(1);
        *used = self.clock;
        Some(spans.clone())
    }

    pub(super) fn begin(&mut self, key: &Key) -> bool {
        if self.pending.len() >= MAX_PENDING_HUNKS || self.pending.contains(key) {
            return false;
        }
        self.pending.insert(key.clone())
    }

    pub(super) fn finish(&mut self, key: Key, spans: HunkSpans) {
        self.pending.remove(&key);
        if self.entries.len() >= MAX_CACHED_HUNKS
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| used)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.clock = self.clock.wrapping_add(1);
        self.entries.insert(key, (spans, self.clock));
    }
}

/// Overlay the change wash without discarding syntax foreground styles.
pub(super) fn tint_runs(
    runs: Vec<gpui::TextRun>,
    spans: &[Range<usize>],
    color: gpui::Hsla,
) -> Vec<gpui::TextRun> {
    if spans.is_empty() {
        return runs;
    }
    let mut result = Vec::new();
    let mut offset = 0;
    let mut span_ix = 0;
    for run in runs {
        let end = offset + run.len;
        let mut at = offset;
        while at < end {
            while spans.get(span_ix).is_some_and(|span| span.end <= at) {
                span_ix += 1;
            }
            let span = spans.get(span_ix);
            let active = span.is_some_and(|span| span.start <= at);
            let next = match span {
                Some(span) if active => span.end.min(end),
                Some(span) => span.start.min(end),
                None => end,
            };
            let mut part = run.clone();
            part.len = next - at;
            if active {
                part.background_color = Some(color);
            }
            result.push(part);
            at = next;
        }
        offset = end;
    }
    result
}

fn push_span(spans: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = spans.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
    } else {
        spans.push(range);
    }
}

pub(super) fn spans(lines: &[DiffLine]) -> Vec<Vec<Range<usize>>> {
    let mut result = vec![Vec::new(); lines.len()];
    if lines.len() > MAX_HUNK_LINES
        || lines.iter().map(|line| line.text.len()).sum::<usize>() > 32_000
    {
        return result;
    }
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != LineKind::Del {
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && lines[index].kind == LineKind::Del {
            index += 1;
        }
        let added = index;
        while index < lines.len() && lines[index].kind == LineKind::Add {
            index += 1;
        }
        if added - start != index - added {
            continue;
        }
        for offset in 0..added - start {
            let old = &lines[start + offset].text;
            let new = &lines[added + offset].text;
            if old.len() > MAX_LINE_BYTES || new.len() > MAX_LINE_BYTES {
                continue;
            }
            let diff = similar::TextDiff::from_chars(old, new);
            let (mut old_offset, mut new_offset) = (0, 0);
            for change in diff.iter_all_changes() {
                let len = change.value().len();
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        old_offset += len;
                        new_offset += len;
                    }
                    similar::ChangeTag::Delete => {
                        push_span(&mut result[start + offset], old_offset..old_offset + len);
                        old_offset += len;
                    }
                    similar::ChangeTag::Insert => {
                        push_span(&mut result[added + offset], new_offset..new_offset + len);
                        new_offset += len;
                    }
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_no: None,
            new_no: None,
            text: text.into(),
        }
    }

    #[test]
    fn pairs_equal_sized_blocks_and_uses_utf8_byte_ranges() {
        let lines = [
            line(LineKind::Del, "let café = cat;"),
            line(LineKind::Add, "let café = dog;"),
        ];
        let spans = spans(&lines);
        assert_eq!(&lines[0].text[spans[0][0].clone()], "cat");
        assert_eq!(&lines[1].text[spans[1][0].clone()], "dog");
    }

    #[test]
    fn leaves_unbalanced_blocks_and_long_lines_unmarked() {
        let lines = [
            line(LineKind::Del, "a"),
            line(LineKind::Del, "b"),
            line(LineKind::Add, "c"),
        ];
        assert!(spans(&lines).iter().all(Vec::is_empty));
        let long = "a".repeat(MAX_LINE_BYTES + 1);
        assert!(
            spans(&[line(LineKind::Del, &long), line(LineKind::Add, "b")])
                .iter()
                .all(Vec::is_empty)
        );
    }

    #[test]
    fn frame_budget_reuses_each_hunk_and_caps_pending_and_cache() {
        let mut cache = Cache::default();
        let lines = (0..100)
            .map(|index| {
                line(
                    if index < 50 {
                        LineKind::Del
                    } else {
                        LineKind::Add
                    },
                    "some moderately long changed line",
                )
            })
            .collect::<Vec<_>>();
        let mut computations = 0;
        for _ in 0..10 {
            for hunk in 0..32 {
                let key = ("revision".to_string(), 0, hunk);
                if cache.get(&key).is_none() && cache.begin(&key) {
                    computations += 1;
                    cache.finish(key, Arc::new(spans(&lines)));
                }
            }
        }
        assert_eq!(computations, 32);
        for hunk in 32..40 {
            assert!(cache.begin(&("revision".into(), 0, hunk)));
        }
        assert!(!cache.begin(&("revision".into(), 0, 40)));
        for hunk in 100..260 {
            let key = ("other".into(), 0, hunk);
            cache.finish(key, Arc::new(Vec::new()));
        }
        assert_eq!(cache.entries.len(), MAX_CACHED_HUNKS);
    }

    #[test]
    fn whitespace_and_balanced_multiple_lines_have_spans() {
        let lines = [
            line(LineKind::Del, "a b"),
            line(LineKind::Del, "cat"),
            line(LineKind::Add, "a  b"),
            line(LineKind::Add, "dog"),
        ];
        let result = spans(&lines);
        assert!(
            result[2]
                .iter()
                .any(|range| &lines[2].text[range.clone()] == " ")
        );
        assert_eq!(&lines[1].text[result[1][0].clone()], "cat");
        assert_eq!(&lines[3].text[result[3][0].clone()], "dog");
    }

    #[test]
    fn changed_range_gets_background_without_losing_syntax_color() {
        let fg = gpui::hsla(0.1, 0.2, 0.3, 1.0);
        let bg = gpui::hsla(0.3, 0.4, 0.5, 0.25);
        let run = gpui::TextRun {
            len: 7,
            font: gpui::font("monospace"),
            color: fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let painted = tint_runs(vec![run], &[2..5], bg);
        assert_eq!(
            painted.iter().map(|run| run.len).collect::<Vec<_>>(),
            [2, 3, 2]
        );
        assert_eq!(painted[1].background_color, Some(bg));
        assert!(painted.iter().all(|run| run.color == fg));
        assert!(painted[0].background_color.is_none() && painted[2].background_color.is_none());
    }
}
