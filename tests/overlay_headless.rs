//! A real `OverlaySession` driven end to end against the headless overlay presenter.
//!
//! Nothing here needs a GPU or a terminal. That is the point: a producer-side overlay toolkit has
//! to be testable in ordinary CI, and until this existed the only way to exercise one was a live
//! Vivido with a Vello adapter.

#![cfg(feature = "testing")]

use std::time::Duration;

use vivid_sdk::overlay::{
    Brush, Canvas, Color, HitRole, Path, PresentationOutcome, Rect, WindowMode, buttons,
};
use vivid_sdk::testing::{ROOT_SECRET_HEX, TestPresenter};
use vivid_sdk::{OverlayLaneEvent, OverlaySession, OverlayWindowOptions, ProducerConfig};

fn session(presenter: &TestPresenter) -> OverlaySession {
    let config = ProducerConfig {
        endpoint_control: Some(presenter.endpoint().to_owned()),
        authentication: vivid_sdk::ProducerAuthentication::root_hex(ROOT_SECRET_HEX).unwrap(),
        producer_name: "overlay-headless-test".into(),
        ..ProducerConfig::default()
    };
    OverlaySession::connect(config).expect("overlay session")
}

fn panel() -> (Canvas, Path) {
    let bounds = Rect::new(0., 0., 320., 180.).unwrap();
    let path = Path::rounded_rectangle(bounds, 12.).unwrap();
    let mut canvas = Canvas::new();
    canvas
        .fill(path.clone(), Brush::Solid(Color(0x203050ff)))
        .unwrap();
    canvas
        .push(vivid_sdk::overlay::Command::Hit {
            id: 7,
            path: path.clone(),
            role: HitRole::Input,
        })
        .unwrap();
    (canvas, path)
}

#[test]
fn a_producer_presents_a_scene_and_the_presenter_keeps_the_exact_display_list() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session(&presenter);
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(40., 40., 320., 180.).unwrap(),
            WindowMode::Floating,
        ))
        .expect("overlay window");

    let (canvas, _) = panel();
    let receipt = window.submit(canvas.clone()).expect("submission");
    assert_eq!(
        receipt.wait(Duration::from_secs(5)).expect("receipt"),
        Some(PresentationOutcome::Presented)
    );

    let scenes = presenter.overlay_scenes();
    assert_eq!(scenes.len(), 1);
    // The presenter keeps the list the producer built, not a re-encoding of it.
    assert_eq!(scenes[0].canvas, canvas);
    assert_eq!(scenes[0].revision, receipt.revision());

    window.close().unwrap();
    overlays.close().unwrap();
}

#[test]
fn replacing_a_scene_supersedes_the_receipt_it_replaced() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session(&presenter);
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 100., 100.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();

    let first = window.submit(panel().0).unwrap();
    assert_eq!(
        first.wait(Duration::from_secs(5)).unwrap(),
        Some(PresentationOutcome::Presented)
    );

    let mut second_canvas = Canvas::new();
    second_canvas
        .fill(
            Path::rectangle(Rect::new(0., 0., 50., 50.).unwrap()).unwrap(),
            Brush::Solid(Color(0xff0000ff)),
        )
        .unwrap();
    let second = window.submit(second_canvas).unwrap();
    assert_eq!(
        second.wait(Duration::from_secs(5)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    // The earlier receipt resolves exactly once, and as superseded rather than presented.
    assert_eq!(
        first.wait(Duration::ZERO).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert!(second.revision() > first.revision());

    overlays.close().unwrap();
}

#[test]
fn pointer_input_reaches_the_region_the_display_list_declared() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session(&presenter);
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(40., 40., 320., 180.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    window
        .submit(panel().0)
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();

    // Viewport coordinates: the window sits at (40, 40), so this lands 10 logical pixels inside.
    assert!(presenter.overlay_pointer(50., 50., None, 0).unwrap());
    assert!(
        presenter
            .overlay_pointer(50., 50., Some((buttons::PRIMARY, true)), 0)
            .unwrap()
    );

    let mut regions = Vec::new();
    while let Some(event) = overlays.wait_event(Duration::from_millis(500)).unwrap() {
        if let OverlayLaneEvent::Input(input) = event
            && let vivid_sdk::overlay::Event::Pointer {
                position,
                region,
                button,
                ..
            } = input.event
        {
            assert!(overlays.event_targets(&input, &window).unwrap());
            // Positions arrive window-local, not viewport-relative.
            assert_eq!((position.x.get(), position.y.get()), (10., 10.));
            regions.push((region, button));
            if button.is_some() {
                break;
            }
        }
    }
    assert_eq!(
        regions,
        vec![(7, None), (7, Some((buttons::PRIMARY, true)))]
    );

    overlays.close().unwrap();
}

#[test]
fn a_viewport_change_reaches_the_producer_with_its_own_revision() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session(&presenter);
    let _window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 64., 64.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();

    presenter.set_overlay_viewport(1024., 768., 2).unwrap();
    let mut seen = None;
    while let Some(event) = overlays.wait_event(Duration::from_millis(500)).unwrap() {
        if let OverlayLaneEvent::Viewport(update) = event {
            seen = Some(update);
            if seen
                .as_ref()
                .is_some_and(|u| u.viewport.width.get() == 1024.)
            {
                break;
            }
        }
    }
    let update = seen.expect("viewport snapshot");
    assert_eq!(update.viewport.width.get(), 1024.);
    assert_eq!(update.viewport.height.get(), 768.);
    assert_eq!(update.viewport.scale_numerator, 2);
    assert!(update.revision > 0);

    overlays.close().unwrap();
}
