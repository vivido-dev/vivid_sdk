//! Request bursts across separate dispatch turns and rendered-owner isolation.

// Rust guideline compliant 2026-10-07
use super::*;
use std::io::Read as _;

fn sync_batch(id: u32, count: usize) -> Vec<u8> {
    [1u32, 12u32 << 16, id]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .cycle()
        .take(count * 12)
        .collect()
}

fn core_error(bytes: &[u8]) -> Option<u32> {
    let mut offset = 0;
    while bytes.len() - offset >= 8 {
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        assert!(size >= 8, "invalid rate fixture response size");
        if size > bytes.len() - offset {
            return None;
        }
        if u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) == 1
            && header & 0xffff == 0
        {
            assert!(size >= 16);
            return Some(u32::from_ne_bytes(
                bytes[offset + 12..offset + 16].try_into().unwrap(),
            ));
        }
        offset += size;
    }
    None
}

pub(super) fn exceed_root_limit(connection: &Connection, qh: &QueueHandle<Fixture>) -> ! {
    let callback = connection.display().sync(qh, ());
    let id = callback.id().protocol_id();
    connection.flush().unwrap();
    let mut socket = UnixStream::from(connection.backend().poll_fd().try_clone_to_owned().unwrap());
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let batch = sync_batch(id, 1024);
    let mut replies = Vec::new();
    for _ in 0..64 {
        let written = socket.write_all(&batch);
        let mut closed = false;
        loop {
            let mut bytes = [0; 32 * 1024];
            match socket.read(&mut bytes) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(count) => replies.extend_from_slice(&bytes[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                    closed = true;
                    break;
                }
                Err(error) => panic!("root rate response: {error}"),
            }
        }
        if let Some(code) = core_error(&replies) {
            assert_eq!(code, 2);
            std::process::exit(0);
        }
        if closed {
            // Unread requests can reset the transport during retirement. The
            // parent additionally requires the exact root's protocol-failure flag.
            std::process::exit(0);
        }
        written.expect("root rate fixture closed without the expected protocol error");
    }
    panic!("root short bursts escaped rate admission");
}

#[test]
fn root_short_bursts_fail_even_when_the_rejected_process_exits_successfully() {
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
    key(&mut host, frame.target, 139);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = host.dispatch().unwrap() {
            present(&mut host, &frame);
        }
        if host.root_failed()
            && let Some(status) = child.0.try_wait().unwrap()
        {
            assert!(
                status.success(),
                "rate-rejected fixture did not exit cleanly: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root rate rejection was not recorded"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!host.is_mapped());
    assert!(host.input_target().is_none());
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}

#[test]
fn separate_core_request_bursts_cannot_bypass_admission_or_retire_another_owner() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _root = client(&host);
    let frame = next(&mut host);
    host.set_active(true);
    present(&mut host, &frame);
    let mut peer = WaylandHost::new(64, 48).unwrap();
    let _peer = client(&peer);
    let other = next(&mut peer);
    peer.set_active(true);
    present(&mut peer, &other);
    let mut helper = UnixStream::connect(host.socket_path()).unwrap();
    helper.set_nonblocking(true).unwrap();
    let batch = sync_batch(2, 1024);
    let mut replies = Vec::new();
    for _ in 0..64 {
        helper.write_all(&batch).unwrap();
        let start = Instant::now();
        if let Some(frame) = host.dispatch().unwrap() {
            present(&mut host, &frame);
        }
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "fixture consumed a watchdog quantum"
        );
        loop {
            let mut bytes = [0; 32 * 1024];
            match helper.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => replies.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("rate fixture read: {error}"),
            }
        }
        if core_error(&replies).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        core_error(&replies),
        Some(2),
        "short bursts escaped the native request budget"
    );
    assert!(!host.root_failed());
    assert_eq!(host.input_target().unwrap().0, frame.target);
    key(&mut host, frame.target, 100);
    assert_eq!(pixel(&next(&mut host), 0, 0), [1, 10, 20, 255]);
    key(&mut peer, other.target, 100);
    assert_eq!(pixel(&next(&mut peer), 0, 0), [1, 10, 20, 255]);
    assert!(!peer.root_failed());
}
