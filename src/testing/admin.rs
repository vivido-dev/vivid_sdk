//! A pure in-memory [`PresenterAdmin`] for controller tests.
//!
//! It mints real activation secrets and tracks live grants, so a test exercises the
//! [`LeaseHandle`](crate::LeaseHandle) lifecycle — take, revoke, drop-revoke — without a socket. It
//! is also the shape [`BridgeAdmin`](crate::BridgeAdmin) will take: the worker consumes a
//! [`LeaseGrant`](crate::LeaseGrant) and cannot tell whether a Vivido window or a gateway issued it.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use vivid_protocol::lease::CleanupPolicy;
use vivid_protocol::resource::ResourceContract;

use crate::controller::{
    ActivationSecret, Carrier, LaneEndpoints, LeaseGrant, LeaseRequest, PresenterAdmin,
    PresenterCapabilities,
};

#[derive(Default)]
struct State {
    grants: HashMap<u64, bool>, // lease_id -> still live
    profiles: Vec<String>,
}

/// An in-memory presenter admin for tests.
pub struct FakeAdmin {
    state: Arc<Mutex<State>>,
    endpoints: LaneEndpoints,
    carrier: Carrier,
}

impl FakeAdmin {
    /// A fake admin that presents the given profiles over a native carrier.
    pub fn new(profiles: Vec<String>, endpoints: LaneEndpoints) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                grants: HashMap::new(),
                profiles,
            })),
            endpoints,
            carrier: Carrier::Native,
        }
    }

    /// How many leases are currently live.
    pub fn live_leases(&self) -> usize {
        self.state
            .lock()
            .expect("fake admin")
            .grants
            .values()
            .filter(|&&live| live)
            .count()
    }
}

impl PresenterAdmin for FakeAdmin {
    fn capabilities(&self) -> io::Result<PresenterCapabilities> {
        let state = self.state.lock().expect("fake admin");
        Ok(PresenterCapabilities {
            profiles: state.profiles.clone(),
            contract_ceiling: ResourceContract::denied(),
            carrier: self.carrier,
        })
    }

    fn issue(&self, request: &LeaseRequest) -> io::Result<LeaseGrant> {
        let mut state = self.state.lock().expect("fake admin");
        if state.grants.contains_key(&request.lease_id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a lease with that id is already live",
            ));
        }
        state.grants.insert(request.lease_id, true);
        let activation = ActivationSecret::new(request.lease_id)?;
        Ok(LeaseGrant {
            endpoints: self.endpoints.clone(),
            context_id: request.parent_context_id,
            lease_id: request.lease_id,
            activation,
            permitted_profiles: request.permitted_profiles.clone(),
            contract: request.contract.clone(),
            grace_us: request.disconnect_grace_us,
            activation_timeout_us: request.activation_timeout_us,
            cleanup_policy: request.cleanup_policy,
            revision: 1,
        })
    }

    fn revoke(&self, grant: &LeaseGrant) -> io::Result<()> {
        let mut state = self.state.lock().expect("fake admin");
        match state.grants.get_mut(&grant.lease_id) {
            Some(live) => {
                *live = false;
                Ok(())
            }
            None => Ok(()), // idempotent: revoking an unknown lease is a no-op
        }
    }
}

/// A [`CleanupPolicy`] sentinel for tests that want the suspend-on-unclean-loss policy without
/// constructing a full request.
pub fn suspending_policy() -> CleanupPolicy {
    CleanupPolicy::SuspendOnUncleanLoss
}
