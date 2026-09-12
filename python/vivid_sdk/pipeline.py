"""Senders that keep media identity continuous across channel recovery.

A recovered channel is a new authenticated generation, but it is still the same track: packet IDs
must keep increasing and the media epoch must never move backward. `TrackSender` owns that
continuity, so a producer that reconnects does not have to reconstruct it.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, Optional

from . import _constant_number, _native
from ._native import Session, Track, TrackChannel, TrackSender

__all__ = [
    "RateSnapshot",
    "VideoRateControl",
    "grant_audio_input",
    "recover_channel",
    "send_audio",
    "send_video",
    "take_audio_input",
]

#: One microphone packet: 20 ms of 48 kHz mono s16LE.
MIC_PACKET_US = _constant_number("MIC_PACKET_US")
#: Bytes in one microphone packet.
MIC_PACKET_BYTES = _constant_number("MIC_PACKET_BYTES")


@dataclass(frozen=True)
class RateSnapshot:
    """What a rate controller has observed and what it currently asks the encoder for."""

    configured_bits_per_second: int
    target_bits_per_second: int
    adjustments: int
    rate_limited_us: int
    flow_limited_us: int
    transport_us: int


def recover_channel(session: Session, track: Track, key_unit: bytes) -> TrackSender:
    """Recover a lost channel: advance, reopen, and send the key unit.

    Only the affected track is touched. The returned sender continues the track's media sequence,
    so the caller keeps sending without renumbering anything.
    """
    return _native.recover_channel(session, track, bytes(key_unit))


def send_video(
    sender: TrackSender,
    data: bytes,
    *,
    pts_us: int,
    dts_us: Optional[int] = None,
    duration_us: int = 0,
    key: bool = False,
    packet_id: Optional[int] = None,
    epoch: Optional[int] = None,
) -> int:
    """Send one video access unit; packet ID and epoch default to the sender's continuity."""
    return _native.sender_send_video(
        sender,
        bytes(data),
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        key=key,
        epoch=epoch,
    )


def send_audio(
    sender: TrackSender,
    data: bytes,
    *,
    pts_us: int,
    duration_us: int,
    packet_id: Optional[int] = None,
    epoch: Optional[int] = None,
) -> int:
    """Send one audio access unit with the same continuity guarantees."""
    return _native.sender_send_audio(
        sender,
        bytes(data),
        packet_id=packet_id,
        pts_us=pts_us,
        duration_us=duration_us,
        epoch=epoch,
    )


def grant_audio_input(channel: TrackChannel) -> None:
    """Ask the presenter for the next microphone packet.

    Microphone audio travels the reverse direction on the same authenticated channel as the
    uplink track, so a grant is what opens the next window of packets.
    """
    _native.grant_audio_input(channel)


def take_audio_input(channel: TrackChannel) -> Optional[Dict[str, object]]:
    """Take one microphone packet, or `None` when the presenter has produced none.

    The packet's `pcm` is exactly `MIC_PACKET_BYTES` of s16LE mono, and `pts_us` places it on the
    track's clock.
    """
    return _native.take_audio_input(channel)


def rate_control(configured_bits_per_second: int) -> "_native.VideoRateControl":
    """An encoder pacing helper fed by `take_send_pressure` observations.

    Send pressure has three causes with opposite remedies: a rate limit means the encoder should
    produce less, a flow limit means the presenter is behind and the sender should wait, and
    transport time means the writes themselves are slow. The controller distinguishes them.
    """
    return _native.VideoRateControl(configured_bits_per_second)


def observe_send(
    control: "_native.VideoRateControl", *, bytes: int, pressure: Dict[str, int]
) -> None:
    """Feed one send's byte count and the pressure that accumulated over it."""
    control.observe_send(bytes, pressure)


def observe_audio_backlog(control: "_native.VideoRateControl", backlog_us: int) -> None:
    """Tell the controller how far audio has fallen behind, so video yields to it."""
    control.observe_audio_backlog(backlog_us)


def poll_target(control: "_native.VideoRateControl") -> Optional[int]:
    """The encoder target, if it changed since the last poll."""
    return control.poll()


def rate_snapshot(control: "_native.VideoRateControl") -> RateSnapshot:
    return RateSnapshot(**control.snapshot())
