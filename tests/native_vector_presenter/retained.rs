//! Retained checked scenes must never acknowledge pending replacements or same-ID peers.

// Rust guideline compliant 2026-10-07
use super::*;
use std::time::Instant;

fn accepted(window: &OverlayWindow, revision: u64) {
    let until = Instant::now() + Duration::from_secs(3);
    while window.reconcile().unwrap().accepted_revision != revision {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn text(host: &mut NativeVectorHost, frame: &VectorFrame, peer: &OverlaySession, revision: u64) {
    use vivid_sdk::presenter::{VectorInputEvent, VectorInputResult};
    let expected = format!("retained peer Ω {revision}");
    let target = host.input_target(frame).unwrap();
    assert_eq!(
        host.dispatch_input(&target, VectorInputEvent::Text(expected.clone()))
            .unwrap(),
        VectorInputResult::Delivered { text: true }
    );
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match peer.wait_event(Duration::from_millis(1)).unwrap() {
            Some(OverlayLaneEvent::Outcome(outcome)) => {
                panic!("duplicate or unregistered outcome: {outcome:?}")
            }
            Some(OverlayLaneEvent::Input(input)) => {
                if let vivid_protocol::overlay::Event::Text(text) = input.event {
                    assert_eq!(text, expected);
                    assert_eq!(input.scene_revision, revision);
                    assert_eq!(
                        (
                            input.address.context_id,
                            input.address.surface_id,
                            input.address.generation
                        ),
                        (1, 1, 1)
                    );
                    return;
                }
            }
            _ => (),
        }
        assert!(Instant::now() < until);
    }
}

fn exercise(mut render: impl FnMut(&VectorFrame, [u8; 4])) {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    let image = a.upload_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
    let mut red = Canvas::new();
    a.draw_image(
        &mut red,
        &image,
        Rect::new(0., 0., 48., 48.).unwrap(),
        u16::MAX,
    )
    .unwrap();
    b.request_focus().unwrap();
    presenter.set_overlay_pane_focus(1, true);
    let one = a.submit(red).unwrap();
    let two = b.submit(scene(0x0000ffff)).unwrap();
    let mut displayed = prepare(&mut host, 2);
    render(&displayed, [0, 0, 255, 255]);
    assert!(host.presented(&displayed).unwrap());
    for receipt in [&one, &two] {
        assert_eq!(
            receipt.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
    }
    let selected = displayed.drawings()[0].window;
    assert_eq!(
        displayed.drawings()[0].submission,
        displayed.drawings()[1].submission
    );
    assert_ne!(selected, displayed.drawings()[1].window);
    let pinned = displayed.drawings()[0].compiled.clone();
    let pending_bad = a.submit(scene(0x000000ff)).unwrap();
    let pending_peer = b.submit(scene(0x00ff00ff)).unwrap();
    accepted(&a, 2);
    accepted(&b, 2);
    a.release_image(&image).unwrap();
    let diagnosis = prepare(&mut host, 2);
    assert_eq!(
        host.prepare(1, 1.).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut frame = host
        .prepare_retaining(1, 1., &displayed, &[selected])
        .unwrap();
    assert_eq!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(frame.drawings()[0].submission.revision, 1);
    assert_eq!(frame.drawings()[1].submission.revision, 2);
    assert!(Arc::ptr_eq(&frame.drawings()[0].compiled, &pinned));
    assert_eq!(pending_peer.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(pending_bad.wait(Duration::ZERO).unwrap(), None);
    render(&frame, [0, 255, 0, 255]);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        pending_peer.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(pending_bad.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(
        (
            a.reconcile().unwrap().accepted_revision,
            a.reconcile().unwrap().presented_revision
        ),
        (2, 1)
    );
    text(&mut host, &frame, &peer, 2);
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err(),
        "old full display context cannot be reused"
    );
    drop(displayed);
    displayed = frame;
    for color in [0xff00ffff, 0x00ff00ff, 0xff00ffff] {
        let receipt = b.submit(scene(color)).unwrap();
        accepted(&b, receipt.revision());
        frame = host
            .prepare_retaining(1, 1., &displayed, &[selected])
            .unwrap();
        assert_eq!(frame.drawings()[0].submission.revision, 1);
        assert_eq!(frame.drawings()[1].submission.revision, receipt.revision());
        assert_eq!(receipt.wait(Duration::ZERO).unwrap(), None);
        render(&frame, color.to_be_bytes());
        assert!(host.presented(&frame).unwrap());
        assert_eq!(
            receipt.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(pending_bad.wait(Duration::ZERO).unwrap(), None);
        text(&mut host, &frame, &peer, receipt.revision());
        displayed = frame;
    }
    assert_eq!(a.reconcile().unwrap().presented_revision, 1);
    assert_eq!(b.reconcile().unwrap().presented_revision, 5);
    assert!(outcomes(&first).is_empty());
    assert!(outcomes(&peer).is_empty());
    drop(diagnosis);
    drop(displayed);
    a.close().unwrap();
    assert_eq!(
        pending_bad.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(b.reconcile().unwrap().presented_revision, 5);
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn retaining_committed_same_id_scene_advances_only_peer_original_receipts() {
    exercise(|_, _| ());
}

#[test]
fn retaining_rejects_unadmitted_foreign_and_changed_contexts_before_new_preparation() {
    let (presenter, mut host) = fixture();
    let first = connect(&presenter);
    let peer = connect(&presenter);
    let a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.submit(scene(0xff0000ff)).unwrap();
    b.submit(scene(0x0000ffff)).unwrap();
    let displayed = prepare(&mut host, 2);
    let selected = displayed.drawings()[0].window;
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    assert!(host.presented(&displayed).unwrap());
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected, selected])
            .is_err()
    );
    assert!(
        host.prepare_retaining(1, 2., &displayed, &[selected])
            .is_err()
    );
    assert!(
        host.prepare_retaining(2, 1., &displayed, &[selected])
            .is_err()
    );
    let (_other, mut foreign) = fixture();
    assert!(
        foreign
            .prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    b.set_bounds(Rect::new(65., 0., 48., 48.).unwrap()).unwrap();
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    drop(displayed);
    let displayed = prepare(&mut host, 2);
    assert!(host.presented(&displayed).unwrap());
    a.replace_track(scene(0x00ffffff)).unwrap();
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    drop(displayed);
    let displayed = prepare(&mut host, 2);
    assert!(host.presented(&displayed).unwrap());
    presenter.update_metrics(1, 17, 16, (8, 8));
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    drop(displayed);
    let displayed = prepare(&mut host, 2);
    assert!(host.presented(&displayed).unwrap());
    host.set_visible(1, false).unwrap();
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn retaining_cannot_rebind_a_resumed_owner_while_the_same_id_peer_stays_presented() {
    let (presenter, mut host) = fixture();
    let mut first = super::resume::leased(&presenter);
    let peer = connect(&presenter);
    let mut a = window(&first, 0.);
    let b = window(&peer, 64.);
    a.submit(scene(0xff0000ff)).unwrap();
    let original = b.submit(scene(0x0000ffff)).unwrap();
    let displayed = prepare(&mut host, 2);
    assert!(host.presented(&displayed).unwrap());
    let selected = displayed.drawings()[0].window;
    assert_eq!(
        original.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first
        .resume(super::resume::config(&presenter), &mut [&mut a])
        .unwrap();
    assert!(
        host.prepare_retaining(1, 1., &displayed, &[selected])
            .is_err()
    );
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    assert_eq!(
        original.wait(Duration::ZERO).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[cfg(feature = "vector-offscreen")]
#[test]
#[ignore = "requires native GPU; committed fallback and four fresh peer readbacks"]
fn native_retaining_committed_scene_reads_exact_pixels_before_original_peer_outcomes() {
    use vivid_sdk::vector_renderer::compositor::offscreen::Offscreen;
    let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
    exercise(|frame, expected| {
        assert!(gpu.render(frame.drawings(), 128, 128).unwrap());
        let until = Instant::now() + Duration::from_secs(10);
        let image = loop {
            if let Some(image) = gpu.poll().unwrap() {
                break image;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        };
        for (x, rgba) in [(5usize, [255, 0, 0, 255]), (69usize, expected)] {
            let offset = (5 * image.width as usize + x) * 4;
            assert_eq!(image.rgba[offset..offset + 4], rgba);
        }
    });
    assert!(!gpu.is_failed());
}
