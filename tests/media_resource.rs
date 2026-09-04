//! Minting and resolving media resources against a real producer.
//!
//! The property under test is the one the design turns on: a pinned reference refuses once the
//! content it named has moved, and never answers with whatever replaced it.

#![cfg(feature = "presenter")]

use std::time::Duration;

use vivid_sdk::presenter::{
    Binding, MediaConfig, PresenterConfig, PresenterListener, ResourceError, SocketListener,
    VirtualVivid,
};
use vivid_sdk::{PaneSession, ProducerAuthentication, ProducerConfig, Session};

const PANE: u64 = 1;
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
    presenter.update_metrics(PANE, 80, 24, (8, 16));
    (presenter, endpoint)
}

fn produce(presenter: &VirtualVivid, endpoint: &str, pane: u64) -> PaneSession {
    let secret = presenter.issue_pane_capability(pane).expect("capability");
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint.to_owned()),
        authentication: ProducerAuthentication::root_hex(&secret).expect("secret"),
        producer_name: "resource-test".into(),
        ..ProducerConfig::default()
    })
    .expect("connect");
    PaneSession::from_session(session).expect("pane session")
}

fn only_source(presenter: &VirtualVivid, pane: u64) -> vivid_sdk::presenter::SourceKey {
    let summary = presenter.pane_media_summary(pane);
    assert_eq!(summary.tracks.len(), 1, "one track: {summary:?}");
    summary.tracks[0].source
}

#[test]
fn a_pinned_resource_describes_the_content_it_was_minted_from() {
    let (presenter, endpoint) = start();
    let mut pane = produce(&presenter, &endpoint, PANE);
    pane.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(PANE, Duration::from_secs(5)));

    let source = only_source(&presenter, PANE);
    let id = presenter
        .announce_media_resource(source, Binding::Pinned)
        .expect("mint");

    let described = presenter.describe_media_resource(&id).expect("describe");
    assert_eq!(described.binding, Binding::Pinned);
    assert_eq!(described.context_id, source.context);
    assert_eq!(described.surface_id, source.surface);

    let track = described
        .track
        .expect("a pinned reference always names a track");
    assert_eq!(track.track_id, source.track);
    assert!(track.capturable, "the pixels are in hand");

    pane.close().expect("close");
}

#[test]
fn a_pinned_resource_goes_stale_rather_than_naming_what_replaced_it() {
    let (presenter, endpoint) = start();
    let mut pane = produce(&presenter, &endpoint, PANE);
    pane.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(PANE, Duration::from_secs(5)));

    let first = only_source(&presenter, PANE);
    let id = presenter
        .announce_media_resource(first, Binding::Pinned)
        .expect("mint");
    assert!(presenter.describe_media_resource(&id).is_ok());

    // Showing a second image replaces the surface and its track. The reference now names content
    // that no longer exists, and the replacement is exactly what it must not return.
    pane.show_rgba(2, 2, &PIXELS).expect("replace");

    let second = only_source(&presenter, PANE);
    assert_ne!(
        (second.surface, second.track),
        (first.surface, first.track),
        "the producer really did replace the content"
    );
    assert_eq!(
        presenter.describe_media_resource(&id),
        Err(ResourceError::Stale),
        "a pinned reference refuses instead of retargeting"
    );

    pane.close().expect("close");
}

#[test]
fn a_live_resource_follows_its_surface() {
    let (presenter, endpoint) = start();
    let mut pane = produce(&presenter, &endpoint, PANE);
    pane.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(PANE, Duration::from_secs(5)));

    let source = only_source(&presenter, PANE);
    let id = presenter
        .announce_media_resource(source, Binding::Live)
        .expect("mint");

    let described = presenter.describe_media_resource(&id).expect("describe");
    assert_eq!(described.binding, Binding::Live);
    assert_eq!(described.surface_id, source.surface);

    pane.close().expect("close");
}

#[test]
fn an_unknown_or_released_resource_is_not_reported_as_stale() {
    // The two are different facts. "I never minted that" and "what you asked for has moved" send a
    // caller to different places.
    let (presenter, endpoint) = start();
    let mut pane = produce(&presenter, &endpoint, PANE);
    pane.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(PANE, Duration::from_secs(5)));

    assert_eq!(
        presenter.describe_media_resource("never-minted"),
        Err(ResourceError::Unknown)
    );

    let id = presenter
        .announce_media_resource(only_source(&presenter, PANE), Binding::Live)
        .expect("mint");
    assert!(presenter.release_media_resource(&id));
    assert_eq!(
        presenter.describe_media_resource(&id),
        Err(ResourceError::Unknown)
    );
    assert!(
        !presenter.release_media_resource(&id),
        "releasing twice is not a lie"
    );

    pane.close().expect("close");
}

#[test]
fn announcing_a_track_this_runtime_does_not_hold_is_refused() {
    let (presenter, _endpoint) = start();

    let absent = vivid_sdk::presenter::SourceKey {
        producer: 1,
        context: 1,
        surface: 1,
        track: 1,
    };
    assert_eq!(
        presenter.announce_media_resource(absent, Binding::Pinned),
        Err(ResourceError::Unknown),
        "minting an id that is guaranteed stale is worse than refusing"
    );
}

#[test]
fn two_owners_minting_from_the_same_local_ids_get_different_resources() {
    let (presenter, endpoint) = start();
    presenter.update_metrics(2, 80, 24, (8, 16));

    let mut first = produce(&presenter, &endpoint, 1);
    let mut second = produce(&presenter, &endpoint, 2);
    first.show_rgba(2, 2, &PIXELS).expect("show");
    second.show_rgba(2, 2, &PIXELS).expect("show");
    assert!(presenter.wait_for_retained_media(1, Duration::from_secs(5)));
    assert!(presenter.wait_for_retained_media(2, Duration::from_secs(5)));

    let first_source = only_source(&presenter, 1);
    let second_source = only_source(&presenter, 2);
    assert_ne!(
        first_source.producer, second_source.producer,
        "different owners, whatever their local numbers"
    );

    let first_id = presenter
        .announce_media_resource(first_source, Binding::Pinned)
        .expect("mint");
    let second_id = presenter
        .announce_media_resource(second_source, Binding::Pinned)
        .expect("mint");
    assert_ne!(first_id, second_id);

    let described_first = presenter
        .describe_media_resource(&first_id)
        .expect("describe");
    let described_second = presenter
        .describe_media_resource(&second_id)
        .expect("describe");
    assert_ne!(
        described_first.producer, described_second.producer,
        "each resource resolves to its own owner"
    );

    first.close().expect("close");
    second.close().expect("close");
}
