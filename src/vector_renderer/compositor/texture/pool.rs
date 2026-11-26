//! Allocation charges retained by jobs, immutable outputs and imported image clones.

// Rust guideline compliant 2026-10-07
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

// One displayed export and one replacement may remain live. A pending job
// reserves its eventual export before allocating a working target.
const MAX_OUTPUTS: usize = 2;

/// A shared two-output budget surviving renderer replacement.
///
/// Reuse the same pool for every replacement of a pane's texture renderer.
/// Jobs reserve credit before rendering; outputs and imported image clones keep
/// it until their last reference and GPU completion retire.
#[derive(Clone, Debug, Default)]
pub struct TexturePool(Arc<State>);

#[derive(Debug, Default)]
struct State {
    outputs: AtomicUsize,
    worker: AtomicBool,
}

impl TexturePool {
    /// Returns charged jobs, exports and imported images, including retiring GPU work.
    pub fn reserved(&self) -> usize {
        self.0.outputs.load(Ordering::Acquire)
    }

    pub(super) fn reserve(&self) -> io::Result<Lease> {
        self.0
            .outputs
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < MAX_OUTPUTS {
                    n.checked_add(1)
                } else {
                    None
                }
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "native texture output pool is full",
                )
            })?;
        Ok(Lease(self.clone()))
    }

    pub(super) fn claim_worker(&self) -> io::Result<WorkerLease> {
        self.0
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "native texture worker is still live or retiring",
                )
            })?;
        Ok(WorkerLease(self.clone()))
    }
}

pub(super) struct Lease(TexturePool);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0
            .0
            .outputs
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .expect("native texture allocation charge underflow");
    }
}

pub(super) struct WorkerLease(TexturePool);
impl Drop for WorkerLease {
    fn drop(&mut self) {
        self.0
            .0
            .worker
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .expect("native texture worker charge missing");
    }
}
