//! This engine's side of an outbound peer link: the paired devices it saved
//! with `mast peer add`, and the loop that collects the worker reports those
//! devices queued for chats here.
//!
//! A paired device (B) cannot call this engine, so when a worker B started for
//! one of our chats calls `report_to_parent`, B keeps the report. This loop
//! dials every saved peer, collects what is waiting, hands each report to
//! [`OrchestratorWakes::report_from_peer`] (the normal "Worker updates" path)
//! and only then acknowledges it. A report is therefore delivered at least
//! once, and the wake keys make a replay harmless.
//!
//! A refused token (revoked, expired) stops the polling for five minutes at a
//! time and says so once; any other failure backs off to a minute.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mast_proto::peer_link as wire;
use mast_rpc::RpcClient;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::now_ms;
use crate::orchestrator_wakes::{OrchestratorWakes, PeerReportOutcome};

const LINKS_FILE: &str = "peer-links.json";
/// How often saved peers are polled.
pub const POLL_EVERY: Duration = Duration::from_secs(3);
const REFUSED_RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a peer may stay unreachable before the parents of its workers are told.
pub const UNREACHABLE_NOTICE_AFTER: Duration = Duration::from_secs(2 * 60);
const STATUS_FILE: &str = "peer-link-status.json";

/// A paired device this engine can dial: written by `mast peer add`, read here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPeer {
    pub name: String,
    pub url: String,
    pub token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Links {
    #[serde(default)]
    pub peers: Vec<SavedPeer>,
}

pub fn links_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LINKS_FILE)
}

/// The saved peers; an absent file is none, a damaged one an error.
pub fn load_links(data_dir: &Path) -> std::io::Result<Links> {
    match std::fs::read(links_path(data_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("peer-links.json is not valid: {e}"))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Links::default()),
        Err(err) => Err(err),
    }
}

/// Replace the saved peers atomically, readable by the owner only (the file holds raw tokens).
pub fn save_links(data_dir: &Path, links: &Links) -> std::io::Result<()> {
    use std::io::Write;
    let path = links_path(data_dir);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&temp)?.write_all(&serde_json::to_vec_pretty(links).map_err(std::io::Error::other)?)?;
    std::fs::rename(&temp, &path)
}

/// How the last poll of one saved peer went, written for `mast peer list` (a
/// separate process) to read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PeerLinkStatus {
    /// The URL that was dialed.
    pub url: String,
    /// `ok`, `refused` or `unreachable`.
    pub state: String,
    pub detail: Option<String>,
    pub last_poll_ms: i64,
    /// When the current outage began, if there is one.
    pub unreachable_since_ms: Option<i64>,
    pub parents_told: usize,
    /// Parent chats on this device that the peer's workers name, kept so an
    /// engine restarted during an outage still knows whom to tell.
    #[serde(default)]
    pub parents: Vec<String>,
}

impl PeerLinkStatus {
    /// Whether nothing but the poll time differs, so a healthy peer does not
    /// rewrite the file on every poll.
    fn same_but_time(&self, other: &Self) -> bool {
        Self { last_poll_ms: other.last_poll_ms, ..self.clone() } == *other
    }
}

/// How often an unchanged status is rewritten, so its age stays meaningful.
const STATUS_REFRESH_MS: i64 = 60_000;

pub fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATUS_FILE)
}

/// The last recorded poll result per saved peer name; absent or damaged is none.
pub fn load_status(data_dir: &Path) -> HashMap<String, PeerLinkStatus> {
    let path = status_path(data_dir);
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_status(data_dir: &Path, status: &HashMap<String, PeerLinkStatus>) {
    let path = status_path(data_dir);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let written = serde_json::to_vec_pretty(status).map_err(std::io::Error::other).and_then(|bytes| std::fs::write(&temp, bytes)).and_then(|_| std::fs::rename(&temp, &path));
    if let Err(error) = written {
        tracing::debug!(%error, "could not write the peer link status");
    }
}

enum PollError {
    /// The peer rejected the token.
    Refused,
    Transient(String),
}

struct Link {
    saved: SavedPeer,
    client: Option<Arc<RpcClient>>,
    retry_at: Instant,
    failures: u32,
    announced_refusal: bool,
    /// Parent chats on this device that the peer's workers have named.
    known_parents: Vec<String>,
    outage: Option<Outage>,
}

/// One stretch of failed polls, from the first failure to the next success.
struct Outage {
    since: Instant,
    since_ms: i64,
    told: Option<usize>,
}

/// Start the collection loop. It reads the saved peers on every round, so
/// `mast peer add` and `remove` take effect without a restart.
pub fn spawn(data_dir: PathBuf, wakes: OrchestratorWakes, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    spawn_with(data_dir, wakes, cancel, POLL_EVERY, UNREACHABLE_NOTICE_AFTER)
}

pub fn spawn_with(data_dir: PathBuf, wakes: OrchestratorWakes, cancel: CancellationToken, poll_every: Duration, notice_after: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut links: HashMap<String, Link> = HashMap::new();
        let mut status: HashMap<String, PeerLinkStatus> = load_status(&data_dir);
        let mut tick = tokio::time::interval(poll_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {}
            }
            let dir = data_dir.clone();
            let saved = match tokio::task::spawn_blocking(move || load_links(&dir)).await {
                Ok(Ok(links)) => links.peers,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "saved peers are unreadable; not collecting reports");
                    continue;
                }
                Err(_) => continue,
            };
            links.retain(|name, _| saved.iter().any(|p| &p.name == name));
            status.retain(|name, _| saved.iter().any(|p| &p.name == name));
            for peer in saved {
                let remembered = status.get(&peer.name).filter(|s| s.url == peer.url).map(|s| s.parents.clone()).unwrap_or_default();
                let fresh = || Link { saved: peer.clone(), client: None, retry_at: Instant::now(), failures: 0, announced_refusal: false, known_parents: remembered.clone(), outage: None };
                let link = links.entry(peer.name.clone()).or_insert_with(fresh);
                if link.saved.url != peer.url || link.saved.token != peer.token {
                    *link = fresh();
                }
                if Instant::now() < link.retry_at {
                    continue;
                }
                // A half-open socket must not stall every other peer's reports.
                let polled = match tokio::time::timeout(POLL_TIMEOUT, poll_once(link, &wakes)).await {
                    Ok(result) => result,
                    Err(_) => Err(PollError::Transient("the peer did not answer in time".into())),
                };
                let outage_detail = match &polled {
                    Ok(()) => None,
                    Err(PollError::Refused) => Some(("refused", "the peer refused this device's token".to_string())),
                    Err(PollError::Transient(error)) => Some(("unreachable", error.clone())),
                };
                match polled {
                    Ok(()) => {
                        if let Some(outage) = link.outage.take() {
                            tracing::info!(peer = %link.saved.name, down_for = ?outage.since.elapsed(), "the peer is reachable again; waiting reports will be collected");
                        }
                        link.failures = 0;
                        link.announced_refusal = false;
                    }
                    Err(PollError::Refused) => {
                        link.client = None;
                        link.retry_at = Instant::now() + REFUSED_RETRY;
                        if !link.announced_refusal {
                            link.announced_refusal = true;
                            tracing::warn!(peer = %link.saved.name, "the peer refused this device's token (revoked or expired); run `mast peer add` again to re-pair");
                        }
                    }
                    Err(PollError::Transient(error)) => {
                        link.client = None;
                        link.failures = link.failures.saturating_add(1);
                        let backoff = poll_every.saturating_mul(1 << link.failures.min(5)).min(MAX_BACKOFF);
                        link.retry_at = Instant::now() + backoff;
                        if link.outage.is_none() {
                            // Once per outage: a dead link must be visible, but not every minute.
                            tracing::warn!(peer = %link.saved.name, url = %link.saved.url, %error, "the peer is unreachable; its workers' reports wait on it until the link is back (check the URL in `mast peer list`)");
                        } else {
                            tracing::debug!(peer = %link.saved.name, %error, ?backoff, "peer report poll failed");
                        }
                    }
                }
                if let Some((_, detail)) = &outage_detail {
                    let outage = link.outage.get_or_insert_with(|| Outage { since: Instant::now(), since_ms: now_ms(), told: None });
                    if outage.told.is_none() && outage.since.elapsed() >= notice_after && !link.known_parents.is_empty() {
                        let id = outage.since_ms.to_string();
                        let detail = format!("{detail}; stored URL {}", link.saved.url);
                        // Nobody told (parents archived, reports off) is tried again on a later poll.
                        let told = wakes.note_peer_unreachable(&link.saved.name, &id, &link.known_parents, &detail).await;
                        outage.told = (told > 0).then_some(told);
                    }
                }
                let entry = PeerLinkStatus {
                    url: link.saved.url.clone(),
                    state: outage_detail.as_ref().map_or("ok", |(state, _)| *state).to_string(),
                    detail: outage_detail.map(|(_, detail)| detail),
                    last_poll_ms: now_ms(),
                    unreachable_since_ms: link.outage.as_ref().map(|o| o.since_ms),
                    parents_told: link.outage.as_ref().and_then(|o| o.told).unwrap_or(0),
                    parents: link.known_parents.clone(),
                };
                let unchanged = status.get(&peer.name).is_some_and(|old| old.same_but_time(&entry) && entry.last_poll_ms - old.last_poll_ms < STATUS_REFRESH_MS);
                if !unchanged {
                    status.insert(peer.name.clone(), entry);
                    let (dir, snapshot) = (data_dir.clone(), status.clone());
                    let _ = tokio::task::spawn_blocking(move || save_status(&dir, &snapshot)).await;
                }
            }
        }
    })
}

async fn client_for(link: &mut Link) -> Result<Arc<RpcClient>, PollError> {
    if let Some(client) = &link.client {
        return Ok(client.clone());
    }
    match mast_rpc::connect_ws_bearer(&link.saved.url, Some(&link.saved.token)).await {
        Ok(client) => {
            let client = Arc::new(client);
            link.client = Some(client.clone());
            Ok(client)
        }
        Err(error) if error.to_string().contains("401 Unauthorized") => Err(PollError::Refused),
        Err(error) => Err(PollError::Transient(error.to_string())),
    }
}

/// Pages one poll may read. Reports that cannot be delivered now are passed
/// over (see [`wire::PeerReportsParams`]), so a poll walks past them; this only
/// bounds one poll when a peer holds a very long queue.
const MAX_PAGES_PER_POLL: usize = 20;

/// Collect what one peer is holding, deliver it, acknowledge it.
async fn poll_once(link: &mut Link, wakes: &OrchestratorWakes) -> Result<(), PollError> {
    let client = client_for(link).await?;
    let peer = link.saved.name.clone();
    let wakes = wakes.clone();
    let parents = collect_reports(&client, &peer, move |peer, report| {
        let wakes = wakes.clone();
        async move { wakes.report_from_peer(&peer, &report).await }
    })
    .await?;
    if !parents.is_empty() {
        link.known_parents = parents;
    }
    Ok(())
}

/// Read a peer's waiting reports page by page, hand each to `deliver` and
/// acknowledge the ones it took. A report `deliver` refuses (`Err`) stays on the
/// peer and is passed over on the next page, so reports for one parent that
/// cannot take them (an archived chat) never hold back the rest of the queue.
async fn collect_reports<F, Fut>(client: &RpcClient, peer: &str, mut deliver: F) -> Result<Vec<String>, PollError>
where
    F: FnMut(String, wire::PeerReportItem) -> Fut,
    Fut: std::future::Future<Output = Result<PeerReportOutcome, String>>,
{
    // Reports passed over so far in this poll: they sit at the front of the peer's queue.
    let mut held = 0usize;
    let mut parents: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..MAX_PAGES_PER_POLL {
        let reply = client
            .call(wire::PEER_REPORTS, serde_json::to_value(wire::PeerReportsParams { skip: held }).unwrap_or_default())
            .await
            .map_err(|e| PollError::Transient(e.to_string()))?;
        let reply: wire::PeerReportsReply = serde_json::from_value(reply).map_err(|e| PollError::Transient(format!("unexpected reply: {e}")))?;
        let full_page = reply.reports.len() >= wire::PEER_REPORTS_PAGE;
        if parents.is_empty() {
            parents = reply.parents;
        }
        // A peer that ignores `skip` offers the same page again: nothing new, so stop.
        let fresh: Vec<wire::PeerReportItem> = reply.reports.into_iter().filter(|r| seen.insert(r.id.clone())).collect();
        if fresh.is_empty() {
            break;
        }
        let mut done = Vec::new();
        for report in fresh {
            let (id, chat_id) = (report.id.clone(), report.chat_id.clone());
            match deliver(peer.to_string(), report).await {
                Ok(PeerReportOutcome::Queued | PeerReportOutcome::ReportsOff) => done.push(id),
                Ok(PeerReportOutcome::Dropped(reason)) => {
                    tracing::warn!(peer, chat = %chat_id, reason, "dropping an undeliverable worker report");
                    done.push(id);
                }
                Err(error) => {
                    held += 1;
                    tracing::warn!(peer, chat = %chat_id, %error, "worker report not delivered yet");
                }
            }
        }
        if !done.is_empty() {
            client
                .call(wire::PEER_REPORTS_ACK, serde_json::to_value(wire::PeerReportsAckParams { ids: done }).unwrap_or_default())
                .await
                .map_err(|e| PollError::Transient(e.to_string()))?;
        }
        if !full_page {
            break;
        }
    }
    Ok(parents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_link::{PAIRING_PREFIX, PEER_PAIR_METHOD, PeerRegistry, RemoteReportSink};
    use async_trait::async_trait;
    use mast_rpc::{RpcError, RpcReply, RpcService};
    use std::sync::Mutex;

    struct Nothing;

    #[async_trait]
    impl RpcService for Nothing {
        async fn handle(&self, _: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            RpcReply::value(&serde_json::json!({}))
        }
    }

    /// A device B with a paired peer and a client connected as that peer.
    async fn paired() -> (tempfile::TempDir, Arc<PeerRegistry>, RpcClient, String) {
        let dir = tempfile::tempdir().unwrap();
        let registry = PeerRegistry::open(dir.path()).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(mast_rpc::serve_ws_listener_authed(listener, registry.authenticator(Arc::new(Nothing))));
        let code = registry.open_pairing().unwrap();
        let pairing = mast_rpc::connect_ws_bearer(&url, Some(&format!("{PAIRING_PREFIX}{code}"))).await.unwrap();
        let token = pairing.call(PEER_PAIR_METHOD, serde_json::json!({"label": "fedora"})).await.unwrap()["token"].as_str().unwrap().to_string();
        let client = mast_rpc::connect_ws_bearer(&url, Some(&token)).await.unwrap();
        let peer_id = registry.list()[0].id.clone();
        (dir, registry, client, peer_id)
    }

    #[tokio::test]
    async fn reports_for_a_parent_that_cannot_take_them_do_not_hold_back_the_rest() {
        let (_dir, registry, client, peer_id) = paired().await;
        registry.claim_chat(&peer_id, "w-stuck", Some("archived-parent")).unwrap();
        registry.claim_chat(&peer_id, "w-fine", Some("live-parent")).unwrap();
        let sink: &dyn RemoteReportSink = &*registry;
        // More than a page for the archived parent, queued first so they head the line.
        let stuck = wire::PEER_REPORTS_PAGE + 10;
        for n in 0..stuck {
            sink.report("w-stuck", None, &format!("stuck {n}")).unwrap().unwrap();
        }
        for n in 0..3 {
            sink.report("w-fine", None, &format!("fine {n}")).unwrap().unwrap();
        }

        let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
        let attempts = Arc::new(Mutex::new(0usize));
        let run = || {
            let (delivered, attempts) = (delivered.clone(), attempts.clone());
            collect_reports(&client, "fedora", move |_, report| {
                let (delivered, attempts) = (delivered.clone(), attempts.clone());
                async move {
                    *attempts.lock().unwrap() += 1;
                    if report.parent_chat_id == "archived-parent" {
                        return Err("the parent chat is archived".to_string());
                    }
                    delivered.lock().unwrap().push(report.text);
                    Ok(PeerReportOutcome::Queued)
                }
            })
        };
        assert!(run().await.is_ok());
        assert_eq!(*delivered.lock().unwrap(), ["fine 0", "fine 1", "fine 2"], "the live parent's reports got through");
        assert_eq!(registry.peer_counts(&peer_id).1, stuck, "the stuck ones wait on the peer, in order");

        // The next poll does not repeat the delivered ones, and the stuck ones are still just retried.
        *attempts.lock().unwrap() = 0;
        assert!(run().await.is_ok());
        assert_eq!(delivered.lock().unwrap().len(), 3);
        assert_eq!(*attempts.lock().unwrap(), stuck, "every stuck report was tried again, none twice");

        // Once the parent can take them, they go out and the peer's queue empties.
        let delivered_late = Arc::new(Mutex::new(0usize));
        let counter = delivered_late.clone();
        let late = collect_reports(&client, "fedora", move |_, _| {
            let counter = counter.clone();
            async move {
                *counter.lock().unwrap() += 1;
                Ok(PeerReportOutcome::Queued)
            }
        })
        .await;
        assert!(late.is_ok());
        assert_eq!(*delivered_late.lock().unwrap(), stuck);
        assert_eq!(registry.peer_counts(&peer_id).1, 0);
    }

    #[tokio::test]
    async fn a_poll_learns_which_parents_the_peers_workers_name() {
        let (_dir, registry, client, peer_id) = paired().await;
        registry.claim_chat(&peer_id, "w1", Some("mac-manager")).unwrap();
        registry.claim_chat(&peer_id, "w2", Some("mac-manager")).unwrap();
        registry.claim_chat(&peer_id, "w3", None).unwrap();
        let parents = collect_reports(&client, "fedora", |_, _| async { Ok(PeerReportOutcome::Queued) }).await.ok().unwrap();
        assert_eq!(parents, ["mac-manager"]);
    }
}
