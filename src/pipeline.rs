//! Bounded media pipeline types and channel recovery.
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::revision::ChannelGeneration;

use crate::{ChannelEvent, RequestMetadata, Session, Track, TrackChannel, invalid_data, lock};

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
