//! Headless overlay state for [`TestPresenter`](super::presenter::TestPresenter).
//!
//! A producer-side overlay toolkit needs somewhere to run that is not a GPU. This is that place:
//! it accepts the overlay profile bundle, keeps window, focus and revision state in the protocol's
//! own [`Windows`] state machine, and validates every display list against the negotiated limits.
//! It composes nothing — "presented" here means the list decoded, validated, and became the
//! window's published scene, which is the part a layout or hit-testing regression depends on.
//!
//! Sharing [`Windows`] with the real host is deliberate. A private reimplementation of focus,
//! modal eligibility and revision ordering would agree with itself and with nothing else.

use std::collections::HashMap;

use vivid_protocol::cbor::Value;
use vivid_protocol::identity::{PresenterInstanceId, SessionIdentity, SurfaceIdentity};
use vivid_protocol::messages::{self, PayloadMap};
use vivid_protocol::overlay::wire::Environment;
use vivid_protocol::overlay::wire::{
    Action, Capture, Clipboard, PresentationOutcome, Query, Renew, SetWindow, Status, Submission,
    SubmissionOutcome, Viewport, WindowAddress,
};
use vivid_protocol::overlay::{Event, PointerReport, Scroll, Windows};
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

#[derive(Default)]
pub(crate) struct Overlays {
    windows: Windows,
    viewport: Option<Viewport>,
    viewport_revision: u64,
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
    lane_generation: Option<u64>,
    pending: Vec<LaneRecord>,
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
    /// The viewport a producer sees before anything resizes it.
    /// Queue the environment for a lane that just opened, as a host does after authentication.
    pub(crate) fn queue_initial_environment(&mut self) {
        self.queue_environment();
    }

    pub(crate) fn install_viewport(&mut self, width: f64, height: f64) {
        let viewport = Viewport {
            width: Scalar::new(width).expect("test viewport width"),
            height: Scalar::new(height).expect("test viewport height"),
            scale_numerator: 1,
            scale_denominator: 1,
        };
        self.set_viewport(viewport);
    }

    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        if self.viewport == Some(viewport) {
            return;
        }
        self.viewport = Some(viewport);
        self.viewport_revision += 1;
        self.queue_viewport();
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

    fn queue_environment(&mut self) {
        let update = vivid_protocol::overlay::wire::EnvironmentChanged {
            revision: self.environment_revision.max(1),
            environment: self.environment.clone(),
        };
        if let Ok(payload) = update.payload() {
            self.pending.push(LaneRecord {
                record_type: messages::OVERLAY_ENV_CHANGED,
                object_id: 0,
                payload,
            });
        }
    }

    /// The environment a producer sees.
    pub(crate) fn environment(&self) -> &Environment {
        &self.environment
    }

    fn queue_viewport(&mut self) {
        let Some(viewport) = self.viewport else {
            return;
        };
        let changed = vivid_protocol::overlay::wire::ViewportChanged {
            revision: self.viewport_revision,
            viewport,
        };
        if let Ok(payload) = changed.payload() {
            self.pending.push(LaneRecord {
                record_type: messages::OVERLAY_VIEWPORT_CHANGED,
                object_id: 0,
                payload,
            });
        }
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Records the producer is owed on its interactive lane, drained in arrival order.
    pub(crate) fn take_pending(&mut self) -> Vec<LaneRecord> {
        std::mem::take(&mut self.pending)
    }

    /// Queue every window event the protocol state machine produced for this owner.
    fn drain_events(&mut self, owner: SessionIdentity) {
        while let Some(event) = self.windows.take_event(owner) {
            let input = vivid_protocol::overlay::wire::InputEvent::from(event);
            let object_id = input.address.surface_id;
            if let Ok(payload) = input.payload() {
                self.pending.push(LaneRecord {
                    record_type: messages::OVERLAY_INPUT_EVENT,
                    object_id,
                    payload,
                });
            }
        }
    }

    // ---- control records -------------------------------------------------------------------

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
        let Some(viewport) = self.viewport else {
            return Err("overlay viewport is absent");
        };
        if self.windows.apply_action(owner, action, viewport).is_err() {
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
        let Some(viewport) = self.viewport else {
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
            viewport,
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
            viewport_revision: self.viewport_revision,
            accepted_revision: self.accepted.get(&id).copied().unwrap_or(0),
        };
        status
            .payload(owner)
            .map(|payload| (messages::OVERLAY_STATUS, payload))
            .map_err(|_| "overlay status could not be encoded")
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

    pub(crate) fn renew(&mut self, object_id: u64, payload: &PayloadMap) -> OverlayReply {
        let Ok(renew) = Renew::decode(object_id, &Value::Map(payload.clone())) else {
            return Err("invalid overlay renewal");
        };
        match self.lane_generation {
            None => self.lane_generation = Some(renew.lane_generation),
            Some(generation) if generation == renew.lane_generation => {}
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
            self.pending.push(LaneRecord {
                record_type: messages::OVERLAY_SUBMISSION_OUTCOME,
                object_id: window.surface_id,
                payload,
            });
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

    // ---- test-facing observation and injection ---------------------------------------------

    /// The cursor the pointer currently calls for, through the same state machine a real host
    /// drives.
    pub(crate) fn cursor(&self) -> Option<vivid_protocol::vector::CursorShape> {
        self.windows.cursor()
    }

    pub(crate) fn scenes(&self) -> Vec<PresentedScene> {
        self.displayed.values().cloned().collect()
    }

    pub(crate) fn assets(&self) -> Vec<RetainedAsset> {
        self.assets.values().copied().collect()
    }

    /// Clipboard text this presenter accepted, in acceptance order.
    pub(crate) fn clipboard(&self) -> &[String] {
        &self.clipboard
    }

    pub(crate) fn focused(&self) -> Option<SurfaceIdentity> {
        self.windows.focus()
    }

    /// Deliver a pointer event through the same hit test a real host performs, against the
    /// window's last published display list.
    pub(crate) fn pointer(
        &mut self,
        session_id: u64,
        position: Point,
        button: Option<(u16, bool)>,
        modifiers: u32,
    ) -> bool {
        let clicks = self.count_clicks(position, button);
        let scenes: Vec<PresentedScene> = self.displayed.values().cloned().collect();
        let consumed = self.windows.pointer(
            PointerReport {
                position,
                button,
                modifiers,
                clicks,
                pressure: None,
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
        self.drain_events(owner(session_id));
        consumed
    }

    pub(crate) fn wheel(
        &mut self,
        session_id: u64,
        position: Point,
        scroll: Scroll,
        modifiers: u32,
    ) -> bool {
        let scenes: Vec<PresentedScene> = self.displayed.values().cloned().collect();
        let consumed = self
            .windows
            .wheel(position, scroll, modifiers, |id, point| {
                scenes
                    .iter()
                    .find(|scene| scene.window == id)
                    .and_then(|scene| hit(&scene.canvas, point))
                    .map(|target| (target.id, target.role))
            });
        self.drain_events(owner(session_id));
        consumed
    }

    pub(crate) fn keyboard(&mut self, session_id: u64, event: Event, escape: bool) -> bool {
        if matches!(event, Event::Key { down: true, .. }) {
            self.last_gesture = self
                .windows
                .focus()
                .map(|id| (id, std::time::Instant::now()));
        }
        let consumed = self.windows.keyboard(event, escape);
        self.drain_events(owner(session_id));
        consumed
    }

    pub(crate) fn set_pane_focus(&mut self, session_id: u64, focused: bool) {
        self.windows.set_pane_focus(focused);
        self.drain_events(owner(session_id));
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

    fn viewport() -> Overlays {
        let mut overlays = Overlays::default();
        overlays.install_viewport(800., 600.);
        overlays
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
}
