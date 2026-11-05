//! Read-only local prerequisite diagnostics.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Serialize)]
struct Check {
    id: String,
    status: Status,
    message: String,
    fix: Option<String>,
}
impl Check {
    fn new(id: &str, status: Status, message: impl Into<String>, fix: Option<&str>) -> Self {
        Self {
            id: id.into(),
            status,
            message: message.into(),
            fix: fix.map(str::to_owned),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    checks: Vec<Check>,
    exit_code: u8,
}
impl Report {
    fn new(checks: Vec<Check>) -> Self {
        let exit_code = u8::from(checks.iter().any(|c| c.status == Status::Error));
        Self { checks, exit_code }
    }
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}
pub fn print_report(report: &Report, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        for check in &report.checks {
            let mark = match check.status {
                Status::Ok => '✓',
                Status::Warning => '!',
                Status::Error => '✗',
            };
            print!("{mark} {}: {}", check.id, check.message);
            if let Some(fix) = &check.fix {
                print!(" — {fix}");
            }
            println!();
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CommandResult {
    success: bool,
    stdout: String,
}
trait Environment {
    fn executable(&self, name: &str) -> Option<PathBuf>;
    fn run(
        &self,
        name: &str,
        executable: &Path,
        args: &[&str],
        capture: bool,
    ) -> Option<CommandResult>;
}
fn invocation_path(name: &str, resolved: &Path, directories: &[PathBuf]) -> PathBuf {
    let canonical = std::fs::canonicalize(resolved).unwrap_or_else(|_| resolved.to_owned());
    let mut dirs = directories.to_vec();
    if let Some(parent) = resolved.parent() {
        dirs.push(parent.to_owned());
    }
    for dir in dirs.into_iter().filter(|dir| dir.is_absolute()) {
        #[cfg(not(windows))]
        let names = vec![name.to_owned()];
        #[cfg(windows)]
        let names = std::iter::once(name.to_owned())
            .chain(["exe", "com", "bat", "cmd"].map(|ext| format!("{name}.{ext}")))
            .collect::<Vec<_>>();
        for candidate in names.into_iter().map(|name| dir.join(name)) {
            if std::fs::canonicalize(&candidate).is_ok_and(|path| path == canonical) {
                return candidate;
            }
        }
    }
    resolved.to_owned()
}
struct System;
impl Environment for System {
    fn executable(&self, name: &str) -> Option<PathBuf> {
        let resolved = match name {
            "claude" => mast_harness::claude::ClaudeHarness::new()
                .resolve_executable()
                .ok(),
            "codex" => mast_harness::codex::CodexHarness::new()
                .resolve_executable()
                .ok(),
            _ => mast_harness::resolve_login_shell_executable(name),
        }?;
        // Harness resolution already preserves overrides and executable aliases.
        if name == "claude" || name == "codex" {
            return Some(resolved);
        }
        let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
            .as_deref()
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .collect();
        if let Some(path) = mast_harness::shell_env::login_shell_path() {
            directories.extend(std::env::split_paths(path));
        }
        // Preserve invocation names for multicall binaries such as rustup.
        // The harness returns canonical paths, which otherwise turn cargo into rustup.
        Some(invocation_path(name, &resolved, &directories))
    }
    fn run(
        &self,
        name: &str,
        executable: &Path,
        args: &[&str],
        capture: bool,
    ) -> Option<CommandResult> {
        let is_git = name == "git";
        let mut command = if is_git {
            mast_harness::git_command()
        } else {
            Command::new(executable)
        };
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        command.stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.env("RUSTUP_AUTO_INSTALL", "0");
        if is_git {
            // git_command is required for all Git spawns; bind its lookup to the
            // exact absolute directory selected by executable discovery.
            if executable.file_stem().is_some_and(|stem| stem == "git") {
                if let Some(parent) = executable.parent() {
                    command.env("PATH", parent);
                }
            }
        } else {
            mast_harness::compose_login_shell_path_std(&mut command);
        }
        let mut child = command.spawn().ok()?;
        // Drain bounded output concurrently so a broken executable cannot deadlock on a full pipe.
        let reader = child.stdout.take().map(|out| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                use std::io::Read;
                let mut bytes = Vec::new();
                let _ = out.take(4096).read_to_end(&mut bytes);
                let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
            });
            receiver
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = reader
            .and_then(|r| r.recv_timeout(Duration::from_millis(200)).ok())
            .unwrap_or_default();
        status.map(|status| CommandResult {
            success: status.success(),
            stdout,
        })
    }
}
fn clean_version(output: &str) -> Option<String> {
    // Publish numeric versions only, never arbitrary executable output. This also
    // accepts prefixes such as git-lfs/3.7.1 and suffixes such as -nightly.
    output
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find_map(|word| {
            let word = word.trim_matches('.');
            let parts: Vec<_> = word.split('.').collect();
            (parts.len() >= 2
                && parts.len() <= 4
                && parts.iter().all(|p| {
                    !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_digit())
                }))
            .then(|| word.to_owned())
        })
}

fn version_check(
    env: &impl Environment,
    name: &str,
    optional: bool,
    install: &str,
) -> (Check, Option<PathBuf>, Option<String>) {
    let Some(path) = env.executable(name) else {
        return (
            Check::new(
                name,
                if optional {
                    Status::Warning
                } else {
                    Status::Error
                },
                "not found",
                Some(install),
            ),
            None,
            None,
        );
    };
    let version = env
        .run(name, &path, &["--version"], true)
        .filter(|o| o.success)
        .and_then(|o| clean_version(&o.stdout));
    let check = match &version {
        Some(v) => Check::new(
            name,
            Status::Ok,
            format!("version {v} ({})", path.display()),
            None,
        ),
        None => Check::new(
            name,
            Status::Warning,
            format!("found at {}; version unavailable", path.display()),
            Some("Run the executable with --version and check its installation"),
        ),
    };
    (check, Some(path), version)
}
fn auth_check(env: &impl Environment, gh: Option<&Path>) -> Check {
    match gh {
        None => Check::new(
            "gh authentication",
            Status::Error,
            "unavailable",
            Some("Install gh and run gh auth login"),
        ),
        Some(path)
            if env
                .run("gh", path, &["auth", "status"], false)
                .is_some_and(|r| r.success) =>
        {
            Check::new("gh authentication", Status::Ok, "signed in", None)
        }
        Some(_) => Check::new(
            "gh authentication",
            Status::Error,
            "not signed in or status unavailable",
            Some("Run gh auth status, then gh auth login if needed"),
        ),
    }
}
fn read_json(path: &Path) -> Result<Option<serde_json::Value>, ()> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| ()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}
fn nonempty(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}
fn account_check(
    name: &str,
    path: &Path,
    present: impl FnOnce(&serde_json::Value) -> bool,
    login: &str,
) -> Check {
    match read_json(path) {
        Ok(Some(value)) if present(&value) => Check::new(
            &format!("{name} account"),
            Status::Ok,
            "local sign-in detected (not validated online)",
            None,
        ),
        Ok(_) => Check::new(
            &format!("{name} account"),
            Status::Warning,
            "no local sign-in detected",
            Some(login),
        ),
        Err(_) => Check::new(
            &format!("{name} account"),
            Status::Warning,
            "local sign-in could not be inspected",
            Some(login),
        ),
    }
}
fn account_checks(env: &impl Environment, data_dir: &Path, cursor: bool) -> Vec<Check> {
    let config = mast_engine::agent_accounts::AgentAccountsConfig::detect(data_dir);
    account_checks_for(env, &config, cursor)
}
fn account_checks_for(
    env: &impl Environment,
    config: &mast_engine::agent_accounts::AgentAccountsConfig,
    cursor: bool,
) -> Vec<Check> {
    let mut claude = account_check(
        "claude",
        &config.claude_config_file,
        |v| nonempty(v.pointer("/oauthAccount/emailAddress")),
        "Run claude auth status or connect an account in Mast",
    );
    if claude.status == Status::Ok {
        let file_credentials = read_json(&config.claude_config_dir.join(".credentials.json"))
            .ok()
            .flatten()
            .is_some_and(|v| nonempty(v.pointer("/claudeAiOauth/accessToken")));
        #[cfg(target_os = "macos")]
        let keychain_credentials = env.executable("security").is_some_and(|security| {
            env.run(
                "security",
                &security,
                &["find-generic-password", "-s", "Claude Code-credentials"],
                false,
            )
            .is_some_and(|result| result.success)
        });
        #[cfg(not(target_os = "macos"))]
        let keychain_credentials = {
            let _ = env;
            false
        };
        if !file_credentials && !keychain_credentials {
            claude.status = Status::Warning;
            claude.message = "account identity found; credentials unavailable".into();
            claude.fix = Some("Run claude auth status or connect an account in Mast".into());
        }
    }
    let mut checks = vec![
        claude,
        account_check(
            "codex",
            &config.codex_home.join("auth.json"),
            |v| nonempty(v.pointer("/tokens/id_token")) || nonempty(v.get("OPENAI_API_KEY")),
            "Run codex login or connect an account in Mast",
        ),
    ];
    if cursor {
        checks.push(account_check(
            "cursor",
            &config.cursor_sdk_auth_file,
            |v| {
                nonempty(v.get("apiKey"))
                    && v.get("apiKeyExpiresAtMs")
                        .and_then(|v| v.as_i64())
                        .is_none_or(|expiry| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .is_ok_and(|now| expiry > now.as_millis() as i64)
                        })
            },
            "Connect Cursor in Mast",
        ));
    }
    checks
}
fn linear_check(env: &impl Environment) -> Check {
    #[cfg(target_os = "macos")]
    {
        let Some(security) = env.executable("security") else {
            return Check::new(
                "Linear",
                Status::Warning,
                "Keychain status unavailable",
                Some("Open Settings → Linear to configure a key"),
            );
        };
        // No -w: security only reports item attributes, and stdout is discarded.
        return if ["computer.mikey.mast.linear", "sh.zeron.linear"]
            .iter()
            .any(|service| {
                env.run(
                    "security",
                    &security,
                    &["find-generic-password", "-s", service, "-a", "api-key"],
                    false,
                )
                .is_some_and(|result| result.success)
            }) {
            Check::new("Linear", Status::Ok, "Keychain item present", None)
        } else {
            Check::new(
                "Linear",
                Status::Warning,
                "Keychain item absent or unavailable",
                Some("Optionally add a key in Settings → Linear"),
            )
        };
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = env;
        Check::new(
            "Linear",
            Status::Warning,
            "Keychain unavailable on this platform",
            Some("Optionally set LINEAR_API_KEY"),
        )
    }
}

fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.exists())
}
#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .ok()
        .is_some_and(|p| unsafe { libc::access(p.as_ptr(), libc::W_OK | libc::X_OK) == 0 })
}
#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| !m.permissions().readonly())
}
#[cfg(unix)]
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}
#[cfg(not(unix))]
fn free_space(_: &Path) -> Option<u64> {
    None
}
fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let metadata = entry.metadata()?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}
fn gib(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    }
}
fn disk_space_check(space: Option<u64>) -> Check {
    match space {
        Some(bytes) if bytes < 20 * 1_000_000_000 => Check::new(
            "disk space",
            Status::Warning,
            format!("{} free", gib(bytes)),
            Some("Free at least 20 GB on the data volume"),
        ),
        Some(bytes) => Check::new(
            "disk space",
            Status::Ok,
            format!("{} free", gib(bytes)),
            None,
        ),
        None => Check::new(
            "disk space",
            Status::Warning,
            "free space unavailable",
            Some("Check free space on the data volume"),
        ),
    }
}
fn data_checks(path: &Path) -> Vec<Check> {
    let Some(parent) = nearest_existing(path) else {
        return vec![Check::new(
            "data directory",
            Status::Error,
            "no existing parent",
            Some("Set MAST_DATA_DIR to a writable location"),
        )];
    };
    let valid = parent.is_dir() && writable(parent);
    let mut checks = vec![Check::new(
        "data directory",
        if valid { Status::Ok } else { Status::Error },
        format!(
            "{} ({})",
            path.display(),
            if path.exists() {
                if valid { "writable" } else { "not writable" }
            } else if valid {
                "creatable"
            } else {
                "not creatable"
            }
        ),
        (!valid).then_some("Set MAST_DATA_DIR to a writable directory or fix its permissions"),
    )];
    checks.push(disk_space_check(free_space(parent)));
    checks.push(if !path.exists() {
        Check::new("data size", Status::Ok, "0 bytes (directory absent)", None)
    } else {
        match directory_size(path) {
            Ok(bytes) => Check::new("data size", Status::Ok, gib(bytes), None),
            Err(_) => Check::new(
                "data size",
                Status::Warning,
                "size unavailable",
                Some("Check permissions on the data directory"),
            ),
        }
    });
    checks
}

#[cfg(unix)]
fn lock_holder(data_dir: &Path) -> std::io::Result<Option<String>> {
    use std::os::fd::AsRawFd;
    let path = data_dir.join("engine.lock");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Ok(None);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() != std::io::ErrorKind::WouldBlock {
        return Err(error);
    }
    // The lock file contains a PID; accept digits only, never print arbitrary file contents.
    let pid = std::fs::read_to_string(path).unwrap_or_default();
    let pid = pid.trim();
    Ok(Some(
        if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
            pid.to_owned()
        } else {
            "unknown".into()
        },
    ))
}
#[cfg(not(unix))]
fn lock_holder(_: &Path) -> std::io::Result<Option<String>> {
    Err(std::io::Error::other("lock probe unavailable"))
}
async fn engine_check(data_dir: &Path, port: u16) -> Check {
    let holder = lock_holder(data_dir);
    let responding = tokio::time::timeout(Duration::from_secs(2), async {
        let client = mast_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
            .await
            .ok()?;
        let value = client
            .call(mast_rpc::methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .ok()?;
        serde_json::from_value::<mast_engine::EngineInfo>(value).ok()
    })
    .await
    .ok()
    .flatten()
    .is_some();
    match (holder, responding) {
        (Ok(Some(pid)), true) => Check::new(
            "engine",
            Status::Ok,
            format!("running (pid {pid}); IPC responds on port {port}"),
            None,
        ),
        (Ok(Some(pid)), false) => Check::new(
            "engine",
            Status::Warning,
            format!("lock held by pid {pid}, but IPC did not respond on port {port}"),
            Some("Check the running Mast instance and MAST_IPC_PORT"),
        ),
        (Ok(None), true) => Check::new(
            "engine",
            Status::Warning,
            format!(
                "IPC responds on port {port}, but this data directory has no running engine lock"
            ),
            Some("Check MAST_DATA_DIR and MAST_IPC_PORT"),
        ),
        (Ok(None), false) => Check::new(
            "engine",
            Status::Warning,
            "not running",
            Some("Open Mast or start zeron headless when needed"),
        ),
        (Err(_), _) => Check::new(
            "engine",
            Status::Warning,
            "lock status unavailable",
            Some("Check permissions on the data directory and run zeron status"),
        ),
    }
}
fn toolchain_channel(start: &Path) -> Result<Option<String>, ()> {
    for dir in start.ancestors() {
        for filename in ["rust-toolchain.toml", "rust-toolchain"] {
            let path = dir.join(filename);
            match std::fs::read_to_string(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(()),
                Ok(content) if filename.ends_with(".toml") => {
                    let value = content.parse::<toml::Value>().map_err(|_| ())?;
                    return value
                        .get("toolchain")
                        .and_then(|v| v.get("channel"))
                        .and_then(|v| v.as_str())
                        .map(|s| Some(s.to_owned()))
                        .ok_or(());
                }
                Ok(content) => return Ok(Some(content.trim().to_owned())),
            }
        }
    }
    Ok(None)
}
fn toolchain_check(
    env: &impl Environment,
    start: &Path,
    cargo_version: Option<&str>,
    rustc_version: Option<&str>,
) -> Check {
    let channel = match toolchain_channel(start) {
        Ok(Some(channel)) if !channel.is_empty() => channel,
        Ok(_) => {
            return Check::new(
                "Rust toolchain",
                Status::Warning,
                "no rust-toolchain file found from the current directory",
                Some("Run zeron doctor from the Mast checkout to compare its toolchain"),
            );
        }
        Err(_) => {
            return Check::new(
                "Rust toolchain",
                Status::Warning,
                "toolchain file unreadable or invalid",
                Some("Inspect rust-toolchain.toml in the checkout"),
            );
        }
    };
    // Do not echo arbitrary file or command contents; channel names have a strict grammar.
    if !channel
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
        || channel.len() > 80
    {
        return Check::new(
            "Rust toolchain",
            Status::Warning,
            "invalid toolchain channel",
            Some("Inspect rust-toolchain.toml"),
        );
    }
    let rustup_match = env
        .executable("rustup")
        .and_then(|p| env.run("rustup", &p, &["show", "active-toolchain"], true))
        .filter(|r| r.success)
        .and_then(|r| r.stdout.split_whitespace().next().map(str::to_owned))
        .is_some_and(|active| {
            active == channel
                || active
                    .strip_prefix(&channel)
                    .is_some_and(|tail| tail.starts_with('-'))
        });
    let numeric_match = channel.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && rustc_version.is_some_and(|v| v == channel);
    let versions_match = cargo_version
        .zip(rustc_version)
        .is_some_and(|(cargo, rustc)| {
            let release = |s: &str| s.split('.').take(2).collect::<Vec<_>>().join(".");
            release(cargo) == release(rustc)
        });
    let numeric_channel = channel.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if (if numeric_channel {
        numeric_match
    } else {
        rustup_match
    }) && versions_match
    {
        Check::new(
            "Rust toolchain",
            Status::Ok,
            if numeric_channel {
                format!("rustc matches {channel}; cargo version agrees")
            } else {
                format!("{channel} selected; cargo and rustc versions agree")
            },
            None,
        )
    } else {
        Check::new(
            "Rust toolchain",
            Status::Warning,
            format!("could not confirm {channel} is active"),
            Some(
                "Run rustup show active-toolchain from the checkout and install/select its pinned toolchain",
            ),
        )
    }
}

pub async fn inspect(data_dir: PathBuf, port: u16) -> Report {
    let env = System;
    let mut checks = Vec::new();
    let (claude, claude_path, _) = version_check(
        &env,
        "claude",
        true,
        "Install Claude Code and make claude available on PATH",
    );
    checks.push(claude);
    let (codex, codex_path, _) = version_check(
        &env,
        "codex",
        true,
        "Install Codex and make codex available on PATH",
    );
    checks.push(codex);
    if claude_path.is_none() && codex_path.is_none() {
        checks.push(Check::new(
            "agent CLI",
            Status::Error,
            "neither Claude Code nor Codex is installed",
            Some("Install and sign in to Claude Code or Codex"),
        ));
    }
    let cursor = env.executable("cursor");
    if cursor.is_some() {
        checks.push(version_check(&env, "cursor", true, "Install Cursor CLI").0);
    }
    checks.extend(account_checks(&env, &data_dir, cursor.is_some()));
    let (gh, gh_path, _) = version_check(&env, "gh", false, "Install gh and run gh auth login");
    checks.push(gh);
    checks.push(auth_check(&env, gh_path.as_deref()));
    checks.push(version_check(&env, "git", false, "Install Git and add it to PATH").0);
    checks.push(
        version_check(
            &env,
            "git-lfs",
            false,
            "Install Git LFS and make git-lfs available on PATH",
        )
        .0,
    );
    checks.extend(data_checks(&data_dir));
    checks.push(engine_check(&data_dir, port).await);
    let (cargo, _, cargo_version) = version_check(&env, "cargo", false, "Install Rust with rustup");
    checks.push(cargo);
    let (rustc, _, rustc_version) = version_check(&env, "rustc", false, "Install Rust with rustup");
    checks.push(rustc);
    if let Ok(cwd) = std::env::current_dir() {
        checks.push(toolchain_check(
            &env,
            &cwd,
            cargo_version.as_deref(),
            rustc_version.as_deref(),
        ));
    }
    checks.push(linear_check(&env));
    Report::new(checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeEnvironment {
        programs: HashMap<String, PathBuf>,
        outputs: HashMap<String, (bool, String)>,
        calls: Mutex<Vec<String>>,
    }
    impl FakeEnvironment {
        fn program(mut self, name: &str, args: &[&str], success: bool, output: &str) -> Self {
            self.programs
                .insert(name.into(), PathBuf::from(format!("/fake/{name}")));
            self.outputs.insert(
                format!("{name} {}", args.join(" ")),
                (success, output.into()),
            );
            self
        }
    }
    impl Environment for FakeEnvironment {
        fn executable(&self, name: &str) -> Option<PathBuf> {
            self.programs.get(name).cloned()
        }
        fn run(
            &self,
            _name: &str,
            executable: &Path,
            args: &[&str],
            _: bool,
        ) -> Option<CommandResult> {
            let key = format!("{} {}", executable.file_name()?.to_str()?, args.join(" "));
            self.calls.lock().unwrap().push(key.clone());
            self.outputs
                .get(&key)
                .map(|(success, stdout)| CommandResult {
                    success: *success,
                    stdout: stdout.clone(),
                })
        }
    }
    #[test]
    fn executable_checks_use_injected_runner_and_discard_arbitrary_output() {
        let env = FakeEnvironment::default().program(
            "git",
            &["--version"],
            true,
            "git version 2.42.1\nsecret-value",
        );
        let (check, path, version) = version_check(&env, "git", false, "install git");
        assert_eq!(check.status, Status::Ok);
        assert_eq!(version.as_deref(), Some("2.42.1"));
        assert!(!check.message.contains("secret-value"));
        assert_eq!(path.unwrap(), PathBuf::from("/fake/git"));
        assert_eq!(
            version_check(&env, "cargo", false, "install").0.status,
            Status::Error
        );
        assert_eq!(
            version_check(&env, "cursor", true, "install").0.status,
            Status::Warning
        );
        assert_eq!(clean_version("unparseable token"), None);
        assert_eq!(
            clean_version("git-lfs/3.7.1 (GitHub; darwin)"),
            Some("3.7.1".into())
        );
        for name in ["claude", "codex", "gh", "git-lfs", "cargo", "rustc"] {
            assert_eq!(
                version_check(&env, name, false, "install").0.status,
                Status::Error
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn executable_invocation_preserves_the_selected_shim_name() {
        let dir = tempfile::tempdir().unwrap();
        let rustup = dir.path().join("rustup");
        std::fs::write(&rustup, "fake executable").unwrap();
        std::os::unix::fs::symlink(&rustup, dir.path().join("cargo")).unwrap();
        std::os::unix::fs::symlink(&rustup, dir.path().join("rustc")).unwrap();
        assert_eq!(
            invocation_path("cargo", &rustup, &[dir.path().to_owned()]),
            dir.path().join("cargo")
        );
        assert_eq!(
            invocation_path("rustc", &rustup, &[dir.path().to_owned()]),
            dir.path().join("rustc")
        );
        assert_eq!(
            invocation_path("missing", &rustup, &[dir.path().to_owned()]),
            rustup
        );
    }
    #[test]
    fn github_auth_uses_only_status_and_never_exposes_output() {
        let env =
            FakeEnvironment::default().program("gh", &["auth", "status"], true, "sensitive output");
        let check = auth_check(&env, env.executable("gh").as_deref());
        assert_eq!(check.status, Status::Ok);
        assert!(!check.message.contains("sensitive"));
        assert_eq!(&*env.calls.lock().unwrap(), &["gh auth status"]);
        assert_eq!(auth_check(&env, None).status, Status::Error);
        let failed = FakeEnvironment::default().program("gh", &["auth", "status"], false, "");
        assert_eq!(
            auth_check(&failed, failed.executable("gh").as_deref()).status,
            Status::Error
        );
    }
    #[test]
    fn account_checks_detect_presence_without_revealing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("account.json");
        let check = |name: &str, predicate: fn(&serde_json::Value) -> bool| {
            account_check(name, &file, predicate, "sign in")
        };
        assert_eq!(check("claude", |_| true).status, Status::Warning);
        std::fs::write(&file, r#"{"oauthAccount":{"emailAddress":"private@example.com"},"tokens":{"id_token":"secret"},"apiKey":"secret"}"#).unwrap();
        let claude = check("claude", |v| {
            nonempty(v.pointer("/oauthAccount/emailAddress"))
        });
        let codex = check("codex", |v| nonempty(v.pointer("/tokens/id_token")));
        let cursor = check("cursor", |v| nonempty(v.get("apiKey")));
        for result in [claude, codex, cursor] {
            assert_eq!(result.status, Status::Ok);
            assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
            assert!(!result.message.contains("private@example.com"));
        }
        std::fs::write(&file, "invalid").unwrap();
        assert_eq!(check("codex", |_| true).status, Status::Warning);
    }
    #[test]
    fn account_detectors_use_injected_paths_and_require_claude_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let config = mast_engine::agent_accounts::AgentAccountsConfig {
            data_dir: dir.path().join("data"),
            claude_config_dir: dir.path().join("claude"),
            claude_config_file: dir.path().join("claude.json"),
            codex_home: dir.path().join("codex"),
            cursor_sdk_auth_file: dir.path().join("cursor.json"),
        };
        std::fs::create_dir_all(&config.claude_config_dir).unwrap();
        std::fs::create_dir_all(&config.codex_home).unwrap();
        std::fs::write(
            &config.claude_config_file,
            r#"{"oauthAccount":{"emailAddress":"private"}}"#,
        )
        .unwrap();
        std::fs::write(
            config.codex_home.join("auth.json"),
            r#"{"tokens":{"id_token":"secret"}}"#,
        )
        .unwrap();
        std::fs::write(
            &config.cursor_sdk_auth_file,
            r#"{"apiKey":"secret","apiKeyExpiresAtMs":1}"#,
        )
        .unwrap();
        let env = FakeEnvironment::default();
        let checks = account_checks_for(&env, &config, true);
        assert_eq!(
            checks.iter().map(|c| c.status).collect::<Vec<_>>(),
            [Status::Warning, Status::Ok, Status::Warning]
        );
        std::fs::write(
            config.claude_config_dir.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"secret"}}"#,
        )
        .unwrap();
        let checks = account_checks_for(&env, &config, true);
        assert_eq!(checks[0].status, Status::Ok);
        assert!(!serde_json::to_string(&checks).unwrap().contains("secret"));
    }
    #[test]
    fn data_checks_do_not_create_directory_and_count_files_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("missing");
        assert_eq!(data_checks(&absent)[0].status, Status::Ok);
        assert!(!absent.exists());
        std::fs::create_dir(&absent).unwrap();
        std::fs::write(absent.join("file"), b"12345").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), absent.join("loop")).unwrap();
        assert_eq!(directory_size(&absent).unwrap(), 5);
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(data_checks(&file.join("child"))[0].status, Status::Error);
    }
    #[test]
    fn disk_space_threshold_and_unknown_space_report_fixes() {
        assert_eq!(
            disk_space_check(Some(19_999_999_999)).status,
            Status::Warning
        );
        assert_eq!(disk_space_check(Some(20_000_000_000)).status, Status::Ok);
        assert!(disk_space_check(None).fix.is_some());
    }
    #[test]
    fn toolchain_comparison_uses_checkout_pin_and_injected_rustup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rust-toolchain.toml");
        let env = FakeEnvironment::default().program(
            "rustup",
            &["show", "active-toolchain"],
            true,
            "stable-aarch64-apple-darwin (overridden)\n",
        );
        assert_eq!(
            toolchain_check(&env, dir.path(), None, None).status,
            Status::Warning
        );
        std::fs::write(&path, "[toolchain]\nchannel = \"stable\"\n").unwrap();
        assert_eq!(
            toolchain_check(&env, dir.path(), Some("1.80.1"), Some("1.80.0")).status,
            Status::Ok
        );
        std::fs::write(&path, "[toolchain]\nchannel = \"1.80.0\"\n").unwrap();
        assert_eq!(
            toolchain_check(&env, dir.path(), Some("1.80.1"), Some("1.80.0")).status,
            Status::Ok
        );
        assert_eq!(
            toolchain_check(&env, dir.path(), Some("1.79.0"), Some("1.79.0")).status,
            Status::Warning
        );
        let misleading = FakeEnvironment::default().program(
            "rustup",
            &["show", "active-toolchain"],
            true,
            "1.80.0-aarch64-apple-darwin",
        );
        assert_eq!(
            toolchain_check(&misleading, dir.path(), Some("1.79.0"), Some("1.79.0")).status,
            Status::Warning
        );
        std::fs::write(&path, "broken = [").unwrap();
        assert_eq!(
            toolchain_check(&env, dir.path(), None, None).status,
            Status::Warning
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn linear_keychain_probe_never_requests_value() {
        let env = FakeEnvironment::default().program(
            "security",
            &[
                "find-generic-password",
                "-s",
                "computer.mikey.mast.linear",
                "-a",
                "api-key",
            ],
            true,
            "secret",
        );
        let check = linear_check(&env);
        assert_eq!(check.status, Status::Ok);
        assert!(!check.message.contains("secret"));
        assert!(!env.calls.lock().unwrap()[0].contains(" -w"));
        assert_eq!(
            linear_check(&FakeEnvironment::default()).status,
            Status::Warning
        );
    }
    #[cfg(unix)]
    #[test]
    fn engine_lock_probe_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(lock_holder(dir.path()).unwrap(), None);
        assert!(!dir.path().join("engine.lock").exists());
        let lock = mast_engine::instance_lock::InstanceLock::acquire(dir.path()).unwrap();
        assert_eq!(
            lock_holder(dir.path()).unwrap().as_deref(),
            Some(std::process::id().to_string().as_str())
        );
        drop(lock);
        assert_eq!(lock_holder(dir.path()).unwrap(), None);
    }
    #[tokio::test]
    async fn stopped_engine_reports_a_fix() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let check = engine_check(dir.path(), port).await;
        assert_eq!(check.status, Status::Warning);
        assert!(check.fix.is_some());
    }
    #[test]
    fn json_shape_and_exit_code_distinguish_errors_from_warnings() {
        let warning = Check::new("disk", Status::Warning, "low", Some("free space"));
        let report = Report::new(vec![warning]);
        assert_eq!(report.exit_code(), 0);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({"checks":[{"id":"disk","status":"warning","message":"low","fix":"free space"}],"exit_code":0})
        );
        let report = Report::new(vec![Check::new(
            "git",
            Status::Error,
            "missing",
            Some("install"),
        )]);
        assert_eq!(report.exit_code(), 1);
    }
}
