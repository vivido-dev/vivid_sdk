//! Drawing refusal must never act on a stale or unrelated source incarnation.

// Rust guideline compliant 2026-10-07
use super::*;
use std::time::Instant;

fn accepted(window: &OverlayWindow, revision: u64) {
    let until = Instant::now() + Duration::from_secs(3);
    while window.reconcile().unwrap().accepted_revision != revision {
        assert!(Instant::now() < until, "source acceptance stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn refusing_one_prepared_drawing_preserves_the_same_id_peer_and_original_receipt() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let red = a.upload_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
    let blue = b.upload_rgba(1, 1, &[0, 0, 255, 255]).unwrap();
    let mut first_scene = scene(0xff0000ff);
    let mut peer_scene = scene(0x0000ffff);
    a.draw_image(
        &mut first_scene,
        &red,
        Rect::new(0., 0., 1., 1.).unwrap(),
        u16::MAX,
    )
    .unwrap();
    b.draw_image(
        &mut peer_scene,
        &blue,
        Rect::new(0., 0., 1., 1.).unwrap(),
        u16::MAX,
    )
    .unwrap();
    let rejected = a.submit(first_scene).unwrap();
    let surviving = b.submit(peer_scene).unwrap();
    let frame = prepare(&mut host, 2);
    let bad = frame.drawings()[0].window;
    let good = frame.drawings()[1].window;
    let compiled = frame.drawings()[1].compiled.clone();
    assert!(compiled.retained_resource_count() > 1);
    assert_eq!(
        frame.drawings()[0].submission,
        frame.drawings()[1].submission
    );
    assert_ne!(bad, good, "identical local IDs belong to different owners");
    // Only the refused source is fenced. A peer's concurrent placement update
    // invalidates whole-frame admission but cannot protect the offending source.
    b.set_bounds(Rect::new(65., 0., 48., 48.).unwrap()).unwrap();
    assert!(host.reject_drawing(&frame, bad).unwrap());
    assert!(!host.reject_drawing(&frame, bad).unwrap());
    assert!(!host.presented(&frame).unwrap());
    assert!(a.reconcile().is_err());
    assert_eq!(surviving.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(b.reconcile().unwrap().presented_revision, 0);
    assert_eq!(
        rejected.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    drop(frame);
    let fresh = prepare(&mut host, 1);
    assert_eq!(fresh.drawings()[0].window, good);
    assert_eq!(fresh.drawings()[0].bounds.origin.x.get(), 65.);
    assert!(Arc::ptr_eq(&fresh.drawings()[0].compiled, &compiled));
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        surviving.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn delayed_refusal_cannot_close_new_content_or_a_replaced_track() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.submit(scene(0xff0000ff)).unwrap();
    let surviving = b.submit(scene(0x0000ffff)).unwrap();
    let old = prepare(&mut host, 2);
    let bad = old.drawings()[0].window;
    let replacement = a.submit(scene(0x00ff00ff)).unwrap();
    accepted(&a, replacement.revision());
    assert!(!host.reject_drawing(&old, bad).unwrap());
    assert_eq!(
        a.reconcile().unwrap().accepted_revision,
        replacement.revision()
    );
    assert_eq!(surviving.wait(Duration::ZERO).unwrap(), None);
    drop(old);
    let old = prepare(&mut host, 2);
    let track = old.drawings()[0].submission.track_id;
    let replacement = a.replace_track(scene(0xffff00ff)).unwrap();
    accepted(&a, replacement.revision());
    assert!(!host.reject_drawing(&old, bad).unwrap());
    drop(old);
    let current = prepare(&mut host, 2);
    assert_ne!(current.drawings()[0].submission.track_id, track);
    assert!(host.reject_drawing(&current, bad).unwrap());
    assert_eq!(surviving.wait(Duration::ZERO).unwrap(), None);
    drop(current);
    let fresh = prepare(&mut host, 1);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        surviving.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn foreign_displayed_and_retargeted_frames_cannot_close_a_current_window() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.submit(scene(0xff0000ff)).unwrap();
    let surviving = b.submit(scene(0x0000ffff)).unwrap();
    let frame = prepare(&mut host, 2);
    let bad = frame.drawings()[0].window;
    let (_foreign_presenter, mut foreign) = fixture();
    assert!(foreign.reject_drawing(&frame, bad).is_err());
    assert!(host.presented(&frame).unwrap());
    assert!(!host.reject_drawing(&frame, bad).unwrap());
    drop(frame);
    let frame = prepare(&mut host, 2);
    presenter.update_metrics(1, 17, 16, (8, 8));
    assert!(!host.reject_drawing(&frame, bad).unwrap());
    assert!(a.reconcile().is_ok());
    assert_eq!(
        b.reconcile().unwrap().presented_revision,
        surviving.revision()
    );
    drop(frame);
    let frame = prepare(&mut host, 2);
    host.set_visible(1, false).unwrap();
    assert!(!host.reject_drawing(&frame, bad).unwrap());
    assert!(a.reconcile().is_ok());
    first.close().unwrap();
    peer.close().unwrap();
}

#[cfg(feature = "vector-offscreen")]
#[test]
#[ignore = "requires native GPU; exact-source refusal after isolated checked work failure"]
fn native_work_refusal_closes_only_the_proven_drawing_and_peer_renders_again() {
    use vivid_sdk::vector_renderer::compositor::{Error, offscreen::Offscreen};
    let (presenter, mut host) = fixture();
    presenter.update_metrics(1, 1024, 1024, (1, 1));
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = first
        .create_window(OverlayWindowOptions::new(
            Rect::new(0., 0., 1024., 1024.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    let b = peer
        .create_window(OverlayWindowOptions::new(
            Rect::new(960., 0., 64., 64.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    let mut canvas = Canvas::new();
    for _ in 0..2048 {
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 1024., 1024.).unwrap()).unwrap(),
                Brush::Solid(Color(0xff000080)),
            )
            .unwrap();
    }
    let rejected = a.submit(canvas).unwrap();
    let surviving = b.submit(scene(0x0000ffff)).unwrap();
    accepted(&a, rejected.revision());
    accepted(&b, surviving.revision());
    let frame = host.prepare(1, 8.).unwrap();
    assert_eq!(frame.drawings().len(), 2);
    assert_eq!(
        frame.drawings()[0].submission,
        frame.drawings()[1].submission
    );
    let bad = frame.drawings()[0].window;
    let good = frame.drawings()[1].window;
    let compiled = frame.drawings()[1].compiled.clone();
    let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
    // The failure is attributed by rendering this drawing alone, not by
    // guessing which owner caused a mixed composition to exceed its budget.
    gpu.render(&frame.drawings()[..1], 8192, 8192).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let before = gpu.metrics().render_passes;
    let mut controls = 0;
    let error = loop {
        assert_eq!(b.reconcile().unwrap().presented_revision, 0);
        controls += 1;
        match gpu.poll() {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("isolated hostile drawing unexpectedly completed"),
        }
        assert!(Instant::now() < until, "isolated work refusal stalled");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(
        error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
        Some(Error::WorkLimit)
    ));
    assert_eq!(gpu.metrics().render_passes - before, 64);
    assert!(!gpu.is_failed());
    assert!(controls > 0);
    assert!(host.reject_drawing(&frame, bad).unwrap());
    assert_eq!(surviving.wait(Duration::ZERO).unwrap(), None);
    assert!(!host.presented(&frame).unwrap());
    drop(frame);
    let fresh = prepare(&mut host, 1);
    assert_eq!(fresh.drawings()[0].window, good);
    assert!(Arc::ptr_eq(&fresh.drawings()[0].compiled, &compiled));
    gpu.render(fresh.drawings(), 1024, 1024).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let pixels = loop {
        if let Some(pixels) = gpu.poll().unwrap() {
            assert_eq!((pixels.width, pixels.height), (1024, 1024));
            break pixels;
        }
        assert!(Instant::now() < until, "surviving peer readback stalled");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(&pixels.rgba[(16 * 1024 + 16) * 4..][..4], &[0; 4]);
    assert_eq!(
        &pixels.rgba[(16 * 1024 + 980) * 4..][..4],
        &[0, 0, 255, 255]
    );
    assert_eq!(surviving.wait(Duration::ZERO).unwrap(), None);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        surviving.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        rejected.wait(Duration::from_secs(2)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    eprintln!(
        "Exact drawing refusal: 64 checked attempts, {controls} peer controls, unchanged peer compilation and fresh transparent/blue output on the same device"
    );
    first.close().unwrap();
    peer.close().unwrap();
}
