use super::*;

/// Green for additions — sampled from the reference diff (soft emerald).
pub(super) fn add_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_add // emerald-400
}

/// Red for deletions — softer than the theme danger, per the reference diff.
pub(super) fn del_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_del // red-400
}

pub(super) fn word_tint(line: &DiffLine, theme: &Theme) -> gpui::Hsla {
    match line.kind { LineKind::Add => add_color(theme), LineKind::Del => del_color(theme), _ => theme.text }.opacity(0.25)
}

/// One notice row ("New file", "Binary file — contents not shown", …).
pub(super) fn notice_row(notice: String, theme: &Theme) -> AnyElement {
    div()
        .h(px(NOTICE_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .text_size(px(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(notice))
        .into_any_element()
}

/// One `@@ … @@` hunk-header row on the bluish-grey wash.
pub(super) fn hunk_header_row(header: &str, symbol: Option<&str>, theme: &Theme) -> AnyElement {
    div()
        .h(px(HUNK_HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .bg(theme.diff_hunk_bg)
        .when(true, crate::components::hunk_header)
        .text_color(theme.text_faint)
        .child(SharedString::from(crate::components::hunk_label(header, symbol)))
        .into_any_element()
}

/// The only part of a diff row allowed to exceed its viewport. The outer
/// element keeps row chrome fixed; the inner element owns the intrinsic code
/// width and is the only plane moved by the file's horizontal scroll handle.
pub(super) fn code_text_viewport(
    text: String,
    runs: Vec<gpui::TextRun>,
    theme: &Theme,
    padding_left: f32,
    content_width: Option<f32>,
    wrapped: bool,
    scroll: Option<DiffCodeScroll>,
) -> AnyElement {
    let content = div()
        .when(wrapped, |el| el.w_full().min_w_0())
        .when_some(content_width, |el, width| {
            // Keep every tracked row's scroll extent identical. The width
            // already includes shaping slack on the right, so clipping here
            // only prevents a child from redefining the shared maximum.
            el.w(px(width)).flex_none().overflow_hidden()
        })
        .pl(px(padding_left))
        .font_family(theme.font_mono.clone())
        .text_size(px(diff_text_size(theme)))
        .line_height(px(diff_line_height(theme)))
        .map(|el| {
            if wrapped {
                el.whitespace_normal()
            } else {
                el.whitespace_nowrap()
            }
        })
        .child(gpui::StyledText::new(text).with_runs(runs));
    let viewport = div()
        .flex_1()
        .min_w_0()
        .min_h(px(diff_line_height(theme)))
        .overflow_hidden()
        .child(content);
    if wrapped {
        return viewport.into_any_element();
    }
    match scroll {
        Some(scroll) => {
            let mut viewport = viewport
                .id(scroll.id)
                .overflow_x_scroll()
                .track_scroll(&scroll.handle);
            // Without this GPUI maps a vertical-only wheel delta onto x for
            // an x-only scroller, starving the virtualized list underneath.
            viewport.style().restrict_scroll_to_axis = Some(true);
            viewport.into_any_element()
        }
        None => viewport.into_any_element(),
    }
}

/// One +/−/context/meta diff line: coloured accent bar, dual line-number
/// gutters (`gutter_px` wide — see [`gutter_width`]), marker column, and
/// paint-only syntax runs.
pub(super) fn diff_line_row(
    line: &DiffLine,
    spans: &[mast_syntax::HighlightSpan],
    word_spans: &[std::ops::Range<usize>],
    theme: &Theme,
    gutter_px: f32,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScroll>,
) -> AnyElement {
    if line.kind == LineKind::Meta {
        return meta_line_row(
            &line.text,
            theme,
            ACCENT_BAR_WIDTH + 2.0 * gutter_px + MARKER_WIDTH + 12.0,
        );
    }

    // Row tints sampled from the reference: ~5–6% washes over the pane tone.
    let mut add_bg = add_color(theme);
    add_bg.a = 0.055;
    let mut del_bg = del_color(theme);
    del_bg.a = 0.055;

    let (marker, marker_color, row_bg, accent, number_color) = match line.kind {
        LineKind::Add => (
            "+",
            add_color(theme),
            Some(add_bg),
            Some(add_color(theme).opacity(0.55)),
            add_color(theme).opacity(0.9),
        ),
        LineKind::Del => (
            "−",
            del_color(theme),
            Some(del_bg),
            Some(del_color(theme).opacity(0.55)),
            del_color(theme).opacity(0.9),
        ),
        _ => (
            "·",
            theme.text_faint.opacity(0.5),
            None,
            None,
            theme.text_faint.opacity(0.8),
        ),
    };
    let gutter = |no: Option<u32>, color: gpui::Hsla| {
        div()
            .w(px(gutter_px))
            .flex_none()
            .font_family(theme.font_mono.clone())
            .text_size(px(11.0))
            .line_height(px(diff_line_height(theme)))
            .text_color(color)
            .flex()
            .justify_end()
            .pr(px(8.0))
            .child(SharedString::from(
                no.map(|n| n.to_string()).unwrap_or_default(),
            ))
    };
    let mono = font(theme.font_mono.clone());
    let runs = render::runs_for_syntax_line_with_plain(
        &line.text,
        spans,
        &mono,
        theme.text.opacity(0.92),
        theme,
    );
    let runs = word_diff::tint_runs(runs, word_spans, word_tint(line, theme));
    let content_width = match code_width {
        DiffCodeWidth::Clipped => None,
        DiffCodeWidth::Scrollable(metrics) => Some(metrics.unified_content_width(gutter_px)),
        DiffCodeWidth::Wrapped => None,
    };
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    div()
        .map(|el| {
            if wrapped {
                el.min_h(px(diff_line_height(theme)))
            } else {
                el.h(px(diff_line_height(theme)))
            }
        })
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_start()
        .when_some(row_bg, |el, bg| el.bg(bg))
        // Accent bar: solid colour on +/− rows, invisible spacer on
        // context rows so columns always align.
        .child(
            div()
                .w(px(ACCENT_BAR_WIDTH))
                .self_stretch()
                .flex_none()
                .when_some(accent, |el, color| el.bg(color)),
        )
        .child(gutter(
            line.old_no,
            if line.kind == LineKind::Del {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
        ))
        .child(gutter(
            line.new_no,
            if line.kind == LineKind::Add {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
        ))
        .child(
            div()
                .w(px(MARKER_WIDTH))
                .flex_none()
                .flex()
                .justify_center()
                .text_size(px(diff_text_size(theme)))
                .line_height(px(diff_line_height(theme)))
                .text_color(marker_color)
                .font_family(theme.font_mono.clone())
                .child(SharedString::from(marker)),
        )
        .child(code_text_viewport(
            line.text.clone(),
            runs,
            theme,
            UNIFIED_CODE_PADDING_LEFT,
            content_width,
            wrapped,
            scroll,
        ))
        .into_any_element()
}

/// `\ No newline at end of file` and friends: a note about the row rather
/// than code, so it is indented past the columns and never tinted. In split
/// mode it spans both halves.
pub(super) fn meta_line_row(text: &str, theme: &Theme, pad_left: f32) -> AnyElement {
    div()
        .h(px(diff_line_height(theme)))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .pl(px(pad_left))
        .text_size(px(10.5))
        .text_color(theme.text_faint)
        .italic()
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Split (side-by-side) rendering
// ---------------------------------------------------------------------------

/// The paint-only syntax runs for one diff line.
pub(super) fn line_runs(
    line: &DiffLine,
    highlights: Option<&DiffHighlights>,
    theme: &Theme,
) -> Vec<gpui::TextRun> {
    let spans = highlights.map(|h| h.spans(line)).unwrap_or(&[]);
    render::runs_for_syntax_line_with_plain(
        &line.text,
        spans,
        &font(theme.font_mono.clone()),
        theme.text.opacity(0.92),
        theme,
    )
}

/// One half of a split row: the same accent bar / gutter / marker / code
/// columns a unified row uses, minus the second gutter — each half numbers
/// only its own side. Takes prebuilt `runs` so a mirrored row can share one
/// set across both columns.
pub(super) fn split_line_cell(
    line: &DiffLine,
    number: Option<u32>,
    runs: Vec<gpui::TextRun>,
    theme: &Theme,
    gutter_px: f32,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScroll>,
) -> gpui::Div {
    let mut add_bg = add_color(theme);
    add_bg.a = 0.055;
    let mut del_bg = del_color(theme);
    del_bg.a = 0.055;
    let (marker, marker_color, row_bg, accent, number_color) = match line.kind {
        LineKind::Add => (
            "+",
            add_color(theme),
            Some(add_bg),
            Some(add_color(theme).opacity(0.55)),
            add_color(theme).opacity(0.9),
        ),
        LineKind::Del => (
            "−",
            del_color(theme),
            Some(del_bg),
            Some(del_color(theme).opacity(0.55)),
            del_color(theme).opacity(0.9),
        ),
        _ => (
            "·",
            theme.text_faint.opacity(0.5),
            None,
            None,
            theme.text_faint.opacity(0.8),
        ),
    };
    let content_width = match code_width {
        DiffCodeWidth::Clipped => None,
        DiffCodeWidth::Scrollable(metrics) => Some(metrics.split_content_width(gutter_px)),
        DiffCodeWidth::Wrapped => None,
    };
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    div()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .overflow_hidden()
        .flex()
        .flex_row()
        .items_start()
        .when_some(row_bg, |el, bg| el.bg(bg))
        .child(
            div()
                .w(px(ACCENT_BAR_WIDTH))
                .self_stretch()
                .flex_none()
                .when_some(accent, |el, color| el.bg(color)),
        )
        .child(
            div()
                .w(px(gutter_px))
                .flex_none()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.0))
                .line_height(px(diff_line_height(theme)))
                .text_color(number_color)
                .flex()
                .justify_end()
                .pr(px(8.0))
                .child(SharedString::from(
                    number.map(|n| n.to_string()).unwrap_or_default(),
                )),
        )
        .child(
            div()
                .w(px(SPLIT_MARKER_WIDTH))
                .flex_none()
                .flex()
                .justify_center()
                .text_size(px(diff_text_size(theme)))
                .line_height(px(diff_line_height(theme)))
                .text_color(marker_color)
                .font_family(theme.font_mono.clone())
                .child(SharedString::from(marker)),
        )
        .child(code_text_viewport(
            line.text.clone(),
            runs,
            theme,
            SPLIT_CODE_PADDING_LEFT,
            content_width,
            wrapped,
            scroll,
        ))
}

/// The empty half of a one-sided split row — a pure-insert row has no old
/// line, and vice versa. A flat wash, quieter than either tint, reads as
/// "nothing here" without competing with the code beside it.
pub(super) fn split_filler() -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .bg(crate::theme::ink(0.03))
}

/// Compose the two halves with the centre hairline.
pub(super) fn split_row(left: AnyElement, right: AnyElement, wrapped: bool, theme: &Theme) -> gpui::Div {
    div()
        .map(|el| {
            if wrapped {
                el.min_h(px(diff_line_height(theme)))
            } else {
                el.h(px(diff_line_height(theme)))
            }
        })
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_stretch()
        .child(left)
        .child(
            div()
                .w(px(SPLIT_DIVIDER_WIDTH))
                .self_stretch()
                .flex_none()
                .bg(crate::theme::hairline(0.06)),
        )
        .child(right)
}

pub const COMMENT_ADDER_SIZE: f32 = 16.0;

/// A split row's `+` only ever appears in the right column, which carries one
/// gutter — so the offset is the same for every line. It is measured from the
/// column, not the row: the halves are fluid, so the right one has no
/// absolute left edge to measure from.
pub fn split_adder_left(gutter_px: f32) -> f32 {
    ACCENT_BAR_WIDTH + (gutter_px - COMMENT_ADDER_SIZE) / 2.0
}

/// A unified row carries both gutters side by side, and a deletion numbers in
/// the first.
pub fn comment_adder_left(side: CommentSide, gutter_px: f32) -> f32 {
    let column = match side {
        CommentSide::Old => 0.0,
        CommentSide::New => gutter_px,
    };
    ACCENT_BAR_WIDTH + column + (gutter_px - COMMENT_ADDER_SIZE) / 2.0
}

pub(super) fn positioned_adder(left: f32, adder: AnyElement) -> gpui::Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .h_full()
        .flex()
        .items_center()
        .child(adder)
}

pub(super) fn render_comment_adder(
    path: &str,
    side: CommentSide,
    line: u32,
    theme: &Theme,
    blocked: Option<&'static str>,
    cx: &Context<Changes>,
) -> AnyElement {
    let target = path.to_string();
    let Some(reason) = blocked else {
        return crate::comment_ui::render_comment_adder(
            format!("cmt-add-{path}-{}-{line}", side.tag()).into(),
            theme,
            cx,
            move |this, window, cx| this.open_draft(target.clone(), side, line, window, cx),
        );
    };
    // Disabled: looks dimmed, explains itself on hover, and a click only repeats the reason.
    let adder = crate::comment_ui::render_comment_adder(
        format!("cmt-add-{path}-{}-{line}", side.tag()).into(),
        theme,
        cx,
        move |this, _, cx| {
            this.local_draft_error = Some(reason.into());
            cx.notify();
        },
    );
    div()
        .id(SharedString::from(format!("cmt-add-disabled-{path}-{line}")))
        .opacity(0.4)
        .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(reason.into())).into())
        .child(adder)
        .into_any_element()
}

/// Mirrors [`ReviewComment::cite_path`] for the not-yet-staged note.
pub(super) fn draft_cite_path(draft: &CommentDraft) -> &str {
    match draft.side {
        CommentSide::Old => draft.old_path.as_deref().unwrap_or(&draft.path),
        CommentSide::New => &draft.path,
    }
}

/// The expanded body of one file section: notices, hunk headers, +/-/context
/// lines with a coloured accent bar, dual line-number gutters, a marker
/// column, and paint-only syntax runs (zeron checkout-diff-sidebar).
/// Shared with the transcript's tool-diff detail blocks — the same component
/// renders a checkout diff section and an inline ACP tool diff. (The changes
/// pane itself virtualizes these rows individually; this stacked form serves
/// the transcript and the fold tween's clipped stand-in.)
/// Full-document old/new highlighting for tool and checkout diffs.
pub(crate) fn render_file_body_with_syntax(
    file: &FileDiff,
    highlights: Option<Arc<DiffHighlights>>,
    theme: &Theme,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let gutter_px = gutter_width(file);
    for notice in file_notices(file) {
        children.push(notice_row(notice, theme));
    }
    for hunk in &file.hunks {
        children.push(hunk_header_row(&hunk.header, None, theme));
        for line in &hunk.lines {
            let spans = highlights
                .as_deref()
                .map(|highlights| highlights.spans(line))
                .unwrap_or(&[]);
            children.push(diff_line_row(
                line,
                spans,
                &[],
                theme,
                gutter_px,
                DiffCodeWidth::Clipped,
                None,
            ));
        }
    }
    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}

/// Build only rows that start above `max_px` so the fold tween's stand-in
/// never materializes lines its clip cannot reveal.
pub(super) fn render_file_body_upto(
    file: &FileDiff,
    highlight: Option<Arc<DiffHighlights>>,
    theme: &Theme,
    max_px: f32,
    mode: DiffMode,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScrollContext>,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let mut y = 0.0f32;
    let gutter_px = gutter_width(file);
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    let spans_for = |line: &DiffLine| {
        highlight
            .as_deref()
            .map(|highlights| highlights.spans(line))
            .unwrap_or(&[])
    };

    'build: {
        for notice in file_notices(file) {
            if y >= max_px {
                break 'build;
            }
            children.push(notice_row(notice, theme));
            y += NOTICE_HEIGHT;
        }
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            if y >= max_px {
                break 'build;
            }
            children.push(hunk_header_row(&hunk.header, None, theme));
            y += HUNK_HEADER_HEIGHT;
            match mode {
                DiffMode::Unified => {
                    for (line_ix, line) in hunk.lines.iter().enumerate() {
                        if y >= max_px {
                            break 'build;
                        }
                        children.push(diff_line_row(
                            line,
                            spans_for(line),
                            &[],
                            theme,
                            gutter_px,
                            code_width,
                            scroll
                                .as_ref()
                                .map(|scroll| scroll.slot(format_args!("{hunk_ix}-{line_ix}"))),
                        ));
                        y += diff_line_height(theme);
                    }
                }
                DiffMode::Split => {
                    // Pair only what the clip can still reveal: the unified
                    // arm breaks out of a lazy walk, so the split arm must not
                    // materialize the whole hunk first.
                    let budget = ((max_px - y) / diff_line_height(theme)).ceil().max(0.0) as usize;
                    for (pair_ix, (left, right)) in split_pairs_upto(&hunk.lines, budget)
                        .into_iter()
                        .enumerate()
                    {
                        if y >= max_px {
                            break 'build;
                        }
                        let line_at =
                            |slot: Option<u32>| slot.and_then(|slot| hunk.lines.get(slot as usize));
                        let cell = |line: Option<&DiffLine>, old: bool| match line {
                            Some(line) => split_line_cell(
                                line,
                                if old { line.old_no } else { line.new_no },
                                line_runs(line, highlight.as_deref(), theme),
                                theme,
                                gutter_px,
                                code_width,
                                scroll.as_ref().map(|scroll| {
                                    scroll.slot(format_args!(
                                        "{hunk_ix}-{pair_ix}-{}",
                                        if old { "old" } else { "new" }
                                    ))
                                }),
                            )
                            .into_any_element(),
                            None => split_filler().into_any_element(),
                        };
                        let (left, right) = (line_at(left), line_at(right));
                        let marker = [left, right]
                            .into_iter()
                            .flatten()
                            .find(|line| line.kind == LineKind::Meta);
                        children.push(match marker {
                            Some(line) => meta_line_row(
                                &line.text,
                                theme,
                                2.0 * (ACCENT_BAR_WIDTH + gutter_px),
                            ),
                            None => split_row(cell(left, true), cell(right, false), wrapped, theme)
                                .into_any_element(),
                        });
                        y += diff_line_height(theme);
                    }
                }
            }
        }
    }

    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}
