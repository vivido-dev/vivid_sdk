//! Object exhaustion with rendered roots, helpers, and independent native owners.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_sdk::gui_wayland::MAX_PROTOCOL_OBJECT_SLOTS;

pub(super) fn exceed_object_limit(state: &Fixture, qh: &QueueHandle<Fixture>) {
    for _ in 0..MAX_PROTOCOL_OBJECT_SLOTS {
        state.compositor.as_ref().unwrap().create_region(qh, ());
    }
}

#[test]
fn helper_object_exhaustion_preserves_the_root_and_an_independent_rendered_owner() {
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
    assert!(
        helper
            .apply(
                &mut host,
                shm_client::Operation::Regions(MAX_PROTOCOL_OBJECT_SLOTS)
            )
            .is_err()
    );
    assert!(!host.root_failed());
    key(&mut host, initial.target, 100);
    assert_eq!(pixel(&next(&mut host), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
}

#[test]
fn a_root_exceeding_object_admission_fails_without_altering_the_peer() {
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
    key(&mut host, initial.target, 132);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = host.dispatch().unwrap();
        if host.root_failed() && bad.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "object exhaustion did not retire its root"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.is_mapped());
    assert!(host.input_target().is_none());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}
