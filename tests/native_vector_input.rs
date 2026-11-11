//! Stable held-input targets across real authenticated vector repaints.

#![cfg(all(feature = "presenter", feature = "vector-render"))]

// Rust guideline compliant 2026-10-07
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use vivid_sdk::overlay::{Brush, Canvas, Color, Command, Event, HitRole, Path, Rect, WindowMode};
use vivid_sdk::presenter::{
    MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorInputEvent as Input,
    VectorInputResult as Result, VectorInputTarget, VirtualVivid,
};
use vivid_sdk::{
    OverlayLaneEvent, OverlaySession, OverlayWindow, OverlayWindowOptions, ProducerAuthentication,
    ProducerConfig,
};

fn fixture() -> (Arc<VirtualVivid>, NativeVectorHost) {
    let mut config = PresenterConfig::terminal_with_overlay(MediaConfig::default());
    config
        .supported_profiles
        .push(vivid_protocol::registry::OVERLAY_CLIPBOARD.into());
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            config,
        )
        .unwrap(),
    );
    let host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    (presenter, host)
}
fn connect(presenter: &VirtualVivid) -> OverlaySession {
    connect_at(presenter, 1)
}
fn connect_at(presenter: &VirtualVivid, pane: u64) -> OverlaySession {
    let secret = presenter.issue_pane_capability(pane).unwrap();
    OverlaySession::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap()
}
fn window(producer: &OverlaySession, x: f64) -> OverlayWindow {
    producer
        .create_window(OverlayWindowOptions::new(
            Rect::new(x, 0., 48., 48.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap()
}

fn scene(role: HitRole) -> Canvas {
    scene_cursor(role, None)
}
fn scene_cursor(role: HitRole, cursor: Option<vivid_sdk::overlay::CursorShape>) -> Canvas {
    let mut canvas = Canvas::new();
    let path = Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap();
    canvas
        .fill(path.clone(), Brush::Solid(Color(0xff0000ff)))
        .unwrap();
    canvas
        .push(Command::Hit {
            id: 7,
            path,
            role,
            cursor,
        })
        .unwrap();
    canvas
}
fn submit(window: &OverlayWindow, role: HitRole) {
    submit_canvas(window, scene(role));
}
fn submit_canvas(window: &OverlayWindow, canvas: Canvas) {
    let submission = window.submit(canvas).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while window.reconcile().unwrap().accepted_revision != submission.revision() {
        assert!(
            Instant::now() < deadline,
            "submitted scene was not accepted"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn display(host: &mut NativeVectorHost, count: usize) -> VectorInputTarget {
    display_at(host, 1, count)
}
fn display_at(host: &mut NativeVectorHost, pane: u64, count: usize) -> VectorInputTarget {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(pane, 1.).unwrap();
        if frame.drawings().len() == count && host.presented(&frame).unwrap() {
            return host.input_target(&frame).unwrap();
        }
        assert!(Instant::now() < deadline, "composition never became ready");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn pointer(x: f64, button: Option<(u16, bool)>) -> Input {
    Input::Pointer {
        position: [x, 10.],
        button,
        modifiers: 0,
    }
}
fn key(down: bool, repeat: bool, text: Option<&str>) -> Input {
    Input::Key {
        usage: 4,
        down,
        repeat,
        modifiers: 0,
        text: text.map(str::to_owned),
    }
}
fn expect(session: &OverlaySession, predicate: impl Fn(&Event) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(OverlayLaneEvent::Input(input)) =
            session.wait_event(Duration::from_millis(10)).unwrap()
            && predicate(&input.event)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected input was not delivered"
        );
    }
}
fn drain(session: &OverlaySession) -> Vec<Event> {
    let mut result = Vec::new();
    while let Some(event) = session.wait_event(Duration::from_millis(10)).unwrap() {
        if let OverlayLaneEvent::Input(input) = event {
            result.push(input.event);
        }
    }
    result
}

fn received_text(session: &OverlaySession, bytes: usize) -> String {
    let mut text = String::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while text.len() < bytes {
        if let Some(OverlayLaneEvent::Input(input)) =
            session.wait_event(Duration::from_millis(10)).unwrap()
            && let Event::Text(chunk) = input.event
        {
            assert!(chunk.len() <= vivid_protocol::overlay::MAX_EVENT_TEXT_BYTES);
            text.push_str(&chunk);
            assert!(text.len() <= bytes, "text transfer emitted extra bytes");
        }
        assert!(Instant::now() < deadline, "text transfer did not complete");
    }
    text
}

fn set_editor(
    host: &mut NativeVectorHost,
    window: &OverlayWindow,
    revision: u64,
    caret: Option<Rect>,
) {
    std::thread::scope(|scope| {
        let job = scope.spawn(|| window.set_editor_geometry(revision, caret));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !job.is_finished() {
            host.poll();
            assert!(
                Instant::now() < deadline,
                "native editor geometry did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        job.join().unwrap().unwrap();
    });
}

#[test]
fn native_editor_geometry_requires_focus_clips_to_its_pane_and_never_revives_after_focus_loss() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    let target = host.text_target(&image).unwrap();
    assert!(
        b.set_editor_geometry(1, Some(Rect::new(0., 0., 1., 8.).unwrap()))
            .is_err()
    );
    assert!(
        a.set_editor_geometry(2, Some(Rect::new(0., 0., 1., 8.).unwrap()))
            .is_err()
    );
    set_editor(
        &mut host,
        &a,
        1,
        Some(Rect::new(-8., 40., 80., 20.).unwrap()),
    );
    assert_eq!(
        host.editor_state(&target).unwrap().caret,
        Some([0., 40. / 128., 48. / 128., 8. / 128.])
    );
    b.request_focus().unwrap();
    a.request_focus().unwrap();
    assert!(host.editor_state(&target).is_none());
    let image = display(&mut host, 2);
    let current = host.text_target(&image).unwrap();
    assert!(!target.same_focus(&current));
    assert_eq!(host.editor_state(&current).unwrap().caret, None);
    set_editor(&mut host, &a, 1, Some(Rect::new(1., 2., 3., 4.).unwrap()));
    submit(&a, HitRole::Input);
    let image = display(&mut host, 2);
    let repainted = host.text_target(&image).unwrap();
    assert!(current.same_focus(&repainted));
    assert_eq!(host.editor_state(&current).unwrap().caret, None);
    set_editor(&mut host, &a, 2, None);
    assert_eq!(host.editor_state(&current).unwrap().caret, None);
}

#[test]
fn native_ime_preedit_and_commit_are_atomic_bounded_and_scoped_to_the_original_focus() {
    use vivid_sdk::presenter::VectorImeEdit;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    let target = host.text_target(&image).unwrap();
    let preedit = VectorImeEdit {
        preedit: "é😀漢".into(),
        selection: Some(2..6),
        ..Default::default()
    };
    assert_eq!(
        host.dispatch_ime(&target, &preedit).unwrap(),
        Result::Delivered { text: false }
    );
    expect(
        &first,
        |event| matches!(event, Event::Ime { preedit, selection: Some((2, 6)) } if preedit == "é😀漢"),
    );
    let invalid = VectorImeEdit {
        commit: Some("must not commit".into()),
        preedit: "😀".into(),
        selection: Some(1..2),
    };
    assert!(host.dispatch_ime(&target, &invalid).is_err());
    for invalid in [
        VectorImeEdit {
            preedit: "x".repeat(vivid_protocol::overlay::MAX_EVENT_TEXT_BYTES + 1),
            ..Default::default()
        },
        VectorImeEdit {
            commit: Some("x".repeat(vivid_sdk::presenter::MAX_VECTOR_TEXT_BYTES + 1)),
            ..Default::default()
        },
    ] {
        assert!(host.dispatch_ime(&target, &invalid).is_err());
    }
    assert!(
        !drain(&first)
            .iter()
            .any(|event| matches!(event, Event::Text(_) | Event::Ime { .. }))
    );
    let commit = VectorImeEdit {
        commit: Some("確定".into()),
        ..Default::default()
    };
    assert_eq!(
        host.dispatch_ime(&target, &commit).unwrap(),
        Result::Delivered { text: true }
    );
    let events = drain(&first);
    assert!(
        matches!(&events[..], [Event::Ime { preedit, selection: None }, Event::Text(text)] if preedit.is_empty() && text == "確定")
    );
    b.request_focus().unwrap();
    let image = display(&mut host, 2);
    let peer_target = host.text_target(&image).unwrap();
    assert_eq!(
        host.dispatch_ime(&target, &commit).unwrap(),
        Result::Rejected
    );
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Text(_) | Event::Ime { .. }))
    );
    first.close().unwrap();
    display(&mut host, 1);
    assert_eq!(
        host.dispatch_ime(&peer_target, &preedit).unwrap(),
        Result::Delivered { text: false }
    );
    expect(
        &peer,
        |event| matches!(event, Event::Ime { preedit, .. } if preedit == "é😀漢"),
    );
    assert!(!format!("{preedit:?}").contains("é😀漢"));
}

#[test]
fn deferred_ime_and_paste_preserve_key_order_expiry_and_peer_progress() {
    use vivid_sdk::presenter::VectorImeEdit;
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 16, 16, (8, 8));
    let first = connect(&presenter);
    let peer = connect_at(&presenter, 2);
    let a = window(&first, 0.);
    let b = window(&peer, 0.);
    submit(&a, HitRole::Drag);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    b.request_focus().unwrap();
    let image = display(&mut host, 1);
    let text = host.text_target(&image).unwrap();
    let peer_image = display_at(&mut host, 2, 1);
    let peer_text = host.text_target(&peer_image).unwrap();
    assert_eq!(
        host.dispatch_input(&image, key(true, false, None)).unwrap(),
        Result::Delivered { text: false }
    );
    host.dispatch_input(&image, pointer(10., Some((0, true))))
        .unwrap();
    host.dispatch_input(&image, pointer(20., None)).unwrap();
    drain(&first);
    for preedit in ["old", "latest"] {
        assert_eq!(
            host.dispatch_ime(
                &text,
                &VectorImeEdit {
                    preedit: preedit.into(),
                    ..Default::default()
                }
            )
            .unwrap(),
            Result::Deferred
        );
    }
    assert_eq!(
        host.dispatch_ime(
            &text,
            &VectorImeEdit {
                commit: Some("確定".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.queue_text(
            &text,
            "expired",
            Instant::now() + Duration::from_millis(100)
        )
        .unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.queue_text(&text, "paste", Instant::now() + Duration::from_secs(3))
            .unwrap(),
        Result::Deferred
    );
    for event in [key(true, true, Some("k")), key(false, false, None)] {
        assert_eq!(
            host.dispatch_input(&image, event).unwrap(),
            Result::Deferred
        );
    }
    assert_eq!(
        host.dispatch_text(&peer_text, "peer").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&peer, 4), "peer");
    assert!(drain(&first).is_empty());
    std::thread::sleep(Duration::from_millis(110));
    display(&mut host, 1);
    assert_eq!(host.flush_input(), 5);
    let events = drain(&first);
    assert!(
        matches!(
            &events[..],
            [Event::Ime { preedit, .. }, Event::Ime { preedit: clear, .. },
             Event::Text(commit), Event::Text(paste), Event::Key { down: true, repeat: true, .. },
             Event::Text(key), Event::Key { down: false, .. }]
             if preedit == "latest" && clear.is_empty() && commit == "確定" && paste == "paste" && key == "k"
        ),
        "unexpected input order: {events:?}"
    );
    assert_eq!(host.flush_input(), 0);
    assert!(drain(&first).is_empty());
}

#[test]
fn queued_text_and_ime_cannot_revive_after_a_window_focus_round_trip() {
    use vivid_sdk::presenter::VectorImeEdit;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Drag);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    let text = host.text_target(&image).unwrap();
    host.dispatch_input(&image, pointer(10., Some((0, true))))
        .unwrap();
    host.dispatch_input(&image, pointer(20., None)).unwrap();
    assert_eq!(
        host.dispatch_ime(
            &text,
            &VectorImeEdit {
                commit: Some("retired IME".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.queue_text(
            &text,
            "retired paste",
            Instant::now() + Duration::from_secs(3)
        )
        .unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.dispatch_input(&image, Input::Text("retired keyboard".into()))
            .unwrap(),
        Result::Deferred
    );
    b.request_focus().unwrap();
    a.request_focus().unwrap();
    let current = display(&mut host, 2);
    assert_eq!(host.flush_input(), 0);
    for owner in [&first, &peer] {
        assert!(!drain(owner).iter().any(|event| match event {
            Event::Text(_) => true,
            Event::Ime { preedit, .. } => !preedit.is_empty(),
            _ => false,
        }));
    }
    let fresh = host.text_target(&current).unwrap();
    assert_eq!(
        host.dispatch_text(&fresh, "new focus").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&first, 9), "new focus");
    b.request_focus().unwrap();
    let current = display(&mut host, 2);
    let fresh = host.text_target(&current).unwrap();
    assert_eq!(
        host.dispatch_text(&fresh, "peer").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&peer, 4), "peer");
}

#[test]
fn asynchronous_text_targets_survive_repaint_and_deliver_bounded_utf8_chunks_to_one_owner() {
    use vivid_sdk::presenter::MAX_VECTOR_TEXT_BYTES;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    let target = host.text_target(&image).unwrap();
    submit(&a, HitRole::Input);
    host.retire_input_target(&image);
    for _ in 0..4 {
        display(&mut host, 2);
    }
    let mut text = "x".repeat(4095) + "👋λ";
    text.push_str(&"λ".repeat(15_000));
    text.push_str(&"z".repeat(MAX_VECTOR_TEXT_BYTES - text.len()));
    assert!(
        host.dispatch_text(&target, &"x".repeat(MAX_VECTOR_TEXT_BYTES + 1))
            .is_err()
    );
    assert_eq!(
        host.dispatch_text(&target, &text).unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&first, text.len()), text);
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Text(_)))
    );
    assert_eq!(
        host.dispatch_text(&target, "").unwrap(),
        Result::Delivered { text: false }
    );
    let (_, mut foreign) = fixture();
    assert_eq!(
        foreign.dispatch_text(&target, "foreign").unwrap(),
        Result::Rejected
    );
}

#[test]
fn asynchronous_text_rejects_focus_round_trips_and_old_epochs_without_cancelling_new_capture() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    let old_focus = host.text_target(&image).unwrap();
    b.request_focus().unwrap();
    a.request_focus().unwrap();
    let image = display(&mut host, 2);
    assert_eq!(
        host.dispatch_text(&old_focus, "old focus").unwrap(),
        Result::Rejected
    );
    let old_epoch = host.text_target(&image).unwrap();
    host.invalidate(1);
    let image = display(&mut host, 2);
    host.dispatch_input(&image, pointer(10., Some((0, true))))
        .unwrap();
    first.capture_pointer(&a, true).unwrap();
    drain(&first);
    assert_eq!(
        host.dispatch_text(&old_epoch, "old epoch").unwrap(),
        Result::Rejected
    );
    assert!(host.has_pointer_grab(1));
    assert!(
        !drain(&first)
            .iter()
            .any(|event| matches!(event, Event::Cancel))
    );
    let current = host.text_target(&image).unwrap();
    assert_eq!(
        host.dispatch_text(&current, "new grant").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&first, 9), "new grant");
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Text(_)))
    );
}

#[test]
fn asynchronous_text_waits_for_displayed_geometry_and_owner_revocation_preserves_a_peer() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 16, 16, (8, 8));
    let first = connect(&presenter);
    let peer = connect_at(&presenter, 2);
    let a = window(&first, 0.);
    let b = window(&peer, 0.);
    submit(&a, HitRole::Drag);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    b.request_focus().unwrap();
    let image = display(&mut host, 1);
    let a_text = host.text_target(&image).unwrap();
    let peer_image = display_at(&mut host, 2, 1);
    let b_text = host.text_target(&peer_image).unwrap();
    host.dispatch_input(&image, pointer(10., Some((0, true))))
        .unwrap();
    host.dispatch_input(&image, pointer(20., None)).unwrap();
    assert_eq!(
        host.dispatch_text(&a_text, "geometry").unwrap(),
        Result::Deferred
    );
    assert!(
        !drain(&first)
            .iter()
            .any(|event| matches!(event, Event::Text(_)))
    );
    display(&mut host, 1);
    assert_eq!(
        host.dispatch_text(&a_text, "geometry").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&first, 8), "geometry");
    presenter.revoke_pane(1);
    assert_eq!(
        host.dispatch_text(&a_text, "retired").unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_text(&b_text, "healthy peer").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&peer, 12), "healthy peer");
}

fn clipboard_request(
    presenter: &VirtualVivid,
    host: &mut NativeVectorHost,
) -> vivid_sdk::presenter::OverlayClipboardWrite {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(request) = host.poll() {
            assert_eq!(
                request.record_type,
                vivid_protocol::messages::SET_OVERLAY_CLIPBOARD
            );
            let write = presenter.overlay_clipboard_write(request.id).unwrap();
            assert!(
                presenter.overlay_clipboard_write(request.id).is_err(),
                "a write request can only be claimed once"
            );
            return write;
        }
        assert!(
            Instant::now() < deadline,
            "clipboard request never reached the platform host"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn native_clipboard_writes_complete_only_after_platform_execution_and_abandonment_is_correlated() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    let target = display(&mut host, 2);
    assert!(a.set_clipboard("no gesture").is_err());
    host.dispatch_input(&target, key(true, false, None))
        .unwrap();
    host.dispatch_input(&target, key(false, false, None))
        .unwrap();
    assert!(b.set_clipboard("wrong owner").is_err());
    assert!(a.set_clipboard("").is_err());
    assert!(a.set_clipboard(&"x".repeat(65_537)).is_err());
    std::thread::scope(|scope| {
        let job = scope.spawn(|| a.set_clipboard("copied λ 👋"));
        let write = clipboard_request(&presenter, &mut host);
        assert!(
            !job.is_finished(),
            "admission cannot acknowledge a platform write"
        );
        assert!(!format!("{write:?}").contains("copied λ"));
        let mut copied = String::new();
        write
            .perform(|text| {
                copied.push_str(text);
                Ok(())
            })
            .unwrap();
        job.join().unwrap().unwrap();
        assert_eq!(copied, "copied λ 👋");
        let job = scope.spawn(|| a.set_clipboard("abandoned"));
        drop(clipboard_request(&presenter, &mut host));
        assert!(job.join().unwrap().is_err());
        let job = scope.spawn(|| a.set_clipboard("platform refused"));
        let write = clipboard_request(&presenter, &mut host);
        assert!(
            write
                .perform(|_| Err(std::io::Error::other("unavailable")))
                .is_err()
        );
        assert!(job.join().unwrap().is_err());
    });
}

#[test]
fn queued_clipboard_write_cannot_use_a_new_gesture_after_a_focus_round_trip() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    submit(&a, HitRole::Input);
    a.request_focus().unwrap();
    let target = display(&mut host, 1);
    host.dispatch_input(&target, key(true, false, None))
        .unwrap();
    host.dispatch_input(&target, key(false, false, None))
        .unwrap();
    std::thread::scope(|scope| {
        let job = scope.spawn(|| a.set_clipboard("old focus"));
        let write = clipboard_request(&presenter, &mut host);
        presenter.set_overlay_pane_focus(1, false);
        host.invalidate(1);
        presenter.set_overlay_pane_focus(1, true);
        let target = display(&mut host, 1);
        host.dispatch_input(&target, key(true, false, None))
            .unwrap();
        host.dispatch_input(&target, key(false, false, None))
            .unwrap();
        assert!(
            write
                .perform(|_| panic!("old clipboard grant reached the platform"))
                .is_err()
        );
        assert!(job.join().unwrap().is_err());
        let job = scope.spawn(|| a.set_clipboard("new focus"));
        clipboard_request(&presenter, &mut host)
            .perform(|text| {
                assert_eq!(text, "new focus");
                Ok(())
            })
            .unwrap();
        job.join().unwrap().unwrap();
    });
}

#[test]
fn revoking_a_queued_clipboard_owner_preserves_a_peer_with_the_same_local_ids() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 16, 16, (8, 8));
    let first = connect(&presenter);
    let peer = connect_at(&presenter, 2);
    let a = window(&first, 0.);
    let b = window(&peer, 0.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    a.request_focus().unwrap();
    b.request_focus().unwrap();
    let a_target = display(&mut host, 1);
    let b_target = display_at(&mut host, 2, 1);
    for target in [&a_target, &b_target] {
        host.dispatch_input(target, key(true, false, None)).unwrap();
        host.dispatch_input(target, key(false, false, None))
            .unwrap();
    }
    std::thread::scope(|scope| {
        let first_job = scope.spawn(|| a.set_clipboard("retired"));
        let a_write = clipboard_request(&presenter, &mut host);
        let peer_job = scope.spawn(|| b.set_clipboard("healthy peer"));
        let b_write = clipboard_request(&presenter, &mut host);
        presenter.revoke_pane(1);
        assert!(
            a_write
                .perform(|_| panic!("revoked owner wrote to the clipboard"))
                .is_err()
        );
        assert!(first_job.join().unwrap().is_err());
        b_write
            .perform(|text| {
                assert_eq!(text, "healthy peer");
                Ok(())
            })
            .unwrap();
        peer_job.join().unwrap().unwrap();
    });
}

#[test]
fn queued_clipboard_write_expires_without_blocking_native_input_or_host_polling() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    submit(&a, HitRole::Input);
    a.request_focus().unwrap();
    let target = display(&mut host, 1);
    host.dispatch_input(&target, key(true, false, None))
        .unwrap();
    host.dispatch_input(&target, key(false, false, None))
        .unwrap();
    std::thread::scope(|scope| {
        let job = scope.spawn(|| a.set_clipboard("expired"));
        let write = clipboard_request(&presenter, &mut host);
        let until = Instant::now() + Duration::from_millis(2_050);
        while Instant::now() < until {
            host.poll();
            host.flush_input();
            std::thread::sleep(Duration::from_millis(5));
        }
        // A newer gesture in the same focus grant cannot extend an already queued write.
        host.dispatch_input(&target, key(true, false, None))
            .unwrap();
        host.dispatch_input(&target, key(false, false, None))
            .unwrap();
        assert!(
            write
                .perform(|_| panic!("expired gesture reached the clipboard"))
                .is_err()
        );
        assert!(job.join().unwrap().is_err());
    });
}

#[test]
fn closing_only_the_input_lane_revokes_a_queued_write_before_the_root_can_close() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    submit(&a, HitRole::Input);
    a.request_focus().unwrap();
    let frame = host.prepare(1, 1.).unwrap();
    assert!(host.presented(&frame).unwrap());
    let target = host.input_target(&frame).unwrap();
    host.dispatch_input(&target, key(true, false, None))
        .unwrap();
    host.dispatch_input(&target, key(false, false, None))
        .unwrap();
    std::thread::scope(|scope| {
        let job = scope.spawn(|| a.set_clipboard("closed lane"));
        let write = clipboard_request(&presenter, &mut host);
        let close = scope.spawn(|| first.close());
        let deadline = Instant::now() + Duration::from_secs(3);
        while host.input_target(&frame).is_some() {
            assert!(
                Instant::now() < deadline,
                "input-only close did not retire its display context"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            host.prepare(1, 1.).unwrap().drawings().len(),
            1,
            "the root retains pixels while its correlated clipboard request is pending"
        );
        assert!(
            write
                .perform(|_| panic!("closed input lane wrote to the clipboard"))
                .is_err()
        );
        assert!(job.join().unwrap().is_err());
        close.join().unwrap().unwrap();
    });
}

#[test]
fn raster_only_sessions_do_not_join_vector_display_or_input_authority() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let initial = host.prepare(1, 1.).unwrap();
    let secret = presenter.issue_pane_capability(1).unwrap();
    let raster = vivid_sdk::Session::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap();
    assert!(
        !raster
            .info()
            .accepted_profiles
            .iter()
            .any(|profile| profile == vivid_protocol::registry::VECTOR_SCENE)
    );
    let mut raster = vivid_sdk::PaneSession::from_session(raster).unwrap();
    let pixels = [0x12, 0x34, 0x56, 0xff].repeat(4);
    raster.show_rgba(2, 2, &pixels).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if presenter.capture_pane(1, 0).layers.iter().any(|layer| {
            matches!(&layer.content, vivid_sdk::presenter::CaptureContent::Raster(raster) if raster.pixels.as_ref() == pixels)
        }) { break; }
        assert!(
            Instant::now() < deadline,
            "peer raster pixels never became capturable"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        host.presented(&initial).unwrap(),
        "a raster-only session does not change vector topology"
    );
    let target = host.input_target(&initial).unwrap();
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, true)),
                ..
            }
        )
    });
    let current = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert!(presenter.capture_pane(1, 0).layers.iter().any(|layer| {
        matches!(&layer.content, vivid_sdk::presenter::CaptureContent::Raster(raster) if raster.pixels.as_ref() == pixels)
    }), "vector input cannot alter a peer's retained raster pixels");
    drop(raster);
    assert_eq!(
        host.dispatch_input(&current, pointer(74., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&peer, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, true)),
                ..
            }
        )
    });
}

#[test]
fn releasing_an_input_grab_refreshes_peer_hover_without_delivering_its_release() {
    for explicit_capture in [false, true] {
        release_refreshes_hover(explicit_capture);
    }
}
fn release_refreshes_hover(explicit_capture: bool) {
    use vivid_sdk::overlay::CursorShape;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit_canvas(&a, scene_cursor(HitRole::Input, Some(CursorShape::Move)));
    submit_canvas(&b, scene_cursor(HitRole::Input, Some(CursorShape::Text)));
    let target = display(&mut host, 2);
    host.dispatch_input(&target, pointer(10., Some((0, true))))
        .unwrap();
    display(&mut host, 2);
    if explicit_capture {
        first.capture_pointer(&a, true).unwrap();
    }
    assert_eq!(host.cursor(1), Some(CursorShape::Move));
    drain(&first);
    drain(&peer);
    assert_eq!(
        host.dispatch_input(&target, pointer(74., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, false)),
                region,
                ..
            } if *region == if explicit_capture { 0 } else { 7 }
        )
    });
    let peer_events = drain(&peer);
    assert!(peer_events.iter().any(|event| matches!(
        event,
        Event::Hover {
            entered: true,
            region: 7
        }
    )));
    assert_eq!(host.cursor(1), Some(CursorShape::Text));
    assert!(
        !peer_events
            .iter()
            .any(|event| matches!(event, Event::Pointer { .. })),
        "refreshing hover cannot deliver another owner's release or motion"
    );
}

#[test]
fn settled_drag_restores_stationary_hover_only_after_its_new_placement_is_displayed() {
    use vivid_sdk::overlay::CursorShape;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit_canvas(&a, scene_cursor(HitRole::Drag, Some(CursorShape::Move)));
    submit_canvas(&b, scene_cursor(HitRole::Input, Some(CursorShape::Text)));
    let target = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(20., None)).unwrap(),
        Result::Delivered { text: false }
    );
    display(&mut host, 2);
    drain(&first);
    assert_eq!(
        host.dispatch_input(&target, pointer(20., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert!(!host.has_pointer_grab(1));
    assert_eq!(host.cursor(1), None);
    let before_display = drain(&first);
    assert!(
        before_display
            .iter()
            .any(|event| matches!(event, Event::Geometry { settled: true, .. }))
    );
    assert!(
        !before_display
            .iter()
            .any(|event| matches!(event, Event::Hover { entered: true, .. }))
    );
    display(&mut host, 2);
    expect(&first, |event| {
        matches!(
            event,
            Event::Hover {
                entered: true,
                region: 7
            }
        )
    });
    assert_eq!(host.cursor(1), Some(CursorShape::Move));
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Pointer { .. }))
    );
}

#[test]
fn displayed_repaint_refreshes_stationary_hover_using_only_accepted_positions() {
    use vivid_sdk::overlay::CursorShape;
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit_canvas(&a, scene_cursor(HitRole::Input, Some(CursorShape::Move)));
    submit_canvas(&b, scene_cursor(HitRole::Input, Some(CursorShape::Text)));
    let target = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., None)).unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(host.cursor(1), Some(CursorShape::Move));
    drain(&first);
    drain(&peer);
    host.retire_input_target(&target);
    assert_eq!(
        host.dispatch_input(&target, pointer(74., None)).unwrap(),
        Result::Rejected
    );
    submit_canvas(
        &a,
        scene_cursor(HitRole::Input, Some(CursorShape::Crosshair)),
    );
    assert_eq!(
        host.cursor(1),
        Some(CursorShape::Move),
        "accepted upload cannot change the displayed cursor"
    );
    display(&mut host, 2);
    assert_eq!(host.cursor(1), Some(CursorShape::Crosshair));
    assert!(!drain(&peer).iter().any(|event| matches!(
        event,
        Event::Hover { entered: true, .. } | Event::Pointer { .. }
    )));
}
#[test]
fn held_reports_survive_repaint_without_retargeting_or_retaining_frame_receipts() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let original = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&original, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, true)),
                region: 7,
                ..
            }
        )
    });
    submit(&a, HitRole::Input);
    host.retire_input_target(&original);
    let current = display(&mut host, 2);
    assert!(host.retains_input_target(&original));
    assert_eq!(
        host.dispatch_input(&original, pointer(74., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(
        &first,
        |event| matches!(event, Event::Pointer { button: Some((0, false)), position, region: 7, .. } if position.x.get() == 74.),
    );
    assert!(!host.retains_input_target(&original));
    assert!(!drain(&peer).iter().any(|event| matches!(
        event,
        Event::Pointer {
            button: Some(_),
            ..
        }
    )));
    assert_eq!(
        host.dispatch_input(&original, key(true, false, Some("wrong")))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&current, key(true, false, Some("a")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &first,
        |event| matches!(event, Event::Text(text) if text == "a"),
    );
    // More than two images prove held bindings consume no frame-receipt slots.
    for _ in 0..4 {
        submit(&a, HitRole::Input);
        let _ = display(&mut host, 2);
    }
    host.retire_input_target(&current);
    assert_eq!(
        host.dispatch_input(&current, key(true, true, Some("a")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Key {
                physical: 4,
                repeat: true,
                ..
            }
        )
    });
    expect(
        &first,
        |event| matches!(event, Event::Text(text) if text == "a"),
    );
    assert_eq!(
        host.dispatch_input(&current, key(false, false, None))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Key {
                physical: 4,
                down: false,
                ..
            }
        )
    });
    assert!(!host.retains_input_target(&current));
    presenter.shutdown();
}
#[test]
fn revoked_or_foreign_targets_cannot_inject_held_text_into_a_peer() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let target = display(&mut host, 2);
    a.request_focus().unwrap();
    let target = {
        let _ = target;
        display(&mut host, 2)
    };
    assert_eq!(
        host.dispatch_input(&target, key(true, false, Some("a")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &first,
        |event| matches!(event, Event::Text(text) if text == "a"),
    );
    b.request_focus().unwrap();
    let peer_target = display(&mut host, 2);
    let cancelled = drain(&first)
        .iter()
        .filter(|event| matches!(event, Event::Cancel))
        .count();
    assert_eq!(
        cancelled, 1,
        "one focus loss must not duplicate cancellation on presentation"
    );
    host.dispatch_input(&peer_target, pointer(74., None))
        .unwrap();
    assert!(
        !drain(&first)
            .iter()
            .any(|event| matches!(event, Event::Cancel)),
        "a later pointer report must not deliver a second queued cancellation"
    );
    assert_eq!(
        host.dispatch_input(&target, key(true, true, Some("bad")))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&target, key(false, false, None))
            .unwrap(),
        Result::Rejected
    );
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Key { .. } | Event::Text(_)))
    );
    assert_eq!(
        host.dispatch_input(&peer_target, key(true, false, Some("b")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &peer,
        |event| matches!(event, Event::Text(text) if text == "b"),
    );
    let (_, mut foreign) = fixture();
    assert_eq!(
        foreign
            .dispatch_input(&peer_target, key(false, false, None))
            .unwrap(),
        Result::Rejected
    );
    host.invalidate(1);
    assert_eq!(
        host.dispatch_input(&peer_target, key(true, true, Some("bad")))
            .unwrap(),
        Result::Rejected
    );
    assert!(
        host.dispatch_input(&peer_target, Input::Text("x".repeat(4097)))
            .is_err()
    );
    presenter.shutdown();
}
#[test]
fn drag_defers_ordered_motion_and_release_until_matching_geometry_is_displayed() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    submit(&a, HitRole::Drag);
    let target = display(&mut host, 1);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(20., None)).unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(30., None)).unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(40., Some((0, false))))
            .unwrap(),
        Result::Deferred
    );
    host.retire_input_target(&target);
    let _ = display(&mut host, 1);
    assert_eq!(host.flush_input(), 1);
    assert!(host.retains_input_target(&target));
    let _ = display(&mut host, 1);
    assert_eq!(host.flush_input(), 1);
    assert!(!host.retains_input_target(&target));
    let frame = host.prepare(1, 1.).unwrap();
    assert_eq!(frame.drawings()[0].bounds.origin.x.get(), 30.);
    assert_eq!(a.reconcile().unwrap().accepted_revision, 1);
    presenter.shutdown();
}

#[test]
fn track_replacement_revokes_old_holds_and_preserves_the_peer_with_reused_ids() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let target = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    let shown = display(&mut host, 2);
    let old_text = host.text_target(&shown).unwrap();
    let replacement = a.replace_track(scene(HitRole::Input)).unwrap();
    assert_eq!(
        a.reconcile().unwrap().accepted_revision,
        replacement.revision()
    );
    let current = display(&mut host, 2);
    assert_eq!(
        host.dispatch_text(&old_text, "retired track").unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, false))))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&current, pointer(74., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    let current = display(&mut host, 2);
    let peer_text = host.text_target(&current).unwrap();
    assert_eq!(
        host.dispatch_input(&current, key(true, false, Some("b")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &peer,
        |event| matches!(event, Event::Text(text) if text == "b"),
    );
    assert!(!drain(&first).iter().any(|event| matches!(
        event,
        Event::Pointer {
            button: Some((0, false)),
            ..
        } | Event::Text(_)
    )));
    first.close().unwrap();
    let _ = display(&mut host, 1);
    assert_eq!(
        host.dispatch_text(&peer_text, "peer survives").unwrap(),
        Result::Delivered { text: true }
    );
    assert_eq!(received_text(&peer, 13), "peer survives");
    assert!(host.retains_input_target(&current));
    assert_eq!(
        host.dispatch_input(&current, key(false, false, None))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&peer, |event| {
        matches!(
            event,
            Event::Key {
                physical: 4,
                down: false,
                ..
            }
        )
    });
    presenter.shutdown();
}

#[test]
fn malformed_reports_preserve_held_state_and_deferred_exhaustion_revokes_it() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    submit(&a, HitRole::Input);
    let target = display(&mut host, 1);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert!(
        host.dispatch_input(
            &target,
            Input::Pointer {
                position: [10., 10.],
                button: Some((0, false)),
                modifiers: u32::MAX
            }
        )
        .is_err()
    );
    assert!(host.retains_input_target(&target));
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    submit(&a, HitRole::Drag);
    let target = display(&mut host, 1);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(20., None)).unwrap(),
        Result::Delivered { text: false }
    );
    for _ in 0..64 {
        assert_eq!(
            host.dispatch_input(&target, pointer(30., Some((1, true))))
                .unwrap(),
            Result::Deferred
        );
    }
    assert_eq!(
        host.dispatch_input(&target, pointer(30., Some((0, false))))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!host.retains_input_target(&target));
    assert_eq!(
        host.dispatch_input(&target, pointer(30., Some((0, false))))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(host.flush_input(), 0);
    presenter.shutdown();
}

#[test]
fn resize_keeps_its_original_source_while_release_waits_for_display() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    // Bit 2 is the protocol's right edge.
    submit(&a, HitRole::Resize(4));
    let target = display(&mut host, 1);
    assert_eq!(
        host.dispatch_input(&target, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(20., None)).unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target, pointer(30., Some((0, false))))
            .unwrap(),
        Result::Deferred
    );
    let _ = display(&mut host, 1);
    assert_eq!(host.flush_input(), 1);
    assert!(!host.retains_input_target(&target));
    let frame = host.prepare(1, 1.).unwrap();
    assert_eq!(frame.drawings()[0].bounds.width.get(), 68.);
    assert_eq!(a.reconcile().unwrap().accepted_revision, 1);
    presenter.shutdown();
}

#[test]
fn dismissed_popup_keeps_its_original_swallowed_release_after_repaint() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let b = window(&peer, 64.);
    let a = first
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 48., 48.).unwrap(),
            WindowMode::Popup,
        ))
        .unwrap();
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let target = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, pointer(74., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| matches!(event, Event::Dismissed(_)));
    host.retire_input_target(&target);
    let _ = display(&mut host, 1);
    assert!(host.retains_input_target(&target));
    assert_eq!(
        host.dispatch_input(&target, pointer(74., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    assert!(!host.retains_input_target(&target));
    assert!(!drain(&peer).iter().any(|event| matches!(
        event,
        Event::Pointer {
            button: Some(_),
            ..
        }
    )));
    presenter.shutdown();
}

#[test]
fn focus_between_windows_of_one_owner_cancels_held_input_once() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&first, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let _ = display(&mut host, 2);
    a.request_focus().unwrap();
    let target = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&target, key(true, false, Some("a")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &first,
        |event| matches!(event, Event::Text(text) if text == "a"),
    );
    b.request_focus().unwrap();
    let _ = display(&mut host, 2);
    assert_eq!(
        drain(&first)
            .iter()
            .filter(|event| matches!(event, Event::Cancel))
            .count(),
        1
    );
    presenter.shutdown();
}

#[test]
fn panes_isolate_modals_capture_held_keys_click_counts_and_cursor_metadata() {
    use vivid_sdk::overlay::CursorShape;
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 16, 16, (8, 8));
    let first = connect_at(&presenter, 1);
    let neighbor = connect_at(&presenter, 1);
    let peer = connect_at(&presenter, 2);
    let sibling = window(&neighbor, 64.);
    let a = first
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 48., 48.).unwrap(),
            WindowMode::Modal,
        ))
        .unwrap();
    let b = window(&peer, 0.);
    submit_canvas(&a, scene_cursor(HitRole::Drag, Some(CursorShape::Move)));
    submit(&sibling, HitRole::Input);
    submit_canvas(&b, scene_cursor(HitRole::Input, Some(CursorShape::Text)));
    let target_a = display_at(&mut host, 1, 2);
    let target_b = display_at(&mut host, 2, 1);
    let drawing_a = host.prepare(1, 1.).unwrap();
    let drawing_b = host.prepare(2, 1.).unwrap();
    assert_eq!(
        drawing_a.drawings()[0].submission.address,
        drawing_b.drawings()[0].submission.address,
        "panes deliberately reuse all local window identity fields"
    );
    assert_ne!(
        drawing_a.drawings()[0].window.context().session(),
        drawing_b.drawings()[0].window.context().session()
    );
    drop((drawing_a, drawing_b));
    assert!(
        sibling.request_focus().is_err(),
        "the modal must still protect its own pane"
    );
    assert_eq!(
        host.dispatch_input(&target_a, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&first, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, true)),
                clicks: 1,
                ..
            }
        )
    });
    assert!(host.has_pointer_grab(1));
    assert!(!host.has_pointer_grab(2));
    assert_eq!(
        host.dispatch_input(&target_b, pointer(10., Some((0, true))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&peer, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, true)),
                clicks: 1,
                ..
            }
        )
    });
    assert!(host.has_pointer_grab(2));
    assert_eq!(host.cursor(1), Some(CursorShape::Move));
    assert_eq!(host.cursor(2), Some(CursorShape::Text));
    for (target, session, text) in [(&target_a, &first, "a"), (&target_b, &peer, "b")] {
        assert_eq!(
            host.dispatch_input(target, key(true, false, Some(text)))
                .unwrap(),
            Result::Delivered { text: true }
        );
        expect(
            session,
            |event| matches!(event, Event::Text(value) if value == text),
        );
    }
    assert_eq!(
        host.dispatch_input(&target_a, pointer(20., None)).unwrap(),
        Result::Delivered { text: false }
    );
    assert_eq!(
        host.dispatch_input(&target_a, pointer(30., Some((0, false))))
            .unwrap(),
        Result::Deferred
    );
    assert_eq!(
        host.dispatch_input(&target_b, key(true, true, Some("b")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &peer,
        |event| matches!(event, Event::Text(value) if value == "b"),
    );
    presenter.set_overlay_pane_focus(1, false);
    host.invalidate(1);
    a.request_focus().unwrap();
    assert!(
        !presenter.overlay_has_focus(1),
        "an inactive pane must not reacquire actual focus from a producer request"
    );
    let inactive = display_at(&mut host, 1, 2);
    assert_eq!(
        host.dispatch_input(&inactive, key(true, false, Some("bad")))
            .unwrap(),
        Result::Rejected
    );
    assert!(host.retains_input_target(&target_b));
    assert_eq!(
        host.dispatch_input(&target_b, pointer(10., Some((0, false))))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&peer, |event| {
        matches!(
            event,
            Event::Pointer {
                button: Some((0, false)),
                ..
            }
        )
    });
    a.close().unwrap();
    assert_eq!(
        host.dispatch_input(&target_b, key(false, false, None))
            .unwrap(),
        Result::Delivered { text: false }
    );
    expect(&peer, |event| {
        matches!(
            event,
            Event::Key {
                physical: 4,
                down: false,
                ..
            }
        )
    });
    assert!(!host.retains_input_target(&target_b));
    assert!(
        !drain(&peer)
            .iter()
            .any(|event| matches!(event, Event::Cancel | Event::Focus(false)))
    );
    presenter.shutdown();
}

#[test]
fn rapid_window_focus_return_does_not_restore_a_cancelled_held_key() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&first, 64.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let _ = display(&mut host, 2);
    a.request_focus().unwrap();
    let original = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&original, key(true, false, Some("a")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &first,
        |event| matches!(event, Event::Text(value) if value == "a"),
    );
    b.request_focus().unwrap();
    a.request_focus().unwrap();
    let _ = display(&mut host, 2);
    assert_eq!(
        host.dispatch_input(&original, key(true, true, Some("bad")))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&original, key(false, false, None))
            .unwrap(),
        Result::Rejected
    );
    assert!(
        !drain(&first)
            .iter()
            .any(|event| matches!(event, Event::Text(value) if value == "bad"))
    );
    presenter.shutdown();
}

#[test]
fn visibility_controller_cannot_revive_an_old_input_target_after_a_fresh_display() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 16, 16, (8, 8));
    let first = connect_at(&presenter, 1);
    let peer = connect_at(&presenter, 2);
    let a = window(&first, 0.);
    let b = window(&peer, 0.);
    submit(&a, HitRole::Input);
    submit(&b, HitRole::Input);
    let _ = display_at(&mut host, 1, 1);
    let _ = display_at(&mut host, 2, 1);
    a.request_focus().unwrap();
    b.request_focus().unwrap();
    let old = display_at(&mut host, 1, 1);
    let neighbor = display_at(&mut host, 2, 1);
    for (target, session, text) in [(&old, &first, "a"), (&neighbor, &peer, "b")] {
        assert_eq!(
            host.dispatch_input(target, key(true, false, Some(text)))
                .unwrap(),
            Result::Delivered { text: true }
        );
        expect(
            session,
            |event| matches!(event, Event::Text(value) if value == text),
        );
    }
    let old_text = host.text_target(&old).unwrap();
    let control = host.visibility_control(1).unwrap();
    assert!(control.set_visible(false).unwrap());
    assert!(control.set_visible(true).unwrap());
    let fresh = host.prepare(1, 1.).unwrap();
    assert!(host.presented(&fresh).unwrap());
    // No new input binding has been acquired: admitting identical pixels must
    // not silently restore authority to the pre-hide cached binding.
    assert_eq!(
        host.dispatch_input(&old, key(true, true, Some("bad")))
            .unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&neighbor, key(true, true, Some("b")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &peer,
        |event| matches!(event, Event::Text(value) if value == "b"),
    );
    let current = host.input_target(&fresh).unwrap();
    let current_text = host.text_target(&current).unwrap();
    assert!(!old_text.same_focus(&current_text));
    assert_eq!(
        host.dispatch_text(&old_text, "bad text").unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&current, key(true, false, Some("fresh")))
            .unwrap(),
        Result::Delivered { text: true }
    );
    expect(
        &first,
        |event| matches!(event, Event::Text(value) if value == "fresh"),
    );
    presenter.shutdown();
}
