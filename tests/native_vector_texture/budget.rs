//! Shared dispatch credits must not grant output or presentation on exhaustion.

// Rust guideline compliant 2026-10-07
use super::*;
use std::num::NonZeroU64;
use vivid_sdk::vector_renderer::compositor::WorkBudget;

#[test]
#[ignore = "requires native GPU and sockets; cooperating checked-texture dispatch and export isolation"]
fn shared_texture_budget_cancels_without_output_blame_or_receipts() -> io::Result<()> {
    let (shared, device, queue, failed) = device()?;
    let pools = [TexturePool::default(), TexturePool::default()];
    let mut public = TextureRenderer::new(&shared, &pools[0])?;
    let mut private = TextureRenderer::new(&shared, &pools[1])?;
    let mut primary =
        vello::Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let mut f = Fixture::new(64)?;
    let receipts = [
        f.windows[0].submit(scene(1, 0xff0000ff, 64))?,
        f.windows[1].submit(scene(1, 0x0000ffff, 64))?,
    ];
    let original = f.prepare(1, 1.)?;
    let budget = WorkBudget::new(NonZeroU64::new(1).unwrap());
    public.start_budgeted(original.drawings(), &[], 64, 64, &budget)?;
    private.start_budgeted(original.drawings(), &[], 64, 64, &budget.clone())?;
    assert!(public.poll()?.is_none());
    let error = private
        .poll()
        .expect_err("one credit cannot admit both native jobs");
    assert!(matches!(
        error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
        Some(Error::Cancelled)
    ));
    let completed = finish(&mut public)?;
    assert_eq!(budget.spent(), 1);
    assert_eq!(budget.remaining(), 0);
    assert_eq!(
        public.metrics().render_passes + private.metrics().render_passes,
        1
    );
    assert!(!public.is_failed() && !private.is_failed());
    idle(&mut private)?;
    assert_eq!(pools[1].reserved(), 0);
    for receipt in &receipts {
        assert_eq!(receipt.wait(std::time::Duration::ZERO)?, None);
    }
    let old = completed.texture.register(&mut primary);
    let pixels = sample(&device, &queue, &mut primary, old.image(), 64, 64, [0; 4])?;
    assert_colors(&pixels, 64, [255, 0, 0, 255], [0, 0, 255, 255], 32, 64, 0);
    assert!(f.host.presented(&original)?);
    for receipt in &receipts {
        assert_eq!(receipt.wait(WAIT)?, Some(PresentationOutcome::Presented));
    }
    // A checked cache hit performs no new tile dispatch, even after exhaustion.
    let cache_job = public.start_budgeted(original.drawings(), &[], 64, 64, &budget)?;
    let cache = finish(&mut public)?;
    assert_eq!(cache.job, cache_job);
    assert_ne!(cache.job, completed.job);
    assert!(!cache.redrawn && cache.texture.same_allocation(&completed.texture));
    assert_eq!(budget.spent(), 1);
    drop(cache);

    let pending = f.windows[0].submit(scene(1, 0x00ff00ff, 64))?;
    let end = Instant::now() + WAIT;
    let current = loop {
        let frame = f.host.prepare(1, 1.)?;
        if frame.drawings().len() == 2
            && frame.drawings()[0].submission.revision == 2
            && frame.drawings()[1].submission.revision == 1
        {
            break frame;
        }
        assert!(
            Instant::now() < end,
            "mixed-revision texture candidate stalled"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    public.start_budgeted(current.drawings(), &[], 64, 64, &budget)?;
    let error = finish(&mut public).expect_err("changed pixels require a dispatch credit");
    assert!(matches!(
        error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
        Some(Error::Cancelled)
    ));
    assert_eq!(pending.wait(std::time::Duration::ZERO)?, None);
    assert_eq!(f.windows[0].reconcile()?.presented_revision, 1);
    assert_eq!(f.windows[1].reconcile()?.presented_revision, 1);
    assert_eq!(
        sample(&device, &queue, &mut primary, old.image(), 64, 64, [0; 4])?,
        pixels
    );
    idle(&mut public)?;
    // Ordinary work has its original independent per-job limit. Cancellation
    // neither replenishes the exhausted episode nor poisons either renderer.
    public.start(current.drawings(), &[], 64, 64)?;
    let recovered = finish(&mut public)?;
    let fresh = recovered.texture.register(&mut primary);
    assert_colors(
        &sample(&device, &queue, &mut primary, fresh.image(), 64, 64, [0; 4])?,
        64,
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        32,
        64,
        0,
    );
    assert_eq!(pending.wait(std::time::Duration::ZERO)?, None);
    assert!(f.host.presented(&current)?);
    assert_eq!(pending.wait(WAIT)?, Some(PresentationOutcome::Presented));
    assert_eq!(budget.spent(), 1);
    assert!(!failed.load(Ordering::Acquire));
    old.unregister(&mut primary);
    fresh.unregister(&mut primary);
    drop((completed, recovered));
    let end = Instant::now() + WAIT;
    while pools.iter().any(|pool| pool.reserved() != 0) {
        public.poll()?;
        private.poll()?;
        assert!(
            Instant::now() < end,
            "cooperating texture exports did not retire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    eprintln!(
        "Shared texture budget: one actual tile across two same-device renderers; cancellation publishes no output or receipt, immutable red retained, exhausted cache reuse, fresh green admission, peer preserved and both pools empty"
    );
    Ok(())
}
