//! Coalesced GUI-thread activation requests with exact instance lifetimes.

// Rust guideline compliant 2026-10-07
use super::InstanceId;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    fmt,
    rc::{Rc, Weak},
};

#[derive(Debug)]
struct Scope {
    live: Cell<bool>,
    visible: Cell<bool>,
}

/// Requests host-controlled activation without moving native keyboard focus directly.
///
/// Hosts drain requests according to their own window/modal policy. Requests
/// coalesce by full instance identity in latest-request order. Hidden, closed
/// and retired hosts refuse requests; this handle grants no Vivid authority.
#[derive(Clone)]
pub struct ActivationRequest {
    id: InstanceId,
    scope: Rc<Scope>,
    pending: Weak<RefCell<VecDeque<InstanceId>>>,
    limit: usize,
}
impl ActivationRequest {
    /// Queues one bounded request, returning false when its owner is unavailable.
    pub fn request(&self) -> bool {
        if !self.scope.live.get() || !self.scope.visible.get() {
            return false;
        }
        let Some(pending) = self.pending.upgrade() else {
            return false;
        };
        let mut pending = pending.borrow_mut();
        pending.retain(|id| *id != self.id);
        if pending.len() >= self.limit {
            return false;
        }
        pending.push_back(self.id);
        true
    }
}
impl fmt::Debug for ActivationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActivationRequest")
            .field("owner", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(super) struct Requests {
    pending: Rc<RefCell<VecDeque<InstanceId>>>,
    scopes: BTreeMap<InstanceId, Rc<Scope>>,
    limit: usize,
}
impl Requests {
    pub fn new(limit: usize) -> Self {
        Self {
            pending: Rc::new(RefCell::new(VecDeque::new())),
            scopes: BTreeMap::new(),
            limit,
        }
    }
    pub fn bind(&mut self, id: InstanceId) -> ActivationRequest {
        let scope = Rc::new(Scope {
            live: Cell::new(true),
            visible: Cell::new(true),
        });
        self.scopes.insert(id, scope.clone());
        ActivationRequest {
            id,
            scope,
            pending: Rc::downgrade(&self.pending),
            limit: self.limit,
        }
    }
    pub fn visible(&self, id: InstanceId, visible: bool) {
        if let Some(scope) = self.scopes.get(&id) {
            scope.visible.set(visible);
        }
        if !visible {
            self.pending.borrow_mut().retain(|pending| *pending != id);
        }
    }
    pub fn remove(&mut self, id: InstanceId) {
        if let Some(scope) = self.scopes.remove(&id) {
            scope.live.set(false);
        }
        self.pending.borrow_mut().retain(|pending| *pending != id);
    }
    pub fn take(&self) -> Vec<InstanceId> {
        self.pending.borrow_mut().drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_coalesces_in_order_and_rejects_hidden_retired_and_foreign_lifetimes() {
        let mut a = Requests::new(2);
        let mut peer = Requests::new(2);
        let first = InstanceId {
            host: [1; 16],
            serial: 1,
        };
        let other = InstanceId {
            host: [2; 16],
            serial: 1,
        };
        let next = InstanceId {
            host: [1; 16],
            serial: 2,
        };
        let old = a.bind(first);
        let later = a.bind(next);
        let survivor = peer.bind(other);
        assert!(old.request() && later.request() && old.request());
        assert_eq!(a.take(), vec![next, first]);
        assert!(old.request());
        a.visible(first, false);
        assert!(!old.request());
        assert!(a.take().is_empty());
        a.visible(first, true);
        assert!(old.request());
        a.remove(first);
        assert!(!old.request());
        assert!(a.take().is_empty());
        assert!(survivor.request());
        assert_eq!(peer.take(), vec![other]);
        drop(a);
        assert!(!later.request());
        assert!(survivor.request());
    }
}
