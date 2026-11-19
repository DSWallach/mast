//! X11 placement and state for the Mast main window.
use anyhow::{Context as _, Result};
use gpui::{Bounds, Global, Window, WindowBounds, point, px, size};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Gravity, Window as Xid,
};
use x11rb::rust_connection::RustConnection;

use crate::settings::WindowGeometry;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
    }
}

pub(crate) struct X11WindowGeometry {
    connection: RustConnection,
    atoms: Atoms,
}

impl Global for X11WindowGeometry {}

impl X11WindowGeometry {
    pub(crate) fn init(cx: &mut gpui::App) -> bool {
        if cx.compositor_name() != "X11" {
            return false;
        }
        if cx.try_global::<Self>().is_some() {
            return true;
        }
        match Self::connect() {
            Ok(x11) => {
                cx.set_global(x11);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "could not connect to X11 for window restoration");
                false
            }
        }
    }

    fn connect() -> Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let atoms = Atoms::new(&connection)?.reply()?;
        Ok(Self { connection, atoms })
    }

    fn window_id(window: &Window) -> Result<Xid> {
        let handle = HasWindowHandle::window_handle(window)?;
        match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
            _ => anyhow::bail!("expected an X11 window handle"),
        }
    }

    pub(crate) fn capture(&self, window: &Window) -> Result<WindowBounds> {
        let id = Self::window_id(window)?;
        let state = self.connection.get_property(
            false,
            id,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            0,
            u32::MAX,
        )?;
        let geometry = self.connection.get_geometry(id)?.reply()?;
        let state = state.reply()?;
        let has_state = |atom| {
            state
                .value32()
                .is_some_and(|mut atoms| atoms.any(|a| a == atom))
        };
        if has_state(self.atoms._NET_WM_STATE_FULLSCREEN) {
            return Ok(WindowBounds::Fullscreen(window.bounds()));
        }
        if has_state(self.atoms._NET_WM_STATE_MAXIMIZED_HORZ)
            && has_state(self.atoms._NET_WM_STATE_MAXIMIZED_VERT)
        {
            return Ok(WindowBounds::Maximized(window.bounds()));
        }

        // ConfigureNotify can report coordinates relative to the reparenting
        // frame, or GPUI can keep the prior origin during a combined move/resize.
        let origin = self
            .connection
            .translate_coordinates(id, geometry.root, 0, 0)?
            .reply()?;
        let scale = window.scale_factor();
        let logical = |n| px(n / scale);
        Ok(WindowBounds::Windowed(Bounds::new(
            point(
                logical(f32::from(origin.dst_x)),
                logical(f32::from(origin.dst_y)),
            ),
            size(
                logical(f32::from(geometry.width)),
                logical(f32::from(geometry.height)),
            ),
        )))
    }

    pub(crate) fn restore(&self, window: &Window, geometry: WindowGeometry) -> Result<()> {
        let id = Self::window_id(window)?;
        let root = self.connection.get_geometry(id)?.reply()?.root;
        let scale = window.scale_factor();
        let position = |n: f32| (n * scale).round() as i32 as u32;
        // EWMH uses StaticGravity to interpret coordinates as the client
        // origin. Request placement after mapping so the WM can apply it.
        let move_request = [
            u32::from(Gravity::STATIC) | (1 << 8) | (1 << 9) | (1 << 12),
            position(geometry.x),
            position(geometry.y),
            0,
            0,
        ];
        let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT;
        self.connection
            .send_event(
                false,
                root,
                mask,
                ClientMessageEvent::new(32, id, self.atoms._NET_MOVERESIZE_WINDOW, move_request),
            )?
            .check()
            .context("send window placement request")?;
        if geometry.maximized {
            // Use the same connection to order the maximize after placement.
            let maximize_request = [
                1, // _NET_WM_STATE_ADD
                self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
                1, // Normal application
                0,
            ];
            self.connection
                .send_event(
                    false,
                    root,
                    mask,
                    ClientMessageEvent::new(32, id, self.atoms._NET_WM_STATE, maximize_request),
                )?
                .check()
                .context("send window maximize request")?;
        }
        Ok(())
    }
}
