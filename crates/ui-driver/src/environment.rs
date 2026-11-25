//! Validation of the filesystem boundary for a driver run.
//!
//! All paths must already exist. Canonicalization is deliberately mandatory: a
//! missing directory or an unresolved symlink is not a safe startup state.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct EnvironmentPaths {
    pub root: PathBuf,
    pub home: PathBuf,
    pub data: PathBuf,
    pub temporary: PathBuf,
}

impl EnvironmentPaths {
    /// Validate paths against the account home captured before clearing the environment.
    pub fn validate(
        root: &Path,
        home: &Path,
        data: &Path,
        temporary: &Path,
        account_home: &Path,
    ) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let home = home.canonicalize()?;
        let data = data.canonicalize()?;
        let temporary = temporary.canonicalize()?;
        let account_home = account_home.canonicalize()?;
        let invalid = |message| io::Error::new(io::ErrorKind::PermissionDenied, message);

        if root.starts_with(&account_home) || account_home.starts_with(&root) {
            return Err(invalid("scratch root overlaps account home"));
        }
        for path in [&home, &data, &temporary] {
            if path == &root || !path.starts_with(&root) || path.starts_with(&account_home) {
                return Err(invalid("environment path is outside scratch root"));
            }
        }
        if home == data
            || home == temporary
            || data == temporary
            || home.starts_with(&data)
            || home.starts_with(&temporary)
            || data.starts_with(&home)
            || data.starts_with(&temporary)
            || temporary.starts_with(&home)
            || temporary.starts_with(&data)
        {
            return Err(invalid("environment paths overlap"));
        }
        Ok(Self {
            root,
            home,
            data,
            temporary,
        })
    }
}

/// Directories allocated for a single run. Allocation alone does not establish
/// a sandbox; callers must not start the driver until isolation is verified.
#[derive(Debug)]
pub struct ScratchDirectories {
    paths: EnvironmentPaths,
    repositories: PathBuf,
    shims: PathBuf,
    stub_log: PathBuf,
}

impl ScratchDirectories {
    pub fn create(account_home: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-drive.XXXXXX"])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("could not allocate scratch root"));
        }
        let output = String::from_utf8(output.stdout)
            .map_err(|_| io::Error::other("mktemp returned a non-UTF-8 path"))?;
        let root = PathBuf::from(output.trim()).canonicalize()?;
        let home = root.join("home");
        let data = root.join("data");
        let temporary = root.join("tmp");
        let repositories = root.join("repos");
        let shims = root.join("shims");
        for path in [&home, &data, &temporary, &repositories, &shims] {
            std::fs::create_dir(path)?;
        }
        let paths = EnvironmentPaths::validate(&root, &home, &data, &temporary, account_home)?;
        let stub_log = root.join("stub.log");
        std::fs::write(&stub_log, b"")?;
        // Real git runs under the outer sandbox, whose write rule confines every
        // write to the scratch root. The shim only logs and execs it.
        let git_log = root.join("git.log");
        std::fs::write(&git_log, b"")?;
        let git_shim = shims.join("git");
        let git_log_text = git_log
            .to_str()
            .filter(|text| {
                !text
                    .chars()
                    .any(|c| c == '"' || c == '\\' || c == '$' || c == '`' || c.is_control())
            })
            .ok_or_else(|| io::Error::other("unsafe git log path"))?;
        std::fs::write(
            &git_shim,
            include_str!("isolation/git_shim.sh").replace("@GIT_LOG@", git_log_text),
        )?;
        std::fs::set_permissions(&git_shim, std::fs::Permissions::from_mode(0o700))?;
        for program in [
            "gh", "claude", "codex", "security", "npm", "node", "curl", "ssh",
        ] {
            let path = shims.join(program);
            // The log location comes from the cleared child environment, not a
            // caller-supplied command argument.
            std::fs::write(&path, b"#!/bin/sh\nprintf '%s\\n' \"${0##*/}\" >> \"${MAST_DRIVER_STUB_LOG:?}\"\necho 'driver: command disabled' >&2\nexit 127\n")?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            paths,
            repositories,
            shims,
            stub_log,
        })
    }

    /// Rebuild the handle inside a process the startup guard already started
    /// (`MAST_DRIVER_ROOT` and the account home come from the guarded
    /// environment). Fails closed unless every path validates.
    pub fn from_env() -> io::Result<Self> {
        let var = |name: &str| {
            std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{name} is not set; run through the driver startup guard"),
                )
            })
        };
        let root = var("MAST_DRIVER_ROOT")?;
        let account = var("MAST_DRIVER_ACCOUNT_HOME")?;
        let paths = EnvironmentPaths::validate(
            &root,
            &root.join("home"),
            &root.join("data"),
            &root.join("tmp"),
            &account,
        )?;
        let base = paths.root.clone();
        Ok(Self {
            repositories: base.join("repos"),
            shims: base.join("shims"),
            stub_log: base.join("stub.log"),
            paths,
        })
    }

    pub fn root(&self) -> &Path {
        &self.paths.root
    }
    pub fn home(&self) -> &Path {
        &self.paths.home
    }
    pub fn data(&self) -> &Path {
        &self.paths.data
    }
    pub fn temporary(&self) -> &Path {
        &self.paths.temporary
    }
    pub fn repositories(&self) -> &Path {
        &self.repositories
    }
    pub fn git_log(&self) -> PathBuf {
        self.paths.root.join("git.log")
    }
    pub fn stub_log(&self) -> &Path {
        &self.stub_log
    }

    /// Run git through the logging shim with a cleared environment. The caller
    /// is responsible for being inside the sandbox; the shim logs and execs.
    pub fn git(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> io::Result<std::process::Output> {
        let path = std::env::join_paths([
            self.shims.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        std::process::Command::new(self.shims.join("git"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &self.paths.home)
            .env("TMPDIR", &self.paths.temporary)
            .env("PATH", path)
            .envs(env.iter().copied())
            .output()
    }

    /// Copy a template data directory into an empty run directory. Symlinks are
    /// rejected rather than followed, including links back to the template.
    #[cfg(test)]
    pub(crate) fn copy_template_data(&self, template: &ScratchDirectories) -> io::Result<()> {
        if self.paths.root == template.paths.root
            || std::fs::read_dir(&self.paths.data)?.next().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "run data directory must be empty and distinct",
            ));
        }
        clone_directory(&template.paths.data, &self.paths.data)
    }

    /// Start a command only after the sandbox and startup guard are active.
    pub fn run_isolated(
        &self,
        account_home: &Path,
        executable: &Path,
        arguments: &[&std::ffi::OsStr],
    ) -> io::Result<std::process::Output> {
        if !executable.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "executable must be absolute",
            ));
        }
        EnvironmentPaths::validate(
            &self.paths.root,
            &self.paths.home,
            &self.paths.data,
            &self.paths.temporary,
            account_home,
        )?;
        let guard = self.paths.root.join("startup_guard.py");
        std::fs::write(&guard, include_str!("isolation/startup_guard.py"))?;
        let path = std::env::join_paths([
            self.shims.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
            Path::new("/usr/sbin"),
            Path::new("/sbin"),
        ])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        #[cfg(target_os = "macos")]
        let (mut command, executable) = {
            let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
            command.arg("-f").arg(self.write_sandbox_profile(account_home)?);
            (command, executable.to_owned())
        };
        #[cfg(target_os = "linux")]
        let driver = linux_lavapipe()?;
        #[cfg(target_os = "linux")]
        let (mut command, executable) = self.linux_command(account_home, executable, driver.as_deref())?;
        command
            .arg("/usr/bin/python3")
            .arg(guard)
            .arg(executable)
            .args(arguments)
            .current_dir(&self.repositories)
            .env_clear()
            .env("HOME", &self.paths.home)
            .env("MAST_DATA_DIR", &self.paths.data)
            .env("MAST_DEVICE_NAME", "Fixture device")
            .env("TMPDIR", &self.paths.temporary)
            .env("MAST_DRIVER_ROOT", &self.paths.root)
            .env("MAST_DRIVER_ACCOUNT_HOME", account_home.canonicalize()?)
            // Test binaries re-exec inside this sandbox. Preserve the already
            // validated scratch HOME instead of allocating another outside it.
            .env("MAST_TEST_ORIGINAL_HOME", account_home)
            .env("MAST_DRIVER_STUB_LOG", &self.stub_log)
            .env("MAST_SOAK", "1")
            .env("MAST_SOAK_SHIM_DIR", &self.shims)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("RUST_BACKTRACE", "1")
            .env("PATH", path);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt as _;
            let namespace = std::fs::metadata("/proc/self/ns/net")?.ino();
            command.env("MAST_SOAK_HOST_NETNS", namespace.to_string());
            // The process has no display service, GPU device or caller XDG settings.
            // Give the loader only the same software driver used by ui-preview.
            command
                .env("VK_DRIVER_FILES", driver.as_deref().unwrap_or(Path::new("/dev/null")))
                .env("VK_ICD_FILENAMES", driver.as_deref().unwrap_or(Path::new("/dev/null")));
        }
        command.stdin(std::process::Stdio::null()).output()
    }

    #[cfg(target_os = "linux")]
    fn linux_command(
        &self,
        account_home: &Path,
        executable: &Path,
        driver: Option<&Path>,
    ) -> io::Result<(std::process::Command, PathBuf)> {
        let mut command = std::process::Command::new("/usr/bin/bwrap");
        command.args([
            "--unshare-all", "--unshare-user", "--disable-userns", "--cap-drop", "ALL",
            "--die-with-parent", "--new-session", "--proc", "/proc", "--dev", "/dev",
            "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin",
            "--symlink", "usr/sbin", "/sbin", "--symlink", "usr/lib", "/lib",
        ]);
        if Path::new("/usr/lib64").is_dir() {
            command.args(["--symlink", "usr/lib64", "/lib64"]);
        }
        for path in ["/etc/passwd", "/etc/fonts", "/etc/ld.so.cache", "/etc/localtime"] {
            command.args(["--ro-bind-try", path, path]);
        }
        // Keep the identity path available for validation without mounting its
        // contents. Reject any layout that would place it in a system mount.
        let account = account_home.canonicalize()?;
        if ["/usr", "/etc", "/proc", "/dev", "/bin", "/sbin", "/lib", "/lib64"]
            .iter()
            .any(|path| account.starts_with(path) || Path::new(path).starts_with(&account))
        {
            return Err(io::Error::other("account home overlaps a system mount"));
        }
        if let Some(driver) = driver.filter(|path| path.starts_with("/etc")) {
            command.arg("--ro-bind").arg(driver).arg(driver);
        }
        let sandbox_executable = self.paths.root.join("driver-executable");
        command
            .arg("--dir").arg(account)
            .arg("--bind").arg(&self.paths.root).arg(&self.paths.root)
            .arg("--ro-bind").arg(executable).arg(&sandbox_executable)
            .args(["--remount-ro", "/"])
            .arg("--chdir").arg(&self.repositories)
            .arg("--");
        Ok((command, sandbox_executable))
    }

    /// Write a restrictive profile for sandbox-exec. This does not itself
    /// enter the sandbox or prove that the profile was enforced.
    #[cfg(target_os = "macos")]
    pub fn write_sandbox_profile(&self, account_home: &Path) -> io::Result<PathBuf> {
        fn literal(path: &Path) -> io::Result<String> {
            let value = path
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 path"))?;
            if value.chars().any(char::is_control) {
                return Err(io::Error::other("control character in sandbox path"));
            }
            Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let root = literal(&self.paths.root)?;
        let account_home = account_home.canonicalize()?;
        let mut profile = format!(
            "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write* (subpath \"{root}\") (literal \"/dev/null\"))\n(deny file-read*\n"
        );
        for name in [
            ".zeron",
            ".mast",
            ".claude",
            ".codex",
            ".config/gh",
            "Library/Keychains",
        ] {
            profile.push_str(&format!(
                "  (subpath \"{}\")\n",
                literal(&account_home.join(name))?
            ));
        }
        profile.push_str(")\n(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\"))\n");
        let path = self.paths.root.join("sandbox.sb");
        std::fs::write(&path, profile)?;
        Ok(path)
    }
}

#[cfg(target_os = "linux")]
fn linux_lavapipe() -> io::Result<Option<PathBuf>> {
    let mut fallback = None;
    let preferred = format!("lvp_icd.{}.json", std::env::consts::ARCH);
    for directory in ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d", "/usr/local/share/vulkan/icd.d"] {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == preferred && entry.path().is_file() {
                return Ok(Some(entry.path()));
            }
            if name.starts_with("lvp_icd") && name.ends_with(".json")
                && !name.contains("i686") && entry.path().is_file()
            {
                fallback.get_or_insert_with(|| entry.path());
            }
        }
    }
    Ok(fallback)
}

fn no_links(source: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            no_links(&entry.path())?;
        } else if !kind.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "template contains a link or special file",
            ));
        }
    }
    Ok(())
}

pub(crate) fn clone_directory(from: &Path, to: &Path) -> io::Result<()> {
    no_links(from)?;
    #[cfg(target_os = "macos")]
    let args = ["-c", "-R"];
    #[cfg(target_os = "linux")]
    let args = ["--reflink=auto", "-R"];
    let status = std::process::Command::new("/bin/cp")
        .args(args)
        .arg(from.join("."))
        .arg(to)
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("template clone failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn creates_scratch_directories_and_fail_closed_stubs() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let scratch = ScratchDirectories::create(&account).unwrap();
        #[cfg(target_os = "macos")]
        {
            let profile = fs::read_to_string(scratch.write_sandbox_profile(&account).unwrap()).unwrap();
            assert!(profile.contains("(deny network*)"));
            assert!(profile.contains(".claude"));
        }
        for program in [
            "gh", "claude", "codex", "security", "npm", "node", "curl", "ssh",
        ] {
            let status = std::process::Command::new(scratch.shims.join(program))
                .env_clear()
                .env("MAST_DRIVER_STUB_LOG", &scratch.stub_log)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(127));
        }
        assert_eq!(
            fs::read_to_string(&scratch.stub_log).unwrap(),
            "gh\nclaude\ncodex\nsecurity\nnpm\nnode\ncurl\nssh\n"
        );
        fs::remove_dir_all(&scratch.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    #[test]
    fn git_shim_logs_and_commits_inside_repos() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let scratch = ScratchDirectories::create(&account).unwrap();
        let run = |cwd: &Path, args: &[&str]| scratch.git(cwd, args, &[]).unwrap();
        let repo = scratch.repositories.join("space");
        fs::create_dir(&repo).unwrap();
        assert!(run(&repo, &["init", "-q", "-b", "main"]).status.success());
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        assert!(run(&repo, &["add", "a.txt"]).status.success());
        let commit = run(&repo, &["commit", "-q", "-m", "first"]);
        assert!(
            commit.status.success(),
            "{}",
            String::from_utf8_lossy(&commit.stderr)
        );
        let head = run(&repo, &["log", "-1", "--format=%an <%ae>"]);
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            "Driver fixture <driver@example.invalid>"
        );
        let log = fs::read_to_string(scratch.git_log()).unwrap();
        assert!(log.contains("git init -q -b main\n") && log.contains("git commit -q -m first\n"));
        assert_eq!(log.lines().count(), 4);
        fs::remove_dir_all(&scratch.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    /// Real git under the sandbox profile: a commit in repos/ works and a write
    /// to a fake target beside the scratch root (same parent, not a user path)
    /// is denied.
    #[test]
    #[ignore = "requires an available host sandbox"]
    fn sandboxed_git_commits_in_repos_and_cannot_write_outside() {
        let account = PathBuf::from(std::env::var_os("MAST_TEST_ORIGINAL_HOME").unwrap());
        let scratch = ScratchDirectories::create(&account).unwrap();
        let git = scratch.shims.join("git");
        let run = |args: &[&str]| {
            let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
            scratch.run_isolated(&account, &git, &args).unwrap()
        };
        let init = run(&["init", "-q", "-b", "main", "space"]);
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let commit = run(&[
            "-C",
            "space",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        assert!(
            commit.status.success(),
            "{}",
            String::from_utf8_lossy(&commit.stderr)
        );
        assert!(scratch.repositories.join("space/.git").is_dir());
        let outside = scratch.paths.root.with_file_name(format!(
            "{}-outside",
            scratch.paths.root.file_name().unwrap().to_string_lossy()
        ));
        let denied = run(&["init", "-q", outside.to_str().unwrap()]);
        assert!(!denied.status.success());
        assert!(!outside.exists());
        let log = fs::read_to_string(scratch.git_log()).unwrap();
        assert_eq!(log.lines().count(), 3);
        assert!(fs::read(&scratch.stub_log).unwrap().is_empty());
        fs::remove_dir_all(&scratch.paths.root).unwrap();
    }

    /// Requires permission to apply a macOS sandbox (the agent sandbox may
    /// reject nested sandbox-exec). Run separately from the normal unit suite.
    #[test]
    #[ignore = "requires an available host sandbox"]
    fn isolated_command_passes_guard_without_invoking_stubs() {
        let account = PathBuf::from(std::env::var_os("MAST_TEST_ORIGINAL_HOME").unwrap());
        let scratch = ScratchDirectories::create(&account).unwrap();
        let output = scratch
            .run_isolated(&account, Path::new("/usr/bin/true"), &[])
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(fs::read(&scratch.stub_log).unwrap().is_empty());
        fs::remove_dir_all(&scratch.paths.root).unwrap();
    }

    #[test]
    #[ignore = "requires an available host sandbox"]
    #[cfg(target_os = "macos")]
    fn sandbox_denies_fake_account_reads_and_external_writes() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        fs::create_dir(account.join(".claude")).unwrap();
        fs::write(account.join(".claude/fixture"), "private").unwrap();
        let scratch = ScratchDirectories::create(&account).unwrap();
        let profile = scratch.write_sandbox_profile(&account).unwrap();
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(profile)
            .args([
                "/bin/sh",
                "-c",
                r#"
                /usr/bin/touch "$DRIVER_ROOT/allowed" || exit 1
                if /usr/bin/touch "$FAKE_ACCOUNT/denied" 2>/dev/null; then exit 2; fi
                if /bin/cat "$FAKE_ACCOUNT/.claude/fixture" >/dev/null 2>&1; then exit 3; fi
            "#,
            ])
            .env_clear()
            .env("DRIVER_ROOT", &scratch.paths.root)
            .env("FAKE_ACCOUNT", &account)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!account.join("denied").exists());
        fs::remove_dir_all(&scratch.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    #[test]
    fn template_copy_starts_each_run_clean() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let template = ScratchDirectories::create(&account).unwrap();
        let first = ScratchDirectories::create(&account).unwrap();
        let second = ScratchDirectories::create(&account).unwrap();
        fs::write(template.paths.data.join("fixture"), "seed").unwrap();
        first.copy_template_data(&template).unwrap();
        fs::write(first.paths.data.join("fixture"), "changed").unwrap();
        second.copy_template_data(&template).unwrap();
        assert_eq!(
            fs::read_to_string(second.paths.data.join("fixture")).unwrap(),
            "seed"
        );
        assert!(first.copy_template_data(&template).is_err());
        fs::remove_dir_all(&template.paths.root).unwrap();
        fs::remove_dir_all(&first.paths.root).unwrap();
        fs::remove_dir_all(&second.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    #[test]
    fn template_copy_rejects_symlinks() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-account-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let account = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let template = ScratchDirectories::create(&account).unwrap();
        let run = ScratchDirectories::create(&account).unwrap();
        fs::write(account.join("private"), "secret").unwrap();
        symlink(account.join("private"), template.paths.data.join("alias")).unwrap();
        assert!(run.copy_template_data(&template).is_err());
        assert!(!run.paths.data.join("alias").exists());
        fs::remove_dir_all(&template.paths.root).unwrap();
        fs::remove_dir_all(&run.paths.root).unwrap();
        fs::remove_dir_all(account).unwrap();
    }

    #[test]
    fn rejects_account_paths_and_symlink_aliases() {
        let output = std::process::Command::new("/usr/bin/mktemp")
            .args(["-d", "/tmp/mast-driver-guard-test.XXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let base = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let account = base.join("account");
        let root = base.join("scratch");
        for path in [
            &account,
            &root,
            &root.join("home"),
            &root.join("data"),
            &root.join("tmp"),
        ] {
            fs::create_dir(path).unwrap();
        }
        for name in [".zeron", ".mast", ".claude", ".codex"] {
            fs::create_dir(account.join(name)).unwrap();
        }
        let home = root.join("home");
        let data = root.join("data");
        let temporary = root.join("tmp");
        assert!(EnvironmentPaths::validate(&root, &home, &data, &temporary, &account).is_ok());
        for name in [".zeron", ".mast", ".claude", ".codex"] {
            let protected = account.join(name);
            assert!(
                EnvironmentPaths::validate(&root, &home, &protected, &temporary, &account).is_err()
            );
            let alias = root.join(format!("alias-{name}"));
            symlink(&protected, &alias).unwrap();
            assert!(
                EnvironmentPaths::validate(&root, &alias, &data, &temporary, &account).is_err()
            );
        }
        assert!(EnvironmentPaths::validate(&root, &account, &data, &temporary, &account).is_err());
        fs::remove_dir_all(&base).unwrap();
    }
}
