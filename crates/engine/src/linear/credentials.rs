//! Where this device's Linear API key lives: the macOS Keychain (via
//! `security`, the same approach as `agent_accounts`), then the
//! `LINEAR_API_KEY` environment variable. Tests (and `MAST_LINEAR_KEY_STORE=
//! memory`) inject an in-memory store so nothing touches the real Keychain.
//!
//! The key is never logged: errors carry `security`'s stderr only, and the
//! value is passed to `security` as an argument (as `agent_accounts` does).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keychain service and account of the stored key.
pub const KEYCHAIN_SERVICE: &str = "computer.mikey.mast.linear";
pub const KEYCHAIN_ACCOUNT: &str = "api-key";
/// Stable identifier for the existing device credential.
pub const DEFAULT_ACCOUNT_ID: &str = "default";

fn keychain_account(account_id: &str) -> &str {
    if account_id == DEFAULT_ACCOUNT_ID {
        KEYCHAIN_ACCOUNT
    } else {
        account_id
    }
}
fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty() || account_id == KEYCHAIN_ACCOUNT {
        Err("Invalid Linear account identifier".into())
    } else {
        Ok(())
    }
}

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
    /// Read a named account without exposing any other account's credential.
    fn read_account(&self, account_id: &str) -> Result<Option<String>, String> {
        if account_id == DEFAULT_ACCOUNT_ID {
            self.read()
        } else {
            Err("Named Linear accounts are not supported by this store".into())
        }
    }
    fn write_account(&self, account_id: &str, key: &str) -> Result<(), String> {
        if account_id == DEFAULT_ACCOUNT_ID {
            self.write(key)
        } else {
            Err("Named Linear accounts are not supported by this store".into())
        }
    }
    fn delete_account(&self, account_id: &str) -> Result<(), String> {
        if account_id == DEFAULT_ACCOUNT_ID {
            self.delete()
        } else {
            Err("Named Linear accounts are not supported by this store".into())
        }
    }
    /// Whether this store is the platform Keychain (reported as `keychain`).
    fn is_keychain(&self) -> bool {
        true
    }
}

/// An in-memory store (tests, `MAST_LINEAR_KEY_STORE=memory`).
#[derive(Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, String>>,
    /// Calls by kind, for tests: `(reads, writes, deletes)`.
    calls: Mutex<(u32, u32, u32)>,
}

impl MemoryKeyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_key(key: &str) -> Arc<Self> {
        let store = Self::default();
        store
            .keys
            .lock()
            .unwrap()
            .insert(DEFAULT_ACCOUNT_ID.into(), key.to_owned());
        Arc::new(store)
    }

    /// The stored key (tests).
    pub fn stored(&self) -> Option<String> {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(DEFAULT_ACCOUNT_ID)
            .cloned()
    }

    /// `(reads, writes, deletes)` so far.
    pub fn calls(&self) -> (u32, u32, u32) {
        *self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl LinearKeyStore for MemoryKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        self.read_account(DEFAULT_ACCOUNT_ID)
    }
    fn write(&self, key: &str) -> Result<(), String> {
        self.write_account(DEFAULT_ACCOUNT_ID, key)
    }
    fn delete(&self) -> Result<(), String> {
        self.delete_account(DEFAULT_ACCOUNT_ID)
    }

    fn read_account(&self, account_id: &str) -> Result<Option<String>, String> {
        validate_account_id(account_id)?;
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
        Ok(self
            .keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned())
    }
    fn write_account(&self, account_id: &str, key: &str) -> Result<(), String> {
        validate_account_id(account_id)?;
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_owned(), key.to_owned());
        Ok(())
    }
    fn delete_account(&self, account_id: &str) -> Result<(), String> {
        validate_account_id(account_id)?;
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).2 += 1;
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account_id);
        Ok(())
    }
}

/// The platform store: the macOS Keychain; elsewhere nothing persists (the
/// environment variable is the only source).
pub struct KeychainKeyStore;

// Keep Keychain lookup independent of the security command so tests can supply
// a fake store and assert that only the Mast Keychain service is queried.
#[cfg(any(target_os = "macos", test))]
fn read_mast_key(
    mut read: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    // Treat an empty item left by an older deletion tombstone as absent.
    Ok(read(KEYCHAIN_SERVICE)?.filter(|key| !key.is_empty()))
}

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    /// `security` blocks indefinitely on an unanswered consent dialog.
    const TIMEOUT: Duration = Duration::from_secs(15);

    fn run(args: &[&str]) -> Result<(bool, String, String), String> {
        use std::process::Stdio;
        let mut child = mast_harness::command("security")
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
        read_mast_key(Self::read_service)
    }

    fn write(&self, key: &str) -> Result<(), String> {
        self.write_account(DEFAULT_ACCOUNT_ID, key)
    }

    fn read_account(&self, account_id: &str) -> Result<Option<String>, String> {
        validate_account_id(account_id)?;
        if account_id == DEFAULT_ACCOUNT_ID {
            return self.read();
        }
        Ok(
            Self::read_service_account(KEYCHAIN_SERVICE, keychain_account(account_id))?
                .filter(|key| !key.is_empty()),
        )
    }

    fn write_account(&self, account_id: &str, key: &str) -> Result<(), String> {
        validate_account_id(account_id)?;
        let (ok, _, stderr) = Self::run(&[
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            keychain_account(account_id),
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
        self.delete_account(DEFAULT_ACCOUNT_ID)
    }

    fn delete_account(&self, account_id: &str) -> Result<(), String> {
        validate_account_id(account_id)?;
        let (ok, _, stderr) = Self::run(&[
            "delete-generic-password", "-s", KEYCHAIN_SERVICE, "-a", keychain_account(account_id),
        ])?;
        if ok || stderr.contains("could not be found") {
            Ok(())
        } else {
            Err(format!("Keychain delete failed: {stderr}"))
        }
    }
}

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    fn read_service(service: &str) -> Result<Option<String>, String> {
        Self::read_service_account(service, KEYCHAIN_ACCOUNT)
    }

    fn read_service_account(service: &str, account: &str) -> Result<Option<String>, String> {
        let (ok, stdout, stderr) =
            Self::run(&["find-generic-password", "-s", service, "-a", account, "-w"])?;
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
mod keychain_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[test]
    fn reads_only_mast_service_and_never_queries_legacy_service() {
        let legacy_service = ["sh", "zeron", "linear"].join(".");
        let items = RefCell::new(HashMap::from([(
            legacy_service.clone(),
            "legacy-secret".to_owned(),
        )]));
        let queried = RefCell::new(Vec::<String>::new());
        let read = || {
            queried.borrow_mut().clear();
            read_mast_key(|service| {
                queried.borrow_mut().push(service.to_owned());
                Ok(items.borrow().get(service).cloned())
            })
        };

        // Legacy-only Keychain: returns None, never queries sh.zeron.linear, leaves legacy untouched.
        assert_eq!(read().unwrap(), None);
        assert_eq!(*queried.borrow(), vec![KEYCHAIN_SERVICE.to_owned()]);
        assert_eq!(
            items.borrow().get(&legacy_service).map(String::as_str),
            Some("legacy-secret")
        );
        assert!(!items.borrow().contains_key(KEYCHAIN_SERVICE));

        // Both-present Keychain: reads Mast service only, leaves legacy untouched.
        items
            .borrow_mut()
            .insert(KEYCHAIN_SERVICE.into(), "current".into());
        assert_eq!(read().unwrap().as_deref(), Some("current"));
        assert_eq!(*queried.borrow(), vec![KEYCHAIN_SERVICE.to_owned()]);
        assert_eq!(
            items.borrow().get(&legacy_service).map(String::as_str),
            Some("legacy-secret")
        );

        // Empty tombstone left by an older deletion still reads as absent.
        items
            .borrow_mut()
            .insert(KEYCHAIN_SERVICE.into(), String::new());
        assert_eq!(read().unwrap(), None);
        assert_eq!(*queried.borrow(), vec![KEYCHAIN_SERVICE.to_owned()]);

        // Empty Keychain returns None.
        items.borrow_mut().clear();
        assert_eq!(read().unwrap(), None);
        assert_eq!(*queried.borrow(), vec![KEYCHAIN_SERVICE.to_owned()]);
    }

    #[test]
    fn default_reads_api_key_entry_in_place() {
        let items = HashMap::from([((KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT), "stored")]);
        let mut queried = Vec::new();
        let value = read_mast_key(|service| {
            queried.push(service.to_owned());
            Ok(items
                .get(&(service, keychain_account(DEFAULT_ACCOUNT_ID)))
                .map(|key| (*key).to_owned()))
        })
        .unwrap();
        assert_eq!(value.as_deref(), Some("stored"));
        assert_eq!(queried, vec![KEYCHAIN_SERVICE.to_owned()]);
        assert_eq!(
            items.get(&(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)),
            Some(&"stored")
        );
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn memory_store_isolates_accounts_and_default_uses_legacy_account() {
        let store = MemoryKeyStore::with_key("default key");
        store.write_account("personal", "personal key").unwrap();
        assert_eq!(store.read().unwrap().as_deref(), Some("default key"));
        assert_eq!(
            store.read_account("personal").unwrap().as_deref(),
            Some("personal key")
        );
        store.delete_account("personal").unwrap();
        assert_eq!(store.read_account("personal").unwrap(), None);
        assert_eq!(store.read().unwrap().as_deref(), Some("default key"));
        assert_eq!(keychain_account(DEFAULT_ACCOUNT_ID), KEYCHAIN_ACCOUNT);
        assert_eq!(keychain_account("personal"), "personal");
        assert!(store.write_account(KEYCHAIN_ACCOUNT, "collision").is_err());
        assert_eq!(store.read().unwrap().as_deref(), Some("default key"));
    }
}
