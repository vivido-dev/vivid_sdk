"""Terminating presenter: the other half of :mod:`vivid_sdk`.

The crate root produces media; this accepts it. A presenter binds an endpoint, issues a capability
per pane, and holds the retained scene a producer sends — so a Python program can now be either end
of a Vivid session, or both.

The presenter's threads are pure Rust and never acquire the GIL. That holds because nothing here
takes a Python callback: you supply an endpoint string and the listener is built in Rust, so a
Python exception can never surface inside an accept loop. Keep it that way.

    from vivid_sdk import presenter

    p = presenter.start("tcp:127.0.0.1:0")
    try:
        presenter.update_metrics(p, pane=1, columns=80, rows=24)
        secret = presenter.issue_pane_capability(p, pane=1)   # hand this to a producer
        presenter.wait_for_media(p, pane=1, timeout=5.0)
        for layer in presenter.capture_pane(p, pane=1).layers:
            ...
    finally:
        presenter.close(p)

Reading a pane is pull-based: there are no callbacks and no event queue to drain, so a slow reader
cannot stall the presenter. :func:`wait_for_media` is a bounded wait for something to read.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Dict, Optional, Tuple, Union

from . import _native
from ._native import Presenter

__all__ = [
    "CaptureLayer",
    "EncodedImage",
    "PaneCapture",
    "PaneMediaSummary",
    "Presenter",
    "RasterFrame",
    "SkippedSource",
    "SourceKey",
    "TrackSummary",
    "capture_pane",
    "close",
    "endpoint",
    "issue_pane_capability",
    "pane_media_summary",
    "revoke_pane",
    "start",
    "update_metrics",
    "wait_for_media",
]

# A capture that produced nothing says why. `undecoded_video` will never produce pixels here —
# encoded video is relayed, not decoded — while `no_retained_pixels` is worth retrying.
SKIP_UNDECODED_VIDEO = "undecoded_video"
SKIP_NO_RETAINED_PIXELS = "no_retained_pixels"
SKIP_NODE_HIDDEN = "node_hidden"


@dataclass(frozen=True)
class SourceKey:
    """The complete owner tuple of one track, as the presenter sees it."""

    producer: int
    context: int
    surface: int
    track: int


@dataclass(frozen=True)
class RasterFrame:
    """Retained pixels, tightly packed sRGB RGBA8.

    ``rgba`` is a copy. The presenter's buffer is mutated by its media threads, so a view into it
    would not stay valid for the lifetime of a Python object.
    """

    epoch: int
    frame_id: int
    width: int
    height: int
    rgba: bytes


@dataclass(frozen=True)
class EncodedImage:
    """An encoded still, in whatever encoding the producer sent."""

    data: bytes


@dataclass(frozen=True)
class CaptureLayer:
    """One node's retained content and the rectangle it occupies."""

    source: SourceKey
    node_id: int
    z_index: int
    x: int
    y: int
    width: int
    height: int
    content: Union[RasterFrame, EncodedImage]


@dataclass(frozen=True)
class SkippedSource:
    """A visual source that contributed nothing, and the reason."""

    source: SourceKey
    node_id: int
    reason: str


@dataclass(frozen=True)
class PaneCapture:
    """What a pane is presenting right now."""

    layers: Tuple[CaptureLayer, ...]
    skipped: Tuple[SkippedSource, ...]


@dataclass(frozen=True)
class TrackSummary:
    """One track a pane owns. ``capturable`` is the stronger claim: pixels are in hand now."""

    source: SourceKey
    kind: str
    capturable: bool


@dataclass(frozen=True)
class PaneMediaSummary:
    """Every surface and track a pane owns, in one call rather than one per track."""

    surfaces: Tuple[str, ...]
    tracks: Tuple[TrackSummary, ...]


def start(
    endpoint: str,
    *,
    desktop: Optional[Tuple[int, int]] = None,
    retained_bytes: Optional[int] = None,
) -> Presenter:
    """Bind ``endpoint`` and serve a presenter on it.

    ``endpoint`` is spelled as producers spell it: ``unix:/absolute/path`` or ``tcp:127.0.0.1:PORT``.
    Port 0 binds an ephemeral port; :func:`endpoint` then reports the one the system chose. TCP is
    restricted to loopback.

    Terminal is the default target. Pass ``desktop=(width, height)`` for a desktop presenter; the
    two are different presentation profiles and a producer negotiating the wrong one is refused.
    """
    return _native.presenter_start(endpoint, desktop, retained_bytes)


def endpoint(presenter: Presenter) -> str:
    """The endpoint this presenter is serving, with an ephemeral port already resolved."""
    return presenter.endpoint


def close(presenter: Presenter) -> None:
    """Stop serving. Idempotent."""
    _native.presenter_close(presenter)


def issue_pane_capability(presenter: Presenter, pane: int) -> str:
    """Mint the root secret a producer authenticates to this pane with.

    This is capability material. Hand it to exactly one producer, over a channel that is not a
    command line, and do not log it.
    """
    return _native.presenter_issue_pane_capability(presenter, pane)


def revoke_pane(presenter: Presenter, pane: int) -> None:
    """Revoke a pane's capability and drop the session and objects it owns."""
    _native.presenter_revoke_pane(presenter, pane)


def update_metrics(
    presenter: Presenter,
    pane: int,
    *,
    columns: int,
    rows: int,
    cell_width: int = 8,
    cell_height: int = 16,
) -> None:
    """Tell a pane its terminal geometry, which a terminal producer negotiates against."""
    _native.presenter_update_metrics(
        presenter, pane, columns, rows, cell_width, cell_height
    )


def wait_for_media(presenter: Presenter, pane: int, timeout: float) -> bool:
    """Block until this pane holds something a capture could compose, or ``timeout`` seconds pass.

    Returns whether media arrived. Prefer this to polling :func:`capture_pane`: it waits on the
    presenter's own condition variable rather than spinning.
    """
    if timeout < 0:
        raise ValueError("timeout must not be negative")
    return _native.presenter_wait_for_media(presenter, pane, int(timeout * 1_000_000))


def capture_pane(presenter: Presenter, pane: int, viewport_offset: int = 0) -> PaneCapture:
    """Read what a pane is presenting.

    This composes the producer's own retained surfaces. It is not a screenshot: terminal text
    belongs to a renderer, and no presenter here has one. A capture that produced nothing explains
    itself in ``skipped``.
    """
    native = _native.presenter_capture_pane(presenter, pane, viewport_offset)
    return PaneCapture(
        layers=tuple(_layer(entry) for entry in native["layers"]),
        skipped=tuple(
            SkippedSource(
                source=_source(entry["source"]),
                node_id=entry["node_id"],
                reason=entry["reason"],
            )
            for entry in native["skipped"]
        ),
    )


def pane_media_summary(presenter: Presenter, pane: int) -> PaneMediaSummary:
    """What a pane owns, and whether capturing it would produce anything."""
    native = _native.presenter_pane_media_summary(presenter, pane)
    return PaneMediaSummary(
        surfaces=tuple(native["surfaces"]),
        tracks=tuple(
            TrackSummary(
                source=_source(entry["source"]),
                kind=entry["kind"],
                capturable=entry["capturable"],
            )
            for entry in native["tracks"]
        ),
    )


def _source(native: Dict[str, Any]) -> SourceKey:
    return SourceKey(**native)


def _layer(native: Dict[str, Any]) -> CaptureLayer:
    content = native["content"]
    if content["kind"] == "raster":
        parsed: Union[RasterFrame, EncodedImage] = RasterFrame(
            epoch=content["epoch"],
            frame_id=content["frame_id"],
            width=content["width"],
            height=content["height"],
            rgba=content["rgba"],
        )
    else:
        parsed = EncodedImage(data=content["data"])
    return CaptureLayer(
        source=_source(native["source"]),
        node_id=native["node_id"],
        z_index=native["z_index"],
        x=native["x"],
        y=native["y"],
        width=native["width"],
        height=native["height"],
        content=parsed,
    )
