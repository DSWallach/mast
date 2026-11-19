//! WorkspaceHost — owns the per-user workspace **registry** (docs/
//! registry-sync.md; replaces the Loro workspace doc after the 2026-07/08
//! wedge incidents): local snapshot persistence, edge room sync
//! (`/registry/{orgId}/ws` → room `reg1/{orgId}/{userId}`, offline-tolerant —
//! spaces/sessions are private to their owner, never org-visible), the device
//! registry row for THIS device, and the typed watch channels the
//! WatchChats/WatchDevices/WatchSessions/WatchSidebarPreferences RPC streams
//! are fed from.
//!
//! Writer discipline (kept from the doc schema): this host writes its own device row,
//! its own session-status rows, and rows for chats it hosts; renames/archives are LWW
//! sets accepted from any device (the Mutate surface).
//!
//! Liveness: `lastSeenAt` is a row write on boot/shutdown ONLY — the periodic 15s
//! heartbeat rides the room's presence frames (memory-only on the DO), so staying
//! online never grows server state.
//!
//! Migration: first boot after the update finds no `registry1` snapshot, reads
//! the legacy `workspace2` Loro snapshot, and seeds the registry from it as
//! pending upserts (historical HLCs — live writes always win). The overlay
//! serves the full sidebar before any server contact; the old `ws4` rooms are
//! simply never joined again. The legacy snapshot is kept for rollback.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use chrono::Utc;
use tokio::sync::watch;

use mast_doc::{DeletedSpace, REGISTRY_DOC_ID, RegistryDoc, WorkspaceDoc};
use mast_proto::{
    Chat, ChatConfig, ChatLinkSource, ChatPrLink, Device, Session, SidebarPreferencesState, Space,
};
use mast_sync::{DocsStore, RegistryClient, RegistryTuning};

use crate::doc_host::EdgeConfig;
use crate::http_error::describe_http_error;
use crate::{EngineError, now_ms};

fn chat_item_title(title: Option<&str>) -> String {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("New chat")
        .chars()
        .take(mast_proto::work_outline::limits::MAX_TITLE_CHARS)
        .collect()
}

/// Legacy Loro workspace snapshot row — now only read once, as the migration
/// source for the registry seed. Kept on disk for rollback.
pub const WORKSPACE_DOC_ID: &str = "workspace2";
/// Legacy (pre-spaces) snapshot row — best-effort deleted on open.
const LEGACY_WORKSPACE_DOC_ID: &str = "workspace";
/// Org used when none is configured (matches the edge's dev-mode `user@org` bearers).
pub const DEFAULT_ORG_ID: &str = "dev-org";
/// User used when none is configured (dev mode without a bearer).
pub const DEFAULT_USER_ID: &str = "dev-user";
/// Presence beat cadence.
const PRESENCE_INTERVAL_MS: u64 = 15_000;
/// Dial-gate thresholds (`peer_liveness`): a device is judged `Dark` — dials
/// parked with zero network traffic — only on POSITIVE evidence: our registry
/// room joined and warm past the warm-up window, and the device's freshest
/// known heartbeat (live map ∪ session cache ∪ durable row) older than the
/// dark window. m1-laptop shape: offline 6 days, yet hot-dialed in 3-dial
/// bursts every ~60s for the life of the app.
const DIAL_GATE_WARMUP_MS: i64 = 60_000;
const DIAL_GATE_DARK_MS: i64 = 5 * 60_000;

/// A presence heartbeat younger than this marks the device alive (3 missed
/// beats = offline). Also the "peer is reachable" signal that clears the
/// peer-dial cooldown.
const PRESENCE_FRESH_MS: i64 = 45_000;
/// Relay-status probe cadence. Presence heartbeats ride the registry room, so
/// any registry pathology (or our own room connection being down) silently
/// starves them — and every device looks offline while its relay works fine.
/// Before believing "offline", ask the device's DeviceRoom
/// (`GET /device/{id}/status` → `hostConnected`), which tracks the host socket
/// authoritatively and shares no machinery with the registry room. Probes only
/// run for devices whose heartbeat is stale, so the steady state (healthy
/// room, fresh beats) sends no extra traffic.
const RELAY_PROBE_INTERVAL_MS: u64 = 30_000;
/// Per-request timeout for a relay-status probe.
const RELAY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Debounce window for local snapshot saves after a change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;
/// Initial-join retry backoff (base, cap). A first registry-room join that
/// fails must not strand the device offline until an app restart — retry until
/// it lands. Jittered so N devices restarting together don't resynchronize
/// their retries into a thundering herd on the cold DO.
pub(crate) const JOIN_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(500);
/// 16s, matching the room clients' BACKOFF_CAP: the cap only bounds the dark
/// window when every event wake (online bus, system wake, token change)
/// missed, so it trades a few extra dials/minute for a tighter worst case.
pub(crate) const JOIN_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(16);

pub(crate) async fn token_changed(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    match changes {
        Some(changes) => {
            let _ = changes.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn token_revoked(token: &Option<Arc<dyn mast_rpc::TokenSource>>) -> bool {
    match token {
        Some(token) => matches!(token.token().await, Err(mast_rpc::TokenError::SignedOut)),
        // Fixed test/dev URLs have no revocable credential source.
        None => false,
    }
}

/// Quiet-probe cadence for the registry room: fixed at 15 minutes. One room
/// per engine, so the fixed cadence costs ~100 DO wakes/day total, and the
/// probe is deadline-checked — a mute room is detected within
/// probe cadence + 10s instead of hours (2026-08-04 deaf-socket lesson).
const REGISTRY_PROBE_QUIET: std::time::Duration = std::time::Duration::from_secs(900);
/// Deaf-socket escalation: live peer presence dark this long after the
/// tripwire probe → redial on a fresh socket (see `check_presence_deafness`).
const PRESENCE_DEAF_REDIAL_MS: i64 = 60_000;

/// State for the presence deafness tripwire. Presence heartbeats ride the
/// SAME socket and the same DO broadcast fan-out as row updates, so "peers I
/// was seeing live via this room all went dark at once" is a *delivery*
/// signal, and it is bounded (~45-60s) at zero server cost — every device
/// already heartbeats each 15s. The monotonic seen-cache and the relay
/// status probe deliberately keep devices *fresh-looking* through other
/// paths; they must never feed this tripwire (they'd mask exactly the
/// failure it exists to catch — 2026-08-04 deaf-socket incident).
#[derive(Default)]
struct PresenceWatch {
    /// Armed once at least one OTHER device has been seen live via the
    /// room's presence map this session.
    armed: bool,
    /// Epoch ms when live peers first all went dark (0 = not dark).
    dark_since_ms: i64,
    /// The cheap first response (probe) already fired.
    probed: bool,
}

/// Cheap decorrelation jitter (0–500ms) without pulling in a rng — derived from
/// the sub-nanosecond wall clock. Mirrors the device relay's `jitter()`.
pub(crate) fn join_retry_jitter() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::time::Duration::from_millis(u64::from(nanos) % 500)
}

/// Durable PR/ticket links (ticket 0xx): which slot a
/// [`WorkspaceHost::set_chat_link`] write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatLinkKind {
    Pr,
    Ticket,
}

#[derive(Debug, Clone)]
pub struct WorkspaceHostConfig {
    pub device_id: String,
    /// Human name for this device's registry row (hostname by default).
    pub device_name: String,
    /// `std::env::consts::OS`-style platform string.
    pub platform: String,
    pub org_id: String,
    /// The signed-in user — registries are per-user (`reg1/{orgId}/{userId}`):
    /// spaces/sessions are private to their owner, never org-visible.
    pub user_id: String,
    /// When present, the host joins `/registry/{orgId}/ws`. `None` = fully offline
    /// (local snapshots only; the registry still drives everything device-side).
    pub edge: Option<EdgeConfig>,
}

#[derive(Default)]
struct PublishedGenerations {
    chats: Option<u64>,
    sessions: Option<u64>,
    spaces: Option<u64>,
    pr_prompt_migration: Option<(u64, u64)>,
}

struct WorkspaceHostInner {
    store: Arc<DocsStore>,
    config: WorkspaceHostConfig,
    reg: Arc<Mutex<RegistryDoc>>,
    chats_tx: watch::Sender<Vec<Chat>>,
    /// Runs before a locally archived chat is restored, while it is still archived.
    before_chat_unarchive: Mutex<Option<Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>>>,
    chat_archive_transition: Mutex<()>,
    devices_tx: watch::Sender<Vec<Device>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    spaces_tx: watch::Sender<Vec<Space>>,
    /// Registry generations of the chat/session/space rows last published,
    /// so `publish_lists` re-reads only the kinds that changed.
    published_generations: Mutex<PublishedGenerations>,
    sidebar_preferences_tx: watch::Sender<SidebarPreferencesState>,
    room: Mutex<Option<Arc<RegistryClient>>>,
    /// Bumped on every registry change (local mutation or applied server
    /// frame) — drives republish + the snapshot debounce in `workspace_task`.
    changed_tx: watch::Sender<u64>,
    /// Freshest presence heartbeat (ms) we have EVER observed per device. The
    /// room's presence map forgets entries after its 30s TTL and starts empty
    /// on a (re)join, so without this cache a receive-side hiccup snaps a
    /// device's overlay back to its boot-time row `lastSeenAt` — an instant
    /// (and false) "offline" badge for a host that beat 20s ago.
    presence_seen: Mutex<std::collections::HashMap<String, i64>>,
    /// Called with a device id whenever its presence heartbeat proves it alive —
    /// wired to `LinkCache::reset_cooldown` so a peer that comes back is dialed
    /// immediately instead of waiting out the failure backoff.
    peer_alive: Mutex<Option<PeerAliveHook>>,
    /// Deaf-socket tripwire state — see `check_presence_deafness`.
    presence_watch: Mutex<PresenceWatch>,
    /// Epoch ms of the registry room's most recent (re)join — the dial gate's
    /// warm-up clock (`peer_liveness`): a just-joined room hasn't heard
    /// anyone's heartbeat yet, and that silence must not read as "offline".
    room_joined_at: std::sync::atomic::AtomicI64,
    /// Work Outline replay-ledger keys whose registry write is not yet in a
    /// durable snapshot; claimed together with the next snapshot save.
    /// Lock order: `reg` before this.
    pending_ledger_marks: Mutex<Vec<String>>,
    /// No registry server will ever ack this replica's writes (no edge is
    /// configured and no test room is attached): the local table is the
    /// whole truth, so local writes settle into it instead of queueing
    /// forever (see `RegistryDoc::settle_pending_locally`).
    local_authority: std::sync::atomic::AtomicBool,
}

/// "This peer is alive" callback (device id) — see `WorkspaceHost::set_peer_alive_hook`.
pub type PeerAliveHook = Arc<dyn Fn(&str) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct WorkspaceHost {
    inner: Arc<WorkspaceHostInner>,
}

enum ChatOrigin {
    Parent(String),
    Scheduled(mast_proto::ScheduledChat),
    ArchivedExternal(mast_proto::ScheduledChat),
    /// A standing role's first-start chat: born hidden under this provisional
    /// marker (see `mast_proto::standing_role_provisional_tag`).
    Provisional(String),
}

impl WorkspaceHost {
    /// Load (or migrate, or init) the registry, upsert this device's row, start
    /// the change-driven task, and join the edge registry room when configured.
    pub fn open(store: Arc<DocsStore>, config: WorkspaceHostConfig) -> Result<Self, EngineError> {
        #[cfg(feature = "startup-bench")]
        let snapshot_started = std::time::Instant::now();
        let snapshot = store.load_snapshot(REGISTRY_DOC_ID)?;
        #[cfg(feature = "startup-bench")]
        eprintln!("BENCH phase | registry_snapshot_read | ms={:.3}", snapshot_started.elapsed().as_secs_f64() * 1000.0);
        #[cfg(feature = "startup-bench")]
        let materialize_started = std::time::Instant::now();
        let mut doc = match snapshot {
            Some(bytes) => RegistryDoc::from_bytes(&bytes, &config.device_id)
                .map_err(|e| EngineError::Other(format!("registry snapshot load failed: {e}")))?,
            None => {
                // MIGRATION (instant, one-time): seed from the legacy Loro
                // workspace snapshot when one exists. Seeds are pending upserts
                // with historical HLCs — the overlay serves the full sidebar
                // immediately, the room converges on first join, and any live
                // write beats a migrated value. The legacy snapshot stays on
                // disk for rollback.
                let mut doc = RegistryDoc::new(&config.device_id);
                match store.load_snapshot(WORKSPACE_DOC_ID) {
                    Ok(Some(bytes)) => {
                        let raw = loro::LoroDoc::new();
                        match raw.import(&bytes) {
                            Ok(_) => {
                                let legacy = WorkspaceDoc::from_doc(raw);
                                match legacy.read_all() {
                                    Ok(state) => match doc.seed_from_workspace(&state) {
                                        Ok(rows) => {
                                            tracing::info!(
                                                rows,
                                                "migrated legacy workspace doc into the registry"
                                            );
                                        }
                                        Err(err) => {
                                            tracing::warn!(error = %err, "workspace migration seed failed");
                                        }
                                    },
                                    Err(err) => {
                                        tracing::warn!(error = %err, "legacy workspace read failed; starting empty");
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "legacy workspace import failed; starting empty");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "legacy workspace snapshot load failed; starting empty");
                    }
                }
                doc
            }
        };
        #[cfg(feature = "startup-bench")]
        eprintln!("BENCH phase | registry_decode | ms={:.3}", materialize_started.elapsed().as_secs_f64() * 1000.0);
        // Destructive-break hygiene: the pre-spaces row stays unreachable.
        store.delete_snapshot(LEGACY_WORKSPACE_DOC_ID).ok();

        // Boot: upsert our own device row. A user-set name (RenameDevice is LWW from
        // any device) survives restarts. The old fallback sentinel is repaired with
        // the platform-resolved name because it was never a user-selected name.
        let now = Utc::now();
        let existing = doc
            .read_devices()?
            .into_iter()
            .find(|d| d.id == config.device_id);
        // Keep the per-harness instruction tokens the last probe stamped;
        // the post-boot restamp replaces them with the fresh probe result.
        let capabilities = mast_proto::capabilities::current_preserving_instruction_harnesses(
            existing
                .as_ref()
                .map(|d| d.capabilities.as_slice())
                .unwrap_or_default(),
        );
        doc.upsert_device(&Device {
            id: config.device_id.clone(),
            name: device_name_on_boot(
                existing.as_ref().map(|device| device.name.as_str()),
                &config.device_name,
            ),
            platform: config.platform.clone(),
            last_seen_at: Some(now),
            // First registration stamps `createdAt`; restarts keep the original
            // (the Devices page "Added …" fragment).
            created_at: existing.and_then(|d| d.created_at).or(Some(now)),
            // Every boot restamps the running binary's version (fleet staleness
            // on the Devices page; workspace version — same for every crate).
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            cursor_sdk_version: Some(mast_harness::CursorHarness::sdk_version().into()),
            capabilities,
        })?;

        // Materialize the previous implicit prompts for existing Spaces. Each
        // row is initialized only when absent, so retries preserve user edits.
        if config.edge.is_none()
            && let Err(error) = migrate_space_pr_prompts(&mut doc, now.timestamp_millis())
        {
            tracing::warn!(%error, "PR prompt migration failed");
        }

        #[cfg(feature = "startup-bench")]
        let settle_started = std::time::Instant::now();
        let local_authority = config.edge.is_none();
        if local_authority && doc.settle_pending_locally() {
            tracing::info!("settled the local registry's queued writes");
        }
        #[cfg(feature = "startup-bench")]
        eprintln!("BENCH phase | registry_settle | ms={:.3}", settle_started.elapsed().as_secs_f64() * 1000.0);
        #[cfg(feature = "startup-bench")]
        let read_started = std::time::Instant::now();
        let state = doc.read_all()?;
        #[cfg(feature = "startup-bench")]
        eprintln!("BENCH phase | registry_materialize | ms={:.3}", read_started.elapsed().as_secs_f64() * 1000.0);
        let (chats_tx, _) = watch::channel(state.chats);
        let (devices_tx, _) = watch::channel(state.devices);
        let (sessions_tx, _) = watch::channel(state.sessions);
        let (spaces_tx, _) = watch::channel(state.spaces);
        let preferences = doc.sidebar_preferences();
        let (sidebar_preferences_tx, _) = watch::channel(SidebarPreferencesState {
            local_handoffs: if local_authority {
                doc.completed_sidebar_handoffs()
            } else {
                Vec::new()
            },
            revision: 0,
            synced: false,
            initialized: preferences.is_some(),
            sections: preferences
                .as_ref()
                .map(|p| p.sections.clone())
                .unwrap_or_default(),
            pinned_session_ids: preferences
                .map(|preferences| preferences.pinned_session_ids)
                .unwrap_or_default(),
        });
        let (changed_tx, changed_rx) = watch::channel(0u64);

        let host = Self {
            inner: Arc::new(WorkspaceHostInner {
                store,
                config,
                reg: Arc::new(Mutex::new(doc)),
                chats_tx,
                before_chat_unarchive: Mutex::new(None),
                chat_archive_transition: Mutex::new(()),
                devices_tx,
                sessions_tx,
                spaces_tx,
                published_generations: Mutex::new(PublishedGenerations::default()),
                sidebar_preferences_tx,
                room: Mutex::new(None),
                changed_tx,
                presence_seen: Mutex::new(std::collections::HashMap::new()),
                peer_alive: Mutex::new(None),
                presence_watch: Mutex::new(PresenceWatch::default()),
                room_joined_at: std::sync::atomic::AtomicI64::new(0),
                pending_ledger_marks: Mutex::new(Vec::new()),
                local_authority: std::sync::atomic::AtomicBool::new(local_authority),
            }),
        };
        // Persist immediately: after this boot the migration source is never
        // read again, so the registry snapshot must exist even if the process
        // dies before the first debounced save.
        #[cfg(feature = "startup-bench")]
        let save_started = std::time::Instant::now();
        host.inner.save_snapshot();
        #[cfg(feature = "startup-bench")]
        eprintln!("BENCH phase | registry_save | ms={:.3}", save_started.elapsed().as_secs_f64() * 1000.0);
        host.join_room();
        tokio::spawn(workspace_task(Arc::downgrade(&host.inner), changed_rx));
        if host.inner.config.edge.is_some() {
            tokio::spawn(relay_probe_task(Arc::downgrade(&host.inner)));
        }
        Ok(host)
    }

    /// Edge room join — offline-tolerant: a failed join logs and stays local-first.
    fn join_room(&self) {
        let Some(edge) = &self.inner.config.edge else {
            return;
        };
        let org_id = self.inner.config.org_id.clone();
        // Per-dial URL provider: the bearer is re-read on every (re)connect.
        let url = edge.room_url(format!("/registry/{org_id}/ws"));
        self.spawn_join(url, edge.token_changes(), Some(edge.token.clone()));
    }

    /// Test seam: join a registry room at a fixed WebSocket URL without an
    /// `EdgeConfig` — integration tests wire hosts to an in-process mock
    /// server through this. Production always goes through [`Self::join_room`].
    #[doc(hidden)]
    pub fn connect_registry_url(&self, url: &str) {
        // From here on a server acks (and must receive) every write; writes
        // that settled locally go out as re-seeds with their original clocks.
        {
            let mut doc = lock(&self.inner.reg);
            if self
                .inner
                .local_authority
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                doc.queue_rows_for_server();
            }
        }
        self.spawn_join(Arc::new(mast_sync::StaticUrl(url.to_string())), None, None);
    }

    fn spawn_join(
        &self,
        url: Arc<dyn mast_sync::UrlProvider>,
        mut token_changes: Option<tokio::sync::watch::Receiver<u64>>,
        token: Option<Arc<dyn mast_rpc::TokenSource>>,
    ) {
        let org_id = self.inner.config.org_id.clone();
        let reg = self.inner.reg.clone();
        let device_id = self.inner.config.device_id.clone();
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut wake = mast_sync::wake::subscribe();
            let mut online = mast_sync::wake::subscribe_online();
            // `RegistryClient` only self-reconnects AFTER a first successful
            // join; an INITIAL failure (a 500 from an overloaded DO, a token
            // racing a refresh, an edge deploy) must not end this task and
            // leave the device offline until an app restart. Retry the first
            // join on a capped, jittered backoff so a transient blip self-heals.
            let mut backoff = JOIN_RETRY_BASE;
            loop {
                if weak.upgrade().is_none() {
                    return; // host dropped
                }
                let tuning = RegistryTuning {
                    probe_quiet: REGISTRY_PROBE_QUIET,
                };
                // Production path (token present): dual transport — the WS
                // dials as before, and a plain-HTTPS pull/push seam derived
                // from the same URL provider bootstraps the doc in ~1 RTT
                // and keeps syncing at backoff cadence when the socket can't
                // connect (airplane-wifi networks strip WS upgrades).
                let result = if token.is_some() {
                    RegistryClient::connect_via_transport(
                        url.clone(),
                        reg.clone(),
                        &device_id,
                        tuning,
                        Arc::new(WsDerivedRegistryTransport::new(url.clone())),
                    )
                    .await
                } else {
                    RegistryClient::connect_via_tuned(url.clone(), reg.clone(), &device_id, tuning)
                        .await
                };
                match result {
                    Ok(client) => {
                        let client = Arc::new(client);
                        client.set_presence(now_ms());
                        let mut events = client.events();
                        if token_revoked(&token).await {
                            return;
                        }
                        let Some(inner) = weak.upgrade() else { return };
                        *lock(&inner.room) = Some(client.clone());
                        inner
                            .room_joined_at
                            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                        inner.bump_changed();
                        tracing::info!(org = %org_id, "registry room joined");
                        drop(inner);
                        // The slot is the sole owner. This lets engine-level
                        // revocation close the socket synchronously by taking it.
                        drop(client);
                        // The event pump lives for the client's whole life
                        // (across its self-reconnects); it ends only when the
                        // client is dropped at host teardown.
                        loop {
                            tokio::select! {
                                event = events.recv() => match event {
                                    Ok(mast_sync::RegistryEvent::Connected) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        // Re-join: restart the dial gate's
                                        // warm-up clock (presence map is empty
                                        // again; silence isn't evidence yet).
                                        inner
                                            .room_joined_at
                                            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                                        inner.bump_changed();
                                    }
                                    Ok(mast_sync::RegistryEvent::Applied) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        inner.bump_changed();
                                    }
                                    Ok(mast_sync::RegistryEvent::Presence) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        inner.publish();
                                    }
                                    Ok(mast_sync::RegistryEvent::Disconnected) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                },
                                _ = token_changed(&mut token_changes) => {
                                    if token_revoked(&token).await {
                                        tracing::info!(org = %org_id,
                                            "registry credentials removed; leaving room");
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(inner) = weak.upgrade() {
                            *lock(&inner.room) = None;
                        }
                        return;
                    }
                    Err(err) => {
                        tracing::warn!(org = %org_id, error = %err, backoff_ms = backoff.as_millis() as u64,
                            "registry room join failed; retrying");
                    }
                }
                // Drain stale online events so only successes DURING this
                // wait cut it short (our own failed dial doesn't count).
                while online.try_recv().is_ok() {}
                tokio::select! {
                    _ = tokio::time::sleep(backoff + join_retry_jitter()) => {
                        backoff = (backoff * 2).min(JOIN_RETRY_CAP);
                    }
                    _ = wake.recv() => {
                        backoff = JOIN_RETRY_BASE;
                    }
                    _ = online.recv() => {
                        backoff = JOIN_RETRY_BASE;
                    }
                    _ = token_changed(&mut token_changes) => {
                        if token_revoked(&token).await {
                            return;
                        }
                        backoff = JOIN_RETRY_BASE;
                    }
                }
            }
        });
    }

    /// Close the current registry membership before account-scoped state is
    /// drained. The auth signal prevents an in-flight join from replacing it.
    pub fn disconnect_edge(&self) {
        lock(&self.inner.room).take();
    }

    /// Wire the "peer is alive" signal (fresh presence heartbeat) to a callback —
    /// the engine points this at `LinkCache::reset_cooldown`.
    pub fn set_peer_alive_hook(&self, hook: PeerAliveHook) {
        *lock(&self.inner.peer_alive) = Some(hook);
    }

    /// Test seam: write a foreign device row (the relay version gate and the
    /// dial gate read these; production rows arrive via registry sync).
    #[doc(hidden)]
    pub fn upsert_device_row(&self, device: &Device) {
        if let Err(err) = self.mutate(|doc| doc.upsert_device(device)) {
            tracing::warn!(error = %err, "device row upsert failed");
        }
    }

    /// Test seam: apply registry rows as if a peer (or a newer build) had
    /// written them (malformed-row tests; production rows arrive via sync).
    #[doc(hidden)]
    pub fn apply_registry_rows(&self, seq: u64, rows: Vec<mast_doc::RegistryRow>) -> bool {
        self.mutate(|doc| doc.apply_rows(seq, rows))
    }

    /// Dial-gate verdict for `LinkCache` (park dials to registry-dark peers).
    /// `Dark` requires positive evidence; every ambiguous state — registry
    /// room down or still in its warm-up window, unknown device, heartbeat in
    /// the gray zone — stays `Unknown`, because a dead registry room (rows
    /// down, relay fine: the 2026-08-18 03:45 incident shape) must never
    /// park the relay. Un-park is presence-driven: heartbeats resume → the
    /// verdict flips and the peer-alive hook clears any dial cooldown.
    pub fn peer_liveness(&self, device_id: &str) -> mast_rpc::PeerLiveness {
        use mast_rpc::PeerLiveness::{Dark, Live, Unknown};
        if device_id == self.inner.config.device_id {
            return Live;
        }
        let now = now_ms();
        let (connected, live_beat) = {
            let room = lock(&self.inner.room);
            match room.as_ref() {
                Some(room) => (
                    room.stats().connected,
                    room.presence().get(device_id).copied(),
                ),
                None => (false, None),
            }
        };
        if !connected {
            return Unknown;
        }
        let cached = lock(&self.inner.presence_seen).get(device_id).copied();
        match live_beat.into_iter().chain(cached).max() {
            Some(ms) if now.saturating_sub(ms) < PRESENCE_FRESH_MS => Live,
            Some(ms) if now.saturating_sub(ms) >= DIAL_GATE_DARK_MS => Dark,
            Some(_) => Unknown,
            None => {
                // Never heard this device beat. Silence is only evidence once
                // OUR room has been joined long enough to have heard it.
                let joined_at = self
                    .inner
                    .room_joined_at
                    .load(std::sync::atomic::Ordering::Relaxed);
                if joined_at == 0 || now.saturating_sub(joined_at) < DIAL_GATE_WARMUP_MS {
                    return Unknown;
                }
                // Fall back to the durable row: a device whose last stamped
                // sighting is ancient AND which isn't beating now is dark.
                let row_seen = self
                    .read(|doc| doc.read_devices())
                    .ok()
                    .into_iter()
                    .flatten()
                    .find(|d| d.id == device_id)
                    .and_then(|d| d.last_seen_at)
                    .map(|at| at.timestamp_millis());
                match row_seen {
                    Some(ms) if now.saturating_sub(ms) >= DIAL_GATE_DARK_MS => Dark,
                    _ => Unknown,
                }
            }
        }
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// This engine's device-local store directory (sidecar files that never
    /// sync, e.g. `StartPrReview` replies).
    pub(crate) fn store_root(&self) -> std::path::PathBuf {
        self.inner.store.root().to_path_buf()
    }

    pub fn connected(&self) -> bool {
        lock(&self.inner.room)
            .as_ref()
            .is_some_and(|room| room.stats().connected)
    }

    /// True once this replica has applied a server state this process (WS
    /// hello answer or HTTPS pull) — or when no edge is configured at all
    /// (local-only: the local table IS the whole truth). Destructive repair
    /// (the orphan sweep) gates on this: a replica that has not heard from
    /// the server this boot may be arbitrarily behind (gapped cursor,
    /// pre-heal, offline at boot), and "row absent locally" on such a view
    /// is not evidence of deletion.
    pub fn registry_synced(&self) -> bool {
        if self.inner.config.edge.is_none() {
            return true;
        }
        lock(&self.inner.room)
            .as_ref()
            .is_some_and(|room| room.stats().synced)
    }

    /// Probe the registry room's liveness NOW (window-focus sweep). Probes are
    /// deadline-checked in the client: an unanswered probe tears the session
    /// down for a fresh socket, so a deaf-receiving room (2026-08-04 incident)
    /// heals within seconds of the user looking at the app.
    pub fn probe(&self) {
        if let Some(room) = lock(&self.inner.room).as_ref() {
            room.probe();
        }
    }

    /// Registry room introspection for SyncStatus / `zeron sync`.
    /// `None` = no room yet (edge-less, or the initial join is still retrying).
    pub fn sync_status(&self) -> Option<mast_sync::RoomStatsSnapshot> {
        lock(&self.inner.room).as_ref().map(|room| room.stats())
    }

    /// The registry room's reconnect posture (next-dial deadline + sticky
    /// last failure) for the connectivity stream. `None` = no room yet.
    pub fn reconnect_state(&self) -> Option<mast_sync::ReconnectState> {
        lock(&self.inner.room)
            .as_ref()
            .map(|room| room.reconnect_state())
    }

    /// Whether this engine was configured with edge transports at all
    /// (`false` = local profile: connectivity is `Disabled`, hide the pill).
    pub fn edge_expected(&self) -> bool {
        self.inner.config.edge.is_some()
    }

    // ── registry access helpers ─────────────────────────────────────────────

    /// Run a mutation under the registry lock, then wake the publish/persist
    /// task and push the write to the room.
    fn mutate<R>(&self, f: impl FnOnce(&mut RegistryDoc) -> R) -> R {
        let result = {
            let mut doc = lock(&self.inner.reg);
            let result = f(&mut doc);
            self.inner.settle_if_local(&mut doc);
            result
        };
        self.inner.bump_changed();
        if let Some(room) = lock(&self.inner.room).as_ref() {
            room.nudge();
        }
        result
    }

    fn read<R>(&self, f: impl FnOnce(&RegistryDoc) -> R) -> R {
        f(&lock(&self.inner.reg))
    }

    /// The chat row as currently known (overlay view).
    pub fn chat(&self, chat_id: &str) -> Result<Option<Chat>, EngineError> {
        Ok(self.read(|doc| doc.chat(chat_id))?)
    }

    /// The space row as currently known (overlay view).
    pub fn space(&self, space_id: &str) -> Result<Option<Space>, EngineError> {
        Ok(self.read(|doc| doc.space(space_id))?)
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, EngineError> {
        Ok(self.read(|doc| doc.read_chats())?)
    }

    pub fn hand_off_chat_children(&self, from: &str, to: &str, actor: &str, at_ms: i64) -> Result<(Vec<String>, bool), String> {
        self.mutate(|doc| doc.hand_off_chat_children(from, to, actor, at_ms))
    }

    pub fn standing_role(&self, role: mast_proto::StandingRoleKind, scope_item_id: &str) -> Option<mast_proto::StandingRole> {
        self.read(|doc| doc.standing_role(role, scope_item_id))
    }

    pub fn manager_role_upgrade(
        &self,
        chat_id: &str,
    ) -> mast_doc::work_outline::instructions::ManagerRoleUpgrade {
        self.read(|doc| doc.manager_role_upgrade(chat_id))
    }

    pub fn chat_is_manager(&self, chat_id: &str) -> bool {
        self.read(|doc| doc.chat_is_manager(chat_id))
    }

    pub fn standing_role_chat(&self, role: mast_proto::StandingRoleKind, scope_item_id: &str) -> Option<String> {
        self.read(|doc| doc.standing_role_chat(role, scope_item_id))
    }

    pub fn standing_roles(&self) -> Vec<mast_proto::StandingRole> {
        self.read(|doc| doc.standing_roles())
    }

    /// The role `chat_id` holds right now; a superseded chat holds none.
    pub fn standing_role_of_chat(&self, chat_id: &str) -> Option<mast_proto::StandingRole> {
        self.read(|doc| doc.standing_role_of_chat(chat_id))
    }

    pub fn adopt_standing_role(
        &self,
        role: mast_proto::StandingRoleKind,
        scope_item_id: &str,
        chat_id: &str,
        fallback_chain: &[mast_proto::StandingRoleModel],
    ) -> Result<mast_proto::StandingRole, String> {
        // Only the chat's own host adopts it: that is the device whose
        // hand-offs rotate the role (and it runs this build, which supports
        // `standing-role-handoff-v1`).
        let host = self.read(|doc| doc.chat(chat_id)).map_err(|e| e.to_string())?
            .ok_or("chat does not exist")?.device_id;
        if host != self.device_id() {
            return Err("a role can only be adopted on the chat's own host".into());
        }
        self.mutate(|doc| doc.adopt_standing_role(role, scope_item_id, chat_id, fallback_chain))
    }

    /// Adopt the role's provisional first-start chat and clear its marker in
    /// one registry batch (see `RegistryDoc::activate_standing_role_chat`).
    pub fn activate_standing_role_chat(
        &self,
        role: mast_proto::StandingRoleKind,
        scope_item_id: &str,
        chat_id: &str,
        fallback_chain: &[mast_proto::StandingRoleModel],
    ) -> Result<mast_proto::StandingRole, String> {
        let host = self.read(|doc| doc.chat(chat_id)).map_err(|e| e.to_string())?
            .ok_or("chat does not exist")?.device_id;
        if host != self.device_id() {
            return Err("a role can only be adopted on the chat's own host".into());
        }
        self.mutate(|doc| doc.activate_standing_role_chat(role, scope_item_id, chat_id, fallback_chain))
    }

    /// Unadopted first-start chats of this role, oldest first.
    pub fn provisional_role_chats(
        &self,
        role: mast_proto::StandingRoleKind,
        scope_item_id: &str,
    ) -> Vec<Chat> {
        let mut chats: Vec<Chat> = self
            .read_chats()
            .unwrap_or_default()
            .into_iter()
            .filter(|chat| !chat.archived && chat.is_provisional_for(role, scope_item_id))
            .collect();
        chats.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        chats
    }

    /// Create the hidden first-start chat of a standing role. It carries the
    /// provisional marker from the moment it exists, so no surface that hides
    /// superseded role chats ever lists it.
    pub(crate) fn create_provisional_role_chat(
        &self,
        chat_id: &str,
        space_id: &str,
        config: Option<ChatConfig>,
        cwd: String,
        tag: String,
    ) -> Result<(), EngineError> {
        self.create_chat_with_origin(
            chat_id,
            Some(space_id),
            None,
            config,
            Some(cwd),
            Some(ChatOrigin::Provisional(tag)),
        )
    }

    /// Whether `chat_id` holds, or was superseded in, a standing role. Hand-offs
    /// of such chats need a host that moves roles.
    pub fn chat_in_standing_role(&self, chat_id: &str) -> bool {
        self.read(|doc| {
            doc.standing_role_of_chat(chat_id).is_some()
                || doc.chat(chat_id).ok().flatten().is_some_and(|c| c.is_standing_role_predecessor())
        })
    }

    /// Persist the source's chosen successor before a hand-off launch writes
    /// anything. See `RegistryDoc::reserve_standing_role_hand_off`.
    pub fn reserve_standing_role_hand_off(&self, from: &str, to: &str, operation_id: &str, to_space: Option<&str>) -> Result<(), String> {
        self.mutate(|doc| doc.reserve_standing_role_hand_off(from, to, operation_id, to_space))
    }

    /// The part-done hand-off from the role chat `from`, if workers already moved.
    pub fn part_done_hand_off_from(&self, from: &str) -> Option<mast_proto::PendingStandingHandOff> {
        self.read(|doc| doc.part_done_hand_off_from(from))
    }

    pub fn pending_standing_hand_off(&self, role: mast_proto::StandingRoleKind, scope_item_id: &str) -> Option<mast_proto::PendingStandingHandOff> {
        self.read(|doc| doc.pending_standing_hand_off(role, scope_item_id))
    }

    /// Read-only precheck of a hand-off launch against the standing role
    /// `from` holds. `to_space` stands in for a successor not yet created.
    pub fn check_standing_role_hand_off(&self, from: &str, to: &str, to_space: Option<&str>) -> Result<(), String> {
        self.read(|doc| doc.check_standing_role_hand_off(from, to, to_space))
    }

    /// If `from` holds a standing role, hand the role to `to` (archiving and
    /// hiding `from`). `Ok(None)` when `from` holds no role, so ordinary
    /// hand-offs are untouched. A replay returns the same role; a source
    /// already replaced by a different chat is an error.
    pub fn rotate_standing_role_for_chat(&self, from: &str, to: &str) -> Result<Option<mast_proto::StandingRole>, String> {
        self.mutate(|doc| doc.rotate_standing_role_for_chat(from, to))
    }

    pub(crate) fn hand_off_recorded(&self, from: &str, to: &str) -> bool {
        self.read(|doc| doc.hand_off_recorded(from, to))
    }

    pub(crate) fn completed_user_hand_off_to(
        &self,
        from: &str,
        requested: &str,
        device: &str,
        space: &str,
        archive: Option<bool>,
    ) -> Option<String> {
        self.read(|doc| {
            let successor = doc.completed_user_hand_off_to(from, requested)?;
            let chat = doc.chat(&successor).ok().flatten()?;
            let row = doc.row(mast_doc::KIND_CHATS, &successor)?;
            (row.fields
                .get("userHandOffArchive")
                .and_then(serde_json::Value::as_bool)
                == archive
                && chat.device_id == device
                && chat.space_id.as_deref() == Some(space)
                && chat.scheduled_task_info.is_none())
            .then_some(successor)
        })
    }

    pub(crate) fn completed_hand_off_retry_operation(
        &self,
        from: &str,
        requested: &str,
    ) -> Option<String> {
        self.read(|doc| {
            let successor = doc.completed_user_hand_off_to(from, requested)?;
            if successor == requested {
                return None;
            }
            let row = doc.row(mast_doc::KIND_CHATS, &successor)?;
            row.fields
                .get("userHandOffOperationId")?
                .as_str()
                .map(str::to_owned)
        })
    }

    pub(crate) fn complete_user_chat_hand_off(
        &self,
        from: &str,
        to: &str,
        requested: &str,
        operation_id: &str,
        archive: Option<bool>,
    ) -> Result<(), String> {
        self.mutate(|doc| {
            doc.complete_user_chat_hand_off(from, to, requested, operation_id, archive)
        })
    }

    /// Every durable PR linked to this chat. Independent link rows are
    /// authoritative; the legacy single-value slot is folded in as a fallback
    /// so pre-migration chats immediately read as a one-element collection.
    pub fn chat_pr_links(&self, chat_id: &str) -> Result<Vec<ChatPrLink>, EngineError> {
        Ok(self.read(|doc| effective_chat_pr_links(doc, chat_id))?)
    }

    /// The chat's diff-target row, `None` when it never bound one.
    pub fn chat_diff_targets(
        &self,
        chat_id: &str,
    ) -> Result<Option<mast_proto::ChatDiffTargetsRow>, EngineError> {
        Ok(self.read(|doc| doc.chat_diff_targets(chat_id)))
    }

    /// Every decodable diff-target row (diff-sync reads this each pass).
    pub fn all_chat_diff_targets(
        &self,
    ) -> Result<Vec<mast_proto::ChatDiffTargetsRow>, EngineError> {
        Ok(self.read(|doc| doc.all_chat_diff_targets()))
    }

    /// Read-modify-write of one chat's diff-target row inside a single
    /// registry mutation. `f` sees the stored row (or an empty one); the row
    /// is written, with a fresh `updatedAt`, only when `f` changed it.
    /// Returns whether a write happened (`false` too when the chat row is
    /// gone).
    pub fn update_chat_diff_targets(
        &self,
        chat_id: &str,
        f: impl FnOnce(&mut mast_proto::ChatDiffTargetsRow),
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| -> Result<bool, mast_doc::DocError> {
            if doc.chat(chat_id)?.is_none() {
                return Ok(false);
            }
            let before =
                doc.chat_diff_targets(chat_id)
                    .unwrap_or_else(|| mast_proto::ChatDiffTargetsRow {
                        chat_id: chat_id.to_owned(),
                        ..Default::default()
                    });
            let mut after = before.clone();
            f(&mut after);
            after.chat_id = chat_id.to_owned();
            if after == before {
                return Ok(false);
            }
            after.updated_at = now_ms();
            doc.put_chat_diff_targets(&after)
        })?)
    }

    pub fn diff_review_state(&self, target: &str) -> Result<Option<mast_proto::DiffReviewState>, EngineError> {
        Ok(self.read(|doc| doc.diff_review_state(target)))
    }

    /// Initialize PR marks from a branch once, under the workspace lock.
    pub fn inherit_diff_review_state(&self, target: &str, branch: &str) -> Result<Option<mast_proto::DiffReviewState>, EngineError> {
        Ok(self.mutate(|doc| {
            if let Some(state) = doc.diff_review_state(target) {
                return Ok(Some(state));
            }
            let Some(mut state) = doc.diff_review_state(branch) else {
                return Ok(None);
            };
            state.target = target.to_owned();
            state.viewed_files.clear();
            doc.put_diff_review_state(&state)?;
            Ok::<_, mast_doc::DocError>(Some(state))
        })?)
    }

    pub fn put_diff_review_state(&self, state: &mast_proto::DiffReviewState) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.put_diff_review_state(state))?)
    }

    pub fn update_diff_review_state(&self, update: &mast_proto::DiffReviewUpdate) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.update_diff_review_state(&update.before, &update.after))?)
    }

    pub fn delete_diff_review_state(&self, target: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| Ok::<_, mast_doc::DocError>(doc.delete_diff_review_state(target)))?)
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, EngineError> {
        Ok(self.read(|doc| doc.read_devices())?)
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, EngineError> {
        Ok(self.read(|doc| doc.read_sessions())?)
    }

    pub fn change_sidebar_pin(
        &self,
        change: &mast_proto::SidebarPinChange,
    ) -> Result<(), EngineError> {
        let synced = self.sync_status().is_some_and(|status| status.synced);
        self.mutate(|doc| {
            if self.edge_expected() && !synced && doc.sidebar_preferences().is_none() {
                return Err(EngineError::Other("Pins are still syncing".into()));
            }
            Ok(doc.change_sidebar_pin(change)?)
        })?;
        if matches!(
            change,
            mast_proto::SidebarPinChange::Section {
                change: mast_proto::SidebarSectionChange::Import { .. }
            }
        ) {
            // The UI removes its legacy copy only after this acknowledgement.
            self.inner.persist_snapshot()?;
        }
        Ok(())
    }

    // ── Work Outline (registry rows; policy in `crate::work_outline`) ──────

    /// The registry change counter: bumped on every local mutation and every
    /// applied server frame. Composed watches (`WatchWorkOutline`) recompute
    /// from it.
    pub fn watch_changes(&self) -> watch::Receiver<u64> {
        self.inner.changed_tx.subscribe()
    }

    /// Clone the Work Outline inputs of one Space under the registry lock;
    /// the caller materializes after the lock is released (the lock also
    /// guards sync-frame application).
    pub fn work_outline_inputs(&self, space_id: &str) -> mast_doc::WorkOutlineInputs {
        let synced = self.registry_synced();
        self.read(|doc| doc.work_outline_inputs(space_id, synced))
    }

    /// Cheap change marker of the Work Outline inputs of one Space:
    /// `(generation of the outline row kinds, fingerprint of the Chat and
    /// session fields the outline reads, registry synced)`. Equal markers ⇒
    /// equal inputs; chat/session writes the outline cannot see (heartbeats,
    /// transcript appends, titles) leave it unchanged.
    pub fn work_outline_inputs_marker(&self, space_id: &str) -> (u64, u64, bool) {
        let synced = self.registry_synced();
        self.read(|doc| Self::outline_marker(doc, space_id, synced))
    }

    fn outline_marker(doc: &mast_doc::RegistryDoc, space_id: &str, synced: bool) -> (u64, u64, bool) {
        (
            doc.work_outline_structure_generation(),
            doc.work_outline_chat_session_fingerprint(space_id),
            synced,
        )
    }

    /// [`Self::work_outline_inputs`] with the marker read under the same
    /// registry lock (so the marker never claims a newer state than the
    /// inputs hold).
    pub fn work_outline_inputs_marked(
        &self,
        space_id: &str,
    ) -> ((u64, u64, bool), mast_doc::WorkOutlineInputs) {
        let synced = self.registry_synced();
        self.read(|doc| {
            (
                Self::outline_marker(doc, space_id, synced),
                doc.work_outline_inputs(space_id, synced),
            )
        })
    }

    /// Apply one Work Outline mutation as one registry write. On success the
    /// `ledger_keys` are queued to be claimed in the device-local
    /// processed-command ledger by the NEXT registry snapshot save, in the
    /// same SQLite transaction as the snapshot that contains this write
    /// (queued under the registry lock, so a snapshot holds the write iff it
    /// carries the marks). A crash before that save loses both the write and
    /// the marks, so a replay re-applies a write that never became durable;
    /// a crash after it keeps both, so a replay is refused.
    pub fn work_outline_mutate(
        &self,
        mutation: &mast_proto::WorkOutlineMutation,
        actor: &mast_proto::WorkOutlineActor,
        ledger_keys: &[String],
    ) -> Result<mast_proto::WorkOutlineMutateReply, mast_proto::WorkOutlineError> {
        self.work_outline_mutate_admin(mutation, actor, None, None, ledger_keys)
    }

    /// Apply a prepared pack under one registry lock after validating it on a
    /// private copy. If a live write unexpectedly fails, compensate in reverse
    /// order while still holding the lock and report any compensation failures.
    /// Callers authorize the actor: the user, or an agent holding an admin grant.
    pub(crate) fn apply_workflow_pack_batch(
        &self,
        space_id: &str,
        pack: &mast_proto::workflow_pack::WorkflowPack,
        scopes: &[mast_proto::workflow_pack::WorkflowPackScopePreview],
        templates: &[mast_proto::workflow_pack::WorkflowPackTemplatePreview],
        operations: &[crate::workflow_pack::WorkflowPackMutationPair],
        actor: &mast_proto::WorkOutlineActor,
        admin_space: Option<&str>,
    ) -> Result<Vec<crate::workflow_pack::WorkflowPackMutationPair>, String> {
        if crate::work_outline::writes_disabled() {
            return Err("Work Outline changes are turned off on this device".into());
        }
        let synced = self.registry_synced();
        let now = Utc::now().timestamp_millis();
        self.mutate(|doc| {
            let snapshot = doc.work_outline_snapshot(space_id, synced);
            crate::workflow_pack::revalidate_import_preview(pack, &snapshot, scopes, templates)?;
            let bytes = doc.to_bytes().map_err(|error| error.to_string())?;
            let mut staged = RegistryDoc::from_bytes(&bytes, doc.device_id()).map_err(|error| error.to_string())?;
            for pair in operations {
                staged.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&pair.forward, actor, now)).map_err(|error| error.to_wire_message())?;
            }
            // Do not apply a pack whose inverses cannot be guarded and run.
            for pair in operations.iter().rev() {
                let snapshot = staged.work_outline_snapshot(space_id, synced);
                crate::workflow_pack::guard_pair_undo(&snapshot, pair)?;
                staged.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&pair.inverse, actor, now)).map_err(|error| error.to_wire_message())?;
            }
            let mut applied: Vec<crate::workflow_pack::WorkflowPackMutationPair> = Vec::new();
            for pair in operations {
                if let Err(error) = doc.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&pair.forward, actor, now)) {
                    let mut failures = Vec::new();
                    for completed in applied.iter().rev() {
                        if let Err(undo_error) = doc.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&completed.inverse, actor, now)) {
                            failures.push(undo_error.to_wire_message());
                        }
                    }
                    return Err(format!("{}; compensation failures: {}", error.to_wire_message(), failures.join(", ")));
                }
                applied.push(pair.clone());
            }
            Ok(applied)
        })
    }

    /// Validate inverses against a staged snapshot, then undo under one lock.
    pub(crate) fn undo_workflow_pack_batch(
        &self,
        space_id: &str,
        receipt: &[crate::workflow_pack::WorkflowPackMutationPair],
        actor: &mast_proto::WorkOutlineActor,
        admin_space: Option<&str>,
    ) -> Result<(), String> {
        if crate::work_outline::writes_disabled() {
            return Err("Work Outline changes are turned off on this device".into());
        }
        let synced = self.registry_synced();
        let now = Utc::now().timestamp_millis();
        self.mutate(|doc| {
            let bytes = doc.to_bytes().map_err(|error| error.to_string())?;
            let mut staged = RegistryDoc::from_bytes(&bytes, doc.device_id()).map_err(|error| error.to_string())?;
            for pair in receipt.iter().rev() {
                let snapshot = staged.work_outline_snapshot(space_id, synced);
                crate::workflow_pack::guard_pair_undo(&snapshot, pair)?;
                staged.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&pair.inverse, actor, now)).map_err(|error| error.to_wire_message())?;
            }
            for pair in receipt.iter().rev() {
                doc.with_admin_scope(admin_space, |scoped| scoped.apply_work_outline_mutation(&pair.inverse, actor, now)).map_err(|error| error.to_wire_message())?;
            }
            Ok(())
        })
    }

    /// Run `f` on a scratch copy of the registry: nothing it does is written,
    /// synced or kept. Used to price a batch step by step before any write.
    pub fn stage<R>(&self, f: impl FnOnce(&mut RegistryDoc) -> R) -> Result<R, String> {
        let (bytes, device) = self.read(|doc| (doc.to_bytes(), doc.device_id().to_owned()));
        let bytes = bytes.map_err(|error| error.to_string())?;
        let mut staged = RegistryDoc::from_bytes(&bytes, &device).map_err(|error| error.to_string())?;
        Ok(f(&mut staged))
    }

    /// [`Self::work_outline_mutate`] for an agent holding a live admin grant
    /// on `admin_space` (the engine re-checks the grant per call).
    pub fn work_outline_mutate_admin(
        &self,
        mutation: &mast_proto::WorkOutlineMutation,
        actor: &mast_proto::WorkOutlineActor,
        admin_space: Option<&str>,
        guard: Option<&crate::work_outline::ManagerGuard>,
        ledger_keys: &[String],
    ) -> Result<mast_proto::WorkOutlineMutateReply, mast_proto::WorkOutlineError> {
        let now = Utc::now().timestamp_millis();
        let synced = self.registry_synced();
        self.mutate(|doc| {
            let charge = match (guard, admin_space) {
                (Some(guard), Some(space)) => guard.verify(doc, space, synced)?,
                _ => 0,
            };
            let request_item = guard.and_then(|g| g.request_item.as_deref());
            let applied = doc.with_manager_request_item(request_item, |doc| {
                doc.with_admin_scope(admin_space, |doc| {
                    doc.apply_work_outline_mutation(mutation, actor, now)
                })
            });
            // The charge was reserved by the guard before the write; a write
            // that did not commit gives it back.
            let reply = match applied {
                Ok(reply) => reply,
                Err(error) => {
                    if let Some(guard) = guard {
                        guard.refund(charge);
                    }
                    return Err(error.into());
                }
            };
            lock(&self.inner.pending_ledger_marks).extend(ledger_keys.iter().cloned());
            Ok(reply)
        })
    }

    /// `keepInstructionSession` with this device's own delivery record
    /// (`kept`, see `SessionsEngine::kept_instruction_session`; `None` = use
    /// the reported Execution Session in the registry). Ledger keys as
    /// [`Self::work_outline_mutate`].
    pub fn work_outline_keep_session(
        &self,
        params: &mast_proto::KeepInstructionSessionParams,
        kept: Option<Option<mast_proto::InstructionRevisionPin>>,
        actor: &mast_proto::WorkOutlineActor,
        ledger_keys: &[String],
    ) -> Result<mast_proto::WorkOutlineMutateReply, mast_proto::WorkOutlineError> {
        let now = Utc::now().timestamp_millis();
        self.mutate(|doc| {
            let id = doc.keep_instruction_session(params, kept, actor, now)?;
            lock(&self.inner.pending_ledger_marks).extend(ledger_keys.iter().cloned());
            Ok(mast_proto::WorkOutlineMutateReply {
                application_id: Some(id),
                ..Default::default()
            })
        })
    }

    /// The live work item `chat_id` is assigned to in `space_id` (the lookup
    /// the agent policy uses; one row read, no materialization).
    pub fn chat_assigned_work_item(&self, chat_id: &str, space_id: &str) -> Option<String> {
        self.read(|doc| doc.chat_assigned_work_item(chat_id, space_id))
    }

    /// The effective (repaired) parent of a work item in `space_id`.
    pub fn effective_work_item_parent(&self, space_id: &str, work_item_id: &str) -> Option<String> {
        self.read(|doc| doc.effective_work_item_parent(space_id, work_item_id))
    }

    /// The Milestone 4 agent policy over the local replica (see
    /// `RegistryDoc::authorize_agent_mutation`).
    pub fn authorize_agent_work_outline(
        &self,
        chat_id: &str,
        mutation: &mast_proto::WorkOutlineMutation,
    ) -> Result<(), mast_proto::WorkOutlineError> {
        self.read(|doc| doc.authorize_agent_mutation(chat_id, mutation))
    }

    /// Whether the durable processed-command ledger holds `key` (a reserved
    /// `work-outline:` key; see `crate::work_outline`). Read errors count as
    /// "not processed": the doc-layer mutators are replay-safe on their own.
    pub fn ledger_holds(&self, key: &str) -> bool {
        self.inner.store.is_processed(key).unwrap_or(false)
    }

    /// Claim `key` in the durable processed-command ledger now (its own
    /// SQLite write, not deferred to the next snapshot). `true` when this
    /// call inserted it. Used by the Work Outline launch workflow, whose
    /// steps span the registry and session docs.
    pub fn ledger_mark(&self, key: &str) -> Result<bool, EngineError> {
        Ok(self.inner.store.mark_processed(key)?)
    }

    /// Whether a registry row is absent, live, or tombstoned (rule R1).
    pub fn registry_row_state(&self, kind: &str, id: &str) -> mast_doc::RowState {
        self.read(|doc| doc.row_state(kind, id))
    }

    /// One overlay registry row with its clocks (diagnostics and tests).
    pub fn registry_row(&self, kind: &str, id: &str) -> Option<mast_doc::RegistryRow> {
        self.read(|doc| doc.row(kind, id))
    }

    /// Unacked registry batches (diagnostics and tests).
    pub fn registry_pending_len(&self) -> usize {
        self.read(|doc| doc.pending_len())
    }

    /// Registry batches written since this host opened, settled or queued
    /// ("did that call write anything"; diagnostics and tests).
    pub fn registry_batches_written(&self) -> u64 {
        self.read(|doc| doc.batches_written())
    }

    pub fn set_friction_status(&self, space_id: &str, fingerprint: &str, status: mast_proto::FrictionStatus, item_id: Option<String>, promoted_at: Option<i64>) -> Result<mast_proto::FrictionReport, EngineError> {
        Ok(self.mutate(|doc| doc.set_friction_status(space_id, fingerprint, status, item_id, promoted_at))?)
    }

    pub fn dismiss_friction(&self, space_id: &str, fingerprint: &str, reason: &str) -> Result<mast_proto::FrictionReport, EngineError> {
        Ok(self.mutate(|doc| -> Result<_, mast_doc::DocError> {
            let report = doc.set_friction_status(space_id, fingerprint, mast_proto::FrictionStatus::Dismissed, None, None)?;
            let hlc = doc.mint_hlc();
            doc.write_ops(vec![mast_doc::registry::RowOp {
                kind: mast_doc::registry::friction::KIND_FRICTION_REPORTS.into(),
                id: report.id.clone(), op: mast_doc::registry::OpKind::Update,
                set: Some(BTreeMap::from([("dismissalReason".into(), serde_json::json!(reason))])), hlc, clocks: None,
            }])?;
            Ok(report)
        })?)
    }

    pub fn record_friction(&self, report: mast_proto::FrictionReport) -> Result<mast_proto::FrictionReport, EngineError> {
        Ok(self.mutate(|doc| doc.record_friction(report))?)
    }

    /// Persist an audit record before an irreversible response is sent.
    pub fn record_friction_durable(&self, report: mast_proto::FrictionReport) -> Result<mast_proto::FrictionReport, EngineError> {
        let row = self.record_friction(report)?;
        self.inner.persist_snapshot()?;
        Ok(row)
    }

    pub fn import_friction(&self, report: mast_proto::FrictionReport, import_id: &str) -> Result<(mast_proto::FrictionReport, bool), EngineError> {
        Ok(self.mutate(|doc| doc.import_friction(report, import_id))?)
    }

    pub fn answer_friction_approval(&self, id: &str, answer: bool) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.answer_friction_approval(id, answer))?)
    }

    pub fn friction_reports(&self, space_id: &str) -> Vec<mast_proto::FrictionReport> {
        self.read(|doc| doc.friction_reports(space_id))
    }

    // ── Planning artifacts (registry rows; policy in `crate::artifacts`) ──

    /// A live artifact row.
    pub fn artifact(&self, id: &str) -> Option<mast_proto::ArtifactView> {
        self.read(|doc| doc.artifact(id))
    }

    /// Every live artifact row (all Spaces).
    pub fn all_artifacts(&self) -> Vec<mast_proto::ArtifactView> {
        self.read(|doc| doc.all_artifacts())
    }

    /// Live artifacts of one Space, sorted.
    pub fn artifacts_of_space(&self, space_id: &str) -> Vec<mast_proto::ArtifactView> {
        self.read(|doc| doc.artifacts_of_space(space_id))
    }

    /// Change marker of the artifact rows.
    pub fn artifacts_generation(&self) -> u64 {
        self.read(|doc| doc.artifacts_generation())
    }

    /// Change marker of the work outline kinds (work items, Required Actions,
    /// planning artifacts): unchanged ⇒ nothing plan-question sync reads changed.
    pub fn work_outline_kinds_generation(&self) -> u64 {
        self.read(|doc| doc.work_outline_kinds_generation())
    }

    /// Create or update one artifact row (returns the row as written).
    pub fn put_artifact(
        &self,
        view: &mast_proto::ArtifactView,
    ) -> Result<mast_proto::ArtifactView, EngineError> {
        Ok(self.mutate(|doc| doc.put_artifact(view))?)
    }

    /// Pin or unpin one artifact as its scope's plan (see
    /// `RegistryDoc::set_artifact_pin`).
    pub fn set_artifact_pin(
        &self,
        id: &str,
        pinned: bool,
    ) -> Result<Option<Vec<String>>, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_pin(id, pinned, now))?)
    }

    /// Replace an artifact's recorded question answers (see
    /// `RegistryDoc::set_artifact_answers`).
    pub fn set_artifact_answers(
        &self,
        id: &str,
        answers: &[mast_proto::ArtifactQuestionAnswer],
    ) -> Result<Option<Vec<mast_proto::ArtifactQuestionAnswer>>, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_answers(id, answers, now))?)
    }

    /// Atomically replace plan answers without truncating the batch.
    pub fn set_artifact_answers_exact(&self, id: &str, answers: &[mast_proto::ArtifactQuestionAnswer]) -> Result<bool, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_answers_exact(id, answers, now))?)
    }

    /// Atomically replace the decisions recorded on a decision sheet.
    pub fn set_artifact_decisions(
        &self,
        id: &str,
        decisions: &[mast_proto::decision_sheet::SheetDecision],
    ) -> Result<bool, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_decisions(id, decisions, now))?)
    }

    /// Replace an artifact's comments (see `RegistryDoc::set_artifact_comments`).
    pub fn set_artifact_comments(
        &self,
        id: &str,
        comments: &[mast_proto::ArtifactComment],
    ) -> Result<Option<Vec<mast_proto::ArtifactComment>>, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_comments(id, comments, now))?)
    }

    /// Record or clear an artifact's picked option (see
    /// `RegistryDoc::set_artifact_pick`).
    pub fn set_artifact_pick(
        &self,
        id: &str,
        pick: Option<&mast_proto::ArtifactOptionPick>,
    ) -> Result<Option<()>, EngineError> {
        let now = Utc::now().timestamp_millis();
        Ok(self.mutate(|doc| doc.set_artifact_pick(id, pick, now))?)
    }

    /// Whether a Space has the Linear integration turned on.
    pub fn space_linear_enabled(&self, space_id: &str) -> bool {
        self.read(|doc| doc.space_linear_enabled(space_id))
    }

    /// Live Spaces with Linear turned on.
    pub fn linear_enabled_space_ids(&self) -> Vec<String> {
        self.read(|doc| doc.linear_enabled_space_ids())
    }

    /// Live Spaces with a linked Linear item (sorted).
    pub fn linear_assigned_space_ids(&self) -> Vec<String> {
        self.read(|doc| doc.linear_assigned_space_ids())
    }

    /// Live Spaces with a PR review sync destination (sorted).
    pub fn pr_review_sync_space_ids(&self) -> Vec<String> {
        self.read(|doc| doc.pr_review_sync_space_ids())
    }

    /// The Space of a live work item (overlay view).
    pub fn work_item_space(&self, work_item_id: &str) -> Option<String> {
        self.read(|doc| doc.work_item_space(work_item_id))
    }

    /// A live instruction revision with its integrity verified.
    pub fn instruction_revision(
        &self,
        revision_id: &str,
    ) -> Option<mast_proto::InstructionRevisionView> {
        self.read(|doc| doc.instruction_revision(revision_id))
    }

    /// The Chat's selected instruction revision (latest accepted application
    /// by `(appliedHlc, id)` whose revision is intact).
    pub fn selected_instruction_revision(
        &self,
        chat_id: &str,
    ) -> Option<mast_proto::InstructionRevisionPin> {
        self.read(|doc| doc.selected_instruction_revision(chat_id))
    }

    /// Whole-row resolution of an execution scope from the local replica
    /// (Milestone 3; pure, no filesystem).
    pub fn resolve_execution(
        &self,
        scope: &mast_proto::ExecutionScope,
    ) -> Result<mast_proto::ResolvedExecutionConfiguration, mast_proto::WorkOutlineError> {
        self.read(|doc| doc.resolve_execution(scope))
    }

    /// The Chat's captured execution binding, if any (Milestone 3).
    pub fn chat_execution_binding(
        &self,
        chat_id: &str,
    ) -> Option<mast_proto::ChatExecutionBindingView> {
        self.read(|doc| doc.chat_execution_binding(chat_id))
    }

    /// The Chat's binding row as a gate reads it: a row that exists but
    /// does not decode is bound-and-invalid, never unbound.
    pub fn chat_execution_binding_state(
        &self,
        chat_id: &str,
    ) -> mast_proto::ChatExecutionBindingState {
        self.read(|doc| doc.chat_execution_binding_state(chat_id))
    }

    /// Gate read of a Chat's binding: `Ok(None)` unbound, `Ok(Some)` bound,
    /// and an `execution-scope:configInvalid` rejection for a malformed row.
    pub fn checked_chat_execution_binding(
        &self,
        chat_id: &str,
    ) -> Result<Option<mast_proto::ChatExecutionBindingView>, EngineError> {
        self.chat_execution_binding_state(chat_id)
            .checked()
            .map_err(|error| {
                EngineError::Other(crate::execution_scope::rejection_message(&[error]))
            })
    }

    /// Every captured execution binding, by Chat id (Milestone 3).
    pub fn chat_execution_bindings(
        &self,
    ) -> std::collections::HashMap<String, mast_proto::ChatExecutionBindingView> {
        self.read(|doc| doc.chat_execution_bindings())
            .into_iter()
            .map(|binding| (binding.chat_id.clone(), binding))
            .collect()
    }

    /// Capture the configuration a Chat launches with (create-if-absent,
    /// keyed by Chat id; see `RegistryDoc::capture_chat_execution_binding`).
    /// An identical replay writes nothing and does not bump the change
    /// counter. Returns whether a write was enqueued.
    pub fn capture_chat_execution_binding(
        &self,
        chat_id: &str,
        resolved: &mast_proto::ResolvedExecutionConfiguration,
        operation_id: &str,
        actor: &mast_proto::WorkOutlineActor,
    ) -> Result<bool, mast_proto::WorkOutlineError> {
        let now = Utc::now().timestamp_millis();
        // Dry run under the read lock first: `mutate` always bumps the
        // change counter, which a replay must not do.
        let replay = self.read(|doc| {
            doc.chat_execution_binding(chat_id).is_some_and(|binding| {
                binding.operation_id.as_deref() == Some(operation_id)
                    && binding.resolved == *resolved
            })
        });
        if replay {
            return Ok(false);
        }
        self.mutate(|doc| {
            doc.capture_chat_execution_binding(chat_id, resolved, operation_id, actor, now)
        })
    }

    /// Record what an Execution Session received on this host's session-status
    /// row (`executionSession`, contract 5.6). Idempotent: an equal report
    /// writes nothing (and does not bump the change counter). Returns whether
    /// a write was enqueued.
    pub fn report_execution_session(
        &self,
        chat_id: &str,
        report: &mast_proto::ExecutionSessionReport,
    ) -> bool {
        let unchanged = self.read(|doc| {
            doc.row(mast_doc::KIND_SESSIONS, chat_id)
                .is_some_and(|row| {
                    !row.deleted
                        && row.fields.get("executionSession")
                            == serde_json::to_value(report).ok().as_ref()
                })
        });
        if unchanged {
            return false;
        }
        let now = Utc::now().timestamp_millis();
        match self.mutate(|doc| doc.report_execution_session(chat_id, report, now)) {
            Ok(written) => written,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "execution session report failed");
                false
            }
        }
    }

    /// Tombstone outline rows whose Space is gone. Destructive on a stale
    /// view: callers gate on [`Self::registry_synced`].
    pub fn sweep_work_outline_orphans(&self) -> usize {
        // Read first: `mutate` always bumps the change counter (registry
        // publish, snapshot write, watch recompute), so the 120 s sweep must
        // not take that path when there is nothing to delete.
        if self.read(|doc| doc.work_outline_orphans().is_empty()) {
            return 0;
        }
        self.mutate(|doc| doc.sweep_work_outline_orphans())
    }

    /// Re-home a chat whose Space is gone when its cwd is a worktree whose
    /// home Space exists: a chat created in a worktree Space while it was
    /// being folded belongs where the fold sent the rest. Never creates a
    /// Space, so a Space the user deleted stays deleted. `false` (nothing
    /// written) for any other chat.
    pub fn rehome_worktree_chat(&self, chat: &Chat) -> bool {
        let Some(cwd) = chat.cwd.as_deref() else {
            return false;
        };
        if worktree_home_origin(std::path::Path::new(cwd)).is_none() {
            return false;
        }
        let result = self
            .read(|doc| doc.read_spaces())
            .map_err(EngineError::from)
            .and_then(
                |spaces| match space_home(&spaces, &self.inner.config.device_id, cwd) {
                    SpaceHome::Existing(space) => {
                        Ok(self.mutate(|doc| doc.set_chat_space(&chat.id, &space))?)
                    }
                    SpaceHome::New(_) => Ok(false),
                },
            );
        match result {
            Ok(written) => written,
            Err(err) => {
                tracing::warn!(chat = %chat.id, error = %err, "worktree chat re-home failed");
                false
            }
        }
    }

    /// Fold this device's Spaces that sit on a linked worktree into the Space
    /// of the checkout the worktree came from ([`space_home`], created when
    /// none exists): their chats move over and the emptied Space is deleted.
    /// Spaces that aren't [foldable](RegistryDoc::space_foldable), or are in
    /// `held` (a scheduled task still launches into them), are left alone.
    /// Destructive on a stale view (a peer's chats or outline rows may not
    /// have arrived yet): callers gate on [`Self::registry_synced`].
    /// Returns how many Spaces were folded.
    pub fn fold_worktree_spaces(&self, held: &std::collections::HashSet<String>) -> usize {
        let device_id = &self.inner.config.device_id;
        let spaces = match self.read(|doc| doc.read_spaces()) {
            Ok(spaces) => spaces,
            Err(err) => {
                tracing::warn!(error = %err, "worktree space fold: space read failed");
                return 0;
            }
        };
        let mut folded = 0;
        for space in spaces
            .iter()
            .filter(|s| s.device_id == *device_id && !held.contains(&s.id))
        {
            if worktree_home_origin(std::path::Path::new(&space.path)).is_none()
                || !self
                    .read(|doc| doc.space_foldable(&space.id))
                    .unwrap_or(false)
            {
                continue;
            }
            let into = match self.space_for_path(&space.path) {
                Ok(into) => into,
                Err(err) => {
                    tracing::warn!(space = %space.id, error = %err, "worktree space fold: no home space");
                    continue;
                }
            };
            match self.mutate(|doc| doc.fold_space(&space.id, &into)) {
                Ok(Some(moved)) => {
                    tracing::info!(
                        space = %space.id, path = %space.path, into = %into, chats = moved.len(),
                        "folded worktree space into its checkout's space"
                    );
                    folded += 1;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(space = %space.id, error = %err, "worktree space fold failed");
                }
            }
        }
        folded
    }

    // ── watches (WatchChats / WatchDevices / merged WatchSessions) ──────────

    pub fn watch_chats(&self) -> watch::Receiver<Vec<Chat>> {
        self.inner.chats_tx.subscribe()
    }

    pub fn watch_devices(&self) -> watch::Receiver<Vec<Device>> {
        self.inner.devices_tx.subscribe()
    }

    /// Raw workspace session-status rows (all devices').
    pub fn watch_session_rows(&self) -> watch::Receiver<Vec<Session>> {
        self.inner.sessions_tx.subscribe()
    }

    pub fn watch_spaces(&self) -> watch::Receiver<Vec<Space>> {
        self.inner.spaces_tx.subscribe()
    }

    pub fn watch_sidebar_preferences(&self) -> watch::Receiver<SidebarPreferencesState> {
        self.inner.sidebar_preferences_tx.subscribe()
    }

    /// Mutation acknowledgements and watches share the same ordered revision.
    pub fn sidebar_preferences_snapshot(&self) -> SidebarPreferencesState {
        let synced = self.sync_status().is_some_and(|status| status.synced);
        let doc = lock(&self.inner.reg);
        self.inner.publish_sidebar_preferences(&doc, synced);
        self.inner.sidebar_preferences_tx.borrow().clone()
    }

    /// WatchSessions source: remote devices' rows from the registry merged with
    /// this engine's live status watch (the local view is fresher for our own runs).
    pub fn merged_sessions_watch(
        &self,
        local: watch::Receiver<Vec<Session>>,
    ) -> watch::Receiver<Vec<Session>> {
        let mut rows = self.watch_session_rows();
        let mut local = local;
        let device_id = self.inner.config.device_id.clone();
        let (tx, rx) = watch::channel(merge_sessions(&device_id, &rows.borrow(), &local.borrow()));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = rows.changed() => if changed.is_err() { break },
                    changed = local.changed() => if changed.is_err() { break },
                }
                let merged = merge_sessions(
                    &device_id,
                    &rows.borrow_and_update(),
                    &local.borrow_and_update(),
                );
                if tx.send(merged).is_err() {
                    break; // no receivers left
                }
            }
        });
        rx
    }

    // ── chat ownership ──────────────────────────────────────────────────────

    /// Writer discipline: the chat's host is its row's `deviceId`. Unknown chats
    /// are claimable — the first run command claims them via [`Self::claim_chat`].
    pub fn is_host(&self, chat_id: &str) -> bool {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(Some(chat)) => chat.device_id == self.inner.config.device_id,
            Ok(None) => true,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                true
            }
        }
    }

    /// Claim-on-first-command: create the chat row under OUR device id when a run
    /// command arrives for a chat with no row yet. No-op when the row exists.
    ///
    /// The claim is a PARTIAL row write (identity/cwd/space only): the command
    /// plane is nudged and outruns the registry channel, so the client's
    /// `createChat` for the same chat routinely arrives AFTER the claim with
    /// older clocks — fields the claim never wrote (`config`, `title`) must
    /// still land then.
    ///
    /// Resolve project cwd claims to a space. The portable home marker `~`
    /// is the composer's projectless target; preserve it without minting a
    /// project when the run outruns createChat on the registry channel.
    ///
    /// A Chat with a captured execution binding is never claimed: its row is
    /// written by the launch workflow before any command, so a bound Chat
    /// without a live row was deleted, and claiming would revive it under a
    /// possibly freshly minted Space (fallback F7).
    pub fn claim_chat(&self, chat_id: &str, cwd: Option<&str>) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(());
        }
        if self.checked_chat_execution_binding(chat_id)?.is_some() {
            return Err(EngineError::Other(format!(
                "Chat {chat_id} has an execution binding but no live chat row; it will not be \
                 recreated"
            )));
        }
        let space_id = match cwd {
            Some("~" | "~/") => None,
            Some(cwd) => Some(self.space_for_path(cwd)?),
            None => None,
        };
        self.mutate(|doc| doc.claim_chat(chat_id, cwd, space_id.as_deref(), Utc::now()));
        Ok(())
    }

    /// The own-device Space [`space_home`] picks, created when none fits.
    fn space_for_path(&self, path: &str) -> Result<String, EngineError> {
        let device_id = &self.inner.config.device_id;
        let spaces = self.read(|doc| doc.read_spaces())?;
        let path = match space_home(&spaces, device_id, path) {
            SpaceHome::Existing(id) => return Ok(id),
            SpaceHome::New(path) => path,
        };
        let space = Space {
            id: crate::new_id(),
            device_id: device_id.clone(),
            path,
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        self.mutate(|doc| {
            doc.create_space_with_pr_prompts(&space, &mast_proto::PrReviewPrompts::neutral())
        })?;
        Ok(space.id)
    }

    /// The chat's configured harness/model row, when present (RunRequest harness
    /// selection; callers fall back to the engine default).
    pub fn chat_config(&self, chat_id: &str) -> Option<ChatConfig> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => chat.and_then(|c| c.config),
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    // ── host-side row writes ────────────────────────────────────────────────

    /// Give a user-created chat durable work ownership. Launches with an
    /// explicit item keep their assignment; children and side chats share
    /// their source's item. The registry lock prevents duplicate defaults
    /// when creation and first-command recovery race.
    pub fn ensure_chat_work_item(&self, chat_id: &str) -> Result<String, EngineError> {
        use mast_proto::{
            AssignChatToWorkItemParams, CreateWorkItemParams, WorkOutlineActor, WorkOutlineActorKind,
        };
        if crate::work_outline::writes_disabled() {
            return Err(EngineError::Other(
                "Work Outline changes are turned off on this device".into(),
            ));
        }
        let personal_root = self.inner.store.root().join("projectless");
        self.mutate(|doc| -> Result<String, EngineError> {
            let chat = doc
                .chat(chat_id)?
                .ok_or_else(|| EngineError::Other("Chat not found".into()))?;
            // A role's predecessor is archived by its own rotation; a retry of
            // that hand-off only needs the item it already has.
            if chat.archived && !chat.is_standing_role_predecessor() {
                return Err(EngineError::Other(
                    "An archived chat cannot start new work".into(),
                ));
            }
            let space_id = match chat.space_id.as_deref() {
                Some(id) => id.to_owned(),
                None => {
                    if chat.device_id != self.device_id() {
                        return Err(EngineError::Other(
                            "The chat's host must assign its personal Space".into(),
                        ));
                    }
                    std::fs::create_dir_all(&personal_root)?;
                    let id = Space::personal_id(&chat.device_id);
                    if doc.space(&id)?.is_none() {
                        doc.create_space_with_pr_prompts(
                            &Space {
                                id: id.clone(),
                                device_id: chat.device_id.clone(),
                                path: personal_root.to_string_lossy().into_owned(),
                                name: Some("Personal".into()),
                                git_detected: false,
                                git_checked_at: None,
                                checkout_id: None,
                                created_at: Utc::now(),
                            },
                            &mast_proto::PrReviewPrompts::neutral(),
                        )?;
                    }
                    doc.set_chat_space(chat_id, &id)?;
                    id
                }
            };
            if let Some(item) = doc.chat_assigned_work_item(chat_id, &space_id) {
                return Ok(item);
            }
            if space_id == Space::personal_id(&chat.device_id) && chat.device_id != self.device_id() {
                return Err(EngineError::Other(
                    "The chat's host must initialize its personal work item".into(),
                ));
            }
            let actor = WorkOutlineActor {
                kind: WorkOutlineActorKind::System,
                chat_id: None,
                device_id: self.device_id().to_owned(),
                message_id: None,
            };
            let now = Utc::now().timestamp_millis();
            let inherited = chat
                .parent_chat_id
                .as_deref()
                .or(chat.side_of.as_deref())
                .and_then(|parent| doc.chat_assigned_work_item(parent, &space_id));
            let operation_id = format!(
                "chat-item:{}",
                mast_proto::derive_id(chat_id, "default-item")
            );
            let item = match inherited {
                Some(item) => item,
                None => {
                    let id = mast_proto::derive_id(&operation_id, mast_proto::roles::ITEM);
                    if doc.row_state(mast_doc::KIND_WORK_ITEMS, &id) != mast_doc::RowState::Live {
                        doc.create_work_item(
                            &CreateWorkItemParams {
                                space_id: space_id.clone(),
                                parent_work_item_id: None,
                                placement: Default::default(),
                                title: chat_item_title(chat.title.as_deref()),
                                description: None,
                                status: None,
                                operation_id: operation_id.clone(),
                            },
                            &actor,
                            now,
                        )
                        .map_err(|e| EngineError::Other(e.to_wire_message()))?;
                    }
                    if space_id == Space::personal_id(&chat.device_id)
                        && doc.row_state(
                            mast_doc::registry::work_outline::KIND_EXECUTION_CONFIGURATIONS,
                            &format!("w:{id}"),
                        ) == mast_doc::RowState::Absent
                    {
                        let root = personal_root.join(crate::run_journal::sanitize_id(chat_id));
                        std::fs::create_dir_all(&root)?;
                        doc.configure_execution(
                            &mast_proto::ConfigureExecutionParams {
                                scope: mast_proto::ExecutionScope::WorkItem {
                                    work_item_id: id.clone(),
                                },
                                configuration: Some(mast_proto::ExecutionConfiguration::implicit(
                                    root.to_string_lossy(),
                                )),
                                operation_id: operation_id.clone(),
                            },
                            &actor,
                            now,
                        )
                        .map_err(|e| EngineError::Other(e.to_wire_message()))?;
                    }
                    id
                }
            };
            doc.assign_chat_to_work_item(
                &AssignChatToWorkItemParams {
                    chat_id: chat_id.to_owned(),
                    work_item_id: item.clone(),
                    operation_id,
                },
                &actor,
                now,
            )
            .map_err(|e| EngineError::Other(e.to_wire_message()))?;
            Ok(item)
        })
    }

    pub(crate) fn ensure_chat_work_item_best_effort(&self, chat_id: &str) {
        if crate::work_outline::writes_disabled() {
            return;
        }
        if let Err(error) = self.ensure_chat_work_item(chat_id) {
            tracing::warn!(chat = %chat_id, %error, "automatic work item assignment deferred");
        }
    }

    /// Sidebar freshness on message persist: preview = first 120 chars of the last
    /// message's text. Claims the row first so a pre-workspace chat gains one.
    pub fn note_message(&self, chat_id: &str, text: &str) {
        let preview = mast_proto::message_source::preview(text);
        let preview: String = preview.chars().take(120).collect();
        let result = self.claim_chat(chat_id, None).and_then(|_| {
            self.mutate(|doc| doc.set_chat_last_message(chat_id, &preview, Utc::now()))
                .map_err(EngineError::from)
        });
        if let Err(err) = result {
            tracing::warn!(chat = %chat_id, error = %err, "registry last-message write failed");
        }
    }

    /// Resume continuity: stamp the chat row with the harness-native session id
    /// of its latest run and the cwd it was created under. An empty `session_id`
    /// tombstones the row ("do not resume" after a rejected resume). Best-effort:
    /// a missing chat row (claim happens on first command) just returns.
    pub fn set_chat_harness_session(&self, chat_id: &str, session_id: &str, cwd: &str) {
        match self.mutate(|doc| doc.set_chat_harness_session(chat_id, session_id, cwd)) {
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry harness-session write failed");
            }
        }
    }

    /// Stamp the model the harness reported it ran the chat on. Best-effort.
    pub fn set_chat_actual_model(&self, chat_id: &str, model: &str) {
        if let Err(err) = self.mutate(|doc| doc.set_chat_actual_model(chat_id, model)) {
            tracing::warn!(chat = %chat_id, error = %err, "registry actual-model write failed");
        }
    }

    /// The chat row's stored harness session `(session_id, cwd)`, if stamped.
    /// The empty-string tombstone passes through — callers must treat it as
    /// "explicitly no resume" (and must NOT fall back to older sources).
    pub fn chat_harness_session(&self, chat_id: &str) -> Option<(String, Option<String>)> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => {
                let chat = chat?;
                let id = chat.harness_session_id?;
                Some((id, chat.harness_session_cwd))
            }
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    /// Session-status row upsert (sessions engine transitions land here too, in
    /// addition to the local watch channel).
    pub fn record_session(&self, session: &Session) {
        if let Err(err) = self.mutate(|doc| doc.upsert_session(session)) {
            tracing::warn!(chat = %session.chat_id, error = %err, "registry session write failed");
        }
    }

    // ── Mutate surface (LWW writes accepted from any device) ────────────────

    /// Create a chat, usually *in a project*: the project fixes the host device
    /// and base cwd (`cwd` override = an isolated-worktree path). With no
    /// `space_id` the chat is project-less: `device_id` picks the host and the
    /// cwd defaults to the portable `~` marker (resolved host-side to a
    /// Mast-owned per-chat scratch directory when the run spawns).
    pub fn create_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
    ) -> Result<(), EngineError> {
        self.create_chat_with_parent(chat_id, space_id, device_id, config, cwd, None)
    }

    /// [`create_chat`](Self::create_chat) recording the creating chat
    /// (`parentChatId`) — the Mast MCP's orchestration link.
    pub fn create_chat_with_parent(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
        parent_chat_id: Option<String>,
    ) -> Result<(), EngineError> {
        self.create_chat_with_origin(
            chat_id,
            space_id,
            device_id,
            config,
            cwd,
            parent_chat_id.map(ChatOrigin::Parent),
        )
    }

    /// Publish scheduled provenance with the chat so a background run is
    /// hidden even before its work-item assignment arrives.
    pub(crate) fn create_scheduled_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        config: ChatConfig,
        cwd: String,
        task: mast_proto::ScheduledChat,
    ) -> Result<(), EngineError> {
        self.create_chat_with_origin(
            chat_id,
            space_id,
            Some(self.device_id()),
            Some(config),
            Some(cwd),
            Some(ChatOrigin::Scheduled(task)),
        )
    }

    pub(crate) fn create_archived_external_chat(
        &self,
        chat_id: &str,
        space_id: &str,
        config: ChatConfig,
        cwd: String,
        task: mast_proto::ScheduledChat,
    ) -> Result<(), EngineError> {
        self.create_chat_with_origin(
            chat_id,
            Some(space_id),
            Some(self.device_id()),
            Some(config),
            Some(cwd),
            Some(ChatOrigin::ArchivedExternal(task)),
        )
    }

    fn create_chat_with_origin(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
        origin: Option<ChatOrigin>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            // Older launchers could stop after creating the chat but before
            // recording its provenance. Complete that step on recovery.
            if let Some(ChatOrigin::Scheduled(task)) = &origin {
                self.mutate(|doc| {
                    if doc
                        .chat(chat_id)?
                        .is_some_and(|chat| chat.scheduled_task_info.is_none())
                    {
                        doc.set_chat_scheduled_task_info(chat_id, task)?;
                    }
                    Ok::<(), mast_doc::DocError>(())
                })?;
            }
            return Ok(());
        }
        let space = match space_id {
            Some(space_id) => match self.read(|doc| doc.space(space_id))? {
                Some(space) => Some(space),
                None => return Err(EngineError::Other(format!("no such space: {space_id}"))),
            },
            None => None,
        };
        let host_device = match (&space, device_id) {
            (Some(space), _) => space.device_id.clone(),
            (None, Some(device_id)) => device_id.to_string(),
            (None, None) => {
                return Err(EngineError::Other(
                    "createChat needs a spaceId or a deviceId".into(),
                ));
            }
        };
        let archived = matches!(&origin, Some(ChatOrigin::ArchivedExternal(_)));
        let provisional = match &origin {
            Some(ChatOrigin::Provisional(tag)) => Some(tag.clone()),
            _ => None,
        };
        let (parent_chat_id, scheduled_task_info) = match origin {
            Some(ChatOrigin::Parent(parent)) if !parent.trim().is_empty() => (Some(parent), None),
            Some(ChatOrigin::Scheduled(task) | ChatOrigin::ArchivedExternal(task)) => (None, Some(task)),
            _ => (None, None),
        };
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: chat_id.to_string(),
                device_id: host_device.clone(),
                title: None,
                archived,
                cwd: Some(cwd.unwrap_or_else(|| {
                    space
                        .as_ref()
                        .filter(|space| !space.is_personal())
                        .map(|s| s.path.clone())
                        .unwrap_or_else(|| "~".to_string())
                })),
                branch: None,
                checkout_id: None,
                source_context: None,
                config,
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                // Born on chat2: a brand-new chat has an empty doc — nothing
                // to seed, no migration race to lose. Only pre-existing chats
                // go through the seed+flip path (the host migration sweep).
                room_gen: Some(2),
                harness_session_cwd: None,
                actual_model: None,
                scheduled_task: scheduled_task_info
                    .as_ref()
                    .map(|task| task.task_name.clone()),
                scheduled_task_info,
                standing_role_successor: provisional.clone(),
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                parent_chat_id,
                side_of: None,
                side_absorbed_through: None,
                linked_pr_url: None,
                linked_pr_source: None,
                linked_ticket_id: None,
                linked_ticket_source: None,
            })
        })?;
        Ok(())
    }

    /// Create `chat_id` as a side chat of `main_id`: same host, Space, cwd,
    /// checkout and model, read-only, linked by `sideOf` (never
    /// `parentChatId`).
    /// Idempotent by id.
    pub fn create_side_chat(&self, chat_id: &str, main_id: &str) -> Result<Chat, EngineError> {
        if let Some(existing) = self.read(|doc| doc.chat(chat_id))? {
            return Ok(existing);
        }
        let main = self
            .read(|doc| doc.chat(main_id))?
            .ok_or_else(|| EngineError::Other(format!("no such chat: {main_id}")))?;
        if main.is_side_chat() {
            return Err(EngineError::Other("a side chat can't open its own side chat".into()));
        }
        let side = Chat {
            id: chat_id.to_string(),
            device_id: main.device_id.clone(),
            title: Some(mast_proto::side_chat::SIDE_CHAT_TITLE.into()),
            archived: false,
            cwd: main.cwd.clone(),
            branch: main.branch.clone(),
            checkout_id: main.checkout_id.clone(),
            source_context: None,
            // Read-only with approvals off: nobody answers a side chat's
            // prompts (dispatch re-applies this on every run).
            config: main.config.clone().map(|mut config| {
                // Antigravity has no read-only mode: keep the main sandbox.
                if mast_proto::side_chat::harness_supports_read_only(config.harness) {
                    config.sandbox = mast_proto::SandboxLevel::ReadOnly;
                }
                config.auto_approve = true;
                config
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            actual_model: None,
            space_id: main.space_id.clone(),
            last_seen_at: None,
            room_gen: Some(2),
            parent_chat_id: None,
            side_of: Some(main.id.clone()),
            side_absorbed_through: None,
            scheduled_task: None,
            scheduled_task_info: None,
            standing_role_successor: None,
            linked_pr_url: None,
            linked_pr_source: None,
            linked_ticket_id: None,
            linked_ticket_source: None,
        };
        self.mutate(|doc| doc.upsert_chat(&side))?;
        Ok(side)
    }

    /// Record how far a side chat has been absorbed into its main chat.
    pub fn set_chat_side_absorbed_through(&self, chat_id: &str, message_id: Option<&str>) -> Result<(), EngineError> {
        self.mutate(|doc| doc.set_chat_side_absorbed_through(chat_id, message_id))?;
        Ok(())
    }

    // ── spaces (Mutate surface + owner stamps) ──────────────────────────────

    /// Create a space (any device). Idempotent by id; a live duplicate of the
    /// same `(deviceId, path)` is a no-op backstop (the UI reuses via
    /// WatchSpaces). `git_detected` is seeded from the picker's FolderEntry;
    /// the owning device's SpacesSync re-verifies.
    pub fn create_space(
        &self,
        space_id: &str,
        device_id: &str,
        path: &str,
        name: Option<String>,
        git_detected: bool,
    ) -> Result<(), EngineError> {
        let spaces = self.read(|doc| doc.read_spaces())?;
        if spaces
            .iter()
            .any(|s| s.id == space_id || (s.device_id == device_id && s.path == path))
        {
            return Ok(());
        }
        self.mutate(|doc| -> Result<(), EngineError> {
            doc.create_space_with_pr_prompts(
                &Space {
                    id: space_id.to_string(),
                    device_id: device_id.to_string(),
                    path: path.to_string(),
                    name,
                    git_detected,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                },
                &mast_proto::PrReviewPrompts::neutral(),
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn rename_space(&self, space_id: &str, name: Option<&str>) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_space(space_id, name))?)
    }

    /// Hard-delete a space and its chats (registry cascade — one atomic batch).
    /// The caller (rpc layer) tears down live runs / doc-host handles for the
    /// returned chat ids.
    pub fn delete_space(&self, space_id: &str) -> Result<DeletedSpace, EngineError> {
        Ok(self.mutate(|doc| doc.delete_space(space_id))?)
    }

    /// Synced seen marker (any device; LWW + monotonic guard in the doc layer).
    pub fn mark_chat_seen(
        &self,
        chat_id: &str,
        at: chrono::DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_seen(chat_id, at))?)
    }

    /// Owner-only git stamp (SpacesSync). Refuses rows owned by another device.
    pub fn set_space_git(
        &self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        match self.read(|doc| doc.space(space_id))? {
            Some(space) if space.device_id == self.inner.config.device_id => {
                Ok(self
                    .mutate(|doc| doc.set_space_git(space_id, detected, checkout_id, Utc::now()))?)
            }
            Some(space) => {
                tracing::warn!(
                    space = %space_id, owner = %space.device_id,
                    "refusing git stamp on space owned by another device"
                );
                Ok(false)
            }
            None => Ok(false),
        }
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, EngineError> {
        Ok(self.read(|doc| doc.read_spaces())?)
    }

    pub fn rename_chat(&self, chat_id: &str, title: &str) -> Result<bool, EngineError> {
        self.mutate(|doc| {
            if !crate::work_outline::writes_disabled()
                && let Some(chat) = doc.chat(chat_id)?
                && let Some(space) = chat.space_id.as_deref()
            {
                let operation = format!(
                    "chat-item:{}",
                    mast_proto::derive_id(chat_id, "default-item")
                );
                let item_id = mast_proto::derive_id(&operation, mast_proto::roles::ITEM);
                if doc.chat_assigned_work_item(chat_id, space).as_deref() == Some(&item_id)
                    && let Some(item) = doc.row(mast_doc::KIND_WORK_ITEMS, &item_id)
                    && item.fields.get("title").and_then(serde_json::Value::as_str)
                        == Some(chat_item_title(chat.title.as_deref()).as_str())
                {
                    let actor = mast_proto::WorkOutlineActor {
                        device_id: self.device_id().to_owned(),
                        ..Default::default()
                    };
                    doc.update_work_item(
                        &mast_proto::UpdateWorkItemParams {
                            work_item_id: item_id,
                            operation_id: operation,
                            changes: mast_proto::WorkItemChanges {
                                title: Some(chat_item_title(Some(title))),
                                ..Default::default()
                            },
                        },
                        &actor,
                        Utc::now().timestamp_millis(),
                    )
                    .map_err(|e| EngineError::Other(e.to_wire_message()))?;
                }
            }
            Ok(doc.rename_chat(chat_id, title)?)
        })
    }

    pub fn set_chat_scheduled_task(&self, chat_id: &str, task: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_scheduled_task(chat_id, task))?)
    }

    /// Backdate a chat's activity timestamps (epoch ms). Returns false when
    /// the chat doesn't exist.
    pub fn set_chat_activity(
        &self,
        chat_id: &str,
        last_message_at: Option<i64>,
        created_at: Option<i64>,
    ) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        if let Some(ms) = last_message_at {
            chat.last_message_at = chrono::DateTime::<Utc>::from_timestamp_millis(ms);
        }
        if let Some(ms) = created_at
            && let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        {
            chat.created_at = at;
        }
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Re-home a chat to another device (tooling/seeds; a future device
    /// migration flow will drive this). Returns false when the chat doesn't
    /// exist.
    pub fn set_chat_host(&self, chat_id: &str, device_id: &str) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        chat.device_id = device_id.to_string();
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Upsert a chat row copied verbatim from another profile (local→synced
    /// import). Same write path as every live mutation, so the row persists
    /// and pushes like any other; the caller fixes `room_gen` beforehand.
    pub fn import_chat_row(&self, chat: &Chat) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_chat(chat))?)
    }

    /// Upsert a space row copied verbatim from another profile (local→synced
    /// import).
    pub fn import_space_row(&self, space: &Space) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_space(space))?)
    }

    /// Flip the chat's sync room generation (docs/chat2-sync.md M2) — the
    /// host calls this in the same breath as seeding the chat2 checkpoint.
    pub fn set_chat_room_gen(&self, chat_id: &str, room_gen: u32) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_room_gen(chat_id, room_gen))?)
    }

    pub(crate) fn set_before_chat_unarchive(&self, hook: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>) {
        *lock(&self.inner.before_chat_unarchive) = Some(hook);
    }

    pub fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<bool, EngineError> {
        let _transition = lock(&self.inner.chat_archive_transition);
        if !archived && self.chat(chat_id)?.is_some_and(|chat| chat.archived) {
            let hook = lock(&self.inner.before_chat_unarchive).clone();
            if let Some(hook) = hook { hook(chat_id).map_err(EngineError::Other)?; }
        }
        Ok(self.mutate(|doc| doc.set_chat_archived(chat_id, archived))?)
    }

    pub(crate) fn scheduled_task_actions(&self, key: &str) -> Vec<mast_proto::UserActionRequestView> {
        self.read(|doc| doc.scheduled_task_actions(key, self.device_id()))
    }

    /// Recheck registry protections together with the archive write. Returns
    /// false when a transient protection should defer the scheduled follow-up.
    pub(crate) fn archive_scheduled_run_if<Ready>(
        &self,
        chat_id: &str,
        expected: impl Fn(&Chat) -> bool,
        load_readiness_check: impl FnOnce() -> Result<Ready, EngineError>,
    ) -> Result<bool, EngineError>
    where
        Ready: FnOnce() -> Result<bool, EngineError>,
    {
        enum Status {
            Done,
            Blocked,
            Candidate,
        }
        let status = |doc: &RegistryDoc| -> Result<Status, EngineError> {
            let Some(chat) = doc.chat(chat_id)? else {
                return Ok(Status::Done);
            };
            if chat.device_id != self.device_id()
                || chat.archived
                || doc.scheduled_success_archived(chat_id)
                || !expected(&chat)
                || doc.sidebar_preferences().is_some_and(|prefs| {
                    prefs.pinned_session_ids.iter().any(|id| id == chat_id)
                })
            {
                return Ok(Status::Done);
            }
            if doc.has_open_action_for_chat(chat_id) {
                return Ok(Status::Blocked);
            }
            Ok(Status::Candidate)
        };
        match self.read(&status)? {
            Status::Done => return Ok(true),
            Status::Blocked => return Ok(false),
            Status::Candidate => {}
        }
        // Load potentially cold session state after the cheap registry
        // checks, outside the registry lock. Recheck under lock before write.
        let ready = load_readiness_check()?;
        let _transition = lock(&self.inner.chat_archive_transition);
        self.mutate(|doc| match status(doc)? {
            Status::Done => Ok(true),
            Status::Blocked => Ok(false),
            Status::Candidate => {
                if !ready()? {
                    return Ok(false);
                }
                doc.archive_scheduled_success(chat_id)?;
                Ok(true)
            }
        })
    }

    pub(crate) fn open_user_action_chats(&self) -> std::collections::HashSet<String> {
        self.read(RegistryDoc::open_user_action_chats)
    }

    fn scheduled_run_cleanup_generation(doc: &RegistryDoc) -> u64 {
        doc.kinds_generation(&[
            mast_doc::registry::KIND_CHATS,
            mast_doc::registry::KIND_SIDEBAR_PINS,
            mast_doc::registry::work_outline::KIND_USER_ACTION_REQUESTS,
        ])
    }

    pub(crate) fn scheduled_run_cleanup_marker(&self) -> u64 {
        self.read(Self::scheduled_run_cleanup_generation)
    }

    /// Commit the confirmed set together, provided its registry inputs have
    /// not changed since selection. In particular, a pin or a Required Action
    /// arriving during selection must win over the archive.
    pub(crate) fn archive_scheduled_runs<'a>(
        &self,
        marker: u64,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<bool, EngineError> {
        let _transition = lock(&self.inner.chat_archive_transition);
        self.mutate(|doc| {
            if Self::scheduled_run_cleanup_generation(doc) != marker {
                return Ok(false);
            }
            for id in ids {
                doc.set_chat_archived(id, true)?;
            }
            Ok(true)
        })
    }

    /// Stamp stable run provenance and the compatible legacy task name together.
    pub fn set_chat_scheduled_task_info(
        &self,
        chat_id: &str,
        task: &mast_proto::ScheduledChat,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_scheduled_task_info(chat_id, task))?)
    }

    /// [`Self::set_chat_scheduled_task_info`] for many chats in one registry
    /// write. Returns how many rows existed.
    pub fn set_chats_scheduled_task_info(
        &self,
        updates: &[(String, mast_proto::ScheduledChat)],
    ) -> Result<usize, EngineError> {
        if updates.is_empty() {
            return Ok(0);
        }
        Ok(self.mutate(|doc| {
            let mut written = 0;
            for (chat_id, task) in updates {
                written += usize::from(doc.set_chat_scheduled_task_info(chat_id, task)?);
            }
            Ok::<_, mast_doc::DocError>(written)
        })?)
    }

    /// LWW full-config replace on the chat row (zeron `SetChatConfig` — the
    /// composer's mid-session model/reasoning/options changes). Returns false
    /// when the chat doesn't exist.
    pub fn set_chat_config(&self, chat_id: &str, config: &ChatConfig) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_config(chat_id, config))?)
    }

    /// Flip only the chat's approval mode (zeron `setChatAutoApprove`): a
    /// read-modify-write of the row's CURRENT config, so a concurrent
    /// model/reasoning pick from another surface isn't clobbered by a stale
    /// full-config replace. Returns false when the chat doesn't exist or has
    /// no config yet — a config-less row can't be patched (`harness` is
    /// required); the composer creates one via `setChatConfig` instead.
    pub fn set_chat_auto_approve(
        &self,
        chat_id: &str,
        auto_approve: bool,
        approval_rules: bool,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| {
            let Some(mut config) = doc.chat(chat_id)?.and_then(|chat| chat.config) else {
                return Ok::<bool, mast_doc::DocError>(false);
            };
            if config.auto_approve == auto_approve && config.approval_rules == approval_rules {
                return Ok(true);
            }
            config.auto_approve = auto_approve;
            config.approval_rules = approval_rules;
            doc.set_chat_config(chat_id, &config)
        })?)
    }

    /// LWW write of one durable link slot (`linkedPrUrl`/`linkedTicketId` +
    /// its source), gated by write-time precedence — manual > created_in_chat
    /// > mentioned (see [`mast_proto::ChatLinkSource::can_overwrite`]).
    /// `value: None` clears the slot; clearing is manual-only (a mined writer
    /// passing `value: None` is refused outright — mining only ever adds a
    /// link, never removes one). Returns `Ok(false)` when the write was
    /// skipped: no such chat row, a mined clear attempt, or an existing
    /// higher-or-equal-ranked source blocked it — none of these are errors,
    /// matching every other best-effort registry write in this file.
    pub fn set_chat_link(
        &self,
        chat_id: &str,
        kind: ChatLinkKind,
        value: Option<&str>,
        source: mast_proto::ChatLinkSource,
    ) -> Result<bool, EngineError> {
        if value.is_none() && source != mast_proto::ChatLinkSource::Manual {
            return Ok(false);
        }
        if kind == ChatLinkKind::Pr {
            return match value {
                Some(url) => self.add_chat_pr_link(chat_id, url, source),
                None => self.clear_chat_pr_links(chat_id),
            };
        }
        let Some(chat) = self.chat(chat_id)? else {
            return Ok(false);
        };
        let current_source = chat.linked_ticket_source;
        if !source.can_overwrite(current_source) {
            return Ok(false);
        }
        // Clearing (value: None) stores no source either — "absent = fall
        // back to inference" per the schema's own contract.
        let stamped_source = value.map(|_| source);
        Ok(self.mutate(|doc| doc.set_chat_ticket_link(chat_id, value, stamped_source))?)
    }

    /// Add one PR without replacing unrelated links. Source precedence is
    /// enforced per canonical URL, not across the chat's whole collection.
    pub fn add_chat_pr_link(
        &self,
        chat_id: &str,
        url: &str,
        source: ChatLinkSource,
    ) -> Result<bool, EngineError> {
        let url = crate::chat_links::canonicalize_pr_url(url);
        if url.is_empty() {
            return Ok(false);
        }
        Ok(self.mutate(|doc| -> Result<bool, mast_doc::DocError> {
            let Some(chat) = doc.chat(chat_id)? else {
                return Ok(false);
            };
            let links = effective_chat_pr_links(doc, chat_id)?;
            let current_source = links
                .iter()
                .find(|link| link.url == url)
                .map(|link| link.source);
            if !source.can_overwrite(current_source) {
                return Ok(false);
            }

            // Materialize a legacy-only link before changing the primary
            // mirror, otherwise adding a second PR would strand the first in
            // a field that can only hold one value.
            if let (Some(legacy_url), Some(legacy_source)) =
                (chat.linked_pr_url.as_deref(), chat.linked_pr_source)
            {
                let legacy_url = crate::chat_links::canonicalize_pr_url(legacy_url);
                doc.upsert_chat_pr_link(
                    chat_id,
                    &ChatPrLink {
                        url: legacy_url,
                        source: legacy_source,
                    },
                )?;
            }
            doc.upsert_chat_pr_link(chat_id, &ChatPrLink { url, source })?;
            mirror_primary_pr_link(doc, chat_id)?;
            Ok(true)
        })?)
    }

    /// Remove exactly one canonical PR URL. Manual RPC callers use this for
    /// per-row unlink; mined writers never call it.
    pub fn remove_chat_pr_link(&self, chat_id: &str, url: &str) -> Result<bool, EngineError> {
        let url = crate::chat_links::canonicalize_pr_url(url);
        Ok(self.mutate(|doc| -> Result<bool, mast_doc::DocError> {
            let Some(chat) = doc.chat(chat_id)? else {
                return Ok(false);
            };
            let legacy_matches = chat
                .linked_pr_url
                .as_deref()
                .map(crate::chat_links::canonicalize_pr_url)
                .as_deref()
                == Some(url.as_str());
            let removed = doc.remove_chat_pr_link(chat_id, &url);
            if legacy_matches {
                doc.set_chat_pr_link(chat_id, None, None)?;
            }
            if removed || legacy_matches {
                mirror_primary_pr_link(doc, chat_id)?;
            }
            Ok(removed || legacy_matches)
        })?)
    }

    /// Remove all durable PR links and clear the legacy primary mirror.
    pub fn clear_chat_pr_links(&self, chat_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| -> Result<bool, mast_doc::DocError> {
            let Some(chat) = doc.chat(chat_id)? else {
                return Ok(false);
            };
            let removed = doc.clear_chat_pr_links(chat_id);
            let had_legacy = chat.linked_pr_url.is_some();
            doc.set_chat_pr_link(chat_id, None, None)?;
            Ok(removed > 0 || had_legacy)
        })?)
    }

    /// Tombstone: removes the chats (and session-status) row; the per-chat session
    /// doc remains untouched.
    pub fn delete_chat(&self, chat_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.delete_chat(chat_id))?)
    }

    pub fn rename_device(&self, device_id: &str, name: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_device(device_id, name))?)
    }

    // ── git metadata (diff-sync host writes) ────────────────────────────────

    /// HEAD-watcher reconciliation: the branch checked out at the chat's cwd.
    pub fn set_chat_branch(&self, chat_id: &str, branch: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_branch(chat_id, branch))?)
    }

    pub fn set_chat_source_context(
        &self,
        chat_id: &str,
        context: &mast_proto::ConversationSourceContext,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_source_context(chat_id, context))?)
    }

    /// Retarget a chat onto another folder (mid-session switch to an existing
    /// worktree). Resume is cwd-scoped — the next run there starts fresh.
    ///
    /// A Chat with a captured execution binding is never retargeted (contract
    /// 6.2): any cwd other than its execution root is refused with
    /// `execution-scope:boundChatRetarget`.
    pub fn set_chat_cwd(&self, chat_id: &str, cwd: &str) -> Result<bool, EngineError> {
        self.guard_bound_retarget(chat_id, cwd)?;
        Ok(self.mutate(|doc| doc.set_chat_cwd(chat_id, cwd))?)
    }

    /// Refuse to move a bound Chat away from its captured execution root.
    pub fn guard_bound_retarget(&self, chat_id: &str, cwd: &str) -> Result<(), EngineError> {
        let Some(binding) = self.checked_chat_execution_binding(chat_id)? else {
            return Ok(());
        };
        let root = binding.resolved.configuration.execution_root;
        let same = !cwd.is_empty()
            && crate::execution_scope::canonical_lenient(std::path::Path::new(&root))
                == crate::execution_scope::canonical_lenient(std::path::Path::new(cwd));
        if same {
            return Ok(());
        }
        Err(EngineError::Other(
            crate::execution_scope::rejection_message(&[
                mast_proto::ExecutionValidationError::BoundChatRetarget {
                    chat_id: chat_id.to_owned(),
                    execution_root: root,
                    requested_path: cwd.to_owned(),
                },
            ]),
        ))
    }

    /// Atomically retarget a chat to a cwd and the identity resolved for that
    /// exact cwd. `None` deliberately clears any prior checkout identity.
    pub fn set_chat_target(
        &self,
        chat_id: &str,
        cwd: &str,
        checkout_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        self.guard_bound_retarget(chat_id, cwd)?;
        Ok(self.mutate(|doc| doc.set_chat_target(chat_id, cwd, checkout_id))?)
    }

    /// Canonical checkout identity for the chat's cwd (diff grouping key).
    pub fn set_chat_checkout(&self, chat_id: &str, checkout_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_checkout(chat_id, checkout_id))?)
    }

    pub fn set_chat_checkout_for_cwd(
        &self,
        chat_id: &str,
        expected_cwd: &str,
        checkout_id: &str,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_checkout_for_cwd(chat_id, expected_cwd, checkout_id))?)
    }

    // ── persistence / teardown ──────────────────────────────────────────────

    /// Persist the snapshot now (shutdown path; bypasses the debounce).
    pub fn flush(&self) {
        self.inner.save_snapshot();
    }

    /// Persist the registry snapshot now and report whether it reached disk,
    /// unlike [`Self::flush`], which logs a failure and moves on. A caller
    /// that must not queue work until the rows it depends on survive a restart
    /// (a standing role's first message) calls this first.
    pub fn persist_registry_now(&self) -> Result<(), EngineError> {
        self.inner.persist_snapshot()
    }

    /// Persist scheduled follow-up registry writes before acknowledging their
    /// queue entries in the scheduler database.
    pub(crate) fn persist_scheduled_followups(&self) -> Result<(), EngineError> {
        self.inner.persist_snapshot()
    }

    /// Shutdown: stamp our `lastSeenAt` (the only periodic-ish row write besides
    /// boot) and flush the snapshot.
    pub fn shutdown(&self) {
        let now = Utc::now();
        let device_id = self.inner.config.device_id.clone();
        if let Err(err) = self.mutate(|doc| doc.set_device_last_seen(&device_id, now)) {
            tracing::warn!(error = %err, "device lastSeenAt stamp failed");
        }
        self.inner.save_snapshot();
    }
}

/// The registry side of the delivery seam: revisions and selections come
/// from the local replica; reports land on this host's session-status row.
impl crate::instruction_registry::InstructionRegistry for WorkspaceHost {
    fn instruction_revision(
        &self,
        revision_id: &str,
    ) -> Option<mast_proto::InstructionRevisionView> {
        WorkspaceHost::instruction_revision(self, revision_id)
    }

    fn selected_instruction_revision(
        &self,
        chat_id: &str,
    ) -> Option<mast_proto::InstructionRevisionPin> {
        WorkspaceHost::selected_instruction_revision(self, chat_id)
    }

    fn report_execution_session(&self, chat_id: &str, report: mast_proto::ExecutionSessionReport) {
        WorkspaceHost::report_execution_session(self, chat_id, &report);
    }
}

fn effective_chat_pr_links(
    doc: &RegistryDoc,
    chat_id: &str,
) -> Result<Vec<ChatPrLink>, mast_doc::DocError> {
    let mut by_url: BTreeMap<String, ChatLinkSource> = BTreeMap::new();
    for link in doc.chat_pr_links(chat_id) {
        let url = crate::chat_links::canonicalize_pr_url(&link.url);
        let current = by_url.get(&url).copied();
        if link.source.can_overwrite(current) {
            by_url.insert(url, link.source);
        }
    }
    if let Some(chat) = doc.chat(chat_id)?
        && let (Some(url), Some(source)) = (chat.linked_pr_url, chat.linked_pr_source)
    {
        let url = crate::chat_links::canonicalize_pr_url(&url);
        let current = by_url.get(&url).copied();
        if source.can_overwrite(current) {
            by_url.insert(url, source);
        }
    }
    let mut links: Vec<ChatPrLink> = by_url
        .into_iter()
        .map(|(url, source)| ChatPrLink { url, source })
        .collect();
    links.sort_by(|a, b| {
        pr_link_source_rank(b.source)
            .cmp(&pr_link_source_rank(a.source))
            .then_with(|| a.url.cmp(&b.url))
    });
    Ok(links)
}

fn mirror_primary_pr_link(doc: &mut RegistryDoc, chat_id: &str) -> Result<(), mast_doc::DocError> {
    let primary = effective_chat_pr_links(doc, chat_id)?.into_iter().next();
    doc.set_chat_pr_link(
        chat_id,
        primary.as_ref().map(|link| link.url.as_str()),
        primary.as_ref().map(|link| link.source),
    )?;
    Ok(())
}

fn pr_link_source_rank(source: ChatLinkSource) -> u8 {
    source.rank()
}

impl WorkspaceHostInner {
    fn bump_changed(&self) {
        self.changed_tx.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn publish(&self) {
        self.publish_lists(false);
    }

    fn publish_lists(&self, clock_tick: bool) {
        let registry_synced = lock(&self.room)
            .as_ref()
            .is_some_and(|room| room.stats().synced);
        let snapshot = {
            let mut doc = lock(&self.reg);
            match doc.reconcile_sidebar_pins(registry_synced) {
                Ok(true) => {
                    // Persist and transmit cleanup just like a user mutation.
                    self.settle_if_local(&mut doc);
                    self.bump_changed();
                    if let Some(room) = lock(&self.room).as_ref() {
                        room.nudge();
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "sidebar pin cleanup failed"),
            }
            self.publish_sidebar_preferences(&doc, registry_synced);
            // Only re-read (and re-parse) the row kinds that changed since
            // the last publish: a session heartbeat must not re-materialize
            // every chat, under the registry lock.
            let mut published = lock(&self.published_generations);
            let settings_kind = mast_doc::registry::work_outline::KIND_WORK_OUTLINE_SPACE_SETTINGS;
            let migration_key = (
                doc.kinds_generation(&[mast_doc::KIND_SPACES]),
                doc.kinds_generation(&[settings_kind]),
            );
            if (registry_synced
                || self
                    .local_authority
                    .load(std::sync::atomic::Ordering::Relaxed))
                && published.pr_prompt_migration != Some(migration_key)
            {
                match migrate_space_pr_prompts(&mut doc, Utc::now().timestamp_millis()) {
                    Ok(changed) => {
                        if changed {
                            self.settle_if_local(&mut doc);
                            self.bump_changed();
                            if let Some(room) = lock(&self.room).as_ref() {
                                room.nudge();
                            }
                        }
                        published.pr_prompt_migration = Some((
                            doc.kinds_generation(&[mast_doc::KIND_SPACES]),
                            doc.kinds_generation(&[settings_kind]),
                        ));
                    }
                    Err(error) => tracing::warn!(%error, "PR prompt migration failed"),
                }
            }
            let chats_gen = doc.kinds_generation(&[mast_doc::KIND_CHATS]);
            let sessions_gen = doc.kinds_generation(&[mast_doc::KIND_SESSIONS]);
            let spaces_gen = doc.kinds_generation(&[mast_doc::KIND_SPACES]);
            (|| -> Result<_, mast_doc::DocError> {
                let devices = doc.read_devices()?;
                let chats = (published.chats != Some(chats_gen))
                    .then(|| doc.read_chats())
                    .transpose()?;
                let sessions = (published.sessions != Some(sessions_gen))
                    .then(|| doc.read_sessions())
                    .transpose()?;
                let spaces = (published.spaces != Some(spaces_gen))
                    .then(|| doc.read_spaces())
                    .transpose()?;
                // Record the generations only once every read succeeded.
                published.chats = Some(chats_gen);
                published.sessions = Some(sessions_gen);
                published.spaces = Some(spaces_gen);
                Ok((devices, chats, sessions, spaces))
            })()
        };
        match snapshot {
            Ok((mut devices, chats, sessions, spaces)) => {
                self.overlay_presence(&mut devices);
                // Retain the latest value even with no subscribers, but don't
                // wake every list for unrelated registry/presence changes.
                if let Some(chats) = chats {
                    publish_if_changed(&self.chats_tx, chats);
                }
                // The periodic presence tick also drives time-based expiry.
                // Unrelated registry writes need no synthetic device event.
                if clock_tick {
                    self.devices_tx.send_replace(devices);
                } else {
                    publish_if_changed(&self.devices_tx, devices);
                }
                if let Some(sessions) = sessions {
                    publish_if_changed(&self.sessions_tx, sessions);
                }
                if let Some(spaces) = spaces {
                    publish_if_changed(&self.spaces_tx, spaces);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry read failed");
            }
        }
    }

    /// Called under the registry lock so publications cannot overtake one another.
    fn publish_sidebar_preferences(&self, doc: &RegistryDoc, synced: bool) {
        let preferences = doc.sidebar_preferences();
        let initialized = preferences.is_some();
        let sections = preferences
            .as_ref()
            .map(|p| p.sections.clone())
            .unwrap_or_default();
        let pins = preferences
            .map(|p| p.pinned_session_ids)
            .unwrap_or_default();
        let local_handoffs = if self.config.edge.is_some() {
            Vec::new()
        } else {
            doc.completed_sidebar_handoffs()
        };
        self.sidebar_preferences_tx.send_if_modified(|current| {
            // Readiness is sticky for this host. A caller that sampled stats
            // before another publisher acquired the lock must not regress it.
            let synced = synced || current.synced;
            if current.synced == synced
                && current.initialized == initialized
                && current.pinned_session_ids == pins
                && current.sections == sections
                && current.local_handoffs == local_handoffs
            {
                return false;
            }
            *current = SidebarPreferencesState {
                revision: current.revision + 1,
                synced,
                initialized,
                pinned_session_ids: pins,
                sections,
                local_handoffs,
            };
            true
        });
    }

    /// Fold the 15s presence heartbeats into the device rows' `lastSeenAt`
    /// before publishing. The row is written on boot/shutdown ONLY (server-
    /// state hygiene), so without this overlay every device looks offline
    /// ~70s after its boot — and a genuinely dead host is indistinguishable
    /// from slow sync. Fresh remote heartbeats also fire the peer-alive hook
    /// (dial-cooldown reset).
    fn overlay_presence(&self, devices: &mut [Device]) {
        let mut alive_peers: Vec<String> = Vec::new();
        {
            // No live room handle is NOT "everyone is offline": the cache (fed
            // by past heartbeats and the relay-status probe) still overlays —
            // a dead registry room must never fake an offline badge for
            // devices whose relay connection is fine.
            let room = lock(&self.room);
            let live_map = room
                .as_ref()
                .map(|room| room.presence())
                .unwrap_or_default();
            let mut seen = lock(&self.presence_seen);
            let now = now_ms();
            let mut live_fresh_peers = 0usize;
            for device in devices.iter_mut() {
                // Freshest of the live presence entry and the cache: the room
                // map's 30s TTL (and its empty state right after a rejoin)
                // must not erase freshness this engine already witnessed — the
                // device is offline only once heartbeats genuinely stop
                // arriving for the UI's whole online window.
                let live = live_map.get(&device.id).copied();
                if device.id != self.config.device_id
                    && live.is_some_and(|ms| now.saturating_sub(ms) < PRESENCE_FRESH_MS)
                {
                    live_fresh_peers += 1;
                }
                let cached = seen.get(&device.id).copied();
                let Some(ms) = live.into_iter().chain(cached).max() else {
                    continue;
                };
                seen.insert(device.id.clone(), ms);
                if let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
                    && device.last_seen_at.is_none_or(|prev| prev < at)
                {
                    device.last_seen_at = Some(at);
                }
                if device.id != self.config.device_id && now.saturating_sub(ms) < PRESENCE_FRESH_MS
                {
                    alive_peers.push(device.id.clone());
                }
            }
            if let Some(room) = room.as_ref() {
                self.check_presence_deafness(room, live_fresh_peers, now);
            }
        }
        if alive_peers.is_empty() {
            return;
        }
        let hook = lock(&self.peer_alive).clone();
        if let Some(hook) = hook {
            for id in &alive_peers {
                hook(id);
            }
        }
    }

    /// The deaf-socket tripwire (see [`PresenceWatch`]). LIVE presence
    /// freshness only — never the seen-cache or relay probe. Escalation
    /// ladder: first all-dark observation → deadline-checked probe (free on a
    /// healthy room); still dark [`PRESENCE_DEAF_REDIAL_MS`] later → fresh-
    /// socket redial (the only cure when the server→client path drops even
    /// probe answers). Disarms after the redial and re-arms when a peer is
    /// next seen live, so a genuinely-offline fleet costs one probe + one
    /// redial, ever.
    fn check_presence_deafness(&self, room: &RegistryClient, live_fresh_peers: usize, now: i64) {
        let mut watch = lock(&self.presence_watch);
        if live_fresh_peers > 0 {
            watch.armed = true;
            watch.dark_since_ms = 0;
            watch.probed = false;
            return;
        }
        if !watch.armed {
            return;
        }
        if watch.dark_since_ms == 0 {
            watch.dark_since_ms = now;
        }
        if !watch.probed {
            tracing::info!(
                "all live peer presence went dark; probing registry room (deaf-socket tripwire)"
            );
            room.probe();
            watch.probed = true;
        } else if now.saturating_sub(watch.dark_since_ms) > PRESENCE_DEAF_REDIAL_MS {
            tracing::warn!(
                dark_ms = now.saturating_sub(watch.dark_since_ms),
                "peer presence still dark after probe; requesting registry room redial"
            );
            room.redial();
            watch.armed = false;
            watch.dark_since_ms = 0;
            watch.probed = false;
        }
    }

    /// Fold queued writes into the local table when no server will ever ack
    /// them (call under the registry lock, right after a write).
    fn settle_if_local(&self, doc: &mut RegistryDoc) {
        if self
            .local_authority
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            doc.settle_pending_locally();
        }
    }

    fn save_snapshot(&self) {
        if let Err(error) = self.persist_snapshot() {
            tracing::warn!(%error, "registry snapshot save failed");
        }
    }

    fn persist_snapshot(&self) -> Result<(), EngineError> {
        // Keep export and disk write serialized: an older background snapshot
        // must not overwrite an acknowledged migration's durable snapshot.
        let mut doc = lock(&self.reg);
        // Any write that bypassed `mutate` settles before it is persisted.
        self.settle_if_local(&mut doc);
        let bytes = doc.to_bytes()?;
        // Taken under the registry lock: exactly the marks whose writes are
        // in `bytes` (see `WorkspaceHost::work_outline_mutate`).
        let marks = std::mem::take(&mut *lock(&self.pending_ledger_marks));
        let saved = if marks.is_empty() {
            self.store.save_snapshot(REGISTRY_DOC_ID, &bytes)
        } else {
            self.store
                .save_snapshot_marking_processed(REGISTRY_DOC_ID, &bytes, &marks)
        };
        saved.map_err(|error| {
            if !marks.is_empty() {
                let mut pending = lock(&self.pending_ledger_marks);
                let newer = std::mem::replace(&mut *pending, marks);
                pending.extend(newer);
            }
            EngineError::Other(format!("registry snapshot save failed: {error}"))
        })
    }

    /// Presence heartbeat — a memory-only frame on the room, never a row write.
    fn presence_tick(&self) {
        if let Some(room) = lock(&self.room).as_ref() {
            room.set_presence(now_ms());
        }
    }
}

/// The checkout a linked git worktree was made from: `<path>/.git` is a FILE
/// containing `gitdir: <git-dir>/worktrees/<name>`. A plain repo's git dir is
/// `<root>/.git`; a submodule's is `<super>/.git/modules/<name>`, whose
/// checkout is its `core.worktree` (e.g. `<super>/flagship`). `None` for a
/// primary checkout (submodule primaries included), a non-repo folder, or any
/// other layout (bare-repo worktrees have no working copy to attribute to).
/// Pure fs reads — no git subprocess; this runs on the synchronous claim path.
pub(crate) fn linked_worktree_root(path: &std::path::Path) -> Option<String> {
    let gitfile = path.join(".git");
    if !std::fs::metadata(&gitfile).ok()?.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&gitfile).ok()?;
    let target = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let mut target = std::path::PathBuf::from(target);
    if target.is_relative() {
        // Rare (`worktree.useRelativePaths`); canonicalize resolves the
        // `../..` hops against the real filesystem.
        target = std::fs::canonicalize(path.join(target)).ok()?;
    }
    // Git writes `commondir` into every worktree's admin dir; a submodule
    // whose own path has a `worktrees` segment doesn't have one.
    let worktrees = target.parent()?;
    if worktrees.file_name()? != "worktrees" || !target.join("commondir").is_file() {
        return None;
    }
    let git_dir = worktrees.parent()?;
    let root = if git_dir.file_name()? == ".git" {
        git_dir.parent()?.to_path_buf()
    } else {
        submodule_checkout(git_dir)?
    };
    Some(root.to_string_lossy().into_owned())
}

/// The working tree of a submodule git dir (`<super>/.git/modules/<name>`):
/// its `core.worktree` (git writes it to `config.worktree` once
/// `extensions.worktreeConfig` is on), else the path the `modules/` nesting
/// spells out.
fn submodule_checkout(git_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for file in ["config.worktree", "config"] {
        if let Ok(config) = std::fs::read_to_string(git_dir.join(file))
            && let Some(worktree) = core_worktree(&config)
        {
            return Some(crate::execution_scope::lexical(&git_dir.join(worktree)));
        }
    }
    let mut names = Vec::new();
    let mut cursor = git_dir;
    loop {
        names.push(cursor.file_name()?);
        let modules = cursor.parent()?;
        if modules.file_name()? != "modules" {
            return None;
        }
        let owner = modules.parent()?;
        if owner.file_name()? == ".git" {
            let mut checkout = owner.parent()?.to_path_buf();
            checkout.extend(names.iter().rev());
            return Some(checkout);
        }
        cursor = owner;
    }
}

/// `core.worktree` from a git config file's text.
fn core_worktree(config: &str) -> Option<&str> {
    let mut in_core = false;
    for line in config.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[') {
            in_core = section
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("core");
            continue;
        }
        if !in_core {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("worktree")
        {
            return Some(value.trim().trim_matches('"'));
        }
    }
    None
}

/// Where a chat at `path` belongs among this device's Spaces.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SpaceHome {
    Existing(String),
    /// No Space fits; create one at this path.
    New(String),
}

/// A linked worktree belongs to the Space it was started from: the closest
/// Space at or above the checkout it was made from that also holds the
/// worktree folder (a flagship worktree under `workspace/.worktrees` belongs
/// to `workspace`, not `workspace/flagship`), short of a broad root like
/// HOME; else the checkout's own Space. Never a Space at the worktree path
/// itself. Any other path, a worktree whose checkout is a broad root
/// included, belongs to the Space at exactly that path.
///
/// Only worktrees compare paths resolved (git writes `gitdir:` with symlinks
/// resolved, Space paths are stored as entered), and only after an exact
/// match fails: resolving touches the filesystem of every Space.
pub(crate) fn space_home(spaces: &[Space], device_id: &str, path: &str) -> SpaceHome {
    use crate::execution_scope::canonical_lenient;
    let own: Vec<&Space> = spaces.iter().filter(|s| s.device_id == device_id).collect();
    let exact = |at: &str| own.iter().find(|s| s.path == at).map(|s| s.id.clone());
    let Some(origin) = worktree_home_origin(std::path::Path::new(path)) else {
        return exact(path).map_or_else(|| SpaceHome::New(path.to_string()), SpaceHome::Existing);
    };
    let resolved = std::cell::OnceCell::new();
    let at = |dir: &std::path::Path| {
        if let Some(id) = dir.to_str().and_then(exact) {
            return Some(id);
        }
        let dir = canonical_lenient(dir);
        resolved
            .get_or_init(|| {
                own.iter()
                    .map(|s| {
                        (
                            s.id.clone(),
                            canonical_lenient(std::path::Path::new(&s.path)),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .iter()
            .find(|(_, space)| *space == dir)
            .map(|(id, _)| id.clone())
    };
    let checkout = std::path::Path::new(&origin);
    let worktree = canonical_lenient(std::path::Path::new(path));
    checkout
        .ancestors()
        .take_while(|dir| !crate::repos::is_broad_workspace_root_resolved(dir))
        .filter(|dir| worktree.starts_with(canonical_lenient(dir)))
        .find_map(at)
        .or_else(|| at(checkout))
        .map_or(SpaceHome::New(origin), SpaceHome::Existing)
}

/// [`worktree_origin`], unless the worktree came from a broad root like HOME
/// (a deleted `~/.claude/worktrees/<name>`): no Space should collect those.
fn worktree_home_origin(path: &std::path::Path) -> Option<String> {
    worktree_origin(path).filter(|origin| {
        !crate::repos::is_broad_workspace_root_resolved(std::path::Path::new(origin))
    })
}

/// [`linked_worktree_root`], plus a worktree folder that is already gone:
/// anything under `.worktrees` (the agent-mode scripts and chat worktrees) or
/// `.claude/worktrees` (Claude Code) came from the folder holding that. A
/// deleted worktree under Mast's own worktrees root (`~/.mast/worktrees/
/// <repoName>/<name>`) is not recognized: its path names only the repo's
/// folder, not where the repo lives.
fn worktree_origin(path: &std::path::Path) -> Option<String> {
    if let Some(root) = linked_worktree_root(path) {
        return Some(root);
    }
    if path.exists() {
        return None;
    }
    let holder = path.ancestors().find_map(|dir| {
        let name = dir.file_name()?;
        let parent = dir.parent()?;
        if name == ".worktrees" {
            Some(parent)
        } else if name == "worktrees" && parent.file_name()? == ".claude" {
            parent.parent()
        } else {
            None
        }
    })?;
    Some(holder.to_string_lossy().into_owned())
}

/// Local live statuses win for this device's chats; every other device's rows come
/// from the registry. Sorted by chat id (stable stream output).
fn merge_sessions(device_id: &str, rows: &[Session], local: &[Session]) -> Vec<Session> {
    let mut merged: std::collections::HashMap<String, Session> = rows
        .iter()
        .filter(|s| s.device_id != device_id)
        .map(|s| (s.chat_id.clone(), s.clone()))
        .collect();
    for session in local {
        merged.insert(session.chat_id.clone(), session.clone());
    }
    let mut list: Vec<Session> = merged.into_values().collect();
    list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    list
}

/// Background task: relay-verified presence. Every [`RELAY_PROBE_INTERVAL_MS`],
/// for each known device whose merged heartbeat freshness has gone stale, ask
/// its DeviceRoom whether the host socket is live (`/device/{id}/status`); a
/// positive answer refreshes the presence cache so the overlay keeps the badge
/// online. The DeviceRoom shares no machinery with the registry room, so a
/// false "offline" now requires BOTH independent paths to be down — at which
/// point the device is, for every purpose the app has, genuinely offline.
/// Steady state (healthy room, fresh heartbeats) probes nothing.
async fn relay_probe_task(weak: Weak<WorkspaceHostInner>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // consume the immediate first tick
    let client = reqwest::Client::new();
    loop {
        tick.tick().await;
        let Some(inner) = weak.upgrade() else { return };
        let Some(edge) = inner.config.edge.clone() else {
            return;
        };
        let self_id = inner.config.device_id.clone();
        let now = now_ms();
        let stale: Vec<String> = {
            let Ok(devices) = lock(&inner.reg).read_devices() else {
                continue;
            };
            let seen = lock(&inner.presence_seen);
            devices
                .into_iter()
                .filter(|d| d.id != self_id)
                .filter(|d| {
                    seen.get(&d.id)
                        .is_none_or(|ms| now.saturating_sub(*ms) >= PRESENCE_FRESH_MS)
                })
                .map(|d| d.id)
                .collect()
        };
        drop(inner);
        if stale.is_empty() {
            continue;
        }
        let Ok(bearer) = edge.bearer().await else {
            continue; // no usable token yet
        };
        let mut refreshed = false;
        for device_id in stale {
            let url = format!(
                "{}/device/{}/status",
                edge.url.trim_end_matches('/'),
                device_id
            );
            let response = client
                .get(&url)
                .bearer_auth(&bearer)
                .timeout(RELAY_PROBE_TIMEOUT)
                .send()
                .await;
            let Ok(response) = response else { continue };
            if !response.status().is_success() {
                continue;
            }
            let Ok(body) = response.json::<serde_json::Value>().await else {
                continue;
            };
            if body
                .get("hostConnected")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                let Some(inner) = weak.upgrade() else { return };
                lock(&inner.presence_seen).insert(device_id.clone(), now_ms());
                tracing::debug!(device = %device_id, "presence: relay-verified alive");
                refreshed = true;
            }
        }
        if refreshed && let Some(inner) = weak.upgrade() {
            inner.publish();
        }
    }
}

/// Background task: reacts to registry changes (local mutations and applied
/// server frames) by re-publishing the watch channels and debouncing snapshots,
/// and refreshes presence every [`PRESENCE_INTERVAL_MS`]. Holds only a weak
/// handle so a dropped host tears the task down.
async fn workspace_task(weak: Weak<WorkspaceHostInner>, mut changed_rx: watch::Receiver<u64>) {
    let mut presence =
        tokio::time::interval(std::time::Duration::from_millis(PRESENCE_INTERVAL_MS));
    presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    presence.tick().await; // consume the immediate first tick
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // host (and its change sender) is gone
                }
                let Some(inner) = weak.upgrade() else { break };
                inner.publish();
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(inner) = weak.upgrade() else { break };
                if let Err(error) = tokio::task::spawn_blocking(move || inner.save_snapshot()).await {
                    tracing::warn!(%error, "registry snapshot worker failed");
                }
            }
            _ = presence.tick() => {
                let Some(inner) = weak.upgrade() else { break };
                inner.presence_tick();
                // Re-publish on the same cadence: remote heartbeats decay when a
                // device goes silent, and watchers (the UI online dot, "host
                // offline" hints) need a tick to observe that staleness.
                inner.publish_lists(true);
            }
        }
    }
}

/// Materialize legacy defaults only for Spaces that do not already have prompts.
fn migrate_space_pr_prompts(doc: &mut RegistryDoc, now_ms: i64) -> Result<bool, EngineError> {
    let generation = doc.generation();
    for space in doc.read_spaces()? {
        if let Err(error) = doc.initialize_pr_review_prompts(
            &space.id,
            &mast_proto::PrReviewPrompts::legacy(),
            now_ms,
        ) {
            tracing::warn!(space_id = %space.id, ?error, "PR prompt migration failed");
        }
    }
    Ok(doc.generation() != generation)
}

fn publish_if_changed<T: PartialEq>(sender: &watch::Sender<T>, value: T) {
    sender.send_if_modified(|current| {
        if *current == value {
            false
        } else {
            *current = value;
            true
        }
    });
}

fn device_name_on_boot(existing_name: Option<&str>, detected_name: &str) -> String {
    existing_name
        .filter(|name| {
            let name = name.trim();
            !name.is_empty() && name != crate::LEGACY_UNKNOWN_DEVICE_NAME
        })
        .unwrap_or(detected_name)
        .to_string()
}

/// Plain-HTTPS registry pull/push derived from the SAME WebSocket URL
/// provider the dial uses (`wss://…/registry/{org}/ws?token=…&device=…`):
/// swap the scheme, swap the `/ws` leaf for `/rows` or `/push`, keep the
/// fresh token+device the provider already mints per attempt. No second
/// auth path to maintain, and the `?beat=1` keeps presence alive for a
/// device that can only reach the edge over HTTPS.
struct WsDerivedRegistryTransport {
    url: Arc<dyn mast_sync::UrlProvider>,
    client: reqwest::Client,
}

impl WsDerivedRegistryTransport {
    fn new(url: Arc<dyn mast_sync::UrlProvider>) -> Self {
        Self {
            url,
            client: reqwest::Client::new(),
        }
    }

    async fn leaf_url(
        provider: &Arc<dyn mast_sync::UrlProvider>,
        leaf: &str,
    ) -> Result<(reqwest::Url, Option<String>), mast_sync::SyncError> {
        let ws = provider.url().await?;
        let mut u = reqwest::Url::parse(&ws)
            .map_err(|e| mast_sync::SyncError::Protocol(format!("bad ws url: {e}")))?;
        let scheme = if u.scheme() == "wss" { "https" } else { "http" };
        let _ = u.set_scheme(scheme);
        let path = u.path().to_string();
        let Some(base) = path.strip_suffix("/ws") else {
            return Err(mast_sync::SyncError::Protocol(
                "ws url without /ws leaf".into(),
            ));
        };
        let new_path = format!("{base}/{leaf}");
        u.set_path(&new_path);
        // Move the WS URL's ?token= into a bearer header (HTTP supports
        // headers; query strings can reach request logs). Other params
        // (device) stay.
        let token = u
            .query_pairs()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned());
        let kept: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, _)| k != "token")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        u.query_pairs_mut().clear();
        for (k, v) in kept {
            u.query_pairs_mut().append_pair(&k, &v);
        }
        Ok((u, token))
    }
}

impl mast_sync::RegistryTransport for WsDerivedRegistryTransport {
    fn fetch(
        &self,
        since: u64,
    ) -> futures::future::BoxFuture<'static, Result<String, mast_sync::SyncError>> {
        let provider = self.url.clone();
        let client = self.client.clone();
        Box::pin(async move {
            let (mut u, token) = Self::leaf_url(&provider, "rows").await?;
            u.query_pairs_mut()
                .append_pair("since", &since.to_string())
                .append_pair("beat", "1");
            let mut req = client.get(u);
            if let Some(token) = token {
                req = req.bearer_auth(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| mast_sync::SyncError::WebSocket(describe_http_error(e)))?;
            if !resp.status().is_success() {
                return Err(mast_sync::SyncError::Protocol(format!(
                    "registry pull http {}",
                    resp.status()
                )));
            }
            resp.text()
                .await
                .map_err(|e| mast_sync::SyncError::WebSocket(describe_http_error(e)))
        })
    }

    fn push(
        &self,
        body: String,
    ) -> futures::future::BoxFuture<'static, Result<String, mast_sync::SyncError>> {
        let provider = self.url.clone();
        let client = self.client.clone();
        Box::pin(async move {
            let (u, token) = Self::leaf_url(&provider, "push").await?;
            let mut req = client
                .post(u)
                .header("content-type", "application/json")
                .body(body);
            if let Some(token) = token {
                req = req.bearer_auth(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| mast_sync::SyncError::WebSocket(describe_http_error(e)))?;
            if !resp.status().is_success() {
                return Err(mast_sync::SyncError::Protocol(format!(
                    "registry push http {}",
                    resp.status()
                )));
            }
            resp.text()
                .await
                .map_err(|e| mast_sync::SyncError::WebSocket(describe_http_error(e)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatLinkKind, WorkspaceHost, device_name_on_boot, linked_worktree_root};

    #[tokio::test]
    async fn workflow_pack_batch_applies_instruction_after_preview() {
        use super::*;
        use crate::workflow_pack::{WorkflowPackMutationPair, plan_instruction_additions};
        use mast_proto::workflow_pack::{WorkflowPack, WorkflowPackMode, WorkflowPackScope, WORKFLOW_PACK_FORMAT, preview_instruction_import, preview_template_import};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let host = WorkspaceHost::open(store, WorkspaceHostConfig {
            device_id:"device".into(), device_name:"Test".into(), platform:"macos".into(),
            org_id:"org".into(), user_id:"user".into(), edge:None,
        }).unwrap();
        host.create_space("space", "device", "/tmp/example", None, false).unwrap();
        let snapshot = host.read(|doc| doc.work_outline_snapshot("space", true));
        let pack = WorkflowPack { format:WORKFLOW_PACK_FORMAT.into(), mode:WorkflowPackMode::Full,
            scopes:vec![WorkflowPackScope { item_path:vec![], instructions:vec!["Rule".into()] }], templates:vec![] };
        let scopes = preview_instruction_import(&pack, &snapshot).unwrap();
        let templates = preview_template_import(&pack, &snapshot).unwrap();
        let pairs: Vec<_> = plan_instruction_additions(&snapshot, &scopes, "import").unwrap().into_iter()
            .map(|addition| WorkflowPackMutationPair::new(addition.create, addition.undo)).collect();
        let actor = mast_proto::WorkOutlineActor { kind:mast_proto::WorkOutlineActorKind::User,
            device_id:"device".into(), chat_id:None, message_id:None };
        let receipt = host.apply_workflow_pack_batch("space", &pack, &scopes, &templates, &pairs, &actor, None).unwrap();
        assert_eq!(receipt.len(), 1);
        let after = host.read(|doc| doc.work_outline_snapshot("space", true));
        assert!(after.instructions.iter().any(|row| !row.archived() && row.text == "Rule"));
        assert!(host.apply_workflow_pack_batch("space", &pack, &scopes, &templates, &pairs, &actor, None).is_err());
        let id = after.instructions.iter().find(|row| !row.archived()).unwrap().id.clone();
        host.work_outline_mutate(&mast_proto::WorkOutlineMutation::UpdateWorkItemInstruction(
            mast_proto::UpdateWorkItemInstructionParams { instruction_id:id.clone(), text:"Edited".into(), operation_id:"edit".into() }
        ), &actor, &[]).unwrap();
        assert!(host.undo_workflow_pack_batch("space", &receipt, &actor, None).is_err());
        host.work_outline_mutate(&mast_proto::WorkOutlineMutation::UpdateWorkItemInstruction(
            mast_proto::UpdateWorkItemInstructionParams { instruction_id:id, text:"Rule".into(), operation_id:"revert".into() }
        ), &actor, &[]).unwrap();
        host.undo_workflow_pack_batch("space", &receipt, &actor, None).unwrap();
        let undone = host.read(|doc| doc.work_outline_snapshot("space", true));
        assert!(undone.instructions.iter().all(|row| row.archived()));
    }

    #[tokio::test]
    async fn workflow_pack_replaces_conflicts_and_templates_and_undoes_them() {
        use super::*;
        use crate::workflow_pack::*;
        use mast_proto::WorkOutlineMutation as M;
        use mast_proto::work_outline_templates::{ImportWorkItemTemplateParams, TemplateNode};
        use mast_proto::workflow_pack::*;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let host = WorkspaceHost::open(store, WorkspaceHostConfig {
            device_id: "device".into(), device_name: "Test".into(), platform: "macos".into(),
            org_id: "org".into(), user_id: "user".into(), edge: None,
        }).unwrap();
        host.create_space("space", "device", "/tmp/example", None, false).unwrap();
        let actor = mast_proto::WorkOutlineActor { kind: mast_proto::WorkOutlineActorKind::User,
            device_id: "device".into(), chat_id: None, message_id: None };
        let node = |title: &str| TemplateNode { title: title.into(), ..Default::default() };
        let import = |name: &str, body: TemplateNode, id: Option<String>, op: &str| M::ImportWorkItemTemplate(ImportWorkItemTemplateParams {
            space_id: "space".into(), name: name.into(), body, template_id: id, operation_id: op.into(),
        });
        host.work_outline_mutate(&M::CreateWorkItemInstruction(mast_proto::CreateWorkItemInstructionParams {
            space_id: "space".into(), scope: mast_proto::InstructionScope::SpaceRoot, text: "Mine".into(),
            placement: None, source: None, operation_id: "mine".into(),
        }), &actor, &[]).unwrap();
        host.work_outline_mutate(&import("Flow", node("Old"), None, "flow"), &actor, &[]).unwrap();
        let snap = || host.read(|doc| doc.work_outline_snapshot("space", true));
        let flow_id = snap().templates[0].id.clone();
        let pack = WorkflowPack { format: WORKFLOW_PACK_FORMAT.into(), mode: WorkflowPackMode::Full,
            scopes: vec![WorkflowPackScope { item_path: vec![], instructions: vec!["Theirs".into()] }],
            templates: vec![
                WorkflowPackTemplate { name: "Flow".into(), body: node("New") },
                WorkflowPackTemplate { name: "Extra".into(), body: node("X") },
            ] };
        let scopes = preview_instruction_import(&pack, &snap()).unwrap();
        let templates = preview_template_import(&pack, &snap()).unwrap();
        let none = WorkflowPackResolutions::default();
        assert!(plan_pack_operations(&snap(), &scopes, &templates, &none, "run").unwrap_err().contains("Choose"));
        let choices = WorkflowPackResolutions {
            scopes: vec![ScopeResolution { item_path: vec![], resolution: ConflictResolution::Replace }],
            templates: vec![TemplateResolution { name: "flow".into(), resolution: ConflictResolution::Replace }],
        };
        let pairs = plan_pack_operations(&snap(), &scopes, &templates, &choices, "run").unwrap();
        assert_eq!(pairs.len(), 4);
        let receipt = host.apply_workflow_pack_batch("space", &pack, &scopes, &templates, &pairs, &actor, None).unwrap();
        let after = snap();
        let live: Vec<_> = after.instructions.iter().filter(|row| !row.archived()).map(|row| row.text.as_str()).collect();
        assert_eq!(live, ["Theirs"]);
        assert_eq!(after.templates.iter().find(|t| t.id == flow_id).unwrap().body, node("New"));
        assert!(after.templates.iter().any(|t| t.name == "Extra"));
        // A later edit makes undo refuse, and nothing is half-undone.
        host.work_outline_mutate(&import("Flow", node("Edited"), Some(flow_id.clone()), "edit"), &actor, &[]).unwrap();
        assert!(host.undo_workflow_pack_batch("space", &receipt, &actor, None).is_err());
        assert_eq!(snap().templates.len(), 2);
        host.work_outline_mutate(&import("Flow", node("New"), Some(flow_id.clone()), "revert"), &actor, &[]).unwrap();
        host.undo_workflow_pack_batch("space", &receipt, &actor, None).unwrap();
        let undone = snap();
        let live: Vec<_> = undone.instructions.iter().filter(|row| !row.archived()).map(|row| row.text.as_str()).collect();
        assert_eq!(live, ["Mine"]);
        assert_eq!(undone.templates.len(), 1);
        assert_eq!(undone.templates[0].body, node("Old"));
    }

    #[tokio::test]
    async fn space_prompt_migration_preserves_existing_and_initializes_new_spaces() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let mut doc = RegistryDoc::new("test-device");
        doc.upsert_space(&Space {
            id: "existing".into(),
            device_id: "test-device".into(),
            path: "/existing".into(),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        })
        .unwrap();
        store
            .save_snapshot(REGISTRY_DOC_ID, &doc.to_bytes().unwrap())
            .unwrap();
        let host = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        let settings = |space: &str| {
            mast_doc::materialize_work_outline(space, &host.work_outline_inputs(space))
                .space_settings
                .pr_review_prompts
        };
        assert_eq!(
            settings("existing"),
            Some(mast_proto::PrReviewPrompts::legacy())
        );
        host.create_space("created", "test-device", "/created", None, false)
            .unwrap();
        assert_eq!(
            settings("created"),
            Some(mast_proto::PrReviewPrompts::neutral())
        );
        let automatic = host.space_for_path("/automatic").unwrap();
        assert_eq!(
            settings(&automatic),
            Some(mast_proto::PrReviewPrompts::neutral())
        );
        host.import_space_row(&Space {
            id: "imported".into(),
            device_id: "test-device".into(),
            path: "/imported".into(),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        })
        .unwrap();
        host.inner.publish();
        assert_eq!(
            settings("imported"),
            Some(mast_proto::PrReviewPrompts::legacy())
        );
    }

    #[tokio::test]
    async fn registry_http_sync_retains_dns_cause() {
        use super::*;
        use crate::http_error::test_support::FailingDns;
        use mast_sync::RegistryTransport;

        let dns = Arc::new(FailingDns::default());
        let transport = WsDerivedRegistryTransport {
            url: Arc::new(mast_sync::StaticUrl(
                "wss://edge.invalid/registry/org/ws?token=token-secret".into(),
            )),
            client: dns.client(),
        };
        let pull = transport.fetch(0).await.unwrap_err();
        let push = transport.push("{}".into()).await.unwrap_err();
        for error in [pull, push] {
            let message = error.to_string();
            assert!(message.contains("injected DNS lookup failure"), "{message}");
            assert!(!message.contains("token-secret"), "{message}");
        }
    }

    #[tokio::test]
    async fn presence_publish_keeps_unchanged_lists_quiet_and_late_subscribers_current() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
                org_id: "test-org".into(),
                user_id: "test-user".into(),
                edge: None,
            },
        )
        .unwrap();
        let chats = host.watch_chats();
        let sessions = host.watch_session_rows();
        let mut spaces = host.watch_spaces();
        let devices = host.watch_devices();

        host.inner.publish();
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        assert!(!spaces.has_changed().unwrap());
        assert!(!devices.has_changed().unwrap());
        host.inner.publish_lists(true);
        assert!(
            devices.has_changed().unwrap(),
            "presence expiry still ticks"
        );
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        assert!(!spaces.has_changed().unwrap());

        host.create_space("space", "test-device", "/project", None, false)
            .unwrap();
        host.inner.publish();
        assert!(
            spaces.has_changed().unwrap(),
            "real changes must wake readers"
        );
        assert_eq!(spaces.borrow_and_update()[0].id, "space");
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        drop(spaces);

        host.rename_space("space", Some("Renamed")).unwrap();
        host.inner.publish();
        assert_eq!(
            host.watch_spaces().borrow()[0].name.as_deref(),
            Some("Renamed")
        );
    }

    #[tokio::test]
    async fn work_outline_sweep_with_nothing_to_do_does_not_bump_the_change_counter() {
        use super::*;
        use mast_doc::{OpKind, RowOp};

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
                org_id: "test-org".into(),
                user_id: "test-user".into(),
                edge: None,
            },
        )
        .unwrap();
        let mut changes = host.watch_changes();
        changes.borrow_and_update();
        let pending = host.registry_batches_written();

        assert_eq!(host.sweep_work_outline_orphans(), 0);
        assert!(!changes.has_changed().unwrap(), "idle sweep must be quiet");
        assert_eq!(
            host.registry_batches_written(),
            pending,
            "and write nothing"
        );

        // An orphan (its Space row never existed) is still swept and notifies.
        host.mutate(|doc| {
            let hlc = doc.mint_hlc();
            doc.write_ops(vec![RowOp {
                kind: mast_doc::KIND_WORK_ITEMS.to_owned(),
                id: "ghost-item".into(),
                op: OpKind::Upsert,
                set: Some(std::collections::BTreeMap::from([(
                    "spaceId".to_owned(),
                    serde_json::json!("ghost-space"),
                )])),
                hlc,
                clocks: None,
            }])
            .unwrap();
        });
        changes.borrow_and_update();
        assert_eq!(host.sweep_work_outline_orphans(), 1);
        assert!(changes.has_changed().unwrap(), "a real sweep notifies");
        changes.borrow_and_update();
        assert_eq!(host.sweep_work_outline_orphans(), 0);
        assert!(!changes.has_changed().unwrap());
    }

    #[tokio::test]
    async fn sidebar_preferences_watch_preserves_explicit_empty_state() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
                org_id: "test-org".into(),
                user_id: "test-user".into(),
                edge: None,
            },
        )
        .unwrap();
        let mut preferences = host.watch_sidebar_preferences();
        assert!(!preferences.borrow().initialized);

        host.change_sidebar_pin(&mast_proto::SidebarPinChange::Unpin {
            session_id: "absent".into(),
        })
        .unwrap();
        host.inner.publish();
        assert!(preferences.has_changed().unwrap());
        let state = preferences.borrow_and_update();
        assert!(state.initialized);
        assert!(!state.synced);
        assert!(state.pinned_session_ids.is_empty());
        let first_revision = state.revision;
        drop(state);
        let acknowledgement = host.sidebar_preferences_snapshot();
        assert_eq!(acknowledgement.revision, first_revision);
        assert!(
            !preferences.has_changed().unwrap(),
            "unchanged acknowledgements must not churn watches"
        );
        host.create_chat("cached", None, Some("test-device"), None, None)
            .unwrap();
        host.change_sidebar_pin(&mast_proto::SidebarPinChange::Pin {
            session_id: "cached".into(),
            after: None,
            before: None,
        })
        .unwrap();
        let acknowledgement = host.sidebar_preferences_snapshot();
        assert!(acknowledgement.revision > first_revision);
        assert_eq!(*preferences.borrow(), acknowledgement);
        host.change_sidebar_pin(&mast_proto::SidebarPinChange::Section {
            change: mast_proto::SidebarSectionChange::Create {
                id: "focus".into(),
                name: "Focus".into(),
            },
        })
        .unwrap();
        let sections = host.sidebar_preferences_snapshot();
        assert!(sections.revision > acknowledgement.revision);
        assert_eq!(sections.sections[0].name, "Focus");
        assert_eq!(*preferences.borrow(), sections);
        host.change_sidebar_pin(&mast_proto::SidebarPinChange::Section {
            change: mast_proto::SidebarSectionChange::Collapse {
                id: "focus".into(),
                collapsed: true,
            },
        })
        .unwrap();
        let collapsed = host.sidebar_preferences_snapshot();
        assert!(collapsed.revision > sections.revision);
        assert!(collapsed.sections[0].collapsed);
        assert_eq!(*preferences.borrow(), collapsed);
    }

    /// Shared setup for the `set_chat_link` precedence/set-clear tests below:
    /// a fresh workspace with one chat row, ready to link.
    fn linkable_test_host() -> (tempfile::TempDir, WorkspaceHost) {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
                org_id: "test-org".into(),
                user_id: "test-user".into(),
                edge: None,
            },
        )
        .unwrap();
        host.create_chat("c1", None, Some("test-device"), None, None)
            .unwrap();
        (dir, host)
    }

    #[tokio::test]
    async fn scheduled_chat_creation_recovers_missing_metadata_once() {
        let (dir, host) = linkable_test_host();
        let original = host.chat("c1").unwrap().unwrap();
        assert!(original.scheduled_task_info.is_none());
        let config = mast_proto::ChatConfig {
            harness: mast_proto::HarnessId::Mock,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            approval_rules: false,
        };
        let task = mast_proto::ScheduledChat {
            task_id: "daily".into(),
            task_name: "Daily".into(),
            run_id: "occurrence".into(),
            output: mast_proto::ScheduledTaskOutput::Background,
            ..Default::default()
        };
        let create = |task| {
            host.create_scheduled_chat(
                "c1",
                None,
                config.clone(),
                dir.path().to_string_lossy().into_owned(),
                task,
            )
            .unwrap();
        };

        create(task.clone());
        let recovered = host.chat("c1").unwrap().unwrap();
        assert_eq!(recovered.created_at, original.created_at);
        assert_eq!(recovered.scheduled_task.as_deref(), Some("Daily"));
        assert_eq!(recovered.scheduled_task_info.as_ref(), Some(&task));
        let row = host.registry_row(mast_doc::KIND_CHATS, "c1").unwrap();

        create(mast_proto::ScheduledChat {
            task_name: "Changed task name".into(),
            ..task
        });
        assert_eq!(host.registry_row(mast_doc::KIND_CHATS, "c1"), Some(row));
    }

    #[tokio::test]
    async fn branch_review_marks_are_inherited_once() {
        let (_dir, host) = linkable_test_host();
        let mut branch = mast_proto::DiffReviewState {
            target: "branch:owner/repo#feature".into(),
            head_sha: Some("abc".into()),
            ..Default::default()
        };
        branch.viewed_hunks.insert("a".repeat(64));
        branch.viewed_files.insert("changed.rs".into());
        host.put_diff_review_state(&branch).unwrap();
        let mut inherited = host.inherit_diff_review_state("pr:owner/repo#42", &branch.target).unwrap().unwrap();
        assert_eq!(inherited.viewed_hunks, branch.viewed_hunks);
        assert!(inherited.viewed_files.is_empty());
        inherited.viewed_hunks.clear();
        host.put_diff_review_state(&inherited).unwrap();
        assert!(host.inherit_diff_review_state(&inherited.target, &branch.target).unwrap().unwrap().viewed_hunks.is_empty());
    }

    #[tokio::test]
    async fn diff_review_state_roundtrip_in_workspace_host() {
        let (_dir, host) = linkable_test_host();
        let mut state = mast_proto::DiffReviewState {
            target: "pr:owner/repo#42".into(),
            head_sha: Some("abc".into()),
            ..Default::default()
        };
        state.viewed_hunks.insert("a".repeat(64));
        assert!(host.put_diff_review_state(&state).unwrap());
        assert_eq!(host.diff_review_state(&state.target).unwrap(), Some(state.clone()));
        assert!(host.delete_diff_review_state(&state.target).unwrap());
        assert_eq!(host.diff_review_state(&state.target).unwrap(), None);
    }

    #[tokio::test]
    async fn set_chat_link_precedence_manual_is_not_clobbered_by_mined_writes() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();

        assert!(
            host.set_chat_link(
                "c1",
                ChatLinkKind::Pr,
                Some("https://github.com/acme/widgets/pull/1"),
                ChatLinkSource::Manual,
            )
            .unwrap()
        );

        // A different mined URL is an additional link, not an attempted
        // overwrite of the manual one.
        assert!(
            host.set_chat_link(
                "c1",
                ChatLinkKind::Pr,
                Some("https://github.com/acme/widgets/pull/2"),
                ChatLinkSource::CreatedInChat,
            )
            .unwrap()
        );
        // Per-URL precedence still protects the manual link itself, including
        // canonical spellings of the same GitHub URL.
        assert!(
            !host
                .set_chat_link(
                    "c1",
                    ChatLinkKind::Pr,
                    Some("http://github.com/ACME/WIDGETS/pull/1/files"),
                    ChatLinkSource::Mentioned,
                )
                .unwrap()
        );
        let links = host.chat_pr_links("c1").unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].source, ChatLinkSource::Manual);
        let chat = host.chat("c1").unwrap().unwrap();
        assert_eq!(
            chat.linked_pr_url.as_deref(),
            Some("https://github.com/acme/widgets/pull/1")
        );
        assert_eq!(chat.linked_pr_source, Some(ChatLinkSource::Manual));
    }

    #[tokio::test]
    async fn set_chat_link_precedence_created_in_chat_beats_mentioned_but_not_manual() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();

        assert!(
            host.set_chat_link(
                "c1",
                ChatLinkKind::Pr,
                Some("https://github.com/acme/widgets/pull/1"),
                ChatLinkSource::CreatedInChat,
            )
            .unwrap()
        );

        // A later `mentioned` write for the same canonical URL cannot
        // downgrade the created-in-chat source.
        assert!(
            !host
                .set_chat_link(
                    "c1",
                    ChatLinkKind::Pr,
                    Some("github.com/acme/widgets/pull/1"),
                    ChatLinkSource::Mentioned,
                )
                .unwrap()
        );
        let chat = host.chat("c1").unwrap().unwrap();
        assert_eq!(
            chat.linked_pr_url.as_deref(),
            Some("https://github.com/acme/widgets/pull/1")
        );
        assert_eq!(chat.linked_pr_source, Some(ChatLinkSource::CreatedInChat));

        // Another created PR is retained alongside it.
        assert!(
            host.set_chat_link(
                "c1",
                ChatLinkKind::Pr,
                Some("https://github.com/acme/widgets/pull/3"),
                ChatLinkSource::CreatedInChat,
            )
            .unwrap()
        );
        assert_eq!(host.chat_pr_links("c1").unwrap().len(), 2);

        // Manual adds a third link and becomes the deterministic legacy
        // primary without removing either created PR.
        assert!(
            host.set_chat_link(
                "c1",
                ChatLinkKind::Pr,
                Some("https://github.com/acme/widgets/pull/4"),
                ChatLinkSource::Manual,
            )
            .unwrap()
        );
        let chat = host.chat("c1").unwrap().unwrap();
        assert_eq!(
            chat.linked_pr_url.as_deref(),
            Some("https://github.com/acme/widgets/pull/4")
        );
        assert_eq!(chat.linked_pr_source, Some(ChatLinkSource::Manual));
        assert_eq!(host.chat_pr_links("c1").unwrap().len(), 3);
    }

    #[tokio::test]
    async fn set_chat_link_clear_is_manual_only() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();

        host.set_chat_link(
            "c1",
            ChatLinkKind::Pr,
            Some("url-1"),
            ChatLinkSource::Mentioned,
        )
        .unwrap();

        // A mined "clear" attempt (value: None, non-manual source) is
        // refused outright — mining never removes a link.
        assert!(
            !host
                .set_chat_link("c1", ChatLinkKind::Pr, None, ChatLinkSource::Mentioned)
                .unwrap()
        );
        assert!(
            !host
                .set_chat_link("c1", ChatLinkKind::Pr, None, ChatLinkSource::CreatedInChat)
                .unwrap()
        );
        let chat = host.chat("c1").unwrap().unwrap();
        assert_eq!(chat.linked_pr_url.as_deref(), Some("url-1"));

        // A manual clear works and drops the source too — "absent = fall
        // back to inference", not "absent with a stale source".
        assert!(
            host.set_chat_link("c1", ChatLinkKind::Pr, None, ChatLinkSource::Manual)
                .unwrap()
        );
        let chat = host.chat("c1").unwrap().unwrap();
        assert!(chat.linked_pr_url.is_none());
        assert!(chat.linked_pr_source.is_none());
    }

    #[tokio::test]
    async fn remove_chat_pr_link_is_targeted_and_legacy_rows_fall_back() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();

        // Simulate a pre-collection registry row.
        host.mutate(|doc| {
            doc.set_chat_pr_link(
                "c1",
                Some("github.com/acme/widgets/pull/1"),
                Some(ChatLinkSource::Mentioned),
            )
        })
        .unwrap();
        assert_eq!(
            host.chat_pr_links("c1").unwrap()[0].url,
            "https://github.com/acme/widgets/pull/1"
        );

        host.add_chat_pr_link(
            "c1",
            "https://github.com/acme/widgets/pull/2",
            ChatLinkSource::Manual,
        )
        .unwrap();
        assert_eq!(host.chat_pr_links("c1").unwrap().len(), 2);
        assert!(
            host.remove_chat_pr_link("c1", "http://github.com/ACME/WIDGETS/pull/1/files")
                .unwrap()
        );
        let links = host.chat_pr_links("c1").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/acme/widgets/pull/2");
    }

    #[tokio::test]
    async fn set_chat_link_the_pr_and_ticket_slots_are_independent() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();

        host.set_chat_link(
            "c1",
            ChatLinkKind::Pr,
            Some("pr-url"),
            ChatLinkSource::Manual,
        )
        .unwrap();
        host.set_chat_link(
            "c1",
            ChatLinkKind::Ticket,
            Some("ENG-1"),
            ChatLinkSource::Mentioned,
        )
        .unwrap();
        let chat = host.chat("c1").unwrap().unwrap();
        assert_eq!(chat.linked_pr_url.as_deref(), Some("pr-url"));
        assert_eq!(chat.linked_pr_source, Some(ChatLinkSource::Manual));
        assert_eq!(chat.linked_ticket_id.as_deref(), Some("ENG-1"));
        assert_eq!(chat.linked_ticket_source, Some(ChatLinkSource::Mentioned));

        // Clearing the PR slot must not touch the ticket slot.
        host.set_chat_link("c1", ChatLinkKind::Pr, None, ChatLinkSource::Manual)
            .unwrap();
        let chat = host.chat("c1").unwrap().unwrap();
        assert!(chat.linked_pr_url.is_none());
        assert_eq!(chat.linked_ticket_id.as_deref(), Some("ENG-1"));
    }

    #[tokio::test]
    async fn set_chat_link_on_a_missing_chat_row_is_a_quiet_no_op() {
        use mast_proto::ChatLinkSource;
        let (_dir, host) = linkable_test_host();
        assert!(
            !host
                .set_chat_link(
                    "no-such-chat",
                    ChatLinkKind::Pr,
                    Some("url"),
                    ChatLinkSource::Manual
                )
                .unwrap()
        );
    }

    #[test]
    fn boot_repairs_the_legacy_unknown_device_sentinel() {
        assert_eq!(
            device_name_on_boot(Some("unknown-device"), "MacBook Pro"),
            "MacBook Pro"
        );
    }

    #[test]
    fn boot_preserves_a_user_selected_device_name() {
        assert_eq!(
            device_name_on_boot(Some("Work laptop"), "MacBook Pro"),
            "Work laptop"
        );
    }

    #[test]
    fn linked_worktree_resolves_to_the_checkout_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let wt = dir.path().join("clever-ember");
        let admin = root.join(".git").join("worktrees").join("clever-ember");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join(".git/worktrees/clever-ember").display()
            ),
        )
        .unwrap();
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(root.to_str().unwrap())
        );
    }

    #[test]
    fn primary_checkouts_and_plain_folders_resolve_to_none() {
        let dir = tempfile::tempdir().unwrap();
        // Primary checkout: `.git` is a directory.
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(primary.join(".git")).unwrap();
        assert_eq!(linked_worktree_root(&primary), None);
        // Not a repo at all.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(linked_worktree_root(&plain), None);
        // A `.git` file pointing somewhere that is not `<root>/.git/worktrees/<name>`.
        let odd = dir.path().join("odd");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert_eq!(linked_worktree_root(&odd), None);
    }

    /// A worktree folder at `wt` whose `.git` file points into
    /// `<git_dir>/worktrees/<name>`, laid out as `git worktree add` does:
    /// symlinks resolved in the `gitdir:` line, `commondir` in the admin dir.
    fn link_worktree(git_dir: &std::path::Path, wt: &std::path::Path) {
        let name = wt.file_name().unwrap();
        let admin = git_dir.join("worktrees").join(name);
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(wt).unwrap();
        let admin = std::fs::canonicalize(&admin).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    }

    #[test]
    fn submodule_worktrees_resolve_to_the_submodule_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let ws = std::fs::canonicalize(dir.path()).unwrap().join("workspace");
        let module = ws.join(".git/modules/flagship");
        let wt = ws.join(".worktrees/flagship-x");
        link_worktree(&module, &wt);
        let expected = ws.join("flagship");

        // `extensions.worktreeConfig` repos keep `core.worktree` here.
        std::fs::write(
            module.join("config.worktree"),
            "[core]\n\tworktree = ../../../flagship\n",
        )
        .unwrap();
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(expected.to_str().unwrap())
        );

        std::fs::remove_file(module.join("config.worktree")).unwrap();
        std::fs::write(
            module.join("config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\tworktree = /wrong\n",
        )
        .unwrap();
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(expected.to_str().unwrap()),
            "no core.worktree: the modules/ layout names the checkout"
        );

        // The submodule's own primary checkout is not a linked worktree.
        std::fs::create_dir_all(&expected).unwrap();
        std::fs::write(
            expected.join(".git"),
            format!("gitdir: {}\n", module.display()),
        )
        .unwrap();
        assert_eq!(linked_worktree_root(&expected), None);
    }

    #[test]
    fn nested_submodule_worktrees_follow_the_modules_layout() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let ws = base.join("workspace");
        let wt = base.join("wt");
        link_worktree(&ws.join(".git/modules/a/modules/b"), &wt);
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(ws.join("a/b").to_str().unwrap())
        );

        // A submodule checked out at a path with a `worktrees` segment is a
        // primary checkout: its git dir has no `commondir`.
        let primary = ws.join("tools/worktrees/x");
        let module = ws.join(".git/modules/tools/worktrees/x");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::write(
            primary.join(".git"),
            format!("gitdir: {}\n", module.display()),
        )
        .unwrap();
        assert_eq!(linked_worktree_root(&primary), None);
    }

    fn space_at(id: &str, device_id: &str, path: &std::path::Path) -> super::Space {
        super::Space {
            id: id.into(),
            device_id: device_id.into(),
            path: path.to_string_lossy().into_owned(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: super::Utc::now(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn worktrees_belong_to_the_closest_space_at_or_above_their_checkout() {
        use super::{SpaceHome, space_home};
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap().join("real");
        // Spaces are stored as entered, here through a symlink; git's
        // `gitdir:` lines name the resolved paths.
        let alias = dir.path().join("alias");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let ws = real.join("workspace");
        let module_wt = ws.join(".worktrees/flagship-x");
        link_worktree(&ws.join(".git/modules/flagship"), &module_wt);
        let mast = real.join("mast");
        let mast_wt = mast.join(".worktrees/feature");
        link_worktree(&mast.join(".git"), &mast_wt);
        let outside_wt = real.parent().unwrap().join("outside/feature");
        link_worktree(&mast.join(".git"), &outside_wt);
        let s = |p: &std::path::Path| p.to_str().unwrap().to_string();
        let existing = |id: &str| SpaceHome::Existing(id.into());

        let mut spaces = vec![
            space_at("ws", "dev", &alias.join("workspace")),
            space_at("phantom", "dev", &mast_wt),
            space_at("peer-mast", "peer", &mast),
            space_at("root", "dev", std::path::Path::new("/")),
            space_at("real", "dev", &real),
        ];
        // No `flagship` Space: the submodule worktree climbs to `workspace`,
        // matched through the symlink.
        assert_eq!(space_home(&spaces, "dev", &s(&module_wt)), existing("ws"));
        // A Space at the worktree path itself is never the home, another
        // device's Space at the checkout doesn't count, and the climb stops
        // at the closest Space holding the worktree.
        assert_eq!(space_home(&spaces, "dev", &s(&mast_wt)), existing("real"));
        spaces.retain(|space| space.id != "real");
        // Never a broad root like `/`, whatever it holds.
        assert_eq!(
            space_home(&spaces, "dev", &s(&mast_wt)),
            SpaceHome::New(s(&mast))
        );
        spaces.push(space_at("mast", "dev", &alias.join("mast")));
        assert_eq!(space_home(&spaces, "dev", &s(&mast_wt)), existing("mast"));
        // With a `flagship` Space too, a flagship worktree started from
        // `workspace` stays there; one started inside flagship goes to it.
        spaces.push(space_at("flagship", "dev", &ws.join("flagship")));
        assert_eq!(space_home(&spaces, "dev", &s(&module_wt)), existing("ws"));
        let inner_wt = ws.join("flagship/.claude/worktrees/inner");
        link_worktree(&ws.join(".git/modules/flagship"), &inner_wt);
        assert_eq!(
            space_home(&spaces, "dev", &s(&inner_wt)),
            existing("flagship")
        );

        // No Space holds a worktree outside every Space: the checkout's own
        // Space, never a Space above it.
        let only_above = vec![space_at("real", "dev", &real)];
        assert_eq!(
            space_home(&only_above, "dev", &s(&outside_wt)),
            SpaceHome::New(s(&mast))
        );
        assert_eq!(
            space_home(&spaces, "dev", &s(&outside_wt)),
            existing("mast")
        );

        // A deleted worktree still maps to the folder holding its worktrees.
        let gone = ws.join(".worktrees/flagship/long-gone");
        assert_eq!(space_home(&spaces, "dev", &s(&gone)), existing("ws"));
        let gone = ws.join(".claude/worktrees/long-gone");
        assert_eq!(space_home(&spaces, "dev", &s(&gone)), existing("ws"));
        // Anything else is matched exactly, as stored.
        let stored = alias.join("mast");
        assert_eq!(space_home(&spaces, "dev", &s(&stored)), existing("mast"));
        assert_eq!(
            space_home(&spaces, "dev", &s(&mast)),
            SpaceHome::New(s(&mast))
        );
        let plain = real.join("plain");
        assert_eq!(
            space_home(&spaces, "dev", &s(&plain)),
            SpaceHome::New(s(&plain))
        );
    }

    #[tokio::test]
    async fn worktree_spaces_fold_into_their_checkout_space() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let repo = base.join("mast");
        let live_wt = repo.join(".worktrees/live");
        link_worktree(&repo.join(".git"), &live_wt);
        let gone_wt = repo.join(".worktrees/gone");
        let ws = base.join("workspace");
        let module_wt = ws.join(".worktrees/flagship-x");
        link_worktree(&ws.join(".git/modules/flagship"), &module_wt);
        let blocked_wt = repo.join(".worktrees/blocked");
        link_worktree(&repo.join(".git"), &blocked_wt);

        let store = Arc::new(DocsStore::open(&dir.path().join("store")).unwrap());
        let mut doc = RegistryDoc::new("test-device");
        let prompts = mast_proto::PrReviewPrompts::neutral();
        for (id, path) in [
            ("repo", repo.as_path()),
            ("live", live_wt.as_path()),
            ("gone", gone_wt.as_path()),
            ("module", module_wt.as_path()),
            ("blocked", blocked_wt.as_path()),
        ] {
            doc.create_space_with_pr_prompts(&space_at(id, "test-device", path), &prompts)
                .unwrap();
        }
        for (chat, space, cwd) in [
            ("c-live", "live", &live_wt),
            ("c-gone", "gone", &gone_wt),
            ("c-module", "module", &module_wt),
            ("c-blocked", "blocked", &blocked_wt),
        ] {
            doc.claim_chat(chat, cwd.to_str(), Some(space), Utc::now());
        }
        doc.apply_work_outline_mutation(
            &mast_proto::work_outline::WorkOutlineMutation::CreateWorkItem(
                mast_proto::work_outline::CreateWorkItemParams {
                    space_id: "blocked".into(),
                    parent_work_item_id: None,
                    placement: mast_proto::work_outline::WorkOutlinePlacement::Last,
                    title: "Keeps its Space".into(),
                    description: None,
                    status: None,
                    operation_id: "op-blocked".into(),
                },
            ),
            &mast_proto::work_outline::WorkOutlineActor {
                kind: mast_proto::work_outline::WorkOutlineActorKind::User,
                device_id: "test-device".into(),
                chat_id: None,
                message_id: None,
            },
            1_700_000_000_000,
        )
        .unwrap();
        store
            .save_snapshot(REGISTRY_DOC_ID, &doc.to_bytes().unwrap())
            .unwrap();
        let host = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();

        assert_eq!(host.fold_worktree_spaces(&Default::default()), 3);
        let spaces = host.read(|doc| doc.read_spaces()).unwrap();
        let module_home = spaces
            .iter()
            .find(|s| s.path == ws.join("flagship").to_str().unwrap())
            .expect("the submodule checkout gets a Space")
            .id
            .clone();
        let mut ids: Vec<&str> = spaces.iter().map(|s| s.id.as_str()).collect();
        ids.retain(|id| *id != module_home);
        ids.sort();
        assert_eq!(ids, ["blocked", "repo"]);
        let space_of = |chat: &str| host.chat(chat).unwrap().unwrap().space_id.unwrap();
        assert_eq!(space_of("c-live"), "repo");
        assert_eq!(space_of("c-gone"), "repo");
        assert_eq!(space_of("c-module"), module_home);
        assert_eq!(space_of("c-blocked"), "blocked");
        assert_eq!(
            host.chat("c-live").unwrap().unwrap().cwd.as_deref(),
            live_wt.to_str()
        );

        // Idempotent, and new chats in the worktree skip the leftover Space.
        assert_eq!(host.fold_worktree_spaces(&Default::default()), 0);
        host.claim_chat("c-new", blocked_wt.to_str()).unwrap();
        assert_eq!(space_of("c-new"), "repo");

        // A worktree chat that lands after its Space was folded is re-homed;
        // any other orphan is left to the sweep.
        host.mutate(|doc| {
            doc.claim_chat("c-late", live_wt.to_str(), Some("live"), Utc::now());
            doc.claim_chat("c-plain", base.to_str(), Some("live"), Utc::now());
        });
        assert!(host.rehome_worktree_chat(&host.chat("c-late").unwrap().unwrap()));
        assert_eq!(space_of("c-late"), "repo");
        assert!(!host.rehome_worktree_chat(&host.chat("c-plain").unwrap().unwrap()));
        assert_eq!(space_of("c-plain"), "live");

        // A worktree whose home Space doesn't exist (the user deleted it) is
        // left to the sweep rather than bringing a Space back.
        let other_wt = base.join("other/.worktrees/x");
        link_worktree(&base.join("other/.git"), &other_wt);
        host.mutate(|doc| {
            doc.claim_chat("c-homeless", other_wt.to_str(), Some("live"), Utc::now());
        });
        let before = host.read(|doc| doc.read_spaces()).unwrap().len();
        assert!(!host.rehome_worktree_chat(&host.chat("c-homeless").unwrap().unwrap()));
        assert_eq!(host.read(|doc| doc.read_spaces()).unwrap().len(), before);
    }

    #[test]
    fn worktrees_made_from_a_broad_root_belong_to_no_home_space() {
        use super::{SpaceHome, space_home};
        let home = crate::repos::home_dir();
        let gone = home.join(".claude/worktrees/mast-test-long-gone-worktree");
        assert!(!gone.exists());
        let spaces = vec![space_at("home", "dev", &home)];
        let gone = gone.to_str().unwrap();
        assert_eq!(
            space_home(&spaces, "dev", gone),
            SpaceHome::New(gone.into())
        );
    }
}
