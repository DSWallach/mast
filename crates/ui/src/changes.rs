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
mod expand_context;
mod file_list;
mod explanation;
mod ref_picker;
mod review_state;
mod review_drafts;
mod word_diff;
pub(crate) mod walkthrough;

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

/// Measure the same runs the row paints. Color boundaries can break kerning
/// and ligatures on native platforms even when every run uses the same font.
fn max_shaped_text_width(
    file: &FileDiff,
    highlights: Option<&DiffHighlights>,
    theme: &Theme,
    text_system: &gpui::WindowTextSystem,
) -> f32 {
    let mono = font(theme.font_mono.clone());
    let size = px(diff_text_size(theme));
    let column_width = text_system
        .ch_advance(text_system.resolve_font(&mono), size)
        .unwrap_or(size * 0.6)
        .as_f32();
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
    body_rows(0, file, comments, draft, mode)
        .iter()
        .map(|row| row.height(comments, line_h))
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
    Some(DiffHighlights { old, new })
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
    Some(DiffHighlights { old, new })
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
    fn height(self, comments: &[ReviewComment], line_h: f32) -> f32 {
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
                .map(|comment| comments::card_height(&comment.body))
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
    fn replace_file_body(&mut self, file_ix: usize, new_body: Vec<DiffRow>) {
        let new_body = if let Some(parsed) = &self.parsed {
            expand_context::add_rows(new_body, &parsed.files, &parsed.key, &self.expanded_context).0
        } else { new_body };
        let new_body = self.add_thread_rows(file_ix, new_body);
        if self.explanation_selection.take().is_some() {
            self.explanation_unified_rows = None;
            self.explanation_hunk = None;
            self.cancel_explanation();
        }
        self.seen_candidates.clear();
        let Some(range) = self.row_ranges.get(file_ix).and_then(Clone::clone) else {
            return;
        };
        let body = range.start + 1..range.end;
        let delta = new_body.len() as isize - body.len() as isize;
        // Only splice the rows that moved: `ListState::splice` clamps the
        // scroll anchor to the range start when the anchored row is inside it,
        // so replacing a whole body jumped the pane to the top of the file.
        let (prefix, suffix) = {
            let old = &self.rows[body.clone()];
            let prefix = old
                .iter()
                .zip(&new_body)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new_body[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            (prefix, suffix)
        };
        if delta == 0 && prefix + suffix >= body.len() {
            return;
        }
        let changed = body.start + prefix..body.end - suffix;
        let mid: Vec<DiffRow> = new_body[prefix..new_body.len() - suffix].to_vec();
        self.list.splice(changed.clone(), mid.len());
        self.rows.splice(changed, mid);
        self.row_ranges[file_ix] = Some(range.start..(range.end as isize + delta) as usize);
        for (ix, range) in self.row_ranges.iter_mut().enumerate() {
            if ix == file_ix {
                continue;
            }
            let Some(range) = range else {
                continue;
            };
            if range.start >= body.end {
                *range =
                    (range.start as isize + delta) as usize..(range.end as isize + delta) as usize;
            }
        }
        for header in &mut self.section_headers {
            if *header >= body.end {
                *header = (*header as isize + delta) as usize;
            }
        }
    }

    fn toggle_fold(&mut self, file_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(file) = parsed.files.get(file_ix) else {
            return;
        };
        if self.wrap_lines {
            let collapsed = !self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed);
            let body = if collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapsed;
            fold.toggled_at = None;
            self.replace_file_body(file_ix, body);
            cx.notify();
            return;
        }
        let expanded_height = body_height_with(
            file,
            &self.comments_for(&file.path, cx),
            self.draft_anchor_in(&file.path),
            self.mode,
            diff_line_height(Theme::of(cx)),
        );
        let fold = self.folds.entry(file.path.clone()).or_default();
        let currently_collapsed = fold.collapsed;
        fold.from = if currently_collapsed {
            0.0
        } else {
            expanded_height
        };
        fold.to = if currently_collapsed {
            expanded_height
        } else {
            0.0
        };
        fold.collapsed = !currently_collapsed;
        fold.epoch += 1;
        fold.toggled_at = Some(std::time::Instant::now());
        // The body tweens as ONE clipped stand-in row; the settle sweep
        // swaps it for steady rows (all lines, or none) once the window
        // elapses.
        self.replace_file_body(
            file_ix,
            vec![DiffRow::FoldingBody {
                file: file_ix as u32,
            }],
        );
        self.ensure_fold_settle(cx);
    }

    fn toggle_submodule(&mut self, group_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(group) = parsed.submodules.get(group_ix) else {
            return;
        };
        let collapsed = !self
            .submodule_folds
            .get(&group.repository_path)
            .copied()
            .unwrap_or(false);
        self.submodule_folds
            .insert(group.repository_path.clone(), collapsed);
        self.reflatten(cx);
    }

    /// Keep a sweep alive while any [`DiffRow::FoldingBody`] stand-ins
    /// remain; each tick settles the ones whose tween window has elapsed.
    fn ensure_fold_settle(&mut self, cx: &mut Context<Self>) {
        if self.fold_settle.is_some() {
            return;
        }
        self.fold_settle = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLD_TWEEN_WINDOW).await;
                let more = this
                    .update(cx, |changes, cx| changes.settle_folds(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
            this.update(cx, |changes, _| changes.fold_settle = None)
                .ok();
        }));
    }

    /// Replace every settled folding stand-in with its steady-state rows.
    /// Returns whether any stand-ins are still mid-tween.
    fn settle_folds(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let files = parsed.files.clone();
        let mut pending = false;
        for file_ix in (0..self.row_ranges.len()).rev() {
            let Some(range) = &self.row_ranges[file_ix] else {
                continue;
            };
            let folding = self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                });
            if !folding {
                continue;
            }
            let Some(file) = files.get(file_ix) else {
                continue;
            };
            let fold = self.folds.get(&file.path).copied().unwrap_or_default();
            if fold.animating() {
                pending = true;
                continue;
            }
            let body = if fold.collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
        pending
    }

    /// Every parsed file currently folded shut?
    fn all_collapsed(&self) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let visible_files = self
            .row_ranges
            .iter()
            .enumerate()
            .filter_map(|(ix, range)| range.as_ref().map(|_| ix))
            .collect::<Vec<_>>();
        let files_collapsed = visible_files.is_empty()
            || visible_files.into_iter().all(|ix| {
                parsed.files.get(ix).is_some_and(|file| {
                    self.folds
                        .get(&file.path)
                        .is_some_and(|fold| fold.collapsed)
                })
            });
        let groups_collapsed = parsed.submodules.is_empty()
            || parsed.submodules.iter().all(|group| {
                self.submodule_folds
                    .get(&group.repository_path)
                    .copied()
                    .unwrap_or(false)
            });
        files_collapsed && groups_collapsed
    }

    /// Collapse every file section, or expand them all when everything is
    /// already shut (the toolbar's fold button, t3code parity). Steady-state
    /// writes — no per-row tween arming, the whole list just snaps. List
    /// splices run bottom-up over the OLD ranges (each is O(log n)), then
    /// the row model rebuilds wholesale; the scroll anchor rides the
    /// splices, landing on the nearest file header when its body vanishes.
    fn toggle_collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let collapse = !self.all_collapsed();
        let files = parsed.files.clone();
        let groups = parsed.submodules.clone();
        for file in files.iter() {
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapse;
            fold.toggled_at = None;
        }
        for group in groups.iter() {
            self.submodule_folds
                .insert(group.repository_path.clone(), collapse);
        }
        if !groups.is_empty() {
            self.reflatten(cx);
            return;
        }
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let Some(range) = &self.row_ranges[file_ix] else {
                continue;
            };
            let body = range.start + 1..range.end;
            let new_len = if collapse {
                0
            } else {
                let file = &files[file_ix];
                let comments: Vec<ReviewComment> = staged
                    .iter()
                    .filter(|comment| !comment.is_file() && comment.path == file.path)
                    .cloned()
                    .collect();
                let body = body_rows(
                    file_ix as u32,
                    file,
                    &comments,
                    self.draft_anchor_in(&file.path),
                    self.mode,
                );
                expand_context::add_rows(body, &files, &parsed.key, &self.expanded_context).0.len()
            };
            if body.len() != new_len {
                self.list.splice(body, new_len);
            }
        }
        let (rows, mut ranges) = flatten_rows(
            &files,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |_| collapse,
        );
        let (rows, indices) = expand_context::add_rows(rows, &files, &parsed.key, &self.expanded_context);
        for range in &mut ranges { *range = indices[range.start]..indices[range.end]; }
        self.rows = rows;
        self.row_ranges = ranges.into_iter().map(Some).collect();
        self.section_headers = self
            .row_ranges
            .iter()
            .filter_map(|range| range.as_ref().map(|range| range.start))
            .collect();
        cx.notify();
    }

    /// Swap unified ⇄ split (toolbar toggle). The parse is untouched — only
    /// the flattening changes — so this rebuilds the row model and re-anchors
    /// the scroll onto whichever file was under the viewport's top edge (row
    /// indices do not survive the re-pairing).
    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = self.mode.toggled();
        self.reset_horizontal_scroll();
        let split = self.mode.is_split();
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_split = split;
        });
        // A draft's `+` sits in a column that may not exist after the swap.
        self.hover = None;
        self.reflatten(cx);
    }

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap_lines = !self.wrap_lines;
        self.reset_horizontal_scroll();
        let wrap = self.wrap_lines;
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_wrap = wrap;
        });

        // A folding stand-in has an analytic fixed-line height. Settle it
        // before switching to variable-height rows; steady rows can then be
        // measured by the virtual list at the current pane width.
        let folding = self
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::FoldingBody { .. }));
        if folding {
            self.fold_settle = None;
            for fold in self.folds.values_mut() {
                fold.toggled_at = None;
            }
            self.reflatten(cx);
        } else {
            self.list.remeasure();
            cx.notify();
        }
    }

    fn reflatten(&mut self, cx: &mut Context<Self>) {
        self.seen_candidates.clear();
        if self.explanation_selection.take().is_some() {
            self.explanation_unified_rows = None;
            self.explanation_hunk = None;
            self.cancel_explanation();
        }
        let Some(parsed) = &self.parsed else {
            cx.notify();
            return;
        };
        let files = parsed.files.clone();
        let groups = parsed.submodules.clone();
        let layout = parsed.layout.clone();
        let top = self.list.logical_scroll_top().item_ix;
        let anchor_row = self.rows.get(top).copied();
        let anchor_file = self
            .row_ranges
            .iter()
            .position(|range| range.as_ref().is_some_and(|range| range.contains(&top)));
        let anchor_group = self.rows.get(top).and_then(|row| match row {
            DiffRow::SubmoduleHeader { group } => Some(*group as usize),
            _ => None,
        });
        let collapsed: Vec<bool> = files
            .iter()
            .map(|file| {
                self.folds
                    .get(&file.path)
                    .is_some_and(|fold| fold.collapsed)
            })
            .collect();
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let group_collapsed = groups
            .iter()
            .map(|group| {
                self.submodule_folds
                    .get(&group.repository_path)
                    .copied()
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        let (rows, mut ranges, mut section_headers) = flatten_diff_tree(
            &files,
            &groups,
            &layout,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |ix| collapsed.get(ix).copied().unwrap_or(false),
            |ix| group_collapsed.get(ix).copied().unwrap_or(false),
        );
        let (rows, indices) = expand_context::add_rows(rows, &files, &parsed.key, &self.expanded_context);
        for range in ranges.iter_mut().flatten() { *range = indices[range.start]..indices[range.end]; }
        for index in &mut section_headers { *index = indices[*index]; }
        let row_height = px(diff_line_height(Theme::of(cx)));
        self.list.reset_with_uniform_height(rows.len(), row_height);
        self.rows = rows;
        self.row_ranges = ranges;
        self.section_headers = section_headers;
        self.refresh_thread_rows(cx);
        let anchor = anchor_row.and_then(|row| self.rows.iter().position(|candidate| *candidate == row)).or_else(|| anchor_file
            .and_then(|ix| self.row_ranges.get(ix))
            .and_then(|range| range.as_ref())
            .map(|range| range.start))
            .or_else(|| {
                anchor_group.and_then(|group| {
                    self.rows.iter().position(|row| {
                        *row == DiffRow::SubmoduleHeader {
                            group: group as u32,
                        }
                    })
                })
            });
        if let Some(start) = anchor {
            self.list.scroll_to_reveal_item(start);
        }
        cx.notify();
    }

    /// Cloned because rendering borrows `self` mutably a moment later.
    fn staged_comments(&self, cx: &App) -> Vec<ReviewComment> {
        // Explicit-checkout panes are read-only inspection surfaces. They may
        // represent an unmatched worktree, so the selected chat's composer
        // key is unrelated and must never leak comments into this diff.
        if self.pinned_cwd.is_some() {
            return Vec::new();
        }
        let key = self
            .owner
            .as_ref()
            .map(|owner| owner.chat_id.as_str())
            .unwrap_or("");
        let state = self.state.read(cx);
        let mut comments: Vec<_> = state
            .review_comments(key)
            .iter()
            .filter(|comment| {
                self.draft
                    .as_ref()
                    .and_then(|draft| draft.editing_id.as_ref())
                    != Some(&comment.id)
            })
            .cloned()
            .collect();
        if let Some(local) = self.review_target.as_ref().and_then(|target| self.local_draft_comments.get(target)) {
            comments.extend(local.iter().filter(|comment| self.draft.as_ref().and_then(|draft| draft.editing_id.as_ref()) != Some(&comment.id)).cloned());
        }
        if self.suggestion_target.as_ref() == self.review_target.as_ref() {
            comments.extend(self.suggestion_comments.iter().cloned());
        }
        comments
    }

    fn comments_for(&self, path: &str, _cx: &App) -> Vec<ReviewComment> {
        self.comment_index.get(path).cloned().unwrap_or_default()
    }

    /// The parsed diff's pre-rename path for `path`, when the file moved.
    fn old_path_of(&self, path: &str) -> Option<String> {
        self.parsed
            .as_ref()?
            .files
            .iter()
            .find(|file| file.path == path)?
            .old_path
            .clone()
    }

    /// A draft belongs to the checkout it was opened over. Chat navigation
    /// swaps both the diff under it and the composer it would stage onto, so
    /// the half-written note is dropped rather than following the user across.
    fn discard_stale_draft(&mut self, cx: &mut Context<Self>) {
        let key = self
            .owner
            .as_ref()
            .map(|owner| owner.chat_id.clone())
            .unwrap_or_default();
        if self.draft.as_ref().is_some_and(|draft| draft.key != key) {
            if self.draft.as_ref().is_some_and(|draft| draft.local.is_some()) { self.commit_draft(cx); }
            else { self.draft = None; }
            self.sync_comment_rows(cx);
            cx.notify();
        }
    }

    fn draft_anchor(&self) -> Option<(String, CommentSide, u32)> {
        self.draft
            .as_ref()
            .filter(|draft| !draft.outdated)
            .map(|draft| (draft.path.clone(), draft.side, draft.line))
    }

    fn draft_anchor_in(&self, path: &str) -> Option<(CommentSide, u32)> {
        self.draft
            .as_ref()
            .filter(|draft| !draft.outdated && draft.path == path)
            .map(|draft| (draft.side, draft.line))
    }

    fn sync_comment_rows(&mut self, cx: &mut Context<Self>) {
        if self.parsed.is_none() {
            return;
        }
        let sync_key = CommentSyncKey {
            revision: self.state.read(cx).review_comments_revision,
            owner: self.owner_chat_id_owned(), target: self.review_target.clone(), pinned: self.pinned_cwd.is_some(),
            editor: self.draft.as_ref().map(|draft| (draft.path.clone(), draft.side, draft.line, draft.editing_id.clone())),
        };
        if self.comment_sync_key.as_ref() == Some(&sync_key) { return; }
        self.comment_sync_key = Some(sync_key);
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let key = comment_state_key(&staged, draft.as_ref());
        if key == self.comment_key {
            return;
        }
        self.comment_key = key;
        self.comment_index.clear();
        for comment in &staged {
            if !comment.is_file() { self.comment_index.entry(comment.path.clone()).or_default().push(comment.clone()); }
        }
        let Some(parsed) = &self.parsed else {
            return;
        };
        let files = parsed.files.clone();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let Some(range) = self.row_ranges[file_ix].clone() else {
                continue;
            };
            let file = &files[file_ix];
            // A mid-tween stand-in is the settle sweep's to replace.
            if self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed)
            {
                continue;
            }
            if self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                })
            {
                continue;
            }
            let comments = self.comment_index.get(&file.path).cloned().unwrap_or_default();
            let body = body_rows(
                file_ix as u32,
                file,
                &comments,
                self.draft_anchor_in(&file.path),
                self.mode,
            );
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
    }

    fn set_hover(
        &mut self,
        path: &str,
        anchor: Option<(CommentSide, u32)>,
        cx: &mut Context<Self>,
    ) {
        if self.pinned_cwd.is_some() {
            if self.hover.take().is_some() {
                cx.notify();
            }
            return;
        }
        let next = anchor.map(|(side, line)| HoverRow {
            path: path.to_string(),
            side,
            line,
        });
        if next != self.hover {
            self.hover = next;
            cx.notify();
        }
    }

    fn hovering(&self, path: &str, anchor: (CommentSide, u32)) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|hover| hover.path == path && (hover.side, hover.line) == anchor)
    }

    fn clear_hover_at(&mut self, path: &str, anchor: (CommentSide, u32), cx: &mut Context<Self>) {
        if self.hovering(path, anchor) {
            self.hover = None;
            cx.notify();
        }
    }

    fn open_draft(
        &mut self,
        path: String,
        side: CommentSide,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_draft_internal(path, side, line, true, window, cx);
    }

    fn open_draft_internal(
        &mut self, path: String, side: CommentSide, line: u32, local_allowed: bool,
        window: &mut Window, cx: &mut Context<Self>,
    ) {
        if self.pinned_cwd.is_some() {
            return;
        }
        if self.draft.is_some() { self.commit_draft(cx); }
        let local = if local_allowed && matches!(self.scope, DiffScope::Branch | DiffScope::Range) {
            if let Some(target) = self.review_target.as_deref() {
                let Some(draft) = self.new_local_draft(target, &path, side, line) else {
                    self.local_draft_error = Some("The review draft is still loading or this line is no longer in the diff.".into());
                    cx.notify();
                    return;
                };
                Some(review_drafts::LocalDraftEdit { draft, original: None })
            } else { None }
        } else { None };
        let input = cx.new(|cx| ComposerInput::new("Request a change…", cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.commit_draft(cx),
            ComposerInputEvent::Edited => this.local_draft_edited(cx),
            _ => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        let key = self.owner_chat_id_owned().unwrap_or_default();
        let old_path = self.old_path_of(&path);
        self.draft = Some(CommentDraft {
            editing_id: None,
            local,
            outdated: false,
            key,
            path,
            old_path,
            side,
            line,
            input,
            _events: events,
        });
        window.focus(&handle, cx);
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn edit_comment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let local = self.review_target.as_ref().and_then(|target| self.local_review_drafts.get(target))
            .and_then(|drafts| drafts.iter().find(|draft| draft.id == id)).cloned();
        let Some(comment) = self.staged_comments(cx)
            .iter()
            .find(|comment| comment.id == id && !comment.is_file())
            .cloned()
        else {
            return;
        };
        let Some((side, line)) = comment.diff_anchor() else {
            return;
        };
        self.open_draft_internal(comment.path.clone(), side, line, local.is_some(), window, cx);
        let Some(draft) = self.draft.as_mut() else { return; };
        draft.editing_id = Some(comment.id);
        draft.local = local.map(|draft| review_drafts::LocalDraftEdit { original: Some(draft.clone()), draft });
        if let comments::CommentSource::Diff { old_path, .. } = comment.source {
            draft.old_path = old_path;
        }
        draft
            .input
            .update(cx, |input, cx| input.set_text(comment.body, cx));
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.take() {
            if let Some(local) = draft.local { self.cancel_local_edit(local, cx); }
        }
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn commit_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let body = draft.input.read(cx).text().trim().to_string();
        if let Some(mut local) = draft.local {
            if body.is_empty() { self.cancel_local_edit(local, cx); }
            else { local.draft.body = body; self.put_local_draft(local.draft, true, cx); self.rebuild_draft_index(cx); }
            self.sync_comment_rows(cx);
            cx.notify();
            return;
        }
        if body.is_empty() {
            self.sync_comment_rows(cx);
            cx.notify();
            return;
        }

        // `draft.key`, not the live one: the note stages onto the composer it
        // was written against even if the selection moved under it.
        let key = draft.key;
        self.state.update(cx, |state, cx| {
            if let Some(id) = draft.editing_id {
                state.update_review_comment_body(&key, &id, body);
            } else {
                let comment = ReviewComment::new(draft.path, draft.side, draft.line, body)
                    .renamed_from(draft.old_path);
                state.add_review_comment(&key, comment);
            }
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn remove_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.review_target.clone().filter(|target| self.local_review_drafts.get(target).is_some_and(|drafts| drafts.iter().any(|draft| draft.id == id))) {
            self.remove_local_draft(&target, id, cx);
            return;
        }
        let key = self.owner_chat_id_owned().unwrap_or_default();
        self.state.update(cx, |state, cx| {
            state.remove_review_comment(&key, id);
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn close_scope_menu(&mut self, cx: &mut Context<Self>) {
        if self.scope_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.scope_menu);
        }
    }

    fn close_ref_menu(&mut self, cx: &mut Context<Self>) {
        if self.ref_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.ref_menu);
        }
    }

    fn close_checkout_menu(&mut self, cx: &mut Context<Self>) {
        if self.checkout_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.checkout_menu);
        }
    }

    /// Handle Escape before focused descendants such as a terminal receive it.
    /// A popup in its exit animation remains a blocker until it unmounts.
    pub(crate) fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.ref_target_menu.is_open() {
            self.close_ref_target_menu(cx);
            return true;
        }
        if self.checkout_menu.is_open() {
            self.close_checkout_menu(cx);
            return true;
        }
        if self.ref_menu.is_open() {
            self.close_ref_menu(cx);
            return true;
        }
        if self.scope_menu.is_open() {
            self.close_scope_menu(cx);
            return true;
        }
        self.scope_menu.get().is_some()
            || self.ref_menu.get().is_some()
            || self.checkout_menu.get().is_some()
            || self.ref_target_menu.get().is_some()
    }

    fn open_ref_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search branches…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.ref_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        // The highlight starts ON the current base (query is empty, so the
        // filtered rows are just the branch list).
        let active = self
            .base_ref
            .as_ref()
            .and_then(|base| self.branches.iter().position(|b| b == base))
            .unwrap_or(0);
        self.ref_menu.open(RefMenu {
            search,
            active,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            _search_events: search_events,
        });
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered branch indices for the open ref menu (ranked substring match).
    fn ref_menu_rows(&self, cx: &App) -> Vec<usize> {
        let query = self
            .ref_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        popover::filter_indices(&query, &self.branches)
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ picks the highlighted branch, Esc closes.
    fn ref_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.ref_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_ref_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.ref_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.ref_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.ref_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self
                    .ref_menu_rows(cx)
                    .get(active)
                    .and_then(|ix| self.branches.get(*ix).cloned());
                if let Some(branch) = pick {
                    self.set_base_ref(branch, cx);
                    self.close_ref_menu(cx);
                }
            }
            _ => {}
        }
    }

    /// Whether the "Diff checkout..." button shows: a chat hosted on this
    /// device with a working directory. Never for a commit view or a pinned
    /// checkout (no chat target to change).
    fn checkout_picker_available(&self, cx: &App) -> bool {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return false;
        }
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        let state = self.state.read(cx);
        state.local_device_id.as_deref() == Some(owner.device_id.as_str())
            && state
                .chats
                .iter()
                .find(|chat| chat.id == owner.chat_id)
                .is_some_and(|chat| chat.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()))
    }

    fn open_checkout_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else {
            return;
        };
        let ticket = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(crate::diff_targets::chat_ticket_id);
        let search = cx
            .new(|cx| ComposerInput::with_context("Search checkouts\u{2026}", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.checkout_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        let list = self
            .state
            .update(cx, |state, cx| state.list_chat_diff_checkouts(&chat_id, cx));
        let load = cx.spawn(async move |this, cx| {
            let result = list.await;
            this.update(cx, |this, cx| {
                if let Some(menu) = this.checkout_menu.open_mut() {
                    menu.candidates = match result {
                        Ok(candidates) => popover::Loadable::Ready(
                            crate::diff_targets::rank_candidates(candidates, ticket.as_deref()),
                        ),
                        Err(message) => popover::Loadable::Error(message),
                    };
                    menu.active = 0;
                }
                cx.notify();
            })
            .ok();
        });
        self.checkout_menu.open(CheckoutMenu {
            search,
            active: 0,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            candidates: popover::Loadable::Loading,
            _search_events: search_events,
            _load: load,
        });
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered candidate indices for the open picker (ranked substring match
    /// over the label, kind, and path; ticket matches stay ahead when the
    /// query is empty).
    fn checkout_menu_rows(&self, cx: &App) -> Vec<usize> {
        let Some(menu) = self.checkout_menu.get() else {
            return Vec::new();
        };
        let Some(rows) = menu.candidates.ready() else {
            return Vec::new();
        };
        let query = menu.search.read(cx).text().to_string();
        let haystacks: Vec<String> = rows
            .iter()
            .map(|row| format!("{} {} {}", row.label(), row.kind_tag(), row.candidate.path))
            .collect();
        popover::filter_indices(&query, &haystacks)
    }

    fn checkout_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if !self.checkout_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_checkout_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.checkout_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.checkout_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.checkout_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self.checkout_menu_rows(cx).get(active).and_then(|ix| {
                    self.checkout_menu
                        .get()?
                        .candidates
                        .ready()?
                        .get(*ix)
                        .map(|row| row.candidate.clone())
                });
                if let Some(candidate) = pick {
                    self.choose_checkout(candidate, cx);
                }
            }
            _ => {}
        }
    }

    /// Persist the picked candidate as the chat's diff target: the own
    /// checkout clears the selection, anything else selects its path. The
    /// session's pill pick is dropped first so the persisted selection (the
    /// next `WatchChatDiffTargets` frame) is what shows; a failed write
    /// surfaces as the pane's error notice (`AppState::chat_diff_target_error`).
    fn choose_checkout(
        &mut self,
        candidate: mast_proto::DiffCheckoutCandidate,
        cx: &mut Context<Self>,
    ) {
        use mast_proto::{DiffCheckoutKind, SetChatDiffTargetAction};
        if let Some(chat_id) = self.owner_chat_id_owned() {
            self.state.update(cx, |state, cx| {
                state.clear_chat_diff_member_pick(&chat_id);
                let (action, path) = match candidate.kind {
                    DiffCheckoutKind::Own => {
                        // Beside targets the own checkout is a pill: keep it
                        // picked so a bound Chat does not fall back to its
                        // primary member.
                        let own = crate::work_outline::execution::own_member_id(&chat_id);
                        if state
                            .chat_execution_members(&chat_id)
                            .iter()
                            .any(|member| member.repo_id == own)
                        {
                            state.pick_chat_execution_member_locally(&chat_id, &own);
                        }
                        (SetChatDiffTargetAction::ClearSelection, None)
                    }
                    DiffCheckoutKind::Submodule | DiffCheckoutKind::Worktree => {
                        (SetChatDiffTargetAction::Select, Some(candidate.path))
                    }
                };
                state
                    .set_chat_diff_target(&chat_id, action, path, cx)
                    .detach();
                cx.notify();
            });
        }
        self.close_checkout_menu(cx);
    }

    fn remove_checkout(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else { return };
        let task = self.state.update(cx, |state, cx| {
            state.set_chat_diff_target(&chat_id, mast_proto::SetChatDiffTargetAction::Unbind, Some(path.clone()), cx)
        });
        cx.spawn(async move |this, cx| {
            if task.await.is_ok() {
                let _ = this.update(cx, |this, cx| {
                    if let Some(menu) = this.checkout_menu.open_mut() {
                        if let popover::Loadable::Ready(rows) = &mut menu.candidates {
                            rows.retain(|row| row.candidate.path != path);
                        }
                        menu.active = 0;
                    }
                    cx.notify();
                });
            }
        }).detach();
    }

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

    fn request_word_spans(&mut self, key: word_diff::Key, hunk: &Hunk, cx: &mut Context<Self>) -> Option<word_diff::HunkSpans> {
        if !self.word_diff { return None; }
        if let Some(spans) = self.word_spans.get(&key) { return Some(spans); }
        if !self.word_spans.begin(&key) { return None; }
        let lines = if hunk.lines.len() <= word_diff::MAX_HUNK_LINES
            && hunk.lines.iter().map(|line| line.text.len()).sum::<usize>() <= 32_000
        { hunk.lines.clone() } else { Vec::new() };
        cx.spawn(async move |this, cx| {
            let spans = cx.background_executor().spawn(async move { Arc::new(word_diff::spans(&lines)) }).await;
            this.update(cx, |this, cx| { this.word_spans.finish(key, spans); cx.notify(); }).ok();
        }).detach();
        None
    }

    /// Start excerpt parsing and a lazy full-source fetch for an expanded file.
    fn request_highlight(
        &mut self,
        file: &FileDiff,
        origin: &FileOrigin,
        parsed_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<Arc<DiffHighlights>> {
        let lang = mast_syntax::language_for_path(&file.path)?;
        let fingerprint = hash64(&[parsed_key, &file.path]);
        if let Some(slot) = self.highlights.get(&file.path)
            && slot.fingerprint == fingerprint
        {
            return match &slot.state {
                DiffHighlightState::Ready(highlights) | DiffHighlightState::Excerpt(highlights) => {
                    Some(highlights.clone())
                }
                DiffHighlightState::Pending | DiffHighlightState::Plain => None,
            };
        }
        if !mast_syntax::supports_language(lang) {
            self.highlights.insert(
                file.path.clone(),
                HighlightSlot {
                    fingerprint,
                    state: DiffHighlightState::Plain,
                    _excerpt_task: None,
                    _fetch_task: None,
                },
            );
            return None;
        }
        #[cfg(test)]
        { self.benchmark_highlight_count += 1; }
        let path = file.path.clone();
        let fetch_origin = origin.clone();
        let excerpt_file = file.clone();
        let excerpt_path = path.clone();
        let excerpt_task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { excerpt_highlights(&excerpt_file, lang).map(Arc::new) })
                .await;
            this.update(cx, |changes, cx| {
                if let Some(slot) = changes.highlights.get_mut(&excerpt_path)
                    && slot.fingerprint == fingerprint
                    && matches!(slot.state, DiffHighlightState::Pending)
                {
                    slot.state = match highlights {
                        Some(highlights) => DiffHighlightState::Excerpt(highlights),
                        None => DiffHighlightState::Plain,
                    };
                    cx.notify();
                }
            })
            .ok();
        });

        let active = self.active_diff(cx).cloned();
        let engine = self.state.read(cx).engine().cloned();
        let target = self.desired_target(cx);
        let chat_id = self.owner_chat_id_owned();
        let mode = self.scope.mode().to_string();
        let (base_ref, head_ref) = match (
            self.scope,
            self.owner.as_ref().and_then(|o| o.range.as_ref()),
        ) {
            (DiffScope::Range, Some(range)) => {
                (Some(range.base_ref.clone()), Some(range.head_ref.clone()))
            }
            _ => (self.base_ref.clone(), None),
        };
        let commit_sha = (self.scope == DiffScope::Commit)
            .then(|| self.commit.as_ref().map(|commit| commit.sha.clone()))
            .flatten();
        let fetch_file = file.clone();
        let fetch_path = path.clone();
        let fetch_task = match (active, engine) {
            (Some(diff), Some(engine)) => Some(cx.spawn(async move |this, cx| {
                let request = file_diff_text_request(
                    &diff,
                    &fetch_origin,
                    mode,
                    base_ref,
                    chat_id,
                    commit_sha,
                    head_ref,
                );
                let mut params = serde_json::to_value(request)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                if let Some(target) = target {
                    params.insert("targetDeviceId".into(), serde_json::Value::String(target));
                }
                let response = engine
                    .client()
                    .call(
                        methods::GET_CHECKOUT_FILE_DIFF_TEXT,
                        serde_json::Value::Object(params),
                    )
                    .await
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<mast_proto::CheckoutFileDiffText>(value).ok()
                    });
                let highlights = match response {
                    Some(response) => {
                        cx.background_executor()
                            .spawn(async move {
                                full_highlights(&fetch_file, lang, &response).map(Arc::new)
                            })
                            .await
                    }
                    None => None,
                };
                this.update(cx, |changes, cx| {
                    if let Some(slot) = changes.highlights.get_mut(&fetch_path)
                        && slot.fingerprint == fingerprint
                        && let Some(highlights) = highlights
                    {
                        slot.state = DiffHighlightState::Ready(highlights);
                        cx.notify();
                    }
                })
                .ok();
            })),
            _ => None,
        };
        self.highlights.insert(
            file.path.clone(),
            HighlightSlot {
                fingerprint,
                state: DiffHighlightState::Pending,
                _excerpt_task: Some(excerpt_task),
                _fetch_task: fetch_task,
            },
        );
        None
    }

    // ---- rendering ----

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = &self.parsed else {
            return gpui::Empty.into_any_element();
        };
        let files = parsed.files.clone();
        let origins = parsed.origins.clone();
        let submodules = parsed.submodules.clone();
        let parsed_key = parsed.key.clone();
        let Some(row) = self.rows.get(ix).copied() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        if let DiffRow::SubmoduleHeader { group } = row {
            let group_ix = group as usize;
            let Some(group) = submodules.get(group_ix) else {
                return gpui::Empty.into_any_element();
            };
            let collapsed = self
                .submodule_folds
                .get(&group.repository_path)
                .copied()
                .unwrap_or(false);
            return self.render_submodule_header(group_ix, group, collapsed, &theme, cx);
        }
        let Some(file_ix) = row.file() else {
            return gpui::Empty.into_any_element();
        };
        let highlight = files
            .get(file_ix)
            .zip(origins.get(file_ix))
            .and_then(|(file, origin)| self.request_highlight(file, origin, &parsed_key, cx));
        let hunk_index = match row {
            DiffRow::Line { hunk, .. } | DiffRow::SplitLine { hunk, .. } => Some(hunk),
            _ => None,
        };
        let word_spans = hunk_index.and_then(|hunk| files.get(file_ix)?.hunks.get(hunk as usize)
            .and_then(|data| self.request_word_spans((parsed_key.clone(), file_ix as u32, hunk), data, cx)));
        let horizontal = &self.parsed.as_ref().unwrap().horizontal[file_ix];
        let code_width = match files.get(file_ix) {
            Some(file) if !self.wrap_lines => {
                let mut metrics = horizontal.metrics(
                file,
                highlight.as_ref(),
                &theme,
                window.text_system(),
                crate::typography::generation(cx),
                );
                if let Some(expand_context::Entry::Ready { max_columns, max_line, .. }) = self.expanded_context.entries.get(&(parsed_key.clone(), file_ix as u32)) {
                    let mono = font(theme.font_mono.clone());
                    let size = px(diff_text_size(&theme));
                    let advance = window.text_system().ch_advance(window.text_system().resolve_font(&mono), size).unwrap_or(size * 0.6).as_f32();
                    metrics.max_text_width = metrics.max_text_width.max(*max_columns as f32 * advance);
                    let digits = (*max_line).max(1).ilog10() + 1;
                    metrics.max_gutter_width = metrics.max_gutter_width.max((digits as f32 * 6.6 + 14.0).max(GUTTER_WIDTH));
                }
                DiffCodeWidth::Scrollable(metrics)
            },
            _ => DiffCodeWidth::Wrapped,
        };
        let code_scroll = DiffCodeScrollContext {
            handle: horizontal.scroll.clone(),
            prefix: SharedString::from(format!("changes-code-row-{ix}")),
        };
        let file_gutter_width = files.get(file_ix).map(|file| {
            let max_line = match self.expanded_context.entries.get(&(parsed_key.clone(), file_ix as u32)) {
                Some(expand_context::Entry::Ready { max_line, .. }) => file.max_line.max(*max_line),
                _ => file.max_line,
            };
            let digits = max_line.max(1).ilog10() + 1;
            (digits as f32 * 6.6 + 14.0).max(GUTTER_WIDTH)
        }).unwrap_or(GUTTER_WIDTH);
        match row {
            DiffRow::SubmoduleHeader { .. } => gpui::Empty.into_any_element(),
            DiffRow::ContextControl { file, gap } => {
                let entry = self.expanded_context.entries.get(&(parsed_key.clone(), file));
                let loading = matches!(entry, Some(expand_context::Entry::Loading));
                let failed = matches!(entry, Some(expand_context::Entry::Failed));
                div().w_full().h(px(HUNK_HEADER_HEIGHT)).flex().justify_center().items_center().gap(px(12.0))
                    .text_color(theme.text_muted).text_size(px(10.0))
                    .child(div().id(("expand-context", ix)).cursor_pointer().child(if loading { "Loading…" } else if failed { "Context unavailable" } else { "Expand 20 lines" })
                        .on_click(cx.listener(move |this, _, _, cx| this.expand_context(file, gap, false, cx))))
                    .when(!loading && !failed, |el| el.child(div().id(("expand-context-all", ix)).cursor_pointer().child("Expand all")
                        .on_click(cx.listener(move |this, _, _, cx| this.expand_context(file, gap, true, cx)))))
                    .into_any_element()
            }
            DiffRow::ContextLine { file, gap, line } => {
                let Some(expand_context::Entry::Ready { gaps, .. }) = self.expanded_context.entries.get(&(parsed_key.clone(), file)) else { return gpui::Empty.into_any_element(); };
                let Some(line) = gaps.get(gap as usize).and_then(|gap| gap.lines.get(line as usize)) else { return gpui::Empty.into_any_element(); };
                let gutter = file_gutter_width;
                if self.mode == DiffMode::Unified {
                    diff_line_row(line, &[], &[], &theme, gutter, code_width, Some(code_scroll.slot("unified")))
                } else {
                    let runs = line_runs(line, None, &theme);
                    let left = split_line_cell(line, line.old_no, runs.clone(), &theme, gutter, code_width, Some(code_scroll.slot("old"))).into_any_element();
                    let right = split_line_cell(line, line.new_no, runs, &theme, gutter, code_width, Some(code_scroll.slot("new"))).into_any_element();
                    split_row(left, right, self.wrap_lines, &theme).into_any_element()
                }
            }
            DiffRow::FileHeader { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                self.render_file_header(
                    file as usize,
                    file_diff,
                    origins
                        .get(file as usize)
                        .map(|origin| origin.nesting_depth)
                        .unwrap_or(0),
                    &fold,
                    FileHeaderPresentation::Row,
                    &theme,
                    cx,
                )
            }
            DiffRow::Notice { file, notice } => files
                .get(file as usize)
                .and_then(|f| file_notices(f).into_iter().nth(notice as usize))
                .map(|text| notice_row(text, &theme))
                .unwrap_or_else(|| gpui::Empty.into_any_element()),
            DiffRow::HunkHeader { file, hunk } => {
                let Some(file_diff) = files.get(file as usize) else { return gpui::Empty.into_any_element(); };
                let Some(hunk_diff) = file_diff.hunks.get(hunk as usize) else { return gpui::Empty.into_any_element(); };
                let Some(identity) = self.parsed.as_ref().and_then(|parsed| parsed.hunk_identities.get(file as usize)).and_then(|items| items.get(hunk as usize)) else { return gpui::Empty.into_any_element(); };
                let viewed = self.review_state.as_ref().is_some_and(|state| state.viewed_hunks.contains(identity));
                let seen = self.seen_hunks.contains(identity);
                let label = if self.review_closed { "PR closed" } else if self.review_error.is_some() { "Review unavailable" } else if self.review_state.is_none() { "Loading review…" } else if viewed { "Viewed" } else if seen { "· Mark viewed" } else { "Mark viewed" };
                // Latest Turn's deleted-only preimage in the checkout's own repository is a tree snapshot, which CodeRef
                // cannot yet name. Expanded submodule hunks anchor to the submodule's own commits, so they never hit that.
                let nested_repository = origins.get(file as usize).is_some_and(|origin| origin.repository_path.is_some());
                let show_reference = nested_repository || !(self.scope == DiffScope::LatestTurn && code_ref::deleted_only_hunk(&hunk_diff.header));
                div().relative().child(hunk_header_row(&hunk_diff.header, &theme))
                    .when(show_reference, |row| row.child(
                        div().id(SharedString::from(format!("changes-hunk-chat-{file}-{hunk}")))
                            .absolute().right(px(100.0)).top(px(2.0)).cursor_pointer()
                            .text_size(px(10.0)).text_color(theme.text_muted).child("Add hunk to chat")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_indexed_hunk_to_chat(file as usize, hunk as usize, cx);
                            }))))
                    .child(div().id(SharedString::from(format!("changes-hunk-viewed-{file}-{hunk}")))
                        .absolute().right(px(8.0)).top(px(2.0)).cursor_pointer()
                        .text_size(px(10.0)).text_color(theme.text_muted)
                        .when(true, |el| crate::components::review_text_action(el, &theme))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_review_hunk(file as usize, hunk as usize, cx)))
                        .child(label))
                    .into_any_element()
            },
            DiffRow::Line {
                file,
                hunk,
                line,
                flat: _,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let line_index = line;
                let Some(line) = file_diff
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line as usize))
                else {
                    return gpui::Empty.into_any_element();
                };
                let spans = highlight
                    .as_deref()
                    .map(|highlights| highlights.spans(line))
                    .unwrap_or(&[]);
                let gutter_px = file_gutter_width;
                let row = diff_line_row(
                    line,
                    spans,
                    word_spans.as_ref().and_then(|spans| spans.get(line_index as usize)).map(Vec::as_slice).unwrap_or(&[]),
                    &theme,
                    gutter_px,
                    code_width,
                    Some(code_scroll.slot("unified")),
                );
                let Some((side, line_no)) = line_anchor(line) else {
                    return row;
                };
                let path = file_diff.path.clone();
                let source_side = match side { CommentSide::Old => SourceSide::Old, CommentSide::New => SourceSide::New };
                let hovered = self.hovering(&path, (side, line_no));
                let move_path = path.clone();
                let leave_path = path.clone();
                let select_path = path.clone();
                let selected = self.explanation_unified_rows.map_or_else(|| self.explanation_line_selected(&path, source_side, line_no), |(start, end, _)| self.explanation_selection.as_ref().is_some_and(|s| s.path == path) && (start.min(end)..=start.max(end)).contains(&ix));
                let selection_end = self.explanation_unified_rows.map_or_else(|| self.explanation_selection.as_ref().is_some_and(|s| s.path == path && s.side == source_side && s.extent == line_no), |(_, end, _)| end == ix && self.explanation_selection.as_ref().is_some_and(|s| s.path == path));
                div()
                    .id(("diff-line", ix))
                    .w_full()
                    .relative()
                    .when(selected, |el| el.bg(theme.element_active))
                    .child(row)
                    .when(selection_end, |el| el.child(self.render_explanation(&theme, cx)))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        window.focus(&this.diff_focus, cx);
                        this.select_unified_explanation_line(&select_path, source_side, line_no, ix, event.modifiers.shift, cx);
                    }))
                    .when(hovered, |el| {
                        el.child(positioned_adder(
                            comment_adder_left(side, gutter_px),
                            render_comment_adder(&path, side, line_no, &theme, cx),
                        ))
                    })
                    .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == move_path) {
                            this.select_unified_explanation_line(&move_path, source_side, line_no, ix, true, cx);
                        }
                        this.set_hover(&move_path, Some((side, line_no)), cx);
                    }))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.clear_hover_at(&leave_path, (side, line_no), cx);
                        }
                    }))
                    .into_any_element()
            }
            DiffRow::SplitLine {
                file,
                hunk,
                left,
                right,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(lines) = file_diff.hunks.get(hunk as usize).map(|h| &h.lines) else {
                    return gpui::Empty.into_any_element();
                };
                let gutter_px = file_gutter_width;
                // Same slot on both sides = a context row: one line, drawn in
                // both columns.
                let mirrored = left.is_some() && left == right;
                let (left_slot, right_slot) = (left, right);
                let left = left.and_then(|slot| lines.get(slot as usize));
                let right = right.and_then(|slot| lines.get(slot as usize));
                // `\ No newline at end of file` is not code on one side — it
                // is a note about the row, so it spans both columns. Pairing
                // never puts a marker opposite code, so either side having one
                // means the whole row is the marker.
                if let Some(line) = [left, right]
                    .into_iter()
                    .flatten()
                    .find(|line| line.kind == LineKind::Meta)
                {
                    return meta_line_row(&line.text, &theme, 2.0 * (ACCENT_BAR_WIDTH + gutter_px));
                }
                // Refcounted, not cloned per listener: a split row wires up to
                // four of them, and this runs for every row in the viewport
                // plus the list's overdraw, every frame.
                let path: SharedString = file_diff.path.clone().into();
                // A mirrored row's columns carry the same text and the same
                // spans, so the runs are built once and shared — context is
                // most of a diff, so this is most of the rows.
                let shared_runs = mirrored
                    .then(|| left.map(|line| line_runs(line, highlight.as_deref(), &theme)))
                    .flatten();
                let cell = |line: Option<&DiffLine>, slot: Option<u32>, old: bool| {
                    line.map(|line| {
                        let runs = shared_runs
                            .clone()
                            .unwrap_or_else(|| line_runs(line, highlight.as_deref(), &theme));
                        let ranges = slot.and_then(|slot| word_spans.as_ref()?.get(slot as usize)).map(Vec::as_slice).unwrap_or(&[]);
                        let runs = word_diff::tint_runs(runs, ranges, word_tint(line, &theme));
                        let number = if old { line.old_no } else { line.new_no };
                        split_line_cell(
                            line,
                            number,
                            runs,
                            &theme,
                            gutter_px,
                            code_width,
                            Some(code_scroll.slot(if old { "old" } else { "new" })),
                        )
                    })
                };
                // The left column is inert. It shows the pre-change file, and
                // a deleted line is not there to be changed — a note on it
                // would cite a line the agent cannot edit. Everything is cited
                // against the new file, so only the right column takes a `+`.
                // Cards for old-side notes still render (they are pushed by
                // the row, not the column), so switching layouts never hides
                // one that is already staged.
                let left_number = left.and_then(|line| line.old_no);
                let left_path = path.clone();
                let left_move_path = path.clone();
                let left = cell(left, left_slot, true)
                    .map(|cell| cell.id(("split-old", ix)).relative()
                        .when(left_number.is_some_and(|number| self.explanation_line_selected(&path, SourceSide::Old, number)), |el| el.bg(theme.element_active))
                        .when(left_number.is_some_and(|number| self.explanation_selection.as_ref().is_some_and(|s| s.path == path.as_ref() && s.side == SourceSide::Old && s.extent == number)), |el| el.child(self.render_explanation(&theme, cx)))
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            window.focus(&this.diff_focus, cx);
                            if let Some(number) = left_number { this.select_explanation_line(&left_path, SourceSide::Old, number, event.modifiers.shift, cx); }
                        }))
                        .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                            if let Some(number) = left_number && event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == left_move_path.as_ref() && s.side == SourceSide::Old) {
                                this.select_explanation_line(&left_move_path, SourceSide::Old, number, true, cx);
                            }
                        }))
                        .into_any_element())
                    .unwrap_or_else(|| split_filler().into_any_element());
                let right = match (cell(right, right_slot, false), right.and_then(line_anchor)) {
                    (Some(cell), Some(anchor)) => {
                        let (side, line_no) = anchor;
                        let (move_path, leave_path) = (path.clone(), path.clone());
                        let select_path = path.clone();
                        cell.id(("split-new", ix)).relative()
                            .when(self.explanation_line_selected(&path, SourceSide::New, line_no), |el| el.bg(theme.element_active))
                            .when(self.explanation_selection.as_ref().is_some_and(|s| s.path == path.as_ref() && s.side == SourceSide::New && s.extent == line_no), |el| el.child(self.render_explanation(&theme, cx)))
                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                window.focus(&this.diff_focus, cx);
                                this.select_explanation_line(&select_path, SourceSide::New, line_no, event.modifiers.shift, cx);
                            }))
                            .when(self.hovering(&path, anchor), |el| {
                                el.relative().child(positioned_adder(
                                    split_adder_left(gutter_px),
                                    render_comment_adder(&path, side, line_no, &theme, cx),
                                ))
                            })
                            .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                                if event.dragging() && this.explanation_selection.as_ref().is_some_and(|s| s.path == move_path.as_ref() && s.side == SourceSide::New) {
                                    this.select_explanation_line(&move_path, SourceSide::New, line_no, true, cx);
                                }
                                this.set_hover(&move_path, Some(anchor), cx);
                            }))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if !*hovered {
                                    this.clear_hover_at(&leave_path, anchor, cx);
                                }
                            }))
                            .into_any_element()
                    }
                    (Some(cell), None) => cell.into_any_element(),
                    (None, _) => split_filler().into_any_element(),
                };
                split_row(left, right, self.wrap_lines, &theme).into_any_element()
            }
            DiffRow::CommentCard { file, card } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let comment = self.comment_index.get(&file_diff.path).and_then(|comments| comments.get(card as usize)).cloned();
                match comment.as_ref() {
                    Some(comment) if self.review_suggestions.contains_key(&comment.id) => self.render_suggestion_card(comment, &theme, cx),
                    Some(comment) if self.is_local_draft(&comment.id) => self.render_local_draft_card(comment, &theme, cx),
                    Some(comment) => crate::comment_ui::render_comment_card(
                        comment,
                        &theme,
                        cx,
                        Self::edit_comment,
                        Self::remove_comment,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::CommentDraft { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                match self
                    .draft
                    .as_ref()
                    .filter(|draft| draft.path == file_diff.path)
                {
                    // Header cites the same path the staged card and the
                    // prompt bullet will.
                    Some(draft) => crate::comment_ui::render_comment_draft(
                        draft_cite_path(draft),
                        draft.line,
                        draft.input.clone(),
                        draft.editing_id.is_some(),
                        &theme,
                        cx,
                        Self::cancel_draft,
                        Self::commit_draft,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::ReviewThread { thread, height, .. } => {
                self.render_review_thread(thread as usize, height, &theme)
            }
            DiffRow::BodyPad { .. } => div().w_full().h(px(BODY_BOTTOM_PAD)).into_any_element(),
            DiffRow::FoldingBody { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                let (from, to) = (fold.from, fold.to);
                // Only the revealable slice is built — the tween never pays
                // for lines it cannot show.
                let cap = from.max(to).min(FOLD_TWEEN_MAX_PX);
                let body = render_file_body_upto(
                    file_diff,
                    highlight,
                    &theme,
                    cap,
                    self.mode,
                    code_width,
                    Some(DiffCodeScrollContext {
                        handle: code_scroll.handle.clone(),
                        prefix: SharedString::from(format!(
                            "changes-fold-code-{file}-{}",
                            fold.epoch
                        )),
                    }),
                );
                let clipped = div().w_full().overflow_hidden().child(body);
                if fold.animating() {
                    clipped
                        .with_animation(
                            SharedString::from(format!("fold-{}-{}", file_diff.path, fold.epoch)),
                            COLLAPSE.animation(),
                            move |el, t| el.h(px(motion::lerp(from, to, t))),
                        )
                        .into_any_element()
                } else {
                    // Post-tween, pre-settle: hold the full target height so
                    // the settle splice swaps rows without any reflow (the
                    // capped slice always covers what the viewport can see —
                    // tweens start from a clicked, on-screen header).
                    clipped.h(px(to)).into_any_element()
                }
            }
        }
    }

    fn render_submodule_header(
        &mut self,
        group_ix: usize,
        group: &SubmoduleGroup,
        collapsed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let repository_path = group.repository_path.clone();
        let revision = match (&group.old_revision, &group.new_revision) {
            (Some(old), Some(new)) => Some(format!(
                "{} → {}",
                &old[..old.len().min(7)],
                &new[..new.len().min(7)]
            )),
            _ => None,
        };
        let left_pad = Theme::SPACE_MD + group.nesting_depth as f32 * 16.0;
        div()
            .id(("submodule-hdr", group_ix))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(left_pad))
            .pr(px(Theme::SPACE_MD))
            .bg(theme.ink(0.025))
            .cursor_pointer()
            .when(true, |element| crate::components::changes_diff_header(element, theme, theme.ink(0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_submodule(group_ix, cx);
            }))
            .child(
                crate::icons::icon(if collapsed {
                    crate::icons::ALT_ARROW_RIGHT
                } else {
                    crate::icons::ALT_ARROW_DOWN
                })
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.7)),
            )
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::directory(&repository_path, false),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(repository_path)),
            )
            .when_some(revision, |el, revision| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(revision)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!(
                        "{} {}",
                        group.file_count,
                        if group.file_count == 1 {
                            "file"
                        } else {
                            "files"
                        }
                    ))),
            )
            .when(group.truncated, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(9.0))
                        .text_color(theme.warning.opacity(0.8))
                        .child("PARTIAL"),
                )
            })
            .when(group.dirty, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(9.0))
                        .px(px(5.0))
                        .py(px(1.0))
                        .when(true, |el| crate::components::changes_warning_badge(el, theme, crate::components::ChangesWarning::Dirty))
                        .child("DIRTY"),
                )
            })
            .child(
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.0))
                    .text_color(add_color(theme))
                    .child(SharedString::from(format!("+{}", group.additions))),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.0))
                    .text_color(del_color(theme))
                    .child(SharedString::from(format!("−{}", group.deletions))),
            )
            .into_any_element()
    }

    fn render_file_header(
        &mut self,
        ix: usize,
        file: &FileDiff,
        nesting_depth: usize,
        fold: &FileFold,
        presentation: FileHeaderPresentation,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = fold.collapsed;
        let path = file.path.clone();
        let adds = file.additions;
        let dels = file.deletions;
        let sticky = presentation == FileHeaderPresentation::Sticky;
        let sticky_paint = sticky.then(|| sticky_file_header_paint(theme));
        let rest_bg = if let Some(paint) = sticky_paint {
            paint.rest_bg
        } else {
            theme.ink(0.025)
        };
        let hover_bg = if let Some(paint) = sticky_paint {
            paint.hover_bg
        } else {
            theme.ink(0.05)
        };

        // Chevron (zeron checkout-diff-sidebar): chevron-right closed,
        // chevron-down open; gpui divs have no rotation transform at the
        // pinned rev, so the glyph swap crossfades over the same 200 ms.
        let chevron_icon = if collapsed {
            crate::icons::ALT_ARROW_RIGHT
        } else {
            crate::icons::ALT_ARROW_DOWN
        };
        let chevron = div().flex_none().size(px(14.0)).child(
            crate::icons::icon(chevron_icon)
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.7)),
        );
        let chevron: AnyElement = if fold.animating() {
            chevron
                .with_animation(
                    SharedString::from(format!(
                        "chev-{}-{path}-{}",
                        presentation.key_prefix(),
                        fold.epoch
                    )),
                    CHEVRON.animation(),
                    |el, t| el.opacity(0.25 + 0.75 * t),
                )
                .into_any_element()
        } else {
            chevron.into_any_element()
        };

        // Header row: chevron + mono path (one quiet tone) + right-aligned
        // +N / −N counts on a slightly raised wash. The header carries the
        // section separator (the per-file wrapper it used to hang on is
        // gone — rows are flat now).
        div()
            .id(presentation.element_id(ix))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .when(
                presentation == FileHeaderPresentation::Row && ix > 0,
                |el| el.border_t_1().border_color(crate::theme::hairline(0.04)),
            )
            .when(sticky, |el| {
                el.border_b_1()
                    .border_color(sticky_paint.expect("sticky paint").border)
                    .block_mouse_except_scroll()
            })
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(Theme::SPACE_MD + nesting_depth as f32 * 16.0))
            .pr(px(Theme::SPACE_MD))
            .bg(rest_bg)
            .cursor_pointer()
            .when(true, |element| crate::components::changes_diff_header(element, theme, hover_bg))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_fold(ix, cx);
                window.focus(&this.diff_focus, cx);
                cx.notify();
            }))
            .child(chevron)
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::file(&file.path),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(file.path.clone())),
            )
            .when(collapsed && is_large_file(file), |el| {
                el.child(div().flex_none().text_size(px(10.0)).text_color(theme.text_muted).child("Load diff"))
            })
            .when(file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from("BIN")),
                )
            })
            .when(adds > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{adds}"))),
                )
            })
            .when(dels > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{dels}"))),
                )
            })
            .into_any_element()
    }

    fn render_sticky_file_header(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scroll_top = self.list.logical_scroll_top();
        let sticky = sticky_file_header(
            &self.row_ranges,
            &self.section_headers,
            scroll_top.item_ix,
            scroll_top.offset_in_item.as_f32(),
        )?;
        debug_assert_eq!(
            self.rows.get(sticky.header_row),
            Some(&DiffRow::FileHeader {
                file: sticky.file_ix as u32,
            })
        );
        let files = self.parsed.as_ref()?.files.clone();
        let origins = self.parsed.as_ref()?.origins.clone();
        let file = files.get(sticky.file_ix)?;
        let fold = self.folds.get(&file.path).copied().unwrap_or_default();
        let next_header_y = sticky.next_header_row.and_then(|row| {
            let bounds = self.list.bounds_for_item(row)?;
            let viewport = self.list.viewport_bounds();
            Some((bounds.origin.y - viewport.origin.y).as_f32())
        });
        let top_offset = sticky_header_push_offset(next_header_y);
        let header = self.render_file_header(
            sticky.file_ix,
            file,
            origins
                .get(sticky.file_ix)
                .map(|origin| origin.nesting_depth)
                .unwrap_or(0),
            &fold,
            FileHeaderPresentation::Sticky,
            theme,
            cx,
        );
        let paint = sticky_file_header_paint(theme);
        // The sticky floats over diff rows, but it belongs to the same content
        // plane. Tint the blur with `theme.bg`; `glass_overlay` is deliberately
        // reserved for elevated menus/cards and produced the wrong hue here.
        let header = if let Some(tint) = paint.frost_tint {
            div().w_full().bg(tint).child(header).into_any_element()
        } else {
            header
        };
        // Frosted is a pass-through when the resolved surface is opaque.
        let header = crate::frost::frosted(0.0, STICKY_FILE_HEADER_BLUR, header);

        Some(
            div()
                .absolute()
                .top(px(top_offset))
                .left_0()
                .w_full()
                .child(header)
                .into_any_element(),
        )
    }

    /// A small hover-washed icon button for the pane header. The header lives
    /// inside the titlebar drag strip, so the button occludes and swallows the
    /// mouse-down (same discipline as the shell's `header_icon_button`).
    fn header_button(
        id: &'static str,
        icon_path: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        Self::header_toggle(id, icon_path, false, theme)
    }

    /// [`Self::header_button`] with a latched look: an `active` toggle holds
    /// the hover wash and the full text tone, so the pane says which layout
    /// it is in without a label.
    fn header_toggle(
        id: &'static str,
        icon_path: &'static str,
        active: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        crate::components::changes_header_toggle(id, icon_path, active, theme)
    }

    /// The unified ⇄ split layout toggle (both the scoped and the
    /// commit-pinned toolbars carry it).
    fn split_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-split",
            crate::icons::SPLIT_COLUMNS,
            self.mode.is_split(),
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_mode(cx);
        }))
        .into_any_element()
    }

    fn wrap_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-wrap",
            crate::icons::WRAP_TEXT,
            self.wrap_lines,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_wrap(cx);
        }))
        .tooltip(|_, cx| {
            cx.new(|_| DiffHeaderTooltip("Wrap long lines".into()))
                .into()
        })
        .tooltip_show_delay(Duration::from_millis(350))
        .into_any_element()
    }

    fn word_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle("changes-word-diff", crate::icons::PEN, self.word_diff, theme)
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.word_diff = !this.word_diff;
                let enabled = this.word_diff;
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| settings.diff_word = enabled);
                cx.notify();
            }))
            .tooltip(|_, cx| cx.new(|_| DiffHeaderTooltip("Highlight changed text".into())).into())
            .tooltip_show_delay(Duration::from_millis(350))
            .into_any_element()
    }

    /// The pane-header controls: scope dropdown, `{branch} → {base ⌄}` ref
    /// selector (branch scope), fold-all. Rendered BY THE SHELL inside the
    /// session titlebar's trailing section (the band above the pane) — the
    /// titlebar overlay owns that strip's hit-testing, so controls mounted
    /// under it would never see a click. The expand and close buttons ride
    /// alongside, shell-owned (they mutate shell state).
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // An unconfirmed bound-Chat member has no diff, so no diff actions.
        if self.pinned_cwd.is_none()
            && self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.unconfirmed.is_some())
        {
            return div()
                .debug_selector(|| "changes-header-unconfirmed".into())
                .into_any_element();
        }
        let theme = Theme::of(cx).clone();
        // Commit-pinned pane: the pin never changes, so a fixed identity
        // chip (mono short sha + subject) replaces the scope dropdown;
        // fold-all still trails.
        if let Some(commit) = self.commit.clone() {
            let short: String = commit.sha.chars().take(7).collect();
            return div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .flex_none()
                        .h(px(crate::surface_chrome::CONTROL_SIZE))
                        .px(px(6.0))
                        .when(true, |el| crate::components::changes_revision_label(el, &theme))
                        .flex()
                        .items_center()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(short)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(commit.subject.clone())),
                )
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element();
        }
        // Repo Map inspection is deliberately pinned to one checkout. Do not
        // expose the normal scope menu, which would imply it can follow the
        // currently selected chat or latest-turn state.
        if self.pinned_cwd.is_some() {
            return div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child("Uncommitted changes"),
                )
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element();
        }
        let scope = self.scope;
        let history_branch = (scope == DiffScope::History)
            .then(|| self.owner_branch().unwrap_or_else(|| "HEAD".to_string()));
        let history_count = (scope == DiffScope::History).then(|| self.history_count(cx));
        let history_search_control =
            (scope == DiffScope::History).then(|| self.history_search_control(cx));
        let history_fetch_button =
            (scope == DiffScope::History).then(|| self.history_fetch_button(cx));
        let history_view_button =
            (scope == DiffScope::History).then(|| self.history_view_button(cx));
        let scope_trigger = div()
            .id("changes-scope-trigger")
            .debug_selector(|| "changes-scope-trigger".into())
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(8.0))
            .max_w(px(250.0))
            .min_w_0()
            .flex_shrink(1.0)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-scope-trigger",
                crate::theme::wash(0.05),
                crate::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("changes-scope-trigger"))
            .when(true, |element| crate::components::changes_selector(element, &theme, crate::components::ChangesSelector::Scope))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.scope_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.scope_menu.take_press_was_open() {
                    this.close_scope_menu(cx);
                } else {
                    this.scope_menu.open(());
                }
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .line_height(px(14.0))
                    .text_color(theme.text)
                    .child(SharedString::from(scope.label())),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if scope == DiffScope::History {
            // The tab already identifies this as History; use the fixed title
            // slot for the current branch instead of repeating the surface name.
            div()
                .id("history-surface-title")
                .min_w_0()
                .max_w(px(160.0))
                .truncate()
                .h(px(crate::surface_chrome::CONTROL_SIZE))
                .px(px(8.0))
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .font_family(theme.font_mono.clone())
                .text_size(px(11.5))
                .line_height(px(14.0))
                .text_color(theme.text_dim)
                .child(SharedString::from(history_branch.unwrap_or_default()))
                .into_any_element()
        } else if scope == DiffScope::Range {
            // A branch / PR target has exactly one scope: name it ("PR #n" /
            // "branch <name>") instead of offering the scope menu.
            let label = self
                .owner
                .as_ref()
                .and_then(|owner| owner.range.as_ref())
                .map(|range| range.label.clone())
                .unwrap_or_else(|| scope.label().to_owned());
            div()
                .id("changes-range-title")
                .debug_selector(|| "changes-range-title".into())
                .h(px(crate::surface_chrome::CONTROL_SIZE))
                .px(px(8.0))
                .max_w(px(250.0))
                .min_w_0()
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    crate::icons::icon(crate::icons::GIT_BRANCH)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .line_height(px(14.0))
                        .text_color(theme.text)
                        .child(SharedString::from(label)),
                )
                .into_any_element()
        } else if self.scope_menu.get().is_some() {
            let closing = self.scope_menu.closing_since();
            let menu = self.render_scope_menu(&theme, cx);
            scope_trigger
                .relative()
                .child(popover::anchored_menu_below_gap(
                    "changes-scope-menu",
                    menu,
                    closing,
                    10.0,
                ))
                .into_any_element()
        } else {
            scope_trigger.into_any_element()
        };

        let trailing: AnyElement = if scope == DiffScope::History {
            div()
                .min_w_0()
                .flex_shrink(1.0)
                .flex()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .children(history_search_control)
                .children(history_fetch_button)
                .children(history_view_button)
                .child(
                    Self::header_button("history-refresh", crate::icons::REFRESH, &theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.history_pane(cx)
                                .update(cx, |history, cx| history.refresh(cx));
                        }),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(self.word_toggle(&theme, cx))
                .child(
                    Self::header_button("changes-fold-all", crate::icons::FOLD_VERTICAL, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_collapse_all(cx);
                        })),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::surface_chrome::CONTROL_GAP))
            .child(trigger)
            .when_some(history_count, |element, count| {
                element.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .h(px(crate::surface_chrome::CONTROL_SIZE))
                        .flex()
                        .items_center()
                        .child(count),
                )
            })
            .children(self.render_ref_selector(&theme, cx))
            .when(scope != DiffScope::History, |element| {
                element.child(div().flex_1())
            })
            .child(trailing)
            .into_any_element()
    }

    fn render_scope_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let current = self.scope;
        popover::popover_card(theme)
            .w(px(310.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_scope_menu(cx)))
            .child(
                // The 2px row gap every other menu carries — rows straight on
                // the card abutted, adjacent washes read as one slab (user
                // report).
                div().flex().flex_col().gap(px(2.0)).children(
                    DiffScope::ALL.into_iter().enumerate().map(|(ix, scope)| {
                        popover::menu_row(
                            theme,
                            scope == current,
                            format!("changes-scope-row-{ix}"),
                        )
                        .id(("changes-scope-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_scope(scope, cx);
                            this.close_scope_menu(cx);
                        }))
                        .child(div().flex_1().child(SharedString::from(scope.label())))
                    }),
                ),
            )
            .into_any_element()
    }

    /// `{branch} → {base ⌄}` — which ref the branch scope compares against
    /// (t3code's ref strip), inlined into the pane header. Branch scope only.
    fn render_ref_selector(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.scope == DiffScope::Range {
            // Fixed pair, no base picker: `head → base` of the target.
            let range = self.owner.as_ref().and_then(|owner| owner.range.as_ref())?;
            return Some(
                div()
                    .id("changes-range-refs")
                    .debug_selector(|| "changes-range-refs".into())
                    .min_w_0()
                    .truncate()
                    .h(px(crate::surface_chrome::CONTROL_SIZE))
                    .flex()
                    .items_center()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(format!(
                        "{} \u{2192} {}",
                        range.head_branch, range.base_branch
                    )))
                    .into_any_element(),
            );
        }
        if self.scope != DiffScope::Branch {
            return None;
        }
        let branch = self.owner_branch().unwrap_or_else(|| "HEAD".to_string());
        let base = self.base_ref.clone().unwrap_or_else(|| "…".to_string());
        // Even truncation: taffy shrinks flex items ∝ factor × basis, and the
        // default factor of 1 splits the deficit proportionally to content —
        // a long branch stayed near-whole while a short base ("main") read as
        // a bare ellipsis (user report). Weighting each side's factor by its
        // own length SQUARED (mono font, so chars ∝ px) lands the deficit
        // ~cubically on the longer name: the short side's loss stays
        // sub-pixel even under a big deficit (a linear weight still cost it
        // a char), while equal lengths still split evenly.
        let branch_weight = (branch.chars().count().max(1) as f32).powi(2);
        let base_weight = (base.chars().count().max(1) as f32).powi(2);
        let trigger = div()
            .id("changes-ref-trigger")
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(6.0))
            // Shrinkable, like the branch label beside it — a flex_none
            // trigger with a long base name plowed over the header buttons
            // (user report); both sides truncate instead.
            .min_w_0()
            .flex_shrink(base_weight)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-ref-trigger",
                crate::theme::wash(0.0),
                crate::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-ref-trigger"))
            .when(true, |element| crate::components::changes_selector(element, theme, crate::components::ChangesSelector::Reference))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.ref_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.ref_menu.take_press_was_open() {
                    this.close_ref_menu(cx);
                    cx.notify();
                } else {
                    this.open_ref_menu(window, cx);
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(theme.text)
                    .child(SharedString::from(base)),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if self.ref_menu.get().is_some() {
            let closing = self.ref_menu.closing_since();
            let menu = self.render_ref_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_gap(
                "changes-ref-menu",
                menu,
                closing,
                10.0,
            ))
        } else {
            trigger
        };
        Some(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                // Extra room off the scope dropdown (row gap alone read
                // cramped — user report).
                .ml(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .min_w_0()
                        .flex_shrink(branch_weight)
                        .truncate()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.5))
                        .text_color(theme.text_dim)
                        .child(SharedString::from(branch)),
                )
                .child(
                    crate::icons::icon(crate::icons::ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint),
                )
                .child(trigger)
                .into_any_element(),
        )
    }

    fn render_ref_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll) = {
            let Some(menu) = self.ref_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
            )
        };
        let rows = self.ref_menu_rows(cx);
        let current = self.base_ref.clone();
        let branches = self.branches.clone();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(if branches.is_empty() {
                    "No branches"
                } else {
                    "No matching branches"
                }))
                .into_any_element()
        } else {
            div()
                .id("changes-ref-list")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, branch_ix)| {
                    let name = branches[branch_ix].clone();
                    let selected = current.as_deref() == Some(name.as_str());
                    let label = name.clone();
                    popover::menu_row_nav(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-ref-row-{row_ix}"),
                    )
                    .id(("changes-ref-row", row_ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_base_ref(name.clone(), cx);
                        this.close_ref_menu(cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(12.0))
                            .child(SharedString::from(label)),
                    )
                }))
                .into_any_element()
        };

        popover::popover_card(theme)
            .w(px(240.0))
            .track_focus(&focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.ref_menu_key(event, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_ref_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    /// The "Diff checkout..." button of the context chrome and, while open,
    /// its picker (`None` when the chat cannot pick a checkout).
    fn render_checkout_picker(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.checkout_picker_available(cx) {
            return None;
        }
        let trigger = div()
            .id("changes-checkout-trigger")
            .debug_selector(|| "changes-checkout-trigger".into())
            .flex_none()
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .bg(motion::hover_blend(
                "changes-checkout-trigger",
                crate::theme::wash(0.0),
                crate::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-checkout-trigger"))
            .when(true, |element| crate::components::changes_selector(element, theme, crate::components::ChangesSelector::Target))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.checkout_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.checkout_menu.take_press_was_open() {
                    this.close_checkout_menu(cx);
                    cx.notify();
                } else {
                    this.open_checkout_menu(window, cx);
                }
            }))
            .child(
                crate::icons::icon(crate::icons::GIT_BRANCH)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from("Diff checkout\u{2026}"));
        let trigger = if self.checkout_menu.get().is_some() {
            let closing = self.checkout_menu.closing_since();
            let menu = self.render_checkout_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_end(
                "changes-checkout-menu",
                menu,
                closing,
            ))
        } else {
            trigger
        };
        Some(trigger.into_any_element())
    }

    fn render_checkout_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll, state) = {
            let Some(menu) = self.checkout_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
                menu.candidates.clone(),
            )
        };
        let rows = self.checkout_menu_rows(cx);
        let note = |text: &'static str| {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(text))
                .into_any_element()
        };
        let list: AnyElement = match &state {
            popover::Loadable::Idle | popover::Loadable::Loading => {
                note("Looking for checkouts\u{2026}")
            }
            popover::Loadable::Error(message) => div()
                .p(px(4.0))
                .child(crate::error_notice::ErrorNotice::from_text(
                    "changes-checkout-list-error",
                    theme,
                    message,
                ))
                .into_any_element(),
            popover::Loadable::Ready(ranked) if rows.is_empty() => note(if ranked.is_empty() {
                "No checkouts found"
            } else {
                "No matching checkouts"
            }),
            popover::Loadable::Ready(ranked) => div()
                .id("changes-checkout-list")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(280.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, ranked_ix)| {
                    let row = ranked[ranked_ix].clone();
                    let tip: SharedString = row.candidate.path.clone().into();
                    let candidate = row.candidate.clone();
                    let selected = row.candidate.selected;
                    let removable = row.candidate.bound && row.candidate.kind != mast_proto::DiffCheckoutKind::Own;
                    let remove_path = row.candidate.path.clone();
                    popover::menu_row_nav(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-checkout-row-{row_ix}"),
                    )
                    .id(("changes-checkout-row", row_ix))
                    .debug_selector(move || format!("changes-checkout-row-{row_ix}"))
                    .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tip.clone())).into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_checkout(candidate.clone(), cx);
                    }))
                    .child(
                        // Check mark = the selected checkout; dot = bound.
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |el| {
                                el.child(
                                    crate::icons::icon(crate::icons::CHECK)
                                        .size(px(12.0))
                                        .text_color(theme.accent),
                                )
                            })
                            .when(!selected && row.candidate.bound, |el| {
                                el.child(
                                    div()
                                        .size(px(5.0))
                                        .rounded_full()
                                        .bg(theme.text_muted.opacity(0.7)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(12.0))
                                            .child(SharedString::from(row.label())),
                                    )
                                    .when(row.suggested, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .px(px(5.0))
                                                .when(true, |el| crate::components::changes_suggested_badge(el, theme))
                                                .text_size(px(9.5))
                                                .child("suggested"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(9.5))
                                            .text_color(theme.text_faint)
                                            .child(row.kind_tag()),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(10.5))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(crate::diff_targets::short_path(
                                        &row.candidate.path,
                                    ))),
                            ),
                    )
                    .when(removable, |element| element.child(
                        div().id(("changes-checkout-remove", row_ix))
                            .debug_selector(move || format!("changes-checkout-remove-{row_ix}"))
                            .flex_none().px(px(6.0)).text_size(px(11.0))
                            .text_color(theme.text_muted).child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_checkout(remove_path.clone(), cx);
                            }))
                    ))
                }))
                .into_any_element(),
        };

        popover::popover_card(theme)
            .w(px(340.0))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.checkout_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_checkout_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    /// The last failed `SetChatDiffTarget` write as a copyable error notice.
    fn render_diff_target_error(&self, theme: &Theme, cx: &App) -> Option<AnyElement> {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return None;
        }
        let chat_id = self.owner_chat_id_owned()?;
        let message = self.state.read(cx).chat_diff_target_error(&chat_id)?;
        let state = self.state.clone();
        Some(
            div()
                .debug_selector(|| "changes-diff-target-error".into())
                .flex_none()
                .px(px(Theme::SPACE_LG))
                .py(px(4.0))
                .child(
                    crate::error_notice::ErrorNotice::from_text(
                        "changes-diff-target-error",
                        theme,
                        message,
                    )
                    .on_dismiss(move |_, cx| {
                        let chat_id = chat_id.clone();
                        state.update(cx, |state, cx| {
                            state.dismiss_chat_diff_target_error(&chat_id, cx)
                        });
                    }),
                )
                .into_any_element(),
        )
    }

    fn render_header_strip(&self, theme: &Theme) -> Option<AnyElement> {
        let parsed = self.parsed.as_ref()?;
        Some(
            div()
                .flex_none()
                .h(px(crate::surface_chrome::HEADER_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .px(px(Theme::SPACE_LG))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(scope_label(
                            self.scope,
                            parsed.file_count,
                            self.base_ref.as_deref(),
                        ))),
                )
                .child(
                    div()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{}", parsed.additions))),
                )
                .child(
                    div()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{}", parsed.deletions))),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!(
                            "{} hunks",
                            logical_hunk_count(&parsed.layout, &parsed.files)
                        ))),
                )
                .child(div().flex_1())
                .when(parsed.truncated, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(px(10.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .when(true, |el| crate::components::changes_warning_badge(el, theme, crate::components::ChangesWarning::Partial))
                            .child(SharedString::from("Partial snapshot")),
                    )
                })
                .into_any_element(),
        )
    }

    fn context_details(&self, cx: &App) -> Option<(String, String, String, String)> {
        let (repo, worktree, branch, path) = if let Some(owner) = &self.owner {
            let (repo, worktree) = owner.repo_and_worktree();
            (
                repo,
                worktree,
                owner.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                owner
                    .cwd
                    .clone()
                    .unwrap_or_else(|| "Path unavailable".to_string()),
            )
        } else if let Some(cwd) = &self.pinned_cwd {
            let target = ChangesTarget {
                unconfirmed: None,
                exact_checkout: false,
                range: None,
                chat_id: String::new(),
                device_id: String::new(),
                cwd: Some(cwd.clone()),
                checkout_id: None,
                branch: None,
            };
            let (repo, worktree) = target.repo_and_worktree();
            (repo, worktree, "HEAD".to_string(), cwd.clone())
        } else {
            return None;
        };
        let freshness = self
            .active_diff(cx)
            .map(|diff| {
                format!(
                    "Snapshot age {}",
                    crate::state::format_time_ago(diff.updated_at, chrono::Utc::now())
                )
            })
            .unwrap_or_else(|| "Snapshot pending".to_string());
        Some((repo, worktree, branch, format!("{path}\n{freshness}")))
    }

    /// Bound Chats: one pill per primary/member repository with its branch
    /// or HEAD; picking one retargets the diff (shared with the composer's
    /// branch indicator through `AppState`).
    fn render_execution_selector(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pinned_cwd.is_some() || self.commit.is_some() {
            return None;
        }
        let chat_id = self.owner_chat_id_owned()?;
        let state = self.state.read(cx);
        let members = state.chat_execution_members(&chat_id);
        if members.is_empty() {
            return None;
        }
        let selected = state
            .selected_execution_member(&chat_id)
            .map(|member| member.repo_id);
        let status = state.chat_execution_state(&chat_id);
        let loading = status.is_some_and(|status| status.loading);
        let error = status.and_then(|status| status.error.clone()).or_else(|| {
            status
                .and_then(|status| status.reply.as_ref())
                .filter(|reply| !reply.ok)
                .and_then(|reply| {
                    crate::work_outline::execution::validation_issues(reply)
                        .into_iter()
                        .next()
                })
                .map(|issue| format!("{} {}", issue.message, issue.remediation))
        });
        Some(
            div()
                .debug_selector(|| "changes-execution-repositories".into())
                .flex_none()
                .px(px(Theme::SPACE_LG))
                .py(px(4.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(4.0))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .children(members.into_iter().enumerate().map(|(ix, member)| {
                    let active = selected.as_deref() == Some(member.repo_id.as_str());
                    let repo_id = member.repo_id.clone();
                    let chat_id = chat_id.clone();
                    let tip: SharedString = match &member.range {
                        Some(range) => format!(
                            "{} \u{2192} {}\nread from {}",
                            range.head_branch, range.base_branch, member.checkout_path
                        )
                        .into(),
                        None => member.checkout_path.clone().into(),
                    };
                    div()
                        .id(("changes-execution-repo", ix))
                        .debug_selector(move || format!("changes-execution-repo-{ix}"))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .when(true, |el| crate::components::changes_repository_chip(el, theme, active))
                        .cursor_pointer()
                        .text_size(px(11.0))
                        .text_color(if active { theme.text } else { theme.text_muted })
                        .hover(|el| el.bg(theme.element_hover))
                        .child(
                            crate::icons::icon(crate::icons::GIT_BRANCH)
                                .size(px(10.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from(member.label()))
                        .when(member.primary, |el| {
                            el.child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(theme.text_faint)
                                    .child("primary"),
                            )
                        })
                        .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tip.clone())).into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.select_chat_execution_member(&chat_id, &repo_id, cx)
                            });
                        }))
                }))
                .when(loading, |el| {
                    el.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child("Reading branches\u{2026}"),
                    )
                })
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .w_full()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(SharedString::from(error)),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_context_chrome(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (repo, worktree, branch, tooltip) = self.context_details(cx)?;
        let picker = self.render_checkout_picker(theme, cx);
        let ref_picker = self.render_ref_target_picker(theme, cx);
        let freshness = tooltip
            .rsplit_once('\n')
            .map(|(_, freshness)| freshness)
            .unwrap_or("Snapshot pending")
            .to_string();
        Some(
            div()
                .id("changes-context-chrome")
                .flex_none()
                .h(px(30.0))
                .px(px(Theme::SPACE_LG))
                .flex()
                .items_center()
                .gap(px(7.0))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .bg(crate::theme::wash(0.025))
                .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tooltip.clone().into())).into())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(SharedString::from(format!("{repo} / {worktree}"))),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(branch)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(freshness)),
                )
                .children(ref_picker)
                .children(picker)
                .into_any_element(),
        )
    }
}

/// Green for additions — sampled from the reference diff (soft emerald).
fn add_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_add // emerald-400
}

/// Red for deletions — softer than the theme danger, per the reference diff.
fn del_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_del // red-400
}

fn word_tint(line: &DiffLine, theme: &Theme) -> gpui::Hsla {
    match line.kind { LineKind::Add => add_color(theme), LineKind::Del => del_color(theme), _ => theme.text }.opacity(0.25)
}

/// One notice row ("New file", "Binary file — contents not shown", …).
fn notice_row(notice: String, theme: &Theme) -> AnyElement {
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
fn hunk_header_row(header: &str, theme: &Theme) -> AnyElement {
    div()
        .h(px(HUNK_HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .bg(theme.diff_hunk_bg)
        .font_family(theme.font_mono.clone())
        .text_size(px(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(header.to_string()))
        .into_any_element()
}

/// The only part of a diff row allowed to exceed its viewport. The outer
/// element keeps row chrome fixed; the inner element owns the intrinsic code
/// width and is the only plane moved by the file's horizontal scroll handle.
fn code_text_viewport(
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
fn diff_line_row(
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
fn meta_line_row(text: &str, theme: &Theme, pad_left: f32) -> AnyElement {
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
fn line_runs(
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
fn split_line_cell(
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
fn split_filler() -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .bg(crate::theme::ink(0.03))
}

/// Compose the two halves with the centre hairline.
fn split_row(left: AnyElement, right: AnyElement, wrapped: bool, theme: &Theme) -> gpui::Div {
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

fn positioned_adder(left: f32, adder: AnyElement) -> gpui::Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .h_full()
        .flex()
        .items_center()
        .child(adder)
}

fn render_comment_adder(
    path: &str,
    side: CommentSide,
    line: u32,
    theme: &Theme,
    cx: &Context<Changes>,
) -> AnyElement {
    let target = path.to_string();
    crate::comment_ui::render_comment_adder(
        format!("cmt-add-{path}-{}-{line}", side.tag()).into(),
        theme,
        cx,
        move |this, window, cx| this.open_draft(target.clone(), side, line, window, cx),
    )
}

/// Mirrors [`ReviewComment::cite_path`] for the not-yet-staged note.
fn draft_cite_path(draft: &CommentDraft) -> &str {
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
        children.push(hunk_header_row(&hunk.header, theme));
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
fn render_file_body_upto(
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
            children.push(hunk_header_row(&hunk.header, theme));
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
        let (rows, mut ranges, mut headers) = flatten_diff_tree(
            &files, &submodules, &layout, &[], None, changes.mode, |_| false, |_| false,
        );
        changes.word_spans.finish((key.clone(), 0, 0), Arc::new(word_diff::spans(&files[0].hunks[0].lines)));
        if let Some(gaps) = expand_context::gaps(&files[0], "// configuration\n// entry point\nfn main() {\n    new();\n}\n// end\n") {
            changes.expanded_context.entries.insert((key.clone(), 0), expand_context::Entry::ready(gaps, vec![2, 0]));
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
