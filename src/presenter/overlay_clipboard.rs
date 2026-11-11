//! One-use clipboard writes serialized with authenticated window input authority.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::Weak;
use vivid_protocol::{identity::SurfaceIdentity, overlay::wire::Clipboard};

#[derive(Clone)]
pub(crate) struct Grant {
    window: SurfaceIdentity,
    generation: u64,
    focus: u64,
    gesture: Instant,
    resume: u64,
    lane: Weak<Writer>,
    text: Arc<str>,
    claimed: bool,
    pub(super) finished: bool,
}
impl Grant {
    pub(super) fn capture(
        state: &State,
        owner: u64,
        request: Clipboard,
    ) -> Result<Self, ControlError> {
        if state
            .overlay_relay
            .clipboard
            .values()
            .any(|grant| grant.window.context().session().session_id() == owner)
        {
            return Err(ControlError::state(
                "an overlay clipboard write is already pending",
            ));
        }
        let window = request
            .address
            .identity(super::super::overlay_host::owner(owner))
            .map_err(|_| ControlError::bad("invalid clipboard window"))?;
        let (focus, gesture) = state
            .overlays
            .clipboard_grant(window)
            .ok_or_else(|| ControlError::state("clipboard gesture is unavailable"))?;
        let session = &state.sessions[&owner];
        let lane = session
            .overlay_lane
            .as_ref()
            .filter(|lane| !lane.is_closed())
            .ok_or_else(|| ControlError::state("clipboard input lane is unavailable"))?;
        Ok(Self {
            window,
            generation: request.address.generation,
            focus,
            gesture,
            resume: session.resume_generation,
            lane: Arc::downgrade(lane),
            text: request.text.into(),
            claimed: false,
            finished: false,
        })
    }
    fn live(&self, state: &State) -> bool {
        self.gesture.elapsed() <= vivid_protocol::overlay::MAX_CLIPBOARD_GESTURE_AGE
            && state
                .overlays
                .clipboard_grant_current(self.window, self.generation, self.focus)
            && state
                .sessions
                .get(&self.window.context().session().session_id())
                .is_some_and(|session| {
                    !session.closed
                        && !session.writer.is_closed()
                        && session.resume_generation == self.resume
                        && session.overlay_lane.as_ref().is_some_and(|lane| {
                            !lane.is_closed() && Weak::ptr_eq(&self.lane, &Arc::downgrade(lane))
                        })
                })
    }
}

/// A one-use, presenter-bound request to write overlay text on the platform thread.
///
/// Obtain it from [`VirtualVivid::overlay_clipboard_write`] after polling a
/// `SET_OVERLAY_CLIPBOARD` host request. Dropping it refuses the correlated request.
/// It exposes no clipboard read capability and redacts its text in diagnostics.
pub struct OverlayClipboardWrite {
    state: Arc<Mutex<State>>,
    id: Option<u64>,
}
impl OverlayClipboardWrite {
    /// Performs the write only while its original input and focus grants remain live.
    ///
    /// Call on the platform thread. The callback must be bounded, must not wait,
    /// and must not reenter the presenter. It runs under the presenter's state
    /// lock, serializing the clipboard operation with focus/lane revocation.
    /// Check the outer application's activation/visibility before calling this.
    ///
    /// # Errors
    /// Refuses retired requests, expired gestures, changed focus, hidden or stale
    /// geometry, replaced input lanes, session loss, or a failed platform write.
    pub fn perform(mut self, write: impl FnOnce(&str) -> io::Result<()>) -> io::Result<()> {
        let id = self.id.take().expect("unconsumed clipboard request");
        let mut state = lock(&self.state);
        overlay_input::maintain(&mut state);
        let grant = state
            .overlay_relay
            .clipboard
            .get(&id)
            .filter(|grant| {
                grant.claimed && !grant.finished && state.overlay_relay.pending.contains_key(&id)
            })
            .cloned();
        let result = match grant.filter(|grant| grant.live(&state)) {
            Some(grant) => write(&grant.text),
            None => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "overlay clipboard write was revoked or expired",
            )),
        };
        let response = match &result {
            Ok(()) => Envelope::new(1, Vec::new())
                .encode()
                .map_err(|_| "clipboard reply encoding failed".into()),
            Err(_) => Err("overlay clipboard write was refused".into()),
        };
        finish(&mut state, id, response);
        result
    }
}
fn finish(state: &mut State, id: u64, response: Result<Vec<u8>, String>) {
    if let Some(grant) = state.overlay_relay.clipboard.get_mut(&id) {
        if grant.finished {
            return;
        }
        grant.finished = true;
        if let Some(sender) = state.overlay_relay.pending.get(&id) {
            let _ = sender.try_send(response);
        }
    }
}
impl Drop for OverlayClipboardWrite {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            finish(
                &mut lock(&self.state),
                id,
                Err("overlay clipboard request was abandoned".into()),
            );
        }
    }
}
impl std::fmt::Debug for OverlayClipboardWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayClipboardWrite")
            .finish_non_exhaustive()
    }
}
impl VirtualVivid {
    /// Claims one pending clipboard request for the bound platform host.
    ///
    /// Pass the ID of a polled `SET_OVERLAY_CLIPBOARD` host request. Claiming does
    /// not grant write permission; [`OverlayClipboardWrite::perform`] rechecks it.
    ///
    /// # Errors
    /// Rejects missing, completed or already claimed clipboard requests.
    pub fn overlay_clipboard_write(&self, id: u64) -> io::Result<OverlayClipboardWrite> {
        let mut state = lock(&self.state);
        let grant = state
            .overlay_relay
            .clipboard
            .get_mut(&id)
            .filter(|grant| !grant.claimed && !grant.finished)
            .ok_or_else(|| invalid("overlay clipboard request is unavailable"))?;
        grant.claimed = true;
        Ok(OverlayClipboardWrite {
            state: self.state.clone(),
            id: Some(id),
        })
    }
}
