//! Accepted overdraw must not silently reuse an older composed peer image.

#![cfg(all(
    feature = "presenter",
    feature = "vector-offscreen",
    feature = "testing"
))]

// Rust guideline compliant 2026-10-07
#[path = "native_vector_arena/adversarial.rs"]
mod adversarial;

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vivid_sdk::overlay::{Brush, Canvas, Color, Path, PresentationOutcome, Rect, WindowMode};
use vivid_sdk::presenter::{
    MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
};
use vivid_sdk::vector_renderer::compositor::offscreen::{Offscreen, Pixels};
use vivid_sdk::{
    OverlaySession, OverlaySubmission, OverlayWindow, OverlayWindowOptions, ProducerAuthentication,
    ProducerConfig,
};

const SIZE: u32 = 1024;
const WAIT: Duration = Duration::from_secs(5);
fn connect(presenter: &VirtualVivid) -> io::Result<OverlaySession> {
    let secret = presenter.issue_pane_capability(1)?;
    OverlaySession::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).map_err(io::Error::other)?,
        ..Default::default()
    })
}
fn scene(count: usize, rgba: u32, extent: f64) -> Canvas {
    let mut canvas = Canvas::new();
    for _ in 0..count {
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., extent, extent).unwrap()).unwrap(),
                Brush::Solid(Color(rgba)),
            )
            .unwrap();
    }
    canvas.validate().unwrap();
    assert!(canvas.encode().unwrap().len() + 12 <= 256 * 1024);
    canvas
}
fn prepare(host: &mut NativeVectorHost, count: usize, revision: u64) -> io::Result<VectorFrame> {
    let end = Instant::now() + WAIT;
    loop {
        let frame = host.prepare(1, 1.)?;
        if frame.drawings().len() == count
            && frame
                .drawings()
                .iter()
                .all(|d| d.submission.revision == revision)
        {
            return Ok(frame);
        }
        assert!(
            Instant::now() < end,
            "arena fixture frame stalled: {frame:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn readback(gpu: &mut Offscreen, frame: &VectorFrame) -> io::Result<Pixels> {
    let (width, height) = frame.dimensions();
    assert!(gpu.render(frame.drawings(), width, height)?);
    let end = Instant::now() + WAIT;
    loop {
        if let Some(image) = gpu.poll()? {
            return Ok(image);
        }
        assert!(Instant::now() < end, "arena fixture readback stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn controlled_readback(
    gpu: &mut Offscreen,
    frame: &VectorFrame,
    peer: &Arc<OverlayWindow>,
    revision: u64,
) -> io::Result<(Pixels, usize, usize, u128)> {
    let stop = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (worker_stop, worker_active, window) = (stop.clone(), active.clone(), peer.clone());
    let worker = std::thread::spawn(move || -> io::Result<(usize, usize, u128)> {
        let (mut count, mut overlapping, mut maximum) = (0usize, 0usize, 0u128);
        let mut ready = Some(ready_tx);
        while !worker_stop.load(Ordering::Acquire) {
            let during = worker_active.load(Ordering::Acquire);
            let begin = Instant::now();
            assert_eq!(window.reconcile()?.accepted_revision, revision);
            maximum = maximum.max(begin.elapsed().as_micros());
            count = count.checked_add(1).expect("bounded control count");
            if during || worker_active.load(Ordering::Acquire) {
                overlapping = overlapping
                    .checked_add(1)
                    .expect("bounded overlapping control count");
            }
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok((count, overlapping, maximum))
    });
    ready_rx
        .recv_timeout(WAIT)
        .expect("concurrent peer control did not start");
    active.store(true, Ordering::Release);
    let result = readback(gpu, frame);
    active.store(false, Ordering::Release);
    stop.store(true, Ordering::Release);
    let (count, overlapping, maximum) = worker.join().expect("peer control worker panicked")?;
    let image = result?;
    assert!(
        overlapping > 0,
        "no control request overlapped the GPU readback interval"
    );
    Ok((image, count, overlapping, maximum))
}
fn wrong_pixels(image: &Pixels, fill: [u8; 4], peer: [u8; 4]) -> usize {
    assert!(image.width > 0 && image.width <= SIZE && image.height > 0 && image.height <= SIZE);
    assert_eq!(
        image.rgba.len(),
        image.width as usize * image.height as usize * 4
    );
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(at, actual)| {
            let x = *at % image.width as usize;
            let y = *at / image.width as usize;
            let expected = if x >= 960 && y < 64 { peer } else { fill };
            **actual != expected
        })
        .count()
}
fn shown(receipts: [&OverlaySubmission; 2]) {
    for receipt in receipts {
        assert_eq!(
            receipt.wait(WAIT).unwrap(),
            Some(PresentationOutcome::Presented)
        );
    }
}
#[test]
#[ignore = "requires native GPU; checked arena retries, work refusal and cancellation"]
fn accepted_overdraw_never_returns_a_stale_peer_image_as_success() -> io::Result<()> {
    let presenter = Arc::new(VirtualVivid::start_configured_eventless(
        SocketListener::bind("tcp:127.0.0.1:0")?,
        PresenterConfig::terminal_with_overlay(MediaConfig::default()),
    )?);
    presenter.update_metrics(1, 128, 64, (8, 16));
    let mut host = NativeVectorHost::attach(presenter.clone(), "")?;
    let a = connect(&presenter)?;
    let b = connect(&presenter)?;
    let first = a.create_window(OverlayWindowOptions::new(
        Rect::new(0., 0., 1024., 1024.).unwrap(),
        WindowMode::Floating,
    ))?;
    let peer = Arc::new(b.create_window(OverlayWindowOptions::new(
        Rect::new(960., 0., 64., 64.).unwrap(),
        WindowMode::Floating,
    ))?);
    let mut gpu = pollster::block_on(Offscreen::new())?;
    let mut wrong_success = false;
    let mut previous: Option<Pixels> = None;
    let mut last_revision = 0;
    let mut last_peer = [0; 4];
    for (index, count) in [128, 512, 1024, 2048].into_iter().enumerate() {
        let revision = index as u64 + 1;
        last_revision = revision;
        let fill = if index % 2 == 0 {
            [255, 0, 0, 255]
        } else {
            [0, 255, 0, 255]
        };
        let peer_color = if index % 2 == 0 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 255, 255]
        };
        last_peer = peer_color;
        let canvas = scene(
            count,
            if index % 2 == 0 {
                0xff000080
            } else {
                0x00ff0080
            },
            1024.,
        );
        let bytes = canvas.encode().unwrap().len();
        let one = first.submit(canvas)?;
        let two = peer.submit(scene(1, u32::from_be_bytes(peer_color), 64.))?;
        let frame = prepare(&mut host, 2, revision)?;
        assert_eq!(
            frame.drawings()[0].submission.address,
            frame.drawings()[1].submission.address
        );
        assert_ne!(frame.drawings()[0].window, frame.drawings()[1].window);
        let start = Instant::now();
        let status = peer.reconcile()?;
        let control_us = start.elapsed().as_micros();
        assert_eq!(status.accepted_revision, revision);
        let (image, concurrent_controls, overlapping_controls, maximum_control_us) =
            controlled_readback(&mut gpu, &frame, &peer, revision)?;
        let mismatch = wrong_pixels(&image, fill, peer_color);
        let identical_to_previous = previous.as_ref().is_some_and(|old| old.rgba == image.rgba);
        eprintln!(
            "Arena pressure observation: commands={count}, scene_bytes={bytes}, target=1024, revision={revision}, mismatched_pixels={mismatch}, device_failed={}, peer_control_us={control_us}, identical_to_previous={identical_to_previous}, concurrent_controls={concurrent_controls}, overlapping_controls={overlapping_controls}, maximum_control_us={maximum_control_us}",
            gpu.is_failed()
        );
        assert_eq!(one.wait(Duration::ZERO)?, None);
        assert_eq!(two.wait(Duration::ZERO)?, None);
        if mismatch != 0 {
            wrong_success = true;
            // No display admission is granted to the corrupted image.
            drop(frame);
            break;
        }
        assert!(!gpu.render(frame.drawings(), image.width, image.height)?);
        assert!(host.presented(&frame)?);
        shown([&one, &two]);
        previous = Some(image);
        drop(frame);
    }
    assert!(
        gpu.metrics().allocation_retries > 0,
        "arena retries were not exercised"
    );
    let limited = host.prepare(1, 8.)?;
    assert_eq!(limited.dimensions(), (8192, 8192));
    let before = gpu.metrics().render_passes;
    let refused = controlled_readback(&mut gpu, &limited, &peer, last_revision)
        .expect_err("bounded GPU work unexpectedly completed");
    assert!(matches!(
            refused.get_ref().and_then(
                |error| error.downcast_ref::<vivid_sdk::vector_renderer::compositor::Error>()
            ),
            Some(vivid_sdk::vector_renderer::compositor::Error::WorkLimit)
        ));
    assert_eq!(gpu.metrics().render_passes - before, 64);
    assert!(!gpu.is_failed());
    drop(limited);
    assert!(gpu.render(&[], 1024, 1024)?);
    let blank = gpu
        .poll()?
        .expect("empty composition must replace refused output");
    assert_eq!((blank.width, blank.height), (1024, 1024));
    assert!(blank.rgba.iter().all(|byte| *byte == 0));
    let current = prepare(&mut host, 2, last_revision)?;
    let image = readback(&mut gpu, &current)?;
    assert_eq!(wrong_pixels(&image, [0, 255, 0, 255], last_peer), 0);
    assert!(host.presented(&current)?);
    drop(current);
    presenter.update_metrics(1, 1023, 1009, (1, 1));
    let odd = prepare(&mut host, 2, last_revision)?;
    assert_eq!(odd.dimensions(), (1023, 1009));
    let image = readback(&mut gpu, &odd)?;
    assert_eq!(wrong_pixels(&image, [0, 255, 0, 255], last_peer), 0);
    assert!(host.presented(&odd)?);
    drop(odd);
    eprintln!(
        "Checked work and geometry: bounded_attempts=64, odd_dimensions=1023x1009, allocation_retries={}",
        gpu.metrics().allocation_retries
    );
    // Source-isolated cleanup and a fresh composition must preserve the peer.
    first.close()?;
    a.close()?;
    let recovered = prepare(&mut host, 1, last_revision)?;
    let image = readback(&mut gpu, &recovered)?;
    assert_eq!(
        wrong_pixels(&image, [0; 4], last_peer),
        0,
        "peer recovery after saturated owner close"
    );
    if wrong_success {
        assert_eq!(
            peer.reconcile()?.presented_revision,
            last_revision - 1,
            "corrupted image advanced original peer presentation"
        );
    }
    assert!(host.presented(&recovered)?);
    assert_eq!(peer.reconcile()?.presented_revision, last_revision);
    eprintln!(
        "Arena pressure cleanup: peer_revision={last_revision}, device_failed={}, recovered_rgba_bytes={}",
        gpu.is_failed(),
        image.rgba.len()
    );
    for destroy in [false, true] {
        let mut cancelled = pollster::block_on(Offscreen::new())?;
        let (width, height) = recovered.dimensions();
        assert!(cancelled.render(recovered.drawings(), width, height)?);
        assert!(cancelled.poll()?.is_none());
        if destroy {
            cancelled.destroy_device_for_test();
        }
        drop(cancelled);
    }
    eprintln!(
        "Checked cancellation: pending validation and logical device loss retired without image admission"
    );
    peer.close()?;
    b.close()?;
    presenter.shutdown();
    assert!(
        !wrong_success,
        "renderer returned success with wrong pixels for an accepted overdraw scene"
    );
    Ok(())
}
