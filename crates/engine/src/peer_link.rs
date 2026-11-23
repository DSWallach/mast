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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;
use mast_proto::peer_link as wire;
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
/// How long a peer's view of its own access (scope, chats) may be reused.
const ACCESS_CACHE: Duration = Duration::from_secs(1);
/// Chats one peer may own, and reports waiting for it to collect.
const MAX_PEER_CHATS: usize = 1000;
const MAX_PENDING_REPORTS: usize = 200;
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
    // Answered by the gate itself, for the authenticated peer only.
    wire::PEER_GIT_PUT,
    wire::PEER_GIT_APPLY,
    wire::PEER_GIT_BUNDLE,
    wire::PEER_GIT_READ,
    wire::PEER_REPORTS,
    wire::PEER_REPORTS_ACK,
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

/// How much of this device a paired peer may reach.
///
/// `own` peers see and drive only the chats they created through the link.
/// `all` is how every pairing worked before scopes existed: a peer with no
/// recorded scope keeps it, so an upgrade neither breaks nor widens a live link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerScope {
    #[default]
    All,
    Own,
}

impl PeerScope {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "all" => Some(Self::All),
            "own" => Some(Self::Own),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Own => "own",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedPeer {
    pub id: String,
    pub label: String,
    pub token_hash: String,
    pub created_at_ms: i64,
    pub last_used_ms: i64,
    #[serde(default)]
    pub scope: PeerScope,
}

/// A chat a peer created through the link, and where its reports go.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerChat {
    peer_id: String,
    chat_id: String,
    /// The parent chat on the peer's own device, kept as an opaque id: it is
    /// never looked up here, and reports for it only ever go back to this peer.
    #[serde(default)]
    remote_parent: Option<String>,
    created_at_ms: i64,
}

/// A worker report waiting for its peer to collect it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredReport {
    peer_id: String,
    #[serde(flatten)]
    item: wire::PeerReportItem,
}

/// What one peer may reach right now.
#[derive(Debug, Clone)]
pub struct PeerAccess {
    scope: PeerScope,
    chats: Arc<HashSet<String>>,
}

impl PeerAccess {
    /// Whether the peer may read or drive this chat.
    pub fn allows(&self, chat_id: &str) -> bool {
        self.scope == PeerScope::All || self.chats.contains(chat_id)
    }

    fn created(&self, chat_id: &str) -> bool {
        self.chats.contains(chat_id)
    }

    pub fn scope(&self) -> PeerScope {
        self.scope
    }
}

/// Where a worker report goes when its chat has no parent on this device.
pub trait RemoteReportSink: Send + Sync {
    /// Queue `text` for the chat's remote parent. `None` means the chat has
    /// none, so the caller keeps its normal path.
    fn report(&self, chat_id: &str, title: Option<&str>, text: &str) -> Option<Result<mast_proto::ReportToParentResult, String>>;
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
    #[serde(default)]
    chats: Vec<PeerChat>,
    #[serde(default)]
    reports: Vec<StoredReport>,
    /// The open pairing window, if any. Kept in the file so `mast peer pair`
    /// (a separate process) can open it for the running engine.
    #[serde(default)]
    pairing: Option<Pairing>,
}

impl PeerState {
    /// Drop chats and reports whose peer is no longer paired.
    fn forget_orphans(&mut self) {
        let PeerState { peers, chats, reports, .. } = self;
        let ids: HashSet<&str> = peers.iter().map(|p| p.id.as_str()).collect();
        chats.retain(|c| ids.contains(c.peer_id.as_str()));
        reports.retain(|r| ids.contains(r.peer_id.as_str()));
    }
}

/// The peers `key` names: an exact id wins over a label that happens to equal
/// it, then labels, then an id prefix of 8+ characters. More than one match is an error.
fn resolve_peer(state: &PeerState, key: &str) -> Result<Vec<String>, String> {
    let by_id: Vec<String> = state.peers.iter().filter(|p| p.id == key).map(|p| p.id.clone()).collect();
    let matches: Vec<String> = if by_id.len() == 1 {
        by_id
    } else {
        state
            .peers
            .iter()
            .filter(|p| p.label == key || (key.len() >= 8 && p.id.starts_with(key)))
            .map(|p| p.id.clone())
            .collect()
    };
    if matches.len() > 1 {
        return Err(format!("{key} matches {} peers; use the id", matches.len()));
    }
    Ok(matches)
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
    data_dir: PathBuf,
    path: PathBuf,
    lock_path: PathBuf,
    /// One entry per open peer socket: the peer id and the signal that closes it.
    live: Mutex<Vec<(String, Arc<Notify>)>>,
    access: Mutex<HashMap<String, (Instant, PeerAccess)>>,
}

impl PeerRegistry {
    pub fn open(data_dir: &Path) -> std::io::Result<Arc<Self>> {
        let registry = Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            path: data_dir.join(STATE_FILE),
            lock_path: data_dir.join(LOCK_FILE),
            live: Mutex::new(Vec::new()),
            access: Mutex::new(HashMap::new()),
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
        let expired: Vec<String> = state.peers.iter().filter(|p| p.expired(now)).map(|p| p.id.clone()).collect();
        if !expired.is_empty() {
            let _ = self.update(|state| {
                state.peers.retain(|p| !p.expired(now));
                state.forget_orphans();
            });
            self.forget_peers(&expired);
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
            // A new pairing starts narrow: it drives only the chats it creates.
            scope: PeerScope::Own,
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

    /// Remove a peer (by id, label, or an id prefix of 8+ characters) and close
    /// its sockets. Its chats stay on this device; its undelivered reports are dropped.
    pub fn revoke(&self, key: &str) -> Result<usize, String> {
        let removed = self
            .update(|state| {
                let matches = resolve_peer(state, key)?;
                state.peers.retain(|p| !matches.contains(&p.id));
                state.forget_orphans();
                Ok(matches)
            })
            .map_err(|e| format!("could not save: {e}"))??;
        self.close_sockets(|id| removed.iter().any(|r| r == id));
        self.forget_peers(&removed);
        Ok(removed.len())
    }

    /// Change what a peer may reach (by id, label, or id prefix). Takes effect
    /// on the running engine within a second, with no reconnect.
    pub fn set_scope(&self, key: &str, scope: PeerScope) -> Result<usize, String> {
        let changed = self
            .update(|state| {
                let matches = resolve_peer(state, key)?;
                for peer in state.peers.iter_mut().filter(|p| matches.contains(&p.id)) {
                    peer.scope = scope;
                }
                Ok(matches)
            })
            .map_err(|e| format!("could not save: {e}"))??;
        self.access.lock().unwrap().clear();
        Ok(changed.len())
    }

    /// Forget what is cached or scratch for peers that are gone.
    fn forget_peers(&self, ids: &[String]) {
        let mut access = self.access.lock().unwrap();
        for id in ids {
            access.remove(id);
            crate::peer_git::remove_peer_transfers(&self.transfers_root(), id);
        }
    }

    /// Where bundles in flight are kept.
    pub fn transfers_root(&self) -> PathBuf {
        self.data_dir.join("peer-transfers")
    }

    /// Revoke every peer.
    pub fn revoke_all(&self) -> std::io::Result<usize> {
        let (count, ids) = self.update(|state| {
            let ids: Vec<String> = state.peers.iter().map(|p| p.id.clone()).collect();
            state.peers.clear();
            state.forget_orphans();
            (ids.len(), ids)
        })?;
        self.close_sockets(|_| true);
        self.forget_peers(&ids);
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

    /// What `peer_id` may reach, or `None` when it is no longer paired. Reused
    /// for a second, so filtering a busy stream does not read the file per item.
    pub fn access(&self, peer_id: &str) -> Option<PeerAccess> {
        let now = Instant::now();
        if let Some((at, access)) = self.access.lock().unwrap().get(peer_id)
            && now.duration_since(*at) < ACCESS_CACHE
        {
            return Some(access.clone());
        }
        let state = self.state_or_deny_all();
        let ms = now_ms();
        let Some(peer) = state.peers.iter().find(|p| p.id == peer_id && !p.expired(ms)) else {
            self.access.lock().unwrap().remove(peer_id);
            return None;
        };
        let chats: HashSet<String> = state.chats.iter().filter(|c| c.peer_id == peer_id).map(|c| c.chat_id.clone()).collect();
        let access = PeerAccess { scope: peer.scope, chats: Arc::new(chats) };
        self.access.lock().unwrap().insert(peer_id.to_string(), (now, access.clone()));
        Some(access)
    }

    /// Record that `peer_id` created `chat_id`, and where its reports go.
    /// Idempotent for the same peer; a chat another peer created is refused.
    pub fn claim_chat(&self, peer_id: &str, chat_id: &str, remote_parent: Option<&str>) -> Result<(), String> {
        let outcome = self
            .update(|state| {
                if !state.peers.iter().any(|p| p.id == peer_id) {
                    return Err("this peer is no longer paired".to_string());
                }
                if let Some(existing) = state.chats.iter().find(|c| c.chat_id == chat_id) {
                    return if existing.peer_id == peer_id { Ok(()) } else { Err("that chat belongs to another peer".to_string()) };
                }
                if state.chats.iter().filter(|c| c.peer_id == peer_id).count() >= MAX_PEER_CHATS {
                    return Err(format!("this peer already owns {MAX_PEER_CHATS} chats; archive some and revoke old pairings"));
                }
                state.chats.push(PeerChat {
                    peer_id: peer_id.to_string(),
                    chat_id: chat_id.to_string(),
                    remote_parent: remote_parent.map(str::to_string),
                    created_at_ms: now_ms(),
                });
                Ok(())
            })
            .map_err(|e| format!("could not record the chat: {e}"))?;
        self.access.lock().unwrap().remove(peer_id);
        outcome
    }

    /// The peer that created `chat_id` and the parent it named, if it named one.
    fn remote_parent(&self, chat_id: &str) -> Option<(String, String)> {
        let state = self.state_or_deny_all();
        let chat = state.chats.iter().find(|c| c.chat_id == chat_id)?;
        if !state.peers.iter().any(|p| p.id == chat.peer_id) {
            return None;
        }
        Some((chat.peer_id.clone(), chat.remote_parent.clone()?))
    }

    /// Queue a report from `chat_id`. The id is derived from the chat, the text
    /// and a 45-second window, so a retried report is one report.
    fn enqueue_report(&self, chat_id: &str, title: Option<&str>, text: &str) -> Result<Option<(String, String)>, String> {
        let now = now_ms();
        let bucket = now / 45_000;
        self.update(|state| {
            let Some(chat) = state.chats.iter().find(|c| c.chat_id == chat_id).cloned() else {
                return Ok(None);
            };
            let Some(parent) = chat.remote_parent.clone() else {
                return Ok(None);
            };
            if !state.peers.iter().any(|p| p.id == chat.peer_id) {
                return Ok(None);
            }
            let id = mast_proto::derive_id(&format!("{chat_id}\n{bucket}\n{text}"), "peer-report");
            if state.reports.iter().any(|r| r.item.id == id) {
                return Ok(Some((chat.peer_id, parent)));
            }
            if state.reports.iter().filter(|r| r.peer_id == chat.peer_id).count() >= MAX_PENDING_REPORTS {
                return Err(format!("{MAX_PENDING_REPORTS} reports are waiting for the peer to collect them; is it connected?"));
            }
            state.reports.push(StoredReport {
                peer_id: chat.peer_id.clone(),
                item: wire::PeerReportItem {
                    id,
                    chat_id: chat_id.to_string(),
                    title: title.map(str::to_string),
                    parent_chat_id: parent.clone(),
                    text: text.to_string(),
                    created_at_ms: now,
                },
            });
            Ok(Some((chat.peer_id, parent)))
        })
        .map_err(|e| format!("could not queue the report: {e}"))?
    }

    /// The oldest undelivered reports for a peer.
    pub fn pending_reports(&self, peer_id: &str, limit: usize) -> Vec<wire::PeerReportItem> {
        self.state_or_deny_all()
            .reports
            .into_iter()
            .filter(|r| r.peer_id == peer_id)
            .take(limit)
            .map(|r| r.item)
            .collect()
    }

    /// Forget reports the peer confirmed. Unknown ids are ignored, so a repeated ack is fine.
    pub fn ack_reports(&self, peer_id: &str, ids: &[String]) -> std::io::Result<usize> {
        self.update(|state| {
            let before = state.reports.len();
            state.reports.retain(|r| !(r.peer_id == peer_id && ids.contains(&r.item.id)));
            before - state.reports.len()
        })
    }

    /// How many chats a peer created and how many reports wait for it.
    pub fn peer_counts(&self, peer_id: &str) -> (usize, usize) {
        let state = self.state_or_deny_all();
        (
            state.chats.iter().filter(|c| c.peer_id == peer_id).count(),
            state.reports.iter().filter(|r| r.peer_id == peer_id).count(),
        )
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
                // A pairing socket lives for one redeem attempt, or 30 seconds,
                // so strangers cannot hold the connection slots.
                let close = Arc::new(Notify::new());
                close_after(close.clone(), PAIRING_SOCKET_LIFETIME);
                let pairing = Arc::new(PairService { registry: registry.clone(), code: code.to_string(), close: close.clone() });
                return Some(PeerSession { service: pairing, revoked: close });
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

impl RemoteReportSink for PeerRegistry {
    fn report(&self, chat_id: &str, title: Option<&str>, text: &str) -> Option<Result<mast_proto::ReportToParentResult, String>> {
        self.remote_parent(chat_id)?;
        let (text, truncated) = mast_proto::cap_worker_report(text);
        if text.is_empty() {
            return Some(Err("text is empty".into()));
        }
        match self.enqueue_report(chat_id, title, &text) {
            Ok(Some((_, parent))) => Some(Ok(mast_proto::ReportToParentResult {
                parent_chat_id: parent,
                delivery: "queuedForRemoteParent".into(),
                held_reason: None,
                truncated,
            })),
            Ok(None) => None,
            Err(message) => Some(Err(message)),
        }
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

/// How long a pairing socket may stay open without redeeming.
const PAIRING_SOCKET_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a pairing socket stays open after a redeem attempt, so the reply is delivered.
const PAIRING_REPLY_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Signal `notify` after `delay`.
fn close_after(notify: Arc<Notify>, delay: std::time::Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        notify.notify_one();
    });
}

/// Serves only [`PEER_PAIR_METHOD`], for a caller that presented a pairing code.
struct PairService {
    registry: Arc<PeerRegistry>,
    code: String,
    close: Arc<Notify>,
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
        let outcome = tokio::task::spawn_blocking(move || registry.redeem(&code, &label)).await;
        // One attempt per socket, successful or not.
        close_after(self.close.clone(), PAIRING_REPLY_GRACE);
        let token = outcome.map_err(|e| RpcError::Failed(e.to_string()))?.map_err(RpcError::Failed)?;
        RpcReply::value(&serde_json::json!({ "token": token }))
    }
}

/// The service a paired peer reaches: [`PEER_METHODS`] only, with `Mutate` and
/// `QueueCommand` narrowed to the operations the MCP server sends, no device
/// routing, and chats hosted on this device only. A peer whose scope is `own`
/// also sees and drives only the chats it created.
pub struct PeerGate {
    inner: Arc<dyn RpcService>,
    local_device: OnceCell<String>,
    peer: Option<(Arc<PeerRegistry>, String)>,
    last_touch: Mutex<i64>,
}

/// What the gate knows about the caller's reach for one call.
enum Reach {
    /// A gate with no registry (tests, in-process embedding): no chat scoping.
    Unscoped,
    Peer(PeerAccess),
}

impl Reach {
    fn allows(&self, chat_id: &str) -> bool {
        match self {
            Reach::Unscoped => true,
            Reach::Peer(access) => access.allows(chat_id),
        }
    }

    fn created(&self, chat_id: &str) -> bool {
        matches!(self, Reach::Peer(access) if access.created(chat_id))
    }

    fn own_only(&self) -> bool {
        matches!(self, Reach::Peer(access) if access.scope() == PeerScope::Own)
    }
}

/// Methods the gate answers itself rather than forwarding to the engine.
fn is_peer_only(method: &str) -> bool {
    matches!(
        method,
        wire::PEER_GIT_PUT | wire::PEER_GIT_APPLY | wire::PEER_GIT_BUNDLE | wire::PEER_GIT_READ | wire::PEER_REPORTS | wire::PEER_REPORTS_ACK
    )
}

const NOT_YOURS: &str = "that chat was not created by this paired peer";

impl PeerGate {
    pub fn new(inner: Arc<dyn RpcService>) -> Arc<dyn RpcService> {
        Arc::new(Self { inner, local_device: OnceCell::new(), peer: None, last_touch: Mutex::new(0) })
    }

    async fn local_device_id(&self) -> Option<&str> {
        // A failed lookup is not cached: the next call retries it.
        self.local_device
            .get_or_try_init(|| async {
                match self.inner.handle(methods::LOCAL_DEVICE, serde_json::json!({})).await {
                    Ok(RpcReply::Value(value)) => value.get("deviceId").and_then(|v| v.as_str()).map(str::to_owned).ok_or(()),
                    _ => Err(()),
                }
            })
            .await
            .ok()
            .map(String::as_str)
    }

    /// The current value of one of the engine's watches.
    async fn first_item(&self, method: &str) -> Option<serde_json::Value> {
        match self.inner.handle(method, serde_json::json!({})).await {
            Ok(RpcReply::Stream(mut stream)) => stream.next().await,
            _ => None,
        }
    }

    async fn chat_exists(&self, chat_id: &str) -> bool {
        self.first_item(methods::WATCH_CHATS)
            .await
            .and_then(|value| value.as_array().map(|rows| rows.iter().any(|row| row.get("id").and_then(|v| v.as_str()) == Some(chat_id))))
            .unwrap_or(false)
    }

    /// The folder of a Space that lives on this device.
    async fn space_path(&self, space_id: &str) -> Result<PathBuf, String> {
        let spaces = self.first_item(methods::WATCH_SPACES).await.ok_or("could not read this device's Spaces")?;
        let local = self.local_device_id().await.ok_or("could not tell which device this is")?;
        let space = spaces
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row.get("id").and_then(|v| v.as_str()) == Some(space_id)))
            .ok_or("no such Space on this device")?;
        if space.get("deviceId").and_then(|v| v.as_str()) != Some(local) {
            return Err("that Space is not on this device".into());
        }
        space.get("path").and_then(|v| v.as_str()).map(PathBuf::from).ok_or_else(|| "that Space has no folder".to_string())
    }

    fn reach(&self) -> Result<Reach, String> {
        match &self.peer {
            None => Ok(Reach::Unscoped),
            Some((registry, id)) => registry.access(id).map(Reach::Peer).ok_or_else(|| "this peer is no longer paired".to_string()),
        }
    }

    /// Why a call is refused, or the params to forward (a peer's `parentChatId`
    /// is removed: it names a chat on the peer's device, never one here).
    async fn vet(&self, method: &str, mut params: serde_json::Value) -> Result<serde_json::Value, String> {
        if !PEER_METHODS.contains(&method) {
            return Err(format!("{method} is not available to a paired peer"));
        }
        if params.get("targetDeviceId").is_some_and(|v| !v.is_null()) {
            return Err("a paired peer cannot route a call to another device".to_string());
        }
        let reach = self.reach()?;
        let chat_param = |params: &serde_json::Value| params.get("chatId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if method == methods::MUTATE {
            let op = params.get("op").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            if !PEER_MUTATE_OPS.contains(&op.as_str()) {
                return Err(format!("Mutate {op:?} is not available to a paired peer"));
            }
            // A chat created here must be hosted here, not on another device of this account.
            if let Some(device) = params.get("deviceId").and_then(|v| v.as_str())
                && self.local_device_id().await != Some(device)
            {
                return Err("a paired peer can only create chats on this device".to_string());
            }
            match op.as_str() {
                "createChat" => self.vet_create_chat(&reach, &mut params).await?,
                _ if !reach.allows(&chat_param(&params)) => return Err(NOT_YOURS.to_string()),
                _ => {}
            }
        }
        if method == methods::QUEUE_COMMAND {
            let kind = params.pointer("/command/kind").and_then(|v| v.as_str()).unwrap_or_default();
            if !PEER_COMMAND_KINDS.contains(&kind) {
                return Err(format!("QueueCommand {kind:?} is not available to a paired peer"));
            }
        }
        if matches!(method, methods::QUEUE_COMMAND | methods::QUEUE_MESSAGE | methods::WATCH_DOC_MESSAGES) && !reach.allows(&chat_param(&params)) {
            return Err(NOT_YOURS.to_string());
        }
        Ok(params)
    }

    async fn vet_create_chat(&self, reach: &Reach, params: &mut serde_json::Value) -> Result<(), String> {
        let chat_id = params
            .get("chatId")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or("createChat needs a chatId")?
            .to_string();
        if reach.own_only() {
            if params.get("spaceId").and_then(|v| v.as_str()).is_none_or(|s| s.trim().is_empty()) {
                return Err("a paired peer can only create chats in a Space".to_string());
            }
            for key in ["sideOf", "followupOf"] {
                if let Some(other) = params.get(key).and_then(|v| v.as_str())
                    && !reach.allows(other)
                {
                    return Err(NOT_YOURS.to_string());
                }
            }
        }
        // An existing chat this peer did not create is never claimed. A peer
        // with a narrow scope may not even recreate it.
        let owned = reach.created(&chat_id);
        let may_claim = owned || !self.chat_exists(&chat_id).await;
        if !may_claim && reach.own_only() {
            return Err("that chat id is already in use".to_string());
        }
        let remote_parent = params.as_object_mut().and_then(|o| o.remove("parentChatId")).and_then(|v| v.as_str().map(|s| s.trim().to_string())).filter(|p| !p.is_empty());
        if remote_parent.as_ref().is_some_and(|p| p.len() > 100) {
            return Err("parentChatId is too long".to_string());
        }
        if may_claim && let Some((registry, peer_id)) = &self.peer {
            let (registry, peer_id, chat_id) = (registry.clone(), peer_id.clone(), chat_id);
            tokio::task::spawn_blocking(move || registry.claim_chat(&peer_id, &chat_id, remote_parent.as_deref()))
                .await
                .map_err(|e| e.to_string())??;
        }
        Ok(())
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

    /// Chat rows or session rows, cut to the chats this peer may see. Applied
    /// to every peer, so a scope that changes mid-stream takes effect.
    fn scoped_stream(&self, stream: BoxStream<'static, serde_json::Value>, key: &'static str) -> BoxStream<'static, serde_json::Value> {
        let Some((registry, peer_id)) = self.peer.clone() else { return stream };
        stream
            .map(move |item| {
                let access = registry.access(&peer_id);
                match item {
                    serde_json::Value::Array(rows) => serde_json::Value::Array(
                        rows.into_iter()
                            .filter(|row| match (&access, row.get(key).and_then(|v| v.as_str())) {
                                (Some(access), Some(id)) => access.allows(id),
                                _ => false,
                            })
                            .collect(),
                    ),
                    other => other,
                }
            })
            .boxed()
    }

    /// The methods only a peer calls. They need the authenticated peer's id.
    async fn peer_method(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let Some((registry, peer_id)) = self.peer.clone() else {
            return Err(RpcError::Failed(format!("{method} needs a paired device")));
        };
        let failed = RpcError::Failed;
        let root = registry.transfers_root();
        match method {
            wire::PEER_GIT_PUT => {
                let p: wire::PeerGitPutParams = serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(p.data.as_bytes()).map_err(|e| RpcError::BadParams(format!("data is not base64: {e}")))?;
                let received = tokio::task::spawn_blocking(move || crate::peer_git::put_chunk(&root, &peer_id, &p.transfer_id, p.offset, &bytes))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
                RpcReply::value(&wire::PeerGitPutReply { received })
            }
            wire::PEER_GIT_APPLY => {
                let p: wire::PeerGitApplyParams = serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
                let repo = self.space_path(&p.space_id).await.map_err(failed)?;
                let space_id = p.space_id.clone();
                let mut reply = tokio::task::spawn_blocking(move || crate::peer_git::apply_bundle(&root, &peer_id, &repo, &p.transfer_id, &p.branch, p.size))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
                reply.space_id = space_id;
                RpcReply::value(&reply)
            }
            wire::PEER_GIT_BUNDLE => {
                let p: wire::PeerGitBundleParams = serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
                let repo = self.space_path(&p.space_id).await.map_err(failed)?;
                let reply = tokio::task::spawn_blocking(move || crate::peer_git::make_bundle(&root, &peer_id, &repo, &p.branch, p.base.as_deref()))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
                RpcReply::value(&reply)
            }
            wire::PEER_GIT_READ => {
                let p: wire::PeerGitReadParams = serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
                let (bytes, eof) = tokio::task::spawn_blocking(move || crate::peer_git::read_chunk(&root, &peer_id, &p.transfer_id, p.offset))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
                RpcReply::value(&wire::PeerGitReadReply { data: base64::engine::general_purpose::STANDARD.encode(bytes), eof })
            }
            wire::PEER_REPORTS => {
                let reports = tokio::task::spawn_blocking(move || registry.pending_reports(&peer_id, wire::PEER_REPORTS_PAGE))
                    .await
                    .map_err(|e| failed(e.to_string()))?;
                RpcReply::value(&wire::PeerReportsReply { reports })
            }
            wire::PEER_REPORTS_ACK => {
                let p: wire::PeerReportsAckParams = serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
                let removed = tokio::task::spawn_blocking(move || registry.ack_reports(&peer_id, &p.ids))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(|e| failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "removed": removed }))
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

#[async_trait]
impl RpcService for PeerGate {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let params = match self.vet(method, params).await {
            Ok(params) => params,
            Err(reason) => {
                tracing::warn!(method, %reason, "refused a paired peer's call");
                return Err(RpcError::Failed(reason));
            }
        };
        self.note_activity();
        if is_peer_only(method) {
            return self.peer_method(method, params).await;
        }
        let reply = self.inner.handle(method, params).await?;
        Ok(match (method, reply) {
            (methods::WATCH_CHATS, RpcReply::Stream(stream)) => RpcReply::Stream(self.scoped_stream(stream, "id")),
            (methods::WATCH_SESSIONS, RpcReply::Stream(stream)) => RpcReply::Stream(self.scoped_stream(stream, "chatId")),
            (_, reply) => reply,
        })
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
        assert!(gate.handle(methods::MUTATE, serde_json::json!({"op": "createChat", "chatId": "chat-1", "deviceId": "device-b"})).await.is_ok());
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
        let elsewhere = serde_json::json!({"op": "createChat", "chatId": "chat-2", "deviceId": "device-a"});
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
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while pairing.call(PEER_PAIR_METHOD, serde_json::json!({"label": "again"})).await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("a pairing socket closes after one redeem attempt");
        assert_eq!(registry.list().len(), 1);

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

    /// An engine stand-in with a chat list that grows when a chat is created.
    struct Fake {
        calls: Mutex<Vec<(String, serde_json::Value)>>,
        chats: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(chats: &[&str]) -> Arc<Self> {
            Arc::new(Self { calls: Mutex::new(Vec::new()), chats: Mutex::new(chats.iter().map(|c| c.to_string()).collect()) })
        }

        fn rows(&self, key: &str) -> serde_json::Value {
            serde_json::Value::Array(self.chats.lock().unwrap().iter().map(|id| serde_json::json!({ key: id })).collect())
        }

        fn forwarded(&self, method: &str) -> Vec<serde_json::Value> {
            self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
        }
    }

    #[async_trait]
    impl RpcService for Fake {
        async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
            self.calls.lock().unwrap().push((method.to_string(), params.clone()));
            match method {
                methods::LOCAL_DEVICE => RpcReply::value(&serde_json::json!({ "deviceId": "device-b" })),
                methods::WATCH_CHATS => Ok(RpcReply::Stream(futures::stream::iter(vec![self.rows("id")]).boxed())),
                methods::WATCH_SESSIONS => Ok(RpcReply::Stream(futures::stream::iter(vec![self.rows("chatId")]).boxed())),
                methods::WATCH_SPACES => Ok(RpcReply::Stream(
                    futures::stream::iter(vec![serde_json::json!([
                        { "id": "space-1", "deviceId": "device-b", "path": "/repo" },
                        { "id": "space-far", "deviceId": "device-a", "path": "/elsewhere" }
                    ])])
                    .boxed(),
                )),
                methods::MUTATE if params["op"] == "createChat" => {
                    self.chats.lock().unwrap().push(params["chatId"].as_str().unwrap().to_string());
                    RpcReply::value(&serde_json::json!({}))
                }
                _ => RpcReply::value(&serde_json::json!({ "handled": method })),
            }
        }
    }

    async fn seen(gate: &Arc<dyn RpcService>, method: &str, key: &str) -> Vec<String> {
        let RpcReply::Stream(mut stream) = gate.handle(method, serde_json::json!({})).await.unwrap() else { panic!("a stream") };
        let rows = stream.next().await.unwrap();
        rows.as_array().unwrap().iter().map(|r| r[key].as_str().unwrap().to_string()).collect()
    }

    fn paired_gate(registry: &Arc<PeerRegistry>, fake: &Arc<Fake>, label: &str) -> Arc<dyn RpcService> {
        let token = pair(registry, label);
        let authenticate = registry.authenticator(fake.clone());
        authenticate(Some(&token)).unwrap().service
    }

    fn create(chat: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut params = serde_json::json!({ "op": "createChat", "chatId": chat, "spaceId": "space-1" });
        params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        params
    }

    #[tokio::test]
    async fn a_peer_with_own_scope_sees_and_drives_only_the_chats_it_created() {
        let (_dir, registry) = registry();
        let fake = Fake::new(&["client-work", "other-peers-chat"]);
        let gate = paired_gate(&registry, &fake, "fedora");
        assert!(seen(&gate, methods::WATCH_CHATS, "id").await.is_empty(), "unrelated chats are invisible");
        assert!(seen(&gate, methods::WATCH_SESSIONS, "chatId").await.is_empty());
        for (method, params) in [
            (methods::QUEUE_MESSAGE, serde_json::json!({"chatId": "client-work", "text": "hi"})),
            (methods::QUEUE_COMMAND, serde_json::json!({"chatId": "client-work", "command": {"kind": "steer"}})),
            (methods::WATCH_DOC_MESSAGES, serde_json::json!({"chatId": "client-work"})),
            (methods::MUTATE, serde_json::json!({"op": "setChatArchived", "chatId": "client-work"})),
            (methods::MUTATE, serde_json::json!({"op": "renameChat", "chatId": "client-work", "title": "x"})),
        ] {
            assert!(refused(gate.handle(method, params).await).contains("not created by this paired peer"), "{method}");
        }
        assert!(refused(gate.handle(methods::MUTATE, serde_json::json!({"op": "createChat", "chatId": "w0"})).await).contains("in a Space"));
        assert!(refused(gate.handle(methods::MUTATE, create("client-work", serde_json::json!({}))).await).contains("already in use"), "an existing chat is never claimed");
        assert!(
            refused(gate.handle(methods::MUTATE, create("w0", serde_json::json!({"sideOf": "client-work"}))).await).contains("not created by this paired peer"),
            "a side chat copies another chat's placement"
        );

        gate.handle(methods::MUTATE, create("w1", serde_json::json!({"parentChatId": "mac-manager"}))).await.unwrap();
        let forwarded = fake.forwarded(methods::MUTATE);
        assert_eq!(forwarded.len(), 1);
        assert!(forwarded[0].get("parentChatId").is_none(), "the engine never sees a parent it cannot look up");
        assert_eq!(seen(&gate, methods::WATCH_CHATS, "id").await, ["w1"]);
        assert_eq!(seen(&gate, methods::WATCH_SESSIONS, "chatId").await, ["w1"]);
        assert!(gate.handle(methods::QUEUE_MESSAGE, serde_json::json!({"chatId": "w1", "text": "go"})).await.is_ok());
        assert!(gate.handle(methods::MUTATE, serde_json::json!({"op": "setChatArchived", "chatId": "w1"})).await.is_ok());
        assert!(gate.handle(methods::MUTATE, create("w1", serde_json::json!({}))).await.is_ok(), "a retried create of its own chat passes");
    }

    #[tokio::test]
    async fn two_peers_never_see_each_others_chats() {
        let (_dir, registry) = registry();
        let fake = Fake::new(&[]);
        let fedora = paired_gate(&registry, &fake, "fedora");
        let laptop = paired_gate(&registry, &fake, "laptop");
        fedora.handle(methods::MUTATE, create("w1", serde_json::json!({}))).await.unwrap();
        laptop.handle(methods::MUTATE, create("w2", serde_json::json!({}))).await.unwrap();
        assert_eq!(seen(&fedora, methods::WATCH_CHATS, "id").await, ["w1"]);
        assert_eq!(seen(&laptop, methods::WATCH_CHATS, "id").await, ["w2"]);
        assert!(refused(laptop.handle(methods::QUEUE_MESSAGE, serde_json::json!({"chatId": "w1", "text": "x"})).await).contains("not created"));
        assert!(refused(laptop.handle(methods::MUTATE, create("w1", serde_json::json!({}))).await).contains("already in use"));
    }

    #[tokio::test]
    async fn a_pairing_from_before_scopes_keeps_reaching_every_chat_until_it_is_narrowed() {
        let dir = tempfile::tempdir().unwrap();
        let token = "legacy-token";
        // The file an older engine wrote: no scope, no chats, no reports.
        std::fs::write(
            dir.path().join(STATE_FILE),
            serde_json::json!({"peers": [{"id": "legacy-1", "label": "old-fedora", "tokenHash": hash(token), "createdAtMs": now_ms(), "lastUsedMs": now_ms()}]}).to_string(),
        )
        .unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        assert_eq!(registry.list()[0].scope, PeerScope::All);
        let fake = Fake::new(&["client-work"]);
        let gate = registry.authenticator(fake.clone())(Some(token)).unwrap().service;
        assert_eq!(seen(&gate, methods::WATCH_CHATS, "id").await, ["client-work"], "an upgrade breaks nothing");
        assert!(gate.handle(methods::QUEUE_MESSAGE, serde_json::json!({"chatId": "client-work", "text": "x"})).await.is_ok());
        // Chats it creates are still recorded, so narrowing later keeps them.
        gate.handle(methods::MUTATE, serde_json::json!({"op": "createChat", "chatId": "w1"})).await.unwrap();

        assert_eq!(registry.set_scope("old-fedora", PeerScope::Own).unwrap(), 1);
        assert_eq!(seen(&gate, methods::WATCH_CHATS, "id").await, ["w1"], "narrowing applies to the open connection");
        assert!(refused(gate.handle(methods::QUEUE_MESSAGE, serde_json::json!({"chatId": "client-work", "text": "x"})).await).contains("not created"));
        assert_eq!(registry.set_scope("legacy-1", PeerScope::All).unwrap(), 1);
        assert_eq!(seen(&gate, methods::WATCH_CHATS, "id").await, ["client-work", "w1"]);
        assert!(registry.set_scope("nobody", PeerScope::Own).unwrap() == 0);
    }

    #[tokio::test]
    async fn a_new_pairing_starts_with_own_scope() {
        let (_dir, registry) = registry();
        pair(&registry, "fedora");
        assert_eq!(registry.list()[0].scope, PeerScope::Own);
    }

    #[tokio::test]
    async fn a_worker_report_waits_for_the_peer_and_survives_a_lost_acknowledgement() {
        let (dir, registry) = registry();
        let fake = Fake::new(&[]);
        let gate = paired_gate(&registry, &fake, "fedora");
        let other = paired_gate(&registry, &fake, "laptop");
        gate.handle(methods::MUTATE, create("w1", serde_json::json!({"parentChatId": "mac-manager"}))).await.unwrap();
        gate.handle(methods::MUTATE, create("w-orphan", serde_json::json!({}))).await.unwrap();

        let sink: &dyn RemoteReportSink = &*registry;
        assert!(sink.report("not-a-peer-chat", None, "x").is_none(), "other chats keep the local path");
        assert!(sink.report("w-orphan", None, "x").is_none(), "a worker with no named parent has nobody to tell");
        let first = sink.report("w1", Some("Build"), "STATUS: done").unwrap().unwrap();
        assert_eq!((first.parent_chat_id.as_str(), first.delivery.as_str()), ("mac-manager", "queuedForRemoteParent"));
        sink.report("w1", Some("Build"), "STATUS: done").unwrap().unwrap();
        assert!(sink.report("w1", None, "  ").unwrap().is_err(), "an empty report is refused");

        let waiting = |gate: Arc<dyn RpcService>| async move {
            let RpcReply::Value(value) = gate.handle(wire::PEER_REPORTS, serde_json::json!({})).await.unwrap() else { panic!("a value") };
            serde_json::from_value::<wire::PeerReportsReply>(value).unwrap().reports
        };
        let reports = waiting(gate.clone()).await;
        assert_eq!(reports.len(), 1, "a retried report is one report");
        assert_eq!((reports[0].chat_id.as_str(), reports[0].title.as_deref(), reports[0].parent_chat_id.as_str()), ("w1", Some("Build"), "mac-manager"));
        assert!(waiting(other.clone()).await.is_empty(), "another peer cannot see them");

        // The reply was lost: the same report is offered again, until it is acknowledged.
        assert_eq!(waiting(gate.clone()).await, reports);
        let reopened = PeerRegistry::open(dir.path()).unwrap();
        let fedora = registry.list().into_iter().find(|p| p.label == "fedora").unwrap().id;
        assert_eq!(reopened.pending_reports(&fedora, 10).len(), 1, "a restart keeps what was not acknowledged");
        let ack = serde_json::json!({ "ids": [reports[0].id] });
        let RpcReply::Value(foreign) = other.handle(wire::PEER_REPORTS_ACK, ack.clone()).await.unwrap() else { panic!() };
        assert_eq!(foreign["removed"], 0, "another peer cannot acknowledge them");
        let RpcReply::Value(done) = gate.handle(wire::PEER_REPORTS_ACK, ack.clone()).await.unwrap() else { panic!() };
        assert_eq!(done["removed"], 1);
        assert!(waiting(gate.clone()).await.is_empty());
        let RpcReply::Value(again) = gate.handle(wire::PEER_REPORTS_ACK, ack).await.unwrap() else { panic!() };
        assert_eq!(again["removed"], 0, "a repeated acknowledgement is harmless");
    }

    #[tokio::test]
    async fn revoking_a_peer_drops_its_reports_and_chat_records_but_not_the_chats() {
        let (_dir, registry) = registry();
        let fake = Fake::new(&[]);
        let gate = paired_gate(&registry, &fake, "fedora");
        gate.handle(methods::MUTATE, create("w1", serde_json::json!({"parentChatId": "mac-manager"}))).await.unwrap();
        let sink: &dyn RemoteReportSink = &*registry;
        sink.report("w1", None, "STATUS: done").unwrap().unwrap();
        let id = registry.list()[0].id.clone();
        assert_eq!(registry.peer_counts(&id), (1, 1));
        assert_eq!(registry.revoke("fedora").unwrap(), 1);
        assert_eq!(registry.peer_counts(&id), (0, 0));
        assert!(sink.report("w1", None, "late").is_none(), "a revoked peer's workers report locally again");
        assert!(refused(gate.handle(methods::WATCH_CHATS, serde_json::json!({})).await).contains("no longer paired"));
        assert!(fake.chats.lock().unwrap().contains(&"w1".to_string()), "the chat itself is left for the owner");
    }

    #[tokio::test]
    async fn peer_only_methods_need_a_paired_peer_and_the_space_must_be_on_this_device() {
        let unpaired = PeerGate::new(Fake::new(&[]));
        assert!(refused(unpaired.handle(wire::PEER_REPORTS, serde_json::json!({})).await).contains("needs a paired device"));
        let (_dir, registry) = registry();
        let fake = Fake::new(&[]);
        let gate = paired_gate(&registry, &fake, "fedora");
        let far = serde_json::json!({"transferId": "transfer-1", "spaceId": "space-far", "branch": "x", "size": 0});
        assert!(refused(gate.handle(wire::PEER_GIT_APPLY, far).await).contains("not on this device"));
        let missing = serde_json::json!({"spaceId": "nope", "branch": "x"});
        assert!(refused(gate.handle(wire::PEER_GIT_BUNDLE, missing).await).contains("no such Space"));
        let routed = serde_json::json!({"targetDeviceId": "device-a", "transferId": "t", "offset": 0, "data": ""});
        assert!(refused(gate.handle(wire::PEER_GIT_PUT, routed).await).contains("another device"));
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
