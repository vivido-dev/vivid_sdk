//! Owned SHM readback and its bounded diagnostic counters.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{ExportMem as _, ImportMemWl as _, pixman::PixmanRenderer},
    },
    reexports::wayland_server::protocol::{wl_buffer, wl_shm},
    utils::{Buffer, Rectangle, Transform},
    wayland::{compositor::Damage, shm::with_buffer_contents},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub mapping: super::viewport::Mapping,
    stride: usize,
    offset: usize,
    format: wl_shm::Format,
}
impl Layout {
    pub(super) fn from_buffer(
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        buffer: &wl_buffer::WlBuffer,
        scale: i32,
        transform: Transform,
    ) -> io::Result<Self> {
        let (data, len) =
            with_buffer_contents(buffer, |_, len, data| (data, len)).map_err(io::Error::other)?;
        let width = u32::try_from(data.width).map_err(|_| invalid("invalid buffer width"))?;
        let height = u32::try_from(data.height).map_err(|_| invalid("invalid buffer height"))?;
        extent(width, height)?;
        let stride = usize::try_from(data.stride).map_err(|_| invalid("invalid SHM stride"))?;
        let offset = usize::try_from(data.offset).map_err(|_| invalid("invalid SHM offset"))?;
        let end = stride
            .checked_mul(height as usize)
            .and_then(|bytes| offset.checked_add(bytes));
        if scale < 1
            || scale > MAX_DIMENSION as i32
            || stride < width as usize * 4
            || len > MAX_SHM_POOL_BYTES
            || end
                .is_none_or(|end| end > len || end > MAX_SHM_POOL_BYTES || end > i32::MAX as usize)
        {
            return Err(invalid("SHM layout exceeds hosted bounds"));
        }
        Ok(Self {
            width,
            height,
            scale: scale as u32,
            mapping: super::viewport::Mapping::for_surface(
                surface,
                width,
                height,
                scale as u32,
                transform,
            )?,
            stride,
            offset,
            format: data.format,
        })
    }
    pub(super) fn bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
    pub(super) fn remap(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        scale: i32,
        transform: Transform,
    ) -> io::Result<()> {
        if scale < 1 || scale > MAX_DIMENSION as i32 {
            return Err(invalid("surface scale exceeds hosted bounds"));
        }
        let mapping = super::viewport::Mapping::for_surface(
            surface,
            self.width,
            self.height,
            scale as u32,
            transform,
        )?;
        self.scale = scale as u32;
        self.mapping = mapping;
        Ok(())
    }
}

fn damage_region(layout: Layout, damage: &[Damage]) -> io::Result<Option<Rectangle<i32, Buffer>>> {
    let mut bounds: Option<[i64; 4]> = None;
    for item in damage {
        let rect = match item {
            Damage::Surface(rect) => {
                if rect.size.w <= 0 || rect.size.h <= 0 {
                    continue;
                }
                let Some((left, right)) = layout.mapping.damage(
                    0,
                    i64::from(rect.loc.x),
                    i64::from(rect.loc.x) + i64::from(rect.size.w),
                ) else {
                    continue;
                };
                let Some((top, bottom)) = layout.mapping.damage(
                    1,
                    i64::from(rect.loc.y),
                    i64::from(rect.loc.y) + i64::from(rect.size.h),
                ) else {
                    continue;
                };
                let a = layout.mapping.raw_point(left, top);
                let b = layout.mapping.raw_point(right, bottom);
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                ]
            }
            Damage::Buffer(rect) => {
                if rect.size.w <= 0 || rect.size.h <= 0 {
                    continue;
                }
                [
                    i64::from(rect.loc.x),
                    i64::from(rect.loc.y),
                    i64::from(rect.loc.x) + i64::from(rect.size.w),
                    i64::from(rect.loc.y) + i64::from(rect.size.h),
                ]
            }
        };
        let [left, top, right, bottom] = [
            rect[0].clamp(0, i64::from(layout.width)),
            rect[1].clamp(0, i64::from(layout.height)),
            rect[2].clamp(0, i64::from(layout.width)),
            rect[3].clamp(0, i64::from(layout.height)),
        ];
        if left >= right || top >= bottom {
            continue;
        }
        bounds = Some(bounds.map_or([left, top, right, bottom], |old| {
            [
                old[0].min(left),
                old[1].min(top),
                old[2].max(right),
                old[3].max(bottom),
            ]
        }));
    }
    Ok(bounds.map(|[left, top, right, bottom]| {
        Rectangle::new(
            (left as i32, top as i32).into(),
            ((right - left) as i32, (bottom - top) as i32).into(),
        )
    }))
}

pub(super) fn capture(
    renderer: &mut PixmanRenderer,
    buffer: &wl_buffer::WlBuffer,
    layout: Layout,
    previous: Option<(Frame, Layout)>,
    damage: &[Damage],
    single_commit: bool,
    stats: &mut ReadbackStats,
) -> io::Result<Arc<[u8]>> {
    let full =
        Rectangle::<i32, Buffer>::from_size((layout.width as i32, layout.height as i32).into());
    // Pixels outside a viewport source are not surface content. A new buffer
    // may change those pixels without surface damage, then reveal them through
    // a later viewport-only commit. Preserve the complete attachment snapshot
    // before releasing cropped client buffers.
    let can_patch = single_commit
        && layout.mapping.is_full()
        && previous
            .as_ref()
            .is_some_and(|(frame, old)| *old == layout && frame.rgba.len() == layout.bytes());
    let region = if can_patch {
        damage_region(layout, damage)?
    } else {
        Some(full)
    };
    let Some(region) = region else {
        stats.reused_buffers = stats.reused_buffers.saturating_add(1);
        return Ok(previous.expect("validated previous poster").0.rgba);
    };
    let partial = can_patch && region != full;
    let texture = renderer
        .import_shm_buffer(buffer, None, &[region])
        .map_err(io::Error::other)?;
    // Pixman's owned mapping guards concurrent SHM truncation. No Rust slice
    // into the client's mutable mapping is created here.
    let mapping = renderer
        .copy_texture(&texture, region, Fourcc::Abgr8888)
        .map_err(io::Error::other)?;
    let pixels = renderer.map_texture(&mapping).map_err(io::Error::other)?;
    let row_bytes = region.size.w as usize * 4;
    let bytes = row_bytes
        .checked_mul(region.size.h as usize)
        .ok_or_else(|| invalid("readback size overflow"))?;
    if pixels.len() != bytes {
        return Err(invalid("unsupported Pixman row padding"));
    }
    let rgba = if partial {
        let mut rgba = previous.expect("validated patch base").0.rgba;
        let target = Arc::make_mut(&mut rgba);
        for row in 0..region.size.h as usize {
            let dst =
                ((region.loc.y as usize + row) * layout.width as usize + region.loc.x as usize) * 4;
            let source = row * row_bytes;
            target
                .get_mut(dst..dst + row_bytes)
                .ok_or_else(|| invalid("patch outside poster"))?
                .copy_from_slice(&pixels[source..source + row_bytes]);
        }
        stats.partial_reads = stats.partial_reads.saturating_add(1);
        rgba
    } else {
        stats.full_reads = stats.full_reads.saturating_add(1);
        pixels.into()
    };
    stats.pixel_bytes = stats.pixel_bytes.saturating_add(bytes as u64);
    Ok(rgba)
}

/// Completed native SHM pixel readback work.
///
/// Counts describe logical pixel bytes copied by the importer, not memory-bus
/// traffic, later scene composition, GPU uploads or child allocations. Diagnostic
/// counters saturate rather than wrapping and grant no runtime authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadbackStats {
    /// Full-surface pixel readbacks.
    pub full_reads: u64,
    /// Subrectangle pixel readbacks.
    pub partial_reads: u64,
    /// Attachments reusing an owned poster without reading SHM pixels.
    pub reused_buffers: u64,
    /// Logical RGBA pixel bytes copied from imported SHM.
    pub pixel_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            width: 64,
            height: 48,
            scale: 2,
            mapping: super::super::viewport::Mapping::new(
                64,
                48,
                2,
                Transform::Normal,
                Default::default(),
            )
            .unwrap(),
            stride: 256,
            offset: 0,
            format: wl_shm::Format::Argb8888,
        }
    }

    #[test]
    fn mixed_damage_spaces_clip_before_forming_one_bounding_rectangle() {
        let damage = [
            Damage::Surface(Rectangle::new((-2, -1).into(), (4, 3).into())),
            Damage::Buffer(Rectangle::new((60, 46).into(), (20, 20).into())),
        ];
        assert_eq!(
            damage_region(layout(), &damage).unwrap(),
            Some(Rectangle::from_size((64, 48).into()))
        );
        assert_eq!(
            damage_region(layout(), &damage[..1]).unwrap(),
            Some(Rectangle::from_size((4, 4).into()))
        );
    }

    #[test]
    fn empty_and_extreme_damage_never_wraps_into_visible_pixels() {
        let mut negative = Rectangle::from_size((1, 20).into());
        negative.size.w = -1;
        let damage = [
            Damage::Surface(Rectangle::new((i32::MAX, 0).into(), (i32::MAX, 1).into())),
            Damage::Surface(Rectangle::new((i32::MIN, 0).into(), (i32::MAX, 1).into())),
            Damage::Buffer(negative),
            Damage::Buffer(Rectangle::new((0, 0).into(), (20, 0).into())),
        ];
        assert!(damage_region(layout(), &damage).unwrap().is_none());
        assert!(damage_region(layout(), &[]).unwrap().is_none());
    }
}
