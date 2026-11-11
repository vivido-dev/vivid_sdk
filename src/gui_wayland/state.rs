//! Protocol dispatch and bounded, owned SHM readback through Smithay/Pixman.

// Rust guideline compliant 2026-10-07
use super::*;
use smithay::{
    backend::renderer::pixman::PixmanRenderer,
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState, pointer::CursorImageStatus},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, ClientId},
        protocol::{
            wl_buffer, wl_callback, wl_output, wl_seat,
            wl_surface::{self, WlSurface},
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            BufferAssignment, CompositorClientState, CompositorHandler, CompositorLimits,
            CompositorState, SurfaceAttributes, with_states,
        },
        output::OutputHandler,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
        shm::{ShmHandler, ShmLimits, ShmState},
        viewporter::ViewporterState,
    },
};
use std::sync::atomic::{AtomicBool, Ordering};

// One root plus a bounded native surface tree and client-owned helper surfaces.
const MAX_SURFACES: usize = 32;

#[derive(Default)]
pub(super) struct ClientState {
    compositor: CompositorClientState,
    pub(super) closed: AtomicBool,
    failed: Arc<AtomicBool>,
    root_class: Arc<AtomicBool>,
    budgets: Option<(
        Arc<wayland_backend::server::RequestBudget>,
        Arc<wayland_backend::server::RequestBudget>,
    )>,
}
pub(super) fn request_limits(helper_pool: bool) -> wayland_backend::server::RequestLimits {
    let scale = if helper_pool { 2 } else { 1 };
    let nonzero = |value: u32| {
        std::num::NonZeroU32::new(
            value
                .checked_mul(scale)
                .expect("fixed native rate fits u32"),
        )
        .expect("nonzero native request limit")
    };
    wayland_backend::server::RequestLimits {
        request_burst: nonzero(REQUEST_BURST),
        requests_per_second: nonzero(REQUESTS_PER_SECOND),
        byte_burst: nonzero(REQUEST_BYTE_BURST),
        bytes_per_second: nonzero(REQUEST_BYTES_PER_SECOND),
    }
}
impl ClientState {
    pub(super) fn with_budgets(
        root: Arc<wayland_backend::server::RequestBudget>,
        helpers: Arc<wayland_backend::server::RequestBudget>,
    ) -> Self {
        Self {
            budgets: Some((root, helpers)),
            ..Self::default()
        }
    }
}
impl ClientData for ClientState {
    fn request_limits(&self) -> Option<wayland_backend::server::RequestLimits> {
        Some(request_limits(false))
    }
    fn shared_request_budget(&self) -> Option<Arc<wayland_backend::server::RequestBudget>> {
        self.budgets.as_ref().map(|(root, helpers)| {
            if self.root_class.load(Ordering::Acquire) {
                root.clone()
            } else {
                helpers.clone()
            }
        })
    }
    fn max_buffered_fds(&self) -> Option<std::num::NonZeroUsize> {
        std::num::NonZeroUsize::new(MAX_BUFFERED_DESCRIPTORS)
    }
    fn max_object_slots(&self) -> Option<std::num::NonZeroUsize> {
        Some(
            std::num::NonZeroUsize::new(MAX_PROTOCOL_OBJECT_SLOTS)
                .expect("nonzero protocol budget"),
        )
    }
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
        if matches!(reason, DisconnectReason::ProtocolError(_)) {
            self.failed.store(true, Ordering::Release);
        }
        self.closed.store(true, Ordering::Release);
    }
}

pub(super) struct State {
    pub(super) cursor: super::cursor::Cursor,
    pub(super) scales: super::scale::Scales,
    pub(super) readback_stats: ReadbackStats,
    compositor: CompositorState,
    shell: XdgShellState,
    shm: ShmState,
    seats: SeatState<Self>,
    pub(super) seat: Seat<Self>,
    pub(super) root: Option<ToplevelSurface>,
    root_failure: Option<Arc<AtomicBool>>,
    root_request_class: Option<Arc<AtomicBool>>,
    failed: bool,
    pub(super) size: [u32; 2],
    pub(super) logical_size: [u32; 2],
    pub(super) frame: Option<Frame>,
    last_frame: Option<Frame>,
    pub(super) scene: super::scene::Scene,
    pub(super) text_input: super::text::TextInputs,
    pub(super) popups: super::popups::Popups,
    pub(super) clipboard: Option<super::clipboard::Clipboard>,
    dirty: bool,
    pub(super) error: Option<&'static str>,
    identity: [u8; 16],
    pub(super) root_serial: u64,
    pub(super) mapped: bool,
    geometry: u64,
    configure: Option<Serial>,
    configured: bool,
    surfaces: usize,
    renderer: PixmanRenderer,
    output: Output,
}

impl State {
    fn retire_root_request_class(&mut self) {
        if let Some(class) = self.root_request_class.take() {
            class.store(false, Ordering::Release);
        }
    }
    pub(super) fn install_clipboard(
        &mut self,
        display: &DisplayHandle,
        client: crate::gui_host::clipboard::Client,
    ) {
        self.clipboard = Some(super::clipboard::Clipboard::new(display, client));
    }
    pub(super) fn poll_clipboard(&mut self) {
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.poll(&self.seat);
        }
    }
    pub(super) fn new(display: &DisplayHandle, width: u32, height: u32) -> io::Result<Self> {
        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(display, "vivid-nested");
        seat.add_keyboard(Default::default(), 25, 600)
            .map_err(|_| io::Error::other("Wayland XKB initialization failed"))?;
        seat.add_pointer();
        let mut identity = [0; 16];
        getrandom::fill(&mut identity).map_err(io::Error::other)?;
        let output = Output::new(
            "vivid-nested".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Vivid".into(),
                model: "Nested display".into(),
            },
        );
        let mode = Mode {
            size: (width as i32, height as i32).into(),
            refresh: 30_000,
        };
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
        output.set_preferred(mode);
        output.create_global::<Self>(display);
        let nonzero =
            |value| std::num::NonZeroUsize::new(value).expect("native limits are nonzero");
        let compositor = CompositorState::with_limits::<Self>(
            display,
            CompositorLimits {
                region_rectangles: nonzero(MAX_REGION_OPERATIONS),
                damage_rectangles: nonzero(MAX_DAMAGE_RECTANGLES),
                frame_callbacks: nonzero(MAX_PENDING_CALLBACKS),
                cached_states: nonzero(MAX_CACHED_SURFACE_STATES),
            },
        );
        super::text::TextInputs::register(display);
        super::shapes::register(display);
        ViewporterState::new::<Self>(display);
        super::scale::Scales::register(display);
        Ok(Self {
            cursor: super::cursor::Cursor::default(),
            scales: super::scale::Scales::default(),
            readback_stats: ReadbackStats::default(),
            compositor,
            shell: XdgShellState::new::<Self>(display),
            // Two maximum-size pools allow double buffering; descriptors and
            // backing pools have independent bounds even without an attachment.
            shm: ShmState::with_limits::<Self>(
                display,
                vec![],
                ShmLimits::new(
                    MAX_SHM_POOL_BYTES,
                    MAX_SHM_BYTES,
                    MAX_SHM_POOLS,
                    MAX_SHM_BUFFERS,
                )?,
            ),
            seats,
            seat,
            root: None,
            root_failure: None,
            root_request_class: None,
            failed: false,
            size: [width, height],
            logical_size: [0; 2],
            frame: None,
            last_frame: None,
            scene: super::scene::Scene::default(),
            text_input: super::text::TextInputs::default(),
            popups: super::popups::Popups::default(),
            clipboard: None,
            dirty: false,
            error: None,
            identity,
            root_serial: 0,
            mapped: false,
            geometry: 1,
            configure: None,
            configured: false,
            surfaces: 0,
            renderer: PixmanRenderer::new().map_err(io::Error::other)?,
            output,
        })
    }
    pub(super) fn target(&self) -> Option<Target> {
        (self.root.as_ref().is_some_and(|root| root.alive())
            && self.configured
            && self.last_frame.is_some())
        .then_some(Target {
            host: self.identity,
            root: self.root_serial,
            geometry: self.geometry,
        })
    }
    pub(super) fn root_failed(&self) -> bool {
        self.failed
            || self
                .root_failure
                .as_ref()
                .is_some_and(|failed| failed.load(Ordering::Acquire))
    }
    pub(super) fn take_callbacks(&mut self, include_cursor: bool) -> Vec<wl_callback::WlCallback> {
        self.scene.take_callbacks(if include_cursor {
            self.cursor.surface()
        } else {
            None
        })
    }
    pub(super) fn hit(&self, x: f64, y: f64) -> Option<(WlSurface, (f64, f64))> {
        self.scene.hit(x, y)
    }
    pub(super) fn keyboard_focus(&self) -> Option<WlSurface> {
        self.root
            .as_ref()
            .map(|root| self.popups.keyboard_focus(&self.scene, root.wl_surface()))
    }
    pub(super) fn dismiss_popups(&mut self) -> bool {
        let changed = self.popups.dismiss_all();
        self.dirty |= changed;
        if changed {
            if let Some(geometry) = self.geometry.checked_add(1) {
                self.geometry = geometry;
            } else {
                self.error = Some("Wayland geometry identity exhausted");
            }
        }
        changed
    }
    pub(super) fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(root) = &self.root else {
            self.mapped = false;
            self.frame = None;
            self.scene.clear_rendered();
            return;
        };
        self.mapped = self.scene.frame(root.wl_surface()).is_some();
        if !self.configured {
            return;
        }
        let composed = self
            .popups
            .reconcile(&self.scene, root.wl_surface())
            .and_then(|()| self.popups.layouts(&self.scene, root.wl_surface()))
            .and_then(|popups| self.scene.compose(root.wl_surface(), &popups));
        match composed {
            Ok(Some((mut frame, changed))) => {
                if changed {
                    let Some(generation) = self.geometry.checked_add(1) else {
                        self.error = Some("Wayland geometry identity exhausted");
                        return;
                    };
                    self.geometry = generation;
                }
                frame.target = Target {
                    host: self.identity,
                    root: self.root_serial,
                    geometry: self.geometry,
                };
                self.logical_size = frame.logical_size;
                self.last_frame = Some(frame.clone());
                self.frame = Some(frame);
            }
            Ok(None) => {
                self.frame = None;
                self.last_frame = None;
            }
            Err(_) => {
                root.wl_surface().post_error(
                    wl_surface::Error::InvalidSize,
                    "surface tree exceeds hosted bounds",
                );
                self.frame = None;
                self.last_frame = None;
                self.mapped = false;
                self.scene.clear_rendered();
            }
        }
    }
    pub(super) fn resize(&mut self, width: u32, height: u32, scale_120: u32) -> io::Result<()> {
        let physical = super::scale::physical(width, height, scale_120)?;
        self.geometry = self
            .geometry
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Wayland geometry generation exhausted"))?;
        self.size = [width, height];
        let mode = Mode {
            size: (physical[0] as i32, physical[1] as i32).into(),
            refresh: 30_000,
        };
        let previous_mode = self.output.current_mode();
        self.output.change_current_state(
            Some(mode),
            None,
            Some(smithay::output::Scale::Fractional(
                f64::from(scale_120) / f64::from(SCALE_DENOMINATOR),
            )),
            None,
        );
        self.scales.update(scale_120);
        self.output.set_preferred(mode);
        if let Some(previous) = previous_mode.filter(|previous| *previous != mode) {
            self.output.delete_mode(previous);
        }
        self.configured = false;
        self.frame = None;
        // Keep an owned poster: configure sizes are advisory and a client may
        // legally commit the unchanged buffer after acknowledging the new state.
        if let Some(root) = &self.root {
            root.with_pending_state(|state| {
                state.size = Some((width as i32, height as i32).into());
            });
            self.configure = Some(root.send_configure());
        }
        Ok(())
    }
    fn read_buffer(
        &mut self,
        surface: &WlSurface,
        buffer: &wl_buffer::WlBuffer,
        scale: i32,
        transform: wl_output::Transform,
        damage: &[smithay::wayland::compositor::Damage],
        single_commit: bool,
    ) -> io::Result<(Frame, super::readback::Layout)> {
        let layout =
            super::readback::Layout::from_buffer(surface, buffer, scale, transform.into())?;
        if smithay::wayland::compositor::get_role(surface)
            == Some(smithay::wayland::seat::CURSOR_IMAGE_ROLE)
            && (layout.width > MAX_CURSOR_DIMENSION || layout.height > MAX_CURSOR_DIMENSION)
        {
            return Err(invalid("cursor dimensions exceed hosted bounds"));
        }
        if !self.scene.replacement_fits(surface, layout.bytes()) {
            return Err(invalid("retained surface pixels exceed hosted bounds"));
        }
        let previous = self.scene.take_poster(surface);
        let rgba = super::readback::capture(
            &mut self.renderer,
            buffer,
            layout,
            previous,
            damage,
            single_commit,
            &mut self.readback_stats,
        )?;
        Ok((
            Frame {
                cursor: CursorImage::Default,
                id: 0,
                target: Target {
                    host: self.identity,
                    root: self.root_serial,
                    geometry: self.geometry,
                },
                width: layout.width,
                height: layout.height,
                logical_size: layout.mapping.logical,
                rgba,
            },
            layout,
        ))
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {}
}
impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}
impl CompositorHandler for State {
    fn subsurface_destroyed(&mut self, surface: &WlSurface, parent: Option<&WlSurface>) {
        if let Some(parent) = parent
            && self
                .cursor
                .surface()
                .is_some_and(|root| super::scene::belongs_to(parent, root))
        {
            if self.cursor.committed(parent).is_err() {
                self.error = Some("cursor revision exhausted");
            }
            self.dirty = true;
        }
        if !self.scene.has_tree(surface) {
            return;
        }
        self.dirty = true;
        if let Some(generation) = self.geometry.checked_add(1) {
            self.geometry = generation;
        } else {
            self.error = Some("Wayland geometry identity exhausted");
        }
    }
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("owned client")
            .compositor
    }
    fn new_surface(&mut self, surface: &WlSurface) {
        self.surfaces += 1;
        if self.surfaces > MAX_SURFACES {
            surface.post_error(0u32, "hosted surface limit exceeded");
            return;
        }
        self.scales.created(surface);
        smithay::wayland::compositor::add_pre_commit_hook::<Self, _>(
            surface,
            |state, _, surface| state.scene.note_commit(surface),
        );
    }
    fn destroyed(&mut self, surface: &WlSurface) {
        if self.cursor.committed(surface).is_err() {
            self.error = Some("cursor revision exhausted");
        }
        self.dirty |= self
            .cursor
            .surface()
            .is_some_and(|root| super::scene::belongs_to(surface, root));
        if self.cursor.surface() == Some(surface) {
            self.cursor_image(&self.seat.clone(), CursorImageStatus::default_named());
        }
        self.surfaces = self.surfaces.saturating_sub(1);
        let rendered = self.scene.has_tree(surface);
        self.scene.remove(surface);
        self.popups.destroyed(surface);
        self.dirty |= rendered;
        if self
            .root
            .as_ref()
            .is_some_and(|root| root.wl_surface() == surface)
        {
            self.retire_root_request_class();
            self.popups.dismiss_all();
            self.root = None;
            self.frame = None;
            self.last_frame = None;
            self.scene.clear_rendered();
            self.configured = false;
            self.mapped = false;
        }
    }
    fn commit(&mut self, surface: &WlSurface) {
        let single_commit = self.scene.single_commit(surface);
        let (buffer, scale, transform, offset, callbacks, damage) =
            with_states(surface, |states| {
                let mut cached = states.cached_state.get::<SurfaceAttributes>();
                let attrs = cached.current();
                (
                    attrs.buffer.take(),
                    attrs.buffer_scale,
                    attrs.buffer_transform,
                    attrs.buffer_delta.take(),
                    std::mem::take(&mut attrs.frame_callbacks),
                    std::mem::take(&mut attrs.damage),
                )
            });
        let root = self
            .root
            .as_ref()
            .is_some_and(|root| root.wl_surface() == surface);
        let removed = buffer
            .as_ref()
            .is_some_and(|assignment| matches!(assignment, BufferAssignment::Removed));
        let popup = smithay::wayland::compositor::get_role(surface)
            == Some(smithay::wayland::shell::xdg::XDG_POPUP_ROLE);
        if popup
            && !self.popups.prepare_commit(
                surface,
                matches!(&buffer, Some(BufferAssignment::NewBuffer(_))),
                removed,
            )
        {
            if let Some(BufferAssignment::NewBuffer(buffer)) = buffer {
                buffer.release();
            }
            self.dirty = true;
            return;
        }
        if root && !self.configured && !removed {
            if let Some(BufferAssignment::NewBuffer(buffer)) = buffer {
                buffer.release();
            }
            return;
        }
        if self.scene.callbacks(surface, callbacks).is_err() {
            surface.post_error(0u32, "hosted frame callback limit exceeded");
            return;
        }
        let cursor_role = smithay::wayland::compositor::get_role(surface)
            == Some(smithay::wayland::seat::CURSOR_IMAGE_ROLE);
        let offset_ok = offset.is_none_or(|offset| {
            if cursor_role {
                super::cursor::Cursor::offset(surface, [offset.x, offset.y]).is_ok()
            } else {
                offset.x == 0 && offset.y == 0
            }
        });
        match buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                let frame = if offset_ok {
                    self.read_buffer(surface, &buffer, scale, transform, &damage, single_commit)
                } else {
                    Err(invalid("unsupported hosted surface offset"))
                };
                buffer.release();
                match frame {
                    Ok((frame, layout)) => self.scene.update(surface, Some(frame), Some(layout)),
                    Err(_) => surface.post_error(
                        wl_surface::Error::InvalidSize,
                        "unsupported or oversized hosted SHM buffer",
                    ),
                }
            }
            Some(BufferAssignment::Removed) => {
                self.scene.update(surface, None, None);
                if root {
                    self.popups.dismiss_all();
                    self.frame = None;
                    self.last_frame = None;
                    self.mapped = false;
                    self.configured = false;
                    self.scene.clear_rendered();
                    let Some(generation) = self.geometry.checked_add(1) else {
                        self.error = Some("Wayland geometry identity exhausted");
                        return;
                    };
                    self.geometry = generation;
                    self.configure = self.root.as_ref().map(|root| root.send_configure());
                }
            }
            None => {
                if !offset_ok || self.scene.remap(surface, scale, transform.into()).is_err() {
                    surface.post_error(
                        wl_surface::Error::InvalidSize,
                        "unsupported hosted surface mapping",
                    );
                }
            }
        }
        if self.cursor.committed(surface).is_err() {
            surface.post_error(
                wl_surface::Error::InvalidSize,
                "invalid hosted cursor state",
            );
        }
        self.dirty = true;
    }
}
impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.shell
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if self
            .root
            .as_ref()
            .is_some_and(|root| root.wl_surface() == surface.wl_surface())
        {
            self.retire_root_request_class();
            self.failed |= self.root_failed();
            self.popups.dismiss_all();
            self.root = None;
            self.frame = None;
            self.last_frame = None;
            self.scene.clear_rendered();
            self.configured = false;
            self.mapped = false;
        }
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.failed |= self.root_failed();
        if self.root.is_some() {
            surface.send_close();
            return;
        }
        let Some(serial) = self.root_serial.checked_add(1) else {
            self.error = Some("Wayland root identity exhausted");
            return;
        };
        self.root_serial = serial;
        self.root_request_class = surface.wl_surface().client().and_then(|client| {
            client.get_data::<ClientState>().map(|data| {
                data.root_class.store(true, Ordering::Release);
                data.root_class.clone()
            })
        });
        self.root_failure = surface.wl_surface().client().and_then(|client| {
            client
                .get_data::<ClientState>()
                .map(|data| data.failed.clone())
        });
        surface.with_pending_state(|state| {
            state.size = Some((self.size[0] as i32, self.size[1] as i32).into());
        });
        self.configure = Some(surface.send_configure());
        self.output.enter(surface.wl_surface());
        self.configured = false;
        self.root = Some(surface);
    }
    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        if let Configure::Popup(ref popup) = configure {
            self.popups.ack(&surface, popup.serial);
        }
        if self
            .root
            .as_ref()
            .is_some_and(|root| root.wl_surface() == &surface)
            && let Configure::Toplevel(configure) = configure
            && self
                .configure
                .is_some_and(|required| configure.serial >= required)
        {
            self.configured = true;
        }
    }
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        if self
            .popups
            .add(
                surface.clone(),
                positioner,
                self.root.as_ref().map(|root| root.wl_surface()),
            )
            .is_err()
        {
            surface.wl_surface().post_error(
                wl_surface::Error::InvalidSize,
                "popup exceeds hosted bounds",
            );
        }
    }
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        if let Some(root) = &self.root {
            self.popups.grab(
                &surface,
                serial,
                root.wl_surface(),
                self.root_serial,
                Seat::<Self>::from_resource(&seat).as_ref() == Some(&self.seat),
            );
        } else {
            surface.send_popup_done();
        }
        self.dirty = true;
    }
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        if self.popups.reposition(&surface, positioner, token).is_err() {
            surface.wl_surface().post_error(
                wl_surface::Error::InvalidSize,
                "popup reposition exceeds hosted bounds",
            );
        }
        self.dirty = true;
    }
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popups.destroyed(surface.wl_surface());
        self.scene.remove(surface.wl_surface());
        self.dirty = true;
        if let Some(geometry) = self.geometry.checked_add(1) {
            self.geometry = geometry;
        } else {
            self.error = Some("Wayland geometry identity exhausted");
        }
    }
}
impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seats
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focus: Option<&WlSurface>) {
        self.clipboard_focus(seat, focus);
        self.text_input.focus(focus);
    }
    fn cursor_image(&mut self, _: &Seat<Self>, status: CursorImageStatus) {
        let surface = match &status {
            CursorImageStatus::Surface(surface) => Some(surface.clone()),
            _ => None,
        };
        let oversized = surface
            .as_ref()
            .and_then(|surface| self.scene.frame(surface))
            .is_some_and(|frame| {
                frame.width > MAX_CURSOR_DIMENSION || frame.height > MAX_CURSOR_DIMENSION
            });
        if oversized {
            if let Some(surface) = surface {
                surface.post_error(
                    wl_surface::Error::InvalidSize,
                    "cursor dimensions exceed hosted bounds",
                );
            }
            return;
        }
        match self.cursor.change(status) {
            Ok(changed) => self.dirty |= changed,
            Err(_) => {
                if let Some(surface) = surface {
                    surface.post_error(
                        wl_surface::Error::InvalidSize,
                        "cursor hotspot exceeds hosted bounds",
                    );
                } else {
                    self.error = Some("cursor revision exhausted");
                }
            }
        }
    }
}
delegate_compositor!(State);
delegate_viewporter!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_seat!(State);
impl OutputHandler for State {}
delegate_output!(State);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resize_keeps_only_the_current_output_mode() {
        let display = Display::<State>::new().unwrap();
        let mut state = State::new(&display.handle(), 64, 48).unwrap();
        for width in 65..200 {
            state.resize(width, 48, SCALE_DENOMINATOR).unwrap();
            assert_eq!(
                state.output.modes().len(),
                1,
                "resize retained old output modes"
            );
        }
    }
    #[test]
    fn fractional_output_modes_round_toplevel_extents_without_accumulating_old_modes() {
        let display = Display::<State>::new().unwrap();
        let mut state = State::new(&display.handle(), 65, 49).unwrap();
        for (units, physical) in [(150, [81, 61]), (180, [98, 74]), (120, [65, 49])] {
            state.resize(65, 49, units).unwrap();
            let mode = state.output.current_mode().unwrap();
            assert_eq!([mode.size.w, mode.size.h], physical);
            assert_eq!(state.output.modes().len(), 1);
            assert_eq!(
                state.output.current_scale().fractional_scale(),
                f64::from(units) / 120.
            );
        }
    }
}
