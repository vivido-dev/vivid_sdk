//! Run a production GUI application inside an authenticated Vivid desktop surface.
//!
//! The application owns one process and one GUI window. Control/input and media
//! use independent workers, bounded mailboxes, and the existing Vivid transports.

// Rust guideline compliant 2026-10-07
pub mod clipboard;
mod input;
mod keyboard;
mod local;
mod reconnect;
mod recovery;
pub mod state;
#[cfg(test)]
mod tests;
mod transport;
#[doc(inline)]
pub use input::hid_usage;
#[doc(inline)]
pub use keyboard::KeyRepeat;

use crate::{
    InputBindingGuard, InputEvent, ProducerConfig,
    gui::{self, Bounds, Entity, Render, Window, WindowBounds, WindowOptions},
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// Keep the CPU-copy correctness path within a 2048-square physical RGBA frame
// on a 2x native display, below both the GPU and Vivid record-body ceilings.
/// Maximum logical extent of the initial bounded CPU-copy backend.
pub const MAX_LOGICAL_DIMENSION: u32 = 1024;
const MAX_PHYSICAL_DIMENSION: u32 = 2048;
const INPUT_CAPACITY: usize = 64;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Reads private GUI launch authority without copying it into command arguments.
///
/// A complete `VIVID_GUI_LEASE_CONTEXT`, `VIVID_GUI_LEASE_ID` and
/// `VIVID_GUI_LEASE_SECRET` environment selects lease activation. With none set,
/// the ordinary root discovery contract applies. Partial credentials fail closed.
///
/// # Errors
/// Returns an error for malformed, partial or non-Unicode lease credentials.
pub fn config_from_env() -> io::Result<ProducerConfig> {
    config_from_launch_environment(|name| match std::env::var(name) {
        Ok(value) => Ok(Some(zeroize::Zeroizing::new(value))),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid GUI lease environment",
        )),
    })
}

fn config_from_launch_environment(
    mut read: impl FnMut(&str) -> io::Result<Option<zeroize::Zeroizing<String>>>,
) -> io::Result<ProducerConfig> {
    let context = read("VIVID_GUI_LEASE_CONTEXT")?;
    let lease = read("VIVID_GUI_LEASE_ID")?;
    let secret = read("VIVID_GUI_LEASE_SECRET")?;
    let mut config = ProducerConfig::desktop();
    match (context, lease, secret) {
        (None, None, None) => (),
        (Some(context), Some(lease), Some(secret)) => {
            let number = |value: &str| {
                if value.is_empty()
                    || value.len() > 20
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid GUI lease identity",
                    ));
                }
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value != 0)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid GUI lease identity")
                    })
            };
            config.authentication = crate::ProducerAuthentication::lease_activation_hex(
                number(&context)?,
                number(&lease)?,
                &secret,
            )?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "incomplete GUI lease credentials",
            ));
        }
    }
    Ok(config)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    connection: u64,
    generation: u64,
    width: u32,
    height: u32,
}

struct Frame {
    geometry: Geometry,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct Injection {
    lifecycle: Option<Arc<crate::session::SessionLifecycle>>,
    guard: InputBindingGuard,
    events: VecDeque<InputEvent>,
    reset: u64,
    origin: Instant,
    clipboard: Arc<clipboard::Gate>,
}

impl Injection {
    fn now(&self) -> vivid_protocol::time::Monotonic {
        vivid_protocol::time::Monotonic::from_micros(
            u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX),
        )
    }
    fn revoke(&mut self) {
        self.clipboard.revoke();
        self.guard.release();
        self.events.clear();
        self.reset = self
            .reset
            .checked_add(1)
            .expect("remote input reset generation exhausted");
    }
}

type Cancel = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    geometry: Mutex<Option<Geometry>>,
    frame: Mutex<Option<Frame>>,
    media: Mutex<Option<transport::MediaJob>>,
    injection: Mutex<Injection>,
    stopped: AtomicBool,
    media_id: AtomicU64,
    cancel: Mutex<Option<(u64, Cancel)>>,
    media_failure: Mutex<Option<(u64, io::ErrorKind)>>,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            geometry: Mutex::new(None),
            frame: Mutex::new(None),
            media: Mutex::new(None),
            injection: Mutex::new(Injection {
                lifecycle: None,
                guard: InputBindingGuard::new(),
                events: VecDeque::new(),
                reset: 0,
                origin: Instant::now(),
                clipboard: Arc::new(clipboard::Gate::default()),
            }),
            stopped: AtomicBool::new(false),
            media_id: AtomicU64::new(0),
            cancel: Mutex::new(None),
            media_failure: Mutex::new(None),
            error: Mutex::new(None),
        }
    }
    fn stop(&self, error: Option<String>) {
        if let Some(error) = error {
            *self.error.lock().expect("error mutex") = Some(error);
        }
        self.stopped.store(true, Ordering::Release);
        let cancel = self
            .cancel
            .lock()
            .expect("cancel mutex")
            .as_ref()
            .map(|(_, cancel)| cancel.clone());
        if let Some(cancel) = cancel {
            cancel();
        }
    }
    fn disconnect(&self) {
        self.media_id.store(0, Ordering::Release);
        *self.geometry.lock().expect("geometry mutex") = None;
        *self.frame.lock().expect("frame mutex") = None;
        *self.media.lock().expect("media mutex") = None;
        {
            let mut injection = self.injection.lock().expect("input mutex");
            injection.revoke();
            injection.lifecycle = None;
        }
        let cancel = self.cancel.lock().expect("cancel mutex").take();
        if let Some((_, cancel)) = cancel {
            cancel();
        }
    }
}

/// Runs one GUI root against a Vivid desktop presenter.
///
/// The caller runs on the native main thread. Credentials use `ProducerConfig`;
/// no authority is sent through arguments, diagnostics, or a terminal PTY.
/// `build` should establish the root's initial focus just as in a standalone app.
/// macOS native termination may exit the process rather than return from `run`.
/// This initial backend carries physical keys and pointer input, not IME services.
/// Producer-owned repeat uses `KeyRepeat` launch policy; invalid policy fails
/// before starting any transport or GUI resources.
/// Root-authenticated connections recover transient loss with fresh sessions and
/// retained view state. Activated leases recover the same logical session using
/// bounded reconciliation, exact uncertain-create receipts and fresh channels.
/// A prepared activation or resume attempt retries its exact handshake only
/// until confirmation. Confirmed activation authority is never reused, and a
/// leased session never falls back to root authentication. Startup with resume
/// authority alone is unsupported because the retained graph lives in memory.
///
/// # Errors
/// Returns an error for invalid repeat policy, non-live configuration, startup/runtime failure, fatal
/// protocol/authority errors, or exhausted reconnect admission budget.
///
/// # Abort
/// Native GUI failure can terminate the application process with status 1.
/// On macOS native quit can exit the process instead of returning from this call.
pub fn run<V: Render + 'static>(
    config: ProducerConfig,
    build: impl FnOnce(&mut Window, &mut gui::App) -> Entity<V> + 'static,
) -> io::Result<()> {
    let repeat = KeyRepeat::from_env()?;
    let shared = Arc::new(Shared::new());
    let gate = shared
        .injection
        .lock()
        .expect("input mutex")
        .clipboard
        .clone();
    let (clipboard, clipboard_service) = clipboard::Producer::from_env(gate)?;
    let control_state = shared.clone();
    let control = std::thread::Builder::new()
        .name("vivid-gui-control".into())
        .spawn(move || {
            let result = reconnect::control(&control_state, config);
            control_state.stop(
                result
                    .err()
                    .filter(|_| !control_state.stopped.load(Ordering::Acquire))
                    .map(|_| "Remote GUI connection ended".to_owned()),
            );
        })?;
    let media_state = shared.clone();
    let media = match std::thread::Builder::new()
        .name("vivid-gui-media".into())
        .spawn(move || transport::media(&media_state))
    {
        Ok(worker) => worker,
        Err(error) => {
            shared.stop(None);
            let _ = control.join();
            return Err(error);
        }
    };
    let app_state = shared.clone();
    crate::gui_platform::headless().run(move |cx| {
        cx.set_hosted_clipboard(std::rc::Rc::new(clipboard));
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    gui::size(gui::px(640.), gui::px(400.)),
                ))),
                show: false,
                focus: false,
                ..Default::default()
            },
            build,
        );
        let handle: gui::AnyWindowHandle = match opened {
            Ok(window) => window.into(),
            Err(_) => {
                app_state.stop(Some("Remote GUI window failed".into()));
                std::process::exit(1);
            }
        };
        cx.spawn(async move |cx| {
            let mut geometry = None;
            let mut last_pixels = None;
            let mut render_revision = 0;
            let mut input = input::Translator::new(repeat);
            while !app_state.stopped.load(Ordering::Acquire) {
                cx.background_executor().timer(FRAME_INTERVAL).await;
                let requested = *app_state.geometry.lock().expect("geometry mutex");
                let changed = geometry != requested;
                let result = handle.update(cx, |_, window, cx| -> io::Result<()> {
                    window.advance_offscreen_frame(cx);
                    {
                        // Serialize final dispatch with revocation and target changes.
                        // A blocked application callback stalls this app process only.
                        let mut injection = app_state.injection.lock().expect("input mutex");
                        if changed {
                            injection.revoke();
                        }
                        input.apply(&mut injection, window, cx);
                    }
                    let Some(requested) = requested else {
                        geometry = None;
                        last_pixels = None;
                        render_revision = 0;
                        return Ok(());
                    };
                    if changed {
                        window.resize(gui::size(
                            gui::px(requested.width as f32),
                            gui::px(requested.height as f32),
                        ));
                        window.bounds_changed(cx);
                        geometry = Some(requested);
                    }
                    let arena = window.needs_draw().then(|| window.draw(cx));
                    if !changed && render_revision == window.render_revision() {
                        if let Some(arena) = arena {
                            arena.clear(cx);
                        }
                        return Ok(());
                    }
                    let image = window.render_to_image();
                    if let Some(arena) = arena {
                        arena.clear(cx);
                    }
                    render_revision = window.render_revision();
                    let image = image.map_err(io::Error::other)?;
                    if image.width() > MAX_PHYSICAL_DIMENSION
                        || image.height() > MAX_PHYSICAL_DIMENSION
                    {
                        return Err(io::Error::other("remote GUI frame exceeds physical bounds"));
                    }
                    if !changed
                        && last_pixels
                            .as_ref()
                            .is_some_and(|old: &Vec<u8>| old == image.as_raw())
                    {
                        return Ok(());
                    }
                    last_pixels = Some(image.as_raw().clone());
                    *app_state.frame.lock().expect("frame mutex") = Some(Frame {
                        geometry: requested,
                        width: image.width(),
                        height: image.height(),
                        rgba: image.into_raw(),
                    });
                    Ok(())
                });
                if !matches!(result, Ok(Ok(()))) {
                    app_state.stop(Some("Remote GUI rendering failed".into()));
                }
            }
            // Release the old target's held state before ending the native loop.
            let _ = handle.update(cx, |_, window, cx| {
                let mut injection = app_state.injection.lock().expect("input mutex");
                injection.revoke();
                input.apply(&mut injection, window, cx);
            });
            if app_state.error.lock().expect("error mutex").is_some() {
                std::process::exit(1);
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    shared.stop(None);
    drop(clipboard_service);
    let _ = control.join();
    let _ = media.join();
    let error = shared.error.lock().expect("error mutex").take();
    error.map_or(Ok(()), |error| Err(io::Error::other(error)))
}

/// Runs a GUI root with optional process-scoped durable state capture.
///
/// The host supplies a private state endpoint through the child environment.
/// With no endpoint, `build` receives no restoration state and no state worker
/// starts. `build` validates its own schema before creating the view; `capture`
/// runs on the GUI thread and must return owned bounded data without blocking.
/// The state channel does not carry Vivid authentication or input authority.
///
/// # Errors
/// Returns launch-state/transport errors or the errors documented by `run`.
///
/// # Abort
/// Invalid app state, failed root construction or failed state worker setup
/// exits the process with status 1. Native GUI quit may also exit the process.
pub fn run_persistent<V: Render + 'static>(
    config: ProducerConfig,
    build: impl FnOnce(
        Option<&crate::gui_host::AppState>,
        &mut Window,
        &mut gui::App,
    ) -> io::Result<Entity<V>>
    + 'static,
    capture: impl Fn(&V, &gui::App) -> io::Result<Option<crate::gui_host::AppState>> + 'static,
) -> io::Result<()> {
    let mut connection = state::Connection::from_env()?;
    // Retire native I/O after the GUI loop returns, outside GUI callbacks.
    let service = std::rc::Rc::new(std::cell::RefCell::new(None));
    let setup = service.clone();
    let result = run(config, move |window, cx| {
        let initial = connection
            .as_ref()
            .and_then(|connection| connection.initial.as_ref());
        let view = match build(initial, window, cx) {
            Ok(view) => view,
            Err(_) => std::process::exit(1),
        };
        if let Some(mut connection) = connection.take() {
            connection.initial.take();
            match connection.attach(view.clone(), capture, cx) {
                Ok(worker) => *setup.borrow_mut() = Some(worker),
                Err(_) => std::process::exit(1),
            }
        }
        view
    });
    drop(service.borrow_mut().take());
    result
}
