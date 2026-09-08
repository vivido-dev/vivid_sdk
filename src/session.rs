//! The control plane: connection establishment, request correlation, and the reader thread.
//!
//! [`Session`] owns the one control connection and the object registries every other module
//! reaches through. Requests are correlated by request ID; unsolicited records become
//! [`SessionEvent`]s rather than blocking a reply.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::{io, thread};

use vivid_protocol::anchor::AnchorKey;
use vivid_protocol::auth::Secret32;
use vivid_protocol::messages::{Envelope, Hello, HelloAuthentication, PayloadMap};
use vivid_protocol::resource::ResourceContract;
use vivid_protocol::revision::{SceneRevision, TargetGeneration, TrackRevision};
use vivid_protocol::wire::{Connection, ConnectionReader, ConnectionWriter, Endpoint, Record};
use vivid_protocol::{auth, messages};

use crate::*;

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

impl SessionInfo {
    /// The parsed desktop target, or `None` when this session's target is not a desktop.
    ///
    /// The descriptor is already validated for the negotiated profile, so a producer reads the
    /// virtual rectangle, output topology, settle flag, and topology revision from here rather
    /// than re-parsing a payload map — and a terminal session gets `None` instead of a guess.
    pub fn desktop_target(&self) -> Option<vivid_protocol::target::DesktopTarget> {
        if self.target_profile != DESKTOP_SURFACE {
            return None;
        }
        vivid_protocol::target::DesktopTarget::decode(&self.target_descriptor).ok()
    }

    /// Whether the target is settled: geometry has stopped moving for this generation.
    pub fn target_settled(&self) -> io::Result<bool> {
        descriptor_settled(&self.target_profile, &self.target_descriptor)
    }
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
    /// A user-originated regular file was dropped on an effective presenter binding.
    FileDropOffered(FileDropOffer),
    /// The presenter cancelled an offered or accepted drop asynchronously.
    FileDropCancelled(CancelFileDrop),
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

pub(crate) struct Endpoints {
    pub(crate) interactive: Option<Endpoint>,
    pub(crate) realtime: Option<Endpoint>,
    pub(crate) bulk: Option<Endpoint>,
}

pub(crate) struct PendingControl {
    pub(crate) requests: Mutex<HashMap<u64, mpsc::Sender<Result<Record, String>>>>,
    pub(crate) events: Mutex<VecDeque<SessionEvent>>,
    pub(crate) closed: AtomicBool,
}

pub(crate) struct PendingInput {
    pub(crate) requests: Mutex<HashMap<u64, mpsc::Sender<Result<Record, String>>>>,
    pub(crate) events: Mutex<VecDeque<InputLaneEvent>>,
    pub(crate) closed: AtomicBool,
}

/// What ended a session, kept beside the diagnostic so callers are not left matching on its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseCause {
    /// The control reader saw the connection end. Nothing more can be written.
    Lost,
    /// [`Session::cancel_handle`] fired, which shuts the writer down. Nothing more can be written.
    Cancelled,
    /// [`Session::close`], [`Session::abort`], or a drop. The control connection may still be live,
    /// so a `GOODBYE` remains possible — `abort` documents exactly that follow-up.
    Local,
}

pub(crate) struct SessionLifecycle {
    pub(crate) closed: AtomicBool,
    pub(crate) diagnostic: Mutex<Option<String>>,
    /// Set with `diagnostic`, under the same first-close-wins rule: the first cause is the real one,
    /// and a later local close is only the caller catching up to it.
    pub(crate) cause: Mutex<Option<CloseCause>>,
    pub(crate) track_flows: Mutex<Vec<Weak<FlowSync>>>,
    pub(crate) input_lanes: Mutex<Vec<Weak<PendingInput>>>,
}

impl SessionLifecycle {
    pub(crate) fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            diagnostic: Mutex::new(None),
            cause: Mutex::new(None),
            track_flows: Mutex::new(Vec::new()),
            input_lanes: Mutex::new(Vec::new()),
        }
    }

    /// Why the connection ended, when it ended in a way that makes writing pointless.
    ///
    /// `Local` is deliberately not reported: it means this side closed the lifecycle while the
    /// control connection may still be usable.
    pub(crate) fn unwritable(&self) -> Option<(CloseCause, String)> {
        if !self.closed.load(Ordering::Acquire) {
            return None;
        }
        let cause = (*self.cause.lock().ok()?)?;
        if matches!(cause, CloseCause::Local) {
            return None;
        }
        let diagnostic = self
            .diagnostic
            .lock()
            .ok()
            .and_then(|value| value.clone())
            .unwrap_or_else(|| "Vivid session is closed".into());
        Some((cause, diagnostic))
    }

    pub(crate) fn ensure_active(&self) -> io::Result<()> {
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

    pub(crate) fn register_track_flow(&self, flow: &Arc<FlowSync>) -> io::Result<()> {
        self.ensure_active()?;
        let mut flows = lock(&self.track_flows, "session track registry")?;
        flows.retain(|pending| pending.strong_count() != 0);
        self.ensure_active()?;
        flows.push(Arc::downgrade(flow));
        Ok(())
    }

    pub(crate) fn register_input_lane(&self, lane: &Arc<PendingInput>) -> io::Result<()> {
        self.ensure_active()?;
        let mut lanes = lock(&self.input_lanes, "session input registry")?;
        lanes.retain(|pending| pending.strong_count() != 0);
        self.ensure_active()?;
        lanes.push(Arc::downgrade(lane));
        Ok(())
    }

    pub(crate) fn close(&self, cause: CloseCause, message: &str) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut diagnostic) = self.diagnostic.lock() {
            diagnostic.get_or_insert_with(|| message.to_owned());
        }
        if let Ok(mut stored) = self.cause.lock() {
            stored.get_or_insert(cause);
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

pub(crate) enum ControlPlane {
    Live {
        writer: ConnectionWriter,
        pending: Arc<PendingControl>,
    },
    Offline {
        connection: Mutex<Connection>,
    },
}

impl ControlPlane {
    pub(crate) fn request(
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

    pub(crate) fn take_event(&self) -> io::Result<Option<SessionEvent>> {
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
    pub(crate) control: ControlPlane,
    pub(crate) lifecycle: Arc<SessionLifecycle>,
    pub(crate) endpoints: Endpoints,
    pub(crate) connection_factory: Option<Arc<dyn ConnectionFactory>>,
    pub(crate) channel_key: Secret32,
    pub(crate) resume_key: Option<Secret32>,
    pub(crate) lease_identity: Option<(u64, u64)>,
    pub(crate) anchor_key: AnchorKey,
    pub(crate) info: SessionInfo,
    pub(crate) next_id: AtomicU64,
    pub(crate) next_request_id: Arc<AtomicU64>,
    pub(crate) surfaces: HashMap<(u64, u64), Arc<Mutex<SurfaceLocal>>>,
    pub(crate) tracks: Arc<Mutex<TrackRegistry>>,
    pub(crate) closed: bool,
    pub(crate) trace_dir: Option<PathBuf>,
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

/// One bounded establishment attempt. Retains identical authenticated HELLO bytes across
/// transport failures; never reuse it for a fresh logical attempt. Contains secrets, no Debug.
pub struct EstablishmentAttempt {
    config: ProducerConfig,
    prepared: Option<(Hello, Secret32, zeroize::Zeroizing<Vec<u8>>)>,
    deadline: std::time::Instant,
    carrier_key: Option<Secret32>,
    tried: bool,
}

impl EstablishmentAttempt {
    pub fn new(mut config: ProducerConfig, retry_timeout: std::time::Duration) -> io::Result<Self> {
        config.validate()?;
        if config.is_offline()
            || retry_timeout.is_zero()
            || retry_timeout > std::time::Duration::from_secs(300)
        {
            return Err(invalid_input(
                "live establishment retry timeout must be in (0, 300 seconds]",
            ));
        }
        let preface = vivid_protocol::wire::encode_preface(
            ConnectionKind::Control,
            vivid_protocol::CONTROL_MAX_RECORD_BODY,
        );
        let (hello, secret) = build_hello(&config, &preface)?;
        let body = zeroize::Zeroizing::new(hello.encode(1)?);
        // The prepared HELLO/secret now own the authentication data, not the retained options.
        if let ProducerAuthentication::LeaseActivation {
            proof_of_possession: Some(proof),
            ..
        } = &mut config.authentication
        {
            zeroize::Zeroize::zeroize(proof);
        }
        config.authentication = ProducerAuthentication::RootFromEnvironment;
        Ok(Self {
            config,
            prepared: Some((hello, secret, body)),
            deadline: std::time::Instant::now() + retry_timeout,
            carrier_key: None,
            tried: false,
        })
    }

    pub fn connect(&mut self) -> io::Result<Session> {
        self.connect_using(None)
    }

    pub fn connect_with_factory(
        &mut self,
        factory: Arc<dyn ConnectionFactory>,
    ) -> io::Result<Session> {
        self.connect_using(Some(factory))
    }

    fn connect_using(
        &mut self,
        factory: Option<Arc<dyn ConnectionFactory>>,
    ) -> io::Result<Session> {
        if std::time::Instant::now() >= self.deadline {
            self.prepared = None;
            self.carrier_key = None;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "establishment attempt expired",
            ));
        }
        let (hello, secret, body) = self
            .prepared
            .as_ref()
            .ok_or_else(|| invalid_input("establishment attempt already completed"))?;
        if self.tried && matches!(hello.authentication, HelloAuthentication::Root { .. }) {
            return Err(invalid_input(
                "root authentication requires a fresh attempt",
            ));
        }
        let binding = Secret32::new(
            factory
                .as_ref()
                .map_or([0; 32], |factory| factory.carrier_binding_key()),
        );
        if self
            .carrier_key
            .as_ref()
            .is_some_and(|previous| !auth::verify_proof(previous.expose(), binding.expose()))
        {
            return Err(invalid_input("establishment retry changed carrier binding"));
        }
        self.carrier_key = Some(binding);
        self.tried = true;
        let result = Session::connect_prepared(&self.config, factory, hello, secret, body);
        if result.is_ok() {
            self.prepared = None;
            self.carrier_key = None;
        }
        result
    }
}

impl Session {
    /// A cloneable unclean shutdown operation usable while another thread owns this session.
    /// Carrier factories must cancel their custom blocking I/O before invoking this operation.
    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let lifecycle = self.lifecycle.clone();
        let writer = match &self.control {
            ControlPlane::Live { writer, .. } => Some(writer.clone()),
            ControlPlane::Offline { .. } => None,
        };
        Arc::new(move || {
            lifecycle.close(CloseCause::Cancelled, "Vivid session cancelled");
            if let Some(writer) = &writer {
                let _ = writer.shutdown();
            }
        })
    }

    pub fn connect(config: ProducerConfig) -> io::Result<Self> {
        config.validate()?;
        if config.is_offline() {
            return Self::connect_offline(config);
        }
        Self::connect_live(config, None)
    }

    /// Connect through a caller-provided transport factory.
    ///
    /// This supports authenticated carriers such as a WebSocket route without weakening or
    /// duplicating the Vivid 1.5 protocol handshake.
    pub fn connect_with_factory(
        config: ProducerConfig,
        connection_factory: Arc<dyn ConnectionFactory>,
    ) -> io::Result<Self> {
        config.validate()?;
        if config.is_offline() {
            return Err(invalid_input(
                "a connection factory cannot be combined with dry-run or trace mode",
            ));
        }
        Self::connect_live(config, Some(connection_factory))
    }

    pub(crate) fn connect_live(
        config: ProducerConfig,
        connection_factory: Option<Arc<dyn ConnectionFactory>>,
    ) -> io::Result<Self> {
        EstablishmentAttempt::new(config, std::time::Duration::from_secs(30))?
            .connect_using(connection_factory)
    }

    fn connect_prepared(
        config: &ProducerConfig,
        connection_factory: Option<Arc<dyn ConnectionFactory>>,
        hello: &Hello,
        session_secret: &Secret32,
        hello_body: &[u8],
    ) -> io::Result<Self> {
        let control_endpoint = if connection_factory.is_some() {
            optional_endpoint(
                config.endpoint_control.as_deref(),
                vivid_protocol::discovery::ENDPOINT_CONTROL,
            )?
        } else {
            Some(endpoint(
                config.endpoint_control.as_deref(),
                vivid_protocol::discovery::ENDPOINT_CONTROL,
            )?)
        };
        let interactive = optional_endpoint(
            config.endpoint_interactive.as_deref(),
            vivid_protocol::discovery::ENDPOINT_INTERACTIVE,
        )?
        .or_else(|| control_endpoint.clone());
        let bulk = optional_endpoint(
            config.endpoint_bulk.as_deref(),
            vivid_protocol::discovery::ENDPOINT_BULK,
        )?
        .or_else(|| control_endpoint.clone());
        let realtime = optional_endpoint(
            config.endpoint_realtime.as_deref(),
            vivid_protocol::discovery::ENDPOINT_REALTIME,
        )?
        .or_else(|| bulk.clone());

        let carrier_binding_key = connection_factory
            .as_ref()
            .map_or(CARRIER_BINDING_NONE, |factory| {
                factory.carrier_binding_key()
            });
        let mut connection = match &connection_factory {
            Some(factory) => factory.open(ConnectionKind::Control, None)?,
            None => Connection::open(
                control_endpoint
                    .as_ref()
                    .ok_or_else(|| invalid_input("missing Vivid control endpoint"))?,
                ConnectionKind::Control,
            )?,
        };
        connection.write_record(messages::HELLO, 0, 0, hello_body)?;
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
            session_secret,
            &hello.client_nonce,
            &welcome.server_nonce,
            &carrier_binding_key,
        );
        let unconfirmed = zeroize::Zeroizing::new(welcome.unconfirmed_payload()?);
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
        let lease_identity = hello_lease_identity(hello);
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
            connection_factory,
            channel_key,
            resume_key,
            lease_identity,
            anchor_key,
            info,
            next_id: AtomicU64::new(1),
            next_request_id: Arc::new(AtomicU64::new(2)),
            surfaces: HashMap::new(),
            tracks,
            closed: false,
            trace_dir: None,
        })
    }

    pub(crate) fn connect_offline(config: ProducerConfig) -> io::Result<Self> {
        let lease_identity = producer_lease_identity(&config.authentication);
        let resumable = lease_identity.is_some();
        let mut connection = match &config.trace_dir {
            Some(directory) => {
                Connection::trace(&directory.join("control.ndjson"), ConnectionKind::Control)?
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
            target_descriptor: offline_target_descriptor(&config.target_profile),
            target_profile: config.target_profile,
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
                interactive: Some(offline_endpoint()?),
                realtime: Some(offline_endpoint()?),
                bulk: Some(offline_endpoint()?),
            },
            connection_factory: None,
            channel_key,
            resume_key,
            lease_identity,
            anchor_key,
            info,
            next_id: AtomicU64::new(1),
            next_request_id: Arc::new(AtomicU64::new(2)),
            surfaces: HashMap::new(),
            tracks: Arc::new(Mutex::new(HashMap::new())),
            closed: false,
            trace_dir: config.trace_dir,
        })
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// The session channel key, which authenticates lane and track opens.
    ///
    /// Exposed for conformance harnesses that drive a lane or channel at the wire. It is session
    /// key material: it must not be logged, serialized, or placed in a command argument.
    pub fn channel_key(&self) -> Secret32 {
        Secret32::new(*self.channel_key.expose())
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

    /// Close the session lifecycle without a `GOODBYE` round trip.
    ///
    /// Wakes every channel-flow wait with a closed error so senders blocked on
    /// a stalled presenter can exit; the quit path calls this before joining
    /// media worker threads. The session may still be closed normally
    /// afterwards, which sends the `GOODBYE` over the live control connection.
    pub fn abort(&mut self) -> io::Result<()> {
        self.lifecycle
            .close(CloseCause::Local, "Vivid session aborted");
        Ok(())
    }

    pub fn close(mut self) -> io::Result<()> {
        self.close_inner()
    }

    /// Why the control connection can no longer be written to, if it cannot.
    pub(crate) fn connection_ended(&self) -> Option<io::Error> {
        self.lifecycle.unwritable().map(|(cause, diagnostic)| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                match cause {
                    CloseCause::Cancelled => {
                        format!("Vivid session was cancelled before it was closed: {diagnostic}")
                    }
                    _ => format!("Vivid connection ended before it was closed: {diagnostic}"),
                },
            )
        })
    }

    pub(crate) fn close_inner(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // A connection the reader has already seen end, or one `cancel_handle` shut down, cannot
        // carry a `GOODBYE`. Attempting it anyway answers with the writer's generic "writer is
        // closed", which is the least informative thing this stack knows: the reason the reader
        // recorded is right here. Local teardown below still runs — the close is complete, and
        // only the report of it changes.
        let ended = self.connection_ended();
        self.lifecycle
            .close(CloseCause::Local, "Vivid session closed");
        let result = match ended {
            Some(error) => Err(error),
            None => self.request_ok(messages::GOODBYE, 0, vec![], &RequestMetadata::default()),
        };
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

    pub(crate) fn request_ok(
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

    pub(crate) fn dispatch_ok(
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

    pub(crate) fn request(
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
                .request(request_id, record_type, object_id, &envelope.encode()?)
                .map_err(|error| io::Error::new(error.kind(), format!(
                    "control request {request_id} type {record_type:#06x} object {object_id}: {error}"
                )))?;
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

    pub(crate) fn next_request(&self) -> io::Result<u64> {
        self.next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| invalid_data("request ID space exhausted"))
    }

    pub(crate) fn advance_allocator_past(&self, adopted_id: u64) -> io::Result<()> {
        let next = adopted_id
            .checked_add(1)
            .ok_or_else(|| invalid_data("adopted object ID exhausts the SDK allocator"))?;
        self.next_id.fetch_max(next, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let ControlPlane::Live { writer, .. } = &self.control {
            let _ = writer.shutdown();
        }
        if !self.closed {
            // Dropping is cancellation/unclean loss, not a clean GOODBYE. This is intentional:
            // resumable sessions must be allowed to suspend rather than being silently destroyed.
            self.lifecycle
                .close(CloseCause::Local, "Vivid control session dropped");
            self.surfaces.clear();
            if let Ok(mut tracks) = self.tracks.lock() {
                tracks.clear();
            }
        }
    }
}

pub(crate) fn spawn_control_reader(
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
                    if !supported_control_record(record.record_type) {
                        if record.flags & vivid_protocol::wire::RECORD_OPTIONAL != 0 {
                            continue;
                        }
                        if let Ok(envelope) = messages::decode_control(&record.body) {
                            let error = messages::ErrorReply {
                                code: messages::ERROR_UNSUPPORTED_PROFILE,
                                request_id: envelope.request_id,
                                detail: messages::ErrorDetail::new(vec![])?,
                                fatal: true,
                                diagnostic: "unsupported required control record".into(),
                            };
                            let _ = writer.write_record(
                                messages::ERROR,
                                0,
                                record.object_id,
                                &error.encode()?,
                            );
                        }
                        return Err(invalid_data("unsupported required control record"));
                    }
                    let envelope = messages::decode_control(&record.body)?;
                    let fatal = if record.record_type == messages::ERROR {
                        messages::parse_error_reply(&record.body)?.fatal
                    } else {
                        false
                    };
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
                        if fatal {
                            return Err(invalid_data("presenter sent a fatal control error"));
                        }
                        continue;
                    }
                    if fatal {
                        return Err(invalid_data("presenter sent a fatal control error"));
                    }
                    if record.record_type == messages::TRACK_LOST {
                        apply_track_lost(record.object_id, &envelope.payload, &tracks)?;
                    }
                    let event =
                        session_event(record.record_type, record.object_id, envelope.payload)?;
                    let mut events = lock(&pending.events, "control event queue")?;
                    if events.len() == MAX_CONTROL_EVENTS {
                        return Err(invalid_data("control event queue exceeded its bound"));
                    }
                    events.push_back(event);
                }
            })();
            let message = result.err().map_or_else(
                || "control connection closed".into(),
                |error| error.to_string(),
            );
            // Recorded before the writer is shut down. The other order leaves a window in which a
            // concurrent send fails with the writer's generic "writer is closed" while the reason
            // for it is a moment away from being stored.
            lifecycle.close(CloseCause::Lost, &message);
            let _ = writer.shutdown();
            pending.closed.store(true, Ordering::Release);
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

pub(crate) fn apply_track_lost(
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
    let detail = ErrorDetail::new(required_map(payload, 5)?.to_vec()).map_err(io::Error::other)?;
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
        state.lost = Some(TrackLostError {
            code: error_code,
            detail,
            diagnostic: diagnostic.to_owned(),
        });
        state.active_media = None;
        let active_flow = state.active_flow.take();
        close_track_flow(active_flow.as_ref(), diagnostic);
    }
    Ok(())
}

// Assigned control records understood by the SDK; unknown OPTIONAL bodies are opaque.
fn supported_control_record(kind: u16) -> bool {
    use vivid_protocol::registry::record::*;
    matches!(
        kind,
        HELLO
            | WELCOME
            | OK
            | ERROR
            | PING
            | PONG
            | GOODBYE
            | QUERY_SESSION
            | SESSION_STATUS
            | LANE_OPEN
            | LANE_ACCEPTED
            | TARGET_CHANGED
            | CAPS_CHANGED
            | SET_OBSERVATION
            | OBSERVATION_GAP
            | CREATE_SURFACE
            | SURFACE_READY
            | UPDATE_SURFACE
            | DESTROY_SURFACE
            | QUERY_SURFACE
            | SURFACE_STATUS
            | SURFACE_CHANGED
            | PROBE_TRACK_CONFIG
            | TRACK_SUPPORT
            | CREATE_TRACK
            | TRACK_READY
            | DESTROY_TRACK
            | TRACK_LOST
            | ACTIVATE_TRACK
            | TRACK_ACTIVATED
            | ADVANCE_CHANNEL
            | CHANNEL_ADVANCED
            | QUERY_TRACK
            | TRACK_STATUS
            | WAIT_TRACK
            | WAIT_SATISFIED
            | CANCEL_WAIT
            | TRACK_CHANGED
            | BEGIN_TXN
            | CREATE_NODE
            | UPDATE_NODE
            | DELETE_NODE
            | COMMIT_TXN
            | ABORT_TXN
            | SCENE_PRESENTED
            | QUERY_SCENE
            | SCENE_STATUS
            | SCENE_CHANGED
            | ANCHOR_READY
            | ANCHOR_GONE
            | QUERY_ANCHOR
            | ANCHOR_STATUS
            | PLAY
            | PAUSE
            | FLUSH
            | DRAIN
            | PLAYBACK_STATE
            | SET_AUDIO_GAIN
            | CREATE_CONTEXT
            | CONTEXT_READY
            | REVOKE_CONTEXT
            | CONTEXT_CHANGED
            | CREATE_SESSION_LEASE
            | SESSION_LEASE_READY
            | REVOKE_SESSION_LEASE
            | SESSION_LEASE_CHANGED
            | SET_FILE_DROP_BINDING
            | FILE_DROP_BOUND
            | FILE_DROP_OFFER
            | ACCEPT_FILE_DROP
            | FILE_DROP_ACCEPTED
            | CANCEL_FILE_DROP
            | FILE_DROP_CANCELLED
            | ADVANCE_FILE_TRANSFER
            | FILE_TRANSFER_ADVANCED
            | QUERY_FILE_DROP
            | FILE_DROP_STATUS
    )
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use std::time::{Duration, Instant};
    use vivid_protocol::cbor::Value;

    fn dispatch_fixture(kind: u16, flags: u16, body: Vec<u8>) -> Result<Record, String> {
        use std::io::Cursor;
        use vivid_protocol::wire::RecordHeader;
        let mut input = Vec::new();
        for (index, (record_type, flags, body)) in [
            (messages::WELCOME, 0, vec![]),
            (kind, flags, body),
            (messages::PONG, 0, messages::empty(7)),
        ]
        .into_iter()
        .enumerate()
        {
            input.extend_from_slice(
                &RecordHeader {
                    body_length: body.len() as u32,
                    record_type,
                    flags,
                    object_id: 0,
                    sequence: index as u64 + 1,
                }
                .encode(),
            );
            input.extend_from_slice(&body);
        }
        let mut connection = Connection::from_streams(
            Box::new(Cursor::new(input)),
            Box::new(std::io::sink()),
            ConnectionKind::Control,
        )
        .unwrap();
        connection.read_record().unwrap();
        let (reader, writer) = connection.split().unwrap();
        let (send, receive) = mpsc::channel();
        let pending = Arc::new(PendingControl {
            requests: Mutex::new(HashMap::from([(7, send)])),
            events: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        spawn_control_reader(
            reader,
            writer,
            pending,
            Arc::new(SessionLifecycle::new()),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();
        receive.recv_timeout(Duration::from_secs(1)).unwrap()
    }
    /// The reader is the only thing that knows why a connection ended, and the writer's generic
    /// "writer is closed" is what a caller gets instead whenever that knowledge is not recorded
    /// first. Recording it before the shutdown is what removes the window between the two.
    #[test]
    fn the_reader_records_why_the_connection_ended_before_it_shuts_the_writer() {
        use std::io::Cursor;
        use vivid_protocol::wire::RecordHeader;
        let mut input = RecordHeader {
            body_length: 0,
            record_type: messages::WELCOME,
            flags: 0,
            object_id: 0,
            sequence: 1,
        }
        .encode()
        .to_vec();
        // A record header cut short mid-stream, which is what a peer that goes away leaves behind.
        input.extend_from_slice(&[0, 0, 0]);
        let mut connection = Connection::from_streams(
            Box::new(Cursor::new(input)),
            Box::new(std::io::sink()),
            ConnectionKind::Control,
        )
        .unwrap();
        connection.read_record().unwrap();
        let (reader, writer) = connection.split().unwrap();
        let lifecycle = Arc::new(SessionLifecycle::new());
        let pending = Arc::new(PendingControl {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        spawn_control_reader(
            reader,
            writer.clone(),
            pending.clone(),
            lifecycle.clone(),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !pending.closed.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "the reader never finished");
            thread::sleep(Duration::from_millis(10));
        }

        // The writer is unusable by now, so a caller reaching it gets the generic error. The point
        // is that it no longer has to: the cause and the reader's own words are already stored.
        assert!(writer.write_record(messages::GOODBYE, 0, 0, &[]).is_err());
        let (cause, diagnostic) = lifecycle.unwritable().expect("no reason was recorded");
        assert_eq!(cause, CloseCause::Lost);
        assert!(
            !diagnostic.contains("Vivid connection writer is closed"),
            "the recorded reason is the writer's own error, not the reader's: {diagnostic}"
        );
        assert!(!diagnostic.is_empty());
    }

    #[test]
    fn optional_opaque_records_do_not_consume_requests() {
        assert_eq!(
            dispatch_fixture(0x6fff, vivid_protocol::wire::RECORD_OPTIONAL, vec![0xff])
                .unwrap()
                .record_type,
            messages::PONG
        );
        assert!(dispatch_fixture(0x6fff, 0, vec![0xff]).is_err());
    }
    #[test]
    fn every_unsolicited_error_is_validated_and_fatal_closes() {
        let valid = messages::ErrorReply {
            code: messages::ERROR_BAD_MESSAGE,
            request_id: 0,
            detail: messages::ErrorDetail::new(vec![]).unwrap(),
            fatal: false,
            diagnostic: String::new(),
        };
        assert!(dispatch_fixture(messages::ERROR, 0, valid.encode().unwrap()).is_ok());
        let fatal = messages::ErrorReply {
            fatal: true,
            ..valid
        };
        assert!(dispatch_fixture(messages::ERROR, 0, fatal.encode().unwrap()).is_err());
        for (code, detail) in [
            (0, vec![]),
            (messages::ERROR_BAD_MESSAGE, vec![(10, Value::Unsigned(1))]),
        ] {
            let body = messages::encode_payload(
                0,
                vec![
                    (0, Value::Unsigned(code)),
                    (1, Value::Unsigned(0)),
                    (2, Value::Map(detail)),
                    (3, Value::Bool(false)),
                    (4, Value::Text(String::new())),
                ],
            )
            .unwrap();
            assert!(dispatch_fixture(messages::ERROR, 0, body).is_err());
        }
    }
    #[test]
    fn expired_attempt_releases_owned_authentication() {
        let config = ProducerConfig {
            authentication: ProducerAuthentication::Root {
                root_secret: Secret32::new([1; 32]),
            },
            ..ProducerConfig::default()
        };
        let mut attempt = EstablishmentAttempt::new(config, Duration::from_secs(1)).unwrap();
        assert!(matches!(
            attempt.config.authentication,
            ProducerAuthentication::RootFromEnvironment
        ));
        attempt.deadline = std::time::Instant::now();
        assert_eq!(
            attempt.connect().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(attempt.prepared.is_none());
        assert!(attempt.carrier_key.is_none());
    }
}
