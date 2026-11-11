//! A producer and the real presenter, meeting over `SocketListener`.
//!
//! Every other test in this crate drives the test presenter, which brings its own socket. These
//! prove the listener a caller actually gets — and that the crate's two halves interoperate over an
//! ordinary endpoint, which is what a binding for another language depends on.

#![cfg(feature = "presenter")]

use std::time::Duration;

use vivid_sdk::presenter::{
    CaptureContent, MediaConfig, PresenterConfig, PresenterListener, SocketListener, VirtualVivid,
};
use vivid_sdk::{PaneSession, ProducerAuthentication, ProducerConfig, Session};

const PANE: u64 = 1;

/// Red, green, blue, white: four distinguishable pixels, so a wrong capture cannot pass by
/// returning uniformly coloured bytes.
const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

fn start() -> (VirtualVivid, String) {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").expect("bind");
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::terminal(MediaConfig::default()),
        None,
    )
    .expect("start");
    (presenter, endpoint)
}

fn connect(endpoint: &str, secret: &str) -> Session {
    Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint.to_owned()),
        authentication: ProducerAuthentication::root_hex(secret).expect("secret"),
        producer_name: "socket-test".into(),
        ..ProducerConfig::default()
    })
    .expect("connect")
}

#[test]
fn a_producer_reaches_the_presenter_over_a_bound_socket_and_its_frame_can_be_captured() {
    let (presenter, endpoint) = start();
    presenter.update_metrics(PANE, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(PANE).expect("capability");

    let mut pane = PaneSession::from_session(connect(&endpoint, &secret)).expect("pane session");
    pane.show_rgba(2, 2, &PIXELS).expect("show");

    assert!(
        presenter.wait_for_retained_media(PANE, Duration::from_secs(5)),
        "the presenter retained the frame"
    );

    let capture = presenter.capture_pane(PANE, 0);
    let layer = capture
        .layers
        .first()
        .unwrap_or_else(|| panic!("expected one layer; skipped: {:?}", capture.skipped));
    match &layer.content {
        CaptureContent::Raster(raster) => {
            assert_eq!((raster.width, raster.height), (2, 2));
            assert_eq!(&raster.pixels[..], &PIXELS[..], "the exact pixels sent");
        }
        other => panic!("expected retained raster, got {other:?}"),
    }

    let summary = presenter.pane_media_summary(PANE);
    assert_eq!(summary.tracks.len(), 1, "one raster track");
    assert!(
        summary.tracks[0].capturable,
        "the track reports as capturable"
    );

    pane.close().expect("close");
}

#[test]
fn empty_eos_uses_sequence_zero_and_does_not_end_another_owners_track() {
    use vivid_protocol::track::{KindConfiguration, MILESTONE_EOS_ACCEPTED, RasterConfiguration};
    use vivid_sdk::*;
    let (presenter, endpoint) = start();
    let mut owners = Vec::new();
    for pane in [1, 2] {
        presenter.update_metrics(pane, 80, 24, (8, 16));
        let mut session = connect(&endpoint, &presenter.issue_pane_capability(pane).unwrap());
        session
            .create_surface(
                SurfaceDefinition {
                    context_id: 1,
                    surface_id: 1,
                    semantic_profile: GENERIC_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 2,
                    logical_height: 2,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: SurfaceRole::Figure,
                        title: String::new(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: vec![],
                },
                &Default::default(),
            )
            .unwrap();
        let track = session
            .create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id: 1,
                    surface_id: 1,
                    track_id: 1,
                    slot: SLOT_POSTER,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: vivid_protocol::media::rgba8_raw_frame_body_len(2, 2)
                        .unwrap(),
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: 1_000_000,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: 1024,
                    kind: KindConfiguration::Raster(RasterConfiguration {
                        width: 2,
                        height: 2,
                        alpha_mode: 1,
                        delta_enabled: false,
                        maximum_delta_operations: 1,
                        zstd_enabled: false,
                    }),
                    target_latency_us: 16_000,
                    maximum_latency_us: 100_000,
                    retained_pixel_charge: 4,
                },
                &Default::default(),
            )
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        owners.push((session, track, channel));
    }
    owners[0].2.eos().unwrap();
    owners[0]
        .0
        .wait_track(
            &owners[0].1,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_EOS_ACCEPTED),
            3_000_000,
        )
        .unwrap();
    let first = owners[0].0.query_track(&owners[0].1).unwrap();
    let peer = owners[1].0.query_track(&owners[1].1).unwrap();
    assert_eq!(first.last_media_record_sequence, 0);
    assert_eq!(first.cumulative_media_records, 0);
    assert_eq!(peer.milestones & MILESTONE_EOS_ACCEPTED, 0);
    owners[1].2.eos().unwrap();
    owners[1]
        .0
        .wait_track(
            &owners[1].1,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_EOS_ACCEPTED),
            3_000_000,
        )
        .unwrap();
    assert_eq!(
        owners[1]
            .0
            .query_track(&owners[1].1)
            .unwrap()
            .last_media_record_sequence,
        0
    );
}

#[test]
fn two_owners_reusing_local_ids_capture_only_their_own_pixels_over_a_socket() {
    // The rule the root AGENTS.md states for owner-scoped work. Both producers below are driven by
    // the same helper, so both allocate the same local surface, track, and node numbers.
    let (presenter, endpoint) = start();
    presenter.update_metrics(1, 80, 24, (8, 16));
    presenter.update_metrics(2, 80, 24, (8, 16));

    let first_secret = presenter.issue_pane_capability(1).expect("capability");
    let second_secret = presenter.issue_pane_capability(2).expect("capability");

    let mut first = PaneSession::from_session(connect(&endpoint, &first_secret)).expect("first");
    let second = PaneSession::from_session(connect(&endpoint, &second_secret)).expect("second");

    // Only the first pane is ever shown anything.
    first.show_rgba(2, 2, &PIXELS).expect("show");

    assert!(presenter.wait_for_retained_media(1, Duration::from_secs(5)));
    assert!(
        !presenter.wait_for_retained_media(2, Duration::from_millis(200)),
        "the second owner holds nothing, despite reusing the first owner's local IDs"
    );

    assert_eq!(presenter.capture_pane(1, 0).layers.len(), 1);
    let second_capture = presenter.capture_pane(2, 0);
    assert!(
        second_capture.layers.is_empty(),
        "the second owner captures none of the first's pixels: {second_capture:?}"
    );

    first.close().expect("close first");
    second.close().expect("close second");
}

#[test]
fn pane_images_follow_target_changes_without_touching_a_peer_with_reused_ids() {
    let (presenter, endpoint) = start();
    for pane in [1, 2] {
        presenter.update_metrics(pane, 80, 24, (8, 16));
    }
    let mut first = PaneSession::from_session(connect(
        &endpoint,
        &presenter.issue_pane_capability(1).unwrap(),
    ))
    .unwrap();
    let mut peer = PaneSession::from_session(connect(
        &endpoint,
        &presenter.issue_pane_capability(2).unwrap(),
    ))
    .unwrap();
    first.show_rgba(2, 2, &PIXELS).unwrap();
    peer.show_rgba(2, 2, &[42; 16]).unwrap();
    for columns in [90, 100, 80] {
        presenter.update_metrics(1, columns, 30, (8, 16));
        first.show_rgba(2, 2, &PIXELS).unwrap();
        let capture = presenter.capture_pane(1, 0);
        assert_eq!(
            capture.layers.len(),
            1,
            "old placement cannot accumulate after resize"
        );
        assert!(
            matches!(&capture.layers[0].content, CaptureContent::Raster(r) if r.pixels.as_ref() == PIXELS)
        );
        let peer_capture = presenter.capture_pane(2, 0);
        assert_eq!(peer_capture.layers.len(), 1);
        assert!(
            matches!(&peer_capture.layers[0].content, CaptureContent::Raster(r) if r.pixels.as_ref() == [42; 16])
        );
    }
    first.close().unwrap();
    assert_eq!(presenter.capture_pane(2, 0).layers.len(), 1);
    peer.close().unwrap();
}

/// The connection was already gone, so failing is right — but the session knows *why* it went, and
/// answering with the writer's generic "writer is closed" throws that away. A caller left holding a
/// bare `BrokenPipe` has to reconstruct from timing alone what the reader recorded exactly.
#[test]
fn closing_after_the_connection_is_lost_reports_the_reason_it_was_lost() {
    let (presenter, endpoint) = start();
    presenter.update_metrics(PANE, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(PANE).expect("capability");

    let mut pane = PaneSession::from_session(connect(&endpoint, &secret)).expect("pane session");
    pane.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(PANE, Duration::from_secs(5)));

    // The presenter drops every session bound to the pane, which is what a revocation, a restart,
    // or a lost network does to a producer that is not watching for it.
    presenter.revoke_pane(PANE);
    let error = wait_for_close_to_fail(pane, Duration::from_secs(5));
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::NotConnected,
        "a connection that ended is not a broken pipe on this write: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("ended before it was closed"),
        "close did not say the connection had ended: {message}"
    );
    assert!(
        !message.contains("Vivid connection writer is closed"),
        "close answered with the writer's generic error instead of the recorded reason: {message}"
    );
}

/// `cancel_handle` shuts the writer down, so a later close cannot round-trip either — but it ended
/// for a different reason, and a caller that cancelled deliberately should not have to tell that
/// apart from a connection that died on its own.
#[test]
fn closing_after_cancelling_says_it_was_cancelled() {
    let (presenter, endpoint) = start();
    presenter.update_metrics(PANE, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(PANE).expect("capability");

    let session = connect(&endpoint, &secret);
    let cancel = session.cancel_handle();
    cancel();

    let error = session
        .close()
        .expect_err("a cancelled session cannot be closed cleanly");
    assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
    assert!(
        error.to_string().contains("cancelled"),
        "close did not distinguish cancellation from loss: {error}"
    );
}

/// `abort` closes the lifecycle without touching the control connection, and its documentation
/// promises the session may still be closed normally afterwards. That `GOODBYE` must still happen:
/// the quit path aborts to wake blocked media senders, then closes.
#[test]
fn closing_after_aborting_still_performs_the_goodbye() {
    let (presenter, endpoint) = start();
    presenter.update_metrics(PANE, 80, 24, (8, 16));
    let secret = presenter.issue_pane_capability(PANE).expect("capability");

    let mut session = connect(&endpoint, &secret);
    session.abort().expect("abort");
    session
        .close()
        .expect("an aborted session still closes cleanly");
}

#[test]
fn shutdown_retires_authenticated_and_incomplete_connections_and_clears_owners() {
    let (presenter, endpoint) = start();
    let presenter = std::sync::Arc::new(presenter);
    let mut owners = Vec::new();
    for pane in [1, 2] {
        presenter.update_metrics(pane, 80, 24, (8, 16));
        let secret = presenter.issue_pane_capability(pane).unwrap();
        let mut owner = PaneSession::from_session(connect(&endpoint, &secret)).unwrap();
        owner.show_rgba(2, 2, &PIXELS).unwrap();
        assert!(presenter.wait_for_retained_media(pane, Duration::from_secs(5)));
        owners.push(owner);
    }
    let incomplete = std::net::TcpStream::connect(endpoint.strip_prefix("tcp:").unwrap()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while presenter.active_connection_count() < 5 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    while presenter
        .wait_media_event(Duration::ZERO)
        .unwrap()
        .is_some()
    {}
    let waiting = presenter.clone();
    let (done, result) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = done.send(waiting.wait_media_event(Duration::from_secs(30)));
    });
    presenter.shutdown();
    presenter.shutdown();
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .is_none()
    );
    waiter.join().unwrap();
    for pane in [1, 2] {
        assert!(presenter.capture_pane(pane, 0).layers.is_empty());
    }
    while presenter.active_connection_count() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "presenter connections did not retire: {}",
            presenter.active_connection_count()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(incomplete);
    drop(owners);
}

/// The control reader runs on its own thread, so a revocation lands asynchronously. Sending until a
/// send fails is the producer-side observation that the connection has gone, using only what any
/// caller has: the reader records the reason before it shuts the writer down, so by the time a send
/// fails the reason is already stored.
fn wait_for_close_to_fail(mut pane: PaneSession, timeout: Duration) -> std::io::Error {
    let deadline = std::time::Instant::now() + timeout;
    while pane.show_rgba(2, 2, &PIXELS).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the producer never noticed the revoked pane"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    pane.close()
        .expect_err("a revoked pane's session closed cleanly")
}
