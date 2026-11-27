//! The codex "Add account" flow must launch the same executable the harness
//! runs — `CODEX_EXECUTABLE` and PATH discovery included — instead of a bare
//! `Command::new("codex")`.
//!
//! Child-process probes: resolution reads process env (PATH, the override),
//! so each case re-runs this test binary with a controlled environment.

#![allow(clippy::disallowed_methods)] // spawn sites here are tests/build scripts (post-mortem P3)
use std::path::{Path, PathBuf};

use mast_engine::{AgentAccounts, AgentAccountsConfig};
use mast_proto::{AgentLoginMode, HarnessId};

fn test_accounts(root: &Path) -> AgentAccounts {
    let config = AgentAccountsConfig {
        data_dir: root.join("data"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
        cursor_sdk_auth_file: root.join("cursor-sdk").join("auth.json"),
    };
    AgentAccounts::new(config)
}

const FAKE_URL: &str = "https://auth.openai.com/authorize?probe=fake";

/// The fake CLI prints the authorize URL and then parks so the login flow
/// stays alive until the probe cancels it.
fn fake_codex(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-codex");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho \"open {FAKE_URL} in your browser\"\nexec /bin/sleep 30\n"),
    )
    .expect("fake codex");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn login_child() {
    let Ok(expected) = std::env::var("CODEX_LOGIN_EXPECTED") else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zeron-codex-login-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let accounts = test_accounts(&root);
    let result = accounts.start_login(HarnessId::Codex).await;
    if expected == "true" {
        let start = result.expect("login starts");
        assert_eq!(start.mode, AgentLoginMode::Browser);
        assert_eq!(start.url, FAKE_URL);
        accounts.cancel_login(&start.login_id);
    } else {
        let error = result
            .expect_err("login must fail without a CLI")
            .to_string();
        assert!(
            error.contains("codex"),
            "error should name the missing CLI: {error}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn probe(path: &Path, override_path: Option<&Path>, expected: bool) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "codex_login_resolution::login_child", "--nocapture"]);
    for key in [
        "HOME",
        "FNM_DIR",
        "FNM_MULTISHELL_PATH",
        "NVM_SYMLINK",
        "VOLTA_HOME",
        "PNPM_HOME",
        "CODEX_EXECUTABLE",
    ] {
        command.env_remove(key);
    }
    command
        .env("PATH", path)
        .env("CODEX_LOGIN_EXPECTED", expected.to_string())
        .env("MAST_NO_LOGIN_SHELL", "1");
    if let Some(path) = override_path {
        command.env("CODEX_EXECUTABLE", path);
    }
    let output = command.output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "child did not run exactly one test: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An installation reachable only through `CODEX_EXECUTABLE` must log in —
/// the exact regression where Add account failed while the harness ran.
#[test]
fn codex_login_honors_override_only_installation() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_codex(dir.path());
    // No codex anywhere on PATH: only the override can start the login.
    probe(dir.path(), Some(&fake), true);
}

#[test]
fn codex_login_without_any_cli_fails_with_a_named_error() {
    if std::path::Path::new("/opt/homebrew/bin/codex").exists()
        || std::path::Path::new("/usr/local/bin/codex").exists()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    probe(dir.path(), None, false);
}
