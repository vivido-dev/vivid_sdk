from __future__ import annotations

import asyncio
import hashlib
import threading
from pathlib import Path

import pytest

import vivid_sdk as vivid
from vivid_sdk import aio


def video_config() -> vivid.VideoSourceConfig:
    return vivid.VideoSourceConfig(
        codec="h264",
        packetization="annex-b",
        width=16,
        height=16,
        max_access_unit_bytes=1024,
    )


def audio_config() -> vivid.AudioSourceConfig:
    return vivid.AudioSourceConfig(
        codec="opus",
        packetization="opus-packet",
        sample_rate=48_000,
        channels=2,
        max_access_unit_bytes=1024,
    )


def test_session_defaults_ids_and_secret_safe_repr(monkeypatch: pytest.MonkeyPatch) -> None:
    secret = "ab" * 32
    monkeypatch.setenv("VIVID_TOKEN", secret)
    session = vivid.connect(dry_run=True)
    try:
        assert vivid.allocate_id(session) == 1
        assert vivid.allocate_id(session) == 2
        assert vivid.supports(session, vivid.FEATURE_AUDIO_ACCESS_UNIT_V1)
        assert vivid.root_context_id(session) == 1
        assert vivid.display_state(session) == vivid.DisplayState(
            display_generation=0,
            viewport_width=800,
            viewport_height=600,
            grid_columns=80,
            grid_rows=24,
            cell_width=10,
            cell_height=25,
            settled=True,
        )
        assert secret not in repr(session)
    finally:
        vivid.close(session)
        vivid.close(session)
    assert session.closed


def test_observability_wait_api_and_secret_safe_handles(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "cd" * 32
    monkeypatch.setenv("VIVID_TOKEN", secret)
    session = vivid.connect(dry_run=True)
    try:
        assert vivid.supports(session, vivid.FEATURE_OBSERVABILITY_CORE_V1)
        assert vivid.revision_state(session) == vivid.RevisionState(0, {})
        vivid.set_observation(session, vivid.OBSERVATION_CLASS_MASK)
        assert vivid.take_observation(session) is None

        source = vivid.create_raster_source(session, 1, 1)
        assert vivid.revision_state(session).source_revisions == {source.id: 0}
        handle = vivid.begin_wait_source(
            session,
            source,
            vivid.WAIT_RASTER_FRAME,
            value=1,
            timeout=1.0,
        )
        assert secret not in repr(handle)
        assert not handle.closed
        satisfied = vivid.wait(handle)
        assert satisfied == vivid.WaitSatisfied(
            source.id, 0, vivid.WAIT_RASTER_FRAME, 1
        )
        assert handle.closed

        cancelled = vivid.begin_wait_source(
            session, source, vivid.WAIT_SOURCE_LOST, timeout=1.0
        )
        vivid.cancel_wait(cancelled)
        assert cancelled.closed
        vivid.cancel_wait(cancelled)

        with pytest.raises(vivid.VividError, match="live presenter"):
            vivid.query_source(session, source)
        with pytest.raises(ValueError, match="non-zero"):
            vivid.query_scene(session, maximum_pages=0)
    finally:
        vivid.close(session)


def test_invalid_features_and_closed_handle_errors() -> None:
    with pytest.raises(ValueError, match="strictly increasing"):
        vivid.connect(dry_run=True, required_features=(3, 1))

    session = vivid.connect(dry_run=True)
    vivid.close(session)
    with pytest.raises(vivid.ClosedHandleError):
        vivid.allocate_id(session)


def test_raster_scene_sender_and_handle_consumption() -> None:
    session = vivid.connect(dry_run=True)
    try:
        source = vivid.create_raster_source(session, 2, 2)
        assert source.id == 1
        assert source.kind == "raster"
        assert vivid.is_visible(source)
        assert vivid.visibility_reasons(source) == 0
        assert vivid.take_event(source) is None
        vivid.wait_until_visible(session, source)
        vivid.check_source(session, source)

        node = vivid.place_source(session, source, 2, 1)
        assert node.source_id == source.id
        sender = vivid.open_sender(session, source)
        assert source.closed
        assert sender.id == 1
        assert sender.kind == "raster"
        with pytest.raises(vivid.ClosedHandleError):
            vivid.open_sender(session, source)

        rgba = bytearray([255, 0, 0, 255] * 4)
        vivid.send_raster(sender, memoryview(rgba), width=2, height=2)
        with pytest.raises(ValueError, match="raster sender"):
            vivid.send_video(
                sender,
                b"packet",
                packet_id=1,
                pts_us=0,
                dts_us=0,
                duration_us=1,
                key=True,
            )
        vivid.destroy_source(session, sender)
        vivid.cancel_sender(sender)
        assert sender.closed
        with pytest.raises(vivid.ClosedHandleError):
            vivid.send_raster(sender, rgba, width=2, height=2)
    finally:
        vivid.close(session)


def test_encoded_image_and_raw_scene_transactions() -> None:
    encoded = b"\x89PNG\r\n\x1a\nexample"
    session = vivid.connect(dry_run=True)
    try:
        source = vivid.create_image_source(
            session,
            vivid.ImageSourceConfig(
                encoding="png",
                width=1,
                height=1,
                encoded_length=len(encoded),
                sha256=hashlib.sha256(encoded).digest(),
            ),
        )
        node = vivid.create_scene_node(
            session,
            vivid.SceneNodeConfig(
                source_id=source.id,
                width=1 << 32,
                height=1 << 32,
                clip=vivid.ClipRect(0, 0, 1 << 32, 1 << 32),
            ),
        )
        updated = vivid.update_scene_node(
            session,
            vivid.SceneNodeConfig(
                node_id=node.id,
                source_id=source.id,
                width=2 << 32,
                height=1 << 32,
            ),
        )
        assert updated == node
        sender = vivid.open_sender(session, source)
        vivid.send_image(sender, bytearray(encoded))
        vivid.delete_scene_node(session, node.id)

        with pytest.raises(ValueError, match="32 bytes"):
            vivid.create_image_source(
                session,
                vivid.ImageSourceConfig("png", 1, 1, 1, b"short"),
            )
    finally:
        vivid.close(session)


def test_video_audio_linking_packets_and_playback_controls() -> None:
    session = vivid.connect(dry_run=True)
    try:
        assert vivid.probe_video_config(session, video_config())
        assert vivid.probe_audio_config(session, audio_config())
        video, audio = vivid.create_linked_av_sources(
            session, video_config(), audio_config()
        )
        vivid.place_source(session, video, 2, 2, anchor=False)
        video_sender = vivid.open_sender(session, video)
        audio_sender = vivid.open_sender(session, audio)

        vivid.send_video(
            video_sender,
            b"\x00\x00\x00\x01\x65",
            packet_id=1,
            pts_us=0,
            dts_us=0,
            duration_us=33_333,
            key=True,
        )
        vivid.send_audio(
            audio_sender,
            b"\xf8\xff\xfe",
            packet_id=1,
            pts_us=0,
            dts_us=0,
            duration_us=20_000,
        )
        vivid.play(session, video_sender, minimum_buffer_us=40_000)
        vivid.pause(session, video_sender)
        vivid.flush(session, video_sender, epoch=2)
        vivid.play(session, video_sender, start_pts_us=33_333)
        vivid.eos(session, video_sender, epoch=2)
        vivid.eos(session, audio_sender, epoch=1)
        vivid.drain(session, video_sender)
        vivid.drain(session, audio_sender, timeout=0.01)
    finally:
        vivid.close(session)


def test_trace_session_writes_protocol_files(tmp_path: Path) -> None:
    session = vivid.connect(trace_dir=tmp_path)
    source = vivid.create_raster_source(session, 1, 1)
    sender = vivid.open_sender(session, source)
    vivid.send_raster(sender, b"\x00\x00\x00\xff", width=1, height=1)
    vivid.close(session)
    assert (tmp_path / "control.vivid").is_file()
    assert (tmp_path / f"raster-{sender.id}.vivid").is_file()


def test_asyncio_facade_parity_and_concurrent_senders() -> None:
    async def scenario() -> None:
        session = await aio.connect(dry_run=True)
        try:
            raster = await aio.create_raster_source(session, 1, 1)
            image = await aio.create_image_source(
                session,
                vivid.ImageSourceConfig("jpeg", 1, 1, 3),
            )
            raster_sender, image_sender = await asyncio.gather(
                aio.open_sender(session, raster),
                aio.open_sender(session, image),
            )
            await asyncio.gather(
                aio.send_raster(
                    raster_sender, b"\x00\x00\x00\xff", width=1, height=1
                ),
                aio.send_image(image_sender, b"jpg"),
            )
            assert await aio.is_visible(raster_sender)
            assert await aio.take_event(image_sender) is None
            state = await aio.revision_state(session)
            assert state.source_revisions == {raster_sender.id: 0, image_sender.id: 0}
            wait = await aio.begin_wait_source(
                session,
                raster_sender,
                vivid.WAIT_RASTER_FRAME,
                value=1,
                timeout=1.0,
            )
            assert await aio.wait(wait) == vivid.WaitSatisfied(
                raster_sender.id, 0, vivid.WAIT_RASTER_FRAME, 1
            )
            await aio.cancel_sender(image_sender)
            assert image_sender.closed
        finally:
            await aio.close(session)

    asyncio.run(scenario())


def test_asyncio_cancellation_waits_for_cleanup() -> None:
    async def scenario() -> None:
        started = threading.Event()
        release = threading.Event()
        cleaned: list[str] = []

        def blocking_operation() -> str:
            started.set()
            release.wait(timeout=1)
            return "finished"

        task = asyncio.create_task(
            aio._call(blocking_operation, cleanup=cleaned.append)
        )
        await asyncio.to_thread(started.wait, 1)
        task.cancel()
        asyncio.get_running_loop().call_later(0.01, release.set)
        with pytest.raises(asyncio.CancelledError):
            await task
        assert cleaned == ["finished"]

    asyncio.run(scenario())


def test_asyncio_wait_cancellation_invokes_protocol_cancel(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def scenario() -> None:
        started = threading.Event()
        released = threading.Event()
        cancelled: list[object] = []
        handle = object()

        def blocking_wait() -> None:
            started.set()
            released.wait(timeout=1)

        def cancel_wait(value: object) -> None:
            cancelled.append(value)
            released.set()

        monkeypatch.setattr(aio._sync, "cancel_wait", cancel_wait)  # type: ignore[attr-defined]
        task = asyncio.create_task(
            aio._call(blocking_wait, cancel_wait=handle)  # type: ignore[arg-type]
        )
        await asyncio.to_thread(started.wait, 1)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        assert cancelled == [handle]

    asyncio.run(scenario())


def test_high_level_image_helper(tmp_path: Path) -> None:
    image = tmp_path / "demo.png"
    image.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + (13).to_bytes(4, "big")
        + b"IHDR"
        + (4).to_bytes(4, "big")
        + (2).to_bytes(4, "big")
    )
    trace_dir = tmp_path / "trace"
    trace_dir.mkdir()
    vivid.display_image(image, 0.5, trace_dir=trace_dir)
    assert (trace_dir / "control.vivid").is_file()
    assert list(trace_dir.glob("blob-*.vivid"))

    with pytest.raises(ValueError, match="positive finite"):
        vivid.display_image(image, 0, dry_run=True)

    async def display_async() -> None:
        await aio.display_image(image, dry_run=True)

    asyncio.run(display_async())
