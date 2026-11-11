//! Owner-scoped video decoding and bounded timed presentation.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::presenter::BridgePositionSnapshot;
use crate::vector_renderer::RasterImage;
use std::sync::Weak;
#[cfg(feature = "vector-audio")]
#[path = "native_video/live.rs"]
mod live;
#[path = "native_video/playback.rs"]
mod playback;
#[cfg(feature = "vector-audio")]
pub(super) use live::Pending;
#[cfg(feature = "vector-audio")]
pub(super) use playback::group_ready;
pub(super) use playback::{
    Playback, control, fields as playback_fields, position as playback_position, recovery_ready,
    validate_group_play,
};

pub(super) fn tick(state: &mut State) {
    playback::tick(state);
    #[cfg(feature = "vector-audio")]
    live::tick(state);
    super::native_hold::tick(state);
}

pub(super) fn hold_identity(track: &TrackEntry) -> Option<(Weak<()>, u64)> {
    Some((
        Arc::downgrade(&track.video_reservation.as_ref()?._token),
        track.video_playback.as_ref()?.serial,
    ))
}

pub(super) fn hold_position(state: &State, key: TrackKey) -> Option<BridgePositionSnapshot> {
    let track = state.tracks.get(&key)?;
    let scope = FrameScope::capture(
        track,
        state
            .surfaces
            .get(&key.surface)
            .map(|surface| surface.state.generation),
        state
            .sessions
            .get(&key.surface.session)
            .filter(|session| !session.closed)
            .map(|session| session.resume_generation),
    )?;
    scope
        .admitted()
        .then(|| playback_position(track, Instant::now()))
        .flatten()
}

// Two immutable output slots and a serialized decoded-output batch are reserved.
// An old GUI/GPU receipt keeps its slot and budget until its last reference dies.
const SLOTS: usize = 2;
const RECHECK: Duration = Duration::from_millis(10);

pub(super) struct Reservation {
    _token: Arc<()>,
    slots: Mutex<Vec<Weak<()>>>,
    work: Mutex<Weak<()>>,
    bytes: usize,
    batch_bytes: usize,
    batch_frames: usize,
    queued: usize,
    slot_capacity: usize,
}
struct Lease {
    _reservation: Arc<Reservation>,
    _slot: Arc<()>,
}
impl Reservation {
    fn work(&self) -> Option<Arc<()>> {
        let mut current = lock(&self.work);
        if current.strong_count() != 0 {
            return None;
        }
        let token = Arc::new(());
        *current = Arc::downgrade(&token);
        Some(token)
    }
    fn acquire(self: &Arc<Self>) -> Option<Arc<Lease>> {
        let mut slots = lock(&self.slots);
        slots.retain(|slot| slot.strong_count() != 0);
        if slots.len() == self.slot_capacity {
            return None;
        }
        let slot = Arc::new(());
        slots.push(Arc::downgrade(&slot));
        Some(Arc::new(Lease {
            _reservation: self.clone(),
            _slot: slot,
        }))
    }
}

#[derive(Clone)]
pub(crate) struct Frame {
    pub incarnation: Weak<()>,
    pub image: Arc<RasterImage>,
    pub epoch: u32,
    pub id: u64,
    pub pts: i64,
    pub reset: u64,
    pub play: u64,
    pub end: i64,
    pub surface: SurfaceGeneration,
    pub generation: ChannelGeneration,
    pub resume: u64,
}

// Read the complete current timeline before mutably borrowing its playback queue.
// Decoded pixels cannot satisfy startup after a channel or authenticated resume
// replacement merely because the producer reused an epoch or local frame ID.
struct FrameScope {
    generation: ChannelGeneration,
    epoch: u32,
    reset: u64,
    surface: Option<SurfaceGeneration>,
    resume: Option<u64>,
    accepted: Option<(SurfaceGeneration, ChannelGeneration)>,
    incarnation: Weak<()>,
}
impl FrameScope {
    fn capture(
        track: &TrackEntry,
        surface: Option<SurfaceGeneration>,
        resume: Option<u64>,
    ) -> Option<Self> {
        Some(Self {
            generation: track.state.channel_generation,
            epoch: track.state.media_epoch,
            reset: track.decoder_reset_serial,
            surface,
            resume,
            accepted: track.accepted_surface_generation,
            incarnation: Arc::downgrade(&track.video_reservation.as_ref()?._token),
        })
    }
    fn admitted(&self) -> bool {
        self.resume.is_some()
            && self
                .surface
                .is_some_and(|surface| self.accepted == Some((surface, self.generation)))
    }
    fn accepts(&self, frame: &Frame) -> bool {
        self.admitted()
            && frame.generation == self.generation
            && frame.epoch == self.epoch
            && frame.reset == self.reset
            && self.surface == Some(frame.surface)
            && self.resume == Some(frame.resume)
            && Weak::ptr_eq(&frame.incarnation, &self.incarnation)
    }
}
pub(super) fn owns(track: &TrackEntry, incarnation: &Weak<()>) -> bool {
    track
        .video_reservation
        .as_ref()
        .is_some_and(|r| Weak::ptr_eq(incarnation, &Arc::downgrade(&r._token)))
}

pub(super) fn supports(config: &TrackConfiguration) -> bool {
    budget(config).is_some()
}
struct Budget {
    bytes: usize,
    batch_bytes: usize,
    batch_frames: usize,
    total: usize,
    queued: usize,
}
fn budget(config: &TrackConfiguration) -> Option<Budget> {
    let KindConfiguration::Video(video) = &config.kind else {
        return None;
    };
    let queued = if config.mode == TrackMode::Timed {
        let count = config
            .maximum_rate_millihertz
            .checked_mul(config.maximum_latency_us)?
            .checked_add(999_999_999)?
            .checked_div(1_000_000_000)?
            .checked_add(1)?;
        let count = usize::try_from(count).ok()?;
        if config.maximum_rate_millihertz == 0 || count > playback::MAX_QUEUED {
            return None;
        }
        count
    } else {
        0
    };
    // Media 1.5 permits a reorder depth up to 64; EOS may release that
    // delayed output plus the current picture in one native batch.
    if video.maximum_reorder_depth > 64 {
        return None;
    }
    let bytes = output_bytes(video)?;
    let batch_frames = usize::from(video.maximum_reorder_depth).checked_add(1)?;
    let batch_bytes = bytes.checked_mul(batch_frames)?;
    if batch_bytes > crate::video::Limits::default().max_output_bytes {
        return None;
    }
    let total = bytes
        .checked_mul(SLOTS.checked_add(queued)?)?
        .checked_add(batch_bytes)?
        .checked_add(video.coded_width as usize * 4)?;
    Some(Budget {
        bytes,
        batch_bytes,
        batch_frames,
        total,
        queued,
    })
}
pub(super) fn output_bytes(video: &VideoConfiguration) -> Option<usize> {
    let bytes = (video.coded_width as usize)
        .checked_mul(video.coded_height as usize)?
        .checked_mul(4)?;
    (video.coded_width != 0
        && video.coded_height != 0
        && video.coded_width <= 8192
        && video.coded_height <= 8192
        && bytes <= crate::vector_renderer::MAX_COMPILED_BYTES)
        .then_some(bytes)
}
pub(super) fn reserve(
    state: &mut State,
    config: &TrackConfiguration,
) -> io::Result<Arc<Reservation>> {
    if !state.video_host {
        return Err(invalid("native video host is absent"));
    }
    let budget =
        budget(config).ok_or_else(|| invalid("native video reservation exceeds limits"))?;
    let token = native_vector::charge(state, budget.total)?;
    Ok(Arc::new(Reservation {
        _token: token,
        bytes: budget.bytes,
        batch_bytes: budget.batch_bytes,
        batch_frames: budget.batch_frames,
        queued: budget.queued,
        slot_capacity: SLOTS + budget.queued,
        slots: Mutex::new(Vec::with_capacity(SLOTS + budget.queued)),
        work: Mutex::new(Weak::new()),
    }))
}

pub(super) fn admissible(state: &State, config: &TrackConfiguration) -> bool {
    if !state.overlays.local_native() || !matches!(config.kind, KindConfiguration::Video(_)) {
        return true;
    }
    let Some(budget) = budget(config) else {
        return false;
    };
    let used = state
        .native_charges
        .iter()
        .filter(|(_, t)| t.strong_count() != 0)
        .try_fold(0u64, |sum, (n, _)| sum.checked_add(*n as u64));
    state.video_host
        && used
            .and_then(|n| n.checked_add(budget.total as u64))
            .is_some_and(|n| n <= state.config.media.aggregate_retained_bytes)
}
pub(super) fn ended(track: &TrackEntry) -> bool {
    #[cfg(feature = "vector-audio")]
    if track.live_video_pending.is_some() {
        return false;
    }
    if track.video_playback.is_some() {
        return track.state.milestones & MILESTONE_BUFFERED_ENDED != 0;
    }
    track.video_drained == Some(track.state.media_epoch)
        && track.native_video.as_ref().is_none_or(|f| {
            track.native_visual_presented.is_some_and(|(c, e, id)| {
                c == track.state.channel_generation && e == f.epoch && id >= f.id
            })
        })
}

// Capture under the same state lock that admits the packet or EOS. Recapturing
// on entry to the decoder would let FLUSH relabel old work with the new epoch.
#[derive(Clone)]
pub(super) struct Admission {
    key: TrackKey,
    generation: ChannelGeneration,
    writer: Arc<Writer>,
    epoch: u32,
    reset: u64,
    resume: u64,
    surface: SurfaceGeneration,
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
        let session = state.sessions.get(&key.surface.session)?;
        if !state.video_host
            || session.closed
            || track.state.lost
            || !owns_media_channel(track, generation, writer)
        {
            return None;
        }
        Some(Self {
            key,
            generation,
            writer: writer.clone(),
            epoch: track.state.media_epoch,
            reset: track.decoder_reset_serial,
            resume: session.resume_generation,
            surface: state.surfaces.get(&key.surface)?.state.generation,
            incarnation: Arc::downgrade(&track.video_reservation.as_ref()?._token),
        })
    }
    fn current(&self, state: &State) -> bool {
        Self::capture(state, self.key, self.generation, &self.writer).is_some_and(|s| {
            s.reset == self.reset
                && s.resume == self.resume
                && s.surface == self.surface
                && s.epoch == self.epoch
                && Weak::ptr_eq(&s.incarnation, &self.incarnation)
        })
    }
    #[cfg(feature = "vector-audio")]
    fn retained(&self, state: &State) -> bool {
        self.current(state)
            || (state.video_host
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
                        && t.channel_writer.is_none()
                        && t.eos_epoch == Some(self.epoch)
                        && t.video_reservation.as_ref().is_some_and(|r| {
                            Weak::ptr_eq(&self.incarnation, &Arc::downgrade(&r._token))
                        })
                }))
    }
}
struct Context {
    reset: u64,
    resume: u64,
    decoder: crate::video::Decoder,
    color: Arc<dyn moxcms::TransformExecutor<u8> + Send + Sync>,
    row: Vec<u8>,
    width: u32,
    height: u32,
    pending: Vec<(i64, i64, SurfaceGeneration)>,
}
impl Context {
    fn convert(&mut self, mut frame: crate::video::DecodedFrame) -> io::Result<RasterImage> {
        for row in frame.rgba.as_mut().chunks_exact_mut(self.row.len()) {
            self.color
                .transform(row, &mut self.row)
                .map_err(io::Error::other)?;
            row.copy_from_slice(&self.row);
        }
        RasterImage::from_decoded(frame).map_err(io::Error::other)
    }
}
pub(super) fn playback_for(reservation: &Option<Arc<Reservation>>) -> Option<Playback> {
    reservation
        .as_ref()
        .filter(|r| r.queued != 0)
        .map(|r| Playback::new(r.queued))
}
pub(super) fn clear(track: &mut TrackEntry) {
    track.native_video = None;
    track.video_drained = None;
    track.video_playback = playback_for(&track.video_reservation);
    #[cfg(feature = "vector-audio")]
    {
        track.live_video_pending = None;
        track.live_video_scope = None;
    }
}
struct TimedImage {
    pts: i64,
    end: i64,
    image: RasterImage,
}
struct TimedBatch {
    frames: Vec<TimedImage>,
    random_access: bool,
    drained: bool,
}
fn publish_timed(
    shared: &Arc<Mutex<State>>,
    changed: &Arc<Condvar>,
    scope: &Admission,
    reservation: &Arc<Reservation>,
    batch: TimedBatch,
    record: Option<&Record>,
) -> io::Result<()> {
    let mut state = lock(shared);
    if scope.current(&state) {
        let track = state
            .tracks
            .get_mut(&scope.key)
            .expect("admitted timed video");
        track.state.milestones |= MILESTONE_DECODER_INITIALIZED;
        if batch.random_access {
            track.recovery_pending = false;
            track.recovery_requested = false;
        }
    }
    for output in batch.frames {
        let mut image = Some(output.image);
        loop {
            if !scope.current(&state) {
                return credit(
                    &mut state,
                    scope.key,
                    scope.generation,
                    &scope.writer,
                    record,
                );
            }
            tick(&mut state);
            let track = state
                .tracks
                .get_mut(&scope.key)
                .expect("current timed video");
            let p = track
                .video_playback
                .as_mut()
                .expect("timed playback reservation");
            p.decoded = Some(output.pts);
            if p.ever_played() && output.pts < p.start_pts() {
                p.late = p.late.saturating_add(1);
                break;
            }
            if p.queue.len() < p.capacity
                && let Some(lease) = reservation.acquire()
            {
                let mut image = image.take().expect("unpublished timed pixels");
                image.retain_resource(lease).map_err(io::Error::other)?;
                track.native_frame_serial = track
                    .native_frame_serial
                    .checked_add(1)
                    .ok_or_else(|| invalid("native video frame IDs exhausted"))?;
                let frame = Frame {
                    incarnation: Arc::downgrade(&reservation._token),
                    image: Arc::new(image),
                    epoch: scope.epoch,
                    id: track.native_frame_serial,
                    pts: output.pts,
                    end: output.end,
                    reset: scope.reset,
                    play: p.serial,
                    surface: scope.surface,
                    generation: scope.generation,
                    resume: scope.resume,
                };
                let preview =
                    (!p.ever_played() || p.prime_allowed()) && track.native_video.is_none();
                if preview {
                    track.native_video = Some(frame.clone());
                }
                p.push(frame);
                track.state.milestones |= MILESTONE_OUTPUT_READY;
                if preview {
                    advance_projection(&mut state);
                }
                break;
            }
            if p.pre_roll_full() {
                return Err(invalid(
                    "video pre-roll exceeds its admitted rate and buffer",
                ));
            }
            state = changed
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
    if scope.current(&state) && batch.drained {
        state
            .tracks
            .get_mut(&scope.key)
            .expect("drained timed video")
            .video_drained = Some(scope.epoch);
    }
    tick(&mut state);
    credit(
        &mut state,
        scope.key,
        scope.generation,
        &scope.writer,
        record,
    )?;
    drop(state);
    changed.notify_all();
    Ok(())
}
#[derive(Default)]
pub(super) struct Decoder {
    context: Option<Context>,
}
impl Decoder {
    pub fn decode(
        &mut self,
        shared: &Arc<Mutex<State>>,
        changed: &Arc<Condvar>,
        admission: &Admission,
        record: &Record,
    ) -> io::Result<()> {
        self.process(shared, changed, admission, Some(record))
    }
    pub fn finish(
        &mut self,
        shared: &Arc<Mutex<State>>,
        changed: &Arc<Condvar>,
        admission: &Admission,
    ) -> io::Result<()> {
        self.process(shared, changed, admission, None)
    }
    fn process(
        &mut self,
        shared: &Arc<Mutex<State>>,
        changed: &Arc<Condvar>,
        scope: &Admission,
        record: Option<&Record>,
    ) -> io::Result<()> {
        let key = scope.key;
        let generation = scope.generation;
        let writer = &scope.writer;
        let (config, mode, reservation, _work, epoch, latency) = {
            let mut state = lock(shared);
            if !scope.current(&state) {
                return credit(&mut state, key, generation, writer, record);
            }
            let track = &state.tracks[&key];
            let KindConfiguration::Video(config) = &track.configuration.kind else {
                return Err(invalid("native video kind changed"));
            };
            let config = config.clone();
            let track_mode = track.configuration.mode;
            let reservation = track
                .video_reservation
                .clone()
                .ok_or_else(|| invalid("native video budget absent"))?;
            let epoch = track.state.media_epoch;
            if record.is_some() && track.eos_epoch.is_some() {
                return Err(invalid("native video after EOS"));
            }
            let latency = Duration::from_micros(track.configuration.maximum_latency_us.max(1));
            let work = loop {
                if !scope.current(&state) {
                    return credit(&mut state, key, generation, writer, record);
                }
                if let Some(work) = reservation.work() {
                    break work;
                }
                state = changed
                    .wait_timeout(state, RECHECK)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            };
            (config, track_mode, reservation, work, epoch, latency)
        };
        // No presenter lock is held across codec initialization, decode or color conversion.
        if self
            .context
            .as_ref()
            .is_none_or(|c| c.reset != scope.reset || c.resume != scope.resume)
        {
            let limits = crate::video::Limits {
                max_dimension: 8192,
                max_frame_bytes: reservation.bytes,
                max_output_bytes: reservation.batch_bytes,
                max_output_frames: reservation.batch_frames,
                threads: 1,
                recycle_buffers: false,
            };
            let decoder = crate::video::Decoder::with_limits(&config, mode, limits)?;
            let primaries: u8 = match config.color_primaries {
                1 => 1,
                2 => 5,
                3 => 6,
                4 => 9,
                _ => return Err(invalid("unsupported video primaries")),
            };
            let transfer: u8 = match config.transfer {
                1 => 1,
                2 => 13,
                _ => return Err(invalid("unsupported video transfer")),
            };
            let profile = moxcms::ColorProfile::new_from_cicp(moxcms::CicpProfile {
                color_primaries: primaries.try_into().map_err(io::Error::other)?,
                transfer_characteristics: transfer.try_into().map_err(io::Error::other)?,
                matrix_coefficients: 0u8.try_into().map_err(io::Error::other)?,
                full_range: true,
            });
            let color = profile
                .create_transform_8bit(
                    moxcms::Layout::Rgba,
                    &moxcms::ColorProfile::new_srgb(),
                    moxcms::Layout::Rgba,
                    Default::default(),
                )
                .map_err(io::Error::other)?;
            self.context = Some(Context {
                reset: scope.reset,
                resume: scope.resume,
                decoder,
                color,
                row: vec![0; config.coded_width as usize * 4],
                width: config.coded_width,
                height: config.coded_height,
                pending: Vec::with_capacity(reservation.batch_frames + 1),
            });
        }
        let context = self
            .context
            .as_mut()
            .expect("initialized native video decoder");
        let (output, random_access) = if let Some(record) = record {
            let packet = media::parse_video_packet(&record.body)?;
            let random = packet.flags & media::VIDEO_PACKET_KEY != 0;
            let pts = packet.pts_us;
            if mode == TrackMode::Timed {
                let duration = i64::try_from(packet.duration_us)
                    .map_err(|_| invalid("video duration exceeds signed time"))?;
                let end = pts
                    .checked_add(duration)
                    .filter(|_| pts != i64::MIN)
                    .ok_or_else(|| invalid("invalid timed video interval"))?;
                if context.pending.len() > reservation.batch_frames {
                    return Err(invalid("video exceeds declared reorder metadata bound"));
                }
                context.pending.push((pts, end, scope.surface));
            }
            let (frames, _) = context.decoder.push_discarding_before(packet, None)?;
            if mode == TrackMode::Live
                && config.maximum_reorder_depth == 0
                && frames.iter().any(|f| f.pts_us != pts)
            {
                return Err(invalid("live video reordered output"));
            }
            (frames, random)
        } else {
            (context.decoder.finish()?, false)
        };
        if output
            .iter()
            .any(|frame| frame.width != context.width || frame.height != context.height)
        {
            return Err(invalid("native video output differs from configuration"));
        }
        if mode == TrackMode::Timed {
            let mut frames = Vec::with_capacity(output.len());
            for frame in output {
                let index = context
                    .pending
                    .iter()
                    .position(|(pts, _, _)| *pts == frame.pts_us)
                    .ok_or_else(|| invalid("timed decoder returned an unknown PTS"))?;
                let (pts, end, surface) = context.pending.remove(index);
                if surface != scope.surface {
                    continue;
                }
                frames.push(TimedImage {
                    pts,
                    end,
                    image: context.convert(frame)?,
                });
            }
            return publish_timed(
                shared,
                changed,
                scope,
                &reservation,
                TimedBatch {
                    frames,
                    random_access,
                    drained: record.is_none(),
                },
                record,
            );
        }
        // Live playback coalesces a returned batch to its latest presentation
        // timestamp. Every picture was decoded to preserve reference state;
        // obsolete images need no color transform or GUI allocation.
        let newest = output.into_iter().max_by_key(|frame| frame.pts_us);
        let newest = newest.filter(|frame| {
            let state = lock(shared);
            scope.current(&state)
                && state.tracks[&scope.key]
                    .outer_position
                    .filter(|p| p.decoder_reset_serial == scope.reset)
                    .is_none_or(|p| frame.pts_us >= p.decoded_pts_us)
        });
        let decoded_pts = newest.as_ref().map(|frame| frame.pts_us);
        let image = if let Some(frame) = newest {
            let pts = frame.pts_us;
            let image = context.convert(frame)?;
            Some((image, pts))
        } else {
            None
        };
        let mut state = lock(shared);
        let mut image = image;
        if image.is_some() && scope.current(&state) {
            tick(&mut state);
            #[cfg(feature = "vector-audio")]
            {
                state
                    .tracks
                    .get_mut(&key)
                    .expect("current live video")
                    .live_video_pending = None;
            }
        }
        let waiting = Instant::now();
        let image = loop {
            if !scope.current(&state) {
                break None;
            }
            let Some((mut pixels, pts)) = image.take() else {
                break None;
            };
            if let Some(lease) = reservation.acquire() {
                pixels.retain_resource(lease).map_err(io::Error::other)?;
                break Some((Arc::new(pixels), pts));
            }
            // Reference state has already advanced. Discard an obsolete output
            // without retagging it or holding control behind a display wait.
            if waiting.elapsed() >= latency {
                break None;
            }
            image = Some((pixels, pts));
            state = changed
                .wait_timeout(
                    state,
                    RECHECK.min(latency.saturating_sub(waiting.elapsed())),
                )
                .unwrap_or_else(|p| p.into_inner())
                .0;
        };
        if scope.current(&state) {
            let track = state
                .tracks
                .get_mut(&key)
                .expect("current native video track");
            track.state.milestones |= MILESTONE_DECODER_INITIALIZED;
            track.video_drained = record.is_none().then_some(epoch);
            if let Some(pts) = decoded_pts {
                let old = track
                    .outer_position
                    .filter(|p| p.decoder_reset_serial == scope.reset);
                track.outer_position = Some(BridgePositionSnapshot {
                    decoder_reset_serial: scope.reset,
                    playing: true,
                    start_pts_us: 0,
                    state: old.map_or(1, |p| p.state),
                    clock_pts_us: None,
                    decoded_pts_us: pts,
                    presented_pts_us: old.map_or(0, |p| p.presented_pts_us),
                    presentation_id: old.map_or(0, |p| p.presentation_id),
                });
            }
            if random_access {
                track.recovery_pending = false;
                track.recovery_requested = false;
            }
            if let Some((image, pts)) = image {
                track.native_frame_serial = track
                    .native_frame_serial
                    .checked_add(1)
                    .ok_or_else(|| invalid("native video frame IDs exhausted"))?;
                let frame = Frame {
                    incarnation: Arc::downgrade(&reservation._token),
                    image,
                    pts,
                    epoch,
                    id: track.native_frame_serial,
                    reset: scope.reset,
                    play: 0,
                    end: pts,
                    surface: scope.surface,
                    generation: scope.generation,
                    resume: scope.resume,
                };
                #[cfg(feature = "vector-audio")]
                let visible = live::publish(&mut state, scope, frame);
                #[cfg(not(feature = "vector-audio"))]
                let visible = {
                    track.native_video = Some(frame);
                    true
                };
                let track = state.tracks.get_mut(&key).expect("current live video");
                track.state.milestones |= MILESTONE_OUTPUT_READY;
                if visible {
                    advance_projection(&mut state);
                }
            }
            let track = state
                .tracks
                .get_mut(&key)
                .expect("current native video track");
            if ended(track) {
                track.state.milestones |= MILESTONE_BUFFERED_ENDED;
            }
            state.wake_waits();
        }
        #[cfg(feature = "vector-audio")]
        live::tick(&mut state);
        credit(&mut state, key, generation, writer, record)?;
        drop(state);
        changed.notify_all();
        Ok(())
    }
}
fn credit(
    state: &mut State,
    key: TrackKey,
    generation: ChannelGeneration,
    writer: &Arc<Writer>,
    record: Option<&Record>,
) -> io::Result<()> {
    if let Some(record) = record
        && let Some(track) = state
            .tracks
            .get_mut(&key)
            .filter(|t| owns_media_channel(t, generation, writer))
    {
        let bytes = track
            .state
            .flow
            .maximum_body_bytes
            .checked_add(record.body.len() as u64)
            .ok_or_else(|| invalid("video byte credit exhausted"))?;
        let records = track
            .state
            .flow
            .maximum_media_records
            .checked_add(1)
            .ok_or_else(|| invalid("video record credit exhausted"))?;
        track.state.flow.raise_maxima(bytes, records);
        send_flow_update(key, track);
    }
    Ok(())
}
