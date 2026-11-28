//! Provider quota broker: decides where work may run given each provider's
//! plan usage, in-flight reservations and the project's cap mode.
//!
//! The broker is pure state plus injected edges (clock, probe, local-usage
//! fallback) so it is unit-testable on a mock clock. It never refreshes
//! OAuth tokens: probes are read-only, and a probe that finds a single-use
//! token expired marks the provider `needs_sign_in` instead of refreshing it.
//!
//! - `BalancedFailover`: dispatch is weighted by headroom below `hard_cap`;
//!   stay on the current provider until it reaches `soft_cap` and another one
//!   below `soft_cap` has room; pause when no provider is eligible.
//! - `PinnedFreeze`: never fail over; answer `Freeze` when the work would
//!   pass `freeze_cap`. Checkpointing and notifying are the caller's job.
//!
//! Unknown usage is never read as 0%: a provider that was never polled, or
//! whose probe failed with no local estimate, is ineligible, and so is one
//! that needs sign-in (even with a local estimate).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
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
    /// Longest any reservation counts, whether or not probes succeed. Should
    /// exceed both poll intervals.
    pub reservation_ttl: Duration,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            hard_cap: 0.85,
            soft_cap: 0.78,
            freeze_cap: 0.90,
            poll_interval: Duration::from_secs(90),
            agy_poll_interval: Duration::from_secs(180),
            reservation_ttl: Duration::from_secs(600),
        }
    }
}

impl BrokerConfig {
    /// Caps must satisfy `0 < soft_cap < hard_cap < freeze_cap <= 1`.
    pub fn validate(&self) -> Result<(), InvalidBrokerConfig> {
        let ordered = 0.0 < self.soft_cap
            && self.soft_cap < self.hard_cap
            && self.hard_cap < self.freeze_cap
            && self.freeze_cap <= 1.0;
        if ordered {
            Ok(())
        } else {
            Err(InvalidBrokerConfig {
                soft_cap: self.soft_cap,
                hard_cap: self.hard_cap,
                freeze_cap: self.freeze_cap,
            })
        }
    }

    fn interval(&self, provider: Provider) -> Duration {
        match provider {
            Provider::Antigravity => self.agy_poll_interval,
            _ => self.poll_interval,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvalidBrokerConfig {
    pub soft_cap: f32,
    pub hard_cap: f32,
    pub freeze_cap: f32,
}

impl std::fmt::Display for InvalidBrokerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "quota caps must satisfy 0 < soft < hard < freeze <= 1 (soft {}, hard {}, freeze {})",
            self.soft_cap, self.hard_cap, self.freeze_cap
        )
    }
}

impl std::error::Error for InvalidBrokerConfig {}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Dispatch(Provider),
    /// No provider is eligible. `resume_at` is the earliest time one may have
    /// room again: a window reset, a reservation ageing out or expiring or,
    /// for usage with no known reset, the next poll. A hint to decide again,
    /// not a promise.
    ///
    /// `None` means no time can be predicted: every provider needs sign-in or
    /// has unknown usage, or the cost alone exceeds `hard_cap`. It is not
    /// permanent. A sign-in, a later successful poll or a smaller cost can
    /// change the answer, so callers must keep polling and decide again rather
    /// than park the work for good.
    ///
    /// `resume_at` can equal the current time when a poll is overdue. Callers
    /// must run `poll_due` before deciding again and wait a minimum delay
    /// between attempts: nothing is re-read while orchestration is inactive,
    /// so sleeping only until `resume_at` could spin.
    PauseAll {
        resume_at: Option<DateTime<Utc>>,
    },
    /// PinnedFreeze cannot continue: the work would pass `freeze_cap`, or the
    /// provider needs sign-in or its usage is unknown. `fraction` is the usage
    /// before this work (it can be under `freeze_cap` when the cost is what
    /// does not fit); `None` when unknown.
    Freeze {
        provider: Provider,
        fraction: Option<f32>,
    },
}

#[derive(Debug, Default)]
struct ProviderState {
    reading: Option<ProbeReading>,
    last_polled: Option<DateTime<Utc>>,
    needs_sign_in: bool,
    fallback_fraction: Option<f32>,
    /// Smooth weighted round-robin credit.
    wrr: f32,
}

#[derive(Debug, Clone, Copy)]
struct Reservation {
    provider: Provider,
    fraction: f32,
    taken_at: DateTime<Utc>,
}

#[derive(Default)]
struct State {
    active: bool,
    providers: BTreeMap<Provider, ProviderState>,
    /// Dispatched work that probes may not reflect yet.
    reservations: BTreeMap<u64, Reservation>,
    next_reservation: u64,
    current: Option<Provider>,
}

impl State {
    fn inflight(&self, provider: Provider) -> f32 {
        self.reservations
            .values()
            .filter(|r| r.provider == provider)
            .map(|r| r.fraction)
            .sum()
    }

    /// Used fraction including reservations; `None` when unknown.
    fn effective(&self, provider: Provider) -> Option<f32> {
        let used = self.providers.get(&provider).and_then(used_fraction)?;
        Some(used + self.inflight(provider))
    }

    /// `effective`, withheld from providers that must not take new work.
    fn dispatchable(&self, provider: Provider) -> Option<f32> {
        let needs_sign_in = self
            .providers
            .get(&provider)
            .is_some_and(|p| p.needs_sign_in);
        self.effective(provider).filter(|_| !needs_sign_in)
    }
}

pub struct QuotaBroker {
    config: BrokerConfig,
    clock: Box<dyn Clock>,
    probe: Box<dyn UsageProbe>,
    fallback: Box<dyn LocalUsageFallback>,
    state: Mutex<State>,
}

/// Every usage figure behind a provider, with the reset that clears it when
/// one is known. Empty when usage is unknown.
///
/// Window scope is deliberately ignored: a dispatch does not say which model
/// the work will use, so an exhausted model-family window caps the whole
/// provider. This can hold back work another model could have taken, but never
/// sends work into an exhausted window.
fn usage_sources(p: &ProviderState) -> Vec<(f32, Option<DateTime<Utc>>)> {
    let Some(reading) = &p.reading else {
        return p.fallback_fraction.map(|f| (f, None)).into_iter().collect();
    };
    let windows = reading.windows.iter().flat_map(|w| {
        std::iter::once(w.used_fraction)
            .chain(w.credit_budget_fraction)
            .map(|f| (f, w.resets_at))
    });
    windows
        .chain(reading.credit_budget_fraction.map(|f| (f, None)))
        .collect()
}

/// The highest usage figure. A reading with no figures, or with a non-finite
/// one, is unknown rather than 0%.
fn used_fraction(p: &ProviderState) -> Option<f32> {
    let sources = usage_sources(p);
    if sources.iter().any(|(f, _)| !f.is_finite()) {
        return None;
    }
    sources.into_iter().map(|(f, _)| f).reduce(f32::max)
}

/// Whether a provider at `fraction` can take `cost` under `cap`.
fn fits(fraction: f32, cost: f32, cap: f32) -> bool {
    // The second test matters at zero cost: a provider exactly at the cap
    // passes `fraction + cost <= cap` but has no headroom, so it is capped.
    fraction + cost <= cap && fraction < cap
}

impl QuotaBroker {
    pub fn new(
        config: BrokerConfig,
        clock: Box<dyn Clock>,
        probe: Box<dyn UsageProbe>,
        fallback: Box<dyn LocalUsageFallback>,
    ) -> Result<Self, InvalidBrokerConfig> {
        config.validate()?;
        Ok(Self {
            config,
            clock,
            probe,
            fallback,
            state: Mutex::new(State::default()),
        })
    }

    fn interval(&self, provider: Provider) -> chrono::Duration {
        chrono::Duration::from_std(self.config.interval(provider)).unwrap()
    }

    fn reservation_ttl(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.config.reservation_ttl).unwrap()
    }

    /// The state with expired reservations already dropped. Every access goes
    /// through here, so the TTL holds even when nothing is polling.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        let mut state = self.state.lock().unwrap();
        let now = self.clock.now();
        let ttl = self.reservation_ttl();
        state.reservations.retain(|_, r| now - r.taken_at < ttl);
        state
    }

    /// Polling happens only while orchestration is active.
    pub fn set_orchestration_active(&self, active: bool) {
        self.state.lock().unwrap().active = active;
    }

    /// Poll every provider whose interval has elapsed. Returns the providers
    /// polled. A no-op while orchestration is inactive.
    ///
    /// A successful probe drops that provider's reservations that are at least
    /// one poll interval old: the reading has had time to include their usage,
    /// so keeping them would count it twice. Younger ones are kept. A failed
    /// probe drops none; `reservation_ttl` bounds how long a reservation that
    /// is never released can hold headroom while probes keep failing.
    pub fn poll_due(&self) -> Vec<Provider> {
        let now = self.clock.now();
        let due: Vec<Provider> = {
            let state = self.lock_state();
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
                        .is_none_or(|at| now - at >= self.interval(*p))
                })
                .collect()
        };
        for provider in &due {
            let result = self.probe.probe(*provider);
            let fallback = match result {
                Ok(_) => None,
                Err(_) => self.fallback.local_fraction(*provider),
            };
            let mut state = self.lock_state();
            if result.is_ok() {
                let max_age = self.interval(*provider);
                state
                    .reservations
                    .retain(|_, r| r.provider != *provider || now - r.taken_at < max_age);
            }
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
        let now = self.clock.now();
        let mut state = self.lock_state();
        Self::reserve_locked(&mut state, provider, fraction, now)
    }

    fn reserve_locked(
        state: &mut State,
        provider: Provider,
        fraction: f32,
        now: DateTime<Utc>,
    ) -> u64 {
        state.next_reservation += 1;
        let id = state.next_reservation;
        state.reservations.insert(
            id,
            Reservation {
                provider,
                fraction,
                taken_at: now,
            },
        );
        id
    }

    /// A no-op for a reservation already released, aged out or expired.
    pub fn release(&self, id: u64) {
        self.lock_state().reservations.remove(&id);
    }

    /// Used fraction including in-flight reservations; `None` when unknown.
    pub fn effective_fraction(&self, provider: Provider) -> Option<f32> {
        self.lock_state().effective(provider)
    }

    /// Choose where the next unit of work runs, without reserving anything.
    pub fn decide(&self, policy: ProviderQuotaPolicy, pinned: Provider) -> Decision {
        let now = self.clock.now();
        let mut state = self.lock_state();
        self.decide_locked(&mut state, policy, pinned, 0.0, now)
    }

    /// Decide and, on dispatch, reserve `cost` under one lock, so a burst of
    /// spawns sees each other's reservations. Under either policy, work that
    /// would pass the cap (`hard_cap` balanced, `freeze_cap` pinned) is not
    /// dispatched. Returns the decision and the reservation id when one was
    /// taken.
    pub fn dispatch(
        &self,
        policy: ProviderQuotaPolicy,
        pinned: Provider,
        cost: f32,
    ) -> (Decision, Option<u64>) {
        let now = self.clock.now();
        let mut state = self.lock_state();
        let decision = self.decide_locked(&mut state, policy, pinned, cost, now);
        let id = match decision {
            Decision::Dispatch(provider) => {
                Some(Self::reserve_locked(&mut state, provider, cost, now))
            }
            _ => None,
        };
        (decision, id)
    }

    fn decide_locked(
        &self,
        state: &mut State,
        policy: ProviderQuotaPolicy,
        pinned: Provider,
        cost: f32,
        now: DateTime<Utc>,
    ) -> Decision {
        match policy {
            ProviderQuotaPolicy::PinnedFreeze => match state.dispatchable(pinned) {
                Some(fraction) if fits(fraction, cost, self.config.freeze_cap) => {
                    Decision::Dispatch(pinned)
                }
                _ => Decision::Freeze {
                    provider: pinned,
                    fraction: state.effective(pinned),
                },
            },
            ProviderQuotaPolicy::BalancedFailover => self.decide_balanced(state, cost, now),
        }
    }

    fn decide_balanced(&self, state: &mut State, cost: f32, now: DateTime<Utc>) -> Decision {
        let cfg = self.config;
        let mut headroom: Vec<(Provider, f32)> = Provider::ALL
            .into_iter()
            .filter_map(|p| state.dispatchable(p).map(|f| (p, f)))
            .filter(|(_, f)| fits(*f, cost, cfg.hard_cap))
            .map(|(p, f)| (p, cfg.hard_cap - f))
            .collect();
        if headroom.is_empty() {
            let resume_at = Provider::ALL
                .into_iter()
                .filter_map(|p| self.room_again_at(state, p, cost, now))
                .min();
            return Decision::PauseAll { resume_at };
        }
        // Sticky: leave the current provider only once it is at or past the
        // soft cap and another one with room is still below it, and then pick
        // among those. When every provider with room is past the soft cap,
        // stay put; moving between them would only alternate.
        let current = state
            .current
            .filter(|c| headroom.iter().any(|(p, _)| p == c));
        if let Some(current) = current {
            let below_soft = |p: Provider| state.effective(p).is_some_and(|f| f < cfg.soft_cap);
            if below_soft(current) || !headroom.iter().any(|(p, _)| below_soft(*p)) {
                return Decision::Dispatch(current);
            }
            headroom.retain(|(p, _)| below_soft(*p));
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

    /// The earliest time a provider capped for `cost` may fit it. Every capped
    /// window must have reset (so the latest of those resets governs) and
    /// enough reservations must have aged out or expired. Usage with no future reset
    /// (credit budget, local estimate, stale window) can next change at the
    /// next poll. `None` if waiting cannot help.
    fn room_again_at(
        &self,
        state: &State,
        provider: Provider,
        cost: f32,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        state.dispatchable(provider)?;
        let entry = state.providers.get(&provider)?;
        let interval = self.interval(provider);
        let next_poll = entry.last_polled.map_or(now, |at| at + interval).max(now);
        let usage: Vec<(f32, DateTime<Utc>)> = usage_sources(entry)
            .into_iter()
            .map(|(f, reset)| (f, reset.filter(|at| *at > now).unwrap_or(next_poll)))
            .collect();
        let reserved: Vec<(f32, DateTime<Utc>)> = state
            .reservations
            .values()
            .filter(|r| r.provider == provider)
            .map(|r| {
                // The first successful poll at that age, or the TTL if sooner.
                let aged_out = (r.taken_at + interval).max(next_poll);
                (
                    r.fraction,
                    aged_out.min(r.taken_at + self.reservation_ttl()),
                )
            })
            .collect();
        let mut times: Vec<DateTime<Utc>> =
            usage.iter().chain(&reserved).map(|(_, at)| *at).collect();
        times.sort();
        times.into_iter().find(|t| {
            let used = usage
                .iter()
                .filter(|(_, at)| at > t)
                .fold(0.0, |max, (f, _)| f32::max(max, *f));
            let inflight: f32 = reserved
                .iter()
                .filter(|(_, at)| at > t)
                .map(|(f, _)| f)
                .sum();
            fits(used + inflight, cost, self.config.hard_cap)
        })
    }

    /// One-line wake header, present only when a provider is above the soft cap.
    pub fn wake_header(&self) -> Option<String> {
        let state = self.lock_state();
        let parts: Vec<String> = Provider::ALL
            .into_iter()
            .filter_map(|p| {
                let f = state.effective(p)?;
                (f >= self.config.soft_cap).then(|| format!("{} {:.0}%", p.slug(), f * 100.0))
            })
            .collect();
        (!parts.is_empty()).then(|| {
            format!(
                "quota: {} (soft cap {:.0}%)",
                parts.join(", "),
                self.config.soft_cap * 100.0
            )
        })
    }

    /// Read-only `quota_status` payload, kept under 550 bytes.
    pub fn status_payload(&self) -> Value {
        let state = self.lock_state();
        let mut providers = serde_json::Map::new();
        for p in Provider::ALL {
            let entry = state.providers.get(&p);
            let used = entry.and_then(used_fraction);
            let mut obj = serde_json::Map::new();
            obj.insert(
                "used".into(),
                used.map_or(Value::Null, |f| json!((f * 100.0).round() / 100.0)),
            );
            obj.insert(
                "inflight".into(),
                json!((state.inflight(p) * 100.0).round() / 100.0),
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

    struct FakeProbe(
        Arc<Mutex<BTreeMap<Provider, Result<ProbeReading, ProbeError>>>>,
        Arc<Mutex<Vec<Provider>>>,
    );
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

    fn rig(
        fallback: Option<f32>,
    ) -> (
        QuotaBroker,
        Arc<MockClock>,
        Readings,
        Arc<Mutex<Vec<Provider>>>,
    ) {
        let clock = Arc::new(MockClock::new(Utc::now()));
        let readings: Readings = Arc::default();
        let calls = Arc::default();
        let broker = QuotaBroker::new(
            BrokerConfig::default(),
            Box::new(SharedClock(clock.clone())),
            Box::new(FakeProbe(readings.clone(), Arc::clone(&calls))),
            Box::new(FakeFallback(fallback)),
        );
        (broker.unwrap(), clock, readings, calls)
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
    fn leaked_reservation_ages_out_on_a_successful_poll() {
        let (b, clock, readings, _) = rig(None);
        set(&readings, 0.5, 0.5, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        let leaked = b.reserve(Provider::ClaudeCode, 0.25);
        clock.advance(Duration::from_secs(60));
        b.reserve(Provider::ClaudeCode, 0.125);
        clock.advance(Duration::from_secs(30));
        b.poll_due();
        // The 90 s old one is gone; the 30 s old one is not yet reflected.
        assert_eq!(b.effective_fraction(Provider::ClaudeCode), Some(0.625));
        b.release(leaked);
        assert_eq!(b.effective_fraction(Provider::ClaudeCode), Some(0.625));
        clock.advance(Duration::from_secs(90));
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::ClaudeCode), Some(0.5));
    }

    #[test]
    fn failed_poll_keeps_reservations() {
        let (b, clock, readings, _) = rig(Some(0.5));
        set(&readings, 0.5, 0.5, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        b.reserve(Provider::Codex, 0.25);
        readings.lock().unwrap().remove(&Provider::Codex);
        clock.advance(Duration::from_secs(180));
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::Codex), Some(0.75));
    }

    #[test]
    fn leaked_reservation_expires_during_a_probe_outage() {
        let (b, clock, readings, _) = rig(Some(0.5));
        b.set_orchestration_active(true);
        b.poll_due();
        b.reserve(Provider::Codex, 0.45);
        let pinned = || b.decide(ProviderQuotaPolicy::PinnedFreeze, Provider::Codex);
        clock.advance(Duration::from_secs(599));
        b.poll_due();
        assert!(matches!(pinned(), Decision::Freeze { .. }));
        // The TTL applies without another poll, even while inactive.
        b.set_orchestration_active(false);
        clock.advance(Duration::from_secs(1));
        assert_eq!(pinned(), Decision::Dispatch(Provider::Codex));
        assert_eq!(b.effective_fraction(Provider::Codex), Some(0.5));
        assert!(readings.lock().unwrap().is_empty(), "every probe failed");
    }

    #[test]
    fn resume_uses_the_reservation_ttl_when_it_comes_before_the_next_poll() {
        let (b, clock, readings, _) = rig(Some(0.0));
        let t = clock.now();
        let later = Some(t + chrono::Duration::minutes(60));
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, later)],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, later)],
            None,
        );
        b.set_orchestration_active(true);
        b.poll_due();
        b.reserve(Provider::Codex, 0.875);
        clock.advance(Duration::from_secs(540));
        b.poll_due();
        clock.advance(Duration::from_secs(50));
        // Next codex poll is at 630 s; the reservation expires at 600 s.
        assert_eq!(resume_at(&b, 0.0), Some(t + chrono::Duration::seconds(600)));
    }

    #[test]
    fn per_window_credit_budget_counts_towards_usage() {
        let (b, clock, readings, _) = rig(None);
        let t = clock.now();
        let at = |mins| Some(t + chrono::Duration::minutes(mins));
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, at(60))],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, at(60))],
            None,
        );
        let credit_capped = ScopedUsageWindow {
            credit_budget_fraction: Some(0.875),
            ..window(0.1, at(30))
        };
        put(&readings, Provider::Codex, vec![credit_capped], None);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::Codex), Some(0.875));
        assert_eq!(resume_at(&b, 0.0), at(30));
    }

    #[test]
    fn never_polled_providers_are_not_dispatched() {
        let (b, _c, _r, _) = rig(None);
        assert_eq!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::PauseAll { resume_at: None }
        );
        assert_eq!(
            b.dispatch(ProviderQuotaPolicy::PinnedFreeze, Provider::Codex, 0.0),
            (
                Decision::Freeze {
                    provider: Provider::Codex,
                    fraction: None
                },
                None
            )
        );
    }

    #[test]
    fn unknown_usage_is_never_read_as_zero() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.5, 0.0, 0.0);
        {
            let mut r = readings.lock().unwrap();
            // A failed probe with no local estimate, and an empty reading.
            r.insert(
                Provider::ClaudeCode,
                Err(ProbeError::Unavailable("down".into())),
            );
            r.insert(Provider::Codex, Ok(ProbeReading::default()));
        }
        b.set_orchestration_active(true);
        b.poll_due();
        for p in [Provider::ClaudeCode, Provider::Codex] {
            assert_eq!(b.effective_fraction(p), None);
            assert_eq!(
                b.decide(ProviderQuotaPolicy::PinnedFreeze, p),
                Decision::Freeze {
                    provider: p,
                    fraction: None
                }
            );
        }
        for _ in 0..5 {
            assert_eq!(
                b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
                Decision::Dispatch(Provider::Antigravity)
            );
        }
    }

    #[test]
    fn non_finite_reading_is_unknown() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, f32::NAN, f32::INFINITY, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.effective_fraction(Provider::Antigravity), None);
        assert_eq!(b.effective_fraction(Provider::ClaudeCode), None);
    }

    #[test]
    fn needs_sign_in_is_never_dispatched() {
        for fallback in [None, Some(0.1)] {
            let (b, _c, readings, _) = rig(fallback);
            set(&readings, 0.5, 0.5, 0.0);
            readings
                .lock()
                .unwrap()
                .insert(Provider::Codex, Err(ProbeError::NeedsSignIn));
            b.set_orchestration_active(true);
            b.poll_due();
            for _ in 0..20 {
                assert_ne!(
                    b.dispatch(ProviderQuotaPolicy::BalancedFailover, Provider::Codex, 0.0)
                        .0,
                    Decision::Dispatch(Provider::Codex)
                );
            }
            assert_eq!(
                b.dispatch(ProviderQuotaPolicy::PinnedFreeze, Provider::Codex, 0.0),
                (
                    Decision::Freeze {
                        provider: Provider::Codex,
                        fraction: fallback
                    },
                    None
                )
            );
        }
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

    /// Publish new readings and poll all three providers.
    fn repoll(b: &QuotaBroker, clock: &MockClock, readings: &Readings, used: [f32; 3]) {
        set(readings, used[0], used[1], used[2]);
        clock.advance(Duration::from_secs(180));
        b.poll_due();
    }

    fn balanced(b: &QuotaBroker) -> Decision {
        b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex)
    }

    #[test]
    fn balanced_fails_over_past_soft_cap_to_a_provider_below_it() {
        let (b, clock, readings, _) = rig(None);
        b.set_orchestration_active(true);
        // Pick claude over codex first, which leaves claude's round-robin
        // credit negative: weighting alone would now favour agy over it.
        repoll(&b, &clock, &readings, [0.90, 0.1, 0.1]);
        assert_eq!(balanced(&b), Decision::Dispatch(Provider::ClaudeCode));
        // Then only agy is eligible.
        repoll(&b, &clock, &readings, [0.1, 0.90, 0.90]);
        assert_eq!(balanced(&b), Decision::Dispatch(Provider::Antigravity));
        // Below the soft cap it is sticky.
        repoll(&b, &clock, &readings, [0.77, 0.50, 0.90]);
        assert_eq!(balanced(&b), Decision::Dispatch(Provider::Antigravity));
        // Past it, work moves to the provider below it.
        repoll(&b, &clock, &readings, [0.79, 0.50, 0.90]);
        assert_eq!(balanced(&b), Decision::Dispatch(Provider::ClaudeCode));
    }

    #[test]
    fn balanced_failover_skips_other_providers_past_soft_cap() {
        let (b, clock, readings, _) = rig(None);
        b.set_orchestration_active(true);
        repoll(&b, &clock, &readings, [0.1, 0.90, 0.90]);
        assert_eq!(balanced(&b), Decision::Dispatch(Provider::Antigravity));
        for _ in 0..20 {
            repoll(&b, &clock, &readings, [0.79, 0.84, 0.5]);
            b.state.lock().unwrap().current = Some(Provider::Antigravity);
            assert_eq!(balanced(&b), Decision::Dispatch(Provider::Codex));
        }
    }

    #[test]
    fn balanced_stays_put_when_every_provider_with_room_is_past_soft_cap() {
        for claude in [0.84, 0.849, 0.78] {
            let (b, clock, readings, _) = rig(None);
            b.set_orchestration_active(true);
            repoll(&b, &clock, &readings, [0.1, 0.90, 0.90]);
            assert_eq!(balanced(&b), Decision::Dispatch(Provider::Antigravity));
            repoll(&b, &clock, &readings, [0.79, claude, 0.90]);
            for _ in 0..50 {
                assert_eq!(balanced(&b), Decision::Dispatch(Provider::Antigravity));
            }
            // It moves, and stays, once the current provider has no room.
            repoll(&b, &clock, &readings, [0.86, claude, 0.90]);
            assert_eq!(balanced(&b), Decision::Dispatch(Provider::ClaudeCode));
            repoll(&b, &clock, &readings, [0.79, claude, 0.90]);
            for _ in 0..50 {
                assert_eq!(balanced(&b), Decision::Dispatch(Provider::ClaudeCode));
            }
        }
    }

    #[test]
    fn config_requires_soft_below_hard_below_freeze() {
        assert!(BrokerConfig::default().validate().is_ok());
        for (soft_cap, hard_cap, freeze_cap) in [
            (0.85, 0.85, 0.90),
            (0.86, 0.85, 0.90),
            (0.78, 0.90, 0.90),
            (0.78, 0.95, 0.90),
            (0.0, 0.85, 0.90),
            (0.78, 0.85, 1.5),
            (f32::NAN, 0.85, 0.90),
        ] {
            let config = BrokerConfig {
                soft_cap,
                hard_cap,
                freeze_cap,
                ..Default::default()
            };
            assert!(config.validate().is_err(), "{config:?}");
            let built = QuotaBroker::new(
                config,
                Box::new(SystemClock),
                Box::new(FakeProbe(Arc::default(), Arc::default())),
                Box::new(FakeFallback(None)),
            );
            assert!(built.is_err(), "{config:?}");
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
            if let Decision::Dispatch(p) =
                b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex)
            {
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
        for (p, mins) in [
            (Provider::Antigravity, 50),
            (Provider::ClaudeCode, 20),
            (Provider::Codex, 90),
        ] {
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

    fn window(used: f32, resets_at: Option<DateTime<Utc>>) -> ScopedUsageWindow {
        ScopedUsageWindow {
            used_fraction: used,
            resets_at,
            ..Default::default()
        }
    }

    fn put(readings: &Readings, p: Provider, windows: Vec<ScopedUsageWindow>, credit: Option<f32>) {
        readings.lock().unwrap().insert(
            p,
            Ok(ProbeReading {
                windows,
                credit_budget_fraction: credit,
            }),
        );
    }

    fn resume_at(b: &QuotaBroker, cost: f32) -> Option<DateTime<Utc>> {
        match b.dispatch(ProviderQuotaPolicy::BalancedFailover, Provider::Codex, cost) {
            (Decision::PauseAll { resume_at }, None) => resume_at,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resume_waits_for_every_capped_window_of_a_provider() {
        let (b, clock, readings, _) = rig(None);
        let t = clock.now();
        let at = |mins| Some(t + chrono::Duration::minutes(mins));
        // agy: both capped, so 300 governs, not 10. claude: only the capped
        // window matters. codex: room at 200.
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, at(10)), window(0.9, at(300))],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, at(120)), window(0.2, at(900))],
            None,
        );
        put(&readings, Provider::Codex, vec![window(0.9, at(200))], None);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(resume_at(&b, 0.0), at(120));
    }

    #[test]
    fn resume_counts_a_window_capped_only_with_inflight() {
        let (b, clock, readings, _) = rig(None);
        let t = clock.now();
        let at = |mins| Some(t + chrono::Duration::minutes(mins));
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, at(60))],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, at(60))],
            None,
        );
        put(&readings, Provider::Codex, vec![window(0.5, at(1))], None);
        b.set_orchestration_active(true);
        b.poll_due();
        b.reserve(Provider::Codex, 0.5);
        // The reservation ages out at 90 s, but the window is what makes room
        // first, and the reservation still fits under the cap once it resets.
        assert_eq!(resume_at(&b, 0.0), at(1));
    }

    #[test]
    fn resume_counts_reservations_ageing_out() {
        let (b, clock, readings, _) = rig(None);
        let t = clock.now();
        let later = Some(t + chrono::Duration::minutes(60));
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, later)],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, later)],
            None,
        );
        put(&readings, Provider::Codex, vec![window(0.0, later)], None);
        b.set_orchestration_active(true);
        b.poll_due();
        b.reserve(Provider::Codex, 0.875);
        assert_eq!(resume_at(&b, 0.0), Some(t + chrono::Duration::seconds(90)));
    }

    #[test]
    fn resume_rechecks_credit_and_local_estimates_at_the_next_poll() {
        let (b, clock, readings, _) = rig(Some(0.9));
        let t = clock.now();
        let later = Some(t + chrono::Duration::minutes(60));
        let next_poll = Some(t + chrono::Duration::seconds(90));
        put(
            &readings,
            Provider::Antigravity,
            vec![window(0.9, later)],
            None,
        );
        put(
            &readings,
            Provider::ClaudeCode,
            vec![window(0.9, later)],
            None,
        );
        // Codex has no reading, so the 0.9 local estimate caps it.
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(resume_at(&b, 0.0), next_poll);

        // Same for a capped credit budget; its window reset does not clear it.
        put(
            &readings,
            Provider::Codex,
            vec![window(0.1, later)],
            Some(0.9),
        );
        clock.advance(Duration::from_secs(180));
        b.poll_due();
        assert_eq!(resume_at(&b, 0.0), Some(t + chrono::Duration::seconds(270)));
    }

    #[test]
    fn resume_is_none_when_waiting_cannot_help() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.5, 0.5, 0.5);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(resume_at(&b, 0.9), None);
    }

    #[test]
    fn pinned_dispatch_counts_cost_against_the_freeze_cap() {
        let (b, _c, readings, _) = rig(None);
        set(&readings, 0.0, 0.75, 0.0);
        b.set_orchestration_active(true);
        b.poll_due();
        let pinned = || {
            b.dispatch(
                ProviderQuotaPolicy::PinnedFreeze,
                Provider::ClaudeCode,
                0.125,
            )
        };
        assert!(matches!(
            pinned(),
            (Decision::Dispatch(Provider::ClaudeCode), Some(_))
        ));
        assert_eq!(
            pinned(),
            (
                Decision::Freeze {
                    provider: Provider::ClaudeCode,
                    fraction: Some(0.875)
                },
                None
            )
        );
        assert_eq!(b.effective_fraction(Provider::ClaudeCode), Some(0.875));
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
            Decision::Freeze {
                provider: Provider::ClaudeCode,
                ..
            }
        ));
    }

    #[test]
    fn wake_header_only_above_soft_cap_and_payload_is_small() {
        let (b, clock, readings, _) = rig(None);
        set(&readings, 0.2, 0.3, 0.4);
        b.set_orchestration_active(true);
        b.poll_due();
        assert_eq!(b.wake_header(), None);
        set(&readings, 0.2, 0.3, 0.8);
        clock.advance(Duration::from_secs(180));
        b.poll_due();
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
        // 0.125 is exact in f32, so the sums do not drift.
        let burst = || {
            b.dispatch(
                ProviderQuotaPolicy::BalancedFailover,
                Provider::Codex,
                0.125,
            )
        };
        let dispatched = (0..50).filter(|_| burst().1.is_some()).count();
        // Each provider has 0.35 of room: two 0.125 spawns each, none more.
        assert_eq!(dispatched, 6);
        for p in Provider::ALL {
            assert_eq!(b.effective_fraction(p), Some(0.75));
        }
        assert!(matches!(
            b.decide(ProviderQuotaPolicy::BalancedFailover, Provider::Codex),
            Decision::Dispatch(_)
        ));
        assert!(matches!(burst().0, Decision::PauseAll { .. }));
    }

    #[test]
    fn concurrent_dispatch_never_overshoots_the_hard_cap() {
        for _ in 0..50 {
            let (b, _c, readings, _) = rig(None);
            set(&readings, 0.5, 0.5, 0.5);
            b.set_orchestration_active(true);
            b.poll_due();
            let start = std::sync::Barrier::new(8);
            let dispatched: usize = std::thread::scope(|scope| {
                let threads: Vec<_> = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            start.wait();
                            (0..20)
                                .filter(|_| {
                                    b.dispatch(
                                        ProviderQuotaPolicy::BalancedFailover,
                                        Provider::Codex,
                                        0.03125,
                                    )
                                    .1
                                    .is_some()
                                })
                                .count()
                        })
                    })
                    .collect();
                threads.into_iter().map(|t| t.join().unwrap()).sum()
            });
            for p in Provider::ALL {
                let fraction = b.effective_fraction(p).unwrap();
                assert!(fraction <= b.config.hard_cap, "{p:?} {fraction}");
            }
            // 11 spawns of 1/32 fit in each provider's 0.35 of room.
            assert_eq!(dispatched, 33);
        }
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
                    assert_eq!(fraction, Some(0.9));
                }
                other => panic!("{other:?}"),
            }
        }
        // No failover side effect: the balanced cursor is untouched.
        assert_eq!(b.state.lock().unwrap().current, None);
    }
}
