//! Paired peers: other Mast engines allowed to drive this one over a direct link.
//!
//! The listener behind this module is opt-in and carries no hosted relay and no
//! sign-in. A peer proves itself with a bearer token it received at pairing.
//! Pairing works like the iOS companion bridge: the owner opens a one-time code
//! (about 244 random bits, five minutes, five attempts, consumed on first
//! success), the peer trades it for a device token of the same strength, and
//! only the token's hash is stored. Tokens expire after 30 days absolute or 7
//! days idle and can be revoked, which also closes the sockets that peer holds.
//!
//! A paired peer reaches [`PeerGate`], not the whole service: only the methods
//! (and, for `Mutate` and `QueueCommand`, only the operations) the unscoped MCP
//! server uses to drive chats. Workspace files, terminals, project actions,
//! settings, device routing (`targetDeviceId`) and the human-only methods are
//! not on the list. Anything that can start an agent here can run code as this
//! user, so the gate limits accidental reach (a `.env` read, a hop to another
//! device on this account), not a hostile paired device. Use it on an engine
//! that is signed out; a synced engine would also sync what a peer writes.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mast_rpc::{PeerAuthenticator, PeerSession, RpcError, RpcReply, RpcService, methods};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, OnceCell};

/// Method a not-yet-paired caller may use once, to trade a pairing code for a token.
pub const PEER_PAIR_METHOD: &str = "PeerPair";
/// Bearer prefix that marks a handshake as a pairing attempt.
pub const PAIRING_PREFIX: &str = "pair-";

const PAIRING_TTL_MS: i64 = 5 * 60 * 1000;
const PAIRING_ATTEMPTS: u32 = 5;
const TOKEN_ABSOLUTE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const TOKEN_IDLE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// A peer's idle clock is refreshed at most this often.
const TOUCH_INTERVAL_MS: i64 = 60 * 1000;
const STATE_FILE: &str = "peer-devices.json";
const LOCK_FILE: &str = "peer-devices.lock";

/// What the unscoped MCP server calls (`mast_mcp::Mast`), and nothing else.
pub const PEER_METHODS: &[&str] = &[
    methods::LOCAL_DEVICE,
    methods::ENGINE_INFO,
    methods::WATCH_DEVICES,
    methods::WATCH_SPACES,
    methods::WATCH_CHATS,
    methods::WATCH_SESSIONS,
    methods::WATCH_DOC_MESSAGES,
    methods::LIST_HARNESSES,
    methods::LIST_MODELS,
    methods::MUTATE,
    methods::QUEUE_COMMAND,
    methods::QUEUE_MESSAGE,
];

/// `Mutate` operations the MCP server sends (create, rename and archive a chat).
const PEER_MUTATE_OPS: &[&str] = &["createChat", "renameChat", "setChatArchived"];
/// `QueueCommand` kinds the MCP server sends (send, steer, interrupt, answer).
const PEER_COMMAND_KINDS: &[&str] = &["run", "steer", "autoMessage", "interrupt", "respondInput"];

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

/// Two v4 UUIDs: 244 random bits from the OS generator (as agent tokens are minted).
fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedPeer {
    pub id: String,
    pub label: String,
    pub token_hash: String,
    pub created_at_ms: i64,
    pub last_used_ms: i64,
}

impl PairedPeer {
    fn expired(&self, now: i64) -> bool {
        now - self.created_at_ms > TOKEN_ABSOLUTE_MS || now - self.last_used_ms > TOKEN_IDLE_MS
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerState {
    #[serde(default)]
    peers: Vec<PairedPeer>,
    /// The open pairing window, if any. Kept in the file so `mast peer pair`
    /// (a separate process) can open it for the running engine.
    #[serde(default)]
    pairing: Option<Pairing>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    code_hash: String,
    expires_at_ms: i64,
    attempts: u32,
}

/// The paired peers of this engine, persisted at `{data_dir}/peer-devices.json`.
///
/// The file is the source of truth. Writers (the engine and `mast peer ...`)
/// take an advisory lock, re-read, change and atomically replace it; readers
/// just read. A file that cannot be read or parsed means nobody is paired, so
/// damage fails closed. [`PeerRegistry::sweep`] closes the sockets of peers
/// that were revoked or expired.
pub struct PeerRegistry {
    path: PathBuf,
    lock_path: PathBuf,
    /// One entry per open peer socket: the peer id and the signal that closes it.
    live: Mutex<Vec<(String, Arc<Notify>)>>,
}

impl PeerRegistry {
    pub fn open(data_dir: &Path) -> std::io::Result<Arc<Self>> {
        let registry = Arc::new(Self {
            path: data_dir.join(STATE_FILE),
            lock_path: data_dir.join(LOCK_FILE),
            live: Mutex::new(Vec::new()),
        });
        // A corrupt file is a startup error, not a silent empty list.
        registry.read_state()?;
        Ok(registry)
    }

    fn read_state(&self) -> std::io::Result<PeerState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(PeerState::default()),
            Err(err) => Err(err),
        }
    }

    /// Readable state, or an empty one (nobody paired) when the file is damaged.
    fn state_or_deny_all(&self) -> PeerState {
        self.read_state().unwrap_or_else(|err| {
            tracing::warn!(error = %err, "peer-devices.json is unreadable; treating every peer as unpaired");
            PeerState::default()
        })
    }

    /// Change the file under the advisory lock: re-read, apply `change`, replace atomically.
    fn update<T>(&self, change: impl FnOnce(&mut PeerState) -> T) -> std::io::Result<T> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)?;
        lock.lock()?;
        let mut state = self.read_state()?;
        let out = change(&mut state);
        let bytes = serde_json::to_vec_pretty(&state).map_err(std::io::Error::other)?;
        let temp = self
            .path
            .with_extension(format!("json.tmp-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
        write_private(&temp, &bytes)?;
        std::fs::rename(&temp, &self.path)?;
        Ok(out)
    }

    /// Close the sockets of peers that are no longer paired (revoked from another
    /// process, expired, or wiped by damage) and forget their expired records.
    pub fn sweep(&self) {
        let now = now_ms();
        let state = self.state_or_deny_all();
        if state.peers.iter().any(|p| p.expired(now)) {
            let _ = self.update(|state| state.peers.retain(|p| !p.expired(now)));
        }
        let live_ids: Vec<&str> = state.peers.iter().filter(|p| !p.expired(now)).map(|p| p.id.as_str()).collect();
        let mut live = self.live.lock().unwrap();
        live.retain(|(id, notify)| {
            let keep = live_ids.contains(&id.as_str());
            if !keep {
                notify.notify_one();
            }
            // A socket that already ended holds the only other reference.
            keep && Arc::strong_count(notify) > 1
        });
    }

    /// Open a one-time pairing window and return the code to type on the peer.
    pub fn open_pairing(&self) -> std::io::Result<String> {
        let code = random_secret();
        let pairing = Pairing { code_hash: hash(&code), expires_at_ms: now_ms() + PAIRING_TTL_MS, attempts: 0 };
        self.update(|state| state.pairing = Some(pairing))?;
        Ok(code)
    }

    /// Whether a pairing window is open right now.
    fn pairing_open(&self) -> bool {
        self.state_or_deny_all()
            .pairing
            .is_some_and(|p| now_ms() <= p.expires_at_ms && p.attempts < PAIRING_ATTEMPTS)
    }

    /// Trade a pairing code for a device token. The code is consumed on success
    /// and the window closes after too many wrong tries or when it expires.
    fn redeem(&self, code: &str, label: &str) -> Result<String, String> {
        let now = now_ms();
        let token = random_secret();
        let peer = PairedPeer {
            id: uuid::Uuid::new_v4().to_string(),
            label: label.trim().chars().take(80).collect(),
            token_hash: hash(&token),
            created_at_ms: now,
            last_used_ms: now,
        };
        let outcome = self
            .update(|state| {
                let Some(pairing) = state.pairing.as_mut() else {
                    return Err("pairing is not open".to_string());
                };
                if now > pairing.expires_at_ms {
                    state.pairing = None;
                    return Err("pairing code expired".to_string());
                }
                pairing.attempts += 1;
                if hash(code) != pairing.code_hash {
                    if pairing.attempts >= PAIRING_ATTEMPTS {
                        state.pairing = None;
                    }
                    return Err("wrong pairing code".to_string());
                }
                state.pairing = None;
                state.peers.push(peer);
                Ok(())
            })
            .map_err(|e| format!("could not save the pairing: {e}"))?;
        outcome.map(|()| token)
    }

    pub fn list(&self) -> Vec<PairedPeer> {
        self.state_or_deny_all().peers
    }

    /// Remove a peer (by id, label, or an id prefix of 8+ characters) and close its sockets.
    pub fn revoke(&self, key: &str) -> Result<usize, String> {
        let removed = self
            .update(|state| {
                let matches: Vec<String> = state
                    .peers
                    .iter()
                    .filter(|p| p.id == key || p.label == key || (key.len() >= 8 && p.id.starts_with(key)))
                    .map(|p| p.id.clone())
                    .collect();
                if matches.len() > 1 {
                    return Err(format!("{key} matches {} peers; use the id", matches.len()));
                }
                state.peers.retain(|p| !matches.contains(&p.id));
                Ok(matches)
            })
            .map_err(|e| format!("could not save: {e}"))??;
        self.close_sockets(|id| removed.iter().any(|r| r == id));
        Ok(removed.len())
    }

    /// Revoke every peer.
    pub fn revoke_all(&self) -> std::io::Result<usize> {
        let count = self.update(|state| {
            let count = state.peers.len();
            state.peers.clear();
            count
        })?;
        self.close_sockets(|_| true);
        Ok(count)
    }

    fn close_sockets(&self, which: impl Fn(&str) -> bool) {
        self.live.lock().unwrap().retain(|(id, notify)| {
            let close = which(id);
            if close {
                notify.notify_one();
            }
            !close
        });
    }

    /// The paired peer a token belongs to, and a fresh signal that closes this
    /// connection when the peer is revoked. Refreshes the peer's idle clock.
    fn identify(&self, token: &str) -> Option<(String, Arc<Notify>)> {
        let now = now_ms();
        let wanted = hash(token);
        let state = self.state_or_deny_all();
        let peer = state.peers.iter().find(|p| p.token_hash == wanted && !p.expired(now))?;
        self.touch(&peer.id, peer.last_used_ms);
        let notify = Arc::new(Notify::new());
        self.live.lock().unwrap().push((peer.id.clone(), notify.clone()));
        Some((peer.id.clone(), notify))
    }

    /// Refresh a peer's idle clock, at most once per [`TOUCH_INTERVAL_MS`].
    fn touch(&self, id: &str, last_used_ms: i64) {
        let now = now_ms();
        if now - last_used_ms < TOUCH_INTERVAL_MS {
            return;
        }
        let _ = self.update(|state| {
            if let Some(peer) = state.peers.iter_mut().find(|p| p.id == id) {
                peer.last_used_ms = now;
            }
        });
    }

    /// The handshake decision for the peer listener. `service` is the engine's
    /// full service; paired peers only ever see it through [`PeerGate`].
    pub fn authenticator(self: &Arc<Self>, service: Arc<dyn RpcService>) -> PeerAuthenticator {
        let registry = self.clone();
        Arc::new(move |bearer: Option<&str>| {
            let bearer = bearer?;
            if let Some(code) = bearer.strip_prefix(PAIRING_PREFIX) {
                // Only while a window is open, and then for one thing: redeeming this code.
                if !registry.pairing_open() {
                    return None;
                }
                let pairing = Arc::new(PairService { registry: registry.clone(), code: code.to_string() });
                return Some(PeerSession { service: pairing, revoked: Arc::new(Notify::new()) });
            }
            let (id, revoked) = registry.identify(bearer)?;
            let gate = PeerGate {
                inner: service.clone(),
                local_device: OnceCell::new(),
                peer: Some((registry.clone(), id)),
                last_touch: Mutex::new(now_ms()),
            };
            Some(PeerSession { service: Arc::new(gate), revoked })
        })
    }
}

/// Create `path` readable by its owner only, from the first byte.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

/// Serves only [`PEER_PAIR_METHOD`], for a caller that presented a pairing code.
struct PairService {
    registry: Arc<PeerRegistry>,
    code: String,
}

#[async_trait]
impl RpcService for PairService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method != PEER_PAIR_METHOD {
            return Err(RpcError::Failed(format!("{method} needs a paired device")));
        }
        let label = params.get("label").and_then(|v| v.as_str()).unwrap_or("peer").to_string();
        let registry = self.registry.clone();
        let code = self.code.clone();
        let token = tokio::task::spawn_blocking(move || registry.redeem(&code, &label))
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .map_err(RpcError::Failed)?;
        RpcReply::value(&serde_json::json!({ "token": token }))
    }
}

/// The service a paired peer reaches: [`PEER_METHODS`] only, with `Mutate` and
/// `QueueCommand` narrowed to the operations the MCP server sends, no device
/// routing, and chats hosted on this device only.
pub struct PeerGate {
    inner: Arc<dyn RpcService>,
    local_device: OnceCell<Option<String>>,
    peer: Option<(Arc<PeerRegistry>, String)>,
    last_touch: Mutex<i64>,
}

impl PeerGate {
    pub fn new(inner: Arc<dyn RpcService>) -> Arc<dyn RpcService> {
        Arc::new(Self { inner, local_device: OnceCell::new(), peer: None, last_touch: Mutex::new(0) })
    }

    async fn local_device_id(&self) -> Option<&str> {
        self.local_device
            .get_or_init(|| async {
                match self.inner.handle(methods::LOCAL_DEVICE, serde_json::json!({})).await {
                    Ok(RpcReply::Value(value)) => value.get("deviceId").and_then(|v| v.as_str()).map(str::to_owned),
                    _ => None,
                }
            })
            .await
            .as_deref()
    }

    /// Why a call is refused, or `None` when a peer may make it.
    async fn refusal(&self, method: &str, params: &serde_json::Value) -> Option<String> {
        if !PEER_METHODS.contains(&method) {
            return Some(format!("{method} is not available to a paired peer"));
        }
        if params.get("targetDeviceId").is_some_and(|v| !v.is_null()) {
            return Some("a paired peer cannot route a call to another device".to_string());
        }
        if method == methods::MUTATE {
            let op = params.get("op").and_then(|v| v.as_str()).unwrap_or_default();
            if !PEER_MUTATE_OPS.contains(&op) {
                return Some(format!("Mutate {op:?} is not available to a paired peer"));
            }
            // A chat created here must be hosted here, not on another device of this account.
            if let Some(device) = params.get("deviceId").and_then(|v| v.as_str())
                && self.local_device_id().await != Some(device)
            {
                return Some("a paired peer can only create chats on this device".to_string());
            }
        }
        if method == methods::QUEUE_COMMAND {
            let kind = params.pointer("/command/kind").and_then(|v| v.as_str()).unwrap_or_default();
            if !PEER_COMMAND_KINDS.contains(&kind) {
                return Some(format!("QueueCommand {kind:?} is not available to a paired peer"));
            }
        }
        None
    }

    fn note_activity(&self) {
        let Some((registry, id)) = &self.peer else { return };
        let now = now_ms();
        {
            let mut last = self.last_touch.lock().unwrap();
            if now - *last < TOUCH_INTERVAL_MS {
                return;
            }
            *last = now;
        }
        let (registry, id) = (registry.clone(), id.clone());
        tokio::task::spawn_blocking(move || registry.touch(&id, 0));
    }
}

#[async_trait]
impl RpcService for PeerGate {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if let Some(reason) = self.refusal(method, &params).await {
            tracing::warn!(method, %reason, "refused a paired peer's call");
            return Err(RpcError::Failed(reason));
        }
        self.note_activity();
        self.inner.handle(method, params).await
    }
}

/// Where a peer listener may bind: loopback, or a tailnet address (100.64.0.0/10,
/// or `fd7a:115c:a1e0::/48`). Anything else would put the engine on a network
/// Mast does not encrypt, so it is refused. `100.64.0.0/10` is also shared
/// carrier address space; only use a tailnet address you own.
pub fn peer_listen_allowed(addr: &std::net::SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Serve the peer listener on `addr` until the returned task is dropped or
/// aborted. Refuses addresses outside [`peer_listen_allowed`].
pub async fn serve_peer_link(
    addr: std::net::SocketAddr,
    registry: Arc<PeerRegistry>,
    service: Arc<dyn RpcService>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    if !peer_listen_allowed(&addr) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{addr} is not loopback or a tailnet address; the peer link is not encrypted by Mast"),
        ));
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "peer link listening");
    let authenticate = registry.authenticator(service);
    Ok(tokio::spawn(async move {
        let sweeping = {
            let registry = registry.clone();
            async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let registry = registry.clone();
                    let _ = tokio::task::spawn_blocking(move || registry.sweep()).await;
                }
            }
        };
        tokio::select! {
            _ = mast_rpc::serve_ws_listener_authed(listener, authenticate) => {}
            _ = sweeping => {}
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl RpcService for Echo {
        async fn handle(&self, method: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            if method == methods::LOCAL_DEVICE {
                return RpcReply::value(&serde_json::json!({ "deviceId": "device-b" }));
            }
            RpcReply::value(&serde_json::json!({ "handled": method }))
        }
    }

    fn refused(reply: Result<RpcReply, RpcError>) -> String {
        reply.err().expect("refused").to_string()
    }

    fn registry() -> (tempfile::TempDir, Arc<PeerRegistry>) {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        (dir, registry)
    }

    fn pair(registry: &PeerRegistry, label: &str) -> String {
        let code = registry.open_pairing().unwrap();
        registry.redeem(&code, label).unwrap()
    }

    #[test]
    fn a_code_pairs_once_and_a_wrong_code_burns_an_attempt() {
        let (_dir, registry) = registry();
        let code = registry.open_pairing().unwrap();
        assert!(registry.redeem("nope", "x").is_err());
        let token = registry.redeem(&code, "fedora").unwrap();
        assert!(registry.identify(&token).is_some());
        assert!(registry.redeem(&code, "again").is_err(), "a code is consumed on first success");
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn five_wrong_codes_close_the_pairing_window() {
        let (_dir, registry) = registry();
        let code = registry.open_pairing().unwrap();
        for _ in 0..PAIRING_ATTEMPTS {
            assert!(registry.redeem("nope", "x").is_err());
        }
        assert!(!registry.pairing_open());
        assert!(registry.redeem(&code, "x").is_err());
    }

    #[test]
    fn an_expired_pairing_code_is_refused() {
        let (_dir, registry) = registry();
        let code = registry.open_pairing().unwrap();
        registry.update(|state| state.pairing.as_mut().unwrap().expires_at_ms = now_ms() - 1).unwrap();
        assert!(!registry.pairing_open());
        assert!(registry.redeem(&code, "x").unwrap_err().contains("expired"));
    }

    #[test]
    fn tokens_are_stored_hashed_and_survive_a_restart() {
        let (dir, registry) = registry();
        let token = pair(&registry, "fedora");
        let on_disk = std::fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(!on_disk.contains(&token), "the token itself is never written");
        let reopened = PeerRegistry::open(dir.path()).unwrap();
        assert!(reopened.identify(&token).is_some());
    }

    #[test]
    fn a_revoked_token_stops_working() {
        let (_dir, registry) = registry();
        let token = pair(&registry, "fedora");
        let (id, _) = registry.identify(&token).unwrap();
        assert_eq!(registry.revoke(&id).unwrap(), 1);
        assert!(registry.identify(&token).is_none());
    }

    #[test]
    fn an_idle_or_old_token_expires_and_a_sweep_forgets_it() {
        let (_dir, registry) = registry();
        let idle = pair(&registry, "idle");
        let old = pair(&registry, "old");
        registry
            .update(|state| {
                state.peers[0].last_used_ms -= TOKEN_IDLE_MS + 1;
                state.peers[1].created_at_ms -= TOKEN_ABSOLUTE_MS + 1;
            })
            .unwrap();
        assert!(registry.identify(&idle).is_none());
        assert!(registry.identify(&old).is_none());
        registry.sweep();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn a_damaged_file_means_nobody_is_paired() {
        let (dir, registry) = registry();
        let token = pair(&registry, "fedora");
        std::fs::write(dir.path().join(STATE_FILE), b"{ not json").unwrap();
        assert!(registry.identify(&token).is_none());
        assert!(registry.list().is_empty());
        assert!(PeerRegistry::open(dir.path()).is_err(), "a damaged file is loud at startup");
    }

    #[test]
    fn a_revoke_from_another_process_closes_live_sockets_on_the_next_sweep() {
        let (dir, registry) = registry();
        let token = pair(&registry, "fedora");
        let (_, notify) = registry.identify(&token).unwrap();
        // The CLI is another process: its own registry value over the same file.
        let cli = PeerRegistry::open(dir.path()).unwrap();
        assert_eq!(cli.revoke("fedora").unwrap(), 1);
        registry.sweep();
        let closed = futures::FutureExt::now_or_never(notify.notified());
        assert!(closed.is_some(), "the stored permit closes the socket even if it was not waiting yet");
    }

    #[tokio::test]
    async fn the_gate_passes_chat_methods_and_refuses_files_terminals_and_routing() {
        let gate = PeerGate::new(Arc::new(Echo));
        assert!(gate.handle(methods::WATCH_CHATS, serde_json::json!({})).await.is_ok());
        assert!(gate.handle(methods::MUTATE, serde_json::json!({"op": "createChat", "deviceId": "device-b"})).await.is_ok());
        assert!(gate.handle(methods::MUTATE, serde_json::json!({"op": "setChatArchived"})).await.is_ok());
        for method in [methods::READ_WORKSPACE_FILE, methods::WRITE_WORKSPACE_FILE, methods::OPEN_TERMINAL, methods::SUBMIT_REVIEW, methods::STOP_ENGINE] {
            assert!(refused(gate.handle(method, serde_json::json!({})).await).contains("not available to a paired peer"), "{method}");
        }
        for op in ["deleteSpace", "deleteChat", "createSpace", "setChatHost", "renameDevice"] {
            assert!(refused(gate.handle(methods::MUTATE, serde_json::json!({"op": op})).await).contains("not available"), "{op}");
        }
        for kind in ["steer", "interrupt", "respondInput", "run", "autoMessage"] {
            assert!(gate.handle(methods::QUEUE_COMMAND, serde_json::json!({"command": {"kind": kind}})).await.is_ok(), "{kind}");
        }
        assert!(refused(gate.handle(methods::QUEUE_COMMAND, serde_json::json!({"command": {"kind": "shell"}})).await).contains("not available"));
        let routed = serde_json::json!({"targetDeviceId": "device-a", "command": {"kind": "run"}});
        assert!(refused(gate.handle(methods::QUEUE_COMMAND, routed).await).contains("another device"));
        assert!(gate.handle(methods::WATCH_DOC_MESSAGES, serde_json::json!({"targetDeviceId": null})).await.is_ok());
        let elsewhere = serde_json::json!({"op": "createChat", "deviceId": "device-a"});
        assert!(refused(gate.handle(methods::MUTATE, elsewhere).await).contains("this device"));
    }

    #[tokio::test]
    async fn an_unpaired_caller_gets_no_service_and_pairing_needs_an_open_window() {
        let (_dir, registry) = registry();
        let authenticate = registry.authenticator(Arc::new(Echo));
        assert!(authenticate(None).is_none());
        assert!(authenticate(Some("not-a-token")).is_none());
        assert!(authenticate(Some(&format!("{PAIRING_PREFIX}whatever"))).is_none(), "no open window, no socket");
        let code = registry.open_pairing().unwrap();
        let session = authenticate(Some(&format!("{PAIRING_PREFIX}{code}"))).unwrap();
        assert!(session.service.handle(methods::MUTATE, serde_json::json!({})).await.is_err());
        assert!(session.service.handle(PEER_PAIR_METHOD, serde_json::json!({"label": "mac"})).await.is_ok());
        assert_eq!(registry.list().len(), 1);
    }

    #[tokio::test]
    async fn over_a_socket_a_peer_pairs_calls_and_is_cut_off_on_revoke() {
        let (_dir, registry) = registry();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, registry.authenticator(Arc::new(Echo))));

        assert!(mast_rpc::connect_ws(&url).await.is_err(), "no token, no socket");
        assert!(mast_rpc::connect_ws_bearer(&url, Some("forged")).await.is_err());

        let code = registry.open_pairing().unwrap();
        let pairing = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}"))).await.unwrap();
        let reply = pairing.call(PEER_PAIR_METHOD, serde_json::json!({"label": "mac"})).await.unwrap();
        let token = reply["token"].as_str().unwrap().to_string();

        let client = mast_rpc::connect_ws_bearer(&url, Some(&token)).await.unwrap();
        assert!(client.call(methods::WATCH_CHATS, serde_json::json!({})).await.is_ok());
        assert!(client.call(methods::READ_WORKSPACE_FILE, serde_json::json!({})).await.is_err());

        assert_eq!(registry.revoke("mac").unwrap(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while client.call(methods::WATCH_CHATS, serde_json::json!({})).await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("revoking closes the peer's open socket");
        assert!(mast_rpc::connect_ws_bearer(&url, Some(&token)).await.is_err());
    }

    #[test]
    fn only_loopback_and_tailnet_addresses_may_listen() {
        for ok in ["127.0.0.1:27700", "100.101.102.103:27700", "[::1]:27700", "[fd7a:115c:a1e0::1]:27700"] {
            assert!(peer_listen_allowed(&ok.parse().unwrap()), "{ok}");
        }
        for bad in ["0.0.0.0:27700", "192.168.1.20:27700", "10.0.0.5:27700", "100.128.0.1:27700", "[::]:27700"] {
            assert!(!peer_listen_allowed(&bad.parse().unwrap()), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_listener_refuses_an_address_off_the_tailnet() {
        let (_dir, registry) = registry();
        let error = serve_peer_link("0.0.0.0:27700".parse().unwrap(), registry, Arc::new(Echo)).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
