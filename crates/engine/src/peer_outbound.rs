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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mast_proto::peer_link as wire;
use mast_rpc::RpcClient;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::orchestrator_wakes::{OrchestratorWakes, PeerReportOutcome};

const LINKS_FILE: &str = "peer-links.json";
/// How often saved peers are polled.
pub const POLL_EVERY: Duration = Duration::from_secs(3);
const REFUSED_RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

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
}

/// Start the collection loop. It reads the saved peers on every round, so
/// `mast peer add` and `remove` take effect without a restart.
pub fn spawn(data_dir: PathBuf, wakes: OrchestratorWakes, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    spawn_with(data_dir, wakes, cancel, POLL_EVERY)
}

pub fn spawn_with(data_dir: PathBuf, wakes: OrchestratorWakes, cancel: CancellationToken, poll_every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut links: HashMap<String, Link> = HashMap::new();
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
            for peer in saved {
                let fresh = || Link { saved: peer.clone(), client: None, retry_at: Instant::now(), failures: 0, announced_refusal: false };
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
                match polled {
                    Ok(()) => {
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
                        tracing::debug!(peer = %link.saved.name, %error, ?backoff, "peer report poll failed");
                    }
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

/// Collect what one peer is holding, deliver it, acknowledge it.
async fn poll_once(link: &mut Link, wakes: &OrchestratorWakes) -> Result<(), PollError> {
    let client = client_for(link).await?;
    let reply = client
        .call(wire::PEER_REPORTS, serde_json::json!({}))
        .await
        .map_err(|e| PollError::Transient(e.to_string()))?;
    let reply: wire::PeerReportsReply = serde_json::from_value(reply).map_err(|e| PollError::Transient(format!("unexpected reply: {e}")))?;
    if reply.reports.is_empty() {
        return Ok(());
    }
    let mut done = Vec::new();
    for report in &reply.reports {
        match wakes.report_from_peer(&link.saved.name, report).await {
            Ok(PeerReportOutcome::Queued | PeerReportOutcome::ReportsOff) => done.push(report.id.clone()),
            Ok(PeerReportOutcome::Dropped(reason)) => {
                tracing::warn!(peer = %link.saved.name, chat = %report.chat_id, reason, "dropping an undeliverable worker report");
                done.push(report.id.clone());
            }
            Err(error) => {
                tracing::warn!(peer = %link.saved.name, chat = %report.chat_id, %error, "worker report not delivered yet");
            }
        }
    }
    if !done.is_empty() {
        client
            .call(wire::PEER_REPORTS_ACK, serde_json::to_value(wire::PeerReportsAckParams { ids: done }).unwrap_or_default())
            .await
            .map_err(|e| PollError::Transient(e.to_string()))?;
    }
    Ok(())
}
