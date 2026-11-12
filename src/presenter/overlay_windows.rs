//! Routes authenticated owners to bounded, independent pane window managers.

// Rust guideline compliant 2026-10-07
use crate::presenter::PaneId;
use std::collections::BTreeMap;
#[cfg(any(feature = "vector-render", feature = "testing"))]
use vivid_protocol::vector::CursorShape;
use vivid_protocol::{
    identity::{SessionIdentity, SurfaceIdentity},
    overlay::{
        self, DismissReason, Event, PointerReport, Scroll, Window, WindowEvent, WindowOptions, wire,
    },
    vector::{HitRegion, InvalidScene, Point},
};

// Matches the compositor's simultaneous pane budget. Empty focus state is retired
// by revoke_pane, while ordinary session reconnect retains the host's focus policy.
const MAX_PANES: usize = 64;

#[derive(Default)]
pub(super) struct PaneWindows {
    owners: BTreeMap<SessionIdentity, PaneId>,
    panes: BTreeMap<PaneId, overlay::Windows>,
}
impl PaneWindows {
    pub fn bind(&mut self, owner: SessionIdentity, pane: PaneId) -> Result<(), InvalidScene> {
        if let Some(bound) = self.owners.get(&owner) {
            return if *bound == pane {
                Ok(())
            } else {
                Err(InvalidScene::new("overlay owner changed pane"))
            };
        }
        if !self.panes.contains_key(&pane) && self.panes.len() >= MAX_PANES {
            return Err(InvalidScene::new("overlay pane capacity exhausted"));
        }
        self.panes.entry(pane).or_default();
        self.owners.insert(owner, pane);
        Ok(())
    }
    pub fn pane(&self, owner: SessionIdentity) -> Option<PaneId> {
        self.owners.get(&owner).copied()
    }
    pub fn pane_of_sessions(&self, sessions: &[u64]) -> Option<PaneId> {
        let pane = self.pane(super::owner(*sessions.first()?))?;
        sessions
            .iter()
            .all(|id| self.pane(super::owner(*id)) == Some(pane))
            .then_some(pane)
    }
    fn for_owner(&self, owner: SessionIdentity) -> Option<&overlay::Windows> {
        self.panes.get(&self.pane(owner)?)
    }
    fn for_owner_mut(
        &mut self,
        owner: SessionIdentity,
    ) -> Result<&mut overlay::Windows, InvalidScene> {
        let pane = self
            .pane(owner)
            .ok_or(InvalidScene::new("overlay owner has no authenticated pane"))?;
        self.panes
            .get_mut(&pane)
            .ok_or(InvalidScene::new("overlay pane is absent"))
    }
    fn owner_capacity(&self, owner: SessionIdentity) -> Result<(), InvalidScene> {
        if !self.has_owner(owner)
            && self
                .panes
                .values()
                .map(overlay::Windows::owner_count)
                .sum::<usize>()
                >= overlay::MAX_OWNERS
        {
            return Err(InvalidScene::new("overlay owner capacity exhausted"));
        }
        Ok(())
    }
    pub fn get(&self, id: SurfaceIdentity) -> Option<&Window> {
        self.for_owner(id.context().session())?.get(id)
    }
    pub fn has_owner(&self, owner: SessionIdentity) -> bool {
        self.for_owner(owner)
            .is_some_and(|windows| windows.has_owner(owner))
    }
    #[cfg(any(test, feature = "vector-render"))]
    pub fn visible(&self) -> impl Iterator<Item = &Window> {
        self.panes.values().flat_map(overlay::Windows::visible)
    }
    pub fn create(
        &mut self,
        id: SurfaceIdentity,
        generation: u64,
        options: WindowOptions,
    ) -> Result<(), InvalidScene> {
        if self
            .panes
            .values()
            .map(overlay::Windows::window_count)
            .sum::<usize>()
            >= overlay::MAX_WINDOWS
        {
            return Err(InvalidScene::new("overlay window capacity exhausted"));
        }
        self.owner_capacity(id.context().session())?;
        self.for_owner_mut(id.context().session())?
            .create(id, generation, options)
    }
    pub fn update(
        &mut self,
        id: SurfaceIdentity,
        generation: u64,
        revision: u64,
        options: WindowOptions,
    ) -> Result<u64, InvalidScene> {
        self.for_owner_mut(id.context().session())?
            .update(id, generation, revision, options)
    }
    pub fn publish_scene(
        &mut self,
        id: SurfaceIdentity,
        generation: u64,
        revision: u64,
    ) -> Result<(), InvalidScene> {
        self.for_owner_mut(id.context().session())?
            .publish_scene(id, generation, revision)
    }
    pub fn apply_action(
        &mut self,
        owner: SessionIdentity,
        action: wire::Action,
        viewport: wire::Viewport,
    ) -> Result<Option<u64>, InvalidScene> {
        self.for_owner_mut(owner)?
            .apply_action(owner, action, viewport)
    }
    pub fn request_focus(&mut self, id: SurfaceIdentity) -> Result<(), InvalidScene> {
        self.for_owner_mut(id.context().session())?
            .request_focus(id)
    }
    pub fn acknowledge_relayed_action(
        &mut self,
        owner: SessionIdentity,
        action: wire::Action,
        bounds: Option<vivid_protocol::vector::Rect>,
    ) -> Result<Option<u64>, InvalidScene> {
        self.for_owner_mut(owner)?
            .acknowledge_relayed_action(owner, action, bounds)
    }
    pub fn clear_focus(&mut self, id: SurfaceIdentity) {
        if let Ok(windows) = self.for_owner_mut(id.context().session()) {
            windows.clear_focus(id);
        }
    }
    pub fn focus(&self, pane: PaneId) -> Option<SurfaceIdentity> {
        self.panes.get(&pane)?.focus()
    }
    pub fn focus_of(&self, id: SurfaceIdentity) -> Option<SurfaceIdentity> {
        self.for_owner(id.context().session())?.focus()
    }
    pub fn focus_generation(&self, pane: PaneId) -> Option<u64> {
        self.panes
            .get(&pane)
            .map(overlay::Windows::focus_generation)
    }
    #[cfg(any(test, feature = "vector-render"))]
    pub fn captured_window(&self, pane: PaneId) -> Option<SurfaceIdentity> {
        self.panes.get(&pane)?.captured_window()
    }
    pub fn input_eligible(&self, id: SurfaceIdentity) -> bool {
        self.for_owner(id.context().session())
            .is_some_and(|windows| windows.input_eligible(id))
    }
    pub fn action_eligible(&self, id: SurfaceIdentity, action: wire::WindowAction) -> bool {
        self.for_owner(id.context().session())
            .is_some_and(|windows| windows.action_eligible(id, action))
    }
    pub fn set_owner_input(
        &mut self,
        owner: SessionIdentity,
        enabled: bool,
    ) -> Result<(), InvalidScene> {
        if !enabled {
            self.owner_capacity(owner)?;
        }
        self.for_owner_mut(owner)?.set_owner_input(owner, enabled)
    }
    pub fn set_pane_focus(&mut self, pane: PaneId, focused: bool) {
        if !self.panes.contains_key(&pane) && self.panes.len() >= MAX_PANES {
            return;
        }
        self.panes.entry(pane).or_default().set_pane_focus(focused);
    }
    pub fn take_event(&mut self, owner: SessionIdentity) -> Option<WindowEvent> {
        self.for_owner_mut(owner).ok()?.take_event(owner)
    }
    pub fn capture_pointer(
        &mut self,
        id: SurfaceIdentity,
        point: Point,
    ) -> Result<(), InvalidScene> {
        self.for_owner_mut(id.context().session())?
            .capture_pointer(id, point)
    }
    pub fn release_pointer(&mut self, id: SurfaceIdentity) {
        if let Ok(windows) = self.for_owner_mut(id.context().session()) {
            windows.release_pointer(id);
        }
    }
    pub fn close(&mut self, id: SurfaceIdentity, reason: DismissReason) {
        if let Ok(windows) = self.for_owner_mut(id.context().session()) {
            windows.close(id, reason);
        }
    }
    pub fn revoke_owner(&mut self, owner: SessionIdentity) {
        if let Ok(windows) = self.for_owner_mut(owner) {
            windows.revoke_owner(owner);
        }
        self.owners.remove(&owner);
    }
    pub fn remove_pane(&mut self, pane: PaneId) {
        self.owners.retain(|_, bound| *bound != pane);
        self.panes.remove(&pane);
    }
    pub fn hovered_window(&self, pane: PaneId) -> Option<SurfaceIdentity> {
        self.panes.get(&pane)?.hovered_window()
    }
    #[cfg(any(feature = "vector-render", feature = "testing"))]
    pub fn cursor(&self, pane: PaneId) -> Option<CursorShape> {
        self.panes.get(&pane)?.cursor()
    }
    #[cfg(feature = "vector-render")]
    pub fn swallowed_button_owner(&self, pane: PaneId, button: u16) -> Option<SessionIdentity> {
        self.panes.get(&pane)?.swallowed_button_owner(button)
    }
    #[cfg(feature = "vector-render")]
    pub fn swallowed_escape_owner(&self, pane: PaneId) -> Option<SessionIdentity> {
        self.panes.get(&pane)?.swallowed_escape_owner()
    }
    pub fn keyboard(&mut self, pane: PaneId, event: Event, escape: bool) -> bool {
        self.panes
            .get_mut(&pane)
            .is_some_and(|windows| windows.keyboard(event, escape))
    }
    pub fn pointer(
        &mut self,
        pane: PaneId,
        report: PointerReport,
        hit: impl Fn(SurfaceIdentity, Point) -> Option<HitRegion>,
    ) -> bool {
        self.panes
            .get_mut(&pane)
            .is_some_and(|windows| windows.pointer(report, hit))
    }
    #[cfg(feature = "vector-render")]
    pub fn refresh_hover(
        &mut self,
        pane: PaneId,
        point: Point,
        hit: impl Fn(SurfaceIdentity, Point) -> Option<HitRegion>,
    ) {
        if let Some(windows) = self.panes.get_mut(&pane) {
            windows.refresh_hover(point, &hit);
        }
    }
    pub fn wheel(
        &mut self,
        pane: PaneId,
        point: Point,
        scroll: Scroll,
        modifiers: u32,
        hit: impl Fn(SurfaceIdentity, Point) -> Option<(u64, vivid_protocol::vector::HitRole)>,
    ) -> bool {
        self.panes
            .get_mut(&pane)
            .is_some_and(|windows| windows.wheel(point, scroll, modifiers, hit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::{identity::PresenterInstanceId, overlay::WindowMode, vector::Rect};
    fn owner(id: u64) -> SessionIdentity {
        SessionIdentity::new(PresenterInstanceId([1; 16]), id).unwrap()
    }
    fn options() -> WindowOptions {
        WindowOptions::new(Rect::new(0., 0., 10., 10.).unwrap(), WindowMode::Floating)
    }
    #[test]
    fn owner_binding_is_immutable_and_owner_limits_remain_global() {
        let mut windows = PaneWindows::default();
        for n in 1..=overlay::MAX_OWNERS as u64 {
            let owner = owner(n);
            windows.bind(owner, n).unwrap();
            windows
                .create(owner.context(1).unwrap().surface(1).unwrap(), 1, options())
                .unwrap();
        }
        assert!(windows.bind(owner(1), 2).is_err());
        assert_eq!(windows.pane(owner(1)), Some(1));
        let extra = owner(100);
        windows.bind(extra, 100).unwrap();
        assert!(
            windows
                .create(extra.context(1).unwrap().surface(1).unwrap(), 1, options())
                .is_err()
        );
        windows.remove_pane(1);
        windows
            .create(extra.context(1).unwrap().surface(1).unwrap(), 1, options())
            .unwrap();
        assert_eq!(windows.visible().count(), overlay::MAX_OWNERS);
    }
    #[test]
    fn distributing_windows_over_panes_does_not_multiply_the_window_budget() {
        let mut windows = PaneWindows::default();
        for n in 1..=8 {
            let owner = owner(n);
            windows.bind(owner, n).unwrap();
            for id in 1..=overlay::MAX_WINDOWS_PER_OWNER as u64 {
                windows
                    .create(owner.context(1).unwrap().surface(id).unwrap(), 1, options())
                    .unwrap();
            }
        }
        assert_eq!(windows.visible().count(), overlay::MAX_WINDOWS);
        let extra = owner(100);
        windows.bind(extra, 100).unwrap();
        assert!(
            windows
                .create(extra.context(1).unwrap().surface(1).unwrap(), 1, options())
                .is_err()
        );
        assert_eq!(windows.visible().count(), overlay::MAX_WINDOWS);
    }
}
