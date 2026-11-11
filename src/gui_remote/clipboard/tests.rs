//! Real native channel regression coverage for owner and clipboard lease boundaries.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::gui::{ClipboardSelection, TestAppContext};
use vivid_protocol::revision::{GrantGeneration, InputEpoch, SurfaceGeneration};

fn directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    tempfile::Builder::new()
        .prefix("vvcb-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn tag() -> crate::InputTuple {
    crate::InputTuple {
        producer_epoch: InputEpoch::new(1),
        grant_generation: GrantGeneration::new(1),
        context_id: 1,
        surface_id: 7,
        surface_generation: SurfaceGeneration::new(1),
    }
}
fn connect(host: &Host) -> LocalStream {
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    local::send(
        &mut stream,
        &Message::Hello {
            version: 1,
            owner: *host.owner,
        },
        1024,
        Instant::now() + BUDGET,
    )
    .unwrap();
    assert!(matches!(
        local::receive(&mut stream, 1024, Instant::now() + BUDGET).unwrap(),
        Message::Ready { version: 1 }
    ));
    stream
}
fn send(stream: &mut LocalStream, message: Message) {
    local::send(stream, &message, MAX_FRAME, Instant::now() + BUDGET).unwrap();
}
fn receive(stream: &mut LocalStream) -> Message {
    local::receive(stream, MAX_FRAME, Instant::now() + BUDGET).unwrap()
}
fn request(host: &Host) -> bridge::Request {
    let end = Instant::now() + BUDGET;
    loop {
        if let Some(request) = host.next_request() {
            return request;
        }
        assert!(Instant::now() < end, "clipboard request was not admitted");
        thread::sleep(POLL);
    }
}

#[test]
fn exact_binding_revokes_late_reads_and_writes_without_disturbing_a_peer() {
    let directory = directory();
    let (one, service_one) = Host::bind(&directory.path().join("one")).unwrap();
    let (two, service_two) = Host::bind(&directory.path().join("two")).unwrap();
    let mut a = connect(&one);
    let mut b = connect(&two);
    for host in [&one, &two] {
        host.set_binding(Some(tag()));
    }
    for stream in [&mut a, &mut b] {
        send(
            stream,
            Message::Read {
                id: 1,
                binding: binding(tag()),
            },
        );
    }
    let old = request(&one);
    let peer = request(&two);
    one.set_binding(None);
    one.set_binding(Some(tag()));
    assert!(
        old.lease()
            .while_live(|| panic!("retired read reached platform"))
            .is_none()
    );
    old.complete(Ok("retired-private-text".into()));
    peer.complete(Ok("peer text".into()));
    assert!(matches!(receive(&mut a), Message::Refused { id: 1 }));
    assert!(matches!(receive(&mut b), Message::Text { id: 1, text } if text == "peer text"));

    send(
        &mut a,
        Message::Write {
            id: 2,
            binding: binding(tag()),
            text: "queued text".into(),
        },
    );
    assert!(matches!(receive(&mut a), Message::Queued { id: 2 }));
    let write = request(&one);
    assert_eq!(write.write_text(), Some("queued text"));
    one.set_binding(None);
    assert!(
        write
            .lease()
            .while_live(|| panic!("retired write reached platform"))
            .is_none()
    );
    drop(write);
    send(
        &mut b,
        Message::Write {
            id: 2,
            binding: binding(tag()),
            text: "peer survives".into(),
        },
    );
    assert!(matches!(receive(&mut b), Message::Queued { id: 2 }));
    let write = request(&two);
    assert!(write.lease().while_live(|| ()).is_some());
    write.complete(Ok(String::new()));
    drop(service_one);
    send(
        &mut b,
        Message::Read {
            id: 3,
            binding: binding(tag()),
        },
    );
    request(&two).complete(Ok("still available".into()));
    assert!(matches!(receive(&mut b), Message::Text { id: 3, .. }));
    drop(service_two);
}

#[test]
fn inactive_foreign_and_replayed_requests_cannot_acquire_clipboard_authority() {
    let directory = directory();
    let (host, service) = Host::bind(&directory.path().join("refuse")).unwrap();
    let mut stream = connect(&host);
    send(
        &mut stream,
        Message::Read {
            id: 1,
            binding: binding(tag()),
        },
    );
    assert!(matches!(receive(&mut stream), Message::Refused { id: 1 }));
    host.set_binding(Some(tag()));
    let mut foreign = binding(tag());
    foreign[2] += 1;
    send(
        &mut stream,
        Message::Read {
            id: 2,
            binding: foreign,
        },
    );
    assert!(matches!(receive(&mut stream), Message::Refused { id: 2 }));
    assert!(host.next_request().is_none());
    send(
        &mut stream,
        Message::Read {
            id: 2,
            binding: binding(tag()),
        },
    );
    assert!(local::receive::<Message>(&mut stream, MAX_FRAME, Instant::now() + BUDGET).is_err());
    drop(service);
}

#[test]
fn a_missing_provider_never_falls_back_to_native_clipboards() {
    let cx = TestAppContext::single();
    let read = cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("native-private-text".into()));
        let provider = Producer {
            sender: None,
            gate: Arc::new(Gate::default()),
            budget: ResourceBudget::new(CAPACITY),
        };
        assert!(!format!("{provider:?}").contains("native-private-text"));
        cx.set_hosted_clipboard(std::rc::Rc::new(provider));
        assert!(cx.read_from_clipboard().is_none());
        #[cfg(target_os = "macos")]
        assert!(cx.read_from_find_pasteboard().is_none());
        let read =
            cx.read_clipboard_text_async(ClipboardSelection::Clipboard, bridge::MAX_TEXT_BYTES);

        cx.write_to_clipboard(ClipboardItem::new_string("refused".into()));
        assert!(!cx.supports_uri_list_clipboard());
        read
    });
    cx.run_until_parked();
    assert!(pollster::block_on(read).is_err());
}

#[test]
fn a_cancelled_or_expired_producer_scope_does_not_return_after_rebinding() {
    let lifecycle = Arc::new(crate::SessionLifecycle::new());
    let gate = Gate::default();
    let old = Arc::new(Grant {
        tag: binding(tag()),
        deadline: Mutex::new(Instant::now() + BUDGET),
        lifecycle: Arc::downgrade(&lifecycle),
        revoked: AtomicBool::new(false),
    });
    *gate.0.lock().unwrap() = Some(old.clone());
    assert!(gate.capture().is_some());
    gate.revoke();
    assert!(!old.live());
    let fresh = Arc::new(Grant {
        tag: binding(tag()),
        deadline: Mutex::new(Instant::now() + BUDGET),
        lifecycle: Arc::downgrade(&lifecycle),
        revoked: AtomicBool::new(false),
    });
    *gate.0.lock().unwrap() = Some(fresh.clone());
    assert!(fresh.live());
    assert!(!old.live());
    lifecycle.closed.store(true, Ordering::Release);
    assert!(!fresh.live());
    lifecycle.closed.store(false, Ordering::Release);
    *fresh.deadline.lock().unwrap() = Instant::now() - BUDGET;
    assert!(!fresh.live());
    assert!(gate.capture().is_none());
}

#[test]
fn queued_writes_keep_the_owner_capacity_charged_until_platform_completion() {
    let directory = directory();
    let (host, service) = Host::bind(&directory.path().join("capacity")).unwrap();
    let mut stream = connect(&host);
    host.set_binding(Some(tag()));
    for id in 1..=5 {
        send(
            &mut stream,
            Message::Write {
                id,
                binding: binding(tag()),
                text: "bounded".into(),
            },
        );
        if id <= CAPACITY as u64 {
            assert!(matches!(receive(&mut stream), Message::Queued { id: got } if got == id));
        } else {
            assert!(matches!(receive(&mut stream), Message::Refused { id: 5 }));
        }
    }
    request(&host).complete(Ok(String::new()));
    send(
        &mut stream,
        Message::Write {
            id: 6,
            binding: binding(tag()),
            text: "capacity returned".into(),
        },
    );
    assert!(matches!(receive(&mut stream), Message::Queued { id: 6 }));
    host.set_binding(None);
    assert!(host.next_request().is_none());
    drop(service);
}

#[test]
fn malformed_authentication_and_oversized_frames_retire_only_their_service() {
    use std::io::Write as _;
    let directory = directory();
    let (host, service) = Host::bind(&directory.path().join("bad")).unwrap();
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    send(
        &mut stream,
        Message::Hello {
            version: 1,
            owner: [0; 16],
        },
    );
    assert!(local::receive::<Message>(&mut stream, 1024, Instant::now() + BUDGET).is_err());
    drop(service);
    let (host, service) = Host::bind(&directory.path().join("large")).unwrap();
    let mut stream = connect(&host);
    stream
        .write_all(&u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes())
        .unwrap();
    assert!(local::receive::<Message>(&mut stream, MAX_FRAME, Instant::now() + BUDGET).is_err());
    assert!(!format!("{host:?}").contains(&host.endpoint.to_string_lossy().to_string()));
    drop(service);
}
