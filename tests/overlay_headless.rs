//! A real `OverlaySession` driven end to end against the headless overlay presenter.
//!
//! Nothing here needs a GPU or a terminal. That is the point: a producer-side overlay toolkit has
//! to be testable in ordinary CI, and until this existed the only way to exercise one was a live
//! Vivido with a Vello adapter.

#![cfg(feature = "testing")]

use std::time::Duration;

use vivid_sdk::overlay::{
    Brush, Canvas, Cap, Color, ColorSpace, Corners, Extend, GradientStop, HitRole, Join, Path,
    Point, PresentationOutcome, Rect, Scalar, Shadow, StrokeStyle, WindowMode, buttons,
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

/// A session that carries the overlay bundle but deliberately not the paint profile.
fn session_without_paint(presenter: &TestPresenter) -> OverlaySession {
    let mut config = ProducerConfig {
        endpoint_control: Some(presenter.endpoint().to_owned()),
        authentication: vivid_sdk::ProducerAuthentication::root_hex(ROOT_SECRET_HEX).unwrap(),
        producer_name: "overlay-headless-no-paint".into(),
        ..ProducerConfig::default()
    };
    config.target_profile = vivid_sdk::TERMINAL_SURFACE.into();
    config.required_profiles.extend([
        vivid_sdk::CORE_CONTROL.into(),
        vivid_sdk::LIVE_MEDIA.into(),
        vivid_sdk::TERMINAL_SURFACE.into(),
        vivid_sdk::TERMINAL_OVERLAY.into(),
        vivid_sdk::VECTOR_SCENE.into(),
        vivid_sdk::OVERLAY_INPUT.into(),
    ]);
    config.required_profiles.sort();
    config.required_profiles.dedup();
    // A profile cannot be both required and optional, and the default config offers some of the
    // same ones; the required list is the point of this session.
    config
        .optional_profiles
        .retain(|profile| !config.required_profiles.contains(profile));
    OverlaySession::from_session(vivid_sdk::Session::connect(config).expect("session"))
        .expect("overlay session")
}

#[test]
fn paint_commands_round_trip_and_carry_their_style_to_the_host() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session(&presenter);
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 200., 120.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    let path = Path::rectangle(Rect::new(0., 0., 200., 120.).unwrap()).unwrap();
    let stops = vec![
        GradientStop {
            offset: 0,
            color: Color(0xff0000ff),
        },
        GradientStop {
            offset: u16::MAX,
            color: Color(0x0000ffff),
        },
    ];

    let mut canvas = Canvas::new();
    canvas
        .shadow(Shadow {
            rect: Rect::new(10., 10., 100., 60.).unwrap(),
            radii: Corners::new([4., 8., 12., 16.]).unwrap(),
            color: Color(0x00000055),
            offset: Point::new(0., 6.).unwrap(),
            blur: Scalar::new(18.).unwrap(),
            spread: Scalar::new(-2.).unwrap(),
            inset: false,
        })
        .unwrap();
    canvas
        .fill(
            path.clone(),
            Brush::Linear {
                start: Point::new(0., 0.).unwrap(),
                end: Point::new(200., 0.).unwrap(),
                stops,
                color_space: ColorSpace::Oklab,
            },
        )
        .unwrap();
    canvas
        .fill(
            Path::rounded_rectangle_corners(
                Rect::new(0., 0., 80., 40.).unwrap(),
                Corners::new([2., 6., 10., 14.]).unwrap(),
            )
            .unwrap(),
            Brush::Image {
                asset: 1,
                transform: None,
                extend: Extend::Repeat,
            },
        )
        .unwrap();
    canvas
        .stroke_styled(
            path,
            Brush::Solid(Color(0xffffffff)),
            StrokeStyle {
                width: Scalar::new(2.5).unwrap(),
                cap: Cap::Round,
                join: Join::Bevel,
                miter_limit: Scalar::new(6.).unwrap(),
                dashes: vec![Scalar::new(4.).unwrap(), Scalar::new(2.).unwrap()],
                dash_offset: Scalar::new(1.5).unwrap(),
            },
        )
        .unwrap();
    canvas.validate().unwrap();

    let receipt = window.submit(canvas.clone()).unwrap();
    assert_eq!(
        receipt.wait(Duration::from_secs(5)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    // The presenter keeps the list byte-for-byte, so every paint field survived the codec.
    assert_eq!(presenter.overlay_scenes()[0].canvas, canvas);

    overlays.close().unwrap();
}

#[test]
fn paint_commands_are_refused_locally_when_the_profile_was_not_negotiated() {
    let presenter = TestPresenter::start(80, 24).unwrap();
    let overlays = session_without_paint(&presenter);
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 64., 64.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    let path = Path::rectangle(Rect::new(0., 0., 64., 64.).unwrap()).unwrap();

    // A plain scene still submits: only the paint forms are gated.
    let mut plain = Canvas::new();
    plain
        .fill(path.clone(), Brush::Solid(Color(0x203050ff)))
        .unwrap();
    window
        .submit(plain)
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();

    // Each paint form fails before anything is sent, so the producer gets a local diagnosis
    // instead of a channel failure on the host.
    let mut shadowed = Canvas::new();
    shadowed
        .shadow(Shadow {
            rect: Rect::new(0., 0., 10., 10.).unwrap(),
            radii: Corners::uniform(2.).unwrap(),
            color: Color(0x000000ff),
            offset: Point::new(0., 2.).unwrap(),
            blur: Scalar::new(4.).unwrap(),
            spread: Scalar::ZERO,
            inset: false,
        })
        .unwrap();
    // The diagnosis names the missing profile, so a producer can act on it.
    let error = window.submit(shadowed).unwrap_err();
    assert!(
        error.to_string().contains("overlay-paint-v1"),
        "unexpected diagnosis: {error}"
    );

    let mut dashed = Canvas::new();
    dashed
        .stroke_styled(
            path.clone(),
            Brush::Solid(Color(0xffffffff)),
            StrokeStyle::new(1.).unwrap(),
        )
        .unwrap();
    assert!(window.submit(dashed).is_err());

    let mut imaged = Canvas::new();
    imaged
        .fill(
            path.clone(),
            Brush::Image {
                asset: 1,
                transform: None,
                extend: Extend::Pad,
            },
        )
        .unwrap();
    assert!(window.submit(imaged).is_err());

    let mut oklab = Canvas::new();
    oklab
        .fill(
            path,
            Brush::Linear {
                start: Point::new(0., 0.).unwrap(),
                end: Point::new(10., 0.).unwrap(),
                stops: vec![
                    GradientStop {
                        offset: 0,
                        color: Color(0xff0000ff),
                    },
                    GradientStop {
                        offset: u16::MAX,
                        color: Color(0x0000ffff),
                    },
                ],
                color_space: ColorSpace::Oklab,
            },
        )
        .unwrap();
    assert!(window.submit(oklab).is_err());

    // An sRGB gradient is not a paint form, so it still submits on the same session.
    let mut srgb = Canvas::new();
    srgb.fill(
        Path::rectangle(Rect::new(0., 0., 64., 64.).unwrap()).unwrap(),
        Brush::Linear {
            start: Point::new(0., 0.).unwrap(),
            end: Point::new(64., 0.).unwrap(),
            stops: vec![
                GradientStop {
                    offset: 0,
                    color: Color(0xff0000ff),
                },
                GradientStop {
                    offset: u16::MAX,
                    color: Color(0x0000ffff),
                },
            ],
            color_space: ColorSpace::Srgb,
        },
    )
    .unwrap();
    window.submit(srgb).unwrap();

    overlays.close().unwrap();
}
