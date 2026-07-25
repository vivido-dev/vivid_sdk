// Python-facing functions intentionally retain explicit signatures for generated help and stubs.
#![allow(clippy::too_many_arguments)]

use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use pyo3::create_exception;
use pyo3::exceptions::{
    PyInterruptedError, PyKeyError, PyOSError, PyOverflowError, PyTimeoutError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyModule};
use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::messages::{
    ClipRect, ImageSourceConfig, PlaybackSnapshot, SceneNodeConfig, SourceStatus, WaitSource,
};
use vivid_protocol::wire::ConnectionKind;
use vivid_sdk::{
    AudioSourceSpec, MediaSender as RustMediaSender, ObservationEvent, ProducerConfig,
    ProducerSession, SourceCancellation, SourceEvent, SourceHandle, SourceWaitCancellation,
    SourceWaitHandle, VideoSourceSpec,
};

create_exception!(_native, VividError, PyOSError);
create_exception!(_native, ClosedHandleError, VividError);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Raster,
    Image,
    Video,
    Audio,
}

impl SourceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Raster => "raster",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    fn connection(self) -> ConnectionKind {
        match self {
            Self::Raster => ConnectionKind::Raster,
            Self::Image => ConnectionKind::Blob,
            Self::Video => ConnectionKind::Video,
            Self::Audio => ConnectionKind::Audio,
        }
    }
}

#[pyclass(name = "Session", module = "vivid_sdk._native")]
struct PySession {
    inner: Mutex<Option<ProducerSession>>,
}

#[pymethods]
impl PySession {
    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "session")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<vivid_sdk.Session closed={}>",
            lock(&self.inner, "session")?.is_none()
        ))
    }
}

#[pyclass(name = "Source", module = "vivid_sdk._native")]
struct PySource {
    inner: Mutex<Option<SourceHandle>>,
    id: u64,
    kind: SourceKind,
}

#[pymethods]
impl PySource {
    #[getter]
    fn id(&self) -> u64 {
        self.id
    }

    #[getter]
    fn kind(&self) -> &'static str {
        self.kind.name()
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "source")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<vivid_sdk.Source id={} kind='{}' closed={}>",
            self.id,
            self.kind.name(),
            lock(&self.inner, "source")?.is_none()
        ))
    }
}

#[pyclass(name = "MediaSender", module = "vivid_sdk._native")]
struct PyMediaSender {
    inner: Mutex<Option<RustMediaSender>>,
    cancellation: SourceCancellation,
    id: u64,
    kind: SourceKind,
}

#[pyclass(name = "Wait", module = "vivid_sdk._native")]
struct PyWait {
    inner: Mutex<Option<SourceWaitHandle>>,
    cancellation: SourceWaitCancellation,
    request_id: u64,
    source_id: u64,
}

#[pymethods]
impl PyWait {
    #[getter]
    fn request_id(&self) -> u64 {
        self.request_id
    }

    #[getter]
    fn source_id(&self) -> u64 {
        self.source_id
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "wait")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<vivid_sdk.Wait request_id={} source_id={} closed={}>",
            self.request_id,
            self.source_id,
            lock(&self.inner, "wait")?.is_none()
        ))
    }
}

#[pymethods]
impl PyMediaSender {
    #[getter]
    fn id(&self) -> u64 {
        self.id
    }

    #[getter]
    fn kind(&self) -> &'static str {
        self.kind.name()
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(lock(&self.inner, "media sender")?.is_none())
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "<vivid_sdk.MediaSender id={} kind='{}' closed={}>",
            self.id,
            self.kind.name(),
            lock(&self.inner, "media sender")?.is_none()
        ))
    }
}

fn lock<'a, T>(
    mutex: &'a Mutex<Option<T>>,
    description: &str,
) -> PyResult<MutexGuard<'a, Option<T>>> {
    mutex
        .lock()
        .map_err(|_| VividError::new_err(format!("{description} state is poisoned")))
}

fn open_mut<'a, T>(value: &'a mut Option<T>, description: &str) -> PyResult<&'a mut T> {
    value
        .as_mut()
        .ok_or_else(|| ClosedHandleError::new_err(format!("{description} is closed or consumed")))
}

fn io_error(error: io::Error) -> PyErr {
    match error.kind() {
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            PyValueError::new_err(error.to_string())
        }
        io::ErrorKind::TimedOut => PyTimeoutError::new_err(error.to_string()),
        io::ErrorKind::Interrupted => PyInterruptedError::new_err(error.to_string()),
        _ => VividError::new_err(error.to_string()),
    }
}

fn source(handle: SourceHandle, kind: SourceKind) -> PySource {
    let id = handle.id;
    PySource {
        inner: Mutex::new(Some(handle)),
        id,
        kind,
    }
}

macro_rules! dict_value {
    ($dict:expr, $key:literal, $ty:ty) => {
        $dict
            .get_item($key)?
            .ok_or_else(|| PyKeyError::new_err($key))?
            .extract::<$ty>()?
    };
}

fn parse_video(config: &Bound<'_, PyDict>) -> PyResult<VideoSourceSpec> {
    Ok(VideoSourceSpec {
        codec: dict_value!(config, "codec", String),
        packetization: dict_value!(config, "packetization", String),
        extradata: dict_value!(config, "extradata", Vec<u8>),
        width: dict_value!(config, "width", u32),
        height: dict_value!(config, "height", u32),
        profile: dict_value!(config, "profile", i32),
        level: dict_value!(config, "level", i32),
        bitrate: dict_value!(config, "bitrate", i64),
        color_primaries: dict_value!(config, "color_primaries", u64),
        transfer: dict_value!(config, "transfer", u64),
        matrix: dict_value!(config, "matrix", u64),
        range: dict_value!(config, "range", u64),
        sar_num: dict_value!(config, "sar_num", u32),
        sar_den: dict_value!(config, "sar_den", u32),
        max_access_unit_bytes: dict_value!(config, "max_access_unit_bytes", u32),
        codec_string: dict_value!(config, "codec_string", Option<String>),
        decoder_config: dict_value!(config, "decoder_config", Option<Vec<u8>>),
    })
}

fn parse_audio(config: &Bound<'_, PyDict>) -> PyResult<AudioSourceSpec> {
    Ok(AudioSourceSpec {
        codec: dict_value!(config, "codec", String),
        packetization: dict_value!(config, "packetization", String),
        extradata: dict_value!(config, "extradata", Vec<u8>),
        sample_rate: dict_value!(config, "sample_rate", u32),
        channels: dict_value!(config, "channels", u16),
        channel_mask: dict_value!(config, "channel_mask", u64),
        bitrate: dict_value!(config, "bitrate", i64),
        max_access_unit_bytes: dict_value!(config, "max_access_unit_bytes", u32),
        codec_string: dict_value!(config, "codec_string", Option<String>),
    })
}

fn parse_clip(value: &Bound<'_, PyAny>) -> PyResult<Option<ClipRect>> {
    if value.is_none() {
        return Ok(None);
    }
    let clip = value.cast::<PyDict>()?;
    Ok(Some(ClipRect {
        x: dict_value!(clip, "x", i64),
        y: dict_value!(clip, "y", i64),
        width: dict_value!(clip, "width", i64),
        height: dict_value!(clip, "height", i64),
    }))
}

fn parse_scene(config: &Bound<'_, PyDict>) -> PyResult<SceneNodeConfig> {
    let clip = config
        .get_item("clip")?
        .ok_or_else(|| PyKeyError::new_err("clip"))?;
    Ok(SceneNodeConfig {
        node_id: dict_value!(config, "node_id", u64),
        source_id: dict_value!(config, "source_id", u64),
        context_id: dict_value!(config, "context_id", u64),
        x: dict_value!(config, "x", i64),
        y: dict_value!(config, "y", i64),
        width: dict_value!(config, "width", i64),
        height: dict_value!(config, "height", i64),
        text_layer: dict_value!(config, "text_layer", u64),
        z_index: dict_value!(config, "z_index", i64),
        visible: dict_value!(config, "visible", bool),
        anchor_id: dict_value!(config, "anchor_id", Option<u64>),
        clip: parse_clip(&clip)?,
    })
}

#[pyfunction]
#[pyo3(signature = (endpoint, bulk_endpoint, token, dry_run, trace_dir, verbose, producer, producer_version, required_features, optional_features))]
fn connect(
    py: Python<'_>,
    endpoint: Option<String>,
    bulk_endpoint: Option<String>,
    token: Option<String>,
    dry_run: bool,
    trace_dir: Option<String>,
    verbose: bool,
    producer: String,
    producer_version: String,
    required_features: Vec<u64>,
    optional_features: Vec<u64>,
) -> PyResult<PySession> {
    let config = ProducerConfig {
        endpoint,
        bulk_endpoint,
        token,
        dry_run,
        trace_dir: trace_dir.map(PathBuf::from),
        verbose,
        producer,
        producer_version,
        required_features,
        optional_features,
        allow_version_retry: false,
    };
    config.validate().map_err(io_error)?;
    let session = py
        .detach(|| ProducerSession::connect(&config).map_err(|error| error.to_string()))
        .map_err(VividError::new_err)?;
    Ok(PySession {
        inner: Mutex::new(Some(session)),
    })
}

#[pyfunction]
fn close(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let Some(mut inner) = guard.take() else {
        return Ok(());
    };
    py.detach(|| inner.goodbye()).map_err(io_error)
}

#[pyfunction]
fn allocate_id(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<u64> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.allocate_id()).map_err(io_error)
}

#[pyfunction]
fn supports(session: PyRef<'_, PySession>, feature: u64) -> PyResult<bool> {
    let mut guard = lock(&session.inner, "session")?;
    Ok(open_mut(&mut guard, "session")?.supports(feature))
}

#[pyfunction]
fn root_context_id(session: PyRef<'_, PySession>) -> PyResult<u64> {
    let mut guard = lock(&session.inner, "session")?;
    Ok(open_mut(&mut guard, "session")?.root_context_id())
}

#[pyfunction]
fn display_state(
    session: PyRef<'_, PySession>,
) -> PyResult<(u64, u32, u32, u32, u32, u32, u32, bool)> {
    let mut guard = lock(&session.inner, "session")?;
    let state = open_mut(&mut guard, "session")?.display_state();
    Ok((
        state.display_generation,
        state.viewport_width,
        state.viewport_height,
        state.grid_columns,
        state.grid_rows,
        state.cell_width,
        state.cell_height,
        state.settled,
    ))
}

#[pyfunction]
fn create_text_anchor(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Option<u64>> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.create_text_anchor()).map_err(io_error)
}

#[pyfunction]
fn create_raster_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    width: u32,
    height: u32,
) -> PyResult<PySource> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.create_raster_source(source_id, width, height))
        .map(|handle| source(handle, SourceKind::Raster))
        .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (session, source_id, encoding, width, height, encoded_length, sha256))]
fn create_image_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    encoding: u64,
    width: u32,
    height: u32,
    encoded_length: u32,
    sha256: Option<Vec<u8>>,
) -> PyResult<PySource> {
    let sha256 = sha256
        .map(|value| {
            value
                .try_into()
                .map_err(|_| PyValueError::new_err("image sha256 must contain exactly 32 bytes"))
        })
        .transpose()?;
    let config = ImageSourceConfig {
        source_id,
        encoding,
        width,
        height,
        encoded_length,
        sha256,
    };
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.create_image_source(&config))
        .map(|handle| source(handle, SourceKind::Image))
        .map_err(io_error)
}

#[pyfunction]
fn create_video_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    config: &Bound<'_, PyDict>,
) -> PyResult<PySource> {
    let config = parse_video(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.create_video_source(source_id, &config))
        .map(|handle| source(handle, SourceKind::Video))
        .map_err(io_error)
}

#[pyfunction]
#[pyo3(signature = (session, source_id, linked_video_source_id, config))]
fn create_audio_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    linked_video_source_id: Option<u64>,
    config: &Bound<'_, PyDict>,
) -> PyResult<PySource> {
    let config = parse_audio(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.create_audio_source(source_id, linked_video_source_id, &config))
        .map(|handle| source(handle, SourceKind::Audio))
        .map_err(io_error)
}

#[pyfunction]
fn create_linked_av_sources(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    video_source_id: u64,
    video_config: &Bound<'_, PyDict>,
    audio_source_id: u64,
    audio_config: &Bound<'_, PyDict>,
) -> PyResult<(PySource, Option<PySource>, Option<String>)> {
    let video_config = parse_video(video_config)?;
    let audio_config = parse_audio(audio_config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let (video, audio) = py
        .detach(|| {
            inner.create_linked_av_sources(
                video_source_id,
                &video_config,
                audio_source_id,
                &audio_config,
            )
        })
        .map_err(io_error)?;
    let video = source(video, SourceKind::Video);
    match audio {
        Ok(audio) => Ok((video, Some(source(audio, SourceKind::Audio)), None)),
        Err(error) => Ok((video, None, Some(error.to_string()))),
    }
}

#[pyfunction]
fn probe_video_config(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<bool> {
    let config = parse_video(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.probe_video_config(&config))
        .map_err(io_error)
}

#[pyfunction]
fn probe_audio_config(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<bool> {
    let config = parse_audio(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.probe_audio_config(&config))
        .map_err(io_error)
}

#[pyfunction]
fn place_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    node_id: u64,
    anchor_id: Option<u64>,
    columns: u32,
    rows: u32,
) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.place_source(source_id, node_id, anchor_id, columns, rows))
        .map_err(io_error)
}

#[pyfunction]
fn create_scene_node(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<(u64, u64)> {
    let config = parse_scene(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let node = py
        .detach(|| inner.create_scene_node(&config))
        .map_err(io_error)?;
    Ok((node.id, node.source_id))
}

#[pyfunction]
fn update_scene_node(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    config: &Bound<'_, PyDict>,
) -> PyResult<(u64, u64)> {
    let config = parse_scene(config)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let node = py
        .detach(|| inner.update_scene_node(&config))
        .map_err(io_error)?;
    Ok((node.id, node.source_id))
}

#[pyfunction]
fn delete_scene_node(py: Python<'_>, session: PyRef<'_, PySession>, node_id: u64) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.delete_scene_node(node_id))
        .map_err(io_error)
}

#[pyfunction]
fn destroy_source(py: Python<'_>, session: PyRef<'_, PySession>, source_id: u64) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.destroy_source(source_id))
        .map_err(io_error)
}

#[pyfunction]
fn wait_until_visible(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source: PyRef<'_, PySource>,
) -> PyResult<()> {
    let mut session_guard = lock(&session.inner, "session")?;
    let session = open_mut(&mut session_guard, "session")?;
    let mut source_guard = lock(&source.inner, "source")?;
    let source = open_mut(&mut source_guard, "source")?;
    py.detach(|| session.wait_until_visible(source))
        .map_err(io_error)
}

#[pyfunction]
fn check_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source: PyRef<'_, PySource>,
) -> PyResult<()> {
    let mut session_guard = lock(&session.inner, "session")?;
    let session = open_mut(&mut session_guard, "session")?;
    let mut source_guard = lock(&source.inner, "source")?;
    let source = open_mut(&mut source_guard, "source")?;
    py.detach(|| session.apply_pending_source_events(source))
        .map_err(io_error)
}

type EventTuple = (String, Option<bool>, Option<u32>, Option<String>);

fn event_tuple(event: SourceEvent) -> EventTuple {
    match event {
        SourceEvent::Visibility(visible) => ("visibility".into(), Some(visible), None, None),
        SourceEvent::NeedKeyframe(epoch) => ("need_keyframe".into(), None, Some(epoch), None),
        SourceEvent::Lost(message) => ("lost".into(), None, None, Some(message)),
    }
}

fn playback_tuple(snapshot: PlaybackSnapshot) -> (u64, i64, u32, u64, u64, u64, u64) {
    (
        snapshot.state,
        snapshot.clock_pts_us,
        snapshot.epoch,
        snapshot.buffered_ahead_us,
        snapshot.underrun_count,
        snapshot.late_drop_count,
        snapshot.eos_state,
    )
}

fn cbor_to_python(py: Python<'_>, value: &vivid_protocol::cbor::Value) -> PyResult<Py<PyAny>> {
    use vivid_protocol::cbor::Value;
    Ok(match value {
        Value::Unsigned(value) => value.into_pyobject(py)?.into_any().unbind(),
        Value::Negative(value) => value.into_pyobject(py)?.into_any().unbind(),
        Value::Bytes(value) => PyBytes::new(py, value).into_any().unbind(),
        Value::Text(value) => value.into_pyobject(py)?.into_any().unbind(),
        Value::Bool(value) => value.into_pyobject(py)?.to_owned().into_any().unbind(),
        Value::Null => py.None(),
        Value::Array(values) => {
            let output = PyList::empty(py);
            for value in values {
                output.append(cbor_to_python(py, value)?)?;
            }
            output.into_any().unbind()
        }
        Value::Map(values) => {
            let output = PyDict::new(py);
            for (key, value) in values {
                output.set_item(key, cbor_to_python(py, value)?)?;
            }
            output.into_any().unbind()
        }
    })
}

fn source_status_dict(py: Python<'_>, status: SourceStatus) -> PyResult<Py<PyDict>> {
    let output = PyDict::new(py);
    output.set_item("source_id", status.source_id)?;
    output.set_item("source_revision", status.source_revision.get())?;
    output.set_item("kind", status.kind)?;
    output.set_item("lifecycle", status.lifecycle)?;
    output.set_item("epoch", status.epoch)?;
    output.set_item("attachment_state", status.attachment_state)?;
    output.set_item("attachment_generation", status.attachment_generation)?;
    output.set_item("last_media_id", status.last_media_id)?;
    output.set_item("last_media_sequence", status.last_media_sequence)?;
    output.set_item("last_decoded_pts_us", status.last_decoded_pts_us)?;
    output.set_item("last_presented_pts_us", status.last_presented_pts_us)?;
    output.set_item("last_presentation_id", status.last_presentation_id)?;
    output.set_item("visible", status.visible)?;
    output.set_item("capture_policy", status.capture_policy)?;
    output.set_item("linked_source_id", status.linked_source_id)?;
    output.set_item("milestones", status.milestones)?;
    output.set_item("outstanding_byte_credit", status.outstanding_byte_credit)?;
    output.set_item(
        "outstanding_packet_credit",
        status.outstanding_packet_credit,
    )?;
    output.set_item("ingress_queue_depth", status.ingress_queue_depth)?;
    output.set_item(
        "descriptor",
        status
            .descriptor
            .as_ref()
            .map(|value| cbor_to_python(py, value))
            .transpose()?,
    )?;
    output.set_item("playback", status.playback.map(playback_tuple))?;
    output.set_item("terminal_loss_code", status.terminal_loss_code)?;
    Ok(output.unbind())
}

type ObservationTuple = (
    String,
    Option<u64>,
    u64,
    u64,
    u64,
    Option<u64>,
    Option<(u64, i64, u32, u64, u64, u64, u64)>,
);

fn observation_tuple(event: ObservationEvent) -> ObservationTuple {
    match event {
        ObservationEvent::Source(event) => (
            "source".into(),
            Some(event.source_id),
            event.source_revision.get(),
            event.changed_fields,
            event.observation_sequence.get(),
            event.first_lost_sequence.map(|sequence| sequence.get()),
            None,
        ),
        ObservationEvent::Scene(event) => (
            "scene".into(),
            None,
            event.scene_revision.get(),
            event.reason_mask,
            event.observation_sequence.get(),
            event.first_lost_sequence.map(|sequence| sequence.get()),
            None,
        ),
        ObservationEvent::Playback(event) => (
            "playback".into(),
            Some(event.source_id),
            event.source_revision.get(),
            event.snapshot.state,
            event.observation_sequence.get(),
            None,
            Some(playback_tuple(event.snapshot)),
        ),
    }
}

#[pyfunction]
fn take_source_event(source: PyRef<'_, PySource>) -> PyResult<Option<EventTuple>> {
    let mut guard = lock(&source.inner, "source")?;
    Ok(open_mut(&mut guard, "source")?
        .take_event()
        .map(event_tuple))
}

#[pyfunction]
fn source_is_visible(source: PyRef<'_, PySource>) -> PyResult<bool> {
    let mut guard = lock(&source.inner, "source")?;
    Ok(open_mut(&mut guard, "source")?.is_visible())
}

#[pyfunction]
fn source_visibility_reasons(source: PyRef<'_, PySource>) -> PyResult<u64> {
    let mut guard = lock(&source.inner, "source")?;
    Ok(open_mut(&mut guard, "source")?.visibility_reasons())
}

#[pyfunction]
fn open_sender(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source: PyRef<'_, PySource>,
) -> PyResult<PyMediaSender> {
    let mut session_guard = lock(&session.inner, "session")?;
    let session = open_mut(&mut session_guard, "session")?;
    let mut source_guard = lock(&source.inner, "source")?;
    let handle = source_guard.take().ok_or_else(|| {
        ClosedHandleError::new_err("source is closed or was already consumed by a sender")
    })?;
    let kind = source.kind;
    let sender = py
        .detach(move || session.open_media_sender(handle, kind.connection()))
        .map_err(io_error)?;
    let id = sender.source_id();
    let cancellation = sender.source().cancellation();
    Ok(PyMediaSender {
        inner: Mutex::new(Some(sender)),
        cancellation,
        id,
        kind,
    })
}

fn ensure_kind(actual: SourceKind, expected: SourceKind) -> PyResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(PyValueError::new_err(format!(
            "{} sender cannot send {} media",
            actual.name(),
            expected.name()
        )))
    }
}

#[pyfunction]
fn send_raster(
    py: Python<'_>,
    sender: PyRef<'_, PyMediaSender>,
    epoch: u32,
    frame_id: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> PyResult<()> {
    ensure_kind(sender.kind, SourceKind::Raster)?;
    let mut guard = lock(&sender.inner, "media sender")?;
    let inner = open_mut(&mut guard, "media sender")?;
    py.detach(|| inner.send_raster(epoch, frame_id, width, height, &rgba))
        .map_err(io_error)
}

#[pyfunction]
fn send_image(py: Python<'_>, sender: PyRef<'_, PyMediaSender>, encoded: Vec<u8>) -> PyResult<()> {
    ensure_kind(sender.kind, SourceKind::Image)?;
    let mut guard = lock(&sender.inner, "media sender")?;
    let inner = open_mut(&mut guard, "media sender")?;
    py.detach(|| inner.send_image(&encoded)).map_err(io_error)
}

#[pyfunction]
fn send_video(
    py: Python<'_>,
    sender: PyRef<'_, PyMediaSender>,
    epoch: u32,
    packet_id: u64,
    pts_us: i64,
    dts_us: i64,
    duration_us: u64,
    key: bool,
    data: Vec<u8>,
) -> PyResult<()> {
    ensure_kind(sender.kind, SourceKind::Video)?;
    let mut guard = lock(&sender.inner, "media sender")?;
    let inner = open_mut(&mut guard, "media sender")?;
    py.detach(|| {
        inner.send_video(VideoPacket {
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
fn send_audio(
    py: Python<'_>,
    sender: PyRef<'_, PyMediaSender>,
    epoch: u32,
    packet_id: u64,
    pts_us: i64,
    dts_us: i64,
    duration_us: u64,
    trim_start_samples: u32,
    trim_end_samples: u32,
    data: Vec<u8>,
) -> PyResult<()> {
    ensure_kind(sender.kind, SourceKind::Audio)?;
    let mut guard = lock(&sender.inner, "media sender")?;
    let inner = open_mut(&mut guard, "media sender")?;
    py.detach(|| {
        inner.send_audio(AudioPacket {
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
fn cancel_sender(sender: PyRef<'_, PyMediaSender>, reason: String) -> PyResult<()> {
    sender.cancellation.cancel(reason);
    lock(&sender.inner, "media sender")?.take();
    Ok(())
}

#[pyfunction]
fn take_sender_event(sender: PyRef<'_, PyMediaSender>) -> PyResult<Option<EventTuple>> {
    let mut guard = lock(&sender.inner, "media sender")?;
    Ok(open_mut(&mut guard, "media sender")?
        .take_event()
        .map(event_tuple))
}

#[pyfunction]
fn sender_is_visible(sender: PyRef<'_, PyMediaSender>) -> PyResult<bool> {
    let mut guard = lock(&sender.inner, "media sender")?;
    Ok(open_mut(&mut guard, "media sender")?.source().is_visible())
}

#[pyfunction]
fn sender_visibility_reasons(sender: PyRef<'_, PyMediaSender>) -> PyResult<u64> {
    let mut guard = lock(&sender.inner, "media sender")?;
    Ok(open_mut(&mut guard, "media sender")?
        .source()
        .visibility_reasons())
}

#[pyfunction]
fn revision_state(session: PyRef<'_, PySession>) -> PyResult<(u64, Vec<(u64, u64)>)> {
    let mut guard = lock(&session.inner, "session")?;
    let state = open_mut(&mut guard, "session")?.revision_state();
    let mut sources = state
        .sources
        .into_iter()
        .map(|(source_id, revision)| (source_id, revision.get()))
        .collect::<Vec<_>>();
    sources.sort_unstable_by_key(|(source_id, _)| *source_id);
    Ok((state.scene.get(), sources))
}

#[pyfunction]
fn set_observation(py: Python<'_>, session: PyRef<'_, PySession>, class_mask: u64) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.set_observation(class_mask))
        .map_err(io_error)
}

#[pyfunction]
fn take_observation(session: PyRef<'_, PySession>) -> PyResult<Option<ObservationTuple>> {
    let mut guard = lock(&session.inner, "session")?;
    Ok(open_mut(&mut guard, "session")?
        .take_observation()
        .map_err(io_error)?
        .map(observation_tuple))
}

#[pyfunction]
fn query_source(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
) -> PyResult<Py<PyDict>> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let status = py
        .detach(|| inner.query_source(source_id))
        .map_err(io_error)?;
    source_status_dict(py, status)
}

#[pyfunction]
#[pyo3(signature = (session, maximum_nodes_per_page=256, maximum_pages=16))]
fn query_scene(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    maximum_nodes_per_page: u64,
    maximum_pages: usize,
) -> PyResult<Py<PyDict>> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let status = py
        .detach(|| inner.query_scene(maximum_nodes_per_page, maximum_pages))
        .map_err(io_error)?;
    let output = PyDict::new(py);
    output.set_item("scene_revision", status.scene_revision.get())?;
    output.set_item("total_nodes", status.total_nodes)?;
    let nodes = PyList::empty(py);
    for parsed in status.nodes {
        let node = PyDict::new(py);
        node.set_item("node_id", parsed.node.node_id)?;
        node.set_item("source_id", parsed.node.source_id)?;
        node.set_item("context_id", parsed.node.context_id)?;
        node.set_item("x", parsed.node.x)?;
        node.set_item("y", parsed.node.y)?;
        node.set_item("width", parsed.node.width)?;
        node.set_item("height", parsed.node.height)?;
        node.set_item("text_layer", parsed.node.text_layer)?;
        node.set_item("z_index", parsed.node.z_index)?;
        node.set_item("visible", parsed.node.visible)?;
        node.set_item("anchor_id", parsed.node.anchor_id)?;
        node.set_item(
            "clip",
            parsed
                .clip
                .map(|clip| (clip.x, clip.y, clip.width, clip.height)),
        )?;
        nodes.append(node)?;
    }
    output.set_item("nodes", nodes)?;
    Ok(output.unbind())
}

#[pyfunction]
fn query_anchor(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    anchor_id: u64,
) -> PyResult<(u64, u64, u64, u64, bool, u64)> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let status = py
        .detach(|| inner.query_anchor(anchor_id))
        .map_err(io_error)?;
    Ok((
        status.anchor_id,
        status.state,
        status.column,
        status.row,
        status.visible,
        status.display_generation,
    ))
}

#[pyfunction]
fn query_limits(py: Python<'_>, session: PyRef<'_, PySession>) -> PyResult<Py<PyDict>> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let status = py.detach(|| inner.query_limits()).map_err(io_error)?;
    let output = PyDict::new(py);
    output.set_item("maximum_sources", status.maximum_sources)?;
    output.set_item("maximum_nodes", status.maximum_nodes)?;
    output.set_item("maximum_transactions", status.maximum_transactions)?;
    output.set_item("maximum_anchors", status.maximum_anchors)?;
    output.set_item("maximum_control_body", status.maximum_control_body)?;
    output.set_item("maximum_media_body", status.maximum_media_body)?;
    output.set_item("maximum_waits", status.maximum_waits)?;
    output.set_item("maximum_pending_requests", status.maximum_pending_requests)?;
    output.set_item("rolling_byte_window", status.rolling_byte_window)?;
    output.set_item("rolling_packet_window", status.rolling_packet_window)?;
    output.set_item("retained_pixel_budget", status.retained_pixel_budget)?;
    output.set_item("current_sources", status.current_sources)?;
    output.set_item("current_nodes", status.current_nodes)?;
    output.set_item("current_retained_pixels", status.current_retained_pixels)?;
    output.set_item("image_cache_budget", status.image_cache_budget)?;
    Ok(output.unbind())
}

fn timeout_duration(seconds: f64) -> PyResult<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(PyValueError::new_err(
            "timeout must be finite and greater than zero",
        ));
    }
    Duration::try_from_secs_f64(seconds)
        .map_err(|error| PyOverflowError::new_err(error.to_string()))
}

#[pyfunction]
#[pyo3(signature = (session, source_id, condition, value, timeout))]
fn begin_wait_source(
    session: PyRef<'_, PySession>,
    source_id: u64,
    condition: u64,
    value: Option<u64>,
    timeout: f64,
) -> PyResult<PyWait> {
    let timeout_us = u64::try_from(timeout_duration(timeout)?.as_micros())
        .map_err(|_| PyOverflowError::new_err("wait timeout is too large"))?;
    let mut guard = lock(&session.inner, "session")?;
    let handle = open_mut(&mut guard, "session")?
        .begin_wait_source(WaitSource {
            source_id,
            condition,
            value,
            timeout_us,
        })
        .map_err(io_error)?;
    let request_id = handle.request_id();
    let cancellation = handle.cancellation();
    Ok(PyWait {
        inner: Mutex::new(Some(handle)),
        cancellation,
        request_id,
        source_id,
    })
}

#[pyfunction]
fn wait_source(py: Python<'_>, wait: PyRef<'_, PyWait>) -> PyResult<(u64, u64, u64, Option<u64>)> {
    let mut handle = lock(&wait.inner, "wait")?
        .take()
        .ok_or_else(|| ClosedHandleError::new_err("wait is closed or consumed"))?;
    let satisfied = py.detach(|| handle.wait()).map_err(io_error)?;
    Ok((
        satisfied.source_id,
        satisfied.source_revision.get(),
        satisfied.condition,
        satisfied.observed_value,
    ))
}

#[pyfunction]
fn cancel_wait(wait: PyRef<'_, PyWait>) -> PyResult<()> {
    wait.cancellation.cancel().map_err(io_error)?;
    lock(&wait.inner, "wait")?.take();
    Ok(())
}

#[pyfunction]
fn wait_until_playing(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    timeout: f64,
) -> PyResult<(u64, u64, u64, Option<u64>)> {
    let timeout = timeout_duration(timeout)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let satisfied = py
        .detach(|| inner.wait_until_playing(source_id, timeout))
        .map_err(io_error)?;
    Ok((
        satisfied.source_id,
        satisfied.source_revision.get(),
        satisfied.condition,
        satisfied.observed_value,
    ))
}

#[pyfunction]
fn play_and_wait_until_playing(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    start_pts_us: i64,
    minimum_buffer_us: u64,
    timeout: f64,
) -> PyResult<(u64, u64, u64, Option<u64>)> {
    let timeout = timeout_duration(timeout)?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    let satisfied = py
        .detach(|| {
            inner.play_and_wait_until_playing(source_id, start_pts_us, minimum_buffer_us, timeout)
        })
        .map_err(io_error)?;
    Ok((
        satisfied.source_id,
        satisfied.source_revision.get(),
        satisfied.condition,
        satisfied.observed_value,
    ))
}

#[pyfunction]
fn play(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    start_pts_us: i64,
    minimum_buffer_us: u64,
) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.play_at(source_id, start_pts_us, minimum_buffer_us))
        .map_err(io_error)
}

#[pyfunction]
fn pause(py: Python<'_>, session: PyRef<'_, PySession>, source_id: u64) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.pause(source_id)).map_err(io_error)
}

#[pyfunction]
fn flush(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    epoch: u32,
) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.flush(source_id, epoch))
        .map_err(io_error)
}

#[pyfunction]
fn eos(py: Python<'_>, session: PyRef<'_, PySession>, source_id: u64, epoch: u32) -> PyResult<()> {
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| inner.eos(source_id, epoch)).map_err(io_error)
}

#[pyfunction]
fn drain(
    py: Python<'_>,
    session: PyRef<'_, PySession>,
    source_id: u64,
    timeout_seconds: Option<f64>,
) -> PyResult<()> {
    let timeout = timeout_seconds
        .map(|seconds| {
            if !seconds.is_finite() || seconds < 0.0 {
                Err(PyValueError::new_err(
                    "timeout_seconds must be finite and non-negative",
                ))
            } else {
                Duration::try_from_secs_f64(seconds)
                    .map_err(|error| PyOverflowError::new_err(error.to_string()))
            }
        })
        .transpose()?;
    let mut guard = lock(&session.inner, "session")?;
    let inner = open_mut(&mut guard, "session")?;
    py.detach(|| match timeout {
        Some(timeout) => inner.drain_with_timeout(source_id, timeout),
        None => inner.drain(source_id),
    })
    .map_err(io_error)
}

#[pymodule]
fn _native(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("VividError", py.get_type::<VividError>())?;
    module.add("ClosedHandleError", py.get_type::<ClosedHandleError>())?;
    module.add_class::<PySession>()?;
    module.add_class::<PySource>()?;
    module.add_class::<PyMediaSender>()?;
    module.add_class::<PyWait>()?;

    module.add_function(wrap_pyfunction!(connect, module)?)?;
    module.add_function(wrap_pyfunction!(close, module)?)?;
    module.add_function(wrap_pyfunction!(allocate_id, module)?)?;
    module.add_function(wrap_pyfunction!(supports, module)?)?;
    module.add_function(wrap_pyfunction!(root_context_id, module)?)?;
    module.add_function(wrap_pyfunction!(display_state, module)?)?;
    module.add_function(wrap_pyfunction!(create_text_anchor, module)?)?;
    module.add_function(wrap_pyfunction!(create_raster_source, module)?)?;
    module.add_function(wrap_pyfunction!(create_image_source, module)?)?;
    module.add_function(wrap_pyfunction!(create_video_source, module)?)?;
    module.add_function(wrap_pyfunction!(create_audio_source, module)?)?;
    module.add_function(wrap_pyfunction!(create_linked_av_sources, module)?)?;
    module.add_function(wrap_pyfunction!(probe_video_config, module)?)?;
    module.add_function(wrap_pyfunction!(probe_audio_config, module)?)?;
    module.add_function(wrap_pyfunction!(place_source, module)?)?;
    module.add_function(wrap_pyfunction!(create_scene_node, module)?)?;
    module.add_function(wrap_pyfunction!(update_scene_node, module)?)?;
    module.add_function(wrap_pyfunction!(delete_scene_node, module)?)?;
    module.add_function(wrap_pyfunction!(destroy_source, module)?)?;
    module.add_function(wrap_pyfunction!(wait_until_visible, module)?)?;
    module.add_function(wrap_pyfunction!(check_source, module)?)?;
    module.add_function(wrap_pyfunction!(take_source_event, module)?)?;
    module.add_function(wrap_pyfunction!(source_is_visible, module)?)?;
    module.add_function(wrap_pyfunction!(source_visibility_reasons, module)?)?;
    module.add_function(wrap_pyfunction!(open_sender, module)?)?;
    module.add_function(wrap_pyfunction!(send_raster, module)?)?;
    module.add_function(wrap_pyfunction!(send_image, module)?)?;
    module.add_function(wrap_pyfunction!(send_video, module)?)?;
    module.add_function(wrap_pyfunction!(send_audio, module)?)?;
    module.add_function(wrap_pyfunction!(cancel_sender, module)?)?;
    module.add_function(wrap_pyfunction!(take_sender_event, module)?)?;
    module.add_function(wrap_pyfunction!(sender_is_visible, module)?)?;
    module.add_function(wrap_pyfunction!(sender_visibility_reasons, module)?)?;
    module.add_function(wrap_pyfunction!(revision_state, module)?)?;
    module.add_function(wrap_pyfunction!(set_observation, module)?)?;
    module.add_function(wrap_pyfunction!(take_observation, module)?)?;
    module.add_function(wrap_pyfunction!(query_source, module)?)?;
    module.add_function(wrap_pyfunction!(query_scene, module)?)?;
    module.add_function(wrap_pyfunction!(query_anchor, module)?)?;
    module.add_function(wrap_pyfunction!(query_limits, module)?)?;
    module.add_function(wrap_pyfunction!(begin_wait_source, module)?)?;
    module.add_function(wrap_pyfunction!(wait_source, module)?)?;
    module.add_function(wrap_pyfunction!(cancel_wait, module)?)?;
    module.add_function(wrap_pyfunction!(wait_until_playing, module)?)?;
    module.add_function(wrap_pyfunction!(play_and_wait_until_playing, module)?)?;
    module.add_function(wrap_pyfunction!(play, module)?)?;
    module.add_function(wrap_pyfunction!(pause, module)?)?;
    module.add_function(wrap_pyfunction!(flush, module)?)?;
    module.add_function(wrap_pyfunction!(eos, module)?)?;
    module.add_function(wrap_pyfunction!(drain, module)?)?;
    Ok(())
}
