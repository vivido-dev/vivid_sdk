//! Pane-local, frameless overlay windows. Drawing commands remain portable across presenters.
//!
//! The session owns all windows. Dropping it cancels the connection, including windows whose
//! handles are still alive. Window operations are serialized; input waits use a separate lane.

use std::collections::BTreeSet;
use std::io;
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

pub use crate::OverlaySubmission;
use vivid_protocol::cbor::Value;
use vivid_protocol::identity::{PresenterInstanceId, SessionIdentity};
use vivid_protocol::messages;
use vivid_protocol::overlay::WindowOptions;
pub use vivid_protocol::overlay::wire::Viewport;
use vivid_protocol::overlay::wire::text::{EditorGeometry, MeasureText};
pub use vivid_protocol::overlay::wire::text::{TextGeometry, TextMeasurement};
use vivid_protocol::overlay::wire::{
    Action, Clipboard, Query, SetSemantics, SetWindow, Status, WindowAction, WindowAddress,
};
pub use vivid_protocol::overlay::wire::{
    Appearance, Environment, EnvironmentChanged, PresentationOutcome,
};
pub use vivid_protocol::overlay::{
    AccessibleAction, DismissReason, Event, Scroll, ScrollPhase, SemanticNode, SemanticRole,
    Semantics, Toggled, WindowMode, buttons, keys, modifiers,
};
use vivid_protocol::vector::Frame;
pub use vivid_protocol::vector::{
    Brush, Canvas, Cap, Color, ColorSpace, Command, Corners, CursorShape, Extend, GradientStop,
    HitRegion, HitRole, Join, Path, PathBuilder, Point, Rect, Scalar, Shadow, StrokeStyle, Text,
    Transform,
};

use crate::*;

#[path = "overlay_layout.rs"]
mod layout;
pub use layout::{OverlayHostHandle, RetainedTextLayout};
pub use vivid_protocol::overlay::wire::text::styled::{
    StyledText, TextAlignment, TextOverflow, TextRun, TextStyle, Typography,
};

/// Initial geometry in viewport logical pixels, independent of terminal cells and scrollback.
#[derive(Debug, Clone)]
pub struct OverlayWindowOptions {
    pub bounds: Rect,
    pub mode: WindowMode,
    pub title: String,
    pub visible: bool,
    /// Floor for host-driven `HitRole::Resize` gestures. Bounds may not start below it.
    pub min_width: Scalar,
    pub min_height: Scalar,
}
impl OverlayWindowOptions {
    pub fn new(bounds: Rect, mode: WindowMode) -> Self {
        Self {
            bounds,
            mode,
            title: String::new(),
            visible: true,
            min_width: Scalar::ONE,
            min_height: Scalar::ONE,
        }
    }
}

/// Authenticated owner of multiple overlay windows and their interactive lane.
#[derive(Debug)]
pub struct OverlaySession {
    session: Arc<Mutex<Option<Session>>>,
    input: Arc<OverlayInputLane>,
    renewal_stop: mpsc::Sender<()>,
    renewal: Option<std::thread::JoinHandle<()>>,
}

/// A frameless window with one persistent surface, vector track, and bulk channel.
#[derive(Debug)]
pub struct OverlayWindow {
    session: Weak<Mutex<Option<Session>>>,
    input: Weak<OverlayInputLane>,
    address: WindowAddress,
    state: Mutex<WindowState>,
}

#[derive(Debug)]
struct WindowState {
    surface: Surface,
    track: Track,
    channel: TrackChannel,
    window: SetWindow,
    next_scene: u64,
    active: bool,
    closed: bool,
    assets: BTreeSet<u64>,
    layouts: BTreeSet<u64>,
}

/// An immutable RGBA image uploaded once to one window's channel. Released when the window closes.
#[derive(Debug, Clone)]
pub struct RetainedImage {
    session: Weak<Mutex<Option<Session>>>,
    window: WindowAddress,
    id: u64,
    track_id: u64,
    channel_generation: u64,
}

impl RetainedImage {
    /// The channel-qualified asset identity. Language bindings expose it so an image can be
    /// referenced by value, as an image brush does.
    pub fn id(&self) -> u64 {
        self.id
    }
}

#[derive(Debug)]
pub struct OverlayWindowStatus {
    pub bounds: Rect,
    pub viewport: Viewport,
    pub viewport_revision: u64,
    pub window_revision: u64,
    pub presented_revision: u64,
    pub accepted_revision: u64,
    pub active_revision: Option<u64>,
    pub focused: bool,
}

impl OverlaySession {
    pub fn from_env() -> io::Result<Self> {
        Self::connect(ProducerConfig::default())
    }

    /// Require the complete overlay profile set, using the normal SDK authentication settings.
    pub fn connect(mut config: ProducerConfig) -> io::Result<Self> {
        config.target_profile = TERMINAL_SURFACE.into();
        for profile in [
            CORE_CONTROL,
            LIVE_MEDIA,
            TERMINAL_SURFACE,
            TERMINAL_OVERLAY,
            VECTOR_SCENE,
            OVERLAY_INPUT,
        ] {
            config.required_profiles.push(profile.into());
        }
        config.required_profiles.sort();
        config.required_profiles.dedup();
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_TEXT.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_PAINT.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_POINTER.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_CLIPBOARD.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_ENV.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_A11Y.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_TEXT_LAYOUT.into());
        config
            .optional_profiles
            .push(vivid_protocol::registry::OVERLAY_TYPOGRAPHY.into());
        config.optional_profiles.sort();
        config.optional_profiles.dedup();
        config
            .optional_profiles
            .retain(|p| !config.required_profiles.contains(p));
        Self::from_session(Session::connect(config)?)
    }

    pub fn from_session(session: Session) -> io::Result<Self> {
        for profile in [
            TERMINAL_SURFACE,
            TERMINAL_OVERLAY,
            VECTOR_SCENE,
            OVERLAY_INPUT,
        ] {
            if !session.supports(profile) {
                return Err(invalid_input(format!("overlay session requires {profile}")));
            }
        }
        let input = Arc::new(session.open_overlay_input_lane(1)?);
        input.renew(2_000_000, Duration::from_secs(1))?;
        let (renewal_stop, stop) = mpsc::channel();
        let lane = Arc::downgrade(&input);
        let renewal = std::thread::Builder::new()
            .name("vivid-overlay-renewal".into())
            .spawn(move || {
                while matches!(
                    stop.recv_timeout(Duration::from_millis(500)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let Some(lane) = lane.upgrade() else {
                        break;
                    };
                    if lane.renew(2_000_000, Duration::from_secs(1)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            session: Arc::new(Mutex::new(Some(session))),
            input,
            renewal_stop,
            renewal: Some(renewal),
        })
    }

    pub fn create_window(&self, options: OverlayWindowOptions) -> io::Result<OverlayWindow> {
        self.create(options, None)
    }

    /// Create a popup or modal child without exposing a wire identity or accepting a foreign owner.
    pub fn create_child(
        &self,
        parent: &OverlayWindow,
        options: OverlayWindowOptions,
    ) -> io::Result<OverlayWindow> {
        if !Weak::ptr_eq(&parent.session, &Arc::downgrade(&self.session)) {
            return Err(invalid_input("overlay parent belongs to another session"));
        }
        self.create(options, Some(parent))
    }

    fn create(
        &self,
        options: OverlayWindowOptions,
        parent: Option<&OverlayWindow>,
    ) -> io::Result<OverlayWindow> {
        options.bounds.validate().map_err(io::Error::other)?;
        let width = options.bounds.width.get().ceil();
        let height = options.bounds.height.get().ceil();
        if width > 16384. || height > 16384. {
            return Err(invalid_input("overlay extent exceeds 16384 logical pixels"));
        }
        let mut guard = lock(&self.session, "overlay session")?;
        let session = guard.as_mut().ok_or_else(closed)?;
        let mut window_options = WindowOptions::new(options.bounds, options.mode);
        window_options.visible = options.visible;
        window_options.min_width = options.min_width;
        window_options.min_height = options.min_height;
        window_options.validate().map_err(io::Error::other)?;
        if let Some(parent) = parent {
            let state = lock(&parent.state, "overlay parent")?;
            if state.closed {
                return Err(closed());
            }
            window_options.parent = Some(state.window.address.identity(owner(session))?);
        }
        let definition = SurfaceBuilder::new(session, width as u64, height as u64)?
            .titled(SurfaceRole::Figure, options.title)
            .build()?;
        let surface = session.create_surface(definition, &RequestMetadata::default())?;
        let result = (|| {
            let limit = session
                .info()
                .vector_limits
                .as_ref()
                .ok_or_else(|| invalid_data("missing negotiated vector limits"))?;
            // Keep enough session capacity for several windows, and respect delegated contracts.
            let contract = &session.info().resource_contract;
            let record_limit = contract
                .get(vivid_protocol::resource::Resource::MediaRecordBody)
                .min(contract.get(vivid_protocol::resource::Resource::InflightMediaBytes))
                .min(256 * 1024);
            let scene_bytes = u32::try_from(limit.values()[0].min(record_limit.saturating_sub(12)))
                .map_err(|_| invalid_data("invalid vector limit"))?;
            let record_bytes = scene_bytes
                .checked_add(12)
                .ok_or_else(|| invalid_data("vector record limit overflow"))?;
            let track = session.create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id: surface.context_id(),
                    surface_id: surface.id(),
                    track_id: session.allocate_id()?,
                    slot: SLOT_VECTOR,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: record_bytes,
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: u64::from(record_bytes) * 8 * 60,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: u64::from(record_bytes),
                    target_latency_us: 0,
                    maximum_latency_us: 0,
                    retained_pixel_charge: (width as u64) * (height as u64),
                    kind: KindConfiguration::VectorScene(VectorConfiguration {
                        width: width as u32,
                        height: height as u32,
                        maximum_scene_bytes: scene_bytes,
                    }),
                },
                &RequestMetadata::default(),
            )?;
            let channel = session.open_track_channel(&track)?;
            let request = SetWindow {
                address: WindowAddress {
                    context_id: surface.context_id(),
                    surface_id: surface.id(),
                    generation: surface.generation().get(),
                },
                expected_revision: 0,
                options: window_options,
            };
            let window = set_window(session, &request)?;
            Ok(OverlayWindow {
                session: Arc::downgrade(&self.session),
                input: Arc::downgrade(&self.input),
                address: window.address,
                state: Mutex::new(WindowState {
                    surface: surface.clone(),
                    track,
                    channel,
                    window,
                    next_scene: 1,
                    active: false,
                    closed: false,
                    assets: BTreeSet::new(),
                    layouts: BTreeSet::new(),
                }),
            })
        })();
        if result.is_err() {
            let _ = session.destroy_surface(&surface, &RequestMetadata::default());
        }
        result
    }

    /// A bounded wait on the separately authenticated input transport.
    pub fn wait_event(&self, timeout: Duration) -> io::Result<Option<OverlayLaneEvent>> {
        self.input.wait_event(timeout)
    }

    /// Match a typed input event to a handle without exposing or narrowing its wire identity.
    pub fn event_targets(
        &self,
        event: &vivid_protocol::overlay::wire::InputEvent,
        window: &OverlayWindow,
    ) -> io::Result<bool> {
        if !Weak::ptr_eq(&window.session, &Arc::downgrade(&self.session)) {
            return Ok(false);
        }
        Ok(window.address == event.address)
    }

    pub fn capture_pointer(&self, window: &OverlayWindow, capture: bool) -> io::Result<()> {
        if !Weak::ptr_eq(&window.session, &Arc::downgrade(&self.session)) {
            return Err(invalid_input("overlay window belongs to another session"));
        }
        let request = window.with_state(|session, state| {
            let status = refresh(session, state)?;
            Ok(vivid_protocol::overlay::wire::Capture {
                address: state.window.address,
                scene_revision: status.scene_revision,
                capture,
            })
        })?;
        self.input.capture(request, Duration::from_secs(1))
    }

    pub fn close(&self) -> io::Result<()> {
        let _ = self.renewal_stop.send(());
        let lane = self.input.close();
        let session = lock(&self.session, "overlay session")?.take();
        let result = session.map_or(Ok(()), Session::close);
        lane.and(result)
    }
}

impl Drop for OverlaySession {
    fn drop(&mut self) {
        let _ = self.renewal_stop.send(());
        let _ = self.input.close();
        if let Some(worker) = self.renewal.take() {
            let _ = worker.join();
        }
    }
}

impl OverlayWindow {
    /// Measure host-shaped text in logical pixels, independent of its drawing origin and color.
    pub fn measure_text(&self, text: &Text) -> io::Result<TextMeasurement> {
        self.with_state(|session, _| {
            if !session.supports(vivid_protocol::registry::OVERLAY_TEXT) {
                return Err(invalid_input("presenter does not support overlay-text-v1"));
            }
            let request = MeasureText {
                address: self.address,
                text: text.clone(),
            };
            let reply = session
                .request(
                    messages::MEASURE_OVERLAY_TEXT,
                    self.address.surface_id,
                    request.payload().map_err(io::Error::other)?,
                    &RequestMetadata::default(),
                    None,
                    None,
                )?
                .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
            expect_record(
                &reply,
                messages::OVERLAY_TEXT_MEASURED,
                self.address.surface_id,
            )?;
            let result =
                TextMeasurement::decode(self.address, &Value::Map(decoded_payload(&reply)?))
                    .map_err(io::Error::other)?;
            result.validate_text(&text.text).map_err(io::Error::other)?;
            Ok(result)
        })
    }

    /// Publish the window's semantic tree for the scene revision it describes.
    ///
    /// A tree is a complete replacement, so a node it no longer names stops existing. The host
    /// refuses a tree whose revision is not the currently published scene, so assistive
    /// technology is never told about a control that is not on screen. Requires
    /// `overlay-a11y-v1`; actions arrive as `OverlayLaneEvent::Accessibility`.
    pub fn set_semantics(&self, semantics: &Semantics) -> io::Result<()> {
        semantics.validate().map_err(io::Error::other)?;
        self.with_state(|session, _| {
            if !session.supports(vivid_protocol::registry::OVERLAY_A11Y) {
                return Err(invalid_input("presenter does not support overlay-a11y-v1"));
            }
            let request = SetSemantics {
                address: self.address,
                semantics: semantics.clone(),
            };
            let reply = session.request(
                messages::SET_OVERLAY_SEMANTICS,
                self.address.surface_id,
                request.payload().map_err(io::Error::other)?,
                &RequestMetadata::default(),
                None,
                None,
            )?;
            let Some(reply) = reply else {
                // A dry-run presenter has no assistive technology to describe anything to.
                return Err(invalid_input(
                    "offline presenter has no accessibility service",
                ));
            };
            expect_record(&reply, messages::OK, self.address.surface_id)?;
            Ok(())
        })
    }

    /// Place text on the user's clipboard.
    ///
    /// The host honors this only for a focused window and only just after a key or pointer press
    /// it delivered there, because a clipboard is shared with every other application on the
    /// machine. There is no way to read a clipboard back; paste arrives as ordinary committed
    /// text. Requires `overlay-clipboard-v1`.
    pub fn set_clipboard(&self, text: &str) -> io::Result<()> {
        self.with_state(|session, _| {
            if !session.supports(vivid_protocol::registry::OVERLAY_CLIPBOARD) {
                return Err(invalid_input(
                    "presenter does not support overlay-clipboard-v1",
                ));
            }
            let request = Clipboard {
                address: self.address,
                text: text.to_owned(),
            };
            let reply = session.request(
                messages::SET_OVERLAY_CLIPBOARD,
                self.address.surface_id,
                request.payload().map_err(io::Error::other)?,
                &RequestMetadata::default(),
                None,
                None,
            )?;
            let Some(reply) = reply else {
                // An offline presenter has no clipboard to write to, and refusing is clearer
                // than reporting a success nothing performed.
                return Err(invalid_input("offline presenter has no clipboard"));
            };
            expect_record(&reply, messages::OK, self.address.surface_id)?;
            Ok(())
        })
    }

    /// Set a window-local logical IME exclusion/caret rectangle for the focused presented scene.
    pub fn set_editor_geometry(&self, scene_revision: u64, caret: Option<Rect>) -> io::Result<()> {
        self.with_state(|session, _| {
            if !session.supports(vivid_protocol::registry::OVERLAY_TEXT) {
                return Err(invalid_input("presenter does not support overlay-text-v1"));
            }
            let request = EditorGeometry {
                address: self.address,
                scene_revision,
                caret,
            };
            let reply = session
                .request(
                    messages::SET_OVERLAY_EDITOR,
                    self.address.surface_id,
                    request.payload().map_err(io::Error::other)?,
                    &RequestMetadata::default(),
                    None,
                    None,
                )?
                .ok_or_else(|| invalid_input("offline presenter has no text service"))?;
            expect_record(&reply, messages::OK, self.address.surface_id)?;
            Ok(())
        })
    }
    pub fn upload_rgba(&self, width: u32, height: u32, rgba: &[u8]) -> io::Result<RetainedImage> {
        self.with_state(|session, state| {
            let bytes = u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|n| n.checked_mul(4));
            if width == 0
                || height == 0
                || bytes != Some(rgba.len() as u64)
                || rgba.len() > vivid_protocol::vector::MAX_ASSET_BYTES
            {
                return Err(invalid_input(
                    "invalid retained RGBA image dimensions or bytes",
                ));
            }
            // Reject before copying the caller's pixels into owned transport storage.
            let record_limit = state.track.configuration()?.maximum_record_body as usize;
            if rgba
                .len()
                .checked_add(16)
                .is_none_or(|length| length > record_limit)
            {
                return Err(invalid_input(
                    "retained image exceeds this window's record limit",
                ));
            }
            let id = session.allocate_id()?;
            state
                .channel
                .send_vector_asset(&vivid_protocol::vector::ImageAsset {
                    id,
                    width,
                    height,
                    rgba: rgba.to_vec(),
                })?;
            state.assets.insert(id);
            Ok(RetainedImage {
                session: self.session.clone(),
                window: state.window.address,
                id,
                track_id: state.track.id(),
                channel_generation: state.track.channel_generation().get(),
            })
        })
    }

    /// Add a retained image to this window's display list, rejecting handles from another window.
    pub fn draw_image(
        &self,
        canvas: &mut Canvas,
        image: &RetainedImage,
        rect: Rect,
        opacity: u16,
    ) -> io::Result<()> {
        self.with_state(|_, state| {
            if !Weak::ptr_eq(&self.session, &image.session)
                || image.window != state.window.address
                || image.track_id != state.track.id()
                || image.channel_generation != state.track.channel_generation().get()
                || !state.assets.contains(&image.id)
            {
                return Err(invalid_input("retained image belongs to another window"));
            }
            rect.validate().map_err(io::Error::other)?;
            canvas
                .push(Command::Image {
                    asset: image.id,
                    rect,
                    opacity,
                })
                .map_err(io::Error::other)?;
            Ok(())
        })
    }
    fn with_state<T>(
        &self,
        f: impl FnOnce(&mut Session, &mut WindowState) -> io::Result<T>,
    ) -> io::Result<T> {
        let shared = self.session.upgrade().ok_or_else(closed)?;
        let mut session = lock(&shared, "overlay session")?;
        let session = session.as_mut().ok_or_else(closed)?;
        let mut state = lock(&self.state, "overlay window")?;
        if state.closed {
            return Err(closed());
        }
        f(session, &mut state)
    }

    /// Submit a full replacement. Initial readiness is bounded to five seconds before activation.
    /// Success acknowledges sending and initial activation, not GPU presentation of later scenes.
    pub fn present(&self, canvas: Canvas) -> io::Result<()> {
        self.submit(canvas).map(|_| ())
    }

    pub fn submit(&self, canvas: Canvas) -> io::Result<OverlaySubmission> {
        self.with_state(|session, state| {
            layout::validate_references(&canvas, &state.layouts)?;
            layout::validate_paint(&canvas, session)?;
            layout::validate_pointer(&canvas, session)?;
            let next = state
                .next_scene
                .checked_add(1)
                .ok_or_else(|| invalid_data("scene revision exhausted"))?;
            let input = self.input.upgrade().ok_or_else(closed)?;
            let receipt = input.register_submission(vivid_protocol::overlay::wire::Submission {
                address: state.window.address,
                track_id: state.track.id(),
                channel_generation: state.track.channel_generation().get(),
                epoch: 1,
                revision: state.next_scene,
            })?;
            let send = state.channel.send_vector(&Frame {
                epoch: 1,
                revision: state.next_scene,
                canvas,
            });
            if let Err(error) = send {
                input.cancel_submission(receipt.identity, &error.to_string());
                return Err(error);
            }
            state.next_scene = next;
            if !state.active {
                session.wait_track(
                    &state.track,
                    TrackWaitCondition::MilestoneSet,
                    Some(MILESTONE_OUTPUT_READY),
                    5_000_000,
                )?;
                session.activate_tracks(
                    &state.surface,
                    &[SlotBinding {
                        slot: SLOT_VECTOR,
                        track_id: state.track.id(),
                        expected_channel_generation: state.track.channel_generation(),
                        required_milestone: MILESTONE_OUTPUT_READY,
                    }],
                    &RequestMetadata::default(),
                )?;
                state.active = true;
            }
            Ok(receipt)
        })
    }

    /// Query authoritative placement and active/presented state; recover the next scene revision.
    pub fn reconcile(&self) -> io::Result<OverlayWindowStatus> {
        self.with_state(|session, state| {
            let status = refresh(session, state)?;
            state.next_scene = state.next_scene.max(
                status
                    .accepted_revision
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("scene revision exhausted"))?,
            );
            Ok(OverlayWindowStatus {
                bounds: status.window.options.bounds,
                viewport: status.viewport,
                viewport_revision: status.viewport_revision,
                window_revision: status.window.expected_revision,
                presented_revision: status.scene_revision,
                accepted_revision: status.accepted_revision,
                active_revision: status.active.map(|s| s.revision),
                focused: status.focused,
            })
        })
    }

    /// Prime and activate a new immutable vector track, preserving this window's identity.
    /// Old-track images cannot be used in the replacement Canvas; upload new images afterwards.
    pub fn replace_track(&self, canvas: Canvas) -> io::Result<OverlaySubmission> {
        canvas.validate().map_err(io::Error::other)?;
        let session = self.session.upgrade().ok_or_else(closed)?;
        if let Some(session) = lock(&session, "overlay session")?.as_ref() {
            layout::validate_paint(&canvas, session)?;
            layout::validate_pointer(&canvas, session)?;
        }
        if canvas
            .commands()
            .iter()
            .any(|c| matches!(c, Command::Image { .. }))
        {
            return Err(invalid_input(
                "replacement scene cannot reference images from the retired track",
            ));
        }
        self.reconcile()?;
        self.with_state(|session, state| {
            layout::validate_references(&canvas, &state.layouts)?;
            let mut config = state.track.configuration()?;
            config.track_id = session.allocate_id()?;
            let track = session.create_track(config, &RequestMetadata::default())?;
            let result = (|| {
                let channel = session.open_track_channel(&track)?;
                let input = self.input.upgrade().ok_or_else(closed)?;
                let receipt =
                    input.register_submission(vivid_protocol::overlay::wire::Submission {
                        address: state.window.address,
                        track_id: track.id(),
                        channel_generation: track.channel_generation().get(),
                        epoch: 1,
                        revision: state.next_scene,
                    })?;
                let next = state
                    .next_scene
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("scene revision exhausted"))?;
                if let Err(error) = channel.send_vector(&Frame {
                    epoch: 1,
                    revision: state.next_scene,
                    canvas,
                }) {
                    input.cancel_submission(receipt.identity, &error.to_string());
                    return Err(error);
                }
                state.next_scene = next;
                session.wait_track(
                    &track,
                    TrackWaitCondition::MilestoneSet,
                    Some(MILESTONE_OUTPUT_READY),
                    5_000_000,
                )?;
                session.activate_tracks(
                    &state.surface,
                    &[SlotBinding {
                        slot: SLOT_VECTOR,
                        track_id: track.id(),
                        expected_channel_generation: track.channel_generation(),
                        required_milestone: MILESTONE_OUTPUT_READY,
                    }],
                    &RequestMetadata::default(),
                )?;
                let old = std::mem::replace(&mut state.track, track.clone());
                let old_channel = std::mem::replace(&mut state.channel, channel);
                state.active = true;
                state.assets.clear();
                let _ = old_channel.eos();
                session.destroy_track(&old, &RequestMetadata::default())?;
                Ok(receipt)
            })();
            if result.is_err() && state.track.id() != track.id() {
                let _ = session.destroy_track(&track, &RequestMetadata::default());
            }
            result
        })
    }

    pub fn release_image(&self, image: &RetainedImage) -> io::Result<()> {
        self.with_state(|_, state| {
            if !Weak::ptr_eq(&self.session, &image.session)
                || image.window != state.window.address
                || image.track_id != state.track.id()
                || image.channel_generation != state.track.channel_generation().get()
                || !state.assets.contains(&image.id)
            {
                return Err(invalid_input(
                    "retained image is released or belongs to another track",
                ));
            }
            state.channel.release_vector_asset(image.id)?;
            state.assets.remove(&image.id);
            Ok(())
        })
    }

    pub fn set_bounds(&self, bounds: Rect) -> io::Result<()> {
        self.with_state(|session, state| {
            let mut request = refresh(session, state)?.window;
            request.options.bounds = bounds;
            state.window = set_window(session, &request)?;
            Ok(())
        })
    }

    pub fn set_visible(&self, visible: bool) -> io::Result<()> {
        self.with_state(|session, state| {
            let mut request = refresh(session, state)?.window;
            request.options.visible = visible;
            state.window = set_window(session, &request)?;
            Ok(())
        })
    }

    pub fn center(&self) -> io::Result<()> {
        self.action(WindowAction::Center)
    }
    pub fn request_focus(&self) -> io::Result<()> {
        self.action(WindowAction::Focus)
    }
    pub fn raise(&self) -> io::Result<()> {
        self.action(WindowAction::Raise)
    }
    pub fn lower(&self) -> io::Result<()> {
        self.action(WindowAction::Lower)
    }

    fn action(&self, action: WindowAction) -> io::Result<()> {
        self.with_state(|session, state| {
            refresh(session, state)?;
            let action = Action {
                address: state.window.address,
                expected_revision: state.window.expected_revision,
                action,
            };
            session.request_ok(
                messages::OVERLAY_ACTION,
                state.surface.id(),
                action.payload()?,
                &RequestMetadata::default(),
            )?;
            refresh(session, state)?;
            Ok(())
        })
    }

    pub fn bounds(&self) -> io::Result<Rect> {
        self.with_state(|session, state| Ok(refresh(session, state)?.window.options.bounds))
    }
    pub fn viewport(&self) -> io::Result<Viewport> {
        self.with_state(|session, state| Ok(refresh(session, state)?.viewport))
    }

    /// Close this window and its child popups, then release its surface and retained resources.
    pub fn close(&self) -> io::Result<()> {
        self.with_state(|session, state| {
            // Destroying the surface also closes a window already dismissed by the host.
            let eos = if state.active {
                state.channel.eos().map(|_| ())
            } else {
                Ok(())
            };
            let destroy = session.destroy_surface(&state.surface, &RequestMetadata::default());
            if destroy.is_ok() {
                state.closed = true;
            }
            destroy.and(eos)
        })
    }
}

// The presenter component is a producer-local namespace marker, never transmitted. The host
// substitutes its authenticated owner; parent handles are additionally checked by Arc identity.
fn owner(session: &Session) -> SessionIdentity {
    SessionIdentity {
        presenter: PresenterInstanceId([0; 16]),
        session_id: session.info().session_id,
    }
}
fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "overlay window or session is closed",
    )
}

impl Session {
    /// Issue `SET_OVERLAY_WINDOW` directly on this session and await `OVERLAY_WINDOW_READY`,
    /// without opening a dedicated [`OverlaySession`]. For a caller that already owns a plain
    /// `Session` for other purposes and only needs one-shot window control — a terminating
    /// gateway relaying a nested producer's overlay window to its own outer session, for example,
    /// alongside the ordinary surfaces and tracks it creates on that same session directly.
    ///
    /// Refused before anything is sent unless this session negotiated `terminal-overlay-v1` and
    /// `vector-scene-v1`, matching the spec's requirement that a producer which did not negotiate
    /// the profile must not send this record.
    pub fn set_overlay_window(&self, request: &SetWindow) -> io::Result<SetWindow> {
        if !self.supports(vivid_protocol::registry::TERMINAL_OVERLAY)
            || !self.supports(vivid_protocol::registry::VECTOR_SCENE)
        {
            return Err(invalid_input(
                "session did not negotiate terminal-overlay-v1 and vector-scene-v1",
            ));
        }
        set_window(self, request)
    }
}

fn set_window(session: &Session, request: &SetWindow) -> io::Result<SetWindow> {
    let reply = session.request(
        messages::SET_OVERLAY_WINDOW,
        request.address.surface_id,
        request.payload(owner(session))?,
        &RequestMetadata::default(),
        None,
        None,
    )?;
    let Some(reply) = reply else {
        let mut window = request.clone();
        window.expected_revision = window
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid_data("window revision exhausted"))?;
        return Ok(window);
    };
    expect_record(
        &reply,
        messages::OVERLAY_WINDOW_READY,
        request.address.surface_id,
    )?;
    let window = SetWindow::decode(
        owner(session),
        request.address.surface_id,
        &Value::Map(decoded_payload(&reply)?),
    )?;
    if window.address != request.address || window.expected_revision <= request.expected_revision {
        return Err(invalid_data(
            "invalid overlay window reply identity or revision",
        ));
    }
    Ok(window)
}

fn refresh(session: &Session, state: &mut WindowState) -> io::Result<Status> {
    let query = Query {
        context_id: state.surface.context_id(),
        surface_id: state.surface.id(),
    };
    let reply = session
        .request(
            messages::QUERY_OVERLAY,
            state.surface.id(),
            query.payload()?,
            &RequestMetadata::default(),
            None,
            None,
        )?
        .ok_or_else(|| invalid_input("offline overlay status has no authoritative viewport"))?;
    expect_record(&reply, messages::OVERLAY_STATUS, state.surface.id())?;
    let status = Status::decode(
        owner(session),
        state.surface.id(),
        &Value::Map(decoded_payload(&reply)?),
    )?;
    if status.window.address != state.window.address {
        return Err(invalid_data(
            "overlay status belongs to another window generation",
        ));
    }
    state.window = status.window.clone();
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> OverlayWindowOptions {
        OverlayWindowOptions::new(
            Rect::new(10., 20., 100., 80.).unwrap(),
            WindowMode::Floating,
        )
    }

    #[test]
    fn handles_preserve_tracks_and_assets_and_are_owned_by_the_session() {
        let session = OverlaySession::connect(ProducerConfig::offline()).unwrap();
        let first = session.create_window(options()).unwrap();
        let second = session.create_window(options()).unwrap();
        let image = first.upload_rgba(1, 1, &[1, 2, 3, 255]).unwrap();
        let rect = Rect::new(0., 0., 1., 1.).unwrap();
        let mut canvas = Canvas::new();
        first
            .draw_image(&mut canvas, &image, rect, u16::MAX)
            .unwrap();
        assert!(
            second
                .draw_image(&mut Canvas::new(), &image, rect, u16::MAX)
                .is_err()
        );
        let track_id = first.state.lock().unwrap().track.id();
        first.present(canvas.clone()).unwrap();
        first.present(canvas).unwrap();
        assert_eq!(first.state.lock().unwrap().track.id(), track_id);
        assert_eq!(first.state.lock().unwrap().next_scene, 3);
        let foreign = OverlaySession::connect(ProducerConfig::offline()).unwrap();
        assert!(foreign.create_child(&first, options()).is_err());
        first.close().unwrap();
        assert!(first.present(Canvas::new()).is_err());
        second.present(Canvas::new()).unwrap();
        session.close().unwrap();
        assert_eq!(
            second.present(Canvas::new()).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn invalid_creation_leaves_no_resources_and_session_drop_invalidates_windows() {
        let session = OverlaySession::connect(ProducerConfig::offline()).unwrap();
        let mut invalid = options();
        invalid.bounds.width = Scalar::ZERO;
        assert!(session.create_window(invalid).is_err());
        assert!(
            session
                .session
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .surfaces
                .is_empty()
        );
        let window = session.create_window(options()).unwrap();
        drop(session);
        assert_eq!(
            window.present(Canvas::new()).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn unsupported_sessions_are_rejected_before_window_creation() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        assert!(OverlaySession::from_session(session).is_err());
    }
}
