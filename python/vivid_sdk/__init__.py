"""Typed Python producer SDK for Vivid Protocol 1.5.

The API intentionally uses the 1.5 object model: stable surfaces own immutable
tracks, and each track is fed through an authenticated channel generation.
"""

from __future__ import annotations

import hashlib
import os
import secrets
import struct
import sys
import time
from dataclasses import dataclass
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
from typing import Any, Dict, Optional, Sequence, Tuple, Union

from . import _native
from ._native import (
    ClosedHandleError,
    IncomingFileTransfer,
    InputLane,
    TrackSender,
    VideoRateControl,
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

# Protocol constants, read from the Rust table that owns them.
#
# The names are this module's API; the values are not. Declaring them explicitly is what
# lets a type checker see them, and reading them from `_native` is what keeps them from
# drifting from `vivid_protocol`: a rename there fails this import instead of silently
# changing a wire value here.
_CONSTANTS: Dict[str, Tuple[Optional[str], Optional[int]]] = {
    name: (text, number)
    for name, text, number in _native.constant_table()
}


def _constant_text(name: str) -> str:
    """The wire name of a profile or semantic-profile constant."""
    text, _ = _CONSTANTS[name]
    if text is None:
        raise KeyError(f"{name} is not a profile constant")
    return text


def _constant_number(name: str) -> int:
    """The numeric assignment of a bit, bound, or identifier constant."""
    _, number = _CONSTANTS[name]
    if number is None:
        raise KeyError(f"{name} is not a numeric constant")
    return number


PROFILE_CORE: str = _constant_text("PROFILE_CORE")
PROFILE_TERMINAL_SURFACE: str = _constant_text("PROFILE_TERMINAL_SURFACE")
PROFILE_DESKTOP_SURFACE: str = _constant_text("PROFILE_DESKTOP_SURFACE")
PROFILE_CANVAS_SURFACE: str = _constant_text("PROFILE_CANVAS_SURFACE")
PROFILE_LIVE_MEDIA: str = _constant_text("PROFILE_LIVE_MEDIA")
PROFILE_TIMED_MEDIA: str = _constant_text("PROFILE_TIMED_MEDIA")
PROFILE_AUDIO_GAIN: str = _constant_text("PROFILE_AUDIO_GAIN")
PROFILE_AUDIO_INPUT: str = _constant_text("PROFILE_AUDIO_INPUT")
PROFILE_DESKTOP_INPUT: str = _constant_text("PROFILE_DESKTOP_INPUT")
PROFILE_FILE_DROP: str = _constant_text("PROFILE_FILE_DROP")
PROFILE_FILE_DROP_PATH: str = _constant_text("PROFILE_FILE_DROP_PATH")
PROFILE_OBSERVABILITY: str = _constant_text("PROFILE_OBSERVABILITY")
PROFILE_WEB_CARRIER: str = _constant_text("PROFILE_WEB_CARRIER")
PROFILE_TERMINAL_OVERLAY: str = _constant_text("PROFILE_TERMINAL_OVERLAY")
PROFILE_VECTOR_SCENE: str = _constant_text("PROFILE_VECTOR_SCENE")
PROFILE_OVERLAY_INPUT: str = _constant_text("PROFILE_OVERLAY_INPUT")
PROFILE_OVERLAY_TEXT: str = _constant_text("PROFILE_OVERLAY_TEXT")
PROFILE_OVERLAY_TEXT_LAYOUT: str = _constant_text("PROFILE_OVERLAY_TEXT_LAYOUT")
PROFILE_OVERLAY_TYPOGRAPHY: str = _constant_text("PROFILE_OVERLAY_TYPOGRAPHY")
SURFACE_GENERIC: str = _constant_text("SURFACE_GENERIC")
SURFACE_TERMINAL: str = _constant_text("SURFACE_TERMINAL")
SURFACE_DESKTOP: str = _constant_text("SURFACE_DESKTOP")
SURFACE_CANVAS: str = _constant_text("SURFACE_CANVAS")
COORDINATE_DESKTOP_LOGICAL_PIXELS: int = _constant_number("COORDINATE_DESKTOP_LOGICAL_PIXELS")
COORDINATE_NORMALIZED: int = _constant_number("COORDINATE_NORMALIZED")
COORDINATE_CANVAS_LOGICAL_UNITS: int = _constant_number("COORDINATE_CANVAS_LOGICAL_UNITS")
COORDINATE_TERMINAL_CONTENT_CELLS: int = _constant_number("COORDINATE_TERMINAL_CONTENT_CELLS")
ROLE_UNSPECIFIED: int = _constant_number("ROLE_UNSPECIFIED")
ROLE_DOCUMENT: int = _constant_number("ROLE_DOCUMENT")
ROLE_DESKTOP: int = _constant_number("ROLE_DESKTOP")
ROLE_TIMED_MEDIA: int = _constant_number("ROLE_TIMED_MEDIA")
ROLE_FIGURE: int = _constant_number("ROLE_FIGURE")
ROLE_TERMINAL: int = _constant_number("ROLE_TERMINAL")
ROLE_CANVAS: int = _constant_number("ROLE_CANVAS")
POLICY_DENY_CAPTURE: int = _constant_number("POLICY_DENY_CAPTURE")
POLICY_DENY_DESCRIPTOR_EXPORT: int = _constant_number("POLICY_DENY_DESCRIPTOR_EXPORT")
POLICY_DENY_POSTER_RETENTION: int = _constant_number("POLICY_DENY_POSTER_RETENTION")
POLICY_DENY_IMAGE_CACHE: int = _constant_number("POLICY_DENY_IMAGE_CACHE")
POLICY_REDUCED_DIAGNOSTICS: int = _constant_number("POLICY_REDUCED_DIAGNOSTICS")
POLICY_KNOWN_MASK: int = _constant_number("POLICY_KNOWN_MASK")
TRACK_MODE_LIVE: int = _constant_number("TRACK_MODE_LIVE")
TRACK_MODE_TIMED: int = _constant_number("TRACK_MODE_TIMED")
TRACK_DIRECTION_DOWNLINK: int = _constant_number("TRACK_DIRECTION_DOWNLINK")
TRACK_DIRECTION_UPLINK: int = _constant_number("TRACK_DIRECTION_UPLINK")
TRACK_KIND_VIDEO: int = _constant_number("TRACK_KIND_VIDEO")
TRACK_KIND_AUDIO: int = _constant_number("TRACK_KIND_AUDIO")
TRACK_KIND_RASTER: int = _constant_number("TRACK_KIND_RASTER")
TRACK_KIND_IMAGE: int = _constant_number("TRACK_KIND_IMAGE")
TRACK_KIND_VECTOR: int = _constant_number("TRACK_KIND_VECTOR")
LANE_CONTROL: int = _constant_number("LANE_CONTROL")
LANE_INTERACTIVE: int = _constant_number("LANE_INTERACTIVE")
LANE_REALTIME: int = _constant_number("LANE_REALTIME")
LANE_BULK: int = _constant_number("LANE_BULK")
SLOT_NONE: int = _constant_number("SLOT_NONE")
SLOT_PRIMARY_VIDEO: int = _constant_number("SLOT_PRIMARY_VIDEO")
SLOT_AUDIO: int = _constant_number("SLOT_AUDIO")
SLOT_RASTER: int = _constant_number("SLOT_RASTER")
SLOT_POSTER: int = _constant_number("SLOT_POSTER")
SLOT_VECTOR: int = _constant_number("SLOT_VECTOR")
FIT_FILL: int = _constant_number("FIT_FILL")
FIT_CONTAIN: int = _constant_number("FIT_CONTAIN")
FIT_COVER: int = _constant_number("FIT_COVER")
FIT_NONE: int = _constant_number("FIT_NONE")
IMAGE_PNG: int = _constant_number("IMAGE_PNG")
IMAGE_JPEG: int = _constant_number("IMAGE_JPEG")
MILESTONE_CHANNEL_ACCEPTED: int = _constant_number("MILESTONE_CHANNEL_ACCEPTED")
MILESTONE_FIRST_MEDIA: int = _constant_number("MILESTONE_FIRST_MEDIA")
MILESTONE_DECODER_INITIALIZED: int = _constant_number("MILESTONE_DECODER_INITIALIZED")
MILESTONE_RANDOM_ACCESS: int = _constant_number("MILESTONE_RANDOM_ACCESS")
MILESTONE_OUTPUT_READY: int = _constant_number("MILESTONE_OUTPUT_READY")
MILESTONE_PRESENTED: int = _constant_number("MILESTONE_PRESENTED")
MILESTONE_CLOCK_STARTED: int = _constant_number("MILESTONE_CLOCK_STARTED")
MILESTONE_EOS_ACCEPTED: int = _constant_number("MILESTONE_EOS_ACCEPTED")
MILESTONE_BUFFERED_ENDED: int = _constant_number("MILESTONE_BUFFERED_ENDED")
MILESTONE_CHANNEL_DETACHED: int = _constant_number("MILESTONE_CHANNEL_DETACHED")
MILESTONE_TRACK_LOST: int = _constant_number("MILESTONE_TRACK_LOST")
MILESTONE_KNOWN_MASK: int = _constant_number("MILESTONE_KNOWN_MASK")
WAIT_REVISION_GREATER: int = _constant_number("WAIT_REVISION_GREATER")
WAIT_MILESTONE_SET: int = _constant_number("WAIT_MILESTONE_SET")
WAIT_RASTER_FRAME_PRESENTED: int = _constant_number("WAIT_RASTER_FRAME_PRESENTED")
WAIT_VIDEO_PTS_PRESENTED: int = _constant_number("WAIT_VIDEO_PTS_PRESENTED")
WAIT_PLAYBACK_STARTED: int = _constant_number("WAIT_PLAYBACK_STARTED")
WAIT_PLAYBACK_ENDED: int = _constant_number("WAIT_PLAYBACK_ENDED")
WAIT_CHANNEL_ACCEPTED: int = _constant_number("WAIT_CHANNEL_ACCEPTED")
WAIT_CHANNEL_CLOSED: int = _constant_number("WAIT_CHANNEL_CLOSED")
WAIT_TRACK_LOST: int = _constant_number("WAIT_TRACK_LOST")
MAX_TRACK_WAIT_TIMEOUT_US: int = _constant_number("MAX_TRACK_WAIT_TIMEOUT_US")
OP_OBSERVE: int = _constant_number("OP_OBSERVE")
OP_SURFACE_TRACK_MEDIA: int = _constant_number("OP_SURFACE_TRACK_MEDIA")
OP_SCENE: int = _constant_number("OP_SCENE")
OP_TERMINAL_ANCHOR: int = _constant_number("OP_TERMINAL_ANCHOR")
OP_DESKTOP_INPUT: int = _constant_number("OP_DESKTOP_INPUT")
OP_DELEGATE: int = _constant_number("OP_DELEGATE")
OP_RECEIVE_FILE_DROP: int = _constant_number("OP_RECEIVE_FILE_DROP")
OP_KNOWN_MASK: int = _constant_number("OP_KNOWN_MASK")
INPUT_CLASS_KEYBOARD: int = _constant_number("INPUT_CLASS_KEYBOARD")
INPUT_CLASS_POINTER_MOTION: int = _constant_number("INPUT_CLASS_POINTER_MOTION")
INPUT_CLASS_POINTER_BUTTON: int = _constant_number("INPUT_CLASS_POINTER_BUTTON")
INPUT_CLASS_POINTER_AXIS: int = _constant_number("INPUT_CLASS_POINTER_AXIS")
INPUT_CLASS_KNOWN_MASK: int = _constant_number("INPUT_CLASS_KNOWN_MASK")
MIN_WATCHDOG_US: int = _constant_number("MIN_WATCHDOG_US")
MAX_WATCHDOG_US: int = _constant_number("MAX_WATCHDOG_US")
COORDINATE_SPACE_GRID_CELL: int = _constant_number("COORDINATE_SPACE_GRID_CELL")
TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH: int = _constant_number("TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH")
MINIMUM_TARGET_BITS_PER_SECOND: int = _constant_number("MINIMUM_TARGET_BITS_PER_SECOND")
DEFAULT_ACTIVATION_TIMEOUT_US: int = _constant_number("DEFAULT_ACTIVATION_TIMEOUT_US")
MAX_ACTIVATION_TIMEOUT_US: int = _constant_number("MAX_ACTIVATION_TIMEOUT_US")
CLEANUP_IMMEDIATE: int = _constant_number("CLEANUP_IMMEDIATE")
CLEANUP_SUSPEND_ON_UNCLEAN_LOSS: int = _constant_number("CLEANUP_SUSPEND_ON_UNCLEAN_LOSS")
DESTINATION_SHELL_CWD: int = _constant_number("DESTINATION_SHELL_CWD")
DESTINATION_DESKTOP_FOLDER: int = _constant_number("DESTINATION_DESKTOP_FOLDER")
DROP_OFFERED: int = _constant_number("DROP_OFFERED")
DROP_ACCEPTED: int = _constant_number("DROP_ACCEPTED")
DROP_TRANSFERRING: int = _constant_number("DROP_TRANSFERRING")
DROP_COMMITTED: int = _constant_number("DROP_COMMITTED")
DROP_CANCELLED: int = _constant_number("DROP_CANCELLED")
DROP_FAILED: int = _constant_number("DROP_FAILED")
MIC_PACKET_US: int = _constant_number("MIC_PACKET_US")
MIC_PACKET_BYTES: int = _constant_number("MIC_PACKET_BYTES")


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
class SceneNode:
    """One node in a surface's retained scene.

    `geometry` is the protocol's integer-keyed map; pass the keys the surface's coordinate model
    defines. `fit` decides how content that does not match the geometry is placed.
    """

    node_id: int
    geometry: Dict[int, Union[int, str, bool]]
    fit: int = FIT_CONTAIN
    linear_sampling: bool = True
    z_index: int = 0
    visible: bool = True
    opacity: int = 0xFFFF  # `0..=65535`, so the byte-sized 255 is nearly transparent.

    def native(self, session: Session, surface: object) -> Dict[str, object]:
        del session, surface  # Identity defaults come from the Rust scene builder.
        return {
            "node_id": self.node_id,
            "geometry": dict(self.geometry),
            "fit": self.fit,
            "linear_sampling": self.linear_sampling,
            "z_index": self.z_index,
            "visible": self.visible,
            "opacity": self.opacity,
        }


@dataclass(frozen=True)
class SlotBinding:
    """One slot's activation binding, naming the exact channel generation it expects."""

    slot: int
    track_id: int
    expected_channel_generation: int
    required_milestone: int = MILESTONE_OUTPUT_READY

    def native(self) -> Dict[str, int]:
        return {
            "slot": self.slot,
            "track_id": self.track_id,
            "expected_channel_generation": self.expected_channel_generation,
            "required_milestone": self.required_milestone,
        }


@dataclass(frozen=True)
class InputBinding:
    """A request to inject input on a surface, as a class mask with a watchdog bound."""

    producer_epoch: int
    context_id: int
    surface_id: int
    surface_generation: int
    requested_classes: int
    reason: int = 1
    requested_watchdog_us: int = 1_000_000

    def native(self) -> Dict[str, int]:
        return {
            "producer_epoch": self.producer_epoch,
            "context_id": self.context_id,
            "surface_id": self.surface_id,
            "surface_generation": self.surface_generation,
            "requested_classes": self.requested_classes,
            "reason": self.reason,
            "requested_watchdog_us": self.requested_watchdog_us,
        }


@dataclass(frozen=True)
class OutputConfig:
    """One captured output in a desktop topology."""

    output_id: int
    origin_x: int
    origin_y: int
    width: int
    height: int
    scale_numerator: int = 1
    scale_denominator: int = 1
    rotation: int = 0
    primary: bool = False

    def native(self) -> Dict[str, object]:
        return {
            "output_id": self.output_id,
            "origin_x": self.origin_x,
            "origin_y": self.origin_y,
            "width": self.width,
            "height": self.height,
            "scale_numerator": self.scale_numerator,
            "scale_denominator": self.scale_denominator,
            "rotation": self.rotation,
            "primary": self.primary,
        }


@dataclass(frozen=True)
class DesktopParameters:
    """Typed parameters that make a surface a desktop surface.

    Without these a `desktop-content-v1` surface carries no captured origin, topology, or input
    capabilities, and a presenter has nothing to map input back onto.
    """

    captured_origin_x: int
    captured_origin_y: int
    topology: Tuple[OutputConfig, ...]
    semantic_generation: int
    input_capabilities: int = 0

    def native(self) -> Dict[str, object]:
        return {
            "captured_origin_x": self.captured_origin_x,
            "captured_origin_y": self.captured_origin_y,
            "topology": [output.native() for output in self.topology],
            "semantic_generation": self.semantic_generation,
            "input_capabilities": self.input_capabilities,
        }


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

    desktop_parameters: Optional["DesktopParameters"] = None

    def native(self, session: Session) -> Dict[str, object]:
        """The configuration dict `create_surface` takes, from the Rust surface builder.

        Identity, profile, and geometry defaults live in `vivid_sdk::SurfaceBuilder`, so a
        binding cannot disagree with the SDK about what an unspecified field means.
        """
        request: Dict[str, object] = {
            "logical_width": self.logical_width,
            "logical_height": self.logical_height,
            "semantic_profile": self.semantic_profile,
            "coordinate_model": self.coordinate_model,
            "role": self.role,
            "title": self.title,
            "policy": self.policy,
        }
        request["semantic_content_revision"] = self.semantic_content_revision
        request["semantic_availability"] = self.semantic_availability
        request["locator_hint"] = self.locator_hint
        if (self.scale_numerator, self.scale_denominator, self.rotation) != (1, 1, 0):
            request["scale_numerator"] = self.scale_numerator
            request["scale_denominator"] = self.scale_denominator
            request["rotation"] = self.rotation
        if self.context_id is not None:
            request["context_id"] = self.context_id
        if self.surface_id is not None:
            request["surface_id"] = self.surface_id
        if self.desktop_parameters is not None:
            request["desktop_parameters"] = self.desktop_parameters.native()
        return _native.build_surface_config(session, request)


@dataclass(frozen=True)
class RasterTrackConfig:
    """A retained raster track.

    A full frame is a fixed size, so the record-body, in-flight, and retained-pixel claims all
    follow from the geometry and are computed in Rust. State them only to narrow them.
    """

    width: int
    height: int
    maximum_rate_millihertz: Optional[int] = None
    alpha_mode: int = 1
    delta_enabled: bool = False
    maximum_delta_operations: int = 1
    zstd_enabled: bool = False
    slot: int = SLOT_RASTER
    mode: int = TRACK_MODE_LIVE
    lane: int = LANE_BULK
    maximum_encoded_bits_per_second: Optional[int] = None
    track_id: Optional[int] = None

    def native(self, session: Session, surface: object) -> Dict[str, object]:
        return _track_native(
            session,
            surface,
            "raster",
            slot=self.slot,
            lane=self.lane,
            mode=self.mode,
            track_id=self.track_id,
            maximum_rate_millihertz=self.maximum_rate_millihertz,
            maximum_encoded_bits_per_second=self.maximum_encoded_bits_per_second,
            width=self.width,
            height=self.height,
            alpha_mode=self.alpha_mode,
            delta_enabled=self.delta_enabled,
            maximum_delta_operations=self.maximum_delta_operations,
            zstd_enabled=self.zstd_enabled,
        )


@dataclass(frozen=True)
class ImageTrackConfig:
    """A one-shot encoded-image track.

    The container is inspected by `probe_encoded_image`, so the declared dimensions and length
    are the file's real ones; pass `sha256` to let a presenter cache the image across
    presentations.
    """

    encoded: bytes
    sha256: Optional[bytes] = None
    cache_lookup: bool = False
    slot: int = SLOT_POSTER
    lane: int = LANE_BULK
    track_id: Optional[int] = None

    def native(self, session: Session, surface: object) -> Dict[str, object]:
        return _track_native(
            session,
            surface,
            "image",
            slot=self.slot,
            lane=self.lane,
            track_id=self.track_id,
            encoded=self.encoded,
            sha256=self.sha256,
            cache_lookup=self.cache_lookup,
        )


@dataclass(frozen=True)
class VideoTrackConfig:
    """A live or timed video track.

    Packetization defaults to `<codec>-annexb-au-v1`; state it only for a codec that spells its
    framing differently. The record-body, in-flight, and decoded-pixel claims follow from the
    coded size.
    """

    codec: str
    width: int
    height: int
    packetization: Optional[str] = None
    maximum_access_unit_bytes: Optional[int] = None
    maximum_rate_millihertz: Optional[int] = None
    maximum_encoded_bits_per_second: Optional[int] = None
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
    track_id: Optional[int] = None

    def native(self, session: Session, surface: object) -> Dict[str, object]:
        return _track_native(
            session,
            surface,
            "video",
            slot=self.slot,
            lane=self.lane,
            mode=self.mode,
            track_id=self.track_id,
            maximum_rate_millihertz=self.maximum_rate_millihertz,
            maximum_encoded_bits_per_second=self.maximum_encoded_bits_per_second,
            width=self.width,
            height=self.height,
            codec=self.codec,
            packetization=self.packetization,
            extradata=self.extradata,
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
    """An audio track. Defaults to Opus in 20 ms packets on the realtime lane."""

    sample_rate: int
    channels: int
    codec: str = "opus"
    packetization: Optional[str] = None
    maximum_access_unit_bytes: Optional[int] = None
    maximum_encoded_bits_per_second: Optional[int] = None
    maximum_rate_millihertz: Optional[int] = None
    extradata: bytes = b""
    channel_mask: int = 0
    codec_string: Optional[str] = None
    slot: int = SLOT_AUDIO
    mode: int = TRACK_MODE_LIVE
    lane: int = LANE_REALTIME
    uplink: bool = False
    track_id: Optional[int] = None

    def native(self, session: Session, surface: object) -> Dict[str, object]:
        return _track_native(
            session,
            surface,
            "audio",
            slot=self.slot,
            lane=self.lane,
            mode=self.mode,
            track_id=self.track_id,
            maximum_rate_millihertz=self.maximum_rate_millihertz,
            maximum_encoded_bits_per_second=self.maximum_encoded_bits_per_second,
            uplink=self.uplink,
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


def _pane_options(
    title: str,
    columns: Optional[int],
    rows: Optional[int],
    text_layer: int,
) -> Dict[str, object]:
    """Pane image options with unset entries left out, which is how the SDK reads "use the default"."""
    options: Dict[str, object] = {"title": title, "text_layer": text_layer}
    if columns is not None:
        options["columns"] = columns
    if rows is not None:
        options["rows"] = rows
    return options


class PaneSession:
    """One image in one terminal pane, over the SDK's own pane state machine.

    The node/surface/track lifecycle, the fixed-point cell geometry, and the 80x24 defaults all
    live in `vivid_sdk::PaneSession`; this class adds the connect options and the repr a Python
    caller expects, and nothing else.
    """

    def __init__(self, session: Session) -> None:
        """Adopt an established session, which the pane then owns."""
        self._pane = _native.PaneSession.from_session(session)

    @classmethod
    def from_env(cls, **connect_options: Any) -> "PaneSession":
        """Connect through the standard discovery environment, or a dry run for tests."""
        session = connect(**connect_options)
        try:
            return cls(session)
        except BaseException:
            close(session)
            raise

    @property
    def closed(self) -> bool:
        return bool(self._pane.closed)

    def __repr__(self) -> str:
        # Deliberately says whether a presentation exists and nothing about the endpoint or the
        # capability behind it.
        return f"PaneSession(has_presentation={bool(self._pane.has_presentation)})"

    def show_encoded_image(
        self,
        encoded: BytesLike,
        *,
        title: str = "image",
        columns: Optional[int] = None,
        rows: Optional[int] = None,
        text_layer: int = TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH,
    ) -> None:
        """Present one complete PNG or JPEG, replacing any current presentation."""
        _native.pane_show_encoded_image(
            self._pane,
            bytes(encoded),
            _pane_options(title, columns, rows, text_layer),
        )

    def show_rgba(
        self,
        width: int,
        height: int,
        rgba: BytesLike,
        *,
        title: str = "image",
        columns: Optional[int] = None,
        rows: Optional[int] = None,
        text_layer: int = TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH,
    ) -> None:
        """Present one tightly packed sRGB RGBA8 frame, replacing any current presentation."""
        _native.pane_show_rgba(
            self._pane,
            width,
            height,
            bytes(rgba),
            _pane_options(title, columns, rows, text_layer),
        )

    def clear(self) -> None:
        """Remove the current presentation; idempotent."""
        _native.pane_clear(self._pane)

    def close(self) -> None:
        _native.pane_close(self._pane)

    def __enter__(self) -> "PaneSession":
        return self

    def __exit__(self, *_: object) -> None:
        self.clear()
        self.close()


def connect(
    *,
    dry_run: bool = False,
    desktop: bool = False,
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
        desktop=desktop,
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


def delete_node(
    session: Session, context_id: int, node_id: int
) -> Tuple[int, int]:
    return _native.delete_node(session, context_id, node_id)


def anchor_marker(
    session: Session, *, context_id: Optional[int] = None, anchor_id: Optional[int] = None
) -> str:
    info = session_info(session)
    return _native.anchor_marker(
        session,
        info.root_context_id if context_id is None else context_id,
        allocate_id(session) if anchor_id is None else anchor_id,
    )


_ANCHOR_STATE_READY = 1


def _anchor_at_cursor(session: Session, *, timeout_us: int = 2_000_000) -> Optional[int]:
    """Create an authenticated anchor at the terminal's current cursor cell.

    The zero-width marker is the one thing a producer may write to the PTY, and the anchor it
    creates is what lets the presenter keep a node with the text it belongs to: anchored nodes
    follow scroll and reflow, and only they can become a retained poster after a clean
    `GOODBYE`. Returns `None` when this session has no text plane to anchor to, leaving the
    caller to position against the terminal grid instead.
    """

    if session_info(session).target_profile != PROFILE_TERMINAL_SURFACE:
        return None
    if os.environ.get("TMUX") or os.environ.get("STY"):
        return None  # A foreign multiplexer owns the text stream and drops the marker.
    try:
        if not sys.stdout.isatty():
            return None
    except ValueError:
        return None  # A closed stream is not a text plane either.
    context_id = session_info(session).root_context_id
    # The anchor ID authenticates the marker, so it comes from the CSPRNG rather than from the
    # session's sequential object IDs, and is never reused in this context.
    anchor_id = 0
    while anchor_id == 0:
        anchor_id = secrets.randbits(64)
    try:
        marker = (
            conpty_anchor_marker(session, context_id, anchor_id)
            if os.environ.get("VIVID_ANCHOR_TRANSPORT") == "conpty"
            else anchor_marker(session, context_id=context_id, anchor_id=anchor_id)
        )
        sys.stdout.write(marker)
        sys.stdout.flush()
        # A node may only name an anchor the presenter has already created, so wait for the
        # marker to be recognized. Polling leaves the session's event queue to its owner.
        deadline = time.monotonic() + timeout_us / 1_000_000
        while True:
            if query_anchor(session, context_id, anchor_id).get("state") == _ANCHOR_STATE_READY:
                return anchor_id
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.005)
    except OSError:
        # `VividError` is an `OSError`: a target that refuses the anchor, and a stream that
        # refuses the write, both leave the caller with the grid rather than with a failure.
        return None


def _place_image_node(
    session: Session, surface: Surface, *, columns: int, rows: int
) -> None:
    """Place `surface` at the cursor, or against the grid when there is no anchor to use."""

    anchor_id = _anchor_at_cursor(session)
    if anchor_id is None:
        place_terminal_surface(session, surface, width=columns << 32, height=rows << 32)
        return
    create_node(
        session,
        surface,
        SceneNode(
            node_id=allocate_id(session),
            geometry={
                0: 2,  # Anchor-cell space: (0,0) is the anchor's own cell.
                1: 0,
                2: 0,
                3: columns << 32,
                4: rows << 32,
                5: 1,
                6: session_info(session).root_context_id,
                7: anchor_id,
            },
        ),
    )
    # Move the text cursor past the rectangle the image now occupies, so the shell prompt and
    # anything printed next land below it rather than behind it. Only this ordinary whitespace
    # crosses the PTY, and the anchor carries the image along when these lines scroll.
    sys.stdout.write("\n" * rows)
    sys.stdout.flush()


def display_image(
    path: Union[str, Path],
    *,
    columns: Optional[int] = None,
    rows: Optional[int] = None,
    **connect_options: Any,
) -> ImagePresentation:
    """Create, activate, and retain one PNG/JPEG presentation.

    Returns once the presenter has accepted the image into the surface's active slot. The
    returned object owns the live session: call ``close()`` when the image should disappear,
    or ``close(session)`` for a clean GOODBYE that leaves the retained image with the
    presenter after this process exits.

    The image is anchored to the cursor cell where it is written, and the text cursor is then
    advanced past it so ordinary output continues underneath. A target with no text plane to
    anchor to — a redirected stdout, a foreign multiplexer, a non-terminal profile — falls back
    to the terminal grid, where the presenter shows the image but retains nothing once this
    session ends.
    """

    encoded = Path(path).read_bytes()
    # The container is read once, in Rust, so the surface geometry, the track's declared
    # dimensions, and the bytes that are sent all come from the same parse.
    _, width, height, _ = probe_encoded_image(encoded)
    cell_columns = columns if columns is not None else min(width, 80)
    cell_rows = rows if rows is not None else min(height, 24)
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
        _place_image_node(session, surface, columns=cell_columns, rows=cell_rows)
        track = create_track(
            session,
            surface,
            ImageTrackConfig(
                encoded=encoded,
                sha256=hashlib.sha256(encoded).digest(),
            ),
        )
        channel = open_track_channel(session, track)
        send_image(channel, encoded)
        # Media and control use independent connections: submitting the bytes does not
        # establish the presenter's readiness for slot activation, so the track must reach
        # OUTPUT_READY before it can take the surface's slot. This is the same handshake the
        # pane state machine performs; whether the presented milestone follows depends on the
        # presenter's own downstream, so it is not part of this helper's contract.
        wait_track(session, track, condition=WAIT_MILESTONE_SET, value=MILESTONE_OUTPUT_READY)
        activate_track(session, surface, track, required_milestone=MILESTONE_OUTPUT_READY)
        return ImagePresentation(session, surface, track, channel)
    except BaseException:
        close(session)
        raise


def _surface_identity(surface: object) -> Dict[str, int]:
    """The context and surface a track configuration should name.

    Accepts either a surface configuration, for a track being described before its surface
    exists, or a live surface handle, for one that already does.
    """
    if isinstance(surface, dict):
        return {
            "context_id": int(surface["context_id"]),
            "surface_id": int(surface["surface_id"]),
        }
    return {"context_id": int(surface.context_id), "surface_id": int(surface.id)}  # type: ignore[attr-defined]


def _track_native(
    session: Session,
    surface: object,
    kind: str,
    *,
    slot: int,
    lane: int,
    mode: int = TRACK_MODE_LIVE,
    track_id: Optional[int] = None,
    maximum_rate_millihertz: Optional[int] = None,
    maximum_encoded_bits_per_second: Optional[int] = None,
    uplink: bool = False,
    **details: object,
) -> Dict[str, object]:
    """Build one track configuration through the Rust track builder.

    Every claim the protocol bounds — record body, in-flight bytes, retained pixels, decoded
    pixels, and the rate defaults that follow from them — is computed by `vivid_sdk`, with
    checked arithmetic. This layer states only what the caller asked for, so an unset claim
    keeps the builder's default for that kind instead of a number copied here.
    """
    request: Dict[str, object] = {"kind": kind, "slot": slot, "lane": lane, "mode": mode}
    # Absence is what tells the builder to keep its own default, so an unset field is left out
    # rather than sent as None.
    request.update({name: value for name, value in details.items() if value is not None})
    if track_id is not None:
        request["track_id"] = track_id
    if maximum_rate_millihertz is not None:
        request["maximum_rate_millihertz"] = maximum_rate_millihertz
    if maximum_encoded_bits_per_second is not None:
        request["maximum_encoded_bits_per_second"] = maximum_encoded_bits_per_second
    if uplink:
        request["uplink"] = True
    return _native.build_track_config(session, _surface_identity(surface), request)


def probe_encoded_image(data: BytesLike) -> Tuple[int, int, int, int]:
    """`(encoding, width, height, encoded_length)` from PNG or JPEG header metadata.

    The container is walked in Rust, beside the configuration it produces, so the dimensions a
    track declares and the pixels it later sends cannot come from two different parsers.
    """
    return _native.probe_encoded_image(bytes(data))



def take_event(session: Session) -> Optional[Dict[str, object]]:
    """The next session event, or `None` when the queue is empty."""
    return _native.take_event(session)


def wait_event(session: Session, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US) -> Optional[Dict[str, object]]:
    """The next session event, waiting up to `timeout_us`.

    Returns `None` both on timeout and once the final `connection_closed` event has been taken,
    which is what ends an event loop.
    """
    return _native.wait_event(session, timeout_us=timeout_us)


def abort(session: Session) -> None:
    """Close the lifecycle without a `GOODBYE` round trip, releasing blocked senders."""
    _native.abort(session)


def query_surface(session: Session, surface: Surface) -> Dict[str, object]:
    return _native.query_surface(session, surface)


def query_track(session: Session, track: Track) -> Dict[str, object]:
    return _native.query_track(session, track)


def probe_track(
    session: Session, surface: Surface, config: TrackConfig
) -> Dict[str, object]:
    """Whether the presenter would admit this track, without creating it."""
    return _native.probe_track(
        session, surface, config.native(session, _surface_identity(surface))
    )


def query_anchor(session: Session, context_id: int, anchor_id: int) -> Dict[str, object]:
    return _native.query_anchor(session, context_id, anchor_id)


def query_session(session: Session) -> Dict[int, Any]:
    return _native.query_session(session)


def play(
    session: Session,
    track: Track,
    *,
    start_pts_us: int = 0,
    minimum_buffer_us: int = 0,
    maximum_latency_us: int = 0,
) -> None:
    """Start a timed track. Requires `timed-media-v1`."""
    _native.play(session, track, start_pts_us, minimum_buffer_us, maximum_latency_us)


def pause(session: Session, track: Track) -> None:
    _native.pause(session, track)


def set_audio_gain(session: Session, track: Track, raw: int) -> None:
    """Set track gain as a micropercent, where `2^32` is unity and `2^33` the protocol maximum."""
    _native.set_audio_gain(session, track, raw)


def flush(session: Session, track: Track, new_epoch: int) -> None:
    """Discard media below a new epoch and keep the channel open."""
    _native.flush(session, track, new_epoch)


def drain(session: Session, track: Track) -> None:
    """Wait until the presenter has consumed everything sent so far."""
    _native.drain(session, track)


def create_node(
    session: Session, surface: Surface, node: SceneNode
) -> Dict[str, object]:
    return _native.create_node(session, surface, node.native(session, surface))


def update_node(
    session: Session, surface: Surface, node: SceneNode
) -> Dict[str, object]:
    return _native.update_node(session, surface, node.native(session, surface))


def activate_tracks(
    session: Session, surface: Surface, bindings: Sequence[SlotBinding]
) -> int:
    """Activate a slot set atomically at a compositor boundary."""
    return _native.activate_tracks(
        session, surface, [binding.native() for binding in bindings]
    )


def conpty_anchor_marker(session: Session, context_id: int, anchor_id: int) -> str:
    """The marker spelling a ConPTY host prints for this anchor."""
    return _native.conpty_anchor_marker(session, context_id, anchor_id)


def send_raster_adaptive(
    channel: TrackChannel,
    rgba: BytesLike,
    *,
    epoch: int = 0,
    frame_id: int = 1,
) -> int:
    """Send a frame, compressing only when the result is actually smaller than raw."""
    return _native.send_raster_adaptive(channel, bytes(rgba), epoch=epoch, frame_id=frame_id)


def take_send_pressure(channel: TrackChannel) -> Dict[str, int]:
    """How long the last sends waited, split by cause.

    The three causes have opposite remedies — lower the encoder's output, wait for the presenter
    to return channel-flow capacity, or shrink the transport writes — so they stay separate.
    """
    return _native.take_send_pressure(channel)


def media_credit_available(channel: TrackChannel, body_length: int) -> bool:
    """Whether a record of `body_length` bytes fits the channel's flow window right now."""
    return _native.media_credit_available(channel, body_length)


def advance_channel(session: Session, track: Track, reason: int) -> TrackChannel:
    """Start a fresh authenticated channel generation and return its channel."""
    return _native.advance_channel(session, track, reason)


def channel_take_event(channel: TrackChannel) -> Optional[Dict[str, object]]:
    return _native.channel_take_event(channel)


def channel_wait_event(
    channel: TrackChannel, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US
) -> Optional[Dict[str, object]]:
    """The next reverse-channel event, waiting up to `timeout_us`.

    A keyframe request that arrives while nobody is looking is the difference between a fast
    recovery and a frozen picture, so a video sender parks here rather than polling.
    """
    return _native.channel_wait_event(channel, timeout_us=timeout_us)


from . import aio as aio  # noqa: E402
from . import automation as automation  # noqa: E402
from . import desktop as desktop  # noqa: E402
from . import file_drop as file_drop  # noqa: E402
from . import input as input  # noqa: E402
from . import lease as lease  # noqa: E402
from . import pipeline as pipeline  # noqa: E402
from . import presenter as presenter  # noqa: E402

__all__ = [
    "AudioTrackConfig",
    "DesktopParameters",
    "InputBinding",
    "InputLane",
    "IncomingFileTransfer",
    "OutputConfig",
    "SceneNode",
    "SlotBinding",
    "TrackSender",
    "VideoRateControl",
    "ClosedHandleError",
    "ImagePresentation",
    "ImageTrackConfig",
    "PaneSession",
    "RasterTrackConfig",
    "Session",
    "SessionInfo",
    "Surface",
    "SurfaceConfig",
    "Track",
    "TrackChannel",
    "VideoTrackConfig",
    "VividError",
    "abort",
    "activate_track",
    "activate_tracks",
    "advance_channel",
    "channel_take_event",
    "channel_wait_event",
    "conpty_anchor_marker",
    "create_node",
    "drain",
    "flush",
    "media_credit_available",
    "pause",
    "play",
    "probe_encoded_image",
    "probe_track",
    "query_anchor",
    "query_session",
    "query_surface",
    "query_track",
    "send_raster_adaptive",
    "set_audio_gain",
    "take_event",
    "take_send_pressure",
    "update_node",
    "wait_event",
    "aio",
    "automation",
    "desktop",
    "file_drop",
    "input",
    "lease",
    "pipeline",
    "presenter",
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
    "delete_node",
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

from . import overlay as overlay
from .overlay import OverlaySession as OverlaySession, OverlayWindow as OverlayWindow, OverlayWindowOptions as OverlayWindowOptions
__all__ += ["overlay", "OverlaySession", "OverlayWindow", "OverlayWindowOptions"]
