//! Where this device's Linear API key lives: the macOS Keychain (via
//! `security`, the same approach as `agent_accounts`), then the
//! `LINEAR_API_KEY` environment variable. Tests (and `MAST_LINEAR_KEY_STORE=
//! memory`) inject an in-memory store so nothing touches the real Keychain.
//!
//! The key is never logged: errors carry `security`'s stderr only, and the
//! value is passed to `security` as an argument (as `agent_accounts` does).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keychain service and account of the stored key.
pub const KEYCHAIN_SERVICE: &str = "computer.mikey.mast.linear";
const LEGACY_KEYCHAIN_SERVICE: &str = "sh.zeron.linear";
pub const KEYCHAIN_ACCOUNT: &str = "api-key";
/// Environment fallback (and the only source when no store holds a key).
pub const ENV_KEY: &str = "LINEAR_API_KEY";
/// `memory` forces the in-memory store (dev/test runs that must never
/// touch the Keychain); anything else is the platform default.
pub const STORE_OVERRIDE_ENV: &str = "MAST_LINEAR_KEY_STORE";

/// A persistent home for the key.
pub trait LinearKeyStore: Send + Sync {
    /// `Ok(None)` when nothing is stored.
    fn read(&self) -> Result<Option<String>, String>;
    fn write(&self, key: &str) -> Result<(), String>;
    /// Deleting an absent key succeeds.
    fn delete(&self) -> Result<(), String>;
    /// Whether this store is the platform Keychain (reported as `keychain`).
    fn is_keychain(&self) -> bool {
        true
    }
}

/// An in-memory store (tests, `MAST_LINEAR_KEY_STORE=memory`).
#[derive(Default)]
pub struct MemoryKeyStore {
    key: Mutex<Option<String>>,
    /// Calls by kind, for tests: `(reads, writes, deletes)`.
    calls: Mutex<(u32, u32, u32)>,
}

impl MemoryKeyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_key(key: &str) -> Arc<Self> {
        let store = Self::default();
        *store.key.lock().unwrap() = Some(key.to_owned());
        Arc::new(store)
    }

    /// The stored key (tests).
    pub fn stored(&self) -> Option<String> {
        self.key.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `(reads, writes, deletes)` so far.
    pub fn calls(&self) -> (u32, u32, u32) {
        *self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl LinearKeyStore for MemoryKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
        Ok(self.stored())
    }

    fn write(&self, key: &str) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).2 += 1;
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

/// The platform store: the macOS Keychain; elsewhere nothing persists (the
/// environment variable is the only source).
pub struct KeychainKeyStore;

// Keep migration independent of the security command so tests can supply a
// fake store. A failed copy is reported rather than silently losing it.
#[cfg(any(target_os = "macos", test))]
fn read_migrated_key(
    mut read: impl FnMut(&str) -> Result<Option<String>, String>,
    mut write: impl FnMut(&str, &str) -> Result<(), String>,
) -> Result<Option<String>, String> {
    if let Some(key) = read(KEYCHAIN_SERVICE)? {
        // An empty item records an explicit deletion and suppresses fallback.
        return Ok((!key.is_empty()).then_some(key));
    }
    let key = read(LEGACY_KEYCHAIN_SERVICE)?;
    if let Some(ref key) = key {
        write(KEYCHAIN_SERVICE, key)?;
    }
    Ok(key)
}

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    /// `security` blocks indefinitely on an unanswered consent dialog.
    const TIMEOUT: Duration = Duration::from_secs(15);

    fn run(args: &[&str]) -> Result<(bool, String, String), String> {
        use std::process::{Command, Stdio};
        let mut child = Command::new("security")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run security: {e}"))?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() > Self::TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Keychain did not answer (is a consent dialog open?)".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(format!("security failed: {e}")),
            }
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("security failed: {e}"))?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(target_os = "macos")]
impl LinearKeyStore for KeychainKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        read_migrated_key(Self::read_service, |_, key| self.write(key))
    }

    fn write(&self, key: &str) -> Result<(), String> {
        let (ok, _, stderr) = Self::run(&[
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-l",
            "Mast Linear API key",
            "-w",
            key,
        ])?;
        if ok {
            Ok(())
        } else {
            Err(format!("Keychain write failed: {stderr}"))
        }
    }

    fn delete(&self) -> Result<(), String> {
        // Retain the legacy item but prevent it from being restored on read.
        self.write("")
    }
}

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    fn read_service(service: &str) -> Result<Option<String>, String> {
        let (ok, stdout, stderr) = Self::run(&[
            "find-generic-password",
            "-s",
            service,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ])?;
        if ok {
            let key = stdout.trim().to_owned();
            return Ok(Some(key));
        }
        // errSecItemNotFound: "The specified item could not be found …".
        if stderr.contains("could not be found") {
            return Ok(None);
        }
        Err(format!("Keychain read failed: {stderr}"))
    }
}

#[cfg(not(target_os = "macos"))]
impl LinearKeyStore for KeychainKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn write(&self, _key: &str) -> Result<(), String> {
        Err("Storing a Linear API key needs macOS; set LINEAR_API_KEY instead".into())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The store a new engine uses: in-memory when
/// `MAST_LINEAR_KEY_STORE=memory`, else the Keychain.
pub fn default_store() -> Arc<dyn LinearKeyStore> {
    match std::env::var(STORE_OVERRIDE_ENV).as_deref() {
        Ok("memory") => MemoryKeyStore::new(),
        _ => Arc::new(KeychainKeyStore),
    }
}

/// Reads the environment fallback (injectable so tests ignore the
/// developer's own `LINEAR_API_KEY`).
pub type EnvKeySource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub fn process_env_source() -> EnvKeySource {
    Arc::new(|| {
        std::env::var(ENV_KEY)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

pub fn no_env_source() -> EnvKeySource {
    Arc::new(|| None)
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[test]
    fn migrates_only_when_current_service_is_absent() {
        let items = RefCell::new(HashMap::from([(
            LEGACY_KEYCHAIN_SERVICE.to_owned(),
            "legacy".to_owned(),
        )]));
        let read = || {
            read_migrated_key(
                |service| Ok(items.borrow().get(service).cloned()),
                |service, key| {
                    items
                        .borrow_mut()
                        .insert(service.to_owned(), key.to_owned());
                    Ok(())
                },
            )
        };
        assert_eq!(read().unwrap().as_deref(), Some("legacy"));
        assert_eq!(
            items.borrow().get(LEGACY_KEYCHAIN_SERVICE).unwrap(),
            "legacy"
        );
        items
            .borrow_mut()
            .insert(KEYCHAIN_SERVICE.into(), "current".into());
        assert_eq!(read().unwrap().as_deref(), Some("current"));
        items
            .borrow_mut()
            .insert(KEYCHAIN_SERVICE.into(), String::new());
        assert_eq!(read().unwrap(), None);
        items.borrow_mut().clear();
        assert_eq!(read().unwrap(), None);
    }

    #[test]
    fn reports_copy_failure_without_removing_legacy_item() {
        let result = read_migrated_key(
            |service| Ok((service == LEGACY_KEYCHAIN_SERVICE).then(|| "legacy".to_owned())),
            |_, _| Err("copy failed".into()),
        );
        assert_eq!(result.unwrap_err(), "copy failed");
    }
}
