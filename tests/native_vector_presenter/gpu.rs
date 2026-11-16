//! Native GPU readback across reserved scenes and exact owner retirement.

// Rust guideline compliant 2026-10-07
use super::{connect, fixture, prepare, scene, window};
use std::time::{Duration, Instant};
use vivid_protocol::overlay::wire::PresentationOutcome;
use vivid_sdk::vector_renderer::compositor::offscreen::{Offscreen, Pixels};
use vivid_sdk::{OverlaySubmission, OverlayWindow};

fn accepted(window: &OverlayWindow, receipt: &OverlaySubmission) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while window.reconcile().unwrap().accepted_revision != receipt.revision() {
        assert!(Instant::now() < deadline, "source scene acceptance stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn pixels(gpu: &mut Offscreen) -> Pixels {
    sized_pixels(gpu, 128, 128)
}

fn sized_pixels(gpu: &mut Offscreen, width: u32, height: u32) -> Pixels {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pixels) = gpu.poll().unwrap() {
            assert_eq!((pixels.width, pixels.height), (width, height));
            assert_eq!(pixels.rgba.len(), width as usize * height as usize * 4);
            return pixels;
        }
        assert!(Instant::now() < deadline, "native GPU readback stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a native GPU; exact large-target tiled composition qualification"]
fn native_gpu_tiles_maximum_target_and_preserves_same_id_peer_after_close() {
    use super::{Brush, Canvas, Color, OverlayWindowOptions, Path, Rect, WindowMode};
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
            Rect::new(512., 0., 512., 1024.).unwrap(),
            WindowMode::Floating,
        ))
        .unwrap();
    let mut left = Canvas::new();
    for (rect, color) in [
        (Rect::new(0., 0., 1024., 1024.).unwrap(), 0xff0000ff),
        (Rect::new(0., 512., 1024., 512.).unwrap(), 0x00ff00ff),
    ] {
        left.fill(Path::rectangle(rect).unwrap(), Brush::Solid(Color(color)))
            .unwrap();
    }
    let mut right = Canvas::new();
    right
        .fill(
            Path::rectangle(Rect::new(0., 0., 512., 1024.).unwrap()).unwrap(),
            Brush::Solid(Color(0x4080c080)),
        )
        .unwrap();
    let one = a.submit(left).unwrap();
    let blue = b.submit(right).unwrap();
    let frame = |host: &mut vivid_sdk::presenter::NativeVectorHost, count| {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let frame = host.prepare(1, 8.).unwrap();
            if frame.drawings().len() == count {
                break frame;
            }
            assert!(Instant::now() < end, "large native sources did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let old = frame(&mut host, 2);
    assert_eq!(old.dimensions(), (8192, 8192));
    assert_eq!(
        old.drawings()[0].submission.address,
        old.drawings()[1].submission.address
    );
    assert_eq!(
        old.drawings()[0].submission.track_id,
        old.drawings()[1].submission.track_id
    );
    assert_ne!(
        old.drawings()[0].window.context().session(),
        old.drawings()[1].window.context().session()
    );
    let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
    assert!(gpu.render(old.drawings(), 8192, 8192).unwrap());
    let image = sized_pixels(&mut gpu, 8192, 8192);
    let pixel = |image: &Pixels, x: usize, y: usize| -> [u8; 4] {
        image.rgba[(y * 8192 + x) * 4..(y * 8192 + x) * 4 + 4]
            .try_into()
            .unwrap()
    };
    for y in [0, 4094, 4095, 4096, 4097, 8191] {
        for x in [0, 4094, 4095, 4096, 4097, 8191] {
            let expected = if x < 4096 {
                if y < 4096 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                }
            } else if y < 4096 {
                [159, 64, 96, 255]
            } else {
                [32, 191, 96, 255]
            };
            let actual = pixel(&image, x, y);
            assert!(
                actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 1),
                "tile seam {x},{y}: {actual:?} != {expected:?}"
            );
        }
    }
    drop(image);
    assert_eq!(gpu.metrics().render_passes, 4);
    assert_eq!(gpu.metrics().target_allocations, 1);
    assert!(!gpu.render(old.drawings(), 8192, 8192).unwrap());
    assert_eq!(gpu.metrics().skipped_passes, 1);
    assert_eq!(one.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(blue.wait(Duration::ZERO).unwrap(), None);
    assert!(host.presented(&old).unwrap());
    assert_eq!(
        one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        blue.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    assert!(!host.presented(&old).unwrap());
    let survivor = frame(&mut host, 1);
    assert!(gpu.render(survivor.drawings(), 8192, 8192).unwrap());
    let image = sized_pixels(&mut gpu, 8192, 8192);
    for y in [0, 4095, 4096, 4097, 8191] {
        for x in [0, 4095, 4096, 4097, 8191] {
            let actual = pixel(&image, x, y);
            let expected = if x < 4096 {
                [0; 4]
            } else {
                [64, 128, 192, 128]
            };
            assert!(
                actual
                    .into_iter()
                    .zip(expected)
                    .all(|(a, e)| a.abs_diff(e) <= 1),
                "retired owner or peer changed at tile seam {x},{y}: {actual:?} != {expected:?}"
            );
        }
    }
    assert_eq!(gpu.metrics().render_passes, 8);
    assert!(host.presented(&survivor).unwrap());
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    peer.close().unwrap();
    presenter.shutdown();
}

fn colors(pixels: &Pixels, left: [u8; 4]) {
    let pixel = |x: usize| {
        let offset = (20 * 128 + x) * 4;
        &pixels.rgba[offset..offset + 4]
    };
    assert_eq!(pixel(20), left);
    assert_eq!(pixel(70), [0, 0, 255, 255], "same-ID peer changed");
    assert_eq!(pixel(60), [0; 4], "composition escaped its window bounds");
}

#[test]
#[ignore = "requires a native GPU; explicit prepared-scene readback qualification"]
fn native_gpu_preserves_reserved_pixels_and_same_id_peer_after_owner_retirement() {
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
        old.drawings()[1].submission.address,
        "owners must reuse their local context and surface IDs"
    );
    assert_eq!(
        old.drawings()[0].submission.track_id,
        old.drawings()[1].submission.track_id
    );
    assert_ne!(
        old.drawings()[0].window.context().session(),
        old.drawings()[1].window.context().session()
    );

    let two = a.submit(scene(0x00ff00ff)).unwrap();
    accepted(&a, &two);
    let newer = prepare(&mut host, 2);
    let mut gpu = pollster::block_on(Offscreen::new()).expect("native GPU adapter required");
    assert!(gpu.render(old.drawings(), 128, 128).unwrap());
    colors(&pixels(&mut gpu), [255, 0, 0, 255]);
    assert_eq!(one.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(peer_one.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(two.wait(Duration::ZERO).unwrap(), None);
    assert!(host.presented(&old).unwrap());
    assert_eq!(
        one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        peer_one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    drop(old);

    assert!(gpu.render(newer.drawings(), 128, 128).unwrap());
    colors(&pixels(&mut gpu), [0, 255, 0, 255]);
    assert_eq!(two.wait(Duration::ZERO).unwrap(), None);
    assert!(host.presented(&newer).unwrap());
    assert_eq!(
        two.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    drop(newer);

    let three = a.submit(scene(0xff00ffff)).unwrap();
    accepted(&a, &three);
    let abandoned = prepare(&mut host, 2);
    let four = a.submit(scene(0x00ffffff)).unwrap();
    accepted(&a, &four);
    assert_eq!(three.wait(Duration::ZERO).unwrap(), None);
    drop(abandoned);
    host.poll();
    assert_eq!(
        three.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(four.wait(Duration::ZERO).unwrap(), None);
    let fresh = prepare(&mut host, 2);
    assert!(gpu.render(fresh.drawings(), 128, 128).unwrap());
    colors(&pixels(&mut gpu), [0, 255, 255, 255]);
    assert_eq!(four.wait(Duration::ZERO).unwrap(), None);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        four.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );

    first.close().unwrap();
    assert!(!host.presented(&fresh).unwrap());
    drop(fresh);
    let survivor = prepare(&mut host, 1);
    assert!(gpu.render(survivor.drawings(), 128, 128).unwrap());
    colors(&pixels(&mut gpu), [0; 4]);
    assert!(host.presented(&survivor).unwrap());
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    peer.close().unwrap();
}
