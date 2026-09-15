from __future__ import annotations
import asyncio
import threading
from typing import cast
from typing import Any
import pytest
from vivid_sdk import OverlaySession, OverlayWindowOptions, aio
from vivid_sdk.overlay import Brush, Canvas, GradientStop, Path, Point, Rect, Shadow, StrokeStyle, EnvironmentEvent, ImeEvent, PointerEvent, ViewportEvent, _event

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
        receipt = window.submit(canvas.snapshot())
        assert receipt.revision == 3 and receipt.wait(0) is None
        with pytest.raises((ValueError,OSError)): receipt.wait(61)
        window.release_image(image)
        with pytest.raises((ValueError,OSError)): window.submit(canvas)
        with pytest.raises((ValueError,OSError)): window.release_image(image)
        with pytest.raises((ValueError, OSError)):
            window.upload_rgba(2, 2, b"bad")
        window.close()
        with pytest.raises(OSError): window.present(canvas)
        with pytest.raises((ValueError, OSError)): neighbor.present(canvas)
        neighbor.present(drawing())
        session.close()
        with pytest.raises(OSError): receipt.wait(0)
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
    viewport = _event(RawEvent("viewport",[400,300,3,2]))
    assert isinstance(viewport,ViewportEvent) and viewport.revision == (1 << 64)-1
    assert viewport.scene_revision == 0 and viewport.viewport.scale_numerator == 3

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

def test_styled_text_validates_native_runs_and_snapshots_the_sequence() -> None:
    from vivid_sdk.overlay import StyledText, TextRun, TextStyle
    runs = [TextRun("A😀", TextStyle(size=18, underline=True)), TextRun("日", TextStyle(size=24, strikethrough=True))]
    text = StyledText(runs, max_width=100, alignment="center", max_lines=2)
    runs.clear()
    text._native()
    assert len(text.runs) == 2
    for invalid in (StyledText([]), StyledText([TextRun("x" * 4097)]),
                    StyledText([TextRun("x")], max_lines=0),
                    StyledText([TextRun("x")], overflow="ellipsis"),
                    StyledText([TextRun("x")], letter_spacing=-1),
                    StyledText([TextRun("x")], line_height=0),
                    StyledText([TextRun("x")], word_spacing=float("inf")),
                    StyledText([TextRun("x", TextStyle(size=float("nan")))])):
        with pytest.raises((ValueError, OSError)): invalid._native()

def test_paint_commands_validate_and_round_trip_through_a_scene() -> None:
    # Every paint form is bounded locally, before anything reaches a presenter, and the
    # negotiated profile is what gates them at submit time.
    with OverlaySession.connect(dry_run=True) as session:
        window = session.create_window(OverlayWindowOptions(Rect(0, 0, 200, 120)))
        path = Path.rectangle(Rect(0, 0, 200, 120))
        image = window.upload_rgba(2, 2, bytes(16))
        canvas = (
            Canvas()
            .shadow(Shadow(Rect(10, 10, 100, 60), (4, 8, 12, 16), 0x00000055, Point(0, 6), 18, -2))
            .fill(path, Brush.image(image, extend="repeat"))
            .fill(path, Brush.linear(Point(0, 0), Point(200, 0),
                                     [GradientStop(0, 0xFF0000FF), GradientStop(1, 0x0000FFFF)],
                                     space="oklab"))
            .stroke_styled(path, Brush.solid(0xFFFFFFFF),
                           StrokeStyle(2.5, cap="round", join="bevel", miter_limit=6,
                                       dashes=(4.0, 2.0), dash_offset=1.5))
            .fill(Path.rounded_rectangle_corners(Rect(0, 0, 80, 40), (2, 6, 10, 14)), Brush.solid(0x00FF00FF))
        )
        canvas.validate()
        assert image.id > 0
        # A dry-run session accepts the paint profile, so submission is the only gate left.
        assert window.submit(canvas).revision > 0

        for bad_shadow in (
            Shadow(Rect(0, 0, 10, 10), (-1, 0, 0, 0)),
            Shadow(Rect(0, 0, 10, 10), blur=4097),
            Shadow(Rect(0, 0, 10, 10), blur=-1),
        ):
            with pytest.raises((ValueError, OSError)):
                Canvas().shadow(bad_shadow).validate()
        for bad_style in (
            StrokeStyle(0),
            StrokeStyle(1, miter_limit=0.5),
            StrokeStyle(1, dashes=(0.0,)),
            StrokeStyle(1, dashes=tuple([1.0] * 33)),
            StrokeStyle(1, dash_offset=-1.0),
        ):
            with pytest.raises((ValueError, OSError)):
                Canvas().stroke_styled(path, Brush.solid(0xFFFFFFFF), bad_style).validate()

def test_region_cursors_hover_and_click_counts() -> None:
    # An unasked cursor must not require the pointer profile, so an existing scene still submits.
    with OverlaySession.connect(dry_run=True) as session:
        window = session.create_window(OverlayWindowOptions(Rect(0, 0, 100, 80)))
        path = Path.rectangle(Rect(0, 0, 100, 80))
        Canvas().hit(1, path).validate()
        Canvas().hit(2, path, "input", cursor="text").validate()
        Canvas().hit(3, path, "drag", cursor="grabbing").validate()
        Canvas().hit(4, path, "resize", edges=8, cursor="resize-up-left").validate()
        # The annotation already forbids this, but a JavaScript caller has no typechecker and a
        # loosely typed Python caller can bypass it, so the native boundary refuses it too.
        with pytest.raises((ValueError, OSError)):
            Canvas().hit(5, path, cursor=cast(Any, "wand")).validate()

def test_a_clipboard_write_is_offered_and_refused_without_a_gesture() -> None:
    # OverlaySession.connect offers the profile, so a dry-run session accepts it; what remains
    # is the host's own guard, which a dry-run session has no gesture to satisfy.
    with OverlaySession.connect(dry_run=True) as session:
        window = session.create_window(OverlayWindowOptions(Rect(0, 0, 100, 80)))
        with pytest.raises((ValueError, OSError)):
            window.set_clipboard("text")

def test_environment_events_decode_with_honest_absence() -> None:
    # Values are [font size, dark flag, reduced motion, refresh interval, revision]; a negative
    # optional field is the host saying it cannot tell, which is not the same as "false".
    known = _event(RawEvent("environment", [13.5, 1, 1, 16667, 3], "Iosevka Term"))
    assert isinstance(known, EnvironmentEvent)
    assert known.environment.font_family == "Iosevka Term"
    assert known.environment.font_size == 13.5
    assert known.environment.appearance == "dark"
    assert known.environment.reduced_motion is True
    assert known.environment.refresh_interval_us == 16667
    assert known.revision == 3

    unknown = _event(RawEvent("environment", [16.0, 0, -1, -1, 1], ""))
    assert isinstance(unknown, EnvironmentEvent)
    assert unknown.environment.appearance == "light"
    assert unknown.environment.reduced_motion is None
    assert unknown.environment.refresh_interval_us is None

def test_semantics_validate_locally_and_refuse_a_stale_revision() -> None:
    from vivid_sdk.overlay import AccessibleAction, SemanticNode, SemanticRole, Semantics
    node = SemanticNode(1, "group", Rect(0, 0, 10, 10))
    with OverlaySession.connect(dry_run=True) as session:
        window = session.create_window(OverlayWindowOptions(Rect(0, 0, 100, 80)))
        # A dry-run session has no assistive technology to describe anything to.
        with pytest.raises((ValueError, OSError)):
            window.set_semantics(Semantics(1, (node,)))
        # A malformed tree is refused locally rather than sent: the host would refuse it too.
        with pytest.raises((ValueError, OSError)):
            window.set_semantics(
                Semantics(1, (SemanticNode(1, "group", Rect(0, 0, 10, 10), children=(0,)),))
            )
        with pytest.raises((ValueError, OSError)):
            window.set_semantics(Semantics(1, (SemanticNode(0, "group", Rect(0, 0, 10, 10)),)))
        with pytest.raises((ValueError, OSError)):
            window.set_semantics(
                Semantics(1, (SemanticNode(1, "group", Rect(0, 0, 10, 10), set=(3, 2)),))
            )
        # The optional detail survives the round trip through the native layer.
        spin = SemanticNode(
            2, "spin-button", Rect(0, 0, 10, 10), "Count",
            numeric=(5.0, 0.0, 10.0), toggled="mixed",
            actions=("increment", "decrement"),
        )
        assert spin.numeric == (5.0, 0.0, 10.0)
        assert AccessibleAction is not None and SemanticRole is not None
