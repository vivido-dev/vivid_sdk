//! Bounded media pipeline types and channel recovery.
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::revision::ChannelGeneration;

use crate::{
    ChannelEvent, RequestMetadata, SendPressure, Session, Track, TrackChannel, invalid_data, lock,
};

/// A single-slot atomic, latest-wins capture boundary with a drop counter.
pub struct LatestFrame<T> {
    inner: Mutex<Option<T>>,
    changed: Condvar,
    dropped: AtomicU64,
}
impl<T> LatestFrame<T> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            changed: Condvar::new(),
            dropped: AtomicU64::new(0),
        }
    }
    pub fn store(&self, frame: T) -> u64 {
        let mut guard = self.inner.lock().expect("latest frame");
        let was_present = guard.is_some();
        *guard = Some(frame);
        self.changed.notify_one();
        if was_present {
            self.dropped.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            0
        }
    }
    pub fn take(&self) -> Option<T> {
        let mut guard = self.inner.lock().expect("latest frame");
        loop {
            if let Some(frame) = guard.take() {
                return Some(frame);
            }
            guard = self.changed.wait(guard).expect("latest frame condvar");
        }
    }
    pub fn try_take(&self) -> Option<T> {
        self.inner.lock().expect("latest frame").take()
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}
impl<T> Default for LatestFrame<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A bounded MPSC queue with a dropped-item counter.
pub struct BoundedQueue<T> {
    inner: Mutex<BoundedInner<T>>,
    not_empty: Condvar,
    not_full: Condvar,
    capacity: usize,
}
struct BoundedInner<T> {
    items: VecDeque<T>,
    dropped: u64,
    closed: bool,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            inner: Mutex::new(BoundedInner {
                items: VecDeque::with_capacity(capacity),
                dropped: 0,
                closed: false,
            }),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            capacity,
        }
    }
    pub fn push_blocking(&self, item: T) -> Result<(), T> {
        let mut g = self.inner.lock().expect("bounded queue");
        while g.items.len() >= self.capacity && !g.closed {
            g = self.not_full.wait(g).expect("not-full");
        }
        if g.closed {
            return Err(item);
        }
        g.items.push_back(item);
        self.not_empty.notify_one();
        Ok(())
    }
    pub fn push_nonblocking(&self, item: T) -> Result<(), T> {
        let mut g = self.inner.lock().expect("bounded queue");
        if g.closed {
            return Err(item);
        }
        if g.items.len() >= self.capacity {
            g.dropped += 1;
            return Err(item);
        }
        g.items.push_back(item);
        self.not_empty.notify_one();
        Ok(())
    }
    #[allow(clippy::result_unit_err)]
    pub fn pop(&self) -> Result<T, ()> {
        let mut g = self.inner.lock().expect("bounded queue");
        loop {
            if let Some(item) = g.items.pop_front() {
                self.not_full.notify_one();
                return Ok(item);
            }
            if g.closed {
                return Err(());
            }
            g = self.not_empty.wait(g).expect("not-empty");
        }
    }
    pub fn close(&self) {
        let mut g = self.inner.lock().expect("bounded queue");
        g.closed = true;
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }
    pub fn len(&self) -> usize {
        self.inner.lock().expect("bounded queue").items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn dropped(&self) -> u64 {
        self.inner.lock().expect("bounded queue").dropped
    }
    pub fn is_closed(&self) -> bool {
        self.inner.lock().expect("bounded queue").closed
    }
}
impl<T> Drop for BoundedQueue<T> {
    fn drop(&mut self) {
        self.close();
    }
}

/// An encoded packet ready to send.
#[derive(Debug, Clone)]
pub enum EncodedPacket {
    Video(VideoPacketData),
    Audio(AudioPacketData),
}
#[derive(Debug, Clone)]
pub struct VideoPacketData {
    pub epoch: u32,
    pub packet_id: u64,
    pub pts_us: i64,
    pub dts_us: i64,
    pub duration_us: u64,
    pub key: bool,
    pub data: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct AudioPacketData {
    pub epoch: u32,
    pub packet_id: u64,
    pub pts_us: i64,
    pub dts_us: i64,
    pub duration_us: u64,
    pub data: Vec<u8>,
}

/// A per-track sender that drains a bounded queue through one channel generation.
///
/// Clones share the channel, the packet-ID counter, and the media epoch, so handing a clone to a
/// media worker continues the exact sequence — exactly one sender per channel generation may send.
#[derive(Clone)]
pub struct TrackSender {
    channel: TrackChannel,
    detached: Arc<AtomicBool>,
    last_packet_id: Arc<AtomicU64>,
    epoch: Arc<AtomicU32>,
}
impl TrackSender {
    pub fn new(channel: TrackChannel) -> Self {
        Self {
            channel,
            detached: Arc::new(AtomicBool::new(false)),
            last_packet_id: Arc::new(AtomicU64::new(0)),
            epoch: Arc::new(AtomicU32::new(1)),
        }
    }

    /// A sender continuing an existing track media sequence.
    ///
    /// Channel recovery must keep packet IDs increasing and the media epoch
    /// non-decreasing across generations, so the sender starts past the recovery
    /// unit's ID and at the track's current epoch.
    pub(crate) fn seeded(channel: TrackChannel, next_packet_id: u64, epoch: u32) -> Self {
        Self {
            channel,
            detached: Arc::new(AtomicBool::new(false)),
            last_packet_id: Arc::new(AtomicU64::new(next_packet_id.saturating_sub(1))),
            epoch: Arc::new(AtomicU32::new(epoch)),
        }
    }
    pub fn channel(&self) -> &TrackChannel {
        &self.channel
    }
    pub fn generation(&self) -> ChannelGeneration {
        self.channel.generation()
    }
    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }
    pub fn detach(&self) {
        self.detached.store(true, Ordering::Release);
    }
    pub fn next_packet_id(&self) -> u64 {
        self.last_packet_id.fetch_add(1, Ordering::Relaxed) + 1
    }
    pub fn bump_epoch(&self) -> u32 {
        self.epoch.fetch_add(1, Ordering::Relaxed) + 1
    }
    pub fn current_epoch(&self) -> u32 {
        self.epoch.load(Ordering::Relaxed)
    }
    pub fn drain_events(&self) -> Vec<ChannelEvent> {
        let mut events = Vec::new();
        while let Ok(Some(event)) = self.channel.take_event() {
            events.push(event);
        }
        events
    }
    pub fn send(&self, packet: &EncodedPacket) -> io::Result<u64> {
        if self.detached.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "track channel is detached",
            ));
        }
        match packet {
            EncodedPacket::Video(p) => self.channel.send_video(VideoPacket {
                epoch: p.epoch,
                packet_id: p.packet_id,
                pts_us: p.pts_us,
                dts_us: p.dts_us,
                duration_us: p.duration_us,
                key: p.key,
                data: &p.data,
            }),
            EncodedPacket::Audio(p) => self.channel.send_audio(AudioPacket {
                epoch: p.epoch,
                packet_id: p.packet_id,
                pts_us: p.pts_us,
                dts_us: p.dts_us,
                duration_us: p.duration_us,
                trim_start_samples: 0,
                trim_end_samples: 0,
                data: &p.data,
            }),
        }
    }
    pub fn flush_one(&self, queue: &BoundedQueue<EncodedPacket>) -> io::Result<Option<u64>> {
        match queue.pop() {
            Ok(packet) => self.send(&packet).map(Some),
            Err(()) => Ok(None),
        }
    }
}

/// Recover a detached channel: advance, reopen, and send a key unit.
///
/// Flow (c): only the affected track is touched. The caller provides a key unit (video) or fresh
/// access unit (audio). The existing surface, node, and input binding are untouched.
///
/// The returned sender continues the track's media sequence: packet IDs stay strictly increasing
/// and the epoch never moves backward across the recovered generation, so subsequent sends through
/// the sender pass the track sequence checks.
pub fn recover_channel(
    session: &mut Session,
    track: &Track,
    key_unit_or_fresh_au: &[u8],
) -> io::Result<TrackSender> {
    let _new_gen =
        session.advance_channel(track, 3 /* recovery */, &RequestMetadata::default())?;
    let channel = session.open_track_channel(track)?;
    let (next_id, epoch) = {
        let snapshot = lock(&track.inner, "track")?.clone();
        let sequence = lock(&snapshot.media_sequence, "track media sequence")?;
        let id = sequence
            .last_id
            .checked_add(1)
            .ok_or_else(|| invalid_data("track media ID space exhausted"))?;
        (id, sequence.last_epoch.max(1))
    };
    // Send the recovery unit as the next media ID at the current epoch.
    channel.send_video(VideoPacket {
        epoch,
        packet_id: next_id,
        pts_us: 0,
        dts_us: 0,
        duration_us: 0,
        key: true,
        data: key_unit_or_fresh_au,
    })?;
    let next = next_id
        .checked_add(1)
        .ok_or_else(|| invalid_data("track media ID space exhausted"))?;
    Ok(TrackSender::seeded(channel, next, epoch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn latest_frame_replaces_and_counts_drops() {
        let frame = LatestFrame::new();
        assert!(frame.try_take().is_none());
        assert_eq!(frame.store(1), 0);
        assert_eq!(frame.store(2), 1);
        assert_eq!(frame.dropped(), 1);
        assert_eq!(frame.try_take(), Some(2));
    }
    #[test]
    fn latest_frame_blocks_until_filled() {
        let frame = Arc::new(LatestFrame::new());
        let f2 = frame.clone();
        let done = Arc::new(AtomicBool::new(false));
        let d2 = done.clone();
        let h = thread::spawn(move || {
            d2.store(true, Ordering::SeqCst);
            f2.store(7);
        });
        while !done.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(frame.take(), Some(7));
        h.join().unwrap();
    }
    #[test]
    fn queue_blocks_and_wakes() {
        let q = Arc::new(BoundedQueue::<i32>::new(1));
        let q2 = q.clone();
        let pushed = Arc::new(AtomicBool::new(false));
        let p2 = pushed.clone();
        let h = thread::spawn(move || {
            p2.store(true, Ordering::SeqCst);
            q2.push_blocking(42).unwrap();
        });
        while !pushed.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(q.pop(), Ok(42));
        h.join().unwrap();
    }
    #[test]
    fn nonblocking_push_drops_when_full() {
        let q = BoundedQueue::<i32>::new(1);
        assert!(q.push_nonblocking(1).is_ok());
        assert!(q.push_nonblocking(2).is_err());
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.pop(), Ok(1));
        q.close();
        assert!(q.push_nonblocking(3).is_err());
    }
    #[test]
    fn queue_close_wakes_all() {
        let q = Arc::new(BoundedQueue::<i32>::new(1));
        let q2 = q.clone();
        let h = thread::spawn(move || q2.pop());
        q.close();
        assert_eq!(h.join().unwrap(), Err(()));
    }
    #[test]
    fn detach_refuses_sends() {
        use crate::{
            CoordinateModel, ProducerConfig, Session, SurfaceDefinition, SurfaceDescriptor,
            SurfaceRole,
        };
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let _surf = s
            .create_surface(
                SurfaceDefinition {
                    context_id: 1,
                    surface_id: 1,
                    semantic_profile: crate::DESKTOP_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 1920,
                    logical_height: 1080,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: SurfaceRole::Desktop,
                        title: "p".into(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: vec![],
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let cfg = crate::TrackConfiguration {
            direction: Default::default(),
            context_id: 1,
            surface_id: 1,
            track_id: 7,
            slot: 1,
            mode: crate::TrackMode::Live,
            lane: crate::LaneClass::Bulk,
            maximum_record_body: 65536,
            maximum_rate_millihertz: 30000,
            maximum_encoded_bits_per_second: 1_000_000,
            maximum_records_per_second: 30,
            maximum_inflight_body_bytes: 131072,
            kind: crate::KindConfiguration::Video(crate::VideoConfiguration {
                codec: "h264".into(),
                packetization: "h264-annexb-au-v1".into(),
                extradata: vec![],
                coded_width: 320,
                coded_height: 240,
                profile: 0,
                level: 0,
                maximum_reorder_depth: 0,
                color_primaries: 1,
                transfer: 1,
                matrix: 1,
                signal_range: 1,
                aspect_numerator: 1,
                aspect_denominator: 1,
                maximum_access_unit_bytes: 4096,
                codec_string: None,
                decoder_configuration: None,
            }),
            target_latency_us: 33000,
            maximum_latency_us: 100000,
            retained_pixel_charge: 76800,
        };
        let tk = s.create_track(cfg, &RequestMetadata::default()).unwrap();
        let ch = s.open_track_channel(&tk).unwrap();
        let sender = TrackSender::new(ch);
        sender.detach();
        let pkt = EncodedPacket::Video(VideoPacketData {
            epoch: 1,
            packet_id: 1,
            pts_us: 0,
            dts_us: 0,
            duration_us: 0,
            key: true,
            data: vec![0; 100],
        });
        assert!(sender.send(&pkt).is_err());
    }
    #[test]
    fn recover_channel_advances_generation() {
        use crate::{
            CoordinateModel, ProducerConfig, Session, SurfaceDefinition, SurfaceDescriptor,
            SurfaceRole,
        };
        let mut s = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let _surf = s
            .create_surface(
                SurfaceDefinition {
                    context_id: 1,
                    surface_id: 1,
                    semantic_profile: crate::DESKTOP_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 1920,
                    logical_height: 1080,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: SurfaceRole::Desktop,
                        title: "r".into(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: vec![],
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let cfg = crate::TrackConfiguration {
            direction: Default::default(),
            context_id: 1,
            surface_id: 1,
            track_id: 7,
            slot: 1,
            mode: crate::TrackMode::Live,
            lane: crate::LaneClass::Bulk,
            maximum_record_body: 65536,
            maximum_rate_millihertz: 30000,
            maximum_encoded_bits_per_second: 1_000_000,
            maximum_records_per_second: 30,
            maximum_inflight_body_bytes: 131072,
            kind: crate::KindConfiguration::Video(crate::VideoConfiguration {
                codec: "h264".into(),
                packetization: "h264-annexb-au-v1".into(),
                extradata: vec![],
                coded_width: 320,
                coded_height: 240,
                profile: 0,
                level: 0,
                maximum_reorder_depth: 0,
                color_primaries: 1,
                transfer: 1,
                matrix: 1,
                signal_range: 1,
                aspect_numerator: 1,
                aspect_denominator: 1,
                maximum_access_unit_bytes: 4096,
                codec_string: None,
                decoder_configuration: None,
            }),
            target_latency_us: 33000,
            maximum_latency_us: 100000,
            retained_pixel_charge: 76800,
        };
        let tk = s.create_track(cfg, &RequestMetadata::default()).unwrap();
        // Stream media first so the track sequence is past its initial ID, which is
        // the state every real recovery faces.
        let ch = s.open_track_channel(&tk).unwrap();
        let first = TrackSender::new(ch);
        assert!(
            first
                .send(&EncodedPacket::Video(VideoPacketData {
                    epoch: 1,
                    packet_id: first.next_packet_id(),
                    pts_us: 0,
                    dts_us: 0,
                    duration_us: 0,
                    key: true,
                    data: vec![0; 100],
                }))
                .is_ok()
        );
        let key_unit = vec![0x00, 0x00, 0x00, 0x01, 0x67];
        let recovered = recover_channel(&mut s, &tk, &key_unit).unwrap();
        assert!(recovered.generation().get() > ChannelGeneration::ONE.get());
        assert_eq!(tk.id(), 7);
        // The recovered sender continues the track sequence: packet IDs strictly
        // increase across the generation boundary and the epoch never moves backward.
        assert!(
            recovered
                .send(&EncodedPacket::Video(VideoPacketData {
                    epoch: recovered.current_epoch(),
                    packet_id: recovered.next_packet_id(),
                    pts_us: 0,
                    dts_us: 0,
                    duration_us: 0,
                    key: false,
                    data: vec![0; 64],
                }))
                .is_ok()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Closed-loop video bitrate control
//
// A producer cannot measure the link directly, so it measures where its sends actually wait. Only
// `SendPressure::transport` means the link will not take the bytes: the declared-rate limiter is
// self-imposed, and channel-flow waiting means the presenter is behind on *records*, which fewer
// bits per frame would not help.
//
// Getting this wrong is not a matter of tuning. If the bitrate answers a frame-rate limit, the
// bytes delivered fall in proportion to the target, the next window sees an even lower delivered
// rate, and the loop decays to the floor with no fixed point: the picture degrades to a blurry
// mess over a few seconds while the link sits idle. Hence both the transport-only signal and the
// delivered-target guard in `next_target`.
// ---------------------------------------------------------------------------------------------

/// Encoder targets move in whole steps so ordinary jitter cannot cause a re-open.
const RATE_STEP_BITS_PER_SECOND: u64 = 250_000;
/// Below this the picture is no longer worth the bits; the frame rate absorbs the rest.
pub const MINIMUM_TARGET_BITS_PER_SECOND: u64 = 400_000;
/// One decision per window. Shorter windows chase individual key frames.
const RATE_WINDOW: Duration = Duration::from_millis(1_000);
/// The share of a window spent inside the transport write that counts as congestion.
const CONGESTED_TRANSPORT_PERCENT: u64 = 10;
/// Below this the link is carrying everything offered and the target may grow again.
const UNCONGESTED_TRANSPORT_PERCENT: u64 = 2;
/// Audio backlog that means the session is congested even if video happens not to be blocked.
const CONGESTED_AUDIO_BACKLOG_US: u64 = 250_000;
/// Delivering this share of the target means the target is not what is limiting the stream, so
/// lowering it cannot help. Without this the loop has no fixed point and decays to the floor.
const DELIVERED_TARGET_PERCENT: u64 = 90;

pub struct VideoRateControl {
    configured_bits_per_second: u64,
    target_bits_per_second: AtomicU64,
    inner: Mutex<RateWindow>,
}

struct RateWindow {
    started: Instant,
    bytes: u64,
    pressure: SendPressure,
    audio_backlog_us: u64,
    adjustments: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoRateSnapshot {
    pub configured_bits_per_second: u64,
    pub target_bits_per_second: u64,
    pub adjustments: u64,
    pub rate_limited: Duration,
    pub flow_limited: Duration,
    pub transport: Duration,
}

impl VideoRateControl {
    pub fn new(configured_bits_per_second: u64) -> Self {
        let configured = configured_bits_per_second.max(MINIMUM_TARGET_BITS_PER_SECOND);
        Self {
            configured_bits_per_second: configured,
            target_bits_per_second: AtomicU64::new(configured),
            inner: Mutex::new(RateWindow {
                started: Instant::now(),
                bytes: 0,
                pressure: SendPressure::default(),
                audio_backlog_us: 0,
                adjustments: 0,
            }),
        }
    }

    pub fn target(&self) -> u64 {
        self.target_bits_per_second.load(Ordering::Acquire)
    }

    pub fn configured(&self) -> u64 {
        self.configured_bits_per_second
    }

    /// Account one completed media send: its body size and where the send waited.
    pub fn observe_send(&self, bytes: usize, pressure: SendPressure) {
        let mut window = self.lock();
        window.bytes = window
            .bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        window.pressure.rate_limited = window
            .pressure
            .rate_limited
            .saturating_add(pressure.rate_limited);
        window.pressure.flow_limited = window
            .pressure
            .flow_limited
            .saturating_add(pressure.flow_limited);
        window.pressure.transport = window.pressure.transport.saturating_add(pressure.transport);
        window.pressure.records = window.pressure.records.saturating_add(pressure.records);
    }

    /// Audio that cannot be handed to the transport is congestion the video target has to answer
    /// for: audio is two orders of magnitude cheaper, so if it is backing up, video is the cause.
    pub fn observe_audio_backlog(&self, queued_duration_us: u64) {
        let mut window = self.lock();
        window.audio_backlog_us = window.audio_backlog_us.max(queued_duration_us);
    }

    /// Close the window if it is due and return a changed target.
    pub fn poll(&self) -> Option<u64> {
        let mut window = self.lock();
        let elapsed = window.started.elapsed();
        if elapsed < RATE_WINDOW {
            return None;
        }
        let current = self.target_bits_per_second.load(Ordering::Acquire);
        let next = next_target(
            current,
            self.configured_bits_per_second,
            achieved_bits_per_second(window.bytes, elapsed),
            blocked_percent(window.pressure.transport, elapsed),
            window.audio_backlog_us,
        );
        window.started = Instant::now();
        window.bytes = 0;
        window.pressure = SendPressure::default();
        window.audio_backlog_us = 0;
        if next == current {
            return None;
        }
        window.adjustments = window.adjustments.saturating_add(1);
        self.target_bits_per_second.store(next, Ordering::Release);
        Some(next)
    }

    pub fn snapshot(&self) -> VideoRateSnapshot {
        let window = self.lock();
        VideoRateSnapshot {
            configured_bits_per_second: self.configured_bits_per_second,
            target_bits_per_second: self.target_bits_per_second.load(Ordering::Acquire),
            adjustments: window.adjustments,
            rate_limited: window.pressure.rate_limited,
            flow_limited: window.pressure.flow_limited,
            transport: window.pressure.transport,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RateWindow> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn achieved_bits_per_second(bytes: u64, elapsed: Duration) -> u64 {
    let micros = elapsed.as_micros().max(1);
    u64::try_from(u128::from(bytes).saturating_mul(8_000_000) / micros).unwrap_or(u64::MAX)
}

fn blocked_percent(blocked: Duration, elapsed: Duration) -> u64 {
    let micros = elapsed.as_micros().max(1);
    u64::try_from(blocked.as_micros().saturating_mul(100) / micros)
        .unwrap_or(100)
        .min(100)
}

/// The control law, split out so the decision is testable without a transport.
///
/// `transport_percent` is the share of the window spent inside the transport write — deliberately
/// not the total time `send` blocked, which also covers this producer's own rate limiter and the
/// presenter's channel-flow window. Neither of those is answered by encoding at a lower rate.
fn next_target(
    current: u64,
    configured: u64,
    achieved_bits_per_second: u64,
    transport_percent: u64,
    audio_backlog_us: u64,
) -> u64 {
    // If the target is reaching the presenter, the target is not the constraint. Whatever else is
    // slow — the encoder, the camera, the presenter's decode — lowering the bitrate only spends
    // the same frames worse, and each lower target would make the next window look worse still.
    // This is the guard that gives the loop a fixed point.
    let delivering = achieved_bits_per_second.saturating_mul(100)
        >= current.saturating_mul(DELIVERED_TARGET_PERCENT);
    let link_full = transport_percent >= CONGESTED_TRANSPORT_PERCENT && !delivering;
    // Audio is two orders of magnitude cheaper than video. If it cannot reach the transport, the
    // session is over-subscribed whatever the video sends look like.
    let over_subscribed = audio_backlog_us >= CONGESTED_AUDIO_BACKLOG_US;
    let candidate = if link_full || over_subscribed {
        // Back off from whatever the link actually carried, not from what was asked for: a target
        // that only ever halves itself takes far too long to reach a link an order of magnitude
        // slower than the configured ceiling.
        let reference = if achieved_bits_per_second > 0 {
            current.min(achieved_bits_per_second)
        } else {
            current
        };
        reference.saturating_mul(7) / 8
    } else if transport_percent <= UNCONGESTED_TRANSPORT_PERCENT
        && audio_backlog_us < CONGESTED_AUDIO_BACKLOG_US
    {
        current.saturating_add(configured / 8)
    } else {
        current
    };
    quantize_target(candidate, configured)
}

fn quantize_target(candidate: u64, configured: u64) -> u64 {
    let clamped = candidate.clamp(MINIMUM_TARGET_BITS_PER_SECOND, configured);
    if clamped >= configured {
        return configured;
    }
    (clamped / RATE_STEP_BITS_PER_SECOND)
        .saturating_mul(RATE_STEP_BITS_PER_SECOND)
        .max(MINIMUM_TARGET_BITS_PER_SECOND)
}

#[cfg(test)]
mod video_rate_tests {
    use super::*;

    const CONFIGURED: u64 = 4_000_000;

    #[test]
    fn an_idle_link_climbs_back_to_the_configured_ceiling() {
        let mut target = 1_000_000;
        for _ in 0..64 {
            target = next_target(target, CONFIGURED, target, 0, 0);
        }
        assert_eq!(target, CONFIGURED);
    }

    #[test]
    fn a_congested_link_backs_off() {
        let lowered = next_target(CONFIGURED, CONFIGURED, 1_000_000, 50, 0);
        assert!(
            lowered < CONFIGURED,
            "a blocked transport must lower the target, got {lowered}"
        );
    }

    #[test]
    fn back_off_references_the_achieved_rate_not_the_target() {
        // A link carrying 1 Mbps against a 4 Mbps target must fall towards 1 Mbps in one step,
        // not shave an eighth off 4 Mbps. Repeatedly halving the *target* takes far too long to
        // reach a link an order of magnitude slower than the ceiling.
        let from_achieved = next_target(CONFIGURED, CONFIGURED, 1_000_000, 50, 0);
        assert!(
            from_achieved <= 1_000_000,
            "expected a step towards the achieved rate, got {from_achieved}"
        );
    }

    #[test]
    fn delivering_the_target_prevents_the_decay_to_the_floor() {
        // The fixed-point guard: transport time is high, but the link is carrying the full
        // target, so something other than the bitrate is slow and lowering it cannot help.
        let held = next_target(CONFIGURED, CONFIGURED, CONFIGURED, 90, 0);
        assert_eq!(held, CONFIGURED);

        // And it must hold across repeated windows rather than merely surviving one.
        let mut target = CONFIGURED;
        for _ in 0..32 {
            target = next_target(target, CONFIGURED, target, 90, 0);
        }
        assert_eq!(target, CONFIGURED);
    }

    #[test]
    fn audio_backlog_alone_triggers_a_back_off() {
        // Video is not blocked at all, but audio cannot reach the transport, so the session is
        // over-subscribed and video is the only thing big enough to be the cause.
        let lowered = next_target(
            CONFIGURED,
            CONFIGURED,
            CONFIGURED,
            0,
            CONGESTED_AUDIO_BACKLOG_US,
        );
        assert!(lowered < CONFIGURED, "got {lowered}");
        // Just under the threshold must not.
        let held = next_target(
            CONFIGURED,
            CONFIGURED,
            CONFIGURED,
            0,
            CONGESTED_AUDIO_BACKLOG_US - 1,
        );
        assert_eq!(held, CONFIGURED);
    }

    #[test]
    fn never_falls_below_the_floor() {
        let mut target = CONFIGURED;
        for _ in 0..200 {
            target = next_target(target, CONFIGURED, 1, 100, 0);
        }
        assert_eq!(target, MINIMUM_TARGET_BITS_PER_SECOND);
    }

    #[test]
    fn targets_are_quantized_to_whole_steps() {
        // Whole steps are what keep ordinary jitter from re-opening the encoder every window.
        for candidate in [500_001, 1_234_567, 3_999_999] {
            let quantized = quantize_target(candidate, CONFIGURED);
            assert_eq!(
                quantized % RATE_STEP_BITS_PER_SECOND,
                0,
                "{candidate} quantized to {quantized}"
            );
            assert!(quantized <= candidate);
        }
        assert_eq!(quantize_target(CONFIGURED + 1, CONFIGURED), CONFIGURED);
        assert_eq!(
            quantize_target(0, CONFIGURED),
            MINIMUM_TARGET_BITS_PER_SECOND
        );
    }

    #[test]
    fn a_moderate_transport_share_holds_the_target_steady() {
        // Between the congested and uncongested thresholds the loop does nothing, so it does not
        // oscillate around the boundary.
        let held = next_target(2_000_000, CONFIGURED, 2_000_000, 5, 0);
        assert_eq!(held, 2_000_000);
    }

    #[test]
    fn observed_sends_accumulate_into_the_window() {
        let control = VideoRateControl::new(CONFIGURED);
        assert_eq!(control.target(), CONFIGURED);
        control.observe_send(
            1_024,
            SendPressure {
                transport: Duration::from_millis(5),
                records: 1,
                ..SendPressure::default()
            },
        );
        control.observe_send(
            2_048,
            SendPressure {
                transport: Duration::from_millis(5),
                records: 1,
                ..SendPressure::default()
            },
        );
        let snapshot = control.snapshot();
        assert_eq!(snapshot.transport, Duration::from_millis(10));
        assert_eq!(snapshot.configured_bits_per_second, CONFIGURED);
        assert_eq!(snapshot.adjustments, 0);
        // The window is not due yet, so no decision is taken.
        assert_eq!(control.poll(), None);
    }

    #[test]
    fn a_configured_rate_below_the_floor_is_raised_to_it() {
        let control = VideoRateControl::new(1_000);
        assert_eq!(control.target(), MINIMUM_TARGET_BITS_PER_SECOND);
        assert_eq!(control.configured(), MINIMUM_TARGET_BITS_PER_SECOND);
    }

    #[test]
    fn rate_helpers_handle_degenerate_windows() {
        assert_eq!(achieved_bits_per_second(0, Duration::ZERO), 0);
        assert_eq!(blocked_percent(Duration::ZERO, Duration::ZERO), 0);
        // Blocked longer than the window is still reported as fully blocked, not more.
        assert_eq!(
            blocked_percent(Duration::from_secs(10), Duration::from_secs(1)),
            100
        );
        assert_eq!(
            achieved_bits_per_second(1_000, Duration::from_secs(1)),
            8_000
        );
    }
}
