//! Shared admission and failure observation across independent native targets.

// Rust guideline compliant 2026-10-07
use super::*;
use std::num::NonZeroU64;
use vivid_protocol::{
    identity::{PresenterInstanceId, SessionIdentity},
    overlay::wire::{Submission, WindowAddress},
};

fn drawing(color: vello::peniko::Color) -> Drawing {
    let mut scene = vello::Scene::new();
    scene.fill(
        vello::peniko::Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        color,
        None,
        &vello::kurbo::Rect::new(0., 0., 64., 64.),
    );
    Drawing {
        window: SessionIdentity::new(PresenterInstanceId([1; 16]), 1)
            .unwrap()
            .context(1)
            .unwrap()
            .surface(1)
            .unwrap(),
        window_revision: 1,
        submission: Submission {
            address: WindowAddress {
                context_id: 1,
                surface_id: 1,
                generation: 1,
            },
            track_id: 1,
            channel_generation: 1,
            epoch: 1,
            revision: 1,
        },
        bounds: vivid_protocol::vector::Rect::new(0., 0., 64., 64.).unwrap(),
        scale: 1.,
        compiled: Arc::new(CompiledScene {
            scene,
            retained: Vec::new(),
            hits: Vec::new(),
            extra_bytes: 0,
        }),
    }
}
fn read(gpu: &mut Offscreen) -> Pixels {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(image) = gpu.poll().unwrap() {
            return image;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires native GPU; shared dispatch budget and forked target/device lifetime"]
fn native_fork_budget_cancels_without_blame_or_cross_target_pixels_and_shares_loss() {
    use vello::peniko::Color;
    let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
    let mut private = gpu.fork().unwrap();
    let budget = super::super::WorkBudget::new(NonZeroU64::new(1).unwrap());
    let red = [drawing(Color::from_rgba8(255, 0, 0, 255))];
    assert!(gpu.render_budgeted(&red, &[], 64, 64, &budget).unwrap());
    assert!(private.render_budgeted(&red, &[], 64, 64, &budget).unwrap());
    assert!(gpu.poll().unwrap().is_none());
    let error = private.poll().unwrap_err();
    assert!(matches!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<Error>()),
        Some(Error::Cancelled)
    ));
    let image = read(&mut gpu);
    assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(budget.spent(), 1);
    assert_eq!(budget.remaining(), 0);
    assert_eq!(
        gpu.metrics().render_passes + private.metrics().render_passes,
        1
    );
    assert!(!gpu.is_failed() && !private.is_failed());
    assert!(
        private
            .render(&[drawing(Color::from_rgba8(0, 255, 0, 255))], 64, 64)
            .unwrap()
    );
    let peer = read(&mut private);
    assert_eq!(&peer.rgba[..4], &[0, 255, 0, 255]);
    assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
    #[cfg(feature = "testing")]
    {
        let mut independent = pollster::block_on(Offscreen::new()).unwrap();
        private.destroy_device_for_test();
        let until = Instant::now() + Duration::from_secs(10);
        while !gpu.is_failed() || !private.is_failed() {
            let _ = gpu.poll();
            let _ = private.poll();
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(gpu.render(&red, 64, 64).is_err());
        assert!(private.render(&red, 64, 64).is_err());
        assert!(independent.render(&red, 64, 64).unwrap());
        assert_eq!(&read(&mut independent).rgba[..4], &[255, 0, 0, 255]);
        assert!(!independent.is_failed());
    }
}
