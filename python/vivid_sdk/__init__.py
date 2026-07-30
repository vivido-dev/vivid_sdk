"""Typed Python producer SDK for Vivid Protocol 1.5.

The API intentionally uses the 1.5 object model: stable surfaces own immutable
tracks, and each track is fed through an authenticated channel generation.
"""

from __future__ import annotations

import hashlib
import struct
from dataclasses import dataclass
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
from typing import Any, Dict, Optional, Sequence, Tuple, Union

from . import _native
from ._native import (
    ClosedHandleError,
    Session,
    Surface,
    Track,
    TrackChannel,
    VividError,
)

try:
    __version__ = version("vivid-sdk")
except PackageNotFoundError:  # pragma: no cover
    __version__ = "1.5.0"

# Negotiated profiles.
PROFILE_CORE = "vivid-core-control-v1"
PROFILE_TERMINAL_SURFACE = "terminal-surface-v1"
PROFILE_DESKTOP_SURFACE = "desktop-surface-v1"
PROFILE_CANVAS_SURFACE = "canvas-surface-v1"
PROFILE_LIVE_MEDIA = "live-media-v1"
PROFILE_TIMED_MEDIA = "timed-media-v1"
PROFILE_DESKTOP_INPUT = "desktop-input-v1"
PROFILE_OBSERVABILITY = "observability-v1"

# Surface semantic profiles and coordinate models.
SURFACE_GENERIC = "generic-content-v1"
SURFACE_TERMINAL = "terminal-content-v1"
SURFACE_DESKTOP = "desktop-content-v1"
SURFACE_CANVAS = "canvas-content-v1"
COORDINATE_DESKTOP_LOGICAL_PIXELS = 1
COORDINATE_NORMALIZED = 2
COORDINATE_CANVAS_LOGICAL_UNITS = 3
COORDINATE_TERMINAL_CONTENT_CELLS = 4

# Descriptor roles.
ROLE_UNSPECIFIED = 0
ROLE_DOCUMENT = 1
ROLE_DESKTOP = 2
ROLE_TIMED_MEDIA = 3
ROLE_FIGURE = 4
ROLE_TERMINAL = 5
ROLE_CANVAS = 6

# Capture/export policies.
POLICY_DENY_CAPTURE = 1 << 0
POLICY_DENY_DESCRIPTOR_EXPORT = 1 << 1
POLICY_DENY_POSTER_RETENTION = 1 << 2
POLICY_DENY_IMAGE_CACHE = 1 << 3
POLICY_REDUCED_DIAGNOSTICS = 1 << 4

# Track modes, lanes, and slots.
TRACK_MODE_LIVE = 1
TRACK_MODE_TIMED = 2
LANE_REALTIME = 2
LANE_BULK = 3
SLOT_PRIMARY_VIDEO = 1
SLOT_AUDIO = 2
SLOT_RASTER = 3
SLOT_POSTER = 4

IMAGE_PNG = 1
IMAGE_JPEG = 2
MILESTONE_OUTPUT_READY = 1 << 4
WAIT_REVISION_GREATER = 1
WAIT_MILESTONE_SET = 2
WAIT_RASTER_FRAME_PRESENTED = 3
WAIT_VIDEO_PTS_PRESENTED = 4
WAIT_PLAYBACK_STARTED = 5
WAIT_PLAYBACK_ENDED = 6
WAIT_CHANNEL_ACCEPTED = 7
WAIT_CHANNEL_CLOSED = 8
WAIT_TRACK_LOST = 9

BytesLike = Union[bytes, bytearray, memoryview]


@dataclass(frozen=True)
class SessionInfo:
    session_id: int
    session_tag: bytes
    root_context_id: int
    target_generation: int
    target_profile: str
    accepted_profiles: Tuple[str, ...]
    session_revision: int
    scene_revision: int
    establishment_state: int
    resume_generation: int


@dataclass(frozen=True)
class WaitSatisfied:
    context_id: int
    surface_id: int
    track_id: int
    revision: int
    channel_generation: int
    condition: int
    observed_value: Optional[int]


@dataclass(frozen=True)
class SurfaceConfig:
    logical_width: int
    logical_height: int
    semantic_profile: str = SURFACE_GENERIC
    coordinate_model: int = COORDINATE_DESKTOP_LOGICAL_PIXELS
    role: int = ROLE_UNSPECIFIED
    title: str = ""
    semantic_content_revision: int = 0
    semantic_availability: int = 0
    locator_hint: str = ""
    policy: int = 0
    scale_numerator: int = 1
    scale_denominator: int = 1
    rotation: int = 0
    context_id: Optional[int] = None
    surface_id: Optional[int] = None

    def native(self, session: Session) -> Dict[str, object]:
        return {
            "context_id": (
                self.context_id
                if self.context_id is not None
                else session_info(session).root_context_id
            ),
            "surface_id": (
                self.surface_id
                if self.surface_id is not None
                else allocate_id(session)
            ),
            "semantic_profile": self.semantic_profile,
            "coordinate_model": self.coordinate_model,
            "logical_width": self.logical_width,
            "logical_height": self.logical_height,
            "scale_numerator": self.scale_numerator,
            "scale_denominator": self.scale_denominator,
            "rotation": self.rotation,
            "role": self.role,
            "title": self.title,
            "semantic_content_revision": self.semantic_content_revision,
            "semantic_availability": self.semantic_availability,
            "locator_hint": self.locator_hint,
            "policy": self.policy,
        }


@dataclass(frozen=True)
class RasterTrackConfig:
    width: int
    height: int
    maximum_rate_millihertz: int = 60_000
    alpha_mode: int = 1
    delta_enabled: bool = False
    maximum_delta_operations: int = 1
    zstd_enabled: bool = False
    slot: int = SLOT_RASTER
    mode: int = TRACK_MODE_LIVE
    lane: int = LANE_BULK
    maximum_encoded_bits_per_second: Optional[int] = None
    maximum_records_per_second: int = 60
    maximum_inflight_body_bytes: Optional[int] = None
    target_latency_us: int = 16_000
    maximum_latency_us: int = 100_000
    retained_pixel_charge: Optional[int] = None
    track_id: Optional[int] = None

    def native(self, session: Session, surface: Surface) -> Dict[str, object]:
        body = _checked_add(72, _checked_mul(_checked_mul(self.width, self.height), 4))
        return _track_common(
            session,
            surface,
            self.track_id,
            "raster",
            self.slot,
            self.mode,
            self.lane,
            body,
            self.maximum_rate_millihertz,
            (
                self.maximum_encoded_bits_per_second
                if self.maximum_encoded_bits_per_second is not None
                else _checked_mul(body, 8 * self.maximum_records_per_second)
            ),
            self.maximum_records_per_second,
            (
                self.maximum_inflight_body_bytes
                if self.maximum_inflight_body_bytes is not None
                else _checked_mul(body, 2)
            ),
            self.target_latency_us,
            self.maximum_latency_us,
            (
                self.retained_pixel_charge
                if self.retained_pixel_charge is not None
                else _checked_mul(self.width, self.height)
            ),
            width=self.width,
            height=self.height,
            alpha_mode=self.alpha_mode,
            delta_enabled=self.delta_enabled,
            maximum_delta_operations=self.maximum_delta_operations,
            zstd_enabled=self.zstd_enabled,
        )


@dataclass(frozen=True)
class ImageTrackConfig:
    width: int
    height: int
    encoded_length: int
    encoding: int
    sha256: Optional[bytes] = None
    cache_lookup: bool = False
    slot: int = SLOT_POSTER
    lane: int = LANE_BULK
    track_id: Optional[int] = None

    def native(self, session: Session, surface: Surface) -> Dict[str, object]:
        return _track_common(
            session,
            surface,
            self.track_id,
            "image",
            self.slot,
            TRACK_MODE_LIVE,
            self.lane,
            self.encoded_length,
            1,
            _checked_mul(self.encoded_length, 8),
            1,
            self.encoded_length,
            0,
            0,
            _checked_mul(self.width, self.height),
            width=self.width,
            height=self.height,
            encoding=self.encoding,
            encoded_length=self.encoded_length,
            sha256=self.sha256,
            cache_lookup=self.cache_lookup,
        )


@dataclass(frozen=True)
class VideoTrackConfig:
    codec: str
    packetization: str
    width: int
    height: int
    maximum_access_unit_bytes: int
    maximum_rate_millihertz: int
    maximum_encoded_bits_per_second: int
    maximum_records_per_second: int
    extradata: bytes = b""
    profile: int = 0
    level: int = 0
    maximum_reorder_depth: int = 0
    color_primaries: int = 1
    transfer: int = 1
    matrix: int = 1
    signal_range: int = 2
    aspect_numerator: int = 1
    aspect_denominator: int = 1
    codec_string: Optional[str] = None
    decoder_configuration: Optional[bytes] = None
    slot: int = SLOT_PRIMARY_VIDEO
    mode: int = TRACK_MODE_LIVE
    lane: int = LANE_BULK
    maximum_inflight_body_bytes: Optional[int] = None
    target_latency_us: int = 100_000
    maximum_latency_us: int = 500_000
    retained_pixel_charge: Optional[int] = None
    track_id: Optional[int] = None

    def native(self, session: Session, surface: Surface) -> Dict[str, object]:
        body = _checked_add(48, self.maximum_access_unit_bytes)
        return _track_common(
            session,
            surface,
            self.track_id,
            "video",
            self.slot,
            self.mode,
            self.lane,
            body,
            self.maximum_rate_millihertz,
            self.maximum_encoded_bits_per_second,
            self.maximum_records_per_second,
            (
                self.maximum_inflight_body_bytes
                if self.maximum_inflight_body_bytes is not None
                else _checked_mul(body, 4)
            ),
            self.target_latency_us,
            self.maximum_latency_us,
            (
                self.retained_pixel_charge
                if self.retained_pixel_charge is not None
                else _checked_mul(self.width, self.height)
            ),
            codec=self.codec,
            packetization=self.packetization,
            extradata=self.extradata,
            width=self.width,
            height=self.height,
            profile=self.profile,
            level=self.level,
            maximum_reorder_depth=self.maximum_reorder_depth,
            color_primaries=self.color_primaries,
            transfer=self.transfer,
            matrix=self.matrix,
            signal_range=self.signal_range,
            aspect_numerator=self.aspect_numerator,
            aspect_denominator=self.aspect_denominator,
            maximum_access_unit_bytes=self.maximum_access_unit_bytes,
            codec_string=self.codec_string,
            decoder_configuration=self.decoder_configuration,
        )


@dataclass(frozen=True)
class AudioTrackConfig:
    codec: str
    packetization: str
    sample_rate: int
    channels: int
    maximum_access_unit_bytes: int
    maximum_encoded_bits_per_second: int
    maximum_records_per_second: int
    extradata: bytes = b""
    channel_mask: int = 0
    codec_string: Optional[str] = None
    slot: int = SLOT_AUDIO
    mode: int = TRACK_MODE_LIVE
    lane: int = LANE_REALTIME
    maximum_rate_millihertz: int = 50_000
    maximum_inflight_body_bytes: Optional[int] = None
    target_latency_us: int = 40_000
    maximum_latency_us: int = 200_000
    track_id: Optional[int] = None

    def native(self, session: Session, surface: Surface) -> Dict[str, object]:
        body = _checked_add(48, self.maximum_access_unit_bytes)
        return _track_common(
            session,
            surface,
            self.track_id,
            "audio",
            self.slot,
            self.mode,
            self.lane,
            body,
            self.maximum_rate_millihertz,
            self.maximum_encoded_bits_per_second,
            self.maximum_records_per_second,
            (
                self.maximum_inflight_body_bytes
                if self.maximum_inflight_body_bytes is not None
                else _checked_mul(body, 8)
            ),
            self.target_latency_us,
            self.maximum_latency_us,
            0,
            codec=self.codec,
            packetization=self.packetization,
            extradata=self.extradata,
            sample_rate=self.sample_rate,
            channels=self.channels,
            channel_mask=self.channel_mask,
            maximum_access_unit_bytes=self.maximum_access_unit_bytes,
            codec_string=self.codec_string,
        )


TrackConfig = Union[
    RasterTrackConfig, ImageTrackConfig, VideoTrackConfig, AudioTrackConfig
]


@dataclass
class ImagePresentation:
    """Live handles for a retained image presentation.

    Keep this object alive for as long as the image should remain in the scene.
    """

    session: Session
    surface: Surface
    track: Track
    channel: TrackChannel

    def close(self) -> None:
        if not self.channel.closed:
            close_channel(self.channel)
        if not self.session.closed:
            destroy_track(self.session, self.track)
            destroy_surface(self.session, self.surface)
            close(self.session)

    def __enter__(self) -> "ImagePresentation":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def connect(
    *,
    dry_run: bool = False,
    trace_dir: Optional[Union[str, Path]] = None,
    endpoint_control: Optional[str] = None,
    endpoint_interactive: Optional[str] = None,
    endpoint_realtime: Optional[str] = None,
    endpoint_bulk: Optional[str] = None,
    root_secret: Optional[str] = None,
    producer_name: str = "vivid-sdk-python",
    producer_version: str = __version__,
    target_profile: str = PROFILE_TERMINAL_SURFACE,
    required_profiles: Optional[Sequence[str]] = None,
    optional_profiles: Optional[Sequence[str]] = None,
) -> Session:
    required = tuple(
        sorted(
            set(
                required_profiles
                if required_profiles is not None
                else (PROFILE_CORE, target_profile)
            )
        )
    )
    optional = tuple(
        sorted(
            set(
                optional_profiles
                if optional_profiles is not None
                else (PROFILE_LIVE_MEDIA, PROFILE_OBSERVABILITY, PROFILE_TIMED_MEDIA)
            ).difference(required)
        )
    )
    return _native.connect(
        dry_run=dry_run,
        trace_dir=trace_dir,
        endpoint_control=endpoint_control,
        endpoint_interactive=endpoint_interactive,
        endpoint_realtime=endpoint_realtime,
        endpoint_bulk=endpoint_bulk,
        root_secret=root_secret,
        producer_name=producer_name,
        producer_version=producer_version,
        target_profile=target_profile,
        required_profiles=required,
        optional_profiles=optional,
    )


def close(session: Session) -> None:
    _native.close(session)


def allocate_id(session: Session) -> int:
    return _native.allocate_id(session)


def supports(session: Session, profile: str) -> bool:
    return _native.supports(session, profile)


def session_info(session: Session) -> SessionInfo:
    return SessionInfo(**_native.session_info(session))


def create_surface(session: Session, config: SurfaceConfig) -> Surface:
    return _native.create_surface(session, config.native(session))


def update_surface(
    session: Session, surface: Surface, config: SurfaceConfig
) -> None:
    native = config.native(session)
    native["context_id"] = surface.context_id
    native["surface_id"] = surface.id
    _native.update_surface(session, surface, native)


def destroy_surface(session: Session, surface: Surface) -> None:
    _native.destroy_surface(session, surface)


def create_track(session: Session, surface: Surface, config: TrackConfig) -> Track:
    return _native.create_track(session, config.native(session, surface))


def destroy_track(session: Session, track: Track) -> None:
    _native.destroy_track(session, track)


def open_track_channel(session: Session, track: Track) -> TrackChannel:
    return _native.open_track_channel(session, track)


def close_channel(channel: TrackChannel) -> None:
    _native.close_channel(channel)


def send_raster(
    channel: TrackChannel,
    rgba: BytesLike,
    *,
    epoch: int = 0,
    frame_id: int = 1,
    compress: bool = False,
) -> int:
    return _native.send_raster(
        channel,
        bytes(rgba),
        epoch=epoch,
        frame_id=frame_id,
        compress=compress,
    )


def send_image(channel: TrackChannel, encoded: BytesLike) -> int:
    return _native.send_image(channel, bytes(encoded))


def send_video(
    channel: TrackChannel,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    key: bool,
    epoch: int = 0,
) -> int:
    return _native.send_video(
        channel,
        bytes(data),
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        key=key,
        epoch=epoch,
    )


def send_audio(
    channel: TrackChannel,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    epoch: int = 0,
    trim_start_samples: int = 0,
    trim_end_samples: int = 0,
) -> int:
    return _native.send_audio(
        channel,
        bytes(data),
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        epoch=epoch,
        trim_start_samples=trim_start_samples,
        trim_end_samples=trim_end_samples,
    )


def channel_eos(channel: TrackChannel) -> int:
    return _native.channel_eos(channel)


def activate_track(
    session: Session,
    surface: Surface,
    track: Track,
    *,
    required_milestone: int = MILESTONE_OUTPUT_READY,
) -> int:
    return _native.activate_track(
        session,
        surface,
        track,
        required_milestone=required_milestone,
    )


def wait_track(
    session: Session,
    track: Track,
    *,
    condition: int,
    value: Optional[int] = None,
    timeout_us: int = 30_000_000,
) -> WaitSatisfied:
    return WaitSatisfied(
        **_native.wait_track(
            session,
            track,
            condition=condition,
            value=value,
            timeout_us=timeout_us,
        )
    )


def place_terminal_surface(
    session: Session,
    surface: Surface,
    *,
    node_id: Optional[int] = None,
    x: int = 0,
    y: int = 0,
    width: int,
    height: int,
    text_layer: int = 1,
) -> Tuple[int, int]:
    return _native.place_terminal_surface(
        session,
        surface,
        node_id=(allocate_id(session) if node_id is None else node_id),
        x=x,
        y=y,
        width=width,
        height=height,
        text_layer=text_layer,
    )


def anchor_marker(
    session: Session, *, context_id: Optional[int] = None, anchor_id: Optional[int] = None
) -> str:
    info = session_info(session)
    return _native.anchor_marker(
        session,
        info.root_context_id if context_id is None else context_id,
        allocate_id(session) if anchor_id is None else anchor_id,
    )


def display_image(
    path: Union[str, Path],
    *,
    columns: Optional[int] = None,
    rows: Optional[int] = None,
    **connect_options: Any,
) -> ImagePresentation:
    """Create and retain one PNG/JPEG presentation.

    The returned object owns the live session. Call ``close()`` when the image
    should disappear.
    """

    encoded = Path(path).read_bytes()
    encoding, width, height = _image_info(encoded)
    session = connect(**connect_options)
    try:
        surface = create_surface(
            session,
            SurfaceConfig(
                logical_width=width,
                logical_height=height,
                role=ROLE_FIGURE,
                title=Path(path).name,
            ),
        )
        place_terminal_surface(
            session,
            surface,
            width=(columns if columns is not None else min(width, 80)) << 32,
            height=(rows if rows is not None else min(height, 24)) << 32,
        )
        track = create_track(
            session,
            surface,
            ImageTrackConfig(
                width=width,
                height=height,
                encoded_length=len(encoded),
                encoding=encoding,
                sha256=hashlib.sha256(encoded).digest(),
            ),
        )
        channel = open_track_channel(session, track)
        send_image(channel, encoded)
        return ImagePresentation(session, surface, track, channel)
    except BaseException:
        close(session)
        raise


def _track_common(
    session: Session,
    surface: Surface,
    track_id: Optional[int],
    kind: str,
    slot: int,
    mode: int,
    lane: int,
    maximum_record_body: int,
    maximum_rate_millihertz: int,
    maximum_encoded_bits_per_second: int,
    maximum_records_per_second: int,
    maximum_inflight_body_bytes: int,
    target_latency_us: int,
    maximum_latency_us: int,
    retained_pixel_charge: int,
    **kind_values: object,
) -> Dict[str, object]:
    result: Dict[str, object] = {
        "context_id": surface.context_id,
        "surface_id": surface.id,
        "track_id": allocate_id(session) if track_id is None else track_id,
        "kind": kind,
        "slot": slot,
        "mode": mode,
        "lane": lane,
        "maximum_record_body": maximum_record_body,
        "maximum_rate_millihertz": maximum_rate_millihertz,
        "maximum_encoded_bits_per_second": maximum_encoded_bits_per_second,
        "maximum_records_per_second": maximum_records_per_second,
        "maximum_inflight_body_bytes": maximum_inflight_body_bytes,
        "target_latency_us": target_latency_us,
        "maximum_latency_us": maximum_latency_us,
        "retained_pixel_charge": retained_pixel_charge,
    }
    result.update(kind_values)
    return result


def _checked_mul(left: int, right: int) -> int:
    value = left * right
    if left < 0 or right < 0 or value > (1 << 64) - 1:
        raise ValueError("resource claim overflows u64")
    return value


def _checked_add(left: int, right: int) -> int:
    value = left + right
    if left < 0 or right < 0 or value > (1 << 64) - 1:
        raise ValueError("resource claim overflows u64")
    return value


def _image_info(data: bytes) -> Tuple[int, int, int]:
    if data.startswith(b"\x89PNG\r\n\x1a\n") and len(data) >= 24:
        width, height = struct.unpack(">II", data[16:24])
        return IMAGE_PNG, width, height
    if data.startswith(b"\xff\xd8"):
        offset = 2
        while offset + 4 <= len(data):
            if data[offset] != 0xFF:
                raise ValueError("invalid JPEG marker stream")
            marker = data[offset + 1]
            offset += 2
            if marker in (0xD8, 0xD9):
                continue
            length = int.from_bytes(data[offset : offset + 2], "big")
            if length < 2 or offset + length > len(data):
                raise ValueError("truncated JPEG segment")
            if marker in range(0xC0, 0xC4):
                if length < 7:
                    raise ValueError("invalid JPEG frame header")
                height = int.from_bytes(data[offset + 3 : offset + 5], "big")
                width = int.from_bytes(data[offset + 5 : offset + 7], "big")
                return IMAGE_JPEG, width, height
            offset += length
    raise ValueError("only complete PNG and JPEG images are supported")


from . import aio as aio  # noqa: E402

__all__ = [
    "AudioTrackConfig",
    "ClosedHandleError",
    "ImagePresentation",
    "ImageTrackConfig",
    "RasterTrackConfig",
    "Session",
    "SessionInfo",
    "Surface",
    "SurfaceConfig",
    "Track",
    "TrackChannel",
    "VideoTrackConfig",
    "VividError",
    "activate_track",
    "aio",
    "allocate_id",
    "anchor_marker",
    "channel_eos",
    "close",
    "close_channel",
    "connect",
    "create_surface",
    "create_track",
    "destroy_surface",
    "destroy_track",
    "display_image",
    "open_track_channel",
    "place_terminal_surface",
    "send_audio",
    "send_image",
    "send_raster",
    "send_video",
    "session_info",
    "supports",
    "update_surface",
]
