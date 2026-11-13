//! Original-incarnation receipts for downstream physical scene outcomes.

// Rust guideline compliant 2026-10-07
use super::*;
use std::sync::Weak;
use vivid_protocol::overlay::wire::{PresentationOutcome, Submission};

/// An exclusive relay that defers source scenes until downstream physical presentation.
///
/// Attach before producers authenticate. It does not render, grant input or authorize
/// a second host. Cancellation leaves presentation unavailable rather than restoring
/// synthetic acknowledgements. Frame receipts retain their original source incarnation.
pub struct OverlayPresentationRoute {
    state: Weak<Mutex<State>>,
    alive: Arc<AtomicBool>,
}
impl std::fmt::Debug for OverlayPresentationRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPresentationRoute")
            .finish_non_exhaustive()
    }
}
/// A one-shot downstream outcome bound to an original pending source submission.
///
/// Neither cloneable nor retargetable. Completing a stale, replaced, resumed or
/// cancelled receipt cannot publish a replacement scene or grant input authority.
pub struct OverlayPresentationReceipt {
    state: Weak<Mutex<State>>,
    alive: Weak<AtomicBool>,
    writer: Weak<Writer>,
    source: BridgeSourceKey,
    submission: Submission,
    resume: u64,
}
impl std::fmt::Debug for OverlayPresentationReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPresentationReceipt")
            .finish_non_exhaustive()
    }
}
impl VirtualVivid {
    /// Defers source presentation to one explicitly bound downstream relay.
    ///
    /// Host relaying must be enabled and no producer may have authenticated yet.
    /// Local native/font hosts and additional presentation routes are refused.
    ///
    /// # Errors
    /// Returns `Unsupported` without host relaying or on a local host, and
    /// `AlreadyExists` after producer authentication or a previous route attachment.
    pub fn attach_overlay_presentation_relay(&self) -> io::Result<OverlayPresentationRoute> {
        let mut state = lock(&self.state);
        if !state.overlay_relay.enabled || state.overlay_relay.local_host_claimed {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presentation forwarding requires a terminating relay",
            ));
        }
        let alive = Arc::new(AtomicBool::new(true));
        if !state.sessions.is_empty()
            || !state
                .overlays
                .install_presentation_relay(Arc::downgrade(&alive))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "presentation route requires an unused presenter",
            ));
        }
        Ok(OverlayPresentationRoute {
            state: Arc::downgrade(&self.state),
            alive,
        })
    }
}
fn current(
    state: &State,
    source: BridgeSourceKey,
    submission: Submission,
    writer: &Weak<Writer>,
    resume: u64,
) -> bool {
    state.sessions.get(&source.producer).is_some_and(|session| {
        !session.closed
            && session.resume_generation == resume
            && writer
                .upgrade()
                .is_some_and(|writer| Arc::ptr_eq(&writer, &session.writer) && !writer.is_closed())
    }) && state
        .surfaces
        .get(&SurfaceKey {
            session: source.producer,
            context: source.context,
            surface: source.surface,
        })
        .is_some_and(|surface| surface.state.generation.get() == submission.address.generation)
        && state
            .tracks
            .get(&TrackKey {
                surface: SurfaceKey {
                    session: source.producer,
                    context: source.context,
                    surface: source.surface,
                },
                track: source.track,
            })
            .is_some_and(|track| {
                track.state.channel_generation.get() == submission.channel_generation
            })
}
fn window(source: BridgeSourceKey) -> Option<vivid_protocol::identity::SurfaceIdentity> {
    super::super::overlay_host::owner(source.producer)
        .context(source.context)
        .ok()?
        .surface(source.surface)
        .ok()
}
impl OverlayPresentationRoute {
    /// Captures the exact pending original source scene before forwarding its frame.
    ///
    /// Returns `None` for retained replay, retired routes, superseded frames or
    /// unavailable source/channel incarnations. Metadata grants no replacement scope.
    pub fn capture(
        &self,
        source: BridgeSourceKey,
        epoch: u32,
        revision: u64,
    ) -> Option<OverlayPresentationReceipt> {
        let state = self.state.upgrade()?;
        let state = lock(&state);
        if !self.alive.load(Ordering::Acquire) {
            return None;
        }
        let submission = state.overlays.presentation_submission(
            window(source)?,
            source.track,
            epoch,
            revision,
        )?;
        let session = state.sessions.get(&source.producer)?;
        let writer = Arc::downgrade(&session.writer);
        let resume = session.resume_generation;
        if !current(&state, source, submission, &writer, resume) {
            return None;
        }
        Some(OverlayPresentationReceipt {
            state: self.state.clone(),
            alive: Arc::downgrade(&self.alive),
            writer,
            source,
            submission,
            resume,
        })
    }
    /// Returns an idempotent cancellation hook for route retirement.
    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let state = self.state.clone();
        let alive = Arc::downgrade(&self.alive);
        Arc::new(move || {
            if let Some(alive) = alive.upgrade() {
                alive.store(false, Ordering::Release);
            }
            if let Some(state) = state.upgrade() {
                let mut state = lock(&state);
                state.overlays.retire_presentations();
                let sessions: Vec<_> = state.sessions.keys().copied().collect();
                state.overlays.retire_relay_focus(&sessions);
                flush_overlay_lanes(&mut state, &sessions);
            }
        })
    }
}
impl Drop for OverlayPresentationRoute {
    fn drop(&mut self) {
        self.cancel_handle()();
    }
}
impl OverlayPresentationReceipt {
    /// Completes once after rechecking original route, writer, resume and channel authority.
    ///
    /// A successful outcome advances only this original pending scene. It does not
    /// prove scanout, refresh input scope or acknowledge another owner's local IDs.
    pub fn complete(self, outcome: PresentationOutcome) -> bool {
        let (Some(state), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return false;
        };
        let mut state = lock(&state);
        if !alive.load(Ordering::Acquire)
            || !current(
                &state,
                self.source,
                self.submission,
                &self.writer,
                self.resume,
            )
        {
            return false;
        }
        let Some(window) = window(self.source) else {
            return false;
        };
        let accepted = state
            .overlays
            .complete_presentation(window, self.submission, outcome);
        flush_overlay_lane(&mut state, self.source.producer);
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presenter::{MediaConfig, PresenterConfig, SocketListener};
    use crate::{
        OverlaySession, OverlayWindow, OverlayWindowOptions, ProducerAuthentication, ProducerConfig,
    };
    use vivid_protocol::vector::{Canvas, Rect};
    fn connect(presenter: &VirtualVivid, pane: u64) -> OverlaySession {
        presenter.update_metrics(pane, 80, 24, (8, 16));
        let secret = presenter.issue_pane_capability(pane).unwrap();
        let endpoint = presenter.endpoint();
        OverlaySession::connect(ProducerConfig {
            authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
            endpoint_control: Some(endpoint.clone()),
            endpoint_interactive: Some(endpoint.clone()),
            endpoint_bulk: Some(endpoint.clone()),
            endpoint_realtime: Some(endpoint),
            ..ProducerConfig::default()
        })
        .unwrap()
    }
    fn accepted(window: &OverlayWindow, revision: u64) {
        let end = Instant::now() + Duration::from_secs(3);
        while window.reconcile().unwrap().accepted_revision < revision {
            assert!(Instant::now() < end, "source acceptance stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn key(presenter: &VirtualVivid, pane: u64) -> BridgeSourceKey {
        presenter
            .inspect_projection_snapshot_with_viewports(&HashSet::from([pane]), &Default::default())
            .sources[0]
            .key
    }
    #[test]
    fn presentation_receipts_fence_supersession_channel_replacement_and_cancellation_between_same_id_owners()
     {
        let presenter = VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap();
        presenter.enable_overlay_host_relay();
        let route = presenter.attach_overlay_presentation_relay().unwrap();
        let first = connect(&presenter, 1);
        let second = connect(&presenter, 2);
        let options = || {
            OverlayWindowOptions::new(
                Rect::new(0., 0., 48., 48.).unwrap(),
                vivid_protocol::overlay::WindowMode::Floating,
            )
        };
        let a = first.create_window(options()).unwrap();
        let b = second.create_window(options()).unwrap();
        let one = a.submit(Canvas::new()).unwrap();
        let peer = b.submit(Canvas::new()).unwrap();
        accepted(&a, 1);
        accepted(&b, 1);
        let ka = key(&presenter, 1);
        let kb = key(&presenter, 2);
        assert_eq!(
            (ka.context, ka.surface, ka.track),
            (kb.context, kb.surface, kb.track)
        );
        assert_ne!(ka.producer, kb.producer);
        assert_eq!(one.wait(Duration::ZERO).unwrap(), None);
        assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
        let receipt = route.capture(ka, 1, 1).unwrap();
        let duplicate = route.capture(ka, 1, 1).unwrap();
        assert!(receipt.complete(PresentationOutcome::Presented));
        assert!(!duplicate.complete(PresentationOutcome::Presented));
        assert_eq!(
            one.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 0);
        assert!(
            route
                .capture(kb, 1, 1)
                .unwrap()
                .complete(PresentationOutcome::Presented)
        );
        assert_eq!(
            peer.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let old = a.submit(Canvas::new()).unwrap();
        accepted(&a, 2);
        let stale = route.capture(ka, 1, 2).unwrap();
        let new = a.replace_track(Canvas::new()).unwrap();
        accepted(&a, 3);
        assert!(!stale.complete(PresentationOutcome::Presented));
        assert_eq!(
            old.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Superseded)
        );
        let current = key(&presenter, 1);
        assert_ne!(current.track, ka.track);
        let late = route.capture(current, 1, 3).unwrap();
        a.close().unwrap();
        assert!(!late.complete(PresentationOutcome::Presented));
        assert_ne!(
            new.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let next = b.submit(Canvas::new()).unwrap();
        accepted(&b, 2);
        let held = route.capture(kb, 1, 2).unwrap();
        route.cancel_handle()();
        assert!(!held.complete(PresentationOutcome::Presented));
        assert_eq!(
            next.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Superseded)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 1);
        assert!(route.capture(kb, 1, 2).is_none());
        assert_eq!(
            presenter
                .attach_overlay_presentation_relay()
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        b.close().unwrap();
        presenter.shutdown();
    }
}
