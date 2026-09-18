//! Private PyO3 extension for the public `vivid_sdk` Python package.

#![allow(clippy::too_many_arguments)]

mod overlay;

use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use pyo3::create_exception;
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::MutexExt;
use pyo3::types::{PyBytes, PyDict, PyList, PyModule, PyNone};
use vivid_protocol::cbor::{self, Value};
use vivid_protocol::context::ContextDefinition;
use vivid_protocol::file_drop::{
    AcceptFileDrop, AdvanceFileTransfer, CancelFileDrop, FileDropBinding, FileDropDestination,
    FileDropTuple, FileResult, QueryFileDrop,
};
use vivid_protocol::lease::CleanupPolicy;
use vivid_protocol::media::RasterDeltaOperation;
use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::messages::{LaneClass, TrackKind};
use vivid_protocol::resource::{RESOURCE_COUNT, ResourceContract};
use vivid_protocol::scene::{Fit, SceneNode};
use vivid_protocol::surface::DesktopSurfaceParameters;
use vivid_protocol::track::{
    AudioConfiguration, ImageConfiguration, KindConfiguration, RasterConfiguration,
    TrackConfiguration, TrackMode, VideoConfiguration,
};
use vivid_sdk::SessionLeaseBuilder;
use vivid_sdk::presenter::{
    Binding, CaptureContent, MediaConfig, PresenterConfig, PresenterListener, SocketListener,
    VirtualVivid,
};
use vivid_sdk::{
    AudioGain, ChannelEvent, CoordinateModel, GENERIC_CONTENT, IncomingFileTransfer,
    IncomingFileTransferEvent, IncomingFileTransferRequest, InputBinding, InputBindingStatus,
    InputLane, InputLaneEvent, InputTuple, ProducerAuthentication, ProducerConfig, RequestMetadata,
    SceneCommit, SendPressure, Session, SessionEvent, SlotBinding, Surface, SurfaceDefinition,
    SurfaceDescriptor, SurfaceRole, SurfaceStatus, Track, TrackChannel, TrackStatus,
    TrackWaitCondition,
};
use vivid_sdk::{
    AudioPacketData, DesktopSession as SdkDesktopSession, EncodedPacket, PaneImageOptions,
    PaneSession as SdkPaneSession, TrackSender, VideoPacketData, VideoRateControl,
    VideoRateSnapshot,
};
use vivid_sdk::{DesktopTarget, OutputDescriptor, Rotation};

create_exception!(_native, VividError, PyOSError);
create_exception!(_native, ClosedHandleError, VividError);

#[pyclass(name = "Session", module = "vivid_sdk._native")]
struct PySession {
    inner: Mutex<Option<Session>>,
}

#[pymethods]
impl PySession {
    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "session")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "session")?;
        Ok(match guard.as_ref() {
            Some(session) => format!(
                "<vivid_sdk.Session id={} target_profile='{}' closed=False>",
                session.info().session_id,
                session.info().target_profile
            ),
            None => "<vivid_sdk.Session closed=True>".into(),
        })
    }
}

/// A running presenter.
///
/// The presenter's own threads are pure Rust and never acquire the GIL. That holds only because
/// nothing here takes a Python callback: the listener is supplied as an endpoint string and built
/// in Rust, so a Python exception can never surface inside an accept loop. Keep it that way.
#[pyclass(name = "Presenter", module = "vivid_sdk._native")]
struct PyPresenter {
    inner: Mutex<Option<VirtualVivid>>,
    endpoint: String,
}

#[pymethods]
impl PyPresenter {
    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "presenter")?.is_none())
    }

    #[getter]
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn __repr__(&self) -> PyResult<String> {
        // The endpoint is addressing, not capability material. Pane secrets are returned to the
        // caller and never held here, so there is nothing else that could leak through a repr.
        let closed = lock(&self.inner, "presenter")?.is_none();
        Ok(format!(
            "<vivid_sdk.Presenter endpoint='{}' closed={}>",
            self.endpoint,
            if closed { "True" } else { "False" }
        ))
    }
}

#[pyclass(name = "Surface", module = "vivid_sdk._native", skip_from_py_object)]
#[derive(Clone)]
struct PySurface {
    inner: Surface,
}

#[pymethods]
impl PySurface {
    #[getter]
    fn context_id(&self) -> u64 {
        self.inner.context_id()
    }

    #[getter]
    fn id(&self) -> u64 {
        self.inner.id()
    }

    #[getter]
    fn revision(&self) -> u64 {
        self.inner.revision().get()
    }

    #[getter]
    fn generation(&self) -> u64 {
        self.inner.generation().get()
    }

    fn __repr__(&self) -> String {
        format!(
            "<vivid_sdk.Surface context_id={} id={} revision={} generation={}>",
            self.context_id(),
            self.id(),
            self.revision(),
            self.generation()
        )
    }
}

#[pyclass(name = "Track", module = "vivid_sdk._native", skip_from_py_object)]
#[derive(Clone)]
struct PyTrack {
    inner: Track,
}

#[pymethods]
impl PyTrack {
    #[getter]
    fn context_id(&self) -> u64 {
        self.inner.context_id()
    }

    #[getter]
    fn surface_id(&self) -> u64 {
        self.inner.surface_id()
    }

    #[getter]
    fn id(&self) -> u64 {
        self.inner.id()
    }

    #[getter]
    fn kind(&self) -> &'static str {
        kind_name(self.inner.kind())
    }

    #[getter]
    fn revision(&self) -> u64 {
        self.inner.revision().get()
    }

    #[getter]
    fn channel_generation(&self) -> u64 {
        self.inner.channel_generation().get()
    }

    fn __repr__(&self) -> String {
        format!(
            "<vivid_sdk.Track context_id={} surface_id={} id={} kind='{}' generation={}>",
            self.context_id(),
            self.surface_id(),
            self.id(),
            self.kind(),
            self.channel_generation()
        )
    }
}

#[pyclass(name = "TrackChannel", module = "vivid_sdk._native")]
struct PyTrackChannel {
    inner: Mutex<Option<TrackChannel>>,
    context_id: u64,
    surface_id: u64,
    track_id: u64,
    kind: TrackKind,
    generation: u64,
}

#[pymethods]
impl PyTrackChannel {
    #[getter]
    fn context_id(&self) -> u64 {
        self.context_id
    }

    #[getter]
    fn surface_id(&self) -> u64 {
        self.surface_id
    }

    #[getter]
    fn track_id(&self) -> u64 {
        self.track_id
    }

    #[getter]
    fn kind(&self) -> &'static str {
        kind_name(self.kind)
    }

    #[getter]
    fn generation(&self) -> u64 {
        self.generation
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "track channel")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<vivid_sdk.TrackChannel context_id={} surface_id={} track_id={} kind='{}' generation={} closed={}>",
            self.context_id,
            self.surface_id,
            self.track_id,
            kind_name(self.kind),
            self.generation,
            self.closed()?
        ))
    }
}

#[pyclass(name = "InputLane", module = "vivid_sdk._native")]
struct PyInputLane {
    inner: Mutex<Option<InputLane>>,
}

#[pymethods]
impl PyInputLane {
    #[getter]
    fn generation(&self) -> PyResult<u64> {
        Ok(lock(&self.inner, "input lane")?
            .as_ref()
            .map(|lane| lane.generation())
            .unwrap_or(0))
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "input lane")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "input lane")?;
        Ok(match guard.as_ref() {
            Some(lane) => format!("<vivid_sdk.InputLane generation={}>", lane.generation()),
            None => "<vivid_sdk.InputLane closed=True>".into(),
        })
    }
}

#[pyfunction]
#[pyo3(signature = (
    *,
    dry_run=false,
    desktop=false,
    trace_dir=None,
    endpoint_control=None,
    endpoint_interactive=None,
    endpoint_realtime=None,
    endpoint_bulk=None,
    root_secret=None,
    producer_name="vivid-sdk-python".to_owned(),
    producer_version=env!("CARGO_PKG_VERSION").to_owned(),
    target_profile="terminal-surface-v1".to_owned(),
    required_profiles=None,
    optional_profiles=None
))]
fn connect(
    py: Python<'_>,
    dry_run: bool,
    desktop: bool,
    trace_dir: Option<PathBuf>,
    endpoint_control: Option<String>,
    endpoint_interactive: Option<String>,
    endpoint_realtime: Option<String>,
    endpoint_bulk: Option<String>,
    root_secret: Option<String>,
    producer_name: String,
    producer_version: String,
    target_profile: String,
    required_profiles: Option<Vec<String>>,
    optional_profiles: Option<Vec<String>>,
) -> PyResult<PySession> {
    let base = if desktop {
        ProducerConfig::desktop()
    } else {
        ProducerConfig::default()
    };
    let mut config = ProducerConfig {
        endpoint_control,
        endpoint_interactive,
        endpoint_realtime,
        endpoint_bulk,
        producer_name,
        producer_version,
        target_profile,
        dry_run,
        trace_dir,
        ..base
    };
    if let Some(secret) = root_secret {
        let secret = zeroize::Zeroizing::new(secret);
        config.authentication = ProducerAuthentication::root_hex(&secret).map_err(value_error)?;
    }
    if let Some(profiles) = required_profiles {
        config.required_profiles = profiles;
    }
    if let Some(profiles) = optional_profiles {
        config.optional_profiles = profiles;
    }
    let session = py.detach(|| Session::connect(config)).map_err(io_error)?;
    Ok(PySession {
        inner: Mutex::new(Some(session)),
    })
}

#[pyfunction]
fn close(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<()> {
    let value = lock(&session.inner, "session")?
        .take()
        .ok_or_else(closed_session)?;
    py.detach(|| value.close()).map_err(io_error)
}

#[pyfunction]
fn allocate_id(session: PyRef<'_, PySession>) -> PyResult<u64> {
    with_session(&session, |session| session.allocate_id())
}

#[pyfunction]
fn supports(session: PyRef<'_, PySession>, profile: &str) -> PyResult<bool> {
    let guard = lock(&session.inner, "session")?;
    Ok(guard.as_ref().ok_or_else(closed_session)?.supports(profile))
}

#[pyfunction]
fn session_info(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let info = guard.as_ref().ok_or_else(closed_session)?.info();
    let result = PyDict::new(py);
    result.set_item("session_id", info.session_id)?;
    result.set_item("session_tag", info.session_tag)?;
    result.set_item("root_context_id", info.root_context_id)?;
    result.set_item("target_generation", info.target_generation.get())?;
    result.set_item("target_profile", &info.target_profile)?;
    result.set_item("accepted_profiles", &info.accepted_profiles)?;
    result.set_item("session_revision", info.session_revision)?;
    result.set_item("scene_revision", info.scene_revision.get())?;
    result.set_item("establishment_state", info.establishment_state)?;
    result.set_item("resume_generation", info.resume_generation)?;
    Ok(result.unbind())
}

#[pyfunction]
fn create_surface(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<PySurface> {
    let definition = parse_surface(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let surface = py
        .detach(|| session.create_surface(definition, &RequestMetadata::default()))
        .map_err(io_error)?;
    Ok(PySurface { inner: surface })
}

#[pyfunction]
fn update_surface(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    config: &Bound<'_, PyDict>,
) -> PyResult<()> {
    let definition = parse_surface(config)?;
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.update_surface(&surface, definition, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn destroy_surface(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
) -> PyResult<()> {
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.destroy_surface(&surface, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn create_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<PyTrack> {
    let configuration = parse_track(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let track = py
        .detach(|| session.create_track(configuration, &RequestMetadata::default()))
        .map_err(io_error)?;
    Ok(PyTrack { inner: track })
}

#[pyfunction]
fn destroy_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
) -> PyResult<()> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.destroy_track(&track, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn open_track_channel(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
) -> PyResult<PyTrackChannel> {
    let track_handle = track.inner.clone();
    let context_id = track_handle.context_id();
    let surface_id = track_handle.surface_id();
    let track_id = track_handle.id();
    let kind = track_handle.kind();
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let channel = py
        .detach(|| session.open_track_channel(&track_handle))
        .map_err(io_error)?;
    Ok(PyTrackChannel {
        context_id,
        surface_id,
        track_id,
        kind,
        generation: channel.generation().get(),
        inner: Mutex::new(Some(channel)),
    })
}

#[pyfunction]
#[pyo3(signature = (channel, rgba, *, epoch=0, frame_id=1, compress=false))]
fn send_raster(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    rgba: Vec<u8>,
    epoch: u32,
    frame_id: u64,
    compress: bool,
) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| channel.send_raster(epoch, frame_id, &rgba, compress))
        .map_err(io_error)
}

#[pyfunction]
fn send_image(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    encoded: Vec<u8>,
) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| channel.send_image(&encoded)).map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (
    channel,
    data,
    *,
    packet_id,
    pts_us,
    dts_us,
    duration_us,
    key,
    epoch=0
))]
fn send_video(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    data: Vec<u8>,
    packet_id: u64,
    pts_us: i64,
    dts_us: i64,
    duration_us: u64,
    key: bool,
    epoch: u32,
) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| {
        channel.send_video(VideoPacket {
            epoch,
            packet_id,
            pts_us,
            dts_us,
            duration_us,
            key,
            data: &data,
        })
    })
    .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (
    channel,
    data,
    *,
    packet_id,
    pts_us,
    dts_us,
    duration_us,
    epoch=0,
    trim_start_samples=0,
    trim_end_samples=0
))]
fn send_audio(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    data: Vec<u8>,
    packet_id: u64,
    pts_us: i64,
    dts_us: i64,
    duration_us: u64,
    epoch: u32,
    trim_start_samples: u32,
    trim_end_samples: u32,
) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| {
        channel.send_audio(AudioPacket {
            epoch,
            packet_id,
            pts_us,
            dts_us,
            duration_us,
            trim_start_samples,
            trim_end_samples,
            data: &data,
        })
    })
    .map_err(io_error)
}

#[pyfunction]
fn channel_eos(py: Python<'_>, channel: PyRef<'_, PyTrackChannel>) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| channel.eos()).map_err(io_error)
}

#[pyfunction]
fn close_channel(py: Python<'_>, channel: PyRef<'_, PyTrackChannel>) -> PyResult<()> {
    let value = lock(&channel.inner, "track channel")?
        .take()
        .ok_or_else(closed_channel)?;
    py.detach(|| value.close()).map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (session, surface, track, *, required_milestone=1 << 4))]
fn activate_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    track: PyRef<'_, PyTrack>,
    required_milestone: u64,
) -> PyResult<u64> {
    let surface_handle = surface.inner.clone();
    let track_handle = track.inner.clone();
    let configuration = track_handle.configuration().map_err(io_error)?;
    let binding = SlotBinding {
        slot: configuration.slot,
        track_id: track_handle.id(),
        expected_channel_generation: track_handle.channel_generation(),
        required_milestone,
    };
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.activate_tracks(&surface_handle, &[binding], &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (
    session,
    track,
    *,
    condition,
    value=None,
    timeout_us=30_000_000
))]
fn wait_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    condition: u64,
    value: Option<u64>,
    timeout_us: u64,
) -> PyResult<Py<PyDict>> {
    let condition = TrackWaitCondition::try_from(condition).map_err(value_error)?;
    let track = track.inner.clone();
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let result = py
        .detach(|| session.wait_track(&track, condition, value, timeout_us))
        .map_err(io_error)?;
    let output = PyDict::new(py);
    output.set_item("context_id", result.context_id)?;
    output.set_item("surface_id", result.surface_id)?;
    output.set_item("track_id", result.track_id)?;
    output.set_item("revision", result.revision.get())?;
    output.set_item("channel_generation", result.channel_generation.get())?;
    output.set_item("condition", result.condition as u64)?;
    output.set_item("observed_value", result.observed_value)?;
    Ok(output.unbind())
}

#[pyfunction]
#[pyo3(signature = (
    session,
    surface,
    *,
    node_id,
    x=0,
    y=0,
    width,
    height,
    text_layer=1
))]
fn place_terminal_surface(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    node_id: u64,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    text_layer: u64,
) -> PyResult<(u64, u64)> {
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let result = py
        .detach(|| {
            session.place_terminal_surface(&surface, node_id, x, y, width, height, text_layer)
        })
        .map_err(io_error)?;
    Ok((result.scene_revision.get(), result.target_generation.get()))
}

#[pyfunction]
fn delete_node(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    context_id: u64,
    node_id: u64,
) -> PyResult<(u64, u64)> {
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let result = py
        .detach(|| session.delete_node(context_id, node_id, &RequestMetadata::default()))
        .map_err(io_error)?;
    Ok((result.scene_revision.get(), result.target_generation.get()))
}

#[pyfunction]
fn anchor_marker(
    session: PyRef<'_, PySession>,
    context_id: u64,
    anchor_id: u64,
) -> PyResult<String> {
    with_session(&session, |session| {
        session.anchor_marker(context_id, anchor_id)
    })
}

/// One raster delta operation, given as a dict:
/// `{"op": "overwrite", x, y, width, height, rgba}` or
/// `{"op": "copy", destination_x, destination_y, width, height, source_x, source_y}`.
fn parse_delta_spec(spec: &Bound<'_, PyDict>) -> PyResult<RasterDeltaSpec> {
    match optional::<String>(spec, "op")?.as_deref() {
        Some("overwrite") => Ok(RasterDeltaSpec::Overwrite {
            x: required(spec, "x")?,
            y: required(spec, "y")?,
            width: required(spec, "width")?,
            height: required(spec, "height")?,
            rgba: required::<Vec<u8>>(spec, "rgba")?,
        }),
        Some("copy") => Ok(RasterDeltaSpec::Copy {
            destination_x: required(spec, "destination_x")?,
            destination_y: required(spec, "destination_y")?,
            width: required(spec, "width")?,
            height: required(spec, "height")?,
            source_x: required(spec, "source_x")?,
            source_y: required(spec, "source_y")?,
        }),
        _ => Err(PyValueError::new_err(
            "delta operation needs 'op' of 'overwrite' or 'copy'",
        )),
    }
}

#[derive(Clone)]
enum RasterDeltaSpec {
    Overwrite {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Copy {
        destination_x: u32,
        destination_y: u32,
        width: u32,
        height: u32,
        source_x: u32,
        source_y: u32,
    },
}

/// Parse owned delta specs. The bytes are cloned into a stable local store and the operations
/// borrow from it, so the protocol's `&[u8]` shape is met inside the callback's scope.
fn parse_delta_specs(specs: &Bound<'_, PyList>) -> PyResult<Vec<RasterDeltaSpec>> {
    specs
        .iter()
        .map(|item| {
            let spec = item
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("each delta operation must be a dict"))?;
            parse_delta_spec(spec)
        })
        .collect()
}

fn with_delta_operations<T>(
    specs: &Bound<'_, PyList>,
    then: impl FnOnce(&[RasterDeltaOperation<'_>]) -> PyResult<T>,
) -> PyResult<T> {
    let parsed = parse_delta_specs(specs)?;
    // The bytes are cloned into a stable local store; the operations borrow from it, and both
    // live only inside this scope, which is exactly as long as the send call needs them.
    let mut rgbas = Vec::new();
    for spec in &parsed {
        if let RasterDeltaSpec::Overwrite { rgba, .. } = spec {
            rgbas.push(rgba.clone());
        }
    }
    let mut blocks = rgbas.iter();
    let mut operations = Vec::with_capacity(parsed.len());
    for spec in &parsed {
        match spec {
            RasterDeltaSpec::Overwrite {
                x,
                y,
                width,
                height,
                ..
            } => {
                if let Some(rgba) = blocks.next() {
                    operations.push(RasterDeltaOperation::Overwrite {
                        x: *x,
                        y: *y,
                        width: *width,
                        height: *height,
                        rgba,
                    });
                }
            }
            RasterDeltaSpec::Copy {
                destination_x,
                destination_y,
                width,
                height,
                source_x,
                source_y,
            } => operations.push(RasterDeltaOperation::Copy {
                destination_x: *destination_x,
                destination_y: *destination_y,
                width: *width,
                height: *height,
                source_x: *source_x,
                source_y: *source_y,
            }),
        }
    }
    then(&operations)
}

// ---------------------------------------------------------------------------
// Constants and builders
// ---------------------------------------------------------------------------

/// Every protocol constant the SDK exposes, as `(name, text, number)` triples.
///
/// The host layer builds its namespace from this at import time, so a value that changes in
/// `vivid_protocol` changes here too without anybody copying a number.
#[pyfunction]
fn constant_table(py: Python<'_>) -> PyResult<Py<PyList>> {
    let entries = PyList::empty(py);
    for (name, value) in vivid_sdk::constant_table() {
        entries.append((*name, value.as_text(), value.as_number()))?;
    }
    Ok(entries.unbind())
}

/// Inspect PNG or JPEG header metadata and return `(encoding, width, height, encoded_length)`.
#[pyfunction]
fn probe_encoded_image(data: Vec<u8>) -> PyResult<(u64, u32, u32, u32)> {
    let image = vivid_sdk::probe_encoded_image(&data).map_err(io_error)?;
    Ok((
        image.encoding,
        image.width,
        image.height,
        image.encoded_length,
    ))
}

/// Build a track configuration from a builder spec, so bindings do not compute resource claims.
///
/// `kind` selects the builder method; the remaining keys are the claims a caller wants to
/// override. Everything unset keeps the builder's own default for that kind.
#[pyfunction]
fn build_track_config(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: &Bound<'_, PyDict>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let context_id: u64 = required(surface, "context_id")?;
    let surface_id: u64 = required(surface, "surface_id")?;
    let contract = session.info().resource_contract.clone();

    let slot = optional(config, "slot")?.unwrap_or_else(|| {
        match required::<String>(config, "kind").as_deref() {
            Ok("video") => vivid_sdk::SLOT_PRIMARY_VIDEO,
            Ok("audio") => vivid_sdk::SLOT_AUDIO,
            Ok("image") => vivid_sdk::SLOT_POSTER,
            _ => vivid_sdk::SLOT_RASTER,
        }
    });
    let mode = TrackMode::try_from(optional(config, "mode")?.unwrap_or(1))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let lane = LaneClass::try_from(optional(config, "lane")?.unwrap_or(3))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;

    let kind_name: String = required(config, "kind")?;
    let mut builder = vivid_sdk::TrackBuilder::detached(context_id, surface_id, slot, mode, lane);
    match kind_name.as_str() {
        "video" => {
            builder = builder.video(
                required(config, "width")?,
                required(config, "height")?,
                &required::<String>(config, "codec")?,
            );
        }
        "audio" => {
            builder = builder.audio(
                required(config, "sample_rate")?,
                required::<u8>(config, "channels")?,
            );
        }
        "raster" => {
            builder = builder
                .raster(required(config, "width")?, required(config, "height")?)
                .map_err(io_error)?;
        }
        "image" => {
            let encoded: Vec<u8> = required(config, "encoded")?;
            let mut image = vivid_sdk::probe_encoded_image(&encoded).map_err(io_error)?;
            if let Some(sha256) = optional::<Vec<u8>>(config, "sha256")? {
                image.sha256 = Some(
                    sha256
                        .try_into()
                        .map_err(|_| PyValueError::new_err("sha256 must contain 32 bytes"))?,
                );
            }
            image.cache_lookup = optional(config, "cache_lookup")?.unwrap_or(false);
            // Keep the probed container so `send_image` can validate against it without a
            // second container walk in the host layer.
            builder = builder.image(image).map_err(io_error)?;
        }
        other => {
            return Err(PyValueError::new_err(format!(
                "track kind must be video, audio, raster, or image, not {other:?}"
            )));
        }
    }

    if optional::<bool>(config, "uplink")?.unwrap_or(false) {
        builder = builder.uplink();
    }
    if let Some(value) = optional(config, "maximum_rate_millihertz")? {
        builder = builder.max_rate_millihertz(value);
    }
    if let Some(value) = optional(config, "maximum_encoded_bits_per_second")? {
        builder = builder.max_encoded_bps(value);
    }

    let track_id = match optional(config, "track_id")? {
        Some(id) => id,
        None => session.allocate_id().map_err(io_error)?,
    };
    let mut configuration = builder.build(&contract, track_id).map_err(io_error)?;
    debug_assert_eq!(configuration.context_id, context_id);
    debug_assert_eq!(configuration.surface_id, surface_id);

    // The builder owns the claims; these are the codec details a caller may state explicitly.
    // Applying them after the build keeps the arithmetic in one place while preserving every
    // field the wire carries.
    match &mut configuration.kind {
        vivid_protocol::track::KindConfiguration::VectorScene(_) => {}
        vivid_protocol::track::KindConfiguration::Video(video) => {
            if let Some(value) = optional::<String>(config, "packetization")? {
                video.packetization = value;
            }
            if let Some(value) = optional::<Vec<u8>>(config, "extradata")? {
                video.extradata = value;
            }
            if let Some(value) = optional(config, "profile")? {
                video.profile = value;
            }
            if let Some(value) = optional(config, "level")? {
                video.level = value;
            }
            if let Some(value) = optional(config, "maximum_reorder_depth")? {
                video.maximum_reorder_depth = value;
            }
            if let Some(value) = optional(config, "color_primaries")? {
                video.color_primaries = value;
            }
            if let Some(value) = optional(config, "transfer")? {
                video.transfer = value;
            }
            if let Some(value) = optional(config, "matrix")? {
                video.matrix = value;
            }
            if let Some(value) = optional(config, "signal_range")? {
                video.signal_range = value;
            }
            if let Some(value) = optional(config, "aspect_numerator")? {
                video.aspect_numerator = value;
            }
            if let Some(value) = optional(config, "aspect_denominator")? {
                video.aspect_denominator = value;
            }
            if let Some(value) = optional(config, "maximum_access_unit_bytes")? {
                video.maximum_access_unit_bytes = value;
            }
            if let Some(value) = optional::<String>(config, "codec_string")? {
                video.codec_string = Some(value);
            }
            if let Some(value) = optional::<Vec<u8>>(config, "decoder_configuration")? {
                video.decoder_configuration = Some(value);
            }
        }
        vivid_protocol::track::KindConfiguration::Audio(audio) => {
            if let Some(value) = optional::<String>(config, "codec")? {
                audio.codec = value;
            }
            if let Some(value) = optional::<String>(config, "packetization")? {
                audio.packetization = value;
            }
            if let Some(value) = optional::<Vec<u8>>(config, "extradata")? {
                audio.extradata = value;
            }
            if let Some(value) = optional(config, "channel_mask")? {
                audio.channel_mask = value;
            }
            if let Some(value) = optional(config, "maximum_access_unit_bytes")? {
                audio.maximum_access_unit_bytes = value;
            }
            if let Some(value) = optional::<String>(config, "codec_string")? {
                audio.codec_string = Some(value);
            }
        }
        vivid_protocol::track::KindConfiguration::Raster(raster) => {
            if let Some(value) = optional(config, "alpha_mode")? {
                raster.alpha_mode = value;
            }
            if let Some(value) = optional(config, "delta_enabled")? {
                raster.delta_enabled = value;
            }
            if let Some(value) = optional(config, "maximum_delta_operations")? {
                raster.maximum_delta_operations = value;
            }
            if let Some(value) = optional(config, "zstd_enabled")? {
                raster.zstd_enabled = value;
            }
        }
        vivid_protocol::track::KindConfiguration::EncodedImage(image) => {
            if let Some(value) = optional::<Vec<u8>>(config, "sha256")? {
                image.sha256 = Some(
                    value
                        .try_into()
                        .map_err(|_| PyValueError::new_err("sha256 must contain 32 bytes"))?,
                );
            }
            if let Some(value) = optional(config, "cache_lookup")? {
                image.cache_lookup = value;
            }
        }
    }

    let dict = PyDict::new(py);
    dict.set_item("context_id", configuration.context_id)?;
    dict.set_item("surface_id", configuration.surface_id)?;
    dict.set_item("track_id", configuration.track_id)?;
    dict.set_item("slot", configuration.slot)?;
    dict.set_item("mode", configuration.mode as u64)?;
    dict.set_item("lane", configuration.lane as u64)?;
    dict.set_item("direction", configuration.direction as u64)?;
    dict.set_item("maximum_record_body", configuration.maximum_record_body)?;
    dict.set_item(
        "maximum_rate_millihertz",
        configuration.maximum_rate_millihertz,
    )?;
    dict.set_item(
        "maximum_encoded_bits_per_second",
        configuration.maximum_encoded_bits_per_second,
    )?;
    dict.set_item(
        "maximum_records_per_second",
        configuration.maximum_records_per_second,
    )?;
    dict.set_item(
        "maximum_inflight_body_bytes",
        configuration.maximum_inflight_body_bytes,
    )?;
    dict.set_item("target_latency_us", configuration.target_latency_us)?;
    dict.set_item("maximum_latency_us", configuration.maximum_latency_us)?;
    dict.set_item("retained_pixel_charge", configuration.retained_pixel_charge)?;
    match &configuration.kind {
        vivid_protocol::track::KindConfiguration::VectorScene(vector) => {
            dict.set_item("kind", "vector")?;
            dict.set_item("width", vector.width)?;
            dict.set_item("height", vector.height)?;
            dict.set_item("maximum_scene_bytes", vector.maximum_scene_bytes)?;
        }
        vivid_protocol::track::KindConfiguration::Video(video) => {
            dict.set_item("kind", "video")?;
            dict.set_item("codec", &video.codec)?;
            dict.set_item("packetization", &video.packetization)?;
            dict.set_item("extradata", PyBytes::new(py, &video.extradata))?;
            dict.set_item("width", video.coded_width)?;
            dict.set_item("height", video.coded_height)?;
            dict.set_item("profile", video.profile)?;
            dict.set_item("level", video.level)?;
            dict.set_item("maximum_reorder_depth", video.maximum_reorder_depth)?;
            dict.set_item("color_primaries", video.color_primaries)?;
            dict.set_item("transfer", video.transfer)?;
            dict.set_item("matrix", video.matrix)?;
            dict.set_item("signal_range", video.signal_range)?;
            dict.set_item("aspect_numerator", video.aspect_numerator)?;
            dict.set_item("aspect_denominator", video.aspect_denominator)?;
            dict.set_item("maximum_access_unit_bytes", video.maximum_access_unit_bytes)?;
            dict.set_item("codec_string", video.codec_string.clone())?;
            dict.set_item(
                "decoder_configuration",
                video
                    .decoder_configuration
                    .as_ref()
                    .map(|value| PyBytes::new(py, value)),
            )?;
        }
        vivid_protocol::track::KindConfiguration::Audio(audio) => {
            dict.set_item("kind", "audio")?;
            dict.set_item("codec", &audio.codec)?;
            dict.set_item("packetization", &audio.packetization)?;
            dict.set_item("extradata", PyBytes::new(py, &audio.extradata))?;
            dict.set_item("sample_rate", audio.sample_rate)?;
            dict.set_item("channels", audio.channels)?;
            dict.set_item("channel_mask", audio.channel_mask)?;
            dict.set_item("maximum_access_unit_bytes", audio.maximum_access_unit_bytes)?;
            dict.set_item("codec_string", audio.codec_string.clone())?;
        }
        vivid_protocol::track::KindConfiguration::Raster(raster) => {
            dict.set_item("kind", "raster")?;
            dict.set_item("width", raster.width)?;
            dict.set_item("height", raster.height)?;
            dict.set_item("alpha_mode", raster.alpha_mode)?;
            dict.set_item("delta_enabled", raster.delta_enabled)?;
            dict.set_item("maximum_delta_operations", raster.maximum_delta_operations)?;
            dict.set_item("zstd_enabled", raster.zstd_enabled)?;
        }
        vivid_protocol::track::KindConfiguration::EncodedImage(image) => {
            dict.set_item("kind", "image")?;
            dict.set_item("encoding", image.encoding)?;
            dict.set_item("width", image.width)?;
            dict.set_item("height", image.height)?;
            dict.set_item("encoded_length", image.encoded_length)?;
            dict.set_item("sha256", image.sha256.map(|value| PyBytes::new(py, &value)))?;
            dict.set_item("cache_lookup", image.cache_lookup)?;
        }
    }
    Ok(dict.unbind())
}

/// Build a surface definition from a builder spec, defaulting identity from the session.
#[pyfunction]
fn build_surface_config(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let mut builder = vivid_sdk::SurfaceBuilder::new(
        session,
        required(config, "logical_width")?,
        required(config, "logical_height")?,
    )
    .map_err(io_error)?;
    if let Some(context_id) = optional(config, "context_id")? {
        builder = builder.context(context_id);
    }
    if let Some(surface_id) = optional(config, "surface_id")? {
        builder = builder.surface_id(surface_id);
    }
    let semantic_profile = optional::<String>(config, "semantic_profile")?
        .unwrap_or_else(|| vivid_sdk::GENERIC_CONTENT.into());
    let coordinate_model =
        CoordinateModel::try_from(optional::<u64>(config, "coordinate_model")?.unwrap_or(1))
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
    builder = builder.semantic(&semantic_profile, coordinate_model);
    let role = SurfaceRole::try_from(optional::<u64>(config, "role")?.unwrap_or(0))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    builder = builder
        .descriptor(SurfaceDescriptor {
            role,
            title: optional::<String>(config, "title")?.unwrap_or_default(),
            semantic_content_revision: optional(config, "semantic_content_revision")?.unwrap_or(0),
            semantic_availability: optional(config, "semantic_availability")?.unwrap_or(0),
            locator_hint: optional::<String>(config, "locator_hint")?.unwrap_or_default(),
        })
        .scale(
            optional(config, "scale_numerator")?.unwrap_or(1),
            optional(config, "scale_denominator")?.unwrap_or(1),
            optional(config, "rotation")?.unwrap_or(0),
        );
    if let Some(policy) = optional(config, "policy")? {
        builder = builder.policy(policy);
    }
    if let Some(parameters) = config.get_item("desktop_parameters")? {
        if !parameters.is_none() {
            let parameters = parameters
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("desktop_parameters must be a dict"))?;
            let encoded = parse_desktop_parameters(Some(parameters.clone().into_any()))?;
            builder = builder.profile_parameters(encoded);
        }
    }
    let definition = builder.build().map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("context_id", definition.context_id)?;
    dict.set_item("surface_id", definition.surface_id)?;
    dict.set_item("semantic_profile", &definition.semantic_profile)?;
    dict.set_item("coordinate_model", definition.coordinate_model as u64)?;
    dict.set_item("logical_width", definition.logical_width)?;
    dict.set_item("logical_height", definition.logical_height)?;
    dict.set_item("scale_numerator", definition.scale_numerator)?;
    dict.set_item("scale_denominator", definition.scale_denominator)?;
    dict.set_item("rotation", definition.rotation)?;
    dict.set_item("role", definition.descriptor.role as u64)?;
    dict.set_item("title", &definition.descriptor.title)?;
    dict.set_item(
        "semantic_content_revision",
        definition.descriptor.semantic_content_revision,
    )?;
    dict.set_item(
        "semantic_availability",
        definition.descriptor.semantic_availability,
    )?;
    dict.set_item("locator_hint", &definition.descriptor.locator_hint)?;
    dict.set_item("policy", definition.policy)?;
    Ok(dict.unbind())
}

// ---------------------------------------------------------------------------
// Events, abort, and queries
// ---------------------------------------------------------------------------

fn payload_to_pydict<'py>(py: Python<'py>, payload: &PayloadMap) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in payload.iter() {
        match value {
            Value::Unsigned(unsigned) => dict.set_item(key, unsigned)?,
            Value::Negative(value) => dict.set_item(key, value)?,
            Value::Text(text) => dict.set_item(key, text)?,
            Value::Bool(value) => dict.set_item(key, value)?,
            Value::Null => dict.set_item(key, py.None())?,
            // Arrays, maps, and bytes are re-encoded deterministically so nothing a relay must
            // preserve byte-for-byte is lost at this boundary.
            other => {
                let encoded = cbor::encode(other)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?;
                dict.set_item(key, PyBytes::new(py, &encoded))?;
            }
        }
    }
    Ok(dict)
}

fn session_event_to_pydict(py: Python<'_>, event: SessionEvent) -> PyResult<Bound<'_, PyDict>> {
    let dict = PyDict::new(py);
    match event {
        SessionEvent::PlaybackHold(hold) => {
            dict.set_item("kind", "playback_hold")?;
            dict.set_item(
                "payload",
                payload_to_pydict(py, &hold.payload().map_err(io_error)?)?,
            )?;
        }
        SessionEvent::TargetChanged(payload) => {
            dict.set_item("kind", "target_changed")?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::AnchorReady {
            context_id,
            anchor_id,
            payload,
        } => {
            dict.set_item("kind", "anchor_ready")?;
            dict.set_item("context_id", context_id)?;
            dict.set_item("anchor_id", anchor_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::AnchorGone {
            context_id,
            anchor_id,
            payload,
        } => {
            dict.set_item("kind", "anchor_gone")?;
            dict.set_item("context_id", context_id)?;
            dict.set_item("anchor_id", anchor_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::TrackLost { object_id, payload } => {
            dict.set_item("kind", "track_lost")?;
            dict.set_item("object_id", object_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::ContextChanged { object_id, payload } => {
            dict.set_item("kind", "context_changed")?;
            dict.set_item("object_id", object_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::FileDropOffered(offer) => {
            dict.set_item("kind", "file_drop_offered")?;
            dict.set_item("binding", file_drop_tuple_to_pydict(py, &offer.binding))?;
            dict.set_item("suggested_name", &offer.suggested_name)?;
            dict.set_item("declared_length", offer.declared_length)?;
        }
        SessionEvent::FileDropCancelled(cancel) => {
            dict.set_item("kind", "file_drop_cancelled")?;
            dict.set_item("drop_id", cancel.binding.drop_id)?;
            dict.set_item("reason", cancel.reason)?;
        }
        SessionEvent::Other {
            record_type,
            object_id,
            payload,
        } => {
            dict.set_item("kind", "other")?;
            dict.set_item("record_type", record_type)?;
            dict.set_item("object_id", object_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        SessionEvent::ConnectionClosed { diagnostic } => {
            dict.set_item("kind", "connection_closed")?;
            dict.set_item("diagnostic", diagnostic)?;
        }
    }
    Ok(dict)
}

#[pyfunction]
fn abort(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.abort()).map_err(io_error)
}

#[pyfunction]
fn take_event(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    match session.take_event().map_err(io_error)? {
        Some(event) => Ok(Some(session_event_to_pydict(py, event)?.unbind())),
        None => Ok(None),
    }
}

/// Wait up to `timeout_us` for the next session event. Returns `None` on timeout and once the
/// session has delivered its final `connection_closed` event, which is what ends an event loop.
#[pyfunction]
#[pyo3(signature = (session, *, timeout_us=30_000_000))]
fn wait_event(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    timeout_us: u64,
) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&session.inner, "session")?;
    let session_ref = guard.as_ref().ok_or_else(closed_session)?;
    let event = py
        .detach(|| session_ref.wait_event(Duration::from_micros(timeout_us)))
        .map_err(io_error)?;
    match event {
        Some(event) => Ok(Some(session_event_to_pydict(py, event)?.unbind())),
        None => Ok(None),
    }
}

fn surface_status_to_pydict(py: Python<'_>, status: &SurfaceStatus) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("context_id", status.context_id)?;
    dict.set_item("surface_id", status.surface_id)?;
    dict.set_item("revision", status.revision.get())?;
    dict.set_item("generation", status.generation.get())?;
    dict.set_item("semantic_profile", &status.semantic_profile)?;
    dict.set_item("coordinate_model", status.coordinate_model as u64)?;
    dict.set_item("logical_width", status.logical_width)?;
    dict.set_item("logical_height", status.logical_height)?;
    dict.set_item("scale_numerator", status.scale_numerator)?;
    dict.set_item("scale_denominator", status.scale_denominator)?;
    dict.set_item("rotation", status.rotation)?;
    dict.set_item("role", status.descriptor.role as u64)?;
    dict.set_item("title", &status.descriptor.title)?;
    dict.set_item(
        "semantic_content_revision",
        status.descriptor.semantic_content_revision,
    )?;
    dict.set_item(
        "semantic_availability",
        status.descriptor.semantic_availability,
    )?;
    dict.set_item("locator_hint", &status.descriptor.locator_hint)?;
    dict.set_item("effective_policy", status.effective_policy)?;
    dict.set_item("active_slots", payload_to_pydict(py, &status.active_slots)?)?;
    dict.set_item("lifecycle", status.lifecycle)?;
    dict.set_item(
        "profile_status",
        payload_to_pydict(py, &status.profile_status)?,
    )?;
    Ok(dict.unbind())
}

#[pyfunction]
fn query_surface(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
) -> PyResult<Py<PyDict>> {
    let surface = surface.inner.clone();
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let status = py
        .detach(|| session.query_surface(&surface))
        .map_err(io_error)?;
    surface_status_to_pydict(py, &status)
}

fn track_status_to_pydict(py: Python<'_>, status: &TrackStatus) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    if let Some(hold) = &status.playback_hold {
        dict.set_item(
            "playback_hold",
            payload_to_pydict(py, &hold.payload().map_err(io_error)?)?,
        )?;
    } else {
        dict.set_item("playback_hold", PyNone::get(py))?;
    }
    dict.set_item("context_id", status.context_id)?;
    dict.set_item("surface_id", status.surface_id)?;
    dict.set_item("track_id", status.track_id)?;
    dict.set_item("kind", kind_name(status.kind))?;
    dict.set_item("mode", status.mode as u64)?;
    dict.set_item("revision", status.revision.get())?;
    dict.set_item("channel_generation", status.channel_generation.get())?;
    dict.set_item("lifecycle", status.lifecycle)?;
    dict.set_item("attachment_state", status.attachment_state)?;
    dict.set_item("milestones", status.milestones)?;
    dict.set_item("media_epoch", status.media_epoch)?;
    dict.set_item("last_media_id", status.last_media_id)?;
    dict.set_item(
        "last_media_record_sequence",
        status.last_media_record_sequence,
    )?;
    dict.set_item("last_decoded_pts_us", status.last_decoded_pts_us)?;
    dict.set_item("last_presented_pts_us", status.last_presented_pts_us)?;
    dict.set_item("last_presentation_id", status.last_presentation_id)?;
    dict.set_item("cumulative_body_bytes", status.cumulative_body_bytes)?;
    dict.set_item("cumulative_media_records", status.cumulative_media_records)?;
    dict.set_item("maximum_body_bytes", status.maximum_body_bytes)?;
    dict.set_item("maximum_media_records", status.maximum_media_records)?;
    dict.set_item("ingress_depth_bucket", status.ingress_depth_bucket)?;
    match &status.playback_state {
        Some(playback) => dict.set_item("playback_state", payload_to_pydict(py, playback)?)?,
        None => dict.set_item("playback_state", PyNone::get(py))?,
    }
    match status.terminal_loss_code {
        Some(code) => dict.set_item("terminal_loss_code", code)?,
        None => dict.set_item("terminal_loss_code", PyNone::get(py))?,
    }
    match &status.audio_gain {
        Some(gain) => {
            let gain_dict = PyDict::new(py);
            gain_dict.set_item("raw", gain.raw())?;
            dict.set_item("audio_gain", gain_dict)?;
        }
        None => dict.set_item("audio_gain", PyNone::get(py))?,
    }
    Ok(dict.unbind())
}

#[pyfunction]
fn query_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
) -> PyResult<Py<PyDict>> {
    let track = track.inner.clone();
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let status = py
        .detach(|| session.query_track(&track))
        .map_err(io_error)?;
    track_status_to_pydict(py, &status)
}

#[pyfunction]
fn probe_track(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let mut configuration = parse_track(config)?;
    // A probe names no track: the protocol requires key 2 to be zero.
    configuration.track_id = 0;
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let support = py
        .detach(|| session.probe_track(&configuration))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("supported", support.supported)?;
    dict.set_item("selected_decoder", &support.selected_decoder)?;
    dict.set_item("capability_generation", support.capability_generation)?;
    dict.set_item(
        "effective_claims",
        payload_to_pydict(py, &support.effective_claims)?,
    )?;
    Ok(dict.unbind())
}

#[pyfunction]
fn query_anchor(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    context_id: u64,
    anchor_id: u64,
) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let status = py
        .detach(|| session.query_anchor(context_id, anchor_id))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("context_id", status.context_id)?;
    dict.set_item("anchor_id", status.anchor_id)?;
    dict.set_item("state", status.state)?;
    match status.target_generation {
        Some(generation) => dict.set_item("target_generation", generation.get())?,
        None => dict.set_item("target_generation", PyNone::get(py))?,
    }
    dict.set_item("payload", payload_to_pydict(py, &status.payload)?)?;
    Ok(dict.unbind())
}

#[pyfunction]
fn query_session(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let payload = py.detach(|| session.query_session()).map_err(io_error)?;
    payload_to_pydict(py, &payload).map(|dict| dict.unbind())
}

// ---------------------------------------------------------------------------
// Channel pressure, adaptive and delta rasters, channel advance
// ---------------------------------------------------------------------------

fn send_pressure_to_pydict(py: Python<'_>, pressure: SendPressure) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("rate_limited_us", pressure.rate_limited.as_micros() as u64)?;
    dict.set_item("flow_limited_us", pressure.flow_limited.as_micros() as u64)?;
    dict.set_item("transport_us", pressure.transport.as_micros() as u64)?;
    dict.set_item("records", pressure.records)?;
    Ok(dict.unbind())
}

#[pyfunction]
fn take_send_pressure(py: Python<'_>, channel: PyRef<'_, PyTrackChannel>) -> PyResult<Py<PyDict>> {
    let guard = lock(&channel.inner, "track channel")?;
    let pressure = guard
        .as_ref()
        .ok_or_else(closed_channel)?
        .take_send_pressure();
    send_pressure_to_pydict(py, pressure)
}

#[pyfunction]
fn media_credit_available(channel: PyRef<'_, PyTrackChannel>, body_length: u32) -> PyResult<bool> {
    let guard = lock(&channel.inner, "track channel")?;
    Ok(guard
        .as_ref()
        .ok_or_else(closed_channel)?
        .media_credit_available(body_length))
}

#[pyfunction]
#[pyo3(signature = (channel, rgba, *, epoch=0, frame_id=1))]
fn send_raster_adaptive(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    rgba: Vec<u8>,
    epoch: u32,
    frame_id: u64,
) -> PyResult<u64> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| channel.send_raster_adaptive(epoch, frame_id, &rgba))
        .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (channel, operations, *, epoch, frame_id, base_frame_id, pts_us=0, duration_us=0, compress=false))]
fn send_raster_delta(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    operations: &Bound<'_, PyList>,
    epoch: u32,
    frame_id: u64,
    base_frame_id: u64,
    pts_us: i64,
    duration_us: u64,
    compress: bool,
) -> PyResult<u64> {
    with_delta_operations(operations, |operations| {
        let guard = lock(&channel.inner, "track channel")?;
        let channel = guard.as_ref().ok_or_else(closed_channel)?;
        py.detach(|| {
            channel.send_raster_delta(
                epoch,
                frame_id,
                base_frame_id,
                pts_us,
                duration_us,
                operations,
                compress,
            )
        })
        .map_err(io_error)
    })
}

#[pyfunction]
#[pyo3(signature = (channel, operations, *, epoch, frame_id, base_frame_id, pts_us=0, duration_us=0))]
fn send_raster_delta_adaptive(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    operations: &Bound<'_, PyList>,
    epoch: u32,
    frame_id: u64,
    base_frame_id: u64,
    pts_us: i64,
    duration_us: u64,
) -> PyResult<u64> {
    with_delta_operations(operations, |operations| {
        let guard = lock(&channel.inner, "track channel")?;
        let channel = guard.as_ref().ok_or_else(closed_channel)?;
        py.detach(|| {
            channel.send_raster_delta_adaptive(
                epoch,
                frame_id,
                base_frame_id,
                pts_us,
                duration_us,
                operations,
            )
        })
        .map_err(io_error)
    })
}

fn channel_event_to_pydict(py: Python<'_>, event: ChannelEvent) -> PyResult<Bound<'_, PyDict>> {
    let dict = PyDict::new(py);
    match event {
        ChannelEvent::NeedKeyframe(payload) => {
            dict.set_item("kind", "need_keyframe")?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        ChannelEvent::NeedFullFrame(payload) => {
            dict.set_item("kind", "need_full_frame")?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        ChannelEvent::Error(presenter_error) => {
            dict.set_item("kind", "error")?;
            dict.set_item("code", presenter_error.code)?;
            dict.set_item("message", presenter_error.to_string())?;
        }
    }
    Ok(dict)
}

#[pyfunction]
fn channel_take_event(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    match channel.take_event().map_err(io_error)? {
        Some(event) => Ok(Some(channel_event_to_pydict(py, event)?.unbind())),
        None => Ok(None),
    }
}

#[pyfunction]
#[pyo3(signature = (channel, *, timeout_us=30_000_000))]
fn channel_wait_event(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
    timeout_us: u64,
) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel_ref = guard.as_ref().ok_or_else(closed_channel)?;
    let event = py
        .detach(|| channel_ref.wait_event(Duration::from_micros(timeout_us)))
        .map_err(io_error)?;
    match event {
        Some(event) => Ok(Some(channel_event_to_pydict(py, event)?.unbind())),
        None => Ok(None),
    }
}

/// Start a fresh authenticated channel generation for the track.
#[pyfunction]
fn advance_channel(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    reason: u64,
) -> PyResult<PyTrackChannel> {
    let track_handle = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let generation = py
        .detach(|| session.advance_channel(&track_handle, reason, &RequestMetadata::default()))
        .map_err(io_error)?;
    let channel = py
        .detach(|| session.open_track_channel(&track_handle))
        .map_err(io_error)?;
    if channel.generation() != generation {
        return Err(io_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "channel advance did not produce the requested generation",
        )));
    }
    Ok(PyTrackChannel {
        context_id: track_handle.context_id(),
        surface_id: track_handle.surface_id(),
        track_id: track_handle.id(),
        kind: track_handle.kind(),
        generation: channel.generation().get(),
        inner: Mutex::new(Some(channel)),
    })
}

// ---------------------------------------------------------------------------
// Timed playback
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(signature = (session, track, start_pts_us, minimum_buffer_us, maximum_latency_us, synchronized=false, hold_serial=None))]
fn play(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    start_pts_us: i64,
    minimum_buffer_us: u64,
    maximum_latency_us: u64,
    synchronized: bool,
    hold_serial: Option<u64>,
) -> PyResult<()> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| {
        session.play_with(
            &track,
            vivid_sdk::PlayOptions {
                start_pts_us,
                minimum_buffer_us,
                maximum_latency_us,
                hold_serial,
                start_policy: if synchronized {
                    vivid_sdk::StartPolicy::Synchronized
                } else {
                    vivid_sdk::StartPolicy::AfterMinimumBuffer
                },
            },
        )
    })
    .map_err(io_error)
}

#[pyfunction]
fn pause(py: Python<'_>, session: PyRef<'_, PySession>, track: PyRef<'_, PyTrack>) -> PyResult<()> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.pause(&track)).map_err(io_error)
}

/// Set track gain; `raw` is a micropercent, where `2^32` is unity and `2^33` is the protocol
/// maximum.
#[pyfunction]
fn set_audio_gain(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    raw: u64,
) -> PyResult<()> {
    let gain = AudioGain::new(raw)
        .ok_or_else(|| PyValueError::new_err("gain must be within 0..=2 * 2^32"))?;
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.set_audio_gain(&track, gain))
        .map_err(io_error)
}

#[pyfunction]
fn flush(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    new_epoch: u32,
) -> PyResult<()> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.flush(&track, new_epoch))
        .map_err(io_error)
}

#[pyfunction]
fn drain(py: Python<'_>, session: PyRef<'_, PySession>, track: PyRef<'_, PyTrack>) -> PyResult<()> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.drain(&track)).map_err(io_error)
}

// ---------------------------------------------------------------------------
// Scene nodes and multi-slot activation
// ---------------------------------------------------------------------------

fn scene_node_from_config(
    session: &Session,
    surface: &Surface,
    config: &Bound<'_, PyDict>,
) -> PyResult<SceneNode> {
    let mut geometry = Vec::new();
    if let Some(dict) = config.get_item("geometry")? {
        let dict = dict
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("scene geometry must be a dict"))?;
        for (key, value) in dict.iter() {
            let key: u64 = key.extract()?;
            if let Ok(text) = value.extract::<String>() {
                geometry.push((key, Value::Text(text)));
            } else if let Ok(unsigned) = value.extract::<u64>() {
                geometry.push((key, Value::Unsigned(unsigned)));
            } else if let Ok(flag) = value.extract::<bool>() {
                geometry.push((key, Value::Bool(flag)));
            } else {
                return Err(PyValueError::new_err(
                    "scene geometry values must be unsigned integers, text, or booleans",
                ));
            }
        }
    }
    let fit = match optional::<u64>(config, "fit")? {
        None => Fit::Contain,
        Some(value) => {
            Fit::try_from(value).map_err(|error| PyValueError::new_err(error.to_string()))?
        }
    };
    Ok(SceneNode {
        owning_context_id: surface.context_id(),
        node_id: optional(config, "node_id")?
            .unwrap_or_else(|| session.allocate_id().map_err(io_error).unwrap()),
        surface_context_id: surface.context_id(),
        surface_id: surface.id(),
        geometry,
        fit,
        linear_sampling: optional(config, "linear_sampling")?.unwrap_or(true),
        z_index: optional(config, "z_index")?.unwrap_or(0),
        visible: optional(config, "visible")?.unwrap_or(true),
        opacity: optional(config, "opacity")?.unwrap_or(255),
        clip: None,
    })
}

fn scene_commit_to_pydict(py: Python<'_>, commit: SceneCommit) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("scene_revision", commit.scene_revision.get())?;
    dict.set_item("target_generation", commit.target_generation.get())?;
    Ok(dict.unbind())
}

#[pyfunction]
#[pyo3(signature = (session, surface, config))]
fn create_node(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let node = scene_node_from_config(session, &surface, config)?;
    let commit = py
        .detach(|| session.create_node(&node, &RequestMetadata::default()))
        .map_err(io_error)?;
    scene_commit_to_pydict(py, commit)
}

#[pyfunction]
#[pyo3(signature = (session, surface, config))]
fn update_node(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let node = scene_node_from_config(session, &surface, config)?;
    let commit = py
        .detach(|| session.update_node(&node, &RequestMetadata::default()))
        .map_err(io_error)?;
    scene_commit_to_pydict(py, commit)
}

/// Activate a set of slot bindings atomically; each binding is a dict with `slot`, `track_id`,
/// `expected_channel_generation`, and `required_milestone`.
#[pyfunction]
fn activate_tracks(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface: PyRef<'_, PySurface>,
    bindings: &Bound<'_, PyList>,
) -> PyResult<u64> {
    if bindings.is_empty() {
        return Err(PyValueError::new_err(
            "at least one slot binding is required",
        ));
    }
    let mut converted = Vec::with_capacity(bindings.len());
    for item in bindings.iter() {
        let binding = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each slot binding must be a dict"))?;
        converted.push(SlotBinding {
            slot: required(binding, "slot")?,
            track_id: required(binding, "track_id")?,
            expected_channel_generation: vivid_protocol::revision::ChannelGeneration::new(
                required::<u64>(binding, "expected_channel_generation")?,
            ),
            required_milestone: optional(binding, "required_milestone")?.unwrap_or(1 << 4),
        });
    }
    let surface = surface.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| session.activate_tracks(&surface, &converted, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn conpty_anchor_marker(
    session: PyRef<'_, PySession>,
    context_id: u64,
    anchor_id: u64,
) -> PyResult<String> {
    let guard = lock(&session.inner, "session")?;
    guard
        .as_ref()
        .ok_or_else(closed_session)?
        .conpty_anchor_marker(context_id, anchor_id)
        .map_err(io_error)
}

// ---------------------------------------------------------------------------
// Input lanes
// ---------------------------------------------------------------------------

fn input_binding_from_config(config: &Bound<'_, PyDict>) -> PyResult<InputBinding> {
    Ok(InputBinding {
        producer_epoch: vivid_protocol::revision::InputEpoch::new(required::<u64>(
            config,
            "producer_epoch",
        )?),
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(required::<u64>(
            config,
            "surface_generation",
        )?),
        requested_classes: required(config, "requested_classes")?,
        reason: optional(config, "reason")?.unwrap_or(1),
        requested_watchdog_us: optional(config, "requested_watchdog_us")?.unwrap_or(1_000_000),
    })
}

fn input_binding_status_to_pydict(py: Python<'_>, status: InputBindingStatus) -> Py<PyDict> {
    let dict = PyDict::new(py);
    let _ = dict.set_item("producer_epoch", status.producer_epoch);
    let _ = dict.set_item("grant_generation", status.grant_generation);
    let _ = dict.set_item("context_id", status.context_id);
    let _ = dict.set_item("surface_id", status.surface_id);
    let _ = dict.set_item("surface_generation", status.surface_generation);
    let _ = dict.set_item("effective_classes", status.effective_classes);
    let _ = dict.set_item("state", status.state);
    let _ = dict.set_item("reason", status.reason);
    let _ = dict.set_item("watchdog_timeout_us", status.watchdog_timeout_us);
    dict.unbind()
}

fn input_tuple_to_pydict(py: Python<'_>, binding: &InputTuple) -> Py<PyDict> {
    let dict = PyDict::new(py);
    let _ = dict.set_item("producer_epoch", binding.producer_epoch.get());
    let _ = dict.set_item("grant_generation", binding.grant_generation.get());
    let _ = dict.set_item("context_id", binding.context_id);
    let _ = dict.set_item("surface_id", binding.surface_id);
    dict.unbind()
}

fn input_lane_event_to_pydict(py: Python<'_>, event: InputLaneEvent) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    match event {
        InputLaneEvent::Input {
            record_type,
            surface_id,
            payload,
        } => {
            dict.set_item("kind", "input")?;
            dict.set_item("record_type", record_type)?;
            dict.set_item("surface_id", surface_id)?;
            dict.set_item("payload", payload_to_pydict(py, &payload)?)?;
        }
        InputLaneEvent::Renew(renewal) => {
            dict.set_item("kind", "renew")?;
            dict.set_item("binding", input_tuple_to_pydict(py, &renewal.binding))?;
            dict.set_item("renewal_sequence", renewal.renewal_sequence)?;
            dict.set_item("watchdog_timeout_us", renewal.watchdog_timeout_us)?;
        }
        InputLaneEvent::Revoked(termination) => {
            dict.set_item("kind", "revoked")?;
            dict.set_item("binding", input_tuple_to_pydict(py, &termination.binding))?;
            dict.set_item("reason", termination.reason)?;
        }
        InputLaneEvent::Reset(termination) => {
            dict.set_item("kind", "reset")?;
            dict.set_item("binding", input_tuple_to_pydict(py, &termination.binding))?;
            dict.set_item("reason", termination.reason)?;
        }
        InputLaneEvent::LaneClosed { diagnostic } => {
            dict.set_item("kind", "lane_closed")?;
            dict.set_item("diagnostic", diagnostic)?;
        }
        InputLaneEvent::Error(presenter_error) => {
            dict.set_item("kind", "error")?;
            dict.set_item("code", presenter_error.code)?;
            dict.set_item("message", presenter_error.to_string())?;
        }
    }
    Ok(dict.unbind())
}

/// Open a desktop-input lane; requires the `desktop-input-v1` profile.
#[pyfunction]
fn open_input_lane(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    lane_generation: u64,
) -> PyResult<PyInputLane> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let lane = py
        .detach(|| session.open_input_lane(lane_generation))
        .map_err(io_error)?;
    Ok(PyInputLane {
        inner: Mutex::new(Some(lane)),
    })
}

#[pyfunction]
fn close_input_lane(py: Python<'_>, lane: PyRef<'_, PyInputLane>) -> PyResult<()> {
    let value = lock(&lane.inner, "input lane")?
        .take()
        .ok_or_else(|| ClosedHandleError::new_err("input lane is closed"))?;
    py.detach(|| value.close()).map_err(io_error)
}

#[pyfunction]
fn set_input_binding(
    py: Python<'_>,
    lane: PyRef<'_, PyInputLane>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let binding = input_binding_from_config(config)?;
    let guard = lock(&lane.inner, "input lane")?;
    let lane = guard
        .as_ref()
        .ok_or_else(|| ClosedHandleError::new_err("input lane is closed"))?;
    let status = py.detach(|| lane.set_binding(&binding)).map_err(io_error)?;
    Ok(input_binding_status_to_pydict(py, status))
}

#[pyfunction]
fn lane_take_event(py: Python<'_>, lane: PyRef<'_, PyInputLane>) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&lane.inner, "input lane")?;
    let lane = guard
        .as_ref()
        .ok_or_else(|| ClosedHandleError::new_err("input lane is closed"))?;
    match lane.take_event().map_err(io_error)? {
        Some(event) => Ok(Some(input_lane_event_to_pydict(py, event)?)),
        None => Ok(None),
    }
}

/// Wait up to `timeout_us` for the next input event on a worker thread.
#[pyfunction]
#[pyo3(signature = (lane, *, timeout_us=1_000_000))]
fn lane_wait_event(
    py: Python<'_>,
    lane: PyRef<'_, PyInputLane>,
    timeout_us: u64,
) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&lane.inner, "input lane")?;
    let lane_ref = guard
        .as_ref()
        .ok_or_else(|| ClosedHandleError::new_err("input lane is closed"))?;
    match py
        .detach(|| lane_ref.wait_event(Duration::from_micros(timeout_us)))
        .map_err(io_error)?
    {
        Some(event) => Ok(Some(input_lane_event_to_pydict(py, event)?)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// File drop
// ---------------------------------------------------------------------------

fn file_drop_binding_from_config(config: &Bound<'_, PyDict>) -> PyResult<FileDropBinding> {
    let destination = match optional::<u64>(config, "destination")? {
        None => None,
        Some(value) => Some(
            FileDropDestination::try_from(value)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
        ),
    };
    Ok(FileDropBinding {
        producer_epoch: vivid_protocol::revision::FileDropEpoch::new(required::<u64>(
            config,
            "producer_epoch",
        )?),
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(required::<u64>(
            config,
            "surface_generation",
        )?),
        destination,
        maximum_file_bytes: required(config, "maximum_file_bytes")?,
        maximum_pending_offers: optional(config, "maximum_pending_offers")?.unwrap_or(8),
        maximum_active_transfers: optional(config, "maximum_active_transfers")?.unwrap_or(4),
        maximum_record_body: required::<u32>(config, "maximum_record_body")?,
        acceptance_timeout_us: optional(config, "acceptance_timeout_us")?.unwrap_or(20_000_000),
        idle_timeout_us: optional(config, "idle_timeout_us")?.unwrap_or(5_000_000),
    })
}

fn file_drop_tuple_to_pydict(py: Python<'_>, binding: &FileDropTuple) -> Py<PyDict> {
    let dict = PyDict::new(py);
    let _ = dict.set_item("producer_epoch", binding.producer_epoch.get());
    let _ = dict.set_item("grant_generation", binding.grant_generation.get());
    let _ = dict.set_item("context_id", binding.context_id);
    let _ = dict.set_item("surface_id", binding.surface_id);
    let _ = dict.set_item("surface_generation", binding.surface_generation.get());
    let _ = dict.set_item("drop_id", binding.drop_id);
    dict.unbind()
}

fn file_drop_tuple_from_config(config: &Bound<'_, PyDict>) -> PyResult<FileDropTuple> {
    Ok(FileDropTuple {
        producer_epoch: vivid_protocol::revision::FileDropEpoch::new(required::<u64>(
            config,
            "producer_epoch",
        )?),
        grant_generation: vivid_protocol::revision::FileDropGrantGeneration::new(required::<u64>(
            config,
            "grant_generation",
        )?),
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(required::<u64>(
            config,
            "surface_generation",
        )?),
        drop_id: required(config, "drop_id")?,
    })
}

fn file_drop_grant_to_pydict(
    py: Python<'_>,
    grant: vivid_protocol::file_drop::FileDropGrant,
) -> Py<PyDict> {
    let dict = PyDict::new(py);
    let _ = dict.set_item("producer_epoch", grant.producer_epoch.get());
    let _ = dict.set_item("grant_generation", grant.grant_generation.get());
    let _ = dict.set_item("context_id", grant.context_id);
    let _ = dict.set_item("surface_id", grant.surface_id);
    let _ = dict.set_item("surface_generation", grant.surface_generation.get());
    let _ = dict.set_item("state", grant.state as u64);
    match grant.destination {
        Some(destination) => {
            let _ = dict.set_item("destination", destination as u64);
        }
        None => {
            let _ = dict.set_item("destination", py.None());
        }
    }
    let _ = dict.set_item("maximum_file_bytes", grant.maximum_file_bytes);
    let _ = dict.set_item("maximum_pending_offers", grant.maximum_pending_offers);
    let _ = dict.set_item("maximum_active_transfers", grant.maximum_active_transfers);
    let _ = dict.set_item("maximum_record_body", grant.maximum_record_body);
    let _ = dict.set_item("acceptance_timeout_us", grant.acceptance_timeout_us);
    let _ = dict.set_item("idle_timeout_us", grant.idle_timeout_us);
    let _ = dict.set_item("reason", grant.reason);
    dict.unbind()
}

#[pyclass(name = "IncomingFileTransfer", module = "vivid_sdk._native")]
struct PyIncomingFileTransfer {
    inner: Mutex<Option<IncomingFileTransfer>>,
}

#[pymethods]
impl PyIncomingFileTransfer {
    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "file transfer")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "file transfer")?;
        Ok(match guard.as_ref() {
            Some(transfer) => format!(
                "<vivid_sdk.IncomingFileTransfer drop_id={} transfer_id={}>",
                transfer.request().drop_id,
                transfer.request().transfer_id
            ),
            None => "<vivid_sdk.IncomingFileTransfer closed=True>".into(),
        })
    }
}

fn transfer_event_to_pydict(
    py: Python<'_>,
    event: IncomingFileTransferEvent,
) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    match event {
        IncomingFileTransferEvent::Data { offset, bytes } => {
            dict.set_item("kind", "data")?;
            dict.set_item("offset", offset)?;
            dict.set_item("bytes", PyBytes::new(py, &bytes))?;
        }
        IncomingFileTransferEvent::Finished(finish) => {
            dict.set_item("kind", "finished")?;
            dict.set_item("final_length", finish.final_length)?;
        }
        IncomingFileTransferEvent::Aborted(abort) => {
            dict.set_item("kind", "aborted")?;
            dict.set_item("reason", abort.reason)?;
            dict.set_item("final_offset", abort.final_offset)?;
        }
    }
    Ok(dict.unbind())
}

fn transfer_request_from_config(
    config: &Bound<'_, PyDict>,
) -> PyResult<IncomingFileTransferRequest> {
    Ok(IncomingFileTransferRequest {
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        producer_epoch: vivid_protocol::revision::FileDropEpoch::new(required::<u64>(
            config,
            "producer_epoch",
        )?),
        grant_generation: vivid_protocol::revision::FileDropGrantGeneration::new(required::<u64>(
            config,
            "grant_generation",
        )?),
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(required::<u64>(
            config,
            "surface_generation",
        )?),
        drop_id: required(config, "drop_id")?,
        transfer_id: required(config, "transfer_id")?,
        transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(
            required::<u64>(config, "transfer_generation")?,
        ),
        resume_offset: optional(config, "resume_offset")?.unwrap_or(0),
        declared_length: required(config, "declared_length")?,
        maximum_record_body: required::<u32>(config, "maximum_record_body")?,
        maximum_body_bytes: required(config, "maximum_body_bytes")?,
        maximum_records: required(config, "maximum_records")?,
    })
}

/// Enable, replace, or disable a surface's file-drop binding.
#[pyfunction]
fn set_file_drop_binding(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let binding = file_drop_binding_from_config(config)?;
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let grant = py
        .detach(|| session.set_file_drop_binding(&binding, &RequestMetadata::default()))
        .map_err(io_error)?;
    Ok(file_drop_grant_to_pydict(py, grant))
}

#[pyfunction]
fn accept_file_drop(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let binding = file_drop_tuple_from_config(config)?;
    let acceptance = AcceptFileDrop {
        binding,
        transfer_id: required(config, "transfer_id")?,
        transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(
            required::<u64>(config, "transfer_generation")?,
        ),
        maximum_record_body: required::<u32>(config, "maximum_record_body")?,
        initial_maximum_body_bytes: required(config, "initial_maximum_body_bytes")?,
        initial_maximum_records: required(config, "initial_maximum_records")?,
    };
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let accepted = py
        .detach(|| session.accept_file_drop(acceptance, &RequestMetadata::default()))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("drop_id", accepted.drop_id)?;
    dict.set_item("transfer_id", accepted.transfer_id)?;
    dict.set_item("transfer_generation", accepted.transfer_generation.get())?;
    dict.set_item("open_timeout_us", accepted.open_timeout_us)?;
    Ok(dict.unbind())
}

#[pyfunction]
fn cancel_file_drop(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
    reason: u64,
) -> PyResult<()> {
    let cancellation = CancelFileDrop {
        binding: file_drop_tuple_from_config(config)?,
        reason,
    };
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    py.detach(|| session.cancel_file_drop(cancellation, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn advance_file_transfer(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Py<PyDict>> {
    let advance = AdvanceFileTransfer {
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        drop_id: required(config, "drop_id")?,
        transfer_id: required(config, "transfer_id")?,
        expected_generation: vivid_protocol::revision::FileTransferGeneration::new(
            required::<u64>(config, "expected_generation")?,
        ),
        new_generation: vivid_protocol::revision::FileTransferGeneration::new(required::<u64>(
            config,
            "new_generation",
        )?),
        committed_offset: required(config, "committed_offset")?,
        maximum_body_bytes: required(config, "maximum_body_bytes")?,
        maximum_records: required(config, "maximum_records")?,
    };
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let advanced = py
        .detach(|| session.advance_file_transfer(advance, &RequestMetadata::default()))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("transfer_id", advanced.transfer_id)?;
    dict.set_item("generation", advanced.generation.get())?;
    dict.set_item("committed_offset", advanced.committed_offset)?;
    dict.set_item("open_timeout_us", advanced.open_timeout_us)?;
    Ok(dict.unbind())
}

#[pyfunction]
fn query_file_drop(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    drop_id: u64,
) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let status = py
        .detach(|| session.query_file_drop(QueryFileDrop { drop_id }, &RequestMetadata::default()))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("drop_id", status.drop_id)?;
    dict.set_item("state", status.state as u64)?;
    dict.set_item("transfer_id", status.transfer_id)?;
    dict.set_item("generation", status.generation.get())?;
    dict.set_item("committed_offset", status.committed_offset)?;
    match status.result {
        Some(code) => dict.set_item("result", code as u64)?,
        None => dict.set_item("result", PyNone::get(py))?,
    }
    dict.set_item("final_name", &status.final_name)?;
    Ok(dict.unbind())
}

/// Take over an incoming transfer connection after `accept_file_drop` succeeds.
#[pyfunction]
fn open_incoming_file_transfer(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<PyIncomingFileTransfer> {
    let request = transfer_request_from_config(config)?;
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let transfer = py
        .detach(|| session.open_incoming_file_transfer(request))
        .map_err(io_error)?;
    Ok(PyIncomingFileTransfer {
        inner: Mutex::new(Some(transfer)),
    })
}

/// Read the next transfer event; bound reads with `set_read_deadline` first.
#[pyfunction]
fn read_transfer_event(
    py: Python<'_>,
    transfer: PyRef<'_, PyIncomingFileTransfer>,
) -> PyResult<Py<PyDict>> {
    let mut guard = lock(&transfer.inner, "file transfer")?;
    let transfer = guard
        .as_mut()
        .ok_or_else(|| ClosedHandleError::new_err("file transfer is closed"))?;
    let event = py
        .detach(|| IncomingFileTransfer::read_event(transfer))
        .map_err(io_error)?;
    transfer_event_to_pydict(py, event)
}

#[pyfunction]
fn set_transfer_read_deadline(
    transfer: PyRef<'_, PyIncomingFileTransfer>,
    timeout_us: Option<u64>,
) -> PyResult<()> {
    let mut guard = lock(&transfer.inner, "file transfer")?;
    let transfer = guard
        .as_mut()
        .ok_or_else(|| ClosedHandleError::new_err("file transfer is closed"))?;
    transfer
        .set_read_deadline(timeout_us.map(Duration::from_micros))
        .map_err(io_error)
}

/// Grant the sender flow capacity: `maximum_body_bytes` in flight and `maximum_records` records.
#[pyfunction]
fn grant_transfer(
    py: Python<'_>,
    transfer: PyRef<'_, PyIncomingFileTransfer>,
    maximum_body_bytes: u64,
    maximum_records: u64,
) -> PyResult<()> {
    let mut guard = lock(&transfer.inner, "file transfer")?;
    let transfer = guard
        .as_mut()
        .ok_or_else(|| ClosedHandleError::new_err("file transfer is closed"))?;
    py.detach(|| transfer.grant(maximum_body_bytes, maximum_records))
        .map_err(io_error)
}

/// Send the final result after all bytes are on disk; `committed_path` requires
/// `file-drop-path-v1`.
#[pyfunction]
fn send_transfer_result(
    transfer: PyRef<'_, PyIncomingFileTransfer>,
    config: &Bound<'_, PyDict>,
) -> PyResult<()> {
    let result = FileResult {
        transfer_id: required(config, "transfer_id")?,
        transfer_generation: vivid_protocol::revision::FileTransferGeneration::new(
            required::<u64>(config, "transfer_generation")?,
        ),
        result: vivid_protocol::file_drop::FileResultCode::try_from(required::<u64>(
            config, "result",
        )?)
        .map_err(|error| PyValueError::new_err(error.to_string()))?,
        committed_length: optional(config, "committed_length")?.unwrap_or(0),
        final_name: optional(config, "final_name")?.unwrap_or_default(),
        committed_path: optional(config, "committed_path")?,
    };
    let guard = lock(&transfer.inner, "file transfer")?;
    let transfer = guard
        .as_ref()
        .ok_or_else(|| ClosedHandleError::new_err("file transfer is closed"))?;
    transfer.send_result(&result).map_err(io_error)
}

#[pyfunction]
fn abort_transfer(transfer: PyRef<'_, PyIncomingFileTransfer>, reason: u64) -> PyResult<()> {
    let guard = lock(&transfer.inner, "file transfer")?;
    let transfer = guard
        .as_ref()
        .ok_or_else(|| ClosedHandleError::new_err("file transfer is closed"))?;
    transfer.abort(reason).map_err(io_error)
}

// ---------------------------------------------------------------------------
// Contexts, session leases, and resume
// ---------------------------------------------------------------------------

fn contract_to_array(contract: &ResourceContract) -> Vec<u64> {
    // The CBOR map's keys are the resource indices, which is exactly the array order.
    let mut values = vec![0_u64; RESOURCE_COUNT];
    if let Value::Map(entries) = contract.to_value() {
        for (key, value) in entries {
            if let (Ok(index), Some(number)) = (usize::try_from(key), value.as_u64()) {
                if index < RESOURCE_COUNT {
                    values[index] = number;
                }
            }
        }
    }
    values
}

fn contract_from_array(values: Vec<u64>) -> PyResult<ResourceContract> {
    if values.len() != RESOURCE_COUNT {
        return Err(PyValueError::new_err(format!(
            "a contract array must name all {RESOURCE_COUNT} resources"
        )));
    }
    Ok(ResourceContract::new(
        values.try_into().expect("length checked above"),
    ))
}

#[pyfunction]
#[pyo3(signature = (session, *, context_id, parent_context_id, operation_classes, label="", lifetime_us=0, contract=None))]
fn create_context(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    context_id: u64,
    parent_context_id: u64,
    operation_classes: u64,
    label: &str,
    lifetime_us: u64,
    contract: Option<Vec<u64>>,
) -> PyResult<Py<PyDict>> {
    let requested_contract = match contract {
        Some(values) => contract_from_array(values)?,
        None => {
            let guard = lock(&session.inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .info()
                .resource_contract
                .clone()
        }
    };
    let definition = ContextDefinition {
        context_id,
        parent_context_id,
        operation_classes,
        label: label.to_owned(),
        lifetime_us,
        requested_contract,
    };
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let ready = py
        .detach(|| session.create_context(&definition, &RequestMetadata::default()))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("context_id", ready.context_id)?;
    dict.set_item("operation_classes", ready.operation_classes)?;
    dict.set_item("contract", contract_to_array(&ready.contract))?;
    dict.set_item("lifetime_us", ready.lifetime_us)?;
    dict.set_item("revision", ready.revision)?;
    Ok(dict.unbind())
}

/// Mint a bounded session lease. Returns the lease identity and, exactly once, the activation
/// secret hex to hand the lease holder through an authenticated channel.
#[pyfunction]
#[pyo3(signature = (session, *, context_id, lease_id, permitted_profiles, activation_timeout_us=20_000_000, disconnect_grace_us=0, cleanup_policy=1, contract=None))]
fn create_session_lease(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    context_id: u64,
    lease_id: u64,
    permitted_profiles: Vec<String>,
    activation_timeout_us: u64,
    disconnect_grace_us: u64,
    cleanup_policy: u64,
    contract: Option<Vec<u64>>,
) -> PyResult<Py<PyDict>> {
    let requested_contract = match contract {
        Some(values) => contract_from_array(values)?,
        None => {
            let guard = lock(&session.inner, "session")?;
            guard
                .as_ref()
                .ok_or_else(closed_session)?
                .info()
                .resource_contract
                .clone()
        }
    };
    let (definition, mut secret) = SessionLeaseBuilder::new(context_id, lease_id)
        .permitted_profiles(permitted_profiles)
        .activation_timeout_us(activation_timeout_us)
        .disconnect_grace_us(disconnect_grace_us)
        .cleanup_policy(
            CleanupPolicy::try_from(cleanup_policy)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
        )
        .contract(requested_contract)
        .build()
        .map_err(io_error)?;
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let ready = py
        .detach(|| session.create_session_lease(&definition, &RequestMetadata::default()))
        .map_err(io_error)?;
    let dict = PyDict::new(py);
    dict.set_item("context_id", ready.context_id)?;
    dict.set_item("lease_id", ready.lease_id)?;
    dict.set_item("state", ready.state)?;
    dict.set_item("activation_timeout_us", ready.activation_timeout_us)?;
    dict.set_item("disconnect_grace_us", ready.disconnect_grace_us)?;
    dict.set_item("cleanup_policy", ready.cleanup_policy)?;
    dict.set_item("permitted_profiles", &ready.permitted_profiles)?;
    dict.set_item("contract", contract_to_array(&ready.contract))?;
    dict.set_item("revision", ready.revision)?;
    dict.set_item(
        "activation_secret_hex",
        secret
            .take()
            .map(|value| {
                value
                    .expose()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            })
            .unwrap_or_default(),
    )?;
    Ok(dict.unbind())
}

#[pyfunction]
fn revoke_session_lease(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    context_id: u64,
    lease_id: u64,
) -> PyResult<()> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    py.detach(|| session.revoke_session_lease(context_id, lease_id, &RequestMetadata::default()))
        .map_err(io_error)
}

#[pyfunction]
fn set_observation(py: Python<'_>, session: PyRef<'_, PySession>, mask: u64) -> PyResult<()> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    py.detach(|| session.set_observation(mask))
        .map_err(io_error)
}

/// Prepare resumable authentication for a leased session. The resume key hex is capability
/// material: keep it out of logs and diagnostics, exactly like the root secret.
#[pyfunction]
fn prepare_resume(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Py<PyDict>> {
    let guard = lock(&session.inner, "session")?;
    let session = guard.as_ref().ok_or_else(closed_session)?;
    let (context_id, lease_id, session_id, resume_generation) = py.detach(|| {
        let authentication = session.resume_authentication().map_err(io_error)?;
        match authentication {
            ProducerAuthentication::Resume {
                context_id,
                lease_id,
                session_id,
                resume_generation,
                ..
            } => Ok((context_id, lease_id, session_id, resume_generation)),
            _ => Err(io_error(io::Error::new(
                io::ErrorKind::Unsupported,
                "resume authentication is only defined for resumed sessions",
            ))),
        }
    })?;
    let dict = PyDict::new(py);
    dict.set_item("context_id", context_id)?;
    dict.set_item("lease_id", lease_id)?;
    dict.set_item("session_id", session_id)?;
    dict.set_item("resume_generation", resume_generation)?;
    Ok(dict.unbind())
}

// ---------------------------------------------------------------------------
// Track sender, channel recovery, rate control, and microphone uplink
// ---------------------------------------------------------------------------

#[pyclass(name = "TrackSender", module = "vivid_sdk._native")]
struct PyTrackSender {
    inner: Mutex<Option<TrackSender>>,
}

#[pymethods]
impl PyTrackSender {
    #[getter]
    fn generation(&self) -> PyResult<u64> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(guard
            .as_ref()
            .ok_or_else(closed_channel)?
            .generation()
            .get())
    }

    #[getter]
    fn detached(&self) -> PyResult<bool> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.is_detached())
    }

    fn next_packet_id(&self) -> PyResult<u64> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.next_packet_id())
    }

    fn current_epoch(&self) -> PyResult<u32> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.current_epoch())
    }

    fn bump_epoch(&self) -> PyResult<u32> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(guard.as_ref().ok_or_else(closed_channel)?.bump_epoch())
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "track sender")?;
        Ok(match guard.as_ref() {
            Some(sender) => format!(
                "<vivid_sdk.TrackSender generation={} epoch={}>",
                sender.generation().get(),
                sender.current_epoch()
            ),
            None => "<vivid_sdk.TrackSender closed=True>".into(),
        })
    }
}

#[pyfunction]
#[pyo3(signature = (sender, data, *, packet_id=None, pts_us, dts_us=None, duration_us=0, key=false, epoch=None))]
fn sender_send_video(
    py: Python<'_>,
    sender: PyRef<'_, PyTrackSender>,
    data: Vec<u8>,
    packet_id: Option<u64>,
    pts_us: i64,
    dts_us: Option<i64>,
    duration_us: u64,
    key: bool,
    epoch: Option<u32>,
) -> PyResult<u64> {
    let guard = lock(&sender.inner, "track sender")?;
    let sender = guard.as_ref().ok_or_else(closed_channel)?;
    let packet_id = packet_id.unwrap_or_else(|| sender.next_packet_id());
    let dts_us = dts_us.unwrap_or(pts_us);
    let epoch = epoch.unwrap_or_else(|| sender.current_epoch());
    py.detach(|| {
        sender.send(&EncodedPacket::Video(VideoPacketData {
            epoch,
            packet_id,
            pts_us,
            dts_us,
            duration_us,
            key,
            data,
        }))
    })
    .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (sender, data, *, packet_id=None, pts_us, duration_us, epoch=None))]
fn sender_send_audio(
    py: Python<'_>,
    sender: PyRef<'_, PyTrackSender>,
    data: Vec<u8>,
    packet_id: Option<u64>,
    pts_us: i64,
    duration_us: u64,
    epoch: Option<u32>,
) -> PyResult<u64> {
    let guard = lock(&sender.inner, "track sender")?;
    let sender = guard.as_ref().ok_or_else(closed_channel)?;
    let packet_id = packet_id.unwrap_or_else(|| sender.next_packet_id());
    let epoch = epoch.unwrap_or_else(|| sender.current_epoch());
    py.detach(|| {
        sender.send(&EncodedPacket::Audio(AudioPacketData {
            epoch,
            packet_id,
            pts_us,
            dts_us: pts_us,
            duration_us,
            data,
        }))
    })
    .map_err(io_error)
}

/// Recover a lost channel: advance, reopen, and send the key unit, keeping packet IDs and the
/// epoch continuous. The returned sender continues the same media sequence.
#[pyfunction]
fn recover_channel(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    track: PyRef<'_, PyTrack>,
    key_unit: Vec<u8>,
) -> PyResult<PyTrackSender> {
    let track = track.inner.clone();
    let mut guard = lock(&session.inner, "session")?;
    let session = guard.as_mut().ok_or_else(closed_session)?;
    let sender = py
        .detach(|| vivid_sdk::recover_channel(session, &track, &key_unit))
        .map_err(io_error)?;
    Ok(PyTrackSender {
        inner: Mutex::new(Some(sender)),
    })
}

#[pyclass(name = "VideoRateControl", module = "vivid_sdk._native")]
struct PyVideoRateControl {
    inner: Mutex<VideoRateControl>,
}

#[pymethods]
impl PyVideoRateControl {
    #[new]
    fn new(configured_bits_per_second: u64) -> Self {
        Self {
            inner: Mutex::new(VideoRateControl::new(configured_bits_per_second)),
        }
    }

    /// Feed the pressure from each sent record; the three causes adjust the target differently.
    fn observe_send(&self, bytes: usize, pressure: &Bound<'_, PyDict>) -> PyResult<()> {
        let pressure = SendPressure {
            rate_limited: Duration::from_micros(required::<u64>(pressure, "rate_limited_us")?),
            flow_limited: Duration::from_micros(required::<u64>(pressure, "flow_limited_us")?),
            transport: Duration::from_micros(required::<u64>(pressure, "transport_us")?),
            records: required::<u64>(pressure, "records")?,
        };
        let guard = lock(&self.inner, "rate control")?;
        guard.observe_send(bytes, pressure);
        Ok(())
    }

    fn observe_audio_backlog(&self, backlog_us: u64) -> PyResult<()> {
        let guard = lock(&self.inner, "rate control")?;
        guard.observe_audio_backlog(backlog_us);
        Ok(())
    }

    /// The encoder target, if it changed since the last poll.
    fn poll(&self) -> PyResult<Option<u64>> {
        let guard = lock(&self.inner, "rate control")?;
        Ok(guard.poll())
    }

    fn snapshot(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let guard = lock(&self.inner, "rate control")?;
        let snapshot: VideoRateSnapshot = guard.snapshot();
        let dict = PyDict::new(py);
        dict.set_item(
            "configured_bits_per_second",
            snapshot.configured_bits_per_second,
        )?;
        dict.set_item("target_bits_per_second", snapshot.target_bits_per_second)?;
        dict.set_item("adjustments", snapshot.adjustments)?;
        dict.set_item("rate_limited_us", snapshot.rate_limited.as_micros() as u64)?;
        dict.set_item("flow_limited_us", snapshot.flow_limited.as_micros() as u64)?;
        dict.set_item("transport_us", snapshot.transport.as_micros() as u64)?;
        Ok(dict.unbind())
    }
}

/// Grant the presenter's microphone flow and take the PCM packets it produces.
#[pyfunction]
fn grant_audio_input(py: Python<'_>, channel: PyRef<'_, PyTrackChannel>) -> PyResult<()> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    py.detach(|| channel.grant_audio_input()).map_err(io_error)
}

#[pyfunction]
fn take_audio_input(
    py: Python<'_>,
    channel: PyRef<'_, PyTrackChannel>,
) -> PyResult<Option<Py<PyDict>>> {
    let guard = lock(&channel.inner, "track channel")?;
    let channel = guard.as_ref().ok_or_else(closed_channel)?;
    let packet = py.detach(|| channel.take_audio_input()).map_err(io_error)?;
    let Some(packet) = packet else {
        return Ok(None);
    };
    let dict = PyDict::new(py);
    dict.set_item("epoch", packet.epoch)?;
    dict.set_item("packet_id", packet.packet_id)?;
    dict.set_item("pts_us", packet.pts_us)?;
    dict.set_item("pcm", PyBytes::new(py, &packet.pcm))?;
    Ok(Some(dict.unbind()))
}

// ---------------------------------------------------------------------------
// Orchestrators: PaneSession and DesktopSession
// ---------------------------------------------------------------------------

#[pyclass(name = "PaneSession", module = "vivid_sdk._native")]
struct PyPaneSession {
    inner: Mutex<Option<SdkPaneSession>>,
}

#[pymethods]
impl PyPaneSession {
    #[staticmethod]
    fn from_env(py: Python<'_>) -> PyResult<PyPaneSession> {
        let pane = py.detach(SdkPaneSession::from_env).map_err(io_error)?;
        Ok(PyPaneSession {
            inner: Mutex::new(Some(pane)),
        })
    }

    /// Wrap an existing session, so a host layer can apply its own connect options first.
    #[staticmethod]
    fn from_session(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<PyPaneSession> {
        let owned = lock(&session.inner, "session")?
            .take()
            .ok_or_else(closed_session)?;
        let pane = py
            .detach(move || SdkPaneSession::from_session(owned))
            .map_err(io_error)?;
        Ok(PyPaneSession {
            inner: Mutex::new(Some(pane)),
        })
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "pane session")?.is_none())
    }

    /// Whether a presentation is currently retained.
    #[getter]
    fn has_presentation(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "pane session")?
            .as_ref()
            .is_some_and(|pane| pane.has_presentation()))
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "pane session")?;
        Ok(match guard.as_ref() {
            Some(pane) => format!(
                "<vivid_sdk.PaneSession has_presentation={}>",
                pane.has_presentation()
            ),
            None => "<vivid_sdk.PaneSession closed=True>".into(),
        })
    }
}

fn pane_options_from_config(config: Option<&Bound<'_, PyDict>>) -> PyResult<PaneImageOptions> {
    let Some(config) = config else {
        return Ok(PaneImageOptions::default());
    };
    Ok(PaneImageOptions {
        title: optional(config, "title")?.unwrap_or_else(|| "image".into()),
        columns: optional(config, "columns")?,
        rows: optional(config, "rows")?,
        text_layer: optional(config, "text_layer")?.unwrap_or(1),
    })
}

macro_rules! pane_op {
    ($self:expr, $py:expr, $body:expr) => {{
        let mut guard = lock(&$self.inner, "pane session")?;
        let pane = guard
            .as_mut()
            .ok_or_else(|| ClosedHandleError::new_err("pane session is closed"))?;
        $py.detach(|| $body(pane)).map_err(io_error)
    }};
}

#[pyfunction]
fn pane_show_encoded_image(
    py: Python<'_>,
    pane: &PyPaneSession,
    encoded: Vec<u8>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let options = pane_options_from_config(options)?;
    pane_op!(pane, py, |pane: &mut SdkPaneSession| pane
        .show_encoded_image_with_options(&encoded, &options))
}

#[pyfunction]
#[pyo3(signature = (pane, width, height, rgba, options=None))]
fn pane_show_rgba(
    py: Python<'_>,
    pane: &PyPaneSession,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let options = pane_options_from_config(options)?;
    pane_op!(pane, py, |pane: &mut SdkPaneSession| pane
        .show_rgba_with_options(width, height, &rgba, &options))
}

#[pyfunction]
fn pane_clear(py: Python<'_>, pane: &PyPaneSession) -> PyResult<()> {
    pane_op!(pane, py, |pane: &mut SdkPaneSession| pane.clear())
}

#[pyfunction]
fn pane_close(py: Python<'_>, pane: PyRef<'_, PyPaneSession>) -> PyResult<()> {
    let pane = lock(&pane.inner, "pane session")?
        .take()
        .ok_or_else(|| ClosedHandleError::new_err("pane session is closed"))?;
    py.detach(move || pane.close()).map_err(io_error)
}

#[pyclass(name = "DesktopSession", module = "vivid_sdk._native")]
struct PyDesktopSession {
    inner: Mutex<Option<SdkDesktopSession>>,
}

#[pymethods]
impl PyDesktopSession {
    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "desktop session")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = lock(&self.inner, "desktop session")?;
        Ok(match guard.as_ref() {
            Some(_) => "<vivid_sdk.DesktopSession closed=False>".into(),
            None => "<vivid_sdk.DesktopSession closed=True>".into(),
        })
    }
}

/// Establish a desktop presentation: desktop surface, full-target node, video track (and
/// optional audio) with senders, and an input lane when `desktop-input-v1` is accepted. Takes
/// ownership of the session.
#[pyfunction]
#[pyo3(signature = (session, surface_config, video_config, audio_config=None))]
fn establish_desktop(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    surface_config: &Bound<'_, PyDict>,
    video_config: &Bound<'_, PyDict>,
    audio_config: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyDesktopSession> {
    let owned = lock(&session.inner, "session")?
        .take()
        .ok_or_else(closed_session)?;
    let surface_definition = parse_surface(surface_config)?;
    let video_configuration = parse_track(video_config)?;
    let audio_configuration = match audio_config {
        Some(config) => Some(parse_track(config)?),
        None => None,
    };
    let desktop = py
        .detach(move || {
            SdkDesktopSession::establish(
                owned,
                surface_definition,
                video_configuration,
                audio_configuration,
            )
        })
        .map_err(io_error)?;
    Ok(PyDesktopSession {
        inner: Mutex::new(Some(desktop)),
    })
}

#[pyfunction]
fn desktop_video_track(desktop: PyRef<'_, PyDesktopSession>) -> PyResult<PyTrack> {
    let guard = lock(&desktop.inner, "desktop session")?;
    let desktop = guard.as_ref().ok_or_else(closed_session)?;
    Ok(PyTrack {
        inner: desktop.video_track().clone(),
    })
}

#[pyfunction]
fn desktop_audio_track(desktop: PyRef<'_, PyDesktopSession>) -> PyResult<Option<PyTrack>> {
    let guard = lock(&desktop.inner, "desktop session")?;
    let desktop = guard.as_ref().ok_or_else(closed_session)?;
    match desktop.audio_track() {
        Some(track) => Ok(Some(PyTrack {
            inner: track.clone(),
        })),
        None => Ok(None),
    }
}

/// Send one video access unit through the desktop session's video sender.
#[pyfunction]
#[pyo3(signature = (desktop, data, *, packet_id=None, pts_us, dts_us=None, duration_us=0, key=false, epoch=None))]
fn desktop_send_video(
    py: Python<'_>,
    desktop: &PyDesktopSession,
    data: Vec<u8>,
    packet_id: Option<u64>,
    pts_us: i64,
    dts_us: Option<i64>,
    duration_us: u64,
    key: bool,
    epoch: Option<u32>,
) -> PyResult<u64> {
    let mut guard = lock(&desktop.inner, "desktop session")?;
    let desktop = guard.as_mut().ok_or_else(closed_session)?;
    let sender = desktop.video_sender();
    let packet_id = packet_id.unwrap_or_else(|| sender.next_packet_id());
    let dts_us = dts_us.unwrap_or(pts_us);
    let epoch = epoch.unwrap_or_else(|| sender.current_epoch());
    py.detach(|| {
        sender.send(&EncodedPacket::Video(VideoPacketData {
            epoch,
            packet_id,
            pts_us,
            dts_us,
            duration_us,
            key,
            data,
        }))
    })
    .map_err(io_error)
}

#[pyfunction]
fn desktop_send_audio(
    py: Python<'_>,
    desktop: &PyDesktopSession,
    data: Vec<u8>,
    packet_id: u64,
    pts_us: i64,
    duration_us: u64,
) -> PyResult<u64> {
    let mut guard = lock(&desktop.inner, "desktop session")?;
    let desktop = guard.as_mut().ok_or_else(closed_session)?;
    let Some(sender) = desktop.audio_sender() else {
        return Err(PyValueError::new_err(
            "this desktop session has no audio track",
        ));
    };
    py.detach(|| {
        sender.send(&EncodedPacket::Audio(AudioPacketData {
            epoch: sender.current_epoch(),
            packet_id,
            pts_us,
            dts_us: pts_us,
            duration_us,
            data,
        }))
    })
    .map_err(io_error)
}

/// Wait for output readiness and atomically activate the video and audio slots.
#[pyfunction]
fn desktop_activate_slots(py: Python<'_>, desktop: &PyDesktopSession) -> PyResult<()> {
    let mut guard = lock(&desktop.inner, "desktop session")?;
    let desktop = guard.as_mut().ok_or_else(closed_session)?;
    py.detach(|| desktop.activate_slots()).map_err(io_error)
}

#[pyfunction]
fn desktop_close(py: Python<'_>, desktop: PyRef<'_, PyDesktopSession>) -> PyResult<()> {
    let desktop = lock(&desktop.inner, "desktop session")?
        .take()
        .ok_or_else(closed_session)?;
    py.detach(move || desktop.close()).map_err(io_error)
}

fn parse_surface<'py>(config: &Bound<'py, PyDict>) -> PyResult<SurfaceDefinition> {
    let role = SurfaceRole::try_from(required::<u64>(config, "role")?)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let coordinate_model = CoordinateModel::try_from(required::<u64>(config, "coordinate_model")?)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(SurfaceDefinition {
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        semantic_profile: optional(config, "semantic_profile")?
            .unwrap_or_else(|| GENERIC_CONTENT.into()),
        coordinate_model,
        logical_width: required(config, "logical_width")?,
        logical_height: required(config, "logical_height")?,
        scale_numerator: optional(config, "scale_numerator")?.unwrap_or(1),
        scale_denominator: optional(config, "scale_denominator")?.unwrap_or(1),
        rotation: optional(config, "rotation")?.unwrap_or(0),
        descriptor: SurfaceDescriptor {
            role,
            title: optional(config, "title")?.unwrap_or_default(),
            semantic_content_revision: optional(config, "semantic_content_revision")?.unwrap_or(0),
            semantic_availability: optional(config, "semantic_availability")?.unwrap_or(0),
            locator_hint: optional(config, "locator_hint")?.unwrap_or_default(),
        },
        policy: optional(config, "policy")?.unwrap_or(0),
        profile_parameters: parse_desktop_parameters(config.get_item("desktop_parameters")?)?,
    })
}

/// Typed desktop parameters from a dict; present they make the surface a desktop surface.
fn parse_desktop_parameters(value: Option<Bound<'_, PyAny>>) -> PyResult<PayloadMap> {
    let Some(any) = value else {
        return Ok(Vec::new());
    };
    if any.is_none() {
        return Ok(Vec::new());
    }
    let config = any
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("desktop_parameters must be a dict"))?;
    let topology_dict = config
        .get_item("topology")?
        .ok_or_else(|| PyValueError::new_err("desktop_parameters need a topology"))?;
    let topology_list = topology_dict
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("topology must be a list"))?;
    let mut topology = Vec::with_capacity(topology_list.len());
    for item in topology_list.iter() {
        let output = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each topology entry must be a dict"))?;
        topology.push(OutputDescriptor {
            output_id: required(output, "output_id")?,
            origin_x: required(output, "origin_x")?,
            origin_y: required(output, "origin_y")?,
            width: required(output, "width")?,
            height: required(output, "height")?,
            scale_numerator: required(output, "scale_numerator")?,
            scale_denominator: required(output, "scale_denominator")?,
            rotation: Rotation::try_from(required::<u64>(output, "rotation")?)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            primary: required(output, "primary")?,
        });
    }
    let parameters = DesktopSurfaceParameters {
        captured_origin_x: required(config, "captured_origin_x")?,
        captured_origin_y: required(config, "captured_origin_y")?,
        topology,
        semantic_generation: required(config, "semantic_generation")?,
        input_capabilities: required(config, "input_capabilities")?,
    };
    Ok(parameters.encode())
}

fn parse_track(config: &Bound<'_, PyDict>) -> PyResult<TrackConfiguration> {
    let kind_name: String = required(config, "kind")?;
    let kind = match kind_name.as_str() {
        "video" => KindConfiguration::Video(VideoConfiguration {
            codec: required(config, "codec")?,
            packetization: required(config, "packetization")?,
            extradata: optional(config, "extradata")?.unwrap_or_default(),
            coded_width: required(config, "width")?,
            coded_height: required(config, "height")?,
            profile: optional(config, "profile")?.unwrap_or(0),
            level: optional(config, "level")?.unwrap_or(0),
            maximum_reorder_depth: optional(config, "maximum_reorder_depth")?.unwrap_or(0),
            color_primaries: optional(config, "color_primaries")?.unwrap_or(1),
            transfer: optional(config, "transfer")?.unwrap_or(1),
            matrix: optional(config, "matrix")?.unwrap_or(1),
            signal_range: optional(config, "signal_range")?.unwrap_or(2),
            aspect_numerator: optional(config, "aspect_numerator")?.unwrap_or(1),
            aspect_denominator: optional(config, "aspect_denominator")?.unwrap_or(1),
            maximum_access_unit_bytes: required(config, "maximum_access_unit_bytes")?,
            codec_string: optional(config, "codec_string")?,
            decoder_configuration: optional(config, "decoder_configuration")?,
        }),
        "audio" => KindConfiguration::Audio(AudioConfiguration {
            codec: required(config, "codec")?,
            packetization: required(config, "packetization")?,
            extradata: optional(config, "extradata")?.unwrap_or_default(),
            sample_rate: required(config, "sample_rate")?,
            channels: required(config, "channels")?,
            channel_mask: optional(config, "channel_mask")?.unwrap_or(0),
            maximum_access_unit_bytes: required(config, "maximum_access_unit_bytes")?,
            codec_string: optional(config, "codec_string")?,
        }),
        "raster" => KindConfiguration::Raster(RasterConfiguration {
            width: required(config, "width")?,
            height: required(config, "height")?,
            alpha_mode: optional(config, "alpha_mode")?.unwrap_or(1),
            delta_enabled: optional(config, "delta_enabled")?.unwrap_or(false),
            maximum_delta_operations: optional(config, "maximum_delta_operations")?.unwrap_or(1),
            zstd_enabled: optional(config, "zstd_enabled")?.unwrap_or(false),
        }),
        "image" => KindConfiguration::EncodedImage(ImageConfiguration {
            encoding: required(config, "encoding")?,
            width: required(config, "width")?,
            height: required(config, "height")?,
            encoded_length: required(config, "encoded_length")?,
            sha256: optional::<Vec<u8>>(config, "sha256")?
                .map(|value| {
                    value
                        .try_into()
                        .map_err(|_| PyValueError::new_err("sha256 must contain 32 bytes"))
                })
                .transpose()?,
            cache_lookup: optional(config, "cache_lookup")?.unwrap_or(false),
        }),
        _ => {
            return Err(PyValueError::new_err(
                "track kind must be video, audio, raster, or image",
            ));
        }
    };
    let mode = TrackMode::try_from(optional(config, "mode")?.unwrap_or(1))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let lane = LaneClass::try_from(optional(config, "lane")?.unwrap_or(3))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(TrackConfiguration {
        direction: Default::default(),
        context_id: required(config, "context_id")?,
        surface_id: required(config, "surface_id")?,
        track_id: required(config, "track_id")?,
        slot: required(config, "slot")?,
        mode,
        lane,
        maximum_record_body: required(config, "maximum_record_body")?,
        maximum_rate_millihertz: required(config, "maximum_rate_millihertz")?,
        maximum_encoded_bits_per_second: required(config, "maximum_encoded_bits_per_second")?,
        maximum_records_per_second: required(config, "maximum_records_per_second")?,
        maximum_inflight_body_bytes: required(config, "maximum_inflight_body_bytes")?,
        kind,
        target_latency_us: optional(config, "target_latency_us")?.unwrap_or(0),
        maximum_latency_us: optional(config, "maximum_latency_us")?.unwrap_or(0),
        retained_pixel_charge: optional(config, "retained_pixel_charge")?.unwrap_or(0),
    })
}

fn required<'py, T>(dict: &Bound<'py, PyDict>, name: &str) -> PyResult<T>
where
    for<'a> T: FromPyObject<'a, 'py, Error = PyErr>,
{
    dict.get_item(name)?
        .ok_or_else(|| PyValueError::new_err(format!("missing configuration field {name}")))?
        .extract()
}

fn optional<'py, T>(dict: &Bound<'py, PyDict>, name: &str) -> PyResult<Option<T>>
where
    for<'a> T: FromPyObject<'a, 'py, Error = PyErr>,
{
    dict.get_item(name)?
        .map(|value| value.extract())
        .transpose()
}

fn with_session<T>(
    session: &PySession,
    operation: impl FnOnce(&Session) -> io::Result<T>,
) -> PyResult<T> {
    let guard = lock(&session.inner, "session")?;
    operation(guard.as_ref().ok_or_else(closed_session)?).map_err(io_error)
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Raster => "raster",
        TrackKind::EncodedImage => "image",
        TrackKind::VectorScene => "vector",
    }
}

#[pyfunction]
#[pyo3(signature = (endpoint, desktop=None, retained_bytes=None))]
fn presenter_start(
    py: Python<'_>,
    endpoint: &str,
    desktop: Option<(u32, u32)>,
    retained_bytes: Option<u64>,
) -> PyResult<PyPresenter> {
    let mut media = MediaConfig::default();
    if let Some(bytes) = retained_bytes {
        media.aggregate_retained_bytes = bytes;
    }
    let config = match desktop {
        Some((width, height)) => PresenterConfig::desktop(media, desktop_target(width, height)),
        None => PresenterConfig::terminal(media),
    };

    let (presenter, endpoint) = py
        .detach(|| {
            let listener = SocketListener::bind(endpoint)?;
            let resolved = listener.endpoint();
            let presenter = VirtualVivid::start_configured(listener, config, None)?;
            io::Result::Ok((presenter, resolved))
        })
        .map_err(io_error)?;

    Ok(PyPresenter {
        inner: Mutex::new(Some(presenter)),
        endpoint,
    })
}

fn desktop_target(width: u32, height: u32) -> DesktopTarget {
    DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width,
        height,
        settled: true,
        topology_revision: 1,
        outputs: vec![OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width,
            height,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: Rotation::None,
            primary: true,
        }],
    }
}

#[pyfunction]
fn presenter_close(py: Python<'_>, presenter: PyRef<'_, PyPresenter>) -> PyResult<()> {
    let taken = lock(&presenter.inner, "presenter")?.take();
    // Dropping signals shutdown and wakes the accept and delivery waiters. Do it off the GIL: the
    // presenter's threads may be mid-write when it happens.
    py.detach(move || drop(taken));
    Ok(())
}

#[pyfunction]
fn presenter_issue_pane_capability(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
) -> PyResult<String> {
    let guard = lock(&presenter.inner, "presenter")?;
    let value = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| value.issue_pane_capability(pane))
        .map_err(io_error)
}

#[pyfunction]
fn presenter_revoke_pane(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
) -> PyResult<()> {
    let guard = lock(&presenter.inner, "presenter")?;
    let value = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| value.revoke_pane(pane));
    Ok(())
}

#[pyfunction]
fn presenter_update_metrics(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    columns: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
) -> PyResult<()> {
    let guard = lock(&presenter.inner, "presenter")?;
    let value = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| value.update_metrics(pane, columns, rows, (cell_width, cell_height)));
    Ok(())
}

#[pyfunction]
fn presenter_wait_for_media(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    timeout_us: u64,
) -> PyResult<bool> {
    let guard = lock(&presenter.inner, "presenter")?;
    let value = guard.as_ref().ok_or_else(closed_presenter)?;
    Ok(py.detach(|| value.wait_for_retained_media(pane, Duration::from_micros(timeout_us))))
}

#[pyfunction]
fn presenter_capture_pane(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    viewport_offset: usize,
) -> PyResult<Py<PyDict>> {
    let capture = {
        let guard = lock(&presenter.inner, "presenter")?;
        let value = guard.as_ref().ok_or_else(closed_presenter)?;
        py.detach(|| value.capture_pane(pane, viewport_offset))
    };

    let layers = PyList::empty(py);
    for layer in &capture.layers {
        let entry = PyDict::new(py);
        entry.set_item("source", source_key(py, layer.source)?)?;
        entry.set_item("node_id", layer.node_id)?;
        entry.set_item("z_index", layer.z_index)?;
        entry.set_item("x", layer.x)?;
        entry.set_item("y", layer.y)?;
        entry.set_item("width", layer.width)?;
        entry.set_item("height", layer.height)?;
        let content = PyDict::new(py);
        match &layer.content {
            CaptureContent::Raster(raster) => {
                content.set_item("kind", "raster")?;
                content.set_item("epoch", raster.epoch)?;
                content.set_item("frame_id", raster.frame_id)?;
                content.set_item("width", raster.width)?;
                content.set_item("height", raster.height)?;
                // A copy, deliberately. The retained buffer is behind the presenter's mutex and is
                // mutated by media threads; a memoryview into it would outlive the guard.
                content.set_item("rgba", PyBytes::new(py, &raster.pixels))?;
            }
            CaptureContent::EncodedImage(bytes) => {
                content.set_item("kind", "encoded_image")?;
                content.set_item("data", PyBytes::new(py, bytes))?;
            }
        }
        entry.set_item("content", content)?;
        layers.append(entry)?;
    }

    let skipped = PyList::empty(py);
    for entry in &capture.skipped {
        let item = PyDict::new(py);
        item.set_item("source", source_key(py, entry.source)?)?;
        item.set_item("node_id", entry.node_id)?;
        item.set_item("reason", entry.reason.as_str())?;
        skipped.append(item)?;
    }

    let result = PyDict::new(py);
    result.set_item("layers", layers)?;
    result.set_item("skipped", skipped)?;
    Ok(result.unbind())
}

#[pyfunction]
fn presenter_pane_media_summary(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
) -> PyResult<Py<PyDict>> {
    let summary = {
        let guard = lock(&presenter.inner, "presenter")?;
        let value = guard.as_ref().ok_or_else(closed_presenter)?;
        py.detach(|| value.pane_media_summary(pane))
    };

    let tracks = PyList::empty(py);
    for track in &summary.tracks {
        let entry = PyDict::new(py);
        entry.set_item("source", source_key(py, track.source)?)?;
        entry.set_item("kind", track.kind)?;
        entry.set_item("capturable", track.capturable)?;
        tracks.append(entry)?;
    }

    let result = PyDict::new(py);
    result.set_item("surfaces", &summary.surfaces)?;
    result.set_item("tracks", tracks)?;
    Ok(result.unbind())
}

fn source_key(py: Python<'_>, source: vivid_sdk::presenter::SourceKey) -> PyResult<Py<PyDict>> {
    let entry = PyDict::new(py);
    entry.set_item("producer", source.producer)?;
    entry.set_item("context", source.context)?;
    entry.set_item("surface", source.surface)?;
    entry.set_item("track", source.track)?;
    Ok(entry.unbind())
}

fn closed_presenter() -> PyErr {
    ClosedHandleError::new_err("presenter is closed")
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> PyResult<MutexGuard<'a, T>> {
    Python::attach(|py| mutex.lock_py_attached(py))
        .map_err(|_| VividError::new_err(format!("{name} lock is poisoned")))
}

fn closed_session() -> PyErr {
    ClosedHandleError::new_err("session is closed")
}

fn closed_channel() -> PyErr {
    ClosedHandleError::new_err("track channel is closed")
}

fn value_error(error: io::Error) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn io_error(error: io::Error) -> PyErr {
    if error.kind() == io::ErrorKind::InvalidInput {
        PyValueError::new_err(error.to_string())
    } else {
        VividError::new_err(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Presenter remainder
// ---------------------------------------------------------------------------

fn presenter_lock<'a>(
    presenter: &'a PyPresenter,
    what: &'static str,
) -> PyResult<std::sync::MutexGuard<'a, Option<VirtualVivid>>> {
    lock(&presenter.inner, what)
}

fn source_key_from_config(config: &Bound<'_, PyDict>) -> PyResult<vivid_sdk::presenter::SourceKey> {
    Ok(vivid_sdk::presenter::SourceKey {
        producer: required(config, "producer")?,
        context: required(config, "context")?,
        surface: required(config, "surface")?,
        track: required(config, "track")?,
    })
}

#[pyfunction]
fn presenter_queue_microphone(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
    generation: u64,
    bytes: Vec<u8>,
) -> PyResult<bool> {
    let source = source_key_from_config(config)?;
    let guard = presenter_lock(&presenter, "presenter")?;
    let presenter = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| presenter.queue_microphone(source, generation, &bytes))
        .map_err(io_error)
}

#[pyfunction]
fn presenter_revoke_microphones(presenter: PyRef<'_, PyPresenter>) -> PyResult<()> {
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .revoke_microphones();
    Ok(())
}

#[pyfunction]
fn presenter_notify_capabilities_changed(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    reason_mask: u64,
) -> PyResult<u64> {
    let guard = presenter_lock(&presenter, "presenter")?;
    let presenter = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| presenter.notify_capabilities_changed(reason_mask))
        .map_err(io_error)
}

#[pyfunction]
fn presenter_update_desktop_target(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    width: u32,
    height: u32,
    reason_mask: u64,
) -> PyResult<u64> {
    let guard = presenter_lock(&presenter, "presenter")?;
    let presenter = guard.as_ref().ok_or_else(closed_presenter)?;
    py.detach(|| presenter.update_desktop_target(pane, desktop_target(width, height), reason_mask))
        .map_err(io_error)
}

#[pyfunction]
fn presenter_observe_marker(
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    value: &str,
    row: i32,
    column: usize,
    alternate: bool,
) -> PyResult<()> {
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .observe_marker(pane, value, row, column, alternate);
    Ok(())
}

#[pyfunction]
fn presenter_scroll_anchors(
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    lines: i32,
    alternate: bool,
) -> PyResult<()> {
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .scroll_anchors(pane, lines, alternate);
    Ok(())
}

#[pyfunction]
fn presenter_clear_anchors(
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    alternate: bool,
) -> PyResult<()> {
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .clear_anchors(pane, alternate);
    Ok(())
}

#[pyfunction]
fn presenter_set_alternate_screen(
    presenter: PyRef<'_, PyPresenter>,
    pane: u64,
    alternate: bool,
) -> PyResult<()> {
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .set_alternate_screen(pane, alternate);
    Ok(())
}

#[pyfunction]
fn presenter_pane_for_source(
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
) -> PyResult<Option<u64>> {
    let source = source_key_from_config(config)?;
    let guard = presenter_lock(&presenter, "presenter")?;
    Ok(guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .pane_for_source(source))
}

#[pyfunction]
fn presenter_projection_revision(presenter: PyRef<'_, PyPresenter>) -> PyResult<u64> {
    let guard = presenter_lock(&presenter, "presenter")?;
    Ok(guard.as_ref().ok_or_else(closed_presenter)?.revision())
}

#[pyfunction]
fn presenter_request_keyframe(
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
    minimum_epoch: Option<u32>,
    reason: u64,
) -> PyResult<&'static str> {
    let source = source_key_from_config(config)?;
    let guard = presenter_lock(&presenter, "presenter")?;
    let outcome = guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .request_keyframe(source, minimum_epoch, reason);
    Ok(match outcome {
        vivid_sdk::presenter::KeyframeRequestOutcome::Forwarded => "forwarded",
        vivid_sdk::presenter::KeyframeRequestOutcome::Damped => "damped",
        vivid_sdk::presenter::KeyframeRequestOutcome::Ignored => "ignored",
    })
}

#[pyfunction]
fn presenter_request_full_frames(
    presenter: PyRef<'_, PyPresenter>,
    sources: &Bound<'_, PyList>,
    reason: u64,
) -> PyResult<()> {
    let mut keys = Vec::with_capacity(sources.len());
    for item in sources.iter() {
        let config = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each source must be a dict"))?;
        keys.push(source_key_from_config(config)?);
    }
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .request_full_frames(&keys, reason);
    Ok(())
}

#[pyfunction]
fn presenter_apply_outer_position(
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
    position: &Bound<'_, PyDict>,
) -> PyResult<()> {
    let source = source_key_from_config(config)?;
    let snapshot = vivid_sdk::presenter::BridgePositionSnapshot {
        decoder_reset_serial: required(position, "decoder_reset_serial")?,
        playing: required(position, "playing")?,
        start_pts_us: required(position, "start_pts_us")?,
        state: required(position, "state")?,
        clock_pts_us: optional(position, "clock_pts_us")?,
        decoded_pts_us: required(position, "decoded_pts_us")?,
        presented_pts_us: required(position, "presented_pts_us")?,
        presentation_id: required(position, "presentation_id")?,
    };
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .apply_outer_position(source, snapshot);
    Ok(())
}

#[pyfunction]
fn presenter_apply_outer_playback(
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
    decoder_reset_serial: u64,
    state_value: u64,
    eos_state: u64,
) -> PyResult<()> {
    let source = source_key_from_config(config)?;
    let guard = presenter_lock(&presenter, "presenter")?;
    guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .apply_outer_playback(source, decoder_reset_serial, state_value, eos_state);
    Ok(())
}

/// Mint a media resource id: `pinned=true` freezes the content, otherwise it follows the surface.
#[pyfunction]
fn presenter_announce_media_resource(
    presenter: PyRef<'_, PyPresenter>,
    config: &Bound<'_, PyDict>,
    pinned: bool,
) -> PyResult<String> {
    let source = source_key_from_config(config)?;
    let binding = if pinned {
        Binding::Pinned
    } else {
        Binding::Live
    };
    let guard = presenter_lock(&presenter, "presenter")?;
    let id = guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .announce_media_resource(source, binding)
        .map_err(|error| VividError::new_err(error.to_string()))?;
    Ok(id)
}

#[pyfunction]
fn presenter_describe_media_resource(
    py: Python<'_>,
    presenter: PyRef<'_, PyPresenter>,
    id: &str,
) -> PyResult<Py<PyDict>> {
    let guard = presenter_lock(&presenter, "presenter")?;
    let description = guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .describe_media_resource(id)
        .map_err(|error| VividError::new_err(error.to_string()))?;
    let dict = PyDict::new(py);
    dict.set_item("pinned", matches!(description.binding, Binding::Pinned))?;
    dict.set_item("producer", description.producer)?;
    dict.set_item("context", description.context_id)?;
    dict.set_item("surface", description.surface_id)?;
    dict.set_item("surface_revision", description.surface_revision)?;
    dict.set_item("surface_generation", description.surface_generation)?;
    match &description.track {
        Some(facts) => {
            let track = PyDict::new(py);
            track.set_item("track_id", facts.track_id)?;
            track.set_item("revision", facts.revision)?;
            track.set_item("channel_generation", facts.channel_generation)?;
            track.set_item("media_epoch", facts.media_epoch)?;
            track.set_item("capturable", facts.capturable)?;
            dict.set_item("track", track)?;
        }
        None => dict.set_item("track", PyNone::get(py))?,
    }
    Ok(dict.unbind())
}

#[pyfunction]
fn presenter_release_media_resource(presenter: PyRef<'_, PyPresenter>, id: &str) -> PyResult<bool> {
    let guard = presenter_lock(&presenter, "presenter")?;
    Ok(guard
        .as_ref()
        .ok_or_else(closed_presenter)?
        .release_media_resource(id))
}

#[pymodule]
fn _native(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    overlay::register(module)?;
    module.add("VividError", py.get_type::<VividError>())?;
    module.add("ClosedHandleError", py.get_type::<ClosedHandleError>())?;
    module.add_class::<PyPresenter>()?;
    module.add_class::<PySession>()?;
    module.add_class::<PySurface>()?;
    module.add_class::<PyTrack>()?;
    module.add_class::<PyTrackChannel>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    module.add_function(wrap_pyfunction!(close, module)?)?;
    module.add_function(wrap_pyfunction!(abort, module)?)?;
    module.add_function(wrap_pyfunction!(take_event, module)?)?;
    module.add_function(wrap_pyfunction!(wait_event, module)?)?;
    module.add_function(wrap_pyfunction!(query_surface, module)?)?;
    module.add_function(wrap_pyfunction!(query_track, module)?)?;
    module.add_function(wrap_pyfunction!(probe_track, module)?)?;
    module.add_function(wrap_pyfunction!(query_anchor, module)?)?;
    module.add_function(wrap_pyfunction!(query_session, module)?)?;
    module.add_function(wrap_pyfunction!(play, module)?)?;
    module.add_function(wrap_pyfunction!(pause, module)?)?;
    module.add_function(wrap_pyfunction!(set_audio_gain, module)?)?;
    module.add_function(wrap_pyfunction!(flush, module)?)?;
    module.add_function(wrap_pyfunction!(drain, module)?)?;
    module.add_function(wrap_pyfunction!(create_node, module)?)?;
    module.add_function(wrap_pyfunction!(update_node, module)?)?;
    module.add_function(wrap_pyfunction!(activate_tracks, module)?)?;
    module.add_function(wrap_pyfunction!(conpty_anchor_marker, module)?)?;
    module.add_function(wrap_pyfunction!(open_input_lane, module)?)?;
    module.add_function(wrap_pyfunction!(close_input_lane, module)?)?;
    module.add_function(wrap_pyfunction!(set_input_binding, module)?)?;
    module.add_function(wrap_pyfunction!(lane_take_event, module)?)?;
    module.add_function(wrap_pyfunction!(lane_wait_event, module)?)?;
    module.add_function(wrap_pyfunction!(set_file_drop_binding, module)?)?;
    module.add_function(wrap_pyfunction!(accept_file_drop, module)?)?;
    module.add_function(wrap_pyfunction!(cancel_file_drop, module)?)?;
    module.add_function(wrap_pyfunction!(advance_file_transfer, module)?)?;
    module.add_function(wrap_pyfunction!(query_file_drop, module)?)?;
    module.add_function(wrap_pyfunction!(open_incoming_file_transfer, module)?)?;
    module.add_function(wrap_pyfunction!(read_transfer_event, module)?)?;
    module.add_function(wrap_pyfunction!(set_transfer_read_deadline, module)?)?;
    module.add_function(wrap_pyfunction!(grant_transfer, module)?)?;
    module.add_function(wrap_pyfunction!(send_transfer_result, module)?)?;
    module.add_function(wrap_pyfunction!(abort_transfer, module)?)?;
    module.add_function(wrap_pyfunction!(create_context, module)?)?;
    module.add_function(wrap_pyfunction!(create_session_lease, module)?)?;
    module.add_function(wrap_pyfunction!(revoke_session_lease, module)?)?;
    module.add_function(wrap_pyfunction!(set_observation, module)?)?;
    module.add_function(wrap_pyfunction!(prepare_resume, module)?)?;
    module.add_function(wrap_pyfunction!(constant_table, module)?)?;
    module.add_function(wrap_pyfunction!(probe_encoded_image, module)?)?;
    module.add_function(wrap_pyfunction!(build_track_config, module)?)?;
    module.add_function(wrap_pyfunction!(build_surface_config, module)?)?;
    module.add_function(wrap_pyfunction!(sender_send_video, module)?)?;
    module.add_function(wrap_pyfunction!(sender_send_audio, module)?)?;
    module.add_function(wrap_pyfunction!(recover_channel, module)?)?;
    module.add_function(wrap_pyfunction!(grant_audio_input, module)?)?;
    module.add_function(wrap_pyfunction!(take_audio_input, module)?)?;
    module.add_function(wrap_pyfunction!(pane_show_encoded_image, module)?)?;
    module.add_function(wrap_pyfunction!(pane_show_rgba, module)?)?;
    module.add_function(wrap_pyfunction!(pane_clear, module)?)?;
    module.add_function(wrap_pyfunction!(pane_close, module)?)?;
    module.add_function(wrap_pyfunction!(establish_desktop, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_video_track, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_audio_track, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_send_video, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_send_audio, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_activate_slots, module)?)?;
    module.add_function(wrap_pyfunction!(desktop_close, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_queue_microphone, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_revoke_microphones, module)?)?;
    module.add_function(wrap_pyfunction!(
        presenter_notify_capabilities_changed,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(presenter_update_desktop_target, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_observe_marker, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_scroll_anchors, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_clear_anchors, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_set_alternate_screen, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_pane_for_source, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_projection_revision, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_request_keyframe, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_request_full_frames, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_apply_outer_position, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_apply_outer_playback, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_announce_media_resource, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_describe_media_resource, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_release_media_resource, module)?)?;
    module.add_class::<PyTrackSender>()?;
    module.add_class::<PyVideoRateControl>()?;
    module.add_class::<PyPaneSession>()?;
    module.add_class::<PyDesktopSession>()?;
    module.add_class::<PyInputLane>()?;
    module.add_class::<PyIncomingFileTransfer>()?;
    module.add_function(wrap_pyfunction!(allocate_id, module)?)?;
    module.add_function(wrap_pyfunction!(supports, module)?)?;
    module.add_function(wrap_pyfunction!(session_info, module)?)?;
    module.add_function(wrap_pyfunction!(create_surface, module)?)?;
    module.add_function(wrap_pyfunction!(update_surface, module)?)?;
    module.add_function(wrap_pyfunction!(destroy_surface, module)?)?;
    module.add_function(wrap_pyfunction!(create_track, module)?)?;
    module.add_function(wrap_pyfunction!(destroy_track, module)?)?;
    module.add_function(wrap_pyfunction!(open_track_channel, module)?)?;
    module.add_function(wrap_pyfunction!(send_raster, module)?)?;
    module.add_function(wrap_pyfunction!(send_image, module)?)?;
    module.add_function(wrap_pyfunction!(send_video, module)?)?;
    module.add_function(wrap_pyfunction!(send_audio, module)?)?;
    module.add_function(wrap_pyfunction!(take_send_pressure, module)?)?;
    module.add_function(wrap_pyfunction!(media_credit_available, module)?)?;
    module.add_function(wrap_pyfunction!(send_raster_adaptive, module)?)?;
    module.add_function(wrap_pyfunction!(send_raster_delta, module)?)?;
    module.add_function(wrap_pyfunction!(send_raster_delta_adaptive, module)?)?;
    module.add_function(wrap_pyfunction!(channel_take_event, module)?)?;
    module.add_function(wrap_pyfunction!(channel_wait_event, module)?)?;
    module.add_function(wrap_pyfunction!(advance_channel, module)?)?;
    module.add_function(wrap_pyfunction!(channel_eos, module)?)?;
    module.add_function(wrap_pyfunction!(close_channel, module)?)?;
    module.add_function(wrap_pyfunction!(activate_track, module)?)?;
    module.add_function(wrap_pyfunction!(wait_track, module)?)?;
    module.add_function(wrap_pyfunction!(place_terminal_surface, module)?)?;
    module.add_function(wrap_pyfunction!(delete_node, module)?)?;
    module.add_function(wrap_pyfunction!(anchor_marker, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_start, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_close, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_issue_pane_capability, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_revoke_pane, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_update_metrics, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_wait_for_media, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_capture_pane, module)?)?;
    module.add_function(wrap_pyfunction!(presenter_pane_media_summary, module)?)?;
    Ok(())
}
