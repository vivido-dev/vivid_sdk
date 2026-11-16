//! Native socket owners and independent upstream-renderer sampling.

// Rust guideline compliant 2026-10-07
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vivid_sdk::overlay::{Brush, Canvas, Color, Path, Rect, WindowMode};
use vivid_sdk::presenter::{
    MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
};
use vivid_sdk::vector_renderer::compositor::texture::{Completed, SharedDevice, TextureRenderer};
use vivid_sdk::{
    OverlaySession, OverlayWindow, OverlayWindowOptions, ProducerAuthentication, ProducerConfig,
};

pub const WAIT: Duration = Duration::from_secs(10);
pub struct Fixture {
    pub presenter: Arc<VirtualVivid>,
    pub host: NativeVectorHost,
    pub windows: [OverlayWindow; 2],
    _sessions: [OverlaySession; 2],
}
pub fn scene(count: usize, rgba: u32, size: u32) -> Canvas {
    let mut canvas = Canvas::new();
    for _ in 0..count {
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., f64::from(size), f64::from(size)).unwrap())
                    .unwrap(),
                Brush::Solid(Color(rgba)),
            )
            .unwrap();
    }
    canvas.validate().unwrap();
    canvas
}
impl Fixture {
    pub fn new(size: u32) -> io::Result<Self> {
        let presenter = Arc::new(VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0")?,
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )?);
        presenter.update_metrics(
            1,
            size.try_into().unwrap(),
            size.try_into().unwrap(),
            (1, 1),
        );
        let host = NativeVectorHost::attach(presenter.clone(), "")?;
        let connect = || -> io::Result<OverlaySession> {
            let secret = presenter.issue_pane_capability(1)?;
            OverlaySession::connect(ProducerConfig {
                endpoint_control: Some(presenter.endpoint()),
                endpoint_interactive: Some(presenter.endpoint()),
                endpoint_realtime: Some(presenter.endpoint()),
                endpoint_bulk: Some(presenter.endpoint()),
                authentication: ProducerAuthentication::root_hex(&secret)
                    .map_err(io::Error::other)?,
                ..Default::default()
            })
        };
        let sessions = [connect()?, connect()?];
        let peer_width = (size / 2).min(64);
        let windows = [
            sessions[0].create_window(OverlayWindowOptions::new(
                Rect::new(0., 0., f64::from(size), f64::from(size)).unwrap(),
                WindowMode::Floating,
            ))?,
            sessions[1].create_window(OverlayWindowOptions::new(
                Rect::new(
                    f64::from(size - peer_width),
                    0.,
                    f64::from(peer_width),
                    f64::from(size.min(64)),
                )
                .unwrap(),
                WindowMode::Floating,
            ))?,
        ];
        Ok(Self {
            presenter,
            host,
            windows,
            _sessions: sessions,
        })
    }
    pub fn prepare(&mut self, revision: u64, scale: f64) -> io::Result<VectorFrame> {
        self.prepare_count(2, revision, scale)
    }
    pub fn prepare_count(
        &mut self,
        count: usize,
        revision: u64,
        scale: f64,
    ) -> io::Result<VectorFrame> {
        let end = Instant::now() + WAIT;
        loop {
            let frame = self.host.prepare(1, scale)?;
            if frame.drawings().len() == count
                && frame
                    .drawings()
                    .iter()
                    .all(|d| d.submission.revision == revision)
            {
                if count == 2 {
                    assert_eq!(
                        frame.drawings()[0].submission.address,
                        frame.drawings()[1].submission.address
                    );
                    assert_ne!(frame.drawings()[0].window, frame.drawings()[1].window);
                }
                return Ok(frame);
            }
            assert!(Instant::now() < end, "texture fixture scene stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

pub fn device() -> io::Result<(SharedDevice, wgpu::Device, wgpu::Queue, Arc<AtomicBool>)> {
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
    let observer = failed.clone();
    let shared = SharedDevice::new(&device, &queue, move || observer.load(Ordering::Acquire));
    Ok((shared, device, queue, failed))
}
pub fn finish(gpu: &mut TextureRenderer) -> io::Result<Completed> {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(output) = gpu.poll()? {
            return Ok(output);
        }
        assert!(Instant::now() < end, "texture completion stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}
pub fn idle(gpu: &mut TextureRenderer) -> io::Result<()> {
    let end = Instant::now() + WAIT;
    while gpu.is_busy() {
        assert!(gpu.poll()?.is_none());
        assert!(Instant::now() < end, "texture cancellation stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
pub fn sample(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut vello::Renderer,
    image: Option<&vello::peniko::ImageData>,
    width: u32,
    height: u32,
    bg: [u8; 4],
) -> io::Result<Vec<u8>> {
    let mut scene = vello::Scene::new();
    if let Some(image) = image {
        scene.draw_image(image, vello::kurbo::Affine::IDENTITY);
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vivid.texture.primary"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    renderer
        .render_to_texture(
            device,
            queue,
            &scene,
            &texture.create_view(&Default::default()),
            &vello::RenderParams {
                width,
                height,
                base_color: vello::peniko::Color::from_rgba8(bg[0], bg[1], bg[2], bg[3]),
                antialiasing_method: vello::AaConfig::Msaa8,
            },
        )
        .map_err(io::Error::other)?;
    let row = width.checked_mul(4).unwrap();
    let stride = row.div_ceil(256).checked_mul(256).unwrap();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vivid.texture.sample"),
        size: u64::from(stride) * u64::from(height),
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
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::sync_channel(1);
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.try_send(r);
    });
    let end = Instant::now() + WAIT;
    loop {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(io::Error::other)?;
        match rx.try_recv() {
            Ok(r) => {
                r.map_err(io::Error::other)?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < end, "texture sample stalled")
            }
            Err(e) => return Err(io::Error::other(e)),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut rgba = Vec::new();
    {
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(io::Error::other)?;
        for line in mapped.chunks_exact(stride as usize) {
            rgba.extend_from_slice(&line[..row as usize]);
        }
    }
    buffer.unmap();
    Ok(rgba)
}
pub fn assert_colors(
    rgba: &[u8],
    width: u32,
    left: [u8; 4],
    peer: [u8; 4],
    peer_x: u32,
    peer_h: u32,
    tolerance: u8,
) {
    for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let x = i % width as usize;
        let y = i / width as usize;
        let expected = if x >= peer_x as usize && y < peer_h as usize {
            peer
        } else {
            left
        };
        assert!(
            p.iter()
                .zip(expected)
                .all(|(a, b)| a.abs_diff(b) <= tolerance),
            "wrong texture pixel ({x},{y}): {p:?} != {expected:?}"
        );
    }
}
