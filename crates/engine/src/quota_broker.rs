//! Provider quota broker: decides where work may run given each provider's
//! plan usage, in-flight reservations and the project's cap mode.
//!
//! The broker is pure state plus injected edges (clock, probe, local-usage
//! fallback) so it is unit-testable on a mock clock. It never refreshes
//! OAuth tokens: probes are read-only, and a probe that finds a single-use
//! token expired marks the provider `needs_sign_in` instead of refreshing it.
//!
//! - `BalancedFailover`: dispatch to the provider with the most headroom
//!   below `hard_cap`; stay on the current provider until it passes
//!   `soft_cap`; pause when every provider is at cap.
//! - `PinnedFreeze`: never fail over; at `freeze_cap` freeze, checkpoint and
//!   notify.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use mast_proto::{ProviderQuotaPolicy, ScopedUsageWindow};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    Antigravity,
    ClaudeCode,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Antigravity, Provider::ClaudeCode, Provider::Codex];

    pub fn slug(self) -> &'static str {
        match self {
            Provider::Antigravity => "agy",
            Provider::ClaudeCode => "claude",
            Provider::Codex => "codex",
        }
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock advanced by hand.
pub struct MockClock(Mutex<DateTime<Utc>>);
impl MockClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self(Mutex::new(start))
    }
    pub fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += chrono::Duration::from_std(by).unwrap();
    }
}
impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProbeReading {
    pub windows: Vec<ScopedUsageWindow>,
    pub credit_budget_fraction: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The single-use token needs a refresh; the broker must not do it.
    NeedsSignIn,
    Unavailable(String),
}

/// Read-only provider usage probe. Implementations must never refresh or
/// rotate OAuth tokens.
pub trait UsageProbe: Send + Sync {
    fn probe(&self, provider: Provider) -> Result<ProbeReading, ProbeError>;
}

/// Local fallback (token-usage.sqlite3) used when a probe fails: the used
/// fraction of the provider's budget estimated from recorded token usage.
pub trait LocalUsageFallback: Send + Sync {
    fn local_fraction(&self, provider: Provider) -> Option<f32>;
}

#[derive(Debug, Clone, Copy)]
pub struct BrokerConfig {
    pub hard_cap: f32,
    pub soft_cap: f32,
    pub freeze_cap: f32,
    pub poll_interval: Duration,
    pub agy_poll_interval: Duration,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            hard_cap: 0.85,
            soft_cap: 0.78,
            freeze_cap: 0.90,
            poll_interval: Duration::from_secs(90),
            agy_poll_interval: Duration::from_secs(180),
        }
    }
}

impl BrokerConfig {
    fn interval(&self, provider: Provider) -> Duration {
        match provider {
            Provider::Antigravity => self.agy_poll_interval,
            _ => self.poll_interval,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Dispatch(Provider),
    /// Every provider is at cap; resume at the earliest known reset.
    PauseAll { resume_at: Option<DateTime<Utc>> },
    /// PinnedFreeze reached `freeze_cap`: freeze, checkpoint and notify.
    Freeze { provider: Provider, fraction: f32 },
}

#[derive(Debug, Default)]
struct ProviderState {
    reading: Option<ProbeReading>,
    last_polled: Option<DateTime<Utc>>,
    needs_sign_in: bool,
    fallback_fraction: Option<f32>,
    /// Sum of reservations for dispatched, not yet reflected work.
    inflight: f32,
    /// Smooth weighted round-robin credit.
    wrr: f32,
}

#[derive(Default)]
struct State {
    active: bool,
    providers: BTreeMap<Provider, ProviderState>,
    reservations: BTreeMap<u64, (Provider, f32)>,
    next_reservation: u64,
    current: Option<Provider>,
}

pub struct QuotaBroker {
    config: BrokerConfig,
    clock: Box<dyn Clock>,
    probe: Box<dyn UsageProbe>,
    fallback: Box<dyn LocalUsageFallback>,
    state: Mutex<State>,
}

fn used_fraction(p: &ProviderState) -> Option<f32> {
    let reading_fraction = p.reading.as_ref().map(|r| {
        let windows = r.windows.iter().map(|w| w.used_fraction);
        windows
            .chain(r.credit_budget_fraction)
            .fold(0.0_f32, f32::max)
    });
    reading_fraction.or(p.fallback_fraction)
}

impl QuotaBroker {
    pub fn new(
        config: BrokerConfig,
        clock: Box<dyn Clock>,
        probe: Box<dyn UsageProbe>,
        fallback: Box<dyn LocalUsageFallback>,
    ) -> Self {
        Self {
            config,
            clock,
            probe,
            fallback,
            state: Mutex::new(State::default()),
        }
    }

    /// Polling happens only while orchestration is active.
    pub fn set_orchestration_active(&self, active: bool) {
        self.state.lock().unwrap().active = active;
    }

    /// Poll every provider whose interval has elapsed. Returns the providers
    /// polled. A no-op while orchestration is inactive.
    pub fn poll_due(&self) -> Vec<Provider> {
        let now = self.clock.now();
        let due: Vec<Provider> = {
            let state = self.state.lock().unwrap();
            if !state.active {
                return Vec::new();
            }
            Provider::ALL
                .into_iter()
                .filter(|p| {
                    state
                        .providers
                        .get(p)
                        .and_then(|s| s.last_polled)
                        .is_none_or(|at| {
                            now - at
                                >= chrono::Duration::from_std(self.config.interval(*p)).unwrap()
                        })
                })
                .collect()
        };
        for provider in &due {
            let result = self.probe.probe(*provider);
            let fallback = match result {
                Ok(_) => None,
                Err(_) => self.fallback.local_fraction(*provider),
            };
            let mut state = self.state.lock().unwrap();
            let entry = state.providers.entry(*provider).or_default();
            entry.last_polled = Some(now);
            match result {
                Ok(reading) => {
                    entry.reading = Some(reading);
                    entry.needs_sign_in = false;
                    entry.fallback_fraction = None;
                }
                Err(error) => {
                    entry.reading = None;
                    entry.needs_sign_in = error == ProbeError::NeedsSignIn;
                    entry.fallback_fraction = fallback;
                }
            }
        }
        due
    }

    /// Reserve `fraction` of a provider for work in flight, so concurrent
    /// dispatches see it before the next poll. Returns a reservation id.
    pub fn reserve(&self, provider: Provider, fraction: f32) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_reservation += 1;
        let id = state.next_reservation;
        state.reservations.insert(id, (provider, fraction));
        state.providers.entry(provider).or_default().inflight += fraction;
        id
    }

    pub fn release(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        if let Some((provider, fraction)) = state.reservations.remove(&id) {
            let entry = state.providers.entry(provider).or_default();
            entry.inflight = (entry.inflight - fraction).max(0.0);
        }
    }

    /// Used fraction including in-flight reservations; `None` when unknown.
    pub fn effective_fraction(&self, provider: Provider) -> Option<f32> {
        let state = self.state.lock().unwrap();
        state
            .providers
            .get(&provider)
            .and_then(|p| used_fraction(p).map(|f| f + p.inflight))
    }

    /// Choose where the next unit of work runs.
    pub fn decide(&self, policy: ProviderQuotaPolicy, pinned: Provider) -> Decision {
        self.decide_with_cost(policy, pinned, 0.0)
    }

    /// Decide and, on dispatch, reserve `cost` atomically, so a burst of
    /// spawns sees each other's reservations and cannot overshoot the cap.
    /// Returns the decision and the reservation id when one was taken.
    pub fn dispatch(
        &self,
        policy: ProviderQuotaPolicy,
        pinned: Provider,
        cost: f32,
    ) -> (Decision, Option<u64>) {
        let decision = self.decide_with_cost(policy, pinned, cost);
        let id = match decision {
            Decision::Dispatch(provider) => Some(self.reserve(provider, cost)),
            _ => None,
        };
        (decision, id)
    }

    fn decide_with_cost(&self, policy: ProviderQuotaPolicy, pinned: Provider, cost: f32) -> Decision {
        let mut state = self.state.lock().unwrap();
        match policy {
            ProviderQuotaPolicy::PinnedFreeze => {
                let fraction = state
                    .providers
                    .get(&pinned)
                    .and_then(|p| used_fraction(p).map(|f| f + p.inflight))
                    .unwrap_or(0.0);
                if fraction >= self.config.freeze_cap {
                    Decision::Freeze {
                        provider: pinned,
                        fraction,
                    }
                } else {
                    Decision::Dispatch(pinned)
                }
            }
            ProviderQuotaPolicy::BalancedFailover => self.decide_balanced(&mut state, cost),
        }
    }

    fn decide_balanced(&self, state: &mut State, cost: f32) -> Decision {
        let cfg = self.config;
        let mut headroom: Vec<(Provider, f32)> = Vec::new();
        for provider in Provider::ALL {
            let fraction = state
                .providers
                .get(&provider)
                .and_then(|p| used_fraction(p).map(|f| f + p.inflight))
                .unwrap_or(0.0);
            if fraction + cost <= cfg.hard_cap && fraction < cfg.hard_cap {
                headroom.push((provider, cfg.hard_cap - fraction));
            }
        }
        if headroom.is_empty() {
            let resume_at = state
                .providers
                .values()
                .filter_map(|p| p.reading.as_ref())
                .flat_map(|r| r.windows.iter())
                .filter(|w| w.used_fraction >= cfg.hard_cap)
                .filter_map(|w| w.resets_at)
                .min();
            return Decision::PauseAll { resume_at };
        }
        // Sticky: keep the current provider while it is below the soft cap.
        if let Some(current) = state.current {
            let below_soft = state
                .providers
                .get(&current)
                .and_then(|p| used_fraction(p).map(|f| f + p.inflight))
                .unwrap_or(0.0)
                < cfg.soft_cap;
            if below_soft && headroom.iter().any(|(p, _)| *p == current) {
                return Decision::Dispatch(current);
            }
        }
        // Smooth weighted round-robin over headroom.
        let total: f32 = headroom.iter().map(|(_, h)| h).sum();
        for (provider, weight) in &headroom {
            state.providers.entry(*provider).or_default().wrr += weight;
        }
        let chosen = headroom
            .iter()
            .max_by(|a, b| {
                let wa = state.providers[&a.0].wrr;
                let wb = state.providers[&b.0].wrr;
                wa.total_cmp(&wb).then(b.0.cmp(&a.0))
            })
            .map(|(p, _)| *p)
            .expect("headroom is non-empty");
        state.providers.entry(chosen).or_default().wrr -= total;
        state.current = Some(chosen);
        Decision::Dispatch(chosen)
    }

    /// One-line wake header, present only when a provider is above the soft cap.
    pub fn wake_header(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        let parts: Vec<String> = Provider::ALL
            .into_iter()
            .filter_map(|p| {
                let f = state
                    .providers
                    .get(&p)
                    .and_then(|s| used_fraction(s).map(|f| f + s.inflight))?;
                (f >= self.config.soft_cap).then(|| format!("{} {:.0}%", p.slug(), f * 100.0))
            })
            .collect();
        (!parts.is_empty()).then(|| format!("quota: {} (soft cap {:.0}%)", parts.join(", "), self.config.soft_cap * 100.0))
    }

    /// Read-only `quota_status` payload, kept under 550 bytes.
    pub fn status_payload(&self) -> Value {
        let state = self.state.lock().unwrap();
        let mut providers = serde_json::Map::new();
        for p in Provider::ALL {
            let entry = state.providers.get(&p);
            let used = entry.and_then(used_fraction);
            let mut obj = serde_json::Map::new();
            obj.insert("used".into(), used.map_or(Value::Null, |f| json!((f * 100.0).round() / 100.0)));
            obj.insert(
                "inflight".into(),
                json!((entry.map_or(0.0, |e| e.inflight) * 100.0).round() / 100.0),
            );
            if let Some(c) = entry
                .and_then(|e| e.reading.as_ref())
                .and_then(|r| r.credit_budget_fraction)
            {
                obj.insert("credit".into(), json!((c * 100.0).round() / 100.0));
            }
            if entry.is_some_and(|e| e.needs_sign_in) {
                obj.insert("signIn".into(), json!(true));
            }
            if entry.is_some_and(|e| e.reading.is_none() && e.fallback_fraction.is_some()) {
                obj.insert("src".into(), json!("local"));
            }
            providers.insert(p.slug().into(), Value::Object(obj));
        }
        json!({
            "active": state.active,
            "hard": self.config.hard_cap,
            "soft": self.config.soft_cap,
            "providers": providers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct FakeProbe(Arc<Mutex<BTreeMap<Provider, Result<ProbeReading, ProbeError>>>>, Arc<Mutex<Vec<Provider>>>);
    impl UsageProbe for FakeProbe {
        fn probe(&self, p: Provider) -> Result<ProbeReading, ProbeError> {
            self.1.lock().unwrap().push(p);
            self.0
                .lock()
                .unwrap()
                .get(&p)
                .cloned()
                .unwrap_or(Err(ProbeError::Unavailable("none".into())))
        }
    }
    struct FakeFallback(Option<f32>);
    impl LocalUsageFallback for FakeFallback {
        fn local_fraction(&self, _: Provider) -> Option<f32> {
            self.0
        }
    }
    struct SharedClock(Arc<MockClock>);
    impl Clock for SharedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0.now()
        }
    }

    type Readings = Arc<Mutex<BTreeMap<Provider, Result<ProbeReading, ProbeError>>>>;

    fn reading(used: f32) -> Result<ProbeReading, ProbeError> {
        Ok(ProbeReading {
            windows: vec![ScopedUsageWindow {
                label: "5h".into(),
                used_fraction: used,
                ..Default::default()
            }],
            credit_budget_fraction: None,
        })
    }

    fn rig(fallback: Option<f32>) -> (QuotaBroker, Arc<MockClock>, Readings, Arc<Mutex<Vec<Provider>>>) {
        let clock = Arc::new(MockClock::new(Utc::now()));
        let readings: Readings = Arc::default();
        let calls = Arc::default();
        let broker = QuotaBroker::new(
            BrokerConfig::default(),
            Box::new(SharedClock(clock.clone())),
            Box::new(FakeProbe(readings.clone(), Arc::clone(&calls))),
            Box::new(FakeFallback(fallback)),
        );
        (broker, clock, readings, calls)
    }

    fn set(readings: &Readings, a: f32, c: f32, x: f32) {
        let mut r = readings.lock().unwrap();
        r.insert(Provider::Antigravity, reading(a));
        r.insert(Provider::ClaudeCode, reading(c));
        r.insert(Provider::Codex, reading(x));
    }

    #[test]
    fn polls_only_while_active_and_on_interval() {
        let (b, clock, readings, calls) = rig(None);
        set(&readings, 0.1, 0.1, 0.1);
        assert!(b.poll_due().is_empty());
        assert!(calls.lock().unwrap().is_empty());
        b.set_orchestration_active(true);
        assert_eq!(b.poll_due().len(), 3);
        clock.advance(Duration::from_secs(89));
        assert!(b.poll_due().is_empty());
        clock.advance(Duration::from_secs(1));
        assert_eq!(b.poll_due(), vec![Provider::ClaudeCode, Provider::Codex]);
        clock.advance(Duration::from_secs(90));
        assert_eq!(b.poll_due().len(), 3, "agy due at 180s");
    }

    #[test]
    fn inflight_reservations_count_until_released() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.5, 0.1, 0.1);
        b.set_orchestration_active(true);
        b.poll_due();
        let id = b.reserve(Provider::Antigravity, 0.2);
        assert!((b.effective_fraction(Provider::Antigravity).unwrap() - 0.7).abs() < 1e-6);
        b.release(id);
        assert!((b.effective_fraction(Provider::Antigravity).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn credit_budget_raises_effective_fraction() {
        let (b, _c, readings, _) = rig(None);
        readings.lock().unwrap().insert(
            Provider::Codex,
            Ok(ProbeReading {
                windows: vec![],
                credit_budget_fraction: Some(0.6),
            }),
        );
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::Codex), Some(0.6));
    }

    #[test]
    fn falls_back_to_local_usage_and_never_refreshes() {
        let (b, _c, readings, _) = rig(Some(0.4));
        readings
            .lock()
            .unwrap()
            .insert(Provider::Codex, Err(ProbeError::NeedsSignIn));
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::Codex), Some(0.4));
        let payload = b.status_payload();
        assert_eq!(payload["providers"]["codex"]["signIn"], true);
        assert_eq!(payload["providers"]["codex"]["src"], "local");
    }

    #[test]
    fn balanced_skips_providers_at_hard_cap() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.85, 0.9, 0.3);
        b.set_orchestration_active(true);
        b.poll_due();
        for _ in 0..5 {
            assert_eq!(
                b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::ClaudeCode),
                Decision::Dispatch(Provider::Codex)
            );
        }
    }

    #[test]
    fn balanced_fails_over_past_soft_cap() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.1, 0.1, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        let first = b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex);
        let Decision::Dispatch(current) = first else { panic!() };
        assert_eq!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::Dispatch(current),
            "sticky below soft cap"
        );
        b.reserve(current, 0.7);
        match b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex) {
            Decision::Dispatch(next) => assert_ne!(next, current),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dispatch_is_headroom_weighted() {
        let (b, _c, readings, _) = rig(None);
        // headroom: agy .75, claude .25, codex 0 (at cap).
        set(&readings, 0.1, 0.6, 0.9);
        b.set_orchestration_active(true);
        b.poll_due();
        // Force non-sticky by reserving after each pick is overkill; use WRR
        // directly: clear stickiness by using soft-cap crossing reservations.
        let mut counts: BTreeMap<Provider, u32> = BTreeMap::new();
        for _ in 0..100 {
            let mut s = b.state.lock().unwrap();
            s.current = None;
            drop(s);
            if let Decision::Dispatch(p) = b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex) {
                *counts.entry(p).or_default() += 1;
            }
        }
        let agy = counts[&Provider::Antigravity];
        let claude = counts[&Provider::ClaudeCode];
        assert!((70..=80).contains(&agy), "agy {agy}");
        assert!((20..=30).contains(&claude), "claude {claude}");
        assert!(!counts.contains_key(&Provider::Codex));
    }

    #[test]
    fn pauses_when_all_at_cap_with_earliest_reset() {
        let (b, clock, readings, _) = rig(None);
        let t = clock.now();
        for (p, mins) in [(Provider::Antigravity, 50), (Provider::ClaudeCode, 20), (Provider::Codex, 90)] {
            readings.lock().unwrap().insert(
                p,
                Ok(ProbeReading {
                    windows: vec![ScopedUsageWindow {
                        used_fraction: 0.9,
                        resets_at: Some(t + chrono::Duration::minutes(mins)),
                        ..Default::default()
                    }],
                    credit_budget_fraction: None,
                }),
            );
        }
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::PauseAll {
                resume_at: Some(t + chrono::Duration::minutes(20))
            }
        );
    }

    #[test]
    fn pinned_never_fails_over_and_freezes_at_ninety() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.0, 0.89, 0.0);
        b.set_orchestration_active(true);
        b.poll_due();
        // 0.89 is above the balanced hard cap but pinned keeps going.
        assert_eq!(
            b.decide(ProviderQuotaPolicy::PinnedFreeze, Provider::ClaudeCode),
            Decision::Dispatch(Provider::ClaudeCode)
        );
        b.reserve(Provider::ClaudeCode, 0.02);
        assert!(matches!(
            b.decide(ProviderQuotaPolicy::PinnedFreeze, Provider::ClaudeCode),
            Decision::Freeze { provider: Provider::ClaudeCode, .. }
        ));
    }

    #[test]
    fn wake_header_only_above_soft_cap_and_payload_is_small() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.2, 0.3, 0.4);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.wake_header(), None);
        set(&readings, 0.2, 0.3, 0.4);
        b.reserve(Provider::Codex, 0.4);
        let header = b.wake_header().unwrap();
        assert!(header.contains("codex 80%") && !header.contains('\n'));
        b.reserve(Provider::ClaudeCode, 0.5);
        b.reserve(Provider::Antigravity, 0.6);
        let payload = serde_json::to_string(&b.status_payload()).unwrap();
        assert!(payload.len() < 550, "{} bytes", payload.len());
    }

    #[test]
    fn burst_reservations_stop_dispatch_overshooting_hard_cap() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.5, 0.5, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        let mut dispatched = 0;
        for _ in 0..50 {
            if let (Decision::Dispatch(_), Some(_)) =
                b.dispatch(ProviderQuotaPolicy::BalancedFailover, Provider::Codex, 0.1)
            {
                dispatched += 1;
            }
        }
        // Each provider has 0.35 of room: three 0.1 spawns each, none more.
        assert_eq!(dispatched, 9);
        for p in Provider::ALL {
            assert!(b.effective_fraction(p).unwrap() <= 0.85 + 1e-6);
        }
        assert!(matches!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::Dispatch(_)
        ));
        assert!(matches!(
            b.dispatch(ProviderQuotaPolicy::BalancedFailover, Provider::Codex, 0.1).0,
            Decision::PauseAll { .. }
        ));
    }

    #[test]
    fn pauses_when_all_three_are_exactly_at_cap() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.85, 0.85, 0.85);
        b.set_orchestration_active(true);
        b.poll_due();
        assert!(matches!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::PauseAll { .. }
        ));
    }

    #[test]
    fn pinned_freeze_keeps_provider_and_ignores_other_headroom() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.0, 0.0, 0.9);
        b.set_orchestration_active(true);
        b.poll_due();
        for _ in 0..3 {
            match b.decide(ProviderQuotaPolicy::PinnedFreeze, Provider::Codex) {
                Decision::Freeze { provider, fraction } => {
                    assert_eq!(provider, Provider::Codex);
                    assert!(fraction >= 0.9);
                }
                other => panic!("{other:?}"),
            }
        }
        // No failover side effect: the balanced cursor is untouched.
        assert_eq!(b.state.lock().unwrap().current, None);
    }
}
