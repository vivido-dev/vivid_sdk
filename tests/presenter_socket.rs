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
