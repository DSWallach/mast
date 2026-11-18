//! Spaces sidebar: the space-filter dropdown (searchable, with "All Spaces"),
//! the filtered Sessions list, and the add-space palette (device
//! tabs + filtered folder browser).
//!
//! A space = a synced (device, folder) pair. Spaces stopped being a
//! navigation spine when tabs went device-local: the dropdown only FILTERS
//! the sidebar's session list (never the tab strip) and hosts space
//! management (add via the palette; rename/delete via row context menus).
//! Child module of `shell` so it renders straight off `Shell`'s private state.

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
    pub(super) fn begin_sidebar_disclosure_motion(
        &mut self,
        key: &str,
        resting_height: f32,
        target_height: f32,
    ) {
        let previous = self.sidebar_disclosure_motion.get(key).copied();
        let from = previous
            .filter(|motion| motion.animating())
            .map(SidebarDisclosureMotion::current)
            .unwrap_or(resting_height);
        let epoch = previous.map_or(1, |motion| motion.epoch + 1);
        self.sidebar_disclosure_motion.insert(
            key.to_owned(),
            SidebarDisclosureMotion::new(epoch, from, target_height),
        );
    }

    pub(super) fn render_sidebar_disclosure_body(
        &self,
        key: &str,
        open: bool,
        full_height: f32,
        content: AnyElement,
    ) -> AnyElement {
        let target = if open { full_height } else { 0.0 };
        let frame = div().w_full().flex_none().overflow_hidden().child(content);
        let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            .filter(|motion| motion.animating())
        else {
            return frame.h(px(target)).into_any_element();
        };
        let denominator = full_height.max(1.0);
        frame
            .with_animation(
                SharedString::from(format!("sidebar-disclosure-{key}-{}", tween.epoch)),
                motion::COLLAPSE.animation(),
                move |el, t| {
                    let height = motion::lerp(tween.from, tween.to, t);
                    let reveal = (height / denominator).clamp(0.0, 1.0);
                    el.h(px(height))
                        .opacity(0.35 + 0.65 * reveal)
                        .relative()
                        .top(px(-3.0 * (1.0 - reveal)))
                },
            )
            .into_any_element()
    }

    pub(super) fn sidebar_disclosure_chevron(
        &self,
        key: &str,
        open: bool,
        theme: &Theme,
    ) -> AnyElement {
        let resting_reveal = if open { 1.0 } else { 0.0 };
        let chevron = icon(icons::ALT_ARROW_RIGHT)
            .size(px(12.0))
            .text_color(theme.text_muted.opacity(0.5));
        if let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            .filter(|motion| motion.animating())
        {
            let denominator = tween.from.max(tween.to).max(1.0);
            let from = (tween.from / denominator).clamp(0.0, 1.0);
            let to = (tween.to / denominator).clamp(0.0, 1.0);
            div()
                .flex_none()
                .size(px(12.0))
                .child(chevron.with_animation(
                    SharedString::from(format!("sidebar-chevron-{key}-{}", tween.epoch)),
                    motion::COLLAPSE.animation(),
                    move |el, t| {
                        let reveal = motion::lerp(from, to, t);
                        el.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                            reveal * 0.25,
                        )))
                    },
                ))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .size(px(12.0))
                .child(
                    chevron.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                        resting_reveal * 0.25,
                    ))),
                )
                .into_any_element()
        }
    }
    // ---- sidebar sections ----

    // ---- add-space flow ----

    pub(super) fn open_add_space(&mut self, cx: &mut Context<Self>) {
        self.command_palette = None;
        // "PaletteSearch" context: navigation keys stay unbound so ↑↓/←/→/⏎
        // bubble to the palette frame (`add_space_key`) instead of moving the
        // text caret — Enter and ⌘Enter are both handled there.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search devices…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                // Typing `/` after a query that names a folder descends into
                // it — the query reads as a path segment, so the slash IS the
                // pick (shell-style). Otherwise the slash stays in the query
                // (it matches nothing, which is honest feedback).
                if this.add_space_slash_descend(cx) {
                    return;
                }
                if let Some(flow) = this.add_space.as_mut() {
                    flow.active = 0;
                    flow.list_scroll.set_offset(gpui::Point::default());
                }
                cx.notify();
            }
        });
        self.add_space = Some(AddSpaceFlow {
            step: ProjectStep::Devices,
            location: None,
            device: None,
            search,
            browser: Loadable::Idle,
            drives: Loadable::Idle,
            browser_path: None,
            home: None,
            browser_repo: false,
            active: 0,
            submit_busy: false,
            error: None,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            focus_pending: true,
            load_task: None,
            drives_task: None,
            submit_task: None,
            pick: None,
            _search_events: search_events,
        });
        cx.notify();
    }

    /// The New-project folder browser reused as a folder picker on the
    /// request's device (relay-forwarded when remote), opened at
    /// `start_path`. Its submit hands the OPEN folder to `on_pick`; closing it
    /// any other way picks nothing. The device cannot be changed.
    pub(crate) fn open_folder_pick(
        &mut self,
        request: crate::work_outline::execution::ExecutionFolderPick,
        cx: &mut Context<Self>,
    ) {
        self.open_add_space(cx);
        let device = self
            .state
            .read(cx)
            .devices
            .iter()
            .find(|device| device.id == request.device_id)
            .cloned()
            .or_else(|| {
                serde_json::from_value(serde_json::json!({
                    "id": request.device_id,
                    "name": request.device_id,
                    "platform": "",
                    "lastSeenAt": null,
                }))
                .ok()
            });
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.step = ProjectStep::Folders;
        flow.device = device;
        flow.location = Some(("Home".to_string(), None));
        flow.pick = Some(FolderPickMode {
            title: request.title,
            confirm_label: request.confirm_label,
            reject_home_directory: request.reject_home_directory,
            on_pick: Some(request.on_pick),
        });
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders…", cx);
            input.set_text("", cx);
        });
        self.load_space_folders(request.start_path, cx);
    }

    /// Pick mode: hand the open folder to the requester and close.
    fn submit_folder_pick(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.step != ProjectStep::Folders {
            return;
        }
        let Some(path) = flow.browser.ready().map(|listing| listing.path.clone()) else {
            return;
        };
        if let Some(refusal) = flow.pick_refusal() {
            flow.error = Some(refusal.into());
            cx.notify();
            return;
        }
        let on_pick = flow.pick.as_mut().and_then(|pick| pick.on_pick.take());
        self.add_space = None;
        cx.notify();
        if let Some(on_pick) = on_pick {
            on_pick(path, cx);
        }
    }

    /// Selecting a device advances to its locations.
    fn add_space_pick_device(&mut self, device: Device, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Locations;
        flow.location = None;
        flow.load_task = None;
        flow.drives_task = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        flow.device = Some(device);
        flow.browser = Loadable::Idle;
        flow.drives = Loadable::Idle;
        flow.browser_path = None;
        flow.home = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search locations…", cx);
            input.set_text("", cx);
        });
        self.load_space_drives(cx);
        cx.notify();
    }

    fn add_space_goto_location(
        &mut self,
        name: String,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Folders;
        flow.location = Some((name, path.clone()));
        flow.browser_repo = false;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders…", cx);
            input.set_text("", cx);
        });
        self.load_space_folders(path, cx);
    }

    fn add_space_back_to(&mut self, step: ProjectStep, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        // A folder pick is bound to its Space's device.
        if flow.pick.is_some() && step == ProjectStep::Devices {
            return;
        }
        flow.focus_pending = true;
        flow.step = step;
        flow.load_task = None;
        flow.browser = Loadable::Idle;
        flow.browser_path = None;
        flow.location = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        if step == ProjectStep::Devices {
            flow.drives_task = None;
            flow.device = None;
            flow.drives = Loadable::Idle;
            flow.home = None;
        }
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder(
                if step == ProjectStep::Devices {
                    "Search devices…"
                } else {
                    "Search locations…"
                },
                cx,
            );
            input.set_text("", cx);
        });
        cx.notify();
    }

    fn add_space_devices(&self, cx: &App) -> Vec<Device> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let devices = &self.state.read(cx).devices;
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| devices[ix].clone())
            .collect()
    }

    fn add_space_locations(&self, cx: &App) -> Vec<(String, Option<String>)> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let locations: Vec<_> = std::iter::once(("Home".to_string(), None))
            .chain(
                flow.drives
                    .ready()
                    .into_iter()
                    .flatten()
                    .map(|d| (d.name.clone(), Some(d.path.clone()))),
            )
            .collect();
        let names: Vec<_> = locations.iter().map(|(name, _)| name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| locations[ix].clone())
            .collect()
    }

    /// ListDrives on the flow's device (relay-forwarded when remote).
    /// Failures stay silent — the section just shows Home; the folder
    /// browser's own error row already covers "device didn't respond".
    fn load_space_drives(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        flow.drives = Loadable::Loading;
        flow.drives_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_DRIVES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(flow) = shell.add_space.as_mut() {
                    flow.drives = match result {
                        Ok(value) => match serde_json::from_value::<DriveListing>(value) {
                            Ok(listing) => Loadable::Ready(listing.drives),
                            Err(err) => Loadable::Error(err.to_string()),
                        },
                        Err(err) => Loadable::Error(err.to_string()),
                    };
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The current listing's folder rows filtered by the search query
    /// (prefix matches first — `popover::filter_indices`).
    fn add_space_filtered(&self, cx: &App) -> Vec<mast_proto::FolderEntry> {
        let Some(flow) = self.add_space.as_ref() else {
            return Vec::new();
        };
        if flow.step != ProjectStep::Folders {
            return Vec::new();
        }
        let Some(listing) = flow.browser.ready() else {
            return Vec::new();
        };
        let dirs = browser_rows(listing);
        let query = flow.search.read(cx).text().to_string();
        let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
        popover::filter_indices(&query, &names)
            .into_iter()
            .map(|ix| dirs[ix].clone())
            .collect()
    }

    /// Descend into the highlighted (filtered) folder; clears the query.
    /// A path-shaped query with no matching rows browses the typed path
    /// instead — `/disk2⏎` must work, not sit on "No folders match" (an
    /// absolute query can never match a folder name anyway).
    fn add_space_open_active(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        match flow.step {
            ProjectStep::Devices => {
                if let Some(device) = self.add_space_devices(cx).get(flow.active).cloned() {
                    self.add_space_pick_device(device, cx);
                }
                return;
            }
            ProjectStep::Locations => {
                if let Some((name, path)) = self.add_space_locations(cx).get(flow.active).cloned() {
                    self.add_space_goto_location(name, path, cx);
                }
                return;
            }
            ProjectStep::Folders => {}
        }
        let rows = self.add_space_filtered(cx);
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if rows.is_empty() {
            let text = flow.search.read(cx).text().to_string();
            if (text.starts_with('/') || text.starts_with('~'))
                && let Some(target) = crate::pickers::typed_path_target(&text, flow.home.as_deref())
            {
                self.add_space_descend(target, false, cx);
            }
            return;
        }
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let Some(entry) = rows.get(flow.active) else {
            return;
        };
        let full = crate::pickers::child_path(&listing.path, &entry.name);
        let is_repo = entry.is_repo;
        let search = flow.search.clone();
        if let Some(flow) = self.add_space.as_mut() {
            flow.browser_repo = is_repo;
        }
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// Slash-descend: when the query ends in `/` and the part before it names
    /// a folder of the current listing (exact name — matching casing wins
    /// over a case-colliding sibling — else a unique prefix), descend into it
    /// as though it were picked. Returns whether it fired —
    /// descending clears the query, so the caller must not keep acting on the
    /// old text.
    fn add_space_slash_descend(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .add_space
            .as_ref()
            .is_none_or(|f| f.step != ProjectStep::Folders)
        {
            return false;
        }
        // A typed PATH jump: an absolute (`/disk2/`) or home-relative (`~/x/`)
        // query browses that path directly — mounts at unconventional roots
        // (and anywhere else) are reachable without a Locations row. Same
        // trailing-`/` trigger as the folder-name descend below.
        {
            let Some(flow) = self.add_space.as_ref() else {
                return false;
            };
            let text = flow.search.read(cx).text().to_string();
            if text.ends_with('/') && (text.starts_with('/') || text.starts_with('~')) {
                let target = crate::pickers::typed_path_target(&text, flow.home.as_deref());
                let Some(target) = target else {
                    // Path-shaped but unresolvable (`~/…` before home is
                    // known) — leave the query alone.
                    return false;
                };
                self.add_space_descend(target, false, cx);
                return true;
            }
        }
        let target = {
            let Some(flow) = self.add_space.as_ref() else {
                return false;
            };
            let text = flow.search.read(cx).text().to_string();
            let Some(query) = text.strip_suffix('/') else {
                return false;
            };
            if query.is_empty() || query.contains('/') {
                return false;
            }
            let Some(listing) = flow.browser.ready() else {
                return false;
            };
            let dirs = browser_rows(listing);
            let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
            crate::pickers::segment_target(&names, query).map(|ix| {
                (
                    crate::pickers::child_path(&listing.path, &dirs[ix].name),
                    dirs[ix].is_repo,
                )
            })
        };
        let Some((full, is_repo)) = target else {
            return false;
        };
        self.add_space_descend(full, is_repo, cx);
        true
    }

    /// The tab-completion target: the highlighted row when the query prefixes
    /// its name, else the first prefix match (filtering ranks those first).
    /// `(full name, remaining suffix)`; `None` on an empty query or when the
    /// match is already complete.
    fn add_space_completion(&self, cx: &App) -> Option<(String, String)> {
        let flow = self.add_space.as_ref()?;
        let query = flow.search.read(cx).text().to_string();
        if query.is_empty() {
            return None;
        }
        let rows = self.add_space_filtered(cx);
        let entry = rows
            .get(flow.active)
            .filter(|e| completion_prefix_len(&e.name, &query).is_some())
            .or_else(|| {
                rows.iter()
                    .find(|e| completion_prefix_len(&e.name, &query).is_some())
            })?;
        let len = completion_prefix_len(&entry.name, &query)?;
        if len >= entry.name.len() {
            return None;
        }
        Some((entry.name.clone(), entry.name[len..].to_string()))
    }

    /// ⇥: accept the completion — the query becomes the full folder name
    /// (the ghost the input was previewing). Descending stays on `/`/⏎.
    fn add_space_accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some((name, _)) = self.add_space_completion(cx) else {
            return;
        };
        if let Some(flow) = self.add_space.as_ref() {
            let search = flow.search.clone();
            search.update(cx, |input, cx| input.set_text(name, cx));
        }
    }

    /// Descend into a specific folder row (mouse path); clears the query.
    fn add_space_descend(&mut self, full: String, is_repo: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.browser_repo = is_repo;
        let search = flow.search.clone();
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// ListFolders on the flow's device (relay-forwarded when remote).
    pub(super) fn load_space_folders(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let went_home = path.is_none();
        flow.browser_path = path.clone();
        flow.browser = Loadable::Loading;
        flow.active = 0;
        flow.list_scroll.set_offset(gpui::Point::default());
        let Some(engine) = engine else {
            flow.browser = Loadable::Error("Device is not connected".into());
            cx.notify();
            return;
        };
        flow.load_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            if let Some(p) = &path {
                params.insert("path".into(), serde_json::Value::String(p.clone()));
            }
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_FOLDERS, serde_json::Value::Object(params))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(flow) = shell.add_space.as_mut() {
                    flow.browser = match result {
                        Ok(value) => match serde_json::from_value::<FolderListing>(value) {
                            Ok(listing) => {
                                // A pathless browse resolved home — remember it
                                // so the breadcrumbs can fold it into the
                                // device crumb.
                                if went_home {
                                    flow.home = Some(listing.path.clone());
                                }
                                Loadable::Ready(listing)
                            }
                            Err(err) => Loadable::Error(err.to_string()),
                        },
                        Err(err) => Loadable::Error(err.to_string()),
                    };
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Create the space for the browser's current folder.
    fn submit_add_space(&mut self, cx: &mut Context<Self>) {
        if self
            .add_space
            .as_ref()
            .is_some_and(|flow| flow.pick.is_some())
        {
            self.submit_folder_pick(cx);
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.submit_busy || flow.step != ProjectStep::Folders {
            return;
        }
        let Some(device) = flow.device.clone() else {
            return;
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let path = listing.path.clone();
        let git_detected = flow.browser_repo;
        // Same (device, folder) already has a space → just switch to it. The
        // engine dedupes this case too (a createSpace for a duplicate pair
        // no-ops), so creating would leave the minted id dangling.
        if let Some(existing) = self
            .state
            .read(cx)
            .spaces
            .iter()
            .find(|s| s.device_id == device.id && s.path == path)
            .map(|s| s.id.clone())
        {
            self.add_space = None;
            self.land_in_space(existing, cx);
            return;
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.submit_busy = true;
        flow.error = None;
        let space_id = uuid::Uuid::new_v4().to_string();
        // Optimistic echo: the watch frame carrying the real row replaces it
        // by id (apply_spaces re-sorts; same-id upsert is idempotent).
        let space = Space {
            id: space_id.clone(),
            device_id: device.id.clone(),
            path: path.clone(),
            name: None,
            git_detected,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        self.state.update(cx, |s, cx| {
            if !s.spaces.iter().any(|existing| existing.id == space.id) {
                s.spaces.push(space);
            }
            cx.notify();
        });
        let params = serde_json::json!({
            "op": "createSpace",
            "spaceId": space_id,
            "deviceId": device.id,
            "path": path,
            "gitDetected": git_detected,
        });
        let submit_id = space_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.add_space = None;
                        shell.land_in_space(submit_id.clone(), cx);
                    }
                    Err(err) => {
                        // Roll the optimistic row back; surface the error inline.
                        shell.state.update(cx, |s, cx| {
                            s.spaces.retain(|space| space.id != submit_id);
                            cx.notify();
                        });
                        if let Some(flow) = shell.add_space.as_mut() {
                            flow.submit_busy = false;
                            flow.error = Some(format!("{err}").into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.add_space.as_mut() {
            flow.submit_task = Some(task);
        }
        cx.notify();
    }

    /// Back traverses folders, then locations, then devices.
    fn add_space_go_up(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = &self.add_space else {
            return;
        };
        match flow.step {
            ProjectStep::Devices => (),
            ProjectStep::Locations => self.add_space_back_to(ProjectStep::Devices, cx),
            ProjectStep::Folders => {
                let listing = flow.browser.ready();
                let root = flow
                    .location
                    .as_ref()
                    .and_then(|(_, path)| path.as_deref())
                    .or(flow.home.as_deref());
                let parent = listing
                    .filter(|l| Some(l.path.as_str()) != root)
                    .and_then(|l| parent_path(&l.path));
                if let Some(parent) = parent {
                    self.add_space_descend(parent, false, cx);
                } else {
                    self.add_space_back_to(ProjectStep::Locations, cx);
                }
            }
        }
    }

    /// Palette keys (bubbling from the focused search input) — every legend
    /// maps to a REAL key: ↑↓ (or ctrl-n/p) navigate, →/⏎ open the
    /// highlighted folder, ← up a level, ⇥ completes the query to the
    /// previewed folder name, ⌘⏎ add the OPEN folder, ⌫ (empty query) also
    /// goes up, esc closes. (Typing `/` also descends — see the Edited
    /// subscription.)
    fn add_space_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // ←/→ act on the FOLDERS, not the text cursor — the palette is a
        // navigator first; queries are short and edited with ⌫.
        match event.keystroke.key.as_str() {
            "right" => {
                self.add_space_open_active(cx);
                return;
            }
            "left" => {
                self.add_space_go_up(cx);
                return;
            }
            // Unbound in "PaletteSearch" (like enter), so it bubbles here
            // instead of editing text or moving focus.
            "tab" => {
                self.add_space_accept_completion(cx);
                return;
            }
            _ => {}
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.add_space = None;
                cx.notify();
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = match self.add_space.as_ref().map(|f| f.step) {
                    Some(ProjectStep::Devices) => self.add_space_devices(cx).len(),
                    Some(ProjectStep::Locations) => self.add_space_locations(cx).len(),
                    _ => self.add_space_filtered(cx).len(),
                };
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.add_space.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Keep the highlighted row in view as the cursor walks
                    // past the viewport (user-reported: the list didn't
                    // follow the keyboard).
                    flow.list_scroll.scroll_to_item(flow.active);
                    cx.notify();
                }
            }
            // ⏎ opens the highlighted folder (an alias for →); the space is
            // added with ⌘⏎ — and the chord acts on the folder OPEN in the
            // breadcrumbs, not the highlight. The highlight auto-rests on the
            // first row, so a chord that took it would add arbitrary
            // subfolders; the usual target (a repo root full of subfolders)
            // is only ever "the folder you're standing in".
            popover::MenuKey::Enter => self.add_space_open_active(cx),
            popover::MenuKey::ModEnter => self.submit_add_space(cx),
            popover::MenuKey::Backspace => {
                let empty = self
                    .add_space
                    .as_ref()
                    .is_some_and(|f| f.search.read(cx).is_empty());
                if empty {
                    self.add_space_go_up(cx);
                }
            }
            popover::MenuKey::Other => {}
        }
    }

    /// The same glass, header, row rhythm, scroll gutters and footer as Cmd+K.
    pub(super) fn render_add_space_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let flow = self.add_space.as_mut()?;
        if std::mem::take(&mut flow.focus_pending) {
            window.focus(&flow.search.focus_handle(cx), cx);
        }
        let step = flow.step;
        let search = flow.search.clone();
        let focus = flow.focus.clone();
        let scroll = flow.list_scroll.clone();
        let device = flow.device.clone();
        let listing = flow.browser.ready().cloned();
        let location = flow.location.clone();
        let home = flow.home.clone();
        let load_error = flow.browser.error().map(str::to_string);
        let error = flow.error.clone();
        let flow_error_absent = error.is_none();
        let busy = flow.submit_busy;
        let pick_labels = flow
            .pick
            .as_ref()
            .map(|pick| (pick.title.clone(), pick.confirm_label.clone()));
        let pick_refusal = flow.pick_refusal();
        let active = flow.active;
        let loading = matches!(flow.browser, Loadable::Idle | Loadable::Loading);
        let drives_loading = matches!(flow.drives, Loadable::Loading);
        let ghost = self
            .add_space_completion(cx)
            .map(|(_, suffix)| SharedString::from(suffix));
        search.update(cx, |input, cx| {
            input.set_ghost(ghost, cx);
        });
        let query = search.read(cx).text().to_string();
        let row = |ix: usize| {
            popover::menu_row(&theme, ix == active, format!("project-result-{ix}"))
                .id(("project-result", ix))
                .rounded(px(popover::PALETTE_ITEM_RADIUS))
                .h(px(32.0))
                .flex_none()
        };
        let mut rows = Vec::new();
        match step {
            ProjectStep::Devices => {
                for (ix, device) in self.add_space_devices(cx).into_iter().enumerate() {
                    let glyph = match device.platform.as_str() {
                        "macos" | "darwin" => icons::LAPTOP,
                        "web" => icons::GLOBAL,
                        "ios" | "android" => icons::SMARTPHONE,
                        _ => icons::MONITOR,
                    };
                    let online = self.state.read(cx).device_online(&device.id, Utc::now());
                    let name = device.name.clone();
                    rows.push(
                        row(ix)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_pick_device(device.clone(), cx)
                            }))
                            .child(icon(glyph).size(px(17.0)).text_color(theme.text_muted))
                            .child(popover::search_highlight(name.into(), Some(&query), &theme))
                            .child(div().flex_1())
                            .child(div().size(px(5.0)).rounded_full().bg(if online {
                                theme.success
                            } else {
                                theme.text_faint
                            }))
                            .into_any_element(),
                    );
                }
            }
            ProjectStep::Locations => {
                for (ix, (name, path)) in self.add_space_locations(cx).into_iter().enumerate() {
                    let glyph = if path.is_none() {
                        icons::HOME
                    } else {
                        icons::HARD_DRIVE
                    };
                    let label = name.clone();
                    rows.push(
                        row(ix)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_goto_location(name.clone(), path.clone(), cx)
                            }))
                            .child(icon(glyph).size(px(17.0)).text_color(theme.text_muted))
                            .child(popover::search_highlight(
                                label.into(),
                                Some(&query),
                                &theme,
                            ))
                            .into_any_element(),
                    );
                }
            }
            ProjectStep::Folders => {
                if !loading && load_error.is_none() {
                    for (ix, entry) in self.add_space_filtered(cx).into_iter().enumerate() {
                        let base = listing.as_ref().map(|l| l.path.as_str()).unwrap_or("");
                        let full = crate::pickers::child_path(base, &entry.name);
                        let is_repo = entry.is_repo;
                        rows.push(
                            row(ix)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_space_descend(full.clone(), is_repo, cx)
                                }))
                                .child(
                                    icon(icons::FOLDER)
                                        .size(px(17.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(popover::search_highlight(
                                    entry.name.into(),
                                    Some(&query),
                                    &theme,
                                ))
                                .child(div().flex_1())
                                .when(is_repo, |el| {
                                    el.child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                })
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        if let Some(flow) = self.add_space.as_mut() {
            flow.active = flow.active.min(rows.len().saturating_sub(1));
        }
        let empty = rows.is_empty();
        let mut results = div()
            .id("project-results")
            .max_h(px((f32::from(viewport.height) - 220.0).clamp(100.0, 424.0)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .px(px(popover::CARD_INSET))
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .children(rows);
        if step == ProjectStep::Folders && loading {
            results = results.child(popover::skeleton_rows(
                "project-loading",
                &theme,
                5,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(message) = load_error.filter(|_| step == ProjectStep::Folders) {
            results = results.child(
                popover::error_row(&theme, &message).p(px(14.0)).child(
                    popover::btn_ghost(&theme, "Retry", "project-retry")
                        .id("project-retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.add_space.as_ref().and_then(|f| f.browser_path.clone());
                            this.load_space_folders(path, cx);
                        })),
                ),
            );
        } else if empty {
            results = results.child(div().p(px(24.0)).text_color(theme.text_muted).child(
                match step {
                    ProjectStep::Devices => "No devices found",
                    ProjectStep::Locations => "No locations found",
                    ProjectStep::Folders if query.is_empty() => "No folders here",
                    ProjectStep::Folders => "No folders match",
                },
            ));
        }
        if step == ProjectStep::Locations && drives_loading {
            results = results.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child("Loading locations…"),
            );
        }
        let crumb =
            |id: SharedString, name: SharedString, glyph: Option<&'static str>, current: bool| {
                div()
                    .id(id)
                    .h(px(26.0))
                    .px(px(7.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .text_color(if current {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .when(current, |el| el.bg(theme.element_hover))
                    .hover(|s| s.bg(theme.element_hover).text_color(theme.text))
                    .when_some(glyph, |el, glyph| {
                        el.child(
                            icon(glyph)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(if current {
                                    theme.text
                                } else {
                                    theme.text_muted
                                }),
                        )
                    })
                    .child(div().max_w(px(140.0)).truncate().child(name))
            };
        // Keep each chevron with its destination when a long path wraps.
        let segment = |item: gpui::Stateful<gpui::Div>| {
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(
                    icon(icons::ALT_ARROW_RIGHT)
                        .size(px(12.0))
                        .text_color(theme.text_faint),
                )
                .child(item)
        };
        let mut trail = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.0))
            .child(
                crumb(
                    "project-crumb-root".into(),
                    pick_labels
                        .as_ref()
                        .map_or_else(|| "New Space".into(), |(title, _)| title.clone()),
                    None,
                    step == ProjectStep::Devices,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.add_space_back_to(ProjectStep::Devices, cx)),
                ),
            );
        if let Some(device) = device {
            let glyph = match device.platform.as_str() {
                "macos" | "darwin" => icons::LAPTOP,
                "ios" | "android" => icons::SMARTPHONE,
                _ => icons::MONITOR,
            };
            trail =
                trail.child(segment(
                    crumb(
                        "project-crumb-device".into(),
                        device.name.into(),
                        Some(glyph),
                        step == ProjectStep::Locations,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_space_back_to(ProjectStep::Locations, cx)
                    })),
                ));
        }
        if let Some((name, path)) = location {
            let glyph = if path.is_none() {
                icons::HOME
            } else {
                icons::HARD_DRIVE
            };
            let root = path.clone().or(home);
            let at_root = listing
                .as_ref()
                .is_none_or(|l| root.as_deref() == Some(l.path.as_str()));
            trail = trail.child(segment(
                crumb(
                    "project-crumb-location".into(),
                    name.clone().into(),
                    Some(glyph),
                    at_root,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.add_space_goto_location(name.clone(), path.clone(), cx)
                })),
            ));
            if let Some(listing) = listing.as_ref() {
                for (ix, (name, full)) in breadcrumbs(&listing.path).into_iter().enumerate() {
                    if root.as_deref().is_some_and(|root| path_under(root, &full)) {
                        continue;
                    }
                    trail = trail.child(segment(
                        crumb(
                            format!("project-crumb-folder-{ix}").into(),
                            name.into(),
                            Some(icons::FOLDER),
                            full == listing.path,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_space_descend(full.clone(), false, cx)
                        })),
                    ));
                }
            }
        }
        let crumbs = div()
            .px(px(14.0))
            .py(px(8.0))
            .flex()
            .items_start()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .child(
                div()
                    .id("project-crumb-back")
                    .size(px(26.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.element_hover).text_color(theme.text))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_space = None;
                        this.toggle_command_palette(window, cx);
                    }))
                    .child(
                        icon(icons::ARROW_LEFT)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mt(px(5.0))
                    .flex_none()
                    .bg(theme.border),
            )
            .child(trail);
        let header = div()
            .h(px(58.0))
            .flex_none()
            .px(px(18.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .child(popover::palette_search_icon(&theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(15.0))
                    .child(search),
            )
            .child(popover::key_hint_text(&theme, "esc", ""));
        let footer = div()
            .flex_none()
            .px(px(18.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(18.0))
            .child(popover::key_hint_pair(
                &theme,
                icons::ARROW_UP,
                icons::ARROW_DOWN,
                "Navigate",
            ))
            .child(popover::key_hint_text(&theme, "↵", "Open"))
            .child(popover::key_hint_text(&theme, "esc", "Close"))
            .child(div().flex_1())
            .when(step == ProjectStep::Folders, |el| {
                el.child(
                    popover::btn_ghost(
                        &theme,
                        match &pick_labels {
                            Some((_, confirm)) => confirm.as_ref(),
                            None if busy => "Adding…",
                            None => "Add Space",
                        },
                        "project-add",
                    )
                    .id("project-add")
                    .h(px(22.0))
                    .py(px(0.0))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .when(busy || listing.is_none() || pick_refusal.is_some(), |el| {
                        el.opacity(0.5)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.submit_add_space(cx)))
                    .child(
                        popover::key_cap(&theme)
                            .text_size(crate::typography::ui_rems(11.0))
                            .child(crate::settings::badge_combo("mod-enter")),
                    ),
                )
            });
        let card =
            div()
                .id("add-space-palette")
                .track_focus(&focus)
                .w(px(600.0_f32.min(f32::from(viewport.width) - 32.0)))
                .flex()
                .flex_col()
                .rounded(px(14.0))
                .border_1()
                .border_color(theme.border)
                .when(!theme.is_frost(), |el| el.shadow_lg())
                .bg(popover::surface_bg(&theme))
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    this.add_space_key(event, cx)
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.add_space = None;
                    cx.notify();
                }))
                .child(header)
                .child(crumbs)
                .child(div().min_h_0().py(px(popover::CARD_INSET)).child(results))
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .px(px(18.0))
                            .pb(px(8.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .when_some(pick_refusal.filter(|_| flow_error_absent), |el, refusal| {
                    el.child(
                        div()
                            .px(px(18.0))
                            .pb(px(8.0))
                            .text_color(theme.text_muted)
                            .child(refusal),
                    )
                })
                .child(footer);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .occlude()
                            .w(viewport.width)
                            .h(viewport.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::frost::frosted(14.0, crate::frost::MENU_BLUR, card)),
                    ),
            )
            .priority(2)
            .into_any_element(),
        )
    }

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
