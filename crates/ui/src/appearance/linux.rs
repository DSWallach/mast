//! Linux system light/dark detection.
//!
//! gpui's Linux backend does listen to the XDG desktop portal, but it is not
//! enough on its own:
//!
//! * it starts out reporting `Light` and only learns the real value once an
//!   async portal read lands, after the first window has already painted;
//! * it maps the portal's `no-preference` to `Light`, which is wrong for the
//!   (still common) setups that express dark mode only through a dark GTK
//!   theme such as `Adwaita-dark`;
//! * with no portal at all (bare window managers, some containers) it never
//!   reports anything but `Light`.
//!
//! So on Linux Mast owns the decision. [`detect`] combines, in order:
//!
//! 1. the portal's `org.freedesktop.appearance` `color-scheme`
//!    (`1` = prefer dark, `2` = prefer light, `0` = no preference);
//! 2. GSettings `org.gnome.desktop.interface color-scheme`;
//! 3. a dark GTK theme: `$GTK_THEME`, GSettings `gtk-theme`, or
//!    `gtk-application-prefer-dark-theme` in `~/.config/gtk-{4,3}.0/settings.ini`;
//!
//! falling back to `Light`. A worker thread re-runs it whenever the portal
//! emits `SettingChanged` for one of those keys (or, without a portal, when
//! `gsettings monitor` reports a change), and whenever gpui itself notices a
//! window appearance change.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc as std_mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::StreamExt;
use gpui::App;
use zvariant::{OwnedValue, Value};

use crate::theme::Appearance;

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE_NS: &str = "org.freedesktop.appearance";
const GNOME_INTERFACE_NS: &str = "org.gnome.desktop.interface";

/// How long boot waits for the first answer before opening windows with
/// gpui's guess. The portal answers in a few milliseconds when it is running;
/// this only bounds the pathological case (a portal being D-Bus activated, a
/// wedged session bus) so startup never stalls on it.
const INITIAL_TIMEOUT: Duration = Duration::from_millis(400);

/// Wakes the worker to re-detect (gpui window callbacks, `gsettings monitor`).
static REFRESH_TX: OnceLock<UnboundedSender<()>> = OnceLock::new();

/// Start following the desktop's light/dark setting. Blocks (briefly — see
/// [`INITIAL_TIMEOUT`]) for the initial value so the first frame is right, then
/// streams changes into [`super::sync`]. Idempotent.
pub(super) fn start(cx: &mut App) {
    if REFRESH_TX.get().is_some() {
        return;
    }
    let (refresh_tx, refresh_rx) = unbounded::<()>();
    let _ = REFRESH_TX.set(refresh_tx);
    let (initial_tx, initial_rx) = std_mpsc::channel::<Appearance>();
    let (change_tx, mut change_rx) = unbounded::<Appearance>();

    let spawned = std::thread::Builder::new()
        .name("mast-appearance".into())
        .spawn(move || {
            async_io::block_on(run_worker(initial_tx, change_tx, refresh_rx));
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "appearance: could not start system theme watcher");
        return;
    }

    match initial_rx.recv_timeout(INITIAL_TIMEOUT) {
        Ok(system) => {
            tracing::debug!(?system, "appearance: initial linux system appearance");
            super::sync(system, cx);
        }
        Err(_) => {
            tracing::debug!("appearance: system theme detection slow; using platform default for now")
        }
    }

    cx.spawn(async move |cx| {
        while let Some(system) = change_rx.next().await {
            cx.update(|cx| super::sync(system, cx));
        }
    })
    .detach();
}

/// gpui saw a window appearance change (it watches the portal too). Its value
/// can be wrong for the reasons in the module docs, so treat it as a hint to
/// re-detect rather than as the answer.
pub(super) fn request_refresh() {
    if let Some(tx) = REFRESH_TX.get() {
        let _ = tx.unbounded_send(());
    }
}

/// Whether the Linux watcher is running (and so owns the system appearance).
pub(super) fn is_active() -> bool {
    REFRESH_TX.get().is_some()
}

async fn run_worker(
    initial_tx: std_mpsc::Sender<Appearance>,
    change_tx: UnboundedSender<Appearance>,
    refresh_rx: UnboundedReceiver<()>,
) {
    let connection = zbus::Connection::session().await.ok();
    let portal_scheme = match &connection {
        Some(conn) => read_portal_color_scheme(conn).await,
        None => None,
    };
    let mut last = detect(portal_scheme.flatten());
    let _ = initial_tx.send(last);
    // Boot may have given up waiting; make sure the main thread hears the
    // initial value either way (`sync` ignores a repeat).
    let _ = change_tx.unbounded_send(last);

    // Change sources, merged into one "something may have changed" stream.
    let portal_signals = match (&connection, portal_scheme.is_some()) {
        (Some(conn), true) => portal_setting_changes(conn).await,
        _ => None,
    };
    if portal_signals.is_none() {
        spawn_gsettings_monitor();
    }
    let mut events = match portal_signals {
        Some(signals) => futures::stream::select(signals, refresh_rx).boxed(),
        None => refresh_rx.boxed(),
    };

    while events.next().await.is_some() {
        let portal = match &connection {
            Some(conn) => read_portal_color_scheme(conn).await.flatten(),
            None => None,
        };
        let next = detect(portal);
        if next != last {
            tracing::debug!(?next, "appearance: linux system appearance changed");
            last = next;
            if change_tx.unbounded_send(next).is_err() {
                return;
            }
        }
    }
}

/// `None` = no portal (or no Settings interface); `Some(None)` = portal is up
/// but has no color-scheme opinion we can read.
async fn read_portal_color_scheme(conn: &zbus::Connection) -> Option<Option<u32>> {
    let args = (APPEARANCE_NS, "color-scheme");
    let reply = match conn
        .call_method(Some(PORTAL_DEST), PORTAL_PATH, Some(PORTAL_SETTINGS), "ReadOne", &args)
        .await
    {
        Ok(reply) => reply,
        // Portal versions before 2 only have the deprecated `Read`, which
        // wraps the value in an extra variant.
        Err(_) => conn
            .call_method(Some(PORTAL_DEST), PORTAL_PATH, Some(PORTAL_SETTINGS), "Read", &args)
            .await
            .ok()?,
    };
    let value: OwnedValue = reply.body().deserialize().ok()?;
    Some(value_to_u32(&value))
}

fn value_to_u32(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::U32(n) => Some(*n),
        Value::Value(inner) => value_to_u32(inner),
        _ => None,
    }
}

/// A stream that yields once per relevant portal `SettingChanged` signal.
async fn portal_setting_changes(
    conn: &zbus::Connection,
) -> Option<futures::stream::BoxStream<'static, ()>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PORTAL_SETTINGS)
        .ok()?
        .member("SettingChanged")
        .ok()?
        .path(PORTAL_PATH)
        .ok()?
        .build();
    let stream = match zbus::MessageStream::for_match_rule(rule, conn, None).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(%error, "appearance: cannot watch portal settings");
            return None;
        }
    };
    Some(
        stream
            .filter_map(|message| async move {
                let message = message.ok()?;
                let (namespace, key, _value): (String, String, OwnedValue) =
                    message.body().deserialize().ok()?;
                is_relevant_setting(&namespace, &key).then_some(())
            })
            .boxed(),
    )
}

fn is_relevant_setting(namespace: &str, key: &str) -> bool {
    matches!(
        (namespace, key),
        (APPEARANCE_NS, "color-scheme")
            | (GNOME_INTERFACE_NS, "color-scheme")
            | (GNOME_INTERFACE_NS, "gtk-theme")
    )
}

/// No portal: fall back to `gsettings monitor`, one refresh per reported line.
/// Silently does nothing when `gsettings` is not installed.
fn spawn_gsettings_monitor() {
    let child = Command::new("gsettings")
        .args(["monitor", GNOME_INTERFACE_NS])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("mast-gsettings-monitor".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.starts_with("color-scheme:") || line.starts_with("gtk-theme:") {
                    request_refresh();
                }
            }
            let _ = child.wait();
        });
}

/// Resolve the system appearance from every source we know about.
fn detect(portal_scheme: Option<u32>) -> Appearance {
    if let Some(appearance) = portal_scheme.and_then(appearance_from_portal) {
        return appearance;
    }
    if let Some(appearance) =
        gsettings_get("color-scheme").and_then(|v| appearance_from_gsettings_color_scheme(&v))
    {
        return appearance;
    }
    let gtk_theme_dark = std::env::var("GTK_THEME")
        .ok()
        .filter(|theme| !theme.trim().is_empty())
        .or_else(|| gsettings_get("gtk-theme"))
        .is_some_and(|theme| gtk_theme_is_dark(&theme));
    if gtk_theme_dark || gtk_settings_ini_prefers_dark() {
        return Appearance::Dark;
    }
    Appearance::Light
}

fn gsettings_get(key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", GNOME_INTERFACE_NS, key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn gtk_settings_ini_prefers_dark() -> bool {
    let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config")))
    else {
        return false;
    };
    ["gtk-4.0", "gtk-3.0"].iter().any(|dir| {
        std::fs::read_to_string(config.join(dir).join("settings.ini"))
            .is_ok_and(|ini| settings_ini_prefers_dark(&ini))
    })
}

/// Portal `color-scheme`: 1 = prefer dark, 2 = prefer light, anything else
/// (0 = no preference) is not an answer.
fn appearance_from_portal(scheme: u32) -> Option<Appearance> {
    match scheme {
        1 => Some(Appearance::Dark),
        2 => Some(Appearance::Light),
        _ => None,
    }
}

/// `gsettings get … color-scheme` prints a quoted enum nick.
fn appearance_from_gsettings_color_scheme(raw: &str) -> Option<Appearance> {
    match raw.trim().trim_matches('\'') {
        "prefer-dark" => Some(Appearance::Dark),
        "prefer-light" => Some(Appearance::Light),
        _ => None,
    }
}

/// `Adwaita-dark`, `Breeze-Dark`, `Materia-dark-compact`, `Adwaita:dark`
/// (`$GTK_THEME` variant syntax), `'Yaru-dark'` (gsettings quoting).
fn gtk_theme_is_dark(raw: &str) -> bool {
    let theme = raw.trim().trim_matches('\'').to_ascii_lowercase();
    theme
        .split(|c: char| c == '-' || c == '_' || c == ':' || c == '.' || c == ' ')
        .any(|part| part == "dark")
}

fn settings_ini_prefers_dark(ini: &str) -> bool {
    ini.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == "gtk-application-prefer-dark-theme"
            && matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_values_map_per_spec() {
        assert_eq!(appearance_from_portal(1), Some(Appearance::Dark));
        assert_eq!(appearance_from_portal(2), Some(Appearance::Light));
        assert_eq!(appearance_from_portal(0), None);
        assert_eq!(appearance_from_portal(7), None);
    }

    #[test]
    fn portal_value_unwraps_nested_variants() {
        assert_eq!(value_to_u32(&Value::U32(1)), Some(1));
        assert_eq!(
            value_to_u32(&Value::Value(Box::new(Value::U32(2)))),
            Some(2)
        );
        assert_eq!(value_to_u32(&Value::Str("dark".into())), None);
    }

    #[test]
    fn gsettings_color_scheme_parses_quoted_nicks() {
        assert_eq!(
            appearance_from_gsettings_color_scheme("'prefer-dark'\n"),
            Some(Appearance::Dark)
        );
        assert_eq!(
            appearance_from_gsettings_color_scheme("'prefer-light'"),
            Some(Appearance::Light)
        );
        assert_eq!(appearance_from_gsettings_color_scheme("'default'"), None);
        assert_eq!(appearance_from_gsettings_color_scheme(""), None);
    }

    #[test]
    fn dark_gtk_theme_names_are_recognised() {
        for dark in [
            "Adwaita-dark",
            "'Yaru-dark'",
            "Breeze-Dark",
            "Materia-dark-compact",
            "Adwaita:dark",
            "Arc_Dark",
        ] {
            assert!(gtk_theme_is_dark(dark), "{dark} should be dark");
        }
        for light in ["Adwaita", "'Yaru'", "Darkly", "Breeze", ""] {
            assert!(!gtk_theme_is_dark(light), "{light} should not be dark");
        }
    }

    #[test]
    fn settings_ini_prefer_dark_flag() {
        assert!(settings_ini_prefers_dark(
            "[Settings]\ngtk-theme-name=Adwaita\ngtk-application-prefer-dark-theme = 1\n"
        ));
        assert!(settings_ini_prefers_dark(
            "[Settings]\ngtk-application-prefer-dark-theme=true"
        ));
        assert!(!settings_ini_prefers_dark(
            "[Settings]\ngtk-application-prefer-dark-theme=0"
        ));
        assert!(!settings_ini_prefers_dark("[Settings]\n"));
    }

    /// Talks to the real session bus; run by hand on a desktop with
    /// `cargo test -p mast-ui --lib live_detection -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_detection_smoke() {
        let (portal, detected) = async_io::block_on(async {
            let conn = zbus::Connection::session().await.expect("session bus");
            let portal = read_portal_color_scheme(&conn).await;
            (portal, detect(portal.flatten()))
        });
        println!("portal color-scheme: {portal:?}; detected: {detected:?}");
        assert!(portal.is_some(), "expected a running XDG desktop portal");
    }

    #[test]
    fn only_appearance_keys_trigger_refresh() {
        assert!(is_relevant_setting(APPEARANCE_NS, "color-scheme"));
        assert!(is_relevant_setting(GNOME_INTERFACE_NS, "gtk-theme"));
        assert!(is_relevant_setting(GNOME_INTERFACE_NS, "color-scheme"));
        assert!(!is_relevant_setting(GNOME_INTERFACE_NS, "cursor-size"));
        assert!(!is_relevant_setting(APPEARANCE_NS, "accent-color"));
    }
}
