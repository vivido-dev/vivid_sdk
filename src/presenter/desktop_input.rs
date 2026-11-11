//! Authenticated desktop input with explicit host eligibility and bounded admission.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_protocol::{
    input::{
        BindingOutcome, INPUT_CLASS_KNOWN_MASK, InputBinding, InputEvent, InputGate, InputTuple,
        SurfaceExtent,
    },
    surface::DesktopSurfaceParameters,
    time::Monotonic,
};

// One bounded maintenance worker serves all sessions. Ten milliseconds leaves ample
// headroom below half of the protocol's shortest 250-ms watchdog.
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_WATCHDOG_US: u64 = 2_000_000;

/// An owner-qualified desktop surface eligible for physical host input.
///
/// Minted only after an active primary track has been presented. Surface or host
/// target or lease resume generation changes invalidate it. Track replacement alone does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopInputTarget {
    presenter: PresenterInstanceId,
    surface: SurfaceKey,
    surface_generation: SurfaceGeneration,
    target_generation: TargetGeneration,
    logical_size: (u64, u64),
    semantic_generation: u64,
    resume_generation: u64,
}

impl DesktopInputTarget {
    /// Returns the authenticated connection's lease resume generation.
    pub fn resume_generation(self) -> u64 {
        self.resume_generation
    }
    /// Returns the surface's canonical logical dimensions at token creation.
    pub fn logical_size(self) -> (u64, u64) {
        self.logical_size
    }

    /// Returns the producer-owned desktop mapping generation at token creation.
    ///
    /// This is independent of the host target generation. A full-canvas remote
    /// GUI backend can define a policy requiring the producer to acknowledge it.
    pub fn semantic_generation(self) -> u64 {
        self.semantic_generation
    }

    /// Returns the host target generation at token creation.
    pub fn target_generation(self) -> u64 {
        self.target_generation.get()
    }
}

pub(super) struct InputState {
    gate: InputGate,
    request: Option<InputBinding>,
    status: Option<crate::InputBindingStatus>,
    generation: u64,
    origin: Instant,
    rate_window: Instant,
    admitted: u64,
    // USB HID keyboard-page usages end at 0xe7, so indexing remains fixed-size.
    keys: [bool; 232],
    buttons: [bool; 5],
    // Authenticated interactive-lane counter, shared by desktop and overlay input.
    pub(super) lane_generation: u64,
    pub(super) lane_nonce: Option<[u8; 16]>,
    pub(super) limit: u64,
    poisoned: bool,
}

impl Default for InputState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            gate: InputGate::default(),
            request: None,
            status: None,
            generation: 0,
            origin: now,
            rate_window: now,
            admitted: 0,
            keys: [false; 232],
            buttons: [false; 5],
            lane_generation: 0,
            lane_nonce: None,
            limit: 0,
            poisoned: false,
        }
    }
}

impl InputState {
    pub(super) fn disabled(&self) -> bool {
        self.gate.active().is_none_or(|grant| {
            self.now()
                .map_or(true, |now| now >= grant.watchdog_deadline)
        })
    }
    fn now(&self) -> io::Result<Monotonic> {
        u64::try_from(self.origin.elapsed().as_micros())
            .map(Monotonic::from_micros)
            .map_err(|_| invalid("input clock exhausted"))
    }

    fn clear_held(&mut self) {
        self.keys.fill(false);
        self.buttons.fill(false);
    }

    fn bind(
        &mut self,
        binding: InputBinding,
        classes: u64,
    ) -> io::Result<crate::InputBindingStatus> {
        if self.poisoned {
            return Err(invalid("input grant state exhausted"));
        }
        let Some(next) = self.generation.checked_add(1) else {
            self.poisoned = true;
            self.status = None;
            self.clear_held();
            return Err(invalid("input grant generation exhausted"));
        };
        let outcome = self.gate.apply_binding(
            binding.clone(),
            classes,
            binding.requested_watchdog_us.min(DEFAULT_WATCHDOG_US),
            self.now()?,
        )?;
        if outcome == BindingOutcome::ExactRetry {
            return self
                .status
                .ok_or_else(|| invalid("input retry has no result"));
        }
        self.clear_held();
        self.generation = next;
        let active = self.gate.active();
        let status = crate::InputBindingStatus {
            producer_epoch: binding.producer_epoch.get(),
            grant_generation: next,
            context_id: active.map_or(0, |grant| grant.binding.context_id),
            surface_id: active.map_or(0, |grant| grant.binding.surface_id),
            surface_generation: active.map_or(0, |grant| grant.binding.surface_generation.get()),
            effective_classes: active.map_or(0, |grant| grant.effective_classes),
            state: if binding.disabled() {
                0
            } else if active.is_some() {
                1
            } else {
                2
            },
            reason: binding.reason,
            watchdog_timeout_us: active.map_or(0, |grant| grant.watchdog_timeout_us),
        };
        self.status = Some(status);
        self.request = Some(binding);
        Ok(status)
    }
}

fn target_for_surface(state: &State, surface: SurfaceKey) -> Option<DesktopInputTarget> {
    let session = state.sessions.get(&surface.session)?;
    if session.closed || !session.accepted_profiles.contains(registry::DESKTOP_INPUT) {
        return None;
    }
    let entry = state.surfaces.get(&surface)?;
    if entry.state.definition.semantic_profile != registry::DESKTOP_CONTENT {
        return None;
    }
    let track = state.tracks.get(&TrackKey {
        surface,
        track: *entry.active_slots.get(&1)?,
    })?;
    if track.state.milestones & MILESTONE_PRESENTED == 0 {
        return None;
    }
    let parameters =
        DesktopSurfaceParameters::decode(&entry.state.definition.profile_parameters).ok()?;
    Some(DesktopInputTarget {
        presenter: state.presenter,
        surface,
        surface_generation: entry.state.generation,
        target_generation: session.target_generation,
        logical_size: (
            entry.state.definition.logical_width,
            entry.state.definition.logical_height,
        ),
        semantic_generation: parameters.semantic_generation,
        resume_generation: session.resume_generation,
    })
}

fn valid_target(state: &State, target: DesktopInputTarget) -> bool {
    target.presenter == state.presenter
        && state
            .sessions
            .get(&target.surface.session)
            .is_some_and(|session| {
                !session.closed
                    && session.resume_generation == target.resume_generation
                    && session.target_generation == target.target_generation
                    && session.accepted_profiles.contains(registry::DESKTOP_INPUT)
            })
        && state.surfaces.get(&target.surface).is_some_and(|surface| {
            surface.state.generation == target.surface_generation
                && surface.state.definition.semantic_profile == registry::DESKTOP_CONTENT
                && surface.active_slots.get(&1).is_some_and(|track| {
                    state.tracks.contains_key(&TrackKey {
                        surface: target.surface,
                        track: *track,
                    })
                })
        })
}

pub(super) struct LaneGuard {
    state: Arc<Mutex<State>>,
    owner: u64,
    writer: Arc<Writer>,
}

impl LaneGuard {
    pub(super) fn new(state: Arc<Mutex<State>>, owner: u64, writer: Arc<Writer>) -> Self {
        Self {
            state,
            owner,
            writer,
        }
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if state
            .sessions
            .get(&self.owner)
            .and_then(|session| session.overlay_lane.as_ref())
            .is_some_and(|writer| Arc::ptr_eq(writer, &self.writer))
        {
            revoke(&mut state, self.owner, 6, true);
        }
    }
}

impl VirtualVivid {
    /// Observes the live connection generation owning a retained desktop canvas.
    ///
    /// This is supervision metadata, never presentation or input authority.
    /// A suspended or retired owner returns `None` even when a poster remains.
    /// Track replacement within the same canvas does not retire its connection.
    pub fn desktop_session_generation(&self, source: SourceKey) -> Option<u64> {
        let state = lock(&self.state);
        state.surfaces.get(&inner_track_key(source).surface)?;
        let session = state.sessions.get(&source.producer)?;
        (!session.closed).then_some(session.resume_generation)
    }
    /// Sets an explicit input-class policy for one delegated lease.
    ///
    /// Defaults to zero. Profile negotiation, surface capabilities, physical
    /// focus, current presentation and a fresh producer binding remain required.
    /// Policy changes revoke the lease's current grant before taking effect.
    ///
    /// # Errors
    /// Returns an error for unknown classes, absent leases or unsupported input.
    pub fn set_lease_desktop_input_classes(
        &self,
        context: u64,
        lease: u64,
        classes: u64,
    ) -> io::Result<()> {
        if classes & !INPUT_CLASS_KNOWN_MASK != 0 {
            return Err(invalid("unknown leased desktop input classes"));
        }
        let mut state = lock(&self.state);
        let entry = state
            .leases
            .get(&(context, lease))
            .ok_or_else(|| invalid("lease is absent"))?;
        if classes != 0
            && (!entry
                .definition
                .permitted_profiles
                .iter()
                .any(|profile| profile == registry::DESKTOP_INPUT)
                || entry.contract.get(Resource::InputEventsPerSecond) == 0)
        {
            return Err(invalid("lease does not permit desktop input"));
        }
        if entry.desktop_input_classes == classes {
            return Ok(());
        }
        let owner = entry.active_session;
        if let Some(owner) = owner {
            revoke(&mut state, owner, 2, false);
        }
        state
            .leases
            .get_mut(&(context, lease))
            .expect("checked lease")
            .desktop_input_classes = classes;
        Ok(())
    }
    /// Disconnects one presented desktop session without revoking its principal.
    ///
    /// This is unclean transport loss: root producers must authenticate a fresh
    /// session; leased producers follow their existing suspension/resume policy.
    ///
    /// # Errors
    /// Returns an error for a stale or foreign target.
    pub fn disconnect_desktop_session(&self, target: DesktopInputTarget) -> io::Result<()> {
        let mut state = lock(&self.state);
        if !valid_target(&state, target) {
            return Err(invalid("desktop session target is stale"));
        }
        if state.sessions[&target.surface.session].lease.is_none() {
            // Root sessions cannot resume. Retire the exact owner before another
            // root handshake competes for the same bounded pane resources.
            cleanup_session(&mut state, target.surface.session);
            advance_projection(&mut state);
        } else {
            let writer = state.sessions[&target.surface.session].writer.clone();
            revoke(&mut state, target.surface.session, 5, true);
            writer.close();
        }
        Ok(())
    }

    /// Identifies a presented primary desktop surface for host input routing.
    ///
    /// # Errors
    /// Returns `WouldBlock` until the exact active primary source is presented.
    pub fn desktop_input_target(&self, source: SourceKey) -> io::Result<DesktopInputTarget> {
        let state = lock(&self.state);
        let key = inner_track_key(source);
        let target = target_for_surface(&state, key.surface).filter(|_| {
            state
                .surfaces
                .get(&key.surface)
                .and_then(|surface| surface.active_slots.get(&1))
                == Some(&key.track)
        });
        target.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "desktop input source is not presented and active",
            )
        })
    }

    /// Selects physical focus and policy; selection never grants input by itself.
    ///
    /// Changed eligibility revokes old grants before the new target becomes visible.
    /// Pass `None, 0` to revoke focus. Producers must request a greater input epoch.
    ///
    /// # Errors
    /// Returns an error for unknown classes or stale/foreign target identities.
    pub fn set_desktop_input_target(
        &self,
        target: Option<DesktopInputTarget>,
        classes: u64,
    ) -> io::Result<()> {
        let mut state = lock(&self.state);
        if classes & !INPUT_CLASS_KNOWN_MASK != 0 || (target.is_none() && classes != 0) {
            return Err(invalid("invalid desktop input eligibility"));
        }
        if target.is_some_and(|target| !valid_target(&state, target)) {
            return Err(invalid("stale desktop input target"));
        }
        let next = target.map(|target| (target, classes));
        if state.input_focus == next {
            return Ok(());
        }
        let reason = if state.input_focus.map(|entry| entry.0) == target {
            2
        } else {
            1
        };
        let owners: Vec<_> = state.sessions.keys().copied().collect();
        for owner in owners {
            revoke(&mut state, owner, reason, false);
        }
        state.input_focus = next;
        Ok(())
    }

    /// Returns the effective binding for a current host target.
    ///
    /// Stale or unfocused targets return `None`. This snapshot is not authority:
    /// `send_desktop_input` revalidates the complete tuple at admission.
    pub fn desktop_input_binding(&self, target: DesktopInputTarget) -> Option<InputTuple> {
        let mut state = lock(&self.state);
        maintain(&mut state);
        if !valid_target(&state, target) || state.input_focus.map(|entry| entry.0) != Some(target) {
            return None;
        }
        let input = &state.sessions.get(&target.surface.session)?.desktop_input;
        if input.poisoned {
            return None;
        }
        input.gate.active().map(|grant| grant.binding)
    }

    /// Queues one physical input transition under its original complete binding.
    ///
    /// Stale events and synthetic repeated presses are discarded. Overflow revokes
    /// the grant, closes the lane, and requires a new producer epoch and lane.
    /// The producer must still perform its final injection-boundary check.
    ///
    /// # Errors
    /// Returns an error for malformed geometry/events, rate exhaustion, or a lost lane.
    pub fn send_desktop_input(
        &self,
        target: DesktopInputTarget,
        event: InputEvent,
    ) -> io::Result<bool> {
        let mut state = lock(&self.state);
        maintain(&mut state);
        if !valid_target(&state, target) || state.input_focus.map(|entry| entry.0) != Some(target) {
            return Ok(false);
        }
        let definition = &state
            .surfaces
            .get(&target.surface)
            .expect("validated surface")
            .state
            .definition;
        let extent = SurfaceExtent {
            width: definition.logical_width,
            height: definition.logical_height,
        };
        let payload = Value::Map(event.payload());
        match event {
            InputEvent::Key { .. } => InputEvent::decode_key(event.binding().surface_id, &payload)?,
            InputEvent::PointerMotion { .. } => {
                InputEvent::decode_motion(event.binding().surface_id, &payload, extent)?
            }
            InputEvent::PointerButton { .. } => {
                InputEvent::decode_button(event.binding().surface_id, &payload, extent)?
            }
            InputEvent::PointerAxis { .. } => {
                InputEvent::decode_axis(event.binding().surface_id, &payload, extent)?
            }
            _ => return Err(invalid("unknown desktop input event")),
        };
        let runtime = state
            .sessions
            .get_mut(&target.surface.session)
            .expect("validated session");
        let input = &mut runtime.desktop_input;
        if input.poisoned
            || input
                .gate
                .authorize(event, target.surface_generation, input.now()?)
                .is_err()
        {
            return Ok(false);
        }
        let held = match event {
            InputEvent::Key { usage, pressed, .. } => {
                Some((&mut input.keys[usize::from(usage)], pressed))
            }
            InputEvent::PointerButton {
                button, pressed, ..
            } => Some((&mut input.buttons[usize::from(button)], pressed)),
            _ => None,
        };
        if let Some((held, pressed)) = held {
            if *held == pressed {
                return Ok(false);
            }
            *held = pressed;
        }
        if input.rate_window.elapsed() >= Duration::from_secs(1) {
            input.rate_window = Instant::now();
            input.admitted = 0;
        }
        if input.admitted >= input.limit {
            revoke(&mut state, target.surface.session, 9, true);
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "desktop input rate exhausted",
            ));
        }
        input.admitted += 1;
        let body = Envelope::new(0, event.payload()).encode()?;
        let result = runtime
            .overlay_lane
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "desktop input lane is closed")
            })?
            .write_record(event.record_type(), event.binding().surface_id, &body);
        if let Err(error) = result {
            revoke(&mut state, target.surface.session, 9, true);
            return Err(error);
        }
        Ok(true)
    }
}

pub(super) fn bind(
    state: &mut State,
    owner: u64,
    binding: InputBinding,
) -> io::Result<crate::InputBindingStatus> {
    maintain(state);
    let runtime = state
        .sessions
        .get(&owner)
        .ok_or_else(|| invalid("input owner is gone"))?;
    if runtime.desktop_input.request.as_ref() == Some(&binding) {
        return runtime
            .desktop_input
            .status
            .ok_or_else(|| invalid("input retry has no state"));
    }
    let mut classes = 0;
    if !binding.disabled() {
        let key = SurfaceKey {
            session: owner,
            context: binding.context_id,
            surface: binding.surface_id,
        };
        if runtime.root_context != binding.context_id {
            return Err(invalid("desktop surface is not owned by this context"));
        }
        let target = target_for_surface(state, key)
            .filter(|target| target.surface_generation == binding.surface_generation)
            .ok_or_else(|| invalid("desktop input requires a current presented primary surface"))?;
        let parameters = DesktopSurfaceParameters::decode(
            &state.surfaces[&key].state.definition.profile_parameters,
        )?;
        if binding.requested_classes & !parameters.input_capabilities != 0 {
            return Err(invalid("input request exceeds surface capabilities"));
        }
        let policy = runtime.lease.map_or(INPUT_CLASS_KNOWN_MASK, |key| {
            state
                .leases
                .get(&key)
                .map_or(0, |lease| lease.desktop_input_classes)
        });
        if runtime.desktop_input.limit > 0
            && let Some((focused, allowed)) = state.input_focus
            && focused == target
        {
            classes = allowed & policy;
        }
    }
    let runtime = state.sessions.get_mut(&owner).expect("checked owner");
    let result = runtime.desktop_input.bind(binding, classes);
    if runtime.desktop_input.poisoned {
        runtime.writer.close();
        if let Some(lane) = runtime.overlay_lane.take() {
            lane.close();
        }
        if state
            .input_focus
            .is_some_and(|entry| entry.0.surface.session == owner)
        {
            state.input_focus = None;
        }
    }
    result
}

pub(super) fn status_payload(status: crate::InputBindingStatus) -> Vec<(u64, Value)> {
    [
        status.producer_epoch,
        status.grant_generation,
        status.context_id,
        status.surface_id,
        status.surface_generation,
        status.effective_classes,
        status.state,
        status.reason,
        status.watchdog_timeout_us,
    ]
    .into_iter()
    .enumerate()
    .map(|(key, value)| (key as u64, Value::Unsigned(value)))
    .collect()
}

pub(super) fn revoke(state: &mut State, owner: u64, reason: u64, close_lane: bool) {
    if close_lane {
        state.overlays.close_lane(owner);
        flush_overlay_lane(state, owner);
    }
    let Some(runtime) = state.sessions.get_mut(&owner) else {
        return;
    };
    let input = &mut runtime.desktop_input;
    let previous = input.gate.active().cloned();
    match input.gate.revoke() {
        Ok(generation) => input.generation = generation.get(),
        Err(_) => {
            input.poisoned = true;
            runtime.writer.close();
        }
    }
    input.clear_held();
    if let Some(status) = &mut input.status {
        status.grant_generation = input.generation;
        status.context_id = 0;
        status.surface_id = 0;
        status.surface_generation = 0;
        status.effective_classes = 0;
        status.watchdog_timeout_us = 0;
        status.state = u64::from(
            input
                .request
                .as_ref()
                .is_some_and(|request| !request.disabled()),
        ) * 2;
    }
    if let Some(grant) = previous
        && let Some(writer) = &runtime.overlay_lane
    {
        let binding = grant.binding;
        let body = Envelope::new(
            0,
            vec![
                (0, Value::Unsigned(binding.producer_epoch.get())),
                (1, Value::Unsigned(input.generation)),
                (2, Value::Unsigned(binding.context_id)),
                (3, Value::Unsigned(binding.surface_id)),
                (4, Value::Unsigned(binding.surface_generation.get())),
                (5, Value::Unsigned(reason)),
            ],
        )
        .encode();
        if body
            .map_err(io::Error::from)
            .and_then(|body| {
                writer.write_record(messages::INPUT_REVOKED, binding.surface_id, &body)
            })
            .is_err()
        {
            writer.close();
        }
    }
    if close_lane && let Some(lane) = runtime.overlay_lane.take() {
        lane.close();
    }
    if state
        .input_focus
        .is_some_and(|entry| entry.0.surface.session == owner)
        && matches!(reason, 3 | 4 | 5 | 7 | 10)
    {
        state.input_focus = None;
    }
}

pub(super) fn invalidate_surface(state: &mut State, key: SurfaceKey, reason: u64) {
    let affected = state
        .sessions
        .get(&key.session)
        .and_then(|runtime| runtime.desktop_input.gate.active())
        .is_some_and(|grant| {
            grant.binding.context_id == key.context && grant.binding.surface_id == key.surface
        })
        || state
            .input_focus
            .is_some_and(|entry| entry.0.surface == key);
    if affected {
        revoke(state, key.session, reason, false);
    }
}

fn maintain(state: &mut State) {
    super::overlay_input::maintain(state);
    let owners: Vec<_> = state.sessions.keys().copied().collect();
    for owner in owners {
        let Some(grant) = state.sessions[&owner].desktop_input.gate.active().cloned() else {
            continue;
        };
        let focus = state.input_focus;
        if !focus.is_some_and(|(target, _)| {
            target.surface.session == owner && valid_target(state, target)
        }) || state.sessions[&owner].closed
            || state.sessions[&owner].overlay_lane.is_none()
        {
            revoke(state, owner, 3, false);
            continue;
        }
        let runtime = state.sessions.get_mut(&owner).expect("known owner");
        let input = &mut runtime.desktop_input;
        let Ok(now) = input.now() else {
            revoke(state, owner, 8, true);
            continue;
        };
        if now >= grant.watchdog_deadline {
            revoke(state, owner, 8, false);
            continue;
        }
        // Renew after one quarter of the interval. The margin accommodates the
        // polling tick while meeting the protocol's at-least-half-timeout rate.
        if grant.watchdog_deadline.saturating_elapsed_since(now) > grant.watchdog_timeout_us * 3 / 4
        {
            continue;
        }
        let Some(sequence) = grant.renewal_sequence.checked_add(1) else {
            revoke(state, owner, 8, true);
            continue;
        };
        let binding = grant.binding;
        let payload = vec![
            (0, Value::Unsigned(binding.producer_epoch.get())),
            (1, Value::Unsigned(binding.grant_generation.get())),
            (2, Value::Unsigned(binding.context_id)),
            (3, Value::Unsigned(binding.surface_id)),
            (4, Value::Unsigned(binding.surface_generation.get())),
            (5, Value::Unsigned(sequence)),
            (6, Value::Unsigned(grant.watchdog_timeout_us)),
        ];
        let result = Envelope::new(0, payload)
            .encode()
            .map_err(io::Error::from)
            .and_then(|body| {
                runtime
                    .overlay_lane
                    .as_ref()
                    .expect("checked lane")
                    .write_record(messages::INPUT_LEASE_RENEW, binding.surface_id, &body)
            });
        if result.is_err()
            || input
                .gate
                .renew(binding, sequence, grant.watchdog_timeout_us, now)
                .is_err()
        {
            revoke(state, owner, 9, true);
        }
    }
}

pub(super) fn start_maintenance(
    state: Arc<Mutex<State>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("vivid-presenter-input".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                maintain(&mut lock(&state));
                thread::sleep(MAINTENANCE_INTERVAL);
            }
        })?;
    Ok(())
}

pub(super) fn shutdown(state: &mut State) {
    let owners: Vec<_> = state.sessions.keys().copied().collect();
    for owner in owners {
        revoke(state, owner, 10, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::revision::InputEpoch;

    #[test]
    fn grant_generation_exhaustion_is_permanent_and_clears_admission_state() {
        let mut input = InputState::default();
        let request = InputBinding {
            producer_epoch: InputEpoch::new(1),
            context_id: 1,
            surface_id: 1,
            surface_generation: SurfaceGeneration::new(1),
            requested_classes: 1,
            reason: 6,
            requested_watchdog_us: 250_000,
        };
        assert_eq!(input.bind(request.clone(), 1).unwrap().state, 1);
        input.keys[4] = true;
        input.generation = u64::MAX;
        let mut next = request;
        next.producer_epoch = InputEpoch::new(2);
        assert!(input.bind(next.clone(), 1).is_err());
        assert!(input.poisoned);
        assert!(input.status.is_none());
        assert!(input.keys.iter().all(|held| !held));
        assert!(input.bind(next, 1).is_err());
        assert_eq!(input.generation, u64::MAX);
    }
}
