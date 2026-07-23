"""Asyncio facade for :mod:`vivid_sdk`.

The Rust SDK is synchronous. These wrappers run the same public functions in worker threads while
the native extension releases Python around blocking I/O. Cancellation never abandons a native
operation with a borrowed handle; media waits are first interrupted through SourceCancellation.
"""

from __future__ import annotations

import asyncio
import os
from contextlib import suppress
from typing import Any, Callable, Iterable, Optional, Tuple, TypeVar, Union

import vivid_sdk as _sync
from vivid_sdk import (
    AudioSourceConfig,
    BytesLike,
    DisplayState,
    ImageSourceConfig,
    MediaSender,
    SceneNode,
    SceneNodeConfig,
    Session,
    Source,
    SourceEvent,
    SourceLike,
    VideoSourceConfig,
)

T = TypeVar("T")


async def _call(
    function: Callable[..., T],
    *args: Any,
    cleanup: Optional[Callable[[T], None]] = None,
    cancel_media: Optional[MediaSender] = None,
    **kwargs: Any,
) -> T:
    worker = asyncio.create_task(asyncio.to_thread(function, *args, **kwargs))
    try:
        return await asyncio.shield(worker)
    except asyncio.CancelledError:
        if cancel_media is not None:
            with suppress(Exception):
                await asyncio.to_thread(
                    _sync.cancel_sender,
                    cancel_media,
                    "asyncio media operation cancelled",
                )
        result: Optional[T] = None
        with suppress(Exception):
            result = await asyncio.shield(worker)
        if cleanup is not None and result is not None:
            with suppress(Exception):
                await asyncio.to_thread(cleanup, result)
        raise


async def connect(
    *,
    endpoint: Optional[str] = None,
    bulk_endpoint: Optional[str] = None,
    token: Optional[str] = None,
    dry_run: bool = False,
    trace_dir: Optional[os.PathLike[str]] = None,
    verbose: bool = False,
    producer: str = "vivid-sdk-python",
    producer_version: str = _sync.__version__,
    required_features: Iterable[int] = _sync.DEFAULT_REQUIRED_FEATURES,
    optional_features: Iterable[int] = _sync.DEFAULT_OPTIONAL_FEATURES,
) -> Session:
    return await _call(
        _sync.connect,
        endpoint=endpoint,
        bulk_endpoint=bulk_endpoint,
        token=token,
        dry_run=dry_run,
        trace_dir=trace_dir,
        verbose=verbose,
        producer=producer,
        producer_version=producer_version,
        required_features=required_features,
        optional_features=optional_features,
        cleanup=_sync.close,
    )


async def close(session: Session) -> None:
    await _call(_sync.close, session)


async def allocate_id(session: Session) -> int:
    return await _call(_sync.allocate_id, session)


async def supports(session: Session, feature: int) -> bool:
    return await _call(_sync.supports, session, feature)


async def root_context_id(session: Session) -> int:
    return await _call(_sync.root_context_id, session)


async def display_state(session: Session) -> DisplayState:
    return await _call(_sync.display_state, session)


async def create_text_anchor(session: Session) -> Optional[int]:
    return await _call(_sync.create_text_anchor, session)


def _destroy(session: Session) -> Callable[[Source], None]:
    return lambda created: _sync.destroy_source(session, created)


async def create_raster_source(
    session: Session,
    width: int,
    height: int,
    *,
    source_id: Optional[int] = None,
) -> Source:
    return await _call(
        _sync.create_raster_source,
        session,
        width,
        height,
        source_id=source_id,
        cleanup=_destroy(session),
    )


async def create_image_source(
    session: Session,
    config: ImageSourceConfig,
    *,
    source_id: Optional[int] = None,
) -> Source:
    return await _call(
        _sync.create_image_source,
        session,
        config,
        source_id=source_id,
        cleanup=_destroy(session),
    )


async def create_video_source(
    session: Session,
    config: VideoSourceConfig,
    *,
    source_id: Optional[int] = None,
) -> Source:
    return await _call(
        _sync.create_video_source,
        session,
        config,
        source_id=source_id,
        cleanup=_destroy(session),
    )


async def create_audio_source(
    session: Session,
    config: AudioSourceConfig,
    *,
    source_id: Optional[int] = None,
    linked_video: Optional[SourceLike] = None,
) -> Source:
    return await _call(
        _sync.create_audio_source,
        session,
        config,
        source_id=source_id,
        linked_video=linked_video,
        cleanup=_destroy(session),
    )


async def create_linked_av_sources(
    session: Session,
    video: VideoSourceConfig,
    audio: AudioSourceConfig,
    *,
    video_source_id: Optional[int] = None,
    audio_source_id: Optional[int] = None,
) -> Tuple[Source, Source]:
    def cleanup(created: Tuple[Source, Source]) -> None:
        for handle in created:
            _sync.destroy_source(session, handle)

    return await _call(
        _sync.create_linked_av_sources,
        session,
        video,
        audio,
        video_source_id=video_source_id,
        audio_source_id=audio_source_id,
        cleanup=cleanup,
    )


async def probe_video_config(session: Session, config: VideoSourceConfig) -> bool:
    return await _call(_sync.probe_video_config, session, config)


async def probe_audio_config(session: Session, config: AudioSourceConfig) -> bool:
    return await _call(_sync.probe_audio_config, session, config)


async def place_source(
    session: Session,
    source: SourceLike,
    columns: int,
    rows: int,
    *,
    node_id: Optional[int] = None,
    anchor: bool = True,
    anchor_id: Optional[int] = None,
) -> SceneNode:
    return await _call(
        _sync.place_source,
        session,
        source,
        columns,
        rows,
        node_id=node_id,
        anchor=anchor,
        anchor_id=anchor_id,
        cleanup=lambda node: _sync.delete_scene_node(session, node.id),
    )


async def create_scene_node(session: Session, config: SceneNodeConfig) -> SceneNode:
    return await _call(
        _sync.create_scene_node,
        session,
        config,
        cleanup=lambda node: _sync.delete_scene_node(session, node.id),
    )


async def update_scene_node(session: Session, config: SceneNodeConfig) -> SceneNode:
    return await _call(_sync.update_scene_node, session, config)


async def delete_scene_node(session: Session, node_id: int) -> None:
    await _call(_sync.delete_scene_node, session, node_id)


async def destroy_source(session: Session, source: SourceLike) -> None:
    await _call(_sync.destroy_source, session, source)


async def wait_until_visible(session: Session, source: Source) -> None:
    await _call(_sync.wait_until_visible, session, source)


async def check_source(session: Session, source: Source) -> None:
    await _call(_sync.check_source, session, source)


async def take_event(handle: Union[Source, MediaSender]) -> Optional[SourceEvent]:
    return await _call(_sync.take_event, handle)


async def is_visible(handle: Union[Source, MediaSender]) -> bool:
    return await _call(_sync.is_visible, handle)


async def visibility_reasons(handle: Union[Source, MediaSender]) -> int:
    return await _call(_sync.visibility_reasons, handle)


async def open_sender(session: Session, source: Source) -> MediaSender:
    return await _call(
        _sync.open_sender,
        session,
        source,
        cleanup=lambda sender: _sync.cancel_sender(
            sender, "asyncio sender creation cancelled"
        ),
    )


async def send_raster(
    sender: MediaSender,
    rgba: BytesLike,
    *,
    width: int,
    height: int,
    epoch: int = 1,
    frame_id: int = 1,
) -> None:
    await _call(
        _sync.send_raster,
        sender,
        rgba,
        width=width,
        height=height,
        epoch=epoch,
        frame_id=frame_id,
        cancel_media=sender,
    )


async def send_image(sender: MediaSender, encoded: BytesLike) -> None:
    await _call(
        _sync.send_image,
        sender,
        encoded,
        cancel_media=sender,
    )


async def send_video(
    sender: MediaSender,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    key: bool,
    epoch: int = 1,
) -> None:
    await _call(
        _sync.send_video,
        sender,
        data,
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        key=key,
        epoch=epoch,
        cancel_media=sender,
    )


async def send_audio(
    sender: MediaSender,
    data: BytesLike,
    *,
    packet_id: int,
    pts_us: int,
    dts_us: int,
    duration_us: int,
    trim_start_samples: int = 0,
    trim_end_samples: int = 0,
    epoch: int = 1,
) -> None:
    await _call(
        _sync.send_audio,
        sender,
        data,
        packet_id=packet_id,
        pts_us=pts_us,
        dts_us=dts_us,
        duration_us=duration_us,
        trim_start_samples=trim_start_samples,
        trim_end_samples=trim_end_samples,
        epoch=epoch,
        cancel_media=sender,
    )


async def cancel_sender(
    sender: MediaSender, reason: str = "Python media sender cancelled"
) -> None:
    await _call(_sync.cancel_sender, sender, reason)


async def play(
    session: Session,
    source: SourceLike,
    *,
    start_pts_us: int = 0,
    minimum_buffer_us: int = 0,
) -> None:
    await _call(
        _sync.play,
        session,
        source,
        start_pts_us=start_pts_us,
        minimum_buffer_us=minimum_buffer_us,
    )


async def pause(session: Session, source: SourceLike) -> None:
    await _call(_sync.pause, session, source)


async def flush(session: Session, source: SourceLike, *, epoch: int) -> None:
    await _call(_sync.flush, session, source, epoch=epoch)


async def eos(session: Session, source: SourceLike, *, epoch: int) -> None:
    await _call(_sync.eos, session, source, epoch=epoch)


async def drain(
    session: Session,
    source: SourceLike,
    *,
    timeout: Optional[float] = None,
) -> None:
    await _call(_sync.drain, session, source, timeout=timeout)


async def display_image(
    path: Union[str, os.PathLike[str]],
    scale: float = 1.0,
    *,
    endpoint: Optional[str] = None,
    bulk_endpoint: Optional[str] = None,
    token: Optional[str] = None,
    dry_run: bool = False,
    trace_dir: Optional[os.PathLike[str]] = None,
    verbose: bool = False,
) -> None:
    await _call(
        _sync.display_image,
        path,
        scale,
        endpoint=endpoint,
        bulk_endpoint=bulk_endpoint,
        token=token,
        dry_run=dry_run,
        trace_dir=trace_dir,
        verbose=verbose,
    )


__all__ = [
    "allocate_id",
    "cancel_sender",
    "check_source",
    "close",
    "connect",
    "create_audio_source",
    "create_image_source",
    "create_linked_av_sources",
    "create_raster_source",
    "create_scene_node",
    "create_text_anchor",
    "create_video_source",
    "delete_scene_node",
    "destroy_source",
    "display_state",
    "display_image",
    "drain",
    "eos",
    "flush",
    "is_visible",
    "open_sender",
    "pause",
    "place_source",
    "play",
    "probe_audio_config",
    "probe_video_config",
    "root_context_id",
    "send_audio",
    "send_image",
    "send_raster",
    "send_video",
    "supports",
    "take_event",
    "update_scene_node",
    "visibility_reasons",
    "wait_until_visible",
]
