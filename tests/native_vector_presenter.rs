//! Authenticated native composition receipts, resources and input isolation.

#![cfg(all(feature = "presenter", feature = "vector-render"))]

// Rust guideline compliant 2026-10-07
use std::{io, sync::Arc, time::Duration};
use vivid_protocol::overlay::wire::{PresentationOutcome, Submission};
use vivid_sdk::overlay::{
    Brush, Canvas, Color, Command, HitRole, Path, Rect, Transform, WindowMode,
};
use vivid_sdk::presenter::{
    MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
};
use vivid_sdk::{
    OverlayLaneEvent, OverlaySession, OverlayWindow, OverlayWindowOptions, ProducerAuthentication,
    ProducerConfig,
};

fn fixture() -> (Arc<VirtualVivid>, NativeVectorHost) {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    (presenter, host)
}
fn connect(presenter: &VirtualVivid) -> OverlaySession {
    let secret = presenter.issue_pane_capability(1).unwrap();
    OverlaySession::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..ProducerConfig::default()
    })
    .unwrap()
}
fn window(producer: &OverlaySession, x: f64) -> OverlayWindow {
    producer
        .create_window(OverlayWindowOptions::new(
            Rect::new(x, 0., 48., 48.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap()
}
fn scene(color: u32) -> Canvas {
    let mut canvas = Canvas::new();
    canvas
        .fill(
            Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
            Brush::Solid(Color(color)),
        )
        .unwrap();
    canvas
        .push(Command::Transform(
            Transform::new([1., 0., 0., 1., 20., 0.]).unwrap(),
        ))
        .unwrap();
    canvas
        .push(Command::Clip(
            Path::rectangle(Rect::new(0., 0., 10., 10.).unwrap()).unwrap(),
        ))
        .unwrap();
    canvas
        .push(Command::Hit {
            id: 7,
            path: Path::rectangle(Rect::new(0., 0., 30., 30.).unwrap()).unwrap(),
            role: HitRole::Input,
            cursor: Some(vivid_sdk::overlay::CursorShape::Pointer),
        })
        .unwrap();
    canvas
}
fn outcomes(producer: &OverlaySession) -> Vec<(Submission, PresentationOutcome)> {
    let mut outcomes = Vec::new();
    while let Some(event) = producer.wait_event(Duration::ZERO).unwrap() {
        if let OverlayLaneEvent::Outcome(value) = event {
            outcomes.push((value.submission, value.outcome));
        }
    }
    outcomes
}
fn prepare(host: &mut NativeVectorHost, count: usize) -> VectorFrame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.drawings().len() == count {
            return frame;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native scene did not become ready"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn only_displayed_frames_acknowledge_exact_submissions_and_enable_compiled_hits() {
    let (presenter, mut host) = fixture();
    assert_eq!(host.cursor(1), None);
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let submitted = a.submit(scene(0xff0000ff)).unwrap();
    let peer_submitted = b.submit(scene(0x0000ffff)).unwrap();
    assert_eq!(
        a.reconcile().unwrap().accepted_revision,
        submitted.revision()
    );
    assert_eq!(a.reconcile().unwrap().presented_revision, 0);
    assert!(
        !outcomes(&first)
            .iter()
            .any(|(_, outcome)| *outcome == PresentationOutcome::Presented)
    );
    assert!(
        !presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    let frame = prepare(&mut host, 2);
    assert_eq!(frame.dimensions(), (128, 128));
    assert_eq!(
        frame.drawings()[0].submission.address,
        frame.drawings()[1].submission.address,
        "owners deliberately reuse local identities"
    );
    assert_ne!(
        frame.drawings()[0].window.context().session(),
        frame.drawings()[1].window.context().session()
    );
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        a.reconcile().unwrap().presented_revision,
        submitted.revision()
    );
    assert_eq!(
        b.reconcile().unwrap().presented_revision,
        peer_submitted.revision()
    );
    assert_eq!(
        submitted.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    let _ = outcomes(&first);
    assert!(
        !presenter
            .overlay_pointer(1, 5., 2., Some((1, true)), 0)
            .unwrap(),
        "raw hit bounding boxes must not authorize input"
    );
    assert!(
        presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    assert_eq!(
        host.cursor(1),
        None,
        "raising a window requires fresh display proof"
    );
    assert!(
        !host.presented(&frame).unwrap(),
        "a receipt cannot resolve twice"
    );
    let _ = outcomes(&first);
    drop(frame);
    let frame = prepare(&mut host, 2);
    assert!(
        host.presented(&frame).unwrap(),
        "a reused GPU image can acknowledge a fresh displayed receipt"
    );
    assert_eq!(
        host.cursor(1),
        Some(vivid_sdk::overlay::CursorShape::Pointer)
    );
    assert_eq!(
        submitted.wait(Duration::ZERO).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn prepared_frames_survive_new_acceptance_but_not_moves_or_owner_retirement() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let old = a.submit(scene(0xff0000ff)).unwrap();
    b.submit(scene(0x0000ffff)).unwrap();
    let frame = prepare(&mut host, 2);
    let replacement = a.submit(scene(0x00ff00ff)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while a.reconcile().unwrap().accepted_revision < replacement.revision() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        old.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(replacement.wait(Duration::ZERO).unwrap(), None);
    drop(frame);
    let frame = prepare(&mut host, 2);
    a.set_bounds(Rect::new(1., 0., 48., 48.).unwrap()).unwrap();
    assert!(!host.presented(&frame).unwrap());
    drop(frame);
    let frame = prepare(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        replacement.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    drop(frame);
    let stale = prepare(&mut host, 2);
    let drawing = stale
        .drawings()
        .iter()
        .find(|drawing| drawing.bounds.origin.x.get() == 1.)
        .unwrap();
    let source = vivid_sdk::presenter::SourceKey {
        producer: drawing.window.context().session().session_id(),
        context: drawing.window.context().context_id(),
        surface: drawing.window.surface_id(),
        track: drawing.submission.track_id,
    };
    first.close().unwrap();
    // GOODBYE's reply precedes server retirement. Observe the owner fence before
    // asserting that a receipt from its former physical composition is stale.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while presenter.desktop_session_generation(source).is_some() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!host.presented(&stale).unwrap());
    drop(stale);
    let frame = prepare(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    let next = b.submit(scene(0xffffffff)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while b.reconcile().unwrap().accepted_revision < next.revision() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(frame);
    let frame = prepare(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(b.reconcile().unwrap().presented_revision, 2);
    peer.close().unwrap();
}

#[test]
fn outstanding_receipts_are_bounded_and_foreign_presenters_are_rejected() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<VectorFrame>();
    let (presenter, mut host) = fixture();
    let producer = connect(&presenter);
    let window = window(&producer, 0.);
    window.submit(scene(0xffffffff)).unwrap();
    let a = prepare(&mut host, 1);
    let b = prepare(&mut host, 1);
    assert!(Arc::ptr_eq(
        &a.drawings()[0].compiled,
        &b.drawings()[0].compiled
    ));
    assert_eq!(
        host.prepare(1, 1.).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let (_, mut foreign) = fixture();
    assert!(foreign.presented(&a).is_err());
    drop(a);
    let c = prepare(&mut host, 1);
    assert!(host.presented(&c).unwrap());
    assert!(!host.presented(&b).unwrap());
    assert!(host.prepare(1, f64::NAN).is_err());
    producer.close().unwrap();
}

#[test]
fn invalidation_revokes_capture_and_requires_a_fresh_displayed_receipt() {
    let (presenter, mut host) = fixture();
    let producer = connect(&presenter);
    let window = window(&producer, 0.);
    window.submit(scene(0xffffffff)).unwrap();
    let frame = prepare(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    drop(frame);
    assert!(
        presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    producer.capture_pointer(&window, true).unwrap();
    let stale = prepare(&mut host, 1);
    host.invalidate(1);
    assert_eq!(host.cursor(1), None);
    assert!(!host.presented(&stale).unwrap());
    drop(stale);
    assert!(
        !presenter
            .overlay_pointer(1, 22., 2., Some((1, false)), 0)
            .unwrap()
    );
    assert!(producer.capture_pointer(&window, true).is_err());
    let fresh = prepare(&mut host, 1);
    assert!(host.presented(&fresh).unwrap());
    assert!(
        presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    producer.close().unwrap();
}

#[test]
fn visibility_controller_revokes_old_receipts_without_a_render_tick_and_expires_with_its_host() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<vivid_sdk::presenter::NativePaneVisibility>();
    let (presenter, mut host) = fixture();
    let (peer_presenter, mut peer_host) = fixture();
    let producer = connect(&presenter);
    let peer = connect(&peer_presenter);
    let a = window(&producer, 0.);
    let b = window(&peer, 0.);
    a.submit(scene(0xff0000ff)).unwrap();
    b.submit(scene(0x0000ffff)).unwrap();
    let old = prepare(&mut host, 1);
    let peer_frame = prepare(&mut peer_host, 1);
    assert!(host.presented(&old).unwrap());
    assert!(peer_host.presented(&peer_frame).unwrap());
    let control = host.visibility_control(1).unwrap();
    let clone = control.clone();
    std::thread::spawn(move || {
        assert!(clone.set_visible(false).unwrap());
        assert!(!clone.set_visible(false).unwrap());
        assert!(clone.set_visible(true).unwrap());
    })
    .join()
    .unwrap();
    // The host did not poll or invalidate between these two transitions.
    assert!(!host.presented(&old).unwrap());
    assert!(
        !presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    assert!(
        peer_presenter
            .overlay_pointer(1, 22., 2., Some((1, true)), 0)
            .unwrap()
    );
    let fresh = prepare(&mut host, 1);
    assert!(!old.same_input_context(&fresh));
    assert!(host.presented(&fresh).unwrap());
    drop(host);
    assert!(!control.set_visible(false).unwrap());
    assert!(
        peer_presenter
            .overlay_pointer(1, 22., 2., Some((1, false)), 0)
            .unwrap()
    );
    producer.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn visibility_controller_registration_uses_the_same_bounded_pane_admission() {
    let (presenter, mut host) = fixture();
    assert!(host.visibility_control(0).is_err());
    assert!(host.visibility_control(2).is_err());
    for pane in 1..=65 {
        presenter.update_metrics(pane, 16, 16, (8, 8));
        if pane <= 64 {
            host.visibility_control(pane).unwrap();
        } else {
            assert!(host.visibility_control(pane).is_err());
        }
    }
    // Refusing a new pane does not invalidate an existing registration.
    assert!(
        host.visibility_control(1)
            .unwrap()
            .set_visible(false)
            .unwrap()
    );
}

#[test]
fn a_scene_rejected_by_the_renderer_closes_only_its_own_window() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let mut malformed = Canvas::new();
    malformed
        .push(Command::Transform(
            Transform::new([100_000., 0., 0., 100_000., 0., 0.]).unwrap(),
        ))
        .unwrap();
    malformed
        .fill(
            Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
            Brush::Solid(Color(0xffffffff)),
        )
        .unwrap();
    let rejected = a.submit(malformed).unwrap();
    let surviving = b.submit(scene(0x0000ffff)).unwrap();
    let frame = prepare(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        rejected.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(
        surviving.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert!(a.reconcile().is_err());
    assert_eq!(
        b.reconcile().unwrap().presented_revision,
        surviving.revision()
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[cfg(feature = "vector-offscreen")]
#[test]
#[ignore = "requires a working native GPU or software Vulkan; explicit native vector acceptance"]
fn native_host_renders_owner_scoped_images_and_retained_glyphs_with_real_gpu_readback() {
    use vivid_protocol::overlay::wire::text::styled::{StyledText, TextStyle};
    use vivid_sdk::vector_renderer::compositor::offscreen::{Offscreen, Pixels};
    fn pixels(gpu: &mut Offscreen) -> Pixels {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(pixels) = gpu.poll().unwrap() {
                return pixels;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native readback stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn pixel(pixels: &Pixels, x: u32, y: u32) -> &[u8] {
        let offset = ((y * pixels.width + x) * 4) as usize;
        &pixels.rgba[offset..offset + 4]
    }
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let red = a.upload_rgba(1, 1, &[255, 0, 0, 128]).unwrap();
    let blue = b.upload_rgba(1, 1, &[0, 0, 255, 255]).unwrap();
    assert_eq!(red.id(), blue.id(), "owners deliberately reuse asset IDs");
    let mut canvas = Canvas::new();
    a.draw_image(
        &mut canvas,
        &red,
        Rect::new(0., 0., 48., 48.).unwrap(),
        u16::MAX,
    )
    .unwrap();
    let a_receipt = a.submit(canvas).unwrap();
    let mut canvas = Canvas::new();
    b.draw_image(
        &mut canvas,
        &blue,
        Rect::new(0., 0., 48., 48.).unwrap(),
        u16::MAX,
    )
    .unwrap();
    let b_receipt = b.submit(canvas).unwrap();
    let frame = prepare(&mut host, 2);
    let mut gpu =
        pollster::block_on(Offscreen::new()).expect("native GPU acceptance requires an adapter");
    assert!(gpu.render(frame.drawings(), 128, 128).unwrap());
    assert_eq!(
        gpu.render(frame.drawings(), 128, 128).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let rendered = pixels(&mut gpu);
    assert_eq!(rendered.rgba.len(), 128 * 128 * 4);
    assert_eq!(
        pixel(&rendered, 20, 20),
        [255, 0, 0, 128],
        "readback must unpremultiply native pixels"
    );
    assert_eq!(pixel(&rendered, 70, 20), [0, 0, 255, 255]);
    assert_eq!(pixel(&rendered, 60, 20), [0, 0, 0, 0]);
    assert_eq!(
        a_receipt.wait(Duration::ZERO).unwrap(),
        None,
        "GPU completion alone cannot acknowledge presentation"
    );
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        a_receipt.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        b_receipt.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert!(!gpu.render(frame.drawings(), 128, 128).unwrap());
    assert_eq!(gpu.metrics().render_passes, 1);
    assert_eq!(gpu.metrics().skipped_passes, 1);
    // Numeric identities can be reused after destruction. A new immutable scene
    // must redraw even when every public submission/cache-key field is equal.
    let mut replacement = frame.drawings().to_vec();
    let mut green = Canvas::new();
    green
        .fill(
            Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
            Brush::Solid(Color(0x00ff00ff)),
        )
        .unwrap();
    replacement[0].compiled = Arc::new(
        vivid_sdk::vector_renderer::compile(
            &green,
            &mut vivid_sdk::vector_renderer::text::FontSystem::new("").unwrap(),
            &std::collections::BTreeMap::new(),
        )
        .unwrap(),
    );
    assert!(gpu.render(&replacement, 128, 128).unwrap());
    let replaced = pixels(&mut gpu);
    assert_eq!(pixel(&replaced, 20, 20), [0, 255, 0, 255]);
    assert_eq!(pixel(&replaced, 70, 20), [0, 0, 255, 255]);
    let layout = std::thread::scope(|scope| {
        let task = scope.spawn(|| {
            b.layout_text(&StyledText::new(
                "Wi",
                TextStyle {
                    color: Color(0x000000ff),
                    ..TextStyle::default()
                },
            ))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !task.is_finished() {
            assert!(host.poll().is_none());
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        task.join().unwrap().unwrap()
    });
    let mut text = Canvas::new();
    text.fill(
        Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
        Brush::Solid(Color(0xffffffff)),
    )
    .unwrap();
    b.draw_text_layout(
        &mut text,
        &layout,
        vivid_sdk::overlay::Point::new(2., 2.).unwrap(),
    )
    .unwrap();
    let glyphs = b.submit(text).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while b.reconcile().unwrap().accepted_revision < glyphs.revision() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(frame);
    let frame = prepare(&mut host, 2);
    assert!(gpu.render(frame.drawings(), 128, 128).unwrap());
    let rendered = pixels(&mut gpu);
    let ink = (2..24)
        .flat_map(|y| (66..90).map(move |x| (x, y)))
        .filter(|(x, y)| pixel(&rendered, *x, *y)[0] < 200)
        .count();
    assert!(
        ink > 10,
        "retained SDK font glyphs must produce visible pixels"
    );
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        glyphs.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    if let Some(path) = std::env::var_os("VIVID_VECTOR_CAPTURE") {
        use std::io::Write;
        let mut output = std::fs::File::create(path).unwrap();
        write!(output, "P6\n{} {}\n255\n", rendered.width, rendered.height).unwrap();
        for pixel in rendered.rgba.as_chunks::<4>().0 {
            output.write_all(&pixel[..3]).unwrap();
        }
    }
    first.close().unwrap();
    peer.close().unwrap();
    drop(host);
    // The frame owns all font/image data after the presenter, font context and
    // resource namespaces retire; a fresh GPU renderer can still draw it.
    drop(gpu);
    let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
    assert!(gpu.render(frame.drawings(), 128, 128).unwrap());
    let retained = pixels(&mut gpu);
    assert_eq!(&*retained.rgba, &*rendered.rgba);
}

#[test]
fn newer_physical_display_and_dropped_reservations_resolve_only_their_original_scenes() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let one = a.submit(scene(0xff0000ff)).unwrap();
    let peer_one = b.submit(scene(0x0000ffff)).unwrap();
    let old = prepare(&mut host, 2);
    assert_eq!(
        old.drawings()[0].submission.address,
        old.drawings()[1].submission.address
    );
    assert_ne!(
        old.drawings()[0].window.context().session(),
        old.drawings()[1].window.context().session()
    );
    let two = a.submit(scene(0x00ff00ff)).unwrap();
    let accepted = std::time::Instant::now() + Duration::from_secs(3);
    while a.reconcile().unwrap().accepted_revision != two.revision() {
        assert!(std::time::Instant::now() < accepted);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(one.wait(Duration::ZERO).unwrap(), None);
    let fresh = prepare(&mut host, 2);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        two.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        peer_one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert!(!host.presented(&old).unwrap());
    drop(old);
    drop(fresh);
    let three = a.submit(scene(0xff00ffff)).unwrap();
    let accepted = std::time::Instant::now() + Duration::from_secs(3);
    while a.reconcile().unwrap().accepted_revision != three.revision() {
        assert!(std::time::Instant::now() < accepted);
        std::thread::sleep(Duration::from_millis(1));
    }
    let abandoned = prepare(&mut host, 2);
    let four = a.submit(scene(0x00ffffff)).unwrap();
    let accepted = std::time::Instant::now() + Duration::from_secs(3);
    while a.reconcile().unwrap().accepted_revision != four.revision() {
        assert!(std::time::Instant::now() < accepted);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(three.wait(Duration::ZERO).unwrap(), None);
    drop(abandoned);
    host.poll();
    assert_eq!(
        three.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(four.wait(Duration::ZERO).unwrap(), None);
    let fresh = prepare(&mut host, 2);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        four.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    first.close().unwrap();
    peer.close().unwrap();
}
