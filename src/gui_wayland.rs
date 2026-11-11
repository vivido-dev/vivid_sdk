//! A nested Linux Wayland display for one hosted SHM application.
//!
//! The caller owns dispatch and child supervision. This transport grants no Vivid
//! authority. The backend supports one xdg toplevel, subsurfaces, bounded popups,
//! keyboard/pointer input and CPU copies. Clipboard globals are opt-in through
//! [`WaylandHost::with_clipboard`]. DMA-BUF is not advertised.

// Rust guideline compliant 2026-10-07
mod clipboard;
mod cursor;
mod dispatch;
mod popups;
mod readback;
mod scale;
mod scene;
mod shapes;
mod state;
mod text;
mod viewport;
#[doc(inline)]
pub use cursor::{CursorImage, CursorSurface, MAX_CURSOR_DIMENSION, MAX_CURSOR_HOTSPOT};
#[doc(inline)]
pub use dispatch::DispatchControl;
#[doc(inline)]
pub use readback::ReadbackStats;
#[doc(inline)]
pub use scale::{MAX_OUTPUT_SCALE, SCALE_DENOMINATOR};
#[doc(inline)]
pub use text::{MAX_IME_BYTES, TextContext, TextEdit, TextInputState, TextTarget};

use smithay::{
    backend::input::{Axis, ButtonState, KeyState},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::{
        Display, ListeningSocket, Resource as _, backend::DisconnectReason,
    },
    utils::SERIAL_COUNTER,
};
use std::{
    collections::BTreeSet,
    fmt, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

/// Largest supported client buffer extent on either axis.
pub const MAX_DIMENSION: u32 = 2048;
/// Maximum mapped size of one native SHM pool, including unused storage.
pub const MAX_SHM_POOL_BYTES: usize = 64 * 1024 * 1024;
/// Maximum mapped SHM storage shared by all clients of one hosted display.
pub const MAX_SHM_BYTES: usize = 128 * 1024 * 1024;
/// Maximum live SHM backing pools, including pools retained by buffers.
// Allows two backing pools for each of the supported 32 surfaces.
pub const MAX_SHM_POOLS: usize = 64;
/// Maximum retained SHM buffer descriptors per hosted display.
pub const MAX_SHM_BUFFERS: usize = 256;
/// Maximum retained protocol-ID slots per native connection, including retired IDs.
pub const MAX_PROTOCOL_OBJECT_SLOTS: usize = 2048;
/// Maximum descriptors in each incoming/outgoing transport queue per connection.
// Allows several ancillary batches while bounding unused descriptors across reads.
pub const MAX_BUFFERED_DESCRIPTORS: usize = 64;
/// Sustained request rate per connection and protected root group.
pub const REQUESTS_PER_SECOND: u32 = 16_384;
/// Maximum immediate request burst per connection and protected root group.
pub const REQUEST_BURST: u32 = 32_768;
/// Sustained declared wire-byte rate per connection and protected root group.
pub const REQUEST_BYTES_PER_SECOND: u32 = 2 * 1024 * 1024;
/// Maximum immediate declared wire-byte burst per connection and root group.
pub const REQUEST_BYTE_BURST: u32 = 4 * 1024 * 1024;
/// Maximum add/subtract operations retained by a native region before assignment.
pub const MAX_REGION_OPERATIONS: usize = 64;
/// Maximum damage rectangles across a surface's pending, cached and current frontend state.
pub const MAX_DAMAGE_RECTANGLES: usize = 256;
/// Maximum callbacks across a surface's pending, cached and current frontend state.
pub const MAX_PENDING_CALLBACKS: usize = 64;
/// Maximum queued snapshots per surface and cached frontend state type.
pub const MAX_CACHED_SURFACE_STATES: usize = 64;
// The server accepts one app and bounded helper connections, not a public seat.
const MAX_CLIENTS: usize = 8;

/// An opaque root and geometry identity issued by one display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    host: [u8; 16],
    root: u64,
    geometry: u64,
}

/// Owned packed RGBA pixels awaiting the host's presentation acknowledgement.
#[derive(Clone)]
pub struct Frame {
    /// Cursor snapshot to paint with this delivery, scoped to `target`.
    pub cursor: CursorImage,
    /// Monotonic delivery identity, local to this display.
    pub id: u64,
    /// Original input target; never retag input for a replacement root.
    pub target: Target,
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Surface-local aspect and dimensions for painting and pointer coordinates.
    pub logical_size: [u32; 2],
    /// Packed premultiplied RGBA pixels.
    pub rgba: Arc<[u8]>,
}
impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("size", &[self.width, self.height])
            .field("logical_size", &self.logical_size)
            .finish_non_exhaustive()
    }
}

/// Physical input in the current surface's local coordinates.
#[derive(Clone, Copy, Debug)]
pub enum Input {
    /// Linux evdev key code, without the XKB offset of eight.
    Key { code: u32, pressed: bool },
    /// Absolute surface-local pointer location.
    Motion { x: f64, y: f64 },
    /// Leaves the painted canvas without changing keyboard focus.
    Leave,
    /// Linux BTN_LEFT through BTN_EXTRA.
    Button { code: u32, pressed: bool },
    /// Pixel-like horizontal and vertical scroll distances.
    Axis { horizontal: f64, vertical: f64 },
    /// Dismisses native popups after a click in the pane's letterbox space.
    DismissPopups,
}

/// A private nested display with explicit dispatch and presentation.
pub struct WaylandHost {
    display: Display<state::State>,
    listener: ListeningSocket,
    directory: tempfile::TempDir,
    socket: PathBuf,
    state: state::State,
    clients: Vec<dispatch::Connection>,
    root_budget: Arc<wayland_backend::server::RequestBudget>,
    helper_budget: Arc<wayland_backend::server::RequestBudget>,
    watchdog: dispatch::Watchdog,
    next_client: usize,
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
    active: bool,
    presented: Option<Target>,
    pending: Option<(
        u64,
        Target,
        Vec<smithay::reexports::wayland_server::protocol::wl_callback::WlCallback>,
    )>,
    next_frame: u64,
    origin: Instant,
}

impl fmt::Debug for WaylandHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaylandHost")
            .field("active", &self.active)
            .field("clients", &self.clients.len())
            .finish_non_exhaustive()
    }
}

impl WaylandHost {
    /// Returns completed importer work without resetting diagnostic counters.
    pub fn readback_stats(&self) -> ReadbackStats {
        self.state.readback_stats
    }
    /// Returns a cancellation handle scoped to this display lifetime.
    pub fn dispatch_control(&self) -> DispatchControl {
        self.watchdog.control()
    }
    fn cancelled(&self) -> bool {
        self.watchdog.is_cancelled()
    }
    /// Returns the current presented editor's bounded native IME metadata.
    pub fn text_input(&self) -> Option<TextInputState> {
        let target = self.state.target()?;
        (self.active && !self.cancelled() && self.presented == Some(target)).then_some(())?;
        self.state
            .text_input
            .snapshot(target, &self.state.scene, self.state.logical_size)
    }
    /// Delivers one atomic text edit to its original focused editor.
    ///
    /// Returns false when focus, editor or presented mapping authority is stale.
    ///
    /// # Errors
    /// Returns an error for excess text, invalid UTF-8 ranges or unsupported replacement.
    pub fn text_edit(&mut self, target: TextTarget, edit: TextEdit) -> io::Result<bool> {
        if !self.active
            || self.cancelled()
            || self.presented != Some(target.root)
            || self.state.target() != Some(target.root)
        {
            return Ok(false);
        }
        self.state.text_input.edit(target, edit)
    }
    /// Creates a display with scoped UTF-8 clipboard and optional primary selection.
    ///
    /// The platform owner must activate, service and revoke the supplied channel.
    /// Native transfers never block dispatch and require current presentation/focus.
    ///
    /// # Errors
    /// Returns an error for invalid size or unavailable native display resources.
    pub fn with_clipboard(
        width: u32,
        height: u32,
        client: crate::gui_host::clipboard::Client,
    ) -> io::Result<Self> {
        let mut host = Self::new(width, height)?;
        host.state.install_clipboard(&host.display.handle(), client);
        Ok(host)
    }
    /// Creates an owner-only display without changing process environment.
    ///
    /// # Errors
    /// Returns an error for invalid size, unavailable XKB/Pixman, socket/thread
    /// setup, or a system server backend without isolated client dispatch.
    pub fn new(width: u32, height: u32) -> io::Result<Self> {
        extent(width, height)?;
        // The system backend dispatches every client even through the single-
        // client API. Refuse it before a watchdog could target the wrong owner.
        if std::any::TypeId::of::<wayland_backend::server::Backend<state::State>>()
            != std::any::TypeId::of::<wayland_backend::rs::server::Backend<state::State>>()
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "nested Wayland requires the Rust server backend",
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("vivid-wayland-")
            .tempdir()?;
        let socket = directory.path().join("wayland-0");
        let listener = ListeningSocket::bind_absolute(socket.clone()).map_err(io::Error::other)?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let display = Display::new().map_err(io::Error::other)?;
        let state = state::State::new(&display.handle(), width, height)?;
        Ok(Self {
            display,
            listener,
            directory,
            socket,
            state,
            clients: vec![],
            root_budget: Arc::new(wayland_backend::server::RequestBudget::new(
                state::request_limits(false),
            )),
            helper_budget: Arc::new(wayland_backend::server::RequestBudget::new(
                state::request_limits(true),
            )),
            watchdog: dispatch::Watchdog::new()?,
            next_client: 0,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            active: false,
            presented: None,
            pending: None,
            next_frame: 0,
            origin: Instant::now(),
        })
    }

    /// Returns the absolute socket path for the child's `WAYLAND_DISPLAY`.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Returns the private directory for the child's `XDG_RUNTIME_DIR`.
    pub fn runtime_directory(&self) -> &Path {
        self.directory.path()
    }

    /// Returns a current input snapshot only after presentation and activation.
    pub fn input_target(&self) -> Option<(Target, [u32; 2])> {
        let target = self.state.target()?;
        (self.active && !self.cancelled() && self.presented == Some(target))
            .then_some((target, self.state.logical_size))
    }

    /// Reports whether the current root owns client pixels, independently of focus.
    pub fn is_mapped(&self) -> bool {
        self.state.mapped
    }

    /// Reports a protocol failure by a client that owned the current root.
    ///
    /// The failure remains recorded after root destruction or replacement.
    /// Helper-only failures do not set it. Supervisors can distinguish protocol
    /// rejection from a clean child exit and retire this host lifetime.
    pub fn root_failed(&self) -> bool {
        self.state.root_failed()
    }

    /// Dispatches available clients and returns at most one owned frame.
    ///
    /// Until that frame is acknowledged, newer client pixels replace one retained
    /// snapshot. Hidden views must keep dispatching but withhold acknowledgements.
    ///
    /// # Errors
    /// Returns an error for display I/O, rendering failure, exhausted identities,
    /// root dispatch timeout, or permanent cancellation (`Interrupted`).
    pub fn dispatch(&mut self) -> io::Result<Option<Frame>> {
        if self.cancelled() {
            self.revoke();
            self.presented = None;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "native dispatch cancelled",
            ));
        }
        // A host-generated protocol error can retire an idle connection. Run
        // backend destructors before removing its last tracked dispatch handle.
        for connection in &self.clients {
            if connection
                .client
                .get_data::<state::ClientState>()
                .is_some_and(|data| data.closed.load(std::sync::atomic::Ordering::Acquire))
            {
                let _ = self
                    .display
                    .backend()
                    .dispatch_single_client(&mut self.state, connection.client.id());
            }
        }
        self.clients.retain(|connection| {
            connection
                .client
                .get_data::<state::ClientState>()
                .is_some_and(|data| !data.closed.load(std::sync::atomic::Ordering::Acquire))
        });
        for _ in 0..MAX_CLIENTS {
            let Some(stream) = self.listener.accept()? else {
                break;
            };
            if self.clients.len() >= MAX_CLIENTS {
                continue;
            }
            let socket = Arc::new(stream.try_clone()?);
            let client = self
                .display
                .handle()
                .insert_client(
                    stream,
                    Arc::new(state::ClientState::with_budgets(
                        self.root_budget.clone(),
                        self.helper_budget.clone(),
                    )),
                )
                .map_err(io::Error::other)?;
            self.clients.push(dispatch::Connection { client, socket });
        }
        let count = self.clients.len();
        let first = self.next_client;
        let root = self
            .state
            .root
            .as_ref()
            .and_then(|root| root.wl_surface().client());
        let priority = root.as_ref().and_then(|root| {
            self.clients
                .iter()
                .position(|connection| connection.client.id() == root.id())
        });
        let order = priority.into_iter().chain(
            (0..count)
                .map(|offset| (first + offset) % count)
                .filter(|index| Some(*index) != priority),
        );
        let started = Instant::now();
        for index in order {
            let connection = &self.clients[index];
            if !connection.ready()? {
                continue;
            }
            let root = self
                .state
                .root
                .as_ref()
                .and_then(|root| root.wl_surface().client())
                .is_some_and(|client| client.id() == connection.client.id());
            let guard = self.watchdog.arm(connection.socket.clone())?;
            // Per-client errors already disconnect/clean that client's resources.
            // Do not turn an unrelated helper's EOF into failure of the root.
            let _ = self
                .display
                .backend()
                .dispatch_single_client(&mut self.state, connection.client.id());
            let expired = guard.finish();
            if self.cancelled() {
                self.state.refresh();
                self.revoke();
                self.presented = None;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "native dispatch cancelled",
                ));
            }
            if expired && root {
                self.state.refresh();
                self.revoke();
                self.presented = None;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native root exceeded its dispatch budget",
                ));
            }
            self.next_client = (index + 1) % count;
            // Preserve a turn for input/supervision rather than draining every
            // ready helper after one of them has consumed a complete quantum.
            if started.elapsed() >= dispatch::MAX_DISPATCH_TIME {
                break;
            }
        }
        if self.cancelled() {
            self.revoke();
            self.presented = None;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "native dispatch cancelled",
            ));
        }
        self.state.refresh();
        self.display.flush_clients().map_err(io::Error::other)?;
        if self
            .presented
            .is_some_and(|target| Some(target) != self.state.target())
        {
            self.revoke();
            self.presented = None;
        }
        if let Some(error) = self.state.error.take() {
            return Err(io::Error::other(error));
        }
        self.state.poll_clipboard();
        // An unmap or replacement root retires its callbacks. A resize keeps
        // the same mapped root and must preserve its outstanding frame clock.
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !self.state.mapped || self.state.root_serial != pending.1.root)
        {
            self.pending = None;
        }
        if self.pending.is_some() {
            return Ok(None);
        }
        let Some(mut frame) = self.state.frame.take() else {
            return Ok(None);
        };
        self.next_frame = self
            .next_frame
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Wayland frame identity exhausted"))?;
        frame.id = self.next_frame;
        let cursor_visible = self.active
            && self.presented == Some(frame.target)
            && self
                .state
                .seat
                .get_pointer()
                .expect("pointer")
                .current_focus()
                .is_some();
        frame.cursor = if cursor_visible {
            self.state.cursor.snapshot(&self.state.scene)
        } else {
            CursorImage::Default
        };
        let cursor_surface = matches!(frame.cursor, CursorImage::Surface(_));
        self.pending = Some((
            frame.id,
            frame.target,
            self.state.take_callbacks(cursor_surface),
        ));
        Ok(Some(frame))
    }

    /// Acknowledges pixels after the embedding host actually rendered them.
    ///
    /// Returns false for old deliveries, geometry changes, and foreign roots.
    /// A matching delivery still releases its frame callbacks after resize;
    /// that acknowledgement never enables input for the retired geometry.
    pub fn presented(&mut self, id: u64, target: Target) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.0 == id && pending.1 == target)
        {
            return false;
        }
        let (_, _, callbacks) = self.pending.take().expect("checked pending frame");
        let time = self.origin.elapsed().as_millis() as u32;
        for callback in callbacks {
            callback.done(time);
        }
        if self.state.target() != Some(target) || self.cancelled() {
            return false;
        }
        self.presented = Some(target);
        self.sync_focus();
        true
    }

    /// Revokes held input before changing activation.
    pub fn set_active(&mut self, active: bool) {
        let active = active && !self.cancelled();
        if self.active != active {
            if !active {
                self.state.dismiss_popups();
            }
            self.revoke();
            self.active = active;
            self.sync_focus();
        }
    }

    /// Requests a new tiled size and invalidates old presentation/input.
    ///
    /// # Errors
    /// Returns an error for invalid size or exhausted geometry identities.
    pub fn resize(&mut self, width: u32, height: u32) -> io::Result<()> {
        self.resize_with_scale(width, height, self.state.scales.units)
    }

    /// Changes tiled geometry and output scale as one validated transition.
    ///
    /// `scale_120` uses 120 units per scale one, from one through four. Logical
    /// dimensions must leave room for rounded-up integer client buffers within
    /// [`MAX_DIMENSION`]. A scale change requires fresh configure/presentation
    /// before input, even when logical dimensions stay unchanged.
    ///
    /// # Errors
    /// Returns an error for invalid geometry/scale or exhausted geometry identities.
    /// Invalid requests leave the existing presentation and input intact.
    pub fn resize_with_scale(&mut self, width: u32, height: u32, scale_120: u32) -> io::Result<()> {
        scale::physical(width, height, scale_120)?;
        if self.state.size == [width, height] && self.state.scales.units == scale_120 {
            return Ok(());
        }
        self.state.dismiss_popups();
        self.revoke();
        self.presented = None;
        self.state.resize(width, height, scale_120)
    }

    /// Delivers a transition only to its original presented and active root.
    ///
    /// # Errors
    /// Returns an error for malformed key codes, geometry, or scroll magnitude.
    pub fn input(&mut self, target: Target, event: Input) -> io::Result<bool> {
        if !self.active
            || self.cancelled()
            || self.presented != Some(target)
            || self.state.target() != Some(target)
        {
            return Ok(false);
        }
        let time = self.origin.elapsed().as_millis() as u32;
        match event {
            Input::DismissPopups => {
                if self.state.dismiss_popups() {
                    self.state.refresh();
                    self.revoke();
                    self.presented = None;
                }
            }
            Input::Leave => {
                let pointer = self.state.seat.get_pointer().expect("pointer");
                pointer.motion(
                    &mut self.state,
                    None,
                    &MotionEvent {
                        location: (0., 0.).into(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                pointer.frame(&mut self.state);
            }
            Input::Key { code, pressed } => {
                if code > 255 {
                    return Err(invalid("unsupported evdev key"));
                }
                if if pressed {
                    !self.keys.insert(code)
                } else {
                    !self.keys.remove(&code)
                } {
                    return Ok(false);
                }
                self.key(code, pressed, time);
            }
            Input::Motion { x, y } => {
                let size = self.state.logical_size;
                if !x.is_finite()
                    || !y.is_finite()
                    || x < 0.
                    || y < 0.
                    || x >= f64::from(size[0])
                    || y >= f64::from(size[1])
                {
                    return Err(invalid("pointer outside Wayland surface"));
                }
                let pointer = self.state.seat.get_pointer().expect("pointer");
                let focus = self
                    .state
                    .hit(x, y)
                    .map(|(surface, origin)| (surface, origin.into()));
                pointer.motion(
                    &mut self.state,
                    focus,
                    &MotionEvent {
                        location: (x, y).into(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                pointer.frame(&mut self.state);
            }
            Input::Button { code, pressed } => {
                if !(0x110..=0x114).contains(&code) {
                    return Err(invalid("unsupported pointer button"));
                }
                if pressed
                    && self
                        .state
                        .seat
                        .get_pointer()
                        .expect("pointer")
                        .current_focus()
                        .is_none()
                    && self.state.popups.has_grabs()
                {
                    self.state.dismiss_popups();
                    self.state.refresh();
                    self.revoke();
                    self.presented = None;
                    return Ok(false);
                }
                if if pressed {
                    !self.buttons.insert(code)
                } else {
                    !self.buttons.remove(&code)
                } {
                    return Ok(false);
                }
                self.button(code, pressed, time);
            }
            Input::Axis {
                horizontal,
                vertical,
            } => {
                if !horizontal.is_finite()
                    || !vertical.is_finite()
                    || horizontal.abs() > 10_000.
                    || vertical.abs() > 10_000.
                {
                    return Err(invalid("invalid Wayland scroll"));
                }
                let pointer = self.state.seat.get_pointer().expect("pointer");
                pointer.axis(
                    &mut self.state,
                    AxisFrame::new(time)
                        .value(Axis::Horizontal, horizontal)
                        .value(Axis::Vertical, vertical),
                );
                pointer.frame(&mut self.state);
            }
        }
        Ok(true)
    }

    fn key(&mut self, code: u32, pressed: bool, time: u32) {
        let keyboard = self.state.seat.get_keyboard().expect("keyboard");
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(clipboard) = &mut self.state.clipboard {
            clipboard.note_serial(serial);
        }
        if pressed {
            self.state
                .popups
                .note_press(serial, self.state.root_serial, keyboard.current_focus());
        }
        keyboard.input::<(), _>(
            &mut self.state,
            (code + 8).into(),
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }
    fn button(&mut self, code: u32, pressed: bool, time: u32) {
        let pointer = self.state.seat.get_pointer().expect("pointer");
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(clipboard) = &mut self.state.clipboard {
            clipboard.note_serial(serial);
        }
        if pressed {
            self.state
                .popups
                .note_press(serial, self.state.root_serial, pointer.current_focus());
        }
        pointer.button(
            &mut self.state,
            &ButtonEvent {
                button: code,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
                serial,
                time,
            },
        );
        pointer.frame(&mut self.state);
    }
    fn revoke(&mut self) {
        let time = self.origin.elapsed().as_millis() as u32;
        let pointer = self.state.seat.get_pointer().expect("pointer");
        if !self.buttons.is_empty() {
            // Wayland has no cancelled-button event. Move a captured gesture
            // outside the root before balancing it, so default release-based
            // widgets do not activate merely because the pane lost focus.
            pointer.motion(
                &mut self.state,
                None,
                &MotionEvent {
                    location: (-1., -1.).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
        }
        for code in std::mem::take(&mut self.buttons) {
            self.button(code, false, time);
        }
        for code in std::mem::take(&mut self.keys) {
            self.key(code, false, time);
        }
        let keyboard = self.state.seat.get_keyboard().expect("keyboard");
        keyboard.set_focus(&mut self.state, None, SERIAL_COUNTER.next_serial());
        let pointer = self.state.seat.get_pointer().expect("pointer");
        pointer.unset_grab(&mut self.state, SERIAL_COUNTER.next_serial(), time);
        pointer.motion(
            &mut self.state,
            None,
            &MotionEvent {
                location: (0., 0.).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(&mut self.state);
    }
    fn sync_focus(&mut self) {
        let focus = (self.active
            && !self.cancelled()
            && self.presented.is_some()
            && self.presented == self.state.target())
        .then(|| self.state.keyboard_focus())
        .flatten();
        let keyboard = self.state.seat.get_keyboard().expect("keyboard");
        let entered = focus.is_some() && keyboard.current_focus() != focus;
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(&mut self.state, focus, serial);
        if entered && let Some(clipboard) = &mut self.state.clipboard {
            clipboard.note_serial(serial);
        }
    }
}

impl Drop for WaylandHost {
    fn drop(&mut self) {
        self.revoke();
        for connection in &self.clients {
            let _ = connection.socket.shutdown(std::net::Shutdown::Both);
            self.display
                .handle()
                .backend_handle()
                .kill_client(connection.client.id(), DisconnectReason::ConnectionClosed);
        }
        let _ = self.display.flush_clients();
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn extent(width: u32, height: u32) -> io::Result<()> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(invalid("Wayland extent exceeds bounds"));
    }
    Ok(())
}
