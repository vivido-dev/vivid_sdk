//! Cached vector overlay composition with a host-owned Vello device and renderer.
//!
//! This module creates no event loop, GPU adapter, Vivid session or input authority.
//! The host supplies back-to-front immutable drawings, owns resource quotas, and
//! acknowledges submissions only after the returned image was actually composed.
//! The cache borrows its renderer per operation; call `clear` before replacing or
//! dropping that renderer so registered textures are unregistered there.

// Rust guideline compliant 2026-10-07
#[cfg(feature = "vector-offscreen")]
#[path = "offscreen.rs"]
pub mod offscreen;

use super::{CompiledScene, RasterDrawing};
use std::sync::Arc;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill, ImageAlphaType, ImageData, Mix};
use vello::{AaConfig, RenderParams, Renderer, Scene};

#[doc(inline)]
pub use super::Drawing;

/// Cumulative work performed by the pane-overlay render cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderMetrics {
    /// Successfully submitted GPU render passes.
    pub render_passes: u64,
    /// Calls that reused unchanged cached output.
    pub skipped_passes: u64,
    /// Registered GPU target allocations.
    pub target_allocations: u64,
}

/// A bounded GPU overlay target and immutable-drawing cache.
#[derive(Default)]
pub struct Compositor {
    target: Option<Target>,
    key: Vec<(
        vivid_protocol::identity::SurfaceIdentity,
        u64,
        vivid_protocol::overlay::wire::Submission,
        vivid_protocol::vector::Rect,
        u64,
    )>,
    generation: u64,
    scenes: Vec<Arc<CompiledScene>>,
    rasters: Vec<RasterDrawing>,
    metrics: RenderMetrics,
}
struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    image: ImageData,
    width: u32,
    height: u32,
}
impl Compositor {
    /// Returns the current texture-cache generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns cumulative composition and allocation counters.
    pub fn metrics(&self) -> RenderMetrics {
        self.metrics
    }

    /// Unregisters the cached texture from the renderer that created it.
    ///
    /// # Panics
    /// Panics if the cache generation counter is exhausted.
    pub fn clear(&mut self, renderer: &mut Renderer) {
        if let Some(target) = self.target.take() {
            renderer.unregister_texture(target.image);
        }
        self.key.clear();
        self.scenes.clear();
        self.rasters.clear();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("overlay cache generation exhausted");
    }

    /// Composes changed drawings into a bounded premultiplied RGBA texture.
    ///
    /// Unchanged identities, revisions, bounds, scale and target dimensions reuse
    /// the texture while their immutable compiled objects are unchanged. Replacing
    /// a compiled object redraws even when its numeric identities are reused.
    /// The boolean reports GPU work, not presentation or scanout.
    /// Use the same renderer and device until `clear` unregisters its target.
    ///
    /// # Errors
    /// Rejects invalid drawings or target limits before changing the cache.
    /// Returns renderer errors without publishing the failed key.
    ///
    /// # Panics
    /// Panics if the cache generation counter is exhausted.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        drawings: &[Drawing],
        width: u32,
        height: u32,
    ) -> Result<(Option<ImageData>, bool), Error> {
        self.render_mixed(device, queue, renderer, drawings, &[], width, height)
    }

    /// Composes scene-node raster layers beneath ordered vector windows.
    ///
    /// Raster layers arrive back to front and grant no input authority. Their
    /// fit, sampling, opacity and clips apply before vector windows are drawn.
    /// Hosts retain ownership of aggregate pixel resources and display receipts.
    ///
    /// # Errors
    /// Rejects invalid layers/targets before cache mutation, or returns GPU errors.
    ///
    /// # Panics
    /// Panics if the cache generation counter is exhausted.
    #[expect(
        clippy::too_many_arguments,
        reason = "extends the existing host-owned renderer interface"
    )]
    pub fn render_mixed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        drawings: &[Drawing],
        rasters: &[RasterDrawing],
        width: u32,
        height: u32,
    ) -> Result<(Option<ImageData>, bool), Error> {
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
        if drawings.is_empty() && rasters.is_empty() && self.target.is_none() {
            return Ok((None, false));
        }
        let new_target = self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if new_target {
            self.clear(renderer);
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
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let mut image = renderer.register_texture(texture.clone());
            image.alpha_type = ImageAlphaType::AlphaPremultiplied;
            self.target = Some(Target {
                _texture: texture,
                view,
                image,
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
        let target = self.target.as_ref().expect("overlay target allocated");
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
            let scene = compose(drawings, rasters, width, height);
            renderer
                .render_to_texture(
                    device,
                    queue,
                    &scene,
                    &target.view,
                    &RenderParams {
                        base_color: Color::TRANSPARENT,
                        width,
                        height,
                        antialiasing_method: AaConfig::Msaa8,
                    },
                )
                .map_err(Error::Render)?;
            renderer.mark_override_image_dirty(&target.image);
            self.key = key;
            // Keep the objects alive as long as their pointer identities are cached.
            self.scenes = drawings
                .iter()
                .map(|drawing| drawing.compiled.clone())
                .collect();
            self.rasters = rasters.to_vec();
            self.metrics.render_passes = self.metrics.render_passes.saturating_add(1);
            Ok((Some(target.image.clone()), true))
        } else {
            self.metrics.skipped_passes = self.metrics.skipped_passes.saturating_add(1);
            Ok((Some(target.image.clone()), false))
        }
    }
}

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
    /// The native renderer failed to produce the requested frame.
    Render(vello::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => f.write_str(message),
            Self::Render(error) => write!(f, "vector composition failed: {error}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Render(error) => Some(error),
            _ => None,
        }
    }
}
impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("generation", &self.generation)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

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
