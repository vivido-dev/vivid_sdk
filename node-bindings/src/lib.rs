//! Private napi-rs extension for the public `@vivido/vivid-sdk` TypeScript package.
//!
//! The mirror of `python-bindings/`: opaque handle classes with getters, verbs as methods, and no
//! protocol logic — constants, claim arithmetic, and image inspection all come from `vivid_sdk`.
//! Every blocking SDK call is exposed twice: an async method that runs the call on a worker thread
//! and never blocks the JS event loop, and a `Sync` twin for short scripts. That is the napi
//! analogue of the Python binding's `py.detach`, and it exists for the same reason the Python
//! binding has no callbacks: presenter and reader threads are pure Rust and never enter JavaScript.

#![allow(clippy::too_many_arguments)]

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use vivid_protocol::cbor::Value;
use vivid_protocol::context::ContextDefinition;
use vivid_protocol::file_drop::{
    AcceptFileDrop, AdvanceFileTransfer, CancelFileDrop, FileDropBinding, FileDropDestination,
    FileDropTuple, FileResult, QueryFileDrop,
};
use vivid_protocol::lease::CleanupPolicy;
use vivid_protocol::media::{AudioPacket, RasterDeltaOperation, VideoPacket};
use vivid_protocol::messages::{LaneClass, TrackKind};
use vivid_protocol::resource::{RESOURCE_COUNT, ResourceContract};
use vivid_protocol::scene::Fit;
use vivid_protocol::scene::SceneNode;
use vivid_protocol::track::{KindConfiguration, TrackConfiguration, TrackMode};
use vivid_sdk::presenter::{
    Binding, CaptureContent, MediaConfig, PresenterConfig, SocketListener, VirtualVivid,
};
use vivid_sdk::{
    AudioPacketData, CoordinateModel, DesktopSession as SdkDesktopSession, EncodedPacket,
    GENERIC_CONTENT, IncomingFileTransferEvent, IncomingFileTransferRequest, InputBinding,
    InputBindingStatus, InputLaneEvent, PaneImageOptions, PaneSession as SdkPaneSession,
    ProducerAuthentication, ProducerConfig, RequestMetadata, SendPressure, Session as SdkSession,
    SessionLeaseBuilder, SlotBinding, Surface as SdkSurface, SurfaceDefinition, SurfaceDescriptor,
    SurfaceRole, Track as SdkTrack, TrackChannel as SdkTrackChannel, TrackStatus,
    TrackWaitCondition, VideoPacketData,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The error class tag every SDK rejection carries as its message prefix.
///
/// napi's `Status` becomes the JS `code` property and is a fixed enum, so the semantic class — the
/// thing Python expresses as a `ClosedHandleError` subclass — rides as a `Class: message` prefix.
/// The TypeScript layer splits on the first `: ` and raises its own subclasses.
const CLOSED: &str = "ClosedHandle";
const INVALID: &str = "InvalidInput";
const VIVID: &str = "VividError";

fn tagged(class: &str, message: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, format!("{class}: {message}"))
}

fn io_error(error: io::Error) -> Error {
    let class = match error.kind() {
        io::ErrorKind::InvalidInput => INVALID,
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected => CLOSED,
        _ => VIVID,
    };
    tagged(class, error)
}

fn value_error(message: impl std::fmt::Display) -> Error {
    tagged(INVALID, message)
}

fn closed_session() -> Error {
    tagged(CLOSED, "session is closed")
}

fn closed_channel() -> Error {
    tagged(CLOSED, "track channel is closed")
}

fn closed_presenter() -> Error {
    tagged(CLOSED, "presenter is closed")
}

fn locked<'a, T>(guard: &'a Mutex<T>, what: &str) -> Result<MutexGuard<'a, T>> {
    guard
        .lock()
        .map_err(|_| tagged(VIVID, format!("{what} lock was poisoned")))
}

/// Run a blocking SDK call on the async runtime's blocking pool.
async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    spawn_blocking(work)
        .await
        .map_err(|join_error| tagged(VIVID, format!("worker failed: {join_error}")))?
}

fn duration_ms(timeout_ms: f64, name: &str) -> Result<Duration> {
    if !timeout_ms.is_finite() || timeout_ms < 0.0 {
        return Err(value_error(format!(
            "{name} must be finite and non-negative"
        )));
    }
    Ok(Duration::from_secs_f64(timeout_ms / 1000.0))
}

fn opt_u64(value: Option<f64>, name: &str) -> Result<u64> {
    match value {
        None | Some(0.0) => Ok(0),
        Some(value)
            if value.is_finite() && value.fract() == 0.0 && value <= 9_007_199_254_740_992.0 =>
        {
            Ok(value as u64)
        }
        Some(_) => Err(value_error(format!(
            "{name} must be a non-negative safe integer"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// One entry of the shared constant table.
#[napi(object)]
pub struct ConstantEntry {
    pub name: String,
    /// The value for profile constants, `null` for numeric ones.
    pub text: Option<String>,
    /// The value for numeric constants, `null` for profile names.
    pub number: Option<f64>,
}

/// The protocol constants every SDK exposes, from the Rust table that owns them.
///
/// The TypeScript layer iterates this at import time to build its namespace, so a value that
/// changes in `vivid_protocol` changes there too without anybody copying a number.
#[napi]
pub fn constant_table() -> Vec<ConstantEntry> {
    vivid_sdk::constant_table()
        .iter()
        .map(|(name, value)| ConstantEntry {
            name: (*name).to_owned(),
            text: value.as_text().map(str::to_owned),
            number: value.as_number().map(|value| value as f64),
        })
        .collect()
}

/// Inspect a complete PNG or JPEG image and return its encoding and dimensions.
#[napi]
pub fn probe_encoded_image(data: Buffer) -> Result<EncodedImageInfo> {
    let image = vivid_sdk::probe_encoded_image(&data).map_err(io_error)?;
    Ok(EncodedImageInfo {
        encoding: image.encoding as f64,
        width: image.width,
        height: image.height,
        encoded_length: image.encoded_length,
    })
}

#[napi(object)]
pub struct EncodedImageInfo {
    pub encoding: f64,
    pub width: u32,
    pub height: u32,
    pub encoded_length: u32,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct ConnectOptions {
    pub dry_run: Option<bool>,
    pub trace_dir: Option<String>,
    pub endpoint_control: Option<String>,
    pub endpoint_interactive: Option<String>,
    pub endpoint_realtime: Option<String>,
    pub endpoint_bulk: Option<String>,
    /// Root secret as hex. Prefer leaving this unset: the discovery environment is read on the
    /// Rust side and the value never crosses JavaScript.
    pub root_secret: Option<String>,
    pub producer_name: Option<String>,
    pub producer_version: Option<String>,
    pub target_profile: Option<String>,
    pub required_profiles: Option<Vec<String>>,
    pub optional_profiles: Option<Vec<String>>,
    /// Bypass every endpoint and connect in-process against the offline contract.
    pub offline: Option<bool>,
    /// The desktop producer profile set: `desktop-surface-v1` target, live media required.
    pub desktop: Option<bool>,
}

fn connect_config(options: ConnectOptions) -> Result<ProducerConfig> {
    let mut config = if options.offline.unwrap_or(false) {
        ProducerConfig::offline()
    } else if options.desktop.unwrap_or(false) {
        ProducerConfig::desktop()
    } else {
        ProducerConfig::default()
    };
    config.endpoint_control = options.endpoint_control;
    config.endpoint_interactive = options.endpoint_interactive;
    config.endpoint_realtime = options.endpoint_realtime;
    config.endpoint_bulk = options.endpoint_bulk;
    config.producer_name = options
        .producer_name
        .unwrap_or_else(|| "vivid-sdk-node".to_owned());
    config.producer_version = options
        .producer_version
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    // Only override the preset when the caller asked; `desktop` already set this.
    if let Some(target_profile) = options.target_profile {
        config.target_profile = target_profile;
    }
    // Only override the preset when the caller asked; `offline` already set this.
    if let Some(dry_run) = options.dry_run {
        config.dry_run = dry_run;
    }
    config.trace_dir = options.trace_dir.map(std::path::PathBuf::from);
    if let Some(secret) = options.root_secret {
        let secret = zeroize::Zeroizing::new(secret);
        config.authentication = ProducerAuthentication::root_hex(&secret).map_err(value_error)?;
    }
    if let Some(profiles) = options.required_profiles {
        config.required_profiles = profiles;
    }
    if let Some(profiles) = options.optional_profiles {
        config.optional_profiles = profiles;
    }
    Ok(config)
}

#[napi]
pub struct Session {
    inner: Arc<Mutex<Option<SdkSession>>>,
}

#[napi(object)]
pub struct SessionInfoPayload {
    pub session_id: f64,
    pub session_tag: String,
    pub root_context_id: f64,
    pub target_generation: f64,
    pub target_profile: String,
    pub accepted_profiles: Vec<String>,
    pub session_revision: f64,
    pub scene_revision: f64,
    pub establishment_state: f64,
    pub resume_generation: f64,
}

fn session_info(info: &vivid_sdk::SessionInfo) -> SessionInfoPayload {
    SessionInfoPayload {
        session_id: info.session_id as f64,
        session_tag: info
            .session_tag
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        root_context_id: info.root_context_id as f64,
        target_generation: info.target_generation.get() as f64,
        target_profile: info.target_profile.clone(),
        accepted_profiles: info.accepted_profiles.clone(),
        session_revision: info.session_revision as f64,
        scene_revision: info.scene_revision.get() as f64,
        establishment_state: info.establishment_state as f64,
        resume_generation: info.resume_generation as f64,
    }
}

/// One session event, flattened by `kind`; only the fields the variant carries are set.
#[napi(object)]
pub struct SessionEventPayload {
    pub kind: String,
    pub context_id: Option<f64>,
    pub anchor_id: Option<f64>,
    pub object_id: Option<f64>,
    pub record_type: Option<f64>,
    pub diagnostic: Option<String>,
    pub payload: Option<PayloadPayload>,
}

/// A control payload, carried as scalar entries plus raw deterministic-CBOR bytes for everything
/// a relay must preserve byte-for-byte.
#[napi(object)]
pub struct PayloadPayload {
    pub scalars: Vec<PayloadScalar>,
    pub raw: Vec<PayloadRaw>,
}

#[napi(object)]
pub struct PayloadScalar {
    pub key: f64,
    /// Unsigned value; `null` when the entry is text or raw bytes.
    pub unsigned: Option<f64>,
    /// Text value; `null` when the entry is unsigned or raw bytes.
    pub text: Option<String>,
}

#[napi(object)]
pub struct PayloadRaw {
    pub key: f64,
    pub value: Buffer,
}

pub(crate) fn payload_payload(
    entries: &vivid_protocol::messages::PayloadMap,
) -> Result<PayloadPayload> {
    use vivid_protocol::cbor::Value;
    let mut scalars = Vec::new();
    let mut raw = Vec::new();
    for (key, value) in entries {
        match value {
            Value::Unsigned(unsigned) => scalars.push(PayloadScalar {
                key: *key as f64,
                unsigned: Some(*unsigned as f64),
                text: None,
            }),
            Value::Text(text) => scalars.push(PayloadScalar {
                key: *key as f64,
                unsigned: None,
                text: Some(text.clone()),
            }),
            // Every other shape is carried as its own deterministic-CBOR encoding, so nothing a
            // relay must preserve byte-for-byte is lost at this boundary.
            other => push_raw(&mut raw, *key, other)?,
        }
    }
    Ok(PayloadPayload { scalars, raw })
}

fn push_raw(raw: &mut Vec<PayloadRaw>, key: u64, value: &Value) -> Result<()> {
    let encoded = vivid_protocol::cbor::encode(value).map_err(|error| {
        tagged(
            VIVID,
            format!("payload entry {key} failed to encode: {error}"),
        )
    })?;
    raw.push(PayloadRaw {
        key: key as f64,
        value: encoded.into(),
    });
    Ok(())
}

fn session_event(event: vivid_sdk::SessionEvent) -> Result<SessionEventPayload> {
    use vivid_sdk::SessionEvent as E;
    let mut out = SessionEventPayload {
        kind: String::new(),
        context_id: None,
        anchor_id: None,
        object_id: None,
        record_type: None,
        diagnostic: None,
        payload: None,
    };
    match event {
        E::TargetChanged(payload) => {
            out.kind = "targetChanged".into();
            out.payload = Some(payload_payload(&payload)?);
        }
        E::AnchorReady {
            context_id,
            anchor_id,
            payload,
        } => {
            out.kind = "anchorReady".into();
            out.context_id = Some(context_id as f64);
            out.anchor_id = Some(anchor_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        E::AnchorGone {
            context_id,
            anchor_id,
            payload,
        } => {
            out.kind = "anchorGone".into();
            out.context_id = Some(context_id as f64);
            out.anchor_id = Some(anchor_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        E::TrackLost { object_id, payload } => {
            out.kind = "trackLost".into();
            out.object_id = Some(object_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        E::ContextChanged { object_id, payload } => {
            out.kind = "contextChanged".into();
            out.object_id = Some(object_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        E::FileDropOffered(offer) => {
            out.kind = "fileDropOffered".into();
            out.object_id = Some(offer.binding.drop_id as f64);
        }
        E::FileDropCancelled(cancel) => {
            out.kind = "fileDropCancelled".into();
            out.object_id = Some(cancel.binding.drop_id as f64);
        }
        E::Other {
            record_type,
            object_id,
            payload,
        } => {
            out.kind = "other".into();
            out.record_type = Some(record_type as f64);
            out.object_id = Some(object_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        E::ConnectionClosed { diagnostic } => {
            out.kind = "connectionClosed".into();
            out.diagnostic = Some(diagnostic);
        }
    }
    Ok(out)
}

#[napi]
impl Session {
    /// End the session with a `GOODBYE` round trip.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let session = locked(&self.inner, "session")?
            .take()
            .ok_or_else(closed_session)?;
        blocking(move || session.close().map_err(io_error)).await
    }

    /// Close the lifecycle without a `GOODBYE` round trip.
    #[napi]
    pub fn abort(&self) -> Result<()> {
        let mut guard = locked(&self.inner, "session")?;
        guard
            .as_mut()
            .ok_or_else(closed_session)?
            .abort()
            .map_err(io_error)
    }

    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "session")?.is_none())
    }

    #[napi]
    pub fn info(&self) -> Result<SessionInfoPayload> {
        let guard = locked(&self.inner, "session")?;
        Ok(session_info(
            guard.as_ref().ok_or_else(closed_session)?.info(),
        ))
    }

    #[napi]
    pub fn supports(&self, profile: String) -> Result<bool> {
        let guard = locked(&self.inner, "session")?;
        Ok(guard
            .as_ref()
            .ok_or_else(closed_session)?
            .supports(&profile))
    }

    #[napi]
    pub fn allocate_id(&self) -> Result<f64> {
        let guard = locked(&self.inner, "session")?;
        let id = guard
            .as_ref()
            .ok_or_else(closed_session)?
            .allocate_id()
            .map_err(io_error)?;
        Ok(id as f64)
    }

    /// Take the next session event without waiting; `null` when the queue is empty.
    #[napi]
    pub fn take_event(&self) -> Result<Option<SessionEventPayload>> {
        let guard = locked(&self.inner, "session")?;
        guard
            .as_ref()
            .ok_or_else(closed_session)?
            .take_event()
            .map_err(io_error)?
            .map(session_event)
            .transpose()
    }

    /// Take the next session event, waiting up to `timeout_ms` on a worker thread.
    ///
    /// Returns `null` on timeout and once the session has delivered its final `connectionClosed`
    /// event, which is what ends an event loop.
    #[napi(ts_return_type = "Promise<SessionEventPayload | null>")]
    pub async fn wait_event(&self, timeout_ms: f64) -> Result<Option<SessionEventPayload>> {
        let timeout = duration_ms(timeout_ms, "timeoutMs")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .wait_event(timeout)
                .map_err(io_error)?
                .map(session_event)
                .transpose()
        })
        .await
    }

    // -- Surfaces ----------------------------------------------------------

    #[napi(ts_return_type = "Promise<Surface>")]
    pub async fn create_surface(&self, config: SurfaceConfig) -> Result<Surface> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let definition = surface_definition(&config, session)?;
            session
                .create_surface(definition, &RequestMetadata::default())
                .map(|inner| Surface { inner })
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn update_surface(&self, surface: &Surface, config: SurfaceConfig) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let definition = surface_definition(&config, session)?;
            session
                .update_surface(&handle, definition, &RequestMetadata::default())
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn destroy_surface(&self, surface: &Surface) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .destroy_surface(&handle, &RequestMetadata::default())
                .map_err(io_error)
        })
        .await
    }

    // -- Tracks ------------------------------------------------------------

    #[napi(ts_return_type = "Promise<Track>")]
    pub async fn create_track(&self, surface: &Surface, config: TrackConfig) -> Result<Track> {
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let configuration =
                track_configuration(&config, session, handle.context_id(), handle.id())?;
            session
                .create_track(configuration, &RequestMetadata::default())
                .map(|inner| Track { inner })
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn destroy_track(&self, track: &Track) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .destroy_track(&handle, &RequestMetadata::default())
                .map_err(io_error)
        })
        .await
    }

    /// Wait for a track condition on a worker thread. `timeout_ms` defaults to the protocol's
    /// 30 s bound.
    #[napi(ts_return_type = "Promise<WaitSatisfiedPayload>")]
    pub async fn wait_track(
        &self,
        track: &Track,
        condition: f64,
        value: Option<f64>,
        timeout_ms: Option<f64>,
    ) -> Result<WaitSatisfiedPayload> {
        let condition = TrackWaitCondition::try_from(opt_u64(Some(condition), "condition")?)
            .map_err(|error| value_error(error.to_string()))?;
        let value = opt_u64(value, "value")?;
        let timeout = duration_ms(timeout_ms.unwrap_or(30_000.0), "timeoutMs")?;
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let result = session
                .wait_track(
                    &handle,
                    condition,
                    (value > 0).then_some(value),
                    timeout.as_micros() as u64,
                )
                .map_err(io_error)?;
            Ok(WaitSatisfiedPayload {
                context_id: result.context_id as f64,
                surface_id: result.surface_id as f64,
                track_id: result.track_id as f64,
                revision: result.revision.get() as f64,
                channel_generation: result.channel_generation.get() as f64,
                condition: result.condition as u64 as f64,
                observed_value: result.observed_value.unwrap_or(0) as f64,
            })
        })
        .await
    }

    /// Activate this track into its configured slot at a compositor boundary.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn activate_track(
        &self,
        surface: &Surface,
        track: &Track,
        required_milestone: Option<f64>,
    ) -> Result<f64> {
        let handle = track.inner.clone();
        let configuration = handle.configuration().map_err(io_error)?;
        let binding = SlotBinding {
            slot: configuration.slot,
            track_id: handle.id(),
            expected_channel_generation: handle.channel_generation(),
            required_milestone: required_milestone
                .map(|value| opt_u64(Some(value), "requiredMilestone"))
                .transpose()?
                .unwrap_or(1 << 4),
        };
        let inner = Arc::clone(&self.inner);
        let surface_handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            session
                .activate_tracks(&surface_handle, &[binding], &RequestMetadata::default())
                .map(|presentation| presentation as f64)
                .map_err(io_error)
        })
        .await
    }

    // -- Channels ----------------------------------------------------------

    #[napi(ts_return_type = "Promise<TrackChannel>")]
    pub async fn open_track_channel(&self, track: &Track) -> Result<TrackChannel> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let channel = session.open_track_channel(&handle).map_err(io_error)?;
            Ok(TrackChannel {
                context_id: handle.context_id(),
                surface_id: handle.surface_id(),
                track_id: handle.id(),
                kind: kind_name(handle.kind()),
                generation: channel.generation().get(),
                inner: Arc::new(Mutex::new(Some(channel))),
            })
        })
        .await
    }

    // -- Scene and anchors ---------------------------------------------------

    /// Place a surface in the terminal grid. Coordinates are signed fixed-point cells encoded as
    /// `value * 2^32`, matching `placeTerminalSurface` in the Python package.
    #[napi(ts_return_type = "Promise<SceneCommitPayload>")]
    pub async fn place_terminal_surface(
        &self,
        surface: &Surface,
        node_id: f64,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text_layer: Option<f64>,
    ) -> Result<SceneCommitPayload> {
        let node_id = opt_u64(Some(node_id), "nodeId")?;
        let x = fixed_of(x, "x")?;
        let y = fixed_of(y, "y")?;
        let width = fixed_of(width, "width")?;
        let height = fixed_of(height, "height")?;
        let text_layer = opt_u64(text_layer, "textLayer").map(|value| value.max(1))?;
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let result = session
                .place_terminal_surface(&handle, node_id, x, y, width, height, text_layer)
                .map_err(io_error)?;
            Ok(SceneCommitPayload {
                scene_revision: result.scene_revision.get() as f64,
                target_generation: result.target_generation.get() as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<SceneCommitPayload>")]
    pub async fn delete_node(&self, context_id: f64, node_id: f64) -> Result<SceneCommitPayload> {
        let context_id = opt_u64(Some(context_id), "contextId")?;
        let node_id = opt_u64(Some(node_id), "nodeId")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let result = session
                .delete_node(context_id, node_id, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(SceneCommitPayload {
                scene_revision: result.scene_revision.get() as f64,
                target_generation: result.target_generation.get() as f64,
            })
        })
        .await
    }

    #[napi]
    pub fn anchor_marker(&self, context_id: f64, anchor_id: f64) -> Result<String> {
        let context_id = opt_u64(Some(context_id), "contextId")?;
        let anchor_id = opt_u64(Some(anchor_id), "anchorId")?;
        let guard = locked(&self.inner, "session")?;
        guard
            .as_ref()
            .ok_or_else(closed_session)?
            .anchor_marker(context_id, anchor_id)
            .map_err(io_error)
    }
}

/// Connect to a presenter. Blocking work runs on a worker thread.
#[napi]
pub async fn connect(config: Option<ConnectOptions>) -> Result<Session> {
    let options = config.unwrap_or(ConnectOptions {
        dry_run: None,
        trace_dir: None,
        endpoint_control: None,
        endpoint_interactive: None,
        endpoint_realtime: None,
        endpoint_bulk: None,
        root_secret: None,
        producer_name: None,
        producer_version: None,
        target_profile: None,
        required_profiles: None,
        optional_profiles: None,
        offline: None,
        desktop: None,
    });
    let config = connect_config(options)?;
    blocking(move || SdkSession::connect(config).map_err(io_error))
        .await
        .map(|inner| Session {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
}

#[napi(object)]
pub struct SceneCommitPayload {
    pub scene_revision: f64,
    pub target_generation: f64,
}

fn fixed_of(value: f64, name: &str) -> Result<i64> {
    // The wire's signed fixed-point: cells scaled by 2^32, exactly what
    // `placeTerminalSurface(width=2 << 32, ...)` passes in Python.
    const FIXED_ONE: f64 = 4_294_967_296.0;
    match value {
        value if value.is_finite() => {
            let scaled = value * FIXED_ONE;
            if scaled >= i64::MIN as f64 && scaled <= i64::MAX as f64 {
                Ok(scaled as i64)
            } else {
                Err(value_error(format!(
                    "{name} is outside the fixed-point cell range"
                )))
            }
        }
        _ => Err(value_error(format!("{name} must be finite"))),
    }
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// One captured output in a desktop surface topology.
#[napi(object)]
pub struct OutputSpec {
    pub output_id: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub width: f64,
    pub height: f64,
    pub scale_numerator: f64,
    pub scale_denominator: f64,
    /// Protocol rotation code: 0 none, 1 90, 2 180, 3 270.
    pub rotation: f64,
    pub primary: bool,
}

/// Typed desktop parameters; present they make the surface a desktop surface.
#[napi(object)]
pub struct DesktopParametersSpec {
    pub captured_origin_x: f64,
    pub captured_origin_y: f64,
    pub topology: Vec<OutputSpec>,
    pub semantic_generation: f64,
    pub input_capabilities: f64,
}

/// A surface configuration, in the same shape the Python package's `SurfaceConfig` dataclass maps to.
#[napi(object)]
pub struct SurfaceConfig {
    pub context_id: Option<f64>,
    pub surface_id: Option<f64>,
    pub semantic_profile: Option<String>,
    pub coordinate_model: Option<f64>,
    pub logical_width: f64,
    pub logical_height: f64,
    pub scale_numerator: Option<f64>,
    pub scale_denominator: Option<f64>,
    pub rotation: Option<f64>,
    pub role: Option<f64>,
    pub title: Option<String>,
    pub semantic_content_revision: Option<f64>,
    pub semantic_availability: Option<f64>,
    pub locator_hint: Option<String>,
    pub policy: Option<f64>,
    /// Typed desktop parameters, required for the `desktop-content-v1` profile.
    pub desktop_parameters: Option<DesktopParametersSpec>,
}

fn surface_definition(config: &SurfaceConfig, session: &SdkSession) -> Result<SurfaceDefinition> {
    let (context_id, surface_id) = match (config.context_id, config.surface_id) {
        (Some(context_id), Some(surface_id)) => (
            opt_u64(Some(context_id), "contextId")?,
            opt_u64(Some(surface_id), "surfaceId")?,
        ),
        (context_id, surface_id) => (
            opt_u64(context_id, "contextId")
                .map(|value| {
                    if value == 0 {
                        session.info().root_context_id
                    } else {
                        value
                    }
                })
                .unwrap_or(session.info().root_context_id),
            match opt_u64(surface_id, "surfaceId")? {
                0 => session.allocate_id().map_err(io_error)?,
                value => value,
            },
        ),
    };
    let coordinate_model = match config.coordinate_model {
        None => CoordinateModel::DesktopLogicalPixels,
        Some(value) => CoordinateModel::try_from(opt_u64(Some(value), "coordinateModel")?)
            .map_err(|error| value_error(error.to_string()))?,
    };
    let role = match config.role {
        None => SurfaceRole::Unspecified,
        Some(value) => SurfaceRole::try_from(opt_u64(Some(value), "role")?)
            .map_err(|error| value_error(error.to_string()))?,
    };
    Ok(SurfaceDefinition {
        context_id,
        surface_id,
        semantic_profile: config
            .semantic_profile
            .clone()
            .unwrap_or_else(|| GENERIC_CONTENT.to_owned()),
        coordinate_model,
        logical_width: opt_u64(Some(config.logical_width), "logicalWidth")?,
        logical_height: opt_u64(Some(config.logical_height), "logicalHeight")?,
        scale_numerator: opt_u64(config.scale_numerator, "scaleNumerator")?.max(1),
        scale_denominator: opt_u64(config.scale_denominator, "scaleDenominator")?.max(1),
        rotation: opt_u64(config.rotation, "rotation")? as u16,
        descriptor: SurfaceDescriptor {
            role,
            title: config.title.clone().unwrap_or_default(),
            semantic_content_revision: opt_u64(
                config.semantic_content_revision,
                "semanticContentRevision",
            )?,
            semantic_availability: opt_u64(config.semantic_availability, "semanticAvailability")?,
            locator_hint: config.locator_hint.clone().unwrap_or_default(),
        },
        policy: opt_u64(config.policy, "policy")?,
        profile_parameters: match &config.desktop_parameters {
            Some(parameters) => vivid_protocol::surface::DesktopSurfaceParameters {
                captured_origin_x: parameters.captured_origin_x as i32,
                captured_origin_y: parameters.captured_origin_y as i32,
                topology: parameters
                    .topology
                    .iter()
                    .map(
                        |output| -> Result<vivid_protocol::target::OutputDescriptor> {
                            Ok(vivid_protocol::target::OutputDescriptor {
                                output_id: opt_u64(Some(output.output_id), "outputId")?,
                                origin_x: output.origin_x as i32,
                                origin_y: output.origin_y as i32,
                                width: opt_u64(Some(output.width), "width")? as u32,
                                height: opt_u64(Some(output.height), "height")? as u32,
                                scale_numerator: opt_u64(
                                    Some(output.scale_numerator),
                                    "scaleNumerator",
                                )? as u32,
                                scale_denominator: opt_u64(
                                    Some(output.scale_denominator),
                                    "scaleDenominator",
                                )? as u32,
                                rotation: vivid_protocol::geometry::Rotation::try_from(opt_u64(
                                    Some(output.rotation),
                                    "rotation",
                                )?)
                                .map_err(|error| value_error(error.to_string()))?,
                                primary: output.primary,
                            })
                        },
                    )
                    .collect::<Result<Vec<_>>>()?,
                semantic_generation: opt_u64(
                    Some(parameters.semantic_generation),
                    "semanticGeneration",
                )?,
                input_capabilities: opt_u64(
                    Some(parameters.input_capabilities),
                    "inputCapabilities",
                )?,
            }
            .encode(),
            None => Vec::new(),
        },
    })
}

#[napi]
pub struct Surface {
    pub(crate) inner: SdkSurface,
}

#[napi]
impl Surface {
    #[napi(getter)]
    pub fn context_id(&self) -> f64 {
        self.inner.context_id() as f64
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id() as f64
    }

    #[napi(getter)]
    pub fn revision(&self) -> f64 {
        self.inner.revision().get() as f64
    }

    #[napi(getter)]
    pub fn generation(&self) -> f64 {
        self.inner.generation().get() as f64
    }
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// A track configuration; which kind fields matter is decided by `kind`.
#[napi(object)]
pub struct TrackConfig {
    pub kind: String,
    // video / raster / image geometry
    pub width: Option<f64>,
    pub height: Option<f64>,
    // video / audio codec identity
    pub codec: Option<String>,
    pub packetization: Option<String>,
    pub extradata: Option<Buffer>,
    pub sample_rate: Option<f64>,
    pub channels: Option<f64>,
    pub channel_mask: Option<f64>,
    // video
    pub profile: Option<f64>,
    pub level: Option<f64>,
    pub maximum_reorder_depth: Option<f64>,
    pub color_primaries: Option<f64>,
    pub transfer: Option<f64>,
    pub matrix: Option<f64>,
    pub signal_range: Option<f64>,
    pub aspect_numerator: Option<f64>,
    pub aspect_denominator: Option<f64>,
    pub maximum_access_unit_bytes: Option<f64>,
    pub codec_string: Option<String>,
    pub decoder_configuration: Option<Buffer>,
    // raster
    pub alpha_mode: Option<f64>,
    pub delta_enabled: Option<bool>,
    pub maximum_delta_operations: Option<f64>,
    pub zstd_enabled: Option<bool>,
    // image
    pub encoded: Option<Buffer>,
    pub encoding: Option<f64>,
    pub encoded_length: Option<f64>,
    pub sha256: Option<Buffer>,
    pub cache_lookup: Option<bool>,
    /// Declare the track as microphone audio flowing toward the producer side.
    pub uplink: Option<bool>,
    // common claims
    pub track_id: Option<f64>,
    pub slot: Option<f64>,
    pub mode: Option<f64>,
    pub lane: Option<f64>,
    pub direction: Option<f64>,
    pub maximum_record_body: Option<f64>,
    pub maximum_rate_millihertz: Option<f64>,
    pub maximum_encoded_bits_per_second: Option<f64>,
    pub maximum_records_per_second: Option<f64>,
    pub maximum_inflight_body_bytes: Option<f64>,
    pub target_latency_us: Option<f64>,
    pub maximum_latency_us: Option<f64>,
    pub retained_pixel_charge: Option<f64>,
}

/// Build a track configuration through the SDK's builder.
///
/// The claims the protocol bounds — record body, in-flight bytes, retained pixels, decoded
/// pixels, and the rate defaults that follow from them — are computed in Rust with checked
/// arithmetic. This function states only what the caller asked for, so an unset claim takes the
/// builder's default for that kind rather than a number written here.
fn track_configuration(
    config: &TrackConfig,
    session: &SdkSession,
    context_id: u64,
    surface_id: u64,
) -> Result<TrackConfiguration> {
    let slot = match config.slot {
        Some(value) => opt_u64(Some(value), "slot")?,
        None => match config.kind.as_str() {
            "video" => vivid_sdk::SLOT_PRIMARY_VIDEO,
            "audio" => vivid_sdk::SLOT_AUDIO,
            "image" => vivid_sdk::SLOT_POSTER,
            _ => vivid_sdk::SLOT_RASTER,
        },
    };
    let mode = match config.mode {
        None => TrackMode::Live,
        Some(value) => TrackMode::try_from(opt_u64(Some(value), "mode")?)
            .map_err(|error| value_error(error.to_string()))?,
    };
    let lane = match config.lane {
        None => LaneClass::Bulk,
        Some(value) => LaneClass::try_from(opt_u64(Some(value), "lane")?)
            .map_err(|error| value_error(error.to_string()))?,
    };

    let contract = session.info().resource_contract.clone();
    let mut builder = vivid_sdk::TrackBuilder::detached(context_id, surface_id, slot, mode, lane);

    match config.kind.as_str() {
        "video" => {
            builder = builder.video(
                opt_u64(config.width, "width")? as u32,
                opt_u64(config.height, "height")? as u32,
                &config
                    .codec
                    .clone()
                    .ok_or_else(|| value_error("video tracks need a codec"))?,
            );
        }
        "audio" => {
            builder = builder.audio(
                opt_u64(config.sample_rate, "sampleRate")? as u32,
                opt_u64(config.channels, "channels")? as u8,
            );
        }
        "raster" => {
            builder = builder
                .raster(
                    opt_u64(config.width, "width")? as u32,
                    opt_u64(config.height, "height")? as u32,
                )
                .map_err(io_error)?;
        }
        "image" => {
            let encoded = config
                .encoded
                .as_ref()
                .ok_or_else(|| value_error("image tracks need the encoded container"))?;
            let mut image = vivid_sdk::probe_encoded_image(encoded).map_err(io_error)?;
            image.sha256 = config
                .sha256
                .as_ref()
                .map(|bytes| {
                    <[u8; 32]>::try_from(bytes.to_vec())
                        .map_err(|_| value_error("sha256 must contain 32 bytes"))
                })
                .transpose()?;
            image.cache_lookup = config.cache_lookup.unwrap_or(false);
            builder = builder.image(image).map_err(io_error)?;
        }
        other => {
            return Err(value_error(format!(
                "track kind must be video, audio, raster, or image, not {other:?}"
            )));
        }
    }

    if config.uplink.unwrap_or(false) {
        builder = builder.uplink();
    }
    if let Some(value) = opt_u64(config.maximum_rate_millihertz, "maximumRateMillihertz")
        .ok()
        .filter(|value| *value > 0)
    {
        builder = builder.max_rate_millihertz(value);
    }
    if let Some(value) = opt_u64(
        config.maximum_encoded_bits_per_second,
        "maximumEncodedBitsPerSecond",
    )
    .ok()
    .filter(|value| *value > 0)
    {
        builder = builder.max_encoded_bps(value);
    }

    let track_id = match config.track_id {
        Some(track_id) => opt_u64(Some(track_id), "trackId")?,
        None => session.allocate_id().map_err(io_error)?,
    };
    let mut configuration = builder.build(&contract, track_id).map_err(io_error)?;

    // The builder owns the claims; these are the codec details a caller may state explicitly.
    match &mut configuration.kind {
        KindConfiguration::Video(video) => {
            if let Some(value) = config.packetization.clone() {
                video.packetization = value;
            }
            if let Some(value) = config.extradata.as_ref() {
                video.extradata = value.to_vec();
            }
            if let Some(value) = config.profile {
                video.profile = value as i32;
            }
            if let Some(value) = config.level {
                video.level = value as i32;
            }
            if let Some(value) = config.maximum_reorder_depth {
                video.maximum_reorder_depth = value as u8;
            }
            if let Some(value) = config.color_primaries {
                video.color_primaries = value as u64;
            }
            if let Some(value) = config.transfer {
                video.transfer = value as u64;
            }
            if let Some(value) = config.matrix {
                video.matrix = value as u64;
            }
            if let Some(value) = config.signal_range {
                video.signal_range = value as u64;
            }
            if let Some(value) = config.aspect_numerator {
                video.aspect_numerator = value as u64;
            }
            if let Some(value) = config.aspect_denominator {
                video.aspect_denominator = value as u64;
            }
            if let Some(value) = config.maximum_access_unit_bytes {
                video.maximum_access_unit_bytes = value as u32;
            }
            video.codec_string = config.codec_string.clone();
            video.decoder_configuration = config
                .decoder_configuration
                .as_ref()
                .map(|value| value.to_vec());
        }
        KindConfiguration::Audio(audio) => {
            if let Some(value) = config.codec.clone() {
                audio.codec = value;
            }
            if let Some(value) = config.packetization.clone() {
                audio.packetization = value;
            }
            if let Some(value) = config.extradata.as_ref() {
                audio.extradata = value.to_vec();
            }
            if let Some(value) = config.channel_mask {
                audio.channel_mask = value as u64;
            }
            if let Some(value) = config.maximum_access_unit_bytes {
                audio.maximum_access_unit_bytes = value as u32;
            }
            audio.codec_string = config.codec_string.clone();
        }
        KindConfiguration::Raster(raster) => {
            if let Some(value) = config.alpha_mode {
                raster.alpha_mode = value as u64;
            }
            if let Some(value) = config.delta_enabled {
                raster.delta_enabled = value;
            }
            if let Some(value) = config.maximum_delta_operations {
                raster.maximum_delta_operations = value as u8;
            }
            if let Some(value) = config.zstd_enabled {
                raster.zstd_enabled = value;
            }
        }
        KindConfiguration::EncodedImage(image) => {
            if let Some(value) = config.encoded_length {
                image.encoded_length = value as u32;
            }
        }
    }
    Ok(configuration)
}

#[napi]
pub struct Track {
    pub(crate) inner: SdkTrack,
}

#[napi]
impl Track {
    #[napi(getter)]
    pub fn context_id(&self) -> f64 {
        self.inner.context_id() as f64
    }

    #[napi(getter)]
    pub fn surface_id(&self) -> f64 {
        self.inner.surface_id() as f64
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id() as f64
    }

    #[napi(getter)]
    pub fn kind(&self) -> String {
        kind_name(self.inner.kind()).to_owned()
    }

    #[napi(getter)]
    pub fn revision(&self) -> f64 {
        self.inner.revision().get() as f64
    }

    #[napi(getter)]
    pub fn channel_generation(&self) -> f64 {
        self.inner.channel_generation().get() as f64
    }
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Raster => "raster",
        TrackKind::EncodedImage => "image",
    }
}

#[napi(object)]
pub struct WaitSatisfiedPayload {
    pub context_id: f64,
    pub surface_id: f64,
    pub track_id: f64,
    pub revision: f64,
    pub channel_generation: f64,
    pub condition: f64,
    pub observed_value: f64,
}

// ---------------------------------------------------------------------------
// Channels and media
// ---------------------------------------------------------------------------

#[napi]
pub struct TrackChannel {
    inner: Arc<Mutex<Option<SdkTrackChannel>>>,
    context_id: u64,
    surface_id: u64,
    track_id: u64,
    kind: &'static str,
    generation: u64,
}

#[napi(object)]
pub struct VideoPacketOptions {
    pub packet_id: f64,
    pub pts_us: f64,
    pub dts_us: f64,
    pub duration_us: f64,
    pub key: bool,
    pub epoch: Option<f64>,
}

#[napi(object)]
pub struct AudioPacketOptions {
    pub packet_id: f64,
    pub pts_us: f64,
    pub dts_us: f64,
    pub duration_us: f64,
    pub epoch: Option<f64>,
    pub trim_start_samples: Option<f64>,
    pub trim_end_samples: Option<f64>,
}

/// How long the last send waited and why.
///
/// The three causes have opposite remedies — lower the encoder's output, wait for the presenter
/// to return channel-flow capacity, or shrink the transport writes — so they are reported
/// separately rather than flattened into one "slow" number.
#[napi(object)]
pub struct SendPressurePayload {
    pub rate_limited_us: f64,
    pub flow_limited_us: f64,
    pub transport_us: f64,
    pub records: f64,
}

fn send_pressure(pressure: &vivid_sdk::SendPressure) -> SendPressurePayload {
    SendPressurePayload {
        rate_limited_us: pressure.rate_limited.as_micros() as f64,
        flow_limited_us: pressure.flow_limited.as_micros() as f64,
        transport_us: pressure.transport.as_micros() as f64,
        records: pressure.records as f64,
    }
}

/// One reverse-channel event, flattened by `kind`.
#[napi(object)]
pub struct ChannelEventPayload {
    pub kind: String,
    pub payload: Option<PayloadPayload>,
    /// The presenter's typed rejection, for the `error` kind.
    pub code: Option<f64>,
    pub message: Option<String>,
}

fn channel_event(event: vivid_sdk::ChannelEvent) -> Result<ChannelEventPayload> {
    use vivid_sdk::ChannelEvent as E;
    Ok(match event {
        E::NeedKeyframe(payload) => ChannelEventPayload {
            kind: "needKeyframe".into(),
            payload: Some(payload_payload(&payload)?),
            code: None,
            message: None,
        },
        E::NeedFullFrame(payload) => ChannelEventPayload {
            kind: "needFullFrame".into(),
            payload: Some(payload_payload(&payload)?),
            code: None,
            message: None,
        },
        E::Error(presenter_error) => ChannelEventPayload {
            kind: "error".into(),
            payload: None,
            code: Some(presenter_error.code as f64),
            message: Some(presenter_error.to_string()),
        },
    })
}

#[napi]
impl TrackChannel {
    #[napi(getter)]
    pub fn context_id(&self) -> f64 {
        self.context_id as f64
    }

    #[napi(getter)]
    pub fn surface_id(&self) -> f64 {
        self.surface_id as f64
    }

    #[napi(getter)]
    pub fn track_id(&self) -> f64 {
        self.track_id as f64
    }

    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.to_owned()
    }

    #[napi(getter)]
    pub fn generation(&self) -> f64 {
        self.generation as f64
    }

    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "track channel")?.is_none())
    }

    /// Send one full RGBA frame, optionally compressed.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_raster(
        &self,
        rgba: Buffer,
        epoch: Option<f64>,
        frame_id: Option<f64>,
        compress: Option<bool>,
    ) -> Result<f64> {
        let epoch = opt_u64(epoch, "epoch")? as u32;
        let frame_id = opt_u64(frame_id, "frameId")?;
        let compress = compress.unwrap_or(false);
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .send_raster(epoch, frame_id, &rgba, compress)
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Send one full RGBA frame, compressing only when the result is actually smaller.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_raster_adaptive(
        &self,
        rgba: Buffer,
        epoch: Option<f64>,
        frame_id: Option<f64>,
    ) -> Result<f64> {
        let epoch = opt_u64(epoch, "epoch")? as u32;
        let frame_id = opt_u64(frame_id, "frameId")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .send_raster_adaptive(epoch, frame_id, &rgba)
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Send one encoded image; the length must match the track's declared `encodedLength`.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_image(&self, encoded: Buffer) -> Result<f64> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .send_image(&encoded)
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_video(&self, data: Buffer, options: VideoPacketOptions) -> Result<f64> {
        let epoch = opt_u64(options.epoch, "epoch")? as u32;
        let packet_id = opt_u64(Some(options.packet_id), "packetId")?;
        let pts_us = options.pts_us as i64;
        let dts_us = options.dts_us as i64;
        let duration_us = opt_u64(Some(options.duration_us), "durationUs")?;
        let key = options.key;
        let data = data.to_vec();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .send_video(VideoPacket {
                    epoch,
                    packet_id,
                    pts_us,
                    dts_us,
                    duration_us,
                    key,
                    data: &data,
                })
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_audio(&self, data: Buffer, options: AudioPacketOptions) -> Result<f64> {
        let epoch = opt_u64(options.epoch, "epoch")? as u32;
        let packet_id = opt_u64(Some(options.packet_id), "packetId")?;
        let pts_us = options.pts_us as i64;
        let dts_us = options.dts_us as i64;
        let duration_us = opt_u64(Some(options.duration_us), "durationUs")?;
        let trim_start_samples = opt_u64(options.trim_start_samples, "trimStartSamples")? as u32;
        let trim_end_samples = opt_u64(options.trim_end_samples, "trimEndSamples")? as u32;
        let data = data.to_vec();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .send_audio(AudioPacket {
                    epoch,
                    packet_id,
                    pts_us,
                    dts_us,
                    duration_us,
                    trim_start_samples,
                    trim_end_samples,
                    data: &data,
                })
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Signal ordered end-of-stream.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn eos(&self) -> Result<f64> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .eos()
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Close this channel generation.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn close(&self) -> Result<()> {
        let channel = locked(&self.inner, "track channel")?
            .take()
            .ok_or_else(closed_channel)?;
        blocking(move || channel.close().map_err(io_error)).await
    }

    /// Take the next reverse-channel event without waiting.
    #[napi]
    pub fn take_event(&self) -> Result<Option<ChannelEventPayload>> {
        let guard = locked(&self.inner, "track channel")?;
        guard
            .as_ref()
            .ok_or_else(closed_channel)?
            .take_event()
            .map_err(io_error)?
            .map(channel_event)
            .transpose()
    }

    /// Take the next reverse-channel event, waiting up to `timeout_ms` on a worker thread.
    ///
    /// A keyframe request that arrives while nobody is looking is the difference between a fast
    /// recovery and a frozen picture, so a video sender parks here rather than polls.
    #[napi(ts_return_type = "Promise<ChannelEventPayload | null>")]
    pub async fn wait_event(&self, timeout_ms: f64) -> Result<Option<ChannelEventPayload>> {
        let timeout = duration_ms(timeout_ms, "timeoutMs")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .wait_event(timeout)
                .map_err(io_error)?
                .map(channel_event)
                .transpose()
        })
        .await
    }

    /// Accumulated send pressure since the last call.
    #[napi]
    pub fn take_send_pressure(&self) -> Result<SendPressurePayload> {
        let guard = locked(&self.inner, "track channel")?;
        let pressure = guard
            .as_ref()
            .ok_or_else(closed_channel)?
            .take_send_pressure();
        Ok(send_pressure(&pressure))
    }

    /// Whether a record body of `body_length` bytes fits in the channel's flow window now.
    #[napi]
    pub fn media_credit_available(&self, body_length: f64) -> Result<bool> {
        let body_length = opt_u64(Some(body_length), "bodyLength")? as u32;
        let guard = locked(&self.inner, "track channel")?;
        let available = guard
            .as_ref()
            .ok_or_else(closed_channel)?
            .media_credit_available(body_length);
        Ok(available)
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

#[napi]
impl Session {
    #[napi(ts_return_type = "Promise<SurfaceStatusPayload>")]
    pub async fn query_surface(&self, surface: &Surface) -> Result<SurfaceStatusPayload> {
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let status = session.query_surface(&handle).map_err(io_error)?;
            Ok(SurfaceStatusPayload {
                context_id: status.context_id as f64,
                surface_id: status.surface_id as f64,
                revision: status.revision.get() as f64,
                generation: status.generation.get() as f64,
                semantic_profile: status.semantic_profile.clone(),
                coordinate_model: status.coordinate_model as u64 as f64,
                logical_width: status.logical_width as f64,
                logical_height: status.logical_height as f64,
                role: status.descriptor.role as u64 as f64,
                title: status.descriptor.title.clone(),
                effective_policy: status.effective_policy as f64,
                lifecycle: status.lifecycle as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<TrackStatusPayload>")]
    pub async fn query_track(&self, track: &Track) -> Result<TrackStatusPayload> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let status = session.query_track(&handle).map_err(io_error)?;
            Ok(track_status_payload(&status))
        })
        .await
    }

    /// Ask the presenter whether it would admit this track, without creating it.
    #[napi(ts_return_type = "Promise<TrackSupportPayload>")]
    pub async fn probe_track(
        &self,
        surface: &Surface,
        config: TrackConfig,
    ) -> Result<TrackSupportPayload> {
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let mut configuration =
                track_configuration(&config, session, handle.context_id(), handle.id())?;
            // A probe names no track: the protocol requires key 2 to be zero.
            configuration.track_id = 0;
            let support = session.probe_track(&configuration).map_err(io_error)?;
            Ok(TrackSupportPayload {
                supported: support.supported,
                selected_decoder: support.selected_decoder,
                capability_generation: support.capability_generation as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<AnchorStatusPayload | null>")]
    pub async fn query_anchor(
        &self,
        context_id: f64,
        anchor_id: f64,
    ) -> Result<Option<AnchorStatusPayload>> {
        let context_id = opt_u64(Some(context_id), "contextId")?;
        let anchor_id = opt_u64(Some(anchor_id), "anchorId")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            match session.query_anchor(context_id, anchor_id) {
                Ok(status) => Ok(Some(AnchorStatusPayload {
                    context_id: status.context_id as f64,
                    anchor_id: status.anchor_id as f64,
                    state: status.state as f64,
                    payload: Some(payload_payload(&status.payload)?),
                })),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(io_error(error)),
            }
        })
        .await
    }

    #[napi(ts_return_type = "Promise<PayloadPayload>")]
    pub async fn query_session(&self) -> Result<PayloadPayload> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let payload = session.query_session().map_err(io_error)?;
            payload_payload(&payload)
        })
        .await
    }

    // -- Timed playback ----------------------------------------------------

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn play(
        &self,
        track: &Track,
        start_pts_us: i64,
        minimum_buffer_us: f64,
        maximum_latency_us: f64,
    ) -> Result<()> {
        let minimum_buffer_us = opt_u64(Some(minimum_buffer_us), "minimumBufferUs")?;
        let maximum_latency_us = opt_u64(Some(maximum_latency_us), "maximumLatencyUs")?;
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .play(&handle, start_pts_us, minimum_buffer_us, maximum_latency_us)
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn pause(&self, track: &Track) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .pause(&handle)
                .map_err(io_error)
        })
        .await
    }

    /// Set track gain as a micropercent (2^32 is unity) and optional mute.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn set_audio_gain(&self, track: &Track, gain_raw: f64) -> Result<()> {
        let gain_raw = opt_u64(Some(gain_raw), "gainRaw")?;
        let gain = vivid_sdk::AudioGain::new(gain_raw)
            .ok_or_else(|| value_error("gain must be within 0..=2 * 2^32"))?;
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .set_audio_gain(&handle, gain)
                .map_err(io_error)
        })
        .await
    }

    /// Discard all media below a new epoch and keep the channel open.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn flush(&self, track: &Track, new_epoch: f64) -> Result<()> {
        let new_epoch = opt_u64(Some(new_epoch), "newEpoch")? as u32;
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .flush(&handle, new_epoch)
                .map_err(io_error)
        })
        .await
    }

    /// Wait until the presenter has consumed everything sent so far.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn drain(&self, track: &Track) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .drain(&handle)
                .map_err(io_error)
        })
        .await
    }

    // -- Scene nodes ---------------------------------------------------------

    #[napi(ts_return_type = "Promise<SceneCommitPayload>")]
    pub async fn create_node(
        &self,
        surface: &Surface,
        node_id: f64,
        spec: Option<SceneNodeSpec>,
    ) -> Result<SceneCommitPayload> {
        let node = scene_node(surface, node_id, spec)?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .create_node(&node, &RequestMetadata::default())
                .map_err(io_error)
                .map(|commit| SceneCommitPayload {
                    scene_revision: commit.scene_revision.get() as f64,
                    target_generation: commit.target_generation.get() as f64,
                })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<SceneCommitPayload>")]
    pub async fn update_node(
        &self,
        surface: &Surface,
        node_id: f64,
        spec: Option<SceneNodeSpec>,
    ) -> Result<SceneCommitPayload> {
        let node = scene_node(surface, node_id, spec)?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .update_node(&node, &RequestMetadata::default())
                .map_err(io_error)
                .map(|commit| SceneCommitPayload {
                    scene_revision: commit.scene_revision.get() as f64,
                    target_generation: commit.target_generation.get() as f64,
                })
        })
        .await
    }

    /// Activate a set of slot bindings atomically; each binding names a track and its expected
    /// channel generation.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn activate_tracks(
        &self,
        surface: &Surface,
        bindings: Vec<SlotBindingSpec>,
    ) -> Result<f64> {
        if bindings.is_empty() {
            return Err(value_error("at least one slot binding is required"));
        }
        let mut converted = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            converted.push(SlotBinding {
                slot: opt_u64(Some(binding.slot), "slot")?,
                track_id: opt_u64(Some(binding.track_id), "trackId")?,
                expected_channel_generation: vivid_protocol::revision::ChannelGeneration::new(
                    opt_u64(
                        Some(binding.expected_channel_generation),
                        "expectedChannelGeneration",
                    )?,
                ),
                required_milestone: required_milestone_of(binding.required_milestone)?,
            });
        }
        let inner = Arc::clone(&self.inner);
        let handle = surface.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .activate_tracks(&handle, &converted, &RequestMetadata::default())
                .map(|presentation| presentation as f64)
                .map_err(io_error)
        })
        .await
    }

    /// The anchor marker spelling for a ConPTY host.
    #[napi]
    pub fn conpty_anchor_marker(&self, context_id: f64, anchor_id: f64) -> Result<String> {
        let context_id = opt_u64(Some(context_id), "contextId")?;
        let anchor_id = opt_u64(Some(anchor_id), "anchorId")?;
        let guard = locked(&self.inner, "session")?;
        guard
            .as_ref()
            .ok_or_else(closed_session)?
            .conpty_anchor_marker(context_id, anchor_id)
            .map_err(io_error)
    }

    // -- Channel generation --------------------------------------------------

    /// Start a fresh authenticated channel generation for the track and return its channel.
    #[napi(ts_return_type = "Promise<TrackChannel>")]
    pub async fn advance_channel(&self, track: &Track, reason: f64) -> Result<TrackChannel> {
        let reason = opt_u64(Some(reason), "reason")?;
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let generation = session
                .advance_channel(&handle, reason, &RequestMetadata::default())
                .map_err(io_error)?;
            let channel = session.open_track_channel(&handle).map_err(io_error)?;
            if channel.generation() != generation {
                return Err(tagged(
                    VIVID,
                    "channel advance did not produce the requested generation",
                ));
            }
            Ok(TrackChannel {
                context_id: handle.context_id(),
                surface_id: handle.surface_id(),
                track_id: handle.id(),
                kind: kind_name(handle.kind()),
                generation: channel.generation().get(),
                inner: Arc::new(Mutex::new(Some(channel))),
            })
        })
        .await
    }
}

/// Geometry and presentation options for one scene node.
#[napi(object)]
pub struct SceneNodeSpec {
    pub geometry: Option<PayloadPayload>,
    pub fit: Option<f64>,
    pub linear_sampling: Option<bool>,
    pub z_index: Option<f64>,
    pub visible: Option<bool>,
    /// 0..=255 opacity.
    pub opacity: Option<f64>,
}

/// One slot activation binding.
#[napi(object)]
pub struct SlotBindingSpec {
    pub slot: f64,
    pub track_id: f64,
    pub expected_channel_generation: f64,
    pub required_milestone: Option<f64>,
}

fn required_milestone_of(value: Option<f64>) -> Result<u64> {
    Ok(match value {
        None => 1 << 4,
        Some(value) => opt_u64(Some(value), "requiredMilestone")?,
    })
}

fn track_status_payload(status: &TrackStatus) -> TrackStatusPayload {
    TrackStatusPayload {
        context_id: status.context_id as f64,
        surface_id: status.surface_id as f64,
        track_id: status.track_id as f64,
        kind: kind_name(status.kind).to_owned(),
        mode: status.mode as u64 as f64,
        revision: status.revision.get() as f64,
        channel_generation: status.channel_generation.get() as f64,
        lifecycle: status.lifecycle as f64,
        attachment_state: status.attachment_state as f64,
        milestones: status.milestones as f64,
        media_epoch: status.media_epoch as f64,
        last_media_id: status.last_media_id as f64,
        last_media_record_sequence: status.last_media_record_sequence as f64,
        last_decoded_pts_us: status.last_decoded_pts_us as f64,
        last_presented_pts_us: status.last_presented_pts_us as f64,
        last_presentation_id: status.last_presentation_id as f64,
        cumulative_flow_records: status.cumulative_media_records as f64,
        cumulative_flow_body_bytes: status.cumulative_body_bytes as f64,
        maximum_body_bytes: status.maximum_body_bytes as f64,
        maximum_media_records: status.maximum_media_records as f64,
        ingress_depth_bucket: status.ingress_depth_bucket as f64,
        playback_state: status
            .playback_state
            .as_ref()
            .and_then(|state| {
                state.iter().next().and_then(|(_, value)| match value {
                    Value::Unsigned(unsigned) => Some(*unsigned as f64),
                    _ => None,
                })
            })
            .unwrap_or(0.0),
        terminal_loss_code: status.terminal_loss_code.map(|code| code as f64),
    }
}

#[napi(object)]
pub struct TrackStatusPayload {
    pub context_id: f64,
    pub surface_id: f64,
    pub track_id: f64,
    pub kind: String,
    pub mode: f64,
    pub revision: f64,
    pub channel_generation: f64,
    pub lifecycle: f64,
    pub attachment_state: f64,
    pub milestones: f64,
    pub media_epoch: f64,
    pub last_media_id: f64,
    pub last_media_record_sequence: f64,
    pub last_decoded_pts_us: f64,
    pub last_presented_pts_us: f64,
    pub last_presentation_id: f64,
    pub cumulative_flow_records: f64,
    pub cumulative_flow_body_bytes: f64,
    pub maximum_body_bytes: f64,
    pub maximum_media_records: f64,
    pub ingress_depth_bucket: f64,
    pub playback_state: f64,
    pub terminal_loss_code: Option<f64>,
}

#[napi(object)]
pub struct SurfaceStatusPayload {
    pub context_id: f64,
    pub surface_id: f64,
    pub revision: f64,
    pub generation: f64,
    pub semantic_profile: String,
    pub coordinate_model: f64,
    pub logical_width: f64,
    pub logical_height: f64,
    pub role: f64,
    pub title: String,
    pub effective_policy: f64,
    pub lifecycle: f64,
}

#[napi(object)]
pub struct TrackSupportPayload {
    pub supported: bool,
    pub selected_decoder: String,
    pub capability_generation: f64,
}

#[napi(object)]
pub struct AnchorStatusPayload {
    pub context_id: f64,
    pub anchor_id: f64,
    /// Unknown (`0`), ready (`1`), or gone (`2`).
    pub state: f64,
    pub payload: Option<PayloadPayload>,
}

fn scene_node(surface: &Surface, node_id: f64, spec: Option<SceneNodeSpec>) -> Result<SceneNode> {
    let spec = spec.unwrap_or(SceneNodeSpec {
        geometry: None,
        fit: None,
        linear_sampling: None,
        z_index: None,
        visible: None,
        opacity: None,
    });
    let geometry = match &spec.geometry {
        Some(payload) => {
            let mut geometry = Vec::new();
            for scalar in &payload.scalars {
                if let Some(unsigned) = scalar.unsigned {
                    geometry.push((scalar.key as u64, Value::Unsigned(unsigned as u64)));
                } else if let Some(text) = &scalar.text {
                    geometry.push((scalar.key as u64, Value::Text(text.clone())));
                }
            }
            geometry
        }
        None => Vec::new(),
    };
    Ok(SceneNode {
        owning_context_id: surface.inner.context_id(),
        node_id: opt_u64(Some(node_id), "nodeId")?,
        surface_context_id: surface.inner.context_id(),
        surface_id: surface.inner.id(),
        geometry,
        fit: match spec.fit {
            None => Fit::Contain,
            Some(value) => Fit::try_from(opt_u64(Some(value), "fit")?)
                .map_err(|error| value_error(error.to_string()))?,
        },
        linear_sampling: spec.linear_sampling.unwrap_or(true),
        z_index: spec.z_index.map(|value| value as i64).unwrap_or(0),
        visible: spec.visible.unwrap_or(true),
        opacity: spec.opacity.map(|value| value as u16).unwrap_or(255),
        clip: None,
    })
}

// ---------------------------------------------------------------------------
// Raster deltas
// ---------------------------------------------------------------------------

/// One raster delta operation: `overwrite` copies a rectangle of RGBA over the base frame, and
/// `copy` moves pixels from one part of the base frame to another.
#[napi(object)]
pub struct DeltaOverwrite {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rgba: Buffer,
}

#[napi(object)]
pub struct DeltaCopy {
    pub destination_x: f64,
    pub destination_y: f64,
    pub width: f64,
    pub height: f64,
    pub source_x: f64,
    pub source_y: f64,
}

#[napi]
impl TrackChannel {
    /// Send a delta against `base_frame_id`; the pixel rectangles are owned by this call.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_raster_delta(
        &self,
        overwrites: Vec<DeltaOverwrite>,
        copies: Vec<DeltaCopy>,
        epoch: Option<f64>,
        frame_id: Option<f64>,
        base_frame_id: f64,
        pts_us: Option<f64>,
        duration_us: Option<f64>,
        compress: Option<bool>,
    ) -> Result<f64> {
        let epoch = opt_u64(epoch, "epoch")? as u32;
        let frame_id = opt_u64(frame_id, "frameId")?;
        let base_frame_id = opt_u64(Some(base_frame_id), "baseFrameId")?;
        let pts_us = pts_us.unwrap_or(0.0) as i64;
        let duration_us = opt_u64(duration_us, "durationUs")?;
        let compress = compress.unwrap_or(false);
        // Two owned stores keep the borrows stable for the duration of the call.
        let rgbas: Vec<Vec<u8>> = overwrites
            .iter()
            .map(|op| {
                let width = op.width as usize;
                let height = op.height as usize;
                let expected = width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or_else(|| value_error("overwrite geometry overflows u32"))?;
                if op.rgba.len() != expected {
                    return Err(value_error(
                        "overwrite rgba length does not equal width * height * 4",
                    ));
                }
                Ok(op.rgba.to_vec())
            })
            .collect::<Result<Vec<_>>>()?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            let channel = guard.as_ref().ok_or_else(closed_channel)?;
            let mut operations = Vec::with_capacity(overwrites.len() + copies.len());
            for ((overwrite, rgba), copy) in overwrites
                .iter()
                .zip(rgbas.iter())
                .zip(copies.iter().map(Some).chain(std::iter::repeat(None)))
            {
                let _ = copy;
                operations.push(RasterDeltaOperation::Overwrite {
                    x: overwrite.x as u32,
                    y: overwrite.y as u32,
                    width: overwrite.width as u32,
                    height: overwrite.height as u32,
                    rgba,
                });
            }
            for copy in &copies {
                operations.push(RasterDeltaOperation::Copy {
                    destination_x: copy.destination_x as u32,
                    destination_y: copy.destination_y as u32,
                    width: copy.width as u32,
                    height: copy.height as u32,
                    source_x: copy.source_x as u32,
                    source_y: copy.source_y as u32,
                });
            }
            channel
                .send_raster_delta(
                    epoch,
                    frame_id,
                    base_frame_id,
                    pts_us,
                    duration_us,
                    &operations,
                    compress,
                )
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Send a delta, compressing only when that is actually smaller than raw.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_raster_delta_adaptive(
        &self,
        overwrites: Vec<DeltaOverwrite>,
        copies: Vec<DeltaCopy>,
        epoch: Option<f64>,
        frame_id: Option<f64>,
        base_frame_id: f64,
        pts_us: Option<f64>,
        duration_us: Option<f64>,
    ) -> Result<f64> {
        // The adaptive variant is the same call with compression decided inside the SDK.
        self.send_raster_delta(
            overwrites,
            copies,
            epoch,
            frame_id,
            base_frame_id,
            pts_us,
            duration_us,
            Some(false),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Input lanes
// ---------------------------------------------------------------------------

/// A binding request for the desktop-input profile.
#[napi(object)]
pub struct InputBindingSpec {
    pub producer_epoch: f64,
    pub context_id: f64,
    pub surface_id: f64,
    pub surface_generation: f64,
    pub requested_classes: f64,
    pub reason: Option<f64>,
    pub requested_watchdog_us: Option<f64>,
}

#[napi(object)]
pub struct InputBindingStatusPayload {
    pub producer_epoch: f64,
    pub grant_generation: f64,
    pub context_id: f64,
    pub surface_id: f64,
    pub surface_generation: f64,
    pub effective_classes: f64,
    pub state: f64,
    pub reason: f64,
    pub watchdog_timeout_us: f64,
}

fn binding_status(status: InputBindingStatus) -> InputBindingStatusPayload {
    InputBindingStatusPayload {
        producer_epoch: status.producer_epoch as f64,
        grant_generation: status.grant_generation as f64,
        context_id: status.context_id as f64,
        surface_id: status.surface_id as f64,
        surface_generation: status.surface_generation as f64,
        effective_classes: status.effective_classes as f64,
        state: status.state as f64,
        reason: status.reason as f64,
        watchdog_timeout_us: status.watchdog_timeout_us as f64,
    }
}

/// One lane event, flattened by `kind`; only the fields the variant carries are set.
#[napi(object)]
pub struct InputLaneEventPayload {
    pub kind: String,
    pub record_type: Option<f64>,
    pub surface_id: Option<f64>,
    pub payload: Option<PayloadPayload>,
    pub producer_epoch: Option<f64>,
    pub grant_generation: Option<f64>,
    pub context_id: Option<f64>,
    pub surface_generation: Option<f64>,
    pub renewal_sequence: Option<f64>,
    pub watchdog_timeout_us: Option<f64>,
    pub reason: Option<f64>,
    pub diagnostic: Option<String>,
    pub message: Option<String>,
}

fn tuple_fields(event: &mut InputLaneEventPayload, binding: &vivid_protocol::input::InputTuple) {
    event.producer_epoch = Some(binding.producer_epoch.get() as f64);
    event.grant_generation = Some(binding.grant_generation.get() as f64);
    event.context_id = Some(binding.context_id as f64);
    event.surface_id = Some(binding.surface_id as f64);
    event.surface_generation = Some(binding.surface_generation.get() as f64);
}

fn input_lane_event(event: InputLaneEvent) -> Result<InputLaneEventPayload> {
    let mut out = InputLaneEventPayload {
        kind: String::new(),
        record_type: None,
        surface_id: None,
        payload: None,
        producer_epoch: None,
        grant_generation: None,
        context_id: None,
        surface_generation: None,
        renewal_sequence: None,
        watchdog_timeout_us: None,
        reason: None,
        diagnostic: None,
        message: None,
    };
    match event {
        InputLaneEvent::Input {
            record_type,
            surface_id,
            payload,
        } => {
            out.kind = "input".into();
            out.record_type = Some(record_type as f64);
            out.surface_id = Some(surface_id as f64);
            out.payload = Some(payload_payload(&payload)?);
        }
        InputLaneEvent::Renew(renewal) => {
            out.kind = "renew".into();
            tuple_fields(&mut out, &renewal.binding);
            out.renewal_sequence = Some(renewal.renewal_sequence as f64);
            out.watchdog_timeout_us = Some(renewal.watchdog_timeout_us as f64);
        }
        InputLaneEvent::Revoked(termination) => {
            out.kind = "revoked".into();
            tuple_fields(&mut out, &termination.binding);
            out.reason = Some(termination.reason as f64);
        }
        InputLaneEvent::Reset(termination) => {
            out.kind = "reset".into();
            tuple_fields(&mut out, &termination.binding);
            out.reason = Some(termination.reason as f64);
        }
        InputLaneEvent::LaneClosed { diagnostic } => {
            out.kind = "lane_closed".into();
            out.diagnostic = Some(diagnostic);
        }
        InputLaneEvent::Error(presenter_error) => {
            out.kind = "error".into();
            out.message = Some(presenter_error.to_string());
        }
    }
    Ok(out)
}

#[napi]
pub struct InputLane {
    inner: Arc<Mutex<Option<InputLane_>>>,
}

type InputLane_ = vivid_sdk::InputLane;

#[napi]
impl InputLane {
    #[napi(getter)]
    pub fn generation(&self) -> f64 {
        locked(&self.inner, "input lane")
            .ok()
            .and_then(|guard| guard.as_ref().map(|lane| lane.generation()))
            .unwrap_or(0) as f64
    }

    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "input lane")?.is_none())
    }

    /// Bind the lane to a surface's input classes. Zero context and surface disable injection.
    #[napi(ts_return_type = "Promise<InputBindingStatusPayload>")]
    pub async fn set_binding(&self, spec: InputBindingSpec) -> Result<InputBindingStatusPayload> {
        let binding = InputBinding {
            producer_epoch: vivid_protocol::revision::InputEpoch::new(opt_u64(
                Some(spec.producer_epoch),
                "producerEpoch",
            )?),
            context_id: opt_u64(Some(spec.context_id), "contextId")?,
            surface_id: opt_u64(Some(spec.surface_id), "surfaceId")?,
            surface_generation: vivid_protocol::revision::SurfaceGeneration::new(opt_u64(
                Some(spec.surface_generation),
                "surfaceGeneration",
            )?),
            requested_classes: opt_u64(Some(spec.requested_classes), "requestedClasses")?,
            reason: opt_u64(spec.reason, "reason").unwrap_or(1),
            requested_watchdog_us: opt_u64(spec.requested_watchdog_us, "requestedWatchdogUs")
                .unwrap_or(1_000_000),
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "input lane")?;
            guard
                .as_ref()
                .ok_or_else(|| tagged(CLOSED, "input lane is closed"))?
                .set_binding(&binding)
                .map_err(io_error)
                .map(binding_status)
        })
        .await
    }

    /// Take the next input event without waiting; pointer-motion bursts make polling costly.
    #[napi]
    pub fn take_event(&self) -> Result<Option<InputLaneEventPayload>> {
        let guard = locked(&self.inner, "input lane")?;
        guard
            .as_ref()
            .ok_or_else(|| tagged(CLOSED, "input lane is closed"))?
            .take_event()
            .map_err(io_error)?
            .map(input_lane_event)
            .transpose()
    }

    /// Take the next input event, waiting up to `timeout_ms` on a worker thread.
    #[napi(ts_return_type = "Promise<InputLaneEventPayload | null>")]
    pub async fn wait_event(&self, timeout_ms: f64) -> Result<Option<InputLaneEventPayload>> {
        let timeout = duration_ms(timeout_ms, "timeoutMs")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "input lane")?;
            guard
                .as_ref()
                .ok_or_else(|| tagged(CLOSED, "input lane is closed"))?
                .wait_event(timeout)
                .map_err(io_error)?
                .map(input_lane_event)
                .transpose()
        })
        .await
    }

    /// Close the lane; the session stays usable.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn close(&self) -> Result<()> {
        let lane = locked(&self.inner, "input lane")?
            .take()
            .ok_or_else(|| tagged(CLOSED, "input lane is closed"))?;
        blocking(move || lane.close().map_err(io_error)).await
    }
}

#[napi]
impl Session {
    /// Open a desktop-input lane; requires the `desktop-input-v1` profile and its own
    /// authenticated connection.
    #[napi(ts_return_type = "Promise<InputLane>")]
    pub async fn open_input_lane(&self, lane_generation: f64) -> Result<InputLane> {
        let lane_generation = opt_u64(Some(lane_generation), "laneGeneration")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            session
                .open_input_lane(lane_generation)
                .map(|lane| InputLane {
                    inner: Arc::new(Mutex::new(Some(lane))),
                })
                .map_err(io_error)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// File drop
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct MediaResourceInfo {
    pub binding_pinned: bool,
    pub producer: f64,
    pub context: f64,
    pub surface: f64,
    pub surface_revision: f64,
    pub surface_generation: f64,
    pub track_id: Option<f64>,
    pub track_revision: Option<f64>,
    pub track_channel_generation: Option<f64>,
    pub track_media_epoch: Option<f64>,
    pub capturable: Option<bool>,
}

/// The outer gateway's playback position for one source.
#[napi(object)]
pub struct PositionSnapshotSpec {
    pub decoder_reset_serial: f64,
    pub playing: bool,
    pub start_pts_us: f64,
    pub state: f64,
    pub clock_pts_us: Option<f64>,
    pub decoded_pts_us: f64,
    pub presented_pts_us: f64,
    pub presentation_id: f64,
}

/// A file-drop binding request; `destination` of `null` disables the binding.
#[napi(object)]
pub struct FileDropBindingSpec {
    pub producer_epoch: f64,
    pub context_id: f64,
    pub surface_id: f64,
    pub surface_generation: f64,
    /// `1` (shell cwd) or `2` (desktop folder); `null` disables the binding.
    pub destination: Option<f64>,
    pub maximum_file_bytes: f64,
    pub maximum_pending_offers: Option<f64>,
    pub maximum_active_transfers: Option<f64>,
    pub maximum_record_body: f64,
    pub acceptance_timeout_us: Option<f64>,
    pub idle_timeout_us: Option<f64>,
}

#[napi(object)]
pub struct FileDropGrantPayload {
    pub producer_epoch: f64,
    pub grant_generation: f64,
    pub context_id: f64,
    pub surface_id: f64,
    pub surface_generation: f64,
    pub state: f64,
    pub destination: Option<f64>,
    pub maximum_file_bytes: f64,
    pub maximum_pending_offers: f64,
    pub maximum_active_transfers: f64,
    pub maximum_record_body: f64,
    pub acceptance_timeout_us: f64,
    pub idle_timeout_us: f64,
    pub reason: f64,
}

/// The complete drop identity every verb names.
#[napi(object)]
pub struct FileDropTupleSpec {
    pub producer_epoch: f64,
    pub grant_generation: f64,
    pub context_id: f64,
    pub surface_id: f64,
    pub surface_generation: f64,
    pub drop_id: f64,
}

#[napi(object)]
pub struct FileDropAcceptedPayload {
    pub drop_id: f64,
    pub transfer_id: f64,
    pub transfer_generation: f64,
    pub open_timeout_us: f64,
}

#[napi(object)]
pub struct FileTransferAdvancedPayload {
    pub transfer_id: f64,
    pub generation: f64,
    pub committed_offset: f64,
    pub open_timeout_us: f64,
}

#[napi(object)]
pub struct FileDropStatusPayload {
    pub drop_id: f64,
    pub state: f64,
    pub transfer_id: f64,
    pub generation: f64,
    pub committed_offset: f64,
    pub result: Option<f64>,
    pub final_name: String,
}

fn tuple_from_spec(spec: &FileDropTupleSpec) -> Result<FileDropTuple> {
    Ok(FileDropTuple {
        producer_epoch: vivid_protocol::revision::FileDropEpoch::new(opt_u64(
            Some(spec.producer_epoch),
            "producerEpoch",
        )?),
        grant_generation: vivid_protocol::revision::FileDropGrantGeneration::new(opt_u64(
            Some(spec.grant_generation),
            "grantGeneration",
        )?),
        context_id: opt_u64(Some(spec.context_id), "contextId")?,
        surface_id: opt_u64(Some(spec.surface_id), "surfaceId")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(opt_u64(
            Some(spec.surface_generation),
            "surfaceGeneration",
        )?),
        drop_id: opt_u64(Some(spec.drop_id), "dropId")?,
    })
}

fn binding_from_spec(spec: FileDropBindingSpec) -> Result<FileDropBinding> {
    Ok(FileDropBinding {
        producer_epoch: vivid_protocol::revision::FileDropEpoch::new(opt_u64(
            Some(spec.producer_epoch),
            "producerEpoch",
        )?),
        context_id: opt_u64(Some(spec.context_id), "contextId")?,
        surface_id: opt_u64(Some(spec.surface_id), "surfaceId")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(opt_u64(
            Some(spec.surface_generation),
            "surfaceGeneration",
        )?),
        destination: match spec.destination {
            None => None,
            Some(value) => Some(
                FileDropDestination::try_from(opt_u64(Some(value), "destination")?)
                    .map_err(|error| value_error(error.to_string()))?,
            ),
        },
        maximum_file_bytes: opt_u64(Some(spec.maximum_file_bytes), "maximumFileBytes")?,
        maximum_pending_offers: opt_u64(spec.maximum_pending_offers, "maximumPendingOffers")
            .unwrap_or(8),
        maximum_active_transfers: opt_u64(spec.maximum_active_transfers, "maximumActiveTransfers")
            .unwrap_or(4),
        maximum_record_body: opt_u64(Some(spec.maximum_record_body), "maximumRecordBody")? as u32,
        acceptance_timeout_us: opt_u64(spec.acceptance_timeout_us, "acceptanceTimeoutUs")
            .unwrap_or(20_000_000),
        idle_timeout_us: opt_u64(spec.idle_timeout_us, "idleTimeoutUs").unwrap_or(5_000_000),
    })
}

#[napi(object)]
pub struct FileTransferAdvanceSpec {
    pub context_id: f64,
    pub surface_id: f64,
    pub drop_id: f64,
    pub transfer_id: f64,
    pub expected_generation: f64,
    pub new_generation: f64,
    pub committed_offset: f64,
    pub maximum_body_bytes: f64,
    pub maximum_records: f64,
}

#[napi(object)]
pub struct IncomingFileTransferSpec {
    pub context_id: f64,
    pub surface_id: f64,
    pub producer_epoch: f64,
    pub grant_generation: f64,
    pub surface_generation: f64,
    pub drop_id: f64,
    pub transfer_id: f64,
    pub transfer_generation: f64,
    pub resume_offset: Option<f64>,
    pub declared_length: f64,
    pub maximum_record_body: f64,
    pub maximum_body_bytes: f64,
    pub maximum_records: f64,
}

#[napi(object)]
pub struct TransferResultSpec {
    pub transfer_id: f64,
    pub transfer_generation: f64,
    pub result: f64,
    pub committed_length: Option<f64>,
    pub final_name: Option<String>,
    /// Only carried under `file-drop-path-v1`; rejected locally otherwise.
    pub committed_path: Option<String>,
}

/// One transfer event, flattened by `kind`.
#[napi(object)]
pub struct TransferEventPayload {
    pub kind: String,
    pub offset: Option<f64>,
    pub bytes: Option<Buffer>,
    pub final_length: Option<f64>,
    pub reason: Option<f64>,
    pub final_offset: Option<f64>,
}

fn transfer_event(event: IncomingFileTransferEvent) -> TransferEventPayload {
    match event {
        IncomingFileTransferEvent::Data { offset, bytes } => TransferEventPayload {
            kind: "data".into(),
            offset: Some(offset as f64),
            bytes: Some(bytes.into()),
            final_length: None,
            reason: None,
            final_offset: None,
        },
        IncomingFileTransferEvent::Finished(finish) => TransferEventPayload {
            kind: "finished".into(),
            offset: None,
            bytes: None,
            final_length: Some(finish.final_length as f64),
            reason: None,
            final_offset: None,
        },
        IncomingFileTransferEvent::Aborted(abort) => TransferEventPayload {
            kind: "aborted".into(),
            offset: None,
            bytes: None,
            final_length: None,
            reason: Some(abort.reason as f64),
            final_offset: Some(abort.final_offset as f64),
        },
    }
}

#[napi]
pub struct IncomingFileTransfer {
    inner: Arc<Mutex<Option<vivid_sdk::IncomingFileTransfer>>>,
}

#[napi]
impl IncomingFileTransfer {
    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "file transfer")?.is_none())
    }

    /// Read the next transfer event on a worker thread. Bound the socket with
    /// `setReadDeadline` first so a stalled sender cannot pin the worker.
    #[napi(ts_return_type = "Promise<TransferEventPayload>")]
    pub async fn read_event(&self) -> Result<TransferEventPayload> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "file transfer")?;
            let transfer = guard
                .as_mut()
                .ok_or_else(|| tagged(CLOSED, "file transfer is closed"))?;
            let event = vivid_sdk::IncomingFileTransfer::read_event(transfer).map_err(io_error)?;
            Ok(transfer_event(event))
        })
        .await
    }

    /// Bound subsequent reads; `null` restores unbounded.
    #[napi]
    pub fn set_read_deadline(&self, timeout_us: Option<f64>) -> Result<()> {
        let timeout = opt_u64(timeout_us, "timeoutUs")?;
        let mut guard = locked(&self.inner, "file transfer")?;
        guard
            .as_mut()
            .ok_or_else(|| tagged(CLOSED, "file transfer is closed"))?
            .set_read_deadline(timeout_us.map(|_| Duration::from_micros(timeout)))
            .map_err(io_error)
    }

    /// Grant the sender flow capacity.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn grant(&self, maximum_body_bytes: f64, maximum_records: f64) -> Result<()> {
        let maximum_body_bytes = opt_u64(Some(maximum_body_bytes), "maximumBodyBytes")?;
        let maximum_records = opt_u64(Some(maximum_records), "maximumRecords")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "file transfer")?;
            guard
                .as_mut()
                .ok_or_else(|| tagged(CLOSED, "file transfer is closed"))?
                .grant(maximum_body_bytes, maximum_records)
                .map_err(io_error)
        })
        .await
    }

    /// Send the final result; `committed_path` requires `file-drop-path-v1`.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn send_result(&self, spec: TransferResultSpec) -> Result<()> {
        let result = FileResult {
            transfer_id: opt_u64(Some(spec.transfer_id), "transferId")?,
            transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(opt_u64(
                Some(spec.transfer_generation),
                "transferGeneration",
            )?),
            result: vivid_protocol::file_drop::FileResultCode::try_from(opt_u64(
                Some(spec.result),
                "result",
            )?)
            .map_err(|error| value_error(error.to_string()))?,
            committed_length: opt_u64(spec.committed_length, "committedLength")?,
            final_name: spec.final_name.unwrap_or_default(),
            committed_path: spec.committed_path,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "file transfer")?;
            guard
                .as_ref()
                .ok_or_else(|| tagged(CLOSED, "file transfer is closed"))?
                .send_result(&result)
                .map_err(io_error)
        })
        .await
    }

    /// Abort the transfer with a protocol reason code.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn abort(&self, reason: f64) -> Result<()> {
        let reason = opt_u64(Some(reason), "reason")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "file transfer")?;
            guard
                .as_ref()
                .ok_or_else(|| tagged(CLOSED, "file transfer is closed"))?
                .abort(reason)
                .map_err(io_error)
        })
        .await
    }
}

#[napi]
impl Session {
    #[napi(ts_return_type = "Promise<FileDropGrantPayload>")]
    pub async fn set_file_drop_binding(
        &self,
        spec: FileDropBindingSpec,
    ) -> Result<FileDropGrantPayload> {
        let binding = binding_from_spec(spec)?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let grant = session
                .set_file_drop_binding(&binding, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(FileDropGrantPayload {
                producer_epoch: grant.producer_epoch.get() as f64,
                grant_generation: grant.grant_generation.get() as f64,
                context_id: grant.context_id as f64,
                surface_id: grant.surface_id as f64,
                surface_generation: grant.surface_generation.get() as f64,
                state: grant.state as u64 as f64,
                destination: grant.destination.map(|value| value as u64 as f64),
                maximum_file_bytes: grant.maximum_file_bytes as f64,
                maximum_pending_offers: grant.maximum_pending_offers as f64,
                maximum_active_transfers: grant.maximum_active_transfers as f64,
                maximum_record_body: grant.maximum_record_body as f64,
                acceptance_timeout_us: grant.acceptance_timeout_us as f64,
                idle_timeout_us: grant.idle_timeout_us as f64,
                reason: grant.reason as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<FileDropAcceptedPayload>")]
    pub async fn accept_file_drop(
        &self,
        binding: FileDropTupleSpec,
        transfer_id: f64,
        transfer_generation: f64,
        maximum_record_body: f64,
        initial_maximum_body_bytes: f64,
        initial_maximum_records: f64,
    ) -> Result<FileDropAcceptedPayload> {
        let acceptance = AcceptFileDrop {
            binding: tuple_from_spec(&binding)?,
            transfer_id: opt_u64(Some(transfer_id), "transferId")?,
            transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(opt_u64(
                Some(transfer_generation),
                "transferGeneration",
            )?),
            maximum_record_body: opt_u64(Some(maximum_record_body), "maximumRecordBody")? as u32,
            initial_maximum_body_bytes: opt_u64(
                Some(initial_maximum_body_bytes),
                "initialMaximumBodyBytes",
            )?,
            initial_maximum_records: opt_u64(
                Some(initial_maximum_records),
                "initialMaximumRecords",
            )?,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let accepted = session
                .accept_file_drop(acceptance, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(FileDropAcceptedPayload {
                drop_id: accepted.drop_id as f64,
                transfer_id: accepted.transfer_id as f64,
                transfer_generation: accepted.transfer_generation.get() as f64,
                open_timeout_us: accepted.open_timeout_us as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn cancel_file_drop(&self, binding: FileDropTupleSpec, reason: f64) -> Result<()> {
        let cancellation = CancelFileDrop {
            binding: tuple_from_spec(&binding)?,
            reason: opt_u64(Some(reason), "reason")?,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .cancel_file_drop(cancellation, &RequestMetadata::default())
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<FileTransferAdvancedPayload>")]
    pub async fn advance_file_transfer(
        &self,
        spec: FileTransferAdvanceSpec,
    ) -> Result<FileTransferAdvancedPayload> {
        let advance = AdvanceFileTransfer {
            context_id: opt_u64(Some(spec.context_id), "contextId")?,
            surface_id: opt_u64(Some(spec.surface_id), "surfaceId")?,
            drop_id: opt_u64(Some(spec.drop_id), "dropId")?,
            transfer_id: opt_u64(Some(spec.transfer_id), "transferId")?,
            expected_generation: vivid_protocol::revision::FileTransferGeneration::new(opt_u64(
                Some(spec.expected_generation),
                "expectedGeneration",
            )?),
            new_generation: vivid_protocol::revision::FileTransferGeneration::new(opt_u64(
                Some(spec.new_generation),
                "newGeneration",
            )?),
            committed_offset: opt_u64(Some(spec.committed_offset), "committedOffset")?,
            maximum_body_bytes: opt_u64(Some(spec.maximum_body_bytes), "maximumBodyBytes")?,
            maximum_records: opt_u64(Some(spec.maximum_records), "maximumRecords")?,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let advanced = session
                .advance_file_transfer(advance, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(FileTransferAdvancedPayload {
                transfer_id: advanced.transfer_id as f64,
                generation: advanced.generation.get() as f64,
                committed_offset: advanced.committed_offset as f64,
                open_timeout_us: advanced.open_timeout_us as f64,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<FileDropStatusPayload>")]
    pub async fn query_file_drop(&self, drop_id: f64) -> Result<FileDropStatusPayload> {
        let query = QueryFileDrop {
            drop_id: opt_u64(Some(drop_id), "dropId")?,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let status = session
                .query_file_drop(query, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(FileDropStatusPayload {
                drop_id: status.drop_id as f64,
                state: status.state as u64 as f64,
                transfer_id: status.transfer_id as f64,
                generation: status.generation.get() as f64,
                committed_offset: status.committed_offset as f64,
                result: status.result.map(|code| code as u64 as f64),
                final_name: status.final_name,
            })
        })
        .await
    }

    /// Take over an incoming transfer connection after `accept_file_drop` succeeds.
    #[napi(ts_return_type = "Promise<IncomingFileTransfer>")]
    pub async fn open_incoming_file_transfer(
        &self,
        spec: IncomingFileTransferSpec,
    ) -> Result<IncomingFileTransfer> {
        let request = IncomingFileTransferRequest {
            context_id: opt_u64(Some(spec.context_id), "contextId")?,
            surface_id: opt_u64(Some(spec.surface_id), "surfaceId")?,
            producer_epoch: vivid_protocol::revision::FileDropEpoch::new(opt_u64(
                Some(spec.producer_epoch),
                "producerEpoch",
            )?),
            grant_generation: vivid_protocol::revision::FileDropGrantGeneration::new(opt_u64(
                Some(spec.grant_generation),
                "grantGeneration",
            )?),
            surface_generation: vivid_protocol::revision::SurfaceGeneration::new(opt_u64(
                Some(spec.surface_generation),
                "surfaceGeneration",
            )?),
            drop_id: opt_u64(Some(spec.drop_id), "dropId")?,
            transfer_id: opt_u64(Some(spec.transfer_id), "transferId")?,
            transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(opt_u64(
                Some(spec.transfer_generation),
                "transferGeneration",
            )?),
            resume_offset: opt_u64(spec.resume_offset, "resumeOffset")?,
            declared_length: opt_u64(Some(spec.declared_length), "declaredLength")?,
            maximum_record_body: opt_u64(Some(spec.maximum_record_body), "maximumRecordBody")?
                as u32,
            maximum_body_bytes: opt_u64(Some(spec.maximum_body_bytes), "maximumBodyBytes")?,
            maximum_records: opt_u64(Some(spec.maximum_records), "maximumRecords")?,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            session
                .open_incoming_file_transfer(request)
                .map(|transfer| IncomingFileTransfer {
                    inner: Arc::new(Mutex::new(Some(transfer))),
                })
                .map_err(io_error)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Contexts, session leases, and resume
// ---------------------------------------------------------------------------

fn contract_to_array(contract: &ResourceContract) -> Vec<f64> {
    // The CBOR map's keys are the resource indices, which is exactly the array order.
    let mut values = vec![0_f64; RESOURCE_COUNT];
    if let Value::Map(entries) = contract.to_value() {
        for (key, value) in entries {
            if let (Ok(index), Some(number)) = (usize::try_from(key), value.as_u64())
                && index < RESOURCE_COUNT
            {
                values[index] = number as f64;
            }
        }
    }
    values
}

fn contract_from_array(values: Vec<f64>) -> Result<ResourceContract> {
    if values.len() != RESOURCE_COUNT {
        return Err(value_error(format!(
            "a contract array must name all {RESOURCE_COUNT} resources"
        )));
    }
    let mut raw = [0_u64; RESOURCE_COUNT];
    for (index, value) in values.iter().enumerate() {
        raw[index] = opt_u64(Some(*value), "contract entry")?;
    }
    Ok(ResourceContract::new(raw))
}

fn session_contract(inner: &Arc<Mutex<Option<SdkSession>>>) -> Result<ResourceContract> {
    let guard = locked(inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    Ok(session.info().resource_contract.clone())
}

#[napi(object)]
pub struct ContextReadyPayload {
    pub context_id: f64,
    pub operation_classes: f64,
    pub contract: Vec<f64>,
    pub lifetime_us: f64,
    pub revision: f64,
}

#[napi(object)]
pub struct SessionLeaseReadyPayload {
    pub context_id: f64,
    pub lease_id: f64,
    pub state: f64,
    pub activation_timeout_us: f64,
    pub disconnect_grace_us: f64,
    pub cleanup_policy: f64,
    pub permitted_profiles: Vec<String>,
    pub contract: Vec<f64>,
    pub revision: f64,
    /// The activation secret, handed exactly once. Capability material: keep it out of logs.
    pub activation_secret_hex: String,
}

#[napi]
impl Session {
    /// Create a child context scoped to the given operation classes and contract.
    #[napi(ts_return_type = "Promise<ContextReadyPayload>")]
    pub async fn create_context(
        &self,
        context_id: f64,
        parent_context_id: f64,
        operation_classes: f64,
        label: Option<String>,
        lifetime_us: Option<f64>,
        contract: Option<Vec<f64>>,
    ) -> Result<ContextReadyPayload> {
        let requested_contract = match contract {
            Some(values) => contract_from_array(values)?,
            None => session_contract(&self.inner)?,
        };
        let definition = ContextDefinition {
            context_id: opt_u64(Some(context_id), "contextId")?,
            parent_context_id: opt_u64(Some(parent_context_id), "parentContextId")?,
            operation_classes: opt_u64(Some(operation_classes), "operationClasses")?,
            label: label.unwrap_or_default(),
            lifetime_us: opt_u64(lifetime_us, "lifetimeUs")?,
            requested_contract,
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let ready = session
                .create_context(&definition, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(ContextReadyPayload {
                context_id: ready.context_id as f64,
                operation_classes: ready.operation_classes as f64,
                contract: contract_to_array(&ready.contract),
                lifetime_us: ready.lifetime_us as f64,
                revision: ready.revision as f64,
            })
        })
        .await
    }

    /// Mint a bounded session lease; the activation secret rides once, for the lease holder.
    #[napi(ts_return_type = "Promise<SessionLeaseReadyPayload>")]
    pub async fn create_session_lease(
        &self,
        context_id: f64,
        lease_id: f64,
        permitted_profiles: Vec<String>,
        activation_timeout_us: Option<f64>,
        disconnect_grace_us: Option<f64>,
        cleanup_policy: Option<f64>,
        contract: Option<Vec<f64>>,
    ) -> Result<SessionLeaseReadyPayload> {
        let requested_contract = match contract {
            Some(values) => contract_from_array(values)?,
            None => session_contract(&self.inner)?,
        };
        let (definition, mut secret) = SessionLeaseBuilder::new(
            opt_u64(Some(context_id), "contextId")?,
            opt_u64(Some(lease_id), "leaseId")?,
        )
        .permitted_profiles(permitted_profiles)
        .activation_timeout_us(opt_u64(activation_timeout_us, "activationTimeoutUs")?)
        .disconnect_grace_us(opt_u64(disconnect_grace_us, "disconnectGraceUs")?)
        .cleanup_policy(
            CleanupPolicy::try_from(opt_u64(cleanup_policy, "cleanupPolicy").unwrap_or(1))
                .map_err(|error| value_error(error.to_string()))?,
        )
        .contract(requested_contract)
        .build()
        .map_err(io_error)?;
        let activation_secret_hex = secret
            .take()
            .map(|value| {
                value
                    .expose()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            })
            .unwrap_or_default();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let ready = session
                .create_session_lease(&definition, &RequestMetadata::default())
                .map_err(io_error)?;
            Ok(SessionLeaseReadyPayload {
                context_id: ready.context_id as f64,
                lease_id: ready.lease_id as f64,
                state: ready.state as f64,
                activation_timeout_us: ready.activation_timeout_us as f64,
                disconnect_grace_us: ready.disconnect_grace_us as f64,
                cleanup_policy: ready.cleanup_policy as f64,
                permitted_profiles: ready.permitted_profiles,
                contract: contract_to_array(&ready.contract),
                revision: ready.revision as f64,
                activation_secret_hex,
            })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn revoke_session_lease(&self, context_id: f64, lease_id: f64) -> Result<()> {
        let context_id = opt_u64(Some(context_id), "contextId")?;
        let lease_id = opt_u64(Some(lease_id), "leaseId")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .revoke_session_lease(context_id, lease_id, &RequestMetadata::default())
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub async fn set_observation(&self, mask: f64) -> Result<()> {
        let mask = opt_u64(Some(mask), "mask")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .set_observation(mask)
                .map_err(io_error)
        })
        .await
    }

    /// Prepare resumable authentication identity for a leased session. The resume key itself
    /// never crosses this boundary; the returned identity plus the secret held by the caller
    /// feed `connect` on the resuming side.
    #[napi(ts_return_type = "Promise<ResumeIdentityPayload>")]
    pub async fn prepare_resume(&self) -> Result<ResumeIdentityPayload> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "session")?;
            let session = guard.as_ref().ok_or_else(closed_session)?;
            let info = session.info();
            Ok(ResumeIdentityPayload {
                context_id: info.root_context_id as f64,
                lease_id: info.session_id as f64,
                session_id: info.session_id as f64,
                resume_generation: info.resume_generation as f64,
            })
        })
        .await
    }
}

#[napi(object)]
pub struct ResumeIdentityPayload {
    pub context_id: f64,
    pub lease_id: f64,
    pub session_id: f64,
    pub resume_generation: f64,
}

// ---------------------------------------------------------------------------
// Track sender, recovery, rate control, and microphone uplink
// ---------------------------------------------------------------------------

/// A video sender that owns packet-ID and epoch continuity across channel recovery.
#[napi]
pub struct TrackSender {
    inner: Arc<Mutex<Option<TrackSender_>>>,
}

type TrackSender_ = vivid_sdk::TrackSender;

/// One video packet; `packetId`/`epoch` default to the sender's continuity.
#[napi(object)]
pub struct VideoSendSpec {
    pub data: Buffer,
    pub packet_id: Option<f64>,
    pub pts_us: f64,
    pub dts_us: Option<f64>,
    pub duration_us: Option<f64>,
    pub key: Option<bool>,
    pub epoch: Option<f64>,
}

/// One audio packet with the same continuity guarantees.
#[napi(object)]
pub struct AudioSendSpec {
    pub data: Buffer,
    pub packet_id: Option<f64>,
    pub pts_us: f64,
    pub duration_us: f64,
    pub epoch: Option<f64>,
}

#[napi]
impl TrackSender {
    #[napi(getter)]
    pub fn generation(&self) -> Result<f64> {
        let guard = locked(&self.inner, "track sender")?;
        Ok(guard
            .as_ref()
            .ok_or_else(closed_channel)?
            .generation()
            .get() as f64)
    }

    #[napi(getter)]
    pub fn detached(&self) -> Result<bool> {
        let guard = locked(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.is_detached())
    }

    #[napi]
    pub fn next_packet_id(&self) -> Result<f64> {
        let guard = locked(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.next_packet_id() as f64)
    }

    #[napi]
    pub fn current_epoch(&self) -> Result<f64> {
        let guard = locked(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.current_epoch() as f64)
    }

    #[napi]
    pub fn bump_epoch(&self) -> Result<f64> {
        let guard = locked(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.bump_epoch() as f64)
    }

    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_video(&self, spec: VideoSendSpec) -> Result<f64> {
        let data = spec.data.to_vec();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track sender")?;
            let sender = guard.as_ref().ok_or_else(closed_channel)?;
            let packet_id = match spec.packet_id {
                Some(value) => opt_u64(Some(value), "packetId")?,
                None => sender.next_packet_id(),
            };
            let epoch = match spec.epoch {
                Some(value) => opt_u64(Some(value), "epoch")? as u32,
                None => sender.current_epoch(),
            };
            sender
                .send(&EncodedPacket::Video(VideoPacketData {
                    epoch,
                    packet_id,
                    pts_us: spec.pts_us as i64,
                    dts_us: spec
                        .dts_us
                        .map(|value| value as i64)
                        .unwrap_or(spec.pts_us as i64),
                    duration_us: opt_u64(spec.duration_us, "durationUs")?,
                    key: spec.key.unwrap_or(false),
                    data,
                }))
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_audio(&self, spec: AudioSendSpec) -> Result<f64> {
        let data = spec.data.to_vec();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "track sender")?;
            let sender = guard.as_ref().ok_or_else(closed_channel)?;
            let packet_id = match spec.packet_id {
                Some(value) => opt_u64(Some(value), "packetId")?,
                None => sender.next_packet_id(),
            };
            let epoch = match spec.epoch {
                Some(value) => opt_u64(Some(value), "epoch")? as u32,
                None => sender.current_epoch(),
            };
            sender
                .send(&EncodedPacket::Audio(AudioPacketData {
                    epoch,
                    packet_id,
                    pts_us: spec.pts_us as i64,
                    dts_us: spec.pts_us as i64,
                    duration_us: opt_u64(Some(spec.duration_us), "durationUs")?,
                    data,
                }))
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Drop the sender so a replacement generation can be opened without closing the channel
    /// mid-send.
    #[napi]
    pub fn detach(&self) -> Result<()> {
        let guard = locked(&self.inner, "track sender")?;
        if let Some(sender) = guard.as_ref() {
            sender.detach();
        }
        Ok(())
    }
}

#[napi]
impl Session {
    /// Recover a lost channel: advance, reopen, and send the key unit, keeping packet IDs and
    /// the epoch continuous. The returned sender continues the same media sequence.
    #[napi(ts_return_type = "Promise<TrackSender>")]
    pub async fn recover_channel(&self, track: &Track, key_unit: Buffer) -> Result<TrackSender> {
        let inner = Arc::clone(&self.inner);
        let handle = track.inner.clone();
        let key_unit = key_unit.to_vec();
        blocking(move || {
            let mut guard = locked(&inner, "session")?;
            let session = guard.as_mut().ok_or_else(closed_session)?;
            let sender =
                vivid_sdk::recover_channel(session, &handle, &key_unit).map_err(io_error)?;
            Ok(TrackSender {
                inner: Arc::new(Mutex::new(Some(sender))),
            })
        })
        .await
    }

    /// Grant the presenter's microphone flow, so it can send PCM on the reverse channel.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn grant_audio_input(&self, channel: &TrackChannel) -> Result<()> {
        let inner = Arc::clone(&channel.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .grant_audio_input()
                .map_err(io_error)
        })
        .await
    }

    /// Take one microphone PCM packet, if the presenter has produced one.
    #[napi(ts_return_type = "Promise<MicPacketPayload | null>")]
    pub async fn take_audio_input(
        &self,
        channel: &TrackChannel,
    ) -> Result<Option<MicPacketPayload>> {
        let inner = Arc::clone(&channel.inner);
        blocking(move || {
            let guard = locked(&inner, "track channel")?;
            let packet = guard
                .as_ref()
                .ok_or_else(closed_channel)?
                .take_audio_input()
                .map_err(io_error)?;
            Ok(packet.map(|packet| MicPacketPayload {
                epoch: packet.epoch as f64,
                packet_id: packet.packet_id as f64,
                pts_us: packet.pts_us as f64,
                pcm: Vec::from(packet.pcm).into(),
            }))
        })
        .await
    }
}

#[napi(object)]
pub struct MicPacketPayload {
    pub epoch: f64,
    pub packet_id: f64,
    pub pts_us: f64,
    /// Exactly `PCM_BYTES` (960) bytes of little-endian s16LE mono at 48 kHz, 20 ms.
    pub pcm: Buffer,
}

/// Producer-side encoder pacing fed by `SendPressure` observations.
#[napi]
pub struct VideoRateControl {
    inner: Mutex<VideoRateControl_>,
}

type VideoRateControl_ = vivid_sdk::VideoRateControl;

#[napi(object)]
pub struct RateSnapshotPayload {
    pub configured_bits_per_second: f64,
    pub target_bits_per_second: f64,
    pub adjustments: f64,
    pub rate_limited_us: f64,
    pub flow_limited_us: f64,
    pub transport_us: f64,
}

#[napi]
impl VideoRateControl {
    #[napi(constructor)]
    pub fn new(configured_bits_per_second: f64) -> Result<Self> {
        Ok(Self {
            inner: Mutex::new(VideoRateControl_::new(opt_u64(
                Some(configured_bits_per_second),
                "configuredBitsPerSecond",
            )?)),
        })
    }

    /// Feed the pressure from each sent record; the three causes adjust the target differently.
    #[napi]
    pub fn observe_send(
        &self,
        bytes: f64,
        rate_limited_us: f64,
        flow_limited_us: f64,
        transport_us: f64,
        records: f64,
    ) -> Result<()> {
        let pressure = SendPressure {
            rate_limited: Duration::from_micros(opt_u64(Some(rate_limited_us), "rateLimitedUs")?),
            flow_limited: Duration::from_micros(opt_u64(Some(flow_limited_us), "flowLimitedUs")?),
            transport: Duration::from_micros(opt_u64(Some(transport_us), "transportUs")?),
            records: opt_u64(Some(records), "records")?,
        };
        let guard = locked(&self.inner, "rate control")?;
        guard.observe_send(bytes as usize, pressure);
        Ok(())
    }

    #[napi]
    pub fn observe_audio_backlog(&self, backlog_us: f64) -> Result<()> {
        let backlog_us = opt_u64(Some(backlog_us), "backlogUs")?;
        let guard = locked(&self.inner, "rate control")?;
        guard.observe_audio_backlog(backlog_us);
        Ok(())
    }

    /// The encoder target, if it changed since the last poll.
    #[napi]
    pub fn poll(&self) -> Option<f64> {
        let guard = locked(&self.inner, "rate control").ok()?;
        guard.poll().map(|target| target as f64)
    }

    #[napi]
    pub fn snapshot(&self) -> Result<RateSnapshotPayload> {
        let guard = locked(&self.inner, "rate control")?;
        let snapshot = guard.snapshot();
        Ok(RateSnapshotPayload {
            configured_bits_per_second: snapshot.configured_bits_per_second as f64,
            target_bits_per_second: snapshot.target_bits_per_second as f64,
            adjustments: snapshot.adjustments as f64,
            rate_limited_us: snapshot.rate_limited.as_micros() as f64,
            flow_limited_us: snapshot.flow_limited.as_micros() as f64,
            transport_us: snapshot.transport.as_micros() as f64,
        })
    }
}

// ---------------------------------------------------------------------------
// Orchestrators: PaneSession and DesktopSession
// ---------------------------------------------------------------------------

/// The one-image-per-pane convenience session, over real discovery. The producer name is
/// `vivid-sdk-node` and authentication comes from the standard environment.
#[napi]
pub struct PaneSession {
    inner: Arc<Mutex<Option<SdkPaneSession>>>,
}

#[napi]
impl PaneSession {
    /// Connect through the standard Vivid discovery environment.
    #[napi(factory)]
    pub async fn connect() -> Result<PaneSession> {
        blocking(move || SdkPaneSession::from_env().map_err(io_error))
            .await
            .map(|pane| PaneSession {
                inner: Arc::new(Mutex::new(Some(pane))),
            })
    }

    /// Wrap an existing session; the session is consumed.
    #[napi(factory, ts_return_type = "Promise<PaneSession>")]
    pub async fn from_session(session: &Session) -> Result<PaneSession> {
        let owned = locked(&session.inner, "session")?
            .take()
            .ok_or_else(closed_session)?;
        blocking(move || SdkPaneSession::from_session(owned).map_err(io_error))
            .await
            .map(|pane| PaneSession {
                inner: Arc::new(Mutex::new(Some(pane))),
            })
    }

    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "pane session")?.is_none())
    }

    /// Present one encoded PNG or JPEG, replacing any current presentation.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn show_encoded_image(
        &self,
        encoded: Buffer,
        title: Option<String>,
        columns: Option<f64>,
        rows: Option<f64>,
        text_layer: Option<f64>,
    ) -> Result<()> {
        let options = PaneImageOptions {
            title: title.unwrap_or_else(|| "image".into()),
            columns: columns.map(|value| value as u32),
            rows: rows.map(|value| value as u32),
            text_layer: text_layer
                .map(|value| opt_u64(Some(value), "textLayer").unwrap_or(1))
                .unwrap_or(1),
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "pane session")?;
            let pane = guard
                .as_mut()
                .ok_or_else(|| tagged(CLOSED, "pane session is closed"))?;
            pane.show_encoded_image_with_options(&encoded, &options)
                .map_err(io_error)
        })
        .await
    }

    /// Present one RGBA frame, replacing any current presentation.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn show_rgba(
        &self,
        width: f64,
        height: f64,
        rgba: Buffer,
        title: Option<String>,
        columns: Option<f64>,
        rows: Option<f64>,
        text_layer: Option<f64>,
    ) -> Result<()> {
        let width = opt_u64(Some(width), "width")? as u32;
        let height = opt_u64(Some(height), "height")? as u32;
        let options = PaneImageOptions {
            title: title.unwrap_or_else(|| "image".into()),
            columns: columns.map(|value| value as u32),
            rows: rows.map(|value| value as u32),
            text_layer: text_layer
                .map(|value| opt_u64(Some(value), "textLayer").unwrap_or(1))
                .unwrap_or(1),
        };
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "pane session")?;
            let pane = guard
                .as_mut()
                .ok_or_else(|| tagged(CLOSED, "pane session is closed"))?;
            pane.show_rgba_with_options(width, height, &rgba, &options)
                .map_err(io_error)
        })
        .await
    }

    /// Remove the current presentation without closing the session.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn clear(&self) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "pane session")?;
            guard
                .as_mut()
                .ok_or_else(|| tagged(CLOSED, "pane session is closed"))?
                .clear()
                .map_err(io_error)
        })
        .await
    }

    /// Close the presentation and the underlying session.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn close(&self) -> Result<()> {
        let pane = locked(&self.inner, "pane session")?
            .take()
            .ok_or_else(|| tagged(CLOSED, "pane session is closed"))?;
        blocking(move || pane.close().map_err(io_error)).await
    }
}

/// The desktop presentation orchestrator: surface, node, video and audio senders, and an input
/// lane when the profile is accepted. Takes ownership of the session.
#[napi]
pub struct DesktopSession {
    inner: Arc<Mutex<Option<SdkDesktopSession>>>,
}

/// Establish a desktop presentation from a live session; the session is consumed. Configs are
/// resolved against the owned session, then everything moves into the orchestrator.
#[napi]
pub async fn establish_desktop(
    session: &Session,
    surface_config: SurfaceConfig,
    video_config: TrackConfig,
    audio_config: Option<TrackConfig>,
) -> Result<DesktopSession> {
    let owned = locked(&session.inner, "session")?
        .take()
        .ok_or_else(closed_session)?;
    let surface_definition = surface_definition(&surface_config, &owned)?;
    let video_configuration = track_configuration(
        &video_config,
        &owned,
        surface_definition.context_id,
        surface_definition.surface_id,
    )?;
    let audio_configuration = match &audio_config {
        Some(config) => Some(track_configuration(
            config,
            &owned,
            surface_definition.context_id,
            surface_definition.surface_id,
        )?),
        None => None,
    };
    blocking(move || {
        SdkDesktopSession::establish(
            owned,
            surface_definition,
            video_configuration,
            audio_configuration,
        )
        .map(|inner| DesktopSession {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
        .map_err(io_error)
    })
    .await
}

#[napi]
impl DesktopSession {
    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "desktop session")?.is_none())
    }

    /// The video track handle, for milestone waits and queries.
    #[napi(ts_return_type = "Promise<Track>")]
    pub async fn video_track(&self) -> Result<Track> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "desktop session")?;
            let desktop = guard.as_ref().ok_or_else(closed_session)?;
            Ok(Track {
                inner: desktop.video_track().clone(),
            })
        })
        .await
    }

    /// The audio track handle, when audio was established.
    #[napi(ts_return_type = "Promise<Track | null>")]
    pub async fn audio_track(&self) -> Result<Option<Track>> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "desktop session")?;
            let desktop = guard.as_ref().ok_or_else(closed_session)?;
            Ok(desktop.audio_track().map(|track| Track {
                inner: track.clone(),
            }))
        })
        .await
    }

    /// Send one video access unit; `packetId`/`epoch` default to the sender's continuity.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_video(&self, spec: VideoSendSpec) -> Result<f64> {
        let data = spec.data.to_vec();
        let packet_id = spec.packet_id;
        let pts_us = spec.pts_us as i64;
        let dts_us = spec.dts_us.map(|value| value as i64);
        let duration_us = opt_u64(spec.duration_us, "durationUs")?;
        let key = spec.key.unwrap_or(false);
        let epoch = spec.epoch;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "desktop session")?;
            let desktop = guard.as_mut().ok_or_else(closed_session)?;
            let sender = desktop.video_sender();
            let packet_id = match packet_id {
                Some(value) => opt_u64(Some(value), "packetId")?,
                None => sender.next_packet_id(),
            };
            let epoch = match epoch {
                Some(value) => opt_u64(Some(value), "epoch")? as u32,
                None => sender.current_epoch(),
            };
            sender
                .send(&EncodedPacket::Video(VideoPacketData {
                    epoch,
                    packet_id,
                    pts_us,
                    dts_us: dts_us.unwrap_or(pts_us),
                    duration_us,
                    key,
                    data,
                }))
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Send one audio access unit through the audio sender.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn send_audio(&self, spec: AudioSendSpec) -> Result<f64> {
        let data = spec.data.to_vec();
        let packet_id = opt_u64(Some(spec.packet_id.unwrap_or(0.0)), "packetId")?;
        let pts_us = spec.pts_us as i64;
        let duration_us = opt_u64(Some(spec.duration_us), "durationUs")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "desktop session")?;
            let desktop = guard.as_mut().ok_or_else(closed_session)?;
            let sender = desktop
                .audio_sender()
                .ok_or_else(|| value_error("this desktop session has no audio track"))?;
            sender
                .send(&EncodedPacket::Audio(AudioPacketData {
                    epoch: sender.current_epoch(),
                    packet_id,
                    pts_us,
                    dts_us: pts_us,
                    duration_us,
                    data,
                }))
                .map(|sequence| sequence as f64)
                .map_err(io_error)
        })
        .await
    }

    /// Wait for output readiness and atomically activate the video and audio slots.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn activate_slots(&self) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let mut guard = locked(&inner, "desktop session")?;
            guard
                .as_mut()
                .ok_or_else(closed_session)?
                .activate_slots()
                .map_err(io_error)
        })
        .await
    }

    /// Close the presentation and the underlying session.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn close(&self) -> Result<()> {
        let desktop = locked(&self.inner, "desktop session")?
            .take()
            .ok_or_else(closed_session)?;
        blocking(move || desktop.close().map_err(io_error)).await
    }
}

// ---------------------------------------------------------------------------
// Presenter
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct PresenterStartOptions {
    /// `unix:/absolute/path` or `tcp:host:port`, loopback only; port 0 is ephemeral.
    pub endpoint: String,
    /// Present a desktop target of this size instead of a terminal target.
    pub desktop_width: Option<f64>,
    pub desktop_height: Option<f64>,
    pub aggregate_retained_bytes: Option<f64>,
}

#[napi]
pub struct Presenter {
    inner: Arc<Mutex<Option<VirtualVivid>>>,
}

#[napi(object)]
pub struct SourceKeyPayload {
    pub producer: f64,
    pub context: f64,
    pub surface: f64,
    pub track: f64,
}

fn source_key_from(key: &SourceKeyPayload) -> vivid_sdk::presenter::SourceKey {
    vivid_sdk::presenter::SourceKey {
        producer: key.producer as u64,
        context: key.context as u64,
        surface: key.surface as u64,
        track: key.track as u64,
    }
}

fn source_key(key: &vivid_sdk::presenter::SourceKey) -> SourceKeyPayload {
    SourceKeyPayload {
        producer: key.producer as f64,
        context: key.context as f64,
        surface: key.surface as f64,
        track: key.track as f64,
    }
}

#[napi(object)]
pub struct CaptureLayerPayload {
    pub source: SourceKeyPayload,
    pub node_id: f64,
    pub z_index: f64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub clip: Option<ClipRectPayload>,
    /// `raster` or `encodedImage`.
    pub content_kind: String,
    pub epoch: Option<f64>,
    pub frame_id: Option<f64>,
    pub raster_width: Option<f64>,
    pub raster_height: Option<f64>,
    pub pixels: Option<Buffer>,
    pub encoded_image: Option<Buffer>,
}

#[napi(object)]
pub struct ClipRectPayload {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[napi(object)]
pub struct SkippedSourcePayload {
    pub source: SourceKeyPayload,
    pub node_id: f64,
    pub reason: String,
}

#[napi(object)]
pub struct PaneCapturePayload {
    pub layers: Vec<CaptureLayerPayload>,
    pub skipped: Vec<SkippedSourcePayload>,
}

#[napi(object)]
pub struct PaneTrackSummaryPayload {
    pub source: SourceKeyPayload,
    pub kind: String,
    pub capturable: bool,
}

#[napi(object)]
pub struct PaneMediaSummaryPayload {
    pub surfaces: Vec<String>,
    pub tracks: Vec<PaneTrackSummaryPayload>,
}

#[napi]
impl Presenter {
    #[napi(getter, js_name = "closed")]
    pub fn closed(&self) -> Result<bool> {
        Ok(locked(&self.inner, "presenter")?.is_none())
    }

    #[napi]
    pub fn endpoint(&self) -> Result<String> {
        let guard = locked(&self.inner, "presenter")?;
        Ok(guard.as_ref().ok_or_else(closed_presenter)?.endpoint())
    }

    /// Mint a pane capability; the secret is returned once and never logged.
    #[napi]
    pub fn issue_pane_capability(&self, pane: f64) -> Result<String> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .issue_pane_capability(pane)
            .map_err(io_error)
    }

    #[napi]
    pub fn revoke_pane(&self, pane: f64) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .revoke_pane(pane);
        Ok(())
    }

    #[napi]
    pub fn update_metrics(
        &self,
        pane: f64,
        columns: f64,
        rows: f64,
        cell_width: f64,
        cell_height: f64,
    ) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let columns = opt_u64(Some(columns), "columns")? as u16;
        let rows = opt_u64(Some(rows), "rows")? as u16;
        let cell_width = opt_u64(Some(cell_width), "cellWidth")? as u16;
        let cell_height = opt_u64(Some(cell_height), "cellHeight")? as u16;
        let guard = locked(&self.inner, "presenter")?;
        guard.as_ref().ok_or_else(closed_presenter)?.update_metrics(
            pane,
            columns,
            rows,
            (cell_width, cell_height),
        );
        Ok(())
    }

    /// Wait for retained media to arrive, bounded, on a worker thread.
    #[napi(ts_return_type = "Promise<boolean>")]
    pub async fn wait_for_media(&self, pane: f64, timeout_ms: f64) -> Result<bool> {
        let pane = opt_u64(Some(pane), "pane")?;
        let timeout = duration_ms(timeout_ms, "timeoutMs")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "presenter")?;
            let waited = guard
                .as_ref()
                .ok_or_else(closed_presenter)?
                .wait_for_retained_media(pane, timeout);
            Ok(waited)
        })
        .await
    }

    /// Read a pane's media back without its presentation state.
    ///
    /// `viewport_offset` is the scrollback row the capture starts at, exactly as in Python.
    #[napi(ts_return_type = "Promise<PaneCapturePayload>")]
    pub async fn capture_pane(
        &self,
        pane: f64,
        viewport_offset: Option<f64>,
    ) -> Result<PaneCapturePayload> {
        let pane = opt_u64(Some(pane), "pane")?;
        let viewport_offset = opt_u64(viewport_offset, "viewportOffset")? as usize;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "presenter")?;
            let capture = guard
                .as_ref()
                .ok_or_else(closed_presenter)?
                .capture_pane(pane, viewport_offset);
            let layers = capture
                .layers
                .iter()
                .map(|layer| {
                    let (
                        content_kind,
                        epoch,
                        frame_id,
                        raster_width,
                        raster_height,
                        pixels,
                        encoded_image,
                    ) = match &layer.content {
                        CaptureContent::Raster(raster) => (
                            "raster",
                            Some(raster.epoch as f64),
                            Some(raster.frame_id as f64),
                            Some(raster.width as f64),
                            Some(raster.height as f64),
                            Some(raster.pixels.to_vec().into()),
                            None,
                        ),
                        CaptureContent::EncodedImage(image) => (
                            "encodedImage",
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(image.to_vec().into()),
                        ),
                    };
                    Ok(CaptureLayerPayload {
                        source: source_key(&layer.source),
                        node_id: layer.node_id as f64,
                        z_index: layer.z_index as f64,
                        x: layer.x as f64,
                        y: layer.y as f64,
                        width: layer.width as f64,
                        height: layer.height as f64,
                        clip: layer.clip.map(|clip| ClipRectPayload {
                            x: clip.x as f64,
                            y: clip.y as f64,
                            width: clip.width as f64,
                            height: clip.height as f64,
                        }),
                        content_kind: content_kind.to_owned(),
                        epoch,
                        frame_id,
                        raster_width,
                        raster_height,
                        pixels,
                        encoded_image,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let skipped = capture
                .skipped
                .iter()
                .map(|skip| SkippedSourcePayload {
                    source: source_key(&skip.source),
                    node_id: skip.node_id as f64,
                    reason: skip.reason.as_str().to_owned(),
                })
                .collect();
            Ok(PaneCapturePayload { layers, skipped })
        })
        .await
    }

    #[napi(ts_return_type = "Promise<PaneMediaSummaryPayload>")]
    pub async fn pane_media_summary(&self, pane: f64) -> Result<PaneMediaSummaryPayload> {
        let pane = opt_u64(Some(pane), "pane")?;
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "presenter")?;
            let summary = guard
                .as_ref()
                .ok_or_else(closed_presenter)?
                .pane_media_summary(pane);
            Ok(PaneMediaSummaryPayload {
                surfaces: summary.surfaces.clone(),
                tracks: summary
                    .tracks
                    .iter()
                    .map(|track| PaneTrackSummaryPayload {
                        source: source_key(&track.source),
                        kind: track.kind.to_owned(),
                        capturable: track.capturable,
                    })
                    .collect(),
            })
        })
        .await
    }

    /// Shut the presenter down, removing a `unix:` endpoint it created.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn close(&self) -> Result<()> {
        let presenter = locked(&self.inner, "presenter")?
            .take()
            .ok_or_else(closed_presenter)?;
        // Dropping on a worker matches the Python binding: the presenter's threads may be
        // mid-write, and the JS thread must not wait on them.
        blocking(move || {
            drop(presenter);
            Ok(())
        })
        .await
    }

    /// Deliver one microphone packet from the pane app; empty bytes revoke the microphone.
    #[napi(ts_return_type = "Promise<boolean>")]
    pub async fn queue_microphone(
        &self,
        source: SourceKeyPayload,
        generation: f64,
        bytes: Buffer,
    ) -> Result<bool> {
        let generation = opt_u64(Some(generation), "generation")?;
        let bytes = bytes.to_vec();
        let inner = Arc::clone(&self.inner);
        blocking(move || {
            let guard = locked(&inner, "presenter")?;
            guard
                .as_ref()
                .ok_or_else(closed_presenter)?
                .queue_microphone(source_key_from(&source), generation, &bytes)
                .map_err(io_error)
        })
        .await
    }

    #[napi]
    pub fn revoke_microphones(&self) -> Result<()> {
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .revoke_microphones();
        Ok(())
    }

    /// Tell the pane app the capability set changed; returns the new generation.
    #[napi]
    pub fn notify_capabilities_changed(&self, reason_mask: f64) -> Result<f64> {
        let reason_mask = opt_u64(Some(reason_mask), "reasonMask")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .notify_capabilities_changed(reason_mask)
            .map(|generation| generation as f64)
            .map_err(io_error)
    }

    /// Replace the reported desktop target; requires a desktop presenter.
    #[napi]
    pub fn update_desktop_target(
        &self,
        pane: f64,
        width: f64,
        height: f64,
        reason_mask: f64,
    ) -> Result<f64> {
        let pane = opt_u64(Some(pane), "pane")?;
        let width = opt_u64(Some(width), "width")? as u32;
        let height = opt_u64(Some(height), "height")? as u32;
        let reason_mask = opt_u64(Some(reason_mask), "reasonMask")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .update_desktop_target(pane, desktop_target(width, height), reason_mask)
            .map(|generation| generation as f64)
            .map_err(io_error)
    }

    #[napi]
    pub fn observe_marker(
        &self,
        pane: f64,
        value: String,
        row: f64,
        column: f64,
        alternate: bool,
    ) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard.as_ref().ok_or_else(closed_presenter)?.observe_marker(
            pane,
            &value,
            row as i32,
            column as usize,
            alternate,
        );
        Ok(())
    }

    #[napi]
    pub fn scroll_anchors(&self, pane: f64, lines: f64, alternate: bool) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .scroll_anchors(pane, lines as i32, alternate);
        Ok(())
    }

    #[napi]
    pub fn clear_anchors(&self, pane: f64, alternate: bool) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .clear_anchors(pane, alternate);
        Ok(())
    }

    #[napi]
    pub fn set_alternate_screen(&self, pane: f64, alternate: bool) -> Result<()> {
        let pane = opt_u64(Some(pane), "pane")?;
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .set_alternate_screen(pane, alternate);
        Ok(())
    }

    /// Which pane a projected source landed on, if any.
    #[napi]
    pub fn pane_for_source(&self, source: SourceKeyPayload) -> Result<Option<f64>> {
        let guard = locked(&self.inner, "presenter")?;
        Ok(guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .pane_for_source(source_key_from(&source))
            .map(|pane| pane as f64))
    }

    #[napi(getter)]
    pub fn projection_revision(&self) -> Result<f64> {
        let guard = locked(&self.inner, "presenter")?;
        Ok(guard.as_ref().ok_or_else(closed_presenter)?.revision() as f64)
    }

    /// Request a keyframe; the outcome names whether it was forwarded, damped, or ignored.
    #[napi]
    pub fn request_keyframe(
        &self,
        source: SourceKeyPayload,
        minimum_epoch: Option<f64>,
        reason: f64,
    ) -> Result<String> {
        let reason = opt_u64(Some(reason), "reason")?;
        let minimum_epoch = match opt_u64(minimum_epoch, "minimumEpoch")? {
            0 => None,
            value => Some(value as u32),
        };
        let guard = locked(&self.inner, "presenter")?;
        let outcome = guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .request_keyframe(source_key_from(&source), minimum_epoch, reason);
        Ok(match outcome {
            vivid_sdk::presenter::KeyframeRequestOutcome::Forwarded => "forwarded".into(),
            vivid_sdk::presenter::KeyframeRequestOutcome::Damped => "damped".into(),
            vivid_sdk::presenter::KeyframeRequestOutcome::Ignored => "ignored".into(),
        })
    }

    #[napi]
    pub fn request_full_frames(&self, sources: Vec<SourceKeyPayload>, reason: f64) -> Result<()> {
        let reason = opt_u64(Some(reason), "reason")?;
        let keys = sources.iter().map(source_key_from).collect::<Vec<_>>();
        let guard = locked(&self.inner, "presenter")?;
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .request_full_frames(&keys, reason);
        Ok(())
    }

    #[napi]
    pub fn apply_outer_position(&self, source: SourceKeyPayload, position: PositionSnapshotSpec) {
        let Ok(guard) = locked(&self.inner, "presenter") else {
            return;
        };
        let Some(presenter) = guard.as_ref() else {
            return;
        };
        presenter.apply_outer_position(
            source_key_from(&source),
            vivid_sdk::presenter::BridgePositionSnapshot {
                decoder_reset_serial: position.decoder_reset_serial as u64,
                playing: position.playing,
                start_pts_us: position.start_pts_us as i64,
                state: position.state as u64,
                clock_pts_us: position.clock_pts_us.map(|value| value as i64),
                decoded_pts_us: position.decoded_pts_us as i64,
                presented_pts_us: position.presented_pts_us as i64,
                presentation_id: position.presentation_id as u64,
            },
        );
    }

    #[napi]
    pub fn apply_outer_playback(
        &self,
        source: SourceKeyPayload,
        decoder_reset_serial: f64,
        state_value: f64,
        eos_state: f64,
    ) {
        let Ok(guard) = locked(&self.inner, "presenter") else {
            return;
        };
        let Some(presenter) = guard.as_ref() else {
            return;
        };
        presenter.apply_outer_playback(
            source_key_from(&source),
            decoder_reset_serial as u64,
            state_value as u64,
            eos_state as u64,
        );
    }

    /// Mint a media resource id: `pinned` freezes the content, `live` follows the surface.
    #[napi]
    pub fn announce_media_resource(
        &self,
        source: SourceKeyPayload,
        pinned: bool,
    ) -> Result<String> {
        let guard = locked(&self.inner, "presenter")?;
        let binding = if pinned {
            Binding::Pinned
        } else {
            Binding::Live
        };
        guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .announce_media_resource(source_key_from(&source), binding)
            .map_err(|error| tagged(VIVID, error.to_string()))
    }

    #[napi]
    pub fn describe_media_resource(&self, id: String) -> Result<MediaResourceInfo> {
        let guard = locked(&self.inner, "presenter")?;
        let description = guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .describe_media_resource(&id)
            .map_err(|error| tagged(VIVID, error.to_string()))?;
        Ok(MediaResourceInfo {
            binding_pinned: matches!(description.binding, Binding::Pinned),
            producer: description.producer as f64,
            context: description.context_id as f64,
            surface: description.surface_id as f64,
            surface_revision: description.surface_revision as f64,
            surface_generation: description.surface_generation as f64,
            track_id: description
                .track
                .as_ref()
                .map(|facts| facts.track_id as f64),
            track_revision: description
                .track
                .as_ref()
                .map(|facts| facts.revision as f64),
            track_channel_generation: description
                .track
                .as_ref()
                .map(|facts| facts.channel_generation as f64),
            track_media_epoch: description
                .track
                .as_ref()
                .map(|facts| facts.media_epoch as f64),
            capturable: description.track.as_ref().map(|facts| facts.capturable),
        })
    }

    /// Whether the resource existed and was released.
    #[napi]
    pub fn release_media_resource(&self, id: String) -> Result<bool> {
        let guard = locked(&self.inner, "presenter")?;
        Ok(guard
            .as_ref()
            .ok_or_else(closed_presenter)?
            .release_media_resource(&id))
    }
}

/// The Python binding reads the presenter behind `Mutex<Option<VirtualVivid>>` from multiple
/// threads by locking; here the same lock guards it, and the unsafe exists only to move an
/// `Arc`-shared view of that same mutex into a worker. `VirtualVivid` is `Send + Sync` — its
/// state is a mutex-protected runtime with its own threads.
/// The single-output, 1:1, primary desktop target — the same starter topology the Python
/// binding fabricates for `presenter_start(desktop=(w, h))`.
fn desktop_target(width: u32, height: u32) -> vivid_protocol::target::DesktopTarget {
    vivid_protocol::target::DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width,
        height,
        settled: true,
        topology_revision: 1,
        outputs: vec![vivid_protocol::target::OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width,
            height,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: vivid_protocol::geometry::Rotation::None,
            primary: true,
        }],
    }
}

/// Start a presenter. Blocking work runs on a worker thread.
#[napi]
pub async fn presenter_start(options: PresenterStartOptions) -> Result<Presenter> {
    let endpoint = options.endpoint.clone();
    let desktop = match (options.desktop_width, options.desktop_height) {
        (Some(width), Some(height)) => Some((
            opt_u64(Some(width), "desktopWidth")? as u32,
            opt_u64(Some(height), "desktopHeight")? as u32,
        )),
        (None, None) => None,
        _ => return Err(value_error("desktop target needs both width and height")),
    };
    let retained = opt_u64(options.aggregate_retained_bytes, "aggregateRetainedBytes")?;
    blocking(move || {
        let listener = SocketListener::bind(&endpoint).map_err(io_error)?;
        let mut media = MediaConfig::default();
        if retained > 0 {
            media.aggregate_retained_bytes = retained;
        }
        let config = match desktop {
            Some((width, height)) => PresenterConfig::desktop(media, desktop_target(width, height)),
            None => PresenterConfig::terminal(media),
        };
        VirtualVivid::start_configured(listener, config, None)
            .map(|inner| Presenter {
                inner: Arc::new(Mutex::new(Some(inner))),
            })
            .map_err(io_error)
    })
    .await
}
