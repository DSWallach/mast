//! Paired peers: other Mast engines allowed to drive this one over a direct link.
//!
//! The listener behind this module is opt-in and carries no hosted relay and no
//! sign-in. A peer proves itself with a bearer token it received at pairing.
//! Pairing works like the iOS companion bridge: the owner opens a one-time code
//! (192 random bits, five minutes, five attempts, consumed on first success),
//! the peer trades it for a 256-bit device token, and only the token's hash is
//! stored. Tokens expire after 30 days absolute or 7 days idle and can be
//! revoked, which also closes the sockets that peer holds.
//!
//! A paired peer reaches [`PeerGate`], not the whole service: only the methods
//! the unscoped MCP server uses to drive chats. Workspace files, terminals,
//! project actions and settings are not on the list, and neither are the
//! human-only methods. Anything that can start an agent here can run code as
//! this user, so the gate limits accidental reach (a `.env` read), not a
//! hostile paired device.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mast_rpc::{PeerAuthenticator, PeerSession, RpcError, RpcReply, RpcService, methods};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

/// Method a not-yet-paired caller may use once, to trade a pairing code for a token.
pub const PEER_PAIR_METHOD: &str = "PeerPair";
/// Bearer prefix that marks a handshake as a pairing attempt.
pub const PAIRING_PREFIX: &str = "pair-";

const PAIRING_TTL_MS: i64 = 5 * 60 * 1000;
const PAIRING_ATTEMPTS: u32 = 5;
const TOKEN_ABSOLUTE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const TOKEN_IDLE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const STATE_FILE: &str = "peer-devices.json";

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

struct Inner {
    state: PeerState,
    revoke: HashMap<String, Arc<Notify>>,
}

/// The paired peers of this engine, persisted at `{data_dir}/peer-devices.json`.
///
/// The file is the source of truth and is re-read before every operation, so
/// the CLI (`mast peer pair/list/revoke`) and the running engine agree without
/// an RPC. [`PeerRegistry::sweep`] closes the sockets of peers the CLI removed.
pub struct PeerRegistry {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl PeerRegistry {
    pub fn open(data_dir: &Path) -> std::io::Result<Arc<Self>> {
        let path = data_dir.join(STATE_FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => PeerState::default(),
            Err(err) => return Err(err),
        };
        Ok(Arc::new(Self {
            path,
            inner: Mutex::new(Inner { state, revoke: HashMap::new() }),
        }))
    }

    fn reload(&self, inner: &mut Inner) {
        if let Ok(bytes) = std::fs::read(&self.path)
            && let Ok(state) = serde_json::from_slice(&bytes)
        {
            inner.state = state;
        }
    }

    /// Close the sockets of peers that were revoked from another process.
    pub fn sweep(&self) {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        let live: Vec<String> = inner.state.peers.iter().map(|p| p.id.clone()).collect();
        let gone: Vec<String> = inner.revoke.keys().filter(|id| !live.contains(id)).cloned().collect();
        for id in gone {
            if let Some(notify) = inner.revoke.remove(&id) {
                notify.notify_waiters();
            }
        }
    }

    fn save(&self, state: &PeerState) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp, &self.path)
    }

    /// Open a one-time pairing window and return the code to type on the peer.
    pub fn open_pairing(&self) -> String {
        let code = random_secret();
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        inner.state.pairing = Some(Pairing {
            code_hash: hash(&code),
            expires_at_ms: now_ms() + PAIRING_TTL_MS,
            attempts: 0,
        });
        let _ = self.save(&inner.state);
        code
    }

    /// Trade a pairing code for a device token. The code is consumed on success
    /// and the window closes after too many wrong tries or when it expires.
    fn redeem(&self, code: &str, label: &str) -> Result<String, String> {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        let now = now_ms();
        let Some(pairing) = inner.state.pairing.as_mut() else {
            return Err("pairing is not open".to_string());
        };
        if now > pairing.expires_at_ms {
            inner.state.pairing = None;
            let _ = self.save(&inner.state);
            return Err("pairing code expired".to_string());
        }
        pairing.attempts += 1;
        if hash(code) != pairing.code_hash {
            if pairing.attempts >= PAIRING_ATTEMPTS {
                inner.state.pairing = None;
            }
            let _ = self.save(&inner.state);
            return Err("wrong pairing code".to_string());
        }
        inner.state.pairing = None;
        let token = random_secret();
        inner.state.peers.push(PairedPeer {
            id: uuid::Uuid::new_v4().to_string(),
            label: label.trim().chars().take(80).collect(),
            token_hash: hash(&token),
            created_at_ms: now,
            last_used_ms: now,
        });
        self.save(&inner.state).map_err(|e| format!("could not save the pairing: {e}"))?;
        Ok(token)
    }

    pub fn list(&self) -> Vec<PairedPeer> {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        inner.state.peers.clone()
    }

    /// Remove a peer (by id, label, or an id prefix of 8+ characters) and close its sockets.
    pub fn revoke(&self, key: &str) -> Result<usize, String> {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        let matches: Vec<String> = inner
            .state
            .peers
            .iter()
            .filter(|p| p.id == key || p.label == key || (key.len() >= 8 && p.id.starts_with(key)))
            .map(|p| p.id.clone())
            .collect();
        if matches.len() > 1 {
            return Err(format!("{key} matches {} peers; use the id", matches.len()));
        }
        inner.state.peers.retain(|p| !matches.contains(&p.id));
        self.save(&inner.state).map_err(|e| format!("could not save: {e}"))?;
        for id in &matches {
            if let Some(notify) = inner.revoke.remove(id) {
                notify.notify_waiters();
            }
        }
        Ok(matches.len())
    }

    /// Revoke every peer.
    pub fn revoke_all(&self) -> std::io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        let count = inner.state.peers.len();
        inner.state.peers.clear();
        self.save(&inner.state)?;
        for (_, notify) in inner.revoke.drain() {
            notify.notify_waiters();
        }
        Ok(count)
    }

    /// The paired peer a token belongs to, refreshing its idle clock.
    /// Expired tokens are dropped.
    fn identify(&self, token: &str) -> Option<(String, Arc<Notify>)> {
        let mut inner = self.inner.lock().unwrap();
        self.reload(&mut inner);
        let now = now_ms();
        let wanted = hash(token);
        let expired = |p: &PairedPeer| now - p.created_at_ms > TOKEN_ABSOLUTE_MS || now - p.last_used_ms > TOKEN_IDLE_MS;
        let before = inner.state.peers.len();
        inner.state.peers.retain(|p| !expired(p));
        let dropped = before != inner.state.peers.len();
        let found = inner.state.peers.iter_mut().find(|p| p.token_hash == wanted).map(|p| {
            p.last_used_ms = now;
            p.id.clone()
        });
        if dropped || found.is_some() {
            let _ = self.save(&inner.state);
        }
        let id = found?;
        let notify = inner.revoke.entry(id.clone()).or_insert_with(|| Arc::new(Notify::new())).clone();
        Some((id, notify))
    }

    /// The handshake decision for the peer listener. `service` is the engine's
    /// full service; paired peers only ever see it through [`PeerGate`].
    pub fn authenticator(self: &Arc<Self>, service: Arc<dyn RpcService>) -> PeerAuthenticator {
        let registry = self.clone();
        Arc::new(move |bearer: Option<&str>| {
            let bearer = bearer?;
            if let Some(code) = bearer.strip_prefix(PAIRING_PREFIX) {
                // A pairing connection can do one thing: redeem the code it presented.
                let pairing = Arc::new(PairService { registry: registry.clone(), code: code.to_string() });
                return Some(PeerSession { service: pairing, revoked: Arc::new(Notify::new()) });
            }
            let (_, revoked) = registry.identify(bearer)?;
            Some(PeerSession { service: Arc::new(PeerGate { inner: service.clone() }), revoked })
        })
    }
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
        let label = params.get("label").and_then(|v| v.as_str()).unwrap_or("peer");
        let token = self.registry.redeem(&self.code, label).map_err(RpcError::Failed)?;
        RpcReply::value(&serde_json::json!({ "token": token }))
    }
}

/// The service a paired peer reaches: [`PEER_METHODS`] only.
pub struct PeerGate {
    inner: Arc<dyn RpcService>,
}

impl PeerGate {
    pub fn new(inner: Arc<dyn RpcService>) -> Arc<dyn RpcService> {
        Arc::new(Self { inner })
    }
}

#[async_trait]
impl RpcService for PeerGate {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if !PEER_METHODS.contains(&method) {
            tracing::warn!(method, "refused a method outside the peer allowlist");
            return Err(RpcError::Failed(format!("{method} is not available to a paired peer")));
        }
        self.inner.handle(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl RpcService for Echo {
        async fn handle(&self, method: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            RpcReply::value(&serde_json::json!({ "handled": method }))
        }
    }

    #[test]
    fn a_code_pairs_once_and_a_wrong_code_burns_an_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let code = registry.open_pairing();
        assert!(registry.redeem("nope", "x").is_err());
        let token = registry.redeem(&code, "fedora").unwrap();
        assert!(registry.identify(&token).is_some());
        assert!(registry.redeem(&code, "again").is_err(), "a code is consumed on first success");
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn five_wrong_codes_close_the_pairing_window() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let code = registry.open_pairing();
        for _ in 0..PAIRING_ATTEMPTS {
            assert!(registry.redeem("nope", "x").is_err());
        }
        assert!(registry.redeem(&code, "x").is_err());
    }

    #[test]
    fn tokens_are_stored_hashed_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let token = registry.redeem_for_test("fedora");
        let on_disk = std::fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(!on_disk.contains(&token), "the token itself is never written");
        let reopened = PeerRegistry::open(dir.path()).unwrap();
        assert!(reopened.identify(&token).is_some());
    }

    #[test]
    fn a_revoked_token_stops_working_and_signals_its_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let token = registry.redeem_for_test("fedora");
        let (id, _) = registry.identify(&token).unwrap();
        assert_eq!(registry.revoke(&id).unwrap(), 1);
        assert!(registry.identify(&token).is_none());
    }

    #[test]
    fn an_idle_token_expires() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let token = registry.redeem_for_test("fedora");
        {
            let mut inner = registry.inner.lock().unwrap();
            inner.state.peers[0].last_used_ms -= TOKEN_IDLE_MS + 1;
            registry.save(&inner.state).unwrap();
        }
        assert!(registry.identify(&token).is_none());
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn the_gate_passes_chat_methods_and_refuses_files_and_terminals() {
        let gate = PeerGate::new(Arc::new(Echo));
        assert!(gate.handle(methods::MUTATE, serde_json::json!({})).await.is_ok());
        for method in [methods::READ_WORKSPACE_FILE, methods::WRITE_WORKSPACE_FILE, methods::OPEN_TERMINAL, methods::SUBMIT_REVIEW] {
            let error = gate.handle(method, serde_json::json!({})).await.err().expect("refused").to_string();
            assert!(error.contains("not available to a paired peer"), "{method}: {error}");
        }
    }

    #[tokio::test]
    async fn an_unpaired_caller_gets_no_service_and_a_pairing_caller_gets_only_the_pair_method() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let authenticate = registry.authenticator(Arc::new(Echo));
        assert!(authenticate(None).is_none());
        assert!(authenticate(Some("not-a-token")).is_none());
        let code = registry.open_pairing();
        let session = authenticate(Some(&format!("{PAIRING_PREFIX}{code}"))).unwrap();
        assert!(session.service.handle(methods::MUTATE, serde_json::json!({})).await.is_err());
        assert!(session.service.handle(PEER_PAIR_METHOD, serde_json::json!({"label": "mac"})).await.is_ok());
        assert_eq!(registry.list().len(), 1);
    }

    #[tokio::test]
    async fn over_a_socket_a_peer_pairs_calls_and_is_cut_off_on_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, registry.authenticator(Arc::new(Echo))));

        assert!(mast_rpc::connect_ws(&url).await.is_err(), "no token, no socket");
        assert!(mast_rpc::connect_ws_bearer(&url, Some("forged")).await.is_err());

        let code = registry.open_pairing();
        let pairing = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}"))).await.unwrap();
        let reply = pairing.call(PEER_PAIR_METHOD, serde_json::json!({"label": "mac"})).await.unwrap();
        let token = reply["token"].as_str().unwrap().to_string();

        let client = mast_rpc::connect_ws_bearer(&url, Some(&token)).await.unwrap();
        assert!(client.call(methods::MUTATE, serde_json::json!({})).await.is_ok());
        assert!(client.call(methods::READ_WORKSPACE_FILE, serde_json::json!({})).await.is_err());

        assert_eq!(registry.revoke("mac").unwrap(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while client.call(methods::MUTATE, serde_json::json!({})).await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("revoking closes the peer's open socket");
        assert!(mast_rpc::connect_ws_bearer(&url, Some(&token)).await.is_err());
    }

    impl PeerRegistry {
        fn redeem_for_test(&self, label: &str) -> String {
            let code = self.open_pairing();
            self.redeem(&code, label).unwrap()
        }
    }
}

/// Where a peer listener may bind: loopback, or a tailnet address (100.64.0.0/10,
/// or `fd7a:115c:a1e0::/48`). Anything else would put the engine on a network
/// Mast does not encrypt, so it is refused.
pub fn peer_listen_allowed(addr: &std::net::SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Serve the peer listener on `addr` until the task is dropped. Refuses
/// addresses outside [`peer_listen_allowed`].
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
    let sweeper = registry.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            sweeper.sweep();
        }
    });
    let authenticate = registry.authenticator(service);
    Ok(tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, authenticate)))
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    #[test]
    fn only_loopback_and_tailnet_addresses_may_listen() {
        for ok in ["127.0.0.1:27700", "100.101.102.103:27700", "[::1]:27700", "[fd7a:115c:a1e0::1]:27700"] {
            assert!(peer_listen_allowed(&ok.parse().unwrap()), "{ok}");
        }
        for bad in ["0.0.0.0:27700", "192.168.1.20:27700", "10.0.0.5:27700", "100.128.0.1:27700", "[::]:27700"] {
            assert!(!peer_listen_allowed(&bad.parse().unwrap()), "{bad}");
        }
    }
}
