"""Contexts, bounded session leases, and resumable authentication.

A lease is how one producer hands a bounded piece of its own authority to another: the holder
gets a context, a profile set, a resource contract, and a deadline, and nothing else. The
activation secret is the lease's whole capability, so it leaves this module exactly once and is
never part of a repr or an error.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

from . import _native
from ._native import Session

__all__ = [
    "ContextReady",
    "SessionLeaseReady",
    "create_context",
    "create_session_lease",
    "prepare_resume",
    "revoke_session_lease",
    "set_observation",
]

#: A lease that is cleaned up as soon as the holder disconnects uncleanly.
CLEANUP_IMMEDIATE = 0
#: A lease that may be resumed within its disconnect grace instead.
CLEANUP_SUSPEND_ON_UNCLEAN_LOSS = 1

#: How long a lease holder has to activate, in microseconds.
DEFAULT_ACTIVATION_TIMEOUT_US = 20_000_000
#: The protocol's ceiling on that window.
MAX_ACTIVATION_TIMEOUT_US = 60_000_000


@dataclass(frozen=True)
class ContextReady:
    """A created child context and the authority it was actually granted."""

    context_id: int
    operation_classes: int
    contract: Tuple[int, ...]
    lifetime_us: int
    revision: int


@dataclass(frozen=True)
class SessionLeaseReady:
    """A minted session lease.

    `activation_secret_hex` is the holder's whole capability. It is returned once and is not part
    of this object's repr; hand it over through an authenticated channel and keep it out of logs
    and command arguments.
    """

    context_id: int
    lease_id: int
    state: int
    activation_timeout_us: int
    disconnect_grace_us: int
    cleanup_policy: int
    permitted_profiles: Tuple[str, ...]
    contract: Tuple[int, ...]
    revision: int
    # Capability material: kept out of the repr so a log line or a traceback cannot carry it.
    activation_secret_hex: str = field(repr=False)


def create_context(
    session: Session,
    *,
    context_id: int,
    parent_context_id: int,
    operation_classes: int,
    label: str = "",
    lifetime_us: int = 0,
    contract: Optional[Sequence[int]] = None,
) -> ContextReady:
    """Create a child context scoped to the operation classes and contract given.

    Omitting `contract` inherits the session's own, which is what a worker context wants; name
    one to narrow it.
    """
    raw = _native.create_context(
        session,
        context_id=context_id,
        parent_context_id=parent_context_id,
        operation_classes=operation_classes,
        label=label,
        lifetime_us=lifetime_us,
        contract=None if contract is None else list(contract),
    )
    raw["contract"] = tuple(raw["contract"])
    return ContextReady(**raw)


def create_session_lease(
    session: Session,
    *,
    context_id: int,
    lease_id: int,
    permitted_profiles: Sequence[str],
    activation_timeout_us: int = DEFAULT_ACTIVATION_TIMEOUT_US,
    disconnect_grace_us: int = 0,
    cleanup_policy: int = CLEANUP_SUSPEND_ON_UNCLEAN_LOSS,
    contract: Optional[Sequence[int]] = None,
) -> SessionLeaseReady:
    """Mint a bounded session lease for a worker.

    `permitted_profiles` must include the session's presentation target profile; a lease that
    could not present anything would be refused on activation with nothing to show for it.
    """
    raw = _native.create_session_lease(
        session,
        context_id=context_id,
        lease_id=lease_id,
        permitted_profiles=sorted(set(permitted_profiles)),
        activation_timeout_us=activation_timeout_us,
        disconnect_grace_us=disconnect_grace_us,
        cleanup_policy=cleanup_policy,
        contract=None if contract is None else list(contract),
    )
    raw["permitted_profiles"] = tuple(raw["permitted_profiles"])
    raw["contract"] = tuple(raw["contract"])
    return SessionLeaseReady(**raw)


def revoke_session_lease(session: Session, context_id: int, lease_id: int) -> None:
    """Revoke a lease, releasing everything scoped to it."""
    _native.revoke_session_lease(session, context_id, lease_id)


def set_observation(session: Session, mask: int) -> None:
    """Choose which observation classes the presenter should report.

    Asking for fewer is a privacy choice as much as a bandwidth one, so the mask is explicit
    rather than a default.
    """
    _native.set_observation(session, mask)


def prepare_resume(session: Session) -> Dict[str, int]:
    """The identity a resuming producer needs, for sessions established through a lease.

    Root sessions are deliberately non-resumable and report so rather than yielding an identity
    that could never be used.
    """
    return _native.prepare_resume(session)
