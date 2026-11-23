//! mast-ui — the gpui viewport. Shell, sidebar, conversation, composer, terminal,
//! diff pane.
//!
//! Design: ARCHITECTURE.md §4; animation catalog docs/research/feature-inventory.md
//! §1.12; virtualization/markdown techniques docs/research/mugen-pretext.md.
//!
//! M3a foundation:
//! - [`theme`] — always-dark monochrome theme (oklch-derived neutrals), a gpui Global;
//! - [`motion`] — the zeron animation catalog over gpui `Animation` + cubic-bezier;
//! - [`state`] — `AppState` entity + `EngineHandle` (connect-or-embed engine);
//! - [`settings`] — persisted pane widths/collapse flags;
//! - [`shell`] — sidebar + main panel + right-pane scaffold + gate;
//! - [`loaders`] — zeron pulse loader, gradient spinner, boot splash.

pub(crate) mod admin;
pub mod app_menus;
pub mod appearance;
pub mod appshots;
pub(crate) mod artifacts;
pub mod attachments;
pub mod badges;
pub mod browser;
pub mod build_watch;
pub mod change_requests;
pub mod changes;
pub mod chat_closeout;
mod chat_item;
mod manager_pending;
mod chat_metadata;
pub mod chat_type_view;
mod chat_work_context;
mod comment_ui;
pub mod comments;
pub mod code_ref;
pub mod composer;
pub mod components;
pub mod stories;
#[cfg(test)]
mod component_lint;
#[cfg(test)]
mod first_run_test;
#[cfg(test)]
mod cached_view_reuse_tests;
mod composer_dock;
mod context_usage;
mod diff_targets;
#[cfg(feature = "dev-inspector")]
pub mod dev_inspector;
pub mod edge_fade;
mod error_notice;
pub mod file_icons;
pub mod files;
pub mod freeze_watchdog;
pub mod frost;
pub(crate) mod github;
pub mod history;
pub mod icons;
pub(crate) mod image_media;
pub(crate) mod image_viewer;
pub(crate) mod linear;
pub mod links;
pub mod loaders;
pub mod managed_accounts;
pub mod markdown;
pub mod menu_bar;
pub mod motion;
mod new_thread_background_effects;
mod new_thread_background_image;
mod new_thread_background_mask;
mod notice;
pub mod notify;
pub(crate) mod observe_gate;
mod orchestrator_wakes;
pub mod overview;
pub mod overview_grouping;
pub mod overview_positions;
#[cfg(test)]
mod perf;
pub(crate) mod personal_assistant;
#[cfg(feature = "startup-bench")]
mod startup_measurement;
pub mod pickers;
pub mod popover;
pub mod popout;
pub(crate) mod pr_page;
pub(crate) mod pr_review_launch;
pub(crate) mod branch_review_launch;
pub mod project_actions;
pub mod quota_guard;
mod queue_activity;
pub mod scheduled_tasks;
pub mod queue;
pub mod rail;
mod review_chats;
pub mod running_chats;
pub mod settings;
pub mod shell;
mod board;
mod team;
mod tracker_cleanup;
mod tracker_lifecycle;
mod tracker_tickets;
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOC: perf::CountingAlloc = perf::CountingAlloc;
#[cfg(all(feature = "snapshot", target_os = "macos"))]
pub mod snapshot;
pub mod sound;
pub mod state;
pub mod status_light;
mod status_style;
pub(crate) mod surface_chrome;
pub mod syntax_cache;
pub mod terminal;
mod text_menu;
mod selection_chip;
pub mod theme;
pub mod theme_library;
pub mod transcript;
pub(crate) use mast_proto::message_source;
pub mod typography;
pub mod ui_style;
pub mod style_overrides;
pub(crate) mod work_outline;
pub mod workspace_chip;
mod workspace_links;
#[cfg(target_os = "linux")]
mod x11_window_geometry;

use std::path::PathBuf;

use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

pub use mast_proto::HarnessId;
pub use state::EngineBootConfig;

/// Whether a control whose primary action is click activation may also start
/// a GPUI drag from the same hitbox. GPUI promotes pointer travel above 2 px
/// to a drag. Normal Windows click jitter can cross that threshold, cancel the
/// click, and leave the drag ghost following the pointer instead of activating
/// the control. Drag-first controls (resize handles, scrollbars, queue rows)
/// intentionally do not use this policy.
pub(crate) const fn click_activation_drag_enabled() -> bool {
    !cfg!(target_os = "windows")
}

/// Everything the headed binary passes in (config/env resolution lives in
/// `apps/mast`, not here).
#[derive(Debug, Clone)]
pub struct UiConfig {
    /// Data directory — engine stores + `ui-settings.json`.
    pub data_dir: PathBuf,
    /// Localhost IPC port: connect if an engine daemon is listening, embed if not.
    pub ipc_port: u16,
    /// Edge base URL for the embedded engine.
    pub edge_url: String,
    /// Edge bearer; `None` runs offline.
    pub edge_token: Option<String>,
    /// Workspace org override for explicit dev-mode runs.
    pub org_id: Option<String>,
    /// WorkOS client id; `Some` makes the embedded headed engine require a
    /// production session before opening identity-scoped stores.
    pub workos_client_id: Option<String>,
    /// Harness for doc-command runs until per-chat config lands (M4).
    pub default_harness: HarnessId,
    /// Conversation URL passed by the OS on a cold launch.
    pub initial_url: Option<String>,
}

impl UiConfig {
    fn boot(&self) -> EngineBootConfig {
        EngineBootConfig {
            data_dir: self.data_dir.clone(),
            ipc_port: self.ipc_port,
            edge_url: self.edge_url.clone(),
            edge_token: self.edge_token.clone(),
            org_id: self.org_id.clone(),
            workos_client_id: self.workos_client_id.clone(),
            default_harness: self.default_harness,
        }
    }
}

/// What a dock-icon reopen needs to rebuild the main window after ⌘W closed it
/// (macOS keeps the process alive with just the menu bar, like zed).
struct ReopenState {
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
}

impl gpui::Global for ReopenState {}

/// Whether the main `Shell` window exists. Pop-outs do not count: with only
/// pop-outs left the Dock, banners and menu bar rebuild the main window.
pub(crate) fn has_main_window(cx: &App) -> bool {
    cx.windows().iter().any(|window| window.downcast::<shell::Shell>().is_some())
}

/// The main window, rebuilt around the running engine when ⌘W closed it.
pub(crate) fn main_shell_or_reopen(cx: &mut App) -> Option<gpui::WindowHandle<shell::Shell>> {
    if let Some(handle) = cx.windows().into_iter().find_map(|window| window.downcast::<shell::Shell>()) {
        return Some(handle);
    }
    let reopen = cx.try_global::<ReopenState>()?;
    let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
    Some(open_main_window(state, boot, cx))
}

/// Main-thread freeze watchdog: the main-thread executor answers the watchdog
/// thread's pings (no timer on the main thread); a stall of 2 s or more leaves
/// a `warn` and a `freeze-<timestamp>.txt` stack sample in the logs dir.
/// `MAST_FREEZE_WATCHDOG=0` turns it off.
fn start_freeze_watchdog(data_dir: &std::path::Path, cx: &mut App) {
    if std::env::var("MAST_FREEZE_WATCHDOG").is_ok_and(|v| v == "0") {
        return;
    }
    let heartbeat = freeze_watchdog::Heartbeat::new();
    cx.spawn({
        let heartbeat = heartbeat.clone();
        async move |_| heartbeat.serve().await
    })
    .detach();
    match freeze_watchdog::start(
        freeze_watchdog::Config::default(),
        heartbeat,
        data_dir.join("logs"),
        Box::new(freeze_watchdog::SampleCapturer),
    ) {
        // Runs for the life of the process.
        Ok(handle) => std::mem::forget(handle),
        Err(error) => tracing::warn!(%error, "freeze watchdog did not start"),
    }
}

/// Run the headed app: tokio bridge up, engine bootstrap kicked off (probe →
/// connect-or-embed), 1320×880 window (min 900×600) with [`shell::Shell`] as the
/// root view, boot splash overlaid until the engine reports ready.
pub fn run_app(config: UiConfig) {
    let data_dir = config.boot().data_dir.clone();
    let ui_settings = tracing::info_span!("startup_load_settings")
        .in_scope(|| settings::UiSettings::load(&data_dir));
    // Retain ownership for the whole application lifetime. The bridge's
    // default runtime has only two workers, insufficient for a desktop engine.
    let runtime = tokio::runtime::Runtime::new().expect("desktop Tokio runtime");
    let runtime_handle = runtime.handle().clone();
    let app = gpui_platform::application().with_assets(icons::Assets);
    let (url_tx, mut url_rx) = futures::channel::mpsc::unbounded::<String>();
    let callback_tx = url_tx.clone();
    app.on_open_urls(move |urls| {
        for url in urls {
            let _ = callback_tx.unbounded_send(url);
        }
    });
    if let Some(url) = config.initial_url.clone() {
        let _ = url_tx.unbounded_send(url);
    }
    // Dock-icon click with no window (⌘W closed it): rebuild the main window
    // around the still-running engine — zed does the same via `on_reopen`
    // (crates/zed/src/main.rs `app.on_reopen`).
    app.on_reopen(|cx| {
        if !has_main_window(cx)
            && let Some(reopen) = cx.try_global::<ReopenState>()
        {
            let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
            open_main_window(state, boot, cx);
        }
    });
    app.run(move |cx: &mut App| {
        gpui_tokio::init_from_handle(cx, runtime_handle);
        start_freeze_watchdog(&data_dir, cx);
        gpui_base::init(cx);
        #[cfg(feature = "dev-inspector")]
        dev_inspector::init(cx);
        settings::init(ui_settings.clone(), data_dir.clone(), cx);
        let font_availability = tracing::info_span!("startup_register_fonts")
            .in_scope(|| typography::register_fonts(cx));
        // Typography first: theme installation reads the effective family, so
        // the first frame has the final font and palette without a flash.
        ui_style::init_with_layout(ui_settings.ui_style, ui_settings.ui_layout, cx);
        style_overrides::init(data_dir.clone(), cx);
        typography::init(
            ui_settings.ui_font_family.clone(),
            ui_settings.ui_font_size,
            ui_settings.terminal_font_family.clone(),
            ui_settings.terminal_font_size,
            ui_settings.code_font_family.clone(),
            ui_settings.code_font_size,
            font_availability,
            cx,
        );
        tracing::info_span!("startup_load_themes")
            .in_scope(|| theme_library::init(data_dir.clone(), cx));
        appearance::init(
            ui_settings.appearance,
            ui_settings.theme_selection,
            ui_settings.accent,
            ui_settings.surface,
            cx,
        );
        tracing::info_span!("startup_follow_system_appearance")
            .in_scope(|| appearance::follow_system(cx));
        history::init(
            ui_settings.git_history_columns,
            ui_settings.git_history_column_widths,
            ui_settings.git_history_column_order,
            ui_settings.git_history_author_display,
            cx,
        );
        composer::init(cx, ui_settings.composer_send_behavior);
        appshots::set_enabled(ui_settings.appshots_enabled);
        terminal::panel::init(cx);
        app_menus::init(cx);
        cx.register_url_scheme("zeron").detach();

        let state = cx.new(|_| state::AppState::new());
        let url_state = state.clone();
        cx.spawn(async move |cx| {
            while let Some(url) = url_rx.next().await {
                url_state.update(cx, |state, cx| state.open_deep_link(&url, cx));
            }
        })
        .detach();
        // Banner clicks land on the notified chat. The AppKit delegate fires
        // mid-event, so hop through a channel rather than updating inline.
        let (click_tx, mut click_rx) = futures::channel::mpsc::unbounded::<String>();
        notify::on_click(move |chat_id| {
            let _ = click_tx.unbounded_send(chat_id);
        });
        let click_state = state.clone();
        cx.spawn(async move |cx| {
            while let Some(chat_id) = click_rx.next().await {
                cx.update(|cx| {
                    if let Some((space_id, request_id, decision)) = notify::parse_decision_target(&chat_id) {
                        #[cfg(any(target_os = "macos", target_os = "linux"))]
                        handle_menu_bar_event(menu_bar::MenuEvent::Action(
                            menu_bar::MenuAction::DecideRequest { space_id, request_id, decision },
                        ), &click_state, cx);
                        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                        let _ = (space_id, request_id, decision);
                    } else if let Some((space_id, request_id)) = notify::parse_request_target(&chat_id) {
                        open_notified_chat(None, &click_state, cx);
                        #[cfg(any(target_os = "macos", target_os = "linux"))]
                        handle_menu_bar_event(menu_bar::MenuEvent::Action(
                            menu_bar::MenuAction::OpenRequest { space_id, request_id },
                        ), &click_state, cx);
                        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                        let _ = (space_id, request_id);
                    } else {
                        open_notified_chat(Some(chat_id), &click_state, cx);
                    }
                });
            }
        })
        .detach();
        state::AppState::bootstrap(state.clone(), config.boot(), cx);

        // Graceful teardown: an in-process engine drains live runs and flushes
        // doc snapshots before the process exits (remote engines outlive us).
        let quit_state = state.clone();
        cx.on_app_quit(move |cx| {
            if let Some(window) = cx
                .windows()
                .into_iter()
                .find_map(|window| window.downcast::<shell::Shell>())
            {
                let _ = window.update(cx, |_, window, cx| save_main_window_geometry(window, cx));
            }
            settings::flush(cx);
            let shutdown =
                quit_state.read(cx).engine().cloned().map(|handle| {
                    gpui_tokio::Tokio::spawn(cx, async move { handle.shutdown().await })
                });
            async move {
                if let Some(task) = shutdown {
                    let _ = task.await;
                }
            }
        })
        .detach();

        cx.set_global(ReopenState {
            state: state.clone(),
            boot: config.boot(),
        });
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(mut events) = menu_bar::take_events() {
            let menu_state = state.clone();
            cx.spawn(async move |cx| {
                while let Some(event) = events.next().await {
                    cx.update(|cx| handle_menu_bar_event(event, &menu_state, cx));
                }
            })
            .detach();
        }
        tracing::info_span!("startup_open_window")
            .in_scope(|| open_main_window(state, config.boot(), cx));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        start_appshot_service(config.boot().data_dir, cx);
        // Native menu bar — macOS gets the standard app menu (About/Services/
        // Hide/Quit ⌘Q), Edit clipboard verbs routed to the focused input, and
        // a Window menu (⌘M/⌘W). Without this, `NSApp.mainMenu` stays nil: no
        // Cmd+Q, and nothing for the system menu bar to show. Set after
        // `open_main_window` because `Shell::new` ran `apply_keymap`
        // synchronously, so `set_menus` reads the final bindings for the ⌘-key
        // equivalents (gpui snapshots the keymap at set time).
        cx.set_menus(app_menus::app_menus());
        cx.activate(true);
    });
}

/// A clicked banner: bring Mast forward on that chat through the sidebar's
/// own path (chat route + composer focus), reopening the main window first if
/// ⌘W closed it.
fn open_notified_chat(
    chat_id: Option<String>,
    state: &gpui::Entity<state::AppState>,
    cx: &mut App,
) {
    cx.activate(true);
    if !has_main_window(cx)
        && let Some(reopen) = cx.try_global::<ReopenState>()
    {
        let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
        open_main_window(state, boot, cx);
    }
    let shell = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<shell::Shell>());
    match shell {
        Some(shell) => {
            let _ = shell.update(cx, |shell, window, cx| {
                window.activate_window();
                if let Some(chat_id) = chat_id {
                    shell.open_chat(chat_id, cx);
                }
            });
        }
        None => state.update(cx, |state, cx| state.select_chat(chat_id, cx)),
    }
}

/// The menu bar item's clicks: bring Mast forward (reopening the window) for
/// chats and "Open Mast", and hand the rest to the Shell.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn handle_menu_bar_event(
    event: menu_bar::MenuEvent,
    state: &gpui::Entity<state::AppState>,
    cx: &mut App,
) {
    use menu_bar::{MenuAction, MenuEvent};
    match event {
        MenuEvent::Action(MenuAction::OpenChat(chat_id)) => {
            open_notified_chat(Some(chat_id), state, cx)
        }
        event @ MenuEvent::Action(MenuAction::OpenRequest { .. } | MenuAction::DecideRequest { .. } | MenuAction::DismissAllRequests(_)) => {
            open_notified_chat(None, state, cx);
            let shell = cx.windows().into_iter().find_map(|window| window.downcast::<shell::Shell>());
            if let Some(shell) = shell {
                let _ = shell.update(cx, |shell, _, cx| shell.handle_menu_bar_event(event, cx));
            }
        }
        MenuEvent::Action(MenuAction::OpenMast) => open_notified_chat(None, state, cx),
        MenuEvent::Action(MenuAction::Quit) => app_menus::request_quit(cx),
        event => {
            let shell = cx
                .windows()
                .into_iter()
                .find_map(|window| window.downcast::<shell::Shell>());
            if let Some(shell) = shell {
                let _ = shell.update(cx, |shell, _, cx| shell.handle_menu_bar_event(event, cx));
            }
        }
    }
}

fn main_window_display_uuid(display: &dyn gpui::PlatformDisplay) -> Option<uuid::Uuid> {
    // GPUI's X11 backend returns a nil UUID for every X screen.
    display.uuid().ok().filter(|uuid| !uuid.is_nil())
}

fn restored_main_window_geometry(cx: &App) -> (settings::WindowGeometry, Option<gpui::DisplayId>) {
    let mut fallback = settings::WindowGeometry::from_bounds(Bounds::centered(
        None,
        size(px(1320.), px(880.)),
        cx,
    ));
    let displays = cx.displays();
    let primary = cx
        .primary_display()
        .and_then(|primary| {
            displays
                .iter()
                .position(|display| display.id() == primary.id())
        })
        .unwrap_or(0);
    let geometries: Vec<_> = displays
        .iter()
        .map(|display| {
            let mut geometry = settings::WindowGeometry::from_bounds(display.visible_bounds());
            geometry.display_uuid = main_window_display_uuid(display.as_ref());
            geometry
        })
        .collect();
    let saved = settings::current_ref(cx)
        .and_then(|settings| settings.window_geometry)
        .filter(|geometry| geometry.is_valid());
    fallback.maximized = saved.is_some_and(|geometry| geometry.maximized);
    if let Some((index, geometry)) = saved.unwrap_or(fallback).restore(&geometries, primary) {
        return (geometry, Some(displays[index].id()));
    }
    // GPUI may open the first Wayland window before wl_output.done. Keep its
    // requested normal size and let the compositor choose the placement.
    if is_wayland(cx) && let Some(saved) = saved {
        fallback.width = saved.width.clamp(900., 8192.);
        fallback.height = saved.height.clamp(600., 8192.);
    }
    (fallback, None)
}

#[cfg(target_os = "macos")]
fn is_native_window_zoomed(window: &gpui::Window) -> bool {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return false;
    };
    // GPUI owns this live NSView for the lifetime of the borrowed handle.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    view.window().is_some_and(|window| window.isZoomed())
}

fn captured_main_window_geometry(
    previous: settings::WindowGeometry,
    state: WindowBounds,
    display_uuid: Option<uuid::Uuid>,
    compositor_owns_position: bool,
) -> Option<settings::WindowGeometry> {
    let geometry = match state {
        WindowBounds::Fullscreen(_) => return None,
        // X11 supplies live maximized bounds. Keep the last normal rectangle.
        WindowBounds::Maximized(_) => settings::WindowGeometry {
            maximized: true,
            ..previous
        },
        WindowBounds::Windowed(bounds) => {
            let mut geometry = settings::WindowGeometry::from_bounds(bounds);
            if compositor_owns_position {
                // Wayland's GPUI origin echoes the startup request. Keep any
                // desktop position previously saved under X11.
                geometry.x = previous.x;
                geometry.y = previous.y;
                geometry.display_uuid = previous.display_uuid;
            } else {
                geometry.display_uuid = display_uuid;
            }
            geometry
        }
    };
    geometry.is_valid().then_some(geometry)
}

fn is_wayland(cx: &App) -> bool {
    cfg!(target_os = "linux") && cx.compositor_name() == "Wayland"
}

fn save_main_window_geometry(window: &gpui::Window, cx: &mut App) {
    let previous = settings::current_ref(cx).and_then(|settings| settings.window_geometry);
    let state = if window.is_fullscreen() {
        WindowBounds::Fullscreen(window.bounds())
    } else {
        window.window_bounds()
    };
    #[cfg(target_os = "linux")]
    let state = cx
        .try_global::<x11_window_geometry::X11WindowGeometry>()
        .and_then(|x11| x11.capture(window).ok())
        .unwrap_or(state);
    // GPUI classifies native AppKit zoom as windowed.
    #[cfg(target_os = "macos")]
    let state = match state {
        WindowBounds::Windowed(bounds) if is_native_window_zoomed(window) => {
            WindowBounds::Maximized(bounds)
        }
        state => state,
    };
    let compositor_owns_position = is_wayland(cx);
    if matches!(state, WindowBounds::Fullscreen(_)) {
        return;
    }
    // X11 reports a nil UUID, Wayland owns placement, and native Linux close
    // callbacks can run while GPUI already holds a mutable display-list borrow.
    let display_uuid = if cfg!(target_os = "linux") {
        None
    } else {
        window
            .display(cx)
            .and_then(|display| main_window_display_uuid(display.as_ref()))
    };
    let baseline = previous.unwrap_or_else(|| {
        let bounds = match state {
            // Wayland supplies its cached normal bounds when maximized.
            WindowBounds::Maximized(bounds) if compositor_owns_position => {
                Bounds::new(Default::default(), bounds.size)
            }
            _ => Bounds::new(Default::default(), size(px(1320.), px(880.))),
        };
        settings::WindowGeometry::from_bounds(bounds)
    });
    if let Some(geometry) =
        captured_main_window_geometry(baseline, state, display_uuid, compositor_owns_position)
        && previous != Some(geometry)
    {
        settings::update(settings::SavePolicy::Debounced, cx, |settings| {
            settings.window_geometry = Some(geometry);
        });
    }
}

fn persist_main_window_geometry(window: &gpui::Window, cx: &mut App) {
    save_main_window_geometry(window, cx);
    settings::flush(cx);
}

fn close_main_window(window: &mut gpui::Window, cx: &mut App) {
    persist_main_window_geometry(window, cx);
    window.remove_window();
}

fn observe_main_window_geometry<T: 'static>(window: &mut gpui::Window, cx: &gpui::Context<T>) {
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    cx.observe_window_bounds(window, move |_, window, _| {
        // On X11 WM state and configure events can arrive separately. Sample
        // after the event batch, when both bounds and maximized state are updated.
        if !pending.replace(true) {
            let pending = pending.clone();
            window.on_next_frame(move |window, cx| {
                pending.set(false);
                save_main_window_geometry(window, cx);
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod window_geometry_tests {
    use super::*;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(gpui::point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn window_geometry_maximize_and_fullscreen_keep_normal_bounds() {
        let normal = settings::WindowGeometry {
            display_uuid: Some(uuid::Uuid::from_u128(1)),
            ..settings::WindowGeometry::from_bounds(bounds(100., 50., 1100., 700.))
        };
        let other_display = Some(uuid::Uuid::from_u128(2));
        let desktop = bounds(0., 0., 1920., 1080.);
        let maximized = captured_main_window_geometry(
            normal,
            WindowBounds::Maximized(desktop),
            other_display,
            false,
        )
        .unwrap();
        assert_eq!(
            maximized,
            settings::WindowGeometry {
                maximized: true,
                ..normal
            }
        );
        assert_eq!(
            captured_main_window_geometry(maximized, WindowBounds::Fullscreen(desktop), None, false),
            None
        );
        let resized = bounds(150., 75., 1200., 800.);
        let restored = captured_main_window_geometry(
            maximized,
            WindowBounds::Windowed(resized),
            other_display,
            false,
        )
        .unwrap();
        assert!(!restored.maximized);
        assert_eq!(restored.bounds(), resized);
        assert_eq!(restored.display_uuid, other_display);
    }

    #[test]
    fn window_geometry_wayland_records_size_and_preserves_desktop_position() {
        let before = settings::WindowGeometry {
            maximized: true,
            display_uuid: Some(uuid::Uuid::from_u128(1)),
            ..settings::WindowGeometry::from_bounds(bounds(-1200., 80., 1100., 700.))
        };
        let resized = WindowBounds::Windowed(bounds(0., 0., 1250., 850.));
        let geometry =
            captured_main_window_geometry(before, resized, Some(uuid::Uuid::from_u128(2)), true)
                .unwrap();
        assert_eq!(
            geometry,
            settings::WindowGeometry {
                maximized: false,
                width: 1250.,
                height: 850.,
                ..before
            }
        );
    }
}

fn open_main_window(
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    cx: &mut App,
) -> gpui::WindowHandle<shell::Shell> {
    let (geometry, display_id) = restored_main_window_geometry(cx);
    let bounds = geometry.bounds();
    #[cfg(target_os = "linux")]
    let restore_x11 = x11_window_geometry::X11WindowGeometry::init(cx)
        && settings::current_ref(cx).is_some_and(|settings| settings.window_geometry.is_some());
    #[cfg(not(target_os = "linux"))]
    let restore_x11 = false;
    let window_bounds = if geometry.maximized && !restore_x11 {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                display_id,
                window_min_size: Some(size(
                    bounds.size.width.min(px(900.)),
                    bounds.size.height.min(px(600.)),
                )),
                // `kind` is deliberately left at its default `WindowKind::Normal`
                // (gpui platform.rs WindowOptions::default), which on macOS maps
                // to `NSNormalWindowLevel` (gpui_macos window.rs) — same as zed's
                // main window. Nothing here raises the window level or touches
                // presentation options; the "menu bar never appears" symptom came
                // from the missing `set_menus` call (nil `NSApp.mainMenu`), not
                // from window kind/level, and `appears_transparent` only affects
                // the titlebar, not the menu bar.
                // macOS: frameless-inset chrome like the original Electron app
                // (`titleBarStyle: "hiddenInset"`, traffic lights at 14,15 —
                // feature-inventory §1.1). The strip is custom-drawn. Windows
                // still needs a native title for taskbar previews and Alt+Tab. On
                // Linux/Windows `appears_transparent` hides the system titlebar
                // for our custom-drawn chrome; harmless where unsupported.
                titlebar: Some(TitlebarOptions {
                    title: cfg!(target_os = "windows").then(|| "Mast".into()),
                    appears_transparent: true,
                    // Native lights are 14px tall: top 14 → center 21, matching
                    // the 38px titlebar row with 4px top-only content padding.
                    traffic_light_position: Some(gpui::point(px(14.), px(14.))),
                }),
                // Our own titlebar strip drags the window (WindowControlArea::
                // Drag + start_window_move) — mark the content view app-owned
                // so AppKit neither dead-zones the strip nor delays clicks.
                app_owns_titlebar_drag: true,
                // Linux: request client-side decorations — zeron draws its own
                // unified titlebar and (under CSD) its own caption buttons
                // (shell.rs `render_linux_caption_controls`). Leaving this unset
                // requests SERVER decorations, which stacked a compositor
                // titlebar on top of the app's chrome under sway/KDE, while
                // compositors without SSD support (GNOME) went client-side
                // anyway — frameless, and before the shell drew caption buttons,
                // with no window controls at all. The compositor can still
                // override via xdg-decoration negotiation; the shell re-resolves
                // what to draw every frame.
                window_decorations: cfg!(target_os = "linux")
                    .then_some(gpui::WindowDecorations::Client),
                // Frosted shell (macOS): blur the desktop behind the window; the
                // shell paints its frost surface translucent so the sidebar reads
                // as glass (shell.rs root). Elsewhere blur support is compositor
                // roulette — stay opaque.
                // One source of truth with the re-apply loop in `appearance::apply`
                // — if these two ever disagree, vibrancy dies on the first theme
                // change and never comes back.
                window_background: theme::Theme::of(cx).window_background_appearance(),
                app_id: Some("zeron".into()),
                ..Default::default()
            },
            move |window, cx| {
                window.set_rem_size(px(typography::effective_rem_px(cx)));
                // React to the user flipping macOS between light and dark. Detached:
                // the subscription lives as long as the window does, and the window
                // owns nothing that would drop it early.
                appearance::observe_window(window, cx).detach();
                let shell = cx.new(|cx| {
                    observe_main_window_geometry(window, cx);
                    shell::Shell::new(state, boot, cx)
                });
                if restore_x11 {
                    #[cfg(target_os = "linux")]
                    window.on_next_frame(move |window, cx| {
                        let x11 = cx.global::<x11_window_geometry::X11WindowGeometry>();
                        if let Err(error) = x11.restore(window, geometry) {
                            tracing::warn!(%error, "could not request X11 window restoration");
                            if geometry.maximized && !window.is_maximized() {
                                window.zoom_window();
                            }
                        }
                    });
                } else if !geometry.maximized {
                    window.on_next_frame(|window, cx| save_main_window_geometry(window, cx));
                }
                let weak_shell = shell.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let should_close = weak_shell
                        .update(cx, |shell, cx| shell.prepare_window_close(cx))
                        .unwrap_or(true);
                    if should_close {
                        persist_main_window_geometry(window, cx);
                    }
                    should_close
                });
                shell
            },
        )
        .expect("failed to open window");
    // Belt and braces: assert the blur once the window actually exists. The
    // `WindowOptions` value is applied during creation, before the view is
    // attached; re-pushing it here means a window is never left opaque.
    appearance::reapply_window_background(cx);
    handle
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn start_appshot_service(activation_dir: std::path::PathBuf, cx: &mut App) {
    let mut shortcuts = appshots::start_global_shortcut(activation_dir);
    cx.spawn(async move |cx| {
        while shortcuts.next().await.is_some() {
            if !appshots::capture_allowed() {
                continue;
            }
            let Some(capture) = cx.update(start_appshot_capture) else {
                continue;
            };
            let capture = capture.await;
            // Coalesce presses made while capture was in flight. Delivery
            // focuses Mast; replaying old activations would capture the wrong
            // app or show a misleading self-capture error after success.
            while matches!(shortcuts.next().now_or_never(), Some(Some(()))) {}
            cx.update(|cx| deliver_appshot(capture, cx));
        }
    })
    .detach();
}

/// Check viewer focus on the UI thread before any native capture or portal
/// request. Portals do not identify the source window, so their backends cannot
/// reject Mast after the picker or capture has already started.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn start_appshot_capture(
    cx: &mut App,
) -> Option<gpui::Task<Result<appshots::CapturedAppshot, appshots::CaptureError>>> {
    if cx.active_window().is_some() {
        return None;
    }
    Some(
        cx.background_executor()
            .spawn(async { appshots::capture_active_window().await }),
    )
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod appshot_activation_tests {
    use super::*;

    struct ViewerWindow;

    impl gpui::Render for ViewerWindow {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    #[gpui::test]
    fn appshot_capture_skips_any_focused_viewer_window(cx: &mut gpui::TestAppContext) {
        // The guard must cover every Mast window, not only a Shell/chat root.
        for _ in 0..2 {
            let window = cx.add_window(|_, _| ViewerWindow);
            window
                .update(cx, |_, window, _| window.activate_window())
                .unwrap();
            cx.run_until_parked();
            cx.update(|cx| {
                assert!(cx.active_window().is_some());
                assert!(start_appshot_capture(cx).is_none());
            });
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn deliver_appshot(
    result: Result<appshots::CapturedAppshot, appshots::CaptureError>,
    cx: &mut App,
) {
    use std::collections::VecDeque;
    use std::sync::{Mutex, OnceLock};

    fn pending() -> &'static Mutex<VecDeque<appshots::CapturedAppshot>> {
        static PENDING: OnceLock<Mutex<VecDeque<appshots::CapturedAppshot>>> = OnceLock::new();
        PENDING.get_or_init(|| Mutex::new(VecDeque::new()))
    }

    if matches!(
        result,
        Err(appshots::CaptureError::Cancelled | appshots::CaptureError::SelfCapture)
    ) {
        return;
    }
    let mut captures = pending()
        .lock()
        .map(|mut queue| queue.drain(..).collect::<VecDeque<_>>())
        .unwrap_or_default();
    let error = match result {
        Ok(appshot) => {
            captures.push_back(appshot);
            None
        }
        Err(error) => Some(error),
    };
    let handle = cx
        .window_stack()
        .unwrap_or_else(|| cx.windows())
        .into_iter()
        .find_map(|handle| handle.downcast::<shell::Shell>())
        .or_else(|| {
            let reopen = cx.try_global::<ReopenState>()?;
            Some(open_main_window(
                reopen.state.clone(),
                reopen.boot.clone(),
                cx,
            ))
        });
    let Some(handle) = handle else {
        if !captures.is_empty() {
            let count = captures.len();
            if let Ok(mut queue) = pending().lock() {
                queue.extend(captures);
            }
            tracing::warn!(
                count,
                "Appshot captured with no Mast window; preserving it for the next delivery"
            );
        }
        return;
    };
    let captured = !captures.is_empty();
    cx.activate(true);
    let _ = handle.update(cx, |shell, window, cx| {
        window.activate_window();
        for appshot in captures {
            shell.receive_appshot(appshot, window, cx);
        }
        if let Some(error) = error {
            shell.show_appshot_error(error.to_string(), window, cx);
        }
    });
    if captured {
        appshots::foreground_after_capture();
    }
}

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
