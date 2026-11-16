//! Immutable RGBA layers below native vector overlays.

// Rust guideline compliant 2026-10-07
use std::sync::Arc;
use vello::kurbo::{Affine, Rect as GpuRect};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use vivid_protocol::{identity::SurfaceIdentity, vector::Rect};

// Matches the native vector target's maximum axis; bytes have their own tighter cap.
const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Bounded RGBA8 pixels and alpha interpretation shared by immutable raster drawings.
pub struct RasterImage {
    pub(crate) data: ImageData,
    pub(crate) charge: Option<Arc<dyn Send + Sync>>,
}
impl RasterImage {
    /// Wraps bounded RGBA pixels without copying their allocation.
    ///
    /// Hosts own aggregate resource accounting; this enforces the per-image limit.
    ///
    /// # Errors
    /// Rejects empty/oversized dimensions and mismatched or overflowing byte lengths.
    pub fn new(
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
    ) -> Result<Self, super::wire::InvalidScene> {
        Self::from_buffer(width, height, Arc::new(rgba))
    }

    #[cfg(feature = "presenter")]
    pub(crate) fn new_premultiplied(
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
    ) -> Result<Self, super::wire::InvalidScene> {
        let mut image = Self::new(width, height, rgba)?;
        image.data.alpha_type = ImageAlphaType::AlphaPremultiplied;
        Ok(image)
    }

    /// Retains decoded video pixels without copying or losing their buffer pool.
    ///
    /// # Errors
    /// Rejects invalid dimensions or mismatched/oversized pixel storage.
    #[cfg(feature = "vector-video")]
    pub fn from_decoded(
        frame: crate::video::DecodedFrame,
    ) -> Result<Self, super::wire::InvalidScene> {
        Self::from_buffer(frame.width, frame.height, Arc::new(frame.rgba))
    }

    // Only immutable library-owned storage reaches this helper; arbitrary
    // external AsRef implementations cannot invalidate the checked byte length.
    fn from_buffer(
        width: u32,
        height: u32,
        rgba: Arc<dyn AsRef<[u8]> + Send + Sync>,
    ) -> Result<Self, super::wire::InvalidScene> {
        let length = rgba.as_ref().as_ref().len();
        let bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4));
        if width == 0
            || height == 0
            || width > MAX_IMAGE_DIMENSION
            || height > MAX_IMAGE_DIMENSION
            || bytes != Some(length as u64)
            || length > super::MAX_COMPILED_BYTES
        {
            return Err(super::wire::InvalidScene::new(
                "invalid native raster image",
            ));
        }
        Ok(Self {
            data: ImageData {
                width,
                height,
                data: Blob::new(rgba),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
            },
            charge: None,
        })
    }
    /// Returns the original pixel dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.data.width, self.data.height)
    }
    /// Retains the host's aggregate pixel-budget charge for this image's lifetime.
    ///
    /// # Errors
    /// Refuses a second charge token without replacing the original accounting.
    pub fn retain_resource<R: Send + Sync + 'static>(
        &mut self,
        token: Arc<R>,
    ) -> Result<(), super::wire::InvalidScene> {
        if self.charge.is_some() {
            return Err(super::wire::InvalidScene::new(
                "native raster image already charged",
            ));
        }
        self.charge = Some(token);
        Ok(())
    }
}
impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterImage")
            .field("dimensions", &self.dimensions())
            .finish_non_exhaustive()
    }
}

/// One retained scene-node image beneath the vector window plane.
#[derive(Clone, Debug)]
pub struct RasterDrawing {
    /// Complete owner-qualified surface identity; this grants no input authority.
    pub surface: SurfaceIdentity,
    /// Owning scene-node context, independent of the referenced surface context.
    pub node_context: u64,
    /// Node identifier within its owning context.
    pub node_id: u64,
    /// Pane-local logical destination rectangle.
    pub bounds: Rect,
    /// Optional additional pane-local logical clip.
    pub clip: Option<Rect>,
    /// Physical pixels per logical pixel.
    pub scale: f64,
    /// Image fitting within the destination rectangle.
    pub fit: crate::Fit,
    /// Linear sampling when true, nearest sampling otherwise.
    pub linear_sampling: bool,
    /// Normative node opacity, from zero through 65,535.
    pub opacity: u16,
    /// Immutable pixels and their retained resource charge.
    pub image: Arc<RasterImage>,
}
impl RasterDrawing {
    /// Validates identity, fitting and scaled geometry before renderer mutation.
    ///
    /// # Errors
    /// Rejects unknown fit modes, zero IDs, invalid scales or overflowing GPU bounds.
    pub fn validate(&self) -> Result<(), super::wire::InvalidScene> {
        if self.node_context == 0
            || self.node_id == 0
            || !self.scale.is_finite()
            || self.scale <= 0.
            || !matches!(
                self.fit,
                crate::Fit::Fill | crate::Fit::Contain | crate::Fit::Cover | crate::Fit::None
            )
        {
            return Err(super::wire::InvalidScene::new(
                "invalid native raster placement",
            ));
        }
        for b in std::iter::once(self.bounds).chain(self.clip) {
            b.validate()?;
            super::check_bounds(
                GpuRect::new(
                    b.origin.x.get(),
                    b.origin.y.get(),
                    b.origin.x.get() + b.width.get(),
                    b.origin.y.get() + b.height.get(),
                ),
                Affine::scale(self.scale),
            )?;
        }
        super::check_bounds(
            GpuRect::new(
                0.,
                0.,
                f64::from(self.image.data.width),
                f64::from(self.image.data.height),
            ),
            self.transform(),
        )
    }
    pub(crate) fn transform(&self) -> Affine {
        let b = self.bounds;
        let (iw, ih) = (
            f64::from(self.image.data.width),
            f64::from(self.image.data.height),
        );
        let (sx, sy) = (b.width.get() / iw, b.height.get() / ih);
        let (sx, sy) = match self.fit {
            crate::Fit::Contain => (sx.min(sy), sx.min(sy)),
            crate::Fit::Cover => (sx.max(sy), sx.max(sy)),
            crate::Fit::None => (1., 1.),
            _ => (sx, sy),
        };
        Affine::scale(self.scale)
            * Affine::translate((
                b.origin.x.get() + (b.width.get() - iw * sx) / 2.,
                b.origin.y.get() + (b.height.get() - ih * sy) / 2.,
            ))
            * Affine::scale_non_uniform(sx, sy)
    }
    /// Compares immutable pixels, ownership and complete placement metadata.
    pub fn same_content(&self, other: &Self) -> bool {
        self.surface == other.surface
            && self.node_context == other.node_context
            && self.node_id == other.node_id
            && self.bounds == other.bounds
            && self.clip == other.clip
            && self.scale == other.scale
            && self.fit == other.fit
            && self.linear_sampling == other.linear_sampling
            && self.opacity == other.opacity
            && Arc::ptr_eq(&self.image, &other.image)
    }
}
