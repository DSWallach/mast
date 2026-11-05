//! Process-wide home isolation, installed before the test harness starts threads.

#[ctor::ctor(unsafe)]
fn isolate_test_home() {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    let original_home = std::env::var_os("MAST_TEST_ORIGINAL_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .expect("test isolation requires an original home directory");
    // A test may re-execute its own binary with a deliberately selected fake
    // HOME or with individual config variables unset. Preserve that fixture
    // when the inherited isolation marker is present and its paths are safe.
    if std::env::var_os("MAST_TEST_ORIGINAL_HOME").is_some() {
        if let Some(inherited_home) = std::env::var_os("HOME") {
            assert!(
                !std::path::Path::new(&inherited_home)
                    .starts_with(std::path::Path::new(&original_home)),
                "inherited test HOME reaches the original home"
            );
            let original = std::fs::canonicalize(&original_home)
                .unwrap_or_else(|_| PathBuf::from(&original_home));
            let inherited = std::fs::canonicalize(&inherited_home)
                .unwrap_or_else(|_| PathBuf::from(&inherited_home));
            assert!(
                inherited.is_absolute() && !inherited.starts_with(&original),
                "inherited test HOME reaches the original home"
            );
            for key in [
                "CLAUDE_CONFIG_DIR",
                "CODEX_HOME",
                "MAST_DATA_DIR",
                "ZERON_DATA_DIR",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    assert!(
                        !std::path::Path::new(&value)
                            .starts_with(std::path::Path::new(&original_home)),
                        "{key} reaches the original home"
                    );
                    let path =
                        std::fs::canonicalize(&value).unwrap_or_else(|_| PathBuf::from(&value));
                    assert!(
                        path.is_absolute() && !path.starts_with(&original),
                        "{key} reaches the original home"
                    );
                }
            }
            return;
        }
    }
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
    // The `codex` CLI refuses to start when CODEX_HOME names a missing directory.
    std::fs::create_dir(home.join(".codex")).expect("create isolated CODEX_HOME");

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
