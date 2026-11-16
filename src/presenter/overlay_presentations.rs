//! Original-incarnation receipts for downstream physical scene outcomes.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::presenter::overlay_host::{MAX_RELAY_FRAMES, MAX_RELAY_OWNER_FRAMES, PresentationClaim};
use std::sync::Weak;
use vivid_protocol::overlay::wire::{PresentationOutcome, Submission};

#[derive(Default)]
struct Credits {
    total: usize,
    owners: HashMap<u64, usize>,
}
struct Credit {
    budget: Arc<Mutex<Credits>>,
    owner: u64,
}
impl Credit {
    fn acquire(budget: &Arc<Mutex<Credits>>, owner: u64) -> Option<Self> {
        let mut state = lock(budget);
        let count = state.owners.get(&owner).copied().unwrap_or(0);
        if state.total >= MAX_RELAY_FRAMES || count >= MAX_RELAY_OWNER_FRAMES {
            return None;
        }
        state.total += 1;
        state.owners.insert(owner, count + 1);
        Some(Self {
            budget: Arc::clone(budget),
            owner,
        })
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        let mut state = lock(&self.budget);
        state.total = state
            .total
            .checked_sub(1)
            .expect("live relay receipt credit");
        let count = state
            .owners
            .get_mut(&self.owner)
            .expect("charged relay owner");
        *count -= 1;
        if *count == 0 {
            state.owners.remove(&self.owner);
        }
    }
}

/// The disposition of an original frame before downstream forwarding.
#[derive(Debug)]
#[must_use]
pub enum OverlayPresentationCapture {
    /// Forward this frame and complete its exact original outcome receipt.
    Forwarded(OverlayPresentationReceipt),
    /// Forward already-presented retained content without another original acknowledgement.
    Replay,
    /// Consume this obsolete or permanently oversized delivery without forwarding it.
    ///
    /// Release its original ingress allowance without reporting a source loss or presentation.
    Discard,
}

/// An exclusive relay that defers source scenes until downstream physical presentation.
///
/// Attach before producers authenticate. It does not render, grant input or authorize
/// a second host. Cancellation leaves presentation unavailable rather than restoring
/// synthetic acknowledgements. Frame receipts retain their original source incarnation.
pub struct OverlayPresentationRoute {
    state: Weak<Mutex<State>>,
    alive: Arc<AtomicBool>,
    credits: Arc<Mutex<Credits>>,
}
impl std::fmt::Debug for OverlayPresentationRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPresentationRoute")
            .finish_non_exhaustive()
    }
}
/// An immutable source-scene identity for retrying bounded capture.
///
/// Created by [`OverlayPresentationRoute::begin_capture`]. It reserves no receipt
/// credit or encoded scene bytes. It cannot rebind to a replacement writer,
/// resume, surface, channel or window scene, even when numeric IDs repeat.
#[must_use = "capture this original attempt or discard the obsolete delivery"]
pub struct OverlayPresentationAttempt {
    state: Weak<Mutex<State>>,
    alive: Weak<AtomicBool>,
    credits: Arc<Mutex<Credits>>,
    writer: Weak<Writer>,
    source: BridgeSourceKey,
    submission: Submission,
    resume: u64,
    origin: Weak<()>,
}
impl std::fmt::Debug for OverlayPresentationAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPresentationAttempt")
            .finish_non_exhaustive()
    }
}
/// A one-shot downstream outcome bound to an original pending source submission.
///
/// Neither cloneable nor retargetable. Completing a stale, replaced, resumed or
/// cancelled receipt cannot publish a replacement scene or grant input authority.
/// Complete or abandon each copy; dropping it alone supplies no physical outcome.
#[must_use = "complete or abandon the physical presentation copy"]
pub struct OverlayPresentationReceipt {
    state: Weak<Mutex<State>>,
    alive: Weak<AtomicBool>,
    writer: Weak<Writer>,
    source: BridgeSourceKey,
    submission: Submission,
    resume: u64,
    origin: Weak<()>,
    _credit: Credit,
}
impl std::fmt::Debug for OverlayPresentationReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayPresentationReceipt")
            .finish_non_exhaustive()
    }
}
impl VirtualVivid {
    /// Counts temporary capture refusals for bounded developer pressure checks.
    ///
    /// This read-only counter saturates at `u64::MAX`; it grants no route or
    /// scene authority and is absent from ordinary builds.
    #[cfg(any(test, feature = "testing"))]
    pub fn overlay_presentation_blocked_captures(&self) -> u64 {
        lock(&self.state).overlays.presentation_blocked_captures()
    }
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
        let byte_limit = state.config.media.aggregate_retained_bytes;
        if !state.sessions.is_empty()
            || !state
                .overlays
                .install_presentation_relay(Arc::downgrade(&alive), byte_limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "presentation route requires an unused presenter",
            ));
        }
        Ok(OverlayPresentationRoute {
            state: Arc::downgrade(&self.state),
            alive,
            credits: Arc::default(),
        })
    }
}
fn current(
    state: &State,
    source: BridgeSourceKey,
    submission: Submission,
    writer: &Weak<Writer>,
    resume: u64,
    origin: &Weak<()>,
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
        && window(source)
            .and_then(|window| {
                state.overlays.presentation_origin(
                    window,
                    source.track,
                    submission.epoch,
                    submission.revision,
                )
            })
            .is_some_and(|(current, life)| current == submission && Weak::ptr_eq(&life, origin))
}
fn window(source: BridgeSourceKey) -> Option<vivid_protocol::identity::SurfaceIdentity> {
    super::super::overlay_host::owner(source.producer)
        .context(source.context)
        .ok()?
        .surface(source.surface)
        .ok()
}
impl OverlayPresentationRoute {
    /// Classifies a frame and reserves its bounded original presentation outcome.
    ///
    /// Complete every `Forwarded` receipt after the physical outcome, or explicitly
    /// abandon it when that physical attachment is retired. Unforwarded frames may
    /// coalesce; accepted successors cannot retire a frame already forwarded.
    /// Four live copies per owner, 64 total copies, and the presenter's configured
    /// aggregate retained-byte budget bound additional scene retention. Temporary
    /// capacity pressure leaves the current frame pending for retry. Obsolete or
    /// permanently oversized frames are `Discard`, without presentation acknowledgement.
    ///
    /// # Errors
    /// Returns `BrokenPipe` after route retirement or original presenter destruction.
    /// Returns `WouldBlock` while other live receipts consume the bounded owner,
    /// global or retained-byte budget. Retry after downstream progress; no outcome
    /// is resolved by that temporary refusal. Use [`Self::begin_capture`] and retain
    /// its attempt when retrying across possible source lifecycle changes.
    pub fn capture(
        &self,
        source: BridgeSourceKey,
        epoch: u32,
        revision: u64,
    ) -> io::Result<OverlayPresentationCapture> {
        match self.begin_capture(source, epoch, revision)? {
            Some(attempt) => attempt.capture(),
            None => Ok(OverlayPresentationCapture::Discard),
        }
    }
    /// Pins an original scene before a potentially delayed capture attempt.
    ///
    /// Returns `None` for an obsolete source or scene. Retain the returned attempt
    /// across `WouldBlock` and retry its `capture` method. Starting another attempt
    /// would classify the then-current source rather than preserve the original.
    /// The attempt holds only weak origin references and reserves no copy credit.
    ///
    /// # Errors
    /// Returns `BrokenPipe` after route retirement or original presenter destruction.
    pub fn begin_capture(
        &self,
        source: BridgeSourceKey,
        epoch: u32,
        revision: u64,
    ) -> io::Result<Option<OverlayPresentationAttempt>> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let state = lock(&state);
        if !self.alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let Some(window) = window(source) else {
            return Ok(None);
        };
        let Some((submission, origin)) =
            state
                .overlays
                .presentation_origin(window, source.track, epoch, revision)
        else {
            return Ok(None);
        };
        let Some(session) = state.sessions.get(&source.producer) else {
            return Ok(None);
        };
        let writer = Arc::downgrade(&session.writer);
        let resume = session.resume_generation;
        if !current(&state, source, submission, &writer, resume, &origin) {
            return Ok(None);
        }
        Ok(Some(OverlayPresentationAttempt {
            state: self.state.clone(),
            alive: Arc::downgrade(&self.alive),
            credits: Arc::clone(&self.credits),
            writer,
            source,
            submission,
            resume,
            origin,
        }))
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
impl OverlayPresentationAttempt {
    /// Compares the complete original scene occurrence without granting capture authority.
    ///
    /// Includes presenter, owner, writer, resume, channel/submission and opaque
    /// scene lifetime. Equal numeric IDs or revisions alone cannot match a
    /// replacement occurrence. Equality does not prove either attempt is still
    /// live; continue using `capture` for original-incarnation admission.
    pub fn same_original(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.state, &other.state)
            && Weak::ptr_eq(&self.writer, &other.writer)
            && Weak::ptr_eq(&self.alive, &other.alive)
            && Weak::ptr_eq(&self.origin, &other.origin)
            && self.source == other.source
            && self.submission == other.submission
            && self.resume == other.resume
    }
    /// Captures this original scene without reclassifying its numeric IDs.
    ///
    /// A replacement or superseded scene returns `Discard` without mutating the
    /// replacement. A successful copy retains the original identity through its
    /// physical outcome. Already-presented content may return `Replay`.
    ///
    /// # Errors
    /// Returns `WouldBlock` for temporary receipt or byte pressure; retry this same
    /// attempt after downstream progress. Returns `BrokenPipe` after route or
    /// presenter retirement. This method performs no waiting or transport I/O.
    pub fn capture(&self) -> io::Result<OverlayPresentationCapture> {
        let (Some(shared), Some(alive)) = (self.state.upgrade(), self.alive.upgrade()) else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let mut state = lock(&shared);
        if !alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if !current(
            &state,
            self.source,
            self.submission,
            &self.writer,
            self.resume,
            &self.origin,
        ) {
            return Ok(OverlayPresentationCapture::Discard);
        }
        let window = window(self.source).expect("validated original window identity");
        let credit = Credit::acquire(&self.credits, self.source.producer);
        let claim = state
            .overlays
            .claim_presentation(window, self.submission, credit.is_some());
        flush_overlay_lane(&mut state, self.source.producer);
        Ok(match claim {
            PresentationClaim::Blocked => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "overlay presentation capacity is full",
                ));
            }
            PresentationClaim::Replay => OverlayPresentationCapture::Replay,
            PresentationClaim::Discard => OverlayPresentationCapture::Discard,
            PresentationClaim::Forwarded => {
                OverlayPresentationCapture::Forwarded(OverlayPresentationReceipt {
                    state: self.state.clone(),
                    alive: self.alive.clone(),
                    writer: self.writer.clone(),
                    source: self.source,
                    submission: self.submission,
                    resume: self.resume,
                    origin: self.origin.clone(),
                    _credit: credit.expect("forwarded frame reserved a credit"),
                })
            }
        })
    }
}
impl OverlayPresentationReceipt {
    /// Retires a physical copy without inventing an original display outcome.
    ///
    /// Use when replacing its downstream attachment. The last abandoned copy
    /// returns the newest original scene to pending so retained replay can acquire
    /// a fresh receipt. Older unshown scenes resolve as superseded. Never call this
    /// under a gateway media-mapping lock: it takes the original presenter lock.
    pub fn abandon(self) -> bool {
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
                &self.origin,
            )
        {
            return false;
        }
        let Some(window) = window(self.source) else {
            return false;
        };
        let abandoned = state.overlays.abandon_presentation(window, self.submission);
        flush_overlay_lane(&mut state, self.source.producer);
        abandoned
    }
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
                &self.origin,
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
    pub(super) fn captured(
        route: &OverlayPresentationRoute,
        source: BridgeSourceKey,
        epoch: u32,
        revision: u64,
    ) -> OverlayPresentationReceipt {
        match route.capture(source, epoch, revision).unwrap() {
            OverlayPresentationCapture::Forwarded(receipt) => receipt,
            other => panic!("expected forwarded capture, got {other:?}"),
        }
    }
    pub(super) fn connect(presenter: &VirtualVivid, pane: u64) -> OverlaySession {
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
    pub(super) fn accepted(window: &OverlayWindow, revision: u64) {
        let end = Instant::now() + Duration::from_secs(3);
        while window.reconcile().unwrap().accepted_revision < revision {
            assert!(Instant::now() < end, "source acceptance stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub(super) fn key(presenter: &VirtualVivid, pane: u64) -> BridgeSourceKey {
        presenter
            .inspect_projection_snapshot_with_viewports(&HashSet::from([pane]), &Default::default())
            .sources[0]
            .key
    }
    #[test]
    fn forwarding_a_scene_preserves_its_outcome_while_a_newer_scene_is_accepted() {
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
        let forwarded = captured(&route, ka, 1, 1);
        let two = a.submit(Canvas::new()).unwrap();
        accepted(&a, 2);
        assert_eq!(
            one.wait(Duration::ZERO).unwrap(),
            None,
            "source acceptance retired a scene already forwarded for physical display"
        );
        assert!(forwarded.complete(PresentationOutcome::Presented));
        assert_eq!(
            one.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(a.reconcile().unwrap().presented_revision, 1);
        assert_eq!(two.wait(Duration::ZERO).unwrap(), None);
        assert_eq!(peer.wait(Duration::ZERO).unwrap(), None);
        assert_eq!(b.reconcile().unwrap().presented_revision, 0);
        assert!(captured(&route, ka, 1, 2).complete(PresentationOutcome::Presented));
        assert_eq!(
            two.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 0);
        a.close().unwrap();
        b.close().unwrap();
        presenter.shutdown();
    }

    #[test]
    fn copies_quota_abandonment_and_replay_preserve_original_outcomes_and_peer_progress() {
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
        let copy = captured(&route, ka, 1, 1);
        let shown = captured(&route, ka, 1, 1);
        assert!(copy.complete(PresentationOutcome::Superseded));
        assert_eq!(
            one.wait(Duration::ZERO).unwrap(),
            None,
            "one discarded copy retired another physical copy"
        );
        assert!(shown.complete(PresentationOutcome::Presented));
        assert_eq!(
            one.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert!(matches!(
            route.capture(ka, 1, 1).unwrap(),
            OverlayPresentationCapture::Replay
        ));
        let mut held = Vec::new();
        let mut submissions = Vec::new();
        for revision in 2..=5 {
            submissions.push(a.submit(Canvas::new()).unwrap());
            accepted(&a, revision);
            held.push(captured(&route, ka, 1, revision));
        }
        let refused = a.submit(Canvas::new()).unwrap();
        accepted(&a, 6);
        assert_eq!(
            route.capture(ka, 1, 6).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            refused.wait(Duration::ZERO).unwrap(),
            None,
            "temporary receipt pressure resolved the current scene"
        );
        assert!(
            matches!(
                route.capture(ka, 1, 1).unwrap(),
                OverlayPresentationCapture::Discard
            ),
            "old replay could overtake a newer forwarded scene"
        );
        assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
        assert_eq!(
            peer.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let newest_held = held.pop().unwrap();
        assert!(newest_held.complete(PresentationOutcome::Presented));
        assert!(captured(&route, ka, 1, 6).complete(PresentationOutcome::Presented));
        assert_eq!(
            refused.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert!(
            matches!(
                route.capture(ka, 1, 2).unwrap(),
                OverlayPresentationCapture::Discard
            ),
            "a new copy of an older scene could replace the displayed scene"
        );
        for receipt in held {
            assert!(receipt.complete(PresentationOutcome::Presented));
        }
        for (index, submission) in submissions.into_iter().enumerate() {
            assert_eq!(
                submission.wait(Duration::from_secs(3)).unwrap(),
                Some(if index == 3 {
                    PresentationOutcome::Presented
                } else {
                    PresentationOutcome::Superseded
                })
            );
        }
        let newest = a.submit(Canvas::new()).unwrap();
        accepted(&a, 7);
        // Repeated downstream replacement cannot consume the bounded receipt budget forever.
        for _ in 0..96 {
            assert!(captured(&route, ka, 1, 7).abandon());
        }
        assert_eq!(newest.wait(Duration::ZERO).unwrap(), None);
        assert!(captured(&route, ka, 1, 7).complete(PresentationOutcome::Presented));
        assert_eq!(
            newest.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 1);
        a.close().unwrap();
        b.close().unwrap();
        presenter.shutdown();
    }

    #[test]
    fn forwarded_scenes_obey_the_byte_budget_without_losing_other_owner_control() {
        use vivid_protocol::vector::{Brush, Color, Path};
        let mut canvas = Canvas::new();
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
                Brush::Solid(Color(0xff33_5577)),
            )
            .unwrap();
        let cost = |canvas: Canvas| {
            u64::try_from(
                vivid_protocol::vector::Frame {
                    epoch: 1,
                    revision: 1,
                    canvas,
                }
                .encode()
                .unwrap()
                .len(),
            )
            .unwrap()
        };
        let bytes = cost(canvas.clone());
        let empty = cost(Canvas::new());
        let media = MediaConfig {
            aggregate_retained_bytes: bytes * 4 + empty * 2,
            ..MediaConfig::default()
        };
        let presenter = VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(media),
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
        let mut held = Vec::new();
        for revision in 1..=4 {
            a.submit(canvas.clone()).unwrap();
            accepted(&a, revision);
            held.push(captured(&route, key(&presenter, 1), 1, revision));
        }
        for revision in 1..=2 {
            b.submit(Canvas::new()).unwrap();
            accepted(&b, revision);
            held.push(captured(&route, key(&presenter, 2), 1, revision));
        }
        let refused = b.submit(Canvas::new()).unwrap();
        accepted(&b, 3);
        assert_eq!(
            route.capture(key(&presenter, 2), 1, 3).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(refused.wait(Duration::ZERO).unwrap(), None);
        b.set_bounds(Rect::new(4., 4., 40., 40.).unwrap()).unwrap();
        assert_eq!(b.bounds().unwrap().origin.x.get(), 4.);
        for receipt in held {
            receipt.complete(PresentationOutcome::Superseded);
        }
        assert!(
            captured(&route, key(&presenter, 2), 1, 3).complete(PresentationOutcome::Presented)
        );
        assert_eq!(
            refused.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let fresh = b.submit(Canvas::new()).unwrap();
        accepted(&b, 4);
        assert!(
            captured(&route, key(&presenter, 2), 1, 4).complete(PresentationOutcome::Presented)
        );
        assert_eq!(
            fresh.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(a.reconcile().unwrap().presented_revision, 0);
        a.close().unwrap();
        b.close().unwrap();
        presenter.shutdown();
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
        let receipt = captured(&route, ka, 1, 1);
        let duplicate = captured(&route, ka, 1, 1);
        assert!(receipt.complete(PresentationOutcome::Presented));
        assert!(!duplicate.complete(PresentationOutcome::Presented));
        assert_eq!(
            one.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 0);
        assert!(captured(&route, kb, 1, 1).complete(PresentationOutcome::Presented));
        assert_eq!(
            peer.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let old = a.submit(Canvas::new()).unwrap();
        accepted(&a, 2);
        let stale = captured(&route, ka, 1, 2);
        let new = a.replace_track(Canvas::new()).unwrap();
        accepted(&a, 3);
        assert!(!stale.complete(PresentationOutcome::Presented));
        assert_eq!(
            old.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Superseded)
        );
        let current = key(&presenter, 1);
        assert_ne!(current.track, ka.track);
        let late = captured(&route, current, 1, 3);
        a.close().unwrap();
        assert!(!late.complete(PresentationOutcome::Presented));
        assert_ne!(
            new.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Presented)
        );
        let next = b.submit(Canvas::new()).unwrap();
        accepted(&b, 2);
        let held = captured(&route, kb, 1, 2);
        route.cancel_handle()();
        assert!(!held.complete(PresentationOutcome::Presented));
        assert_eq!(
            next.wait(Duration::from_secs(3)).unwrap(),
            Some(PresentationOutcome::Superseded)
        );
        assert_eq!(b.reconcile().unwrap().presented_revision, 1);
        assert_eq!(
            route.capture(kb, 1, 2).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
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

#[cfg(test)]
#[path = "overlay_presentation_attempt_tests.rs"]
mod attempt_tests;
