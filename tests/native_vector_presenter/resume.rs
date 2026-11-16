//! Complete high-level overlay window adoption across leased reconnects.

// Rust guideline compliant 2026-10-07
use super::*;
use std::time::Instant;
use vivid_protocol::resource::{RESOURCE_COUNT, ResourceContract};
use vivid_sdk::{CleanupPolicy, SessionLeaseBuilder};

pub(super) fn config(presenter: &VirtualVivid) -> ProducerConfig {
    ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        ..ProducerConfig::default()
    }
}

pub(super) fn leased(presenter: &VirtualVivid) -> OverlaySession {
    let (definition, mut activation) = SessionLeaseBuilder::new(1, 7)
        .permitted_profiles(
            [
                vivid_sdk::CORE_CONTROL,
                vivid_sdk::LIVE_MEDIA,
                vivid_sdk::OBSERVABILITY,
                vivid_sdk::TERMINAL_SURFACE,
                vivid_sdk::TERMINAL_OVERLAY,
                vivid_sdk::VECTOR_SCENE,
                vivid_sdk::OVERLAY_INPUT,
                vivid_sdk::OVERLAY_PAINT,
                vivid_sdk::OVERLAY_POINTER,
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
    let mut config = config(presenter);
    config.authentication =
        ProducerAuthentication::lease_activation_bytes(1, 7, activation.take().unwrap()).unwrap();
    OverlaySession::connect(config).unwrap()
}

fn fresh(
    host: &mut NativeVectorHost,
    owner: vivid_protocol::identity::SessionIdentity,
    generation: u64,
    revision: u64,
) -> VectorFrame {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.drawings().len() == 2
            && frame.drawings().iter().any(|drawing| {
                drawing.window.context().session() == owner
                    && drawing.submission.channel_generation == generation
                    && drawing.submission.revision == revision
            })
        {
            return frame;
        }
        assert!(Instant::now() < until, "resumed overlay scene stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn leased_overlay_resume_adopts_windows_and_retires_only_its_own_old_handles() {
    let (presenter, mut host) = fixture();
    let mut first = leased(&presenter);
    let peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let b = window(&peer, 64.);
    let old_image = a.upload_rgba(1, 1, &[255; 4]).unwrap();
    let old = a.submit(scene(0xff0000ff)).unwrap();
    let peer_one = b.submit(scene(0x0000ffff)).unwrap();
    let before = prepare(&mut host, 2);
    let source = &before.drawings()[0];
    let owner = source.window.context().session();
    let address = source.submission.address;
    assert_eq!(address, before.drawings()[1].submission.address);
    let mut generation = source.submission.channel_generation;
    let stale_cancel = first.cancel_handle().unwrap();
    first.resume(config(&presenter), &mut [&mut a]).unwrap();
    stale_cancel();
    assert!(old.wait(Duration::ZERO).is_err());
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    assert!(
        !host
            .reject_drawing(&before, before.drawings()[0].window)
            .unwrap(),
        "an old GPU refusal cannot close an adopted resumed window"
    );
    assert!(a.reconcile().is_ok());
    assert!(!host.presented(&before).unwrap());
    drop(before);
    assert!(
        a.draw_image(
            &mut Canvas::new(),
            &old_image,
            Rect::new(0., 0., 1., 1.).unwrap(),
            u16::MAX
        )
        .is_err()
    );
    for _ in 0..2 {
        generation += 1;
        let receipt = a.submit(scene(0x00ff00ff)).unwrap();
        let frame = fresh(&mut host, owner, generation, receipt.revision());
        assert_eq!(
            frame
                .drawings()
                .iter()
                .find(|d| d.window.context().session() == owner)
                .unwrap()
                .submission
                .address,
            address
        );
        assert!(host.presented(&frame).unwrap());
        assert_eq!(
            receipt.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(
            peer_one.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        drop(frame);
        if generation == 2 {
            first.resume(config(&presenter), &mut [&mut a]).unwrap();
        }
    }
    // The replacement renewal worker must keep this new lane alive past one lease.
    std::thread::sleep(Duration::from_millis(2200));
    a.request_focus().unwrap();
    let extra = window(&first, 0.);
    extra.close().unwrap();
    a.close().unwrap();
    first.close().unwrap();
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    peer.close().unwrap();
}

#[test]
fn resume_preflight_rejects_missing_foreign_and_root_windows_without_retirement() {
    let (presenter, _host) = fixture();
    let mut first = leased(&presenter);
    let mut peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let mut b = window(&peer, 64.);
    assert!(first.resume(config(&presenter), &mut []).is_err());
    assert!(first.resume(config(&presenter), &mut [&mut b]).is_err());
    assert_eq!(
        peer.resume(config(&presenter), &mut [&mut b])
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(a.submit(scene(0xff0000ff)).is_ok());
    assert!(b.submit(scene(0x0000ffff)).is_ok());
    first.resume(config(&presenter), &mut [&mut a]).unwrap();
    assert!(a.submit(scene(0x00ff00ff)).is_ok());
    let mut child = first
        .create_child(
            &a,
            OverlayWindowOptions::new(Rect::new(8., 8., 16., 16.).unwrap(), WindowMode::Floating),
        )
        .unwrap();
    child.present(Canvas::new()).unwrap();
    assert!(first.resume(config(&presenter), &mut [&mut a]).is_err());
    first
        .resume(config(&presenter), &mut [&mut child, &mut a])
        .unwrap();
    assert!(a.submit(scene(0x00ff00ff)).is_ok());
    child.present(Canvas::new()).unwrap();
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn failed_resume_closes_its_wrapper_without_retiring_the_same_id_peer() {
    use vivid_sdk::presenter::PresenterListener as _;
    let (presenter, mut host) = fixture();
    let mut first = leased(&presenter);
    let peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.present(scene(0xff0000ff)).unwrap();
    let peer_one = b.submit(scene(0x0000ffff)).unwrap();
    let frame = prepare(&mut host, 2);
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let unavailable = listener.endpoint();
    drop(listener);
    let mut bad = config(&presenter);
    bad.endpoint_control = Some(unavailable);
    assert!(first.resume(bad, &mut [&mut a]).is_err());
    assert!(a.submit(scene(0x00ff00ff)).is_err());
    assert!(!host.presented(&frame).unwrap());
    drop(frame);
    let peer_frame = prepare(&mut host, 1);
    assert!(host.presented(&peer_frame).unwrap());
    assert_eq!(
        peer_one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[cfg(feature = "testing")]
#[test]
fn host_disconnect_fences_the_complete_source_and_resume_generation() {
    use vivid_sdk::presenter::SourceKey;
    let (presenter, mut host) = fixture();
    let mut first = leased(&presenter);
    let peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.present(scene(0xff0000ff)).unwrap();
    let peer_one = b.submit(scene(0x0000ffff)).unwrap();
    let old = prepare(&mut host, 2);
    let drawing = &old.drawings()[0];
    let owner = drawing.window.context().session();
    let source = SourceKey {
        producer: owner.session_id(),
        context: drawing.submission.address.context_id,
        surface: drawing.submission.address.surface_id,
        track: drawing.submission.track_id,
    };
    let mut wrong = source;
    wrong.track += 1;
    assert!(
        presenter
            .disconnect_leased_source_for_test(wrong, 0, 1)
            .is_err()
    );
    let peer_drawing = &old.drawings()[1];
    let peer_source = SourceKey {
        producer: peer_drawing.window.context().session().session_id(),
        context: peer_drawing.submission.address.context_id,
        surface: peer_drawing.submission.address.surface_id,
        track: peer_drawing.submission.track_id,
    };
    assert!(
        presenter
            .disconnect_leased_source_for_test(peer_source, 0, 1)
            .is_err()
    );
    assert!(
        presenter
            .disconnect_leased_source_for_test(source, 0, 2)
            .is_err()
    );
    presenter
        .disconnect_leased_source_for_test(source, 0, 1)
        .unwrap();
    first.resume(config(&presenter), &mut [&mut a]).unwrap();
    assert!(
        presenter
            .disconnect_leased_source_for_test(source, 0, 1)
            .is_err()
    );
    assert!(
        presenter
            .disconnect_leased_source_for_test(source, 1, 1)
            .is_err()
    );
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    assert!(!host.presented(&old).unwrap());
    drop(old);
    let receipt = a.submit(scene(0x00ff00ff)).unwrap();
    let frame = fresh(&mut host, owner, 2, receipt.revision());
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        receipt.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        peer_one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn retained_recovery_cut_releases_only_selected_same_id_deliveries_and_fences_resume() {
    use std::collections::{HashMap, HashSet};
    let (events, received) = std::sync::mpsc::sync_channel(8);
    let presenter = Arc::new(
        VirtualVivid::start_configured(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
            Some(events),
        )
        .unwrap(),
    );
    presenter.update_metrics(1, 16, 16, (8, 8));
    presenter.enable_overlay_host_relay();
    let _route = presenter.attach_overlay_presentation_relay().unwrap();
    let mut first = leased(&presenter);
    let peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let b = window(&peer, 64.);
    let one = a.submit(scene(0xff0000ff)).unwrap();
    let peer_one = b.submit(scene(0x0000ffff)).unwrap();
    let mut deliveries = [
        received.recv_timeout(Duration::from_secs(3)).unwrap(),
        received.recv_timeout(Duration::from_secs(3)).unwrap(),
    ];
    deliveries.sort_by_key(|event| event.source.producer);
    let [a_event, b_event] = deliveries;
    assert_eq!(
        (
            a_event.source.context,
            a_event.source.surface,
            a_event.source.track
        ),
        (
            b_event.source.context,
            b_event.source.surface,
            b_event.source.track
        )
    );
    assert_ne!(a_event.source.producer, b_event.source.producer);
    let (projection, cut) = presenter.prepare_bridge_recovery_snapshot_with_viewports(
        &HashSet::from([1]),
        &HashMap::new(),
        &HashSet::from([a_event.source]),
    );
    assert_eq!(projection.sources.len(), 2);
    assert!(presenter.bridge_recovery_source_is_current(&cut, a_event.source));
    assert!(!presenter.bridge_recovery_source_is_current(&cut, b_event.source));
    assert!(!presenter.bridge_delivery_is_pending(a_event.delivery_id, a_event.source));
    assert!(presenter.bridge_delivery_is_pending(b_event.delivery_id, b_event.source));
    assert!(!presenter.release_bridge_delivery(a_event.delivery_id));
    assert_eq!(one.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    let two = a.submit(scene(0x00ff00ff)).unwrap();
    let later = received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(later.source, a_event.source);
    assert!(presenter.bridge_delivery_is_pending(later.delivery_id, later.source));
    assert!(presenter.bridge_recovery_source_is_current(&cut, a_event.source));
    assert_eq!(
        vivid_protocol::vector::Frame::decode(
            projection
                .sources
                .iter()
                .find(|s| s.key == a_event.source)
                .unwrap()
                .retained
                .as_ref()
                .unwrap()
        )
        .unwrap()
        .revision,
        1
    );
    let (other, _other_host) = fixture();
    let foreign = connect(&other);
    let foreign_window = window(&foreign, 0.);
    let _foreign_receipt = foreign_window.submit(scene(0x0000ffff)).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    let foreign_source = loop {
        let snapshot =
            other.inspect_projection_snapshot_with_viewports(&HashSet::from([1]), &HashMap::new());
        if let Some(source) = snapshot.sources.first() {
            break source.key;
        }
        assert!(
            Instant::now() < until,
            "foreign same-ID source did not appear"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(
        foreign_source, a_event.source,
        "fixture must reuse IDs across presenters"
    );
    assert!(!other.bridge_recovery_source_is_current(&cut, a_event.source));
    first.resume(config(&presenter), &mut [&mut a]).unwrap();
    assert!(!presenter.bridge_recovery_source_is_current(&cut, a_event.source));
    assert_ne!(
        one.wait(Duration::ZERO).ok().flatten(),
        Some(vivid_sdk::overlay::PresentationOutcome::Presented)
    );
    assert_ne!(
        two.wait(Duration::ZERO).ok().flatten(),
        Some(vivid_sdk::overlay::PresentationOutcome::Presented)
    );
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    let _three = a.submit(scene(0xffff00ff)).unwrap();
    let resumed = received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(resumed.source, a_event.source);
    let (_, peer_cut) = presenter.prepare_bridge_recovery_snapshot_with_viewports(
        &HashSet::from([1]),
        &HashMap::new(),
        &HashSet::from([b_event.source]),
    );
    assert!(presenter.bridge_recovery_source_is_current(&peer_cut, b_event.source));
    assert!(!presenter.bridge_delivery_is_pending(b_event.delivery_id, b_event.source));
    assert!(presenter.bridge_delivery_is_pending(resumed.delivery_id, resumed.source));
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    assert!(presenter.release_bridge_delivery(resumed.delivery_id));
}
