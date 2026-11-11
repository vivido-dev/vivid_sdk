//! Bounded audio DRAIN replies fenced to their admitted source timeline.

// Rust guideline compliant 2026-10-07
use super::*;

struct Reply {
    request: u64,
    object: u64,
    writer: Arc<Writer>,
}
pub(crate) struct PendingDrain {
    pub(crate) fingerprint: [u8; 32],
    replies: Vec<Reply>,
}
pub(crate) enum DrainAdmission {
    Complete,
    Joined,
    Start(DrainWait),
}
pub(crate) struct DrainWait {
    state: Arc<Mutex<State>>,
    scope: Admission,
    cache: Option<([u8; 16], [u8; 32])>,
    reply: Option<Reply>,
    request: u64,
    object: u64,
    workers: Arc<AtomicUsize>,
    settled: bool,
}
impl DrainWait {
    fn take_replies(&mut self, state: &mut State) -> Vec<Reply> {
        let replies = if let Some((key, _)) = self.cache {
            state
                .audio_drains
                .remove(&(self.scope.key.surface.session, key))
                .map_or_else(Vec::new, |p| p.replies)
        } else {
            self.reply.take().into_iter().collect()
        };
        if let Some(session) = state.sessions.get_mut(&self.scope.key.surface.session) {
            session.pending_waits = session.pending_waits.saturating_sub(replies.len());
        }
        self.settled = true;
        replies
    }
    fn wait(&self) -> Result<(), (u64, &'static str)> {
        let deadline =
            Instant::now() + Duration::from_micros(vivid_protocol::MAX_TRACK_WAIT_TIMEOUT_US);
        let mut state = lock(&self.state);
        let changed = state.wait_changed.clone();
        loop {
            let session = state.sessions.get(&self.scope.key.surface.session);
            if session.is_some_and(|s| {
                s.resume_generation != self.scope.resume || (s.closed && s.lease.is_some())
            }) {
                return Err((
                    messages::ERROR_UNKNOWN_OUTCOME,
                    "audio DRAIN lost its admitted control session",
                ));
            }
            if !self.scope.current(&state) {
                return Err((
                    messages::ERROR_CANCELLED,
                    "audio DRAIN source timeline retired",
                ));
            }
            if state.timed_track_held(self.scope.key) {
                return Err((messages::ERROR_NOT_VISIBLE, "timed surface is held"));
            }
            if state.tracks[&self.scope.key].state.milestones & MILESTONE_BUFFERED_ENDED != 0 {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err((messages::ERROR_TIMEOUT, "audio DRAIN timed out"));
            }
            state.parked_waits += 1;
            state = changed
                .wait_timeout(state, remaining.min(WAIT_RECHECK_INTERVAL))
                .unwrap_or_else(|p| p.into_inner())
                .0;
            state.parked_waits -= 1;
        }
    }
    fn run(mut self) {
        let result = self.wait();
        let (kind, body) = match result {
            Ok(()) => (messages::OK, messages::ok(self.request)),
            Err((code, message)) => (
                messages::ERROR,
                match protocol_error(self.request, code, false, message) {
                    Ok(body) => body,
                    Err(_) => return,
                },
            ),
        };
        let shared = self.state.clone();
        let mut state = lock(&shared);
        let replies = self.take_replies(&mut state);
        let owner = self.scope.key.surface.session;
        // Visibility is transient. A producer may retry this nonce after the
        // pane returns without inheriting a permanently cached failed DRAIN.
        if result
            .as_ref()
            .err()
            .is_none_or(|(code, _)| *code != messages::ERROR_NOT_VISIBLE)
            && let Some((key, fingerprint)) = self.cache
            && state
                .sessions
                .get(&owner)
                .is_some_and(|s| !s.closed || s.lease.is_some())
        {
            let cache_key = (owner, key);
            if !state.idempotency.contains_key(&cache_key) {
                while state.idempotency.len() >= 256 {
                    let Some(oldest) = state.idempotency_order.pop_front() else {
                        break;
                    };
                    state.idempotency.remove(&oldest);
                }
                state.idempotency_order.push_back(cache_key);
            }
            state.idempotency.insert(
                cache_key,
                CachedMutation {
                    fingerprint,
                    record_type: kind,
                    object_id: self.object,
                    body: body.clone(),
                },
            );
        }
        drop(state);
        for reply in replies {
            if let Ok(body) = recorrelate_cached_reply(kind, &body, reply.request) {
                let _ = reply.writer.write_record(kind, reply.object, &body);
            }
        }
    }
    pub(crate) fn spawn(self) -> Result<(), ControlError> {
        thread::Builder::new()
            .name("vivid-audio-drain".into())
            .spawn(move || self.run())
            .map(|_| ())
            .map_err(|_| ControlError::state("could not start audio DRAIN wait"))
    }
}
impl Drop for DrainWait {
    fn drop(&mut self) {
        if !self.settled {
            let shared = self.state.clone();
            self.take_replies(&mut lock(&shared));
        }
        self.workers.fetch_sub(1, Ordering::Release);
        lock(&self.state).wake_waits();
    }
}

pub(crate) fn prepare(
    state: &mut State,
    shared: &Arc<Mutex<State>>,
    key: TrackKey,
    request: u64,
    object: u64,
    cache: Option<([u8; 16], [u8; 32])>,
) -> Result<DrainAdmission, ControlError> {
    let owner = key.surface.session;
    let session = &state.sessions[&owner];
    if session.pending_waits >= MAX_WAITS {
        return Err(ControlError {
            code: messages::ERROR_LIMIT_EXCEEDED,
            message: "audio DRAIN wait capacity exhausted",
        });
    }
    let writer = session.writer.clone();
    if let Some((id, fingerprint)) = cache
        && let Some(pending) = state.audio_drains.get_mut(&(owner, id))
    {
        if pending.fingerprint != fingerprint {
            return Err(ControlError::bad(
                "idempotency key changed during audio DRAIN",
            ));
        }
        pending.replies.push(Reply {
            request,
            object,
            writer,
        });
        state
            .sessions
            .get_mut(&owner)
            .expect("drain owner")
            .pending_waits += 1;
        return Ok(DrainAdmission::Joined);
    }
    let track = state
        .tracks
        .get_mut(&key)
        .ok_or_else(|| ControlError::missing("audio DRAIN track is absent"))?;
    if track.eos_epoch != Some(track.state.media_epoch) {
        return Err(ControlError::state(
            "audio DRAIN requires current channel EOS",
        ));
    }
    if track.state.milestones & MILESTONE_BUFFERED_ENDED != 0 {
        return Ok(DrainAdmission::Complete);
    }
    if track.state.last_media_id == 0 && track.native_audio.is_none() {
        track.state.milestones |= MILESTONE_BUFFERED_ENDED;
        state.wake_waits();
        return Ok(DrainAdmission::Complete);
    }
    let scope = track
        .native_audio
        .as_ref()
        .ok_or_else(|| ControlError::state("audio DRAIN source is unavailable"))?
        .scope
        .clone();
    if !scope.current(state) {
        return Err(ControlError::state("audio DRAIN source is retired"));
    }
    let mut reply = Some(Reply {
        request,
        object,
        writer,
    });
    if let Some((id, fingerprint)) = cache {
        state.audio_drains.insert(
            (owner, id),
            PendingDrain {
                fingerprint,
                replies: reply.take().into_iter().collect(),
            },
        );
    }
    state
        .sessions
        .get_mut(&owner)
        .expect("drain owner")
        .pending_waits += 1;
    let workers = state.audio_workers.clone();
    workers.fetch_add(1, Ordering::AcqRel);
    Ok(DrainAdmission::Start(DrainWait {
        state: shared.clone(),
        scope,
        cache,
        reply,
        request,
        object,
        workers,
        settled: false,
    }))
}
