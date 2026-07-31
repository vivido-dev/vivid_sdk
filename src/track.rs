//! Immutable tracks and their readiness model.
//!
//! A track's configuration never changes; a new codec is a new track. Milestones are reported per
//! channel generation, so a caller must never carry a bit across an advance.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, Weak};

use vivid_protocol::cbor::Value;
use vivid_protocol::messages;
use vivid_protocol::messages::{PayloadMap, TrackKind};
use vivid_protocol::revision::{ChannelGeneration, TrackRevision};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration, TrackMode};

use crate::*;

pub(crate) type TrackRegistry = HashMap<(u64, u64, u64), Arc<Mutex<TrackLocal>>>;

/// An immutable, owner-qualified track handle.
#[derive(Clone)]
pub struct Track {
    pub(crate) inner: Arc<Mutex<TrackLocal>>,
}

#[derive(Clone)]
pub(crate) struct TrackLocal {
    pub(crate) configuration: TrackConfiguration,
    pub(crate) revision: TrackRevision,
    pub(crate) channel_generation: ChannelGeneration,
    pub(crate) open_deadline_us: u64,
    pub(crate) maximum_record_body: u32,
    pub(crate) effective_claims: PayloadMap,
    pub(crate) connection_required: bool,
    pub(crate) delta_operation_limit: u32,
    pub(crate) media_sequence: Arc<Mutex<TrackMediaSequence>>,
    pub(crate) active_flow: Option<Weak<FlowSync>>,
    pub(crate) active_media: Option<Weak<Mutex<ChannelMediaState>>>,
    pub(crate) destroyed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TrackMediaSequence {
    pub(crate) last_id: u64,
    pub(crate) last_epoch: u32,
    pub(crate) last_record_sequence: u64,
}

impl TrackMediaSequence {
    pub(crate) fn accept(&mut self, id: u64, epoch: u32) -> io::Result<()> {
        if id == 0 || id <= self.last_id {
            return Err(invalid_input(
                "media ID is zero or not strictly increasing across track generations",
            ));
        }
        if epoch < self.last_epoch {
            return Err(invalid_input("media epoch moved backward"));
        }
        self.last_id = id;
        self.last_epoch = epoch;
        Ok(())
    }

    pub(crate) fn reconcile_status(&mut self, status: &TrackStatus, generation_changed: bool) {
        // TRACK_STATUS travels on control while media travels on an independently ordered track
        // connection. The presenter's accepted snapshot may therefore lag records already
        // submitted by this producer. Preserve the producer's track-wide monotonic sequence and
        // merge only progress that the presenter reports ahead of it.
        self.last_id = self.last_id.max(status.last_media_id);
        self.last_epoch = self.last_epoch.max(status.media_epoch);
        self.last_record_sequence = if generation_changed {
            status.last_media_record_sequence
        } else {
            self.last_record_sequence
                .max(status.last_media_record_sequence)
        };
    }
}

impl std::fmt::Debug for Track {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.lock() {
            Ok(state) => formatter
                .debug_struct("Track")
                .field("context_id", &state.configuration.context_id)
                .field("surface_id", &state.configuration.surface_id)
                .field("track_id", &state.configuration.track_id)
                .field("kind", &state.configuration.kind.kind())
                .field("revision", &state.revision)
                .field("channel_generation", &state.channel_generation)
                .field("destroyed", &state.destroyed)
                .finish(),
            Err(_) => formatter.write_str("Track(<poisoned>)"),
        }
    }
}

impl Track {
    pub fn context_id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.configuration.context_id)
    }

    pub fn surface_id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.configuration.surface_id)
    }

    pub fn id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.configuration.track_id)
    }

    pub fn kind(&self) -> TrackKind {
        self.inner
            .lock()
            .map_or(TrackKind::Video, |state| state.configuration.kind.kind())
    }

    pub fn revision(&self) -> TrackRevision {
        self.inner
            .lock()
            .map_or(TrackRevision::ZERO, |state| state.revision)
    }

    pub fn channel_generation(&self) -> ChannelGeneration {
        self.inner
            .lock()
            .map_or(ChannelGeneration::ZERO, |state| state.channel_generation)
    }

    pub fn configuration(&self) -> io::Result<TrackConfiguration> {
        Ok(lock(&self.inner, "track")?.configuration.clone())
    }

    pub fn effective_claims(&self) -> io::Result<PayloadMap> {
        Ok(lock(&self.inner, "track")?.effective_claims.clone())
    }

    pub fn channel_open_deadline_us(&self) -> io::Result<u64> {
        Ok(lock(&self.inner, "track")?.open_deadline_us)
    }

    pub fn connection_required(&self) -> io::Result<bool> {
        Ok(lock(&self.inner, "track")?.connection_required)
    }

    /// Effective raster delta operation limit granted by `TRACK_READY`.
    ///
    /// Zero means this track accepts no delta frames, either because it is not a raster track or
    /// because the presenter granted less than the requested configuration. A producer plans its
    /// delta operations against this value, not against the limit it asked for.
    pub fn delta_operation_limit(&self) -> io::Result<u32> {
        Ok(lock(&self.inner, "track")?.delta_operation_limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSupport {
    pub supported: bool,
    pub selected_decoder: String,
    pub capability_generation: u64,
    pub effective_claims: PayloadMap,
}

/// Authoritative state returned by `QUERY_TRACK`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackStatus {
    pub context_id: u64,
    pub surface_id: u64,
    pub track_id: u64,
    pub revision: TrackRevision,
    pub kind: TrackKind,
    pub mode: TrackMode,
    pub lifecycle: u64,
    pub channel_generation: ChannelGeneration,
    pub attachment_state: u64,
    pub milestones: u64,
    pub media_epoch: u32,
    pub last_media_id: u64,
    pub last_media_record_sequence: u64,
    pub last_decoded_pts_us: i64,
    pub last_presented_pts_us: i64,
    pub last_presentation_id: u64,
    pub cumulative_body_bytes: u64,
    pub cumulative_media_records: u64,
    pub maximum_body_bytes: u64,
    pub maximum_media_records: u64,
    pub ingress_depth_bucket: u64,
    pub playback_state: Option<PayloadMap>,
    pub terminal_loss_code: Option<u64>,
}

/// One bounded `WAIT_TRACK` condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum TrackWaitCondition {
    RevisionGreater = 1,
    MilestoneSet = 2,
    RasterFramePresented = 3,
    VideoPtsPresented = 4,
    PlaybackStarted = 5,
    PlaybackEnded = 6,
    ChannelAccepted = 7,
    ChannelClosed = 8,
    TrackLost = 9,
}

impl TrackWaitCondition {
    pub(crate) fn validate_value(self, value: Option<u64>) -> io::Result<()> {
        match self {
            Self::RevisionGreater
            | Self::MilestoneSet
            | Self::RasterFramePresented
            | Self::VideoPtsPresented
                if value.is_none() =>
            {
                Err(invalid_input(
                    "selected track wait condition requires a value",
                ))
            }
            Self::PlaybackStarted
            | Self::PlaybackEnded
            | Self::ChannelAccepted
            | Self::ChannelClosed
            | Self::TrackLost
                if value.is_some() =>
            {
                Err(invalid_input(
                    "selected track wait condition does not accept a value",
                ))
            }
            Self::MilestoneSet
                if value == Some(0)
                    || value.is_some_and(|value| value & !MILESTONE_KNOWN_MASK != 0) =>
            {
                Err(invalid_input("track wait milestone mask is invalid"))
            }
            _ => Ok(()),
        }
    }
}

impl TryFrom<u64> for TrackWaitCondition {
    type Error = io::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::RevisionGreater),
            2 => Ok(Self::MilestoneSet),
            3 => Ok(Self::RasterFramePresented),
            4 => Ok(Self::VideoPtsPresented),
            5 => Ok(Self::PlaybackStarted),
            6 => Ok(Self::PlaybackEnded),
            7 => Ok(Self::ChannelAccepted),
            8 => Ok(Self::ChannelClosed),
            9 => Ok(Self::TrackLost),
            _ => Err(invalid_input("unknown track wait condition")),
        }
    }
}

/// Positive result from one bounded `WAIT_TRACK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackWaitSatisfied {
    pub context_id: u64,
    pub surface_id: u64,
    pub track_id: u64,
    pub revision: TrackRevision,
    pub channel_generation: ChannelGeneration,
    pub condition: TrackWaitCondition,
    pub observed_value: Option<u64>,
}

impl Session {
    pub fn probe_track(&mut self, configuration: &TrackConfiguration) -> io::Result<TrackSupport> {
        configuration.validate(true)?;
        let reply = self.request(
            messages::PROBE_TRACK_CONFIG,
            0,
            configuration.payload(true)?,
            &RequestMetadata::default(),
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::TRACK_SUPPORT, 0)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("TRACK_SUPPORT", &payload, 0..=3)?;
            Ok(TrackSupport {
                supported: required_bool(&payload, 0)?,
                selected_decoder: required_text(&payload, 1)?.to_owned(),
                capability_generation: required_u64(&payload, 2)?,
                effective_claims: required_map(&payload, 3)?.to_vec(),
            })
        } else {
            Ok(TrackSupport {
                supported: true,
                selected_decoder: "offline-validator".into(),
                capability_generation: 1,
                effective_claims: configuration.payload(true)?,
            })
        }
    }

    pub fn create_track(
        &mut self,
        configuration: TrackConfiguration,
        metadata: &RequestMetadata,
    ) -> io::Result<Track> {
        configuration.validate(false)?;
        if !self
            .surfaces
            .contains_key(&(configuration.context_id, configuration.surface_id))
        {
            return Err(invalid_input(
                "track references a surface not owned by this SDK session",
            ));
        }
        let key = (
            configuration.context_id,
            configuration.surface_id,
            configuration.track_id,
        );
        let tracks = lock(&self.tracks, "track registry")?;
        if tracks.contains_key(&key) {
            return Err(invalid_input("track identity is already live"));
        }
        drop(tracks);
        let reply = self.request(
            messages::CREATE_TRACK,
            configuration.track_id,
            configuration.payload(false)?,
            metadata,
            None,
            None,
        )?;
        let ready = if let Some(record) = reply {
            expect_record(&record, messages::TRACK_READY, configuration.track_id)?;
            let payload = decoded_payload(&record)?;
            validate_payload_keys("TRACK_READY", &payload, 0..=8, &[9])?;
            validate_track_owner(&payload, &configuration)?;
            TrackReadyValues {
                revision: TrackRevision::new(required_u64(&payload, 3)?),
                generation: ChannelGeneration::new(required_u64(&payload, 4)?),
                open_deadline_us: required_u64(&payload, 5)?,
                maximum_record_body: required_u32(&payload, 6)?,
                effective_claims: required_map(&payload, 7)?.to_vec(),
                connection_required: required_bool(&payload, 8)?,
                delta_operation_limit: optional_u64(&payload, 9)?
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| invalid_data("delta operation limit exceeds u32"))?
                    .unwrap_or(0),
            }
        } else {
            TrackReadyValues {
                revision: TrackRevision::ONE,
                generation: ChannelGeneration::ONE,
                open_deadline_us: 30_000_000,
                maximum_record_body: configuration.maximum_record_body,
                effective_claims: configuration.payload(false)?,
                connection_required: true,
                delta_operation_limit: match &configuration.kind {
                    KindConfiguration::Raster(value) if value.delta_enabled => {
                        u32::from(value.maximum_delta_operations)
                    }
                    _ => 0,
                },
            }
        };
        ready.revision.require_nonzero()?;
        ready.generation.require_nonzero()?;
        if ready.generation != ChannelGeneration::ONE
            || ready.open_deadline_us == 0
            || ready.open_deadline_us > 30_000_000
            || ready.maximum_record_body == 0
            || ready.maximum_record_body > configuration.maximum_record_body
            || (!ready.connection_required
                && !matches!(
                    &configuration.kind,
                    KindConfiguration::EncodedImage(image) if image.cache_lookup
                ))
        {
            return Err(invalid_data(
                "TRACK_READY returned invalid initial channel state",
            ));
        }
        let inner = Arc::new(Mutex::new(TrackLocal {
            configuration,
            revision: ready.revision,
            channel_generation: ready.generation,
            open_deadline_us: ready.open_deadline_us,
            maximum_record_body: ready.maximum_record_body,
            effective_claims: ready.effective_claims,
            connection_required: ready.connection_required,
            delta_operation_limit: ready.delta_operation_limit,
            media_sequence: Arc::new(Mutex::new(TrackMediaSequence::default())),
            active_flow: None,
            active_media: None,
            destroyed: false,
        }));
        lock(&self.tracks, "track registry")?.insert(key, inner.clone());
        Ok(Track { inner })
    }

    pub fn query_track(&self, track: &Track) -> io::Result<TrackStatus> {
        let snapshot = lock(&track.inner, "track")?.clone();
        let reply = self.request(
            messages::QUERY_TRACK,
            snapshot.configuration.track_id,
            vec![
                (0, Value::Unsigned(snapshot.configuration.context_id)),
                (1, Value::Unsigned(snapshot.configuration.surface_id)),
                (2, Value::Unsigned(snapshot.configuration.track_id)),
            ],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let status = if let Some(record) = reply {
            expect_record(
                &record,
                messages::TRACK_STATUS,
                snapshot.configuration.track_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_payload_keys("TRACK_STATUS", &payload, 0..=20, &[21, 22])?;
            validate_track_tuple(&payload, &snapshot.configuration)?;
            let kind = TrackKind::try_from(required_u64(&payload, 4)?).map_err(io::Error::other)?;
            let mode = TrackMode::try_from(required_u64(&payload, 5)?).map_err(io::Error::other)?;
            let lifecycle = required_u64(&payload, 6)?;
            let attachment_state = required_u64(&payload, 8)?;
            let milestones = required_u64(&payload, 9)?;
            if lifecycle > 7 || attachment_state > 2 || milestones & !MILESTONE_KNOWN_MASK != 0 {
                return Err(invalid_data("TRACK_STATUS contains an unknown state bit"));
            }
            TrackStatus {
                context_id: required_u64(&payload, 0)?,
                surface_id: required_u64(&payload, 1)?,
                track_id: required_u64(&payload, 2)?,
                revision: TrackRevision::new(required_u64(&payload, 3)?),
                kind,
                mode,
                lifecycle,
                channel_generation: ChannelGeneration::new(required_u64(&payload, 7)?),
                attachment_state,
                milestones,
                media_epoch: required_u32(&payload, 10)?,
                last_media_id: required_u64(&payload, 11)?,
                last_media_record_sequence: required_u64(&payload, 12)?,
                last_decoded_pts_us: required_i64(&payload, 13)?,
                last_presented_pts_us: required_i64(&payload, 14)?,
                last_presentation_id: required_u64(&payload, 15)?,
                cumulative_body_bytes: required_u64(&payload, 16)?,
                cumulative_media_records: required_u64(&payload, 17)?,
                maximum_body_bytes: required_u64(&payload, 18)?,
                maximum_media_records: required_u64(&payload, 19)?,
                ingress_depth_bucket: required_u64(&payload, 20)?,
                playback_state: optional_map(&payload, 21)?.cloned(),
                terminal_loss_code: optional_u64(&payload, 22)?,
            }
        } else {
            let media = *lock(&snapshot.media_sequence, "track media sequence")?;
            let (lifecycle, attachment_state, milestones, flow) =
                if let Some(flow) = snapshot.active_flow.as_ref().and_then(Weak::upgrade) {
                    let state = lock(&flow.state, "channel flow state")?;
                    if state.closed {
                        (4, 2, MILESTONE_CHANNEL_DETACHED, Some(state.flow))
                    } else {
                        (1, 1, MILESTONE_CHANNEL_ACCEPTED, Some(state.flow))
                    }
                } else if snapshot.destroyed {
                    (7, 2, 0, None)
                } else {
                    (0, 0, 0, None)
                };
            let flow = flow.unwrap_or_default();
            TrackStatus {
                context_id: snapshot.configuration.context_id,
                surface_id: snapshot.configuration.surface_id,
                track_id: snapshot.configuration.track_id,
                revision: snapshot.revision,
                kind: snapshot.configuration.kind.kind(),
                mode: snapshot.configuration.mode,
                lifecycle,
                channel_generation: snapshot.channel_generation,
                attachment_state,
                milestones,
                media_epoch: media.last_epoch,
                last_media_id: media.last_id,
                last_media_record_sequence: media.last_record_sequence,
                last_decoded_pts_us: 0,
                last_presented_pts_us: 0,
                last_presentation_id: 0,
                cumulative_body_bytes: flow.sent_body_bytes,
                cumulative_media_records: flow.sent_media_records,
                maximum_body_bytes: flow.maximum_body_bytes,
                maximum_media_records: flow.maximum_media_records,
                ingress_depth_bucket: 0,
                playback_state: None,
                terminal_loss_code: None,
            }
        };
        status.revision.require_nonzero()?;
        status.channel_generation.require_nonzero()?;
        if status.kind != snapshot.configuration.kind.kind()
            || status.mode != snapshot.configuration.mode
            || status.cumulative_body_bytes > status.maximum_body_bytes
            || status.cumulative_media_records > status.maximum_media_records
        {
            return Err(invalid_data(
                "TRACK_STATUS changed immutable state or contains invalid flow progress",
            ));
        }

        let mut state = lock(&track.inner, "track")?;
        let generation_changed = status.channel_generation != state.channel_generation;
        if generation_changed {
            let active_flow = state.active_flow.take();
            state.active_media = None;
            close_track_flow(
                active_flow.as_ref(),
                "TRACK_STATUS reconciled a different channel generation",
            );
        }
        if matches!(status.lifecycle, 6 | 7) {
            let active_flow = state.active_flow.take();
            state.active_media = None;
            close_track_flow(
                active_flow.as_ref(),
                if status.lifecycle == 6 {
                    "TRACK_STATUS reported a lost track"
                } else {
                    "TRACK_STATUS reported a track tombstone"
                },
            );
        }
        state.revision = status.revision;
        state.channel_generation = status.channel_generation;
        state.destroyed = matches!(status.lifecycle, 6 | 7);
        let mut sequence = lock(&state.media_sequence, "track media sequence")?;
        sequence.reconcile_status(&status, generation_changed);
        Ok(status)
    }

    pub fn wait_track(
        &self,
        track: &Track,
        condition: TrackWaitCondition,
        value: Option<u64>,
        timeout_us: u64,
    ) -> io::Result<TrackWaitSatisfied> {
        condition.validate_value(value)?;
        if timeout_us == 0 || timeout_us > vivid_protocol::MAX_TRACK_WAIT_TIMEOUT_US {
            return Err(invalid_input(
                "track wait timeout must be within 1..=30000000 microseconds",
            ));
        }
        let snapshot = lock(&track.inner, "track")?.clone();
        ensure_live_track(&snapshot)?;
        let mut payload = vec![
            (0, Value::Unsigned(snapshot.configuration.context_id)),
            (1, Value::Unsigned(snapshot.configuration.surface_id)),
            (2, Value::Unsigned(snapshot.configuration.track_id)),
            (3, Value::Unsigned(condition as u64)),
        ];
        if let Some(value) = value {
            payload.push((4, Value::Unsigned(value)));
        }
        payload.push((5, Value::Unsigned(timeout_us)));
        payload.push((6, Value::Unsigned(snapshot.channel_generation.get())));
        let reply = self.request(
            messages::WAIT_TRACK,
            snapshot.configuration.track_id,
            payload,
            &RequestMetadata::default(),
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(
                &record,
                messages::WAIT_SATISFIED,
                snapshot.configuration.track_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_payload_keys("WAIT_SATISFIED", &payload, 0..=5, &[6])?;
            validate_track_tuple(&payload, &snapshot.configuration)?;
            if required_u64(&payload, 4)? != snapshot.channel_generation.get()
                || required_u64(&payload, 5)? != condition as u64
            {
                return Err(invalid_data(
                    "WAIT_SATISFIED returned a stale generation or condition",
                ));
            }
            let revision = TrackRevision::new(required_u64(&payload, 3)?);
            let channel_generation = ChannelGeneration::new(required_u64(&payload, 4)?);
            revision.require_nonzero()?;
            channel_generation.require_nonzero()?;
            Ok(TrackWaitSatisfied {
                context_id: required_u64(&payload, 0)?,
                surface_id: required_u64(&payload, 1)?,
                track_id: required_u64(&payload, 2)?,
                revision,
                channel_generation,
                condition,
                observed_value: optional_u64(&payload, 6)?,
            })
        } else {
            Ok(TrackWaitSatisfied {
                context_id: snapshot.configuration.context_id,
                surface_id: snapshot.configuration.surface_id,
                track_id: snapshot.configuration.track_id,
                revision: snapshot.revision,
                channel_generation: snapshot.channel_generation,
                condition,
                observed_value: value,
            })
        }
    }

    pub fn destroy_track(&mut self, track: &Track, metadata: &RequestMetadata) -> io::Result<()> {
        let snapshot = lock(&track.inner, "track")?.clone();
        ensure_live_track(&snapshot)?;
        self.request_ok(
            messages::DESTROY_TRACK,
            snapshot.configuration.track_id,
            vec![
                (0, Value::Unsigned(snapshot.configuration.context_id)),
                (1, Value::Unsigned(snapshot.configuration.surface_id)),
                (2, Value::Unsigned(snapshot.configuration.track_id)),
            ],
            metadata,
        )?;
        let mut state = lock(&track.inner, "track")?;
        state.destroyed = true;
        let active_flow = state.active_flow.take();
        state.active_media = None;
        drop(state);
        close_track_flow(active_flow.as_ref(), "track destroyed");
        lock(&self.tracks, "track registry")?.remove(&(
            snapshot.configuration.context_id,
            snapshot.configuration.surface_id,
            snapshot.configuration.track_id,
        ));
        Ok(())
    }

    pub fn play(
        &mut self,
        track: &Track,
        start_pts_us: i64,
        minimum_buffer_us: u64,
        maximum_latency_us: u64,
    ) -> io::Result<()> {
        if !self.supports(TIMED_MEDIA) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "timed-media-v1 was not accepted",
            ));
        }
        let state = lock(&track.inner, "track")?.clone();
        ensure_live_track(&state)?;
        if state.configuration.mode != TrackMode::Timed {
            return Err(invalid_input("PLAY requires a timed track"));
        }
        self.request_ok(
            messages::PLAY,
            state.configuration.track_id,
            vec![
                (0, Value::Unsigned(state.configuration.context_id)),
                (1, Value::Unsigned(state.configuration.surface_id)),
                (2, Value::Unsigned(state.configuration.track_id)),
                (3, signed(start_pts_us)),
                (4, Value::Unsigned(minimum_buffer_us)),
                (5, Value::Unsigned(maximum_latency_us)),
                (6, signed(1_i64 << 32)),
                (7, Value::Unsigned(1)),
                (8, Value::Unsigned(0)),
                (9, Value::Unsigned(1)),
                (10, Value::Unsigned(state.channel_generation.get())),
            ],
            &RequestMetadata::default(),
        )
    }

    pub fn pause(&mut self, track: &Track) -> io::Result<()> {
        self.track_control(messages::PAUSE, track, vec![])
    }

    pub fn flush(&mut self, track: &Track, new_epoch: u32) -> io::Result<()> {
        let snapshot = lock(&track.inner, "track")?.clone();
        let media_sequence = snapshot.media_sequence.clone();
        if new_epoch <= lock(&media_sequence, "track media sequence")?.last_epoch {
            return Err(invalid_input(
                "FLUSH epoch must be greater than the current media epoch",
            ));
        }
        self.track_control(
            messages::FLUSH,
            track,
            vec![(3, Value::Unsigned(u64::from(new_epoch)))],
        )?;
        lock(&media_sequence, "track media sequence")?.last_epoch = new_epoch;
        if let Some(media) = snapshot.active_media.and_then(|media| media.upgrade()) {
            let mut media = lock(&media, "channel media state")?;
            media.needs_recovery = true;
            media.minimum_recovery_epoch = new_epoch;
        }
        Ok(())
    }

    pub fn drain(&mut self, track: &Track) -> io::Result<()> {
        self.track_control(messages::DRAIN, track, vec![])
    }

    pub(crate) fn track_control(
        &mut self,
        record_type: u16,
        track: &Track,
        mut suffix: PayloadMap,
    ) -> io::Result<()> {
        let state = lock(&track.inner, "track")?.clone();
        ensure_live_track(&state)?;
        let mut payload = vec![
            (0, Value::Unsigned(state.configuration.context_id)),
            (1, Value::Unsigned(state.configuration.surface_id)),
            (2, Value::Unsigned(state.configuration.track_id)),
        ];
        payload.append(&mut suffix);
        self.request_ok(
            record_type,
            state.configuration.track_id,
            payload,
            &RequestMetadata::default(),
        )
    }
}
