"""Desktop input lanes: ordinary input over an authenticated interactive connection.

A lane is a separate authenticated connection from control and media, so a stalled or malformed
media source cannot delay input and a revoked grant cannot hold up anything else. Nothing here
injects input; a lane only reports what arrived. Injection belongs to the host that owns the OS
seat, behind its own gate.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, Optional

from . import _native
from ._native import InputLane, Session, VividError

__all__ = [
    "InputBindingStatus",
    "InputGrantTermination",
    "InputLaneEvent",
    "InputLeaseRenewal",
    "close_lane",
    "lane_take_event",
    "lane_wait_event",
    "open_lane",
    "set_binding",
]

#: How long a lane wait blocks before returning nothing.
DEFAULT_LANE_WAIT_US = 1_000_000


@dataclass(frozen=True)
class InputBindingStatus:
    """The presenter's answer to a binding request."""

    producer_epoch: int
    grant_generation: int
    context_id: int
    surface_id: int
    surface_generation: int
    effective_classes: int
    state: int
    reason: int
    watchdog_timeout_us: int


@dataclass(frozen=True)
class InputLeaseRenewal:
    """A watchdog renewal the host must answer within `watchdog_timeout_us`."""

    producer_epoch: int
    grant_generation: int
    context_id: int
    surface_id: int
    surface_generation: int
    renewal_sequence: int
    watchdog_timeout_us: int


@dataclass(frozen=True)
class InputGrantTermination:
    """A validated revocation or reset of an effective grant."""

    producer_epoch: int
    grant_generation: int
    context_id: int
    surface_id: int
    surface_generation: int
    reason: int


@dataclass(frozen=True)
class InputLaneEvent:
    """One actionable lane event.

    `kind` is `input`, `renew`, `revoked`, `reset`, `lane_closed`, or `error`. An `input` event
    carries the presenter's exact payload, still generation-qualified: it has to pass the host's
    injection gate before it reaches the OS, so decoding it early would only lose information.
    """

    kind: str
    record_type: Optional[int] = None
    surface_id: Optional[int] = None
    payload: Optional[Dict[int, object]] = None
    producer_epoch: Optional[int] = None
    grant_generation: Optional[int] = None
    context_id: Optional[int] = None
    surface_generation: Optional[int] = None
    renewal_sequence: Optional[int] = None
    watchdog_timeout_us: Optional[int] = None
    reason: Optional[int] = None
    diagnostic: Optional[str] = None
    message: Optional[str] = None


def _event(raw: Dict[str, object]) -> InputLaneEvent:
    """One lane event, built field by field so the wire shape stays the only authority."""
    payload = raw.get("payload")
    return InputLaneEvent(
        kind=str(raw["kind"]),
        record_type=_opt_int(raw.get("record_type")),
        surface_id=_opt_int(raw.get("surface_id")),
        payload=_payload(payload),
        producer_epoch=_opt_int(raw.get("producer_epoch")),
        grant_generation=_opt_int(raw.get("grant_generation")),
        context_id=_opt_int(raw.get("context_id")),
        surface_generation=_opt_int(raw.get("surface_generation")),
        renewal_sequence=_opt_int(raw.get("renewal_sequence")),
        watchdog_timeout_us=_opt_int(raw.get("watchdog_timeout_us")),
        reason=_opt_int(raw.get("reason")),
        diagnostic=None if raw.get("diagnostic") is None else str(raw["diagnostic"]),
        message=None if raw.get("message") is None else str(raw["message"]),
    )


def _opt_int(value: object) -> Optional[int]:
    """An optional integer field, absent unless the wire carried one."""
    if value is None:
        return None
    if isinstance(value, int):
        return value
    raise TypeError(f"expected an integer payload field, got {type(value).__name__}")


def _payload(value: object) -> Optional[Dict[int, object]]:
    """The presenter's integer-keyed payload, carried through unchanged."""
    if value is None:
        return None
    if isinstance(value, dict):
        return {int(key): item for key, item in value.items()}
    raise TypeError(f"expected a payload mapping, got {type(value).__name__}")


def open_lane(session: Session, *, lane_generation: int = 1) -> InputLane:
    """Open a desktop-input lane. Requires `desktop-input-v1`."""
    return _native.open_input_lane(session, lane_generation)


def close_lane(lane: InputLane) -> None:
    """Close the lane. The session stays usable."""
    _native.close_input_lane(lane)


def set_binding(
    lane: InputLane,
    *,
    producer_epoch: int,
    context_id: int,
    surface_id: int,
    surface_generation: int,
    requested_classes: int,
    reason: int = 1,
    requested_watchdog_us: int = DEFAULT_LANE_WAIT_US,
) -> InputBindingStatus:
    """Bind the lane to a surface's input classes.

    A zero context and surface disable injection; anything else asks for exactly the classes
    named. The presenter answers with the classes it actually granted, which is what the host's
    injection gate must be driven from.
    """
    raw = _native.set_input_binding(
        lane,
        {
            "producer_epoch": producer_epoch,
            "context_id": context_id,
            "surface_id": surface_id,
            "surface_generation": surface_generation,
            "requested_classes": requested_classes,
            "reason": reason,
            "requested_watchdog_us": requested_watchdog_us,
        },
    )
    return InputBindingStatus(**raw)


def lane_take_event(lane: InputLane) -> Optional[InputLaneEvent]:
    """The next lane event, or `None` when the queue is empty."""
    raw = _native.lane_take_event(lane)
    return None if raw is None else _event(raw)


def lane_wait_event(
    lane: InputLane, *, timeout_us: int = DEFAULT_LANE_WAIT_US
) -> Optional[InputLaneEvent]:
    """The next lane event, waiting up to `timeout_us`.

    Pointer motion arrives far faster than any sensible poll interval, and a slow reader is what
    overruns the lane's queue bound, so drive a lane from here rather than from `lane_take_event`.
    """
    raw = _native.lane_wait_event(lane, timeout_us=timeout_us)
    return None if raw is None else _event(raw)
