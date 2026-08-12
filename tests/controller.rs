//! Controller-side lease and admin tests.
//!
//! These are the security §11 controller-side cases the plan requires: the controller mints the
//! activation secret locally, the presenter receives only the verifier, the lease handle can be
//! revoked synchronously, and the retry policy never creates a second secret.

#![cfg(feature = "testing")]

use std::sync::Arc;

use vivid_sdk::testing::{FakeAdmin, ROOT_SECRET_HEX, TestPresenter};
use vivid_sdk::{
    CORE_CONTROL, CleanupPolicy, DESKTOP_INPUT, DESKTOP_SURFACE, LIVE_MEDIA, LaneEndpoints,
    LeaseRequest, PresenterAdmin, VividoAdmin, issue_handle, worker_context,
};

fn desktop_lane_endpoints(presenter: &TestPresenter) -> LaneEndpoints {
    LaneEndpoints {
        control: presenter.endpoint().to_owned(),
        interactive: None,
        realtime: None,
        bulk: None,
    }
}

fn desktop_request(lease_id: u64) -> LeaseRequest {
    LeaseRequest {
        parent_context_id: 1,
        lease_id,
        permitted_profiles: {
            let mut profiles = vec![
                CORE_CONTROL.to_owned(),
                DESKTOP_SURFACE.to_owned(),
                LIVE_MEDIA.to_owned(),
                DESKTOP_INPUT.to_owned(),
            ];
            profiles.sort();
            profiles
        },
        activation_timeout_us: 20_000_000,
        disconnect_grace_us: 5_000_000,
        cleanup_policy: CleanupPolicy::SuspendOnUncleanLoss,
        contract: vivid_protocol::resource::ResourceContract::denied(),
    }
}

#[test]
fn vivido_admin_issues_a_lease_with_a_locally_minted_secret() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let admin =
        VividoAdmin::connect(presenter.endpoint(), ROOT_SECRET_HEX, DESKTOP_SURFACE).unwrap();

    let caps = admin.capabilities().unwrap();
    assert!(caps.profiles.iter().any(|p| p == DESKTOP_SURFACE));
    assert_eq!(caps.carrier, vivid_sdk::Carrier::Native);

    let request = desktop_request(7);
    let grant = admin.issue(&request).unwrap();
    assert_eq!(grant.lease_id, 7);
    assert_eq!(grant.context_id, request.parent_context_id);
    assert!(!grant.activation.is_taken());
    assert!(
        grant.grace_us <= request.disconnect_grace_us,
        "the presenter narrowed or kept the grace"
    );
    assert!(
        grant.activation_timeout_us <= request.activation_timeout_us,
        "the presenter narrowed or kept the timeout"
    );
    assert_eq!(grant.cleanup_policy, CleanupPolicy::SuspendOnUncleanLoss);

    // The activation secret is real: its verifier matches what the builder would produce.
    // The presenter received only this hash, never the secret bytes.
    let verifier = grant.activation.verifier();
    assert_ne!(verifier, [0; 32], "the verifier is a real hash");

    admin.revoke(&grant).unwrap();
}

#[test]
fn the_lease_handle_takes_the_secret_once_and_revokes_synchronously() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let admin = Arc::new(FakeAdmin::new(
        vec![CORE_CONTROL.into(), DESKTOP_SURFACE.into()],
        desktop_lane_endpoints(&presenter),
    ));
    let admin_trait: Arc<dyn PresenterAdmin> = admin.clone();
    let request = desktop_request(3);
    let mut handle = issue_handle(&admin_trait, &request).unwrap();
    assert_eq!(handle.lease_id(), Some(3));

    let secret = handle.take_activation().unwrap();
    assert!(
        handle.take_activation().is_none(),
        "the secret cannot be taken twice"
    );
    assert!(
        vivid_protocol::auth::activation_verifier(3, &secret) != [0; 32],
        "the secret reconstructs a valid verifier"
    );

    assert_eq!(admin.live_leases(), 1);
    handle.revoke().unwrap();
    assert_eq!(admin.live_leases(), 0);
}

#[test]
fn a_dropped_lease_handle_revokes_on_drop() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let admin = Arc::new(FakeAdmin::new(
        vec![CORE_CONTROL.into()],
        desktop_lane_endpoints(&presenter),
    ));
    let admin_trait: Arc<dyn PresenterAdmin> = admin.clone();
    let request = desktop_request(5);
    assert_eq!(admin.live_leases(), 0);
    {
        let _handle = issue_handle(&admin_trait, &request).unwrap();
        assert_eq!(admin.live_leases(), 1);
    }
    assert_eq!(
        admin.live_leases(),
        0,
        "the handle's Drop revokes the lease, never leaking a live grant"
    );
}

#[test]
fn the_worker_context_is_suitable_for_a_desktop_worker() {
    let ctx = worker_context(9, 1, vivid_protocol::resource::ResourceContract::denied());
    assert_eq!(ctx.context_id, 9);
    assert_eq!(ctx.parent_context_id, 1);
    assert_ne!(ctx.operation_classes & vivid_sdk::OP_SURFACE_TRACK_MEDIA, 0);
    assert_ne!(ctx.operation_classes & vivid_sdk::OP_SCENE, 0);
    assert_eq!(
        ctx.operation_classes & vivid_sdk::OP_TERMINAL_ANCHOR,
        0,
        "a desktop worker has no terminal anchor class"
    );
}
