//! Dispatch liveness with real core-protocol traffic and independent native owners.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    io::Read as _,
    net::Shutdown,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

#[test]
fn native_backend_selection_is_checked_before_private_display_setup() {
    let rust = std::any::TypeId::of::<wayland_backend::server::Backend<()>>()
        == std::any::TypeId::of::<wayland_backend::rs::server::Backend<()>>();
    let result = WaylandHost::new(16, 16);
    if rust {
        assert!(result.is_ok())
    } else {
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported)
    }
}

pub(super) fn flood_root(connection: &Connection, qh: &QueueHandle<Fixture>) -> ! {
    // Reserve a valid free callback ID, then act as a malicious wire client.
    // The normal event queue must not resume after bypassing its object map.
    let callback = connection.display().sync(qh, ());
    let id = callback.id().protocol_id();
    connection.flush().unwrap();
    let socket = UnixStream::from(connection.backend().poll_fd().try_clone_to_owned().unwrap());
    let flood = Flood::from_socket(socket, id);
    while !flood.closed.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(2));
    }
    drop(flood);
    std::process::exit(0)
}

#[test]
fn a_flooding_presented_root_fails_with_revoked_input_and_retires_its_display() {
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
    key(&mut host, initial.target, 130);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match host.dispatch() {
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
                break;
            }
            Ok(Some(frame)) => present(&mut host, &frame),
            Ok(None) => (),
        }
        if host.root_failed() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root did not exhaust a continuous dispatch"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(host.input_target().is_none());
    assert!(host.text_input().is_none());
    assert!(
        !host
            .input(
                initial.target,
                Input::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
    );
    let socket = host.socket_path().to_owned();
    drop(host);
    assert!(!socket.exists());
    key(&mut peer, other.target, 100);
    let survivor = next(&mut peer);
    assert_eq!(pixel(&survivor, 0, 0), [1, 10, 20, 255]);
}

struct Flood {
    socket: UnixStream,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    closed: Arc<AtomicBool>,
}
impl Flood {
    fn connect(host: &WaylandHost) -> Self {
        let socket = UnixStream::connect(host.socket_path()).unwrap();
        Self::from_socket(socket, 2)
    }
    fn from_socket(socket: UnixStream, callback_id: u32) -> Self {
        socket.set_nonblocking(false).unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut writer = socket.try_clone().unwrap();
        let mut reader = socket.try_clone().unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let received = closed.clone();
        let reader = thread::spawn(move || {
            let mut bytes = [0; 65536];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            received.store(true, Ordering::Release);
        });
        let written = Arc::new(AtomicUsize::new(0));
        let progress = written.clone();
        let writer = thread::spawn(move || {
            // wl_display.sync creates callback 2, whose done event destroys it.
            // Reuse stays inside this connection's namespace, not the root's.
            let mut bytes = Vec::with_capacity(4096 * 12);
            for _ in 0..4096 {
                bytes.extend_from_slice(&1u32.to_ne_bytes());
                bytes.extend_from_slice(&(12u32 << 16).to_ne_bytes());
                bytes.extend_from_slice(&callback_id.to_ne_bytes());
            }
            while writer.write_all(&bytes).is_ok() {
                progress.fetch_add(4096, Ordering::Release);
            }
        });
        let result = Self {
            socket,
            writer: Some(writer),
            reader: Some(reader),
            closed,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while written.load(Ordering::Acquire) < 4096 {
            assert!(
                Instant::now() < deadline,
                "flood did not preload its socket"
            );
            thread::yield_now();
        }
        result
    }
}
impl Drop for Flood {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        if let Some(writer) = self.writer.take() {
            writer.join().unwrap();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

#[test]
fn a_core_request_flood_cannot_prevent_dispatch_return_or_revoke_another_client() {
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
    let flood = Flood::connect(&host);
    let closed = flood.closed.clone();
    let (done, result) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let start = Instant::now();
        host.dispatch().unwrap();
        key(&mut host, initial.target, 100);
        let response = next(&mut host);
        let root_alive = pixel(&response, 0, 0) == [1, 10, 20, 255];
        let _ = done.send((start.elapsed(), root_alive));
    });
    key(&mut peer, other.target, 100);
    let survivor = next(&mut peer);
    present(&mut peer, &survivor);
    assert_eq!(pixel(&survivor, 0, 0), [1, 10, 20, 255]);
    let outcome = result.recv_timeout(Duration::from_secs(2));
    let disconnected = closed.load(Ordering::Acquire);
    drop(flood); // Also releases an unpatched dispatch before reporting failure.
    worker.join().unwrap();
    let (elapsed, root_alive) =
        outcome.expect("continuous core traffic prevented dispatch from returning");
    assert!(elapsed < Duration::from_secs(2));
    assert!(disconnected, "flooding helper was not disconnected");
    assert!(
        root_alive,
        "the flooding helper retired its unrelated root client"
    );
}

#[test]
fn display_cancellation_interrupts_a_drain_and_old_handles_cannot_cancel_a_new_owner() {
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
    let control = host.dispatch_control();
    let flood = Flood::connect(&host);
    let socket = host.socket_path().to_owned();
    let (started, ready) = mpsc::sync_channel(1);
    let (done, result) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        let error = loop {
            match host.dispatch() {
                Err(error) => break error,
                Ok(_) => thread::yield_now(),
            }
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        host.set_active(true);
        assert!(host.input_target().is_none());
        assert!(host.text_input().is_none());
        assert!(
            !host
                .input(
                    initial.target,
                    Input::Key {
                        code: 30,
                        pressed: true
                    }
                )
                .unwrap()
        );
        assert_eq!(
            host.dispatch().unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        drop(host);
        done.send(()).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    thread::sleep(Duration::from_millis(20));
    control.cancel();
    assert!(control.is_cancelled());
    let completed = result.recv_timeout(Duration::from_secs(2));
    drop(flood);
    worker.join().unwrap();
    completed.unwrap();
    assert!(!socket.exists());
    control.cancel();
    key(&mut peer, other.target, 100);
    let survivor = next(&mut peer);
    assert_eq!(pixel(&survivor, 0, 0), [1, 10, 20, 255]);
}

#[test]
fn multiple_flooding_helpers_yield_a_turn_before_all_connection_budgets_are_spent() {
    let mut host = WaylandHost::new(64, 48).unwrap();
    let _root = client(&host);
    let initial = next(&mut host);
    host.set_active(true);
    present(&mut host, &initial);
    let floods: Vec<_> = (0..7).map(|_| Flood::connect(&host)).collect();
    let (done, result) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let start = Instant::now();
        host.dispatch().unwrap();
        let _ = done.send(start.elapsed());
    });
    let elapsed = result.recv_timeout(Duration::from_secs(1));
    drop(floods);
    worker.join().unwrap();
    assert!(
        elapsed.expect("one dispatch consumed every flooding helper's budget")
            < Duration::from_secs(1)
    );
}
