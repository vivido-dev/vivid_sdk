use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vivid_protocol::anchor::{self, AnchorKey};
use vivid_protocol::media::{self, VideoPacket};
use vivid_protocol::messages::{
    self, AudioSourceConfig, Credits, ImageSourceConfig, SceneNodeConfig, SourceReady,
    VideoSourceConfig,
};
use vivid_protocol::wire::{Connection, ConnectionKind, ConnectionWriter, Endpoint, Record};
use vivid_protocol::{VIVID_MAJOR, VIVID_MINOR};

const SYNTHETIC_CREDITS: u64 = u64::MAX / 4;
const MAX_PENDING_CONTROL_RECORDS: usize = 4096;
const MAX_PENDING_DESKTOP_INPUT: usize = 512;
/// Cadence for opportunistic RTT sampling probes while the control connection is active.
const RTT_SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
const CONPTY_ANCHOR_TRANSPORT: &str = "conpty";

pub use vivid_protocol::messages::DisplayChanged as DisplayState;

/// A presenter's structured response to a connection-preface version mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRejectionError {
    pub attempted_version: (u8, u8),
    pub supported_version: Option<(u64, u64)>,
    pub fatal: bool,
}

impl std::fmt::Display for VersionRejectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "presenter rejected Vivid {}.{} (supported version: {})",
            self.attempted_version.0,
            self.attempted_version.1,
            self.supported_version
                .map(|(major, minor)| format!("{major}.{minor}"))
                .unwrap_or_else(|| "not reported".to_owned())
        )
    }
}

impl std::error::Error for VersionRejectionError {}

/// Allocation-free snapshot of coarse producer hot-path measurements.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotPathCounters {
    /// Media records successfully written.
    pub media_records_sent: u64,
    /// Bytes copied into SDK-owned media bodies.
    pub media_bytes_copied: u64,
    /// SDK-owned allocations made while constructing media bodies.
    pub media_allocations: u64,
    /// Cumulative time spent blocked for source credit.
    pub credit_wait_us: u64,
    /// Cumulative request-to-reply latency for correlated control records.
    pub control_reply_latency_us: u64,
    /// Number of samples in `control_reply_latency_us`.
    pub control_reply_samples: u64,
    /// Maximum concurrently pending correlated control requests.
    pub pending_request_high_water: usize,
    /// Maximum correlated replies waiting for their caller.
    pub reply_queue_high_water: usize,
    /// Maximum source events queued before their source was registered.
    pub source_event_queue_high_water: usize,
    /// Maximum pending desktop-input events.
    pub desktop_input_queue_high_water: usize,
}

#[derive(Debug, Default)]
struct HotPathCounterState {
    media_records_sent: AtomicU64,
    media_bytes_copied: AtomicU64,
    media_allocations: AtomicU64,
    credit_wait_us: AtomicU64,
    control_reply_latency_us: AtomicU64,
    control_reply_samples: AtomicU64,
    pending_request_high_water: AtomicUsize,
    reply_queue_high_water: AtomicUsize,
    source_event_queue_high_water: AtomicUsize,
    desktop_input_queue_high_water: AtomicUsize,
}

impl HotPathCounterState {
    fn snapshot(&self) -> HotPathCounters {
        HotPathCounters {
            media_records_sent: self.media_records_sent.load(Ordering::Relaxed),
            media_bytes_copied: self.media_bytes_copied.load(Ordering::Relaxed),
            media_allocations: self.media_allocations.load(Ordering::Relaxed),
            credit_wait_us: self.credit_wait_us.load(Ordering::Relaxed),
            control_reply_latency_us: self.control_reply_latency_us.load(Ordering::Relaxed),
            control_reply_samples: self.control_reply_samples.load(Ordering::Relaxed),
            pending_request_high_water: self.pending_request_high_water.load(Ordering::Relaxed),
            reply_queue_high_water: self.reply_queue_high_water.load(Ordering::Relaxed),
            source_event_queue_high_water: self
                .source_event_queue_high_water
                .load(Ordering::Relaxed),
            desktop_input_queue_high_water: self
                .desktop_input_queue_high_water
                .load(Ordering::Relaxed),
        }
    }

    fn record_media_sent(&self) {
        saturating_add(&self.media_records_sent, 1);
    }

    fn record_media_work(&self, allocations: u64, bytes_copied: u64) {
        saturating_add(&self.media_allocations, allocations);
        saturating_add(&self.media_bytes_copied, bytes_copied);
    }

    fn record_credit_wait(&self, started: Option<Instant>) {
        if let Some(started) = started {
            saturating_add(&self.credit_wait_us, duration_us(started.elapsed()));
        }
    }

    fn record_control_reply(&self, elapsed: Duration) {
        saturating_add(&self.control_reply_latency_us, duration_us(elapsed));
        saturating_add(&self.control_reply_samples, 1);
    }
}

fn saturating_add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

fn update_high_water(counter: &AtomicUsize, value: usize) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (value > current).then_some(value)
    });
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Connection and feature policy for a Vivid producer. Token-bearing values deliberately do not
/// implement `Debug` so an application cannot accidentally log the presenter capability.
pub struct ProducerConfig {
    pub endpoint: Option<String>,
    pub bulk_endpoint: Option<String>,
    pub token: Option<String>,
    pub dry_run: bool,
    pub trace_dir: Option<PathBuf>,
    pub verbose: bool,
    pub producer: String,
    pub producer_version: String,
    pub required_features: Vec<u64>,
    pub optional_features: Vec<u64>,
    /// Permit one explicit retry on a fresh connection after a typed version rejection.
    ///
    /// Disabled by default by all SDK integrations. The SDK retries only versions for which it
    /// retains a complete negotiation implementation.
    pub allow_version_retry: bool,
}

impl ProducerConfig {
    pub fn is_dry_run(&self) -> bool {
        self.dry_run || self.trace_dir.is_some()
    }

    pub fn validate(&self) -> io::Result<()> {
        if !self.is_dry_run() && self.endpoint.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "VIVID_ENDPOINT is not set",
            ));
        }
        if !self.is_dry_run() && self.token.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VIVID_TOKEN is not set",
            ));
        }
        if self.producer.is_empty() || self.producer.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Vivid producer name must contain 1 to 64 bytes",
            ));
        }
        if self.producer_version.is_empty() || self.producer_version.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Vivid producer version must contain 1 to 64 bytes",
            ));
        }
        validate_feature_ids("required", &self.required_features)?;
        validate_feature_ids("optional", &self.optional_features)?;
        if self
            .required_features
            .iter()
            .any(|feature| self.optional_features.binary_search(feature).is_ok())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "required and optional Vivid feature sets overlap",
            ));
        }
        Ok(())
    }
}

fn validate_feature_ids(description: &str, features: &[u64]) -> io::Result<()> {
    if features.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{description} Vivid feature IDs must be strictly increasing"),
        ));
    }
    Ok(())
}

/// Supplies a borrowed portable Vivid video configuration without coupling the producer to a
/// particular demuxer or encoder crate.
pub trait VideoConfig {
    fn vivid_video_config(&self, source_id: u64) -> VideoSourceConfig<'_>;
}

/// Owned video configuration used by live encoders such as Veston.
#[derive(Clone, Debug)]
pub struct VideoSourceSpec {
    pub codec: String,
    pub packetization: String,
    pub extradata: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub profile: i32,
    pub level: i32,
    pub bitrate: i64,
    pub color_primaries: u64,
    pub transfer: u64,
    pub matrix: u64,
    pub range: u64,
    pub sar_num: u32,
    pub sar_den: u32,
    pub max_access_unit_bytes: u32,
    /// Optional RFC 6381 codec string (`decoder-description-v1`); the session scrubs it when the
    /// presenter did not accept the feature.
    pub codec_string: Option<String>,
    /// Optional ISO-BMFF decoder configuration box body (avcC/hvcC/vpcC/av1C); scrubbed with
    /// [`VideoSourceSpec::codec_string`].
    pub decoder_config: Option<Vec<u8>>,
}

impl VideoConfig for VideoSourceSpec {
    fn vivid_video_config(&self, source_id: u64) -> VideoSourceConfig<'_> {
        VideoSourceConfig {
            source_id,
            codec: &self.codec,
            packetization: &self.packetization,
            extradata: &self.extradata,
            width: self.width,
            height: self.height,
            profile: self.profile,
            level: self.level,
            bitrate: self.bitrate,
            color_primaries: self.color_primaries,
            transfer: self.transfer,
            matrix: self.matrix,
            range: self.range,
            sar_num: self.sar_num,
            sar_den: self.sar_den,
            max_access_unit_bytes: self.max_access_unit_bytes,
            codec_string: self.codec_string.as_deref(),
            decoder_config: self.decoder_config.as_deref(),
        }
    }
}

pub trait AudioConfig {
    fn vivid_audio_config(
        &self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
    ) -> AudioSourceConfig<'_>;
}

#[derive(Clone, Debug)]
pub struct AudioSourceSpec {
    pub codec: String,
    pub packetization: String,
    pub extradata: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u64,
    pub bitrate: i64,
    pub max_access_unit_bytes: u32,
    /// Optional RFC 6381 codec string (`decoder-description-v1`); the session scrubs it when the
    /// presenter did not accept the feature.
    pub codec_string: Option<String>,
}

impl AudioConfig for AudioSourceSpec {
    fn vivid_audio_config(
        &self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
    ) -> AudioSourceConfig<'_> {
        AudioSourceConfig {
            source_id,
            linked_video_source_id,
            codec: &self.codec,
            packetization: &self.packetization,
            extradata: &self.extradata,
            sample_rate: self.sample_rate,
            channels: self.channels,
            channel_mask: self.channel_mask,
            bitrate: self.bitrate,
            max_access_unit_bytes: self.max_access_unit_bytes,
            codec_string: self.codec_string.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    Visibility(bool),
    NeedKeyframe(u32),
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopInputEvent {
    Key {
        usage: u16,
        pressed: bool,
    },
    PointerMotion {
        source_id: u64,
        x: u32,
        y: u32,
    },
    PointerButton {
        source_id: u64,
        button: u8,
        pressed: bool,
    },
    PointerAxis {
        source_id: u64,
        horizontal_120: i32,
        vertical_120: i32,
    },
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneNode {
    pub id: u64,
    pub source_id: u64,
}

#[derive(Debug)]
pub struct SourceHandle {
    pub id: u64,
    ticket: Vec<u8>,
    record_limit: u64,
    state: Arc<SourceSync>,
    counters: Arc<HotPathCounterState>,
    last_record_sequence: u64,
    attachment_generation: u64,
    acknowledged_credit_returns: u64,
    observed_visible: bool,
    reported_lost: bool,
}

/// A cloneable local wake-up handle for a source-specific media worker. Cancelling a source does
/// not write a protocol record; it only interrupts local credit waits so session shutdown cannot
/// deadlock behind a presenter that has stopped granting credit.
#[derive(Clone)]
pub struct SourceCancellation {
    state: Arc<SourceSync>,
}

impl SourceCancellation {
    pub fn cancel(&self, reason: impl Into<String>) {
        let mut state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.mark_lost(reason);
        self.state.changed.notify_all();
    }
}

#[derive(Debug)]
struct SourceRuntime {
    credits: messages::CreditLedger,
    visible: bool,
    visible_reasons: u64,
    need_keyframe_epoch: Option<u32>,
    lost: Option<String>,
    credit_returns: u64,
}

impl SourceRuntime {
    fn mark_lost(&mut self, reason: impl Into<String>) {
        self.credits.mark_lost();
        if self.lost.is_none() {
            self.lost = Some(reason.into());
        }
    }
}

#[derive(Debug)]
struct SourceSync {
    state: Mutex<SourceRuntime>,
    changed: Condvar,
}

struct DispatcherState {
    replies: HashMap<u64, Record>,
    pending_requests: HashMap<u64, Instant>,
    sources: HashMap<u64, Arc<SourceSync>>,
    pending_source_events: HashMap<u64, VecDeque<Record>>,
    pending_source_event_count: usize,
    desktop_input_enabled: bool,
    desktop_input: VecDeque<DesktopInputEvent>,
    anchors: HashSet<u64>,
    display: DisplayState,
    closed: Option<String>,
    last_inbound: Instant,
    last_probe_sent: Option<Instant>,
    unanswered_probes: u8,
    next_ping_id: u64,
    pending_pings: HashMap<u64, Instant>,
    /// Last RTT sampling probe. Sampling probes are independent of the idle liveness probes:
    /// they never advance `unanswered_probes` or `last_probe_sent`, so they cannot change
    /// disconnect detection.
    last_rtt_probe: Option<Instant>,
    rtt_us: Option<u64>,
}

struct DispatcherShared {
    state: Mutex<DispatcherState>,
    changed: Condvar,
    counters: Arc<HotPathCounterState>,
}

struct ControlDispatcher {
    writer: ConnectionWriter,
    shared: Arc<DispatcherShared>,
}

enum ClientControl {
    Direct(Connection),
    Live(ControlDispatcher),
}

impl ControlDispatcher {
    fn start(
        connection: Connection,
        display: DisplayState,
        desktop_input_enabled: bool,
        counters: Arc<HotPathCounterState>,
    ) -> io::Result<Self> {
        let (mut reader, writer) = connection.split()?;
        let shared = Arc::new(DispatcherShared {
            state: Mutex::new(DispatcherState {
                replies: HashMap::new(),
                pending_requests: HashMap::new(),
                sources: HashMap::new(),
                pending_source_events: HashMap::new(),
                pending_source_event_count: 0,
                desktop_input_enabled,
                desktop_input: VecDeque::new(),
                anchors: HashSet::new(),
                display,
                closed: None,
                last_inbound: Instant::now(),
                last_probe_sent: None,
                unanswered_probes: 0,
                next_ping_id: u64::MAX,
                pending_pings: HashMap::new(),
                last_rtt_probe: None,
                rtt_us: None,
            }),
            changed: Condvar::new(),
            counters,
        });
        let reader_shared = shared.clone();
        let reader_writer = writer.clone();
        thread::Builder::new()
            .name("vivid-sdk-control".into())
            .spawn(move || {
                loop {
                    let record = match reader.read_record() {
                        Ok(record) => record,
                        Err(error) => {
                            close_dispatcher(&reader_shared, error.to_string());
                            break;
                        }
                    };
                    {
                        let mut state = reader_shared
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.last_inbound = Instant::now();
                        state.last_probe_sent = None;
                        state.unanswered_probes = 0;
                        if record.record_type != messages::PONG {
                            state.pending_pings.clear();
                        }
                    }
                    if record.record_type == messages::PING {
                        let response =
                            messages::decode_control(&record.body).and_then(|envelope| {
                                if record.object_id != 0 || envelope.request_id == 0 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Vivid PING is not a correlated session-level request",
                                    ));
                                }
                                reader_writer.write_record(
                                    messages::PONG,
                                    0,
                                    0,
                                    &messages::ok(envelope.request_id),
                                )
                            });
                        if let Err(error) = response {
                            close_dispatcher(&reader_shared, error.to_string());
                            break;
                        }
                        continue;
                    }

                    let mut state = reader_shared
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let routed = match record.record_type {
                        messages::CREDIT
                        | messages::VISIBILITY
                        | messages::NEED_KEYFRAME
                        | messages::SOURCE_LOST => {
                            if let Some(source) = state.sources.get(&record.object_id) {
                                apply_source_record(source, &record)
                            } else if state.pending_source_event_count
                                >= MAX_PENDING_CONTROL_RECORDS
                            {
                                Err(io::Error::new(
                                    io::ErrorKind::OutOfMemory,
                                    "Vivid pending source-event queue exceeded its bound",
                                ))
                            } else {
                                state
                                    .pending_source_events
                                    .entry(record.object_id)
                                    .or_default()
                                    .push_back(record);
                                state.pending_source_event_count += 1;
                                update_high_water(
                                    &reader_shared.counters.source_event_queue_high_water,
                                    state.pending_source_event_count,
                                );
                                Ok(())
                            }
                        }
                        messages::DISPLAY_CHANGED => messages::parse_display_changed(&record.body)
                            .map(|display| state.display = display),
                        messages::KEY_INPUT
                        | messages::POINTER_MOTION
                        | messages::POINTER_BUTTON
                        | messages::POINTER_AXIS
                        | messages::INPUT_RESET => {
                            apply_desktop_input_record(&mut state, &record, &reader_shared.counters)
                        }
                        messages::ANCHOR_READY => {
                            messages::parse_anchor_event(&record.body).map(|anchor| {
                                state.anchors.insert(anchor);
                            })
                        }
                        messages::PONG => {
                            messages::request_id(&record.body).and_then(|request_id| {
                                if record.object_id != 0 || request_id == 0 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Vivid PONG is not a correlated session-level reply",
                                    ));
                                }
                                if let Some(sent) = state.pending_pings.remove(&request_id) {
                                    let sample = u64::try_from(sent.elapsed().as_micros())
                                        .unwrap_or(u64::MAX);
                                    state.rtt_us = Some(state.rtt_us.map_or(sample, |current| {
                                        current.saturating_mul(7).saturating_add(sample) / 8
                                    }));
                                }
                                Ok(())
                            })
                        }
                        _ => messages::request_id(&record.body).and_then(|request_id| {
                            if request_id == 0 {
                                return Ok(());
                            }
                            let Some(sent) = state.pending_requests.remove(&request_id) else {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "Vivid received a reply for an unknown request",
                                ));
                            };
                            reader_shared.counters.record_control_reply(sent.elapsed());
                            if state.replies.len() >= MAX_PENDING_CONTROL_RECORDS
                                || state.replies.insert(request_id, record).is_some()
                            {
                                return Err(io::Error::new(
                                    io::ErrorKind::OutOfMemory,
                                    "Vivid reply queue exceeded its bound or received a duplicate",
                                ));
                            }
                            update_high_water(
                                &reader_shared.counters.reply_queue_high_water,
                                state.replies.len(),
                            );
                            Ok(())
                        }),
                    };
                    if let Err(error) = routed {
                        state.closed.get_or_insert_with(|| error.to_string());
                    }
                    reader_shared.changed.notify_all();
                    if state.closed.is_some() {
                        for source in state.sources.values() {
                            source.changed.notify_all();
                        }
                        break;
                    }
                }
            })?;
        let heartbeat_shared = shared.clone();
        let heartbeat_writer = writer.clone();
        thread::Builder::new()
            .name("vivid-sdk-heartbeat".into())
            .spawn(move || {
                loop {
                    thread::sleep(Duration::from_secs(1));
                    let request = {
                        let mut state = heartbeat_shared
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if state.closed.is_some() {
                            break;
                        }
                        let now = Instant::now();
                        if now.duration_since(state.last_inbound) < Duration::from_secs(15)
                            || state.last_probe_sent.is_some_and(|sent| {
                                now.duration_since(sent) < Duration::from_secs(15)
                            })
                        {
                            // Not idle: opportunistically sample RTT so `play_at`'s
                            // RTT-derived minimum buffer works with a live estimate instead
                            // of falling back to static prebuffer guesses. Sampling probes
                            // stay out of the liveness accounting entirely, and at most one
                            // is outstanding; intervening records still discard the sample
                            // (the specification's clean-sample rule).
                            if state.pending_pings.is_empty()
                                && state.last_rtt_probe.is_none_or(|sent| {
                                    now.duration_since(sent) >= RTT_SAMPLE_INTERVAL
                                })
                            {
                                let request = state.next_ping_id;
                                state.next_ping_id = state.next_ping_id.saturating_sub(1);
                                state.last_rtt_probe = Some(now);
                                state.pending_pings.insert(request, now);
                                drop(state);
                                if let Err(error) = heartbeat_writer.write_record(
                                    messages::PING,
                                    0,
                                    0,
                                    &messages::ok(request),
                                ) {
                                    close_dispatcher(&heartbeat_shared, error.to_string());
                                    break;
                                }
                            }
                            continue;
                        }
                        if state.unanswered_probes >= 3 {
                            state.closed = Some("Vivid control heartbeat timed out".into());
                            heartbeat_shared.changed.notify_all();
                            for source in state.sources.values() {
                                source
                                    .state
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                                    .mark_lost("Vivid control heartbeat timed out");
                                source.changed.notify_all();
                            }
                            break;
                        }
                        let request = state.next_ping_id;
                        state.next_ping_id = state.next_ping_id.saturating_sub(1);
                        state.last_probe_sent = Some(now);
                        state.unanswered_probes = state.unanswered_probes.saturating_add(1);
                        state.pending_pings.insert(request, now);
                        request
                    };
                    if let Err(error) =
                        heartbeat_writer.write_record(messages::PING, 0, 0, &messages::ok(request))
                    {
                        close_dispatcher(&heartbeat_shared, error.to_string());
                        break;
                    }
                }
            })?;
        Ok(Self { writer, shared })
    }

    fn write_record(
        &self,
        record_type: u16,
        flags: u16,
        object_id: u64,
        body: &[u8],
    ) -> io::Result<()> {
        let request_id = messages::request_id(body)?;
        if request_id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "outbound Vivid request has request ID zero",
            ));
        }
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.pending_requests.len() >= MAX_PENDING_CONTROL_RECORDS
                || state.pending_requests.contains_key(&request_id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "Vivid pending request bound exceeded or request ID was reused",
                ));
            }
            state.pending_requests.insert(request_id, Instant::now());
            update_high_water(
                &self.shared.counters.pending_request_high_water,
                state.pending_requests.len(),
            );
        }
        if let Err(error) = self
            .writer
            .write_record(record_type, flags, object_id, body)
        {
            self.shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pending_requests
                .remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    fn wait_reply(
        &self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
    ) -> io::Result<Record> {
        self.wait_reply_until(request_id, accepted, expected_object_id, None)
    }

    fn wait_reply_deadline(
        &self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
        deadline: Instant,
    ) -> io::Result<Record> {
        self.wait_reply_until(request_id, accepted, expected_object_id, Some(deadline))
    }

    fn wait_reply_until(
        &self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
        deadline: Option<Instant>,
    ) -> io::Result<Record> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(record) = state.replies.remove(&request_id) {
                if record.object_id != expected_object_id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "reply {request_id} has object {}, expected {expected_object_id}",
                            record.object_id
                        ),
                    ));
                }
                if record.record_type != messages::ERROR && !accepted.contains(&record.record_type)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "reply {request_id} was {}, expected one of {accepted:?}",
                            messages::name(record.record_type)
                        ),
                    ));
                }
                return Ok(record);
            }
            if let Some(error) = &state.closed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
            }
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    state.pending_requests.remove(&request_id);
                    state.closed = Some(format!("Vivid request {request_id} timed out"));
                    self.shared.changed.notify_all();
                    for source in state.sources.values() {
                        let mut source_state = source
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        source_state.mark_lost("Vivid control request timed out");
                        source.changed.notify_all();
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Vivid control reply deadline expired",
                    ));
                }
                state = self
                    .shared
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            } else {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
    }

    fn register_source(&self, source_id: u64, credits: Credits) -> io::Result<Arc<SourceSync>> {
        let source = Arc::new(SourceSync {
            state: Mutex::new(SourceRuntime {
                credits: messages::CreditLedger::new(credits),
                visible: true,
                visible_reasons: 0,
                need_keyframe_epoch: None,
                lost: None,
                credit_returns: 0,
            }),
            changed: Condvar::new(),
        });
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.sources.insert(source_id, source.clone()).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Vivid source dispatcher state already exists",
            ));
        }
        if let Some(mut pending) = state.pending_source_events.remove(&source_id) {
            state.pending_source_event_count = state
                .pending_source_event_count
                .saturating_sub(pending.len());
            while let Some(record) = pending.pop_front() {
                apply_source_record(&source, &record)?;
            }
        }
        Ok(source)
    }

    fn wait_anchor(&self, anchor_id: u64) -> io::Result<()> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.anchors.remove(&anchor_id) {
                return Ok(());
            }
            if let Some(error) = &state.closed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Wait for `ANCHOR_READY` until the deadline. Returns false when the presenter has not
    /// confirmed the anchor in time.
    fn wait_anchor_deadline(&self, anchor_id: u64, deadline: Instant) -> io::Result<bool> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.anchors.remove(&anchor_id) {
                return Ok(true);
            }
            if let Some(error) = &state.closed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn display_generation(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .display
            .display_generation
    }

    fn display_state(&self) -> DisplayState {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .display
    }

    fn adjusted_minimum_buffer(&self, requested_us: u64) -> u64 {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        messages::minimum_buffer_for_rtt(requested_us, state.rtt_us)
    }

    fn take_desktop_input(&self) -> io::Result<Option<DesktopInputEvent>> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(state.desktop_input.front(), Some(DesktopInputEvent::Reset)) {
            return Ok(state.desktop_input.pop_front());
        }
        if let Some(error) = &state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
        }
        Ok(state.desktop_input.pop_front())
    }
}

impl Drop for ControlDispatcher {
    fn drop(&mut self) {
        close_dispatcher(&self.shared, "Vivid control dispatcher closed".into());
    }
}

fn close_dispatcher(shared: &DispatcherShared, message: String) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.desktop_input_enabled {
        state.desktop_input.clear();
        state.desktop_input.push_back(DesktopInputEvent::Reset);
        update_high_water(&shared.counters.desktop_input_queue_high_water, 1);
    }
    state.closed.get_or_insert(message);
    shared.changed.notify_all();
    for source in state.sources.values() {
        source
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mark_lost("Vivid control connection closed");
        source.changed.notify_all();
    }
}

fn apply_source_record(source: &SourceSync, record: &Record) -> io::Result<()> {
    let mut state = source
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match record.record_type {
        messages::CREDIT => {
            let added = messages::parse_credit(&record.body)?;
            state.credits.grant(added)?;
            state.credit_returns = state.credit_returns.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "credit return counter overflow")
            })?;
        }
        messages::VISIBILITY => {
            let visibility = messages::parse_visibility(&record.body)?;
            state.visible = visibility.visible;
            state.visible_reasons = visibility.reasons;
        }
        messages::NEED_KEYFRAME => {
            let request = messages::parse_need_keyframe(&record.body)?;
            if request.source_id != record.object_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NEED_KEYFRAME source/object ID mismatch",
                ));
            }
            state.need_keyframe_epoch = Some(request.minimum_epoch);
        }
        messages::SOURCE_LOST => {
            state.mark_lost(source_lost_error(record)?.to_string());
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "record is not a source event",
            ));
        }
    }
    source.changed.notify_all();
    Ok(())
}

fn apply_desktop_input_record(
    state: &mut DispatcherState,
    record: &Record,
    counters: &HotPathCounterState,
) -> io::Result<()> {
    let event = decode_desktop_input_record(state.desktop_input_enabled, record)?;
    let result = push_desktop_input(&mut state.desktop_input, event);
    update_high_water(
        &counters.desktop_input_queue_high_water,
        state.desktop_input.len(),
    );
    result
}

fn decode_desktop_input_record(
    desktop_input_enabled: bool,
    record: &Record,
) -> io::Result<DesktopInputEvent> {
    if !desktop_input_enabled {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "desktop input was not negotiated",
        ));
    }
    Ok(match record.record_type {
        messages::KEY_INPUT => {
            if record.object_id != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "KEY_INPUT is not session-level",
                ));
            }
            let input = messages::parse_key_input(&record.body)?;
            DesktopInputEvent::Key {
                usage: input.usage,
                pressed: input.pressed,
            }
        }
        messages::POINTER_MOTION => {
            let input = messages::parse_pointer_motion(&record.body)?;
            if record.object_id != input.source_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "POINTER_MOTION source/object ID mismatch",
                ));
            }
            DesktopInputEvent::PointerMotion {
                source_id: input.source_id,
                x: input.x,
                y: input.y,
            }
        }
        messages::POINTER_BUTTON => {
            let input = messages::parse_pointer_button(&record.body)?;
            if record.object_id != input.source_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "POINTER_BUTTON source/object ID mismatch",
                ));
            }
            DesktopInputEvent::PointerButton {
                source_id: input.source_id,
                button: input.button,
                pressed: input.pressed,
            }
        }
        messages::POINTER_AXIS => {
            let input = messages::parse_pointer_axis(&record.body)?;
            if record.object_id != input.source_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "POINTER_AXIS source/object ID mismatch",
                ));
            }
            DesktopInputEvent::PointerAxis {
                source_id: input.source_id,
                horizontal_120: input.horizontal_120,
                vertical_120: input.vertical_120,
            }
        }
        messages::INPUT_RESET => {
            if record.object_id != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "INPUT_RESET is not session-level",
                ));
            }
            messages::parse_input_reset(&record.body)?;
            DesktopInputEvent::Reset
        }
        _ => unreachable!("caller routes only desktop input records"),
    })
}

fn push_desktop_input(
    queue: &mut VecDeque<DesktopInputEvent>,
    event: DesktopInputEvent,
) -> io::Result<()> {
    if event == DesktopInputEvent::Reset {
        queue.clear();
        queue.push_back(event);
        return Ok(());
    }
    if let DesktopInputEvent::PointerMotion { source_id, .. } = event
        && matches!(
            queue.back(),
            Some(DesktopInputEvent::PointerMotion {
                source_id: pending,
                ..
            }) if *pending == source_id
        )
    {
        *queue.back_mut().expect("matched queue tail") = event;
        return Ok(());
    }
    if queue.len() >= MAX_PENDING_DESKTOP_INPUT {
        queue.clear();
        queue.push_back(DesktopInputEvent::Reset);
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "desktop input queue exceeded its bound",
        ));
    }
    queue.push_back(event);
    Ok(())
}

impl ClientControl {
    fn write_record(
        &mut self,
        record_type: u16,
        flags: u16,
        object_id: u64,
        body: &[u8],
    ) -> io::Result<()> {
        match self {
            Self::Direct(connection) => connection
                .write_record(record_type, flags, object_id, body)
                .map(|_| ()),
            Self::Live(dispatcher) => dispatcher.write_record(record_type, flags, object_id, body),
        }
    }

    fn wait_reply(
        &self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
    ) -> io::Result<Record> {
        match self {
            Self::Live(dispatcher) => {
                dispatcher.wait_reply(request_id, accepted, expected_object_id)
            }
            Self::Direct(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "trace/dry-run control connections have no replies",
            )),
        }
    }

    fn wait_reply_deadline(
        &self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
        deadline: Instant,
    ) -> io::Result<Record> {
        match self {
            Self::Live(dispatcher) => {
                dispatcher.wait_reply_deadline(request_id, accepted, expected_object_id, deadline)
            }
            Self::Direct(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "trace/dry-run control connections have no replies",
            )),
        }
    }

    fn register_source(&self, source_id: u64, credits: Credits) -> io::Result<Arc<SourceSync>> {
        match self {
            Self::Live(dispatcher) => dispatcher.register_source(source_id, credits),
            Self::Direct(_) => Ok(Arc::new(SourceSync {
                state: Mutex::new(SourceRuntime {
                    credits: messages::CreditLedger::new(credits),
                    visible: true,
                    visible_reasons: 0,
                    need_keyframe_epoch: None,
                    lost: None,
                    credit_returns: 0,
                }),
                changed: Condvar::new(),
            })),
        }
    }

    fn display_generation(&self, fallback: u64) -> u64 {
        match self {
            Self::Live(dispatcher) => dispatcher.display_generation(),
            Self::Direct(_) => fallback,
        }
    }

    fn display_state(&self, fallback: DisplayState) -> DisplayState {
        match self {
            Self::Live(dispatcher) => dispatcher.display_state(),
            Self::Direct(_) => fallback,
        }
    }

    fn adjusted_minimum_buffer(&self, requested_us: u64) -> u64 {
        match self {
            Self::Live(dispatcher) => dispatcher.adjusted_minimum_buffer(requested_us),
            Self::Direct(_) => requested_us,
        }
    }

    fn wait_anchor(&self, anchor_id: u64) -> io::Result<()> {
        match self {
            Self::Live(dispatcher) => dispatcher.wait_anchor(anchor_id),
            Self::Direct(_) => Ok(()),
        }
    }

    fn wait_anchor_deadline(&self, anchor_id: u64, deadline: Instant) -> io::Result<bool> {
        match self {
            Self::Live(dispatcher) => dispatcher.wait_anchor_deadline(anchor_id, deadline),
            Self::Direct(_) => Ok(true),
        }
    }
}

pub struct MediaChannel {
    connection: Connection,
    source_id: u64,
}

impl MediaChannel {
    fn send_parts(
        &mut self,
        source: &mut SourceHandle,
        dry_run: bool,
        record_type: u16,
        parts: &[&[u8]],
        interrupt_for_events: bool,
    ) -> io::Result<u64> {
        let bytes = media_body_len(parts)?;
        if bytes > source.record_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Vivid media record is {bytes} bytes, exceeding source {}'s {}-byte limit",
                    source.id, source.record_limit
                ),
            ));
        }
        source.consume_credits(bytes, dry_run, interrupt_for_events)?;
        let sequence = self
            .connection
            .write_record_parts(record_type, 0, self.source_id, parts)?;
        source.last_record_sequence = sequence;
        source.counters.record_media_sent();
        Ok(sequence)
    }
}

fn media_body_len(parts: &[&[u8]]) -> io::Result<u64> {
    parts.iter().try_fold(0_u64, |length, part| {
        let part_length = u64::try_from(part.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "media body is too large"))?;
        length
            .checked_add(part_length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "media body is too large"))
    })
}

/// A source-specific media writer. It owns no control-session lock, so independent audio and
/// video writers can block on their own credits without blocking each other.
pub struct MediaSender {
    source: SourceHandle,
    channel: MediaChannel,
    dry_run: bool,
    raster_zstd: bool,
}

impl MediaSender {
    pub fn source_id(&self) -> u64 {
        self.source.id
    }

    pub fn source(&self) -> &SourceHandle {
        &self.source
    }

    pub fn source_mut(&mut self) -> &mut SourceHandle {
        &mut self.source
    }

    pub fn take_event(&mut self) -> Option<SourceEvent> {
        self.source.take_event()
    }

    pub fn send_video(&mut self, packet: VideoPacket<'_>) -> io::Result<()> {
        let prefix = media::video_packet_prefix(&packet)?;
        self.send_record_parts(
            messages::VIDEO_PACKET,
            &[prefix.as_slice(), packet.data],
            true,
        )
    }

    pub fn send_audio(&mut self, packet: media::AudioPacket<'_>) -> io::Result<()> {
        let prefix = media::audio_packet_prefix(&packet)?;
        // Audio sources have no scene node of their own. Linked audio therefore commonly reports
        // false visibility even while its video is visible, and standalone audio is never placed
        // in the scene at all. VISIBILITY is advisory, so it must not interrupt audio credit
        // waits and turn the linked master clock into a packet-dropping loop.
        self.send_record_parts(
            messages::AUDIO_PACKET,
            &[prefix.as_slice(), packet.data],
            false,
        )
    }

    /// Send one complete RGBA8 raster frame and wait until the presenter returns media credit.
    ///
    /// This owned-sender form preserves source-scoped backpressure without requiring callers to
    /// retain a mutable [`ProducerSession`] alongside the media worker.
    pub fn send_raster(
        &mut self,
        epoch: u32,
        frame_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> io::Result<()> {
        let raw_prefix =
            media::raster_full_frame_prefix(epoch, frame_id, width, height, rgba.len())?;
        let raw_parts = [raw_prefix.as_slice(), rgba];
        let raw_length = media_body_len(&raw_parts)?;
        if self.raster_zstd {
            let compressed = media::raster_frame_body_with_compression(
                epoch, frame_id, width, height, rgba, true,
            )?;
            self.source
                .counters
                .record_media_work(2, u64::try_from(compressed.len()).unwrap_or(u64::MAX));
            if u64::try_from(compressed.len()).unwrap_or(u64::MAX) < raw_length {
                self.send_one_shot_parts(messages::RASTER_FRAME, &[compressed.as_slice()])
            } else {
                self.send_one_shot_parts(messages::RASTER_FRAME, &raw_parts)
            }
        } else {
            self.send_one_shot_parts(messages::RASTER_FRAME, &raw_parts)
        }
    }

    /// Send one complete encoded image and wait until the presenter returns media credit.
    pub fn send_image(&mut self, encoded: &[u8]) -> io::Result<()> {
        self.send_one_shot_parts(messages::IMAGE_DATA, &[encoded])
    }

    fn send_one_shot_parts(&mut self, record_type: u16, parts: &[&[u8]]) -> io::Result<()> {
        self.send_record_parts(record_type, parts, false)?;
        if self.dry_run {
            Ok(())
        } else {
            self.source.wait_for_credit_return()
        }
    }

    fn send_record_parts(
        &mut self,
        record_type: u16,
        parts: &[&[u8]],
        interrupt_for_events: bool,
    ) -> io::Result<()> {
        self.channel.send_parts(
            &mut self.source,
            self.dry_run,
            record_type,
            parts,
            interrupt_for_events,
        )?;
        Ok(())
    }
}

pub struct ProducerSession {
    control: ClientControl,
    counters: Arc<HotPathCounterState>,
    endpoint: Option<Endpoint>,
    bulk_endpoint: Option<Endpoint>,
    trace_dir: Option<PathBuf>,
    dry_run: bool,
    verbose: bool,
    next_request_id: u64,
    next_object_id: u64,
    root_context_id: u64,
    display: DisplayState,
    session_tag: [u8; 16],
    anchor_key: AnchorKey,
    accepted_features: Vec<u64>,
    unconfirmed_anchors: Vec<u64>,
    label: String,
}

impl ProducerSession {
    pub fn connect(config: &ProducerConfig) -> Result<Self, Box<dyn std::error::Error>> {
        config.validate()?;
        let counters = Arc::new(HotPathCounterState::default());
        let dry_run = config.is_dry_run();
        let endpoint = config
            .endpoint
            .as_deref()
            .map(Endpoint::parse)
            .transpose()?;
        let bulk_endpoint = config
            .bulk_endpoint
            .as_deref()
            .map(Endpoint::parse)
            .transpose()?;
        let mut control = if let Some(trace_dir) = &config.trace_dir {
            Connection::trace(&trace_dir.join("control.vivid"), ConnectionKind::Control)?
        } else if dry_run {
            Connection::sink(ConnectionKind::Control)?
        } else {
            Connection::open(
                endpoint.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint")
                })?,
                ConnectionKind::Control,
            )?
        };

        let dry_token = "00".repeat(32);
        let token = config.token.as_deref().unwrap_or(&dry_token);
        let token_bytes = anchor::decode_token(token)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let hello_request = 1;

        let (root_context_id, display, session_tag, accepted_features, control_limit) = if dry_run {
            send_hello(
                &mut control,
                config,
                token,
                hello_request,
                (VIVID_MAJOR, VIVID_MINOR),
            )?;
            if config.verbose {
                eprintln!(
                    "{}: dry-run HELLO (Vivid {}.{})",
                    config.producer, VIVID_MAJOR, VIVID_MINOR
                );
            }
            let accepted_features = messages::negotiate_features(
                &config.required_features,
                &config.optional_features,
                |_| true,
            )
            .expect("a universally supported feature set cannot fail negotiation");
            (
                1,
                DisplayState {
                    display_generation: 0,
                    viewport_width: 800,
                    viewport_height: 600,
                    grid_columns: 80,
                    grid_rows: 24,
                    cell_width: 10,
                    cell_height: 25,
                },
                [0; 16],
                accepted_features,
                vivid_protocol::CONTROL_MAX_RECORD_BODY,
            )
        } else {
            let mut version = (VIVID_MAJOR, VIVID_MINOR);
            let mut retried = false;
            let welcome = loop {
                send_hello(&mut control, config, token, hello_request, version)?;
                let record = read_negotiation_reply(&mut control, hello_request)?;
                if record.record_type == messages::WELCOME {
                    break messages::parse_welcome_for_version(
                        &record.body,
                        u64::from(version.0),
                        u64::from(version.1),
                    )?;
                }
                let rejection = messages::parse_error_reply(&record.body)?;
                let supported = rejection.supported_version.and_then(|(major, minor)| {
                    Some((u8::try_from(major).ok()?, u8::try_from(minor).ok()?))
                });
                if config.allow_version_retry
                    && !retried
                    && rejection.code == messages::ERROR_UNSUPPORTED_VERSION
                    && rejection.fatal
                    && supported == Some((1, 0))
                {
                    let retry_version = supported.unwrap();
                    eprintln!(
                        "{}: presenter rejected Vivid {}.{}; retrying once on a fresh connection with reported Vivid {}.{}",
                        config.producer, version.0, version.1, retry_version.0, retry_version.1
                    );
                    drop(control);
                    control = Connection::open_version(
                        endpoint.as_ref().ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint")
                        })?,
                        ConnectionKind::Control,
                        retry_version.0,
                        retry_version.1,
                    )?;
                    version = retry_version;
                    retried = true;
                    continue;
                }
                if rejection.code == messages::ERROR_UNSUPPORTED_VERSION {
                    return Err(VersionRejectionError {
                        attempted_version: version,
                        supported_version: rejection.supported_version,
                        fatal: rejection.fatal,
                    }
                    .into());
                }
                return Err(io::Error::other(format!(
                    "presenter rejected HELLO with error {}: {}",
                    rejection.code, rejection.diagnostic
                ))
                .into());
            };
            let accepted_features = messages::negotiate_features(
                &config.required_features,
                &config.optional_features,
                |feature| welcome.accepted_features.contains(&feature),
            )
            .map_err(|required| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("presenter did not accept required Vivid feature {required}"),
                )
            })?;
            if accepted_features != welcome.accepted_features {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WELCOME accepted features are unsorted, duplicated, or were not offered",
                )
                .into());
            }
            let session_tag: [u8; 16] =
                welcome.session_tag.as_slice().try_into().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "WELCOME session tag is not 128 bits",
                    )
                })?;
            if config.verbose {
                eprintln!(
                    "{}: session={} tag={} bytes root={} grid={}x{} generation={}",
                    config.producer,
                    welcome.session_id,
                    welcome.session_tag.len(),
                    welcome.root_context_id,
                    welcome.grid_columns,
                    welcome.grid_rows,
                    welcome.display_generation
                );
            }
            (
                welcome.root_context_id,
                DisplayState {
                    display_generation: welcome.display_generation,
                    viewport_width: welcome.viewport_width,
                    viewport_height: welcome.viewport_height,
                    grid_columns: u32::try_from(welcome.grid_columns).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "WELCOME grid width exceeds u32")
                    })?,
                    grid_rows: u32::try_from(welcome.grid_rows).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "WELCOME grid height exceeds u32",
                        )
                    })?,
                    cell_width: welcome.cell_width,
                    cell_height: welcome.cell_height,
                },
                session_tag,
                accepted_features,
                welcome.maximum_control_body,
            )
        };
        control.set_send_body_limit(control_limit)?;
        let control = if dry_run {
            ClientControl::Direct(control)
        } else {
            ClientControl::Live(ControlDispatcher::start(
                control,
                display,
                accepted_features.contains(&messages::FEATURE_DESKTOP_INPUT_V1),
                counters.clone(),
            )?)
        };
        let anchor_key = anchor::derive_key(&token_bytes, &session_tag);

        Ok(Self {
            control,
            counters,
            endpoint,
            bulk_endpoint,
            trace_dir: config.trace_dir.clone(),
            dry_run,
            verbose: config.verbose,
            next_request_id: hello_request,
            next_object_id: 0,
            root_context_id,
            display,
            session_tag,
            anchor_key,
            accepted_features,
            unconfirmed_anchors: Vec::new(),
            label: config.producer.clone(),
        })
    }

    pub fn hot_path_counters(&self) -> HotPathCounters {
        self.counters.snapshot()
    }

    pub fn allocate_id(&mut self) -> io::Result<u64> {
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Vivid object ID space exhausted"))?;
        Ok(self.next_object_id)
    }

    pub fn create_raster_source(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
    ) -> io::Result<SourceHandle> {
        let request_id = self.request_id()?;
        let body = messages::create_raster_config(
            request_id,
            &messages::RasterSourceConfig {
                source_id,
                width,
                height,
                alpha_mode: messages::ALPHA_STRAIGHT,
                compression_mode: if self.supports(messages::FEATURE_RASTER_ZSTD_V1) {
                    messages::COMPRESSION_RAW_OR_ZSTD
                } else {
                    messages::COMPRESSION_NONE
                },
            },
        );
        self.control
            .write_record(messages::CREATE_RASTER, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "raster")
    }

    pub fn create_video_source<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
    ) -> io::Result<SourceHandle> {
        let request_id = self.request_id()?;
        let config = self.scrub_video_description(info.vivid_video_config(source_id));
        let body = messages::create_video(request_id, &config);
        self.control
            .write_record(messages::CREATE_VIDEO, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "video")
    }

    /// Drop `decoder-description-v1` fields when the presenter did not accept the feature; the
    /// specification forbids sending keys 21/22 (video) and 11 (audio) without acceptance.
    fn scrub_video_description<'a>(
        &self,
        mut config: VideoSourceConfig<'a>,
    ) -> VideoSourceConfig<'a> {
        if !self.supports(messages::FEATURE_DECODER_DESCRIPTION_V1) {
            config.codec_string = None;
            config.decoder_config = None;
        }
        config
    }

    fn scrub_audio_description<'a>(
        &self,
        mut config: AudioSourceConfig<'a>,
    ) -> AudioSourceConfig<'a> {
        if !self.supports(messages::FEATURE_DECODER_DESCRIPTION_V1) {
            config.codec_string = None;
        }
        config
    }

    pub fn create_audio_source<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
    ) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_AUDIO_ACCESS_UNIT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks audio-access-unit-v1",
            ));
        }
        let config = self
            .scrub_audio_description(info.vivid_audio_config(source_id, linked_video_source_id));
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::CREATE_AUDIO,
            0,
            source_id,
            &messages::create_audio(request_id, &config),
        )?;
        self.source_ready(request_id, source_id, "audio")
    }

    /// Create a linked video/audio pair in one ordered control flight. The video result remains
    /// usable when the presenter rejects the audio configuration.
    pub fn create_linked_av_sources<V, A>(
        &mut self,
        video_source_id: u64,
        video: &V,
        audio_source_id: u64,
        audio: &A,
    ) -> io::Result<(SourceHandle, io::Result<SourceHandle>)>
    where
        V: VideoConfig + ?Sized,
        A: AudioConfig + ?Sized,
    {
        if !self.supports(messages::FEATURE_AUDIO_ACCESS_UNIT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks audio-access-unit-v1",
            ));
        }
        let video_request = self.request_id()?;
        let audio_request = self.request_id()?;
        self.control.write_record(
            messages::CREATE_VIDEO,
            0,
            video_source_id,
            &messages::create_video(
                video_request,
                &self.scrub_video_description(video.vivid_video_config(video_source_id)),
            ),
        )?;
        self.control.write_record(
            messages::CREATE_AUDIO,
            0,
            audio_source_id,
            &messages::create_audio(
                audio_request,
                &self.scrub_audio_description(
                    audio.vivid_audio_config(audio_source_id, Some(video_source_id)),
                ),
            ),
        )?;
        let video = self.source_ready(video_request, video_source_id, "video")?;
        let audio = self.source_ready(audio_request, audio_source_id, "audio");
        Ok((video, audio))
    }

    #[allow(dead_code)] // Explicit diagnostic/conformance API; normal playback creates directly.
    pub fn probe_video_config<C: VideoConfig + ?Sized>(&mut self, info: &C) -> io::Result<bool> {
        let request = self.request_id()?;
        self.control.write_record(
            messages::PROBE_VIDEO_CONFIG,
            0,
            0,
            &messages::probe_video_config(
                request,
                &self.scrub_video_description(info.vivid_video_config(0)),
            ),
        )?;
        if self.dry_run {
            Ok(true)
        } else {
            let reply = self.wait_for_reply(request, &[messages::VIDEO_SUPPORT], 0)?;
            messages::parse_video_support(&reply.body)
        }
    }

    #[allow(dead_code)] // Explicit diagnostic/conformance API; normal playback creates directly.
    pub fn probe_audio_config<C: AudioConfig + ?Sized>(&mut self, info: &C) -> io::Result<bool> {
        let request = self.request_id()?;
        self.control.write_record(
            messages::PROBE_AUDIO_CONFIG,
            0,
            0,
            &messages::probe_audio_config(
                request,
                &self.scrub_audio_description(info.vivid_audio_config(0, None)),
            ),
        )?;
        if self.dry_run {
            Ok(true)
        } else {
            let reply = self.wait_for_reply(request, &[messages::AUDIO_SUPPORT], 0)?;
            messages::parse_audio_support(&reply.body)
        }
    }

    pub fn create_image_source(&mut self, config: &ImageSourceConfig) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_ENCODED_IMAGE_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks encoded-image-v1",
            ));
        }
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::CREATE_IMAGE,
            0,
            config.source_id,
            &messages::create_image(request_id, config),
        )?;
        self.source_ready(request_id, config.source_id, "image")
    }

    pub fn place_source(
        &mut self,
        source_id: u64,
        node_id: u64,
        anchor_id: Option<u64>,
        columns: u32,
        rows: u32,
    ) -> io::Result<()> {
        let (x, y) = if anchor_id.is_none() && !self.dry_run {
            crossterm::cursor::position()
                .map(|(column, row)| (i64::from(column) << 32, i64::from(row) << 32))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        self.create_scene_node(&SceneNodeConfig {
            node_id,
            source_id,
            context_id: self.root_context_id,
            x,
            y,
            width: i64::from(columns) << 32,
            height: i64::from(rows) << 32,
            text_layer: messages::TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH,
            z_index: 0,
            visible: true,
            anchor_id,
            clip: None,
        })?;
        if self.verbose {
            eprintln!(
                "{}: placed source {source_id} as node {node_id} in {columns}x{rows} cells",
                self.label
            );
        }
        Ok(())
    }

    pub fn root_context_id(&self) -> u64 {
        self.root_context_id
    }

    pub fn display_state(&self) -> DisplayState {
        self.control.display_state(self.display)
    }

    pub fn create_scene_node(&mut self, node: &SceneNodeConfig) -> io::Result<SceneNode> {
        self.transact_scene_node(messages::CREATE_NODE, node)?;
        Ok(SceneNode {
            id: node.node_id,
            source_id: node.source_id,
        })
    }

    pub fn update_scene_node(&mut self, node: &SceneNodeConfig) -> io::Result<SceneNode> {
        self.transact_scene_node(messages::UPDATE_NODE, node)?;
        Ok(SceneNode {
            id: node.node_id,
            source_id: node.source_id,
        })
    }

    pub fn delete_scene_node(&mut self, node_id: u64) -> io::Result<()> {
        let transaction_id = self.allocate_id()?;
        let begin_request = self.request_id()?;
        self.control.write_record(
            messages::BEGIN_TXN,
            0,
            0,
            &messages::begin_transaction(begin_request, transaction_id),
        )?;
        let node_request = self.request_id()?;
        self.control.write_record(
            messages::DELETE_NODE,
            0,
            node_id,
            &messages::delete_node(node_request, transaction_id, node_id),
        )?;
        self.commit_transaction(transaction_id, begin_request, node_request, node_id)
    }

    pub fn destroy_source(&mut self, source_id: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::DESTROY_SOURCE,
            0,
            source_id,
            &messages::destroy_source(request_id, source_id),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    fn transact_scene_node(&mut self, record_type: u16, node: &SceneNodeConfig) -> io::Result<()> {
        let transaction_id = self.allocate_id()?;
        let begin_request = self.request_id()?;
        self.control.write_record(
            messages::BEGIN_TXN,
            0,
            0,
            &messages::begin_transaction(begin_request, transaction_id),
        )?;
        let node_request = self.request_id()?;
        self.control.write_record(
            record_type,
            0,
            node.node_id,
            &messages::create_scene_node(node_request, transaction_id, node),
        )?;
        self.commit_transaction(transaction_id, begin_request, node_request, node.node_id)
    }

    fn commit_transaction(
        &mut self,
        transaction_id: u64,
        begin_request: u64,
        action_request: u64,
        action_object_id: u64,
    ) -> io::Result<()> {
        let mut stale_retries = 0;
        loop {
            let commit_request = self.request_id()?;
            self.control.write_record(
                messages::COMMIT_TXN,
                0,
                0,
                &messages::commit_transaction(
                    commit_request,
                    transaction_id,
                    self.control
                        .display_generation(self.display.display_generation),
                ),
            )?;
            if self.dry_run {
                return Ok(());
            }
            if stale_retries == 0 {
                self.wait_for_reply(begin_request, &[messages::OK], 0)?;
                self.wait_for_reply(action_request, &[messages::OK], action_object_id)?;
            }
            let record = self.wait_for_reply_raw(
                commit_request,
                &[messages::OK, messages::PRESENTED, messages::ERROR],
                0,
            )?;
            if record.record_type != messages::ERROR {
                return Ok(());
            }
            let error = messages::parse_error_reply(&record.body)?;
            if error.code == messages::ERROR_STALE_DISPLAY_GENERATION && stale_retries < 3 {
                stale_retries += 1;
                continue;
            }
            return Err(io::Error::other(format!(
                "presenter error {}: {}",
                error.code, error.diagnostic
            )));
        }
    }

    /// Insert an authenticated marker at the current terminal cursor. APC transports wait for the
    /// presenter to attach it before continuing. ConPTY uses a scanner-compatible envelope and
    /// permits the node and marker to arrive in either order because it can defer terminal output
    /// while the producer is blocked.
    pub fn create_text_anchor(&mut self) -> io::Result<Option<u64>> {
        if std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some() {
            return Ok(None);
        }
        if self.dry_run {
            return self.allocate_id().map(Some);
        }
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let anchor_id = u64::from_be_bytes(bytes);
        if anchor_id == 0 {
            return self.create_text_anchor();
        }

        let marker = anchor::encode_marker(&self.anchor_key, &self.session_tag, anchor_id)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let conpty_transport = uses_conpty_anchor_transport();
        let marker = marker_for_transport(marker, conpty_transport);
        let mut stdout = io::stdout().lock();
        stdout.write_all(marker.as_bytes())?;
        stdout.flush()?;
        drop(stdout);

        if conpty_transport {
            if self.verbose {
                eprintln!(
                    "{}: submitted asynchronous text anchor {anchor_id} over ConPTY transport",
                    self.label
                );
            }
            self.unconfirmed_anchors.push(anchor_id);
            return Ok(Some(anchor_id));
        }

        self.control.wait_anchor(anchor_id)?;
        Ok(Some(anchor_id))
    }

    pub fn open_media_channel(
        &self,
        source: &SourceHandle,
        kind: ConnectionKind,
    ) -> io::Result<MediaChannel> {
        let mut connection = if let Some(trace_dir) = &self.trace_dir {
            let label = match kind {
                ConnectionKind::Video => "video",
                ConnectionKind::Raster => "raster",
                ConnectionKind::Blob => "blob",
                ConnectionKind::Control => "control",
                ConnectionKind::LocalBuffer => "buffer",
                ConnectionKind::Audio => "audio",
            };
            Connection::trace(
                &trace_dir.join(format!("{label}-{}.vivid", source.id)),
                kind,
            )?
        } else if self.dry_run {
            Connection::sink(kind)?
        } else {
            let primary = self
                .endpoint
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint"))?;
            if let Some(bulk) = &self.bulk_endpoint {
                Connection::open(bulk, kind).or_else(|_| Connection::open(primary, kind))?
            } else {
                Connection::open(primary, kind)?
            }
        };
        connection.write_record(
            messages::ATTACH_CHANNEL,
            0,
            source.id,
            &messages::attach_channel(&source.ticket),
        )?;
        connection.set_send_body_limit(u32::try_from(source.record_limit).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "source body limit exceeds u32")
        })?)?;
        Ok(MediaChannel {
            connection,
            source_id: source.id,
        })
    }

    pub fn open_media_sender(
        &self,
        source: SourceHandle,
        kind: ConnectionKind,
    ) -> io::Result<MediaSender> {
        let channel = self.open_media_channel(&source, kind)?;
        Ok(MediaSender {
            source,
            channel,
            dry_run: self.dry_run,
            raster_zstd: self.supports(messages::FEATURE_RASTER_ZSTD_V1),
        })
    }

    pub fn send_raster_frame(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        epoch: u32,
        frame_id: u64,
        size: (u32, u32),
        rgba: &[u8],
    ) -> io::Result<()> {
        let (width, height) = size;
        let raw_prefix =
            media::raster_full_frame_prefix(epoch, frame_id, width, height, rgba.len())?;
        let raw_parts = [raw_prefix.as_slice(), rgba];
        let raw_length = media_body_len(&raw_parts)?;
        if self.supports(messages::FEATURE_RASTER_ZSTD_V1) {
            let compressed = media::raster_frame_body_with_compression(
                epoch, frame_id, width, height, rgba, true,
            )?;
            source
                .counters
                .record_media_work(2, u64::try_from(compressed.len()).unwrap_or(u64::MAX));
            if u64::try_from(compressed.len()).unwrap_or(u64::MAX) < raw_length {
                channel.send_parts(
                    source,
                    self.dry_run,
                    messages::RASTER_FRAME,
                    &[compressed.as_slice()],
                    false,
                )?;
            } else {
                channel.send_parts(
                    source,
                    self.dry_run,
                    messages::RASTER_FRAME,
                    &raw_parts,
                    false,
                )?;
            }
        } else {
            channel.send_parts(
                source,
                self.dry_run,
                messages::RASTER_FRAME,
                &raw_parts,
                false,
            )?;
        }
        self.wait_for_media_credit(source)
    }

    pub fn send_video_packet(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        packet: VideoPacket<'_>,
    ) -> io::Result<()> {
        let prefix = media::video_packet_prefix(&packet)?;
        channel.send_parts(
            source,
            self.dry_run,
            messages::VIDEO_PACKET,
            &[prefix.as_slice(), packet.data],
            false,
        )?;
        Ok(())
    }

    pub fn send_audio_packet(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        packet: media::AudioPacket<'_>,
    ) -> io::Result<()> {
        let prefix = media::audio_packet_prefix(&packet)?;
        channel.send_parts(
            source,
            self.dry_run,
            messages::AUDIO_PACKET,
            &[prefix.as_slice(), packet.data],
            false,
        )?;
        Ok(())
    }

    pub fn send_image_data(
        &mut self,
        source: &mut SourceHandle,
        channel: &mut MediaChannel,
        encoded: &[u8],
    ) -> io::Result<()> {
        channel.send_parts(
            source,
            self.dry_run,
            messages::IMAGE_DATA,
            &[encoded],
            false,
        )?;
        self.wait_for_media_credit(source)
    }

    pub fn supports(&self, feature: u64) -> bool {
        self.accepted_features.binary_search(&feature).is_ok()
    }

    pub fn take_desktop_input(&mut self) -> io::Result<Option<DesktopInputEvent>> {
        match &self.control {
            ClientControl::Live(dispatcher) => dispatcher.take_desktop_input(),
            ClientControl::Direct(_) => Ok(None),
        }
    }

    pub fn play_at(
        &mut self,
        source_id: u64,
        start_pts_us: i64,
        minimum_buffer_us: u64,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let minimum_buffer_us = self
            .control
            .adjusted_minimum_buffer(minimum_buffer_us)
            .min(500_000);
        let mut play = messages::PlayRequest::baseline(source_id, minimum_buffer_us);
        play.start_pts_us = start_pts_us;
        play.maximum_latency_us = play.maximum_latency_us.max(play.minimum_buffer_us);
        self.control.write_record(
            messages::PLAY,
            0,
            source_id,
            &messages::play_request(request_id, &play),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn eos(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::EOS,
            0,
            source_id,
            &messages::eos(request_id, source_id, epoch),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn drain(&mut self, source_id: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::DRAIN,
            0,
            source_id,
            &messages::drain(request_id, source_id),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn drain_with_timeout(&mut self, source_id: u64, timeout: Duration) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::DRAIN,
            0,
            source_id,
            &messages::drain(request_id, source_id),
        )?;
        if self.dry_run {
            return Ok(());
        }
        let record = self.control.wait_reply_deadline(
            request_id,
            &[messages::OK],
            source_id,
            Instant::now() + timeout,
        )?;
        if record.record_type == messages::ERROR {
            return Err(io::Error::other(messages::parse_error(&record.body)?));
        }
        Ok(())
    }

    pub fn pause(&mut self, source_id: u64) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::PAUSE,
            0,
            source_id,
            &messages::pause(request_id, source_id),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn flush(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::FLUSH,
            0,
            source_id,
            &messages::flush(request_id, source_id, epoch),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn wait_until_visible(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        source.wait_until_visible()
    }

    pub fn apply_pending_source_events(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        source.check_lost()
    }

    pub fn goodbye(&mut self) -> io::Result<()> {
        self.confirm_conpty_anchors();
        let request_id = self.request_id()?;
        self.control
            .write_record(messages::GOODBYE, 0, 0, &messages::goodbye(request_id))?;
        if !self.dry_run {
            let _ = self.wait_for_reply(request_id, &[messages::OK], 0)?;
        }
        Ok(())
    }

    /// ConPTY anchor markers travel through the terminal text path while GOODBYE travels on
    /// the control connection; nothing orders the two. A one-shot producer that disconnects
    /// before its marker reaches the presenter loses the anchored node, so give the presenter
    /// a bounded window to confirm outstanding anchors before saying goodbye.
    fn confirm_conpty_anchors(&mut self) {
        if self.dry_run || self.unconfirmed_anchors.is_empty() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        for anchor_id in std::mem::take(&mut self.unconfirmed_anchors) {
            match self.control.wait_anchor_deadline(anchor_id, deadline) {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "{}: warning: the presenter did not confirm text anchor {anchor_id}; \
                     the display may have discarded this submission",
                    self.label
                ),
                Err(_) => return,
            }
        }
    }

    pub fn verbose(&self, message: impl std::fmt::Display) {
        if self.verbose {
            eprintln!("{}: {message}", self.label);
        }
    }

    fn source_ready(
        &mut self,
        request_id: u64,
        source_id: u64,
        kind: &str,
    ) -> io::Result<SourceHandle> {
        let ready = if self.dry_run {
            let mut ticket = vec![0; 32];
            ticket[24..].copy_from_slice(&source_id.to_be_bytes());
            SourceReady {
                source_id,
                media_ticket: ticket,
                byte_credits: SYNTHETIC_CREDITS,
                packet_credits: SYNTHETIC_CREDITS,
                fragment_credits: SYNTHETIC_CREDITS,
                max_media_body: vivid_protocol::HARD_MAX_RECORD_BODY,
            }
        } else {
            let record = self.wait_for_reply(request_id, &[messages::SOURCE_READY], source_id)?;
            messages::parse_source_ready(&record.body)?
        };
        if ready.source_id != source_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "SOURCE_READY is for {}, expected {source_id}",
                    ready.source_id
                ),
            ));
        }
        if self.verbose {
            eprintln!(
                "{}: {kind} source {source_id} ready with {} byte / {} packet credits",
                self.label, ready.byte_credits, ready.packet_credits
            );
        }
        let credits = Credits {
            bytes: ready.byte_credits,
            packets: ready.packet_credits,
            fragments: ready.fragment_credits,
        };
        let state = self.control.register_source(source_id, credits)?;
        Ok(SourceHandle {
            id: source_id,
            ticket: ready.media_ticket,
            record_limit: u64::from(ready.max_media_body),
            state,
            counters: self.counters.clone(),
            last_record_sequence: 0,
            attachment_generation: 0,
            acknowledged_credit_returns: 0,
            observed_visible: true,
            reported_lost: false,
        })
    }

    /// Wait until the presenter has consumed a one-shot image or raster record before the media
    /// channel and control session are allowed to close.
    fn wait_for_media_credit(&mut self, source: &mut SourceHandle) -> io::Result<()> {
        if self.dry_run {
            return Ok(());
        }
        source.wait_for_credit_return()
    }

    fn request_id(&mut self) -> io::Result<u64> {
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Vivid request ID space exhausted"))?;
        Ok(self.next_request_id)
    }

    fn wait_for_reply(
        &mut self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
    ) -> io::Result<Record> {
        let record = self.wait_for_reply_raw(request_id, accepted, expected_object_id)?;
        if record.record_type == messages::ERROR {
            return Err(io::Error::other(messages::parse_error(&record.body)?));
        }
        Ok(record)
    }

    fn wait_for_ok(&mut self, request_id: u64, expected_object_id: u64) -> io::Result<()> {
        if !self.dry_run {
            self.wait_for_reply(request_id, &[messages::OK], expected_object_id)?;
        }
        Ok(())
    }

    fn wait_for_reply_raw(
        &mut self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
    ) -> io::Result<Record> {
        self.control
            .wait_reply(request_id, accepted, expected_object_id)
    }
}

fn uses_conpty_anchor_transport() -> bool {
    cfg!(windows)
        || configured_anchor_transport_is_conpty(
            std::env::var("VIVID_ANCHOR_TRANSPORT").ok().as_deref(),
        )
}

fn configured_anchor_transport_is_conpty(transport: Option<&str>) -> bool {
    transport == Some(CONPTY_ANCHOR_TRANSPORT)
}

fn marker_for_transport(marker: String, conpty_transport: bool) -> String {
    if conpty_transport {
        format!("{};VIVID-END", &marker[2..marker.len() - 2])
    } else {
        marker
    }
}

impl SourceHandle {
    pub fn last_record_sequence(&self) -> u64 {
        self.last_record_sequence
    }

    pub fn attachment_generation(&self) -> u64 {
        self.attachment_generation
    }

    pub fn hot_path_counters(&self) -> HotPathCounters {
        self.counters.snapshot()
    }

    pub fn cancellation(&self) -> SourceCancellation {
        SourceCancellation {
            state: self.state.clone(),
        }
    }

    pub fn take_event(&mut self) -> Option<SourceEvent> {
        let mut state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.reported_lost {
            if let Some(error) = &state.lost {
                self.reported_lost = true;
                return Some(SourceEvent::Lost(error.clone()));
            }
        }
        if let Some(epoch) = state.need_keyframe_epoch.take() {
            return Some(SourceEvent::NeedKeyframe(epoch));
        }
        if state.visible != self.observed_visible {
            self.observed_visible = state.visible;
            return Some(SourceEvent::Visibility(state.visible));
        }
        None
    }

    pub fn take_keyframe_request(&mut self) -> Option<u32> {
        self.state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .need_keyframe_epoch
            .take()
    }

    pub fn is_visible(&self) -> bool {
        self.state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .visible
    }

    /// Bitmask from the presenter's last `VISIBILITY` record: bit 0 set means the source's scene
    /// node does not intersect the viewport (off-screen geometry); bit 1 set means the presenter
    /// surface is not renderable (e.g. occluded). Zero when visible or never reported.
    pub fn visibility_reasons(&self) -> u64 {
        self.state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .visible_reasons
    }

    fn check_lost(&self) -> io::Result<()> {
        let state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = &state.lost {
            Err(io::Error::other(error.clone()))
        } else {
            Ok(())
        }
    }

    fn consume_credits(
        &self,
        bytes: u64,
        dry_run: bool,
        interrupt_for_events: bool,
    ) -> io::Result<()> {
        let mut state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut wait_started = None;
        loop {
            if let Some(error) = &state.lost {
                self.counters.record_credit_wait(wait_started);
                return Err(io::Error::other(error.clone()));
            }
            if interrupt_for_events && state.need_keyframe_epoch.is_some() {
                self.counters.record_credit_wait(wait_started);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "presenter requested a fresh keyframe",
                ));
            }
            if interrupt_for_events && !state.visible {
                self.counters.record_credit_wait(wait_started);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "source became invisible while waiting for media credit",
                ));
            }
            if state.credits.can_consume(bytes) {
                state.credits.consume(bytes)?;
                self.counters.record_credit_wait(wait_started);
                return Ok(());
            }
            if dry_run {
                self.counters.record_credit_wait(wait_started);
                return Err(io::Error::other("synthetic dry-run credits exhausted"));
            }
            wait_started.get_or_insert_with(Instant::now);
            state = self
                .state
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn wait_for_credit_return(&mut self) -> io::Result<()> {
        let mut state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut wait_started = None;
        loop {
            if let Some(error) = &state.lost {
                self.counters.record_credit_wait(wait_started);
                return Err(io::Error::other(error.clone()));
            }
            if state.credit_returns > self.acknowledged_credit_returns {
                self.acknowledged_credit_returns = state.credit_returns;
                self.counters.record_credit_wait(wait_started);
                return Ok(());
            }
            wait_started.get_or_insert_with(Instant::now);
            state = self
                .state
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn wait_until_visible(&self) -> io::Result<()> {
        let mut state = self
            .state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !state.visible {
            if let Some(error) = &state.lost {
                return Err(io::Error::other(error.clone()));
            }
            state = self
                .state
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        Ok(())
    }
}

fn send_hello(
    connection: &mut Connection,
    config: &ProducerConfig,
    token: &str,
    request_id: u64,
    version: (u8, u8),
) -> io::Result<()> {
    let body = messages::try_encode_hello_for_version(
        request_id,
        &messages::HelloConfig {
            minimum_major: u64::from(version.0),
            minimum_minor: u64::from(version.1),
            maximum_major: u64::from(version.0),
            maximum_minor: u64::from(version.1),
            token,
            producer: &config.producer,
            producer_version: &config.producer_version,
            required_features: &config.required_features,
            optional_features: &config.optional_features,
            maximum_record_body: vivid_protocol::CONTROL_MAX_RECORD_BODY,
            authentication_kind: messages::AUTHENTICATION_WINDOW_ROOT,
            preserved_fields: &[],
        },
        u64::from(version.0),
        u64::from(version.1),
    )?;
    connection
        .write_record(messages::HELLO, 0, 0, &body)
        .map(|_| ())
}

fn read_negotiation_reply(
    connection: &mut Connection,
    expected_request_id: u64,
) -> io::Result<Record> {
    loop {
        let record = connection.read_record()?;
        match record.record_type {
            messages::ERROR => return Ok(record),
            messages::DISPLAY_CHANGED => continue,
            messages::WELCOME if messages::request_id(&record.body)? == expected_request_id => {
                return Ok(record);
            }
            _ => continue,
        }
    }
}

fn source_lost_error(record: &Record) -> io::Result<io::Error> {
    let lost = messages::parse_source_lost(&record.body)?;
    if lost.source_id != record.object_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SOURCE_LOST object ID mismatch",
        ));
    }
    Ok(io::Error::other(format!(
        "Vivid source {} was lost ({}): {}",
        lost.source_id, lost.code, lost.diagnostic
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn version_rejection_record(major: u64, minor: u64) -> Vec<u8> {
        use vivid_protocol::cbor::{self, Value};
        use vivid_protocol::wire::{HEADER_SIZE, RecordHeader};

        let body = cbor::encode(&Value::Map(vec![
            (0, Value::Unsigned(0)),
            (
                3,
                Value::Map(vec![
                    (0, Value::Unsigned(messages::ERROR_UNSUPPORTED_VERSION)),
                    (1, Value::Unsigned(0)),
                    (
                        2,
                        Value::Map(vec![
                            (11, Value::Unsigned(major)),
                            (12, Value::Unsigned(minor)),
                        ]),
                    ),
                    (4, Value::Bool(true)),
                    (5, Value::Text("unsupported Vivid version".to_owned())),
                ]),
            ),
        ]))
        .unwrap();
        let mut record = Vec::with_capacity(HEADER_SIZE + body.len());
        record.extend_from_slice(
            &RecordHeader {
                body_length: body.len() as u32,
                record_type: messages::ERROR,
                flags: 0,
                object_id: 0,
                sequence: 1,
            }
            .encode(),
        );
        record.extend_from_slice(&body);
        record
    }

    fn producer_config(required_features: Vec<u64>, optional_features: Vec<u64>) -> ProducerConfig {
        ProducerConfig {
            endpoint: None,
            bulk_endpoint: None,
            token: None,
            dry_run: true,
            trace_dir: None,
            verbose: false,
            producer: "test".to_owned(),
            producer_version: "1".to_owned(),
            required_features,
            optional_features,
            allow_version_retry: false,
        }
    }

    #[test]
    fn producer_config_rejects_noncanonical_feature_sets() {
        assert!(!producer_config(Vec::new(), Vec::new()).allow_version_retry);
        let error = producer_config(vec![1, 3], vec![8, 7])
            .validate()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "optional Vivid feature IDs must be strictly increasing"
        );

        let error = producer_config(vec![1, 3], vec![3, 7])
            .validate()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "required and optional Vivid feature sets overlap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn version_retry_is_disabled_by_default_and_does_not_reconnect() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        use vivid_protocol::wire::PREFACE_SIZE;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("no-version-retry.sock");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping version retry socket test: {error}");
                return;
            }
            Err(error) => panic!("fake presenter bind failed: {error}"),
        };
        let server = thread::spawn(move || -> io::Result<usize> {
            let (mut stream, _) = listener.accept()?;
            let mut preface = [0; PREFACE_SIZE];
            stream.read_exact(&mut preface)?;
            assert_eq!(&preface[4..6], &[VIVID_MAJOR, VIVID_MINOR]);
            stream.write_all(&vivid_protocol::wire::unsupported_version_record())?;
            stream.flush()?;
            drop(stream);

            listener.set_nonblocking(true)?;
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => return Ok(2),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(1)
        });

        let mut config = producer_config(Vec::new(), Vec::new());
        config.dry_run = false;
        config.endpoint = Some(socket.to_string_lossy().into_owned());
        config.token = Some("00".repeat(32));
        let error = match ProducerSession::connect(&config) {
            Ok(_) => panic!("default-disabled version retry unexpectedly connected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("supported version: 1.1"));
        assert_eq!(
            error.downcast_ref::<VersionRejectionError>(),
            Some(&VersionRejectionError {
                attempted_version: (1, 1),
                supported_version: Some((1, 1)),
                fatal: true,
            })
        );
        assert_eq!(server.join().unwrap().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn enabled_version_retry_uses_one_fresh_legacy_connection() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        use vivid_protocol::wire::{HEADER_SIZE, PREFACE_SIZE, RecordHeader};

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("one-version-retry.sock");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping version retry socket test: {error}");
                return;
            }
            Err(error) => panic!("fake presenter bind failed: {error}"),
        };
        let server = thread::spawn(move || -> io::Result<()> {
            let (mut first, _) = listener.accept()?;
            let mut preface = [0; PREFACE_SIZE];
            first.read_exact(&mut preface)?;
            assert_eq!(&preface[4..6], &[1, 1]);
            first.write_all(&version_rejection_record(1, 0))?;
            first.flush()?;
            drop(first);

            let (mut second, _) = listener.accept()?;
            second.read_exact(&mut preface)?;
            assert_eq!(&preface[4..6], &[1, 0]);
            let mut header = [0; HEADER_SIZE];
            second.read_exact(&mut header)?;
            let header = RecordHeader::decode(header);
            assert_eq!(header.record_type, messages::HELLO);
            let mut hello = vec![0; header.body_length as usize];
            second.read_exact(&mut hello)?;
            let (_, parsed_hello) = messages::parse_hello(&hello)?;
            assert_eq!(
                (
                    parsed_hello.minimum_major,
                    parsed_hello.minimum_minor,
                    parsed_hello.maximum_major,
                    parsed_hello.maximum_minor,
                ),
                (1, 0, 1, 0)
            );
            assert_eq!(
                parsed_hello.authentication_kind,
                messages::AUTHENTICATION_WINDOW_ROOT
            );

            let welcome = messages::try_encode_welcome_for_version(
                1,
                &messages::WelcomeConfig {
                    session_id: 1,
                    session_tag: &[1; 16],
                    root_context_id: 2,
                    capability_generation: 1,
                    display: DisplayState {
                        display_generation: 1,
                        viewport_width: 800,
                        viewport_height: 600,
                        grid_columns: 80,
                        grid_rows: 24,
                        cell_width: 10,
                        cell_height: 25,
                    },
                    maximum_control_body: vivid_protocol::CONTROL_MAX_RECORD_BODY,
                    accepted_profiles: &[],
                    selected_major: 1,
                    selected_minor: 0,
                    accepted_features: &[],
                    initial_scene_revision: 0,
                    preserved_fields: &[],
                },
                1,
                0,
            )?;
            second.write_all(
                &RecordHeader {
                    body_length: welcome.len() as u32,
                    record_type: messages::WELCOME,
                    flags: 0,
                    object_id: 0,
                    sequence: 1,
                }
                .encode(),
            )?;
            second.write_all(&welcome)?;
            second.flush()
        });

        let mut config = producer_config(Vec::new(), Vec::new());
        config.dry_run = false;
        config.endpoint = Some(socket.to_string_lossy().into_owned());
        config.token = Some("00".repeat(32));
        config.allow_version_retry = true;
        let session = ProducerSession::connect(&config).unwrap();
        drop(session);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn decoder_description_is_emitted_only_when_negotiated() {
        let without = ProducerSession::connect(&producer_config(Vec::new(), Vec::new())).unwrap();
        let with = ProducerSession::connect(&producer_config(
            Vec::new(),
            vec![messages::FEATURE_DECODER_DESCRIPTION_V1],
        ))
        .unwrap();
        let avcc = [1, 0x64, 0, 0x1f, 0xff, 0xe1, 0];
        let video = || VideoSourceConfig {
            source_id: 1,
            codec: "h264",
            packetization: "h264-annexb-au-v1",
            extradata: &[0, 0, 0, 1, 0x67],
            width: 64,
            height: 64,
            profile: 100,
            level: 31,
            bitrate: 1,
            color_primaries: 1,
            transfer: 1,
            matrix: 1,
            range: 1,
            sar_num: 1,
            sar_den: 1,
            max_access_unit_bytes: 1024,
            codec_string: Some("avc1.64001F"),
            decoder_config: Some(&avcc),
        };
        let scrubbed = without.scrub_video_description(video());
        assert_eq!(scrubbed.codec_string, None);
        assert_eq!(scrubbed.decoder_config, None);

        let retained = with.scrub_video_description(video());
        assert_eq!(retained.codec_string, Some("avc1.64001F"));
        assert_eq!(retained.decoder_config, Some(avcc.as_slice()));

        let audio = || AudioSourceConfig {
            source_id: 2,
            linked_video_source_id: Some(1),
            codec: "aac",
            packetization: "aac-raw-au-v1",
            extradata: &[0x11, 0x90],
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            bitrate: 1,
            max_access_unit_bytes: 1024,
            codec_string: Some("mp4a.40.2"),
        };
        assert_eq!(without.scrub_audio_description(audio()).codec_string, None);
        assert_eq!(
            with.scrub_audio_description(audio()).codec_string,
            Some("mp4a.40.2")
        );
    }

    /// A live control connection with routine traffic must produce an RTT estimate quickly, so
    /// `play_at`'s RTT-derived minimum buffer works before streaming begins. The fake presenter
    /// answers every `PING`; nothing here is idle for the 15-second liveness threshold, so only
    /// the active sampling path can produce the estimate.
    #[cfg(unix)]
    #[test]
    fn active_rtt_sampling_populates_estimate_during_traffic() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        use vivid_protocol::wire::{
            Connection, ConnectionKind, Endpoint, HEADER_SIZE, PREFACE_SIZE, RecordHeader,
        };

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("rtt-sampling.sock");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping RTT sampling socket test: {error}");
                return;
            }
            Err(error) => panic!("fake presenter bind failed: {error}"),
        };
        thread::spawn(move || -> io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut preface = [0; PREFACE_SIZE];
            stream.read_exact(&mut preface)?;
            let mut sequence = 0_u64;
            loop {
                let mut header = [0; HEADER_SIZE];
                if stream.read_exact(&mut header).is_err() {
                    return Ok(());
                }
                let header = RecordHeader::decode(header);
                let mut body = vec![0; header.body_length as usize];
                stream.read_exact(&mut body)?;
                let request_id = messages::request_id(&body)?;
                let (record_type, reply) = if header.record_type == messages::PING {
                    (messages::PONG, messages::ok(request_id))
                } else {
                    (messages::OK, messages::ok(request_id))
                };
                sequence += 1;
                stream.write_all(
                    &RecordHeader {
                        body_length: reply.len() as u32,
                        record_type,
                        flags: 0,
                        object_id: 0,
                        sequence,
                    }
                    .encode(),
                )?;
                stream.write_all(&reply)?;
                stream.flush()?;
            }
        });

        let endpoint = Endpoint::parse(socket.to_str().unwrap()).unwrap();
        let connection = Connection::open(&endpoint, ConnectionKind::Control).unwrap();
        let counters = Arc::new(HotPathCounterState::default());
        let dispatcher = ControlDispatcher::start(
            connection,
            DisplayState {
                display_generation: 1,
                viewport_width: 800,
                viewport_height: 600,
                grid_columns: 80,
                grid_rows: 24,
                cell_width: 10,
                cell_height: 25,
            },
            false,
            counters.clone(),
        )
        .unwrap();

        dispatcher
            .write_record(messages::GOODBYE, 0, 0, &messages::goodbye(1))
            .unwrap();
        dispatcher.wait_reply(1, &[messages::OK], 0).unwrap();
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.control_reply_samples, 1);
        assert_eq!(snapshot.pending_request_high_water, 1);
        assert_eq!(snapshot.reply_queue_high_water, 1);

        let deadline = Instant::now() + Duration::from_secs(10);
        let sampled = loop {
            let rtt_us = dispatcher
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .rtt_us;
            if let Some(rtt_us) = rtt_us {
                break rtt_us;
            }
            assert!(
                Instant::now() < deadline,
                "active RTT sampling produced no estimate within 10 seconds"
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            dispatcher.adjusted_minimum_buffer(0),
            messages::minimum_buffer_for_rtt(0, Some(sampled))
        );
    }

    fn source(id: u64, byte_credits: u64, packet_credits: u64) -> SourceHandle {
        SourceHandle {
            id,
            ticket: vec![0; 32],
            record_limit: 1024,
            state: Arc::new(SourceSync {
                state: Mutex::new(SourceRuntime {
                    credits: messages::CreditLedger::new(Credits {
                        bytes: byte_credits,
                        packets: packet_credits,
                        fragments: 0,
                    }),
                    visible: true,
                    visible_reasons: 0,
                    need_keyframe_epoch: None,
                    lost: None,
                    credit_returns: 0,
                }),
                changed: Condvar::new(),
            }),
            counters: Arc::new(HotPathCounterState::default()),
            last_record_sequence: 0,
            attachment_generation: 0,
            acknowledged_credit_returns: 0,
            observed_visible: true,
            reported_lost: false,
        }
    }

    #[test]
    fn session_anchor_uses_v2_marker() {
        let key = anchor::derive_key(&[0; 32], &[0; 16]);
        let marker = anchor::encode_marker(&key, &[0; 16], 7).unwrap();
        assert!(marker.starts_with("\x1b_VIVID;2;A;"));
        assert!(marker.len() <= 128);
    }

    #[test]
    fn remote_conpty_transport_selects_windows_marker_envelope() {
        let key = anchor::derive_key(&[0; 32], &[0; 16]);
        let marker = anchor::encode_marker(&key, &[0; 16], 7).unwrap();

        assert!(configured_anchor_transport_is_conpty(Some("conpty")));
        assert!(!configured_anchor_transport_is_conpty(None));
        assert!(!configured_anchor_transport_is_conpty(Some("apc")));

        let transported = marker_for_transport(marker.clone(), true);
        assert!(transported.starts_with("VIVID;2;A;"));
        assert!(transported.ends_with(";VIVID-END"));
        assert!(!transported.contains('\x1b'));
        assert_eq!(marker_for_transport(marker.clone(), false), marker);
    }

    #[test]
    fn source_cancellation_wakes_a_blocked_credit_wait() {
        let source = source(7, 0, 0);
        let cancel = source.cancellation();
        let (done, result) = std::sync::mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            let error = source.consume_credits(1, false, true).unwrap_err();
            done.send(error.to_string()).unwrap();
        });
        cancel.cancel("local shutdown");
        assert_eq!(
            result.recv_timeout(Duration::from_secs(1)).unwrap(),
            "local shutdown"
        );
        join.join().unwrap();
    }

    #[test]
    fn source_credit_waits_are_independent() {
        let blocked = source(1, 0, 0);
        let ready = source(2, 64, 1);
        let cancel = blocked.cancellation();
        let join = thread::spawn(move || blocked.consume_credits(1, false, true));
        ready.consume_credits(32, false, true).unwrap();
        cancel.cancel("test complete");
        assert!(join.join().unwrap().is_err());
    }

    #[test]
    fn media_waits_interrupt_for_visibility_and_keyframe_events() {
        let source = source(3, 64, 1);
        {
            let mut state = source.state.state.lock().unwrap();
            state.visible = false;
        }
        assert_eq!(
            source.consume_credits(1, false, true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        {
            let mut state = source.state.state.lock().unwrap();
            state.visible = true;
            state.need_keyframe_epoch = Some(9);
        }
        assert_eq!(
            source.consume_credits(1, false, true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn audio_send_ignores_advisory_false_visibility() {
        let source = source(4, 1024, 1);
        source.state.state.lock().unwrap().visible = false;
        let mut sender = MediaSender {
            source,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Audio).unwrap(),
                source_id: 4,
            },
            dry_run: false,
            raster_zstd: false,
        };
        sender
            .send_audio(media::AudioPacket {
                epoch: 1,
                packet_id: 1,
                pts_us: 0,
                dts_us: 0,
                duration_us: 20_000,
                trim_start_samples: 0,
                trim_end_samples: 0,
                data: &[0xf8, 0xff, 0xfe],
            })
            .unwrap();
    }

    #[test]
    fn owned_sender_sends_raster_and_image_in_dry_run() {
        let raster = source(5, 4096, 2);
        let mut raster_sender = MediaSender {
            source: raster,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Raster).unwrap(),
                source_id: 5,
            },
            dry_run: true,
            raster_zstd: true,
        };
        raster_sender
            .send_raster(1, 1, 2, 1, &[255, 0, 0, 255, 0, 255, 0, 255])
            .unwrap();
        let raster_counters = raster_sender.source().hot_path_counters();
        assert_eq!(raster_counters.media_records_sent, 1);
        assert_eq!(raster_counters.media_allocations, 2);
        assert!(raster_counters.media_bytes_copied > 0);
        assert_eq!(
            raster_sender
                .send_raster(1, 2, 2, 1, &[0, 0, 0, 255])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let image = source(6, 4096, 1);
        let mut image_sender = MediaSender {
            source: image,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Blob).unwrap(),
                source_id: 6,
            },
            dry_run: true,
            raster_zstd: false,
        };
        image_sender.send_image(b"encoded image").unwrap();
        assert_eq!(
            image_sender.source().hot_path_counters(),
            HotPathCounters {
                media_records_sent: 1,
                ..HotPathCounters::default()
            }
        );
    }

    #[test]
    fn owned_sender_tracks_media_sequences_without_payload_copies() {
        let source = source(8, 4096, 2);
        let mut sender = MediaSender {
            source,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Video).unwrap(),
                source_id: 8,
            },
            dry_run: true,
            raster_zstd: false,
        };
        assert_eq!(sender.source().last_record_sequence(), 0);
        assert_eq!(sender.source().attachment_generation(), 0);
        for packet_id in 1..=2 {
            sender
                .send_video(VideoPacket {
                    epoch: 1,
                    packet_id,
                    pts_us: 0,
                    dts_us: 0,
                    duration_us: 16_667,
                    key: packet_id == 1,
                    data: &[0, 0, 1, 0x65],
                })
                .unwrap();
        }
        assert_eq!(sender.source().last_record_sequence(), 2);
        assert_eq!(
            sender.source().hot_path_counters(),
            HotPathCounters {
                media_records_sent: 2,
                ..HotPathCounters::default()
            }
        );
    }

    #[test]
    fn session_counter_accessor_observes_owned_raw_raster_sender() {
        let mut session =
            ProducerSession::connect(&producer_config(Vec::new(), Vec::new())).unwrap();
        let source = session.create_raster_source(1, 1, 1).unwrap();
        let mut sender = session
            .open_media_sender(source, ConnectionKind::Raster)
            .unwrap();
        sender.send_raster(1, 1, 1, 1, &[0, 0, 0, 255]).unwrap();
        assert_eq!(
            session.hot_path_counters(),
            HotPathCounters {
                media_records_sent: 1,
                ..HotPathCounters::default()
            }
        );
    }

    #[test]
    fn counter_snapshot_reports_timings_and_queue_high_water_marks() {
        let counters = HotPathCounterState::default();
        counters.record_media_sent();
        counters.record_media_work(2, 72);
        counters.record_control_reply(Duration::from_micros(25));
        update_high_water(&counters.pending_request_high_water, 3);
        update_high_water(&counters.reply_queue_high_water, 2);
        update_high_water(&counters.source_event_queue_high_water, 4);
        update_high_water(&counters.desktop_input_queue_high_water, 5);

        assert_eq!(
            counters.snapshot(),
            HotPathCounters {
                media_records_sent: 1,
                media_bytes_copied: 72,
                media_allocations: 2,
                control_reply_latency_us: 25,
                control_reply_samples: 1,
                pending_request_high_water: 3,
                reply_queue_high_water: 2,
                source_event_queue_high_water: 4,
                desktop_input_queue_high_water: 5,
                ..HotPathCounters::default()
            }
        );
    }

    #[test]
    fn owned_one_shot_sender_waits_for_credit_return() {
        let source = source(7, 4096, 1);
        let state = source.state.clone();
        let mut sender = MediaSender {
            source,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Blob).unwrap(),
                source_id: 7,
            },
            dry_run: false,
            raster_zstd: false,
        };
        let join = thread::spawn(move || sender.send_image(b"encoded image"));
        {
            let mut runtime = state.state.lock().unwrap();
            runtime.credit_returns += 1;
            state.changed.notify_all();
        }
        join.join().unwrap().unwrap();
    }

    #[test]
    fn desktop_input_queue_coalesces_motion_and_reset_is_authoritative() {
        let mut queue = VecDeque::new();
        push_desktop_input(
            &mut queue,
            DesktopInputEvent::PointerMotion {
                source_id: 7,
                x: 10,
                y: 20,
            },
        )
        .unwrap();
        push_desktop_input(
            &mut queue,
            DesktopInputEvent::PointerMotion {
                source_id: 7,
                x: 30,
                y: 40,
            },
        )
        .unwrap();
        assert_eq!(
            queue.pop_front(),
            Some(DesktopInputEvent::PointerMotion {
                source_id: 7,
                x: 30,
                y: 40,
            })
        );

        push_desktop_input(
            &mut queue,
            DesktopInputEvent::Key {
                usage: 4,
                pressed: true,
            },
        )
        .unwrap();
        push_desktop_input(&mut queue, DesktopInputEvent::Reset).unwrap();
        assert_eq!(queue, VecDeque::from([DesktopInputEvent::Reset]));
    }

    #[test]
    fn desktop_input_queue_preserves_order_and_overflow_fails_closed() {
        let mut queue = VecDeque::new();
        push_desktop_input(
            &mut queue,
            DesktopInputEvent::Key {
                usage: 4,
                pressed: true,
            },
        )
        .unwrap();
        push_desktop_input(
            &mut queue,
            DesktopInputEvent::PointerButton {
                source_id: 7,
                button: 0,
                pressed: true,
            },
        )
        .unwrap();
        push_desktop_input(
            &mut queue,
            DesktopInputEvent::PointerAxis {
                source_id: 7,
                horizontal_120: -120,
                vertical_120: 240,
            },
        )
        .unwrap();
        assert_eq!(
            queue,
            VecDeque::from([
                DesktopInputEvent::Key {
                    usage: 4,
                    pressed: true,
                },
                DesktopInputEvent::PointerButton {
                    source_id: 7,
                    button: 0,
                    pressed: true,
                },
                DesktopInputEvent::PointerAxis {
                    source_id: 7,
                    horizontal_120: -120,
                    vertical_120: 240,
                },
            ])
        );

        while queue.len() < MAX_PENDING_DESKTOP_INPUT {
            let usage = 4 + u16::try_from(queue.len() % 32).unwrap();
            let pressed = queue.len() % 2 == 0;
            push_desktop_input(&mut queue, DesktopInputEvent::Key { usage, pressed }).unwrap();
        }
        assert!(
            push_desktop_input(
                &mut queue,
                DesktopInputEvent::Key {
                    usage: 4,
                    pressed: false,
                },
            )
            .is_err()
        );
        assert_eq!(queue, VecDeque::from([DesktopInputEvent::Reset]));
    }

    #[test]
    fn desktop_input_records_require_negotiation_and_matching_object_ids() {
        let record = Record {
            record_type: messages::KEY_INPUT,
            flags: 0,
            object_id: 0,
            sequence: 1,
            body: messages::key_input(4, true),
        };
        assert!(decode_desktop_input_record(false, &record).is_err());
        assert_eq!(
            decode_desktop_input_record(true, &record).unwrap(),
            DesktopInputEvent::Key {
                usage: 4,
                pressed: true,
            }
        );

        let mismatched = Record {
            record_type: messages::POINTER_MOTION,
            flags: 0,
            object_id: 8,
            sequence: 2,
            body: messages::pointer_motion(7, 10, 20),
        };
        assert!(decode_desktop_input_record(true, &mismatched).is_err());
    }
}
