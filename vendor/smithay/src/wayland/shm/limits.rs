//! Shared SHM charges retained until backing storage and buffer metadata retire.

// Rust guideline compliant 2026-10-07
use std::{
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

/// Validated limits shared by every client of one SHM global.
#[derive(Clone, Copy, Debug)]
pub struct ShmLimits {
    pool_bytes: usize,
    total_bytes: usize,
    pools: usize,
    buffers: usize,
}

impl ShmLimits {
    /// Validates pool size, total mapped bytes, pool count, and buffer count.
    ///
    /// # Errors
    /// Returns an error for zero limits or a pool ceiling above the total ceiling.
    pub fn new(pool_bytes: usize, total_bytes: usize, pools: usize, buffers: usize) -> io::Result<Self> {
        if pool_bytes == 0 || total_bytes < pool_bytes || pools == 0 || buffers == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid SHM limits"));
        }
        Ok(Self {
            pool_bytes,
            total_bytes,
            pools,
            buffers,
        })
    }
}

#[derive(Debug, Default)]
struct Usage {
    bytes: usize,
    pools: usize,
    buffers: usize,
}

#[derive(Debug)]
pub(super) struct Quota {
    limits: ShmLimits,
    usage: Mutex<Usage>,
}

impl Quota {
    pub(super) fn new(limits: ShmLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            usage: Mutex::default(),
        })
    }

    pub(super) fn pool(self: &Arc<Self>, bytes: usize) -> Option<PoolPermit> {
        if bytes == 0 || bytes > self.limits.pool_bytes {
            return None;
        }
        let mut usage = self.usage.lock().expect("SHM quota poisoned");
        let total = usage
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.limits.total_bytes)?;
        let pools = usage
            .pools
            .checked_add(1)
            .filter(|count| *count <= self.limits.pools)?;
        usage.bytes = total;
        usage.pools = pools;
        Some(PoolPermit {
            quota: self.clone(),
            bytes: AtomicUsize::new(bytes),
        })
    }

    pub(super) fn buffer(self: &Arc<Self>) -> Option<BufferPermit> {
        let mut usage = self.usage.lock().expect("SHM quota poisoned");
        usage.buffers = usage
            .buffers
            .checked_add(1)
            .filter(|count| *count <= self.limits.buffers)?;
        Some(BufferPermit(self.clone()))
    }
}

#[derive(Debug)]
pub(super) struct PoolPermit {
    quota: Arc<Quota>,
    bytes: AtomicUsize,
}

impl PoolPermit {
    // The pool's mapping write lock serializes growth and keeps its permit alive.
    pub(super) fn grow(&self, bytes: usize) -> Option<Growth<'_>> {
        if bytes > self.quota.limits.pool_bytes {
            return None;
        }
        let extra = bytes.checked_sub(self.bytes.load(Ordering::Relaxed))?;
        let mut usage = self.quota.usage.lock().expect("SHM quota poisoned");
        usage.bytes = usage
            .bytes
            .checked_add(extra)
            .filter(|total| *total <= self.quota.limits.total_bytes)?;
        Some(Growth {
            pool: self,
            bytes,
            extra,
            committed: false,
        })
    }
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        let mut usage = self.quota.usage.lock().expect("SHM quota poisoned");
        usage.bytes = usage
            .bytes
            .checked_sub(*self.bytes.get_mut())
            .expect("SHM byte charge lost");
        usage.pools = usage.pools.checked_sub(1).expect("SHM pool charge lost");
    }
}

pub(super) struct Growth<'a> {
    pool: &'a PoolPermit,
    bytes: usize,
    extra: usize,
    committed: bool,
}

impl Growth<'_> {
    pub(super) fn commit(mut self) {
        self.pool.bytes.store(self.bytes, Ordering::Relaxed);
        self.committed = true;
    }
}

impl Drop for Growth<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let mut usage = self.pool.quota.usage.lock().expect("SHM quota poisoned");
            usage.bytes = usage
                .bytes
                .checked_sub(self.extra)
                .expect("SHM growth charge lost");
        }
    }
}

#[derive(Debug)]
pub(super) struct BufferPermit(Arc<Quota>);

impl Drop for BufferPermit {
    fn drop(&mut self) {
        let mut usage = self.0.usage.lock().expect("SHM quota poisoned");
        usage.buffers = usage.buffers.checked_sub(1).expect("SHM buffer charge lost");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_rollback_growth_and_keep_independent_owners_isolated() {
        let limits = ShmLimits::new(16, 24, 2, 2).unwrap();
        let owner = Quota::new(limits);
        let peer = Quota::new(limits);
        let first = owner.pool(8).unwrap();
        let second = owner.pool(16).unwrap();
        let unrelated = peer.pool(16).unwrap();
        assert!(owner.pool(1).is_none());
        assert!(first.grow(9).is_none());
        drop(second);
        drop(first.grow(16).unwrap()); // Failed remap rolls its reservation back.
        assert_eq!(owner.usage.lock().unwrap().bytes, 8);
        first.grow(16).unwrap().commit();
        assert_eq!(owner.usage.lock().unwrap().bytes, 16);
        assert!(first.grow(17).is_none());
        let buffer = owner.buffer().unwrap();
        let another = owner.buffer().unwrap();
        assert!(owner.buffer().is_none());
        drop(buffer);
        drop(owner.buffer().unwrap());
        drop(another);
        drop(first);
        assert_eq!(owner.usage.lock().unwrap().bytes, 0);
        assert_eq!(owner.usage.lock().unwrap().pools, 0);
        assert_eq!(owner.usage.lock().unwrap().buffers, 0);
        assert_eq!(peer.usage.lock().unwrap().bytes, 16);
        drop(unrelated);
    }

    #[test]
    fn checked_totals_reject_overflow_without_changing_live_charges() {
        let quota = Quota::new(ShmLimits::new(usize::MAX, usize::MAX, 2, 1).unwrap());
        let pool = quota.pool(usize::MAX).unwrap();
        assert!(quota.pool(1).is_none());
        assert_eq!(quota.usage.lock().unwrap().bytes, usize::MAX);
        drop(pool);
        assert_eq!(quota.usage.lock().unwrap().bytes, 0);
    }
}
