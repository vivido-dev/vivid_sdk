//! Checked raw GPU output must retain exact scenes without blocking a primary peer.

#![cfg(all(
    feature = "presenter",
    feature = "vector-offscreen",
    feature = "testing"
))]

// Rust guideline compliant 2026-10-07
#[path = "native_vector_texture/support.rs"]
mod support;
use std::{
    io,
    sync::{atomic::Ordering, mpsc},
    time::Instant,
};
use support::*;
use vivid_sdk::overlay::PresentationOutcome;
use vivid_sdk::vector_renderer::compositor::{
    Error,
    texture::{JobId, SharedDevice, Texture, TextureImport, TexturePool, TextureRenderer},
};

#[test]
#[ignore = "requires native GPU; immutable textures, upstream sampling and retained import quotas"]
fn immutable_exports_and_import_clones_preserve_exact_original_receipts() -> io::Result<()> {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<SharedDevice>();
    send_sync::<TexturePool>();
    send_sync::<Texture>();
    send_sync::<TextureImport>();
    send_sync::<JobId>();
    let (shared, device, queue, failed) = device()?;
    let pool = TexturePool::default();
    let mut gpu = TextureRenderer::new(&shared, &pool)?;
    assert_eq!(
        TextureRenderer::new(&shared, &pool).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut primary =
        vello::Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let mut f = Fixture::new(64)?;
    let one = f.windows[0].submit(scene(1, 0x4080c080, 64))?;
    let two = f.windows[1].submit(scene(1, 0x0000ffff, 64))?;
    let frame = f.prepare(1, 1.)?;
    let job = gpu.start(frame.drawings(), frame.rasters(), 64, 64)?;
    assert!(gpu.poll()?.is_none());
    let first = finish(&mut gpu)?;
    assert_eq!(first.job, job);
    assert!(first.redrawn);
    assert_eq!(f.windows[0].reconcile()?.presented_revision, 0);
    let old = first.texture.register(&mut primary);
    let old_clone = old.clone();
    let before = sample(
        &device,
        &queue,
        &mut primary,
        old.image(),
        64,
        64,
        [32, 64, 96, 255],
    )?;
    assert_colors(&before, 64, [48, 96, 144, 255], [0, 0, 255, 255], 32, 64, 1);
    assert!(f.host.presented(&frame)?);
    for receipt in [&one, &two] {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    let hit = gpu.start(frame.drawings(), frame.rasters(), 64, 64)?;
    assert_ne!(hit, job);
    let hit = finish(&mut gpu)?;
    assert!(!hit.redrawn);
    assert!(hit.texture.same_allocation(&first.texture));
    drop(hit);
    let a = f.windows[0].submit(scene(1, 0xff0000ff, 64))?;
    let b = f.windows[1].submit(scene(1, 0x00ff00ff, 64))?;
    let second_frame = f.prepare(2, 1.)?;
    gpu.start(second_frame.drawings(), second_frame.rasters(), 64, 64)?;
    let second = finish(&mut gpu)?;
    assert!(!first.texture.same_allocation(&second.texture));
    assert_eq!(
        sample(
            &device,
            &queue,
            &mut primary,
            old.image(),
            64,
            64,
            [32, 64, 96, 255]
        )?,
        before
    );
    let current = second.texture.register(&mut primary);
    assert_colors(
        &sample(
            &device,
            &queue,
            &mut primary,
            current.image(),
            64,
            64,
            [0; 4],
        )?,
        64,
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        32,
        64,
        0,
    );
    assert_eq!(pool.reserved(), 2);
    assert_eq!(
        gpu.start(second_frame.drawings(), &[], 64, 64)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(f.windows[0].reconcile()?.presented_revision, 1);
    assert!(f.host.presented(&second_frame)?);
    assert!(!f.host.presented(&frame)?);
    for receipt in [&a, &b] {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    drop(first);
    drop(old);
    assert_eq!(pool.reserved(), 2);
    assert!(old_clone.unregister(&mut primary));
    assert!(old_clone.image().is_none());
    assert!(!old_clone.unregister(&mut primary));
    let end = Instant::now() + WAIT;
    while pool.reserved() != 1 {
        gpu.poll()?;
        assert!(
            Instant::now() < end,
            "unregistered atlas metadata retained output credit"
        );
    }
    // Keep the invalidated import clone and Vello's resident metadata alive.
    // Neither may retain the retired source texture after explicit unregister.
    gpu.start(&[], &[], 64, 64)?;
    let blank = finish(&mut gpu)?;
    let clear = blank.texture.register(&mut primary);
    assert!(
        sample(&device, &queue, &mut primary, clear.image(), 64, 64, [0; 4])?
            .iter()
            .all(|b| *b == 0)
    );
    clear.unregister(&mut primary);
    drop(blank);
    let end = Instant::now() + WAIT;
    while pool.reserved() != 1 {
        gpu.poll()?;
        assert!(Instant::now() < end);
    }
    f.windows[0].close()?;
    assert!(!f.host.presented(&second_frame)?);
    drop(frame);
    drop(second_frame);
    let recovered = f.prepare_count(1, 2, 1.)?;
    gpu.start(recovered.drawings(), &[], 64, 64)?;
    let surviving = finish(&mut gpu)?;
    let peer = surviving.texture.register(&mut primary);
    assert_colors(
        &sample(&device, &queue, &mut primary, peer.image(), 64, 64, [0; 4])?,
        64,
        [0; 4],
        [0, 255, 0, 255],
        32,
        64,
        0,
    );
    assert_colors(
        &sample(
            &device,
            &queue,
            &mut primary,
            current.image(),
            64,
            64,
            [0; 4],
        )?,
        64,
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        32,
        64,
        0,
    );
    assert!(f.host.presented(&recovered)?);
    assert_eq!(f.windows[1].reconcile()?.presented_revision, 2);
    peer.unregister(&mut primary);
    current.unregister(&mut primary);
    drop(surviving);
    drop(second);
    let end = Instant::now() + WAIT;
    while pool.reserved() != 0 {
        gpu.poll()?;
        assert!(Instant::now() < end, "import credit did not retire");
    }
    let passes = gpu.metrics().render_passes;
    gpu.start(recovered.drawings(), &[], 64, 64)?;
    let copied = finish(&mut gpu)?;
    assert!(!copied.redrawn);
    assert_eq!(gpu.metrics().render_passes, passes);
    let copied_image = copied.texture.register(&mut primary);
    assert_colors(
        &sample(
            &device,
            &queue,
            &mut primary,
            copied_image.image(),
            64,
            64,
            [0; 4],
        )?,
        64,
        [0; 4],
        [0, 255, 0, 255],
        32,
        64,
        0,
    );
    copied_image.unregister(&mut primary);
    drop(copied);
    let end = Instant::now() + WAIT;
    while pool.reserved() != 0 {
        gpu.poll()?;
        assert!(Instant::now() < end);
    }
    assert!(!failed.load(Ordering::Acquire));
    eprintln!(
        "Immutable texture qualification: exact same-ID owners, straight alpha, retained full image, fresh job/cache reuse, two-export quota, exact-owner close/stale admission and explicit atlas import retirement passed"
    );
    Ok(())
}

#[test]
#[ignore = "requires native GPU; hostile overdraw, work refusal and empty raw output"]
fn raw_overdraw_refuses_bounded_work_without_publishing_old_pixels() -> io::Result<()> {
    let (shared, device, queue, failed) = device()?;
    let pool = TexturePool::default();
    let mut gpu = TextureRenderer::new(&shared, &pool)?;
    let mut primary =
        vello::Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let mut f = Fixture::new(1024)?;
    let one = f.windows[0].submit(scene(2048, 0xff000080, 1024))?;
    let two = f.windows[1].submit(scene(1, 0x0000ffff, 64))?;
    let frame = f.prepare(1, 1.)?;
    gpu.start(frame.drawings(), &[], 1024, 1024)?;
    let output = finish(&mut gpu)?;
    let old = output.texture.register(&mut primary);
    let before = sample(
        &device,
        &queue,
        &mut primary,
        old.image(),
        1024,
        1024,
        [0; 4],
    )?;
    assert_colors(
        &before,
        1024,
        [255, 0, 0, 255],
        [0, 0, 255, 255],
        960,
        64,
        0,
    );
    assert!(gpu.metrics().allocation_retries > 0);
    assert_eq!(f.windows[1].reconcile()?.presented_revision, 0);
    let limited = f.prepare(1, 8.)?;
    let passes = gpu.metrics().render_passes;
    gpu.start(limited.drawings(), &[], 8192, 8192)?;
    let error =
        finish(&mut gpu).expect_err("hostile overdraw exceeded checked budget without refusal");
    assert!(matches!(
        error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
        Some(Error::WorkLimit)
    ));
    assert_eq!(gpu.metrics().render_passes - passes, 64);
    assert!(!gpu.is_failed());
    idle(&mut gpu)?;
    assert_eq!(
        sample(
            &device,
            &queue,
            &mut primary,
            old.image(),
            1024,
            1024,
            [0; 4]
        )?,
        before
    );
    assert_eq!(f.windows[0].reconcile()?.presented_revision, 0);
    gpu.start(&[], &[], 1024, 1024)?;
    let blank = finish(&mut gpu)?;
    let clear = blank.texture.register(&mut primary);
    assert!(
        sample(
            &device,
            &queue,
            &mut primary,
            clear.image(),
            1024,
            1024,
            [0; 4]
        )?
        .iter()
        .all(|b| *b == 0)
    );
    clear.unregister(&mut primary);
    drop(blank);
    idle(&mut gpu)?;
    // Release the old export/import before requesting another bounded export.
    old.unregister(&mut primary);
    drop(output);
    let end = Instant::now() + WAIT;
    while pool.reserved() != 0 {
        gpu.poll()?;
        assert!(Instant::now() < end);
    }
    drop(limited);
    drop(frame);
    f.presenter.update_metrics(1, 1023, 1009, (1, 1));
    let odd = f.prepare(1, 1.)?;
    gpu.start(odd.drawings(), &[], 1023, 1009)?;
    let fresh = finish(&mut gpu)?;
    let image = fresh.texture.register(&mut primary);
    assert_colors(
        &sample(
            &device,
            &queue,
            &mut primary,
            image.image(),
            1023,
            1009,
            [0; 4],
        )?,
        1023,
        [255, 0, 0, 255],
        [0, 0, 255, 255],
        960,
        64,
        0,
    );
    assert!(f.host.presented(&odd)?);
    for receipt in [&one, &two] {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    image.unregister(&mut primary);
    assert!(!failed.load(Ordering::Acquire));
    eprintln!(
        "Raw arena qualification: 2048 accepted fills, 64-attempt refusal, immutable old pixels, transparent recovery, odd 1023x1009 output and original receipts passed"
    );
    Ok(())
}

#[test]
#[ignore = "requires native GPU; worker cancellation/deadline preserve shared primary renderer"]
fn cancellation_deadline_and_worker_retirement_preserve_shared_device() -> io::Result<()> {
    let (shared, device, queue, failed) = device()?;
    let pool = TexturePool::default();
    let mut primary =
        vello::Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let mut f = Fixture::new(64)?;
    let _one = f.windows[0].submit(scene(1, 0xff0000ff, 64))?;
    let _two = f.windows[1].submit(scene(1, 0x0000ffff, 64))?;
    let frame = f.prepare(1, 1.)?;
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let worker_shared = shared.clone();
    let worker_pool = pool.clone();
    let drawings = frame.drawings().to_vec();
    let worker = std::thread::spawn(move || -> io::Result<()> {
        let mut gpu = TextureRenderer::new(&worker_shared, &worker_pool)?;
        gpu.start(&drawings, &[], 64, 64)?;
        assert!(gpu.poll()?.is_none());
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(WAIT).unwrap();
        gpu.cancel();
        assert_eq!(
            gpu.start(&drawings, &[], 64, 64).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        idle(&mut gpu)?;
        Ok(())
    });
    ready_rx.recv_timeout(WAIT).unwrap();
    for wave in 0..8_u8 {
        let color = [wave * 16, 30, 90, 255];
        assert!(
            sample(&device, &queue, &mut primary, None, 64, 64, color)?
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == color)
        );
        assert_eq!(f.windows[1].reconcile()?.accepted_revision, 1);
    }
    release_tx.send(()).unwrap();
    worker.join().expect("texture worker panicked")?;
    let construct = || -> io::Result<TextureRenderer> {
        let end = Instant::now() + WAIT;
        loop {
            device
                .poll(wgpu::PollType::Poll)
                .map_err(io::Error::other)?;
            match TextureRenderer::new(&shared, &pool) {
                Ok(g) => break Ok(g),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < end, "worker retirement stalled")
                }
                Err(e) => break Err(e),
            }
        }
    };
    let mut gpu = construct()?;
    for later in [false, true] {
        gpu.start(frame.drawings(), &[], 64, 64)?;
        assert!(gpu.poll()?.is_none());
        if later {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(WAIT),
                })
                .map_err(io::Error::other)?;
            assert!(gpu.poll()?.is_none());
        }
        gpu.cancel();
        idle(&mut gpu)?;
        assert!(!gpu.is_failed());
    }
    gpu.start(frame.drawings(), &[], 64, 64)?;
    assert!(gpu.poll()?.is_none());
    gpu.expire_pending_for_test();
    assert_eq!(gpu.poll().unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(gpu.is_failed());
    assert!(!failed.load(Ordering::Acquire));
    drop(gpu);
    let mut gpu = construct()?;
    assert!(
        sample(
            &device,
            &queue,
            &mut primary,
            None,
            64,
            64,
            [40, 80, 120, 255]
        )?
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| *p == [40, 80, 120, 255])
    );
    gpu.start(frame.drawings(), &[], 64, 64)?;
    assert!(gpu.poll()?.is_none());
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(WAIT),
        })
        .map_err(io::Error::other)?;
    device.destroy();
    let end = Instant::now() + WAIT;
    while !failed.load(Ordering::Acquire) {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(io::Error::other)?;
        assert!(Instant::now() < end);
    }
    assert!(gpu.poll().is_err());
    assert!(gpu.is_failed());
    assert!(TextureRenderer::new(&shared, &TexturePool::default()).is_err());
    eprintln!(
        "Shared texture lifecycle: independent primary/control progress during a held worker, both map cancellations, local timeout without device failure, pool-preserving replacement and host loss fencing passed"
    );
    Ok(())
}

#[path = "native_vector_texture/budget.rs"]
mod budget;
