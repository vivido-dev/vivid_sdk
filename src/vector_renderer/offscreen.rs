//! Native vector rendering into bounded, asynchronously read back GUI images.
//!
//! This owns a native WGPU device and one readback slot. It creates no window,
//! terminal or event loop. Pixels are straight RGBA8; reading them grants no
//! Vivid presentation or input authority. The outer GUI acknowledges its own
//! displayed frame through the presenter's matching private receipt.

// Rust guideline compliant 2026-10-07
use super::*;
#[cfg(test)]
#[path = "offscreen_budget_tests.rs"]
mod budget_tests;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use vivid_vello_backend::Renderer;

/// Straight RGBA8 pixels ready for a GUI image upload.
#[derive(Clone)]
pub struct Pixels {
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Exactly `width * height * 4` bytes without GPU row padding.
    pub rgba: Arc<[u8]>,
}
impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pixels")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}
struct Readback {
    buffer: wgpu::Buffer,
    done: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    width: u32,
    height: u32,
    stride: u32,
    started: Instant,
}

type RenderResult = (Cache, Renderer, Result<(), super::Error>);
type RenderFuture = Pin<Box<dyn Future<Output = RenderResult>>>;
struct Rendering {
    future: RenderFuture,
    width: u32,
    height: u32,
    stride: u32,
    size: u64,
    started: Instant,
    cancelled: Arc<AtomicBool>,
}

/// One native vector renderer with an isolated GPU target and readback slot.
///
/// Construct and poll this renderer on its render worker. Pending Vello GPU
/// validation futures are thread-local; this type cannot move between threads.
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Option<Renderer>,
    cache: Cache,
    failed: Arc<AtomicBool>,
    pending: Option<Readback>,
    rendering: Option<Rendering>,
    retained_metrics: RenderMetrics,
    ready: Option<Pixels>,
    blank: Option<(u32, u32)>,
}
impl Offscreen {
    /// Creates an independent target on this renderer's device, queue and health.
    ///
    /// The caller bounds renderer count and polls both workers. Loss or logical
    /// destruction of their shared device invalidates both; cancelling one job
    /// does not destroy that device. No adapter or new device is created.
    ///
    /// # Errors
    /// Returns existing device failure or backend initialization failure.
    pub fn fork(&self) -> io::Result<Self> {
        self.check()?;
        let renderer = Renderer::new(
            &self.device,
            vivid_vello_backend::RendererOptions::default(),
        )
        .map_err(io::Error::other)?;
        Ok(Self {
            device: self.device.clone(),
            queue: self.queue.clone(),
            failed: self.failed.clone(),
            renderer: Some(renderer),
            cache: Cache::default(),
            pending: None,
            rendering: None,
            retained_metrics: RenderMetrics::default(),
            ready: None,
            blank: None,
        })
    }
    /// Opens a native adapter, falling back to a software GPU when necessary.
    ///
    /// # Errors
    /// Returns adapter, device or renderer initialization failures. A missing
    /// adapter is an error and never counts as successful rendering.
    pub async fn new() -> io::Result<Self> {
        let instance = wgpu::Instance::default();
        let mut selected = None;
        for (power, fallback) in [
            (wgpu::PowerPreference::HighPerformance, false),
            (wgpu::PowerPreference::LowPower, false),
            (wgpu::PowerPreference::LowPower, true),
        ] {
            if let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power,
                    force_fallback_adapter: fallback,
                    compatible_surface: None,
                    apply_limit_buckets: false,
                })
                .await
            {
                selected = Some(adapter);
                break;
            }
        }
        let adapter = selected.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no native vector GPU adapter is available",
            )
        })?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("vivid.vector.device"),
                ..Default::default()
            })
            .await
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
        let renderer = Renderer::new(&device, vivid_vello_backend::RendererOptions::default())
            .map_err(io::Error::other)?;
        Ok(Self {
            device,
            queue,
            renderer: Some(renderer),
            cache: Cache::default(),
            failed,
            pending: None,
            rendering: None,
            retained_metrics: RenderMetrics::default(),
            ready: None,
            blank: None,
        })
    }

    /// Starts a changed output image without blocking on GPU completion.
    ///
    /// `false` means the previous CPU image can be reused. It still needs a
    /// fresh host display acknowledgement after visibility or target changes.
    ///
    /// # Errors
    /// Returns `WouldBlock` until the previous readback is polled, rejects
    /// invalid geometry before allocation, and reports native GPU failures.
    pub fn render(&mut self, drawings: &[Drawing], width: u32, height: u32) -> io::Result<bool> {
        self.render_mixed(drawings, &[], width, height)
    }

    /// Starts raster-under-vector output with the same bounded readback policy.
    ///
    /// # Errors
    /// Returns pending-readback, invalid-layer/geometry or native GPU errors.
    pub fn render_mixed(
        &mut self,
        drawings: &[Drawing],
        rasters: &[super::super::RasterDrawing],
        width: u32,
        height: u32,
    ) -> io::Result<bool> {
        self.render_inner(drawings, rasters, width, height, None)
    }

    /// Starts checked output charged to a caller-wide dispatch budget.
    ///
    /// Budget exhaustion cancels unpublished output without blaming any drawing
    /// or changing device health. The ordinary per-job work bound still applies.
    ///
    /// # Errors
    /// Returns the same validation, capacity and device errors as [`Self::render_mixed`].
    pub fn render_budgeted(
        &mut self,
        drawings: &[Drawing],
        rasters: &[super::super::RasterDrawing],
        width: u32,
        height: u32,
        budget: &super::WorkBudget,
    ) -> io::Result<bool> {
        self.render_inner(drawings, rasters, width, height, Some(budget.clone()))
    }

    fn render_inner(
        &mut self,
        drawings: &[Drawing],
        rasters: &[super::super::RasterDrawing],
        width: u32,
        height: u32,
        budget: Option<super::WorkBudget>,
    ) -> io::Result<bool> {
        self.check()?;
        if self.pending.is_some() || self.rendering.is_some() || self.ready.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native vector readback is pending",
            ));
        }
        let (stride, size, bytes) =
            layout(width, height, self.device.limits().max_texture_dimension_2d)?;
        if size > self.device.limits().max_buffer_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native readback exceeds device limits",
            ));
        }
        if drawings.is_empty() && rasters.is_empty() && self.cache.target.is_none() {
            if self.blank == Some((width, height)) {
                return Ok(false);
            }
            self.blank = Some((width, height));
            self.ready = Some(Pixels {
                width,
                height,
                rgba: vec![0; bytes].into(),
            });
            return Ok(true);
        }
        self.blank = None;
        let Some(plan) = self
            .cache
            .prepare(&self.device, drawings, rasters, width, height)
            .map_err(io::Error::other)?
        else {
            return Ok(false);
        };
        self.retained_metrics = self.cache.metrics();
        let mut cache = std::mem::take(&mut self.cache);
        let mut renderer = self.renderer.take().expect("prepared native renderer");
        let device = self.device.clone();
        let queue = self.queue.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        self.rendering = Some(Rendering {
            future: Box::pin(async move {
                let result = cache
                    .checked_with_budget(
                        &device,
                        &queue,
                        &mut renderer,
                        plan,
                        &worker_cancelled,
                        budget.as_ref(),
                    )
                    .await;
                (cache, renderer, result)
            }),
            width,
            height,
            stride,
            size,
            started: Instant::now(),
            cancelled,
        });
        Ok(true)
    }

    fn begin_readback(
        &mut self,
        width: u32,
        height: u32,
        stride: u32,
        size: u64,
    ) -> io::Result<()> {
        let target = self
            .cache
            .target
            .as_ref()
            .ok_or_else(|| io::Error::other("native vector target is absent"))?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vivid.vector.readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vivid.vector.copy"),
            });
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, done) = mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.try_send(result);
            });
        self.pending = Some(Readback {
            buffer,
            done,
            width,
            height,
            stride,
            started: Instant::now(),
        });
        self.check()?;
        Ok(())
    }

    /// Polls GPU completion and converts one image to straight RGBA8.
    ///
    /// # Errors
    /// Reports allocation or bounded-work refusal without returning an image.
    /// Those failures leave the device reusable. Device loss, mapping failure or
    /// a ten-second validation/readback timeout require renderer replacement and
    /// invalidation of host presentation.
    pub fn poll(&mut self) -> io::Result<Option<Pixels>> {
        self.check()?;
        if let Some(pixels) = self.ready.take() {
            return Ok(Some(pixels));
        }
        if let Err(error) = self.device.poll(wgpu::PollType::Poll) {
            self.failed.store(true, Ordering::Release);
            return Err(io::Error::other(error));
        }
        self.check()?;
        if let Some(mut rendering) = self.rendering.take() {
            match rendering
                .future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Pending => {
                    if rendering.started.elapsed() >= Duration::from_secs(10) {
                        rendering.cancelled.store(true, Ordering::Release);
                        self.rendering = Some(rendering);
                        self.failed.store(true, Ordering::Release);
                        self.device.destroy();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "native allocation validation timed out",
                        ));
                    }
                    self.rendering = Some(rendering);
                    return Ok(None);
                }
                Poll::Ready((cache, renderer, result)) => {
                    self.cache = cache;
                    self.renderer = Some(renderer);
                    self.retained_metrics = self.cache.metrics();
                    result.map_err(io::Error::other)?;
                    self.begin_readback(
                        rendering.width,
                        rendering.height,
                        rendering.stride,
                        rendering.size,
                    )?;
                }
            }
        }
        let Some(readback) = &self.pending else {
            return Ok(None);
        };
        let result = match readback.done.try_recv() {
            Ok(result) => result.map_err(io::Error::other),
            Err(mpsc::TryRecvError::Empty)
                if readback.started.elapsed() < Duration::from_secs(10) =>
            {
                return Ok(None);
            }
            Err(mpsc::TryRecvError::Empty) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native vector GPU readback timed out",
            )),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("native vector GPU readback was cancelled"))
            }
        };
        let readback = self.pending.take().expect("pending native readback");
        if let Err(error) = result {
            self.failed.store(true, Ordering::Release);
            return Err(error);
        }
        let result = (|| {
            let mapped = readback
                .buffer
                .slice(..)
                .get_mapped_range()
                .map_err(io::Error::other)?;
            let expected = (readback.stride as usize)
                .checked_mul(readback.height as usize)
                .ok_or_else(|| io::Error::other("native mapped size overflow"))?;
            if mapped.len() != expected {
                return Err(io::Error::other(
                    "native mapped buffer has an invalid length",
                ));
            }
            let row = usize::try_from(readback.width)
                .ok()
                .and_then(|width| width.checked_mul(4))
                .ok_or_else(|| io::Error::other("native row length overflow"))?;
            let mut rgba = Vec::with_capacity(
                row.checked_mul(readback.height as usize)
                    .ok_or_else(|| io::Error::other("native output size overflow"))?,
            );
            for line in mapped
                .chunks_exact(readback.stride as usize)
                .take(readback.height as usize)
            {
                rgba.extend_from_slice(&line[..row]);
            }
            // Vello 0.11 already writes straight-alpha RGBA pixels.
            Ok(Pixels {
                width: readback.width,
                height: readback.height,
                rgba: rgba.into(),
            })
        })();
        readback.buffer.unmap();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result.map(Some)
    }

    /// Returns actual GPU pass, cache-hit and target-allocation counts.
    pub fn metrics(&self) -> RenderMetrics {
        if self.rendering.is_some() {
            self.retained_metrics
        } else {
            self.cache.metrics()
        }
    }

    /// Reports a native device, mapping or readback failure requiring replacement.
    ///
    /// This only observes this renderer's failure flag and grants no presentation
    /// authority. Keep calling [`Self::poll`] even without pending readback to
    /// service native device callbacks. Rejected geometry does not set the flag.
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Reports the selected adapter for explicit native GPU qualification.
    ///
    /// This reads the existing device's metadata without opening another adapter,
    /// changing selection or granting presentation authority. Tests can distinguish
    /// hardware backends from the software fallback allowed by [`Self::new`].
    #[cfg(feature = "testing")]
    pub fn adapter_info_for_test(&self) -> wgpu::AdapterInfo {
        self.device.adapter_info()
    }

    /// Destroys this isolated logical GPU device for frontend recovery tests.
    ///
    /// This uses WGPU's ordinary device destruction and loss callback; it does
    /// not inject presenter or input authority. Forks share this device loss;
    /// independent [`Self::new`] devices remain unaffected. Poll to service the callback.
    #[cfg(feature = "testing")]
    pub fn destroy_device_for_test(&self) {
        self.device.destroy();
    }

    fn check(&self) -> io::Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(io::Error::other("native vector GPU is unavailable"))
        } else {
            Ok(())
        }
    }
}
impl Drop for Offscreen {
    fn drop(&mut self) {
        self.pending.take();
        if let Some(rendering) = self.rendering.take() {
            // The pinned backend tolerates callbacks after their receiver retires.
            // Dropping the job submits no more work and never waits for the GPU.
            rendering.cancelled.store(true, Ordering::Release);
            drop(rendering);
        }
        self.cache.clear();
    }
}
impl std::fmt::Debug for Offscreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offscreen")
            .field("pending_validation", &self.rendering.is_some())
            .field("pending_readback", &self.pending.is_some())
            .field("metrics", &self.metrics())
            .finish_non_exhaustive()
    }
}
fn layout(width: u32, height: u32, limit: u32) -> io::Result<(u32, u64, usize)> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid native vector readback dimensions",
        )
    };
    if width == 0 || height == 0 || width > limit || height > limit {
        return Err(invalid());
    }
    let row = width.checked_mul(4).ok_or_else(invalid)?;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let stride = row
        .checked_add(alignment - 1)
        .map(|row| row / alignment * alignment)
        .ok_or_else(invalid)?;
    let size = u64::from(stride)
        .checked_mul(u64::from(height))
        .filter(|size| *size <= MAX_TARGET_BYTES)
        .ok_or_else(invalid)?;
    let bytes = usize::try_from(u64::from(row) * u64::from(height)).map_err(|_| invalid())?;
    Ok((stride, size, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a native GPU; straight-alpha readback and re-upload qualification"]
    fn readback_and_reupload_preserve_straight_alpha() {
        use vello::{
            Scene,
            kurbo::{Affine, Rect},
            peniko::{Blob, Color, Fill, ImageAlphaType, ImageData, ImageFormat},
        };
        use vivid_protocol::{
            identity::{PresenterInstanceId, SessionIdentity},
            overlay::wire::{Submission, WindowAddress},
        };
        let mut gpu = pollster::block_on(Offscreen::new()).unwrap();
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(64, 128, 192, 128),
            None,
            &Rect::new(0., 0., 64., 64.),
        );
        let mut drawing = Drawing {
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
        };
        let read = |gpu: &mut Offscreen| {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(image) = gpu.poll().unwrap() {
                    break image;
                }
                assert!(Instant::now() < end, "straight-alpha readback stalled");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let check = |image: &Pixels, expected: [u8; 4]| {
            let actual: [u8; 4] = image.rgba[..4].try_into().unwrap();
            assert!(
                actual
                    .into_iter()
                    .zip(expected)
                    .all(|(a, e)| a.abs_diff(e) <= 1),
                "straight alpha changed: {actual:?} != {expected:?}"
            );
        };
        assert!(gpu.render(std::slice::from_ref(&drawing), 64, 64).unwrap());
        let pixels = read(&mut gpu);
        check(&pixels, [64, 128, 192, 128]);
        // Offscreen exposes readback bytes, never its private mutable target.
        // Re-upload exactly that public output as an ordinary host image.
        let image = ImageData {
            data: Blob::new(Arc::new(pixels.rgba)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: pixels.width,
            height: pixels.height,
        };
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(32, 64, 96, 255),
            None,
            &Rect::new(0., 0., 64., 64.),
        );
        scene.draw_image(&image, Affine::IDENTITY);
        drawing.compiled = Arc::new(CompiledScene {
            scene,
            retained: Vec::new(),
            hits: Vec::new(),
            extra_bytes: 0,
        });
        drawing.submission.revision = 2;
        assert!(gpu.render(&[drawing], 64, 64).unwrap());
        check(&read(&mut gpu), [48, 96, 144, 255]);
    }

    #[test]
    fn readback_layout_is_checked_before_allocation_and_supports_unaligned_rows() {
        assert_eq!(layout(3, 2, 8192).unwrap(), (256, 512, 24));
        assert!(layout(0, 2, 8192).is_err());
        assert!(layout(u32::MAX, 2, u32::MAX).is_err());
        assert!(layout(8192, 8193, 16384).is_err());
        fn send<T: Send>(_: T) {}
        send(Offscreen::new());
    }
}
