//! Owner-scoped receipt and deferred transition budgets through socket completion.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

// Vivid 1.5 bounds pending outcomes plus unresolved submissions to 256 per owner.
const MAX_RECEIPTS: usize = 256;
// Preserve the protocol input queue bound after events leave the window manager.
const MAX_TRANSITIONS: usize = vivid_protocol::overlay::MAX_PENDING_EVENTS;

pub(crate) struct Permit(Arc<AtomicUsize>);
impl Permit {
    fn acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        counter
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|next| *next <= limit)
            })
            .ok()?;
        Some(Self(Arc::clone(counter)))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Default)]
pub(super) struct Owner {
    receipts: Arc<AtomicUsize>,
    transitions: Arc<AtomicUsize>,
    pub(super) unresolved: HashMap<Submission, Permit>,
    pub revoked: bool,
}
impl Owner {
    pub fn reserve(&mut self, submission: Submission) -> bool {
        if self.revoked || self.unresolved.contains_key(&submission) {
            return false;
        }
        let Some(permit) = Permit::acquire(&self.receipts, MAX_RECEIPTS) else {
            return false;
        };
        self.unresolved.insert(submission, permit);
        true
    }
    pub fn resolve(&mut self, submission: Submission) -> Option<Permit> {
        self.unresolved.remove(&submission)
    }
    pub fn record(&self, outcome: bool) -> Option<Permit> {
        if self.revoked {
            return None;
        }
        if outcome {
            Permit::acquire(&self.receipts, MAX_RECEIPTS)
        } else {
            Permit::acquire(&self.transitions, MAX_TRANSITIONS)
        }
    }
    fn revoke(&mut self) {
        self.revoked = true;
        self.unresolved.clear();
    }
}
impl Overlays {
    pub(super) fn revoke_backlog(&mut self, session: u64) {
        let backlog = self.backlogs.entry(session).or_default();
        if backlog.revoked {
            return;
        }
        backlog.revoke();
        self.lane_overflows.insert(session);
        self.pending.remove(&session);
        // Receipt waits terminate with lane loss, without invented outcomes.
        self.forget_presentations(session);
        #[cfg(feature = "vector-render")]
        if let Some(native) = &mut self.native {
            native.remove_owner(session);
        }
        self.close_lane(session);
    }
    pub(crate) fn take_lane_overflow(&mut self, session: u64) -> bool {
        self.lane_overflows.remove(&session)
    }
    pub(crate) fn overflowed_lanes(&self) -> Vec<u64> {
        self.lane_overflows.iter().copied().collect()
    }
}
