"""File-drop bindings and incoming transfers.

A drop is offered to a surface's effective binding, accepted by a policy or a person, and carried
on its own authenticated connection with independent flow control. The transfer connection is the
only place file bytes travel, and it never touches the control or media lanes.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

from . import _constant_number, _native
from ._native import IncomingFileTransfer, Session

__all__ = [
    "FileDropAccepted",
    "FileDropGrant",
    "FileDropStatus",
    "FileTransferAdvanced",
    "IncomingTransferRequest",
    "TransferEvent",
    "abort_transfer",
    "accept",
    "advance",
    "cancel",
    "grant_transfer",
    "open_transfer",
    "query",
    "read_transfer_event",
    "send_transfer_result",
    "set_binding",
    "set_read_deadline",
]

#: A destination the host is willing to commit into.
DESTINATION_SHELL_CWD = _constant_number("DESTINATION_SHELL_CWD")
#: The user's desktop folder.
DESTINATION_DESKTOP_FOLDER = _constant_number("DESTINATION_DESKTOP_FOLDER")

#: Transfer states, from the specification's `FILE_DROP_STATUS`.
DROP_OFFERED = _constant_number("DROP_OFFERED")
DROP_ACCEPTED = _constant_number("DROP_ACCEPTED")
DROP_TRANSFERRING = _constant_number("DROP_TRANSFERRING")
DROP_COMMITTED = _constant_number("DROP_COMMITTED")
DROP_CANCELLED = _constant_number("DROP_CANCELLED")
DROP_FAILED = _constant_number("DROP_FAILED")


@dataclass(frozen=True)
class FileDropGrant:
    """The presenter's effective file-drop binding."""

    producer_epoch: int
    grant_generation: int
    context_id: int
    surface_id: int
    surface_generation: int
    state: int
    destination: Optional[int]
    maximum_file_bytes: int
    maximum_pending_offers: int
    maximum_active_transfers: int
    maximum_record_body: int
    acceptance_timeout_us: int
    idle_timeout_us: int
    reason: int


@dataclass(frozen=True)
class FileDropAccepted:
    drop_id: int
    transfer_id: int
    transfer_generation: int
    open_timeout_us: int


@dataclass(frozen=True)
class FileTransferAdvanced:
    transfer_id: int
    generation: int
    committed_offset: int
    open_timeout_us: int


@dataclass(frozen=True)
class FileDropStatus:
    drop_id: int
    state: int
    transfer_id: int
    generation: int
    committed_offset: int
    result: Optional[int]
    final_name: str


@dataclass(frozen=True)
class TransferEvent:
    """One event on an incoming transfer connection.

    `data` carries a byte range at `offset`; `finished` reports the final length; `aborted` says
    the sender gave up. A receiver must write `data` at its offset and answer with `grant_transfer`
    to keep flow moving.
    """

    kind: str
    offset: Optional[int] = None
    bytes: Optional[bytes] = None
    final_length: Optional[int] = None
    reason: Optional[int] = None
    final_offset: Optional[int] = None


def set_binding(
    session: Session,
    *,
    producer_epoch: int,
    context_id: int,
    surface_id: int,
    surface_generation: int,
    maximum_file_bytes: int,
    maximum_record_body: int,
    destination: Optional[int] = None,
    maximum_pending_offers: int = 8,
    maximum_active_transfers: int = 4,
    acceptance_timeout_us: int = 20_000_000,
    idle_timeout_us: int = 5_000_000,
) -> FileDropGrant:
    """Enable, replace, or disable a surface's file-drop binding.

    A `None` destination disables the binding rather than offering a default: a host that has
    nowhere to put a file should say so, not accept one it will drop.
    """
    raw = _native.set_file_drop_binding(
        session,
        {
            "producer_epoch": producer_epoch,
            "context_id": context_id,
            "surface_id": surface_id,
            "surface_generation": surface_generation,
            "destination": destination,
            "maximum_file_bytes": maximum_file_bytes,
            "maximum_pending_offers": maximum_pending_offers,
            "maximum_active_transfers": maximum_active_transfers,
            "maximum_record_body": maximum_record_body,
            "acceptance_timeout_us": acceptance_timeout_us,
            "idle_timeout_us": idle_timeout_us,
        },
    )
    return FileDropGrant(**raw)


def _drop_tuple(
    *,
    producer_epoch: int,
    grant_generation: int,
    context_id: int,
    surface_id: int,
    surface_generation: int,
    drop_id: int,
) -> Dict[str, int]:
    return {
        "producer_epoch": producer_epoch,
        "grant_generation": grant_generation,
        "context_id": context_id,
        "surface_id": surface_id,
        "surface_generation": surface_generation,
        "drop_id": drop_id,
    }


def accept(
    session: Session,
    *,
    drop: Dict[str, int],
    transfer_id: int,
    transfer_generation: int,
    maximum_record_body: int,
    initial_maximum_body_bytes: int,
    initial_maximum_records: int,
) -> FileDropAccepted:
    """Accept an offer, naming the transfer this receiver is about to read.

    `drop` is the complete identity the offer carried; accepting under anything less would let a
    recycled identifier name somebody else's file.
    """
    raw = _native.accept_file_drop(
        session,
        dict(drop),
        transfer_id,
        transfer_generation,
        maximum_record_body,
        initial_maximum_body_bytes,
        initial_maximum_records,
    )
    return FileDropAccepted(**raw)


def cancel(session: Session, *, drop: Dict[str, int], reason: int) -> None:
    """Decline an offer or give up on an accepted one."""
    _native.cancel_file_drop(session, dict(drop), reason)


def advance(
    session: Session,
    *,
    context_id: int,
    surface_id: int,
    drop_id: int,
    transfer_id: int,
    expected_generation: int,
    new_generation: int,
    committed_offset: int,
    maximum_body_bytes: int,
    maximum_records: int,
) -> FileTransferAdvanced:
    """Move a transfer onto a fresh generation after a resume, at a committed offset."""
    raw = _native.advance_file_transfer(
        session,
        {
            "context_id": context_id,
            "surface_id": surface_id,
            "drop_id": drop_id,
            "transfer_id": transfer_id,
            "expected_generation": expected_generation,
            "new_generation": new_generation,
            "committed_offset": committed_offset,
            "maximum_body_bytes": maximum_body_bytes,
            "maximum_records": maximum_records,
        },
    )
    return FileTransferAdvanced(**raw)


def query(session: Session, drop_id: int) -> FileDropStatus:
    """What the presenter believes about a drop, for reconciling after a reconnect."""
    return FileDropStatus(**_native.query_file_drop(session, drop_id))


def open_transfer(
    session: Session,
    *,
    context_id: int,
    surface_id: int,
    producer_epoch: int,
    grant_generation: int,
    surface_generation: int,
    drop_id: int,
    transfer_id: int,
    transfer_generation: int,
    declared_length: int,
    maximum_record_body: int,
    maximum_body_bytes: int,
    maximum_records: int,
    resume_offset: int = 0,
) -> IncomingFileTransfer:
    """Take over the transfer connection the presenter opened for this acceptance."""
    return _native.open_incoming_file_transfer(
        session,
        {
            "context_id": context_id,
            "surface_id": surface_id,
            "producer_epoch": producer_epoch,
            "grant_generation": grant_generation,
            "surface_generation": surface_generation,
            "drop_id": drop_id,
            "transfer_id": transfer_id,
            "transfer_generation": transfer_generation,
            "resume_offset": resume_offset,
            "declared_length": declared_length,
            "maximum_record_body": maximum_record_body,
            "maximum_body_bytes": maximum_body_bytes,
            "maximum_records": maximum_records,
        },
    )


def read_transfer_event(transfer: IncomingFileTransfer) -> TransferEvent:
    """Read the next event, blocking until one arrives.

    Bound the wait with `set_read_deadline` when a send can stall, so a quiet peer cannot pin the
    reading thread forever.
    """
    return TransferEvent(**_native.read_transfer_event(transfer))


def set_read_deadline(transfer: IncomingFileTransfer, timeout_us: Optional[int]) -> None:
    """Bound every later read; `None` restores unbounded reads."""
    _native.set_transfer_read_deadline(transfer, timeout_us)


def grant_transfer(
    transfer: IncomingFileTransfer, *, maximum_body_bytes: int, maximum_records: int
) -> None:
    """Return flow capacity to the sender after committing the bytes read so far."""
    _native.grant_transfer(transfer, maximum_body_bytes, maximum_records)


def send_transfer_result(
    transfer: IncomingFileTransfer,
    *,
    transfer_id: int,
    transfer_generation: int,
    result: int,
    committed_length: int = 0,
    final_name: str = "",
    committed_path: Optional[str] = None,
) -> None:
    """Report the outcome. `committed_path` requires `file-drop-path-v1` and is refused locally
    without it, rather than failing the whole connection."""
    _native.send_transfer_result(
        transfer,
        {
            "transfer_id": transfer_id,
            "transfer_generation": transfer_generation,
            "result": result,
            "committed_length": committed_length,
            "final_name": final_name,
            "committed_path": committed_path,
        },
    )


def abort_transfer(transfer: IncomingFileTransfer, reason: int) -> None:
    """Abandon the transfer, telling the sender why."""
    _native.abort_transfer(transfer, reason)
