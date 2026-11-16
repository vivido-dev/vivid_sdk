//! Native shared-device cancellation must preserve another renderer's pixels.

#![cfg(feature = "vector-offscreen")]

// Rust guideline compliant 2026-10-07
use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::{Color, Fill},
};
use vivid_vello_backend::{AaConfig, RenderParams, Renderer};

fn target(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vivid.shared.cancellation"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
fn scene(color: [u8; 4]) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(color[0], color[1], color[2], color[3]),
        None,
        &Rect::new(0., 0., 64., 64.),
    );
    scene
}
fn pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> io::Result<Vec<u8>> {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vivid.shared.peer.readback"),
        size: 64 * 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.try_send(result);
        });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(io::Error::other)?;
        match rx.try_recv() {
            Ok(result) => {
                result.map_err(io::Error::other)?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let bytes = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(io::Error::other)?
        .to_vec();
    buffer.unmap();
    Ok(bytes)
}

#[test]
#[ignore = "requires native GPU; shared-device pending validation cancellation"]
fn dropping_pending_validation_does_not_panic_or_break_a_shared_device_peer() -> io::Result<()> {
    shared_cancellation(false)
}

#[test]
#[ignore = "requires native GPU; shared-device pending debug download cancellation"]
fn dropping_pending_debug_download_does_not_panic_or_break_a_shared_device_peer() -> io::Result<()>
{
    shared_cancellation(true)
}

#[expect(
    deprecated,
    reason = "Vello 0.11 exposes allocation status only through its diagnostic API"
)]
fn shared_cancellation(debug_download: bool) -> io::Result<()> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .map_err(io::Error::other)?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
        .map_err(io::Error::other)?;
    let failed = Arc::new(AtomicBool::new(false));
    let errors = failed.clone();
    device.on_uncaptured_error(Arc::new(move |_| {
        errors.store(true, Ordering::Release);
    }));
    let loss = failed.clone();
    device.set_device_lost_callback(move |_, _| {
        loss.store(true, Ordering::Release);
    });
    let mut peer = vello::Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let peer_texture = target(&device);
    let peer_view = peer_texture.create_view(&Default::default());
    let params = RenderParams {
        base_color: Color::TRANSPARENT,
        width: 64,
        height: 64,
        antialiasing_method: AaConfig::Msaa8,
    };
    let peer_params = vello::RenderParams {
        base_color: Color::TRANSPARENT,
        width: 64,
        height: 64,
        antialiasing_method: vello::AaConfig::Msaa8,
    };
    for wave in 0..8_u8 {
        let color = [wave * 16, 40, 90, 255];
        peer.render_to_texture(&device, &queue, &scene(color), &peer_view, &peer_params)
            .map_err(io::Error::other)?;
        assert!(
            pixels(&device, &queue, &peer_texture)?
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == color)
        );
        let mut retiring = Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
        let texture = target(&device);
        let view = texture.create_view(&Default::default());
        let pending_scene = scene([255, 0, 0, 128]);
        let mut pending = Box::pin(retiring.render_to_texture_async(
            &device,
            &queue,
            &pending_scene,
            &view,
            &params,
            vivid_vello_backend::low_level::DebugLayers::none(),
        ));
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        if debug_download {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(5)),
                })
                .map_err(io::Error::other)?;
            assert!(matches!(
                pending
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        drop(pending);
        drop(retiring);
        // Polling for the independent peer also services the cancelled callback.
        let image = pixels(&device, &queue, &peer_texture)?;
        assert!(image.as_chunks::<4>().0.iter().all(|pixel| *pixel == color));
        let fresh = [color[0] + 8, 90, 40, 255];
        peer.render_to_texture(&device, &queue, &scene(fresh), &peer_view, &peer_params)
            .map_err(io::Error::other)?;
        assert!(
            pixels(&device, &queue, &peer_texture)?
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == fresh)
        );
        assert!(!failed.load(Ordering::Acquire));
        eprintln!(
            "Shared-device cancellation: debug_download={debug_download}, wave={wave}, peer_rgba_bytes={}, fresh_upstream_peer=true, device_failed=false",
            image.len()
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires native GPU; pending mapped validation after logical device loss"]
#[expect(
    deprecated,
    reason = "Vello 0.11 exposes allocation status only through its diagnostic API"
)]
fn host_retires_mapped_validation_after_device_loss_without_submitting_fine() -> io::Result<()> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .map_err(io::Error::other)?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
        .map_err(io::Error::other)?;
    let failed = Arc::new(AtomicBool::new(false));
    let errors = failed.clone();
    device.on_uncaptured_error(Arc::new(move |_| {
        errors.store(true, Ordering::Release);
    }));
    let loss = failed.clone();
    device.set_device_lost_callback(move |_, _| {
        loss.store(true, Ordering::Release);
    });
    let mut renderer = Renderer::new(&device, Default::default()).map_err(io::Error::other)?;
    let texture = target(&device);
    let view = texture.create_view(&Default::default());
    let scene = scene([255, 0, 0, 128]);
    let params = RenderParams {
        base_color: Color::TRANSPARENT,
        width: 64,
        height: 64,
        antialiasing_method: AaConfig::Msaa8,
    };
    let mut pending = Box::pin(renderer.render_to_texture_async(
        &device,
        &queue,
        &scene,
        &view,
        &params,
        vivid_vello_backend::low_level::DebugLayers::none(),
    ));
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        })
        .map_err(io::Error::other)?;
    device.destroy();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !failed.load(Ordering::Acquire) {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(io::Error::other)?;
        assert!(
            Instant::now() < deadline,
            "logical device loss was not observed"
        );
    }
    // The host must observe its device health before polling Vello again.
    // Continuing a successful map into fine submission on a destroyed device
    // is invalid WGPU use, not a promised Vello error-return path.
    drop(pending);
    drop(renderer);
    device
        .poll(wgpu::PollType::Poll)
        .map_err(io::Error::other)?;
    eprintln!(
        "Mapped validation device loss: host retired pending work without fine submission or image admission"
    );
    Ok(())
}
