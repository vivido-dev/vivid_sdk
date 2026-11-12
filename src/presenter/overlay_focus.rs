//! Original-session admission for producer focus requests through an explicit relay.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::Weak;
use vivid_protocol::overlay::wire::Action;

// The correlated host RPC already has the same ten-second deadline.
const LIFETIME: Duration = Duration::from_secs(10);

pub(crate) struct Grant {
    route: Weak<AtomicBool>,
    request: super::super::OverlayHostRequest,
    writer: Weak<Writer>,
    lane: Weak<Writer>,
    lane_generation: u64,
    resume: u64,
    started: Instant,
}

impl Grant {
    pub(super) fn capture(
        state: &State,
        request: &super::super::OverlayHostRequest,
    ) -> Result<Self, ControlError> {
        let route = state
            .overlay_relay
            .focus_route
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|route| route.load(Ordering::Acquire))
            .ok_or_else(|| ControlError::state("focus relay is unavailable"))?;
        let session = &state.sessions[&request.surface.producer];
        let lane = session
            .overlay_lane
            .as_ref()
            .filter(|lane| !lane.is_closed())
            .ok_or_else(|| ControlError::state("focus input lane is unavailable"))?;
        let lane_generation = state
            .overlays
            .input_generation(request.surface.producer)
            .ok_or_else(|| ControlError::state("focus input lane is expired"))?;
        Ok(Self {
            route: Arc::downgrade(&route),
            request: request.clone(),
            writer: Arc::downgrade(&session.writer),
            lane: Arc::downgrade(lane),
            lane_generation,
            resume: session.resume_generation,
            started: Instant::now(),
        })
    }
}

/// An exclusive, original-presenter route for producer-requested window focus.
///
/// Off by default. Dropping the route cancels pending focus requests; subsequent
/// requests fail closed until another route attaches. This does not activate a
/// native pane or grant input. The physical host decides focus and returns events.
pub struct OverlayFocusRoute {
    state: Weak<Mutex<State>>,
    scope: Arc<AtomicBool>,
}

/// A one-shot focus admission tied to its original route and authenticated request.
///
/// Neither serializable nor retargetable. Claim immediately before dispatching the
/// already-remapped physical request. Admission precedes any concurrent retirement;
/// an admitted RPC may finish later, but must never be retried against a new route.
pub struct OverlayFocusRequest {
    state: Weak<Mutex<State>>,
    scope: Weak<AtomicBool>,
    request: super::super::OverlayHostRequest,
}

impl std::fmt::Debug for OverlayFocusRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayFocusRoute").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for OverlayFocusRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayFocusRequest")
            .finish_non_exhaustive()
    }
}

impl VirtualVivid {
    /// Attaches one exclusive relay for existing producer window-focus actions.
    ///
    /// Enable host relaying first. The caller must remap and service focus RPCs,
    /// and relay physical input/focus events back to this original presenter.
    /// Legacy hosts that do not attach keep their existing local action behavior.
    ///
    /// # Errors
    /// Returns `Unsupported` for local native hosts or disabled relaying, and
    /// `AlreadyExists` when another live focus route is already attached.
    pub fn attach_overlay_focus_relay(&self) -> io::Result<OverlayFocusRoute> {
        let mut state = lock(&self.state);
        if !state.overlay_relay.enabled || state.overlay_relay.local_host_claimed {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "focus requires a terminating relay",
            ));
        }
        if state
            .overlay_relay
            .focus_route
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "focus relay is already attached",
            ));
        }
        let scope = Arc::new(AtomicBool::new(true));
        state.overlay_relay.focus_route = Some(Arc::downgrade(&scope));
        Ok(OverlayFocusRoute {
            state: Arc::downgrade(&self.state),
            scope,
        })
    }
}

impl OverlayFocusRoute {
    /// Pins a request to this route without consuming its one-shot admission.
    pub fn request(&self, request: &super::super::OverlayHostRequest) -> OverlayFocusRequest {
        OverlayFocusRequest {
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
            state.overlay_relay.focus_route = Some(Arc::downgrade(&self.scope));
        }
    }

    /// Returns an idempotent cancellation hook without extending the route lifetime.
    ///
    /// Cancelling refuses future focus admission and releases original pending
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

impl Drop for OverlayFocusRoute {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            retire(&mut lock(&state), &self.scope);
        }
    }
}

fn retire(state: &mut State, scope: &Arc<AtomicBool>) {
    scope.store(false, Ordering::Release);
    if state
        .overlay_relay
        .focus_route
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
        .focus
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
        state.overlay_relay.focus.remove(id);
        state.overlay_relay.pending.remove(id);
        state.overlay_relay.reservations.remove(id);
    }
    state
        .overlay_relay
        .queued
        .retain(|request| !ids.contains(&request.id));
}

impl OverlayFocusRequest {
    /// Consumes admission after checking the original request, owner, writer and input lane.
    ///
    /// Returns false for altered, duplicate, expired, retired or replaced requests.
    /// This authorizes one dispatch, not its outcome or any subsequent input.
    pub fn claim(self) -> bool {
        let (Some(state), Some(scope)) = (self.state.upgrade(), self.scope.upgrade()) else {
            return false;
        };
        let mut state = lock(&state);
        overlay_input::maintain(&mut state);
        if !scope.load(Ordering::Acquire) {
            return false;
        }
        let Some(grant) = state.overlay_relay.focus.get(&self.request.id) else {
            return false;
        };
        let Some(session) = state.sessions.get(&self.request.surface.producer) else {
            return false;
        };
        if grant.request != self.request
            || grant.started.elapsed() >= LIFETIME
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
            || !grant.lane.upgrade().is_some_and(|lane| {
                session
                    .overlay_lane
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(&lane, current) && !lane.is_closed())
            })
            || state
                .overlays
                .input_generation(self.request.surface.producer)
                != Some(grant.lane_generation)
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
                .validate_focus_request(self.request.surface.producer, action)
                .is_err()
        {
            return false;
        }
        state.overlay_relay.focus.remove(&self.request.id);
        true
    }
}
