//! Pane visibility policy without ownership of native rendering resources.

// Rust guideline compliant 2026-10-07
use super::{
    PaneId, State, VirtualVivid, flush_overlay_lane, invalid, lock, overlay_sessions_for_pane,
};
use std::{
    io,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

/// Controls one native pane's visibility without waiting for rendering.
///
/// Obtain this from [`super::NativeVectorHost::visibility_control`] after metrics
/// installation. Clones are `Send + Sync`; all changes serialize with presenter
/// state. Weak references fence the owning host lifetime and retain no media,
/// presenter, device or GPU resources after it retires.
#[derive(Clone)]
pub struct NativePaneVisibility {
    pane: PaneId,
    state: Weak<Mutex<State>>,
    alive: Weak<AtomicBool>,
    changed: Weak<Condvar>,
}
impl std::fmt::Debug for NativePaneVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePaneVisibility")
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}
impl NativePaneVisibility {
    pub(super) fn new(
        presenter: &Arc<VirtualVivid>,
        alive: &Arc<AtomicBool>,
        pane: PaneId,
    ) -> Self {
        Self {
            pane,
            state: Arc::downgrade(&presenter.state),
            alive: Arc::downgrade(alive),
            changed: Arc::downgrade(&presenter.delivery_changed),
        }
    }

    /// Applies visibility while preserving producer play intent and retained media.
    ///
    /// Both transitions revoke input and invalidate every earlier display receipt,
    /// even if the host has not returned from render work between hiding and
    /// showing. Hidden timed media pauses; Live media continues. No callback wait,
    /// native device call, font operation or GPU operation runs here. A last
    /// already-admitted audio callback may finish asynchronously.
    ///
    /// Returns `false` for an unchanged value or a retired host. A retired clone
    /// cannot affect another host that reused this pane ID. Keep polling the host
    /// when available to service rendering, text work and clock refinements.
    ///
    /// # Errors
    /// Returns an error if the visibility revision is exhausted. Exhaustion hides
    /// the pane and revokes its authority without wrapping the revision.
    pub fn set_visible(&self, visible: bool) -> io::Result<bool> {
        let (Some(shared), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return Ok(false);
        };
        let mut state = lock(&shared);
        if !alive.load(Ordering::Acquire) {
            return Ok(false);
        }
        let was_visible = !state.native_hidden.contains(&self.pane);
        if was_visible == visible {
            return Ok(false);
        }
        let next = state
            .native_visibility
            .get(&self.pane)
            .and_then(|revision| revision.checked_add(1));
        if visible && next.is_some() {
            state.native_hidden.remove(&self.pane);
        } else {
            state.native_hidden.insert(self.pane);
        }
        if let Some(next) = next {
            state.native_visibility.insert(self.pane, next);
        }
        if let Some(native) = &mut state.overlays.native {
            native.compositions.remove(&self.pane);
        }
        let owners = overlay_sessions_for_pane(&state, self.pane);
        state.overlays.revoke_native_input(&owners);
        for owner in owners {
            flush_overlay_lane(&mut state, owner);
        }
        // Release wire hold state before media ticks consult it; publish the
        // retained physical observation before pausing, then refine after ticks.
        #[cfg(any(feature = "vector-audio", feature = "vector-video"))]
        super::super::native_hold::tick(&mut state);
        #[cfg(feature = "vector-audio")]
        super::super::native_audio::tick(&mut state);
        #[cfg(feature = "vector-video")]
        super::super::native_video::tick(&mut state);
        state.wake_waits();
        drop(state);
        if let Some(changed) = self.changed.upgrade() {
            changed.notify_all();
        }
        if next.is_none() {
            return Err(invalid("native visibility revision exhausted"));
        }
        Ok(true)
    }

    /// Reports whether this pane needs an explicit native media retry.
    ///
    /// This read-only observation covers live owners with a local device-failure
    /// hold. Visibility holds, producer PAUSE and replacement priming alone do
    /// not request a retry. It performs no device, font or GPU work and remains
    /// available while the render worker is busy. Retired controllers return
    /// `false`, even if a new host reuses the pane number.
    ///
    /// The result is advisory: a failure or recovery can occur immediately after
    /// observation. Call [`Self::recover_media`] only following an explicit retry
    /// or a frontend notification that output availability has been restored.
    #[cfg(any(feature = "vector-audio", feature = "vector-video"))]
    pub fn media_recovery_pending(&self) -> bool {
        let (Some(shared), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return false;
        };
        let state = lock(&shared);
        alive.load(Ordering::Acquire)
            && super::super::native_hold::pane_recovery_pending(&state, self.pane)
    }

    /// Signals that this pane can accept replacement native media after failure.
    ///
    /// Call after restoring frontend/device availability. This releases local
    /// device holds without opening a device or restarting retired PLAY operations.
    /// Producers still replace tracks and satisfy synchronized startup readiness.
    /// Retired controllers and panes without a device hold return `false`.
    ///
    #[cfg(any(feature = "vector-audio", feature = "vector-video"))]
    pub fn recover_media(&self) -> bool {
        let (Some(shared), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return false;
        };
        let mut state = lock(&shared);
        if !alive.load(Ordering::Acquire) {
            return false;
        }
        let changed = super::super::native_hold::recover_pane(&mut state, self.pane);
        drop(state);
        if changed && let Some(wake) = self.changed.upgrade() {
            wake.notify_all();
        }
        changed
    }

    /// Stops a clocked test output to exercise ordinary device-failure recovery.
    ///
    /// The owner/context/surface/track/generation tuple must name a current binding in this
    /// pane. Available only with explicit test support and clocked output selected
    /// before authentication. It never stops a hardware device. Missing bindings,
    /// foreign panes and retired controllers return `false`. The source worker
    /// reports the failure through its ordinary scoped recovery path.
    #[cfg(all(feature = "testing", feature = "vector-audio"))]
    pub fn stop_test_audio(
        &self,
        owner: u64,
        context: u64,
        surface: u64,
        track: u64,
        generation: u64,
    ) -> bool {
        let (Some(shared), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return false;
        };
        let state = lock(&shared);
        if !alive.load(Ordering::Acquire)
            || state
                .sessions
                .get(&owner)
                .is_none_or(|session| session.pane != self.pane)
        {
            return false;
        }
        let key = super::super::TrackKey {
            surface: super::super::SurfaceKey {
                session: owner,
                context,
                surface,
            },
            track,
        };
        if state
            .tracks
            .get(&key)
            .is_none_or(|entry| entry.state.channel_generation.get() != generation)
        {
            return false;
        }
        super::super::native_audio::fail_output_for_test(&state, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presenter::{MediaConfig, NativeVectorHost, PresenterConfig, SocketListener};

    #[test]
    fn exhausted_visibility_revision_hides_without_wrapping_or_changing_a_peer() {
        let presenter = Arc::new(
            VirtualVivid::start_configured_eventless(
                SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
                PresenterConfig::terminal_with_overlay(MediaConfig::default()),
            )
            .unwrap(),
        );
        let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
        for pane in [1, 2] {
            presenter.update_metrics(pane, 16, 16, (8, 8));
        }
        let a = host.visibility_control(1).unwrap();
        let b = host.visibility_control(2).unwrap();
        lock(&presenter.state).native_visibility.insert(1, u64::MAX);
        assert!(a.set_visible(false).is_err());
        assert!(a.set_visible(true).is_err());
        assert!(b.set_visible(false).unwrap());
        assert!(b.set_visible(true).unwrap());
        let state = lock(&presenter.state);
        assert_eq!(state.native_visibility[&1], u64::MAX);
        assert!(state.native_hidden.contains(&1));
        assert_eq!(state.native_visibility[&2], 2);
        assert!(!state.native_hidden.contains(&2));
    }
}
