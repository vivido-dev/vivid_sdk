//! Native vector rendering into bounded, asynchronously read back GUI images.
//!
//! This owns a native WGPU device and one readback slot. It creates no window,
//! terminal or event loop. Pixels are straight RGBA8; reading them grants no
//! Vivid presentation or input authority. The outer GUI acknowledges its own
//! displayed frame through the presenter's matching private receipt.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

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

/// One native vector renderer with an isolated GPU target and readback slot.
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    cache: Compositor,
    failed: Arc<AtomicBool>,
    pending: Option<Readback>,
    ready: Option<Pixels>,
    blank: Option<(u32, u32)>,
}
impl Offscreen {
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
        let renderer =
            Renderer::new(&device, vello::RendererOptions::default()).map_err(io::Error::other)?;
        Ok(Self {
            device,
            queue,
            renderer,
            cache: Compositor::default(),
            failed,
            pending: None,
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
        self.check()?;
        if self.pending.is_some() || self.ready.is_some() {
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
        let (_, changed) = self
            .cache
            .render_mixed(
                &self.device,
                &self.queue,
                &mut self.renderer,
                drawings,
                rasters,
                width,
                height,
            )
            .map_err(io::Error::other)?;
        self.check()?;
        if !changed {
            return Ok(false);
        }
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
            target._texture.as_image_copy(),
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
        Ok(true)
    }

    /// Polls GPU completion and converts one image to straight RGBA8.
    ///
    /// # Errors
    /// Reports device loss, mapping failures or a ten-second readback timeout.
    /// Recreate the renderer and invalidate host presentation after a failure.
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
            for pixel in rgba.as_chunks_mut::<4>().0 {
                let alpha = u16::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = (u16::from(*channel) * 255 + alpha / 2)
                        .checked_div(alpha)
                        .unwrap_or(0)
                        .min(255) as u8;
                }
            }
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
        self.cache.metrics()
    }

    /// Reports a native device, mapping or readback failure requiring replacement.
    ///
    /// This only observes this renderer's failure flag and grants no presentation
    /// authority. Keep calling [`Self::poll`] even without pending readback to
    /// service native device callbacks. Rejected geometry does not set the flag.
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Destroys this isolated logical GPU device for frontend recovery tests.
    ///
    /// This uses WGPU's ordinary device destruction and loss callback; it does
    /// not inject presenter or input authority. Other renderers keep their own
    /// devices. Poll this renderer to service the resulting callback.
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
        self.cache.clear(&mut self.renderer);
    }
}
impl std::fmt::Debug for Offscreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offscreen")
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
    fn readback_layout_is_checked_before_allocation_and_supports_unaligned_rows() {
        assert_eq!(layout(3, 2, 8192).unwrap(), (256, 512, 24));
        assert!(layout(0, 2, 8192).is_err());
        assert!(layout(u32::MAX, 2, u32::MAX).is_err());
        assert!(layout(8192, 8193, 16384).is_err());
        fn send<T: Send>(_: T) {}
        send(Offscreen::new());
    }
}
