//! Process-wide home isolation, installed before the test harness starts threads.

#[ctor::ctor(unsafe)]
fn isolate_test_home() {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    let original_home = std::env::var_os("MAST_TEST_ORIGINAL_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .expect("test isolation requires an original home directory");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    #[cfg(unix)]
    let temporary_root = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let temporary_root = std::env::temp_dir();
    assert!(
        !temporary_root.starts_with(std::path::Path::new(&original_home)),
        "test temporary directory is inside the original home"
    );
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    assert!(
        !temporary_root.starts_with(workspace),
        "test temporary directory is inside the workspace"
    );
    let home = temporary_root.join(format!("mast-test-home-{}-{timestamp}", std::process::id()));
    std::fs::create_dir(&home).expect("create isolated test home");

    // SAFETY: the constructor runs before the Rust test harness starts threads.
    unsafe {
        std::env::set_var("MAST_TEST_ORIGINAL_HOME", original_home);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
        std::env::set_var("CODEX_HOME", home.join(".codex"));
        std::env::set_var("MAST_DATA_DIR", home.join(".zeron"));
        std::env::set_var("ZERON_DATA_DIR", home.join(".zeron"));
    }
}
