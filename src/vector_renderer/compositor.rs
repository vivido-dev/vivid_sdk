//! Checked vector output and shared bounded drawing validation.
//!
//! This module creates no event loop, Vivid session or input authority.
//! With `vector-texture`, `texture::TextureRenderer` owns its checked worker
//! renderer and exports immutable images on the host's shared device. The host
//! retains exact prepared drawings until completion and acknowledges only the
//! imported image actually composed. `vector-offscreen` adds checked CPU readback.
//! Working targets and cache mutation remain private to these checked backends.

// Rust guideline compliant 2026-10-07
#[cfg(feature = "vector-texture")]
mod budget;
#[cfg(feature = "vector-texture")]
mod checked;
#[cfg(feature = "vector-offscreen")]
#[path = "offscreen.rs"]
pub mod offscreen;
#[cfg(feature = "vector-texture")]
pub mod texture;
#[cfg(feature = "vector-texture")]
mod tiles;
#[cfg(feature = "vector-texture")]
#[doc(inline)]
pub use budget::WorkBudget;

#[cfg(any(feature = "vector-texture", test))]
use super::CompiledScene;
#[cfg(feature = "vector-texture")]
use super::RasterDrawing;
#[cfg(any(feature = "vector-texture", test))]
use std::sync::Arc;
#[cfg(any(feature = "vector-texture", test))]
use vello::Scene;
#[cfg(any(feature = "vector-texture", test))]
use vello::kurbo::{Affine, Rect};
#[cfg(feature = "vector-texture")]
use vello::peniko::{Color, Fill, Mix};
#[cfg(feature = "vector-texture")]
use vello::{AaConfig, RenderParams};

#[doc(inline)]
pub use super::Drawing;

/// Cumulative work performed by the pane-overlay render cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderMetrics {
    /// Submitted tile render attempts, including allocation retries and failed attempts.
    pub render_passes: u64,
    /// Calls that reused unchanged cached output.
    pub skipped_passes: u64,
    /// Private working-target allocations.
    pub target_allocations: u64,
    /// Area-sensitive arena failures retried with smaller checked tiles.
    pub allocation_retries: u64,
}

/// Working resources owned exclusively by a checked output renderer.
#[cfg(feature = "vector-texture")]
#[derive(Default)]
struct Cache {
    target: Option<Target>,
    scratch: Option<tiles::Scratch>,
    key: Vec<(
        vivid_protocol::identity::SurfaceIdentity,
        u64,
        vivid_protocol::overlay::wire::Submission,
        vivid_protocol::vector::Rect,
        u64,
    )>,
    scenes: Vec<Arc<CompiledScene>>,
    rasters: Vec<RasterDrawing>,
    metrics: RenderMetrics,
}
#[cfg(feature = "vector-texture")]
struct Target {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}
#[cfg(feature = "vector-texture")]
struct Plan {
    scene: Scene,
    key: Vec<(
        vivid_protocol::identity::SurfaceIdentity,
        u64,
        vivid_protocol::overlay::wire::Submission,
        vivid_protocol::vector::Rect,
        u64,
    )>,
    drawings: Vec<Drawing>,
    rasters: Vec<RasterDrawing>,
    width: u32,
    height: u32,
}
#[cfg(feature = "vector-texture")]
impl Plan {
    fn params(&self) -> RenderParams {
        RenderParams {
            base_color: Color::TRANSPARENT,
            width: self.width,
            height: self.height,
            antialiasing_method: AaConfig::Msaa8,
        }
    }
}

#[cfg(feature = "vector-texture")]
impl Cache {
    /// Returns cumulative composition and allocation counters.
    fn metrics(&self) -> RenderMetrics {
        self.metrics
    }

    fn clear(&mut self) {
        self.target = None;
        self.scratch = None;
        self.key.clear();
        self.scenes.clear();
        self.rasters.clear();
    }

    fn prepare(
        &mut self,
        device: &wgpu::Device,
        drawings: &[Drawing],
        rasters: &[RasterDrawing],
        width: u32,
        height: u32,
    ) -> Result<Option<Plan>, Error> {
        validate(
            drawings,
            width,
            height,
            device.limits().max_texture_dimension_2d,
        )?;
        validate_rasters(rasters)?;
        let overhead = rasters
            .len()
            .checked_mul(1024)
            .and_then(|n| n.checked_add(1024));
        let encoded = drawings.iter().fold(overhead, |total, d| {
            total
                .and_then(|n| n.checked_add(d.compiled.encoded_size()))
                .and_then(|n| n.checked_add(1024))
        });
        if encoded.is_none_or(|n| n > super::MAX_COMPILED_BYTES) {
            return Err(Error::InvalidInput(
                "mixed composition encoding exceeds byte budget",
            ));
        }
        let new_target = self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if new_target {
            self.clear();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vivid.vector.overlay.target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.target = Some(Target {
                texture,
                width,
                height,
            });
            self.metrics.target_allocations = self.metrics.target_allocations.saturating_add(1);
        }
        let key: Vec<_> = drawings
            .iter()
            .map(|d| {
                (
                    d.window,
                    d.window_revision,
                    d.submission,
                    d.bounds,
                    d.scale.to_bits(),
                )
            })
            .collect();
        let same_scenes = self.scenes.len() == drawings.len()
            && self
                .scenes
                .iter()
                .zip(drawings)
                .all(|(scene, drawing)| Arc::ptr_eq(scene, &drawing.compiled));
        let same_rasters = self.rasters.len() == rasters.len()
            && self
                .rasters
                .iter()
                .zip(rasters)
                .all(|(a, b)| a.same_content(b));
        if new_target || self.key != key || !same_scenes || !same_rasters {
            Ok(Some(Plan {
                scene: compose(drawings, rasters, width, height),
                key,
                drawings: drawings.to_vec(),
                rasters: rasters.to_vec(),
                width,
                height,
            }))
        } else {
            self.metrics.skipped_passes = self.metrics.skipped_passes.saturating_add(1);
            Ok(None)
        }
    }
    fn commit(&mut self, plan: Plan) {
        self.key = plan.key;
        self.scenes = plan
            .drawings
            .into_iter()
            .map(|drawing| drawing.compiled)
            .collect();
        self.rasters = plan.rasters;
    }

    async fn checked_with_budget(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut vivid_vello_backend::Renderer,
        plan: Plan,
        cancelled: &std::sync::atomic::AtomicBool,
        budget: Option<&WorkBudget>,
    ) -> Result<(), Error> {
        let target = self
            .target
            .as_ref()
            .expect("prepared checked overlay target");
        let scratch = self
            .scratch
            .get_or_insert_with(|| tiles::Scratch::new(device, plan.width, plan.height));
        let result = checked::render(
            device,
            queue,
            renderer,
            &plan.scene,
            scratch,
            &target.texture,
            &plan.params(),
            &mut self.metrics,
            cancelled,
            budget,
        )
        .await;
        match result {
            Ok(()) => {
                self.commit(plan);
                Ok(())
            }
            Err(error) => {
                // Retire partial pixels as well as their key. Merely clearing the
                // key would make a later empty composition appear unchanged.
                self.clear();
                Err(error)
            }
        }
    }
}

#[cfg(feature = "vector-texture")]
fn compose(drawings: &[Drawing], rasters: &[RasterDrawing], width: u32, height: u32) -> Scene {
    let mut scene = Scene::new();
    scene.push_layer(
        Fill::NonZero,
        Mix::Normal,
        1.,
        Affine::IDENTITY,
        &Rect::new(0., 0., f64::from(width), f64::from(height)),
    );
    for drawing in rasters {
        let b = drawing.bounds;
        let pane = Affine::scale(drawing.scale);
        let bounds = Rect::new(
            b.origin.x.get(),
            b.origin.y.get(),
            b.origin.x.get() + b.width.get(),
            b.origin.y.get() + b.height.get(),
        );
        scene.push_layer(
            Fill::NonZero,
            Mix::Normal,
            f32::from(drawing.opacity) / f32::from(u16::MAX),
            pane,
            &bounds,
        );
        if let Some(clip) = drawing.clip {
            scene.push_layer(
                Fill::NonZero,
                Mix::Normal,
                1.,
                pane,
                &Rect::new(
                    clip.origin.x.get(),
                    clip.origin.y.get(),
                    clip.origin.x.get() + clip.width.get(),
                    clip.origin.y.get() + clip.height.get(),
                ),
            );
        }
        let image = &drawing.image.data;
        let transform = drawing.transform();
        let quality = if drawing.linear_sampling {
            vello::peniko::ImageQuality::Medium
        } else {
            vello::peniko::ImageQuality::Low
        };
        scene.draw_image(
            &vello::peniko::ImageBrush::new(image.clone()).with_quality(quality),
            transform,
        );
        if drawing.clip.is_some() {
            scene.pop_layer();
        }
        scene.pop_layer();
    }
    for drawing in drawings {
        let b = drawing.bounds;
        let transform =
            Affine::scale(drawing.scale) * Affine::translate((b.origin.x.get(), b.origin.y.get()));
        scene.push_layer(
            Fill::NonZero,
            Mix::Normal,
            1.,
            transform,
            &Rect::new(0., 0., b.width.get(), b.height.get()),
        );
        scene.append(&drawing.compiled.scene, Some(transform));
        scene.pop_layer();
    }
    scene.pop_layer();
    scene
}

#[cfg(feature = "vector-texture")]
fn validate_rasters(rasters: &[RasterDrawing]) -> Result<(), Error> {
    if rasters.len() > vivid_protocol::overlay::MAX_WINDOWS {
        return Err(Error::InvalidInput(
            "native raster layer count exceeds limits",
        ));
    }
    let mut nodes = std::collections::BTreeSet::new();
    let mut images = std::collections::BTreeSet::new();
    let mut bytes = 0usize;
    for drawing in rasters {
        if images.insert(Arc::as_ptr(&drawing.image)) {
            bytes = bytes
                .checked_add(drawing.image.data.data.len())
                .filter(|n| *n <= super::MAX_COMPILED_BYTES)
                .ok_or(Error::InvalidInput(
                    "native raster layers exceed byte budget",
                ))?;
        }
        drawing
            .validate()
            .map_err(|_| Error::InvalidInput("invalid native raster placement"))?;
        if !nodes.insert((
            drawing.surface.context().session(),
            drawing.node_context,
            drawing.node_id,
        )) {
            return Err(Error::InvalidInput("duplicate native raster node identity"));
        }
    }
    Ok(())
}

/// The target matches Vivido's existing maximum screenshot allocation.
pub const MAX_TARGET_BYTES: u64 = 256 * 1024 * 1024;

/// A rejected composition request or native rendering failure.
#[derive(Debug)]
pub enum Error {
    /// Invalid target extent or drawing metadata.
    InvalidInput(&'static str),
    /// The private checked backend failed to produce the requested frame.
    Backend(Box<dyn std::error::Error + Send + Sync>),
    /// A native GPU allocation stage could not fit its bounded arena.
    Arena(u32),
    /// Checked subdivision reached its bounded per-frame GPU work limit.
    WorkLimit,
    /// The host cancelled an unpublished native GPU composition.
    Cancelled,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => f.write_str(message),
            Self::Backend(error) => write!(f, "checked vector composition failed: {error}"),
            Self::Arena(stages) => write!(f, "vector GPU allocation failed (stages {stages:#x})"),
            Self::WorkLimit => f.write_str("checked vector GPU work limit exceeded"),
            Self::Cancelled => f.write_str("checked vector composition cancelled"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
#[cfg(feature = "vector-texture")]
impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

#[cfg(any(feature = "vector-texture", test))]
fn validate(drawings: &[Drawing], width: u32, height: u32, limit: u32) -> Result<(), Error> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0
        || height == 0
        || width > limit
        || height > limit
        || bytes.is_none_or(|bytes| bytes > MAX_TARGET_BYTES)
        || drawings.len() > vivid_protocol::overlay::MAX_WINDOWS
    {
        return Err(Error::InvalidInput(
            "vector composition target or drawing count exceeds limits",
        ));
    }
    let mut windows = std::collections::BTreeSet::new();
    // Reserve clip/layer bookkeeping as well as the copied encoding streams.
    let mut bytes = 1024usize;
    for drawing in drawings {
        bytes = bytes
            .checked_add(drawing.compiled.encoded_size())
            .and_then(|bytes| bytes.checked_add(1024))
            .filter(|bytes| *bytes <= super::MAX_COMPILED_BYTES)
            .ok_or(Error::InvalidInput(
                "composed vector encoding exceeds byte budget",
            ))?;
        if !drawing.scale.is_finite()
            || drawing.scale <= 0.0
            || drawing.window_revision == 0
            || drawing.window.context().context_id() != drawing.submission.address.context_id
            || drawing.window.surface_id() != drawing.submission.address.surface_id
            || drawing.submission.validate().is_err()
            || !windows.insert(drawing.window)
        {
            return Err(Error::InvalidInput(
                "invalid vector drawing identity, revision or scale",
            ));
        }
        drawing
            .bounds
            .validate()
            .map_err(|_| Error::InvalidInput("invalid vector drawing bounds"))?;
        let bounds = drawing.bounds;
        super::check_bounds(
            Rect::new(0., 0., bounds.width.get(), bounds.height.get()),
            Affine::scale(drawing.scale)
                * Affine::translate((bounds.origin.x.get(), bounds.origin.y.get())),
        )
        .map_err(|_| Error::InvalidInput("scaled vector window exceeds coordinate bounds"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::identity::{PresenterInstanceId, SessionIdentity};
    use vivid_protocol::{overlay::wire::Submission, vector::Rect as LogicalRect};

    fn drawing(owner: u64) -> Drawing {
        Drawing {
            window: SessionIdentity::new(PresenterInstanceId([1; 16]), owner)
                .unwrap()
                .context(1)
                .unwrap()
                .surface(1)
                .unwrap(),
            window_revision: 1,
            bounds: LogicalRect::new(0., 0., 10., 10.).unwrap(),
            scale: 1.,
            compiled: Arc::new(CompiledScene {
                scene: Scene::new(),
                retained: Vec::new(),
                hits: Vec::new(),
                extra_bytes: 0,
            }),
            submission: Submission {
                address: vivid_protocol::overlay::wire::WindowAddress {
                    context_id: 1,
                    surface_id: 1,
                    generation: 1,
                },
                track_id: 1,
                channel_generation: 1,
                epoch: 1,
                revision: 1,
            },
        }
    }

    #[test]
    fn target_limits_fail_before_gpu_allocation() {
        assert!(validate(&[], 0, 1, 16384).is_err());
        assert!(validate(&[], 1, 0, 16384).is_err());
        assert!(validate(&[], 16385, 1, 16384).is_err());
        assert!(validate(&[], u32::MAX, u32::MAX, u32::MAX).is_err());
        assert!(validate(&[], 8192, 8192, 16384).is_ok());
        assert!(validate(&[], 8192, 8193, 16384).is_err());
    }

    #[test]
    fn drawings_validate_complete_owner_keys_and_geometry_before_publication() {
        assert!(validate(&[drawing(1), drawing(2)], 32, 32, 16384).is_ok());
        assert!(validate(&[drawing(1), drawing(1)], 32, 32, 16384).is_err());
        for scale in [0., -1., f64::NAN, f64::INFINITY, 1_000_000.] {
            let mut invalid = drawing(1);
            invalid.scale = scale;
            assert!(validate(&[invalid], 32, 32, 16384).is_err());
        }
        let mut invalid = drawing(1);
        invalid.submission.address.surface_id = 2;
        assert!(validate(&[invalid], 32, 32, 16384).is_err());
        let mut invalid = drawing(1);
        invalid.submission.epoch = 0;
        assert!(validate(&[invalid], 32, 32, 16384).is_err());
    }
}
