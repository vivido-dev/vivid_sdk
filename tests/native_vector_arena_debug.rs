//! Read Vello's native allocation counters without changing the ordinary renderer.

#![cfg(all(feature = "presenter", feature = "vector-offscreen"))]

// Rust guideline compliant 2026-10-07
use std::{
    future::Future,
    io,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};
use vello::{AaConfig, RenderParams, Renderer, Scene};

#[test]
#[ignore = "requires native GPU and explicit vello/debug_layers diagnostic feature"]
#[expect(
    deprecated,
    reason = "Vello 0.11 exposes actual allocation counters only through this diagnostic API"
)]
fn native_allocation_counters_identify_accepted_overdraw_exhaustion() -> io::Result<()> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(io::Error::other)?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(io::Error::other)?;
    let mut renderer = Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vivid.arena.diagnostic"),
        size: wgpu::Extent3d {
            width: 1024,
            height: 1024,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    for count in [128, 512, 1024, 2048] {
        let mut scene = Scene::new();
        for _ in 0..count {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(255, 0, 0, 128),
                None,
                &Rect::new(0., 0., 1024., 1024.),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 255),
            None,
            &Rect::new(960., 0., 1024., 64.),
        );
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: 1024,
            height: 1024,
            antialiasing_method: AaConfig::Msaa8,
        };
        let mut future = Box::pin(renderer.render_to_texture_async(
            &device,
            &queue,
            &scene,
            &view,
            &params,
            vello::low_level::DebugLayers::none(),
        ));
        let end = Instant::now() + Duration::from_secs(5);
        let bump = loop {
            match future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Ready(result) => {
                    break result.map_err(io::Error::other)?.expect(
                        "enable explicit vello/debug_layers for native allocation counters",
                    );
                }
                Poll::Pending => {
                    device
                        .poll(wgpu::PollType::Poll)
                        .map_err(io::Error::other)?;
                }
            }
            assert!(
                Instant::now() < end,
                "diagnostic allocation readback stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        eprintln!(
            "Native Vello allocation counters: commands={count}, failed={}, binning={}, ptcl={}, tile={}, seg_counts={}, segments={}, blend={}, lines={}",
            bump.failed,
            bump.binning,
            bump.ptcl,
            bump.tile,
            bump.seg_counts,
            bump.segments,
            bump.blend,
            bump.lines
        );
        if count == 128 {
            assert_eq!(bump.failed, 0);
        } else {
            assert_ne!(
                bump.failed, 0,
                "the fixed-size native allocation limit was not reached"
            );
        }
    }
    Ok(())
}
