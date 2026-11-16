//! When to try again after the connection to a server was lost.
//!
//! The delays grow so a server that is down (or that is rejecting us) is not hammered, and each one
//! is spread out a little at random so many clients that lost the same server together do not all
//! come back at the same instant. Pure logic: no timers here, the front end sleeps.

use std::time::Duration;

/// Delay before the first retry; each following one doubles up to [`MAX_DELAY`].
pub const FIRST_DELAY: Duration = Duration::from_secs(5);
pub const MAX_DELAY: Duration = Duration::from_secs(120);
/// How many retries in a row before giving up (about 22 minutes of trying with the defaults).
pub const MAX_ATTEMPTS: u32 = 15;

#[derive(Clone, Debug)]
pub struct Backoff {
    attempts: u32,
    max_attempts: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { attempts: 0, max_attempts: MAX_ATTEMPTS }
    }
}

impl Backoff {
    pub fn new(max_attempts: u32) -> Backoff {
        Backoff { attempts: 0, max_attempts }
    }

    /// The (unjittered) delay for retry number `attempt` (1-based): 5 s, 10 s, 20 s ... capped.
    pub fn base_delay(attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        FIRST_DELAY.saturating_mul(1u32 << shift).min(MAX_DELAY)
    }

    /// Count a retry and return how long to wait before it, or `None` once the limit is reached.
    /// `random` is a number in `0.0..1.0` (the caller supplies it, which keeps this testable).
    pub fn next_delay(&mut self, random: f64) -> Option<Duration> {
        if self.attempts >= self.max_attempts {
            return None;
        }
        self.attempts += 1;
        Some(jitter(Self::base_delay(self.attempts), random))
    }

    /// The connection worked: start counting from scratch next time.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Retries made so far in this run of failures.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
}

/// Spread `base` by up to +/-20 % according to `random` in `0.0..1.0` (0.5 leaves it unchanged).
pub fn jitter(base: Duration, random: f64) -> Duration {
    let r = if random.is_finite() { random.clamp(0.0, 1.0) } else { 0.5 };
    base.mul_f64(0.8 + 0.4 * r)
}

/// A cheap random number in `0.0..1.0` from the system's per-process hasher keys (no extra crate;
/// this only spreads retries out and is not used for anything secret).
pub fn random_unit() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0));
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_and_stop_growing_at_the_cap() {
        let secs: Vec<u64> = (1..=9).map(|a| Backoff::base_delay(a).as_secs()).collect();
        assert_eq!(secs, [5, 10, 20, 40, 80, 120, 120, 120, 120]);
        assert_eq!(Backoff::base_delay(0), FIRST_DELAY, "attempt 0 is treated like the first");
        assert_eq!(Backoff::base_delay(u32::MAX), MAX_DELAY, "no overflow however many attempts");
    }

    #[test]
    fn it_gives_up_after_the_limit_and_resets_when_the_connection_works() {
        let mut b = Backoff::new(3);
        assert!(b.next_delay(0.5).is_some());
        assert!(b.next_delay(0.5).is_some());
        assert!(b.next_delay(0.5).is_some());
        assert_eq!(b.attempts(), 3);
        assert!(b.next_delay(0.5).is_none(), "the fourth try is refused");
        assert!(b.next_delay(0.5).is_none(), "and it stays refused");
        b.reset();
        assert_eq!(b.attempts(), 0);
        assert_eq!(b.next_delay(0.5), Some(FIRST_DELAY), "after a success the delays start over");
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let base = Duration::from_secs(100);
        assert_eq!(jitter(base, 0.5), base);
        assert_eq!(jitter(base, 0.0), Duration::from_secs(80));
        assert_eq!(jitter(base, 1.0), Duration::from_secs(120));
        for r in [f64::NAN, f64::INFINITY, -5.0, 7.0] {
            let d = jitter(base, r);
            assert!(d >= Duration::from_secs(80) && d <= Duration::from_secs(120), "{r} -> {d:?}");
        }
        // the very first retry never comes sooner than 4 s or later than 6 s
        let mut b = Backoff::default();
        let d = b.next_delay(random_unit()).unwrap();
        assert!(d >= Duration::from_secs(4) && d <= Duration::from_secs(6), "{d:?}");
    }

    #[test]
    fn random_unit_is_in_range_and_varies() {
        let xs: Vec<f64> = (0..50).map(|_| random_unit()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        assert!(xs.iter().any(|x| (x - xs[0]).abs() > 1e-9), "all 50 values were identical");
    }

    #[test]
    fn the_default_gives_up_after_about_twenty_two_minutes() {
        let mut b = Backoff::default();
        let total: u64 = std::iter::from_fn(|| b.next_delay(0.5)).map(|d| d.as_secs()).sum();
        assert_eq!(b.attempts(), MAX_ATTEMPTS);
        assert_eq!(total, 5 + 10 + 20 + 40 + 80 + 10 * 120, "total wait {total}s");
    }
}
