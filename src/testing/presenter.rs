//! An in-process Vivid 1.5 presenter for producer tests.
//!
//! It runs the real authentication transcript, the real record framing, and the real track-channel
//! and interactive-lane handshakes, so a test observes exactly what a producer puts on the wire.
//! It is not a conformant presenter: it validates what a regression depends on, and it will
//! cheerfully do things a real presenter must not — that is what [`Script`] is for.
//!
//! Every producer in this tree tests against this one presenter. A private copy per producer is
//! how three crates end up with three different notions of correct, which is the failure mode this
//! module exists to prevent.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use vivid_protocol::auth::{self, Secret32};
use vivid_protocol::cbor::Value;
use vivid_protocol::messages::{
    self, Envelope, ErrorDetail, ErrorReply, Hello, HelloAuthentication, PayloadMap, Welcome,
    WelcomeAuthentication,
};
use vivid_protocol::registry;
use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::wire::{HEADER_SIZE, PREFACE_SIZE, Preface, Record, RecordHeader};

use crate::testing::script::Script;

/// The one session this presenter serves. Overlay state is session-scoped, because a real
/// presenter serves many panes; here there is only ever this one.
const TEST_SESSION_ID: u64 = 1;

/// Root secret the harness accepts. Tests place it in `VIVID_ROOT_SECRET`.
pub const ROOT_SECRET_HEX: &str =
    "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

/// Which target this presenter presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A terminal grid, in columns and rows.
    Terminal { columns: u64, rows: u64 },
    /// A desktop virtual rectangle, in logical pixels.
    Desktop { width: u32, height: u32 },
}

impl TargetKind {
    fn profile(self) -> &'static str {
        match self {
            Self::Terminal { .. } => registry::TERMINAL_SURFACE,
            Self::Desktop { .. } => registry::DESKTOP_SURFACE,
        }
    }

    /// The overlay viewport this target implies, in logical pixels. Terminal cells are given a
    /// fixed 8x16 size so a test's geometry is arithmetic rather than font-dependent.
    fn overlay_width(self) -> u32 {
        match self {
            Self::Terminal { columns, .. } => (columns as u32).saturating_mul(8).max(1),
            Self::Desktop { width, .. } => width.max(1),
        }
    }

    fn overlay_height(self) -> u32 {
        match self {
            Self::Terminal { rows, .. } => (rows as u32).saturating_mul(16).max(1),
            Self::Desktop { height, .. } => height.max(1),
        }
    }
}

/// A surface's immutable fields, retained from `CREATE_SURFACE` so `QUERY_SURFACE` can echo them
/// back byte-identical, exactly as a real presenter must.
#[derive(Debug, Clone)]
struct SurfaceImmutableFields {
    semantic_profile: String,
    coordinate_model: u64,
    logical_width: u64,
    logical_height: u64,
    scale_numerator: u64,
    scale_denominator: u64,
    rotation: u64,
}

/// One control-connection request the presenter observed, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub record_type: u16,
    pub object_id: u64,
    pub payload: PayloadMap,
}

/// What a track connection did, recorded so a test can assert transport ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackChannelLog {
    pub track_id: u64,
    pub channel_generation: u64,
    /// Media records accepted before the peer closed.
    pub media_records: u64,
}

/// One observed `DESTROY_TRACK`, with the evidence a relay would use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestroyObservation {
    pub track_id: u64,
    /// True when the producer closed the track transport before the ordered destroy was serviced.
    /// A relay that sees this removes the track on EOF and then rejects the destroy.
    pub closed_before_destroy: bool,
}

#[derive(Default)]
struct Shared {
    observed: Vec<Observed>,
    channels: Vec<TrackChannelLog>,
    /// Track IDs whose transport reached EOF, in arrival order.
    closed_channels: Vec<u64>,
    destroys: Vec<DestroyObservation>,
    /// Maximum media-record body granted per track, echoed back on CHANNEL_ACCEPTED.
    track_bodies: HashMap<u64, u32>,
    /// Interactive lane generations accepted, in arrival order.
    lanes: Vec<u64>,
    /// Input bindings observed on the interactive lane, in arrival order.
    input_bindings: Vec<ObservedBinding>,
    /// Headless overlay windows, scenes and assets. Empty unless a producer negotiates them.
    overlays: crate::testing::overlay::Overlays,
    /// Sessions that negotiated the overlay bundle. An overlay record on any other lane would
    /// be a record that lane never agreed to carry.
    overlay_sessions: std::collections::HashSet<u64>,
}

/// One `SET_INPUT_BINDING` the presenter answered, with the grant it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedBinding {
    pub producer_epoch: u64,
    pub context_id: u64,
    pub surface_id: u64,
    pub surface_generation: u64,
    pub requested_classes: u64,
    /// Classes the presenter actually granted, after the script's denials.
    pub effective_classes: u64,
    pub grant_generation: u64,
    /// 0 disabled, 1 enabled, 2 denied.
    pub state: u64,
}

/// The presentation target, as the presenter's own truth rather than what a producer has read.
#[derive(Debug, Clone, Copy)]
struct Target {
    generation: u64,
    kind: TargetKind,
}

impl Target {
    /// The descriptor for this target's profile, with the settle flag applied.
    fn descriptor(&self, settled: bool) -> PayloadMap {
        match self.kind {
            TargetKind::Terminal { columns, rows } => {
                let mut payload = terminal_descriptor(columns, rows);
                payload[6].1 = Value::Bool(settled);
                payload
            }
            TargetKind::Desktop { width, height } => {
                let mut payload = desktop_descriptor(width, height, self.generation);
                payload[5].1 = Value::Bool(settled);
                payload
            }
        }
    }
}

/// An in-process presenter serving one Vivid session.
pub struct TestPresenter {
    endpoint: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    control_shutdown: Arc<Mutex<Option<TcpStream>>>,
    control_writer: Arc<Mutex<Option<TcpStream>>>,
    sequence: Arc<Mutex<u64>>,
    target: Arc<Mutex<Target>>,
    script: Script,
    /// The interactive lane's writer and its own record sequence, separate from control's.
    lane_writer: Arc<Mutex<Option<(TcpStream, u64)>>>,
    join: Option<JoinHandle<io::Result<()>>>,
}

impl TestPresenter {
    /// Start a presenter with the given terminal grid.
    ///
    /// Each session gets its own numeric ID space, which lets two owners deliberately reuse the
    /// same object numbers — the only way to catch a scoped-identity bug.
    pub fn start(columns: u64, rows: u64) -> io::Result<Self> {
        Self::start_with(TargetKind::Terminal { columns, rows }, Script::new())
    }

    /// Start a presenter presenting a desktop of the given logical size.
    pub fn start_desktop(width: u32, height: u32) -> io::Result<Self> {
        Self::start_with(TargetKind::Desktop { width, height }, Script::new())
    }

    /// Start a presenter with a target and an armed fault script.
    pub fn start_with(kind: TargetKind, script: Script) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("tcp:{}", listener.local_addr()?);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let control_shutdown = Arc::new(Mutex::new(None));
        let control_writer = Arc::new(Mutex::new(None));
        let sequence = Arc::new(Mutex::new(0));
        let target = Arc::new(Mutex::new(Target {
            generation: 1,
            kind,
        }));
        let lane_writer = Arc::new(Mutex::new(None));

        let join = {
            let shared = shared.clone();
            let stop = stop.clone();
            let control_shutdown = control_shutdown.clone();
            let control_writer = control_writer.clone();
            let sequence = sequence.clone();
            let target = target.clone();
            let script = script.clone();
            let lane_writer = lane_writer.clone();
            thread::Builder::new()
                .name("test-presenter".to_owned())
                .spawn(move || {
                    serve(Serving {
                        listener,
                        shared,
                        stop,
                        control_shutdown,
                        control_writer,
                        sequence,
                        target,
                        script,
                        lane_writer,
                    })
                })?
        };

        Ok(Self {
            endpoint,
            shared,
            stop,
            control_shutdown,
            control_writer,
            sequence,
            target,
            script,
            lane_writer,
            join: Some(join),
        })
    }

    /// The fault script this presenter consults. Faults may be armed at any time.
    pub fn script(&self) -> &Script {
        &self.script
    }

    /// Input bindings this presenter observed on the interactive lane, in arrival order.
    pub fn input_bindings(&self) -> Vec<ObservedBinding> {
        self.shared.lock().expect("shared").input_bindings.clone()
    }

    /// Interactive lane generations this presenter accepted, in arrival order.
    pub fn lanes(&self) -> Vec<u64> {
        self.shared.lock().expect("shared").lanes.clone()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn observed(&self) -> Vec<Observed> {
        self.shared.lock().expect("shared").observed.clone()
    }

    pub fn channels(&self) -> Vec<TrackChannelLog> {
        self.shared.lock().expect("shared").channels.clone()
    }

    pub fn destroys(&self) -> Vec<DestroyObservation> {
        self.shared.lock().expect("shared").destroys.clone()
    }

    /// Move the presentation target and announce it, exactly as a terminal resize does.
    ///
    /// The generation advances before the announcement is written, so a commit already planned
    /// against the previous target is rejected as stale until the producer catches up.
    pub fn change_target(&self, kind: TargetKind, settled: bool) -> io::Result<()> {
        if self.script.state.lock().expect("script").target_frozen() {
            return Ok(());
        }
        let target = {
            let mut guard = self.target.lock().expect("target");
            guard.generation += 1;
            guard.kind = kind;
            *guard
        };
        let mut payload = target.descriptor(settled);
        payload.push((9, Value::Unsigned(target.generation)));
        payload.push((10, Value::Unsigned(0)));
        let body = Envelope::new(0, payload)
            .encode()
            .map_err(io::Error::other)?;
        self.push(messages::TARGET_CHANGED, 0, &body)
    }

    /// Resize a terminal target, the common case for a terminal producer's regressions.
    pub fn resize_terminal(&self, columns: u64, rows: u64, settled: bool) -> io::Result<()> {
        self.change_target(TargetKind::Terminal { columns, rows }, settled)
    }

    /// Revoke the current input grant, as focus loss or local policy would.
    pub fn revoke_input(&self, reason: u64) -> io::Result<()> {
        let binding = self
            .shared
            .lock()
            .expect("shared")
            .input_bindings
            .last()
            .copied()
            .ok_or_else(|| io::Error::other("no input grant to revoke"))?;
        let body = Envelope::new(
            0,
            vec![
                (0, Value::Unsigned(binding.producer_epoch)),
                (1, Value::Unsigned(binding.grant_generation)),
                (2, Value::Unsigned(binding.context_id)),
                (3, Value::Unsigned(binding.surface_id)),
                (4, Value::Unsigned(binding.surface_generation)),
                (5, Value::Unsigned(reason)),
            ],
        )
        .encode()
        .map_err(io::Error::other)?;
        self.push_lane(messages::INPUT_REVOKED, binding.surface_id, &body)
    }

    /// Push an uncorrelated `TRACK_LOST` for a complete owner identity.
    pub fn lose_track(&self, context_id: u64, surface_id: u64, track_id: u64) -> io::Result<()> {
        let body = Envelope::new(
            0,
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(surface_id)),
                (2, Value::Unsigned(track_id)),
                (3, Value::Unsigned(registry::error::DECODER)),
                (4, Value::Unsigned(2)),
                (5, Value::Map(Vec::new())),
                (6, Value::Text("decoder failed".to_owned())),
            ],
        )
        .encode()
        .map_err(io::Error::other)?;
        self.push(messages::TRACK_LOST, track_id, &body)
    }

    // ---- headless overlays -----------------------------------------------------------------

    /// Every display list this presenter accepted, keyed by the window that published it.
    pub fn overlay_scenes(&self) -> Vec<crate::testing::overlay::PresentedScene> {
        self.shared.lock().expect("shared").overlays.scenes()
    }

    /// Retained images still reachable by a future scene.
    pub fn overlay_assets(&self) -> Vec<crate::testing::overlay::RetainedAsset> {
        self.shared.lock().expect("shared").overlays.assets()
    }

    /// The environment this presenter reports to a producer.
    pub fn overlay_environment(&self) -> vivid_protocol::overlay::wire::Environment {
        self.shared
            .lock()
            .expect("shared")
            .overlays
            .environment()
            .clone()
    }

    /// Change the environment, as a font or theme change would.
    pub fn set_overlay_environment(&self, environment: vivid_protocol::overlay::wire::Environment) {
        {
            let mut guard = self.shared.lock().expect("shared");
            guard.overlays.set_environment(environment);
        }
        flush_overlay_lane(&self.shared, &self.lane_writer);
    }

    /// Clipboard text this presenter accepted, in acceptance order. A refused write leaves no
    /// trace here, so an assertion on this is an assertion that the guard allowed it.
    pub fn overlay_clipboard(&self) -> Vec<String> {
        self.shared
            .lock()
            .expect("shared")
            .overlays
            .clipboard()
            .to_vec()
    }

    /// The semantic tree this presenter holds for the only window that has one, as the
    /// accessibility adapter would read it.
    pub fn overlay_semantics(&self) -> Option<vivid_protocol::overlay::Semantics> {
        let guard = self.shared.lock().expect("shared");
        guard
            .overlays
            .windows_with_semantics()
            .into_iter()
            .next()
            .map(|(_, s)| s.clone())
    }

    /// Where the focused editor last said its caret was, with the revision it said it at.
    pub fn overlay_editor_caret(&self) -> Option<(u64, Option<vivid_protocol::vector::Rect>)> {
        self.shared.lock().expect("shared").overlays.editor_caret()
    }

    /// Deliver an assistive-technology action for a node, exactly as the adapter would.
    pub fn overlay_accessibility_action(
        &self,
        node: u64,
        action: vivid_protocol::overlay::AccessibleAction,
    ) -> io::Result<bool> {
        let queued = {
            let guard = self.shared.lock().expect("shared");
            let Some((window, _)) = guard.overlays.windows_with_semantics().into_iter().next()
            else {
                return Ok(false);
            };
            let window = *window;
            drop(guard);
            let mut guard = self.shared.lock().expect("shared");
            guard.overlays.queue_accessibility(window, node, action)
        };
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(queued)
    }

    /// The cursor the currently hovered overlay region asks for, as the display layer sees it.
    pub fn overlay_cursor(&self) -> Option<vivid_protocol::vector::CursorShape> {
        self.shared.lock().expect("shared").overlays.cursor()
    }

    /// Whether any overlay window currently holds focus.
    pub fn overlay_focused(&self) -> bool {
        self.shared
            .lock()
            .expect("shared")
            .overlays
            .focused()
            .is_some()
    }

    /// Replace the overlay viewport, as a pane resize or DPI change would.
    pub fn set_overlay_viewport(&self, width: f64, height: f64, scale: u32) -> io::Result<()> {
        {
            let mut guard = self.shared.lock().expect("shared");
            let viewport = vivid_protocol::overlay::wire::Viewport {
                width: vivid_protocol::vector::Scalar::new(width).map_err(io::Error::other)?,
                height: vivid_protocol::vector::Scalar::new(height).map_err(io::Error::other)?,
                scale_numerator: scale.max(1),
                scale_denominator: 1,
            };
            // This presenter serves one session, so its viewport is that session's.
            guard.overlays.set_viewport(TEST_SESSION_ID, viewport);
        }
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(())
    }

    /// Deliver a pointer event in viewport logical pixels. `button` is `(button, pressed)`.
    pub fn overlay_pointer(
        &self,
        x: f64,
        y: f64,
        button: Option<(u16, bool)>,
        modifiers: u32,
    ) -> io::Result<bool> {
        let position = vivid_protocol::vector::Point::new(x, y).map_err(io::Error::other)?;
        let consumed = self.shared.lock().expect("shared").overlays.pointer(
            &[TEST_SESSION_ID],
            position,
            button,
            modifiers,
            None,
        );
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Deliver a wheel delta in viewport logical pixels.
    pub fn overlay_wheel(
        &self,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        modifiers: u32,
    ) -> io::Result<bool> {
        let position = vivid_protocol::vector::Point::new(x, y).map_err(io::Error::other)?;
        let scroll = vivid_protocol::overlay::Scroll {
            dx: vivid_protocol::vector::Scalar::new(dx).map_err(io::Error::other)?,
            dy: vivid_protocol::vector::Scalar::new(dy).map_err(io::Error::other)?,
            precise: true,
            phase: vivid_protocol::overlay::ScrollPhase::Changed,
        };
        let consumed = self.shared.lock().expect("shared").overlays.wheel(
            &[TEST_SESSION_ID],
            position,
            scroll,
            modifiers,
        );
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Deliver a pressure sample, as a device that has one reports.
    pub fn overlay_pressure(&self, x: f64, y: f64, pressure: f64) -> io::Result<bool> {
        let position = vivid_protocol::vector::Point::new(x, y).map_err(io::Error::other)?;
        let consumed = self.shared.lock().expect("shared").overlays.pointer(
            &[TEST_SESSION_ID],
            position,
            None,
            0,
            Some(vivid_protocol::vector::Scalar::new(pressure).map_err(io::Error::other)?),
        );
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Deliver a composition from an input method.
    pub fn overlay_ime(&self, preedit: &str, selection: Option<(u32, u32)>) -> io::Result<bool> {
        let event = vivid_protocol::overlay::Event::Ime {
            preedit: preedit.to_owned(),
            selection,
        };
        let consumed =
            self.shared
                .lock()
                .expect("shared")
                .overlays
                .keyboard(&[TEST_SESSION_ID], event, false);
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Deliver a physical key transition. `usage` is a USB HID keyboard-page usage.
    pub fn overlay_key(&self, usage: u32, down: bool, modifiers: u32) -> io::Result<bool> {
        let event = vivid_protocol::overlay::Event::Key {
            physical: usage,
            down,
            repeat: false,
            modifiers,
        };
        // Escape dismissal is the host's policy, and it keys off the usage rather than the text.
        let escape = usage == 0x29;
        let consumed = self.shared.lock().expect("shared").overlays.keyboard(
            &[TEST_SESSION_ID],
            event,
            escape,
        );
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Deliver committed text, as a platform text-input path would after a key or an IME commit.
    pub fn overlay_text(&self, text: &str) -> io::Result<bool> {
        let event = vivid_protocol::overlay::Event::Text(text.to_owned());
        let consumed =
            self.shared
                .lock()
                .expect("shared")
                .overlays
                .keyboard(&[TEST_SESSION_ID], event, false);
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(consumed)
    }

    /// Gain or lose native pane focus, which cancels gestures and held input state.
    pub fn set_overlay_pane_focus(&self, focused: bool) -> io::Result<()> {
        self.shared
            .lock()
            .expect("shared")
            .overlays
            .set_pane_focus(&[TEST_SESSION_ID], focused);
        flush_overlay_lane(&self.shared, &self.lane_writer);
        Ok(())
    }

    /// Write an unsolicited record on the interactive lane.
    fn push_lane(&self, record_type: u16, object_id: u64, body: &[u8]) -> io::Result<()> {
        let mut guard = self.lane_writer.lock().expect("lane writer");
        let (stream, sequence) = guard
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no interactive lane"))?;
        *sequence += 1;
        write_record(stream, *sequence, record_type, 0, object_id, body)
    }

    fn push(&self, record_type: u16, object_id: u64, body: &[u8]) -> io::Result<()> {
        let mut guard = self.control_writer.lock().expect("control writer");
        let stream = guard
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no control connection"))?;
        let mut sequence = self.sequence.lock().expect("sequence");
        *sequence += 1;
        write_record(stream, *sequence, record_type, 0, object_id, body)
    }
}

impl Drop for TestPresenter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake an accepted connection even when it has not completed the handshake yet. The
        // serving thread installs this clone while holding the same mutex and checks `stop` before
        // doing so, which closes the race between this take and its handoff.
        let waiting_for_accept = match self.control_shutdown.lock() {
            Ok(mut guard) => match guard.take() {
                Some(stream) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    false
                }
                None => true,
            },
            Err(_) => true,
        };
        // The accept loop wakes on its own connection, so a probe connect unblocks it.
        // Do not reconnect to a listener already being torn down after an accepted handshake.
        // On Windows that connect can wait longer than the entire shutdown budget.
        if waiting_for_accept
            && let Some(address) = self
                .endpoint
                .strip_prefix("tcp:")
                .and_then(|s| s.parse().ok())
        {
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(100));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn write_record(
    stream: &mut TcpStream,
    sequence: u64,
    record_type: u16,
    flags: u16,
    object_id: u64,
    body: &[u8],
) -> io::Result<()> {
    let header = RecordHeader {
        body_length: u32::try_from(body.len()).map_err(io::Error::other)?,
        record_type,
        flags,
        object_id,
        sequence,
    };
    stream.write_all(&header.encode())?;
    stream.write_all(body)?;
    stream.flush()
}

fn read_record(stream: &mut impl Read) -> io::Result<Record> {
    let mut header = [0_u8; HEADER_SIZE];
    stream.read_exact(&mut header)?;
    let header = RecordHeader::decode(header);
    let mut body = vec![0_u8; header.body_length as usize];
    stream.read_exact(&mut body)?;
    Ok(Record {
        record_type: header.record_type,
        flags: header.flags,
        object_id: header.object_id,
        sequence: header.sequence,
        body,
    })
}

/// Poll cancellation only during establishment. Some Windows socket providers do not wake an
/// already-blocked read reliably when another cloned handle calls shutdown.
struct HandshakeReader<'a> {
    stream: &'a mut TcpStream,
    stop: &'a AtomicBool,
}
impl Read for HandshakeReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "test presenter stopped during handshake",
                ));
            }
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
}

fn read_preface(stream: &mut TcpStream) -> io::Result<Preface> {
    let mut bytes = [0_u8; PREFACE_SIZE];
    stream.read_exact(&mut bytes)?;
    Preface::decode(bytes)
}

/// Everything one serving thread owns. A struct rather than eight arguments, because the lane and
/// track acceptors need most of it too.
struct Serving {
    listener: TcpListener,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    control_shutdown: Arc<Mutex<Option<TcpStream>>>,
    control_writer: Arc<Mutex<Option<TcpStream>>>,
    sequence: Arc<Mutex<u64>>,
    target: Arc<Mutex<Target>>,
    script: Script,
    lane_writer: Arc<Mutex<Option<(TcpStream, u64)>>>,
}

struct ControlShutdownGuard(Arc<Mutex<Option<TcpStream>>>);

impl Drop for ControlShutdownGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.0.lock() {
            guard.take();
        }
    }
}

fn serve(serving: Serving) -> io::Result<()> {
    let Serving {
        listener,
        shared,
        stop,
        control_shutdown,
        control_writer,
        sequence,
        target,
        script,
        lane_writer,
    } = serving;
    let initial_target = *target.lock().expect("target");
    let address = listener.local_addr()?;
    let (mut control, _) = listener.accept()?;
    {
        let mut guard = control_shutdown.lock().expect("control shutdown");
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        *guard = Some(control.try_clone()?);
    }
    let _control_shutdown_guard = ControlShutdownGuard(control_shutdown);
    // A short read timeout makes incomplete establishment cancellable on every platform. Remove
    // it after HELLO: an ordinary idle established session must not be abandoned on a timeout.
    control.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut handshake = HandshakeReader {
        stream: &mut control,
        stop: &stop,
    };
    let mut preface_bytes = [0_u8; PREFACE_SIZE];
    handshake.read_exact(&mut preface_bytes)?;
    let _ = Preface::decode(preface_bytes)?;

    let hello_record = read_record(&mut handshake)?;
    control.set_read_timeout(None)?;
    if hello_record.record_type != messages::HELLO {
        return Err(io::Error::other("first control record was not HELLO"));
    }
    let (hello_request, hello) = Hello::decode(&hello_record.body).map_err(io::Error::other)?;
    let root = Secret32::from_hex(ROOT_SECRET_HEX).map_err(io::Error::other)?;
    let HelloAuthentication::Root { proof } = &hello.authentication else {
        return Err(io::Error::other("expected root authentication"));
    };
    let authless = hello.authless_payload().map_err(io::Error::other)?;
    if !auth::verify_root_hello_proof(&root, &preface_bytes, &authless, proof) {
        return Err(io::Error::other("root HELLO proof did not verify"));
    }

    let server_nonce = [7_u8; 32];
    let prk = auth::extract_handshake_prk(&root, &hello.client_nonce, &server_nonce, &[0; 32]);
    let mut accepted_profiles = hello.required_profiles.clone();
    accepted_profiles.extend(hello.optional_profiles.iter().cloned());
    accepted_profiles.sort();
    accepted_profiles.dedup();
    // The producer's requested target profile has to match what this presenter presents; a
    // producer negotiating a desktop against a terminal presenter is a test-setup error, and
    // silently answering with the wrong descriptor would hide it.
    if hello.target_profile != initial_target.kind.profile() {
        return Err(io::Error::other(format!(
            "producer requested target profile {:?} but this presenter presents {:?}",
            hello.target_profile,
            initial_target.kind.profile()
        )));
    }
    let mut welcome = Welcome {
        session_id: TEST_SESSION_ID,
        session_tag: [3; messages::SESSION_TAG_BYTES],
        root_context_id: 1,
        target_generation: 1,
        target_profile: hello.target_profile.clone(),
        target_descriptor: initial_target.descriptor(true),
        accepted_profiles,
        maximum_control_body: vivid_protocol::CONTROL_MAX_RECORD_BODY,
        server_nonce,
        authentication: WelcomeAuthentication {
            kind: messages::AUTHENTICATION_ROOT,
            confirmation: [0; 32],
            lease_state: 0,
            activation_attempt_status: 0,
        },
        session_revision: 1,
        scene_revision: 1,
        resource_contract: generous_contract(),
        establishment_state: 0,
        resume_generation: 0,
        extensions: Vec::new(),
    };
    if welcome
        .accepted_profiles
        .iter()
        .any(|profile| profile == registry::VECTOR_SCENE)
    {
        let mut guard = shared.lock().expect("shared");
        guard.overlay_sessions.insert(welcome.session_id);
        guard.overlays.install_viewport(
            welcome.session_id,
            f64::from(initial_target.kind.overlay_width()),
            f64::from(initial_target.kind.overlay_height()),
        );
        welcome
            .extensions
            .push((15, guard.overlays.limits().to_value()));
    }
    welcome.confirm(&prk).map_err(io::Error::other)?;
    let welcome_body = welcome.encode(hello_request).map_err(io::Error::other)?;
    // Install the control writer before WELCOME can reach the client: a producer returns from
    // connect the moment it processes WELCOME, so anything it pushes afterwards must find the
    // writer already in place even when this thread is preempted between the two steps.
    *control_writer.lock().expect("control writer") = Some(control.try_clone()?);
    {
        let mut guard = sequence.lock().expect("sequence");
        *guard += 1;
        write_record(&mut control, *guard, messages::WELCOME, 0, 0, &welcome_body)?;
    }

    let channel_key = {
        let (keys, _) = auth::derive_session_keys(
            &prk,
            welcome.session_id,
            welcome.resume_generation,
            &welcome.session_tag,
        );
        Secret32::new(*keys.channel_key())
    };

    // Track connections arrive on the same endpoint; a dedicated acceptor keeps the control loop
    // responsive while a producer opens a channel mid-request.
    let acceptor = {
        let shared = shared.clone();
        let stop = stop.clone();
        let channel_key = Secret32::new(*channel_key.expose());
        let script = script.clone();
        let lane_writer = lane_writer.clone();
        thread::spawn(move || {
            accept_secondary_connections(listener, shared, stop, channel_key, script, lane_writer)
        })
    };

    let mut scene_revision = 1_u64;
    let mut surface_revisions: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
    // The immutable fields a real presenter's `QUERY_SURFACE` reply must keep byte-identical to
    // what the producer created the surface with.
    let mut surface_definitions: HashMap<(u64, u64), SurfaceImmutableFields> = HashMap::new();
    let mut track_revisions: HashMap<u64, u64> = HashMap::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let record = match read_record(&mut control) {
            Ok(record) => record,
            Err(_) => break,
        };
        let envelope = messages::decode_control(&record.body).map_err(io::Error::other)?;
        let request_id = envelope.request_id;
        let payload = envelope.payload.clone();
        shared.lock().expect("shared").observed.push(Observed {
            record_type: record.record_type,
            object_id: record.object_id,
            payload: payload.clone(),
        });
        let unsigned = |key: u64| {
            payload
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .and_then(|(_, value)| value.as_u64())
        };
        let text = |key: u64| {
            payload
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .and_then(|(_, value)| match value {
                    Value::Text(text) => Some(text.clone()),
                    _ => None,
                })
        };

        let (reply_type, reply_body) = match record.record_type {
            messages::CREATE_SURFACE => {
                let key = (unsigned(0).unwrap_or(0), unsigned(1).unwrap_or(0));
                surface_revisions.insert(key, (1, 1));
                surface_definitions.insert(
                    key,
                    SurfaceImmutableFields {
                        semantic_profile: text(2)
                            .unwrap_or_else(|| crate::GENERIC_CONTENT.to_owned()),
                        coordinate_model: unsigned(3).unwrap_or(1),
                        logical_width: unsigned(4).unwrap_or(1),
                        logical_height: unsigned(5).unwrap_or(1),
                        scale_numerator: unsigned(6).unwrap_or(1),
                        scale_denominator: unsigned(7).unwrap_or(1),
                        rotation: unsigned(8).unwrap_or(0),
                    },
                );
                (
                    messages::SURFACE_READY,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(key.0)),
                            (1, Value::Unsigned(key.1)),
                            (2, Value::Unsigned(1)),
                            (3, Value::Unsigned(1)),
                            (4, Value::Unsigned(unsigned(10).unwrap_or(0))),
                            (5, Value::Map(Vec::new())),
                        ],
                    )?,
                )
            }
            messages::UPDATE_SURFACE => {
                let key = (unsigned(0).unwrap_or(0), unsigned(1).unwrap_or(0));
                if let Some(state) = surface_revisions.get_mut(&key) {
                    state.0 += 1;
                }
                (messages::OK, messages::ok(request_id))
            }
            messages::QUERY_SURFACE => {
                let key = (unsigned(0).unwrap_or(0), unsigned(1).unwrap_or(0));
                let (revision, generation) = surface_revisions.get(&key).copied().unwrap_or((1, 1));
                let fields = surface_definitions.get(&key).cloned().unwrap_or_else(|| {
                    SurfaceImmutableFields {
                        semantic_profile: crate::GENERIC_CONTENT.to_owned(),
                        coordinate_model: 1,
                        logical_width: 1,
                        logical_height: 1,
                        scale_numerator: 1,
                        scale_denominator: 1,
                        rotation: 0,
                    }
                });
                (
                    messages::SURFACE_STATUS,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(key.0)),
                            (1, Value::Unsigned(key.1)),
                            (2, Value::Unsigned(revision)),
                            (3, Value::Unsigned(generation)),
                            (4, Value::Text(fields.semantic_profile)),
                            (5, Value::Unsigned(fields.coordinate_model)),
                            (6, Value::Unsigned(fields.logical_width)),
                            (7, Value::Unsigned(fields.logical_height)),
                            (8, Value::Unsigned(fields.scale_numerator)),
                            (9, Value::Unsigned(fields.scale_denominator)),
                            (10, Value::Unsigned(fields.rotation)),
                            (
                                11,
                                Value::Map(vec![
                                    (0, Value::Unsigned(1)),
                                    (1, Value::Text(String::new())),
                                    (2, Value::Unsigned(0)),
                                    (3, Value::Unsigned(0)),
                                    (4, Value::Text(String::new())),
                                ]),
                            ),
                            (12, Value::Unsigned(0)),
                            (13, Value::Map(Vec::new())),
                            (14, Value::Unsigned(1)),
                            (15, Value::Map(Vec::new())),
                        ],
                    )?,
                )
            }
            messages::PROBE_TRACK_CONFIG => (
                messages::TRACK_SUPPORT,
                ok_payload(
                    request_id,
                    vec![
                        (0, Value::Bool(true)),
                        (1, Value::Text("fake-raster".to_owned())),
                        (2, Value::Unsigned(1)),
                        (3, Value::Map(Vec::new())),
                    ],
                )?,
            ),
            messages::CREATE_TRACK => {
                track_revisions.insert(record.object_id, 1);
                let maximum_body = unsigned(7).unwrap_or(1);
                {
                    let mut guard = shared.lock().expect("shared");
                    guard.track_bodies.insert(
                        record.object_id,
                        u32::try_from(maximum_body).unwrap_or(u32::MAX),
                    );
                    guard.overlays.note_track(
                        welcome.session_id,
                        record.object_id,
                        unsigned(0).unwrap_or(0),
                        unsigned(1).unwrap_or(0),
                    );
                }
                let delta_operations = payload
                    .iter()
                    .find(|(key, _)| *key == 12)
                    .and_then(|(_, value)| match value {
                        Value::Map(map) => map
                            .iter()
                            .find(|(key, _)| *key == 5)
                            .and_then(|(_, value)| value.as_u64()),
                        _ => None,
                    })
                    .unwrap_or(0);
                (
                    messages::TRACK_READY,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(unsigned(0).unwrap_or(0))),
                            (1, Value::Unsigned(unsigned(1).unwrap_or(0))),
                            (2, Value::Unsigned(record.object_id)),
                            (3, Value::Unsigned(1)),
                            (4, Value::Unsigned(1)),
                            (5, Value::Unsigned(30_000_000)),
                            (6, Value::Unsigned(maximum_body)),
                            (7, Value::Map(Vec::new())),
                            (8, Value::Bool(true)),
                            (9, Value::Unsigned(delta_operations)),
                        ],
                    )?,
                )
            }
            messages::SET_OVERLAY_EDITOR
            | messages::SET_OVERLAY_WINDOW
            | messages::OVERLAY_ACTION
            | messages::QUERY_OVERLAY
            | messages::SET_OVERLAY_CLIPBOARD
            | messages::SET_OVERLAY_SEMANTICS
            | messages::MEASURE_OVERLAY_TEXT_BATCH
            | messages::RELEASE_OVERLAY_TEXT_LAYOUTS => {
                let mut guard = shared.lock().expect("shared");
                let overlays = &mut guard.overlays;
                let outcome = match record.record_type {
                    messages::SET_OVERLAY_WINDOW => {
                        overlays.set_window(welcome.session_id, record.object_id, &payload)
                    }
                    messages::OVERLAY_ACTION => {
                        overlays.action(welcome.session_id, record.object_id, &payload)
                    }
                    messages::SET_OVERLAY_CLIPBOARD => {
                        overlays.set_clipboard(welcome.session_id, record.object_id, &payload)
                    }
                    messages::SET_OVERLAY_SEMANTICS => {
                        overlays.set_semantics(welcome.session_id, record.object_id, &payload)
                    }
                    messages::SET_OVERLAY_EDITOR => {
                        overlays.set_editor_geometry(welcome.session_id, record.object_id, &payload)
                    }
                    messages::MEASURE_OVERLAY_TEXT_BATCH => {
                        overlays.measure_text_batch(welcome.session_id, record.object_id, &payload)
                    }
                    messages::RELEASE_OVERLAY_TEXT_LAYOUTS => overlays.release_text_layouts(
                        welcome.session_id,
                        record.object_id,
                        &payload,
                    ),
                    _ => overlays.status(welcome.session_id, record.object_id, &payload),
                };
                drop(guard);
                flush_overlay_lane(&shared, &lane_writer);
                overlay_reply(request_id, outcome)?
            }
            messages::WAIT_TRACK => {
                let revision = *track_revisions.get(&record.object_id).unwrap_or(&1);
                (
                    messages::WAIT_SATISFIED,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(unsigned(0).unwrap_or(0))),
                            (1, Value::Unsigned(unsigned(1).unwrap_or(0))),
                            (2, Value::Unsigned(record.object_id)),
                            (3, Value::Unsigned(revision)),
                            (4, Value::Unsigned(unsigned(6).unwrap_or(1))),
                            (5, Value::Unsigned(unsigned(3).unwrap_or(2))),
                            // The observed condition value, exactly as vivido reports it.
                            (6, Value::Unsigned(unsigned(4).unwrap_or(0))),
                        ],
                    )?,
                )
            }
            messages::ACTIVATE_TRACK => {
                let key = (unsigned(0).unwrap_or(0), unsigned(1).unwrap_or(0));
                let revision = surface_revisions
                    .get_mut(&key)
                    .map(|state| {
                        state.0 += 1;
                        state.0
                    })
                    .unwrap_or(2);
                (
                    messages::TRACK_ACTIVATED,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(key.0)),
                            (1, Value::Unsigned(key.1)),
                            (2, Value::Array(Vec::new())),
                            (3, Value::Unsigned(revision)),
                            (4, Value::Unsigned(1)),
                        ],
                    )?,
                )
            }
            messages::ADVANCE_CHANNEL => {
                let revision = track_revisions
                    .entry(record.object_id)
                    .and_modify(|value| *value += 1)
                    .or_insert(2);
                (
                    messages::CHANNEL_ADVANCED,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(unsigned(0).unwrap_or(0))),
                            (1, Value::Unsigned(unsigned(1).unwrap_or(0))),
                            (2, Value::Unsigned(record.object_id)),
                            (3, Value::Unsigned(unsigned(4).unwrap_or(2))),
                            (4, Value::Unsigned(30_000_000)),
                            (5, Value::Unsigned(*revision)),
                        ],
                    )?,
                )
            }
            messages::COMMIT_TXN => {
                // A commit names the target generation it was planned against. The presenter owns
                // the current one, so a commit that crosses a resize is rejected, not applied to
                // a target it was never planned for.
                let generation = target.lock().expect("target").generation;
                if envelope.expected_target_generation != Some(generation) {
                    (
                        messages::ERROR,
                        messages::ErrorReply {
                            code: registry::error::STALE_TARGET_GENERATION,
                            request_id,
                            detail: messages::ErrorDetail::new(Vec::new())
                                .map_err(io::Error::other)?,
                            fatal: false,
                            diagnostic: "stale target generation".to_owned(),
                        }
                        .encode()
                        .map_err(io::Error::other)?,
                    )
                } else {
                    scene_revision += 1;
                    (
                        messages::SCENE_PRESENTED,
                        ok_payload(
                            request_id,
                            vec![
                                (0, Value::Unsigned(scene_revision)),
                                (1, Value::Unsigned(generation)),
                            ],
                        )?,
                    )
                }
            }
            messages::DESTROY_TRACK => {
                // Give a wrongly dropped transport time to reach EOF before the ordered destroy is
                // serviced, exactly as a relay's media worker would observe it.
                thread::sleep(Duration::from_millis(100));
                let mut guard = shared.lock().expect("shared");
                let closed_before_destroy = guard.closed_channels.contains(&record.object_id);
                guard.destroys.push(DestroyObservation {
                    track_id: record.object_id,
                    closed_before_destroy,
                });
                drop(guard);
                (messages::OK, messages::ok(request_id))
            }
            messages::CREATE_CONTEXT => (
                messages::CONTEXT_READY,
                ok_payload(
                    request_id,
                    vec![
                        (0, Value::Unsigned(record.object_id)),
                        (1, Value::Unsigned(unsigned(2).unwrap_or(0))),
                        (2, generous_contract().to_value()),
                        (3, Value::Unsigned(unsigned(4).unwrap_or(0))),
                        (4, Value::Unsigned(1)),
                    ],
                )?,
            ),
            messages::CREATE_SESSION_LEASE => {
                // A lease's effective terms are the presenter's, bounded by the request: the
                // producer validates that they never exceed what it asked for.
                // Request keys: 0 context, 1 lease, 2 verifier, 3 timeout, 4 grace, 5 policy,
                // 6 permitted profiles, 7 contract. The reply echoes the effective terms.
                let permitted = payload
                    .iter()
                    .find(|(key, _)| *key == 6)
                    .map(|(_, value)| value.clone())
                    .unwrap_or(Value::Array(Vec::new()));
                (
                    messages::SESSION_LEASE_READY,
                    ok_payload(
                        request_id,
                        vec![
                            (0, Value::Unsigned(unsigned(0).unwrap_or(0))),
                            (1, Value::Unsigned(record.object_id)),
                            (2, Value::Unsigned(1)),
                            (3, Value::Unsigned(unsigned(3).unwrap_or(1))),
                            (4, Value::Unsigned(unsigned(4).unwrap_or(0))),
                            (5, Value::Unsigned(unsigned(5).unwrap_or(0))),
                            (6, permitted),
                            (7, generous_contract().to_value()),
                            (8, Value::Unsigned(1)),
                        ],
                    )?,
                )
            }
            // Echo the payload, as every real presenter and the SDK's own producer do. Replying
            // with an empty envelope let a producer regression that dropped the payload pass here
            // and fail against Vivido.
            messages::PING => (
                messages::PONG,
                crate::wire::pong_body(request_id, envelope.payload.clone())?,
            ),
            _ => (messages::OK, messages::ok(request_id)),
        };

        // A dropped reply is a lost reply, not a skipped request: the request above was already
        // serviced and recorded, so a producer that retries must find the outcome already taken.
        if script.state.lock().expect("script").take_drop(reply_type) {
            if record.record_type == messages::GOODBYE {
                break;
            }
            continue;
        }
        if let Some(delay) = script.state.lock().expect("script").delay_for(reply_type) {
            thread::sleep(delay);
        }

        let object_id = match reply_type {
            messages::SCENE_PRESENTED => record.object_id,
            _ => record.object_id,
        };
        let mut guard = sequence.lock().expect("sequence");
        *guard += 1;
        write_record(&mut control, *guard, reply_type, 0, object_id, &reply_body)?;
        drop(guard);
        if record.record_type == messages::GOODBYE {
            break;
        }
    }

    stop.store(true, Ordering::SeqCst);
    // The track acceptor blocks in accept(); one probe connection releases it.
    let _ = TcpStream::connect(address);
    let _ = acceptor.join();
    Ok(())
}

/// Accept every connection after control: track channels and the interactive lane both arrive
/// here, and the preface's connection kind says which is which.
fn accept_secondary_connections(
    listener: TcpListener,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    channel_key: Secret32,
    script: Script,
    lane_writer: Arc<Mutex<Option<(TcpStream, u64)>>>,
) -> io::Result<()> {
    let mut workers = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(_) => break,
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let preface = match read_preface(&mut stream) {
            Ok(preface) => preface,
            // A probe connection that carries no preface is the shutdown wake-up.
            Err(_) => break,
        };
        let shared = shared.clone();
        let key = Secret32::new(*channel_key.expose());
        let script = script.clone();
        let shutdown = stream.try_clone()?;
        match preface.kind {
            vivid_protocol::wire::ConnectionKind::Lane => {
                let lane_writer = lane_writer.clone();
                let stop = stop.clone();
                workers.push((
                    thread::spawn(move || {
                        serve_interactive_lane(stream, shared, key, script, lane_writer, stop)
                    }),
                    shutdown,
                ));
            }
            vivid_protocol::wire::ConnectionKind::Track => {
                let lane_writer = lane_writer.clone();
                workers.push((
                    thread::spawn(move || {
                        serve_track_channel(stream, shared, key, script, lane_writer)
                    }),
                    shutdown,
                ));
            }
            vivid_protocol::wire::ConnectionKind::Control
            | vivid_protocol::wire::ConnectionKind::FileTransfer => {
                // The test presenter advertises neither a second control leg nor file-drop-v1.
            }
        }
    }
    // A producer may leave secondary channels open when the control session ends. Wake every
    // worker before joining so teardown is prompt and no test-owned thread outlives its presenter.
    for (worker, stream) in workers {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let _ = worker.join();
    }
    Ok(())
}

fn serve_track_channel(
    mut stream: TcpStream,
    shared: Arc<Mutex<Shared>>,
    channel_key: Secret32,
    script: Script,
    lane_writer: Arc<Mutex<Option<(TcpStream, u64)>>>,
) -> io::Result<()> {
    let open_record = read_record(&mut stream)?;
    if open_record.record_type != messages::CHANNEL_OPEN {
        return Err(io::Error::other("track connection did not open a channel"));
    }
    let open = messages::ChannelOpen::decode(open_record.object_id, &open_record.body)
        .map_err(io::Error::other)?;
    let expected = auth::channel_tag(
        channel_key.expose(),
        open.session_id,
        open.context_id,
        open.surface_id,
        open.track_id,
        open.channel_generation,
        open.track_kind as u32,
        open.lane as u32,
        &open.client_nonce,
    );
    if !auth::verify_tag(&expected, &open.authentication_tag) {
        return Err(io::Error::other("CHANNEL_OPEN authentication tag failed"));
    }

    let maximum_body = shared
        .lock()
        .expect("shared")
        .track_bodies
        .get(&open.track_id)
        .copied()
        .unwrap_or(64 * 1024);
    // A stalled channel is granted the smallest legal window and never widened, so a producer's
    // sender blocks against it while everything else stays live.
    let (window_bytes, window_records) = if script
        .state
        .lock()
        .expect("script")
        .flow_stalled(open.track_id)
    {
        (u64::from(maximum_body), 1)
    } else {
        (u64::from(maximum_body) * 4096, 4096)
    };
    if let Some(delay) = script
        .state
        .lock()
        .expect("script")
        .delay_for(messages::CHANNEL_ACCEPTED)
    {
        thread::sleep(delay);
    }
    let accepted = envelope_body(
        1,
        vec![
            (0, Value::Unsigned(open.context_id)),
            (1, Value::Unsigned(open.surface_id)),
            (2, Value::Unsigned(open.track_id)),
            (3, Value::Unsigned(open.channel_generation)),
            (4, Value::Unsigned(window_bytes)),
            (5, Value::Unsigned(window_records)),
            (6, Value::Unsigned(u64::from(maximum_body))),
            (7, Value::Unsigned(2)),
        ],
    )?;
    write_record(
        &mut stream,
        1,
        messages::CHANNEL_ACCEPTED,
        0,
        open.track_id,
        &accepted,
    )?;

    // Log the accepted generation immediately: a producer's channel may legitimately stay open for
    // the whole session, so a test must not have to wait for EOF to see that it was established.
    shared
        .lock()
        .expect("shared")
        .channels
        .push(TrackChannelLog {
            track_id: open.track_id,
            channel_generation: open.channel_generation,
            media_records: 0,
        });

    let close_after = script
        .state
        .lock()
        .expect("script")
        .close_after(open.track_id);
    let mut accepted_records = 0_u64;
    loop {
        match read_record(&mut stream) {
            Ok(record) if record.record_type == messages::CHANNEL_EOS => break,
            Ok(record) => {
                accepted_records += 1;
                let mut guard = shared.lock().expect("shared");
                if let Some(entry) = guard.channels.iter_mut().find(|entry| {
                    entry.track_id == open.track_id
                        && entry.channel_generation == open.channel_generation
                }) {
                    entry.media_records += 1;
                }
                let accepted = guard.overlays.media_record(
                    open.session_id,
                    record.record_type,
                    open.track_id,
                    &record.body,
                );
                drop(guard);
                flush_overlay_lane(&shared, &lane_writer);
                if !accepted {
                    // A real presenter fails the channel rather than the request when a producer
                    // breaks its contract; the producer must recover this track on its own.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    break;
                }
                if close_after.is_some_and(|limit| accepted_records >= limit) {
                    // Drop the transport mid-stream: the producer must recover this track alone.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    shared
        .lock()
        .expect("shared")
        .closed_channels
        .push(open.track_id);
    Ok(())
}

/// Serve one interactive-lane generation.
///
/// The lane is a separate authenticated connection precisely so a saturated media track cannot
/// delay a revocation, so this loop shares nothing with the control loop but the observation log.
fn serve_interactive_lane(
    mut stream: TcpStream,
    shared: Arc<Mutex<Shared>>,
    channel_key: Secret32,
    script: Script,
    lane_writer: Arc<Mutex<Option<(TcpStream, u64)>>>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    let session_id = TEST_SESSION_ID;
    let open_record = read_record(&mut stream)?;
    if open_record.record_type != messages::LANE_OPEN {
        return Err(io::Error::other("lane connection did not open a lane"));
    }
    let envelope = messages::decode_control(&open_record.body).map_err(io::Error::other)?;
    let open = messages::LaneOpen::decode(&open_record.body).map_err(io::Error::other)?;
    let expected = auth::lane_tag(
        channel_key.expose(),
        open.session_id,
        messages::LaneClass::Interactive as u32,
        open.lane_generation,
        &open.client_nonce,
    );
    if !auth::verify_tag(&expected, &open.authentication_tag) {
        return Err(io::Error::other("LANE_OPEN authentication tag failed"));
    }

    let maximum_body = vivid_protocol::LANE_MAX_RECORD_BODY;
    let accepted = ok_payload(
        envelope.request_id,
        vec![
            (0, Value::Unsigned(open.session_id)),
            (1, Value::Unsigned(messages::LaneClass::Interactive as u64)),
            (2, Value::Unsigned(open.lane_generation)),
            (3, Value::Unsigned(u64::from(maximum_body))),
        ],
    )?;
    let mut sequence = 1_u64;
    write_record(
        &mut stream,
        sequence,
        messages::LANE_ACCEPTED,
        0,
        0,
        &accepted,
    )?;
    shared
        .lock()
        .expect("shared")
        .lanes
        .push(open.lane_generation);
    *lane_writer.lock().expect("lane writer") = Some((stream.try_clone()?, sequence));

    if shared
        .lock()
        .expect("shared")
        .overlay_sessions
        .contains(&session_id)
    {
        shared
            .lock()
            .expect("shared")
            .overlays
            .open_lane(session_id);
        flush_overlay_lane(&shared, &lane_writer);
    }

    let mut grant_generation = 0_u64;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let record = match read_record(&mut stream) {
            Ok(record) => record,
            Err(_) => break,
        };
        if matches!(
            record.record_type,
            messages::OVERLAY_INPUT_RENEW | messages::OVERLAY_INPUT_CAPTURE
        ) {
            let envelope = messages::decode_control(&record.body).map_err(io::Error::other)?;
            let mut guard = shared.lock().expect("shared");
            let outcome = if record.record_type == messages::OVERLAY_INPUT_RENEW {
                guard
                    .overlays
                    .renew(session_id, record.object_id, &envelope.payload)
            } else {
                guard
                    .overlays
                    .capture(session_id, record.object_id, &envelope.payload)
            };
            drop(guard);
            let (reply, body) = overlay_reply(envelope.request_id, outcome)?;
            let mut writer = lane_writer.lock().expect("lane writer");
            if let Some((stream, sequence)) = writer.as_mut() {
                *sequence += 1;
                write_record(stream, *sequence, reply, 0, record.object_id, &body)?;
            }
            drop(writer);
            flush_overlay_lane(&shared, &lane_writer);
            continue;
        }
        if record.record_type != messages::SET_INPUT_BINDING {
            // Ordinary input events travel producer-to-presenter on a real desktop presenter;
            // here they are simply counted by arriving, and nothing is echoed.
            continue;
        }
        let envelope = messages::decode_control(&record.body).map_err(io::Error::other)?;
        let binding = vivid_protocol::input::InputBinding::decode(
            record.object_id,
            &Value::Map(envelope.payload.clone()),
        )
        .map_err(io::Error::other)?;

        // Desktop §5.2: a presenter narrows the requested classes; it never broadens them. The
        // script's denials are exactly a presenter's local policy refusing a class.
        let denied = script.state.lock().expect("script").denied_classes();
        let effective = binding.requested_classes & !denied;
        grant_generation += 1;
        let state = if binding.disabled() {
            0
        } else if effective == 0 {
            2
        } else {
            1
        };
        let observed = ObservedBinding {
            producer_epoch: binding.producer_epoch.get(),
            context_id: binding.context_id,
            surface_id: binding.surface_id,
            surface_generation: binding.surface_generation.get(),
            requested_classes: binding.requested_classes,
            effective_classes: if state == 1 { effective } else { 0 },
            grant_generation,
            state,
        };
        shared.lock().expect("shared").input_bindings.push(observed);

        let bound = ok_payload(
            envelope.request_id,
            vec![
                (0, Value::Unsigned(observed.producer_epoch)),
                (1, Value::Unsigned(observed.grant_generation)),
                (
                    2,
                    Value::Unsigned(if state == 1 { observed.context_id } else { 0 }),
                ),
                (
                    3,
                    Value::Unsigned(if state == 1 { observed.surface_id } else { 0 }),
                ),
                (
                    4,
                    Value::Unsigned(if state == 1 {
                        observed.surface_generation
                    } else {
                        0
                    }),
                ),
                (5, Value::Unsigned(observed.effective_classes)),
                (6, Value::Unsigned(state)),
                (7, Value::Unsigned(binding.reason)),
                (
                    8,
                    Value::Unsigned(if state == 1 {
                        binding.requested_watchdog_us
                    } else {
                        0
                    }),
                ),
            ],
        )?;
        if script
            .state
            .lock()
            .expect("script")
            .take_drop(messages::INPUT_BOUND)
        {
            continue;
        }
        let mut guard = lane_writer.lock().expect("lane writer");
        if let Some((writer, lane_sequence)) = guard.as_mut() {
            *lane_sequence += 1;
            sequence = *lane_sequence;
            write_record(
                writer,
                sequence,
                messages::INPUT_BOUND,
                0,
                binding.surface_id,
                &bound,
            )?;
        }
    }
    *lane_writer.lock().expect("lane writer") = None;
    Ok(())
}

/// The record to write and its encoded body for one overlay answer.
///
/// A refusal has to be a well-formed ERROR carrying the request it failed: a producer cannot act
/// on a diagnostic it cannot associate with anything, and a payload of the wrong shape reads as a
/// protocol fault rather than a refusal.
fn overlay_reply(
    request_id: u64,
    outcome: crate::testing::overlay::OverlayReply,
) -> io::Result<(u16, Vec<u8>)> {
    match outcome {
        Ok((reply, fields)) => Ok((reply, ok_payload(request_id, fields)?)),
        Err(reason) => Ok((
            messages::ERROR,
            ErrorReply {
                code: messages::ERROR_BAD_STATE,
                request_id,
                detail: ErrorDetail::new(Vec::new())?,
                fatal: false,
                diagnostic: reason.to_owned(),
            }
            .encode()?,
        )),
    }
}

/// Write every lane record the overlay state owes a producer.
///
/// Bulk and control records arrive on their own connections, but submission outcomes, viewport
/// snapshots and input events are all owed on the interactive lane, so every producer of one
/// queues it and drains here. A lane that is not open yet simply keeps the queue.
fn flush_overlay_lane(
    shared: &Arc<Mutex<Shared>>,
    lane_writer: &Arc<Mutex<Option<(TcpStream, u64)>>>,
) {
    let mut guard = lane_writer.lock().expect("lane writer");
    let Some((stream, sequence)) = guard.as_mut() else {
        return;
    };
    let pending = shared
        .lock()
        .expect("shared")
        .overlays
        .take_pending(TEST_SESSION_ID);
    for record in pending {
        let Ok(body) = Envelope::new(0, record.payload).encode() else {
            continue;
        };
        *sequence += 1;
        if write_record(
            stream,
            *sequence,
            record.record_type,
            0,
            record.object_id,
            &body,
        )
        .is_err()
        {
            break;
        }
    }
}

fn envelope_body(request_id: u64, payload: PayloadMap) -> io::Result<Vec<u8>> {
    ok_payload(request_id, payload)
}

fn ok_payload(request_id: u64, payload: PayloadMap) -> io::Result<Vec<u8>> {
    Envelope::correlated(request_id, payload)
        .and_then(|envelope| envelope.encode())
        .map_err(io::Error::other)
}

/// A single-output desktop target covering the whole virtual rectangle.
fn desktop_descriptor(width: u32, height: u32, topology_revision: u64) -> PayloadMap {
    vivid_protocol::target::DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width,
        height,
        outputs: vec![vivid_protocol::target::OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width,
            height,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: vivid_protocol::geometry::Rotation::None,
            primary: true,
        }],
        settled: true,
        topology_revision,
    }
    .encode()
}

fn terminal_descriptor(cols: u64, rows: u64) -> PayloadMap {
    vec![
        (0, Value::Unsigned(cols * 10)),
        (1, Value::Unsigned(rows * 20)),
        (2, Value::Unsigned(cols)),
        (3, Value::Unsigned(rows)),
        (4, Value::Unsigned(10)),
        (5, Value::Unsigned(20)),
        (6, Value::Bool(true)),
        (7, Value::Unsigned(3)),
        (8, Value::Unsigned(64)),
    ]
}

fn generous_contract() -> ResourceContract {
    let mut contract =
        ResourceContract::new([u64::MAX / 4; vivid_protocol::resource::RESOURCE_COUNT]);
    contract.set(
        Resource::ControlRecordBody,
        u64::from(vivid_protocol::CONTROL_MAX_RECORD_BODY),
    );
    contract.set(Resource::MediaRecordBody, 16 * 1024 * 1024);
    contract
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn assert_drop_completes(presenter: TestPresenter) {
        let (dropped, observed) = mpsc::sync_channel(1);
        let dropper = thread::spawn(move || {
            drop(presenter);
            dropped.send(()).expect("drop observer");
        });
        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("presenter drop blocked");
        dropper.join().expect("drop thread");
    }

    #[test]
    fn drop_wakes_an_unaccepted_control_connection() {
        let presenter = TestPresenter::start(80, 24).expect("presenter");
        assert_drop_completes(presenter);
    }

    #[test]
    fn drop_wakes_an_incomplete_control_handshake() {
        let presenter = TestPresenter::start(80, 24).expect("presenter");
        let _stalled_peer = TcpStream::connect(
            presenter
                .endpoint()
                .strip_prefix("tcp:")
                .expect("TCP endpoint"),
        )
        .expect("connect stalled peer");

        let deadline = Instant::now() + Duration::from_secs(2);
        while presenter
            .control_shutdown
            .lock()
            .expect("control shutdown")
            .is_none()
        {
            assert!(Instant::now() < deadline, "presenter did not accept peer");
            thread::yield_now();
        }

        assert_drop_completes(presenter);
    }
}
