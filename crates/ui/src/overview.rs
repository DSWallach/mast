//! Multi-chat overview (ticket 002 phase 1):
//! every current chat, at once, in one view — the thing
//! `session_canvas.html`/`session_canvas_server.py` (the `agent-mode-tools`
//! stopgap tool) already does, without leaving for the single-chat route.
//! "View full chat →" opens a side panel that embeds the Shell's OWN
//! `Transcript`/`Composer` entities (live transcript + working composer) by
//! selecting the chat without changing the route — see
//! [`Overview::chat_panel_open`]. The same right-side slot has a second,
//! read-only mode: clicking one of a chat's compact subagent rows "peeks"
//! into that subagent's transcript (`READ_SUBAGENT_TRANSCRIPT`; no composer
//! — a subagent can't be continued) — see [`Overview::subagent_peek`].
//!
//! Two rendering modes over the SAME `OverviewRow` data:
//! - List: the plain row list, optionally grouped.
//! - Outline: the Work Outline tree of work items and their Chats.
//!
//! The acknowledged-"done" set, the repo filter's hidden set and the work-item
//! filter persist per device
//! (`overview_positions`, JSON under the engine data dir; the view mode, the
//! toggles and the grouping persist in `UI_FLAGS_FILE`). Ticket 002 puts
//! cross-device layout sync out of scope for v1.
//!
//! Freshness: the TTL-gated `ensure_*` fetches only get a chance to re-run
//! when something calls them, so a mounted, visible overview runs a light
//! [`IDLE_REFRESH`] tick (`refresh_tick`) that re-checks every visible row's
//! TTLs and repaints relative times without forcing a row rebuild unless a fetch
//! actually changed something.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{
    Context, Entity, ListAlignment, ListState, MouseButton, ScrollHandle, SharedString,
    Subscription, Window, div, list, prelude::*, px,
};

use mast_engine::pr_ticket_cache::{ChecksStatus, PrAction, PrStatus, TicketStatus};
use mast_proto::{Chat, ChatIndicator, ChatLinkSource, Space};
use mast_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::overview_grouping::{self, GroupDimension, GroupKeyed, Partition};
use crate::overview_positions;
use crate::popover;
use crate::shell::Shell;
use crate::state::{AppState, format_time_ago};
use crate::theme::Theme;
use crate::transcript::Transcript;
use crate::work_outline::chips::TextTooltip;
use crate::work_outline::details_panel::{DetailsEvent, WorkItemDetails};
use crate::work_outline::filter::{self as work_item_filter, WorkItemIndex, WorkItemPathLabel};
use crate::work_outline::hints::{HintKey, WithHint};
use crate::work_outline::model::{OutlineChat, OutlineOptions, OutlineSpace, row_key};
use crate::work_outline::outline_view::{OutlineEvent, OutlineInputs, OutlineView};
use crate::work_outline::store::WorkOutlineSnapshot;
use crate::work_outline::{WorkOutlineCommand, WorkOutlineLoad};

mod chat_link_status;
mod chat_panel;
mod details_width;
mod github_host;
mod layout;
mod sidebar;
pub(crate) use sidebar::OverviewNavigation;
mod top_sessions;
mod initialization;
mod interaction;
mod linear_host;
mod list_details;
/// The Linear panel host (`overview/linear_host.rs`).
mod list_rendering;
mod outline_integration;
mod row_derivation;
mod subagent_peek;
mod toolbar_menus;
mod view_preferences;

/// A chat with no activity for longer than this, and not currently
/// working/awaiting input, reads as stale. Adapted from
/// `session_canvas_server.py`'s `CURSOR_STALE_SECONDS` (3600s) — that value
/// times a continuously-polled cursor-liveness hook, a much noisier signal
/// than Mast's per-message timestamps, so a day is the closer match in
/// spirit ("this chat has gone quiet") rather than a literal port of the
/// number.
const STALE_AFTER: chrono::Duration = chrono::Duration::days(7);

/// How long a fetched `ChatLinkStatus` stays good before this view refetches
/// it — independent of the server's own cache TTL, just how often the UI
/// bothers to poll for a fresher answer.
const LINK_STATUS_REFRESH: Duration = Duration::from_secs(45);
/// How long a host's scheduled-task list (schedule, next run, outcome) stays fresh.
const SCHEDULED_TASKS_REFRESH: Duration = Duration::from_secs(60);

/// Subagent scans are settled-ish once a transcript exists, but an imported
/// chat that keeps running elsewhere (or gets resumed here) grows new
/// subagents — a generous TTL instead of fetch-once-forever.
const SUBAGENTS_REFRESH: Duration = Duration::from_secs(120);
/// Subagent refetch TTL while any of a chat's subagents is still `running`
/// — a running subagent flips to `done` on its own, and the row's status
/// dot should follow within an idle tick or so, not two minutes later. Just
/// under [`IDLE_REFRESH`], same reasoning as [`CONTEXT_USAGE_REFRESH`].
const SUBAGENTS_RUNNING_REFRESH: Duration = Duration::from_secs(12);

/// One subagent, as `SCAN_CHAT_SUBAGENTS` reports it — the UI's own decode
/// of the wire item (a subset of `mast_engine::subagent_scan::SubagentSummary`
/// plus the `status` field), so this view depends on the RPC contract rather
/// than the engine struct's exact shape. Unknown extra keys
/// (`transcriptPath`) are ignored.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentItem {
    agent_id: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    status: SubagentStatus,
}

impl SubagentItem {
    /// The web's `sub.description || sub.agent_type || sub.id`.
    fn display_name(&self) -> String {
        self.description
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.agent_type.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| self.agent_id.clone())
    }
}

/// `SCAN_CHAT_SUBAGENTS`'s `status`. Missing (an engine that predates the
/// field) or unrecognized reads as `Done` — the settled, non-alarming
/// default; a subagent is never shown "running" without the engine saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum SubagentStatus {
    Running,
    #[default]
    #[serde(other)]
    Done,
}

impl SubagentStatus {
    /// Reuses the reference status palette: running = `--accent` (the
    /// working color), done = `--status-good`.
    fn color(self, appearance: crate::theme::Appearance) -> gpui::Hsla {
        let status = match self {
            SubagentStatus::Running => crate::status_style::Status::Working,
            SubagentStatus::Done => crate::status_style::Status::Done,
        };
        crate::status_style::status_style(status, appearance).color
    }

    fn label(self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Done => "done",
        }
    }
}

/// `READ_SUBAGENT_TRANSCRIPT` reply: `{turns, model}`.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
struct SubagentTranscript {
    #[serde(default)]
    turns: Vec<TranscriptTurn>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct TranscriptTurn {
    /// `"user" | "assistant"`; anything else renders under its raw name.
    role: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    tools: Vec<TranscriptTool>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptTool {
    name: String,
    #[serde(default)]
    input_preview: String,
    #[serde(default)]
    result_preview: Option<String>,
}

/// The web's `turn-role` label: "You" / "Assistant".
fn turn_role_label(role: &str) -> String {
    match role {
        "user" => "You".to_string(),
        "assistant" => "Assistant".to_string(),
        other => other.to_string(),
    }
}

/// Split message text on ``` fences into `(is_code, chunk)` segments — the
/// web `renderMarkdownLite`'s code-block treatment, minus everything else
/// (plain text keeps its newlines as-is). A fence's language tag line is
/// dropped; an unterminated fence runs to the end. Empty prose chunks
/// (e.g. between back-to-back fences) are skipped.
fn split_fenced_code(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(open) = rest.find("```") else {
            let prose = rest.trim_matches('\n');
            if !prose.trim().is_empty() {
                out.push((false, prose.to_string()));
            }
            return out;
        };
        let prose = rest[..open].trim_matches('\n');
        if !prose.trim().is_empty() {
            out.push((false, prose.to_string()));
        }
        let after = &rest[open + 3..];
        // Drop the language tag (everything up to the first newline).
        let body_start = after.find('\n').map_or(after.len(), |nl| nl + 1);
        let body = &after[body_start..];
        let (code, next) = match body.find("```") {
            Some(close) => (&body[..close], &body[close + 3..]),
            None => (body, ""),
        };
        out.push((true, code.trim_end_matches('\n').to_string()));
        rest = next;
    }
}

/// Web `formatTurnTime`: a short local "Mon D, HH:MM", empty when absent or
/// unparseable.
fn format_turn_time(timestamp: Option<&str>) -> String {
    timestamp
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Where a subagent-transcript peek's load stands.
#[derive(Debug, Clone, PartialEq)]
enum PeekLoad {
    Loading,
    Loaded(SubagentTranscript),
    Failed,
}

/// The read-only subagent-transcript mode of the right-side panel (web
/// `openTranscriptPanel(sessionId, subagentId, ...)`): distinct from the
/// live chat panel (`Overview::chat_panel_open`) — a subagent can't be
/// continued, so there's no composer, only its turns.
#[derive(Debug, Clone)]
struct SubagentPeek {
    chat_id: String,
    agent_id: String,
    name: String,
    agent_type: Option<String>,
    /// The `Overview::peek_request_seq` value this peek's fetch was issued
    /// under — web `transcriptRequestId`. See [`peek_reply_is_current`].
    request_id: u64,
    load: PeekLoad,
    /// `(turn index, tool index)` pairs whose result preview is disclosed
    /// (web `<details class="tool-detail">`).
    open_tools: HashSet<(usize, usize)>,
}

/// Stale-response guard (web `if (requestId !== transcriptRequestId)
/// return`): a landed `READ_SUBAGENT_TRANSCRIPT` reply only applies if the
/// panel is still showing the peek that issued it — not closed since, and
/// not superseded by a quicker click on another subagent (or the same one
/// again, which issues a fresh id).
fn peek_reply_is_current(peek: Option<&SubagentPeek>, request_id: u64) -> bool {
    peek.is_some_and(|p| p.request_id == request_id)
}

/// Context-window % moves constantly for a live chat, so it gets the
/// shortest TTL of any per-row fetch. Set just under [`IDLE_REFRESH`] so
/// every idle tick refetches it; a TTL of exactly 15s would lose the race
/// against the tick's own timer jitter and only refresh every other tick.
const CONTEXT_USAGE_REFRESH: Duration = Duration::from_secs(12);

/// Idle-refresh cadence while the overview is the visible route — see
/// `refresh_tick`. Only re-checks TTLs (each fetch keeps its own, longer
/// TTL) and repaints; it never forces a row rebuild by itself.
const IDLE_REFRESH: Duration = Duration::from_secs(15);

/// How long the Outline selection must rest on a Chat row before the chat
/// panel switches to it, so holding an arrow key does not reload a
/// transcript per row.
pub(crate) const OUTLINE_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(100);

/// Every Nth idle tick also re-derives rows (staleness is time-based, so a
/// chat can cross `STALE_AFTER` while nothing else changes). ~60s — cheap
/// relative to the per-frame rebuild `rows_dirty` exists to avoid.
const STALENESS_REDERIVE_EVERY_TICKS: u32 = 4;

/// Work counters for the perf regression test: what the Overview did in
/// response to `AppState` changes. Plain increments, always on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverviewPerf {
    /// `rows()` calls that rebuilt the row list.
    rows_rebuilds: u64,
    /// `AppState` observer callbacks delivered to the Overview.
    observer_calls: u64,
    /// Of those, the ones that went on to `cx.notify()`.
    observer_notifies: u64,
    /// Per-chat `ensure_row_fetches` evaluations.
    fetch_checks: u64,
    /// Engine requests actually spawned by the per-row fetches.
    fetch_requests: u64,
}

/// One row's worth of what this pass can show.
#[derive(Clone)]
struct OverviewRow {
    status: ChatIndicator,
    chat: Chat,
    stale: bool,
    /// Linked ticket identifier, if `link_status` has it cached by the time
    /// `rows()` builds this row — copied in at construction (rather than
    /// read from `Overview.link_status` inside `group_key`) because
    /// `GroupKeyed::group_key` only has `&self` on the row itself, matching
    /// how `overview_grouping` is deliberately UI-crate-agnostic.
    ticket: Option<String>,
    /// [`repo_key_for_chat`] — the Repo grouping key and the repo filter's
    /// key, computed once so both agree by construction.
    repo: String,
    /// Linked PR number/title, copied in like `ticket`: the number is the
    /// Ticket dimension's `PR #<n>` fallback (`ticketGroupKey`), the title is
    /// a search field (`sessionMatchesSearch` includes `pr.title`).
    pr_number: Option<u64>,
    pr_title: Option<String>,
    /// Search-only values for every linked PR (number and title). The two
    /// singular fields above remain the stable grouping fallback.
    pr_search_values: Vec<String>,
    /// `CHAT_CONTEXT_USAGE` fraction (`0.0..=1.0`), copied in like `ticket`
    /// so the cached rows carry it. `None` until the fetch lands, or when the
    /// engine has no usage for this chat.
    context_pct: Option<f32>,
    /// How many compact subagent rows exist for this chat — copied in from
    /// `Overview.subagents` like `ticket`. `0` while the fetch is pending or
    /// the chat genuinely has none — both render no rows, so they're
    /// indistinguishable here on purpose.
    subagent_count: usize,
    /// `chat.cwd` cheaply looks like a linked worktree — the expanded detail
    /// offers "Close out worktree…" and the engine plan decides actual
    /// eligibility. Computed in the cached `rows()` pass so the fs check never
    /// runs per frame.
    closeable: bool,
    /// The associated work item's breadcrumb (path chip), copied in from the
    /// Work Outline index in `rows()` like `ticket`. The chip shares an
    /// existing line of the row.
    work_item_path: Option<WorkItemPathLabel>,
    /// The associated work item is in User Action Required.
    user_action_required: bool,
    /// Its unresolved user-action request count (0 until Milestone 4).
    user_action_count: u32,
    /// The Chat's effective instructions changed since its applied revision
    /// (Work Outline drift). The mark shares the title line.
    instruction_drift: bool,
    /// The Chat captured an execution configuration that differs from its
    /// item's current one (Work Outline exception). Shares the title line.
    execution_exception: bool,
    /// Mast agent-tool calls that failed in the session's last completed turn.
    tool_failures: Option<mast_proto::ToolFailures>,
}

impl OverviewRow {
    fn needs_interaction(&self) -> bool {
        self.status == ChatIndicator::AwaitingInput || self.user_action_required
    }

    fn is_stalled(&self) -> bool {
        // Tool failures can be handled within a successful turn and do not
        // establish that the session itself has stopped on an error.
        self.status == ChatIndicator::Errored
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AttentionFilter {
    #[default]
    All,
    NeedsInteraction,
    Stalled,
}

impl GroupKeyed for OverviewRow {
    /// Repo/Ticket are proxies (cwd basename, linked-ticket identifier).
    fn group_key(&self, dim: GroupDimension) -> String {
        match dim {
            GroupDimension::Repo => self.repo.clone(),
            GroupDimension::Ticket => ticket_group_key(self.ticket.as_deref(), self.pr_number),
        }
    }
}

impl GroupKeyed for &OverviewRow {
    fn group_key(&self, dim: GroupDimension) -> String {
        (*self).group_key(dim)
    }
}

/// Port of `repoGroupKey` (`session_canvas.html:968-972`): the cwd's last
/// path segment after stripping trailing slashes, `NONE_KEY` for no cwd (or
/// a cwd that is nothing but slashes).
///
/// Deliberate divergence from the reference: agent worktrees carry a
/// `.workspace-root` file at their root whose first line is the absolute path
/// of the superproject checkout. When `<cwd>/.workspace-root` exists and that
/// trimmed first line is non-empty, the key is the last segment of THAT path
/// instead, so worktree chats group under their real repo rather than each
/// worktree name becoming its own one-chat "repo". A missing, unreadable, or
/// empty file falls back to the reference behavior above.
///
/// Does a small fs read per call; only invoked from the cached `rows()`
/// recompute (not per frame), so it is not memoized.
fn repo_key(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return overview_grouping::NONE_KEY.to_string();
    };
    if let Some(root) = workspace_root_of(cwd)
        && let Some(key) = last_segment(&root)
    {
        return key;
    }
    last_segment(cwd).unwrap_or_else(|| overview_grouping::NONE_KEY.to_string())
}

/// Resolve a chat's repo label without mistaking an isolated checkout's
/// folder name for the owning project. The persisted Space is authoritative
/// when the chat runs somewhere other than the Space root (normally a linked
/// worktree). Same-root and legacy chats retain the cwd-based behavior above.
///
/// This is deliberately a lexical comparison: grouping must not canonicalize
/// arbitrary chat paths merely to render the overview.
fn repo_key_for_chat(chat: &Chat, space: Option<&Space>) -> String {
    if let (Some(cwd), Some(space)) = (chat.cwd.as_deref(), space)
        && lexical_path(cwd) != lexical_path(&space.path)
    {
        return space.display_name().to_string();
    }
    repo_key(chat.cwd.as_deref())
}

fn lexical_path(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

fn last_segment(path: &str) -> Option<String> {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(last) if !last.is_empty() => Some(last.to_string()),
        _ => None,
    }
}

/// First line (trimmed) of `<cwd>/.workspace-root`, if present and non-empty.
fn workspace_root_of(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if mast_engine::repos::is_automatic_access_blocked(std::path::Path::new(cwd)) {
        return None;
    }
    let contents =
        std::fs::read_to_string(std::path::Path::new(cwd).join(".workspace-root")).ok()?;
    let line = contents.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Port of `ticketGroupKey` (`session_canvas.html:955-957`): ticket id, else
/// `PR #<n>` when a PR is linked, else `NONE_KEY`.
fn ticket_group_key(ticket: Option<&str>, pr_number: Option<u64>) -> String {
    match (ticket, pr_number) {
        (Some(t), _) => t.to_string(),
        (None, Some(n)) => format!("PR #{n}"),
        (None, None) => overview_grouping::NONE_KEY.to_string(),
    }
}

/// Display label for a raw group key (`PartitionGroup::key`) on `dim` —
/// the reference's per-dimension `getMeta().label`: "No repo"/"No ticket /
/// PR" for the `NONE_KEY` bucket (`repoGroupMeta`/`ticketGroupMeta`), the raw
/// key (a folder name, a ticket id) otherwise.
fn group_label(dim: GroupDimension, key: &str) -> String {
    match dim {
        GroupDimension::Repo if key == overview_grouping::NONE_KEY => "No repo".into(),
        GroupDimension::Ticket if key == overview_grouping::NONE_KEY => "No ticket / PR".into(),
        GroupDimension::Repo | GroupDimension::Ticket => key.to_string(),
    }
}

/// `session_canvas.html`'s `HASH_COLOR_PALETTE` (`~888`) — Repo/Ticket group
/// colors have no fixed lookup table, so the same key always lands on the
/// same hue via a deterministic hash instead.
const HASH_COLOR_PALETTE: [u32; 8] = [
    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767,
];

/// Exact port of `session_canvas.html`'s `hashColor` (`~889-893`):
/// `hash = (hash * 31 + charCode) >>> 0` per char, then `PALETTE[hash %
/// PALETTE.length]`. `wrapping_mul`/`wrapping_add` on `u32` reproduces JS's
/// `>>> 0` unsigned-32-bit truncation exactly — this must NOT be `i32` math,
/// which would diverge on overflow.
fn hash_color(key: &str) -> gpui::Hsla {
    let mut hash: u32 = 0;
    for ch in key.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(ch as u32);
    }
    let idx = (hash as usize) % HASH_COLOR_PALETTE.len();
    gpui::rgb(HASH_COLOR_PALETTE[idx]).into()
}

/// Shared chat status glyph and descriptive label.
fn status_style(status: ChatIndicator) -> (&'static str, &'static str) {
    let glyph =
        crate::status_style::status_style(status.into(), crate::theme::Appearance::Dark).glyph;
    let label = match status {
        ChatIndicator::Working => "working",
        ChatIndicator::AwaitingInput => "needs input",
        ChatIndicator::Errored => "error",
        ChatIndicator::Completed => "done",
        ChatIndicator::Idle => "idle",
    };
    (glyph, label)
}

/// `CHECKS_META` (`session_canvas.html:912-916`): color + icon per checks
/// rollup.
fn checks_style(checks: ChecksStatus) -> (gpui::Hsla, &'static str, &'static str) {
    match checks {
        ChecksStatus::Passing => (gpui::rgb(0x0ca30c).into(), "✓", "passing"),
        ChecksStatus::Pending => (gpui::rgb(0xfab219).into(), "…", "pending"),
        ChecksStatus::Failing => (gpui::rgb(0xd03b3b).into(), "✕", "failing"),
    }
}

/// Web `isUnreadDone` reconciliation (`session_canvas.html:1289-1298`): a
/// chat that is no longer `done` drops out of the acknowledged set so its
/// NEXT completion starts unread again. Returns whether the set changed (so
/// the caller persists it).
fn reconcile_acknowledged(
    acknowledged: &mut BTreeSet<String>,
    chat_id: &str,
    is_done: bool,
) -> bool {
    !is_done && acknowledged.remove(chat_id)
}

/// Whether a row should carry the unread-done ribbon.
fn is_unread_done(acknowledged: &BTreeSet<String>, chat_id: &str, is_done: bool) -> bool {
    is_done && !acknowledged.contains(chat_id)
}

/// `sessionMatchesSearch` (`session_canvas.html:1464-1470`): case-insensitive
/// substring over whichever fields the row carries. `query_lower` must
/// already be trimmed + lowercased; empty matches everything.
fn matches_search(query_lower: &str, fields: &[Option<&str>]) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    fields
        .iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(query_lower))
}

/// Legacy UI-flag entry from when a Canvas view existed. Read-only: a saved
/// `viewMode:canvas` loads as Outline ([`OverviewMode::from_flags`]) and is
/// never written back (the writer purges every `viewMode:` flag).
#[cfg(test)]
const LEGACY_VIEW_CANVAS_FLAG: &str = "viewMode:canvas";
/// UI-flag entry: the Work Outline is the main column's mode. Older builds
/// read it as an unknown flag, i.e. List (downgrade-safe).
const VIEW_OUTLINE_FLAG: &str = "viewMode:outline";
/// Prefix shared by every overview-mode flag; the writer purges all of them
/// before inserting the one for the active mode.
const VIEW_MODE_PREFIX: &str = "viewMode:";
/// UI-flag entry: the "Hide stale" toggle is on.
const HIDE_STALE_FLAG: &str = "hideStale";
/// UI-flag entry: the "Show archived" toggle is on.
const SHOW_ARCHIVED_FLAG: &str = "showArchived";
/// Prefix of the UI-flag entry holding the active grouping dimensions in
/// activation (= nesting) order, comma-joined:
/// `groupBy:ticket,repo`. Exactly one such entry exists at a time (a string
/// set has no order of its own, so the order lives inside the one string).
const GROUP_BY_PREFIX: &str = "groupBy:";

/// The overview's persisted view state other than the repo filter (which has
/// its own `HIDDEN_REPOS_FILE`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ViewPrefs {
    mode: OverviewMode,
    hide_stale: bool,
    show_archived: bool,
    groups: Vec<GroupDimension>,
}

/// Restore [`ViewPrefs`] from the persisted UI-flag set. Unknown flags and
/// unknown/duplicate grouping dimensions are dropped rather than failing the
/// load.
fn view_prefs_from_flags(flags: &BTreeSet<String>) -> ViewPrefs {
    let mut groups: Vec<GroupDimension> = Vec::new();
    if let Some(list) = flags
        .iter()
        .find_map(|flag| flag.strip_prefix(GROUP_BY_PREFIX))
    {
        for dim in list
            .split(',')
            .filter_map(|key| GroupDimension::from_persist_key(key.trim()))
        {
            if !groups.contains(&dim) {
                groups.push(dim);
            }
        }
    }
    ViewPrefs {
        mode: OverviewMode::from_flags(flags),
        hide_stale: flags.contains(HIDE_STALE_FLAG),
        show_archived: flags.contains(SHOW_ARCHIVED_FLAG),
        groups,
    }
}

/// Write [`ViewPrefs`] into the UI-flag set, replacing any previous view-pref
/// entries and leaving every other flag (including retired ones such as the
/// old PR-sidebar flags, and future flags) untouched.
fn apply_view_prefs_to_flags(flags: &mut BTreeSet<String>, prefs: &ViewPrefs) {
    flags.retain(|flag| !flag.starts_with(GROUP_BY_PREFIX) && !flag.starts_with(VIEW_MODE_PREFIX));
    if let Some(flag) = prefs.mode.flag() {
        flags.insert(flag.to_string());
    }
    for (flag, on) in [
        (HIDE_STALE_FLAG, prefs.hide_stale),
        (SHOW_ARCHIVED_FLAG, prefs.show_archived),
    ] {
        if on {
            flags.insert(flag.to_string());
        } else {
            flags.remove(flag);
        }
    }
    if !prefs.groups.is_empty() {
        let list: Vec<&str> = prefs.groups.iter().map(|dim| dim.persist_key()).collect();
        flags.insert(format!("{GROUP_BY_PREFIX}{}", list.join(",")));
    }
}

/// Whether a just-restored repo filter hides every repo that currently has
/// chats — an invisible "everything is gone" state (typically a stale filter
/// naming repos that were since renamed/cleaned up). `known` empty means the
/// chat list hasn't arrived yet, which says nothing.
fn restored_repo_filter_hides_everything(
    hidden: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> bool {
    !known.is_empty() && known.iter().all(|repo| hidden.contains(repo))
}

/// Drop hover state when its element is no longer mounted. Hover-leave is
/// not guaranteed when filtering or changing view modes removes the element
/// under the pointer, so renderers reconcile against the
/// ids they actually built this frame.
fn reconcile_hovered_row(hovered: &mut Option<String>, mounted_ids: &[String]) -> bool {
    let stale = hovered
        .as_ref()
        .is_some_and(|id| !mounted_ids.iter().any(|mounted| mounted == id));
    if stale {
        *hovered = None;
    }
    stale
}

/// Repo-filter button label (`updateRepoFilterBtnState`,
/// `session_canvas.html:2101-2105`).
fn repo_filter_label(hidden: usize) -> String {
    if hidden > 0 {
        format!("Repos ({hidden} hidden)")
    } else {
        "Repos".to_string()
    }
}

/// Toggle one repo's visibility in the hidden set (a checkbox click).
fn toggle_repo_hidden(hidden: &mut BTreeSet<String>, repo: &str) {
    if !hidden.remove(repo) {
        hidden.insert(repo.to_string());
    }
}

/// Repo-filter list order (`renderRepoFilterList`): alphabetical, `NONE_KEY`
/// last.
fn sorted_repo_keys(known: &BTreeSet<String>) -> Vec<String> {
    let mut keys: Vec<String> = known
        .iter()
        .filter(|k| k.as_str() != overview_grouping::NONE_KEY)
        .cloned()
        .collect();
    if known.contains(overview_grouping::NONE_KEY) {
        keys.push(overview_grouping::NONE_KEY.to_string());
    }
    keys
}

/// Web's `.worktree` badge rule (`is_worktree`, `session_canvas_server.py:
/// 300-304`): cwd under `/.worktrees/` or `/.agent-worktrees/`. UI-side
/// fallback until/unless the engine's link status carries its own flag.
fn cwd_is_worktree(cwd: Option<&str>) -> bool {
    cwd.is_some_and(|c| c.contains("/.worktrees/") || c.contains("/.agent-worktrees/"))
}

/// Compact, always-visible model metadata for chat cards: the model that
/// actually ran when the harness reported it, else the configured one, else
/// an honest "default model" (the harness decides) — never the picker default.
fn chat_model_label(chat: &Chat) -> String {
    crate::chat_metadata::chat_model_summary(chat)
        .0
        .unwrap_or_else(|| "default model".to_string())
}

fn model_badge(chat: &Chat, theme: &Theme) -> gpui::AnyElement {
    let label: SharedString = chat_model_label(chat).into();
    let tooltip: SharedString = crate::chat_metadata::chat_model_summary(chat).1.into();
    div()
        .id(SharedString::from(format!("overview-model-{}", chat.id)))
        .debug_selector(|| "overview-card-model".into())
        .flex_none()
        .max_w(px(112.0))
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(theme.element_hover.opacity(0.7))
        .truncate()
        .text_size(crate::typography::ui_rems(9.0))
        .text_color(theme.text_muted.opacity(0.85))
        .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
        .child(label)
        .into_any_element()
}

/// A group's display color for `dim`/`key` — `NONE_KEY` is always
/// `--text-muted` (matches `ticketGroupMeta`/`repoGroupMeta`'s explicit
/// special-case, `session_canvas.html:~899,~914`); Repo/Ticket hash the key
/// since they have no bounded value set.
fn group_color(dim: GroupDimension, key: &str) -> gpui::Hsla {
    if key == overview_grouping::NONE_KEY {
        return gpui::rgb(0x898781).into(); // --text-muted
    }
    match dim {
        GroupDimension::Repo | GroupDimension::Ticket => hash_color(key),
    }
}

#[cfg(test)]
mod color_tests;

/// Total leaf-row count under a `Partition` node, for a group header's
/// `(N)` count — a `Groups` node's own count is the sum of its children's,
/// recursed all the way down to the `Leaves` at the bottom.
fn count_rows<T>(partition: &Partition<T>) -> usize {
    match partition {
        Partition::Leaves(rows) => rows.len(),
        Partition::Groups { groups, .. } => groups.iter().map(|g| count_rows(&g.items)).sum(),
    }
}

/// One flattened item for List mode's indented grouped rendering — a group
/// header (label/nesting-depth/leaf-count) or a row, in the depth-first order
/// [`flatten_partition`] visits the tree.
enum ListItem<'a> {
    Header {
        label: String,
        color: gpui::Hsla,
        depth: usize,
        count: usize,
    },
    Row(&'a OverviewRow),
}

/// Depth-first flatten of a `Partition` tree into `ListItem`s: a header
/// immediately followed by everything in its subtree (nested headers and
/// rows alike), each nested level one `depth` deeper — what List mode
/// indents by. The flat/no-groups case never reaches this function (see
/// `render_list_body`'s `active_groups.is_empty()` branch).
fn flatten_partition<'a>(
    partition: &Partition<&'a OverviewRow>,
    depth: usize,
    out: &mut Vec<ListItem<'a>>,
) {
    match partition {
        Partition::Leaves(rows) => {
            for &row in rows {
                out.push(ListItem::Row(row));
            }
        }
        Partition::Groups { dimension, groups } => {
            for g in groups {
                out.push(ListItem::Header {
                    label: group_label(*dimension, &g.key),
                    color: group_color(*dimension, &g.key),
                    depth,
                    count: count_rows(&g.items),
                });
                flatten_partition(&g.items, depth + 1, out);
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum ListPresentationItem {
    Header {
        label: String,
        color: gpui::Hsla,
        depth: usize,
        count: usize,
    },
    Row {
        chat_id: String,
        row_index: usize,
    },
}

impl ListPresentationItem {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Row { chat_id: left, .. }, Self::Row { chat_id: right, .. }) => left == right,
            _ => self == other,
        }
    }
}

struct LinkStatusEntry {
    status: OverviewLinkStatus,
    /// Uncommitted diff stat, if the engine's `CHAT_LINK_STATUS` response
    /// carries a `diffStat` object (`{filesChanged, linesAdded,
    /// linesRemoved}`, the reference's `diff_stat`). Read off the raw JSON
    /// rather than a `ChatLinkStatus` field so this lights up the moment the
    /// engine adds it, with no UI change — absent today.
    diff_stat: Option<DiffStat>,
    /// Engine-provided `isWorktree`, when present; otherwise the detail pane
    /// falls back to [`cwd_is_worktree`].
    is_worktree: Option<bool>,
    fetched_at: Instant,
}

/// UI-owned decode of `CHAT_LINK_STATUS`. Keeping the wire shape local lets
/// the overview accept the additive `prLinks` field while remaining
/// compatible with engines that only return the legacy singular `pr` /
/// `prSource` pair.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverviewLinkStatus {
    #[serde(default)]
    pr: Option<PrStatus>,
    #[serde(default)]
    ticket: Option<TicketStatus>,
    #[serde(default)]
    pr_source: Option<ChatLinkSource>,
    #[serde(default)]
    ticket_source: Option<ChatLinkSource>,
    #[serde(default)]
    pr_links: Vec<OverviewPrLink>,
}

/// One PR associated with a chat. `detail` can be absent while the engine's
/// background GitHub lookup is still pending; `url` remains enough to show a
/// stable placeholder and to remove this exact link.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverviewPrLink {
    url: String,
    #[serde(default)]
    source: Option<ChatLinkSource>,
    #[serde(default)]
    detail: Option<PrStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffStat {
    files_changed: u64,
    lines_added: u64,
    lines_removed: u64,
}

/// What of a landed link status feeds `OverviewRow`/grouping/search — a
/// refetch whose fingerprint is unchanged must not dirty rows (badges/detail
/// read `link_status` live at render).
fn link_fingerprint(
    status: &OverviewLinkStatus,
) -> (Option<String>, Vec<(String, u64, Option<String>)>) {
    let prs = effective_pr_links(status)
        .into_iter()
        .map(|link| {
            let number = link
                .detail
                .as_ref()
                .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number);
            let title = link.detail.as_ref().and_then(|pr| pr.title.clone());
            (link.url, number, title)
        })
        .collect();
    (status.ticket.as_ref().map(|t| t.identifier.clone()), prs)
}

/// Whether `badges_for` would render a PR/ticket badge row for this link
/// status: a linked ticket, or at least one linked PR.
fn link_has_badge_row(status: &OverviewLinkStatus) -> bool {
    status.ticket.is_some() || !effective_pr_links(status).is_empty()
}

/// Which durable link slot a `SET_CHAT_LINK` write targets — the RPC's
/// `kind: "pr" | "ticket"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Pr,
    Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkOperation {
    Add,
    Remove,
    Clear,
}

impl LinkOperation {
    fn wire(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Clear => "clear",
        }
    }
}

impl LinkKind {
    fn wire(self) -> &'static str {
        match self {
            LinkKind::Pr => "pr",
            LinkKind::Ticket => "ticket",
        }
    }
}

/// What the "Add PR / ticket…" input parsed to: the RPC `kind` plus the
/// normalized value to store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedLink {
    kind: LinkKind,
    value: String,
}

/// Error text shown under the link input when nothing parses.
const LINK_PARSE_ERROR: &str =
    "Paste a GitHub PR URL (github.com/owner/repo/pull/123) or a ticket id like ENG-1234.";

/// The link input's one-box smart parse:
/// - a `github.com/<owner>/<repo>/pull/<n>` URL (scheme and `www.`
///   optional, any trailing `/files`, `?query` or `#fragment` ignored) →
///   `pr`, normalized to `https://github.com/<owner>/<repo>/pull/<n>` —
///   the exact shape `gh` reports, which PR-link matching relies on. A
///   bare `#123` is NOT accepted: it doesn't say which repo.
/// - an `XX-1234`-shaped id (2-6 letters, `-`, digits — the engine's own
///   `extract_ticket_id` shape) → `ticket`, uppercased. A Linear issue URL
///   (`linear.app/<ws>/issue/ENG-1234/...`) is accepted too and reduced to
///   its id.
/// - anything else → [`LINK_PARSE_ERROR`].
fn parse_link_input(raw: &str) -> Result<ParsedLink, &'static str> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(LINK_PARSE_ERROR);
    }
    let without_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let without_www = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    // Drop `?query` / `#fragment` before splitting into path segments.
    let path = without_www.split(['?', '#']).next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').collect();
    match segments
        .first()
        .map(|host| host.to_ascii_lowercase())
        .as_deref()
    {
        Some("github.com") => {
            if let [_, owner, repo, "pull", number, ..] = segments.as_slice()
                && !owner.is_empty()
                && !repo.is_empty()
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                return Ok(ParsedLink {
                    kind: LinkKind::Pr,
                    value: format!("https://github.com/{owner}/{repo}/pull/{number}"),
                });
            }
            Err(LINK_PARSE_ERROR)
        }
        Some("linear.app") => match segments.as_slice() {
            [_, _, "issue", id, ..] => ticket_id_shape(id)
                .map(|value| ParsedLink {
                    kind: LinkKind::Ticket,
                    value,
                })
                .ok_or(LINK_PARSE_ERROR),
            _ => Err(LINK_PARSE_ERROR),
        },
        _ => ticket_id_shape(input)
            .map(|value| ParsedLink {
                kind: LinkKind::Ticket,
                value,
            })
            .ok_or(LINK_PARSE_ERROR),
    }
}

/// `Some(uppercased)` when the WHOLE of `s` is a ticket id: 2-6 ASCII
/// letters, `-`, one or more digits (the engine's `extract_ticket_id`
/// shape, anchored instead of searched).
fn ticket_id_shape(s: &str) -> Option<String> {
    let (letters, digits) = s.split_once('-')?;
    let ok = (2..=6).contains(&letters.len())
        && letters.bytes().all(|b| b.is_ascii_alphabetic())
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| s.to_ascii_uppercase())
}

/// Provenance tooltip for a PR/ticket line. `None` source = no durable link
/// stored; the shown PR/ticket came from branch/title inference.
fn link_source_label(kind: LinkKind, source: Option<ChatLinkSource>) -> &'static str {
    match (kind, source) {
        (_, Some(ChatLinkSource::Manual)) => "Linked manually",
        (LinkKind::Pr, Some(ChatLinkSource::CreatedInChat)) => "PR created in this chat",
        (LinkKind::Ticket, Some(ChatLinkSource::CreatedInChat)) => "Created in this chat",
        (_, Some(ChatLinkSource::Mentioned)) => "Mentioned in conversation",
        (_, Some(inferred)) => inferred.inferred_label().unwrap_or("Linked"),
        (_, None) => "Inferred from branch",
    }
}

/// `SET_CHAT_LINK`'s reply: the chat row's durable link fields after the
/// write.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetChatLinkReply {
    #[serde(default)]
    linked_pr_url: Option<String>,
    #[serde(default)]
    linked_pr_source: Option<ChatLinkSource>,
    #[serde(default)]
    linked_pr_links: Option<Vec<StoredPrLink>>,
    #[serde(default)]
    linked_ticket_id: Option<String>,
    #[serde(default)]
    linked_ticket_source: Option<ChatLinkSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPrLink {
    url: String,
    source: ChatLinkSource,
}

/// PR number off a `.../pull/<n>` URL (`0` if it has none).
fn pr_number_from_url(url: &str) -> u64 {
    url.rsplit_once("/pull/")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Stand-in `PrStatus` for a just-linked URL whose detail the engine's
/// cache hasn't fetched yet (`CHAT_LINK_STATUS` is a pure cache read — a
/// miss comes back `pr: null` until the background sweep fills it). No
/// action reasons (`has_reviewer_requested: true`), so it never paints a
/// false "Needs reviewer" badge.
fn placeholder_pr(url: &str) -> PrStatus {
    PrStatus {
        additions: None,
        deletions: None,
        merged_at: None,
        number: pr_number_from_url(url),
        url: Some(url.to_string()),
        state: "loading…".to_string(),
        is_draft: false,
        review_decision: None,
        reviewers: Vec::new(),
        checks: None,
        title: None,
        branch: None,
        head_sha: None,
        mergeable: String::new(),
        has_reviewer_requested: true,
    }
}

/// Optimistic local update after a successful `SET_CHAT_LINK` — the badge/
/// detail change the same frame instead of waiting on the refetch (and on
/// the engine's sweep to cache the new PR/ticket's detail). A set whose
/// value already matches what's shown keeps the real detail; a different
/// value gets a placeholder; a clear drops the slot (the follow-up refetch
/// brings back whatever inference finds).
fn effective_pr_links(status: &OverviewLinkStatus) -> Vec<OverviewPrLink> {
    if !status.pr_links.is_empty() {
        return status.pr_links.clone();
    }
    status
        .pr
        .as_ref()
        .map(|pr| OverviewPrLink {
            url: pr.url.clone().unwrap_or_default(),
            source: status.pr_source,
            detail: Some(pr.clone()),
        })
        .into_iter()
        .collect()
}

fn header_links(status: &OverviewLinkStatus) -> crate::chat_metadata::HeaderLinks {
    crate::chat_metadata::HeaderLinks {
        prs: effective_pr_links(status)
            .into_iter()
            .map(|link| crate::chat_metadata::HeaderPr {
                number: link
                    .detail
                    .as_ref()
                    .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number),
                title: link.detail.as_ref().and_then(|pr| pr.title.clone()),
                status: link
                    .detail
                    .as_ref()
                    .map(crate::chat_metadata::HeaderPr::status_from_detail),
                url: link.url,
                source: link.source,
            })
            .collect(),
        ticket: status
            .ticket
            .as_ref()
            .map(|ticket| crate::chat_metadata::HeaderTicket {
                identifier: ticket.identifier.clone(),
                title: ticket.title.clone(),
                url: ticket.url.clone(),
                state: ticket.status.clone().filter(|s| !s.trim().is_empty()),
            }),
    }
}

fn apply_link_reply(status: &mut OverviewLinkStatus, kind: LinkKind, reply: &SetChatLinkReply) {
    match kind {
        LinkKind::Pr => {
            if let Some(links) = &reply.linked_pr_links {
                let previous = effective_pr_links(status);
                status.pr_links = links
                    .iter()
                    .map(|link| {
                        let detail = previous
                            .iter()
                            .find(|old| old.url == link.url)
                            .and_then(|old| old.detail.clone())
                            .or_else(|| Some(placeholder_pr(&link.url)));
                        OverviewPrLink {
                            url: link.url.clone(),
                            source: Some(link.source),
                            detail,
                        }
                    })
                    .collect();
                status.pr = status.pr_links.first().and_then(|link| link.detail.clone());
                status.pr_source = status.pr_links.first().and_then(|link| link.source);
            } else {
                match reply.linked_pr_url.as_deref() {
                    Some(url) => {
                        if status.pr.as_ref().and_then(|p| p.url.as_deref()) != Some(url) {
                            status.pr = Some(placeholder_pr(url));
                        }
                        status.pr_source = reply.linked_pr_source;
                    }
                    None => {
                        status.pr = None;
                        status.pr_source = None;
                    }
                }
            }
        }
        LinkKind::Ticket => match reply.linked_ticket_id.as_deref() {
            Some(id) => {
                if status.ticket.as_ref().map(|t| t.identifier.as_str()) != Some(id) {
                    status.ticket = Some(TicketStatus {
                        identifier: id.to_string(),
                        title: None,
                        url: None,
                        status: None,
                    });
                }
                status.ticket_source = reply.linked_ticket_source;
            }
            None => {
                status.ticket = None;
                status.ticket_source = None;
            }
        },
    }
}

/// A landed `CHAT_LINK_STATUS` refetch, merged over what was shown: when a
/// durable link exists (`*_source` set) but the engine's cache hasn't got
/// its detail yet (`pr`/`ticket` null), keep the previous value (typically
/// [`apply_link_reply`]'s placeholder) instead of blanking the badge until
/// the sweep catches up.
fn merge_refetched_link(
    previous: Option<&OverviewLinkStatus>,
    mut next: OverviewLinkStatus,
) -> OverviewLinkStatus {
    if let Some(prev) = previous {
        for link in &mut next.pr_links {
            if link.detail.is_none() {
                link.detail = effective_pr_links(prev)
                    .into_iter()
                    .find(|old| old.url == link.url)
                    .and_then(|old| old.detail);
            }
        }
        if next.pr.is_none() && next.pr_source.is_some() && prev.pr_source.is_some() {
            next.pr = prev.pr.clone();
        }
        if next.ticket.is_none() && next.ticket_source.is_some() && prev.ticket_source.is_some() {
            next.ticket = prev.ticket.clone();
        }
    }
    next
}

/// Tiny hover tooltip carrying one short text (link provenance, a compact
/// subagent row's full name). Wraps past `max_w` (web `.name-tooltip`'s
/// `max-width` + `white-space: normal`).
struct LinkSourceTooltip(SharedString);

impl Render for LinkSourceTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .max_w(px(280.0))
            .text_size(px(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// Attach the provenance tooltip ([`link_source_label`]) to a PR/ticket
/// element.
fn link_source_hover(
    el: gpui::Stateful<gpui::Div>,
    kind: LinkKind,
    source: Option<ChatLinkSource>,
) -> gpui::Stateful<gpui::Div> {
    let label = SharedString::from(link_source_label(kind, source));
    el.tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(label.clone())).into())
}

/// The tiny pin glyph marking a manually linked PR/ticket.
fn manual_link_pin(color: gpui::Hsla) -> gpui::Svg {
    crate::icons::icon(crate::icons::PIN)
        .size(px(9.0))
        .text_color(color.opacity(0.8))
}

/// Port of `session_canvas_server.py`'s staleness spirit ("quiet AND not
/// currently doing anything"), not its literal cursor-liveness mechanism —
/// see `STALE_AFTER`'s doc comment.
fn is_stale(chat: &Chat, status: ChatIndicator, now: chrono::DateTime<Utc>) -> bool {
    if matches!(
        status,
        ChatIndicator::Working | ChatIndicator::AwaitingInput
    ) {
        return false;
    }
    let last_activity = chat.last_message_at.unwrap_or(chat.created_at);
    now.signed_duration_since(last_activity) > STALE_AFTER
}

/// Label + severity color for a PR action reason — exact `ACTION_META`
/// (`session_canvas.html:2206-2212`) labels and `--status-*` hex values.
fn pr_action_badge(action: PrAction) -> (&'static str, gpui::Hsla) {
    let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
    match action {
        PrAction::CiFailing => ("CI failing", critical),
        PrAction::MergeConflict => ("Merge conflict", critical),
        PrAction::ChangesRequested => ("Changes requested", critical),
        PrAction::NeedsReviewer => ("Needs reviewer", gpui::rgb(0xfab219).into()),
        PrAction::ReadyToMerge => ("Ready to merge", gpui::rgb(0x0ca30c).into()),
    }
}

/// The main column's rendering mode.
///
/// Existing view-mode preferences load as Outline. List remains only for
/// legacy rendering and tests while its implementation is retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OverviewMode {
    List,
    #[default]
    Outline,
}

impl OverviewMode {
    fn from_flags(_flags: &BTreeSet<String>) -> Self {
        Self::Outline
    }

    /// The flag written for this mode (`None` = List, written as absence).
    fn flag(self) -> Option<&'static str> {
        match self {
            Self::List => None,
            Self::Outline => Some(VIEW_OUTLINE_FLAG),
        }
    }
}

/// Height of the PR/ticket badge row `badges_for` renders when present.
const BADGE_ROW_H: f32 = 18.0;
/// Height of one compact nested subagent row (`.tile.compact`).
const SUBAGENT_TILE_H: f32 = 24.0;
/// Gap between adjacent compact subagent tiles.
const SUBAGENT_TILE_GAP: f32 = 4.0;
/// Keep rows scannable at rest. A fourth compact row communicates the
/// remainder; hovering the row swaps the preview for the complete stack.
const SUBAGENT_PREVIEW_LIMIT: usize = 3;

/// The fill bar's filled fraction: `Math.round((pct || 0) * 100)%`.
fn context_fill_fraction(pct: Option<f32>) -> f32 {
    context_pct_percent(pct).unwrap_or(0) as f32 / 100.0
}

/// Rounded whole percent, `None` for null. Everything this view renders
/// from the pct (width, fill bar, label) is determined by this value, so
/// it's also the refetch no-op key (see [`context_pct_changed`]).
fn context_pct_percent(pct: Option<f32>) -> Option<i32> {
    pct.filter(|p| p.is_finite())
        .map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as i32)
}

/// `.pct-label`: `${pctLabel} context`, `pctLabel` = `N%` or `—` for null.
fn context_pct_label(pct: Option<f32>) -> String {
    match context_pct_percent(pct) {
        Some(n) => format!("{n}% context"),
        None => "— context".to_string(),
    }
}

/// Whether a landed context fetch changes anything visible — only then may
/// it dirty rows. A first fetch that comes back
/// null is no change (the row already rendered as null), and sub-percent
/// drift on a live chat is no change either (see [`context_pct_percent`]).
fn context_pct_changed(previous: Option<Option<f32>>, next: Option<f32>) -> bool {
    context_pct_percent(previous.flatten()) != context_pct_percent(next)
}

/// The context bar + `N% context` label from `renderTile`'s tile face
/// (`.fill-bar` 4px/radius 2 on `--gridline` with an `--accent` fill,
/// `.pct-label` 10px muted tabular), as shown on each list row.
fn context_meter(pct: Option<f32>, theme: &Theme) -> gpui::Div {
    let accent: gpui::Hsla = gpui::rgb(0x3987e5).into(); // --accent
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .w_full()
                .h(px(4.0))
                .rounded(px(2.0))
                .overflow_hidden()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(context_fill_fraction(pct)))
                        .bg(accent),
                ),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(context_pct_label(pct))),
        )
}

/// Width of the interactive chat panel. Wide enough that the embedded
/// composer's footer (pickers, send) isn't crushed — the composer itself
/// caps at `COMPOSER_MAX_WIDTH` (768).
const CHAT_PANEL_W: f32 = 540.0;
/// Horizontal inset of the composer inside the chat panel.
const CHAT_PANEL_COMPOSER_PAD: f32 = 10.0;
/// The embedded transcript's top inset (`Transcript::set_top_inset`): the
/// panel's own header sits ABOVE the transcript rather than overlaying it,
/// so only a little breathing room replaces the chat route's titlebar inset.
pub(crate) const CHAT_PANEL_TRANSCRIPT_TOP_INSET: f32 = 8.0;

/// The chat the interactive panel shows: always the current selection, and
/// only while the panel is open. `None` while open means the selection went
/// away (deleted/archived/new-session) and the panel should close itself.
fn panel_chat_id(panel_open: bool, selected_chat: Option<&str>) -> Option<String> {
    if !panel_open {
        return None;
    }
    selected_chat
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub struct Overview {
    state: Entity<AppState>,
    shell: gpui::WeakEntity<Shell>,
    list: ListState,
    list_items: Vec<ListPresentationItem>,
    list_rows: std::rc::Rc<Vec<OverviewRow>>,
    /// The focus target for the List surface: Esc, Enter and Down
    /// from the search field land here, and a click on the surface takes it.
    body_focus: gpui::FocusHandle,
    /// The keyboard-selected List row (chat id).
    list_selected: Option<String>,
    /// Rendered List rows in on-screen order: `(chat id, child index in the
    /// scroll container)`, headers counted. Rebuilt every List render.
    list_order: Vec<(String, usize)>,
    view_mode: OverviewMode,
    pub(crate) in_sidebar: bool,
    compact_show_assistant: bool,
    sidebar_scope: Option<String>,
    /// Rows currently showing their expanded detail (toggled by a click,
    /// matching the reference's `expandedIds`/`toggleExpanded`). Never causes
    /// navigation; only the explicit "Open chat" affordance does that.
    expanded: HashSet<String>,
    link_status: HashMap<String, LinkStatusEntry>,
    link_checkouts: HashMap<String, Option<(String, String, Option<String>)>>,
    link_status_pending: HashSet<String>,
    /// Each host device's scheduled tasks, for task rows.
    scheduled_tasks: HashMap<String, (Instant, std::sync::Arc<mast_proto::ScheduledTasksSnapshot>)>,
    scheduled_tasks_pending: HashSet<String>,
    /// Subagents per chat id. Fetched once per chat (not refreshed on an
    /// interval like `link_status` — a session's own subagent set is
    /// settled once its transcript exists, unlike a PR's live checks/review
    /// state) and only for chats that came through the external-import flow
    /// (anything else has no transcript path to scan, per
    /// `SCAN_CHAT_SUBAGENTS`'s contract).
    subagents: HashMap<String, Vec<SubagentItem>>,
    subagent_pending: HashSet<String>,
    /// The list row with subagent overflow currently hovered.
    /// Kept separate from `expanded`: hover must not acknowledge a completed
    /// chat or open its detail panel.
    hovered_row: Option<String>,
    /// Last attempt time per chat (success OR failure), so a chat with
    /// nothing to scan retries on `SUBAGENTS_REFRESH` instead of on every
    /// row rebuild.
    subagents_fetched_at: HashMap<String, Instant>,
    /// Per chat: whether it had a harness session when last checked, and the
    /// earliest instant any of its row fetches (subagents, context usage,
    /// link status) can be due again. `rows()` and the idle tick skip the
    /// chat until then. Absent while a fetch is in flight or never landed.
    row_fetch_due: HashMap<String, (bool, Instant)>,
    /// `CHAT_CONTEXT_USAGE` per chat id (`None` = engine had no usage).
    /// Refetched on `CONTEXT_USAGE_REFRESH`.
    context_usage: HashMap<String, Option<f32>>,
    context_usage_pending: HashSet<String>,
    context_usage_fetched_at: HashMap<String, Instant>,
    /// Chat ids whose "done" has been acknowledged (row expanded / chat
    /// opened) — the web's `acknowledgedDone`, persisted via
    /// `overview_positions::ACKNOWLEDGED_DONE_FILE`.
    acknowledged_done: BTreeSet<String>,
    /// Repo keys the repo filter hides (web `hiddenRepos`), persisted via
    /// `overview_positions::HIDDEN_REPOS_FILE`.
    hidden_repos: BTreeSet<String>,
    /// Every repo key seen across rows before the repo/search/stale filters
    /// — the repo filter lists these so unchecking a repo never makes its
    /// own checkbox vanish (web: built from `lastSessions`).
    known_repos: BTreeSet<String>,
    /// Whether `acknowledged_done`/`hidden_repos` have been read from disk
    /// (lazily, once `AppState.data_dir` exists).
    prefs_loaded: bool,
    /// A view pref (mode / toggles / grouping) was changed by the user before
    /// `prefs_loaded` — the older file must not then overwrite it.
    view_prefs_touched: bool,
    /// The restored repo filter still has to be sanity-checked against the
    /// first non-empty chat list (see [`restored_repo_filter_hides_everything`]).
    /// One-shot so a deliberate "None" in the dropdown isn't undone later.
    repo_filter_needs_validation: bool,
    /// The Repos dropdown — the app's standard `Popup` lifecycle (exit
    /// animation + the trigger-press note that keeps a trigger click from
    /// close-then-reopening).
    repo_filter: popover::Popup<()>,
    filter_menu: popover::Popup<()>,
    assistant_chooser: popover::Popup<()>,
    /// Rows before any filter (archived-inclusion aside) — lets the empty
    /// state tell "no chats at all" from "everything filtered out".
    unfiltered_count: usize,
    refresh_ticks: u32,
    _refresh_task: gpui::Task<()>,
    /// Composable grouping dimensions, in activation (click) order — the
    /// order they appear here IS the nesting order, matching the reference's
    /// `activeGroups` insertion-order semantics (`overview_grouping::partition`'s
    /// doc comment). Empty = flat/ungrouped, the default.
    active_groups: Vec<GroupDimension>,
    /// "Hide stale" toggle — default off, matches the reference tool's own
    /// `hideStale` default (`session_canvas.html`).
    hide_stale: bool,
    /// "Show archived" toggle — default off, matches the reference's own
    /// `showArchived` default. Archived rows render heavily dimmed when on.
    show_archived: bool,
    /// Quick filter for sessions needing interaction or stalled.
    attention_filter: AttentionFilter,
    /// Pre-attention-filter count of rows needing user interaction.
    needs_interaction_count: usize,
    /// Pre-attention-filter count of errored rows.
    stalled_count: usize,
    /// The chat the List should scroll into view on the next render (opening
    /// the link editor) — consumed by the List body
    /// and cleared after that render either way.
    focus_due: Option<String>,
    /// Set whenever something that could change `rows()`'s OUTPUT happens
    /// (the underlying chat list, `hide_stale`/`show_archived`, or a
    /// link-status/subagent/context-usage fetch landing with
    /// changed data) — `false` the rest of the time, including every
    /// hover/scroll frame. `rows()` skips its expensive rebuild (walking
    /// `AppState`, cloning every `Chat`, re-deriving staleness) on a `false`
    /// read and returns the cache instead.
    rows_dirty: bool,
    perf: OverviewPerf,
    /// `AppState::overview_fingerprint` as of the last observed change.
    state_fingerprint: Option<u64>,
    /// The snapshots (in space order) `work_item_index` was built from; the
    /// index is rebuilt only when one of them is replaced.
    work_item_index_key: Option<Vec<std::sync::Arc<WorkOutlineSnapshot>>>,
    rows_cache: std::rc::Rc<Vec<OverviewRow>>,
    /// Filters `rows()` by title/cwd/preview substring (case-insensitive) —
    /// there was no way at all to search/filter across a real 227+-chat
    /// set before this. Same `ComposerInput` pattern `pickers.rs` already
    /// uses for its own search boxes.
    search: Entity<ComposerInput>,
    /// "View full chat →" opens an interactive chat panel alongside the
    /// overview instead of navigating away to `Route::Chat` — the reference
    /// tool's own `openTranscriptPanel` shape (an in-page panel, never
    /// leaving the overview), but live: it SELECTS the chat
    /// (`Shell::preview_chat_in_overview`, route untouched) and renders the
    /// Shell's own `transcript`/`composer` entities, so sending, queueing,
    /// the question wizard, interrupt, attachments and resume all work
    /// exactly as on the chat route (they key off `AppState.selected_chat`,
    /// not the route). The chat shown is always `selected_chat` — no id is
    /// stored here, so a sidebar click while the panel is open retargets it.
    ///
    /// Invariant: those entities must never render in two places in one
    /// frame. This view only renders on `Route::Overview`, and the Shell's
    /// chat stack only renders on `Route::Chat` (`render_main`'s early
    /// return), so the routes keep them exclusive.
    chat_panel_open: bool,
    /// Chat whose overview-panel identity control is showing "Copied".
    copied_panel_identity: Option<String>,
    /// Invalidates an older feedback timer when the user copies again.
    copied_panel_identity_seq: u64,
    /// The Shell's own conversation transcript (see `chat_panel_open`).
    transcript: Entity<Transcript>,
    /// The Shell's own composer (see `chat_panel_open`).
    composer: Entity<crate::composer::Composer>,
    /// The chat the panel shows this frame (`panel_chat_id`), snapshotted at
    /// the top of `render` so `render_list_row` can outline it without a
    /// `cx`.
    panel_highlight: Option<String>,
    /// The chat whose expanded detail shows the inline "Add PR / ticket…"
    /// input (at most one at a time — `link_input` is a single entity and
    /// must render in one place per frame).
    link_editor_chat: Option<String>,
    /// The link editor's own input (`PALETTE_SEARCH_CONTEXT`, so editing
    /// keys work and Enter/Escape bubble to the wrapper's key handler).
    link_input: Entity<ComposerInput>,
    /// A `SET_CHAT_LINK` from the editor is in flight (ignore repeat Enter).
    link_submitting: bool,
    /// `(chat_id, message)`: parse or RPC error to show in that chat's
    /// expanded detail (editor or unlink).
    link_error: Option<(String, String)>,
    /// The right-side panel's read-only subagent-transcript mode (see
    /// [`SubagentPeek`]). Mutually exclusive with `chat_panel_open`: opening
    /// either replaces the other (`open_subagent_peek`/`open_chat_panel`),
    /// and closing a peek returns to the bare list, never to a chat
    /// panel it replaced.
    subagent_peek: Option<SubagentPeek>,
    /// Monotonic id for peek fetches — web `transcriptRequestId`.
    peek_request_seq: u64,
    /// The peek body's scroll, pinned to the bottom when its turns land.
    peek_scroll: ScrollHandle,
    /// Focused when a peek opens, so Escape reaches the panel's key handler
    /// wherever the pointer is.
    peek_focus: gpui::FocusHandle,
    /// The Outline presentation (`OverviewMode::Outline`).
    outline: Entity<OutlineView>,
    /// Where Toggle Outline Focus returns from the tree: the last Chat opened
    /// from the Outline (Enter, Details, Required Actions) or the Chat the
    /// toggle left, with the layout it was shown in.
    outline_chat_return: Option<OutlineChatReturn>,
    /// Focus the tree on the next render: arriving from another route, its
    /// element is not mounted yet when the toggle asks for focus, and the
    /// Shell's unmounted-focus recovery would take it back.
    outline_focus_pending: bool,
    /// The pending preview-on-select (see `schedule_outline_preview`);
    /// replacing or dropping it cancels the switch.
    outline_preview: Option<gpui::Task<()>>,
    /// The Outline's inputs must be re-pushed (set whenever `rows()`
    /// rebuilds, or an Outline-only option changes).
    outline_dirty: bool,
    /// Every Chat as the Outline sees it (child and archived Chats included,
    /// with the Overview's filters folded into `visible`). Built by `rows()`.
    outline_chats: std::sync::Arc<Vec<OutlineChat>>,
    /// The explicit "reveal completed items" control (they hide while
    /// "Hide stale" is on).
    outline_show_completed: bool,
    /// Test/fixture override for the per-Space Work Outline loads (instead
    /// of `AppState::work_outline`).
    outline_fixture: Option<HashMap<String, WorkOutlineLoad>>,
    /// The Space set last declared through `AppState::set_work_outline_visible`.
    declared_outline_spaces: Option<Vec<String>>,
    /// Chat → work item lookups across every loaded Space (path chips,
    /// attention, the work-item filter). Rebuilt by `rows()`.
    work_item_index: WorkItemIndex,
    /// List work-item filter (item ids + `UNASSOCIATED_KEY`),
    /// persisted via `overview_positions::WORK_ITEM_FILTER_FILE`.
    work_item_filter: BTreeSet<String>,
    /// One-shot stale-filter check once every Space's outline has loaded.
    work_item_filter_needs_validation: bool,
    work_item_filter_popup: popover::Popup<()>,
    /// Work Item Details (Overview-owned right surface). While `Some` it
    /// renders in the right slot instead of the chat panel / peek, leaving
    /// `chat_panel_open`, `subagent_peek`, and the selection untouched, so
    /// closing it restores the previous layout.
    work_item_details: Option<Entity<WorkItemDetails>>,
    /// Expand Effective Instructions once details for this `(space, item)`
    /// open (palette "Open Effective Instructions" across a discard prompt).
    pending_effective: Option<(String, String)>,
    /// Expand Proposed instructions once details for this `(space, item)`
    /// open (Outline / chat-header proposal indicators).
    pending_proposals: Option<(String, String)>,
    /// The global Required Actions surface (Milestone 4): renders in the
    /// right slot under Work Item Details (details opened from it stack on
    /// top; closing them shows it again) and above the chat panel / peek.
    required_actions_panel:
        Option<Entity<crate::work_outline::required_actions_view::RequiredActionsList>>,
    _required_actions_events: Option<Subscription>,
    /// The Linear panel (Linear-enabled Spaces only): its own left column,
    /// independent of the right slot. `Some` while it shows.
    linear_panel: Option<Entity<crate::linear::panel::LinearPanel>>,
    _linear_panel_events: Option<Subscription>,
    /// The Linear column's persisted open state and width, its open/close
    /// tween, and an in-flight edge drag (`overview/linear_host.rs`).
    linear_column: linear_host::LinearColumnState,
    /// Width of the right-slot Details / Required Actions panel.
    details_width: details_width::DetailsWidthState,
    /// The GitHub section of the same left column (PR review sync;
    /// `overview/github_host.rs`). `Some` while it shows.
    github_panel: Option<Entity<crate::github::panel::GithubPanel>>,
    _github_panel_events: Option<Subscription>,
    /// Tests run without a Shell: PR review starts land here.
    #[cfg(test)]
    pub(crate) captured_pr_review: Option<(
        mast_proto::PullRequestRef,
        Option<(String, String)>,
        mast_proto::PrReviewAction,
    )>,
    /// "Add issues from <project>…" on a project-linked item: a picker over
    /// the Overview, independent of the Linear panel (`linear_host.rs`).
    project_issue_picker: Option<Entity<crate::linear::project_picker::ProjectIssuePicker>>,
    _project_issue_picker_events: Option<Subscription>,
    /// Tests run without a Shell: Start Chat requests land here.
    #[cfg(test)]
    pub(crate) captured_start_chat: Option<(String, String)>,
    #[cfg(test)]
    pub(crate) captured_start_manager: Option<(String, String)>,
    _details_events: Option<Subscription>,
    _outline_events: Subscription,
    _observe: Subscription,
    _search_events: Subscription,
    _link_input_events: Subscription,
}

impl Overview {
























    fn outline_spaces_in_scope(&self, scope: Option<&str>, cx: &gpui::App) -> Vec<OutlineSpace> {
        let state = self.state.read(cx);
        let mut spaces: Vec<&Space> = state
            .spaces
            .iter()
            .filter(|space| scope.is_none_or(|scope| space.id == scope))
            .collect();
        spaces.sort_by_cached_key(|space| (space.display_name().to_lowercase(), space.id.clone()));
        spaces
            .into_iter()
            .map(|space| OutlineSpace {
                id: space.id.clone(),
                label: space.display_name().to_string(),
                load: match &self.outline_fixture {
                    Some(map) => map
                        .get(&space.id)
                        .cloned()
                        .unwrap_or(WorkOutlineLoad::Loading),
                    None => state.work_outline(&space.id),
                },
                // The Outline applies its own hidden set (`OutlineView`).
                hidden: false,
            })
            .collect()
    }



    /// Open the interactive chat panel for `chat_id` alongside the overview,
    /// instead of navigating away — see `chat_panel_open`'s doc comment.
    /// Selecting the chat is what wires the Shell's transcript/composer to
    /// it; re-clicking the already-shown chat just refocuses the composer.
    pub(crate) fn is_chat_panel_open(&self) -> bool {
        self.chat_panel_open
    }










    /// Refresh `device`'s scheduled-task list (its owner answers through the
    /// existing device routing) for the task rows' schedule and next run.
    fn ensure_scheduled_tasks(&mut self, device: &str, cx: &mut Context<Self>) {
        let fresh = self
            .scheduled_tasks
            .get(device)
            .is_some_and(|(at, _)| at.elapsed() < SCHEDULED_TASKS_REFRESH);
        if fresh || self.scheduled_tasks_pending.contains(device) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let device = device.to_owned();
        self.scheduled_tasks_pending.insert(device.clone());
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<mast_proto::ScheduledTasksSnapshot>(
                    methods::SCHEDULED_TASKS,
                    serde_json::json!({ "action": "list", "targetDeviceId": device }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.scheduled_tasks_pending.remove(&device);
                // A host that cannot answer keeps its rows, without schedule
                // details, and is retried after the same interval.
                let snapshot = result.map(std::sync::Arc::new).unwrap_or_else(|_| {
                    this.scheduled_tasks
                        .get(&device)
                        .map(|(_, s)| s.clone())
                        .unwrap_or_default()
                });
                // Task rows read this live, like badges: repaint only.
                this.scheduled_tasks
                    .insert(device, (Instant::now(), snapshot));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The task definition behind a scheduled run, when its host reported it.
    fn task_for(
        &self,
        chat: &Chat,
    ) -> Option<(&mast_proto::ScheduledTask, &[mast_proto::ScheduledRun])> {
        let info = chat.scheduled_task_info.as_ref()?;
        let (_, snapshot) = self.scheduled_tasks.get(&chat.device_id)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == info.task_id && t.device_id == chat.device_id)?;
        Some((task, snapshot.runs.as_slice()))
    }


















}

impl Render for Overview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.in_sidebar {
            self.render_sidebar_workspace(window, cx)
        } else {
            let navigation = self.render_navigation(window, cx);
            self.render_workspace(navigation, window, cx)
        }
    }
}

impl Overview {






}

/// How a card/row is dimmed and whether it carries the unread-done ribbon:
/// `(opacity, ribbon)`.
///
/// The ribbon replaced a white box-shadow halo: gpui's `opacity` multiplies
/// the alpha of EVERYTHING the element paints — its box-shadow included —
/// and a gpui shadow is a blurred filled rect painted *under* the card, so
/// on a translucent (dimmed) card the white halo both faded AND bled through
/// the card's own background as a milky wash. That doesn't apply to the
/// ribbon (it only ever shows at full opacity — see below), but the
/// precedence it sits under is unchanged. Dimmed + unread was also the
/// common case, not an edge case: the unread set starts empty, so every
/// never-opened done chat quiet for >`STALE_AFTER` was stale and unread at
/// once. Hence:
/// - unread-done beats stale: it's the newest, most attention-worthy
///   signal, so the card renders at full opacity with the ribbon showing;
///   once acknowledged, normal stale dimming returns.
/// - archived beats unread-done: the user explicitly put it away, so it
///   dims (0.4) and shows no ribbon at all (a faded, bleeding ribbon is
///   exactly the look being avoided).
fn card_dimming(archived: bool, stale: bool, unread_done: bool) -> (Option<f32>, bool) {
    if archived {
        return (Some(0.4), false);
    }
    if unread_done {
        return (None, true);
    }
    (stale.then_some(0.55), false)
}

/// Width (px) of the unread-done left-edge ribbon, replacing the old white
/// halo (`unread_done_glow`, a two-layer box-shadow — removed: it bled
/// through a dimmed card's translucent background as a milky wash, per
/// `card_dimming`'s doc comment). A solid edge bar has no such bleed
/// concern (the ribbon only ever renders at full opacity), reads cleanly
/// against any of the status-tinted card backgrounds (blue/green/red/
/// yellow), and against the green tint every done card already carries —
/// plain white is the safest choice against all of them, so that's the
/// color used at the call sites, at full opacity.
///
/// `scale` is the swatch scale (`1.0` for list rows), floored so the
/// ribbon stays visible zoomed out — same floor `unread_done_glow` used for
/// its blur/spread.
const UNREAD_DONE_RIBBON_WIDTH: f32 = 3.0;
const UNREAD_DONE_RIBBON_MIN_SCALE: f32 = 0.5;

fn unread_done_ribbon_width(scale: f32) -> f32 {
    UNREAD_DONE_RIBBON_WIDTH * scale.max(UNREAD_DONE_RIBBON_MIN_SCALE)
}

/// The ribbon itself: an absolutely-positioned bar flush along the left
/// edge, `top`/`bottom` pinned to `0` so it spans the card's full height.
/// Relies on the caller already being `overflow_hidden()` with rounded
/// corners (every caller here is) to clip it to the corner radius, rather
/// than rounding the bar's own corners — one clip mask instead of matching
/// radii by hand at every zoom level.
fn unread_done_ribbon(scale: f32) -> gpui::Div {
    div()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        .left(px(0.0))
        .w(px(unread_done_ribbon_width(scale)))
        .bg(gpui::white())
}

impl Overview {


    /// A scheduled task's row: the task, not its run. Name, schedule, next
    /// run and last outcome, with the dot of its latest run. Clicking opens
    /// that run; History opens the task's run history.
    fn render_task_row(
        &mut self,
        ix: usize,
        row: &OverviewRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let chat = &row.chat;
        let Some(info) = chat.scheduled_task_info.clone() else {
            return div().into_any_element();
        };
        self.ensure_scheduled_tasks(&chat.device_id, cx);
        let chat_id = chat.id.clone();
        let task = self.task_for(chat);
        let name = task.map_or(info.task_name.clone(), |(t, _)| t.draft.name.clone());
        let mut meta = Vec::new();
        if let Some((task, runs)) = task {
            meta.push(crate::scheduled_tasks::schedule_label(&task.draft.schedule));
            meta.push(
                crate::scheduled_tasks::next_run_label(task).unwrap_or_else(|| {
                    if task.draft.enabled {
                        "No next run"
                    } else {
                        "Paused"
                    }
                    .into()
                }),
            );
            meta.push(format!(
                "Last: {}",
                crate::scheduled_tasks::last_outcome_label(runs, &task.id)
            ));
        } else {
            let (_, label) = status_style(row.status);
            meta.push(format!("Last: {label}"));
        }
        let last_run = format!(
            "Latest run {}",
            format_time_ago(
                chrono::DateTime::from_timestamp_millis(mast_proto::task_chat_order(chat).0)
                    .unwrap_or(chat.created_at),
                Utc::now()
            )
        );
        let dot = crate::status_style::status_style(row.status.into(), theme.appearance).color;
        let indicator = crate::components::outline_chat_indicator(
            row.status.into(),
            format!("overview-task-status-{chat_id}"),
            dot,
            theme,
            cx.entity_id(),
            cx,
        );
        let open_id = chat_id.clone();
        let history_task = info.task_id.clone();
        let history_device = chat.device_id.clone();
        let row_selector = format!("overview-task-row-{}", info.task_id);
        let history_selector = format!("overview-task-history-{}", info.task_id);
        crate::components::scheduled_task_parts::task_row()
            .id(("overview-task-row", ix))
            .debug_selector(move || row_selector.clone())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(8.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .when(
                self.panel_highlight.as_deref() == Some(chat_id.as_str()),
                |el| el.border_color(theme.accent),
            )
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover))
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat_panel(open_id.clone(), cx)))
            .child(indicator)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                crate::components::scheduled_task_parts::task_kind()
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child("Scheduled task"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(name),
                            ),
                    )
                    .child(
                        crate::components::scheduled_task_parts::task_details()
                            .truncate()
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(meta.join(" · ")),
                    ),
            )
            .child(
                crate::components::scheduled_task_parts::task_time()
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(last_run),
            )
            .child(
                crate::components::scheduled_task_parts::task_history()
                    .id(SharedString::from(history_selector.clone()))
                    .debug_selector(move || history_selector.clone())
                    .flex_none()
                    .px(px(8.0))
                    .py(px(2.0))
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .bg(theme.accent.opacity(0.1))
                    .hover(|el| el.bg(theme.accent.opacity(0.2)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let (task, device) = (history_task.clone(), history_device.clone());
                        let _ = this.shell.update(cx, |shell, cx| {
                            shell.open_scheduled_task_history(task, device, cx)
                        });
                    }))
                    .child("History"),
            )
            .into_any_element()
    }

}

impl Overview {




    #[cfg(test)]
    pub(crate) fn work_item_details_for_test(&self) -> Option<Entity<WorkItemDetails>> {
        self.work_item_details.clone()
    }






































    /// Open Start Chat for a project's Manager (its placeholder row).
    pub(crate) fn start_manager_for_item(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot = self
            .outline_snapshot(&space_id, cx)
            .ok_or("This Space's outline is not loaded")?;
        let item = snapshot.item(&item_id).ok_or("That work item is gone")?;
        if !item.is_project {
            return Err("Only a project has a Manager");
        }
        #[cfg(test)]
        if self.shell.upgrade().is_none() {
            self.captured_start_manager = Some((space_id, item_id));
            return Ok(());
        }
        let shell = self
            .shell
            .upgrade()
            .ok_or("Starting a Manager needs the app window")?;
        cx.defer(move |cx| {
            shell.update(cx, |shell, cx| {
                shell.open_start_manager_or_notice(&space_id, &item_id, cx)
            });
        });
        Ok(())
    }







    pub(crate) fn dismiss_required_actions(
        &mut self,
        request_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.open_required_actions(cx);
        let spaces = self.required_action_spaces(cx);
        let chats = self.outline_chats.clone();
        if let Some(panel) = &self.required_actions_panel {
            panel.update(cx, |panel, cx| {
                panel.set_data(spaces, chats);
                for request_id in request_ids {
                    panel.external_dismiss(request_id, "menu", cx);
                }
            });
        }
    }





    #[cfg(feature = "snapshot")]
    pub(crate) fn show_snapshot_attention_empty(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.apply_work_outline_frame(
                "fixture-space",
                mast_proto::WorkOutlineSnapshot {
                    space_id: "fixture-space".into(),
                    available: true,
                    synced: true,
                    ..Default::default()
                },
            );
            cx.notify();
        });
        self.attention_filter = AttentionFilter::NeedsInteraction;
        self.rows_dirty = true;
        self.outline_dirty = true;
        cx.notify();
    }

    /// Outline with `w-api` marked a project: `chat` gives its Manager the
    /// fixture chat `fixture-design` (model badge), `placeholder` leaves the
    /// Manager before its first message. `unselected` also makes `w-ui` a
    /// project with a placeholder Manager, selects neither Manager so every
    /// row shows its resting fill, and collapses `w-api`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn show_snapshot_manager(
        &mut self,
        with_chat: bool,
        unselected: bool,
        cx: &mut Context<Self>,
    ) {
        use mast_proto::{StandingRole, StandingRoleKind, StandingRoleState, standing_role_id};
        let mut snapshot = crate::work_outline::store::fixture_snapshot("fixture-space");
        snapshot.external_references.clear();
        let project = "w-api";
        for item in &mut snapshot.items {
            item.is_project |= item.id == project || (unselected && item.id == "w-ui");
        }
        if unselected {
            snapshot.standing_roles.push(StandingRole {
                id: standing_role_id(StandingRoleKind::Manager, "w-ui"),
                role: StandingRoleKind::Manager,
                scope_item_id: "w-ui".into(),
                chain: Vec::new(),
                current_chat_id: None,
                fallback_chain: Vec::new(),
                state: StandingRoleState::Idle,
                worker_model: None,
                read_only: None,
            });
        }
        let chat = with_chat.then(|| "fixture-design".to_owned());
        if let Some(chat) = &chat {
            let mut assignment = snapshot.assignments.first().cloned().unwrap();
            assignment.chat_id = chat.clone();
            assignment.work_item_id = Some(project.into());
            snapshot.assignments.push(assignment);
            snapshot
                .items
                .iter_mut()
                .find(|i| i.id == project)
                .unwrap()
                .chat_ids
                .push(chat.clone());
        }
        snapshot.standing_roles.push(StandingRole {
            id: standing_role_id(StandingRoleKind::Manager, project),
            role: StandingRoleKind::Manager,
            scope_item_id: project.into(),
            chain: chat.iter().cloned().collect(),
            current_chat_id: chat,
            fallback_chain: Vec::new(),
            state: StandingRoleState::Idle,
            worker_model: None,
            read_only: None,
        });
        self.set_outline_loads(
            [(
                "fixture-space".into(),
                WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
            )]
            .into_iter()
            .collect(),
            cx,
        );
        self.set_mode(OverviewMode::Outline, cx);
        self.sync_outline(cx);
        let selected = if unselected {
            row_key::item("fixture-space", "w-launch")
        } else {
            row_key::manager("fixture-space", project)
        };
        self.outline.update(cx, |outline, cx| {
            if unselected {
                outline.fixture_collapse(&row_key::item("fixture-space", project), cx);
            }
            outline.select(&selected, cx)
        });
    }



}

/// The Chat Toggle Outline Focus returns to, and how it was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutlineChatReturn {
    pub chat_id: String,
    /// `true`: the full chat route; `false`: the Overview's chat panel.
    pub full_route: bool,
}

/// Where the Overview goes when it leaves Work Item Details.
enum DetailsExit {
    Close,
    OpenChat(String),
    OpenChatFromClick(String),
    OpenFullChat(String),
    OpenTracker(String),
    OpenItem { space_id: String, item_id: String },
}

/// The store fixture for the `ix`-th Space: the first verbatim, later ones
/// with every work item and Chat id suffixed `-<ix>` so Spaces never share
/// ids (chat ids are global).
fn fixture_snapshot_for(space_id: &str, ix: usize) -> WorkOutlineSnapshot {
    use crate::work_outline::store::fixture_ids::*;
    let snapshot = crate::work_outline::store::fixture_snapshot(space_id);
    if ix == 0 {
        return snapshot;
    }
    let mut json = serde_json::to_string(&snapshot).unwrap_or_default();
    for id in [
        LAUNCH,
        API,
        API_AUTH,
        UI,
        HOUSEKEEPING,
        RETIRED,
        CHAT_API,
        CHAT_AUTH,
        CHAT_UI,
        CHAT_LOOSE_A,
        CHAT_LOOSE_B,
        CHAT_GHOST,
        CHAT_ORPHAN,
        "w-deleted",
    ] {
        json = json.replace(&format!("\"{id}\""), &format!("\"{id}-{ix}\""));
    }
    serde_json::from_str(&json).unwrap_or(snapshot)
}

#[cfg(test)]
mod logic_tests;

#[cfg(test)]
#[path = "perf/overview_bench.rs"]
mod perf_bench;
