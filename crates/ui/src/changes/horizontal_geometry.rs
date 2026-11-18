use super::*;

/// Width inputs that are independent of the active window's font metrics.
/// They are computed once with the parsed patch, off the render path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DiffHorizontalGeometry {
    pub(super) max_code_columns: usize,
    pub(super) max_gutter_width: f32,
}

impl DiffHorizontalGeometry {
    pub(super) fn from_file(file: &FileDiff) -> Self {
        let max_code_columns = file
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .map(|line| visual_columns(&line.text))
            .max()
            .unwrap_or(0);
        let max_gutter_width = gutter_width(file);
        Self {
            max_code_columns,
            max_gutter_width,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Lines shaped by `max_shaped_text_width` on this thread (benchmarks).
    pub(crate) static SHAPED_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// Font lookups (`resolve_font` + `ch_advance`) performed by `diff_ch_advance`.
    pub(crate) static CH_ADVANCE_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static CH_ADVANCE_CACHE: std::cell::RefCell<Option<((u32, u32, u32, SharedString), f32)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn reset_ch_advance_cache() {
    CH_ADVANCE_CACHE.with(|cache| *cache.borrow_mut() = None);
}

/// Width of one monospace column at the diff text size. Resolving the font
/// and asking the shaper is done once per typography/theme change, not once
/// per rendered row.
pub(super) fn diff_ch_advance(
    text_system: &gpui::WindowTextSystem,
    theme: &Theme,
    typography_generation: u32,
) -> f32 {
    let size = px(diff_text_size(theme));
    let key = (
        typography_generation,
        crate::theme::style_generation(),
        diff_text_size(theme).to_bits(),
        theme.font_mono.clone(),
    );
    CH_ADVANCE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((cached_key, advance)) = cache.as_ref()
            && *cached_key == key {
                return *advance;
            }
        #[cfg(test)]
        CH_ADVANCE_LOOKUPS.with(|count| count.set(count.get() + 1));
        let mono = font(theme.font_mono.clone());
        let advance = text_system
            .ch_advance(text_system.resolve_font(&mono), size)
            .unwrap_or(size * 0.6)
            .as_f32();
        *cache = Some((key, advance));
        advance
    })
}

/// Measure the same runs the row paints. Color boundaries can break kerning
/// and ligatures on native platforms even when every run uses the same font.
pub(super) fn max_shaped_text_width(
    file: &FileDiff,
    highlights: Option<&DiffHighlights>,
    theme: &Theme,
    text_system: &gpui::WindowTextSystem,
    generation: u32,
) -> f32 {
    let size = px(diff_text_size(theme));
    let column_width = diff_ch_advance(text_system, theme, generation);
    #[cfg(test)]
    SHAPED_LINES.with(|count| {
        count.set(count.get() + file.hunks.iter().map(|hunk| hunk.lines.len()).sum::<usize>())
    });
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .fold(0.0f32, |widest, line| {
            let runs = line_runs(line, highlights, theme);
            let shaped = text_system
                .shape_line(line.text.clone().into(), size, &runs, None)
                .width()
                .as_f32();
            // Preserve the old column estimate as a floor, including tab stops.
            widest
                .max(shaped)
                .max(visual_columns(&line.text) as f32 * column_width)
        })
}

/// Count terminal-style display columns, including tab stops and wide
/// Unicode glyphs. This is only a floor; actual shaped runs determine the extent.
pub(super) fn visual_columns(text: &str) -> usize {
    text.chars().fold(0usize, |columns, ch| {
        if ch == '\t' {
            columns + (DIFF_TAB_SIZE - columns % DIFF_TAB_SIZE)
        } else {
            columns + ch.width().unwrap_or(0)
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DiffHorizontalMetrics {
    pub(super) max_text_width: f32,
    pub(super) max_gutter_width: f32,
}

impl DiffHorizontalMetrics {
    /// Compensating for the file-local gutter keeps every unified code
    /// viewport's effective scroll range identical.
    pub(super) fn unified_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + UNIFIED_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + 2.0 * (self.max_gutter_width - gutter_width)
    }

    /// Split has one gutter per half. Both halves use this same extent so old
    /// and new remain synchronized even when one side is a filler.
    pub(super) fn split_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + SPLIT_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + (self.max_gutter_width - gutter_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum DiffCodeWidth {
    /// Inline tool diffs keep their existing local clipping behavior.
    Clipped,
    /// Changes rows expose a stable intrinsic code width.
    Scrollable(DiffHorizontalMetrics),
    /// Changes rows consume their viewport width and grow vertically.
    Wrapped,
}

#[derive(Clone)]
pub(super) struct DiffCodeScroll {
    pub(super) handle: gpui::ScrollHandle,
    pub(super) id: SharedString,
}

#[derive(Clone)]
pub(super) struct DiffCodeScrollContext {
    pub(super) handle: gpui::ScrollHandle,
    pub(super) prefix: SharedString,
}

impl DiffCodeScrollContext {
    pub(super) fn slot(&self, suffix: impl std::fmt::Display) -> DiffCodeScroll {
        DiffCodeScroll {
            handle: self.handle.clone(),
            id: SharedString::from(format!("{}-{suffix}", self.prefix)),
        }
    }
}

pub(super) fn reset_horizontal_scroll(handle: &gpui::ScrollHandle) {
    handle.set_offset(gpui::Point::default());
}

pub(super) struct MeasuredDiffWidth {
    pub(super) key: (u32, u32, u32, SharedString),
    // Retaining the Arc makes pointer identity safe against allocator reuse.
    pub(super) highlights: Option<Arc<DiffHighlights>>,
    pub(super) width: f32,
}

pub(super) struct FileHorizontalState {
    pub(super) geometry: DiffHorizontalGeometry,
    pub(super) scroll: gpui::ScrollHandle,
    /// Shape once per file, typography/theme, and highlight revision.
    pub(super) measured: std::cell::RefCell<Option<MeasuredDiffWidth>>,
}

impl FileHorizontalState {
    pub(super) fn new(file: &FileDiff) -> Self {
        Self {
            geometry: DiffHorizontalGeometry::from_file(file),
            scroll: gpui::ScrollHandle::new(),
            measured: std::cell::RefCell::new(None),
        }
    }

    pub(super) fn metrics(
        &self,
        file: &FileDiff,
        highlights: Option<&Arc<DiffHighlights>>,
        theme: &Theme,
        text_system: &gpui::WindowTextSystem,
        generation: u32,
    ) -> DiffHorizontalMetrics {
        let key = (
            generation,
            crate::theme::style_generation(),
            diff_text_size(theme).to_bits(),
            theme.font_mono.clone(),
        );
        let mut cached = self.measured.borrow_mut();
        let current = cached.as_ref().is_some_and(|cached| {
            cached.key == key
                && match (cached.highlights.as_ref(), highlights) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                }
        });
        if !current {
            *cached = Some(MeasuredDiffWidth {
                key,
                highlights: highlights.cloned(),
                width: max_shaped_text_width(
                    file,
                    highlights.map(AsRef::as_ref),
                    theme,
                    text_system,
                    generation,
                ),
            });
        }
        DiffHorizontalMetrics {
            max_text_width: cached.as_ref().unwrap().width,
            max_gutter_width: self.geometry.max_gutter_width,
        }
    }
}

