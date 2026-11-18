//! The composer: a hand-rolled multiline text input (adapted from gpui's
//! `examples/input.rs`), the compact↔expanded flip, the Send/Queue/Stop morph,
//! optimistic send with failure recovery, per-chat drafts, and the question
//! wizard that replaces the composer while a run awaits input.
//!
//! Pure decision logic (flip, auto-grow math, button morph, wizard reducer,
//! pending-input detection) lives in free functions/structs with unit tests;
//! the gpui element only feeds them measurements.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyTooltip, App, BorderStyle, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle,
    DispatchPhase, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, KeyBinding, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PaintQuad, PathPromptOptions, Pixels, Point, Role,
    ScrollWheelEvent, SharedString, Style, StyledImage as _, Subscription, Task, TextRun,
    TextStyle, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, img, point,
    prelude::*, px, quad, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use mast_doc::{MessagePart, MessageRole, SessionCommandPayload, SessionMessageEntry};
use mast_proto::{
    FileSearchMatch, HarnessId, RunRequest, SandboxLevel, SlashCommand, UserInputAnswer,
    UserInputQuestion, capabilities,
};
use mast_rpc::{RpcError, methods};

use crate::appshots::{self, CapturedAppshot};

mod attachment_staging;
mod code_ref_chips;
mod file_mention_flow;
mod flip_morph;
mod input;
mod question_wizard;
pub use question_wizard::*;
pub use input::ComposerInput;
pub use flip_morph::*;
use flip_morph::model_handoff;
pub(crate) mod instruction_reset;
pub(crate) mod skill_completion;
pub(crate) mod skill_files;
use crate::attachments::{self, StagedAttachment};
use crate::motion;
use crate::notice::{NoticeChipIcon, notice_chip};
use crate::pickers::Pickers;
use crate::settings::{ComposerSendBehavior, platform_combo};
use crate::state::{AppState, Indicator};
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Constants + pure decision logic
// ---------------------------------------------------------------------------

/// Expanded-mode textarea vertical padding: `pt-4 pb-1` (zeron composer.tsx
/// line 578) = 16 + 4.
pub const TEXTAREA_PAD_V: f32 = 20.0;
/// The expanded textarea BOX (content + padding) is clamped by the original's
/// auto-grow effect: `ta.style.height = Math.min(Math.max(scrollHeight, 76),
/// 260)` (zeron composer.tsx line 235). The 76px floor applies even when
/// empty — it's what makes the always-expanded new-chat composer tall.
pub const TEXTAREA_MIN: f32 = 76.0;
pub const TEXTAREA_MAX: f32 = 260.0;
/// Expanded actions row: `pt-1` (4) + h-8 picker chips (32 — the tallest
/// children; composer/styles.tsx pickerChip) + `pb-2.5` (10) — zeron
/// composer-actions.tsx line 60.
pub const ACTIONS_ROW_HEIGHT: f32 = 46.0;
/// The pill's 1px hairline, top + bottom (`rounded-[26px] border`).
pub const PILL_BORDER_V: f32 = 2.0;
/// Corner radius shared by the composer and the queue tray behind it.
pub(crate) const COMPOSER_RADIUS: f32 = 26.0;
/// Expanded composer bounds, border-box: 76 + 46 + 2 = 124 when empty (the
/// new-chat canvas), 260 + 46 + 2 = 308 at the content cap.
pub const COMPOSER_MIN_HEIGHT: f32 = TEXTAREA_MIN + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
pub const COMPOSER_MAX_HEIGHT: f32 = TEXTAREA_MAX + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
/// Compact pill, border-box: one-line textarea `py-3` (24) + one 22.75px line
/// (scrollHeight rounds to 47 in the original) + the 2px hairline = 49. The
/// compact cluster (`py-1.5` + h-8 = 44) is shorter, so the textarea wins.
pub const COMPACT_TOTAL_HEIGHT: f32 = 49.0;
/// `max-w-3xl`: stable outer width of the centered composer column.
pub const COMPOSER_MAX_WIDTH: f32 = 768.0;
/// The queue reads as a narrower tray emerging from behind the composer.
const QUEUE_SIDE_INSET: f32 = 16.0;
/// The composer covers the tray's lower padding so the queue reads as emerging
/// from behind it instead of as a separate rounded pill.
pub(crate) const QUEUE_COMPOSER_OVERLAP: f32 = 18.0;
/// The original floating selector rows use the same 20px chip height as the
/// established-thread footer. Their surrounding rows own no plate or border.
const NEW_THREAD_SELECTOR_ROW_HEIGHT: f32 = 20.0;
// Accommodate the 24px usage indicator and PR badge without overflowing the
// row's equal 8px top/bottom gutters.
const SESSION_FOOTER_HEIGHT: f32 = 24.0;

/// Route chrome dissolves around the middle of the shared-element move. The
/// two ramps never overlap, which avoids duplicate picker ids/popovers while
/// still letting their surrounding geometry collapse continuously.
fn route_chrome_opacities(new_thread_chrome: f32) -> (f32, f32) {
    let new_thread = ((new_thread_chrome.clamp(0.0, 1.0) - 0.5) * 2.0).clamp(0.0, 1.0);
    let session = (((1.0 - new_thread_chrome.clamp(0.0, 1.0)) - 0.5) * 2.0).clamp(0.0, 1.0);
    (new_thread, session)
}
/// Ignore subpixel noise when the shell reports the conversation width.
const COMPOSER_WIDTH_EPSILON: f32 = 0.5;
/// Below this pill input width the composer always expands.
pub const MIN_COMPACT_INPUT_WIDTH: f32 = 200.0;
/// Input text metrics: `text-[14px] leading-relaxed` = 14 × 1.625 = 22.75.
pub const INPUT_LINE_HEIGHT: f32 = 22.75;
pub const INPUT_TEXT_SIZE: f32 = 14.0;
/// A compact ramp; the glyph-ascent inset keeps the clip edge invisible.
const INPUT_FADE_BAND: f32 = 12.0;
/// Single-select questions auto-advance after this long.
pub const AUTO_ADVANCE_MS: u64 = 220;
/// Drag-selection autoscroll runs at the display-friendly 60fps cadence.
pub const DRAG_SCROLL_FRAME_MS: u64 = 16;

/// Hysteresis slack for the expanded→compact flip: once expanded, the composer
/// only collapses when the text is comfortably narrower than the compact
/// capacity — expanding and collapsing share no boundary, so a width right at
/// the flip threshold can't oscillate between the two layouts.
pub const COLLAPSE_HYSTERESIS: f32 = 32.0;
/// During an interactive resize, collapsing back to the compact mode waits
/// until the measured widths have been stable this long. Expansion remains
/// immediate so a narrowing panel never traps the controls in a compact row.
pub const RESIZE_SETTLE_MS: u64 = 150;

/// Compact↔expanded flip with hysteresis. `capacity` is the *compact-mode*
/// input capacity (a layout-stable width: measured while compact, tracked by
/// container-width deltas while expanded — never the post-flip measured width,
/// which differs per mode and would feed back into the decision):
/// - a newline always expands;
/// - while `resizing`, an expanded composer stays expanded until sizes settle;
/// - a too-narrow pill (`capacity < MIN_COMPACT_INPUT_WIDTH`) always expands;
/// - compact expands only when `text_width > capacity`; expanded collapses
///   only when `text_width < capacity - COLLAPSE_HYSTERESIS`.
pub fn composer_flip(
    expanded: bool,
    text_width: f32,
    capacity: f32,
    has_newline: bool,
    resizing: bool,
) -> bool {
    if has_newline {
        return true;
    }
    if capacity < MIN_COMPACT_INPUT_WIDTH {
        return true;
    }
    if expanded {
        resizing || text_width >= capacity - COLLAPSE_HYSTERESIS
    } else {
        text_width > capacity
    }
}

fn composer_width_changed(previous: Option<f32>, current: f32) -> bool {
    previous.is_none_or(|previous| (current - previous).abs() > COMPOSER_WIDTH_EPSILON)
}

/// Caret blink half-period (standard textarea cadence: ~500ms on / 500ms off).
pub const CARET_BLINK_MS: u64 = 500;

/// Caret blink phase for a time since the last keystroke/caret move: solid
/// through the first half-period (typing bursts never blink — each keystroke
/// resets the phase), then alternating.
pub fn caret_visible(ms_since_activity: u64) -> bool {
    (ms_since_activity / CARET_BLINK_MS).is_multiple_of(2)
}

/// Auto-grow: content height for a wrapped-line count.
pub fn input_content_height(wrapped_lines: usize) -> f32 {
    wrapped_lines.max(1) as f32 * INPUT_LINE_HEIGHT
}

/// [`ComposerInput::with_auto_grow`]: the editor never shows fewer rows.
pub const AUTO_GROW_MIN_LINES: usize = 3;
/// [`ComposerInput::with_auto_grow`]: the share of the window height the
/// editor may grow to before it scrolls internally.
pub const AUTO_GROW_MAX_FRACTION: f32 = 0.4;

/// The content cap of an auto-growing editor in a `window_height` window:
/// [`AUTO_GROW_MAX_FRACTION`] of it, rounded down to whole rows so the last
/// visible row is never sliced, and never below [`AUTO_GROW_MIN_LINES`].
pub fn auto_grow_cap(window_height: f32, line_height: f32) -> f32 {
    let line_height = line_height.max(1.0);
    let rows = ((window_height * AUTO_GROW_MAX_FRACTION) / line_height).floor();
    rows.max(AUTO_GROW_MIN_LINES as f32) * line_height
}

/// Total expanded composer height (border-box) for a content height: the
/// textarea BOX (content + `pt-4 pb-1`) clamps to 76–260 exactly like the
/// original's auto-grow effect, then the 46px actions row and the hairline
/// ride on top. Range 124–308.
pub fn composer_total_height(content_height: f32) -> f32 {
    (content_height + TEXTAREA_PAD_V).clamp(TEXTAREA_MIN, TEXTAREA_MAX)
        + ACTIONS_ROW_HEIGHT
        + PILL_BORDER_V
}

fn input_max_scroll(content_height: f32, viewport_height: f32) -> f32 {
    (content_height - viewport_height).max(0.0)
}

/// Only settled overflow gets a scroll fade. The animated viewport can be
/// smaller for a few frames while an otherwise fitting draft grows into it.
fn input_overflow_edges(
    content_height: f32,
    settled_height: f32,
    visible_height: f32,
    scroll_top: f32,
) -> (bool, bool) {
    if input_max_scroll(content_height, settled_height) <= 1.0 {
        return (false, false);
    }
    let max_scroll = input_max_scroll(content_height, visible_height);
    (scroll_top > 1.0, scroll_top < max_scroll - 1.0)
}

/// During the reveal, stop at a complete row boundary instead of slicing
/// glyphs with a moving clip. Scrolling offsets the row grid inside the box.
/// Keeps a drag-selection pointer inside the text box. A collapsed box (under
/// 0.5px) would put the upper limit below the lower one, and `clamp` panics on
/// that, so the upper limit never drops below the lower.
fn input_drag_position(position: Point<Pixels>, bounds: Bounds<Pixels>) -> Point<Pixels> {
    point(
        position
            .x
            .clamp(bounds.left(), (bounds.right() - px(0.5)).max(bounds.left())),
        position
            .y
            .clamp(bounds.top(), (bounds.bottom() - px(0.5)).max(bounds.top())),
    )
}

fn input_reveal_height(visible: f32, scroll: f32, line_height: f32, resizing: bool) -> f32 {
    if !resizing {
        return visible;
    }
    let row_end = ((scroll + visible + 0.001) / line_height).floor() * line_height;
    (row_end - scroll).clamp(0.0, visible.max(0.0))
}

/// Apply GPUI's wheel delta to a top-origin input offset. Positive deltas mean
/// scrolling toward the start, matching gpui's built-in list/div behavior.
fn input_scroll_offset(
    current: f32,
    delta_y: f32,
    content_height: f32,
    viewport_height: f32,
) -> f32 {
    (current - delta_y).clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Minimally adjust the viewport so the caret row is fully visible.
fn input_scroll_offset_for_cursor(
    current: f32,
    cursor_top: f32,
    cursor_height: f32,
    content_height: f32,
    viewport_height: f32,
    settled_height: Option<f32>,
) -> f32 {
    // Resize the reveal, not the scroll position: existing text stays fixed
    // relative to the input origin throughout the height animation.
    let viewport_height = settled_height.unwrap_or(viewport_height);
    let mut next = current;
    if cursor_top < next {
        next = cursor_top;
    } else if cursor_top + cursor_height > next + viewport_height {
        next = cursor_top + cursor_height - viewport_height;
    }
    next.clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// What a mouse press in a text field asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressIntent {
    /// Take the whole field.
    SelectAll,
    /// Grow the current selection to the pressed position.
    ExtendSelection,
    /// Put the caret at the pressed position.
    PlaceCaret,
}

impl PressIntent {
    /// Whether the press starts a drag selection. A select-all must not, or
    /// the next mouse move shrinks it back to a drag from the press position.
    fn arms_drag(self) -> bool {
        !matches!(self, Self::SelectAll)
    }
}

/// Read the intent from the press. Two clicks or more take the whole field,
/// and every further click keeps it, so holding the button through a third
/// click does not change what is selected.
fn press_intent(click_count: usize, shift: bool) -> PressIntent {
    if click_count >= 2 {
        PressIntent::SelectAll
    } else if shift {
        PressIntent::ExtendSelection
    } else {
        PressIntent::PlaceCaret
    }
}

/// Per-frame drag-selection scroll. Distance increases speed, capped at one
/// text row per frame so crossing the input boundary never causes a jump.
fn input_drag_scroll_delta(
    pointer_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    line_height: f32,
) -> f32 {
    let distance = if pointer_y < viewport_top {
        pointer_y - viewport_top
    } else if pointer_y > viewport_bottom {
        pointer_y - viewport_bottom
    } else {
        return 0.0;
    };
    distance.signum() * (distance.abs() * 0.2).clamp(1.0, line_height.max(1.0))
}

/// Staged-attachment strip metrics (zeron attachment-ui.tsx AttachmentStrip:
/// `flex flex-wrap gap-2 px-4 pt-3`, `size-14` thumbs).
pub const STRIP_THUMB: f32 = 56.0;
pub const STRIP_GAP: f32 = 8.0;
pub const STRIP_PAD_TOP: f32 = 12.0;
pub const STRIP_PAD_X: f32 = 16.0;

/// Height the wrap strip adds to the pill for `count` staged thumbnails at an
/// `inner_width` pill content width (0 when empty). Mirrors flex-wrap: as many
/// 56px thumbs per row as fit with 8px gaps inside the 16px side insets.
pub fn attachment_strip_height(count: usize, inner_width: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let usable = (inner_width - 2.0 * STRIP_PAD_X).max(STRIP_THUMB);
    let per_row = (((usable + STRIP_GAP) / (STRIP_THUMB + STRIP_GAP)).floor() as usize).max(1);
    let rows = count.div_ceil(per_row);
    STRIP_PAD_TOP + rows as f32 * STRIP_THUMB + (rows - 1) as f32 * STRIP_GAP
}

pub fn comment_strip_height(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    STRIP_PAD_TOP + crate::badges::BADGE_HEIGHT
}

/// Engines at or above this version understand `pending://` attachment refs
/// and QueueCommand `transfers` (send-is-a-local-write attachments). Gated on
/// BOTH the local engine (an IPC daemon may be older than this UI) and, for
/// remotely-hosted chats, the host device's stamped registry version.
const QUEUED_ATTACHMENTS_MIN: (u64, u64, u64) = (0, 2, 12);

/// What the send button is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendButtonMode {
    /// No live run: plain send.
    Send,
    /// Live run with text typed: queue for the next turn.
    Queue,
    /// Live run, nothing typed: red stop square.
    Stop,
}

/// What the composer holds that a send could carry. A staged image or diff
/// comment counts: both synthesize their own prompt body, so either alone is
/// a legal send — and during a live run has to read as Queue, not Stop.
pub fn composer_has_content(text: &str, attachments: usize, comments: usize) -> bool {
    !text.trim().is_empty() || attachments > 0 || comments > 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModifiedSubmitTarget {
    SubmitContent,
    ActivateLatestQueued,
}

fn modified_submit_target(has_content: bool) -> ModifiedSubmitTarget {
    if has_content {
        ModifiedSubmitTarget::SubmitContent
    } else {
        ModifiedSubmitTarget::ActivateLatestQueued
    }
}

pub const APPSHOT_TILE_MIN_WIDTH: f32 = 96.0;
pub const APPSHOT_IMAGE_INSET: f32 = 12.0;
pub const APPSHOT_PREVIEW_HEIGHT: f32 = 148.0;
pub const APPSHOT_IMAGE_MAX_WIDTH: f32 = 320.0;
pub const APPSHOT_IMAGE_MAX_HEIGHT: f32 = 132.0;
pub const APPSHOT_TILE_HEIGHT: f32 = 192.0;

struct AppshotActionTooltip(SharedString);

impl Render for AppshotActionTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .text_size(px(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// Give ordinary captures a shared height while their width follows the
/// source window. Extreme panoramas and narrow composers cap width without
/// cropping. Explicit dimensions also bound the native image while decoding.
pub fn appshot_contained_size(dimensions: Option<(u32, u32)>, max_width: f32) -> (f32, f32) {
    let max_width = if max_width.is_finite() {
        max_width.clamp(1.0, APPSHOT_IMAGE_MAX_WIDTH)
    } else {
        APPSHOT_IMAGE_MAX_WIDTH
    };
    let (width, height) = dimensions
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((16, 10));
    let scale = (max_width / width as f32).min(APPSHOT_IMAGE_MAX_HEIGHT / height as f32);
    (width as f32 * scale, height as f32 * scale)
}

pub fn appshot_strip_height(count: usize) -> f32 {
    if count == 0 {
        0.0
    } else {
        STRIP_PAD_TOP + APPSHOT_TILE_HEIGHT
    }
}

pub fn send_button_mode(run_live: bool, has_text: bool) -> SendButtonMode {
    match (run_live, has_text) {
        (false, _) => SendButtonMode::Send,
        (true, true) => SendButtonMode::Queue,
        (true, false) => SendButtonMode::Stop,
    }
}

/// Queue rows are represented by the queue panel until the host promotes them
/// into the transcript. They must never publish (or refresh) a local echo.
fn should_publish_optimistic_echo(queue: bool) -> bool {
    !queue
}

fn begin_interrupt(pending: &mut HashSet<String>, chat_id: &str) -> bool {
    pending.insert(chat_id.to_string())
}

fn retain_live_interrupts(pending: &mut HashSet<String>, mut is_live: impl FnMut(&str) -> bool) {
    pending.retain(|chat_id| is_live(chat_id));
}

fn interrupt_params(chat_id: &str) -> serde_json::Value {
    serde_json::json!({
        "chatId": chat_id,
        "command": { "kind": "interrupt" },
    })
}

fn escape_dismisses_completion(key: &str, completion_open: bool) -> bool {
    key == "escape" && completion_open
}

fn wizard_escape_goes_back(key: &str, input_focused: bool, input_empty: bool) -> bool {
    key == "escape" && (!input_focused || input_empty)
}

// ---------------------------------------------------------------------------
// Multiline text input (adapted from gpui examples/input.rs)
// ---------------------------------------------------------------------------

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Copy,
        Cut,
        Paste,
        Newline,
        MessageNewlineOrAccept,
        ModifiedSubmit,
        Submit,
        Undo,
        Redo,
        MentionTab,
    ]
);

/// How long a run of single-character edits keeps merging into one undo step.
/// A pause longer than this starts a fresh step, so undo rewinds in the
/// bursts the user actually typed rather than one character at a time.
const UNDO_COALESCE: Duration = Duration::from_millis(700);

/// Cap on retained undo steps — a long-lived composer must not grow forever.
const UNDO_LIMIT: usize = 200;

/// The literal `@` a chip displays before its file name. Projected as TEXT so
/// it shapes, wraps, and hit-tests with the label — the earlier SVG icons
/// painted into a reserved whitespace slot never sat right at text size
/// (user report). Chips read as inline code: `@name` in the mono font over
/// the code wash.
const MENTION_PREFIX: char = '@';
const MENTION_TOOLTIP_DELAY: Duration = Duration::from_millis(420);
const MENTION_TOOLTIP_HEIGHT: f32 = 24.0;
const MENTION_SIDE_PAD: &str = "\u{00A0}";
/// A private URI scheme keeps file mentions distinguishable from ordinary
/// Markdown links pasted into the composer.
use crate::workspace_links::{FILE_MENTION_SCHEME, LEGACY_FILE_MENTION_SCHEME, strip_file_scheme};

/// A restorable point in the input's history: text plus where the caret and
/// selection sat when the edit landed.
#[derive(Clone)]
struct EditSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

/// A strict, local-only Markdown representation of a file mention. The
/// underlying prompt always contains this form; the editor projects it to a
/// chip for display without leaking a second data model into submission.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileMentionLink {
    range: Range<usize>,
    basename: String,
    path: String,
    is_dir: bool,
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn percent_decode_path(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let raw = encoded.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn escape_mention_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn local_file_link(path: &str, is_dir: bool) -> String {
    let path = path.trim_end_matches('/');
    let basename = path
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(path);
    format!(
        "[{}]({}{})",
        escape_mention_label(basename),
        FILE_MENTION_SCHEME,
        percent_encode_path(&format!("{path}{}", if is_dir { "/" } else { "" }))
    )
}

/// Build the text inserted when a workspace item is dropped at an arbitrary
/// selection. Unlike completion, a drop does not necessarily happen at a
/// token boundary, so it supplies its own leading separator when needed.
fn dropped_file_mention(
    content: &str,
    range: Range<usize>,
    path: &str,
    is_dir: bool,
) -> Option<(String, usize)> {
    if range.start > range.end
        || !(local_path_is_safe(path) || absolute_path_is_safe(path))
        || !content.is_char_boundary(range.start)
    {
        return None;
    }
    let suffix = content.get(range.end..)?;
    let prefix = if range.start > 0
        && content[..range.start]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace())
    {
        " "
    } else {
        ""
    };
    let existing_separator = suffix
        .chars()
        .next()
        .filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
    let trailing = if existing_separator.is_some() {
        ""
    } else {
        " "
    };
    let reference = if absolute_path_is_safe(path) {
        format!("`{path}{}`", if is_dir { "/" } else { "" })
    } else {
        local_file_link(path, is_dir)
    };
    let inserted = format!("{prefix}{reference}{trailing}");
    let cursor_advance = inserted.len() + existing_separator.map(char::len_utf8).unwrap_or(0);
    Some((inserted, cursor_advance))
}

fn local_path_is_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn absolute_path_is_safe(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('`')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && local_path_is_safe(&path[1..])
}

fn label_close(text: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (at, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ']' && text[start + at + 1..].starts_with('(') {
            return Some(start + at);
        }
    }
    None
}

fn file_mention_links(text: &str) -> Vec<FileMentionLink> {
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(relative_start) = text[search..].find('[') {
        let start = search + relative_start;
        let Some(label_end) = label_close(text, start + 1) else {
            search = start + 1;
            continue;
        };
        let target_start = label_end + 2;
        let Some(relative_end) = text[target_start..].find(')') else {
            search = start + 1;
            continue;
        };
        let end = target_start + relative_end + 1;
        let label = &text[start + 1..label_end];
        let Some(encoded) = strip_file_scheme(&text[target_start..end - 1]) else {
            search = end;
            continue;
        };
        let parsed = percent_decode_path(encoded).and_then(|target| {
            let is_dir = target.ends_with('/');
            let path = target.strip_suffix('/').unwrap_or(&target);
            (local_path_is_safe(path)
                && percent_encode_path(&target) == encoded
                && path
                    .rsplit('/')
                    .next()
                    .is_some_and(|basename| escape_mention_label(basename) == label))
            .then(|| (path.to_string(), is_dir))
        });
        if let Some((path, is_dir)) = parsed {
            let basename = path.rsplit('/').next().unwrap_or_default().to_string();
            links.push(FileMentionLink {
                range: start..end,
                basename,
                path,
                is_dir,
            });
        }
        search = end;
    }
    links
}

#[derive(Debug, Clone, Default)]
struct TextProjection {
    display: String,
    mentions: Vec<(FileMentionLink, Range<usize>)>,
}

/// A path alone is not enough: two identical relative paths can appear in a
/// draft, so the raw range remains part of the hover identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionTooltipTarget {
    range: Range<usize>,
    path: SharedString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MentionTooltipPhase {
    Hidden,
    Waiting {
        target: MentionTooltipTarget,
        generation: u64,
    },
    Visible {
        target: MentionTooltipTarget,
        generation: u64,
    },
}

impl MentionTooltipPhase {
    fn target(&self) -> Option<&MentionTooltipTarget> {
        match self {
            Self::Hidden => None,
            Self::Waiting { target, .. } | Self::Visible { target, .. } => Some(target),
        }
    }
}

/// Pure tooltip lifecycle reducer. Motion within the same chip preserves both
/// waiting and visible phases, so normal pointer jitter cannot starve the
/// delay or flicker an already-visible tooltip.
fn mention_tooltip_reduce(
    phase: MentionTooltipPhase,
    pointer_target: Option<MentionTooltipTarget>,
    pointer_in_popup: bool,
    generation: u64,
) -> MentionTooltipPhase {
    match pointer_target {
        Some(target) if phase.target() == Some(&target) => phase,
        Some(target) => MentionTooltipPhase::Waiting { target, generation },
        None if pointer_in_popup && matches!(phase, MentionTooltipPhase::Visible { .. }) => phase,
        None => MentionTooltipPhase::Hidden,
    }
}

fn mention_tooltip_promote(
    phase: MentionTooltipPhase,
    generation: u64,
    target_is_live: bool,
) -> MentionTooltipPhase {
    match phase {
        MentionTooltipPhase::Waiting {
            target,
            generation: current,
        } if current == generation && target_is_live => MentionTooltipPhase::Visible {
            target,
            generation: current,
        },
        MentionTooltipPhase::Waiting {
            generation: current,
            ..
        } if current == generation => MentionTooltipPhase::Hidden,
        phase => phase,
    }
}

fn mention_tooltip_contains(in_chip: bool, in_popup: bool) -> bool {
    in_chip || in_popup
}

fn display_row_segments(
    range: Range<usize>,
    row_ends: impl IntoIterator<Item = usize>,
) -> Vec<(usize, usize, Range<usize>)> {
    let mut segments = Vec::new();
    let mut row_start = 0usize;
    for (row_ix, row_end) in row_ends.into_iter().enumerate() {
        let start = range.start.max(row_start);
        let end = range.end.min(row_end);
        if start < end {
            segments.push((row_ix, row_start, start..end));
        }
        row_start = row_end;
        if row_start >= range.end {
            break;
        }
    }
    segments
}

#[derive(Debug, Clone)]
struct MentionHit {
    target: MentionTooltipTarget,
    bounds: Bounds<Pixels>,
    anchor: Point<Pixels>,
}

impl TextProjection {
    fn new(raw: &str) -> Self {
        let links = file_mention_links(raw);
        let labels = mention_display_labels(&links);
        let mut projection = Self::default();
        let mut raw_at = 0;
        for (link, label) in links.into_iter().zip(labels) {
            projection.display.push_str(&raw[raw_at..link.range.start]);
            let display_start = projection.display.len();
            // The chip is plain projected text — `@` plus the label between
            // non-breaking side bearings; the rounded code wash beneath it is
            // painted by `ComposerTextElement::paint`. Every character here
            // must exist in Geist (no exotic whitespace — U+2003/U+202F shape
            // at fallback width and collapsed the chip once already).
            projection.display.push_str(MENTION_SIDE_PAD);
            projection.display.push(MENTION_PREFIX);
            for ch in label.chars() {
                projection
                    .display
                    .push(if ch == ' ' { '\u{00A0}' } else { ch });
            }
            projection.display.push('\u{00A0}');
            let display_end = projection.display.len();
            projection
                .mentions
                .push((link.clone(), display_start..display_end));
            raw_at = link.range.end;
        }
        projection.display.push_str(&raw[raw_at..]);
        projection
    }

    fn raw_to_display(&self, raw: usize) -> usize {
        let mut raw_at = 0;
        let mut display_at = 0;
        for (link, display) in &self.mentions {
            if raw <= link.range.start {
                return display_at + raw.saturating_sub(raw_at);
            }
            if raw < link.range.end {
                return display.start;
            }
            raw_at = link.range.end;
            display_at = display.end;
        }
        display_at + raw.saturating_sub(raw_at)
    }

    fn display_to_raw(&self, display_offset: usize) -> usize {
        let mut raw_at = 0;
        let mut display_at = 0;
        for (link, display) in &self.mentions {
            if display_offset <= display.start {
                return raw_at + display_offset.saturating_sub(display_at);
            }
            if display_offset < display.end {
                return if display_offset - display.start < display.len() / 2 {
                    link.range.start
                } else {
                    link.range.end
                };
            }
            raw_at = link.range.end;
            display_at = display.end;
        }
        raw_at + display_offset.saturating_sub(display_at)
    }

    fn normalize_range(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            for (link, _) in &self.mentions {
                if link.range.start < range.start && range.start < link.range.end {
                    let midpoint = link.range.start + link.range.len() / 2;
                    let at = if range.start < midpoint {
                        link.range.start
                    } else {
                        link.range.end
                    };
                    return at..at;
                }
            }
            return range;
        }
        let mut normalized = range;
        for (link, _) in &self.mentions {
            if normalized.start < link.range.end && normalized.end > link.range.start {
                normalized.start = normalized.start.min(link.range.start);
                normalized.end = normalized.end.max(link.range.end);
            }
        }
        normalized
    }

    fn previous_boundary(&self, raw: usize) -> Option<usize> {
        self.mentions
            .iter()
            .find_map(|(link, _)| (raw == link.range.end).then_some(link.range.start))
    }

    fn next_boundary(&self, raw: usize) -> Option<usize> {
        self.mentions
            .iter()
            .find_map(|(link, _)| (raw == link.range.start).then_some(link.range.end))
    }
}

/// Basenames are compact in the common case. When the same basename appears
/// more than once, use the shortest unique path suffix so chips remain
/// distinguishable without always expanding to full paths.
fn mention_display_labels(links: &[FileMentionLink]) -> Vec<String> {
    links
        .iter()
        .enumerate()
        .map(|(ix, link)| {
            if links
                .iter()
                .filter(|other| other.basename == link.basename)
                .count()
                == 1
            {
                return link.basename.clone();
            }
            let parts: Vec<_> = link.path.split('/').collect();
            (1..=parts.len())
                .map(|count| parts[parts.len() - count..].join("/"))
                .find(|suffix| {
                    let suffix: Vec<_> = suffix.split('/').collect();
                    links.iter().enumerate().all(|(other_ix, other)| {
                        other_ix == ix
                            || !other
                                .path
                                .split('/')
                                .rev()
                                .take(suffix.len())
                                .eq(suffix.iter().rev().copied())
                    })
                })
                .unwrap_or_else(|| link.path.clone())
        })
        .collect()
}

/// One chip in a *sent* message: its byte range over the projected display
/// string (`@label` between side bearings). The transcript renders these
/// read-only — no editing state, no tooltip machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMentionSpan {
    pub range: Range<usize>,
    pub badge_range: Option<Range<usize>>,
    /// Full workspace-relative path (labels can be shortened to basenames).
    pub path: SharedString,
    pub is_dir: bool,
    pub code_ref: Option<std::sync::Arc<crate::code_ref::StagedCodeRef>>,
    pub resolution_key: Option<SharedString>,
    pub resolution: Option<crate::code_ref::Resolution>,
}

/// Project a sent message's raw Markdown for transcript display: mention links
/// collapse to the same chip labels the composer shows, everything else passes
/// through untouched. `None` when the text has no valid mention — the
/// substring probe keeps ordinary prompts on the zero-allocation path, so this
/// is safe to call for every user row.
pub fn sent_mention_display(raw: &str) -> Option<(String, Vec<SentMentionSpan>)> {
    if !raw.contains(FILE_MENTION_SCHEME) && !raw.contains(LEGACY_FILE_MENTION_SCHEME) {
        return None;
    }
    let projection = TextProjection::new(raw);
    if projection.mentions.is_empty() {
        return None;
    }
    let spans = projection
        .mentions
        .iter()
        .map(|(link, display)| SentMentionSpan {
            range: display.clone(),
            badge_range: None,
            path: SharedString::from(format!(
                "{}{}",
                link.path,
                if link.is_dir { "/" } else { "" }
            )),
            is_dir: link.is_dir,
            code_ref: None,
            resolution_key: None,
            resolution: None,
        })
        .collect();
    Some((projection.display, spans))
}

/// Direction of the last edit — a run only merges with edits of its own kind.
#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Insert,
    Delete,
}

const GENERIC_COMPOSER_CONTEXT: &str = "Composer";
const MESSAGE_COMPOSER_CONTEXT: &str = "MessageComposer";
/// Key context for palettes that reserve the arrow keys for navigation.
pub(crate) const PALETTE_SEARCH_CONTEXT: &str = "PaletteSearch";
pub(crate) const WORKSPACE_SEARCH_CONTEXT: &str = "WorkspaceSearch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageEnterBindingAction {
    Submit,
    ModifiedSubmit,
    NewlineOrAccept,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageEnterBinding {
    keystroke: String,
    action: MessageEnterBindingAction,
}

fn message_enter_bindings(
    behavior: ComposerSendBehavior,
    modifier_combo: &str,
) -> Vec<MessageEnterBinding> {
    match behavior {
        ComposerSendBehavior::Enter => vec![
            MessageEnterBinding {
                keystroke: "enter".into(),
                action: MessageEnterBindingAction::Submit,
            },
            MessageEnterBinding {
                keystroke: modifier_combo.into(),
                action: MessageEnterBindingAction::ModifiedSubmit,
            },
        ],
        ComposerSendBehavior::ModEnter => vec![
            MessageEnterBinding {
                keystroke: "enter".into(),
                action: MessageEnterBindingAction::NewlineOrAccept,
            },
            MessageEnterBinding {
                keystroke: modifier_combo.into(),
                action: MessageEnterBindingAction::ModifiedSubmit,
            },
        ],
    }
}

fn message_input_context(wizard_active: bool) -> &'static str {
    if wizard_active {
        GENERIC_COMPOSER_CONTEXT
    } else {
        MESSAGE_COMPOSER_CONTEXT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterOutcome {
    AcceptCompletion,
    Submit,
    Newline,
}

fn enter_outcome(has_completion: bool, fallback: EnterOutcome) -> EnterOutcome {
    if has_completion {
        EnterOutcome::AcceptCompletion
    } else {
        fallback
    }
}

fn input_bindings(context: &'static str, result_navigation: bool) -> Vec<KeyBinding> {
    let ctx = Some(context);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // macOS line/document motion — a laptop keyboard has no home/end keys,
        // so Cmd+arrow is the only way users reach either edge.
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("cmd-up", DocStart, ctx),
        KeyBinding::new("cmd-down", DocEnd, ctx),
        KeyBinding::new("shift-cmd-left", SelectHome, ctx),
        KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
        KeyBinding::new("shift-cmd-up", SelectDocStart, ctx),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, ctx),
        // Line-edge deletion (Cmd+Delete on macOS).
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
    ];
    if !result_navigation {
        bindings.push(KeyBinding::new("tab", MentionTab, ctx));
        bindings.push(KeyBinding::new("shift-enter", Newline, ctx));
        bindings.push(KeyBinding::new("up", Up, ctx));
        bindings.push(KeyBinding::new("down", Down, ctx));
    }
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, ctx));
        bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, ctx));
    }
    // Word-level editing: Option on macOS, Ctrl on Windows/Linux.
    let word_edit_prefix = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        ctx,
    ));
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, ctx));
    }
    bindings
}

/// Bind the composer keymap. Call once at app boot.
pub fn init(cx: &mut App, send_behavior: ComposerSendBehavior) {
    let mut generic_bindings = input_bindings(GENERIC_COMPOSER_CONTEXT, false);
    generic_bindings.push(KeyBinding::new(
        "enter",
        Submit,
        Some(GENERIC_COMPOSER_CONTEXT),
    ));

    let mut message_bindings = input_bindings(MESSAGE_COMPOSER_CONTEXT, false);
    for binding in message_enter_bindings(send_behavior, &platform_combo("mod-enter")) {
        match binding.action {
            MessageEnterBindingAction::Submit => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                Submit,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
            MessageEnterBindingAction::ModifiedSubmit => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                ModifiedSubmit,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
            MessageEnterBindingAction::NewlineOrAccept => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                MessageNewlineOrAccept,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
        }
    }

    let word_edit_prefix = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    // Palette-search context: TEXT-EDITING keys only. gpui dispatches matched
    // keybindings BEFORE raw key listeners (window.rs `dispatch_key_event`),
    // so anything bound here can never reach a palette's `on_key_down` —
    // navigation keys (up/down/left/right/enter) are deliberately unbound and
    // bubble to the palette frame instead.
    let palette = Some(PALETTE_SEARCH_CONTEXT);
    let mut palette_bindings = vec![
        KeyBinding::new("backspace", Backspace, palette),
        KeyBinding::new("delete", Delete, palette),
        KeyBinding::new("home", Home, palette),
        KeyBinding::new("end", End, palette),
        KeyBinding::new("shift-left", SelectLeft, palette),
        KeyBinding::new("shift-right", SelectRight, palette),
        // Modifier-qualified motion is safe here: the palette's own navigation
        // uses BARE arrows/enter, which stay unbound and bubble to its frame.
        KeyBinding::new("cmd-left", Home, palette),
        KeyBinding::new("cmd-right", End, palette),
        KeyBinding::new("shift-cmd-left", SelectHome, palette),
        KeyBinding::new("shift-cmd-right", SelectEnd, palette),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, palette),
    ];
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        palette,
    ));
    for prefix in ["cmd", "ctrl"] {
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, palette));
        palette_bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, palette));
    }
    cx.bind_keys(palette_bindings);
    cx.bind_keys(input_bindings(WORKSPACE_SEARCH_CONTEXT, true));
    cx.bind_keys(generic_bindings);
    cx.bind_keys(message_bindings);
}

/// Events the composer wrapper listens for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerInputEvent {
    Submitted,
    ModifiedSubmitted,
    Edited,
    CursorMoved,
    ViewportChanged,
    MentionNavigate(isize),
    MentionAccept,
    /// The secondary accept (Cmd/Ctrl+Enter) on an open completion with a
    /// selection. The slash popup ATTACHES the row as a skill chip; other
    /// completions treat it exactly like [`Self::MentionAccept`].
    MentionAttach,
    MentionDismiss,
    /// Images pasted from the clipboard (screenshots / copied image data) —
    /// the wrapper stages them as attachments (use-attachments.ts onPaste).
    PastedImages(Vec<gpui::Image>),
    /// File paths pasted from the clipboard (a file manager "Copy").
    PastedPaths(Vec<PathBuf>),
}

// ---------------------------------------------------------------------------
// Composer wrapper
// ---------------------------------------------------------------------------

/// Events the shell listens for.
#[derive(Debug, Clone)]
pub enum ComposerEvent {
    SignIn { device: String, harness: mast_proto::HarnessId, account: Option<String>, snapshot: mast_proto::ManagedAccountsSnapshot },
    /// Arm the shared-element transition before the draft route is replaced
    /// by the newly-created session. Emitting this before `select_chat` keeps
    /// the first destination frame on the same timeline as the source frame.
    NewThreadTransitionStarted,
    /// A prompt was sent optimistically — give the transcript its exact row
    /// identity so it can anchor the prompt at the top with the reply's
    /// reserved space below it.
    Sent { chat_id: String, message_id: String },
    /// A new worktree's host-side setup attempt completed after its chat id
    /// was minted. The shell attaches an already-open terminal to that exact
    /// chat, even when the user has selected another chat in the meantime.
    WorktreeSetup {
        chat_id: String,
        setup_action: Option<mast_proto::ProjectActionRun>,
        setup_error: Option<String>,
        target_device_id: Option<String>,
    },
    /// A locally-authored queue row was accepted. It is not a transcript send
    /// yet: the transcript remembers the stable id and promotes it to an
    /// own-turn anchor only when the host materializes the matching bubble.
    Queued { chat_id: String, message_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionToken {
    range: Range<usize>,
    query: String,
}

/// The `@` must begin a token. This intentionally excludes `name@example.com`
/// and ordinary words while allowing punctuation such as `(@src`.
fn mention_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token_start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    let relative_at = text[token_start..cursor].rfind('@')?;
    let at = token_start + relative_at;
    let valid_boundary = at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{'));
    if text[at + 1..cursor].contains('@') || !valid_boundary {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    Some(MentionToken {
        range: at..end,
        query: text[at + 1..cursor].to_string(),
    })
}

/// The `/` must begin a whitespace-delimited token — the start of the input
/// or anywhere after whitespace. Offset 0 is the whole-prompt command
/// (`/compact`, `/goal ship it`) the CLI expands natively; mid-message
/// tokens are plain-text skill mentions (`fix this /code-review`) the model
/// reads and loads via its Skill tool. Either way the popup offers the same
/// list and acceptance replaces the token with `/name `.
///
/// Guards, all per-token: a token containing another `/` (a typed path such
/// as `/usr/bin` or `see /tmp/x`) never triggers, nor does one containing
/// `@` (that shape belongs to the file-mention popup); the cursor must sit
/// inside the token after its `/`.
fn slash_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    if !text[start..].starts_with('/') || cursor == start {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    let body = &text[start + 1..end];
    if body.contains('/') || body.contains('@') {
        return None;
    }
    Some(MentionToken {
        range: start..end,
        query: text[start + 1..cursor].to_string(),
    })
}

/// Height of the slash popup's key legend (22px caps + breathing room).
const SLASH_FOOTER_HEIGHT: f32 = 30.0;

/// The attach chord as the legend spells it (the `mod-enter` binding).
fn slash_attach_key_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘↵"
    } else {
        "ctrl ↵"
    }
}

/// The preamble attached-skill chips serialize into, prepended to the
/// user's text. Plain, visible text — the model reads it and loads each
/// named skill via its Skill tool; nothing is hidden from the transcript.
fn skill_preamble(skills: &[String]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let list = skills
        .iter()
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Load and follow these skills for this task: {list}."
    ))
}

/// `text` with the attached-skills preamble prepended (unchanged when no
/// skills are attached). An otherwise-empty message sends the preamble
/// alone — a legitimate "load these" turn.
fn with_skill_preamble(skills: &[String], text: &str) -> String {
    match skill_preamble(skills) {
        None => text.to_string(),
        Some(preamble) if text.trim().is_empty() => preamble,
        Some(preamble) => format!("{preamble}\n\n{text}"),
    }
}

/// Add `name` to a draft's attached skills; no-op (false) if already there.
fn attach_skill(skills: &mut Vec<String>, name: &str) -> bool {
    if name.is_empty() || skills.iter().any(|s| s == name) {
        return false;
    }
    skills.push(name.to_string());
    true
}

/// Drop `name` from a draft's attached skills; false if it wasn't attached.
fn detach_skill(skills: &mut Vec<String>, name: &str) -> bool {
    let before = skills.len();
    skills.retain(|s| s != name);
    skills.len() != before
}

/// The span to delete when a typed `/query` token becomes a chip: the token
/// plus one following space, so `fix /cod now` → `fix now` (not a double
/// space) while `fix /cod` → `fix ` keeps the caret ready to type on.
fn attach_removal_range(text: &str, token: Range<usize>) -> Range<usize> {
    let end = if text[token.end..].starts_with(' ') {
        token.end + 1
    } else {
        token.end
    };
    token.start..end
}

/// Slash-command completion state: like [`FileMentionState`] but the
/// candidate list is fetched once per (harness, cwd) (`ListCommands`) and
/// filtered locally per keystroke — no RPC, debounce, or skeleton churn while
/// typing.
#[derive(Debug, Clone, Default)]
struct SlashState {
    token: Option<MentionToken>,
    /// Indices into the cached command list, filter-ranked for the query.
    filtered: Vec<usize>,
    active: Option<usize>,
    /// Harness the popup is showing commands for (half of the cache key).
    harness: Option<HarnessId>,
    /// The active chat's cwd when the popup last (re)opened — empty when
    /// none is known yet (a blank new-chat screen). The other half of the
    /// cache key: project-scoped commands/skills differ per repo/worktree,
    /// so a chat must never show another chat's cached list just because
    /// they share a harness.
    cwd: String,
    request: u64,
    loading: bool,
    error: Option<SharedString>,
    dismissed: Option<(Range<usize>, String)>,
}

/// `(harness, cwd)`: cwd is part of the cache key because a harness's
/// advertised commands can be project-scoped (Claude Code's
/// `.claude/commands` and skills) — two chats on the same harness but
/// different repos/worktrees must each get their own list.
type SlashCacheKey = (HarnessId, String);

#[derive(Debug, Clone)]
struct SlashCacheEntry {
    commands: Vec<SlashCommand>,
    fetched_at: Instant,
}

/// A skill/command added to a project mid-session should eventually show up
/// without restarting zeron, but a picker reopen (or a quick tab-switch
/// between a couple of chats) shouldn't re-fetch every time either — this is
/// the same order of magnitude as the engine's own per-cwd discovery cache
/// (see `mast_harness::claude::discovery::CommandsCache`).
const SLASH_CACHE_TTL: Duration = Duration::from_secs(300);

/// Whether a cached entry for `cached_key` can still answer a lookup for
/// `key`: same (harness, cwd) AND not yet past the TTL. Pulled out as a pure
/// function so the cache-key/invalidation decision is unit-testable without
/// a GPUI context.
fn slash_cache_entry_is_fresh(
    cached_key: &SlashCacheKey,
    fetched_at: Instant,
    key: &SlashCacheKey,
    now: Instant,
) -> bool {
    cached_key == key && now.saturating_duration_since(fetched_at) < SLASH_CACHE_TTL
}

#[derive(Debug, Clone, Default)]
struct FileMentionState {
    token: Option<MentionToken>,
    results: Vec<FileSearchMatch>,
    active: Option<usize>,
    request: u64,
    loading: bool,
    /// Why the last search failed, for the popup. A failure MUST NOT render
    /// as "No matching files": cross-device searches fail for reasons the
    /// user can act on (host daemon too old for `SearchFiles`, device
    /// offline), and the empty state hid them (user report).
    error: Option<SharedString>,
    /// Full token text, not just the cursor-relative query: moving within a
    /// dismissed token keeps it closed, while any edit re-enables completion.
    dismissed: Option<(Range<usize>, String)>,
}

fn mention_response_is_current(state: &FileMentionState, request: u64) -> bool {
    state.request == request && state.token.is_some()
}

/// A failed file search, translated for the popup. `UnknownMethod` is the
/// version-skew case: `SearchFiles` shipped after v0.1.9, so a session hosted
/// by a device on an older daemon answers "unknown method" while the same
/// search works for local sessions.
fn mention_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Mast — update it to search its files".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => "File search failed".into(),
    }
}

/// The folder the chat composer's `/` list discovers project skills in: the
/// folder the agent runs (or will run) in, because Claude Code loads
/// `<cwd>/.claude/skills` (walking up to the cwd's git root, stopping at a
/// nested repo/submodule) and nothing else.
/// - A selected chat: its own cwd (a Work Outline chat's validated
///   execution root, e.g. a submodule), else its Space folder.
/// - A new-chat draft: the reused worktree when one is picked, else the
///   selected Space folder. Empty only with no project at all — that list
///   holds user-scoped skills only.
fn slash_discovery_cwd(
    chat: Option<&mast_proto::Chat>,
    chat_space: Option<&mast_proto::Space>,
    selected_space: Option<&mast_proto::Space>,
    reuse_worktree: Option<String>,
) -> String {
    let non_empty = |path: &str| (!path.trim().is_empty()).then(|| path.to_owned());
    match chat {
        Some(chat) => chat
            .cwd
            .as_deref()
            .and_then(non_empty)
            .or_else(|| chat_space.and_then(|space| non_empty(&space.path)))
            .unwrap_or_default(),
        None => reuse_worktree
            .as_deref()
            .and_then(non_empty)
            .or_else(|| selected_space.and_then(|space| non_empty(&space.path)))
            .unwrap_or_default(),
    }
}

/// `ListCommands` params for `(harness, cwd)`, targeted like file search:
/// the chat/space host device owns the agent binary, and a chat id lets the
/// engine resolve the chat's own folder safely. Shared by the chat composer
/// and [`skill_completion::SkillCompletion`].
fn list_commands_params(
    harness: HarnessId,
    cwd: &str,
    target_device_id: Option<&str>,
    chat_id: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({ "harness": harness, "cwd": cwd });
    if let Some(object) = params.as_object_mut() {
        if let Some(target) = target_device_id {
            object.insert("targetDeviceId".into(), target.into());
        }
        if let Some(chat_id) = chat_id {
            object.insert("chatId".into(), chat_id.into());
        }
    }
    params
}

/// One slash-popup row's body: the command icon, `/name`, and the muted
/// description (with its argument hint). Shared by the chat composer's
/// popup and [`skill_completion::SkillCompletion`].
fn slash_row_body(theme: &Theme, command: &SlashCommand) -> gpui::Div {
    let name: SharedString = format!("/{}", command.name).into();
    let mut description = command.description.clone();
    if let Some(hint) = &command.input_hint {
        if description.is_empty() {
            description = format!("<{hint}>");
        } else {
            description = format!("{description} · <{hint}>");
        }
    }
    let description: SharedString = description.into();
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            crate::icons::icon(crate::icons::COMMAND)
                .size(px(14.0))
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_none()
                .text_size(crate::typography::ui_rems(12.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(name),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(description),
        )
}

/// A failed command discovery, translated for the popup.
fn slash_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Mast — update it to list commands".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => {
            "Couldn't load this agent's commands".into()
        }
    }
}

/// What the composer's render reads from `AppState` (the rest is its own
/// fields and its child entities): the selected chat and project rows, queue,
/// review comments for the current draft key, delivery posture, device
/// presence and capabilities, context usage and whether the run is live.
#[derive(Debug, Clone, PartialEq)]
struct ComposerSlice {
    selected_chat: Option<String>,
    chat: Option<crate::pickers::ChatControlsSlice>,
    space: Option<mast_proto::Space>,
    effective_device: Option<String>,
    local_device_id: Option<String>,
    devices: Vec<(mast_proto::Device, bool)>,
    queue: Vec<mast_doc::QueuedMessage>,
    comments: Vec<crate::comments::ReviewComment>,
    code_refs: Vec<String>,
    code_ref_pending: bool,
    comment_flush_pending: bool,
    delivery_degraded: bool,
    connectivity: mast_proto::Connectivity,
    context_usage: Option<mast_proto::ContextUsage>,
    live: bool,
}

impl ComposerSlice {
    fn read(state: &AppState, key: &str, now: chrono::DateTime<chrono::Utc>) -> Self {
        let selected = state.selected_chat.as_deref();
        Self {
            selected_chat: state.selected_chat.clone(),
            chat: state
                .selected_chat_row()
                .map(crate::pickers::ChatControlsSlice::from),
            space: state.selected_space_row().cloned(),
            effective_device: state.effective_device_id(),
            local_device_id: state.local_device_id.clone(),
            devices: state
                .devices
                .iter()
                .map(|d| (d.clone(), state.device_online(&d.id, now)))
                .collect(),
            queue: state.queue.clone(),
            comments: state.review_comments(key).to_vec(),
            code_refs: state.code_refs(key).iter().map(|reference| reference.id.clone()).collect(),
            code_ref_pending: state.code_ref_load_pending(key),
            comment_flush_pending: state.review_comment_flush_pending(key),
            delivery_degraded: selected.is_some_and(|id| state.chat_delivery_degraded(id)),
            connectivity: state.connectivity.clone(),
            context_usage: state.context_usage,
            live: selected.is_some_and(|id| {
                matches!(
                    state.indicator_for(id, now),
                    Indicator::Working | Indicator::AwaitingInput
                )
            }),
        }
    }
}

#[derive(Clone)]
struct FailedSend {
    key: String,
    text: String,
    message_id: String,
    queued: bool,
    attachment_ids: Vec<String>,
    appshot_ids: Vec<String>,
    skills: Vec<String>,
    comments: Vec<crate::comments::ReviewComment>,
    code_refs: Vec<crate::code_ref::StagedCodeRef>,
}

pub struct Composer {
    sign_in_check: Option<(Option<String>, mast_proto::HarnessId, Option<String>, Instant, bool)>,
    pub(crate) state: Entity<AppState>,
    seen: crate::observe_gate::ObserveGate<ComposerSlice>,
    pub(crate) input: Entity<ComposerInput>,
    /// Draft displaced while a queued message occupies the composer.
    pub(crate) queue_edit_draft: Option<(String, Vec<StagedAttachment>, Vec<CapturedAppshot>)>,
    /// Composer actions row plus the new-session floating target tab
    /// ([`Pickers::render_new_thread_target_selectors`]).
    pickers: Entity<Pickers>,
    /// Draft text per chat key ("" = new-chat canvas), surviving navigation.
    drafts: HashMap<String, String>,
    /// Throttle for "the user is acting here" pings to the engine (orchestrator
    /// wakes hold events while the user is typing).
    activity: crate::orchestrator_wakes::ActivityReporter,
    /// Staged-but-unsent attachments per chat key (use-attachments.ts `stash`):
    /// navigating away and back restores them; memory-only, like the original.
    pub(crate) attachments: HashMap<String, Vec<StagedAttachment>>,
    /// Rich window captures keyed exactly like drafts and ordinary staged
    /// attachments. Each owns one screenshot that joins the existing upload
    /// path only at send time.
    pub(crate) appshots: HashMap<String, Vec<CapturedAppshot>>,
    /// Skills attached from the slash popup (Cmd/Ctrl+Enter or the row's
    /// ＋), keyed exactly like drafts. Per-draft: they serialize into the
    /// outgoing message's preamble ([`with_skill_preamble`]) and clear on
    /// send — the model keeps a loaded skill in context for the rest of the
    /// chat, so nothing persists per chat.
    skills: HashMap<String, Vec<String>>,
    code_ref_focus: HashMap<String, code_ref_chips::ReferenceChipFocus>,
    code_ref_focus_card: Option<(String, Entity<crate::components::code_ref::ReferenceCard>)>,
    code_ref_focus_pending: Option<String>,
    appshot_entrances: HashMap<String, Instant>,
    /// The staged attachment being viewed full-size (click a thumbnail).
    preview: Option<attachments::PreviewImage>,
    /// Focused while the lightbox is open so Escape reaches it; the input
    /// gets focus back on close.
    preview_focus: FocusHandle,
    /// Focus grab deferred to the next render (open sites don't all have a
    /// `Window` — the `MAST_ATTACH_PREVIEW` boot knob opens in `new`).
    preview_focus_pending: bool,
    /// In-flight file-picker prompt (paperclip).
    picker_task: Option<Task<()>>,
    mention_task: Option<Task<()>>,
    mention: FileMentionState,
    slash_task: Option<Task<()>>,
    slash: SlashState,
    /// Advertised commands per (harness, cwd) — one `ListCommands` per key
    /// per [`SLASH_CACHE_TTL`] (the engine caches discovery on its side too,
    /// also per-cwd for harnesses that support it).
    slash_cache: HashMap<SlashCacheKey, SlashCacheEntry>,
    /// Slash-popup row scroll — the stack overflows into a wheel/keyboard-
    /// scrollable list once it outgrows the card.
    slash_scroll: gpui::ScrollHandle,
    /// File-mention popup row scroll (same treatment).
    mention_scroll: gpui::ScrollHandle,
    /// Shared scrollbar hover/drag state for both popups' floating rails —
    /// they never show at once (mutually exclusive by token shape).
    popup_bar: crate::popover::MenuScrollbarState,
    pub(crate) current_key: String,
    sending: bool,
    /// Armed immediately before a blank-canvas send selects its minted chat.
    /// The state observer consumes it to distinguish that handoff from normal
    /// session navigation, which must continue to snap.
    launching_new_chat: bool,
    pub(crate) failure: Option<SharedString>,
    /// The chat key `failure` belongs to (`None` = global, e.g. "Engine not
    /// connected"). Chat-scoped failures survive navigation and render only
    /// under their own chat — a blanket clear-on-switch erased the one
    /// visible trace of a failed send (2026-08-19).
    failure_key: Option<String>,
    failed_send: Option<FailedSend>,
    retry_send_id: Option<String>,
    /// Work Outline instruction refusals (context reset confirmation).
    instruction_reset: instruction_reset::InstructionResetState,
    wizard: Option<Wizard>,
    wizard_focus: FocusHandle,
    /// Requests already answered locally (suppresses the panel until the doc
    /// frame marks them resolved).
    answered_requests: HashSet<String>,
    advance_task: Option<Task<()>>,
    send_task: Option<Task<()>>,
    /// The queued message being edited in the composer (see
    /// [`Composer::begin_queue_edit`]).
    pub(crate) editing_queued: Option<String>,
    /// Host-issued generation protecting `editing_queued` from automatic
    /// delivery. The text buffer is kept until Finish receives an ACK.
    pub(crate) queue_edit_lease_id: Option<String>,
    pub(crate) queue_edit_base_text_hash: Option<String>,
    pub(crate) queue_edit_chat_id: Option<String>,
    pub(crate) queue_edit_host_device_id: Option<String>,
    pub(crate) queue_edit_instance_id: String,
    pub(crate) queue_edit_pending_id: Option<String>,
    pub(crate) queue_edit_finishing: bool,
    pub(crate) queue_edit_task: Option<Task<()>>,
    pub(crate) queue_edit_renew_task: Option<Task<()>>,
    /// Focus once on mount, navigation, or after opening/closing a queue edit.
    pub(crate) focus_pending: bool,
    /// Live drag over the queue panel: which row, and where it would land.
    pub(crate) queue_drag: Option<crate::queue::QueueDragState>,
    pub(crate) queue_scroll: gpui::ScrollHandle,
    pub(crate) queue_full_preview: Option<Task<()>>,
    pub(crate) queue_previews: HashMap<(String, String), crate::queue::QueuePreview>,
    /// Rows awaiting a host-authoritative removal acknowledgement. They stay
    /// visible but inert until the host wins the race against queue delivery.
    pub(crate) queue_removing: HashSet<String>,
    /// Whether the modifier overlay should currently reveal the queue hint.
    /// The shell owns modifier tracking and clears this on window deactivation.
    queue_shortcut_revealed: bool,
    /// Interrupt/answer commands get their own slot: assigning `send_task`
    /// DROPPED an in-flight send future mid-upload — no banner, no cleanup,
    /// `sending` stuck true forever (2026-08-19 incident, "press Stop while
    /// a send grinds" shape).
    action_task: Option<Task<()>>,
    /// Chats whose durable Interrupt command has been accepted or is still
    /// being queued. Kept independently so stopping one chat cannot replace
    /// another chat's request when the user navigates quickly.
    interrupting: HashSet<String>,
    interrupt_tasks: HashMap<String, Task<()>>,
    // -- compact/expanded flip state (hysteresis; see `composer_flip`) --
    /// Current layout mode (persisted across frames — never derived fresh).
    expanded_mode: bool,
    /// `layout_epoch` of the measurement that caused the last flip: the flip is
    /// re-evaluated only after the input has been laid out in the new mode, so
    /// at most one flip can happen per layout pass.
    flip_epoch: u64,
    /// Compact-mode input capacity, learned while compact (layout-stable).
    compact_capacity: f32,
    /// Input width first measured after expanding — container-width deltas
    /// while expanded shift `compact_capacity` by the same amount.
    expanded_anchor: f32,
    /// Last input width seen in the current mode (resize detection).
    last_seen_width: f32,
    /// Stable outer composer width supplied by the shell. Unlike Taffy's
    /// provisional input measurements, this changes only when the actual
    /// conversation column changes and can safely drive a follow-up render.
    last_available_width: Option<f32>,
    /// Set while an interactive resize is in flight; collapse is deferred
    /// until widths have settled for [`RESIZE_SETTLE_MS`].
    width_changed_at: Option<Instant>,
    settle_task: Option<Task<()>>,
    /// In-flight compact↔expanded morph (one per committed flip; manual
    /// drive — see [`FlipMorph`]).
    flip_morph: Option<FlipMorph>,
    /// Pill height actually rendered last frame — a committed flip morphs
    /// from here, so mid-flight reversals hand off without a jump.
    last_rendered_height: f32,
    model_handoff_position: f32,
    model_handoff_from: f32,
    model_handoff_morph: Option<FlipMorph>,
    model_bounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    dock_frame: Option<crate::composer_dock::DockFrame>,
    /// The shared clock owns this frame's height, including its final step.
    dock_height_changed: bool,
    dock_clearance_correction: f32,
    surface_bounds: crate::new_thread_background_mask::SurfaceBounds,
    last_target_height: f32,
    height_morph: Option<FlipMorph>,
    /// Monotonic clock anchor for the morph timeline.
    morph_clock: Instant,
    /// Set on every session/route change: flips committed before this instant
    /// SNAP instead of morphing (see [`ROUTE_SNAP_MS`]).
    route_snap_until: Option<Instant>,
    _observe: Subscription,
    _pickers_observe: Subscription,
    _picker_focus: Subscription,
    _input_events: Subscription,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_attachment(&mut self, bytes: Vec<u8>) {
        let attachment = attachments::stage_png_bytes("preview.png".into(), bytes);
        self.attachments.entry(self.current_key.clone()).or_default().push(attachment);
    }
    fn refresh_sign_in_status(&mut self, cx: &mut Context<Self>) -> Option<(String, mast_proto::HarnessId, Option<String>, mast_proto::ManagedAccountsSnapshot)> {
        let state = self.state.read(cx);
        if state.selected_chat.is_some() { self.sign_in_check = None; return None; }
        let harness = self.pickers.read(cx).resolved(cx).harness?;
        if !crate::managed_accounts::supports_accounts(harness) { return None; }
        let device = state.effective_device_id();
        let missing_snapshot = state.managed_accounts_for(device.as_deref()).is_none();
        let has_space = state.selected_space_row().is_some();
        let snapshot = state.managed_accounts_for(device.as_deref()).cloned().unwrap_or_default();
        let account = state.selected_space_row().and_then(|space| crate::managed_accounts::project_account(&snapshot, &space.id, harness)).map(|account| account.id.clone());
        let key = (device.clone(), harness, account.clone());
        let refresh = self.sign_in_check.as_ref().is_none_or(|(d, h, a, at, _)| (d, *h, a) != (&key.0, key.1, &key.2) || at.elapsed() >= Duration::from_secs(30));
        if refresh {
            let rpc = state.accounts_rpc(device.as_deref());
            if missing_snapshot {
                self.state.update(cx, |state, cx| state.load_managed_accounts(device.clone(), cx));
            }
            let started = Instant::now();
            self.sign_in_check = Some((device.clone(), harness, account.clone(), started, false));
            let request_key = key.clone();
            cx.spawn(async move |this, cx| {
                let result = rpc.call(mast_rpc::methods::LOCAL_AGENT_SIGN_IN_STATUS, serde_json::json!({"harness": harness, "accountId": request_key.2.clone()})).await;
                this.update(cx, |this, cx| {
                    if let Some((d, h, a, at, signed_out)) = &mut this.sign_in_check
                        && (&*d, *h, &*a, *at) == (&request_key.0, request_key.1, &request_key.2, started) {
                            *signed_out = result.ok().and_then(|value| value.as_str().map(str::to_owned)).as_deref() == Some("signed_out");
                            cx.notify();
                        }
                }).ok();
            }).detach();
            return None;
        }
        if missing_snapshot && has_space { return None; }
        self.sign_in_check.as_ref().is_some_and(|entry| entry.4).then(|| (device.unwrap_or_default(), harness, account, snapshot))
    }
    pub(crate) fn set_dock_frame(
        &mut self,
        frame: crate::composer_dock::DockFrame,
        cx: &mut Context<Self>,
    ) {
        let changed = self.dock_frame != Some(frame);
        self.dock_height_changed |= self
            .dock_frame
            .is_none_or(|previous| previous.amount != frame.amount);
        self.dock_frame = Some(frame);
        if frame.active {
            self.flip_morph = None;
            self.height_morph = None;
        }
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn dock_clearance_correction(&self) -> f32 {
        self.dock_clearance_correction
    }

    pub(crate) fn surface_bounds(&self) -> crate::new_thread_background_mask::SurfaceBounds {
        self.surface_bounds.clone()
    }

    /// The picker entity, for the shell's canvas target selectors.
    pub fn pickers(&self) -> &Entity<Pickers> {
        &self.pickers
    }

    /// Feed the stable conversation-column width into responsive composer
    /// controls.
    pub fn set_available_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let composer_width = width.clamp(0.0, COMPOSER_MAX_WIDTH);
        if composer_width_changed(self.last_available_width, composer_width) {
            self.last_available_width = Some(composer_width);
            // The shell renders before this child, so this queues one more
            // pass after the input has been laid out at its final width. That
            // pass can consume the completed measurement without emitting an
            // event from inside Taffy's multi-pass measurement callback.
            cx.notify();
        }
    }

    pub(crate) fn set_queue_shortcut_revealed(&mut self, revealed: bool, cx: &mut Context<Self>) {
        if self.queue_shortcut_revealed != revealed {
            self.queue_shortcut_revealed = revealed;
            cx.notify();
        }
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| this.release_queue_previews(cx))
            .detach();
        let input = cx.new(|cx| {
            let mut input =
                ComposerInput::with_context("Do anything…", MESSAGE_COMPOSER_CONTEXT, cx);
            input.enable_mentions();
            input
        });
        let pickers = cx.new(|cx| Pickers::new(state.clone(), cx));
        // The footer toolbar (checkout kind + ref picker) is rendered INLINE
        // by the composer from picker state — a pickers-side notify (refs
        // loaded, popover toggled, pick made) must repaint the composer too.
        let pickers_observe = cx.observe(&pickers, |_, _, cx| cx.notify());
        let picker_focus = cx.subscribe(
            &pickers,
            |this: &mut Self, _, _: &crate::pickers::ReturnComposerFocus, cx| {
                this.focus_pending = true;
                cx.notify();
            },
        );
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        let input_events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.on_submit(cx),
            ComposerInputEvent::ModifiedSubmitted => this.on_modified_submit(cx),
            ComposerInputEvent::Edited => this.on_input_edited(cx),
            ComposerInputEvent::CursorMoved => {
                let input = this.input.read(cx);
                if this.mention.token.is_some()
                    || this.slash.token.is_some()
                    || this.mention.dismissed.is_some()
                    || this.slash.dismissed.is_some()
                    || mention_token(input.text(), input.cursor_offset()).is_some()
                    || slash_token(input.text(), input.cursor_offset()).is_some()
                {
                    this.on_input_edited(cx);
                } else {
                    this.report_user_activity(cx);
                }
            }
            ComposerInputEvent::ViewportChanged => cx.notify(),
            // The slash popup and the mention popup share the input's
            // completion key routing; they are mutually exclusive by token
            // shape (a token opening with `/` vs `@` at a token boundary —
            // `slash_token` rejects any token containing `@`, and
            // `on_input_edited` never opens the file popup while a slash
            // token is live), so routing on `slash.token` is unambiguous.
            ComposerInputEvent::MentionNavigate(delta) => {
                if this.slash.token.is_some() {
                    this.move_slash(*delta, cx)
                } else {
                    this.move_mention(*delta, cx)
                }
            }
            ComposerInputEvent::MentionAccept => {
                if this.slash.token.is_some() {
                    this.accept_slash(cx)
                } else {
                    this.accept_mention(cx)
                }
            }
            // Cmd/Ctrl+Enter: the slash popup's secondary action attaches the
            // row as a skill chip; the file popup has no second action.
            ComposerInputEvent::MentionAttach => {
                if this.slash.token.is_some() {
                    this.attach_slash(cx)
                } else {
                    this.accept_mention(cx)
                }
            }
            ComposerInputEvent::MentionDismiss => {
                if this.slash.token.is_some() {
                    this.dismiss_slash(cx)
                } else {
                    this.dismiss_mention(cx)
                }
            }
            ComposerInputEvent::PastedImages(images) => {
                let staged = images
                    .iter()
                    .map(|image| attachments::stage_clipboard_image(image.clone()))
                    .collect();
                this.add_staged(staged, cx);
            }
            ComposerInputEvent::PastedPaths(paths) => this.add_paths(paths.clone(), cx),
        });
        let current_key = state.read(cx).selected_chat.clone().unwrap_or_default();
        let mut composer = Self {
            sign_in_check: None,
            state,
            input,
            queue_edit_draft: None,
            pickers,
            drafts: HashMap::new(),
            activity: Default::default(),
            attachments: HashMap::new(),
            appshots: HashMap::new(),
            skills: HashMap::new(),
            code_ref_focus: HashMap::new(),
            code_ref_focus_card: None,
            code_ref_focus_pending: None,
            appshot_entrances: HashMap::new(),
            preview: None,
            preview_focus: cx.focus_handle(),
            preview_focus_pending: false,
            picker_task: None,
            mention_task: None,
            mention: FileMentionState::default(),
            slash_task: None,
            slash: SlashState::default(),
            slash_cache: HashMap::new(),
            slash_scroll: gpui::ScrollHandle::new(),
            mention_scroll: gpui::ScrollHandle::new(),
            popup_bar: crate::popover::MenuScrollbarState::default(),
            current_key,
            sending: false,
            launching_new_chat: false,
            failure: None,
            seen: Default::default(),
            wizard: None,
            wizard_focus: cx.focus_handle(),
            answered_requests: HashSet::new(),
            failure_key: None,
            failed_send: None,
            retry_send_id: None,
            instruction_reset: Default::default(),
            action_task: None,
            advance_task: None,
            send_task: None,
            interrupting: HashSet::new(),
            interrupt_tasks: HashMap::new(),
            editing_queued: None,
            queue_edit_lease_id: None,
            queue_edit_base_text_hash: None,
            queue_edit_chat_id: None,
            queue_edit_host_device_id: None,
            queue_edit_instance_id: uuid::Uuid::new_v4().to_string(),
            queue_edit_pending_id: None,
            queue_edit_finishing: false,
            queue_edit_task: None,
            queue_edit_renew_task: None,
            focus_pending: true,
            queue_drag: None,
            queue_scroll: gpui::ScrollHandle::new(),
            queue_full_preview: None,
            queue_previews: HashMap::new(),
            queue_removing: HashSet::new(),
            queue_shortcut_revealed: false,
            expanded_mode: false,
            flip_epoch: 0,
            compact_capacity: 0.0,
            expanded_anchor: 0.0,
            last_seen_width: 0.0,
            last_available_width: None,
            width_changed_at: None,
            settle_task: None,
            flip_morph: None,
            last_rendered_height: 0.0,
            model_handoff_position: 1.0,
            model_handoff_from: 1.0,
            model_handoff_morph: None,
            model_bounds: Default::default(),
            dock_frame: None,
            dock_height_changed: false,
            dock_clearance_correction: 0.0,
            surface_bounds: Default::default(),
            last_target_height: 0.0,
            height_morph: None,
            morph_clock: Instant::now(),
            route_snap_until: None,
            _observe: observe,
            _pickers_observe: pickers_observe,
            _picker_focus: picker_focus,
            _input_events: input_events,
        };
        // Dev knob: pre-stage attachments (drop/paste can't be synthesized on
        // a rig) — `MAST_ATTACH=/path/a.png[,/path/b.png]`, and
        // `MAST_ATTACH_PREVIEW=1` boots with the first one's lightbox open.
        if let Ok(spec) = mast_proto::env::var("MAST_ATTACH") {
            let staged: Vec<StagedAttachment> = spec
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .filter_map(|path| {
                    match attachments::stage_file(std::path::Path::new(path.trim())) {
                        Ok(att) => Some(att),
                        Err(err) => {
                            tracing::warn!(%path, error = %err, "MAST_ATTACH stage failed");
                            None
                        }
                    }
                })
                .collect();
            if mast_proto::env::var("MAST_ATTACH_PREVIEW").is_ok_and(|v| v == "1")
                && let Some(first) = staged.first()
            {
                composer.preview = Some(attachments::PreviewImage::new(
                    first.name.clone(),
                    first.image.clone(),
                ));
                composer.preview_focus_pending = true;
            }
            if !staged.is_empty() {
                composer
                    .attachments
                    .entry(composer.current_key.clone())
                    .or_default()
                    .extend(staged);
            }
        }
        composer
    }

    /// Capture-knob passthrough (`MAST_OPEN_DIALOG=model`): open the
    /// combined harness/model menu.
    pub fn debug_open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pickers
            .update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
    }

    /// The prompt input's current draft (tests).
    #[cfg(test)]
    pub(crate) fn draft_text(&self, cx: &App) -> String {
        self.input.read(cx).text().to_owned()
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    pub(crate) fn can_edit_queue_in_composer(&self) -> bool {
        !self.sending && self.wizard.is_none()
    }

    // ---- attachment staging (use-attachments.ts) ----

    /// Staged attachments for the chat the composer is showing.
    pub(crate) fn staged(&self) -> &[StagedAttachment] {
        self.attachments
            .get(&self.current_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn queue_preview_limit(&self) -> usize {
        if self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH) < 520.0 {
            1
        } else {
            2
        }
    }

    pub(crate) fn show_queue_image(
        &mut self,
        preview: attachments::PreviewImage,
        cx: &mut Context<Self>,
    ) {
        self.preview = Some(preview);
        self.preview_focus_pending = true;
        cx.notify();
    }

    pub(crate) fn staged_appshots(&self) -> &[CapturedAppshot] {
        self.appshots
            .get(&self.current_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Skills attached to the draft the composer is showing.
    fn staged_skills(&self) -> &[String] {
        self.skills
            .get(&self.current_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn remove_skill(&mut self, name: &str, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        if let Some(list) = self.skills.get_mut(&key)
            && detach_skill(list, name)
        {
            if list.is_empty() {
                self.skills.remove(&key);
            }
            self.focus_pending = true;
            cx.notify();
        }
    }

    /// The attached-skills row: one removable chip per skill, above the input
    /// (same strip geometry as the review-comments chip, so the pill's
    /// arithmetic sizing stays exact — see [`comment_strip_height`]).
    fn update_slash(
        &mut self,
        token: Option<MentionToken>,
        still_dismissed: bool,
        cx: &mut Context<Self>,
    ) {
        if token.is_none() && self.slash.token.is_none() {
            return;
        }
        if still_dismissed {
            self.slash.token = None;
            self.sync_mention_controls(cx);
            return;
        }
        self.slash.dismissed = None;
        let harness = self.pickers.read(cx).resolved(cx).harness;
        let reuse_worktree = match self.pickers.read(cx).checkout_plan() {
            crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } => Some(path),
            _ => None,
        };
        let cwd = {
            let state = self.state.read(cx);
            let chat = state.selected_chat_row();
            let chat_space = chat
                .and_then(|chat| chat.space_id.as_deref())
                .and_then(|id| state.space_row(id));
            slash_discovery_cwd(chat, chat_space, state.selected_space_row(), reuse_worktree)
        };
        let key_changed = self.slash.harness != harness || self.slash.cwd != cwd;
        if token == self.slash.token && !key_changed {
            return;
        }
        self.slash.token = token.clone();
        self.slash.harness = harness;
        self.slash.cwd = cwd.clone();
        self.slash.error = None;
        if token.is_none() {
            self.slash.active = None;
            self.sync_mention_controls(cx);
            return;
        }
        // No resolved harness (catalog still loading): empty popup, no fetch.
        let Some(harness) = harness else {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        };
        let key: SlashCacheKey = (harness, cwd.clone());
        let now = Instant::now();
        let fresh = self
            .slash_cache
            .get(&key)
            .is_some_and(|entry| slash_cache_entry_is_fresh(&key, entry.fetched_at, &key, now));
        if fresh {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        }
        // First open for this (harness, cwd) — or a stale/evicted entry: one
        // ListCommands, targeted like file search (the chat/space host
        // device owns the agent binary). A chat switch that lands back on an
        // already-cached (harness, cwd) within the TTL never re-fetches.
        self.slash.request = self.slash.request.wrapping_add(1);
        self.slash.loading = true;
        self.refilter_slash(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.slash.loading = false;
            return;
        };
        let (target, chat_id) = {
            let state = self.state.read(cx);
            match state.selected_chat_row() {
                Some(chat) => (Some(chat.device_id.clone()), Some(chat.id.clone())),
                None => (
                    state
                        .selected_space_row()
                        .map(|space| space.device_id.clone()),
                    None,
                ),
            }
        };
        let request = self.slash.request;
        let params = list_commands_params(harness, &key.1, target.as_deref(), chat_id.as_deref());
        self.slash_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_COMMANDS, params).await;
            this.update(cx, |composer, cx| {
                if composer.slash.request != request {
                    return;
                }
                composer.slash.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => {
                            composer.slash_cache.insert(
                                key,
                                SlashCacheEntry {
                                    commands,
                                    fetched_at: Instant::now(),
                                },
                            );
                        }
                        Err(err) => tracing::warn!(%err, "slash command decode failed"),
                    },
                    Err(err) => {
                        tracing::debug!(%err, "slash command discovery failed");
                        composer.slash.error = Some(slash_error_message(&err));
                    }
                }
                composer.refilter_slash(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// The cache key for the popup's currently-tracked (harness, cwd), if a
    /// harness is resolved.
    fn slash_key(&self) -> Option<SlashCacheKey> {
        self.slash
            .harness
            .map(|harness| (harness, self.slash.cwd.clone()))
    }

    /// Re-rank the cached list for the current query (pure local filter).
    fn refilter_slash(&mut self, cx: &mut Context<Self>) {
        let query = self
            .slash
            .token
            .as_ref()
            .map(|t| t.query.clone())
            .unwrap_or_default();
        let commands = self
            .slash_key()
            .and_then(|key| self.slash_cache.get(&key))
            .map(|entry| entry.commands.as_slice())
            .unwrap_or_default();
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        self.slash.filtered = crate::popover::filter_indices(&query, &names);
        self.slash.active = (!self.slash.filtered.is_empty()).then_some(0);
        // A fresh query/reopen restarts the row stack at the top.
        crate::popover::reset_menu_scroll(&self.slash_scroll, &mut self.popup_bar);
        self.sync_mention_controls(cx);
        cx.notify();
    }

    fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.slash.active =
            crate::popover::menu_step(self.slash.active, self.slash.filtered.len(), delta);
        if let Some(active) = self.slash.active {
            // Keep the keyboard cursor visible in the scrolled row stack.
            self.slash_scroll.scroll_to_item(active);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    fn dismiss_slash(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.slash.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_slash(dismissed, cx);
        cx.notify();
    }

    /// The command under the popup's keyboard cursor.
    fn active_slash_command(&self) -> Option<SlashCommand> {
        self.slash
            .active
            .and_then(|active| self.slash.filtered.get(active))
            .and_then(|&ix| {
                self.slash_key()
                    .and_then(|key| self.slash_cache.get(&key))
                    .and_then(|entry| entry.commands.get(ix))
            })
            .cloned()
    }

    /// Enter/Tab/click: replace the typed token with `/name `. At offset 0
    /// that is the whole-prompt command the CLI expands natively (unchanged
    /// behavior); anywhere else the same text is an inline skill mention the
    /// model reads — no private URI scheme, it is just text.
    fn accept_slash(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(command) = self.active_slash_command() else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &format!("/{}", command.name), cx)
        });
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Cmd/Ctrl+Enter or the row's ＋: attach the command as a skill chip
    /// and delete the typed `/query` token. While a queued row is being
    /// edited there is no send to carry a preamble, so it inserts instead.
    fn attach_slash(&mut self, cx: &mut Context<Self>) {
        if self.editing_queued.is_some() {
            self.accept_slash(cx);
            return;
        }
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(command) = self.active_slash_command() else {
            return;
        };
        let key = self.current_key.clone();
        attach_skill(self.skills.entry(key).or_default(), &command.name);
        let removal = {
            let text = self.input.read(cx).text();
            (token.range.end <= text.len()).then(|| attach_removal_range(text, token.range))
        };
        if let Some(removal) = removal {
            self.input
                .update(cx, |input, cx| input.remove_plain_range(removal, cx));
        }
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Tear down the slash completion (mirrors [`Self::reset_mention`]).
    fn reset_slash(&mut self, dismissed: Option<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let request = self.slash.request.wrapping_add(1);
        self.slash_task = None;
        self.slash = SlashState {
            request,
            dismissed,
            harness: self.slash.harness,
            cwd: self.slash.cwd.clone(),
            ..SlashState::default()
        };
        self.sync_mention_controls(cx);
    }

    fn render_slash_popup(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &theme.for_popup();
        // Only while a slash token is active.
        self.slash.token.as_ref()?;
        let commands = self
            .slash_key()
            .and_then(|key| self.slash_cache.get(&key))
            .map(|entry| entry.commands.as_slice())
            .unwrap_or_default();
        // Full pill width at the mention card's height budget — both composer
        // completions share the same surface shape.
        let mut card = crate::components::conversation_menu_card(theme, false)
            .w_full()
            .max_h(px(320.0))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_slash(cx)));
        if self.slash.loading && commands.is_empty() {
            card = card.child(crate::popover::skeleton_rows(
                "slash-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(error) = self.slash.error.clone() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.danger_muted)
                    .child(error),
            );
        } else if self.slash.filtered.is_empty() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(if commands.is_empty() {
                        "This agent has no slash commands"
                    } else {
                        "No matching commands"
                    }),
            );
        } else {
            // A queued-row edit has no send to carry a preamble (attach falls
            // back to insert there), so the affordance is hidden.
            let can_attach = self.editing_queued.is_none();
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.slash.filtered.len());
            for (row_ix, &cmd_ix) in self.slash.filtered.iter().enumerate() {
                let Some(command) = commands.get(cmd_ix) else {
                    continue;
                };
                let selected = self.slash.active == Some(row_ix);
                let row_group: SharedString = format!("slash-row-{row_ix}").into();
                rows.push(
                    crate::components::conversation_menu_row(theme, selected, format!("slash-result-{row_ix}"))
                        .group(row_group.clone())
                        .id(("slash-result", row_ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.slash.active = Some(row_ix);
                            this.accept_slash(cx);
                        }))
                        .child(slash_row_body(theme, command).when(can_attach, |row| {
                            row.child(Self::render_slash_attach_button(
                                theme, row_ix, selected, row_group, cx,
                            ))
                        }))
                        .into_any_element(),
                );
            }
            // Overflowing rows wheel-scroll inside a bounded viewport; the
            // floating rail mirrors the model-list scrollbar treatment.
            card = card.child(
                div()
                    .id("slash-scroll-host")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("slash-list")
                            .max_h(px(if can_attach {
                                312.0 - SLASH_FOOTER_HEIGHT
                            } else {
                                312.0
                            }))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.slash_scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(self, "slash-scrollbar", theme, cx)),
            );
            if can_attach {
                card = card.child(Self::render_slash_footer(theme));
            }
        }
        // Full pill width above the composer, matching the file-mention popup.
        Some(crate::popover::full_width_menu_above(
            "slash-popup",
            card.into_any_element(),
            None,
        ))
    }

    /// The row's secondary action: attach this command as a skill chip
    /// instead of inserting it. Visible on the keyboard-selected row and on
    /// hover; the click never reaches the row's own insert handler.
    fn render_slash_attach_button(
        theme: &Theme,
        row_ix: usize,
        selected: bool,
        row_group: SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(("slash-attach", row_ix))
            .role(gpui::Role::Button)
            .aria_label("Attach skill to chat")
            .flex_none()
            .size(px(20.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(true, |el| crate::components::conversation_reveal(el, theme, row_group, selected))
            .hover(|s| s.bg(crate::theme::ink(0.08)))
            .tooltip(|_, cx| {
                cx.new(|_| {
                    crate::workspace_chip::WorkspaceChipTooltip(
                        format!("Attach to chat ({})", slash_attach_key_label()).into(),
                    )
                })
                .into()
            })
            .tooltip_show_delay(Duration::from_millis(350))
            // Mouse-down bubbles to the card, which keeps the input focused
            // (an unfocused input tears the popup down on the next render).
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.slash.active = Some(row_ix);
                this.attach_slash(cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }))
            .child(
                crate::icons::icon(crate::icons::PLUS)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .into_any_element()
    }

    /// One-line legend under the slash rows: Enter inserts, the modified
    /// Enter attaches.
    fn render_slash_footer(theme: &Theme) -> gpui::Div {
        div()
            .h(px(SLASH_FOOTER_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .child(crate::popover::key_hint_text(theme, "↵", "Insert"))
            .child(crate::popover::key_hint_text(
                theme,
                slash_attach_key_label(),
                "Attach to chat",
            ))
    }

    /// The popup whose rows a scrollbar drag is moving — the tokens are
    /// mutually exclusive, so at most one exists.
    fn active_popup_scroll(&self) -> Option<gpui::ScrollHandle> {
        if self.slash.token.is_some() {
            Some(self.slash_scroll.clone())
        } else if self.mention.token.is_some() {
            Some(self.mention_scroll.clone())
        } else {
            None
        }
    }

    fn on_popup_list_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup_bar.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Reconcile composer-owned state with `AppState` (draft swap on
    /// navigation, question panel, queue-edit and interrupt bookkeeping), and
    /// repaint only when that changed something visible: the slice of state
    /// the composer renders differs, or the reconcile itself moved composer
    /// state. Streaming frames and other chats' updates do neither.
    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let before = self.reconcile_signature();
        self.reconcile_with_state(cx);
        let slice = ComposerSlice::read(self.state.read(cx), &self.current_key, chrono::Utc::now());
        let slice_changed = self.seen.changed(slice);
        if slice_changed || before != self.reconcile_signature() {
            cx.notify();
        }
    }

    /// The composer-owned fields `reconcile_with_state` can change.
    fn reconcile_signature(&self) -> impl PartialEq + use<> {
        (
            self.current_key.clone(),
            self.wizard.as_ref().map(|w| w.request_id.clone()),
            self.editing_queued.clone(),
            self.failure.clone(),
            self.interrupting.len(),
            self.queue_removing.len(),
        )
    }

    fn reconcile_with_state(&mut self, cx: &mut Context<Self>) {
        {
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            retain_live_interrupts(&mut self.interrupting, |chat_id| {
                matches!(
                    state.indicator_for(chat_id, now),
                    Indicator::Working | Indicator::AwaitingInput
                )
            });
            self.queue_removing
                .retain(|id| state.queue.iter().any(|item| item.id == *id));
        }
        self.interrupt_tasks
            .retain(|chat_id, _| self.interrupting.contains(chat_id));

        let editing_id = self.editing_queued.clone();
        let (key, pending, edited_row_exists) = {
            let s = self.state.read(cx);
            (
                s.selected_chat.clone().unwrap_or_default(),
                pending_input_request(&s.transcript),
                editing_id
                    .as_ref()
                    .is_none_or(|id| s.queue.iter().any(|item| item.id == *id)),
            )
        };

        // A queue edit belongs to exactly one visible row. Navigation or a
        // remote drain/removal cancels it instead of leaving a focused but
        // unmounted editor entity behind.
        if key != self.current_key && self.editing_queued.is_some() {
            self.clear_queue_edit(cx);
        } else if !edited_row_exists && self.editing_queued.is_some() && !self.queue_edit_finishing
        {
            self.failure =
                Some("The queued message was removed; your edit remains in the composer".into());
            // Recover both drafts when another device removes the reserved row.
            if let Some((draft, mut attachments, mut appshots)) = self.queue_edit_draft.take() {
                appshots.extend(self.appshots.remove(&self.current_key).unwrap_or_default());
                self.appshots.insert(self.current_key.clone(), appshots);
                let edited = self.input.read(cx).text().to_string();
                let text = [draft, edited]
                    .into_iter()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.input.update(cx, |input, cx| input.set_text(text, cx));
                attachments.extend(
                    self.attachments
                        .remove(&self.current_key)
                        .unwrap_or_default(),
                );
                self.attachments
                    .insert(self.current_key.clone(), attachments);
            }
            self.clear_queue_edit(cx);
        }

        // Draft swap on chat navigation — the input entity itself survives.
        if key != self.current_key {
            let new_thread_launch =
                self.launching_new_chat && self.current_key.is_empty() && !key.is_empty();
            let returning_to_new_thread = !self.current_key.is_empty() && key.is_empty();
            self.launching_new_chat = false;
            let old_text = self.input.read(cx).text().to_string();
            if old_text.is_empty() {
                self.drafts.remove(&self.current_key);
            } else {
                self.drafts.insert(self.current_key.clone(), old_text);
            }
            let draft = self.drafts.get(&key).cloned().unwrap_or_default();
            self.current_key = key;
            // `failure` deliberately survives navigation: chat-scoped
            // failures render only under their own chat (see `failure_key`),
            // so switching away and back must not erase the one visible
            // trace of a failed send.
            self.wizard = None;
            // Attachments stay stashed under their chat key (the map swap IS
            // the navigation); only the transient chrome resets.
            self.preview = None;
            self.reset_mention(None, cx);
            // Route changes snap (round 5/6): a mode difference between the
            // old and new session's composer must not glide across
            // navigation. Killing the in-flight morph here isn't enough —
            // the nav-driven flip only commits AFTER the swapped draft has
            // been re-measured, one or two renders later, so the whole
            // window snaps (see ROUTE_SNAP_MS).
            self.height_morph = None;
            self.last_target_height = 0.0;
            if (new_thread_launch || returning_to_new_thread)
                && !motion::reduced_motion(cx)
                && self.last_rendered_height > 0.0
            {
                // Both directions share one timeline. The blank canvas is
                // always expanded; an established session begins compact.
                self.expanded_mode = returning_to_new_thread;
                let now_ms =
                    self.morph_clock.elapsed().as_secs_f32() * 1000.0 / motion::speed_scale();
                self.flip_morph = Some(FlipMorph::new_thread_transition(
                    self.last_rendered_height,
                    now_ms,
                ));
                self.route_snap_until = None;
            } else {
                self.flip_morph = None;
                self.last_rendered_height = 0.0;
                self.route_snap_until = Some(Instant::now() + Duration::from_millis(ROUTE_SNAP_MS));
            }
            self.input.update(cx, |input, cx| input.set_text(draft, cx));
            // Landing on a chat is activity there; the restored draft decides
            // the flag.
            self.report_user_activity(cx);
        }

        // A pending agent question must not take over an active queue edit.
        if self.editing_queued.is_some() {
            return;
        }
        // Question panel lifecycle (wizard state cached per request id).
        match pending {
            Some((request_id, questions)) if !self.answered_requests.contains(&request_id) => {
                let same = self
                    .wizard
                    .as_ref()
                    .is_some_and(|w| w.request_id == request_id);
                if !same {
                    self.reset_mention(None, cx);
                    self.wizard = Some(Wizard::new(request_id, questions));
                    self.advance_task = None;
                    // The shared input becomes the panel's free-text override.
                    self.input.update(cx, |input, cx| {
                        input.set_placeholder("Type your own answer, or pick an option above", cx)
                    });
                }
            }
            _ => {
                if let Some(wizard) = self.wizard.as_ref() {
                    // LATCH (original composer.tsx `inputLatch`): a transient
                    // fold/sync blip — or a steer appended behind the
                    // streaming entry — must not unmount the panel and lose
                    // the user's picks. Release only on explicit resolution
                    // (here or on another device) or when a NON-EMPTY
                    // transcript shows the question superseded (a newer
                    // assistant entry took over). Never on run death: the
                    // question stays answerable until answered — the engine
                    // delivers a dead run's answer as a resumed turn.
                    let transcript = self.state.read(cx).transcript.clone();
                    let released = input_request_resolved(&transcript, &wizard.request_id)
                        || (!transcript.is_empty()
                            && !self.answered_requests.contains(&wizard.request_id));
                    if released {
                        self.wizard = None;
                        self.advance_task = None;
                        self.input
                            .update(cx, |input, cx| input.set_placeholder("Do anything…", cx));
                    }
                }
            }
        }
        let input_context = message_input_context(self.wizard.is_some());
        self.input
            .update(cx, |input, cx| input.set_key_context(input_context, cx));
    }

    pub(crate) fn run_live(&self, cx: &App) -> bool {
        let s = self.state.read(cx);
        let Some(chat_id) = s.selected_chat.as_deref() else {
            return false;
        };
        matches!(
            s.indicator_for(chat_id, chrono::Utc::now()),
            Indicator::Working | Indicator::AwaitingInput
        )
    }

    /// New chats need a runnable agent, but may target the device's home
    /// directory without a project. Existing chats carry their own run config.
    fn send_blocked(&self, cx: &App) -> bool {
        if self.queue_edit_finishing {
            return true;
        }
        let state = self.state.read(cx);
        if state.review_comment_flush_pending(&self.current_key) || state.code_ref_load_pending(&self.current_key) {
            return true;
        }
        if state.selected_chat.is_some() {
            return false;
        }
        // New-chat canvas: needs a runnable agent. The
        // no-agents check only fires once the catalog is loaded — offline
        // and still-loading states must not block (the harness resolves from
        // the remembered default and the engine reports real failures).
        self.pickers.read(cx).no_agents_available()
    }

    fn button_mode(&self, cx: &App) -> SendButtonMode {
        if self.editing_queued.is_some() {
            return SendButtonMode::Send;
        }
        let has_text = composer_has_content(
            self.input.read(cx).text(),
            self.staged().len() + self.staged_appshots().len() + self.staged_skills().len(),
            self.staged_comments(cx).len() + self.state.read(cx).code_refs(&self.current_key).iter().filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File).count()
                + usize::from(self.state.read(cx).code_ref_load_pending(&self.current_key)),
        );
        send_button_mode(self.run_live(cx), has_text)
    }

    fn on_submit(&mut self, cx: &mut Context<Self>) {
        if self.commit_queue_edit(cx) {
            return;
        }
        if self.wizard.is_some() {
            // Enter inside the panel's free-text input submits the page.
            let typed = self.input.read(cx).text().trim().to_string();
            if let Some(w) = self.wizard.as_mut() {
                w.set_typed(typed);
            }
            self.wizard_advance(cx);
            return;
        }
        let text = self.input.read(cx).text().trim().to_string();
        let no_content = !composer_has_content(
            &text,
            self.staged().len() + self.staged_appshots().len() + self.staged_skills().len(),
            self.staged_comments(cx).len() + self.state.read(cx).code_refs(&self.current_key).iter().filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File).count(),
        );
        match self.button_mode(cx) {
            // Enter never stops a run: Stop mode implies an empty composer,
            // so a stray extra Enter right after sending landed an interrupt
            // on the just-dispatched prompt and the agent ate it silently
            // (issue #406). Stop stays on the button — and on Esc when
            // escape_stops_active_agent is enabled.
            SendButtonMode::Stop => {}
            _ if no_content => {}
            _ if self.send_blocked(cx) => {}
            SendButtonMode::Send => self.send(text, false, cx),
            // Busy: keep the message queued until the current turn ends.
            SendButtonMode::Queue => self.send(text, true, cx),
        }
    }

    /// Cmd/Ctrl+Enter remains an ordinary submit while the composer carries
    /// content. With a truly empty composer it instead activates the most
    /// recently queued row, and never turns an empty chord into Stop.
    fn on_modified_submit(&mut self, cx: &mut Context<Self>) {
        if self.commit_queue_edit(cx) {
            return;
        }
        let has_content = composer_has_content(
            self.input.read(cx).text(),
            self.staged().len() + self.staged_appshots().len() + self.staged_skills().len(),
            self.staged_comments(cx).len() + self.state.read(cx).code_refs(&self.current_key).iter().filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File).count(),
        );
        match modified_submit_target(has_content) {
            ModifiedSubmitTarget::SubmitContent => self.on_submit(cx),
            ModifiedSubmitTarget::ActivateLatestQueued => self.activate_latest_queued(cx),
        }
    }

    /// Queue a Run doc command with an optimistic echo — or, with the agent
    /// busy, park the message on the chat's pending queue instead. New chats
    /// thread the picked config in: worktree creation (when the isolated toggle
    /// is on), `Mutate createChat` with the `ChatConfig` + cwd, and the model /
    /// reasoning / options on the Run request itself (§1.7).
    fn send(&mut self, text: String, queue: bool, cx: &mut Context<Self>) {
        if self.sending { return; }
        let message_id = self.retry_send_id.take().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let failed_send = FailedSend {
            key: self.current_key.clone(), text: text.clone(), message_id: message_id.clone(), queued: queue,
            attachment_ids: self.staged().iter().map(|item| item.id.clone()).collect(),
            appshot_ids: self.staged_appshots().iter().map(|item| item.id.clone()).collect(),
            skills: self.staged_skills().to_vec(), comments: self.staged_comments(cx),
            code_refs: self.state.read(cx).code_refs(&self.current_key).to_vec(),
        };
        self.failed_send = None;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            self.failure_key = None; // global — meaningful on every chat
            self.failed_send = Some(failed_send);
            cx.notify();
            return;
        };
        // Chat id: existing selection, or client-minted for the new-chat canvas
        // (the chat then appears from the doc host once the doc materializes).
        let (chat_id, is_new) = match self.state.read(cx).selected_chat.clone() {
            Some(id) => (id, false),
            None => (uuid::Uuid::new_v4().to_string(), true),
        };
        // Where the new session runs (Current checkout / reuse an existing
        // worktree / fresh worktree off the picked base) — resolved NOW so
        // the async block needs no picker access.
        let plan = self.pickers.read(cx).checkout_plan();
        // New-chat worktree chip (`crate::workspace_chip`): taken (and reset
        // to Main checkout) only for an eligible new chat — existing chats, the
        // overview's panel (always an existing chat), non-git / remote
        // projects and explicit checkout-picker worktrees never see it.
        let workspace_choice = if is_new && self.pickers.read(cx).workspace_chip_eligible(cx) {
            Some(self.pickers.update(cx, |p, _| p.take_workspace_choice()))
        } else {
            None
        };
        // Fully-resolved model/reasoning/options — concrete values (chat config
        // or defaults), so the engine never has to guess a "default".
        let resolved = self.pickers.read(cx).resolved(cx);
        let existing_cwd = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.cwd.clone());
        // The PROJECT fixes the new chat's device + base folder — sessions are
        // minted onto the project's device, not necessarily this one. With no
        // project ("Don't work in a project") the composer's device pick is
        // the host and the session runs from `~` there.
        let space = self.state.read(cx).selected_space_row().cloned();
        let local_device_id = self.state.read(cx).local_device_id.clone();
        let target_device_id = self.state.read(cx).effective_device_id();
        let device_id = if is_new {
            target_device_id
                .clone()
                .unwrap_or_else(|| "local".to_string())
        } else {
            self.state
                .read(cx)
                .selected_chat_row()
                .map(|c| c.device_id.clone())
                .or_else(|| local_device_id.clone())
                .unwrap_or_else(|| "local".to_string())
        };
        // Uploads/read-backs target the chat's HOST device (forwardable RPCs);
        // for a new chat that's the target device (None when it's local).
        let host_device_id = if is_new {
            target_device_id
                .clone()
                .filter(|id| local_device_id.as_deref() != Some(id.as_str()))
        } else {
            self.state
                .read(cx)
                .selected_chat_row()
                .map(|c| c.device_id.clone())
        };
        let space_id = space.as_ref().map(|s| s.id.clone());
        let space_path = space.as_ref().map(|s| s.path.clone());
        if queue && !is_new {
            let capability = if self.staged().is_empty() && self.staged_appshots().is_empty() {
                capabilities::MESSAGE_QUEUE_V1
            } else {
                capabilities::MESSAGE_QUEUE_ATTACHMENTS_V1
            };
            if !engine.engine_info().supports(capability)
                || !self.state.read(cx).chat_host_supports(&chat_id, capability)
            {
                self.failure =
                    Some("Update the chat's engine to queue messages during a response.".into());
                cx.notify();
                return;
            }
        }
        // Snapshot-and-clear NOW (use-attachments.ts takeAttachments): the
        // strip empties the instant you hit send; a failure hands the files
        // back into the chat's stash.
        let ordinary_staged = self
            .attachments
            .remove(&self.current_key)
            .unwrap_or_default();
        let staged_appshots = self.appshots.remove(&self.current_key).unwrap_or_default();
        // Attached-skill chips are per-draft: taken now, handed back on
        // failure like the attachments.
        let staged_skills = self.skills.remove(&self.current_key).unwrap_or_default();
        let mut staged = ordinary_staged.clone();
        staged.extend(
            staged_appshots
                .iter()
                .map(|appshot| appshot.screenshot.clone()),
        );
        // `typed` keeps the user's own words for the failure hand-back below:
        // restoring the folded prompt would paste the comment block into the
        // input as literal text.
        let key = self.current_key.clone();
        let comments = self.state.update(cx, |state, cx| {
            let taken = state.take_review_comments(&key);
            if !taken.is_empty() {
                cx.notify();
            }
            taken
        });
        let typed = text.clone();
        let mentioned_files: HashSet<_> = file_mention_links(&typed).into_iter().filter(|link| !link.is_dir).map(|link| link.path).collect();
        let code_refs = self.state.update(cx, |state, cx| {
            let mut seen_files = HashSet::new();
            let references = state.take_code_refs(&key).into_iter().filter(|reference| {
                reference.reference.kind != mast_proto::CodeRefKind::File || (mentioned_files.contains(&reference.reference.location.path) && seen_files.insert(reference.reference.location.path.clone()))
            }).collect::<Vec<_>>();
            cx.notify();
            references
        });
        // Visible preamble ahead of the user's words (never hidden prompt
        // injection): the transcript, queue row and Run prompt all carry it.
        let text = with_skill_preamble(&staged_skills, &text);
        let text = crate::code_ref::with_code_refs(&text, &code_refs);
        let text = crate::comments::with_comments(&text, &comments);
        self.preview = None;
        self.instruction_reset.remember_send(&message_id, &typed);
        let created_at = chrono::Utc::now().timestamp_millis();
        // Existing busy chats always queue; compatibility was checked before
        // taking the draft, attachments, or review comments.
        let queue = queue && !is_new;
        let clean_queue_attachment_text = staged.is_empty()
            || (engine
                .engine_info()
                .supports(capabilities::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1)
                && self.state.read(cx).chat_host_supports(
                    &chat_id,
                    capabilities::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
                ));

        // Queued-attachment flow (durable-by-design): stage the bytes on the
        // LOCAL engine, queue the command immediately with `pending://` refs,
        // and let the engine push the bytes to a remote host afterwards —
        // staging must never gate the queue (2026-08-19 incident: a send
        // died with a zombie peer link because the upload sat in front of
        // QueueCommand). Requires every engine involved to understand the
        // ref scheme — the local engine (an IPC daemon may be older than
        // this UI) and, for remotely-hosted chats, the host; anything older
        // keeps the legacy blocking upload.
        let host_is_remote = host_device_id
            .as_deref()
            .is_some_and(|id| local_device_id.as_deref() != Some(id));
        // Queue rows do not carry upstream's attachment-transfer escort, so
        // they retain the proven host-upload path and store absolute refs.
        // Appshot XML attributes require escaped final paths. The engine's plain
        // string replacement of pending refs cannot safely rewrite those, so
        // rich captures use the existing upload-before-send path.
        let queued_flow = !queue && staged_appshots.is_empty() && !staged.is_empty() && {
            let state = self.state.read(cx);
            let local_ok = local_device_id
                .as_deref()
                .is_some_and(|id| state.device_version_at_least(id, QUEUED_ATTACHMENTS_MIN));
            let host_ok = !host_is_remote
                || host_device_id
                    .as_deref()
                    .is_some_and(|id| state.device_version_at_least(id, QUEUED_ATTACHMENTS_MIN));
            local_ok && host_ok
        };
        // Upload identities minted NOW: in the queued flow the `pending://`
        // ref IS the persisted transport until the host rewrites it, so the
        // id must exist before any bytes move.
        let upload_ids: Vec<String> = staged
            .iter()
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        // The echo carries attachment refs from the first frame, so photos
        // render while the send is still pending. Queued flow: the refs are
        // the real `pending://` identities (stable — no post-upload refresh).
        // Legacy flow: synthetic `pending/…` paths that the post-upload
        // refresh replaces with the host's absolute paths. Either way the
        // staged bytes are seeded into the transcript cache under every
        // device key the transcript consults.
        let echo_paths: Vec<String> = if queued_flow {
            staged
                .iter()
                .zip(&upload_ids)
                .map(|(att, id)| format!("pending://{id}/{}", att.name))
                .collect()
        } else {
            staged
                .iter()
                .map(|att| format!("pending/{}/{}", att.id, att.name))
                .collect()
        };
        let echo_appshot_paths: HashMap<String, String> = staged
            .iter()
            .zip(&echo_paths)
            .map(|(attachment, path)| (attachment.id.clone(), path.clone()))
            .collect();
        let echo_text = attachments::with_attachments(
            &appshots::with_appshots(&text, &staged_appshots, &echo_appshot_paths),
            &echo_paths,
        );
        // Queued flow also seeds the UPLOAD ALIAS: the host rewrites the
        // persisted ref to `{its uploads dir}/{id8}-{name}` — an absolute
        // path the sender can't predict, but whose id8 it minted. The alias
        // keeps the thumbnail on the already-local bytes through that
        // rewrite instead of blanking into a reload skeleton.
        if queued_flow {
            for (upload_id, att) in upload_ids.iter().zip(&staged) {
                attachments::seed_attachment_alias(
                    &device_id,
                    upload_id,
                    &att.name,
                    att.image.clone(),
                );
                if let Some(local) = local_device_id.as_deref()
                    && local != device_id
                {
                    attachments::seed_attachment_alias(
                        local,
                        upload_id,
                        &att.name,
                        att.image.clone(),
                    );
                }
            }
        }
        for (path, att) in echo_paths.iter().zip(&staged) {
            attachments::seed_attachment(&device_id, path, &att.name, att.image.clone());
            if let Some(local) = local_device_id.as_deref()
                && local != device_id
            {
                attachments::seed_attachment(local, path, &att.name, att.image.clone());
            }
        }

        // Optimistic echo (client-minted id doubles as the persisted message id,
        // so the doc frame dedups it away).
        let echo = SessionMessageEntry {
            id: message_id.clone(),
            role: mast_doc::MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: echo_text.clone(),
            }],
            created_at,
            device_id: "local".into(),
            status: None,
            continuation_of: None,
        };
        self.launching_new_chat = is_new;
        if is_new {
            cx.emit(ComposerEvent::NewThreadTransitionStarted);
        }
        // A queued message is not in the transcript yet — the queue panel is
        // its echo, and it gets a real bubble when the host sends it.
        self.state.update(cx, |s, cx| {
            if is_new {
                s.select_chat(Some(chat_id.clone()), cx);
            }
            if should_publish_optimistic_echo(queue) {
                s.push_echo(&chat_id, echo);
                // Working overlay until the host executes the queued command —
                // without it a remote send flashed Completed (and could ring
                // the done-chime) in the queue→drain→sync gap.
                s.begin_pending_send(&chat_id, &message_id, chrono::Utc::now());
            }
            cx.notify();
        });
        if let Some(choice) = workspace_choice {
            use crate::workspace_chip::{WorkspaceChoice, WorkspaceOutcome};
            let initial = match choice {
                WorkspaceChoice::Auto => WorkspaceOutcome::Deciding,
                WorkspaceChoice::Worktree => WorkspaceOutcome::Creating,
                WorkspaceChoice::MainCheckout => WorkspaceOutcome::Main {
                    plan: None,
                    note: None,
                    warning: false,
                },
            };
            self.pickers
                .update(cx, |p, cx| p.set_workspace_outcome(&chat_id, initial, cx));
        }
        // Jev (the worktree chip's Auto classifier) sees the skill preamble
        // too: an attached `/open-pr` or `/implement-ticket` is a strong
        // "this will change code" signal. The worktree NAME still derives
        // from the user's own words — a slug of "Load and follow these…"
        // would name every skill-attached worktree the same.
        let workspace_message = with_skill_preamble(&staged_skills, &typed);
        let worktree_name_source = if typed.trim().is_empty() {
            workspace_message.clone()
        } else {
            typed.clone()
        };

        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.drafts.remove(&self.current_key);
        self.report_user_activity(cx);
        self.failure = None;
        self.sending = true;
        // A queued row is represented by the queue panel, not the transcript.
        // Claiming an own-turn anchor for it here would replace the live
        // turn's runway with an id that has no transcript row yet.
        if should_publish_optimistic_echo(queue) {
            cx.emit(ComposerEvent::Sent {
                chat_id: chat_id.clone(),
                message_id: message_id.clone(),
            });
        }
        cx.notify();

        let restore_text = typed;
        let err_chat_id = chat_id.clone();
        let err_message_id = message_id.clone();
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let mut dispatch_attempted = false;
            let result: Result<Option<String>, String> = async {
                // Attachments stage FIRST — before the chat row or anything
                // else exists. Staging is chat-independent (keyed by
                // uploadId), and ordering it first makes a new-chat send
                // atomic: a staging failure aborts with NOTHING created,
                // instead of stranding a just-minted empty chat (v0.2.12
                // "failed to stage → empty transcript" report).
                //
                // Queued flow: commit the bytes to the LOCAL engine's uploads
                // dir (fast, offline-safe) — the queued command carries the
                // `pending://` refs and the engine delivers the bytes to a
                // remote host afterwards, retrying until they land. Legacy
                // flow (old engines): stage on the host device up front,
                // bounded by a total budget so a degraded link fails the send
                // loudly instead of grinding through silent per-chunk retries
                // for minutes.
                let mut content = text.clone();
                let mut attachment_paths: Vec<String> = Vec::new();
                let mut transfers: Vec<serde_json::Value> = Vec::new();
                if !staged.is_empty() && queued_flow {
                    // Local staging is disk-speed; publish progress anyway so
                    // huge files still narrate.
                    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total: u64 = staged.iter().map(|a| a.bytes().len() as u64).sum();
                    {
                        let progress = progress.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.begin_upload_progress(total, progress);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                    for (att, upload_id) in staged.iter().zip(&upload_ids) {
                        if let Err(err) = attachments::upload_attachment(
                            &engine,
                            cx.background_executor(),
                            None,
                            upload_id,
                            att,
                            Some(progress.clone()),
                        )
                        .await
                        {
                            tracing::warn!(name = %att.name, error = %err, "local attachment stage failed");
                            return Err("Couldn't stage the attachment locally.".to_string());
                        }
                        transfers.push(serde_json::json!({
                            "uploadId": upload_id,
                            "fileName": att.name,
                        }));
                    }
                    // The echo refs ARE the persisted refs — no refresh pass.
                    attachment_paths = echo_paths.clone();
                    content = echo_text.clone();
                } else if !staged.is_empty() {
                    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total: u64 = staged.iter().map(|a| a.bytes().len() as u64).sum();
                    {
                        let progress = progress.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.begin_upload_progress(total, progress);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                    for (att, upload_id) in staged.iter().zip(&upload_ids) {
                        match attachments::upload_attachment(
                            &engine,
                            cx.background_executor(),
                            host_device_id.as_deref(),
                            upload_id,
                            att,
                            Some(progress.clone()),
                        )
                        .await
                        {
                            Ok(path) => attachment_paths.push(path),
                            Err(err) => {
                                tracing::warn!(name = %att.name, error = %err, "attachment upload failed");
                                return Err(
                                    "Couldn't upload the attachment — the device may be offline."
                                        .to_string(),
                                );
                            }
                        }
                    }
                    // Seed the transcript cache from local bytes so the sent
                    // bubble's thumbnails never round-trip (seedTranscript-
                    // Attachment in the original send path).
                    let seed_device = host_device_id.clone().unwrap_or_else(|| device_id.clone());
                    for (path, att) in attachment_paths.iter().zip(&staged) {
                        attachments::seed_attachment(&seed_device, path, &att.name, att.image.clone());
                        if seed_device != device_id {
                            attachments::seed_attachment(&device_id, path, &att.name, att.image.clone());
                        }
                    }
                    let appshot_paths: HashMap<String, String> = staged
                        .iter()
                        .zip(&attachment_paths)
                        .map(|(attachment, path)| (attachment.id.clone(), path.clone()))
                        .collect();
                    content = attachments::with_attachments(
                        &appshots::with_appshots(&text, &staged_appshots, &appshot_paths),
                        &attachment_paths,
                    );
                    // A normal send already has an optimistic echo: refresh it
                    // in place with the uploaded refs so its thumbnails never
                    // flicker. A queued message has no transcript echo at all;
                    // its queue row is the only representation until dispatch.
                    if should_publish_optimistic_echo(queue) {
                        let refreshed = SessionMessageEntry {
                            id: message_id.clone(),
                            role: mast_doc::MessageRole::User,
                            parts: vec![MessagePart::Text {
                                id: "t0".into(),
                                text: content.clone(),
                            }],
                            created_at,
                            device_id: "local".into(),
                            status: None,
                            continuation_of: None,
                        };
                        let echo_chat_id = chat_id.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.remove_echo(&echo_chat_id, &message_id);
                                s.push_echo(&echo_chat_id, refreshed);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                }

                // Resolve the working directory: existing chats keep theirs;
                // new chats run per the checkout plan (t3code env-mode): the
                // space's folder as-is, an EXISTING worktree of the picked ref
                // (a plain cwd override — multiple sessions share one
                // worktree), or a fresh isolated worktree created off the
                // picked base ref (CreateWorktree on send, targeted at the
                // space's device; the RPC relay-forwards).
                let mut cwd = if is_new {
                    // Project-less sessions run from the host's home dir —
                    // "~" is expanded on the host when the run spawns.
                    space_path.clone().or_else(|| Some("~".to_string()))
                } else {
                    existing_cwd
                }
                .unwrap_or_else(|| ".".to_string());
                let mut worktree_cwd: Option<String> = None;
                // Fresh-worktree plans ride the QUEUED Run command (a
                // WorktreeSpec the HOST materializes at drain time) instead of
                // a blocking CreateWorktree relay RPC here: the RPC had no
                // timeout, so a lost relay frame wedged the send on "Sending…"
                // forever while the session ran remotely anyway (2026-08-18).
                let mut run_worktree: Option<mast_proto::WorktreeSpec> = None;
                // The picked ref rides createChat so the session footer names
                // it from the first frame (it read "Select ref" until the
                // host's diff reconciler got around to stamping the branch).
                let mut chat_branch: Option<String> = None;
                if is_new && space_path.is_some() {
                    match &plan {
                        crate::pickers::CheckoutPlan::CurrentCheckout { branch } => {
                            chat_branch = branch.clone();
                        }
                        crate::pickers::CheckoutPlan::ReuseWorktree { path, branch } => {
                            cwd = path.clone();
                            worktree_cwd = Some(path.clone());
                            chat_branch = Some(branch.clone());
                        }
                        crate::pickers::CheckoutPlan::NewWorktree { base } => {
                            // Footer shows the base until the host stamps the
                            // actual zeron/<name> branch post-creation. cwd
                            // stays the repo folder — an old host that doesn't
                            // know the spec degrades to the main checkout
                            // instead of failing the run.
                            chat_branch = base.clone();
                            if let Some(repo_path) = &space_path {
                                // A remote repo's branch list loads over the
                                // relay — on a bad link it may never arrive
                                // and the picker has no base. That must NOT
                                // silently drop the isolation the user picked
                                // (2026-08-19: "New worktree" ran in the main
                                // checkout): default to HEAD, which git — any
                                // host version — resolves as the repo's
                                // current checkout state.
                                let base =
                                    base.clone().unwrap_or_else(|| "HEAD".to_string());
                                run_worktree = Some(mast_proto::WorktreeSpec {
                                    repo_path: repo_path.clone(),
                                    base,
                                    space_id: space_id.clone(),
                                });
                            }
                        }
                    }
                }

                // Best-effort Mutate createChat with the picked config: the
                // engine resolves device + cwd from the PROJECT row when one
                // is picked; project-less chats name the host device outright
                // (idempotent; the doc host would materialize the chat on
                // first command anyway, so failures are non-fatal).
                if is_new {
                    let mut mutate = serde_json::json!({
                        "op": "createChat",
                        "chatId": chat_id,
                    });
                    if let Some(object) = mutate.as_object_mut() {
                        match &space_id {
                            Some(space_id) => {
                                object.insert(
                                    "spaceId".into(),
                                    serde_json::Value::String(space_id.clone()),
                                );
                            }
                            None => {
                                object.insert(
                                    "deviceId".into(),
                                    serde_json::Value::String(device_id.clone()),
                                );
                            }
                        }
                    }
                    if let Some(object) = mutate.as_object_mut() {
                        if let Some(worktree_cwd) = &worktree_cwd {
                            object.insert(
                                "cwd".into(),
                                serde_json::Value::String(worktree_cwd.clone()),
                            );
                        }
                        if let Some(branch) = &chat_branch {
                            object.insert(
                                "branch".into(),
                                serde_json::Value::String(branch.clone()),
                            );
                        }
                        if let Some(config) = resolved.chat_config()
                            && let Ok(config) = serde_json::to_value(&config)
                        {
                            object.insert("config".into(), config);
                        }
                    }
                    if let Err(err) = attachments::call_with_timeout(
                        &engine,
                        cx.background_executor(),
                        methods::MUTATE,
                        mutate,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    {
                        tracing::warn!(error = %err, "CreateChat mutate unavailable; doc host will materialize the chat");
                    }
                }

                // Worktree chip pre-dispatch step (new chats only; after
                // createChat so the engine has a row to stamp the worktree
                // cwd onto). Never fails the send: every error path falls
                // back to the base checkout.
                if let (Some(choice), Some(repo_path)) = (workspace_choice, &space_path) {
                    use crate::workspace_chip as wc;
                    let set_outcome = |outcome: wc::WorkspaceOutcome, failure: Option<String>, cx: &mut gpui::AsyncApp| {
                        let chat_id = chat_id.clone();
                        this.update(cx, |composer, cx| {
                            composer
                                .pickers
                                .update(cx, |p, cx| p.set_workspace_outcome(&chat_id, outcome, cx));
                            if let Some(failure) = failure {
                                composer.failure = Some(failure.into());
                                composer.failure_key = Some(chat_id.clone());
                            }
                            cx.notify();
                        })
                        .ok();
                    };
                    let mut create = choice.first_step() == wc::PreSendStep::Create;
                    let mut plan_reply: Option<wc::PlanReply> = None;
                    if choice.first_step() == wc::PreSendStep::Plan {
                        let planned = attachments::call_with_timeout(
                            &engine,
                            cx.background_executor(),
                            wc::PLAN_CHAT_WORKSPACE,
                            serde_json::json!({ "message": workspace_message, "cwd": repo_path }),
                            wc::PLAN_TIMEOUT,
                        )
                        .await
                        .and_then(|value| wc::decode_plan(&value));
                        if wc::should_create_after_plan(&planned) {
                            create = true;
                            plan_reply = planned.ok();
                            set_outcome(wc::WorkspaceOutcome::Creating, None, cx);
                        } else {
                            if let Err(err) = &planned {
                                tracing::warn!(error = %err, "PlanChatWorkspace failed; using the main checkout");
                            }
                            set_outcome(wc::main_after_plan(planned), None, cx);
                        }
                    }
                    if create {
                        let created = attachments::call_with_timeout(
                            &engine,
                            cx.background_executor(),
                            wc::CREATE_CHAT_WORKTREE,
                            serde_json::json!({
                                "chatId": chat_id,
                                "repoPath": repo_path,
                                "name": wc::worktree_name(&worktree_name_source),
                            }),
                            wc::CREATE_TIMEOUT,
                        )
                        .await;
                        let resolution = wc::resolve_create(created, plan_reply);
                        if let Some(worktree) = resolution.cwd {
                            // The engine already stamped the chat's dispatch
                            // cwd; the Run request agrees with it.
                            cwd = worktree;
                        } else if let Some(failure) = &resolution.failure {
                            tracing::warn!(error = %failure, "CreateChatWorktree failed; dispatching in the base checkout");
                        }
                        set_outcome(resolution.outcome, resolution.failure, cx);
                    }
                }

                if queue {
                    // A queue row is editable UI state, so its text must stay
                    // free of the internal attachment-path trailer. The host
                    // rebuilds that transport when it promotes the row.
                    let appshot_paths = staged.iter().zip(&attachment_paths)
                        .map(|(attachment, path)| (attachment.id.clone(), path.clone())).collect();
                    let queue_body = appshots::with_appshots(&text, &staged_appshots, &appshot_paths);
                    let queue_text = if !clean_queue_attachment_text {
                        content.as_str()
                    } else if queue_body.trim().is_empty() && !attachment_paths.is_empty() {
                        attachments::ATTACHMENT_ONLY_TEXT
                    } else {
                        queue_body.as_str()
                    };
                    let params = serde_json::json!({
                        "chatId": chat_id,
                        "messageId": message_id,
                        "text": queue_text,
                        "attachments": attachment_paths,
                        "holdForTurnEnd": true,
                    });
                    let reply = engine
                        .client()
                        .call(methods::QUEUE_MESSAGE, params)
                        .await
                        // A pinned row the host can't deliver is refused
                        // with an actionable instruction-delivery message.
                        .map_err(|e| instruction_reset::send_failure_text(&e.to_string()))?;
                    let queue_id = reply
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| "Send failed: queue did not return an id".to_string())?;
                    return Ok(Some(queue_id.to_string()));
                }

                let expects_setup_handoff = run_worktree
                    .as_ref()
                    .and_then(|spec| spec.space_id.as_ref())
                    .is_some();
                let command = SessionCommandPayload::Run {
                    request: RunRequest {
                        prompt: content.clone(),
                        harness: resolved.harness,
                        model: resolved.model.clone(),
                        reasoning: resolved.reasoning,
                        model_options: resolved.model_options.clone(),
                        cwd,
                        sandbox: SandboxLevel::WorkspaceWrite,
                        // The chat's persisted approval mode (chip in the
                        // pickers row; new-chat draft on first send).
                        auto_approve: resolved.auto_approve,
                        resume: None,
                        attachments: attachment_paths,
                        worktree: run_worktree,
                        instructions: None,
                    },
                    message_id: message_id.clone(),
                };
                let command = serde_json::to_value(&command)
                    .map_err(|e| format!("Send failed: {e}"))?;
                let mut params = serde_json::json!({ "chatId": chat_id, "command": command, "commandId": message_id });
                if !transfers.is_empty() {
                    params["transfers"] = serde_json::Value::Array(transfers);
                }
                // Deadline-bounded: QueueCommand is a local write (in-process
                // or IPC), but a deferred engine handle can park forever.
                dispatch_attempted = true;
                let queued = attachments::call_with_timeout(
                    &engine,
                    cx.background_executor(),
                    methods::QUEUE_COMMAND,
                    params,
                    std::time::Duration::from_secs(30),
                )
                .await
                // Ingestion refuses an instruction-bearing send to a host
                // that can't deliver it (`instructionHostUnsupported`): show
                // its actionable message; the failure path keeps the draft.
                .map_err(|e| instruction_reset::send_failure_text(&e))?;
                if expects_setup_handoff
                    && let Some(command_id) = queued
                        .get("commandId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                {
                    let poll_engine = engine.clone();
                    let poll_chat_id = chat_id.clone();
                    let poll_target_device_id = host_device_id.clone();
                    this.update(cx, |_, cx| {
                        cx.spawn(async move |this, cx| {
                            for _ in 0..480 {
                                let mut params = serde_json::json!({
                                    "chatId": poll_chat_id,
                                    "commandId": command_id,
                                });
                                if let (Some(target), Some(object)) =
                                    (&poll_target_device_id, params.as_object_mut())
                                {
                                    object.insert(
                                        "targetDeviceId".into(),
                                        serde_json::Value::String(target.clone()),
                                    );
                                }
                                match attachments::call_with_timeout(
                                    &poll_engine,
                                    cx.background_executor(),
                                    methods::TAKE_PROJECT_ACTION_SETUP,
                                    params,
                                    std::time::Duration::from_secs(10),
                                )
                                .await
                                {
                                    Ok(value) if value.get("ready").and_then(|v| v.as_bool()) == Some(true) => {
                                        let setup_action = value
                                            .get("setupAction")
                                            .cloned()
                                            .filter(|value| !value.is_null())
                                            .and_then(|value| serde_json::from_value(value).ok());
                                        let setup_error = value
                                            .get("setupError")
                                            .and_then(|value| value.as_str())
                                            .map(str::to_string);
                                        this.update(cx, |_, cx| {
                                            cx.emit(ComposerEvent::WorktreeSetup {
                                                chat_id: poll_chat_id.clone(),
                                                setup_action,
                                                setup_error,
                                                target_device_id: poll_target_device_id.clone(),
                                            });
                                        })
                                        .ok();
                                        return;
                                    }
                                    Err(error) if error.starts_with("unknown method: ") => return,
                                    Ok(_) | Err(_) => {}
                                }
                                cx.background_executor()
                                    .timer(Duration::from_millis(250))
                                    .await;
                            }
                            tracing::warn!(
                                chat = %poll_chat_id,
                                command = %command_id,
                                "worktree setup handoff timed out"
                            );
                        })
                        .detach();
                    })
                    .ok();
                }
                Ok(None)
            }
            .await;
            if queue && let Err(error) = &result {
                tracing::warn!(chat = %err_chat_id, %error, "queue message failed");
            }
            if result.is_err() && is_new && !dispatch_attempted {
                // A failed new-chat send must not strand a just-minted empty
                // chat in the sidebar (v0.2.12 "empty transcript" report).
                // Staging now runs before CreateChat, so usually nothing was
                // created — but a post-mutate failure (QueueCommand) still
                // leaves a row. Best-effort delete; a no-op if the chat was
                // never materialized.
                let _ = attachments::call_with_timeout(
                    &engine,
                    cx.background_executor(),
                    methods::MUTATE,
                    serde_json::json!({ "op": "deleteChat", "chatId": err_chat_id }),
                    std::time::Duration::from_secs(5),
                )
                .await;
            }
            this.update(cx, |composer, cx| {
                composer.sending = false;
                composer
                    .state
                    .update(cx, |s, _| s.end_upload_progress());
                if let Ok(Some(message_id)) = &result {
                    cx.emit(ComposerEvent::Queued {
                        chat_id: err_chat_id.clone(),
                        message_id: message_id.clone(),
                    });
                }
                if let Err(message) = result {
                    // Failure: red banner, echo removed, prompt back in the
                    // draft, staged files back in the stash. A failed NEW
                    // chat restores to the CANVAS (key "") and navigates back
                    // there — the minted chat is gone (deleted above), so
                    // nothing may restore under its key.
                    let restore_key = if is_new && !dispatch_attempted {
                        String::new()
                    } else {
                        err_chat_id.clone()
                    };
                    let mut failed_send = failed_send;
                    failed_send.key = restore_key.clone();
                    composer.failed_send = Some(failed_send);
                    composer.state.update(cx, |s, cx| {
                        s.remove_echo(&err_chat_id, &err_message_id);
                        s.end_pending_send(&err_chat_id, &err_message_id);
                        if is_new && !dispatch_attempted && s.selected_chat.as_deref() == Some(err_chat_id.as_str()) {
                            // Back to the canvas; the navigation draft-swap
                            // loads the restored draft below.
                            s.select_chat(None, cx);
                        }
                        for comment in &comments {
                            s.add_review_comment(&restore_key, comment.clone());
                        }
                        for reference in &code_refs { s.add_code_ref(&restore_key, reference.clone()); }
                        cx.notify();
                    });
                    composer.restore_failed_text(&restore_key, &restore_text, &message, cx);
                    if !staged_skills.is_empty() {
                        // Chips attached while the send was in flight stay;
                        // the failed send's chips rejoin them (deduped).
                        let slot = composer.skills.entry(restore_key.clone()).or_default();
                        let fresh = std::mem::take(slot);
                        for name in staged_skills.iter().chain(&fresh) {
                            attach_skill(slot, name);
                        }
                    }
                    if !ordinary_staged.is_empty() {
                        // Merge by id (stashAttachments): files the user staged
                        // while the send was in flight survive the hand-back —
                        // draining the minted chat's slot too when the restore
                        // target is the canvas.
                        let mut merged = ordinary_staged.clone();
                        for key in [err_chat_id.clone(), restore_key.clone()] {
                            if let Some(slot) = composer.attachments.get_mut(&key) {
                                let fresh: Vec<_> = slot
                                    .drain(..)
                                    .filter(|e| !merged.iter().any(|f| f.id == e.id))
                                    .collect();
                                merged.extend(fresh);
                            }
                        }
                        composer.attachments.insert(restore_key.clone(), merged);
                    }
                    composer.restore_failed_appshots(&staged_appshots, &err_chat_id, &restore_key);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn restore_failed_text(&mut self, key: &str, text: &str, error: &str, cx: &mut Context<Self>) {
        self.failure = Some(error.to_owned().into());
        self.failure_key = Some(key.to_owned());
        let merge = |existing: &str| {
            if existing.is_empty() { text.to_owned() }
            else if text.is_empty() { existing.to_owned() }
            else { format!("{text}\n\n{existing}") }
        };
        if self.current_key == key {
            let restored = merge(self.input.read(cx).text());
            self.input.update(cx, |input, cx| input.set_text(restored, cx));
        } else {
            let restored = merge(self.drafts.get(key).map(String::as_str).unwrap_or(""));
            self.drafts.insert(key.to_owned(), restored);
        }
    }

    pub(crate) fn interrupt_selected(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        self.interrupt_chat(chat_id, cx);
    }

    pub(crate) fn interrupt_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if !begin_interrupt(&mut self.interrupting, &chat_id) {
            return;
        }
        let params = interrupt_params(&chat_id);
        let task_chat_id = chat_id.clone();
        let failure_chat = chat_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::QUEUE_COMMAND, params).await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.interrupting.remove(&task_chat_id);
                    composer.failure = Some(format!("Stop failed: {err}").into());
                    composer.failure_key = Some(failure_chat);
                    cx.notify();
                })
                .ok();
            }
        });
        self.interrupt_tasks.insert(chat_id, task);
    }

    pub(crate) fn is_interrupting(&self, chat_id: &str) -> bool {
        self.interrupting.contains(chat_id)
    }

    // ---- wizard glue ----

    fn wizard_select(&mut self, option_ix: usize, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let step = wizard.select(option_ix);
        let has_pick = wizard.page_has_pick();
        self.input.update(cx, |input, cx| {
            input.set_placeholder(
                if has_pick {
                    "Type your own answer, or leave this blank to use the selected option"
                } else {
                    "Type your own answer, or pick an option above"
                },
                cx,
            )
        });
        match step {
            WizardStep::AutoAdvance => self.schedule_auto_advance(cx),
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            WizardStep::Stay => {}
        }
        cx.notify();
    }

    fn schedule_auto_advance(&mut self, cx: &mut Context<Self>) {
        self.advance_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(AUTO_ADVANCE_MS))
                .await;
            this.update(cx, |composer, cx| composer.wizard_advance(cx))
                .ok();
        }));
    }

    fn wizard_advance(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        match wizard.advance() {
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            _ => {
                // Moving on: clear the shared free-text input for the next page.
                self.input.update(cx, |input, cx| input.set_text("", cx));
                cx.notify();
            }
        }
    }

    fn wizard_back(&mut self, cx: &mut Context<Self>) {
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.back();
            cx.notify();
        }
    }

    /// Submit RespondInput and retire the panel.
    fn wizard_finish(&mut self, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.take() else {
            return;
        };
        self.advance_task = None;
        self.answered_requests.insert(wizard.request_id.clone());
        self.input.update(cx, |input, cx| {
            input.set_text("", cx);
            // The panel borrowed the composer input; hand back its identity.
            input.set_placeholder("Do anything…", cx);
            input.set_key_context(MESSAGE_COMPOSER_CONTEXT, cx);
        });
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        let request_id = wizard.request_id.clone();
        let command = SessionCommandPayload::RespondInput {
            request_id: request_id.clone(),
            answers,
        };
        let failure_chat = chat_id.clone();
        let params = match serde_json::to_value(&command) {
            Ok(value) => serde_json::json!({ "chatId": chat_id, "command": value }),
            Err(_) => return,
        };
        // `action_task`, NOT `send_task` — see `interrupt`.
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::QUEUE_COMMAND, params).await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Answer failed: {err}").into());
                    composer.failure_key = Some(failure_chat);
                    // The answer never left this device — put the panel back.
                    composer.answered_requests.remove(&request_id);
                    cx.notify();
                })
                .ok();
                return;
            }
            // Safety net against a dead-looking session: the command queued,
            // but the host may still REJECT it (e.g. the run's resolver is
            // gone). If the very same request is still the live pending input
            // once the host has had ample time to execute and the resolved
            // flag to sync back, the answer demonstrably didn't take —
            // un-hide the panel instead of leaving the question unanswerable.
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |composer, cx| {
                let transcript = composer.state.read(cx).transcript.clone();
                let still_pending = pending_input_request(&transcript)
                    .is_some_and(|(pending_id, _)| pending_id == request_id);
                if still_pending && composer.answered_requests.remove(&request_id) {
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_wizard_key(&mut self, event: &KeyDownEvent, window: &Window, cx: &mut Context<Self>) {
        // Keys bubbling out of the free-text input must not double-handle:
        // digits select options only while the input is empty, and Enter is the
        // input's own Submit action when it has focus.
        let input_focused = self.input.read(cx).focus_handle.is_focused(window);
        let input_empty = self.input.read(cx).is_empty();
        let key = event.keystroke.key.as_str();
        // A BARE digit picks an option. With a modifier held the keystroke
        // belongs to an app shortcut — ⌘1..⌘9 jump to a sidebar row — and the
        // panel must not also consume it as a selection.
        if let Ok(digit) = key.parse::<usize>()
            && (1..=9).contains(&digit)
            && !event.keystroke.modifiers.modified()
        {
            if !input_focused || input_empty {
                self.wizard_select(digit - 1, cx);
                // Consumed as a selection: stop the platform from also
                // inserting the digit into the focused free-text input.
                cx.stop_propagation();
            }
        } else if key == "enter" {
            if !input_focused {
                self.wizard_advance(cx);
                cx.stop_propagation();
            }
        } else if key == "escape" {
            if wizard_escape_goes_back(key, input_focused, input_empty) {
                self.wizard_back(cx);
            }
            cx.stop_propagation();
        }
    }

    // ---- render pieces ----

    /// The agent-asked-a-question panel (zeron question-panel.tsx), rendered in
    /// place of the composer: the same floating-pill chrome (`rounded-[26px]
    /// border-white/[0.08] bg-white/[0.03] shadow-xl`), uppercase header +
    /// "1/3" counter chip, option rows with number kbd chips, a free-text
    /// override over a hairline, and Back / Next-Submit footer.
    fn render_wizard(&mut self, window: &gpui::Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(wizard) = self.wizard.clone() else {
            return gpui::Empty.into_any_element();
        };
        let counter = wizard.counter();
        let Some(question) = wizard.current().cloned() else {
            return gpui::Empty.into_any_element();
        };
        let page = wizard.page;
        let last = page + 1 >= wizard.questions.len();
        let typed_empty = self.input.read(cx).is_empty();
        let can_advance = wizard.page_has_pick() || !typed_empty;

        let options = question.options.iter().enumerate().map(|(ix, label)| {
            // Selection reads on the row only while no typed override exists
            // (typed answers win — zeron question-panel.tsx `isSel`).
            let picked = wizard.is_picked(ix) && typed_empty;
            div()
                .id(SharedString::from(format!("wizard-option-{ix}")))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(if picked {
                    crate::theme::ink(0.16)
                } else {
                    gpui::transparent_black()
                })
                // zeron question-panel.tsx option rows: `transition-colors`.
                .bg(if picked {
                    crate::theme::ink(0.09)
                } else {
                    motion::hover_blend(
                        &format!("wizard-option-{ix}"),
                        crate::theme::ink(0.025),
                        crate::theme::ink(0.06),
                    )
                })
                .on_hover(motion::hover_listener(format!("wizard-option-{ix}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.wizard_select(ix, cx)))
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(label.clone()))
                .aria_selected(picked)
                .debug_selector(move || format!("wizard-option-{ix}"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(13.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if picked {
                            theme.text
                        } else {
                            theme.text.opacity(0.9)
                        })
                        .child(SharedString::from(label.clone())),
                )
                .when(ix < 9, |el| {
                    el.child(
                        // Number kbd chip: `size-[22px] rounded-md text-[11px]`.
                        div()
                            .flex_none()
                            .size(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(if picked {
                                crate::theme::ink(0.16)
                            } else {
                                crate::theme::ink(0.05)
                            })
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(if picked {
                                theme.text
                            } else {
                                theme.text_muted.opacity(0.6)
                            })
                            .child(SharedString::from(format!("{}", ix + 1))),
                    )
                })
        });

        crate::components::conversation_wizard_surface(div()
            .id("question-panel")
            .debug_selector(|| "question-panel".into())
            .role(gpui::Role::Group)
            .when(window.is_a11y_active(), |el| {
                el.aria_label(SharedString::from(question.header.clone()))
            })
            .track_focus(&self.wizard_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_wizard_key(event, window, cx)
            })), &theme, COMPOSER_RADIUS)
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(16.0))
                    .flex()
                    .flex_col()
                    // Header: tracked uppercase + counter chip when paged.
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(crate::popover::tracked_upper(
                                        &question.header,
                                    ))),
                            )
                            .when(wizard.questions.len() > 1, |el| {
                                el.child(
                                    div()
                                        .h(px(20.0))
                                        .px(px(6.0))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.0))
                                        .bg(crate::theme::ink(0.06))
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text_muted.opacity(0.6))
                                        .child(SharedString::from(counter)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(crate::typography::ui_rems(15.0))
                            .line_height(px(20.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(question.question.clone())),
                    )
                    .when(question.multi_select, |el| {
                        el.child(
                            div()
                                .mt(px(4.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from("Select one or more options.")),
                        )
                    })
                    .child(
                        div()
                            .mt(px(12.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .children(options),
                    )
                    // Free-text override over a hairline (shares the composer
                    // input entity).
                    .child(
                        div()
                            .mt(px(12.0))
                            .border_t_1()
                            .border_color(crate::theme::hairline(0.06))
                            .pt(px(12.0))
                            .pb(px(4.0))
                            .px(px(4.0))
                            .child(self.input.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .px(px(16.0))
                    .pb(px(16.0))
                    .pt(px(4.0))
                    .child(if page > 0 {
                        crate::popover::btn_ghost(&theme, "Back", "wizard-back")
                            .id("wizard-back")
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_back(cx)))
                            .into_any_element()
                    } else {
                        gpui::Empty.into_any_element()
                    })
                    .child(
                        crate::popover::btn_primary(&theme, if last { "Submit" } else { "Next" })
                            .id("wizard-submit")
                            .role(gpui::Role::Button)
                            .aria_label(if last { "Submit" } else { "Next" })
                            .debug_selector(|| "wizard-submit".into())
                            .px(px(16.0))
                            .when(!can_advance, |el| el.opacity(0.4))
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_advance(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_send_button(
        &mut self,
        mode: SendButtonMode,
        identity: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx);
        let stop = mode == SendButtonMode::Stop;
        let blocked = !stop && self.send_blocked(cx);
        crate::components::conversation_send_key(theme, identity, stop, mode == SendButtonMode::Queue)
            .when(blocked, |el| el.opacity(0.35))
            .when(!blocked, |el| {
                el.cursor_pointer().hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if stop { this.interrupt_selected(cx); } else { this.on_submit(cx); }
                    }))
            })
            .into_any_element()
    }
}

/// The completion popups' floating rails run through
/// [`crate::popover::rail`]: the shared `popup_bar` state plus whichever
/// popup's rows are mounted — see [`Composer::active_popup_scroll`].
impl crate::popover::ScrollRailHost for Composer {
    fn rail_bar(&mut self) -> &mut crate::popover::MenuScrollbarState {
        &mut self.popup_bar
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.active_popup_scroll()
    }
}

/// Focus lands on the prompt input (window-level focus fallbacks — e.g. after
/// the focused terminal panel is hidden — route here).
impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_pending = false;
            let focus = self.input.focus_handle(cx);
            window.focus(&focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let wizard_active = self.wizard.is_some();
        let pickers = self.pickers.read(cx);
        let identity = theme.agent_identity_color(pickers.effective_harness(cx), pickers.identity_model_id(cx));
        let identity_text = crate::components::conversation_input_text_color(&theme, identity);
        if self.mention.token.is_some()
            && (wizard_active || !self.input.focus_handle(cx).is_focused(window))
        {
            self.reset_mention(None, cx);
        }
        if self.slash.token.is_some()
            && (wizard_active || !self.input.focus_handle(cx).is_focused(window))
        {
            self.reset_slash(None, cx);
        }
        let mode = self.button_mode(cx);
        let text_size = crate::components::conversation_composer_text_size(&theme, INPUT_TEXT_SIZE);
        // Shape the current draft before sizing the pill. Waiting for the child
        // layout leaves the parent using the previous edit's height.
        self.input.update(cx, |input, cx| {
            if input.text_size != text_size {
                input.text_size = text_size;
                input.needs_measure = true;
            }
            if input.identity_text != identity_text {
                input.identity_text = identity_text;
                input.needs_measure = true;
            }
            if input.needs_measure && input.last_width > 0.0 {
                let mut style = window.text_style();
                style.font_family = theme.font_sans.clone();
                style.font_size = crate::typography::ui_rems(text_size).into();
                style.color = if let Some(color) = identity_text { color } else if input.content.is_empty() {
                    theme.text_faint
                } else {
                    theme.text
                };
                input.layout_text(px(input.last_width), &style, window, cx);
            }
        });
        let (text_width, has_newline, content_height, last_width, epoch) = {
            let input = self.input.read(cx);
            (
                input.measured_text_width(),
                input.has_newline(),
                input.measured_content_height(),
                input.last_width,
                input.layout_epoch,
            )
        };
        let content_height = crate::components::conversation_composer_content_height(&theme, content_height, INPUT_LINE_HEIGHT);
        let now = Instant::now();
        // Only measurements taken *after* the last flip may drive the next one
        // (at most one flip per layout pass — a flip invalidates the widths).
        let measured_since_flip = epoch > self.flip_epoch && last_width > 0.0;
        if measured_since_flip {
            // A same-mode width change is an interactive window/pane resize:
            // defer collapse until sizes settle for RESIZE_SETTLE_MS. Expansion
            // remains live so compact controls never squeeze the input away.
            if self.last_seen_width > 0.0 && (last_width - self.last_seen_width).abs() > 0.5 {
                self.width_changed_at = Some(now);
            }
            self.last_seen_width = last_width;
            if self.expanded_mode {
                if self.expanded_anchor <= 0.0 {
                    self.expanded_anchor = last_width;
                }
            } else {
                // The compact pill's content box is the layout-stable capacity
                // both thresholds measure against.
                self.compact_capacity = last_width - 8.0;
            }
        }
        let resizing = self
            .width_changed_at
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(RESIZE_SETTLE_MS));
        if resizing && self.settle_task.is_none() {
            // Re-evaluate once the settle window has passed.
            self.settle_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(RESIZE_SETTLE_MS + 20))
                    .await;
                this.update(cx, |composer, cx| {
                    composer.settle_task = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        // Layout-stable compact capacity: measured directly while compact;
        // while expanded, the learned value shifted by any container resize
        // (the expanded input width tracks the container 1:1).
        let capacity = if !self.expanded_mode {
            if last_width > 0.0 {
                last_width - 8.0
            } else {
                f32::MAX // before first measure default to compact
            }
        } else if self.compact_capacity > 0.0 {
            if self.expanded_anchor > 0.0 && last_width > 0.0 {
                self.compact_capacity + (last_width - self.expanded_anchor)
            } else {
                self.compact_capacity
            }
        } else {
            f32::MAX
        };
        let next = composer_flip(
            self.expanded_mode,
            text_width,
            capacity,
            has_newline,
            resizing,
        );
        let committed_flip = next != self.expanded_mode && measured_since_flip;
        if committed_flip {
            self.expanded_mode = next;
            self.flip_epoch = epoch;
            self.expanded_anchor = 0.0;
            // The mode change moves the input width; don't read that jump as
            // an interactive resize.
            self.last_seen_width = 0.0;
        }
        // New chats render expanded regardless of `expanded_mode` (see below),
        // so a mode flip there changes nothing visible — never morph it.
        let new_chat = self.state.read(cx).selected_chat.is_none();
        let sign_in_notice = self.refresh_sign_in_status(cx);
        // Morph clock in ms; dividing by the measurement knob stretches the
        // timeline exactly like shell.rs eval_tween's scaled duration.
        let now_ms = self.morph_clock.elapsed().as_secs_f32() * 1000.0 / motion::speed_scale();
        let route_snap = self
            .route_snap_until
            .is_some_and(|until| Instant::now() < until);
        let dock_height_changed = std::mem::take(&mut self.dock_height_changed);
        self.flip_morph =
            if dock_height_changed || self.dock_frame.is_some_and(|frame| frame.active) {
                None
            } else {
                flip_morph_step(
                    self.flip_morph,
                    committed_flip && !new_chat,
                    self.last_rendered_height,
                    now_ms,
                    motion::reduced_motion(cx),
                    route_snap,
                )
            };
        let expanded = crate::components::conversation_composer_expanded(
            &theme,
            self.expanded_mode,
            self.surface_bounds.get().map(|bounds| f32::from(bounds.size.width)),
        );

        // Chat-scoped failures render only under their own chat; a global
        // failure (no key) renders everywhere.
        let failure = self.failure.clone().filter(|_| {
            self.failure_key
                .as_ref()
                .is_none_or(|key| *key == self.current_key)
        });
        let retry = self.failed_send.clone().filter(|failed| {
            let allowed = failure.as_ref().is_some_and(|message| {
                let lower = message.to_lowercase();
                !mast_harness::is_authentication_failure(message)
                    && !["capability", "policy", "unsupported", "not supported", "refused", "permission denied"].iter().any(|word| lower.contains(word))
            });
            allowed && !self.sending && failed.key == self.current_key
                && self.input.read(cx).text().trim() == failed.text
                && self.staged().iter().map(|item| &item.id).eq(failed.attachment_ids.iter())
                && self.staged_appshots().iter().map(|item| &item.id).eq(failed.appshot_ids.iter())
                && self.staged_skills() == failed.skills
                && self.staged_comments(cx) == failed.comments
                && self.state.read(cx).code_refs(&self.current_key) == failed.code_refs
        });
        // Composer honesty: when the target's delivery path is degraded, say
        // UP FRONT that a send will queue (a durable local write delivered on
        // reconnect) instead of letting the button imply instant delivery.
        let queue_notice: Option<(SharedString, bool)> = {
            use mast_proto::ConnectivityState as S;
            let state = self.state.read(cx);
            let degraded = match state.selected_chat.as_deref() {
                Some(id) => state.chat_delivery_degraded(id),
                None => {
                    // New-chat canvas: judge by the picked target device.
                    let remote_target = state
                        .effective_device_id()
                        .is_some_and(|id| state.local_device_id.as_deref() != Some(id.as_str()));
                    remote_target
                        && (matches!(state.connectivity.state, S::Offline | S::Reconnecting)
                            || state
                                .effective_device_id()
                                .is_some_and(|id| !state.device_online(&id, chrono::Utc::now())))
                }
            };
            let offline = state.connectivity.state == S::Offline;
            degraded.then(|| {
                let text: SharedString = if offline {
                    "Offline — messages will send when you're back online.".into()
                } else {
                    "Messages will send once the connection recovers.".into()
                };
                (text, offline)
            })
        };
        let context_reset = self.render_context_reset_prompt(&theme, cx);
        // Centered composer column (zeron `mx-auto w-full max-w-3xl`).
        let container = div()
            .w_full()
            .max_w(px(crate::components::conversation_composer_width(&theme, COMPOSER_MAX_WIDTH, Theme::SPACE_LG)))
            .mx_auto()
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_SM))
            .px(px(Theme::SPACE_LG))
            .pb(px(Theme::SPACE_LG))
            .when_some(failure, |el, message| {
                // Amber "Warning" chip for the offline-ish case (engine not
                // connected); everything else is a send/run/worktree failure
                // and gets the persistent, copyable error card (full detail,
                // Copy button) that stays until dismissed.
                let offline = message.as_ref() == "Engine not connected";
                if offline {
                    return el.child(
                        notice_chip(&theme, true, "Warning", message, NoticeChipIcon::Plain)
                            .id("composer-failure")
                            .mx(px(4.0))
                            .mt(px(6.0))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.failure = None;
                                this.failure_key = None;
                                cx.notify();
                            })),
                    );
                }
                let composer = cx.entity();
                el.child(
                    div()
                        .id("composer-failure")
                        .debug_selector(|| "composer-failure".into())
                        .mx(px(4.0))
                        .mt(px(6.0))
                        .child(
                            crate::error_notice::ErrorNotice::from_text(
                                "composer-failure-card",
                                &theme,
                                &message,
                            )
                            .on_dismiss(move |_, cx| {
                                composer.update(cx, |this, cx| {
                                    this.failure = None;
                                    this.failure_key = None;
                                    cx.notify();
                                })
                            }),
                        ),
                )
            })
            .when_some(retry, |el, failed| el.child(
                div().id("composer-retry").debug_selector(|| "composer-retry".into())
                    .cursor_pointer().text_color(theme.accent).child("Retry")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.sending { return; }
                        this.retry_send_id = Some(failed.message_id.clone());
                        this.send(failed.text.clone(), failed.queued, cx);
                    }))
            ))
            .children(context_reset)
            .when_some(queue_notice, |el, (notice, offline)| {
                // Not a warning box (v0.2.12 feedback: the amber Notice read
                // as an error and flashed on every blip — pre-grace). One
                // quiet caption line, amber dot only for hard offline; it
                // clears itself the moment the path heals.
                let dot = if offline {
                    theme.warning
                } else {
                    theme.text_faint
                };
                el.child(crate::motion::fade_in(
                    "composer-queue-notice",
                    div()
                        .id("composer-queue-notice")
                        .mx(px(8.0))
                        .mt(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_faint)
                        .child(div().size(px(5.0)).rounded_full().bg(dot))
                        .child(div().min_w_0().truncate().child(notice)),
                ))
            });

        if wizard_active {
            let wizard = self.render_wizard(window, cx);
            return container.child(motion::fade_quick("composer-wizard", div().child(wizard)));
        }

        // What is waiting to be sent, stacked directly above the box it was
        // typed in — the queue is a property of this composer, not a panel
        // somewhere else.
        let show_queue_latest_shortcut = self.queue_shortcut_revealed
            && self.editing_queued.is_none()
            && !self.pickers.read(cx).is_open()
            && !composer_has_content(
                self.input.read(cx).text(),
                self.staged().len() + self.staged_appshots().len() + self.staged_skills().len(),
                self.staged_comments(cx).len() + self.state.read(cx).code_refs(&self.current_key).iter().filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File).count(),
            );
        let container = container.when_some(
            self.render_queue_panel(show_queue_latest_shortcut, window, cx),
            |el, panel| {
                el.child(motion::fade_quick(
                    "composer-queue",
                    div()
                        .mx(px(QUEUE_SIDE_INSET))
                        // Cancel the column gap, then tuck the tray one pixel
                        // behind the composer painted after it.
                        .mb(px(-(Theme::SPACE_SM + QUEUE_COMPOSER_OVERLAP)))
                        .child(panel),
                ))
            },
        );
        // Escape backs out of a queue-row edit (the row keeps its old text).
        // Bound here rather than in the input: the input's own Escape belongs
        // to the mention/slash popups, which outrank this while they're open.
        let container = container.when(self.editing_queued.is_some(), |el| {
            el.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && this.mention.token.is_none()
                    && this.slash.token.is_none()
                    && this.cancel_queue_edit(cx)
                {
                    cx.stop_propagation();
                }
            }))
        });

        // Route coordination must not force an established thread into the
        // two-row layout. Short drafts keep the original skinny composer.
        let session_expanded = expanded;
        let expanded = expanded || new_chat;
        let dock_amount = self.dock_frame.map_or(0.0, |frame| frame.amount);
        let dock_height = |amount: f32| {
            let hero = composer_total_height(content_height);
            let session = if session_expanded {
                (content_height + TEXTAREA_PAD_V).clamp(TEXTAREA_MIN - 16.0, TEXTAREA_MAX)
                    + ACTIONS_ROW_HEIGHT
                    + PILL_BORDER_V
            } else {
                COMPACT_TOTAL_HEIGHT
            };
            motion::lerp(hero, session, amount)
        };

        // Committed-height morph: the layout below is already the NEW mode's;
        // only the pill's height (and the entrance fade/text glide driven by
        // `morph_t`) animates. Steady state renders exactly the target.
        // Staged attachments add the wrap strip's height to the pill in BOTH
        // modes (attachment-ui.tsx AttachmentStrip sits above the input row).
        let staged_count = self.staged().len();
        // The input width excludes the inline controls in compact mode.
        // Wrap against the pill's content width in both modes, accounting
        // for the outer container padding and the pill's 1px borders.
        let strip_width_hint =
            self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH) - 2.0 * Theme::SPACE_LG - 2.0;
        let appshot_count = self.staged_appshots().len();
        let strip_h = attachment_strip_height(staged_count, strip_width_hint);
        // The attached-skills row shares the comment chip's strip geometry;
        // both fold into one height term so every sizing site below agrees.
        let comment_strip_h = comment_strip_height(self.staged_comments(cx).len())
            + comment_strip_height(self.staged_skills().len())
            + comment_strip_height(self.state.read(cx).code_refs(&self.current_key).iter().filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File).count());
        let base_height = if self.dock_frame.is_some() {
            dock_height(dock_amount)
        } else if expanded {
            composer_total_height(content_height)
        } else {
            COMPACT_TOTAL_HEIGHT
        };
        let target_height =
            base_height + strip_h + appshot_strip_height(appshot_count) + comment_strip_h;
        let coordinated_route_morph = self
            .flip_morph
            .filter(|m| m.spec == motion::NEW_THREAD_TRANSITION && !m.done(now_ms));
        // The route state commits before its shared-element animation begins.
        // Reconstruct the departing chrome at t=0, then progressively trade
        // it for the destination chrome so neither route changes the outer
        // composer geometry in a single frame.
        let new_thread_chrome = self
            .dock_frame
            .map(|frame| frame.selectors())
            .unwrap_or_else(|| {
                coordinated_route_morph.map_or_else(
                    || if new_chat { 1.0 } else { 0.0 },
                    |morph| {
                        let progress = morph.progress(now_ms);
                        if new_chat { progress } else { 1.0 - progress }
                    },
                )
            });
        let (new_thread_chrome_opacity, session_chrome_opacity) = self.dock_frame.map_or_else(
            || route_chrome_opacities(new_thread_chrome),
            |frame| (frame.selectors(), frame.footer()),
        );
        self.height_morph =
            if dock_height_changed || self.dock_frame.is_some_and(|frame| frame.active) {
                None
            } else if coordinated_route_morph.is_some() {
                coordinated_route_morph
            } else {
                flip_morph_step(
                    self.height_morph,
                    (target_height - self.last_target_height).abs() > 0.5,
                    self.last_rendered_height,
                    now_ms,
                    motion::reduced_motion(cx),
                    route_snap,
                )
            };
        self.last_target_height = target_height;
        let pill_height = self
            .height_morph
            .map_or(target_height, |m| m.height(target_height, now_ms));
        if self.height_morph.is_some() {
            window.request_animation_frame();
        }
        let (_, morph_t, morphing) = match self.flip_morph {
            Some(m) if !m.done(now_ms) => {
                (m.height(target_height, now_ms), m.progress(now_ms), true)
            }
            _ => (target_height, 1.0, false),
        };
        if !morphing {
            self.flip_morph = None;
        } else {
            // Manual tween drive: keep frames coming (shell.rs motion_active).
            window.request_animation_frame();
        }
        self.last_rendered_height = pill_height;
        self.dock_clearance_correction = self.dock_frame.map_or(0.0, |frame| {
            dock_height(if frame.docked { 1.0 } else { 0.0 })
                + strip_h
                + appshot_strip_height(appshot_count)
                + comment_strip_h
                - pill_height
        });
        // Route morphs use the dock's reversible clock; typing flips keep
        // their existing local clock once the composer reaches its dock.
        let layout_morph_t =
            if self.dock_frame.is_some_and(|frame| frame.active) && !session_expanded {
                if expanded {
                    1.0 - dock_amount
                } else {
                    dock_amount
                }
            } else {
                morph_t
            };
        let text_pt = morph_text_pad(layout_morph_t);
        let surface_radius = crate::components::conversation_panel_radius(&theme, COMPOSER_RADIUS - 4.0 * dock_amount);
        let route_to_single_line =
            self.dock_frame.is_some_and(|frame| frame.active) && !session_expanded;
        let textarea_height = (pill_height
            - strip_h
            - appshot_strip_height(appshot_count)
            - comment_strip_h
            - PILL_BORDER_V
            - ACTIONS_ROW_HEIGHT)
            .max(if route_to_single_line {
                INPUT_LINE_HEIGHT + text_pt + 4.0
            } else {
                0.0
            });
        self.input.update(cx, |input, cx| {
            let height = if expanded {
                (textarea_height - text_pt - 4.0).max(0.0)
            } else {
                INPUT_LINE_HEIGHT
            };
            let settled_height = if expanded {
                (base_height - PILL_BORDER_V - ACTIONS_ROW_HEIGHT - TEXTAREA_PAD_V).max(
                    if route_to_single_line {
                        INPUT_LINE_HEIGHT
                    } else {
                        0.0
                    },
                )
            } else {
                INPUT_LINE_HEIGHT
            };
            let resizing = self.height_morph.is_some();
            let top_padding = if expanded { text_pt } else { 0.0 };
            if input.viewport_height != Some(height)
                || input.settled_viewport_height != Some(settled_height)
                || input.resizing != resizing
                || input.overflow_top_padding != top_padding
            {
                input.resizing = resizing;
                input.overflow_top_padding = top_padding;
                input.viewport_height = Some(height);
                input.settled_viewport_height = Some(settled_height);
                cx.notify();
            }
        });

        let send_button = self.render_send_button(mode, identity, cx);
        // Attach button — opens the native image picker (the original's hidden
        // `<input type=file accept="image/*" multiple>`); paste/drop also feed
        // the same strip. The leading utility group owns the spacing between
        // this button and the model picker.
        let attach = crate::components::conversation_attach_key(&theme)
            .on_click(cx.listener(|this, _, _, cx| this.open_file_picker(cx)));
        // Staged-thumbnail strip (attachment-ui.tsx AttachmentStrip), above
        // the input inside the pill in both modes.
        let strip = self.render_attachment_strip(&theme, cx);
        let appshot_strip = self.render_appshot_strip(&theme, window, cx);
        let comments_chip = self.render_comments_chip(&theme, cx);
        let code_refs = self.render_code_refs(&theme, window, cx);
        let skill_chips = self.render_skill_chips(&theme, cx);

        // A translucent cool silver/slate edge sits more naturally on frost
        // than the general-purpose white/black separator color.
        let pill_border = if theme.is_frost() {
            match theme.appearance {
                crate::theme::Appearance::Dark => gpui::hsla(210.0 / 360.0, 0.18, 0.78, 0.09),
                crate::theme::Appearance::Light => gpui::hsla(210.0 / 360.0, 0.18, 0.32, 0.10),
            }
        } else {
            theme.border
        };
        // Compensate for the transcript canvas beneath the frosted surface.
        // Keep the opaque fallback when frost is disabled or unsupported.
        let pill = crate::components::conversation_input_surface(div()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // Padding and action controls are part of the text composer.
                    // Open menus keep their own keyboard/search focus.
                    if !this.pickers.read(cx).is_open() {
                        window.focus(&this.input.focus_handle(cx), cx);
                    }
                }),
            ), &theme, identity, surface_radius, pill_border);
        // The pill's bottom edge is stationary on screen (the composer sits at
        // the bottom of the shell column; growth moves the TOP edge), so the
        // controls pin to the bottom and only the text glides with the reveal
        // (round-9 follow-up: the send/attach/chips must not ride the height,
        // while the model picker fades between its two horizontal anchors).
        let cluster_dy = morph_cluster_dy(layout_morph_t);
        // Share the height/route timeline instead of starting an independent
        // animation. Reversals continue from the current handoff phase.
        if self.model_handoff_morph != self.flip_morph {
            self.model_handoff_from = self.model_handoff_position;
            self.model_handoff_morph = self.flip_morph;
        }
        let compact_target = if expanded { 0.0 } else { 1.0 };
        self.model_handoff_position =
            if self.dock_frame.is_some_and(|frame| frame.active) && !session_expanded {
                dock_amount
            } else {
                self.flip_morph.map_or(compact_target, |morph| {
                    motion::lerp(
                        self.model_handoff_from,
                        compact_target,
                        motion::EASE_IN_OUT.eval(morph.raw(now_ms)),
                    )
                })
            };
        let surface_width = self
            .surface_bounds
            .get()
            .map_or(strip_width_hint + PILL_BORDER_V, |bounds| {
                f32::from(bounds.size.width)
            });
        let send_hint = crate::components::conversation_send_hint(&theme, mode != SendButtonMode::Stop, surface_width,
            match crate::settings::current(cx).composer_send_behavior {
                crate::settings::ComposerSendBehavior::Enter => "enter",
                crate::settings::ComposerSendBehavior::ModEnter => "mod-enter",
            });
        let model_travel = (surface_width
            - PILL_BORDER_V
            - 12.0
            - 28.0
            - ACTION_UTILITY_GAP
            - self
                .model_bounds
                .get()
                .map_or(0.0, |bounds| f32::from(bounds.size.width))
            - ACTION_PRIMARY_GAP
            - 28.0
            - morph_cluster_inset(expanded, layout_morph_t))
        .max(0.0);
        let (model_side, model_opacity, model_drift) = model_handoff(self.model_handoff_position);
        let model_offset = (model_side - compact_target) * model_travel + model_drift;
        let measured_model_bounds = self.model_bounds.clone();
        let (model_picker, horizon_picker) = crate::components::conversation_model_picker(
            &theme, surface_width, model_offset, model_opacity, self.pickers.clone().into_any_element(),
                gpui::canvas(
                    move |bounds, _, _| measured_model_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0().into_any_element(),
        );
        let horizon_tabs = {
            let selected = self.pickers.read(cx).effective_harness(cx);
            let harnesses = self.pickers.read(cx).horizon_harnesses(cx);
            let tabs = harnesses.into_iter().map(|harness| {
                let (path, _) = crate::pickers::harness_brand_icon(harness);
                let selected = selected == Some(harness);
                let tab_identity = theme.agent_identity_color(Some(harness), Some("selected"));
                crate::components::conversation_harness_tab(&theme, SharedString::from(format!("horizon-harness-{harness:?}")), tab_identity, identity, path, selected)
                    .on_click(cx.listener(move |this, _, window, cx| this.pickers.update(cx, |pickers, cx| {
                        pickers.pick_harness(harness, cx);
                        pickers.open_model_menu(window, cx);
                    }))).into_any_element()
            }).collect::<Vec<_>>();
            let add = crate::components::conversation_add_harness_key(&theme)
                    .on_click(cx.listener(|this, _, window, cx| this.pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx))))
                    .into_any_element();
            crate::components::conversation_harness_tabs(&theme, tabs, add)
        };
        let text_glide = if self.dock_frame.is_some_and(|frame| frame.active) {
            collapse_text_glide(dock_height(0.0), dock_amount)
        } else {
            match self.flip_morph {
                Some(m) if morphing => collapse_text_glide(m.from, morph_t),
                _ => 0.0,
            }
        };
        let strips = code_refs.into_iter().map(|el| el.into_any_element())
            .chain(comments_chip.into_iter().map(|el| el.into_any_element()))
            .chain(skill_chips.into_iter().map(|el| el.into_any_element()))
            .chain(appshot_strip)
            .chain(strip.into_iter().map(|el| el.into_any_element()))
            .collect();
        let body = crate::components::conversation_composer_body(&theme, crate::components::ConversationComposerBody {
            pill, input: self.render_input_with_completion().into_any_element(), strips,
            attach: attach.into_any_element(), send: send_button,
            model: model_picker.map(|el| el.into_any_element()),
            horizon_model: horizon_picker.map(|el| el.into_any_element()),
            send_hint: send_hint.map(|el| el.into_any_element()),
            expanded, pill_height, textarea_height, text_padding: text_pt,
            cluster_offset: cluster_dy,
            expanded_inset: morph_cluster_inset(true, layout_morph_t),
            compact_inset: morph_cluster_inset(false, layout_morph_t),
            text_glide, surface_width,
        });
        let new_thread_target_selectors = (new_thread_chrome_opacity > 0.0).then(|| {
            self.pickers.update(cx, |pickers, cx| {
                pickers.render_new_thread_target_selectors(cx)
            })
        });
        let new_thread_git_selectors = (new_thread_chrome_opacity > 0.0)
            .then(|| {
                self.pickers.update(cx, |pickers, cx| {
                    pickers.render_new_thread_git_selectors(cx)
                })
            })
            .flatten();
        let has_new_thread_git_selectors = self
            .state
            .read(cx)
            .selected_space_row()
            .is_some_and(|space| space.git_detected);
        // The file dropzone lives in the shell (the whole conversation column,
        // not just the pill — shell.rs `chat-dropzone`); drops land back here
        // via `add_paths`.
        // Frosted: the pill backdrop-blurs the transcript scrolling under it
        // (the popover glass treatment; radius matches the pill's rounding).
        // The shell keeps this entity under one parent on both routes. The
        // surface itself never fades, and frost follows the same morph radius.
        let pill_surface = div()
            .relative()
            .id("composer-surface")
            .debug_selector(|| "composer-surface".into())
            .child(crate::frost::frosted(surface_radius, 16.0, body))
            .children(horizon_tabs)
            .child({
                let measured = self.surface_bounds.clone();
                // All prepaint completes before any paint. The background
                // reads this cell during paint, never last frame's geometry.
                gpui::canvas(
                    move |bounds, _, _| measured.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
            })
            // Both completion popups span the full pill width above it —
            // the file-mention and slash tokens are mutually exclusive.
            .children(self.render_file_mention_popup(&theme, cx))
            .children(self.render_slash_popup(&theme, cx));
        // Restore the original chip-only selector treatment: destination at
        // the top-right, no surrounding surface. Cancel the column gap as the
        // row collapses so the pill never jumps at the route boundary.
        let container = if self.dock_frame.is_some() {
            // Floating selectors share the surface's origin and never change its height.
            container.relative().child(
                div()
                    .id("dock-target-selectors")
                    .absolute()
                    .top(px(-28.0))
                    .left(px(Theme::SPACE_LG + 10.0))
                    .right(px(Theme::SPACE_LG + 10.0))
                    .h(px(NEW_THREAD_SELECTOR_ROW_HEIGHT))
                    .flex()
                    .items_start()
                    .justify_end()
                    .opacity(new_thread_chrome_opacity)
                    .children(new_thread_target_selectors),
            )
        } else if new_thread_chrome > 0.0 {
            container.child(
                div()
                    .w_full()
                    .h(px(NEW_THREAD_SELECTOR_ROW_HEIGHT * new_thread_chrome))
                    .mb(px(-Theme::SPACE_SM * (1.0 - new_thread_chrome)))
                    .px(px(10.0))
                    .flex()
                    .items_start()
                    .justify_end()
                    .opacity(new_thread_chrome_opacity)
                    .children(new_thread_target_selectors),
            )
        } else {
            container
        };
        let container = container.when_some(sign_in_notice, |container, (device, harness, account, snapshot)| {
            let name = if harness == mast_proto::HarnessId::ClaudeCode { "Claude Code" } else { "Codex" };
            container.child(div().debug_selector(|| "composer-sign-in-notice".into()).flex().items_center().gap(px(8.0))
                .text_color(theme.warning).child(SharedString::from(format!("{name} isn't signed in")))
                .child(crate::popover::btn_ghost(&theme, "Sign in…", "composer-sign-in")
                    .id("composer-sign-in").debug_selector(|| "composer-sign-in".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sign_in_check = None;
                        cx.emit(ComposerEvent::SignIn { device: device.clone(), harness, account: account.clone(), snapshot: snapshot.clone() });
                    })))
                .child(crate::popover::btn_ghost(&theme, "Start anyway", "composer-start-anyway")
                    .id("composer-start-anyway").debug_selector(|| "composer-start-anyway".into())
                    .on_click(cx.listener(|this, _, _, cx| this.on_submit(cx)))))
        }).child(pill_surface);

        // The lower slot keeps a stable footprint for Git projects while its
        // old floating checkout/ref controls dissolve into the session footer.
        // Non-Git sessions grow the slot continuously from zero.
        let session_chrome = 1.0 - new_thread_chrome;
        let bottom_slot = if has_new_thread_git_selectors || self.dock_frame.is_some() {
            1.0
        } else {
            session_chrome
        };
        let container = if bottom_slot > 0.0 {
            let footer = (session_chrome_opacity > 0.0).then(|| {
                self.pickers
                    .update(cx, |pickers, cx| pickers.render_footer(cx))
            });
            let usage = self.state.read(cx).context_usage;
            let usage = (session_chrome_opacity > 0.0 && crate::context_usage::has_window(usage)).then(||
                crate::context_usage::render(usage, self.state.clone(), &theme).into_any_element());
            container.child(crate::components::conversation_footer_slot(
                SESSION_FOOTER_HEIGHT, bottom_slot, new_thread_chrome_opacity, session_chrome_opacity,
                new_thread_git_selectors.map(|el| el.into_any_element()),
                footer.flatten().map(|el| el.into_any_element()), usage,
            ))
        } else {
            container
        };
        // Full-size preview of a staged thumbnail (AttachmentPreviewDialog).
        if let Some(preview) = self.preview.clone() {
            if std::mem::take(&mut self.preview_focus_pending) {
                window.focus(&self.preview_focus, cx);
            }
            let weak = cx.weak_entity();
            return container.child(attachments::lightbox(
                window,
                &preview,
                &self.preview_focus,
                move |window, cx| {
                    // Hand focus back to the input so typing (and the next
                    // Escape) lands where it did before the lightbox opened.
                    if let Ok(input_focus) = weak.update(cx, |this, cx| {
                        this.preview = None;
                        cx.notify();
                        this.input.read(cx).focus_handle.clone()
                    }) {
                        window.focus(&input_focus, cx);
                    }
                },
                cx,
            ));
        }
        container
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod appshot_rebase_tests;

#[cfg(feature = "appshots-fixture")]
impl Composer {
    pub fn fixture_clear_appshots(&mut self, cx: &mut Context<Self>) {
        self.appshots.clear();
        cx.notify();
    }
}
