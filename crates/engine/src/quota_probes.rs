//! The quota broker's production edges and the read-only service behind
//! `quota_status` and `GetQuotaHeadroom`.
//!
//! Probes go through [`AgentAccounts::quota_probe`], which reads the live
//! sign-in and never refreshes it. When a probe fails, usage is estimated from
//! `token-usage.sqlite3`. The ledger records tokens but no plan budget, so the
//! estimate is the last good probe scaled by how much the provider's token
//! total has grown since; with no recent good probe, usage is unknown.
//!
//! There is no background poll loop: a read polls whatever is due, so a
//! provider is probed at most once per poll interval however often it is read.
//! Nothing here dispatches work.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use mast_proto::{HarnessId, ScopedUsageWindow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_accounts::{lock, AgentAccounts};
use crate::quota_broker::{
    BrokerConfig, Clock, LocalUsageFallback, ProbeError, ProbeReading, Provider, QuotaBroker,
    SystemClock, UsageProbe,
};
use crate::quota_settings::{self, QuotaSettings};
use crate::token_usage::TokenUsage;

/// How far back the token total behind an estimate reaches, and how long a
/// good probe can anchor one. Matches the shortest plan window (5 hours).
const ESTIMATE_WINDOW_HOURS: i64 = 5;

fn harness(provider: Provider) -> HarnessId {
    match provider {
        Provider::Antigravity => HarnessId::Antigravity,
        Provider::ClaudeCode => HarnessId::ClaudeCode,
        Provider::Codex => HarnessId::Codex,
    }
}

/// A provider's recorded token total since a time, on this device.
trait TokenTotals: Send + Sync {
    fn total_since(&self, provider: Provider, since: DateTime<Utc>) -> Option<u64>;
}

impl TokenTotals for TokenUsage {
    fn total_since(&self, provider: Provider, since: DateTime<Utc>) -> Option<u64> {
        self.harness_tokens_since(
            &format!("{:?}", harness(provider)),
            since.timestamp_millis(),
        )
        .inspect_err(|error| tracing::debug!(target: "usage", %error, "token total unavailable"))
        .ok()
    }
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    fraction: f32,
    at: DateTime<Utc>,
    tokens: u64,
}

/// The last good probe per provider, and the estimate scaled from it.
struct Estimator {
    clock: Box<dyn Clock>,
    totals: Option<Box<dyn TokenTotals>>,
    anchors: Mutex<[Option<Anchor>; 3]>,
}

impl Estimator {
    fn window() -> chrono::Duration {
        chrono::Duration::hours(ESTIMATE_WINDOW_HOURS)
    }

    fn record(&self, provider: Provider, reading: &ProbeReading) {
        let at = self.clock.now();
        let fraction = reading
            .windows
            .iter()
            .flat_map(|w| std::iter::once(w.used_fraction).chain(w.credit_budget_fraction))
            .chain(reading.credit_budget_fraction)
            .filter(|f| f.is_finite())
            .reduce(f32::max);
        let tokens = self
            .totals
            .as_ref()
            .and_then(|t| t.total_since(provider, at - Self::window()));
        lock(&self.anchors)[provider as usize] =
            fraction.zip(tokens).map(|(fraction, tokens)| Anchor {
                fraction,
                at,
                tokens,
            });
    }

    /// `None` (unknown) without a good probe in the last five hours, or when
    /// it had no recorded tokens to scale from.
    fn estimate(&self, provider: Provider) -> Option<f32> {
        let anchor = lock(&self.anchors)[provider as usize]?;
        if self.clock.now() - anchor.at >= Self::window() || anchor.tokens == 0 {
            return None;
        }
        let tokens = self
            .totals
            .as_ref()?
            .total_since(provider, anchor.at - Self::window())?;
        // Never below the last real reading, even if ledger rows were deleted.
        let growth = (tokens as f64 / anchor.tokens as f64).max(1.0);
        Some((f64::from(anchor.fraction) * growth).min(1.0) as f32)
    }
}

struct LiveProbe {
    accounts: AgentAccounts,
    runtime: tokio::runtime::Handle,
    estimator: Arc<Estimator>,
}

impl UsageProbe for LiveProbe {
    /// Blocks; the service calls `poll_due` on a blocking thread.
    fn probe(&self, provider: Provider) -> Result<ProbeReading, ProbeError> {
        let windows = self
            .runtime
            .block_on(self.accounts.quota_probe(harness(provider)))?;
        let reading = ProbeReading {
            windows,
            credit_budget_fraction: None,
        };
        self.estimator.record(provider, &reading);
        Ok(reading)
    }
}

struct LedgerFallback(Arc<Estimator>);

impl LocalUsageFallback for LedgerFallback {
    fn local_fraction(&self, provider: Provider) -> Option<f32> {
        self.0.estimate(provider)
    }
}

/// What `GetQuotaHeadroom` returns: the broker's view of each provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaHeadroom {
    pub providers: Vec<ProviderHeadroom>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHeadroom {
    /// `claude-code`, `codex` or `antigravity`.
    pub harness: String,
    /// The last successful probe's windows; empty when that probe failed.
    pub windows: Vec<ScopedUsageWindow>,
    /// The local estimate, only while probes fail.
    pub estimated: Option<f32>,
    pub needs_sign_in: bool,
}

/// What a `quota_status` reply adds to the account caches.
pub(crate) struct BrokerStatus {
    pub payload: Value,
    pub wake: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct QuotaService(Arc<Inner>);

#[derive(Default)]
struct Inner {
    estimator: OnceLock<Arc<Estimator>>,
    /// Held across a poll so concurrent reads cannot probe twice.
    broker: tokio::sync::Mutex<Option<(QuotaSettings, Arc<QuotaBroker>)>>,
}

/// An engine builds one RPC surface per transport (in-app, relay, peers), so
/// the service is shared per profile store: otherwise each surface would probe
/// on its own schedule. Weak, so it goes when the last surface does.
static SERVICES: LazyLock<Mutex<HashMap<PathBuf, Weak<Inner>>>> = LazyLock::new(Mutex::default);

impl QuotaService {
    pub(crate) fn shared(store_root: &Path) -> Self {
        let mut services = lock(&SERVICES);
        services.retain(|_, service| service.strong_count() > 0);
        if let Some(inner) = services.get(store_root).and_then(Weak::upgrade) {
            return Self(inner);
        }
        let service = Self::default();
        services.insert(store_root.to_path_buf(), Arc::downgrade(&service.0));
        service
    }

    /// The broker for the saved caps, with every due provider polled. Changed
    /// caps rebuild it; it holds no reservations since nothing dispatches.
    async fn refreshed(
        &self,
        accounts: &AgentAccounts,
        usage: Option<TokenUsage>,
        store_root: PathBuf,
    ) -> Result<Arc<QuotaBroker>, String> {
        let mut slot = self.0.broker.lock().await;
        let settings = quota_settings::load(&store_root);
        let broker = match slot.as_ref() {
            Some((saved, broker)) if *saved == settings => broker.clone(),
            _ => {
                let estimator = self.0.estimator.get_or_init(|| {
                    Arc::new(Estimator {
                        clock: Box::new(SystemClock),
                        totals: usage.map(|u| Box::new(u) as Box<dyn TokenTotals>),
                        anchors: Mutex::default(),
                    })
                });
                let mut config = BrokerConfig::default();
                settings.apply_to(&mut config);
                let probe = LiveProbe {
                    accounts: accounts.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    estimator: estimator.clone(),
                };
                let broker = QuotaBroker::new(
                    config,
                    Box::new(SystemClock),
                    Box::new(probe),
                    Box::new(LedgerFallback(estimator.clone())),
                )
                .map_err(|error| error.to_string())?;
                broker.set_orchestration_active(true);
                let broker = Arc::new(broker);
                *slot = Some((settings, broker.clone()));
                broker
            }
        };
        let polling = broker.clone();
        tokio::task::spawn_blocking(move || polling.poll_due())
            .await
            .map_err(|error| error.to_string())?;
        Ok(broker)
    }

    pub(crate) async fn status(
        &self,
        accounts: &AgentAccounts,
        usage: Option<TokenUsage>,
        store_root: PathBuf,
    ) -> Result<BrokerStatus, String> {
        let broker = self.refreshed(accounts, usage, store_root).await?;
        Ok(BrokerStatus {
            payload: broker.status_payload(),
            wake: broker.wake_header(),
        })
    }

    pub(crate) async fn headroom(
        &self,
        accounts: &AgentAccounts,
        usage: Option<TokenUsage>,
        store_root: PathBuf,
    ) -> Result<QuotaHeadroom, String> {
        let broker = self.refreshed(accounts, usage, store_root).await?;
        Ok(broker.headroom())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota_broker::MockClock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    struct Totals(Arc<AtomicU64>);
    impl TokenTotals for Totals {
        fn total_since(&self, _: Provider, _: DateTime<Utc>) -> Option<u64> {
            Some(self.0.load(Ordering::SeqCst))
        }
    }
    struct Shared(Arc<MockClock>);
    impl Clock for Shared {
        fn now(&self) -> DateTime<Utc> {
            self.0.now()
        }
    }

    fn rig() -> (Estimator, Arc<MockClock>, Arc<AtomicU64>) {
        let clock = Arc::new(MockClock::new(Utc::now()));
        let tokens = Arc::new(AtomicU64::new(1_000));
        let estimator = Estimator {
            clock: Box::new(Shared(clock.clone())),
            totals: Some(Box::new(Totals(tokens.clone()))),
            anchors: Mutex::default(),
        };
        (estimator, clock, tokens)
    }

    fn reading(used: f32) -> ProbeReading {
        ProbeReading {
            windows: vec![ScopedUsageWindow {
                used_fraction: used,
                ..Default::default()
            }],
            credit_budget_fraction: None,
        }
    }

    #[test]
    fn estimate_scales_the_last_good_probe_by_token_growth() {
        let (estimator, clock, tokens) = rig();
        assert_eq!(estimator.estimate(Provider::Codex), None, "never probed");
        estimator.record(Provider::Codex, &reading(0.25));
        assert_eq!(estimator.estimate(Provider::Codex), Some(0.25));
        tokens.store(2_000, Ordering::SeqCst);
        assert_eq!(estimator.estimate(Provider::Codex), Some(0.5));
        tokens.store(500, Ordering::SeqCst);
        assert_eq!(estimator.estimate(Provider::Codex), Some(0.25));
        tokens.store(1_000_000, Ordering::SeqCst);
        assert_eq!(estimator.estimate(Provider::Codex), Some(1.0));
        assert_eq!(estimator.estimate(Provider::ClaudeCode), None);
        clock.advance(Duration::from_secs(5 * 60 * 60));
        assert_eq!(estimator.estimate(Provider::Codex), None, "too old");
    }

    #[test]
    fn estimate_is_unknown_without_tokens_to_scale_from() {
        let (estimator, _clock, tokens) = rig();
        tokens.store(0, Ordering::SeqCst);
        estimator.record(Provider::Codex, &reading(0.25));
        tokens.store(1_000, Ordering::SeqCst);
        assert_eq!(estimator.estimate(Provider::Codex), None);
        let no_ledger = Estimator {
            clock: Box::new(SystemClock),
            totals: None,
            anchors: Mutex::default(),
        };
        no_ledger.record(Provider::Codex, &reading(0.25));
        assert_eq!(no_ledger.estimate(Provider::Codex), None);
    }

    /// A local usage service answering every request with `reply`, and the
    /// request lines it saw.
    async fn fake_usage(
        reply: Arc<Mutex<(u16, &'static str)>>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                lock(&seen).push(request.lines().next().unwrap_or_default().to_string());
                let (status, body) = *lock(&reply);
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (url, requests)
    }

    fn signed_in(root: &std::path::Path) -> (AgentAccounts, [PathBuf; 2]) {
        let claude = root.join("claude");
        let codex = root.join("codex");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        let files = [claude.join(".credentials.json"), codex.join("auth.json")];
        std::fs::write(
            &files[0],
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"single-use","expiresAt":0}}"#,
        )
        .unwrap();
        std::fs::write(
            &files[1],
            r#"{"tokens":{"access_token":"b","refresh_token":"single-use"}}"#,
        )
        .unwrap();
        let accounts = AgentAccounts::new(crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.join("data"),
            claude_config_file: claude.join(".claude.json"),
            claude_config_dir: claude,
            codex_home: codex,
            cursor_sdk_auth_file: root.join("cursor.json"),
        });
        (accounts, files)
    }

    #[tokio::test]
    async fn a_rejected_sign_in_is_reported_and_never_refreshed() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, files) = signed_in(root.path());
        let before = files.clone().map(|f| std::fs::read(f).unwrap());
        let (url, requests) = fake_usage(Arc::new(Mutex::new((401, "{}")))).await;
        accounts.set_usage_urls(&url, &url);
        let status = QuotaService::default()
            .status(&accounts, None, root.path().to_path_buf())
            .await
            .unwrap();
        for provider in ["claude", "codex"] {
            let seen = &status.payload["providers"][provider];
            assert_eq!(seen["signIn"], true, "{seen}");
            assert!(seen["used"].is_null(), "{seen}");
        }
        let requests = lock(&requests).clone();
        assert_eq!(requests.len(), 2, "one read per provider: {requests:?}");
        assert!(
            requests.iter().all(|r| r.starts_with("GET /usage ")),
            "{requests:?}"
        );
        assert_eq!(files.map(|f| std::fs::read(f).unwrap()), before);
        assert!(
            !root.path().join("data/agent-accounts").exists(),
            "no slot is saved"
        );
    }

    #[tokio::test]
    async fn reads_are_throttled_and_wake_only_above_the_soft_cap() {
        const CLAUDE_HIGH: &str = r#"{"five_hour":{"utilization":80.0,"resets_at":null}}"#;
        let root = tempfile::tempdir().unwrap();
        let (accounts, _) = signed_in(root.path());
        let (url, requests) = fake_usage(Arc::new(Mutex::new((200, CLAUDE_HIGH)))).await;
        accounts.set_usage_urls(&url, &url);
        let service = QuotaService::default();
        let read = || service.status(&accounts, None, root.path().to_path_buf());
        let status = read().await.unwrap();
        assert_eq!(status.payload["providers"]["claude"]["used"], 0.8);
        assert_eq!(
            status.wake.as_deref(),
            Some("quota: claude 80% (soft cap 78%)")
        );
        read().await.unwrap();
        assert_eq!(lock(&requests).len(), 2, "nothing was due the second time");
        let headroom = service
            .headroom(&accounts, None, root.path().to_path_buf())
            .await
            .unwrap();
        let claude = headroom
            .providers
            .iter()
            .find(|p| p.harness == "claude-code")
            .unwrap();
        assert_eq!(claude.windows[0].used_fraction, 0.8);

        // Changed caps take effect on the next read.
        let settings = QuotaSettings {
            soft_cap: 0.82,
            ..Default::default()
        };
        quota_settings::save(root.path(), &settings).unwrap();
        assert_eq!(read().await.unwrap().wake, None);
    }

    #[test]
    fn surfaces_of_one_profile_share_a_service() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let first = QuotaService::shared(a.path());
        assert!(Arc::ptr_eq(&first.0, &QuotaService::shared(a.path()).0));
        assert!(!Arc::ptr_eq(&first.0, &QuotaService::shared(b.path()).0));
        let weak = Arc::downgrade(&first.0);
        drop(first);
        assert!(weak.upgrade().is_none(), "the registry keeps nothing alive");
    }

    #[test]
    fn the_ledger_sum_is_per_harness_and_windowed() {
        use mast_proto::token_usage::{TokenCounts, UsageMeasurement};
        let root = tempfile::tempdir().unwrap();
        let usage = TokenUsage::open(root.path()).unwrap();
        for (chat, harness, input) in [("a", "codex", 10), ("b", "claude-code", 500)] {
            let request: mast_proto::RunRequest = serde_json::from_value(serde_json::json!({
                "prompt": "p", "cwd": root.path(), "sandbox": "read-only",
                "attachments": [], "harness": harness,
            }))
            .unwrap();
            usage.begin(chat, chat, &request, "Conversation");
            usage.observe(
                chat,
                &mast_proto::AgentEvent::UsageMeasurement {
                    measurement: UsageMeasurement {
                        id: chat.into(),
                        model: Some("m".into()),
                        scope: "turn".into(),
                        counts: TokenCounts {
                            input,
                            output: 2,
                            ..Default::default()
                        },
                    },
                },
            );
        }
        let hour_ago = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(usage.total_since(Provider::Codex, hour_ago), Some(12));
        assert_eq!(usage.total_since(Provider::ClaudeCode, hour_ago), Some(502));
        assert_eq!(usage.total_since(Provider::Antigravity, hour_ago), Some(0));
        let later = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(usage.total_since(Provider::Codex, later), Some(0));
    }
}
