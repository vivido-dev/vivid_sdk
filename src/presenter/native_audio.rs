//! Source-scoped native audio admission, device ownership and bounded decode work.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::audio_output::{Channel, Output, OutputControl};
use std::sync::{
    Weak,
    atomic::{AtomicI64, AtomicU64, AtomicUsize},
};

// Reserve both conversion allocations and the device ring before native work.
// Native codec/device internals remain additional, as in the video integration.
const BATCH_SAMPLES: usize = 2 * 1024 * 1024;
const RING_SAMPLES: usize = 4 * 1024 * 1024;
const RECHECK: Duration = Duration::from_millis(10);
const SILENCE_SAMPLES: usize = 4096;
#[path = "native_audio/drain.rs"]
mod drain;
#[cfg(all(test, feature = "testing"))]
#[path = "native_audio_failure_tests.rs"]
mod failure_tests;
#[path = "native_audio/timeline.rs"]
mod timeline;
pub(super) use drain::prepare as prepare_drain;
pub(super) use drain::{DrainAdmission, PendingDrain};
// Successful device initialization always allocates this complete duration;
// requests above it could never satisfy their startup reserve.
const RING_DURATION_US: u64 = 2_000_000;

pub(super) struct Reservation {
    token: Arc<()>,
    work: Mutex<Weak<()>>,
}
fn bytes(config: &TrackConfiguration) -> Option<usize> {
    if config.direction != crate::TrackDirection::Downlink
        || !matches!(config.kind, KindConfiguration::Audio(_))
    {
        return None;
    }
    RING_SAMPLES
        .checked_add(BATCH_SAMPLES.checked_mul(2)?)?
        .checked_mul(std::mem::size_of::<f32>())?
        .checked_add(
            crate::audio::Limits::default()
                .max_output_frames
                .checked_mul(std::mem::size_of::<crate::audio::DecodedSpan>())?,
        )?
        .checked_add(SILENCE_SAMPLES.checked_mul(std::mem::size_of::<f32>())?)?
        .checked_add((config.maximum_record_body as usize).checked_mul(2)?)
}
pub(super) fn admissible(state: &State, config: &TrackConfiguration) -> bool {
    if config.direction == crate::TrackDirection::Uplink
        || !state.overlays.local_native()
        || !matches!(config.kind, KindConfiguration::Audio(_))
    {
        return true;
    }
    let Some(bytes) = bytes(config) else {
        return false;
    };
    state.audio_host
        && state
            .native_charges
            .iter()
            .filter(|(_, t)| t.strong_count() != 0)
            .try_fold(bytes as u64, |sum, (n, _)| sum.checked_add(*n as u64))
            .is_some_and(|n| n <= state.config.media.aggregate_retained_bytes)
}
pub(super) fn reserve(
    state: &mut State,
    config: &TrackConfiguration,
) -> io::Result<Arc<Reservation>> {
    if !admissible(state, config) {
        return Err(invalid("native audio budget unavailable"));
    }
    Ok(Arc::new(Reservation {
        token: native_vector::charge(
            state,
            bytes(config).ok_or_else(|| invalid("native audio mode unsupported"))?,
        )?,
        work: Mutex::new(Weak::new()),
    }))
}

// A packet's scope is captured while its wire admission still holds the state
// lock. Neither a queued job nor a native call may recapture a replacement epoch.
#[derive(Clone)]
pub(super) struct Admission {
    key: TrackKey,
    generation: ChannelGeneration,
    epoch: u32,
    reset: u64,
    resume: u64,
    surface: SurfaceGeneration,
    writer: Arc<Writer>,
    incarnation: Weak<()>,
}
impl Admission {
    pub(super) fn capture(
        state: &State,
        key: TrackKey,
        generation: ChannelGeneration,
        writer: &Arc<Writer>,
    ) -> Option<Self> {
        let track = state.tracks.get(&key)?;
        if !owns_media_channel(track, generation, writer) {
            return None;
        }
        let scope = Self {
            key,
            generation,
            epoch: track.state.media_epoch,
            reset: track.decoder_reset_serial,
            resume: state.sessions.get(&key.surface.session)?.resume_generation,
            surface: state.surfaces.get(&key.surface)?.state.generation,
            writer: writer.clone(),
            incarnation: Arc::downgrade(&track.audio_reservation.as_ref()?.token),
        };
        scope.current(state).then_some(scope)
    }
    fn same(&self, other: &Self) -> bool {
        self.key == other.key
            && self.generation == other.generation
            && self.epoch == other.epoch
            && self.reset == other.reset
            && self.resume == other.resume
            && self.surface == other.surface
            && Arc::ptr_eq(&self.writer, &other.writer)
            && Weak::ptr_eq(&self.incarnation, &other.incarnation)
    }
    fn current(&self, state: &State) -> bool {
        state.audio_host
            && state
                .sessions
                .get(&self.key.surface.session)
                .is_some_and(|s| !s.closed && s.resume_generation == self.resume)
            && state
                .surfaces
                .get(&self.key.surface)
                .is_some_and(|s| s.state.generation == self.surface)
            && state.tracks.get(&self.key).is_some_and(|t| {
                !t.state.lost
                    && t.state.channel_generation == self.generation
                    && t.state.media_epoch == self.epoch
                    && t.decoder_reset_serial == self.reset
                    && t.audio_reservation
                        .as_ref()
                        .is_some_and(|r| Weak::ptr_eq(&self.incarnation, &Arc::downgrade(&r.token)))
                    && (owns_media_channel(t, self.generation, &self.writer)
                        || (t.channel_writer.is_none() && t.eos_epoch == Some(self.epoch)))
            })
    }
}

struct Control {
    stopped: AtomicBool,
    output: Mutex<Option<OutputControl>>,
    decoded: AtomicI64,
    play_baseline: AtomicU64,
    started_play: AtomicU64,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            output: Mutex::new(None),
            decoded: AtomicI64::new(i64::MIN),
            play_baseline: AtomicU64::new(0),
            started_play: AtomicU64::new(0),
        }
    }
}
impl Control {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(output) = lock(&self.output).as_ref() {
            output.stop();
        }
    }
}
pub(super) struct Binding {
    scope: Admission,
    control: Arc<Control>,
    jobs: mpsc::SyncSender<Job>,
}
impl Drop for Binding {
    fn drop(&mut self) {
        self.control.stop();
    }
}

pub(super) fn failed(state: &mut State, scope: &Admission) {
    if !scope.current(state) {
        return;
    }
    super::native_hold::device_failed(state, scope.key);
    let track = state
        .tracks
        .get_mut(&scope.key)
        .expect("failed audio owner");
    let _ = track.state.lose();
    if let Some(writer) = &track.channel_writer {
        writer.close();
    }
    state.wake_waits();
}

#[cfg(all(test, feature = "testing", feature = "vector-video"))]
pub(super) fn fail_output_for_test(state: &State, key: TrackKey) -> bool {
    if !state.audio_test_device.clocked() {
        return false;
    }
    stop_bound_output_for_test(state, key)
}

#[cfg(feature = "testing")]
pub(super) fn stop_bound_output_for_test(state: &State, key: TrackKey) -> bool {
    let Some(binding) = state
        .tracks
        .get(&key)
        .and_then(|track| track.native_audio.as_ref())
    else {
        return false;
    };
    if !binding.scope.current(state) {
        return false;
    }
    lock(&binding.control.output)
        .as_ref()
        .is_some_and(|output| {
            output.stop();
            true
        })
}
struct Job {
    record: Option<Record>,
    done: mpsc::SyncSender<io::Result<()>>,
}
pub(super) fn clear(track: &mut TrackEntry) {
    stop_binding(track);
    if track.audio_reservation.is_some() && track.configuration.mode == TrackMode::Timed {
        track.audio_play_serial = 0;
        track.playing = false;
    }
}
fn stop_binding(track: &mut TrackEntry) {
    if track.native_audio.take().is_some() {
        track.outer_position = None;
    }
}

pub(super) fn validate_activation(
    state: &State,
    surface: SurfaceKey,
    active: &HashMap<u64, u64>,
) -> io::Result<()> {
    if state
        .surfaces
        .get(&surface)
        .is_some_and(|s| s.active_slots.get(&crate::SLOT_AUDIO) != active.get(&crate::SLOT_AUDIO))
        && active.values().any(|id| {
            state
                .tracks
                .get(&TrackKey {
                    surface,
                    track: *id,
                })
                .is_some_and(|t| {
                    t.configuration.mode == TrackMode::Timed
                        && t.playing
                        && matches!(t.configuration.kind, KindConfiguration::Video(_))
                })
        })
    {
        return Err(invalid(
            "pause timed video before replacing its audio master",
        ));
    }
    validate_members(state, surface, active.values().copied())
}
fn validate_members(
    state: &State,
    surface: SurfaceKey,
    members: impl Iterator<Item = u64>,
) -> io::Result<()> {
    if !state.overlays.local_native() {
        return Ok(());
    }
    let mut audio = false;
    let mut video = false;
    let mut timed = false;
    let mut live = false;
    let mut intent = None;
    let mut inconsistent = false;
    for id in members {
        if let Some(track) = state.tracks.get(&TrackKey { surface, track: id }) {
            if track.state.lost
                || (track.audio_reservation.is_some()
                    && track.state.milestones & MILESTONE_BUFFERED_ENDED != 0)
            {
                continue;
            }
            audio |= track.audio_reservation.is_some();
            video |= matches!(track.configuration.kind, KindConfiguration::Video(_));
            timed |= track.configuration.mode == TrackMode::Timed;
            live |= track.configuration.mode == TrackMode::Live;
            if let Some((playing, request)) = intent {
                inconsistent |=
                    playing != track.playing || (playing && request != track.play_request);
            } else {
                intent = Some((track.playing, track.play_request));
            }
        }
    }
    if audio && video && ((timed && (live || inconsistent)) || !cfg!(feature = "vector-video")) {
        return Err(invalid(
            "native A/V activation requires matching modes and timed intent",
        ));
    }
    Ok(())
}

fn validate_play(track: &TrackEntry, request: PlayRequest) -> io::Result<()> {
    if track.audio_reservation.is_none() {
        return Ok(());
    }
    if track.state.lost
        || track.configuration.mode != TrackMode::Timed
        || !matches!(request.start_policy, 1 | 2)
        || (request.hold_serial.is_some() && request.start_policy != 2)
        || request.start_pts_us == i64::MIN
        || request.minimum_buffer_us > RING_DURATION_US
        || request.maximum_latency_us > track.configuration.maximum_latency_us
        || track.state.milestones & MILESTONE_BUFFERED_ENDED != 0
    {
        return Err(invalid("native timed audio PLAY exceeds admitted policy"));
    }
    if track.audio_play_serial == u64::MAX {
        return Err(invalid("audio PLAY serial exhausted"));
    }
    if let Some(binding) = &track.native_audio
        && let Some(output) = lock(&binding.control.output).as_ref()
    {
        output.validate_timed_queue(request.start_pts_us, request.minimum_buffer_us)?;
    }
    Ok(())
}

// Validate every active member before configuring the single native device or
// mutating PLAY intent. A video-targeted PLAY must admit its audio master too.
pub(super) fn admit_group_play(
    state: &mut State,
    key: TrackKey,
    request: PlayRequest,
) -> io::Result<()> {
    let keys = group_keys(state, key);
    let audio = keys
        .iter()
        .copied()
        .filter(|k| {
            state.tracks.get(k).is_some_and(|t| {
                t.audio_reservation.is_some()
                    && (*k == key
                        || (!t.state.lost && t.state.milestones & MILESTONE_BUFFERED_ENDED == 0))
            })
        })
        .collect::<Vec<_>>();
    if audio.len() > 1 {
        return Err(invalid("PLAY cannot address two native audio masters"));
    }
    validate_members(state, key.surface, keys.iter().map(|k| k.track))?;
    for key in &audio {
        validate_play(&state.tracks[key], request)?;
    }
    for key in audio {
        let track = state.tracks.get_mut(&key).expect("validated audio member");
        if let Some(binding) = &track.native_audio
            && let Some(output) = lock(&binding.control.output).as_ref()
        {
            output.configure_timed_queue(request.start_pts_us, request.minimum_buffer_us)?;
            output.pause();
            binding
                .control
                .play_baseline
                .store(output.rendered_frames(), Ordering::Release);
        }
        track.audio_play_serial += 1;
    }
    Ok(())
}

fn group_keys(state: &State, key: TrackKey) -> HashSet<TrackKey> {
    let mut keys = state
        .surfaces
        .get(&key.surface)
        .map(|s| {
            s.active_slots
                .values()
                .map(|track| TrackKey {
                    surface: key.surface,
                    track: *track,
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    keys.insert(key);
    keys
}

#[cfg(feature = "vector-video")]
#[derive(Clone, Copy)]
pub(super) struct Clock {
    pub pts: i64,
    pub started: bool,
    pub playing: bool,
    pub paused: bool,
    pub ended: bool,
}

#[cfg(feature = "vector-video")]
pub(super) fn master_clock(state: &State, surface: SurfaceKey) -> Option<Clock> {
    clock_for(state, surface, TrackMode::Timed)
}
#[cfg(feature = "vector-video")]
pub(super) fn live_clock(state: &State, surface: SurfaceKey) -> Option<Clock> {
    clock_for(state, surface, TrackMode::Live)
}
#[cfg(feature = "vector-video")]
fn clock_for(state: &State, surface: SurfaceKey, mode: TrackMode) -> Option<Clock> {
    let id = state
        .surfaces
        .get(&surface)?
        .active_slots
        .get(&crate::SLOT_AUDIO)?;
    let track = state.tracks.get(&TrackKey {
        surface,
        track: *id,
    })?;
    if track.state.lost
        || track.audio_reservation.is_none()
        || (mode == TrackMode::Timed && track.audio_play_serial == 0)
        || track.configuration.mode != mode
        || state
            .sessions
            .get(&surface.session)
            .is_none_or(|s| s.closed)
    {
        return None;
    }
    let playing = mode == TrackMode::Live
        || (track.playing
            && track.audio_play_serial != 0
            && !state.timed_track_held(TrackKey {
                surface,
                track: *id,
            }));
    let mut clock = Clock {
        pts: track.play_request.start_pts_us,
        started: false,
        playing,
        paused: mode == TrackMode::Timed && !playing,
        ended: track.state.milestones & MILESTONE_BUFFERED_ENDED != 0,
    };
    if let Some(binding) = &track.native_audio {
        if !binding.scope.current(state) {
            return None;
        }
        if let Some(output) = lock(&binding.control.output).as_ref() {
            clock.pts = output.clock_pts().unwrap_or(clock.pts);
            if !clock.playing {
                let paused = output.paused_clock();
                let confirmed = track.outer_position.filter(|p| p.state == 3);
                clock.paused = paused.is_some() || confirmed.is_some();
                if let Some(pts) = paused
                    .and_then(|clock| clock.pts_us)
                    .or_else(|| confirmed.and_then(|p| p.clock_pts_us))
                {
                    clock.pts = pts;
                }
            }
            clock.started = if mode == TrackMode::Live {
                output.rendered_frames() != 0
            } else {
                binding.control.started_play.load(Ordering::Acquire) == track.audio_play_serial
            };
        }
    }
    Some(clock)
}

// Handles stored in State contain no CPAL stream or native decoder. Dropping a
// binding only flips atomics, even while its device worker is in a native call.
pub(super) fn hold_identity(track: &TrackEntry) -> Option<(Weak<()>, u64)> {
    Some((
        Arc::downgrade(&track.audio_reservation.as_ref()?.token),
        track.audio_play_serial,
    ))
}

pub(super) fn recovery_ready(state: &State, key: TrackKey) -> bool {
    let Some(track) = state.tracks.get(&key) else {
        return false;
    };
    if track.audio_reservation.is_none() {
        return true;
    }
    track.native_audio.as_ref().is_some_and(|binding| {
        binding.scope.current(state)
            && ((track.state.milestones & MILESTONE_BUFFERED_ENDED != 0
                && track.eos_epoch == Some(track.state.media_epoch))
                || (!binding.control.stopped.load(Ordering::Acquire)
                    && track.audio_play_serial != 0
                    && lock(&binding.control.output)
                        .as_ref()
                        .is_some_and(|output| {
                            output.ready_to_start() && output.paused_clock().is_some()
                        })))
    })
}

pub(super) fn hold_position(
    state: &State,
    key: TrackKey,
) -> Option<super::super::BridgePositionSnapshot> {
    let track = state.tracks.get(&key)?;
    let binding = track.native_audio.as_ref()?;
    (binding.scope.current(state)
        && binding.control.started_play.load(Ordering::Acquire) == track.audio_play_serial)
        .then_some(track.outer_position)
        .flatten()
}

pub(super) fn tick(state: &mut State) {
    let stale = state
        .tracks
        .iter()
        .filter_map(|(key, t)| {
            t.native_audio
                .as_ref()
                .filter(|b| !b.scope.current(state))
                .map(|_| *key)
        })
        .collect::<Vec<_>>();
    let mut changed = !stale.is_empty();
    for key in stale {
        clear(state.tracks.get_mut(&key).expect("listed audio track"));
    }
    #[cfg(feature = "vector-video")]
    let ready = state
        .tracks
        .keys()
        .map(|key| (*key, native_video::group_ready(state, key.surface)))
        .collect::<HashMap<_, _>>();
    let held = state
        .tracks
        .keys()
        .copied()
        .filter(|key| state.timed_track_held(*key))
        .collect::<HashSet<_>>();
    for (key, track) in &mut state.tracks {
        let Some(binding) = &track.native_audio else {
            continue;
        };
        let output = lock(&binding.control.output);
        let Some(output) = output.as_ref() else {
            continue;
        };
        let previous_milestones = track.state.milestones;
        let previous_position = track.outer_position;
        output.set_gain(track.audio_gain);
        let active = state
            .surfaces
            .get(&key.surface)
            .is_some_and(|s| s.active_slots.get(&track.configuration.slot) == Some(&key.track));
        let timed = track.configuration.mode == TrackMode::Timed;
        let playing = active
            && (!timed || (track.playing && track.audio_play_serial != 0 && !held.contains(key)));
        #[cfg(not(feature = "vector-video"))]
        let buffered = true;
        #[cfg(feature = "vector-video")]
        let buffered = !timed
            || binding.control.started_play.load(Ordering::Acquire) == track.audio_play_serial
            || ready[key];
        if playing && buffered {
            output.start();
        } else {
            output.pause();
        }
        if timed && output.rendered_frames() > binding.control.play_baseline.load(Ordering::Acquire)
        {
            binding
                .control
                .started_play
                .store(track.audio_play_serial, Ordering::Release);
        }
        if output.rendered_frames() != 0
            && (!timed
                || binding.control.started_play.load(Ordering::Acquire) == track.audio_play_serial)
        {
            track.state.milestones |= MILESTONE_CLOCK_STARTED;
        }
        if output.presented_frames() != 0 {
            track.state.milestones |= MILESTONE_PRESENTED;
        }
        let decoded = binding.control.decoded.load(Ordering::Acquire);
        let rendered = output.rendered_frames();
        let presented = output.presented_frames();
        let paused = (!playing).then(|| output.paused_clock()).flatten();
        // Once this paused binding was confirmed, a later silent callback
        // cannot advance it. PLAY/PAUSE and binding replacement clear the
        // previous position before any new consuming operation can start.
        let confirmed = previous_position.filter(|p| !playing && p.state == 3);
        let clock = if playing {
            output.clock_pts()
        } else {
            paused
                .and_then(|clock| clock.pts_us)
                .or_else(|| confirmed.and_then(|p| p.clock_pts_us))
        };
        track.outer_position = Some(crate::presenter::BridgePositionSnapshot {
            decoder_reset_serial: track.decoder_reset_serial,
            playing,
            start_pts_us: if timed {
                track.play_request.start_pts_us
            } else {
                0
            },
            state: if track.state.milestones & MILESTONE_BUFFERED_ENDED != 0 {
                4
            } else if playing {
                if rendered == 0 || !buffered { 1 } else { 2 }
            } else if paused.is_some() || confirmed.is_some() {
                3
            } else {
                // A callback admitted before PAUSE may still commit its last
                // block. Do not publish that earlier position as frozen.
                1
            },
            clock_pts_us: clock,
            decoded_pts_us: if decoded == i64::MIN { 0 } else { decoded },
            presented_pts_us: output.presented_pts().unwrap_or(0),
            presentation_id: presented,
        });
        changed |= previous_milestones != track.state.milestones
            || previous_position != track.outer_position;
    }
    if changed {
        state.wake_waits();
    }
    super::native_hold::tick(state);
}

struct WorkerGuard {
    count: Arc<AtomicUsize>,
    changed: Arc<Condvar>,
    _work: Arc<()>,
    _reservation: Arc<Reservation>,
}
struct SilenceOnDrop(OutputControl);
impl Drop for SilenceOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Release);
        self.changed.notify_all();
    }
}

fn ensure_worker(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    scope: &Admission,
    wait: bool,
) -> io::Result<bool> {
    let mut state = lock(shared);
    if !scope.current(&state) {
        return Ok(false);
    }
    let replace = state.tracks[&scope.key]
        .native_audio
        .as_ref()
        .is_none_or(|b| !b.scope.same(scope));
    if replace {
        stop_binding(state.tracks.get_mut(&scope.key).expect("admitted audio"));
        let reservation = state.tracks[&scope.key]
            .audio_reservation
            .clone()
            .ok_or_else(|| invalid("native audio reservation absent"))?;
        let work = loop {
            if !scope.current(&state) {
                return Ok(false);
            }
            let mut previous = lock(&reservation.work);
            if previous.strong_count() == 0 {
                let work = Arc::new(());
                *previous = Arc::downgrade(&work);
                break work;
            }
            drop(previous);
            if !wait {
                return Ok(false);
            }
            state = changed
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        };
        let control = Arc::new(Control::default());
        let (jobs, receiver) = mpsc::sync_channel(1);
        let guard = WorkerGuard {
            count: state.audio_workers.clone(),
            changed: changed.clone(),
            _work: work,
            _reservation: reservation,
        };
        guard.count.fetch_add(1, Ordering::AcqRel);
        let worker_control = control.clone();
        let worker_scope = scope.clone();
        let weak = Arc::downgrade(shared);
        let wake = changed.clone();
        let config = state.tracks[&scope.key].configuration.clone();
        #[cfg(feature = "testing")]
        let test_device = state.audio_test_device.clone();
        #[cfg(not(feature = "testing"))]
        let test_device = DeviceSelection::Default;
        state
            .tracks
            .get_mut(&scope.key)
            .expect("admitted audio")
            .native_audio = Some(Binding {
            scope: scope.clone(),
            control,
            jobs,
        });
        // Publish counted ownership first, then release presenter state even
        // across OS thread creation. Shutdown cannot return the runtime permit
        // while an admitted worker is starting or retiring.
        drop(state);
        let spawned = thread::Builder::new()
            .name("vivid-native-audio".into())
            .spawn(move || {
                let result = run(
                    &weak,
                    &wake,
                    &worker_scope,
                    &worker_control,
                    receiver,
                    config,
                    test_device,
                );
                worker_control.stop();
                if result.is_err()
                    && let Some(shared) = weak.upgrade()
                {
                    let mut state = lock(&shared);
                    failed(&mut state, &worker_scope);
                }
                drop(guard);
            });
        state = lock(shared);
        if let Err(error) = spawned {
            let current = scope.current(&state);
            if let Some(track) = state.tracks.get_mut(&scope.key)
                && track
                    .native_audio
                    .as_ref()
                    .is_some_and(|b| b.scope.same(scope))
            {
                stop_binding(track);
            }
            return if current { Err(error) } else { Ok(false) };
        }
        if !scope.current(&state) {
            return Ok(false);
        }
    }
    Ok(true)
}

// PLAY may precede media. Prime only available source permits, without waiting
// for a retiring native destructor or sending a synthetic media/EOS job.
pub(super) fn prime_requested(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    owner: Option<u64>,
) {
    let scopes = {
        let state = lock(shared);
        state
            .tracks
            .iter()
            .filter(|(key, track)| {
                owner.is_none_or(|owner| key.surface.session == owner)
                    && track.audio_reservation.is_some()
                    && track.configuration.mode == TrackMode::Timed
                    && track.audio_play_serial != 0
                    && track.playing
                    && track.native_audio.is_none()
                    && track.eos_epoch.is_none()
            })
            .take(8)
            .filter_map(|(key, track)| {
                Admission::capture(
                    &state,
                    *key,
                    track.state.channel_generation,
                    track.channel_writer.as_ref()?,
                )
            })
            .collect::<Vec<_>>()
    };
    for scope in scopes {
        if ensure_worker(shared, changed, &scope, false).is_err() {
            let mut state = lock(shared);
            failed(&mut state, &scope);
        }
    }
}

pub(super) fn submit(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    scope: Admission,
    record: Option<Record>,
) -> io::Result<()> {
    let length = record.as_ref().map(|r| r.body.len());
    let mut state = lock(shared);
    tick(&mut state);
    if !scope.current(&state) {
        return credit(&mut state, &scope, length);
    }
    if record.is_some() && state.tracks[&scope.key].eos_epoch.is_some() {
        credit(&mut state, &scope, length)?;
        return Err(invalid("native audio after EOS"));
    }
    if record.is_none()
        && state.tracks[&scope.key].state.last_media_id == 0
        && state.tracks[&scope.key].native_audio.is_none()
    {
        // There is no decoder or device work to drain. In particular, an empty
        // stream must not need a physical audio device just to accept EOS.
        state
            .tracks
            .get_mut(&scope.key)
            .expect("empty audio owner")
            .state
            .milestones |= MILESTONE_BUFFERED_ENDED;
        state.wake_waits();
        drop(state);
        changed.notify_all();
        return Ok(());
    }
    drop(state);
    if let Err(error) = ensure_worker(shared, changed, &scope, true) {
        return complete(shared, changed, &scope, length, Err(error));
    }
    let mut state = lock(shared);
    if !scope.current(&state) {
        return credit(&mut state, &scope, length);
    }
    let binding = state.tracks[&scope.key]
        .native_audio
        .as_ref()
        .expect("audio worker installed");
    let jobs = binding.jobs.clone();
    let control = binding.control.clone();
    let (done, completion) = mpsc::sync_channel(1);
    let mut job = Job { record, done };
    loop {
        if !scope.current(&state) {
            return credit(&mut state, &scope, length);
        }
        if control.stopped.load(Ordering::Acquire) {
            credit(&mut state, &scope, length)?;
            return Err(invalid("native audio worker ended"));
        }
        match jobs.try_send(job) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(returned)) => job = returned,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                credit(&mut state, &scope, length)?;
                return Err(invalid("native audio worker ended"));
            }
        }
        state = changed
            .wait_timeout(state, RECHECK)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
    drop(state);
    let result = loop {
        match completion.recv_timeout(RECHECK) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(invalid("native audio worker ended"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Ingress alone returns credit, including a queued job dropped during
        // retirement before native code receives it. The worker never credits.
        if control.stopped.load(Ordering::Acquire) {
            break Err(invalid("native audio worker retired before completion"));
        }
    };
    complete(shared, changed, &scope, length, result)
}

pub(super) fn complete(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    scope: &Admission,
    length: Option<usize>,
    result: io::Result<()>,
) -> io::Result<()> {
    let mut state = lock(shared);
    let current = scope.current(&state);
    credit(&mut state, scope, length)?;
    state.wake_waits();
    drop(state);
    changed.notify_all();
    // A codec/device error returning after FLUSH belongs to its retired scope.
    // Propagating it would make handle_track lose the replacement epoch on this
    // same canonical channel. Current-source failures still propagate normally.
    if current { result } else { Ok(()) }
}

fn credit(state: &mut State, scope: &Admission, length: Option<usize>) -> io::Result<()> {
    if let Some(length) = length
        && let Some(track) = state
            .tracks
            .get_mut(&scope.key)
            .filter(|t| owns_media_channel(t, scope.generation, &scope.writer))
    {
        let bytes = track
            .state
            .flow
            .maximum_body_bytes
            .checked_add(length as u64)
            .ok_or_else(|| invalid("audio byte credit exhausted"))?;
        let records = track
            .state
            .flow
            .maximum_media_records
            .checked_add(1)
            .ok_or_else(|| invalid("audio record credit exhausted"))?;
        track.state.flow.raise_maxima(bytes, records);
        send_flow_update(scope.key, track);
    }
    Ok(())
}

fn current(shared: &Weak<Mutex<State>>, scope: &Admission, control: &Control) -> bool {
    !control.stopped.load(Ordering::Acquire)
        && shared.upgrade().is_some_and(|s| scope.current(&lock(&s)))
}
#[derive(Clone, Default)]
pub(super) enum DeviceSelection {
    #[default]
    Default,
    #[cfg(feature = "testing")]
    Clocked,
    #[cfg(feature = "testing")]
    Native(crate::audio_output::testing::DeviceId),
}
impl DeviceSelection {
    #[cfg(feature = "testing")]
    pub(super) fn clocked(&self) -> bool {
        matches!(self, Self::Clocked)
    }
}
fn output(generation: u64, selection: DeviceSelection) -> io::Result<Arc<Output>> {
    let limits = crate::audio_output::Limits {
        max_ring_samples: RING_SAMPLES,
        buffer_duration_us: RING_DURATION_US,
    };
    match selection {
        DeviceSelection::Default => Output::open_with_limits(generation, limits),
        #[cfg(feature = "testing")]
        DeviceSelection::Clocked => Ok(Output::test_clocked_output(generation)),
        #[cfg(feature = "testing")]
        DeviceSelection::Native(id) => {
            crate::audio_output::testing::open_device(&id, generation, limits)
        }
    }
}

struct Staging<'a> {
    shared: &'a Weak<Mutex<State>>,
    scope: &'a Admission,
    control: &'a Control,
    output: &'a Output,
    channel: &'a Channel,
    timeline: timeline::Timeline,
    timed: bool,
}
impl Staging<'_> {
    fn stage(&mut self, batch: &crate::audio::DecodedBatch) -> io::Result<()> {
        let channels = usize::from(batch.channels());
        let zeros = [0.0; SILENCE_SAMPLES];
        for span in batch.spans() {
            if !current(self.shared, self.scope, self.control) {
                return Ok(());
            }
            let placement = self.timeline.place(
                span.origin_pts_us(),
                span.frame_offset(),
                batch.sample_rate(),
            )?;
            if placement.first {
                if self.timed {
                    self.channel.observe_timed_queue(
                        span.origin_pts_us()
                            .ok_or_else(|| invalid("native timed audio requires a decoded PTS"))?,
                        span.frame_offset(),
                    )?;
                } else {
                    self.channel.observe_audio_pts(
                        span.pts_us()
                            .ok_or_else(|| invalid("native audio requires a decoded PTS"))?,
                    )?;
                }
            }
            self.control
                .decoded
                .store(span.pts_us().unwrap_or(i64::MIN), Ordering::Release);
            let mut gap = placement.gap;
            while gap != 0 {
                let frames = gap.min((SILENCE_SAMPLES / channels) as u64);
                if !self.enqueue(&zeros[..frames as usize * channels], false)? {
                    return Ok(());
                }
                gap -= frames;
            }
            let skip = placement.skip.min(span.frames()) as usize * channels;
            if !self.enqueue(&batch.samples()[span.sample_range()][skip..], true)? {
                return Ok(());
            }
        }
        Ok(())
    }

    fn enqueue(&mut self, samples: &[f32], real: bool) -> io::Result<bool> {
        let mut offset = 0;
        while offset < samples.len() {
            if !current(self.shared, self.scope, self.control) {
                return Ok(false);
            }
            let capacity = self.output.ring_capacity_samples() as usize;
            let end = offset + (samples.len() - offset).min(capacity);
            let accepted = if self.output.buffered_samples() >= self.output.ring_capacity_samples()
            {
                // Keep the full-ring fast path, but still observe stopped or
                // failed output: that ring can never free space on its own.
                self.output.poll_drained().transpose()?;
                0
            } else {
                match self.channel.try_push(&samples[offset..end]) {
                    Ok(n) => n,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(error) => return Err(error),
                }
            };
            self.timeline
                .advance((accepted / usize::from(self.output.format().channels)) as u64)?;
            offset += accepted;
            if let Some(shared) = self.shared.upgrade() {
                let mut state = lock(&shared);
                if self.scope.current(&state) {
                    let track = state
                        .tracks
                        .get_mut(&self.scope.key)
                        .expect("decoded audio");
                    if real && accepted != 0 {
                        track.state.milestones |= MILESTONE_OUTPUT_READY;
                        track.recovery_pending = false;
                        track.recovery_requested = false;
                    }
                    tick(&mut state);
                    state.wake_waits();
                }
            }
            if accepted == 0 {
                thread::sleep(RECHECK);
            }
        }
        Ok(true)
    }
}
fn run(
    shared: &Weak<Mutex<State>>,
    changed: &Arc<Condvar>,
    scope: &Admission,
    control: &Control,
    jobs: mpsc::Receiver<Job>,
    config: TrackConfiguration,
    test: DeviceSelection,
) -> io::Result<()> {
    if !current(shared, scope, control) {
        return Ok(());
    }
    let output = output(scope.generation.get(), test)?;
    let timed = config.mode == TrackMode::Timed;
    if !timed {
        output.configure_timestamped_live(config.target_latency_us);
    }
    {
        let Some(shared) = shared.upgrade() else {
            return Ok(());
        };
        let state = lock(&shared);
        if !scope.current(&state) {
            return Ok(());
        }
        let mut installed = lock(&control.output);
        if control.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let track = &state.tracks[&scope.key];
        if timed && track.audio_play_serial != 0 {
            output.control().configure_timed_queue(
                track.play_request.start_pts_us,
                track.play_request.minimum_buffer_us,
            )?;
        }
        *installed = Some(output.control());
    }
    let format = output.format();
    let KindConfiguration::Audio(audio) = config.kind else {
        return Err(invalid("native audio kind changed"));
    };
    let mut decoder = crate::audio::Decoder::with_limits(
        &audio,
        format.sample_rate,
        format.channels,
        crate::audio::Limits {
            max_output_samples: BATCH_SAMPLES,
            ..Default::default()
        },
    )?;
    if let Some(shared) = shared.upgrade() {
        let mut state = lock(&shared);
        if scope.current(&state) {
            state
                .tracks
                .get_mut(&scope.key)
                .expect("initialized audio")
                .state
                .milestones |= MILESTONE_DECODER_INITIALIZED;
            state.wake_waits();
        }
    }
    let channel = output.channel(scope.generation.get())?;
    let mut eos = false;
    let mut staging = Staging {
        shared,
        scope,
        control,
        output: &output,
        channel: &channel,
        timeline: timeline::Timeline::default(),
        timed,
    };
    // Drop this before the decoder or stream: a slow native destructor cannot
    // leave old queued samples playing after this worker has failed or retired.
    let _silence = SilenceOnDrop(output.control());
    loop {
        if !current(shared, scope, control) {
            return Ok(());
        }
        if let Some(shared) = shared.upgrade() {
            let mut state = lock(&shared);
            tick(&mut state);
            let drained = if scope.current(&state) {
                output.poll_drained().transpose()?
            } else {
                None
            };
            if eos && drained.is_some() {
                state
                    .tracks
                    .get_mut(&scope.key)
                    .expect("drained audio")
                    .state
                    .milestones |= MILESTONE_BUFFERED_ENDED;
                state.wake_waits();
                return Ok(());
            }
        }
        let job = match jobs.recv_timeout(RECHECK) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let result = (|| {
            if !current(shared, scope, control) {
                return Ok(());
            }
            if eos {
                return Err(invalid("native audio after EOS"));
            }
            let batch = if let Some(record) = &job.record {
                decoder.push_timed(media::parse_audio_packet(&record.body)?)?
            } else {
                eos = true;
                decoder.finish_timed()?
            };
            staging.stage(&batch)?;
            if !current(shared, scope, control) {
                return Ok(());
            }
            if eos {
                channel.finish_decode()?;
                channel.signal_eos()?;
            }
            Ok(())
        })();
        changed.notify_all();
        let failed = result.is_err();
        let _ = job.done.send(result);
        if failed {
            return Err(invalid("native audio decode or output failed"));
        }
    }
}
