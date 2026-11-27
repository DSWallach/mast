//! Executable discovery shared by native and ACP harnesses.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) fn home_dir() -> Option<PathBuf> {
    home_dir_with(&|key| std::env::var_os(key))
}

/// Resolve a user base directory, falling back to the current directory.
pub(crate) fn home_or_current_dir() -> PathBuf {
    home_or_current_dir_with(&|key| std::env::var_os(key), &std::env::current_dir)
}

/// A private, non-HOME working directory for automatic discovery processes.
///
/// Harnesses are also useful outside the engine, so they cannot assume an
/// engine profile is available. The engine normally supplies a profile-local
/// directory through each harness's `with_model_discovery_cwd` builder; this
/// fallback keeps direct harness use from inheriting a broad process cwd (or
/// choosing HOME) when no such directory was configured.
pub(crate) fn model_discovery_cwd(
    configured: Option<&Path>,
) -> Result<PathBuf, crate::HarnessError> {
    let cwd = configured.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("zeron-model-discovery-{}", std::process::id()))
    });
    std::fs::create_dir_all(&cwd).map_err(crate::HarnessError::Io)?;
    Ok(cwd)
}

fn home_or_current_dir_with(
    env: &impl Fn(&str) -> Option<OsString>,
    current_dir: &impl Fn() -> std::io::Result<PathBuf>,
) -> PathBuf {
    home_dir_with(env)
        .or_else(|| current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Accept a launchable executable override. The path must exist, so
/// `installed()` and the launch path agree on what an override means.
pub(crate) fn validate_native_override(path: &Path) -> Result<PathBuf, crate::HarnessError> {
    if !path.is_file() {
        return Err(crate::HarnessError::NotInstalled(format!(
            "{} does not exist",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

pub(crate) fn find_on_paths(exe: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    find_on_paths_matching_with(
        exe,
        extra,
        &|key| std::env::var_os(key),
        crate::shell_env::login_shell_path().map(OsString::from),
        |path| {
            if runnable(path) {
                candidates.push(path.to_path_buf());
            }
            false
        },
    );
    newest_candidate(candidates)
}

pub(crate) fn binary_hint(path: &Path) -> String {
    format!(
        "{} (version {})",
        path.display(),
        binary_version(path)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}

fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

fn newest_candidate(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    newest_candidate_with(candidates, binary_version)
}

fn newest_candidate_with(
    candidates: Vec<PathBuf>,
    version_of: impl Fn(&Path) -> Option<semver::Version>,
) -> Option<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())))
        .collect();
    let mut best = candidates.first()?.clone();
    if candidates.len() > 1 {
        let mut version = version_of(&best);
        for path in candidates.iter().skip(1) {
            let next = version_of(path);
            if next.as_ref().is_some_and(|next| {
                version
                    .as_ref()
                    .is_none_or(|current| next.cmp_precedence(current).is_gt())
            }) {
                best = path.clone();
                version = next;
            }
        }
    }
    Some(best)
}

/// Probe once per executable identity. Failures are cached too, including timeout.
/// Keep the lock during the short probe so concurrent descriptor requests coalesce.
pub fn binary_version(path: &Path) -> Option<semver::Version> {
    binary_version_with(path, std::time::Duration::from_secs(2), |start, timeout| {
        start.elapsed() >= timeout
    })
}

fn binary_version_with(
    path: &Path,
    timeout: std::time::Duration,
    mut expired: impl FnMut(std::time::Instant, std::time::Duration) -> bool,
) -> Option<semver::Version> {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        time::{Duration, SystemTime},
    };
    type Key = (PathBuf, Option<SystemTime>, u64);
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<semver::Version>>>> = OnceLock::new();
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    let key = (canonical, metadata.modified().ok(), metadata.len());
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(version) = cache.get(&key) {
        return version.clone();
    }
    let mut probe = || -> Option<semver::Version> {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let mut command = Command::new(path);
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().ok()?;
        fn reader(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.take(65536).read_to_end(&mut bytes);
                bytes
            })
        }
        let readers = [reader(child.stdout.take()?), reader(child.stderr.take()?)];
        let start = std::time::Instant::now();
        let success = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if !expired(start, timeout) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        if !success {
            return None;
        }
        let mut bytes = Vec::new();
        for reader in readers {
            while !reader.is_finished() && !expired(start, timeout) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !reader.is_finished() {
                return None;
            }
            bytes.extend(reader.join().ok()?);
            bytes.push(b' ');
        }
        parse_version(&bytes)
    };
    let version = probe();
    cache.retain(|(p, _, _), _| p != &key.0);
    cache.insert(key, version.clone());
    version
}

fn parse_version(bytes: &[u8]) -> Option<semver::Version> {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .find_map(|word| {
            semver::Version::parse(
                word.trim_matches(|c: char| matches!(c, '(' | ')' | ','))
                    .trim_start_matches('v'),
            )
            .ok()
        })
}

fn home_dir_with(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn node_version_manager_bins_with(env: &impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let home = home_dir_with(env);
    let mut dirs = Vec::new();

    let mut fnm_roots: Vec<PathBuf> = env("FNM_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(home) = &home {
        fnm_roots.push(home.join(".local").join("share").join("fnm"));
        fnm_roots.push(home.join("Library").join("Application Support").join("fnm"));
        fnm_roots.push(home.join(".fnm"));
    }
    for root in fnm_roots {
        dirs.push(root.join("aliases").join("default").join("bin"));
    }

    if let Some(home) = &home {
        dirs.push(home.join(".volta").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join("Library").join("pnpm"));
        dirs.push(home.join(".local").join("share").join("pnpm"));

        let nvm = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin"))
                .collect();
            versions.sort();
            versions.reverse();
            dirs.append(&mut versions);
        }
    }
    dirs
}

pub(crate) fn find_on_paths_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
) -> Option<PathBuf> {
    find_on_paths_matching_with(exe, extra, env, login_shell_path, |_| true)
}

pub(crate) fn find_on_paths_matching_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
    mut predicate: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let from_path = |path: OsString| {
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(exe))
            .collect::<Vec<_>>()
    };
    let mut candidates = env("PATH").map(from_path).unwrap_or_default();
    if let Some(shell_path) = login_shell_path {
        candidates.extend(from_path(shell_path));
    }
    candidates.extend(extra);
    candidates.extend(
        node_version_manager_bins_with(env)
            .into_iter()
            .map(|dir| dir.join(exe)),
    );

    candidates
        .into_iter()
        .find(|path| path.is_file() && predicate(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn semantic_versions_support_cli_labels_and_prereleases() {
        assert_eq!(
            parse_version(b"Claude Code v2.1.3 (native)"),
            Some(semver::Version::new(2, 1, 3))
        );
        assert!(
            parse_version(b"codex-cli 0.100.0-beta.2").unwrap() < semver::Version::new(0, 100, 0)
        );
        assert!(parse_version(b"unknown").is_none());
    }

    #[test]
    fn newest_binary_deduplicates_caches_and_invalidates() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one/codex");
        let second = dir.path().join("two/codex");
        let script = |path: &Path, version: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "#!/bin/sh\necho codex-cli {version}\necho x >> '{}.calls'\n",
                    path.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(&first, "0.99.0");
        script(&second, "0.110.0");
        let alias = dir.path().join("alias");
        symlink(&second, &alias).unwrap();
        for _ in 0..3 {
            assert_eq!(
                newest_candidate(vec![first.clone(), second.clone(), alias.clone()]),
                Some(second.clone())
            );
        }
        assert_eq!(
            std::fs::read_to_string(second.with_file_name("codex.calls")).unwrap(),
            "x\n"
        );
        script(&first, "1.200.0");
        assert_eq!(
            newest_candidate(vec![first.clone(), second.clone()]),
            Some(first.clone())
        );
        script(&second, "1.200.0");
        assert_eq!(newest_candidate(vec![first.clone(), second]), Some(first));
    }

    #[test]
    fn stderr_versions_are_read_and_build_metadata_does_not_break_ties() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for (path, version) in [(&first, "1.0.0+aaa"), (&second, "1.0.0+zzz")] {
            std::fs::write(path, format!("#!/bin/sh\necho codex-cli {version} >&2\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let probe = |path: &Path| {
            binary_version_with(
                path,
                std::time::Duration::from_secs(30),
                |start, timeout| start.elapsed() >= timeout,
            )
        };
        assert_eq!(probe(&first).unwrap().to_string(), "1.0.0+aaa");
        assert_eq!(
            newest_candidate_with(vec![first.clone(), second], probe),
            Some(first)
        );
    }

    #[test]
    fn version_probe_bounds_hangs_and_rejects_nonzero() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let failed = dir.path().join("failed");
        std::fs::write(&failed, "#!/bin/sh\necho 9.0.0; exit 1\n").unwrap();
        std::fs::set_permissions(&failed, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            binary_version_with(
                &failed,
                std::time::Duration::from_secs(30),
                |start, timeout| start.elapsed() >= timeout
            ),
            None
        );
        let hung = dir.path().join("hung");
        let ready = dir.path().join("ready");
        std::fs::write(
            &hung,
            format!(
                "#!/bin/sh\necho $$ > '{}'; exec sleep 60\n",
                ready.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hung, std::fs::Permissions::from_mode(0o755)).unwrap();
        let guard = std::time::Instant::now();
        let mut armed = false;
        assert_eq!(
            binary_version_with(&hung, std::time::Duration::from_secs(2), |_, timeout| {
                assert_eq!(timeout, std::time::Duration::from_secs(2));
                assert!(
                    guard.elapsed() < std::time::Duration::from_secs(30),
                    "child did not start"
                );
                armed = std::fs::read_to_string(&ready)
                    .is_ok_and(|pid| pid.trim().parse::<i32>().is_ok());
                armed
            }),
            None
        );
        assert!(armed);
        let pid: i32 = std::fs::read_to_string(ready)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "timed-out child must be reaped"
        );
        assert_eq!(newest_candidate(vec![hung.clone()]), Some(hung));
    }

    fn env(values: &[(&str, OsString)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let values: HashMap<String, OsString> = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        move |key| values.get(key).cloned()
    }

    fn joined(paths: &[&Path]) -> OsString {
        std::env::join_paths(paths).unwrap()
    }

    #[test]
    fn unix_pi_discovery_finds_node_manager_defaults_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let lookup = env(&[("HOME", home.as_os_str().to_owned())]);
        for relative in [
            ".volta/bin",
            ".bun/bin",
            ".local/share/pnpm",
            "Library/pnpm",
            ".nvm/versions/node/v22/bin",
            ".fnm/aliases/default/bin",
            ".local/share/fnm/aliases/default/bin",
            ".local/bin",
        ] {
            let bin = home.join(relative);
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("pi"), "fixture").unwrap();
            // Pi supplies ~/.local/bin as an explicit npm fallback.
            let extra = vec![home.join(".local/bin/pi")];
            assert_eq!(
                find_on_paths_with("pi", extra, &lookup, None),
                Some(bin.join("pi")),
                "{relative}"
            );
            std::fs::remove_file(bin.join("pi")).unwrap();
        }
    }

    #[test]
    fn unix_discovery_preserves_exact_name_and_source_order() {
        let temp = tempfile::tempdir().unwrap();
        let path_dir = temp.path().join("path");
        let shell_dir = temp.path().join("shell");
        let extra_dir = temp.path().join("extra");
        for dir in [&path_dir, &shell_dir, &extra_dir] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("agent"), b"shim").unwrap();
        }
        let found = find_on_paths_with(
            "agent",
            vec![extra_dir.join("agent")],
            &env(&[("PATH", joined(&[Path::new(""), &path_dir]))]),
            Some(joined(&[&shell_dir])),
        );
        assert_eq!(found, Some(path_dir.join("agent")));
    }

    #[test]
    fn native_overrides_require_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("agent.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        assert_eq!(validate_native_override(&script).unwrap(), script);
        let missing = temp.path().join("agent");
        let error = validate_native_override(&missing).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn base_dir_uses_current_dir_when_home_is_unset() {
        let current = PathBuf::from("/work/comet");
        assert_eq!(
            home_or_current_dir_with(&env(&[]), &|| Ok(current.clone())),
            current
        );
    }
}
