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

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

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

impl QuotaService {
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
}
