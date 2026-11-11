//! Checked fixed-point viewport mapping for pixels, logical input and damage.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Buffer, Point, Size, Transform},
    wayland::{
        compositor::with_states,
        viewporter::{ViewportCachedState, ensure_viewport_valid_f64},
    },
};

// Wayland source rectangles use signed 24.8 fixed-point surface coordinates.
const FIXED: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Mapping {
    /// Source origin and size in post-transform physical 24.8 coordinates.
    pub source: [u64; 4],
    pub logical: [u32; 2],
    pub physical: [u32; 2],
    pub transform: Transform,
}
impl Mapping {
    pub(super) fn for_surface(
        surface: &WlSurface,
        width: u32,
        height: u32,
        scale: u32,
        transform: Transform,
    ) -> io::Result<Self> {
        with_states(surface, |states| {
            let size: Size<u32, Buffer> = transform.transform_size((width, height).into());
            if !ensure_viewport_valid_f64(
                states,
                (
                    f64::from(size.w) / f64::from(scale),
                    f64::from(size.h) / f64::from(scale),
                )
                    .into(),
            ) {
                return Err(invalid("viewport source is outside its buffer"));
            }
            let viewport = *states.cached_state.get::<ViewportCachedState>().current();
            Self::new(width, height, scale, transform, viewport)
        })
    }
    pub(super) fn new(
        width: u32,
        height: u32,
        scale: u32,
        transform: Transform,
        viewport: ViewportCachedState,
    ) -> io::Result<Self> {
        extent(width, height)?;
        let size: Size<u32, Buffer> = transform.transform_size((width, height).into());
        let (width, height) = (size.w, size.h);
        if !(1..=MAX_DIMENSION).contains(&scale) {
            return Err(invalid("surface scale exceeds hosted bounds"));
        }
        let source = if let Some(src) = viewport.src {
            let mut source = [0; 4];
            for (out, value) in source
                .iter_mut()
                .zip([src.loc.x, src.loc.y, src.size.w, src.size.h])
            {
                let fixed = value * FIXED as f64;
                if !fixed.is_finite()
                    || fixed < 0.
                    || fixed > f64::from(MAX_DIMENSION) * FIXED as f64
                    || fixed.fract() != 0.
                {
                    return Err(invalid("invalid viewport source"));
                }
                *out = (fixed as u64)
                    .checked_mul(u64::from(scale))
                    .ok_or_else(|| invalid("viewport source overflow"))?;
            }
            source
        } else {
            [0, 0, u64::from(width) * FIXED, u64::from(height) * FIXED]
        };
        if source[2] == 0
            || source[3] == 0
            || source[0]
                .checked_add(source[2])
                .is_none_or(|v| v > u64::from(width) * FIXED)
            || source[1]
                .checked_add(source[3])
                .is_none_or(|v| v > u64::from(height) * FIXED)
        {
            return Err(invalid("viewport source exceeds buffer"));
        }
        let logical = if let Some(dst) = viewport.dst {
            [
                u32::try_from(dst.w).map_err(|_| invalid("invalid viewport width"))?,
                u32::try_from(dst.h).map_err(|_| invalid("invalid viewport height"))?,
            ]
        } else {
            let denominator = FIXED * u64::from(scale);
            if !source[2].is_multiple_of(denominator) || !source[3].is_multiple_of(denominator) {
                return Err(invalid("viewport requires an integer destination"));
            }
            [
                (source[2] / denominator) as u32,
                (source[3] / denominator) as u32,
            ]
        };
        extent(logical[0], logical[1])?;
        Ok(Self {
            source,
            logical,
            physical: [width, height],
            transform,
        })
    }
    pub(super) fn is_full(self) -> bool {
        let [width, height] = self.physical;
        self.source == [0, 0, u64::from(width) * FIXED, u64::from(height) * FIXED]
    }
    pub(super) fn can_reuse_pixels(self) -> bool {
        self.transform == Transform::Normal && self.is_full()
    }
    /// Maps post-transform edges to raw buffer edges.
    ///
    /// The client has already applied its declared buffer transform. Mapping
    /// surface coordinates to that buffer therefore uses the declared transform;
    /// producing the displayed image samples its inverse implicitly.
    pub(super) fn raw_point(self, x: i64, y: i64) -> [i64; 2] {
        let point: Point<i64, Buffer> = self.transform.transform_point_in(
            (x, y).into(),
            &(i64::from(self.physical[0]), i64::from(self.physical[1])).into(),
        );
        [point.x, point.y]
    }
    /// Maps a pixel center without reflecting an edge beyond the buffer.
    pub(super) fn raw_pixel(self, x: u64, y: u64) -> [u64; 2] {
        let point: Point<i64, Buffer> = self.transform.transform_point_in(
            (2 * x as i64 + 1, 2 * y as i64 + 1).into(),
            &(
                2 * i64::from(self.physical[0]),
                2 * i64::from(self.physical[1]),
            )
                .into(),
        );
        [(point.x / 2) as u64, (point.y / 2) as u64]
    }
    /// Maps a clipped surface damage interval back to a physical buffer interval.
    pub(super) fn damage(self, axis: usize, start: i64, end: i64) -> Option<(i64, i64)> {
        let extent = i64::from(self.logical[axis]);
        let start = start.clamp(0, extent) as u64;
        let end = end.clamp(0, extent) as u64;
        if start >= end {
            return None;
        }
        let logical = u64::from(self.logical[axis]);
        let origin = self.source[axis] * logical;
        let length = self.source[axis + 2];
        let denominator = logical * FIXED;
        // The validated source/destination bounds keep each product below 2^41.
        Some((
            ((origin + start * length) / denominator) as i64,
            (origin + end * length).div_ceil(denominator) as i64,
        ))
    }
    /// Samples one axis without rounding an intermediate logical coordinate.
    pub(super) fn sample(self, axis: usize, position: u64, canvas: u32) -> u64 {
        let denominator = u64::from(canvas) * u64::from(self.logical[axis]);
        (self.source[axis] * denominator + position * self.source[axis + 2]) / (denominator * FIXED)
    }
}
