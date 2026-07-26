use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vivid_protocol::anchor::{self, AnchorKey};
use vivid_protocol::media::{self, VideoPacket};
use vivid_protocol::messages::{
    self, AudioSourceConfig, Credits, ImageSourceConfig, SceneNodeConfig, SourceReady,
    VideoSourceConfig,
};
use vivid_protocol::trace::{
    TraceComponent, TraceDirection, TraceEmitter, TraceGuard, TraceHop, TraceObjectKind,
    TraceOutcome,
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
pub use vivid_protocol::messages::{
    AnchorStatus, LimitsStatus, PlaybackSnapshot, PlaybackState, ReportedSourceDescriptor,
    RequestMetadata, SceneChanged, SceneQuery, SceneStatus, SourceChanged, SourceDescriptor,
    SourceStatus, WaitSatisfied, WaitSource,
};
pub use vivid_protocol::revision::{SceneRevision, SourceRevision};
pub use vivid_protocol::trace::{
    TraceComponent as DiagnosticTraceComponent, TraceRecord as DiagnosticTraceRecord,
};

/// An unsolicited, coalesced observability event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationEvent {
    Source(messages::SourceChanged),
    Scene(messages::SceneChanged),
    Playback(messages::PlaybackState),
}

/// Typed unsolicited session state that is not tied to observability feature 18.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Capabilities(messages::CapsChanged),
}

/// The latest authoritative revisions observed on the control connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionState {
    pub scene: SceneRevision,
    pub sources: HashMap<u64, SourceRevision>,
}

/// Result of resolving an uncertain `ATTACH_CHANNEL` write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentResolution {
    NotConsumed,
    ConsumedAttached { generation: u64 },
    ConsumedClosed { generation: u64 },
    Indeterminate,
    RecreateRequired,
}

#[derive(Debug)]
pub struct AttachmentError {
    pub source_id: u64,
    pub resolution: AttachmentResolution,
    diagnostic: String,
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "media attachment for source {} could not be completed ({:?}): {}",
            self.source_id, self.resolution, self.diagnostic
        )
    }
}

impl std::error::Error for AttachmentError {}

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

/// A machine-readable presenter rejection.
///
/// Callers should branch on `code` and `detail`; `diagnostic` is display-only protocol prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterError {
    pub code: u64,
    pub request_id: u64,
    pub fatal: bool,
    pub detail: messages::ErrorDetail,
    pub diagnostic: String,
}

impl std::fmt::Display for PresenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "presenter error {}: {}",
            self.code, self.diagnostic
        )
    }
}

impl std::error::Error for PresenterError {}

impl From<messages::ErrorReply> for PresenterError {
    fn from(error: messages::ErrorReply) -> Self {
        Self {
            code: error.code,
            request_id: error.request_id,
            fatal: error.fatal,
            detail: error.detail,
            diagnostic: error.diagnostic,
        }
    }
}

fn presenter_error(body: &[u8]) -> io::Result<io::Error> {
    Ok(io::Error::other(PresenterError::from(
        messages::parse_error_reply(body)?,
    )))
}

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

/// Presenter-advertised bounds for a producer-side media queue.
///
/// These values are sizing hints, not grants. Callers must still submit through `MediaSender`,
/// which consumes only credit actually received from the presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaQueueLimits {
    pub max_bytes: usize,
    pub max_packets: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockEstimate {
    pub offset_us: i64,
    pub delay_us: u64,
    pub responder_processing_us: u64,
    pub accepted_samples: u64,
    pub rejected_samples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterSendKind {
    Full,
    Delta,
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

fn random_trace_hint() -> io::Result<[u8; 16]> {
    let mut hint = [0_u8; 16];
    getrandom::fill(&mut hint)
        .map_err(|error| io::Error::other(format!("trace hint generation failed: {error}")))?;
    Ok(hint)
}

fn trace_component_for_producer(producer: &str) -> TraceComponent {
    match producer {
        "vivi" => TraceComponent::Vivi,
        "vvrd" => TraceComponent::Vvrd,
        "veston" => TraceComponent::Veston,
        "vvsway" => TraceComponent::Vvsway,
        _ => TraceComponent::Sdk,
    }
}

fn trace_file_stem_for_producer(producer: &str) -> &'static str {
    match producer {
        "vivi" => "vivi",
        "vvrd" => "vvrd",
        "veston" => "veston",
        "vvsway" => "vvsway",
        _ => "vivid-sdk",
    }
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
    /// HELLO authentication kind. Use `AUTHENTICATION_DELEGATED_CONTEXT` with a capability
    /// supplied out of band in `token` to bind this connection to a delegated context.
    pub authentication_kind: u64,
    /// Permit one explicit retry on a fresh connection after a typed version rejection.
    ///
    /// Disabled by default by all SDK integrations. The SDK retries only versions for which it
    /// retains a complete negotiation implementation.
    pub allow_version_retry: bool,
}

/// An opaque bearer capability for one delegated context.
///
/// This type deliberately does not implement `Debug` or `Display`. Callers should expose the
/// bytes only while transferring the capability through a protected channel.
pub struct DelegatedCapability([u8; messages::CONTEXT_CAPABILITY_BYTES]);

impl DelegatedCapability {
    pub fn expose_bytes(&self) -> &[u8; messages::CONTEXT_CAPABILITY_BYTES] {
        &self.0
    }

    pub fn expose_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(self.0.len() * 2);
        for byte in self.0 {
            encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

impl Drop for DelegatedCapability {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl ProducerConfig {
    pub fn is_dry_run(&self) -> bool {
        self.dry_run || self.trace_dir.is_some()
    }

    pub fn validate(&self) -> io::Result<()> {
        if !matches!(
            self.authentication_kind,
            messages::AUTHENTICATION_WINDOW_ROOT | messages::AUTHENTICATION_DELEGATED_CONTEXT
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported Vivid authentication kind",
            ));
        }
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
    trace: SharedTrace,
    last_record_sequence: u64,
    attachment_generation: u64,
    rolling_byte_window: u64,
    rolling_packet_window: u64,
    delta_operation_limit: Option<u32>,
    media_connection_required: bool,
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
    need_full_frame_reason: Option<u64>,
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
    discard_replies: HashSet<u64>,
    sources: HashMap<u64, Arc<SourceSync>>,
    pending_source_events: HashMap<u64, VecDeque<Record>>,
    pending_source_event_count: usize,
    desktop_input_enabled: bool,
    desktop_input: VecDeque<DesktopInputEvent>,
    anchors: HashSet<u64>,
    display: DisplayState,
    scene_revision: SceneRevision,
    source_revisions: HashMap<u64, SourceRevision>,
    observations: VecDeque<ObservationEvent>,
    session_events: VecDeque<SessionEvent>,
    capability_generation: u64,
    closed: Option<String>,
    last_inbound: Instant,
    last_probe_sent: Option<Instant>,
    unanswered_probes: u8,
    next_internal_request_id: u64,
    pending_pings: HashMap<u64, PendingPing>,
    /// Last RTT sampling probe. Sampling probes are independent of the idle liveness probes:
    /// they never advance `unanswered_probes` or `last_probe_sent`, so they cannot change
    /// disconnect detection.
    last_rtt_probe: Option<Instant>,
    rtt_us: Option<u64>,
    clock_estimate: Option<ClockEstimate>,
    rejected_clock_samples: u64,
}

struct DispatcherShared {
    state: Mutex<DispatcherState>,
    changed: Condvar,
    counters: Arc<HotPathCounterState>,
    trace: SharedTrace,
    clock_origin: Instant,
    clock_sampling_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingPing {
    sent: Instant,
    sender_transmit_us: Option<u64>,
}

#[derive(Default)]
struct TraceState {
    emitter: Option<TraceEmitter>,
    restricted_sources: HashSet<u64>,
}

impl std::fmt::Debug for TraceState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TraceState")
            .field("enabled", &self.emitter.is_some())
            .field("restricted_source_count", &self.restricted_sources.len())
            .finish()
    }
}

type SharedTrace = Arc<Mutex<TraceState>>;

fn emit_control_trace(
    trace: &SharedTrace,
    direction: TraceDirection,
    record_type: u16,
    object_id: u64,
    sequence: u64,
    body: &[u8],
    outcome: TraceOutcome,
) {
    let state = trace
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(emitter) = &state.emitter else {
        return;
    };
    if object_id != 0 && state.restricted_sources.contains(&object_id) {
        emitter.emit_restricted_source(
            direction,
            record_type,
            u64::try_from(body.len()).unwrap_or(u64::MAX),
            sequence,
        );
    } else {
        emitter.emit_control(
            direction,
            record_type,
            body,
            sequence,
            vivid_protocol::trace::object_kind(record_type, object_id),
            (object_id != 0).then_some(object_id),
            outcome,
        );
    }
}

fn emit_media_trace(
    trace: &SharedTrace,
    record_type: u16,
    object_id: u64,
    sequence: u64,
    body_length: u64,
) {
    let state = trace
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(emitter) = &state.emitter else {
        return;
    };
    if state.restricted_sources.contains(&object_id) {
        emitter.emit_restricted_source(TraceDirection::Send, record_type, body_length, sequence);
    } else {
        emitter.emit(
            TraceDirection::Send,
            record_type,
            body_length,
            sequence,
            TraceObjectKind::Source,
            Some(object_id),
            None,
            None,
            TraceOutcome::Ok,
        );
    }
}

struct ControlDispatcher {
    writer: ConnectionWriter,
    shared: Arc<DispatcherShared>,
}

#[derive(Clone)]
struct WaitDispatcher {
    writer: ConnectionWriter,
    shared: Arc<DispatcherShared>,
}

enum ClientControl {
    Direct(Connection, SharedTrace),
    Live(ControlDispatcher),
}

/// A cancellation-safe in-flight source wait.
///
/// Dropping this handle before completion sends `CANCEL_WAIT`. Call [`Self::cancel`] when the
/// cancellation result itself matters.
pub struct SourceWaitHandle {
    dispatcher: Option<WaitDispatcher>,
    request_id: u64,
    source_id: u64,
    synthetic: Option<WaitSatisfied>,
    completed: bool,
    active: Arc<AtomicBool>,
}

impl SourceWaitHandle {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn cancellation(&self) -> SourceWaitCancellation {
        SourceWaitCancellation {
            dispatcher: self.dispatcher.clone(),
            request_id: self.request_id,
            active: self.active.clone(),
        }
    }

    pub fn wait(&mut self) -> io::Result<WaitSatisfied> {
        if self.completed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Vivid source wait has already completed or been cancelled",
            ));
        }
        if let Some(satisfied) = self.synthetic.take() {
            self.completed = true;
            self.active.store(false, Ordering::Release);
            return Ok(satisfied);
        }
        let dispatcher = self
            .dispatcher
            .as_ref()
            .ok_or_else(|| io::Error::other("Vivid source wait has no dispatcher"))?;
        let record = wait_dispatcher_reply(
            dispatcher,
            self.request_id,
            &[messages::WAIT_SATISFIED],
            self.source_id,
            &self.active,
        )?;
        self.completed = true;
        self.active.store(false, Ordering::Release);
        if record.record_type == messages::ERROR {
            return Err(presenter_error(&record.body)?);
        }
        let (request_id, satisfied) = messages::parse_wait_satisfied(&record.body)?;
        if request_id != self.request_id
            || satisfied.source_id != self.source_id
            || record.object_id != self.source_id
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAIT_SATISFIED correlation mismatch",
            ));
        }
        Ok(satisfied)
    }

    pub fn cancel(&mut self) -> io::Result<()> {
        if self.completed {
            return Ok(());
        }
        self.completed = true;
        if self.active.swap(false, Ordering::AcqRel)
            && let Some(dispatcher) = &self.dispatcher
        {
            cancel_dispatched_wait(dispatcher, self.request_id)?;
        }
        Ok(())
    }
}

impl Drop for SourceWaitHandle {
    fn drop(&mut self) {
        if !self.completed && self.active.swap(false, Ordering::AcqRel) {
            if let Some(dispatcher) = &self.dispatcher {
                let _ = cancel_dispatched_wait(dispatcher, self.request_id);
            }
            self.completed = true;
        }
    }
}

/// A cloneable cancellation token for an in-flight source wait.
#[derive(Clone)]
pub struct SourceWaitCancellation {
    dispatcher: Option<WaitDispatcher>,
    request_id: u64,
    active: Arc<AtomicBool>,
}

impl SourceWaitCancellation {
    pub fn cancel(&self) -> io::Result<()> {
        if self.active.swap(false, Ordering::AcqRel)
            && let Some(dispatcher) = &self.dispatcher
        {
            cancel_dispatched_wait(dispatcher, self.request_id)?;
        }
        Ok(())
    }
}

impl ControlDispatcher {
    #[allow(clippy::too_many_arguments)]
    fn start(
        connection: Connection,
        display: DisplayState,
        scene_revision: SceneRevision,
        capability_generation: u64,
        desktop_input_enabled: bool,
        clock_sampling_enabled: bool,
        counters: Arc<HotPathCounterState>,
        trace: SharedTrace,
    ) -> io::Result<Self> {
        let (mut reader, writer) = connection.split()?;
        let shared = Arc::new(DispatcherShared {
            state: Mutex::new(DispatcherState {
                replies: HashMap::new(),
                pending_requests: HashMap::new(),
                discard_replies: HashSet::new(),
                sources: HashMap::new(),
                pending_source_events: HashMap::new(),
                pending_source_event_count: 0,
                desktop_input_enabled,
                desktop_input: VecDeque::new(),
                anchors: HashSet::new(),
                display,
                scene_revision,
                source_revisions: HashMap::new(),
                observations: VecDeque::new(),
                session_events: VecDeque::new(),
                capability_generation,
                closed: None,
                last_inbound: Instant::now(),
                last_probe_sent: None,
                unanswered_probes: 0,
                next_internal_request_id: u64::MAX,
                pending_pings: HashMap::new(),
                last_rtt_probe: None,
                rtt_us: None,
                clock_estimate: None,
                rejected_clock_samples: 0,
            }),
            changed: Condvar::new(),
            counters,
            trace,
            clock_origin: Instant::now(),
            clock_sampling_enabled,
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
                    let received_at = Instant::now();
                    emit_control_trace(
                        &reader_shared.trace,
                        TraceDirection::Receive,
                        record.record_type,
                        record.object_id,
                        record.sequence,
                        &record.body,
                        TraceOutcome::Ok,
                    );
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
                        let response = messages::parse_clock_ping(&record.body).and_then(|ping| {
                                if record.object_id != 0 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Vivid PING is not a correlated session-level request",
                                    ));
                                }
                                if ping.sender_transmit_us.is_some()
                                    && !reader_shared.clock_sampling_enabled
                                {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "timestamped PING was not negotiated",
                                    ));
                                }
                                let timestamps = ping.sender_transmit_us.map(|echoed| {
                                    let receive_us =
                                        monotonic_us(reader_shared.clock_origin, received_at);
                                    let transmit_us = monotonic_us(
                                        reader_shared.clock_origin,
                                        Instant::now(),
                                    )
                                    .max(receive_us);
                                    messages::ClockPongTimestamps {
                                        echoed_sender_transmit_us: echoed,
                                        responder_receive_us: receive_us,
                                        responder_transmit_us: transmit_us,
                                    }
                                });
                                reader_writer.write_record(
                                    messages::PONG,
                                    0,
                                    0,
                                    &messages::clock_pong(ping.request_id, timestamps)?,
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
                    let state = &mut *state;
                    let routed = match record.record_type {
                        messages::CREDIT
                        | messages::VISIBILITY
                        | messages::NEED_KEYFRAME
                        | messages::NEED_FULL_FRAME
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
                        messages::CAPS_CHANGED => messages::parse_caps_changed(&record.body)
                            .and_then(|event| {
                                apply_caps_changed(
                                    &mut state.capability_generation,
                                    &mut state.session_events,
                                    event,
                                )
                            }),
                        messages::SOURCE_CHANGED => messages::parse_source_changed(&record.body)
                            .and_then(|event| {
                                state
                                    .source_revisions
                                    .insert(event.source_id, event.source_revision);
                                push_observation(
                                    &mut state.observations,
                                    ObservationEvent::Source(event),
                                )
                            }),
                        messages::SCENE_CHANGED => messages::parse_scene_changed(&record.body)
                            .and_then(|event| {
                                state.scene_revision = event.scene_revision;
                                push_observation(
                                    &mut state.observations,
                                    ObservationEvent::Scene(event),
                                )
                            }),
                        messages::PLAYBACK_STATE => messages::parse_playback_state(&record.body)
                            .and_then(|event| {
                                state
                                    .source_revisions
                                    .insert(event.source_id, event.source_revision);
                                push_observation(
                                    &mut state.observations,
                                    ObservationEvent::Playback(event),
                                )
                            }),
                        messages::KEY_INPUT
                        | messages::POINTER_MOTION
                        | messages::POINTER_BUTTON
                        | messages::POINTER_AXIS
                        | messages::INPUT_RESET => {
                            apply_desktop_input_record(
                                &mut *state,
                                &record,
                                &reader_shared.counters,
                            )
                        }
                        messages::ANCHOR_READY => {
                            messages::parse_anchor_event(&record.body).map(|anchor| {
                                state.anchors.insert(anchor);
                            })
                        }
                        messages::PONG => {
                            messages::parse_clock_pong(&record.body).and_then(|pong| {
                                if record.object_id != 0 || pong.request_id == 0 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Vivid PONG is not a correlated session-level reply",
                                    ));
                                }
                                if let Some(pending) = state.pending_pings.remove(&pong.request_id) {
                                    let sample = u64::try_from(pending.sent.elapsed().as_micros())
                                        .unwrap_or(u64::MAX);
                                    state.rtt_us = Some(state.rtt_us.map_or(sample, |current| {
                                        current.saturating_mul(7).saturating_add(sample) / 8
                                    }));
                                    match (pending.sender_transmit_us, pong.timestamps) {
                                        (None, None) => {}
                                        (Some(sent_us), Some(timestamps))
                                            if timestamps.echoed_sender_transmit_us == sent_us =>
                                        {
                                            let received_us = monotonic_us(
                                                reader_shared.clock_origin,
                                                received_at,
                                            );
                                            if let Some(sample) =
                                                messages::calculate_clock_sample(
                                                    sent_us,
                                                    timestamps.responder_receive_us,
                                                    timestamps.responder_transmit_us,
                                                    received_us,
                                                    messages::MAX_CLOCK_SAMPLE_PROCESSING_US,
                                                )
                                            {
                                                update_clock_estimate(
                                                    &mut state.clock_estimate,
                                                    sample,
                                                    state.rejected_clock_samples,
                                                );
                                            } else {
                                                state.rejected_clock_samples = state
                                                    .rejected_clock_samples
                                                    .saturating_add(1);
                                                if let Some(estimate) =
                                                    &mut state.clock_estimate
                                                {
                                                    estimate.rejected_samples =
                                                        state.rejected_clock_samples;
                                                }
                                            }
                                        }
                                        (Some(_), Some(_)) => {
                                            return Err(io::Error::new(
                                                io::ErrorKind::InvalidData,
                                                "PONG did not echo the PING timestamp",
                                            ));
                                        }
                                        _ => {
                                            return Err(io::Error::new(
                                                io::ErrorKind::InvalidData,
                                                "PONG timestamp presence does not match PING",
                                            ));
                                        }
                                    }
                                }
                                Ok(())
                            })
                        }
                        messages::ERROR => messages::parse_error_reply(&record.body).and_then(
                            |error| {
                                if error.code == messages::ERROR_CONTEXT_REVOKED
                                    && error.request_id == 0
                                    && error.fatal
                                {
                                    state.closed = Some("delegated Vivid context was revoked".into());
                                    return Ok(());
                                }
                                if error.request_id == 0
                                    && !error.fatal
                                    && record.object_id != 0
                                {
                                    if state.sources.contains_key(&record.object_id) {
                                        // Source-scoped media rejections are followed by an
                                        // actionable recovery event such as NEED_FULL_FRAME.
                                        return Ok(());
                                    }
                                    return Ok(());
                                }
                                let request_id = error.request_id;
                                let Some(sent) = state.pending_requests.remove(&request_id) else {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Vivid received an ERROR for an unknown request",
                                    ));
                                };
                                reader_shared.counters.record_control_reply(sent.elapsed());
                                if state.discard_replies.remove(&request_id) {
                                    return Ok(());
                                }
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
                            },
                        ),
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
                            if state.discard_replies.remove(&request_id) {
                                return Ok(());
                            }
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
                                let request = state.next_internal_request_id;
                                state.next_internal_request_id =
                                    state.next_internal_request_id.saturating_sub(1);
                                state.last_rtt_probe = Some(now);
                                let sender_transmit_us = heartbeat_shared
                                    .clock_sampling_enabled
                                    .then(|| monotonic_us(heartbeat_shared.clock_origin, now));
                                state.pending_pings.insert(
                                    request,
                                    PendingPing {
                                        sent: now,
                                        sender_transmit_us,
                                    },
                                );
                                drop(state);
                                if let Err(error) = heartbeat_writer.write_record(
                                    messages::PING,
                                    0,
                                    0,
                                    &messages::clock_ping(request, sender_transmit_us)
                                        .expect("internal PING request IDs are nonzero"),
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
                        let request = state.next_internal_request_id;
                        state.next_internal_request_id =
                            state.next_internal_request_id.saturating_sub(1);
                        state.last_probe_sent = Some(now);
                        state.unanswered_probes = state.unanswered_probes.saturating_add(1);
                        let sender_transmit_us = heartbeat_shared
                            .clock_sampling_enabled
                            .then(|| monotonic_us(heartbeat_shared.clock_origin, now));
                        state.pending_pings.insert(
                            request,
                            PendingPing {
                                sent: now,
                                sender_transmit_us,
                            },
                        );
                        (request, sender_transmit_us)
                    };
                    if let Err(error) = heartbeat_writer.write_record(
                        messages::PING,
                        0,
                        0,
                        &messages::clock_ping(request.0, request.1)
                            .expect("internal PING request IDs are nonzero"),
                    ) {
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
        let sequence = match self
            .writer
            .write_record(record_type, flags, object_id, body)
        {
            Ok(sequence) => sequence,
            Err(error) => {
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pending_requests
                    .remove(&request_id);
                return Err(error);
            }
        };
        emit_control_trace(
            &self.shared.trace,
            TraceDirection::Send,
            record_type,
            object_id,
            sequence,
            body,
            TraceOutcome::Ok,
        );
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

    fn register_source(
        &self,
        source_id: u64,
        credits: Credits,
        revision: SourceRevision,
    ) -> io::Result<Arc<SourceSync>> {
        let source = Arc::new(SourceSync {
            state: Mutex::new(SourceRuntime {
                credits: messages::CreditLedger::new(credits),
                visible: true,
                visible_reasons: 0,
                need_keyframe_epoch: None,
                need_full_frame_reason: None,
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
        state.source_revisions.insert(source_id, revision);
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

    fn take_observation(&self) -> io::Result<Option<ObservationEvent>> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(event) = state.observations.pop_front() {
            return Ok(Some(event));
        }
        if let Some(error) = &state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
        }
        Ok(None)
    }

    fn take_session_event(&self) -> io::Result<Option<SessionEvent>> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(event) = state.session_events.pop_front() {
            return Ok(Some(event));
        }
        if let Some(error) = &state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
        }
        Ok(None)
    }

    fn capability_generation(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capability_generation
    }

    fn revisions(&self) -> RevisionState {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RevisionState {
            scene: state.scene_revision,
            sources: state.source_revisions.clone(),
        }
    }

    fn record_source_revision(&self, source_id: u64, revision: SourceRevision) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .source_revisions
            .insert(source_id, revision);
    }

    fn record_scene_revision(&self, revision: SceneRevision) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scene_revision = revision;
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

    fn clock_estimate(&self) -> Option<ClockEstimate> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clock_estimate
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

fn monotonic_us(origin: Instant, timestamp: Instant) -> u64 {
    u64::try_from(timestamp.saturating_duration_since(origin).as_micros()).unwrap_or(u64::MAX)
}

fn update_clock_estimate(
    estimate: &mut Option<ClockEstimate>,
    sample: messages::ClockSample,
    rejected_samples: u64,
) {
    *estimate = Some(match *estimate {
        None => ClockEstimate {
            offset_us: sample.offset_us,
            delay_us: sample.delay_us,
            responder_processing_us: sample.responder_processing_us,
            accepted_samples: 1,
            rejected_samples,
        },
        Some(current) => ClockEstimate {
            offset_us: i64::try_from(
                (i128::from(current.offset_us) * 7 + i128::from(sample.offset_us)) / 8,
            )
            .unwrap_or(sample.offset_us),
            delay_us: current
                .delay_us
                .saturating_mul(7)
                .saturating_add(sample.delay_us)
                / 8,
            responder_processing_us: current
                .responder_processing_us
                .saturating_mul(7)
                .saturating_add(sample.responder_processing_us)
                / 8,
            accepted_samples: current.accepted_samples.saturating_add(1),
            rejected_samples,
        },
    });
}

impl Drop for ControlDispatcher {
    fn drop(&mut self) {
        close_dispatcher(&self.shared, "Vivid control dispatcher closed".into());
    }
}

fn wait_dispatcher_reply(
    dispatcher: &WaitDispatcher,
    request_id: u64,
    accepted: &[u16],
    expected_object_id: u64,
    active: &AtomicBool,
) -> io::Result<Record> {
    let mut state = dispatcher
        .shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if !active.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Vivid source wait was cancelled",
            ));
        }
        if let Some(record) = state.replies.remove(&request_id) {
            if record.object_id != expected_object_id
                || (record.record_type != messages::ERROR
                    && !accepted.contains(&record.record_type))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Vivid source wait received a mismatched reply",
                ));
            }
            return Ok(record);
        }
        if let Some(error) = &state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
        }
        state = dispatcher
            .shared
            .changed
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

fn cancel_dispatched_wait(dispatcher: &WaitDispatcher, wait_request_id: u64) -> io::Result<()> {
    let cancel_request_id = {
        let mut state = dispatcher
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = &state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
        }
        state.replies.remove(&wait_request_id);
        if state.pending_requests.contains_key(&wait_request_id) {
            state.discard_replies.insert(wait_request_id);
        }
        if state.pending_requests.len() >= MAX_PENDING_CONTROL_RECORDS {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Vivid pending request bound exceeded while cancelling wait",
            ));
        }
        let request_id = state.next_internal_request_id;
        state.next_internal_request_id = state
            .next_internal_request_id
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("Vivid internal request ID space exhausted"))?;
        state.pending_requests.insert(request_id, Instant::now());
        state.discard_replies.insert(request_id);
        dispatcher.shared.changed.notify_all();
        request_id
    };
    let body = messages::cancel_wait(cancel_request_id, wait_request_id)?;
    if let Err(error) = dispatcher
        .writer
        .write_record(messages::CANCEL_WAIT, 0, 0, &body)
    {
        let mut state = dispatcher
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending_requests.remove(&cancel_request_id);
        state.discard_replies.remove(&cancel_request_id);
        return Err(error);
    }
    Ok(())
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

fn push_observation(
    queue: &mut VecDeque<ObservationEvent>,
    event: ObservationEvent,
) -> io::Result<()> {
    let same_subject = |pending: &ObservationEvent| match (pending, &event) {
        (ObservationEvent::Source(left), ObservationEvent::Source(right)) => {
            left.source_id == right.source_id
        }
        (ObservationEvent::Scene(_), ObservationEvent::Scene(_)) => true,
        (ObservationEvent::Playback(left), ObservationEvent::Playback(right)) => {
            left.source_id == right.source_id
        }
        _ => false,
    };
    if let Some(index) = queue.iter().position(same_subject) {
        queue.remove(index);
    }
    if queue.len() >= MAX_PENDING_CONTROL_RECORDS {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "Vivid observation queue exceeded its bound",
        ));
    }
    queue.push_back(event);
    Ok(())
}

fn push_session_event(queue: &mut VecDeque<SessionEvent>, event: SessionEvent) -> io::Result<()> {
    if matches!(event, SessionEvent::Capabilities(_)) {
        queue.retain(|pending| !matches!(pending, SessionEvent::Capabilities(_)));
    }
    if queue.len() >= MAX_PENDING_CONTROL_RECORDS {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "Vivid session-event queue exceeded its bound",
        ));
    }
    queue.push_back(event);
    Ok(())
}

fn apply_caps_changed(
    capability_generation: &mut u64,
    queue: &mut VecDeque<SessionEvent>,
    event: messages::CapsChanged,
) -> io::Result<()> {
    if event.capability_generation <= *capability_generation {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Vivid capability generation did not advance",
        ));
    }
    *capability_generation = event.capability_generation;
    push_session_event(queue, SessionEvent::Capabilities(event))
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
        messages::NEED_FULL_FRAME => {
            let request = messages::parse_need_full_frame(&record.body)?;
            if request.source_id != record.object_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NEED_FULL_FRAME source/object ID mismatch",
                ));
            }
            state.need_full_frame_reason = Some(request.reason);
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
            Self::Direct(connection, trace) => {
                let sequence = connection.write_record(record_type, flags, object_id, body)?;
                emit_control_trace(
                    trace,
                    TraceDirection::Send,
                    record_type,
                    object_id,
                    sequence,
                    body,
                    TraceOutcome::Ok,
                );
                Ok(())
            }
            Self::Live(dispatcher) => dispatcher.write_record(record_type, flags, object_id, body),
        }
    }

    fn clock_estimate(&self) -> Option<ClockEstimate> {
        match self {
            Self::Live(dispatcher) => dispatcher.clock_estimate(),
            Self::Direct(_, _) => None,
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
            Self::Direct(_, _) => Err(io::Error::new(
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
            Self::Direct(_, _) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "trace/dry-run control connections have no replies",
            )),
        }
    }

    fn register_source(
        &self,
        source_id: u64,
        credits: Credits,
        revision: SourceRevision,
    ) -> io::Result<Arc<SourceSync>> {
        match self {
            Self::Live(dispatcher) => dispatcher.register_source(source_id, credits, revision),
            Self::Direct(_, _) => Ok(Arc::new(SourceSync {
                state: Mutex::new(SourceRuntime {
                    credits: messages::CreditLedger::new(credits),
                    visible: true,
                    visible_reasons: 0,
                    need_keyframe_epoch: None,
                    need_full_frame_reason: None,
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
            Self::Direct(_, _) => fallback,
        }
    }

    fn display_state(&self, fallback: DisplayState) -> DisplayState {
        match self {
            Self::Live(dispatcher) => dispatcher.display_state(),
            Self::Direct(_, _) => fallback,
        }
    }

    fn adjusted_minimum_buffer(&self, requested_us: u64) -> u64 {
        match self {
            Self::Live(dispatcher) => dispatcher.adjusted_minimum_buffer(requested_us),
            Self::Direct(_, _) => requested_us,
        }
    }

    fn wait_anchor(&self, anchor_id: u64) -> io::Result<()> {
        match self {
            Self::Live(dispatcher) => dispatcher.wait_anchor(anchor_id),
            Self::Direct(_, _) => Ok(()),
        }
    }

    fn wait_anchor_deadline(&self, anchor_id: u64, deadline: Instant) -> io::Result<bool> {
        match self {
            Self::Live(dispatcher) => dispatcher.wait_anchor_deadline(anchor_id, deadline),
            Self::Direct(_, _) => Ok(true),
        }
    }

    fn wait_dispatcher(&self) -> Option<WaitDispatcher> {
        match self {
            Self::Live(dispatcher) => Some(WaitDispatcher {
                writer: dispatcher.writer.clone(),
                shared: dispatcher.shared.clone(),
            }),
            Self::Direct(_, _) => None,
        }
    }

    fn take_observation(&self) -> io::Result<Option<ObservationEvent>> {
        match self {
            Self::Live(dispatcher) => dispatcher.take_observation(),
            Self::Direct(_, _) => Ok(None),
        }
    }

    fn take_session_event(&self) -> io::Result<Option<SessionEvent>> {
        match self {
            Self::Live(dispatcher) => dispatcher.take_session_event(),
            Self::Direct(_, _) => Ok(None),
        }
    }

    fn capability_generation(&self, fallback: u64) -> u64 {
        match self {
            Self::Live(dispatcher) => dispatcher.capability_generation(),
            Self::Direct(_, _) => fallback,
        }
    }

    fn revisions(
        &self,
        scene: SceneRevision,
        sources: &HashMap<u64, SourceRevision>,
    ) -> RevisionState {
        match self {
            Self::Live(dispatcher) => dispatcher.revisions(),
            Self::Direct(_, _) => RevisionState {
                scene,
                sources: sources.clone(),
            },
        }
    }

    fn record_source_revision(&self, source_id: u64, revision: SourceRevision) {
        if let Self::Live(dispatcher) = self {
            dispatcher.record_source_revision(source_id, revision);
        }
    }

    fn record_scene_revision(&self, revision: SceneRevision) {
        if let Self::Live(dispatcher) = self {
            dispatcher.record_scene_revision(revision);
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
        emit_media_trace(&source.trace, record_type, self.source_id, sequence, bytes);
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

    pub fn take_full_frame_request(&mut self) -> Option<u64> {
        self.source.take_full_frame_request()
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

    /// Send a retained raster delta when its actual wire representation is smaller than the
    /// equivalent full frame; otherwise send the full frame.
    ///
    /// The comparison includes negotiated zstd encoding. The caller supplies the complete
    /// composed framebuffer so the SDK can satisfy the mandatory full-frame fallback without
    /// reconstructing retained state.
    #[allow(clippy::too_many_arguments)]
    pub fn send_raster_delta_or_full(
        &mut self,
        epoch: u32,
        frame_id: u64,
        base_frame_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        operations: &[media::RasterDeltaOperation<'_>],
    ) -> io::Result<RasterSendKind> {
        let operation_limit = self.source.delta_operation_limit.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "raster source was not created in delta mode",
            )
        })?;
        let raw_delta = media::raster_delta_frame_body(
            epoch,
            frame_id,
            base_frame_id,
            0,
            0,
            width,
            height,
            operation_limit,
            operations,
            false,
        )?;
        let mut allocations = 1_u64;
        let mut copied = u64::try_from(raw_delta.len()).unwrap_or(u64::MAX);
        let compressed_delta = if self.raster_zstd {
            let body = media::raster_delta_frame_body(
                epoch,
                frame_id,
                base_frame_id,
                0,
                0,
                width,
                height,
                operation_limit,
                operations,
                true,
            )?;
            allocations = allocations.saturating_add(1);
            copied = copied.saturating_add(u64::try_from(body.len()).unwrap_or(u64::MAX));
            Some(body)
        } else {
            None
        };
        let delta = compressed_delta
            .as_ref()
            .filter(|body| body.len() < raw_delta.len())
            .unwrap_or(&raw_delta);

        let full_raw_prefix =
            media::raster_full_frame_prefix(epoch, frame_id, width, height, rgba.len())?;
        let full_raw_len = usize::try_from(media_body_len(&[full_raw_prefix.as_slice(), rgba])?)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "raster frame is too large")
            })?;
        let compressed_full_len = if self.raster_zstd {
            let body = media::raster_frame_body_with_compression(
                epoch, frame_id, width, height, rgba, true,
            )?;
            allocations = allocations.saturating_add(1);
            copied = copied.saturating_add(u64::try_from(body.len()).unwrap_or(u64::MAX));
            Some(body.len())
        } else {
            None
        };
        let full_len = compressed_full_len
            .filter(|length| *length < full_raw_len)
            .unwrap_or(full_raw_len);

        self.source.counters.record_media_work(allocations, copied);
        if delta.len() >= full_len {
            self.send_raster(epoch, frame_id, width, height, rgba)?;
            Ok(RasterSendKind::Full)
        } else {
            self.send_one_shot_parts(messages::RASTER_FRAME, &[delta.as_slice()])?;
            Ok(RasterSendKind::Delta)
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
    trace: SharedTrace,
    trace_guard: Option<TraceGuard>,
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
    capability_generation: u64,
    scene_revision: SceneRevision,
    source_revisions: HashMap<u64, SourceRevision>,
    unconfirmed_anchors: Vec<u64>,
    label: String,
}

impl ProducerSession {
    fn atomic_body(&self, body: &[u8], metadata: &RequestMetadata) -> io::Result<Vec<u8>> {
        if (!metadata.preconditions.is_empty() || metadata.idempotency_key.is_some())
            && !self.supports(messages::FEATURE_ATOMIC_CONTROL_V1)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks atomic-control-v1",
            ));
        }
        messages::with_request_metadata(body, metadata)
    }

    pub fn connect(config: &ProducerConfig) -> Result<Self, Box<dyn std::error::Error>> {
        config.validate()?;
        let counters = Arc::new(HotPathCounterState::default());
        let trace = Arc::new(Mutex::new(TraceState::default()));
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

        let (
            root_context_id,
            display,
            session_tag,
            accepted_features,
            control_limit,
            initial_scene_revision,
            capability_generation,
        ) = if dry_run {
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
                    settled: true,
                },
                [0; 16],
                accepted_features,
                vivid_protocol::CONTROL_MAX_RECORD_BODY,
                SceneRevision::ZERO,
                1,
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
                    settled: true,
                },
                session_tag,
                accepted_features,
                welcome.maximum_control_body,
                welcome.initial_scene_revision,
                welcome.capability_generation,
            )
        };
        control.set_send_body_limit(control_limit)?;
        let control = if dry_run {
            ClientControl::Direct(control, trace.clone())
        } else {
            ClientControl::Live(ControlDispatcher::start(
                control,
                display,
                initial_scene_revision,
                capability_generation,
                accepted_features.contains(&messages::FEATURE_DESKTOP_INPUT_V1),
                accepted_features.contains(&messages::FEATURE_CLOCK_SAMPLING_V1),
                counters.clone(),
                trace.clone(),
            )?)
        };
        let anchor_key = anchor::derive_key(&token_bytes, &session_tag);

        let mut session = Self {
            control,
            counters,
            trace,
            trace_guard: None,
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
            capability_generation,
            scene_revision: initial_scene_revision,
            source_revisions: HashMap::new(),
            unconfirmed_anchors: Vec::new(),
            label: config.producer.clone(),
        };
        if let Some(trace_dir) = &config.trace_dir {
            session.enable_trace_file(
                &trace_dir.join("events.ndjson"),
                trace_component_for_producer(&config.producer),
            )?;
        } else if let Some(trace_dir) = std::env::var_os("VIVID_DIAGNOSTIC_TRACE_DIR") {
            let trace_path = PathBuf::from(trace_dir).join(format!(
                "{}-{}.ndjson",
                trace_file_stem_for_producer(&config.producer),
                std::process::id()
            ));
            session
                .enable_trace_file(&trace_path, trace_component_for_producer(&config.producer))?;
        }
        Ok(session)
    }

    pub fn hot_path_counters(&self) -> HotPathCounters {
        self.counters.snapshot()
    }

    /// Latest diagnostic four-timestamp clock estimate.
    ///
    /// This estimate is intentionally not consulted by playback, credit, drop, epoch, or queue
    /// sizing code. Existing RTT-based minimum buffering continues to use the independent clean
    /// round-trip estimator.
    pub fn clock_estimate(&self) -> Option<ClockEstimate> {
        self.control.clock_estimate()
    }

    pub fn set_trace_callback(
        &mut self,
        component: DiagnosticTraceComponent,
        callback: impl Fn(DiagnosticTraceRecord) + Send + 'static,
    ) -> io::Result<()> {
        let hint = random_trace_hint()?;
        let guard = TraceGuard::callback(component, TraceHop::Producer, hint, move |record| {
            callback(record)
        })?;
        self.install_trace_guard(guard);
        Ok(())
    }

    pub fn enable_trace_file(
        &mut self,
        path: &Path,
        component: DiagnosticTraceComponent,
    ) -> io::Result<()> {
        let guard = TraceGuard::file(path, component, TraceHop::Producer, random_trace_hint()?)?;
        self.install_trace_guard(guard);
        Ok(())
    }

    fn install_trace_guard(&mut self, guard: TraceGuard) {
        self.trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .emitter = Some(guard.emitter());
        self.trace_guard = Some(guard);
    }

    fn mark_trace_policy(&self, source_id: u64, capture_policy: u64) {
        let mut trace = self
            .trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if capture_policy & messages::CAPTURE_POLICY_REDUCE_DIAGNOSTICS != 0 {
            trace.restricted_sources.insert(source_id);
        }
    }

    pub fn capability_generation(&self) -> u64 {
        self.control
            .capability_generation(self.capability_generation)
    }

    pub fn take_session_event(&self) -> io::Result<Option<SessionEvent>> {
        self.control.take_session_event()
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
        self.create_raster_source_with_metadata(
            source_id,
            width,
            height,
            &RequestMetadata::default(),
        )
    }

    pub fn create_raster_source_with_metadata(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_raster_source_with_policy_and_metadata(source_id, width, height, 0, metadata)
    }

    pub fn create_raster_source_with_policy(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        capture_policy: u64,
    ) -> io::Result<SourceHandle> {
        self.create_raster_source_with_policy_and_metadata(
            source_id,
            width,
            height,
            capture_policy,
            &RequestMetadata::default(),
        )
    }

    pub fn create_raster_source_with_policy_and_metadata(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        capture_policy: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_raster_source_with_options(
            source_id,
            width,
            height,
            capture_policy,
            None,
            metadata,
        )
    }

    pub fn create_raster_source_with_descriptor(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        descriptor: &SourceDescriptor,
    ) -> io::Result<SourceHandle> {
        self.create_raster_source_with_options(
            source_id,
            width,
            height,
            0,
            Some(descriptor),
            &RequestMetadata::default(),
        )
    }

    pub fn create_raster_source_with_options(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.ensure_capture_policy(capture_policy)?;
        self.ensure_source_descriptor(descriptor)?;
        self.mark_trace_policy(source_id, capture_policy);
        let request_id = self.request_id()?;
        let body = self.atomic_body(
            &messages::create_raster_with_extensions(
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
                capture_policy,
                descriptor,
            ),
            metadata,
        )?;
        self.control
            .write_record(messages::CREATE_RASTER, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "raster", None)
    }

    /// Create a raster source that may accept retained-frame delta updates.
    ///
    /// The returned source reports the presenter's effective operation limit. Callers must still
    /// send a full frame first and after every request returned by
    /// [`MediaSender::take_full_frame_request`].
    #[allow(clippy::too_many_arguments)]
    pub fn create_raster_delta_source_with_options(
        &mut self,
        source_id: u64,
        width: u32,
        height: u32,
        operation_limit: u32,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_RASTER_DELTA_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks raster-delta-v1",
            ));
        }
        self.ensure_capture_policy(capture_policy)?;
        self.ensure_source_descriptor(descriptor)?;
        self.mark_trace_policy(source_id, capture_policy);
        let request_id = self.request_id()?;
        let body = self.atomic_body(
            &messages::create_raster_with_update_extensions(
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
                messages::RasterUpdateConfig {
                    mode: messages::RASTER_FULL_FRAME_AND_DELTA,
                    operation_limit,
                },
                capture_policy,
                descriptor,
            )?,
            metadata,
        )?;
        self.control
            .write_record(messages::CREATE_RASTER, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "raster delta", Some(operation_limit))
    }

    pub fn create_video_source<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
    ) -> io::Result<SourceHandle> {
        self.create_video_source_with_metadata(source_id, info, &RequestMetadata::default())
    }

    pub fn create_video_source_with_metadata<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_video_source_with_policy_and_metadata(source_id, info, 0, metadata)
    }

    pub fn create_video_source_with_policy<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
        capture_policy: u64,
    ) -> io::Result<SourceHandle> {
        self.create_video_source_with_policy_and_metadata(
            source_id,
            info,
            capture_policy,
            &RequestMetadata::default(),
        )
    }

    pub fn create_video_source_with_policy_and_metadata<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
        capture_policy: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_video_source_with_options(source_id, info, capture_policy, None, metadata)
    }

    pub fn create_video_source_with_descriptor<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
        descriptor: &SourceDescriptor,
    ) -> io::Result<SourceHandle> {
        self.create_video_source_with_options(
            source_id,
            info,
            0,
            Some(descriptor),
            &RequestMetadata::default(),
        )
    }

    pub fn create_video_source_with_options<C: VideoConfig + ?Sized>(
        &mut self,
        source_id: u64,
        info: &C,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.ensure_capture_policy(capture_policy)?;
        self.ensure_source_descriptor(descriptor)?;
        self.mark_trace_policy(source_id, capture_policy);
        let request_id = self.request_id()?;
        let config = self.scrub_video_description(info.vivid_video_config(source_id));
        let body = self.atomic_body(
            &messages::create_video_with_extensions(
                request_id,
                &config,
                capture_policy,
                descriptor,
            ),
            metadata,
        )?;
        self.control
            .write_record(messages::CREATE_VIDEO, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "video", None)
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
        self.create_audio_source_with_metadata(
            source_id,
            linked_video_source_id,
            info,
            &RequestMetadata::default(),
        )
    }

    pub fn create_audio_source_with_metadata<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_audio_source_with_policy_and_metadata(
            source_id,
            linked_video_source_id,
            info,
            0,
            metadata,
        )
    }

    pub fn create_audio_source_with_policy<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
        capture_policy: u64,
    ) -> io::Result<SourceHandle> {
        self.create_audio_source_with_policy_and_metadata(
            source_id,
            linked_video_source_id,
            info,
            capture_policy,
            &RequestMetadata::default(),
        )
    }

    pub fn create_audio_source_with_policy_and_metadata<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
        capture_policy: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_audio_source_with_options(
            source_id,
            linked_video_source_id,
            info,
            capture_policy,
            None,
            metadata,
        )
    }

    pub fn create_audio_source_with_descriptor<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
        descriptor: &SourceDescriptor,
    ) -> io::Result<SourceHandle> {
        self.create_audio_source_with_options(
            source_id,
            linked_video_source_id,
            info,
            0,
            Some(descriptor),
            &RequestMetadata::default(),
        )
    }

    pub fn create_audio_source_with_options<C: AudioConfig + ?Sized>(
        &mut self,
        source_id: u64,
        linked_video_source_id: Option<u64>,
        info: &C,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_AUDIO_ACCESS_UNIT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks audio-access-unit-v1",
            ));
        }
        self.ensure_capture_policy(capture_policy)?;
        self.ensure_source_descriptor(descriptor)?;
        self.mark_trace_policy(source_id, capture_policy);
        let config = self
            .scrub_audio_description(info.vivid_audio_config(source_id, linked_video_source_id));
        let request_id = self.request_id()?;
        let body = self.atomic_body(
            &messages::create_audio_with_extensions(
                request_id,
                &config,
                capture_policy,
                descriptor,
            ),
            metadata,
        )?;
        self.control
            .write_record(messages::CREATE_AUDIO, 0, source_id, &body)?;
        self.source_ready(request_id, source_id, "audio", None)
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
        self.create_linked_av_sources_with_policy(
            video_source_id,
            video,
            0,
            audio_source_id,
            audio,
            0,
        )
    }

    pub fn create_linked_av_sources_with_policy<V, A>(
        &mut self,
        video_source_id: u64,
        video: &V,
        video_capture_policy: u64,
        audio_source_id: u64,
        audio: &A,
        audio_capture_policy: u64,
    ) -> io::Result<(SourceHandle, io::Result<SourceHandle>)>
    where
        V: VideoConfig + ?Sized,
        A: AudioConfig + ?Sized,
    {
        self.create_linked_av_sources_with_options(
            video_source_id,
            video,
            video_capture_policy,
            None,
            audio_source_id,
            audio,
            audio_capture_policy,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_linked_av_sources_with_options<V, A>(
        &mut self,
        video_source_id: u64,
        video: &V,
        video_capture_policy: u64,
        video_descriptor: Option<&SourceDescriptor>,
        audio_source_id: u64,
        audio: &A,
        audio_capture_policy: u64,
        audio_descriptor: Option<&SourceDescriptor>,
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
        self.ensure_capture_policy(video_capture_policy)?;
        self.ensure_capture_policy(audio_capture_policy)?;
        self.ensure_source_descriptor(video_descriptor)?;
        self.ensure_source_descriptor(audio_descriptor)?;
        self.mark_trace_policy(video_source_id, video_capture_policy);
        self.mark_trace_policy(audio_source_id, audio_capture_policy);
        let video_request = self.request_id()?;
        let audio_request = self.request_id()?;
        self.control.write_record(
            messages::CREATE_VIDEO,
            0,
            video_source_id,
            &messages::create_video_with_extensions(
                video_request,
                &self.scrub_video_description(video.vivid_video_config(video_source_id)),
                video_capture_policy,
                video_descriptor,
            ),
        )?;
        self.control.write_record(
            messages::CREATE_AUDIO,
            0,
            audio_source_id,
            &messages::create_audio_with_extensions(
                audio_request,
                &self.scrub_audio_description(
                    audio.vivid_audio_config(audio_source_id, Some(video_source_id)),
                ),
                audio_capture_policy,
                audio_descriptor,
            ),
        )?;
        let video = self.source_ready(video_request, video_source_id, "video", None)?;
        let audio = self.source_ready(audio_request, audio_source_id, "audio", None);
        Ok((video, audio))
    }

    #[allow(dead_code)] // Explicit diagnostic/conformance API; normal playback creates directly.
    pub fn probe_video_config<C: VideoConfig + ?Sized>(&mut self, info: &C) -> io::Result<bool> {
        Ok(self.probe_video_support(info)?.supported)
    }

    pub fn probe_video_support<C: VideoConfig + ?Sized>(
        &mut self,
        info: &C,
    ) -> io::Result<messages::CapabilitySupport> {
        let config = self.scrub_video_description(info.vivid_video_config(0));
        for _ in 0..3 {
            let request = self.request_id()?;
            self.control.write_record(
                messages::PROBE_VIDEO_CONFIG,
                0,
                0,
                &messages::probe_video_config(request, &config),
            )?;
            let support = if self.dry_run {
                messages::CapabilitySupport {
                    supported: true,
                    decoder: config.codec.to_owned(),
                    capability_generation: self.capability_generation(),
                }
            } else {
                let reply = self.wait_for_reply(request, &[messages::VIDEO_SUPPORT], 0)?;
                messages::parse_capability_support(&reply.body)?
            };
            if support.capability_generation >= self.capability_generation() {
                return Ok(support);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Vivid video capabilities changed repeatedly during the probe",
        ))
    }

    #[allow(dead_code)] // Explicit diagnostic/conformance API; normal playback creates directly.
    pub fn probe_audio_config<C: AudioConfig + ?Sized>(&mut self, info: &C) -> io::Result<bool> {
        Ok(self.probe_audio_support(info)?.supported)
    }

    pub fn probe_audio_support<C: AudioConfig + ?Sized>(
        &mut self,
        info: &C,
    ) -> io::Result<messages::CapabilitySupport> {
        let config = self.scrub_audio_description(info.vivid_audio_config(0, None));
        for _ in 0..3 {
            let request = self.request_id()?;
            self.control.write_record(
                messages::PROBE_AUDIO_CONFIG,
                0,
                0,
                &messages::probe_audio_config(request, &config),
            )?;
            let support = if self.dry_run {
                messages::CapabilitySupport {
                    supported: true,
                    decoder: config.codec.to_owned(),
                    capability_generation: self.capability_generation(),
                }
            } else {
                let reply = self.wait_for_reply(request, &[messages::AUDIO_SUPPORT], 0)?;
                messages::parse_capability_support(&reply.body)?
            };
            if support.capability_generation >= self.capability_generation() {
                return Ok(support);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Vivid audio capabilities changed repeatedly during the probe",
        ))
    }

    pub fn create_image_source(&mut self, config: &ImageSourceConfig) -> io::Result<SourceHandle> {
        self.create_image_source_with_metadata(config, &RequestMetadata::default())
    }

    pub fn create_image_source_with_metadata(
        &mut self,
        config: &ImageSourceConfig,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_policy_and_metadata(config, 0, metadata)
    }

    pub fn create_image_source_with_policy(
        &mut self,
        config: &ImageSourceConfig,
        capture_policy: u64,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_policy_and_metadata(
            config,
            capture_policy,
            &RequestMetadata::default(),
        )
    }

    pub fn create_image_source_with_policy_and_metadata(
        &mut self,
        config: &ImageSourceConfig,
        capture_policy: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_options(config, capture_policy, None, metadata)
    }

    pub fn create_image_source_with_descriptor(
        &mut self,
        config: &ImageSourceConfig,
        descriptor: &SourceDescriptor,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_options(
            config,
            0,
            Some(descriptor),
            &RequestMetadata::default(),
        )
    }

    pub fn create_image_source_with_options(
        &mut self,
        config: &ImageSourceConfig,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_cache_options(
            config,
            false,
            capture_policy,
            descriptor,
            metadata,
        )
    }

    pub fn create_image_source_with_cache(
        &mut self,
        config: &ImageSourceConfig,
    ) -> io::Result<SourceHandle> {
        self.create_image_source_with_cache_options(
            config,
            true,
            0,
            None,
            &RequestMetadata::default(),
        )
    }

    pub fn create_image_source_with_cache_options(
        &mut self,
        config: &ImageSourceConfig,
        cache_lookup: bool,
        capture_policy: u64,
        descriptor: Option<&SourceDescriptor>,
        metadata: &RequestMetadata,
    ) -> io::Result<SourceHandle> {
        if !self.supports(messages::FEATURE_ENCODED_IMAGE_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks encoded-image-v1",
            ));
        }
        if cache_lookup && !self.supports(messages::FEATURE_IMAGE_CACHE_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks image-cache-v1",
            ));
        }
        self.ensure_capture_policy(capture_policy)?;
        self.ensure_source_descriptor(descriptor)?;
        self.mark_trace_policy(config.source_id, capture_policy);
        let request_id = self.request_id()?;
        let body = self.atomic_body(
            &messages::create_image_with_cache_extensions(
                request_id,
                config,
                cache_lookup,
                capture_policy,
                descriptor,
            )?,
            metadata,
        )?;
        self.control
            .write_record(messages::CREATE_IMAGE, 0, config.source_id, &body)?;
        self.source_ready(request_id, config.source_id, "image", None)
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
        self.destroy_source_with_metadata(source_id, &RequestMetadata::default())
    }

    pub fn destroy_source_with_metadata(
        &mut self,
        source_id: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = self.atomic_body(&messages::destroy_source(request_id, source_id), metadata)?;
        self.control
            .write_record(messages::DESTROY_SOURCE, 0, source_id, &body)?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn set_source_policy(&mut self, source_id: u64, capture_policy: u64) -> io::Result<()> {
        self.ensure_capture_policy(capture_policy)?;
        self.mark_trace_policy(source_id, capture_policy);
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::SET_SOURCE_POLICY,
            0,
            source_id,
            &messages::set_source_policy(request_id, source_id, capture_policy),
        )?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn update_source_descriptor(
        &mut self,
        source_id: u64,
        descriptor: &SourceDescriptor,
    ) -> io::Result<()> {
        self.update_source_descriptor_with_metadata(
            source_id,
            descriptor,
            &RequestMetadata::default(),
        )
    }

    pub fn update_source_descriptor_with_metadata(
        &mut self,
        source_id: u64,
        descriptor: &SourceDescriptor,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        self.ensure_source_descriptor(Some(descriptor))?;
        let request_id = self.request_id()?;
        let body = self.atomic_body(
            &messages::update_source_descriptor(request_id, source_id, descriptor),
            metadata,
        )?;
        self.control
            .write_record(messages::UPDATE_SOURCE_DESCRIPTOR, 0, source_id, &body)?;
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
            return Err(io::Error::other(PresenterError::from(error)));
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
        &mut self,
        source: &SourceHandle,
        kind: ConnectionKind,
    ) -> io::Result<MediaChannel> {
        if !source.media_connection_required {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache-hit image source does not require a media connection",
            ));
        }
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
        let attach = connection.write_record(
            messages::ATTACH_CHANNEL,
            0,
            source.id,
            &messages::attach_channel(&source.ticket),
        );
        if let Err(attach_error) = attach {
            let resolution = self.resolve_attachment_failure(source.id);
            match resolution {
                Ok(AttachmentResolution::NotConsumed) => {
                    let primary = self.endpoint.as_ref().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "missing Vivid endpoint")
                    })?;
                    connection = Connection::open(primary, kind)?;
                    connection.write_record(
                        messages::ATTACH_CHANNEL,
                        0,
                        source.id,
                        &messages::attach_channel(&source.ticket),
                    )?;
                }
                Ok(resolution) => {
                    return Err(io::Error::other(AttachmentError {
                        source_id: source.id,
                        resolution,
                        diagnostic: attach_error.to_string(),
                    }));
                }
                Err(query_error) => {
                    return Err(io::Error::other(AttachmentError {
                        source_id: source.id,
                        resolution: AttachmentResolution::Indeterminate,
                        diagnostic: format!(
                            "{attach_error}; attachment resolution failed: {query_error}"
                        ),
                    }));
                }
            }
        }
        connection.set_send_body_limit(u32::try_from(source.record_limit).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "source body limit exceeds u32")
        })?)?;
        Ok(MediaChannel {
            connection,
            source_id: source.id,
        })
    }

    pub fn open_media_sender(
        &mut self,
        mut source: SourceHandle,
        kind: ConnectionKind,
    ) -> io::Result<MediaSender> {
        let channel = self.open_media_channel(&source, kind)?;
        source.attachment_generation = source
            .attachment_generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("attachment generation exhausted"))?;
        Ok(MediaSender {
            source,
            channel,
            dry_run: self.dry_run,
            raster_zstd: self.supports(messages::FEATURE_RASTER_ZSTD_V1),
        })
    }

    /// Resolve whether a media ticket was consumed after an uncertain attachment write.
    pub fn resolve_attachment_failure(
        &mut self,
        source_id: u64,
    ) -> io::Result<AttachmentResolution> {
        if !self.supports(messages::FEATURE_OBSERVABILITY_CORE_V1) {
            return Ok(AttachmentResolution::RecreateRequired);
        }
        let status = self.query_source(source_id)?;
        self.resolve_attachment_status(&status)
    }

    fn resolve_attachment_status(&self, status: &SourceStatus) -> io::Result<AttachmentResolution> {
        match status.attachment_state {
            messages::ATTACHMENT_NEVER => Ok(AttachmentResolution::NotConsumed),
            messages::ATTACHMENT_ATTACHED => Ok(AttachmentResolution::ConsumedAttached {
                generation: status.attachment_generation,
            }),
            messages::ATTACHMENT_CLOSED => Ok(AttachmentResolution::ConsumedClosed {
                generation: status.attachment_generation,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SOURCE_STATUS contains an unknown attachment state",
            )),
        }
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

    fn ensure_capture_policy(&self, capture_policy: u64) -> io::Result<()> {
        messages::validate_capture_policy(capture_policy)?;
        if capture_policy != 0 && !self.supports(messages::FEATURE_SOURCE_CAPTURE_POLICY_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks source-capture-policy-v1",
            ));
        }
        Ok(())
    }

    fn ensure_source_descriptor(&self, descriptor: Option<&SourceDescriptor>) -> io::Result<()> {
        let Some(descriptor) = descriptor else {
            return Ok(());
        };
        messages::validate_source_descriptor(descriptor)?;
        if !self.supports(messages::FEATURE_SOURCE_DESCRIPTOR_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks source-descriptor-v1",
            ));
        }
        Ok(())
    }

    pub fn take_desktop_input(&mut self) -> io::Result<Option<DesktopInputEvent>> {
        match &self.control {
            ClientControl::Live(dispatcher) => dispatcher.take_desktop_input(),
            ClientControl::Direct(_, _) => Ok(None),
        }
    }

    pub fn revision_state(&self) -> RevisionState {
        self.control
            .revisions(self.scene_revision, &self.source_revisions)
    }

    pub fn take_observation(&self) -> io::Result<Option<ObservationEvent>> {
        self.ensure_observability()?;
        self.control.take_observation()
    }

    pub fn set_observation(&mut self, class_mask: u64) -> io::Result<()> {
        self.ensure_observability()?;
        let request_id = self.request_id()?;
        let body = messages::set_observation(request_id, class_mask)?;
        self.control
            .write_record(messages::SET_OBSERVATION, 0, 0, &body)?;
        self.wait_for_ok(request_id, 0)
    }

    pub fn create_context(
        &mut self,
        request: &messages::CreateContextRequest,
    ) -> io::Result<messages::ContextReady> {
        if !self.supports(messages::FEATURE_DELEGATED_CONTEXT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks delegated-context-v1",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::create_context(request_id, request)?;
        self.control
            .write_record(messages::CREATE_CONTEXT, 0, request.context_id, &body)?;
        let record =
            self.wait_for_reply(request_id, &[messages::CONTEXT_READY], request.context_id)?;
        let (reply_request_id, ready) = messages::parse_context_ready(&record.body)?;
        if reply_request_id != request_id || ready.context_id != request.context_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CONTEXT_READY correlation mismatch",
            ));
        }
        Ok(ready)
    }

    pub fn delegate_context(&mut self, context_id: u64) -> io::Result<DelegatedCapability> {
        if !self.supports(messages::FEATURE_DELEGATED_CONTEXT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks delegated-context-v1",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::delegate_context(request_id, context_id);
        self.control
            .write_record(messages::DELEGATE_CONTEXT, 0, context_id, &body)?;
        let record =
            self.wait_for_reply(request_id, &[messages::CONTEXT_CAPABILITY], context_id)?;
        let (reply_request_id, reply_context_id, capability) =
            messages::parse_context_capability(&record.body)?;
        if reply_request_id != request_id || reply_context_id != context_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CONTEXT_CAPABILITY correlation mismatch",
            ));
        }
        Ok(DelegatedCapability(capability))
    }

    pub fn revoke_context(&mut self, context_id: u64) -> io::Result<()> {
        if !self.supports(messages::FEATURE_DELEGATED_CONTEXT_V1) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter lacks delegated-context-v1",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::revoke_context(request_id, context_id);
        self.control
            .write_record(messages::REVOKE_CONTEXT, 0, context_id, &body)?;
        self.wait_for_ok(request_id, context_id)
    }

    pub fn query_source(&mut self, source_id: u64) -> io::Result<SourceStatus> {
        self.ensure_observability()?;
        if self.dry_run {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QUERY_SOURCE requires a live presenter",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::query_source(request_id, source_id)?;
        self.control
            .write_record(messages::QUERY_SOURCE, 0, source_id, &body)?;
        let record = self.wait_for_reply(request_id, &[messages::SOURCE_STATUS], source_id)?;
        let (reply_request_id, status) = messages::parse_source_status(&record.body)?;
        if reply_request_id != request_id || status.source_id != source_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SOURCE_STATUS correlation mismatch",
            ));
        }
        self.source_revisions
            .insert(source_id, status.source_revision);
        self.control
            .record_source_revision(source_id, status.source_revision);
        Ok(status)
    }

    pub fn query_scene_page(&mut self, query: SceneQuery) -> io::Result<SceneStatus> {
        self.ensure_observability()?;
        if self.dry_run {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QUERY_SCENE requires a live presenter",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::query_scene(request_id, &query)?;
        self.control
            .write_record(messages::QUERY_SCENE, 0, 0, &body)?;
        let record = self.wait_for_reply(request_id, &[messages::SCENE_STATUS], 0)?;
        let (reply_request_id, status) = messages::parse_scene_status(&record.body)?;
        if reply_request_id != request_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SCENE_STATUS correlation mismatch",
            ));
        }
        self.scene_revision = status.scene_revision;
        self.control.record_scene_revision(status.scene_revision);
        Ok(status)
    }

    /// Fetch a scene through bounded pagination.
    pub fn query_scene(
        &mut self,
        maximum_nodes_per_page: u64,
        maximum_pages: usize,
    ) -> io::Result<SceneStatus> {
        if maximum_nodes_per_page == 0 || maximum_pages == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scene pagination bounds must be non-zero",
            ));
        }
        let mut status = self.query_scene_page(SceneQuery {
            expected_revision: None,
            cursor: None,
            maximum_nodes: Some(maximum_nodes_per_page),
        })?;
        let revision = status.scene_revision;
        let total_nodes = status.total_nodes;
        let mut nodes = std::mem::take(&mut status.nodes);
        let mut cursor = status.cursor;
        for _ in 1..maximum_pages {
            let Some(next) = cursor else {
                return Ok(SceneStatus {
                    scene_revision: revision,
                    nodes,
                    cursor: None,
                    total_nodes,
                });
            };
            let mut page = self.query_scene_page(SceneQuery {
                expected_revision: Some(revision),
                cursor: Some(next),
                maximum_nodes: Some(maximum_nodes_per_page),
            })?;
            if page.scene_revision != revision || page.total_nodes != total_nodes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "scene changed during bounded pagination",
                ));
            }
            nodes.append(&mut page.nodes);
            cursor = page.cursor;
        }
        if cursor.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "scene pagination exceeded the caller's page bound",
            ));
        }
        Ok(SceneStatus {
            scene_revision: revision,
            nodes,
            cursor: None,
            total_nodes,
        })
    }

    pub fn query_anchor(&mut self, anchor_id: u64) -> io::Result<AnchorStatus> {
        self.ensure_observability()?;
        if self.dry_run {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QUERY_ANCHOR requires a live presenter",
            ));
        }
        let request_id = self.request_id()?;
        let body = messages::query_anchor(request_id, anchor_id)?;
        self.control
            .write_record(messages::QUERY_ANCHOR, 0, anchor_id, &body)?;
        let record = self.wait_for_reply(request_id, &[messages::ANCHOR_STATUS], anchor_id)?;
        let (reply_request_id, status) = messages::parse_anchor_status(&record.body)?;
        if reply_request_id != request_id || status.anchor_id != anchor_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ANCHOR_STATUS correlation mismatch",
            ));
        }
        Ok(status)
    }

    pub fn query_limits(&mut self) -> io::Result<LimitsStatus> {
        self.ensure_observability()?;
        if self.dry_run {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QUERY_LIMITS requires a live presenter",
            ));
        }
        let request_id = self.request_id()?;
        self.control.write_record(
            messages::QUERY_LIMITS,
            0,
            0,
            &messages::query_limits(request_id),
        )?;
        let record = self.wait_for_reply(request_id, &[messages::LIMITS_STATUS], 0)?;
        let (reply_request_id, status) = messages::parse_limits_status(&record.body)?;
        if reply_request_id != request_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LIMITS_STATUS correlation mismatch",
            ));
        }
        Ok(status)
    }

    pub fn begin_wait_source(&mut self, wait: WaitSource) -> io::Result<SourceWaitHandle> {
        self.ensure_observability()?;
        let request_id = self.request_id()?;
        let body = messages::wait_source(request_id, wait)?;
        self.control
            .write_record(messages::WAIT_SOURCE, 0, wait.source_id, &body)?;
        Ok(SourceWaitHandle {
            dispatcher: self.control.wait_dispatcher(),
            request_id,
            source_id: wait.source_id,
            synthetic: self.dry_run.then_some(WaitSatisfied {
                source_id: wait.source_id,
                source_revision: self
                    .source_revisions
                    .get(&wait.source_id)
                    .copied()
                    .unwrap_or(SourceRevision::ZERO),
                condition: wait.condition,
                observed_value: wait.value,
            }),
            completed: false,
            active: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn wait_source(&mut self, wait: WaitSource) -> io::Result<WaitSatisfied> {
        self.begin_wait_source(wait)?.wait()
    }

    pub fn play_at(
        &mut self,
        source_id: u64,
        start_pts_us: i64,
        minimum_buffer_us: u64,
    ) -> io::Result<()> {
        self.play_at_with_metadata(
            source_id,
            start_pts_us,
            minimum_buffer_us,
            &RequestMetadata::default(),
        )
    }

    pub fn play_at_with_metadata(
        &mut self,
        source_id: u64,
        start_pts_us: i64,
        minimum_buffer_us: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let minimum_buffer_us = self
            .control
            .adjusted_minimum_buffer(minimum_buffer_us)
            .min(500_000);
        let mut play = messages::PlayRequest::baseline(source_id, minimum_buffer_us);
        play.start_pts_us = start_pts_us;
        play.maximum_latency_us = play.maximum_latency_us.max(play.minimum_buffer_us);
        let body = self.atomic_body(&messages::play_request(request_id, &play), metadata)?;
        self.control
            .write_record(messages::PLAY, 0, source_id, &body)?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn wait_until_playing(
        &mut self,
        source_id: u64,
        timeout: Duration,
    ) -> io::Result<WaitSatisfied> {
        let timeout_us = u64::try_from(timeout.as_micros()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "playback wait timeout is too large",
            )
        })?;
        self.wait_source(WaitSource {
            source_id,
            condition: messages::WAIT_PLAYBACK_STARTED,
            value: None,
            timeout_us,
        })
    }

    pub fn play_and_wait_until_playing(
        &mut self,
        source_id: u64,
        start_pts_us: i64,
        minimum_buffer_us: u64,
        timeout: Duration,
    ) -> io::Result<WaitSatisfied> {
        self.play_at(source_id, start_pts_us, minimum_buffer_us)?;
        self.wait_until_playing(source_id, timeout)
    }

    pub fn eos(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        self.eos_with_metadata(source_id, epoch, &RequestMetadata::default())
    }

    pub fn eos_sender(&mut self, sender: &MediaSender, epoch: u32) -> io::Result<()> {
        let source = sender.source();
        if self.supports(messages::FEATURE_MEDIA_ORDER_BARRIER_V1)
            && source.attachment_generation() != 0
            && source.last_record_sequence() != 0
        {
            self.eos_with_media_order(
                source.id,
                epoch,
                source.attachment_generation(),
                source.last_record_sequence(),
            )
        } else {
            self.eos(source.id, epoch)
        }
    }

    pub fn eos_with_media_order(
        &mut self,
        source_id: u64,
        epoch: u32,
        attachment_generation: u64,
        final_record_sequence: u64,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = messages::eos_with_barrier(
            request_id,
            source_id,
            epoch,
            attachment_generation,
            final_record_sequence,
        );
        self.control
            .write_record(messages::EOS, 0, source_id, &body)?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn eos_with_metadata(
        &mut self,
        source_id: u64,
        epoch: u32,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = self.atomic_body(&messages::eos(request_id, source_id, epoch), metadata)?;
        self.control
            .write_record(messages::EOS, 0, source_id, &body)?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn drain(&mut self, source_id: u64) -> io::Result<()> {
        self.drain_with_metadata(source_id, &RequestMetadata::default())
    }

    pub fn drain_with_metadata(
        &mut self,
        source_id: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = self.atomic_body(&messages::drain(request_id, source_id), metadata)?;
        self.control
            .write_record(messages::DRAIN, 0, source_id, &body)?;
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
            return Err(presenter_error(&record.body)?);
        }
        Ok(())
    }

    pub fn pause(&mut self, source_id: u64) -> io::Result<()> {
        self.pause_with_metadata(source_id, &RequestMetadata::default())
    }

    pub fn pause_with_metadata(
        &mut self,
        source_id: u64,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = self.atomic_body(&messages::pause(request_id, source_id), metadata)?;
        self.control
            .write_record(messages::PAUSE, 0, source_id, &body)?;
        self.wait_for_ok(request_id, source_id)
    }

    pub fn flush(&mut self, source_id: u64, epoch: u32) -> io::Result<()> {
        self.flush_with_metadata(source_id, epoch, &RequestMetadata::default())
    }

    pub fn flush_with_metadata(
        &mut self,
        source_id: u64,
        epoch: u32,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.request_id()?;
        let body = self.atomic_body(&messages::flush(request_id, source_id, epoch), metadata)?;
        self.control
            .write_record(messages::FLUSH, 0, source_id, &body)?;
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
        requested_delta_operation_limit: Option<u32>,
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
                rolling_byte_window: SYNTHETIC_CREDITS,
                rolling_packet_window: SYNTHETIC_CREDITS,
                initial_source_revision: vivid_protocol::revision::SourceRevision::ZERO,
                media_connection_required: true,
                delta_operation_limit: requested_delta_operation_limit.map(u64::from),
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
        match (requested_delta_operation_limit, ready.delta_operation_limit) {
            (None, None) => {}
            (Some(requested), Some(effective))
                if effective != 0 && effective <= u64::from(requested) => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SOURCE_READY raster delta operation limit does not match the request",
                ));
            }
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
        self.source_revisions
            .insert(source_id, ready.initial_source_revision);
        let state =
            self.control
                .register_source(source_id, credits, ready.initial_source_revision)?;
        Ok(SourceHandle {
            id: source_id,
            ticket: ready.media_ticket,
            record_limit: u64::from(ready.max_media_body),
            state,
            counters: self.counters.clone(),
            trace: self.trace.clone(),
            last_record_sequence: 0,
            attachment_generation: 0,
            rolling_byte_window: ready.rolling_byte_window,
            rolling_packet_window: ready.rolling_packet_window,
            delta_operation_limit: ready
                .delta_operation_limit
                .map(u32::try_from)
                .transpose()
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "SOURCE_READY raster delta operation limit exceeds u32",
                    )
                })?,
            media_connection_required: ready.media_connection_required,
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

    fn ensure_observability(&self) -> io::Result<()> {
        if self.supports(messages::FEATURE_OBSERVABILITY_CORE_V1) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "presenter did not negotiate observability-core-v1",
            ))
        }
    }

    fn wait_for_reply(
        &mut self,
        request_id: u64,
        accepted: &[u16],
        expected_object_id: u64,
    ) -> io::Result<Record> {
        let record = self.wait_for_reply_raw(request_id, accepted, expected_object_id)?;
        if record.record_type == messages::ERROR {
            return Err(presenter_error(&record.body)?);
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

    pub fn rolling_byte_window(&self) -> u64 {
        self.rolling_byte_window
    }

    pub fn rolling_packet_window(&self) -> u64 {
        self.rolling_packet_window
    }

    pub fn delta_operation_limit(&self) -> Option<u32> {
        self.delta_operation_limit
    }

    pub fn media_connection_required(&self) -> bool {
        self.media_connection_required
    }

    pub fn is_cache_hit(&self) -> bool {
        !self.media_connection_required
    }

    /// Convert the presenter's steady-state advertisement into local bounded-queue limits.
    pub fn media_queue_limits(&self) -> MediaQueueLimits {
        MediaQueueLimits {
            max_bytes: usize::try_from(self.rolling_byte_window)
                .unwrap_or(usize::MAX)
                .max(1),
            max_packets: usize::try_from(self.rolling_packet_window)
                .unwrap_or(usize::MAX)
                .max(1),
        }
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

    pub fn take_full_frame_request(&mut self) -> Option<u64> {
        self.state
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .need_full_frame_reason
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
            authentication_kind: config.authentication_kind,
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
            authentication_kind: messages::AUTHENTICATION_WINDOW_ROOT,
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

    #[test]
    fn capability_session_events_coalesce_to_the_newest_generation() {
        let mut events = VecDeque::new();
        let mut generation = 1;
        apply_caps_changed(
            &mut generation,
            &mut events,
            messages::CapsChanged {
                capability_generation: 2,
                reason_mask: messages::CAPS_CHANGE_DECODER_AVAILABILITY,
            },
        )
        .unwrap();
        apply_caps_changed(
            &mut generation,
            &mut events,
            messages::CapsChanged {
                capability_generation: 4,
                reason_mask: messages::CAPS_CHANGE_RESOURCE_PRESSURE,
            },
        )
        .unwrap();
        assert_eq!(
            events,
            VecDeque::from([SessionEvent::Capabilities(messages::CapsChanged {
                capability_generation: 4,
                reason_mask: messages::CAPS_CHANGE_RESOURCE_PRESSURE,
            })])
        );
        assert!(
            apply_caps_changed(
                &mut generation,
                &mut events,
                messages::CapsChanged {
                    capability_generation: 4,
                    reason_mask: messages::CAPS_CHANGE_DEVICE_AVAILABILITY,
                },
            )
            .is_err()
        );
        assert_eq!(generation, 4);
        assert_eq!(trace_file_stem_for_producer("../../outside"), "vivid-sdk");
    }

    #[test]
    fn sdk_trace_callback_is_secret_free_and_restricts_source_ids() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let output = records.clone();
        let guard = TraceGuard::callback(
            TraceComponent::Sdk,
            TraceHop::Producer,
            [0x44; 16],
            move |record| output.lock().unwrap().push(record),
        )
        .unwrap();
        let trace = Arc::new(Mutex::new(TraceState {
            emitter: Some(guard.emitter()),
            restricted_sources: HashSet::from([55]),
        }));
        let token = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";
        let hello = messages::encode_hello(
            9,
            &messages::HelloConfig {
                minimum_major: 1,
                minimum_minor: 1,
                maximum_major: 1,
                maximum_minor: 1,
                token,
                producer: "private sdk producer",
                producer_version: "1",
                required_features: &[],
                optional_features: &[],
                maximum_record_body: 4096,
                authentication_kind: messages::AUTHENTICATION_WINDOW_ROOT,
                preserved_fields: &[],
            },
        );
        emit_control_trace(
            &trace,
            TraceDirection::Send,
            messages::HELLO,
            0,
            1,
            &hello,
            TraceOutcome::Ok,
        );
        emit_media_trace(&trace, messages::VIDEO_PACKET, 55, 2, 32);
        let capability = [b'K'; messages::CONTEXT_CAPABILITY_BYTES];
        let capability_reply = messages::context_capability(10, 5, &capability);
        emit_control_trace(
            &trace,
            TraceDirection::Receive,
            messages::CONTEXT_CAPABILITY,
            5,
            3,
            &capability_reply,
            TraceOutcome::Ok,
        );
        let ticket = [b'T'; 32];
        let ready = messages::source_ready(
            11,
            56,
            &ticket,
            messages::Credits {
                bytes: 4096,
                packets: 1,
                fragments: 0,
            },
            4096,
        );
        emit_control_trace(
            &trace,
            TraceDirection::Receive,
            messages::SOURCE_READY,
            56,
            4,
            &ready,
            TraceOutcome::Ok,
        );
        let cookie = "vvbridge_session=COOKIE-SENTINEL";
        let marker = "\u{1b}_GVIVID1;MARKER-SENTINEL\u{1b}\\";
        let diagnostic = messages::error(
            12,
            messages::ERROR_BAD_MESSAGE,
            &format!("{cookie} {marker}"),
        );
        emit_control_trace(
            &trace,
            TraceDirection::Receive,
            messages::ERROR,
            0,
            5,
            &diagnostic,
            TraceOutcome::Error,
        );
        drop(trace);
        drop(guard);

        let records = records.lock().unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records[1].outcome, TraceOutcome::Restricted);
        assert_eq!(records[1].object_id, None);
        let trace = records
            .iter()
            .map(|record| record.ndjson_line())
            .collect::<String>();
        for forbidden in [
            token,
            "private sdk producer",
            "\"object_id\":55",
            "endpoint",
            "descriptor",
            "KKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKK",
            "TTTTTTTTTTTTTTTTTTTTTTTTTTTTTTTT",
            cookie,
            "MARKER-SENTINEL",
        ] {
            assert!(!trace.contains(forbidden), "trace leaked {forbidden}");
        }
    }

    #[test]
    fn presenter_error_exposes_structured_detail_without_parsing_diagnostic() {
        let detail = messages::ErrorDetail::limit(messages::LIMIT_SOURCES, 64, 64);
        let body = messages::error_with_detail(
            9,
            messages::ERROR_LIMIT_EXCEEDED,
            false,
            &detail,
            "display only",
        )
        .unwrap();
        let error = presenter_error(&body).unwrap();
        let structured = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<PresenterError>())
            .unwrap();
        assert_eq!(structured.code, messages::ERROR_LIMIT_EXCEEDED);
        assert_eq!(
            structured.detail.get_u64(messages::ERROR_DETAIL_LIMIT_ID),
            Some(messages::LIMIT_SOURCES)
        );
        assert_eq!(
            structured.detail.get_u64(messages::ERROR_DETAIL_MAXIMUM),
            Some(64)
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
                        settled: true,
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

    #[cfg(unix)]
    #[test]
    fn dropping_source_wait_sends_cancel_wait() {
        use std::io::Read;
        use std::os::unix::net::UnixListener;

        use vivid_protocol::wire::{HEADER_SIZE, PREFACE_SIZE, RecordHeader};

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("cancel-wait.sock");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skipping wait cancellation socket test: {error}");
                return;
            }
            Err(error) => panic!("fake presenter bind failed: {error}"),
        };
        let server = thread::spawn(move || -> io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut preface = [0; PREFACE_SIZE];
            stream.read_exact(&mut preface)?;
            let mut records = Vec::new();
            for _ in 0..4 {
                let mut header = [0; HEADER_SIZE];
                stream.read_exact(&mut header)?;
                let header = RecordHeader::decode(header);
                let mut body = vec![0; header.body_length as usize];
                stream.read_exact(&mut body)?;
                records.push((header, body));
            }
            assert_eq!(records[0].0.record_type, messages::WAIT_SOURCE);
            assert_eq!(records[1].0.record_type, messages::CANCEL_WAIT);
            let (cancel, wait_request_id) = messages::parse_cancel_wait(&records[1].1)?;
            assert_eq!(wait_request_id, 42);
            assert_ne!(cancel.request_id, 0);
            assert_eq!(records[2].0.record_type, messages::WAIT_SOURCE);
            assert_eq!(records[3].0.record_type, messages::CANCEL_WAIT);
            let (_, wait_request_id) = messages::parse_cancel_wait(&records[3].1)?;
            assert_eq!(wait_request_id, 43);
            Ok(())
        });

        let endpoint = Endpoint::parse(socket.to_str().unwrap()).unwrap();
        let connection = Connection::open(&endpoint, ConnectionKind::Control).unwrap();
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
                settled: true,
            },
            SceneRevision::ZERO,
            1,
            false,
            false,
            Arc::new(HotPathCounterState::default()),
            Arc::new(Mutex::new(TraceState::default())),
        )
        .unwrap();
        let wait = WaitSource {
            source_id: 7,
            condition: messages::WAIT_PLAYBACK_STARTED,
            value: None,
            timeout_us: 1_000_000,
        };
        dispatcher
            .write_record(
                messages::WAIT_SOURCE,
                0,
                wait.source_id,
                &messages::wait_source(42, wait).unwrap(),
            )
            .unwrap();
        drop(SourceWaitHandle {
            dispatcher: Some(WaitDispatcher {
                writer: dispatcher.writer.clone(),
                shared: dispatcher.shared.clone(),
            }),
            request_id: 42,
            source_id: 7,
            synthetic: None,
            completed: false,
            active: Arc::new(AtomicBool::new(true)),
        });
        dispatcher
            .write_record(
                messages::WAIT_SOURCE,
                0,
                wait.source_id,
                &messages::wait_source(43, wait).unwrap(),
            )
            .unwrap();
        let mut cancellable = SourceWaitHandle {
            dispatcher: Some(WaitDispatcher {
                writer: dispatcher.writer.clone(),
                shared: dispatcher.shared.clone(),
            }),
            request_id: 43,
            source_id: 7,
            synthetic: None,
            completed: false,
            active: Arc::new(AtomicBool::new(true)),
        };
        let cancellation = cancellable.cancellation();
        let waiter = thread::spawn(move || cancellable.wait());
        cancellation.cancel().unwrap();
        let error = waiter.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn attachment_resolution_distinguishes_unconsumed_and_consumed_tickets() {
        let session = ProducerSession::connect(&producer_config(
            Vec::new(),
            vec![messages::FEATURE_OBSERVABILITY_CORE_V1],
        ))
        .unwrap();
        assert_eq!(
            session
                .resolve_attachment_status(&source_status_with_attachment(
                    messages::ATTACHMENT_NEVER,
                    0,
                ))
                .unwrap(),
            AttachmentResolution::NotConsumed
        );
        assert_eq!(
            session
                .resolve_attachment_status(&source_status_with_attachment(
                    messages::ATTACHMENT_ATTACHED,
                    3,
                ))
                .unwrap(),
            AttachmentResolution::ConsumedAttached { generation: 3 }
        );
        assert_eq!(
            session
                .resolve_attachment_status(&source_status_with_attachment(
                    messages::ATTACHMENT_CLOSED,
                    4,
                ))
                .unwrap(),
            AttachmentResolution::ConsumedClosed { generation: 4 }
        );
    }

    fn source_status_with_attachment(state: u64, generation: u64) -> SourceStatus {
        SourceStatus {
            source_id: 1,
            source_revision: SourceRevision::ZERO,
            kind: messages::SOURCE_KIND_RASTER,
            lifecycle: messages::SOURCE_LIFECYCLE_CREATED,
            epoch: 0,
            attachment_state: state,
            attachment_generation: generation,
            last_media_id: 0,
            last_media_sequence: 0,
            last_decoded_pts_us: 0,
            last_presented_pts_us: 0,
            last_presentation_id: 0,
            visible: true,
            capture_policy: 0,
            linked_source_id: 0,
            milestones: 0,
            outstanding_byte_credit: 0,
            outstanding_packet_credit: 0,
            ingress_queue_depth: 0,
            descriptor: None,
            playback: None,
            terminal_loss_code: None,
        }
    }

    /// A live control connection with routine traffic must produce RTT and diagnostic clock
    /// estimates quickly. Nothing here is idle for the 15-second liveness threshold, so only the
    /// active sampling path can produce either estimate.
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
                let (record_type, reply) = if header.record_type == messages::PING {
                    let ping = messages::parse_clock_ping(&body)?;
                    let timestamps =
                        ping.sender_transmit_us
                            .map(|sent| messages::ClockPongTimestamps {
                                echoed_sender_transmit_us: sent,
                                responder_receive_us: sent.saturating_add(1_000),
                                responder_transmit_us: sent.saturating_add(1_000),
                            });
                    (
                        messages::PONG,
                        messages::clock_pong(ping.request_id, timestamps)?,
                    )
                } else {
                    let request_id = messages::request_id(&body)?;
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
                settled: true,
            },
            SceneRevision::ZERO,
            1,
            false,
            true,
            counters.clone(),
            Arc::new(Mutex::new(TraceState::default())),
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
        let (sampled, clock) = loop {
            let state = dispatcher
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let (Some(rtt_us), Some(clock)) = (state.rtt_us, state.clock_estimate) {
                break (rtt_us, clock);
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
        assert_eq!(clock.accepted_samples, 1);
        assert_eq!(clock.rejected_samples, 0);
        assert_eq!(dispatcher.clock_estimate(), Some(clock));

        let before = dispatcher.adjusted_minimum_buffer(10_000);
        dispatcher.shared.state.lock().unwrap().clock_estimate = Some(ClockEstimate {
            offset_us: i64::MAX,
            delay_us: u64::MAX,
            responder_processing_us: u64::MAX,
            accepted_samples: u64::MAX,
            rejected_samples: u64::MAX,
        });
        assert_eq!(dispatcher.adjusted_minimum_buffer(10_000), before);
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
                    need_full_frame_reason: None,
                    lost: None,
                    credit_returns: 0,
                }),
                changed: Condvar::new(),
            }),
            counters: Arc::new(HotPathCounterState::default()),
            trace: Arc::new(Mutex::new(TraceState::default())),
            last_record_sequence: 0,
            attachment_generation: 0,
            rolling_byte_window: byte_credits,
            rolling_packet_window: packet_credits,
            delta_operation_limit: None,
            media_connection_required: true,
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
    fn source_exposes_presenter_advertised_queue_limits() {
        let source = source(7, 8 * 1024 * 1024, 96);
        assert_eq!(source.rolling_byte_window(), 8 * 1024 * 1024);
        assert_eq!(source.rolling_packet_window(), 96);
        assert_eq!(
            source.media_queue_limits(),
            MediaQueueLimits {
                max_bytes: 8 * 1024 * 1024,
                max_packets: 96
            }
        );
    }

    #[test]
    fn cache_hit_source_cannot_open_a_ticketless_media_channel() {
        let mut cached = source(17, 0, 0);
        cached.media_connection_required = false;
        assert!(cached.is_cache_hit());
        assert!(!cached.media_connection_required());
        let mut session =
            ProducerSession::connect(&producer_config(Vec::new(), Vec::new())).unwrap();
        let error = session
            .open_media_sender(cached, ConnectionKind::Blob)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
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
    fn owned_sender_chooses_smaller_delta_and_falls_back_to_full() {
        let mut raster = source(7, 4096, 4);
        raster.delta_operation_limit = Some(4);
        let mut sender = MediaSender {
            source: raster,
            channel: MediaChannel {
                connection: Connection::sink(ConnectionKind::Raster).unwrap(),
                source_id: 7,
            },
            dry_run: true,
            raster_zstd: false,
        };
        let frame = vec![0_u8; 4 * 4 * 4];
        let pixel = [1, 2, 3, 255];
        assert_eq!(
            sender
                .send_raster_delta_or_full(
                    1,
                    2,
                    1,
                    4,
                    4,
                    &frame,
                    &[media::RasterDeltaOperation::Overwrite {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                        rgba: &pixel,
                    }],
                )
                .unwrap(),
            RasterSendKind::Delta
        );
        assert_eq!(
            sender
                .send_raster_delta_or_full(
                    1,
                    3,
                    2,
                    4,
                    4,
                    &frame,
                    &[media::RasterDeltaOperation::Overwrite {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 4,
                        rgba: &frame,
                    }],
                )
                .unwrap(),
            RasterSendKind::Full
        );
    }

    #[test]
    fn need_full_frame_is_a_source_scoped_recovery_request() {
        let mut source = source(9, 4096, 2);
        apply_source_record(
            &source.state,
            &Record {
                record_type: messages::NEED_FULL_FRAME,
                flags: 0,
                object_id: 9,
                sequence: 2,
                body: messages::need_full_frame(9, messages::NEED_FULL_FRAME_BASE_UNAVAILABLE)
                    .unwrap(),
            },
        )
        .unwrap();
        assert_eq!(
            source.take_full_frame_request(),
            Some(messages::NEED_FULL_FRAME_BASE_UNAVAILABLE)
        );
        assert_eq!(source.take_full_frame_request(), None);
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
    fn opened_sender_tracks_attachment_generation_and_barrier_sequence() {
        let mut session =
            ProducerSession::connect(&producer_config(Vec::new(), Vec::new())).unwrap();
        let mut sender = session
            .open_media_sender(source(18, 4096, 1), ConnectionKind::Video)
            .unwrap();
        assert_eq!(sender.source().attachment_generation(), 1);
        sender
            .send_video(VideoPacket {
                epoch: 1,
                packet_id: 1,
                pts_us: 0,
                dts_us: 0,
                duration_us: 16_667,
                key: true,
                data: &[0, 0, 1, 0x65],
            })
            .unwrap();
        assert_eq!(
            sender.source().last_record_sequence(),
            2,
            "ATTACH_CHANNEL is sequence 1 and the final media record is sequence 2"
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
    fn source_descriptor_creation_and_updates_require_negotiation_and_valid_bounds() {
        let descriptor = SourceDescriptor {
            role: messages::SOURCE_ROLE_DOCUMENT,
            title: "guide.pdf".into(),
            content_revision: 1,
            semantic_availability: messages::SEMANTIC_AVAILABLE_TEXT,
            locator: "vvrd+unix:///owner-only/control.sock".into(),
        };
        let mut session = ProducerSession::connect(&producer_config(
            Vec::new(),
            vec![messages::FEATURE_SOURCE_DESCRIPTOR_V1],
        ))
        .unwrap();
        session
            .create_raster_source_with_descriptor(1, 1, 1, &descriptor)
            .unwrap();
        session
            .update_source_descriptor(
                1,
                &SourceDescriptor {
                    content_revision: 2,
                    ..descriptor.clone()
                },
            )
            .unwrap();

        let mut unsupported =
            ProducerSession::connect(&producer_config(Vec::new(), Vec::new())).unwrap();
        assert_eq!(
            unsupported
                .create_raster_source_with_descriptor(1, 1, 1, &descriptor)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let oversized = SourceDescriptor {
            title: "x".repeat(messages::MAX_SOURCE_DESCRIPTOR_TITLE_BYTES + 1),
            ..descriptor
        };
        assert_eq!(
            session
                .update_source_descriptor(1, &oversized)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
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
