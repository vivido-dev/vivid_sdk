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
    connect_version(host, 1)
}
fn connect_version(host: &Host, version: u32) -> LocalStream {
    let mut stream = LocalStream::connect(&host.endpoint).unwrap();
    local::send(
        &mut stream,
        &Message::Hello {
            version,
            owner: *host.owner,
        },
        1024,
        Instant::now() + BUDGET,
    )
    .unwrap();
    assert!(matches!(
        local::receive(&mut stream, 1024, Instant::now() + BUDGET).unwrap(),
        Message::Ready { version: got } if got == version
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
fn file_offer_sampling_cannot_cross_binding_replacement() {
    let directory = directory();
    let (host, _service) = Host::bind_with_formats(&directory.path().join("offers"), true).unwrap();
    let mut stream = connect_version(&host, 3);
    let mut probe = |id, tag| {
        send(
            &mut stream,
            Message::ProbeFiles {
                id,
                binding: binding(tag),
            },
        );
        match receive(&mut stream) {
            Message::FileOffer { id: got, available } if id == got => available,
            _ => panic!("unexpected file offer response"),
        }
    };
    host.refresh_file_offer(|| panic!("inactive host inspected clipboard"));
    host.set_binding(Some(tag()));
    host.refresh_file_offer(|| {
        host.set_binding(None);
        host.set_binding(Some(tag()));
        true
    });
    assert!(!probe(1, tag()));
    host.refresh_file_offer(|| true);
    assert!(probe(2, tag()));
    let mut foreign = tag();
    foreign.context_id += 1;
    assert!(!probe(3, foreign));
    host.set_binding(None);
    assert!(!probe(4, tag()));
    assert!(host.next_request().is_none());
}

#[test]
fn a_missing_provider_never_falls_back_to_native_clipboards() {
    let cx = TestAppContext::single();
    let read = cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("native-private-text".into()));
        let provider = Producer {
            picker: None,
            web_urls: false,
            image_copy: false,
            media: false,
            file_lists: false,
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
        file_offer_until: Mutex::new(None),
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
        file_offer_until: Mutex::new(None),
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

#[test]
fn file_transfers_share_owner_revocation_and_preserve_the_other_process() {
    let directory = directory();
    let (one, _first) = Host::bind_with_formats(&directory.path().join("one"), true).unwrap();
    let (two, _second) = Host::bind_with_formats(&directory.path().join("two"), true).unwrap();
    let mut a = connect_version(&one, 2);
    let mut b = connect_version(&two, 2);
    for (host, stream) in [(&one, &mut a), (&two, &mut b)] {
        host.set_binding(Some(tag()));
        send(
            stream,
            Message::ReadFiles {
                id: 1,
                binding: binding(tag()),
            },
        );
    }
    let old = request(&one);
    let peer = request(&two);
    assert_eq!(peer.format(), bridge::Format::Files);
    one.set_binding(None);
    one.set_binding(Some(tag()));
    let files = bridge::FileList::from_paths(&[PathBuf::from("/tmp/文 1")]).unwrap();
    old.complete(Ok(files.as_uri_list().into()));
    peer.complete(Ok(files.as_uri_list().into()));
    assert!(matches!(receive(&mut a), Message::Refused { id: 1 }));
    assert!(matches!(receive(&mut b), Message::Files {id: 1, uris} if uris == files.as_uri_list()));
    send(
        &mut b,
        Message::WriteFiles {
            id: 2,
            binding: binding(tag()),
            uris: "file://remote/path".into(),
        },
    );
    assert!(matches!(receive(&mut b), Message::Refused { id: 2 }));
    assert!(two.next_request().is_none());
    send(
        &mut b,
        Message::WriteFiles {
            id: 3,
            binding: binding(tag()),
            uris: files.as_uri_list().into(),
        },
    );
    assert!(matches!(receive(&mut b), Message::Queued { id: 3 }));
    let write = request(&two);
    assert_eq!(write.write_files(), Some(&files));
    two.set_binding(None);
    assert!(!write.is_live());
}

#[test]
fn older_text_producer_can_connect_to_file_capable_host() {
    let directory = directory();
    let (host, _service) = Host::bind_with_formats(&directory.path().join("legacy"), true).unwrap();
    let mut stream = connect(&host);
    host.set_binding(Some(tag()));
    send(
        &mut stream,
        Message::Read {
            id: 1,
            binding: binding(tag()),
        },
    );
    request(&host).complete(Ok("text".into()));
    assert!(matches!(receive(&mut stream), Message::Text {id: 1, text} if text == "text"));
    send(
        &mut stream,
        Message::ReadFiles {
            id: 2,
            binding: binding(tag()),
        },
    );
    assert!(local::receive::<Message>(&mut stream, MAX_FRAME, Instant::now() + BUDGET).is_err());
    assert!(host.next_request().is_none());
}

#[test]
fn file_clipboard_crosses_the_hosted_app_api_and_child_process() {
    const CHILD: &str = "VIVID_SDK_TEST_FILE_CLIPBOARD_CHILD";
    let paths = [PathBuf::from("/tmp/文 1"), PathBuf::from("/tmp/2")];
    let files = bridge::FileList::from_paths(&paths).unwrap();
    if std::env::var_os(CHILD).is_some() {
        let lifecycle = Arc::new(crate::SessionLifecycle::new());
        let gate = Arc::new(Gate::default());
        *gate.0.lock().unwrap() = Some(Arc::new(Grant {
            tag: binding(tag()),
            deadline: Mutex::new(Instant::now() + STARTUP),
            lifecycle: Arc::downgrade(&lifecycle),
            revoked: AtomicBool::new(false),
            file_offer_until: Mutex::new(None),
        }));
        let (producer, _service) = Producer::from_env(gate.clone()).unwrap();
        let cx = TestAppContext::single();
        let (done, result) = mpsc::sync_channel(1);
        cx.update(|cx| {
            cx.set_hosted_clipboard(std::rc::Rc::new(producer));
            assert!(cx.supports_uri_list_clipboard());
        });
        let deadline = Instant::now() + STARTUP;
        while !cx.update(|cx| cx.clipboard_has_file_list()) {
            assert!(Instant::now() < deadline, "file offer was never published");
            thread::sleep(POLL);
        }
        cx.update(|cx| {
            let read = cx.read_clipboard_uri_list_async(bridge::MAX_TEXT_BYTES);
            cx.foreground_executor()
                .spawn(async move { done.send(read.await).unwrap() })
                .detach();
        });
        let deadline = Instant::now() + STARTUP;
        loop {
            cx.run_until_parked();
            if let Ok(result) = result.try_recv() {
                assert_eq!(result.unwrap().as_deref(), Some(files.as_uri_list()));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(POLL);
        }
        cx.update(|cx| {
            cx.write_to_clipboard(ClipboardItem {
                entries: vec![gui::ClipboardEntry::ExternalPaths(gui::ExternalPaths(
                    paths.into_iter().rev().collect(),
                ))],
            })
        });
        // A subsequent read acts as a barrier for the queued write.
        let read = cx.update(|cx| cx.read_clipboard_uri_list_async(1));
        let (done, result) = mpsc::sync_channel(1);
        cx.update(|cx| {
            cx.foreground_executor()
                .spawn(async move { done.send(read.await).unwrap() })
                .detach()
        });
        loop {
            cx.run_until_parked();
            if let Ok(result) = result.try_recv() {
                assert_eq!(result, Err(ClipboardReadError::UnsupportedContent));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(POLL);
        }
        while cx.update(|cx| cx.clipboard_has_file_list()) {
            assert!(
                Instant::now() < deadline,
                "withdrawn offer remained visible"
            );
            thread::sleep(POLL);
        }
        cx.update(|cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("withdrawal observed".into()))
        });
        while !cx.update(|cx| cx.clipboard_has_file_list()) {
            assert!(
                Instant::now() < deadline,
                "second file offer was never published"
            );
            thread::sleep(POLL);
        }
        gate.revoke();
        assert!(!cx.update(|cx| cx.clipboard_has_file_list()));
        return;
    }

    let directory = directory();
    let (host, _service) = Host::bind_with_formats(&directory.path().join("files"), true).unwrap();
    host.set_binding(Some(tag()));
    let mut command = Command::new(std::env::current_exe().unwrap());
    host.configure(&mut command);
    let mut child = command.arg("--exact")
        .arg("gui_remote::clipboard::tests::file_clipboard_crosses_the_hosted_app_api_and_child_process")
        .env(CHILD, "1").spawn().unwrap();
    let deadline = Instant::now() + STARTUP;
    let initial = loop {
        host.refresh_file_offer(|| true);
        if let Some(request) = host.next_request() {
            break request;
        }
        assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline);
        thread::sleep(POLL);
    };
    assert_eq!(initial.format(), bridge::Format::Files);
    initial.complete(Ok(files.as_uri_list().into()));
    let written = request(&host);
    let reversed = bridge::FileList::from_paths(&[paths[1].clone(), paths[0].clone()]).unwrap();
    assert_eq!(written.write_files(), Some(&reversed));
    written.complete(Ok(String::new()));
    request(&host).complete(Ok(reversed.into_uri_list()));
    let mut reoffered = false;
    while child.try_wait().unwrap().is_none() {
        host.refresh_file_offer(|| reoffered);
        if let Some(ack) = host.next_request() {
            assert_eq!(ack.write_text(), Some("withdrawal observed"));
            ack.complete(Ok(String::new()));
            reoffered = true;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(POLL);
    }
    assert!(reoffered);
    assert!(child.wait().unwrap().success());
}

// Two RGBA pixels, opaque red and translucent blue.
const FIXTURE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 244, 34, 127, 138, 0, 0, 0, 14, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 66, 245,
    0, 15, 121, 3, 125, 21, 103, 11, 0, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[test]
fn explicit_media_paste_reaches_only_the_live_child() {
    const CHILD: &str = "VIVID_SDK_TEST_CLIPBOARD_MEDIA_CHILD";
    let bitmap = || gui::ClipboardPng::from_encoded(gui::ImageFormat::Png, FIXTURE_PNG).unwrap();
    let files = bridge::FileList::from_paths(&[PathBuf::from("/tmp/media fixture")]).unwrap();
    if std::env::var_os(CHILD).is_some() {
        let lifecycle = Arc::new(crate::SessionLifecycle::new());
        let gate = Arc::new(Gate::default());
        *gate.0.lock().unwrap() = Some(Arc::new(Grant {
            tag: binding(tag()),
            deadline: Mutex::new(Instant::now() + STARTUP),
            lifecycle: Arc::downgrade(&lifecycle),
            revoked: AtomicBool::new(false),
            file_offer_until: Mutex::new(None),
        }));
        let (producer, _service) = Producer::from_env(gate).unwrap();
        let cx = TestAppContext::single();
        cx.update(|cx| cx.set_hosted_clipboard(std::rc::Rc::new(producer)));
        let paste = || {
            let (done, result) = mpsc::sync_channel(1);
            cx.update(|cx| {
                let read = cx.read_clipboard_media_async();
                cx.foreground_executor()
                    .spawn(async move {
                        let _ = done.send(read.await);
                    })
                    .detach();
            });
            let deadline = Instant::now() + STARTUP;
            loop {
                cx.run_until_parked();
                if let Ok(value) = result.try_recv() {
                    return value;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(POLL);
            }
        };
        cx.update(|cx| {
            cx.write_to_clipboard(ClipboardItem::new_image(&gui::Image::from_bytes(
                gui::ImageFormat::Png,
                FIXTURE_PNG.to_vec(),
            )))
        });
        let Ok(Some(gui::ClipboardPasteMedia::Image(image))) = paste() else {
            panic!("missing child image")
        };
        assert_eq!(image.into_bytes(), bitmap().into_bytes());
        assert!(
            matches!(paste(), Ok(Some(gui::ClipboardPasteMedia::Files(uris))) if uris == files.as_uri_list())
        );
        assert!(matches!(paste(), Ok(None)));
        assert!(matches!(paste(), Err(ClipboardReadError::Unavailable)));
        return;
    }
    let directory = directory();
    let (host, _service) = Host::bind_with_media(&directory.path().join("media")).unwrap();
    let (peer, _peer_service) = Host::bind_with_media(&directory.path().join("peer")).unwrap();
    let mut stream = connect_version(&peer, 4);
    host.set_binding(Some(tag()));
    peer.set_binding(Some(tag()));
    let mut command = Command::new(std::env::current_exe().unwrap());
    host.configure(&mut command);
    let mut child = command
        .arg("--exact")
        .arg("gui_remote::clipboard::tests::explicit_media_paste_reaches_only_the_live_child")
        .env(CHILD, "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + STARTUP;
    let first = loop {
        if let Some(read) = host.next_request() {
            break read;
        }
        assert!(Instant::now() < deadline && child.try_wait().unwrap().is_none());
        thread::sleep(POLL);
    };
    let mut first = first;
    let copied = first.take_write_image().expect("child bitmap copy");
    assert_eq!(copied.format, gui::ImageFormat::Png);
    assert_eq!(copied.bytes, bitmap().into_bytes());
    first.complete(Ok(String::new()));
    request(&host).complete_media(Ok(Some(gui::ClipboardPasteMedia::Image(bitmap()))));
    request(&host).complete_media(Ok(Some(gui::ClipboardPasteMedia::Files(
        files.into_uri_list(),
    ))));
    request(&host).complete_media(Ok(None));
    let retired = request(&host);
    send(
        &mut stream,
        Message::ReadMedia {
            id: 1,
            binding: binding(tag()),
        },
    );
    let surviving = request(&peer);
    host.set_binding(None);
    host.set_binding(Some(tag()));
    retired.complete_media(Ok(Some(gui::ClipboardPasteMedia::Image(bitmap()))));
    surviving.complete_media(Ok(Some(gui::ClipboardPasteMedia::Image(bitmap()))));
    let Message::Png { id: 1, bytes } = receive(&mut stream) else {
        panic!("missing peer image")
    };
    let expected = bitmap().into_bytes();
    assert_eq!(usize::try_from(bytes).unwrap(), expected.len());
    let mut png = vec![0; expected.len()];
    local::read_exact(&mut stream, &mut png, Instant::now() + BUDGET).unwrap();
    assert_eq!(png, expected);
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_complete_bitmap_tail_can_be_read_after_the_writer_exits() {
    for bytes in [FIXTURE_PNG, &FIXTURE_PNG[..FIXTURE_PNG.len() - 1]] {
        let (mut writer, mut reader) = LocalStream::pair().unwrap();
        let deadline = Instant::now() + BUDGET;
        send_bitmap_bytes(&mut writer, bytes, deadline).unwrap();
        drop(writer);
        let body = receive_bitmap_bytes(&mut reader, FIXTURE_PNG.len() as u32, deadline);
        if bytes.len() == FIXTURE_PNG.len() {
            assert_eq!(body.unwrap(), FIXTURE_PNG);
        } else {
            assert_eq!(body.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        }
    }
}

#[test]
fn binary_image_tail_transfers_the_limit_and_rejects_invalid_lengths() {
    let (mut writer, mut reader) = LocalStream::pair().unwrap();
    for size in [
        0,
        u32::try_from(gui::MAX_CLIPBOARD_IMAGE_BYTES + 1).unwrap(),
    ] {
        assert_eq!(
            receive_bitmap_bytes(&mut reader, size, Instant::now() + BUDGET)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    let payload: Vec<u8> = (0..=255u8)
        .cycle()
        .take(gui::MAX_CLIPBOARD_IMAGE_BYTES)
        .collect();
    let end = Instant::now() + BUDGET;
    thread::scope(|scope| {
        let sender = scope.spawn(|| send_bitmap_bytes(&mut writer, &payload, end));
        let received =
            receive_bitmap_bytes(&mut reader, u32::try_from(payload.len()).unwrap(), end).unwrap();
        sender.join().unwrap().unwrap();
        assert_eq!(received, payload);
    });
}

#[test]
fn bitmap_copy_revalidates_the_binding_after_the_body_and_rejects_invalid_images() {
    let dir = directory();
    let (host, _service) = Host::bind_with_media(&dir.path().join("copy")).unwrap();
    let mut stream = connect_version(&host, 5);
    let header = |id, bytes| Message::WriteBitmap {
        id,
        binding: binding(tag()),
        bytes,
        format: BitmapFormat::Png,
    };
    let (peer, _peer_service) = Host::bind_with_media(&dir.path().join("peer")).unwrap();
    let mut peer_stream = connect_version(&peer, 5);
    peer.set_binding(Some(tag()));
    let size = u32::try_from(FIXTURE_PNG.len()).unwrap();
    send(&mut stream, header(1, size));
    assert!(matches!(receive(&mut stream), Message::Refused { id: 1 }));
    host.set_binding(Some(tag()));
    send(&mut stream, header(2, u32::MAX));
    assert!(matches!(receive(&mut stream), Message::Refused { id: 2 }));
    send(&mut stream, header(3, size));
    assert!(matches!(
        receive(&mut stream),
        Message::SendBitmap { id: 3 }
    ));
    send(&mut peer_stream, header(1, size));
    assert!(matches!(
        receive(&mut peer_stream),
        Message::SendBitmap { id: 1 }
    ));
    host.set_binding(None);
    host.set_binding(Some(tag()));
    send(
        &mut peer_stream,
        Message::BitmapBody {
            id: 1,
            active: true,
        },
    );
    send_bitmap_bytes(&mut peer_stream, FIXTURE_PNG, Instant::now() + BUDGET).unwrap();
    assert!(matches!(
        receive(&mut peer_stream),
        Message::Queued { id: 1 }
    ));
    let mut peer_copy = request(&peer);
    assert_eq!(
        peer_copy.take_write_image().unwrap().bytes,
        gui::ClipboardPng::from_encoded(gui::ImageFormat::Png, FIXTURE_PNG)
            .unwrap()
            .into_bytes()
    );
    send(
        &mut stream,
        Message::BitmapBody {
            id: 3,
            active: true,
        },
    );
    send_bitmap_bytes(&mut stream, FIXTURE_PNG, Instant::now() + BUDGET).unwrap();
    assert!(matches!(receive(&mut stream), Message::Refused { id: 3 }));
    send(&mut stream, header(4, 4));
    assert!(matches!(
        receive(&mut stream),
        Message::SendBitmap { id: 4 }
    ));
    send(
        &mut stream,
        Message::BitmapBody {
            id: 4,
            active: true,
        },
    );
    send_bitmap_bytes(&mut stream, b"oops", Instant::now() + BUDGET).unwrap();
    assert!(matches!(receive(&mut stream), Message::Refused { id: 4 }));
    assert!(host.next_request().is_none());
    send(&mut stream, header(5, size));
    assert!(matches!(
        receive(&mut stream),
        Message::SendBitmap { id: 5 }
    ));
    send(
        &mut stream,
        Message::BitmapBody {
            id: 5,
            active: false,
        },
    );
    assert!(matches!(receive(&mut stream), Message::Refused { id: 5 }));
    send(&mut stream, header(6, size));
    assert!(matches!(
        receive(&mut stream),
        Message::SendBitmap { id: 6 }
    ));
    send(
        &mut stream,
        Message::BitmapBody {
            id: 6,
            active: true,
        },
    );
    send_bitmap_bytes(&mut stream, FIXTURE_PNG, Instant::now() + BUDGET).unwrap();
    assert!(matches!(receive(&mut stream), Message::Queued { id: 6 }));
    let copy = request(&host);
    host.set_binding(None);
    assert!(
        copy.lease()
            .while_live(|| panic!("revoked write reached platform"))
            .is_none()
    );
}

#[test]
fn web_url_dispatch_consumes_one_live_owner_gesture() {
    let directory = directory();
    let (one, _one_service) = Host::bind(&directory.path().join("one")).unwrap();
    let (two, _two_service) = Host::bind(&directory.path().join("two")).unwrap();
    let mut a = connect(&one);
    let mut b = connect(&two);
    let url = "https://example.invalid/path";
    let attempt = |stream: &mut LocalStream, id, url: &str| {
        send(
            stream,
            Message::OpenWebUrl {
                id,
                binding: binding(tag()),
                url: url.to_owned(),
            },
        );
        matches!(receive(stream), Message::Queued { id: got } if got == id)
    };
    for host in [&one, &two] {
        host.set_binding(Some(tag()));
    }
    one.deliver_web_gesture(tag(), || Ok(false)).unwrap();
    assert!(!attempt(&mut a, 1, url));
    one.deliver_web_gesture(tag(), || Ok(true)).unwrap();
    assert!(!attempt(&mut b, 1, url));
    assert!(!attempt(&mut a, 2, "file:///tmp/example"));
    assert!(attempt(&mut a, 3, url));
    assert!(!attempt(&mut a, 4, url));
    two.deliver_web_gesture(tag(), || Ok(true)).unwrap();
    assert!(attempt(&mut b, 2, url));
    one.set_binding(None);
    one.set_binding(Some(tag()));
    one.dispatch_web_url(Some(tag()), |_| panic!("retired URL reached the platform"));
    let mut opened = None;
    two.dispatch_web_url(Some(tag()), |url| {
        two.set_binding(None);
        opened = Some(url.to_owned());
    });
    assert_eq!(opened.as_deref(), Some(url));
    two.dispatch_web_url(Some(tag()), |_| panic!("URL dispatched twice"));
    assert!(!attempt(&mut a, 5, url));
    one.deliver_web_gesture(tag(), || Ok(true)).unwrap();
    assert!(attempt(&mut a, 6, url));
    thread::sleep(BUDGET);
    one.dispatch_web_url(Some(tag()), |_| panic!("expired URL reached the platform"));
    one.deliver_web_gesture(tag(), || Ok(true)).unwrap();
    assert!(attempt(&mut a, 7, url));
    let mut foreign = tag();
    foreign.surface_id += 1;
    one.dispatch_web_url(Some(foreign), |_| {
        panic!("foreign GUI grant dispatched URL")
    });
    one.dispatch_web_url(Some(tag()), |_| panic!("discarded URL returned"));
    for bad in [
        "https://user:secret@example.invalid",
        "https://example.invalid/\n",
        "javascript:alert(1)",
        "http:relative",
    ] {
        assert!(web_url(bad).is_none());
    }
}

#[test]
fn same_numbered_path_panels_retire_and_complete_on_their_own_connections() {
    let directory = directory();
    let (one, _first_service) = Host::bind(&directory.path().join("one")).unwrap();
    let (two, _second_service) = Host::bind(&directory.path().join("two")).unwrap();
    let mut first = connect(&one);
    let mut second = connect(&two);
    let cx = TestAppContext::single();
    let mut first_panels = PathPanels::default();
    let mut second_panels = PathPanels::default();
    for (host, stream, panels) in [
        (&one, &mut first, &mut first_panels),
        (&two, &mut second, &mut second_panels),
    ] {
        host.enable_path_panels(&mut Command::new("unused"));
        host.set_binding(Some(tag()));
        assert!(host.deliver_web_gesture(tag(), || Ok(true)).unwrap());
        send(
            stream,
            Message::Picker {
                id: 1,
                request: picker::Request::Start {
                    binding: binding(tag()),
                    options: picker::Options::Open {
                        files: true,
                        directories: false,
                        multiple: false,
                        prompt: None,
                    },
                },
            },
        );
        assert!(matches!(
            receive(stream),
            Message::PickerReply {
                id: 1,
                answer: picker::Answer::Pending
            }
        ));
        cx.update(|cx| panels.poll(host, Some(tag()), cx));
        // An already-presented OS dialog takes focus from the original window.
        host.set_binding(None);
    }
    one.retire_path_panel();
    cx.update(|cx| first_panels.poll(&one, None, cx));
    cx.simulate_path_prompt_response(|_| Some(vec!["/tmp/retired-choice".into()]));
    cx.simulate_path_prompt_response(|_| Some(vec!["/tmp/peer-choice".into()]));
    cx.update(|cx| second_panels.poll(&two, None, cx));
    for stream in [&mut first, &mut second] {
        send(
            stream,
            Message::Picker {
                id: 2,
                request: picker::Request::Poll(1),
            },
        );
    }
    assert!(matches!(
        receive(&mut first),
        Message::PickerReply {
            id: 2,
            answer: picker::Answer::Refused
        }
    ));
    let Message::PickerReply {
        id: 2,
        answer: picker::Answer::Selected(uris),
    } = receive(&mut second)
    else {
        panic!("missing peer selection")
    };
    assert_eq!(
        bridge::FileList::from_uri_list(&uris)
            .unwrap()
            .paths()
            .unwrap(),
        [PathBuf::from("/tmp/peer-choice")]
    );
}
