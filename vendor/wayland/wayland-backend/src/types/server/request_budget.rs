//! Monotonic request admission shared by selected Rust-backend clients.

// Rust guideline compliant 2026-10-07
use std::{num::NonZeroU32, sync::Mutex, time::Instant};

/// Burst and sustained ceilings for incoming native requests.
///
/// Request counts and declared wire bytes use independent token buckets.
/// Each bucket starts full, refills with monotonic time, and never exceeds its
/// burst ceiling. Wire sizes are charged once before payload parsing, including
/// a request that subsequently waits for more bytes or descriptors.
#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    /// Maximum requests admitted in an immediate burst.
    pub request_burst: NonZeroU32,
    /// Request tokens replenished each second.
    pub requests_per_second: NonZeroU32,
    /// Maximum declared wire bytes admitted in an immediate burst.
    pub byte_burst: NonZeroU32,
    /// Declared wire-byte tokens replenished each second.
    pub bytes_per_second: NonZeroU32,
}

/// A shared request allowance for a group of native clients.
///
/// Hosts can retain this across client connections so reconnecting cannot reset
/// a group's allowance. The Rust server evaluates the client-data policy for
/// every newly admitted request; switching a client's group does not reset that
/// group's allowance. Admission never waits for tokens and rejects atomically
/// if either the request or byte bucket is exhausted.
#[derive(Debug)]
pub struct RequestBudget(Mutex<State>);

#[derive(Debug)]
struct State {
    requests: Bucket,
    bytes: Bucket,
}
#[derive(Debug)]
struct Bucket {
    credit: u128,
    capacity: u128,
    rate: u128,
    updated: Instant,
}
const UNITS_PER_TOKEN: u128 = 1_000_000_000;

impl Bucket {
    fn new(burst: NonZeroU32, rate: NonZeroU32, now: Instant) -> Self {
        let capacity = u128::from(burst.get()) * UNITS_PER_TOKEN;
        Self { credit: capacity, capacity, rate: u128::from(rate.get()), updated: now }
    }
    fn refill(&mut self, now: Instant) {
        let elapsed = now.checked_duration_since(self.updated).unwrap_or_default();
        self.updated = self.updated.max(now);
        let refill = elapsed.as_nanos().saturating_mul(self.rate);
        self.credit = self.credit.saturating_add(refill).min(self.capacity);
    }
}

impl RequestBudget {
    /// Creates a full allowance with independent request and wire-byte buckets.
    pub fn new(limits: RequestLimits) -> Self {
        let now = Instant::now();
        Self(Mutex::new(State {
            requests: Bucket::new(limits.request_burst, limits.requests_per_second, now),
            bytes: Bucket::new(limits.byte_burst, limits.bytes_per_second, now),
        }))
    }
    pub(crate) fn admit(&self, bytes: usize) -> bool {
        self.admit_at(bytes, Instant::now())
    }
    fn admit_at(&self, bytes: usize, now: Instant) -> bool {
        let cost = (bytes as u128).saturating_mul(UNITS_PER_TOKEN);
        let mut state = self.0.lock().unwrap();
        state.requests.refill(now);
        state.bytes.refill(now);
        if state.requests.credit < UNITS_PER_TOKEN || state.bytes.credit < cost {
            return false;
        }
        state.requests.credit -= UNITS_PER_TOKEN;
        state.bytes.credit -= cost;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn limits(requests: u32, bytes: u32) -> RequestLimits {
        RequestLimits {
            request_burst: NonZeroU32::new(requests).unwrap(),
            requests_per_second: NonZeroU32::new(requests).unwrap(),
            byte_burst: NonZeroU32::new(bytes).unwrap(),
            bytes_per_second: NonZeroU32::new(bytes).unwrap(),
        }
    }
    #[test]
    fn fractional_refill_is_retained_and_idle_time_cannot_expand_the_burst() {
        let budget = RequestBudget::new(limits(2, 100));
        let start = budget.0.lock().unwrap().requests.updated;
        assert!(budget.admit_at(0, start));
        assert!(budget.admit_at(0, start));
        assert!(!budget.admit_at(0, start + Duration::from_millis(250)));
        assert!(budget.admit_at(0, start + Duration::from_millis(500)));
        assert!(!budget.admit_at(0, start + Duration::from_millis(500)));
        let later = start + Duration::from_secs(3600);
        assert!(budget.admit_at(0, later));
        assert!(budget.admit_at(0, later));
        assert!(!budget.admit_at(0, later));
    }
    #[test]
    fn byte_denial_does_not_spend_request_credit() {
        let budget = RequestBudget::new(limits(1, 1));
        let now = budget.0.lock().unwrap().requests.updated;
        assert!(!budget.admit_at(2, now));
        assert!(budget.admit_at(1, now));
        assert!(!budget.admit_at(1, now));
    }
    #[test]
    fn backwards_samples_cannot_mint_refill_credit() {
        let budget = RequestBudget::new(limits(1, 100));
        let start = budget.0.lock().unwrap().requests.updated;
        assert!(budget.admit_at(0, start + Duration::from_secs(2)));
        assert!(!budget.admit_at(0, start));
        assert!(!budget.admit_at(0, start + Duration::from_millis(2500)));
        assert!(budget.admit_at(0, start + Duration::from_secs(3)));
    }
}
