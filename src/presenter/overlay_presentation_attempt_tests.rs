//! Pending capture identity through pressure, replacement and retained replay.

// Rust guideline compliant 2026-10-07
use super::tests::{accepted, captured, connect, key};
use super::*;
use crate::presenter::{MediaConfig, PresenterConfig, SocketListener};
use crate::{OverlaySession, OverlayWindow, OverlayWindowOptions};
use vivid_protocol::vector::{Canvas, Rect};

fn fixture() -> (
    VirtualVivid,
    OverlayPresentationRoute,
    OverlaySession,
    OverlaySession,
    OverlayWindow,
    OverlayWindow,
) {
    let presenter = VirtualVivid::start_configured_eventless(
        SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
        PresenterConfig::terminal_with_overlay(MediaConfig::default()),
    )
    .unwrap();
    presenter.enable_overlay_host_relay();
    let route = presenter.attach_overlay_presentation_relay().unwrap();
    let first = connect(&presenter, 1);
    let second = connect(&presenter, 2);
    let options = || {
        OverlayWindowOptions::new(
            Rect::new(0., 0., 48., 48.).unwrap(),
            vivid_protocol::overlay::WindowMode::Floating,
        )
    };
    let a = first.create_window(options()).unwrap();
    let b = second.create_window(options()).unwrap();
    (presenter, route, first, second, a, b)
}
fn pair(presenter: &VirtualVivid) -> (BridgeSourceKey, BridgeSourceKey) {
    let a = key(presenter, 1);
    let b = key(presenter, 2);
    assert_eq!(
        (a.context, a.surface, a.track),
        (b.context, b.surface, b.track)
    );
    assert_ne!(a.producer, b.producer);
    (a, b)
}
fn attempt(
    route: &OverlayPresentationRoute,
    source: BridgeSourceKey,
    revision: u64,
) -> OverlayPresentationAttempt {
    route.begin_capture(source, 1, revision).unwrap().unwrap()
}
fn forward(attempt: &OverlayPresentationAttempt) -> OverlayPresentationReceipt {
    match attempt.capture().unwrap() {
        OverlayPresentationCapture::Forwarded(receipt) => receipt,
        other => panic!("expected original copy, got {other:?}"),
    }
}
#[test]
fn blocked_attempt_preserves_pending_then_retries_without_stealing_peer_credit() {
    let (presenter, route, first, second, a, b) = fixture();
    let one = a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let held = (0..4)
        .map(|_| captured(&route, ka, 1, 1))
        .collect::<Vec<_>>();
    let two = a.submit(Canvas::new()).unwrap();
    accepted(&a, 2);
    let target = attempt(&route, ka, 2);
    assert!(target.same_original(&attempt(&route, ka, 2)));
    assert!(!target.same_original(&attempt(&route, kb, 1)));
    assert_eq!(
        target.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(two.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(
        lock(&route.credits).total,
        4,
        "a waiting target charged a fifth copy"
    );
    assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
    assert_eq!(
        peer.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    for receipt in held {
        receipt.complete(PresentationOutcome::Superseded);
    }
    assert_eq!(
        one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert!(forward(&target).complete(PresentationOutcome::Presented));
    assert_eq!(
        two.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert!(matches!(
        target.capture().unwrap(),
        OverlayPresentationCapture::Replay
    ));
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}
#[test]
fn superseded_attempt_cannot_reserve_its_successor() {
    let (presenter, route, first, second, a, b) = fixture();
    a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let held = (0..4)
        .map(|_| captured(&route, ka, 1, 1))
        .collect::<Vec<_>>();
    let old = a.submit(Canvas::new()).unwrap();
    accepted(&a, 2);
    let stale = attempt(&route, ka, 2);
    assert_eq!(
        stale.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let fresh = a.submit(Canvas::new()).unwrap();
    accepted(&a, 3);
    assert!(matches!(
        stale.capture().unwrap(),
        OverlayPresentationCapture::Discard
    ));
    assert_eq!(
        stale.origin.strong_count(),
        0,
        "target retained obsolete scene storage"
    );
    assert_eq!(
        old.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(fresh.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    for receipt in held {
        receipt.complete(PresentationOutcome::Superseded);
    }
    assert!(forward(&attempt(&route, ka, 3)).complete(PresentationOutcome::Presented));
    assert_eq!(
        fresh.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}
#[test]
fn role_recreation_with_identical_submission_does_not_rebind_old_attempt_or_receipt() {
    let (presenter, route, first, second, a, b) = fixture();
    let old = a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let target = attempt(&route, ka, 1);
    let stale_copy = forward(&target);
    let held = (0..3).map(|_| forward(&target)).collect::<Vec<_>>();
    assert_eq!(
        target.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let submission = target.submission;
    // Model role retirement/reinstatement directly at the window-host layer.
    // A producer cannot send a duplicate live media revision. The source,
    // surface generation, channel, epoch and revision deliberately all repeat;
    // only the window/scene occurrence changes. No duplicate wire record is sent.
    {
        let mut state = lock(&presenter.state);
        let id = window(ka).unwrap();
        state.overlays.close_surface(id);
        let request = vivid_protocol::overlay::wire::SetWindow {
            address: submission.address,
            expected_revision: 0,
            options: vivid_protocol::overlay::WindowOptions::new(
                Rect::new(0., 0., 48., 48.).unwrap(),
                vivid_protocol::overlay::WindowMode::Floating,
            ),
        };
        state
            .overlays
            .set_window(
                ka.producer,
                ka.surface,
                &request.payload(id.context().session()).unwrap(),
            )
            .unwrap();
        let body = vivid_protocol::vector::Frame {
            epoch: 1,
            revision: 1,
            canvas: Canvas::new(),
        }
        .encode()
        .unwrap();
        assert!(state.overlays.media_record_for(
            id,
            messages::VECTOR_FRAME,
            ka.track,
            submission.channel_generation,
            &body
        ));
    }
    assert!(matches!(
        target.capture().unwrap(),
        OverlayPresentationCapture::Discard
    ));
    assert!(!stale_copy.complete(PresentationOutcome::Presented));
    for receipt in held {
        assert!(!receipt.complete(PresentationOutcome::Presented));
    }
    assert_eq!(
        old.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    let fresh = attempt(&route, ka, 1);
    assert_eq!(fresh.submission, target.submission);
    assert!(!fresh.same_original(&target));
    assert!(!Weak::ptr_eq(&fresh.origin, &target.origin));
    assert_eq!(
        lock(&presenter.state)
            .overlays
            .window(window(ka).unwrap())
            .unwrap()
            .scene_revision,
        0
    );
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    assert!(forward(&fresh).complete(PresentationOutcome::Presented));
    assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}
#[test]
fn blocked_attempt_cannot_follow_a_replacement_track_or_cancelled_route() {
    let (presenter, route, first, second, a, b) = fixture();
    a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let held = (0..4)
        .map(|_| captured(&route, ka, 1, 1))
        .collect::<Vec<_>>();
    let old = a.submit(Canvas::new()).unwrap();
    accepted(&a, 2);
    let stale = attempt(&route, ka, 2);
    assert_eq!(
        stale.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let fresh = a.replace_track(Canvas::new()).unwrap();
    accepted(&a, 3);
    assert!(matches!(
        stale.capture().unwrap(),
        OverlayPresentationCapture::Discard
    ));
    assert_eq!(
        old.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    for receipt in held {
        assert!(!receipt.complete(PresentationOutcome::Presented));
    }
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(fresh.wait(Duration::ZERO).unwrap(), None);
    assert!(captured(&route, key(&presenter, 1), 1, 3).complete(PresentationOutcome::Presented));
    let peer_target = attempt(&route, kb, 1);
    route.cancel_handle()();
    assert_eq!(
        peer_target.capture().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}

#[test]
fn channel_advance_fences_a_blocked_attempt_before_deferred_cleanup() {
    let (presenter, route, first, second, a, b) = fixture();
    a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let held = (0..4)
        .map(|_| captured(&route, ka, 1, 1))
        .collect::<Vec<_>>();
    let pending = a.submit(Canvas::new()).unwrap();
    accepted(&a, 2);
    let stale = attempt(&route, ka, 2);
    assert_eq!(
        stale.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    // Model the interval after authenticated channel advance but before overlay
    // cleanup. The old scene token deliberately remains live throughout.
    let original = {
        let mut state = lock(&presenter.state);
        let track = state.tracks.get_mut(&inner_track_key(ka)).unwrap();
        let original = track.state.channel_generation;
        track.state.channel_generation = ChannelGeneration::new(original.get() + 1);
        original
    };
    assert_eq!(stale.origin.strong_count(), 1);
    assert!(matches!(
        stale.capture().unwrap(),
        OverlayPresentationCapture::Discard
    ));
    assert_eq!(pending.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    lock(&presenter.state)
        .tracks
        .get_mut(&inner_track_key(ka))
        .unwrap()
        .state
        .channel_generation = original;
    for receipt in held {
        receipt.complete(PresentationOutcome::Superseded);
    }
    assert!(forward(&stale).complete(PresentationOutcome::Presented));
    assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}

#[test]
fn leased_resume_retires_blocked_attempt_without_touching_same_id_peer() {
    use crate::{CleanupPolicy, ProducerAuthentication, ProducerConfig, SessionLeaseBuilder};
    use vivid_protocol::resource::{RESOURCE_COUNT, ResourceContract};
    let presenter = VirtualVivid::start_configured_eventless(
        SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
        PresenterConfig::terminal_with_overlay(MediaConfig::default()),
    )
    .unwrap();
    presenter.enable_overlay_host_relay();
    let route = presenter.attach_overlay_presentation_relay().unwrap();
    presenter.update_metrics(1, 80, 24, (8, 16));
    let (definition, mut activation) = SessionLeaseBuilder::new(1, 7)
        .permitted_profiles(
            [
                crate::CORE_CONTROL,
                crate::LIVE_MEDIA,
                crate::OBSERVABILITY,
                crate::TERMINAL_SURFACE,
                crate::TERMINAL_OVERLAY,
                crate::VECTOR_SCENE,
                crate::OVERLAY_INPUT,
                crate::OVERLAY_PAINT,
                crate::OVERLAY_POINTER,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        )
        .contract(ResourceContract::new([u64::MAX / 4; RESOURCE_COUNT]))
        .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
        .disconnect_grace_us(10_000_000)
        .build()
        .unwrap();
    presenter.issue_lease(1, definition).unwrap();
    let config = || ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        ..ProducerConfig::default()
    };
    let mut initial = config();
    initial.authentication =
        ProducerAuthentication::lease_activation_bytes(1, 7, activation.take().unwrap()).unwrap();
    let mut first = OverlaySession::connect(initial).unwrap();
    let second = connect(&presenter, 2);
    let options = || {
        OverlayWindowOptions::new(
            Rect::new(0., 0., 48., 48.).unwrap(),
            vivid_protocol::overlay::WindowMode::Floating,
        )
    };
    let mut a = first.create_window(options()).unwrap();
    let b = second.create_window(options()).unwrap();
    a.submit(Canvas::new()).unwrap();
    let peer = b.submit(Canvas::new()).unwrap();
    accepted(&a, 1);
    accepted(&b, 1);
    let (ka, kb) = pair(&presenter);
    let held = (0..4)
        .map(|_| captured(&route, ka, 1, 1))
        .collect::<Vec<_>>();
    let old = a.submit(Canvas::new()).unwrap();
    accepted(&a, 2);
    let stale = attempt(&route, ka, 2);
    assert_eq!(
        stale.capture().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    first.resume(config(), &mut [&mut a]).unwrap();
    assert!(matches!(
        stale.capture().unwrap(),
        OverlayPresentationCapture::Discard
    ));
    assert!(old.wait(Duration::ZERO).is_err());
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    for receipt in held {
        assert!(!receipt.complete(PresentationOutcome::Presented));
    }
    let fresh = a.submit(Canvas::new()).unwrap();
    accepted(&a, fresh.revision());
    assert_eq!(key(&presenter, 1), ka, "resume changed logical source IDs");
    assert!(captured(&route, ka, 1, fresh.revision()).complete(PresentationOutcome::Presented));
    assert_eq!(
        fresh.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
    assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
    assert_eq!(
        peer.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    a.close().unwrap();
    b.close().unwrap();
    first.close().unwrap();
    second.close().unwrap();
    presenter.shutdown();
}
