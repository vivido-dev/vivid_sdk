//! The terminating presenter a pane actually gets, hosting a real overlay producer over a socket.
//!
//! `overlay_headless.rs` drives the same producer against the test presenter. This drives it
//! against the production one, so the lane server, the vector track channel, the window handshake,
//! the pane-derived viewport, and the projection a relay reads are all the real implementations.

#![cfg(feature = "presenter")]

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use vivid_sdk::overlay::{Brush, Canvas, Color, Path, Rect, WindowMode};
use vivid_sdk::presenter::{
    BridgeSourceKind, MediaConfig, PresenterConfig, PresenterListener, ProjectionSnapshot,
    SocketListener, SourceDescriptor, VirtualVivid,
};
use vivid_sdk::{
    OverlayLaneEvent, OverlaySession, OverlayWindowOptions, ProducerAuthentication, ProducerConfig,
};

const CELL: (u16, u16) = (8, 16);

fn presenter() -> (VirtualVivid, String) {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").expect("bind");
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured_eventless(
        listener,
        PresenterConfig::terminal_with_overlay(MediaConfig::default()),
    )
    .expect("overlay-hosting presenter");
    (presenter, endpoint)
}

/// Every lane is pinned to the presenter under test, so an ambient producer-discovery environment
/// cannot redirect part of this session somewhere else.
fn connect(endpoint: &str, secret: &str, name: &str) -> OverlaySession {
    OverlaySession::connect(ProducerConfig {
        endpoint_control: Some(endpoint.to_owned()),
        endpoint_interactive: Some(endpoint.to_owned()),
        endpoint_realtime: Some(endpoint.to_owned()),
        endpoint_bulk: Some(endpoint.to_owned()),
        authentication: ProducerAuthentication::root_hex(secret).expect("pane secret"),
        producer_name: name.into(),
        ..ProducerConfig::default()
    })
    .expect("overlay session")
}

fn scene(colour: u32) -> Canvas {
    let mut canvas = Canvas::new();
    canvas
        .fill(
            Path::rectangle(Rect::new(0., 0., 120., 60.).unwrap()).unwrap(),
            Brush::Solid(Color(colour)),
        )
        .unwrap();
    canvas
}

/// The logical size of the next viewport snapshot the lane delivers.
fn await_viewport(overlays: &OverlaySession) -> (u64, f64, f64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some(event) = overlays
            .wait_event(Duration::from_millis(250))
            .expect("overlay lane")
        else {
            continue;
        };
        match event {
            OverlayLaneEvent::Viewport(update) => {
                return (
                    update.revision,
                    update.viewport.width.get(),
                    update.viewport.height.get(),
                );
            }
            OverlayLaneEvent::ConnectionLost { diagnostic } => {
                panic!("overlay lane lost: {diagnostic}")
            }
            _ => {}
        }
    }
    panic!("no viewport snapshot arrived on the lane");
}

fn overlay_surfaces(snapshot: &ProjectionSnapshot) -> usize {
    snapshot
        .surfaces
        .iter()
        .filter(|surface| surface.overlay_window.is_some())
        .count()
}

#[test]
fn a_pane_presenter_hosts_an_overlay_window_end_to_end() {
    const PANE: u64 = 3;
    const COLUMNS: u16 = 80;
    const ROWS: u16 = 24;

    let (presenter, endpoint) = presenter();
    presenter.update_metrics(PANE, COLUMNS, ROWS, CELL);
    let secret = presenter.issue_pane_capability(PANE).expect("capability");
    let overlays = connect(&endpoint, &secret, "overlay-presenter-test");

    // Connecting at all proves the overlay profile bundle was negotiated and the interactive lane
    // was served: `OverlaySession` opens and renews that lane before it returns.
    let (revision, width, height) = await_viewport(&overlays);
    assert!(revision > 0, "the initial snapshot carries a revision");
    assert_eq!(
        (width, height),
        (
            f64::from(COLUMNS) * f64::from(CELL.0),
            f64::from(ROWS) * f64::from(CELL.1)
        ),
        "an overlay window lays out against the pane's own pixel rectangle"
    );

    // Surface, vector-scene track, bulk channel and SET_OVERLAY_WINDOW all cross the presenter.
    let bounds = Rect::new(12., 20., 240., 120.).unwrap();
    let window = overlays
        .create_window(OverlayWindowOptions::new(bounds, WindowMode::Floating))
        .expect("overlay window");

    // Presenting also activates the window's slot, which the producer holds behind
    // `MILESTONE_OUTPUT_READY`. It only returns if accepting the display list set that.
    window.present(scene(0x203050ff)).expect("present");

    // An interactive overlay redraws on every hover and click, so the channel's allowance has to
    // come back as each display list is accepted. A presenter that only honours the initial grant
    // lets the first scene through and then blocks the producer forever.
    for frame in 0..5 {
        window
            .present(scene(0x203050ff + frame))
            .expect("a redraw must not block on credit that never returns");
    }

    let status = window.reconcile().expect("window status");
    assert_eq!(
        status.bounds, bounds,
        "the host kept the requested placement"
    );
    assert!(status.presented_revision > 0, "the display list published");

    // What a terminating relay reads: one overlay surface carrying its window geometry, one
    // vector-scene source, and no scene node — an overlay window is placed by its own bounds.
    let panes = HashSet::from([PANE]);
    let snapshot = presenter.prepare_projection_snapshot_with_viewports(&panes, &HashMap::new());
    let surface = snapshot
        .surfaces
        .iter()
        .find(|surface| surface.overlay_window.is_some())
        .expect("overlay surface");
    let projected = surface.overlay_window.expect("window geometry");
    assert_eq!(
        (
            projected.x,
            projected.y,
            projected.width,
            projected.height,
            projected.mode,
            projected.visible
        ),
        (12, 20, 240, 120, 0, true)
    );
    let source = snapshot
        .sources
        .iter()
        .find(|source| matches!(source.descriptor, SourceDescriptor::VectorScene(_)))
        .expect("vector-scene source");
    assert!(source.active, "the producer activated its slot");

    let bridged = snapshot.bridge_projection();
    assert!(
        bridged
            .surfaces
            .iter()
            .any(|surface| surface.overlay_window.is_some())
    );
    assert!(
        bridged
            .sources
            .iter()
            .any(|source| matches!(source.kind, BridgeSourceKind::VectorScene { .. }))
    );
    assert!(
        bridged.nodes.is_empty(),
        "an overlay window is positioned by SET_OVERLAY_WINDOW, never by a scene node"
    );
}

/// A declarative toolkit measures every string it is about to draw before it can decide where
/// anything goes, so this service is not an enhancement — it is what makes such a program run at
/// all. A presenter that negotiates the overlay bundle without it accepts the connection and then
/// refuses the first layout pass, which is indistinguishable from the toolkit being broken.
#[test]
fn a_pane_presenter_measures_the_text_a_layout_pass_asks_about() {
    use vivid_protocol::overlay::wire::text::styled::{StyledText, TextStyle};

    const PANE: u64 = 4;
    let (presenter, endpoint) = presenter();
    presenter.update_metrics(PANE, 80, 24, CELL);
    let secret = presenter.issue_pane_capability(PANE).expect("capability");
    let overlays = connect(&endpoint, &secret, "overlay-measure-test");
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 420., 240.).unwrap(),
            WindowMode::Floating,
        ))
        .expect("overlay window");

    let style = TextStyle::default();
    let short = StyledText::new("Increment", style.clone());
    let long = StyledText::new("Clicked 0 times, and then several more", style);
    let measured = window
        .measure_text_batch(std::slice::from_ref(&short))
        .expect("the host answers a measurement request");
    assert_eq!(measured.len(), 1);
    let one = &measured[0];
    assert!(
        one.width.get() > 0. && one.height.get() > 0.,
        "a measured run has an extent to lay out against"
    );
    assert!(!one.lines.is_empty(), "and at least one line of geometry");

    // A batch is answered in request order, which is the only thing a caller can match its own
    // runs against — a layout pass measures many strings at once and keeps no other correlator.
    let batch = window
        .measure_text_batch(&[short, long])
        .expect("a batch is answered");
    assert_eq!(batch.len(), 2);
    assert!(
        batch[1].width.get() > batch[0].width.get(),
        "the longer run measured wider, so the answers were not transposed"
    );

    // Retaining is what a wrapped or aligned paragraph is drawn through, and the handle has to
    // survive long enough to be referenced by the scene that uses it.
    let retained = window
        .layout_text_batch(std::slice::from_ref(&StyledText::new(
            "a paragraph that wraps",
            TextStyle::default(),
        )))
        .expect("the host retains a layout");
    assert_eq!(retained.len(), 1);
    assert_ne!(retained[0].id(), 0, "a retained layout has a usable id");
}

#[test]
fn a_pane_resize_moves_only_its_own_overlay_viewport_and_a_disconnect_spares_the_other() {
    // One presenter, two panes, one producer each. Both name their surface and track from their
    // own identity space, so these two windows deliberately reuse the same local numbers.
    let (presenter, endpoint) = presenter();
    presenter.update_metrics(1, 80, 24, CELL);
    presenter.update_metrics(2, 40, 12, CELL);
    let first_secret = presenter
        .issue_pane_capability(1)
        .expect("first capability");
    let second_secret = presenter
        .issue_pane_capability(2)
        .expect("second capability");
    let first = connect(&endpoint, &first_secret, "pane-one");
    let second = connect(&endpoint, &second_secret, "pane-two");

    assert_eq!(await_viewport(&first).1, 640., "pane one is 80 cells wide");
    assert_eq!(await_viewport(&second).1, 320., "pane two is 40 cells wide");

    let window_one = first
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 200., 100.).unwrap(),
            WindowMode::Floating,
        ))
        .expect("first window");
    let window_two = second
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 200., 100.).unwrap(),
            WindowMode::Floating,
        ))
        .expect("second window");
    window_one.present(scene(0xff0000ff)).expect("first scene");
    window_two.present(scene(0x0000ffff)).expect("second scene");

    presenter.update_metrics(1, 100, 30, CELL);
    let (_, width, height) = await_viewport(&first);
    assert_eq!(
        (width, height),
        (800., 480.),
        "the resized pane's producer is told"
    );
    let resized = window_one.viewport().expect("pane one viewport");
    let untouched = window_two.viewport().expect("pane two viewport");
    assert_eq!((resized.width.get(), resized.height.get()), (800., 480.));
    assert_eq!(
        (untouched.width.get(), untouched.height.get()),
        (320., 192.),
        "resizing one pane must not relayout another pane's window"
    );

    // Losing one producer leaves the other's window intact and still able to present.
    drop(window_one);
    drop(first);
    let panes = HashSet::from([1, 2]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot =
            presenter.prepare_projection_snapshot_with_viewports(&panes, &HashMap::new());
        if overlay_surfaces(&snapshot) == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the lost producer's overlay surface was never released"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    window_two
        .present(scene(0x00ff00ff))
        .expect("the surviving owner can still present");
    let snapshot = presenter.prepare_projection_snapshot_with_viewports(&panes, &HashMap::new());
    assert_eq!(overlay_surfaces(&snapshot), 1);
    assert_eq!(
        window_two
            .reconcile()
            .expect("surviving window status")
            .bounds,
        Rect::new(0., 0., 200., 100.).unwrap()
    );
}
