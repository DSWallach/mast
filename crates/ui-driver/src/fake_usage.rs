//! The driver's usage backend, so a flow never reads the runner's sign-ins,
//! calls a provider or runs `agy`. Claude answers 62% used, Codex rejects its
//! sign-in, and Antigravity (`agy`) is never run, so it is unreadable.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::OnceLock;

use mast_engine::driver_hooks::{FakeUsage, use_fake_usage};

const CLAUDE_USAGE: &str = r#"{"five_hour":{"utilization":62.0,"resets_at":null}}"#;

/// One local server per process; it lives as long as the driver.
fn serve() -> std::io::Result<String> {
    static URL: OnceLock<String> = OnceLock::new();
    if let Some(url) = URL.get() {
        return Ok(url.clone());
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let (status, body) = if buffer[..read].starts_with(b"GET /claude ") {
                    (200, CLAUDE_USAGE)
                } else {
                    (401, "{}")
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    Ok(URL.get_or_init(|| url).clone())
}

/// Write the fake sign-ins under `data` and point every later engine at them.
pub(crate) fn install(data: &Path) -> std::io::Result<()> {
    let sign_ins = data.join("fake-sign-ins");
    for (file, contents) in [
        (
            "claude/.credentials.json",
            r#"{"claudeAiOauth":{"accessToken":"fake"}}"#,
        ),
        ("codex/auth.json", r#"{"tokens":{"access_token":"fake"}}"#),
    ] {
        let path = sign_ins.join(file);
        std::fs::create_dir_all(path.parent().expect("the path has a parent"))?;
        std::fs::write(path, contents)?;
    }
    use_fake_usage(FakeUsage {
        url: serve()?,
        sign_ins,
    });
    Ok(())
}
