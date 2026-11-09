#![cfg(unix)]

#[test]
fn doctor_reports_missing_prerequisites_with_fixes() {
    let profile = tempfile::Builder::new()
        .prefix("mast-first-run-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let root = profile.path();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mast"))
        .args(["doctor", "--json"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("MAST_DATA_DIR", root.join("data"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_EXECUTABLE", root.join("missing-claude"))
        .env("CODEX_EXECUTABLE", root.join("missing-codex"))
        .env("MAST_NO_LOGIN_SHELL", "1")
        .env("PATH", root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["exit_code"], 1);
    let checks = report["checks"].as_array().unwrap();
    for id in [
        "claude",
        "codex",
        "agent CLI",
        "gh",
        "git",
        "cargo",
        "rustc",
    ] {
        let check = checks.iter().find(|check| check["id"] == id).unwrap();
        assert_ne!(check["status"], "ok", "{check}");
        assert!(
            check["fix"].as_str().is_some_and(|fix| !fix.is_empty()),
            "{check}"
        );
    }
    let agent = checks
        .iter()
        .find(|check| check["id"] == "agent CLI")
        .unwrap();
    assert_eq!(agent["status"], "error");
    assert!(agent["fix"].as_str().unwrap().contains("sign in"));
}
