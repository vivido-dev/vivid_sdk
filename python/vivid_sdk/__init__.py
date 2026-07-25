"""Function-oriented Python interface to the Vivid 1.1 producer SDK."""

from __future__ import annotations

import hashlib
import math
import os
import struct
import sys
from dataclasses import dataclass
from importlib.metadata import PackageNotFoundError, version
from typing import Any, Dict, Iterable, Literal, Optional, Tuple, Union

from . import _native
from ._native import ClosedHandleError, MediaSender, Session, Source, VividError, Wait

try:
    __version__ = version("vivid-sdk")
except PackageNotFoundError:  # pragma: no cover - source tree without an installed distribution
    __version__ = "0.1.0"

FEATURE_RASTER_RGBA8 = 1
FEATURE_SCENE_TRANSACTIONS = 3
FEATURE_GRID_CELL_NODES = 4
FEATURE_CREDIT_FLOW_CONTROL = 5
FEATURE_ENCODED_IMAGE_V1 = 7
FEATURE_RASTER_ZSTD_V1 = 8
FEATURE_RASTER_PREMULTIPLIED_ALPHA = 9
FEATURE_VISIBILITY_EVENTS_V1 = 10
FEATURE_VIDEO_ACCESS_UNIT_V1 = 11
FEATURE_VIDEO_CONTROL_V1 = 12
FEATURE_TEXT_ANCHORS_V2 = 13
FEATURE_AUDIO_ACCESS_UNIT_V1 = 14
FEATURE_NODE_CLIP_RECT_V1 = 15
FEATURE_DECODER_DESCRIPTION_V1 = 16
FEATURE_OBSERVABILITY_CORE_V1 = 18
FEATURE_ATOMIC_CONTROL_V1 = 19
FEATURE_DELEGATED_CONTEXT_V1 = 21
FEATURE_SOURCE_CAPTURE_POLICY_V1 = 22

CAPTURE_POLICY_DENY_CAPTURE = 1 << 0
CAPTURE_POLICY_DENY_SEMANTIC_EXPORT = 1 << 1
CAPTURE_POLICY_DENY_POSTER_RETENTION = 1 << 2
CAPTURE_POLICY_DENY_CACHE = 1 << 3
CAPTURE_POLICY_REDUCE_DIAGNOSTICS = 1 << 4
CAPTURE_POLICY_MASK = (1 << 5) - 1

AUTHENTICATION_WINDOW_ROOT = 0
AUTHENTICATION_DELEGATED_CONTEXT = 1

CONTEXT_CLASS_OBSERVE = 1 << 0
CONTEXT_CLASS_CREATE_SOURCE = 1 << 1
CONTEXT_CLASS_MUTATE_SCENE = 1 << 2
CONTEXT_CLASS_CREATE_ANCHOR = 1 << 3
CONTEXT_CLASS_DESKTOP_INPUT = 1 << 4
CONTEXT_CLASS_ADMINISTER = 1 << 5

OBSERVE_SOURCE_TRANSITIONS = 1 << 0
OBSERVE_SCENE_CHANGES = 1 << 1
OBSERVE_PLAYBACK_TRANSITIONS = 1 << 2
OBSERVATION_CLASS_MASK = (
    OBSERVE_SOURCE_TRANSITIONS
    | OBSERVE_SCENE_CHANGES
    | OBSERVE_PLAYBACK_TRANSITIONS
)

WAIT_SOURCE_REVISION = 1
WAIT_FIRST_VISIBLE_PRESENTATION = 2
WAIT_RASTER_FRAME = 3
WAIT_VIDEO_PTS = 4
WAIT_PLAYBACK_STARTED = 5
WAIT_PLAYBACK_ENDED = 6
WAIT_MEDIA_ATTACHED = 7
WAIT_MEDIA_CLOSED = 8
WAIT_SOURCE_LOST = 9

IMAGE_PNG = 1
IMAGE_JPEG = 2
TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH = 1

DEFAULT_REQUIRED_FEATURES: Tuple[int, ...] = (
    FEATURE_RASTER_RGBA8,
    FEATURE_SCENE_TRANSACTIONS,
    FEATURE_GRID_CELL_NODES,
    FEATURE_CREDIT_FLOW_CONTROL,
    FEATURE_TEXT_ANCHORS_V2,
)
DEFAULT_OPTIONAL_FEATURES: Tuple[int, ...] = (
    FEATURE_ENCODED_IMAGE_V1,
    FEATURE_RASTER_ZSTD_V1,
    FEATURE_RASTER_PREMULTIPLIED_ALPHA,
    FEATURE_VISIBILITY_EVENTS_V1,
    FEATURE_VIDEO_ACCESS_UNIT_V1,
    FEATURE_VIDEO_CONTROL_V1,
    FEATURE_AUDIO_ACCESS_UNIT_V1,
    FEATURE_NODE_CLIP_RECT_V1,
    FEATURE_DECODER_DESCRIPTION_V1,
    FEATURE_OBSERVABILITY_CORE_V1,
    FEATURE_ATOMIC_CONTROL_V1,
    FEATURE_DELEGATED_CONTEXT_V1,
    FEATURE_SOURCE_CAPTURE_POLICY_V1,
)

BytesLike = Union[bytes, bytearray, memoryview]
SourceLike = Union[int, Source, MediaSender]


class LinkedAudioError(VividError):
    """Audio creation failed after the linked video source became usable."""

    video_source: Source

    def __init__(self, message: str, video_source: Source) -> None:
        super().__init__(message)
        self.video_source = video_source


@dataclass(frozen=True)
class DisplayState:
    display_generation: int
    viewport_width: int
    viewport_height: int
    grid_columns: int
    grid_rows: int
    cell_width: int
    cell_height: int
    settled: bool = True


@dataclass(frozen=True)
class RevisionState:
    scene_revision: int
    source_revisions: Dict[int, int]


@dataclass(frozen=True)
class ContextQuotas:
    maximum_sources: int
    maximum_nodes: int
    maximum_retained_pixels: int
    maximum_media_bytes: int
    maximum_media_connections: int


@dataclass(frozen=True)
class ContextReady:
    context_id: int
    class_mask: int
    expiry_us: int
    quotas: ContextQuotas


@dataclass(frozen=True)
class PlaybackSnapshot:
    state: int
    clock_pts_us: int
    epoch: int
    buffered_ahead_us: int
    underrun_count: int
    late_drop_count: int
    eos_state: int


@dataclass(frozen=True)
class SourceStatus:
    source_id: int
    source_revision: int
    kind: int
    lifecycle: int
    epoch: int
    attachment_state: int
    attachment_generation: int
    last_media_id: int
    last_media_sequence: int
    last_decoded_pts_us: int
    last_presented_pts_us: int
    last_presentation_id: int
    visible: bool
    capture_policy: int
    linked_source_id: int
    milestones: int
    outstanding_byte_credit: int
    outstanding_packet_credit: int
    ingress_queue_depth: int
    descriptor: Optional[object]
    playback: Optional[PlaybackSnapshot]
    terminal_loss_code: Optional[int]


@dataclass(frozen=True)
class SceneNodeStatus:
    node_id: int
    source_id: int
    context_id: int
    x: int
    y: int
    width: int
    height: int
    text_layer: int
    z_index: int
    visible: bool
    anchor_id: Optional[int]
    clip: Optional["ClipRect"]


@dataclass(frozen=True)
class SceneStatus:
    scene_revision: int
    nodes: Tuple[SceneNodeStatus, ...]
    total_nodes: int


@dataclass(frozen=True)
class AnchorStatus:
    anchor_id: int
    state: int
    column: int
    row: int
    visible: bool
    display_generation: int


@dataclass(frozen=True)
class LimitsStatus:
    maximum_sources: int
    maximum_nodes: int
    maximum_transactions: int
    maximum_anchors: int
    maximum_control_body: int
    maximum_media_body: int
    maximum_waits: int
    maximum_pending_requests: int
    rolling_byte_window: int
    rolling_packet_window: int
    retained_pixel_budget: int
    current_sources: int
    current_nodes: int
    current_retained_pixels: int
    image_cache_budget: Optional[int]


@dataclass(frozen=True)
class WaitSatisfied:
    source_id: int
    source_revision: int
    condition: int
    observed_value: Optional[int]


@dataclass(frozen=True)
class SourceChangedEvent:
    source_id: int
    source_revision: int
    changed_fields: int
    observation_sequence: int
    first_lost_sequence: Optional[int]


@dataclass(frozen=True)
class SceneChangedEvent:
    scene_revision: int
    reason_mask: int
    observation_sequence: int
    first_lost_sequence: Optional[int]


@dataclass(frozen=True)
class PlaybackStateEvent:
    source_id: int
    source_revision: int
    observation_sequence: int
    snapshot: PlaybackSnapshot


ObservationEvent = Union[SourceChangedEvent, SceneChangedEvent, PlaybackStateEvent]


@dataclass(frozen=True)
class VideoSourceConfig:
    codec: str
    packetization: str
    width: int
    height: int
    max_access_unit_bytes: int
    extradata: BytesLike = b""
    profile: int = 0
    level: int = 0
    bitrate: int = 0
    color_primaries: int = 2
    transfer: int = 2
    matrix: int = 2
    range: int = 0
    sar_num: int = 1
    sar_den: int = 1
    codec_string: Optional[str] = None
    decoder_config: Optional[BytesLike] = None


@dataclass(frozen=True)
class AudioSourceConfig:
    codec: str
    packetization: str
    sample_rate: int
    channels: int
    max_access_unit_bytes: int
    extradata: BytesLike = b""
    channel_mask: int = 0
    bitrate: int = 0
    codec_string: Optional[str] = None


ImageEncoding = Union[Literal["png", "jpeg"], int]


@dataclass(frozen=True)
class ImageSourceConfig:
    encoding: ImageEncoding
    width: int
    height: int
    encoded_length: int
    sha256: Optional[BytesLike] = None


@dataclass(frozen=True)
class ClipRect:
    """A clip rectangle in signed 32.32 cell coordinates."""

    x: int
    y: int
    width: int
    height: int


@dataclass(frozen=True)
class SceneNodeConfig:
    """Complete scene-node state; geometry uses signed 32.32 cell coordinates."""

    source_id: int
    width: int
    height: int
    node_id: Optional[int] = None
    context_id: Optional[int] = None
    x: int = 0
    y: int = 0
    text_layer: int = TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH
    z_index: int = 0
    visible: bool = True
    anchor_id: Optional[int] = None
    clip: Optional[ClipRect] = None


@dataclass(frozen=True)
class SceneNode:
    id: int
    source_id: int


@dataclass(frozen=True)
class VisibilityEvent:
    visible: bool


@dataclass(frozen=True)
class NeedKeyframeEvent:
    epoch: int


@dataclass(frozen=True)
class SourceLostEvent:
    message: str


SourceEvent = Union[VisibilityEvent, NeedKeyframeEvent, SourceLostEvent]


@dataclass(frozen=True)
class _EncodedImage:
    encoding: int
    width: int
    height: int
    data: bytes


def _jpeg_dimensions(data: bytes) -> Tuple[int, int]:
    offset = 2
    start_of_frame = {
        0xC0,
        0xC1,
        0xC2,
        0xC3,
        0xC5,
        0xC6,
        0xC7,
        0xC9,
        0xCA,
        0xCB,
        0xCD,
        0xCE,
        0xCF,
    }
    while offset < len(data):
        while offset < len(data) and data[offset] == 0xFF:
            offset += 1
        if offset >= len(data):
            break
        marker = data[offset]
        offset += 1
        if marker == 0x00 or marker == 0xD8 or 0xD0 <= marker <= 0xD7:
            continue
        if marker in (0xD9, 0xDA) or offset + 2 > len(data):
            break
        segment_length = struct.unpack(">H", data[offset : offset + 2])[0]
        if segment_length < 2 or offset + segment_length > len(data):
            raise ValueError("JPEG contains a truncated segment")
        if marker in start_of_frame:
            if segment_length < 7:
                raise ValueError("JPEG frame header is too short")
            height, width = struct.unpack(">HH", data[offset + 3 : offset + 7])
            return width, height
        offset += segment_length
    raise ValueError("JPEG dimensions were not found")


def _inspect_encoded_image(path: Union[str, os.PathLike[str]]) -> _EncodedImage:
    image_path = os.fspath(path)
    maximum_bytes = 64 * 1024 * 1024
    size = os.stat(image_path).st_size
    if size <= 0 or size > maximum_bytes:
        raise ValueError("encoded image exceeds the Vivid media-record limit")
    with open(image_path, "rb") as stream:
        data = stream.read(maximum_bytes + 1)
    if len(data) > maximum_bytes:
        raise ValueError("encoded image exceeds the Vivid media-record limit")
    if len(data) != size:
        raise OSError("image changed while it was being read")
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        if len(data) < 24 or data[12:16] != b"IHDR":
            raise ValueError("PNG is missing its IHDR header")
        width, height = struct.unpack(">II", data[16:24])
        encoding = IMAGE_PNG
    elif data.startswith(b"\xff\xd8"):
        width, height = _jpeg_dimensions(data)
        encoding = IMAGE_JPEG
    else:
        raise ValueError("only PNG and JPEG encoded images are supported")
    if width == 0 or height == 0 or width > 8192 or height > 8192:
        raise ValueError(f"unsupported image dimensions: {width}x{height}")
    return _EncodedImage(encoding, width, height, data)


def _fitted_image_cells(
    image: _EncodedImage, display: DisplayState, scale: float
) -> Tuple[int, int]:
    if not math.isfinite(scale) or scale <= 0:
        raise ValueError("scale must be a positive finite number")
    desired_width = image.width * scale
    desired_height = image.height * scale
    if not math.isfinite(desired_width) or not math.isfinite(desired_height):
        raise ValueError("scaled image dimensions are too large")
    maximum_width = max(1, display.grid_columns - 4) * display.cell_width
    maximum_height = max(1, display.grid_rows - 2) * display.cell_height
    fit = min(maximum_width / desired_width, maximum_height / desired_height, 1.0)
    target_width = max(1, round(desired_width * fit))
    target_height = max(1, round(desired_height * fit))
    return (
        math.ceil(target_width / display.cell_width),
        math.ceil(target_height / display.cell_height),
    )


def _reserve_terminal_rows(rows: int) -> None:
    raw = b"\r\n" * rows
    stream = getattr(sys.stdout, "buffer", None)
    if stream is not None:
        stream.write(raw)
        stream.flush()
    else:  # pragma: no cover - text-only stdout implementations are uncommon
        sys.stdout.write(raw.decode("ascii"))
        sys.stdout.flush()


def _owned_bytes(value: BytesLike) -> bytes:
    return bytes(value)


def _video_dict(config: VideoSourceConfig) -> dict[str, object]:
    return {
        "codec": config.codec,
        "packetization": config.packetization,
        "extradata": _owned_bytes(config.extradata),
        "width": config.width,
        "height": config.height,
        "profile": config.profile,
        "level": config.level,
        "bitrate": config.bitrate,
        "color_primaries": config.color_primaries,
        "transfer": config.transfer,
        "matrix": config.matrix,
        "range": config.range,
        "sar_num": config.sar_num,
        "sar_den": config.sar_den,
        "max_access_unit_bytes": config.max_access_unit_bytes,
        "codec_string": config.codec_string,
        "decoder_config": (
            None
            if config.decoder_config is None
            else _owned_bytes(config.decoder_config)
        ),
    }


def _audio_dict(config: AudioSourceConfig) -> dict[str, object]:
    return {
        "codec": config.codec,
        "packetization": config.packetization,
        "extradata": _owned_bytes(config.extradata),
        "sample_rate": config.sample_rate,
        "channels": config.channels,
        "channel_mask": config.channel_mask,
        "bitrate": config.bitrate,
        "max_access_unit_bytes": config.max_access_unit_bytes,
        "codec_string": config.codec_string,
    }


def _scene_dict(session: Session, config: SceneNodeConfig, *, updating: bool) -> dict[str, object]:
    if updating and config.node_id is None:
        raise ValueError("an update requires an explicit node_id")
    node_id = allocate_id(session) if config.node_id is None else config.node_id
    context_id = root_context_id(session) if config.context_id is None else config.context_id
    clip: Optional[dict[str, int]]
    if config.clip is None:
        clip = None
    else:
        clip = {
            "x": config.clip.x,
            "y": config.clip.y,
            "width": config.clip.width,
            "height": config.clip.height,
        }
    return {
        "node_id": node_id,
        "source_id": config.source_id,
        "context_id": context_id,
        "x": config.x,
        "y": config.y,
        "width": config.width,
        "height": config.height,
        "text_layer": config.text_layer,
        "z_index": config.z_index,
        "visible": config.visible,
        "anchor_id": config.anchor_id,
        "clip": clip,
    }


def _allocated_id(session: Session, value: Optional[int]) -> int:
    return allocate_id(session) if value is None else value


def _source_id(source: SourceLike) -> int:
    if isinstance(source, int):
        return source
    if isinstance(source, (Source, MediaSender)):
        return source.id
    raise TypeError("expected a source ID, Source, or MediaSender")


def source_id(source: SourceLike) -> int:
    """Return the numeric protocol ID for a source, sender, or integer ID."""

    return _source_id(source)


def connect(
    *,
    endpoint: Optional[str] = None,
    bulk_endpoint: Optional[str] = None,
    token: Optional[str] = None,
    dry_run: bool = False,
    trace_dir: Optional[os.PathLike[str]] = None,
    verbose: bool = False,
    producer: str = "vivid-sdk-python",
    producer_version: str = __version__,
    required_features: Iterable[int] = DEFAULT_REQUIRED_FEATURES,
    optional_features: Iterable[int] = DEFAULT_OPTIONAL_FEATURES,
    authentication_kind: int = AUTHENTICATION_WINDOW_ROOT,
) -> Session:
    """Connect to a Vivid presenter or create a dry-run/trace session."""

    endpoint = os.environ.get("VIVID_ENDPOINT") if endpoint is None else endpoint
    bulk_endpoint = (
        os.environ.get("VIVID_ENDPOINT_BULK") if bulk_endpoint is None else bulk_endpoint
    )
    token = os.environ.get("VIVID_TOKEN") if token is None else token
    return _native.connect(
        endpoint,
        bulk_endpoint,
        token,
        dry_run,
        None if trace_dir is None else os.fspath(trace_dir),
        verbose,
        producer,
        producer_version,
        list(required_features),
        list(optional_features),
        authentication_kind,
    )


def close(session: Session) -> None:
    """Send GOODBYE and close a session. Repeated calls are harmless."""

    _native.close(session)


def allocate_id(session: Session) -> int:
    return _native.allocate_id(session)


def supports(session: Session, feature: int) -> bool:
    return _native.supports(session, feature)


def root_context_id(session: Session) -> int:
    return _native.root_context_id(session)


def display_state(session: Session) -> DisplayState:
    return DisplayState(*_native.display_state(session))


def revision_state(session: Session) -> RevisionState:
    scene_revision, source_revisions = _native.revision_state(session)
    return RevisionState(scene_revision, dict(source_revisions))


def set_observation(session: Session, class_mask: int) -> None:
    _native.set_observation(session, class_mask)


def create_context(
    session: Session,
    *,
    context_id: int,
    parent_context_id: int,
    class_mask: int,
    label: str,
    expiry_us: int,
    quotas: ContextQuotas,
) -> ContextReady:
    values = _native.create_context(
        session,
        context_id,
        parent_context_id,
        class_mask,
        label,
        expiry_us,
        quotas.maximum_sources,
        quotas.maximum_nodes,
        quotas.maximum_retained_pixels,
        quotas.maximum_media_bytes,
        quotas.maximum_media_connections,
    )
    return ContextReady(values[0], values[1], values[2], ContextQuotas(*values[3:]))


def delegate_context(session: Session, context_id: int) -> bytes:
    """Mint an opaque capability; keep the returned bytes out of logs and traces."""

    return bytes(_native.delegate_context(session, context_id))


def revoke_context(session: Session, context_id: int) -> None:
    _native.revoke_context(session, context_id)


def _playback(value: Tuple[int, int, int, int, int, int, int]) -> PlaybackSnapshot:
    return PlaybackSnapshot(*value)


def take_observation(session: Session) -> Optional[ObservationEvent]:
    value = _native.take_observation(session)
    if value is None:
        return None
    kind, source, revision, detail, sequence, first_lost, playback = value
    if kind == "source" and source is not None:
        return SourceChangedEvent(source, revision, detail, sequence, first_lost)
    if kind == "scene":
        return SceneChangedEvent(revision, detail, sequence, first_lost)
    if kind == "playback" and source is not None and playback is not None:
        return PlaybackStateEvent(source, revision, sequence, _playback(playback))
    raise VividError("native observation event is malformed")


def query_source(session: Session, source: SourceLike) -> SourceStatus:
    values: Dict[str, Any] = _native.query_source(session, _source_id(source))
    playback = values.get("playback")
    values["playback"] = None if playback is None else _playback(playback)
    return SourceStatus(**values)


def query_scene(
    session: Session,
    *,
    maximum_nodes_per_page: int = 256,
    maximum_pages: int = 16,
) -> SceneStatus:
    values: Dict[str, Any] = _native.query_scene(
        session, maximum_nodes_per_page, maximum_pages
    )
    nodes = []
    for raw in values["nodes"]:
        clip = raw.get("clip")
        raw["clip"] = None if clip is None else ClipRect(*clip)
        nodes.append(SceneNodeStatus(**raw))
    return SceneStatus(
        scene_revision=values["scene_revision"],
        nodes=tuple(nodes),
        total_nodes=values["total_nodes"],
    )


def query_anchor(session: Session, anchor_id: int) -> AnchorStatus:
    return AnchorStatus(*_native.query_anchor(session, anchor_id))


def query_limits(session: Session) -> LimitsStatus:
    values: Dict[str, Any] = _native.query_limits(session)
    return LimitsStatus(**values)


def begin_wait_source(
    session: Session,
    source: SourceLike,
    condition: int,
    *,
    value: Optional[int] = None,
    timeout: float = 30.0,
) -> Wait:
    return _native.begin_wait_source(
        session, _source_id(source), condition, value, timeout
    )


def wait(wait_handle: Wait) -> WaitSatisfied:
    return WaitSatisfied(*_native.wait_source(wait_handle))


def cancel_wait(wait_handle: Wait) -> None:
    _native.cancel_wait(wait_handle)


def wait_source(
    session: Session,
    source: SourceLike,
    condition: int,
    *,
    value: Optional[int] = None,
    timeout: float = 30.0,
) -> WaitSatisfied:
    return wait(
        begin_wait_source(
            session, source, condition, value=value, timeout=timeout
        )
    )


def create_text_anchor(session: Session) -> Optional[int]:
    return _native.create_text_anchor(session)


def create_raster_source(
    session: Session,
    width: int,
    height: int,
    *,
    source_id: Optional[int] = None,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
    capture_policy: int = 0,
) -> Source:
    return _native.create_raster_source(
        session,
        _allocated_id(session, source_id),
        width,
        height,
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
        capture_policy,
    )


def _image_encoding(encoding: ImageEncoding) -> int:
    if encoding == "png":
        return IMAGE_PNG
    if encoding == "jpeg":
        return IMAGE_JPEG
    if isinstance(encoding, int):
        return encoding
    raise ValueError("image encoding must be 'png', 'jpeg', or a numeric registry value")


def create_image_source(
    session: Session,
    config: ImageSourceConfig,
    *,
    source_id: Optional[int] = None,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
    capture_policy: int = 0,
) -> Source:
    digest = None if config.sha256 is None else _owned_bytes(config.sha256)
    return _native.create_image_source(
        session,
        _allocated_id(session, source_id),
        _image_encoding(config.encoding),
        config.width,
        config.height,
        config.encoded_length,
        digest,
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
        capture_policy,
    )


def create_video_source(
    session: Session,
    config: VideoSourceConfig,
    *,
    source_id: Optional[int] = None,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
    capture_policy: int = 0,
) -> Source:
    return _native.create_video_source(
        session,
        _allocated_id(session, source_id),
        _video_dict(config),
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
        capture_policy,
    )


def create_audio_source(
    session: Session,
    config: AudioSourceConfig,
    *,
    source_id: Optional[int] = None,
    linked_video: Optional[SourceLike] = None,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
    capture_policy: int = 0,
) -> Source:
    linked_id = None if linked_video is None else _source_id(linked_video)
    return _native.create_audio_source(
        session,
        _allocated_id(session, source_id),
        linked_id,
        _audio_dict(config),
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
        capture_policy,
    )


def create_linked_av_sources(
    session: Session,
    video: VideoSourceConfig,
    audio: AudioSourceConfig,
    *,
    video_source_id: Optional[int] = None,
    audio_source_id: Optional[int] = None,
    video_capture_policy: int = 0,
    audio_capture_policy: int = 0,
) -> Tuple[Source, Source]:
    video_handle, audio_handle, audio_error = _native.create_linked_av_sources(
        session,
        _allocated_id(session, video_source_id),
        _video_dict(video),
        _allocated_id(session, audio_source_id),
        _audio_dict(audio),
        video_capture_policy,
        audio_capture_policy,
    )
    if audio_error is not None or audio_handle is None:
        raise LinkedAudioError(audio_error or "linked audio source was rejected", video_handle)
    return video_handle, audio_handle


def set_source_policy(
    session: Session, source: SourceLike, capture_policy: int
) -> None:
    _native.set_source_policy(session, _source_id(source), capture_policy)


def probe_video_config(session: Session, config: VideoSourceConfig) -> bool:
    return _native.probe_video_config(session, _video_dict(config))


def probe_audio_config(session: Session, config: AudioSourceConfig) -> bool:
    return _native.probe_audio_config(session, _audio_dict(config))


def place_source(
    session: Session,
    source: SourceLike,
    columns: int,
    rows: int,
    *,
    node_id: Optional[int] = None,
    anchor: bool = True,
    anchor_id: Optional[int] = None,
) -> SceneNode:
    if not anchor and anchor_id is not None:
        raise ValueError("anchor_id cannot be supplied when anchor=False")
    actual_node_id = _allocated_id(session, node_id)
    actual_anchor_id = (
        create_text_anchor(session) if anchor and anchor_id is None else anchor_id
    )
    actual_source_id = _source_id(source)
    _native.place_source(
        session, actual_source_id, actual_node_id, actual_anchor_id, columns, rows
    )
    return SceneNode(actual_node_id, actual_source_id)


def create_scene_node(session: Session, config: SceneNodeConfig) -> SceneNode:
    return SceneNode(*_native.create_scene_node(session, _scene_dict(session, config, updating=False)))


def update_scene_node(session: Session, config: SceneNodeConfig) -> SceneNode:
    return SceneNode(*_native.update_scene_node(session, _scene_dict(session, config, updating=True)))


def delete_scene_node(session: Session, node_id: int) -> None:
    _native.delete_scene_node(session, node_id)


def destroy_source(
    session: Session,
    source: SourceLike,
    *,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
) -> None:
    _native.destroy_source(
        session,
        _source_id(source),
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
    )


def wait_until_visible(session: Session, source: Source) -> None:
    _native.wait_until_visible(session, source)


def check_source(session: Session, source: Source) -> None:
    _native.check_source(session, source)


def _event(value: Optional[Tuple[str, Optional[bool], Optional[int], Optional[str]]]) -> Optional[SourceEvent]:
    if value is None:
        return None
    kind, visible, epoch, message = value
    if kind == "visibility" and visible is not None:
        return VisibilityEvent(visible)
    if kind == "need_keyframe" and epoch is not None:
        return NeedKeyframeEvent(epoch)
    if kind == "lost" and message is not None:
        return SourceLostEvent(message)
    raise VividError("native source event is malformed")


def take_event(handle: Union[Source, MediaSender]) -> Optional[SourceEvent]:
    if isinstance(handle, Source):
        return _event(_native.take_source_event(handle))
    if isinstance(handle, MediaSender):
        return _event(_native.take_sender_event(handle))
    raise TypeError("expected a Source or MediaSender")


def is_visible(handle: Union[Source, MediaSender]) -> bool:
    if isinstance(handle, Source):
        return _native.source_is_visible(handle)
    if isinstance(handle, MediaSender):
        return _native.sender_is_visible(handle)
    raise TypeError("expected a Source or MediaSender")


def visibility_reasons(handle: Union[Source, MediaSender]) -> int:
    if isinstance(handle, Source):
        return _native.source_visibility_reasons(handle)
    if isinstance(handle, MediaSender):
        return _native.sender_visibility_reasons(handle)
    raise TypeError("expected a Source or MediaSender")


def open_sender(session: Session, source: Source) -> MediaSender:
    """Consume a source handle and open its independently synchronized media sender."""

    return _native.open_sender(session, source)


def send_raster(
    sender: MediaSender,
    rgba: BytesLike,
    *,
    width: int,
    height: int,
    epoch: int = 1,
    frame_id: int = 1,
) -> None:
    _native.send_raster(
        sender, epoch, frame_id, width, height, _owned_bytes(rgba)
    )


def send_image(sender: MediaSender, encoded: BytesLike) -> None:
    _native.send_image(sender, _owned_bytes(encoded))


def send_video(
    sender: MediaSender,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    key: bool,
    epoch: int = 1,
) -> None:
    _native.send_video(
        sender,
        epoch,
        packet_id,
        pts_us,
        dts_us,
        duration_us,
        key,
        _owned_bytes(data),
    )


def send_audio(
    sender: MediaSender,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    trim_start_samples: int = 0,
    trim_end_samples: int = 0,
    epoch: int = 1,
) -> None:
    _native.send_audio(
        sender,
        epoch,
        packet_id,
        pts_us,
        dts_us,
        duration_us,
        trim_start_samples,
        trim_end_samples,
        _owned_bytes(data),
    )


def cancel_sender(sender: MediaSender, reason: str = "Python media sender cancelled") -> None:
    _native.cancel_sender(sender, reason)


def play(
    session: Session,
    source: SourceLike,
    *,
    start_pts_us: int = 0,
    minimum_buffer_us: int = 0,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
) -> None:
    _native.play(
        session,
        _source_id(source),
        start_pts_us,
        minimum_buffer_us,
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
    )


def wait_until_playing(
    session: Session, source: SourceLike, *, timeout: float = 30.0
) -> WaitSatisfied:
    return WaitSatisfied(
        *_native.wait_until_playing(session, _source_id(source), timeout)
    )


def play_and_wait_until_playing(
    session: Session,
    source: SourceLike,
    *,
    start_pts_us: int = 0,
    minimum_buffer_us: int = 0,
    timeout: float = 30.0,
) -> WaitSatisfied:
    return WaitSatisfied(
        *_native.play_and_wait_until_playing(
            session,
            _source_id(source),
            start_pts_us,
            minimum_buffer_us,
            timeout,
        )
    )


def pause(
    session: Session,
    source: SourceLike,
    *,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
) -> None:
    _native.pause(
        session,
        _source_id(source),
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
    )


def flush(
    session: Session,
    source: SourceLike,
    *,
    epoch: int,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
) -> None:
    _native.flush(
        session,
        _source_id(source),
        epoch,
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
    )


def eos(
    session: Session,
    source: SourceLike,
    *,
    epoch: int,
    preconditions: Optional[Dict[int, int]] = None,
    idempotency_key: Optional[BytesLike] = None,
    causation_id: Optional[BytesLike] = None,
) -> None:
    _native.eos(
        session,
        _source_id(source),
        epoch,
        preconditions,
        None if idempotency_key is None else _owned_bytes(idempotency_key),
        None if causation_id is None else _owned_bytes(causation_id),
    )


def drain(
    session: Session,
    source: SourceLike,
    *,
    timeout: Optional[float] = None,
) -> None:
    _native.drain(session, _source_id(source), timeout)


def display_image(
    path: Union[str, os.PathLike[str]],
    scale: float = 1.0,
    *,
    endpoint: Optional[str] = None,
    bulk_endpoint: Optional[str] = None,
    token: Optional[str] = None,
    dry_run: bool = False,
    trace_dir: Optional[os.PathLike[str]] = None,
    verbose: bool = False,
) -> None:
    """Display one retained PNG or JPEG, fitted to the current terminal viewport."""

    if not math.isfinite(scale) or scale <= 0:
        raise ValueError("scale must be a positive finite number")
    image = _inspect_encoded_image(path)
    offline = dry_run or trace_dir is not None
    if not offline and not sys.stdout.isatty():
        raise VividError("stdout must be attached to the Vivido terminal")
    required_features = tuple(
        sorted(DEFAULT_REQUIRED_FEATURES + (FEATURE_ENCODED_IMAGE_V1,))
    )
    optional_features = tuple(
        feature
        for feature in DEFAULT_OPTIONAL_FEATURES
        if feature != FEATURE_ENCODED_IMAGE_V1
    )
    session = connect(
        endpoint=endpoint,
        bulk_endpoint=bulk_endpoint,
        token=token,
        dry_run=dry_run,
        trace_dir=trace_dir,
        verbose=verbose,
        producer="vivid-python-image",
        required_features=required_features,
        optional_features=optional_features,
    )
    try:
        columns, rows = _fitted_image_cells(image, display_state(session), scale)
        source = create_image_source(
            session,
            ImageSourceConfig(
                encoding=image.encoding,
                width=image.width,
                height=image.height,
                encoded_length=len(image.data),
                sha256=hashlib.sha256(image.data).digest(),
            ),
        )
        place_source(session, source, columns, rows)
        if not offline:
            _reserve_terminal_rows(rows)
        sender = open_sender(session, source)
        send_image(sender, image.data)
        if not offline and supports(session, FEATURE_OBSERVABILITY_CORE_V1):
            wait_source(
                session,
                sender,
                WAIT_FIRST_VISIBLE_PRESENTATION,
                timeout=10.0,
            )
    finally:
        close(session)


__all__ = [
    "AudioSourceConfig",
    "BytesLike",
    "ClipRect",
    "ClosedHandleError",
    "DEFAULT_OPTIONAL_FEATURES",
    "DEFAULT_REQUIRED_FEATURES",
    "DisplayState",
    "RevisionState",
    "PlaybackSnapshot",
    "SourceStatus",
    "SceneNodeStatus",
    "SceneStatus",
    "AnchorStatus",
    "LimitsStatus",
    "Wait",
    "WaitSatisfied",
    "ObservationEvent",
    "SourceChangedEvent",
    "SceneChangedEvent",
    "PlaybackStateEvent",
    "CAPTURE_POLICY_DENY_CAPTURE",
    "CAPTURE_POLICY_DENY_SEMANTIC_EXPORT",
    "CAPTURE_POLICY_DENY_POSTER_RETENTION",
    "CAPTURE_POLICY_DENY_CACHE",
    "CAPTURE_POLICY_REDUCE_DIAGNOSTICS",
    "CAPTURE_POLICY_MASK",
    "FEATURE_AUDIO_ACCESS_UNIT_V1",
    "FEATURE_CREDIT_FLOW_CONTROL",
    "FEATURE_DECODER_DESCRIPTION_V1",
    "FEATURE_ENCODED_IMAGE_V1",
    "FEATURE_GRID_CELL_NODES",
    "FEATURE_NODE_CLIP_RECT_V1",
    "FEATURE_OBSERVABILITY_CORE_V1",
    "FEATURE_RASTER_PREMULTIPLIED_ALPHA",
    "FEATURE_RASTER_RGBA8",
    "FEATURE_RASTER_ZSTD_V1",
    "FEATURE_SCENE_TRANSACTIONS",
    "FEATURE_TEXT_ANCHORS_V2",
    "FEATURE_SOURCE_CAPTURE_POLICY_V1",
    "FEATURE_VIDEO_ACCESS_UNIT_V1",
    "FEATURE_VIDEO_CONTROL_V1",
    "FEATURE_VISIBILITY_EVENTS_V1",
    "OBSERVE_SOURCE_TRANSITIONS",
    "OBSERVE_SCENE_CHANGES",
    "OBSERVE_PLAYBACK_TRANSITIONS",
    "OBSERVATION_CLASS_MASK",
    "WAIT_SOURCE_REVISION",
    "WAIT_FIRST_VISIBLE_PRESENTATION",
    "WAIT_RASTER_FRAME",
    "WAIT_VIDEO_PTS",
    "WAIT_PLAYBACK_STARTED",
    "WAIT_PLAYBACK_ENDED",
    "WAIT_MEDIA_ATTACHED",
    "WAIT_MEDIA_CLOSED",
    "WAIT_SOURCE_LOST",
    "IMAGE_JPEG",
    "IMAGE_PNG",
    "ImageSourceConfig",
    "LinkedAudioError",
    "MediaSender",
    "NeedKeyframeEvent",
    "SceneNode",
    "SceneNodeConfig",
    "Session",
    "Source",
    "SourceEvent",
    "SourceLike",
    "SourceLostEvent",
    "TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH",
    "VideoSourceConfig",
    "VisibilityEvent",
    "VividError",
    "__version__",
    "allocate_id",
    "begin_wait_source",
    "cancel_wait",
    "cancel_sender",
    "check_source",
    "close",
    "connect",
    "create_audio_source",
    "create_image_source",
    "create_linked_av_sources",
    "create_raster_source",
    "create_scene_node",
    "create_text_anchor",
    "create_video_source",
    "delete_scene_node",
    "destroy_source",
    "display_state",
    "display_image",
    "drain",
    "eos",
    "flush",
    "is_visible",
    "open_sender",
    "pause",
    "place_source",
    "play",
    "play_and_wait_until_playing",
    "probe_audio_config",
    "probe_video_config",
    "root_context_id",
    "revision_state",
    "send_audio",
    "send_image",
    "send_raster",
    "send_video",
    "source_id",
    "supports",
    "set_observation",
    "set_source_policy",
    "query_source",
    "query_scene",
    "query_anchor",
    "query_limits",
    "take_observation",
    "take_event",
    "update_scene_node",
    "visibility_reasons",
    "wait_until_visible",
    "wait",
    "wait_source",
    "wait_until_playing",
]
