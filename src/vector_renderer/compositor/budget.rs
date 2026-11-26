//! Shared dispatch admission without changing the checked per-job limit.

// Rust guideline compliant 2026-10-07
use std::{
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Bounds total dispatch attempts across related checked rendering jobs.
///
/// Clones share one monotonic counter. Each native dispatch claims one credit
/// before submission; cancelled/failed attempts never replenish it. Cache hits
/// consume no credit. Exhaustion returns [`super::Error::Cancelled`], not source
/// work refusal. Every job also retains its independent 64-attempt ceiling.
#[derive(Clone, Debug)]
pub struct WorkBudget {
    limit: u64,
    remaining: Arc<AtomicU64>,
}
impl WorkBudget {
    /// Creates a positive shared dispatch budget.
    pub fn new(limit: NonZeroU64) -> Self {
        Self {
            limit: limit.get(),
            remaining: Arc::new(AtomicU64::new(limit.get())),
        }
    }
    /// Returns attempts admitted across every clone, including in-flight work.
    pub fn spent(&self) -> u64 {
        self.limit - self.remaining.load(Ordering::Acquire)
    }
    /// Returns unused credits without reserving them.
    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Acquire)
    }
    pub(super) fn claim(&self) -> bool {
        self.remaining
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_sub(1)
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_clones_cannot_overspend_or_replenish_dispatch_credits() {
        let budget = WorkBudget::new(NonZeroU64::new(127).unwrap());
        let workers = (0..4)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || {
                    let mut spent = 0;
                    while budget.claim() {
                        spent += 1;
                    }
                    spent
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<u64>(),
            127
        );
        assert_eq!(budget.spent(), 127);
        assert_eq!(budget.remaining(), 0);
        assert!(!budget.clone().claim());
    }
}
