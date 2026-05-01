//! Token-bucket rate limiter for webhook intake.
//!
//! Defends against the T7 (resource exhaustion via lots of small
//! PRs) attacker scenario described in `docs/THREAT-MODEL.md`. The
//! threat model previously noted T7 as "operator concern, out of
//! scope for v1"; this module closes it with sensible in-process
//! defaults that operators can tune via `AR_WEBHOOK_RATE_PER_SEC`
//! and `AR_WEBHOOK_BURST`.
//!
//! Bucket semantics: capacity = `burst`; refill = `rate_per_sec`
//! tokens per second; each request consumes one token. When the
//! bucket is empty, [`TokenBucket::try_take`] returns false and the
//! webhook handler should reply 429.
//!
//! Granularity: global, not per-source. A bot fronting a single
//! Forgejo instance has one legitimate traffic source; a shared
//! bucket is enough. A multi-tenant bot would want per-source
//! limits but that's out of scope for v1 (single-tenant
//! deployments per the architecture decision).

use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    state: Mutex<BucketState>,
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refilled: Instant,
}

impl TokenBucket {
    /// `burst` = bucket capacity; `rate_per_sec` = tokens added per
    /// second. The bucket starts full, so a freshly-constructed
    /// limiter accepts up to `burst` requests immediately before
    /// throttling kicks in.
    pub fn new(burst: u32, rate_per_sec: u32) -> Self {
        let capacity = burst.max(1) as f64;
        Self {
            capacity,
            refill_per_sec: rate_per_sec.max(1) as f64,
            state: Mutex::new(BucketState {
                tokens: capacity,
                last_refilled: Instant::now(),
            }),
        }
    }

    /// Attempt to consume one token. Returns true on success
    /// (request allowed), false on empty bucket (request denied —
    /// caller should reply 429).
    pub fn try_take(&self) -> bool {
        self.try_take_at(Instant::now())
    }

    /// Like [`try_take`] but with an injectable `now` for tests.
    pub fn try_take_at(&self, now: Instant) -> bool {
        let mut state = self.state.lock().expect("bucket lock");
        let elapsed = now
            .saturating_duration_since(state.last_refilled)
            .as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        state.last_refilled = now;
        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fresh_bucket_accepts_burst_consecutive_requests() {
        let bucket = TokenBucket::new(5, 1);
        for i in 0..5 {
            assert!(bucket.try_take(), "expected take {i}");
        }
        // Bucket is now empty — sixth take fails.
        assert!(!bucket.try_take());
    }

    #[test]
    fn refill_replenishes_at_configured_rate() {
        let bucket = TokenBucket::new(2, 10);
        let t0 = Instant::now();
        // Drain the burst budget.
        assert!(bucket.try_take_at(t0));
        assert!(bucket.try_take_at(t0));
        assert!(!bucket.try_take_at(t0));
        // 100ms later, 1 token has refilled (10/s × 0.1).
        let t1 = t0 + Duration::from_millis(100);
        assert!(bucket.try_take_at(t1));
        assert!(!bucket.try_take_at(t1));
    }

    #[test]
    fn refill_caps_at_capacity() {
        let bucket = TokenBucket::new(3, 100);
        let t0 = Instant::now();
        assert!(bucket.try_take_at(t0));
        // Bucket has 2 tokens; wait long enough that linear refill
        // would say 200 tokens; cap clamps to 3.
        let t1 = t0 + Duration::from_secs(2);
        assert!(bucket.try_take_at(t1));
        assert!(bucket.try_take_at(t1));
        assert!(bucket.try_take_at(t1));
        // Now empty — no overflow above capacity.
        assert!(!bucket.try_take_at(t1));
    }

    #[test]
    fn zero_args_clamp_to_one() {
        // Defensive: zero values shouldn't divide-by-zero or
        // permanently lock out traffic. Both clamp to 1.
        let bucket = TokenBucket::new(0, 0);
        assert!(bucket.try_take());
    }

    #[test]
    fn time_going_backwards_does_not_underflow() {
        let bucket = TokenBucket::new(2, 1);
        let t0 = Instant::now();
        assert!(bucket.try_take_at(t0));
        // Earlier time. saturating_duration_since prevents
        // underflow; bucket should not refill backwards.
        let t_earlier = t0;
        assert!(bucket.try_take_at(t_earlier));
        // Now empty.
        assert!(!bucket.try_take_at(t_earlier));
    }
}
