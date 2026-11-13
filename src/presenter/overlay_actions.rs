//! Original-session admission for producer window actions through an explicit relay.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::Weak;
use vivid_protocol::overlay::wire::{Action, WindowAction, WindowAddress};
use vivid_protocol::vector::Rect;

// The correlated host RPC already has the same ten-second deadline.
const LIFETIME: Duration = Duration::from_secs(10);
// Match the typed document relay: a slow caller cannot retain native input indefinitely.
const INPUT_LIFETIME: Duration = Duration::from_secs(1);

pub(crate) struct Grant {
    route: Weak<AtomicBool>,
    request: super::super::OverlayHostRequest,
    writer: Weak<Writer>,
    lane: Option<(Weak<Writer>, u64)>,
    resume: u64,
    started: Instant,
    action: Action,
    viewport_revision: Option<u64>,
    pub(super) centered_bounds: Option<Rect>,
    pub(super) claimed: bool,
    pub(super) closed_observed: bool,
}

impl Grant {
    pub(super) fn deadline(&self) -> Instant {
        self.started + LIFETIME
    }

    pub(super) fn expired(&self) -> bool {
        self.started.elapsed() >= LIFETIME
    }

    pub(super) fn capture(
        state: &State,
        request: &super::super::OverlayHostRequest,
    ) -> Result<Self, ControlError> {
        let route = state
            .overlay_relay
            .action_route
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|route| route.load(Ordering::Acquire))
            .ok_or_else(|| ControlError::state("window-action relay is unavailable"))?;
        let session = &state.sessions[&request.surface.producer];
        let envelope = messages::decode_control(&request.body)
            .map_err(|_| ControlError::bad("invalid window action"))?;
        let action = Action::decode(request.surface.surface, &Value::Map(envelope.payload))
            .map_err(|_| ControlError::bad("invalid window action"))?;
        let lane = if matches!(
            action.action,
            WindowAction::Focus | WindowAction::Raise | WindowAction::Lower
        ) {
            let lane = session
                .overlay_lane
                .as_ref()
                .filter(|lane| !lane.is_closed())
                .ok_or_else(|| ControlError::state("action input lane is unavailable"))?;
            Some((
                Arc::downgrade(lane),
                state
                    .overlays
                    .input_generation(request.surface.producer)
                    .ok_or_else(|| ControlError::state("action input lane is expired"))?,
            ))
        } else {
            None
        };
        let centered_bounds = if action.action == WindowAction::Center {
            Some(
                state
                    .overlays
                    .centered_action_bounds(request.surface.producer, action)
                    .ok_or_else(|| ControlError::state("center viewport is unavailable"))?,
            )
        } else {
            None
        };
        Ok(Self {
            route: Arc::downgrade(&route),
            request: request.clone(),
            writer: Arc::downgrade(&session.writer),
            lane,
            resume: session.resume_generation,
            started: Instant::now(),
            action,
            viewport_revision: centered_bounds.and_then(|_| {
                state
                    .overlays
                    .action_viewport_revision(request.surface.producer)
            }),
            centered_bounds,
            claimed: false,
            closed_observed: false,
        })
    }

    pub(super) fn observe_close(&mut self, surface: BridgeSurfaceKey, address: WindowAddress) {
        if self.claimed
            && self.action.action == WindowAction::Close
            && self.request.surface == surface
            && self.action.address == address
        {
            self.closed_observed = true;
        }
    }
}

/// Non-authoritative incarnation metadata for one original overlay input source.
///
/// Compare across projection reconciliation. A changed value requires retiring
/// the old physical window and its queued input before binding a replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayInputEpoch {
    presenter: PresenterInstanceId,
    surface: BridgeSurfaceKey,
    resume: u64,
    lane: u64,
}

/// One-shot physical input bound to its original source resume and input lane.
///
/// Forward immediately. Lane replacement, source resume/removal, route retirement
/// or a one-second local delay retires the receipt. It grants no replacement scope.
pub struct OverlayRelayInput {
    state: Weak<Mutex<State>>,
    scope: Weak<AtomicBool>,
    epoch: OverlayInputEpoch,
    input: vivid_protocol::overlay::wire::InputEvent,
    captured: Instant,
}
impl std::fmt::Debug for OverlayRelayInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayRelayInput").finish_non_exhaustive()
    }
}
impl OverlayRelayInput {
    /// Forwards once after atomically rechecking the original input incarnation.
    pub fn forward(self) -> bool {
        let (Some(state), Some(scope)) = (self.state.upgrade(), self.scope.upgrade()) else {
            return false;
        };
        let mut state = lock(&state);
        overlay_input::maintain(&mut state);
        if !scope.load(Ordering::Acquire)
            || self.captured.elapsed() >= INPUT_LIFETIME
            || input_epoch(&state, self.epoch.surface) != Some(self.epoch)
            || !input_surface_current(&state, self.epoch.surface, self.input.address.generation)
        {
            return false;
        }
        super::forward_overlay_input(&mut state, self.epoch.surface, self.input)
    }
}

fn input_surface_current(state: &State, surface: BridgeSurfaceKey, generation: u64) -> bool {
    state
        .surfaces
        .get(&SurfaceKey {
            session: surface.producer,
            context: surface.context,
            surface: surface.surface,
        })
        .is_some_and(|source| source.state.generation.get() == generation)
}

fn input_epoch(state: &State, surface: BridgeSurfaceKey) -> Option<OverlayInputEpoch> {
    let session = state.sessions.get(&surface.producer)?;
    if session.closed
        || session.writer.is_closed()
        || session
            .overlay_lane
            .as_ref()
            .is_none_or(|lane| lane.is_closed())
        || !state.surfaces.contains_key(&SurfaceKey {
            session: surface.producer,
            context: surface.context,
            surface: surface.surface,
        })
    {
        return None;
    }
    Some(OverlayInputEpoch {
        presenter: state.presenter,
        surface,
        resume: session.resume_generation,
        lane: state.overlays.input_generation(surface.producer)?,
    })
}

/// An exclusive, original-presenter route for producer-requested window actions.
///
/// Off by default. Dropping the route cancels pending window actions; subsequent
/// requests fail closed until another route attaches. This does not activate a
/// native pane or grant input. Focus, raise, lower, center and close use the existing
/// window-action contract. The physical host decides focus and returns events.
pub struct OverlayActionRoute {
    state: Weak<Mutex<State>>,
    scope: Arc<AtomicBool>,
}

/// A one-shot window-action admission tied to its original route and authenticated request.
///
/// Neither serializable nor retargetable. Claim immediately before dispatching the
/// already-remapped physical request. Admission precedes any concurrent retirement;
/// an admitted RPC may finish later, but must never be retried against a new route.
pub struct OverlayActionRequest {
    state: Weak<Mutex<State>>,
    scope: Weak<AtomicBool>,
    request: super::super::OverlayHostRequest,
}

impl std::fmt::Debug for OverlayActionRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayActionRoute").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for OverlayActionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayActionRequest")
            .finish_non_exhaustive()
    }
}

impl VirtualVivid {
    /// Attaches one exclusive relay for existing producer window actions.
    ///
    /// Enable host relaying first. The caller must remap and service window-action RPCs,
    /// and forward physical input/focus using original-epoch `OverlayRelayInput`
    /// receipts so already-drained input cannot target a replacement incarnation.
    /// Center captures the original pane viewport; close retires its window group.
    /// Legacy hosts that do not attach keep their existing local action behavior.
    ///
    /// # Errors
    /// Returns `Unsupported` for local native hosts or disabled relaying, and
    /// `AlreadyExists` when another live action route is already attached.
    pub fn attach_overlay_action_relay(&self) -> io::Result<OverlayActionRoute> {
        let mut state = lock(&self.state);
        if !state.overlay_relay.enabled || state.overlay_relay.local_host_claimed {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "window actions require a terminating relay",
            ));
        }
        if state
            .overlay_relay
            .action_route
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "window-action relay is already attached",
            ));
        }
        let scope = Arc::new(AtomicBool::new(true));
        state.overlay_relay.action_route = Some(Arc::downgrade(&scope));
        Ok(OverlayActionRoute {
            state: Arc::downgrade(&self.state),
            scope,
        })
    }
}

impl OverlayActionRoute {
    /// Captures the current original input incarnation without granting input.
    ///
    /// Returns `None` after route/session/lane loss or source removal. Cache the
    /// result with its physical window and compare before forwarding input.
    pub fn input_epoch(&self, surface: BridgeSurfaceKey) -> Option<OverlayInputEpoch> {
        let state = self.state.upgrade()?;
        let mut state = lock(&state);
        overlay_input::maintain(&mut state);
        if !self.scope.load(Ordering::Acquire) {
            return None;
        }
        input_epoch(&state, surface)
    }

    /// Captures one original-incarnation input receipt from physical feedback.
    ///
    /// Returns `None` when the captured physical epoch is no longer current.
    /// The returned receipt is neither serializable nor retargetable.
    ///
    /// # Errors
    /// Returns an error for malformed input or an address outside `epoch`.
    pub fn input(
        &self,
        epoch: OverlayInputEpoch,
        body: &[u8],
    ) -> io::Result<Option<OverlayRelayInput>> {
        let envelope = messages::decode_control(body)?;
        let input = vivid_protocol::overlay::wire::InputEvent::decode(
            epoch.surface.surface,
            &Value::Map(envelope.payload),
        )?;
        if input.address.context_id != epoch.surface.context {
            return Err(invalid("overlay input owner mismatch"));
        }
        let Some(state) = self.state.upgrade() else {
            return Ok(None);
        };
        let mut state = lock(&state);
        overlay_input::maintain(&mut state);
        if !self.scope.load(Ordering::Acquire)
            || input_epoch(&state, epoch.surface) != Some(epoch)
            || !input_surface_current(&state, epoch.surface, input.address.generation)
        {
            return Ok(None);
        }
        Ok(Some(OverlayRelayInput {
            state: self.state.clone(),
            scope: Arc::downgrade(&self.scope),
            epoch,
            input,
            captured: Instant::now(),
        }))
    }

    /// Pins a request to this route without consuming its one-shot admission.
    pub fn request(&self, request: &super::super::OverlayHostRequest) -> OverlayActionRequest {
        OverlayActionRequest {
            state: self.state.clone(),
            scope: Arc::downgrade(&self.scope),
            request: request.clone(),
        }
    }

    /// Cancels pending and drained requests before replacing the physical session.
    ///
    /// New producer requests use a fresh local scope. Old request receipts cannot
    /// claim against it, even when all owner-local IDs are reused.
    pub fn retire_pending(&mut self) {
        if let Some(state) = self.state.upgrade() {
            let mut state = lock(&state);
            retire(&mut state, &self.scope);
            self.scope = Arc::new(AtomicBool::new(true));
            state.overlay_relay.action_route = Some(Arc::downgrade(&self.scope));
        }
    }

    /// Returns an idempotent cancellation hook without extending the route lifetime.
    ///
    /// Cancelling refuses future window-action admission and releases original pending
    /// requests. Drop the route before attaching another one.
    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let state = self.state.clone();
        let scope = Arc::downgrade(&self.scope);
        Arc::new(move || {
            if let (Some(state), Some(scope)) = (state.upgrade(), scope.upgrade()) {
                retire(&mut lock(&state), &scope);
            }
        })
    }
}

impl Drop for OverlayActionRoute {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            retire(&mut lock(&state), &self.scope);
        }
    }
}

pub(super) fn retire_input(state: &mut State, owner: u64) {
    let ids: HashSet<_> = state
        .overlay_relay
        .actions
        .iter()
        .filter_map(|(id, grant)| {
            (grant.request.surface.producer == owner && grant.lane.is_some()).then_some(*id)
        })
        .collect();
    for id in &ids {
        state.overlay_relay.actions.remove(id);
        state.overlay_relay.pending.remove(id);
        state.overlay_relay.reservations.remove(id);
    }
    state
        .overlay_relay
        .queued
        .retain(|request| !ids.contains(&request.id));
}

fn retire(state: &mut State, scope: &Arc<AtomicBool>) {
    scope.store(false, Ordering::Release);
    if state
        .overlay_relay
        .action_route
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|current| Arc::ptr_eq(&current, scope))
    {
        let sessions: Vec<_> = state.sessions.keys().copied().collect();
        state.overlays.retire_relay_focus(&sessions);
        flush_overlay_lanes(state, &sessions);
    }
    let ids: HashSet<_> = state
        .overlay_relay
        .actions
        .iter()
        .filter_map(|(id, grant)| {
            grant
                .route
                .upgrade()
                .filter(|route| Arc::ptr_eq(route, scope))
                .map(|_| *id)
        })
        .collect();
    for id in &ids {
        state.overlay_relay.actions.remove(id);
        state.overlay_relay.pending.remove(id);
        state.overlay_relay.reservations.remove(id);
    }
    state
        .overlay_relay
        .queued
        .retain(|request| !ids.contains(&request.id));
}

impl OverlayActionRequest {
    /// Checks whether this receipt still awaits dispatch before its original deadline.
    ///
    /// This non-consuming queue check grants no authority. Call `claim` immediately
    /// before physical mutation to recheck the complete authenticated scope.
    pub fn is_pending(&self) -> bool {
        let (Some(state), Some(scope)) = (self.state.upgrade(), self.scope.upgrade()) else {
            return false;
        };
        let state = lock(&state);
        scope.load(Ordering::Acquire)
            && state.overlay_relay.pending.contains_key(&self.request.id)
            && state
                .overlay_relay
                .actions
                .get(&self.request.id)
                .is_some_and(|grant| {
                    !grant.claimed
                        && !grant.expired()
                        && grant.request == self.request
                        && grant
                            .route
                            .upgrade()
                            .is_some_and(|route| Arc::ptr_eq(&route, &scope))
                })
    }

    /// Returns the captured original-pane rectangle for a Center request.
    pub fn centered_bounds(&self) -> Option<Rect> {
        let state = self.state.upgrade()?;
        let scope = self.scope.upgrade()?;
        let state = lock(&state);
        let grant = state.overlay_relay.actions.get(&self.request.id)?;
        (scope.load(Ordering::Acquire)
            && grant.request == self.request
            && grant
                .route
                .upgrade()
                .is_some_and(|route| Arc::ptr_eq(&route, &scope)))
        .then_some(grant.centered_bounds)
        .flatten()
    }
    /// Consumes admission after checking the original request and authenticated owner.
    ///
    /// Returns false for altered, duplicate, expired, retired or replaced requests.
    /// Focus, raise and lower also require their original canonical input lane.
    /// Center rechecks the captured viewport revision. Close requires window control
    /// authority and no input grant. This authorizes one dispatch, not its outcome
    /// or any subsequent input.
    pub fn claim(self) -> bool {
        let (Some(state), Some(scope)) = (self.state.upgrade(), self.scope.upgrade()) else {
            return false;
        };
        let mut state = lock(&state);
        overlay_input::maintain(&mut state);
        if !scope.load(Ordering::Acquire) {
            return false;
        }
        let Some(grant) = state.overlay_relay.actions.get(&self.request.id) else {
            return false;
        };
        let Some(session) = state.sessions.get(&self.request.surface.producer) else {
            return false;
        };
        if grant.request != self.request
            || grant.claimed
            || grant.expired()
            || !state.overlay_relay.pending.contains_key(&self.request.id)
            || !grant
                .route
                .upgrade()
                .is_some_and(|route| Arc::ptr_eq(&scope, &route))
            || session.closed
            || session.resume_generation != grant.resume
            || !grant
                .writer
                .upgrade()
                .is_some_and(|writer| Arc::ptr_eq(&writer, &session.writer) && !writer.is_closed())
            || grant.lane.as_ref().is_some_and(|(lane, generation)| {
                !lane.upgrade().is_some_and(|lane| {
                    session
                        .overlay_lane
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(&lane, current) && !lane.is_closed())
                }) || state
                    .overlays
                    .input_generation(self.request.surface.producer)
                    != Some(*generation)
            })
            || grant.viewport_revision.is_some_and(|revision| {
                state
                    .overlays
                    .action_viewport_revision(self.request.surface.producer)
                    != Some(revision)
            })
        {
            return false;
        }
        let Ok(envelope) = messages::decode_control(&self.request.body) else {
            return false;
        };
        let Ok(action) =
            Action::decode(self.request.surface.surface, &Value::Map(envelope.payload))
        else {
            return false;
        };
        if action.address.context_id != self.request.surface.context
            || state
                .surfaces
                .get(&SurfaceKey {
                    session: self.request.surface.producer,
                    context: self.request.surface.context,
                    surface: self.request.surface.surface,
                })
                .is_none_or(|surface| surface.state.generation.get() != action.address.generation)
            || state
                .overlays
                .validate_action_request(self.request.surface.producer, action)
                .is_err()
        {
            return false;
        }
        state
            .overlay_relay
            .actions
            .get_mut(&self.request.id)
            .expect("validated action admission")
            .claimed = true;
        true
    }
}
