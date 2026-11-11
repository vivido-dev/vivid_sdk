//! Owned surface pixels, bounded tree composition, and matching input regions.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    reexports::wayland_server::{
        backend::ObjectId,
        protocol::{wl_callback::WlCallback, wl_surface::WlSurface},
    },
    wayland::compositor::{
        RectangleKind, RegionAttributes, SubsurfaceCachedState, SurfaceAttributes, TraversalAction,
        with_surface_tree_upward,
    },
};
use std::{cell::Cell, collections::HashMap};

// Bound retained decoded surfaces independently of the composed 16-MiB canvas.
const MAX_RETAINED: usize = 64 * 1024 * 1024;
const MAX_CALLBACKS: usize = 256;
const MAX_SURFACE_CALLBACKS: usize = MAX_PENDING_CALLBACKS;
const MAX_TREE_DEPTH: u8 = 8;
const MAX_OFFSET: i32 = 8192;
const MAX_REGION_RECTS: usize = MAX_REGION_OPERATIONS;

#[derive(Default)]
struct Image {
    frame: Option<Frame>,
    layout: Option<super::readback::Layout>,
    commits: u8,
    callbacks: Vec<WlCallback>,
}
#[derive(Clone, PartialEq)]
struct Placement {
    id: ObjectId,
    origin: [i32; 2],
    logical: [u32; 2],
    physical: [u32; 2],
    mapping: super::viewport::Mapping,
    // None means the entire surface; an empty region accepts no input.
    region: Option<Vec<[i32; 5]>>,
}
struct Layer {
    surface: WlSurface,
    frame: Frame,
    placement: Placement,
}
struct HitLayer {
    surface: WlSurface,
    placement: Placement,
}

#[derive(Default)]
pub(super) struct Scene {
    images: HashMap<ObjectId, Image>,
    layers: Vec<HitLayer>,
    previous: Option<Vec<Placement>>,
    retained: usize,
}
impl Scene {
    pub(super) fn origin(&self, surface: &WlSurface) -> Option<[i32; 2]> {
        self.layers
            .iter()
            .find(|layer| layer.surface == *surface)
            .map(|layer| layer.placement.origin)
    }
    pub(super) fn replacement_fits(&self, surface: &WlSurface, bytes: usize) -> bool {
        let old = self
            .images
            .get(&surface.id())
            .and_then(|image| image.frame.as_ref())
            .map_or(0, |frame| frame.rgba.len());
        self.retained
            .checked_sub(old)
            .and_then(|sum| sum.checked_add(bytes))
            .is_some_and(|sum| sum <= MAX_RETAINED)
    }
    pub(super) fn frame(&self, surface: &WlSurface) -> Option<&Frame> {
        self.images.get(&surface.id())?.frame.as_ref()
    }
    pub(super) fn window_geometry(
        &self,
        surface: &WlSurface,
    ) -> io::Result<smithay::utils::Rectangle<i32, smithay::utils::Logical>> {
        use smithay::wayland::{compositor::with_states, shell::xdg::SurfaceCachedState};
        if self.frame(surface).is_none() {
            return Err(invalid("unmapped popup parent"));
        }
        let mut layers = Vec::new();
        self.collect(surface, [0, 0], None, &mut layers)?;
        let mut bounds = None;
        for layer in layers {
            let rectangle = smithay::utils::Rectangle::new(
                (layer.placement.origin[0], layer.placement.origin[1]).into(),
                (
                    layer.placement.logical[0] as i32,
                    layer.placement.logical[1] as i32,
                )
                    .into(),
            );
            bounds = Some(bounds.map_or(
                rectangle,
                |bounds: smithay::utils::Rectangle<i32, smithay::utils::Logical>| {
                    bounds.merge(rectangle)
                },
            ));
        }
        let bounds = bounds.ok_or_else(|| invalid("empty surface tree geometry"))?;
        let geometry = with_states(surface, |states| {
            states
                .cached_state
                .get::<SurfaceCachedState>()
                .current()
                .geometry
        });
        let Some(geometry) = geometry else {
            return Ok(bounds);
        };
        if geometry.loc.x.unsigned_abs() > MAX_OFFSET as u32
            || geometry.loc.y.unsigned_abs() > MAX_OFFSET as u32
            || geometry.size.w <= 0
            || geometry.size.h <= 0
            || geometry.size.w > MAX_OFFSET
            || geometry.size.h > MAX_OFFSET
        {
            return Err(invalid("window geometry exceeds hosted bounds"));
        }
        geometry
            .intersection(bounds)
            .ok_or_else(|| invalid("empty window geometry"))
    }
    pub(super) fn has_tree(&self, root: &WlSurface) -> bool {
        self.layers
            .iter()
            .any(|layer| belongs_to(&layer.surface, root))
    }
    pub(super) fn note_commit(&mut self, surface: &WlSurface) {
        let image = self.images.entry(surface.id()).or_default();
        image.commits = image.commits.saturating_add(1).min(2);
    }
    pub(super) fn single_commit(&mut self, surface: &WlSurface) -> bool {
        self.images
            .get_mut(&surface.id())
            .is_some_and(|image| std::mem::take(&mut image.commits) == 1)
    }
    pub(super) fn take_poster(
        &mut self,
        surface: &WlSurface,
    ) -> Option<(Frame, super::readback::Layout)> {
        let image = self.images.get_mut(&surface.id())?;
        let frame = image.frame.take()?;
        self.retained -= frame.rgba.len();
        Some((frame, image.layout.take().expect("imported poster layout")))
    }
    pub(super) fn remap(
        &mut self,
        surface: &WlSurface,
        scale: i32,
        transform: smithay::utils::Transform,
    ) -> io::Result<()> {
        if let Some(image) = self.images.get_mut(&surface.id())
            && let Some(frame) = &mut image.frame
        {
            let layout = image.layout.as_mut().expect("imported poster layout");
            layout.remap(surface, scale, transform)?;
            frame.logical_size = layout.mapping.logical;
        }
        Ok(())
    }
    pub(super) fn update(
        &mut self,
        surface: &WlSurface,
        frame: Option<Frame>,
        layout: Option<super::readback::Layout>,
    ) {
        let image = self.images.entry(surface.id()).or_default();
        let old = image.frame.as_ref().map_or(0, |frame| frame.rgba.len());
        let new = frame.as_ref().map_or(0, |frame| frame.rgba.len());
        self.retained = self
            .retained
            .checked_sub(old)
            .and_then(|sum| sum.checked_add(new))
            .expect("validated retained pixel charge");
        image.frame = frame;
        image.layout = layout;
        if image.frame.is_none() {
            image.callbacks.clear();
        }
    }
    pub(super) fn callbacks(
        &mut self,
        surface: &WlSurface,
        callbacks: Vec<WlCallback>,
    ) -> io::Result<()> {
        let total = self
            .images
            .values()
            .map(|image| image.callbacks.len())
            .sum::<usize>();
        let surface_total = self
            .images
            .get(&surface.id())
            .map_or(0, |image| image.callbacks.len())
            .checked_add(callbacks.len());
        if surface_total.is_none_or(|count| count > MAX_SURFACE_CALLBACKS)
            || total
                .checked_add(callbacks.len())
                .is_none_or(|total| total > MAX_CALLBACKS)
        {
            return Err(invalid("hosted frame callback limit exceeded"));
        }
        self.images
            .entry(surface.id())
            .or_default()
            .callbacks
            .extend(callbacks);
        Ok(())
    }
    pub(super) fn remove(&mut self, surface: &WlSurface) {
        if let Some(image) = self.images.remove(&surface.id()) {
            self.retained -= image.frame.map_or(0, |frame| frame.rgba.len());
        }
    }
    pub(super) fn clear_rendered(&mut self) {
        self.layers.clear();
        self.previous = None;
    }
    pub(super) fn take_callbacks(&mut self, cursor: Option<&WlSurface>) -> Vec<WlCallback> {
        let mut callbacks = Vec::new();
        for layer in &self.layers {
            if let Some(image) = self.images.get_mut(&layer.surface.id()) {
                callbacks.append(&mut image.callbacks);
            }
        }
        if let Some(surface) = cursor {
            let mut layers = Vec::new();
            if let Some(base) = self.frame(surface)
                && self
                    .collect(surface, [0, 0], Some(base.logical_size), &mut layers)
                    .is_ok()
            {
                for layer in layers {
                    if let Some(image) = self.images.get_mut(&layer.surface.id()) {
                        callbacks.append(&mut image.callbacks);
                    }
                }
            }
        }
        callbacks
    }
    pub(super) fn cursor_frame(&self, surface: &WlSurface) -> io::Result<Option<Frame>> {
        let Some(mut base) = self.frame(surface).cloned() else {
            return Ok(None);
        };
        let mut layers = Vec::new();
        self.collect(surface, [0, 0], Some(base.logical_size), &mut layers)?;
        if layers.len() == 1 && layers[0].placement.mapping.can_reuse_pixels() {
            return Ok(Some(base));
        }
        [base.width, base.height] = self.images[&surface.id()]
            .layout
            .expect("imported poster layout")
            .mapping
            .physical;
        let mut pixels = vec![0; base.rgba.len()];
        for layer in layers {
            blend(&mut pixels, &base, &layer);
        }
        Ok(Some(Frame {
            rgba: pixels.into(),
            ..base
        }))
    }
    pub(super) fn hit(&self, x: f64, y: f64) -> Option<(WlSurface, (f64, f64))> {
        self.layers.iter().rev().find_map(|layer| {
            let p = &layer.placement;
            let (local_x, local_y) = (x - f64::from(p.origin[0]), y - f64::from(p.origin[1]));
            if local_x < 0.
                || local_y < 0.
                || local_x >= f64::from(p.logical[0])
                || local_y >= f64::from(p.logical[1])
            {
                return None;
            }
            let mut accepts = p.region.is_none();
            if let Some(region) = &p.region {
                for [add, rx, ry, width, height] in region {
                    if local_x >= f64::from(*rx)
                        && local_y >= f64::from(*ry)
                        && local_x < f64::from(*rx) + f64::from(*width)
                        && local_y < f64::from(*ry) + f64::from(*height)
                    {
                        accepts = *add != 0;
                    }
                }
            }
            accepts.then(|| {
                (
                    layer.surface.clone(),
                    (f64::from(p.origin[0]), f64::from(p.origin[1])),
                )
            })
        })
    }
    pub(super) fn compose(
        &mut self,
        root: &WlSurface,
        popups: &[(WlSurface, [i32; 2])],
    ) -> io::Result<Option<(Frame, bool)>> {
        let Some(mut base) = self.frame(root).cloned() else {
            self.clear_rendered();
            return Ok(None);
        };
        let mut layers = Vec::new();
        self.collect(root, [0, 0], Some(base.logical_size), &mut layers)?;
        for (surface, origin) in popups {
            self.collect(surface, *origin, Some(base.logical_size), &mut layers)?;
        }
        let placements: Vec<_> = layers.iter().map(|layer| layer.placement.clone()).collect();
        let changed = self
            .previous
            .as_ref()
            .is_some_and(|previous| *previous != placements);
        self.previous = Some(placements);
        [base.width, base.height] = self.images[&root.id()]
            .layout
            .expect("imported poster layout")
            .mapping
            .physical;
        let rgba = if layers.len() == 1
            && layers[0].surface == *root
            && layers[0].placement.mapping.can_reuse_pixels()
        {
            base.rgba.clone()
        } else {
            let mut pixels = vec![0; base.rgba.len()];
            for layer in &layers {
                blend(&mut pixels, &base, layer);
            }
            pixels.into()
        };
        self.layers = layers
            .into_iter()
            .map(|layer| HitLayer {
                surface: layer.surface,
                placement: layer.placement,
            })
            .collect();
        Ok(Some((Frame { rgba, ..base }, changed)))
    }
    fn collect(
        &self,
        root: &WlSurface,
        origin: [i32; 2],
        canvas: Option<[u32; 2]>,
        layers: &mut Vec<Layer>,
    ) -> io::Result<()> {
        let failed = Cell::new(false);
        let parent_committed =
            |surface: &WlSurface, states: &smithay::wayland::compositor::SurfaceData| {
                surface == root
                    || states
                        .cached_state
                        .get::<SubsurfaceCachedState>()
                        .current()
                        .parent_committed
            };
        let position = |surface: &WlSurface,
                        states: &smithay::wayland::compositor::SurfaceData,
                        parent: &([i32; 2], u8)| {
            let mut point = parent.0;
            if surface != root {
                let location = states
                    .cached_state
                    .get::<SubsurfaceCachedState>()
                    .current()
                    .location;
                point = [
                    point[0].checked_add(location.x)?,
                    point[1].checked_add(location.y)?,
                ];
            }
            if parent.1 > MAX_TREE_DEPTH
                || point
                    .iter()
                    .any(|value| value.unsigned_abs() > MAX_OFFSET as u32)
            {
                return None;
            }
            Some(point)
        };
        with_surface_tree_upward(
            root,
            (origin, 0u8),
            |surface, states, parent| {
                if !parent_committed(surface, states) {
                    return TraversalAction::SkipChildren;
                }
                let Some(point) = position(surface, states, parent) else {
                    failed.set(true);
                    return TraversalAction::SkipChildren;
                };
                if self.frame(surface).is_none() {
                    return TraversalAction::SkipChildren;
                }
                TraversalAction::DoChildren((point, parent.1 + 1))
            },
            |surface, states, parent| {
                if !parent_committed(surface, states) {
                    return;
                }
                let (Some(point), Some(frame)) =
                    (position(surface, states, parent), self.frame(surface))
                else {
                    return;
                };
                if canvas.is_some_and(|canvas| {
                    i64::from(point[0]) + i64::from(frame.logical_size[0]) <= 0
                        || i64::from(point[1]) + i64::from(frame.logical_size[1]) <= 0
                        || i64::from(point[0]) >= i64::from(canvas[0])
                        || i64::from(point[1]) >= i64::from(canvas[1])
                }) {
                    return;
                }
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                let region = &attributes.current().input_region;
                let Ok(region) = region_rects(region.as_ref()) else {
                    failed.set(true);
                    return;
                };
                layers.push(Layer {
                    surface: surface.clone(),
                    frame: frame.clone(),
                    placement: Placement {
                        id: surface.id(),
                        origin: point,
                        logical: frame.logical_size,
                        physical: [frame.width, frame.height],
                        mapping: self
                            .images
                            .get(&surface.id())
                            .and_then(|image| image.layout)
                            .expect("imported surface layout")
                            .mapping,
                        region,
                    },
                });
            },
            |_, _, _| true,
        );
        if failed.get() {
            return Err(invalid("surface tree geometry exceeds hosted bounds"));
        }
        Ok(())
    }
}

pub(super) fn belongs_to(surface: &WlSurface, root: &WlSurface) -> bool {
    let mut current = Some(surface.clone());
    for _ in 0..=MAX_TREE_DEPTH {
        let Some(surface) = current else { return false };
        if &surface == root {
            return true;
        }
        current = smithay::wayland::compositor::get_parent(&surface);
    }
    false
}

fn region_rects(region: Option<&RegionAttributes>) -> io::Result<Option<Vec<[i32; 5]>>> {
    let Some(region) = region else {
        return Ok(None);
    };
    if region.rects.len() > MAX_REGION_RECTS {
        return Err(invalid("surface input region exceeds bounds"));
    }
    region
        .rects
        .iter()
        .map(|(kind, rect)| {
            if rect.loc.x.unsigned_abs() > MAX_OFFSET as u32
                || rect.loc.y.unsigned_abs() > MAX_OFFSET as u32
                || rect.size.w < 0
                || rect.size.h < 0
                || rect.size.w > MAX_OFFSET
                || rect.size.h > MAX_OFFSET
            {
                return Err(invalid("surface input region exceeds bounds"));
            }
            Ok([
                i32::from(matches!(kind, RectangleKind::Add)),
                rect.loc.x,
                rect.loc.y,
                rect.size.w,
                rect.size.h,
            ])
        })
        .collect::<io::Result<Vec<_>>>()
        .map(Some)
}

fn blend(pixels: &mut [u8], base: &Frame, layer: &Layer) {
    let [logical_width, logical_height] = base.logical_size;
    let [ox, oy] = layer.placement.origin;
    let x0 = ((i64::from(ox).max(0) * i64::from(base.width)) / i64::from(logical_width)) as u32;
    let y0 = ((i64::from(oy).max(0) * i64::from(base.height)) / i64::from(logical_height)) as u32;
    let x1 = ((i64::from(ox) + i64::from(layer.frame.logical_size[0]))
        .clamp(0, i64::from(logical_width))
        * i64::from(base.width)
        / i64::from(logical_width)) as u32;
    let y1 = ((i64::from(oy) + i64::from(layer.frame.logical_size[1]))
        .clamp(0, i64::from(logical_height))
        * i64::from(base.height)
        / i64::from(logical_height)) as u32;
    // Retain the fractional logical position until converting to source pixels.
    // Rounding to whole logical coordinates first loses physical pixels even
    // when the root and child have exactly the same scale. Validated 2048-pixel
    // extents and 8192-pixel origins keep these products within u64.
    for y in y0..y1 {
        let ly = i64::from(y) * i64::from(logical_height) - i64::from(oy) * i64::from(base.height);
        let sy = layer
            .placement
            .mapping
            .sample(1, ly.max(0) as u64, base.height)
            .min(u64::from(layer.placement.mapping.physical[1] - 1));
        for x in x0..x1 {
            let lx =
                i64::from(x) * i64::from(logical_width) - i64::from(ox) * i64::from(base.width);
            let sx = layer
                .placement
                .mapping
                .sample(0, lx.max(0) as u64, base.width)
                .min(u64::from(layer.placement.mapping.physical[0] - 1));
            let [sx, sy] = layer.placement.mapping.raw_pixel(sx, sy);
            let source = (sy * u64::from(layer.frame.width) + sx) as usize * 4;
            let dest = (u64::from(y) * u64::from(base.width) + u64::from(x)) as usize * 4;
            let src = &layer.frame.rgba[source..source + 4];
            for channel in 0..4 {
                pixels[dest + channel] = (u16::from(src[channel])
                    + (u16::from(pixels[dest + channel]) * u16::from(255 - src[3]) + 127) / 255)
                    .min(255) as u8;
            }
        }
    }
}
