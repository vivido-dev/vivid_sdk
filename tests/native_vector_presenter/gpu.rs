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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pixels) = gpu.poll().unwrap() {
            assert_eq!((pixels.width, pixels.height), (128, 128));
            assert_eq!(pixels.rgba.len(), 128 * 128 * 4);
            return pixels;
        }
        assert!(Instant::now() < deadline, "native GPU readback stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
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
