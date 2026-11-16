//! Bounded intra-line changes, indexed by the original hunk line.

use std::ops::Range;

use super::{DiffLine, LineKind};

const MAX_HUNK_LINES: usize = 400;
const MAX_LINE_BYTES: usize = 2_000;

pub(super) fn spans(lines: &[DiffLine]) -> Vec<Vec<Range<usize>>> {
    let mut result = vec![Vec::new(); lines.len()];
    if lines.len() > MAX_HUNK_LINES {
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
            let diff = similar::TextDiff::from_words(old, new);
            let (mut old_offset, mut new_offset) = (0, 0);
            for change in diff.iter_all_changes() {
                let len = change.value().len();
                match change.tag() {
                    similar::ChangeTag::Equal => { old_offset += len; new_offset += len; }
                    similar::ChangeTag::Delete => {
                        result[start + offset].push(old_offset..old_offset + len);
                        old_offset += len;
                    }
                    similar::ChangeTag::Insert => {
                        result[added + offset].push(new_offset..new_offset + len);
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
        DiffLine { kind, old_no: None, new_no: None, text: text.into() }
    }

    #[test]
    fn pairs_equal_sized_blocks_and_uses_utf8_byte_ranges() {
        let lines = [line(LineKind::Del, "let café = one;"), line(LineKind::Add, "let café = two;")];
        let spans = spans(&lines);
        assert_eq!(&lines[0].text[spans[0][0].clone()], "one");
        assert_eq!(&lines[1].text[spans[1][0].clone()], "two");
    }

    #[test]
    fn leaves_unbalanced_blocks_and_long_lines_unmarked() {
        let lines = [line(LineKind::Del, "a"), line(LineKind::Del, "b"), line(LineKind::Add, "c")];
        assert!(spans(&lines).iter().all(Vec::is_empty));
        let long = "a".repeat(MAX_LINE_BYTES + 1);
        assert!(spans(&[line(LineKind::Del, &long), line(LineKind::Add, "b")]).iter().all(Vec::is_empty));
    }
}
