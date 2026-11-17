//! One live headless app: the real shell on `HeadlessAppContext`, the real
//! in-process engine booted behind it on the scratch data dir, and input
//! through the public `Window::dispatch_event` / `dispatch_keystroke`.
//!
//! Locator -> bounds -> input -> settle -> tree. No window, no renderer unless
//! a PNG was asked for at start.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, Entity, HeadlessAppContext, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, point, px, size,
};
use mast_ui::{
    shell::Shell,
    theme::{Appearance, Theme},
    ui_style::{UiLayout, UiStyleId},
};

use crate::tree::{Frame, Rect};

pub const VIEWPORT: (f32, f32) = (1200.0, 800.0);
/// Draws per settle, so a never-quiet UI cannot hang a step.
const MAX_DRAWS: u32 = 24;
/// Time given to engine replies (tokio threads) to land before a step ends.
const QUIET_CHECKS: u32 = 3;
const QUIET_SLEEP: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timing {
    /// Wall time of the whole step, input included.
    pub wall_ms: f64,
    /// Summed wall time of `Window::draw` over the step.
    pub draw_ms: f64,
    pub draws: u32,
    pub nodes: usize,
    /// Not counted: gpui exposes no cheap notify counter.
    pub notifies: Option<u64>,
}

pub struct Session {
    // Field order matters: the shell handle must drop before the context, or
    // gpui reports it as a leaked handle.
    shell: Entity<Shell>,
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    render: bool,
    frame: Frame,
    header: String,
    // Keeps the tokio runtime alive for the engine.
    _runtime: Arc<tokio::runtime::Runtime>,
}

fn init_app(
    cx: &mut gpui::App,
    data: &Path,
    runtime: &tokio::runtime::Handle,
) {
    gpui_tokio::init_from_handle(cx, runtime.clone());
    gpui_base::init(cx);
    // Settle: animations jump to their end state.
    cx.set_reduce_motion(true);
    let mut s = mast_ui::settings::UiSettings::default();
    s.ui_style = UiStyleId::Default;
    s.ui_layout = UiLayout::Minimal;
    s.appearance = mast_ui::appearance::AppearanceMode::Dark;
    s.sidebar_collapsed = false;
    s.live_app_icon = false;
    s.show_menu_bar_item = false;
    mast_ui::settings::init(s.clone(), data, cx);
    let fonts = mast_ui::typography::register_fonts(cx);
    mast_ui::typography::init(
        s.ui_font_family.clone(),
        s.ui_font_size,
        s.terminal_font_family.clone(),
        s.terminal_font_size,
        s.code_font_family.clone(),
        s.code_font_size,
        fonts,
        cx,
    );
    mast_ui::history::init(
        s.git_history_columns,
        s.git_history_column_widths,
        s.git_history_column_order,
        s.git_history_author_display,
        cx,
    );
    mast_ui::composer::init(cx, s.composer_send_behavior);
    mast_ui::terminal::panel::init(cx);
    Theme::install_selection(
        Appearance::Dark,
        "default",
        mast_theme::AccentSelection::ThemeDefault,
        mast_theme::SurfacePreference::default(),
        cx,
    );
}

fn rect_of(bounds: &gpui::Bounds<gpui::Pixels>) -> Rect {
    [
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    ]
}

impl Session {
    /// Open the window on `rows`, draw once, then boot the engine behind it.
    /// Booting before the window exists panics ("Leases must be ended"): the
    /// attach notification reaches the shell's observer mid-`open_window`.
    pub fn open(
        data: &Path,
        header_prefix: String,
        rows: mast_ui::snapshot::WorldRows,
        runtime: Arc<tokio::runtime::Runtime>,
        render: bool,
    ) -> Result<Self, String> {
        let _guard = runtime.enter();
        let native = gpui_platform::current_platform(true);
        let renderer: fn() -> Option<Box<dyn gpui::PlatformHeadlessRenderer>> = if render {
            gpui_platform::current_headless_renderer
        } else {
            || None
        };
        let mut cx = HeadlessAppContext::with_platform(
            native.text_system(),
            Arc::new(mast_ui::icons::Assets),
            renderer,
        );
        cx.set_a11y_enabled(true);
        let handle = runtime.handle().clone();
        cx.update(|cx| init_app(cx, data, &handle));
        let dir = data.to_path_buf();
        let shell = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot = shell.clone();
        let window = cx
            .open_window(size(px(VIEWPORT.0), px(VIEWPORT.1)), move |_, cx| {
                let entity = mast_ui::snapshot::world_shell(dir, rows, cx);
                *slot.borrow_mut() = Some(entity.clone());
                entity
            })
            .map_err(|e| format!("open window: {e:#}"))?;
        let shell = shell.borrow_mut().take().ok_or("shell was not created")?;
        cx.update_window(window.into(), |_, w, _| w.set_window_title("Mast"))
            .map_err(|e| e.to_string())?;
        let mut session = Self {
            cx,
            window: window.into(),
            shell,
            render,
            frame: Frame::default(),
            header: header_prefix,
            _runtime: runtime,
        };
        session.draw();
        session.draw();
        let shell = session.shell.clone();
        session
            .cx
            .update(|cx| mast_ui::snapshot::world_boot_engine(&shell, cx));
        session.wait_for_engine(Duration::from_secs(20))?;
        Ok(session)
    }

    fn engine_ready(&mut self) -> bool {
        self.cx.update(|cx| self.shell.read(cx).fixture_engine_ready(cx))
    }

    /// Pump until the engine is attached and its first frames have settled.
    fn wait_for_engine(&mut self, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        while !self.engine_ready() {
            if started.elapsed() > limit {
                let status = self.status();
                return Err(format!("engine did not attach in time ({status})"));
            }
            self.pump(Duration::from_millis(20));
        }
        self.settle();
        Ok(())
    }

    /// Let real time pass and tell the test scheduler's clock the same:
    /// its timers (retry backoff, debounces) are virtual and only fire when
    /// the clock is advanced.
    fn pump(&mut self, wall: Duration) {
        std::thread::sleep(wall);
        self.cx.advance_clock(wall);
        self.cx.run_until_parked();
    }

    fn draw(&mut self) -> Duration {
        self.cx.run_until_parked();
        let started = Instant::now();
        self.cx
            .update_window(self.window, |_, w, c| {
                // Cached views (the sidebar) reuse their last paint, which
                // carries no a11y nodes or debug bounds; refreshing makes every
                // draw a full one, so the tree is complete. Draw timing is
                // therefore the uncached worst case.
                w.refresh();
                w.draw(c).clear()
            })
            .expect("draw");
        started.elapsed()
    }

    fn capture(&mut self) -> Result<Frame, String> {
        let (tree, bounds) = self
            .cx
            .update_window(self.window, |_, w, _| {
                (w.debug_a11y_tree_json(), w.debug_bounds_all())
            })
            .map_err(|e| e.to_string())?;
        let tree = tree.ok_or("a11y tree is unavailable")?;
        let bounds: Vec<(String, Rect)> = bounds
            .iter()
            .map(|(name, b)| (name.clone(), rect_of(b)))
            .collect();
        Frame::parse(&tree, &bounds)
    }

    /// Draw until the tree hash is the same twice (capped), then give engine
    /// replies a few quiet intervals to land; any change restarts the count.
    pub fn settle(&mut self) -> Timing {
        let started = Instant::now();
        let mut timing = Timing::default();
        let mut last = String::new();
        let mut stable = 0;
        let mut quiet = 0;
        while timing.draws < MAX_DRAWS {
            timing.draw_ms += self.draw().as_secs_f64() * 1000.0;
            timing.draws += 1;
            self.frame = self.capture().unwrap_or_default();
            let hash = self.frame.hash();
            if hash == last {
                stable += 1;
            } else {
                stable = 0;
                quiet = 0;
                last = hash;
            }
            if stable >= 1 {
                if quiet >= QUIET_CHECKS {
                    break;
                }
                quiet += 1;
                self.pump(QUIET_SLEEP);
            }
        }
        timing.nodes = self.frame.nodes.len();
        timing.wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        timing
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Shell state line (connection, chats, selection, splash).
    pub fn status(&mut self) -> String {
        let shell = self.shell.clone();
        self.cx.update(|cx| mast_ui::snapshot::world_status(&shell, cx))
    }

    pub fn header(&self) -> String {
        format!(
            "{} frame={} viewport={}x{} nodes={}",
            self.header,
            self.frame.frame_number,
            self.frame.viewport.0 as u32,
            self.frame.viewport.1 as u32,
            self.frame.nodes.len()
        )
    }

    fn pointer(&mut self, locator: &str, button_clicks: &[MouseButton]) -> Result<(), String> {
        let (_, rect) = self
            .frame
            .resolve_with_bounds(locator)
            .map_err(|e| e.to_string())?;
        let position = point(
            px(rect[0] + rect[2] / 2.0),
            px(rect[1] + rect[3] / 2.0),
        );
        let m = Modifiers::default();
        self.cx
            .update_window(self.window, |_, w, c| {
                w.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers: m,
                    }),
                    c,
                );
                for button in button_clicks {
                    w.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button: *button,
                            position,
                            modifiers: m,
                            click_count: 1,
                            first_mouse: false,
                        }),
                        c,
                    );
                    w.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button: *button,
                            position,
                            modifiers: m,
                            click_count: 1,
                        }),
                        c,
                    );
                }
            })
            .map_err(|e| e.to_string())
    }

    /// Resolve against the current frame, so an ambiguous locator fails here
    /// before any input is sent.
    pub fn click(&mut self, locator: &str) -> Result<Timing, String> {
        let started = Instant::now();
        self.pointer(locator, &[MouseButton::Left])?;
        let mut timing = self.settle();
        timing.wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(timing)
    }

    fn keystroke(&mut self, keystroke: Keystroke) -> Result<(), String> {
        self.cx
            .update_window(self.window, |_, w, c| {
                w.dispatch_keystroke(keystroke, c);
            })
            .map_err(|e| e.to_string())
    }

    /// Click the element to focus it, then type the text a key at a time.
    pub fn type_text(&mut self, locator: &str, text: &str) -> Result<Timing, String> {
        let started = Instant::now();
        self.pointer(locator, &[MouseButton::Left])?;
        self.settle();
        for ch in text.chars() {
            let keystroke = match ch {
                '\n' => Keystroke::parse("enter").map_err(|e| e.to_string())?,
                '\t' => Keystroke::parse("tab").map_err(|e| e.to_string())?,
                c => Keystroke {
                    modifiers: Modifiers::default(),
                    key: c.to_string(),
                    key_char: Some(c.to_string()),
                },
            };
            self.keystroke(keystroke)?;
        }
        let mut timing = self.settle();
        timing.wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(timing)
    }

    pub fn key(&mut self, chord: &str) -> Result<Timing, String> {
        let started = Instant::now();
        let keystroke = Keystroke::parse(chord).map_err(|e| format!("bad chord {chord:?}: {e}"))?;
        self.keystroke(keystroke)?;
        let mut timing = self.settle();
        timing.wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(timing)
    }

    /// Write the current frame as a PNG. Needs `--render` at start.
    pub fn png(&mut self, path: &Path) -> Result<(), String> {
        if !self.render {
            return Err("png needs the renderer: start with `ui-drive start --render`".into());
        }
        self.draw();
        let image = self
            .cx
            .capture_screenshot(self.window)
            .map_err(|e| format!("capture: {e:#}"))?;
        image.save(path).map_err(|e| e.to_string())
    }
}
