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
mod horizontal_geometry;
mod patch_parser;
mod explanation;
mod ref_picker;
mod ref_and_checkout_menus;
mod rereview;
mod review_state;
mod review_drafts;
mod review_submit;
mod row_renderer;
mod scope_map;
mod syntax_excerpts;
mod split_pairing;
mod watch_lifecycle;
mod word_diff;
pub(crate) mod walkthrough;

use diff_painters::*;
pub use diff_painters::{comment_adder_left, split_adder_left};
pub(crate) use diff_painters::{render_file_body_with_syntax, COMMENT_ADDER_SIZE};
use split_pairing::*;
pub use split_pairing::{line_anchor, split_pairs, split_pairs_upto, LinePair};
use syntax_excerpts::*;
pub use patch_parser::{file_notices, parse_patch, truncate_file_lines};
#[cfg(test)]
use patch_parser::{parse_git_paths, parse_hunk_header};
use horizontal_geometry::*;
#[cfg(test)]
pub(crate) use horizontal_geometry::{CH_ADVANCE_LOOKUPS, SHAPED_LINES, reset_ch_advance_cache};
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

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

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
    file_tree_scroll: gpui::UniformListScrollHandle,
    scope_map_reveal: Option<(String, usize)>,
    file_list_width: f32,
    scope_map_selected: bool,
    scope_map_focus: FocusHandle,
    scope_toggle_focus: FocusHandle,
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
    review_state_generation: u64,
    /// Re-review mode: show what changed since the last-reviewed head.
    rereview: bool,
    /// A last-reviewed head that could not be fetched (GC'd); the full diff shows instead.
    rereview_unavailable: Option<String>,
    /// Hunk identities of the latest full (non re-review) diff and the head they belong to.
    full_identities: Option<(String, Arc<Vec<Vec<String>>>)>,
    /// Files of that same full diff: drafts always anchor against it, never against the delta.
    full_files: Option<(String, Arc<Vec<FileDiff>>)>,
    /// The completed scoped diff is a re-review range diff (not the full PR diff).
    scoped_is_since: bool,
    /// The PR head the completed scoped diff was requested at.
    scoped_head: Option<String>,
    /// A baseline stamped before the full diff for its head was known: (target, head) awaiting its snapshot.
    pending_snapshot: Option<(String, String)>,
    review_walkthrough: bool,
    scope_map_walkthrough: Option<gpui::WeakEntity<walkthrough::Walkthrough>>,
    scope_map_data: scope_map::data::Cache,
    scope_map_view: scope_map::view::State,
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
    submit_dialog: Option<review_submit::SubmitDialog>,
    /// The in-flight prepare/post/check lives here, not on the dialog, so closing or
    /// replacing the dialog never drops a reply.
    submit_task: Option<Task<()>>,
    submit_prepare_task: Option<Task<()>>,
    /// Review target with a post or check running.
    submit_inflight: Option<String>,
    /// The params of the post in flight, kept with the task so an unknown result is always
    /// checked even if the dialog was closed and reopened meanwhile.
    submit_params: Option<mast_proto::SubmitReviewParams>,
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
            file_tree_scroll: gpui::UniformListScrollHandle::new(),
            scope_map_reveal: None,
            file_list_width: settings.changes_file_list_width,
            scope_map_selected: settings.changes_scope_map,
            scope_map_focus: cx.focus_handle().tab_stop(true).tab_index(0),
            scope_toggle_focus: cx.focus_handle().tab_stop(true).tab_index(0),
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
            review_state_generation: 0,
            rereview: false,
            rereview_unavailable: None,
            full_identities: None,
            full_files: None,
            pending_snapshot: None,
            scoped_is_since: false,
            scoped_head: None,
            review_walkthrough: false,
            scope_map_walkthrough: None,
            scope_map_data: scope_map::data::Cache::default(),
            scope_map_view: scope_map::view::State::default(),
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
            submit_dialog: None,
            submit_task: None,
            submit_prepare_task: None,
            submit_inflight: None,
            submit_params: None,
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
                changes.refresh_scope_map_data();
                changes.reset_scope_map_layout();
                changes.note_full_identities(cx);
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
                                    .children((!self.review_tour).then(|| self.render_scope_sidebar(&theme, window, cx)))
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

/// A PR review the reviewer has already seen once: the baseline differs from
/// the head, one file is new since, and `rereview` selects the "Since" view.
#[cfg(feature = "snapshot")]
pub(crate) fn snapshot_rereview_changes(rereview: bool, cx: &mut App) -> Entity<Changes> {
    let changes = snapshot_populated_changes(cx);
    changes.update(cx, |changes, _| {
        changes.scope = DiffScope::Branch;
        let identities = changes.parsed.as_ref().map(|p| p.hunk_identities.clone()).unwrap_or_default();
        let mut state = mast_proto::DiffReviewState {
            target: "pr:example/project#42".into(),
            head_sha: Some("b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6".into()),
            ..Default::default()
        };
        stamp_reviewed(&mut state, "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", Some(&identities[1..]));
        state.reviewed_at = Some(chrono::Utc::now().timestamp_millis() - 2 * 86_400_000);
        state.viewed_hunks.extend(identities[1..].iter().flatten().cloned());
        changes.review_target = Some(state.target.clone());
        changes.review_state = Some(state);
        changes.rereview = rereview;
    });
    changes
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "perf/diff_bench.rs"]
mod perf_bench;
