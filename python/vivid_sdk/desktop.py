"""Desktop presentation orchestration.

Establishing a desktop presentation is one shape every time: a desktop surface carrying typed
parameters, a full-target node, a video track with its sender, optional audio, and an input lane
when the presenter accepts injection. `DesktopSession` owns that shape, and the session it takes.
"""

from __future__ import annotations

from typing import Optional

from . import _native
from ._native import DesktopSession, Session, Track
from . import SurfaceConfig, TrackConfig

__all__ = [
    "DesktopSession",
    "activate_slots",
    "audio_track",
    "close",
    "establish",
    "send_audio",
    "send_video",
    "video_track",
]


def establish(
    session: Session,
    surface: SurfaceConfig,
    video: TrackConfig,
    audio: Optional[TrackConfig] = None,
) -> DesktopSession:
    """Establish a desktop presentation, taking ownership of `session`.

    The session is consumed because the orchestrator holds it for its whole life; passing it in
    as a live handle would let a caller close it out from under the senders.
    """
    # The surface is configured, not yet created: its identity is what the track builders need,
    # and both configurations are resolved before the orchestrator takes the session over.
    surface_config = surface.native(session)
    return _native.establish_desktop(
        session,
        surface_config,
        video.native(session, surface_config),
        None if audio is None else audio.native(session, surface_config),
    )


def video_track(desktop: DesktopSession) -> Track:
    return _native.desktop_video_track(desktop)


def audio_track(desktop: DesktopSession) -> Optional[Track]:
    return _native.desktop_audio_track(desktop)


def send_video(
    desktop: DesktopSession,
    data: bytes,
    *,
    pts_us: int,
    dts_us: Optional[int] = None,
    duration_us: int = 0,
    key: bool = False,
    packet_id: Optional[int] = None,
    epoch: Optional[int] = None,
) -> int:
    return _native.desktop_send_video(
        desktop,
        bytes(data),
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        key=key,
        epoch=epoch,
    )


def send_audio(
    desktop: DesktopSession,
    data: bytes,
    *,
    packet_id: int,
    pts_us: int,
    duration_us: int,
) -> int:
    return _native.desktop_send_audio(desktop, bytes(data), packet_id, pts_us, duration_us)


def activate_slots(desktop: DesktopSession) -> None:
    """Wait for decoded-output readiness and activate the video and audio slots atomically."""
    _native.desktop_activate_slots(desktop)


def close(desktop: DesktopSession) -> None:
    _native.desktop_close(desktop)
