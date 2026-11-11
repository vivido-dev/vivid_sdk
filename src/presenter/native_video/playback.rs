//! Bounded pre-roll with software or admitted audio-master presentation clocks.

// Rust guideline compliant 2026-10-07
use super::*;
use std::collections::BTreeMap;

// Bound metadata and iteration independently of a producer's rate/latency claims.
pub(super) const MAX_QUEUED: usize = 256;

pub(crate) struct Playback {
    pub serial: u64,
    pub capacity: usize,
    pub queue: BTreeMap<(i64, u64), Frame>,
    start: i64,
    origin: i64,
    minimum: u64,
    running: Option<Instant>,
    frozen: i64,
    requested: bool,
    held: bool,
    started: bool,
    synchronized: bool,
    ever_played: bool,
    stopped: bool,
    drain_ceiling: Option<i64>,
    pub last_end: Option<i64>,
    pub decoded: Option<i64>,
    pub presented: i64,
    pub presentation_id: u64,
    pub late: u64,
    pub reported: Option<(u64, u64)>,
    #[cfg(feature = "vector-audio")]
    master: Option<native_audio::Clock>,
}
impl Playback {
    pub fn new(capacity: usize) -> Self {
        Self {
            serial: 0,
            capacity,
            queue: BTreeMap::new(),
            start: 0,
            origin: 0,
            minimum: 0,
            running: None,
            frozen: 0,
            requested: false,
            held: false,
            started: false,
            synchronized: false,
            ever_played: false,
            stopped: false,
            drain_ceiling: None,
            last_end: None,
            decoded: None,
            presented: 0,
            presentation_id: 0,
            late: 0,
            reported: None,
            #[cfg(feature = "vector-audio")]
            master: None,
        }
    }
    pub fn clock(&self, now: Instant) -> i64 {
        #[cfg(feature = "vector-audio")]
        if let Some(master) = self.master {
            return master.pts;
        }
        let pts = self.running.map_or(self.frozen, |origin| {
            self.origin.saturating_add(
                i64::try_from(now.saturating_duration_since(origin).as_micros())
                    .unwrap_or(i64::MAX),
            )
        });
        self.drain_ceiling.map_or(pts, |end| pts.min(end))
    }
    pub fn play(&mut self, request: PlayRequest) -> io::Result<()> {
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| invalid("video playback serial exhausted"))?;
        self.start = request.start_pts_us;
        self.origin = self.start;
        self.frozen = self.start;
        self.minimum = request.minimum_buffer_us;
        self.synchronized = request.start_policy == 2;
        self.running = None;
        self.requested = true;
        self.started = false;
        self.ever_played = true;
        self.stopped = false;
        self.drain_ceiling = None;
        self.presentation_id = 0;
        self.presented = 0;
        let before = self.queue.len();
        self.queue.retain(|(pts, _), _| *pts >= self.start);
        self.late = self.late.saturating_add((before - self.queue.len()) as u64);
        self.last_end = self.queue.values().map(|f| f.end).max();
        Ok(())
    }
    pub fn pause(&mut self, now: Instant) {
        self.frozen = self.clock(now);
        self.running = None;
        self.requested = false;
        self.ever_played = true;
    }
    fn set_held(&mut self, held: bool, now: Instant) {
        if self.held == held {
            return;
        }
        self.frozen = self.clock(now);
        self.origin = self.frozen;
        self.running = None;
        self.held = held;
    }
    pub fn push(&mut self, frame: Frame) {
        self.decoded = Some(frame.pts);
        if self.ever_played && frame.pts < self.start {
            self.late = self.late.saturating_add(1);
            return;
        }
        self.last_end = Some(self.last_end.map_or(frame.end, |old| old.max(frame.end)));
        self.queue.insert((frame.pts, frame.id), frame);
    }
    pub fn start_if_ready(&mut self, now: Instant, drained: bool, poster: Option<i64>) -> bool {
        if drained && self.drain_ceiling.is_none() {
            // Stop at the known end while final display proof is pending.
            // Late EOS must not rewind a clock already observed beyond it.
            self.drain_ceiling = Some(
                self.last_end
                    .unwrap_or(self.start)
                    .max(self.start)
                    .max(self.clock(now)),
            );
        }
        #[cfg(not(feature = "vector-audio"))]
        let clock_ready = true;
        #[cfg(feature = "vector-audio")]
        let clock_ready = self
            .master
            .is_none_or(|clock| clock.started && clock.playing);
        if self.requested
            && !self.held
            && self.running.is_none()
            && !self.stopped
            && clock_ready
            && (self.started || self.ready(drained, self.eligible(poster), self.has_master()))
        {
            self.running = Some(now);
            self.started = true;
            return true;
        }
        false
    }
    fn has_master(&self) -> bool {
        #[cfg(feature = "vector-audio")]
        {
            self.master.is_some()
        }
        #[cfg(not(feature = "vector-audio"))]
        {
            false
        }
    }
    fn eligible(&self, poster: Option<i64>) -> bool {
        poster.is_some_and(|pts| pts >= self.start)
            || self.queue.keys().any(|(pts, _)| *pts >= self.start)
    }
    fn ready(&self, drained: bool, eligible: bool, audio_master: bool) -> bool {
        if self.synchronized && !drained {
            if !eligible {
                return false;
            }
            if audio_master {
                return true;
            }
        }
        drained
            || self.minimum == 0
            || self
                .last_end
                .is_some_and(|end| end.saturating_sub(self.start).max(0) as u64 >= self.minimum)
    }
    #[cfg(feature = "vector-audio")]
    fn update_master(&mut self, clock: Option<native_audio::Clock>, now: Instant) {
        if let Some(clock) = clock.filter(|c| !c.ended) {
            self.started |= clock.started;
            self.master = Some(clock);
            return;
        }
        if let Some(old) = self.master.take() {
            // Loss or a shorter audio stream relinquishes clock authority.
            // Continue video from its last admitted audio position, never from
            // the original PLAY or the first queued picture.
            self.frozen = clock.map_or(old.pts, |c| c.pts).max(old.pts);
            self.origin = self.frozen;
            // A short stream may start and drain entirely between video ticks.
            // Its final callback observation is newer authority than the cached
            // prebuffer snapshot, including while this pane is hidden.
            let started = old.started || clock.is_some_and(|clock| clock.started);
            self.started |= started;
            self.running = (self.requested && !self.held && started).then_some(now);
        }
    }
    pub fn take_due(&mut self, now: Instant) -> Option<Frame> {
        self.running?;
        let clock = self.clock(now);
        let mut latest = None;
        while self
            .queue
            .first_key_value()
            .is_some_and(|((pts, _), _)| *pts <= clock)
        {
            let (_, mut frame) = self.queue.pop_first().expect("due video frame");
            frame.play = self.serial;
            if latest.replace(frame).is_some() {
                self.late = self.late.saturating_add(1);
            }
        }
        latest
    }
    pub fn pre_roll_full(&self) -> bool {
        self.requested
            && !self.held
            && self.running.is_none()
            && !self.ready(false, self.eligible(None), self.has_master())
            && self.queue.len() >= self.capacity
    }
    pub fn state(&self) -> u64 {
        if self.stopped {
            4
        } else if self.requested {
            if self.held && self.started {
                #[cfg(feature = "vector-audio")]
                if self.master.is_some_and(|c| !c.playing && !c.paused) {
                    return 1;
                }
                return 3;
            }
            if self.running.is_some() { 2 } else { 1 }
        } else if self.ever_played {
            #[cfg(feature = "vector-audio")]
            if self.master.is_some_and(|c| !c.playing && !c.paused) {
                // Group PAUSE waits for the audio callback's final commit.
                return 1;
            }
            3
        } else {
            0
        }
    }
    pub fn finish(&mut self, now: Instant) {
        self.frozen = self.clock(now);
        self.running = None;
        self.requested = false;
        self.stopped = true;
    }
    pub fn ahead(&self, now: Instant) -> u64 {
        self.last_end
            .map_or(0, |end| end.saturating_sub(self.clock(now)).max(0) as u64)
    }
    pub fn start_pts(&self) -> i64 {
        self.start
    }
    pub fn ever_played(&self) -> bool {
        self.ever_played
    }
    pub fn prime_allowed(&self) -> bool {
        self.synchronized && !self.requested && !self.started && !self.stopped
    }
}

pub(crate) fn validate_play(track: &TrackEntry, request: PlayRequest) -> io::Result<()> {
    if track.state.lost {
        return Err(invalid("PLAY target is lost"));
    }
    if track.video_reservation.is_some() && track.video_playback.is_none() {
        return Err(invalid("PLAY requires a timed video track"));
    }
    if track.video_playback.is_some()
        && (!matches!(request.start_policy, 1 | 2)
            || (request.hold_serial.is_some() && request.start_policy != 2)
            || request.maximum_latency_us > track.configuration.maximum_latency_us
            || request.start_pts_us == i64::MIN
            || track.native_frame_serial == u64::MAX
            || track
                .video_playback
                .as_ref()
                .is_some_and(|p| p.serial == u64::MAX))
    {
        return Err(invalid(
            "native timed video PLAY exceeds its admitted policy",
        ));
    }
    Ok(())
}

pub(crate) fn validate_group_play(
    state: &State,
    key: TrackKey,
    request: PlayRequest,
) -> io::Result<()> {
    let target = key;
    let mut keys = state
        .surfaces
        .get(&key.surface)
        .map(|s| {
            s.active_slots
                .values()
                .map(|id| TrackKey {
                    surface: key.surface,
                    track: *id,
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    keys.insert(key);
    for key in keys {
        if request.start_policy == 2
            && state
                .tracks
                .get(&key)
                .is_some_and(|track| track.state.lost && track.video_playback.is_some())
        {
            return Err(invalid(
                "synchronized PLAY requires every active video member to be healthy",
            ));
        }
        if let Some(track) = state
            .tracks
            .get(&key)
            .filter(|t| !t.state.lost || key == target)
        {
            validate_play(track, request)?;
        }
    }
    Ok(())
}

#[cfg(feature = "vector-audio")]
pub(crate) fn group_ready(state: &State, surface: SurfaceKey) -> bool {
    let Some(entry) = state.surfaces.get(&surface) else {
        return false;
    };
    let resume = state
        .sessions
        .get(&surface.session)
        .filter(|s| !s.closed)
        .map(|s| s.resume_generation);
    entry.active_slots.values().all(|id| {
        let Some(track) = state.tracks.get(&TrackKey {
            surface,
            track: *id,
        }) else {
            return false;
        };
        let Some(p) = &track.video_playback else {
            return true;
        };
        if track.state.lost {
            return track.play_request.start_policy != 2;
        }
        if !p.synchronized {
            return p.ready(
                track.video_drained == Some(track.state.media_epoch),
                true,
                true,
            );
        }
        let Some(scope) = FrameScope::capture(track, Some(entry.state.generation), resume) else {
            return false;
        };
        if !scope.admitted() {
            return false;
        }
        let eligible = p
            .queue
            .values()
            .chain(track.native_video.iter())
            .any(|frame| frame.pts >= p.start && scope.accepts(frame));
        p.ready(
            track.video_drained == Some(track.state.media_epoch),
            eligible,
            true,
        )
    })
}

pub(crate) fn recovery_ready(state: &State, key: TrackKey) -> bool {
    let Some(track) = state.tracks.get(&key) else {
        return false;
    };
    let Some(p) = &track.video_playback else {
        return track.video_reservation.is_none();
    };
    let Some(scope) = FrameScope::capture(
        track,
        state.surfaces.get(&key.surface).map(|s| s.state.generation),
        state
            .sessions
            .get(&key.surface.session)
            .filter(|s| !s.closed)
            .map(|s| s.resume_generation),
    ) else {
        return false;
    };
    let eligible = p
        .queue
        .values()
        .chain(track.native_video.iter())
        .any(|frame| frame.pts >= p.start && scope.accepts(frame));
    scope.admitted()
        && p.ready(
            track.video_drained == Some(track.state.media_epoch),
            eligible,
            p.has_master(),
        )
}

pub(crate) fn control(
    state: &mut State,
    key: TrackKey,
    kind: u16,
    request: Option<PlayRequest>,
) -> io::Result<()> {
    let mut keys = state
        .surfaces
        .get(&key.surface)
        .map(|s| {
            s.active_slots
                .values()
                .map(|id| TrackKey {
                    surface: key.surface,
                    track: *id,
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    keys.insert(key);
    for member in keys {
        let Some(track) = state.tracks.get_mut(&member) else {
            continue;
        };
        if track.state.lost && member != key {
            continue;
        }
        let Some(p) = track.video_playback.as_mut() else {
            continue;
        };
        match kind {
            messages::PLAY => {
                let request = request.expect("validated PLAY");
                let resume = p.state() == 3 && p.clock(Instant::now()) == request.start_pts_us;
                let poster = resume.then(|| track.native_video.clone()).flatten();
                p.play(request)?;
                track.native_video = if let Some(mut frame) = poster {
                    track.native_frame_serial = track
                        .native_frame_serial
                        .checked_add(1)
                        .ok_or_else(|| invalid("native video frame IDs exhausted"))?;
                    frame.id = track.native_frame_serial;
                    frame.play = p.serial;
                    p.last_end = Some(p.last_end.unwrap_or(request.start_pts_us).max(frame.end));
                    Some(frame)
                } else {
                    None
                };
                track.native_visual_presented = None;
                track.state.milestones &=
                    !(MILESTONE_CLOCK_STARTED | MILESTONE_PRESENTED | MILESTONE_BUFFERED_ENDED);
            }
            messages::PAUSE => p.pause(Instant::now()),
            messages::FLUSH => {
                if member == key {
                    *p = Playback::new(p.capacity);
                } else {
                    p.pause(Instant::now());
                }
                track.playing = false;
            }
            _ => (),
        }
    }
    tick(state);
    Ok(())
}

pub(crate) fn position(track: &TrackEntry, now: Instant) -> Option<BridgePositionSnapshot> {
    let p = track.video_playback.as_ref()?;
    Some(BridgePositionSnapshot {
        decoder_reset_serial: track.decoder_reset_serial,
        playing: track.playing,
        start_pts_us: p.start_pts(),
        state: if track.state.lost { 5 } else { p.state() },
        clock_pts_us: Some(p.clock(now)),
        decoded_pts_us: p.decoded.unwrap_or(0),
        presented_pts_us: p.presented,
        presentation_id: p.presentation_id,
    })
}

pub(crate) fn fields(key: TrackKey, track: &TrackEntry, now: Instant) -> Option<Vec<(u64, Value)>> {
    let p = track.video_playback.as_ref()?;
    Some(vec![
        (0, Value::Unsigned(key.surface.context)),
        (1, Value::Unsigned(key.surface.surface)),
        (2, Value::Unsigned(key.track)),
        (
            3,
            Value::Unsigned(if track.state.lost { 5 } else { p.state() }),
        ),
        (4, signed(p.clock(now))),
        (5, Value::Unsigned(u64::from(track.state.media_epoch))),
        (6, Value::Unsigned(p.ahead(now))),
        (7, Value::Unsigned(0)),
        (8, Value::Unsigned(p.late)),
        (
            9,
            Value::Unsigned(if p.stopped {
                2
            } else if track.eos_epoch.is_some() {
                1
            } else {
                0
            }),
        ),
        (10, Value::Unsigned(track.state.revision.get())),
    ])
}

pub(crate) fn tick(state: &mut State) {
    if !state.video_host {
        return;
    }
    let now = Instant::now();
    #[cfg(feature = "vector-audio")]
    let masters = state
        .tracks
        .keys()
        .map(|key| (*key, native_audio::master_clock(state, key.surface)))
        .collect::<HashMap<_, _>>();
    let mut changed = false;
    let mut reports = Vec::new();
    let held = state
        .tracks
        .keys()
        .copied()
        .filter(|key| state.timed_track_held(*key))
        .collect::<HashSet<_>>();
    for (key, track) in &mut state.tracks {
        if track.state.lost {
            continue;
        }
        let scope = FrameScope::capture(
            track,
            state.surfaces.get(&key.surface).map(|s| s.state.generation),
            state
                .sessions
                .get(&key.surface.session)
                .filter(|s| !s.closed)
                .map(|s| s.resume_generation),
        );
        let Some(p) = track.video_playback.as_mut() else {
            continue;
        };
        #[cfg(feature = "vector-audio")]
        p.update_master(masters[key], now);
        p.set_held(held.contains(key), now);
        let current = |f: &Frame| scope.as_ref().is_some_and(|scope| scope.accepts(f));
        let before = p.queue.len();
        p.queue.retain(|_, frame| current(frame));
        if track.native_video.as_ref().is_some_and(|f| !current(f)) {
            track.native_video = None;
            changed = true;
        }
        if before != p.queue.len() {
            p.last_end = p
                .queue
                .values()
                .map(|f| f.end)
                .chain(track.native_video.iter().map(|f| f.end))
                .max();
        }
        let drained = track.video_drained == Some(track.state.media_epoch);
        p.start_if_ready(now, drained, track.native_video.as_ref().map(|f| f.pts));
        // A fresh audio callback observation can prove group-clock startup
        // even when a short master already handed over to software above.
        if p.started {
            track.state.milestones |= MILESTONE_CLOCK_STARTED;
        }
        let selected = p.take_due(now).or_else(|| {
            (p.prime_allowed() && track.native_video.is_none())
                .then(|| p.queue.first_key_value().map(|(_, f)| f.clone()))
                .flatten()
        });
        if let Some(mut frame) = selected {
            // Decode order and presentation order can differ. Publish a fresh
            // monotonic ID when the scheduler selects the picture. Paused
            // priming belongs to this PLAY even if decode preceded it.
            let Some(id) = track.native_frame_serial.checked_add(1) else {
                let _ = track.state.lose();
                p.queue.clear();
                track.native_video = None;
                changed = true;
                continue;
            };
            track.native_frame_serial = id;
            frame.id = id;
            frame.play = p.serial;
            track.native_video = Some(frame);
            changed = true;
        }
        let end = drained
            && p.queue.is_empty()
            && p.last_end
                .is_none_or(|end| p.ever_played && p.clock(now) >= end)
            && track.native_video.as_ref().is_none_or(|f| {
                track.native_visual_presented.is_some_and(|(c, e, id)| {
                    c == track.state.channel_generation && e == f.epoch && id >= f.id
                })
            });
        if end && !p.stopped {
            p.finish(now);
            track.state.milestones |= MILESTONE_BUFFERED_ENDED;
        }
        let report = (
            p.state(),
            if p.stopped {
                2
            } else {
                u64::from(track.eos_epoch.is_some())
            },
        );
        let notify = p.reported != Some(report);
        p.reported = Some(report);
        track.outer_position = position(track, now);
        if notify && let Some(fields) = fields(*key, track, now) {
            reports.push((*key, fields));
        }
    }
    if changed {
        advance_projection(state);
    }
    let notify = changed || !reports.is_empty();
    for (key, fields) in reports {
        if let Some(session) = state.sessions.get(&key.surface.session)
            && let Ok(body) = Envelope::new(0, fields).encode()
        {
            let _ = session
                .writer
                .write_record(messages::PLAYBACK_STATE, key.track, &body);
        }
    }
    if notify {
        state.wake_waits();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(pts: i64, end: i64, id: u64) -> Frame {
        Frame {
            incarnation: Weak::new(),
            image: Arc::new(RasterImage::new(1, 1, Arc::from([0, 0, 0, 255])).unwrap()),
            epoch: 0,
            id,
            pts,
            end,
            reset: 1,
            play: 0,
            surface: SurfaceGeneration::ONE,
            generation: ChannelGeneration::ONE,
            resume: 0,
        }
    }
    fn request(start: i64, minimum: u64) -> PlayRequest {
        PlayRequest {
            start_pts_us: start,
            minimum_buffer_us: minimum,
            ..PlayRequest::baseline()
        }
    }
    #[test]
    fn synchronized_software_start_needs_a_picture_and_its_own_buffer_horizon() {
        let now = Instant::now();
        let mut p = Playback::new(4);
        let mut play = request(10_000, 0);
        play.start_policy = 2;
        p.play(play).unwrap();
        assert!(!p.start_if_ready(now, false, None));
        p.push(frame(0, 40_000, 1));
        assert!(p.queue.is_empty());
        assert!(!p.start_if_ready(now, false, None));
        p.push(frame(20_000, 60_000, 2));
        assert!(p.start_if_ready(now, false, None));
        p.pause(now);
        play.minimum_buffer_us = 100_000;
        p.play(play).unwrap();
        assert!(!p.start_if_ready(now, false, None));
        p.push(frame(60_000, 100_000, 3));
        assert!(!p.start_if_ready(now, false, None));
        p.push(frame(100_000, 140_000, 4));
        assert!(p.start_if_ready(now, false, None));
        assert_eq!(p.clock(now), 10_000);
    }
    #[test]
    fn synchronized_drained_eos_resolves_no_picture_without_rebasing_the_target() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        let mut play = request(70_000, 100_000);
        play.start_policy = 2;
        p.play(play).unwrap();
        assert!(!p.start_if_ready(now, false, None));
        assert!(p.start_if_ready(now, true, None));
        assert_eq!(p.clock(now + Duration::from_secs(1)), 70_000);
        assert!(p.take_due(now).is_none());
    }
    #[test]
    #[cfg(feature = "vector-audio")]
    fn a_final_audio_start_observation_survives_a_hidden_software_handoff() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        p.push(frame(0, 40_000, 1));
        let mut play = request(0, 100_000);
        play.start_policy = 2;
        p.play(play).unwrap();
        p.update_master(
            Some(native_audio::Clock {
                pts: 0,
                started: false,
                playing: true,
                paused: false,
                ended: false,
            }),
            now,
        );
        assert!(!p.start_if_ready(now, false, None));
        p.set_held(true, now);
        p.update_master(
            Some(native_audio::Clock {
                pts: 40_000,
                started: true,
                playing: false,
                paused: true,
                ended: true,
            }),
            now,
        );
        assert_eq!(p.state(), 3);
        let shown = now + Duration::from_secs(5);
        assert_eq!(p.clock(shown), 40_000);
        p.set_held(false, shown);
        assert!(p.start_if_ready(shown, false, None));
        assert_eq!(p.clock(shown + Duration::from_micros(10_000)), 50_000);
    }
    #[test]
    fn visibility_freezes_software_time_and_releases_without_new_preroll_or_play() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        p.set_held(true, now);
        p.push(frame(0, 100_000, 1));
        p.play(request(0, 100_000)).unwrap();
        assert!(!p.start_if_ready(now, false, None));
        assert_eq!(p.clock(now + Duration::from_secs(5)), 0);
        p.set_held(false, now);
        assert!(p.start_if_ready(now, false, None));
        p.take_due(now).unwrap();
        p.set_held(true, now + Duration::from_micros(25_000));
        assert_eq!(p.state(), 3);
        assert_eq!(p.clock(now + Duration::from_secs(5)), 25_000);
        assert!(!p.pre_roll_full());
        let shown = now + Duration::from_secs(5);
        p.set_held(false, shown);
        assert!(p.start_if_ready(shown, false, None));
        assert_eq!(p.clock(shown + Duration::from_micros(10_000)), 35_000);
        p.pause(shown + Duration::from_micros(10_000));
        p.set_held(true, shown);
        p.set_held(false, shown);
        assert!(!p.start_if_ready(shown, false, None));
        assert_eq!(p.clock(shown + Duration::from_secs(5)), 35_000);
    }
    #[test]
    fn exact_start_and_pause_do_not_rebase_to_a_future_picture() {
        let now = Instant::now();
        let mut p = Playback::new(4);
        p.push(frame(100_000, 150_000, 1));
        p.push(frame(150_000, 200_000, 2));
        p.push(frame(200_000, 250_000, 3));
        p.play(request(115_000, 80_000)).unwrap();
        assert!(p.start_if_ready(now, false, None));
        assert_eq!(p.clock(now), 115_000);
        assert!(p.take_due(now + Duration::from_micros(34_999)).is_none());
        let old = p.take_due(now + Duration::from_micros(35_000)).unwrap();
        assert_eq!((old.pts, old.play), (150_000, 1));
        p.pause(now + Duration::from_micros(40_000));
        assert_eq!(p.clock(now + Duration::from_secs(3600)), 155_000);
        assert!(p.take_due(now + Duration::from_secs(3600)).is_none());
        p.play(request(155_000, 0)).unwrap();
        let resumed = now + Duration::from_secs(3600);
        assert!(p.start_if_ready(resumed, false, None));
        assert_eq!(p.clock(resumed), 155_000);
        let next = p.take_due(resumed + Duration::from_micros(45_000)).unwrap();
        assert_eq!((next.pts, next.play), (200_000, 2));
        assert_ne!(old.play, p.serial);
    }
    #[test]
    fn only_buffer_horizon_or_drained_eos_releases_pre_roll() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        p.push(frame(0, 40_000, 1));
        p.play(request(0, 100_000)).unwrap();
        assert!(!p.start_if_ready(now, false, None));
        p.push(frame(40_000, 80_000, 2));
        assert!(!p.start_if_ready(now, false, None));
        assert!(p.start_if_ready(now, true, None));
        assert_eq!(p.clock(now), 0);
        assert_eq!(p.take_due(now).unwrap().pts, 0);
        assert!(p.take_due(now + Duration::from_micros(39_999)).is_none());
        assert_eq!(
            p.take_due(now + Duration::from_micros(40_000)).unwrap().pts,
            40_000
        );
        let mut buffered = Playback::new(3);
        buffered.push(frame(0, 100_000, 1));
        buffered.play(request(0, 100_000)).unwrap();
        assert!(buffered.start_if_ready(now, false, None));
    }
    #[test]
    fn drained_clock_stops_for_display_proof_and_late_eos_never_rewinds_it() {
        let now = Instant::now();
        let mut p = Playback::new(2);
        p.push(frame(0, 40_000, 1));
        p.play(request(0, 0)).unwrap();
        p.start_if_ready(now, false, None);
        p.start_if_ready(now + Duration::from_micros(10_000), true, None);
        assert_eq!(p.clock(now + Duration::from_secs(1)), 40_000);
        let mut late = Playback::new(2);
        late.push(frame(0, 40_000, 1));
        late.play(request(0, 0)).unwrap();
        late.start_if_ready(now, false, None);
        let observed = late.clock(now + Duration::from_micros(100_000));
        late.start_if_ready(now + Duration::from_micros(120_000), true, None);
        late.finish(now + Duration::from_micros(120_000));
        assert!(late.clock(now + Duration::from_secs(1)) >= observed);
    }

    #[test]
    #[cfg(feature = "vector-audio")]
    fn admitted_audio_holds_video_without_starting_an_independent_clock() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        p.push(frame(0, 40_000, 1));
        p.push(frame(40_000, 80_000, 2));
        p.play(request(0, 40_000)).unwrap();
        p.update_master(
            Some(native_audio::Clock {
                pts: 0,
                started: false,
                playing: true,
                paused: false,
                ended: false,
            }),
            now,
        );
        assert!(!p.start_if_ready(now + Duration::from_secs(1), false, None));
        assert_eq!(p.clock(now + Duration::from_secs(1)), 0);
        p.update_master(
            Some(native_audio::Clock {
                pts: 45_000,
                started: true,
                playing: true,
                paused: false,
                ended: false,
            }),
            now,
        );
        assert!(p.start_if_ready(now, false, None));
        assert_eq!(p.take_due(now).unwrap().pts, 40_000);
        assert_eq!(p.clock(now + Duration::from_secs(3600)), 45_000);
        assert!(p.take_due(now + Duration::from_secs(3600)).is_none());
    }

    #[test]
    #[cfg(feature = "vector-audio")]
    fn group_pause_reports_frozen_only_after_the_audio_callback_settles() {
        let now = Instant::now();
        let mut p = Playback::new(3);
        p.push(frame(0, 100_000, 1));
        p.play(request(0, 0)).unwrap();
        let mut clock = native_audio::Clock {
            pts: 20_000,
            started: true,
            playing: true,
            paused: false,
            ended: false,
        };
        p.update_master(Some(clock), now);
        assert!(p.start_if_ready(now, false, None));
        p.pause(now);
        clock.playing = false;
        p.update_master(Some(clock), now);
        assert_eq!(p.state(), 1, "the audio callback can still advance");
        clock.pts = 40_000;
        clock.paused = true;
        p.update_master(Some(clock), now);
        assert_eq!(p.state(), 3);
        assert_eq!(p.clock(now + Duration::from_secs(60)), 40_000);
        assert!(p.take_due(now).is_none());
    }

    #[test]
    #[cfg(feature = "vector-audio")]
    fn audio_retirement_and_eos_continue_from_the_last_master_clock() {
        for (ended, video_started) in [(false, false), (false, true), (true, false), (true, true)] {
            let now = Instant::now();
            let mut p = Playback::new(3);
            p.push(frame(0, 100_000, 1));
            p.push(frame(100_000, 200_000, 2));
            p.play(request(0, 0)).unwrap();
            p.update_master(
                Some(native_audio::Clock {
                    pts: 70_000,
                    started: true,
                    playing: true,
                    paused: false,
                    ended: false,
                }),
                now,
            );
            if video_started {
                assert!(p.start_if_ready(now, false, None));
            }
            let clock = ended.then_some(native_audio::Clock {
                pts: 80_000,
                started: true,
                playing: true,
                paused: false,
                ended: true,
            });
            p.update_master(clock, now);
            assert_eq!(p.clock(now), if ended { 80_000 } else { 70_000 });
            assert_eq!(p.start_pts(), 0);
            assert_eq!(
                p.clock(now + Duration::from_micros(30_000)),
                if ended { 110_000 } else { 100_000 }
            );
            let held = now + Duration::from_micros(30_000);
            p.set_held(true, held);
            let frozen = p.clock(held);
            assert_eq!(p.state(), 3);
            let shown = held + Duration::from_secs(5);
            assert_eq!(p.clock(shown), frozen);
            p.set_held(false, shown);
            assert!(p.start_if_ready(shown, false, None));
            assert_eq!(
                p.clock(shown + Duration::from_micros(10_000)),
                frozen + 10_000
            );
        }
    }
}
