//! Checked immutable GPU outputs for an existing host-owned device.
//!
//! Construct and poll `TextureRenderer` on its render worker. The host's primary
//! renderer remains independently usable. Output contains no Vivid authority;
//! retain the exact prepared snapshot with each job and acknowledge only the
//! image actually composed by the host. Poll while cancellation is retiring.
//!
//! Imports retain the output's allocation charge through their image data.
//! Unregister each imported image from its original renderer before releasing
//! its last image clone. Never reuse an import with a different logical device.

// Rust guideline compliant 2026-10-07
mod pool;
mod worker;

#[doc(inline)]
pub use pool::TexturePool;

use super::{Cache, Drawing, RasterDrawing, RenderMetrics, WorkBudget};
use pool::Lease;
use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
    sync::{Arc, Mutex, Weak},
};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// A host-owned logical device, queue and existing failure observer.
///
/// `failed` must observe the host's device-error/loss callbacks. This adapter
/// installs no callbacks and creates no device. Keep the observer fast and
/// safe to call from the render worker; `true` means GPU work must retire.
#[derive(Clone)]
pub struct SharedDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    failed: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl SharedDevice {
    /// Borrows device ownership and the host's existing failure observer.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        failed: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            failed: Arc::new(failed),
        }
    }
}
impl fmt::Debug for SharedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedDevice").finish_non_exhaustive()
    }
}

/// An opaque job identity, unique while any handle or completion remains live.
#[derive(Clone, Debug)]
pub struct JobId(Arc<()>);
impl PartialEq for JobId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for JobId {}
impl Hash for JobId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

struct Allocation {
    texture: wgpu::Texture,
    queue: wgpu::Queue,
    _lease: Lease,
}
struct Registration(Mutex<Option<Arc<Allocation>>>);
impl AsRef<[u8]> for Registration {
    fn as_ref(&self) -> &[u8] {
        &[]
    }
}

/// An immutable straight-alpha RGBA8 export with a retained allocation charge.
///
/// Cloning shares the original allocation. Future render jobs never write into
/// this texture. Register it only on the same logical device used for rendering.
#[derive(Clone)]
pub struct Texture(Arc<Allocation>);
impl Texture {
    /// Returns physical output dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.0.texture.width(), self.0.texture.height())
    }

    /// Reports whether two handles share exactly the same immutable output.
    pub fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Registers output on the host's original upstream Vello renderer.
    ///
    /// The import and its image clones retain the allocation charge until
    /// explicitly unregistered. Register once per import and unregister it from
    /// this same renderer after its final use. Unregister invalidates all clones.
    /// The host must use the export's original logical device and preserve it
    /// through rendering. Registration grants no display receipt or input grant.
    pub fn register(&self, renderer: &mut vello::Renderer) -> TextureImport {
        let (width, height) = self.dimensions();
        let registration = Arc::new(Registration(Mutex::new(Some(self.0.clone()))));
        let image = ImageData {
            data: Blob::new(registration.clone()),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        renderer.override_image(
            &image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: self.0.texture.clone(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        TextureImport {
            image,
            registration,
        }
    }
}

/// A same-device Vello import retaining its source allocation until unregistered.
///
/// Clones share one registration. Explicitly unregister after the final use;
/// dropping a guard alone does not remove the host renderer's texture override.
/// Cached atlas metadata may outlive unregister without retaining the source GPU
/// allocation. Raw image clones must not be rendered after unregister.
#[derive(Clone)]
pub struct TextureImport {
    image: ImageData,
    registration: Arc<Registration>,
}
impl TextureImport {
    /// Returns the image while its shared registration remains live.
    ///
    /// The host must coordinate image use with unregister on its render thread.
    /// The returned image and its clones lose validity when any guard unregisters.
    pub fn image(&self) -> Option<&ImageData> {
        self.registration
            .0
            .lock()
            .expect("texture registration poisoned")
            .is_some()
            .then_some(&self.image)
    }

    /// Removes the original renderer's override and invalidates every import clone.
    ///
    /// Returns whether this call retired a live registration. The host must pass
    /// the same renderer used for registration, after its final sampling
    /// submission. Source allocation retirement is fenced behind submitted GPU
    /// work without blocking this call.
    pub fn unregister(&self, renderer: &mut vello::Renderer) -> bool {
        let mut allocation = self
            .registration
            .0
            .lock()
            .expect("texture registration poisoned");
        if allocation.is_none() {
            return false;
        }
        renderer.unregister_texture(self.image.clone());
        let allocation = allocation.take().expect("live texture registration");
        let queue = allocation.queue.clone();
        queue.on_submitted_work_done(move || drop(allocation));
        true
    }
}
impl fmt::Debug for TextureImport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureImport")
            .field("registered", &self.image().is_some())
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for Texture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Texture")
            .field("dimensions", &self.dimensions())
            .finish_non_exhaustive()
    }
}

/// One completed job and the exact immutable texture it produced.
#[derive(Clone, Debug)]
pub struct Completed {
    /// Identity returned by the corresponding start call.
    pub job: JobId,
    /// Checked output ready for same-device sampling.
    pub texture: Texture,
    /// Whether this job rerendered drawings rather than reusing checked pixels.
    pub redrawn: bool,
}

/// A worker-owned checked renderer with one pending job and bounded exports.
///
/// The validation future is not `Send`; construct this on its render worker.
/// Cancellation never waits for or destroys the host's shared device. A retiring
/// GPU fence holds credit and prevents working-target replacement until polled.
pub struct TextureRenderer {
    shared: SharedDevice,
    pool: TexturePool,
    worker: Option<pool::WorkerLease>,
    cache: Cache,
    renderer: Option<vivid_vello_backend::Renderer>,
    pending: Option<worker::Work>,
    retiring: Option<std::sync::mpsc::Receiver<()>>,
    last: Weak<Allocation>,
    metrics: RenderMetrics,
    failed: bool,
}
impl TextureRenderer {
    /// Creates a checked renderer without creating or taking over a device.
    ///
    /// # Errors
    /// Returns an already-failed host device or backend initialization error.
    /// Returns `WouldBlock` while the pool's previous worker is live or retiring.
    pub fn new(shared: &SharedDevice, pool: &TexturePool) -> io::Result<Self> {
        if (shared.failed)() {
            return Err(unavailable());
        }
        let worker = pool.claim_worker()?;
        let renderer = vivid_vello_backend::Renderer::new(
            &shared.device,
            vivid_vello_backend::RendererOptions::default(),
        )
        .map_err(io::Error::other)?;
        Ok(Self {
            shared: shared.clone(),
            pool: pool.clone(),
            worker: Some(worker),
            cache: Cache::default(),
            renderer: Some(renderer),
            pending: None,
            retiring: None,
            last: Weak::new(),
            metrics: RenderMetrics::default(),
            failed: false,
        })
    }

    /// Starts one checked raster-under-vector texture job.
    ///
    /// Retain the exact prepared snapshot under the returned identity. Even a
    /// cache hit gets a fresh job identity; display admission remains the host's
    /// responsibility. Coalesce later candidates while this renderer is busy.
    ///
    /// # Errors
    /// Returns `WouldBlock` for pending/retiring work or a full output pool,
    /// rejects invalid geometry/resources, or reports device/backend failure.
    pub fn start(
        &mut self,
        drawings: &[Drawing],
        rasters: &[RasterDrawing],
        width: u32,
        height: u32,
    ) -> io::Result<JobId> {
        self.start_job(drawings, rasters, width, height, None)
    }

    /// Starts checked texture output within a shared tile-dispatch budget.
    ///
    /// Each actual checked tile attempt claims a credit from `budget`, including
    /// subdivision retries. Cache reuse claims no credits. The ordinary per-job
    /// limits and deadlines still apply. Clone the same budget across cooperating
    /// renderers; exhaustion returns [`super::Error::Cancelled`] from `poll`,
    /// publishes no output, and grants no source-fault evidence. The budget does
    /// not coordinate job polling, output pools, device ownership or admission.
    ///
    /// # Errors
    /// Returns the same admission errors as [`Self::start`]. Checked rendering and
    /// shared-budget exhaustion are reported asynchronously by [`Self::poll`].
    pub fn start_budgeted(
        &mut self,
        drawings: &[Drawing],
        rasters: &[RasterDrawing],
        width: u32,
        height: u32,
        budget: &WorkBudget,
    ) -> io::Result<JobId> {
        self.start_job(drawings, rasters, width, height, Some(budget.clone()))
    }

    /// Polls validation and the output-copy fence without waiting for the GPU.
    ///
    /// # Errors
    /// Reports checked allocation/work refusal without an image. Device errors
    /// or a ten-second job timeout require replacement using the same pool.
    pub fn poll(&mut self) -> io::Result<Option<Completed>> {
        self.poll_job()
    }

    /// Cancels pending output and asynchronously retires its working resources.
    ///
    /// Keep polling while retiring; new work returns `WouldBlock` until its
    /// queue fence completes. Existing exported textures remain immutable.
    pub fn cancel(&mut self) {
        self.cancel_job();
    }

    /// Reports pending output or GPU retirement requiring further polling.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some() || self.retiring.is_some()
    }

    /// Reports host device failure or a local renderer failure requiring replacement.
    pub fn is_failed(&self) -> bool {
        self.failed || (self.shared.failed)()
    }

    /// Returns reported checked render passes, retries and cache reuse counters.
    ///
    /// Cancelling an incomplete job retains its previous reported counters;
    /// attempts in the discarded future are not included.
    pub fn metrics(&self) -> RenderMetrics {
        if self.pending.as_ref().is_some_and(|p| p.is_rendering()) {
            self.metrics
        } else {
            self.cache.metrics()
        }
    }

    /// Expires a pending job without waiting ten seconds.
    #[cfg(feature = "testing")]
    pub fn expire_pending_for_test(&mut self) {
        if let Some(work) = &mut self.pending {
            work.started = std::time::Instant::now() - worker::TIMEOUT;
        }
    }
}
impl Drop for TextureRenderer {
    fn drop(&mut self) {
        self.cancel_job();
        if let Some(worker) = self.worker.take() {
            self.shared
                .queue
                .on_submitted_work_done(move || drop(worker));
        }
    }
}
impl fmt::Debug for TextureRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureRenderer")
            .field("busy", &self.is_busy())
            .field("reserved", &self.pool.reserved())
            .field("metrics", &self.metrics())
            .finish_non_exhaustive()
    }
}

fn unavailable() -> io::Error {
    io::Error::other("shared native vector GPU is unavailable")
}
