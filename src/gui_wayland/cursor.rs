//! Client cursor snapshots under the presented root's pointer authority.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::protocol::wl_surface::{self, WlSurface},
    wayland::compositor::with_states,
};

/// Maximum physical cursor extent, allowing a 64-pixel cursor at scale four.
pub const MAX_CURSOR_DIMENSION: u32 = 256;
/// Maximum absolute surface-local hotspot coordinate.
pub const MAX_CURSOR_HOTSPOT: u32 = 8192;

/// Cursor pixels associated with one host frame's root and geometry.
#[derive(Clone, Debug, Default)]
pub enum CursorImage {
    /// Use the embedding host's ordinary pointer.
    #[default]
    Default,
    /// Use a named platform cursor over the presented client viewport.
    Named(crate::gui::CursorStyle),
    /// Hide the pointer over the presented client viewport.
    Hidden,
    /// Paint an immutable client cursor in surface-local coordinates.
    Surface(CursorSurface),
}

/// An owned, bounded premultiplied RGBA cursor image.
#[derive(Clone)]
pub struct CursorSurface {
    /// Checked revision local to this display; qualify it with the frame target.
    pub revision: u64,
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// Surface-local dimensions, accounting for integer buffer scale.
    pub logical_size: [u32; 2],
    /// Surface-local pointer hotspot; it may lie outside the image.
    pub hotspot: [i32; 2],
    /// Immutable packed premultiplied RGBA pixels.
    pub rgba: Arc<[u8]>,
}
impl fmt::Debug for CursorSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CursorSurface")
            .field("revision", &self.revision)
            .field("size", &[self.width, self.height])
            .field("logical_size", &self.logical_size)
            .field("hotspot", &self.hotspot)
            .finish_non_exhaustive()
    }
}

pub(super) struct Cursor {
    status: CursorImageStatus,
    revision: u64,
}
impl Default for Cursor {
    fn default() -> Self {
        Self {
            status: CursorImageStatus::default_named(),
            revision: 0,
        }
    }
}
impl Cursor {
    pub(super) fn offset(surface: &WlSurface, offset: [i32; 2]) -> io::Result<()> {
        let old = hotspot(surface)?;
        let x = old[0]
            .checked_sub(offset[0])
            .ok_or_else(|| invalid("cursor offset overflow"))?;
        let y = old[1]
            .checked_sub(offset[1])
            .ok_or_else(|| invalid("cursor offset overflow"))?;
        if x.unsigned_abs() > MAX_CURSOR_HOTSPOT || y.unsigned_abs() > MAX_CURSOR_HOTSPOT {
            return Err(invalid("cursor offset exceeds hosted bounds"));
        }
        with_states(surface, |states| {
            states
                .data_map
                .get::<CursorImageSurfaceData>()
                .expect("validated cursor attributes")
                .lock()
                .expect("cursor attributes mutex")
                .hotspot = (x, y).into();
        });
        Ok(())
    }
    pub(super) fn surface(&self) -> Option<&WlSurface> {
        match &self.status {
            CursorImageStatus::Surface(surface) => Some(surface),
            _ => None,
        }
    }
    pub(super) fn change(&mut self, status: CursorImageStatus) -> io::Result<bool> {
        if self.status == status && !matches!(status, CursorImageStatus::Surface(_)) {
            return Ok(false);
        }
        if let CursorImageStatus::Surface(surface) = &status {
            hotspot(surface)?;
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("cursor revision exhausted"))?;
        self.status = status;
        Ok(true)
    }
    pub(super) fn committed(&mut self, surface: &WlSurface) -> io::Result<()> {
        if self
            .surface()
            .is_some_and(|root| super::scene::belongs_to(surface, root))
        {
            self.change(self.status.clone())?;
        }
        Ok(())
    }
    pub(super) fn snapshot(&self, scene: &super::scene::Scene) -> CursorImage {
        match &self.status {
            CursorImageStatus::Hidden => CursorImage::Hidden,
            CursorImageStatus::Named(icon) => {
                let style = super::shapes::style(*icon);
                if style == crate::gui::CursorStyle::Arrow {
                    CursorImage::Default
                } else {
                    CursorImage::Named(style)
                }
            }
            CursorImageStatus::Surface(surface) => {
                let frame = match scene.cursor_frame(surface) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return CursorImage::Hidden,
                    Err(_) => {
                        surface.post_error(
                            wl_surface::Error::InvalidSize,
                            "cursor tree exceeds hosted bounds",
                        );
                        return CursorImage::Default;
                    }
                };
                let Ok(hotspot) = hotspot(surface) else {
                    return CursorImage::Default;
                };
                if frame.width > MAX_CURSOR_DIMENSION || frame.height > MAX_CURSOR_DIMENSION {
                    return CursorImage::Default;
                }
                CursorImage::Surface(CursorSurface {
                    revision: self.revision,
                    width: frame.width,
                    height: frame.height,
                    logical_size: frame.logical_size,
                    hotspot,
                    rgba: frame.rgba.clone(),
                })
            }
        }
    }
}
fn hotspot(surface: &WlSurface) -> io::Result<[i32; 2]> {
    let point = with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|data| data.lock().expect("cursor attributes mutex").hotspot)
    })
    .ok_or_else(|| invalid("missing cursor attributes"))?;
    if point.x.unsigned_abs() > MAX_CURSOR_HOTSPOT || point.y.unsigned_abs() > MAX_CURSOR_HOTSPOT {
        return Err(invalid("cursor hotspot exceeds hosted bounds"));
    }
    Ok([point.x, point.y])
}
