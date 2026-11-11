//! Immutable, bounded Metal frames copied on the GPU into shareable IOSurfaces.
//!
//! The producer observes GPU completion before publication. The GUI retains the
//! frame lease through its own GPU completion; this is not a display receipt.
//! This in-process prototype accepts premultiplied-alpha source pixels only.

#![cfg(target_os = "macos")]

// Rust guideline compliant 2026-10-07
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CFDictionary, CFRetained, CFType};
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferCreate, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_metal::{MTLCommandQueue, MTLDevice, MTLResource, MTLTextureType};
use std::{
    fmt, io, ptr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Maximum bytes in one immutable shared frame (2048 square RGBA).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
// Match the native screenshot GPU deadline; callers must use a native worker.
const GPU_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Local {
    allocated: AtomicUsize,
    active: AtomicUsize,
    limit: usize,
    serial: AtomicU64,
}
/// Bounds retained GPU allocations across producers and retiring runtime instances.
#[derive(Clone, Debug)]
pub struct Budget(Arc<Quota>);
#[derive(Debug)]
struct Quota {
    allocated: AtomicUsize,
    active: AtomicUsize,
    limit: usize,
}
impl Budget {
    /// Creates an aggregate budget allowing one through 64 maximum-size frames.
    ///
    /// # Errors
    /// Returns `InvalidInput` for an invalid capacity.
    pub fn new(limit: usize) -> io::Result<Self> {
        if !(1..=64).contains(&limit) {
            return Err(invalid("invalid aggregate GPU frame capacity"));
        }
        Ok(Self(Arc::new(Quota {
            allocated: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            limit,
        })))
    }
    /// Reports active allocations, including retired producers' GPU work, excluding idle cache entries.
    pub fn retained(&self) -> usize {
        self.0.active.load(Ordering::Acquire)
    }
    /// Reports charged allocations, including idle entries and retiring producers' GPU work.
    pub fn allocated(&self) -> usize {
        self.0.allocated.load(Ordering::Acquire)
    }
    fn acquire(&self) -> io::Result<SharedPermit> {
        self.0
            .allocated
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.0.limit).then_some(used + 1)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "aggregate GPU frame capacity exhausted",
                )
            })?;
        Ok(SharedPermit(self.clone()))
    }
}
struct SharedPermit(Budget);
impl Drop for SharedPermit {
    fn drop(&mut self) {
        self.0.0.allocated.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Permit {
    local: Arc<Local>,
    _aggregate: SharedPermit,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.local.allocated.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Active {
    local: Arc<Local>,
    budget: Budget,
}
impl Active {
    fn new(permit: &Permit) -> Self {
        // One active lease owns each charged allocation; recycling happens only
        // after this lease retires, so neither count can exceed resident capacity.
        permit.local.active.fetch_add(1, Ordering::AcqRel);
        permit._aggregate.0.0.active.fetch_add(1, Ordering::AcqRel);
        Self {
            local: permit.local.clone(),
            budget: permit._aggregate.0.clone(),
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.local.active.fetch_sub(1, Ordering::AcqRel);
        self.budget.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Bounds immutable frames retained by one producer and its GUI consumers.
#[derive(Clone, Debug)]
pub struct Pool {
    local: Arc<Local>,
    budget: Budget,
    // Separate from Local: submitted storage must not retain a device/pipeline
    // through its permit and form a cycle with that device's completion callbacks.
    conversion: Arc<Mutex<Option<Arc<Conversion>>>>,
    images: Arc<Mutex<Option<Images>>>,
}
#[derive(Debug)]
struct Conversion {
    device: wgpu::Device,
    blitter: wgpu::util::TextureBlitter,
}
#[derive(Debug)]
struct Images {
    device: wgpu::Device,
    size: [u32; 2],
    idle: Arc<Idle>,
}
struct Idle(Mutex<Vec<Allocation>>);
impl fmt::Debug for Idle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleImages").finish_non_exhaustive()
    }
}
impl Pool {
    /// Creates a pool allowing one through eight frames, each at most 16 MiB.
    ///
    /// # Errors
    /// Returns `InvalidInput` for an invalid capacity.
    pub fn new(limit: usize) -> io::Result<Self> {
        Self::with_budget(limit, Budget::new(limit)?)
    }
    /// Creates a producer pool sharing a host-wide allocation budget.
    ///
    /// Both quotas stay charged through native storage and GUI/GPU retirement.
    /// Dropping a producer or its pool does not return a still-retained slot.
    ///
    /// # Errors
    /// Returns `InvalidInput` unless the producer limit is one through eight.
    pub fn with_budget(limit: usize, aggregate: Budget) -> io::Result<Self> {
        if !(1..=8).contains(&limit) {
            return Err(invalid("invalid shared frame capacity"));
        }
        Ok(Self {
            local: Arc::new(Local {
                allocated: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                limit,
                serial: AtomicU64::new(1),
            }),
            budget: aggregate,
            conversion: Arc::new(Mutex::new(None)),
            images: Arc::new(Mutex::new(None)),
        })
    }
    /// Reports frames still retained by the producer, scene or GPU commands.
    pub fn retained(&self) -> usize {
        self.local.active.load(Ordering::Acquire)
    }

    /// Reports charged allocations, including idle images and retiring GPU work.
    pub fn allocated(&self) -> usize {
        self.local.allocated.load(Ordering::Acquire)
    }

    /// Releases idle images and conversion resources without revoking frames or submitted work.
    ///
    /// Call on the native worker when hiding or retiring a renderer. Pool clones
    /// share the caches; the next export rebuilds them lazily. Old submitted work
    /// retains its resources, and late retirement cannot refill the cleared cache.
    ///
    /// # Panics
    /// Panics if another cache user poisoned its mutex.
    pub fn clear_cache(&self) {
        let previous = self
            .conversion
            .lock()
            .expect("GPU conversion cache mutex")
            .take();
        drop(previous);
        let previous = self.images.lock().expect("GPU image cache mutex").take();
        drop(previous);
    }

    fn allocation(
        &self,
        device: &wgpu::Device,
        metal: &ProtocolObject<dyn MTLDevice>,
        size: [u32; 2],
    ) -> io::Result<Arc<Storage>> {
        let idle = {
            let mut images = self.images.lock().expect("GPU image cache mutex");
            if images
                .as_ref()
                .is_none_or(|entry| entry.device != *device || entry.size != size)
            {
                *images = Some(Images {
                    device: device.clone(),
                    size,
                    idle: Arc::new(Idle(Mutex::new(Vec::with_capacity(self.local.limit)))),
                });
            }
            images
                .as_ref()
                .expect("image cache initialized")
                .idle
                .clone()
        };
        let available = idle.0.lock().expect("idle GPU image mutex").pop();
        let allocation = if let Some(allocation) = available {
            allocation
        } else {
            let aggregate = self.budget.acquire()?;
            self.local
                .allocated
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    (used < self.local.limit).then_some(used + 1)
                })
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "shared frame capacity exhausted")
                })?;
            Allocation::new(
                metal,
                size[0],
                size[1],
                kCVPixelFormatType_32BGRA,
                objc2_metal::MTLPixelFormat::BGRA8Unorm,
                Permit {
                    local: self.local.clone(),
                    _aggregate: aggregate,
                },
            )?
        };
        let active = Active::new(&allocation._permit);
        Ok(Arc::new(Storage {
            allocation: Some(allocation),
            active: Some(active),
            idle: Arc::downgrade(&idle),
            reusable: AtomicBool::new(false),
        }))
    }

    fn conversion(&self, device: &wgpu::Device, rgba: bool) -> Option<Arc<Conversion>> {
        let mut cache = self.conversion.lock().expect("GPU conversion cache mutex");
        // Device equality is logical, not physical: two WGPU devices may share
        // one Metal registry ID but cannot share pipelines or bind-group layouts.
        if cache.as_ref().is_some_and(|entry| entry.device != *device) {
            *cache = None;
        }
        rgba.then(|| {
            cache
                .get_or_insert_with(|| {
                    Arc::new(Conversion {
                        device: device.clone(),
                        blitter: wgpu::util::TextureBlitter::new(
                            device,
                            wgpu::TextureFormat::Bgra8Unorm,
                        ),
                    })
                })
                .clone()
        })
    }

    /// Copies a complete RGBA/BGRA texture on its own device and waits for completion.
    ///
    /// Call on a native worker, never the GUI thread. Every successful result
    /// owns an independent immutable IOSurface; source redraws cannot change it.
    /// Pixel data is never mapped or copied through CPU memory by this operation.
    ///
    /// # Errors
    /// Returns an error for unsupported geometry/format/backend, foreign device
    /// resources, exhausted capacity, native allocation or GPU completion failure.
    ///
    /// # Panics
    /// WGPU validation may panic for programming errors, including resources
    /// from different logical devices sharing the same physical Metal device.
    pub fn copy(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
    ) -> io::Result<Arc<Frame>> {
        self.begin_copy(device, queue, source)?.wait()
    }

    /// Submits an immutable GPU copy without waiting for GPU completion.
    ///
    /// Use the same source requirements as [`Pool::copy`]. Submission orders the
    /// copy before subsequent producer redraws on this queue. Poll the returned
    /// operation from the native event pump; it owns its device and allocation.
    /// Dropping it cancels publication while submitted work retains its storage.
    /// A pool caches one RGBA conversion pipeline for its most recent logical
    /// device. BGRA copies need no pipeline; switching devices evicts the cache.
    /// Released images are reused only for the current logical device and size.
    /// Idle images remain charged; [`Pool::clear_cache`] returns their capacity.
    ///
    /// # Errors
    /// Returns an error for unsupported sources, exhausted capacity or allocation failure.
    ///
    /// # Panics
    /// WGPU validation may panic for resources from incompatible logical devices.
    pub fn begin_copy(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
    ) -> io::Result<Pending> {
        let size = source.size();
        let bytes = usize::try_from(size.width)
            .ok()
            .and_then(|w| {
                usize::try_from(size.height)
                    .ok()
                    .and_then(|h| w.checked_mul(h))
            })
            .and_then(|n| n.checked_mul(4));
        if size.width == 0
            || size.height == 0
            || size.depth_or_array_layers != 1
            || size.width > 2048
            || size.height > 2048
            || bytes.is_none_or(|n| n > MAX_FRAME_BYTES)
            || source.dimension() != wgpu::TextureDimension::D2
            || source.sample_count() != 1
            || source.mip_level_count() != 1
            || !source.usage().contains(wgpu::TextureUsages::COPY_SRC)
        {
            return Err(invalid("invalid shared frame geometry or usage"));
        }
        let format = source.format();
        match format {
            wgpu::TextureFormat::Rgba8Unorm
                if source
                    .usage()
                    .contains(wgpu::TextureUsages::TEXTURE_BINDING) => {}
            wgpu::TextureFormat::Bgra8Unorm => (),
            _ => return Err(invalid("unsupported shared frame format")),
        }
        // SAFETY: These guards only inspect live backend objects, never mutate or
        // destroy them. Owned device references are retained before guards retire.
        let metal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| invalid("shared frames require Metal"))?
            .raw_device()
            .clone();
        {
            // SAFETY: Read-only backend inspection; both guards remain alive
            // while their native object devices are compared with this device.
            let raw_source = unsafe { source.as_hal::<wgpu::hal::api::Metal>() }
                .ok_or_else(|| invalid("shared source is not Metal"))?;
            let raw_queue = unsafe { queue.as_hal::<wgpu::hal::api::Metal>() }
                .ok_or_else(|| invalid("shared queue is not Metal"))?;
            if !ptr::eq(&*metal, &*raw_source.raw_handle().device())
                || !ptr::eq(&*metal, &*raw_queue.as_raw().device())
            {
                return Err(invalid("shared frame resources belong to another device"));
            }
        }
        let storage = self.allocation(device, &metal, [size.width, size.height])?;
        let id = self
            .local
            .serial
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| invalid("shared frame identity exhausted"))?;
        let raw = CVMetalTextureGetTexture(&storage.allocation().texture)
            .ok_or_else(|| invalid("shared Metal texture unavailable"))?;
        let lifetime = storage.clone();
        // SAFETY: CoreVideo created this 2D, one-layer, one-mip native texture
        // on the exact device above. The full copy initializes all pixels. Its
        // drop callback retains both CoreVideo wrappers and the allocation permit.
        let raw = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                wgpu::TextureFormat::Bgra8Unorm,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: size.width,
                    height: size.height,
                    depth: 1,
                },
                Some(Box::new(move || drop(lifetime))),
            )
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("vivid.shared-frame"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };
        // SAFETY: The native allocation matches desc and was made on this device;
        // contents may be discarded and are completely overwritten before publish.
        let output = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                raw,
                &desc,
                wgpu::TextureUses::UNINITIALIZED,
            )
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vivid.shared-frame.copy"),
        });
        if let Some(conversion) = self.conversion(device, format == wgpu::TextureFormat::Rgba8Unorm)
        {
            // CoreVideo's portable Metal-compatible packed allocation is BGRA.
            // The standard blitter converts RGBA on the GPU without CPU pixels.
            conversion.blitter.copy(
                device,
                &mut encoder,
                &source.create_view(&wgpu::TextureViewDescriptor::default()),
                &output.create_view(&wgpu::TextureViewDescriptor::default()),
            );
        } else {
            encoder.copy_texture_to_texture(source.as_image_copy(), output.as_image_copy(), size);
        }
        let submission = queue.submit([encoder.finish()]);
        // Retain native wrappers even on timeout/device-error return. The callback
        // retires only after the submitted work completes (or is abandoned on loss).
        let pending = storage.clone();
        let completed = Arc::new(AtomicBool::new(false));
        let notification = completed.clone();
        queue.on_submitted_work_done(move || {
            drop(pending);
            notification.store(true, Ordering::Release);
        });
        Ok(Pending {
            device: device.clone(),
            submission,
            started: Instant::now(),
            completed,
            output: Some(output),
            frame: Some(Arc::new(Frame {
                id,
                width: size.width,
                height: size.height,
                device: metal.registryID(),
                _storage: storage,
            })),
        })
    }
}

/// A submitted GPU export that publishes its immutable frame once.
#[derive(Debug)]
pub struct Pending {
    device: wgpu::Device,
    submission: wgpu::SubmissionIndex,
    started: Instant,
    completed: Arc<AtomicBool>,
    output: Option<wgpu::Texture>,
    frame: Option<Arc<Frame>>,
}
impl Pending {
    /// Waits for completion on a native worker, with the existing GPU deadline.
    ///
    /// Use for synchronous export or worker teardown; never call on the GUI thread.
    ///
    /// # Errors
    /// Returns an error after consumption, GPU wait failure or deadline expiry.
    pub fn wait(mut self) -> io::Result<Arc<Frame>> {
        if self.frame.is_none() {
            return Err(invalid("shared frame operation consumed"));
        }
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(self.submission.clone()),
                timeout: Some(GPU_TIMEOUT.saturating_sub(self.started.elapsed())),
            })
            .map_err(|_| io::Error::other("shared frame GPU completion failed"))?;
        self.poll()?
            .ok_or_else(|| io::Error::other("shared frame completion absent"))
    }
    /// Observes GPU progress without waiting, returning the completed frame once.
    ///
    /// Returns `None` while work is pending. The absolute five-second deadline
    /// starts at submission. After completion or error this operation is consumed.
    /// Dropped or failed operations retain submitted resources until GPU retirement.
    ///
    /// # Errors
    /// Returns `TimedOut` after the deadline, `InvalidInput` after consumption,
    /// or another error when device polling or resource retirement fails.
    pub fn poll(&mut self) -> io::Result<Option<Arc<Frame>>> {
        if self.frame.is_none() {
            return Err(invalid("shared frame operation consumed"));
        }
        let result = (|| {
            if self.started.elapsed() > GPU_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shared frame GPU deadline expired",
                ));
            }
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|_| io::Error::other("shared frame GPU polling failed"))?;
            if !self.completed.load(Ordering::Acquire) {
                return Ok(None);
            }
            self.output.take();
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|_| io::Error::other("shared frame retirement failed"))?;
            if let Some(frame) = &self.frame {
                frame._storage.reusable.store(true, Ordering::Release);
            }
            Ok(self.frame.take())
        })();
        if result.is_err() {
            self.output.take();
            self.frame.take();
            // Expired work may already have completed while its owner was busy.
            let _ = self.device.poll(wgpu::PollType::Poll);
        }
        result
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct Storage {
    allocation: Option<Allocation>,
    active: Option<Active>,
    idle: Weak<Idle>,
    reusable: AtomicBool,
}
impl Storage {
    fn allocation(&self) -> &Allocation {
        self.allocation.as_ref().expect("live GPU frame allocation")
    }
}
impl Drop for Storage {
    fn drop(&mut self) {
        // This is the last producer/HAL/GUI/GPU reference. Return active capacity
        // before making the native allocation available to a new export lease.
        self.active.take();
        if self.reusable.load(Ordering::Acquire)
            && let Some(idle) = self.idle.upgrade()
            && let Ok(mut slots) = idle.0.lock()
            && let Some(allocation) = self.allocation.take()
        {
            slots.push(allocation);
        }
    }
}
struct Allocation {
    _buffer: CFRetained<CVPixelBuffer>,
    texture: CFRetained<CVMetalTexture>,
    _permit: Permit,
}
// SAFETY: Both CF objects have thread-safe reference counting. Their handles
// remain private and are never CPU-mapped. Only an exclusive checkout starts
// GPU writes. Storage and all consumer leases retire before it can be recycled.
unsafe impl Send for Allocation {}
// SAFETY: A Storage owns its allocation until all producer/HAL/GUI/GPU references
// retire. Shared references only inspect private handles; no mutation/reuse can
// begin while this Storage still exists, including native completion callbacks.
unsafe impl Send for Storage {}
// SAFETY: Shared references only retain the immutable buffer/texture or release
// them. The one GPU write finishes before Frame is published to any consumer.
unsafe impl Sync for Storage {}
impl Allocation {
    fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        width: u32,
        height: u32,
        format: u32,
        metal_format: objc2_metal::MTLPixelFormat,
        permit: Permit,
    ) -> io::Result<Self> {
        let empty = CFDictionary::<CFType, CFType>::empty();
        // SAFETY: CoreVideo exposes immortal attribute keys. The IOSurface
        // property key with an empty dictionary requests shareable backing.
        let attributes = unsafe {
            let values: [&CFType; 2] = [
                empty.as_ref(),
                objc2_core_foundation::kCFBooleanTrue
                    .ok_or_else(|| invalid("CoreFoundation boolean unavailable"))?
                    .as_ref(),
            ];
            CFDictionary::from_slices(
                &[
                    kCVPixelBufferIOSurfacePropertiesKey,
                    kCVPixelBufferMetalCompatibilityKey,
                ],
                &values,
            )
        };
        let mut buffer = ptr::null_mut();
        // SAFETY: Dimensions were bounded above, attributes are live, and the
        // successful Create call transfers a retained buffer through this output.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width as usize,
                height as usize,
                format,
                Some(attributes.as_opaque()),
                ptr::NonNull::from(&mut buffer),
            )
        };
        if status != kCVReturnSuccess {
            return Err(io::Error::other("shared pixel buffer allocation failed"));
        }
        // SAFETY: A successful Create supplied the non-null owned CF object.
        let buffer = unsafe {
            CFRetained::from_raw(
                ptr::NonNull::new(buffer).ok_or_else(|| invalid("shared buffer absent"))?,
            )
        };
        let mut cache = ptr::null_mut();
        // SAFETY: The device and output are live; the cache is local to this copy.
        let status = unsafe {
            CVMetalTextureCache::create(None, None, device, None, ptr::NonNull::from(&mut cache))
        };
        if status != kCVReturnSuccess {
            return Err(io::Error::other("shared texture cache allocation failed"));
        }
        // SAFETY: A successful Create transfers the non-null cache reference.
        let cache = unsafe {
            CFRetained::from_raw(
                ptr::NonNull::new(cache).ok_or_else(|| invalid("shared cache absent"))?,
            )
        };
        let mut texture = ptr::null_mut();
        // SAFETY: Pixel format, extent and plane zero match the validated buffer.
        let usage = objc2_core_foundation::CFNumber::new_i64(
            (objc2_metal::MTLTextureUsage::ShaderRead | objc2_metal::MTLTextureUsage::RenderTarget)
                .0 as i64,
        );
        // SAFETY: The immortal usage key and retained number are valid CF objects.
        let texture_attributes = unsafe {
            CFDictionary::<objc2_core_foundation::CFString,objc2_core_foundation::CFNumber>::from_slices(&[objc2_core_video::kCVMetalTextureUsage],&[usage.as_ref()])
        };
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &cache,
                &buffer,
                Some(texture_attributes.as_opaque()),
                metal_format,
                width as usize,
                height as usize,
                0,
                ptr::NonNull::from(&mut texture),
            )
        };
        if status != kCVReturnSuccess {
            return Err(io::Error::other("shared Metal mapping failed"));
        }
        // SAFETY: A successful Create transfers the non-null mapping reference.
        let texture = unsafe {
            CFRetained::from_raw(
                ptr::NonNull::new(texture).ok_or_else(|| invalid("shared mapping absent"))?,
            )
        };
        Ok(Self {
            _buffer: buffer,
            texture,
            _permit: permit,
        })
    }
}

/// An immutable completed GPU image; native handles remain private.
pub struct Frame {
    id: u64,
    width: u32,
    height: u32,
    device: u64,
    _storage: Arc<Storage>,
}
impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedFrame")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}
impl Frame {
    /// Returns the checked physical pixel extent.
    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }
    /// Returns the frame serial, scoped to its originating pool.
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Identifies the physical Metal device that completed the producer copy.
    pub fn device_registry_id(&self) -> u64 {
        self.device
    }
    /// Paints the image while retaining its lease through scene and GPU completion.
    ///
    /// # Panics
    /// Must be called during the window's paint phase, as for other GUI paint APIs.
    #[cfg(feature = "gui")]
    pub fn paint(self: &Arc<Self>, bounds: gpui::Bounds<gpui::Pixels>, window: &mut gpui::Window) {
        window.paint_surface_with_lease(
            bounds,
            self._storage.allocation()._buffer.clone(),
            self.clone(),
        );
    }
}
#[cfg(feature = "gui")]
impl gpui::SurfaceLease for Frame {
    fn device_registry_id(&self) -> u64 {
        self.device
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_bounded() {
        for limit in [0, 9, usize::MAX] {
            assert_eq!(
                Pool::new(limit).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        for limit in [0, 65, usize::MAX] {
            assert!(Budget::new(limit).is_err());
        }
        assert_eq!(Pool::new(8).unwrap().retained(), 0);
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn immutable_formats_capacity_and_validation() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let make_texture = |format, width, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("sharing-test-source"),
                size: wgpu::Extent3d {
                    width,
                    height: 8,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let usage = wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::TEXTURE_BINDING;
        let pool = Pool::new(1).unwrap();
        for (format, width, usage) in [
            (wgpu::TextureFormat::Rgba8UnormSrgb, 16, usage),
            (wgpu::TextureFormat::Rgba8Unorm, 2049, usage),
            (
                wgpu::TextureFormat::Rgba8Unorm,
                16,
                wgpu::TextureUsages::COPY_SRC,
            ),
            (
                wgpu::TextureFormat::Bgra8Unorm,
                16,
                wgpu::TextureUsages::COPY_DST,
            ),
        ] {
            let source = make_texture(format, width, usage);
            assert_eq!(
                pool.copy(&device, &queue, &source).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(pool.retained(), 0);
        }
        let mut previous_id = 0;
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ] {
            let source = make_texture(format, 16, usage);
            let mut pixels = Vec::new();
            for y in 0..8u8 {
                for x in 0..16u8 {
                    let rgba = [x * 4, y * 8, 10, 128];
                    pixels.extend(if format == wgpu::TextureFormat::Rgba8Unorm {
                        rgba
                    } else {
                        [rgba[2], rgba[1], rgba[0], rgba[3]]
                    });
                }
            }
            let layout = wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(8),
            };
            queue.write_texture(source.as_image_copy(), &pixels, layout, source.size());
            let frame = pool.copy(&device, &queue, &source).unwrap();
            assert!(frame.id() > previous_id);
            previous_id = frame.id();
            assert_eq!(frame.size(), [16, 8]);
            assert_eq!(
                pool.copy(&device, &queue, &source).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            let retained = frame.clone();
            drop(frame);
            assert_eq!(pool.retained(), 1);
            // A subsequent producer redraw must not mutate the published frame.
            queue.write_texture(source.as_image_copy(), &[0; 512], layout, source.size());
            // A fresh export submits the queued upload and proves that it
            // reached the source while the original immutable image is retained.
            let peer_pool = Pool::new(1).unwrap();
            let overwritten = peer_pool.copy(&device, &queue, &source).unwrap();
            assert_eq!(overwritten.id(), 1);
            assert!(read_bgra(&overwritten).iter().all(|byte| *byte == 0));
            drop(overwritten);
            assert_eq!(peer_pool.retained(), 0);
            assert_eq!(pool.retained(), 1);
            let actual = read_bgra(&retained);
            for y in 0..8usize {
                for x in 0..16usize {
                    assert_eq!(
                        &actual[(y * 16 + x) * 4..(y * 16 + x + 1) * 4],
                        &[10, y as u8 * 8, x as u8 * 4, 128]
                    );
                }
            }
            drop(retained);
            assert_eq!(pool.retained(), 0);
        }
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn submitted_exports_order_redraws_expire_and_retire_without_publication() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("async-sharing-source"),
            size: wgpu::Extent3d {
                width: 16,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(64),
            rows_per_image: Some(8),
        };
        queue.write_texture(
            source.as_image_copy(),
            &[80, 30, 10, 128].repeat(128),
            layout,
            source.size(),
        );
        let pool = Pool::new(1).unwrap();
        let mut pending = pool.begin_copy(&device, &queue, &source).unwrap();
        assert_eq!(pool.retained(), 1);
        // The renderer may submit another redraw before observing export completion.
        queue.write_texture(source.as_image_copy(), &[0; 512], layout, source.size());
        queue.submit([]);
        let frame = loop {
            if let Some(frame) = pending.poll().unwrap() {
                break frame;
            }
            std::thread::yield_now();
        };
        assert_eq!(read_bgra(&frame), [10, 30, 80, 128].repeat(128));
        assert_eq!(
            pending.poll().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        drop(frame);
        assert_eq!(pool.retained(), 0);
        let mut expired = pool.begin_copy(&device, &queue, &source).unwrap();
        expired.started = Instant::now() - GPU_TIMEOUT - Duration::from_millis(1);
        assert_eq!(expired.poll().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            expired.poll().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(GPU_TIMEOUT),
            })
            .unwrap();
        assert_eq!(pool.retained(), 0);
        drop(pool.begin_copy(&device, &queue, &source).unwrap());
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(GPU_TIMEOUT),
            })
            .unwrap();
        assert_eq!(pool.retained(), 0);
        // Closing producers cannot make a replacement bypass still-retained frames.
        let budget = Budget::new(2).unwrap();
        let a = Pool::with_budget(1, budget.clone()).unwrap();
        let b = Pool::with_budget(1, budget.clone()).unwrap();
        let first = a.copy(&device, &queue, &source).unwrap();
        let peer = b.copy(&device, &queue, &source).unwrap();
        assert_eq!(first.id(), peer.id());
        assert_eq!(budget.retained(), 2);
        drop(a);
        drop(b);
        let replacement = Pool::with_budget(1, budget.clone()).unwrap();
        assert_eq!(
            replacement
                .begin_copy(&device, &queue, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(replacement.retained(), 0);
        drop(first);
        let next = replacement.copy(&device, &queue, &source).unwrap();
        assert_eq!(next.id(), 1);
        drop(next);
        assert_eq!(budget.retained(), 1);
        assert!(read_bgra(&peer).iter().all(|byte| *byte == 0));
        drop(peer);
        assert_eq!(budget.retained(), 0);
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn conversion_cache_reuses_only_the_logical_device_and_retires_independently() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let (other, other_queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        assert_ne!(device, other);
        let source =
            |device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, width| {
                let source = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("conversion-cache-source"),
                    size: wgpu::Extent3d {
                        width,
                        height: 8,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let color = if format == wgpu::TextureFormat::Rgba8Unorm {
                    [80u8, 30, 10, 128]
                } else {
                    [10u8, 30, 80, 128]
                };
                queue.write_texture(
                    source.as_image_copy(),
                    &color.repeat(width as usize * 8),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width * 4),
                        rows_per_image: Some(8),
                    },
                    source.size(),
                );
                source
            };
        let cached = |pool: &Pool| pool.conversion.lock().unwrap().as_ref().map(Arc::downgrade);
        let budget = Budget::new(8).unwrap();
        let pool = Pool::with_budget(4, budget.clone()).unwrap();
        let clone = pool.clone();
        let bgra = source(&device, &queue, wgpu::TextureFormat::Bgra8Unorm, 16);
        let direct = Pool::new(1).unwrap();
        drop(direct.copy(&device, &queue, &bgra).unwrap());
        assert!(cached(&direct).is_none());
        drop(direct);
        let rgba = source(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, 16);
        let first = pool.copy(&device, &queue, &rgba).unwrap();
        let original = cached(&pool).unwrap();
        let resized = source(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, 32);
        let next = clone.copy(&device, &queue, &resized).unwrap();
        assert!(std::sync::Weak::ptr_eq(&original, &cached(&clone).unwrap()));
        assert_eq!(read_bgra(&next), [10, 30, 80, 128].repeat(32 * 8));
        drop(next);
        let pending = pool.begin_copy(&device, &queue, &rgba).unwrap();
        let other_rgba = source(&other, &other_queue, wgpu::TextureFormat::Rgba8Unorm, 16);
        let other_pending = pool.begin_copy(&other, &other_queue, &other_rgba).unwrap();
        assert!(original.upgrade().is_none());
        let replacement = cached(&pool).unwrap();
        // BGRA on a different logical device also evicts the retired RGBA cache.
        drop(pool.copy(&device, &queue, &bgra).unwrap());
        assert!(cached(&pool).is_none());
        assert!(replacement.upgrade().is_none());
        let completed = pending.wait().unwrap();
        let other_completed = other_pending.wait().unwrap();
        assert_eq!(
            first.device_registry_id(),
            other_completed.device_registry_id()
        );
        for frame in [&first, &completed, &other_completed] {
            assert_eq!(read_bgra(frame), [10, 30, 80, 128].repeat(16 * 8));
        }
        drop(completed);
        drop(other_completed);
        let peer = Pool::with_budget(1, budget.clone()).unwrap();
        let peer_frame = peer.copy(&other, &other_queue, &other_rgba).unwrap();
        assert_eq!(first.id(), peer_frame.id());
        let peer_cache = cached(&peer).unwrap();
        let pending = pool.begin_copy(&device, &queue, &rgba).unwrap();
        let before_clear = cached(&pool).unwrap();
        clone.clear_cache();
        assert!(before_clear.upgrade().is_none());
        assert!(peer_cache.upgrade().is_some());
        let completed = pending.wait().unwrap();
        assert_eq!(read_bgra(&completed), read_bgra(&first));
        drop(completed);
        let pending = pool.begin_copy(&device, &queue, &rgba).unwrap();
        let before_drop = cached(&pool).unwrap();
        drop(pool);
        assert!(before_drop.upgrade().is_some());
        drop(clone);
        assert!(before_drop.upgrade().is_none());
        let completed = pending.wait().unwrap();
        assert_eq!(read_bgra(&completed), read_bgra(&first));
        drop(completed);
        drop(peer);
        assert!(peer_cache.upgrade().is_none());
        assert_eq!(budget.retained(), 2);
        assert_eq!(read_bgra(&peer_frame), [10, 30, 80, 128].repeat(16 * 8));
        drop(first);
        drop(peer_frame);
        assert_eq!(budget.retained(), 0);
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn image_reuse_waits_for_every_reference_and_keeps_idle_capacity_charged() {
        let (device, queue) = native_device();
        let source = solid_source(&device, &queue, 16, [80, 30, 10, 128]);
        let budget = Budget::new(2).unwrap();
        let pool = Pool::with_budget(2, budget.clone()).unwrap();
        let clone = pool.clone();
        let peer = Pool::with_budget(1, budget.clone()).unwrap();
        let first = pool.copy(&device, &queue, &source).unwrap();
        let first_buffer = buffer_identity(&first);
        let held = first.clone();
        drop(first);
        queue.write_texture(
            source.as_image_copy(),
            &[0; 512],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(8),
            },
            source.size(),
        );
        let second = pool.copy(&device, &queue, &source).unwrap();
        let second_buffer = buffer_identity(&second);
        assert_ne!(first_buffer, second_buffer);
        assert_eq!(read_bgra(&held), [10, 30, 80, 128].repeat(128));
        assert!(read_bgra(&second).iter().all(|byte| *byte == 0));
        drop(second);
        assert_eq!((pool.retained(), pool.allocated()), (1, 2));
        assert_eq!((budget.retained(), budget.allocated()), (1, 2));
        assert_eq!(
            peer.begin_copy(&device, &queue, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!((peer.retained(), peer.allocated()), (0, 0));
        let next = clone.copy(&device, &queue, &source).unwrap();
        assert_eq!(buffer_identity(&next), second_buffer);
        assert_eq!(next.id(), 3);
        drop(held);
        let reused_first = pool.copy(&device, &queue, &source).unwrap();
        assert_eq!(buffer_identity(&reused_first), first_buffer);
        assert_eq!(reused_first.id(), 4);
        assert!(read_bgra(&reused_first).iter().all(|byte| *byte == 0));
        // A HAL or consumer-GPU reference must block reuse even after Frame retires.
        let pending_owner = next._storage.clone();
        drop(next);
        assert_eq!(pool.retained(), 2);
        assert_eq!(
            pool.begin_copy(&device, &queue, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(pending_owner);
        let returned = pool.copy(&device, &queue, &source).unwrap();
        assert_eq!(buffer_identity(&returned), second_buffer);
        drop(returned);
        drop(reused_first);
        assert_eq!((budget.retained(), budget.allocated()), (0, 2));
        clone.clear_cache();
        assert_eq!((budget.retained(), budget.allocated()), (0, 0));
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn image_generations_retire_on_resize_clear_and_owner_drop_without_touching_a_peer() {
        let (device, queue) = native_device();
        let source = solid_source(&device, &queue, 16, [80, 30, 10, 128]);
        let resized = solid_source(&device, &queue, 32, [40, 20, 60, 128]);
        let budget = Budget::new(3).unwrap();
        let first = Pool::with_budget(2, budget.clone()).unwrap();
        let peer = Pool::with_budget(1, budget.clone()).unwrap();
        let old_frame = first.copy(&device, &queue, &source).unwrap();
        let peer_frame = peer.copy(&device, &queue, &source).unwrap();
        assert_eq!(old_frame.id(), peer_frame.id());
        let old = Arc::downgrade(&first.images.lock().unwrap().as_ref().unwrap().idle);
        let peer_cache = Arc::downgrade(&peer.images.lock().unwrap().as_ref().unwrap().idle);
        drop(first.copy(&device, &queue, &source).unwrap());
        assert_eq!(budget.allocated(), 3);
        let new_frame = first.copy(&device, &queue, &resized).unwrap();
        assert!(old.upgrade().is_none());
        assert_eq!(budget.allocated(), 3);
        drop(old_frame);
        assert_eq!(budget.allocated(), 2);
        assert!(
            first
                .images
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .idle
                .0
                .lock()
                .unwrap()
                .is_empty()
        );
        let current = Arc::downgrade(&first.images.lock().unwrap().as_ref().unwrap().idle);
        first.clear_cache();
        assert!(current.upgrade().is_none());
        assert_eq!(first.allocated(), 1);
        assert_eq!(read_bgra(&new_frame), [60, 20, 40, 128].repeat(32 * 8));
        drop(new_frame);
        assert_eq!(first.allocated(), 0);
        assert!(peer_cache.upgrade().is_some());
        assert_eq!(read_bgra(&peer_frame), [10, 30, 80, 128].repeat(128));
        drop(first);
        let replacement = Pool::with_budget(1, budget.clone()).unwrap();
        let replacement_frame = replacement.copy(&device, &queue, &resized).unwrap();
        assert_eq!(replacement_frame.id(), peer_frame.id());
        drop(replacement);
        assert_eq!(budget.allocated(), 2);
        drop(replacement_frame);
        assert_eq!(budget.allocated(), 1);
        drop(peer_frame);
        assert_eq!((peer.retained(), peer.allocated()), (0, 1));
        peer.clear_cache();
        assert_eq!((budget.retained(), budget.allocated()), (0, 0));
    }

    #[test]
    #[ignore = "requires a native Metal device; run explicitly on macOS"]
    fn cancelled_and_expired_leases_discard_even_previously_recycled_images() {
        let (device, queue) = native_device();
        let source = solid_source(&device, &queue, 16, [80, 30, 10, 128]);
        let pool = Pool::new(1).unwrap();
        let finish = || {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(GPU_TIMEOUT),
                })
                .unwrap();
        };
        let mut expired = pool.begin_copy(&device, &queue, &source).unwrap();
        expired.started = Instant::now() - GPU_TIMEOUT - Duration::from_millis(1);
        assert_eq!(expired.poll().unwrap_err().kind(), io::ErrorKind::TimedOut);
        finish();
        assert_eq!((pool.retained(), pool.allocated()), (0, 0));
        drop(pool.begin_copy(&device, &queue, &source).unwrap());
        finish();
        assert_eq!((pool.retained(), pool.allocated()), (0, 0));
        drop(pool.copy(&device, &queue, &source).unwrap());
        assert_eq!((pool.retained(), pool.allocated()), (0, 1));
        drop(pool.begin_copy(&device, &queue, &source).unwrap());
        finish();
        assert_eq!((pool.retained(), pool.allocated()), (0, 0));
    }

    fn native_device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        pollster::block_on(adapter.request_device(&Default::default())).unwrap()
    }
    fn solid_source(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        color: [u8; 4],
    ) -> wgpu::Texture {
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("recycled-image-source"),
            size: wgpu::Extent3d {
                width,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        queue.write_texture(
            source.as_image_copy(),
            &color.repeat(width as usize * 8),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(8),
            },
            source.size(),
        );
        source
    }
    fn buffer_identity(frame: &Frame) -> usize {
        ptr::from_ref(&*frame._storage.allocation()._buffer) as usize
    }

    fn read_bgra(frame: &Frame) -> Vec<u8> {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let buffer = &frame._storage.allocation()._buffer;
        // SAFETY: Test-only readback starts after producer GPU completion, holds
        // a read-only lock, and copies only checked rows while the frame is live.
        unsafe {
            let flags = CVPixelBufferLockFlags::ReadOnly;
            assert_eq!(
                CVPixelBufferLockBaseAddress(buffer, flags),
                kCVReturnSuccess
            );
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let bytes = std::slice::from_raw_parts(
                CVPixelBufferGetBaseAddress(buffer).cast::<u8>(),
                stride * frame.height as usize,
            );
            let result = bytes
                .chunks_exact(stride)
                .take(frame.height as usize)
                .flat_map(|row| row[..frame.width as usize * 4].iter().copied())
                .collect();
            assert_eq!(
                CVPixelBufferUnlockBaseAddress(buffer, flags),
                kCVReturnSuccess
            );
            result
        }
    }
}
