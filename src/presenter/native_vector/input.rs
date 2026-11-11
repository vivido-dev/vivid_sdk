//! Display-qualified input with stable owner bindings for held transitions.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};
use vivid_protocol::{
    overlay::{
        Event, MAX_EVENT_TEXT_BYTES, PointerReport, Scroll, ScrollPhase, buttons, keys, modifiers,
    },
    vector::{Point, Scalar},
};

// Matches the presenter's ordered connection queue. Motion coalesces; transitions
// keep their order. Exhaustion revokes this pane instead of dropping a release.
const MAX_PENDING: usize = 64;
// USB HID keyboard-page Escape, shared with the window manager's dismissal policy.
const ESCAPE: u32 = 0x29;

/// An opaque input binding to one actually displayed pane composition.
#[derive(Clone)]
pub struct VectorInputTarget(Arc<Target>);
struct Target {
    presenter: PresenterInstanceId,
    host: Weak<AtomicBool>,
    visibility: u64,
    pane: PaneId,
    serial: u64,
    epoch: u64,
    metrics: u64,
    owners: Vec<u64>,
    resumes: Vec<(u64, u64)>,
    lanes: HashMap<u64, Weak<Writer>>,
    windows: HashMap<SurfaceIdentity, Scope>,
    retired: AtomicBool,
}
#[derive(Clone)]
struct Scope {
    window: SurfaceIdentity,
    source: SourceKey,
    pane: PaneId,
    resume: u64,
    generation: u64,
    channel: u64,
    writer: Weak<Writer>,
    lane: Option<Weak<Writer>>,
}
impl std::fmt::Debug for VectorInputTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorInputTarget").finish_non_exhaustive()
    }
}

/// Maximum UTF-8 bytes in one bounded native text transfer.
pub const MAX_VECTOR_TEXT_BYTES: usize = vivid_protocol::overlay::MAX_CLIPBOARD_BYTES;

/// Pins a displayed window and focus grant for an asynchronous text transfer.
///
/// This retains no compiled scene or frame receipt. It survives ordinary repaint
/// while rejecting focus round trips, input-lane replacement and host invalidation.
#[derive(Clone)]
pub struct VectorTextTarget(Held);
impl std::fmt::Debug for VectorTextTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorTextTarget").finish_non_exhaustive()
    }
}
impl VectorTextTarget {
    /// Compares the complete window, focus and input grant across repaint.
    pub fn same_focus(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        let (Some(x), Some(y)) = (&a.scope, &b.scope) else {
            return false;
        };
        a.focus_generation == b.focus_generation
            && a.target.0.presenter == b.target.0.presenter
            && Weak::ptr_eq(&a.target.0.host, &b.target.0.host)
            && a.target.0.visibility == b.target.0.visibility
            && a.target.0.epoch == b.target.0.epoch
            && a.target.0.metrics == b.target.0.metrics
            && x.window == y.window
            && x.source == y.source
            && x.pane == y.pane
            && x.resume == y.resume
            && x.generation == y.generation
            && x.channel == y.channel
            && Weak::ptr_eq(&x.writer, &y.writer)
            && match (&x.lane, &y.lane) {
                (Some(a), Some(b)) => Weak::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// Cached native editor state for one immutable text destination.
#[derive(Clone, Debug)]
pub struct VectorEditorState {
    /// The original window and focus grant retained on native callbacks.
    pub target: VectorTextTarget,
    /// Clipped candidate rectangle normalized to the pane, or no current geometry.
    pub caret: Option<[f64; 4]>,
}

/// An atomic committed-text and preedit update for a native input method.
#[derive(Clone, Default)]
pub struct VectorImeEdit {
    /// Optional committed text; clears the prior preedit before insertion.
    pub commit: Option<String>,
    /// Replacement preedit, at most 4,096 UTF-8 bytes; empty text clears it.
    pub preedit: String,
    /// Ordered UTF-8 byte offsets within preedit, or an invisible cursor.
    pub selection: Option<std::ops::Range<usize>>,
}
impl std::fmt::Debug for VectorImeEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorImeEdit")
            .field("commit_bytes", &self.commit.as_ref().map(String::len))
            .field("preedit_bytes", &self.preedit.len())
            .finish_non_exhaustive()
    }
}

/// One bounded physical input report in pane-local logical coordinates.
pub enum VectorInputEvent {
    /// Motion or a normative button transition; held reports retain their press target.
    Pointer {
        position: [f64; 2],
        button: Option<(u16, bool)>,
        modifiers: u32,
    },
    /// Logical wheel deltas at a pane-local position.
    Wheel {
        position: [f64; 2],
        delta: [f64; 2],
        modifiers: u32,
    },
    /// USB usage 4–231 and optional text, dispatched atomically to the same window.
    Key {
        usage: u32,
        down: bool,
        repeat: bool,
        modifiers: u32,
        text: Option<String>,
    },
    /// Committed text independent of a physical key, bounded to 4,096 UTF-8 bytes.
    Text(String),
}
impl std::fmt::Debug for VectorInputEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pointer { .. } => "VectorInputEvent::Pointer",
            Self::Wheel { .. } => "VectorInputEvent::Wheel",
            Self::Key { .. } => "VectorInputEvent::Key",
            Self::Text(_) => "VectorInputEvent::Text",
        })
    }
}
/// Whether a report was delivered, retained for presentation, or rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorInputResult {
    /// The host consumed the report; `text` reports committed text enqueueing.
    Delivered { text: bool },
    /// Matching display or earlier input must complete. The called API documents
    /// whether the SDK queues the operation or the caller must retry it.
    Deferred,
    /// No report reached a producer and no replacement target was substituted.
    Rejected,
}
use VectorInputResult::{Deferred, Delivered, Rejected};

#[derive(Clone)]
struct Held {
    target: VectorInputTarget,
    scope: Option<Scope>,
    owner: SessionIdentity,
    focus_generation: u64,
}
struct Grab {
    held: Held,
    region: u64,
    buttons: BTreeSet<u16>,
}
struct Pending {
    target: VectorInputTarget,
    event: PendingEvent,
    focus: Option<u64>,
}
enum PendingEvent {
    Physical(VectorInputEvent),
    Ime(Box<(VectorTextTarget, VectorImeEdit)>),
    Text(Box<(VectorTextTarget, String, std::time::Instant)>),
}
#[derive(Default)]
pub(super) struct InputState {
    epochs: HashMap<PaneId, u64>,
    current: HashMap<PaneId, VectorInputTarget>,
    keys: BTreeMap<(PaneId, u32), Held>,
    grabs: HashMap<PaneId, Grab>,
    pending: VecDeque<Pending>,
    pointers: HashMap<PaneId, Point>,
}
impl InputState {
    pub(super) fn prune(&mut self, state: &mut State) -> BTreeSet<u64> {
        let valid = |held: &Held| {
            self.base(state, &held.target)
                && if let Some(scope) = &held.scope {
                    scope.live(state)
                        && state.overlays.native_eligible(scope.window)
                        && state.overlays.native_focus(scope.pane) == Some(scope.window)
                        && state.overlays.native_focus_generation(scope.pane)
                            == Some(held.focus_generation)
                } else {
                    held.target.0.owner_live(state, held.owner)
                }
        };
        let removed: Vec<_> = self
            .keys
            .iter()
            .filter(|(_, held)| !valid(held))
            .map(|(usage, held)| (*usage, held.clone()))
            .collect();
        let remove_grabs: Vec<_> = self
            .grabs
            .iter()
            .filter(|(_, grab)| !valid(&grab.held))
            .map(|(pane, _)| *pane)
            .collect();
        let mut cancelled = BTreeSet::new();
        for (usage, held) in removed {
            if let Some(scope) = &held.scope
                && scope.live(state)
                && cancelled.insert(scope.window)
            {
                self.cancel(state, &held);
            }
            self.keys.remove(&usage);
        }
        for pane in remove_grabs {
            if let Some(grab) = self.grabs.remove(&pane)
                && let Some(scope) = &grab.held.scope
                && scope.live(state)
                && cancelled.insert(scope.window)
            {
                self.cancel(state, &grab.held);
            }
        }
        cancelled
            .iter()
            .map(|window| window.context().session().session_id())
            .collect()
    }
    pub(super) fn invalidate(&mut self, pane: PaneId) {
        let epoch = self.epochs.entry(pane).or_insert(1);
        *epoch = if *epoch == 0 {
            0
        } else {
            epoch.checked_add(1).unwrap_or(0)
        };
        self.keys.retain(|_, held| held.target.0.pane != pane);
        self.grabs.remove(&pane);
        self.pending.retain(|pending| pending.target.0.pane != pane);
        self.current.remove(&pane);
        self.pointers.remove(&pane);
    }
    fn base(&self, state: &State, target: &VectorInputTarget) -> bool {
        target.0.presenter == state.presenter
            && target
                .0
                .host
                .upgrade()
                .is_some_and(|alive| alive.load(Ordering::Acquire))
            && !state.native_pane_hidden(target.0.pane)
            && state
                .native_visibility
                .get(&target.0.pane)
                .copied()
                .unwrap_or(0)
                == target.0.visibility
            && target.0.epoch != 0
            && self.epochs.get(&target.0.pane).copied().unwrap_or(1) == target.0.epoch
            && state
                .metrics
                .get(&target.0.pane)
                .is_some_and(|metrics| metrics.generation == target.0.metrics)
    }
    fn fresh(&self, state: &State, target: &VectorInputTarget) -> bool {
        self.base(state, target)
            && !target.0.retired.load(Ordering::Acquire)
            && self
                .current
                .get(&target.0.pane)
                .is_some_and(|current| Arc::ptr_eq(&current.0, &target.0))
            && state.overlays.native_ready(&target.0.owners)
    }
    fn cancel(&mut self, state: &mut State, held: &Held) {
        if let Some(scope) = &held.scope
            && scope.live(state)
        {
            state
                .overlays
                .native_cancel(scope.window, held.focus_generation);
        }
    }
    fn scope_ready(&mut self, state: &mut State, held: &Held) -> VectorInputResult {
        let Some(scope) = &held.scope else {
            return if self.base(state, &held.target) && held.target.0.owner_live(state, held.owner)
            {
                Delivered { text: false }
            } else {
                Rejected
            };
        };
        if !self.base(state, &held.target)
            || !scope.live(state)
            || !state.overlays.native_eligible(scope.window)
            || state.overlays.native_focus(scope.pane) != Some(scope.window)
            || state.overlays.native_focus_generation(scope.pane) != Some(held.focus_generation)
        {
            self.cancel(state, held);
            return Rejected;
        }
        if !state.overlays.native_window_ready(scope.window) {
            return Deferred;
        }
        Delivered { text: false }
    }
    fn focused(&self, state: &State, target: &VectorInputTarget) -> Option<Scope> {
        if !self.fresh(state, target) {
            return None;
        }
        let id = state.overlays.native_focus(target.0.pane)?;
        let scope = target.0.windows.get(&id)?;
        (scope.live(state)
            && state.overlays.native_eligible(id)
            && state.overlays.native_window_ready(id))
        .then(|| scope.clone())
    }
    fn current_windows(&self, state: &State, pane: PaneId) -> Vec<SurfaceIdentity> {
        self.current
            .get(&pane)
            .filter(|target| self.fresh(state, target))
            .into_iter()
            .flat_map(|target| target.0.windows.values())
            .filter(|scope| scope.live(state) && state.overlays.native_window_ready(scope.window))
            .map(|scope| scope.window)
            .collect()
    }
    fn refresh_hover(&self, state: &mut State, pane: PaneId) {
        if self.grabs.contains_key(&pane) {
            return;
        }
        let Some(position) = self.pointers.get(&pane).copied() else {
            return;
        };
        let sessions = overlay_sessions_for_pane(state, pane);
        let allowed = self.current_windows(state, pane);
        state
            .overlays
            .native_refresh_hover(pane, &sessions, &allowed, position);
        for owner in sessions {
            flush_overlay_lane(state, owner);
        }
    }
    fn process(
        &mut self,
        state: &mut State,
        target: &VectorInputTarget,
        event: &VectorInputEvent,
    ) -> VectorInputResult {
        let result = self.process_event(state, target, event);
        if matches!(result, Delivered { .. })
            && let VectorInputEvent::Pointer { position, .. } = event
        {
            self.pointers.insert(
                target.0.pane,
                Point::new(position[0], position[1]).expect("validated pointer"),
            );
            self.refresh_hover(state, target.0.pane);
        }
        result
    }
    fn process_event(
        &mut self,
        state: &mut State,
        target: &VectorInputTarget,
        event: &VectorInputEvent,
    ) -> VectorInputResult {
        if !self.base(state, target) {
            return Rejected;
        }
        match event {
            VectorInputEvent::Key {
                usage,
                down,
                repeat,
                modifiers,
                text,
            } => {
                let previous = self.keys.get(&(target.0.pane, *usage)).cloned();
                let held = if *down && !*repeat {
                    if previous.is_some() {
                        return Rejected;
                    }
                    let Some(scope) = self.focused(state, target) else {
                        return Rejected;
                    };
                    Held {
                        target: target.clone(),
                        owner: scope.window.context().session(),
                        focus_generation: state
                            .overlays
                            .native_focus_generation(scope.pane)
                            .expect("bound pane"),
                        scope: Some(scope),
                    }
                } else {
                    let Some(held) = previous else {
                        return Rejected;
                    };
                    if !Arc::ptr_eq(&held.target.0, &target.0) {
                        return Rejected;
                    }
                    held
                };
                let readiness = self.scope_ready(state, &held);
                if readiness == Rejected {
                    self.keys.remove(&(target.0.pane, *usage));
                    return Rejected;
                }
                if readiness == Deferred {
                    return Deferred;
                }
                if held.scope.is_none() {
                    if !*down {
                        self.keys.remove(&(target.0.pane, *usage));
                        return if state.overlays.native_swallowed_key(held.owner) {
                            Delivered { text: false }
                        } else {
                            Rejected
                        };
                    }
                    return Rejected;
                }
                let scope = held.scope.as_ref().expect("ordinary held key scope");
                let consumed = state.overlays.native_key(
                    scope.window,
                    Event::Key {
                        physical: *usage,
                        down: *down,
                        repeat: *repeat,
                        modifiers: *modifiers,
                    },
                    *usage == ESCAPE,
                );
                if !consumed {
                    return Rejected;
                }
                let mut delivered_text = false;
                if *down {
                    let mut held = held.clone();
                    if *usage == ESCAPE && state.overlays.native_escape_swallowed(held.owner) {
                        held.scope = None;
                    }
                    self.keys.insert((target.0.pane, *usage), held);
                    if let Some(text) = text
                        && scope.live(state)
                        && state.overlays.native_focus(scope.pane) == Some(scope.window)
                    {
                        delivered_text = state.overlays.native_key(
                            scope.window,
                            Event::Text(text.clone()),
                            false,
                        );
                    }
                } else {
                    self.keys.remove(&(target.0.pane, *usage));
                }
                Delivered {
                    text: delivered_text,
                }
            }
            VectorInputEvent::Text(text) => {
                let Some(scope) = self.focused(state, target) else {
                    return Rejected;
                };
                if state
                    .overlays
                    .native_key(scope.window, Event::Text(text.clone()), false)
                {
                    Delivered { text: true }
                } else {
                    Rejected
                }
            }
            VectorInputEvent::Pointer {
                position,
                button,
                modifiers,
            } => {
                let point = Point::new(position[0], position[1])
                    .expect("validated native pointer position");
                if let Some(grab) = self.grabs.get(&target.0.pane) {
                    let held = grab.held.clone();
                    if !Arc::ptr_eq(&held.target.0, &target.0) {
                        return Rejected;
                    }
                    if let Some((button, down)) = button
                        && grab.buttons.contains(button) == *down
                    {
                        return Rejected;
                    }
                    let region = grab.region;
                    let readiness = self.scope_ready(state, &held);
                    if readiness == Rejected {
                        self.grabs.remove(&target.0.pane);
                        return Rejected;
                    }
                    if readiness == Deferred {
                        return Deferred;
                    }
                    let consumed = if let Some(scope) = &held.scope {
                        if state
                            .overlays
                            .native_capture(target.0.pane)
                            .is_some_and(|captured| captured != scope.window)
                        {
                            self.cancel(state, &held);
                            self.grabs.remove(&target.0.pane);
                            return Rejected;
                        }
                        let sessions = overlay_sessions_for_pane(state, target.0.pane);
                        let allowed = self.current_windows(state, target.0.pane);
                        state.overlays.native_pointer_held(
                            &sessions,
                            &allowed,
                            scope.window,
                            region,
                            PointerReport {
                                position: point,
                                button: *button,
                                modifiers: *modifiers,
                                clicks: 0,
                                pressure: None,
                            },
                        )
                    } else if let Some((button, false)) = button {
                        state.overlays.native_swallowed_release(*button, held.owner)
                    } else {
                        false
                    };
                    if let Some((button, down)) = button
                        && let Some(grab) = self.grabs.get_mut(&target.0.pane)
                    {
                        if *down {
                            grab.buttons.insert(*button);
                        } else {
                            grab.buttons.remove(button);
                        }
                        if grab.buttons.is_empty() {
                            self.grabs.remove(&target.0.pane);
                        }
                    }
                    return if consumed {
                        Delivered { text: false }
                    } else {
                        Rejected
                    };
                }
                if button.is_some_and(|(_, down)| !down) || !self.fresh(state, target) {
                    return Rejected;
                }
                let allowed: Vec<_> = target
                    .0
                    .windows
                    .values()
                    .filter(|scope| {
                        scope.live(state) && state.overlays.native_window_ready(scope.window)
                    })
                    .map(|scope| scope.window)
                    .collect();
                let (consumed, pressed) = state.overlays.native_pointer_start(
                    &target.0.owners,
                    &allowed,
                    point,
                    *button,
                    *modifiers,
                );
                if consumed && let Some((button, true)) = button {
                    let scope = pressed.and_then(|(id, _)| target.0.windows.get(&id).cloned());
                    if let Some(owner) = scope
                        .as_ref()
                        .map(|scope| scope.window.context().session())
                        .or_else(|| {
                            state
                                .overlays
                                .native_swallowed_owner(target.0.pane, *button)
                        })
                    {
                        self.grabs.insert(
                            target.0.pane,
                            Grab {
                                held: Held {
                                    target: target.clone(),
                                    scope,
                                    owner,
                                    focus_generation: state
                                        .overlays
                                        .native_focus_generation(target.0.pane)
                                        .expect("bound pane"),
                                },
                                region: pressed.map_or(0, |(_, region)| region),
                                buttons: BTreeSet::from([*button]),
                            },
                        );
                    }
                }
                if consumed {
                    Delivered { text: false }
                } else {
                    Rejected
                }
            }
            VectorInputEvent::Wheel {
                position,
                delta,
                modifiers,
            } => {
                if self.grabs.contains_key(&target.0.pane) {
                    return Rejected;
                }
                if !self.fresh(state, target) {
                    return Rejected;
                }
                let allowed: Vec<_> = target
                    .0
                    .windows
                    .values()
                    .filter(|scope| {
                        scope.live(state) && state.overlays.native_window_ready(scope.window)
                    })
                    .map(|scope| scope.window)
                    .collect();
                let consumed = state.overlays.native_wheel_scoped(
                    &target.0.owners,
                    &allowed,
                    Point::new(position[0], position[1]).expect("validated wheel position"),
                    Scroll {
                        dx: Scalar::new(delta[0]).expect("validated wheel delta"),
                        dy: Scalar::new(delta[1]).expect("validated wheel delta"),
                        precise: false,
                        phase: ScrollPhase::Changed,
                    },
                    *modifiers,
                );
                if consumed {
                    Delivered { text: false }
                } else {
                    Rejected
                }
            }
        }
    }
    fn enqueue(
        &mut self,
        target: &VectorInputTarget,
        event: PendingEvent,
        focus: Option<u64>,
    ) -> io::Result<()> {
        if let PendingEvent::Physical(VectorInputEvent::Pointer { button: None, .. }) = &event
            && let Some(last) = self.pending.back_mut()
            && Arc::ptr_eq(&last.target.0, &target.0)
            && matches!(
                last.event,
                PendingEvent::Physical(VectorInputEvent::Pointer { button: None, .. })
            )
        {
            last.event = event;
            return Ok(());
        }
        if let Some(last) = self.pending.back_mut()
            && let (PendingEvent::Ime(previous), PendingEvent::Ime(next)) = (&last.event, &event)
            && previous.0.same_focus(&next.0)
            && previous.1.commit.is_none()
            && next.1.commit.is_none()
        {
            last.event = event;
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native deferred input queue exhausted",
            ));
        }
        self.pending.push_back(Pending {
            target: target.clone(),
            event,
            focus,
        });
        Ok(())
    }
}
impl Target {
    fn owner_live(&self, state: &State, owner: SessionIdentity) -> bool {
        let id = owner.session_id();
        state.sessions.get(&id).is_some_and(|session| {
            !session.closed
                && state.overlays.input_enabled(id)
                && session.pane == self.pane
                && self.resumes.contains(&(id, session.resume_generation))
                && session.overlay_lane.as_ref().is_some_and(|lane| {
                    !lane.is_closed()
                        && self
                            .lanes
                            .get(&id)
                            .is_some_and(|old| Weak::ptr_eq(old, &Arc::downgrade(lane)))
                })
        })
    }
}
impl Scope {
    fn live(&self, state: &State) -> bool {
        let key = SurfaceKey {
            session: self.source.producer,
            context: self.source.context,
            surface: self.source.surface,
        };
        let Some(session) = state.sessions.get(&key.session) else {
            return false;
        };
        let Some(surface) = state.surfaces.get(&key) else {
            return false;
        };
        let Some(track) = state.tracks.get(&TrackKey {
            surface: key,
            track: self.source.track,
        }) else {
            return false;
        };
        !session.closed
            && state.overlays.input_enabled(self.source.producer)
            && session.pane == self.pane
            && session.resume_generation == self.resume
            && session.overlay_lane.as_ref().is_some_and(|lane| {
                !lane.is_closed()
                    && self
                        .lane
                        .as_ref()
                        .is_some_and(|old| Weak::ptr_eq(old, &Arc::downgrade(lane)))
            })
            && surface.state.generation.get() == self.generation
            && surface.active_slots.get(&SLOT_VECTOR) == Some(&self.source.track)
            && track.state.channel_generation.get() == self.channel
            && track.channel_writer.as_ref().is_some_and(|writer| {
                !writer.is_closed() && Weak::ptr_eq(&self.writer, &Arc::downgrade(writer))
            })
            && state
                .overlays
                .window(self.window)
                .is_some_and(|window| window.generation == self.generation)
    }
}

impl NativeVectorHost {
    /// Pins the currently focused window before starting asynchronous platform work.
    pub fn text_target(&mut self, target: &VectorInputTarget) -> Option<VectorTextTarget> {
        let mut state = lock(&self.presenter.state);
        overlay_input::maintain(&mut state);
        let scope = self.input.focused(&state, target)?;
        Some(VectorTextTarget(Held {
            target: target.clone(),
            owner: scope.window.context().session(),
            focus_generation: state.overlays.native_focus_generation(scope.pane)?,
            scope: Some(scope),
        }))
    }

    fn text_ready(&self, state: &State, target: &VectorTextTarget) -> VectorInputResult {
        let held = &target.0;
        let Some(scope) = &held.scope else {
            return Rejected;
        };
        if !self.input.base(state, &held.target)
            || !scope.live(state)
            || !state.overlays.native_eligible(scope.window)
            || state.overlays.native_focus(scope.pane) != Some(scope.window)
            || state.overlays.native_focus_generation(scope.pane) != Some(held.focus_generation)
        {
            return Rejected;
        }
        if !state.overlays.native_window_ready(scope.window) {
            return Deferred;
        }
        Delivered { text: false }
    }

    /// Returns locally cached geometry while the original editor focus remains live.
    ///
    /// Geometry may be absent between display and republication by the producer.
    /// That does not revoke a composition's otherwise live focus grant.
    pub fn editor_state(&mut self, target: &VectorTextTarget) -> Option<VectorEditorState> {
        let mut state = lock(&self.presenter.state);
        overlay_input::maintain(&mut state);
        let ready = self.text_ready(&state, target);
        if ready == Rejected {
            return None;
        }
        let scope = target.0.scope.as_ref()?;
        Some(VectorEditorState {
            target: target.clone(),
            caret: (ready != Deferred)
                .then(|| state.overlays.native_editor_caret(scope.pane))
                .flatten(),
        })
    }

    /// Delivers an IME update atomically to its original live window and focus grant.
    ///
    /// A commit clears the previous preedit, inserts bounded text, then installs
    /// the new preedit. Deferred updates enter the bounded ordered input queue.
    ///
    /// # Errors
    /// Rejects oversized text and invalid UTF-8 selection boundaries before mutation.
    pub fn dispatch_ime(
        &mut self,
        target: &VectorTextTarget,
        edit: &VectorImeEdit,
    ) -> io::Result<VectorInputResult> {
        if edit.preedit.len() > MAX_EVENT_TEXT_BYTES
            || edit
                .commit
                .as_ref()
                .is_some_and(|text| text.len() > MAX_VECTOR_TEXT_BYTES)
            || edit.selection.as_ref().is_some_and(|range| {
                range.start > range.end
                    || range.end > edit.preedit.len()
                    || !edit.preedit.is_char_boundary(range.start)
                    || !edit.preedit.is_char_boundary(range.end)
            })
        {
            return Err(invalid("invalid native IME update"));
        }
        let result = {
            let mut state = lock(&self.presenter.state);
            overlay_input::maintain(&mut state);
            if self.text_ready(&state, target) == Rejected {
                Rejected
            } else if self
                .input
                .pending
                .iter()
                .any(|pending| pending.target.0.pane == target.0.target.0.pane)
            {
                Deferred
            } else {
                self.ime_locked(&mut state, target, edit)
            }
        };
        if result == Deferred
            && let Err(error) = self.input.enqueue(
                &target.0.target,
                PendingEvent::Ime(Box::new((target.clone(), edit.clone()))),
                None,
            )
        {
            self.invalidate(target.0.target.0.pane);
            return Err(error);
        }
        Ok(result)
    }

    fn ime_locked(
        &self,
        state: &mut State,
        target: &VectorTextTarget,
        edit: &VectorImeEdit,
    ) -> VectorInputResult {
        let ready = self.text_ready(state, target);
        if !matches!(ready, Delivered { .. }) {
            return ready;
        }
        let id = target
            .0
            .scope
            .as_ref()
            .expect("validated editor scope")
            .window;
        if let Some(text) = &edit.commit {
            state.overlays.native_emit(
                id,
                Event::Ime {
                    preedit: String::new(),
                    selection: None,
                },
            );
            if !emit_text(state, id, text) {
                return Rejected;
            }
        }
        if edit.commit.is_none() || !edit.preedit.is_empty() {
            state.overlays.native_emit(
                id,
                Event::Ime {
                    preedit: edit.preedit.clone(),
                    selection: edit
                        .selection
                        .as_ref()
                        .map(|range| (range.start as u32, range.end as u32)),
                },
            );
        }
        flush_overlay_lane(state, id.context().session().session_id());
        Delivered {
            text: edit.commit.as_ref().is_some_and(|text| !text.is_empty()),
        }
    }

    /// Commits bounded UTF-8 text to its original live window and focus grant.
    ///
    /// Text is split into normative event-sized chunks under one presenter lock.
    /// Repaint may replace the image; a new focus, lane or host epoch rejects the
    /// transfer. `Deferred` emits nothing and lets the host retry after display or
    /// queued physical input completes. This does not enqueue or retain the text.
    ///
    /// # Errors
    /// Refuses text larger than [`MAX_VECTOR_TEXT_BYTES`] before mutation.
    pub fn dispatch_text(
        &mut self,
        target: &VectorTextTarget,
        text: &str,
    ) -> io::Result<VectorInputResult> {
        if text.len() > MAX_VECTOR_TEXT_BYTES {
            return Err(invalid("native text transfer exceeds limit"));
        }
        let mut state = lock(&self.presenter.state);
        overlay_input::maintain(&mut state);
        let ready = self.text_ready(&state, target);
        if !matches!(ready, Delivered { .. }) {
            return Ok(ready);
        }
        if self
            .input
            .pending
            .iter()
            .any(|pending| pending.target.0.pane == target.0.target.0.pane)
        {
            return Ok(Deferred);
        }
        let scope = target.0.scope.as_ref().expect("validated text scope");
        if !emit_text(&mut state, scope.window, text) {
            return Ok(Rejected);
        }
        flush_overlay_lane(&mut state, scope.window.context().session().session_id());
        Ok(Delivered {
            text: !text.is_empty(),
        })
    }

    /// Queues a text transfer in physical input order with a host-supplied deadline.
    ///
    /// Unlike [`Self::dispatch_text`], `Deferred` transfers ownership to the SDK.
    /// Expiry drops only this text operation and never cancels another input grant.
    ///
    /// # Errors
    /// Rejects oversized text and revokes the pane on ordered-queue exhaustion.
    pub fn queue_text(
        &mut self,
        target: &VectorTextTarget,
        text: &str,
        deadline: std::time::Instant,
    ) -> io::Result<VectorInputResult> {
        if std::time::Instant::now() >= deadline {
            return Ok(Rejected);
        }
        let result = self.dispatch_text(target, text)?;
        if result == Deferred
            && let Err(error) = self.input.enqueue(
                &target.0.target,
                PendingEvent::Text(Box::new((target.clone(), text.to_owned(), deadline))),
                None,
            )
        {
            self.invalidate(target.0.target.0.pane);
            return Err(error);
        }
        Ok(result)
    }

    /// Creates an opaque input target for a successfully displayed frame.
    ///
    /// Targets do not retain compiled scenes or consume frame-receipt capacity.
    /// Returning a target grants no eligibility; dispatch validates live identity.
    pub fn input_target(&mut self, frame: &VectorFrame) -> Option<VectorInputTarget> {
        let data = &frame.0;
        let mut state = lock(&self.presenter.state);
        overlay_input::maintain(&mut state);
        for owner in self.input.prune(&mut state) {
            flush_overlay_lane(&mut state, owner);
        }
        if data.presenter != state.presenter
            || !Weak::ptr_eq(&data.host, &Arc::downgrade(&self.alive))
            || state.native_pane_hidden(data.pane)
            || state
                .native_visibility
                .get(&data.pane)
                .copied()
                .unwrap_or(0)
                != data.visibility
            || data
                .input_context
                .iter()
                .any(|(owner, generation)| state.overlays.input_generation(*owner) != *generation)
            || self.presented.get(&data.pane) != Some(&data.serial)
            || !state
                .overlays
                .native
                .as_ref()?
                .compositions
                .contains_key(&data.pane)
        {
            return None;
        }
        if let Some(target) = self.input.current.get(&data.pane).cloned()
            && target.0.serial == data.serial
        {
            self.input.refresh_hover(&mut state, data.pane);
            return Some(target);
        }
        let epoch = *self.input.epochs.entry(data.pane).or_insert(1);
        if epoch == 0 {
            return None;
        }
        let windows = data
            .entries
            .iter()
            .zip(&data.drawings)
            .map(|(entry, drawing)| {
                (
                    entry.window,
                    Scope {
                        window: entry.window,
                        source: entry.source,
                        pane: data.pane,
                        resume: entry.resume,
                        generation: drawing.submission.address.generation,
                        channel: drawing.submission.channel_generation,
                        writer: entry.writer.clone(),
                        lane: state
                            .sessions
                            .get(&entry.source.producer)
                            .and_then(|session| session.overlay_lane.as_ref())
                            .map(Arc::downgrade),
                    },
                )
            })
            .collect();
        let target = VectorInputTarget(Arc::new(Target {
            presenter: data.presenter,
            host: data.host.clone(),
            visibility: data.visibility,
            pane: data.pane,
            serial: data.serial,
            epoch,
            metrics: data.target,
            owners: data.owners.clone(),
            resumes: data.resumes.clone(),
            lanes: data
                .owners
                .iter()
                .filter_map(|id| {
                    state
                        .sessions
                        .get(id)
                        .and_then(|session| session.overlay_lane.as_ref())
                        .map(|lane| (*id, Arc::downgrade(lane)))
                })
                .collect(),
            windows,
            retired: AtomicBool::new(false),
        }));
        self.input.current.insert(data.pane, target.clone());
        self.input.refresh_hover(&mut state, data.pane);
        Some(target)
    }
    /// Retires an image for new input while preserving its original held transitions.
    pub fn retire_input_target(&self, target: &VectorInputTarget) {
        if target.0.presenter == lock(&self.presenter.state).presenter {
            target.0.retired.store(true, Ordering::Release);
        }
    }
    /// Reports whether held or deferred input still references this exact target.
    pub fn retains_input_target(&self, target: &VectorInputTarget) -> bool {
        self.input
            .keys
            .values()
            .any(|held| Arc::ptr_eq(&held.target.0, &target.0))
            || self
                .input
                .grabs
                .values()
                .any(|grab| Arc::ptr_eq(&grab.held.target.0, &target.0))
            || self
                .input
                .pending
                .iter()
                .any(|pending| Arc::ptr_eq(&pending.target.0, &target.0))
    }
    /// Reports whether this presenter currently retains a physical pointer grab.
    pub fn has_pointer_grab(&self, pane: PaneId) -> bool {
        self.input.grabs.contains_key(&pane)
    }

    /// Dispatches input without replacing its original image or held-window binding.
    ///
    /// Call `invalidate` on focus, visibility or target loss. Deferred held input
    /// is retried by `poll` or `flush_input` after matching geometry is displayed.
    ///
    /// # Errors
    /// Rejects invalid geometry, key/button codes, modifier bits and text lengths
    /// before mutation. Queue exhaustion revokes the affected pane's input.
    pub fn dispatch_input(
        &mut self,
        target: &VectorInputTarget,
        event: VectorInputEvent,
    ) -> io::Result<VectorInputResult> {
        validate(&event)?;
        let (result, focus) = {
            let mut state = lock(&self.presenter.state);
            overlay_input::maintain(&mut state);
            let before = state.overlays.native_stamp(&target.0.owners);
            let result = if self.input.base(&state, target)
                && self
                    .input
                    .pending
                    .iter()
                    .any(|pending| pending.target.0.pane == target.0.pane)
            {
                Deferred
            } else {
                self.input.process(&mut state, target, &event)
            };
            if state.overlays.native_stamp(&target.0.owners) != before {
                advance_projection(&mut state);
            }
            for owner in &target.0.owners {
                flush_overlay_lane(&mut state, *owner);
            }
            (
                result,
                matches!(
                    event,
                    VectorInputEvent::Key { .. } | VectorInputEvent::Text(_)
                )
                .then(|| {
                    state
                        .overlays
                        .native_focus_generation(target.0.pane)
                        .unwrap_or(0)
                }),
            )
        };
        if result == Deferred
            && let Err(error) = self
                .input
                .enqueue(target, PendingEvent::Physical(event), focus)
        {
            self.invalidate(target.0.pane);
            return Err(error);
        }
        Ok(result)
    }
    /// Retries bounded held input after actual display admission, preserving transition order.
    pub fn flush_input(&mut self) -> usize {
        let mut delivered = 0;
        {
            let mut state = lock(&self.presenter.state);
            overlay_input::maintain(&mut state);
            for owner in self.input.prune(&mut state) {
                flush_overlay_lane(&mut state, owner);
            }
        }
        let mut blocked = BTreeSet::new();
        for _ in 0..self.input.pending.len() {
            let Some(pending) = self.input.pending.pop_front() else {
                break;
            };
            if blocked.contains(&pending.target.0.pane) {
                self.input.pending.push_back(pending);
                continue;
            }
            let mut state = lock(&self.presenter.state);
            overlay_input::maintain(&mut state);
            let before = state.overlays.native_stamp(&pending.target.0.owners);
            let result = match &pending.event {
                PendingEvent::Physical(event) => {
                    if pending.focus.is_some_and(|generation| {
                        state
                            .overlays
                            .native_focus_generation(pending.target.0.pane)
                            != Some(generation)
                    }) {
                        Rejected
                    } else {
                        self.input.process(&mut state, &pending.target, event)
                    }
                }
                PendingEvent::Ime(value) => self.ime_locked(&mut state, &value.0, &value.1),
                PendingEvent::Text(value) => {
                    let ready = self.text_ready(&state, &value.0);
                    if std::time::Instant::now() >= value.2 {
                        Rejected
                    } else if matches!(ready, Delivered { .. }) {
                        if emit_text(
                            &mut state,
                            value
                                .0
                                .0
                                .scope
                                .as_ref()
                                .expect("validated text scope")
                                .window,
                            &value.1,
                        ) {
                            Delivered {
                                text: !value.1.is_empty(),
                            }
                        } else {
                            Rejected
                        }
                    } else {
                        ready
                    }
                }
            };
            if state.overlays.native_stamp(&pending.target.0.owners) != before {
                advance_projection(&mut state);
            }
            for owner in &pending.target.0.owners {
                flush_overlay_lane(&mut state, *owner);
            }
            drop(state);
            if result == Deferred {
                blocked.insert(pending.target.0.pane);
                self.input.pending.push_back(pending);
            }
            if matches!(result, Delivered { .. }) {
                delivered += 1;
            }
        }
        delivered
    }
}

fn emit_text(state: &mut State, id: SurfaceIdentity, mut text: &str) -> bool {
    while !text.is_empty() {
        let mut end = text.len().min(MAX_EVENT_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if !state
            .overlays
            .native_emit(id, Event::Text(text[..end].to_owned()))
        {
            return false;
        }
        text = &text[end..];
    }
    true
}

fn validate(event: &VectorInputEvent) -> io::Result<()> {
    let mask = match event {
        VectorInputEvent::Pointer {
            position,
            button,
            modifiers,
        } => {
            Point::new(position[0], position[1]).map_err(io::Error::other)?;
            if button.is_some_and(|(button, _)| button > buttons::MAXIMUM) {
                return Err(invalid("invalid native button"));
            }
            *modifiers
        }
        VectorInputEvent::Wheel {
            position,
            delta,
            modifiers,
        } => {
            Point::new(position[0], position[1]).map_err(io::Error::other)?;
            Scalar::new(delta[0]).map_err(io::Error::other)?;
            Scalar::new(delta[1]).map_err(io::Error::other)?;
            *modifiers
        }
        VectorInputEvent::Key {
            usage,
            down,
            repeat,
            modifiers,
            text,
        } => {
            if !(keys::FIRST_USAGE..=keys::LAST_USAGE).contains(usage)
                || (*repeat && !*down)
                || (!*down && text.is_some())
                || text
                    .as_ref()
                    .is_some_and(|text| text.len() > MAX_EVENT_TEXT_BYTES)
            {
                return Err(invalid("invalid native key or text"));
            }
            *modifiers
        }
        VectorInputEvent::Text(text) => {
            if text.len() > MAX_EVENT_TEXT_BYTES {
                return Err(invalid("native committed text exceeds limit"));
            }
            0
        }
    };
    if mask & !modifiers::KNOWN_MASK != 0 {
        return Err(invalid("invalid native modifier mask"));
    }
    Ok(())
}
