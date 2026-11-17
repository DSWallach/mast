//! Presentation of a bounded, pre-highlighted code excerpt.
use gpui::{div, prelude::*, px, StyledText};
use mast_syntax::HighlightedDocument;

use crate::theme::Theme;

pub(crate) const MAX_BYTES: usize = 32 * 1024;
pub(crate) const MAX_LINES: usize = 200;

/// Bound input before scheduling syntax work, preserving UTF-8 boundaries.
pub(crate) fn bounded_source(source: &str) -> String {
    let mut end = source.len().min(MAX_BYTES);
    while !source.is_char_boundary(end) { end -= 1; }
    let source = &source[..end];
    match source.match_indices('\n').nth(MAX_LINES - 1) {
        Some((end, _)) => source[..end].to_owned(),
        None => source.to_owned(),
    }
}

/// Render neutral syntax spans with the current theme. Highlighting is prepared
/// by the owner off the render path and may be absent while it is loading.
pub(crate) fn code_block(
    source: &str,
    first_line: u32,
    document: Option<&HighlightedDocument>,
    theme: &Theme,
    text_size: gpui::AbsoluteLength,
) -> gpui::Div {
    let mono = gpui::font(theme.font_mono.clone());
    let lines = source.split('\n').enumerate().map(|(index, line)| {
        let spans = document.and_then(|doc| doc.lines.get(index)).map(Vec::as_slice).unwrap_or(&[]);
        let runs = crate::markdown::render::runs_for_syntax_line(line, spans, &mono, theme);
        div().flex().gap(px(Theme::SPACE_MD))
            .child(div().w(px(Theme::SPACE_LG * 3.0)).flex_none().text_right()
                .text_color(theme.text_faint).child(first_line.saturating_add(index as u32).to_string()))
            .child(div().flex_1().min_w_0().child(StyledText::new(line.to_owned()).with_runs(runs)))
    });
    div().font_family(theme.font_mono.clone()).text_size(text_size).children(lines)
}

/// Render inline backtick spans without changing the surrounding prose.
pub(crate) fn inline_code(source: &str, theme: &Theme, color: gpui::Hsla) -> StyledText {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut remaining = source;
    while let Some(open) = remaining.find('`') {
        let rest = &remaining[open..];
        let count = rest.bytes().take_while(|byte| *byte == b'`').count();
        let delimiter = &rest[..count];
        let Some(close) = rest[count..].find(delimiter) else { break };
        let plain = &remaining[..open];
        text.push_str(plain);
        if !plain.is_empty() { runs.push(inline_run(plain.len(), false, theme, color)); }
        let code = &rest[count..count + close];
        text.push_str(code);
        if !code.is_empty() { runs.push(inline_run(code.len(), true, theme, color)); }
        remaining = &rest[count + close + count..];
    }
    text.push_str(remaining);
    if !remaining.is_empty() { runs.push(inline_run(remaining.len(), false, theme, color)); }
    StyledText::new(text).with_runs(runs)
}

fn inline_run(len: usize, code: bool, theme: &Theme, color: gpui::Hsla) -> gpui::TextRun {
    gpui::TextRun {
        len,
        font: gpui::font(if code { theme.font_mono.clone() } else { theme.font_sans.clone() }),
        color: if code { theme.code_text } else { color },
        background_color: code.then_some(theme.code_wash),
        underline: None,
        strikethrough: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_limits_preserve_utf8_and_bound_lines() {
        let source = "é".repeat(MAX_BYTES);
        let bounded = bounded_source(&source);
        assert!(bounded.len() <= MAX_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
        assert_eq!(bounded_source(&"x\n".repeat(MAX_LINES + 10)).lines().count(), MAX_LINES);
    }
}
