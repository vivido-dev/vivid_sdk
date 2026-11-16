//! Authenticated lane overflow is isolated from a same-ID peer and control.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::overlay::{Brush, Canvas, Color, Path, Rect, WindowMode};
use crate::{OverlaySession, OverlayWindowOptions, ProducerAuthentication, ProducerConfig};

#[test]
fn outcome_overflow_closes_only_the_authenticated_owner_lane() {
    exercise(false);
}
#[cfg(feature = "vector-offscreen")]
#[test]
#[ignore = "requires a native GPU; owner-isolated backlog overflow and peer display"]
fn native_gpu_backlog_overflow_keeps_peer_pixels_and_rejects_retired_receipt() {
    exercise(true);
}
fn exercise(native: bool) {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            super::super::SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    #[cfg(not(feature = "vector-offscreen"))]
    assert!(!native);
    #[cfg(feature = "vector-offscreen")]
    let mut host =
        native.then(|| super::super::NativeVectorHost::attach(presenter.clone(), "").unwrap());
    #[cfg(feature = "vector-offscreen")]
    let mut gpu = native.then(|| {
        pollster::block_on(crate::vector_renderer::compositor::offscreen::Offscreen::new()).unwrap()
    });
    presenter.update_metrics(7, 40, 4, (8, 16));
    let mut owners = Vec::new();
    for index in 0..2 {
        let secret = presenter.issue_pane_capability(7).unwrap();
        let endpoint = presenter.endpoint();
        let session = OverlaySession::connect(ProducerConfig {
            endpoint_control: Some(endpoint.clone()),
            endpoint_interactive: Some(endpoint.clone()),
            endpoint_realtime: Some(endpoint.clone()),
            endpoint_bulk: Some(endpoint),
            authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
            ..Default::default()
        })
        .unwrap();
        let window = session
            .create_window(OverlayWindowOptions::new(
                Rect::new(f64::from(index) * 64., 0., 64., 64.).unwrap(),
                WindowMode::Floating,
            ))
            .unwrap();
        let mut canvas = Canvas::new();
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 64., 64.).unwrap()).unwrap(),
                Brush::Solid(Color(0xff0000ff)),
            )
            .unwrap();
        window.present(canvas.clone()).unwrap();
        owners.push((session, window, canvas));
    }
    let snapshot = presenter.projection_snapshot(&HashSet::from([7]));
    let mut sources = snapshot
        .sources
        .iter()
        .filter(|s| matches!(s.descriptor, SourceDescriptor::VectorScene(_)))
        .collect::<Vec<_>>();
    sources.sort_by_key(|s| s.key.producer);
    assert_eq!(sources.len(), 2);
    let a = sources[0].key;
    let b = sources[1].key;
    assert_ne!(a.producer, b.producer);
    assert_eq!(
        (a.context, a.surface, a.track),
        (b.context, b.surface, b.track)
    );
    let (lane_a, lane_b, control_a, control_b) = {
        let state = lock(&presenter.state);
        (
            state.sessions[&a.producer].overlay_lane.clone().unwrap(),
            state.sessions[&b.producer].overlay_lane.clone().unwrap(),
            state.sessions[&a.producer].writer.clone(),
            state.sessions[&b.producer].writer.clone(),
        )
    };
    #[cfg(feature = "vector-offscreen")]
    let retired = host
        .as_mut()
        .map(|host| compose(host, gpu.as_mut().unwrap(), b.producer, 1, false));
    lane_a.flush().unwrap();
    lane_b.flush().unwrap();
    // Model a service burst while the state lock prevents flushing to either lane.
    {
        let mut state = lock(&presenter.state);
        let frame = |revision| {
            vivid_protocol::vector::Frame {
                epoch: 1,
                revision,
                canvas: owners[0].2.clone(),
            }
            .encode()
            .unwrap()
        };
        let id = super::super::overlay_host::owner(a.producer)
            .context(a.context)
            .unwrap()
            .surface(a.surface)
            .unwrap();
        for revision in 2..=257 {
            let body = frame(revision);
            assert!(
                state
                    .overlays
                    .media_record_for(id, messages::VECTOR_FRAME, a.track, 1, &body)
            );
            let track = state
                .tracks
                .get_mut(&TrackKey {
                    surface: SurfaceKey {
                        session: a.producer,
                        context: a.context,
                        surface: a.surface,
                    },
                    track: a.track,
                })
                .unwrap();
            track
                .retained_vector
                .accept(messages::VECTOR_FRAME, &body)
                .unwrap();
            track.retained = track.retained_vector.frame();
        }
        assert!(!lane_a.is_closed());
        assert!(!state.overlays.media_record_for(
            id,
            messages::VECTOR_FRAME,
            a.track,
            1,
            &frame(258)
        ));
        // Exercise the production overflow-to-transport closure path.
        flush_overlay_lane(&mut state, a.producer);
        assert!(state.sessions[&a.producer].overlay_lane.is_none());
        assert!(!state.overlays.input_enabled(a.producer));
        assert!(state.sessions[&b.producer].overlay_lane.is_some());
        assert!(state.overlays.input_enabled(b.producer));
    }
    assert!(lane_a.is_closed());
    assert!(!lane_b.is_closed());
    assert!(!control_a.is_closed());
    assert!(!control_b.is_closed());
    let peer = owners
        .iter()
        .find(|(_, w, _)| w.reconcile().is_ok_and(|s| s.accepted_revision == 1))
        .unwrap();
    #[cfg(feature = "vector-offscreen")]
    if let Some(host) = &mut host {
        assert!(!host.presented(retired.as_ref().unwrap()).unwrap());
    }
    #[cfg(feature = "vector-offscreen")]
    drop(retired);
    let mut blue = Canvas::new();
    blue.fill(
        Path::rectangle(Rect::new(0., 0., 64., 64.).unwrap()).unwrap(),
        Brush::Solid(Color(0x0000ffff)),
    )
    .unwrap();
    let peer_receipt = peer.1.submit(blue).unwrap();
    #[cfg(feature = "vector-offscreen")]
    if let Some(host) = &mut host {
        drop(compose(host, gpu.as_mut().unwrap(), b.producer, 2, true));
    }
    // Later media and control queries use independent lanes. Sending a frame
    // does not prove it has been accepted or presented before QUERY_OVERLAY.
    assert_eq!(
        peer_receipt.wait(Duration::from_secs(3)).unwrap(),
        Some(vivid_protocol::overlay::wire::PresentationOutcome::Presented)
    );
    assert_eq!(peer.1.reconcile().unwrap().presented_revision, 2);
    presenter.shutdown();
}

#[cfg(feature = "vector-offscreen")]
fn compose(
    host: &mut super::super::NativeVectorHost,
    gpu: &mut crate::vector_renderer::compositor::offscreen::Offscreen,
    peer: u64,
    revision: u64,
    blue: bool,
) -> super::super::VectorFrame {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(7, 1.).unwrap();
        if frame.drawings().len() == if blue { 1 } else { 2 }
            && frame.drawings().iter().any(|d| {
                d.window.context().session().session_id() == peer
                    && d.submission.revision == revision
            })
        {
            assert_eq!(frame.dimensions(), (320, 64));
            assert!(gpu.render(frame.drawings(), 320, 64).unwrap());
            let image = loop {
                if let Some(image) = gpu.poll().unwrap() {
                    break image;
                }
                assert!(Instant::now() < end, "backlog GPU readback stalled");
                thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(image.rgba.len(), 320 * 64 * 4);
            for y in 0..64 {
                for x in 0..320 {
                    let expected = if x < 64 {
                        if blue { [0; 4] } else { [255, 0, 0, 255] }
                    } else if x < 128 {
                        if blue {
                            [0, 0, 255, 255]
                        } else {
                            [255, 0, 0, 255]
                        }
                    } else {
                        [0; 4]
                    };
                    let at = (y * 320 + x) * 4;
                    assert_eq!(
                        &image.rgba[at..at + 4],
                        &expected,
                        "backlog pixels at {x},{y}"
                    );
                }
            }
            assert!(host.presented(&frame).unwrap());
            return frame;
        }
        assert!(
            Instant::now() < end,
            "peer composition stalled: revision={revision} blue={blue}, frame={frame:?}, drawings={:?}",
            frame
                .drawings()
                .iter()
                .map(|d| (
                    d.window.context().session().session_id(),
                    d.submission.revision,
                    d.bounds
                ))
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(1));
    }
}
