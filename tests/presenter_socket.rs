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
