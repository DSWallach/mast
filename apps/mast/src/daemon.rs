//! `mast daemon …` — install/manage `mast headless` as a background service:
//! a systemd **user** unit on Linux (the VPS deployment target), a launchd
//! LaunchAgent on macOS. The unit runs the current executable with the
//! `MAST_*` environment captured at install time, so
//! `MAST_EDGE_URL=… mast daemon install` bakes that override in.
//!
//! Auth is decoupled: without a saved session the service remains up on the
//! local-only profile. `mast login` and a service restart opt into sync.

#![allow(clippy::disallowed_methods)] // TODO(post-mortem P3): route direct spawns (5 non-test sites) through mast_harness::command / git_command
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const LAUNCHD_LABEL: &str = "computer.mikey.mast";
const SYSTEMD_UNIT: &str = "mast.service";
/// Pre-0.3.0 names. Mast never stops, disables or deletes these: a user who
/// still runs one stops it by hand (see [`legacy_service_notice`]).
const LEGACY_SYSTEMD_UNIT: &str = "zeron.service";
const LEGACY_LAUNCHD_LABEL: &str = "sh.zeron.app";
/// Loopback IPC port the engine binds when `MAST_IPC_PORT` is unset.
const DEFAULT_IPC_PORT: u16 = 27654;

/// Environment captured into the unit file. `PATH` is always included (the
/// engine spawns harness CLIs like `claude`, which service managers' minimal
/// default PATH won't find); the `MAST_*`/logging vars only when set.
const CAPTURED_ENV: &[&str] = &[
    "PATH",
    "MAST_DATA_DIR",
    "MAST_EDGE_URL",
    "MAST_EDGE_TOKEN",
    "MAST_ORG_ID",
    "MAST_WORKOS_CLIENT_ID",
    "MAST_WORKOS_API_BASE",
    "MAST_IPC_PORT",
    "MAST_CALLBACK_PORT",
    "MAST_HARNESS",
    "MAST_DEVICE_NAME",
    "RUST_LOG",
];

pub fn install(data_dir: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("resolving the Mast executable path")?;
    let env = captured_env();
    // Refuse before touching any unit: a legacy `zeron.service` (or anything
    // else) already holding the engine port would make the new service crash-loop.
    if let Some(conflict) = port_conflict_error(
        ipc_port(&env),
        own_service_running(),
        &legacy_service_notice(),
        port_in_use,
    ) {
        bail!("{conflict}");
    }
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        std::fs::create_dir_all(plist.parent().expect("LaunchAgents parent"))?;
        std::fs::create_dir_all(data_dir)?;
        // Reinstall-friendly: unload any previous incarnation before rewriting.
        let _ = run_quiet("launchctl", &["bootout", &launchd_service_target()?]);
        std::fs::write(
            &plist,
            render_launchd_plist(&exe, &env, &data_dir.join("daemon.log")),
        )?;
        run(
            "launchctl",
            &["bootstrap", &launchd_domain()?, &plist.to_string_lossy()],
        )?;
        println!(
            "Installed and started {LAUNCHD_LABEL} ({}).",
            plist.display()
        );
    } else if cfg!(target_os = "linux") {
        let unit = systemd_unit_path()?;
        std::fs::create_dir_all(unit.parent().expect("systemd user dir"))?;
        std::fs::write(&unit, render_systemd_unit(&exe, &env))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT])?;
        println!("Installed and started {SYSTEMD_UNIT} ({}).", unit.display());
        println!(
            "For start-at-boot without an active login session (VPS): loginctl enable-linger $USER"
        );
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!(
        "Without a saved account the engine stays local-only; sign-in and restart are optional for sync."
    );
    println!(
        "Logs: {}",
        if cfg!(target_os = "macos") {
            format!("{}", data_dir.join("daemon.log").display())
        } else {
            format!("journalctl --user -u {SYSTEMD_UNIT}")
        }
    );
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let _ = run_quiet("launchctl", &["bootout", &launchd_service_target()?]);
        let plist = launchd_plist_path()?;
        match std::fs::remove_file(&plist) {
            Ok(()) => println!("Removed {}.", plist.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("Not installed.")
            }
            Err(err) => return Err(err.into()),
        }
    } else if cfg!(target_os = "linux") {
        let _ = run_quiet("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        let unit = systemd_unit_path()?;
        match std::fs::remove_file(&unit) {
            Ok(()) => {
                run("systemctl", &["--user", "daemon-reload"])?;
                println!("Removed {}.", unit.display());
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("Not installed.")
            }
            Err(err) => return Err(err.into()),
        }
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    Ok(())
}

pub fn start() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        if !plist.exists() {
            bail!("not installed — run `mast daemon install` first");
        }
        // `stop` boots the job out of the domain, so start = bootstrap; already
        // loaded is fine, then kickstart guarantees a running process either way.
        let _ = run_quiet(
            "launchctl",
            &["bootstrap", &launchd_domain()?, &plist.to_string_lossy()],
        );
        run("launchctl", &["kickstart", &launchd_service_target()?])?;
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "start", SYSTEMD_UNIT])?;
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!("Started.");
    Ok(())
}

pub fn stop() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        // bootout (not `kill`): with KeepAlive the job would otherwise respawn.
        run("launchctl", &["bootout", &launchd_service_target()?])?;
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "stop", SYSTEMD_UNIT])?;
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!("Stopped.");
    Ok(())
}

pub fn restart() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        if run_quiet(
            "launchctl",
            &["kickstart", "-k", &launchd_service_target()?],
        )
        .is_err()
        {
            // Not loaded (e.g. after `stop`) — fall through to a plain start.
            return start();
        }
        println!("Restarted.");
        Ok(())
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "restart", SYSTEMD_UNIT])?;
        println!("Restarted.");
        Ok(())
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
}

pub fn status() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        print_legacy_service_notice();
        let output = Command::new("launchctl")
            .args(["print", &launchd_service_target()?])
            .output()
            .context("running launchctl")?;
        if !output.status.success() {
            println!(
                "{LAUNCHD_LABEL}: not loaded{}",
                if launchd_plist_path()?.exists() {
                    " (installed — `mast daemon start`)"
                } else {
                    " (not installed — `mast daemon install`)"
                }
            );
            return Ok(());
        }
        // `launchctl print` is pages long; surface just the liveness lines.
        let text = String::from_utf8_lossy(&output.stdout);
        println!("{LAUNCHD_LABEL}: loaded");
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("state = ")
                || trimmed.starts_with("pid = ")
                || trimmed.starts_with("last exit code = ")
            {
                println!("  {trimmed}");
            }
        }
        Ok(())
    } else if cfg!(target_os = "linux") {
        print_legacy_service_notice();
        // Passthrough; `status` exits nonzero for inactive units, which is not an
        // error for us to report — the output already says it.
        let _ = Command::new("systemctl")
            .args(["--user", "--no-pager", "status", SYSTEMD_UNIT])
            .status()
            .context("running systemctl")?;
        Ok(())
    } else {
        bail!("mast daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
}

/// Tell the user about a pre-0.3.0 service still installed on this machine. Read-only:
/// it only inspects unit files and never stops or removes anything.
fn print_legacy_service_notice() {
    let notice = legacy_service_notice();
    if !notice.is_empty() {
        println!("{notice}");
    }
}

/// Instructions for stopping a leftover `zeron.service` / `sh.zeron.app` agent by hand,
/// or an empty string when neither is installed.
fn legacy_service_notice() -> String {
    let systemd = systemd_unit_path_for(LEGACY_SYSTEMD_UNIT).is_ok_and(|path| path.exists());
    let launchd = launchd_plist_path_for(LEGACY_LAUNCHD_LABEL).is_ok_and(|path| path.exists());
    legacy_notice_text(systemd, launchd)
}

fn legacy_notice_text(systemd: bool, launchd: bool) -> String {
    let mut lines = Vec::new();
    if systemd {
        lines.push(format!(
            "An old {LEGACY_SYSTEMD_UNIT} is still installed. Mast does not touch it; stop it yourself: `systemctl --user disable --now {LEGACY_SYSTEMD_UNIT}`."
        ));
    }
    if launchd {
        lines.push(format!(
            "An old {LEGACY_LAUNCHD_LABEL} launch agent is still installed. Mast does not touch it; stop it yourself: `launchctl bootout gui/$(id -u)/{LEGACY_LAUNCHD_LABEL}`."
        ));
    }
    lines.join("\n")
}

/// The engine IPC port the service will be started with: the captured
/// `MAST_IPC_PORT`, else the default.
fn ipc_port(env: &[(String, String)]) -> u16 {
    env.iter()
        .find(|(key, _)| key == "MAST_IPC_PORT")
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(DEFAULT_IPC_PORT)
}

/// True when something already listens on the loopback `port`. Probes by binding, so it
/// cannot disturb the other listener.
fn port_in_use(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// Whether the Mast service itself is running (a reinstall must not count its own
/// listener as a conflict).
fn own_service_running() -> bool {
    if cfg!(target_os = "macos") {
        launchd_service_target()
            .is_ok_and(|target| run_quiet("launchctl", &["print", &target]).is_ok())
    } else {
        run_quiet("systemctl", &["--user", "is-active", "--quiet", SYSTEMD_UNIT]).is_ok()
    }
}

/// The error to show when installing would collide with another listener, or `None` when
/// it is safe to proceed. Pure and non-destructive: it never stops the other process.
fn port_conflict_error(
    port: u16,
    own_service_running: bool,
    legacy_notice: &str,
    in_use: impl Fn(u16) -> bool,
) -> Option<String> {
    if own_service_running || !in_use(port) {
        return None;
    }
    let mut message = format!(
        "port {port} is already in use, so the Mast service could not start. Nothing was installed or stopped."
    );
    if !legacy_notice.is_empty() {
        message.push('\n');
        message.push_str(legacy_notice);
    }
    message.push_str(
        "\nStop whatever holds the port, or choose another with `MAST_IPC_PORT=<port> mast daemon install`.",
    );
    Some(message)
}

// ---------------------------------------------------------------------------
// Unit rendering (pure — unit-tested below)
// ---------------------------------------------------------------------------

fn captured_env() -> Vec<(String, String)> {
    CAPTURED_ENV
        .iter()
        .filter_map(|key| mast_proto::env::var(key).ok().map(|v| (key.to_string(), v)))
        .collect()
}

fn render_systemd_unit(exe: &Path, env: &[(String, String)]) -> String {
    let mut unit = String::from(
        "[Unit]\nDescription=Mast headless engine\nAfter=network-online.target\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Service]\n",
    );
    for (key, value) in env {
        // systemd unquotes the value; escape the characters it treats specially.
        let value = value.replace('\\', "\\\\").replace('"', "\\\"");
        unit.push_str(&format!("Environment=\"{key}={value}\"\n"));
    }
    unit.push_str(&format!(
        "ExecStart={} headless\nRestart=on-failure\nRestartSec=5\nEnvironmentFile=-%h/.mast/env\n\n[Install]\nWantedBy=default.target\n",
        systemd_exec_path(exe)
    ));
    unit
}

/// The ExecStart binary path. An exe under `~/.mast/app/` came from a
/// managed install, whose upgrades relink `app/current` — point the unit at
/// the symlink so it never pins one version.
/// (`current_exe` resolves symlinks, so the versioned dir is what we see here.)
fn systemd_exec_path(exe: &Path) -> String {
    exec_path_for(exe, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

fn exec_path_for(exe: &Path, home: Option<&Path>) -> String {
    let installed = home
        .map(|home| home.join(".mast/app"))
        .is_some_and(|app_root| exe.starts_with(app_root));
    if installed {
        "%h/.mast/app/current/mast".to_string()
    } else {
        format!("{}", exe.display())
    }
}

fn render_launchd_plist(exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let mut env_dict = String::new();
    for (key, value) in env {
        env_dict.push_str(&format!(
            "      <key>{}</key><string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exe}</string>
      <string>headless</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env_dict}    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict>
      <key>SuccessfulExit</key><false/>
    </dict>
    <key>ThrottleInterval</key><integer>30</integer>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
  </dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        exe = xml_escape(&exe.to_string_lossy()),
        env_dict = env_dict,
        log = xml_escape(&log.to_string_lossy()),
    )
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Paths + process helpers
// ---------------------------------------------------------------------------

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}

fn launchd_plist_path() -> anyhow::Result<PathBuf> {
    launchd_plist_path_for(LAUNCHD_LABEL)
}

fn launchd_plist_path_for(label: &str) -> anyhow::Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

fn systemd_unit_path() -> anyhow::Result<PathBuf> {
    systemd_unit_path_for(SYSTEMD_UNIT)
}

fn systemd_unit_path_for(unit: &str) -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".config"));
    Ok(config.join("systemd/user").join(unit))
}

fn launchd_domain() -> anyhow::Result<String> {
    let output = Command::new("id").arg("-u").output().context("id -u")?;
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() {
        bail!("could not determine the current uid");
    }
    Ok(format!("gui/{uid}"))
}

fn launchd_service_target() -> anyhow::Result<String> {
    Ok(format!("{}/{LAUNCHD_LABEL}", launchd_domain()?))
}

/// Run a command echoing it first; error (with stderr) on nonzero exit.
fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    println!("$ {program} {}", args.join(" "));
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Run without echoing; used where failure is an expected branch.
fn run_quiet(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!("{program} failed ({})", output.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_is_mast_service_and_the_legacy_unit_is_only_named_for_notices() {
        assert_eq!(SYSTEMD_UNIT, "mast.service");
        assert_eq!(LAUNCHD_LABEL, "computer.mikey.mast");
        assert_eq!(LEGACY_SYSTEMD_UNIT, "zeron.service");
    }

    #[test]
    fn port_conflict_is_a_non_destructive_error() {
        let notice = legacy_notice_text(true, false);
        let message = port_conflict_error(27654, false, &notice, |_| true).expect("conflict");
        assert!(message.contains("port 27654 is already in use"));
        assert!(message.contains("Nothing was installed or stopped"));
        // Names the old service and tells the user to stop it themselves.
        assert!(message.contains("zeron.service"));
        assert!(message.contains("stop it yourself"));
        assert!(message.contains("systemctl --user disable --now zeron.service"));
        assert!(message.contains("MAST_IPC_PORT"));
    }

    #[test]
    fn free_port_or_own_running_service_is_not_a_conflict() {
        assert!(port_conflict_error(27654, false, "", |_| false).is_none());
        // Reinstalling over the running Mast service is fine: its own listener holds the port.
        assert!(port_conflict_error(27654, true, "", |_| true).is_none());
    }

    #[test]
    fn port_probe_sees_a_real_listener_and_does_not_release_it() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_in_use(port));
        // The probe left the listener alone.
        assert!(listener.local_addr().is_ok());
        drop(listener);
        assert!(!port_in_use(port));
    }

    #[test]
    fn ipc_port_follows_the_captured_environment() {
        assert_eq!(ipc_port(&[]), DEFAULT_IPC_PORT);
        assert_eq!(ipc_port(&[("MAST_IPC_PORT".into(), "31000".into())]), 31000);
        assert_eq!(ipc_port(&[("MAST_IPC_PORT".into(), "nope".into())]), DEFAULT_IPC_PORT);
    }

    #[test]
    fn legacy_notice_covers_each_manager_and_is_empty_when_clean() {
        assert_eq!(legacy_notice_text(false, false), "");
        assert!(legacy_notice_text(false, true).contains("launchctl bootout"));
        let both = legacy_notice_text(true, true);
        assert!(both.contains("zeron.service") && both.contains("sh.zeron.app"));
    }

    #[test]
    fn systemd_unit_shape() {
        let unit = render_systemd_unit(
            Path::new("/usr/local/bin/mast"),
            &[
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("MAST_EDGE_URL".into(), "https://edge.example".into()),
                ("RUST_LOG".into(), "info,mast=\"debug\"".into()),
            ],
        );
        assert!(unit.contains("ExecStart=/usr/local/bin/mast headless\n"));
        assert!(unit.contains("Environment=\"PATH=/usr/bin:/bin\"\n"));
        assert!(unit.contains("Environment=\"MAST_EDGE_URL=https://edge.example\"\n"));
        // Inner quotes escaped so systemd re-parses the value verbatim.
        assert!(unit.contains("Environment=\"RUST_LOG=info,mast=\\\"debug\\\"\"\n"));
        assert!(unit.contains("StartLimitIntervalSec=60\n"));
        assert!(unit.contains("StartLimitBurst=5\n"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(!unit.contains("session.json"));
        assert!(!unit.contains("ConditionPathExists"));
        assert!(unit.contains("EnvironmentFile=-%h/.mast/env"));
        assert!(!unit.contains("zeron"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn installed_exe_uses_the_current_symlink() {
        // Installer-managed binary (current_exe resolves the `current` symlink to
        // the versioned dir): the unit must point back at the symlink.
        assert_eq!(
            exec_path_for(
                Path::new("/home/u/.mast/app/0.3.0/mast"),
                Some(Path::new("/home/u")),
            ),
            "%h/.mast/app/current/mast"
        );
        // Source build: literal path.
        assert_eq!(
            exec_path_for(
                Path::new("/src/target/debug/mast"),
                Some(Path::new("/home/u"))
            ),
            "/src/target/debug/mast"
        );
    }

    #[test]
    fn launchd_plist_shape() {
        let plist = render_launchd_plist(
            Path::new("/Users/x/mast & co/mast"),
            &[("MAST_EDGE_URL".into(), "https://e?a=1&b=2".into())],
            Path::new("/Users/x/.mast/daemon.log"),
        );
        assert!(plist.contains("<key>Label</key><string>computer.mikey.mast</string>"));
        // XML-escaped exe path and env value.
        assert!(plist.contains("<string>/Users/x/mast &amp; co/mast</string>"));
        assert!(plist.contains("<string>https://e?a=1&amp;b=2</string>"));
        assert!(plist.contains("<string>headless</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(
            plist.contains("<key>StandardOutPath</key><string>/Users/x/.mast/daemon.log</string>")
        );
    }
}
