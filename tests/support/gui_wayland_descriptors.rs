//! Ancillary descriptor admission and exact-connection reclamation.

// Rust guideline compliant 2026-10-07
use super::*;
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
use std::{io::Read as _, mem::MaybeUninit, os::fd::BorrowedFd};

fn send_descriptors(socket: &UnixStream, descriptor: BorrowedFd<'_>, count: usize, id: u32) {
    let bytes = [1u32, 12u32 << 16, id]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    let fds = vec![descriptor; count];
    let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(64))];
    let mut ancillary = SendAncillaryBuffer::new(&mut storage);
    assert!(ancillary.push(SendAncillaryMessage::ScmRights(&fds)));
    assert_eq!(
        sendmsg(
            socket,
            &[std::io::IoSlice::new(&bytes)],
            &mut ancillary,
            SendFlags::NOSIGNAL
        )
        .unwrap(),
        bytes.len()
    );
}

fn drain(socket: &mut UnixStream) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let mut bytes = [0; 4096];
        match socket.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => result.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("descriptor fixture read: {error}"),
        }
    }
    result
}

fn assert_no_memory(bytes: &[u8]) {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        assert!(
            size >= 8 && size <= bytes.len() - offset,
            "invalid fixture response framing"
        );
        if object == 1 && header & 0xffff == 0 {
            assert!(size >= 16, "truncated core error");
            assert_eq!(
                u32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()),
                2
            );
            return;
        }
        offset += size;
    }
    panic!("native descriptor rejection did not send core no-memory");
}

fn assert_reclaimed(host: &mut WaylandHost, probe: &mut UnixStream, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match probe.read(&mut [0]) {
            Ok(count) => {
                assert_eq!(count, 0, "{label} received unexpected data");
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "{label} leaked descriptors");
                let _ = host.dispatch().unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{label} reclamation: {error}"),
        }
    }
}

pub(super) fn exceed_root_limit(connection: &Connection, qh: &QueueHandle<Fixture>) -> ! {
    let callback = connection.display().sync(qh, ());
    let id = callback.id().protocol_id();
    connection.flush().unwrap();
    let mut socket = UnixStream::from(connection.backend().poll_fd().try_clone_to_owned().unwrap());
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (_probe, writer) = UnixStream::pair().unwrap();
    for count in [28, 28, 8, 1] {
        send_descriptors(&socket, writer.as_fd(), count, id);
    }
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).unwrap();
    assert_no_memory(&bytes);
    // The normal client queue cannot resume after bypassing its object map.
    std::process::exit(0)
}

#[test]
fn oversized_and_truncated_ancillary_batches_close_every_delivered_descriptor() {
    for count in [29, 64] {
        let mut host = WaylandHost::new(16, 16).unwrap();
        let mut helper = UnixStream::connect(host.socket_path()).unwrap();
        helper.set_nonblocking(true).unwrap();
        let (mut probe, writer) = UnixStream::pair().unwrap();
        probe.set_nonblocking(true).unwrap();
        send_descriptors(&helper, writer.as_fd(), count, 2);
        drop(writer);
        let _ = host.dispatch().unwrap();
        assert_no_memory(&drain(&mut helper));
        assert_reclaimed(&mut host, &mut probe, "oversized ancillary batch");
        assert!(!host.root_failed());
    }
}

#[test]
fn root_descriptor_rejection_is_protocol_failure_even_after_a_successful_child_exit() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let mut child = client(&host);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let _peer = client(&peer);
    let other = next(&mut peer);
    peer.set_active(true);
    present(&mut peer, &other);
    key(&mut host, frame.target, 138);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = host.dispatch().unwrap();
        if host.root_failed()
            && let Some(status) = child.0.try_wait().unwrap()
        {
            assert!(
                status.success(),
                "descriptor fixture did not exit cleanly: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root descriptor quota was not observed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!host.is_mapped());
    assert!(host.input_target().is_none());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}

#[test]
fn unused_descriptors_are_bounded_across_receives_and_reclaimed_without_harming_rendered_owners() {
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
    let mut helper = UnixStream::connect(host.socket_path()).unwrap();
    helper.set_nonblocking(true).unwrap();
    let (mut probe, writer) = UnixStream::pair().unwrap();
    probe.set_nonblocking(true).unwrap();
    for count in [28, 28, 8] {
        send_descriptors(&helper, writer.as_fd(), count, 2);
        let _ = host.dispatch().unwrap();
        assert!(
            !drain(&mut helper).is_empty(),
            "legal descriptor batch did not complete core sync"
        );
    }
    drop(writer);
    assert_eq!(
        probe.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let (mut extra, writer) = UnixStream::pair().unwrap();
    extra.set_nonblocking(true).unwrap();
    send_descriptors(&helper, writer.as_fd(), 1, 2);
    drop(writer);
    let _ = host.dispatch().unwrap();
    assert_no_memory(&drain(&mut helper));
    assert_reclaimed(&mut host, &mut probe, "retained incoming descriptors");
    assert_reclaimed(&mut host, &mut extra, "rejected ancillary descriptors");
    assert!(!host.root_failed());
    assert_eq!(host.input_target().unwrap().0, initial.target);
    key(&mut host, initial.target, 100);
    assert_eq!(pixel(&next(&mut host), 0, 0), [1, 10, 20, 255]);
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}
