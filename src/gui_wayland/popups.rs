//! Bounded xdg popup placement and presentation-aware grab policy.

// Rust guideline compliant 2026-10-07
use super::{scene::Scene, *};
use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle, Serial},
    wayland::{
        compositor::with_states,
        shell::xdg::{PopupSurface, PositionerState, XdgPopupSurfaceData},
    },
};

// Popup roles share the display's surface/pixel budgets. Separate bounds also
// cap dismissed roles, pending configure responses and retained serial proofs.
const MAX_POPUPS: usize = 8;
const MAX_CONFIGURES: usize = 16;
const MAX_PROOFS: usize = 16;
const MAX_OFFSET: i32 = 8192;
const PROOF_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5);

struct Popup {
    role: PopupSurface,
    parent: WlSurface,
    positioner: PositionerState,
    desired: Option<Rectangle<i32, Logical>>,
    pending: Vec<Serial>,
    requests: std::collections::VecDeque<(PositionerState, u32)>,
    committed: bool,
    dismissed: bool,
    grabbing: bool,
}
struct Proof {
    serial: Serial,
    root: u64,
    source: WlSurface,
    at: Instant,
}
#[derive(Default)]
pub(super) struct Popups {
    entries: Vec<Popup>,
    proofs: Vec<Proof>,
}
impl Popups {
    pub(super) fn add(
        &mut self,
        role: PopupSurface,
        positioner: PositionerState,
        root: Option<&WlSurface>,
    ) -> io::Result<()> {
        validate_positioner(positioner)?;
        let parent = role
            .get_parent_surface()
            .ok_or_else(|| invalid("popup has no parent"))?;
        if Some(&parent) != root
            && !self
                .entries
                .iter()
                .any(|p| p.role.wl_surface() == &parent && !p.dismissed && p.role.alive())
        {
            role.send_popup_done();
            return Ok(());
        }
        if self.entries.len() >= MAX_POPUPS {
            return Err(invalid("popup role budget exceeded"));
        }
        self.entries.push(Popup {
            role,
            parent,
            positioner,
            desired: None,
            pending: vec![],
            requests: Default::default(),
            committed: false,
            dismissed: false,
            grabbing: false,
        });
        Ok(())
    }
    pub(super) fn prepare_commit(
        &mut self,
        surface: &WlSurface,
        buffer: bool,
        removed: bool,
    ) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|p| p.role.wl_surface() == surface)
        else {
            return false;
        };
        let popup = &mut self.entries[index];
        if popup.dismissed || !popup.role.alive() {
            return false;
        }
        if buffer && !popup.role.ensure_configured() {
            return false;
        }
        popup.committed = true;
        if removed && popup.desired.is_some() {
            // Unmapping ends this presentation and its descendant grabs. A
            // subsequent initial commit may configure this still-live role again.
            popup.role.reset_initial_configure_sent();
            popup.desired = None;
            popup.pending.clear();
            popup.requests.clear();
            popup.grabbing = false;
            popup.committed = false;
            self.dismiss_descendants(surface);
        }
        true
    }
    pub(super) fn ack(&mut self, surface: &WlSurface, serial: Serial) {
        if let Some(popup) = self
            .entries
            .iter_mut()
            .find(|p| p.role.wl_surface() == surface)
        {
            popup.pending.retain(|pending| *pending > serial);
        }
    }
    pub(super) fn reposition(
        &mut self,
        role: &PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) -> io::Result<()> {
        validate_positioner(positioner)?;
        if let Some(popup) = self
            .entries
            .iter_mut()
            .find(|p| &p.role == role && !p.dismissed)
        {
            if popup.requests.len() >= MAX_CONFIGURES {
                return Err(invalid("popup reposition budget exceeded"));
            }
            // Never coalesce client tokens: each accepted request gets its own
            // repositioned/configure sequence, bounded by outstanding ACKs.
            popup.requests.push_back((positioner, token));
        }
        Ok(())
    }
    pub(super) fn note_press(&mut self, serial: Serial, root: u64, source: Option<WlSurface>) {
        let Some(source) = source else { return };
        self.proofs
            .retain(|p| p.at.elapsed() <= PROOF_LIFETIME && p.source.is_alive());
        if self.proofs.len() >= MAX_PROOFS {
            self.proofs.remove(0);
        }
        self.proofs.push(Proof {
            serial,
            root,
            source,
            at: Instant::now(),
        });
    }
    pub(super) fn clear_proofs(&mut self) {
        self.proofs.clear();
    }
    pub(super) fn grab(
        &mut self,
        role: &PopupSurface,
        serial: Serial,
        root: &WlSurface,
        root_serial: u64,
        seat_valid: bool,
    ) {
        let Some(index) = self.entries.iter().position(|p| &p.role == role) else {
            return;
        };
        let top = self
            .entries
            .iter()
            .rev()
            .find(|p| p.grabbing && !p.dismissed && p.role.alive())
            .map(|p| p.role.wl_surface());
        let popup = &self.entries[index];
        let valid_parent = top.map_or(&popup.parent == root, |top| &popup.parent == top);
        let valid_proof = self.proofs.iter().any(|p| {
            p.serial == serial
                && p.root == root_serial
                && p.at.elapsed() <= PROOF_LIFETIME
                && p.source.is_alive()
                && p.source.id().same_client_as(&role.wl_surface().id())
        });
        if popup.committed || popup.grabbing {
            role.xdg_popup().post_error(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_popup::Error::InvalidGrab,
                "Popup grab must precede its initial commit.");
        } else if popup.dismissed || !seat_valid || !valid_parent || !valid_proof {
            self.dismiss(index);
        } else {
            self.entries[index].grabbing = true;
        }
    }
    pub(super) fn has_grabs(&self) -> bool {
        self.entries
            .iter()
            .any(|p| p.grabbing && !p.dismissed && p.role.alive())
    }
    pub(super) fn dismiss_all(&mut self) -> bool {
        let changed = self.entries.iter().any(|p| !p.dismissed && p.role.alive());
        for popup in self.entries.iter_mut().rev() {
            if !popup.dismissed && popup.role.alive() {
                popup.role.send_popup_done();
            }
            popup.dismissed = true;
            popup.grabbing = false;
        }
        self.clear_proofs();
        changed
    }
    fn dismiss(&mut self, index: usize) {
        let surface = self.entries[index].role.wl_surface().clone();
        self.dismiss_descendants(&surface);
        let popup = &mut self.entries[index];
        if !popup.dismissed && popup.role.alive() {
            popup.role.send_popup_done();
        }
        popup.dismissed = true;
        popup.grabbing = false;
    }
    fn dismiss_descendants(&mut self, surface: &WlSurface) {
        let mut parents = vec![surface.clone()];
        // Parents precede their children in registration order. Reverse the
        // notifications so nested clients can destroy their roles topmost first.
        let mut children = Vec::new();
        for (index, popup) in self.entries.iter().enumerate() {
            if parents.contains(&popup.parent) {
                parents.push(popup.role.wl_surface().clone());
                children.push(index);
            }
        }
        for index in children.into_iter().rev() {
            let popup = &mut self.entries[index];
            if !popup.dismissed && popup.role.alive() {
                popup.role.send_popup_done();
            }
            popup.dismissed = true;
            popup.grabbing = false;
        }
    }
    pub(super) fn destroyed(&mut self, surface: &WlSurface) {
        self.dismiss_descendants(surface);
        self.entries.retain(|p| p.role.wl_surface() != surface);
    }
    pub(super) fn keyboard_focus(&self, scene: &Scene, root: &WlSurface) -> WlSurface {
        self.entries
            .iter()
            .rev()
            .find(|p| {
                p.grabbing && !p.dismissed && p.role.alive() && scene.has_tree(p.role.wl_surface())
            })
            .map_or_else(|| root.clone(), |p| p.role.wl_surface().clone())
    }
    pub(super) fn layouts(
        &self,
        scene: &Scene,
        root: &WlSurface,
    ) -> io::Result<Vec<(WlSurface, [i32; 2])>> {
        let mut layouts = Vec::new();
        for popup in &self.entries {
            if popup.dismissed
                || !popup.role.alive()
                || scene.frame(popup.role.wl_surface()).is_none()
            {
                continue;
            }
            let current = with_states(popup.role.wl_surface(), |states| {
                let state = states
                    .data_map
                    .get::<XdgPopupSurfaceData>()
                    .expect("xdg popup state")
                    .lock()
                    .expect("popup state lock");
                state.configured.then_some(state.current.geometry)
            });
            let Some(current) = current else { continue };
            let Some(parent) = parent_origin(&layouts, root, &popup.parent) else {
                continue;
            };
            let parent_geometry = scene.window_geometry(&popup.parent)?;
            let own_geometry = scene.window_geometry(popup.role.wl_surface())?;
            let x = parent[0]
                .checked_add(parent_geometry.loc.x)
                .and_then(|v| v.checked_add(current.loc.x))
                .and_then(|v| v.checked_sub(own_geometry.loc.x));
            let y = parent[1]
                .checked_add(parent_geometry.loc.y)
                .and_then(|v| v.checked_add(current.loc.y))
                .and_then(|v| v.checked_sub(own_geometry.loc.y));
            let origin = bounded_origin(x, y)?;
            layouts.push((popup.role.wl_surface().clone(), origin));
        }
        Ok(layouts)
    }
    pub(super) fn reconcile(&mut self, scene: &Scene, root: &WlSurface) -> io::Result<()> {
        let Some(canvas) = scene.frame(root) else {
            return Ok(());
        };
        let layouts = self.layouts(scene, root)?;
        for popup in &mut self.entries {
            if popup.dismissed || !popup.role.alive() || !popup.committed {
                continue;
            }
            let Some(origin) = parent_origin(&layouts, root, &popup.parent) else {
                continue;
            };
            let parent_geometry = scene.window_geometry(&popup.parent)?;
            let origin = bounded_origin(
                origin[0].checked_add(parent_geometry.loc.x),
                origin[1].checked_add(parent_geometry.loc.y),
            )?;
            let target = Rectangle::new(
                (-origin[0], -origin[1]).into(),
                (canvas.logical_size[0] as i32, canvas.logical_size[1] as i32).into(),
            );
            loop {
                let request = popup.requests.pop_front();
                if let Some((positioner, _)) = request {
                    popup.positioner = positioner;
                }
                let geometry = popup.positioner.get_unconstrained_geometry(target);
                if popup.desired.is_some()
                    && request.is_none()
                    && (!popup.positioner.reactive || popup.desired == Some(geometry))
                {
                    break;
                }
                if popup.pending.len() >= MAX_CONFIGURES {
                    return Err(invalid("popup configure budget exceeded"));
                }
                popup.role.with_pending_state(|state| {
                    state.positioner = popup.positioner;
                    state.geometry = geometry;
                });
                let serial = if let Some((_, token)) = request {
                    popup.role.send_repositioned(token)
                } else {
                    popup
                        .role
                        .send_configure()
                        .map_err(|_| invalid("popup cannot be reconfigured"))?
                };
                popup.desired = Some(geometry);
                popup.pending.push(serial);
                if popup.requests.is_empty() {
                    break;
                }
            }
        }
        Ok(())
    }
}
fn parent_origin(
    layouts: &[(WlSurface, [i32; 2])],
    root: &WlSurface,
    parent: &WlSurface,
) -> Option<[i32; 2]> {
    if parent == root {
        Some([0, 0])
    } else {
        layouts
            .iter()
            .find(|(surface, _)| surface == parent)
            .map(|(_, origin)| *origin)
    }
}
fn bounded_origin(x: Option<i32>, y: Option<i32>) -> io::Result<[i32; 2]> {
    let point = [
        x.ok_or_else(|| invalid("popup coordinate overflow"))?,
        y.ok_or_else(|| invalid("popup coordinate overflow"))?,
    ];
    if point
        .iter()
        .any(|value| value.unsigned_abs() > MAX_OFFSET as u32)
    {
        return Err(invalid("popup coordinate exceeds hosted bounds"));
    }
    Ok(point)
}
fn validate_positioner(p: PositionerState) -> io::Result<()> {
    if p.rect_size.w <= 0
        || p.rect_size.h <= 0
        || p.rect_size.w > MAX_DIMENSION as i32
        || p.rect_size.h > MAX_DIMENSION as i32
        || p.anchor_rect.size.w <= 0
        || p.anchor_rect.size.h <= 0
        || p.anchor_rect.size.w > MAX_OFFSET
        || p.anchor_rect.size.h > MAX_OFFSET
        || [
            p.anchor_rect.loc.x,
            p.anchor_rect.loc.y,
            p.offset.x,
            p.offset.y,
        ]
        .iter()
        .any(|n| n.unsigned_abs() > MAX_OFFSET as u32)
        || p.parent_size.is_some_and(|size| {
            size.w <= 0 || size.h <= 0 || size.w > MAX_OFFSET || size.h > MAX_OFFSET
        })
    {
        return Err(invalid("popup positioner exceeds hosted bounds"));
    }
    Ok(())
}
