//! SHM admission and retained-storage limits through real native connections.
#![cfg(all(feature = "gui-wayland", target_os = "linux"))]

// Rust guideline compliant 2026-10-07
#[path = "support/gui_wayland_shm_client.rs"]
#[expect(
    dead_code,
    reason = "The region command is exercised by native object-limit checks"
)]
mod client;
use client::{Failure, Operation, Peer, Reply};
use vivid_sdk::gui_wayland::{
    MAX_SHM_BUFFERS, MAX_SHM_BYTES, MAX_SHM_POOL_BYTES, MAX_SHM_POOLS, WaylandHost,
};

fn no_memory(reply: Reply) {
    // libwayland maps wl_display.no_memory to ENOMEM rather than EPROTO.
    assert!(
        reply == Err(Failure::Protocol(2, "wl_display".into()))
            || reply == Err(Failure::Io(std::io::ErrorKind::OutOfMemory)),
        "expected an explicit native memory-limit error, got {reply:?}"
    );
}

#[test]
fn oversize_pool_and_growth_reject_only_the_offending_native_owner() {
    for grow in [false, true] {
        let mut host = WaylandHost::new(16, 16).unwrap();
        let client = Peer::new(&host);
        let mut other = WaylandHost::new(16, 16).unwrap();
        let peer = Peer::new(&other);
        peer.apply(&mut other, Operation::Pool(MAX_SHM_POOL_BYTES))
            .unwrap();
        if grow {
            client.apply(&mut host, Operation::Pool(4)).unwrap();
            no_memory(client.apply(
                &mut host,
                Operation::Resize(0, (MAX_SHM_POOL_BYTES + 1) as i32),
            ));
        } else {
            no_memory(client.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES + 1)));
        }
        peer.apply(&mut other, Operation::Buffer(0)).unwrap();
        peer.apply(&mut other, Operation::Resize(0, MAX_SHM_POOL_BYTES as i32))
            .unwrap();
        let replacement = Peer::new(&host);
        replacement
            .apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
            .unwrap();
    }
}

#[test]
fn total_storage_is_shared_by_helper_connections_and_reclaimed_after_disconnect() {
    let mut host = WaylandHost::new(16, 16).unwrap();
    let root = Peer::new(&host);
    let helper = Peer::new(&host);
    root.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
        .unwrap();
    helper
        .apply(
            &mut host,
            Operation::Pool(MAX_SHM_BYTES - MAX_SHM_POOL_BYTES),
        )
        .unwrap();
    no_memory(helper.apply(&mut host, Operation::Pool(1)));
    root.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
        .unwrap();
    root.apply(&mut host, Operation::Buffer(0)).unwrap();
}

#[test]
fn pool_destruction_does_not_release_storage_still_retained_by_buffers() {
    let mut host = WaylandHost::new(16, 16).unwrap();
    let client = Peer::new(&host);
    for index in 0..MAX_SHM_POOLS {
        client.apply(&mut host, Operation::Pool(4)).unwrap();
        client.apply(&mut host, Operation::Buffer(index)).unwrap();
        client.apply(&mut host, Operation::DropPool(index)).unwrap();
    }
    client.apply(&mut host, Operation::DropBuffer(0)).unwrap();
    client.apply(&mut host, Operation::Pool(4)).unwrap();
    no_memory(client.apply(&mut host, Operation::Pool(4)));
    let next = Peer::new(&host);
    next.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
        .unwrap();
}

#[test]
fn buffer_metadata_is_bounded_without_attach_and_destroyed_slots_are_reclaimed() {
    let mut host = WaylandHost::new(16, 16).unwrap();
    let client = Peer::new(&host);
    client.apply(&mut host, Operation::Pool(4)).unwrap();
    for _ in 0..MAX_SHM_BUFFERS {
        client.apply(&mut host, Operation::Buffer(0)).unwrap();
    }
    client.apply(&mut host, Operation::DropBuffer(0)).unwrap();
    client.apply(&mut host, Operation::Buffer(0)).unwrap();
    no_memory(client.apply(&mut host, Operation::Buffer(0)));
    let next = Peer::new(&host);
    next.apply(&mut host, Operation::Pool(4)).unwrap();
    next.apply(&mut host, Operation::Buffer(0)).unwrap();
}

#[test]
fn growth_checks_the_total_ceiling_before_remapping_and_preserves_another_connection() {
    let mut host = WaylandHost::new(16, 16).unwrap();
    let root = Peer::new(&host);
    let helper = Peer::new(&host);
    root.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
        .unwrap();
    root.apply(&mut host, Operation::Pool(4)).unwrap();
    helper
        .apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES - 4))
        .unwrap();
    no_memory(helper.apply(&mut host, Operation::Resize(0, MAX_SHM_POOL_BYTES as i32)));
    root.apply(&mut host, Operation::Buffer(0)).unwrap();
    root.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES - 4))
        .unwrap();
}

#[test]
fn malformed_resize_cannot_panic_and_failed_mappings_cannot_leak_capacity() {
    let mut host = WaylandHost::new(16, 16).unwrap();
    for size in [0, -1] {
        let bad = Peer::new(&host);
        bad.apply(&mut host, Operation::Pool(4)).unwrap();
        assert!(bad.apply(&mut host, Operation::Resize(0, size)).is_err());
    }
    for _ in 0..MAX_SHM_POOLS + 1 {
        let bad = Peer::new(&host);
        assert!(bad.apply(&mut host, Operation::BadPool).is_err());
    }
    let good = Peer::new(&host);
    good.apply(&mut host, Operation::Pool(MAX_SHM_POOL_BYTES))
        .unwrap();
    good.apply(&mut host, Operation::Buffer(0)).unwrap();
}
