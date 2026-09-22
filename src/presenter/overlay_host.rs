//! Presenter-neutral overlay hosting: window/focus/revision bookkeeping shared by every real or
//! test presenter that hosts the Vivid overlay bundle.
//!
//! A presenter needs somewhere to keep window, focus and revision state that is not itself a GPU:
//! this accepts the overlay profile bundle, keeps that state in the protocol's own [`Windows`]
//! state machine, and validates every display list against the negotiated limits. It composes
//! nothing — "presented" here means the list decoded, validated, and became the window's published
//! scene, which is the part a layout, hit-testing, or relay regression depends on. A real presenter
//! (GPU rasterization, real font shaping) and [`crate::testing::presenter::TestPresenter`]
//! (synthetic metrics, no rasterization) both build on exactly this state.
//!
//! Sharing [`Windows`] between every host is deliberate. A private reimplementation of focus,
//! modal eligibility and revision ordering would agree with itself and with nothing else.
//!
//! One instance serves every session a presenter accepts, distinguishing owners the same way
//! [`Windows`] itself does: every window, track binding, asset and queued record is keyed by the
//! owning session, so two sessions that happen to reuse the same local context/surface/track/asset
//! numbers never observe or disturb each other's state, and [`Overlays::remove_session`] tears down
//! exactly one owner's state when its session ends.

use std::collections::{BTreeSet, HashMap};

use vivid_protocol::cbor::Value;
use vivid_protocol::identity::{PresenterInstanceId, SessionIdentity, SurfaceIdentity};
use vivid_protocol::messages::{self, PayloadMap};
#[cfg(any(test, feature = "testing"))]
use vivid_protocol::overlay::AccessibleAction;
use vivid_protocol::overlay::wire::Environment;
use vivid_protocol::overlay::wire::text::EditorGeometry;
use vivid_protocol::overlay::wire::text::styled::{
    BatchMeasured, MeasureBatch, ReleaseLayouts, StyledText,
};
use vivid_protocol::overlay::wire::text::{TextGeometry, TextMeasurement};
use vivid_protocol::overlay::wire::{
    Action, Capture, Clipboard, PresentationOutcome, Query, Renew, SetSemantics, SetWindow, Status,
    Submission, SubmissionOutcome, Viewport, WindowAddress,
};
use vivid_protocol::overlay::{Event, PointerReport, Scroll, Semantics, Windows};
use vivid_protocol::vector::{Canvas, Frame, Limits, Point, Scalar};

/// A display list the presenter accepted, with the identity a real host would key it by.
#[derive(Debug, Clone)]
pub struct PresentedScene {
    pub window: SurfaceIdentity,
    pub revision: u64,
    pub canvas: Canvas,
}

/// One retained image upload, by the channel-qualified identity the protocol requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedAsset {
    pub width: u32,
    pub height: u32,
}

/// One overlay answer: the record to reply with and its payload, or why it was refused.
pub(crate) type OverlayReply = Result<(u16, PayloadMap), &'static str>;

/// A lane record the presenter owes a producer, queued because bulk records arrive on a different
/// connection than the interactive lane they are acknowledged on.
pub(crate) struct LaneRecord {
    pub record_type: u16,
    pub object_id: u64,
    pub payload: PayloadMap,
}

/// One session's viewport and the revision it last changed at.
///
/// A viewport describes the pane a session's windows lay out against, so it is per session rather
/// than per presenter: one presenter serves many panes, and resizing one of them must not relayout
/// the windows of another.
#[derive(Debug, Clone, Copy)]
struct SessionViewport {
    viewport: Viewport,
    revision: u64,
}

#[derive(Default)]
pub(crate) struct Overlays {
    windows: Windows,
    viewports: HashMap<u64, SessionViewport>,
    limits: Limits,
    /// Which surface a bulk track belongs to, learned at CREATE_TRACK — before the overlay
    /// window exists, because a producer builds its track first. Keyed by owner as well as track:
    /// two producers may each name track 5, and they are not the same track.
    tracks: HashMap<(u64, u64), (u64, u64)>,
    /// The last accepted display list per window, and the revision that published it.
    displayed: HashMap<SurfaceIdentity, PresentedScene>,
    accepted: HashMap<SurfaceIdentity, u64>,
    assets: HashMap<(u64, u64, u64), RetainedAsset>,
    /// Strictly increasing per channel generation, including after release.
    highest_asset: HashMap<(u64, u64), u64>,
    lane_generation: HashMap<u64, u64>,
    /// Records a session is owed on its interactive lane, drained in arrival order. An environment
    /// change is broadcast to every session with overlay state, because it describes the host
    /// itself; viewports, window events, scene outcomes and accessibility actions are queued only
    /// for the session that owns them.
    pending: HashMap<u64, Vec<LaneRecord>>,
    /// Every session known to this host, so an environment broadcast reaches exactly the sessions
    /// that might be listening and no others.
    active_sessions: BTreeSet<u64>,
    /// The environment a producer sees, and the revision it last changed at.
    environment: Environment,
    environment_revision: u64,
    /// The last press of the current click sequence, as the real host tracks it.
    last_click: Option<(std::time::Instant, u16, Point, u8)>,
    /// The window a key or pointer press last reached, as the real host tracks it.
    last_gesture: Option<(SurfaceIdentity, std::time::Instant)>,
    /// Clipboard text this presenter accepted, in acceptance order. A refused write leaves no
    /// trace here, so asserting on this is asserting the guard allowed it.
    clipboard: Vec<String>,
    /// Each window's semantic tree, retired when the scene it described is replaced.
    semantics: HashMap<SurfaceIdentity, Semantics>,
    /// Retained text layouts, by the identity the producer was given.
    retained_layouts: HashMap<u64, TextMeasurement>,
    /// Where a focused editor last said its caret was, with the revision it said it at.
    editor_caret: Option<(u64, Option<vivid_protocol::vector::Rect>)>,
    next_layout: u64,
}

/// The synthetic metric one run is measured with.
///
/// A run is char-count times 0.6 of its size wide and, wrapped to `max_width`, as many lines as
/// that width needs — so a layout that depends on a wrapped height is exercised here. It is a
/// model, not a font: nothing here knows a glyph.
///
/// A host that composites its own scenes replaces this with its shaper. A host that *relays* them
/// has a harder problem and does not solve it here: it answers the measurement itself, so the boxes
/// a toolkit lays out come from this model while the glyphs are shaped downstream by whoever
/// finally paints them, and the two do not agree. Plain [`Command::Text`] still renders — only its
/// box is approximate — but a run that needs shaping is drawn through
/// [`Command::TextLayout`](vivid_protocol::vector::Command::TextLayout), whose id belongs to *this*
/// host's retained namespace and means nothing to the one downstream. Getting either of those right
/// through a relay needs the measurement request forwarded and its retention mirrored, which is not
/// implemented.
fn measurement_of(text: &StyledText) -> TextMeasurement {
    let style = text
        .runs
        .first()
        .map(|run| run.style.clone())
        .unwrap_or_default();
    let size = style.size.get() as f32;
    let characters = text.text().chars().count() as f32;
    let natural = characters * size * 0.6;
    let (width, mut lines) = match (text.wrap, text.max_width) {
        (true, Some(limit)) if limit.get() as f32 > 0. => {
            let limit = limit.get() as f32;
            (limit, (natural / limit).ceil().max(1.))
        }
        _ => (natural, 1.),
    };
    if let Some(ceiling) = text.max_lines {
        lines = lines.min(f32::from(ceiling).max(1.));
    }
    let line_height = size * 1.2;
    let width = Scalar::new(width as f64).unwrap_or(Scalar::ZERO);
    let height = Scalar::new((lines * line_height) as f64).unwrap_or(Scalar::ZERO);
    TextMeasurement {
        truncated_at: None,
        width,
        height,
        lines: (0..lines as u32)
            .map(|line| TextGeometry {
                start: 0,
                end: text.text().len() as u32,
                x: Scalar::ZERO,
                y: Scalar::new((line as f32 * line_height) as f64).unwrap_or(Scalar::ZERO),
                width,
                height: Scalar::new(line_height as f64).unwrap_or(Scalar::ZERO),
                baseline: Scalar::new((line as f32 * line_height + size * 0.8) as f64)
                    .unwrap_or(Scalar::ZERO),
                rtl: false,
            })
            .collect(),
        clusters: Vec::new(),
    }
}

/// The producer names its own session with a placeholder presenter instance, so the presenter
/// side must derive the identical identity or every window would key to a different owner.
pub(crate) fn owner(session_id: u64) -> SessionIdentity {
    SessionIdentity {
        presenter: PresenterInstanceId([0; 16]),
        session_id,
    }
}

impl Overlays {
    /// Test-only convenience for a viewport given as a plain logical size at scale 1.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn install_viewport(&mut self, session_id: u64, width: f64, height: f64) {
        let viewport = Viewport {
            width: Scalar::new(width).expect("test viewport width"),
            height: Scalar::new(height).expect("test viewport height"),
            scale_numerator: 1,
            scale_denominator: 1,
        };
        self.install_session_viewport(session_id, viewport);
    }

    /// Record the pane geometry a session's windows lay out against, without queueing a record.
    ///
    /// A session learns its viewport from the initial snapshot [`Overlays::open_lane`] queues at
    /// lane authentication, so seeding it at session establishment — before any lane exists —
    /// must not also queue one.
    pub(crate) fn install_session_viewport(&mut self, session_id: u64, viewport: Viewport) {
        self.note_session(session_id);
        self.store_viewport(session_id, viewport);
    }

    /// Move one session's viewport and tell it, as a pane resize or DPI change does. Unchanged
    /// geometry neither advances the revision nor queues a record.
    pub(crate) fn set_viewport(&mut self, session_id: u64, viewport: Viewport) {
        if !self.store_viewport(session_id, viewport) {
            return;
        }
        if let Some(record) = self.viewport_record(session_id) {
            self.queue_for(session_id, record);
        }
    }

    /// Returns whether the geometry actually changed. Revisions strictly increase on a change.
    fn store_viewport(&mut self, session_id: u64, viewport: Viewport) -> bool {
        match self.viewports.get_mut(&session_id) {
            Some(current) if current.viewport == viewport => false,
            Some(current) => {
                current.viewport = viewport;
                current.revision = current.revision.saturating_add(1);
                true
            }
            None => {
                self.viewports.insert(
                    session_id,
                    SessionViewport {
                        viewport,
                        revision: 1,
                    },
                );
                true
            }
        }
    }

    /// The pane geometry one session's windows lay out against, once it has been told.
    fn viewport_of(&self, session_id: u64) -> Option<SessionViewport> {
        self.viewports.get(&session_id).copied()
    }

    /// Set the environment this presenter reports, as a host would on a font or theme change.
    pub(crate) fn set_environment(&mut self, environment: Environment) {
        if self.environment == environment || environment.validate().is_err() {
            return;
        }
        self.environment = environment;
        self.environment_revision = self.environment_revision.max(1).saturating_add(1);
        self.queue_environment();
    }

    /// Note that a session is talking to this overlay host, so a later broadcast record (viewport,
    /// environment) is queued for it. Idempotent.
    fn note_session(&mut self, session_id: u64) {
        self.active_sessions.insert(session_id);
    }

    fn queue_for(&mut self, session_id: u64, record: LaneRecord) {
        self.pending.entry(session_id).or_default().push(record);
    }

    fn environment_record(&self) -> Option<LaneRecord> {
        let update = vivid_protocol::overlay::wire::EnvironmentChanged {
            revision: self.environment_revision.max(1),
            environment: self.environment.clone(),
        };
        Some(LaneRecord {
            record_type: messages::OVERLAY_ENV_CHANGED,
            object_id: 0,
            payload: update.payload().ok()?,
        })
    }

    fn viewport_record(&self, session_id: u64) -> Option<LaneRecord> {
        let current = self.viewport_of(session_id)?;
        let changed = vivid_protocol::overlay::wire::ViewportChanged {
            revision: current.revision,
            viewport: current.viewport,
        };
        Some(LaneRecord {
            record_type: messages::OVERLAY_VIEWPORT_CHANGED,
            object_id: 0,
            payload: changed.payload().ok()?,
        })
    }

    fn queue_environment(&mut self) {
        let Some(record) = self.environment_record() else {
            return;
        };
        let sessions: Vec<u64> = self.active_sessions.iter().copied().collect();
        for session_id in sessions {
            self.queue_for(
                session_id,
                LaneRecord {
                    record_type: record.record_type,
                    object_id: record.object_id,
                    payload: record.payload.clone(),
                },
            );
        }
    }

    /// The environment a producer sees.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Queue the initial environment and viewport snapshot the spec requires "after lane
    /// authentication", and mark the session active so later changes reach it too. Idempotent
    /// with respect to session activation, but a lane that reopens gets a fresh snapshot, which is
    /// what a reconnecting producer needs to recover current state rather than wait for a change.
    ///
    /// The environment goes first: a producer that adopts the host's font wants it before it lays
    /// anything out against the viewport.
    pub(crate) fn open_lane(&mut self, session_id: u64) {
        self.note_session(session_id);
        if let Some(record) = self.environment_record() {
            self.queue_for(session_id, record);
        }
        if let Some(record) = self.viewport_record(session_id) {
            self.queue_for(session_id, record);
        }
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Records one session is owed on its interactive lane, drained in arrival order.
    pub(crate) fn take_pending(&mut self, session_id: u64) -> Vec<LaneRecord> {
        self.pending.remove(&session_id).unwrap_or_default()
    }

    /// Queue every window event the protocol state machine produced for this owner.
    fn drain_events(&mut self, owner: SessionIdentity) {
        while let Some(event) = self.windows.take_event(owner) {
            let input = vivid_protocol::overlay::wire::InputEvent::from(event);
            let object_id = input.address.surface_id;
            if let Ok(payload) = input.payload() {
                self.queue_for(
                    owner.session_id,
                    LaneRecord {
                        record_type: messages::OVERLAY_INPUT_EVENT,
                        object_id,
                        payload,
                    },
                );
            }
        }
    }

    // ---- control records -------------------------------------------------------------------

    pub(crate) fn relay_input(
        &mut self,
        session: u64,
        input: vivid_protocol::overlay::wire::InputEvent,
    ) -> bool {
        let Ok(id) = input.address.identity(owner(session)) else {
            return false;
        };
        let Some(window) = self.windows.get(id) else {
            return false;
        };
        if window.generation != input.address.generation || !window.options.visible {
            return false;
        }
        if matches!(
            input.event,
            Event::Key { down: true, .. }
                | Event::Pointer {
                    button: Some((_, true)),
                    ..
                }
        ) {
            self.last_gesture = Some((id, std::time::Instant::now()));
        }
        match input.event {
            Event::Geometry { bounds, .. } if window.options.bounds != bounds => {
                let mut options = window.options.clone();
                options.bounds = bounds;
                if self
                    .windows
                    .update(id, input.address.generation, window.revision, options)
                    .is_err()
                {
                    return false;
                }
            }
            Event::Focus(true) => {
                let _ = self.windows.request_focus(id);
            }
            Event::Focus(false) if self.windows.focus() == Some(id) => {
                self.windows.set_pane_focus(false)
            }
            _ => {}
        }
        // Native routing already performed hit testing, click counting, keyboard translation,
        // and IME handling. Preserve that event instead of inventing a second device sample.
        while self.windows.take_event(owner(session)).is_some() {}
        let Ok(payload) = input.payload() else {
            return false;
        };
        self.queue_for(
            session,
            LaneRecord {
                record_type: messages::OVERLAY_INPUT_EVENT,
                object_id: input.address.surface_id,
                payload,
            },
        );
        true
    }

    /// Count a press in the current sequence, matching the real host's policy so a producer
    /// testing against this presenter sees the same counts it will see live.
    fn count_clicks(&mut self, position: Point, button: Option<(u16, bool)>) -> u8 {
        let Some((button_id, true)) = button else {
            return 0;
        };
        let now = std::time::Instant::now();
        let clicks = match self.last_click {
            Some((at, id, point, count))
                if id == button_id
                    && now.saturating_duration_since(at)
                        < std::time::Duration::from_millis(400)
                    && (point.x.get() - position.x.get()).abs() <= 4.
                    && (point.y.get() - position.y.get()).abs() <= 4. =>
            {
                if count >= vivid_protocol::overlay::MAX_CLICKS {
                    1
                } else {
                    count + 1
                }
            }
            _ => 1,
        };
        self.last_click = Some((now, button_id, position, clicks));
        clicks
    }

    /// Note a track's surface so a later vector record can be attributed to a window.
    pub(crate) fn note_track(
        &mut self,
        session_id: u64,
        track_id: u64,
        context_id: u64,
        surface_id: u64,
    ) {
        self.note_session(session_id);
        self.tracks
            .insert((session_id, track_id), (context_id, surface_id));
    }

    fn track_window(&self, session_id: u64, track_id: u64) -> Option<SurfaceIdentity> {
        let (context_id, surface_id) = *self.tracks.get(&(session_id, track_id))?;
        owner(session_id)
            .context(context_id)
            .ok()?
            .surface(surface_id)
            .ok()
    }

    pub(crate) fn set_window(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        self.note_session(session_id);
        let owner = owner(session_id);
        let Ok(request) = SetWindow::decode(owner, object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay window request");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay window identity");
        };
        let revision = if request.expected_revision == 0 {
            if self
                .windows
                .create(id, request.address.generation, request.options.clone())
                .is_err()
            {
                return Err("overlay window could not be created");
            }
            1
        } else {
            match self.windows.update(
                id,
                request.address.generation,
                request.expected_revision,
                request.options.clone(),
            ) {
                Ok(revision) => revision,
                Err(_) => return Err("stale or invalid overlay window update"),
            }
        };
        self.drain_events(owner);
        let mut reply = request;
        reply.expected_revision = revision;
        reply
            .payload(owner)
            .map(|payload| (messages::OVERLAY_WINDOW_READY, payload))
            .map_err(|_| "overlay window reply could not be encoded")
    }

    pub(crate) fn action(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(action) = Action::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay action");
        };
        let Some(current) = self.viewport_of(session_id) else {
            return Err("overlay viewport is absent");
        };
        if self
            .windows
            .apply_action(owner, action, current.viewport)
            .is_err()
        {
            return Err("overlay action was refused");
        }
        self.drain_events(owner);
        Ok((messages::OK, Vec::new()))
    }

    pub(crate) fn status(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(query) = Query::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay query");
        };
        let Some(current) = self.viewport_of(session_id) else {
            return Err("overlay viewport is absent");
        };
        let Ok(context) = owner.context(query.context_id) else {
            return Err("invalid overlay context");
        };
        let Ok(id) = context.surface(query.surface_id) else {
            return Err("invalid overlay surface");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window does not exist");
        };
        let presented = self.displayed.get(&id);
        let status = Status {
            window: SetWindow {
                address: WindowAddress {
                    context_id: query.context_id,
                    surface_id: query.surface_id,
                    generation: window.generation,
                },
                expected_revision: window.revision,
                options: window.options.clone(),
            },
            viewport: current.viewport,
            focused: self.windows.focus() == Some(id),
            scene_revision: presented.map_or(0, |scene| scene.revision),
            active: presented.map(|scene| Submission {
                address: WindowAddress {
                    context_id: query.context_id,
                    surface_id: query.surface_id,
                    generation: window.generation,
                },
                track_id: self
                    .tracks
                    .iter()
                    .find(|((session, _), surface)| {
                        *session == session_id && **surface == (query.context_id, query.surface_id)
                    })
                    .map_or(0, |((_, track), _)| *track),
                channel_generation: 1,
                epoch: 1,
                revision: scene.revision,
            }),
            viewport_revision: current.revision,
            accepted_revision: self.accepted.get(&id).copied().unwrap_or(0),
        };
        status
            .payload(owner)
            .map(|payload| (messages::OVERLAY_STATUS, payload))
            .map_err(|_| "overlay status could not be encoded")
    }

    /// A semantic tree, refused unless it describes the scene the window is showing.
    pub(crate) fn set_semantics(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(request) = SetSemantics::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay semantics");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != request.address.generation {
            return Err("stale overlay window generation");
        }
        let presented = self.displayed.get(&id).map_or(0, |scene| scene.revision);
        if request.semantics.scene_revision != presented {
            return Err("semantics must describe the currently published scene");
        }
        self.semantics.insert(id, request.semantics);
        Ok((messages::OK, Vec::new()))
    }

    /// Every window with a live semantic tree, in a stable order.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn windows_with_semantics(&self) -> Vec<(&SurfaceIdentity, &Semantics)> {
        self.semantics.iter().collect()
    }

    /// A clipboard write, refused with the reason a real host would give.
    pub(crate) fn set_clipboard(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(request) = Clipboard::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay clipboard request");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != request.address.generation {
            return Err("stale overlay window generation");
        }
        if !self.authorize_clipboard(session_id, id) {
            return Err("a clipboard write requires a focused window and a recent gesture in it");
        }
        self.clipboard.push(request.text);
        Ok((messages::OK, Vec::new()))
    }

    /// Shape a batch of text, or retain it, as a host with a font system would.
    ///
    /// The metrics here are synthetic: with no font to shape with, a run is `0.6 * size` per
    /// character wide and `1.2 * size` tall. That is enough for layout, hit testing, and geometry
    /// to be exercised deterministically. A test that depends on real glyph widths belongs
    /// against a live host, not here.
    pub(crate) fn measure_text_batch(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(request) = MeasureBatch::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay text measurement request");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != request.address.generation {
            return Err("stale overlay window generation");
        }
        let mut layouts = Vec::with_capacity(request.texts.len());
        for text in &request.texts {
            let measurement = measurement_of(text);
            if request.retain {
                let layout = self.next_layout.max(1);
                self.next_layout = layout + 1;
                self.retained_layouts.insert(layout, measurement.clone());
                layouts.push((layout, measurement));
            } else {
                layouts.push((0, measurement));
            }
        }
        let measured = BatchMeasured {
            address: request.address,
            layouts,
        };
        Ok((
            messages::OVERLAY_TEXT_BATCH_MEASURED,
            measured.payload().map_err(|_| "batch too large")?,
        ))
    }

    /// Drop retained layouts, so a producer that releases one cannot paint it again.
    pub(crate) fn release_text_layouts(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(request) = ReleaseLayouts::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay layout release");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != request.address.generation {
            return Err("stale overlay window generation");
        }
        for layout in &request.ids {
            self.retained_layouts.remove(layout);
        }
        Ok((messages::OK, Vec::new()))
    }

    /// Record where a focused editor says its caret is.
    ///
    /// The geometry names the revision it describes, so a host knows which scene it belongs to:
    /// an editor that moved between frames must say so again rather than leaving a stale caret.
    pub(crate) fn set_editor_geometry(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(request) = EditorGeometry::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay editor geometry");
        };
        let Ok(id) = request.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != request.address.generation {
            return Err("stale overlay window generation");
        }
        let Some(presented) = self.displayed.get(&id) else {
            return Err("no scene is displayed to describe a caret in");
        };
        if request.scene_revision != presented.revision {
            return Err("editor geometry must describe the displayed scene");
        }
        self.editor_caret = Some((request.scene_revision, request.caret));
        Ok((messages::OK, Vec::new()))
    }

    /// Where the focused editor last placed its caret.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn editor_caret(&self) -> Option<(u64, Option<vivid_protocol::vector::Rect>)> {
        self.editor_caret
    }

    /// Queue an action for the producer, as the accessibility adapter does.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn queue_accessibility(
        &mut self,
        window: SurfaceIdentity,
        node: u64,
        action: AccessibleAction,
    ) -> bool {
        let Some(current) = self.windows.get(window) else {
            return false;
        };
        // An adapter builds its tree from what was published, so a node the live tree no longer
        // names is one the user is not looking at: the scene was replaced while the asker held
        // the old tree.
        if !self
            .semantics
            .get(&window)
            .is_some_and(|tree| tree.nodes.iter().any(|n| n.id == node))
        {
            return false;
        }
        let event = vivid_protocol::overlay::wire::InputEvent {
            address: vivid_protocol::overlay::wire::WindowAddress {
                context_id: window.context.context_id,
                surface_id: window.surface_id,
                generation: current.generation,
            },
            scene_revision: self
                .displayed
                .get(&window)
                .map_or(0, |scene| scene.revision),
            event: Event::Accessibility { node, action },
        };
        let Ok(payload) = event.payload() else {
            return false;
        };
        self.queue_for(
            window.context.session.session_id,
            LaneRecord {
                record_type: messages::OVERLAY_INPUT_EVENT,
                object_id: window.surface_id,
                payload,
            },
        );
        true
    }

    /// Whether a clipboard write from this window would be honored. This presenter applies the
    /// same rules the real host does, so a producer meets the same refusals here as live.
    pub(crate) fn authorize_clipboard(&self, _session_id: u64, id: SurfaceIdentity) -> bool {
        if self.windows.get(id).is_none() || self.windows.focus() != Some(id) {
            return false;
        }
        match self.last_gesture {
            Some((window, at)) => {
                window == id && at.elapsed() <= vivid_protocol::overlay::MAX_CLIPBOARD_GESTURE_AGE
            }
            None => false,
        }
    }

    // ---- interactive lane ------------------------------------------------------------------

    pub(crate) fn renew(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let Ok(renew) = Renew::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay renewal");
        };
        match self.lane_generation.get(&session_id) {
            None => {
                self.lane_generation
                    .insert(session_id, renew.lane_generation);
            }
            Some(&generation) if generation == renew.lane_generation => {}
            Some(_) => return Err("stale overlay lane generation"),
        }
        Ok((messages::OK, Vec::new()))
    }

    pub(crate) fn capture(
        &mut self,
        session_id: u64,
        object_id: u64,
        payload: &PayloadMap,
    ) -> OverlayReply {
        let owner = owner(session_id);
        let Ok(capture) = Capture::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay capture");
        };
        let Ok(id) = capture.address.identity(owner) else {
            return Err("invalid overlay identity");
        };
        let Some(window) = self.windows.get(id) else {
            return Err("overlay window is absent");
        };
        if window.generation != capture.address.generation
            || window.scene_revision != capture.scene_revision
        {
            return Err("stale overlay capture scene");
        }
        if capture.capture {
            // A real presenter uses its own last observed pointer position, never the producer's.
            if self.windows.capture_pointer(id, Point::default()).is_err() {
                return Err("overlay capture was refused");
            }
        } else {
            self.windows.release_pointer(id);
        }
        Ok((messages::OK, Vec::new()))
    }

    // ---- bulk channel ----------------------------------------------------------------------

    /// Accept one vector record. Returns false when the producer broke the channel's contract,
    /// which a real presenter answers by failing the channel rather than the request.
    pub(crate) fn media_record(
        &mut self,
        session_id: u64,
        record_type: u16,
        track_id: u64,
        body: &[u8],
    ) -> bool {
        match record_type {
            messages::VECTOR_FRAME => self.vector_frame(session_id, track_id, body),
            messages::VECTOR_ASSET => self.vector_asset(session_id, track_id, body),
            messages::VECTOR_ASSET_RELEASE => self.release_asset(session_id, track_id, body),
            _ => true,
        }
    }

    fn vector_frame(&mut self, session_id: u64, track_id: u64, body: &[u8]) -> bool {
        let Ok(frame) = Frame::decode(body) else {
            return false;
        };
        if frame.canvas.validate_with_limits(&self.limits).is_err() {
            return false;
        }
        let Some(window) = self.track_window(session_id, track_id) else {
            return false;
        };
        // Revisions advance across every track a window owns, including one being primed.
        let accepted = self.accepted.entry(window).or_insert(0);
        if frame.revision <= *accepted {
            return false;
        }
        *accepted = frame.revision;

        // Anything the replacement supersedes resolves before the new scene publishes.
        if let Some(previous) = self.displayed.get(&window) {
            self.resolve(
                window,
                track_id,
                previous.revision,
                PresentationOutcome::Superseded,
            );
        }
        let generation = self.windows.get(window).map_or(0, |w| w.generation);
        if self
            .windows
            .publish_scene(window, generation, frame.revision)
            .is_err()
        {
            return false;
        }
        // A tree described the scene that was just replaced, so it no longer describes anything.
        self.semantics.remove(&window);
        self.displayed.insert(
            window,
            PresentedScene {
                window,
                revision: frame.revision,
                canvas: frame.canvas,
            },
        );
        self.resolve(
            window,
            track_id,
            frame.revision,
            PresentationOutcome::Presented,
        );
        self.drain_events(owner(session_id));
        true
    }

    fn resolve(
        &mut self,
        window: SurfaceIdentity,
        track_id: u64,
        revision: u64,
        outcome: PresentationOutcome,
    ) {
        let Some(state) = self.windows.get(window) else {
            return;
        };
        let result = SubmissionOutcome {
            submission: Submission {
                address: WindowAddress {
                    context_id: window.context.context_id,
                    surface_id: window.surface_id,
                    generation: state.generation,
                },
                track_id,
                channel_generation: 1,
                epoch: 1,
                revision,
            },
            outcome,
        };
        if let Ok(payload) = result.payload() {
            self.queue_for(
                window.context.session.session_id,
                LaneRecord {
                    record_type: messages::OVERLAY_SUBMISSION_OUTCOME,
                    object_id: window.surface_id,
                    payload,
                },
            );
        }
    }

    fn vector_asset(&mut self, session_id: u64, track_id: u64, body: &[u8]) -> bool {
        if body.len() < 16 {
            return false;
        }
        let read = |at: usize| u32::from_be_bytes(body[at..at + 4].try_into().expect("four bytes"));
        let id = u64::from_be_bytes(body[..8].try_into().expect("eight bytes"));
        let (width, height) = (read(8), read(12));
        let Some(expected) = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return false;
        };
        if id == 0 || body.len() - 16 != expected as usize {
            return false;
        }
        let highest = self
            .highest_asset
            .entry((session_id, track_id))
            .or_insert(0);
        if id <= *highest {
            return false;
        }
        *highest = id;
        self.assets
            .insert((session_id, track_id, id), RetainedAsset { width, height });
        true
    }

    fn release_asset(&mut self, session_id: u64, track_id: u64, body: &[u8]) -> bool {
        let Ok(bytes) = <[u8; 8]>::try_from(body) else {
            return false;
        };
        let id = u64::from_be_bytes(bytes);
        // Displayed scenes keep their references; only future lookup is removed.
        self.assets.remove(&(session_id, track_id, id)).is_some()
    }

    // ---- session lifecycle ------------------------------------------------------------------

    /// Discard every window, track binding, asset, and queued record this session owns, exactly
    /// as a producer disconnect must: an unrelated session that reused the same local context,
    /// surface, track, or asset numbers keeps its own state untouched.
    pub(crate) fn remove_session(&mut self, session_id: u64) {
        let owner = owner(session_id);
        self.windows.revoke_owner(owner);
        self.tracks.retain(|(session, _), _| *session != session_id);
        self.displayed
            .retain(|id, _| id.context.session.session_id != session_id);
        self.accepted
            .retain(|id, _| id.context.session.session_id != session_id);
        self.assets
            .retain(|(session, _, _), _| *session != session_id);
        self.highest_asset
            .retain(|(session, _), _| *session != session_id);
        self.semantics
            .retain(|id, _| id.context.session.session_id != session_id);
        if self
            .last_gesture
            .is_some_and(|(id, _)| id.context.session.session_id == session_id)
        {
            self.last_gesture = None;
        }
        self.pending.remove(&session_id);
        self.lane_generation.remove(&session_id);
        self.viewports.remove(&session_id);
        self.active_sessions.remove(&session_id);
    }

    // ---- observation (test-facing today; a production reader arrives with vvmux's compositor) --

    /// The cursor the pointer currently calls for, through the same state machine a real host
    /// drives.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn cursor(&self) -> Option<vivid_protocol::vector::CursorShape> {
        self.windows.cursor()
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn scenes(&self) -> Vec<PresentedScene> {
        self.displayed.values().cloned().collect()
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn assets(&self) -> Vec<RetainedAsset> {
        self.assets.values().copied().collect()
    }

    /// Clipboard text this presenter accepted, in acceptance order.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn clipboard(&self) -> &[String] {
        &self.clipboard
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn focused(&self) -> Option<SurfaceIdentity> {
        self.windows.focus()
    }

    /// One window's current bounds, mode, visibility, generation and revision, if it exists. Read
    /// by the projection snapshot so a terminating bridge can re-issue `SET_OVERLAY_WINDOW` in its
    /// own outer coordinate space.
    pub(crate) fn window(&self, id: SurfaceIdentity) -> Option<&vivid_protocol::overlay::Window> {
        self.windows.get(id)
    }

    // ---- interactive lane and bulk-channel entry points, shared by every host -----------------

    /// The display lists belonging to a given set of owners.
    ///
    /// Input is dispatched against exactly these. One presenter serves many panes, and a pane is
    /// its own coordinate space: without this filter a click at a pane-local position could hit a
    /// window that happens to sit at the same position in a different pane.
    fn scenes_of(&self, sessions: &[u64]) -> Vec<PresentedScene> {
        self.displayed
            .values()
            .filter(|scene| sessions.contains(&scene.window.context.session.session_id))
            .cloned()
            .collect()
    }

    fn drain_events_of(&mut self, sessions: &[u64]) {
        for session_id in sessions {
            self.drain_events(owner(*session_id));
        }
    }

    /// Whether the focused window belongs to one of these owners. Keyboard input follows focus
    /// rather than a position, so routing it for a pane that does not hold the focused window
    /// would deliver a keystroke to another pane's producer.
    pub(crate) fn focus_belongs_to(&self, sessions: &[u64]) -> bool {
        self.windows
            .focus()
            .is_some_and(|id| sessions.contains(&id.context.session.session_id))
    }

    /// Deliver a pointer event through the same hit test a real host performs, against the
    /// window's last published display list.
    pub(crate) fn pointer(
        &mut self,
        sessions: &[u64],
        position: Point,
        button: Option<(u16, bool)>,
        modifiers: u32,
        pressure: Option<vivid_protocol::vector::Scalar>,
    ) -> bool {
        let clicks = self.count_clicks(position, button);
        let scenes = self.scenes_of(sessions);
        let consumed = self.windows.pointer(
            PointerReport {
                position,
                button,
                modifiers,
                clicks,
                pressure,
            },
            |id, point| {
                scenes
                    .iter()
                    .find(|scene| scene.window == id)
                    .and_then(|scene| hit(&scene.canvas, point))
            },
        );
        if consumed && button.is_some_and(|(_, down)| down) {
            // Read after the dispatch: that is what settles which window the press reached. A
            // press that reached no window clears any gesture a producer might otherwise bank.
            self.last_gesture = self
                .windows
                .hovered_window()
                .map(|id| (id, std::time::Instant::now()));
        }
        self.drain_events_of(sessions);
        consumed
    }

    pub(crate) fn wheel(
        &mut self,
        sessions: &[u64],
        position: Point,
        scroll: Scroll,
        modifiers: u32,
    ) -> bool {
        let scenes = self.scenes_of(sessions);
        let consumed = self
            .windows
            .wheel(position, scroll, modifiers, |id, point| {
                scenes
                    .iter()
                    .find(|scene| scene.window == id)
                    .and_then(|scene| hit(&scene.canvas, point))
                    .map(|target| (target.id, target.role))
            });
        self.drain_events_of(sessions);
        consumed
    }

    pub(crate) fn keyboard(&mut self, sessions: &[u64], event: Event, escape: bool) -> bool {
        if !self.focus_belongs_to(sessions) {
            return false;
        }
        if matches!(event, Event::Key { down: true, .. }) {
            self.last_gesture = self
                .windows
                .focus()
                .map(|id| (id, std::time::Instant::now()));
        }
        let consumed = self.windows.keyboard(event, escape);
        self.drain_events_of(sessions);
        consumed
    }

    /// Tell one pane's windows that the pane gained or lost the terminal's focus.
    ///
    /// One host serves every pane, so the move only belongs to this pane when the focus it carries
    /// does: losing focus is the pane that holds it, and regaining it is the pane whose window
    /// would come back. Asking the state machine instead of tracking a second copy is what keeps a
    /// pane from pulling another pane's window in or out of focus.
    pub(crate) fn set_pane_focus(&mut self, sessions: &[u64], focused: bool) {
        let moved = if focused {
            self.windows.restorable_focus()
        } else {
            self.windows.focus()
        };
        if !moved.is_some_and(|id| sessions.contains(&id.context.session.session_id)) {
            return;
        }
        self.windows.set_pane_focus(focused);
        self.drain_events_of(sessions);
    }
}

/// The hit region a point falls in, with the protocol's last-region-wins precedence.
///
/// This walks the display list rather than a compiled scene: a headless presenter has no
/// rasterizer, and an axis-aligned bounding test is enough for the region identity a producer's
/// event routing depends on.
fn hit(canvas: &Canvas, point: Point) -> Option<vivid_protocol::vector::HitRegion> {
    use vivid_protocol::vector::{Command, Segment};
    let mut found = None;
    for command in canvas.commands() {
        let Command::Hit {
            id,
            path,
            role,
            cursor,
        } = command
        else {
            continue;
        };
        let (mut left, mut top) = (f64::MAX, f64::MAX);
        let (mut right, mut bottom) = (f64::MIN, f64::MIN);
        for segment in &path.segments {
            let points: &[Point] = match segment {
                Segment::Move(a) | Segment::Line(a) => std::slice::from_ref(a),
                Segment::Quad(a, b) => &[*a, *b],
                Segment::Cubic(a, b, c) => &[*a, *b, *c],
                Segment::Close => &[],
            };
            for p in points {
                left = left.min(p.x.get());
                right = right.max(p.x.get());
                top = top.min(p.y.get());
                bottom = bottom.max(p.y.get());
            }
        }
        let (x, y) = (point.x.get(), point.y.get());
        if x >= left && x < right && y >= top && y < bottom {
            found = Some(vivid_protocol::vector::HitRegion {
                id: *id,
                role: *role,
                cursor: *cursor,
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::overlay::{WindowMode, WindowOptions};
    use vivid_protocol::vector::{Brush, Color, Path, Rect};

    /// Both owners these tests use are given the same pane geometry, as two producers in one pane
    /// would have.
    fn viewport() -> Overlays {
        let mut overlays = Overlays::default();
        overlays.install_viewport(1, 800., 600.);
        overlays.install_viewport(2, 800., 600.);
        overlays
    }

    fn sized(width: f64, height: f64) -> Viewport {
        Viewport {
            width: Scalar::new(width).expect("width"),
            height: Scalar::new(height).expect("height"),
            scale_numerator: 1,
            scale_denominator: 1,
        }
    }

    fn window(overlays: &mut Overlays, session_id: u64) -> SurfaceIdentity {
        // Both owners deliberately name context 1, surface 1 and track 5.
        let id = owner(session_id).context(1).unwrap().surface(1).unwrap();
        overlays.note_track(session_id, 5, 1, 1);
        overlays
            .windows
            .create(
                id,
                1,
                WindowOptions::new(Rect::new(0., 0., 100., 100.).unwrap(), WindowMode::Floating),
            )
            .unwrap();
        id
    }

    fn scene(colour: u32, revision: u64) -> Vec<u8> {
        let mut canvas = Canvas::new();
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 10., 10.).unwrap()).unwrap(),
                Brush::Solid(Color(colour)),
            )
            .unwrap();
        Frame {
            epoch: 1,
            revision,
            canvas,
        }
        .encode()
        .unwrap()
    }

    fn asset(id: u64, width: u32, height: u32) -> Vec<u8> {
        let mut body = id.to_be_bytes().to_vec();
        body.extend(width.to_be_bytes());
        body.extend(height.to_be_bytes());
        body.extend(vec![0_u8; (width * height * 4) as usize]);
        body
    }

    #[test]
    fn two_owners_reusing_local_ids_keep_separate_scenes_and_assets() {
        // Local numeric IDs are not identities. Both owners use context 1, surface 1, track 5,
        // and asset 1; neither may observe, overwrite, or release the other's resources.
        let mut overlays = viewport();
        let first = window(&mut overlays, 1);
        let second = window(&mut overlays, 2);
        assert_ne!(first, second);

        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &scene(0xff0000ff, 1)));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &scene(0x0000ffff, 1)));
        assert_eq!(overlays.scenes().len(), 2);

        assert!(overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(1, 2, 2)));
        assert!(overlays.media_record(2, messages::VECTOR_ASSET, 5, &asset(1, 4, 4)));
        let mut sizes: Vec<u32> = overlays.assets().iter().map(|a| a.width).collect();
        sizes.sort_unstable();
        assert_eq!(
            sizes,
            vec![2, 4],
            "one owner's upload must not replace another's"
        );

        // Releasing the first owner's asset leaves the second owner's in place.
        assert!(overlays.media_record(1, messages::VECTOR_ASSET_RELEASE, 5, &1_u64.to_be_bytes()));
        assert_eq!(overlays.assets().len(), 1);
        assert_eq!(overlays.assets()[0].width, 4);

        // And the first owner's revision counter did not advance the second owner's window.
        assert_eq!(overlays.accepted.get(&first), Some(&1));
        assert_eq!(overlays.accepted.get(&second), Some(&1));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &scene(0x00ff00ff, 2)));
        assert_eq!(overlays.accepted.get(&first), Some(&1));
        assert_eq!(overlays.accepted.get(&second), Some(&2));
    }

    #[test]
    fn a_stale_revision_is_refused_without_publishing_anything() {
        let mut overlays = viewport();
        let id = window(&mut overlays, 1);
        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &scene(0xff0000ff, 4)));
        // Replaying an already accepted revision must not republish or resolve a receipt.
        assert!(!overlays.media_record(1, messages::VECTOR_FRAME, 5, &scene(0x00ff00ff, 4)));
        assert_eq!(overlays.accepted.get(&id), Some(&4));
        assert_eq!(overlays.scenes().len(), 1);
    }

    #[test]
    fn an_asset_id_never_repeats_within_a_channel_generation() {
        let mut overlays = viewport();
        window(&mut overlays, 1);
        assert!(overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(2, 1, 1)));
        // Strictly increasing, including after release: a receiver need not retain retired IDs.
        assert!(!overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(2, 1, 1)));
        assert!(!overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(1, 1, 1)));
        assert!(overlays.media_record(1, messages::VECTOR_ASSET_RELEASE, 5, &2_u64.to_be_bytes()));
        assert!(!overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(2, 1, 1)));
        assert!(overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(3, 1, 1)));
    }

    #[test]
    fn an_environment_change_reaches_every_session_that_has_talked_to_this_host() {
        // The environment describes the host itself, so a change reaches every session with
        // overlay state here — but not one this host has never heard from.
        let mut overlays = Overlays::default();
        overlays.note_track(1, 5, 1, 1);
        overlays.note_track(2, 5, 1, 1);
        overlays.set_environment(Environment {
            appearance: vivid_protocol::overlay::wire::Appearance::Dark,
            ..Default::default()
        });
        assert_eq!(overlays.take_pending(1).len(), 1, "session 1 is told");
        assert_eq!(overlays.take_pending(2).len(), 1, "session 2 is told too");
        assert_eq!(
            overlays.take_pending(3).len(),
            0,
            "an unrelated session is not"
        );
    }

    #[test]
    fn a_lane_that_opens_is_owed_the_environment_before_the_viewport() {
        // The spec requires an initial snapshot after lane authentication, and a producer that
        // adopts the host's font wants it before it lays anything out.
        let mut overlays = Overlays::default();
        overlays.install_viewport(1, 320., 240.);
        overlays.open_lane(1);
        let queued: Vec<u16> = overlays
            .take_pending(1)
            .into_iter()
            .map(|record| record.record_type)
            .collect();
        assert_eq!(
            queued,
            vec![
                messages::OVERLAY_ENV_CHANGED,
                messages::OVERLAY_VIEWPORT_CHANGED
            ]
        );
    }

    #[test]
    fn a_viewport_belongs_to_one_session_and_never_moves_another_pane() {
        // One presenter serves many panes. Resizing one must neither relayout another pane's
        // windows nor tell its producer anything.
        let mut overlays = Overlays::default();
        overlays.install_viewport(1, 320., 240.);
        overlays.install_viewport(2, 800., 600.);
        assert_eq!(
            overlays.take_pending(1).len(),
            0,
            "seeding queues nothing; the lane's initial snapshot carries it"
        );
        let seeded = overlays.viewport_of(1).expect("session 1 viewport");
        assert_eq!(seeded.viewport.width.get(), 320.);
        assert_eq!(
            overlays
                .viewport_of(2)
                .expect("session 2 viewport")
                .viewport,
            sized(800., 600.)
        );

        overlays.set_viewport(1, sized(400., 300.));
        assert_eq!(
            overlays.take_pending(1).len(),
            1,
            "the resized session is told"
        );
        assert_eq!(
            overlays.take_pending(2).len(),
            0,
            "the other pane's session is not"
        );
        let resized = overlays.viewport_of(1).expect("resized");
        assert_eq!(resized.viewport, sized(400., 300.));
        assert!(
            resized.revision > seeded.revision,
            "a change advances the revision"
        );
        assert_eq!(
            overlays.viewport_of(2).expect("untouched").viewport,
            sized(800., 600.),
            "the other pane keeps its own geometry"
        );

        // Unchanged geometry neither advances the revision nor queues a record.
        overlays.set_viewport(1, resized.viewport);
        assert_eq!(overlays.take_pending(1).len(), 0);
        assert_eq!(
            overlays.viewport_of(1).expect("unchanged").revision,
            resized.revision
        );
    }

    /// A scene whose whole window is one hit region, so a pointer inside the window lands on it.
    fn hit_scene(revision: u64) -> Vec<u8> {
        let path = Path::rectangle(Rect::new(0., 0., 100., 100.).unwrap()).unwrap();
        let mut canvas = Canvas::new();
        canvas
            .fill(path.clone(), Brush::Solid(Color(0x203050ff)))
            .unwrap();
        canvas
            .push(vivid_protocol::vector::Command::Hit {
                id: 7,
                path,
                role: vivid_protocol::vector::HitRole::Input,
                cursor: None,
            })
            .unwrap();
        Frame {
            epoch: 1,
            revision,
            canvas,
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn a_pointer_event_reaches_only_the_pane_it_was_routed_to() {
        // Two panes' windows sit at the same pane-local position, because a pane is its own
        // coordinate space. Dispatch is scoped to the routed owner, or a click in one pane would
        // land in another pane's window.
        let mut overlays = viewport();
        let first = window(&mut overlays, 1);
        let second = window(&mut overlays, 2);
        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        let _ = overlays.take_pending(1);
        let _ = overlays.take_pending(2);

        let position = Point::new(5., 5.).expect("position");
        let press = Some((vivid_protocol::overlay::buttons::PRIMARY, true));
        assert!(
            overlays.pointer(&[1], position, press, 0, None),
            "the routed owner's window is hit"
        );
        assert!(
            !overlays.take_pending(1).is_empty(),
            "the routed owner is told"
        );
        assert!(
            overlays.take_pending(2).is_empty(),
            "the other pane's owner hears nothing about it"
        );
        assert_eq!(overlays.focused(), Some(first));

        assert!(overlays.pointer(&[2], position, press, 0, None));
        assert!(!overlays.take_pending(2).is_empty());
        assert_eq!(overlays.focused(), Some(second));

        // A pane with no overlay window of its own consumes nothing at that same position.
        assert!(!overlays.pointer(&[3], position, press, 0, None));
    }

    #[test]
    fn a_keystroke_reaches_only_the_pane_whose_window_holds_focus() {
        let mut overlays = viewport();
        let first = window(&mut overlays, 1);
        window(&mut overlays, 2);
        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        let position = Point::new(5., 5.).expect("position");
        let press = Some((vivid_protocol::overlay::buttons::PRIMARY, true));
        assert!(overlays.pointer(&[1], position, press, 0, None));
        assert_eq!(overlays.focused(), Some(first));

        let key = Event::Key {
            physical: 0x04,
            down: true,
            repeat: false,
            modifiers: 0,
        };
        assert!(
            !overlays.keyboard(&[2], key.clone(), false),
            "a pane that does not hold the focused window must not swallow a keystroke"
        );
        assert!(
            overlays.keyboard(&[1], key, false),
            "the pane holding focus takes it"
        );
    }

    #[test]
    fn a_pane_losing_the_terminal_focus_only_moves_its_own_window_and_can_get_it_back() {
        let mut overlays = viewport();
        let first = window(&mut overlays, 1);
        window(&mut overlays, 2);
        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &hit_scene(1)));
        let position = Point::new(5., 5.).expect("position");
        let press = Some((vivid_protocol::overlay::buttons::PRIMARY, true));
        assert!(overlays.pointer(&[1], position, press, 0, None));
        assert_eq!(overlays.focused(), Some(first));

        // The other pane is told it gained focus first, which is the order a focus move arrives in
        // when the gaining pane sorts ahead of the losing one.
        overlays.set_pane_focus(&[2], true);
        assert_eq!(
            overlays.focused(),
            Some(first),
            "another pane gaining focus must not pull the focused window away from its owner"
        );
        overlays.set_pane_focus(&[2], false);
        assert_eq!(
            overlays.focused(),
            Some(first),
            "nor may it push that window out of focus"
        );

        overlays.set_pane_focus(&[1], false);
        assert_eq!(
            overlays.focused(),
            None,
            "the owning pane's loss reaches its window"
        );
        overlays.set_pane_focus(&[2], true);
        assert_eq!(
            overlays.focused(),
            None,
            "an unrelated pane cannot claim the remembered focus"
        );
        overlays.set_pane_focus(&[1], true);
        assert_eq!(
            overlays.focused(),
            Some(first),
            "the owning pane gets its window's focus back"
        );
    }

    #[test]
    fn removing_a_session_does_not_disturb_another_owner_reusing_the_same_local_ids() {
        let mut overlays = viewport();
        let first = window(&mut overlays, 1);
        let second = window(&mut overlays, 2);
        assert!(overlays.media_record(1, messages::VECTOR_FRAME, 5, &scene(0xff0000ff, 1)));
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &scene(0x0000ffff, 1)));
        assert!(overlays.media_record(1, messages::VECTOR_ASSET, 5, &asset(1, 2, 2)));
        assert!(overlays.media_record(2, messages::VECTOR_ASSET, 5, &asset(1, 4, 4)));

        overlays.remove_session(1);

        assert!(
            overlays.windows.get(first).is_none(),
            "removed owner's window is gone"
        );
        assert!(
            overlays.windows.get(second).is_some(),
            "unrelated owner's window survives"
        );
        assert_eq!(overlays.scenes().len(), 1);
        assert_eq!(overlays.scenes()[0].window, second);
        assert_eq!(overlays.assets().len(), 1, "removed owner's asset is gone");
        assert_eq!(overlays.assets()[0].width, 4);

        // The surviving owner can still commit a subsequent update.
        assert!(overlays.media_record(2, messages::VECTOR_FRAME, 5, &scene(0x00ff00ff, 2)));
        assert_eq!(overlays.accepted.get(&second), Some(&2));
    }
}
