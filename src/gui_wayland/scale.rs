//! Bounded output scale and per-surface fractional preferences.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    delegate_fractional_scale,
    reexports::wayland_server::{DisplayHandle, Weak, protocol::wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
    },
};

/// The protocol's integer scale denominator; 120 units represent scale one.
pub const SCALE_DENOMINATOR: u32 = 120;
/// Largest output scale supported by the bounded SHM backend (four).
pub const MAX_OUTPUT_SCALE: u32 = 4 * SCALE_DENOMINATOR;

pub(super) fn physical(width: u32, height: u32, units: u32) -> io::Result<[u32; 2]> {
    if !(SCALE_DENOMINATOR..=MAX_OUTPUT_SCALE).contains(&units) {
        return Err(invalid("output scale exceeds hosted bounds"));
    }
    extent(width, height)?;
    // Integer-only clients use the rounded-up wl_output scale. Reserve room
    // for those buffers as well as the fractional client's preferred buffers.
    let integer = units.div_ceil(SCALE_DENOMINATOR);
    for value in [width, height] {
        if value.checked_mul(integer).is_none_or(|v| v > MAX_DIMENSION) {
            return Err(invalid("scaled output exceeds hosted buffer bounds"));
        }
    }
    Ok([
        (width * units + SCALE_DENOMINATOR / 2) / SCALE_DENOMINATOR,
        (height * units + SCALE_DENOMINATOR / 2) / SCALE_DENOMINATOR,
    ])
}

pub(super) struct Scales {
    pub units: u32,
    surfaces: Vec<Weak<WlSurface>>,
}
impl Default for Scales {
    fn default() -> Self {
        Self {
            units: SCALE_DENOMINATOR,
            surfaces: Vec::new(),
        }
    }
}
impl Scales {
    pub fn register(display: &DisplayHandle) {
        FractionalScaleManagerState::new::<state::State>(display);
    }
    pub fn created(&mut self, surface: &WlSurface) {
        self.surfaces.retain(|surface| surface.is_alive());
        self.surfaces.push(surface.downgrade());
        self.notify(surface);
    }
    pub fn notify(&self, surface: &WlSurface) {
        with_states(surface, |states| {
            with_fractional_scale(states, |scale| {
                scale.set_preferred_scale(f64::from(self.units) / f64::from(SCALE_DENOMINATOR));
            });
        });
        if surface.version() >= 6 {
            surface.preferred_buffer_scale(self.units.div_ceil(SCALE_DENOMINATOR) as i32);
        }
    }
    pub fn update(&mut self, units: u32) {
        self.units = units;
        self.surfaces.retain(|surface| surface.is_alive());
        for surface in &self.surfaces {
            if let Ok(surface) = surface.upgrade() {
                self.notify(&surface);
            }
        }
    }
}
impl FractionalScaleHandler for state::State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.scales.notify(&surface);
    }
}
delegate_fractional_scale!(state::State);
