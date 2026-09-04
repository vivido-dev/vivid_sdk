"""Cancellation-safe asyncio facade for :mod:`vivid_sdk`.

Native operations run in worker threads so flow waits and control replies do
not block the event loop. Cancellation waits for the native call to finish;
it never abandons a handle while Rust is mutating it.
"""

from __future__ import annotations

import asyncio
import functools
from pathlib import Path
from typing import Any, Callable, Optional, TypeVar, Union

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
