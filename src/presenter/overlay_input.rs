//! Monotonic overlay input lease expiry, independent of media and GUI cadence.

// Rust guideline compliant 2026-10-07
use super::*;

pub(super) fn maintain(state: &mut State) {
    for owner in state.overlays.expired_lanes() {
        desktop_input::revoke(state, owner, 8, true);
    }
}
