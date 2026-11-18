use super::*;

/// Column budget for soft-wrapping thought text into detail lines. The
/// detail body is preformatted (no element wrapping), so the wrap happens
/// here — conservative enough to fit the card at typical transcript widths.
pub(super) const THOUGHT_WRAP_COLS: usize = 96;

/// Flatten a thought's parsed markdown into wrapped, STYLED detail lines —
/// inline markers render as real styling (bold/italic/code/links) instead of
/// literal `**` glyphs; blocks flatten structurally (headings bold, list
/// bullets, quote bars, verbatim code lines). Every line is one fixed-height
/// row, so the detail height stays analytic (lines × [`OUTPUT_LINE_HEIGHT`])
/// and the group's fold tween keeps working without measurement.
pub(super) fn thought_lines(tree: &BlockTree) -> Vec<Vec<InlineRun>> {
    let mut out: Vec<Vec<InlineRun>> = Vec::new();
    for top in &tree.blocks {
        if !out.is_empty() {
            // One blank separator row between top-level blocks (the old
            // plain-text wrap kept paragraph gaps the same way).
            out.push(Vec::new());
        }
        thought_block_lines(&top.block, 0, &mut out);
    }
    while out
        .last()
        .is_some_and(|l| l.iter().all(|r| r.text.trim().is_empty()))
    {
        out.pop();
    }
    out
}

/// The slot-0 indent run every emitted thought line opens with (possibly
/// empty). List/quote handlers rewrite it in place to plant markers/bars, so
/// it must exist even at zero indent.
pub(super) fn indent_run(indent: usize) -> Vec<InlineRun> {
    vec![InlineRun {
        text: " ".repeat(indent),
        style: InlineStyle::default(),
    }]
}

/// Append text to a line's run list, merging into the tail run when styles
/// match (keeps run counts small for the shaper).
pub(super) fn push_styled(line: &mut Vec<InlineRun>, text: &str, style: &InlineStyle) {
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some(last) if last.style == *style => last.text.push_str(text),
        _ => line.push(InlineRun {
            text: text.to_owned(),
            style: style.clone(),
        }),
    }
}

/// Close a wrapped line: the slot-0 indent run in front (see [`indent_run`]).
pub(super) fn finish_line(indent: usize, mut line: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut full = indent_run(indent);
    full.append(&mut line);
    full
}

/// Word-wrap styled runs at the thought column budget. Char-counted like
/// every detail wrap — block heights must stay analytic — with words glued
/// across style boundaries (`**bold**tail` wraps as one unit), separator
/// spaces riding the preceding run, and pathological overlong tokens
/// hard-split at the budget. Hard breaks (`\n` runs) split into
/// separately-wrapped segments.
pub(super) fn wrap_styled_runs(runs: &[InlineRun], indent: usize, out: &mut Vec<Vec<InlineRun>>) {
    let budget = THOUGHT_WRAP_COLS.saturating_sub(indent).max(16);
    let mut segments: Vec<Vec<InlineRun>> = vec![Vec::new()];
    for run in runs {
        for (ix, piece) in run.text.split('\n').enumerate() {
            if ix > 0 {
                segments.push(Vec::new());
            }
            if !piece.is_empty() {
                segments.last_mut().unwrap().push(InlineRun {
                    text: piece.to_owned(),
                    style: run.style.clone(),
                });
            }
        }
    }
    for segment in segments {
        // Tokens: maximal non-whitespace piece lists, glued across run
        // boundaries so a word split by styling never wraps mid-word.
        let mut tokens: Vec<Vec<InlineRun>> = Vec::new();
        let mut in_token = false;
        for run in &segment {
            let text = run.text.as_str();
            let mut pos = 0;
            while pos < text.len() {
                let rest = &text[pos..];
                let ws = rest.chars().next().is_some_and(char::is_whitespace);
                let end = rest
                    .char_indices()
                    .find(|(_, c)| c.is_whitespace() != ws)
                    .map_or(text.len(), |(i, _)| pos + i);
                if ws {
                    in_token = false;
                } else {
                    if !in_token {
                        tokens.push(Vec::new());
                        in_token = true;
                    }
                    push_styled(tokens.last_mut().unwrap(), &text[pos..end], &run.style);
                }
                pos = end;
            }
        }
        let mut line: Vec<InlineRun> = Vec::new();
        let mut len = 0usize;
        for token in tokens {
            let tok_len: usize = token.iter().map(|r| r.text.chars().count()).sum();
            if tok_len > budget {
                // Hard-split a pathological token at the budget.
                if len > 0 {
                    out.push(finish_line(indent, std::mem::take(&mut line)));
                    len = 0;
                }
                for piece in token {
                    let mut chars = piece.text.chars();
                    loop {
                        let chunk: String = chars.by_ref().take(budget - len).collect();
                        if chunk.is_empty() {
                            break;
                        }
                        len += chunk.chars().count();
                        push_styled(&mut line, &chunk, &piece.style);
                        if len == budget {
                            out.push(finish_line(indent, std::mem::take(&mut line)));
                            len = 0;
                        }
                    }
                }
                continue;
            }
            if len > 0 && len + 1 + tok_len > budget {
                out.push(finish_line(indent, std::mem::take(&mut line)));
                len = 0;
            }
            if len > 0 {
                if let Some(last) = line.last_mut() {
                    last.text.push(' ');
                }
                len += 1;
            }
            for piece in token {
                push_styled(&mut line, &piece.text, &piece.style);
            }
            len += tok_len;
        }
        if len > 0 {
            out.push(finish_line(indent, line));
        }
    }
}

pub(super) fn runs_text(runs: &[InlineRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// Plain text of a block, for the accessibility tree (screen readers and the
/// headless UI driver read message text from it).
pub(super) fn block_plain_text(block: &Block) -> String {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => runs_text(runs),
        Block::CodeBlock { code, .. } => code.clone(),
        Block::BlockQuote { children } => children
            .iter()
            .map(block_plain_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Block::List { items, .. } => items
            .iter()
            .map(|item| {
                item.iter()
                    .map(block_plain_text)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Table { header, rows, .. } => std::iter::once(header)
            .chain(rows.iter())
            .map(|row| row.iter().map(|cell| runs_text(cell)).collect::<Vec<_>>().join(" | "))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Attach a row's text to the a11y tree. The text is built only while a11y is
/// active (a screen reader or the headless driver); otherwise nothing is
/// computed on the render path.
pub(super) fn with_accessible_text(
    el: gpui::Stateful<gpui::Div>,
    kind: &RowKind,
    a11y_active: bool,
) -> gpui::Stateful<gpui::Div> {
    if !a11y_active {
        return el;
    }
    match row_accessible_text(kind) {
        Some(text) => el.role(gpui::Role::Paragraph).aria_label(text),
        None => el,
    }
}

/// Text a transcript row exposes to the accessibility tree; `None` for rows
/// with no readable text of their own (tool groups, chips, images).
pub(super) fn row_accessible_text(kind: &RowKind) -> Option<SharedString> {
    #[cfg(test)]
    ACCESSIBLE_TEXT_BUILDS.with(|count| count.set(count.get() + 1));
    let text = match kind {
        RowKind::User { text, .. } => text.to_string(),
        RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
            block_plain_text(&tree.blocks.get(*block_ix)?.block)
        }
        _ => return None,
    };
    (!text.is_empty()).then(|| SharedString::from(text))
}

/// One markdown block into thought detail lines, `indent` spaces deep.
pub(super) fn thought_block_lines(block: &Block, indent: usize, out: &mut Vec<Vec<InlineRun>>) {
    match block {
        Block::Paragraph { runs } => wrap_styled_runs(runs, indent, out),
        Block::Heading { runs, .. } => {
            // Headings keep the detail's single type size — bold is the cue
            // (an 18px line box can't host display sizes).
            let bold: Vec<InlineRun> = runs
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    r.style.bold = true;
                    r
                })
                .collect();
            wrap_styled_runs(&bold, indent, out);
        }
        Block::CodeBlock { code, .. } => {
            let style = InlineStyle {
                code: true,
                ..InlineStyle::default()
            };
            for line in code.lines() {
                for chunk in wrap_cols(line, THOUGHT_WRAP_COLS.saturating_sub(indent).max(16)) {
                    let mut row = indent_run(indent);
                    if !chunk.is_empty() {
                        row.push(InlineRun {
                            text: chunk.to_string(),
                            style: style.clone(),
                        });
                    }
                    out.push(row);
                }
            }
        }
        Block::List {
            ordered_start,
            items,
        } => {
            // Tight rendering: no blank rows inside a list.
            for (ix, item) in items.iter().enumerate() {
                let marker = match ordered_start {
                    Some(start) => format!("{}. ", start + ix as u64),
                    None => "• ".to_string(),
                };
                let inner = indent + marker.chars().count();
                let mark = out.len();
                for child in item {
                    thought_block_lines(child, inner, out);
                }
                if out.len() == mark {
                    // An empty item still shows its marker.
                    out.push(indent_run(inner));
                }
                // The item's first line trades its indent spaces for the
                // marker (the slot-0 run is always the indent).
                if let Some(first) = out[mark].first_mut() {
                    first.text = format!("{}{marker}", " ".repeat(indent));
                }
            }
        }
        Block::Question(card) => {
            // A thought shows the question as one line: "Q-3 question text".
            let mut line = Vec::new();
            if let Some(id) = &card.id {
                push_styled(&mut line, &format!("{id} "), &InlineStyle::default());
            }
            line.extend(card.text.iter().cloned());
            wrap_styled_runs(&line, indent, out);
        }
        Block::BlockQuote { children } => {
            let mark = out.len();
            for (ix, child) in children.iter().enumerate() {
                if ix > 0 {
                    out.push(Vec::new());
                }
                thought_block_lines(child, indent + 2, out);
            }
            // Trade the two quote-indent spaces for the bar on every quoted
            // line — replace, not overwrite: nested list handlers already
            // planted markers after their own deeper indent.
            for line in &mut out[mark..] {
                if let Some(first) = line.first_mut()
                    && first.text.len() >= indent + 2
                {
                    first.text.replace_range(indent..indent + 2, "│ ");
                }
            }
        }
        Block::Table { header, rows, .. } => {
            // A thought is a record, not a layout surface: cells joined with
            // a dot separator, header bold — no column machinery.
            let join = |cells: &[Vec<InlineRun>], bold: bool| -> Vec<InlineRun> {
                let mut line: Vec<InlineRun> = Vec::new();
                for (ix, cell) in cells.iter().enumerate() {
                    if ix > 0 {
                        push_styled(&mut line, " · ", &InlineStyle::default());
                    }
                    for r in cell {
                        let mut r = r.clone();
                        r.style.bold |= bold;
                        line.push(r);
                    }
                }
                line
            };
            wrap_styled_runs(&join(header, true), indent, out);
            for row in rows {
                wrap_styled_runs(&join(row, false), indent, out);
            }
        }
        Block::Rule => {
            let mut row = indent_run(indent);
            row.push(InlineRun {
                text: "———".into(),
                style: InlineStyle::default(),
            });
            out.push(row);
        }
    }
}

/// A reasoning part as a tool-group chip: "Thought process" header over the
/// thought's markdown flattened into styled detail lines (analytic height —
/// the group's fold tween needs it; see [`thought_lines`]). Capped like tool
/// outputs, with the counted tail. `live` = the part is still streaming
/// (chip defaults open).
pub(super) fn thought_item(part_id: &str, tree: &BlockTree, live: bool) -> ToolItem {
    let mut lines = thought_lines(tree);
    let truncated_by = lines.len().saturating_sub(OUTPUT_DETAIL_MAX_LINES);
    if truncated_by > 0 {
        // Keep the TAIL while streaming (the fresh thinking is the signal);
        // settled thoughts keep the head like tool outputs do.
        if live {
            lines.drain(..truncated_by);
            // The cut can land on a block separator — drop the orphan blank.
            while lines
                .first()
                .is_some_and(|l| l.iter().all(|r| r.text.trim().is_empty()))
            {
                lines.remove(0);
            }
        } else {
            lines.truncate(OUTPUT_DETAIL_MAX_LINES);
        }
    }
    ToolItem {
        part_id: part_id.into(),
        call: ToolCall::Unknown {
            name: "Thought process".into(),
            input: None,
        },
        is_error: false,
        resolved: !live,
        detail: (!lines.is_empty()).then(|| {
            Arc::new(ToolDetail::Thought {
                lines,
                truncated_by,
            })
        }),
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        is_thought: true,
    }
}

