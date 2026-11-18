//! Spaces sidebar: the space-filter dropdown (searchable, with "All Spaces"),
//! the filtered Sessions list, and the add-space palette (device
//! tabs + filtered folder browser).
//!
//! A space = a synced (device, folder) pair. Spaces stopped being a
//! navigation spine when tabs went device-local: the dropdown only FILTERS
//! the sidebar's session list (never the tab strip) and hosts space
//! management (add via the palette; rename/delete via row context menus).
//! Child module of `shell` so it renders straight off `Shell`'s private state.

mod add_space_flow;
mod sidebar_disclosure;
mod sidebar_drag_transfer;
mod sidebar_view_menu;
mod sidebar_session_list;
mod spaces_menu;

use super::*;
use crate::pickers::{breadcrumbs, browser_rows, completion_prefix_len, parent_path};
use gpui::{FocusHandle, Window};
use mast_proto::{ChatIndicator, Device, DriveEntry, DriveListing, FolderListing, Space};
use std::collections::HashSet;

/// Reserve a row's exact slot in the list, but lay out its contents only when
/// the slot intersects the current scroll and disclosure clip.
struct VisibleSidebarRow {
    height: f32,
    child: AnyElement,
    /// Runs with the slot's bounds every frame, whether or not it is visible.
    on_slot: Option<Box<dyn FnOnce(gpui::Bounds<gpui::Pixels>, &mut gpui::App)>>,
}

impl gpui::IntoElement for VisibleSidebarRow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for VisibleSidebarRow {
    type RequestLayoutState = AnyElement;
    type PrepaintState = bool;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, AnyElement) {
        let mut slot = div()
            .w_full()
            .h(px(self.height))
            .flex_none()
            .into_any_element();
        (slot.request_layout(window, cx), slot)
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        _: &mut AnyElement,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> bool {
        if let Some(on_slot) = self.on_slot.take() {
            on_slot(bounds, cx);
        }
        // Keep nearby rows alive for decorations that extend beyond the slot
        // and for small scroll changes at either edge.
        let clip = window.content_mask().bounds;
        let margin = px(32.0);
        if bounds.right() + margin <= clip.left()
            || bounds.left() - margin >= clip.right()
            || bounds.bottom() + margin <= clip.top()
            || bounds.top() - margin >= clip.bottom()
        {
            return false;
        }
        self.child.layout_as_root(bounds.size.into(), window, cx);
        self.child.prepaint_at(bounds.origin, window, cx);
        true
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut AnyElement,
        visible: &mut bool,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        if *visible {
            self.child.paint(window, cx);
        }
    }
}

#[cfg(test)]
#[path = "spaces_tests/visible_row_tests.rs"]
mod visible_row_tests;

/// Promote the user's ordered pins above the untouched activity projection.
/// Every unpinned id keeps exactly the relative order supplied by recency.
pub(super) fn project_pinned_first(recency_ids: &[String], pinned_ids: &[String]) -> Vec<String> {
    let active: HashSet<&str> = recency_ids.iter().map(String::as_str).collect();
    let pinned: HashSet<&str> = pinned_ids.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    pinned_ids
        .iter()
        .filter(|id| active.contains(id.as_str()))
        .chain(
            recency_ids
                .iter()
                .filter(|id| !pinned.contains(id.as_str())),
        )
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// Move only the dragged pin. Every other pin, including hidden/archived pins,
/// keeps its relative order; no other position needs to be written.
pub(super) fn reorder_visible_pins(
    pinned_ids: &[String],
    visible_ids: &[String],
    from: usize,
    to: usize,
) -> Vec<String> {
    if from >= visible_ids.len() || to >= visible_ids.len() || from == to {
        return pinned_ids.to_vec();
    }

    let moved = &visible_ids[from];
    let anchor = &visible_ids[to];
    let mut result: Vec<_> = pinned_ids
        .iter()
        .filter(|id| *id != moved)
        .cloned()
        .collect();
    let Some(index) = result.iter().position(|id| id == anchor) else {
        return pinned_ids.to_vec();
    };
    result.insert(index + usize::from(from < to), moved.clone());
    result
}

/// Remove only ids absent from the workspace. Archived sessions remain known
/// so unarchiving restores their local pin and position.
pub(super) fn retain_known_pins(
    pinned_ids: &mut Vec<String>,
    known_chat_ids: &HashSet<String>,
) -> bool {
    let before = pinned_ids.len();
    let mut seen = HashSet::new();
    pinned_ids.retain(|id| known_chat_ids.contains(id) && seen.insert(id.clone()));
    pinned_ids.len() != before
}

/// Convert viewport coordinates to the first pinned row, below its disclosure.
fn pinned_session_pointer_y(pointer_y: f32, viewport_top: f32, scroll_top: f32) -> f32 {
    pointer_y - viewport_top + scroll_top
        - super::SIDEBAR_LIST_PAD_TOP
        - SIDEBAR_DISCLOSURE_HEADER_HEIGHT
        - SIDEBAR_DISCLOSURE_BODY_INSET
}

#[cfg(test)]
pub(super) fn pinned_session_drop_index(rel_y: f32, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let height = count as f32 * super::SIDEBAR_SESSION_SLOT - super::SIDEBAR_LIST_GAP;
    (rel_y >= 0.0 && rel_y <= height)
        .then(|| ((rel_y / super::SIDEBAR_SESSION_SLOT).floor() as usize).min(count - 1))
}

/// Hit testing shares the exact row metrics used by layout, including mixed PR rows.
pub(super) fn row_drop_index(y: f32, heights: &[f32], clamp: bool) -> Option<usize> {
    if heights.is_empty() {
        return None;
    }
    let total =
        heights.iter().sum::<f32>() + heights.len().saturating_sub(1) as f32 * SIDEBAR_LIST_GAP;
    if !clamp && !(0.0..=total).contains(&y) {
        return None;
    }
    let mut bottom = 0.0;
    for (index, height) in heights.iter().enumerate() {
        bottom += height + SIDEBAR_LIST_GAP;
        if y < bottom {
            return Some(index);
        }
    }
    Some(heights.len() - 1)
}

/// Keep a sidebar-wide drag physically bounded to the pinned section. The
/// strict drop helper above still identifies whether the pointer is actually
/// inside the section; this helper supplies the nearest valid pinned slot
/// while the pointer is over regular sessions.
#[cfg(test)]
pub(super) fn pinned_session_clamped_index(rel_y: f32, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(((rel_y.max(0.0) / super::SIDEBAR_SESSION_SLOT).floor() as usize).min(count - 1))
}

/// A drop can change pin membership or pinned order, never activity ordering.
fn sidebar_session_drop_pins(
    saved: &[String],
    visible: &[String],
    chat_id: &str,
    target: SidebarSessionDrop,
) -> Vec<String> {
    let mut next = saved.to_vec();
    match target {
        SidebarSessionDrop::Regular | SidebarSessionDrop::Section(_) => {
            next.retain(|id| id != chat_id)
        }
        SidebarSessionDrop::Pinned(index) => {
            if let Some(from) = visible.iter().position(|id| id == chat_id) {
                return reorder_visible_pins(saved, visible, from, index.min(visible.len() - 1));
            }
            if saved.iter().any(|id| id == chat_id) {
                return next;
            }
            let insertion = visible
                .get(index)
                .and_then(|anchor| next.iter().position(|id| id == anchor))
                .or_else(|| {
                    visible
                        .last()
                        .and_then(|anchor| next.iter().position(|id| id == anchor))
                        .map(|ix| ix + 1)
                })
                .unwrap_or(next.len());
            next.insert(insertion, chat_id.to_owned());
        }
    }
    next
}

/// Preview geometry only: regular ordering is never persisted by a drag.
fn sidebar_gap_offset(row: usize, source: Option<usize>, boundary: usize, height: f32) -> f32 {
    match source {
        Some(source) if row == source => 0.0,
        Some(source) if row < source && row >= boundary => height,
        Some(source) if row > source && row < boundary => -height,
        None if row >= boundary => height,
        _ => 0.0,
    }
}

/// Last laid-out `(slot, top, bottom)` of each session row, in sidebar
/// scroll-content coordinates. Every slot re-records each frame, culled or not;
/// an extent is trusted only while the row keeps its slot, since a re-sort
/// moves it before any measurement does.
pub(super) type SidebarRowExtents =
    std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, (usize, f32, f32)>>>;

/// A row last laid out more than one viewport outside the visible range renders
/// as an empty slot of the same height. Unmeasured rows always render, and
/// every scroll re-renders the sidebar, so a row is filled before it shows.
fn sidebar_row_offscreen(extent: Option<(f32, f32)>, scroll_top: f32, viewport: f32) -> bool {
    let Some((top, bottom)) = extent else {
        return false;
    };
    viewport > 0.0 && (bottom < scroll_top - viewport || top > scroll_top + 2.0 * viewport)
}

pub(super) fn pinned_drag_scroll_delta(
    pointer_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
) -> f32 {
    if viewport_bottom <= viewport_top {
        return 0.0;
    }
    if pointer_y < viewport_top + super::SIDEBAR_DRAG_SCROLL_BAND {
        let penetration = ((viewport_top + super::SIDEBAR_DRAG_SCROLL_BAND - pointer_y)
            / super::SIDEBAR_DRAG_SCROLL_BAND)
            .clamp(0.0, 1.0);
        -super::SIDEBAR_DRAG_SCROLL_MAX * penetration
    } else if pointer_y > viewport_bottom - super::SIDEBAR_DRAG_SCROLL_BAND {
        let penetration = ((pointer_y - (viewport_bottom - super::SIDEBAR_DRAG_SCROLL_BAND))
            / super::SIDEBAR_DRAG_SCROLL_BAND)
            .clamp(0.0, 1.0);
        super::SIDEBAR_DRAG_SCROLL_MAX * penetration
    } else {
        0.0
    }
}

#[cfg(test)]
#[path = "spaces_tests/pinned_session_tests.rs"]
mod pinned_session_tests;

pub(super) fn pinned_drag_scroll_step(
    drag_active: bool,
    loop_generation: u64,
    drag_generation: u64,
    current: f32,
    max: f32,
    delta: f32,
) -> Option<f32> {
    if !drag_active || loop_generation != drag_generation || delta == 0.0 {
        return None;
    }
    let next = (current + delta).clamp(0.0, max.max(0.0));
    (next != current).then_some(next)
}

pub(super) fn pinned_drag_snapshot_is_valid(
    dragged_id: &str,
    snapshot_ids: &[String],
    current_ids: &[String],
) -> bool {
    snapshot_ids.iter().any(|id| id == dragged_id) && snapshot_ids == current_ids
}

struct ActiveChatRow {
    status: ChatIndicator,
    chat: mast_proto::Chat,
    folder: String,
    branch: Option<String>,
    change_request: Option<mast_proto::ChangeRequestSummary>,
    group: Option<(String, String)>,
}

pub(super) fn compare_sidebar_chats(
    sort: SidebarSort,
    left: &mast_proto::Chat,
    right: &mast_proto::Chat,
) -> std::cmp::Ordering {
    let primary = match sort {
        SidebarSort::Created => right.created_at.cmp(&left.created_at),
        SidebarSort::LastUpdated => right
            .last_message_at
            .unwrap_or(right.created_at)
            .cmp(&left.last_message_at.unwrap_or(left.created_at)),
    };
    primary.then_with(|| left.id.cmp(&right.id))
}

/// The space-filter dropdown, `Some` while open. The same searchable-menu
/// recipe as the composer's ref picker: filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card), ranked substring
/// rows, keyboard highlight.
pub(super) struct SpacesMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight — an index into [`Shell::spaces_menu_rows`], or
    /// that list's length when the pinned "New Space…" footer holds it.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

pub(super) struct SidebarViewMenu {
    submenu: Option<usize>,
    submenu_active: Option<usize>,
    submenu_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    submenu_on_left: bool,
    hover_intent: popover::HoverIntent<usize>,
    /// Keyboard cursor. Mouse-opened menus start without one so the persisted
    /// radio/check state is the only selection signal until an arrow key is
    /// pressed.
    active: Option<usize>,
    focus: FocusHandle,
}

struct SidebarViewOptionsTooltip;

impl Render for SidebarViewOptionsTooltip {
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
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child("Sidebar view options")
    }
}

struct SidebarOverviewTooltip;

impl Render for SidebarOverviewTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧O"
        } else {
            "Ctrl+Shift+O"
        };
        div()
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child(format!("Overview  {shortcut}"))
    }
}

#[derive(Clone, Copy)]
enum SidebarViewRow {
    ByProject,
    Compact,
    ShowProjectIcon,
    ShowProjectLabel,
    ByDevice,
    InOneList,
    LastUpdated,
    Created,
    ShowBranch,
    ShowPullRequest,
    ShowHarness,
}

impl SidebarViewRow {
    /// Radio choices dismiss their submenu after selection. Show toggles
    /// stay open for batch changes.
    fn closes_submenu(self) -> bool {
        matches!(
            self,
            Self::ByProject | Self::ByDevice | Self::InOneList | Self::LastUpdated | Self::Created
        )
    }
}

const SIDEBAR_VIEW_GROUPS: [(&str, std::ops::Range<usize>); 3] =
    [("Organize", 0..3), ("Sort", 3..5), ("Show", 5..10)];

const SIDEBAR_VIEW_ROWS: [SidebarViewRow; 10] = [
    SidebarViewRow::ByDevice,
    SidebarViewRow::ByProject,
    SidebarViewRow::InOneList,
    SidebarViewRow::LastUpdated,
    SidebarViewRow::Created,
    SidebarViewRow::ShowBranch,
    SidebarViewRow::ShowPullRequest,
    SidebarViewRow::ShowHarness,
    SidebarViewRow::ShowProjectIcon,
    SidebarViewRow::ShowProjectLabel,
];

// list items stay tightly related at 2px, while section boundaries use 12px
// (well over 2x the intra-list gap). Disclosure content gets a small 4px
// handoff from its header without leaving dead space while collapsed.
const SIDEBAR_SECTION_GAP: f32 = 12.0;
pub(super) const SIDEBAR_DISCLOSURE_HEADER_HEIGHT: f32 = 28.0;
pub(super) const SIDEBAR_DISCLOSURE_BODY_INSET: f32 = 4.0;
const SIDEBAR_DISCLOSURE_SECTION_HEIGHT: f32 =
    SIDEBAR_SECTION_GAP + SIDEBAR_DISCLOSURE_HEADER_HEIGHT;
pub(super) const SIDEBAR_DISCLOSURE_TWEEN_GRACE: std::time::Duration =
    std::time::Duration::from_millis(120);

/// Put this machine's device group first without disturbing the recency-based
/// order of any remote groups. A targeted promotion is more truthful than a
/// full name sort: local context leads, then the user's chosen chat sort wins.
fn promote_local_device_group<T>(
    groups: &mut Vec<(Option<(String, String)>, Vec<T>)>,
    local_device_id: Option<&str>,
) {
    let Some(local_device_id) = local_device_id else {
        return;
    };
    let Some(index) = groups.iter().position(|(group, _)| {
        group
            .as_ref()
            .is_some_and(|(device_id, _)| device_id == local_device_id)
    }) else {
        return;
    };
    if index > 0 {
        let local = groups.remove(index);
        groups.insert(0, local);
    }
}

pub(super) use crate::components::shell_parts::sidebar_separator;

fn sidebar_disclosure_header(theme: &Theme, label: SharedString, chevron: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(SIDEBAR_DISCLOSURE_HEADER_HEIGHT))
        .px(px(Theme::SPACE_SM))
        .cursor_pointer()
        .child(super::sidebar_faded_label(
            "sidebar-disclosure-label".into(),
            false,
            crate::components::shell_parts::sidebar_section_label(theme, label),
        ))
        .child(div().flex_1())
        .child(chevron)
}

/// One activatable row of the open dropdown, in nav order. `AddSpace` names
/// the card's pinned "New Space…" footer, not a list row — keyboard nav
/// maps the list-length index to it.
#[derive(Clone, PartialEq)]
pub(super) enum SpacesMenuRow {
    All,
    Space(String),
    AddSpace,
}

/// New project navigates devices, locations, then folders on a command-palette surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectStep {
    Devices,
    Locations,
    Folders,
}

pub(super) struct AddSpaceFlow {
    step: ProjectStep,
    location: Option<(String, Option<String>)>,
    /// The selected device.
    device: Option<Device>,
    /// Filter input; Enter descends into the highlighted folder. Carries the
    /// tab-completion ghost (the faint suffix ⇥ accepts), and a trailing `/`
    /// on a folder-naming query descends immediately.
    search: Entity<ComposerInput>,
    browser: Loadable<FolderListing>,
    /// The selected device's mounted drives/volumes.
    /// Best-effort: an error just leaves the section at Home only.
    drives: Loadable<Vec<DriveEntry>>,
    /// Requested browser path (`None` = the device's default, i.e. home).
    browser_path: Option<String>,
    /// The device's home (the path a `None` browse resolved to) — breadcrumbs
    /// fold everything up to here into the Home crumb.
    home: Option<String>,
    /// Best-effort git seed for the CURRENT browser path (known when we
    /// descended through an entry whose `is_repo` we saw; the owning device's
    /// SpacesSync re-verifies either way).
    browser_repo: bool,
    /// Keyboard highlight within the current step’s filtered rows.
    active: usize,
    submit_busy: bool,
    error: Option<SharedString>,
    /// Tracked on the card (`track_focus`) — puts the card on the keyboard
    /// dispatch path so ↑↓/⌫/esc reach `add_space_key` while the search input
    /// holds focus (the structure every working picker uses).
    focus: FocusHandle,
    /// Folder-list scroll — keyboard navigation keeps the highlighted row in
    /// view (`scroll_to_item`).
    list_scroll: gpui::ScrollHandle,
    focus_pending: bool,
    load_task: Option<Task<()>>,
    drives_task: Option<Task<()>>,
    submit_task: Option<Task<()>>,
    /// Folder-pick mode (Work Outline execution root / repository): the
    /// same browser on a fixed device, whose submit hands the open folder to
    /// `on_pick` instead of creating a Space.
    pick: Option<FolderPickMode>,
    _search_events: Subscription,
}

/// The pick-mode half of [`AddSpaceFlow`].
pub(super) struct FolderPickMode {
    title: SharedString,
    confirm_label: SharedString,
    /// Execution roots: the home directory itself cannot be confirmed.
    reject_home_directory: bool,
    on_pick: Option<Box<dyn FnOnce(String, &mut App) + 'static>>,
}

impl AddSpaceFlow {
    /// Pick mode refuses the open folder (an execution root at the device's
    /// home directory); the reason to show.
    fn pick_refusal(&self) -> Option<&'static str> {
        let pick = self.pick.as_ref()?;
        let listing = self.browser.ready()?;
        (pick.reject_home_directory
            && crate::work_outline::execution::is_device_home(&listing.path, self.home.as_deref()))
        .then_some(crate::work_outline::execution::HOME_ROOT_REFUSAL)
    }
}

/// Segment-aware "is `path` at or under `base`" (`/media/a` is not under
/// `/media/ab`); a root base covers everything.
fn path_under(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    base.is_empty() || path == base || path.starts_with(&format!("{base}/"))
}

/// The space-row Rename dialog (same shape as [`RenameChatDialog`]).
pub(super) struct RenameSpaceDialog {
    pub space_id: String,
    pub input: Entity<ComposerInput>,
    pub focus_pending: bool,
    pub _events: Subscription,
}

/// Dot color for a chat's display status (tab dots + Sessions rows).
pub(crate) fn status_dot_color(status: ChatIndicator, theme: &Theme) -> gpui::Hsla {
    crate::status_style::dot_color(status.into(), theme)
}

// Handle-based rail host for the spaces dropdown: its list is a plain
// tracked scroller, so the trait's default metrics/press/drag (off the live
// ScrollHandle) apply unchanged.
impl popover::ScrollRailHost for Shell {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.spaces_menu_bar
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.spaces_menu.get().map(|menu| menu.list_scroll.clone())
    }
}

impl Shell {
    // ---- sidebar sections ----

    // ---- space context menu / rename / delete overlays ----

    fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    pub(super) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Space name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    /// Open the per-project agent account picker (Default + managed accounts).
    pub(super) fn open_project_account_picker(
        &mut self,
        space_id: String,
        harness: mast_proto::HarnessId,
        cx: &mut Context<Self>,
    ) {
        self.close_space_menu(cx);
        let Some(device_id) = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.device_id.clone())
        else {
            return;
        };
        let state = self.state.clone();
        let flow = cx.new(|cx| {
            crate::managed_accounts::AccountFlow::new(
                state,
                harness,
                Some(space_id),
                Some(device_id),
                cx,
            )
        });
        self.account_flow_sub = Some(cx.subscribe(&flow, |this: &mut Shell, _, _, cx| {
            this.account_flow = None;
            this.account_flow_sub = None;
            cx.notify();
        }));
        self.account_flow = Some(flow);
        cx.notify();
    }

    pub(super) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(super) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some((space_id, position)) = self.space_menu.get().cloned() {
            let closing = self.space_menu.closing_since();
            let rename_id = space_id.clone();
            let delete_id = space_id.clone();
            let account_labels: Vec<(mast_proto::HarnessId, String)> = {
                let state = self.state.read(cx);
                let snapshot = state
                    .space_row(&space_id)
                    .and_then(|space| state.managed_accounts_for(Some(&space.device_id)));
                [
                    mast_proto::HarnessId::ClaudeCode,
                    mast_proto::HarnessId::Codex,
                ]
                .into_iter()
                .map(|h| {
                    (
                        h,
                        crate::managed_accounts::menu_label(snapshot, &space_id, h),
                    )
                })
                .collect()
            };
            let account_rows: Vec<AnyElement> = account_labels
                .into_iter()
                .map(|(harness, label)| {
                    let pick_id = space_id.clone();
                    popover::menu_row(&theme, false, format!("space-menu-account-{harness:?}"))
                        .id(SharedString::from(format!(
                            "space-menu-account-{harness:?}"
                        )))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_project_account_picker(pick_id.clone(), harness, cx)
                        }))
                        .child(
                            icon(icons::KEY_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from(label))
                        .into_any_element()
                })
                .collect();
            let menu = popover::popover_card(&theme)
                .w(px(230.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_space_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-rename-{space_id}"))
                        .id("space-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_space(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .children(account_rows)
                .child(popover::menu_separator(&theme))
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-delete-{space_id}"))
                        .id("space-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_space_menu(cx);
                            this.delete_space_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Remove…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "space-context-menu",
                position,
                menu,
                closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_space_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_space_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename Space"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(&theme, input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-space-cancel")
                                .id("rename-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_space_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-space-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_space(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-space-dialog", viewport, card));
        }

        if let Some(flow) = self.account_flow.clone() {
            let card = flow.update(cx, |flow, cx| flow.render_card(&theme, window, cx));
            overlays.push(popover::modal("account-flow-dialog", viewport, card));
        }

        if let Some(space_id) = self.delete_space_confirm.clone() {
            let (name, device, count) = {
                let state = self.state.read(cx);
                let space = state.space_row(&space_id);
                (
                    space
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "this Space".into()),
                    space
                        .and_then(|s| state.device_name(&s.device_id))
                        .unwrap_or("its device")
                        .to_string(),
                    state.chats_in_space(&space_id).len(),
                )
            };
            let copy = delete_space_copy(&name, &device, count);
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Remove Space?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-space-cancel")
                                .id("delete-space-cancel")
                                .debug_selector(|| "delete-space-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_space_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Remove")
                                .id("delete-space-confirm")
                                .debug_selector(|| "delete-space-confirm".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_space(space_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-space-dialog", viewport, card));
        }

        overlays
    }
}

/// The Remove project confirmation's body (the sidebar's and the Outline's
/// Space-row menu share it): what goes, that it is permanent, and the
/// non-destructive alternative.
pub(crate) fn delete_space_copy(name: &str, device: &str, sessions: usize) -> String {
    let sessions = if sessions == 1 {
        "its 1 session".to_string()
    } else {
        format!("its {sessions} sessions")
    };
    format!(
        "Removing “{name}” permanently deletes {sessions} on {device}: its chats leave Mast’s \
         index, and its whole Work Outline (work items, instructions, and requests) is deleted. \
         This can’t be undone. To just declutter the Outline, use Hide from Outline."
    )
}

#[cfg(test)]
#[path = "spaces_tests/tests.rs"]
mod tests;

/// Synthetic responses for the isolated native screenshot fixture only.
#[cfg(feature = "project-palette-fixture")]
impl Shell {
    pub fn fixture_project_responses(&mut self, cx: &mut Context<Self>) {
        if mast_proto::env::var_os("MAST_FIXTURE_BACKGROUND").is_some() {
            self.composer
                .read(cx)
                .pickers()
                .clone()
                .update(cx, |pickers, cx| pickers.fixture_model_catalog(cx));
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.device.is_some() && !matches!(flow.drives, Loadable::Ready(_)) {
            flow.drives = Loadable::Ready(
                serde_json::from_value(serde_json::json!([
                    {"name":"Projects", "path":"/projects"},
                    {"name":"System", "path":"/"}
                ]))
                .unwrap(),
            );
            cx.notify();
        }
        if flow.step == ProjectStep::Folders && !matches!(flow.browser, Loadable::Ready(_)) {
            let path = flow
                .browser_path
                .clone()
                .unwrap_or_else(|| "/home/alex".into());
            if flow.browser_path.is_none() {
                flow.home = Some(path.clone());
            }
            let names = match path.as_str() {
                "/home/alex" => vec!["Desktop", "Documents", "Downloads", "Projects"],
                "/projects" | "/home/alex/Projects" => vec!["fieldnotes", "mobile-app", "website"],
                _ => vec!["assets", "docs", "src", "tests"],
            };
            flow.browser = Loadable::Ready(
                serde_json::from_value(serde_json::json!({
                    "path":path,
                    "entries":names.into_iter().map(|name| serde_json::json!({
                        "name":name,"isDir":true,"isRepo":name=="fieldnotes"
                    })).collect::<Vec<_>>()
                }))
                .unwrap(),
            );
            cx.notify();
        }
    }
}

#[cfg(test)]
#[path = "spaces_tests/project_flow_tests.rs"]
mod project_flow_tests;
