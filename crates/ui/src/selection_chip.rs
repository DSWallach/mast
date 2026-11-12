//! Lossless selected-text chips use readable fenced Markdown as their draft
//! representation, so navigation, undo, retries and queued edits retain them.
use std::ops::Range;

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = selection_chip, no_json)]
pub(crate) struct AddToSession(pub String);

pub(crate) struct SelectionBlock<'a> {
    pub range: Range<usize>,
    pub text: &'a str,
}

pub(crate) fn markdown(text: &str) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}selected-text\n{text}\n{fence}")
}

pub(crate) fn blocks(raw: &str) -> Vec<SelectionBlock<'_>> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let line_end = raw[offset..]
            .find('\n')
            .map_or(raw.len(), |end| offset + end);
        let line = &raw[offset..line_end];
        let ticks = line.bytes().take_while(|byte| *byte == b'`').count();
        if ticks >= 3 && &line[ticks..] == "selected-text" && line_end < raw.len() {
            let fence = &line[..ticks];
            let body = line_end + 1;
            let mut closing = body;
            while closing < raw.len() {
                let end = raw[closing..]
                    .find('\n')
                    .map_or(raw.len(), |end| closing + end);
                if &raw[closing..end] == fence && closing > body {
                    result.push(SelectionBlock {
                        range: offset..end,
                        text: &raw[body..closing - 1],
                    });
                    offset = end;
                    break;
                }
                closing = end.saturating_add(1);
            }
            if closing < raw.len() {
                continue;
            }
        }
        offset = line_end.saturating_add(1);
    }
    result
}

pub(crate) fn label(text: &str) -> String {
    let lines = text.split('\n').count();
    format!(
        "Selected text · {lines} {}",
        if lines == 1 { "line" } else { "lines" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_chip_round_trips_arbitrary_text_and_fences() {
        for text in [
            "hello",
            "  α\r\n🙂\n",
            "```\n```selected-text\ninside\n```\n````",
            "[a.rs](local-file://src/a.rs)",
            "",
        ] {
            let raw = format!("before\n{}\nafter", markdown(text));
            let parsed = blocks(&raw);
            assert_eq!(parsed.len(), 1, "{raw}");
            assert_eq!(parsed[0].text, text);
            assert_eq!(&raw[parsed[0].range.clone()], markdown(text));
        }
    }

    #[test]
    fn selection_chip_parser_keeps_multiple_blocks_and_rejects_unclosed_blocks() {
        let raw = format!("{}\n{}", markdown("first"), markdown("second\nline"));
        let parsed = blocks(&raw);
        assert_eq!(
            parsed.iter().map(|block| block.text).collect::<Vec<_>>(),
            ["first", "second\nline"]
        );
        assert!(blocks("```selected-text\nunclosed").is_empty());
        assert!(blocks("prefix ```selected-text\nbody\n```").is_empty());
    }
}
