from __future__ import annotations
import asyncio
import threading
from typing import Any
import pytest
from vivid_sdk import OverlaySession, OverlayWindowOptions, aio
from vivid_sdk.overlay import Brush, Canvas, GradientStop, Path, Point, Rect, ImeEvent, PointerEvent, _event

def drawing() -> Canvas:
    path = Path().move_to(0, 0).line_to(50, 0).quad_to(60, 20, 50, 40).cubic_to(30, 60, 10, 60, 0, 40).close()
    stops = [GradientStop(0, 0xFF0000FF), GradientStop(1, 0x0000FFFF)]
    return (Canvas().save().clip(Path.rounded_rectangle(Rect(0, 0, 100, 80), 8))
        .transform(1, 0, 0, 1, 2, 3).opacity(0.75)
        .fill(path, Brush.linear(Point(0, 0), Point(60, 40), stops))
        .stroke(Path.ellipse(Rect(5, 5, 40, 20)), Brush.radial(Point(10, 10), 30, stops), 2)
        .text("A😀日本語", Point(4, 60), 12, 0xFFFFFFFF)
        .hit((1 << 64) - 1, Path.rectangle(Rect(0, 0, 100, 80)))
        .restore())

def test_native_canvas_ownership_and_session_cleanup() -> None:
    with OverlaySession.connect(dry_run=True) as session, OverlaySession.connect(dry_run=True) as other:
        options = OverlayWindowOptions(Rect(10, 20, 100, 80))
        window, neighbor = session.create_window(options), session.create_window(options)
        with pytest.raises((ValueError, OSError)):
            other.create_window(options, parent=window)
        image = window.upload_rgba(1, 1, b"\xff\x00\x00\xff")
        canvas = drawing()
        canvas.validate()
        window.draw_image(canvas, image, Rect(2, 2, 10, 10))
        with pytest.raises((ValueError, OSError)):
            neighbor.draw_image(Canvas(), image, Rect(0, 0, 10, 10))
        window.present(canvas)
        window.present(canvas.snapshot())
        with pytest.raises((ValueError, OSError)):
            window.upload_rgba(2, 2, b"bad")
        window.close()
        with pytest.raises(OSError): window.present(canvas)
        neighbor.present(canvas)
        session.close()
        with pytest.raises(OSError): neighbor.present(canvas)

def test_invalid_scenes_and_numeric_boundaries() -> None:
    with pytest.raises((ValueError, OSError)): Path.rectangle(Rect(0, 0, float("nan"), 1))
    with pytest.raises((ValueError, OSError)): Canvas().opacity(1.1)
    with pytest.raises((ValueError, OverflowError, OSError)): Canvas().hit(1 << 64, Path.rectangle(Rect(0, 0, 1, 1)))
    with pytest.raises((ValueError, OSError)): Canvas().restore().validate()
    valid = drawing()
    snapshot = valid.snapshot()
    valid.restore()
    snapshot.validate()
    with pytest.raises((ValueError, OSError)): valid.validate()
    with pytest.raises((ValueError, OSError)):
        Canvas().fill(Path.rectangle(Rect(0, 0, 1, 1)), Brush.solid(-1))

class RawEvent:
    def __init__(self, kind: str, values: list[float], text: str = "") -> None:
        self.value: dict[str, Any] = dict(kind=kind, values=values, text=text, revision=(1 << 64)-1, region=(1 << 63)+17)
    def data(self) -> dict[str, Any]: return self.value

def test_event_offsets_and_full_width_values() -> None:
    ime = _event(RawEvent("ime", [1, 5], "A😀日"))
    assert isinstance(ime, ImeEvent) and ime.selection == (1, 2)
    pointer = _event(RawEvent("pointer", [1, 2, 0, 1, 1]))
    assert isinstance(pointer, PointerEvent)
    assert pointer.scene_revision == (1 << 64)-1
    assert pointer.application_id == (1 << 63)+17

def test_async_workflow_and_creation_cancellation() -> None:
    async def run() -> None:
        async with await aio.OverlaySession.connect(dry_run=True) as session:
            async with await session.create_window(OverlayWindowOptions(Rect(0, 0, 100, 80))) as window:
                await window.present(drawing())
                image = await window.upload_rgba(1, 1, b"\xff\x00\x00\xff")
                canvas = Canvas()
                await window.draw_image(canvas, image, Rect(0, 0, 10, 10))
                await window.present(canvas)
            with pytest.raises(ValueError): await session.wait_event(61)
        from vivid_sdk.overlay_async import _create
        started, release = threading.Event(), threading.Event()
        owned = OverlaySession.connect(dry_run=True)
        def create() -> OverlaySession:
            started.set()
            assert release.wait(2)
            return owned
        task = asyncio.create_task(_create(create))
        while not started.is_set(): await asyncio.sleep(0)
        task.cancel()
        await asyncio.sleep(0)
        task.cancel()
        await asyncio.sleep(0)
        release.set()
        with pytest.raises(asyncio.CancelledError): await task
        assert owned.closed
    asyncio.run(run())

def test_unsupported_presenter_rejects_overlay_negotiation() -> None:
    from vivid_sdk import presenter
    running = presenter.start("tcp:127.0.0.1:0")
    try:
        secret = presenter.issue_pane_capability(running, 1)
        with pytest.raises(OSError, match="profile"):
            OverlaySession.connect(endpoint_control=presenter.endpoint(running), root_secret=secret)
    finally:
        presenter.close(running)
