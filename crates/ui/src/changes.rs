//! The right-pane "Changes" content (feature-inventory §1.11): a unified-diff
//! viewer over `WatchCheckoutDiffs`.
//!
//! - pure patch parser: `diff --git` sections → file/hunk/line/notice rows,
//!   with add/delete/rename/binary detection and per-file counts;
//! - resolution: the shown diff matches the selected chat by `checkout_id`
//!   first, then by device+cwd, then cwd alone;
//! - states: *preparing* (no diff yet), *clean* (empty patch), *list*; a watch
//!   error shows a banner while the last content stays;
//! - virtualized with gpui `list()` at LINE granularity — every file header,
//!   hunk header, and diff line is its own row (the flat model Zed's editor
//!   uses for its project diff: only the visible slice materializes, and a
//!   collapsed file's body rows are removed from the list outright, not
//!   hidden); nowrap sections collapse with a 180 ms height tween on a
//!   clipped stand-in row (analytic heights, capped to what the clip can
//!   reveal), while variable-height wrapped sections settle immediately;
//! - syntax highlight reuses the markdown tokenizer per diff line, computed
//!   time-sliced on the background executor and applied as paint-only run
//!   colors (layout never changes);
//! - scopes (t3code parity): *Working tree* rides the watch stream; *Branch
//!   changes* (vs a selectable base ref, default branch preselected) and
//!   *Latest turn* fetch one-shot `GetCheckoutDiff` captures, refreshed when
//!   the watch checksum says the tree moved;
//! - two layouts ([`DiffMode`], toolbar toggle, persisted): *unified* stacks
//!   old and new; *split* pairs each hunk's deletions against its additions
//!   into one row with two columns. Split is a pure re-flatten of the same
//!   parse — the row model, virtualization, folds, and highlights are shared.
//!   Its left column is inert: notes are cited against the post-change file,
//!   so only the right column takes a `+` (already-staged old-side notes
//!   still show their cards).
//! - long-line wrapping is a persisted toolbar choice. It only changes the
//!   code plane: gutters, comment affordances, and sticky headers stay fixed,
//!   while the virtual list measures each logical row's resulting height.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, ListAlignment, ListState,
    SharedString, Subscription, Task, Window, div, font, list, prelude::*, px,
};
use unicode_width::UnicodeWidthChar as _;

use mast_proto::{Chat, CheckoutDiff, GitHistoryCommit, SubmoduleDiffSummary};
use mast_rpc::methods;

use crate::comments::{self, CommentSide, ReviewComment};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::history::{
    GitHistory, GitHistoryCount, GitHistoryEvent, GitHistoryFetchButton, GitHistorySearchControl,
    GitHistoryViewButton,
};
use crate::markdown::render;
use crate::motion::{self, AnimationExt as _, CHEVRON, COLLAPSE};
use crate::popover::{self, Popup};
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;
use mast_syntax::LanguageId as Lang;

mod code_ref;
mod comment_drafts;
mod diff_painters;
mod expand_context;
mod file_list;
mod fold_state;
mod header_controls;
mod highlight_requests;
mod explanation;
mod ref_picker;
mod ref_and_checkout_menus;
mod review_state;
mod review_drafts;
mod row_renderer;
mod word_diff;
pub(crate) mod walkthrough;

use diff_painters::*;
pub use diff_painters::{comment_adder_left, split_adder_left};
use file_list::*;
use review_state::*;

/// The chat/worktree identity a Changes surface was created for.
///
/// `chat_id` never changes. The remaining fields may be refreshed only from
/// that chat's authoritative registry row; selecting another chat must never
/// retarget a hidden or restored Changes surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangesTarget {
    pub chat_id: String,
    pub device_id: String,
    pub cwd: Option<String>,
    pub checkout_id: Option<String>,
    pub branch: Option<String>,
    /// A bound Chat's selected repository that validation has not confirmed:
    /// why (the validation remediation). The pane shows no diff and no
    /// actions for it rather than whatever sits at the configured path.
    pub unconfirmed: Option<String>,
    /// Match a diff only by `checkout_id` (a validated member), never by a
    /// cwd fallback.
    pub exact_checkout: bool,
    /// Set when the selected pill is a branch / PR range target: the pane
    /// shows `merge-base(base, head)...head` from refs ([`DiffScope::Range`])
    /// of the repository checkout in `cwd`, with no working tree or turn
    /// scopes.
    pub range: Option<crate::work_outline::execution::RefRange>,
}

impl ChangesTarget {
    fn from_chat(chat: &Chat) -> Self {
        Self {
            chat_id: chat.id.clone(),
            device_id: chat.device_id.clone(),
            cwd: chat.cwd.clone(),
            checkout_id: chat.checkout_id.clone(),
            branch: chat.branch.clone(),
            unconfirmed: None,
            exact_checkout: false,
            range: None,
        }
    }

    /// A bound Chat (Work Outline execution binding) shows one repository at
    /// a time: the picked member's checkout, through the same scoped
    /// `WatchCheckoutDiffs {cwd, checkoutId}`. Unbound Chats are unchanged.
    ///
    /// A member validation has not confirmed (validation failed or has not
    /// answered yet) gets no target at all: no watch, no cwd fallback, no
    /// actions; the pane explains why instead.
    fn for_chat_in(chat: &Chat, state: &AppState) -> Self {
        let mut target = Self::from_chat(chat);
        if let Some(member) = state.selected_execution_member(&chat.id) {
            // The chat's own checkout (listed beside diff targets) is the
            // plain chat target: cwd fallback included, exactly as before
            // the chat had any target.
            if member.kind == crate::work_outline::execution::MemberKind::Own {
                return target;
            }
            match member.unconfirmed {
                Some(reason) => {
                    target.cwd = None;
                    target.checkout_id = None;
                    target.branch = None;
                    target.unconfirmed = Some(reason);
                }
                None => {
                    target.cwd = Some(member.checkout_path);
                    target.checkout_id = member.checkout_id;
                    target.branch = member.branch;
                    target.exact_checkout = true;
                    target.range = member.range;
                }
            }
        }
        target
    }

    fn resolve<'a>(&self, diffs: &'a [CheckoutDiff]) -> Option<&'a CheckoutDiff> {
        if self.unconfirmed.is_some() {
            return None;
        }
        if let Some(checkout_id) = self.checkout_id.as_deref()
            && let Some(diff) = diffs.iter().find(|diff| diff.checkout_id == checkout_id)
        {
            return Some(diff);
        }
        if self.exact_checkout {
            return None;
        }
        let cwd = self.cwd.as_deref()?;
        diffs
            .iter()
            .find(|diff| diff.device_id == self.device_id && diff.cwd == cwd)
    }

    fn repo_and_worktree(&self) -> (String, String) {
        let Some(cwd) = self.cwd.as_deref() else {
            return ("Unknown repo".to_string(), "Unknown worktree".to_string());
        };
        let path = Path::new(cwd);
        let worktree = path
            .file_name()
            .and_then(|part| part.to_str())
            .unwrap_or(cwd)
            .to_string();
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|part| part.to_str());
        let grandparent = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|part| part.to_str());
        let repo = if matches!(grandparent, Some(".worktrees" | "worktrees"))
            && let Some(repo) = parent
        {
            // `<workspace>/.worktrees/<repo>/<name>`: the layout agents use
            // for a submodule's linked worktrees.
            repo.to_string()
        } else if matches!(parent, Some(".worktrees" | "worktrees")) {
            path.parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|part| part.to_str())
                .unwrap_or(&worktree)
                .to_string()
        } else {
            // For an ordinary checkout the checkout folder is the repository
            // name. Avoid claiming its containing projects folder is the repo.
            worktree.clone()
        };
        (repo, worktree)
    }
}

// ---------------------------------------------------------------------------
// Layout numbers (analytic — they drive the fold tween)
// ---------------------------------------------------------------------------

pub const FILE_HEADER_HEIGHT: f32 = crate::surface_chrome::HEADER_HEIGHT;
const STICKY_FILE_HEADER_BLUR: f32 = 16.0;
/// Coverage of the theme's content-plane tint over the sticky header blur.
/// Light needs substantially more coverage: dark text is much more vulnerable
/// to rows ghosting through the blur than light text is on a dark tint.
const STICKY_FILE_HEADER_TINT_ALPHA_DARK: f32 = 0.40;
const STICKY_FILE_HEADER_TINT_ALPHA_LIGHT: f32 = 0.85;
pub const HUNK_HEADER_HEIGHT: f32 = 28.0;
pub const DIFF_LINE_HEIGHT: f32 = 21.0;
pub const NOTICE_HEIGHT: f32 = 24.0;
pub const BODY_BOTTOM_PAD: f32 = 8.0;
/// Gutter width per line-number column.
pub const GUTTER_WIDTH: f32 = 36.0;
/// The +/−/· marker column between the gutters and the code.
pub const MARKER_WIDTH: f32 = 28.0;
/// Width of the coloured accent bar on the left edge of +/− rows.
pub const ACCENT_BAR_WIDTH: f32 = 3.0;
/// The marker column in split mode: each half pays for its own, so it is
/// narrower than [`MARKER_WIDTH`] to leave the code the room.
pub const SPLIT_MARKER_WIDTH: f32 = 18.0;
/// Hairline between the two split columns.
pub const SPLIT_DIVIDER_WIDTH: f32 = 1.0;
const DIFF_TEXT_SIZE: f32 = 12.0;
const DIFF_TAB_SIZE: usize = 4;
/// One shared `code_font_size` setting drives several surfaces that never
/// agreed on a size historically. Each scales off its own baseline so the
/// default setting reproduces the size that surface always had, and a
/// user-chosen size moves them all while keeping those proportions.
const DIFF_TEXT_SIZE_RATIO: f32 = DIFF_TEXT_SIZE / crate::typography::CODE_FONT_SIZE_DEFAULT;

/// Size of the painted diff body text, and the size the column measurement in
/// [`DiffHorizontalGeometry::resolve`] must use: they desync otherwise.
fn diff_text_size(theme: &Theme) -> f32 {
    crate::typography::clamp_font_size(theme.code_font_size * DIFF_TEXT_SIZE_RATIO)
}

/// The row box and the painted line box must agree, or code clips once the
/// user moves the code font size off [`DIFF_TEXT_SIZE`].
fn diff_line_height(theme: &Theme) -> f32 {
    diff_text_size(theme) * (DIFF_LINE_HEIGHT / DIFF_TEXT_SIZE)
}

const UNIFIED_CODE_PADDING_LEFT: f32 = 12.0;
const SPLIT_CODE_PADDING_LEFT: f32 = 6.0;
/// Breathing room after the widest source line when scrolled fully right.
const CODE_PADDING_RIGHT: f32 = 24.0;

/// How the diff is laid out. Persisted in `ui-settings.json` (`diffSplit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    /// One column: deletions above additions (the classic patch reading).
    #[default]
    Unified,
    /// Two columns: old on the left, new on the right, paired per hunk.
    Split,
}

impl DiffMode {
    pub fn from_split(split: bool) -> Self {
        if split { Self::Split } else { Self::Unified }
    }

    pub fn is_split(self) -> bool {
        self == Self::Split
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Unified => Self::Split,
            Self::Split => Self::Unified,
        }
    }
}

// ---------------------------------------------------------------------------
// Patch model + parser (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file` and friends.
    Meta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceSide {
    Old,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceLineRef {
    pub side: SourceSide,
    /// One-based source line number.
    pub line_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffHighlights {
    pub old: Option<Arc<mast_syntax::HighlightedDocument>>,
    pub new: Option<Arc<mast_syntax::HighlightedDocument>>,
    pub hunk_symbols: Vec<Option<String>>,
}

impl DiffHighlights {
    pub fn source_ref(&self, line: &DiffLine) -> Option<SourceLineRef> {
        match line.kind {
            LineKind::Del => line.old_no.map(|line_number| SourceLineRef {
                side: SourceSide::Old,
                line_number,
            }),
            LineKind::Add => line.new_no.map(|line_number| SourceLineRef {
                side: SourceSide::New,
                line_number,
            }),
            LineKind::Context => line
                .new_no
                .filter(|_| self.new.is_some())
                .map(|line_number| SourceLineRef {
                    side: SourceSide::New,
                    line_number,
                })
                .or_else(|| {
                    line.old_no.map(|line_number| SourceLineRef {
                        side: SourceSide::Old,
                        line_number,
                    })
                }),
            LineKind::Meta => None,
        }
    }

    pub fn spans(&self, line: &DiffLine) -> &[mast_syntax::HighlightSpan] {
        let Some(source_ref) = self.source_ref(line) else {
            return &[];
        };
        let document = match source_ref.side {
            SourceSide::Old => self.old.as_deref(),
            SourceSide::New => self.new.as_deref(),
        };
        document
            .and_then(|document| document.lines.get(source_ref.line_number as usize - 1))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    /// Display path (the post-change side).
    pub path: String,
    /// Pre-rename path, when different.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    /// Parser-collected notices (mode changes etc.).
    pub notices: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
    /// Largest line number on either side — sizes the gutters analytically
    /// (a fixed column overflowed past 4 digits; user report).
    pub max_line: u32,
}

impl FileDiff {
    fn new(path: String, old_path: Option<String>) -> Self {
        Self {
            path,
            old_path,
            status: FileStatus::Modified,
            binary: false,
            notices: Vec::new(),
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            max_line: 0,
        }
    }
}

/// Width of one line-number gutter column, fitted to the file's largest
/// line number: 11px mono ≈ 6.6px per digit, the 8px right pad, and a 6px
/// left gap so the number never abuts the accent bar (at 4 digits the old
/// formula left 1.6px — visually touching; user report). Never narrower
/// than the classic 36px column.
pub fn gutter_width(file: &FileDiff) -> f32 {
    let digits = file.max_line.max(1).ilog10() + 1;
    (digits as f32 * 6.6 + 8.0 + 6.0).max(GUTTER_WIDTH)
}

/// Width inputs that are independent of the active window's font metrics.
/// They are computed once with the parsed patch, off the render path.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DiffHorizontalGeometry {
    max_code_columns: usize,
    max_gutter_width: f32,
}

impl DiffHorizontalGeometry {
    fn from_file(file: &FileDiff) -> Self {
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
fn diff_ch_advance(
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
fn max_shaped_text_width(
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
fn visual_columns(text: &str) -> usize {
    text.chars().fold(0usize, |columns, ch| {
        if ch == '\t' {
            columns + (DIFF_TAB_SIZE - columns % DIFF_TAB_SIZE)
        } else {
            columns + ch.width().unwrap_or(0)
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DiffHorizontalMetrics {
    max_text_width: f32,
    max_gutter_width: f32,
}

impl DiffHorizontalMetrics {
    /// Compensating for the file-local gutter keeps every unified code
    /// viewport's effective scroll range identical.
    fn unified_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + UNIFIED_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + 2.0 * (self.max_gutter_width - gutter_width)
    }

    /// Split has one gutter per half. Both halves use this same extent so old
    /// and new remain synchronized even when one side is a filler.
    fn split_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + SPLIT_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + (self.max_gutter_width - gutter_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DiffCodeWidth {
    /// Inline tool diffs keep their existing local clipping behavior.
    Clipped,
    /// Changes rows expose a stable intrinsic code width.
    Scrollable(DiffHorizontalMetrics),
    /// Changes rows consume their viewport width and grow vertically.
    Wrapped,
}

#[derive(Clone)]
struct DiffCodeScroll {
    handle: gpui::ScrollHandle,
    id: SharedString,
}

#[derive(Clone)]
struct DiffCodeScrollContext {
    handle: gpui::ScrollHandle,
    prefix: SharedString,
}

impl DiffCodeScrollContext {
    fn slot(&self, suffix: impl std::fmt::Display) -> DiffCodeScroll {
        DiffCodeScroll {
            handle: self.handle.clone(),
            id: SharedString::from(format!("{}-{suffix}", self.prefix)),
        }
    }
}

fn reset_horizontal_scroll(handle: &gpui::ScrollHandle) {
    handle.set_offset(gpui::Point::default());
}

fn strip_git_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Split the tail of a `diff --git a/… b/…` line into (old, new) paths.
/// Quoted paths (spaces/unicode) are handled; for unquoted paths with spaces
/// the split favors the last ` b/` separator, which is git's own convention.
fn parse_git_paths(rest: &str) -> (String, String) {
    fn unquote(s: &str) -> String {
        let trimmed = s.trim();
        if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
            trimmed[1..trimmed.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
        } else {
            trimmed.to_string()
        }
    }
    if let Some(pos) = rest.rfind(" b/").or_else(|| rest.rfind(" \"b/")) {
        let old = unquote(&rest[..pos]);
        let new = unquote(&rest[pos + 1..]);
        (
            strip_git_prefix(&old).to_string(),
            strip_git_prefix(&new).to_string(),
        )
    } else {
        let p = strip_git_prefix(&unquote(rest)).to_string();
        (p.clone(), p)
    }
}

/// Parse one `@@ -a[,b] +c[,d] @@ …` header into starting line numbers.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@")?;
    let minus = rest.find('-')?;
    let after_minus = &rest[minus + 1..];
    let old: u32 = after_minus
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()?
        .parse()
        .ok()?;
    let plus = rest.find('+')?;
    let after_plus = &rest[plus + 1..];
    let new: u32 = after_plus
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

/// Parse a unified git patch into file sections. Tolerant: unknown header
/// lines are skipped, truncated hunks keep what parsed so far.
pub fn parse_patch(patch: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut in_hunk = false;
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;

    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let (old, new) = parse_git_paths(rest);
            let old_path = (old != new).then_some(old);
            files.push(FileDiff::new(new, old_path));
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };

        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(raw) {
                old_no = o;
                new_no = n;
                file.hunks.push(Hunk {
                    header: raw.to_string(),
                    lines: Vec::new(),
                });
                in_hunk = true;
            }
            continue;
        }

        if in_hunk {
            let mut chars = raw.chars();
            let marker = chars.next();
            let body: String = chars.collect();
            let line = match marker {
                Some('+') => {
                    file.additions += 1;
                    let l = DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text: body,
                    };
                    new_no += 1;
                    Some(l)
                }
                Some('-') => {
                    file.deletions += 1;
                    let l = DiffLine {
                        kind: LineKind::Del,
                        old_no: Some(old_no),
                        new_no: None,
                        text: body,
                    };
                    old_no += 1;
                    Some(l)
                }
                Some(' ') | None => {
                    let l = DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: body,
                    };
                    old_no += 1;
                    new_no += 1;
                    Some(l)
                }
                Some('\\') => Some(DiffLine {
                    kind: LineKind::Meta,
                    old_no: None,
                    new_no: None,
                    text: raw.trim_start_matches('\\').trim().to_string(),
                }),
                _ => {
                    // A non-hunk line ends the hunk; reprocess as a header.
                    in_hunk = false;
                    None
                }
            };
            if let Some(line) = line
                && let Some(hunk) = file.hunks.last_mut()
            {
                file.max_line = file
                    .max_line
                    .max(line.old_no.unwrap_or(0))
                    .max(line.new_no.unwrap_or(0));
                hunk.lines.push(line);
                continue;
            }
            if in_hunk {
                continue;
            }
        }

        // File header territory.
        if raw.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if raw.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(from) = raw.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(from.trim().to_string());
        } else if let Some(to) = raw.strip_prefix("rename to ") {
            file.status = FileStatus::Renamed;
            file.path = to.trim().to_string();
        } else if raw.starts_with("Binary files") || raw.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(mode) = raw.strip_prefix("new mode ") {
            file.notices
                .push(format!("Mode changed to {}", mode.trim()));
        } else if let Some(new) = raw.strip_prefix("+++ ") {
            let new = new.trim();
            if new == "/dev/null" {
                file.status = FileStatus::Deleted;
            } else if file.old_path.is_none() {
                file.path = strip_git_prefix(new).to_string();
            }
        } else if let Some(old) = raw.strip_prefix("--- ")
            && old.trim() == "/dev/null"
        {
            file.status = FileStatus::Added;
        }
        // "index …", "similarity index …", "old mode …" etc.: skipped.
    }
    files
}

/// Derived per-file notice rows (new/deleted/renamed/binary + parser notices).
pub fn file_notices(file: &FileDiff) -> Vec<String> {
    let mut notices = Vec::new();
    match file.status {
        FileStatus::Added => notices.push("New file".to_string()),
        FileStatus::Deleted => notices.push("Deleted file".to_string()),
        FileStatus::Renamed => {
            let from = file.old_path.as_deref().unwrap_or("?");
            notices.push(format!("Renamed from {from}"));
        }
        FileStatus::Modified => {}
    }
    if file.binary {
        notices.push("Binary file — contents not shown".to_string());
    }
    notices.extend(file.notices.iter().cloned());
    notices
}

/// Cap a file's hunks at `max_lines` total diff lines, appending a notice
/// when lines were dropped. The transcript renders a tool diff as ONE
/// stacked element inside its row, so an unbounded diff (a fetched
/// full-diff blob, a whole-file rewrite) would otherwise build tens of
/// thousands of elements every frame it is visible.
pub fn truncate_file_lines(file: &mut FileDiff, max_lines: usize) {
    let total: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    if total <= max_lines {
        return;
    }
    let mut budget = max_lines;
    file.hunks.retain_mut(|hunk| {
        if budget == 0 {
            return false;
        }
        if hunk.lines.len() > budget {
            hunk.lines.truncate(budget);
        }
        budget -= hunk.lines.len();
        true
    });
    file.notices.push(format!(
        "Diff truncated — showing first {max_lines} of {total} lines"
    ));
    // The gutter fits what actually renders.
    file.max_line = file
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| l.old_no.unwrap_or(0).max(l.new_no.unwrap_or(0)))
        .max()
        .unwrap_or(0);
}

/// Analytic expanded-body height — drives the 180 ms fold tween without
/// measurement.
pub fn body_height(file: &FileDiff) -> f32 {
    body_height_with(file, &[], None, DiffMode::Unified, DIFF_LINE_HEIGHT)
}

pub fn body_height_with(
    file: &FileDiff,
    comments: &[ReviewComment],
    draft: Option<(CommentSide, u32)>,
    mode: DiffMode,
    line_h: f32,
) -> f32 {
    body_height_with_card_heights(file, comments, draft, mode, line_h, |comment| comments::card_height(&comment.body))
}

fn body_height_with_card_heights(
    file: &FileDiff,
    comments: &[ReviewComment],
    draft: Option<(CommentSide, u32)>,
    mode: DiffMode,
    line_h: f32,
    card_height: impl Fn(&ReviewComment) -> f32,
) -> f32 {
    body_rows(0, file, comments, draft, mode)
        .iter()
        .map(|row| row.height(comments, line_h, &card_height))
        .sum()
}

/// One split row: indices into the hunk's lines for the left (old) and right
/// (new) column. `None` on a side means that column is empty for this row.
pub type LinePair = (Option<u32>, Option<u32>);

/// Pair a hunk's lines into split rows.
///
/// A hunk reads as runs: context lines sit on both sides, and each run of
/// deletions immediately followed by additions is a *change block* whose two
/// sides line up index-for-index (the shape git already emits — an edited
/// line's `-`/`+` are adjacent). The longer side's leftovers get one-sided
/// rows, so a 3-for-1 rewrite is 1 paired row and 2 add-only rows rather than
/// a ragged interleave. A deletion arriving after additions opens a new block
/// (`-a +b -c +d` is two edits, not one four-line one).
///
/// Pure and index-only: the caller keeps owning the lines, and the result is
/// small enough to live in the row model.
pub fn split_pairs(lines: &[DiffLine]) -> Vec<LinePair> {
    split_pairs_upto(lines, usize::MAX)
}

/// [`split_pairs`], stopping at `max_rows`.
///
/// The fold tween's stand-in builds only the slice its clip can reveal and
/// re-renders every frame of the tween, so it must not pay to pair a 50k-line
/// hunk to draw twenty rows of it. Bounding the *output* is not enough — the
/// pending runs are bounded too, since a change block yields
/// `max(dels, adds)` rows and so anything past the budget can only land past
/// it as well.
pub fn split_pairs_upto(lines: &[DiffLine], max_rows: usize) -> Vec<LinePair> {
    /// The block being accumulated: the two sides' code lines, plus the
    /// `\ No newline…` marker each side may end on.
    #[derive(Default)]
    struct Block {
        dels: Vec<u32>,
        adds: Vec<u32>,
        del_meta: Vec<u32>,
        add_meta: Vec<u32>,
    }

    fn flush(pairs: &mut Vec<LinePair>, block: &mut Block, max_rows: usize) {
        let mut drain = |left: &mut Vec<u32>, right: &mut Vec<u32>| {
            for ix in 0..left.len().max(right.len()) {
                if pairs.len() >= max_rows {
                    break;
                }
                pairs.push((left.get(ix).copied(), right.get(ix).copied()));
            }
            left.clear();
            right.clear();
        };
        drain(&mut block.dels, &mut block.adds);
        // Markers trail the code they annotate, and pair with each other — a
        // modification where both files lost their final newline is one
        // aligned row plus one marker row, not two one-sided rows plus two
        // markers. They never share a row with code, so both render arms can
        // treat a marker on either side as spanning the row.
        drain(&mut block.del_meta, &mut block.add_meta);
    }

    let mut pairs = Vec::with_capacity(lines.len().min(max_rows));
    let mut block = Block::default();
    let mut pending_side: Option<LineKind> = None;
    for (ix, line) in lines.iter().enumerate() {
        match line.kind {
            LineKind::Del => {
                // A marker already closes its side, so code arriving after one
                // starts a fresh block — the marker row keeps its place in the
                // file's order.
                if !block.adds.is_empty()
                    || !block.del_meta.is_empty()
                    || !block.add_meta.is_empty()
                {
                    flush(&mut pairs, &mut block, max_rows);
                }
                let remaining = max_rows - pairs.len().min(max_rows);
                if remaining == 0 {
                    break;
                }
                if block.dels.len() < remaining {
                    block.dels.push(ix as u32);
                }
                pending_side = Some(LineKind::Del);
            }
            LineKind::Add => {
                // The old side's marker is the one case where a marker does
                // not close the block: `-old`, marker, `+new` is one edit.
                if !block.add_meta.is_empty() {
                    flush(&mut pairs, &mut block, max_rows);
                }
                let remaining = max_rows - pairs.len().min(max_rows);
                if remaining == 0 {
                    break;
                }
                if block.adds.len() < remaining {
                    block.adds.push(ix as u32);
                }
                pending_side = Some(LineKind::Add);
            }
            // `\ No newline at end of file` belongs to the side whose line it
            // follows, so it must NOT close the block: git writes `-old`,
            // marker, `+new`, marker for an edited last line, and treating
            // either marker as a boundary would tear that edit apart. A marker
            // after context describes the same line on both sides.
            LineKind::Meta => match pending_side {
                Some(LineKind::Del) => block.del_meta.push(ix as u32),
                Some(LineKind::Add) => block.add_meta.push(ix as u32),
                _ => {
                    block.del_meta.push(ix as u32);
                    block.add_meta.push(ix as u32);
                }
            },
            // Context sits on both sides.
            LineKind::Context => {
                flush(&mut pairs, &mut block, max_rows);
                if pairs.len() >= max_rows {
                    break;
                }
                pairs.push((Some(ix as u32), Some(ix as u32)));
                pending_side = Some(LineKind::Context);
            }
        }
    }
    flush(&mut pairs, &mut block, max_rows);
    pairs.truncate(max_rows);
    pairs
}

/// Every anchor a split row can *display* a card for, left column first.
///
/// Wider than what the row lets you write: only the right column takes a `+`
/// (see the `SplitLine` render arm), but an old-side note staged from the
/// unified layout must still show its card here, or toggling layouts would
/// look like it dropped one. A context row names the same anchor on both
/// sides, so the duplicate is dropped — the caller flattens, and two
/// identical anchors would stage the card twice. A fixed array, not a `Vec`:
/// this runs per row of every re-flatten.
fn pair_anchors(lines: &[DiffLine], pair: LinePair) -> [Option<(CommentSide, u32)>; 2] {
    let anchor = |ix: Option<u32>| {
        ix.and_then(|ix| lines.get(ix as usize))
            .and_then(line_anchor)
    };
    let (left, right) = (anchor(pair.0), anchor(pair.1));
    if left == right {
        [left, None]
    } else {
        [left, right]
    }
}

/// A deletion only exists in the pre-change file; everything else is cited
/// against the post-change file, which is what the agent edits.
pub fn line_anchor(line: &DiffLine) -> Option<(CommentSide, u32)> {
    match line.kind {
        LineKind::Meta => None,
        LineKind::Del => line.old_no.map(|no| (CommentSide::Old, no)),
        _ => line.new_no.map(|no| (CommentSide::New, no)),
    }
}

// ---------------------------------------------------------------------------
// Resolution + states (pure)
// ---------------------------------------------------------------------------

/// The diff shown for a chat: `checkout_id` match first, then exact
/// device+cwd. Never fall across devices merely because two hosts expose the
/// same path string.
pub fn resolve_diff<'a>(diffs: &'a [CheckoutDiff], chat: &Chat) -> Option<&'a CheckoutDiff> {
    if let Some(checkout_id) = chat.checkout_id.as_deref()
        && let Some(diff) = diffs.iter().find(|d| d.checkout_id == checkout_id)
    {
        return Some(diff);
    }
    let cwd = chat.cwd.as_deref()?;
    diffs
        .iter()
        .find(|d| d.device_id == chat.device_id && d.cwd == cwd)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPhase {
    /// No diff for this checkout yet.
    Preparing,
    /// Diff arrived and it's empty — working tree clean.
    Clean,
    List,
}

pub fn diff_phase(resolved: Option<&CheckoutDiff>) -> DiffPhase {
    match resolved {
        None => DiffPhase::Preparing,
        Some(diff) if diff.patch.trim().is_empty() && diff.files.is_empty() => DiffPhase::Clean,
        Some(_) => DiffPhase::List,
    }
}

fn pane_diff_phase(
    no_chat: bool,
    pinned_checkout: bool,
    resolved: Option<&CheckoutDiff>,
) -> DiffPhase {
    if no_chat && !pinned_checkout {
        DiffPhase::Clean
    } else {
        diff_phase(resolved)
    }
}

fn shows_scoped_error(no_chat: bool, pinned_checkout: bool, scope: DiffScope) -> bool {
    (!no_chat || pinned_checkout) && (scope != DiffScope::WorkingTree || pinned_checkout)
}

/// Header label: "N Uncommitted change(s)".
pub fn uncommitted_label(count: usize) -> String {
    if count == 1 {
        "1 Uncommitted change".to_string()
    } else {
        format!("{count} Uncommitted changes")
    }
}

/// What the pane diffs against (t3code's scope dropdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffScope {
    /// Uncommitted changes vs HEAD — the live watch stream.
    #[default]
    WorkingTree,
    /// Everything this branch adds over `merge-base(base_ref, HEAD)`,
    /// working tree included.
    Branch,
    /// Changes since the current chat's last turn started.
    LatestTurn,
    /// Repository commit graph. History owns this scope in its own right-pane
    /// surface tab; it remains a `Changes` variant so commit rows can open
    /// their corresponding diff tabs through the existing event path.
    History,
    /// One commit's own changes (parent vs commit) — the per-commit tab a
    /// History row click opens. Never listed in the scope menu
    /// ([`Self::ALL`]); a commit-pinned pane is born this way and stays.
    Commit,
    /// "Diff branch / PR…": `merge-base(base, head)...head` read from a
    /// repository's refs, no checkout. Never listed in the scope menu
    /// ([`Self::ALL`]); a pane is in it exactly while its chat's selected pill
    /// is a branch / PR target, and then has no working tree or turn scope.
    Range,
}

impl DiffScope {
    /// Scopes available from a Diff tab. History is selected from the surface
    /// picker instead, so it can keep its own tab and toolbar state.
    pub const ALL: [DiffScope; 3] = [Self::WorkingTree, Self::Branch, Self::LatestTurn];

    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "All changes in this worktree",
            Self::Branch => "Branch changes in this worktree",
            Self::LatestTurn => "Checkout changes since this chat’s latest turn",
            Self::History => "History",
            Self::Commit => "Commit",
            Self::Range => "Branch or PR changes",
        }
    }

    /// Wire value for `GetCheckoutDiff` `mode` (and parse-key discriminant).
    pub fn mode(self) -> &'static str {
        match self {
            Self::WorkingTree => "workingTree",
            Self::Branch => "branch",
            Self::LatestTurn => "turn",
            Self::History => "history",
            Self::Commit => "commit",
            Self::Range => "range",
        }
    }
}

/// Header-strip label per scope.
pub fn scope_label(scope: DiffScope, count: usize, base: Option<&str>) -> String {
    let files = if count == 1 { "file" } else { "files" };
    match scope {
        DiffScope::WorkingTree => uncommitted_label(count),
        DiffScope::Branch | DiffScope::Range => match base {
            Some(base) => format!("{count} Changed {files} vs {base}"),
            None => format!("{count} Changed {files}"),
        },
        DiffScope::LatestTurn => format!("{count} Changed {files} this turn"),
        DiffScope::History => "History".to_string(),
        DiffScope::Commit => format!("{count} Changed {files} in this commit"),
    }
}

/// The comparison ref the branch scope preselects. `branches` comes from
/// `ListBranches` with the repo's default branch first — but a repo with no
/// `origin/HEAD` falls back to the *checked-out* branch there, and comparing a
/// branch with itself is useless; prefer `main`/`master` in that case.
pub fn default_base_ref(branches: &[String], current: Option<&str>) -> Option<String> {
    let first = branches.first()?;
    if current != Some(first.as_str()) {
        return Some(first.clone());
    }
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    branches
        .iter()
        .find(|b| current != Some(b.as_str()))
        .or(Some(first))
        .cloned()
}

/// Empty-state copy per scope.
pub fn clean_message(scope: DiffScope, base: Option<&str>) -> String {
    match scope {
        DiffScope::WorkingTree => "No uncommitted changes".to_string(),
        DiffScope::Branch | DiffScope::Range => match base {
            Some(base) => format!("No changes vs {base}"),
            None => "No branch changes".to_string(),
        },
        DiffScope::LatestTurn => "No changes this turn".to_string(),
        DiffScope::History => "No commits found".to_string(),
        DiffScope::Commit => "Empty commit".to_string(),
    }
}

/// Fold a `WatchCheckoutDiffs` frame into the diff set. Accepts either a full
/// list (replace) or a single `CheckoutDiff` (upsert by checkout id) — the
/// contract streams `CheckoutDiff` items, but list frames cost nothing to
/// support. Returns whether anything changed.
pub fn apply_diff_frame(diffs: &mut Vec<CheckoutDiff>, value: serde_json::Value) -> bool {
    if let Ok(all) = serde_json::from_value::<Vec<CheckoutDiff>>(value.clone()) {
        if *diffs != all {
            *diffs = all;
            return true;
        }
        return false;
    }
    match serde_json::from_value::<CheckoutDiff>(value) {
        Ok(one) => {
            if let Some(existing) = diffs.iter_mut().find(|d| d.checkout_id == one.checkout_id) {
                if *existing == one {
                    return false;
                }
                *existing = one;
            } else {
                diffs.push(one);
            }
            true
        }
        Err(err) => {
            tracing::warn!(error = %err, "changes: dropping malformed diff frame");
            false
        }
    }
}

fn watch_checkout_params(
    target: Option<&str>,
    checkout_id: Option<&str>,
    cwd: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::Map::new();
    if let Some(target) = target {
        params.insert(
            "targetDeviceId".into(),
            serde_json::Value::String(target.to_string()),
        );
    }
    if let Some(checkout_id) = checkout_id {
        params.insert(
            "checkoutId".into(),
            serde_json::Value::String(checkout_id.to_string()),
        );
    }
    if let Some(cwd) = cwd {
        params.insert("cwd".into(), serde_json::Value::String(cwd.to_string()));
    }
    serde_json::Value::Object(params)
}

const MAX_EXCERPT_SOURCE_LINES: usize = 200_000;

fn excerpt_side(
    file: &FileDiff,
    side: SourceSide,
    language: Lang,
    path: &str,
) -> Option<Arc<mast_syntax::HighlightedDocument>> {
    let max_line = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| match side {
            SourceSide::Old => line.old_no,
            SourceSide::New => line.new_no,
        })
        .max()
        .unwrap_or(0) as usize;
    if max_line > MAX_EXCERPT_SOURCE_LINES {
        return None;
    }
    let mut lines = vec![Vec::new(); max_line];
    for hunk in &file.hunks {
        let visible = hunk
            .lines
            .iter()
            .filter_map(|line| {
                let number = match side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                }?;
                (line.kind != LineKind::Meta).then_some((number, line.text.as_str()))
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            continue;
        }
        let source = visible
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        let document = mast_syntax::highlight(mast_syntax::HighlightRequest {
            source: &source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()?;
        for ((number, _), spans) in visible.into_iter().zip(document.lines) {
            lines[number as usize - 1] = spans;
        }
    }
    Some(Arc::new(mast_syntax::HighlightedDocument {
        language,
        lines,
    }))
}

fn excerpt_highlights(file: &FileDiff, language: Lang) -> Option<DiffHighlights> {
    if !mast_syntax::supports_language(language) {
        return None;
    }
    let old = if file.status == FileStatus::Added {
        None
    } else {
        Some(excerpt_side(
            file,
            SourceSide::Old,
            language,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?)
    };
    let new = if file.status == FileStatus::Deleted {
        None
    } else {
        Some(excerpt_side(file, SourceSide::New, language, &file.path)?)
    };
    Some(DiffHighlights { old, new, hunk_symbols: Vec::new() })
}

fn sources_match_patch(file: &FileDiff, response: &mast_proto::CheckoutFileDiffText) -> bool {
    let old = response
        .old_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    let new = response
        .new_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    file.hunks.iter().flat_map(|hunk| &hunk.lines).all(|line| {
        let actual = match line.kind {
            LineKind::Del => line
                .old_no
                .and_then(|number| old.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Add => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Context => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied())
                .or_else(|| {
                    line.old_no
                        .and_then(|number| old.as_ref()?.get(number as usize - 1).copied())
                }),
            LineKind::Meta => return true,
        };
        actual == Some(line.text.as_str())
    })
}

fn full_highlights(
    file: &FileDiff,
    language: Lang,
    response: &mast_proto::CheckoutFileDiffText,
) -> Option<DiffHighlights> {
    if response.stale
        || response.binary
        || response.truncated
        || !sources_match_patch(file, response)
    {
        return None;
    }
    let parse = |source: &str, path: &str| {
        mast_syntax::highlight(mast_syntax::HighlightRequest {
            source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()
        .map(Arc::new)
    };
    let old = match response.old_text.as_deref() {
        Some(source) => Some(parse(
            source,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?),
        None => None,
    };
    let new = match response.new_text.as_deref() {
        Some(source) => Some(parse(source, &file.path)?),
        None => None,
    };
    if old.is_none() && new.is_none() && mast_syntax::supports_language(language) {
        return None;
    }
    let hunk_symbols = response.new_text.as_deref().map(|source| {
        let lines: Vec<_> = file.hunks.iter().map(|hunk| hunk.lines.iter()
            .find(|line| line.kind == LineKind::Add && line.new_no.is_some())
            .or_else(|| hunk.lines.iter().find(|line| line.new_no.is_some()))
            .and_then(|line| line.new_no).unwrap_or(1)).collect();
        mast_syntax::enclosing_symbols(source, &file.path, &lines)
    }).unwrap_or_default();
    Some(DiffHighlights { old, new, hunk_symbols })
}

fn file_diff_text_request(
    diff: &CheckoutDiff,
    origin: &FileOrigin,
    mode: String,
    base_ref: Option<String>,
    chat_id: Option<String>,
    commit_sha: Option<String>,
    head_ref: Option<String>,
) -> mast_proto::GetCheckoutFileDiffTextRequest {
    mast_proto::GetCheckoutFileDiffTextRequest {
        checkout_id: diff.checkout_id.clone(),
        cwd: diff.cwd.clone(),
        path: origin.repository_relative_path.clone(),
        repository_path: origin.repository_path.clone(),
        mode,
        base_ref,
        chat_id,
        commit_sha,
        head_ref,
        diff_checksum: diff.checksum.clone(),
    }
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

struct MeasuredDiffWidth {
    key: (u32, u32, u32, SharedString),
    // Retaining the Arc makes pointer identity safe against allocator reuse.
    highlights: Option<Arc<DiffHighlights>>,
    width: f32,
}

struct FileHorizontalState {
    geometry: DiffHorizontalGeometry,
    scroll: gpui::ScrollHandle,
    /// Shape once per file, typography/theme, and highlight revision.
    measured: std::cell::RefCell<Option<MeasuredDiffWidth>>,
}

impl FileHorizontalState {
    fn new(file: &FileDiff) -> Self {
        Self {
            geometry: DiffHorizontalGeometry::from_file(file),
            scroll: gpui::ScrollHandle::new(),
            measured: std::cell::RefCell::new(None),
        }
    }

    fn metrics(
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

struct ParsedDiff {
    /// `checkout_id:checksum` — identity of the parsed content.
    key: String,
    truncated: bool,
    additions: u32,
    deletions: u32,
    file_count: usize,
    /// Indexed like `files`; survives row virtualization and folding.
    horizontal: Vec<FileHorizontalState>,
    hunk_identities: Arc<Vec<Vec<String>>>,
    files: Arc<Vec<FileDiff>>,
    origins: Arc<Vec<FileOrigin>>,
    submodules: Arc<Vec<SubmoduleGroup>>,
    layout: Arc<Vec<DiffTreeNode>>,
}

// ---------------------------------------------------------------------------
// Row model — the diff flattened to line granularity (pure)
// ---------------------------------------------------------------------------

/// One virtualized list row. The diff is flattened so each visible LINE is
/// its own row (Zed's editor draws exactly the visible line range the same
/// way): scrolling a 10k-line file materializes ~50 line rows per frame, not
/// one 10k-line element, and a collapsed file contributes no body rows at
/// all. Nowrap heights use the analytic constants above; wrapped line rows
/// are measured by the list at the current pane width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    SubmoduleHeader {
        group: u32,
    },
    FileHeader {
        file: u32,
    },
    Notice {
        file: u32,
        notice: u32,
    },
    HunkHeader {
        file: u32,
        hunk: u32,
    },
    ContextControl { file: u32, gap: u32 },
    ContextLine { file: u32, gap: u32, line: u32 },
    Line {
        file: u32,
        hunk: u32,
        line: u32,
        /// Flat index across the file's hunks — keys into the highlight slot.
        flat: u32,
    },
    /// One split row: the two line indices [`split_pairs`] paired. Carrying
    /// them inline keeps the pairing off the render path — it is computed
    /// once, when the body is flattened.
    SplitLine {
        file: u32,
        hunk: u32,
        left: Option<u32>,
        right: Option<u32>,
    },
    /// `card` indexes the file's own staged-comment slice, in staged order.
    CommentCard {
        file: u32,
        card: u32,
    },
    CommentDraft {
        file: u32,
    },
    ReviewThread {
        file: u32,
        thread: u32,
        height: u32,
    },
    /// Trailing pad closing an expanded body ([`BODY_BOTTOM_PAD`]).
    BodyPad {
        file: u32,
    },
    /// A body mid-fold-tween: one height-animated, clipped row standing in
    /// for the whole body. Only the slice that can be revealed is built —
    /// the tween never pays for off-screen lines.
    FoldingBody {
        file: u32,
    },
}

impl DiffRow {
    fn file(self) -> Option<usize> {
        match self {
            Self::SubmoduleHeader { .. } => None,
            Self::FileHeader { file }
            | Self::Notice { file, .. }
            | Self::HunkHeader { file, .. }
            | Self::ContextControl { file, .. }
            | Self::ContextLine { file, .. }
            | Self::Line { file, .. }
            | Self::SplitLine { file, .. }
            | Self::CommentCard { file, .. }
            | Self::CommentDraft { file }
            | Self::ReviewThread { file, .. }
            | Self::BodyPad { file }
            | Self::FoldingBody { file } => Some(file as usize),
        }
    }

    /// `FoldingBody` is height-animated, so it reports 0 and never lands in a
    /// height sum.
    fn height(self, comments: &[ReviewComment], line_h: f32, card_height: &impl Fn(&ReviewComment) -> f32) -> f32 {
        match self {
            DiffRow::SubmoduleHeader { .. } => FILE_HEADER_HEIGHT,
            DiffRow::FileHeader { .. } => FILE_HEADER_HEIGHT,
            DiffRow::Notice { .. } => NOTICE_HEIGHT,
            DiffRow::HunkHeader { .. } => HUNK_HEADER_HEIGHT,
            DiffRow::ContextControl { .. } => HUNK_HEADER_HEIGHT,
            DiffRow::ContextLine { .. } => line_h,
            DiffRow::Line { .. } | DiffRow::SplitLine { .. } => line_h,
            DiffRow::CommentCard { card, .. } => comments
                .get(card as usize)
                .map(card_height)
                .unwrap_or(0.0),
            DiffRow::CommentDraft { .. } => comments::DRAFT_CARD_HEIGHT,
            DiffRow::ReviewThread { height, .. } => height as f32,
            DiffRow::BodyPad { .. } => BODY_BOTTOM_PAD,
            DiffRow::FoldingBody { .. } => 0.0,
        }
    }
}

/// Capacity hint only — comment cards are not counted. Split pairs can only
/// shrink the line count, so the unified count is a safe hint for both.
pub fn body_row_count(file: &FileDiff) -> usize {
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    file_notices(file).len() + file.hunks.len() + lines + 1
}

pub fn body_rows(
    file_ix: u32,
    file: &FileDiff,
    comments: &[ReviewComment],
    draft: Option<(CommentSide, u32)>,
    mode: DiffMode,
) -> Vec<DiffRow> {
    fn push_cards(
        rows: &mut Vec<DiffRow>,
        file_ix: u32,
        cards: &HashMap<(CommentSide, u32), Vec<u32>>,
        draft: Option<(CommentSide, u32)>,
        anchors: &[Option<(CommentSide, u32)>],
    ) {
        for anchor in anchors.iter().flatten() {
            if let Some(indices) = cards.get(anchor) {
                for &ix in indices {
                    rows.push(DiffRow::CommentCard {
                        file: file_ix,
                        card: ix,
                    });
                }
            }
            if draft == Some(*anchor) {
                rows.push(DiffRow::CommentDraft { file: file_ix });
            }
        }
    }

    let mut cards: HashMap<(CommentSide, u32), Vec<u32>> = HashMap::new();
    for (ix, comment) in comments.iter().enumerate() {
        if let Some(anchor) = comment.diff_anchor() {
            cards.entry(anchor).or_default().push(ix as u32);
        }
    }
    let mut rows = Vec::with_capacity(body_row_count(file));
    for notice in 0..file_notices(file).len() {
        rows.push(DiffRow::Notice {
            file: file_ix,
            notice: notice as u32,
        });
    }
    let mut hunk_flat = 0u32;
    for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::HunkHeader {
            file: file_ix,
            hunk: hunk_ix as u32,
        });
        match mode {
            DiffMode::Unified => {
                for (line_ix, line) in hunk.lines.iter().enumerate() {
                    rows.push(DiffRow::Line {
                        file: file_ix,
                        hunk: hunk_ix as u32,
                        line: line_ix as u32,
                        flat: hunk_flat + line_ix as u32,
                    });
                    push_cards(&mut rows, file_ix, &cards, draft, &[line_anchor(line)]);
                }
            }
            DiffMode::Split => {
                for (left, right) in split_pairs(&hunk.lines) {
                    rows.push(DiffRow::SplitLine {
                        file: file_ix,
                        hunk: hunk_ix as u32,
                        left,
                        right,
                    });
                    let anchors = pair_anchors(&hunk.lines, (left, right));
                    push_cards(&mut rows, file_ix, &cards, draft, &anchors);
                }
            }
        }
        hunk_flat += hunk.lines.len() as u32;
    }
    rows.push(DiffRow::BodyPad { file: file_ix });
    rows
}

/// Flatten all files into rows + each file's row span (header at
/// `range.start`, body rows after it). `collapsed(ix)` folds a file to just
/// its header. `comments` is the whole staged set; each file takes its own
/// path's slice.
pub fn flatten_rows(
    files: &[FileDiff],
    comments: &[ReviewComment],
    draft: Option<(&str, CommentSide, u32)>,
    mode: DiffMode,
    mut collapsed: impl FnMut(usize) -> bool,
) -> (Vec<DiffRow>, Vec<std::ops::Range<usize>>) {
    let mut rows = Vec::new();
    let mut ranges = Vec::with_capacity(files.len());
    for (ix, file) in files.iter().enumerate() {
        let start = rows.len();
        rows.push(DiffRow::FileHeader { file: ix as u32 });
        if !collapsed(ix) {
            let file_comments: Vec<ReviewComment> = comments
                .iter()
                .filter(|comment| !comment.is_file() && comment.path == file.path)
                .cloned()
                .collect();
            let file_draft = draft
                .filter(|(path, _, _)| *path == file.path)
                .map(|(_, side, line)| (side, line));
            rows.extend(body_rows(ix as u32, file, &file_comments, file_draft, mode));
        }
        ranges.push(start..rows.len());
    }
    (rows, ranges)
}

fn flatten_diff_tree(
    files: &[FileDiff],
    groups: &[SubmoduleGroup],
    layout: &[DiffTreeNode],
    comments: &[ReviewComment],
    draft: Option<(&str, CommentSide, u32)>,
    mode: DiffMode,
    mut file_collapsed: impl FnMut(usize) -> bool,
    mut group_collapsed: impl FnMut(usize) -> bool,
) -> (
    Vec<DiffRow>,
    Vec<Option<std::ops::Range<usize>>>,
    Vec<usize>,
) {
    struct FlattenContext<'a, FC, GC> {
        files: &'a [FileDiff],
        groups: &'a [SubmoduleGroup],
        comments: &'a [ReviewComment],
        draft: Option<(&'a str, CommentSide, u32)>,
        mode: DiffMode,
        file_collapsed: &'a mut FC,
        group_collapsed: &'a mut GC,
        rows: Vec<DiffRow>,
        ranges: Vec<Option<std::ops::Range<usize>>>,
        section_headers: Vec<usize>,
    }

    impl<FC: FnMut(usize) -> bool, GC: FnMut(usize) -> bool> FlattenContext<'_, FC, GC> {
        fn push_nodes(&mut self, nodes: &[DiffTreeNode]) {
            for node in nodes {
                match node {
                    DiffTreeNode::File(file_ix) => {
                        let file_ix = *file_ix as usize;
                        let Some(file) = self.files.get(file_ix) else {
                            continue;
                        };
                        let start = self.rows.len();
                        self.section_headers.push(start);
                        self.rows.push(DiffRow::FileHeader {
                            file: file_ix as u32,
                        });
                        if !(self.file_collapsed)(file_ix) {
                            let file_comments = self
                                .comments
                                .iter()
                                .filter(|comment| !comment.is_file() && comment.path == file.path)
                                .cloned()
                                .collect::<Vec<_>>();
                            let file_draft = self
                                .draft
                                .filter(|(path, _, _)| *path == file.path)
                                .map(|(_, side, line)| (side, line));
                            self.rows.extend(body_rows(
                                file_ix as u32,
                                file,
                                &file_comments,
                                file_draft,
                                self.mode,
                            ));
                        }
                        self.ranges[file_ix] = Some(start..self.rows.len());
                    }
                    DiffTreeNode::Submodule { group, children } => {
                        let group_ix = *group as usize;
                        if self.groups.get(group_ix).is_none() {
                            continue;
                        }
                        self.section_headers.push(self.rows.len());
                        self.rows.push(DiffRow::SubmoduleHeader { group: *group });
                        if !(self.group_collapsed)(group_ix) {
                            self.push_nodes(children);
                        }
                    }
                }
            }
        }
    }

    let mut context = FlattenContext {
        files,
        groups,
        comments,
        draft,
        mode,
        file_collapsed: &mut file_collapsed,
        group_collapsed: &mut group_collapsed,
        rows: Vec::new(),
        ranges: vec![None; files.len()],
        section_headers: Vec::new(),
    };
    context.push_nodes(layout);
    (context.rows, context.ranges, context.section_headers)
}

/// The file header that should remain visible for a logical list position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StickyFileHeader {
    file_ix: usize,
    header_row: usize,
    next_header_row: Option<usize>,
}

/// Resolve a sticky file header from the current flattened row ranges.
///
/// This remains independent of the rendered list so folds and diff resets
/// cannot leave a second, stale active-file state behind.
fn sticky_file_header(
    row_ranges: &[Option<std::ops::Range<usize>>],
    section_headers: &[usize],
    item_ix: usize,
    offset_in_item: f32,
) -> Option<StickyFileHeader> {
    let (file_ix, range) = row_ranges
        .iter()
        .enumerate()
        .filter_map(|(file_ix, range)| range.as_ref().map(|range| (file_ix, range)))
        .find(|(_, range)| range.contains(&item_ix))?;

    // A reset can briefly leave ListState pointing past the replacement
    // model. Treat that frame as having no sticky header.
    if item_ix == range.start && offset_in_item <= 0.0 {
        return None;
    }

    Some(StickyFileHeader {
        file_ix,
        header_row: range.start,
        next_header_row: section_headers
            .iter()
            .copied()
            .find(|header| *header > range.start),
    })
}

/// Offset a sticky header upward as the next file header enters its slot.
fn sticky_header_push_offset(next_header_y: Option<f32>) -> f32 {
    next_header_y
        .map(|y| (y - FILE_HEADER_HEIGHT).min(0.0))
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileHeaderPresentation {
    Row,
    Sticky,
}

impl FileHeaderPresentation {
    fn key_prefix(self) -> &'static str {
        match self {
            Self::Row => "file-hdr",
            Self::Sticky => "sticky-file-hdr",
        }
    }

    fn element_id(self, file_ix: usize) -> SharedString {
        let prefix = self.key_prefix();
        SharedString::from(format!("{prefix}-{file_ix}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct StickyFileHeaderPaint {
    rest_bg: gpui::Hsla,
    hover_bg: gpui::Hsla,
    border: gpui::Hsla,
    frost_tint: Option<gpui::Hsla>,
}

/// Resolve the sticky header from the diff's content plane, not the elevated
/// overlay plane used by menus and popovers.
fn sticky_file_header_paint(theme: &Theme) -> StickyFileHeaderPaint {
    if theme.is_frost() {
        let tint_alpha = match theme.appearance {
            crate::theme::Appearance::Dark => STICKY_FILE_HEADER_TINT_ALPHA_DARK,
            crate::theme::Appearance::Light => STICKY_FILE_HEADER_TINT_ALPHA_LIGHT,
        };
        StickyFileHeaderPaint {
            rest_bg: theme.ink(0.025),
            hover_bg: theme.glass_hover(),
            border: theme.border,
            frost_tint: Some(theme.bg.opacity(tint_alpha)),
        }
    } else {
        StickyFileHeaderPaint {
            rest_bg: crate::theme::flatten(theme.ink(0.025), theme.bg),
            hover_bg: crate::theme::flatten(theme.element_hover, theme.bg),
            border: theme.border,
            frost_tint: None,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct FileFold {
    collapsed: bool,
    /// Bumped per toggle — keys the height tween + chevron transition.
    epoch: usize,
    from: f32,
    to: f32,
    /// When the toggle happened: the tweens are armed only briefly after the
    /// click — gpui replays an element's animation on remount, and in the
    /// virtualized list a row scrolling back into view is a remount (the
    /// transcript's tool groups had the same flash; user report).
    toggled_at: Option<std::time::Instant>,
}

/// Tween arming window after a fold toggle (COLLAPSE's 180ms plus margin).
const FOLD_TWEEN_WINDOW: Duration = Duration::from_millis(400);

/// Ceiling on how much body a fold tween's stand-in row materializes. A
/// tween always starts from a clicked (on-screen) header, so the revealable
/// slice is at most one viewport tall — everything past this is clipped or
/// below the fold either way.
const FOLD_TWEEN_MAX_PX: f32 = 2400.0;

impl FileFold {
    fn animating(&self) -> bool {
        self.epoch > 0
            && self
                .toggled_at
                .is_some_and(|at| at.elapsed() < FOLD_TWEEN_WINDOW)
    }
}

struct HighlightSlot {
    fingerprint: u64,
    state: DiffHighlightState,
    _excerpt_task: Option<Task<()>>,
    _fetch_task: Option<Task<()>>,
}

enum DiffHighlightState {
    Pending,
    Ready(Arc<DiffHighlights>),
    Excerpt(Arc<DiffHighlights>),
    Plain,
}

/// The open base-ref dropdown — the same searchable-menu recipe as the
/// composer's ref picker and the spaces filter: a filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card's key handler),
/// ranked substring rows below.
struct RefMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

/// The "Diff checkout..." picker: a filter input over the engine's
/// checkout candidates (own checkout, submodules, worktrees), ticket matches
/// ranked first. Same keyboard shape as [`RefMenu`].
struct CheckoutMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    candidates: popover::Loadable<Vec<crate::diff_targets::RankedCandidate>>,
    _search_events: Subscription,
    _load: Task<()>,
}

/// The line the pointer is on. Only one element per anchor ever takes the
/// hover — the unified row, or a split row's right column — so the anchor
/// alone identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HoverRow {
    path: String,
    side: CommentSide,
    line: u32,
}

struct CommentDraft {
    editing_id: Option<String>,
    local: Option<review_drafts::LocalDraftEdit>,
    outdated: bool,
    /// Composer the note will stage onto, captured when the card opened. A
    /// draft belongs to the checkout it was written over, so it must not
    /// follow the user onto whatever chat is selected by commit time.
    key: String,
    path: String,
    /// The file's pre-rename path, when it moved — carried onto the comment so
    /// an `Old`-side citation names the file that line lives in.
    old_path: Option<String>,
    side: CommentSide,
    line: u32,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

#[derive(PartialEq, Eq)]
struct CommentSyncKey {
    revision: u64,
    owner: Option<String>,
    target: Option<String>,
    pinned: bool,
    editor: Option<(String, CommentSide, u32, Option<String>)>,
}

/// The Changes pane entity. Lazy: no RPC until [`Changes::ensure_watch`] runs
/// (the shell calls it when the pane first opens).
pub struct Changes {
    #[cfg(test)]
    benchmark_render_count: usize,
    #[cfg(test)]
    benchmark_highlight_count: usize,
    #[cfg(test)]
    benchmark_sync_count: usize,
    state: Entity<AppState>,
    /// Immutable owner identity plus the latest authoritative target for that
    /// owner. This is deliberately independent of AppState's global selection.
    owner: Option<ChangesTarget>,
    diffs: Vec<CheckoutDiff>,
    started: bool,
    error: Option<SharedString>,
    /// Device the running watch targets: `None` = the connected engine itself,
    /// `Some(id)` = a remote chat's host (relay-forwarded). The stream only
    /// carries the TARGET device's checkouts, so a selection change onto a
    /// chat hosted elsewhere tears the watch down and re-subscribes.
    watch_target: Option<String>,
    /// Device + expected checkout identity + cwd for the running stream.
    watch_for: Option<String>,
    watch_task: Option<Task<()>>,
    parsed: Option<ParsedDiff>,
    parse_task: Option<Task<()>>,
    folds: HashMap<String, FileFold>,
    /// UI disclosure state for successfully expanded committed submodules.
    /// Missing means open, matching ordinary files' expanded default.
    submodule_folds: HashMap<String, bool>,
    highlights: HashMap<String, HighlightSlot>,
    word_spans: word_diff::Cache,
    expanded_context: expand_context::Cache,
    /// The flattened row model the list virtualizes over (line granularity;
    /// collapsed bodies excluded) + each file's row span within it.
    rows: Vec<DiffRow>,
    file_filter: Entity<ComposerInput>,
    file_tree_collapsed: HashMap<String, std::collections::HashSet<String>>,
    file_tree_focus: FocusHandle,
    file_tree_focused_path: Option<String>,
    file_list_width: f32,
    file_list_resize_anchor: Option<(f32, f32)>,
    file_list_resize_hover: bool,
    diff_focus: FocusHandle,
    review_target: Option<String>,
    review_link_chat: Option<String>,
    review_link_status: Option<mast_engine::pr_ticket_cache::ChatLinkStatus>,
    review_pr: Option<String>,
    review_link_task: Option<Task<()>>,
    review_head_task: Option<Task<()>>,
    review_head_for: Option<String>,
    review_head: Option<(String, String)>,
    review_checkout_matches: Option<bool>,
    review_branch_repositories: HashMap<(String, String), Option<String>>,
    review_branch_lookup: Option<(String, String)>,
    review_branch_lookup_task: Option<Task<()>>,
    review_status_task: Option<Task<()>>,
    review_status_checked: Option<std::time::Instant>,
    review_status_base: Option<String>,
    review_closed: bool,
    review_state: Option<mast_proto::DiffReviewState>,
    review_walkthrough: bool,
    walkthrough_target: Option<mast_proto::ReviewTarget>,
    review_tour: bool,
    review_task: Option<Task<()>>,
    review_threads: Vec<mast_proto::ReviewThread>,
    collapsed_review_threads: std::collections::HashSet<String>,
    review_thread_index: review_drafts::ThreadIndex,
    unplaced_review_threads: Vec<usize>,
    review_threads_task: Option<Task<()>>,
    review_threads_error: Option<String>,
    local_review_drafts: HashMap<String, Vec<mast_proto::ReviewDraft>>,
    local_draft_comments: HashMap<String, Vec<ReviewComment>>,
    local_draft_ids: std::collections::HashSet<String>,
    local_outdated_drafts: Vec<mast_proto::ReviewDraft>,
    comment_index: HashMap<String, Vec<ReviewComment>>,
    comment_sync_key: Option<CommentSyncKey>,
    draft_anchor_index: review_drafts::DraftAnchorIndex,
    local_draft_pending: HashMap<String, Vec<mast_proto::ReviewDraft>>,
    local_draft_acknowledged: HashMap<String, Vec<mast_proto::ReviewDraft>>,
    local_draft_load_task: Option<Task<()>>,
    local_draft_save_task: Option<Task<()>>,
    local_draft_debounce_task: Option<Task<()>>,
    local_draft_save_ready: bool,
    local_draft_error: Option<String>,
    review_suggestions: HashMap<String, mast_proto::ReviewSuggestedComment>,
    suggestion_comments: Vec<ReviewComment>,
    suggestion_target: Option<String>,
    suggestion_head: Option<String>,
    suggestion_display_head: Option<Option<String>>,
    outdated_suggestions: Vec<(String, String)>,
    outdated_limit: usize,
    dismissed_suggestions: std::collections::HashSet<String>,
    suggestion_dispositions_target: Option<String>,
    suggestion_load_task: Option<Task<()>>,
    suggestion_accept_task: Option<Task<()>>,
    review_save_task: Option<Task<()>>,
    review_pending: HashMap<String, mast_proto::DiffReviewState>,
    review_acknowledged: HashMap<String, mast_proto::DiffReviewState>,
    review_error: Option<String>,
    seen_candidates: HashMap<String, review_state::SeenCandidate>,
    seen_hunks: std::collections::HashSet<String>,
    explanation_selection: Option<explanation::DiffSelection>,
    explanation_hunk: Option<(usize, usize)>,
    explanation_unified_rows: Option<(usize, usize, SourceSide)>,
    explanation_panel: Option<explanation::ExplanationPanel>,
    explanation_task: Option<Task<()>>,
    explanation_cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    explanation_shell: Option<gpui::WeakEntity<crate::shell::Shell>>,
    seen_task: Option<Task<()>>,
    seen_last_render: std::time::Instant,
    seen_scroll_epoch: u64,
    _file_filter_events: Subscription,
    row_ranges: Vec<Option<std::ops::Range<usize>>>,
    /// Ordered file and submodule headers; sticky file headers stop at either.
    section_headers: Vec<usize>,
    /// Sweeps [`DiffRow::FoldingBody`] stand-ins back to steady-state rows
    /// once their tween window elapses.
    fold_settle: Option<Task<()>>,
    list: ListState,
    /// What the pane diffs against (toolbar dropdown).
    scope: DiffScope,
    /// Unified or side-by-side (toolbar toggle, persisted per user).
    mode: DiffMode,
    /// Wrap long source lines instead of exposing the horizontal code plane.
    wrap_lines: bool,
    word_diff: bool,
    /// Comparison ref for [`DiffScope::Branch`] — preset to the repo's
    /// default branch once the branch list lands.
    base_ref: Option<String>,
    branches: Vec<String>,
    /// `device:cwd` the branch list was fetched for.
    branches_for: Option<String>,
    branches_task: Option<Task<()>>,
    /// One-shot scoped capture (Branch / Latest turn) + its fetch key.
    scoped: Option<CheckoutDiff>,
    scoped_for: Option<String>,
    scoped_error: Option<SharedString>,
    scoped_inflight: Option<String>,
    scoped_task: Option<Task<()>>,
    scope_menu: Popup<()>,
    ref_menu: Popup<RefMenu>,
    /// The "Diff checkout..." picker (chat diff targets).
    checkout_menu: Popup<CheckoutMenu>,
    /// The "Diff branch / PR…" picker (branch / PR range targets).
    ref_target_menu: Popup<ref_picker::RefTargetMenu>,
    /// Only ever one: a second `+` moves the card rather than stacking two
    /// half-written notes.
    draft: Option<CommentDraft>,
    hover: Option<HoverRow>,
    comment_key: u64,
    history: Option<Entity<GitHistory>>,
    history_count: Option<Entity<GitHistoryCount>>,
    history_search_control: Option<Entity<GitHistorySearchControl>>,
    history_fetch_button: Option<Entity<GitHistoryFetchButton>>,
    history_view_button: Option<Entity<GitHistoryViewButton>>,
    history_events: Option<Subscription>,
    /// Pinned commit for a [`DiffScope::Commit`] pane (sha + subject drive
    /// the fetch and the surface-tab title).
    commit: Option<GitHistoryCommit>,
    /// Checkout selected outside chat navigation (Repo Map close-out
    /// inspection). Pinned panes must not silently follow the selected chat.
    pinned_cwd: Option<String>,
    _observe: Subscription,
}

/// Events the host (the right pane's surface strip) listens for.
pub enum ChangesEvent {
    /// A History row was clicked — open this commit as its own diff tab.
    OpenCommit(GitHistoryCommit),
}

impl gpui::EventEmitter<ChangesEvent> for Changes {}

struct DiffHeaderTooltip(SharedString);

impl Render for DiffHeaderTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .when(true, |el| crate::components::changes_tooltip(el, theme))
            .shadow_md()
            .text_size(px(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

impl Changes {
    /// Test hook: mark file 0 as having loaded (empty) expanded context, so
    /// every row render takes the expanded-context width path.
    #[cfg(test)]
    pub(crate) fn test_mark_context_ready(&mut self) {
        let key = self.parsed.as_ref().unwrap().key.clone();
        self.expanded_context
            .entries
            .insert((key, 0), expand_context::Entry::ready(Vec::new(), Vec::new()));
    }

    pub(crate) fn diff_is_focused(&self, window: &Window, _cx: &App) -> bool {
        self.diff_focus.is_focused(window)
    }

    pub(crate) fn changed_paths(&self) -> Option<Vec<String>> {
        self.parsed.as_ref().map(|parsed| {
            parsed.files.iter().filter(|file| file.status != FileStatus::Deleted)
                .map(|file| file.path.clone()).collect()
        })
    }

    pub(crate) fn go_to_line(&mut self, target: u32, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else { return; };
        let top = self.list.logical_scroll_top().item_ix;
        let file_index = self.rows.get(top).and_then(|row| row.file()).unwrap_or(0);
        let Some(file) = parsed.files.get(file_index) else { return; };
        let path = file.path.clone();
        if self.folds.get(&path).is_some_and(|fold| fold.collapsed) {
            self.toggle_fold(file_index, cx);
            if let Some(fold) = self.folds.get_mut(&path) {
                fold.toggled_at = None;
            }
            self.settle_folds(cx);
        }
        let Some(parsed) = &self.parsed else { return; };
        let find = |new_side| self.rows.iter().position(|row| {
            let (file, hunk, line) = match *row {
                DiffRow::Line { file, hunk, line, .. } => (file, hunk, Some(line)),
                DiffRow::SplitLine { file, hunk, left, right } => (file, hunk, if new_side { right } else { left }),
                _ => return false,
            };
            if file as usize != file_index { return false; }
            line.is_some_and(|line| {
                let line = &parsed.files[file as usize].hunks[hunk as usize].lines[line as usize];
                if new_side { line.new_no == Some(target) } else { line.old_no == Some(target) }
            })
        });
        if let Some(index) = find(true).or_else(|| find(false)) {
            self.list.scroll_to_reveal_item(index);
            cx.notify();
        }
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let owner = state
            .read(cx)
            .selected_chat_row()
            .map(ChangesTarget::from_chat);
        Self::with_owner(state, owner, cx)
    }

    /// Create a normal Changes surface owned by `chat_id`. This is the shell's
    /// preferred constructor: ownership is captured before the tab is hidden
    /// and survives global chat selection changes.
    pub(crate) fn for_chat(
        state: Entity<AppState>,
        chat_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        // Opening refreshes a bound Chat's per-member branch/HEAD.
        state.update(cx, |state, cx| {
            state.refresh_chat_execution(&chat_id, true, cx)
        });
        let owner = state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map(|chat| ChangesTarget::for_chat_in(chat, state.read(cx)))
            .or_else(|| {
                Some(ChangesTarget {
                    unconfirmed: None,
                    exact_checkout: false,
                    range: None,
                    chat_id,
                    device_id: String::new(),
                    cwd: None,
                    checkout_id: None,
                    branch: None,
                })
            });
        let mut changes = Self::with_owner(state, owner, cx);
        changes.sync_scope_with_owner();
        changes
    }

    fn with_owner(
        state: Entity<AppState>,
        owner: Option<ChangesTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        let settings = crate::settings::current(cx);
        let mode = DiffMode::from_split(settings.diff_split);
        let list = ListState::new(0, ListAlignment::Top, px(1024.0));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |_, window, cx| {
            weak.update(cx, |this: &mut Changes, cx| {
                this.note_seen_scroll(window, cx);
            }).ok();
        });
        let file_filter = cx.new(|cx| ComposerInput::with_context("Filter files…", "PaletteSearch", cx));
        let file_filter_events = cx.subscribe(&file_filter, |_: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) { cx.notify(); }
        });
        Self {
            #[cfg(test)]
            benchmark_render_count: 0,
            #[cfg(test)]
            benchmark_highlight_count: 0,
            #[cfg(test)]
            benchmark_sync_count: 0,
            state,
            owner,
            mode,
            wrap_lines: settings.diff_wrap,
            word_diff: settings.diff_word,
            diffs: Vec::new(),
            started: false,
            error: None,
            watch_target: None,
            watch_for: None,
            watch_task: None,
            parsed: None,
            parse_task: None,
            folds: HashMap::new(),
            submodule_folds: HashMap::new(),
            highlights: HashMap::new(),
            word_spans: word_diff::Cache::default(),
            expanded_context: expand_context::Cache::default(),
            rows: Vec::new(),
            file_filter,
            file_tree_collapsed: HashMap::new(),
            file_tree_focus: cx.focus_handle(),
            file_tree_focused_path: None,
            file_list_width: settings.changes_file_list_width,
            file_list_resize_anchor: None,
            file_list_resize_hover: false,
            diff_focus: cx.focus_handle(),
            review_target: None,
            review_link_chat: None,
            review_link_status: None,
            review_pr: None,
            review_link_task: None,
            review_head_task: None,
            review_head_for: None,
            review_head: None,
            review_checkout_matches: None,
            review_branch_repositories: HashMap::new(),
            review_branch_lookup: None,
            review_branch_lookup_task: None,
            review_status_task: None,
            review_status_checked: None,
            review_status_base: None,
            review_closed: false,
            review_state: None,
            review_walkthrough: false,
            walkthrough_target: None,
            review_tour: false,
            review_task: None,
            review_threads: Vec::new(),
            collapsed_review_threads: std::collections::HashSet::new(),
            review_thread_index: review_drafts::ThreadIndex::default(),
            unplaced_review_threads: Vec::new(),
            review_threads_task: None,
            review_threads_error: None,
            local_review_drafts: HashMap::new(),
            local_draft_comments: HashMap::new(),
            local_draft_ids: std::collections::HashSet::new(),
            local_outdated_drafts: Vec::new(),
            comment_index: HashMap::new(),
            comment_sync_key: None,
            draft_anchor_index: review_drafts::DraftAnchorIndex::default(),
            local_draft_pending: HashMap::new(),
            local_draft_acknowledged: HashMap::new(),
            local_draft_load_task: None,
            local_draft_save_task: None,
            local_draft_debounce_task: None,
            local_draft_save_ready: false,
            local_draft_error: None,
            review_suggestions: HashMap::new(),
            suggestion_comments: Vec::new(),
            suggestion_target: None,
            suggestion_head: None,
            suggestion_display_head: None,
            outdated_suggestions: Vec::new(),
            outdated_limit: 20,
            dismissed_suggestions: std::collections::HashSet::new(),
            suggestion_dispositions_target: None,
            suggestion_load_task: None,
            suggestion_accept_task: None,
            review_save_task: None,
            review_pending: HashMap::new(),
            review_acknowledged: HashMap::new(),
            review_error: None,
            seen_candidates: HashMap::new(),
            seen_hunks: std::collections::HashSet::new(),
            explanation_selection: None,
            explanation_hunk: None,
            explanation_unified_rows: None,
            explanation_panel: None,
            explanation_task: None,
            explanation_cancelled: None,
            explanation_shell: None,
            seen_task: None,
            seen_last_render: std::time::Instant::now(),
            seen_scroll_epoch: 0,
            _file_filter_events: file_filter_events,
            row_ranges: Vec::new(),
            section_headers: Vec::new(),
            fold_settle: None,
            // Rows are single lines now — a deep overdraw is cheap and keeps
            // fast wheel flicks from outrunning measurement.
            list,
            scope: DiffScope::default(),
            base_ref: None,
            branches: Vec::new(),
            branches_for: None,
            branches_task: None,
            scoped: None,
            scoped_for: None,
            scoped_error: None,
            scoped_inflight: None,
            scoped_task: None,
            scope_menu: Popup::default(),
            ref_menu: Popup::default(),
            checkout_menu: Popup::default(),
            ref_target_menu: Popup::default(),
            draft: None,
            hover: None,
            comment_key: 0,
            history: None,
            history_count: None,
            history_search_control: None,
            history_fetch_button: None,
            history_view_button: None,
            history_events: None,
            commit: None,
            pinned_cwd: None,
            _observe: observe,
        }
    }

    /// A pane pinned to one commit's diff (a History row click) — fetches
    /// `parent vs commit` once and never offers the scope menu.
    pub fn for_commit(
        state: Entity<AppState>,
        commit: GitHistoryCommit,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = state
            .read(cx)
            .selected_chat_row()
            .map(ChangesTarget::from_chat);
        Self::for_commit_with_owner(state, commit, owner, None, cx)
    }

    pub(crate) fn for_commit_for_chat(
        state: Entity<AppState>,
        commit: GitHistoryCommit,
        chat_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map(ChangesTarget::from_chat);
        Self::for_commit_with_owner(state, commit, owner, None, cx)
    }

    /// A commit diff pinned to an explicit checkout. Repo Map uses this for
    /// unmatched worktrees and must not depend on the active chat's cwd.
    pub fn for_commit_at_cwd(
        state: Entity<AppState>,
        commit: GitHistoryCommit,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::for_commit_with_owner(state, commit, None, cwd, cx)
    }

    fn for_commit_with_owner(
        state: Entity<AppState>,
        commit: GitHistoryCommit,
        owner: Option<ChangesTarget>,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut changes = Self::with_owner(state, owner, cx);
        changes.scope = DiffScope::Commit;
        changes.commit = Some(commit);
        changes.pinned_cwd = cwd;
        changes
    }

    /// One-shot uncommitted diff for an explicit Repo Map checkout.
    pub fn for_checkout(state: Entity<AppState>, cwd: String, cx: &mut Context<Self>) -> Self {
        let mut changes = Self::with_owner(state, None, cx);
        changes.scope = DiffScope::WorkingTree;
        changes.pinned_cwd = Some(cwd);
        changes
    }

    /// A dedicated History surface. It shares the commit-opening event path
    /// with diffs, but is never offered as an item in a Diff tab's scope menu.
    pub fn for_history(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let owner = state
            .read(cx)
            .selected_chat_row()
            .map(ChangesTarget::from_chat);
        let mut changes = Self::with_owner(state, owner, cx);
        changes.scope = DiffScope::History;
        changes
    }

    pub(crate) fn history_for_chat(
        state: Entity<AppState>,
        chat_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut changes = Self::for_chat(state, chat_id, cx);
        changes.scope = DiffScope::History;
        changes
    }

    pub fn is_history(&self) -> bool {
        self.scope == DiffScope::History
    }

    pub(crate) fn owner_chat_id(&self) -> Option<&str> {
        self.owner.as_ref().map(|owner| owner.chat_id.as_str())
    }

    /// The surface-tab title (contextual, user request): the pinned commit's
    /// subject (short sha for subject-less commits), else the scope's label.
    pub fn tab_title(&self) -> gpui::SharedString {
        if let Some(commit) = &self.commit {
            let subject = commit.subject.trim();
            if !subject.is_empty() {
                return subject.to_string().into();
            }
            return commit.sha.chars().take(7).collect::<String>().into();
        }
        gpui::SharedString::from(self.scope.label())
    }

    /// Refresh only this surface's owning chat from the registry. A global
    /// selection change is intentionally irrelevant. Returns true when the
    /// authoritative worktree target changed.
    fn sync_owner(&mut self, cx: &App) -> bool {
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        let state = self.state.read(cx);
        let Some(chat) = state.chats.iter().find(|chat| chat.id == owner.chat_id) else {
            return false;
        };
        let next = ChangesTarget::for_chat_in(chat, state);
        if self.owner.as_ref() == Some(&next) {
            return false;
        }
        self.owner = Some(next);
        self.sync_scope_with_owner();
        self.diffs.clear();
        self.parsed = None;
        self.explanation_selection = None;
        self.explanation_unified_rows = None;
        self.cancel_explanation();
        self.rows.clear();
        self.row_ranges.clear();
        self.section_headers.clear();
        self.list.reset(0);
        self.branches_for = None;
        self.branches.clear();
        self.scoped = None;
        self.scoped_for = None;
        self.scoped_inflight = None;
        self.scoped_task = None;
        self.error = None;
        self.watch_task = None;
        self.started = false;
        self.watch_for = None;
        true
    }

    /// A pane is in [`DiffScope::Range`] exactly while its owner's selected
    /// pill is a branch / PR target. Leaving one restores the working tree
    /// scope; History and commit panes never change.
    fn sync_scope_with_owner(&mut self) {
        let ranged = self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.range.is_some());
        if ranged {
            if matches!(
                self.scope,
                DiffScope::WorkingTree | DiffScope::Branch | DiffScope::LatestTurn
            ) {
                self.scope = DiffScope::Range;
            }
        } else if self.scope == DiffScope::Range {
            self.scope = DiffScope::WorkingTree;
        }
    }

    fn owner_chat_id_owned(&self) -> Option<String> {
        self.owner.as_ref().map(|owner| owner.chat_id.clone())
    }

    fn owner_branch(&self) -> Option<String> {
        self.owner.as_ref().and_then(|owner| owner.branch.clone())
    }

    /// The owning chat's host device when it differs from the connected
    /// engine's own — diffs are produced where the checkout lives, so a
    /// remote chat's watch must relay-forward (`targetDeviceId`) to its host.
    /// Without this the local stream simply never carries the remote checkout
    /// and the pane sits on "Preparing diff…" forever (user report).
    fn desired_target(&self, cx: &App) -> Option<String> {
        if self.pinned_cwd.is_some() {
            return None;
        }
        let state = self.state.read(cx);
        let device = self.owner.as_ref()?.device_id.clone();
        if device.is_empty() {
            return None;
        }
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    /// Start the `WatchCheckoutDiffs` subscription (idempotent per target).
    /// Retries with a flat 2 s delay if the stream fails or ends; the last
    /// content stays visible under an error banner meanwhile.
    pub fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        if self.pinned_cwd.is_none()
            && self.owner.as_ref().is_none_or(|owner| {
                owner.device_id.is_empty()
                    || (owner.checkout_id.is_none() && owner.cwd.as_deref().is_none())
            })
        {
            return;
        }
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let cwd = self
            .pinned_cwd
            .clone()
            .or_else(|| self.owner.as_ref().and_then(|owner| owner.cwd.clone()));
        let watch_for = format!(
            "{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            checkout_id.as_deref().unwrap_or(""),
            cwd.as_deref().unwrap_or("")
        );
        if self.started && self.watch_for.as_deref() == Some(watch_for.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            // Engine still booting — retry on the next state change via sync().
            return;
        };
        // Retarget: the old task (and its stream) drop; rows from the previous
        // device would resolve against the wrong checkouts, so clear them.
        if self.started {
            self.diffs.clear();
            self.error = None;
        }
        self.started = true;
        self.watch_target = target.clone();
        self.watch_for = Some(watch_for);
        self.watch_task = Some(Self::spawn_watch(engine, target, checkout_id, cwd, cx));
    }

    fn spawn_watch(
        engine: EngineHandle,
        target: Option<String>,
        checkout_id: Option<String>,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut backoff = mast_harness::backoff::Backoff::new(
                Duration::from_secs(2),
                Duration::from_secs(30),
            );
            loop {
                let params = watch_checkout_params(
                    target.as_deref(),
                    checkout_id.as_deref(),
                    cwd.as_deref(),
                );
                let subscribed = engine
                    .client()
                    .subscribe_checked(methods::WATCH_CHECKOUT_DIFFS, params)
                    .await;
                match subscribed {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            backoff.reset();
                            let alive = this.update(cx, |changes, cx| {
                                changes.error = None;
                                if apply_diff_frame(&mut changes.diffs, value) {
                                    changes.sync(cx);
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        // Stream ended (engine restart / reconnect): banner + retry.
                        if this
                            .update(cx, |changes, cx| {
                                changes.error = Some("Diff stream interrupted — retrying".into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        if this
                            .update(cx, |changes, cx| {
                                changes.error =
                                    Some(format!("Diff watch unavailable: {err}").into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(backoff.next_delay()).await;
            }
        })
    }

    fn resolved(&self, _cx: &App) -> Option<&CheckoutDiff> {
        if self.pinned_cwd.is_some() {
            // A pinned watch is server-scoped by canonical cwd, so its frame
            // can contain at most the intended checkout. This snapshot drives
            // one-shot refreshes without consulting global chat selection.
            return self.diffs.first();
        }
        self.owner.as_ref()?.resolve(&self.diffs)
    }

    /// The checkout root the scoped RPCs address: the watch-resolved diff's
    /// canonical cwd when available, else the chat row's own.
    fn scoped_cwd(&self, cx: &App) -> Option<String> {
        if let Some(cwd) = &self.pinned_cwd {
            return Some(cwd.clone());
        }
        if let Some(diff) = self.resolved(cx) {
            return Some(diff.cwd.clone());
        }
        self.owner.as_ref()?.cwd.clone()
    }

    /// The diff the pane currently displays: the watch stream for the working
    /// tree, the one-shot scoped capture otherwise.
    fn active_diff(&self, cx: &App) -> Option<&CheckoutDiff> {
        match self.scope {
            DiffScope::WorkingTree if self.pinned_cwd.is_some() => self.scoped.as_ref(),
            DiffScope::WorkingTree => self.resolved(cx),
            DiffScope::Branch | DiffScope::LatestTurn | DiffScope::Commit | DiffScope::Range => {
                self.scoped.as_ref()
            }
            DiffScope::History => None,
        }
    }

    /// Scope discriminant folded into the parse key, so a scope or base
    /// switch re-parses even when checksums collide.
    fn scope_key(&self) -> String {
        match self.scope {
            DiffScope::WorkingTree => "wt".to_string(),
            DiffScope::Branch => format!("br:{}", self.base_ref.as_deref().unwrap_or("")),
            DiffScope::LatestTurn => "turn".to_string(),
            DiffScope::Range => match self.owner.as_ref().and_then(|o| o.range.as_ref()) {
                Some(range) => format!("range:{}:{}", range.base_ref, range.head_ref),
                None => "range".to_string(),
            },
            DiffScope::History => "history".to_string(),
            DiffScope::Commit => format!(
                "commit:{}",
                self.commit.as_ref().map(|c| c.sha.as_str()).unwrap_or("")
            ),
        }
    }

    fn parse_key(&self, diff: &CheckoutDiff) -> String {
        format!(
            "{}:{}:{}",
            diff.checkout_id,
            diff.checksum,
            self.scope_key()
        )
    }

    /// Fetch the branch list for the selected chat's checkout (idempotent per
    /// device+cwd); the repo's default branch (first entry) becomes the
    /// comparison base unless the user already picked one that still exists.
    fn ensure_branches(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let key = format!(
            "{}:{}:{}",
            target.as_deref().unwrap_or("local"),
            checkout_id.as_deref().unwrap_or(""),
            cwd
        );
        if self.branches_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.branches_for = Some(key.clone());
        self.branches_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("repoPath".into(), serde_json::Value::String(cwd));
            if let Some(checkout_id) = checkout_id {
                params.insert("checkoutId".into(), serde_json::Value::String(checkout_id));
            }
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(methods::LIST_BRANCHES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |changes, cx| {
                if changes.branches_for.as_deref() != Some(key.as_str()) {
                    return; // superseded by a chat/device switch
                }
                match result {
                    Ok(value) => {
                        changes.branches =
                            serde_json::from_value::<Vec<String>>(value).unwrap_or_default();
                        let keep = changes
                            .base_ref
                            .as_ref()
                            .is_some_and(|base| changes.branches.contains(base));
                        if !keep {
                            let current = changes.owner_branch();
                            changes.base_ref =
                                default_base_ref(&changes.branches, current.as_deref());
                        }
                        changes.sync(cx);
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "changes: branch list failed");
                        // Allow a retry on the next state change.
                        changes.branches_for = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keep the one-shot scoped capture fresh. The fetch key folds in the
    /// watch checksum, so any working-tree change (or commit — HEAD rides the
    /// checksum) re-captures; a context change (chat/scope/base) clears the
    /// stale content first so the pane shows the spinner, while a
    /// checksum-only refresh keeps the old diff visible until the new one
    /// lands.
    fn ensure_scoped(&mut self, cx: &mut Context<Self>) {
        if self.scope == DiffScope::History
            || (self.scope == DiffScope::WorkingTree && self.pinned_cwd.is_none())
        {
            self.scoped_inflight = None;
            self.scoped_task = None;
            return;
        }
        let chat_id = self.owner_chat_id_owned();
        if self.scope == DiffScope::LatestTurn && chat_id.is_none() {
            return;
        }
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let base = match self.scope {
            DiffScope::Branch => match &self.base_ref {
                Some(base) => Some(base.clone()),
                None => return, // branch list still loading
            },
            _ => None,
        };
        let commit_sha = match self.scope {
            DiffScope::Commit => match &self.commit {
                Some(commit) => Some(commit.sha.clone()),
                None => return, // a commit pane without its pin never fetches
            },
            _ => None,
        };
        // A range pane reads two refs of the repository checkout.
        let range = match self.scope {
            DiffScope::Range => match self.owner.as_ref().and_then(|o| o.range.clone()) {
                Some(range) => Some(range),
                None => return,
            },
            _ => None,
        };
        let target = self.desired_target(cx);
        let checkout_id = self
            .owner
            .as_ref()
            .and_then(|owner| owner.checkout_id.clone());
        let context = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            chat_id.as_deref().unwrap_or(""),
            checkout_id.as_deref().unwrap_or(""),
            cwd,
            self.scope.mode(),
            base.as_deref().unwrap_or(""),
            commit_sha.as_deref().unwrap_or("")
        );
        let context = match &range {
            Some(range) => format!(
                "{context}|{}|{}|{}",
                range.base_ref,
                range.head_ref,
                range.head_sha.as_deref().unwrap_or("")
            ),
            None => context,
        };
        // Refs only: the checkout's own working tree is irrelevant.
        let watch_sum = if range.is_some() {
            String::new()
        } else {
            self.resolved(cx).map(|d| d.checksum.clone()).unwrap_or_default()
        };
        let key = format!("{context}|{watch_sum}");
        if self.scoped_for.as_deref() == Some(key.as_str())
            || self.scoped_inflight.as_deref() == Some(key.as_str())
        {
            return;
        }
        if self
            .scoped_for
            .as_deref()
            .is_none_or(|prev| !prev.starts_with(&format!("{context}|")))
        {
            self.scoped = None;
            self.scoped_error = None;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mode = self.scope.mode();
        self.scoped_inflight = Some(key.clone());
        self.scoped_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("cwd".into(), serde_json::Value::String(cwd));
            if let Some(checkout_id) = checkout_id {
                params.insert("checkoutId".into(), serde_json::Value::String(checkout_id));
            }
            params.insert("mode".into(), serde_json::Value::String(mode.to_string()));
            if let Some(chat_id) = chat_id {
                params.insert("chatId".into(), serde_json::Value::String(chat_id));
            }
            if let Some(base) = base {
                params.insert("baseRef".into(), serde_json::Value::String(base));
            }
            if let Some(sha) = commit_sha {
                params.insert("commitSha".into(), serde_json::Value::String(sha));
            }
            if let Some(range) = range {
                params.insert("baseRef".into(), serde_json::Value::String(range.base_ref));
                params.insert("headRef".into(), serde_json::Value::String(range.head_ref));
            }
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(
                    methods::GET_CHECKOUT_DIFF,
                    serde_json::Value::Object(params),
                )
                .await;
            this.update(cx, |changes, cx| {
                if changes.scoped_inflight.as_deref() != Some(key.as_str()) {
                    return; // superseded
                }
                changes.scoped_inflight = None;
                match result.and_then(|value| {
                    serde_json::from_value::<CheckoutDiff>(value)
                        .map_err(|e| mast_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(diff) => {
                        changes.scoped = Some(diff);
                        changes.scoped_error = None;
                    }
                    Err(err) => {
                        changes.scoped = None;
                        changes.scoped_error = Some(err.to_string().into());
                    }
                }
                changes.scoped_for = Some(key);
                changes.sync(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn reset_horizontal_scroll(&self) {
        if let Some(parsed) = &self.parsed {
            for file in &parsed.horizontal {
                reset_horizontal_scroll(&file.scroll);
            }
        }
    }

    pub(crate) fn set_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.explanation_selection = None;
            self.explanation_unified_rows = None;
            self.cancel_explanation();
            self.reset_horizontal_scroll();
            if scope == DiffScope::History {
                self.history_pane(cx)
                    .update(cx, |history, cx| history.ensure_loaded(cx));
            }
            self.sync(cx);
        }
        cx.notify();
    }

    fn history_pane(&mut self, cx: &mut Context<Self>) -> Entity<GitHistory> {
        if let Some(history) = &self.history {
            return history.clone();
        }
        let state = self.state.clone();
        let owner_chat_id = self.owner_chat_id_owned();
        let history = cx.new(|cx| match owner_chat_id {
            Some(chat_id) => GitHistory::for_chat(state, chat_id, cx),
            None => GitHistory::new(state, cx),
        });
        self.history_events =
            Some(
                cx.subscribe(&history, |this: &mut Self, _, event, cx| match event {
                    GitHistoryEvent::OpenCommit(commit) => {
                        // Bubble to the host — the surface strip opens the tab.
                        cx.emit(ChangesEvent::OpenCommit(commit.clone()));
                    }
                    GitHistoryEvent::FetchSucceeded => {
                        // Remote refs affect branch choices and every scoped diff
                        // based on a ref. Force fresh reads after the engine has
                        // also kicked its checkout-status watcher.
                        this.branches_for = None;
                        this.scoped_for = None;
                        this.scoped_inflight = None;
                        this.scoped_task = None;
                        this.ensure_branches(cx);
                        if this.scope != DiffScope::History {
                            this.ensure_scoped(cx);
                        }
                        cx.notify();
                    }
                }),
            );
        self.history = Some(history.clone());
        history
    }

    fn history_count(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryCount> {
        if let Some(count) = &self.history_count {
            return count.clone();
        }
        let history = self.history_pane(cx);
        let count = cx.new(|cx| GitHistoryCount::new(history, cx));
        self.history_count = Some(count.clone());
        count
    }

    fn history_fetch_button(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryFetchButton> {
        if let Some(button) = &self.history_fetch_button {
            return button.clone();
        }
        let history = self.history_pane(cx);
        let button = cx.new(|cx| GitHistoryFetchButton::new(history, cx));
        self.history_fetch_button = Some(button.clone());
        button
    }

    fn history_search_control(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<GitHistorySearchControl> {
        if let Some(control) = &self.history_search_control {
            return control.clone();
        }
        let history = self.history_pane(cx);
        let control = cx.new(|cx| GitHistorySearchControl::new(history, cx));
        self.history_search_control = Some(control.clone());
        control
    }

    fn history_view_button(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryViewButton> {
        if let Some(button) = &self.history_view_button {
            return button.clone();
        }
        let history = self.history_pane(cx);
        let button = cx.new(|cx| GitHistoryViewButton::new(history, cx));
        self.history_view_button = Some(button.clone());
        button
    }

    fn set_base_ref(&mut self, base: String, cx: &mut Context<Self>) {
        if self.base_ref.as_deref() != Some(base.as_str()) {
            self.base_ref = Some(base);
            self.sync(cx);
        }
        cx.notify();
    }

    /// Everything the pane needs kicked when (re)shown: the watch plus the
    /// scope-specific loads (branches, scoped/commit capture, history) — the
    /// shell's hook for freshly-mounted surface tabs.
    pub fn ensure_content(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
    }

    /// Reconcile parsed content with the currently-active diff.
    fn sync(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.benchmark_sync_count += 1;
        }
        // A binding that arrives after the pane opened still gets its
        // per-member data (idempotent once fetched).
        if self.pinned_cwd.is_none()
            && self.commit.is_none()
            && let Some(chat_id) = self.owner_chat_id_owned()
        {
            self.state.update(cx, |state, cx| {
                state.refresh_chat_execution(&chat_id, false, cx)
            });
        }
        self.sync_owner(cx);
        self.discard_stale_draft(cx);
        // The watch follows the selected chat's host device (idempotent when
        // the target is unchanged); a boot-deferred attempt retries here too.
        if self.pinned_cwd.is_none() {
            self.ensure_watch(cx);
        }
        if self.scope == DiffScope::History {
            self.history_pane(cx)
                .update(cx, |history, cx| history.ensure_loaded(cx));
            return;
        }
        if !matches!(self.scope, DiffScope::Commit | DiffScope::Range) && self.pinned_cwd.is_none()
        {
            self.ensure_branches(cx);
        }
        self.ensure_scoped(cx);
        if self.active_diff(cx).is_none() {
            if self.parsed.take().is_some() {
                self.rows.clear();
                self.row_ranges.clear();
                self.section_headers.clear();
                self.list.reset(0);
                self.folds.clear();
                self.submodule_folds.clear();
                self.highlights.clear();
                cx.notify();
            }
            return;
        }
        self.ensure_review_state(cx);
        // Borrowed, not cloned: the patch can be megabytes and this runs on
        // every AppState notify.
        let Some(diff) = self.active_diff(cx) else {
            return;
        };
        let key = self.parse_key(diff);
        if self.parsed.as_ref().is_some_and(|p| p.key == key) {
            self.sync_comment_rows(cx);
            return;
        }
        // Parse off the render path — patches run to megabytes.
        let patch = diff.patch.clone();
        let truncated = diff.truncated;
        let additions = diff.additions;
        let deletions = diff.deletions;
        let file_count = diff.files.len();
        let submodules = diff.submodules.clone();
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let (files, origins, submodules, layout, identities) = cx
                .background_executor()
                .spawn(async move {
                    let (files, origins, submodules, layout) = parse_diff_tree(&patch, &submodules);
                    let identities = hunk_identities(&files);
                    (files, origins, submodules, layout, identities)
                })
                .await;
            this.update(cx, |changes, cx| {
                // Late results for a superseded diff are re-checked by key.
                let current = changes.active_diff(cx).map(|d| changes.parse_key(d));
                if current.as_deref() != Some(key.as_str()) {
                    return;
                }
                let logical_count = logical_file_count(&layout);
                let file_count = if logical_count > 0 {
                    logical_count
                } else if file_count > 0 {
                    file_count
                } else {
                    files.len()
                };
                let horizontal = files.iter().map(FileHorizontalState::new).collect();
                changes.folds.clear();
                changes.seen_candidates.clear();
                for file in &files {
                    if is_generated_file(&file.path) || is_large_file(file) {
                        changes.folds.insert(
                            file.path.clone(),
                            FileFold {
                                collapsed: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                changes.submodule_folds.clear();
                changes.highlights.clear();
                let staged = changes.staged_comments(cx);
                let draft = changes.draft_anchor();
                let (rows, mut ranges, mut section_headers) = flatten_diff_tree(
                    &files,
                    &submodules,
                    &layout,
                    &staged,
                    draft
                        .as_ref()
                        .map(|(path, side, line)| (path.as_str(), *side, *line)),
                    changes.mode,
                    |index| {
                        changes
                            .folds
                            .get(&files[index].path)
                            .is_some_and(|fold| fold.collapsed)
                    },
                    |_| false,
                );
                let (rows, indices) = expand_context::add_rows(rows, &files, &key, &changes.expanded_context);
                for range in ranges.iter_mut().flatten() { *range = indices[range.start]..indices[range.end]; }
                for index in &mut section_headers { *index = indices[*index]; }
                changes.comment_key = comment_state_key(&staged, draft.as_ref());
                // The uniform hint keeps offsets for never-rendered rows
                // sane (most rows ARE lines); real heights land as rows
                // render.
                let row_height = px(diff_line_height(Theme::of(cx)));
                changes
                    .list
                    .reset_with_uniform_height(rows.len(), row_height);
                changes.rows = rows;
                changes.row_ranges = ranges;
                changes.section_headers = section_headers;
                changes.parsed = Some(ParsedDiff {
                    key,
                    truncated,
                    additions,
                    deletions,
                    file_count,
                    horizontal,
                    hunk_identities: Arc::new(identities),
                    files: Arc::new(files),
                    origins: Arc::new(origins),
                    submodules: Arc::new(submodules),
                    layout: Arc::new(layout),
                });
                changes.rebuild_draft_index(cx);
                changes.ensure_review_state(cx);
                changes.refresh_thread_rows(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Swap one file's body rows (everything after its header) for
    /// `new_body`, splicing both the row model and the list state. gpui's
    /// `splice` shifts the logical scroll anchor by the count delta, so
    /// content below the fold stays put.
    fn expand_context(&mut self, file_ix: u32, gap: u32, all: bool, cx: &mut Context<Self>) {
        let Some(parsed) = self.parsed.as_ref() else { return; };
        let key = (parsed.key.clone(), file_ix);
        if self.expanded_context.entries.contains_key(&key) {
            if self.expanded_context.expand(&key, gap as usize, all) { self.reflatten(cx); }
            return;
        }
        let Some(file) = parsed.files.get(file_ix as usize).cloned() else { return; };
        let Some(origin) = parsed.origins.get(file_ix as usize).cloned() else { return; };
        let (Some(diff), Some(engine)) = (self.active_diff(cx).cloned(), self.state.read(cx).engine().cloned()) else { return; };
        let mode = self.scope.mode().to_string();
        let (base_ref, head_ref) = match (self.scope, self.owner.as_ref().and_then(|o| o.range.as_ref())) {
            (DiffScope::Range, Some(range)) => (Some(range.base_ref.clone()), Some(range.head_ref.clone())),
            _ => (self.base_ref.clone(), None),
        };
        let commit = (self.scope == DiffScope::Commit).then(|| self.commit.as_ref().map(|c| c.sha.clone())).flatten();
        let request = file_diff_text_request(&diff, &origin, mode, base_ref, self.owner_chat_id_owned(), commit, head_ref);
        let target = self.desired_target(cx);
        if !self.expanded_context.begin(key.clone()) { return; }
        self.reflatten(cx);
        cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default();
            if let Some(target) = target { params.insert("targetDeviceId".into(), serde_json::Value::String(target)); }
            let response = engine.client().call(methods::GET_CHECKOUT_FILE_DIFF_TEXT, serde_json::Value::Object(params)).await.ok()
                .and_then(|v| serde_json::from_value::<mast_proto::CheckoutFileDiffText>(v).ok());
            let entry = cx.background_executor().spawn(async move {
                let response = response?;
                if response.binary || response.truncated || response.stale { return None; }
                let gaps = expand_context::gaps(&file, response.new_text.as_deref()?)?;
                let mut shown = vec![0; gaps.len()];
                if let (Some(data), Some(count)) = (gaps.get(gap as usize), shown.get_mut(gap as usize)) { *count = if all { data.lines.len() } else { data.lines.len().min(20) }; }
                Some(expand_context::Entry::ready(gaps, shown))
            }).await;
            this.update(cx, |this, cx| {
                let Some(expand_context::Entry::Loading) = this.expanded_context.entries.remove(&key) else { return; };
                this.expanded_context.entries.insert(key, entry.unwrap_or(expand_context::Entry::Failed));
                this.reflatten(cx);
            }).ok();
        }).detach();
    }


}

impl Render for Changes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        { self.benchmark_render_count += 1; }
        if self.scope == DiffScope::History {
            let history = self.history_pane(cx);
            history.update(cx, |history, cx| history.ensure_loaded(cx));
            let theme = Theme::of(cx).clone();
            return div()
                .size_full()
                .flex()
                .flex_col()
                .children(self.render_context_chrome(&theme, cx))
                .children(self.render_execution_selector(&theme, cx))
                .children(self.render_diff_target_error(&theme, cx))
                .child(history)
                .into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let active = self.active_diff(cx);
        let scope = self.scope;
        let base = self.base_ref.clone();
        // With no session selected (new-chat canvas) there is nothing to
        // prepare — show the quiet empty state, not an endless spinner.
        let no_chat = self.owner.is_none() && self.pinned_cwd.is_none();
        let pinned = self.pinned_cwd.is_some();
        let phase = pane_diff_phase(no_chat, pinned, active);
        let error = self.error.clone().map(|message| {
            if active.is_some() {
                SharedString::from(format!(
                    "{message} — showing the last snapshot for this worktree"
                ))
            } else {
                message
            }
        });
        // Scoped fetch failures replace the content area. "no turn recorded"
        // is the expected pre-first-turn state, not an error; "unknown
        // method" is version skew — the chat's host engine predates
        // GetCheckoutDiff (a still-running daemon after an app update, or a
        // remote device behind on releases) — say that instead of leaking
        // the raw RPC error (user report).
        let scoped_notice: Option<(SharedString, bool)> = shows_scoped_error(no_chat, pinned, scope)
            .then(|| self.scoped_error.clone())
            .flatten()
            .map(|message| {
                if message.contains("no turn recorded") {
                    (
                        SharedString::from("No turn recorded yet — send a message first"),
                        false,
                    )
                } else if message.contains("unknown method") {
                    (
                        SharedString::from(
                            "This chat's device is running an older Mast — update it to view branch and turn diffs",
                        ),
                        false,
                    )
                } else {
                    (message, true)
                }
            });

        let unconfirmed = self
            .owner
            .as_ref()
            .filter(|_| !pinned)
            .and_then(|owner| owner.unconfirmed.clone());
        let content: AnyElement = if let Some(reason) = unconfirmed {
            div()
                .debug_selector(|| "changes-member-unconfirmed".into())
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .px(px(Theme::SPACE_LG))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(
                    "No changes shown: Mast could not confirm this repository's checkout.",
                ))
                .child(SharedString::from(reason))
                .into_any_element()
        } else if let Some((message, warn)) = scoped_notice {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(Theme::SPACE_LG))
                .text_size(px(12.0))
                .text_color(if warn {
                    theme.warning.opacity(0.85)
                } else {
                    theme.text_faint
                })
                .child(message)
                .into_any_element()
        } else {
            match phase {
                DiffPhase::Preparing => div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(crate::loaders::gradient_spinner(
                        "changes-preparing",
                        &theme,
                        3.0,
                        cx.entity_id(),
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from("Preparing diff…")),
                    )
                    .into_any_element(),
                DiffPhase::Clean => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(clean_message(scope, base.as_deref())))
                    .into_any_element(),
                DiffPhase::List => {
                    if self.parsed.is_some() {
                        self.ensure_seen_monitor(window, cx);
                        let sticky_header = self.render_sticky_file_header(&theme, cx);
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .children(self.render_header_strip(&theme))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_1()
                                    .min_h_0()
                                    .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<FileListResize>, _, cx| {
                                        if let Some((start, width)) = this.file_list_resize_anchor {
                                            let width = (width + f32::from(event.event.position.x) - start).clamp(160.0, 600.0);
                                            this.file_list_width = width;
                                            crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |settings| settings.changes_file_list_width = width);
                                            cx.notify();
                                        }
                                    }))
                                    .children((!self.review_tour).then(|| self.render_file_list(&theme, window, cx)))
                                    .child(
                                        div()
                                            .track_focus(&self.diff_focus)
                                            .key_context("ChangesDiff")
                                            .on_action(cx.listener(|this, _: &explanation::ExplainDiff, _, cx| this.explain_focused_hunk(cx)))
                                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                                                this.diff_key_down(event, window, cx);
                                            }))
                                            .relative()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .child(
                                                list(self.list.clone(), cx.processor(Self::render_row))
                                                    .size_full()
                                                    .with_sizing_behavior(gpui::ListSizingBehavior::Auto),
                                            )
                                            .when_some(sticky_header, |el, header| el.child(header)),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        // Diff known, parse still running.
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::loaders::gradient_spinner(
                                "changes-parsing",
                                &theme,
                                3.0,
                                cx.entity_id(),
                                cx,
                            ))
                            .into_any_element()
                    }
                }
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            // Changes is a code-adjacent surface: chrome stays Geist while
            // paths, hunks, gutters, and source runs keep their mono overrides.
            .font_family(theme.font_sans_fixed.clone())
            .children(self.render_context_chrome(&theme, cx))
            .children(self.render_execution_selector(&theme, cx))
            .children(self.render_diff_target_error(&theme, cx))
            .children(self.render_outdated_drafts(&theme, cx))
            .when_some(self.local_draft_error.clone(), |el, message| {
                el.child(div().flex().items_center().gap(px(8.0))
                    .child(crate::error_notice::ErrorNotice::from_text("local-review-draft-error", &theme, &message))
                    .child(div().id("local-review-draft-retry").cursor_pointer().text_color(theme.text_muted)
                        .on_click(cx.listener(|this, _, _, cx| this.retry_local_drafts(cx))).child("Retry")))
            })
            .when_some(self.review_threads_error.clone(), |el, message| {
                el.child(crate::error_notice::ErrorNotice::from_text(
                    "changes-review-threads-error", &theme, &format!("Review threads: {message}"),
                ))
            })
            .when_some(self.review_error.clone(), |el, message| {
                el.child(div().flex().items_center().gap(px(8.0))
                    .child(crate::error_notice::ErrorNotice::from_text("changes-review-state-error", &theme, &message))
                    .child(div().id("changes-review-state-retry").cursor_pointer().text_color(theme.text_muted)
                        .when(true, |el| crate::components::review_text_action(el, &theme))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_review_state(cx)))
                        .child("Retry")))
            })
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_MD))
                        .py(px(4.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(11.0))
                        .text_color(theme.warning)
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }
}

#[cfg(feature = "snapshot")]
pub(crate) fn snapshot_populated_changes(cx: &mut App) -> Entity<Changes> {
    let state = cx.new(|_| AppState::new());
    cx.new(|cx| {
        let mut changes = Changes::new(state, cx);
        let patch = "diff --git a/src/hooks/decorated/main.rs b/src/hooks/decorated/main.rs\n--- a/src/hooks/decorated/main.rs\n+++ b/src/hooks/decorated/main.rs\n@@ -3,3 +3,3 @@\n fn main() {\n-    old();\n+    new();\n }\ndiff --git a/src/model.rs b/src/model.rs\n--- a/src/model.rs\n+++ b/src/model.rs\n@@ -1 +1 @@\n-old model\n+new model\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old text\n+new text\n";
        let diff = CheckoutDiff {
            checkout_id: "snapshot".into(),
            device_id: "local".into(),
            cwd: "/snapshot".into(),
            patch: patch.into(),
            files: Vec::new(),
            submodules: Vec::new(),
            additions: 4,
            deletions: 3,
            truncated: false,
            checksum: "snapshot".into(),
            updated_at: chrono::Utc::now(),
        };
        changes.pinned_cwd = Some("/snapshot".into());
        changes.scoped_for = Some("local|||/snapshot|workingTree|||".into());
        let key = changes.parse_key(&diff);
        let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
        let identities = hunk_identities(&files);
        if let Some(mut highlights) = excerpt_highlights(&files[0], Lang::Rust) {
            highlights.hunk_symbols = mast_syntax::enclosing_symbols("// configuration\n// entry point\nfn main() {\n    new();\n}\n", &files[0].path, &[4]);
            changes.highlights.insert(files[0].path.clone(), HighlightSlot {
                fingerprint: hash64(&[&key, &files[0].path]),
                state: DiffHighlightState::Ready(Arc::new(highlights)),
                _excerpt_task: None, _fetch_task: None,
            });
        }
        let (rows, mut ranges, mut headers) = flatten_diff_tree(
            &files, &submodules, &layout, &[], None, changes.mode, |_| false, |_| false,
        );
        changes.word_spans.finish((key.clone(), 0, 0), Arc::new(word_diff::spans(&files[0].hunks[0].lines)));
        if let Some(gaps) = expand_context::gaps(&files[0], "// configuration\n// entry point\nfn main() {\n    new();\n}\n// end\n") {
            changes.expanded_context.entries.insert((key.clone(), 0), expand_context::Entry::ready(gaps, vec![0, 0]));
            changes.expanded_context.order.push((key.clone(), 0));
        }
        let (rows, indices) = expand_context::add_rows(rows, &files, &key, &changes.expanded_context);
        for range in ranges.iter_mut().flatten() { *range = indices[range.start]..indices[range.end]; }
        for index in &mut headers { *index = indices[*index]; }
        changes.list.reset_with_uniform_height(
            rows.len(), px(diff_line_height(Theme::of(cx))),
        );
        changes.rows = rows;
        changes.row_ranges = ranges;
        changes.section_headers = headers;
        changes.parsed = Some(ParsedDiff {
            key,
            truncated: false,
            additions: 4,
            deletions: 3,
            file_count: files.len(),
            horizontal: files.iter().map(FileHorizontalState::new).collect(),
            hunk_identities: Arc::new(identities),
            files: Arc::new(files),
            origins: Arc::new(origins),
            submodules: Arc::new(submodules),
            layout: Arc::new(layout),
        });
        changes.scoped = Some(diff);
        changes
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "perf/diff_bench.rs"]
mod perf_bench;
