from __future__ import annotations

import asyncio
import hashlib
from pathlib import Path

import pytest

import vivid_sdk as vivid
from vivid_sdk import aio


def raster_config() -> vivid.RasterTrackConfig:
    return vivid.RasterTrackConfig(width=2, height=2)


def test_session_profiles_and_redacted_repr() -> None:
    secret = "11" * 32
    session = vivid.connect(dry_run=True, root_secret=secret)
    try:
        info = vivid.session_info(session)
        assert info.session_id == 1
        assert info.root_context_id == 1
        assert info.target_profile == vivid.PROFILE_TERMINAL_SURFACE
        assert vivid.supports(session, vivid.PROFILE_CORE)
        assert vivid.supports(session, vivid.PROFILE_LIVE_MEDIA)
        assert secret not in repr(session)
        assert "ROOT_SECRET" not in repr(session)
    finally:
        vivid.close(session)
    assert session.closed


def test_surface_track_channel_and_ordered_eos() -> None:
    session = vivid.connect(dry_run=True)
    try:
        surface = vivid.create_surface(
            session,
            vivid.SurfaceConfig(
                logical_width=2,
                logical_height=2,
                role=vivid.ROLE_FIGURE,
                title="pixels",
            ),
        )
        vivid.place_terminal_surface(
            session,
            surface,
            width=2 << 32,
            height=1 << 32,
        )
        track = vivid.create_track(session, surface, raster_config())
        channel = vivid.open_track_channel(session, track)
        assert surface.context_id == track.context_id
        assert surface.id == track.surface_id
        assert channel.track_id == track.id
        assert channel.generation == track.channel_generation == 1
        waited = vivid.wait_track(
            session,
            track,
            condition=vivid.WAIT_CHANNEL_ACCEPTED,
            timeout_us=1_000_000,
        )
        assert waited.track_id == track.id
        assert waited.channel_generation == channel.generation
        media_sequence = vivid.send_raster(
            channel,
            bytes([255, 0, 0, 255] * 4),
            frame_id=1,
        )
        eos_sequence = vivid.channel_eos(channel)
        assert eos_sequence > media_sequence
        with pytest.raises(ValueError, match="CHANNEL_EOS"):
            vivid.send_raster(channel, bytes([0, 0, 0, 255] * 4), frame_id=2)
        vivid.close_channel(channel)
        assert channel.closed
    finally:
        vivid.close(session)


def test_track_replacement_keeps_surface_generation() -> None:
    session = vivid.connect(dry_run=True)
    try:
        surface = vivid.create_surface(
            session, vivid.SurfaceConfig(logical_width=2, logical_height=2)
        )
        generation = surface.generation
        first = vivid.create_track(session, surface, raster_config())
        second = vivid.create_track(session, surface, raster_config())
        vivid.destroy_track(session, first)
        assert surface.generation == generation
        assert second.surface_id == surface.id

        vivid.update_surface(
            session,
            surface,
            vivid.SurfaceConfig(logical_width=3, logical_height=2),
        )
        assert surface.generation == generation + 1
    finally:
        vivid.close(session)


def test_encoded_image_track_is_immutable_and_one_shot() -> None:
    encoded = b"\x89PNG\r\n\x1a\n" + b"example"
    session = vivid.connect(dry_run=True)
    try:
        surface = vivid.create_surface(
            session, vivid.SurfaceConfig(logical_width=1, logical_height=1)
        )
        track = vivid.create_track(
            session,
            surface,
            vivid.ImageTrackConfig(
                width=1,
                height=1,
                encoded_length=len(encoded),
                encoding=vivid.IMAGE_PNG,
                sha256=hashlib.sha256(encoded).digest(),
            ),
        )
        channel = vivid.open_track_channel(session, track)
        vivid.send_image(channel, encoded)
        with pytest.raises(ValueError, match="exactly one"):
            vivid.send_image(channel, encoded)
    finally:
        vivid.close(session)


def test_trace_uses_1_5_control_and_track_prefaces(tmp_path: Path) -> None:
    session = vivid.connect(trace_dir=tmp_path)
    surface = vivid.create_surface(
        session, vivid.SurfaceConfig(logical_width=2, logical_height=2)
    )
    track = vivid.create_track(session, surface, raster_config())
    channel = vivid.open_track_channel(session, track)
    vivid.send_raster(channel, bytes([0, 0, 0, 255] * 4))
    marker = vivid.anchor_marker(session, anchor_id=7)
    vivid.close(session)

    control = (tmp_path / "control.vivid").read_bytes()
    track_files = tuple(tmp_path.glob("track-*.vivid"))
    assert control[:7] == b"VIVD\x01\x05\x00"
    assert len(track_files) == 1
    assert track_files[0].read_bytes()[:7] == b"VIVD\x01\x05\x02"
    assert "VIVID;3;A;" in marker
    assert ";0000000000000001;0000000000000007;" in marker


def test_async_facade_preserves_owner_handles() -> None:
    async def scenario() -> None:
        session = await aio.connect(dry_run=True)
        try:
            surface = await aio.create_surface(
                session, vivid.SurfaceConfig(logical_width=1, logical_height=1)
            )
            track = await aio.create_track(
                session, surface, vivid.RasterTrackConfig(width=1, height=1)
            )
            channel = await aio.open_track_channel(session, track)
            waited = await aio.wait_track(
                session,
                track,
                condition=vivid.WAIT_CHANNEL_ACCEPTED,
                timeout_us=1_000_000,
            )
            assert waited.track_id == track.id
            await aio.send_raster(channel, b"\x00\x00\x00\xff")
            await aio.channel_eos(channel)
            assert channel.surface_id == surface.id
        finally:
            await aio.close(session)

    asyncio.run(scenario())


def test_invalid_profile_order_rejected_by_native_boundary() -> None:
    with pytest.raises(ValueError, match="sorted"):
        vivid._native.connect(
            dry_run=True,
            required_profiles=(
                vivid.PROFILE_CORE,
                vivid.PROFILE_TERMINAL_SURFACE,
            ),
        )


def test_public_connect_removes_required_profiles_from_optional_set() -> None:
    session = vivid.connect(
        dry_run=True,
        required_profiles=(
            vivid.PROFILE_CORE,
            vivid.PROFILE_LIVE_MEDIA,
            vivid.PROFILE_TERMINAL_SURFACE,
        ),
    )
    vivid.close(session)


def test_pane_session_replaces_media_and_clears_idempotently() -> None:
    png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR" + (2).to_bytes(4, "big") + (
        1
    ).to_bytes(4, "big") + b"\x08\x06\0\0\0"
    pane = vivid.PaneSession.from_env(dry_run=True)
    try:
        pane.show_encoded_image(png, title="encoded")
        pane.show_rgba(1, 1, b"\x01\x02\x03\x04", title="raster")
        pane.clear()
        pane.clear()
        assert repr(pane) == "PaneSession(has_presentation=False)"
    finally:
        pane.close()


def test_pane_session_repr_does_not_expose_authentication() -> None:
    secret = "42" * 32
    pane = vivid.PaneSession.from_env(dry_run=True, root_secret=secret)
    try:
        debug = repr(pane)
        assert secret not in debug
        assert "secret" not in debug.lower()
        assert "endpoint" not in debug.lower()
    finally:
        pane.close()


def test_pane_session_rejects_invalid_replacement_without_clearing() -> None:
    pane = vivid.PaneSession.from_env(dry_run=True)
    try:
        pane.show_rgba(1, 1, b"\0\0\0\xff")
        with pytest.raises(ValueError, match="dimensions"):
            pane.show_rgba(0, 1, b"")
        assert repr(pane) == "PaneSession(has_presentation=True)"
    finally:
        pane.close()
