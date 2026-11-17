//! Capped exponential backoff with jitter: the one way to retry.
//!
//! Post-mortem 2026-11-16 (P4): fixed-interval retry loops turned a permanent
//! failure (a deleted worktree) into an unbounded background `git` storm.
//! Retry loops use [`Backoff`] (or [`delay_for_attempt`] when the attempt count
//! lives elsewhere). "Not found" and "bad params" are not transient: callers
//! pick a larger ceiling for them.

use std::time::Duration;

/// Fraction of a delay (in permille) that jitter may remove. Jitter only
/// shortens, so a delay never exceeds its ceiling.
const JITTER_PERMILLE: u64 = 200;

/// Deterministic delay before retry `attempt` (0-based): `base` doubling to
/// `ceiling`. A `ceiling` below `base` is treated as `base`.
pub fn delay_for_attempt(base: Duration, ceiling: Duration, attempt: u32) -> Duration {
    let ceiling = ceiling.max(base);
    // 2^20 already exceeds any realistic base/ceiling ratio.
    let factor = 1u32 << attempt.min(20);
    base.checked_mul(factor).unwrap_or(ceiling).min(ceiling)
}

/// Shorten `delay` by a random 0..20% so retries from many watches spread out.
pub fn jittered(delay: Duration) -> Duration {
    jittered_with(delay, random_permille())
}

/// [`jittered`] with an explicit random value in 0..=1000 (testable).
pub fn jittered_with(delay: Duration, random_permille: u64) -> Duration {
    let cut = JITTER_PERMILLE * random_permille.min(1000) / 1000;
    delay.mul_f64((1000 - cut) as f64 / 1000.0)
}

fn random_permille() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    // `RandomState` is seeded per process and per instance: free randomness
    // without a `rand` dependency.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as u64),
    );
    hasher.finish() % 1001
}

/// Stateful backoff for a retry loop: call [`next_delay`](Self::next_delay)
/// after each failure and [`reset`](Self::reset) after real progress (a frame
/// received, not merely a connection opened).
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    ceiling: Duration,
    attempt: u32,
}

impl Backoff {
    pub const fn new(base: Duration, ceiling: Duration) -> Self {
        let ceiling = if ceiling.as_nanos() < base.as_nanos() { base } else { ceiling };
        Self { base, ceiling, attempt: 0 }
    }

    /// Delay for the current attempt, with jitter; advances the attempt.
    pub fn next_delay(&mut self) -> Duration {
        jittered(self.next_delay_exact())
    }

    /// Delay for the current attempt, no jitter; advances the attempt.
    pub fn next_delay_exact(&mut self) -> Duration {
        let delay = delay_for_attempt(self.base, self.ceiling, self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Failures since the last reset.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn doubles_to_ceiling() {
        let got: Vec<u64> = (0..8).map(|a| delay_for_attempt(S(2), S(30), a).as_secs()).collect();
        assert_eq!(got, [2, 4, 8, 16, 30, 30, 30, 30]);
        assert_eq!(delay_for_attempt(S(2), S(30), u32::MAX), S(30));
    }

    #[test]
    fn ceiling_below_base_is_base() {
        assert_eq!(delay_for_attempt(S(5), S(1), 3), S(5));
        let mut b = Backoff::new(S(5), S(1));
        assert_eq!(b.next_delay_exact(), S(5));
        assert_eq!(b.next_delay_exact(), S(5));
    }

    #[test]
    fn jitter_only_shortens_within_twenty_percent() {
        assert_eq!(jittered_with(S(10), 0), S(10));
        assert_eq!(jittered_with(S(10), 1000), S(8));
        assert_eq!(jittered_with(S(10), 5000), S(8));
        for _ in 0..200 {
            let j = jittered(S(10));
            assert!(j >= S(8) && j <= S(10), "{j:?}");
        }
    }

    #[test]
    fn stateful_backoff_resets() {
        let mut b = Backoff::new(S(2), S(300));
        let first: Vec<u64> = (0..9).map(|_| b.next_delay_exact().as_secs()).collect();
        assert_eq!(first, [2, 4, 8, 16, 32, 64, 128, 256, 300]);
        assert_eq!(b.attempt(), 9);
        b.reset();
        assert_eq!(b.next_delay_exact(), S(2));
        assert!(b.next_delay() <= S(4));
    }
}
