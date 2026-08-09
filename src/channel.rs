//! Authenticated track-channel generations: the flow authority for media bytes.
//!
//! One [`TrackChannel`] is one accepted transport generation for one track. It owns the absolute
//! cumulative flow window, the sustained-rate bucket, and the media sequence discipline that
//! survives a channel advance.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};
use std::{io, thread};

use vivid_protocol::cbor::Value;
use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::messages::{ChannelOpen, Envelope, TrackKind};
use vivid_protocol::resource::{ChannelFlow, ResourceError, TokenBucket};
use vivid_protocol::revision::{ChannelGeneration, TrackRevision};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration};
use vivid_protocol::wire::{Connection, ConnectionReader, ConnectionWriter};
use vivid_protocol::{auth, media, messages};

use crate::*;

pub(crate) struct FlowSync {
    pub(crate) state: Mutex<FlowLocal>,
    pub(crate) changed: Condvar,
}

pub(crate) struct FlowLocal {
    pub(crate) flow: ChannelFlow,
    pub(crate) initial_maximum_body_bytes: u64,
    pub(crate) initial_maximum_media_records: u64,
    pub(crate) closed: bool,
    pub(crate) diagnostic: Option<String>,
}

pub(crate) struct ChannelMediaState {
    pub(crate) last_sequence: u64,
    pub(crate) needs_recovery: bool,
    pub(crate) minimum_recovery_epoch: u32,
    pub(crate) image_sent: bool,
    pub(crate) eos: bool,
}

pub(crate) struct ChannelRateState {
    pub(crate) body_bytes: TokenBucket,
    pub(crate) records: TokenBucket,
    pub(crate) updated_at: Instant,
}

/// Where a media send actually spent its time.
///
/// A producer adapting to a slow session has to know *which* limit it hit, because the three have
/// opposite answers. Waiting on the declared-rate limiter is self-imposed pacing and means nothing
/// is wrong. Waiting for channel-flow capacity means the presenter is behind on *records*, which
/// fewer, larger frames would not help. Only time inside the transport write says the link itself
/// will not take the bytes, which is the one case where encoding at a lower rate is the answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendPressure {
    /// Held back by this producer's own `maximum_encoded_bits_per_second`/records-per-second claim.
    pub rate_limited: Duration,
    /// Waiting for the presenter to return channel-flow capacity.
    pub flow_limited: Duration,
    /// Inside the transport write.
    pub transport: Duration,
    /// Media records that completed while this pressure accumulated.
    pub records: u64,
}

impl SendPressure {
    fn accumulate(&mut self, other: Self) {
        self.rate_limited = self.rate_limited.saturating_add(other.rate_limited);
        self.flow_limited = self.flow_limited.saturating_add(other.flow_limited);
        self.transport = self.transport.saturating_add(other.transport);
        self.records = self.records.saturating_add(other.records);
    }
}

/// One accepted, authenticated track-channel generation.
#[derive(Clone)]
pub struct TrackChannel {
    pub(crate) track: Track,
    pub(crate) generation: ChannelGeneration,
    pub(crate) writer: ConnectionWriter,
    pub(crate) lifecycle: Arc<SessionLifecycle>,
    pub(crate) flow: Arc<FlowSync>,
    pub(crate) track_sequence: Arc<Mutex<TrackMediaSequence>>,
    pub(crate) media: Arc<Mutex<ChannelMediaState>>,
    pub(crate) events: Arc<Mutex<VecDeque<ChannelEvent>>>,
    pub(crate) rate: Option<Arc<Mutex<ChannelRateState>>>,
    pub(crate) pressure: Arc<Mutex<SendPressure>>,
}

impl std::fmt::Debug for TrackChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrackChannel")
            .field("track", &self.track)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl TrackChannel {
    pub(crate) fn establish(
        mut connection: Connection,
        open_body: Vec<u8>,
        track: Track,
        lifecycle: Arc<SessionLifecycle>,
        offline: bool,
    ) -> io::Result<Self> {
        let snapshot = lock(&track.inner, "track")?.clone();
        connection.write_record(
            messages::CHANNEL_OPEN,
            0,
            snapshot.configuration.track_id,
            &open_body,
        )?;
        let (maximum_bytes, maximum_records, maximum_body, revision, reader) = if offline {
            let bytes = snapshot
                .configuration
                .maximum_inflight_body_bytes
                .max(u64::from(snapshot.maximum_record_body))
                .max(OFFLINE_FLOW_BYTES);
            (
                bytes,
                OFFLINE_FLOW_RECORDS,
                snapshot.maximum_record_body,
                snapshot.revision.advance()?,
                None,
            )
        } else {
            let reply = connection.read_record()?;
            if reply.record_type == messages::ERROR {
                return Err(presenter_error(&reply.body)?);
            }
            expect_record(
                &reply,
                messages::CHANNEL_ACCEPTED,
                snapshot.configuration.track_id,
            )?;
            let accepted = messages::decode_control(&reply.body)?;
            if accepted.request_id != 1 {
                return Err(invalid_data(
                    "CHANNEL_ACCEPTED request ID does not match CHANNEL_OPEN",
                ));
            }
            let payload = accepted.payload;
            validate_exact_payload_keys("CHANNEL_ACCEPTED", &payload, 0..=7)?;
            validate_track_tuple(&payload, &snapshot.configuration)?;
            let accepted_generation = ChannelGeneration::new(required_u64(&payload, 3)?);
            if accepted_generation != snapshot.channel_generation {
                return Err(invalid_data("CHANNEL_ACCEPTED returned a stale generation"));
            }
            (
                required_u64(&payload, 4)?,
                required_u64(&payload, 5)?,
                required_u32(&payload, 6)?,
                TrackRevision::new(required_u64(&payload, 7)?),
                Some(()),
            )
        };
        if maximum_bytes < u64::from(maximum_body)
            || maximum_records == 0
            || maximum_body == 0
            || maximum_body > snapshot.maximum_record_body
        {
            return Err(invalid_data(
                "CHANNEL_ACCEPTED returned unusable flow maxima",
            ));
        }
        connection.set_send_body_limit(maximum_body)?;
        if !offline {
            connection.set_receive_body_limit(64 * 1024)?;
        }
        let (reader, writer) = if reader.is_some() {
            let (reader, writer) = connection.split()?;
            (Some(reader), writer)
        } else {
            (None, connection.writer())
        };
        {
            let mut state = lock(&track.inner, "track")?;
            state.revision = revision;
        }
        let flow = Arc::new(FlowSync {
            state: Mutex::new(FlowLocal {
                flow: ChannelFlow::new(maximum_bytes, maximum_records),
                initial_maximum_body_bytes: maximum_bytes,
                initial_maximum_media_records: maximum_records,
                closed: false,
                diagnostic: None,
            }),
            changed: Condvar::new(),
        });
        lifecycle.register_track_flow(&flow)?;
        let media = Arc::new(Mutex::new(ChannelMediaState {
            last_sequence: 0,
            needs_recovery: true,
            minimum_recovery_epoch: lock(&snapshot.media_sequence, "track media sequence")?
                .last_epoch,
            image_sent: false,
            eos: false,
        }));
        {
            let mut state = lock(&track.inner, "track")?;
            if state.channel_generation != snapshot.channel_generation {
                return Err(invalid_data(
                    "track generation changed while its channel was opening",
                ));
            }
            if state
                .active_flow
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|active| active.state.lock().is_ok_and(|active| !active.closed))
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "track already has a live channel for this generation",
                ));
            }
            state.active_flow = Some(Arc::downgrade(&flow));
            state.active_media = Some(Arc::downgrade(&media));
        }
        let events = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(reader) = reader {
            spawn_channel_reader(
                reader,
                snapshot.configuration.clone(),
                snapshot.channel_generation,
                flow.clone(),
                media.clone(),
                events.clone(),
            )?;
        }
        Ok(Self {
            track,
            generation: snapshot.channel_generation,
            writer,
            lifecycle,
            flow,
            track_sequence: snapshot.media_sequence,
            media,
            events,
            rate: (!offline).then(|| {
                let byte_rate = snapshot
                    .configuration
                    .maximum_encoded_bits_per_second
                    .saturating_add(7)
                    / 8;
                Arc::new(Mutex::new(ChannelRateState {
                    body_bytes: TokenBucket::new(
                        byte_rate,
                        u64::from(snapshot.maximum_record_body),
                    ),
                    records: TokenBucket::new(snapshot.configuration.maximum_records_per_second, 1),
                    updated_at: Instant::now(),
                }))
            }),
            pressure: Arc::new(Mutex::new(SendPressure::default())),
        })
    }

    /// Take and reset the accumulated [`SendPressure`] for this channel.
    pub fn take_send_pressure(&self) -> SendPressure {
        self.pressure
            .lock()
            .map(|mut pressure| std::mem::take(&mut *pressure))
            .unwrap_or_default()
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn generation(&self) -> ChannelGeneration {
        self.generation
    }

    pub fn take_event(&self) -> io::Result<Option<ChannelEvent>> {
        Ok(lock(&self.events, "channel event queue")?.pop_front())
    }

    /// Wait until the presenter has made the ingress capacity used by every media record already
    /// submitted on this channel reusable.
    ///
    /// This is an ingress-storage barrier, not a decode or presentation acknowledgment. A caller
    /// that keeps at most one record outstanding can use it to avoid advancing another control or
    /// media connection ahead of the presenter's processing of that record.
    pub fn wait_for_reusable_media_capacity(&self) -> io::Result<()> {
        if self.rate.is_none() {
            // Offline sessions have no peer or reverse channel to return capacity.
            return Ok(());
        }
        let mut state = lock(&self.flow.state, "channel flow state")?;
        let required_body_bytes = state
            .initial_maximum_body_bytes
            .checked_add(state.flow.sent_body_bytes)
            .ok_or_else(|| invalid_data("channel byte flow maximum would saturate"))?;
        let required_media_records = state
            .initial_maximum_media_records
            .checked_add(state.flow.sent_media_records)
            .ok_or_else(|| invalid_data("channel record flow maximum would saturate"))?;
        loop {
            self.lifecycle.ensure_active()?;
            if state.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    state
                        .diagnostic
                        .clone()
                        .unwrap_or_else(|| "track channel is closed".into()),
                ));
            }
            if state.flow.maximum_body_bytes >= required_body_bytes
                && state.flow.maximum_media_records >= required_media_records
            {
                return Ok(());
            }
            state = self
                .flow
                .changed
                .wait(state)
                .map_err(|_| io::Error::other("channel flow lock is poisoned"))?;
        }
    }

    pub fn send_video(&self, packet: VideoPacket<'_>) -> io::Result<u64> {
        if self.track.kind() != TrackKind::Video {
            return Err(invalid_input("VIDEO_PACKET requires a video track"));
        }
        let body = media::video_packet_body(VideoPacket {
            epoch: packet.epoch,
            packet_id: packet.packet_id,
            pts_us: packet.pts_us,
            dts_us: packet.dts_us,
            duration_us: packet.duration_us,
            key: packet.key,
            data: packet.data,
        })?;
        self.send_media(
            messages::VIDEO_PACKET,
            packet.packet_id,
            packet.epoch,
            packet.key,
            &body,
        )
    }

    pub fn send_audio(&self, packet: AudioPacket<'_>) -> io::Result<u64> {
        if self.track.kind() != TrackKind::Audio {
            return Err(invalid_input("AUDIO_PACKET requires an audio track"));
        }
        let body = media::audio_packet_body(AudioPacket {
            epoch: packet.epoch,
            packet_id: packet.packet_id,
            pts_us: packet.pts_us,
            dts_us: packet.dts_us,
            duration_us: packet.duration_us,
            trim_start_samples: packet.trim_start_samples,
            trim_end_samples: packet.trim_end_samples,
            data: packet.data,
        })?;
        self.send_media(
            messages::AUDIO_PACKET,
            packet.packet_id,
            packet.epoch,
            true,
            &body,
        )
    }

    pub fn send_raster(
        &self,
        epoch: u32,
        frame_id: u64,
        rgba: &[u8],
        compress: bool,
    ) -> io::Result<u64> {
        let configuration = self.track.configuration()?;
        let KindConfiguration::Raster(raster) = configuration.kind else {
            return Err(invalid_input("RASTER_FRAME requires a raster track"));
        };
        if compress && !raster.zstd_enabled {
            return Err(invalid_input(
                "track configuration did not permit zstd raster frames",
            ));
        }
        let body = media::raster_frame_body_with_compression(
            epoch,
            frame_id,
            raster.width,
            raster.height,
            rgba,
            compress,
        )?;
        self.send_media(messages::RASTER_FRAME, frame_id, epoch, true, &body)
    }

    /// Send a full raster frame using zstd only when the negotiated track permits it and the
    /// encoded body is smaller than the raw representation.
    ///
    /// The size check is important because a track's maximum record body is commonly claimed from
    /// the raw framebuffer size. An incompressible zstd frame can be slightly larger than that
    /// claim, while a raw fallback is always admissible.
    pub fn send_raster_adaptive(&self, epoch: u32, frame_id: u64, rgba: &[u8]) -> io::Result<u64> {
        let configuration = self.track.configuration()?;
        let KindConfiguration::Raster(raster) = configuration.kind else {
            return Err(invalid_input("RASTER_FRAME requires a raster track"));
        };
        if raster.zstd_enabled {
            let compressed = media::raster_frame_body_with_compression(
                epoch,
                frame_id,
                raster.width,
                raster.height,
                rgba,
                true,
            )?;
            let raw_length = usize::try_from(
                media::rgba8_raw_frame_body_len(raster.width, raster.height)
                    .map_err(|error| invalid_input(error.to_string()))?,
            )
            .map_err(|_| invalid_input("raw raster body exceeds address space"))?;
            if compressed.len() < raw_length {
                return self.send_media(messages::RASTER_FRAME, frame_id, epoch, true, &compressed);
            }
        }
        self.send_raster(epoch, frame_id, rgba, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_raster_delta(
        &self,
        epoch: u32,
        frame_id: u64,
        base_frame_id: u64,
        pts_us: i64,
        duration_us: u64,
        operations: &[RasterDeltaOperation<'_>],
        compress: bool,
    ) -> io::Result<u64> {
        let body = self.raster_delta_body(
            epoch,
            frame_id,
            base_frame_id,
            pts_us,
            duration_us,
            operations,
            compress,
        )?;
        self.send_media(messages::RASTER_FRAME, frame_id, epoch, false, &body)
    }

    /// Send a raster delta using zstd only when every negotiated constraint permits it and the
    /// resulting body is smaller than the raw delta.
    #[allow(clippy::too_many_arguments)]
    pub fn send_raster_delta_adaptive(
        &self,
        epoch: u32,
        frame_id: u64,
        base_frame_id: u64,
        pts_us: i64,
        duration_us: u64,
        operations: &[RasterDeltaOperation<'_>],
    ) -> io::Result<u64> {
        let raw = self.raster_delta_body(
            epoch,
            frame_id,
            base_frame_id,
            pts_us,
            duration_us,
            operations,
            false,
        )?;
        let configuration = self.track.configuration()?;
        let KindConfiguration::Raster(raster) = configuration.kind else {
            return Err(invalid_input("RASTER_FRAME requires a raster track"));
        };
        let body = if raster.zstd_enabled {
            let compressed = self.raster_delta_body(
                epoch,
                frame_id,
                base_frame_id,
                pts_us,
                duration_us,
                operations,
                true,
            )?;
            if compressed.len() < raw.len() {
                compressed
            } else {
                raw
            }
        } else {
            raw
        };
        self.send_media(messages::RASTER_FRAME, frame_id, epoch, false, &body)
    }

    #[allow(clippy::too_many_arguments)]
    fn raster_delta_body(
        &self,
        epoch: u32,
        frame_id: u64,
        base_frame_id: u64,
        pts_us: i64,
        duration_us: u64,
        operations: &[RasterDeltaOperation<'_>],
        compress: bool,
    ) -> io::Result<Vec<u8>> {
        let state = lock(&self.track.inner, "track")?.clone();
        let KindConfiguration::Raster(raster) = state.configuration.kind else {
            return Err(invalid_input("RASTER_FRAME requires a raster track"));
        };
        if compress && !raster.zstd_enabled {
            return Err(invalid_input(
                "track configuration did not permit zstd raster frames",
            ));
        }
        if !raster.delta_enabled || state.delta_operation_limit == 0 {
            return Err(invalid_input(
                "track configuration did not permit raster deltas",
            ));
        }
        if lock(&self.media, "channel media state")?.needs_recovery {
            return Err(invalid_input(
                "a recovered raster channel must begin with a full frame",
            ));
        }
        if base_frame_id != lock(&self.track_sequence, "track media sequence")?.last_id {
            return Err(invalid_input(
                "raster delta base must be the immediately preceding accepted frame",
            ));
        }
        media::raster_delta_frame_body(
            epoch,
            frame_id,
            base_frame_id,
            pts_us,
            duration_us,
            raster.width,
            raster.height,
            state.delta_operation_limit,
            operations,
            compress,
        )
    }

    pub fn send_image(&self, encoded: &[u8]) -> io::Result<u64> {
        self.lifecycle.ensure_active()?;
        let configuration = self.track.configuration()?;
        let KindConfiguration::EncodedImage(image) = configuration.kind else {
            return Err(invalid_input("IMAGE_DATA requires an encoded-image track"));
        };
        if encoded.len() != image.encoded_length as usize {
            return Err(invalid_input(
                "encoded image length differs from immutable track configuration",
            ));
        }
        let mut state = lock(&self.media, "channel media state")?;
        if state.eos {
            return Err(invalid_input("media cannot follow CHANNEL_EOS"));
        }
        if state.image_sent {
            return Err(invalid_input(
                "encoded-image track accepts exactly one IMAGE_DATA record per generation",
            ));
        }
        let body_length =
            u32::try_from(encoded.len()).map_err(|_| invalid_input("image body exceeds u32"))?;
        let sequence = self.write_charged_record(
            messages::IMAGE_DATA,
            configuration.track_id,
            body_length,
            encoded,
        )?;
        state.last_sequence = sequence;
        state.needs_recovery = false;
        state.image_sent = true;
        Ok(sequence)
    }

    pub fn eos(&self) -> io::Result<u64> {
        self.lifecycle.ensure_active()?;
        let configuration = self.track.configuration()?;
        let mut media_state = lock(&self.media, "channel media state")?;
        if media_state.eos {
            return Err(invalid_input("CHANNEL_EOS was already sent"));
        }
        let body = Envelope::new(
            0,
            vec![
                (0, Value::Unsigned(configuration.context_id)),
                (1, Value::Unsigned(configuration.surface_id)),
                (2, Value::Unsigned(configuration.track_id)),
                (3, Value::Unsigned(self.generation.get())),
                (
                    4,
                    Value::Unsigned(u64::from(
                        lock(&self.track_sequence, "track media sequence")?.last_epoch,
                    )),
                ),
                (5, Value::Unsigned(media_state.last_sequence)),
            ],
        )
        .encode()?;
        let sequence =
            self.writer
                .write_record(messages::CHANNEL_EOS, 0, configuration.track_id, &body)?;
        media_state.eos = true;
        Ok(sequence)
    }

    pub fn close(&self) -> io::Result<()> {
        let mut state = lock(&self.flow.state, "channel flow state")?;
        state.closed = true;
        self.flow.changed.notify_all();
        Ok(())
    }

    pub(crate) fn send_media(
        &self,
        record_type: u16,
        media_id: u64,
        epoch: u32,
        recovery_unit: bool,
        body: &[u8],
    ) -> io::Result<u64> {
        self.lifecycle.ensure_active()?;
        let body_length =
            u32::try_from(body.len()).map_err(|_| invalid_input("media body exceeds u32"))?;
        let configuration = self.track.configuration()?;
        let mut media_state = lock(&self.media, "channel media state")?;
        if media_state.eos {
            return Err(invalid_input("media cannot follow CHANNEL_EOS"));
        }
        if media_state.needs_recovery && !recovery_unit {
            return Err(invalid_input(
                "channel generation must begin with a recovery unit",
            ));
        }
        if recovery_unit && epoch < media_state.minimum_recovery_epoch {
            return Err(invalid_input(
                "recovery unit epoch is below the presenter-requested minimum",
            ));
        }
        let mut track_sequence = lock(&self.track_sequence, "track media sequence")?;
        let mut next_sequence = *track_sequence;
        next_sequence.accept(media_id, epoch)?;
        let sequence =
            self.write_charged_record(record_type, configuration.track_id, body_length, body)?;
        *track_sequence = next_sequence;
        track_sequence.last_record_sequence = sequence;
        media_state.last_sequence = sequence;
        if recovery_unit {
            media_state.needs_recovery = false;
            media_state.minimum_recovery_epoch = epoch;
        }
        Ok(sequence)
    }

    pub(crate) fn write_charged_record(
        &self,
        record_type: u16,
        object_id: u64,
        body_length: u32,
        body: &[u8],
    ) -> io::Result<u64> {
        let rate_started = Instant::now();
        self.wait_for_rate(body_length)?;
        let rate_limited = rate_started.elapsed();
        let flow_started = Instant::now();
        let mut state = lock(&self.flow.state, "channel flow state")?;
        loop {
            self.lifecycle.ensure_active()?;
            if state.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    state
                        .diagnostic
                        .clone()
                        .unwrap_or_else(|| "track channel is closed".into()),
                ));
            }
            let mut admitted = state.flow;
            match admitted.admit(body_length) {
                Ok(()) => {
                    let flow_limited = flow_started.elapsed();
                    // Commit the allowance before entering transport I/O, then release the flow
                    // lock. The reverse-channel reader needs this same lock to publish
                    // MAX_CHANNEL_DATA. Holding it across a blocked media write can fill both
                    // socket directions: the presenter cannot finish returning credit, and the
                    // producer cannot observe the credit that would let it make progress.
                    state.flow = admitted;
                    drop(state);
                    let transport_started = Instant::now();
                    let result = self.writer.write_record(record_type, 0, object_id, body);
                    if result.is_ok() {
                        self.record_send_pressure(SendPressure {
                            rate_limited,
                            flow_limited,
                            transport: transport_started.elapsed(),
                            records: 1,
                        });
                    }
                    return result;
                }
                Err(ResourceError::FlowControl) => {
                    state = self
                        .flow
                        .changed
                        .wait(state)
                        .map_err(|_| io::Error::other("channel flow lock is poisoned"))?;
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }

    fn record_send_pressure(&self, pressure: SendPressure) {
        if let Ok(mut total) = self.pressure.lock() {
            total.accumulate(pressure);
        }
    }

    pub(crate) fn wait_for_rate(&self, body_length: u32) -> io::Result<()> {
        let Some(rate) = &self.rate else {
            return Ok(());
        };
        loop {
            self.lifecycle.ensure_active()?;
            if lock(&self.flow.state, "channel flow state")?.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "track channel is closed",
                ));
            }
            let mut state = lock(rate, "channel rate state")?;
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(state.updated_at);
            state.updated_at = now;
            state
                .body_bytes
                .replenish(elapsed)
                .map_err(io::Error::other)?;
            state.records.replenish(elapsed).map_err(io::Error::other)?;
            let mut body_bytes = state.body_bytes.clone();
            let mut records = state.records.clone();
            if body_bytes.charge(u64::from(body_length)).is_ok() && records.charge(1).is_ok() {
                state.body_bytes = body_bytes;
                state.records = records;
                return Ok(());
            }
            drop(state);
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for TrackChannel {
    fn drop(&mut self) {
        if let Ok(mut state) = self.flow.state.lock() {
            state.closed = true;
            self.flow.changed.notify_all();
        }
    }
}

impl Session {
    pub fn open_track_channel(&self, track: &Track) -> io::Result<TrackChannel> {
        self.lifecycle.ensure_active()?;
        let state = lock(&track.inner, "track")?.clone();
        ensure_live_track(&state)?;
        if !state.connection_required {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "TRACK_READY reported a cache hit that requires no channel",
            ));
        }
        let endpoint = match state.configuration.lane {
            LaneClass::Realtime => self.endpoints.realtime.as_ref(),
            LaneClass::Bulk => self.endpoints.bulk.as_ref(),
            _ => return Err(invalid_input("track lane must be realtime or bulk")),
        };
        let mut nonce = [0; 16];
        random_bytes(&mut nonce)?;
        let tag = auth::channel_tag(
            self.channel_key.expose(),
            self.info.session_id,
            state.configuration.context_id,
            state.configuration.surface_id,
            state.configuration.track_id,
            state.channel_generation.get(),
            state.configuration.kind.kind() as u32,
            state.configuration.lane as u32,
            &nonce,
        );
        let open = ChannelOpen {
            session_id: self.info.session_id,
            context_id: state.configuration.context_id,
            surface_id: state.configuration.surface_id,
            track_id: state.configuration.track_id,
            channel_generation: state.channel_generation.get(),
            track_kind: state.configuration.kind.kind(),
            lane: state.configuration.lane,
            client_nonce: nonce,
            authentication_tag: tag,
        };
        let open_body = Envelope::correlated(1, open.payload())?.encode()?;
        let connection = if let Some(directory) = &self.trace_dir {
            Connection::trace(
                &directory.join(format!(
                    "track-{}-{}-{}-{}.vivid",
                    state.configuration.context_id,
                    state.configuration.surface_id,
                    state.configuration.track_id,
                    state.channel_generation.get()
                )),
                ConnectionKind::Track,
            )?
        } else if matches!(self.control, ControlPlane::Offline { .. }) {
            Connection::sink(ConnectionKind::Track)?
        } else if let Some(factory) = &self.connection_factory {
            factory.open(ConnectionKind::Track, Some(state.configuration.lane))?
        } else {
            Connection::open(
                endpoint.ok_or_else(|| invalid_input("missing Vivid track endpoint"))?,
                ConnectionKind::Track,
            )?
        };
        TrackChannel::establish(
            connection,
            open_body,
            track.clone(),
            self.lifecycle.clone(),
            matches!(&self.control, ControlPlane::Offline { .. }),
        )
    }

    pub fn advance_channel(
        &mut self,
        track: &Track,
        reason: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<ChannelGeneration> {
        let snapshot = lock(&track.inner, "track")?.clone();
        ensure_live_track(&snapshot)?;
        let next = snapshot.channel_generation.advance()?;
        let reply = self.request(
            messages::ADVANCE_CHANNEL,
            snapshot.configuration.track_id,
            vec![
                (0, Value::Unsigned(snapshot.configuration.context_id)),
                (1, Value::Unsigned(snapshot.configuration.surface_id)),
                (2, Value::Unsigned(snapshot.configuration.track_id)),
                (3, Value::Unsigned(snapshot.channel_generation.get())),
                (4, Value::Unsigned(next.get())),
                (5, Value::Unsigned(reason)),
            ],
            metadata,
            None,
            None,
        )?;
        let (generation, deadline, revision) = if let Some(record) = reply {
            expect_record(
                &record,
                messages::CHANNEL_ADVANCED,
                snapshot.configuration.track_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("CHANNEL_ADVANCED", &payload, 0..=5)?;
            validate_track_tuple(&payload, &snapshot.configuration)?;
            (
                ChannelGeneration::new(required_u64(&payload, 3)?),
                required_u64(&payload, 4)?,
                TrackRevision::new(required_u64(&payload, 5)?),
            )
        } else {
            (next, 30_000_000, snapshot.revision.advance()?)
        };
        if generation != next {
            return Err(invalid_data(
                "CHANNEL_ADVANCED did not return the requested next generation",
            ));
        }
        revision.require_nonzero()?;
        if deadline == 0 || deadline > 30_000_000 {
            return Err(invalid_data(
                "CHANNEL_ADVANCED returned an invalid open deadline",
            ));
        }
        let mut state = lock(&track.inner, "track")?;
        let old_flow = state.active_flow.take();
        state.active_media = None;
        lock(&state.media_sequence, "track media sequence")?.last_record_sequence = 0;
        state.channel_generation = generation;
        state.open_deadline_us = deadline;
        state.revision = revision;
        drop(state);
        close_track_flow(old_flow.as_ref(), "track channel generation advanced");
        Ok(generation)
    }
}

pub(crate) fn spawn_channel_reader(
    mut reader: ConnectionReader,
    configuration: TrackConfiguration,
    generation: ChannelGeneration,
    flow: Arc<FlowSync>,
    media: Arc<Mutex<ChannelMediaState>>,
    events: Arc<Mutex<VecDeque<ChannelEvent>>>,
) -> io::Result<()> {
    thread::Builder::new()
        .name(format!("vivid-track-reader-{}", configuration.track_id))
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                loop {
                    let record = reader.read_record()?;
                    if record.object_id != configuration.track_id {
                        return Err(invalid_data("reverse track record has the wrong object ID"));
                    }
                    if record.record_type == messages::ERROR {
                        let error = messages::parse_error_reply(&record.body)?;
                        push_channel_event(&events, ChannelEvent::Error(error.into()))?;
                        continue;
                    }
                    let payload = decoded_payload(&record)?;
                    validate_track_tuple(&payload, &configuration)?;
                    if required_u64(&payload, 3)? != generation.get() {
                        return Err(invalid_data(
                            "reverse track record uses a stale channel generation",
                        ));
                    }
                    match record.record_type {
                        messages::MAX_CHANNEL_DATA => {
                            validate_exact_payload_keys("MAX_CHANNEL_DATA", &payload, 0..=5)?;
                            let maximum_bytes = required_u64(&payload, 4)?;
                            let maximum_records = required_u64(&payload, 5)?;
                            let mut state = lock(&flow.state, "channel flow state")?;
                            state.flow.raise_maxima(maximum_bytes, maximum_records);
                            flow.changed.notify_all();
                        }
                        messages::NEED_KEYFRAME => {
                            validate_payload_keys("NEED_KEYFRAME", &payload, 0..=5, &[6])?;
                            let minimum_epoch = required_u32(&payload, 4)?;
                            let mut state = lock(&media, "channel media state")?;
                            state.needs_recovery = true;
                            state.minimum_recovery_epoch =
                                state.minimum_recovery_epoch.max(minimum_epoch);
                            drop(state);
                            push_channel_event(&events, ChannelEvent::NeedKeyframe(payload))?;
                        }
                        messages::NEED_FULL_FRAME => {
                            validate_exact_payload_keys("NEED_FULL_FRAME", &payload, 0..=4)?;
                            lock(&media, "channel media state")?.needs_recovery = true;
                            push_channel_event(&events, ChannelEvent::NeedFullFrame(payload))?;
                        }
                        _ if record.flags & vivid_protocol::wire::RECORD_OPTIONAL != 0 => {}
                        _ => {
                            return Err(invalid_data("unexpected required reverse track record"));
                        }
                    }
                }
            })();
            if let Ok(mut state) = flow.state.lock() {
                state.closed = true;
                state.diagnostic = result.err().map(|error| error.to_string());
                flow.changed.notify_all();
            }
        })
        .map(|_| ())
}

pub(crate) fn push_channel_event(
    events: &Mutex<VecDeque<ChannelEvent>>,
    event: ChannelEvent,
) -> io::Result<()> {
    let mut events = lock(events, "channel event queue")?;
    if events.len() == MAX_CHANNEL_EVENTS {
        return Err(invalid_data("track event queue exceeded its bound"));
    }
    events.push_back(event);
    Ok(())
}

pub(crate) fn close_track_flow(flow: Option<&Weak<FlowSync>>, message: &str) {
    let Some(flow) = flow.and_then(Weak::upgrade) else {
        return;
    };
    if let Ok(mut state) = flow.state.lock() {
        state.closed = true;
        state.diagnostic.get_or_insert_with(|| message.to_owned());
        flow.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use vivid_protocol::messages::{self, LaneClass};
    use vivid_protocol::track::{
        KindConfiguration, RasterConfiguration, TrackConfiguration, TrackMode,
    };
    use vivid_protocol::wire::{Connection, ConnectionKind};

    use crate::{
        CoordinateModel, ProducerConfig, RequestMetadata, Session, SurfaceDefinition,
        SurfaceDescriptor, SurfaceRole,
    };

    struct BlockingWriter {
        armed: Arc<AtomicBool>,
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.armed.swap(false, Ordering::SeqCst) {
                let _ = self.entered.send(());
                let _ = self.release.recv();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_transport_write_does_not_hide_returned_flow_credit() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let context = session.info().root_context_id;
        let surface = session
            .create_surface(
                SurfaceDefinition {
                    context_id: context,
                    surface_id: 1,
                    semantic_profile: crate::GENERIC_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 1,
                    logical_height: 1,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: SurfaceRole::Figure,
                        title: "flow-lock-test".into(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: Vec::new(),
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let track = session
            .create_track(
                TrackConfiguration {
                    context_id: context,
                    surface_id: surface.id(),
                    track_id: 2,
                    slot: 3,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: 1024,
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: 1_000_000,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: 2048,
                    kind: KindConfiguration::Raster(RasterConfiguration {
                        width: 1,
                        height: 1,
                        alpha_mode: 1,
                        delta_enabled: false,
                        maximum_delta_operations: 1,
                        zstd_enabled: false,
                    }),
                    target_latency_us: 16_000,
                    maximum_latency_us: 100_000,
                    retained_pixel_charge: 4,
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let mut channel = session.open_track_channel(&track).unwrap();

        let armed = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let connection = Connection::from_streams(
            Box::new(io::empty()),
            Box::new(BlockingWriter {
                armed: armed.clone(),
                entered: entered_tx,
                release: release_rx,
            }),
            ConnectionKind::Track,
        )
        .unwrap();
        channel.writer = connection.writer();

        armed.store(true, Ordering::SeqCst);
        let sending = channel.clone();
        let sender = thread::spawn(move || {
            sending.write_charged_record(messages::RASTER_FRAME, 2, 64, &[0; 64])
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("test transport never blocked");
        let reader_can_publish_credit = channel.flow.state.try_lock().is_ok();
        release_tx.send(()).unwrap();
        sender.join().unwrap().unwrap();
        assert!(
            reader_can_publish_credit,
            "a blocked transport write held the flow lock needed by MAX_CHANNEL_DATA"
        );
    }
}
