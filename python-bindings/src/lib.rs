//! Private PyO3 extension for the public `vivid_sdk` Python package.

#![allow(clippy::too_many_arguments)]

use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use pyo3::create_exception;
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::messages::{LaneClass, TrackKind};
use vivid_protocol::track::{
    AudioConfiguration, ImageConfiguration, KindConfiguration, RasterConfiguration,
    TrackConfiguration, TrackMode, VideoConfiguration,
};
use vivid_sdk::{
    CoordinateModel, GENERIC_CONTENT, ProducerAuthentication, ProducerConfig, RequestMetadata,
    Session, SlotBinding, Surface, SurfaceDefinition, SurfaceDescriptor, SurfaceRole, Track,
    TrackChannel, TrackWaitCondition,
};

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

#[pyfunction]
#[pyo3(signature = (
    *,
    dry_run=false,
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
        ..ProducerConfig::default()
    };
    if let Some(secret) = root_secret {
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

fn parse_surface(config: &Bound<'_, PyDict>) -> PyResult<SurfaceDefinition> {
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
        profile_parameters: vec![],
    })
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
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> PyResult<MutexGuard<'a, T>> {
    mutex
        .lock()
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

#[pymodule]
fn _native(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("VividError", py.get_type::<VividError>())?;
    module.add("ClosedHandleError", py.get_type::<ClosedHandleError>())?;
    module.add_class::<PySession>()?;
    module.add_class::<PySurface>()?;
    module.add_class::<PyTrack>()?;
    module.add_class::<PyTrackChannel>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    module.add_function(wrap_pyfunction!(close, module)?)?;
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
    module.add_function(wrap_pyfunction!(channel_eos, module)?)?;
    module.add_function(wrap_pyfunction!(close_channel, module)?)?;
    module.add_function(wrap_pyfunction!(activate_track, module)?)?;
    module.add_function(wrap_pyfunction!(wait_track, module)?)?;
    module.add_function(wrap_pyfunction!(place_terminal_surface, module)?)?;
    module.add_function(wrap_pyfunction!(delete_node, module)?)?;
    module.add_function(wrap_pyfunction!(anchor_marker, module)?)?;
    Ok(())
}
