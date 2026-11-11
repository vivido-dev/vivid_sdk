//! Receipt-time watchdogs and qualified input termination regressions.

// Rust guideline compliant 2026-10-07
use vivid_protocol::{
    input::INPUT_CLASS_KEYBOARD,
    revision::{GrantGeneration, SurfaceGeneration},
    time::Monotonic,
};
use vivid_sdk::{
    DesktopPreconditions, InputBindingGuard, InputBindingStatus, InputGrantTermination,
    InputLeaseRenewal,
};

fn requested() -> (InputBindingGuard, InputBindingStatus) {
    let mut guard = InputBindingGuard::new();
    guard.set_preconditions(DesktopPreconditions {
        surface_present: true,
        surface_generation: SurfaceGeneration::ONE,
        capability_mask: INPUT_CLASS_KEYBOARD,
        presented: true,
        lane_live: true,
    });
    guard
        .enable_for_context(
            1,
            2,
            SurfaceGeneration::ONE,
            INPUT_CLASS_KEYBOARD,
            1_000_000,
            6,
        )
        .unwrap();
    let status = InputBindingStatus {
        producer_epoch: 1,
        grant_generation: 7,
        context_id: 1,
        surface_id: 2,
        surface_generation: 1,
        effective_classes: INPUT_CLASS_KEYBOARD,
        state: 1,
        reason: 6,
        watchdog_timeout_us: 1_000_000,
    };
    (guard, status)
}

#[test]
fn initial_grant_expires_without_ever_receiving_a_renewal() {
    let (mut guard, status) = requested();
    guard
        .handle_bound_at(&status, Monotonic::from_micros(10))
        .unwrap();
    assert!(guard.is_armed(Monotonic::from_micros(1_000_009)));
    assert!(!guard.is_armed(Monotonic::from_micros(1_000_010)));
    let renewal = InputLeaseRenewal {
        binding: guard.current_tag().unwrap(),
        renewal_sequence: 1,
        watchdog_timeout_us: 1_000_000,
    };
    assert!(
        guard
            .handle_renewal(&renewal, Monotonic::from_micros(1_000_010))
            .is_err()
    );
}

#[test]
fn denied_binding_has_zero_effective_identity_and_cannot_rearm() {
    let (mut guard, status) = requested();
    guard.handle_bound_at(&status, Monotonic::ZERO).unwrap();
    let denied = InputBindingStatus {
        context_id: 0,
        surface_id: 0,
        surface_generation: 0,
        effective_classes: 0,
        watchdog_timeout_us: 0,
        state: 2,
        grant_generation: 8,
        ..status
    };
    guard
        .handle_bound_at(&denied, Monotonic::from_micros(10))
        .unwrap();
    assert!(guard.grant().is_none());
    for invalid in [
        InputBindingStatus {
            surface_id: 2,
            ..denied
        },
        InputBindingStatus {
            grant_generation: 0,
            ..denied
        },
    ] {
        assert!(guard.handle_bound_at(&invalid, Monotonic::ZERO).is_err());
    }
}

#[test]
fn newer_termination_releases_only_the_exact_owner_epoch_and_surface() {
    let (mut first, status) = requested();
    let (mut peer, _) = requested();
    first.handle_bound_at(&status, Monotonic::ZERO).unwrap();
    peer.handle_bound_at(&status, Monotonic::ZERO).unwrap();
    let tag = first.current_tag().unwrap();
    for foreign in [
        tag,
        vivid_sdk::InputTuple {
            context_id: 9,
            grant_generation: GrantGeneration::new(8),
            ..tag
        },
        vivid_sdk::InputTuple {
            surface_generation: SurfaceGeneration::new(2),
            grant_generation: GrantGeneration::new(8),
            ..tag
        },
    ] {
        assert!(
            first
                .handle_revocation(&InputGrantTermination {
                    binding: foreign,
                    reason: 1
                })
                .is_err()
        );
        assert!(first.is_armed(Monotonic::ZERO));
    }
    first
        .handle_revocation(&InputGrantTermination {
            binding: vivid_sdk::InputTuple {
                grant_generation: GrantGeneration::new(8),
                ..tag
            },
            reason: 1,
        })
        .unwrap();
    assert!(first.grant().is_none());
    assert!(peer.is_armed(Monotonic::ZERO));
    assert_eq!(peer.current_tag(), Some(tag));
}

#[test]
fn overflowing_initial_deadline_does_not_partially_install_authority() {
    let (mut guard, status) = requested();
    assert!(
        guard
            .handle_bound_at(&status, Monotonic::from_micros(u64::MAX))
            .is_err()
    );
    assert!(guard.grant().is_none());
}
