//! Explicit leased overlay recovery with complete window adoption.

// Rust guideline compliant 2026-10-07
use super::*;
use std::time::Instant;

// Bound admission of establishment retries while the presenter is unavailable.
const RETRY_BUDGET: Duration = Duration::from_secs(5);

impl OverlaySession {
    /// Returns cancellation bound to this connection, never a later resumed connection.
    ///
    /// This is unclean transport loss; it does not revoke the lease principal.
    /// Custom carriers must first release their own blocking I/O.
    ///
    /// # Errors
    /// Returns an error when this wrapper is already closed.
    pub fn cancel_handle(&self) -> io::Result<Arc<dyn Fn() + Send + Sync>> {
        let guard = lock(&self.session, "overlay session")?;
        Ok(guard.as_ref().ok_or_else(closed)?.cancel_handle())
    }

    /// Resumes a leased owner and adopts all its overlay windows.
    ///
    /// Supply every live window, including children, and the original endpoint
    /// configuration. Authentication comes from this session's resume authority;
    /// root sessions cannot resume. Logical window identities and scene revision
    /// allocation survive. Input and media channels advance; pending receipts,
    /// retained images and text layouts from the old connection become unusable.
    /// Republish full scenes and editor context after recovery. Discard application
    /// preedit, held gestures and queued events from the old connection.
    ///
    /// This operation performs blocking I/O and belongs on the producer worker.
    /// Transport establishment retries retain one authenticated HELLO. Once
    /// retirement starts, any failure leaves this wrapper closed; restart the
    /// application rather than blindly repeating an uncertain recovery mutation.
    ///
    /// # Errors
    /// Rejects root authority, missing, foreign, closed or dismissed windows,
    /// additional surfaces or tracks, invalid configuration, authentication or
    /// transport failure, and any failed handle adoption or channel advance.
    pub fn resume(
        &mut self,
        mut config: ProducerConfig,
        windows: &mut [&mut OverlayWindow],
    ) -> io::Result<()> {
        let mut guard = lock(&self.session, "overlay session")?;
        let old = guard.as_ref().ok_or_else(closed)?;
        if windows.len() != old.surfaces.len()
            || windows.len() != lock(&old.tracks, "track registry")?.len()
        {
            return Err(invalid_input(
                "resume requires the complete overlay window set",
            ));
        }
        let mut addresses = BTreeSet::new();
        for window in windows.iter() {
            if !Weak::ptr_eq(&window.session, &Arc::downgrade(&self.session)) {
                return Err(invalid_input("resume window belongs to another session"));
            }
            let state = lock(&window.state, "overlay window")?;
            if state.closed
                || window.life.is_dismissed()
                || !addresses.insert((state.surface.context_id(), state.surface.id()))
                || !old
                    .surfaces
                    .contains_key(&(state.surface.context_id(), state.surface.id()))
            {
                return Err(invalid_input("resume window set is incomplete or retired"));
            }
        }
        let generation = self
            .input
            .generation()
            .checked_add(1)
            .ok_or_else(|| invalid_data("overlay input generation exhausted"))?;
        let expected_owner = old.info().session_id;
        let expected_resume = old
            .info()
            .resume_generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("overlay resume generation exhausted"))?;
        let allocator = old.next_id.load(Ordering::Acquire);
        let factory = old.connection_factory.clone();
        config.authentication = old.resume_authentication()?;
        config.target_profile = old.info().target_profile.clone();
        config.required_profiles = old.info().accepted_profiles.clone();
        config.optional_profiles.clear();
        let until = Instant::now() + RETRY_BUDGET;
        let mut attempt = EstablishmentAttempt::new(config, RETRY_BUDGET)?;
        (old.cancel_handle())();
        let old = guard.take();
        drop(guard);
        drop(old);
        let _ = self.renewal_stop.send(());
        let _ = self.input.close();
        if let Some(worker) = self.renewal.take() {
            let _ = worker.join();
        }
        let mut next = loop {
            let result = if let Some(factory) = &factory {
                attempt.connect_with_factory(Arc::clone(factory))
            } else {
                attempt.connect()
            };
            match result {
                Ok(session) => break session,
                Err(error) if Instant::now() < until && transient(error.kind()) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        if next.info().session_id != expected_owner
            || next.info().resume_generation != expected_resume
        {
            return Err(invalid_data(
                "overlay resume changed its authenticated owner",
            ));
        }
        next.next_id.fetch_max(allocator, Ordering::AcqRel);
        for window in windows.iter() {
            let mut state = lock(&window.state, "overlay window")?;
            next.adopt_surface(&state.surface)?;
            next.adopt_track(&state.track)?;
            next.advance_channel(&state.track, 1, &RequestMetadata::default())?;
            state.channel = next.open_track_channel(&state.track)?;
            state.active = false;
            state.assets.clear();
            state.layouts.clear();
            refresh(&next, &mut state)?;
        }
        let replacement = Self::from_session_generation(next, generation)?;
        for window in windows.iter_mut() {
            window.session = Arc::downgrade(&replacement.session);
            window.input = Arc::downgrade(&replacement.input);
        }
        *self = replacement;
        Ok(())
    }
}

fn transient(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}
