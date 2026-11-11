//! Root failure reporting and independent rendered owners after SHM rejection.

// Rust guideline compliant 2026-10-07
use super::*;

pub(super) fn exceed_pool_limit(state: &Fixture, qh: &QueueHandle<Fixture>) {
    let bytes = vivid_sdk::gui_wayland::MAX_SHM_POOL_BYTES + 1;
    let file = tempfile::tempfile().unwrap();
    file.set_len(bytes as u64).unwrap();
    state
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), bytes as i32, qh, ());
}

#[test]
fn root_protocol_failure_is_retained_after_teardown_and_preserves_the_peer() {
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
    key(&mut host, initial.target, 131);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = host.dispatch().unwrap();
        if host.root_failed()
            && let Some(status) = bad.0.try_wait().unwrap()
        {
            assert!(
                status.success(),
                "rejected fixture should exit cleanly: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native SHM rejection was not recorded"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.is_mapped());
    assert!(host.input_target().is_none());
    assert!(!peer.root_failed());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    let _replacement = client(&host);
    let _new = next(&mut host);
    assert!(
        host.root_failed(),
        "a replacement role erased an observed failure"
    );
}

#[test]
fn helper_shm_rejection_cannot_mark_a_presented_root_or_its_peer_failed() {
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
                shm_client::Operation::Pool(vivid_sdk::gui_wayland::MAX_SHM_POOL_BYTES + 1),
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
