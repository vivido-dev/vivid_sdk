//! Full-duplex producer SDK for Vivid Protocol 1.5.
//!
//! The public object model deliberately follows the 1.5 wire model:
//!
//! - [`Surface`] is stable semantic, scene, policy, and input identity.
//! - [`Track`] is one immutable media configuration owned by a surface.
//! - [`TrackChannel`] is one authenticated transport generation for a track.
//!
//! There is no source, media-ticket, incremental-credit, or feature-ID compatibility layer.

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::env;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use vivid_protocol::anchor::{self, AnchorKey};
use vivid_protocol::auth::{self, Secret32};
use vivid_protocol::cbor::Value;
use vivid_protocol::media::{self, AudioPacket, VideoPacket};
use vivid_protocol::messages::{
    self, ChannelOpen, Envelope, Hello, HelloAuthentication, LaneClass, LaneOpen, PayloadMap,
    TrackKind,
};
use vivid_protocol::resource::{
    ChannelFlow, Resource, ResourceContract, ResourceError, TokenBucket,
};
use vivid_protocol::revision::{
    ChannelGeneration, GrantGeneration, InputEpoch, SceneRevision, SurfaceGeneration,
    SurfaceRevision, TargetGeneration, TrackRevision,
};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration, TrackMode};
use vivid_protocol::wire::{
    Connection, ConnectionKind, ConnectionReader, ConnectionWriter, Endpoint, Record,
};

pub use vivid_protocol::context::{
    ContextDefinition, OP_DELEGATE, OP_DESKTOP_INPUT, OP_KNOWN_MASK, OP_OBSERVE, OP_SCENE,
    OP_SURFACE_TRACK_MEDIA, OP_TERMINAL_ANCHOR,
};
pub use vivid_protocol::input::{
    INPUT_CLASS_KEYBOARD, INPUT_CLASS_KNOWN_MASK, INPUT_CLASS_POINTER_AXIS,
    INPUT_CLASS_POINTER_BUTTON, INPUT_CLASS_POINTER_MOTION, InputBinding, InputEvent, InputGate,
    InputTuple,
};
pub use vivid_protocol::lease::{CleanupPolicy, SessionLeaseDefinition};
pub use vivid_protocol::media::RasterDeltaOperation;
pub use vivid_protocol::messages::ErrorDetail;
pub use vivid_protocol::registry::{
    CANVAS_CONTENT, CANVAS_SURFACE, CORE_CONTROL, DESKTOP_CONTENT, DESKTOP_INPUT, DESKTOP_SURFACE,
    GENERIC_CONTENT, LIVE_MEDIA, OBSERVABILITY, TERMINAL_CONTENT, TERMINAL_SURFACE, TIMED_MEDIA,
};
pub use vivid_protocol::scene::{Fit, SceneNode};
pub use vivid_protocol::surface::{
    CoordinateModel, POLICY_DENY_CAPTURE, POLICY_DENY_DESCRIPTOR_EXPORT, POLICY_DENY_IMAGE_CACHE,
    POLICY_DENY_POSTER_RETENTION, POLICY_REDUCED_DIAGNOSTICS, SurfaceDefinition, SurfaceDescriptor,
    SurfaceRole,
};
pub use vivid_protocol::track::{
    AudioConfiguration, ImageConfiguration, MILESTONE_BUFFERED_ENDED, MILESTONE_CHANNEL_ACCEPTED,
    MILESTONE_CHANNEL_DETACHED, MILESTONE_CLOCK_STARTED, MILESTONE_DECODER_INITIALIZED,
    MILESTONE_EOS_ACCEPTED, MILESTONE_FIRST_MEDIA, MILESTONE_KNOWN_MASK, MILESTONE_OUTPUT_READY,
    MILESTONE_PRESENTED, MILESTONE_RANDOM_ACCESS, MILESTONE_TRACK_LOST, RasterConfiguration,
    VideoConfiguration,
};

const MAX_CONTROL_EVENTS: usize = 1024;
const MAX_INPUT_EVENTS: usize = 1024;
const MAX_CHANNEL_EVENTS: usize = 256;
const DEFAULT_CONTROL_BODY: u32 = vivid_protocol::CONTROL_MAX_RECORD_BODY;
const OFFLINE_SESSION_ID: u64 = 1;
const OFFLINE_CONTEXT_ID: u64 = 1;
const OFFLINE_FLOW_BYTES: u64 = 1 << 60;
const OFFLINE_FLOW_RECORDS: u64 = 1 << 40;
const CARRIER_BINDING_NONE: [u8; 32] = [0; 32];

type TrackRegistry = HashMap<(u64, u64, u64), Arc<Mutex<TrackLocal>>>;

/// Metadata carried in the common Vivid 1.5 request envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMetadata {
    pub preconditions: PayloadMap,
    pub idempotency_key: Option<[u8; messages::IDEMPOTENCY_KEY_BYTES]>,
    pub causation_id: Option<[u8; messages::CAUSATION_ID_BYTES]>,
}

impl RequestMetadata {
    pub fn validate(&self) -> io::Result<()> {
        if self
            .preconditions
            .windows(2)
            .any(|pair| pair[0].0 >= pair[1].0)
            || self.preconditions.iter().any(|(key, _)| *key > 9)
        {
            return Err(invalid_input(
                "preconditions must be sorted, unique, and use keys 0 through 9",
            ));
        }
        Ok(())
    }

    fn apply(&self, envelope: &mut Envelope) -> io::Result<()> {
        self.validate()?;
        envelope.preconditions.clone_from(&self.preconditions);
        envelope.idempotency_key = self.idempotency_key;
        envelope.causation_id = self.causation_id;
        Ok(())
    }
}

/// A typed presenter rejection. Diagnostic text is display-only and never drives protocol state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterError {
    pub code: u64,
    pub request_id: u64,
    pub detail: ErrorDetail,
    pub fatal: bool,
    pub diagnostic: String,
}

impl std::fmt::Display for PresenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Vivid presenter rejected request {} with error {}: {}",
            self.request_id, self.code, self.diagnostic
        )
    }
}

impl std::error::Error for PresenterError {}

impl From<messages::ErrorReply> for PresenterError {
    fn from(value: messages::ErrorReply) -> Self {
        Self {
            code: value.code,
            request_id: value.request_id,
            detail: value.detail,
            fatal: value.fatal,
            diagnostic: value.diagnostic,
        }
    }
}

/// Authentication material for a new or resumed 1.5 session.
///
/// This type intentionally implements neither `Debug` nor `Display`.
pub enum ProducerAuthentication {
    /// Read `VIVID_ROOT_SECRET` at connect time.
    RootFromEnvironment,
    Root {
        root_secret: Secret32,
    },
    LeaseActivation {
        context_id: u64,
        lease_id: u64,
        activation_secret: Secret32,
        attempt_id: [u8; auth::ATTEMPT_ID_BYTES],
        proof_of_possession: Option<Vec<u8>>,
    },
    Resume {
        context_id: u64,
        lease_id: u64,
        session_id: u64,
        resume_generation: u64,
        attempt_id: [u8; auth::ATTEMPT_ID_BYTES],
        prior_resume_key: Secret32,
    },
}

impl ProducerAuthentication {
    pub fn root_hex(value: &str) -> io::Result<Self> {
        Ok(Self::Root {
            root_secret: Secret32::from_hex(value)
                .map_err(|error| invalid_input(error.to_string()))?,
        })
    }

    pub fn lease_activation_hex(context_id: u64, lease_id: u64, value: &str) -> io::Result<Self> {
        let mut attempt_id = [0; auth::ATTEMPT_ID_BYTES];
        random_bytes(&mut attempt_id)?;
        Ok(Self::LeaseActivation {
            context_id,
            lease_id,
            activation_secret: Secret32::from_hex(value)
                .map_err(|error| invalid_input(error.to_string()))?,
            attempt_id,
            proof_of_possession: None,
        })
    }
}

/// Connection and negotiation policy. Secret-bearing fields are deliberately non-debuggable.
pub struct ProducerConfig {
    pub endpoint_control: Option<String>,
    pub endpoint_interactive: Option<String>,
    pub endpoint_realtime: Option<String>,
    pub endpoint_bulk: Option<String>,
    pub authentication: ProducerAuthentication,
    pub producer_name: String,
    pub producer_version: String,
    pub target_profile: String,
    pub required_profiles: Vec<String>,
    pub optional_profiles: Vec<String>,
    pub maximum_control_body: u32,
    pub dry_run: bool,
    pub trace_dir: Option<PathBuf>,
}

impl Default for ProducerConfig {
    fn default() -> Self {
        Self {
            endpoint_control: None,
            endpoint_interactive: None,
            endpoint_realtime: None,
            endpoint_bulk: None,
            authentication: ProducerAuthentication::RootFromEnvironment,
            producer_name: "vivid-sdk".into(),
            producer_version: env!("CARGO_PKG_VERSION").into(),
            target_profile: TERMINAL_SURFACE.into(),
            required_profiles: vec![TERMINAL_SURFACE.into(), CORE_CONTROL.into()],
            optional_profiles: vec![LIVE_MEDIA.into(), OBSERVABILITY.into(), TIMED_MEDIA.into()],
            maximum_control_body: DEFAULT_CONTROL_BODY,
            dry_run: false,
            trace_dir: None,
        }
    }
}

impl ProducerConfig {
    pub fn offline() -> Self {
        Self {
            dry_run: true,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.producer_name.len() > 256 || self.producer_version.len() > 128 {
            return Err(invalid_input("producer name or version is too long"));
        }
        if self.maximum_control_body == 0
            || self.maximum_control_body > vivid_protocol::CONTROL_MAX_RECORD_BODY
        {
            return Err(invalid_input("maximum control body must be in 1..=1048576"));
        }
        validate_profiles(&self.required_profiles)?;
        validate_profiles(&self.optional_profiles)?;
        if self
            .required_profiles
            .iter()
            .any(|profile| self.optional_profiles.contains(profile))
        {
            return Err(invalid_input("required and optional profile lists overlap"));
        }
        if !self
            .required_profiles
            .iter()
            .any(|value| value == CORE_CONTROL)
            || !self
                .required_profiles
                .iter()
                .any(|value| value == &self.target_profile)
        {
            return Err(invalid_input(
                "required profiles must contain core and the selected target profile",
            ));
        }
        let offered: BTreeSet<&str> = self
            .required_profiles
            .iter()
            .chain(&self.optional_profiles)
            .map(String::as_str)
            .collect();
        for profile in offered.iter().copied() {
            if let Some(prerequisites) = vivid_protocol::registry::prerequisites(profile) {
                if prerequisites
                    .iter()
                    .any(|required| !offered.contains(required))
                {
                    return Err(invalid_input(format!(
                        "profile {profile:?} is missing a prerequisite"
                    )));
                }
            } else if self.required_profiles.iter().any(|value| value == profile) {
                return Err(invalid_input(format!(
                    "required profile {profile:?} is not registered"
                )));
            }
        }
        Ok(())
    }

    fn is_offline(&self) -> bool {
        self.dry_run || self.trace_dir.is_some()
    }
}

/// Immutable information returned by `WELCOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u64,
    pub session_tag: [u8; messages::SESSION_TAG_BYTES],
    pub root_context_id: u64,
    pub target_generation: TargetGeneration,
    pub target_profile: String,
    pub target_descriptor: PayloadMap,
    pub accepted_profiles: Vec<String>,
    pub session_revision: u64,
    pub scene_revision: SceneRevision,
    pub establishment_state: u64,
    pub resume_generation: u64,
    pub resource_contract: ResourceContract,
}

/// A stable, owner-qualified surface handle.
#[derive(Clone)]
pub struct Surface {
    inner: Arc<Mutex<SurfaceLocal>>,
}

#[derive(Debug, Clone)]
struct SurfaceLocal {
    definition: SurfaceDefinition,
    revision: SurfaceRevision,
    generation: SurfaceGeneration,
    destroyed: bool,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.lock() {
            Ok(state) => formatter
                .debug_struct("Surface")
                .field("context_id", &state.definition.context_id)
                .field("surface_id", &state.definition.surface_id)
                .field("revision", &state.revision)
                .field("generation", &state.generation)
                .field("destroyed", &state.destroyed)
                .finish(),
            Err(_) => formatter.write_str("Surface(<poisoned>)"),
        }
    }
}

impl Surface {
    pub fn context_id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.definition.context_id)
    }

    pub fn id(&self) -> u64 {
        self.inner
            .lock()
            .map_or(0, |state| state.definition.surface_id)
    }

    pub fn revision(&self) -> SurfaceRevision {
        self.inner
            .lock()
            .map_or(SurfaceRevision::ZERO, |state| state.revision)
    }

    pub fn generation(&self) -> SurfaceGeneration {
        self.inner
            .lock()
            .map_or(SurfaceGeneration::ZERO, |state| state.generation)
    }

    pub fn definition(&self) -> io::Result<SurfaceDefinition> {
        Ok(lock(&self.inner, "surface")?.definition.clone())
    }
}

/// An immutable, owner-qualified track handle.
#[derive(Clone)]
pub struct Track {
    inner: Arc<Mutex<TrackLocal>>,
}

#[derive(Clone)]
struct TrackLocal {
    configuration: TrackConfiguration,
    revision: TrackRevision,
    channel_generation: ChannelGeneration,
    open_deadline_us: u64,
    maximum_record_body: u32,
    effective_claims: PayloadMap,
    connection_required: bool,
    delta_operation_limit: u32,
    media_sequence: Arc<Mutex<TrackMediaSequence>>,
    active_flow: Option<Weak<FlowSync>>,
    active_media: Option<Weak<Mutex<ChannelMediaState>>>,
    destroyed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TrackMediaSequence {
    last_id: u64,
    last_epoch: u32,
    last_record_sequence: u64,
}

impl TrackMediaSequence {
    fn accept(&mut self, id: u64, epoch: u32) -> io::Result<()> {
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

    fn reconcile_status(&mut self, status: &TrackStatus, generation_changed: bool) {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSupport {
    pub supported: bool,
    pub selected_decoder: String,
    pub capability_generation: u64,
    pub effective_claims: PayloadMap,
}

/// Authoritative state returned by `QUERY_SURFACE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceStatus {
    pub context_id: u64,
    pub surface_id: u64,
    pub revision: SurfaceRevision,
    pub generation: SurfaceGeneration,
    pub semantic_profile: String,
    pub coordinate_model: CoordinateModel,
    pub logical_width: u64,
    pub logical_height: u64,
    pub scale_numerator: u64,
    pub scale_denominator: u64,
    pub rotation: u16,
    pub descriptor: SurfaceDescriptor,
    pub effective_policy: u64,
    pub active_slots: PayloadMap,
    pub lifecycle: u64,
    pub profile_status: PayloadMap,
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
    fn validate_value(self, value: Option<u64>) -> io::Result<()> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBinding {
    pub slot: u64,
    pub track_id: u64,
    pub expected_channel_generation: ChannelGeneration,
    pub required_milestone: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneCommit {
    pub scene_revision: SceneRevision,
    pub target_generation: TargetGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    TargetChanged(PayloadMap),
    AnchorReady {
        context_id: u64,
        anchor_id: u64,
        payload: PayloadMap,
    },
    AnchorGone {
        context_id: u64,
        anchor_id: u64,
        payload: PayloadMap,
    },
    TrackLost {
        object_id: u64,
        payload: PayloadMap,
    },
    ContextChanged {
        object_id: u64,
        payload: PayloadMap,
    },
    Other {
        record_type: u16,
        object_id: u64,
        payload: PayloadMap,
    },
    /// The control transport became unusable; live session state must be reconciled or resumed.
    ConnectionClosed {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorStatus {
    pub context_id: u64,
    pub anchor_id: u64,
    /// Unknown (`0`), ready (`1`), or gone (`2`).
    pub state: u64,
    pub target_generation: Option<TargetGeneration>,
    /// Complete validated status payload, including optional cell/intersection fields.
    pub payload: PayloadMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelEvent {
    NeedKeyframe(PayloadMap),
    NeedFullFrame(PayloadMap),
    Error(PresenterError),
}

/// Parsed effective state returned by `INPUT_BOUND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputBindingStatus {
    pub producer_epoch: u64,
    pub grant_generation: u64,
    pub context_id: u64,
    pub surface_id: u64,
    pub surface_generation: u64,
    pub effective_classes: u64,
    pub state: u64,
    pub reason: u64,
    pub watchdog_timeout_us: u64,
}

/// A validated watchdog renewal for the currently effective input grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLeaseRenewal {
    pub binding: InputTuple,
    pub renewal_sequence: u64,
    pub watchdog_timeout_us: u64,
}

/// A validated revocation or reset of an effective input grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputGrantTermination {
    pub binding: InputTuple,
    pub reason: u64,
}

/// Actionable traffic from an authenticated interactive lane.
///
/// Ordinary input is kept as the exact strict payload until the caller supplies the authoritative
/// current surface dimensions to [`InputLaneEvent::decode_input`]. The resulting [`InputEvent`]
/// still has to pass an [`InputGate`] immediately before the OS injection API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputLaneEvent {
    Input {
        record_type: u16,
        surface_id: u64,
        payload: PayloadMap,
    },
    Renew(InputLeaseRenewal),
    Revoked(InputGrantTermination),
    Reset(InputGrantTermination),
    /// The lane became unusable. Atomically disable injection and release all held input state.
    LaneClosed {
        diagnostic: String,
    },
    Error(PresenterError),
}

impl InputLaneEvent {
    pub fn decode_input(&self, width: u64, height: u64) -> io::Result<InputEvent> {
        let Self::Input {
            record_type,
            surface_id,
            payload,
        } = self
        else {
            return Err(invalid_input("lane event is not an ordinary input event"));
        };
        let value = Value::Map(payload.clone());
        match *record_type {
            messages::KEY_INPUT => Ok(InputEvent::decode_key(*surface_id, &value)?),
            messages::POINTER_MOTION => Ok(InputEvent::decode_motion(
                *surface_id,
                &value,
                width,
                height,
            )?),
            messages::POINTER_BUTTON => Ok(InputEvent::decode_button(
                *surface_id,
                &value,
                width,
                height,
            )?),
            messages::POINTER_AXIS => {
                Ok(InputEvent::decode_axis(*surface_id, &value, width, height)?)
            }
            _ => Err(invalid_data("unknown ordinary input record type")),
        }
    }
}

struct Endpoints {
    interactive: Endpoint,
    realtime: Endpoint,
    bulk: Endpoint,
}

struct PendingControl {
    requests: Mutex<HashMap<u64, mpsc::Sender<Result<Record, String>>>>,
    events: Mutex<VecDeque<SessionEvent>>,
    closed: AtomicBool,
}

struct PendingInput {
    requests: Mutex<HashMap<u64, mpsc::Sender<Result<Record, String>>>>,
    events: Mutex<VecDeque<InputLaneEvent>>,
    closed: AtomicBool,
}

struct SessionLifecycle {
    closed: AtomicBool,
    diagnostic: Mutex<Option<String>>,
    track_flows: Mutex<Vec<Weak<FlowSync>>>,
    input_lanes: Mutex<Vec<Weak<PendingInput>>>,
}

impl SessionLifecycle {
    fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            diagnostic: Mutex::new(None),
            track_flows: Mutex::new(Vec::new()),
            input_lanes: Mutex::new(Vec::new()),
        }
    }

    fn ensure_active(&self) -> io::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            let diagnostic = self
                .diagnostic
                .lock()
                .ok()
                .and_then(|value| value.clone())
                .unwrap_or_else(|| "Vivid session is closed".into());
            Err(io::Error::new(io::ErrorKind::BrokenPipe, diagnostic))
        } else {
            Ok(())
        }
    }

    fn register_track_flow(&self, flow: &Arc<FlowSync>) -> io::Result<()> {
        self.ensure_active()?;
        let mut flows = lock(&self.track_flows, "session track registry")?;
        flows.retain(|pending| pending.strong_count() != 0);
        self.ensure_active()?;
        flows.push(Arc::downgrade(flow));
        Ok(())
    }

    fn register_input_lane(&self, lane: &Arc<PendingInput>) -> io::Result<()> {
        self.ensure_active()?;
        let mut lanes = lock(&self.input_lanes, "session input registry")?;
        lanes.retain(|pending| pending.strong_count() != 0);
        self.ensure_active()?;
        lanes.push(Arc::downgrade(lane));
        Ok(())
    }

    fn close(&self, message: &str) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut diagnostic) = self.diagnostic.lock() {
            diagnostic.get_or_insert_with(|| message.to_owned());
        }
        if let Ok(mut flows) = self.track_flows.lock() {
            flows.retain(|pending| {
                let Some(flow) = pending.upgrade() else {
                    return false;
                };
                if let Ok(mut state) = flow.state.lock() {
                    state.closed = true;
                    state.diagnostic.get_or_insert_with(|| message.to_owned());
                    flow.changed.notify_all();
                }
                true
            });
        }
        if let Ok(mut lanes) = self.input_lanes.lock() {
            lanes.retain(|pending| {
                let Some(lane) = pending.upgrade() else {
                    return false;
                };
                close_input_lane(&lane, message);
                true
            });
        }
    }
}

enum ControlPlane {
    Live {
        writer: ConnectionWriter,
        pending: Arc<PendingControl>,
    },
    Offline {
        connection: Mutex<Connection>,
    },
}

impl ControlPlane {
    fn request(
        &self,
        request_id: u64,
        record_type: u16,
        object_id: u64,
        body: &[u8],
    ) -> io::Result<Option<Record>> {
        match self {
            Self::Offline { connection } => {
                lock(connection, "offline control connection")?.write_record(
                    record_type,
                    0,
                    object_id,
                    body,
                )?;
                Ok(None)
            }
            Self::Live { writer, pending } => {
                if pending.closed.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Vivid control connection is closed",
                    ));
                }
                let (send, receive) = mpsc::channel();
                {
                    let mut requests = lock(&pending.requests, "pending request table")?;
                    if pending.closed.load(Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Vivid control connection is closed",
                        ));
                    }
                    if requests.insert(request_id, send).is_some() {
                        return Err(invalid_data("duplicate live request ID"));
                    }
                }
                if let Err(error) = writer.write_record(record_type, 0, object_id, body) {
                    let _ = lock(&pending.requests, "pending request table")?.remove(&request_id);
                    return Err(error);
                }
                let result = receive.recv().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Vivid control dispatcher stopped",
                    )
                })?;
                result
                    .map(Some)
                    .map_err(|message| io::Error::new(io::ErrorKind::BrokenPipe, message))
            }
        }
    }

    fn take_event(&self) -> io::Result<Option<SessionEvent>> {
        match self {
            Self::Live { pending, .. } => {
                Ok(lock(&pending.events, "control event queue")?.pop_front())
            }
            Self::Offline { .. } => Ok(None),
        }
    }
}

/// An established Vivid 1.5 logical session.
pub struct Session {
    control: ControlPlane,
    lifecycle: Arc<SessionLifecycle>,
    endpoints: Endpoints,
    channel_key: Secret32,
    resume_key: Option<Secret32>,
    lease_identity: Option<(u64, u64)>,
    anchor_key: AnchorKey,
    info: SessionInfo,
    next_id: AtomicU64,
    next_request_id: AtomicU64,
    surfaces: HashMap<(u64, u64), Arc<Mutex<SurfaceLocal>>>,
    tracks: Arc<Mutex<TrackRegistry>>,
    closed: bool,
    trace_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("session_id", &self.info.session_id)
            .field("root_context_id", &self.info.root_context_id)
            .field("target_profile", &self.info.target_profile)
            .field("accepted_profiles", &self.info.accepted_profiles)
            .field("closed", &self.closed)
            .finish()
    }
}

impl Session {
    pub fn connect(config: ProducerConfig) -> io::Result<Self> {
        config.validate()?;
        if config.is_offline() {
            return Self::connect_offline(config);
        }
        Self::connect_live(config)
    }

    fn connect_live(config: ProducerConfig) -> io::Result<Self> {
        let control_endpoint = endpoint(
            config.endpoint_control.as_deref(),
            vivid_protocol::discovery::ENDPOINT_CONTROL,
        )?;
        let interactive = optional_endpoint(
            config.endpoint_interactive.as_deref(),
            vivid_protocol::discovery::ENDPOINT_INTERACTIVE,
        )?
        .unwrap_or_else(|| control_endpoint.clone());
        let bulk = optional_endpoint(
            config.endpoint_bulk.as_deref(),
            vivid_protocol::discovery::ENDPOINT_BULK,
        )?
        .unwrap_or_else(|| control_endpoint.clone());
        let realtime = optional_endpoint(
            config.endpoint_realtime.as_deref(),
            vivid_protocol::discovery::ENDPOINT_REALTIME,
        )?
        .unwrap_or_else(|| bulk.clone());

        let mut connection = Connection::open(&control_endpoint, ConnectionKind::Control)?;
        let preface = vivid_protocol::wire::encode_preface(
            ConnectionKind::Control,
            vivid_protocol::CONTROL_MAX_RECORD_BODY,
        );
        let (hello, session_secret) = build_hello(&config, &preface)?;
        let hello_body = hello.encode(1)?;
        connection.write_record(messages::HELLO, 0, 0, &hello_body)?;
        let reply = connection.read_record()?;
        if reply.record_type == messages::ERROR {
            return Err(presenter_error(&reply.body)?);
        }
        if reply.record_type != messages::WELCOME || reply.object_id != 0 {
            return Err(invalid_data("expected session-level WELCOME"));
        }
        let (welcome_request, welcome) = messages::Welcome::decode(&reply.body)?;
        if welcome_request != 1 {
            return Err(invalid_data("WELCOME request ID does not match HELLO"));
        }
        if welcome.target_profile != config.target_profile {
            return Err(invalid_data("WELCOME selected a different target profile"));
        }
        let accepted: BTreeSet<&str> = welcome
            .accepted_profiles
            .iter()
            .map(String::as_str)
            .collect();
        let offered: BTreeSet<&str> = hello
            .required_profiles
            .iter()
            .chain(&hello.optional_profiles)
            .map(String::as_str)
            .collect();
        if hello
            .required_profiles
            .iter()
            .any(|profile| !accepted.contains(profile.as_str()))
            || accepted.iter().any(|profile| !offered.contains(profile))
            || accepted.iter().any(|profile| {
                vivid_protocol::registry::prerequisites(profile)
                    .is_some_and(|required| required.iter().any(|value| !accepted.contains(value)))
            })
        {
            return Err(invalid_data(
                "WELCOME profile selection is not offered, complete, and prerequisite-closed",
            ));
        }
        let auth_kind = hello.authentication.kind();
        if welcome.authentication.kind != auth_kind {
            return Err(invalid_data(
                "WELCOME authentication kind does not match HELLO",
            ));
        }
        match &hello.authentication {
            HelloAuthentication::Root { .. }
                if welcome.authentication.lease_state != 0
                    || welcome.establishment_state != 0
                    || welcome.resume_generation != 0 =>
            {
                return Err(invalid_data("root WELCOME contains resumable lease state"));
            }
            HelloAuthentication::LeaseActivation { .. }
                if welcome.authentication.lease_state != 3 || welcome.establishment_state != 0 =>
            {
                return Err(invalid_data(
                    "lease-activation WELCOME is not a new active lease",
                ));
            }
            HelloAuthentication::Resume {
                session_id,
                resume_generation,
                ..
            } if welcome.authentication.lease_state != 3
                || welcome.establishment_state != 1
                || welcome.session_id != *session_id
                || resume_generation.checked_add(1) != Some(welcome.resume_generation) =>
            {
                return Err(invalid_data(
                    "resume WELCOME does not advance the named active lease",
                ));
            }
            _ => {}
        }
        let prk = auth::extract_handshake_prk(
            &session_secret,
            &hello.client_nonce,
            &welcome.server_nonce,
            &CARRIER_BINDING_NONE,
        );
        let unconfirmed = welcome.unconfirmed_payload()?;
        if !auth::verify_welcome_confirmation(
            &prk,
            &unconfirmed,
            &welcome.authentication.confirmation,
        ) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "WELCOME authentication confirmation failed",
            ));
        }
        let (keys, anchor_key) = auth::derive_session_keys(
            &prk,
            welcome.session_id,
            welcome.resume_generation,
            &welcome.session_tag,
        );
        let channel_key = Secret32::new(*keys.channel_key());
        let resume_key =
            (auth_kind != messages::AUTHENTICATION_ROOT).then(|| Secret32::new(*keys.resume_key()));
        let lease_identity = hello_lease_identity(&hello);
        connection.set_send_body_limit(welcome.maximum_control_body)?;
        connection.set_receive_body_limit(config.maximum_control_body)?;
        let info = session_info(&welcome);
        let (reader, writer) = connection.split()?;
        let pending = Arc::new(PendingControl {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        let lifecycle = Arc::new(SessionLifecycle::new());
        let tracks = Arc::new(Mutex::new(HashMap::new()));
        spawn_control_reader(
            reader,
            writer.clone(),
            pending.clone(),
            lifecycle.clone(),
            tracks.clone(),
        )?;
        Ok(Self {
            control: ControlPlane::Live { writer, pending },
            lifecycle,
            endpoints: Endpoints {
                interactive,
                realtime,
                bulk,
            },
            channel_key,
            resume_key,
            lease_identity,
            anchor_key,
            info,
            next_id: AtomicU64::new(1),
            next_request_id: AtomicU64::new(2),
            surfaces: HashMap::new(),
            tracks,
            closed: false,
            trace_dir: None,
        })
    }

    fn connect_offline(config: ProducerConfig) -> io::Result<Self> {
        let lease_identity = producer_lease_identity(&config.authentication);
        let resumable = lease_identity.is_some();
        let mut connection = match &config.trace_dir {
            Some(directory) => {
                Connection::trace(&directory.join("control.vivid"), ConnectionKind::Control)?
            }
            None => Connection::sink(ConnectionKind::Control)?,
        };
        let preface = vivid_protocol::wire::encode_preface(
            ConnectionKind::Control,
            vivid_protocol::CONTROL_MAX_RECORD_BODY,
        );
        let offline_secret = Secret32::new([0x5a; 32]);
        let mut client_nonce = [0; auth::NONCE_BYTES];
        random_bytes(&mut client_nonce)?;
        let mut hello = Hello {
            producer_name: config.producer_name,
            producer_version: config.producer_version,
            required_profiles: config.required_profiles.clone(),
            optional_profiles: config.optional_profiles.clone(),
            maximum_control_body: config.maximum_control_body,
            client_nonce,
            authentication: HelloAuthentication::Root { proof: [0; 32] },
            target_profile: config.target_profile.clone(),
            extensions: vec![],
        };
        hello.authenticate_root(&offline_secret, &preface)?;
        connection.write_record(messages::HELLO, 0, 0, &hello.encode(1)?)?;
        let mut accepted_profiles = config.required_profiles;
        accepted_profiles.extend(config.optional_profiles);
        accepted_profiles.sort();
        accepted_profiles.dedup();
        let server_nonce = [0x33; auth::NONCE_BYTES];
        let prk = auth::extract_handshake_prk(
            &offline_secret,
            &client_nonce,
            &server_nonce,
            &CARRIER_BINDING_NONE,
        );
        let session_tag = [0x44; messages::SESSION_TAG_BYTES];
        let (keys, anchor_key) =
            auth::derive_session_keys(&prk, OFFLINE_SESSION_ID, 0, &session_tag);
        let channel_key = Secret32::new(*keys.channel_key());
        let resume_key = resumable.then(|| Secret32::new(*keys.resume_key()));
        let resource_contract = offline_contract();
        let info = SessionInfo {
            session_id: OFFLINE_SESSION_ID,
            session_tag,
            root_context_id: OFFLINE_CONTEXT_ID,
            target_generation: TargetGeneration::ONE,
            target_profile: config.target_profile,
            target_descriptor: offline_target_descriptor(),
            accepted_profiles,
            session_revision: 1,
            scene_revision: SceneRevision::ZERO,
            establishment_state: 0,
            resume_generation: 0,
            resource_contract,
        };
        Ok(Self {
            control: ControlPlane::Offline {
                connection: Mutex::new(connection),
            },
            lifecycle: Arc::new(SessionLifecycle::new()),
            endpoints: Endpoints {
                interactive: offline_endpoint()?,
                realtime: offline_endpoint()?,
                bulk: offline_endpoint()?,
            },
            channel_key,
            resume_key,
            lease_identity,
            anchor_key,
            info,
            next_id: AtomicU64::new(1),
            next_request_id: AtomicU64::new(2),
            surfaces: HashMap::new(),
            tracks: Arc::new(Mutex::new(HashMap::new())),
            closed: false,
            trace_dir: config.trace_dir,
        })
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// Prepare a fresh, secret-bearing resume authentication request for this leased session.
    ///
    /// Callers should retain this value before releasing the old session object. It implements
    /// neither `Debug` nor `Display`.
    pub fn resume_authentication(&self) -> io::Result<ProducerAuthentication> {
        let (context_id, lease_id) = self.lease_identity.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "root sessions are intentionally non-resumable",
            )
        })?;
        let resume_key = self
            .resume_key
            .as_ref()
            .ok_or_else(|| invalid_data("leased session has no resume key"))?;
        let mut attempt_id = [0; auth::ATTEMPT_ID_BYTES];
        random_bytes(&mut attempt_id)?;
        Ok(ProducerAuthentication::Resume {
            context_id,
            lease_id,
            session_id: self.info.session_id,
            resume_generation: self.info.resume_generation,
            attempt_id,
            prior_resume_key: Secret32::new(*resume_key.expose()),
        })
    }

    pub fn supports(&self, profile: &str) -> bool {
        self.info
            .accepted_profiles
            .binary_search_by(|value| value.as_str().cmp(profile))
            .is_ok()
    }

    pub fn allocate_id(&self) -> io::Result<u64> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid_data("SDK object ID space exhausted"))
    }

    pub fn take_event(&self) -> io::Result<Option<SessionEvent>> {
        self.control.take_event()
    }

    /// Validate and apply one terminal `TARGET_CHANGED` payload to the cached target snapshot.
    pub fn apply_target_changed(&mut self, payload: &PayloadMap) -> io::Result<TargetGeneration> {
        validate_exact_payload_keys("TARGET_CHANGED", payload, 0..=10)?;
        let generation = TargetGeneration::new(required_u64(payload, 9)?);
        generation.require_nonzero()?;
        if generation <= self.info.target_generation {
            return Err(invalid_data(
                "TARGET_CHANGED did not advance the target generation",
            ));
        }
        let descriptor = payload
            .iter()
            .filter(|(key, _)| *key <= 8)
            .cloned()
            .collect();
        validate_terminal_target_descriptor(&descriptor)?;
        self.info.target_generation = generation;
        self.info.target_descriptor = descriptor;
        Ok(generation)
    }

    /// Re-associate a retained surface handle after authenticated session resume.
    pub fn adopt_surface(&mut self, surface: &Surface) -> io::Result<SurfaceStatus> {
        let status = self.query_surface(surface)?;
        if status.lifecycle == 3 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot adopt a surface tombstone",
            ));
        }
        let key = (status.context_id, status.surface_id);
        if let Some(existing) = self.surfaces.get(&key) {
            if Arc::ptr_eq(existing, &surface.inner) {
                return Ok(status);
            }
            return Err(invalid_input(
                "a different retained surface uses the same owner-qualified identity",
            ));
        }
        self.surfaces.insert(key, surface.inner.clone());
        self.advance_allocator_past(status.surface_id)?;
        Ok(status)
    }

    /// Re-associate a retained immutable track handle after its surface has been adopted.
    pub fn adopt_track(&mut self, track: &Track) -> io::Result<TrackStatus> {
        let configuration = track.configuration()?;
        if !self
            .surfaces
            .contains_key(&(configuration.context_id, configuration.surface_id))
        {
            return Err(invalid_input(
                "adopt the track's owner surface before adopting the track",
            ));
        }
        let status = self.query_track(track)?;
        if matches!(status.lifecycle, 6 | 7) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot adopt a lost track or tombstone",
            ));
        }
        let key = (status.context_id, status.surface_id, status.track_id);
        let mut tracks = lock(&self.tracks, "track registry")?;
        if let Some(existing) = tracks.get(&key) {
            if Arc::ptr_eq(existing, &track.inner) {
                return Ok(status);
            }
            return Err(invalid_input(
                "a different retained track uses the same complete identity",
            ));
        }
        tracks.insert(key, track.inner.clone());
        drop(tracks);
        self.advance_allocator_past(status.track_id)?;
        Ok(status)
    }

    /// Open one authenticated interactive-lane generation.
    ///
    /// `desktop-input-v1` must have been accepted. Lane loss never recreates an old grant; callers
    /// reconcile state and use a greater producer input epoch before enabling input again.
    pub fn open_input_lane(&self, lane_generation: u64) -> io::Result<InputLane> {
        self.lifecycle.ensure_active()?;
        if !self.supports(DESKTOP_INPUT) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "desktop-input-v1 was not accepted",
            ));
        }
        if lane_generation == 0 {
            return Err(invalid_input("interactive lane generation must be nonzero"));
        }
        let mut nonce = [0; 16];
        random_bytes(&mut nonce)?;
        let tag = auth::lane_tag(
            self.channel_key.expose(),
            self.info.session_id,
            LaneClass::Interactive as u32,
            lane_generation,
            &nonce,
        );
        let open = LaneOpen {
            session_id: self.info.session_id,
            lane_generation,
            client_nonce: nonce,
            authentication_tag: tag,
        };
        let body = Envelope::correlated(1, open.payload())?.encode()?;
        let offline = matches!(&self.control, ControlPlane::Offline { .. });
        let mut connection = if let Some(directory) = &self.trace_dir {
            Connection::trace(
                &directory.join(format!("interactive-{lane_generation}.vivid")),
                ConnectionKind::Lane,
            )?
        } else if offline {
            Connection::sink(ConnectionKind::Lane)?
        } else {
            Connection::open(&self.endpoints.interactive, ConnectionKind::Lane)?
        };
        connection.write_record(messages::LANE_OPEN, 0, 0, &body)?;
        let maximum_body = if offline {
            64 * 1024
        } else {
            let reply = connection.read_record()?;
            if reply.record_type == messages::ERROR {
                return Err(presenter_error(&reply.body)?);
            }
            expect_record(&reply, messages::LANE_ACCEPTED, 0)?;
            let accepted = messages::decode_control(&reply.body)?;
            if accepted.request_id != 1 {
                return Err(invalid_data(
                    "LANE_ACCEPTED request ID does not match LANE_OPEN",
                ));
            }
            let payload = accepted.payload;
            validate_exact_payload_keys("LANE_ACCEPTED", &payload, 0..=3)?;
            if required_u64(&payload, 0)? != self.info.session_id
                || required_u64(&payload, 1)? != LaneClass::Interactive as u64
                || required_u64(&payload, 2)? != lane_generation
            {
                return Err(invalid_data(
                    "LANE_ACCEPTED contains the wrong session or generation",
                ));
            }
            required_u32(&payload, 3)?
        };
        if maximum_body == 0 || maximum_body > 64 * 1024 {
            return Err(invalid_data(
                "LANE_ACCEPTED maximum body is outside 1..=65536",
            ));
        }
        connection.set_send_body_limit(maximum_body)?;
        if !offline {
            connection.set_receive_body_limit(maximum_body)?;
        }
        let shared = Arc::new(PendingInput {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        self.lifecycle.register_input_lane(&shared)?;
        let writer = if offline {
            connection.writer()
        } else {
            let (reader, writer) = connection.split()?;
            spawn_input_reader(reader, writer.clone(), shared.clone())?;
            writer
        };
        Ok(InputLane {
            writer,
            shared,
            lifecycle: self.lifecycle.clone(),
            lane_generation,
            next_request_id: AtomicU64::new(2),
            offline,
        })
    }

    pub fn create_context(
        &mut self,
        definition: &ContextDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<ContextReady> {
        definition.validate(definition.context_id)?;
        let reply = self.request(
            messages::CREATE_CONTEXT,
            definition.context_id,
            definition.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::CONTEXT_READY, definition.context_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("CONTEXT_READY", &payload, 0..=4)?;
            let ready = ContextReady {
                context_id: required_u64(&payload, 0)?,
                operation_classes: required_u64(&payload, 1)?,
                contract: required_contract(&payload, 2)?,
                lifetime_us: required_u64(&payload, 3)?,
                revision: required_u64(&payload, 4)?,
            };
            if ready.context_id != definition.context_id
                || ready.operation_classes & !definition.operation_classes != 0
                || ready.lifetime_us > definition.lifetime_us
                || ready.revision == 0
            {
                return Err(invalid_data(
                    "CONTEXT_READY contains invalid effective identity or authority",
                ));
            }
            Ok(ready)
        } else {
            Ok(ContextReady {
                context_id: definition.context_id,
                operation_classes: definition.operation_classes,
                contract: definition.requested_contract.clone(),
                lifetime_us: definition.lifetime_us,
                revision: 1,
            })
        }
    }

    pub fn create_session_lease(
        &mut self,
        definition: &SessionLeaseDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<SessionLeaseReady> {
        definition.validate()?;
        let reply = self.request(
            messages::CREATE_SESSION_LEASE,
            definition.lease_id,
            definition.payload()?,
            metadata,
            None,
            None,
        )?;
        if let Some(record) = reply {
            expect_record(&record, messages::SESSION_LEASE_READY, definition.lease_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SESSION_LEASE_READY", &payload, 0..=8)?;
            let ready = SessionLeaseReady {
                context_id: required_u64(&payload, 0)?,
                lease_id: required_u64(&payload, 1)?,
                state: required_u64(&payload, 2)?,
                activation_timeout_us: required_u64(&payload, 3)?,
                disconnect_grace_us: required_u64(&payload, 4)?,
                cleanup_policy: required_u64(&payload, 5)?,
                permitted_profiles: required_text_array(&payload, 6)?,
                contract: required_contract(&payload, 7)?,
                revision: required_u64(&payload, 8)?,
            };
            if ready.context_id != definition.context_id
                || ready.lease_id != definition.lease_id
                || ready.state != 1
                || ready.activation_timeout_us == 0
                || ready.activation_timeout_us > definition.activation_timeout_us
                || ready.disconnect_grace_us > definition.requested_disconnect_grace_us
                || ready.cleanup_policy != definition.cleanup_policy as u64
                || ready.revision == 0
                || validate_profiles(&ready.permitted_profiles).is_err()
                || ready
                    .permitted_profiles
                    .iter()
                    .any(|profile| !definition.permitted_profiles.contains(profile))
            {
                return Err(invalid_data(
                    "SESSION_LEASE_READY contains invalid effective lease state",
                ));
            }
            Ok(ready)
        } else {
            Ok(SessionLeaseReady {
                context_id: definition.context_id,
                lease_id: definition.lease_id,
                state: 1,
                activation_timeout_us: definition.activation_timeout_us,
                disconnect_grace_us: definition.requested_disconnect_grace_us,
                cleanup_policy: definition.cleanup_policy as u64,
                permitted_profiles: definition.permitted_profiles.clone(),
                contract: definition.requested_contract.clone(),
                revision: 1,
            })
        }
    }

    pub fn create_surface(
        &mut self,
        definition: SurfaceDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<Surface> {
        definition.validate()?;
        let key = (definition.context_id, definition.surface_id);
        if self.surfaces.contains_key(&key) {
            return Err(invalid_input("surface identity is already live"));
        }
        let reply = self.request(
            messages::CREATE_SURFACE,
            definition.surface_id,
            definition.create_payload()?,
            metadata,
            None,
            None,
        )?;
        let (revision, generation, effective_policy, parameters) = if let Some(record) = reply {
            expect_record(&record, messages::SURFACE_READY, definition.surface_id)?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SURFACE_READY", &payload, 0..=5)?;
            validate_owner_pair(&payload, definition.context_id, definition.surface_id)?;
            (
                SurfaceRevision::new(required_u64(&payload, 2)?),
                SurfaceGeneration::new(required_u64(&payload, 3)?),
                required_u64(&payload, 4)?,
                required_map(&payload, 5)?.to_vec(),
            )
        } else {
            (
                SurfaceRevision::ONE,
                SurfaceGeneration::ONE,
                definition.policy,
                definition.profile_parameters.clone(),
            )
        };
        revision.require_nonzero()?;
        generation.require_nonzero()?;
        if generation != SurfaceGeneration::ONE {
            return Err(invalid_data("SURFACE_READY initial generation is not one"));
        }
        let mut effective_definition = definition;
        effective_definition.policy = effective_policy;
        effective_definition.profile_parameters = parameters;
        let inner = Arc::new(Mutex::new(SurfaceLocal {
            definition: effective_definition,
            revision,
            generation,
            destroyed: false,
        }));
        self.surfaces.insert(key, inner.clone());
        Ok(Surface { inner })
    }

    pub fn update_surface(
        &mut self,
        surface: &Surface,
        replacement: SurfaceDefinition,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        replacement.validate()?;
        let current = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&current)?;
        if replacement.context_id != current.definition.context_id
            || replacement.surface_id != current.definition.surface_id
            || replacement.semantic_profile != current.definition.semantic_profile
            || replacement.coordinate_model != current.definition.coordinate_model
        {
            return Err(invalid_input(
                "surface update changes immutable identity or semantic profile",
            ));
        }
        let payload = vec![
            (0, Value::Unsigned(replacement.context_id)),
            (1, Value::Unsigned(replacement.surface_id)),
            (2, Value::Unsigned(current.revision.get())),
            (3, Value::Unsigned(current.generation.get())),
            (4, Value::Unsigned(replacement.logical_width)),
            (5, Value::Unsigned(replacement.logical_height)),
            (6, Value::Unsigned(replacement.scale_numerator)),
            (7, Value::Unsigned(replacement.scale_denominator)),
            (8, Value::Unsigned(u64::from(replacement.rotation))),
            (9, replacement.descriptor.to_value()?),
            (10, Value::Unsigned(replacement.policy)),
            (11, Value::Map(replacement.profile_parameters.clone())),
        ];
        self.request_ok(
            messages::UPDATE_SURFACE,
            replacement.surface_id,
            payload,
            metadata,
        )?;
        let mapping_changed = current.definition.logical_width != replacement.logical_width
            || current.definition.logical_height != replacement.logical_height
            || current.definition.scale_numerator != replacement.scale_numerator
            || current.definition.scale_denominator != replacement.scale_denominator
            || current.definition.rotation != replacement.rotation
            || current.definition.profile_parameters != replacement.profile_parameters;
        let mut state = lock(&surface.inner, "surface")?;
        state.revision = state.revision.advance()?;
        if mapping_changed {
            state.generation = state.generation.advance()?;
        }
        let mut replacement = replacement;
        replacement.policy |= state.definition.policy;
        state.definition = replacement;
        Ok(())
    }

    pub fn destroy_surface(
        &mut self,
        surface: &Surface,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let snapshot = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&snapshot)?;
        self.request_ok(
            messages::DESTROY_SURFACE,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
            ],
            metadata,
        )?;
        let context_id = snapshot.definition.context_id;
        let surface_id = snapshot.definition.surface_id;
        lock(&surface.inner, "surface")?.destroyed = true;
        self.surfaces.remove(&(context_id, surface_id));
        lock(&self.tracks, "track registry")?.retain(|(context, owner, _), state| {
            let keep = *context != context_id || *owner != surface_id;
            if !keep {
                if let Ok(mut state) = state.lock() {
                    state.destroyed = true;
                    let active_flow = state.active_flow.take();
                    state.active_media = None;
                    close_track_flow(active_flow.as_ref(), "owning surface destroyed");
                }
            }
            keep
        });
        Ok(())
    }

    pub fn query_surface(&self, surface: &Surface) -> io::Result<SurfaceStatus> {
        let snapshot = lock(&surface.inner, "surface")?.clone();
        let reply = self.request(
            messages::QUERY_SURFACE,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
            ],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let status = if let Some(record) = reply {
            expect_record(
                &record,
                messages::SURFACE_STATUS,
                snapshot.definition.surface_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SURFACE_STATUS", &payload, 0..=15)?;
            validate_owner_pair(
                &payload,
                snapshot.definition.context_id,
                snapshot.definition.surface_id,
            )?;
            let rotation = u16::try_from(required_u64(&payload, 10)?)
                .map_err(|_| invalid_data("SURFACE_STATUS rotation exceeds u16"))?;
            let lifecycle = required_u64(&payload, 14)?;
            if !(1..=3).contains(&lifecycle) {
                return Err(invalid_data("SURFACE_STATUS has an unknown lifecycle"));
            }
            SurfaceStatus {
                context_id: required_u64(&payload, 0)?,
                surface_id: required_u64(&payload, 1)?,
                revision: SurfaceRevision::new(required_u64(&payload, 2)?),
                generation: SurfaceGeneration::new(required_u64(&payload, 3)?),
                semantic_profile: required_text(&payload, 4)?.to_owned(),
                coordinate_model: CoordinateModel::try_from(required_u64(&payload, 5)?)
                    .map_err(io::Error::other)?,
                logical_width: required_u64(&payload, 6)?,
                logical_height: required_u64(&payload, 7)?,
                scale_numerator: required_u64(&payload, 8)?,
                scale_denominator: required_u64(&payload, 9)?,
                rotation,
                descriptor: SurfaceDescriptor::from_value(required_value(&payload, 11)?)
                    .map_err(io::Error::other)?,
                effective_policy: required_u64(&payload, 12)?,
                active_slots: required_map(&payload, 13)?.to_vec(),
                lifecycle,
                profile_status: required_map(&payload, 15)?.to_vec(),
            }
        } else {
            SurfaceStatus {
                context_id: snapshot.definition.context_id,
                surface_id: snapshot.definition.surface_id,
                revision: snapshot.revision,
                generation: snapshot.generation,
                semantic_profile: snapshot.definition.semantic_profile.clone(),
                coordinate_model: snapshot.definition.coordinate_model,
                logical_width: snapshot.definition.logical_width,
                logical_height: snapshot.definition.logical_height,
                scale_numerator: snapshot.definition.scale_numerator,
                scale_denominator: snapshot.definition.scale_denominator,
                rotation: snapshot.definition.rotation,
                descriptor: snapshot.definition.descriptor.clone(),
                effective_policy: snapshot.definition.policy,
                active_slots: vec![],
                lifecycle: if snapshot.destroyed { 3 } else { 1 },
                profile_status: snapshot.definition.profile_parameters.clone(),
            }
        };
        status.revision.require_nonzero()?;
        status.generation.require_nonzero()?;
        if status.logical_width == 0
            || status.logical_height == 0
            || status.scale_numerator == 0
            || status.scale_denominator == 0
            || !matches!(status.rotation, 0 | 90 | 180 | 270)
            || status.semantic_profile != snapshot.definition.semantic_profile
            || status.coordinate_model != snapshot.definition.coordinate_model
        {
            return Err(invalid_data(
                "SURFACE_STATUS contains invalid or changed immutable configuration",
            ));
        }
        let mut state = lock(&surface.inner, "surface")?;
        state.revision = status.revision;
        state.generation = status.generation;
        state.definition.logical_width = status.logical_width;
        state.definition.logical_height = status.logical_height;
        state.definition.scale_numerator = status.scale_numerator;
        state.definition.scale_denominator = status.scale_denominator;
        state.definition.rotation = status.rotation;
        state.definition.descriptor = status.descriptor.clone();
        state.definition.policy = status.effective_policy;
        state.destroyed = status.lifecycle == 3;
        Ok(status)
    }

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
        if timeout_us == 0 {
            return Err(invalid_input("track wait timeout must be nonzero"));
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
            LaneClass::Realtime => &self.endpoints.realtime,
            LaneClass::Bulk => &self.endpoints.bulk,
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
        } else {
            Connection::open(endpoint, ConnectionKind::Track)?
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

    pub fn activate_tracks(
        &mut self,
        surface: &Surface,
        bindings: &[SlotBinding],
        metadata: &RequestMetadata,
    ) -> io::Result<u64> {
        if bindings.is_empty() {
            return Err(invalid_input("ACTIVATE_TRACK requires a slot binding"));
        }
        let snapshot = lock(&surface.inner, "surface")?.clone();
        ensure_live_surface(&snapshot)?;
        let mut seen_slots = BTreeSet::new();
        for binding in bindings {
            if !(1..=4).contains(&binding.slot) || !seen_slots.insert(binding.slot) {
                return Err(invalid_input(
                    "ACTIVATE_TRACK slots must be known and unique",
                ));
            }
            if binding.track_id == 0 {
                if binding.expected_channel_generation != ChannelGeneration::ZERO
                    || binding.required_milestone != 0
                {
                    return Err(invalid_input(
                        "a cleared slot requires zero generation and milestone",
                    ));
                }
                continue;
            }
            if binding.expected_channel_generation == ChannelGeneration::ZERO
                || binding.required_milestone.count_ones() != 1
                || binding.required_milestone & !MILESTONE_KNOWN_MASK != 0
            {
                return Err(invalid_input(
                    "active slot binding has an invalid generation or milestone",
                ));
            }
            let tracks = lock(&self.tracks, "track registry")?;
            let track = tracks
                .get(&(
                    snapshot.definition.context_id,
                    snapshot.definition.surface_id,
                    binding.track_id,
                ))
                .ok_or_else(|| {
                    invalid_input("ACTIVATE_TRACK references a track outside this surface")
                })?;
            let track = lock(track, "track")?;
            ensure_live_track(&track)?;
            if track.configuration.slot != binding.slot
                || track.channel_generation != binding.expected_channel_generation
            {
                return Err(invalid_input(
                    "ACTIVATE_TRACK binding does not match current track state",
                ));
            }
        }
        let payload_bindings = bindings
            .iter()
            .map(|binding| {
                Value::Map(vec![
                    (0, Value::Unsigned(binding.slot)),
                    (1, Value::Unsigned(binding.track_id)),
                    (
                        2,
                        Value::Unsigned(binding.expected_channel_generation.get()),
                    ),
                    (3, Value::Unsigned(binding.required_milestone)),
                ])
            })
            .collect();
        let reply = self.request(
            messages::ACTIVATE_TRACK,
            snapshot.definition.surface_id,
            vec![
                (0, Value::Unsigned(snapshot.definition.context_id)),
                (1, Value::Unsigned(snapshot.definition.surface_id)),
                (2, Value::Array(payload_bindings)),
                (3, Value::Unsigned(snapshot.revision.get())),
            ],
            metadata,
            None,
            None,
        )?;
        let (new_revision, presentation_id) = if let Some(record) = reply {
            expect_record(
                &record,
                messages::TRACK_ACTIVATED,
                snapshot.definition.surface_id,
            )?;
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("TRACK_ACTIVATED", &payload, 0..=4)?;
            validate_owner_pair(
                &payload,
                snapshot.definition.context_id,
                snapshot.definition.surface_id,
            )?;
            (
                SurfaceRevision::new(required_u64(&payload, 3)?),
                required_u64(&payload, 4)?,
            )
        } else {
            (snapshot.revision.advance()?, 1)
        };
        new_revision.require_nonzero()?;
        if new_revision <= snapshot.revision || presentation_id == 0 {
            return Err(invalid_data(
                "TRACK_ACTIVATED did not advance revision and presentation identity",
            ));
        }
        lock(&surface.inner, "surface")?.revision = new_revision;
        Ok(presentation_id)
    }

    pub fn create_node(
        &mut self,
        node: &SceneNode,
        metadata: &RequestMetadata,
    ) -> io::Result<SceneCommit> {
        node.validate()?;
        let transaction_id = self.allocate_id()?;
        let begin_request = self.next_request()?;
        let mut begin = Envelope::correlated(
            begin_request,
            vec![
                (0, Value::Unsigned(node.owning_context_id)),
                (1, Value::Unsigned(transaction_id)),
            ],
        )?;
        begin.transaction_id = Some(transaction_id);
        metadata.apply(&mut begin)?;
        self.dispatch_ok(
            begin_request,
            messages::BEGIN_TXN,
            transaction_id,
            &begin.encode()?,
        )?;

        let mutation_request = self.next_request()?;
        let mut mutation = Envelope::correlated(mutation_request, node.payload()?)?;
        mutation.transaction_id = Some(transaction_id);
        metadata.apply(&mut mutation)?;
        if let Err(error) = self.dispatch_ok(
            mutation_request,
            messages::CREATE_NODE,
            node.node_id,
            &mutation.encode()?,
        ) {
            let _ = self.abort_transaction(transaction_id);
            return Err(error);
        }

        let commit_request = self.next_request()?;
        let mut commit = Envelope::correlated(commit_request, vec![(0, Value::Unsigned(0))])?;
        commit.transaction_id = Some(transaction_id);
        commit.expected_target_generation = Some(self.info.target_generation.get());
        commit.preconditions = vec![(0, Value::Unsigned(self.info.scene_revision.get()))];
        commit.idempotency_key = metadata.idempotency_key;
        commit.causation_id = metadata.causation_id;
        let reply = match self.control.request(
            commit_request,
            messages::COMMIT_TXN,
            transaction_id,
            &commit.encode()?,
        ) {
            Ok(reply) => reply,
            Err(error) => {
                let _ = self.abort_transaction(transaction_id);
                return Err(error);
            }
        };
        let result = if let Some(record) = reply {
            if record.record_type == messages::ERROR {
                let _ = self.abort_transaction(transaction_id);
                return Err(presenter_error(&record.body)?);
            }
            if let Err(error) = expect_record(&record, messages::SCENE_PRESENTED, transaction_id) {
                let _ = self.abort_transaction(transaction_id);
                return Err(error);
            }
            let payload = decoded_payload(&record)?;
            validate_exact_payload_keys("SCENE_PRESENTED", &payload, 0..=1)?;
            SceneCommit {
                scene_revision: SceneRevision::new(required_u64(&payload, 0)?),
                target_generation: TargetGeneration::new(required_u64(&payload, 1)?),
            }
        } else {
            SceneCommit {
                scene_revision: self.info.scene_revision.advance()?,
                target_generation: self.info.target_generation,
            }
        };
        self.info.scene_revision = result.scene_revision;
        Ok(result)
    }

    /// Create a terminal grid node using signed 32.32 cell coordinates.
    #[allow(clippy::too_many_arguments)]
    pub fn place_terminal_surface(
        &mut self,
        surface: &Surface,
        node_id: u64,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        text_layer: u64,
    ) -> io::Result<SceneCommit> {
        if width <= 0 || height <= 0 || text_layer > 2 {
            return Err(invalid_input("invalid terminal node geometry"));
        }
        let node = SceneNode {
            owning_context_id: surface.context_id(),
            node_id,
            surface_context_id: surface.context_id(),
            surface_id: surface.id(),
            geometry: vec![
                (0, Value::Unsigned(1)),
                (1, signed(x)),
                (2, signed(y)),
                (3, signed(width)),
                (4, signed(height)),
                (5, Value::Unsigned(text_layer)),
            ],
            fit: Fit::Contain,
            linear_sampling: true,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        };
        self.create_node(&node, &RequestMetadata::default())
    }

    pub fn anchor_marker(&self, context_id: u64, anchor_id: u64) -> io::Result<String> {
        anchor::encode_marker(
            &self.anchor_key,
            &self.info.session_tag,
            context_id,
            anchor_id,
        )
        .map_err(|message| invalid_input(message.to_owned()))
    }

    pub fn conpty_anchor_marker(&self, context_id: u64, anchor_id: u64) -> io::Result<String> {
        anchor::encode_conpty_marker(
            &self.anchor_key,
            &self.info.session_tag,
            context_id,
            anchor_id,
        )
        .map_err(|message| invalid_input(message.to_owned()))
    }

    pub fn query_anchor(&self, context_id: u64, anchor_id: u64) -> io::Result<AnchorStatus> {
        if context_id == 0 || anchor_id == 0 {
            return Err(invalid_input("anchor identity must be nonzero"));
        }
        let reply = self.request(
            messages::QUERY_ANCHOR,
            anchor_id,
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(anchor_id)),
            ],
            &RequestMetadata::default(),
            None,
            None,
        )?;
        let payload = if let Some(record) = reply {
            expect_record(&record, messages::ANCHOR_STATUS, anchor_id)?;
            let payload = decoded_payload(&record)?;
            validate_payload_keys("ANCHOR_STATUS", &payload, 0..=2, &[3, 4, 5, 6])?;
            payload
        } else {
            vec![
                (0, Value::Unsigned(context_id)),
                (1, Value::Unsigned(anchor_id)),
                (2, Value::Unsigned(0)),
                (6, Value::Unsigned(self.info.target_generation.get())),
            ]
        };
        if required_u64(&payload, 0)? != context_id || required_u64(&payload, 1)? != anchor_id {
            return Err(invalid_data(
                "ANCHOR_STATUS changed complete anchor identity",
            ));
        }
        let state = required_u64(&payload, 2)?;
        if state > 2 {
            return Err(invalid_data("ANCHOR_STATUS has an unknown lifecycle state"));
        }
        let target_generation = optional_u64(&payload, 6)?
            .map(TargetGeneration::new)
            .filter(|generation| *generation != TargetGeneration::ZERO);
        Ok(AnchorStatus {
            context_id,
            anchor_id,
            state,
            target_generation,
            payload,
        })
    }

    pub fn emit_anchor<W: io::Write>(
        &self,
        output: &mut W,
        context_id: u64,
        anchor_id: u64,
    ) -> io::Result<()> {
        output.write_all(self.anchor_marker(context_id, anchor_id)?.as_bytes())?;
        output.flush()
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

    fn track_control(
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

    fn abort_transaction(&self, transaction_id: u64) -> io::Result<()> {
        let request_id = self.next_request()?;
        let mut envelope = Envelope::correlated(request_id, vec![])?;
        envelope.transaction_id = Some(transaction_id);
        self.dispatch_ok(
            request_id,
            messages::ABORT_TXN,
            transaction_id,
            &envelope.encode()?,
        )
    }

    pub fn close(mut self) -> io::Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.lifecycle.close("Vivid session closed");
        let result = self.request_ok(messages::GOODBYE, 0, vec![], &RequestMetadata::default());
        for state in self.surfaces.values() {
            if let Ok(mut state) = state.lock() {
                state.destroyed = true;
            }
        }
        let mut tracks = lock(&self.tracks, "track registry")?;
        for state in tracks.values() {
            if let Ok(mut state) = state.lock() {
                state.destroyed = true;
            }
        }
        self.surfaces.clear();
        tracks.clear();
        result
    }

    fn request_ok(
        &self,
        record_type: u16,
        object_id: u64,
        payload: PayloadMap,
        metadata: &RequestMetadata,
    ) -> io::Result<()> {
        let request_id = self.next_request()?;
        let mut envelope = Envelope::correlated(request_id, payload)?;
        metadata.apply(&mut envelope)?;
        self.dispatch_ok(request_id, record_type, object_id, &envelope.encode()?)
    }

    fn dispatch_ok(
        &self,
        request_id: u64,
        record_type: u16,
        object_id: u64,
        body: &[u8],
    ) -> io::Result<()> {
        if let Some(record) = self
            .control
            .request(request_id, record_type, object_id, body)?
        {
            if record.record_type == messages::ERROR {
                return Err(presenter_error(&record.body)?);
            }
            expect_record(&record, messages::OK, object_id)?;
            let envelope = messages::decode_control(&record.body)?;
            if envelope.request_id != request_id || !envelope.payload.is_empty() {
                return Err(invalid_data("malformed OK reply"));
            }
        }
        Ok(())
    }

    fn request(
        &self,
        record_type: u16,
        object_id: u64,
        payload: PayloadMap,
        metadata: &RequestMetadata,
        transaction_id: Option<u64>,
        expected_target_generation: Option<u64>,
    ) -> io::Result<Option<Record>> {
        let request_id = self.next_request()?;
        let mut envelope = Envelope::correlated(request_id, payload)?;
        envelope.transaction_id = transaction_id;
        envelope.expected_target_generation = expected_target_generation;
        metadata.apply(&mut envelope)?;
        let reply =
            self.control
                .request(request_id, record_type, object_id, &envelope.encode()?)?;
        if let Some(record) = &reply {
            if record.record_type == messages::ERROR {
                return Err(presenter_error(&record.body)?);
            }
            let response = messages::decode_control(&record.body)?;
            if response.request_id != request_id {
                return Err(invalid_data("reply request ID does not match request"));
            }
        }
        Ok(reply)
    }

    fn next_request(&self) -> io::Result<u64> {
        self.next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid_data("request ID space exhausted"))
    }

    fn advance_allocator_past(&self, adopted_id: u64) -> io::Result<()> {
        let next = adopted_id
            .checked_add(1)
            .ok_or_else(|| invalid_data("adopted object ID exhausts the SDK allocator"))?;
        self.next_id.fetch_max(next, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed {
            // Dropping is cancellation/unclean loss, not a clean GOODBYE. This is intentional:
            // resumable sessions must be allowed to suspend rather than being silently destroyed.
            self.lifecycle.close("Vivid control session dropped");
            self.surfaces.clear();
            if let Ok(mut tracks) = self.tracks.lock() {
                tracks.clear();
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReady {
    pub context_id: u64,
    pub operation_classes: u64,
    pub contract: ResourceContract,
    pub lifetime_us: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLeaseReady {
    pub context_id: u64,
    pub lease_id: u64,
    pub state: u64,
    pub activation_timeout_us: u64,
    pub disconnect_grace_us: u64,
    pub cleanup_policy: u64,
    pub permitted_profiles: Vec<String>,
    pub contract: ResourceContract,
    pub revision: u64,
}

struct TrackReadyValues {
    revision: TrackRevision,
    generation: ChannelGeneration,
    open_deadline_us: u64,
    maximum_record_body: u32,
    effective_claims: PayloadMap,
    connection_required: bool,
    delta_operation_limit: u32,
}

/// One authenticated interactive-lane generation.
pub struct InputLane {
    writer: ConnectionWriter,
    shared: Arc<PendingInput>,
    lifecycle: Arc<SessionLifecycle>,
    lane_generation: u64,
    next_request_id: AtomicU64,
    offline: bool,
}

impl std::fmt::Debug for InputLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputLane")
            .field("lane_generation", &self.lane_generation)
            .field("closed", &self.shared.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl InputLane {
    pub const fn generation(&self) -> u64 {
        self.lane_generation
    }

    pub fn set_binding(&self, binding: &InputBinding) -> io::Result<InputBindingStatus> {
        self.lifecycle.ensure_active()?;
        binding.validate(binding.surface_id)?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "interactive lane is closed",
            ));
        }
        let request_id = self
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid_data("interactive request ID space exhausted"))?;
        let body = Envelope::correlated(request_id, binding.payload())?.encode()?;
        if self.offline {
            self.writer
                .write_record(messages::SET_INPUT_BINDING, 0, binding.surface_id, &body)?;
            return Ok(InputBindingStatus {
                producer_epoch: binding.producer_epoch.get(),
                grant_generation: binding.producer_epoch.get(),
                context_id: binding.context_id,
                surface_id: binding.surface_id,
                surface_generation: binding.surface_generation.get(),
                effective_classes: binding.requested_classes,
                state: u64::from(!binding.disabled()),
                reason: binding.reason,
                watchdog_timeout_us: binding.requested_watchdog_us,
            });
        }
        let (send, receive) = mpsc::channel();
        {
            let mut requests = lock(&self.shared.requests, "input request table")?;
            if self.shared.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interactive lane is closed",
                ));
            }
            if requests.insert(request_id, send).is_some() {
                return Err(invalid_data("duplicate interactive request ID"));
            }
        }
        if let Err(error) =
            self.writer
                .write_record(messages::SET_INPUT_BINDING, 0, binding.surface_id, &body)
        {
            let _ = lock(&self.shared.requests, "input request table")?.remove(&request_id);
            return Err(error);
        }
        let record = receive
            .recv()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interactive lane dispatcher stopped",
                )
            })?
            .map_err(|message| io::Error::new(io::ErrorKind::BrokenPipe, message))?;
        if record.record_type == messages::ERROR {
            return Err(presenter_error(&record.body)?);
        }
        expect_record(&record, messages::INPUT_BOUND, binding.surface_id)?;
        let payload = decoded_payload(&record)?;
        validate_exact_payload_keys("INPUT_BOUND", &payload, 0..=8)?;
        if required_u64(&payload, 0)? != binding.producer_epoch.get() {
            return Err(invalid_data(
                "INPUT_BOUND returned a different producer input epoch",
            ));
        }
        let status = InputBindingStatus {
            producer_epoch: required_u64(&payload, 0)?,
            grant_generation: required_u64(&payload, 1)?,
            context_id: required_u64(&payload, 2)?,
            surface_id: required_u64(&payload, 3)?,
            surface_generation: required_u64(&payload, 4)?,
            effective_classes: required_u64(&payload, 5)?,
            state: required_u64(&payload, 6)?,
            reason: required_u64(&payload, 7)?,
            watchdog_timeout_us: required_u64(&payload, 8)?,
        };
        if status.grant_generation == 0
            || status.state > 2
            || status.effective_classes & !binding.requested_classes != 0
            || (status.state == 1
                && (status.context_id != binding.context_id
                    || status.surface_id != binding.surface_id
                    || status.surface_generation != binding.surface_generation.get()
                    || status.effective_classes == 0
                    || !(vivid_protocol::input::MIN_WATCHDOG_US
                        ..=vivid_protocol::input::MAX_WATCHDOG_US)
                        .contains(&status.watchdog_timeout_us)))
        {
            return Err(invalid_data(
                "INPUT_BOUND contains an invalid effective grant",
            ));
        }
        Ok(status)
    }

    pub fn take_event(&self) -> io::Result<Option<InputLaneEvent>> {
        Ok(lock(&self.shared.events, "input event queue")?.pop_front())
    }

    pub fn close(&self) -> io::Result<()> {
        close_input_lane(&self.shared, "interactive lane closed");
        Ok(())
    }
}

impl Drop for InputLane {
    fn drop(&mut self) {
        close_input_lane(&self.shared, "interactive lane dropped");
    }
}

struct FlowSync {
    state: Mutex<FlowLocal>,
    changed: Condvar,
}

struct FlowLocal {
    flow: ChannelFlow,
    closed: bool,
    diagnostic: Option<String>,
}

struct ChannelMediaState {
    last_sequence: u64,
    needs_recovery: bool,
    minimum_recovery_epoch: u32,
    image_sent: bool,
    eos: bool,
}

struct ChannelRateState {
    body_bytes: TokenBucket,
    records: TokenBucket,
    updated_at: Instant,
}

/// One accepted, authenticated track-channel generation.
pub struct TrackChannel {
    track: Track,
    generation: ChannelGeneration,
    writer: ConnectionWriter,
    lifecycle: Arc<SessionLifecycle>,
    flow: Arc<FlowSync>,
    track_sequence: Arc<Mutex<TrackMediaSequence>>,
    media: Arc<Mutex<ChannelMediaState>>,
    events: Arc<Mutex<VecDeque<ChannelEvent>>>,
    rate: Option<Mutex<ChannelRateState>>,
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
    fn establish(
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
                Mutex::new(ChannelRateState {
                    body_bytes: TokenBucket::new(
                        byte_rate,
                        u64::from(snapshot.maximum_record_body),
                    ),
                    records: TokenBucket::new(snapshot.configuration.maximum_records_per_second, 1),
                    updated_at: Instant::now(),
                })
            }),
        })
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
        let state = lock(&self.track.inner, "track")?.clone();
        let KindConfiguration::Raster(raster) = state.configuration.kind else {
            return Err(invalid_input("RASTER_FRAME requires a raster track"));
        };
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
        let body = media::raster_delta_frame_body(
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
        )?;
        self.send_media(messages::RASTER_FRAME, frame_id, epoch, false, &body)
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

    fn send_media(
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

    fn write_charged_record(
        &self,
        record_type: u16,
        object_id: u64,
        body_length: u32,
        body: &[u8],
    ) -> io::Result<u64> {
        self.wait_for_rate(body_length)?;
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
                    let sequence = self.writer.write_record(record_type, 0, object_id, body)?;
                    state.flow = admitted;
                    return Ok(sequence);
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

    fn wait_for_rate(&self, body_length: u32) -> io::Result<()> {
        let Some(rate) = &self.rate else {
            return Ok(());
        };
        loop {
            self.lifecycle.ensure_active()?;
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

fn producer_lease_identity(authentication: &ProducerAuthentication) -> Option<(u64, u64)> {
    match authentication {
        ProducerAuthentication::LeaseActivation {
            context_id,
            lease_id,
            ..
        }
        | ProducerAuthentication::Resume {
            context_id,
            lease_id,
            ..
        } => Some((*context_id, *lease_id)),
        ProducerAuthentication::RootFromEnvironment | ProducerAuthentication::Root { .. } => None,
    }
}

fn hello_lease_identity(hello: &Hello) -> Option<(u64, u64)> {
    match &hello.authentication {
        HelloAuthentication::LeaseActivation {
            context_id,
            lease_id,
            ..
        }
        | HelloAuthentication::Resume {
            context_id,
            lease_id,
            ..
        } => Some((*context_id, *lease_id)),
        HelloAuthentication::Root { .. } => None,
    }
}

fn build_hello(config: &ProducerConfig, preface: &[u8; 16]) -> io::Result<(Hello, Secret32)> {
    let mut client_nonce = [0; auth::NONCE_BYTES];
    random_bytes(&mut client_nonce)?;
    let (authentication, session_secret) = match &config.authentication {
        ProducerAuthentication::RootFromEnvironment => {
            let value = env::var(vivid_protocol::discovery::ROOT_SECRET).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "VIVID_ROOT_SECRET is required for root authentication",
                )
            })?;
            let secret =
                Secret32::from_hex(&value).map_err(|error| invalid_input(error.to_string()))?;
            (HelloAuthentication::Root { proof: [0; 32] }, secret)
        }
        ProducerAuthentication::Root { root_secret } => (
            HelloAuthentication::Root { proof: [0; 32] },
            Secret32::new(*root_secret.expose()),
        ),
        ProducerAuthentication::LeaseActivation {
            context_id,
            lease_id,
            activation_secret,
            attempt_id,
            proof_of_possession,
        } => (
            HelloAuthentication::LeaseActivation {
                context_id: *context_id,
                lease_id: *lease_id,
                activation_secret: Secret32::new(*activation_secret.expose()),
                attempt_id: *attempt_id,
                proof_of_possession: proof_of_possession.clone(),
            },
            Secret32::new(*activation_secret.expose()),
        ),
        ProducerAuthentication::Resume {
            context_id,
            lease_id,
            session_id,
            resume_generation,
            attempt_id,
            prior_resume_key,
        } => (
            HelloAuthentication::Resume {
                context_id: *context_id,
                lease_id: *lease_id,
                session_id: *session_id,
                resume_generation: *resume_generation,
                attempt_id: *attempt_id,
                proof: [0; 32],
            },
            Secret32::new(*prior_resume_key.expose()),
        ),
    };
    let mut hello = Hello {
        producer_name: config.producer_name.clone(),
        producer_version: config.producer_version.clone(),
        required_profiles: config.required_profiles.clone(),
        optional_profiles: config.optional_profiles.clone(),
        maximum_control_body: config.maximum_control_body,
        client_nonce,
        authentication,
        target_profile: config.target_profile.clone(),
        extensions: vec![],
    };
    match &config.authentication {
        ProducerAuthentication::RootFromEnvironment | ProducerAuthentication::Root { .. } => {
            hello.authenticate_root(&session_secret, preface)?;
        }
        ProducerAuthentication::Resume { .. } => {
            hello.authenticate_resume(session_secret.expose(), preface)?;
        }
        ProducerAuthentication::LeaseActivation { .. } => {
            hello.validate()?;
        }
    }
    Ok((hello, session_secret))
}

fn spawn_control_reader(
    mut reader: ConnectionReader,
    writer: ConnectionWriter,
    pending: Arc<PendingControl>,
    lifecycle: Arc<SessionLifecycle>,
    tracks: Arc<Mutex<TrackRegistry>>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("vivid-control-reader".into())
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                loop {
                    let record = reader.read_record()?;
                    let envelope = messages::decode_control(&record.body)?;
                    if record.record_type == messages::PING {
                        writer.write_record(
                            messages::PONG,
                            0,
                            0,
                            &Envelope::new(envelope.request_id, envelope.payload).encode()?,
                        )?;
                        continue;
                    }
                    if envelope.request_id != 0 {
                        let Some(sender) = lock(&pending.requests, "pending request table")?
                            .remove(&envelope.request_id)
                        else {
                            return Err(invalid_data(
                                "control reply has no matching pending request",
                            ));
                        };
                        let _ = sender.send(Ok(record));
                        continue;
                    }
                    if record.record_type == messages::TRACK_LOST {
                        apply_track_lost(record.object_id, &envelope.payload, &tracks)?;
                    }
                    let event =
                        session_event(record.record_type, record.object_id, envelope.payload);
                    let mut events = lock(&pending.events, "control event queue")?;
                    if events.len() == MAX_CONTROL_EVENTS {
                        return Err(invalid_data("control event queue exceeded its bound"));
                    }
                    events.push_back(event);
                }
            })();
            pending.closed.store(true, Ordering::Release);
            let message = result.err().map_or_else(
                || "control connection closed".into(),
                |error| error.to_string(),
            );
            lifecycle.close(&message);
            if let Ok(mut events) = pending.events.lock() {
                events.clear();
                events.push_back(SessionEvent::ConnectionClosed {
                    diagnostic: message.clone(),
                });
            }
            if let Ok(mut requests) = pending.requests.lock() {
                for (_, sender) in requests.drain() {
                    let _ = sender.send(Err(message.clone()));
                }
            }
        })
        .map(|_| ())
}

fn apply_track_lost(
    object_id: u64,
    payload: &PayloadMap,
    tracks: &Mutex<TrackRegistry>,
) -> io::Result<()> {
    validate_exact_payload_keys("TRACK_LOST", payload, 0..=6)?;
    let context_id = required_u64(payload, 0)?;
    let surface_id = required_u64(payload, 1)?;
    let track_id = required_u64(payload, 2)?;
    let error_code = required_u64(payload, 3)?;
    let revision = TrackRevision::new(required_u64(payload, 4)?);
    let _detail = ErrorDetail::new(required_map(payload, 5)?.to_vec()).map_err(io::Error::other)?;
    let diagnostic = required_text(payload, 6)?;
    if track_id != object_id
        || context_id == 0
        || surface_id == 0
        || track_id == 0
        || error_code == 0
        || diagnostic.len() > 4096
    {
        return Err(invalid_data("TRACK_LOST contains invalid actionable state"));
    }
    revision.require_nonzero()?;
    let track = lock(tracks, "track registry")?
        .get(&(context_id, surface_id, track_id))
        .cloned();
    if let Some(track) = track {
        let mut state = lock(&track, "track")?;
        if revision < state.revision {
            return Err(invalid_data("TRACK_LOST moves track revision backward"));
        }
        state.revision = revision;
        state.destroyed = true;
        state.active_media = None;
        let active_flow = state.active_flow.take();
        close_track_flow(active_flow.as_ref(), diagnostic);
    }
    Ok(())
}

fn spawn_input_reader(
    mut reader: ConnectionReader,
    writer: ConnectionWriter,
    pending: Arc<PendingInput>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("vivid-input-reader".into())
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                loop {
                    let record = reader.read_record()?;
                    let envelope = messages::decode_control(&record.body)?;
                    if record.record_type == messages::PING {
                        if record.object_id != 0 {
                            return Err(invalid_data("interactive PING has a nonzero object ID"));
                        }
                        writer.write_record(
                            messages::PONG,
                            0,
                            0,
                            &Envelope::new(envelope.request_id, envelope.payload).encode()?,
                        )?;
                        continue;
                    }
                    if envelope.request_id != 0 {
                        let Some(sender) = lock(&pending.requests, "input request table")?
                            .remove(&envelope.request_id)
                        else {
                            return Err(invalid_data(
                                "interactive reply has no matching pending request",
                            ));
                        };
                        let _ = sender.send(Ok(record));
                        continue;
                    }
                    let event = match record.record_type {
                        messages::KEY_INPUT
                        | messages::POINTER_MOTION
                        | messages::POINTER_BUTTON
                        | messages::POINTER_AXIS => InputLaneEvent::Input {
                            record_type: record.record_type,
                            surface_id: record.object_id,
                            payload: envelope.payload,
                        },
                        messages::INPUT_LEASE_RENEW => InputLaneEvent::Renew(decode_input_renewal(
                            record.object_id,
                            &envelope.payload,
                        )?),
                        messages::INPUT_REVOKED => {
                            InputLaneEvent::Revoked(decode_input_termination(
                                "INPUT_REVOKED",
                                record.object_id,
                                &envelope.payload,
                            )?)
                        }
                        messages::INPUT_RESET => InputLaneEvent::Reset(decode_input_termination(
                            "INPUT_RESET",
                            record.object_id,
                            &envelope.payload,
                        )?),
                        messages::ERROR => {
                            InputLaneEvent::Error(messages::parse_error_reply(&record.body)?.into())
                        }
                        _ if record.flags & vivid_protocol::wire::RECORD_OPTIONAL != 0 => continue,
                        _ => {
                            return Err(invalid_data(
                                "unexpected required record on interactive lane",
                            ));
                        }
                    };
                    let mut events = lock(&pending.events, "input event queue")?;
                    if events.len() == MAX_INPUT_EVENTS {
                        return Err(invalid_data(
                            "interactive input queue exceeded its safety bound",
                        ));
                    }
                    events.push_back(event);
                }
            })();
            let message = result.err().map_or_else(
                || "interactive lane closed".into(),
                |error| error.to_string(),
            );
            close_input_lane(&pending, &message);
        })
        .map(|_| ())
}

fn close_input_lane(pending: &PendingInput, message: &str) {
    pending.closed.store(true, Ordering::Release);
    if let Ok(mut events) = pending.events.lock() {
        events.clear();
        events.push_back(InputLaneEvent::LaneClosed {
            diagnostic: message.to_owned(),
        });
    }
    fail_pending_input(pending, message);
}

fn fail_pending_input(pending: &PendingInput, message: &str) {
    if let Ok(mut requests) = pending.requests.lock() {
        for (_, sender) in requests.drain() {
            let _ = sender.send(Err(message.to_owned()));
        }
    }
}

fn decode_input_tuple(
    schema: &str,
    object_id: u64,
    payload: &PayloadMap,
) -> io::Result<InputTuple> {
    let tuple = InputTuple {
        producer_epoch: InputEpoch::new(required_u64(payload, 0)?),
        grant_generation: GrantGeneration::new(required_u64(payload, 1)?),
        context_id: required_u64(payload, 2)?,
        surface_id: required_u64(payload, 3)?,
        surface_generation: SurfaceGeneration::new(required_u64(payload, 4)?),
    };
    if tuple.producer_epoch == InputEpoch::ZERO
        || tuple.grant_generation == GrantGeneration::ZERO
        || tuple.context_id == 0
        || tuple.surface_id == 0
        || tuple.surface_generation == SurfaceGeneration::ZERO
        || tuple.surface_id != object_id
    {
        return Err(invalid_data(format!(
            "{schema} contains an invalid owner-qualified input tuple"
        )));
    }
    Ok(tuple)
}

fn decode_input_renewal(object_id: u64, payload: &PayloadMap) -> io::Result<InputLeaseRenewal> {
    validate_exact_payload_keys("INPUT_LEASE_RENEW", payload, 0..=6)?;
    let renewal_sequence = required_u64(payload, 5)?;
    let watchdog_timeout_us = required_u64(payload, 6)?;
    if renewal_sequence == 0
        || !(vivid_protocol::input::MIN_WATCHDOG_US..=vivid_protocol::input::MAX_WATCHDOG_US)
            .contains(&watchdog_timeout_us)
    {
        return Err(invalid_data(
            "INPUT_LEASE_RENEW has an invalid sequence or watchdog",
        ));
    }
    Ok(InputLeaseRenewal {
        binding: decode_input_tuple("INPUT_LEASE_RENEW", object_id, payload)?,
        renewal_sequence,
        watchdog_timeout_us,
    })
}

fn decode_input_termination(
    schema: &str,
    object_id: u64,
    payload: &PayloadMap,
) -> io::Result<InputGrantTermination> {
    validate_exact_payload_keys(schema, payload, 0..=5)?;
    let reason = required_u64(payload, 5)?;
    if reason == 0 || reason > 10 {
        return Err(invalid_data(format!("{schema} has an unknown reason")));
    }
    Ok(InputGrantTermination {
        binding: decode_input_tuple(schema, object_id, payload)?,
        reason,
    })
}

fn spawn_channel_reader(
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

fn push_channel_event(
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

fn close_track_flow(flow: Option<&Weak<FlowSync>>, message: &str) {
    let Some(flow) = flow.and_then(Weak::upgrade) else {
        return;
    };
    if let Ok(mut state) = flow.state.lock() {
        state.closed = true;
        state.diagnostic.get_or_insert_with(|| message.to_owned());
        flow.changed.notify_all();
    }
}

fn session_event(record_type: u16, object_id: u64, payload: PayloadMap) -> SessionEvent {
    match record_type {
        messages::TARGET_CHANGED => SessionEvent::TargetChanged(payload),
        messages::ANCHOR_READY => SessionEvent::AnchorReady {
            context_id: optional_u64(&payload, 0)
                .unwrap_or(None)
                .unwrap_or_default(),
            anchor_id: optional_u64(&payload, 1)
                .unwrap_or(None)
                .unwrap_or(object_id),
            payload,
        },
        messages::ANCHOR_GONE => SessionEvent::AnchorGone {
            context_id: optional_u64(&payload, 0)
                .unwrap_or(None)
                .unwrap_or_default(),
            anchor_id: optional_u64(&payload, 1)
                .unwrap_or(None)
                .unwrap_or(object_id),
            payload,
        },
        messages::TRACK_LOST => SessionEvent::TrackLost { object_id, payload },
        messages::CONTEXT_CHANGED => SessionEvent::ContextChanged { object_id, payload },
        _ => SessionEvent::Other {
            record_type,
            object_id,
            payload,
        },
    }
}

fn session_info(welcome: &messages::Welcome) -> SessionInfo {
    SessionInfo {
        session_id: welcome.session_id,
        session_tag: welcome.session_tag,
        root_context_id: welcome.root_context_id,
        target_generation: TargetGeneration::new(welcome.target_generation),
        target_profile: welcome.target_profile.clone(),
        target_descriptor: welcome.target_descriptor.clone(),
        accepted_profiles: welcome.accepted_profiles.clone(),
        session_revision: welcome.session_revision,
        scene_revision: SceneRevision::new(welcome.scene_revision),
        establishment_state: welcome.establishment_state,
        resume_generation: welcome.resume_generation,
        resource_contract: welcome.resource_contract.clone(),
    }
}

fn decoded_payload(record: &Record) -> io::Result<PayloadMap> {
    Ok(messages::decode_control(&record.body)?.payload)
}

fn presenter_error(body: &[u8]) -> io::Result<io::Error> {
    Ok(io::Error::other(PresenterError::from(
        messages::parse_error_reply(body)?,
    )))
}

fn expect_record(record: &Record, expected: u16, object_id: u64) -> io::Result<()> {
    if record.record_type != expected || record.object_id != object_id {
        return Err(invalid_data(format!(
            "expected record {expected:#06x} for object {object_id}, received {:#06x} for {}",
            record.record_type, record.object_id
        )));
    }
    Ok(())
}

fn validate_owner_pair(payload: &PayloadMap, context_id: u64, object_id: u64) -> io::Result<()> {
    if required_u64(payload, 0)? != context_id || required_u64(payload, 1)? != object_id {
        return Err(invalid_data(
            "reply contains the wrong owner-qualified identity",
        ));
    }
    Ok(())
}

fn validate_track_owner(
    payload: &PayloadMap,
    configuration: &TrackConfiguration,
) -> io::Result<()> {
    validate_track_tuple(payload, configuration)
}

fn validate_track_tuple(
    payload: &PayloadMap,
    configuration: &TrackConfiguration,
) -> io::Result<()> {
    if required_u64(payload, 0)? != configuration.context_id
        || required_u64(payload, 1)? != configuration.surface_id
        || required_u64(payload, 2)? != configuration.track_id
    {
        return Err(invalid_data(
            "reply contains the wrong complete track identity",
        ));
    }
    Ok(())
}

fn required_value(payload: &PayloadMap, key: u64) -> io::Result<&Value> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .ok_or_else(|| invalid_data(format!("reply omits payload key {key}")))
}

fn validate_exact_payload_keys(
    schema: &str,
    payload: &PayloadMap,
    expected: std::ops::RangeInclusive<u64>,
) -> io::Result<()> {
    let expected = expected.collect::<Vec<_>>();
    if payload.len() != expected.len()
        || payload
            .iter()
            .zip(expected)
            .any(|((actual, _), expected)| *actual != expected)
    {
        return Err(invalid_data(format!(
            "{schema} payload keys are not the exact canonical schema"
        )));
    }
    Ok(())
}

fn validate_payload_keys(
    schema: &str,
    payload: &PayloadMap,
    required: std::ops::RangeInclusive<u64>,
    optional: &[u64],
) -> io::Result<()> {
    let required = required.collect::<Vec<_>>();
    if payload.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || required
            .iter()
            .any(|key| !payload.iter().any(|(actual, _)| actual == key))
        || payload
            .iter()
            .any(|(key, _)| !required.contains(key) && !optional.contains(key))
    {
        return Err(invalid_data(format!(
            "{schema} payload keys do not match its canonical schema"
        )));
    }
    Ok(())
}

fn required_u64(payload: &PayloadMap, key: u64) -> io::Result<u64> {
    required_value(payload, key)?
        .as_u64()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not an unsigned integer")))
}

fn required_i64(payload: &PayloadMap, key: u64) -> io::Result<i64> {
    required_value(payload, key)?
        .as_i64()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not a signed integer")))
}

fn optional_u64(payload: &PayloadMap, key: u64) -> io::Result<Option<u64>> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| invalid_data(format!("payload key {key} is not unsigned")))
        })
        .transpose()
}

fn optional_map(payload: &PayloadMap, key: u64) -> io::Result<Option<&PayloadMap>> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .map(|value| match value {
            Value::Map(value) => Ok(value),
            _ => Err(invalid_data(format!("payload key {key} is not a map"))),
        })
        .transpose()
}

fn required_u32(payload: &PayloadMap, key: u64) -> io::Result<u32> {
    u32::try_from(required_u64(payload, key)?)
        .map_err(|_| invalid_data(format!("payload key {key} exceeds u32")))
}

fn required_bool(payload: &PayloadMap, key: u64) -> io::Result<bool> {
    required_value(payload, key)?
        .as_bool()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not a boolean")))
}

fn required_text(payload: &PayloadMap, key: u64) -> io::Result<&str> {
    required_value(payload, key)?
        .as_text()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not text")))
}

fn required_map(payload: &PayloadMap, key: u64) -> io::Result<&PayloadMap> {
    match required_value(payload, key)? {
        Value::Map(value) => Ok(value),
        _ => Err(invalid_data(format!("payload key {key} is not a map"))),
    }
}

fn required_text_array(payload: &PayloadMap, key: u64) -> io::Result<Vec<String>> {
    required_value(payload, key)?
        .as_array()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not an array")))?
        .iter()
        .map(|value| {
            value
                .as_text()
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid_data(format!("payload key {key} contains non-text")))
        })
        .collect()
}

fn required_contract(payload: &PayloadMap, key: u64) -> io::Result<ResourceContract> {
    ResourceContract::from_value(required_value(payload, key)?).map_err(io::Error::other)
}

fn ensure_live_surface(state: &SurfaceLocal) -> io::Result<()> {
    if state.destroyed {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "surface is destroyed",
        ))
    } else {
        Ok(())
    }
}

fn ensure_live_track(state: &TrackLocal) -> io::Result<()> {
    if state.destroyed {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "track is destroyed",
        ))
    } else {
        Ok(())
    }
}

fn endpoint(explicit: Option<&str>, variable: &str) -> io::Result<Endpoint> {
    let value = explicit
        .map(ToOwned::to_owned)
        .or_else(|| env::var(variable).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("required Vivid discovery variable {variable} is absent"),
            )
        })?;
    Endpoint::parse(&value)
}

fn optional_endpoint(explicit: Option<&str>, variable: &str) -> io::Result<Option<Endpoint>> {
    explicit
        .map(ToOwned::to_owned)
        .or_else(|| env::var(variable).ok())
        .map(|value| Endpoint::parse(&value))
        .transpose()
}

fn offline_endpoint() -> io::Result<Endpoint> {
    #[cfg(unix)]
    {
        Ok(Endpoint::Unix(PathBuf::from("/dev/null")))
    }
    #[cfg(not(unix))]
    {
        Endpoint::parse("tcp:127.0.0.1:1")
    }
}

fn offline_contract() -> ResourceContract {
    let mut contract = ResourceContract::new([1_000_000; 33]);
    contract.set(
        Resource::MediaRecordBody,
        u64::from(vivid_protocol::HARD_MAX_RECORD_BODY),
    );
    contract.set(
        Resource::ControlRecordBody,
        u64::from(vivid_protocol::CONTROL_MAX_RECORD_BODY),
    );
    contract
}

fn offline_target_descriptor() -> PayloadMap {
    vec![
        (0, Value::Unsigned(1920)),
        (1, Value::Unsigned(1080)),
        (2, Value::Unsigned(80)),
        (3, Value::Unsigned(24)),
        (4, Value::Unsigned(24)),
        (5, Value::Unsigned(45)),
        (6, Value::Bool(true)),
        (7, Value::Unsigned(3)),
        (8, Value::Unsigned(256)),
    ]
}

fn validate_terminal_target_descriptor(descriptor: &PayloadMap) -> io::Result<()> {
    validate_exact_payload_keys("terminal target descriptor", descriptor, 0..=8)?;
    for key in 0..=5 {
        if required_u64(descriptor, key)? == 0 {
            return Err(invalid_data(
                "terminal target descriptor contains a zero dimension",
            ));
        }
    }
    let _settled = required_bool(descriptor, 6)?;
    if required_u64(descriptor, 7)? != 3 || required_u64(descriptor, 8)? == 0 {
        return Err(invalid_data(
            "terminal target descriptor has unsupported anchor capabilities",
        ));
    }
    Ok(())
}

fn validate_profiles(profiles: &[String]) -> io::Result<()> {
    if profiles.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_input("profile lists must be sorted and unique"));
    }
    Ok(())
}

fn random_bytes(bytes: &mut [u8]) -> io::Result<()> {
    getrandom::fill(bytes)
        .map_err(|error| io::Error::other(format!("secure randomness failed: {error}")))
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> io::Result<std::sync::MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other(format!("{name} lock is poisoned")))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn signed(value: i64) -> Value {
    if value >= 0 {
        Value::Unsigned(value as u64)
    } else {
        Value::Negative(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::track::RasterConfiguration;

    fn surface(context_id: u64, surface_id: u64) -> SurfaceDefinition {
        SurfaceDefinition {
            context_id,
            surface_id,
            semantic_profile: GENERIC_CONTENT.into(),
            coordinate_model: CoordinateModel::DesktopLogicalPixels,
            logical_width: 2,
            logical_height: 2,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: 0,
            descriptor: SurfaceDescriptor {
                role: SurfaceRole::Figure,
                title: "test".into(),
                semantic_content_revision: 1,
                semantic_availability: 0,
                locator_hint: String::new(),
            },
            policy: 0,
            profile_parameters: vec![],
        }
    }

    fn raster_track(context_id: u64, surface_id: u64, track_id: u64) -> TrackConfiguration {
        TrackConfiguration {
            context_id,
            surface_id,
            track_id,
            slot: 3,
            mode: TrackMode::Live,
            lane: LaneClass::Bulk,
            maximum_record_body: 88,
            maximum_rate_millihertz: 60_000,
            maximum_encoded_bits_per_second: 1_000_000,
            maximum_records_per_second: 60,
            maximum_inflight_body_bytes: 176,
            kind: KindConfiguration::Raster(RasterConfiguration {
                width: 2,
                height: 2,
                alpha_mode: 1,
                delta_enabled: false,
                maximum_delta_operations: 1,
                zstd_enabled: false,
            }),
            target_latency_us: 16_000,
            maximum_latency_us: 100_000,
            retained_pixel_charge: 4,
        }
    }

    #[test]
    fn offline_lifecycle_uses_surfaces_tracks_and_ordered_eos() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        let media_sequence = channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
        let eos_sequence = channel.eos().unwrap();
        assert!(eos_sequence > media_sequence);
        assert_eq!(surface.id(), 7);
        assert_eq!(track.surface_id(), 7);
        assert_eq!(track.channel_generation(), ChannelGeneration::ONE);
        session.close().unwrap();
    }

    #[test]
    fn track_status_may_lag_an_independently_ordered_media_connection() {
        let mut submitted = TrackMediaSequence {
            last_id: 12,
            last_epoch: 3,
            last_record_sequence: 14,
        };
        let mut status = TrackStatus {
            context_id: 1,
            surface_id: 2,
            track_id: 3,
            revision: TrackRevision::new(4),
            kind: TrackKind::Video,
            mode: TrackMode::Timed,
            lifecycle: 2,
            channel_generation: ChannelGeneration::ONE,
            attachment_state: 1,
            milestones: MILESTONE_OUTPUT_READY,
            media_epoch: 2,
            last_media_id: 9,
            last_media_record_sequence: 11,
            last_decoded_pts_us: 0,
            last_presented_pts_us: 0,
            last_presentation_id: 0,
            cumulative_body_bytes: 100,
            cumulative_media_records: 9,
            maximum_body_bytes: 1_000,
            maximum_media_records: 100,
            ingress_depth_bucket: 0,
            playback_state: None,
            terminal_loss_code: None,
        };

        submitted.reconcile_status(&status, false);
        assert_eq!(
            submitted,
            TrackMediaSequence {
                last_id: 12,
                last_epoch: 3,
                last_record_sequence: 14,
            }
        );

        status.last_media_id = 15;
        status.media_epoch = 4;
        status.last_media_record_sequence = 2;
        submitted.reconcile_status(&status, true);
        assert_eq!(
            submitted,
            TrackMediaSequence {
                last_id: 15,
                last_epoch: 4,
                last_record_sequence: 2,
            }
        );
    }

    #[test]
    fn track_replacement_preserves_surface_generation() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 5), &RequestMetadata::default())
            .unwrap();
        let first = session
            .create_track(raster_track(1, 5, 1), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_track(raster_track(1, 5, 2), &RequestMetadata::default())
            .unwrap();
        let generation = surface.generation();
        session
            .destroy_track(&first, &RequestMetadata::default())
            .unwrap();
        assert_eq!(surface.generation(), generation);
        assert_eq!(second.surface_id(), surface.id());
    }

    #[test]
    fn complete_owner_identity_prevents_same_id_aliasing() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let first = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_surface(surface(2, 7), &RequestMetadata::default())
            .unwrap();
        session
            .destroy_surface(&first, &RequestMetadata::default())
            .unwrap();
        assert_eq!(second.context_id(), 2);
        assert_eq!(second.id(), 7);
        assert!(!lock(&second.inner, "surface").unwrap().destroyed);
    }

    #[test]
    fn config_rejects_feature_id_style_or_unsorted_profiles() {
        let mut config = ProducerConfig::offline();
        config.required_profiles.reverse();
        assert!(config.validate().is_err());
    }

    #[test]
    fn secret_bearing_configuration_has_no_debug_surface() {
        let authentication = ProducerAuthentication::root_hex(&"11".repeat(32)).unwrap();
        let config = ProducerConfig {
            authentication,
            dry_run: true,
            ..ProducerConfig::default()
        };
        let session = Session::connect(config).unwrap();
        let debug = format!("{session:?}");
        assert!(!debug.contains(&"11".repeat(8)));
        assert!(!debug.contains("ROOT_SECRET"));
    }

    #[test]
    fn surface_mapping_change_advances_only_surface_generation() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let handle = session
            .create_surface(surface(1, 3), &RequestMetadata::default())
            .unwrap();
        let mut replacement = handle.definition().unwrap();
        replacement.logical_width = 3;
        session
            .update_surface(&handle, replacement, &RequestMetadata::default())
            .unwrap();
        assert_eq!(handle.generation().get(), 2);
        assert_eq!(handle.revision().get(), 2);
    }

    #[test]
    fn desktop_input_requires_explicit_profile_and_uses_a_separate_lane() {
        let default_session = Session::connect(ProducerConfig::offline()).unwrap();
        assert_eq!(
            default_session.open_input_lane(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );

        let mut config = ProducerConfig::offline();
        config.target_profile = DESKTOP_SURFACE.into();
        config.required_profiles = vec![
            DESKTOP_INPUT.into(),
            DESKTOP_SURFACE.into(),
            LIVE_MEDIA.into(),
            CORE_CONTROL.into(),
        ];
        config.optional_profiles = vec![];
        let session = Session::connect(config).unwrap();
        let lane = session.open_input_lane(1).unwrap();
        assert_eq!(lane.generation(), 1);
        let status = lane
            .set_binding(&InputBinding {
                producer_epoch: InputEpoch::ONE,
                context_id: 1,
                surface_id: 7,
                surface_generation: SurfaceGeneration::ONE,
                requested_classes: INPUT_CLASS_KEYBOARD,
                reason: 6,
                requested_watchdog_us: 2_000_000,
            })
            .unwrap();
        assert_eq!(status.state, 1);
        assert_eq!(status.effective_classes, INPUT_CLASS_KEYBOARD);
        lane.close().unwrap();
    }

    #[test]
    fn input_events_remain_generation_qualified_until_final_injection_gate() {
        let binding = InputTuple {
            producer_epoch: InputEpoch::ONE,
            grant_generation: GrantGeneration::ONE,
            context_id: 1,
            surface_id: 7,
            surface_generation: SurfaceGeneration::ONE,
        };
        let key = InputEvent::Key {
            binding,
            usage: 0x04,
            pressed: true,
        };
        let raw = InputLaneEvent::Input {
            record_type: messages::KEY_INPUT,
            surface_id: 7,
            payload: key.payload(),
        };
        assert_eq!(raw.decode_input(1920, 1080).unwrap(), key);

        let wrong_owner = InputLaneEvent::Input {
            record_type: messages::KEY_INPUT,
            surface_id: 8,
            payload: key.payload(),
        };
        assert!(wrong_owner.decode_input(1920, 1080).is_err());
    }

    #[test]
    fn input_and_track_queue_pressure_fail_closed_without_dropping_transitions() {
        let pending = PendingInput {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::from([InputLaneEvent::Input {
                record_type: messages::KEY_INPUT,
                surface_id: 7,
                payload: vec![],
            }])),
            closed: AtomicBool::new(false),
        };
        close_input_lane(&pending, "test lane loss");
        assert!(pending.closed.load(Ordering::Acquire));
        let events = lock(&pending.events, "input events").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.front(),
            Some(InputLaneEvent::LaneClosed { diagnostic })
                if diagnostic == "test lane loss"
        ));
        drop(events);

        let channel_events = Mutex::new(VecDeque::from(vec![
            ChannelEvent::NeedKeyframe(vec![]);
            MAX_CHANNEL_EVENTS
        ]));
        assert!(push_channel_event(&channel_events, ChannelEvent::NeedFullFrame(vec![])).is_err());
        assert_eq!(
            lock(&channel_events, "channel events").unwrap().len(),
            MAX_CHANNEL_EVENTS
        );
    }

    #[test]
    fn control_loss_stops_existing_media_and_input_lanes() {
        let mut config = ProducerConfig::offline();
        config.target_profile = DESKTOP_SURFACE.into();
        config.required_profiles = vec![
            DESKTOP_INPUT.into(),
            DESKTOP_SURFACE.into(),
            LIVE_MEDIA.into(),
            CORE_CONTROL.into(),
        ];
        config.optional_profiles = vec![];
        let mut session = Session::connect(config).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        let lane = session.open_input_lane(1).unwrap();

        drop(session);

        assert_eq!(
            channel
                .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            lane.set_binding(&InputBinding {
                producer_epoch: InputEpoch::ONE,
                context_id: 1,
                surface_id: surface.id(),
                surface_generation: surface.generation(),
                requested_classes: INPUT_CLASS_KEYBOARD,
                reason: 6,
                requested_watchdog_us: 2_000_000,
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(matches!(
            lane.take_event().unwrap(),
            Some(InputLaneEvent::LaneClosed { .. })
        ));
    }

    #[test]
    fn channel_advance_closes_old_transport_and_preserves_track_media_ids() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let first = session.open_track_channel(&track).unwrap();
        first
            .send_raster(0, 5, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();

        assert_eq!(
            session
                .advance_channel(&track, 1, &RequestMetadata::default())
                .unwrap(),
            ChannelGeneration::new(2)
        );
        assert_eq!(
            first
                .send_raster(0, 6, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );

        let second = session.open_track_channel(&track).unwrap();
        assert!(
            second
                .send_raster(0, 5, &[0, 0, 0, 255].repeat(4), false)
                .is_err()
        );
        second
            .send_raster(0, 6, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn status_wait_and_flush_keep_generation_and_epoch_domains_separate() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();

        let surface_status = session.query_surface(&surface).unwrap();
        assert_eq!(surface_status.generation, SurfaceGeneration::ONE);
        let track_status = session.query_track(&track).unwrap();
        assert_eq!(track_status.channel_generation, ChannelGeneration::ONE);
        assert_eq!(track_status.last_media_id, 1);
        assert_eq!(track_status.cumulative_media_records, 1);
        let waited = session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_CHANNEL_ACCEPTED),
                1_000_000,
            )
            .unwrap();
        assert_eq!(waited.channel_generation, ChannelGeneration::ONE);

        session.flush(&track, 1).unwrap();
        assert!(session.flush(&track, 1).is_err());
        assert!(
            channel
                .send_raster(0, 2, &[0, 0, 0, 255].repeat(4), false)
                .is_err()
        );
        channel
            .send_raster(1, 2, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn resumed_session_can_adopt_owner_qualified_handles_and_advance_channels() {
        let mut original = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = original
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = original
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let old_channel = original.open_track_channel(&track).unwrap();
        old_channel
            .send_raster(0, 10, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
        drop(original);

        let mut resumed = Session::connect(ProducerConfig::offline()).unwrap();
        resumed.adopt_surface(&surface).unwrap();
        resumed.adopt_track(&track).unwrap();
        assert!(resumed.allocate_id().unwrap() > 9);
        resumed
            .advance_channel(&track, 1, &RequestMetadata::default())
            .unwrap();
        let channel = resumed.open_track_channel(&track).unwrap();
        channel
            .send_raster(0, 11, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn leased_sessions_prepare_secret_redacted_resume_authentication() {
        let root = Session::connect(ProducerConfig::offline()).unwrap();
        let Err(error) = root.resume_authentication() else {
            panic!("root session unexpectedly produced resume authentication");
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);

        let mut config = ProducerConfig::offline();
        config.authentication = ProducerAuthentication::LeaseActivation {
            context_id: 4,
            lease_id: 8,
            activation_secret: Secret32::new([0x55; 32]),
            attempt_id: [0x44; auth::ATTEMPT_ID_BYTES],
            proof_of_possession: None,
        };
        let leased = Session::connect(config).unwrap();
        match leased.resume_authentication().unwrap() {
            ProducerAuthentication::Resume {
                context_id,
                lease_id,
                session_id,
                resume_generation,
                ..
            } => {
                assert_eq!((context_id, lease_id), (4, 8));
                assert_eq!(session_id, OFFLINE_SESSION_ID);
                assert_eq!(resume_generation, 0);
            }
            _ => panic!("leased session did not return resume authentication"),
        }
    }

    #[test]
    fn actionable_track_loss_is_scoped_by_complete_owner_identity() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        session
            .create_surface(surface(2, 7), &RequestMetadata::default())
            .unwrap();
        let first = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_track(raster_track(2, 7, 9), &RequestMetadata::default())
            .unwrap();
        let first_channel = session.open_track_channel(&first).unwrap();
        let second_channel = session.open_track_channel(&second).unwrap();

        apply_track_lost(
            9,
            &vec![
                (0, Value::Unsigned(1)),
                (1, Value::Unsigned(7)),
                (2, Value::Unsigned(9)),
                (3, Value::Unsigned(18)),
                (4, Value::Unsigned(3)),
                (5, Value::Map(vec![])),
                (6, Value::Text("decoder lost".into())),
            ],
            &session.tracks,
        )
        .unwrap();

        assert!(lock(&first.inner, "track").unwrap().destroyed);
        assert!(!lock(&second.inner, "track").unwrap().destroyed);
        assert_eq!(
            first_channel
                .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        second_channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn terminal_target_changes_and_marker_transports_are_generation_safe() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let mut changed = session.info().target_descriptor.clone();
        changed.push((9, Value::Unsigned(2)));
        changed.push((10, Value::Unsigned(1)));
        assert_eq!(
            session.apply_target_changed(&changed).unwrap(),
            TargetGeneration::new(2)
        );
        assert_eq!(session.info().target_generation, TargetGeneration::new(2));

        let context_id = session.info().root_context_id;
        let marker = session.conpty_anchor_marker(context_id, 77).unwrap();
        let parsed = anchor::parse_conpty_marker(&marker).unwrap();
        assert_eq!((parsed.context_id, parsed.anchor_id), (context_id, 77));
        let status = session.query_anchor(context_id, 77).unwrap();
        assert_eq!(status.state, 0);
        assert_eq!(status.target_generation, Some(TargetGeneration::new(2)));
    }
}
