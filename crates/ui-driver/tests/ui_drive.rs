//! B4: the warm driver (serve mode) and its file protocol, end to end.
//!
//! `drive_launcher` runs outside the sandbox: it allocates the scratch root
//! and runs `drive_serve` (this same binary) inside `run_isolated`.
//! `scripts/ui-drive` starts the launcher; the tests below start it the same
//! way and talk through `Client`, which is the protocol the CLI speaks.
#![cfg(target_os = "macos")]
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mast_ui_driver::{environment::ScratchDirectories, serve};
use serde_json::{Value, json};

fn account_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

/// Inside the sandbox: build the world, open the app, answer requests.
#[test]
#[ignore = "runs inside the driver sandbox; started by drive_launcher"]
fn drive_serve() {
    if let Err(error) = serve::serve() {
        panic!("driver failed: {error}");
    }
}

/// Outside the sandbox: `MAST_DRIVE_SESSION` is a directory the caller owns.
/// Writes `root` there, runs the sandboxed server, then `serve.log` and
/// `exited`, and removes the scratch root it created.
#[test]
#[ignore = "requires permission to apply a macOS sandbox"]
fn drive_launcher() {
    let session = PathBuf::from(std::env::var_os("MAST_DRIVE_SESSION").expect("MAST_DRIVE_SESSION"));
    let seed: u64 = std::env::var("MAST_DRIVE_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let preset = std::env::var("MAST_DRIVE_PRESET").unwrap_or_else(|_| "small".into());
    let render = std::env::var_os("MAST_DRIVE_RENDER").is_some();
    let account = account_home();
    let scratch = ScratchDirectories::create(&account).expect("scratch");
    let ctl = serve::control_dir(scratch.root());
    std::fs::create_dir(&ctl).unwrap();
    let config = serve::Config {
        preset,
        seed,
        render,
    };
    std::fs::write(ctl.join("config.json"), config.to_json().to_string()).unwrap();
    std::fs::write(session.join("root"), scratch.root().to_string_lossy().as_bytes()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--ignored",
        "--exact",
        "drive_serve",
        "--test-threads=1",
        "--nocapture",
    ];
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    let out = scratch.run_isolated(&account, &exe, &args);
    let log = match &out {
        Ok(out) => format!(
            "status: {}\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(error) => format!("could not start: {error}"),
    };
    std::fs::write(session.join("serve.log"), log).unwrap();
    std::fs::write(session.join("exited"), b"").unwrap();
    // The scratch root is ours: created above by mktemp.
    std::fs::remove_dir_all(scratch.root()).unwrap();
}

struct Driver {
    launcher: Child,
    session: PathBuf,
    ctl: PathBuf,
    next_id: u64,
}

impl Driver {
    fn start(seed: u64, render: bool) -> Self {
        let session = tempfile::tempdir().unwrap().keep();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "drive_launcher", "--test-threads=1"])
            .env("MAST_DRIVE_SESSION", &session)
            .env("MAST_DRIVE_SEED", seed.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if render {
            command.env("MAST_DRIVE_RENDER", "1");
        }
        let launcher = command.spawn().unwrap();
        let mut driver = Self {
            launcher,
            ctl: PathBuf::new(),
            session,
            next_id: 0,
        };
        let started = Instant::now();
        loop {
            if let Ok(root) = std::fs::read_to_string(driver.session.join("root"))
                && !root.is_empty()
            {
                let ctl = serve::control_dir(Path::new(&root));
                if ctl.join("ready").exists() {
                    driver.ctl = ctl;
                    return driver;
                }
            }
            if driver.session.join("exited").exists() {
                let log = std::fs::read_to_string(driver.session.join("serve.log")).unwrap_or_default();
                panic!("driver exited during start:\n{log}");
            }
            assert!(started.elapsed() < Duration::from_secs(300), "driver start timed out");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn call(&mut self, mut request: Value) -> Value {
        self.next_id += 1;
        request["id"] = json!(self.next_id);
        let response = self.ctl.join("response.json");
        let _ = std::fs::remove_file(&response);
        let tmp = self.ctl.join("request.tmp");
        std::fs::write(&tmp, request.to_string()).unwrap();
        std::fs::rename(&tmp, self.ctl.join("request.json")).unwrap();
        let started = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&response)
                && let Ok(value) = serde_json::from_str::<Value>(&text)
                && value["id"] == json!(self.next_id)
            {
                return value;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "request timed out: {request}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn tree(&mut self) -> Value {
        self.call(json!({"cmd": "tree"}))
    }
    fn click(&mut self, locator: &str) -> Value {
        self.call(json!({"cmd": "click", "locator": locator}))
    }

    fn stop(mut self) {
        self.call(json!({"cmd": "stop"}));
        let _ = self.launcher.wait();
        let log = std::fs::read_to_string(self.session.join("serve.log")).unwrap_or_default();
        assert!(log.contains("status: exit status: 0"), "serve exited badly:\n{log}");
        std::fs::remove_dir_all(&self.session).unwrap();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // A failed test must not leave a sandboxed server polling forever.
        if !self.ctl.as_os_str().is_empty() {
            let _ = std::fs::write(self.ctl.join("shutdown"), b"");
        }
        let _ = self.launcher.wait();
    }
}

fn tree_of(response: &Value) -> &str {
    response["tree"].as_str().expect("tree text")
}

fn selected_chat(response: &Value) -> String {
    let status = response["status"].as_str().unwrap();
    status
        .split_whitespace()
        .find_map(|part| part.strip_prefix("selected="))
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "requires permission to apply a macOS sandbox"]
fn select_chat_type_key_and_ambiguity() {
    let mut d = Driver::start(1, false);
    let first = d.tree();
    assert_eq!(first["ok"], json!(true));
    assert!(first["header"].as_str().unwrap().starts_with("preset=small seed=1 template="));
    assert!(tree_of(&first).contains("Review the workspace layout."));
    let before = selected_chat(&first);

    // Select another chat by role + name: the engine-backed selection changes
    // and that chat's transcript replaces the first one.
    let world = mast_ui_driver::world::SmallWorld::small(1);
    let target = world.chats.iter().find(|c| c.title == "Verify the changes").unwrap();
    let wanted = target
        .entries
        .iter()
        .flat_map(|e| e["parts"].as_array().cloned().unwrap_or_default())
        .find_map(|p| p["text"].as_str().map(str::to_owned))
        .expect("a text part");
    let clicked = d.click("button[name=\"Verify the changes\"]");
    assert_eq!(clicked["ok"], json!(true), "{clicked}");
    assert_ne!(selected_chat(&clicked), before);
    assert_eq!(selected_chat(&clicked), target.id);
    let snippet: String = wanted.chars().take(40).collect();
    assert!(tree_of(&clicked).contains(&snippet), "transcript missing {snippet:?}:\n{}", tree_of(&clicked));
    assert!(!tree_of(&clicked).contains("Review the workspace layout."));
    assert!(clicked["timing"]["draws"].as_u64().unwrap() >= 1);
    assert!(clicked["timing"]["nodes"].as_u64().unwrap() > 5);

    // Type into the composer; the value shows in the tree. Then one key.
    let typed = d.call(json!({"cmd": "type", "locator": "#composer-input", "text": "hello world"}));
    assert_eq!(typed["ok"], json!(true), "{typed}");
    assert!(tree_of(&typed).contains("value=\"hello world\""), "{}", tree_of(&typed));
    let keyed = d.call(json!({"cmd": "key", "chord": "backspace"}));
    assert!(tree_of(&keyed).contains("value=\"hello worl\""), "{}", tree_of(&keyed));

    // Ambiguous: two nodes contain "Verify"; an error lists both, nothing is clicked.
    let ambiguous = d.click("text=\"Verify\"");
    assert_eq!(ambiguous["ok"], json!(false));
    let error = ambiguous["error"].as_str().unwrap();
    assert!(error.contains("ambiguous") && error.matches("\n  e").count() >= 2, "{error}");
    // :nth picks one explicitly; unknown locators and bad syntax are errors too.
    assert_eq!(d.click("#nope")["ok"], json!(false));
    assert_eq!(d.click("???")["ok"], json!(false));
    d.stop();
}

#[test]
#[ignore = "requires permission to apply a macOS sandbox and Metal"]
fn png_is_written_and_same_seed_same_tree() {
    let mut a = Driver::start(7, true);
    let tree_a = a.tree();
    let png = a.call(json!({"cmd": "png"}));
    assert_eq!(png["ok"], json!(true), "{png}");
    let bytes = std::fs::read(png["message"].as_str().unwrap()).unwrap();
    assert!(bytes.starts_with(b"\x89PNG") && bytes.len() > 10_000, "png is {} bytes", bytes.len());
    a.stop();
    // A run without the renderer sees the same tree: the gate never needs Metal.
    let mut b = Driver::start(7, false);
    let tree_b = b.tree();
    assert_eq!(tree_a["tree_hash"], tree_b["tree_hash"]);
    assert_eq!(tree_of(&tree_a), tree_of(&tree_b));
    let refused = b.call(json!({"cmd": "png"}));
    assert_eq!(refused["ok"], json!(false), "png without --render must fail closed");
    b.stop();
}
