"""Cancellation-safe asyncio facade for :mod:`vivid_sdk`.

Native operations run in worker threads so flow waits and control replies do
not block the event loop. Cancellation waits for the native call to finish;
it never abandons a handle while Rust is mutating it.
"""

from __future__ import annotations

import asyncio
import functools
from pathlib import Path
from typing import Any, AsyncIterator, Callable, Dict, Optional, TypeVar, Union

from . import (
    AudioTrackConfig,
    BytesLike,
    ImageTrackConfig,
    RasterTrackConfig,
    Session,
    SessionInfo,
    Surface,
    SurfaceConfig,
    Track,
    TrackChannel,
    VideoTrackConfig,
    WaitSatisfied,
    MILESTONE_OUTPUT_READY,
)
from . import activate_track as _activate_track
from . import anchor_marker as _anchor_marker
from . import channel_eos as _channel_eos
from . import close as _close
from . import close_channel as _close_channel
from . import connect as _connect
from . import create_surface as _create_surface
from . import create_track as _create_track
from . import destroy_surface as _destroy_surface
from . import destroy_track as _destroy_track
from . import open_track_channel as _open_track_channel
from . import place_terminal_surface as _place_terminal_surface
from . import send_audio as _send_audio
from . import send_image as _send_image
from . import send_raster as _send_raster
from . import send_video as _send_video
from . import session_info as _session_info
from . import supports as _supports
from . import update_surface as _update_surface
from . import presenter as _presenter_module
from . import wait_track as _wait_track
from . import abort as _abort
from . import activate_tracks as _activate_tracks
from . import advance_channel as _advance_channel
from . import channel_take_event as _channel_take_event
from . import channel_wait_event as _channel_wait_event
from . import conpty_anchor_marker as _conpty_anchor_marker
from . import create_node as _create_node
from . import display_image as _display_image
from . import drain as _drain
from . import flush as _flush
from . import media_credit_available as _media_credit_available
from . import open_track_channel as _open_track_channel_again
from . import pause as _pause
from . import play as _play
from . import probe_track as _probe_track
from . import query_anchor as _query_anchor
from . import query_session as _query_session
from . import query_surface as _query_surface
from . import query_track as _query_track
from . import send_raster_adaptive as _send_raster_adaptive
from . import set_audio_gain as _set_audio_gain
from . import take_event as _take_event
from . import take_send_pressure as _take_send_pressure
from . import update_node as _update_node
from . import wait_event as _wait_event
from . import MAX_TRACK_WAIT_TIMEOUT_US
from .presenter import PaneCapture, PaneMediaSummary, Presenter


_T = TypeVar("_T")


async def _run(function: Callable[..., _T], *args: Any, **kwargs: Any) -> _T:
    loop = asyncio.get_running_loop()
    future = loop.run_in_executor(None, functools.partial(function, *args, **kwargs))
    try:
        return await asyncio.shield(future)
    except asyncio.CancelledError:
        await future
        raise


async def connect(
    *,
    dry_run: bool = False,
    trace_dir: Optional[Union[str, Path]] = None,
    **options: Any,
) -> Session:
    return await _run(_connect, dry_run=dry_run, trace_dir=trace_dir, **options)


async def close(session: Session) -> None:
    await _run(_close, session)


async def supports(session: Session, profile: str) -> bool:
    return await _run(_supports, session, profile)


async def session_info(session: Session) -> SessionInfo:
    return await _run(_session_info, session)


async def create_surface(session: Session, config: SurfaceConfig) -> Surface:
    return await _run(_create_surface, session, config)


async def update_surface(
    session: Session, surface: Surface, config: SurfaceConfig
) -> None:
    await _run(_update_surface, session, surface, config)


async def destroy_surface(session: Session, surface: Surface) -> None:
    await _run(_destroy_surface, session, surface)


async def create_track(
    session: Session,
    surface: Surface,
    config: Union[
        RasterTrackConfig, ImageTrackConfig, VideoTrackConfig, AudioTrackConfig
    ],
) -> Track:
    return await _run(_create_track, session, surface, config)


async def destroy_track(session: Session, track: Track) -> None:
    await _run(_destroy_track, session, track)


async def open_track_channel(session: Session, track: Track) -> TrackChannel:
    return await _run(_open_track_channel, session, track)


async def close_channel(channel: TrackChannel) -> None:
    await _run(_close_channel, channel)


async def send_raster(
    channel: TrackChannel,
    rgba: BytesLike,
    *,
    epoch: int = 0,
    frame_id: int = 1,
    compress: bool = False,
) -> int:
    return await _run(
        _send_raster,
        channel,
        rgba,
        epoch=epoch,
        frame_id=frame_id,
        compress=compress,
    )


async def send_image(channel: TrackChannel, encoded: BytesLike) -> int:
    return await _run(_send_image, channel, encoded)


async def send_video(
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
    return await _run(
        _send_video,
        channel,
        data,
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        key=key,
        epoch=epoch,
    )


async def send_audio(
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
    return await _run(
        _send_audio,
        channel,
        data,
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        epoch=epoch,
        trim_start_samples=trim_start_samples,
        trim_end_samples=trim_end_samples,
    )


async def channel_eos(channel: TrackChannel) -> int:
    return await _run(_channel_eos, channel)


async def activate_track(
    session: Session,
    surface: Surface,
    track: Track,
    *,
    required_milestone: int = MILESTONE_OUTPUT_READY,
) -> int:
    return await _run(
        _activate_track,
        session,
        surface,
        track,
        required_milestone=required_milestone,
    )


async def wait_track(
    session: Session,
    track: Track,
    *,
    condition: int,
    value: Optional[int] = None,
    timeout_us: int = 30_000_000,
) -> WaitSatisfied:
    return await _run(
        _wait_track,
        session,
        track,
        condition=condition,
        value=value,
        timeout_us=timeout_us,
    )


async def place_terminal_surface(
    session: Session,
    surface: Surface,
    *,
    node_id: Optional[int] = None,
    x: int = 0,
    y: int = 0,
    width: int,
    height: int,
    text_layer: int = 1,
) -> tuple[int, int]:
    return await _run(
        _place_terminal_surface,
        session,
        surface,
        node_id=node_id,
        x=x,
        y=y,
        width=width,
        height=height,
        text_layer=text_layer,
    )


async def anchor_marker(
    session: Session,
    *,
    context_id: Optional[int] = None,
    anchor_id: Optional[int] = None,
) -> str:
    return await _run(
        _anchor_marker,
        session,
        context_id=context_id,
        anchor_id=anchor_id,
    )


class _PresenterFacade:
    """The presenter API, mirrored as coroutines.

    Every call runs in a worker thread and is cancellation-safe the same way the producer facade is:
    cancelling waits for the native call to finish rather than abandoning a handle while Rust is
    mutating it. :func:`wait_for_media` is the one that matters here — it blocks for its whole
    timeout, and running it on the event loop thread would stall every other task.
    """

    @staticmethod
    async def start(endpoint: str, **options: Any) -> Presenter:
        return await _run(_presenter_module.start, endpoint, **options)

    @staticmethod
    def endpoint(presenter: Presenter) -> str:
        # A field read, not a call into the presenter. Left synchronous rather than wrapped in a
        # thread that would cost more than the read.
        return _presenter_module.endpoint(presenter)

    @staticmethod
    async def close(presenter: Presenter) -> None:
        await _run(_presenter_module.close, presenter)

    @staticmethod
    async def issue_pane_capability(presenter: Presenter, pane: int) -> str:
        return await _run(_presenter_module.issue_pane_capability, presenter, pane)

    @staticmethod
    async def revoke_pane(presenter: Presenter, pane: int) -> None:
        await _run(_presenter_module.revoke_pane, presenter, pane)

    @staticmethod
    async def update_metrics(presenter: Presenter, pane: int, **geometry: Any) -> None:
        await _run(_presenter_module.update_metrics, presenter, pane, **geometry)

    @staticmethod
    async def wait_for_media(presenter: Presenter, pane: int, timeout: float) -> bool:
        return await _run(_presenter_module.wait_for_media, presenter, pane, timeout)

    @staticmethod
    async def capture_pane(
        presenter: Presenter, pane: int, viewport_offset: int = 0
    ) -> PaneCapture:
        return await _run(_presenter_module.capture_pane, presenter, pane, viewport_offset)

    @staticmethod
    async def pane_media_summary(presenter: Presenter, pane: int) -> PaneMediaSummary:
        return await _run(_presenter_module.pane_media_summary, presenter, pane)


presenter = _PresenterFacade()


# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------


async def take_event(session: Session) -> Optional[Dict[str, Any]]:
    """The next session event, or `None` when the queue is empty."""
    return await _run(_take_event, session)


async def wait_event(
    session: Session, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US
) -> Optional[Dict[str, Any]]:
    """The next session event, waiting up to `timeout_us`."""
    return await _run(_wait_event, session, timeout_us=timeout_us)


async def events(
    session: Session, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US
) -> "AsyncIterator[Dict[str, Any]]":
    """Iterate a session's events until it closes.

    Each step parks a worker on a bounded wait, so the loop costs nothing while the session is
    quiet and the event loop stays free. The iterator ends when the session reports
    `connection_closed`, which is the last event it will ever produce.
    """
    while True:
        event = await wait_event(session, timeout_us=timeout_us)
        if event is None or event.get("kind") == "connection_closed":
            return
        yield event


async def channel_events(
    channel: TrackChannel, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US
) -> "AsyncIterator[Dict[str, Any]]":
    """Iterate a track channel's reverse events until the transport ends."""
    while True:
        event = await _run(_channel_wait_event, channel, timeout_us=timeout_us)
        if event is None:
            return
        yield event


# ---------------------------------------------------------------------------
# Queries, playback, and scene
# ---------------------------------------------------------------------------


async def query_surface(session: Session, surface: Surface) -> Dict[str, Any]:
    return await _run(_query_surface, session, surface)


async def query_track(session: Session, track: Track) -> Dict[str, Any]:
    return await _run(_query_track, session, track)


async def probe_track(session: Session, surface: Surface, config: Any) -> Dict[str, Any]:
    return await _run(_probe_track, session, surface, config)


async def query_anchor(session: Session, context_id: int, anchor_id: int) -> Dict[str, Any]:
    return await _run(_query_anchor, session, context_id, anchor_id)


async def query_session(session: Session) -> Dict[int, Any]:
    return await _run(_query_session, session)


async def abort(session: Session) -> None:
    """Close the lifecycle without a `GOODBYE`, waking senders blocked on a stalled presenter."""
    await _run(_abort, session)


async def play(session: Session, track: Track, **options: Any) -> None:
    await _run(_play, session, track, **options)


async def pause(session: Session, track: Track) -> None:
    await _run(_pause, session, track)


async def set_audio_gain(session: Session, track: Track, raw: int) -> None:
    await _run(_set_audio_gain, session, track, raw)


async def flush(session: Session, track: Track, new_epoch: int) -> None:
    await _run(_flush, session, track, new_epoch)


async def drain(session: Session, track: Track) -> None:
    await _run(_drain, session, track)


async def create_node(session: Session, surface: Surface, node: Any) -> Dict[str, Any]:
    return await _run(_create_node, session, surface, node)


async def update_node(session: Session, surface: Surface, node: Any) -> Dict[str, Any]:
    return await _run(_update_node, session, surface, node)


async def activate_tracks(session: Session, surface: Surface, bindings: Any) -> int:
    return await _run(_activate_tracks, session, surface, bindings)


async def conpty_anchor_marker(session: Session, context_id: int, anchor_id: int) -> str:
    return await _run(_conpty_anchor_marker, session, context_id, anchor_id)


async def send_raster_adaptive(channel: TrackChannel, rgba: BytesLike, **options: Any) -> int:
    return await _run(_send_raster_adaptive, channel, rgba, **options)


async def take_send_pressure(channel: TrackChannel) -> Dict[str, Any]:
    """How long the last sends waited, split by cause."""
    return await _run(_take_send_pressure, channel)


async def media_credit_available(channel: TrackChannel, body_length: int) -> bool:
    return await _run(_media_credit_available, channel, body_length)


async def channel_take_event(channel: TrackChannel) -> Optional[Dict[str, Any]]:
    return await _run(_channel_take_event, channel)


async def channel_wait_event(
    channel: TrackChannel, *, timeout_us: int = MAX_TRACK_WAIT_TIMEOUT_US
) -> Optional[Dict[str, Any]]:
    return await _run(_channel_wait_event, channel, timeout_us=timeout_us)


async def advance_channel(session: Session, track: Track, reason: int) -> TrackChannel:
    return await _run(_advance_channel, session, track, reason)


async def display_image(path: Union[str, Path], **options: Any) -> Any:
    return await _run(_display_image, path, **options)


from .overlay_async import OverlaySession as OverlaySession, OverlayWindow as OverlayWindow
