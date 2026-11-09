//! Run first-launch UI checks in a separate process with a fresh profile.

pub(crate) fn isolated(test_name: &str) -> bool {
    const MARKER: &str = "MAST_TEST_FIRST_RUN_UI_ISOLATED";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let profile = tempfile::Builder::new()
        .prefix("mast-first-run-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let root = profile.path();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", root)
        .env("MAST_DATA_DIR", root.join("data"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_EXECUTABLE", root.join("missing-claude"))
        .env("CODEX_EXECUTABLE", root.join("missing-codex"))
        .env("MAST_NO_LOGIN_SHELL", "1")
        .env("TMPDIR", "/private/tmp")
        .env("PATH", root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "test filter did not match: {test_name}"
    );
    false
}
