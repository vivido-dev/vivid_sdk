//! Admission before uncommitted regions, damage, callbacks and cached states grow.

// Rust guideline compliant 2026-10-07
use super::*;
use shm_client::Operation;
use vivid_sdk::gui_wayland::{
    MAX_CACHED_SURFACE_STATES, MAX_DAMAGE_RECTANGLES, MAX_PENDING_CALLBACKS, MAX_REGION_OPERATIONS,
};

pub(super) fn exceed_root_limit(state: &Fixture, code: u32, qh: &QueueHandle<Fixture>) {
    let root = state.root.as_ref().unwrap();
    match code {
        134 => {
            for _ in 0..=MAX_DAMAGE_RECTANGLES {
                root.damage_buffer(0, 0, 1, 1);
            }
        }
        135 => {
            let region = state.compositor.as_ref().unwrap().create_region(qh, ());
            for _ in 0..=MAX_REGION_OPERATIONS {
                region.add(0, 0, 1, 1);
            }
        }
        136 => {
            for _ in 0..=MAX_PENDING_CALLBACKS {
                root.frame(qh, ());
            }
        }
        137 => {
            for _ in 0..=MAX_CACHED_SURFACE_STATES {
                state.child.as_ref().unwrap().surface.commit();
            }
        }
        _ => unreachable!("unknown pending quota fixture command"),
    }
}

fn helper_limit(first: &[Operation], last: Operation) {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _root = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let _peer = client(&peer);
    let other = next(&mut peer);
    peer.set_active(true);
    present(&mut peer, &other);
    let helper = shm_client::Peer::new(&host);
    for operation in first {
        helper.apply(&mut host, *operation).unwrap();
    }
    assert_eq!(
        helper.apply(&mut host, last),
        Err(shm_client::Failure::Protocol(2, "wl_display".into()))
    );
    assert!(!host.root_failed());
    assert_eq!(host.input_target().unwrap().0, initial.target);
    key(&mut host, initial.target, 100);
    assert_eq!(pixel(&next(&mut host), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
}

#[test]
fn pending_region_operations_count_add_and_subtract_before_any_surface_commit() {
    helper_limit(
        &[Operation::RegionOperations(64, false)],
        Operation::RegionOperations(1, true),
    );
    helper_limit(&[], Operation::RegionOperations(65, true));
}

#[test]
fn pending_damage_combines_surface_and_buffer_coordinates_before_commit() {
    helper_limit(
        &[Operation::Damage(128, false), Operation::Damage(128, true)],
        Operation::Damage(1, false),
    );
}

#[test]
fn pending_damage_and_callbacks_include_synchronized_cached_state() {
    helper_limit(
        &[
            Operation::CachedCommits(0, false),
            Operation::Damage(128, false),
            Operation::CommitSurface,
            Operation::Damage(128, true),
        ],
        Operation::Damage(1, true),
    );
    helper_limit(
        &[
            Operation::CachedCommits(0, false),
            Operation::Frames(32),
            Operation::CommitSurface,
            Operation::Frames(32),
        ],
        Operation::Frames(1),
    );
}

#[test]
fn pending_callbacks_are_bounded_before_surface_commit() {
    helper_limit(&[Operation::Frames(64)], Operation::Frames(1));
}

#[test]
fn pending_synchronized_snapshots_are_bounded_and_parent_application_reclaims_them() {
    helper_limit(
        &[
            Operation::RegionOperations(64, false),
            Operation::CachedCommits(0, false),
            Operation::AssignRegion,
            Operation::CachedCommits(64, true),
            Operation::CachedCommits(64, true),
        ],
        Operation::CachedCommits(65, false),
    );
}

#[test]
fn root_pending_quotas_retire_only_the_rejected_rendered_owner() {
    for code in 134..=137 {
        let mut host = WaylandHost::new(64, 48).unwrap();
        let mut bad = client(&host);
        let initial = next(&mut host);
        host.set_active(true);
        present(&mut host, &initial);
        let mut peer = WaylandHost::new(64, 48).unwrap();
        let _peer = client(&peer);
        let other = next(&mut peer);
        peer.set_active(true);
        present(&mut peer, &other);
        key(&mut host, initial.target, code);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = host.dispatch().unwrap();
            if host.root_failed() && bad.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "root pending quota {code} did not retire its owner"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!host.is_mapped());
        assert!(host.input_target().is_none());
        key(&mut peer, other.target, 100);
        assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
        assert!(!peer.root_failed());
    }
}
